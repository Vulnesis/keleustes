//! Tray icon: closing the window keeps Keleustes serving nodes; the tray
//! shows live status, opens screens, and drives the factory controls. On
//! Linux the tray needs a GTK main loop, which runs on its own thread;
//! elsewhere it lives on the window's thread.
//!
//! Every state change from the tray goes through the governance API with
//! origin [`ORIGIN`] and is audited; a refused one writes a refusal entry.
//! Stop never acts from a tray click: it opens the window's confirm.

use anyhow::Result;
use kls_core::Core;
use kls_core::audit::config::{self, keys};
use kls_core::control::{self, Mode};
use kls_node::Hub;
use kls_ui::{Screen, brand, icons};
use serde_json::json;
use std::sync::mpsc;
use tray_icon::menu::{
    CheckMenuItem, Icon as MenuIcon, IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
    Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Who the ledger records for tray actions.
pub const ORIGIN: &str = "owner:tray";

/// Something a tray click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Open(Screen),
    NewSession,
    AddNode,
    Pause,
    /// Opens the window's Stop confirm; never stops by itself.
    Stop,
    Resume,
    ToggleA2a,
    ToggleMute,
    Theme {
        dark: bool,
    },
    Quit,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Open(_) => "open",
            Action::NewSession => "new_session",
            Action::AddNode => "add_node",
            Action::Pause => "pause",
            Action::Stop => "stop",
            Action::Resume => "resume",
            Action::ToggleA2a => "a2a_blackout",
            Action::ToggleMute => "mute",
            Action::Theme { .. } => "theme",
            Action::Quit => "quit",
        }
    }
}

// Menu item ids.
const ID_NODES: &str = "status.nodes";
const ID_NEEDS: &str = "status.needs";
const ID_FACTORY: &str = "status.factory";
const ID_ROUTE: &str = "status.route";
const OPEN: [(&str, Screen); 5] = [
    ("open.cockpit", Screen::Cockpit),
    ("open.sessions", Screen::Sessions),
    ("open.checkpoints", Screen::Checkpoints),
    ("open.fleet", Screen::Fleet),
    ("open.vault", Screen::Vault),
];
const ID_NEW_SESSION: &str = "new_session";
const ID_ADD_NODE: &str = "add_node";
const ID_PAUSE: &str = "factory.pause";
const ID_STOP: &str = "factory.stop";
const ID_RESUME: &str = "factory.resume";
const ID_A2A: &str = "a2a";
const ID_MUTE: &str = "mute";
const ID_LIGHT: &str = "theme.light";
const ID_DARK: &str = "theme.dark";
const ID_QUIT: &str = "quit";

/// The action a menu id stands for (status lines have none).
pub fn action_for(id: &str) -> Option<Action> {
    if let Some((_, s)) = OPEN.iter().find(|(i, _)| *i == id) {
        return Some(Action::Open(*s));
    }
    Some(match id {
        ID_NEW_SESSION => Action::NewSession,
        ID_ADD_NODE => Action::AddNode,
        ID_PAUSE => Action::Pause,
        ID_STOP => Action::Stop,
        ID_RESUME => Action::Resume,
        ID_A2A => Action::ToggleA2a,
        ID_MUTE => Action::ToggleMute,
        ID_LIGHT => Action::Theme { dark: false },
        ID_DARK => Action::Theme { dark: true },
        ID_QUIT => Action::Quit,
        _ => return None,
    })
}

// ---- model -----------------------------------------------------------------

/// What the menu shows, read from the core.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub online: usize,
    pub joined: usize,
    pub needs: usize,
    pub mode: Mode,
    pub a2a_blackout: bool,
    pub muted: bool,
    /// The stored theme: `dark`, `light` or `system`.
    pub theme: String,
    /// The GitHub route label: "via local gh", "via node X", "unavailable: …".
    pub route: String,
}

/// The menu's text and enabled states for a [`Snap`].
#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub nodes: String,
    pub needs: String,
    pub factory: String,
    pub route: String,
    pub pause: bool,
    pub stop: bool,
    pub resume: bool,
    pub a2a: bool,
    /// Mute notifications (checked: muted).
    pub muted: bool,
    pub light: bool,
    pub dark: bool,
    pub tooltip: String,
}

