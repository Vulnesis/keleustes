//! Host view: one belai node opened from Fleet, with everything on it that
//! can be configured, managed or dispatched (docs/gaps/host-management.md
//! §3). Every action goes through `kls_core::hosts::request_action`, which
//! enforces the kill switch, the A2A blackout, the host gate and the
//! per-class confirmations; this file only gates buttons so a person sees
//! why one is disabled.
//!
//! Fleet calls [`show`] each frame and [`open`] on a row click; deep links
//! `keleustes://fleet/<node>[/<tab>][?item=…]` arrive through [`open_link`].

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice, Snapshot};
use crate::theme::{palette, primary_button};
use egui::RichText;
use kls_core::hosts::{
    self, ActionRequest, Class, HostActionRowExt, HostView, Inventory, Level, Origin, PlanStep,
};
use kls_store::host::HostActionRow;
use kls_store::sessions::{Session, SessionQuery};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

// ---- tabs and routing ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Products,
    Repos,
    Agents,
    Schedules,
    Library,
    Config,
    Sessions,
    Provision,
    Diagnostics,
    Audit,
}

impl Tab {
    pub const ALL: [Tab; 11] = [
        Tab::Overview,
        Tab::Products,
        Tab::Repos,
        Tab::Agents,
        Tab::Schedules,
        Tab::Library,
        Tab::Config,
        Tab::Sessions,
        Tab::Provision,
        Tab::Diagnostics,
        Tab::Audit,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Products => "products",
            Tab::Repos => "repos",
            Tab::Agents => "agents",
            Tab::Schedules => "schedules",
            Tab::Library => "library",
            Tab::Config => "config",
            Tab::Sessions => "sessions",
            Tab::Provision => "provision",
            Tab::Diagnostics => "diagnostics",
            Tab::Audit => "audit",
        }
    }

    /// An unknown slug opens the overview.
    pub fn parse(s: &str) -> Tab {
        Tab::ALL
            .into_iter()
            .find(|t| t.slug() == s.trim().to_ascii_lowercase())
            .unwrap_or(Tab::Overview)
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::Overview => icons::GAUGE,
            Tab::Products => icons::PACKAGE,
            Tab::Repos => icons::GIT_BRANCH,
            Tab::Agents => icons::ROBOT,
            Tab::Schedules => icons::CALENDAR,
            Tab::Library => icons::BOOKS,
            Tab::Config => icons::SLIDERS,
            Tab::Sessions => icons::TERMINAL_WINDOW,
            Tab::Provision => icons::WRENCH,
            Tab::Diagnostics => icons::STETHOSCOPE,
            Tab::Audit => icons::SCROLL,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Products => "Products",
            Tab::Repos => "Repos",
            Tab::Agents => "Agents",
            Tab::Schedules => "Schedules",
            Tab::Library => "Library",
            Tab::Config => "Config",
            Tab::Sessions => "Sessions",
            Tab::Provision => "Provision",
            Tab::Diagnostics => "Diagnostics",
            Tab::Audit => "Audit",
        }
    }
}

/// A parsed `fleet/<node>[/<tab>][?item=…]` focus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub node: String,
    pub tab: Tab,
    pub item: Option<String>,
}

/// Splits a Fleet focus (`n1`, `n1/products`, `n1/products?item=cli`).
pub fn parse_target(focus: &str) -> Option<Target> {
    let (path, query) = focus.split_once('?').unwrap_or((focus, ""));
    let mut parts = path.trim_matches('/').splitn(3, '/');
    let node = parts.next().filter(|n| !n.is_empty())?.to_string();
    let tab = parts.next().map(Tab::parse).unwrap_or(Tab::Overview);
    let item = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("item="))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| parts.next().filter(|s| !s.is_empty()).map(str::to_string));
    Some(Target { node, tab, item })
}

/// The deep link for a node's tab.
pub fn link(node: &str, tab: Tab) -> String {
    format!("keleustes://fleet/{node}/{}", tab.slug())
}

#[derive(Default)]
struct Slot {
    pending: Option<Target>,
    screen: Option<HostScreen>,
    close: bool,
}

type Shared = Arc<Mutex<Slot>>;

fn slot(ctx: &egui::Context) -> Shared {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(egui::Id::unique("kls.host.view"), || {
            Arc::new(Mutex::new(Slot::default()))
        })
        .clone()
    })
}

/// Opens a node's view (Fleet row click).
pub fn open(ctx: &egui::Context, node_id: &str, tab: Option<Tab>) {
    slot(ctx).lock().unwrap().pending = Some(Target {
        node: node_id.into(),
        tab: tab.unwrap_or(Tab::Overview),
        item: None,
    });
    ctx.request_repaint();
}

/// Opens a node's view from a deep-link focus.
pub fn open_link(ctx: &egui::Context, focus: &str) {
    if let Some(t) = parse_target(focus) {
        slot(ctx).lock().unwrap().pending = Some(t);
        ctx.request_repaint();
    }
}

/// Closes the view (back to the Fleet table).
pub fn close(ctx: &egui::Context) {
    slot(ctx).lock().unwrap().close = true;
}

/// Draws the open host view, if any. Returns whether it drew.
pub fn show(ui: &mut egui::Ui, model: &Model, snap: &Snapshot) -> bool {
    let shared = slot(ui.ctx());
    let mut screen = {
        let mut s = shared.lock().unwrap();
        if std::mem::take(&mut s.close) {
            s.screen = None;
            s.pending = None;
        }
        if let Some(t) = s.pending.take() {
            match s.screen.as_mut() {
                Some(sc) if sc.node_id == t.node => sc.focus(t.tab, t.item),
                _ => s.screen = Some(HostScreen::new(model, t)),
            }
        }
        match s.screen.take() {
            Some(sc) => sc,
            None => return false,
        }
    };
    let keep = screen.ui(ui, model, snap);
    let mut s = shared.lock().unwrap();
    if keep && s.screen.is_none() && !s.close {
        s.screen = Some(screen);
    }
    true
}

// ---- gating ---------------------------------------------------------------------------

/// Why an action button is hidden or disabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateState {
    /// The host does not list the kind: hide it.
    Hidden,
    Disabled(String),
    Enabled,
}

/// The facts gating uses (from the host view).
#[derive(Clone, Debug, Default)]
pub struct Gates {
    pub online: bool,
    pub stopped: bool,
    pub level: Option<Level>,
    pub offered: Vec<String>,
    pub shell: bool,
    /// The host runs with --web-project-settings (per-repo prefs).
    pub project_settings: bool,
}

impl Gates {
    fn of(v: &HostView) -> Self {
        let inv = v.inventory.as_ref().map(|(_, i)| i);
        Gates {
            online: v.online,
            stopped: !v.control.mode.running(),
            level: inv.map(Inventory::level),
            offered: inv
                .map(|i| i.manage.host_actions.clone())
                .unwrap_or_default(),
            shell: inv.is_some_and(|i| i.manage.manage_shell),
            project_settings: inv.is_some_and(|i| i.gates.web_project_settings),
        }
    }

    /// Hidden when the host does not offer it; disabled offline, when the
    /// host gate is too low, or for a change while the factory is stopped.
    pub fn state(&self, kind: &str) -> GateState {
        let Some(class) = hosts::class_of(kind) else {
            return GateState::Hidden;
        };
        if !self.offered.iter().any(|k| k == kind) {
            return GateState::Hidden;
        }
        if !self.online {
            return GateState::Disabled("Offline".into());
        }
        let have = self.level.unwrap_or(Level::Off);
        let need = match class {
            Class::Read => Level::Read,
            Class::Config => Level::Config,
            Class::Provision | Class::Shell => Level::Provision,
        };
        if have < need {
            return GateState::Disabled(format!("Host gate: {}", have.as_str()));
        }
        if matches!(kind, "agents_set" | "project_prefs_set") && !self.project_settings {
            return GateState::Disabled("Host: --web-project-settings off".into());
        }
        if class == Class::Shell && !self.shell {
            return GateState::Disabled("Host shell off".into());
        }
        if self.stopped && class.mutating() {
            return GateState::Disabled("Factory stopped".into());
        }
        GateState::Enabled
    }
}

// ---- screen ----------------------------------------------------------------------------

/// Results the async actions hand back to the screen.
enum Msg {
    Preview { action_id: String, title: String },
    PlanPreviews(Vec<String>),
}

/// A provision action waiting for its host-side preview and the typed name.
struct Preview {
    action_id: String,
    title: String,
    typed: String,
}

struct ConfigEdit {
    key: String,
    old: String,
    value: String,
    /// A project preference (project_prefs_set) for this repo path; None: global.
    path: Option<String>,
}

/// §3.2 Dispatch, from the header or a repo row (cwd preset).
#[derive(Default)]
struct DispatchForm {
    kind: String,
    cwd: String,
    agent: String,
    prompt: String,
}

#[derive(Default)]
struct CloneForm {
    open: bool,
    forge_host: String,
    owner: String,
    name: String,
    depth: String,
}

#[derive(Default)]
struct SchedForm {
    profile: String,
    dir: String,
    cron: String,
}

struct InstallForm {
    product: String,
    method: String,
    version: String,
    profile: String,
}

/// Uninstall of a sensor whose profile is locked: the host refuses without a
/// one-time code the console makes for that device.
struct UninstallForm {
    product: String,
    code: String,
}

struct PartsForm {
    product: String,
    parts: Vec<(String, String, bool)>,
    purge: bool,
    scope: String,
}

pub struct HostScreen {
    node_id: String,
    tab: Tab,
    item: Option<String>,
    view: Loader<HostView>,
    sessions: Loader<Vec<Session>>,
    lines: Option<(String, Loader<Vec<kls_core::hosts::HostActionLine>>)>,
    inbox: Arc<Mutex<Vec<Msg>>>,
    preview: Option<Preview>,
    edit: Option<ConfigEdit>,
    clone: CloneForm,
    parts: Option<PartsForm>,
    install: Option<InstallForm>,
    uninstall: Option<UninstallForm>,
    repo_filter: String,
    agents_repo: Option<String>,
    log_lines: u32,
    plan_sel: Option<String>,
    plan_previews: Vec<String>,
    plan_typed: String,
    new_step_kind: String,
    new_step_params: String,
    reason: String,
    shell_argv: String,
    sched_form: SchedForm,
    /// Cron edits in progress, by schedule id.
    sched_cron: std::collections::HashMap<String, String>,
    /// A hook or MCP install armed by its first click.
    lib_armed: Option<String>,
    /// A hook install waiting on its files' confirmation (P5 item 1).
    lib_files: Option<crate::screens::library::FilesConfirm>,
    dispatch: Option<DispatchForm>,
    rename: Option<String>,
    revoke_armed: bool,
    trust_dir: String,
    /// Sessions tab filtered to a repo (Repos row menu).
    session_cwd: Option<String>,
    /// Config tab scope: None is global, Some(path) a project.
    config_scope: Option<String>,
    /// The plan-run step whose log is shown (HM-9).
    run_lines: Option<(String, Loader<Vec<kls_core::hosts::HostActionLine>>)>,
}

const RELOAD: [&str; 5] = ["host.", "node.", "factory.", "a2a.", "audit.appended"];

impl HostScreen {
    fn new(model: &Model, t: Target) -> Self {
        let node = t.node.clone();
        let view = Loader::new(model, &RELOAD, move |core| {
            let n = node.clone();
            async move { hosts::view(&core, &n).await }
        });
        let node = t.node.clone();
        let sessions = Loader::new(model, &["session."], move |core| {
            let n = node.clone();
            async move {
                core.store()
                    .sessions(&SessionQuery {
                        node_id: Some(n),
                        limit: 200,
                        ..Default::default()
                    })
                    .await
            }
        });
        let mut s = HostScreen {
            node_id: t.node,
            tab: Tab::Overview,
            item: None,
            view,
            sessions,
            lines: None,
            inbox: Arc::default(),
            preview: None,
            edit: None,
            clone: CloneForm::default(),
            parts: None,
            install: None,
            uninstall: None,
            repo_filter: String::new(),
            agents_repo: None,
            log_lines: 200,
            plan_sel: None,
            plan_previews: Vec::new(),
            plan_typed: String::new(),
            new_step_kind: "product_probe".into(),
            new_step_params: String::new(),
            reason: String::new(),
            shell_argv: String::new(),
            sched_form: SchedForm::default(),
            sched_cron: Default::default(),
            lib_armed: None,
            lib_files: None,
            dispatch: None,
            rename: None,
            revoke_armed: false,
            trust_dir: String::new(),
            session_cwd: None,
            config_scope: None,
            run_lines: None,
        };
        s.focus(t.tab, t.item);
        s
    }

    fn focus(&mut self, tab: Tab, item: Option<String>) {
        self.tab = tab;
        if tab == Tab::Agents
            && let Some(i) = &item
        {
            self.agents_repo = Some(i.clone());
        }
        self.item = item;
    }

    fn origin(&self) -> Origin {
        let mut o = Origin::owner(self.tab.slug());
        o.reason = self.reason.trim().to_string();
        o
    }

    /// Requests an action now (read and config kinds).
    fn request(&self, model: &Model, kind: &str, params: &[(&str, &str)]) {
        let mut req = ActionRequest::new(&self.node_id, kind, self.origin());
        for (k, v) in params {
            req = req.param(k, v);
        }
        model.act(move |m| async move {
            hosts::request_action(m.core(), req).await?;
            Ok(None)
        });
    }

    /// Asks the host for a dry run, then opens the confirm dialog on it.
    fn preview(&self, model: &Model, kind: &str, params: &[(&str, &str)], title: &str) {
        let mut req = ActionRequest::new(&self.node_id, kind, self.origin()).dry();
        for (k, v) in params {
            req = req.param(k, v);
        }
        let inbox = self.inbox.clone();
        let title = title.to_string();
        model.act(move |m| async move {
            let row = hosts::request_action(m.core(), req).await?;
            inbox.lock().unwrap().push(Msg::Preview {
                action_id: row.id,
                title,
            });
            m.egui_ctx().request_repaint();
            Ok(None)
        });
    }

    /// A gated button. Returns true when clicked.
    fn gated(&self, ui: &mut egui::Ui, g: &Gates, kind: &str, icon: &str, text: &str) -> bool {
        match g.state(kind) {
            GateState::Hidden => false,
            GateState::Disabled(why) => {
                let lock = if why.starts_with("Host gate") {
                    icons::LOCK_SIMPLE
                } else {
                    icon
                };
                ui.add_enabled(false, egui::Button::new(icons::label(lock, text)))
                    .on_disabled_hover_text(why);
                false
            }
            GateState::Enabled => kit::ghost_button(ui, &icons::label(icon, text)).clicked(),
        }
    }

    /// Draws the view. Returns false when the person went back to Fleet.
    fn ui(&mut self, ui: &mut egui::Ui, model: &Model, snap: &Snapshot) -> bool {
        for m in std::mem::take(&mut *self.inbox.lock().unwrap()) {
            match m {
                Msg::Preview { action_id, title } => {
                    self.preview = Some(Preview {
                        action_id,
                        title,
                        typed: String::new(),
                    })
                }
                Msg::PlanPreviews(ids) => self.plan_previews = ids,
            }
        }
        let p = palette(ui.ctx());
        let view = match self.view.get() {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return self.back(ui);
            }
            None => {
                ui.spinner();
                return true;
            }
        };
        let Some(node) = view.node.clone() else {
            kit::empty(ui, icons::HARD_DRIVES, "Unknown node");
            return self.back(ui);
        };
        let g = Gates::of(&view);
        let mut keep = true;
        self.header(ui, model, snap, &view, &node, &g, &mut keep);
        self.banners(ui, &view);
        ui.horizontal_wrapped(|ui| {
            for t in Tab::ALL {
                if kit::tab(ui, &icons::label(t.icon(), t.title()), self.tab == t).clicked() {
                    self.tab = t;
                }
            }
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.tab {
                Tab::Overview => self.overview(ui, model, &view, &g),
                Tab::Products => self.products(ui, model, &view, &g),
                Tab::Repos => self.repos(ui, model, &view, &g),
                Tab::Agents => self.agents(ui, model, &view, &g),
                Tab::Schedules => self.schedules(ui, model, &view, &g),
                Tab::Library => self.library(ui, model, &view, &node, &g),
                Tab::Config => self.config(ui, model, &view, &g),
                Tab::Sessions => self.sessions_tab(ui, model, &view, &g),
                Tab::Provision => self.provision(ui, model, &view, &g, &node.name),
                Tab::Diagnostics => self.diagnostics(ui, model, &view, &g, &node),
                Tab::Audit => self.audit(ui, model, &view),
            });
        self.preview_dialog(ui.ctx(), model, &view, &node.name);
        self.edit_dialog(ui.ctx(), model, &view);
        self.parts_dialog(ui.ctx(), model);
        self.install_dialog(ui.ctx(), model);
        self.uninstall_dialog(ui.ctx(), model);
        self.clone_dialog(ui.ctx(), model);
        self.dispatch_dialog(ui.ctx(), model, &view, &node);
        self.rename_dialog(ui.ctx(), model, &node);
        if self.revoke_dialog(ui.ctx(), model, &node) {
            keep = false;
        }
        keep
    }

