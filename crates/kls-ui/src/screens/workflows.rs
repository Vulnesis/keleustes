//! Workflows screen: the list, the builder (a node-graph canvas drawn with
//! the egui painter, the palette, the inspector, live validation, dry run,
//! publish, enable and versions) and the Schedules view. Owner: M3 engine.
//!
//! egui-snarl was considered for the canvas; its newest release (0.12)
//! needs egui 0.36, so the canvas is our own.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{self, palette};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use egui_extras::{Column, TableBuilder};
use kls_engine::dryrun::DryRun;
use kls_engine::graph::{self as g, Edge, Graph, Node, Pos};
use kls_engine::ops::{self, StartRun, TriggerInfo, WorkflowDoc};
use kls_engine::validate::{self, Issue, Level, Mode};
use kls_store::engine::{Run, RunFilter, Schedule, Workflow};
use kls_store::now_ms;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

const NODE_W: f32 = 220.0;
const NODE_H: f32 = 84.0;
/// The node card's coloured top strip and its kind header below it.
const NODE_STRIP: f32 = 3.0;
const NODE_HEAD: f32 = 26.0;
const PORT_GAP: f32 = 18.0;
/// Canvas dot grid spacing.
const DOT_STEP: f32 = 20.0;

#[derive(Clone)]
struct Row {
    w: Workflow,
    last: Option<Run>,
    /// Why the workflow cannot start (no provider for a model step).
    blocked: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Workflows,
    Schedules,
}

#[derive(Clone, PartialEq)]
enum Sel {
    None,
    Node(String),
    Edge(usize),
}

struct Editor {
    wf_id: String,
    version: i64,
    revision: i64,
    graph: Graph,
    /// Hash of the graph the store holds (dirty when ours differs).
    base_sha: String,
    /// The store changed under local edits.
    stale: bool,
    sel: Sel,
    pan: Vec2,
    link: Option<(String, String)>,
    dry: Option<DryRun>,
    note: String,
    /// The repository a dry run pretends to run on.
    dry_repo: String,
    /// The publish panel (note, problems, triggers) is open.
    confirm_publish: bool,
    json_buf: Option<(String, String)>,
    show_versions: bool,
    rename: Option<(String, String)>,
    confirm_archive: bool,
}

impl Editor {
    fn new(doc: &WorkflowDoc) -> Self {
        let (version, revision, mut graph) = doc.editable();
        let base_sha = graph.sha256();
        layout_missing(&mut graph);
        Self {
            wf_id: doc.workflow.id.clone(),
            version,
            revision,
            // Positions filled in for display do not make the draft dirty.
            base_sha: if graph.sha256() == base_sha {
                base_sha
            } else {
                graph.sha256()
            },
            graph,
            stale: false,
            sel: Sel::None,
            pan: Vec2::new(20.0, 20.0),
            link: None,
            dry: None,
            note: String::new(),
            dry_repo: String::new(),
            confirm_publish: false,
            json_buf: None,
            show_versions: false,
            rename: None,
            confirm_archive: false,
        }
    }

    fn dirty(&self) -> bool {
        self.graph.sha256() != self.base_sha
    }

    /// Takes in what the store now has.
    fn sync(&mut self, doc: &WorkflowDoc) {
        let (version, revision, mut graph) = doc.editable();
        layout_missing(&mut graph);
        let sha = graph.sha256();
        if sha == self.base_sha && version == self.version && revision == self.revision {
            return;
        }
        if !self.dirty() {
            self.graph = graph;
            self.base_sha = sha;
            self.version = version;
            self.revision = revision;
            self.stale = false;
        } else if self.graph.sha256() == sha {
            // Our save landed.
            self.base_sha = sha;
            self.version = version;
            self.revision = revision;
            self.stale = false;
        } else {
            self.stale = true;
        }
    }

    fn reload(&mut self, doc: &WorkflowDoc) {
        let keep = (self.pan, self.show_versions);
        *self = Editor::new(doc);
        (self.pan, self.show_versions) = keep;
    }
}

/// Gives nodes without a position a place by depth in the graph.
fn layout_missing(graph: &mut Graph) {
    if graph.nodes.iter().all(|n| n.position.is_some()) {
        return;
    }
    let order = validate::topo_order(graph);
    let mut depth: HashMap<String, usize> = HashMap::new();
    for id in &order {
        let d = graph
            .edges
            .iter()
            .filter(|e| &e.to == id && !e.is_loop)
            .filter_map(|e| depth.get(&e.from))
            .map(|d| d + 1)
            .max()
            .unwrap_or(0);
        depth.insert(id.clone(), d);
    }
    let mut rows: HashMap<usize, usize> = HashMap::new();
    for n in graph.nodes.iter_mut() {
        let d = depth.get(&n.id).copied().unwrap_or(0);
        let r = rows.entry(d).or_insert(0);
        if n.position.is_none() {
            n.position = Some(Pos {
                x: 40.0 + d as f32 * 280.0,
                y: 60.0 + *r as f32 * 130.0,
            });
        }
        *r += 1;
    }
}

