//! Keleustes persistence.
//!
//! One `Store` over sqlx's `Any` driver, backed by SQLite (default) or
//! Postgres. Queries are written once with `?` placeholders and rewritten to
//! `$n` for Postgres; each dialect has its own migration folder. Values are
//! kept to the types both drivers share (i64, f64, text, bytes): timestamps
//! are Unix milliseconds and structured fields are JSON text.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, AssertSqlSafe, Row};
use std::path::Path;

static SQLITE_MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/sqlite");
static POSTGRES_MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

#[derive(Clone)]
pub struct Store {
    pub(crate) pool: AnyPool,
    dialect: Dialect,
    /// SQLite: the background WAL checkpointer, stopped when the last clone drops.
    checkpointer: Option<std::sync::Arc<Checkpointer>>,
}

/// How often the checkpointer folds the WAL into the database file.
pub(crate) const CHECKPOINT_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Pages of WAL after which a committing statement checkpoints itself: a
/// safety net only, the checkpointer keeps the WAL far below it.
const WAL_AUTOCHECKPOINT_PAGES: i64 = 10_000;

/// Checkpoints the SQLite WAL on its own connection, off every statement's
/// path. With `synchronous=NORMAL` a WAL commit does not fsync; the
/// checkpoint does, and on a busy disk that fsync can take seconds. Run
/// from a committing statement (the default `wal_autocheckpoint`) it made
/// that statement slow; here it delays nothing.
pub(crate) struct Checkpointer {
    _stop: tokio::sync::oneshot::Sender<()>,
    runs: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Checkpointer {
    fn start(url: String) -> Self {
        let (tx, mut stop) = tokio::sync::oneshot::channel::<()>();
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = runs.clone();
        tokio::spawn(async move {
            use sqlx::Connection;
            let mut conn = match sqlx::AnyConnection::connect(&url).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("wal checkpointer: {e:#}");
                    return;
                }
            };
            let mut tick = tokio::time::interval(CHECKPOINT_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = &mut stop => break,
                }
                // PASSIVE: never waits on or blocks readers and writers.
                match sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
                    .execute(&mut conn)
                    .await
                {
                    Ok(_) => {
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => tracing::debug!("wal checkpoint: {e:#}"),
                }
            }
            let _ = conn.close().await;
        });
        Self { _stop: tx, runs }
    }
}

/// Rewrites `?` placeholders to `$1..$n`, leaving quoted text alone.
fn to_postgres(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0;
    let mut in_quote = false;
    for c in sql.chars() {
        match c {
            '\'' => {
                in_quote = !in_quote;
                out.push(c);
            }
            '?' if !in_quote => {
                n += 1;
                out.push('$');
                out.push_str(&n.to_string());
            }
            _ => out.push(c),
        }
    }
    out
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    pub gh_authenticated: bool,
    pub terminal: bool,
    pub remote_control: bool,
    pub providers: Vec<String>,
    /// Max concurrent dispatched sessions (0 = the host default).
    #[serde(default)]
    pub max_sessions: u32,
    /// Dispatch kinds the host understands; empty means an older host
    /// (session, worker, crew).
    #[serde(default)]
    pub dispatch_kinds: Vec<String>,
    #[serde(default)]
    pub job_kinds: Vec<String>,
    #[serde(default)]
    pub can_clone: bool,
    /// Models per provider the host offers.
    #[serde(default)]
    pub models: Vec<ProviderModels>,
    /// How each worker profile claims cards (HostAdvert.agent_routes).
    #[serde(default)]
    pub agent_routes: Vec<AgentRoute>,
    /// Forge CLI logins on the host (forges.md WP-9).
    #[serde(default)]
    pub forges: Vec<ForgeLogin>,
    /// The host runs third-party harness sessions.
    #[serde(default)]
    pub harness_sessions: bool,
    /// The host's library inventory (HostAdvert.inventory): hashes only.
    #[serde(default)]
    pub inventory: Vec<InventoryRef>,
    /// Optional dispatch fields the host applies (Capabilities.dispatch_features:
    /// "resume", "fork", "answers", "gates", "adhoc", "plan_upload",
    /// "session_actions"). An older host lists none.
    #[serde(default)]
    pub dispatch_features: Vec<String>,
}

