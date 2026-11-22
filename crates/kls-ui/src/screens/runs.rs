//! Runs screen: the runs table with filters and a run's trace (graph,
//! steps, decisions, approvals; pause, resume, cancel, retry). Owner: M3.

use crate::fmt;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::screens::workflows::{self, status_color};
use crate::theme::{self, palette};
use crate::{icons, kit};
use egui::{RichText, Stroke, Vec2};
use egui_extras::{Column, TableBuilder};
use kls_engine::ops::{self, RunDetail};
use kls_store::engine::{Run, RunFilter, RunStep};
use kls_store::now_ms;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

#[cfg(test)]
const STATUSES: [&str; 8] = [
    "held",
    "queued",
    "running",
    "waiting_approval",
    "paused",
    "succeeded",
    "failed",
    "cancelled",
];
const ACTIVE: [&str; 5] = ["held", "queued", "running", "waiting_approval", "paused"];
const TRIGGERS: [&str; 6] = ["manual", "cron", "kanban", "event", "webhook", "label"];

#[derive(Clone, Default)]
struct ListData {
    runs: Arc<Vec<Run>>,
    names: Arc<HashMap<String, String>>,
    /// Why the kill switch or a window holds new work, if it does.
    held: Option<String>,
    /// Run id to the models its sessions used.
    models: Arc<HashMap<String, String>>,
}

pub struct Screen {
    filter: Arc<Mutex<RunFilter>>,
    q: String,
    list: Loader<ListData>,
    selected: Arc<Mutex<Option<String>>>,
    detail: Loader<Option<Arc<RunDetail>>>,
    note: String,
    open_step: Option<String>,
    /// The run whose Cancel waits for a second click.
    confirm_cancel: Option<String>,
    estimate: Loader<Option<kls_core::estimates::Estimate>>,
    /// Delivery, agent and verified for the selected run (`run_axes`).
    axes: Loader<String>,
}

fn duration(r: &Run) -> String {
    let Some(s) = r.started_at else {
        return String::new();
    };
    let ms = r.finished_at.unwrap_or_else(now_ms) - s;
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

/// The icon for a run, step or approval status.
pub(crate) fn status_icon(status: &str) -> &'static str {
    match status {
        "succeeded" | "approved" | "done" | "ok" => icons::CHECK,
        "failed" | "rejected" | "error" => icons::X,
        "expired" => icons::TIMER,
        "running" => icons::PLAY,
        "pending" | "waiting" | "waiting_approval" | "queued" => icons::CLOCK,
        "paused" => icons::PAUSE,
        "held" => icons::HOURGLASS,
        "cancelled" | "skipped" => icons::PROHIBIT,
        _ => icons::CIRCLE,
    }
}

/// A status pill: outline and text in the status colour.
pub(crate) fn status_chip(ui: &mut egui::Ui, status: &str) -> egui::Response {
    let c = status_color(ui, status);
    kit::chip(
        ui,
        &icons::label(status_icon(status), &status.replace('_', " ")),
        c,
    )
}

/// The quick status filters: label and the statuses they select.
fn status_filters() -> Vec<(&'static str, Vec<String>)> {
    let one = |s: &str| vec![s.to_string()];
    vec![
        ("All", Vec::new()),
        ("Active", ACTIVE.map(String::from).to_vec()),
        ("Running", one("running")),
        ("Waiting", one("waiting_approval")),
        ("Queued", one("queued")),
        ("Held", one("held")),
        ("Paused", one("paused")),
        ("Done", one("succeeded")),
        ("Failed", one("failed")),
        ("Cancelled", one("cancelled")),
    ]
}

/// The reason shown on a list row: a held run's window (`held: ` dropped,
/// cut to 40 characters).
fn row_reason(r: &Run) -> Option<String> {
    if r.status != "held" || r.reason.trim().is_empty() {
        return None;
    }
    let why = r.reason.trim();
    let why = why.strip_prefix("held:").unwrap_or(why).trim();
    let mut s: String = why.chars().take(40).collect();
    if why.chars().count() > 40 {
        s.push('…');
    }
    Some(s)
}