pub struct Screen {
    tab: Tab,
    list: Loader<Arc<Vec<Row>>>,
    selected: Arc<Mutex<Option<String>>>,
    detail: Loader<Option<Arc<WorkflowDoc>>>,
    schedules: Loader<Arc<Vec<(Schedule, String)>>>,
    /// Models and nodes for the crew inspector.
    pickers: Loader<(Vec<String>, Vec<String>)>,
    editor: Option<Editor>,
    new_name: Option<String>,
    filter: String,
    /// Schedules: kind filter, the row being re-timed and the new-schedule form.
    sched_kind: String,
    retime: Option<(String, String, String)>,
    new_sched: Option<(String, String, String)>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let list = Loader::new(
            model,
            &[
                "workflow.",
                "run.",
                "schedule.",
                "budget.provider",
                "vault.",
            ],
            |core| async move {
                let mut rows = Vec::new();
                let providers: Vec<String> = core
                    .store()
                    .providers()
                    .await?
                    .into_iter()
                    .filter(|p| p.factory)
                    .map(|p| p.id)
                    .collect();
                for w in core.store().workflows(false).await? {
                    let g = match core
                        .store()
                        .workflow_version(&w.id, w.published_version.max(w.draft_version))
                        .await?
                    {
                        Some(v) => Graph::parse(&v.graph_json).unwrap_or_default(),
                        None => Graph::default(),
                    };
                    let blocked = model_block(&g, &providers);
                    let last = core
                        .store()
                        .runs(&RunFilter {
                            workflow_id: Some(w.id.clone()),
                            limit: 1,
                            ..Default::default()
                        })
                        .await?
                        .into_iter()
                        .next();
                    rows.push(Row { w, last, blocked });
                }
                Ok(Arc::new(rows))
            },
        );
        let selected: Arc<Mutex<Option<String>>> = Arc::default();
        let sel = selected.clone();
        let detail = Loader::new(model, &["workflow.", "schedule."], move |core| {
            let id = sel.lock().unwrap().clone();
            async move {
                match id {
                    Some(id) => Ok(ops::workflow_doc(&core, &id).await?.map(Arc::new)),
                    None => Ok(None),
                }
            }
        });
        let schedules = Loader::new(model, &["schedule.", "workflow."], |core| async move {
            let names: HashMap<String, String> = core
                .store()
                .workflows(true)
                .await?
                .into_iter()
                .map(|w| (w.id, w.name))
                .collect();
            Ok(Arc::new(
                core.store()
                    .schedules()
                    .await?
                    .into_iter()
                    .map(|s| {
                        let n = names.get(&s.workflow_id).cloned().unwrap_or_default();
                        (s, n)
                    })
                    .collect(),
            ))
        });
        Self {
            tab: Tab::Workflows,
            list,
            selected,
            detail,
            schedules,
            pickers: Loader::new(model, &["budget.provider", "node."], |core| async move {
                let mut models = Vec::new();
                for p in core.store().providers().await? {
                    for m in &p.models {
                        models.push(format!("{}/{m}", p.id));
                    }
                }
                let nodes = core
                    .store()
                    .nodes()
                    .await?
                    .into_iter()
                    .filter(|n| n.revoked_at.is_none())
                    .map(|n| n.id)
                    .collect();
                Ok((models, nodes))
            }),
            editor: None,
            new_name: None,
            filter: String::new(),
            sched_kind: String::new(),
            retime: None,
            new_sched: None,
        }
    }

    /// Opens a workflow (id or slug) in the builder.
    pub fn focus(&mut self, id: &str) {
        self.tab = Tab::Workflows;
        match id {
            // The cockpit's "New workflow": the create dialog.
            "new" | "?new=1" | "new=1" => {
                self.open(None);
                self.new_name = Some(String::new());
            }
            "schedules" => self.tab = Tab::Schedules,
            _ => self.open(Some(id.to_string())),
        }
    }

    fn open(&mut self, id: Option<String>) {
        *self.selected.lock().unwrap() = id;
        self.editor = None;
        self.detail.reload();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        if let Some(Ok((m, n))) = self.pickers.get() {
            set_pickers(m, n);
        }
        let in_builder = self.tab == Tab::Workflows && self.selected.lock().unwrap().is_some();
        // The builder carries its own header (the workflow's name).
        if !in_builder {
            let (tab, new_name) = (&mut self.tab, &mut self.new_name);
            kit::page_header(ui, icons::TREE_STRUCTURE, "Workflows", |ui| {
                if *tab == Tab::Workflows {
                    if theme::primary_button(ui, &icons::label(icons::PLUS, "New workflow"))
                        .clicked()
                    {
                        *new_name = Some(String::new());
                    }
                    if kit::ghost_button(ui, &icons::label(icons::STACK, "Template")).clicked() {
                        model.go(crate::Screen::Templates, None);
                    }
                    ui.add_space(8.0);
                }
                if kit::tab_rich(
                    ui,
                    RichText::new(icons::label(icons::CALENDAR, "Schedules")),
                    *tab == Tab::Schedules,
                    30.0,
                )
                .clicked()
                {
                    *tab = Tab::Schedules;
                }
                if kit::tab_rich(
                    ui,
                    RichText::new(icons::label(icons::LIST, "Workflows")),
                    *tab == Tab::Workflows,
                    30.0,
                )
                .clicked()
                {
                    *tab = Tab::Workflows;
                }
            });
        }
        match self.tab {
            Tab::Schedules => self.schedules_ui(ui, model),
            Tab::Workflows => {
                let sel = self.selected.lock().unwrap().clone();
                match sel {
                    None => self.list_ui(ui, model),
                    Some(_) => self.builder_ui(ui, model),
                }
            }
        }
        self.new_dialog(ui.ctx(), model);
    }

    // ---- list --------------------------------------------------------------

    fn list_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Filter"))
                    .desired_width(260.0),
            );
        });
        ui.add_space(10.0);
        let rows = match self.list.get() {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        if rows.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    kit::empty(ui, icons::TREE_STRUCTURE, "No workflows");
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, &icons::label(icons::PLUS, "New workflow"))
                            .clicked()
                        {
                            self.new_name = Some(String::new());
                        }
                        if kit::ghost_button(ui, &icons::label(icons::STACK, "Template")).clicked()
                        {
                            model.go(crate::Screen::Templates, None);
                        }
                    });
                });
            return;
        }
        let q = self.filter.to_lowercase();
        let now = now_ms();
        let mut open = None;
        let p = palette(ui.ctx());
        kit::card(ui)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                TableBuilder::new(ui)
                    .id_salt("workflows_table")
                    .striped(false)
                    .resizable(true)
                    .sense(Sense::click())
                    .column(Column::auto().at_least(240.0))
                    .column(Column::auto().at_least(100.0))
                    .column(Column::auto().at_least(120.0))
                    .column(Column::auto().at_least(140.0))
                    .column(Column::remainder().at_least(120.0))
                    .column(Column::auto())
                    .header(30.0, |mut h| {
                        for t in ["Workflow", "State", "Version", "Last run", "Updated", ""] {
                            h.col(|ui| {
                                kit::th(ui, t);
                                kit::header_rule(ui);
                            });
                        }
                    })
                    .body(|mut body| {
                        for r in rows.iter().filter(|r| {
                            q.is_empty()
                                || r.w.name.to_lowercase().contains(&q)
                                || r.w.slug.contains(&q)
                        }) {
                            body.row(36.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(icons::label(
                                            icons::TREE_STRUCTURE,
                                            &r.w.name,
                                        ))
                                        .strong(),
                                    )
                                    .on_hover_text(&r.w.slug);
                                    if let Some(why) = &r.blocked {
                                        kit::chip(
                                            ui,
                                            &icons::label(icons::PROHIBIT, "blocked"),
                                            p.red,
                                        )
                                        .on_hover_text(why);
                                    }
                                    if r.w.origin == "system" {
                                        kit::chip(
                                            ui,
                                            if r.w.customised {
                                                "customised"
                                            } else {
                                                "default"
                                            },
                                            p.muted,
                                        );
                                    }
                                });
                                row.col(|ui| {
                                    if r.w.status == "published" {
                                        let on = r.w.enabled;
                                        if kit::toggle_chip(
                                            ui,
                                            &icons::label(
                                                icons::POWER,
                                                if on { "on" } else { "off" },
                                            ),
                                            on,
                                        )
                                        .on_hover_text(if on { "Turn off" } else { "Turn on" })
                                        .clicked()
                                        {
                                            let id = r.w.id.clone();
                                            model.act(move |m| async move {
                                                kls_engine::ops::set_workflow_enabled(
                                                    m.core(),
                                                    &id,
                                                    !on,
                                                )
                                                .await?;
                                                Ok(None)
                                            });
                                        }
                                    } else {
                                        kit::chip(ui, "draft", p.gold_text);
                                    }
                                });
                                row.col(|ui| {
                                    let mut s = String::new();
                                    if r.w.published_version > 0 {
                                        s.push_str(&format!("v{}", r.w.published_version));
                                    }
                                    if r.w.draft_version > 0 {
                                        if !s.is_empty() {
                                            s.push_str(" · ");
                                        }
                                        s.push_str(&format!("v{} draft", r.w.draft_version));
                                    }
                                    ui.label(RichText::new(s).monospace().color(p.muted));
                                });
                                row.col(|ui| match &r.last {
                                    Some(run) => {
                                        let c = status_color(ui, &run.status);
                                        ui.horizontal(|ui| {
                                            kit::dot(ui, c);
                                            ui.label(
                                                RichText::new(run.status.replace('_', " "))
                                                    .color(c),
                                            );
                                        })
                                        .response
                                        .on_hover_text(fmt::when(run.created_at));
                                    }
                                    None => {
                                        ui.label(RichText::new("—").color(p.muted));
                                    }
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(fmt::ago(now, r.w.updated_at)).color(p.muted),
                                    );
                                });
                                row.col(|ui| {
                                    if ui
                                        .button(icons::ARROW_RIGHT)
                                        .on_hover_text("Open")
                                        .clicked()
                                    {
                                        open = Some(r.w.id.clone());
                                    }
                                });
                                if row.response().clicked() {
                                    open = Some(r.w.id.clone());
                                }
                            });
                        }
                    });
            });
        if let Some(id) = open {
            self.open(Some(id));
        }
    }

    fn new_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(name) = self.new_name.as_mut() else {
            return;
        };
        let mut open = true;
        let mut create = false;
        egui::Window::new(icons::label(icons::PLUS, "New workflow"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Name");
                let r = ui.text_edit_singleline(name);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    create = true;
                }
                if theme::primary_button(ui, &icons::label(icons::PLUS, "Create"))
                    .on_hover_text("Starts as a draft with one manual trigger")
                    .clicked()
                {
                    create = true;
                }
            });
        if create {
            let name = name.trim().to_string();
            model.act(move |m| async move {
                let w = ops::create_workflow(
                    m.core(),
                    ops::NewWorkflow {
                        name,
                        ..Default::default()
                    },
                )
                .await?;
                m.go(crate::Screen::Workflows, Some(w.id));
                Ok(None)
            });
            self.new_name = None;
        } else if !open {
            self.new_name = None;
        }
    }

    // ---- builder -----------------------------------------------------------

    fn builder_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let doc = match self.detail.get() {
            Some(Ok(Some(d))) => d,
            Some(Ok(None)) => {
                ui.weak(icons::label(icons::WARNING, "Workflow not found"));
                if ui.button(icons::label(icons::ARROW_LEFT, "Back")).clicked() {
                    self.open(None);
                }
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        // The loader may still hold the previous workflow for a frame.
        let want = self.selected.lock().unwrap().clone().unwrap_or_default();
        if doc.workflow.id != want && doc.workflow.slug != want {
            ui.spinner();
            return;
        }
        match &mut self.editor {
            Some(ed) if ed.wf_id == doc.workflow.id => ed.sync(&doc),
            _ => self.editor = Some(Editor::new(&doc)),
        }
        let mut back = false;
        {
            let ed = self.editor.as_mut().unwrap();
            let issues = validate::validate(&ed.graph, Mode::Publish);
            let p = palette(ui.ctx());
            egui::Panel::top("wf_header")
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    left: 0,
                    right: 0,
                    top: 0,
                    bottom: 12,
                }))
                .show_separator_line(false)
                .show(ui, |ui| {
                    back = header_ui(ui, model, ed, &doc, &issues);
                });
            // Narrow: the palette becomes a strip on top and the inspector
            // a panel under the canvas.
            let narrow = ui.available_width() < 900.0;
            let side_frame = kit::card(ui).inner_margin(egui::Margin::same(14));
            if narrow {
                egui::Panel::top("wf_palette_strip")
                    .frame(side_frame)
                    .show_separator_line(false)
                    .show(ui, |ui| palette_ui(ui, ed, true));
            } else {
                egui::Panel::left("wf_palette")
                    .frame(side_frame)
                    .resizable(false)
                    .show_separator_line(false)
                    .exact_size(200.0)
                    .show(ui, |ui| palette_ui(ui, ed, false));
            }
            let inspector_frame = kit::glow(ui).inner_margin(egui::Margin::same(16));
            let inspector = |ui: &mut egui::Ui, ed: &mut Editor| {
                egui::ScrollArea::vertical()
                    .id_salt("wf_inspector_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        inspector_ui(ui, ed);
                        ui.add_space(10.0);
                        issues_ui(ui, ed, &issues);
                        if let Some(d) = ed.dry.clone() {
                            ui.add_space(10.0);
                            dry_run_ui(ui, &d, &ed.graph);
                        }
                        schedules_block(ui, &doc.schedules);
                    });
            };
            if narrow {
                egui::Panel::bottom("wf_inspector_bottom")
                    .frame(inspector_frame)
                    .resizable(true)
                    .show_separator_line(false)
                    .default_size(300.0)
                    .min_size(160.0)
                    .show(ui, |ui| inspector(ui, ed));
            } else {
                egui::Panel::right("wf_inspector")
                    .frame(inspector_frame)
                    .resizable(true)
                    .show_separator_line(false)
                    .default_size(340.0)
                    .min_size(280.0)
                    .max_size(460.0)
                    .show(ui, |ui| inspector(ui, ed));
            }
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(p.bg)
                        .stroke(Stroke::new(1.0, p.line)),
                )
                .show(ui, |ui| canvas_ui(ui, ed, &issues));
            versions_window(ui.ctx(), model, ed, &doc);
            rename_window(ui.ctx(), model, ed);
        }
        if back {
            self.open(None);
        }
    }

    // ---- schedules ---------------------------------------------------------

    fn schedules_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let rows = match self.schedules.get() {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        if rows.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    kit::empty(ui, icons::CALENDAR, "No schedules");
                })
                .response
                .on_hover_text("Publish a workflow with a schedule, card or event trigger");
            return;
        }
        let now = now_ms();
        let mut open = None;
        let p = palette(ui.ctx());
        let workflows: Vec<(String, String)> = self
            .list
            .get()
            .and_then(|r| r.ok())
            .map(|l| {
                l.iter()
                    .filter(|r| r.w.status == "published")
                    .map(|r| (r.w.id.clone(), r.w.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            for k in ["", "cron", "kanban", "event", "webhook", "label"] {
                let n = rows
                    .iter()
                    .filter(|(s, _)| k.is_empty() || s.kind == k)
                    .count();
                let label = if k.is_empty() { "All" } else { k };
                if kit::toggle_chip(ui, &format!("{label} {n}"), self.sched_kind == k).clicked() {
                    self.sched_kind = k.to_string();
                }
            }
            if self.new_sched.is_none()
                && !workflows.is_empty()
                && theme::primary_button(ui, &icons::label(icons::PLUS, "New schedule")).clicked()
            {
                self.new_sched = Some((workflows[0].0.clone(), "0 9 * * 1-5".into(), "UTC".into()));
            }
        });
        if let Some((wf, cron, tz)) = self.new_sched.as_mut() {
            let mut close = false;
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("new-sched-wf")
                    .selected_text(
                        workflows
                            .iter()
                            .find(|(id, _)| id == wf)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for (id, n) in &workflows {
                            ui.selectable_value(wf, id.clone(), n);
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(cron)
                        .desired_width(120.0)
                        .font(egui::TextStyle::Monospace),
                );
                ui.add(egui::TextEdit::singleline(tz).desired_width(110.0));
                ui.weak(kls_engine::cron::describe(cron, tz));
                if theme::primary_button(ui, &icons::label(icons::CHECK, "Add")).clicked() {
                    let (wf, cron, tz) = (wf.clone(), cron.clone(), tz.clone());
                    model.act(move |m| async move {
                        let p = ops::add_cron_schedule(m.core(), &wf, &cron, &tz).await?;
                        Ok(Some(Notice::Info(format!("Published v{}", p.version))))
                    });
                    close = true;
                }
                if kit::ghost_button(ui, icons::X).clicked() {
                    close = true;
                }
            });
            if close {
                self.new_sched = None;
            }
        }
        let kind = self.sched_kind.clone();
        let rows: Vec<&(Schedule, String)> = rows
            .iter()
            .filter(|(s, _)| kind.is_empty() || s.kind == kind)
            .collect();
        let mut retime = self.retime.take();
        kit::card(ui)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                TableBuilder::new(ui)
                    .id_salt("schedules_table")
                    .striped(false)
                    .resizable(true)
                    .sense(Sense::click())
                    .column(Column::auto().at_least(40.0))
                    .column(Column::auto().at_least(200.0))
                    .column(Column::auto().at_least(90.0))
                    .column(Column::auto().at_least(260.0))
                    .column(Column::auto().at_least(160.0))
                    .column(Column::remainder().at_least(110.0))
                    .header(30.0, |mut h| {
                        for t in ["On", "Workflow", "Kind", "When", "Next", "Last"] {
                            h.col(|ui| {
                                kit::th(ui, t);
                                kit::header_rule(ui);
                            });
                        }
                    })
                    .body(|mut body| {
                        for (s, name) in rows.iter().map(|r| (&r.0, &r.1)) {
                            body.row(34.0, |mut row| {
                                row.col(|ui| {
                                    let mut on = s.enabled;
                                    if ui.checkbox(&mut on, "").changed() {
                                        let id = s.id.clone();
                                        model.act(move |m| async move {
                                            ops::set_schedule_enabled(m.core(), &id, on).await?;
                                            Ok(None)
                                        });
                                    }
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(name).strong());
                                });
                                row.col(|ui| {
                                    let i = match s.kind.as_str() {
                                        "cron" => icons::CLOCK,
                                        "kanban" => icons::KANBAN,
                                        _ => icons::LIGHTNING,
                                    };
                                    kit::chip(ui, &icons::label(i, &s.kind), p.cyan);
                                });
                                row.col(|ui| {
                                    match retime.as_mut().filter(|(id, _, _)| *id == s.id) {
                                        Some((_, cron, tz)) => {
                                            ui.add(
                                                egui::TextEdit::singleline(cron)
                                                    .desired_width(110.0)
                                                    .font(egui::TextStyle::Monospace),
                                            );
                                            ui.add(
                                                egui::TextEdit::singleline(tz).desired_width(90.0),
                                            );
                                            if ui
                                                .button(icons::CHECK)
                                                .on_hover_text("Re-time (publishes a new version)")
                                                .clicked()
                                            {
                                                let (id, cron, tz) =
                                                    (s.id.clone(), cron.clone(), tz.clone());
                                                model.act(move |m| async move {
                                                    let p = ops::retime_schedule(
                                                        m.core(),
                                                        &id,
                                                        &cron,
                                                        &tz,
                                                    )
                                                    .await?;
                                                    Ok(Some(Notice::Info(format!(
                                                        "Published v{}",
                                                        p.version
                                                    ))))
                                                });
                                                retime = None;
                                            } else if ui.button(icons::X).clicked() {
                                                retime = None;
                                            }
                                        }
                                        None => {
                                            ui.label(schedule_text(s)).on_hover_text(&s.cron);
                                            if s.kind == "cron"
                                                && ui
                                                    .small_button(icons::PENCIL_SIMPLE)
                                                    .on_hover_text("Re-time")
                                                    .clicked()
                                            {
                                                retime = Some((
                                                    s.id.clone(),
                                                    s.cron.clone(),
                                                    s.tz.clone(),
                                                ));
                                            }
                                        }
                                    }
                                });
                                row.col(|ui| match s.next_run_at.filter(|_| s.enabled) {
                                    Some(t) => {
                                        ui.label(RichText::new(fmt::when(t)).monospace());
                                    }
                                    None => {
                                        ui.label(RichText::new("—").color(p.muted));
                                    }
                                });
                                row.col(|ui| {
                                    match s.last_fired_at {
                                        Some(t) => {
                                            ui.label(
                                                RichText::new(fmt::ago(now, t)).color(p.muted),
                                            );
                                        }
                                        None => {
                                            ui.label(RichText::new("—").color(p.muted));
                                        }
                                    }
                                    ui.menu_button(icons::DOTS_THREE, |ui| {
                                        if ui
                                            .button(icons::label(icons::TRASH, "Delete trigger"))
                                            .clicked()
                                        {
                                            let id = s.id.clone();
                                            model.act(move |m| async move {
                                                let p = ops::delete_schedule(m.core(), &id).await?;
                                                Ok(Some(Notice::Info(format!(
                                                    "Published v{}",
                                                    p.version
                                                ))))
                                            });
                                            ui.close();
                                        }
                                    });
                                });
                                if row.response().clicked() {
                                    open = Some(s.workflow_id.clone());
                                }
                            });
                        }
                    });
            });
        self.retime = retime;
        if let Some(id) = open {
            self.tab = Tab::Workflows;
            self.open(Some(id));
        }
    }
}