/// One library item a host holds (kind, id, name, version, hash).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct InventoryRef {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: i64,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ForgeLogin {
    pub cli: String,
    pub host: String,
    pub kind: String,
    pub login: String,
    pub signed_in: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderModels {
    pub provider: String,
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentRoute {
    pub name: String,
    pub mode: String,
    pub lists: Vec<String>,
    pub labels: Vec<String>,
    pub read_only: bool,
}

impl Capabilities {
    /// Whether the host applies the optional dispatch field `feature`.
    pub fn takes_feature(&self, feature: &str) -> bool {
        self.dispatch_features.iter().any(|f| f == feature)
    }

    /// The dispatch kinds the host takes (an older host lists none).
    pub fn takes_dispatch(&self, kind: &str) -> bool {
        if self.dispatch_kinds.is_empty() {
            matches!(kind, "session" | "worker" | "crew")
        } else {
            self.dispatch_kinds.iter().any(|k| k == kind)
        }
    }
}

/// One harness a node advertises (`HostAdvert.harnesses`, harnesses.md).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// "launch", "unverified", "broken", "gui", "configured".
    pub state: String,
    pub detail: String,
    pub resume: bool,
    pub a2a: bool,
    pub logged_in: bool,
    pub grant_env: Vec<String>,
    pub modes: Vec<String>,
    pub transcript: String,
    pub allow_keys: Vec<String>,
    pub path: String,
    pub version_hash: String,
    pub probed_at_ms: i64,
    pub conf: String,
    /// Capability flags (O-046): process-group kill, first-run and tool asks,
    /// a named native session, resume continuity methods.
    pub kill: bool,
    pub asks: bool,
    pub named: bool,
    pub continuity: Vec<String>,
    /// Machine-readable limits {code, note} (O-047).
    pub caveats: Vec<HarnessCaveat>,
    /// Startup overhead measured on this node, spawn to first output (O-048).
    pub startup_p50_ms: i64,
    pub startup_p95_ms: i64,
    pub startup_samples: i64,
    pub startup_measured_at_ms: i64,
}

/// One machine-readable harness caveat.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessCaveat {
    pub code: String,
    pub note: String,
}

