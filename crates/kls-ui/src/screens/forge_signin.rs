//! Sign a forge CLI in (github.com through `gh` by default): browser
//! (one-time device code copied and the device page opened; or the CLI's
//! own browser flow with a countdown) or a guided token walk-through
//! (pre-filled token page where the forge has one, a scopes checklist where
//! it does not). And the Connect dialog that adds a forge connection.

use crate::icons;
use crate::kit;
use crate::model::{Model, Notice};
use crate::theme;
use kls_forge::forge::auth::{CliProbe, SignInMethod, auth_for};
use kls_forge::forge::server::ServerInfo;
use kls_forge::login::{WebLogin, WebState};
use kls_forge::{CliKind, Connection, ForgeKind};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default, PartialEq, Eq)]
enum TokenState {
    #[default]
    Idle,
    Busy,
    Done,
    Failed(String),
}

/// The sign-in methods a CLI offers for a host, and its probe.
type Offered = (Vec<SignInMethod>, CliProbe);

/// The local command shown for hosts without a window.
pub const HEADLESS_CMD: &str = "keleustes github login --with-token < token.txt";

fn headless_cmd(conn: &Connection) -> String {
    if conn.is_github_com() {
        HEADLESS_CMD.into()
    } else {
        format!(
            "keleustes forge login --kind {} --host {} --with-token < token.txt",
            conn.kind.id(),
            conn.host
        )
    }
}

pub struct SignIn {
    open: bool,
    conn: Connection,
    mode: SignInMethod,
    /// Methods the CLI offers for this host (github.com: device, token).
    methods: Arc<Mutex<Option<Offered>>>,
    web: Option<WebLogin>,
    /// The device code (or URL) already copied and opened in the browser.
    handled: Option<String>,
    token: String,
    token_state: Arc<Mutex<TokenState>>,
    /// Add scopes to the signed-in account (`gh auth refresh`).
    refresh: bool,
    /// Start the browser flow on the next frame (deep link).
    autostart: bool,
    /// Open a connection by id on the next frame (deep link).
    pending_id: Arc<Mutex<Option<String>>>,
    /// The connection as loaded (a deep link names only its id).
    fresh: Option<Arc<Mutex<Option<Connection>>>>,
}