    /// §3.2 Dispatch: a session or worker on this node, through the gateway
    /// (kill switch, node checks and the dispatch row's provenance there).
    fn dispatch_dialog(
        &mut self,
        ctx: &egui::Context,
        model: &Model,
        view: &HostView,
        node: &kls_store::Node,
    ) {
        let Some(mut f) = self.dispatch.take() else {
            return;
        };
        let mut open = true;
        let mut send = false;
        let dirs: Vec<String> = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.repos.iter().map(|r| r.path.clone()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .chain(node.host.trusted_dirs.iter().cloned())
            .collect();
        egui::Window::new(icons::label(
            icons::PLAY,
            &format!("Dispatch · {}", node.name),
        ))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            kit::field(ui, "Kind", |ui| {
                for k in ["session", "worker"] {
                    ui.selectable_value(&mut f.kind, k.to_string(), k);
                }
            });
            kit::field(ui, "Dir", |ui| {
                egui::ComboBox::from_id_salt("host-dispatch-dir")
                    .selected_text(icons::label(icons::FOLDER_SIMPLE, or_dash(&f.cwd)))
                    .show_ui(ui, |ui| {
                        for d in &dirs {
                            ui.selectable_value(&mut f.cwd, d.clone(), d);
                        }
                    });
            });
            kit::field(ui, "Agent", |ui| {
                egui::ComboBox::from_id_salt("host-dispatch-agent")
                    .selected_text(if f.agent.is_empty() {
                        "default"
                    } else {
                        f.agent.as_str()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.agent, String::new(), "default");
                        for a in &node.host.agents {
                            ui.selectable_value(&mut f.agent, a.clone(), a);
                        }
                    });
            });
            kit::field(ui, "Prompt", |ui| {
                ui.add(egui::TextEdit::multiline(&mut f.prompt).desired_rows(3))
            });
            let ready = view.online
                && view.control.mode.running()
                && !f.cwd.trim().is_empty()
                && (f.kind == "session" || !f.agent.is_empty());
            let why = if !view.online {
                "Offline"
            } else if !view.control.mode.running() {
                "Factory stopped"
            } else if f.kind == "worker" && f.agent.is_empty() {
                "Pick an agent"
            } else {
                "Pick a dir"
            };
            if ui
                .add_enabled(ready, egui::Button::new(icons::label(icons::PLAY, "Start")))
                .on_disabled_hover_text(why)
                .clicked()
            {
                send = true;
            }
        });
        if send {
            let spec = kls_core::gateway::DispatchSpec {
                kind: f.kind.clone(),
                cwd: f.cwd.trim().to_string(),
                agent: f.agent.trim().to_string(),
                prompt: f.prompt.trim().to_string(),
                trigger: Some(kls_core::gateway::Trigger {
                    source: "console".into(),
                    subject: self.origin().surface,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let node_id = self.node_id.clone();
            model.act(move |m| async move {
                let id = m.core().gateway()?.dispatch(&node_id, spec).await?;
                Ok(Some(Notice::Info(format!("Dispatched {}", short(&id)))))
            });
            return;
        }
        if open {
            self.dispatch = Some(f);
        }
    }

    fn rename_dialog(&mut self, ctx: &egui::Context, model: &Model, node: &kls_store::Node) {
        let Some(mut name) = self.rename.take() else {
            return;
        };
        let mut keep = true;
        egui::Window::new(icons::label(icons::PENCIL_SIMPLE, "Rename"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(kit::mono(&node.name, 12.0, palette(ui.ctx()).muted));
                ui.add(egui::TextEdit::singleline(&mut name).char_limit(hosts::NODE_NAME_MAX));
                ui.horizontal(|ui| {
                    let ok = !name.trim().is_empty() && name.trim() != node.name;
                    if ui
                        .add_enabled(ok, egui::Button::new(icons::label(icons::CHECK, "Save")))
                        .clicked()
                    {
                        let (id, n, o) =
                            (self.node_id.clone(), name.trim().to_string(), self.origin());
                        model.act(move |m| async move {
                            hosts::rename_node(m.core(), &id, &n, o).await?;
                            Ok(None)
                        });
                        keep = false;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.rename = Some(name);
        }
    }

    /// Returns true when the node was revoked (the view goes back to Fleet).
    fn revoke_dialog(
        &mut self,
        ctx: &egui::Context,
        model: &Model,
        node: &kls_store::Node,
    ) -> bool {
        if !self.revoke_armed {
            return false;
        }
        let mut gone = false;
        egui::Window::new(icons::label(icons::PROHIBIT, "Revoke"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let p = palette(ui.ctx());
                ui.colored_label(
                    p.amber,
                    icons::label(icons::WARNING, &format!("Revoke {}?", node.name)),
                )
                .on_hover_text("Certificate stops at once; rejoin needs a new token");
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::PROHIBIT, "Revoke")).clicked() {
                        let (id, o) = (self.node_id.clone(), self.origin());
                        let why = if o.reason.is_empty() {
                            "revoked from the host view".to_string()
                        } else {
                            format!("revoked from the host view: {}", o.reason)
                        };
                        model.act(move |m| async move {
                            let changed = m.hub.revoke(&id, &why).await?;
                            hosts::record_revoke(m.core(), &id, changed, o).await?;
                            Ok(Some(Notice::Info("Node revoked".into())))
                        });
                        self.revoke_armed = false;
                        gone = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        self.revoke_armed = false;
                    }
                });
            });
        gone
    }

    fn back(&mut self, ui: &mut egui::Ui) -> bool {
        !kit::ghost_button(ui, &icons::label(icons::ARROW_LEFT, "Fleet")).clicked()
    }

    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        snap: &Snapshot,
        view: &HostView,
        node: &kls_store::Node,
        g: &Gates,
        keep: &mut bool,
    ) {
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        if kit::ghost_button(ui, &icons::label(icons::ARROW_LEFT, "Fleet")).clicked() {
            *keep = false;
        }
        let inv = view.inventory.as_ref().map(|(_, i)| i);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(icons::HARD_DRIVES)
                    .size(24.0)
                    .color(p.gold_text),
            );
            ui.label(RichText::new(&node.name).size(24.0).strong())
                .on_hover_text(&node.id);
            let (txt, st) = if node.revoked_at.is_some() {
                ("Revoked", "revoked")
            } else if snap.online.contains(&node.id) || view.online {
                ("Online", "online")
            } else {
                ("Offline", "offline")
            };
            let c = kit::status_color(&p, st);
            kit::dot(ui, c);
            ui.label(RichText::new(txt).color(c));
            ui.label(kit::mono(
                &format!("{}/{}", node.host.os, node.host.arch),
                12.0,
                p.muted,
            ));
            let ver = inv
                .map(|i| i.belai.version.clone())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| node.host.belai_version.clone());
            ui.label(kit::mono(&format!("belai {ver}"), 12.0, p.text));
            if let Some(i) = inv.filter(|i| i.belai.update_available) {
                kit::chip(ui, &format!("↑ {}", i.belai.latest), p.gold_text);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let level = g.level.map(Level::as_str).unwrap_or("—");
            kit::chip(ui, &format!("gate: {level}"), p.cyan)
                .on_hover_text("belai rc --keleustes-manage");
            let shell = if g.shell { "shell on" } else { "shell off" };
            kit::chip(ui, &icons::label(icons::LOCK_SIMPLE, shell), p.muted);
            let c = &node.host.capabilities;
            for (on, t) in [
                (c.remote_control, "rc"),
                (c.gh_authenticated, "gh"),
                (c.terminal, "terminal"),
            ] {
                kit::tag(ui, &format!("{t} {}", if on { "✓" } else { "✗" }));
            }
            if let Some((at, _)) = &view.inventory {
                ui.label(RichText::new(fmt::ago(now, *at)).color(p.muted))
                    .on_hover_text(fmt::when(*at));
            } else {
                ui.label(RichText::new("no inventory").color(p.muted))
                    .on_hover_text("Host update needed");
            }
            if self.gated(
                ui,
                g,
                "inventory_refresh",
                icons::ARROW_CLOCKWISE,
                "Refresh",
            ) {
                self.request(model, "inventory_refresh", &[]);
            }
            let live = node.revoked_at.is_none();
            let can_dispatch = live && node.host.capabilities.remote_control;
            if ui
                .add_enabled(
                    can_dispatch && g.online && !g.stopped,
                    egui::Button::new(icons::label(icons::PLAY, "Dispatch")),
                )
                .on_disabled_hover_text(if !can_dispatch {
                    "No rc on host"
                } else if !g.online {
                    "Offline"
                } else {
                    "Factory stopped"
                })
                .clicked()
            {
                self.dispatch = Some(DispatchForm {
                    kind: "session".into(),
                    ..Default::default()
                });
            }
            if ui
                .add_enabled(
                    live && !g.stopped,
                    egui::Button::new(icons::label(icons::PENCIL_SIMPLE, "Rename")),
                )
                .on_disabled_hover_text("Factory stopped")
                .clicked()
            {
                self.rename = Some(node.name.clone());
            }
            if live && kit::danger_button(ui, &icons::label(icons::PROHIBIT, "Revoke")).clicked() {
                self.revoke_armed = true;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.reason)
                    .hint_text("reason (audit)")
                    .desired_width(180.0),
            );
        });
        if !view.online {
            ui.colored_label(
                p.amber,
                icons::label(icons::WARNING, "Offline: last inventory"),
            );
        }
        ui.add_space(6.0);
    }

    fn banners(&self, ui: &mut egui::Ui, view: &HostView) {
        let p = palette(ui.ctx());
        if !view.control.mode.running() {
            ui.colored_label(
                p.red,
                icons::label(icons::POWER, "Factory stopped: changes blocked"),
            );
        }
        if view.control.a2a_blackout {
            ui.colored_label(p.amber, icons::label(icons::BROADCAST, "A2A blackout"));
        }
    }

    // ---- overview ---------------------------------------------------------------------

    fn overview(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        ui.columns(3, |cols| {
            kit::section(
                &mut cols[0],
                icons::PULSE,
                "Health",
                false,
                |_| {},
                |ui| {
                    let ok = inv.preflight.iter().filter(|c| c.status == "ok").count();
                    let rtt = hosts::link_rtt_ms(&view.actions)
                        .map(|ms| format!("  ≤{ms}ms"))
                        .unwrap_or_default();
                    status_line(ui, view.online, &format!("link{rtt}"));
                    if let Some(left) = inv.cert_left_ms(kls_store::now_ms()) {
                        status_line(ui, left > CERT_WARN_MS, &format!("cert {}", days(left)));
                    }
                    status_line(
                        ui,
                        ok == inv.preflight.len(),
                        &format!("preflight {ok}/{}", inv.preflight.len()),
                    );
                    ui.horizontal_wrapped(|ui| {
                        for t in &inv.tools {
                            let c = if t.ok { p.green } else { p.muted };
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    if t.ok { "✓" } else { "—" },
                                    t.name
                                ))
                                .color(c),
                            );
                        }
                    });
                },
            );
            kit::section(
                &mut cols[1],
                icons::PACKAGE,
                "Products",
                false,
                |_| {},
                |ui| {
                    for id in hosts::PRODUCTS {
                        let st = inv.product(id).cloned().unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(icons::label(product_icon(id), product_name(id)));
                            let state = if st.state.is_empty() {
                                "—"
                            } else {
                                &st.state
                            };
                            ui.label(RichText::new(state).color(state_color(&p, state)));
                            ui.label(kit::mono(&st.version, 12.0, p.muted));
                        });
                    }
                },
            );
            kit::section(
                &mut cols[2],
                icons::CPU,
                "Load",
                false,
                |_| {},
                |ui| {
                    let l = &inv.limits;
                    let pair = |a: u32, b: u32| {
                        if b == 0 {
                            "—".to_string()
                        } else {
                            format!("{a} / {b}")
                        }
                    };
                    // Live counts come from WP-B HostStatus when the host sends it.
                    let (sess, work) = match &view.status {
                        Some((_, s)) => (
                            s.running_sessions,
                            s.workers
                                .iter()
                                .filter(|w| !matches!(w.state.as_str(), "stopped" | "failed"))
                                .count() as u32,
                        ),
                        None => (l.sessions, l.workers),
                    };
                    ui.label(format!("sessions {}", pair(sess, l.max_sessions)));
                    ui.label(format!("workers  {}", pair(work, l.max_workers)));
                    ui.label(format!("jobs     {}", pair(l.jobs, l.max_jobs)));
                    ui.label(format!("disk sessions {}", bytes(inv.disk.sessions)));
                },
            );
        });
        if !view.plan_limits.is_empty() {
            // Row 48: routing skips this host while a window is nearly used.
            ui.add_space(10.0);
            kit::section(
                ui,
                icons::GAUGE,
                "Plan limits",
                false,
                |_| {},
                |ui| crate::fleet::plan_limits_ui(ui, &view.plan_limits),
            );
        }
        ui.add_space(10.0);
        let waiting: Vec<HostActionRow> = view
            .actions
            .iter()
            .filter(|a| a.state == "needs_human")
            .cloned()
            .collect();
        kit::section(
            ui,
            icons::HAND_PALM,
            "Needs you",
            !waiting.is_empty(),
            |_| {},
            |ui| {
                if waiting.is_empty() && !inv.belai.update_available {
                    kit::empty(ui, icons::CHECK, "Nothing");
                }
                for a in &waiting {
                    let h = a.handoff().unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(icons::label(
                            icons::HAND_PALM,
                            &format!("{} {}", a.kind.replace('_', " "), param(a, "product")),
                        ));
                        kit::tag(ui, &h.reason);
                        ui.label(kit::mono(&h.command, 12.0, p.text));
                        if kit::ghost_button(ui, &icons::label(icons::COPY, "Copy command"))
                            .clicked()
                        {
                            ui.ctx().copy_text(h.command.clone());
                        }
                        let checking = !h.check_action.is_empty();
                        if ui
                            .add_enabled(
                                !checking && g.online,
                                egui::Button::new(icons::label(icons::CHECK, "Done")),
                            )
                            .on_disabled_hover_text(if checking { "Checking" } else { "Offline" })
                            .clicked()
                        {
                            let (id, o) = (a.id.clone(), self.origin());
                            model.act(move |m| async move {
                                hosts::handoff_done(m.core(), &id, o).await?;
                                Ok(None)
                            });
                        }
                    });
                }
                if inv.belai.update_available {
                    ui.horizontal(|ui| {
                        ui.label(icons::label(
                            icons::WARNING,
                            &format!("belai {} available", inv.belai.latest),
                        ));
                        if kit::ghost_button(ui, &icons::label(icons::ARROW_RIGHT, "Provision"))
                            .clicked()
                        {
                            self.tab = Tab::Provision;
                        }
                    });
                }
            },
        );
    }

    // ---- products -------------------------------------------------------------------------

    fn products(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        let now = kls_store::now_ms();
        for id in hosts::PRODUCTS {
            let st = inv
                .product(id)
                .cloned()
                .unwrap_or_else(|| hosts::ProductStatus {
                    product: id.into(),
                    ..Default::default()
                });
            let open = self.item.as_deref() == Some(id);
            kit::section(
                ui,
                product_icon(id),
                product_name(id),
                open,
                |ui| {
                    if kit::ghost_button(ui, &icons::label(icons::ARROW_SQUARE_OUT, "Console"))
                        .on_hover_text(console_url(id))
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(console_url(id)));
                    }
                },
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        let state = if st.state.is_empty() {
                            "—"
                        } else {
                            st.state.as_str()
                        };
                        kit::dot(ui, state_color(&p, state));
                        ui.label(RichText::new(state).color(state_color(&p, state)));
                        kit::field(ui, "Version", |ui| {
                            ui.label(kit::mono(or_dash(&st.version), 12.0, p.text))
                        });
                        kit::field(ui, "Latest", |ui| {
                            ui.label(kit::mono(or_dash(&st.latest), 12.0, p.muted))
                        });
                        kit::field(ui, "Auth / mode", |ui| ui.label(auth_mode(&st)));
                        if !st.entitlement.is_empty() {
                            kit::field(ui, "Add-on", |ui| ui.label(&st.entitlement));
                        }
                        if !st.parts.is_empty() {
                            kit::field(ui, product_parts_label(id), |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for t in &st.parts {
                                        kit::tag(
                                            ui,
                                            &format!("{} {}", t.id, if t.on { "✓" } else { "✗" }),
                                        );
                                    }
                                });
                            });
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let unsupported = st.state == "unsupported";
                        if self.gated(ui, g, "product_probe", icons::ARROW_CLOCKWISE, "Probe") {
                            self.request(model, "product_probe", &[("product", id)]);
                        }
                        if unsupported {
                            return;
                        }
                        let absent = st.state.is_empty() || st.state == "absent";
                        let installable = matches!(id, "cli" | "sensor");
                        if installable
                            && absent
                            && self.gated(
                                ui,
                                g,
                                "product_install",
                                icons::DOWNLOAD_SIMPLE,
                                "Install",
                            )
                        {
                            self.install = Some(InstallForm {
                                product: id.into(),
                                method: if st.install_method.is_empty() {
                                    "brew".into()
                                } else {
                                    st.install_method.clone()
                                },
                                version: st.latest.clone(),
                                profile: String::new(),
                            });
                        }
                        if installable
                            && !absent
                            && self.gated(ui, g, "product_upgrade", icons::ARROW_UP, "Upgrade")
                        {
                            self.preview(
                                model,
                                "product_upgrade",
                                &[("product", id)],
                                &format!("Upgrade {}", product_name(id)),
                            );
                        }
                        if id == "cli"
                            && !absent
                            && !st.authenticated
                            && self.gated(ui, g, "product_auth", icons::KEY, "Sign in")
                        {
                            self.preview(
                                model,
                                "product_auth",
                                &[("product", "cli")],
                                "CLI sign in",
                            );
                        }
                        if matches!(id, "package_firewall" | "ai_firewall") {
                            let label = if id == "ai_firewall" {
                                "Clients"
                            } else {
                                "Ecosystems"
                            };
                            if self.gated(ui, g, "product_configure", icons::SLIDERS, label) {
                                self.parts = Some(PartsForm {
                                    product: id.into(),
                                    parts: st
                                        .parts
                                        .iter()
                                        .map(|t| (t.id.clone(), t.label.clone(), t.on))
                                        .collect(),
                                    purge: false,
                                    scope: "user".into(),
                                });
                            }
                        }
                        if !absent
                            && self.gated(ui, g, "product_uninstall", icons::TRASH, "Uninstall")
                        {
                            if locked(&st) {
                                self.uninstall = Some(UninstallForm {
                                    product: id.into(),
                                    code: String::new(),
                                });
                            } else {
                                self.preview(
                                    model,
                                    "product_uninstall",
                                    &[("product", id)],
                                    &format!("Uninstall {}", product_name(id)),
                                );
                            }
                        }
                        if kit::ghost_button(ui, &icons::label(icons::COPY, "Copy diagnostics"))
                            .clicked()
                        {
                            ui.ctx()
                                .copy_text(serde_json::to_string_pretty(&st).unwrap_or_default());
                        }
                    });
                    egui::CollapsingHeader::new(RichText::new("Checks").color(p.muted))
                        .id_salt(("checks", id))
                        .default_open(open)
                        .show(ui, |ui| {
                            for c in &st.checks {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&c.status).color(state_color(&p, &c.status)),
                                    );
                                    ui.label(kit::mono(&c.id, 12.0, p.text));
                                    ui.label(RichText::new(&c.hint).color(p.muted));
                                });
                            }
                            if !st.path.is_empty() {
                                ui.label(kit::mono(&st.path, 12.0, p.muted));
                            }
                            for u in &st.units {
                                ui.label(kit::mono(u, 12.0, p.muted));
                            }
                            for d in &st.degraded {
                                ui.colored_label(p.amber, icons::label(icons::WARNING, d));
                            }
                            if st.probed_at_ms > 0 {
                                ui.label(
                                    RichText::new(format!(
                                        "probed {}",
                                        fmt::ago(now, st.probed_at_ms)
                                    ))
                                    .color(p.muted),
                                );
                            }
                        });
                },
            );
            ui.add_space(8.0);
        }
    }

    fn install_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(mut f) = self.install.take() else {
            return;
        };
        let mut keep = true;
        let title = format!("Install {}", product_name(&f.product));
        egui::Window::new(icons::label(icons::DOWNLOAD_SIMPLE, &title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                if f.product == "cli" {
                    ui.horizontal(|ui| {
                        for m in ["brew", "scoop", "direct"] {
                            ui.radio_value(&mut f.method, m.to_string(), m);
                        }
                    });
                }
                kit::field(ui, "Version", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.version).hint_text("latest"))
                });
                if f.product == "sensor" {
                    kit::field(ui, "Profile", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut f.profile).hint_text("profile id"))
                    });
                }
                ui.horizontal(|ui| {
                    if primary_button(ui, &icons::label(icons::EYE, "Preview")).clicked() {
                        let mut params = vec![("product", f.product.as_str())];
                        if f.product == "cli" {
                            params.push(("method", f.method.as_str()));
                        }
                        if !f.version.trim().is_empty() {
                            params.push(("version", f.version.trim()));
                        }
                        if !f.profile.trim().is_empty() {
                            params.push(("profile", f.profile.trim()));
                        }
                        self.preview(model, "product_install", &params, &title);
                        keep = false;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.install = Some(f);
        }
    }

    fn uninstall_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(mut f) = self.uninstall.take() else {
            return;
        };
        let mut keep = true;
        let title = format!("Uninstall {}", product_name(&f.product));
        egui::Window::new(icons::label(icons::TRASH, &title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(icons::label(icons::LOCK_SIMPLE, "Locked profile"));
                kit::field(ui, "Code", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.code).hint_text("XXXX-XXXX-XXXX"))
                });
                ui.horizontal(|ui| {
                    let code = f.code.trim();
                    if ui
                        .add_enabled(
                            !code.is_empty(),
                            egui::Button::new(icons::label(icons::EYE, "Preview")),
                        )
                        .clicked()
                    {
                        self.preview(
                            model,
                            "product_uninstall",
                            &[("product", f.product.as_str()), ("code", code)],
                            &title,
                        );
                        keep = false;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.uninstall = Some(f);
        }
    }

    fn parts_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(mut f) = self.parts.take() else {
            return;
        };
        let mut keep = true;
        let title = if f.product == "ai_firewall" {
            "AI firewall clients"
        } else {
            "Package firewall ecosystems"
        };
        egui::Window::new(icons::label(icons::SLIDERS, title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                for (id, label, on) in &mut f.parts {
                    let text = if label.is_empty() {
                        id.clone()
                    } else {
                        label.clone()
                    };
                    ui.checkbox(on, text);
                }
                if f.product == "ai_firewall" {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut f.scope, "user".into(), "user");
                        ui.radio_value(&mut f.scope, "project".into(), "project");
                    });
                } else {
                    ui.checkbox(&mut f.purge, "purge");
                }
                ui.horizontal(|ui| {
                    if primary_button(ui, &icons::label(icons::EYE, "Preview")).clicked() {
                        let on: Vec<&str> = f
                            .parts
                            .iter()
                            .filter(|(_, _, on)| *on)
                            .map(|(id, _, _)| id.as_str())
                            .collect();
                        let on = on.join(",");
                        let purge = if f.purge { "true" } else { "false" };
                        let mut params = vec![("product", f.product.as_str()), ("on", on.as_str())];
                        if f.product == "ai_firewall" {
                            params.push(("scope", f.scope.as_str()));
                        } else {
                            params.push(("purge", purge));
                        }
                        self.preview(model, "product_configure", &params, title);
                        keep = false;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.parts = Some(f);
        }
    }

    /// The confirm dialog of a provision action: the host's preview, then the
    /// host name typed to confirm. Run stays disabled until the preview is in.
    fn preview_dialog(&mut self, ctx: &egui::Context, model: &Model, view: &HostView, name: &str) {
        let Some(mut pv) = self.preview.take() else {
            return;
        };
        let row = view.actions.iter().find(|a| a.id == pv.action_id).cloned();
        let mut keep = true;
        egui::Window::new(icons::label(icons::EYE, &pv.title))
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| {
                let p = palette(ui.ctx());
                match &row {
                    None => {
                        ui.spinner();
                    }
                    Some(r) if !r.settled() => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new(&r.state).color(p.muted));
                        });
                    }
                    Some(r) if r.state != "done" => {
                        ui.colored_label(
                            p.red,
                            icons::label(icons::X_CIRCLE, &format!("{}: {}", r.state, r.error)),
                        );
                    }
                    Some(r) => {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(kit::mono(
                                        &pretty(&r.preview_json),
                                        12.0,
                                        p.text,
                                    ))
                                    .wrap_mode(egui::TextWrapMode::Wrap)
                                    .selectable(true),
                                );
                            });
                        ui.label(kit::mono(
                            &format!("sha256 {}", short(&r.preview_sha256)),
                            11.0,
                            p.muted,
                        ));
                        let needs_name = r
                            .class_enum()
                            .is_some_and(|c| matches!(c, Class::Provision | Class::Shell));
                        if needs_name {
                            ui.add(
                                egui::TextEdit::singleline(&mut pv.typed)
                                    .hint_text(format!("type {name}"))
                                    .desired_width(240.0),
                            );
                        }
                        let ok = !needs_name || pv.typed.trim() == name;
                        if ui
                            .add_enabled(ok, egui::Button::new(icons::label(icons::CHECK, "Run")))
                            .on_disabled_hover_text("Type the host name")
                            .clicked()
                        {
                            let (id, typed, o) =
                                (r.id.clone(), pv.typed.trim().to_string(), self.origin());
                            model.act(move |m| async move {
                                hosts::confirm(m.core(), &id, &typed, o).await?;
                                Ok(Some(Notice::Info("Queued".into())))
                            });
                            keep = false;
                        }
                    }
                }
                if kit::ghost_button(ui, &icons::label(icons::X, "Close")).clicked() {
                    keep = false;
                }
            });
        if keep {
            self.preview = Some(pv);
        }
    }

    // ---- repos ----------------------------------------------------------------------------

    fn repos(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(icons::MAGNIFYING_GLASS);
            ui.add(
                egui::TextEdit::singleline(&mut self.repo_filter)
                    .hint_text("filter")
                    .desired_width(200.0),
            );
            if g.state("repo_trust") != GateState::Hidden {
                ui.add(
                    egui::TextEdit::singleline(&mut self.trust_dir)
                        .hint_text("~/src/repo")
                        .desired_width(180.0),
                );
                let dir = self.trust_dir.trim().to_string();
                if self.gated(ui, g, "repo_trust", icons::FOLDER_PLUS, "Trust dir")
                    && !dir.is_empty()
                {
                    self.request(model, "repo_trust", &[("path", &dir)]);
                    self.trust_dir.clear();
                }
            }
            if self.gated(ui, g, "repo_clone", icons::CLOUD_ARROW_DOWN, "Clone repo") {
                self.clone.open = true;
            }
        });
        let f = self.repo_filter.to_lowercase();
        egui::Grid::new("host-repos")
            .num_columns(8)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in [
                    "Repo", "Path", "Branch", "State", "Trusted", "Agents", "Scans", "",
                ] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for r in inv.repos.iter().filter(|r| {
                    f.is_empty()
                        || r.path.to_lowercase().contains(&f)
                        || r.name.to_lowercase().contains(&f)
                }) {
                    match r.link() {
                        Some(_) => {
                            if kit::link(ui, &format!("{}/{} ↗", r.owner, r.name), p.cyan)
                                .on_hover_text("Open in Repos")
                                .clicked()
                            {
                                model.go(
                                    crate::Screen::Repos,
                                    Some(format!("{}/{}", r.owner, r.name)),
                                );
                            }
                        }
                        None => {
                            ui.label(RichText::new("(no remote)").color(p.muted));
                        }
                    }
                    ui.label(kit::mono(&r.path, 12.0, p.muted));
                    ui.label(kit::mono(or_dash(&r.branch), 12.0, p.text));
                    let dirty = r.git.as_ref().is_some_and(|g| g.dirty);
                    ui.label(if r.git.is_none() {
                        RichText::new("—").color(p.muted)
                    } else if dirty {
                        RichText::new("! dirty").color(p.amber)
                    } else {
                        RichText::new("✓ clean").color(p.green)
                    });
                    ui.label(if r.trusted { "✓" } else { "✗" });
                    let on = r.agents.iter().filter(|t| t.on).count();
                    if kit::link(ui, &on.to_string(), p.cyan)
                        .on_hover_text("Agents")
                        .clicked()
                    {
                        self.agents_repo = Some(r.path.clone());
                        self.tab = Tab::Agents;
                    }
                    ui.label(r.detections.scans.join(" "));
                    ui.horizontal(|ui| {
                        if r.trusted {
                            if self.gated(ui, g, "repo_untrust", icons::PROHIBIT, "Untrust") {
                                self.request(model, "repo_untrust", &[("path", &r.path)]);
                            }
                        } else if self.gated(ui, g, "repo_trust", icons::SHIELD_CHECK, "Trust") {
                            self.request(model, "repo_trust", &[("path", &r.path)]);
                        }
                        let stale: Vec<String> = inv
                            .worktrees
                            .iter()
                            .filter(|w| w.stale && worktree_of(w, r))
                            .map(|w| w.path.clone())
                            .collect();
                        ui.menu_button(icons::DOTS_THREE, |ui| {
                            if !stale.is_empty()
                                && self.gated(
                                    ui,
                                    g,
                                    "worktree_drop",
                                    icons::TRASH,
                                    &format!("Drop stale worktrees ({})", stale.len()),
                                )
                            {
                                for w in &stale {
                                    self.request(model, "worktree_drop", &[("path", w)]);
                                }
                                ui.close();
                            }
                            if kit::ghost_button(
                                ui,
                                &icons::label(icons::TERMINAL_WINDOW, "Sessions here"),
                            )
                            .clicked()
                            {
                                self.session_cwd = Some(r.path.clone());
                                self.tab = Tab::Sessions;
                                ui.close();
                            }
                            if kit::ghost_button(ui, &icons::label(icons::PLAY, "Dispatch here"))
                                .clicked()
                            {
                                self.dispatch = Some(DispatchForm {
                                    kind: "session".into(),
                                    cwd: r.path.clone(),
                                    ..Default::default()
                                });
                                ui.close();
                            }
                        });
                    });
                    ui.end_row();
                }
            });
        if inv.repos.is_empty() {
            kit::empty(ui, icons::GIT_BRANCH, "No repos");
        }
    }

    fn clone_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        if !self.clone.open {
            return;
        }
        let mut submit = false;
        let mut close = false;
        egui::Window::new(icons::label(icons::CLOUD_ARROW_DOWN, "Clone repo"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                for (cap, v, hint) in [
                    ("Forge", &mut self.clone.forge_host, "github.com"),
                    ("Owner", &mut self.clone.owner, "acme"),
                    ("Name", &mut self.clone.name, "web"),
                    ("Depth", &mut self.clone.depth, "optional"),
                ] {
                    kit::field(ui, cap, |ui| {
                        ui.add(egui::TextEdit::singleline(v).hint_text(hint))
                    });
                }
                ui.horizontal(|ui| {
                    let ok =
                        !self.clone.owner.trim().is_empty() && !self.clone.name.trim().is_empty();
                    if ui
                        .add_enabled(ok, egui::Button::new(icons::label(icons::EYE, "Preview")))
                        .clicked()
                    {
                        submit = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        if submit {
            self.clone.open = false;
            let fh = if self.clone.forge_host.trim().is_empty() {
                "github.com".to_string()
            } else {
                self.clone.forge_host.trim().to_string()
            };
            let (o, n, d) = (
                self.clone.owner.trim().to_string(),
                self.clone.name.trim().to_string(),
                self.clone.depth.trim().to_string(),
            );
            let mut params = vec![
                ("forge_host", fh.as_str()),
                ("owner", o.as_str()),
                ("name", n.as_str()),
            ];
            if !d.is_empty() {
                params.push(("depth", d.as_str()));
            }
            self.preview(model, "repo_clone", &params, &format!("Clone {o}/{n}"));
        }
        if close {
            self.clone.open = false;
        }
    }

    // ---- agents and detections ---------------------------------------------------------

    fn agents(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        if self.agents_repo.is_none() {
            self.agents_repo = inv.repos.first().map(|r| r.path.clone());
        }
        let repo = self
            .agents_repo
            .as_ref()
            .and_then(|path| {
                inv.repos
                    .iter()
                    .find(|r| &r.path == path || format!("{}/{}", r.owner, r.name) == *path)
            })
            .cloned();
        ui.horizontal_wrapped(|ui| {
            ui.label("Repo");
            egui::ComboBox::from_id_salt("host-agents-repo")
                .selected_text(repo.as_ref().map(repo_title).unwrap_or_else(|| "—".into()))
                .show_ui(ui, |ui| {
                    for r in &inv.repos {
                        if ui
                            .selectable_label(
                                repo.as_ref().is_some_and(|x| x.path == r.path),
                                repo_title(r),
                            )
                            .clicked()
                        {
                            self.agents_repo = Some(r.path.clone());
                        }
                    }
                });
            let on = inv.agents_enabled;
            let label = format!("agents.enabled {}", if on { "✓" } else { "✗" });
            if self.gated(ui, g, "config_set", icons::ROBOT, &label) {
                self.edit = Some(ConfigEdit {
                    key: "agents.enabled".into(),
                    old: on.to_string(),
                    value: (!on).to_string(),
                    path: None,
                });
            }
            let mw = inv.limits.max_workers.to_string();
            if self.gated(
                ui,
                g,
                "config_set",
                icons::USERS_THREE,
                &format!("max workers {mw}"),
            ) {
                self.edit = Some(ConfigEdit {
                    key: "agents.max_workers".into(),
                    old: mw.clone(),
                    value: mw,
                    path: None,
                });
            }
            if let Some(r) = &repo
                && r.link().is_some()
                && kit::link(ui, &format!("Findings {}/{} ↗", r.owner, r.name), p.cyan).clicked()
            {
                model.go(
                    crate::Screen::Repos,
                    Some(format!("{}/{}", r.owner, r.name)),
                );
            }
        });
        let Some(repo) = repo else {
            kit::empty(ui, icons::GIT_BRANCH, "No repos");
            return;
        };
        ui.add_space(8.0);
        kit::section(
            ui,
            icons::ROBOT,
            "Built-in agents",
            false,
            |_| {},
            |ui| {
                let mut names = agent_names(&inv.builtin_agents);
                for t in &repo.agents {
                    if !names.contains(&t.id) {
                        names.push(t.id.clone());
                    }
                }
                if names.is_empty() {
                    kit::empty(ui, icons::ROBOT, "None advertised");
                }
                egui::Grid::new("host-agents")
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for name in &names {
                            let t = repo.agents.iter().find(|t| &t.id == name);
                            let state = t
                                .map(|t| t.state.clone())
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "inherit".into());
                            let on = t.map(|t| t.on).unwrap_or(inv.agents_enabled);
                            ui.label(kit::mono(name, 12.0, if on { p.text } else { p.muted }));
                            ui.label(RichText::new(&state).color(p.muted));
                            ui.horizontal(|ui| {
                                for s in ["on", "off", "inherit"] {
                                    if s != state
                                        && self.gated(ui, g, "agents_set", icons::SLIDERS, s)
                                    {
                                        self.request(
                                            model,
                                            "agents_set",
                                            &[
                                                ("path", &repo.path),
                                                ("profile", name),
                                                ("state", s),
                                            ],
                                        );
                                    }
                                }
                            });
                            let can = g.online && !g.stopped && on;
                            if ui
                                .add_enabled(
                                    can,
                                    egui::Button::new(icons::label(icons::PLAY, "Run")),
                                )
                                .on_disabled_hover_text(if !g.online {
                                    "Offline"
                                } else if g.stopped {
                                    "Factory stopped"
                                } else {
                                    "Off for this repo"
                                })
                                .clicked()
                            {
                                let (node, agent, cwd) =
                                    (self.node_id.clone(), name.clone(), repo.path.clone());
                                model.act(move |m| async move {
                                    let spec = kls_core::DispatchSpec {
                                        kind: "worker".into(),
                                        agent,
                                        cwd,
                                        trigger: Some(kls_core::gateway::Trigger {
                                            source: "console".into(),
                                            subject: "fleet.host/agents".into(),
                                            ..Default::default()
                                        }),
                                        ..Default::default()
                                    };
                                    m.core().gateway()?.dispatch(&node, spec).await?;
                                    Ok(Some(Notice::Info("Dispatched".into())))
                                });
                            }
                            ui.end_row();
                        }
                    });
            },
        );
        ui.add_space(8.0);
        kit::section(
            ui,
            icons::TARGET,
            "Detections",
            false,
            |_| {},
            |ui| {
                let d = &repo.detections;
                ui.horizontal_wrapped(|ui| {
                    for (k, v) in [
                        ("build", &d.build),
                        ("lint", &d.lint),
                        ("test", &d.test),
                        ("languages", &d.languages),
                    ] {
                        kit::field(ui, k, |ui| {
                            ui.label(kit::mono(or_dash(&v.join(", ")), 12.0, p.text))
                        });
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Scans");
                    for s in SCANS {
                        let on = d.scans.iter().any(|x| x == s);
                        // One bool pref per scanner (belai ProjectPrefs).
                        let key = format!("vulnetix.scans.{s}");
                        let label = format!("{} {s}", if on { "✓" } else { "✗" });
                        if self.gated(ui, g, "project_prefs_set", icons::SLIDERS, &label) {
                            self.request(
                                model,
                                "project_prefs_set",
                                &[
                                    ("path", &repo.path),
                                    ("key", &key),
                                    ("value", if on { "false" } else { "true" }),
                                ],
                            );
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for (key, on, label) in [
                        ("vulnetix.dep_watch", d.dep_watch, "dep watch"),
                        ("vulnetix.sweep", d.in_sweep, "sweep"),
                    ] {
                        let text = format!("{} {label}", if on { "✓" } else { "✗" });
                        if self.gated(ui, g, "project_prefs_set", icons::SLIDERS, &text) {
                            let v = if on { "false" } else { "true" };
                            self.request(
                                model,
                                "project_prefs_set",
                                &[("path", &repo.path), ("key", key), ("value", v)],
                            );
                        }
                    }
                });
            },
        );
    }

    // ---- schedules --------------------------------------------------------------------------

    /// HM-10: the host's schedules (WP-G) and factory schedules that target
    /// it. Edits go through `hosts::save_schedule` (kill switch, origin,
    /// audit); the host still checks the profile, the directory and the cron.
    fn schedules(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        let node = view.node.clone().unwrap_or_else(|| kls_store::Node {
            id: self.node_id.clone(),
            name: String::new(),
            host: Default::default(),
            cert_serial: String::new(),
            created_at: 0,
            last_seen_at: None,
            revoked_at: None,
            revoked_reason: None,
            node_seq: 0,
            server_acked: 0,
        });
        let can_edit = !g.stopped && node.revoked_at.is_none();
        let why = if g.stopped {
            "Factory stopped"
        } else {
            "Node revoked"
        };
        // New schedule.
        ui.horizontal_wrapped(|ui| {
            let f = &mut self.sched_form;
            egui::ComboBox::from_id_salt("host-sched-profile")
                .selected_text(if f.profile.is_empty() {
                    "Profile"
                } else {
                    f.profile.as_str()
                })
                .show_ui(ui, |ui| {
                    for a in &node.host.agents {
                        ui.selectable_value(&mut f.profile, a.clone(), a);
                    }
                });
            egui::ComboBox::from_id_salt("host-sched-dir")
                .selected_text(if f.dir.is_empty() {
                    "Dir"
                } else {
                    f.dir.as_str()
                })
                .show_ui(ui, |ui| {
                    for d in &node.host.trusted_dirs {
                        ui.selectable_value(&mut f.dir, d.clone(), d);
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut f.cron)
                    .hint_text("0 3 * * *")
                    .desired_width(120.0),
            );
            let ready = can_edit
                && !f.profile.is_empty()
                && !f.dir.is_empty()
                && kls_core::belai::schedules::cron_shape(if f.cron.trim().is_empty() {
                    "@daily"
                } else {
                    f.cron.trim()
                });
            if ui
                .add_enabled(ready, egui::Button::new(icons::label(icons::PLUS, "New")))
                .on_disabled_hover_text(if can_edit { "Profile, dir, cron" } else { why })
                .clicked()
            {
                let s = kls_core::hosts::ScheduleRow {
                    node_id: self.node_id.clone(),
                    profile: f.profile.clone(),
                    cron: if f.cron.trim().is_empty() {
                        "@daily".into()
                    } else {
                        f.cron.trim().into()
                    },
                    cwd: f.dir.clone(),
                    enabled: true,
                    ..Default::default()
                };
                f.cron.clear();
                self.save_schedule(model, s);
            }
        });
        ui.add_space(6.0);
        if view.schedules.is_empty() && view.factory_schedules.is_empty() {
            kit::empty(ui, icons::CALENDAR, "No schedules");
        }
        let mut retime: Option<(kls_core::hosts::ScheduleRow, String)> = None;
        egui::Grid::new("host-schedules")
            .num_columns(8)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in [
                    "Source",
                    "Profile / crew",
                    "Cron",
                    "TZ",
                    "Dir",
                    "Enabled",
                    "Last",
                    "",
                ] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for s in &view.schedules {
                    kit::tag(ui, "host");
                    let target = if s.crew.is_empty() {
                        s.profile.clone()
                    } else {
                        format!("crew:{}", s.crew)
                    };
                    ui.label(kit::mono(&target, 12.0, p.text));
                    let mut cron = self
                        .sched_cron
                        .get(&s.id)
                        .cloned()
                        .unwrap_or_else(|| s.cron.clone());
                    let r = ui.add_enabled(
                        can_edit,
                        egui::TextEdit::singleline(&mut cron).desired_width(110.0),
                    );
                    if r.changed() {
                        self.sched_cron.insert(s.id.clone(), cron.clone());
                    }
                    if r.lost_focus() && cron.trim() != s.cron {
                        retime = Some((s.clone(), cron.trim().to_string()));
                    }
                    ui.label(or_dash(&s.tz));
                    ui.label(kit::mono(&s.cwd, 12.0, p.muted));
                    let mut on = s.enabled;
                    if ui
                        .add_enabled(can_edit, egui::Checkbox::new(&mut on, ""))
                        .on_disabled_hover_text(why)
                        .changed()
                    {
                        let mut e = s.clone();
                        e.enabled = on;
                        self.save_schedule(model, e);
                    }
                    ui.label(if s.last_fired > 0 {
                        RichText::new(format!(
                            "{} {}",
                            if s.last_status == "ok" { "✓" } else { "✗" },
                            fmt::ago(now, s.last_fired)
                        ))
                        .color(state_color(&p, &s.last_status))
                    } else {
                        RichText::new("—").color(p.muted)
                    });
                    ui.horizontal(|ui| {
                        let run_ok = g.online && !g.stopped && s.crew.is_empty();
                        if ui
                            .add_enabled(
                                run_ok,
                                egui::Button::new(icons::label(icons::PLAY, "Run now")),
                            )
                            .on_disabled_hover_text(if !g.online {
                                "Offline"
                            } else if g.stopped {
                                "Factory stopped"
                            } else {
                                "Crew schedules run on the host"
                            })
                            .clicked()
                        {
                            let (s, o) = (s.clone(), self.origin());
                            model.act(move |m| async move {
                                hosts::run_schedule_now(m.core(), &s, o).await?;
                                Ok(Some(Notice::Info("Dispatched".into())))
                            });
                        }
                        if ui
                            .add_enabled(can_edit, egui::Button::new(icons::TRASH))
                            .on_hover_text("Delete")
                            .on_disabled_hover_text(why)
                            .clicked()
                        {
                            let mut e = s.clone();
                            e.deleted = true;
                            self.save_schedule(model, e);
                        }
                    });
                    ui.end_row();
                }
                for s in &view.factory_schedules {
                    kit::tag(ui, "factory");
                    if kit::link(ui, "workflow ↗", p.cyan).clicked() {
                        model.go(crate::Screen::Workflows, Some(s.workflow_id.clone()));
                    }
                    ui.label(kit::mono(&s.cron, 12.0, p.text));
                    ui.label(or_dash(&s.tz));
                    ui.label(RichText::new("—").color(p.muted));
                    ui.label(if s.enabled { "✓" } else { "✗" });
                    ui.label(
                        s.last_fired_at
                            .map(|t| fmt::ago(now, t))
                            .unwrap_or_else(|| "—".into()),
                    );
                    ui.label("");
                    ui.end_row();
                }
            });
        if let Some((mut s, cron)) = retime {
            self.sched_cron.remove(&s.id);
            if kls_core::belai::schedules::cron_shape(&cron) {
                s.cron = cron;
                self.save_schedule(model, s);
            } else {
                model.notify(Notice::Error(
                    "cron: five fields or @hourly, @daily, @weekly, @monthly".into(),
                ));
            }
        }
    }

    fn save_schedule(&self, model: &Model, s: kls_core::hosts::ScheduleRow) {
        let o = self.origin();
        model.act(move |m| async move {
            hosts::save_schedule(m.core(), s, o).await?;
            Ok(None)
        });
    }

    // ---- library ------------------------------------------------------------------------------

    /// HM-11: what the host holds against the library (WP-E reconcile),
    /// install or back up items, and scan + import (WP-K). The same
    /// `hosts::library_dispatch` / `library_import` the Fleet Library row uses.
    fn library(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        view: &HostView,
        node: &kls_store::Node,
        g: &Gates,
    ) {
        let p = palette(ui.ctx());
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        let lib = &view.library;
        let caps = &node.host.capabilities;
        let ok = |kind: &str| -> Result<(), &'static str> {
            if !caps.takes_dispatch(kind) {
                Err("Host update needed")
            } else if !g.online {
                Err("Offline")
            } else if g.stopped {
                Err("Factory stopped")
            } else {
                Ok(())
            }
        };
        let send = |model: &Model,
                    kind: &'static str,
                    items: Vec<kls_core::gateway::LibraryRef>,
                    code: bool,
                    o: Origin| {
            let node = node.id.clone();
            model.act(move |m| async move {
                hosts::library_dispatch(m.core(), &node, kind, items, code, o).await?;
                Ok(Some(Notice::Info("Sent".into())))
            });
        };
        ui.horizontal(|ui| {
            let st = ok("library_scan");
            if ui
                .add_enabled(
                    st.is_ok(),
                    egui::Button::new(icons::label(icons::MAGNIFYING_GLASS, "Scan")),
                )
                .on_hover_text("List what this host's harnesses hold")
                .on_disabled_hover_text(st.err().unwrap_or(""))
                .clicked()
            {
                send(model, "library_scan", Vec::new(), false, self.origin());
            }
        });
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::BOOKS,
            "On the host",
            false,
            |_| {},
            |ui| {
                if lib.held.is_empty() {
                    kit::empty(ui, icons::BOOKS, "Nothing reported");
                }
                egui::Grid::new("host-lib-held")
                    .num_columns(5)
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for h in ["Kind", "Name", "Host hash", "State", ""] {
                            kit::th(ui, h);
                        }
                        ui.end_row();
                        for h in &lib.held {
                            ui.label(RichText::new(&h.item.kind).color(p.muted));
                            ui.label(kit::mono(&h.item.name, 12.0, p.text));
                            ui.label(kit::mono(short(&h.item.sha256), 12.0, p.muted));
                            let (word, c) = match h.verdict.as_str() {
                                "current" => ("✓ current", p.green),
                                "push" => ("host only", p.muted),
                                _ => ("diverged", p.amber),
                            };
                            ui.label(RichText::new(word).color(c));
                            let r = || kls_core::gateway::LibraryRef {
                                kind: h.item.kind.clone(),
                                id: h.item.id.clone(),
                                name: h.item.name.clone(),
                                version: h.item.version,
                                sha256: h.item.sha256.clone(),
                            };
                            ui.horizontal(|ui| {
                                if h.verdict != "current" {
                                    let st = ok("library_backup");
                                    if ui
                                        .add_enabled(
                                            st.is_ok(),
                                            egui::Button::new(icons::label(
                                                icons::UPLOAD_SIMPLE,
                                                "Backup",
                                            )),
                                        )
                                        .on_disabled_hover_text(st.err().unwrap_or(""))
                                        .clicked()
                                    {
                                        send(
                                            model,
                                            "library_backup",
                                            vec![r()],
                                            false,
                                            self.origin(),
                                        );
                                    }
                                }
                                if h.verdict == "diverged"
                                    && !kls_core::belai::library::INSTALL_ONLY
                                        .contains(&h.item.kind.as_str())
                                {
                                    let st = ok("library_install");
                                    if ui
                                        .add_enabled(
                                            st.is_ok(),
                                            egui::Button::new(icons::label(
                                                icons::DOWNLOAD_SIMPLE,
                                                "Update",
                                            )),
                                        )
                                        .on_hover_text("Replace with the library's latest")
                                        .on_disabled_hover_text(st.err().unwrap_or(""))
                                        .clicked()
                                    {
                                        send(
                                            model,
                                            "library_install",
                                            vec![r()],
                                            false,
                                            self.origin(),
                                        );
                                    }
                                }
                            });
                            ui.end_row();
                        }
                        for pl in &inv.plugins {
                            ui.label(RichText::new("plugin").color(p.muted));
                            ui.label(kit::mono(pl, 12.0, p.text));
                            ui.label("—");
                            ui.label(RichText::new("read only").color(p.muted));
                            ui.label("");
                            ui.end_row();
                        }
                    });
            },
        );
        ui.add_space(6.0);
        let held: Vec<(String, String)> = lib
            .held
            .iter()
            .map(|h| (lib_kind(&h.item.kind), h.item.name.clone()))
            .collect();
        let offer: Vec<&kls_store::vault::LibraryItem> = lib
            .items
            .iter()
            .filter(|i| !held.contains(&(lib_kind(&i.kind), i.name.clone())))
            .collect();
        kit::section(
            ui,
            icons::DOWNLOAD_SIMPLE,
            "From the library",
            false,
            |_| {},
            |ui| {
                if offer.is_empty() {
                    kit::empty(ui, icons::CHECK, "All on the host");
                }
                for it in offer {
                    let code = matches!(it.kind.as_str(), "hook" | "mcp");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&it.kind).color(p.muted));
                        ui.label(kit::mono(&it.name, 12.0, p.text));
                        ui.label(RichText::new(format!("v{}", it.latest_version)).color(p.muted));
                        let armed = self.lib_armed.as_deref() == Some(it.id.as_str());
                        let label = if code && !armed {
                            icons::label(icons::WARNING, "Install (runs code)")
                        } else {
                            icons::label(icons::DOWNLOAD_SIMPLE, "Install")
                        };
                        let st = ok("library_install");
                        if ui
                            .add_enabled(st.is_ok(), egui::Button::new(label))
                            .on_disabled_hover_text(st.err().unwrap_or(""))
                            .clicked()
                        {
                            if code && !armed {
                                self.lib_armed = Some(it.id.clone());
                            } else {
                                self.lib_armed = None;
                                if it.kind == "hook" {
                                    // A hook's bundle files get their own window.
                                    self.lib_files =
                                        Some(crate::screens::library::FilesConfirm::open(
                                            model,
                                            &node.id,
                                            it.clone(),
                                        ));
                                } else {
                                    crate::screens::library::send_install(
                                        model,
                                        &node.id,
                                        it,
                                        false,
                                        self.origin(),
                                    );
                                }
                            }
                        }
                    });
                }
            },
        );
        let step = self
            .lib_files
            .as_mut()
            .filter(|f| f.node == node.id)
            .map(|f| f.show(ui.ctx()));
        match step {
            Some(crate::screens::library::FilesStep::Cancel) => self.lib_files = None,
            Some(crate::screens::library::FilesStep::Send { confirm }) => {
                if let Some(fc) = self.lib_files.take() {
                    fc.send(model, confirm, self.origin());
                }
            }
            _ => {}
        }
        if !lib.scan.is_empty() {
            ui.add_space(6.0);
            kit::section(
                ui,
                icons::MAGNIFYING_GLASS,
                "Last scan",
                false,
                |_| {},
                |ui| {
                    for it in lib.scan.iter().filter(|i| i.verdict != "invalid") {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} {}", it.kind, it.name)).strong());
                            ui.label(kit::mono(
                                &format!("{} · {}", it.harness, it.path),
                                12.0,
                                p.muted,
                            ));
                            let st = ok("library_import");
                            if ui
                                .add_enabled(
                                    st.is_ok(),
                                    egui::Button::new(icons::label(
                                        icons::DOWNLOAD_SIMPLE,
                                        "Import",
                                    )),
                                )
                                .on_disabled_hover_text(st.err().unwrap_or(""))
                                .clicked()
                            {
                                let (node, scan, idx, o) = (
                                    node.id.clone(),
                                    it.dispatch_id.clone(),
                                    it.idx,
                                    self.origin(),
                                );
                                model.act(move |m| async move {
                                    hosts::library_import(m.core(), &node, &scan, idx, o).await?;
                                    Ok(Some(Notice::Info("Import sent".into())))
                                });
                            }
                        });
                    }
                },
            );
        }
    }

    // ---- config ------------------------------------------------------------------------------

    fn config(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label("Scope");
            let sel = match &self.config_scope {
                None => "Global".to_string(),
                Some(p) => format!("Project: {}", repo_label(&inv, p)),
            };
            egui::ComboBox::from_id_salt("host-config-scope")
                .selected_text(icons::label(icons::SLIDERS, &sel))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.config_scope, None, "Global");
                    for r in &inv.repos {
                        ui.selectable_value(
                            &mut self.config_scope,
                            Some(r.path.clone()),
                            icons::label(icons::FOLDER_SIMPLE, &repo_title(r)),
                        );
                    }
                });
        });
        match self.config_scope.clone() {
            None => self.config_global(ui, model, view, g, &inv),
            Some(path) => self.config_project(ui, model, view, g, &inv, &path),
        }
    }

    /// HM-5 project scope: the repo's redacted project settings (read only)
    /// and its preferences, each editable through project_prefs_set.
    fn config_project(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        view: &HostView,
        g: &Gates,
        inv: &Inventory,
        path: &str,
    ) {
        let p = palette(ui.ctx());
        let last = latest_config(&view.actions, Some(path));
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::mono(path, 12.0, p.muted));
            if self.gated(ui, g, "config_get", icons::ARROW_CLOCKWISE, "Load") {
                self.request(model, "config_get", &[("scope", "project"), ("path", path)]);
            }
            if let Some(a) = &last
                && kit::ghost_button(ui, &icons::label(icons::CODE, "Raw"))
                    .on_hover_text("Copy redacted JSON")
                    .clicked()
            {
                ui.ctx().copy_text(pretty(&a.result_json));
            }
        });
        let doc: serde_json::Value = last
            .as_ref()
            .and_then(|a| serde_json::from_str(&a.result_json).ok())
            .unwrap_or_default();
        // Prefs: the loaded ones, else what the inventory reported.
        let mut prefs: BTreeMap<String, String> = inv
            .repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.prefs.clone().into_iter().collect())
            .unwrap_or_default();
        if let Some(m) = doc.get("prefs").and_then(|v| v.as_object()) {
            prefs = m
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect();
        }
        let settings = doc.get("settings").map(flatten).unwrap_or_default();
        egui::Grid::new("host-config-project")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["Section", "Key", "Value", "Remote"] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for (k, v) in &prefs {
                    ui.label(RichText::new("Prefs").color(p.muted));
                    ui.label(kit::mono(k, 12.0, p.text));
                    ui.label(kit::mono(or_dash(v), 12.0, p.text));
                    if self.gated(ui, g, "project_prefs_set", icons::PENCIL_SIMPLE, "edit") {
                        self.edit = Some(ConfigEdit {
                            key: k.clone(),
                            old: v.clone(),
                            value: v.clone(),
                            path: Some(path.to_string()),
                        });
                    }
                    ui.end_row();
                }
                for (k, v) in &settings {
                    ui.label(RichText::new("Settings").color(p.muted));
                    ui.label(kit::mono(k, 12.0, p.text));
                    ui.label(kit::mono(or_dash(v), 12.0, p.text));
                    ui.label(icons::label(icons::LOCK_SIMPLE, "repo file"));
                    ui.end_row();
                }
            });
        if prefs.is_empty() && settings.is_empty() {
            kit::empty(ui, icons::SLIDERS, "No project settings");
        }
        if self.gated(ui, g, "project_prefs_set", icons::PLUS, "Pref") {
            self.edit = Some(ConfigEdit {
                key: String::new(),
                old: String::new(),
                value: String::new(),
                path: Some(path.to_string()),
            });
        }
    }

    fn config_global(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        view: &HostView,
        g: &Gates,
        inv: &Inventory,
    ) {
        let p = palette(ui.ctx());
        let last = latest_config(&view.actions, None);
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::mono(
                &format!("sha {}", short(&inv.settings_sha256)),
                12.0,
                p.muted,
            ));
            if self.gated(ui, g, "config_get", icons::ARROW_CLOCKWISE, "Load") {
                self.request(model, "config_get", &[("scope", "global")]);
            }
            if let Some(a) = &last
                && kit::ghost_button(ui, &icons::label(icons::CODE, "Raw"))
                    .on_hover_text("Copy redacted JSON")
                    .clicked()
            {
                ui.ctx().copy_text(pretty(&a.result_json));
            }
        });
        let values: BTreeMap<String, String> = last
            .as_ref()
            .map(|a| config_values(&a.result_json))
            .unwrap_or_default();
        egui::Grid::new("host-config")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["Section", "Key", "Value", "Remote"] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for (section, key, mode) in REMOTE_KEYS {
                    ui.label(RichText::new(*section).color(p.muted));
                    ui.label(kit::mono(key, 12.0, p.text));
                    let v = values.get(*key).cloned().unwrap_or_default();
                    ui.label(kit::mono(or_dash(&v), 12.0, p.text));
                    let label = if *mode == "tighten" {
                        "tighten"
                    } else {
                        "edit"
                    };
                    if self.gated(ui, g, "config_set", icons::PENCIL_SIMPLE, label) {
                        self.edit = Some(ConfigEdit {
                            key: key.to_string(),
                            old: v.clone(),
                            value: v,
                            path: None,
                        });
                    }
                    ui.end_row();
                }
                for name in telemetry_headers(&values) {
                    ui.label(RichText::new("Telemetry").color(p.muted));
                    ui.label(kit::mono(
                        &format!("{TELEMETRY_HEADER}{name}"),
                        12.0,
                        p.text,
                    ));
                    ui.label(icons::label(icons::LOCK_SIMPLE, "present"));
                    if self.gated(ui, g, "config_set", icons::PENCIL_SIMPLE, "edit") {
                        self.edit = Some(ConfigEdit {
                            key: format!("{TELEMETRY_HEADER}{name}"),
                            old: String::new(),
                            value: String::new(),
                            path: None,
                        });
                    }
                    ui.end_row();
                }
                for gate in [
                    "--web-controls",
                    "--web-shell",
                    "--web-project-settings",
                    "--web-allow-guardrails-off",
                    "--no-clone",
                ] {
                    ui.label(RichText::new("rc gates").color(p.muted));
                    ui.label(kit::mono(gate, 12.0, p.text));
                    let on = match gate {
                        "--web-controls" => inv.gates.web_controls,
                        "--web-shell" => inv.gates.web_shell,
                        "--web-project-settings" => inv.gates.web_project_settings,
                        "--web-allow-guardrails-off" => inv.gates.allow_guardrails_off,
                        _ => inv.gates.no_clone,
                    };
                    ui.label(if on { "on" } else { "off" });
                    ui.label(icons::label(icons::LOCK_SIMPLE, "host-only"));
                    ui.end_row();
                }
                for pr in &inv.providers {
                    ui.label(RichText::new("Keys").color(p.muted));
                    ui.label(kit::mono(
                        &format!("providers.{}", pr.provider),
                        12.0,
                        p.text,
                    ));
                    ui.label(if pr.key_present { "✓ present" } else { "✗" });
                    ui.label(icons::label(icons::LOCK_SIMPLE, "vault"));
                    ui.end_row();
                }
            });
        if self.gated(ui, g, "config_set", icons::PLUS, "Telemetry header") {
            self.edit = Some(ConfigEdit {
                key: TELEMETRY_HEADER.into(),
                old: String::new(),
                value: "env:".into(),
                path: None,
            });
        }
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, model: &Model, view: &HostView) {
        let Some(mut e) = self.edit.take() else {
            return;
        };
        let sha = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.settings_sha256.clone())
            .unwrap_or_default();
        let mut keep = true;
        let title = if e.key.is_empty() { "Pref" } else { &e.key };
        egui::Window::new(icons::label(icons::SLIDERS, title))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let p = palette(ui.ctx());
                if let Some(path) = &e.path {
                    ui.label(kit::mono(path, 12.0, p.muted));
                    if e.old.is_empty() {
                        ui.add(egui::TextEdit::singleline(&mut e.key).hint_text("key"));
                    }
                } else if e.key.starts_with(TELEMETRY_HEADER) && e.old.is_empty() {
                    // A new OTLP header: its name, then env:NAME (or empty: remove).
                    ui.add(egui::TextEdit::singleline(&mut e.key).hint_text("header"));
                }
                ui.label(kit::mono(&format!("{} → ", or_dash(&e.old)), 12.0, p.muted));
                ui.add(
                    egui::TextEdit::singleline(&mut e.value).hint_text(if e.path.is_some() {
                        "empty clears"
                    } else {
                        ""
                    }),
                );
                if e.path.is_none() {
                    ui.label(kit::mono(&format!("if sha {}", short(&sha)), 11.0, p.muted));
                }
                ui.horizontal(|ui| {
                    let ok = !e.key.trim().is_empty() && e.key.trim() != TELEMETRY_HEADER;
                    if ui
                        .add_enabled(ok, egui::Button::new(icons::label(icons::CHECK, "Save")))
                        .clicked()
                    {
                        let kind = if e.path.is_some() {
                            "project_prefs_set"
                        } else {
                            "config_set"
                        };
                        let mut req = ActionRequest::new(&self.node_id, kind, self.origin())
                            .param("key", e.key.trim())
                            .param("value", e.value.trim());
                        match &e.path {
                            Some(path) => req = req.param("path", path),
                            None => req.if_match = sha.clone(),
                        }
                        model.act(move |m| async move {
                            hosts::request_action(m.core(), req).await?;
                            Ok(None)
                        });
                        keep = false;
                    }
                    // The factory-wide collector: the same config_set on every host.
                    if e.key == "telemetry.otlp_endpoint"
                        && kit::ghost_button(ui, &icons::label(icons::GLOBE, "All hosts")).clicked()
                    {
                        let value = e.value.trim().to_string();
                        let origin = self.origin();
                        model.act(move |m| async move {
                            let res = hosts::telemetry_everywhere(m.core(), &value, origin).await?;
                            let sent = res.iter().filter(|(_, r)| r.is_ok()).count();
                            Ok(Some(Notice::Info(format!(
                                "Telemetry {sent}/{}",
                                res.len()
                            ))))
                        });
                        keep = false;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.edit = Some(e);
        }
    }

    // ---- sessions ------------------------------------------------------------------------------

    fn sessions_tab(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        self.workforce(ui, model, view, g);
        ui.horizontal(|ui| {
            if self.gated(ui, g, "session_prune", icons::TRASH, "Prune ended") {
                self.request(model, "session_prune", &[]);
            }
        });
        let rows = match self.sessions.get() {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let rows: Vec<&Session> = rows
            .iter()
            .filter(|s| {
                self.session_cwd
                    .as_deref()
                    .is_none_or(|d| cwd_matches(&s.cwd, d))
            })
            .collect();
        if let Some(d) = self.session_cwd.clone() {
            ui.horizontal(|ui| {
                kit::chip(ui, &icons::label(icons::FOLDER_SIMPLE, &d), p.cyan);
                if kit::ghost_button(ui, icons::X)
                    .on_hover_text("All dirs")
                    .clicked()
                {
                    self.session_cwd = None;
                }
            });
        }
        if rows.is_empty() {
            kit::empty(ui, icons::TERMINAL_WINDOW, "No sessions");
        }
        egui::Grid::new("host-sessions")
            .num_columns(5)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["Session", "State", "Kind", "Dir", "Seen"] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for s in rows.iter().take(200) {
                    let title = if s.title.is_empty() {
                        short(&s.id).to_string()
                    } else {
                        s.title.clone()
                    };
                    if kit::link(ui, &title, p.cyan).clicked() {
                        model.go(crate::Screen::Sessions, Some(s.id.clone()));
                    }
                    ui.label(RichText::new(&s.state).color(kit::status_color(&p, &s.state)));
                    ui.label(&s.kind);
                    ui.label(kit::mono(&s.cwd, 12.0, p.muted));
                    ui.label(RichText::new(fmt::ago(now, s.updated_at)).color(p.muted));
                    ui.end_row();
                }
            });
    }

    /// Live workers and crew runs (WP-B), with pause, resume and stop
    /// through the existing dispatch kinds (WP-D), gated by what the host
    /// takes. The gateway checks the kill switch on every dispatch.
    fn workforce(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView, g: &Gates) {
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        let Some((at, st)) = &view.status else {
            return;
        };
        let caps = view
            .node
            .as_ref()
            .map(|n| n.host.capabilities.clone())
            .unwrap_or_default();
        kit::section(
            ui,
            icons::USERS_THREE,
            "Workers",
            false,
            |ui| {
                ui.label(RichText::new(fmt::ago(now, *at)).color(p.muted));
            },
            |ui| {
                if st.workers.is_empty() {
                    kit::empty(ui, icons::USERS_THREE, "No workers");
                }
                egui::Grid::new("host-workers")
                    .num_columns(6)
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for h in ["Profile", "State", "Card", "Dir", "Done / failed", ""] {
                            kit::th(ui, h);
                        }
                        ui.end_row();
                        for w in &st.workers {
                            ui.label(kit::mono(&w.profile, 12.0, p.text));
                            ui.label(
                                RichText::new(&w.state).color(kit::status_color(&p, &w.state)),
                            )
                            .on_hover_text(&w.reason);
                            ui.label(kit::mono(or_dash(&w.card_id), 12.0, p.muted));
                            ui.label(kit::mono(&w.cwd, 12.0, p.muted));
                            ui.label(format!("{} / {}", w.done, w.failed));
                            ui.horizontal(|ui| {
                                let live = !matches!(w.state.as_str(), "stopped" | "failed");
                                let ctl =
                                    |ui: &mut egui::Ui, kind: &str, icon: &str, text: &str| {
                                        let ok =
                                            g.online && !g.stopped && caps.takes_dispatch(kind);
                                        let why = if !g.online {
                                            "Offline"
                                        } else if g.stopped {
                                            "Factory stopped"
                                        } else {
                                            "Host update needed"
                                        };
                                        if ui
                                            .add_enabled(
                                                ok,
                                                egui::Button::new(icons::label(icon, text)),
                                            )
                                            .on_disabled_hover_text(why)
                                            .clicked()
                                        {
                                            let spec = kls_core::DispatchSpec {
                                                kind: kind.into(),
                                                worker_id: w.id.clone(),
                                                session_id: w.session_id.clone(),
                                                trigger: Some(kls_core::gateway::Trigger {
                                                    source: "console".into(),
                                                    subject: "fleet.host/sessions".into(),
                                                    ..Default::default()
                                                }),
                                                ..Default::default()
                                            };
                                            let node = self.node_id.clone();
                                            model.act(move |m| async move {
                                                m.core().gateway()?.dispatch(&node, spec).await?;
                                                Ok(None)
                                            });
                                        }
                                    };
                                if live && w.state == "paused" {
                                    ctl(ui, "resume", icons::PLAY, "Resume");
                                } else if live {
                                    ctl(ui, "pause", icons::PAUSE, "Pause");
                                }
                                if live && !w.session_id.is_empty() {
                                    ctl(ui, "stop", icons::STOP, "Stop");
                                }
                                if !w.session_id.is_empty()
                                    && kit::ghost_button(ui, icons::TERMINAL_WINDOW)
                                        .on_hover_text("Open session")
                                        .clicked()
                                {
                                    model.go(crate::Screen::Sessions, Some(w.session_id.clone()));
                                }
                            });
                            ui.end_row();
                        }
                    });
                for r in &st.crew_runs {
                    ui.horizontal(|ui| {
                        ui.label(icons::label(icons::USERS_THREE, &r.crew));
                        ui.label(RichText::new(&r.state).color(kit::status_color(&p, &r.state)));
                        ui.label(RichText::new(r.scope_dirs.join(" ")).color(p.muted));
                        ui.label(format!("max {}", r.max_workers));
                    });
                }
            },
        );
        ui.add_space(8.0);
    }

    // ---- provision ----------------------------------------------------------------------------

    fn provision(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        view: &HostView,
        g: &Gates,
        name: &str,
    ) {
        let p = palette(ui.ctx());
        if self.plan_sel.is_none() {
            self.plan_sel = view.plans.first().map(|p| p.id.clone());
        }
        let plan = view
            .plans
            .iter()
            .find(|x| Some(&x.id) == self.plan_sel.as_ref())
            .cloned();
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label("Plan");
            egui::ComboBox::from_id_salt("host-plan")
                .selected_text(
                    plan.as_ref()
                        .map(|x| x.name.clone())
                        .unwrap_or_else(|| "—".into()),
                )
                .show_ui(ui, |ui| {
                    for x in &view.plans {
                        if ui
                            .selectable_label(Some(&x.id) == self.plan_sel.as_ref(), &x.name)
                            .clicked()
                        {
                            self.plan_sel = Some(x.id.clone());
                            self.plan_previews.clear();
                        }
                    }
                });
            if kit::ghost_button(ui, &icons::label(icons::PLUS, "Baseline plan")).clicked() {
                model.act(|m| async move {
                    hosts::save_plan(m.core(), "", "Baseline dev host", &hosts::baseline_steps())
                        .await?;
                    Ok(None)
                });
            }
            if let Some(pl) = &plan {
                if kit::ghost_button(ui, &icons::label(icons::COPY, "Duplicate")).clicked() {
                    let (n, s) = (format!("{} copy", pl.name), pl.steps.clone());
                    model.act(move |m| async move {
                        hosts::save_plan(m.core(), "", &n, &s).await?;
                        Ok(None)
                    });
                }
                if kit::danger_button(ui, icons::TRASH)
                    .on_hover_text("Delete plan")
                    .clicked()
                {
                    let id = pl.id.clone();
                    self.plan_sel = None;
                    model.act(move |m| async move {
                        hosts::delete_plan(m.core(), &id).await?;
                        Ok(None)
                    });
                }
            }
            if inv.belai.update_available
                && self.gated(
                    ui,
                    g,
                    "belai_upgrade",
                    icons::ARROW_UP,
                    &format!("belai {}", inv.belai.latest),
                )
            {
                self.preview(
                    model,
                    "belai_upgrade",
                    &[("version", &inv.belai.latest)],
                    "belai upgrade",
                );
            }
            if self.gated(ui, g, "rc_restart", icons::ARROWS_CLOCKWISE, "Restart rc") {
                // After an upgrade the host checks it came back on that version
                // and rolls back to belai.prev if not.
                let expect = upgrade_target(&view.actions);
                let mut params = vec![("force_after_s", "300")];
                if let Some(v) = expect.as_deref() {
                    params.push(("expect_version", v));
                }
                self.preview(model, "rc_restart", &params, "Restart rc");
            }
        });
        let Some(plan) = plan else {
            kit::empty(ui, icons::WRENCH, "No plans");
            self.history(ui, model, view);
            return;
        };
        let previews: Vec<Option<HostActionRow>> = self
            .plan_previews
            .iter()
            .map(|id| view.actions.iter().find(|a| &a.id == id).cloned())
            .collect();
        let mut remove: Option<usize> = None;
        egui::Grid::new("host-plan-steps")
            .num_columns(5)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["#", "Step", "Class", "Preview", ""] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for (i, s) in plan.steps.iter().enumerate() {
                    ui.label(format!("{}", i + 1));
                    let label = if s.label.is_empty() {
                        s.kind.clone()
                    } else {
                        s.label.clone()
                    };
                    ui.label(label)
                        .on_hover_text(serde_json::to_string(&s.params).unwrap_or_default());
                    ui.label(hosts::class_of(&s.kind).map(Class::as_str).unwrap_or("?"));
                    match previews.get(i).cloned().flatten() {
                        Some(r) if r.state == "done" => {
                            ui.label(kit::mono(&summary(&r.preview_json), 12.0, p.text))
                                .on_hover_text(pretty(&r.preview_json));
                        }
                        Some(r) => {
                            ui.label(RichText::new(&r.state).color(state_color(&p, &r.state)))
                                .on_hover_text(&r.error);
                        }
                        None => {
                            ui.label(RichText::new("—").color(p.muted));
                        }
                    }
                    if kit::ghost_button(ui, icons::X)
                        .on_hover_text("Remove step")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            let mut steps = plan.steps.clone();
            steps.remove(i);
            self.save_steps(model, &plan.id, &plan.name, steps);
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("host-step-kind")
                .selected_text(&self.new_step_kind)
                .show_ui(ui, |ui| {
                    for (k, _) in hosts::KINDS {
                        ui.selectable_value(&mut self.new_step_kind, k.to_string(), k);
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.new_step_params)
                    .hint_text("key=value, key=value")
                    .desired_width(260.0),
            );
            if kit::ghost_button(ui, &icons::label(icons::PLUS, "Step")).clicked() {
                let mut steps = plan.steps.clone();
                steps.push(PlanStep {
                    kind: self.new_step_kind.clone(),
                    params: parse_params(&self.new_step_params),
                    label: String::new(),
                });
                self.new_step_params.clear();
                self.save_steps(model, &plan.id, &plan.name, steps);
            }
        });
        ui.add_space(6.0);
        let all_done = !previews.is_empty()
            && previews.len() == plan.steps.len()
            && previews
                .iter()
                .all(|r| r.as_ref().is_some_and(|r| r.state == "done"));
        ui.horizontal_wrapped(|ui| {
            let can = g.online && !g.stopped && !plan.steps.is_empty();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(icons::label(icons::EYE, "Preview on host")),
                )
                .on_disabled_hover_text(if g.online {
                    "Factory stopped"
                } else {
                    "Offline"
                })
                .clicked()
            {
                let (pid, node, o, inbox) = (
                    plan.id.clone(),
                    self.node_id.clone(),
                    self.origin(),
                    self.inbox.clone(),
                );
                model.act(move |m| async move {
                    let rows = hosts::preview_plan(m.core(), &pid, &node, o).await?;
                    inbox
                        .lock()
                        .unwrap()
                        .push(Msg::PlanPreviews(rows.into_iter().map(|r| r.id).collect()));
                    m.egui_ctx().request_repaint();
                    Ok(None)
                });
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.plan_typed)
                    .hint_text(format!("type {name}"))
                    .desired_width(200.0),
            );
            let ok = all_done && can && self.plan_typed.trim() == name;
            if ui
                .add_enabled(ok, egui::Button::new(icons::label(icons::CHECK, "Run")))
                .on_disabled_hover_text(if !all_done {
                    "Preview every step"
                } else {
                    "Type the host name"
                })
                .clicked()
            {
                let (pid, node, typed, ids, o) = (
                    plan.id.clone(),
                    self.node_id.clone(),
                    self.plan_typed.trim().to_string(),
                    self.plan_previews.clone(),
                    self.origin(),
                );
                self.plan_typed.clear();
                model.act(move |m| async move {
                    hosts::run_plan(m.core(), &pid, &node, &typed, &ids, o).await?;
                    Ok(Some(Notice::Info("Plan started".into())))
                });
            }
        });
        self.history(ui, model, view);
    }

    fn save_steps(&self, model: &Model, id: &str, name: &str, steps: Vec<PlanStep>) {
        let (id, name) = (id.to_string(), name.to_string());
        model.act(move |m| async move {
            hosts::save_plan(m.core(), &id, &name, &steps).await?;
            Ok(None)
        });
    }

    /// HM-9 / §3.11: the running plan (else the latest run), each step with
    /// its progress bar, and the scrubbed log (≤200 lines) of the step in
    /// flight or the one picked.
    fn run_progress(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView) {
        let Some(run) = view
            .plan_runs
            .iter()
            .find(|r| matches!(r.state.as_str(), "running" | "cancelling"))
            .or_else(|| view.plan_runs.first())
            .cloned()
        else {
            return;
        };
        let p = palette(ui.ctx());
        let steps = view
            .plans
            .iter()
            .find(|x| x.id == run.plan_id)
            .map(|x| x.steps.clone())
            .unwrap_or_default();
        let acts = view.run_actions(&run.id);
        let live = matches!(run.state.as_str(), "running" | "cancelling");
        // Follow the step in flight unless the person picked one.
        if live
            && let Some(cur) = acts.iter().rev().find(|a| !a.settled())
            && self.run_lines.as_ref().is_none_or(|(id, _)| {
                // Not this run's, or the step that just finished.
                acts.iter()
                    .find(|a| &a.id == id)
                    .is_none_or(|a| a.settled() && a.step + 1 == cur.step)
            })
        {
            self.pick_run_lines(model, &cur.id);
        }
        ui.add_space(8.0);
        let title = format!(
            "{} · {}",
            if live { "Running" } else { "Last run" },
            run.state
        );
        let mut pick: Option<String> = None;
        kit::section(
            ui,
            icons::WRENCH,
            &title,
            live,
            |_| {},
            |ui| {
                let total = steps.len().max(acts.len()).max(1);
                ui.add(
                    egui::ProgressBar::new(run_fraction(&acts, total))
                        .text(format!("step {}/{total}", run.step.max(0)))
                        .desired_width(320.0),
                );
                egui::Grid::new("host-run-steps")
                    .num_columns(4)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for i in 0..total {
                            let n = i as i64 + 1;
                            let label = steps
                                .get(i)
                                .map(|s| {
                                    if s.label.is_empty() {
                                        s.kind.clone()
                                    } else {
                                        s.label.clone()
                                    }
                                })
                                .unwrap_or_else(|| format!("step {n}"));
                            ui.label(format!("{n}"));
                            ui.label(label);
                            match acts.iter().rev().find(|a| a.step == n) {
                                Some(a) => {
                                    let frac = if a.state == "done" {
                                        1.0
                                    } else {
                                        a.pct as f32 / 100.0
                                    };
                                    let text = if a.step_label.is_empty() {
                                        a.state.clone()
                                    } else {
                                        format!("{} · {}", a.state, a.step_label)
                                    };
                                    ui.add(
                                        egui::ProgressBar::new(frac)
                                            .text(text)
                                            .desired_width(220.0),
                                    )
                                    .on_hover_text(&a.error);
                                    if kit::ghost_button(ui, icons::FILE_TEXT)
                                        .on_hover_text("Log")
                                        .clicked()
                                    {
                                        pick = Some(a.id.clone());
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("—").color(p.muted));
                                    ui.label("");
                                }
                            }
                            ui.end_row();
                        }
                    });
                if let Some((id, l)) = &self.run_lines
                    && acts.iter().any(|a| &a.id == id)
                {
                    ui.label(kit::mono(&format!("log {}", short(id)), 11.0, p.muted));
                    let text = match l.get() {
                        Some(Ok(rows)) => tail_lines(&rows, RUN_LOG_LINES),
                        Some(Err(e)) => e,
                        None => String::new(),
                    };
                    egui::ScrollArea::vertical()
                        .id_salt("host-run-log")
                        .max_height(220.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(kit::mono(&text, 11.0, p.text)).selectable(true),
                            );
                        });
                }
            },
        );
        if let Some(id) = pick {
            self.pick_run_lines(model, &id);
        }
    }

    fn pick_run_lines(&mut self, model: &Model, action_id: &str) {
        if self
            .run_lines
            .as_ref()
            .is_some_and(|(id, _)| id == action_id)
        {
            return;
        }
        let id2 = action_id.to_string();
        let l = Loader::new(model, &["host.action."], move |core| {
            let i = id2.clone();
            async move { hosts::lines(&core, &i).await }
        });
        self.run_lines = Some((action_id.to_string(), l));
    }

    fn history(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView) {
        self.run_progress(ui, model, view);
        let p = palette(ui.ctx());
        ui.add_space(8.0);
        kit::section(
            ui,
            icons::CLOCK,
            "History",
            false,
            |_| {},
            |ui| {
                if view.plan_runs.is_empty() {
                    kit::empty(ui, icons::CLOCK, "No runs");
                }
                for r in &view.plan_runs {
                    let o: Origin = serde_json::from_str(&r.origin_json).unwrap_or_default();
                    let name = view
                        .plans
                        .iter()
                        .find(|x| x.id == r.plan_id)
                        .map(|x| x.name.clone())
                        .unwrap_or_else(|| short(&r.plan_id).into());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&r.state).color(state_color(&p, &r.state)));
                        ui.label(fmt::when(r.started_at));
                        ui.label(RichText::new(&name).strong());
                        ui.label(RichText::new(format!("by {}", o.label())).color(p.muted));
                        ui.label(format!("step {}", r.step));
                        if !r.error.is_empty() {
                            ui.colored_label(p.red, &r.error);
                        }
                        if r.state == "running"
                            && kit::ghost_button(ui, &icons::label(icons::STOP, "Cancel")).clicked()
                        {
                            let id = r.id.clone();
                            model.act(move |m| async move {
                                hosts::cancel_plan_run(m.core(), &id).await?;
                                Ok(None)
                            });
                        }
                    });
                }
            },
        );
    }

    // ---- diagnostics --------------------------------------------------------------------------

    fn diagnostics(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        view: &HostView,
        g: &Gates,
        node: &kls_store::Node,
    ) {
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        let inv = view
            .inventory
            .as_ref()
            .map(|(_, i)| i.clone())
            .unwrap_or_default();
        kit::section(
            ui,
            icons::LIST_CHECKS,
            "Preflight",
            false,
            |_| {},
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if self.gated(ui, g, "preflight", icons::ARROW_CLOCKWISE, "Run") {
                        self.request(model, "preflight", &[]);
                    }
                    for c in &inv.preflight {
                        let mark = match c.status.as_str() {
                            "ok" => "✓",
                            "warn" => "!",
                            "skip" => "—",
                            _ => "✗",
                        };
                        ui.label(
                            RichText::new(format!("{mark} {}", c.id))
                                .color(state_color(&p, &c.status)),
                        )
                        .on_hover_text(&c.hint);
                    }
                });
            },
        );
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::PLUGS_CONNECTED,
            "Link",
            false,
            |_| {},
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    status_line(ui, view.online, "link");
                    ui.label(format!(
                        "seen {}",
                        node.last_seen_at
                            .map(|t| fmt::ago(now, t))
                            .unwrap_or_else(|| "never".into())
                    ));
                    match hosts::link_rtt_ms(&view.actions) {
                        Some(ms) => ui
                            .label(format!("rtt ≤{ms}ms"))
                            .on_hover_text("Fastest recent action round trip"),
                        None => ui.label(RichText::new("rtt —").color(p.muted)),
                    };
                    ui.label(format!("resume_after {}", node.server_acked));
                    match inv.cert_left_ms(now) {
                        Some(left) => {
                            let c = if left > CERT_WARN_MS { p.text } else { p.amber };
                            ui.label(
                                RichText::new(if left > 0 {
                                    format!("cert expires {}", days(left))
                                } else {
                                    "cert expired".to_string()
                                })
                                .color(c),
                            )
                            .on_hover_text(fmt::when(inv.link.cert_not_after_ms));
                        }
                        None => {
                            ui.label(RichText::new("cert —").color(p.muted));
                        }
                    }
                    match inv.clock_skew_ms() {
                        Some(ms) => {
                            let c = if ms.abs() > SKEW_WARN_MS {
                                p.amber
                            } else {
                                p.text
                            };
                            ui.label(RichText::new(format!("clock skew {}", skew(ms))).color(c))
                                .on_hover_text(
                                    "Host clock at report minus arrival (latency included)",
                                );
                        }
                        None => {
                            ui.label(RichText::new("clock skew —").color(p.muted));
                        }
                    }
                });
            },
        );
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::DATABASE,
            "Disk",
            false,
            |_| {},
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("sessions {}", bytes(inv.disk.sessions)));
                    ui.label(format!("worktrees {}", bytes(inv.disk.worktrees)));
                    ui.label(format!("logs {}", bytes(inv.disk.logs)));
                    ui.label(format!("free {}", bytes(inv.disk.free)));
                    if self.gated(ui, g, "session_prune", icons::TRASH, "Prune sessions") {
                        self.request(model, "session_prune", &[]);
                    }
                    let stale: Vec<String> = inv
                        .worktrees
                        .iter()
                        .filter(|w| w.stale)
                        .map(|w| w.path.clone())
                        .collect();
                    if !stale.is_empty()
                        && self.gated(
                            ui,
                            g,
                            "worktree_drop",
                            icons::TRASH,
                            &format!("Drop worktrees ({})", stale.len()),
                        )
                    {
                        for w in &stale {
                            self.request(model, "worktree_drop", &[("path", w)]);
                        }
                    }
                });
                for w in inv.worktrees.iter().filter(|w| w.stale) {
                    ui.horizontal(|ui| {
                        ui.label(kit::mono(&w.path, 12.0, p.muted));
                        ui.label(bytes(w.bytes));
                        if self.gated(ui, g, "worktree_drop", icons::TRASH, "Drop") {
                            self.request(model, "worktree_drop", &[("path", &w.path)]);
                        }
                    });
                }
            },
        );
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::WRENCH,
            "Tools",
            false,
            |_| {},
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    for t in &inv.tools {
                        let v = if t.version.is_empty() {
                            "—"
                        } else {
                            &t.version
                        };
                        ui.label(
                            RichText::new(format!(
                                "{} {v} {}",
                                t.name,
                                if t.ok { "✓" } else { "" }
                            ))
                            .color(if t.ok {
                                p.text
                            } else {
                                p.muted
                            }),
                        )
                        .on_hover_text(&t.note);
                    }
                });
            },
        );
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::FILE_TEXT,
            "Log",
            false,
            |_| {},
            |ui| {
                ui.horizontal(|ui| {
                    ui.label("rc.log");
                    egui::ComboBox::from_id_salt("host-log-lines")
                        .selected_text(format!("lines {}", self.log_lines))
                        .show_ui(ui, |ui| {
                            for n in [200, 500, 1000] {
                                ui.selectable_value(&mut self.log_lines, n, n.to_string());
                            }
                        });
                    if self.gated(ui, g, "log_tail", icons::ARROW_CLOCKWISE, "Fetch") {
                        let n = self.log_lines.to_string();
                        self.request(model, "log_tail", &[("lines", &n)]);
                    }
                    if kit::ghost_button(ui, &icons::label(icons::COPY, "Copy bundle"))
                        .on_hover_text("Inventory and checks, no secrets")
                        .clicked()
                    {
                        ui.ctx()
                            .copy_text(serde_json::to_string_pretty(&inv).unwrap_or_default());
                    }
                });
                if let Some(a) = latest_done(&view.actions, "log_tail") {
                    let v: serde_json::Value =
                        serde_json::from_str(&a.result_json).unwrap_or_default();
                    let text = v
                        .get("lines")
                        .and_then(|l| l.as_array())
                        .map(|l| {
                            l.iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_else(|| pretty(&a.result_json));
                    egui::ScrollArea::vertical()
                        .id_salt("host-log")
                        .max_height(320.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(kit::mono(&text, 11.0, p.text)).selectable(true),
                            );
                        });
                }
            },
        );
        self.shell(ui, model, g);
    }

    /// HM-15: one command, no shell, only when the host runs with
    /// --keleustes-shell and the provision gate. Preview, then the host name.
    fn shell(&mut self, ui: &mut egui::Ui, model: &Model, g: &Gates) {
        if g.state("shell_exec") == GateState::Hidden {
            return;
        }
        ui.add_space(6.0);
        kit::section(
            ui,
            icons::TERMINAL,
            "Shell",
            false,
            |_| {},
            |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.shell_argv)
                            .hint_text("program arg arg")
                            .desired_width(360.0),
                    );
                    let argv: Vec<&str> = self.shell_argv.split_whitespace().collect();
                    if !argv.is_empty() && self.gated(ui, g, "shell_exec", icons::EYE, "Preview") {
                        let json = serde_json::to_string(&argv).unwrap_or_default();
                        self.preview(model, "shell_exec", &[("argv_json", &json)], "Shell");
                    }
                });
            },
        );
    }

    // ---- audit --------------------------------------------------------------------------------

    fn audit(&mut self, ui: &mut egui::Ui, model: &Model, view: &HostView) {
        let p = palette(ui.ctx());
        if kit::ghost_button(ui, &icons::label(icons::ARROW_SQUARE_OUT, "Open in Audit")).clicked()
        {
            model.go(crate::Screen::Audit, Some(format!("node:{}", self.node_id)));
        }
        let mut pick: Option<String> = None;
        egui::Grid::new("host-actions")
            .num_columns(6)
            .striped(true)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["When", "Action", "Class", "State", "Origin", "Error"] {
                    kit::th(ui, h);
                }
                ui.end_row();
                for a in view.actions.iter().take(200) {
                    ui.label(RichText::new(fmt::when(a.requested_at)).color(p.muted));
                    let label = format!("{}{}", a.kind, if a.dry_run { " (preview)" } else { "" });
                    if kit::link(ui, &label, p.cyan)
                        .on_hover_text("Lines")
                        .clicked()
                    {
                        pick = Some(a.id.clone());
                    }
                    ui.label(&a.class);
                    ui.label(RichText::new(&a.state).color(state_color(&p, &a.state)));
                    ui.label(RichText::new(a.origin().label()).color(p.muted));
                    ui.label(RichText::new(&a.error).color(p.red));
                    ui.end_row();
                }
            });
        if let Some(id) = pick {
            let id2 = id.clone();
            let l = Loader::new(model, &["host.action."], move |core| {
                let i = id2.clone();
                async move { hosts::lines(&core, &i).await }
            });
            self.lines = Some((id, l));
        }
        if let Some((id, l)) = &self.lines {
            ui.add_space(6.0);
            ui.label(kit::mono(&format!("lines {}", short(id)), 12.0, p.muted));
            if let Some(Ok(rows)) = l.get() {
                let text = rows
                    .iter()
                    .map(|r| r.line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                egui::ScrollArea::vertical()
                    .id_salt("host-lines")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(kit::mono(&text, 11.0, p.text)).selectable(true));
                    });
            }
        }
    }
}