/// "10:04:41" from a timestamp.
fn time_of(ms: i64) -> String {
    fmt::when(ms).get(11..).unwrap_or_default().to_string()
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let filter = Arc::new(Mutex::new(RunFilter {
            limit: 300,
            ..Default::default()
        }));
        let f = filter.clone();
        let list = Loader::new(model, &["run.", "workflow.", "factory."], move |core| {
            let f = f.lock().unwrap().clone();
            async move {
                let names: HashMap<String, String> = core
                    .store()
                    .workflows(true)
                    .await?
                    .into_iter()
                    .map(|w| (w.id, w.name))
                    .collect();
                let mut models: HashMap<String, String> = HashMap::new();
                for s in core
                    .store()
                    .sessions(&kls_store::sessions::SessionQuery {
                        limit: 1000,
                        ..Default::default()
                    })
                    .await?
                    .into_iter()
                    .filter(|s| !s.run_id.is_empty() && !s.model.is_empty())
                {
                    let e = models.entry(s.run_id).or_default();
                    if !e.split(", ").any(|m| m == s.model) {
                        if !e.is_empty() {
                            e.push_str(", ");
                        }
                        e.push_str(&s.model);
                    }
                }
                Ok(ListData {
                    runs: Arc::new(core.store().runs(&f).await?),
                    names: Arc::new(names),
                    held: kls_engine::guard::halted(&core).await,
                    models: Arc::new(models),
                })
            }
        });
        let selected: Arc<Mutex<Option<String>>> = Arc::default();
        let sel = selected.clone();
        let sel2 = sel.clone();
        let estimate = Loader::new(
            model,
            &["run.finished", "session.ended", "settings."],
            move |core| {
                let id = sel2.lock().unwrap().clone();
                async move {
                    match id {
                        Some(id) => Ok(kls_core::estimates::for_run(&core, &id).await.ok()),
                        None => Ok(None),
                    }
                }
            },
        );
        let sel3 = selected.clone();
        let axes = Loader::new(model, &["run.", "dispatch."], move |core| {
            let id = sel3.lock().unwrap().clone();
            async move {
                let Some(id) = id else {
                    return Ok(String::new());
                };
                let store = core.store();
                let Some(run) = store.run(&id).await? else {
                    return Ok(String::new());
                };
                let steps = store.run_steps(&id).await?;
                let mut ds = Vec::new();
                for s in &steps {
                    if let Some(d) = s.dispatch_id.as_deref()
                        && let Some(d) = store.dispatch(d).await?
                    {
                        ds.push(d);
                    }
                }
                Ok(kls_core::sessions::axes::run_axes(&run, &steps, &ds).label())
            }
        });
        let detail = Loader::new(model, &["run.", "approval."], move |core| {
            let id = sel.lock().unwrap().clone();
            async move {
                match id {
                    Some(id) => Ok(ops::run_detail(&core, &id).await?.map(Arc::new)),
                    None => Ok(None),
                }
            }
        });
        Self {
            filter,
            q: String::new(),
            list,
            selected,
            detail,
            note: String::new(),
            open_step: None,
            confirm_cancel: None,
            estimate,
            axes,
        }
    }

    pub fn focus(&mut self, id: &str) {
        self.select(Some(id.to_string()));
    }

    fn select(&mut self, id: Option<String>) {
        *self.selected.lock().unwrap() = id;
        self.open_step = None;
        self.detail.reload();
        self.estimate.reload();
        self.axes.reload();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        kit::page_header(ui, icons::PLAY_CIRCLE, "Runs", |_| {});
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
        if let Some(why) = &data.held {
            kit::chip(
                ui,
                &icons::label(
                    icons::PAUSE_CIRCLE,
                    &format!("Held: {}", why.trim_start_matches("kill_switch:")),
                ),
                palette(ui.ctx()).amber,
            )
            .on_hover_text("Open runs are held; nothing new starts until the switch lifts");
        }
        run_totals(ui, &data.runs);
        self.filters_ui(ui, &data);
        ui.add_space(10.0);
        let has_sel = self.selected.lock().unwrap().is_some();
        // The table over the trace, as on the artboard; the table keeps
        // about two fifths of the height while a run is open.
        let table_h = if has_sel {
            (ui.available_height() * 0.4).max(160.0)
        } else {
            ui.available_height()
        };
        kit::card(ui)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.table_ui(ui, &data, table_h - 36.0);
                list_totals(ui, &data.runs);
            });
        if has_sel {
            ui.add_space(16.0);
            egui::ScrollArea::vertical()
                .id_salt("run_trace_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| self.trace_ui(ui, model));
        }
    }

    fn filters_ui(&mut self, ui: &mut egui::Ui, data: &ListData) {
        let mut changed = false;
        let mut f = self.filter.lock().unwrap();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut matched = false;
            for (label, statuses) in status_filters() {
                let on = !matched && f.statuses == statuses;
                matched |= on;
                if kit::pill(ui, label, on).clicked() && !on {
                    f.statuses = statuses;
                    changed = true;
                }
            }
            if !matched {
                // A status set from elsewhere: show it so it can be cleared.
                let label = f.statuses.join(", ");
                if kit::pill(ui, &label, true).on_hover_text("Clear").clicked() {
                    f.statuses.clear();
                    changed = true;
                }
            }
            ui.add_space(10.0);
            let wf = f.workflow_id.clone();
            let wf_name = wf
                .as_ref()
                .and_then(|w| data.names.get(w))
                .cloned()
                .unwrap_or_else(|| "Any workflow".into());
            egui::ComboBox::from_id_salt("runs-wf")
                .selected_text(icons::label(icons::TREE_STRUCTURE, &wf_name))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(wf.is_none(), "Any workflow").clicked() {
                        f.workflow_id = None;
                        changed = true;
                    }
                    let mut names: Vec<(&String, &String)> = data.names.iter().collect();
                    names.sort_by(|a, b| a.1.cmp(b.1));
                    for (id, name) in names {
                        if ui
                            .selectable_label(wf.as_deref() == Some(id), name)
                            .clicked()
                        {
                            f.workflow_id = Some(id.clone());
                            changed = true;
                        }
                    }
                });
            let trig = f.trigger_kind.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("runs-trigger")
                .selected_text(icons::label(
                    icons::LIGHTNING,
                    if trig.is_empty() {
                        "Any trigger"
                    } else {
                        trig.as_str()
                    },
                ))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(trig.is_empty(), "Any trigger")
                        .clicked()
                    {
                        f.trigger_kind = None;
                        changed = true;
                    }
                    for t in TRIGGERS {
                        if ui.selectable_label(trig == t, t).clicked() {
                            f.trigger_kind = Some(t.to_string());
                            changed = true;
                        }
                    }
                });
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut self.q)
                        .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                        .desired_width(200.0),
                )
                .on_hover_text("Run id, repo, card, reason");
            if r.changed() {
                f.q = Some(self.q.trim().to_string()).filter(|s| !s.is_empty());
                changed = true;
            }
        });
        drop(f);
        if changed {
            self.list.reload();
        }
    }

    fn table_ui(&mut self, ui: &mut egui::Ui, data: &ListData, max_h: f32) {
        if data.runs.is_empty() {
            kit::empty(ui, icons::PLAY_CIRCLE, "No runs");
            return;
        }
        let p = palette(ui.ctx());
        let selected = self.selected.lock().unwrap().clone();
        let now = now_ms();
        let mut pick = None;
        TableBuilder::new(ui)
            .id_salt("runs_table")
            .striped(false)
            .resizable(true)
            .sense(egui::Sense::click())
            .max_scroll_height(max_h.max(80.0))
            .auto_shrink([false, true])
            .column(Column::auto().at_least(110.0))
            .column(Column::auto().at_least(180.0))
            .column(Column::auto().at_least(130.0))
            .column(Column::auto().at_least(120.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(100.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(120.0).clip(true))
            .column(Column::remainder().at_least(110.0).clip(true))
            .header(30.0, |mut h| {
                for t in [
                    "Run", "Workflow", "State", "Trigger", "Card", "Started", "Length", "Tokens",
                    "Repo", "Model",
                ] {
                    h.col(|ui| {
                        kit::th(ui, t);
                        kit::header_rule(ui);
                    });
                }
            })
            .body(|body| {
                body.rows(34.0, data.runs.len(), |mut row| {
                    let r = &data.runs[row.index()];
                    row.set_selected(selected.as_deref() == Some(r.id.as_str()));
                    row.col(|ui| {
                        ui.label(
                            RichText::new(kls_engine::exec::short_id(&r.id))
                                .monospace()
                                .color(p.cyan),
                        );
                    });
                    row.col(|ui| {
                        ui.label(
                            data.names
                                .get(&r.workflow_id)
                                .map(String::as_str)
                                .unwrap_or("?"),
                        );
                    });
                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            status_chip(ui, &r.status).on_hover_text(&r.reason);
                            if let Some(why) = row_reason(r) {
                                ui.label(RichText::new(why).size(12.0).color(p.muted))
                                    .on_hover_text(&r.reason);
                            }
                        });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&r.trigger_label).color(p.muted))
                            .on_hover_text(&r.trigger_kind);
                    });
                    row.col(|ui| {
                        let card = if r.card_id.is_empty() {
                            "—"
                        } else {
                            r.card_id.as_str()
                        };
                        ui.label(RichText::new(card).monospace());
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(fmt::ago(now, r.started_at.unwrap_or(r.created_at)))
                                .color(p.muted),
                        )
                        .on_hover_text(fmt::when(r.created_at));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(duration(r)).monospace());
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(r.tokens.to_string()).monospace());
                    });
                    row.col(|ui| {
                        let repo = if r.repo.is_empty() { "—" } else { &r.repo };
                        ui.label(RichText::new(repo).color(p.muted));
                    });
                    row.col(|ui| {
                        let m = data.models.get(&r.id).map(String::as_str).unwrap_or("—");
                        ui.label(RichText::new(m).monospace().size(12.0).color(p.muted))
                            .on_hover_text(m);
                    });
                    if row.response().clicked() {
                        pick = Some(r.id.clone());
                    }
                });
            });
        if let Some(id) = pick {
            self.select(Some(id));
        }
    }

    fn trace_ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let d = match self.detail.get() {
            Some(Ok(Some(d))) => d,
            Some(Ok(None)) => {
                kit::empty(ui, icons::WARNING, "Run gone");
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
        let want = self.selected.lock().unwrap().clone().unwrap_or_default();
        if d.run.id != want {
            ui.spinner();
            return;
        }
        let r = &d.run;
        let wf_name = d
            .workflow
            .as_ref()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let title = format!("{} · {wf_name}", kls_engine::exec::short_id(&r.id));
        let mut close = false;
        let mut confirm = self.confirm_cancel.clone();
        kit::section(
            ui,
            icons::PLAY_CIRCLE,
            &title,
            true,
            |ui| {
                // Right to left: close, cancel, retry, pause/resume, status.
                if ui.button(icons::X).on_hover_text("Close").clicked() {
                    close = true;
                }
                let id = r.id.clone();
                if d.can_cancel() {
                    if confirm.as_deref() == Some(id.as_str()) {
                        if kit::ghost_button(ui, &icons::label(icons::PLAY, "Keep running"))
                            .clicked()
                        {
                            confirm = None;
                        }
                        if kit::danger_button(ui, &icons::label(icons::STOP, "Yes, cancel"))
                            .clicked()
                        {
                            confirm = None;
                            model.act(move |m| async move {
                                ops::cancel_run(m.core(), &id, "cancelled by owner").await?;
                                Ok(None)
                            });
                        }
                    } else if kit::danger_button(ui, &icons::label(icons::STOP, "Cancel")).clicked()
                    {
                        confirm = Some(id);
                    }
                }
                let id = r.id.clone();
                if d.can_retry()
                    && kit::ghost_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Retry"))
                        .clicked()
                {
                    model.act(move |m| async move {
                        ops::retry_run(m.core(), &id, None).await?;
                        Ok(Some(Notice::Info("Retrying".into())))
                    });
                }
                let id = r.id.clone();
                if d.can_pause() {
                    if kit::ghost_button(ui, &icons::label(icons::PAUSE, "Pause")).clicked() {
                        model.act(move |m| async move {
                            ops::pause_run(m.core(), &id).await?;
                            Ok(None)
                        });
                    }
                } else if d.can_resume()
                    && kit::ghost_button(ui, &icons::label(icons::PLAY, "Resume")).clicked()
                {
                    model.act(move |m| async move {
                        ops::resume_run(m.core(), &id).await?;
                        Ok(None)
                    });
                }
                status_chip(ui, &r.status);
            },
            |ui| self.trace_body(ui, model, &d),
        );
        self.confirm_cancel = confirm;
        if close {
            *self.selected.lock().unwrap() = None;
        }
    }

    fn trace_body(&mut self, ui: &mut egui::Ui, model: &Model, d: &RunDetail) {
        let p = palette(ui.ctx());
        let r = &d.run;
        let wf_name = d
            .workflow
            .as_ref()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            if ui
                .link(icons::label(
                    icons::TREE_STRUCTURE,
                    &format!("{wf_name} v{}", r.workflow_version),
                ))
                .clicked()
            {
                model.go(crate::Screen::Workflows, Some(r.workflow_id.clone()));
            }
            let meta = |ui: &mut egui::Ui, icon: &str, text: &str, hover: &str| {
                ui.label(
                    RichText::new(icons::label(icon, text))
                        .monospace()
                        .size(12.0)
                        .color(p.muted),
                )
                .on_hover_text(hover);
            };
            meta(ui, icons::LIGHTNING, &r.trigger_label, "Trigger");
            meta(ui, icons::CALENDAR, &fmt::when(r.created_at), "Created");
            if !r.repo.is_empty() {
                meta(ui, icons::GIT_BRANCH, &r.repo, "Repository");
            }
            if !r.card_id.is_empty()
                && ui
                    .link(icons::label(icons::KANBAN, &r.card_id))
                    .on_hover_text("Card")
                    .clicked()
            {
                model.go(crate::Screen::Board, Some(r.card_id.clone()));
            }
            meta(ui, icons::CPU, &r.tokens.to_string(), "Tokens");
            meta(ui, icons::TIMER, &duration(r), "Duration");
            if let Some(Ok(a)) = self.axes.get()
                && !a.is_empty()
            {
                meta(ui, icons::SEAL_CHECK, &a, "Delivery · agent · verified");
            }
            let t = d.totals();
            if let Some(pct) = t.budget_pct {
                meta(ui, icons::GAUGE, &format!("{pct:.0}%"), "Run budget used");
            }
            if t.decisions > 0 {
                meta(ui, icons::BRAIN, &t.decisions.to_string(), "Decisions made");
            }
            if t.cards > 0 {
                meta(ui, icons::KANBAN, &t.cards.to_string(), "Routing cards");
            }
            if t.handoffs > 0 {
                meta(ui, icons::ARROW_RIGHT, &t.handoffs.to_string(), "Handoffs");
            }
            if kit::ghost_button(ui, icons::DOWNLOAD_SIMPLE)
                .on_hover_text("Write the evidence pack (JSON)")
                .clicked()
            {
                let id = r.id.clone();
                model.act(move |m| async move {
                    let p = ops::write_evidence(m.core(), &id).await?;
                    Ok(Some(Notice::Info(format!("Evidence: {}", p.display()))))
                });
            }
            if let Some(Ok(Some(e))) = self.estimate.get() {
                if let Some(c) = e.model_cost {
                    meta(ui, icons::COINS, &format!("${c:.2}"), "Model cost");
                }
                if e.by_hand_hours > 0.0 {
                    meta(
                        ui,
                        icons::USER,
                        &format!("{:.0} h", e.by_hand_hours),
                        "By hand",
                    );
                }
            }
        });
        if !r.reason.is_empty() {
            ui.label(RichText::new(icons::label(icons::INFO, &r.reason)).color(p.muted))
                .on_hover_text("Reason");
        }
        let facts = d.facts();
        ui.horizontal_wrapped(|ui| {
            for (icon, vals) in [
                (icons::GIT_BRANCH, &facts.repos),
                (icons::GIT_PULL_REQUEST, &facts.pulls),
                (icons::ROBOT, &facts.agents),
                (icons::CPU, &facts.models),
                (icons::HARD_DRIVES, &facts.nodes),
            ] {
                for v in vals {
                    kit::chip(ui, &icons::label(icon, v), p.muted);
                }
            }
        });
        ui.add_space(8.0);
        // Latest status per node on the graph.
        let mut latest: HashMap<String, (i64, String)> = HashMap::new();
        for s in &d.steps {
            let e = latest
                .entry(s.node_id.clone())
                .or_insert((0, String::new()));
            if s.seq >= e.0 {
                *e = (s.seq, s.status.clone());
            }
        }
        let status: HashMap<String, String> =
            latest.into_iter().map(|(k, (_, v))| (k, v)).collect();
        if let Some(n) = workflows::preview(ui, &d.graph, &status, 170.0) {
            ui.label(RichText::new(icons::label(icons::CARET_RIGHT, &n)).color(p.muted));
        }

        // Approvals waiting on you.
        let pending: Vec<_> = d
            .approvals
            .iter()
            .filter(|a| a.state == "pending")
            .collect();
        if !pending.is_empty() {
            ui.add_space(10.0);
            kit::glow(ui)
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(kit::caps(
                        &icons::label(icons::SEAL_CHECK, "Approval"),
                        p.gold_text,
                    ));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.note)
                            .hint_text(icons::label(icons::NOTE, "Note"))
                            .desired_width(f32::INFINITY),
                    )
                    .on_hover_text("Optional decision note");
                    for a in pending {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&a.title).strong());
                            if let Some(sla) = a.sla_at {
                                ui.label(
                                    RichText::new(icons::label(icons::TIMER, &fmt::when(sla)))
                                        .color(p.muted),
                                )
                                .on_hover_text("Due by");
                            }
                            let (id, note) = (a.id.clone(), self.note.clone());
                            if theme::primary_button(ui, &icons::label(icons::CHECK, "Approve"))
                                .clicked()
                            {
                                model.act(move |m| async move {
                                    ops::decide_approval(m.core(), &id, true, &note, None).await?;
                                    Ok(None)
                                });
                            }
                            let (id, note) = (a.id.clone(), self.note.clone());
                            if kit::danger_button(ui, &icons::label(icons::X, "Reject")).clicked() {
                                model.act(move |m| async move {
                                    ops::decide_approval(m.core(), &id, false, &note, None).await?;
                                    Ok(None)
                                });
                            }
                        });
                    }
                });
        }

        ui.add_space(12.0);
        ui.label(kit::caps(
            &icons::label(icons::LIST_CHECKS, "Steps"),
            p.muted,
        ));
        ui.add_space(6.0);
        if d.steps.is_empty() {
            kit::empty(ui, icons::LIST_CHECKS, "No steps yet");
        }
        let n = d.steps.len();
        for (i, s) in d.steps.iter().enumerate() {
            self.step_ui(ui, model, d, s, i + 1 == n);
        }

        let decided: Vec<_> = d
            .approvals
            .iter()
            .filter(|a| a.state != "pending")
            .collect();
        if !decided.is_empty() {
            ui.add_space(8.0);
            ui.label(kit::caps(
                &icons::label(icons::SEAL_CHECK, "Approvals"),
                p.muted,
            ));
            for a in decided {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&a.title);
                    status_chip(ui, &a.state);
                    if !a.decided_by.is_empty() {
                        ui.label(
                            RichText::new(icons::label(icons::USER, &a.decided_by)).color(p.muted),
                        );
                    }
                    if !a.note.is_empty() {
                        ui.label(format!("“{}”", a.note));
                    }
                });
            }
        }
    }

    /// One step on the timeline: a status ring (filled once finished) on a
    /// line to the next step, the title and time, a muted detail line, and
    /// the step's details when opened.
    fn step_ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        d: &RunDetail,
        s: &RunStep,
        last: bool,
    ) {
        let p = palette(ui.ctx());
        let r = &d.run;
        let open = self.open_step.as_deref() == Some(s.id.as_str());
        let color = status_color(ui, &s.status);
        let finished = matches!(
            s.status.as_str(),
            "succeeded" | "failed" | "cancelled" | "skipped" | "approved" | "rejected" | "expired"
        );
        let row = ui.horizontal_top(|ui| {
            let (marker, _) = ui.allocate_exact_size(Vec2::new(16.0, 18.0), egui::Sense::hover());
            let c = egui::Pos2::new(marker.center().x, marker.top() + 9.0);
            if finished {
                ui.painter().circle_filled(c, 6.0, color);
            } else {
                ui.painter()
                    .circle(c, 6.0, egui::Color32::TRANSPARENT, Stroke::new(2.0, color));
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let caret = if open {
                        icons::CARET_DOWN
                    } else {
                        icons::CARET_RIGHT
                    };
                    let head = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!("{caret} {}", s.title))
                                    .size(14.0)
                                    .strong(),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if head.clicked() {
                        self.open_step = if open { None } else { Some(s.id.clone()) };
                    }
                    status_chip(ui, &s.status);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let t = s.started_at.map(time_of).unwrap_or_default();
                        ui.label(RichText::new(t).monospace().size(12.0).color(p.muted));
                        if d.can_retry()
                            && matches!(s.status.as_str(), "failed" | "cancelled" | "succeeded")
                            && ui
                                .small_button(icons::ARROW_CLOCKWISE)
                                .on_hover_text("Retry from here")
                                .clicked()
                        {
                            let (id, sid) = (r.id.clone(), s.id.clone());
                            model.act(move |m| async move {
                                ops::retry_run(m.core(), &id, Some(&sid)).await?;
                                Ok(None)
                            });
                        }
                        if let Some(sid) = s.session_id.clone().filter(|x| !x.is_empty())
                            && ui
                                .small_button(icons::TERMINAL_WINDOW)
                                .on_hover_text("Open session")
                                .clicked()
                        {
                            model.go(crate::Screen::Sessions, Some(sid));
                        }
                    });
                });
                let mut detail = vec![workflows::type_label(&s.node_type).to_string()];
                if !s.route.is_empty() {
                    detail.push(format!("{} {}", icons::ARROW_RIGHT, s.route));
                }
                if s.attempt > 1 {
                    detail.push(format!("{} {}", icons::ARROWS_CLOCKWISE, s.attempt));
                }
                if s.tokens > 0 {
                    detail.push(format!("{} tokens", s.tokens));
                }
                if let Some(nr) = s.node_ref.as_ref().filter(|x| !x.is_empty()) {
                    detail.push(format!("{} {nr}", icons::HARD_DRIVES));
                }
                ui.label(RichText::new(detail.join(" · ")).size(13.0).color(p.muted));
                if !s.error.is_empty() {
                    ui.label(RichText::new(&s.error).size(13.0).color(p.red));
                }
                if open {
                    step_details(ui, s);
                }
                ui.add_space(12.0);
            });
            marker
        });
        if !last {
            let marker = row.inner;
            let x = marker.center().x;
            ui.painter().vline(
                x,
                egui::Rangef::new(marker.top() + 17.0, row.response.rect.bottom() + 2.0),
                Stroke::new(2.0, p.line),
            );
        }
    }
}

