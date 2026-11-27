//! Keleustes domain core.
//!
//! `Core` owns the store and the in-process event bus. Every change worth
//! showing is persisted to the `events` table and broadcast; the UI and the
//! node server subscribe instead of polling (this replaces vdb-site's
//! Postgres LISTEN/NOTIFY and works the same on SQLite and Postgres).

use anyhow::Result;
use kls_store::{Event, Store};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

pub use kls_store;

pub mod artifacts;
/// Prompt and dispatch attachments (#53) and agent files (agent B).
pub mod attachments;
pub mod audit;
pub mod belai;
pub mod board;
pub mod classify;
pub mod control;
pub mod engine;
pub mod estimates;
pub mod gateway;
pub mod github;
pub mod harness;
pub mod hosts;
pub mod libsearch;
pub mod needs;
pub mod repo_profile;
pub mod sessions;
pub mod sweeps;
pub mod vault;

pub use gateway::{DispatchSpec, JobNeeds, JobOutcome, NodeGateway};

/// Event kinds. Strings are stored, so they never change once shipped.
pub mod kinds {
    pub const NODE_JOINED: &str = "node.joined";
    pub const NODE_ONLINE: &str = "node.online";
    pub const NODE_OFFLINE: &str = "node.offline";
    pub const NODE_UPDATED: &str = "node.updated";
    pub const NODE_REVOKED: &str = "node.revoked";
    pub const TOKEN_MINTED: &str = "token.minted";
    // Areas add theirs as `<area>.<what>` constants in their own module
    // (for example `sessions::kinds`); the UI reloads by prefix.
}

/// Settings keys.
pub mod settings {
    pub const SERVER_ID: &str = "server.id";
    pub const SERVER_NAME: &str = "server.name";
    pub const LISTEN_ADDR: &str = "node.listen_addr";
    pub const ADVERTISE_ADDRS: &str = "node.advertise_addrs";
}

pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:7443";

#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    data_dir: PathBuf,
    bus: broadcast::Sender<Event>,
    gateway: std::sync::OnceLock<Arc<dyn NodeGateway>>,
    /// Ids of events this process emitted, until the tail passes them.
    local_events: std::sync::Mutex<std::collections::HashSet<i64>>,
    /// Set once the tail runs; until then nothing is tracked.
    tailing: std::sync::atomic::AtomicBool,
    /// Held across append+track and across the tail's fetch, so the tail
    /// never sees a local event before it is tracked.
    emit_lock: tokio::sync::Mutex<()>,
}

impl Core {
    /// Opens the data directory: `keleustes.db` there unless `database_url`
    /// names another database (for example Postgres).
    pub async fn open(data_dir: &Path, database_url: Option<&str>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let store = match database_url {
            Some(url) => Store::open(url).await?,
            None => Store::open_sqlite(&data_dir.join("keleustes.db")).await?,
        };
        let (bus, _) = broadcast::channel(1024);
        let core = Self {
            inner: Arc::new(Inner {
                store,
                data_dir: data_dir.to_path_buf(),
                bus,
                gateway: std::sync::OnceLock::new(),
                local_events: Default::default(),
                tailing: Default::default(),
                emit_lock: Default::default(),
            }),
        };
        core.ensure_identity().await?;
        Ok(core)
    }

    async fn ensure_identity(&self) -> Result<()> {
        let s = self.store();
        if s.setting(settings::SERVER_ID).await?.is_none() {
            s.set_setting(settings::SERVER_ID, &uuid_like()).await?;
        }
        if s.setting(settings::SERVER_NAME).await?.is_none() {
            let name = hostname().unwrap_or_else(|| "Keleustes".into());
            s.set_setting(settings::SERVER_NAME, &name).await?;
        }
        Ok(())
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    pub async fn server_id(&self) -> Result<String> {
        Ok(self
            .store()
            .setting(settings::SERVER_ID)
            .await?
            .unwrap_or_default())
    }

    pub async fn server_name(&self) -> Result<String> {
        Ok(self
            .store()
            .setting(settings::SERVER_NAME)
            .await?
            .unwrap_or_default())
    }

    pub async fn listen_addr(&self) -> Result<String> {
        Ok(self
            .store()
            .setting(settings::LISTEN_ADDR)
            .await?
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.into()))
    }

    /// Persists and broadcasts an event.
    pub async fn emit<T: Serialize>(&self, kind: &str, subject: &str, data: &T) -> Result<Event> {
        let json = serde_json::to_string(data)?;
        let ev = {
            let _g = self.inner.emit_lock.lock().await;
            let ev = self.store().append_event(kind, subject, &json).await?;
            if self
                .inner
                .tailing
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                self.inner.local_events.lock().unwrap().insert(ev.id);
            }
            ev
        };
        // No subscribers is fine.
        let _ = self.inner.bus.send(ev.clone());
        Ok(ev)
    }

    /// Rebroadcasts events other processes wrote to the shared database (a
    /// `keleustes` CLI command next to the running app, or a second app on
    /// the same Postgres), so screens, the engine and the node links see
    /// them. Events this process emitted are not sent twice.
    pub fn start_event_tail(&self, every: std::time::Duration) {
        let core = self.clone();
        self.inner
            .tailing
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(async move {
            let mut cursor = core
                .store()
                .recent_events(1)
                .await
                .ok()
                .and_then(|v| v.last().map(|e| e.id))
                .unwrap_or(0);
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                let fetched = {
                    let _g = core.inner.emit_lock.lock().await;
                    core.store().events_after(cursor, 500).await
                };
                let Ok(batch) = fetched else {
                    continue;
                };
                for ev in batch {
                    cursor = cursor.max(ev.id);
                    let mine = core.inner.local_events.lock().unwrap().remove(&ev.id);
                    if !mine {
                        let _ = core.inner.bus.send(ev);
                    }
                }
                // Locally emitted ids at or below the cursor are settled.
                core.inner
                    .local_events
                    .lock()
                    .unwrap()
                    .retain(|id| *id > cursor);
            }
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.bus.subscribe()
    }

    /// Installs the node gateway (done once by kls-node at startup).
    pub fn set_gateway(&self, g: Arc<dyn NodeGateway>) {
        let _ = self.inner.gateway.set(g);
    }

    /// The node gateway; an error when no node server runs in this process
    /// (for example a CLI subcommand).
    pub fn gateway(&self) -> Result<Arc<dyn NodeGateway>> {
        self.inner
            .gateway
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no node server in this process"))
    }
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn uuid_like() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tail_rebroadcasts_other_processes_events_once() {
        let dir = tempfile::tempdir().unwrap();
        let app = Core::open(dir.path(), None).await.unwrap();
        let cli = Core::open(dir.path(), None).await.unwrap();
        let mut rx = app.subscribe();
        app.start_event_tail(std::time::Duration::from_millis(20));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        app.emit("test.local", "a", &serde_json::json!({}))
            .await
            .unwrap();
        cli.emit("test.remote", "b", &serde_json::json!({}))
            .await
            .unwrap();

        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            seen.push(ev.kind);
        }
        assert_eq!(
            seen,
            vec!["test.local".to_string(), "test.remote".to_string()]
        );
    }

    #[tokio::test]
    async fn emit_persists_and_broadcasts() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        assert!(!core.server_id().await.unwrap().is_empty());
        let mut rx = core.subscribe();
        core.emit(kinds::NODE_ONLINE, "n1", &serde_json::json!({}))
            .await
            .unwrap();
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.kind, kinds::NODE_ONLINE);
        assert_eq!(core.store().recent_events(5).await.unwrap().len(), 1);
    }
}
