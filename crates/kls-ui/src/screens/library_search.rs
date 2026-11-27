//! Library › Discover: search public sources without credentials, preview
//! the conversion, review the flags and import with provenance
//! (`docs/gaps/library-search.md` §5). Owner: library search workstream.
//!
//! Nothing fetched is executed or installed from here: import stores the
//! item (hooks and MCP stdio servers are stored only).

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui::{Align, Color32, Layout, Margin, RichText, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use kls_core::libsearch::kls_libsearch::{
    Class, Ecosystem, Flag, GhApi, Hit, ItemType, LicenceClass, LicencePolicy, Locator, NoteKind,
    Query, Trust,
};
use kls_core::libsearch::{self as ls, Ack, Preview, SearchResult, Status};
use kls_store::libsearch::{BundleEntry, Provenance, UpdateState};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const READY: bool = true;

const DEBOUNCE: Duration = Duration::from_millis(300);
const FILTER_W: f32 = 220.0;
const PREVIEW_W: f32 = 380.0;
const GAP: f32 = 16.0;

pub fn type_icon(t: ItemType) -> &'static str {
    match t {
        ItemType::Agent => icons::ROBOT,
        ItemType::Skill => icons::SPARKLE,
        ItemType::Command => icons::TERMINAL,
        ItemType::Hook => icons::LIGHTNING,
        ItemType::Mcp => icons::PLUG,
        ItemType::Prompt => icons::SCROLL,
    }
}

fn kind_icon(kind: &str) -> &'static str {
    ItemType::parse(kind).map(type_icon).unwrap_or(icons::BOOKS)
}

fn flag_icon(f: &Flag) -> &'static str {
    match f {
        Flag::RunsCode => icons::LIGHTNING,
        Flag::ShellExpansion | Flag::PipeToShell => icons::TERMINAL,
        Flag::FileInclude => icons::FILE_TEXT,
        Flag::RemoteUrl(_) => icons::GLOBE,
        Flag::HiddenText => icons::EYE_SLASH,
        Flag::SecretLike => icons::KEY,
        Flag::PromptInjection => icons::WARNING_OCTAGON,
        Flag::LongBase64 => icons::CODE,
        Flag::Upstream(_) => icons::SEAL_WARNING,
        Flag::ProtocolMismatch => icons::PLUGS_CONNECTED,
        Flag::FilesNotCarried(_) => icons::FOLDER,
        Flag::NeedsProgram(_) => icons::CPU,
        Flag::LicenceUnknown | Flag::LicenceRestricted(_) => icons::LOCK_SIMPLE,
        Flag::Refused(_) => icons::PROHIBIT,
    }
}

fn flag_text(f: &Flag) -> String {
    let d = f.detail();
    if d.is_empty() {
        f.name().to_string()
    } else {
        format!("{}  {d}", f.name())
    }
}

fn trust_label(t: Trust) -> &'static str {
    match t {
        Trust::Official => "Official",
        Trust::Trusted => "Trusted",
        Trust::Community => "Community",
    }
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

fn short_sha(s: &str) -> String {
    s.chars().take(7).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Source,
    Converted,
    Diff,
    Notes,
    Files,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Results,
    Imported,
}

/// The review dialog's state.
struct Review {
    ticks: BTreeSet<String>,
    licence: bool,
    rename: String,
}

type BundleLoader = ((String, i64), Loader<Vec<BundleEntry>>);

type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// A signed-in local `gh`, budgeted by kls-github (WP8 boost).
struct GhBoost {
    svc: Arc<kls_github::service::Service>,
    quota: Mutex<Option<(i64, i64)>>,
}

#[async_trait::async_trait]
impl GhApi for GhBoost {
    async fn get_json(&self, path: &str, class: Class) -> anyhow::Result<serde_json::Value> {
        let caller = match class {
            Class::User => kls_github::service::USER,
            Class::Background => "poller",
        };
        let v = self.svc.client().await?.get_json(path, caller).await?;
        if let Ok(q) = self.svc.quota().await
            && let Some(c) = q.core
        {
            *self.quota.lock().unwrap() = Some((c.remaining, c.limit));
        }
        Ok(v)
    }

    fn quota(&self) -> Option<(i64, i64)> {
        *self.quota.lock().unwrap()
    }
}

pub struct Discover {
    query: Arc<Mutex<Query>>,
    text: String,
    types: BTreeSet<ItemType>,
    ecos: BTreeSet<Ecosystem>,
    off_sources: BTreeSet<String>,
    trust: BTreeSet<Trust>,
    licence: LicencePolicy,
    hide_flagged: bool,
    dirty_at: Option<Instant>,
    results: Loader<SearchResult>,
    status: Loader<Status>,
    origins: Loader<Vec<(Provenance, Option<UpdateState>)>>,
    selected: Option<Hit>,
    preview: Slot<Preview>,
    preview_key: Option<String>,
    tab: Tab,
    edit: String,
    edit_dirty: bool,
    edit_error: Option<String>,
    review: Option<Review>,
    view: View,
    show_sources: bool,
    tap_spec: String,
    tap_globs: String,
    tap_type: Option<ItemType>,
    tap_eco: Option<Ecosystem>,
    well_known: String,
    opened: bool,
    bundle: Option<BundleLoader>,
}

impl Discover {
    pub fn new(model: &Model) -> Self {
        let query: Arc<Mutex<Query>> = Arc::default();
        let q2 = query.clone();
        Self {
            results: Loader::new(
                model,
                &[
                    "libsearch.refreshed",
                    "library.imported_from",
                    "libsearch.source_changed",
                ],
                move |core| {
                    let q = q2.lock().unwrap().clone();
                    async move { ls::search(&core, &q).await }
                },
            ),
            status: Loader::new(
                model,
                &["libsearch.", "factory.", "library.refused"],
                |core| async move { ls::status(&core).await },
            ),
            origins: Loader::new(model, &["library."], |core| async move {
                ls::origins(&core).await
            }),
            query,
            text: String::new(),
            types: BTreeSet::new(),
            ecos: BTreeSet::new(),
            off_sources: BTreeSet::new(),
            trust: BTreeSet::new(),
            licence: LicencePolicy::Any,
            hide_flagged: false,
            dirty_at: None,
            selected: None,
            preview: Arc::default(),
            preview_key: None,
            tab: Tab::Converted,
            edit: String::new(),
            edit_dirty: false,
            edit_error: None,
            review: None,
            view: View::Results,
            show_sources: false,
            tap_spec: String::new(),
            tap_globs: String::new(),
            tap_type: None,
            tap_eco: None,
            well_known: String::new(),
            opened: false,
            bundle: None,
        }
    }

    /// First open of the Library: background refresh of stale sources and
    /// an update check (never on start-up, never under the kill switch),
    /// and the gh boost when a local `gh` is signed in.
    fn on_open(&mut self, model: &Model) {
        if self.opened {
            return;
        }
        self.opened = true;
        let core = model.core().clone();
        model.rt.spawn(async move {
            let svc = kls_github::service(&core);
            let gh: Option<Arc<dyn GhApi>> = if svc.local_auth().await.signed_in() {
                Some(Arc::new(GhBoost {
                    svc,
                    quota: Mutex::new(None),
                }))
            } else {
                None
            };
            let _ = ls::set_gh_api(&core, gh).await;
            if !ls::stopped(&core).await {
                if let Err(e) = ls::refresh_stale(&core).await {
                    tracing::debug!("library refresh: {e:#}");
                }
                let _ = ls::check_updates_as(&core, &ls::Actor::System).await;
            }
        });
    }

    fn mark_dirty(&mut self) {
        self.dirty_at = Some(Instant::now());
    }

