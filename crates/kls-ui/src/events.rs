//! Events: the live factory event stream, newest first, with search, kind
//! facets, a per-minute activity strip and pause/resume.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::model::Snapshot;
use crate::theme::{Palette, palette};
use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};
use kls_store::Event;

/// Minutes shown in the activity strip.
const SPARK_MINUTES: usize = 40;
/// Kind prefixes offered as facets (when present).
const FACETS: [&str; 10] = [
    "run.",
    "session.",
    "card.",
    "node.",
    "github.",
    "approval.",
    "memory.",
    "budget.",
    "a2a.",
    "dispatch.",
];

/// The colour an event kind is shown in, by its prefix.
pub(crate) fn kind_color(p: &Palette, kind: &str) -> Color32 {
    let prefix = kind.split('.').next().unwrap_or("");
    if kind.contains("fail") || kind.contains("error") || kind.contains("refused") {
        return p.red;
    }
    match prefix {
        "run" | "schedule" | "approval" | "factory" => p.gold_text,
        "session" => p.cyan,
        "card" | "github" | "pull" => p.violet,
        "budget" | "conflict" => p.amber,
        "memory" | "dispatch" | "audit" | "workflow" => p.green,
        "a2a" => p.blue,
        "node" | "token" | "settings" => p.muted,
        _ => p.text,
    }
}

/// Wall-clock "HH:MM:SS" for recent times, the full date otherwise.
pub(crate) fn clock(now: i64, ms: i64) -> String {
    if now - ms < 20 * 3_600_000 {
        match jiff::Timestamp::from_millisecond(ms) {
            Ok(ts) => ts
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string(),
            Err(_) => "?".into(),
        }
    } else {
        fmt::when(ms)
    }
}

/// Event counts per minute for the last `SPARK_MINUTES` minutes, oldest first.
fn per_minute(events: &[Event], now: i64) -> Vec<usize> {
    let mut out = vec![0; SPARK_MINUTES];
    for e in events {
        let age = (now - e.at).max(0) / 60_000;
        if (age as usize) < SPARK_MINUTES {
            out[SPARK_MINUTES - 1 - age as usize] += 1;
        }
    }
    out
}

/// The row's detail text and up to two `key=value` facts from its data.
fn detail(data_json: &str) -> (String, Vec<String>) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(data_json)
    else {
        return (data_json.to_string(), Vec::new());
    };
    let scalar = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    let text_key = [
        "message", "title", "text", "summary", "reason", "detail", "name",
    ]
    .into_iter()
    .find(|k| map.get(*k).and_then(scalar).is_some_and(|s| !s.is_empty()));
    let text = match text_key {
        Some(k) => map.get(k).and_then(scalar).unwrap_or_default(),
        None if map.is_empty() => String::new(),
        // No message field: a compact summary instead of raw JSON. Clock
        // fields are left out (the When column has the time).
        None => {
            let parts: Vec<String> = map
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "at" | "atMs" | "at_ms" | "ts"))
                .filter_map(|(k, v)| match v {
                    serde_json::Value::Array(a) => Some(format!("{k}[{}]", a.len())),
                    serde_json::Value::Object(_) => None,
                    other => scalar(other).map(|s| format!("{k}={s}")),
                })
                .take(6)
                .collect();
            if parts.is_empty() {
                data_json.to_string()
            } else {
                parts.join(" · ")
            }
        }
    };
    let facts = if text_key.is_some() {
        map.iter()
            .filter(|(k, _)| Some(k.as_str()) != text_key)
            .filter_map(|(k, v)| scalar(v).map(|s| format!("{k}={s}")))
            .filter(|s| s.len() <= 40)
            .take(2)
            .collect()
    } else {
        Vec::new()
    };
    (text, facts)
}

/// Time range of the stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Range {
    #[default]
    Live,
    Hour,
    Day,
    Week,
}

impl Range {
    fn ms(self) -> Option<i64> {
        match self {
            Range::Live => None,
            Range::Hour => Some(3_600_000),
            Range::Day => Some(86_400_000),
            Range::Week => Some(7 * 86_400_000),
        }
    }
}

/// Saved Events views: JSON list of [name, filter].
const VIEWS_SETTING: &str = "events.views";
/// Saved views as they load: (name, filter) pairs.
type ViewsSlot = std::sync::Arc<std::sync::Mutex<Option<Vec<(String, String)>>>>;

/// A keleustes:// link that opens Events with `filter`.
pub(crate) fn view_link(filter: &str) -> String {
    let enc: String = filter
        .trim()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b':' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("keleustes://events?id={enc}")
}