/// A step's timing, dispatch and details JSON.
fn step_details(ui: &mut egui::Ui, s: &RunStep) {
    let p = palette(ui.ctx());
    egui::Frame::new()
        .fill(p.bg)
        .stroke(Stroke::new(1.0, p.line))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if s.node_type == "decision"
                && let Ok(d) = serde_json::from_str::<serde_json::Value>(&s.details_json)
                && d.get("decision").is_some()
            {
                decision_card(ui, &d);
            }
            if let Some(t) = s.started_at {
                ui.label(
                    RichText::new(format!(
                        "{}  {}{}",
                        icons::CLOCK,
                        fmt::when(t),
                        s.finished_at
                            .map(|f| format!("  {}  {} ms", icons::TIMER, f - t))
                            .unwrap_or_default()
                    ))
                    .monospace()
                    .size(12.0)
                    .color(p.muted),
                );
            }
            if let Some(dp) = &s.dispatch_id {
                ui.label(
                    RichText::new(format!(
                        "{}  {dp}  {}  {}",
                        icons::PAPER_PLANE_TILT,
                        icons::HARD_DRIVES,
                        s.node_ref.clone().unwrap_or_default()
                    ))
                    .monospace()
                    .size(12.0)
                    .color(p.muted),
                )
                .on_hover_text("Dispatch · node");
            }
            let v: Value = serde_json::from_str(&s.details_json).unwrap_or(Value::Null);
            if let Value::Object(m) = &v {
                egui::Grid::new(("details", &s.id))
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (k, val) in m {
                            ui.label(kit::caps(k, p.muted));
                            let txt = match val {
                                Value::String(x) => x.clone(),
                                other => serde_json::to_string_pretty(other).unwrap_or_default(),
                            };
                            ui.label(RichText::new(txt).monospace());
                            ui.end_row();
                        }
                    });
            }
        });
}