impl Default for SignIn {
    fn default() -> Self {
        Self {
            open: false,
            conn: Connection::github_com(),
            mode: SignInMethod::DeviceCode,
            methods: Arc::default(),
            web: None,
            handled: None,
            token: String::new(),
            token_state: Arc::default(),
            refresh: false,
            autostart: false,
            pending_id: Arc::default(),
            fresh: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Ok,
    Warn,
    Unknown,
}

/// The Connect dialog's server chip: `✓ GitLab 18.4`; a warning when the
/// server is another kind than the one picked (Forgejo picked as Gitea);
/// `? Server` when it did not say.
fn server_chip(s: &ServerInfo, picked: ForgeKind) -> (String, Tone) {
    if !s.known() {
        return (icons::label(icons::QUESTION, "Server"), Tone::Unknown);
    }
    match s.kind() {
        Some(k) if k != picked => (icons::label(icons::WARNING, &s.chip()), Tone::Warn),
        _ => (icons::label(icons::CHECK, &s.chip()), Tone::Ok),
    }
}

fn github_methods() -> (Vec<SignInMethod>, CliProbe) {
    (
        vec![SignInMethod::DeviceCode, SignInMethod::Token],
        CliProbe {
            installed: true,
            ..Default::default()
        },
    )
}

impl SignIn {
    /// Opens the dialog for github.com: a new sign-in, or `refresh` to
    /// grant the missing scopes to the signed-in account.
    pub fn open(&mut self, refresh: bool) {
        self.open_conn(Connection::github_com(), refresh);
    }

    /// Opens the dialog for one connection.
    pub fn open_conn(&mut self, conn: Connection, refresh: bool) {
        let keep = self.mode;
        *self = Self {
            open: true,
            refresh,
            mode: keep,
            ..Self::default()
        };
        if conn.is_github_com() {
            *self.methods.lock().unwrap() = Some(github_methods());
        }
        if refresh && conn.cli() != CliKind::Gh {
            // Other CLIs have no scope refresh: a new token with every scope.
            self.mode = SignInMethod::Token;
            self.refresh = false;
        }
        self.conn = conn;
    }

    /// Opens the dialog for a connection id (deep link); resolved on the next frame.
    pub fn open_id(&mut self, id: &str, refresh: bool) {
        self.open_conn(Connection::github_com(), refresh);
        *self.pending_id.lock().unwrap() = Some(id.to_string());
        *self.methods.lock().unwrap() = None;
    }

    /// From a deep link: pick the Token tab, or start the browser flow at once.
    pub fn preset(&mut self, token: bool, start_browser: bool) {
        if token {
            self.mode = SignInMethod::Token;
        } else if start_browser {
            self.mode = SignInMethod::DeviceCode;
            self.autostart = true;
        }
    }

    fn load(&mut self, model: &Model) {
        if self.methods.lock().unwrap().is_some() {
            return;
        }
        let slot = self.methods.clone();
        let pending = self.pending_id.lock().unwrap().take();
        let conn = self.conn.clone();
        let fresh = Arc::new(Mutex::new(None::<Connection>));
        *slot.lock().unwrap() = Some((Vec::new(), CliProbe::default()));
        let got = fresh.clone();
        model.act(move |m| async move {
            let svc = kls_forge::service(m.core());
            let conn = match pending {
                Some(id) => svc.connection(&id).await?,
                None => conn,
            };
            let probe = svc.probe(&conn).await;
            let methods = auth_for(conn.cli()).methods(&probe, &conn);
            *slot.lock().unwrap() = Some((methods, probe));
            *got.lock().unwrap() = Some(conn);
            m.egui_ctx().request_repaint();
            Ok(None)
        });
        self.fresh = Some(fresh);
    }

    /// Draws the dialog while it is open. True on the frame a sign-in
    /// succeeded.
    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) -> bool {
        if !self.open {
            return false;
        }
        self.load(model);
        if let Some(f) = &self.fresh
            && let Some(c) = f.lock().unwrap().take()
        {
            self.conn = c;
        }
        let (methods, probe) = self.methods.lock().unwrap().clone().unwrap_or_default();
        if !methods.is_empty() && !methods.contains(&self.mode) {
            self.mode = methods[0];
        }
        let mut done = false;
        if !methods.is_empty()
            && std::mem::take(&mut self.autostart)
            && self.web.is_none()
            && self.mode != SignInMethod::Token
        {
            self.start_web(model);
        }
        let p = theme::palette(ui.ctx());
        let mut close = false;
        let conn = self.conn.clone();
        let icon = super::repos::kind_icon(conn.kind, &conn.host);
        let resp = egui::Modal::new(egui::Id::unique("forge_signin"))
            .frame(kit::glow(ui).inner_margin(20.0))
            .show(ui.ctx(), |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    let title = if self.refresh {
                        "Grant scopes".to_string()
                    } else if conn.is_github_com() {
                        "Sign in to GitHub".to_string()
                    } else {
                        format!("Sign in to {}", conn.name())
                    };
                    ui.label(
                        egui::RichText::new(icons::label(icon, &title))
                            .size(18.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(icons::X).frame(false))
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                let page = auth_for(conn.cli()).token_page(&conn);
                ui.horizontal_wrapped(|ui| {
                    kit::chip(
                        ui,
                        &icons::label(icons::TERMINAL, &format!("local {}", conn.cli().name())),
                        p.cyan,
                    );
                    kit::chip(ui, &conn.host, p.muted);
                    kit::chip(
                        ui,
                        &icons::label(icons::KEY, &format!("{} scopes", page.scopes.len())),
                        p.gold,
                    )
                    .on_hover_text(page.scopes.join("\n"));
                });
                ui.add_space(10.0);
                if methods.is_empty() {
                    ui.spinner();
                    return false;
                }
                if !probe.installed {
                    install(ui, conn.cli(), &probe.error);
                    ui.add_space(6.0);
                    if kit::ghost_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Check again"))
                        .clicked()
                    {
                        *self.methods.lock().unwrap() = None;
                    }
                    return false;
                }
                ui.horizontal(|ui| {
                    for m in &methods {
                        let (ic, label) = match m {
                            // gh's device code runs in the browser: keep its label.
                            SignInMethod::DeviceCode if conn.cli() == CliKind::Gh => {
                                (icons::GLOBE, "Browser")
                            }
                            SignInMethod::DeviceCode => (icons::DEVICE_MOBILE, "Device"),
                            SignInMethod::BrowserLoopback => (icons::GLOBE, "Browser"),
                            SignInMethod::Token => (icons::KEY, "Token"),
                        };
                        if kit::tab(ui, &icons::label(ic, label), self.mode == *m).clicked()
                            && self.mode != *m
                        {
                            self.mode = *m;
                            if let Some(w) = self.web.take()
                                && !w.state().finished()
                            {
                                w.cancel();
                            }
                        }
                    }
                });
                ui.add_space(12.0);
                let ok = match self.mode {
                    SignInMethod::Token => self.token(ui, model),
                    _ => self.browser(ui, model),
                };
                if ok {
                    ui.add_space(8.0);
                    if theme::primary_button(ui, &icons::label(icons::CHECK, "Done")).clicked() {
                        close = true;
                    }
                }
                ok
            });
        if resp.inner && self.handled.as_deref() != Some("done") {
            self.handled = Some("done".into());
            done = true;
        }
        if close || resp.should_close() {
            self.close();
        }
        done
    }

    fn close(&mut self) {
        if let Some(w) = self.web.take()
            && !w.state().finished()
        {
            w.cancel();
        }
        *self = Self {
            mode: self.mode,
            ..Self::default()
        };
    }

    /// Returns true once signed in.
    fn browser(&mut self, ui: &mut egui::Ui, model: &Model) -> bool {
        let p = theme::palette(ui.ctx());
        let state = self.web.as_ref().map(WebLogin::state);
        match &state {
            None | Some(WebState::Cancelled) => {
                if theme::primary_button(ui, &icons::label(icons::GLOBE, "Start")).clicked() {
                    self.start_web(model);
                }
            }
            Some(WebState::Starting) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(format!("Starting {}", self.conn.cli().name()))
                            .color(p.muted),
                    );
                });
            }
            Some(WebState::Code { code, url }) => {
                if self.handled.as_deref() != Some(code) {
                    self.handled = Some(code.clone());
                    ui.ctx().copy_text(code.clone());
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
                ui.label(kit::caps("One-time code", p.muted));
                ui.horizontal(|ui| {
                    ui.label(kit::mono(code, 32.0, p.gold).strong());
                    if kit::ghost_button(ui, icons::COPY)
                        .on_hover_text("Copy code")
                        .clicked()
                    {
                        ui.ctx().copy_text(code.clone());
                    }
                });
                ui.add_space(6.0);
                if kit::ghost_button(ui, &icons::label(icons::ARROW_SQUARE_OUT, url))
                    .on_hover_text("Open in browser")
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Waiting for approval").color(p.muted));
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked()
                        && let Some(w) = &self.web
                    {
                        w.cancel();
                    }
                });
            }
            Some(WebState::Browser { url, until }) => {
                // The CLI opens the browser itself; Open again if it did not.
                let left = ((until - kls_store::now_ms()) / 1000).max(0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Approve in browser").color(p.muted));
                    ui.label(kit::mono(
                        &icons::label(icons::TIMER, &format!("{}:{:02}", left / 60, left % 60)),
                        13.0,
                        if left < 15 { p.amber } else { p.muted },
                    ));
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if !url.is_empty()
                        && kit::ghost_button(
                            ui,
                            &icons::label(icons::ARROW_SQUARE_OUT, "Open again"),
                        )
                        .on_hover_text(url)
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked()
                        && let Some(w) = &self.web
                    {
                        w.cancel();
                    }
                });
            }
            Some(WebState::Done) => {
                signed_in(ui, &self.conn);
                return true;
            }
            Some(WebState::Failed(e)) => {
                ui.colored_label(p.red, icons::label(icons::WARNING_CIRCLE, e));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::primary_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Retry"))
                        .clicked()
                    {
                        self.start_web(model);
                    }
                    if kit::ghost_button(ui, &icons::label(icons::KEY, "Use token")).clicked() {
                        self.mode = SignInMethod::Token;
                    }
                });
            }
        }
        false
    }

    fn start_web(&mut self, model: &Model) {
        let login = if self.conn.is_github_com() {
            if self.refresh {
                WebLogin::refresh()
            } else {
                WebLogin::default()
            }
        } else {
            WebLogin::for_conn(&self.conn, self.mode, self.refresh)
        };
        self.web = Some(login.clone());
        self.handled = None;
        let name = if self.conn.is_github_com() {
            "GitHub".to_string()
        } else {
            self.conn.name().to_string()
        };
        model.act(move |m| async move {
            let ctx = m.egui_ctx();
            let route = kls_forge::service(m.core())
                .login_web(&login, move || ctx.request_repaint())
                .await;
            Ok(match login.state() {
                WebState::Done => Some(Notice::Info(format!("{name} {}", route.label()))),
                _ => None,
            })
        });
    }

    /// Returns true once signed in.
    fn token(&mut self, ui: &mut egui::Ui, model: &Model) -> bool {
        let p = theme::palette(ui.ctx());
        let state = self.token_state.lock().unwrap().clone();
        if state == TokenState::Done {
            signed_in(ui, &self.conn);
            return true;
        }
        let conn = self.conn.clone();
        let page = auth_for(conn.cli()).token_page(&conn);
        let mut n = 0u8;
        let mut next = || {
            n += 1;
            n
        };
        let title = if conn.is_github_com() {
            "Create a classic token"
        } else {
            "Create a token"
        };
        step(ui, next(), title, |ui| {
            if kit::ghost_button(ui, &icons::label(icons::ARROW_SQUARE_OUT, "New token"))
                .on_hover_text(if page.prefilled {
                    "Opens the token page with every scope ticked"
                } else {
                    "Opens the token page"
                })
                .clicked()
            {
                let url = if conn.is_github_com() {
                    kls_forge::login::token_url()
                } else {
                    page.url.clone()
                };
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            ui.label(
                egui::RichText::new(if page.prefilled {
                    "Pick an expiry · Generate · Copy"
                } else {
                    "Name: Keleustes · tick the scopes below · Generate · Copy"
                })
                .size(12.0)
                .color(p.muted),
            );
        });
        if !page.prefilled {
            step(ui, next(), "Tick", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for s in &page.scopes {
                        kit::chip(ui, &icons::label(icons::CHECK, s), p.gold);
                    }
                    if kit::ghost_button(ui, &icons::label(icons::COPY, "Copy list")).clicked() {
                        ui.ctx().copy_text(page.scopes.join("\n"));
                    }
                });
            });
        }
        if page.expiry_required {
            step(ui, next(), "Expiry", |ui| {
                ui.label(
                    egui::RichText::new(icons::label(icons::TIMER, "up to 365 days (required)"))
                        .size(12.0)
                        .color(p.muted),
                );
            });
        }
        let hint = match conn.cli() {
            CliKind::Gh => "ghp_…",
            CliKind::Glab => "glpat-…",
            CliKind::Tea => "token",
        };
        step(ui, next(), "Paste it", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.token)
                    .password(true)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
        });
        step(ui, next(), "Sign in", |ui| {
            let busy = state == TokenState::Busy;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled_ui(!busy && !self.token.trim().is_empty(), |ui| {
                        theme::primary_button(ui, &icons::label(icons::KEY, "Sign in"))
                    })
                    .inner
                    .clicked()
                {
                    let token = std::mem::take(&mut self.token);
                    let st = self.token_state.clone();
                    *st.lock().unwrap() = TokenState::Busy;
                    let conn = conn.clone();
                    model.act(move |m| async move {
                        let svc = kls_forge::service(m.core());
                        let r = if conn.is_github_com() {
                            svc.login_with_token(&token).await
                        } else {
                            svc.login_token_conn(&conn, &token, "user").await
                        };
                        let (next, notice) = match r {
                            Ok(route) => (
                                TokenState::Done,
                                Some(Notice::Info(format!("{} {}", conn.name(), route.label()))),
                            ),
                            Err(e) => (TokenState::Failed(format!("{e:#}")), None),
                        };
                        *st.lock().unwrap() = next;
                        m.egui_ctx().request_repaint();
                        Ok(notice)
                    });
                }
                if busy {
                    ui.spinner();
                }
            });
            if let TokenState::Failed(e) = &state {
                ui.colored_label(p.red, icons::label(icons::WARNING_CIRCLE, e));
            }
        });
        ui.add_space(10.0);
        kit::rule(ui);
        ui.add_space(6.0);
        ui.label(kit::caps(
            &icons::label(icons::TERMINAL_WINDOW, "Headless host"),
            p.muted,
        ));
        let cmd = headless_cmd(&conn);
        ui.horizontal(|ui| {
            ui.label(kit::mono(&cmd, 12.0, p.text));
            if kit::ghost_button(ui, icons::COPY)
                .on_hover_text("Copy")
                .clicked()
            {
                ui.ctx().copy_text(cmd.clone());
            }
        });
        false
    }
}