// ---- builder parts ----------------------------------------------------------

/// Returns true to go back to the list.
fn header_ui(
    ui: &mut egui::Ui,
    model: &Model,
    ed: &mut Editor,
    doc: &WorkflowDoc,
    issues: &[Issue],
) -> bool {
    let w = &doc.workflow;
    let p = palette(ui.ctx());
    let mut back = false;
    let errors = issues.iter().filter(|i| i.level == Level::Error).count();
    ui.horizontal_wrapped(|ui| {
        if kit::ghost_button(ui, icons::ARROW_LEFT)
            .on_hover_text("Back to workflows")
            .clicked()
        {
            back = true;
        }
        ui.label(
            RichText::new(icons::TREE_STRUCTURE)
                .size(28.0)
                .color(p.gold_text),
        );
        ui.label(RichText::new(&w.name).size(28.0).strong());
        if ui
            .small_button(icons::PENCIL_SIMPLE)
            .on_hover_text("Rename")
            .clicked()
        {
            ed.rename = Some((w.name.clone(), w.description.clone()));
        }
        if w.published_version > 0 {
            let (t, c) = if w.enabled {
                (
                    format!("v{} published · enabled", w.published_version),
                    p.green,
                )
            } else {
                (format!("v{} published · off", w.published_version), p.muted)
            };
            kit::chip(ui, &t, c);
        }
        if w.draft_version > 0 || ed.dirty() {
            let mut t = if w.draft_version > 0 {
                format!("draft v{}", w.draft_version)
            } else {
                "draft".to_string()
            };
            if errors > 0 {
                t.push_str(&format!(
                    " · {errors} issue{}",
                    if errors == 1 { "" } else { "s" }
                ));
            }
            kit::chip(ui, &t, p.gold_text);
        }
        if ed.dirty() {
            kit::chip(ui, &icons::label(icons::DOT_OUTLINE, "unsaved"), p.amber);
        }
        if ed.stale {
            kit::chip(
                ui,
                &icons::label(icons::WARNING, "changed elsewhere"),
                p.red,
            );
            if kit::ghost_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Reload")).clicked() {
                ed.reload(doc);
            }
        }
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        let id = w.id.clone();
        if ui
            .add_enabled_ui(ed.dirty(), |ui| {
                kit::ghost_button(ui, &icons::label(icons::FLOPPY_DISK, "Save"))
            })
            .inner
            .on_hover_text("Save draft")
            .clicked()
        {
            let (rev, graph) = (ed.revision, ed.graph.clone());
            model.act(move |m| async move {
                ops::save_draft(m.core(), &id, rev, &graph).await?;
                Ok(Some(Notice::Info("Draft saved".into())))
            });
        }
        let (lo, hi) = kls_engine::dryrun::estimate(&ed.graph);
        ui.label(
            RichText::new(icons::label(
                icons::CPU,
                &format!("{}–{}", short_tokens(lo), short_tokens(hi)),
            ))
            .color(theme::palette(ui.ctx()).muted),
        )
        .on_hover_text("Tokens per run (estimate)");
        if kit::ghost_button(ui, icons::CODE)
            .on_hover_text("Copy the workflow as JSON")
            .clicked()
        {
            ui.ctx()
                .copy_text(serde_json::to_string_pretty(&ed.graph).unwrap_or_default());
            model.notify(Notice::Info("Workflow JSON copied".into()));
        }
        ui.add(
            egui::TextEdit::singleline(&mut ed.dry_repo)
                .hint_text(icons::label(icons::GIT_BRANCH, "repo for dry run"))
                .desired_width(140.0),
        );
        if kit::ghost_button(ui, &icons::label(icons::EYE, "Dry run"))
            .on_hover_text("Walk the graph without dispatching anything")
            .clicked()
        {
            let repo = ed.dry_repo.trim().to_lowercase();
            let sample = if repo.is_empty() {
                Value::Null
            } else {
                json!({ "run": { "repo": repo }, "event": { "data": { "repo": repo } }, "inputs": {}, "steps": {} })
            };
            ed.dry = Some(kls_engine::dry_run(&ed.graph, &sample));
        }
        let can_publish = errors == 0 && (ed.dirty() || w.draft_version > 0);
        ui.add(
            egui::TextEdit::singleline(&mut ed.note)
                .char_limit(200)
                .hint_text(icons::label(icons::NOTE, "Note"))
                .desired_width(160.0),
        );
        let publish = ui
            .add_enabled_ui(can_publish, |ui| {
                theme::primary_button(ui, &icons::label(icons::ROCKET, "Publish"))
            })
            .inner
            .on_disabled_hover_text(if errors > 0 {
                format!("{errors} problems to fix first")
            } else {
                "Nothing new to publish".into()
            });
        if publish.clicked() {
            ed.confirm_publish = true;
        }
        let mut confirmed = false;
        if ed.confirm_publish && can_publish {
            let warnings = kls_engine::validate(&ed.graph, kls_engine::Mode::Publish)
                .iter()
                .filter(|i| i.level == kls_engine::Level::Warn)
                .count();
            let triggers: Vec<String> = ed
                .graph
                .triggers()
                .map(|n| format!("{} · {}", n.title, n.summary()))
                .collect();
            ui.label(
                RichText::new(icons::label(
                    icons::ROCKET,
                    &format!("{warnings} warnings · {} triggers", triggers.len()),
                ))
                .color(theme::palette(ui.ctx()).muted),
            )
            .on_hover_text(triggers.join("\n"));
            if theme::primary_button(ui, &icons::label(icons::CHECK, "Confirm")).clicked() {
                confirmed = true;
                ed.confirm_publish = false;
            }
            if kit::ghost_button(ui, icons::X)
                .on_hover_text("Back")
                .clicked()
            {
                ed.confirm_publish = false;
            }
        }
        if confirmed {
            let id = w.id.clone();
            let (rev, graph, dirty, note) =
                (ed.revision, ed.graph.clone(), ed.dirty(), ed.note.clone());
            let draft = w.draft_version;
            model.act(move |m| async move {
                let version = if dirty {
                    ops::save_draft(m.core(), &id, rev, &graph).await?.version
                } else {
                    draft
                };
                let p = ops::publish(m.core(), &id, version, &note).await?;
                Ok(Some(Notice::Info(format!(
                    "Published v{} with {} triggers",
                    p.version,
                    p.schedules.len()
                ))))
            });
            ed.note.clear();
        }
        if w.draft_version > 0
            && w.published_version > 0
            && kit::ghost_button(ui, &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Discard"))
                .on_hover_text("Discard draft")
                .clicked()
        {
            let id = w.id.clone();
            model.act(move |m| async move {
                ops::discard_draft(m.core(), &id).await?;
                Ok(Some(Notice::Info("Draft discarded".into())))
            });
        }
        if w.published_version > 0 {
            let mut on = w.enabled;
            if ui
                .checkbox(&mut on, icons::label(icons::POWER, "Enabled"))
                .changed()
            {
                let id = w.id.clone();
                model.act(move |m| async move {
                    ops::set_workflow_enabled(m.core(), &id, on).await?;
                    Ok(None)
                });
            }
            if ui
                .add_enabled_ui(w.enabled, |ui| {
                    kit::ghost_button(ui, &icons::label(icons::PLAY, "Run"))
                })
                .inner
                .on_hover_text("Start a run of the published version")
                .clicked()
            {
                let id = w.id.clone();
                model.act(move |m| async move {
                    let out = ops::start_run(
                        m.core(),
                        StartRun {
                            workflow_id: id,
                            trigger: TriggerInfo {
                                kind: "manual".into(),
                                label: "started by hand".into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )
                    .await?;
                    match out.run() {
                        Some(r) => {
                            m.go(crate::Screen::Runs, Some(r.id.clone()));
                            Ok(None)
                        }
                        None => Ok(Some(Notice::Info("A run is already going".into()))),
                    }
                });
            }
        }
        if kit::ghost_button(ui, &icons::label(icons::CLOCK, "Versions")).clicked() {
            ed.show_versions = !ed.show_versions;
        }
        if !ed.confirm_archive {
            if kit::danger_button(ui, &icons::label(icons::ARCHIVE, "Archive")).clicked() {
                ed.confirm_archive = true;
            }
        } else {
            ui.colored_label(p.amber, icons::label(icons::WARNING, "Cancels its runs"));
            if kit::danger_button(ui, &icons::label(icons::ARCHIVE, "Archive")).clicked() {
                let id = w.id.clone();
                model.act(move |m| async move {
                    ops::archive_workflow(m.core(), &id, true).await?;
                    Ok(Some(Notice::Info("Workflow archived".into())))
                });
                back = true;
            }
            if kit::ghost_button(ui, &icons::label(icons::X, "Keep")).clicked() {
                ed.confirm_archive = false;
            }
        }
    });
    back
}

/// The step palette: mono group labels over items with a coloured left
/// edge. `strip` lays the items out in wrapped rows (narrow windows).
fn palette_ui(ui: &mut egui::Ui, ed: &mut Editor, strip: bool) {
    let p = palette(ui.ctx());
    let mut add = None;
    let mut items = |ui: &mut egui::Ui, group: &str, width: f32| {
        for item in g::PALETTE.iter().filter(|x| x.group == group) {
            let icon = type_icon(item.node_type);
            let mut text = icons::label(icon, item.label);
            if let Some(r) = item.requires {
                text.push_str(&format!(" ({r})"));
            }
            let color = type_color(ui, item.node_type);
            if palette_item(ui, &text, color, width)
                .on_hover_text(item.help)
                .clicked()
            {
                add = Some(item);
            }
        }
    };
    if strip {
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::caps(&icons::label(icons::PLUS, "Add step"), p.muted));
            for (group, _) in g::PALETTE_GROUPS {
                items(ui, group, 0.0);
            }
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("wf_palette_scroll")
            .show(ui, |ui| {
                ui.label(kit::caps(
                    &icons::label(icons::PLUS, "Add step"),
                    p.gold_text,
                ));
                for (group, label) in g::PALETTE_GROUPS {
                    ui.add_space(10.0);
                    ui.label(kit::caps(label, p.muted));
                    let w = ui.available_width();
                    items(ui, group, w);
                }
            });
    }
    if let Some(item) = add {
        let n_existing = ed.graph.nodes.len() as f32;
        let pos = Pos {
            x: 60.0 - ed.pan.x + (n_existing % 4.0) * 30.0,
            y: 60.0 - ed.pan.y + (n_existing % 4.0) * 30.0,
        };
        let n = g::node_from_palette(&ed.graph, item, pos);
        ed.sel = Sel::Node(n.id.clone());
        ed.graph.nodes.push(n);
    }
}

/// One palette entry: the page background, a line border and a 3px left
/// edge in the step type's colour. `width` 0 sizes it to the text.
fn palette_item(ui: &mut egui::Ui, text: &str, color: Color32, width: f32) -> egui::Response {
    let p = palette(ui.ctx());
    let font = FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), p.text);
    let w = if width > 0.0 {
        width
    } else {
        galley.size().x + 26.0
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, if resp.hovered() { p.panel2 } else { p.bg });
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, if resp.hovered() { p.gold_edge } else { p.line }),
        StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
        0.0,
        color,
    );
    let clip = rect.shrink2(Vec2::new(4.0, 0.0));
    ui.painter_at(clip).galley(
        Pos2::new(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn issues_ui(ui: &mut egui::Ui, ed: &mut Editor, issues: &[Issue]) {
    let p = palette(ui.ctx());
    let errors: Vec<&Issue> = issues.iter().filter(|i| i.level == Level::Error).collect();
    let warns: Vec<&Issue> = issues.iter().filter(|i| i.level != Level::Error).collect();
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(kit::caps("Checks", p.muted));
        if errors.is_empty() {
            kit::chip(ui, &icons::label(icons::CHECK_CIRCLE, "valid"), p.green);
        } else {
            kit::chip(
                ui,
                &icons::label(icons::X_CIRCLE, &format!("{} problems", errors.len())),
                p.red,
            );
        }
    });
    for i in errors.iter().chain(warns.iter()) {
        let (color, icon) = match i.level {
            Level::Error => (p.red, icons::X_CIRCLE),
            Level::Warn => (p.amber, icons::WARNING),
            Level::Info => (p.muted, icons::INFO),
        };
        let step = i
            .node_id
            .as_deref()
            .and_then(|id| ed.graph.node(id))
            .map(|n| n.title.clone())
            .unwrap_or_default();
        let r = egui::Frame::new()
            .stroke(Stroke::new(1.0, color))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(icons::label(icon, &i.message)).color(color));
                ui.label(
                    RichText::new(format!("{} · {} · {step}", i.path, i.code))
                        .monospace()
                        .size(11.0)
                        .color(p.muted),
                );
            })
            .response
            .interact(Sense::click());
        if r.clicked()
            && let Some(n) = &i.node_id
        {
            ed.sel = Sel::Node(n.clone());
        }
        ui.add_space(4.0);
    }
}

