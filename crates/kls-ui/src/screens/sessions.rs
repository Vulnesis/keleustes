//! Sessions screen: live and recent belai sessions, a session's transcript
//! with its prompt composer, answer box and controls, and starting a new
//! session on an rc-capable node. Owner: M1 sessions workstream.

use crate::fmt;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::screens::workflows::text_fit;
use crate::theme::palette;
use crate::{icons, kit, theme};
use egui::{Align2, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use kls_core::DispatchSpec;
use kls_core::belai::{Accepts, PendingAsk, SessionExtras};
use kls_core::gateway::McpBinding;
use kls_core::sessions::start::StartFields;
use kls_core::sessions::{Dispatch, Entry, InboxBody, InboxItem, Session, SessionQuery};
use kls_store::{Node, now_ms};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;
/// The focus id that opens the New session dialog (`keleustes://sessions/new`).
pub const NEW_FOCUS: &str = "new";

/// A session the host has not reported for this long reads as quiet.
const LIVE_WINDOW_MS: i64 = 45_000;
/// Body lines shown for one entry before "… more".
const MAX_BODY_LINES: usize = 400;
const LIST_LIMIT: i64 = 300;
/// Width of the transcript's role column.
const ROLE_W: f32 = 104.0;

#[derive(Clone, Default)]
struct ListData {
    sessions: Arc<Vec<Session>>,
    nodes: Arc<Vec<Node>>,
    online: Arc<Vec<String>>,
    /// Session id to the crew its worker belongs to (node status).
    crews: Arc<Vec<(String, String)>>,
}

#[derive(Clone, Default)]
struct Detail {
    id: String,
    session: Option<Session>,
    entries: Arc<Vec<Entry>>,
    inbox: Arc<Vec<InboxItem>>,
    /// Structured ask, accepts, git and usage (WP-A).
    extras: Option<SessionExtras>,
    /// Who asked for it, when another node did (WP-I provenance).
    origin: String,
    /// Delivered / agent / verified, kept apart (O-061; OP-1 hook).
    axes: String,
    /// The `--label` a console dispatch carried (O-008; OP-1 hook).
    label: String,
    /// Run step, budgets, PR checks, cards, todo (WP5).
    context: kls_core::sessions::context::SessionContext,
    /// How it ended (outcome taxonomy); empty while open.
    outcome: String,
    /// The agent profile's effort, when known.
    effort: String,
}

/// Entries already loaded for the selected session, so a new entry costs
/// one small query instead of a full reload.
#[derive(Default)]
struct EntryCache {
    id: String,
    entries: Arc<Vec<Entry>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateFilter {
    Open,
    Waiting,
    Ended,
    All,
}

impl StateFilter {
    fn label(self) -> String {
        let (icon, word) = match self {
            StateFilter::Open => (icons::PLAY, "Live"),
            StateFilter::Waiting => (icons::CLOCK, "Waiting"),
            StateFilter::Ended => (icons::STOP, "Ended"),
            StateFilter::All => (icons::LIST, "All"),
        };
        icons::label(icon, word)
    }
}

/// Where a session came from (the list's source tabs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    All,
    /// Asked for from this console.
    Console,
    /// A workflow run.
    Run,
    /// A worker claiming cards.
    Worker,
    /// Started on the host itself (or by a peer node).
    Host,
}

impl Source {
    fn of(s: &Session) -> Source {
        if !s.run_id.is_empty() {
            Source::Run
        } else if s.kind == "worker" {
            Source::Worker
        } else if !s.dispatch_id.is_empty() {
            Source::Console
        } else {
            Source::Host
        }
    }

    fn label(self) -> String {
        let (icon, word) = match self {
            Source::All => (icons::LIST, "All"),
            Source::Console => (icons::TERMINAL_WINDOW, "Console"),
            Source::Run => (icons::FLOW_ARROW, "Runs"),
            Source::Worker => (icons::ROBOT, "Workers"),
            Source::Host => (icons::HARD_DRIVES, "Host"),
        };
        icons::label(icon, word)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Header,
    Body(usize),
    More(usize),
}

#[derive(Clone, Copy)]
struct Line {
    entry: usize,
    kind: LineKind,
}

/// What the New session dialog needs for a harness: the key policy, the
/// vault keys (metadata only) and ended harness sessions it may resume:
/// (node, harness, cwd, native session id, title).
#[derive(Clone, Default)]
struct HarnessCtx {
    policy: kls_core::harness::KeyPolicy,
    keys: Vec<kls_store::vault::SecretMeta>,
    resumable: Vec<(String, String, String, String, String)>,
    /// Measured startup per (node, harness) from stored end facts.
    startup: kls_core::harness::StartupTable,
    /// Stored MCP bindings (names only) the dialog may attach.
    mcp: Vec<McpBinding>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Transcript,
    Screen,
    A2a,
}

#[derive(Default)]
struct NewSession {
    node_id: String,
    cwd: String,
    prompt: String,
    provider: String,
    model: String,
    agent: String,
    /// A crew the node advertises; set, the dispatch starts the crew.
    crew: String,
    mode: String,
    /// Dispatch sent; waiting for its outcome ("" until its id is known).
    sent: Option<String>,
    /// Why the last attempt could not be sent.
    error: Option<String>,
    /// belai or a third-party harness (harnesses.md).
    picker: super::harness_picker::AgentPicker,
    /// Start the agent profile as a claiming worker instead of a session.
    as_worker: bool,
    /// The start fields (#7, #8): extra trusted directories, workspace,
    /// effort, git sync, ask, stay, crew fill and clone.
    dirs: Vec<String>,
    workspace: String,
    effort: String,
    git_sync: Option<bool>,
    ask: Option<bool>,
    stay: bool,
    /// Minutes a staying worker waits for work ("" = host default).
    stay_idle_min: String,
    fill: bool,
    /// `owner/name` or `owner/name@branch` the host clones first.
    clone: String,
    /// Stored MCP bindings picked for this dispatch, by (server, key).
    mcp: Vec<(String, String)>,
    /// The Options section is open.
    more: bool,
    /// #52: scripted answers (belai `-answers` JSON) and plan first.
    answers: String,
    plan_first: bool,
    /// #51: harness gates switched on (tighten only); commit mode "" = host.
    gate_on: Vec<&'static str>,
    commit_mode: String,
    /// #53: files and images on a session's first prompt.
    attach: super::attach::AttachBox,
}

impl NewSession {
    /// The dispatch kind: a crew when one is picked, a worker when asked
    /// for with a profile, else a session.
    fn kind(&self) -> &'static str {
        if !self.crew.trim().is_empty() {
            "crew"
        } else if self.as_worker && !self.agent.trim().is_empty() {
            "worker"
        } else {
            "session"
        }
    }

    /// The dispatch the dialog sends (start fields go on in core).
    fn spec(&self) -> DispatchSpec {
        DispatchSpec {
            kind: self.kind().into(),
            cwd: self.cwd.trim().to_string(),
            prompt: self.prompt.trim().to_string(),
            provider: self.provider.clone(),
            model: self.model.trim().to_string(),
            agent: self.agent.trim().to_string(),
            crew: self.crew.trim().to_string(),
            mode: if self.plan_first && self.kind() == "session" {
                "plan".into()
            } else {
                self.mode.clone()
            },
            ..Default::default()
        }
    }

    /// The start fields that fit the kind; the rest are left out.
    fn fields(&self, bindings: &[McpBinding]) -> Result<StartFields, String> {
        let kind = self.kind();
        let session = kind == "session";
        let mut f = StartFields {
            effort: self.effort.clone(),
            ask: self.ask,
            mcp: bindings
                .iter()
                .filter(|b| self.mcp.contains(&(b.server.clone(), b.key.clone())))
                .cloned()
                .collect(),
            ..Default::default()
        };
        if session {
            f.dirs = self
                .dirs
                .iter()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty() && d != self.cwd.trim())
                .collect();
            f.workspace = self.workspace.trim().to_string();
            f.git_sync = self.git_sync;
            f.answers_json = self.answers.trim().to_string();
            f.attachments = self.attach.list();
            for (k, v) in GATE_SWITCHES {
                if self.gate_on.contains(&k) {
                    f.gates.insert(k.into(), v.into());
                }
            }
            if !self.commit_mode.is_empty() {
                f.gates
                    .insert("commit_mode".into(), self.commit_mode.clone());
            }
        } else {
            f.stay = self.stay;
            if self.stay && !self.stay_idle_min.trim().is_empty() {
                let m: u32 = self
                    .stay_idle_min
                    .trim()
                    .parse()
                    .map_err(|_| "idle wait is a number of minutes".to_string())?;
                f.stay_idle_max_s = m.saturating_mul(60);
            }
            f.fill = kind == "crew" && self.fill;
            let c = self.clone.trim();
            if !c.is_empty() {
                let mut cfg = serde_json::Map::new();
                cfg.insert("clone".into(), c.into());
                f.clone = StartFields::from_config(&cfg)?.clone;
            }
        }
        f.validate(kind)?;
        Ok(f)
    }
}