// ---- helpers ---------------------------------------------------------------------------------

/// Settings keys the Config tab offers, with how the host takes them
/// (belai `config.RemoteKeys`; the host is the final gate).
const REMOTE_KEYS: &[(&str, &str, &str)] = &[
    ("Model", "model", "set"),
    ("Model", "effort", "set"),
    ("Safety", "sandbox.mode", "tighten"),
    ("Safety", "guardrails", "tighten"),
    ("Safety", "ask_permission", "tighten"),
    ("Safety", "read_only", "tighten"),
    ("Workers", "agents.enabled", "set"),
    ("Workers", "agents.max_workers", "set"),
    ("Sessions", "session_retention_days", "set"),
    ("Sync", "sync.remote_prompts", "tighten"),
    ("Sync", "sync.hooks", "tighten"),
    ("A2A", "a2a.enabled", "tighten"),
    // OpenTelemetry export (row 50): facts only; headers name a host env var.
    ("Telemetry", "telemetry.otlp_endpoint", "set"),
    ("Telemetry", "telemetry.traces", "set"),
    ("Telemetry", "telemetry.metrics", "set"),
];

/// The prefix of one OTLP export header key (`telemetry.headers.<Name>`).
const TELEMETRY_HEADER: &str = "telemetry.headers.";