/// The workflow's published triggers: when each fires and when next.
fn schedules_block(ui: &mut egui::Ui, schedules: &[Schedule]) {
    if schedules.is_empty() {
        return;
    }
    let p = palette(ui.ctx());
    ui.add_space(10.0);
    ui.separator();
    ui.label(kit::caps("Schedules", p.muted));
    let now = now_ms();
    for s in schedules {
        ui.horizontal_wrapped(|ui| {
            kit::dot(ui, if s.enabled { p.green } else { p.muted });
            ui.label(RichText::new(schedule_text(s)).size(13.0));
        });
        if let Some(t) = s.next_run_at.filter(|_| s.enabled) {
            ui.label(
                RichText::new(icons::label(icons::CLOCK, &fmt::when(t)))
                    .size(12.0)
                    .color(p.muted),
            )
            .on_hover_text("Next");
        }
        if let Some(t) = s.last_fired_at {
            ui.label(
                RichText::new(icons::label(icons::LIGHTNING, &fmt::ago(now, t)))
                    .size(12.0)
                    .color(p.muted),
            )
            .on_hover_text("Last fired");
        }
    }
}

/// "Weekdays at 16:00 (UTC)", "card todo → doing", "on session.ended".
fn schedule_text(s: &Schedule) -> String {
    let cfg: Value = serde_json::from_str(&s.config_json).unwrap_or_default();
    match s.kind.as_str() {
        "cron" => kls_engine::cron::describe(&s.cron, &s.tz),
        "kanban" => format!(
            "card {} → {}",
            cfg["fromList"]
                .as_str()
                .filter(|x| !x.is_empty())
                .unwrap_or("any"),
            cfg["toList"]
                .as_str()
                .filter(|x| !x.is_empty())
                .unwrap_or("any")
        ),
        _ => format!("on {}", cfg["event"].as_str().unwrap_or("?")),
    }
}

fn dry_run_ui(ui: &mut egui::Ui, d: &DryRun, graph: &Graph) {
    ui.separator();
    ui.label(kit::caps(
        &icons::label(icons::EYE, "Dry run"),
        palette(ui.ctx()).gold_text,
    ))
    .on_hover_text("Nothing was dispatched; this is what a run would do.");
    ui.label(icons::label(icons::GIT_BRANCH, &d.repositories.text));
    ui.label(icons::label(
        icons::COINS,
        &format!("{}–{} tokens", d.totals.min, d.totals.max),
    ));
    ui.label(icons::label(
        icons::GITHUB_LOGO,
        &format!("{}–{} GitHub calls", d.calls.min, d.calls.max),
    ));
    for p in &d.plan {
        let mut line = format!("{} · {}", p.title, p.would);
        if let Some(s) = &p.simulated {
            line.push_str(&format!(" → {}", s["outcome"].as_str().unwrap_or("?")));
        }
        if let Some(b) = p.brief_chars {
            line.push_str(&format!(" ({b} chars)"));
        }
        if p.calls.max > 0 {
            line.push_str(&format!(" · {}–{} calls", p.calls.min, p.calls.max));
        }
        let t = if p.default_path {
            egui::RichText::new(line)
        } else {
            egui::RichText::new(line).weak()
        };
        ui.label(t);
    }
    if d.plan.len() < graph.nodes.len() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            icons::label(
                icons::WARNING,
                &format!("{} unreached", graph.nodes.len() - d.plan.len()),
            ),
        )
        .on_hover_text("Steps not reached from a trigger");
    }
}

fn versions_window(ctx: &egui::Context, model: &Model, ed: &mut Editor, doc: &WorkflowDoc) {
    if !ed.show_versions {
        return;
    }
    let mut open = true;
    egui::Window::new(icons::label(icons::CLOCK, "Versions"))
        .open(&mut open)
        .default_width(460.0)
        .show(ctx, |ui| {
            for v in doc.versions.iter().rev() {
                ui.horizontal(|ui| {
                    ui.strong(format!("v{}", v.version));
                    let (c, i) = match v.status.as_str() {
                        "published" => (theme::palette(ui.ctx()).green, icons::CHECK),
                        "draft" => (ui.visuals().warn_fg_color, icons::PENCIL_SIMPLE),
                        _ => (ui.visuals().weak_text_color(), icons::ARCHIVE),
                    };
                    ui.colored_label(c, icons::label(i, &v.status));
                    ui.weak(fmt::when(v.published_at.unwrap_or(v.updated_at)));
                    if !v.note.is_empty() {
                        ui.label(format!("“{}”", v.note));
                    }
                    if v.status != "draft"
                        && ui
                            .small_button(icons::label(icons::ARROW_U_UP_LEFT, "Restore"))
                            .on_hover_text("Copy into the draft")
                            .clicked()
                    {
                        let (id, version) = (doc.workflow.id.clone(), v.version);
                        model.act(move |m| async move {
                            ops::restore_version(m.core(), &id, version).await?;
                            Ok(Some(Notice::Info(format!(
                                "v{version} copied into the draft"
                            ))))
                        });
                    }
                });
            }
        });
    ed.show_versions = open;
}

fn rename_window(ctx: &egui::Context, model: &Model, ed: &mut Editor) {
    let Some((name, desc)) = ed.rename.as_mut() else {
        return;
    };
    let mut open = true;
    let mut save = false;
    egui::Window::new(icons::label(icons::PENCIL_SIMPLE, "Rename"))
        .collapsible(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(name);
            ui.label("Description");
            ui.text_edit_multiline(desc);
            save = theme::primary_button(ui, &icons::label(icons::FLOPPY_DISK, "Save")).clicked();
        });
    if save {
        let (id, n, d) = (ed.wf_id.clone(), name.clone(), desc.clone());
        model.act(move |m| async move {
            ops::rename_workflow(m.core(), &id, &n, &d).await?;
            Ok(None)
        });
        ed.rename = None;
    } else if !open {
        ed.rename = None;
    }
}

// ---- inspector -------------------------------------------------------------

fn text(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, label: &str) {
    kit::field_label(ui, label);
    let w = ui.available_width();
    text_input(ui, cfg, key, w);
    ui.add_space(4.0);
}

fn text_input(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, width: f32) {
    let mut s = cfg
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if ui
        .add(egui::TextEdit::singleline(&mut s).desired_width(width))
        .changed()
    {
        if s.is_empty() {
            cfg.remove(key);
        } else {
            cfg.insert(key.into(), Value::String(s));
        }
    }
}

fn multiline(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, label: &str, max: usize) {
    let mut s = cfg
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    kit::field_label(ui, &format!("{label} ({}/{max})", s.trim().chars().count()));
    if ui
        .add(
            egui::TextEdit::multiline(&mut s)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        )
        .changed()
    {
        if s.trim().is_empty() {
            cfg.remove(key);
        } else {
            cfg.insert(key.into(), Value::String(s));
        }
    }
}

fn combo(
    ui: &mut egui::Ui,
    cfg: &mut Map<String, Value>,
    key: &str,
    label: &str,
    opts: &[&str],
    salt: &str,
) {
    let cur = cfg
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    kit::field_label(ui, label);
    egui::ComboBox::from_id_salt((salt, key))
        .width(ui.available_width() - 8.0)
        .selected_text(if cur.is_empty() {
            "(default)"
        } else {
            cur.as_str()
        })
        .show_ui(ui, |ui| {
            if ui.selectable_label(cur.is_empty(), "(default)").clicked() {
                cfg.remove(key);
            }
            for o in opts {
                if ui.selectable_label(cur == *o, *o).clicked() {
                    cfg.insert(key.into(), Value::String(o.to_string()));
                }
            }
        });
    ui.add_space(4.0);
}

/// A combo that also takes free text (agents, crews).
fn suggest(
    ui: &mut egui::Ui,
    cfg: &mut Map<String, Value>,
    key: &str,
    label: &str,
    opts: &[&str],
    salt: &str,
) {
    kit::field_label(ui, label);
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 48.0).max(80.0);
        text_input(ui, cfg, key, w);
        egui::ComboBox::from_id_salt((salt, key, "s"))
            .selected_text("…")
            .width(24.0)
            .show_ui(ui, |ui| {
                for o in opts {
                    if ui.selectable_label(false, *o).clicked() {
                        cfg.insert(key.into(), Value::String(o.to_string()));
                    }
                }
            });
    });
}

fn int(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, label: &str, lo: i64, hi: i64) {
    let cur = cfg.get(key).and_then(Value::as_i64);
    ui.horizontal(|ui| {
        let mut set = cur.is_some();
        if ui.checkbox(&mut set, label).changed() {
            if set {
                cfg.insert(key.into(), json!(lo.max(1).min(hi)));
            } else {
                cfg.remove(key);
            }
        }
        if let Some(mut v) = cfg.get(key).and_then(Value::as_i64)
            && ui
                .add(egui::DragValue::new(&mut v).range(lo..=hi))
                .changed()
        {
            cfg.insert(key.into(), json!(v));
        }
    });
}

/// A comma-separated list held in `cfg[obj][key]` (an array of strings).
fn list_field(ui: &mut egui::Ui, obj: &mut Map<String, Value>, key: &str, label: &str, hint: &str) {
    let mut s = obj
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    kit::field_label(ui, label);
    if ui
        .add(
            egui::TextEdit::singleline(&mut s)
                .hint_text(hint)
                .desired_width(ui.available_width()),
        )
        .changed()
    {
        let v: Vec<Value> = s
            .split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| Value::String(t.into()))
            .collect();
        if v.is_empty() {
            obj.remove(key);
        } else {
            obj.insert(key.into(), Value::Array(v));
        }
    }
    ui.add_space(4.0);
}

/// GitHub trigger conditions (`conditions`).
fn conditions_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, salt: &str) {
    let mut c = cfg
        .get("conditions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    egui::CollapsingHeader::new(icons::label(icons::FUNNEL, "Conditions"))
        .id_salt((salt, "conditions"))
        .default_open(!c.is_empty())
        .show(ui, |ui| {
            list_field(ui, &mut c, "actions", "Actions", "opened, synchronize");
            list_field(
                ui,
                &mut c,
                "baseBranches",
                "Base branches",
                "main, release/*",
            );
            list_field(ui, &mut c, "headBranches", "Head branches", "feature/*");
            list_field(ui, &mut c, "paths", "Changed paths", "src/**");
            list_field(ui, &mut c, "ignorePaths", "Ignored paths", "docs/**");
            list_field(ui, &mut c, "labelsAny", "Any label", "bug");
            list_field(ui, &mut c, "labelsAll", "All labels", "");
            combo(
                ui,
                &mut c,
                "authorType",
                "Author",
                &kls_engine::triggers::AUTHOR_TYPES,
                salt,
            );
            combo(
                ui,
                &mut c,
                "senders",
                "Senders",
                &kls_engine::triggers::SENDERS,
                salt,
            );
            list_field(ui, &mut c, "authorsAllow", "Only authors", "");
            list_field(ui, &mut c, "authorsDeny", "Not authors", "");
            combo(
                ui,
                &mut c,
                "drafts",
                "Drafts",
                &kls_engine::triggers::DRAFTS,
                salt,
            );
            combo(
                ui,
                &mut c,
                "origin",
                "Origin",
                &kls_engine::triggers::ORIGINS,
                salt,
            );
            list_field(ui, &mut c, "reviewStates", "Review states", "APPROVED");
        });
    if c.is_empty() {
        cfg.remove("conditions");
    } else {
        cfg.insert("conditions".into(), Value::Object(c));
    }
}

/// Debounce, hourly cap and fan-out priority of an event trigger.
fn safeguards_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>) {
    int(ui, cfg, "debounceSeconds", "Coalesce within, s", 0, 86_400);
    int(ui, cfg, "maxRunsPerHour", "Max runs per hour", 1, 10_000);
    int(ui, cfg, "priority", "Priority (1 high)", 1, 5);
}