    fn sync_query(&mut self, ctx: &egui::Context) {
        let Some(at) = self.dirty_at else {
            return;
        };
        let left = DEBOUNCE.saturating_sub(at.elapsed());
        if !left.is_zero() {
            ctx.request_repaint_after(left);
            return;
        }
        self.dirty_at = None;
        let status = self.status.get().and_then(Result::ok);
        let mut q = Query {
            text: self.text.trim().to_string(),
            types: self.types.iter().copied().collect(),
            ecosystems: self.ecos.iter().copied().collect(),
            trust: self.trust.iter().copied().collect(),
            licence: self.licence,
            hide_flagged: self.hide_flagged,
            limit: 300,
            ..Default::default()
        };
        if !self.off_sources.is_empty()
            && let Some(st) = status
        {
            q.sources = st
                .sources
                .iter()
                .filter(|s| !self.off_sources.contains(&s.id))
                .map(|s| s.id.clone())
                .collect();
        }
        *self.query.lock().unwrap() = q;
        self.results.reload();
    }

    /// Header actions while the Discover tab is open: ↻ Refresh, ◎ Sources.
    pub fn header_actions(&mut self, ui: &mut Ui, model: &Model) {
        let st = self.status.get().and_then(Result::ok);
        let stopped = st.as_ref().is_some_and(|s| s.stopped);
        let (on, all) = st
            .as_ref()
            .map(|s| (s.sources.iter().filter(|x| x.on).count(), s.sources.len()))
            .unwrap_or((0, 0));
        if kit::ghost_button(
            ui,
            &icons::label(icons::GLOBE, &format!("Sources {on}/{all}")),
        )
        .clicked()
        {
            self.show_sources = !self.show_sources;
        }
        let r = ui.add_enabled_ui(!stopped, |ui| {
            kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Refresh"))
        });
        if r.inner.clicked() {
            model.act(|m| async move {
                let r = ls::refresh(m.core(), None).await?;
                Ok(Some(Notice::Info(format!(
                    "Refreshed {} · unchanged {} · failed {}",
                    r.refreshed,
                    r.not_modified,
                    r.failed.len()
                ))))
            });
        }
        let v = self.view;
        if kit::tab(
            ui,
            &icons::label(icons::GITHUB_LOGO, "Imported"),
            v == View::Imported,
        )
        .clicked()
        {
            self.view = if v == View::Imported {
                View::Results
            } else {
                View::Imported
            };
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, model: &Model) {
        self.on_open(model);
        self.sync_query(ui.ctx());
        let pal = theme::palette(ui.ctx());
        let status = self.status.get().and_then(Result::ok).unwrap_or_default();
        if status.stopped {
            egui::Frame::new()
                .fill(pal.red.gamma_multiply(0.15))
                .stroke(Stroke::new(1.0, pal.red))
                .inner_margin(Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(icons::label(icons::PROHIBIT, "Factory stopped"))
                            .color(pal.red)
                            .strong(),
                    )
                    .on_hover_text("Local index only · import and refresh off");
                });
            ui.add_space(8.0);
        }
        let result = self.results.get();
        self.search_row(ui, result.as_ref().and_then(|r| r.as_ref().ok()));
        ui.add_space(10.0);
        match self.view {
            View::Imported => self.imported_ui(ui, model),
            View::Results => self.three_columns(ui, model, &status, result),
        }
        self.review_ui(ui.ctx(), model, status.stopped);
        if self.show_sources {
            self.sources_ui(ui.ctx(), model, &status);
        }
    }

    /// Picks or drops a type pill. The ecosystem filter follows: the
    /// ecosystems that publish the picked types (`catalogue::ecosystems_for`),
    /// or all again when no type is picked.
    pub(crate) fn toggle_type(&mut self, t: ItemType) {
        if !self.types.remove(&t) {
            self.types.insert(t);
        }
        self.ecos = self
            .types
            .iter()
            .flat_map(|t| kls_core::libsearch::kls_libsearch::catalogue::ecosystems_for(*t))
            .collect();
        self.mark_dirty();
    }

    fn search_row(&mut self, ui: &mut Ui, res: Option<&SearchResult>) {
        let pal = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.text)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                    .desired_width(320.0),
            );
            if r.changed() {
                self.mark_dirty();
            }
            ui.add_space(8.0);
            for t in ItemType::ALL {
                let n = res
                    .and_then(|r| r.facets.types.get(&t).copied())
                    .unwrap_or(0);
                let on = self.types.contains(&t);
                if kit::pill(ui, &format!("{}  {}  {n}", type_icon(t), t.label()), on).clicked() {
                    self.toggle_type(t);
                }
            }
            if let Some(r) = res {
                if r.stopped {
                    ui.label(RichText::new(icons::label(icons::TRAY, "cached")).color(pal.muted))
                        .on_hover_text("Factory stopped: local index only");
                } else if !r.offline.is_empty() {
                    ui.label(RichText::new(icons::label(icons::TRAY, "offline")).color(pal.amber))
                        .on_hover_text(r.offline.join(", "));
                }
            }
        });
    }

    fn three_columns(
        &mut self,
        ui: &mut Ui,
        model: &Model,
        status: &Status,
        result: Option<Result<SearchResult, String>>,
    ) {
        let w = ui.available_width();
        let h = ui.available_height();
        let wide = w >= FILTER_W + PREVIEW_W + 420.0 + 2.0 * GAP;
        let res = match result {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(r)) => r,
        };
        if wide {
            ui.horizontal_top(|ui| {
                let sx = ui.spacing().item_spacing.x;
                let mid = w - FILTER_W - PREVIEW_W - 2.0 * GAP;
                ui.allocate_ui_with_layout(vec2(FILTER_W, h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(FILTER_W);
                    egui::ScrollArea::vertical()
                        .id_salt("ls_filters")
                        .show(ui, |ui| self.filters_ui(ui, &res, status));
                });
                ui.add_space(GAP - sx);
                ui.allocate_ui_with_layout(vec2(mid, h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(mid);
                    self.results_ui(ui, model, &res);
                });
                ui.add_space(GAP - sx);
                ui.allocate_ui_with_layout(
                    vec2(PREVIEW_W, h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(PREVIEW_W);
                        egui::ScrollArea::vertical()
                            .id_salt("ls_preview")
                            .show(ui, |ui| self.preview_ui(ui, model, status.stopped));
                    },
                );
            });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("ls_stack")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::CollapsingHeader::new(icons::label(icons::FUNNEL, "Filters"))
                        .default_open(false)
                        .show(ui, |ui| self.filters_ui(ui, &res, status));
                    self.results_ui(ui, model, &res);
                    ui.add_space(GAP);
                    self.preview_ui(ui, model, status.stopped);
                });
        }
    }

    fn filters_ui(&mut self, ui: &mut Ui, res: &SearchResult, status: &Status) {
        let pal = theme::palette(ui.ctx());
        let now = kls_store::now_ms();
        kit::card(ui).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(icons::label(icons::FUNNEL, "Filters")).strong());
            ui.add_space(6.0);
            ui.label(kit::caps("Ecosystem", pal.muted));
            let mut ecos: Vec<(Ecosystem, u64)> = res
                .facets
                .ecosystems
                .iter()
                .map(|(e, n)| (*e, *n))
                .collect();
            for e in &self.ecos {
                if !ecos.iter().any(|(x, _)| x == e) {
                    ecos.push((*e, 0));
                }
            }
            ecos.sort_by_key(|e| std::cmp::Reverse(e.1));
            let mut changed = false;
            for (e, n) in ecos {
                let mut on = self.ecos.is_empty() || self.ecos.contains(&e);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut on, e.label()).changed() {
                        if self.ecos.is_empty() {
                            // From "all" to "all but this one".
                            self.ecos = res.facets.ecosystems.keys().copied().collect();
                        }
                        if on {
                            self.ecos.insert(e);
                        } else {
                            self.ecos.remove(&e);
                        }
                        changed = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(n.to_string()).color(pal.muted).monospace());
                    });
                });
            }
            ui.add_space(8.0);
            ui.label(kit::caps("Source", pal.muted));
            for s in status.sources.iter().filter(|s| s.on) {
                let mut on = !self.off_sources.contains(&s.id);
                ui.horizontal(|ui| {
                    let icon = if s.id.starts_with("gh:") {
                        icons::GITHUB_LOGO
                    } else if s.types == [ItemType::Mcp] {
                        icons::PLUG
                    } else {
                        icons::GLOBE
                    };
                    if ui
                        .checkbox(&mut on, format!("{icon} {}", s.label))
                        .changed()
                    {
                        if on {
                            self.off_sources.remove(&s.id);
                        } else {
                            self.off_sources.insert(s.id.clone());
                        }
                        changed = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        source_state_chip(ui, s, now);
                    });
                });
            }
            ui.add_space(8.0);
            ui.label(kit::caps("Licence", pal.muted));
            ui.horizontal_wrapped(|ui| {
                for (p, l) in [
                    (LicencePolicy::Any, "Any"),
                    (LicencePolicy::Permissive, "Permissive"),
                    (LicencePolicy::KnownOnly, "Known"),
                ] {
                    if ui.radio_value(&mut self.licence, p, l).changed() {
                        changed = true;
                    }
                }
            });
            ui.label(kit::caps("Trust", pal.muted));
            ui.horizontal_wrapped(|ui| {
                for t in Trust::ALL {
                    let mut on = self.trust.is_empty() || self.trust.contains(&t);
                    if ui.checkbox(&mut on, trust_label(t)).changed() {
                        if self.trust.is_empty() {
                            self.trust = Trust::ALL.into_iter().collect();
                        }
                        if on {
                            self.trust.insert(t);
                        } else {
                            self.trust.remove(&t);
                        }
                        if self.trust.len() == Trust::ALL.len() {
                            self.trust.clear();
                        }
                        changed = true;
                    }
                }
            });
            if ui
                .checkbox(
                    &mut self.hide_flagged,
                    icons::label(icons::PROHIBIT, "Hide flagged"),
                )
                .changed()
            {
                changed = true;
            }
            ui.add_space(8.0);
            budget_line(ui, status, now);
            if changed {
                self.mark_dirty();
            }
        });
    }

    fn results_ui(&mut self, ui: &mut Ui, model: &Model, res: &SearchResult) {
        let pal = theme::palette(ui.ctx());
        let now = kls_store::now_ms();
        let mut picked: Option<Hit> = None;
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new()
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("Results {}", res.hits.len())).strong());
                });
            if res.hits.is_empty() {
                egui::Frame::new()
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.weak(icons::label(icons::MAGNIFYING_GLASS, "No results"));
                    });
                return;
            }
            let sel = self.selected.as_ref().map(Hit::id);
            let max_h = ui.available_height().max(240.0);
            TableBuilder::new(ui)
                .id_salt("ls_results")
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .max_scroll_height(max_h)
                .column(Column::remainder().at_least(140.0).clip(true))
                .column(Column::initial(40.0))
                .column(Column::initial(90.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::initial(70.0).clip(true))
                .column(Column::initial(50.0))
                .column(Column::initial(50.0))
                .column(Column::initial(36.0))
                .header(30.0, |mut h| {
                    for t in ["Name", "Type", "Eco", "Source", "Lic", "★", "Upd", ""] {
                        h.col(|ui| {
                            kit::rule(ui);
                            kit::th(ui, t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, res.hits.len(), |mut row| {
                        let h = &res.hits[row.index()];
                        row.set_selected(sel.as_deref() == Some(h.id().as_str()));
                        row.col(|ui| {
                            kit::rule(ui);
                            let r = ui
                                .add(egui::Label::new(RichText::new(&h.name).strong()).truncate());
                            if !h.description.is_empty() {
                                r.on_hover_text(&h.description);
                            }
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(type_icon(h.item_type)).color(pal.gold_text))
                                .on_hover_text(h.item_type.label());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(h.ecosystem.label()).color(pal.muted));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            let icon = if h.source.starts_with("gh:") {
                                icons::GITHUB_LOGO
                            } else {
                                icons::GLOBE
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{icon} {}",
                                        h.source.trim_start_matches("gh:")
                                    ))
                                    .color(pal.muted),
                                )
                                .truncate(),
                            )
                            .on_hover_text(h.locator.short());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            licence_label(ui, &h.licence.spdx, h.licence.class);
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(
                                RichText::new(h.popularity.map(compact).unwrap_or("—".into()))
                                    .color(pal.muted),
                            );
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            let t = h.updated_at.map(|u| fmt::ago(now, u)).unwrap_or("—".into());
                            ui.label(RichText::new(t).color(pal.muted));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            if h.flags.iter().any(Flag::refused) {
                                ui.label(RichText::new(icons::PROHIBIT).color(pal.red))
                                    .on_hover_text(flags_hover(&h.flags));
                            } else if !h.flags.is_empty() {
                                ui.label(RichText::new(icons::SHIELD_WARNING).color(pal.amber))
                                    .on_hover_text(flags_hover(&h.flags));
                            }
                        });
                        if row.response().clicked() {
                            picked = Some(h.clone());
                        }
                    });
                });
        });
        if let Some(h) = picked {
            self.open_preview(model, h);
        }
    }

    fn open_preview(&mut self, model: &Model, h: Hit) {
        let key = h.id();
        if self.preview_key.as_deref() == Some(&key) {
            return;
        }
        self.preview_key = Some(key);
        self.selected = Some(h.clone());
        self.edit_dirty = false;
        self.edit_error = None;
        self.tab = Tab::Converted;
        let slot = self.preview.clone();
        *slot.lock().unwrap() = None;
        let core = model.core().clone();
        let ctx = model.egui_ctx();
        model.rt.spawn(async move {
            let r = ls::preview(&core, &h).await.map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }

    fn open_update(&mut self, model: &Model, kind: String, id: String) {
        self.view = View::Results;
        self.preview_key = Some(format!("update:{kind}:{id}"));
        self.selected = None;
        self.edit_dirty = false;
        self.tab = Tab::Diff;
        let slot = self.preview.clone();
        *slot.lock().unwrap() = None;
        let core = model.core().clone();
        let ctx = model.egui_ctx();
        model.rt.spawn(async move {
            let r = ls::update_preview(&core, &kind, &id)
                .await
                .map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }

    /// `ls::node_convert`: belai's own conversion on a library node replaces
    /// the preview (refused and audited under the kill switch or with no
    /// node); the person reviews and imports it as usual.
    fn convert_on_node(&mut self, model: &Model, p: Preview, format: &'static str) {
        let slot = self.preview.clone();
        self.edit_dirty = false;
        self.edit_error = None;
        model.act(move |m| async move {
            let np = ls::node_convert(m.core(), &p, format).await?;
            let name = np.converted.name.clone();
            *slot.lock().unwrap() = Some(Ok(np));
            Ok(Some(Notice::Info(format!("Converted {name} on a node"))))
        });
    }

    fn preview_ui(&mut self, ui: &mut Ui, model: &Model, stopped: bool) {
        let pal = theme::palette(ui.ctx());
        kit::glow(ui).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.preview_key.is_none() {
                ui.label(
                    RichText::new(icons::label(icons::BOOK_OPEN, "No item selected"))
                        .color(pal.muted),
                );
                return;
            }
            let p = self.preview.lock().unwrap().clone();
            let p = match p {
                None => {
                    ui.spinner();
                    return;
                }
                Some(Err(e)) => {
                    let offline = e.contains("offline");
                    let icon = if offline { icons::TRAY } else { icons::WARNING };
                    let text = if offline { "not cached" } else { e.as_str() };
                    ui.colored_label(pal.red, icons::label(icon, text))
                        .on_hover_text(&e);
                    return;
                }
                Some(Ok(p)) => p,
            };
            self.preview_body(ui, model, p, stopped);
        });
    }

    fn preview_body(&mut self, ui: &mut Ui, model: &Model, mut p: Preview, stopped: bool) {
        let pal = theme::palette(ui.ctx());
        let c = &p.converted;
        let t = ItemType::parse(c.target.kind()).unwrap_or(p.hit.item_type);
        ui.label(
            RichText::new(icons::label(type_icon(t), &c.name))
                .size(18.0)
                .strong(),
        );
        ui.horizontal_wrapped(|ui| {
            let (ti, tc) = match p.hit.trust {
                Trust::Official => (icons::SEAL_CHECK, pal.green),
                Trust::Trusted => (icons::SEAL_CHECK, pal.cyan),
                Trust::Community => (icons::USERS, pal.muted),
            };
            kit::chip(ui, &icons::label(ti, trust_label(p.hit.trust)), tc);
            licence_chip(ui, &c.licence.spdx, c.licence.class);
            let at = c
                .upstream
                .commit
                .as_deref()
                .map(short_sha)
                .or(c.upstream.version.clone())
                .unwrap_or_default();
            if !at.is_empty() {
                kit::chip(ui, &icons::label(icons::GITHUB_LOGO, &at), pal.muted);
            }
            kit::chip(
                ui,
                &icons::label(icons::FINGERPRINT, &short_sha(&c.sha256)),
                pal.muted,
            )
            .on_hover_text(&c.sha256);
        });
        ui.label(
            RichText::new(p.hit.locator.short())
                .color(pal.muted)
                .size(12.0),
        );
        if let Locator::GitHub { path, .. } = &p.hit.locator
            && !path.is_empty()
        {
            ui.add(
                egui::Label::new(RichText::new(path).monospace().size(11.0).color(pal.muted))
                    .truncate(),
            );
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (Tab::Source, icons::label(icons::FILE_TEXT, "Source")),
                (Tab::Converted, icons::label(icons::CODE, "Converted")),
                (Tab::Diff, icons::label(icons::GIT_DIFF, "Diff")),
                (
                    Tab::Notes,
                    icons::label(icons::NOTE, &format!("Notes {}", c.notes.len())),
                ),
                (
                    Tab::Files,
                    icons::label(
                        icons::FOLDER,
                        &format!("Files {}", c.files.len().max(p.fetched.files.len())),
                    ),
                ),
            ] {
                if kit::tab(ui, &label, self.tab == tab).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(6.0);
        if !self.edit_dirty {
            self.edit = c.doc_text();
        }
        let mono_frame = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
            egui::Frame::new()
                .fill(pal.bg)
                .stroke(Stroke::new(1.0, pal.line))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("ls_doc")
                        .max_height(320.0)
                        .show(ui, add);
                });
        };
        match self.tab {
            Tab::Source => mono_frame(ui, &mut |ui: &mut Ui| {
                for (path, text) in &p.source_files {
                    ui.label(kit::caps(path, pal.gold_text));
                    ui.label(RichText::new(text).monospace().size(11.0));
                    ui.add_space(6.0);
                }
            }),
            Tab::Converted => {
                let mut changed = false;
                mono_frame(ui, &mut |ui: &mut Ui| {
                    changed = ui
                        .add(
                            egui::TextEdit::multiline(&mut self.edit)
                                .font(egui::TextStyle::Monospace)
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                });
                if changed {
                    self.edit_dirty = true;
                    let mut q = p.clone();
                    match ls::edit_converted(&mut q, &self.edit) {
                        Ok(()) => {
                            self.edit_error = None;
                            *self.preview.lock().unwrap() = Some(Ok(q.clone()));
                            p = q;
                        }
                        Err(e) => self.edit_error = Some(format!("{e:#}")),
                    }
                }
                if let Some(e) = &self.edit_error {
                    ui.colored_label(pal.red, icons::label(icons::WARNING, e));
                }
            }
            Tab::Diff => mono_frame(ui, &mut |ui: &mut Ui| match p.diff_base() {
                None => {
                    ui.label(
                        RichText::new(icons::label(icons::PLUS_CIRCLE, "New item"))
                            .color(pal.muted),
                    );
                }
                Some(base) => diff_ui(ui, base, &self.edit),
            }),
            Tab::Notes => {
                for n in &p.converted.notes {
                    let (icon, color) = match n.kind {
                        NoteKind::Warning => (icons::WARNING, pal.amber),
                        NoteKind::Dropped => (icons::MINUS, pal.muted),
                        NoteKind::Mapped => (icons::ARROW_RIGHT, pal.text),
                        NoteKind::Metadata => (icons::TAG, pal.muted),
                    };
                    let mut s = format!("{}  {}", n.kind.as_str(), n.field);
                    if !n.text.is_empty() {
                        s.push_str(&format!(" · {}", n.text));
                    }
                    ui.label(
                        RichText::new(icons::label(icon, &s))
                            .color(color)
                            .size(12.0),
                    );
                }
            }
            Tab::Files => {
                let c = &p.converted;
                if !c.files.is_empty() {
                    ui.label(kit::caps("Bundle", pal.muted));
                    for f in &c.files {
                        ui.label(
                            RichText::new(format!(
                                "{}  {}  {} B",
                                f.path,
                                short_sha(&f.sha256),
                                f.data.len()
                            ))
                            .monospace()
                            .size(11.0),
                        );
                    }
                }
                ui.label(kit::caps("Source", pal.muted));
                for f in &c.source_files {
                    let r = ui.add(
                        egui::Label::new(
                            RichText::new(format!("{}  {}", f.path, short_sha(&f.sha256)))
                                .monospace()
                                .size(11.0),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if r.on_hover_text(&f.url).clicked() && !f.url.is_empty() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&f.url));
                    }
                }
            }
        }
        ui.add_space(8.0);
        let c = &p.converted;
        for f in c
            .flags
            .iter()
            .filter(|f| !matches!(f, Flag::FilesNotCarried(_)))
        {
            let color = if f.refused() { pal.red } else { pal.amber };
            ui.label(
                RichText::new(icons::label(flag_icon(f), &flag_text(f)))
                    .color(color)
                    .size(12.0),
            );
        }
        let dropped: Vec<&str> = c
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Dropped)
            .map(|n| n.field.as_str())
            .take(8)
            .collect();
        if !dropped.is_empty() {
            ui.label(
                RichText::new(icons::label(
                    icons::INFO,
                    &format!("dropped: {}", dropped.join(", ")),
                ))
                .color(pal.muted)
                .size(12.0),
            );
        }
        if let Some(n) = c.flags.iter().find_map(|f| match f {
            Flag::FilesNotCarried(n) => Some(*n),
            _ => None,
        }) {
            ui.label(
                RichText::new(icons::label(
                    icons::INFO,
                    &format!("files not carried: {n}"),
                ))
                .color(pal.muted)
                .size(12.0),
            );
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can = !stopped && self.edit_error.is_none();
            let label = if p.update_of.is_some() {
                icons::label(icons::ARROWS_CLOCKWISE, "Update")
            } else {
                icons::label(icons::DOWNLOAD_SIMPLE, "Import")
            };
            if ui
                .add_enabled_ui(can, |ui| theme::primary_button(ui, &label))
                .inner
                .on_disabled_hover_text(if stopped {
                    "Factory stopped"
                } else {
                    "Fix the converted text"
                })
                .clicked()
            {
                self.review = Some(Review {
                    ticks: BTreeSet::new(),
                    licence: false,
                    rename: p.converted.name.clone(),
                });
            }
            // Formats only belai reads: convert on a node, then review.
            if let Some(format) = ls::node_format(&p)
                && ui
                    .add_enabled(
                        !stopped,
                        egui::Button::new(icons::label(icons::HARD_DRIVES, "Convert on node")),
                    )
                    .on_hover_text(format!("{format}: belai converts it"))
                    .on_disabled_hover_text("Factory stopped")
                    .clicked()
            {
                self.convert_on_node(model, p.clone(), format);
            }
            let url = p.hit.locator.web_url();
            if kit::ghost_button(ui, &icons::label(icons::ARROW_SQUARE_OUT, "Open"))
                .on_hover_text(&url)
                .clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            if let (Locator::GitHub { owner, repo, .. }, Some(sha)) =
                (&p.hit.locator, p.converted.upstream.commit.clone())
                && kit::ghost_button(ui, &icons::label(icons::FINGERPRINT, "Pin"))
                    .on_hover_text(format!("Tap {owner}/{repo}@{}", short_sha(&sha)))
                    .clicked()
            {
                let spec = format!("{owner}/{repo}@{sha}");
                let ty = Some(p.hit.item_type);
                let eco = Some(p.hit.ecosystem);
                model.act(move |m| async move {
                    let id = ls::add_tap(m.core(), &spec, &[], ty, eco).await?;
                    Ok(Some(Notice::Info(format!("Pinned {id}"))))
                });
            }
        });
    }

    fn review_ui(&mut self, ctx: &egui::Context, model: &Model, stopped: bool) {
        let Some(rv) = &mut self.review else {
            return;
        };
        let Some(Ok(p)) = self.preview.lock().unwrap().clone() else {
            self.review = None;
            return;
        };
        let pal = theme::palette(ctx);
        let c = &p.converted;
        let t = ItemType::parse(c.target.kind()).unwrap_or(p.hit.item_type);
        let mut close = false;
        let title = if p.update_of.is_some() {
            icons::label(
                icons::ARROWS_CLOCKWISE,
                &format!("Update {} {}", type_icon(t), c.name),
            )
        } else {
            icons::label(
                icons::DOWNLOAD_SIMPLE,
                &format!("Import {} {}", type_icon(t), c.name),
            )
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let at = c
                        .upstream
                        .commit
                        .as_deref()
                        .map(short_sha)
                        .or(c.upstream.version.clone())
                        .unwrap_or_default();
                    ui.label(icons::label(
                        icons::GITHUB_LOGO,
                        &format!("{} @ {at}", p.hit.locator.short()),
                    ));
                    ui.label(
                        RichText::new(icons::label(icons::FINGERPRINT, &short_sha(&c.sha256)))
                            .monospace(),
                    )
                    .on_hover_text(&c.sha256);
                    licence_chip(ui, &c.licence.spdx, c.licence.class);
                });
                if let Some(r) = c.refused() {
                    ui.add_space(8.0);
                    ui.colored_label(pal.red, icons::label(icons::PROHIBIT, &r));
                    ui.add_space(8.0);
                    if kit::ghost_button(ui, &icons::label(icons::X, "Close")).clicked() {
                        close = true;
                    }
                    return;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    kit::field_label(ui, "Name");
                    ui.add(egui::TextEdit::singleline(&mut rv.rename).desired_width(200.0));
                    let target = match &p.existing {
                        Some((_, v, _)) => format!("v{} of existing", v + 1),
                        None => "New".to_string(),
                    };
                    ui.label(RichText::new(target).color(pal.muted));
                });
                if t == ItemType::Hook {
                    ui.label(
                        RichText::new(icons::label(
                            icons::LIGHTNING,
                            "RunsCode · stored, not installed",
                        ))
                        .color(pal.amber),
                    );
                }
                if t == ItemType::Mcp
                    && let Some(argv) = mcp_argv(&c.doc)
                {
                    ui.label(
                        RichText::new(icons::label(icons::PLUG, &argv))
                            .monospace()
                            .size(12.0),
                    );
                }
                let review = c.review_flags();
                if !review.is_empty() || c.licence.needs_ack() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(icons::label(icons::SHIELD_WARNING, "Review")).strong());
                }
                for f in &review {
                    let key = f.key();
                    let mut on = rv.ticks.contains(&key);
                    if ui
                        .checkbox(&mut on, icons::label(flag_icon(f), &flag_text(f)))
                        .changed()
                    {
                        if on {
                            rv.ticks.insert(key);
                        } else {
                            rv.ticks.remove(&key);
                        }
                    }
                }
                if c.licence.needs_ack() {
                    let l = if c.licence.spdx.is_empty() {
                        "Licence: unknown".to_string()
                    } else {
                        format!("Licence: {}", c.licence.spdx)
                    };
                    ui.checkbox(&mut rv.licence, icons::label(icons::LOCK_SIMPLE, &l));
                }
                let dropped: Vec<&str> = c
                    .notes
                    .iter()
                    .filter(|n| n.kind == NoteKind::Dropped)
                    .map(|n| n.field.as_str())
                    .take(8)
                    .collect();
                if !dropped.is_empty() {
                    ui.label(
                        RichText::new(icons::label(
                            icons::INFO,
                            &format!("dropped: {}", dropped.join(", ")),
                        ))
                        .color(pal.muted)
                        .size(12.0),
                    );
                }
                ui.add_space(8.0);
                let all = review.iter().all(|f| rv.ticks.contains(&f.key()))
                    && (!c.licence.needs_ack() || rv.licence)
                    && !stopped;
                ui.horizontal(|ui| {
                    let label = if p.update_of.is_some() {
                        icons::label(icons::ARROWS_CLOCKWISE, "Update")
                    } else {
                        icons::label(icons::DOWNLOAD_SIMPLE, "Import")
                    };
                    if ui
                        .add_enabled_ui(all, |ui| theme::primary_button(ui, &label))
                        .inner
                        .clicked()
                    {
                        let ack = Ack {
                            flags: review.iter().map(|f| (*f).clone()).collect(),
                            licence: rv.licence,
                            rename: Some(rv.rename.trim().to_string()).filter(|n| *n != c.name),
                        };
                        let p = p.clone();
                        model.act(move |m| async move {
                            let (_, v) = ls::import(m.core(), &p, &ack).await?;
                            Ok(Some(Notice::Info(format!(
                                "Imported {} v{v}",
                                ack.rename.as_deref().unwrap_or(&p.converted.name)
                            ))))
                        });
                        close = true;
                    }
                    if kit::ghost_button(ui, &icons::label(icons::X, "Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.review = None;
        }
    }

    fn sources_ui(&mut self, ctx: &egui::Context, model: &Model, status: &Status) {
        let pal = theme::palette(ctx);
        let now = kls_store::now_ms();
        let mut open = true;
        egui::Window::new(icons::label(icons::GLOBE, "Sources"))
            .open(&mut open)
            .default_width(720.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        egui::Grid::new("ls_sources")
                            .num_columns(8)
                            .spacing([12.0, 6.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for h in [
                                    "On", "Source", "Types", "Mode", "Last", "Items", "Status", "",
                                ] {
                                    ui.label(kit::caps(h, pal.muted));
                                }
                                ui.end_row();
                                for s in &status.sources {
                                    let mut on = s.on;
                                    if ui.checkbox(&mut on, "").changed() {
                                        let id = s.id.clone();
                                        model.act(move |m| async move {
                                            ls::set_source(m.core(), &id, on).await?;
                                            Ok(None)
                                        });
                                    }
                                    let icon = if s.id.starts_with("gh:") {
                                        icons::GITHUB_LOGO
                                    } else if s.types == [ItemType::Mcp] {
                                        icons::PLUG
                                    } else {
                                        icons::GLOBE
                                    };
                                    let mut label = format!("{icon} {}", s.label);
                                    if s.needs_gh {
                                        label.push_str(" · gh");
                                    }
                                    ui.label(label);
                                    ui.label(
                                        s.types.iter().map(|t| type_icon(*t)).collect::<String>(),
                                    );
                                    ui.label(RichText::new(s.mode.label()).monospace());
                                    let last = match (&s.state, s.mode.indexed()) {
                                        (_, false) => "live".to_string(),
                                        (Some(st), true) => {
                                            let mut t = st
                                                .refreshed_at
                                                .map(|r| fmt::ago(now, r))
                                                .unwrap_or("—".into());
                                            if let Some(c) = &st.commit {
                                                t.push_str(&format!(
                                                    "@{}",
                                                    c.chars().take(4).collect::<String>()
                                                ));
                                            }
                                            t
                                        }
                                        (None, true) => "—".into(),
                                    };
                                    ui.label(RichText::new(last).color(pal.muted));
                                    let items = s
                                        .state
                                        .as_ref()
                                        .map(|st| compact(st.items.max(0) as u64))
                                        .unwrap_or("—".into());
                                    ui.label(RichText::new(items).monospace());
                                    source_state_chip(ui, s, now);
                                    if matches!(s.kind.as_str(), "tap" | "well_known") {
                                        if kit::ghost_button(ui, icons::TRASH)
                                            .on_hover_text("Remove")
                                            .clicked()
                                        {
                                            let id = s.id.clone();
                                            model.act(move |m| async move {
                                                ls::remove_source(m.core(), &id).await?;
                                                Ok(None)
                                            });
                                        }
                                    } else {
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(icons::GITHUB_LOGO);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.tap_spec)
                            .hint_text("owner/repo@ref")
                            .desired_width(180.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.tap_globs)
                            .hint_text("globs")
                            .desired_width(140.0),
                    );
                    egui::ComboBox::from_id_salt("ls_tap_type")
                        .selected_text(self.tap_type.map(|t| t.label()).unwrap_or("type"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tap_type, None, "Any");
                            for t in ItemType::ALL {
                                ui.selectable_value(&mut self.tap_type, Some(t), t.label());
                            }
                        });
                    egui::ComboBox::from_id_salt("ls_tap_eco")
                        .selected_text(self.tap_eco.map(|e| e.label()).unwrap_or("eco"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tap_eco, None, "Detect");
                            for e in Ecosystem::ALL {
                                ui.selectable_value(&mut self.tap_eco, Some(e), e.label());
                            }
                        });
                    if kit::ghost_button(ui, &icons::label(icons::PLUS, "Add")).clicked()
                        && !self.tap_spec.trim().is_empty()
                    {
                        let spec = self.tap_spec.trim().to_string();
                        let globs: Vec<String> = self
                            .tap_globs
                            .split([',', ' '])
                            .map(str::trim)
                            .filter(|g| !g.is_empty())
                            .map(str::to_string)
                            .collect();
                        let (ty, eco) = (self.tap_type, self.tap_eco);
                        model.act(move |m| async move {
                            let id = ls::add_tap(m.core(), &spec, &globs, ty, eco).await?;
                            Ok(Some(Notice::Info(format!("Added {id}"))))
                        });
                        self.tap_spec.clear();
                        self.tap_globs.clear();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(icons::GLOBE);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.well_known)
                            .hint_text("https://site/.well-known/skills/index.json")
                            .desired_width(320.0),
                    );
                    if kit::ghost_button(ui, &icons::label(icons::PLUS, "Add")).clicked()
                        && !self.well_known.trim().is_empty()
                    {
                        let url = self.well_known.trim().to_string();
                        model.act(move |m| async move {
                            let id = ls::add_well_known(m.core(), &url).await?;
                            Ok(Some(Notice::Info(format!("Added {id}"))))
                        });
                        self.well_known.clear();
                    }
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    budget_line(ui, status, now);
                    let mut gh = status.gh_on;
                    let label = if status.gh_signed_in {
                        icons::label(icons::GITHUB_LOGO, "Use gh  ✓")
                    } else {
                        icons::label(icons::GITHUB_LOGO, "Use gh")
                    };
                    if ui
                        .add_enabled(status.gh_signed_in, egui::Checkbox::new(&mut gh, label))
                        .on_disabled_hover_text("gh not signed in")
                        .changed()
                    {
                        model.act(move |m| async move {
                            ls::set_setting(m.core(), "gh", gh).await?;
                            Ok(None)
                        });
                    }
                    // WP8: a node's gh when this process has no path (off by default).
                    let mut node_gh = status.node_gh;
                    if ui
                        .checkbox(
                            &mut node_gh,
                            icons::label(icons::PLUGS_CONNECTED, "Node gh"),
                        )
                        .on_hover_text("Offline: fetch GitHub through a node's gh")
                        .changed()
                    {
                        model.act(move |m| async move {
                            ls::set_setting(m.core(), "node_gh", node_gh).await?;
                            Ok(None)
                        });
                    }
                    let mut s4 = status.skills_sh;
                    if ui
                        .checkbox(&mut s4, icons::label(icons::FLAG, "skills.sh"))
                        .on_hover_text("Undocumented API")
                        .changed()
                    {
                        model.act(move |m| async move {
                            ls::set_setting(m.core(), "skills_sh", s4).await?;
                            Ok(None)
                        });
                    }
                });
            });
        if !open {
            self.show_sources = false;
        }
    }

    /// §5.3: imported items with their source and update state.
    fn imported_ui(&mut self, ui: &mut Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        let now = kls_store::now_ms();
        let rows = match self.origins.get() {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(r)) => r,
        };
        let stopped = self
            .status
            .get()
            .and_then(Result::ok)
            .is_some_and(|s| s.stopped);
        ui.horizontal(|ui| {
            if ui
                .add_enabled_ui(!stopped, |ui| {
                    kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Check updates"))
                })
                .inner
                .clicked()
            {
                model.act(|m| async move {
                    let n = ls::check_updates(m.core()).await?;
                    Ok(Some(Notice::Info(format!("{n} behind"))))
                });
            }
        });
        ui.add_space(8.0);
        let mut open: Option<(String, String)> = None;
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if rows.is_empty() {
                kit::empty(ui, icons::GITHUB_LOGO, "Nothing imported");
                return;
            }
            TableBuilder::new(ui)
                .id_salt("ls_imported")
                .striped(false)
                .resizable(true)
                .column(Column::remainder().at_least(140.0).clip(true))
                .column(Column::initial(50.0))
                .column(Column::initial(70.0))
                .column(Column::initial(80.0))
                .column(Column::initial(200.0).clip(true))
                .column(Column::initial(140.0))
                .header(30.0, |mut h| {
                    for t in ["Item", "Type", "Version", "Saved", "Source", "Update"] {
                        h.col(|ui| {
                            kit::rule(ui);
                            kit::th(ui, t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let (p, u) = &rows[row.index()];
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(short_sha(&p.item_id)).monospace())
                                .on_hover_text(&p.item_id);
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(kind_icon(&p.item_kind)).color(pal.gold_text));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(format!("v{}", p.version)).monospace());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(RichText::new(fmt::ago(now, p.reviewed_at)).color(pal.muted));
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            let lock = if p.licence.is_empty() {
                                format!(" {}?", icons::LOCK_SIMPLE)
                            } else {
                                String::new()
                            };
                            let r = ui.add(
                                egui::Label::new(format!(
                                    "{} {}@{}{lock}",
                                    icons::GITHUB_LOGO,
                                    p.source.trim_start_matches("gh:"),
                                    p.short_ref()
                                ))
                                .truncate()
                                .sense(egui::Sense::click()),
                            );
                            let url = p.urls().first().cloned().unwrap_or_default();
                            if r.on_hover_text(&url).clicked() && !url.is_empty() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            }
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            match u.as_ref().map(|u| u.state.as_str()) {
                                Some("behind") => {
                                    let latest = u
                                        .as_ref()
                                        .map(|u| {
                                            if u.latest_commit.is_empty() {
                                                u.latest_version.clone()
                                            } else {
                                                short_sha(&u.latest_commit)
                                            }
                                        })
                                        .unwrap_or_default();
                                    if kit::link(
                                        ui,
                                        &icons::label(
                                            icons::ARROWS_CLOCKWISE,
                                            &format!("behind ({latest})"),
                                        ),
                                        pal.amber,
                                    )
                                    .clicked()
                                    {
                                        open = Some((p.item_kind.clone(), p.item_id.clone()));
                                    }
                                }
                                Some("current") => {
                                    ui.label(RichText::new(icons::CHECK).color(pal.green));
                                }
                                Some("gone") => {
                                    ui.label(
                                        RichText::new(icons::label(icons::X_CIRCLE, "gone"))
                                            .color(pal.red),
                                    );
                                }
                                Some("error") => {
                                    ui.label(RichText::new(icons::WARNING).color(pal.amber));
                                }
                                _ => {
                                    ui.label(RichText::new("—").color(pal.muted));
                                }
                            }
                        });
                    });
                });
        });
        if let Some((k, id)) = open {
            self.open_update(model, k, id);
        }
    }

    // ---- hooks used by the Library editor -----------------------------------------

    /// The editor header chip `⎇ owner/repo@sha7 ⌗ hash7` (opens the
    /// source URL), when the item was imported.
    pub fn origin_chip(&self, ui: &mut Ui, kind: &str, id: &str) {
        if id.is_empty() {
            return;
        }
        let Some(Ok(rows)) = self.origins.get() else {
            return;
        };
        let Some((p, _)) = rows
            .iter()
            .find(|(p, _)| p.item_id == id && p.item_kind == kind)
        else {
            return;
        };
        let pal = theme::palette(ui.ctx());
        let text = format!(
            "{} {}@{}  {} {}",
            icons::GITHUB_LOGO,
            p.source.trim_start_matches("gh:"),
            p.short_ref(),
            icons::FINGERPRINT,
            short_sha(&p.doc_sha256)
        );
        let url = p.urls().first().cloned().unwrap_or_default();
        let r = kit::chip(ui, &text, pal.muted);
        let r = ui
            .interact(r.rect, r.id.with("origin"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("{} · {}", icons::ARROW_SQUARE_OUT, url));
        if r.clicked() && !url.is_empty() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
    }

    /// A hook's bundle files (path, size, hash) for the open version.
    pub fn bundle_ui(&mut self, ui: &mut Ui, model: &Model, id: &str, version: i64) {
        if id.is_empty() || version <= 0 {
            return;
        }
        let key = (id.to_string(), version);
        if self.bundle.as_ref().is_none_or(|(k, _)| *k != key) {
            let (item, v) = key.clone();
            let l = Loader::new(model, &["library."], move |core| {
                let item = item.clone();
                async move { core.store().bundle_files(&item, v).await }
            });
            self.bundle = Some((key, l));
        }
        let Some(Ok(files)) = self.bundle.as_ref().and_then(|(_, l)| l.get()) else {
            return;
        };
        let pal = theme::palette(ui.ctx());
        ui.add_space(6.0);
        ui.label(
            RichText::new(icons::label(
                icons::FOLDER,
                &format!("Files {}", files.len()),
            ))
            .size(12.0)
            .color(pal.muted),
        )
        .on_hover_text("Stored, not installed");
        for f in &files {
            ui.label(
                RichText::new(format!(
                    "{}  {} B  {}",
                    f.path,
                    f.size_bytes,
                    short_sha(&f.sha256)
                ))
                .monospace()
                .size(11.0),
            );
        }
    }
}