/// Structured filter keys and the payload fields they match.
const FILTER_KEYS: [(&str, &[&str]); 8] = [
    ("run", &["runId", "run_id", "run"]),
    ("card", &["cardId", "card_id", "card", "key"]),
    ("repo", &["repo", "repository"]),
    ("node", &["nodeId", "node_id", "node"]),
    ("wf", &["workflowId", "workflow_id", "workflow"]),
    ("session", &["sessionId", "session_id", "session"]),
    ("budget", &["budgetId", "budget"]),
    ("agent", &["agent", "agentName"]),
];

/// Who or what caused an event, from its payload: `actor`, `by`,
/// `origin` (a string, or `{kind, actor|node|workflow}`), `caller`,
/// `nodeId`, else "" (unknown).
pub(crate) fn origin_of(data_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data_json) else {
        return String::new();
    };
    let s = |x: &serde_json::Value| x.as_str().map(str::trim).unwrap_or("").to_string();
    for k in ["actor", "by", "origin", "caller", "source"] {
        let Some(x) = v.get(k) else { continue };
        let out = match x {
            serde_json::Value::Object(o) => ["actor", "agent", "workflow", "node", "kind"]
                .iter()
                .find_map(|f| o.get(*f).map(s).filter(|t| !t.is_empty()))
                .unwrap_or_default(),
            other => s(other),
        };
        if !out.is_empty() {
            return out;
        }
    }
    ["nodeId", "node_id"]
        .iter()
        .find_map(|k| v.get(*k).map(s).filter(|t| !t.is_empty()))
        .map(|n| format!("node:{n}"))
        .unwrap_or_default()
}

/// The `key=value` facts of a row match (a fact filter).
fn fact_matches(data: &serde_json::Value, key: &str, value: &str) -> bool {
    data.get(key).is_some_and(|x| match x {
        serde_json::Value::String(s) => s.to_lowercase() == value,
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => false,
        o => o.to_string().to_lowercase() == value,
    })
}

/// The filter with `token` added (or removed when it is there already).
pub(crate) fn toggle_token(filter: &str, token: &str) -> String {
    let mut toks: Vec<&str> = filter.split_whitespace().collect();
    match toks.iter().position(|t| t.eq_ignore_ascii_case(token)) {
        Some(i) => {
            toks.remove(i);
        }
        None => toks.push(token),
    }
    toks.join(" ")
}

/// A parsed search: `key:value` filters (all must match) and free words.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Query {
    pub filters: Vec<(String, String)>,
    pub kind: Option<String>,
    pub needs: bool,
    /// `origin:<who>` (see [`origin_of`]).
    pub origin: Option<String>,
    /// `key=value` facts of the payload.
    pub facts: Vec<(String, String)>,
    pub words: Vec<String>,
}

pub(crate) fn parse_query(s: &str) -> Query {
    let mut q = Query::default();
    for tok in s.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=')
            && !k.is_empty()
            && !v.is_empty()
            && !k.contains(':')
        {
            q.facts.push((k.to_string(), v.to_lowercase()));
            continue;
        }
        match tok.split_once(':') {
            Some(("kind", v)) if !v.is_empty() => q.kind = Some(v.to_lowercase()),
            Some(("origin", v)) if !v.is_empty() => q.origin = Some(v.to_lowercase()),
            Some((k, v)) if !v.is_empty() && FILTER_KEYS.iter().any(|(n, _)| *n == k) => {
                q.filters.push((k.to_string(), v.to_lowercase()))
            }
            _ if tok == "needs" || tok == "needs:me" => q.needs = true,
            _ => q.words.push(tok.to_lowercase()),
        }
    }
    q
}

const NEEDS_KINDS: [&str; 6] = [
    "approval.requested",
    "approval.renotified",
    "session.question",
    "budget.blocked",
    "factory.refused",
    "a2a.withheld",
];

pub(crate) fn matches(q: &Query, e: &Event) -> bool {
    if q.kind
        .as_deref()
        .is_some_and(|k| !e.kind.to_lowercase().starts_with(k))
    {
        return false;
    }
    if q.needs && !NEEDS_KINDS.contains(&e.kind.as_str()) {
        return false;
    }
    if q.origin
        .as_deref()
        .is_some_and(|o| origin_of(&e.data_json).to_lowercase() != o)
    {
        return false;
    }
    let data: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
    if !q.facts.iter().all(|(k, v)| fact_matches(&data, k, v)) {
        return false;
    }
    let subject = e.subject.to_lowercase();
    for (k, v) in &q.filters {
        let fields = FILTER_KEYS
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, f)| *f)
            .unwrap_or(&[]);
        let in_data = fields.iter().any(|f| {
            data.get(*f)
                .map(|x| match x {
                    serde_json::Value::String(s) => s.to_lowercase() == *v,
                    o => format!("{o}") == *v,
                })
                .unwrap_or(false)
        });
        // The subject of the owning kind counts too (run.* subject is a run id).
        let own = e.kind.starts_with(match k.as_str() {
            "wf" => "workflow",
            other => other,
        }) && subject == *v;
        if !in_data && !own {
            return false;
        }
    }
    let hay = || format!("{} {} {}", e.kind, e.subject, e.data_json).to_lowercase();
    q.words.is_empty() || {
        let h = hay();
        q.words.iter().all(|w| h.contains(w.as_str()))
    }
}