/// A SystemOne decision node.
fn decision_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, salt: &str) {
    multiline(ui, cfg, "question", "Question", g::MAX_BRIEF);
    let mut outs: Vec<String> = cfg
        .get("outcomes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|o| o.get("value").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut s = outs.join(", ");
    kit::field_label(ui, "Outcomes");
    if ui
        .add(egui::TextEdit::singleline(&mut s).desired_width(ui.available_width()))
        .changed()
    {
        outs = s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        cfg.insert(
            "outcomes".into(),
            Value::Array(outs.iter().map(|o| json!({ "value": o })).collect()),
        );
    }
    let mut t = cfg
        .get("threshold")
        .and_then(Value::as_f64)
        .unwrap_or(kls_engine::decision::DEFAULT_THRESHOLD) as f32;
    kit::field_label(ui, "Min confidence");
    if ui.add(egui::Slider::new(&mut t, 0.0..=1.0)).changed() {
        cfg.insert(
            "threshold".into(),
            json!((t as f64 * 100.0).round() / 100.0),
        );
    }
    combo(
        ui,
        cfg,
        "belowThreshold",
        "Below it",
        &kls_engine::decision::BELOW_ACTIONS,
        salt,
    );
    if cfg.get("belowThreshold").and_then(Value::as_str) == Some("safest") {
        let opts: Vec<&str> = outs.iter().map(String::as_str).collect();
        combo(ui, cfg, "safest", "Safest outcome", &opts, salt);
    }
    combo(
        ui,
        cfg,
        "onAbstain",
        "If it abstains",
        &kls_engine::decision::ABSTAIN_ACTIONS,
        salt,
    );
    text(ui, cfg, "runOn", "Run on (node:<id>)");
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn text_value(s: &str) -> Value {
    match serde_json::from_str::<Value>(s) {
        Ok(v) if !v.is_string() => v,
        _ => Value::String(s.to_string()),
    }
}

fn inspector_ui(ui: &mut egui::Ui, ed: &mut Editor) {
    match ed.sel.clone() {
        Sel::None => workflow_settings_ui(ui, ed),
        Sel::Edge(i) => edge_ui(ui, ed, i),
        Sel::Node(id) => node_ui(ui, ed, &id),
    }
}

/// The inspector's head: "INSPECTOR · KIND" in gold and the title.
fn inspector_head(ui: &mut egui::Ui, kind: &str, title: &str) {
    let p = palette(ui.ctx());
    ui.label(kit::caps(&format!("Inspector · {kind}"), p.gold_text));
    ui.add(egui::Label::new(RichText::new(title).size(18.0).strong()).truncate());
    ui.add_space(6.0);
}

/// The per-card agent limits (`limits`, sent as `Dispatch.budget`): the
/// field, its label, range and the value a tick starts at.
type LimitRow<'a> = (&'a mut Option<i64>, &'static str, i64, i64, i64);

fn limit_rows(l: &mut g::Limits) -> [LimitRow<'_>; 6] {
    [
        (&mut l.finalize_at_pct, "Finalize at %", 1, 100, 80),
        (&mut l.max_attempts_per_card, "Attempts", 1, 20, 3),
        (&mut l.max_continuations, "Continuations", 0, 50, 3),
        (&mut l.max_passes, "Passes", 1, 50, 4),
        (&mut l.max_wall_minutes, "Wall minutes", 1, 10_080, 60),
        (&mut l.max_iterations, "Tool rounds", 1, 1_000, 50),
    ]
}

/// The agents a crew step may give a model: the catalogue crew's members,
/// then any agent `memberModels` already names.
fn crew_member_names(crew: &str, own: Option<&Map<String, Value>>) -> Vec<String> {
    let mut out: Vec<String> = kls_engine::catalogue::crew(crew)
        .map(|c| c.members.iter().map(|m| m.profile.clone()).collect())
        .unwrap_or_default();
    for k in own.into_iter().flat_map(|o| o.keys()) {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// A step's start keys (`kls_core::sessions::start::StartFields`): the
/// fields that apply to `kind`, and the engine's own check of what is set.
fn start_fields_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, kind: &str, salt: &str) {
    use kls_core::sessions::start::{EFFORTS, StartFields};
    let session = kind == "session";
    let workers = matches!(kind, "worker" | "crew");
    egui::CollapsingHeader::new(icons::label(icons::GEAR_SIX, "Start options"))
        .id_salt((salt, "start"))
        .show(ui, |ui| {
            if session {
                kit::field_label(ui, &icons::label(icons::FOLDER, "Extra directories"));
                let mut s = dirs_text(cfg);
                if ui
                    .add(
                        egui::TextEdit::multiline(&mut s)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("/abs/path, one per line"),
                    )
                    .changed()
                {
                    set_dirs(cfg, &s);
                }
                text(ui, cfg, "workspace", "Workspace");
                tri(ui, cfg, "gitSync", "Git sync", salt);
                tri(ui, cfg, "ask", "Ask permission", salt);
            }
            combo(ui, cfg, "effort", "Effort", &EFFORTS, salt);
            if workers {
                if kind == "crew" {
                    flag(ui, cfg, "fill", "Fill missing replicas");
                }
                flag(ui, cfg, "stay", "Stay for work");
                if cfg.get("stay").and_then(Value::as_bool) == Some(true) {
                    int(ui, cfg, "stayIdleMaxS", "Idle wait (s)", 1, 86_400);
                } else {
                    cfg.remove("stayIdleMaxS");
                }
                text(ui, cfg, "clone", "Clone (owner/name@branch)");
            }
            mcp_ui(ui, cfg, salt);
            let check = StartFields::from_config(cfg).and_then(|f| f.validate(kind));
            if let Err(why) = check {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    icons::label(icons::WARNING, &why),
                );
            }
        });
}

/// `dirs` as editable text, one directory per line.
fn dirs_text(cfg: &Map<String, Value>) -> String {
    match cfg.get("dirs") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Stores `dirs` as a list (an empty text removes the key).
fn set_dirs(cfg: &mut Map<String, Value>, text: &str) {
    let dirs: Vec<Value> = text
        .split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .collect();
    if dirs.is_empty() {
        cfg.remove("dirs");
    } else {
        cfg.insert("dirs".into(), Value::Array(dirs));
    }
}

/// An on / off / host-default switch.
fn tri(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, label: &str, salt: &str) {
    let cur = cfg.get(key).and_then(Value::as_bool);
    kit::field_label(ui, label);
    egui::ComboBox::from_id_salt((salt, key, "tri"))
        .selected_text(match cur {
            None => "(host default)",
            Some(true) => "On",
            Some(false) => "Off",
        })
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(cur.is_none(), "(host default)")
                .clicked()
            {
                cfg.remove(key);
            }
            for (v, l) in [(true, "On"), (false, "Off")] {
                if ui.selectable_label(cur == Some(v), l).clicked() {
                    cfg.insert(key.into(), json!(v));
                }
            }
        });
    ui.add_space(4.0);
}

/// A checkbox stored only when on.
fn flag(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, key: &str, label: &str) {
    let mut on = cfg.get(key).and_then(Value::as_bool).unwrap_or(false);
    if ui.checkbox(&mut on, label).changed() {
        if on {
            cfg.insert(key.into(), json!(true));
        } else {
            cfg.remove(key);
        }
    }
}

/// MCP bindings: one row per server key, its value from a vault env secret.
fn mcp_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, salt: &str) {
    kit::field_label(ui, &icons::label(icons::PLUG, "MCP secrets"));
    let mut rows: Vec<[String; 3]> = cfg
        .get("mcp")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| {
                    let t = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    [t("server"), t("key"), t("secret")]
                })
                .collect()
        })
        .unwrap_or_default();
    let mut changed = false;
    let mut drop = None;
    for (i, r) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let w = ((ui.available_width() - 40.0) / 3.0).max(50.0);
            for (j, hint) in ["server", "key", "secret"].iter().enumerate() {
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut r[j])
                            .desired_width(w)
                            .hint_text(*hint)
                            .id_salt((salt, "mcp", i, j)),
                    )
                    .changed();
            }
            if ui
                .small_button(icons::TRASH)
                .on_hover_text("Remove")
                .clicked()
            {
                drop = Some(i);
            }
        });
    }
    if let Some(i) = drop {
        rows.remove(i);
        changed = true;
    }
    if ui
        .small_button(icons::label(icons::PLUS, "Binding"))
        .clicked()
    {
        rows.push(Default::default());
        changed = true;
    }
    if changed {
        set_mcp(cfg, &rows);
    }
}

fn set_mcp(cfg: &mut Map<String, Value>, rows: &[[String; 3]]) {
    if rows.is_empty() {
        cfg.remove("mcp");
        return;
    }
    let v: Vec<Value> = rows
        .iter()
        .map(|[s, k, x]| json!({ "server": s.trim(), "key": k.trim(), "secret": x.trim() }))
        .collect();
    cfg.insert("mcp".into(), Value::Array(v));
}

/// Per-member models of a crew step (`memberModels`: `{agent: "provider/model"}`).
fn member_models_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, models: &[&str], salt: &str) {
    let crew = cfg
        .get("crew")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut mm = cfg
        .get("memberModels")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let names = crew_member_names(&crew, Some(&mm));
    egui::CollapsingHeader::new(icons::label(icons::USERS, "Member models"))
        .id_salt((salt, "memberModels"))
        .default_open(!mm.is_empty())
        .show(ui, |ui| {
            if names.is_empty() {
                ui.weak(icons::label(icons::INFO, "Pick a crew"));
            }
            for a in &names {
                suggest(ui, &mut mm, a, a, models, &format!("{salt}-mm"));
            }
        });
    if mm.is_empty() {
        cfg.remove("memberModels");
    } else {
        cfg.insert("memberModels".into(), Value::Object(mm));
    }
}