fn step(ui: &mut egui::Ui, n: u8, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui.ctx());
    ui.horizontal_top(|ui| {
        ui.label(kit::mono(&n.to_string(), 16.0, p.gold).strong());
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).strong());
            body(ui);
        });
    });
    ui.add_space(8.0);
}

fn signed_in(ui: &mut egui::Ui, conn: &Connection) {
    let p = theme::palette(ui.ctx());
    ui.label(
        egui::RichText::new(icons::label(
            icons::CHECK_CIRCLE,
            &format!("Signed in · via local {}", conn.cli().name()),
        ))
        .size(16.0)
        .color(p.green)
        .strong(),
    );
}

/// The CLI is missing: install commands with Copy, and the docs.
fn install(ui: &mut egui::Ui, cli: CliKind, why: &str) {
    let p = theme::palette(ui.ctx());
    ui.colored_label(
        p.red,
        icons::label(icons::X_CIRCLE, &format!("{} not installed", cli.name())),
    )
    .on_hover_text(why);
    for (os, cmd) in cli.install() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(*os).size(12.0).color(p.muted));
            ui.label(kit::mono(&format!("$ {cmd}"), 12.0, p.text));
            if kit::ghost_button(ui, icons::COPY)
                .on_hover_text("Copy")
                .clicked()
            {
                ui.ctx().copy_text(cmd.to_string());
            }
        });
    }
    ui.hyperlink_to(
        icons::label(icons::ARROW_SQUARE_OUT, "Docs"),
        cli.docs_url(),
    );
}