type Slot = std::sync::Arc<std::sync::Mutex<Option<Result<Vec<Event>, String>>>>;

#[derive(Default)]
pub struct Events {
    filter: String,
    paused: Option<i64>,
    facet: Option<String>,
    range: Range,
    /// Events of a past range, newest first.
    history: Vec<Event>,
    /// Older events loaded below the live window, newest first.
    older: Vec<Event>,
    loading: Option<(Slot, bool)>,
    error: Option<String>,
    selected: Option<Event>,
    /// Saved views (name, filter), loaded once from `events.views`.
    saved_views: ViewsSlot,
    views_asked: bool,
    view_name: String,
}

impl Events {
    /// The saved views; the first call starts loading them.
    fn views(&mut self, model: &crate::Model) -> Vec<(String, String)> {
        if !self.views_asked {
            self.views_asked = true;
            let slot = self.saved_views.clone();
            let core = model.core().clone();
            let ctx = model.egui_ctx();
            model.rt.spawn(async move {
                let v = core
                    .store()
                    .setting(VIEWS_SETTING)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                *slot.lock().unwrap() = Some(v);
                ctx.request_repaint();
            });
        }
        self.saved_views.lock().unwrap().clone().unwrap_or_default()
    }

    fn save_views(&mut self, model: &crate::Model, views: Vec<(String, String)>) {
        *self.saved_views.lock().unwrap() = Some(views.clone());
        model.act(move |m| async move {
            m.core()
                .store()
                .set_setting(VIEWS_SETTING, &serde_json::to_string(&views)?)
                .await?;
            Ok(None)
        });
    }

    /// Opens the stream filtered: `run:<id>`, `card:K-1 repo:acme/api`,
    /// `kind:budget.`, `needs`, or plain words.
    pub fn focus(&mut self, filter: &str) {
        self.filter = filter.trim().to_string();
        self.facet = None;
        self.selected = None;
    }