fn workflow_settings_ui(ui: &mut egui::Ui, ed: &mut Editor) {
    inspector_head(ui, "Workflow", &icons::label(icons::SLIDERS, "Settings"));
    let l = &mut ed.graph.limits;
    let opt = |ui: &mut egui::Ui, v: &mut Option<i64>, label: &str, lo: i64, hi: i64, def: i64| {
        ui.horizontal(|ui| {
            let mut set = v.is_some();
            if ui.checkbox(&mut set, label).changed() {
                *v = set.then_some(def);
            }
            if let Some(x) = v.as_mut() {
                ui.add(egui::DragValue::new(x).range(lo..=hi));
            }
        });
    };
    opt(
        ui,
        &mut l.run_budget_tokens,
        "Run budget (tokens)",
        0,
        100_000_000,
        400_000,
    );
    opt(
        ui,
        &mut l.month_budget_tokens,
        "Month budget (tokens)",
        0,
        10_000_000_000,
        6_000_000,
    );
    opt(ui, &mut l.concurrency, "Runs at once", 1, 50, 2);
    opt(
        ui,
        &mut l.stop_the_line_attempts,
        "Stop the line after",
        1,
        10,
        3,
    );
    ui.add_space(6.0);
    kit::field_label(ui, &icons::label(icons::ROBOT, "Per card"));
    ui.horizontal(|ui| {
        let mut set = l.max_cost_usd_per_card.is_some();
        if ui.checkbox(&mut set, "Cost per card ($)").changed() {
            l.max_cost_usd_per_card = set.then_some(5.0);
        }
        if let Some(x) = l.max_cost_usd_per_card.as_mut() {
            ui.add(
                egui::DragValue::new(x)
                    .range(0.0..=10_000.0)
                    .speed(0.5)
                    .fixed_decimals(2),
            );
        }
    });
    for (v, label, lo, hi, def) in limit_rows(l) {
        opt(ui, v, label, lo, hi, def);
    }
    ui.add_space(6.0);
    kit::field_label(ui, &icons::label(icons::GIT_BRANCH, "Scope"));
    let mut repos = ed.graph.scope.repos.join(", ");
    if ui
        .add(
            egui::TextEdit::singleline(&mut repos)
                .hint_text("owner/name, …")
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(
            "Comma-separated repositories. Empty: the triggering repository, or any repository an online node has.",
        )
        .changed()
    {
        ed.graph.scope.repos = repos
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
    }
}

fn edge_ui(ui: &mut egui::Ui, ed: &mut Editor, i: usize) {
    let Some(e) = ed.graph.edges.get(i).cloned() else {
        ed.sel = Sel::None;
        return;
    };
    let from = ed.graph.node(&e.from).cloned().unwrap_or_default();
    let to_title = ed
        .graph
        .node(&e.to)
        .map(|n| n.title.clone())
        .unwrap_or_default();
    inspector_head(
        ui,
        "Connection",
        &format!("{} {} {}", from.title, icons::ARROW_RIGHT, to_title),
    );
    let edge = &mut ed.graph.edges[i];
    let outcomes = from.outcomes();
    if !outcomes.is_empty() {
        kit::field_label(ui, "Outcome");
        egui::ComboBox::from_id_salt(("edge-outcome", i))
            .width(ui.available_width() - 8.0)
            .selected_text(if edge.outcome.is_empty() {
                "(none)"
            } else {
                edge.outcome.as_str()
            })
            .show_ui(ui, |ui| {
                for o in &outcomes {
                    ui.selectable_value(&mut edge.outcome, o.value.clone(), &o.value);
                }
            });
    }
    kit::field_label(ui, "Label").on_hover_text("The artifact it carries");
    ui.add(egui::TextEdit::singleline(&mut edge.label).desired_width(f32::INFINITY));
    ui.checkbox(
        &mut edge.is_loop,
        icons::label(icons::ARROWS_CLOCKWISE, "Loop"),
    )
    .on_hover_text("Send work back to a crew");
    ui.add_space(6.0);
    if kit::danger_button(ui, &icons::label(icons::TRASH, "Remove"))
        .on_hover_text("Remove connection")
        .clicked()
    {
        ed.graph.edges.remove(i);
        ed.sel = Sel::None;
    }
}

fn node_ui(ui: &mut egui::Ui, ed: &mut Editor, id: &str) {
    let Some(idx) = ed.graph.nodes.iter().position(|n| n.id == id) else {
        ed.sel = Sel::None;
        return;
    };
    let mut remove = false;
    let json_buf = &mut ed.json_buf;
    let node = &mut ed.graph.nodes[idx];
    inspector_head(
        ui,
        type_label(&node.kind),
        &icons::label(type_icon(&node.kind), &node.title),
    );
    ui.label(
        RichText::new(&node.id)
            .monospace()
            .size(11.0)
            .color(palette(ui.ctx()).muted),
    );
    kit::field_label(ui, "Title");
    ui.add(egui::TextEdit::singleline(&mut node.title).desired_width(f32::INFINITY));
    ui.add_space(4.0);
    let salt = node.id.clone();
    let cfg = &mut node.config;
    match node.kind.as_str() {
        "trigger" => {
            combo(ui, cfg, "kind", "Starts on", &g::TRIGGER_KINDS, &salt);
            match cfg.get("kind").and_then(Value::as_str).unwrap_or("manual") {
                "cron" => {
                    text(ui, cfg, "cron", "Cron (m h dom mon dow)");
                    text(ui, cfg, "tz", "Time zone (IANA)");
                    let c = cfg
                        .get("cron")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let tz = cfg
                        .get("tz")
                        .and_then(Value::as_str)
                        .unwrap_or("UTC")
                        .to_string();
                    ui.weak(kls_engine::cron::describe(&c, &tz));
                    for t in kls_engine::cron::upcoming(&c, &tz, now_ms(), 3) {
                        ui.weak(icons::label(icons::CLOCK, &fmt::when(t)));
                    }
                    combo(
                        ui,
                        cfg,
                        "overlap",
                        "If a run is still going",
                        &g::OVERLAP,
                        &salt,
                    );
                    combo(
                        ui,
                        cfg,
                        "catchUp",
                        "Missed while closed",
                        &g::CATCH_UP,
                        &salt,
                    );
                }
                "kanban" => {
                    combo(
                        ui,
                        cfg,
                        "fromList",
                        "From list (empty: any)",
                        &g::CARD_STATUSES,
                        &salt,
                    );
                    combo(
                        ui,
                        cfg,
                        "toList",
                        "To list (empty: any)",
                        &g::CARD_STATUSES,
                        &salt,
                    );
                    combo(
                        ui,
                        cfg,
                        "overlap",
                        "If a run is still going",
                        &g::OVERLAP,
                        &salt,
                    );
                }
                "event" => {
                    text(ui, cfg, "event", "Event (session.ended, run.*)");
                    combo(
                        ui,
                        cfg,
                        "overlap",
                        "If a run is still going",
                        &g::OVERLAP,
                        &salt,
                    );
                    safeguards_ui(ui, cfg);
                }
                "webhook" => {
                    text(ui, cfg, "event", "GitHub event");
                    conditions_ui(ui, cfg, &salt);
                    safeguards_ui(ui, cfg);
                    needs_github(ui);
                }
                "label" => {
                    text(ui, cfg, "label", "Label");
                    conditions_ui(ui, cfg, &salt);
                    safeguards_ui(ui, cfg);
                    needs_github(ui);
                }
                _ => {
                    ui.weak(icons::label(icons::HAND, "Manual"))
                        .on_hover_text("Starts when you press Run");
                }
            }
        }
        "crew" => {
            combo(ui, cfg, "dispatch", "Start", &g::DISPATCH_KINDS, &salt);
            match cfg
                .get("dispatch")
                .and_then(Value::as_str)
                .unwrap_or("worker")
            {
                "crew" => suggest(ui, cfg, "crew", "Crew", &g::crews(), &salt),
                "session" => {}
                _ => suggest(ui, cfg, "agent", "Agent", &g::workers(), &salt),
            }
            combo(ui, cfg, "mode", "Mode", &g::MODES, &salt);
            multiline(ui, cfg, "brief", "Brief", g::MAX_BRIEF);
            let (models, nodes) = pickers();
            let models: Vec<&str> = models.iter().map(String::as_str).collect();
            suggest(ui, cfg, "model", "Model (empty: inherit)", &models, &salt);
            if cfg.get("dispatch").and_then(Value::as_str) == Some("crew") {
                member_models_ui(ui, cfg, &models, &salt);
            }
            let nodes: Vec<String> = nodes.iter().map(|n| format!("node:{n}")).collect();
            let mut opts: Vec<&str> = vec!["auto"];
            opts.extend(nodes.iter().map(String::as_str));
            suggest(ui, cfg, "runOn", "Run on", &opts, &salt);
            combo(
                ui,
                cfg,
                "runOnFallback",
                "If that node is offline",
                &["wait", "any", "fail"],
                &salt,
            );
            text(ui, cfg, "cwd", "Working directory");
            combo(ui, cfg, "await", "Wait for", &g::AWAIT, &salt);
            int(ui, cfg, "maxAttempts", "Attempts", 1, 10);
            int(ui, cfg, "timeoutMinutes", "Time limit (min)", 1, 10_080);
            let kind = cfg
                .get("dispatch")
                .and_then(Value::as_str)
                .unwrap_or("worker")
                .to_string();
            start_fields_ui(ui, cfg, &kind, &salt);
        }
        "job" => {
            text(ui, cfg, "job", "Job kind");
            text(ui, cfg, "runOn", "Run on (auto, node:<id>)");
            let mut gh = cfg.get("needsGh").and_then(Value::as_bool).unwrap_or(false);
            if ui.checkbox(&mut gh, "Needs gh signed in").changed() {
                cfg.insert("needsGh".into(), json!(gh));
            }
            int(ui, cfg, "timeoutSeconds", "Time limit (s)", 1, 86_400);
            int(ui, cfg, "maxAttempts", "Attempts", 1, 10);
            kit::field_label(ui, "Payload (JSON)");
            if json_buf.as_ref().is_none_or(|(n, _)| *n != salt) {
                let cur = cfg.get("payload").cloned().unwrap_or_else(|| json!({}));
                *json_buf = Some((
                    salt.clone(),
                    serde_json::to_string_pretty(&cur).unwrap_or_default(),
                ));
            }
            if let Some((_, buf)) = json_buf.as_mut() {
                let r = ui.add(egui::TextEdit::multiline(buf).code_editor().desired_rows(4));
                match serde_json::from_str::<Value>(buf) {
                    Ok(v) if v.is_object() => {
                        if r.changed() {
                            cfg.insert("payload".into(), v);
                        }
                    }
                    _ => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            icons::label(icons::X_CIRCLE, "Not a JSON object"),
                        );
                    }
                }
            }
        }
        "condition" => condition_ui(ui, cfg, &salt),
        "decision" => decision_ui(ui, cfg, &salt),
        "provision" => {
            text(ui, cfg, "planId", "Host plan id");
            let (_, nodes) = pickers();
            let opts: Vec<&str> = nodes.iter().map(String::as_str).collect();
            suggest(ui, cfg, "node", "Node", &opts, &salt);
            text(ui, cfg, "confirmName", "Type the host name to confirm");
            ui.weak(icons::label(
                icons::SHIELD_CHECK,
                "Previewed, then run; held by the kill switch",
            ));
        }
        "human" => {
            let mut s = cfg
                .get("approvers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            kit::field_label(ui, "Approvers").on_hover_text("Comma-separated; owner is you");
            if ui
                .add(egui::TextEdit::singleline(&mut s).hint_text("owner, …"))
                .changed()
            {
                let v: Vec<Value> = s
                    .split(',')
                    .map(|x| x.trim())
                    .filter(|x| !x.is_empty())
                    .map(|x| json!(x))
                    .collect();
                cfg.insert("approvers".into(), Value::Array(v));
            }
            text(ui, cfg, "message", "Message");
            int(ui, cfg, "slaHours", "Answer within (hours)", 1, 720);
            combo(
                ui,
                cfg,
                "onTimeout",
                "If nobody answers",
                &g::ON_TIMEOUT,
                &salt,
            );
        }
        "gate" => {
            combo(ui, cfg, "check", "Checked by", &g::GATE_CHECKS, &salt);
            combo(ui, cfg, "onFail", "On fail", &g::GATE_ON_FAIL, &salt);
            ui.weak(icons::label(icons::INFO, "Manual gates wait in Checkpoints"))
                .on_hover_text(
                    "A manual gate waits in Checkpoints for you to pass or fail it. Connect its fail port back to a crew as a loop to retry the work.",
                );
        }
        "wait" => {
            int(ui, cfg, "minutes", "Wait (minutes)", 0, 10_080);
            text(ui, cfg, "event", "Or until event");
            int(ui, cfg, "timeoutMinutes", "Give up after (min)", 1, 10_080);
        }
        "card" => {
            combo(ui, cfg, "action", "Action", &g::CARD_ACTIONS, &salt);
            match cfg.get("action").and_then(Value::as_str).unwrap_or("move") {
                "label" | "unlabel" => text(ui, cfg, "label", "Label"),
                "assign" => text(ui, cfg, "assignee", "Assignee"),
                _ => combo(ui, cfg, "status", "To list", &g::CARD_STATUSES, &salt),
            }
            text(ui, cfg, "card", "Card (empty: the run's card)");
        }
        "notify" => {
            multiline(ui, cfg, "message", "Message", g::MAX_MESSAGE);
            ui.weak(icons::label(icons::INFO, "Placeholders"))
                .on_hover_text("{workflow}, {run}, {repo}, {node} and {status} are filled in.");
            combo(ui, cfg, "severity", "Severity", &g::SEVERITIES, &salt);
        }
        "github" => {
            combo(
                ui,
                cfg,
                "action",
                "GitHub action",
                &g::GITHUB_ACTIONS,
                &salt,
            );
            match cfg.get("action").and_then(Value::as_str).unwrap_or("") {
                "draft_pr" => {
                    combo(ui, cfg, "prState", "Opens as", &["draft", "ready"], &salt);
                    text(ui, cfg, "reviewers", "Reviewers (codeowners, logins)");
                    combo(ui, cfg, "base", "Base", &["default", "pr_head"], &salt);
                    combo(
                        ui,
                        cfg,
                        "ifNoBranch",
                        "If the crew pushed no branch",
                        &["skip", "fail"],
                        &salt,
                    );
                }
                "file_issues" => {
                    text(ui, cfg, "label", "Label");
                    text(ui, cfg, "title", "Title");
                    combo(
                        ui,
                        cfg,
                        "minSeverity",
                        "Min severity",
                        &["low", "medium", "high", "critical"],
                        &salt,
                    );
                    combo(
                        ui,
                        cfg,
                        "group",
                        "One issue per",
                        &["run", "finding"],
                        &salt,
                    );
                    int(ui, cfg, "maxIssues", "Max issues", 1, 100);
                }
                "check_run" => {
                    text(ui, cfg, "name", "Check name");
                    combo(ui, cfg, "when", "When", &["always", "on_findings"], &salt);
                }
                _ => text(ui, cfg, "message", "Text (comments)"),
            }
            ui.weak(icons::label(
                icons::SHIELD_CHECK,
                "Needs the repo's autonomy",
            ));
            needs_github(ui);
        }
        _ => {}
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let mut set = node.budget.is_some();
        if ui
            .checkbox(&mut set, icons::label(icons::COINS, "Token budget"))
            .changed()
        {
            node.budget = set.then_some(g::Budget { tokens: 100_000 });
        }
        if let Some(b) = node.budget.as_mut() {
            ui.add(egui::DragValue::new(&mut b.tokens).range(0..=100_000_000));
        }
    });
    ui.collapsing(icons::label(icons::CODE, "JSON"), |ui| {
        let mut s = serde_json::to_string_pretty(&node.config).unwrap_or_default();
        ui.add(
            egui::TextEdit::multiline(&mut s)
                .code_editor()
                .interactive(false),
        );
    });
    ui.add_space(6.0);
    let mut add_approval = false;
    ui.horizontal(|ui| {
        if ed.graph.node(id).is_some_and(|n| n.kind != "human")
            && kit::ghost_button(ui, &icons::label(icons::HAND_PALM, "Approval after"))
                .on_hover_text("Insert an approval between this step and the next")
                .clicked()
        {
            add_approval = true;
        }
        if kit::danger_button(ui, &icons::label(icons::TRASH, "Remove"))
            .on_hover_text("Remove step")
            .clicked()
        {
            remove = true;
        }
    });
    if add_approval {
        insert_approval_after(&mut ed.graph, id);
    }
    if remove {
        ed.graph.remove_node(id);
        ed.sel = Sel::None;
    }
}

fn condition_ui(ui: &mut egui::Ui, cfg: &mut Map<String, Value>, salt: &str) {
    let mut outcomes: Vec<g::Outcome> = cfg
        .get("outcomes")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let mut rules: Vec<g::Rule> = cfg
        .get("rules")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let mut changed = false;
    kit::field_label(ui, "Outcomes");
    let mut drop = None;
    for (i, o) in outcomes.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui
                .add(egui::TextEdit::singleline(&mut o.value).desired_width(120.0))
                .changed();
            changed |= ui
                .checkbox(&mut o.end, icons::label(icons::FLAG, "End"))
                .on_hover_text("Ends the run")
                .changed();
            if ui
                .small_button(icons::X)
                .on_hover_text("Remove outcome")
                .clicked()
            {
                drop = Some(i);
            }
        });
    }
    if let Some(i) = drop {
        outcomes.remove(i);
        changed = true;
    }
    if ui
        .small_button(icons::label(icons::PLUS, "Outcome"))
        .clicked()
    {
        outcomes.push(g::Outcome {
            value: format!("outcome_{}", outcomes.len() + 1),
            end: false,
        });
        changed = true;
    }
    let names: Vec<String> = outcomes.iter().map(|o| o.value.clone()).collect();
    kit::field_label(ui, "Rules").on_hover_text(
        "First match wins. Paths read run.*, event.*, inputs.* and steps.<step id>.* (status, route, output...).",
    );
    let mut drop = None;
    for (i, r) in rules.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut r.path)
                        .hint_text("steps.n2.output.x")
                        .desired_width(120.0),
                )
                .changed();
            egui::ComboBox::from_id_salt((salt, "op", i))
                .selected_text(&r.op)
                .width(60.0)
                .show_ui(ui, |ui| {
                    for op in g::RULE_OPS {
                        changed |= ui.selectable_value(&mut r.op, op.to_string(), op).changed();
                    }
                });
            let mut v = value_text(&r.value);
            if ui
                .add(egui::TextEdit::singleline(&mut v).desired_width(70.0))
                .changed()
            {
                r.value = text_value(&v);
                changed = true;
            }
            ui.label("→");
            egui::ComboBox::from_id_salt((salt, "out", i))
                .selected_text(&r.outcome)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for n in &names {
                        changed |= ui.selectable_value(&mut r.outcome, n.clone(), n).changed();
                    }
                });
            if ui
                .small_button(icons::X)
                .on_hover_text("Remove rule")
                .clicked()
            {
                drop = Some(i);
            }
        });
    }
    if let Some(i) = drop {
        rules.remove(i);
        changed = true;
    }
    if ui.small_button(icons::label(icons::PLUS, "Rule")).clicked() {
        rules.push(g::Rule {
            path: "event.data.".into(),
            op: "eq".into(),
            value: Value::Null,
            outcome: names.first().cloned().unwrap_or_default(),
        });
        changed = true;
    }
    let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
    combo(ui, cfg, "default", "Default outcome", &names_ref, salt);
    if changed {
        cfg.insert(
            "outcomes".into(),
            serde_json::to_value(&outcomes).unwrap_or_default(),
        );
        cfg.insert(
            "rules".into(),
            serde_json::to_value(&rules).unwrap_or_default(),
        );
    }
}