pub struct Screen {
    list: Loader<ListData>,
    detail: Loader<Detail>,
    dispatches: Loader<Arc<Vec<Dispatch>>>,
    selected: Arc<Mutex<Option<String>>>,
    /// Result of the dispatch action just run (set from the async action).
    dispatched: Arc<Mutex<Option<Result<String, String>>>>,
    filter: String,
    state_filter: StateFilter,
    node_filter: String,
    /// The Agent filter: "" all, "belai", or a harness id (K2).
    agent_filter: String,
    composer: String,
    /// Files and images on the next prompt (#53).
    composer_attach: super::attach::AttachBox,
    answer: String,
    /// Clarify choices for the open ask: (ask id, chosen per group).
    clarify: (String, Vec<Vec<bool>>),
    /// The control picked from the menu and its arguments.
    control: Option<String>,
    control_args: String,
    /// The console session action picked (#45) and its text.
    action: Option<String>,
    action_text: String,
    /// The relay note being edited: (ask id, text).
    relay_edit: (String, String),
    /// The first prompt of a resume or fork (#46).
    resume_prompt: String,
    expanded: HashSet<i64>,
    /// Rebuilt when (session, entry count, expansion) changes.
    lines: Vec<Line>,
    lines_key: (String, usize, u64),
    /// Entries a rewind took off the conversation (#45), with `lines`.
    rewound: HashSet<i64>,
    expand_gen: u64,
    picked: Option<i64>,
    confirm_stop: bool,
    new: Option<NewSession>,
    /// Key policy, vault keys and resumable harness sessions for the dialog.
    harness_ctx: Loader<HarnessCtx>,
    tab: DetailTab,
    /// The attached harness screen: (session, view, live feed).
    screen_view: Option<(
        String,
        super::harness_screen::ScreenView,
        kls_node::screen::Attached,
    )>,
    a2a: Option<super::harness_a2a::A2aPanel>,
    source: Source,
    /// Group the list by crew.
    by_crew: bool,
    /// Git strip: the branch to switch to, the node to teleport to.
    git_branch: String,
    teleport_to: String,
    /// The factory context panel is open.
    show_context: bool,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let list = Loader::new(model, &["session.", "node."], |core| async move {
            let sessions = core
                .store()
                .sessions(&SessionQuery {
                    limit: LIST_LIMIT,
                    ..Default::default()
                })
                .await?;
            let online = match core.gateway() {
                Ok(g) => g.online().await,
                Err(_) => Vec::new(),
            };
            let crews = core
                .node_statuses()
                .await
                .unwrap_or_default()
                .into_iter()
                .flat_map(|(_, _, st)| st.workers)
                .filter(|w| !w.crew.is_empty() && !w.session_id.is_empty())
                .map(|w| (w.session_id, w.crew))
                .collect();
            Ok(ListData {
                sessions: Arc::new(sessions),
                nodes: Arc::new(core.store().nodes().await?),
                online: Arc::new(online),
                crews: Arc::new(crews),
            })
        });
        let selected: Arc<Mutex<Option<String>>> = Arc::default();
        let cache: Arc<Mutex<EntryCache>> = Arc::default();
        let sel = selected.clone();
        let detail = Loader::new(model, &["session."], move |core| {
            let sel = sel.clone();
            let cache = cache.clone();
            async move {
                let Some(id) = sel.lock().unwrap().clone() else {
                    return Ok(Detail::default());
                };
                let store = core.store();
                let session = store.session(&id).await?;
                let (have, from) = {
                    let c = cache.lock().unwrap();
                    if c.id == id {
                        (c.entries.clone(), c.entries.last().map(|e| e.idx + 1))
                    } else {
                        (Arc::default(), None)
                    }
                };
                let expect = session.as_ref().map(|s| s.entry_count as usize);
                let mut entries = match from {
                    Some(from) => {
                        let more = store.entries(&id, from, 0).await?;
                        if more.is_empty() {
                            have
                        } else {
                            let mut v = (*have).clone();
                            v.extend(more);
                            Arc::new(v)
                        }
                    }
                    None => Arc::new(store.entries(&id, 0, 0).await?),
                };
                // A host that filled a gap below the cached tail: reload all.
                if expect.is_some_and(|n| n != entries.len()) {
                    entries = Arc::new(store.entries(&id, 0, 0).await?);
                }
                *cache.lock().unwrap() = EntryCache {
                    id: id.clone(),
                    entries: entries.clone(),
                };
                let inbox = store.inbox_items(&id, 50).await?;
                let extras = store.session_extras(&id).await?;
                let mut origin = String::new();
                if let Some(s) = session.as_ref().filter(|s| !s.dispatch_id.is_empty())
                    && let Some(d) = store.dispatch(&s.dispatch_id).await?
                    && let Ok(spec) = serde_json::from_str::<DispatchSpec>(&d.spec_json)
                    && let Some(t) = spec.trigger.filter(|t| t.source == "node")
                {
                    origin = if t.origin_name.is_empty() {
                        t.origin_node
                    } else {
                        t.origin_name
                    };
                }
                let mut label = String::new();
                if let Some(s) = session.as_ref().filter(|s| !s.dispatch_id.is_empty())
                    && let Some(d) = store.dispatch(&s.dispatch_id).await?
                    && let Ok(spec) = serde_json::from_str::<DispatchSpec>(&d.spec_json)
                    && let Some(t) = spec.trigger.filter(|t| t.source == "console")
                {
                    label = t.subject;
                }
                let axes = core
                    .session_axes(&id)
                    .await
                    .map(|a| a.label())
                    .unwrap_or_default();
                let context = core.session_context(&id).await.unwrap_or_default();
                let outcome = match session.as_ref() {
                    Some(s) if !s.is_open() => core
                        .session_outcome(&id)
                        .await
                        .map(|o| o.outcome.to_string())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let mut effort = String::new();
                if let Some(s) = session.as_ref().filter(|s| !s.agent.is_empty())
                    && let Some(a) = store.agent_profile_by_name(&s.agent).await?
                {
                    effort = serde_json::from_str::<serde_json::Value>(&a.meta_json)
                        .ok()
                        .and_then(|v| v.get("effort").and_then(|e| e.as_str()).map(String::from))
                        .unwrap_or_default();
                }
                Ok(Detail {
                    context,
                    outcome,
                    effort,
                    id,
                    session,
                    entries,
                    inbox: Arc::new(inbox),
                    extras,
                    origin,
                    axes,
                    label,
                })
            }
        });
        let harness_ctx = Loader::new(
            model,
            &["harness.", "vault.", "session.ended"],
            |core| async move {
                let ended = core
                    .store()
                    .sessions(&SessionQuery {
                        limit: 200,
                        ..Default::default()
                    })
                    .await?
                    .into_iter()
                    .filter(|s| !s.is_open())
                    .collect::<Vec<_>>();
                let mut resumable = Vec::new();
                for s in ended {
                    if let Some(h) = core
                        .store()
                        .session_extras(&s.id)
                        .await?
                        .and_then(|x| x.harness)
                        && !h.session_id.is_empty()
                    {
                        resumable.push((
                            s.node_id.clone(),
                            h.harness,
                            s.cwd.clone(),
                            h.session_id,
                            title_of(&s),
                        ));
                    }
                }
                Ok(HarnessCtx {
                    policy: core.harness_key_policy().await?,
                    keys: kls_core::vault::list_secrets(&core)
                        .await
                        .unwrap_or_default(),
                    resumable,
                    startup: core.harness_startup().await?,
                    mcp: kls_core::sessions::start::mcp_bindings(&core)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|b| b.binding)
                        .collect(),
                })
            },
        );
        let dispatches = Loader::new(model, &["dispatch."], |core| async move {
            Ok(Arc::new(core.store().dispatches(None, 50).await?))
        });
        Self {
            list,
            detail,
            dispatches,
            selected,
            dispatched: Arc::default(),
            filter: String::new(),
            state_filter: StateFilter::Open,
            node_filter: String::new(),
            agent_filter: String::new(),
            composer: String::new(),
            composer_attach: Default::default(),
            answer: String::new(),
            clarify: Default::default(),
            control: None,
            control_args: String::new(),
            action: None,
            action_text: String::new(),
            relay_edit: (String::new(), String::new()),
            resume_prompt: String::new(),
            expanded: HashSet::new(),
            lines: Vec::new(),
            lines_key: Default::default(),
            rewound: HashSet::new(),
            expand_gen: 0,
            picked: None,
            confirm_stop: false,
            new: None,
            harness_ctx,
            tab: DetailTab::Transcript,
            screen_view: None,
            a2a: None,
            source: Source::All,
            by_crew: false,
            git_branch: String::new(),
            teleport_to: String::new(),
            show_context: true,
        }
    }

    /// Opens a session by id; `new` opens the New session dialog (tray).
    pub fn focus(&mut self, id: &str) {
        if id == NEW_FOCUS {
            self.new = Some(NewSession {
                mode: "agent".into(),
                ..Default::default()
            });
            return;
        }
        self.select(Some(id.to_string()));
    }

    fn select(&mut self, id: Option<String>) {
        let mut s = self.selected.lock().unwrap();
        if *s != id {
            *s = id;
            drop(s);
            self.composer.clear();
            self.composer_attach.clear();
            self.answer.clear();
            self.expanded.clear();
            self.picked = None;
            self.confirm_stop = false;
            self.detail.reload();
        }
    }

    fn selected_id(&self) -> Option<String> {
        self.selected.lock().unwrap().clone()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let data = match self.list.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        self.follow_dispatch();

        let new = &mut self.new;
        kit::page_header(ui, icons::TERMINAL_WINDOW, "Sessions", |ui| {
            if theme::primary_button(ui, &icons::label(icons::PLUS, "New session")).clicked() {
                *new = Some(NewSession {
                    mode: "agent".into(),
                    ..Default::default()
                });
            }
        });

        // The list would take every pixel it can; leave the transcript at
        // least half when a session is open.
        let max_list = if self.selected_id().is_some() {
            (ui.available_width() * 0.5).max(320.0)
        } else {
            ui.available_width()
        };
        egui::Panel::left("sessions_list")
            .frame(kit::card(ui).outer_margin(egui::Margin {
                left: 0,
                right: 16,
                top: 0,
                bottom: 0,
            }))
            .show_separator_line(false)
            .resizable(true)
            .default_size(420.0_f32.min(max_list))
            .min_size(320.0)
            .max_size(max_list)
            .show(ui, |ui| self.list_ui(ui, &data));

        egui::CentralPanel::default()
            .frame(kit::card(ui))
            .show(ui, |ui| match self.selected_id() {
                Some(id) => self.detail_ui(ui, model, &data, &id),
                None => {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        kit::empty(ui, icons::TERMINAL_WINDOW, "No session selected");
                    });
                }
            });

        self.new_dialog(ui.ctx(), model, &data);
        // "Updated" ages and the live window lapses without an event.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(5));
    }

    // ---- list ----------------------------------------------------------------

    fn list_ui(&mut self, ui: &mut egui::Ui, data: &ListData) {
        let now = now_ms();
        let p = palette(ui.ctx());
        let live = data
            .sessions
            .iter()
            .filter(|s| s.is_open() && now - s.seen_at < LIVE_WINDOW_MS)
            .count();
        let waiting = data
            .sessions
            .iter()
            .filter(|s| s.is_open() && !s.pending_question_id.is_empty())
            .count();
        egui::Frame::new()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for f in [
                        StateFilter::Open,
                        StateFilter::Waiting,
                        StateFilter::Ended,
                        StateFilter::All,
                    ] {
                        if kit::pill(ui, &f.label(), self.state_filter == f).clicked() {
                            self.state_filter = f;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for s in [
                        Source::All,
                        Source::Console,
                        Source::Run,
                        Source::Worker,
                        Source::Host,
                    ] {
                        if kit::tab(ui, &s.label(), self.source == s).clicked() {
                            self.source = s;
                        }
                    }
                    if kit::toggle_chip(
                        ui,
                        &icons::label(icons::USERS_THREE, "By crew"),
                        self.by_crew,
                    )
                    .clicked()
                    {
                        self.by_crew = !self.by_crew;
                    }
                });
                ui.add_space(8.0);
                kit::field_label(ui, "Filter sessions");
                ui.horizontal(|ui| {
                    let label = data
                        .nodes
                        .iter()
                        .find(|n| n.id == self.node_filter)
                        .map(|n| n.name.clone())
                        .unwrap_or_else(|| "All nodes".into());
                    let w = (ui.available_width() - 150.0).max(120.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(icons::label(
                                icons::MAGNIFYING_GLASS,
                                "title, directory or repo",
                            ))
                            .desired_width(w),
                    )
                    .on_hover_text("Title, directory, model, agent");
                    egui::ComboBox::from_id_salt("sessions_node_filter")
                        .selected_text(icons::label(icons::HARD_DRIVES, &label))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.node_filter, String::new(), "All nodes");
                            for n in data.nodes.iter() {
                                ui.selectable_value(&mut self.node_filter, n.id.clone(), &n.name);
                            }
                        });
                    let agents = agent_choices(&data.sessions);
                    if agents.len() > 1 {
                        egui::ComboBox::from_id_salt("sessions_agent_filter")
                            .selected_text(icons::label(
                                icons::ROBOT,
                                if self.agent_filter.is_empty() {
                                    "Agent"
                                } else {
                                    &self.agent_filter
                                },
                            ))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.agent_filter,
                                    String::new(),
                                    "All agents",
                                );
                                for a in &agents {
                                    ui.selectable_value(&mut self.agent_filter, a.clone(), a);
                                }
                            });
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    kit::dot(ui, if live > 0 { p.green } else { p.muted });
                    ui.label(
                        RichText::new(format!("{live} live"))
                            .size(12.0)
                            .color(p.muted),
                    );
                    ui.add_space(6.0);
                    kit::dot(ui, if waiting > 0 { p.gold_text } else { p.muted });
                    ui.label(
                        RichText::new(format!("{waiting} waiting"))
                            .size(12.0)
                            .color(p.muted),
                    );
                });
            });
        let r = ui.min_rect();
        ui.painter()
            .hline(r.x_range(), r.bottom(), Stroke::new(1.0, p.line));

        let needle = self.filter.trim().to_lowercase();
        let rows: Vec<&Session> = data
            .sessions
            .iter()
            .filter(|s| match self.state_filter {
                StateFilter::Open => s.is_open(),
                StateFilter::Waiting => s.is_open() && !s.pending_question_id.is_empty(),
                StateFilter::Ended => !s.is_open(),
                StateFilter::All => true,
            })
            .filter(|s| self.node_filter.is_empty() || s.node_id == self.node_filter)
            .filter(|s| self.source == Source::All || Source::of(s) == self.source)
            .filter(|s| self.agent_filter.is_empty() || agent_of(s) == self.agent_filter)
            .filter(|s| {
                needle.is_empty()
                    || [&s.title, &s.cwd, &s.repo, &s.model, &s.agent, &s.id]
                        .iter()
                        .any(|f| f.to_lowercase().contains(&needle))
            })
            .collect();
        if rows.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                if data.sessions.is_empty() {
                    kit::empty(ui, icons::TERMINAL_WINDOW, "No sessions");
                    if theme::primary_button(ui, &icons::label(icons::PLUS, "New session"))
                        .clicked()
                    {
                        self.new = Some(NewSession {
                            mode: "agent".into(),
                            ..Default::default()
                        });
                    }
                } else {
                    kit::empty(ui, icons::FUNNEL, "No matches");
                }
            });
            return;
        }
        let selected = self.selected_id();
        let mut clicked = None;
        let row_h = 58.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        if self.by_crew {
            let groups = group_by_crew(&rows, &data.crews);
            egui::ScrollArea::vertical()
                .id_salt("sessions_list_crews")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (crew, list) in &groups {
                        ui.add_space(6.0);
                        let head = if crew.is_empty() {
                            icons::label(icons::MINUS, "No crew")
                        } else {
                            icons::label(icons::USERS_THREE, crew)
                        };
                        ui.label(kit::caps(&format!("{head} · {}", list.len()), p.muted));
                        ui.add_space(4.0);
                        for s in list {
                            let on = selected.as_deref() == Some(s.id.as_str());
                            if session_row(ui, data, s, now, on, row_h).clicked() {
                                clicked = Some(s.id.clone());
                            }
                        }
                    }
                });
            if let Some(id) = clicked {
                self.select(Some(id));
            }
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("sessions_list_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for s in &rows[range] {
                    let on = selected.as_deref() == Some(s.id.as_str());
                    if session_row(ui, data, s, now, on, row_h).clicked() {
                        clicked = Some(s.id.clone());
                    }
                }
            });
        if let Some(id) = clicked {
            self.select(Some(id));
        }
    }

    // ---- detail --------------------------------------------------------------

    fn detail_ui(&mut self, ui: &mut egui::Ui, model: &Model, data: &ListData, id: &str) {
        let detail = match self.detail.get() {
            Some(Ok(d)) if d.id == id => d,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            _ => {
                ui.spinner();
                return;
            }
        };
        let Some(s) = detail.session.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(icons::label(icons::CLOCK, short(id)))
                    .on_hover_text("Waiting for the node to report");
            });
            if ui.button(icons::label(icons::X, "Close")).clicked() {
                self.select(None);
            }
            return;
        };
        let now = now_ms();
        let open = s.is_open();

        let p = palette(ui.ctx());
        egui::Panel::top("session_header")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (txt, color) = state_label(ui, &s, now);
                    kit::chip(ui, &txt, color);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(icons::X).on_hover_text("Close").clicked() {
                            self.select(None);
                        }
                        if ui.button(icons::COPY).on_hover_text("Copy id").clicked() {
                            ui.ctx().copy_text(s.id.clone());
                        }
                        if open {
                            if self.confirm_stop {
                                if kit::danger_button(ui, &icons::label(icons::STOP, "Confirm"))
                                    .on_hover_text("Really stop the session")
                                    .clicked()
                                {
                                    self.command(model, &s.id, "stop");
                                    self.confirm_stop = false;
                                }
                                if kit::ghost_button(ui, &icons::label(icons::X, "Cancel"))
                                    .clicked()
                                {
                                    self.confirm_stop = false;
                                }
                            } else if kit::danger_button(ui, &icons::label(icons::STOP, "Stop"))
                                .on_hover_text("Ask the session to end")
                                .clicked()
                            {
                                self.confirm_stop = true;
                            }
                            if kit::ghost_button(ui, &icons::label(icons::HAND_PALM, "Interrupt"))
                                .on_hover_text("Interrupt the current turn")
                                .clicked()
                            {
                                self.command(model, &s.id, "interrupt");
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                let title = title_of(&s);
                ui.add(egui::Label::new(RichText::new(&title).size(18.0).strong()).truncate())
                    .on_hover_text(&title);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let node = node_name(data, &s.node_id);
                    let usage = usage(&s);
                    let entries = s.entry_count.to_string();
                    let git = detail
                        .extras
                        .as_ref()
                        .and_then(|x| x.git.as_ref())
                        .map(|g| {
                            let mut t: String = g.commit.chars().take(8).collect();
                            if g.dirty {
                                t.push_str(" *");
                            }
                            if g.ahead > 0 || g.behind > 0 {
                                t.push_str(&format!(" +{} -{}", g.ahead, g.behind));
                            }
                            t
                        })
                        .unwrap_or_default();
                    for (icon, hover, v) in [
                        (icons::HARD_DRIVES, "Node", &node),
                        (icons::FOLDER, "Directory", &s.cwd),
                        (icons::GIT_BRANCH, "Repository", &s.repo),
                        (icons::GIT_COMMIT, "Branch", &s.branch),
                        (icons::PLUGS_CONNECTED, "Provider", &s.provider),
                        (icons::CPU, "Model", &s.model),
                        (icons::SLIDERS, "Mode", &s.mode),
                        (icons::GAUGE, "Effort", &detail.effort),
                        (icons::FLAG, "Outcome", &detail.outcome),
                        (icons::ROBOT, "Agent", &s.agent),
                        (icons::TAG, "Kind", &s.kind),
                        (icons::GIT_COMMIT, "Commit", &git),
                        (icons::TREE_STRUCTURE, "Asked by node", &detail.origin),
                        (icons::TAG, "Label", &detail.label),
                        (
                            icons::SEAL_CHECK,
                            "Delivery · agent · verified",
                            &detail.axes,
                        ),
                        (icons::COINS, "Usage", &usage),
                        (icons::LIST, "Entries", &entries),
                    ] {
                        if !v.is_empty() {
                            ui.label(
                                RichText::new(icons::label(icon, v))
                                    .monospace()
                                    .size(12.0)
                                    .color(p.muted),
                            )
                            .on_hover_text(hover);
                        }
                    }
                    if let Some(e) = s.ended_at {
                        let r = ui.label(
                            RichText::new(icons::label(icons::STOP, &fmt::when(e)))
                                .monospace()
                                .size(12.0)
                                .color(p.muted),
                        );
                        if s.end_reason.is_empty() {
                            r.on_hover_text("Ended");
                        } else {
                            r.on_hover_text(format!("Ended: {}", s.end_reason));
                        }
                        // A harness session's end facts (OP-2).
                        if let Some(end) = detail
                            .extras
                            .as_ref()
                            .and_then(|x| x.harness.as_ref())
                            .and_then(|h| h.end.as_ref())
                        {
                            let (icon, text, hover) = if end.termination_verified {
                                (icons::SEAL_CHECK, "Stopped", "Termination verified")
                            } else {
                                (icons::SEAL_WARNING, "Unverified", "Termination unverified")
                            };
                            let mut chips = vec![(icon, text.to_string(), hover, p.muted)];
                            if end.empty_result {
                                chips.push((
                                    icons::WARNING,
                                    "Empty".into(),
                                    "No reply and no change in the work tree",
                                    p.amber,
                                ));
                            }
                            if end.adapter_error {
                                chips.push((
                                    icons::WARNING_CIRCLE,
                                    "Adapter".into(),
                                    "No final assistant turn after the last prompt",
                                    p.red,
                                ));
                            }
                            if end.wall_ms > 0 {
                                chips.push((
                                    icons::TIMER,
                                    format!("{}s", end.wall_ms / 1000),
                                    "Startup, wall time",
                                    p.muted,
                                ));
                            }
                            for (icon, text, hover, color) in chips {
                                let hover = if icon == icons::TIMER {
                                    format!(
                                        "Startup {} ms · wall {} ms",
                                        end.startup_ms, end.wall_ms
                                    )
                                } else {
                                    hover.to_string()
                                };
                                ui.label(
                                    RichText::new(icons::label(icon, &text))
                                        .monospace()
                                        .size(12.0)
                                        .color(color),
                                )
                                .on_hover_text(hover);
                            }
                        }
                    }
                });
                if !s.card_id.is_empty() || !s.run_id.is_empty() {
                    ui.horizontal(|ui| {
                        if !s.card_id.is_empty()
                            && ui
                                .link(icons::label(icons::KANBAN, short(&s.card_id)))
                                .on_hover_text("Card")
                                .clicked()
                        {
                            model.go(crate::Screen::Board, Some(s.card_id.clone()));
                        }
                        if !s.run_id.is_empty()
                            && ui
                                .link(icons::label(icons::PLAY_CIRCLE, short(&s.run_id)))
                                .on_hover_text("Run")
                                .clicked()
                        {
                            model.go(crate::Screen::Runs, Some(s.run_id.clone()));
                        }
                    });
                }
                if open {
                    self.git_strip_ui(ui, model, data, &s, detail.extras.as_ref());
                }
                if !detail.context.is_empty() {
                    let label = icons::label(icons::TREE_STRUCTURE, "Factory");
                    if kit::toggle_chip(ui, &label, self.show_context).clicked() {
                        self.show_context = !self.show_context;
                    }
                    if self.show_context {
                        context_ui(ui, model, &detail.context);
                    }
                }
                ui.add_space(4.0);
            });

        egui::Panel::bottom("session_composer")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)))
            .resizable(false)
            .show(ui, |ui| {
                self.inbox_ui(ui, &detail.inbox);
                let accepts = detail.extras.as_ref().and_then(|x| x.accepts.clone());
                match detail.extras.as_ref().and_then(|x| x.pending_ask.clone()) {
                    Some(ask) if open && !ask.kind.is_empty() => {
                        self.ask_ui(ui, model, &s, &ask, accepts.as_ref());
                        ui.add_space(10.0);
                    }
                    _ if open && !s.pending_question_id.is_empty() => {
                        self.answer_ui(ui, model, &s);
                        ui.add_space(10.0);
                    }
                    _ => {}
                }
                let is_harness = kls_core::harness::session_harness(&s).is_some();
                if open && is_harness && accepts.as_ref().is_some_and(|a| a.controls) {
                    let allow = super::harness_screen::allowed_keys(&data.nodes, &s);
                    super::harness_screen::keys_ui(ui, model, &s.id, &allow);
                    ui.add_space(6.0);
                } else if open && accepts.as_ref().is_some_and(|a| a.controls) {
                    self.controls_ui(ui, model, &s.id, accepts.as_ref().unwrap());
                    let cps = detail
                        .extras
                        .as_ref()
                        .map(|x| x.checkpoints.clone())
                        .unwrap_or_default();
                    self.actions_ui(
                        ui,
                        model,
                        &s.id,
                        accepts.as_ref().unwrap(),
                        &cps,
                        &detail.entries,
                    );
                    ui.add_space(6.0);
                }
                if open {
                    self.composer_ui(ui, model, &s.id, accepts.as_ref());
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::label(icons::STOP, "Ended")).color(p.muted));
                        super::harness_screen::resume_ui(
                            ui,
                            model,
                            &data.nodes,
                            &s,
                            detail.extras.as_ref().and_then(|x| x.harness.as_ref()),
                        );
                        self.resume_ui(ui, model, &detail, &s);
                    });
                }
            });

        if let Some(idx) = self.picked {
            match detail.entries.iter().find(|e| e.idx == idx) {
                Some(e) => {
                    let e = e.clone();
                    egui::Panel::bottom("session_entry")
                        .frame(
                            egui::Frame::new()
                                .fill(p.bg)
                                .inner_margin(egui::Margin::symmetric(16, 10)),
                        )
                        .resizable(true)
                        .default_size(180.0)
                        .show(ui, |ui| self.entry_ui(ui, &e));
                }
                None => self.picked = None,
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 8)))
            .show(ui, |ui| {
                let harness = kls_core::harness::session_harness(&s).map(str::to_string);
                ui.horizontal(|ui| {
                    for (tab, icon, word) in [
                        (DetailTab::Transcript, icons::LIST, "Transcript"),
                        (DetailTab::Screen, icons::TERMINAL_WINDOW, "Screen"),
                        (DetailTab::A2a, icons::CHAT_CIRCLE_DOTS, "A2A"),
                    ] {
                        if tab == DetailTab::Screen && harness.is_none() {
                            continue;
                        }
                        if kit::pill(ui, &icons::label(icon, word), self.tab == tab).clicked() {
                            self.tab = tab;
                        }
                    }
                    if let Some(h) = &harness {
                        ui.add_space(8.0);
                        harness_rules_chip(ui, h);
                    }
                });
                ui.add_space(4.0);
                match self.tab {
                    DetailTab::Screen if harness.is_some() => self.screen_tab(ui, model, data, &s),
                    DetailTab::A2a => {
                        let agent = format!("{}:{}", s.node_id, s.id);
                        if self.a2a.as_ref().is_none_or(|p| p.agent_id() != agent) {
                            self.a2a = Some(super::harness_a2a::A2aPanel::new(model, &agent));
                        }
                        if let Some(p) = self.a2a.as_mut() {
                            p.ui(ui, model);
                        }
                    }
                    _ => self.transcript_ui(ui, &detail),
                }
            });
    }

    /// A harness session's live terminal (output only) and its allowed keys.
    fn screen_tab(&mut self, ui: &mut egui::Ui, model: &Model, data: &ListData, s: &Session) {
        if self
            .screen_view
            .as_ref()
            .is_none_or(|(id, _, _)| id != &s.id)
        {
            self.screen_view = model.hub.screen(&s.id).map(|a| {
                let (cols, rows) = a.size.unwrap_or((120, 40));
                let mut v = super::harness_screen::ScreenView::default();
                v.open(rows as u16, cols as u16, &a.replay, a.offset);
                (s.id.clone(), v, a)
            });
        }
        let Some((_, view, att)) = self.screen_view.as_mut() else {
            ui.weak(icons::label(icons::TERMINAL_WINDOW, "No screen"));
            return;
        };
        while let Ok(b) = att.rx.try_recv() {
            let at = view.offset;
            view.feed(&b, at);
        }
        view.ui(ui);
        let allow = data
            .nodes
            .iter()
            .find(|n| n.id == s.node_id)
            .and_then(|n| {
                let h = kls_core::harness::session_harness(s)?;
                n.host
                    .harnesses
                    .iter()
                    .find(|x| x.id == h)
                    .map(|x| x.allow_keys.clone())
            })
            .unwrap_or_default();
        if s.is_open() {
            super::harness_screen::keys_ui(ui, model, &s.id, &allow);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn command(&self, model: &Model, session_id: &str, name: &'static str) {
        let id = session_id.to_string();
        model.act(move |m| async move {
            m.core().queue_command(&id, name, &[]).await?;
            Ok(Some(Notice::Info(format!("Sent {name} to the session"))))
        });
    }

    fn inbox_ui(&self, ui: &mut egui::Ui, inbox: &[InboxItem]) {
        let open: Vec<&InboxItem> = inbox
            .iter()
            .filter(|i| i.status != "done")
            .rev()
            .take(5)
            .collect();
        for i in open.into_iter().rev() {
            let what = match InboxBody::of(i) {
                Ok(InboxBody::Prompt { text, attachments }) if !attachments.is_empty() => {
                    icons::label(
                        icons::PAPER_PLANE_TILT,
                        &format!("{} · {} attached", one_line(&text, 70), attachments.len()),
                    )
                }
                Ok(InboxBody::Prompt { text, .. }) => {
                    icons::label(icons::PAPER_PLANE_TILT, &one_line(&text, 80))
                }
                Ok(InboxBody::Answer { text, .. }) => {
                    icons::label(icons::CHAT_CIRCLE_DOTS, &one_line(&text, 80))
                }
                Ok(InboxBody::Command { name, args }) => {
                    format!("/{name} {}", args.join(" ")).trim().to_string()
                }
                Err(_) => i.kind.clone(),
            };
            let p = palette(ui.ctx());
            let (icon, status) = match i.status.as_str() {
                "pending" => (icons::CLOCK, "queued"),
                "delivered" => (icons::CHECK, "delivered"),
                "refused" | "failed" => (icons::X, i.status.as_str()),
                s => (icons::CIRCLE, s),
            };
            let color = match status {
                "delivered" => p.green,
                s => kit::status_color(&p, s),
            };
            ui.horizontal(|ui| {
                kit::chip(ui, &icons::label(icon, status), color);
                let r = ui.label(RichText::new(what).size(13.0));
                if !i.detail.is_empty() {
                    r.on_hover_text(&i.detail);
                }
            });
        }
    }

    fn answer_ui(&mut self, ui: &mut egui::Ui, model: &Model, s: &Session) {
        let p = palette(ui.ctx());
        kit::glow(ui)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(kit::caps(
                    &icons::label(icons::QUESTION, "Question"),
                    p.gold_text,
                ));
                ui.label(RichText::new(&s.pending_question).size(14.0).strong());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.answer)
                            .hint_text("Answer")
                            .desired_width(ui.available_width() - 110.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (theme::primary_button(ui, &icons::label(icons::CHAT_CIRCLE_DOTS, "Answer"))
                        .clicked()
                        || enter)
                        && !self.answer.trim().is_empty()
                    {
                        let (sid, qid, text) = (
                            s.id.clone(),
                            s.pending_question_id.clone(),
                            self.answer.trim().to_string(),
                        );
                        model.act(move |m| async move {
                            m.core().queue_answer(&sid, &qid, &text).await?;
                            Ok(None)
                        });
                        self.answer.clear();
                    }
                });
            });
    }

    /// A structured ask: one button per option the host listed, or a form
    /// built from a clarify questionnaire. Every string is the host's or a
    /// model's text and is shown as plain text only.
    fn ask_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        s: &Session,
        ask: &PendingAsk,
        accepts: Option<&Accepts>,
    ) {
        let p = palette(ui.ctx());
        let takes = accepts.is_none_or(|a| a.answers);
        kit::glow(ui)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::QUESTION, &ask.kind.replace('_', " ")),
                        p.gold_text,
                    ));
                    if !ask.tool.is_empty() {
                        kit::chip(ui, &icons::label(icons::WRENCH, &ask.tool), p.muted);
                    }
                    if !takes {
                        kit::chip(ui, &icons::label(icons::LOCK_SIMPLE, "Host only"), p.muted)
                            .on_hover_text("The host does not take remote answers");
                    }
                });
                ui.add(egui::Label::new(RichText::new(&ask.title).size(14.0).strong()).wrap());
                if !takes {
                    return;
                }
                ui.add_space(4.0);
                let send = |decision: String, payload: String, note: String| {
                    let (sid, aid) = (s.id.clone(), ask.id.clone());
                    model.act(move |m| async move {
                        m.core()
                            .queue_ask_answer(&sid, &aid, &decision, &payload, &note)
                            .await?;
                        Ok(None)
                    });
                };
                match ask.kind.as_str() {
                    "clarify" | "mode_choice" => self.clarify_ui(ui, ask, send),
                    "relay_review" => self.relay_ui(ui, ask, send),
                    _ => {
                        if ask.kind == "plan_review" || ask.kind == "question" {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.answer)
                                    .hint_text(if ask.kind == "question" {
                                        "Answer"
                                    } else {
                                        "Notes"
                                    })
                                    .desired_width(ui.available_width()),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            for o in &ask.options {
                                let deny = o.id == "deny" || o.id == "stay";
                                let label = icons::label(
                                    if deny { icons::X } else { icons::CHECK },
                                    &o.label,
                                );
                                let r = if deny {
                                    kit::ghost_button(ui, &label)
                                } else {
                                    theme::primary_button(ui, &label)
                                };
                                if r.clicked() {
                                    send(o.id.clone(), String::new(), self.answer.trim().into());
                                    self.answer.clear();
                                }
                            }
                        });
                    }
                }
            });
    }

    fn clarify_ui(
        &mut self,
        ui: &mut egui::Ui,
        ask: &PendingAsk,
        send: impl Fn(String, String, String),
    ) {
        struct Opt {
            label: String,
            description: String,
        }
        struct Group {
            context: String,
            multi: bool,
            options: Vec<Opt>,
        }
        let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let parsed = serde_json::from_str::<serde_json::Value>(&ask.questions_json)
            .ok()
            .and_then(|v| v["groups"].as_array().cloned())
            .map(|gs| Q {
                groups: gs
                    .iter()
                    .map(|g| Group {
                        context: s(g, "context"),
                        multi: g["multi"].as_bool().unwrap_or(false),
                        options: g["options"]
                            .as_array()
                            .map(|os| {
                                os.iter()
                                    .map(|o| Opt {
                                        label: s(o, "label"),
                                        description: s(o, "description"),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect(),
            });
        struct Q {
            groups: Vec<Group>,
        }
        let Some(q) = parsed else {
            ui.label(icons::label(icons::WARNING, "Unreadable questionnaire"));
            return;
        };
        if self.clarify.0 != ask.id || self.clarify.1.len() != q.groups.len() {
            self.clarify = (
                ask.id.clone(),
                q.groups
                    .iter()
                    .map(|g| vec![false; g.options.len()])
                    .collect(),
            );
        }
        for (gi, g) in q.groups.iter().enumerate() {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&g.context).strong()).wrap());
            for (oi, o) in g.options.iter().enumerate() {
                let mut on = self.clarify.1[gi][oi];
                let r = if g.multi {
                    ui.checkbox(&mut on, &o.label)
                } else {
                    ui.radio(on, &o.label)
                };
                let r = if o.description.is_empty() {
                    r
                } else {
                    r.on_hover_text(&o.description)
                };
                if r.clicked() {
                    if !g.multi {
                        self.clarify.1[gi].iter_mut().for_each(|c| *c = false);
                        on = true;
                    }
                    self.clarify.1[gi][oi] = on;
                }
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, &icons::label(icons::PAPER_PLANE_TILT, "Answer")).clicked()
            {
                let answers: Vec<serde_json::Value> = self
                    .clarify
                    .1
                    .iter()
                    .enumerate()
                    .map(|(gi, picked)| {
                        let chosen: Vec<usize> = picked
                            .iter()
                            .enumerate()
                            .filter(|(_, on)| **on)
                            .map(|(i, _)| i)
                            .collect();
                        if chosen.is_empty() {
                            serde_json::json!({ "group": gi, "skipped": true })
                        } else {
                            serde_json::json!({ "group": gi, "chosen": chosen })
                        }
                    })
                    .collect();
                send(
                    String::new(),
                    serde_json::json!({ "answers": answers }).to_string(),
                    String::new(),
                );
            }
            if kit::ghost_button(ui, &icons::label(icons::X, "Decline")).clicked() {
                send(
                    String::new(),
                    serde_json::json!({ "decline": true }).to_string(),
                    String::new(),
                );
            }
        });
    }

    /// Session controls the host listed (`--web-controls`), with arguments.
    fn controls_ui(&mut self, ui: &mut egui::Ui, model: &Model, session_id: &str, a: &Accepts) {
        let names: Vec<&String> = a
            .controls_allowed
            .iter()
            .filter(|c| !matches!(c.as_str(), "stop" | "interrupt"))
            .filter(|c| a.shell || c.as_str() != "shell")
            .filter(|c| !kls_core::sessions::actions::is_action(c))
            .collect();
        if names.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            let current = self.control.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("session_control")
                .selected_text(icons::label(
                    icons::SLIDERS,
                    if current.is_empty() {
                        "Control"
                    } else {
                        &current
                    },
                ))
                .show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(current == **n, n.as_str()).clicked() {
                            self.control = Some((*n).clone());
                        }
                    }
                });
            if self.control.is_some() {
                ui.add(
                    egui::TextEdit::singleline(&mut self.control_args)
                        .hint_text("Arguments")
                        .desired_width((ui.available_width() - 90.0).max(80.0)),
                );
                if theme::primary_button(ui, &icons::label(icons::PAPER_PLANE_TILT, "Send"))
                    .clicked()
                {
                    let name = self.control.take().unwrap_or_default();
                    let args: Vec<String> = self
                        .control_args
                        .split_whitespace()
                        .map(String::from)
                        .collect();
                    self.control_args.clear();
                    let sid = session_id.to_string();
                    model.act(move |m| async move {
                        m.core().queue_command(&sid, &name, &args).await?;
                        Ok(Some(Notice::Info(format!("Sent {name}"))))
                    });
                }
            }
        });
    }

    /// The console session actions the session lists (#45): one button
    /// each; text actions open a field, rewind a checkpoint picker. Core
    /// checks the kill switch, the list and the arguments, and audits.
    fn actions_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        session_id: &str,
        a: &Accepts,
        checkpoints: &[kls_core::belai::Checkpoint],
        entries: &[Entry],
    ) {
        let listed: Vec<&str> = action_buttons()
            .iter()
            .map(|(n, _, _)| *n)
            .filter(|n| {
                a.controls_allowed
                    .iter()
                    .any(|c| c.trim_start_matches('/') == *n)
            })
            .collect();
        if listed.is_empty() {
            return;
        }
        let send = |name: String, args: Vec<String>| {
            let sid = session_id.to_string();
            model.act(move |m| async move {
                m.core().queue_action(&sid, &name, &args, "owner").await?;
                Ok(Some(Notice::Info(format!("Sent /{name}"))))
            });
        };
        ui.horizontal_wrapped(|ui| {
            for (name, icon, word) in action_buttons() {
                if !listed.contains(&name) {
                    continue;
                }
                match name {
                    "undo" => {
                        if kit::ghost_button(ui, &icons::label(icon, word)).clicked() {
                            send(name.into(), Vec::new());
                        }
                    }
                    "rewind" => {
                        ui.add_enabled_ui(!checkpoints.is_empty(), |ui| {
                            ui.menu_button(icons::label(icon, word), |ui| {
                                for c in checkpoints {
                                    let label = format!(
                                        "{} · {} · {} files",
                                        time_of(c.at_ms),
                                        one_line(&c.label, 60),
                                        c.files
                                    );
                                    if ui.button(label).clicked() {
                                        send(name.into(), vec![c.id.clone()]);
                                        // The rewound prompt goes back for
                                        // editing, as in belai's TUI.
                                        if self.composer.trim().is_empty()
                                            && let Some(p) =
                                                kls_core::sessions::actions::rewind_prompt(
                                                    entries, c,
                                                )
                                        {
                                            self.composer = p;
                                        }
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_disabled_hover_text("No checkpoints");
                        });
                    }
                    _ => {
                        let on = self.action.as_deref() == Some(name);
                        if kit::pill(ui, &icons::label(icon, word), on).clicked() {
                            self.action = if on { None } else { Some(name.into()) };
                            self.action_text.clear();
                        }
                    }
                }
            }
        });
        let Some(name) = self.action.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            let hint = match name.as_str() {
                "compact" => "Focus (optional)",
                "relay" => "Direction (optional)",
                "commit" => "Message (optional)",
                "btw" => "Question",
                _ => "Title",
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.action_text)
                    .hint_text(hint)
                    .desired_width((ui.available_width() - 90.0).max(80.0)),
            );
            let needs_text = matches!(name.as_str(), "btw" | "rename");
            let ready = !needs_text || !self.action_text.trim().is_empty();
            if ui
                .add_enabled(
                    ready,
                    egui::Button::new(icons::label(icons::PAPER_PLANE_TILT, "Send")),
                )
                .clicked()
            {
                let text = self.action_text.trim().to_string();
                let args = if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text]
                };
                send(name.clone(), args);
                self.action = None;
                self.action_text.clear();
            }
        });
    }

    /// Resume or fork an ended belai session, and run its approved plan
    /// (#46, #52). Fork continues from the entry picked in the transcript.
    fn resume_ui(&mut self, ui: &mut egui::Ui, model: &Model, d: &Detail, s: &Session) {
        if kls_core::harness::session_harness(s).is_some() {
            return;
        }
        let fork_entry = self
            .picked
            .and_then(|idx| d.entries.iter().find(|e| e.idx == idx))
            .and_then(entry_belai_id);
        ui.add(
            egui::TextEdit::singleline(&mut self.resume_prompt)
                .hint_text(kls_core::sessions::resume::DEFAULT_PROMPT)
                .desired_width(220.0),
        );
        let go = |fork: String, prompt: String| {
            let sid = s.id.clone();
            model.act(move |m| async move {
                let id = kls_core::sessions::resume::resume(
                    m.core(),
                    kls_core::sessions::resume::Resume {
                        session_id: sid,
                        fork_entry: fork,
                        prompt,
                        ..Default::default()
                    },
                    &resume_origin(),
                )
                .await?;
                Ok(Some(Notice::Info(format!("Sent dispatch {}", short(&id)))))
            });
        };
        if theme::primary_button(ui, &icons::label(icons::PLAY, "Resume")).clicked() {
            go(String::new(), self.resume_prompt.trim().into());
            self.resume_prompt.clear();
        }
        let fork = ui
            .add_enabled(
                fork_entry.is_some(),
                egui::Button::new(icons::label(icons::GIT_FORK, "Fork")),
            )
            .on_disabled_hover_text("Pick an entry");
        if fork.clicked()
            && let Some(e) = fork_entry
        {
            go(e, self.resume_prompt.trim().into());
            self.resume_prompt.clear();
        }
        let plan = d
            .extras
            .as_ref()
            .map(|x| x.plan_sha256.clone())
            .unwrap_or_default();
        if !plan.is_empty() {
            if let Some(e) = d.entries.iter().rev().find(|e| is_plan_file(e))
                && kit::ghost_button(ui, &icons::label(icons::FILE_TEXT, "Plan")).clicked()
            {
                self.picked = Some(e.idx);
            }
            if theme::primary_button(ui, &icons::label(icons::ROCKET, "Execute plan"))
                .on_hover_text(format!("plan {}", short(&plan)))
                .clicked()
            {
                let sid = s.id.clone();
                model.act(move |m| async move {
                    let id =
                        kls_core::sessions::resume::execute_plan(m.core(), &sid, &resume_origin())
                            .await?;
                    Ok(Some(Notice::Info(format!("Sent dispatch {}", short(&id)))))
                });
            }
        }
    }

    /// A relay review (#45): the drafted handoff, approved, edited or
    /// dropped. The draft is a model's text, shown as plain text.
    fn relay_ui(
        &mut self,
        ui: &mut egui::Ui,
        ask: &PendingAsk,
        send: impl Fn(String, String, String),
    ) {
        let draft = relay_text(&ask.relay_json);
        if self.relay_edit.0 != ask.id {
            self.relay_edit = (ask.id.clone(), draft.clone());
        }
        egui::ScrollArea::vertical()
            .id_salt("relay_draft")
            .max_height(220.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.relay_edit.1)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6),
                );
            });
        let edited = self.relay_edit.1.trim() != draft.trim();
        ui.horizontal(|ui| {
            let has = |id: &str| ask.options.iter().any(|o| o.id == id);
            if has("approve")
                && !edited
                && theme::primary_button(ui, &icons::label(icons::CHECK, "Approve")).clicked()
            {
                send("approve".into(), String::new(), String::new());
            }
            if has("edit")
                && edited
                && !self.relay_edit.1.trim().is_empty()
                && theme::primary_button(ui, &icons::label(icons::PENCIL_SIMPLE, "Send edit"))
                    .clicked()
            {
                send("edit".into(), String::new(), self.relay_edit.1.clone());
            }
            if has("drop") && kit::ghost_button(ui, &icons::label(icons::X, "Drop")).clicked() {
                send("drop".into(), String::new(), String::new());
            }
        });
    }

    /// Branch switch, sync toggle and teleport, when the session lists them
    /// (teleport: any other online node). Core checks the kill switch and
    /// audits each.
    fn git_strip_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        data: &ListData,
        s: &Session,
        extras: Option<&SessionExtras>,
    ) {
        use kls_core::sessions::GitAction;
        let listed = |name: &str| {
            extras.and_then(|x| x.accepts.as_ref()).is_some_and(|a| {
                a.controls_allowed
                    .iter()
                    .chain(&a.commands)
                    .any(|c| c.trim_start_matches('/') == name)
            })
        };
        let peers: Vec<&Node> = data
            .nodes
            .iter()
            .filter(|n| n.id != s.node_id && data.online.contains(&n.id) && n.revoked_at.is_none())
            .collect();
        let (branch, sync) = (listed("branch"), listed("sync"));
        if !branch && !sync && peers.is_empty() {
            return;
        }
        let act = |a: GitAction| {
            let sid = s.id.clone();
            model.act(move |m| async move {
                m.core().session_git(&sid, a, "owner").await?;
                Ok(Some(Notice::Info("Sent".into())))
            });
        };
        ui.horizontal_wrapped(|ui| {
            if branch {
                ui.add(
                    egui::TextEdit::singleline(&mut self.git_branch)
                        .hint_text(icons::label(icons::GIT_BRANCH, "branch"))
                        .desired_width(140.0),
                );
                if kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Switch")).clicked()
                    && !self.git_branch.trim().is_empty()
                {
                    act(GitAction::Branch(std::mem::take(&mut self.git_branch)));
                }
            }
            if sync {
                if kit::ghost_button(ui, &icons::label(icons::UPLOAD_SIMPLE, "Sync on")).clicked() {
                    act(GitAction::Sync(true));
                }
                if kit::ghost_button(ui, &icons::label(icons::PAUSE, "Sync off")).clicked() {
                    act(GitAction::Sync(false));
                }
            }
            if !peers.is_empty() {
                let to = peers
                    .iter()
                    .find(|n| n.id == self.teleport_to)
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| "node".into());
                egui::ComboBox::from_id_salt("teleport_to")
                    .selected_text(icons::label(icons::HARD_DRIVES, &to))
                    .show_ui(ui, |ui| {
                        for n in &peers {
                            ui.selectable_value(&mut self.teleport_to, n.id.clone(), &n.name);
                        }
                    });
                if ui
                    .add_enabled(
                        peers.iter().any(|n| n.id == self.teleport_to),
                        egui::Button::new(icons::label(icons::ROCKET, "Teleport")),
                    )
                    .clicked()
                {
                    act(GitAction::Teleport(self.teleport_to.clone()));
                }
            }
        });
    }

    /// The prompt box: `/name args` sends a session command (completed from
    /// what the session lists), `!cmd` a shell line when the session takes
    /// shell, anything else a prompt. Core refuses all three under the kill
    /// switch.
    fn composer_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        session_id: &str,
        accepts: Option<&Accepts>,
    ) {
        use kls_core::sessions::{Composed, composer_completions, parse_composer};
        let p = palette(ui.ctx());
        let mode = parse_composer(&self.composer);
        let head = match &mode {
            Composed::Shell(_) => icons::label(icons::TERMINAL, "Shell"),
            Composed::Command(..) => icons::label(icons::SLIDERS, "Command"),
            Composed::Prompt => "Prompt".to_string(),
        };
        kit::field_label(ui, &head);
        if let Composed::Shell(_) = mode
            && !accepts.is_some_and(|a| a.shell)
        {
            ui.label(RichText::new(icons::label(icons::PROHIBIT, "No shell")).color(p.amber));
        }
        let typed = self.composer.trim_start();
        if typed.starts_with('/') && !typed.contains(char::is_whitespace) {
            let opts = composer_completions(accepts, typed);
            ui.horizontal_wrapped(|ui| {
                for o in opts.iter().take(12) {
                    if kit::pill(ui, &format!("/{o}"), false).clicked() {
                        self.composer = format!("/{o} ");
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            let r = ui
                .add(
                    egui::TextEdit::multiline(&mut self.composer)
                        .hint_text("Queued until idle · /command · !shell")
                        .desired_rows(2)
                        .desired_width(ui.available_width() - 100.0),
                )
                .on_hover_text("Ctrl+Enter to send");
            let send_key = r.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            if (theme::primary_button(ui, &icons::label(icons::PAPER_PLANE_TILT, "Send")).clicked()
                || send_key)
                && !self.composer.trim().is_empty()
            {
                let (sid, text) = (session_id.to_string(), self.composer.trim().to_string());
                // #53: a prompt with attachments takes the checked path
                // (caps, host feature, kill switch, audit with the hashes).
                let atts = match mode {
                    Composed::Prompt => self.composer_attach.list(),
                    _ => Vec::new(),
                };
                model.act(move |m| async move {
                    if atts.is_empty() {
                        m.core().queue_composer(&sid, &text, "owner").await?;
                    } else {
                        kls_core::attachments::queue_prompt(m.core(), &sid, &text, atts, "owner")
                            .await?;
                    }
                    Ok(None)
                });
                self.composer.clear();
                if matches!(mode, Composed::Prompt) {
                    self.composer_attach.clear();
                }
            }
        });
        if matches!(mode, Composed::Prompt) {
            self.composer_attach
                .ui(ui, model, &format!("sessions/composer/{session_id}"));
        }
    }

    fn entry_ui(&mut self, ui: &mut egui::Ui, e: &Entry) {
        let p = palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(kit::caps(
                &format!("#{} {}", e.idx, role_title(e)),
                role_color(ui, &e.role),
            ));
            if e.at > 0 {
                ui.label(
                    RichText::new(fmt::when(e.at))
                        .monospace()
                        .size(11.0)
                        .color(p.muted),
                );
            }
            if let Some(chip) = origin_chip(e) {
                let r = kit::chip(ui, &chip, p.gold);
                if let Some(h) = source_hover(e) {
                    r.on_hover_text(h);
                }
            } else if let Some(h) = source_hover(e) {
                ui.label(kit::mono(icons::INFO, 12.0, p.muted))
                    .on_hover_text(h);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(icons::X).on_hover_text("Close").clicked() {
                    self.picked = None;
                }
                if ui.button(icons::COPY).on_hover_text("Copy").clicked() {
                    ui.ctx().copy_text(e.text.clone());
                }
            });
        });
        egui::ScrollArea::vertical()
            .id_salt("session_entry_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&e.text).monospace()).wrap());
                if !e.meta_json.is_empty() {
                    ui.separator();
                    ui.weak(&e.meta_json);
                }
            });
    }

    fn transcript_ui(&mut self, ui: &mut egui::Ui, d: &Detail) {
        if d.entries.is_empty() {
            ui.weak(icons::label(icons::CHAT_TEXT, "No transcript"));
            return;
        }
        let key = (d.id.clone(), d.entries.len(), self.expand_gen);
        if key != self.lines_key {
            self.lines = build_lines(&d.entries, &self.expanded);
            self.rewound = kls_core::sessions::actions::rewound(&d.entries);
            self.lines_key = key;
        }
        // Fixed-height rows: the role in a mono column on the left, the text
        // beside it, a dashed rule between entries.
        let row_h = 22.0;
        let p = palette(ui.ctx());
        let mut toggle = None;
        let mut pick = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt(("session_transcript", d.id.as_str()))
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_h, self.lines.len(), |ui, range| {
                let first = range.start;
                for (k, line) in self.lines[range].iter().enumerate() {
                    let e = &d.entries[line.entry];
                    // A rewound entry stays in the transcript, muted and
                    // chipped, as belai keeps it in the file.
                    let gone = self.rewound.contains(&e.idx);
                    let color = if gone {
                        p.muted
                    } else {
                        role_color(ui, &e.role)
                    };
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
                    let painter = ui.painter_at(rect);
                    if resp.hovered() {
                        painter.rect_filled(rect, 0.0, p.panel2.gamma_multiply(0.5));
                    }
                    let text_x = rect.left() + ROLE_W;
                    let text_w = (rect.right() - text_x - 4.0).max(10.0);
                    let mid = rect.center().y;
                    match line.kind {
                        LineKind::Header => {
                            if first + k > 0 {
                                painter.add(egui::Shape::dashed_line(
                                    &[rect.left_top(), rect.right_top()],
                                    Stroke::new(1.0, p.line),
                                    4.0,
                                    3.0,
                                ));
                            }
                            let collapsible = is_tool(&e.role);
                            let open = self.expanded.contains(&e.idx);
                            text_fit(
                                &painter,
                                Pos2::new(rect.left(), mid),
                                Align2::LEFT_CENTER,
                                &role_label(e).to_uppercase(),
                                FontId::monospace(11.0),
                                color,
                                ROLE_W - 8.0,
                            );
                            let time_w = if e.at > 0 {
                                text_fit(
                                    &painter,
                                    Pos2::new(rect.right() - 4.0, mid),
                                    Align2::RIGHT_CENTER,
                                    &time_of(e.at),
                                    FontId::monospace(11.0),
                                    p.muted,
                                    120.0,
                                )
                                .width()
                                    + 12.0
                            } else {
                                0.0
                            };
                            let chip = if gone {
                                Some(icons::label(icons::ARROW_COUNTER_CLOCKWISE, "rewound"))
                            } else {
                                origin_chip(e)
                            };
                            let time_w = match chip {
                                Some(chip) => {
                                    text_fit(
                                        &painter,
                                        Pos2::new(rect.right() - 4.0 - time_w, mid),
                                        Align2::RIGHT_CENTER,
                                        &chip,
                                        FontId::monospace(11.0),
                                        p.gold,
                                        140.0,
                                    )
                                    .width()
                                        + time_w
                                        + 12.0
                                }
                                None => time_w,
                            };
                            if collapsible {
                                let mut text = String::from(if open {
                                    icons::CARET_DOWN
                                } else {
                                    icons::CARET_RIGHT
                                });
                                if !open {
                                    text.push(' ');
                                    text.push_str(&one_line(&e.text, 160));
                                }
                                text_fit(
                                    &painter,
                                    Pos2::new(text_x, mid),
                                    Align2::LEFT_CENTER,
                                    &text,
                                    FontId::monospace(12.5),
                                    if gone { p.muted } else { p.text },
                                    text_w - time_w,
                                );
                            }
                            let resp = match source_hover(e) {
                                Some(h) => resp.on_hover_text(h),
                                None => resp,
                            };
                            if resp.clicked() {
                                if collapsible {
                                    toggle = Some(e.idx);
                                }
                                pick = Some(e.idx);
                            }
                        }
                        LineKind::Body(n) => {
                            let text = e.text.lines().nth(n).unwrap_or("");
                            let (font, c) = match e.role.as_str() {
                                "tool" => (FontId::monospace(12.5), p.text),
                                "tool_result" => (FontId::monospace(12.5), p.muted),
                                _ => (FontId::proportional(14.0), p.text),
                            };
                            let c = if gone { p.muted } else { c };
                            text_fit(
                                &painter,
                                Pos2::new(text_x, mid),
                                Align2::LEFT_CENTER,
                                text,
                                font,
                                c,
                                text_w,
                            );
                            if resp.clicked() {
                                pick = Some(e.idx);
                            }
                        }
                        LineKind::More(n) => {
                            text_fit(
                                &painter,
                                Pos2::new(text_x, mid),
                                Align2::LEFT_CENTER,
                                &format!("{}  {n} more", icons::DOTS_THREE),
                                FontId::proportional(13.0),
                                p.muted,
                                text_w,
                            );
                            if resp.clicked() {
                                pick = Some(e.idx);
                            }
                        }
                    }
                }
            });
        if let Some(idx) = toggle {
            if !self.expanded.remove(&idx) {
                self.expanded.insert(idx);
            }
            self.expand_gen += 1;
        }
        if pick.is_some() {
            self.picked = pick;
        }
    }

    // ---- new session ---------------------------------------------------------

    /// Navigates to the session once the awaited dispatch reports it.
    fn follow_dispatch(&mut self) {
        if let Some(r) = self.dispatched.lock().unwrap().take()
            && let Some(n) = self.new.as_mut()
        {
            match r {
                Ok(id) => n.sent = Some(id),
                Err(e) => {
                    n.sent = None;
                    n.error = Some(e);
                }
            }
        }
        let Some(sent) = self.new.as_ref().and_then(|n| n.sent.clone()) else {
            return;
        };
        let Some(Ok(ds)) = self.dispatches.get() else {
            return;
        };
        if let Some(d) = ds.iter().find(|d| d.id == sent)
            && d.status == "started"
            && !d.session_id.is_empty()
        {
            self.new = None;
            self.state_filter = StateFilter::Open;
            self.select(Some(d.session_id.clone()));
        }
    }

    fn new_dialog(&mut self, ctx: &egui::Context, model: &Model, data: &ListData) {
        let hctx = self
            .harness_ctx
            .get()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        let Some(n) = self.new.as_mut() else {
            return;
        };
        let rc_nodes: Vec<&Node> = data
            .nodes
            .iter()
            .filter(|x| {
                x.revoked_at.is_none()
                    && x.host.capabilities.remote_control
                    && data.online.contains(&x.id)
            })
            .collect();
        if n.node_id.is_empty()
            && let Some(first) = rc_nodes.first()
        {
            n.node_id = first.id.clone();
        }
        let node = rc_nodes.iter().find(|x| x.id == n.node_id).copied();
        let dispatch = n.sent.as_ref().and_then(|id| match self.dispatches.get() {
            Some(Ok(ds)) => ds.iter().find(|d| &d.id == id).cloned(),
            _ => None,
        });
        let mut open = true;
        let mut send = false;
        egui::Window::new(icons::label(icons::TERMINAL_WINDOW, "New session"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(id) = &n.sent {
                    match &dispatch {
                        Some(d) if d.status == "failed" => {
                            ui.colored_label(
                                ui.visuals().error_fg_color,
                                icons::label(icons::X, &d.error),
                            );
                            if ui
                                .button(icons::label(icons::ARROW_CLOCKWISE, "Retry"))
                                .clicked()
                            {
                                n.sent = None;
                            }
                        }
                        Some(d) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(match d.status.as_str() {
                                    "queued" => icons::label(icons::CLOCK, "Queued"),
                                    "delivered" => icons::label(icons::CHECK, "Delivered"),
                                    _ => icons::label(icons::PLAY, "Starting"),
                                });
                            });
                        }
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(icons::label(icons::PAPER_PLANE_TILT, short(id)))
                                    .on_hover_text("Dispatch sent");
                            });
                        }
                    }
                    return;
                }
                if rc_nodes.is_empty() {
                    ui.weak(icons::label(icons::HARD_DRIVES, "No rc nodes online"))
                        .on_hover_text("Run `belai rc` on a joined host");
                    return;
                }
                egui::Grid::new("new_session_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Node");
                        egui::ComboBox::from_id_salt("new_session_node")
                            .selected_text(node.map(|x| x.name.as_str()).unwrap_or("choose"))
                            .show_ui(ui, |ui| {
                                for x in &rc_nodes {
                                    ui.selectable_value(&mut n.node_id, x.id.clone(), &x.name);
                                }
                            });
                        ui.end_row();

                        ui.label("Agent");
                        ui.vertical(|ui| {
                            let mut list =
                                node.map(|x| x.host.harnesses.clone()).unwrap_or_default();
                            kls_core::harness::apply_startup(&mut list, &n.node_id, &hctx.startup);
                            let ver = node
                                .map(|x| x.host.belai_version.clone())
                                .unwrap_or_default();
                            n.picker.agent_ui(ui, &ver, &list);
                            if n.picker.is_harness() {
                                let resumable: Vec<(String, String)> = hctx
                                    .resumable
                                    .iter()
                                    .filter(|r| {
                                        r.0 == n.node_id
                                            && Some(&r.1) == n.picker.harness.as_ref()
                                            && r.2 == n.cwd.trim()
                                    })
                                    .map(|r| (r.3.clone(), r.4.clone()))
                                    .collect();
                                n.picker.options_ui(
                                    ui,
                                    &list,
                                    &hctx.policy,
                                    &hctx.keys,
                                    &resumable,
                                );
                            }
                        });
                        ui.end_row();

                        ui.label("Directory");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut n.cwd)
                                    .hint_text("Trusted directory")
                                    .desired_width(300.0),
                            );
                            if let Some(x) = node
                                && !x.host.trusted_dirs.is_empty()
                            {
                                ui.menu_button(icons::CARET_DOWN, |ui| {
                                    for d in &x.host.trusted_dirs {
                                        if ui.button(d).clicked() {
                                            n.cwd = d.clone();
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        });
                        ui.end_row();

                        ui.label("Prompt");
                        ui.add(
                            egui::TextEdit::multiline(&mut n.prompt)
                                .hint_text("Task")
                                .desired_rows(4)
                                .desired_width(320.0),
                        );
                        ui.end_row();

                        if n.kind() == "session" && !n.picker.is_harness() {
                            ui.label("Attach");
                            ui.vertical(|ui| n.attach.ui(ui, model, "sessions/new"));
                            ui.end_row();
                        }

                        if !n.picker.is_harness() {
                            ui.label("Provider");
                            egui::ComboBox::from_id_salt("new_session_provider")
                                .selected_text(if n.provider.is_empty() {
                                    "host default"
                                } else {
                                    n.provider.as_str()
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut n.provider,
                                        String::new(),
                                        "host default",
                                    );
                                    if let Some(x) = node {
                                        for p in &x.host.capabilities.providers {
                                            ui.selectable_value(&mut n.provider, p.clone(), p);
                                        }
                                    }
                                });
                            ui.end_row();

                            ui.label("Model");
                            let offered: Vec<String> = node
                                .and_then(|x| {
                                    x.host
                                        .capabilities
                                        .models
                                        .iter()
                                        .find(|m| m.provider == n.provider)
                                })
                                .map(|m| m.models.clone())
                                .unwrap_or_default();
                            if offered.is_empty() {
                                ui.add(
                                    egui::TextEdit::singleline(&mut n.model)
                                        .hint_text("host default")
                                        .desired_width(200.0),
                                );
                            } else {
                                egui::ComboBox::from_id_salt("new_session_model")
                                    .selected_text(if n.model.is_empty() {
                                        "host default"
                                    } else {
                                        n.model.as_str()
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut n.model,
                                            String::new(),
                                            "host default",
                                        );
                                        for m in &offered {
                                            ui.selectable_value(&mut n.model, m.clone(), m);
                                        }
                                    });
                            }
                            ui.end_row();

                            ui.label("Agent");
                            match node.filter(|x| !x.host.agents.is_empty()) {
                                Some(x) => {
                                    egui::ComboBox::from_id_salt("new_session_agent")
                                        .selected_text(if n.agent.is_empty() {
                                            "none (plain belai)"
                                        } else {
                                            n.agent.as_str()
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut n.agent,
                                                String::new(),
                                                "none (plain belai)",
                                            );
                                            for a in &x.host.agents {
                                                ui.selectable_value(&mut n.agent, a.clone(), a);
                                            }
                                        });
                                }
                                None => {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut n.agent)
                                            .hint_text("none (plain belai)")
                                            .desired_width(200.0),
                                    );
                                }
                            }
                            ui.end_row();

                            ui.label("Crew");
                            match node.filter(|x| !x.host.crews.is_empty()) {
                                Some(x) => {
                                    egui::ComboBox::from_id_salt("new_session_crew")
                                        .selected_text(if n.crew.is_empty() {
                                            "none (one session)"
                                        } else {
                                            n.crew.as_str()
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut n.crew,
                                                String::new(),
                                                "none (one session)",
                                            );
                                            for c in &x.host.crews {
                                                ui.selectable_value(&mut n.crew, c.clone(), c);
                                            }
                                        });
                                }
                                None => {
                                    ui.weak("None");
                                }
                            }
                            ui.end_row();

                            ui.label("Mode");
                            ui.horizontal(|ui| {
                                for m in ["agent", "plan", "goal", "auto"] {
                                    ui.selectable_value(&mut n.mode, m.to_string(), m);
                                }
                            });
                            ui.end_row();
                        }
                    });
                if !n.picker.is_harness() {
                    ui.add_space(4.0);
                    options_ui(ui, n, node, &hctx.mcp);
                }
                ui.add_space(6.0);
                if let Some(e) = &n.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                let trusted = node.is_some_and(|x| {
                    x.host.trusted_dirs.is_empty()
                        || x.host.trusted_dirs.iter().any(|d| d == n.cwd.trim())
                });
                if !n.cwd.trim().is_empty() && !trusted {
                    ui.colored_label(
                        theme::palette(ui.ctx()).amber,
                        icons::label(icons::WARNING, "Untrusted directory"),
                    )
                    .on_hover_text("The node may refuse the dispatch");
                }
                let ready = node.is_some() && !n.cwd.trim().is_empty();
                if ui
                    .add_enabled_ui(ready, |ui| {
                        theme::primary_button(ui, &icons::label(icons::PLAY, "Start"))
                    })
                    .inner
                    .clicked()
                {
                    send = true;
                }
            });
        if send {
            let node_id = n.node_id.clone();
            let spec = n.spec();
            let harness = n.picker.request(&n.node_id, &n.cwd, &n.prompt);
            let fields = if harness.is_some() {
                StartFields::default()
            } else {
                match n.fields(&hctx.mcp) {
                    Ok(f) => f,
                    Err(e) => {
                        n.error = Some(e);
                        return;
                    }
                }
            };
            let advertised = rc_nodes
                .iter()
                .find(|x| x.id == n.node_id)
                .map(|x| x.host.harnesses.clone())
                .unwrap_or_default();
            let keys = hctx.keys.clone();
            n.sent = Some(String::new());
            n.error = None;
            let slot = self.dispatched.clone();
            model.act(move |m| async move {
                let r = match (harness, m.core().gateway()) {
                    // A harness: the audited plan, key mode and dispatch.
                    (Some(req), _) => {
                        m.core()
                            .dispatch_harness(&req, &advertised, &keys, &m.hub)
                            .await
                    }
                    // Kill switch, field checks, provenance and audit in core.
                    (None, Ok(_)) => {
                        kls_core::sessions::start::dispatch(
                            m.core(),
                            &node_id,
                            spec,
                            &fields,
                            &new_session_origin(),
                        )
                        .await
                    }
                    (None, Err(e)) => Err(e),
                };
                *slot.lock().unwrap() =
                    Some(r.as_ref().map(Clone::clone).map_err(|e| format!("{e:#}")));
                m.egui_ctx().request_repaint();
                r.map(|_| None)
            });
        }
        if !open {
            self.new = None;
        }
    }
}