    fn fetch(
        &mut self,
        model: &crate::Model,
        before: Option<i64>,
        from: Option<i64>,
        append: bool,
    ) {
        let slot: Slot = Default::default();
        let s2 = slot.clone();
        let core = model.core().clone();
        let ctx = model.egui_ctx();
        let prefixes = self.facet.iter().cloned().collect();
        model.rt.spawn(async move {
            let r = core
                .store()
                .events_query(&kls_store::audit::EventQuery {
                    prefixes,
                    before_id: before,
                    from_at: from,
                    limit: 1000,
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("{e:#}"));
            *s2.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        self.loading = Some((slot, append));
    }

    fn settle(&mut self) {
        let Some((slot, append)) = &self.loading else {
            return;
        };
        let append = *append;
        let done = slot.lock().unwrap().take();
        if let Some(r) = done {
            match r {
                Ok(evs) => {
                    let target = if self.range == Range::Live {
                        &mut self.older
                    } else {
                        &mut self.history
                    };
                    if append {
                        target.extend(evs);
                    } else {
                        *target = evs;
                    }
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
            self.loading = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &crate::Model, snap: &Snapshot) {
        self.settle();
        let p = palette(ui.ctx());
        let now = kls_store::now_ms();
        let last_minute = snap.events.iter().filter(|e| now - e.at < 60_000).count();
        let mut new_range = None;
        kit::page_header(ui, icons::PULSE, "Events", |ui| {
            for (r, label) in [
                (Range::Week, "7 d"),
                (Range::Day, "24 h"),
                (Range::Hour, "1 h"),
                (Range::Live, "Live"),
            ] {
                if kit::tab(ui, label, self.range == r).clicked() && self.range != r {
                    new_range = Some(r);
                }
            }
            ui.add_space(8.0);
            if self.range == Range::Live {
                let label = if self.paused.is_some() {
                    icons::label(icons::PLAY, "Resume")
                } else {
                    icons::label(icons::PAUSE, "Pause")
                };
                if kit::ghost_button(ui, &label).clicked() {
                    self.paused = match self.paused {
                        Some(_) => None,
                        None => snap.events.last().map(|e| e.id),
                    };
                }
                ui.add_space(8.0);
                let (text, c) = if self.paused.is_some() {
                    ("Paused".to_string(), p.muted)
                } else if snap.error.is_some() {
                    ("Stream error".to_string(), p.red)
                } else {
                    (format!("Live · {last_minute}/min"), p.green)
                };
                ui.label(RichText::new(text).size(13.0).color(p.muted));
                kit::dot(ui, c);
            } else if kit::ghost_button(ui, &icons::label(icons::ARROW_LEFT, "Back to live"))
                .clicked()
            {
                new_range = Some(Range::Live);
            }
        });
        if let Some(r) = new_range {
            self.range = r;
            self.history.clear();
            if let Some(ms) = r.ms() {
                self.fetch(model, None, Some(now - ms), false);
            }
        }

        // The rows: the live window (plus older pages) or a past range.
        let cutoff = self.paused.unwrap_or(i64::MAX);
        let owned: Vec<Event> = if self.range == Range::Live {
            snap.events
                .iter()
                .rev()
                .filter(|e| e.id <= cutoff)
                .chain(
                    self.older
                        .iter()
                        .filter(|o| snap.events.first().is_none_or(|f| o.id < f.id)),
                )
                .cloned()
                .collect()
        } else {
            self.history.clone()
        };
        let source: Vec<&Event> = owned.iter().collect();
        let q = parse_query(&self.filter);
        let mut facet_changed = false;
        let mut export_now = false;
        kit::card(ui)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    kit::field(ui, "Search", |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text(icons::label(icons::MAGNIFYING_GLASS, "run:… card:… repo:… needs"))
                                .desired_width(300.0_f32.max(ui.available_width() * 0.3)),
                        )
                        .on_hover_text("Words, or run: card: repo: node: wf: session: budget: agent: kind: needs");
                    });
                    if !self.filter.is_empty()
                        && kit::ghost_button(ui, icons::X).on_hover_text("Clear").clicked()
                    {
                        self.filter.clear();
                    }
                    // Saved views, export and a link to this view.
                    let views = self.views(model);
                    if !views.is_empty() {
                        egui::ComboBox::from_id_salt("event_views")
                            .selected_text(icons::label(icons::BOOKS, "Views"))
                            .show_ui(ui, |ui| {
                                for (name, f) in &views {
                                    if ui.selectable_label(self.filter == *f, name).clicked() {
                                        self.filter = f.clone();
                                    }
                                }
                            });
                    }
                    if !self.filter.trim().is_empty() {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.view_name)
                                .hint_text("view name")
                                .desired_width(110.0),
                        );
                        if kit::ghost_button(ui, icons::FLOPPY_DISK)
                            .on_hover_text("Save this view")
                            .clicked()
                            && !self.view_name.trim().is_empty()
                        {
                            let mut next: Vec<(String, String)> =
                                views.into_iter().filter(|(n, _)| n != self.view_name.trim()).collect();
                            next.push((self.view_name.trim().to_string(), self.filter.trim().to_string()));
                            self.save_views(model, next);
                            self.view_name.clear();
                        }
                        if let Some(pos) = self.views(model).iter().position(|(_, f)| *f == self.filter)
                            && kit::ghost_button(ui, icons::TRASH).on_hover_text("Delete this view").clicked()
                        {
                            let mut next = self.views(model);
                            next.remove(pos);
                            self.save_views(model, next);
                        }
                    }
                    if kit::ghost_button(ui, icons::LINK).on_hover_text("Copy a link to this view").clicked() {
                        ui.ctx().copy_text(view_link(&self.filter));
                    }
                    export_now = kit::ghost_button(ui, icons::DOWNLOAD_SIMPLE)
                        .on_hover_text("Export the rows shown as JSONL")
                        .clicked();
                    ui.add_space(6.0);
                    if kit::toggle_chip(ui, &format!("All {}", source.len()), self.facet.is_none()).clicked()
                    {
                        facet_changed = self.facet.is_some();
                        self.facet = None;
                    }
                    for f in FACETS {
                        let n = source.iter().filter(|e| e.kind.starts_with(f)).count();
                        if n == 0 && self.facet.as_deref() != Some(f) {
                            continue;
                        }
                        let on = self.facet.as_deref() == Some(f);
                        if kit::toggle_chip(ui, &format!("{f} {n}"), on).clicked() {
                            self.facet = if on { None } else { Some(f.to_string()) };
                            facet_changed = true;
                        }
                    }
                });
                // Origin chips: who caused the rows shown, most first.
                let mut origins: Vec<(String, usize)> = Vec::new();
                for e in &source {
                    let o = origin_of(&e.data_json);
                    if o.is_empty() || o.contains(char::is_whitespace) {
                        continue;
                    }
                    match origins.iter_mut().find(|(x, _)| *x == o) {
                        Some((_, n)) => *n += 1,
                        None => origins.push((o, 1)),
                    }
                }
                origins.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                if !origins.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(kit::caps(&icons::label(icons::USER, "Origin"), p.muted));
                        for (o, n) in origins.iter().take(10) {
                            let on = q.origin.as_deref() == Some(o.to_lowercase().as_str());
                            if kit::toggle_chip(ui, &format!("{o} {n}"), on).clicked() {
                                let tok = format!("origin:{o}");
                                let base: String = self
                                    .filter
                                    .split_whitespace()
                                    .filter(|t| !t.starts_with("origin:"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                self.filter = if on { base } else { toggle_token(&base, &tok) };
                            }
                        }
                    });
                }
                if !q.filters.is_empty()
                    || q.needs
                    || q.kind.is_some()
                    || q.origin.is_some()
                    || !q.facts.is_empty()
                {
                    ui.horizontal_wrapped(|ui| {
                        for (k, v) in &q.filters {
                            kit::chip(ui, &format!("{k}: {v}"), p.cyan);
                        }
                        if let Some(o) = &q.origin {
                            kit::chip(ui, &icons::label(icons::USER, o), p.cyan);
                        }
                        for (k, v) in &q.facts {
                            if kit::chip(ui, &format!("{k} = {v}  {}", icons::X), p.cyan)
                                .interact(egui::Sense::click())
                                .on_hover_text("Remove this fact")
                                .clicked()
                            {
                                self.filter = self
                                    .filter
                                    .split_whitespace()
                                    .filter(|t| !t.eq_ignore_ascii_case(&format!("{k}={v}")))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                            }
                        }
                        if let Some(k) = &q.kind {
                            kit::chip(ui, &format!("kind: {k}"), p.cyan);
                        }
                        if q.needs {
                            kit::chip(ui, &icons::label(icons::HAND_PALM, "needs a person"), p.gold_text);
                        }
                    });
                }
            });
        if facet_changed && let Some(ms) = self.range.ms() {
            self.fetch(model, None, Some(now - ms), false);
        }
        ui.add_space(12.0);

        if self.range == Range::Live {
            spark(ui, &p, &snap.events, now);
            ui.add_space(12.0);
            let buffered = snap.events.iter().filter(|e| e.id > cutoff).count();
            if buffered > 0 {
                kit::card(ui)
                    .stroke(egui::Stroke::new(1.0, p.gold_edge))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if kit::link(
                            ui,
                            &icons::label(icons::ARROW_UP, &format!("Show {buffered} new")),
                            p.gold_text,
                        )
                        .clicked()
                        {
                            self.paused = snap.events.last().map(|e| e.id);
                        }
                    });
                ui.add_space(12.0);
            }
        }
        if let Some(e) = &self.error {
            ui.colored_label(p.red, e);
        }

        let rows: Vec<&Event> = source
            .into_iter()
            .filter(|e| self.facet.as_deref().is_none_or(|x| e.kind.starts_with(x)))
            .filter(|e| matches(&q, e))
            .collect();
        if export_now {
            let lines: Vec<String> = rows
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id, "at": e.at, "kind": e.kind, "subject": e.subject,
                        "data": serde_json::from_str::<serde_json::Value>(&e.data_json).unwrap_or_default(),
                    })
                    .to_string()
                })
                .collect();
            let filter = self.filter.clone();
            model.act(move |m| async move {
                let dir = m.core().data_dir().join("exports");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("events-{}.jsonl", kls_store::now_ms()));
                std::fs::write(&path, lines.join("\n") + "\n")?;
                m.core()
                    .emit(
                        "audit.exported",
                        &path.display().to_string(),
                        &serde_json::json!({"entries": lines.len(), "what": "events", "filter": filter, "by": "owner"}),
                    )
                    .await?;
                Ok(Some(crate::model::Notice::Info(format!(
                    "{} events to {}",
                    lines.len(),
                    path.display()
                ))))
            });
        }
        let mut pick: Option<Event> = None;
        let mut add_fact: Option<String> = None;
        let mut add_origin: Option<String> = None;
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if rows.is_empty() {
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(icons::label(icons::PULSE, "No events")).color(p.muted),
                        );
                    });
            } else {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .striped(false)
                            .sense(egui::Sense::click())
                            .column(Column::auto().at_least(80.0))
                            .column(Column::auto().at_least(140.0))
                            .column(Column::auto().at_least(150.0).clip(true))
                            .column(Column::remainder().at_least(160.0).clip(true))
                            .column(Column::auto().at_least(120.0))
                            .column(Column::auto().at_least(90.0).clip(true))
                            .header(30.0, |mut h| {
                                for t in ["When", "Kind", "Subject", "Detail", "Facts", "Origin"] {
                                    h.col(|ui| {
                                        kit::rule(ui);
                                        kit::th(ui, t);
                                    });
                                }
                            })
                            .body(|body| {
                                body.rows(34.0, rows.len(), |mut row| {
                                    let e = rows[row.index()];
                                    let sel = self.selected.as_ref().is_some_and(|s| s.id == e.id);
                                    row.set_selected(sel);
                                    let (text, facts) = detail(&e.data_json);
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(&clock(now, e.at), 13.0, p.muted))
                                            .on_hover_text(fmt::when(e.at));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(&e.kind, 13.0, kind_color(&p, &e.kind)));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        let r = ui.add(
                                            egui::Label::new(
                                                RichText::new(&e.subject).size(13.0).color(p.cyan),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        );
                                        if let Some(screen) = target(&e.kind)
                                            && r.on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked()
                                        {
                                            model.go(screen, Some(e.subject.clone()));
                                        }
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.add(
                                            egui::Label::new(RichText::new(&text).size(13.0))
                                                .truncate(),
                                        );
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.horizontal(|ui| {
                                            for fact in &facts {
                                                if kit::pick_chip(ui, fact, false, p.muted)
                                                    .on_hover_text("Filter on this fact")
                                                    .clicked()
                                                {
                                                    add_fact = Some(fact.clone());
                                                }
                                            }
                                        });
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        let o = origin_of(&e.data_json);
                                        if !o.is_empty()
                                            && kit::pick_chip(ui, &o, false, p.cyan)
                                                .on_hover_text("Events from this origin")
                                                .clicked()
                                        {
                                            add_origin = Some(o);
                                        }
                                    });
                                    if row.response().clicked() {
                                        pick = Some(e.clone());
                                    }
                                });
                            });
                    });
            }
            ui.horizontal(|ui| {
                let oldest = rows.last().map(|e| e.id);
                if self.loading.is_some() {
                    ui.spinner();
                } else if let Some(id) = oldest
                    && kit::ghost_button(ui, &icons::label(icons::ARROW_DOWN, "Load older"))
                        .clicked()
                {
                    let from = self.range.ms().map(|ms| now - ms);
                    self.fetch(model, Some(id), from, true);
                }
                ui.label(
                    RichText::new(icons::label(
                        icons::INFO,
                        "Kept 30 days; the audit ledger keeps the record",
                    ))
                    .size(12.0)
                    .color(p.muted),
                );
                if kit::link(ui, &icons::label(icons::SCROLL, "Audit"), p.cyan).clicked() {
                    model.go(crate::Screen::Audit, None);
                }
            });
        });
        if let Some(e) = pick {
            self.selected = Some(e);
        }
        if let Some(f) = add_fact.filter(|f| !f.contains(char::is_whitespace)) {
            self.filter = toggle_token(&self.filter, &f);
        }
        if let Some(o) = add_origin.filter(|o| !o.contains(char::is_whitespace)) {
            self.filter = toggle_token(&self.filter, &format!("origin:{o}"));
        }
        self.detail_window(ui.ctx(), model);
    }

    /// The selected event: kind, time, subject, every fact, links and the
    /// payload.
    fn detail_window(&mut self, ctx: &egui::Context, model: &crate::Model) {
        let Some(e) = self.selected.clone() else {
            return;
        };
        let p = palette(ctx);
        let mut open = true;
        let data: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
        egui::Window::new(icons::label(icons::PULSE, &e.kind))
            .open(&mut open)
            .default_width(460.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(kit::mono(
                    &format!("#{} · {}", e.id, fmt::when(e.at)),
                    12.0,
                    p.muted,
                ));
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&e.subject).color(p.cyan));
                    if let Some(screen) = target(&e.kind)
                        && kit::ghost_button(
                            ui,
                            &icons::label(icons::ARROW_SQUARE_OUT, screen.title()),
                        )
                        .clicked()
                    {
                        model.go(screen, Some(e.subject.clone()));
                    }
                    if !e.kind.starts_with("workflow.")
                        && !e.kind.starts_with("engine.")
                        && kit::ghost_button(
                            ui,
                            &icons::label(icons::TREE_STRUCTURE, "Start a workflow"),
                        )
                        .on_hover_text("A new draft whose trigger answers this event")
                        .clicked()
                    {
                        let ev = e.clone();
                        model.act(move |m| async move {
                            let w = kls_engine::ops::create_from_event(m.core(), &ev).await?;
                            m.go(crate::Screen::Workflows, Some(w.id.clone()));
                            Ok(Some(crate::model::Notice::Info(format!(
                                "{} is a draft",
                                w.name
                            ))))
                        });
                    }
                });
                ui.add_space(4.0);
                // Links to what the payload names.
                ui.horizontal_wrapped(|ui| {
                    for (k, fields) in FILTER_KEYS {
                        let Some(v) = fields
                            .iter()
                            .find_map(|f| data.get(*f).and_then(|x| x.as_str()))
                        else {
                            continue;
                        };
                        if v.is_empty() {
                            continue;
                        }
                        if kit::link(ui, &format!("{k}: {v}"), p.cyan)
                            .on_hover_text("Events for this")
                            .clicked()
                        {
                            self.filter = format!("{k}:{v}");
                        }
                    }
                });
                if let serde_json::Value::Object(map) = &data {
                    egui::Grid::new("event_facts")
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (k, v) in map {
                                if v.is_object() || v.is_array() {
                                    continue;
                                }
                                ui.label(RichText::new(k).color(p.muted));
                                let s = v
                                    .as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| v.to_string());
                                let fact = format!("{k}={s}");
                                if s.len() <= 80 && !fact.contains(char::is_whitespace) {
                                    if ui
                                        .add(
                                            egui::Label::new(kit::mono(&s, 12.0, p.cyan))
                                                .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text("Filter on this fact")
                                        .clicked()
                                    {
                                        self.filter = toggle_token(&self.filter, &fact);
                                    }
                                } else {
                                    ui.label(kit::mono(&s, 12.0, p.text));
                                }
                                ui.end_row();
                            }
                        });
                }
                ui.add_space(6.0);
                let pretty =
                    serde_json::to_string_pretty(&data).unwrap_or_else(|_| e.data_json.clone());
                ui.horizontal(|ui| {
                    ui.label(kit::caps("Payload", p.muted));
                    if kit::ghost_button(ui, icons::COPY)
                        .on_hover_text("Copy JSON")
                        .clicked()
                    {
                        ui.ctx().copy_text(pretty.clone());
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(kit::mono(&pretty, 12.0, p.text)).wrap());
                    });
            });
        if !open {
            self.selected = None;
        }
    }
}