/// Amber "Needs GitHub" chip for steps that do nothing until it is connected.
fn needs_github(ui: &mut egui::Ui) {
    let p = palette(ui.ctx());
    kit::chip(
        ui,
        &icons::label(icons::GITHUB_LOGO, "Needs GitHub"),
        p.amber,
    )
    .on_hover_text("Does nothing until GitHub is connected");
}

fn type_icon(kind: &str) -> &'static str {
    match kind {
        "trigger" => icons::LIGHTNING,
        "crew" => icons::ROBOT,
        "job" => icons::TERMINAL,
        "condition" => icons::FLOW_ARROW,
        "decision" => icons::BRAIN,
        "provision" => icons::HARD_DRIVES,
        "wait" => icons::HOURGLASS,
        "human" => icons::HAND_PALM,
        "gate" => icons::SEAL_CHECK,
        "card" => icons::KANBAN,
        "notify" => icons::BELL,
        "github" => icons::GITHUB_LOGO,
        _ => icons::CIRCLE,
    }
}

pub(crate) fn type_label(kind: &str) -> &'static str {
    match kind {
        "trigger" => "Trigger",
        "crew" => "Agent",
        "job" => "Job",
        "condition" => "Condition",
        "decision" => "Decision",
        "provision" => "Provision",
        "wait" => "Wait",
        "human" => "Approval",
        "gate" => "Gate",
        "card" => "Card",
        "notify" => "Notify",
        "github" => "GitHub",
        _ => "Step",
    }
}

// ---- canvas ----------------------------------------------------------------

fn out_ports(n: &Node) -> Vec<String> {
    match n.kind.as_str() {
        "condition" | "gate" | "decision" => n.outcomes().into_iter().map(|o| o.value).collect(),
        _ => vec![String::new()],
    }
}

fn node_size(n: &Node) -> Vec2 {
    let ports = out_ports(n).len().max(1) as f32;
    Vec2::new(
        NODE_W,
        NODE_H.max(NODE_STRIP + NODE_HEAD + 16.0 + ports * PORT_GAP),
    )
}

fn node_rect(origin: Pos2, n: &Node) -> Rect {
    let p = n.position.unwrap_or_default();
    Rect::from_min_size(origin + Vec2::new(p.x, p.y), node_size(n))
}

/// Output ports sit on the right edge, centred on the card body (below the
/// kind header).
fn out_port_pos(r: Rect, n: &Node, outcome: &str) -> Pos2 {
    out_port_pos_scaled(r, n, outcome, 1.0)
}

fn in_port_pos(r: Rect) -> Pos2 {
    // Centred on the card body, like the output ports; the width gives the
    // preview's scale.
    let scale = r.width() / NODE_W;
    let body_top = r.top() + (NODE_STRIP + NODE_HEAD) * scale;
    Pos2::new(r.left(), (body_top + r.bottom()) / 2.0)
}

/// The colour of a step type: the node card's top strip and the palette
/// item's edge (triggers gold, agents cyan, logic blue, GitHub violet).
fn type_color(ui: &egui::Ui, kind: &str) -> Color32 {
    let p = palette(ui.ctx());
    match kind {
        "trigger" => p.gold_text,
        "crew" | "job" => p.cyan,
        "condition" | "wait" | "decision" => p.blue,
        "human" | "gate" => p.gold,
        "github" => p.violet,
        "card" => p.amber,
        _ => p.green,
    }
}

/// Colour for a run or step status: the kit's status colours, with running
/// in cyan as on the Runs artboard.
pub(crate) fn status_color(ui: &egui::Ui, status: &str) -> Color32 {
    let p = palette(ui.ctx());
    match status {
        "running" => p.cyan,
        "approved" => p.green,
        "rejected" => p.red,
        "skipped" | "canceled" => p.muted,
        s => kit::status_color(&p, s),
    }
}

/// The wire colour: the muted text colour, dimmed.
fn wire_color(ui: &egui::Ui) -> Color32 {
    palette(ui.ctx()).muted.gamma_multiply(0.6)
}

fn bezier(painter: &egui::Painter, a: Pos2, b: Pos2, stroke: Stroke) -> Pos2 {
    let dx = ((b.x - a.x).abs() * 0.5).max(40.0);
    let pts = [a, a + Vec2::new(dx, 0.0), b - Vec2::new(dx, 0.0), b];
    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
        pts,
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
    // The curve's midpoint (t = 0.5).
    let p = |i: usize| pts[i].to_vec2();
    (p(0) * 0.125 + p(1) * 0.375 + p(2) * 0.375 + p(3) * 0.125).to_pos2()
}

/// Paints one line of text anchored at `pos`, elided with "…" to `width`.
pub(crate) fn text_fit(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Rect {
    // One line never shows more than this; long tool output stays cheap.
    let text: String = text.chars().take(1000).collect();
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

/// A node card: panel fill, coloured top strip, a mono kind header over a
/// line, the title and a muted summary (or the step's status). Selected
/// cards get the gold edge and the soft gold glow.
fn draw_node(
    painter: &egui::Painter,
    ui: &egui::Ui,
    r: Rect,
    n: &Node,
    border: Stroke,
    status: Option<&str>,
    glow: bool,
) {
    let p = palette(ui.ctx());
    if glow {
        painter.add(
            egui::Shadow {
                offset: [0, 0],
                blur: 44,
                spread: 0,
                color: p.glow,
            }
            .as_shape(r, egui::CornerRadius::ZERO),
        );
    }
    painter.rect_filled(r, 0.0, p.panel);
    let color = type_color(ui, &n.kind);
    painter.rect_filled(
        Rect::from_min_size(r.min, Vec2::new(r.width(), NODE_STRIP)),
        0.0,
        color,
    );
    let head_bottom = r.top() + NODE_STRIP + NODE_HEAD;
    painter.hline(r.x_range(), head_bottom, Stroke::new(1.0, p.line));
    let inner = r.width() - 24.0;
    text_fit(
        painter,
        Pos2::new(r.left() + 12.0, r.top() + NODE_STRIP + NODE_HEAD / 2.0),
        Align2::LEFT_CENTER,
        &format!(
            "{}  {}",
            type_icon(&n.kind),
            type_label(&n.kind).to_uppercase()
        ),
        FontId::monospace(11.0),
        p.muted,
        inner,
    );
    text_fit(
        painter,
        Pos2::new(r.left() + 12.0, head_bottom + 10.0),
        Align2::LEFT_TOP,
        &n.title,
        FontId::proportional(14.0),
        p.text,
        inner,
    );
    let (sub, sub_color) = match status {
        Some(s) => (s.replace('_', " "), status_color(ui, s)),
        None => (n.summary(), p.muted),
    };
    text_fit(
        painter,
        Pos2::new(r.left() + 12.0, head_bottom + 30.0),
        Align2::LEFT_TOP,
        &sub,
        FontId::proportional(12.0),
        sub_color,
        inner,
    );
    painter.rect_stroke(r, 0.0, border, StrokeKind::Inside);
    for o in out_ports(n) {
        let pt = out_port_pos(r, n, &o);
        painter.circle_filled(pt, 5.0, color);
        painter.circle_stroke(pt, 5.0, Stroke::new(1.5, p.panel));
        if !o.is_empty() {
            painter.text(
                pt + Vec2::new(8.0, -4.0),
                Align2::LEFT_BOTTOM,
                &o,
                FontId::monospace(10.0),
                p.muted,
            );
        }
    }
    if n.kind != "trigger" {
        let pt = in_port_pos(r);
        painter.circle_filled(pt, 5.0, p.muted);
        painter.circle_stroke(pt, 5.0, Stroke::new(1.5, p.panel));
    }
}

/// The canvas dot grid, moving with the pan.
fn dot_grid(painter: &egui::Painter, rect: Rect, pan: Vec2, color: Color32) {
    let ox = pan.x.rem_euclid(DOT_STEP);
    let oy = pan.y.rem_euclid(DOT_STEP);
    let mut y = rect.top() + oy;
    while y < rect.bottom() {
        let mut x = rect.left() + ox;
        while x < rect.right() {
            painter.rect_filled(
                Rect::from_center_size(Pos2::new(x, y), Vec2::splat(1.5)),
                0.0,
                color,
            );
            x += DOT_STEP;
        }
        y += DOT_STEP;
    }
}

fn canvas_ui(ui: &mut egui::Ui, ed: &mut Editor, issues: &[Issue]) {
    let p = palette(ui.ctx());
    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    dot_grid(&painter, resp.rect, ed.pan, p.line);
    let origin = resp.rect.min + ed.pan;
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta);
        ed.pan += scroll;
    }
    if resp.dragged() && ed.link.is_none() {
        ed.pan += resp.drag_delta();
    }
    if resp.clicked() {
        ed.sel = Sel::None;
    }
    let bad: std::collections::HashSet<&str> = issues
        .iter()
        .filter(|i| i.level == Level::Error)
        .filter_map(|i| i.node_id.as_deref())
        .collect();
    let sel_node = match &ed.sel {
        Sel::Node(id) => Some(id.clone()),
        _ => None,
    };

    // Edges. Those touching the selected step are drawn in gold.
    let mut clicked_edge = None;
    for (i, e) in ed.graph.edges.iter().enumerate() {
        let (Some(a), Some(b)) = (ed.graph.node(&e.from), ed.graph.node(&e.to)) else {
            continue;
        };
        let from = out_port_pos(node_rect(origin, a), a, &e.outcome);
        let to = in_port_pos(node_rect(origin, b));
        let selected = ed.sel == Sel::Edge(i);
        let touches = sel_node
            .as_deref()
            .is_some_and(|s| s == e.from || s == e.to);
        let color = if selected || touches {
            p.gold
        } else if e.is_loop {
            p.amber
        } else {
            wire_color(ui)
        };
        let mid = bezier(
            &painter,
            from,
            to,
            Stroke::new(if selected { 3.0 } else { 2.0 }, color),
        );
        let mut label = e.label.clone();
        if e.is_loop {
            label = format!("{} {label}", icons::ARROWS_CLOCKWISE);
        }
        if !label.trim().is_empty() {
            painter.text(
                mid + Vec2::new(0.0, -10.0),
                Align2::CENTER_BOTTOM,
                label.trim(),
                FontId::monospace(10.0),
                if selected || touches {
                    p.gold_text
                } else {
                    p.muted
                },
            );
        }
        let hit = ui.interact(
            Rect::from_center_size(mid, Vec2::splat(12.0)),
            egui::Id::unique(("wf-edge", i)),
            Sense::click(),
        );
        painter.circle_filled(mid, if hit.hovered() { 5.0 } else { 3.5 }, color);
        if hit.clicked() {
            clicked_edge = Some(i);
        }
    }
    if let Some(i) = clicked_edge {
        ed.sel = Sel::Edge(i);
    }

    // Nodes.
    let mut select = None;
    let mut new_link: Option<(String, String)> = None;
    let mut drop_at: Option<Pos2> = None;
    let ids: Vec<String> = ed.graph.nodes.iter().map(|n| n.id.clone()).collect();
    for id in &ids {
        let Some(n) = ed.graph.node(id).cloned() else {
            continue;
        };
        let r = node_rect(origin, &n);
        let body = ui.interact(
            r,
            egui::Id::unique(("wf-node", id)),
            Sense::click_and_drag(),
        );
        if body.dragged()
            && let Some(nm) = ed.graph.node_mut(id)
        {
            let pos = nm.position.get_or_insert_default();
            pos.x += body.drag_delta().x;
            pos.y += body.drag_delta().y;
        }
        if body.clicked() || body.drag_started() {
            select = Some(id.clone());
        }
        let selected = ed.sel == Sel::Node(id.clone());
        let border = if bad.contains(id.as_str()) {
            Stroke::new(2.0, p.red)
        } else if selected {
            Stroke::new(1.5, p.gold)
        } else if body.hovered() {
            Stroke::new(1.0, p.gold_edge)
        } else {
            Stroke::new(1.0, p.line)
        };
        draw_node(&painter, ui, r, &n, border, None, selected);
        for o in out_ports(&n) {
            let pt = out_port_pos(r, &n, &o);
            let port = ui.interact(
                Rect::from_center_size(pt, Vec2::splat(14.0)),
                egui::Id::unique(("wf-port", id, &o)),
                Sense::drag(),
            );
            if port.drag_started() {
                new_link = Some((id.clone(), o.clone()));
            }
            if port.drag_stopped() {
                drop_at = ui.input(|i| i.pointer.interact_pos());
            }
        }
    }
    if let Some(s) = select {
        ed.sel = Sel::Node(s);
    }
    if let Some(l) = new_link {
        ed.link = Some(l);
    }
    if let Some((from, outcome)) = ed.link.clone() {
        if let (Some(a), Some(pt)) = (ed.graph.node(&from), ui.input(|i| i.pointer.interact_pos()))
        {
            let start = out_port_pos(node_rect(origin, a), a, &outcome);
            bezier(&painter, start, pt, Stroke::new(2.0, p.gold));
        }
        if let Some(pt) = drop_at {
            let target = ed
                .graph
                .nodes
                .iter()
                .find(|n| node_rect(origin, n).contains(pt))
                .map(|n| (n.id.clone(), n.kind.clone()));
            if let Some((to, kind)) = target
                && to != from
                && !ed
                    .graph
                    .edges
                    .iter()
                    .any(|e| e.from == from && e.to == to && e.outcome == outcome)
            {
                let from_kind = ed
                    .graph
                    .node(&from)
                    .map(|n| n.kind.clone())
                    .unwrap_or_default();
                ed.graph.edges.push(Edge {
                    from: from.clone(),
                    to,
                    outcome: outcome.clone(),
                    is_loop: from_kind == "gate" && outcome == g::OUTCOME_FAIL && kind == "crew",
                    ..Default::default()
                });
                ed.sel = Sel::Edge(ed.graph.edges.len() - 1);
            }
            ed.link = None;
        }
        if !ui.input(|i| i.pointer.any_down()) && drop_at.is_none() {
            ed.link = None;
        }
    }

    if ed.graph.nodes.is_empty() {
        painter.text(
            resp.rect.center(),
            Align2::CENTER_CENTER,
            icons::label(icons::LIGHTNING, "Add a trigger"),
            FontId::proportional(14.0),
            p.muted,
        );
    }
    painter.text(
        resp.rect.left_bottom() + Vec2::new(10.0, -8.0),
        Align2::LEFT_BOTTOM,
        "PORT DRAG: CONNECT · DRAG/SCROLL: PAN · DEL: REMOVE",
        FontId::monospace(10.0),
        p.muted,
    );

    // Delete the selection when no text box has focus.
    let typing = ui.ctx().memory(|m| m.focused().is_some());
    if !typing
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
    {
        match ed.sel.clone() {
            Sel::Node(id) => ed.graph.remove_node(&id),
            Sel::Edge(i) if i < ed.graph.edges.len() => {
                ed.graph.edges.remove(i);
            }
            _ => {}
        }
        ed.sel = Sel::None;
    }
}