pub fn menu_state(s: &Snap) -> MenuState {
    let word = match s.mode {
        Mode::Running => "Running",
        Mode::Pause => "Paused",
        Mode::Stop => "Stopped",
    };
    let nodes = format!("{} of {} nodes online", s.online, s.joined);
    let needs = format!("{} needs you", s.needs);
    MenuState {
        tooltip: format!("Keleustes · {word} · {nodes} · {needs}"),
        factory: format!("Factory: {word}"),
        nodes,
        needs,
        route: if s.route.is_empty() {
            "GitHub: checking".into()
        } else {
            format!("GitHub {}", s.route)
        },
        pause: s.mode == Mode::Running,
        stop: s.mode != Mode::Stop,
        resume: s.mode != Mode::Running,
        a2a: s.a2a_blackout,
        muted: s.muted,
        light: s.theme == "light",
        dark: s.theme == "dark",
    }
}

/// Reads the menu's snapshot.
pub async fn snapshot(hub: &Hub) -> Result<Snap> {
    let core = hub.core();
    let st = control::state(core).await?;
    let joined = core
        .store()
        .nodes()
        .await?
        .iter()
        .filter(|n| n.revoked_at.is_none())
        .count();
    let eff = config::Effective::load(core).await?;
    let theme = core
        .store()
        .setting(keys::THEME)
        .await?
        .unwrap_or_else(|| "dark".into());
    Ok(Snap {
        online: hub.online().await.len(),
        joined,
        needs: core.needs_you().await?.len(),
        mode: st.mode,
        a2a_blackout: st.a2a_blackout,
        muted: eff.notify_muted,
        theme,
        route: kls_forge::service(core).route().await.route.label(),
    })
}

/// Keeps the menu current: a fresh [`MenuState`] after core events
/// (bursts coalesced, at most about two a second) and every 15 s (nodes
/// coming and going). Call within the runtime.
pub fn feed(hub: Hub, tx: mpsc::Sender<MenuState>, wake: impl Fn() + Send + 'static) {
    tokio::spawn(async move {
        let mut bus = hub.core().subscribe();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        let mut last: Option<MenuState> = None;
        loop {
            match snapshot(&hub).await {
                Ok(s) => {
                    let st = menu_state(&s);
                    if last.as_ref() != Some(&st) {
                        if tx.send(st.clone()).is_err() {
                            return;
                        }
                        wake();
                        last = Some(st);
                    }
                }
                Err(e) => tracing::debug!("tray snapshot: {e:#}"),
            }
            tokio::select! {
                r = bus.recv() => {
                    if matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    while bus.try_recv().is_ok() {}
                }
                _ = tick.tick() => {}
            }
        }
    });
}

// ---- actions ---------------------------------------------------------------

/// What an action left for the window to show.
#[derive(Clone, Debug, PartialEq)]
pub enum Done {
    Nothing,
    /// A join token was minted: the `/connect` line to copy.
    Token {
        connect: String,
        expires_at: i64,
    },
    Info(String),
}

/// Runs a state-changing tray action through the governance API as
/// [`ORIGIN`]. Window-only actions (open, new session, stop's confirm,
/// quit) change nothing here. A refusal is audited and returned.
pub async fn perform(hub: &Hub, a: Action) -> Result<Done> {
    let core = hub.core();
    let r = run(hub, a).await;
    if let Err(e) = &r {
        refused(core, a, e).await;
    }
    r
}