/// A `config_get` result as key → value: the host's `remote_keys` rows
/// first (the effective values), then its redacted settings flattened.
pub fn config_values(result_json: &str) -> BTreeMap<String, String> {
    let v: serde_json::Value = serde_json::from_str(result_json).unwrap_or_default();
    let mut out = flatten(v.get("settings").unwrap_or(&serde_json::Value::Null));
    for k in v
        .get("remote_keys")
        .and_then(|k| k.as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(key), Some(val)) = (k["key"].as_str(), k["value"].as_str()) {
            out.insert(key.to_string(), val.to_string());
        }
    }
    out
}

/// The OTLP header names a `config_get` result lists (values stay redacted).
pub fn telemetry_headers(values: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = values
        .keys()
        .filter_map(|k| k.strip_prefix(TELEMETRY_HEADER))
        .map(|rest| rest.split('.').next().unwrap_or(rest).to_string())
        .filter(|n| !n.is_empty())
        .collect();
    out.dedup();
    out
}

/// The built-in agents a repo can switch: what the host advertises, then
/// the bundled catalogue (builtin-agents P1) for a host that lists none.
pub fn agent_names(advertised: &[String]) -> Vec<String> {
    let mut out: Vec<String> = advertised.to_vec();
    if out.is_empty() {
        out = kls_engine::catalogue::profiles()
            .iter()
            .filter(|p| p.is_worker() || p.is_background())
            .map(|p| p.name.clone())
            .collect();
    }
    out.sort();
    out.dedup();
    out
}