fn source_state_chip(ui: &mut Ui, s: &ls::SourceView, now: i64) {
    let pal = theme::palette(ui.ctx());
    let (color, text, hover): (Color32, String, String) = match (&s.state, s.mode.indexed()) {
        _ if s.offline_at.is_some() && !s.mode.indexed() => (
            pal.amber,
            format!("{} offline", icons::TRAY),
            "Not reachable".into(),
        ),
        (_, false) => (pal.green, "live".into(), "Queried live".into()),
        (None, true) => (pal.muted, String::new(), "Never refreshed".into()),
        (Some(st), true) => {
            if let Some(p) = st.parked_until.filter(|p| *p > now) {
                (
                    pal.amber,
                    format!("{} {}", icons::CLOCK, fmt::ago(p, now)),
                    "Rate limited".into(),
                )
            } else if let Some(e) = &st.error {
                let age = st.ok_at.map(|o| fmt::ago(now, o)).unwrap_or_default();
                let t = if st.items > 0 {
                    format!("{} cached {age}", icons::TRAY)
                } else {
                    String::new()
                };
                (pal.amber, t, e.clone())
            } else {
                (
                    pal.green,
                    st.refreshed_at
                        .map(|r| fmt::ago(now, r))
                        .unwrap_or_default(),
                    "OK".into(),
                )
            }
        }
    };
    ui.horizontal(|ui| {
        if s.state.is_none() && s.mode.indexed() {
            ui.label(RichText::new(icons::CIRCLE).color(color).size(10.0))
                .on_hover_text(&hover);
        } else if color == pal.amber {
            ui.label(RichText::new(icons::WARNING).color(color).size(11.0))
                .on_hover_text(&hover);
        } else {
            kit::dot(ui, color);
        }
        if !text.is_empty() {
            ui.label(RichText::new(text).size(11.0).color(pal.muted))
                .on_hover_text(&hover);
        }
    });
}