async fn run(hub: &Hub, a: Action) -> Result<Done> {
    let core = hub.core();
    Ok(match a {
        Action::Pause => {
            control::set_mode(core, Mode::Pause, "", ORIGIN).await?;
            Done::Info("Paused".into())
        }
        Action::Resume => {
            control::set_mode(core, Mode::Running, "", ORIGIN).await?;
            Done::Info("Resumed".into())
        }
        Action::ToggleA2a => {
            let on = !control::state(core).await?.a2a_blackout;
            control::set_a2a_blackout(core, on, "", ORIGIN).await?;
            Done::Info(if on { "A2A blackout on" } else { "A2A relayed" }.into())
        }
        Action::ToggleMute => {
            let doc = core.settings_doc().await?;
            let muted = doc.values.get(keys::NOTIFY_MUTED) == Some(&json!(true));
            let ch = [(keys::NOTIFY_MUTED.to_string(), json!(!muted))].into();
            core.settings_save_as(doc.revision, ch, ORIGIN).await?;
            Done::Info(if muted { "Unmuted" } else { "Muted" }.into())
        }
        Action::Theme { dark } => {
            let doc = core.settings_doc().await?;
            let v = if dark { "dark" } else { "light" };
            let ch = [(keys::THEME.to_string(), json!(v))].into();
            core.settings_save_as(doc.revision, ch, ORIGIN).await?;
            Done::Nothing
        }
        Action::AddNode => {
            let minutes = match core.store().setting(keys::TOKEN_TTL_MINUTES).await? {
                Some(v) => v.trim().parse::<i64>().unwrap_or(15),
                None => 15,
            }
            .clamp(1, 7 * 24 * 60);
            let t = hub.mint_token_by("tray", minutes * 60_000, ORIGIN).await?;
            Done::Token {
                connect: format!("/connect {}", t.token),
                expires_at: t.expires_at,
            }
        }
        Action::Open(_) | Action::NewSession | Action::Stop | Action::Quit => Done::Nothing,
    })
}

async fn refused(core: &Core, a: Action, e: &anyhow::Error) {
    let d = kls_store::audit::LedgerDraft {
        kind: "tray".into(),
        actor: ORIGIN.into(),
        actor_kind: "user".into(),
        msg: format!("tray {} refused: {e:#}", a.name()),
        subject: json!({ "action": a.name() }),
        payload: json!({ "action": a.name(), "origin": ORIGIN, "outcome": "refused", "error": format!("{e:#}") }),
        ..Default::default()
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!("tray refusal audit: {e:#}");
    }
}

// ---- the menu --------------------------------------------------------------

fn glyph(g: &str, rgb: [u8; 3]) -> Option<MenuIcon> {
    MenuIcon::from_rgba(brand::glyph_rgba(g, 16, rgb), 16, 16).ok()
}

fn item(id: &str, g: &str, text: &str, enabled: bool) -> IconMenuItem {
    IconMenuItem::with_id(id, text, enabled, glyph(g, brand::MENU_GREY), None)
}

/// The live menu items, updated in place from a [`MenuState`].
struct Items {
    tray: TrayIcon,
    nodes: MenuItem,
    needs: MenuItem,
    factory: MenuItem,
    route: MenuItem,
    pause: IconMenuItem,
    stop: IconMenuItem,
    resume: IconMenuItem,
    a2a: CheckMenuItem,
    muted: CheckMenuItem,
    light: CheckMenuItem,
    dark: CheckMenuItem,
    shown: Option<MenuState>,
}

impl Items {
    fn apply(&mut self, s: &MenuState) {
        if self.shown.as_ref() == Some(s) {
            return;
        }
        self.nodes.set_text(&s.nodes);
        self.needs.set_text(&s.needs);
        self.factory.set_text(&s.factory);
        self.route.set_text(&s.route);
        self.pause.set_enabled(s.pause);
        self.stop.set_enabled(s.stop);
        self.resume.set_enabled(s.resume);
        self.a2a.set_checked(s.a2a);
        self.muted.set_checked(s.muted);
        self.light.set_checked(s.light);
        self.dark.set_checked(s.dark);
        let _ = self.tray.set_tooltip(Some(&s.tooltip));
        self.shown = Some(s.clone());
    }
}

/// A status line: text only, since GTK cannot relabel an item with an icon.
fn status(id: &str, text: &str) -> MenuItem {
    MenuItem::with_id(id, text, false, None)
}