/// The agent a session runs: a third-party harness id, or belai (K2).
fn agent_of(s: &Session) -> &str {
    kls_core::harness::session_harness(s).unwrap_or("belai")
}

/// The Agent filter's choices: belai first, then each harness seen.
fn agent_choices(sessions: &[Session]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in sessions {
        let a = agent_of(s);
        if !out.iter().any(|x| x == a) {
            out.push(a.to_string());
        }
    }
    out.sort_by_key(|a| (a != "belai", a.clone()));
    out
}

/// The sandboxing disclosure tag of a third-party harness session (C6.5, R6).
const HARNESS_RULES: &str = "harness rules";

fn harness_rules_hover(h: &str) -> String {
    format!("{h} runs under its own permission rules, not belai's sandbox")
}

fn harness_rules_chip(ui: &mut egui::Ui, h: &str) {
    let p = palette(ui.ctx());
    kit::chip(
        ui,
        &icons::label(icons::SHIELD_WARNING, HARNESS_RULES),
        p.amber,
    )
    .on_hover_text(harness_rules_hover(h));
}

/// The origin chip of a typed harness entry (C7.2): who put the line in.
fn origin_chip(e: &Entry) -> Option<String> {
    let h = kls_core::harness::entry_harness(e)?;
    let (icon, word) = match h.origin.as_str() {
        "" => return None,
        "dispatch" => (icons::FLOW_ARROW, "dispatch".to_string()),
        "web" => (icons::TERMINAL_WINDOW, "console".to_string()),
        "owner-answer" => (icons::HAND, "answer".to_string()),
        "harness:a2a-notice" => (icons::CHAT_CIRCLE_DOTS, "A2A".to_string()),
        o => (icons::TAG, kls_core::harness::clean_line(o, 24)),
    };
    Some(icons::label(icon, &word))
}