/// A SystemOne decision: the question, the pick, confidence against the
/// threshold, what the engine did and who answered.
fn decision_card(ui: &mut egui::Ui, d: &serde_json::Value) {
    let p = palette(ui.ctx());
    let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| d.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    ui.label(RichText::new(icons::label(icons::BRAIN, &s("question"))).strong());
    let conf = f("confidence");
    let thr = f("threshold");
    ui.horizontal_wrapped(|ui| {
        kit::chip(
            ui,
            &s("outcome"),
            if s("why") == "chosen" {
                p.green
            } else {
                p.amber
            },
        );
        ui.label(RichText::new(format!("{:.0}% vs {:.0}%", conf * 100.0, thr * 100.0)).monospace())
            .on_hover_text("Confidence vs threshold (uncalibrated)");
        ui.label(RichText::new(s("why").replace('_', " ")).color(p.muted));
        if !s("model").is_empty() {
            ui.label(RichText::new(format!("{} · {}", s("provider"), s("model"))).color(p.muted));
        }
    });
    kit::meter(
        ui,
        conf.clamp(0.0, 1.0) as f32,
        if conf >= thr { p.green } else { p.amber },
    );
}

/// Totals over the loaded runs: today, live, failed today, tokens 7 days.
/// Totals of the listed runs: (count, succeeded, failed, tokens).
fn totals_of(runs: &[Run]) -> (usize, usize, usize, i64) {
    (
        runs.len(),
        runs.iter().filter(|r| r.status == "succeeded").count(),
        runs.iter().filter(|r| r.status == "failed").count(),
        runs.iter().map(|r| r.tokens).sum(),
    )
}