impl HarnessInfo {
    pub fn launchable(&self) -> bool {
        self.state == "launch"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub belai_version: String,
    pub host_id: String,
    pub capabilities: Capabilities,
    pub trusted_dirs: Vec<String>,
    /// Agent profiles installed on the host (HostAdvert.agents).
    pub agents: Vec<String>,
    /// Crews installed on the host (HostAdvert.crews).
    pub crews: Vec<String>,
    /// Third-party harnesses on the host (HostAdvert.harnesses).
    pub harnesses: Vec<HarnessInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub host: HostInfo,
    pub cert_serial: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub revoked_reason: Option<String>,
    pub node_seq: i64,
    pub server_acked: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinToken {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub used_by_node: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub subject: String,
    pub data_json: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RedeemError {
    #[error("join token is unknown, used or expired")]
    Invalid,
}

const NODE_COLS: &str = "id, name, hostname, os, arch, belai_version, host_id, capabilities_json, \
     trusted_dirs_json, agents_json, crews_json, harnesses_json, cert_serial, created_at, last_seen_at, \
     revoked_at, revoked_reason, node_seq, server_acked";

fn node_from_row(r: &AnyRow) -> Result<Node> {
    let caps: String = r.try_get("capabilities_json")?;
    let dirs: String = r.try_get("trusted_dirs_json")?;
    let agents: String = r.try_get("agents_json")?;
    let crews: String = r.try_get("crews_json")?;
    let harnesses: String = r.try_get("harnesses_json")?;
    Ok(Node {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        host: HostInfo {
            hostname: r.try_get("hostname")?,
            os: r.try_get("os")?,
            arch: r.try_get("arch")?,
            belai_version: r.try_get("belai_version")?,
            host_id: r.try_get("host_id")?,
            capabilities: serde_json::from_str(&caps).unwrap_or_default(),
            trusted_dirs: serde_json::from_str(&dirs).unwrap_or_default(),
            agents: serde_json::from_str(&agents).unwrap_or_default(),
            crews: serde_json::from_str(&crews).unwrap_or_default(),
            harnesses: serde_json::from_str(&harnesses).unwrap_or_default(),
        },
        cert_serial: r.try_get("cert_serial")?,
        created_at: r.try_get("created_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        revoked_at: r.try_get("revoked_at")?,
        revoked_reason: r.try_get("revoked_reason")?,
        node_seq: r.try_get("node_seq")?,
        server_acked: r.try_get("server_acked")?,
    })
}

impl Store {
    /// Opens (creating if needed) the SQLite database at `path`.
    pub async fn open_sqlite(path: &Path) -> Result<Self> {
        let url = format!("sqlite://{}?mode=rwc", path.display());
        Self::open(&url).await
    }

    /// Opens `sqlite://...` or `postgres://...` and applies migrations.
    pub async fn open(url: &str) -> Result<Self> {
        sqlx::any::install_default_drivers();
        let dialect = if url.starts_with("sqlite:") {
            Dialect::Sqlite
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            Dialect::Postgres
        } else {
            bail!("unsupported database url (want sqlite: or postgres:)");
        };
        let mut opts =
            AnyPoolOptions::new().max_connections(if dialect == Dialect::Sqlite { 4 } else { 10 });
        if dialect == Dialect::Sqlite {
            opts = opts.after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA journal_mode=WAL")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA busy_timeout=5000")
                        .execute(&mut *conn)
                        .await?;
                    // WAL plus NORMAL: a commit appends to the WAL without
                    // an fsync (still atomic and crash-safe; a power cut can
                    // lose the last commits, never corrupt). FULL fsynced on
                    // every commit while holding the write lock, so on a
                    // busy disk every writer queued behind multi-second
                    // fsyncs and ran into busy_timeout (SQLITE_BUSY).
                    sqlx::query("PRAGMA synchronous=NORMAL")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query(AssertSqlSafe(format!(
                        "PRAGMA wal_autocheckpoint={WAL_AUTOCHECKPOINT_PAGES}"
                    )))
                    .execute(&mut *conn)
                    .await?;
                    sqlx::query("PRAGMA foreign_keys=ON")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            });
        }
        let pool = opts.connect(url).await.context("connect database")?;
        match dialect {
            Dialect::Sqlite => SQLITE_MIGRATIONS.run(&pool).await,
            Dialect::Postgres => POSTGRES_MIGRATIONS.run(&pool).await,
        }
        .context("apply migrations")?;
        let in_memory = url.contains(":memory:") || url.contains("mode=memory");
        let checkpointer = (dialect == Dialect::Sqlite && !in_memory)
            .then(|| std::sync::Arc::new(Checkpointer::start(url.to_string())));
        Ok(Self {
            pool,
            dialect,
            checkpointer,
        })
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// WAL checkpoints the background checkpointer has run (0 on Postgres).
    pub fn wal_checkpoints(&self) -> u64 {
        self.checkpointer
            .as_ref()
            .map_or(0, |c| c.runs.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// A transaction that will write. On SQLite it takes the write lock up
    /// front (`BEGIN IMMEDIATE`), so `busy_timeout` can wait for another
    /// writer; a deferred transaction that reads first and writes later fails
    /// with `SQLITE_BUSY_SNAPSHOT` (517) instead, which no timeout retries.
    pub async fn begin_write(&self) -> sqlx::Result<sqlx::Transaction<'static, sqlx::Any>> {
        match self.dialect {
            Dialect::Sqlite => self.pool.begin_with("BEGIN IMMEDIATE").await,
            Dialect::Postgres => self.pool.begin().await,
        }
    }

    /// Every statement is a constant in this file (column lists included);
    /// values always travel as bind parameters, never in the text.
    pub(crate) fn sql(&self, q: &str) -> AssertSqlSafe<String> {
        AssertSqlSafe(match self.dialect {
            Dialect::Sqlite => q.to_string(),
            Dialect::Postgres => to_postgres(q),
        })
    }

    // ---- settings ----------------------------------------------------------

    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query(self.sql("SELECT value FROM settings WHERE key = ?"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        ))
        .bind(key)
        .bind(value)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- join tokens -------------------------------------------------------

    pub async fn insert_join_token(
        &self,
        id: &str,
        secret_sha256: &str,
        label: &str,
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO join_tokens (id, secret_sha256, label, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(id)
        .bind(secret_sha256)
        .bind(label)
        .bind(now_ms())
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Marks the token used if it is unused, unexpired and its secret hash
    /// matches. Atomic: two concurrent joins with one token cannot both win.
    pub async fn redeem_join_token(
        &self,
        id: &str,
        secret_sha256: &str,
        node_id: &str,
    ) -> Result<JoinToken> {
        let now = now_ms();
        let row = sqlx::query(self.sql(
            "UPDATE join_tokens SET used_at = ?, used_by_node = ? \
             WHERE id = ? AND secret_sha256 = ? AND used_at IS NULL AND expires_at > ? \
             RETURNING id, label, created_at, expires_at, used_at, used_by_node",
        ))
        .bind(now)
        .bind(node_id)
        .bind(id)
        .bind(secret_sha256)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        let r = row.ok_or(RedeemError::Invalid)?;
        Ok(JoinToken {
            id: r.try_get("id")?,
            label: r.try_get("label")?,
            created_at: r.try_get("created_at")?,
            expires_at: r.try_get("expires_at")?,
            used_at: r.try_get("used_at")?,
            used_by_node: r.try_get("used_by_node")?,
        })
    }

    pub async fn list_join_tokens(&self) -> Result<Vec<JoinToken>> {
        let rows = sqlx::query(
            "SELECT id, label, created_at, expires_at, used_at, used_by_node FROM join_tokens ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(JoinToken {
                    id: r.try_get("id")?,
                    label: r.try_get("label")?,
                    created_at: r.try_get("created_at")?,
                    expires_at: r.try_get("expires_at")?,
                    used_at: r.try_get("used_at")?,
                    used_by_node: r.try_get("used_by_node")?,
                })
            })
            .collect()
    }

    // ---- nodes -------------------------------------------------------------

    pub async fn insert_node(
        &self,
        id: &str,
        name: &str,
        host: &HostInfo,
        cert_serial: &str,
        token_id: &str,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO nodes (id, name, hostname, os, arch, belai_version, host_id, capabilities_json, \
             trusted_dirs_json, agents_json, crews_json, harnesses_json, cert_serial, joined_via_token, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(id)
        .bind(name)
        .bind(&host.hostname)
        .bind(&host.os)
        .bind(&host.arch)
        .bind(&host.belai_version)
        .bind(&host.host_id)
        .bind(serde_json::to_string(&host.capabilities)?)
        .bind(serde_json::to_string(&host.trusted_dirs)?)
        .bind(serde_json::to_string(&host.agents)?)
        .bind(serde_json::to_string(&host.crews)?)
        .bind(serde_json::to_string(&host.harnesses)?)
        .bind(cert_serial)
        .bind(token_id)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_node_host(&self, id: &str, host: &HostInfo) -> Result<()> {
        sqlx::query(self.sql(
            "UPDATE nodes SET hostname = ?, os = ?, arch = ?, belai_version = ?, host_id = ?, \
             capabilities_json = ?, trusted_dirs_json = ?, agents_json = ?, crews_json = ?, \
             harnesses_json = ?, last_seen_at = ? WHERE id = ?",
        ))
        .bind(&host.hostname)
        .bind(&host.os)
        .bind(&host.arch)
        .bind(&host.belai_version)
        .bind(&host.host_id)
        .bind(serde_json::to_string(&host.capabilities)?)
        .bind(serde_json::to_string(&host.trusted_dirs)?)
        .bind(serde_json::to_string(&host.agents)?)
        .bind(serde_json::to_string(&host.crews)?)
        .bind(serde_json::to_string(&host.harnesses)?)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_node(&self, id: &str) -> Result<()> {
        sqlx::query(self.sql("UPDATE nodes SET last_seen_at = ? WHERE id = ?"))
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rename_node(&self, id: &str, name: &str) -> Result<()> {
        sqlx::query(self.sql("UPDATE nodes SET name = ? WHERE id = ?"))
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn node(&self, id: &str) -> Result<Option<Node>> {
        let q = format!("SELECT {NODE_COLS} FROM nodes WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(node_from_row).transpose()
    }

    pub async fn nodes(&self) -> Result<Vec<Node>> {
        let q = format!("SELECT {NODE_COLS} FROM nodes ORDER BY created_at");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(node_from_row).collect()
    }

    pub async fn revoke_node(&self, id: &str, reason: &str) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE nodes SET revoked_at = ?, revoked_reason = ? WHERE id = ? AND revoked_at IS NULL",
        ))
        .bind(now_ms())
        .bind(reason)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Records the highest node frame applied. Never moves backwards.
    pub async fn set_node_seq(&self, id: &str, seq: i64) -> Result<()> {
        sqlx::query(self.sql("UPDATE nodes SET node_seq = ? WHERE id = ? AND node_seq < ?"))
            .bind(seq)
            .bind(id)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- server -> node outbox --------------------------------------------

    /// Appends an encoded ServerFrame body and returns its seq.
    pub async fn outbox_push(&self, node_id: &str, frame: &[u8]) -> Result<i64> {
        let mut tx = self.begin_write().await?;
        let row = sqlx::query(
            self.sql("SELECT COALESCE(MAX(seq), 0) AS m FROM node_outbox WHERE node_id = ?"),
        )
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        let acked = sqlx::query(self.sql("SELECT server_acked FROM nodes WHERE id = ?"))
            .bind(node_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get::<i64, _>("server_acked"))
            .unwrap_or(0);
        let seq = row.get::<i64, _>("m").max(acked) + 1;
        sqlx::query(
            self.sql(
                "INSERT INTO node_outbox (node_id, seq, frame, created_at) VALUES (?, ?, ?, ?)",
            ),
        )
        .bind(node_id)
        .bind(seq)
        .bind(frame.to_vec())
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(seq)
    }

    pub async fn outbox_after(&self, node_id: &str, after: i64) -> Result<Vec<(i64, Vec<u8>)>> {
        let rows =
            sqlx::query(self.sql(
                "SELECT seq, frame FROM node_outbox WHERE node_id = ? AND seq > ? ORDER BY seq",
            ))
            .bind(node_id)
            .bind(after)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<i64, _>("seq"), r.get::<Vec<u8>, _>("frame")))
            .collect())
    }

    /// The node applied everything up to `seq`: drop it from the outbox.
    pub async fn outbox_ack(&self, node_id: &str, seq: i64) -> Result<()> {
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM node_outbox WHERE node_id = ? AND seq <= ?"))
            .bind(node_id)
            .bind(seq)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            self.sql("UPDATE nodes SET server_acked = ? WHERE id = ? AND server_acked < ?"),
        )
        .bind(seq)
        .bind(node_id)
        .bind(seq)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // ---- events ------------------------------------------------------------

    pub async fn append_event(&self, kind: &str, subject: &str, data_json: &str) -> Result<Event> {
        let at = now_ms();
        let row = sqlx::query(self.sql(
            "INSERT INTO events (at, kind, subject, data_json) VALUES (?, ?, ?, ?) RETURNING id",
        ))
        .bind(at)
        .bind(kind)
        .bind(subject)
        .bind(data_json)
        .fetch_one(&self.pool)
        .await?;
        Ok(Event {
            id: row.try_get("id")?,
            at,
            kind: kind.into(),
            subject: subject.into(),
            data_json: data_json.into(),
        })
    }

    pub async fn events_after(&self, after: i64, limit: i64) -> Result<Vec<Event>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, at, kind, subject, data_json FROM events WHERE id > ? ORDER BY id LIMIT ?",
        ))
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Event {
                    id: r.try_get("id")?,
                    at: r.try_get("at")?,
                    kind: r.try_get("kind")?,
                    subject: r.try_get("subject")?,
                    data_json: r.try_get("data_json")?,
                })
            })
            .collect()
    }

    /// Most recent `limit` events, oldest first.
    pub async fn recent_events(&self, limit: i64) -> Result<Vec<Event>> {
        let rows =
            sqlx::query(self.sql(
                "SELECT id, at, kind, subject, data_json FROM events ORDER BY id DESC LIMIT ?",
            ))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut out: Vec<Event> = rows
            .iter()
            .map(|r| {
                Ok(Event {
                    id: r.try_get("id")?,
                    at: r.try_get("at")?,
                    kind: r.try_get("kind")?,
                    subject: r.try_get("subject")?,
                    data_json: r.try_get("data_json")?,
                })
            })
            .collect::<Result<_>>()?;
        out.reverse();
        Ok(out)
    }
}

pub mod audit;
pub mod belai;
pub mod board;
pub mod engine;
pub mod github;
pub mod host;
pub mod libsearch;
pub mod sessions;
pub mod vault;

#[cfg(test)]
mod tests;