/// Where a harness entry came from, for its hover (C7.2): the native
/// record (type, id, byte offset) or belai's own line kind.
fn source_hover(e: &Entry) -> Option<String> {
    let h = kls_core::harness::entry_harness(e)?;
    let v: serde_json::Value = serde_json::from_str(&e.meta_json).ok()?;
    let m = v.get("meta").unwrap_or(&v);
    let clean = |s: &str| kls_core::harness::clean_line(s, 80);
    let mut out = vec![format!("{} harness", clean(&h.harness))];
    if let Some(n) = m.get("native") {
        let s = |k: &str| {
            n.get(k)
                .and_then(|x| x.as_str())
                .map(clean)
                .unwrap_or_default()
        };
        out.push(format!("native {}", s("type")));
        if !s("id").is_empty() {
            out.push(format!("id {}", s("id")));
        }
        if let Some(off) = n.get("off").and_then(|x| x.as_i64()) {
            out.push(format!("offset {off}"));
        }
    } else if !h.own.is_empty() {
        out.push(format!("belai {}", clean(&h.own)));
    }
    if let Some(id) = m.get("promptId").and_then(|x| x.as_str()) {
        out.push(format!("prompt {}", clean(id)));
    }
    if !h.origin.is_empty() {
        out.push(format!("from {}", clean(&h.origin)));
    }
    Some(out.join("\n"))
}