/// One name for a library kind on both sides (a host's "profile" is the
/// library's agent kind).
fn lib_kind(kind: &str) -> String {
    if kind == "profile" {
        kls_core::vault::AGENT_KIND.to_string()
    } else {
        kind.to_string()
    }
}

/// Vulnetix scanner subcommands a repo may run.
const SCANS: [&str; 5] = ["sca", "sast", "secrets", "iac", "containers"];

/// A sensor whose profile is locked reports a "protection" check (belai
/// `internal/products/sensor.go`): its uninstall needs a code.
fn locked(st: &hosts::ProductStatus) -> bool {
    st.product == "sensor" && st.checks.iter().any(|c| c.id == "protection")
}

fn product_name(id: &str) -> &'static str {
    match id {
        "cli" => "CLI",
        "package_firewall" => "Package firewall",
        "ai_firewall" => "AI firewall",
        "sensor" => "Sensor",
        _ => "Product",
    }
}

fn product_icon(id: &str) -> &'static str {
    match id {
        "cli" => icons::TERMINAL,
        "package_firewall" => icons::PACKAGE,
        "ai_firewall" => icons::SHIELD_CHECK,
        "sensor" => icons::RADAR,
        _ => icons::CIRCLE,
    }
}

fn product_parts_label(id: &str) -> &'static str {
    if id == "ai_firewall" {
        "Clients"
    } else {
        "Ecosystems"
    }
}