fn build() -> Result<Items> {
    let menu = Menu::new();
    let nodes = status(ID_NODES, "Nodes");
    let needs = status(ID_NEEDS, "Needs you");
    let factory = status(ID_FACTORY, "Factory");
    let route = status(ID_ROUTE, "GitHub");
    menu.append_items(&[&nodes, &needs, &factory, &route])?;
    menu.append(&PredefinedMenuItem::separator())?;
    for (id, s) in OPEN {
        menu.append(&item(id, s.icon(), s.title(), true))?;
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item(
        ID_NEW_SESSION,
        icons::TERMINAL_WINDOW,
        "New session…",
        true,
    ))?;
    menu.append(&item(ID_ADD_NODE, icons::PLUS, "Add node", true))?;
    menu.append(&PredefinedMenuItem::separator())?;
    let pause = item(ID_PAUSE, icons::PAUSE, "Pause", true);
    let stop = item(ID_STOP, icons::STOP, "Stop…", true);
    let resume = item(ID_RESUME, icons::PLAY, "Resume", false);
    menu.append(&Submenu::with_items(
        "Factory",
        true,
        &[&pause, &stop, &resume],
    )?)?;
    let a2a = CheckMenuItem::with_id(ID_A2A, "A2A blackout", true, false, None);
    let muted = CheckMenuItem::with_id(ID_MUTE, "Mute notifications", true, false, None);
    menu.append_items(&[&a2a, &muted])?;
    let light = CheckMenuItem::with_id(ID_LIGHT, "Light", true, false, None);
    let dark = CheckMenuItem::with_id(ID_DARK, "Dark", true, true, None);
    menu.append(&Submenu::with_items("Theme", true, &[&light, &dark])?)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&item(ID_QUIT, icons::POWER, "Quit", true))?;
    let n = 32;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Keleustes")
        .with_icon(Icon::from_rgba(brand::icon_rgba(n), n, n)?)
        .build()?;
    Ok(Items {
        tray,
        nodes,
        needs,
        factory,
        route,
        pause,
        stop,
        resume,
        a2a,
        muted,
        light,
        dark,
        shown: None,
    })
}

fn forward(tx: mpsc::Sender<Action>, wake: impl Fn() + Send + Sync + 'static) {
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some(a) = action_for(&e.id.0) {
            let _ = tx.send(a);
            wake();
        }
    }));
}

/// A running tray. Keep it alive for as long as the tray should show.
pub struct Tray {
    /// Off Linux the menu lives on the window's thread.
    #[cfg(not(target_os = "linux"))]
    local: (Items, mpsc::Receiver<MenuState>),
}

impl Tray {
    /// Applies menu updates (off Linux; the GTK thread does its own).
    pub fn pump(&mut self) {
        #[cfg(not(target_os = "linux"))]
        {
            let (items, rx) = &mut self.local;
            if let Some(s) = rx.try_iter().last() {
                items.apply(&s);
            }
        }
    }
}

/// Starts the tray. `wake` is called after each action is queued (to
/// repaint the window so it reads the action); `states` carries menu
/// updates (see [`feed`]). None when no tray could be shown (no
/// status-notifier host, no GTK): closing then quits as usual.
#[cfg(target_os = "linux")]
pub fn start(
    tx: mpsc::Sender<Action>,
    states: mpsc::Receiver<MenuState>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Option<Tray> {
    let (ok_tx, ok_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if gtk::init().is_err() {
                let _ = ok_tx.send(false);
                return;
            }
            match build() {
                Ok(mut items) => {
                    forward(tx, wake);
                    let _ = ok_tx.send(true);
                    gtk::glib::timeout_add_local(
                        std::time::Duration::from_millis(250),
                        move || {
                            if let Some(s) = states.try_iter().last() {
                                items.apply(&s);
                            }
                            gtk::glib::ControlFlow::Continue
                        },
                    );
                    gtk::main();
                }
                Err(e) => {
                    tracing::warn!("tray: {e:#}");
                    let _ = ok_tx.send(false);
                }
            }
        })
        .ok()?;
    match ok_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(true) => Some(Tray {}),
        _ => None,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn start(
    tx: mpsc::Sender<Action>,
    states: mpsc::Receiver<MenuState>,
    wake: impl Fn() + Send + Sync + 'static,
) -> Option<Tray> {
    match build() {
        Ok(items) => {
            forward(tx, wake);
            Some(Tray {
                local: (items, states),
            })
        }
        Err(e) => {
            tracing::warn!("tray: {e:#}");
            None
        }
    }
}

// ---- the window side -------------------------------------------------------

/// Brings the window forward. Wayland ignores un-minimise and focus
/// without an activation token; attention at least lights it in the taskbar.
fn raise(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
        egui::UserAttentionType::Informational,
    ));
}

/// Puts `text` on the clipboard even while the window is minimised: a
/// Wayland data-control client when there is one, else egui's clipboard.
fn copy(ctx: &egui::Context, text: &str) {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        use std::io::Write;
        if let Ok(mut c) = std::process::Command::new("wl-copy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(text.as_bytes());
            }
            let _ = c.wait();
            return;
        }
    }
    ctx.copy_text(text.to_string());
    ctx.request_repaint();
}