/// Who starts a dispatch from the New session dialog (rule 1).
fn new_session_origin() -> kls_core::hosts::Origin {
    kls_core::hosts::Origin {
        actor: "owner".into(),
        surface: "sessions/new".into(),
        ..Default::default()
    }
}

/// The on/off harness gates of the dialog (#51): key and the value sent.
const GATE_SWITCHES: [(&str, &str); 4] = [
    ("finish_review", "on"),
    ("lint_loop", "on"),
    ("checkpoints", "on"),
    ("code_mode", "off"),
];

fn gate_label(k: &str) -> &'static str {
    match k {
        "finish_review" => "Finish review",
        "lint_loop" => "Lint loop",
        "checkpoints" => "Checkpoints",
        _ => "No code mode",
    }
}

/// The console session actions (#45): name, icon, label, in menu order.
fn action_buttons() -> [(&'static str, &'static str, &'static str); 7] {
    [
        ("compact", icons::ARCHIVE, "Compact"),
        ("relay", icons::EXPORT, "Relay"),
        ("undo", icons::ARROW_U_UP_LEFT, "Undo"),
        ("rewind", icons::ARROW_COUNTER_CLOCKWISE, "Rewind"),
        ("commit", icons::GIT_COMMIT, "Commit"),
        ("btw", icons::CHAT_TEXT, "Btw"),
        ("rename", icons::PENCIL_SIMPLE, "Rename"),
    ]
}

/// The belai entry id of a transcript entry (`meta_json` "id").
fn entry_belai_id(e: &Entry) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&e.meta_json)
        .ok()?
        .get("id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Whether an entry is the plan file a plan-mode session wrote (#52).
fn is_plan_file(e: &Entry) -> bool {
    serde_json::from_str::<serde_json::Value>(&e.meta_json)
        .ok()
        .is_some_and(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("plan_file")
                || v.pointer("/meta/kind").and_then(|t| t.as_str()) == Some("plan_file")
        })
}