// ---- Connect forge -----------------------------------------------------------

/// The kinds the Connect dialog offers (Codeberg is Forgejo on codeberg.org).
const CHOICES: [(&str, ForgeKind, Option<&str>); 6] = [
    ("GitHub", ForgeKind::Github, Some("github.com")),
    ("GitHub Enterprise", ForgeKind::Ghes, None),
    ("GitLab", ForgeKind::Gitlab, Some("gitlab.com")),
    ("Gitea", ForgeKind::Gitea, Some("gitea.com")),
    ("Forgejo", ForgeKind::Forgejo, None),
    ("Codeberg", ForgeKind::Forgejo, Some("codeberg.org")),
];

#[derive(Default)]
pub struct Connect {
    open: bool,
    choice: usize,
    host: String,
    probe: Arc<Mutex<Option<CliProbe>>>,
    busy: bool,
    made: Arc<Mutex<Option<Connection>>>,
}

impl Connect {
    /// Opens the dialog, with a kind preselected (`gitlab`, `codeberg`, …).
    pub fn open(&mut self, kind: &str) {
        *self = Self {
            open: true,
            ..Self::default()
        };
        if let Some(i) = CHOICES
            .iter()
            .position(|c| c.0.eq_ignore_ascii_case(kind) || c.1.id() == kind && !kind.is_empty())
        {
            self.choice = i;
        }
        self.host = CHOICES[self.choice].2.unwrap_or("").into();
    }