/// `⎇ anon 41/60 ⏲ 38m` or `⎇ gh ✓ 4980/5000`.
fn budget_line(ui: &mut Ui, st: &Status, now: i64) {
    let pal = theme::palette(ui.ctx());
    if st.gh_on && st.gh_signed_in {
        let q = st
            .gh_quota
            .map(|(r, l)| format!(" {r}/{l}"))
            .unwrap_or_default();
        ui.label(
            RichText::new(format!("{} gh ✓{q}", icons::GITHUB_LOGO))
                .color(pal.muted)
                .size(12.0),
        );
        return;
    }
    let b = st.budgets.iter().find(|b| b.key == "api.github.com");
    let (rem, cap, reset) = b
        .map(|b| (b.remaining, b.capacity, b.reset_at))
        .unwrap_or((55, 55, now));
    let mut s = format!("{} anon {rem}/{cap}", icons::GITHUB_LOGO);
    if rem < cap && reset > now {
        s.push_str(&format!(" {} {}", icons::CLOCK, fmt::ago(reset, now)));
    }
    ui.label(RichText::new(s).color(pal.muted).size(12.0))
        .on_hover_text("api.github.com without credentials · search 9/min");
}

fn licence_color(class: LicenceClass, ui: &Ui) -> Color32 {
    let pal = theme::palette(ui.ctx());
    match class {
        LicenceClass::Permissive => pal.green,
        LicenceClass::Copyleft => pal.amber,
        LicenceClass::Restricted | LicenceClass::Unknown => pal.red,
    }
}