/// The handoff text of a relay review's `relay_json`.
fn relay_text(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_default()
}

fn resume_origin() -> kls_core::hosts::Origin {
    kls_core::hosts::Origin {
        actor: "owner".into(),
        surface: "sessions/resume".into(),
        ..Default::default()
    }
}

/// A tri-state switch: host default, on, off.
fn tri_ui(ui: &mut egui::Ui, id: &str, v: &mut Option<bool>) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(match v {
            None => "host default",
            Some(true) => "on",
            Some(false) => "off",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(v, None, "host default");
            ui.selectable_value(v, Some(true), "on");
            ui.selectable_value(v, Some(false), "off");
        });
}

/// The dialog's start fields (#7, #8, #26), folded under Options.
fn options_ui(ui: &mut egui::Ui, n: &mut NewSession, node: Option<&Node>, bindings: &[McpBinding]) {
    let open = n.more;
    if kit::toggle_chip(ui, &icons::label(icons::SLIDERS, "Options"), open).clicked() {
        n.more = !open;
    }
    if !n.more {
        return;
    }
    let kind = n.kind();
    egui::Grid::new("new_session_options")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            if n.crew.trim().is_empty() && !n.agent.trim().is_empty() {
                ui.label(icons::label(icons::ROBOT, "Worker"));
                ui.checkbox(&mut n.as_worker, "Claim cards")
                    .on_hover_text("Run the profile as a worker");
                ui.end_row();
            }
            ui.label(icons::label(icons::GAUGE, "Effort"));
            egui::ComboBox::from_id_salt("new_session_effort")
                .selected_text(if n.effort.is_empty() {
                    "host default"
                } else {
                    n.effort.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut n.effort, String::new(), "host default");
                    for e in kls_core::sessions::start::EFFORTS {
                        ui.selectable_value(&mut n.effort, e.to_string(), e);
                    }
                });
            ui.end_row();
            ui.label(icons::label(icons::HAND, "Ask"));
            tri_ui(ui, "new_session_ask", &mut n.ask);
            ui.end_row();
            if kind == "session" {
                ui.label(icons::label(icons::FOLDER_PLUS, "Extra dirs"));
                ui.vertical(|ui| {
                    let trusted: Vec<String> = node
                        .map(|x| x.host.trusted_dirs.clone())
                        .unwrap_or_default();
                    let mut drop = None;
                    for (i, d) in n.dirs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(kit::mono(d, 12.0, theme::palette(ui.ctx()).muted));
                            if ui.small_button(icons::X).on_hover_text("Remove").clicked() {
                                drop = Some(i);
                            }
                        });
                    }
                    if let Some(i) = drop {
                        n.dirs.remove(i);
                    }
                    let offer: Vec<&String> = trusted
                        .iter()
                        .filter(|d| d.as_str() != n.cwd.trim() && !n.dirs.contains(d))
                        .collect();
                    if !offer.is_empty() && n.dirs.len() < kls_core::sessions::start::MAX_DIRS {
                        ui.menu_button(icons::label(icons::PLUS, "Add"), |ui| {
                            for d in offer {
                                if ui.button(d).clicked() {
                                    n.dirs.push(d.clone());
                                    ui.close();
                                }
                            }
                        });
                    } else if trusted.is_empty() {
                        ui.weak("No trusted dirs");
                    }
                });
                ui.end_row();
                ui.label(icons::label(icons::GIT_FORK, "Workspace"));
                ui.add(
                    egui::TextEdit::singleline(&mut n.workspace)
                        .hint_text("owner/repo/name")
                        .desired_width(220.0),
                );
                ui.end_row();
                ui.label(icons::label(icons::ARROWS_CLOCKWISE, "Git sync"));
                tri_ui(ui, "new_session_git_sync", &mut n.git_sync);
                ui.end_row();
                ui.label(icons::label(icons::LIST_CHECKS, "Plan first"));
                ui.checkbox(&mut n.plan_first, "Plan mode")
                    .on_hover_text("Returns the plan file for review");
                ui.end_row();
                ui.label(icons::label(icons::SHIELD_CHECK, "Gates"));
                ui.horizontal_wrapped(|ui| {
                    for (k, _) in GATE_SWITCHES {
                        let mut on = n.gate_on.contains(&k);
                        if ui.checkbox(&mut on, gate_label(k)).changed() {
                            if on {
                                n.gate_on.push(k);
                            } else {
                                n.gate_on.retain(|x| *x != k);
                            }
                        }
                    }
                    egui::ComboBox::from_id_salt("new_session_commit_mode")
                        .selected_text(if n.commit_mode.is_empty() {
                            "commit: host"
                        } else {
                            n.commit_mode.as_str()
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut n.commit_mode, String::new(), "commit: host");
                            for m in ["off", "ask", "auto"] {
                                ui.selectable_value(&mut n.commit_mode, m.to_string(), m);
                            }
                        });
                });
                ui.end_row();
                ui.label(icons::label(icons::CHAT_CIRCLE_DOTS, "Answers"));
                ui.add(
                    egui::TextEdit::multiline(&mut n.answers)
                        .hint_text(r#"[{"choose":0,"note":"…"}]"#)
                        .desired_rows(2)
                        .desired_width(260.0),
                );
                ui.end_row();
            } else {
                ui.label(icons::label(icons::HOURGLASS, "Stay"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut n.stay, "Wait for work");
                    if n.stay {
                        ui.add(
                            egui::TextEdit::singleline(&mut n.stay_idle_min)
                                .hint_text("idle min")
                                .desired_width(60.0),
                        );
                    }
                });
                ui.end_row();
                if kind == "crew" {
                    ui.label(icons::label(icons::USERS_THREE, "Fill"));
                    ui.checkbox(&mut n.fill, "Every member slot");
                    ui.end_row();
                }
                ui.label(icons::label(icons::DOWNLOAD_SIMPLE, "Clone"));
                ui.add(
                    egui::TextEdit::singleline(&mut n.clone)
                        .hint_text("owner/name@branch")
                        .desired_width(220.0),
                );
                ui.end_row();
            }
            ui.label(icons::label(icons::PLUG, "MCP"));
            ui.vertical(|ui| {
                if bindings.is_empty() {
                    ui.weak("No bindings")
                        .on_hover_text("Vault › MCP binds an env secret to a server key");
                }
                for b in bindings {
                    let id = (b.server.clone(), b.key.clone());
                    let mut on = n.mcp.contains(&id);
                    if ui
                        .checkbox(&mut on, format!("{}:{}", b.server, b.key))
                        .on_hover_text(format!("Vault secret {}", b.secret))
                        .changed()
                    {
                        if on {
                            n.mcp.push(id);
                        } else {
                            n.mcp.retain(|x| x != &id);
                        }
                    }
                }
            });
            ui.end_row();
        });
}