/// Activity strip: events per minute, the latest minutes in gold.
fn spark(ui: &mut egui::Ui, p: &Palette, events: &[Event], now: i64) {
    let bars = per_minute(events, now);
    kit::card(ui)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
            let max = bars.iter().copied().max().unwrap_or(0).max(1) as f32;
            let gap = 3.0;
            let bw = (rect.width() - gap * (SPARK_MINUTES as f32 - 1.0)) / SPARK_MINUTES as f32;
            let painter = ui.painter_at(rect);
            for (i, n) in bars.iter().enumerate() {
                let h = if *n == 0 {
                    2.0
                } else {
                    (*n as f32 / max * rect.height()).max(4.0)
                };
                let x = rect.left() + i as f32 * (bw + gap);
                let bar = egui::Rect::from_min_max(
                    egui::pos2(x, rect.bottom() - h),
                    egui::pos2(x + bw, rect.bottom()),
                );
                let c = if i + 4 >= SPARK_MINUTES {
                    p.gold
                } else {
                    p.line
                };
                painter.rect_filled(bar, 0.0, c);
            }
            resp.on_hover_text(format!(
                "{} events in the last {SPARK_MINUTES} min",
                bars.iter().sum::<usize>()
            ));
        });
}
/// The screen an event's subject opens on, by kind prefix.
pub(crate) fn target(kind: &str) -> Option<crate::Screen> {
    use crate::Screen as S;
    let area = kind.split('.').next().unwrap_or("");
    Some(match area {
        "session" | "dispatch" | "a2a" => S::Sessions,
        "card" => S::Board,
        "approval" => S::Checkpoints,
        "run" | "job" => S::Runs,
        "workflow" | "schedule" => S::Workflows,
        "template" => S::Templates,
        "node" | "token" => S::Fleet,
        "github" => S::Repos,
        "memory" => S::Memory,
        "budget" => S::Budgets,
        "vault" => S::Vault,
        "agent" | "crew" => S::Workforce,
        "library" => S::Library,
        "audit" | "teleport" => S::Audit,
        "settings" | "factory" => S::Settings,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(at: i64) -> Event {
        Event {
            id: at,
            at,
            kind: "run.step".into(),
            subject: "r1".into(),
            data_json: "{}".into(),
        }
    }

    #[test]
    fn minute_buckets_put_the_newest_last() {
        let now = 100 * 60_000;
        let b = per_minute(&[ev(now - 1), ev(now - 61_000), ev(now - 50 * 60_000)], now);
        assert_eq!(b.len(), SPARK_MINUTES);
        assert_eq!(b[SPARK_MINUTES - 1], 1);
        assert_eq!(b[SPARK_MINUTES - 2], 1);
        assert_eq!(b.iter().sum::<usize>(), 2);
    }

    #[test]
    fn detail_prefers_a_message_and_lists_facts() {
        let (t, f) = detail(r#"{"message":"hello","node":"atlas","n":3,"deep":{"x":1}}"#);
        assert_eq!(t, "hello");
        assert_eq!(f.len(), 2);
        assert!(f.contains(&"n=3".to_string()) && f.contains(&"node=atlas".to_string()));
        let (t, f) = detail("{}");
        assert!(t.is_empty() && f.is_empty());
        let (t, _) = detail(r#"{"x":[1],"at":5,"route":"local"}"#);
        assert_eq!(t, "route=local · x[1]");
        let (t, _) = detail(r#"{"deep":{"x":1}}"#);
        assert_eq!(t, r#"{"deep":{"x":1}}"#);
    }

    #[test]
    fn view_links_encode_the_filter() {
        assert_eq!(
            view_link("run:R1 deploy"),
            "keleustes://events?id=run:R1%20deploy"
        );
    }

    #[test]
    fn structured_filters() {
        let e = |kind: &str, subject: &str, data: &str| Event {
            id: 1,
            at: 0,
            kind: kind.into(),
            subject: subject.into(),
            data_json: data.into(),
        };
        let q = parse_query("run:R1 deploy");
        assert_eq!(q.filters, vec![("run".into(), "r1".into())]);
        assert_eq!(q.words, vec!["deploy".to_string()]);
        assert!(matches(
            &q,
            &e("run.notify", "R1", r#"{"message":"deploy done"}"#)
        ));
        assert!(matches(
            &q,
            &e("session.ended", "s", r#"{"runId":"R1","x":"deploy"}"#)
        ));
        assert!(!matches(
            &q,
            &e("session.ended", "s", r#"{"runId":"R2","x":"deploy"}"#)
        ));
        let q = parse_query("kind:budget. needs");
        assert!(q.needs);
        assert!(matches(&q, &e("budget.blocked", "b", "{}")));
        assert!(!matches(&q, &e("budget.alert", "b", "{}")));
        let q = parse_query("repo:acme/api");
        assert!(matches(
            &q,
            &e(
                "github.pull.updated",
                "acme/api#1",
                r#"{"repo":"acme/api"}"#
            )
        ));
        assert!(matches(&parse_query(""), &e("x", "y", "{}")));
    }

    #[test]
    fn origin_chips_and_fact_filters() {
        let e = |data: &str| Event {
            id: 1,
            at: 0,
            kind: "card.moved".into(),
            subject: "c1".into(),
            data_json: data.into(),
        };
        assert_eq!(origin_of(r#"{"actor":"owner"}"#), "owner");
        assert_eq!(
            origin_of(r#"{"origin":{"kind":"engine","workflow":"wf1"}}"#),
            "wf1"
        );
        assert_eq!(origin_of(r#"{"nodeId":"n1"}"#), "node:n1");
        assert_eq!(origin_of("{}"), "");
        let q = parse_query("origin:owner to=done");
        assert_eq!(q.origin.as_deref(), Some("owner"));
        assert_eq!(q.facts, vec![("to".into(), "done".into())]);
        assert!(q.words.is_empty());
        assert!(matches(&q, &e(r#"{"actor":"owner","to":"done"}"#)));
        assert!(!matches(&q, &e(r#"{"actor":"github","to":"done"}"#)));
        assert!(!matches(&q, &e(r#"{"actor":"owner","to":"review"}"#)));
        assert!(matches(&parse_query("n=3"), &e(r#"{"n":3}"#)));
        assert_eq!(toggle_token("a b", "c"), "a b c");
        assert_eq!(toggle_token("a b c", "b"), "a c");
    }

    #[test]
    fn kinds_get_distinct_colours() {
        let p = crate::theme::DARK;
        assert_ne!(
            kind_color(&p, "run.step"),
            kind_color(&p, "session.entries")
        );
        assert_eq!(kind_color(&p, "run.failed"), p.red);
    }
}