fn licence_label(ui: &mut Ui, spdx: &str, class: LicenceClass) {
    let c = licence_color(class, ui);
    let t = if spdx.is_empty() { "?" } else { spdx };
    ui.add(egui::Label::new(RichText::new(t).color(c)).truncate());
}

fn licence_chip(ui: &mut Ui, spdx: &str, class: LicenceClass) {
    let c = licence_color(class, ui);
    let t = if spdx.is_empty() { "?" } else { spdx };
    let icon = if class == LicenceClass::Permissive {
        icons::CHECK
    } else {
        icons::LOCK_SIMPLE
    };
    kit::chip(ui, &icons::label(icon, t), c);
}

fn flags_hover(flags: &[Flag]) -> String {
    flags.iter().map(flag_text).collect::<Vec<_>>().join("\n")
}

/// `npx -y @x/y@1.2.3` from a canonical MCP doc (stdio only).
fn mcp_argv(doc: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(doc).ok()?;
    let cmd = v.get("command")?.as_str()?;
    let args: Vec<&str> = v
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    Some(
        std::iter::once(cmd)
            .chain(args)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn diff_ui(ui: &mut Ui, old: &str, new: &str) {
    let pal = theme::palette(ui.ctx());
    let d = similar::TextDiff::from_lines(old, new);
    for ch in d.iter_all_changes() {
        let (sign, color) = match ch.tag() {
            similar::ChangeTag::Delete => ("-", pal.red),
            similar::ChangeTag::Insert => ("+", pal.green),
            similar::ChangeTag::Equal => (" ", pal.muted),
        };
        ui.label(
            RichText::new(format!("{sign} {}", ch.value().trim_end_matches('\n')))
                .monospace()
                .size(11.0)
                .color(color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_helpers() {
        assert_eq!(compact(41_400), "41.4k");
        assert_eq!(compact(1_000), "1k");
        assert_eq!(compact(52), "52");
        assert_eq!(type_icon(ItemType::Hook), icons::LIGHTNING);
        assert_eq!(kind_icon("mcp"), icons::PLUG);
        assert_eq!(
            mcp_argv(
                br#"{"name":"x","transport":"stdio","command":"npx","args":["-y","@x/y@1.2.3"]}"#
            )
            .as_deref(),
            Some("npx -y @x/y@1.2.3")
        );
        assert!(mcp_argv(br#"{"name":"x","transport":"http","url":"https://x"}"#).is_none());
        assert_eq!(
            flag_text(&Flag::RemoteUrl("example.com".into())),
            "RemoteUrl  example.com"
        );
        assert_eq!(flag_icon(&Flag::Refused("x".into())), icons::PROHIBIT);
    }
}

/// §7: the Discover pane's state against a FakeSource (no network): typing
/// searches it, a type pill narrows the ecosystems and the results, and
/// the pane renders the hits.
#[cfg(test)]
mod discover_tests {
    use super::*;
    use kls_core::libsearch::kls_libsearch::{
        self as kl, Ctx, FakeFetcher, Fetched, Licence, Locator, Mode, Refresh, Source, Upstream,
    };

    struct FakeSource;

    fn hit(name: &str, t: ItemType, e: Ecosystem) -> Hit {
        Hit {
            source: "fake".into(),
            locator: Locator::GitHub {
                owner: "o".into(),
                repo: "r".into(),
                git_ref: None,
                path: name.into(),
            },
            item_type: t,
            ecosystem: e,
            name: name.into(),
            title: name.into(),
            description: format!("{name} for tests"),
            tags: vec![],
            licence: Licence::from_id("MIT", "index"),
            trust: Trust::Trusted,
            popularity: Some(5),
            updated_at: None,
            flags: vec![],
        }
    }

    #[async_trait::async_trait]
    impl Source for FakeSource {
        fn id(&self) -> &str {
            "fake"
        }
        fn label(&self) -> &str {
            "Fake"
        }
        fn types(&self) -> &[ItemType] {
            &ItemType::ALL
        }
        fn ecosystems(&self) -> &[Ecosystem] {
            &[Ecosystem::ClaudeCode, Ecosystem::McpRegistry]
        }
        fn mode(&self) -> Mode {
            Mode::Remote
        }
        async fn refresh(&self, _cx: &Ctx, _cursor: Option<&str>) -> anyhow::Result<Refresh> {
            Ok(Refresh::default())
        }
        async fn search(&self, _cx: &Ctx, q: &Query) -> anyhow::Result<Vec<Hit>> {
            Ok([
                hit("pdf-tools", ItemType::Skill, Ecosystem::ClaudeCode),
                hit("pdf-server", ItemType::Mcp, Ecosystem::McpRegistry),
            ]
            .into_iter()
            .filter(|h| h.name.contains(q.text.trim()))
            .collect())
        }
        async fn fetch(&self, _cx: &Ctx, _hit: &Hit) -> anyhow::Result<Fetched> {
            anyhow::bail!("not fetched in this test")
        }
        async fn latest(&self, _cx: &Ctx, _loc: &Locator) -> anyhow::Result<Option<Upstream>> {
            Ok(None)
        }
    }

    fn wait<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < until, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn names(d: &Discover) -> Option<Vec<String>> {
        let r = d.results.get()?.ok()?;
        Some(r.hits.iter().map(|h| h.name.clone()).collect())
    }

    #[test]
    fn discover_searches_narrows_by_type_and_renders() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let model = rt.block_on(async {
            let core = kls_core::Core::open(dir.path(), None).await.unwrap();
            ls::install(&core, FakeFetcher::new()).await.unwrap();
            ls::add_source(&core, Arc::new(FakeSource)).await.unwrap();
            let hub = kls_node::hub(core).await.unwrap();
            Model::new(hub, rt.handle().clone(), egui::Context::default())
        });
        let mut d = Discover::new(&model);
        // No first-open refresh or gh lookup in a test.
        d.opened = true;
        let ctx = egui::Context::default();
        d.text = "pdf".into();
        d.dirty_at = Some(Instant::now() - DEBOUNCE * 2);
        d.sync_query(&ctx);
        let got = wait("both hits", || names(&d).filter(|n| n.len() == 2));
        assert!(got.contains(&"pdf-tools".to_string()) && got.contains(&"pdf-server".to_string()));

        // The MCP pill narrows the ecosystems to those that publish MCP
        // servers, and the results to the MCP hit.
        d.toggle_type(ItemType::Mcp);
        let mcp: BTreeSet<Ecosystem> = kl::catalogue::ecosystems_for(ItemType::Mcp)
            .into_iter()
            .collect();
        assert_eq!(d.ecos, mcp);
        d.dirty_at = Some(Instant::now() - DEBOUNCE * 2);
        d.sync_query(&ctx);
        assert_eq!(d.query.lock().unwrap().types, vec![ItemType::Mcp]);
        let got = wait("the MCP hit only", || names(&d).filter(|n| n.len() == 1));
        assert_eq!(got, vec!["pdf-server".to_string()]);
        // Dropping the pill lifts the ecosystem filter.
        d.toggle_type(ItemType::Mcp);
        assert!(d.ecos.is_empty());

        // The pane renders the hits.
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| d.ui(ui, &model));
        });
        out.textures_delta.clear();
    }
}