fn build_lines(entries: &[Entry], expanded: &HashSet<i64>) -> Vec<Line> {
    let mut out = Vec::with_capacity(entries.len() * 2);
    for (i, e) in entries.iter().enumerate() {
        if is_noise(e) {
            continue;
        }
        out.push(Line {
            entry: i,
            kind: LineKind::Header,
        });
        if is_tool(&e.role) && !expanded.contains(&e.idx) {
            continue;
        }
        let n = e.text.lines().count();
        for l in 0..n.min(MAX_BODY_LINES) {
            out.push(Line {
                entry: i,
                kind: LineKind::Body(l),
            });
        }
        if n > MAX_BODY_LINES {
            out.push(Line {
                entry: i,
                kind: LineKind::More(n - MAX_BODY_LINES),
            });
        }
    }
    out
}

/// System lines with nothing to read: blank ones, bare internal tags such as
/// `mode_forced`, and the JSON header a session file starts with. They keep
/// their index on the server; the transcript just does not draw them.
/// The factory context: run step, budgets, PR checks, cards, todo.
fn context_ui(ui: &mut egui::Ui, model: &Model, c: &kls_core::sessions::context::SessionContext) {
    let p = palette(ui.ctx());
    let fact = |ui: &mut egui::Ui, icon: &str, text: &str, color: egui::Color32| {
        ui.label(
            RichText::new(icons::label(icon, text))
                .monospace()
                .size(12.0)
                .color(color),
        );
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if let Some(st) = &c.step {
            let text = if st.title.is_empty() {
                st.run_status.clone()
            } else {
                format!("{} · {}", st.title, st.status)
            };
            if ui
                .link(RichText::new(icons::label(icons::FLOW_ARROW, &text)).size(12.0))
                .on_hover_text(format!("Run {} {}", short(&st.run_id), st.run_status))
                .clicked()
            {
                model.go(crate::Screen::Runs, Some(st.run_id.clone()));
            }
        }
        for b in &c.budgets {
            let color = if b.pct >= 100.0 {
                p.red
            } else if b.pct >= 80.0 {
                p.amber
            } else {
                p.muted
            };
            fact(
                ui,
                icons::WALLET,
                &format!("{} {:.0}%", b.name, b.pct),
                color,
            );
        }
        for pr in &c.pulls {
            let color = kit::status_color(&p, &pr.checks_state);
            let r = ui
                .link(
                    RichText::new(icons::label(
                        icons::GIT_PULL_REQUEST,
                        &format!("#{} {}", pr.number, pr.checks_state),
                    ))
                    .size(12.0)
                    .color(color),
                )
                .on_hover_text(if pr.failing.is_empty() {
                    pr.title.clone()
                } else {
                    format!("{}\nFailing: {}", pr.title, pr.failing.join(", "))
                });
            if r.clicked() && !pr.url.is_empty() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.url));
            }
        }
        for card in &c.cards {
            if ui
                .link(
                    RichText::new(icons::label(
                        icons::KANBAN,
                        &format!("{} {}", card.key, card.status),
                    ))
                    .size(12.0),
                )
                .on_hover_text(&card.title)
                .clicked()
            {
                model.go(crate::Screen::Board, Some(card.id.clone()));
            }
        }
    });
    if !c.todo.is_empty() {
        let done = c.todo.iter().filter(|t| t.status == "completed").count();
        ui.collapsing(
            icons::label(icons::LIST_CHECKS, &format!("Todo {done}/{}", c.todo.len())),
            |ui| {
                for t in &c.todo {
                    let (icon, color) = match t.status.as_str() {
                        "completed" => (icons::CHECK_CIRCLE, p.green),
                        "in_progress" => (icons::CIRCLE_NOTCH, p.gold_text),
                        _ => (icons::CIRCLE, p.muted),
                    };
                    ui.label(
                        RichText::new(icons::label(icon, &t.text))
                            .size(12.0)
                            .color(color),
                    );
                }
            },
        );
    }
}

/// Sessions grouped by their worker's crew (crews by name, "" last), each
/// group in list order.
fn group_by_crew<'a>(
    rows: &[&'a Session],
    crews: &[(String, String)],
) -> Vec<(String, Vec<&'a Session>)> {
    let crew_of = |id: &str| {
        crews
            .iter()
            .find(|(s, _)| s == id)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    };
    let mut out: Vec<(String, Vec<&Session>)> = Vec::new();
    for s in rows {
        let c = crew_of(&s.id);
        match out.iter_mut().find(|(k, _)| *k == c) {
            Some((_, v)) => v.push(s),
            None => out.push((c, vec![s])),
        }
    }
    out.sort_by(|a, b| (a.0.is_empty(), &a.0).cmp(&(b.0.is_empty(), &b.0)));
    out
}