fn os_notice(summary: &str, body: &str) {
    let (s, b) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new()
            .appname("Keleustes")
            .summary(&s)
            .body(&b)
            .icon("keleustes")
            .show()
        {
            tracing::debug!("notification: {e}");
        }
    });
}

/// Wraps the app: a close request minimises the window while the tray is
/// up (nodes keep being served), and the tray's Quit really closes it.
pub struct WithTray {
    pub app: kls_ui::App,
    pub model: kls_ui::Model,
    pub actions: mpsc::Receiver<Action>,
    pub active: bool,
    pub quitting: bool,
    pub tray: Option<Tray>,
}

impl WithTray {
    /// While the tray is up, a close request (title-bar X, Alt+F4, the
    /// compositor) minimises instead: nodes keep being served. Only the
    /// tray's Quit really closes.
    fn keep_running(&self, ctx: &egui::Context) {
        if self.active && !self.quitting && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    fn handle(&mut self, ctx: &egui::Context, a: Action) {
        use kls_ui::screens::{sessions, settings};
        match a {
            Action::Open(s) => {
                self.model.go(s, None);
                raise(ctx);
            }
            Action::NewSession => {
                self.model
                    .go(Screen::Sessions, Some(sessions::NEW_FOCUS.into()));
                raise(ctx);
            }
            Action::Stop => {
                self.model
                    .go(Screen::Settings, Some(settings::STOP_FROM_TRAY.into()));
                raise(ctx);
            }
            Action::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {
                let (m, ctx) = (self.model.clone(), ctx.clone());
                self.model.rt.spawn(async move {
                    match perform(&m.hub, a).await {
                        Ok(Done::Token {
                            connect,
                            expires_at,
                        }) => {
                            copy(&ctx, &connect);
                            os_notice("Join token copied", &connect);
                            let token = connect.trim_start_matches("/connect ").to_string();
                            m.notify(kls_ui::Notice::Token { token, expires_at });
                        }
                        Ok(Done::Info(i)) => m.notify(kls_ui::Notice::Info(i)),
                        Ok(Done::Nothing) => {}
                        Err(e) => {
                            os_notice("Keleustes", &format!("{e:#}"));
                            m.notify(kls_ui::Notice::Error(format!("{e:#}")));
                        }
                    }
                });
            }
        }
    }
}

impl eframe::App for WithTray {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(t) = &mut self.tray {
            t.pump();
        }
        while let Ok(a) = self.actions.try_recv() {
            self.handle(ctx, a);
        }
        self.keep_running(ctx);
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // eframe 0.37 reports the window's close request in the frame pass,
        // not in `logic`; check here too or the close goes through.
        self.keep_running(ui.ctx());
        self.app.ui(ui, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::audit::LedgerQuery;

    fn snap(mode: Mode) -> Snap {
        Snap {
            online: 2,
            joined: 3,
            needs: 1,
            mode,
            theme: "dark".into(),
            route: "via local gh".into(),
            ..Default::default()
        }
    }

    #[test]
    fn menu_follows_the_switch() {
        let r = menu_state(&snap(Mode::Running));
        assert_eq!(r.nodes, "2 of 3 nodes online");
        assert_eq!(r.needs, "1 needs you");
        assert_eq!(r.factory, "Factory: Running");
        assert_eq!(r.route, "GitHub via local gh");
        assert_eq!((r.pause, r.stop, r.resume), (true, true, false));
        assert!(r.dark && !r.light && !r.a2a && !r.muted);

        let p = menu_state(&snap(Mode::Pause));
        assert_eq!(p.factory, "Factory: Paused");
        assert_eq!((p.pause, p.stop, p.resume), (false, true, true));

        let s = menu_state(&Snap {
            a2a_blackout: true,
            muted: true,
            theme: "light".into(),
            ..snap(Mode::Stop)
        });
        assert_eq!(s.factory, "Factory: Stopped");
        assert_eq!((s.pause, s.stop, s.resume), (false, false, true));
        assert!(s.a2a && s.muted && s.light && !s.dark);
        assert!(s.tooltip.contains("Stopped"));
    }

    #[test]
    fn ids_map_to_actions() {
        assert_eq!(action_for("open.fleet"), Some(Action::Open(Screen::Fleet)));
        assert_eq!(action_for(ID_STOP), Some(Action::Stop));
        assert_eq!(action_for(ID_DARK), Some(Action::Theme { dark: true }));
        assert_eq!(action_for(ID_NODES), None, "status lines do nothing");
        for id in [
            ID_NEW_SESSION,
            ID_ADD_NODE,
            ID_PAUSE,
            ID_RESUME,
            ID_A2A,
            ID_MUTE,
            ID_QUIT,
        ] {
            assert!(action_for(id).is_some(), "{id}");
        }
    }

    async fn hub() -> (Hub, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        (kls_node::hub(core).await.unwrap(), dir)
    }

    async fn ledger(core: &Core) -> Vec<kls_store::audit::LedgerEntry> {
        core.audit_sync().await.unwrap();
        core.audit_entries(&LedgerQuery::default()).await.unwrap()
    }

    #[tokio::test]
    async fn factory_actions_are_governed_and_audited_as_the_tray() {
        let (hub, _d) = hub().await;
        let core = hub.core().clone();
        perform(&hub, Action::Pause).await.unwrap();
        assert_eq!(control::state(&core).await.unwrap().mode, Mode::Pause);
        assert!(menu_state(&snapshot(&hub).await.unwrap()).resume);

        // Stop from the tray only opens the confirm: nothing changes.
        assert_eq!(perform(&hub, Action::Stop).await.unwrap(), Done::Nothing);
        assert_eq!(control::state(&core).await.unwrap().mode, Mode::Pause);

        // Pausing again is refused, and the refusal is chained.
        let e = perform(&hub, Action::Pause).await.unwrap_err();
        assert!(e.to_string().contains("already paused"), "{e}");
        let l = ledger(&core).await;
        let mode = l
            .iter()
            .find(|e| e.msg.starts_with("factory.mode"))
            .unwrap();
        assert_eq!(mode.actor, ORIGIN);
        let r = l.iter().find(|e| e.kind == "tray").unwrap();
        assert_eq!(r.actor, ORIGIN);
        assert!(r.msg.contains("pause refused"), "{}", r.msg);

        perform(&hub, Action::Resume).await.unwrap();
        assert_eq!(control::state(&core).await.unwrap().mode, Mode::Running);

        // A2A blackout through the same API; agent paths now refuse.
        perform(&hub, Action::ToggleA2a).await.unwrap();
        assert!(control::state(&core).await.unwrap().a2a_blackout);
        assert!(
            control::check_a2a(&core, "n1:s1", "n2:s2", "test")
                .await
                .is_err()
        );
        let l = ledger(&core).await;
        assert!(
            l.iter()
                .any(|e| e.actor == ORIGIN && e.msg.contains("a2aBlackout=true")),
            "{:?}",
            l.iter().map(|e| (&e.actor, &e.msg)).collect::<Vec<_>>()
        );
        assert!(l.iter().any(|e| e.kind == "a2a_withheld"));
        perform(&hub, Action::ToggleA2a).await.unwrap();
        assert!(!control::state(&core).await.unwrap().a2a_blackout);
    }

    #[tokio::test]
    async fn settings_and_tokens_record_the_tray() {
        let (hub, _d) = hub().await;
        let core = hub.core().clone();
        perform(&hub, Action::ToggleMute).await.unwrap();
        assert!(snapshot(&hub).await.unwrap().muted);
        perform(&hub, Action::Theme { dark: false }).await.unwrap();
        assert_eq!(snapshot(&hub).await.unwrap().theme, "light");
        let l = ledger(&core).await;
        for key in [keys::NOTIFY_MUTED, keys::THEME] {
            let e = l
                .iter()
                .find(|e| e.kind == "setting" && e.msg.starts_with(key))
                .unwrap();
            assert_eq!(e.actor, ORIGIN, "{key}");
        }

        // Add node mints a single-use token while paused too (it is not a
        // dispatch); the ledger names the tray.
        control::pause(&core, "").await.unwrap();
        let Done::Token { connect, .. } = perform(&hub, Action::AddNode).await.unwrap() else {
            panic!("no token");
        };
        assert!(connect.starts_with("/connect "));
        assert_eq!(core.store().list_join_tokens().await.unwrap().len(), 1);
        let l = ledger(&core).await;
        let t = l.iter().find(|e| e.kind == "token").unwrap();
        assert_eq!(t.actor, ORIGIN);
    }
}