    /// Draws the dialog; returns the connection once added (open its sign-in).
    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) -> Option<Connection> {
        if let Some(c) = self.made.lock().unwrap().take() {
            self.open = false;
            self.busy = false;
            return Some(c);
        }
        if !self.open {
            return None;
        }
        let p = theme::palette(ui.ctx());
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::unique("forge_connect"))
            .frame(kit::glow(ui).inner_margin(20.0))
            .show(ui.ctx(), |ui| {
                ui.set_width(480.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(icons::label(icons::PLUS, "Connect forge"))
                            .size(18.0)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(icons::X).frame(false))
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, (label, kind, host)) in CHOICES.iter().enumerate() {
                        let ic = super::repos::kind_icon(*kind, host.unwrap_or(""));
                        if kit::pick_chip(ui, &icons::label(ic, label), self.choice == i, p.cyan)
                            .clicked()
                        {
                            self.choice = i;
                            self.host = host.unwrap_or("").into();
                            *self.probe.lock().unwrap() = None;
                        }
                    }
                });
                ui.add_space(10.0);
                let (_, kind, fixed) = CHOICES[self.choice];
                let read_only = matches!(fixed, Some("github.com" | "codeberg.org"));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Host").color(p.muted));
                    ui.add_enabled(
                        !read_only,
                        egui::TextEdit::singleline(&mut self.host)
                            .hint_text("git.example.com")
                            .desired_width(280.0),
                    );
                    if kit::ghost_button(ui, &icons::label(icons::MAGNIFYING_GLASS, "Check"))
                        .clicked()
                    {
                        let slot = self.probe.clone();
                        let host = self.host.clone();
                        model.act(move |m| async move {
                            let svc = kls_forge::service(m.core());
                            let conn = Connection::new(kind, &host, "user")
                                .unwrap_or_else(Connection::github_com);
                            let pr = svc.probe(&conn).await;
                            *slot.lock().unwrap() = Some(pr);
                            m.egui_ctx().request_repaint();
                            Ok(None)
                        });
                    }
                });
                let host_ok = kls_forge::forge::normalize_host(&self.host).is_some();
                if let Some(pr) = self.probe.lock().unwrap().clone() {
                    ui.add_space(6.0);
                    if pr.installed {
                        let conn = Connection::new(kind, &self.host, "user")
                            .unwrap_or_else(Connection::github_com);
                        let methods = auth_for(kind.cli()).methods(&pr, &conn);
                        ui.horizontal_wrapped(|ui| {
                            if let Some(s) = &pr.server {
                                let (label, tone) = server_chip(s, kind);
                                let color = match tone {
                                    Tone::Ok => p.green,
                                    Tone::Warn => p.amber,
                                    Tone::Unknown => p.muted,
                                };
                                kit::chip(ui, &label, color);
                            }
                            kit::chip(
                                ui,
                                &icons::label(
                                    icons::CHECK,
                                    &format!("{} {}", kind.cli().name(), pr.version),
                                ),
                                p.green,
                            );
                            for (m, label) in [
                                (SignInMethod::DeviceCode, "device"),
                                (SignInMethod::BrowserLoopback, "browser"),
                                (SignInMethod::Token, "token"),
                            ] {
                                let on = methods.contains(&m);
                                kit::chip(
                                    ui,
                                    &icons::label(if on { icons::CHECK } else { icons::X }, label),
                                    if on { p.green } else { p.muted },
                                );
                            }
                        });
                    } else {
                        install(ui, kind.cli(), &pr.error);
                    }
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled_ui(host_ok && !self.busy, |ui| {
                            theme::primary_button(ui, &icons::label(icons::ARROW_RIGHT, "Continue"))
                        })
                        .inner
                        .clicked()
                    {
                        self.busy = true;
                        let host = self.host.clone();
                        let label = if CHOICES[self.choice].0 == "Codeberg" {
                            "Codeberg".to_string()
                        } else {
                            String::new()
                        };
                        let made = self.made.clone();
                        model.act(move |m| async move {
                            let c = kls_forge::service(m.core())
                                .add_connection(kind, &host, &label, "user")
                                .await?;
                            *made.lock().unwrap() = Some(c);
                            m.egui_ctx().request_repaint();
                            Ok(None)
                        });
                    }
                    if self.busy {
                        ui.spinner();
                    }
                });
            });
        if close || resp.should_close() {
            self.open = false;
            self.busy = false;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_forge::forge::server::parse;
    use serde_json::json;

    #[test]
    fn server_chip_says_what_the_server_is() {
        let fj = parse(ForgeKind::Gitea, &json!({"version": "13.0.5+gitea-1.22.0"}));
        // Picked Forgejo: a plain check. Picked Gitea: a warning, same words.
        assert_eq!(
            server_chip(&fj, ForgeKind::Forgejo),
            (icons::label(icons::CHECK, "Forgejo 13.0"), Tone::Ok)
        );
        assert_eq!(server_chip(&fj, ForgeKind::Gitea).1, Tone::Warn);
        assert_eq!(
            server_chip(&ServerInfo::default(), ForgeKind::Gitlab),
            (icons::label(icons::QUESTION, "Server"), Tone::Unknown)
        );
    }
}