fn is_noise(e: &Entry) -> bool {
    if e.role != "system" {
        return false;
    }
    let t = e.text.trim();
    t.is_empty()
        || (t.starts_with('{') && t.ends_with('}'))
        || t.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

fn is_tool(role: &str) -> bool {
    matches!(role, "tool" | "tool_result")
}

/// "tool Bash", "assistant", ... (a tool's name from its meta when present).
fn role_title(e: &Entry) -> String {
    if let Some(t) = kls_core::harness::harness_role_title(e) {
        return t;
    }
    if is_tool(&e.role)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.meta_json)
        && let Some(name) = ["name", "tool", "toolName"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
    {
        return format!("{} {name}", e.role);
    }
    e.role.clone()
}

/// The role column's word: "you" for the person, else [`role_title`].
fn role_label(e: &Entry) -> String {
    if e.role == "user" {
        "you".into()
    } else {
        role_title(e)
    }
}

fn role_color(ui: &egui::Ui, role: &str) -> egui::Color32 {
    let p = palette(ui.ctx());
    match role {
        "user" => p.gold_text,
        "assistant" => p.text,
        "tool" => p.cyan,
        "tool_result" => p.muted,
        "error" => p.red,
        "a2a" => p.violet,
        "shell" => p.amber,
        _ => p.muted,
    }
}

/// One session in the list: title and state on top, "node · model ·
/// updated · usage" in mono below; the open one gets the panel tint and a
/// gold left edge.
fn session_row(
    ui: &mut egui::Ui,
    data: &ListData,
    s: &Session,
    now: i64,
    on: bool,
    h: f32,
) -> egui::Response {
    let p = palette(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    let painter = ui.painter_at(rect);
    if on {
        painter.rect_filled(rect, 0.0, p.panel2);
        painter.rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
            0.0,
            p.gold,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, p.panel2.gamma_multiply(0.6));
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, p.line),
    );
    let (state, color) = state_label(ui, s, now);
    let right = rect.right() - 14.0;
    let state_rect = text_fit(
        &painter,
        Pos2::new(right, rect.top() + 12.0),
        Align2::RIGHT_TOP,
        &state,
        FontId::proportional(12.0),
        color,
        120.0,
    );
    let left = rect.left() + 14.0;
    text_fit(
        &painter,
        Pos2::new(left, rect.top() + 10.0),
        Align2::LEFT_TOP,
        &title_of(s),
        FontId::proportional(14.0),
        p.text,
        state_rect.left() - left - 8.0,
    );
    let mut meta = vec![node_name(data, &s.node_id)];
    let harness = kls_core::harness::session_harness(s);
    if let Some(h) = harness {
        meta.insert(0, icons::label(icons::ROBOT, h));
        meta.insert(1, icons::label(icons::SHIELD_WARNING, HARNESS_RULES));
    }
    if !s.model.is_empty() {
        meta.push(s.model.clone());
    }
    meta.push(fmt::ago(now, s.seen_at.max(s.updated_at)));
    meta.push(usage(s));
    text_fit(
        &painter,
        Pos2::new(left, rect.top() + 33.0),
        Align2::LEFT_TOP,
        &meta.join(" · "),
        FontId::monospace(12.0),
        p.muted,
        right - left,
    );
    let mut hover = if s.cwd.is_empty() {
        s.id.clone()
    } else {
        format!("{}\n{}", s.cwd, s.id)
    };
    if let Some(h) = harness {
        hover.push('\n');
        hover.push_str(&harness_rules_hover(h));
    }
    resp.on_hover_text(hover)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn state_label(ui: &egui::Ui, s: &Session, now: i64) -> (String, egui::Color32) {
    let p = palette(ui.ctx());
    if !s.is_open() {
        return (
            icons::label(icons::STOP, "ended"),
            kit::status_color(&p, "ended"),
        );
    }
    if !s.pending_question_id.is_empty() {
        return (
            icons::label(icons::CLOCK, "waiting"),
            kit::status_color(&p, "waiting"),
        );
    }
    if now - s.seen_at > LIVE_WINDOW_MS {
        return (
            icons::label(icons::PAUSE, "quiet"),
            kit::status_color(&p, "idle"),
        );
    }
    let state = if s.state.is_empty() { "live" } else { &s.state };
    (
        icons::label(icons::PLAY, state),
        kit::status_color(&p, "live"),
    )
}

fn title_of(s: &Session) -> String {
    if !s.title.is_empty() {
        s.title.clone()
    } else if !s.agent.is_empty() {
        s.agent.clone()
    } else {
        format!("session {}", short(&s.id))
    }
}

fn node_name(data: &ListData, id: &str) -> String {
    data.nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| short(id).to_string())
}

fn usage(s: &Session) -> String {
    let tokens = s.input_tokens + s.output_tokens;
    let t = if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1e6)
    } else if tokens >= 1000 {
        format!("{:.1}k tok", tokens as f64 / 1e3)
    } else {
        format!("{tokens} tok")
    };
    if s.cost_usd > 0.0 {
        format!("{t} · ${:.2}", s.cost_usd)
    } else {
        t
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn time_of(ms: i64) -> String {
    fmt::when(ms).get(11..).unwrap_or_default().to_string()
}

fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut s: String = first.chars().take(max).collect();
    if first.chars().count() > max || text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_system_lines_are_hidden_but_text_is_kept() {
        let e = |role: &str, text: &str| Entry {
            role: role.into(),
            text: text.into(),
            ..Default::default()
        };
        assert!(is_noise(&e("system", "")));
        assert!(is_noise(&e("system", "  ")));
        assert!(is_noise(&e("system", "mode_forced")));
        assert!(is_noise(&e("system", r#"{"schema":2}"#)));
        assert!(!is_noise(&e(
            "system",
            "Checked what your message returned  14 clean"
        )));
        assert!(!is_noise(&e("assistant", "")));
        assert!(!is_noise(&e("user", "mode_forced")));
        let lines = build_lines(&[e("system", ""), e("user", "hi")], &HashSet::new());
        assert!(lines.iter().all(|l| l.entry == 1));
    }

    #[test]
    fn dialog_sends_answers_gates_and_plan_first() {
        let mut n = NewSession {
            cwd: "/w".into(),
            prompt: "go".into(),
            answers: r#"[{"choose":0}]"#.into(),
            plan_first: true,
            gate_on: vec!["finish_review", "code_mode"],
            commit_mode: "off".into(),
            ..Default::default()
        };
        assert_eq!(n.spec().mode, "plan");
        let f = n.fields(&[]).unwrap();
        assert_eq!(f.answers_json, r#"[{"choose":0}]"#);
        assert_eq!(f.gates.get("finish_review").unwrap(), "on");
        assert_eq!(f.gates.get("code_mode").unwrap(), "off");
        assert_eq!(f.gates.get("commit_mode").unwrap(), "off");
        n.answers = "not json".into();
        assert!(n.fields(&[]).is_err());
    }

    #[test]
    fn entry_ids_plan_files_and_relay_text() {
        let e = Entry {
            idx: 4,
            role: "system".into(),
            meta_json: r#"{"type":"plan_file","id":"e4","meta":{"kind":"plan_file"}}"#.into(),
            ..Default::default()
        };
        assert_eq!(entry_belai_id(&e).as_deref(), Some("e4"));
        assert!(is_plan_file(&e));
        assert_eq!(relay_text(r#"{"text":"note","direction":""}"#), "note");
        assert_eq!(relay_text("bad"), "");
        let names: Vec<&str> = action_buttons().iter().map(|a| a.0).collect();
        assert_eq!(names, kls_core::sessions::actions::ACTIONS);
    }

    #[test]
    fn new_session_spec_picks_session_or_crew() {
        let mut n = NewSession {
            cwd: " /src ".into(),
            agent: "belai:builder".into(),
            mode: "agent".into(),
            ..Default::default()
        };
        let s = n.spec();
        assert_eq!(
            (s.kind.as_str(), s.cwd.as_str(), s.agent.as_str()),
            ("session", "/src", "belai:builder")
        );
        assert!(s.crew.is_empty());
        n.crew = "belai:repro".into();
        let s = n.spec();
        assert_eq!((s.kind.as_str(), s.crew.as_str()), ("crew", "belai:repro"));
    }

    #[derive(Default)]
    struct Gw {
        sent: Mutex<Vec<(String, DispatchSpec)>>,
    }

    #[async_trait::async_trait]
    impl kls_core::NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> anyhow::Result<String> {
            let mut s = self.sent.lock().unwrap();
            s.push((node_id.into(), spec));
            Ok(format!("d{}", s.len()))
        }
        async fn job(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: kls_core::JobNeeds,
            _: i64,
        ) -> anyhow::Result<kls_core::gateway::JobOutcome> {
            anyhow::bail!("no jobs")
        }
    }

    /// P3 acceptance: the dialog's dirs, effort and stay reach the gateway
    /// through core (provenance and audit), and the kill switch refuses.
    #[tokio::test]
    async fn dialog_dispatch_carries_dirs_effort_and_stay() {
        let dir = tempfile::tempdir().unwrap();
        let core = kls_core::Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw::default());
        core.set_gateway(gw.clone());
        let bindings = vec![McpBinding {
            server: "github".into(),
            key: "token".into(),
            secret: "GH_MCP_TOKEN".into(),
        }];
        let mut n = NewSession {
            cwd: "/src/app".into(),
            prompt: "fix it".into(),
            mode: "agent".into(),
            dirs: vec!["/src/lib".into(), "/src/app".into()],
            effort: "high".into(),
            git_sync: Some(false),
            // Worker-only: left out of a session.
            stay: true,
            mcp: vec![("github".into(), "token".into())],
            ..Default::default()
        };
        let f = n.fields(&bindings).unwrap();
        assert_eq!(f.dirs, ["/src/lib"], "the primary directory is not extra");
        assert!(!f.stay);
        let o = new_session_origin();
        kls_core::sessions::start::dispatch(&core, "n1", n.spec(), &f, &o)
            .await
            .unwrap();
        // The same dialog as a staying worker.
        n.agent = "patcher".into();
        n.as_worker = true;
        n.stay_idle_min = "15".into();
        let f = n.fields(&bindings).unwrap();
        kls_core::sessions::start::dispatch(&core, "n1", n.spec(), &f, &o)
            .await
            .unwrap();
        {
            let sent = gw.sent.lock().unwrap();
            let (node, s) = &sent[0];
            assert_eq!(node, "n1");
            assert_eq!(s.kind, "session");
            assert_eq!(s.dirs, ["/src/lib"]);
            assert_eq!(s.effort, "high");
            assert_eq!(s.git_sync, Some(false));
            assert_eq!(s.mcp, bindings);
            assert_eq!(s.trigger.as_ref().unwrap().source, "console");
            let s = &sent[1].1;
            assert_eq!(s.kind, "worker");
            assert!(s.stay);
            assert_eq!(s.stay_idle_max_s, 900);
            assert_eq!(s.effort, "high");
            assert!(s.dirs.is_empty());
        }
        let audited = core
            .audit_entries(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "dispatch" && e.payload["origin"]["surface"] == "sessions/new")
            .count();
        assert_eq!(audited, 2);
        // A bad idle wait never leaves the dialog.
        n.stay_idle_min = "soon".into();
        assert!(n.fields(&bindings).is_err());
        n.stay_idle_min.clear();

        kls_core::control::stop(&core, "test").await.unwrap();
        let f = n.fields(&bindings).unwrap();
        assert!(
            kls_core::sessions::start::dispatch(&core, "n1", n.spec(), &f, &o)
                .await
                .is_err()
        );
        assert_eq!(gw.sent.lock().unwrap().len(), 2);
    }

    #[test]
    fn harness_rows_tag_origin_and_agent() {
        let s = |kind: &str, provider: &str| Session {
            kind: kind.into(),
            provider: provider.into(),
            ..Default::default()
        };
        let list = vec![
            s("harness", "codex"),
            s("session", "openai"),
            s("harness", "claude"),
        ];
        assert_eq!(agent_of(&list[0]), "codex");
        assert_eq!(agent_of(&list[1]), "belai");
        assert_eq!(agent_choices(&list), ["belai", "claude", "codex"]);
        let mut typed = e(3, "user", "go");
        typed.meta_json =
            r#"{"meta":{"harness":"codex","own":"harness_prompt","origin":"dispatch","promptId":"p1"}}"#
                .into();
        assert!(origin_chip(&typed).unwrap().contains("dispatch"));
        let h = source_hover(&typed).unwrap();
        assert!(
            h.contains("belai harness_prompt") && h.contains("prompt p1"),
            "{h}"
        );
        let mut native = e(4, "assistant", "ok");
        native.meta_json =
            r#"{"meta":{"harness":"codex","native":{"type":"response_item","id":"r9","off":120}}}"#
                .into();
        assert!(origin_chip(&native).is_none());
        let h = source_hover(&native).unwrap();
        assert!(
            h.contains("native response_item") && h.contains("offset 120"),
            "{h}"
        );
        assert!(source_hover(&e(5, "user", "plain")).is_none());
        assert!(harness_rules_hover("codex").contains("not belai"));
    }

    fn e(idx: i64, role: &str, text: &str) -> Entry {
        Entry {
            idx,
            role: role.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tool_rows_collapse() {
        let entries = vec![
            e(0, "user", "fix it\nplease"),
            e(1, "tool", "a\nb\nc"),
            e(2, "assistant", "done"),
        ];
        let lines = build_lines(&entries, &HashSet::new());
        // user: header + 2, tool: header only, assistant: header + 1
        assert_eq!(lines.len(), 6);
        let mut open = HashSet::new();
        open.insert(1);
        assert_eq!(build_lines(&entries, &open).len(), 9);
    }

    #[test]
    fn long_bodies_are_capped() {
        let text = vec!["x"; MAX_BODY_LINES + 5].join("\n");
        let lines = build_lines(&[e(0, "assistant", &text)], &HashSet::new());
        assert_eq!(lines.len(), MAX_BODY_LINES + 2);
        assert!(matches!(lines.last().unwrap().kind, LineKind::More(5)));
    }

    #[test]
    fn tool_titles_and_summaries() {
        let mut t = e(0, "tool", "ls -la\nmore");
        t.meta_json = r#"{"name":"Bash"}"#.into();
        assert_eq!(role_title(&t), "tool Bash");
        assert_eq!(one_line(&t.text, 80), "ls -la…");
        assert_eq!(one_line("short", 80), "short");
        let s = Session {
            input_tokens: 1500,
            output_tokens: 500,
            cost_usd: 0.123,
            ..Default::default()
        };
        assert_eq!(usage(&s), "2.0k tok · $0.12");
    }
}