/// One line under the table: what the filtered list adds up to.
fn list_totals(ui: &mut egui::Ui, runs: &[Run]) {
    let p = palette(ui.ctx());
    let (n, ok, failed, tokens) = totals_of(runs);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        for (icon, text) in [
            (icons::LIST, format!("{n} runs")),
            (icons::CHECK, format!("{ok} succeeded")),
            (icons::X, format!("{failed} failed")),
            (icons::CPU, format!("{tokens} tokens")),
        ] {
            ui.label(
                RichText::new(icons::label(icon, &text))
                    .size(12.0)
                    .color(p.muted),
            );
        }
    });
}

fn run_totals(ui: &mut egui::Ui, runs: &[Run]) {
    let p = palette(ui.ctx());
    let now = now_ms();
    let day = now - 86_400_000;
    let week = now - 7 * 86_400_000;
    let today = runs.iter().filter(|r| r.created_at >= day).count();
    let live = runs
        .iter()
        .filter(|r| matches!(r.status.as_str(), "queued" | "running" | "waiting_approval"))
        .count();
    let failed = runs
        .iter()
        .filter(|r| r.created_at >= day && r.status == "failed")
        .count();
    let tokens: i64 = runs
        .iter()
        .filter(|r| r.created_at >= week)
        .map(|r| r.tokens)
        .sum();
    let tiles = [
        (icons::PLAY_CIRCLE, "Runs today", today.to_string(), None),
        (icons::PULSE, "Live", live.to_string(), None),
        (
            icons::WARNING,
            "Failed today",
            failed.to_string(),
            (failed > 0).then_some(p.red),
        ),
        (icons::CPU, "Tokens 7d", tokens.to_string(), None),
    ];
    kit::Grid::new(180.0).show(ui, tiles.len(), |ui, i, w| {
        let (icon, label, value, color) = &tiles[i];
        kit::kpi_tile(ui, w, icon, label, value, None, *color);
    });
    ui.add_space(10.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_filters_cover_every_status_once() {
        let f = status_filters();
        for s in STATUSES {
            let n = f.iter().filter(|(_, v)| v.len() == 1 && v[0] == s).count();
            assert_eq!(n, 1, "{s}");
        }
        assert!(f.iter().any(|(l, v)| *l == "All" && v.is_empty()));
        assert!(ACTIVE.contains(&"held"));
        assert_eq!(status_icon("held"), icons::HOURGLASS);
    }

    #[test]
    fn held_rows_show_their_window() {
        let r = |status: &str, reason: &str| Run {
            status: status.into(),
            reason: reason.into(),
            ..Default::default()
        };
        assert_eq!(
            row_reason(&r("held", "held: schedules.blackout")).as_deref(),
            Some("schedules.blackout")
        );
        assert_eq!(row_reason(&r("failed", "boom")), None);
        assert_eq!(row_reason(&r("held", " ")), None);
        let long = row_reason(&r("held", &"x".repeat(60))).unwrap();
        assert_eq!(long.chars().count(), 41);
    }

    #[test]
    fn list_totals_add_up() {
        let r = |status: &str, tokens: i64| Run {
            status: status.into(),
            tokens,
            ..Default::default()
        };
        let runs = [r("succeeded", 10), r("failed", 5), r("running", 1)];
        assert_eq!(totals_of(&runs), (3, 1, 1, 16));
    }
}