/// The Vulnesis console page for a product (org policy lives there).
fn console_url(id: &str) -> &'static str {
    match id {
        "package_firewall" => "https://www.vulnes.is/resolve/package-firewall",
        "ai_firewall" => "https://www.vulnes.is/resolve/ai-firewall",
        "sensor" => "https://www.vulnes.is/resolve/sensor",
        _ => "https://www.vulnes.is/",
    }
}

fn auth_mode(s: &hosts::ProductStatus) -> String {
    match s.product.as_str() {
        "cli" if s.authenticated => format!("org {} {}", short(&s.org_id), s.auth_source),
        "cli" => "signed out".into(),
        "sensor" if !s.mode.is_empty() => format!(
            "{} {}",
            s.mode,
            if s.running { "running" } else { "stopped" }
        ),
        _ => "—".into(),
    }
}

fn state_color(p: &crate::theme::Palette, state: &str) -> egui::Color32 {
    match state {
        "active" | "configured" | "installed" | "ok" | "done" | "accepted" => p.green,
        "degraded" | "warn" | "needs_human" | "running" | "queued" => p.amber,
        "error" | "failed" | "refused" => p.red,
        "absent" | "unsupported" | "skip" | "cancelled" | "—" | "" => p.muted,
        s => kit::status_color(p, s),
    }
}

