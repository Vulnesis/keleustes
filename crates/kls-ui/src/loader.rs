//! `Loader<T>`: screen data fetched in the background and refetched when an
//! event whose kind starts with one of the given prefixes is published.
//! Screens read `get()` each frame; nothing polls.
//!
//! ```ignore
//! let runs = Loader::new(model, &["run.", "workflow."], |core| async move {
//!     core.store().runs(100).await
//! });
//! if let Some(Ok(rows)) = runs.get() { ... }
//! ```

use crate::model::Model;
use kls_core::Core;
use std::future::Future;
use std::sync::{Arc, Mutex};

pub struct Loader<T> {
    slot: Arc<Mutex<Option<Result<T, String>>>>,
    kick: tokio::sync::mpsc::UnboundedSender<()>,
}

impl<T: Clone + Send + 'static> Loader<T> {
    pub fn new<F, Fut>(model: &Model, prefixes: &[&str], fetch: F) -> Self
    where
        F: Fn(Core) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let slot: Arc<Mutex<Option<Result<T, String>>>> = Arc::default();
        let (kick, mut kicked) = tokio::sync::mpsc::unbounded_channel::<()>();
        let prefixes: Vec<String> = prefixes.iter().map(|s| s.to_string()).collect();
        let core = model.core().clone();
        let ctx = model.egui_ctx();
        let s2 = slot.clone();
        model.rt.spawn(async move {
            let mut bus = core.subscribe();
            loop {
                let r = fetch(core.clone()).await.map_err(|e| format!("{e:#}"));
                *s2.lock().unwrap() = Some(r);
                ctx.request_repaint();
                // Wait for a relevant event or an explicit reload.
                loop {
                    tokio::select! {
                        ev = bus.recv() => match ev {
                            Ok(ev) if prefixes.iter().any(|p| ev.kind.starts_with(p.as_str())) => break,
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                            Err(_) => return,
                        },
                        k = kicked.recv() => match k {
                            Some(()) => break,
                            None => return, // Loader dropped
                        },
                    }
                }
                // Coalesce bursts.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                while bus.try_recv().is_ok() {}
                while kicked.try_recv().is_ok() {}
            }
        });
        Self { slot, kick }
    }

    /// The latest result, if the first fetch finished.
    pub fn get(&self) -> Option<Result<T, String>> {
        self.slot.lock().unwrap().clone()
    }

    /// Refetch now.
    pub fn reload(&self) {
        let _ = self.kick.send(());
    }
}