/// A read-only picture of a graph (template preview, run trace). `status`
/// colours steps by their latest status.
pub(crate) fn preview(
    ui: &mut egui::Ui,
    graph: &Graph,
    status: &HashMap<String, String>,
    height: f32,
) -> Option<String> {
    let p = palette(ui.ctx());
    let mut graph = graph.clone();
    layout_missing(&mut graph);
    let (min, max) = graph.nodes.iter().fold(
        (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
        |(lo, hi), n| {
            let pos = n.position.unwrap_or_default();
            let v = Vec2::new(pos.x, pos.y);
            (lo.min(v), hi.max(v + node_size(n)))
        },
    );
    if graph.nodes.is_empty() {
        return None;
    }
    let size = Vec2::new(ui.available_width(), height);
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    painter.rect_filled(resp.rect, 0.0, p.bg);
    dot_grid(&painter, resp.rect, Vec2::ZERO, p.line);
    painter.rect_stroke(resp.rect, 0.0, Stroke::new(1.0, p.line), StrokeKind::Inside);
    let span = (max - min).max(Vec2::splat(1.0));
    let scale = ((size.x - 20.0) / span.x)
        .min((size.y - 20.0) / span.y)
        .min(1.0);
    let to_screen =
        |pos: Pos| resp.rect.min + Vec2::new(10.0, 10.0) + (Vec2::new(pos.x, pos.y) - min) * scale;
    let rect_of = |n: &Node| {
        Rect::from_min_size(
            to_screen(n.position.unwrap_or_default()),
            node_size(n) * scale,
        )
    };
    for e in &graph.edges {
        let (Some(a), Some(b)) = (graph.node(&e.from), graph.node(&e.to)) else {
            continue;
        };
        let ra = rect_of(a);
        let rb = rect_of(b);
        let color = if e.is_loop { p.amber } else { wire_color(ui) };
        bezier(
            &painter,
            out_port_pos_scaled(ra, a, &e.outcome, scale),
            in_port_pos(rb),
            Stroke::new(1.5, color),
        );
    }
    let mut hovered = None;
    for n in &graph.nodes {
        let r = rect_of(n);
        let st = status.get(&n.id).map(String::as_str);
        let border = match st {
            Some(s) => Stroke::new(2.0, status_color(ui, s)),
            None => Stroke::new(1.0, p.line),
        };
        if scale > 0.9 {
            draw_node(&painter, ui, r, n, border, st, false);
        } else {
            painter.rect_filled(r, 0.0, p.panel);
            painter.rect_filled(
                Rect::from_min_size(r.min, Vec2::new(r.width(), (NODE_STRIP * scale).max(2.0))),
                0.0,
                type_color(ui, &n.kind),
            );
            painter.rect_stroke(r, 0.0, border, StrokeKind::Inside);
            text_fit(
                &painter,
                r.left_center() + Vec2::new(6.0, 0.0),
                Align2::LEFT_CENTER,
                &n.title,
                FontId::proportional((12.0 * scale).clamp(9.0, 12.0)),
                p.text,
                r.width() - 10.0,
            );
        }
        if resp.hovered()
            && ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pt| r.contains(pt))
        {
            hovered = Some(n.id.clone());
        }
    }
    hovered
}

fn out_port_pos_scaled(r: Rect, n: &Node, outcome: &str, scale: f32) -> Pos2 {
    let ports = out_ports(n);
    let i = ports.iter().position(|o| o == outcome).unwrap_or(0) as f32;
    let k = ports.len().max(1) as f32;
    let body_top = r.top() + (NODE_STRIP + NODE_HEAD) * scale;
    let centre = (body_top + r.bottom()) / 2.0;
    let top = centre - (k - 1.0) * PORT_GAP * scale / 2.0;
    Pos2::new(r.right(), top + i * PORT_GAP * scale)
}

/// Models (`provider/model`) and node ids the crew inspector suggests,
/// refreshed by the screen from the vault and the fleet.
static PICKERS: std::sync::Mutex<(Vec<String>, Vec<String>)> =
    std::sync::Mutex::new((Vec::new(), Vec::new()));

fn pickers() -> (Vec<String>, Vec<String>) {
    PICKERS.lock().map(|p| p.clone()).unwrap_or_default()
}

fn set_pickers(models: Vec<String>, nodes: Vec<String>) {
    if let Ok(mut p) = PICKERS.lock() {
        *p = (models, nodes);
    }
}

/// Why a workflow cannot start for want of a model: a model step names a
/// provider the factory may not use, or no provider is open to the factory.
fn model_block(g: &Graph, factory_providers: &[String]) -> Option<String> {
    let needs = g
        .nodes
        .iter()
        .filter(|n| matches!(n.kind.as_str(), "crew" | "decision"));
    let mut any = false;
    for n in needs {
        any = true;
        if let Some((p, _)) = n.str("model").split_once('/')
            && !factory_providers.iter().any(|x| x == p)
        {
            return Some(format!("{p} is not open to the factory"));
        }
    }
    (any && factory_providers.is_empty()).then(|| "no provider is open to the factory".to_string())
}

/// `12k`, `1.2M`.
fn short_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Puts a human approval between `id` and every step after it.
fn insert_approval_after(g: &mut Graph, id: &str) {
    let hid = g.next_id();
    let pos = g
        .node(id)
        .and_then(|n| n.position)
        .map(|p| kls_engine::graph::Pos {
            x: p.x + 40.0,
            y: p.y + 110.0,
        });
    let mut h = kls_engine::graph::Node::new(&hid, "human", "Approve");
    h.config.insert("approvers".into(), json!(["owner"]));
    h.config.insert("slaHours".into(), json!(24));
    h.config
        .insert("onTimeout".into(), json!("park_and_renotify"));
    h.position = pos;
    // Outcome edges (conditions, decisions, gates) keep their routes.
    for e in g
        .edges
        .iter_mut()
        .filter(|e| e.from == id && e.outcome.is_empty() && !e.is_loop)
    {
        e.from = hid.clone();
    }
    g.nodes.push(h);
    g.edges.push(kls_engine::graph::Edge {
        from: id.to_string(),
        to: hid,
        ..Default::default()
    });
}

#[cfg(test)]
mod parity_ui_tests {
    use super::*;

    #[test]
    fn start_field_editors_write_what_the_engine_reads() {
        use kls_core::sessions::start::StartFields;
        let mut cfg = Map::new();
        set_dirs(&mut cfg, "/srv/a\n /srv/b ,");
        assert_eq!(dirs_text(&cfg), "/srv/a\n/srv/b");
        cfg.insert("effort".into(), json!("high"));
        cfg.insert("gitSync".into(), json!(false));
        let f = StartFields::from_config(&cfg).unwrap();
        assert_eq!(f.dirs, vec!["/srv/a", "/srv/b"]);
        assert!(f.validate("session").is_ok());
        // Session-only keys are refused on a worker; the form shows why.
        assert!(f.validate("worker").is_err());
        let mut w = Map::new();
        w.insert("stay".into(), json!(true));
        w.insert("stayIdleMaxS".into(), json!(600));
        w.insert("clone".into(), json!("acme/api@main"));
        set_mcp(
            &mut w,
            &[["gh".into(), "GITHUB_TOKEN".into(), "GH_PAT".into()]],
        );
        let f = StartFields::from_config(&w).unwrap();
        assert_eq!(f.mcp.len(), 1);
        assert_eq!(f.clone.as_ref().unwrap().branch, "main");
        assert!(f.validate("worker").is_ok());
        set_dirs(&mut w, "  ");
        set_mcp(&mut w, &[]);
        assert!(!w.contains_key("dirs") && !w.contains_key("mcp"));
    }

    #[test]
    fn crew_member_models_list_the_crew_then_named_agents() {
        let own: Map<String, Value> =
            serde_json::from_value(json!({"belai:builder": "a/b", "extra": "c/d"})).unwrap();
        assert_eq!(
            crew_member_names("belai:feature", Some(&own)),
            ["belai:planner", "belai:builder", "belai:reviewer", "extra"]
        );
        assert_eq!(crew_member_names("feature", None).len(), 3, "bare name");
        assert!(crew_member_names("nope", None).is_empty());
    }

    #[test]
    fn limit_rows_cover_every_per_card_limit() {
        let mut l = kls_engine::graph::Limits::default();
        for (v, _, lo, hi, def) in limit_rows(&mut l) {
            assert!(lo <= def && def <= hi);
            *v = Some(def);
        }
        assert_eq!(
            (
                l.finalize_at_pct,
                l.max_attempts_per_card,
                l.max_continuations,
                l.max_passes,
                l.max_wall_minutes,
                l.max_iterations
            ),
            (Some(80), Some(3), Some(3), Some(4), Some(60), Some(50))
        );
        let labels: Vec<&str> = limit_rows(&mut l).iter().map(|r| r.1).collect();
        assert!(labels.contains(&"Tool rounds"));
    }

    #[test]
    fn approval_goes_between_a_step_and_the_next() {
        let mut g = Graph::starter();
        let mut n = kls_engine::graph::Node::new("n2", "notify", "Say");
        n.config.insert("message".into(), json!("hi"));
        g.nodes.push(n);
        g.edges.push(kls_engine::graph::Edge {
            from: "n1".into(),
            to: "n2".into(),
            ..Default::default()
        });
        insert_approval_after(&mut g, "n1");
        let h = g
            .nodes
            .iter()
            .find(|n| n.kind == "human")
            .unwrap()
            .id
            .clone();
        assert!(g.edges.iter().any(|e| e.from == "n1" && e.to == h));
        assert!(g.edges.iter().any(|e| e.from == h && e.to == "n2"));
        assert_eq!(short_tokens(12_500), "12k");
    }

    #[test]
    fn blocked_without_a_factory_provider() {
        let mut g = Graph::starter();
        let mut n = kls_engine::graph::Node::new("n2", "crew", "Build");
        n.config.insert("model".into(), json!("acme/m1"));
        g.nodes.push(n);
        assert!(
            model_block(&g, &["anthropic".into()])
                .unwrap()
                .contains("acme")
        );
        assert!(model_block(&g, &["acme".into()]).is_none());
        assert!(
            model_block(&Graph::starter(), &[]).is_none(),
            "no model step, nothing blocks"
        );
    }
}