fn status_line(ui: &mut egui::Ui, ok: bool, text: &str) {
    let p = palette(ui.ctx());
    let (m, c) = if ok { ("✓", p.green) } else { ("!", p.amber) };
    ui.label(RichText::new(format!("{m} {text}")).color(c));
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "—" } else { s }
}

fn short(s: &str) -> &str {
    s.get(..8).unwrap_or(s)
}

fn param(a: &HostActionRow, k: &str) -> String {
    a.params().get(k).cloned().unwrap_or_default()
}

fn repo_title(r: &hosts::HostRepo) -> String {
    if r.owner.is_empty() {
        r.path.clone()
    } else {
        format!("{}/{}", r.owner, r.name)
    }
}

fn bytes(n: i64) -> String {
    let f = n as f64;
    if n <= 0 {
        "—".into()
    } else if f >= 1e9 {
        format!("{:.1}G", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.0}M", f / 1e6)
    } else {
        format!("{:.0}K", f / 1e3)
    }
}

fn pretty(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string())
}

/// One line for a preview: its argv when it has one.
fn summary(preview_json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(preview_json).unwrap_or_default();
    let argv = v.get("argv").and_then(|a| a.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    });
    let s = argv.unwrap_or_else(|| preview_json.to_string());
    s.chars().take(80).collect()
}

