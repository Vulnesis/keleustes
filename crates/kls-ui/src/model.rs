//! Background model: a snapshot the UI reads, refreshed from the core on
//! bus events (never on a timer), plus a channel for action results.

use kls_core::Core;
use kls_node::Hub;
use kls_store::{Event, JoinToken, Node};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

#[derive(Default, Clone)]
pub struct Snapshot {
    pub server_name: String,
    pub server_id: String,
    pub listen_addr: String,
    pub nodes: Vec<Node>,
    pub online: Vec<String>,
    pub tokens: Vec<JoinToken>,
    pub events: Vec<Event>,
    pub error: Option<String>,
}

/// A screen to open and, optionally, an item to focus there.
pub type NavRequest = (crate::Screen, Option<String>);

pub enum Notice {
    Info(String),
    Error(String),
    /// A freshly minted join token to show once.
    Token {
        token: String,
        expires_at: i64,
    },
}

#[derive(Clone)]
pub struct Model {
    pub hub: Hub,
    pub rt: Handle,
    pub snap: Arc<Mutex<Snapshot>>,
    notices: Arc<Mutex<Vec<Notice>>>,
    nav: Arc<Mutex<Option<NavRequest>>>,
    ctx: egui::Context,
}

const EVENT_WINDOW: i64 = 500;

impl Model {
    pub fn new(hub: Hub, rt: Handle, ctx: egui::Context) -> Self {
        let m = Self {
            hub,
            rt,
            snap: Arc::default(),
            notices: Arc::default(),
            nav: Arc::default(),
            ctx,
        };
        m.start();
        m
    }

    pub fn core(&self) -> &Core {
        self.hub.core()
    }

    pub fn egui_ctx(&self) -> egui::Context {
        self.ctx.clone()
    }

    fn start(&self) {
        let m = self.clone();
        self.rt.spawn(async move {
            let mut rx = m.core().subscribe();
            m.refresh().await;
            // Lagged is fine: a refresh re-reads everything anyway.
            while !matches!(
                rx.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed)
            ) {
                // Coalesce bursts: drain whatever else is queued.
                while rx.try_recv().is_ok() {}
                m.refresh().await;
            }
        });
    }

    pub async fn refresh(&self) {
        let core = self.core();
        let store = core.store();
        let res: anyhow::Result<Snapshot> = async {
            Ok(Snapshot {
                server_name: core.server_name().await?,
                server_id: core.server_id().await?,
                listen_addr: core.listen_addr().await?,
                nodes: store.nodes().await?,
                online: self.hub.online().await,
                tokens: store.list_join_tokens().await?,
                events: store.recent_events(EVENT_WINDOW).await?,
                error: None,
            })
        }
        .await;
        {
            let mut s = self.snap.lock().unwrap();
            match res {
                Ok(n) => *s = n,
                Err(e) => s.error = Some(format!("{e:#}")),
            }
        }
        self.ctx.request_repaint();
    }

    /// Asks the app to switch screen, optionally focusing an item there.
    pub fn go(&self, screen: crate::Screen, focus: Option<String>) {
        *self.nav.lock().unwrap() = Some((screen, focus));
        self.ctx.request_repaint();
    }

    pub(crate) fn take_nav(&self) -> Option<NavRequest> {
        self.nav.lock().unwrap().take()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().unwrap().clone()
    }

    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *self.notices.lock().unwrap())
    }

    pub fn notify(&self, n: Notice) {
        self.notices.lock().unwrap().push(n);
        self.ctx.request_repaint();
    }

    /// Runs an async action; its error (if any) becomes a notice.
    pub fn act<F, Fut>(&self, f: F)
    where
        F: FnOnce(Model) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Option<Notice>>> + Send,
    {
        let m = self.clone();
        self.rt.spawn(async move {
            match f(m.clone()).await {
                Ok(Some(n)) => m.notify(n),
                Ok(None) => {}
                Err(e) => m.notify(Notice::Error(format!("{e:#}"))),
            }
            m.refresh().await;
        });
    }
}