/// The version a finished belai upgrade installed, when no restart has run
/// since: what the next rc_restart should come back on.
pub fn upgrade_target(actions: &[HostActionRow]) -> Option<String> {
    let up = latest_done(actions, "belai_upgrade")?;
    if latest_done(actions, "rc_restart").is_some_and(|r| r.requested_at > up.requested_at) {
        return None;
    }
    up.params()
        .get("version")
        .cloned()
        .filter(|v| !v.is_empty())
}

/// The newest finished action of `kind`.
fn latest_done(actions: &[HostActionRow], kind: &str) -> Option<HostActionRow> {
    actions
        .iter()
        .filter(|a| a.kind == kind && a.state == "done" && !a.dry_run)
        .max_by_key(|a| a.requested_at)
        .cloned()
}

/// The latest `config_get` for a scope: global (`None`) or a project path.
pub fn latest_config(actions: &[HostActionRow], project: Option<&str>) -> Option<HostActionRow> {
    actions
        .iter()
        .filter(|a| a.kind == "config_get" && a.state == "done" && !a.dry_run)
        .filter(|a| {
            let ps = a.params();
            let scope = ps.get("scope").map(String::as_str).unwrap_or("global");
            match project {
                None => scope != "project",
                Some(p) => scope == "project" && ps.get("path").is_some_and(|x| x == p),
            }
        })
        .max_by_key(|a| a.requested_at)
        .cloned()
}

/// A certificate with less than this left shows amber.
const CERT_WARN_MS: i64 = 30 * 86_400_000;
/// A clock further off than this shows amber.
const SKEW_WARN_MS: i64 = 5_000;
/// The provision log keeps the last lines only (§3.11).
const RUN_LOG_LINES: usize = 200;

/// "21d", "5h", "12m".
fn days(ms: i64) -> String {
    let ms = ms.max(0);
    if ms >= 86_400_000 {
        format!("{}d", ms / 86_400_000)
    } else if ms >= 3_600_000 {
        format!("{}h", ms / 3_600_000)
    } else {
        format!("{}m", ms / 60_000)
    }
}

/// "+0.2s", "-3.0s".
fn skew(ms: i64) -> String {
    format!(
        "{}{:.1}s",
        if ms >= 0 { "+" } else { "-" },
        ms.abs() as f64 / 1000.0
    )
}

/// How far a plan run is: done steps plus the running one's percentage.
pub fn run_fraction(acts: &[HostActionRow], total: usize) -> f32 {
    let mut by_step: BTreeMap<i64, f32> = BTreeMap::new();
    for a in acts {
        let f = if a.state == "done" {
            1.0
        } else {
            a.pct.clamp(0, 100) as f32 / 100.0
        };
        by_step.insert(a.step, f);
    }
    (by_step.values().sum::<f32>() / total.max(1) as f32).clamp(0.0, 1.0)
}

/// The last `n` lines, joined.
pub fn tail_lines(rows: &[kls_core::hosts::HostActionLine], n: usize) -> String {
    let skip = rows.len().saturating_sub(n);
    rows[skip..]
        .iter()
        .map(|r| r.line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a worktree belongs to a repo row (by path, owner/name or name).
pub fn worktree_of(w: &hosts::WorktreeFact, r: &hosts::HostRepo) -> bool {
    !w.repo.is_empty()
        && (w.repo == r.path
            || (!r.owner.is_empty() && w.repo == format!("{}/{}", r.owner, r.name))
            || (!r.name.is_empty() && w.repo == r.name))
}

/// Whether a session's cwd is in a repo path the inventory reports
/// (home-relative `~/src/api` against an absolute cwd).
pub fn cwd_matches(cwd: &str, path: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return false;
    }
    let under = |base: &str| cwd == base || cwd.starts_with(&format!("{base}/"));
    match path.strip_prefix('~') {
        Some(rest) if !rest.is_empty() => {
            under(path) || {
                // Absolute cwd: some home directory, then the rest.
                cwd.find(rest).is_some_and(|i| {
                    let after = &cwd[i + rest.len()..];
                    (after.is_empty() || after.starts_with('/'))
                        && cwd[..i].matches('/').count() >= 1
                })
            }
        }
        _ => under(path),
    }
}

fn repo_label(inv: &Inventory, path: &str) -> String {
    inv.repos
        .iter()
        .find(|r| r.path == path)
        .map(repo_title)
        .unwrap_or_else(|| path.to_string())
}

/// Dotted keys for a JSON object (`{"a":{"b":1}}` → `a.b = 1`).
pub fn flatten(v: &serde_json::Value) -> BTreeMap<String, String> {
    fn walk(prefix: &str, v: &serde_json::Value, out: &mut BTreeMap<String, String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&key, x, out);
                }
            }
            serde_json::Value::String(s) => {
                out.insert(prefix.into(), s.clone());
            }
            other => {
                out.insert(prefix.into(), other.to_string());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk("", v, &mut out);
    out
}

/// `key=value, key=value` into step params.
pub fn parse_params(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_split_node_tab_and_item() {
        assert_eq!(
            parse_target("n1/products?item=cli"),
            Some(Target {
                node: "n1".into(),
                tab: Tab::Products,
                item: Some("cli".into())
            })
        );
        assert_eq!(parse_target("n1").unwrap().tab, Tab::Overview);
        assert_eq!(parse_target("n1/bogus").unwrap().tab, Tab::Overview);
        assert_eq!(
            parse_target("n1/agents/acme/api").unwrap().item.as_deref(),
            Some("acme/api")
        );
        assert_eq!(parse_target(""), None);
        for t in Tab::ALL {
            assert_eq!(Tab::parse(t.slug()), t);
        }
        assert_eq!(link("n1", Tab::Repos), "keleustes://fleet/n1/repos");
    }

    fn gates(level: Level) -> Gates {
        Gates {
            online: true,
            stopped: false,
            level: Some(level),
            offered: hosts::KINDS.iter().map(|(k, _)| k.to_string()).collect(),
            shell: false,
            project_settings: true,
        }
    }

    #[test]
    fn gating_hides_unoffered_and_disables_by_gate() {
        let mut g = gates(Level::Read);
        assert_eq!(g.state("preflight"), GateState::Enabled);
        assert_eq!(
            g.state("config_set"),
            GateState::Disabled("Host gate: read".into())
        );
        assert_eq!(
            g.state("product_install"),
            GateState::Disabled("Host gate: read".into())
        );
        g.offered.retain(|k| k != "log_tail");
        assert_eq!(g.state("log_tail"), GateState::Hidden);
        assert_eq!(g.state("not_a_kind"), GateState::Hidden);

        let mut g = gates(Level::Provision);
        assert_eq!(g.state("product_install"), GateState::Enabled);
        assert_eq!(
            g.state("shell_exec"),
            GateState::Disabled("Host shell off".into())
        );
        g.stopped = true;
        assert_eq!(
            g.state("config_set"),
            GateState::Disabled("Factory stopped".into())
        );
        assert_eq!(g.state("preflight"), GateState::Enabled, "a click may read");
        g.online = false;
        assert_eq!(g.state("preflight"), GateState::Disabled("Offline".into()));
        // An older belai lists nothing: every action hidden.
        let old = Gates::default();
        for (k, _) in hosts::KINDS {
            assert_eq!(old.state(k), GateState::Hidden);
        }
    }

    #[test]
    fn repo_rows_link_to_repos() {
        let r = hosts::HostRepo {
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        };
        assert_eq!(r.link().as_deref(), Some("keleustes://repos/acme/api"));
        assert_eq!(hosts::HostRepo::default().link(), None);
    }

    #[test]
    fn restart_expects_the_upgraded_version_until_it_ran() {
        let row = |kind: &str, at: i64, params: &str| HostActionRow {
            kind: kind.into(),
            state: "done".into(),
            requested_at: at,
            params_json: params.into(),
            ..Default::default()
        };
        let up = row("belai_upgrade", 10, r#"{"version":"v0.44.0"}"#);
        assert_eq!(
            upgrade_target(std::slice::from_ref(&up)).as_deref(),
            Some("v0.44.0")
        );
        let old_restart = row("rc_restart", 5, "{}");
        assert_eq!(
            upgrade_target(&[up.clone(), old_restart]).as_deref(),
            Some("v0.44.0")
        );
        let after = row("rc_restart", 20, "{}");
        assert_eq!(upgrade_target(&[up, after]), None);
        assert_eq!(upgrade_target(&[]), None);
    }

    #[test]
    fn flatten_and_params() {
        let v = serde_json::json!({"agents": {"max_workers": 4, "enabled": true}, "model": "m"});
        let f = flatten(&v);
        assert_eq!(f["agents.max_workers"], "4");
        assert_eq!(f["model"], "m");
        let p = parse_params("product=cli, on = npm");
        assert_eq!(p["product"], "cli");
        assert_eq!(p["on"], "npm");
    }

    #[test]
    fn config_values_read_remote_keys_and_list_telemetry_headers() {
        let r = serde_json::json!({
            "scope": "global",
            "settings": {"model": "m", "telemetry": {"headers": {"x-api-key": {"present": true}}}},
            "remote_keys": [
                {"key": "telemetry.otlp_endpoint", "value": "http://localhost:4318"},
                {"key": "model", "value": "m2"}
            ]
        });
        let v = config_values(&r.to_string());
        assert_eq!(v["telemetry.otlp_endpoint"], "http://localhost:4318");
        assert_eq!(v["model"], "m2");
        assert_eq!(telemetry_headers(&v), ["x-api-key"]);
        assert!(
            REMOTE_KEYS
                .iter()
                .any(|(s, k, _)| *s == "Telemetry" && *k == "telemetry.otlp_endpoint")
        );
    }

    #[test]
    fn session_cwd_matches_home_relative_repo_paths() {
        assert!(cwd_matches("/home/u/src/api", "~/src/api"));
        assert!(cwd_matches("/home/u/src/api/sub", "~/src/api"));
        assert!(cwd_matches("~/src/api", "~/src/api/"));
        assert!(!cwd_matches("/home/u/src/api2", "~/src/api"));
        assert!(!cwd_matches("/srv/api", "~/src/api"));
        assert!(cwd_matches("/srv/api", "/srv/api"));
        assert!(!cwd_matches("/srv/api", ""));
    }

    #[test]
    fn worktrees_belong_to_their_repo_row() {
        let r = hosts::HostRepo {
            path: "~/src/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        };
        let w = |repo: &str| hosts::WorktreeFact {
            repo: repo.into(),
            ..Default::default()
        };
        assert!(worktree_of(&w("~/src/api"), &r));
        assert!(worktree_of(&w("acme/api"), &r));
        assert!(worktree_of(&w("api"), &r));
        assert!(!worktree_of(&w("acme/web"), &r));
        assert!(!worktree_of(&w(""), &r));
    }

    #[test]
    fn config_scope_picks_its_own_load() {
        let get = |id: &str, at: i64, params: &str| HostActionRow {
            id: id.into(),
            kind: "config_get".into(),
            state: "done".into(),
            requested_at: at,
            params_json: params.into(),
            ..Default::default()
        };
        let acts = [
            get("g", 1, r#"{"scope":"global"}"#),
            get("p1", 2, r#"{"scope":"project","path":"~/a"}"#),
            get("p2", 3, r#"{"scope":"project","path":"~/b"}"#),
        ];
        assert_eq!(latest_config(&acts, None).unwrap().id, "g");
        assert_eq!(latest_config(&acts, Some("~/a")).unwrap().id, "p1");
        assert!(latest_config(&acts, Some("~/c")).is_none());
    }

    #[test]
    fn run_progress_counts_done_and_running_steps() {
        let a = |step: i64, state: &str, pct: i64| HostActionRow {
            step,
            state: state.into(),
            pct,
            ..Default::default()
        };
        assert_eq!(run_fraction(&[], 4), 0.0);
        let f = run_fraction(&[a(1, "done", 0), a(2, "running", 50)], 4);
        assert!((f - 0.375).abs() < 1e-6, "{f}");
        let rows: Vec<kls_core::hosts::HostActionLine> = (0..250)
            .map(|i| kls_core::hosts::HostActionLine {
                line: format!("l{i}"),
                ..Default::default()
            })
            .collect();
        let t = tail_lines(&rows, RUN_LOG_LINES);
        assert_eq!(t.lines().count(), 200);
        assert!(t.starts_with("l50\n") && t.ends_with("l249"));
    }

    #[test]
    fn link_fact_labels() {
        assert_eq!(days(21 * 86_400_000 + 5), "21d");
        assert_eq!(days(5 * 3_600_000), "5h");
        assert_eq!(days(-1), "0m");
        assert_eq!(skew(200), "+0.2s");
        assert_eq!(skew(-3_000), "-3.0s");
    }

    #[test]
    fn trust_dir_and_worktree_drop_are_gated_like_any_change() {
        let mut g = gates(Level::Config);
        assert_eq!(g.state("repo_trust"), GateState::Enabled);
        assert_eq!(g.state("worktree_drop"), GateState::Enabled);
        g.stopped = true;
        assert!(matches!(g.state("repo_trust"), GateState::Disabled(_)));
        g.offered.retain(|k| k != "repo_trust");
        assert_eq!(g.state("repo_trust"), GateState::Hidden);
    }

    #[test]
    fn only_a_locked_sensor_asks_for_a_code() {
        let mut st = hosts::ProductStatus {
            product: "sensor".into(),
            ..Default::default()
        };
        assert!(!locked(&st));
        st.checks.push(hosts::Check {
            id: "protection".into(),
            status: "info".into(),
            hint: "locked".into(),
        });
        assert!(locked(&st));
        st.product = "cli".into();
        assert!(!locked(&st));
    }
}
