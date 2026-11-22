//! Board: the kanban. Columns triage to done with drag and drop, list tabs
//! (review, backlog, done, triage), filters and sort, the card drawer
//! (edit, activity, send to a session) and new cards.

use crate::events::clock;
use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme::{self, palette};
use egui::RichText;
use egui_extras::{Column, TableBuilder};
use kls_core::DispatchSpec;
use kls_core::board::{self as domain, CardDraft, CardPatch, OWNER, STATUSES};
use kls_store::board::{Card, CardActivity};

mod bulk;
mod claims;
mod github;
mod tabs;

pub const READY: bool = true;

/// Board lanes: the statuses plus the Blocked list (todo cards belai marks blocked).
const LANES: [&str; 7] = [
    "triage", "backlog", "todo", "doing", "blocked", "review", "done",
];

fn lane_of(c: &Card) -> &str {
    if domain::cards::is_blocked(c) {
        "blocked"
    } else {
        c.status.as_str()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Board,
    Review,
    Backlog,
    Done,
    Triage,
    /// Blocked and held cards by cause.
    Stuck,
    /// Board vs GitHub field conflicts.
    Sync,
}

impl Tab {
    const ALL: [Tab; 7] = [
        Tab::Board,
        Tab::Review,
        Tab::Backlog,
        Tab::Done,
        Tab::Stuck,
        Tab::Triage,
        Tab::Sync,
    ];
    fn title(self) -> &'static str {
        match self {
            Tab::Board => "Board",
            Tab::Review => "Review",
            Tab::Backlog => "Backlog",
            Tab::Done => "Done",
            Tab::Triage => "Intake",
            Tab::Stuck => "Triage",
            Tab::Sync => "Sync",
        }
    }
    fn status(self) -> Option<&'static str> {
        match self {
            Tab::Board => None,
            Tab::Review => Some("review"),
            Tab::Backlog => Some("backlog"),
            Tab::Done => Some("done"),
            Tab::Triage => Some("triage"),
            Tab::Stuck => None,
            Tab::Sync => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Updated,
    Created,
    Priority,
    Title,
}

impl SortKey {
    const ALL: [SortKey; 4] = [
        SortKey::Updated,
        SortKey::Created,
        SortKey::Priority,
        SortKey::Title,
    ];
    fn title(self) -> &'static str {
        match self {
            SortKey::Updated => "Updated",
            SortKey::Created => "Created",
            SortKey::Priority => "Priority",
            SortKey::Title => "Title",
        }
    }
}

/// Board filters: fields are ANDed; an empty field matches everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub label: String,
    pub assignee: String,
    pub repo: String,
    pub text: String,
}

impl Filters {
    pub fn matches(&self, c: &Card) -> bool {
        (self.label.is_empty() || c.labels.iter().any(|l| l == &self.label))
            && (self.assignee.is_empty() || c.assignee == self.assignee)
            && (self.repo.is_empty() || c.repo == self.repo)
            && (self.text.trim().is_empty() || {
                let t = self.text.trim().to_lowercase();
                c.title.to_lowercase().contains(&t)
                    || c.body.to_lowercase().contains(&t)
                    || c.key.to_lowercase().contains(&t)
                    || c.labels.iter().any(|l| l.to_lowercase().contains(&t))
            })
    }
}

/// Sorts in place; `desc` flips the direction. Ties fall back to the key.
pub fn sort_cards(cards: &mut [&Card], key: SortKey, desc: bool) {
    cards.sort_by(|a, b| {
        let o = match key {
            SortKey::Updated => a.updated_at.cmp(&b.updated_at),
            SortKey::Created => a.created_at.cmp(&b.created_at),
            SortKey::Priority => a.priority.cmp(&b.priority),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        };
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| a.key.cmp(&b.key))
    });
}

fn split_labels(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Drag payload.
#[derive(Clone)]
struct Dragged {
    id: String,
    version: i64,
    status: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Edit {
    title: String,
    body: String,
    status: String,
    priority: i32,
    labels: String,
    assignee: String,
    repo: String,
    branch: String,
}

impl Edit {
    fn of(c: &Card) -> Self {
        Self {
            title: c.title.clone(),
            body: c.body.clone(),
            status: c.status.clone(),
            priority: c.priority,
            labels: c.labels.join(", "),
            assignee: c.assignee.clone(),
            repo: c.repo.clone(),
            branch: c.branch.clone(),
        }
    }
}

struct Drawer {
    id: String,
    base_version: i64,
    loaded: Edit,
    buf: Edit,
    activity: Loader<Vec<CardActivity>>,
    node: Option<String>,
    confirm_delete: bool,
    claims: claims::ClaimUi,
    estimate: Loader<kls_core::estimates::Estimate>,
    tab: tabs::DrawerTab,
    issue: Loader<github::IssueView>,
    work: Loader<tabs::Work>,
}

pub struct Screen {
    cards: Loader<Vec<Card>>,
    workflows: Loader<Vec<kls_store::engine::Workflow>>,
    sync: Loader<Vec<domain::SyncRow>>,
    /// The Review tab's keyboard cursor (J/K).
    review_cursor: Option<usize>,
    /// GitHub mirror stamp: last sync and repositories mirrored.
    stamp: Loader<(i64, usize)>,
    tab: Tab,
    filters: Filters,
    sort: SortKey,
    desc: bool,
    drawer: Option<Drawer>,
    pending_focus: Option<String>,
    new_card: Option<Edit>,
    lens: bulk::Lens,
    group: bulk::Group,
    select_mode: bool,
    selected: std::collections::HashSet<String>,
    send_wf: String,
    lane_salt: String,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        Self {
            cards: Loader::new(model, &["card."], |core| async move { core.cards().await }),
            workflows: Loader::new(model, &["workflow."], |core| async move {
                core.store().workflows(false).await
            }),
            sync: Loader::new(model, &["card.", "github.issue"], |core| async move {
                core.sync_conflicts_3way().await
            }),
            review_cursor: None,
            stamp: Loader::new(model, &["github."], |core| async move {
                let repos = core.store().gh_managed_repos().await?;
                let last = repos.iter().map(|r| r.synced_at).max().unwrap_or(0);
                Ok((
                    last,
                    repos
                        .iter()
                        .filter(|r| r.settings.kanban_mode != "off" || r.mirror_issues)
                        .count(),
                ))
            }),
            tab: Tab::Board,
            filters: Filters::default(),
            sort: SortKey::Updated,
            desc: true,
            drawer: None,
            pending_focus: None,
            new_card: None,
            lens: bulk::Lens::All,
            group: bulk::Group::None,
            select_mode: false,
            selected: Default::default(),
            send_wf: String::new(),
            lane_salt: String::new(),
        }
    }

    /// Opens a card by id or key.
    pub fn focus(&mut self, id: &str) {
        self.pending_focus = Some(id.to_string());
    }

    fn open(&mut self, model: &Model, c: &Card) {
        let id = c.id.clone();
        let activity = Loader::new(model, &["card."], move |core| {
            let id = id.clone();
            async move { core.card_activity(&id).await }
        });
        let eid = c.id.clone();
        let estimate = Loader::new(model, &["card.", "session.", "settings."], move |core| {
            let id = eid.clone();
            async move { kls_core::estimates::for_card(&core, &id).await }
        });
        self.drawer = Some(Drawer {
            id: c.id.clone(),
            base_version: c.version,
            loaded: Edit::of(c),
            buf: Edit::of(c),
            activity,
            node: None,
            confirm_delete: false,
            claims: claims::ClaimUi::default(),
            estimate,
            tab: tabs::DrawerTab::Card,
            issue: github::loader(model, &c.id),
            work: tabs::work_loader(model, &c.id),
        });
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = palette(ui.ctx());
        let cards = match self.cards.get() {
            None => {
                kit::page_header(ui, icons::KANBAN, "Board", |_| {});
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                kit::page_header(ui, icons::KANBAN, "Board", |_| {});
                ui.colored_label(p.red, e);
                return;
            }
            Some(Ok(c)) => c,
        };
        if let Some(f) = self.pending_focus.take() {
            match cards.iter().find(|c| c.id == f || c.key == f) {
                Some(c) => {
                    let c = c.clone();
                    self.open(model, &c);
                }
                None => model.notify(Notice::Error(format!("No card {f} on the board"))),
            }
        }

        self.header(ui, model, &cards);
        self.filter_bar(ui, &cards);
        bulk::toolbar(
            ui,
            &mut self.lens,
            &mut self.group,
            &mut self.select_mode,
            &cards,
        );
        {
            let wfs = self
                .workflows
                .get()
                .and_then(|r| r.ok())
                .unwrap_or_default();
            bulk::bulk_bar(ui, model, &mut self.selected, &wfs, &mut self.send_wf);
        }
        ui.add_space(14.0);

        // The drawer first, so the central area takes what is left; when
        // narrow it sits above the lanes instead.
        if self.drawer.is_none() && ui.available_width() >= 1200.0 {
            let mut open = None;
            egui::Panel::right("board-rail")
                .resizable(false)
                .default_size(260.0)
                .frame(kit::card(ui).outer_margin(egui::Margin {
                    left: 14,
                    ..Default::default()
                }))
                .show(ui, |ui| open = right_rail(ui, &cards));
            if let Some(c) = open {
                self.open(model, &c);
            }
        }
        if self.drawer.is_some() {
            if ui.available_width() >= 820.0 {
                egui::Panel::right("board-drawer")
                    .resizable(true)
                    .default_size(360.0)
                    .min_size(300.0)
                    .frame(kit::glow(ui).outer_margin(egui::Margin {
                        left: 14,
                        ..Default::default()
                    }))
                    .show(ui, |ui| self.drawer_ui(ui, model, &cards));
            } else {
                let h = (ui.available_height() * 0.5).max(280.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
                    kit::glow(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        self.drawer_ui(ui, model, &cards)
                    });
                });
                ui.add_space(14.0);
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let mut shown: Vec<&Card> = cards
                    .iter()
                    .filter(|c| self.filters.matches(c) && bulk::lens_matches(self.lens, c))
                    .collect();
                sort_cards(&mut shown, self.sort, self.desc);
                match self.tab.status() {
                    None if self.tab == Tab::Sync => {
                        let rows = self.sync.get().and_then(|r| r.ok()).unwrap_or_default();
                        tabs::sync_tab(ui, model, &rows);
                    }
                    Some("done") => {
                        let rows: Vec<&Card> =
                            shown.into_iter().filter(|c| c.status == "done").collect();
                        let mut open = None;
                        tabs::done_by_day(ui, model, &rows, &mut open);
                        if let Some(c) = open {
                            self.open(model, &c);
                        }
                    }
                    None if self.tab == Tab::Stuck => {
                        let mut open = None;
                        claims::triage_tab(ui, model, &shown, &mut open);
                        if let Some(c) = open {
                            self.open(model, &c);
                        }
                    }
                    None if self.group != bulk::Group::None => {
                        let wfs = self
                            .workflows
                            .get()
                            .and_then(|r| r.ok())
                            .unwrap_or_default();
                        let mut keys: Vec<String> = shown
                            .iter()
                            .map(|c| bulk::group_key(self.group, c, &wfs))
                            .collect();
                        keys.sort();
                        keys.dedup();
                        egui::ScrollArea::vertical()
                            .id_salt("lanes")
                            .show(ui, |ui| {
                                for k in keys {
                                    let lane: Vec<&Card> = shown
                                        .iter()
                                        .copied()
                                        .filter(|c| bulk::group_key(self.group, c, &wfs) == k)
                                        .collect();
                                    egui::CollapsingHeader::new(
                                        RichText::new(format!("{k} · {}", lane.len())).strong(),
                                    )
                                    .id_salt(("lane", &k))
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        ui.set_min_height(300.0);
                                        self.lane_salt = k.clone();
                                        self.columns(ui, model, &lane);
                                    });
                                }
                            });
                        self.lane_salt.clear();
                    }
                    None => self.columns(ui, model, &shown),
                    Some(s) => {
                        let rows: Vec<&Card> =
                            shown.into_iter().filter(|c| c.status == s).collect();
                        self.list(ui, model, &rows)
                    }
                }
            });
        self.new_card_dialog(ui.ctx(), model);
    }

    fn new_card_draft(&self) -> Edit {
        Edit {
            status: self.tab.status().unwrap_or("triage").into(),
            ..Default::default()
        }
    }

    fn tab_button(&mut self, ui: &mut egui::Ui, t: Tab, cards: &[Card]) {
        let n = match (t, t.status()) {
            (Tab::Sync, _) => self
                .sync
                .get()
                .and_then(|r| r.ok())
                .map(|v| v.len())
                .unwrap_or(0),
            (Tab::Stuck, _) => cards
                .iter()
                .filter(|c| domain::cards::is_blocked(c) || domain::cards::is_held(c))
                .count(),
            (_, Some(s)) => cards.iter().filter(|c| c.status == s).count(),
            (_, None) => cards.len(),
        };
        if kit::tab_rich(
            ui,
            RichText::new(format!("{} {n}", t.title())).size(13.0),
            self.tab == t,
            0.0,
        )
        .clicked()
        {
            self.tab = t;
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, _model: &Model, cards: &[Card]) {
        let narrow = ui.available_width() < 980.0;
        let mut new = false;
        let stamp = self.stamp.get();
        kit::page_header(ui, icons::KANBAN, "Board", |ui| {
            if theme::primary_button(ui, &icons::label(icons::PLUS, "New card")).clicked() {
                new = true;
            }
            if let Some(Ok((last, repos))) = stamp {
                let p = palette(ui.ctx());
                let now = kls_store::now_ms();
                let text = if last > 0 {
                    format!("{repos} repos · {}", fmt::ago(now, last))
                } else {
                    "not mirrored".to_string()
                };
                ui.label(
                    RichText::new(icons::label(icons::GITHUB_LOGO, &text))
                        .size(12.0)
                        .color(p.muted),
                )
                .on_hover_text("GitHub mirror: issues reach the board through the poller");
            }
            if !narrow {
                for t in Tab::ALL.into_iter().rev() {
                    self.tab_button(ui, t, cards);
                }
            }
        });
        if narrow {
            ui.horizontal_wrapped(|ui| {
                for t in Tab::ALL {
                    self.tab_button(ui, t, cards);
                }
            });
            ui.add_space(10.0);
        }
        if new {
            self.new_card = Some(self.new_card_draft());
        }
    }

    fn filter_bar(&mut self, ui: &mut egui::Ui, cards: &[Card]) {
        let mut labels: Vec<String> = cards.iter().flat_map(|c| c.labels.clone()).collect();
        let mut assignees: Vec<String> = cards
            .iter()
            .map(|c| c.assignee.clone())
            .filter(|a| !a.is_empty())
            .collect();
        let mut repos: Vec<String> = cards
            .iter()
            .map(|c| c.repo.clone())
            .filter(|r| !r.is_empty())
            .collect();
        for v in [&mut labels, &mut assignees, &mut repos] {
            v.sort();
            v.dedup();
        }
        kit::card(ui)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    kit::field(ui, "Filter", |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.filters.text)
                                .hint_text(icons::label(
                                    icons::MAGNIFYING_GLASS,
                                    "title, key or label",
                                ))
                                .desired_width(240.0),
                        );
                    });
                    kit::field(ui, "Repository", |ui| {
                        combo(
                            ui,
                            icons::GIT_BRANCH,
                            "All repositories",
                            &mut self.filters.repo,
                            &repos,
                        )
                    });
                    kit::field(ui, "Assignee", |ui| {
                        combo(
                            ui,
                            icons::USER,
                            "Anyone",
                            &mut self.filters.assignee,
                            &assignees,
                        )
                    });
                    kit::field(ui, "Label", |ui| {
                        combo(
                            ui,
                            icons::TAG,
                            "Any label",
                            &mut self.filters.label,
                            &labels,
                        )
                    });
                    kit::field(ui, "Sort", |ui| {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("board-sort")
                                .selected_text(icons::label(icons::FUNNEL, self.sort.title()))
                                .show_ui(ui, |ui| {
                                    for k in SortKey::ALL {
                                        ui.selectable_value(&mut self.sort, k, k.title());
                                    }
                                });
                            if kit::ghost_button(
                                ui,
                                if self.desc {
                                    icons::ARROW_DOWN
                                } else {
                                    icons::ARROW_UP
                                },
                            )
                            .on_hover_text(if self.desc { "Descending" } else { "Ascending" })
                            .clicked()
                            {
                                self.desc = !self.desc;
                            }
                        });
                    });
                    if self.filters != Filters::default() {
                        kit::field(ui, " ", |ui| {
                            if kit::ghost_button(ui, &icons::label(icons::X, "Clear")).clicked() {
                                self.filters = Filters::default();
                            }
                        });
                    }
                });
            });
    }

    fn columns(&mut self, ui: &mut egui::Ui, model: &Model, shown: &[&Card]) {
        let p = palette(ui.ctx());
        let mut open: Option<Card> = None;
        let mut toggled: Vec<String> = Vec::new();
        let mut dropped: Option<(Dragged, &'static str)> = None;
        let gap = 12.0;
        let n = LANES.len() as f32;
        let width = ((ui.available_width() - gap * (n - 1.0)) / n).max(220.0);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                let lane_h = ui.available_height().max(420.0);
                for status in LANES {
                    let in_col: Vec<&&Card> =
                        shown.iter().filter(|c| lane_of(c) == status).collect();
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, lane_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                            let (_, payload) =
                                ui.dnd_drop_zone::<Dragged, ()>(kit::card(ui), |ui| {
                                    ui.set_width(width - 2.0);
                                    ui.set_min_height(lane_h - 2.0);
                                    egui::Frame::new()
                                        .inner_margin(egui::Margin::symmetric(14, 10))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(icons::label(
                                                        status_icon(status),
                                                        &title_case(status),
                                                    ))
                                                    .size(14.0)
                                                    .strong(),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        ui.label(kit::mono(
                                                            &in_col.len().to_string(),
                                                            12.0,
                                                            p.muted,
                                                        ));
                                                    },
                                                );
                                            });
                                        });
                                    let r = ui.min_rect();
                                    ui.painter().hline(
                                        r.x_range(),
                                        r.bottom(),
                                        egui::Stroke::new(2.0, lane_color(&p, status)),
                                    );
                                    egui::Frame::new().inner_margin(10).show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        egui::ScrollArea::vertical()
                                            .id_salt(("col", status, self.lane_salt.clone()))
                                            .max_height(ui.available_height().max(300.0))
                                            .auto_shrink([false, true])
                                            .show(ui, |ui| {
                                                ui.spacing_mut().item_spacing.y = 10.0;
                                                for c in in_col {
                                                    if self.card_tile(ui, c) {
                                                        let toggle = self.select_mode
                                                            || ui.input(|i| i.modifiers.command);
                                                        if toggle {
                                                            toggled.push(c.id.clone());
                                                        } else {
                                                            open = Some((*c).clone());
                                                        }
                                                    }
                                                }
                                            });
                                    });
                                });
                            if let Some(p) = payload {
                                dropped = Some(((*p).clone(), status));
                            }
                        },
                    );
                }
            });
        });
        for id in toggled {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
        }
        if let Some(c) = open {
            self.open(model, &c);
        }
        if let Some((d, to)) = dropped
            && d.status != to
        {
            model.act(move |m| async move {
                let core = m.core();
                if to == "blocked" {
                    core.block_card(&d.id, d.version, "", OWNER).await?;
                } else if d.status == "blocked" {
                    // Out of Blocked: back to the backlog first (claim and attempts cleared).
                    let c = core.retry_card(&d.id, d.version, "", OWNER).await?;
                    if to != "backlog" {
                        core.move_card(&c.id, c.version, to, OWNER).await?;
                    }
                } else {
                    core.move_card(&d.id, d.version, to, OWNER).await?;
                }
                Ok(None)
            });
        }
    }

    /// One card in a column; true when it was clicked open.
    fn card_tile(&self, ui: &mut egui::Ui, c: &Card) -> bool {
        let p = palette(ui.ctx());
        let selected =
            self.drawer.as_ref().is_some_and(|d| d.id == c.id) || self.selected.contains(&c.id);
        let out = ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt(("card-tile", &c.id))
                .sense(egui::Sense::click()),
            |ui| {
                let hovered = ui.response().contains_pointer();
                let edge = if selected {
                    p.gold
                } else if hovered {
                    p.gold_edge
                } else {
                    p.line
                };
                egui::Frame::new()
                    .fill(if selected { p.panel2 } else { p.bg })
                    .stroke(egui::Stroke::new(1.0, edge))
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.horizontal(|ui| {
                            ui.dnd_drag_source(
                                ui.scope_id().with(("card-drag", &c.id)),
                                Dragged {
                                    id: c.id.clone(),
                                    version: c.version,
                                    status: lane_of(c).to_string(),
                                },
                                |ui| {
                                    ui.label(RichText::new(icons::DOTS_THREE).color(p.muted))
                                        .on_hover_text("Drag to move");
                                },
                            );
                            ui.label(kit::mono(&c.key, 11.0, p.muted));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !c.repo.is_empty() {
                                        ui.add(
                                            egui::Label::new(kit::mono(&c.repo, 11.0, p.muted))
                                                .truncate(),
                                        );
                                    }
                                },
                            );
                        });
                        ui.add(
                            egui::Label::new(RichText::new(&c.title).size(14.0).strong()).wrap(),
                        );
                        if !c.labels.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                for l in &c.labels {
                                    kit::tag(ui, l);
                                }
                            });
                        }
                        claims::tile_marks(ui, c);
                        ui.horizontal(|ui| {
                            let who = if c.assignee.is_empty() {
                                "unassigned".to_string()
                            } else {
                                format!("@{}", c.assignee)
                            };
                            ui.label(RichText::new(who).size(12.0).color(p.muted));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if c.priority != 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} {}",
                                                icons::FLAG,
                                                c.priority
                                            ))
                                            .size(12.0)
                                            .color(priority_color(&p, c.priority)),
                                        )
                                        .on_hover_text("Priority");
                                    }
                                },
                            );
                        });
                    });
            },
        );
        out.response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
    }

    fn list(&mut self, ui: &mut egui::Ui, model: &Model, rows: &[&Card]) {
        let p = palette(ui.ctx());
        if rows.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(20))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icons::label(icons::TRAY, "No cards")).color(p.muted),
                        );
                        if theme::primary_button(ui, &icons::label(icons::PLUS, "New card"))
                            .clicked()
                        {
                            self.new_card = Some(self.new_card_draft());
                        }
                    });
                });
            return;
        }
        let now = kls_store::now_ms();
        let mut open: Option<Card> = None;
        let review = self.tab == Tab::Review;
        let backlog = self.tab == Tab::Backlog;
        let done = self.tab == Tab::Done;
        // Review keys: J/K move the cursor, C confirms, X dismisses.
        let mut cursor = None;
        if review {
            if let Some(k) = tabs::review_key(ui.ctx()) {
                self.review_cursor = tabs::step_cursor(self.review_cursor, rows.len(), k);
                if let Some(c) = self.review_cursor.map(|i| rows[i]) {
                    match k {
                        tabs::ReviewKey::Confirm => move_act(model, c, "backlog"),
                        tabs::ReviewKey::Dismiss => dismiss_act(model, c),
                        _ => {}
                    }
                }
            }
            cursor = self.review_cursor.map(|i| i.min(rows.len() - 1));
            ui.label(
                RichText::new(icons::label(
                    icons::INFO,
                    "J / K move · C confirm · X dismiss",
                ))
                .size(12.0)
                .color(p.muted),
            );
        }
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    TableBuilder::new(ui)
                        .striped(false)
                        .sense(egui::Sense::click())
                        .resizable(true)
                        .column(Column::auto().at_least(80.0))
                        .column(Column::remainder().at_least(200.0).clip(true))
                        .column(Column::auto().at_least(40.0))
                        .column(Column::auto().at_least(100.0))
                        .column(Column::auto().at_least(80.0))
                        .column(Column::auto().at_least(100.0))
                        .column(Column::auto().at_least(70.0))
                        .column(Column::auto())
                        .header(30.0, |mut h| {
                            for t in [
                                "Key", "Title", "Pri", "Labels", "Assignee", "Repo", "Updated", "",
                            ] {
                                h.col(|ui| {
                                    kit::rule(ui);
                                    kit::th(ui, t);
                                });
                            }
                        })
                        .body(|mut body| {
                            for (i, c) in rows.iter().enumerate() {
                                body.row(34.0, |mut row| {
                                    row.set_selected(cursor == Some(i));
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(&c.key, 12.0, p.muted));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        if bits_link(ui, &c.title).clicked() {
                                            open = Some((*c).clone());
                                        }
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        if backlog {
                                            // Inline priority: drag or type, saved on release.
                                            let id = egui::Id::unique(("board-prio", &c.id));
                                            let mut v = ui
                                                .data(|d| d.get_temp::<i32>(id))
                                                .unwrap_or(c.priority);
                                            let r = ui.add(
                                                egui::DragValue::new(&mut v)
                                                    .range(
                                                        domain::PRIORITY_MIN..=domain::PRIORITY_MAX,
                                                    )
                                                    .speed(0.05),
                                            );
                                            ui.data_mut(|d| d.insert_temp(id, v));
                                            if (r.drag_stopped() || r.lost_focus())
                                                && v != c.priority
                                            {
                                                ui.data_mut(|d| d.remove::<i32>(id));
                                                let (cid, ver) = (c.id.clone(), c.version);
                                                model.act(move |m| async move {
                                                    let p = CardPatch {
                                                        priority: Some(v),
                                                        ..Default::default()
                                                    };
                                                    m.core()
                                                        .update_card(&cid, ver, p, OWNER)
                                                        .await?;
                                                    Ok(None)
                                                });
                                            } else if !r.dragged() && !r.has_focus() {
                                                ui.data_mut(|d| d.remove::<i32>(id));
                                            }
                                        } else if c.priority != 0 {
                                            ui.label(
                                                RichText::new(c.priority.to_string())
                                                    .color(priority_color(&p, c.priority)),
                                            );
                                        }
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        for l in &c.labels {
                                            kit::tag(ui, l);
                                        }
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(&c.assignee);
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(kit::mono(&c.repo, 12.0, p.muted));
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        ui.label(
                                            RichText::new(fmt::ago(now, c.updated_at))
                                                .color(p.muted),
                                        );
                                    });
                                    row.col(|ui| {
                                        kit::rule(ui);
                                        if review {
                                            if theme::primary_button(
                                                ui,
                                                &icons::label(icons::CHECK, "Confirm"),
                                            )
                                            .on_hover_text("To the backlog")
                                            .clicked()
                                            {
                                                move_act(model, c, "backlog");
                                            }
                                            if kit::ghost_button(
                                                ui,
                                                &icons::label(icons::X, "Dismiss"),
                                            )
                                            .on_hover_text("Done, not planned")
                                            .clicked()
                                            {
                                                dismiss_act(model, c);
                                            }
                                        }
                                        if backlog {
                                            for (icon, d) in
                                                [(icons::ARROW_UP, 1), (icons::ARROW_DOWN, -1)]
                                            {
                                                let to = (c.priority + d).clamp(
                                                    domain::PRIORITY_MIN,
                                                    domain::PRIORITY_MAX,
                                                );
                                                if to != c.priority
                                                    && ui
                                                        .small_button(icon)
                                                        .on_hover_text("Priority")
                                                        .clicked()
                                                {
                                                    let (id, v) = (c.id.clone(), c.version);
                                                    model.act(move |m| async move {
                                                        let p = CardPatch {
                                                            priority: Some(to),
                                                            ..Default::default()
                                                        };
                                                        m.core()
                                                            .update_card(&id, v, p, OWNER)
                                                            .await?;
                                                        Ok(None)
                                                    });
                                                }
                                            }
                                        }
                                        if done
                                            && kit::ghost_button(
                                                ui,
                                                &icons::label(icons::ARROW_U_UP_LEFT, "Reopen"),
                                            )
                                            .on_hover_text("Back to review")
                                            .clicked()
                                        {
                                            move_act(model, c, "review");
                                        }
                                        ui.menu_button(icons::ARROW_RIGHT, |ui| {
                                            for s in STATUSES {
                                                if s != c.status
                                                    && ui
                                                        .button(icons::label(
                                                            status_icon(s),
                                                            &title_case(s),
                                                        ))
                                                        .clicked()
                                                {
                                                    move_act(model, c, s);
                                                    ui.close();
                                                }
                                            }
                                        })
                                        .response
                                        .on_hover_text("Move to");
                                    });
                                    if row.response().clicked() {
                                        open = Some((*c).clone());
                                    }
                                });
                            }
                        });
                });
        });
        if let Some(c) = open {
            self.open(model, &c);
        }
    }

    fn drawer_ui(&mut self, ui: &mut egui::Ui, model: &Model, cards: &[Card]) {
        let p = palette(ui.ctx());
        let workflows = self
            .workflows
            .get()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        let Some(d) = self.drawer.as_mut() else {
            return;
        };
        let Some(card) = cards.iter().find(|c| c.id == d.id).cloned() else {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.label(RichText::new(icons::label(icons::TRASH, "Deleted")).color(p.muted));
                    if kit::ghost_button(ui, &icons::label(icons::X, "Close")).clicked() {
                        self.drawer = None;
                    }
                });
            return;
        };
        let dirty = d.buf != d.loaded;
        if card.version != d.base_version && !dirty {
            d.base_version = card.version;
            d.loaded = Edit::of(&card);
            d.buf = d.loaded.clone();
        }
        let mut close = false;
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(kit::mono(
                        &format!("{} · {}", card.key, title_case(&card.status)),
                        12.0,
                        p.gold_text,
                    ));
                    ui.label(kit::mono(&format!("v{}", card.version), 11.0, p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::ghost_button(ui, icons::X)
                            .on_hover_text("Close")
                            .clicked()
                        {
                            close = true;
                        }
                        if kit::ghost_button(ui, icons::LINK)
                            .on_hover_text("Copy a link to this card")
                            .clicked()
                        {
                            ui.ctx()
                                .copy_text(format!("keleustes://board/{}", card.key));
                        }
                        if kit::ghost_button(ui, icons::PULSE)
                            .on_hover_text("Events for this card")
                            .clicked()
                        {
                            model.go(crate::Screen::Events, Some(format!("card:{}", card.id)));
                        }
                    });
                });
            });
        let r = ui.min_rect();
        ui.painter()
            .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, p.line));
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 10.0;
                    if card.version != d.base_version {
                        ui.horizontal_wrapped(|ui| {
                            kit::chip(
                                ui,
                                &icons::label(icons::WARNING, "Changed elsewhere"),
                                p.amber,
                            );
                            if kit::ghost_button(
                                ui,
                                &icons::label(icons::ARROWS_CLOCKWISE, "Reload"),
                            )
                            .on_hover_text("Discard my edits and reload")
                            .clicked()
                            {
                                d.base_version = card.version;
                                d.loaded = Edit::of(&card);
                                d.buf = d.loaded.clone();
                            }
                        });
                    }
                    ui.horizontal_wrapped(|ui| {
                        for t in tabs::DrawerTab::ALL {
                            let (icon, label) = t.face();
                            if kit::tab(ui, &icons::label(icon, label), d.tab == t).clicked() {
                                d.tab = t;
                            }
                        }
                    });
                    match d.tab {
                        tabs::DrawerTab::GitHub => github::tab(ui, model, &d.issue, &card),
                        tabs::DrawerTab::Sessions => tabs::sessions_tab(ui, model, &d.work),
                        tabs::DrawerTab::Run => tabs::run_tab(ui, model, &d.work),
                        _ => {}
                    }
                    if d.tab == tabs::DrawerTab::Card {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.buf.title)
                                .font(egui::FontId::proportional(19.0))
                                .hint_text("Title")
                                .desired_width(f32::INFINITY),
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut d.buf.body)
                                .hint_text("Body")
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        );
                        tabs::ticket_gaps(
                            ui,
                            &domain::rules::ticket_issues(&d.buf.title, &d.buf.body),
                        );
                        egui::Grid::new("card-edit")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .show(ui, |ui| {
                                ui.label(kit::caps("Status", p.muted));
                                status_combo(ui, "drawer-status", &mut d.buf.status);
                                ui.end_row();
                                ui.label(kit::caps("Priority", p.muted));
                                ui.add(egui::Slider::new(
                                    &mut d.buf.priority,
                                    domain::PRIORITY_MIN..=domain::PRIORITY_MAX,
                                ));
                                ui.end_row();
                                ui.label(kit::caps("Labels", p.muted));
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.buf.labels).hint_text("a, b"),
                                );
                                ui.end_row();
                                ui.label(kit::caps("Assignee", p.muted));
                                ui.text_edit_singleline(&mut d.buf.assignee);
                                ui.end_row();
                                ui.label(kit::caps("Repo", p.muted));
                                ui.text_edit_singleline(&mut d.buf.repo);
                                ui.end_row();
                                ui.label(kit::caps("Branch", p.muted));
                                ui.text_edit_singleline(&mut d.buf.branch);
                                ui.end_row();
                            });
                        ui.horizontal_wrapped(|ui| {
                            let save = ui
                                .add_enabled_ui(dirty, |ui| {
                                    theme::primary_button(
                                        ui,
                                        &icons::label(icons::FLOPPY_DISK, "Save"),
                                    )
                                })
                                .inner;
                            if save.clicked() {
                                let (id, base, b) =
                                    (card.id.clone(), d.base_version, d.buf.clone());
                                let patch = CardPatch {
                                    title: Some(b.title),
                                    body: Some(b.body),
                                    status: Some(b.status),
                                    priority: Some(b.priority),
                                    labels: Some(split_labels(&b.labels)),
                                    assignee: Some(b.assignee),
                                    repo: Some(b.repo),
                                    branch: Some(b.branch),
                                    ..Default::default()
                                };
                                // Treat the save as loaded so the next version resets the buffer.
                                d.loaded = d.buf.clone();
                                model.act(move |m| async move {
                                    m.core().update_card(&id, base, patch, OWNER).await?;
                                    Ok(Some(Notice::Info("Card saved".into())))
                                });
                            }
                            let revert = ui
                                .add_enabled_ui(dirty, |ui| {
                                    kit::ghost_button(
                                        ui,
                                        &icons::label(icons::ARROW_COUNTER_CLOCKWISE, "Revert"),
                                    )
                                })
                                .inner;
                            if revert.clicked() {
                                d.buf = d.loaded.clone();
                            }
                            if d.confirm_delete {
                                if kit::danger_button(ui, &icons::label(icons::TRASH, "Confirm"))
                                    .on_hover_text("Delete this card")
                                    .clicked()
                                {
                                    let (id, v) = (card.id.clone(), card.version);
                                    model.act(move |m| async move {
                                        m.core().delete_card(&id, v, OWNER).await?;
                                        Ok(Some(Notice::Info("Card deleted".into())))
                                    });
                                    close = true;
                                }
                                if kit::ghost_button(ui, &icons::label(icons::X, "Keep")).clicked()
                                {
                                    d.confirm_delete = false;
                                }
                            } else if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete"))
                                .clicked()
                            {
                                d.confirm_delete = true;
                            }
                            if !card.pr_url.is_empty()
                                && kit::ghost_button(
                                    ui,
                                    &icons::label(icons::GIT_PULL_REQUEST, "Pull request"),
                                )
                                .on_hover_text(&card.pr_url)
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(&card.pr_url));
                            }
                        });

                        ui.add_space(4.0);
                        claims::drawer_sections(ui, model, &mut d.claims, &card, &workflows);
                        ui.horizontal_wrapped(|ui| bulk::assign_menu(ui, model, &card, &[]));
                        if let Some(Ok(e)) = d.estimate.get() {
                            estimate_line(ui, &e);
                        }
                        classify_line(ui, model, &card);
                        ui.add_space(4.0);
                        send_ui(ui, model, d, &card);
                    }
                    if d.tab == tabs::DrawerTab::Activity {
                        ui.add_space(4.0);
                        ui.label(kit::caps(&icons::label(icons::PULSE, "Activity"), p.muted));
                        match d.activity.get() {
                            None => {
                                ui.spinner();
                            }
                            Some(Err(e)) => {
                                ui.colored_label(p.red, e);
                            }
                            Some(Ok(rows)) => {
                                let now = kls_store::now_ms();
                                ui.spacing_mut().item_spacing.y = 6.0;
                                for a in rows {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(kit::mono(&clock(now, a.at), 12.0, p.muted))
                                            .on_hover_text(fmt::when(a.at));
                                        ui.label(RichText::new(&a.kind).size(13.0));
                                        ui.label(RichText::new(&a.actor).size(13.0).color(p.muted));
                                        if a.detail_json != "{}" {
                                            ui.label(
                                                RichText::new(&a.detail_json)
                                                    .size(12.0)
                                                    .color(p.muted),
                                            );
                                        }
                                    });
                                }
                            }
                        }
                    }
                });
        });
        if close {
            self.drawer = None;
        }
    }

    fn new_card_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some(mut e) = self.new_card.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new(icons::label(icons::PLUS, "New card"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let p = palette(ui.ctx());
                egui::Grid::new("new-card")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(kit::caps("Title", p.muted));
                        ui.text_edit_singleline(&mut e.title);
                        ui.end_row();
                        ui.label(kit::caps("Status", p.muted));
                        status_combo(ui, "new-status", &mut e.status);
                        ui.end_row();
                        ui.label(kit::caps("Priority", p.muted));
                        ui.add(egui::Slider::new(
                            &mut e.priority,
                            domain::PRIORITY_MIN..=domain::PRIORITY_MAX,
                        ));
                        ui.end_row();
                        ui.label(kit::caps("Labels", p.muted));
                        ui.add(egui::TextEdit::singleline(&mut e.labels).hint_text("a, b"));
                        ui.end_row();
                        ui.label(kit::caps("Assignee", p.muted));
                        ui.text_edit_singleline(&mut e.assignee);
                        ui.end_row();
                        ui.label(kit::caps("Repo", p.muted));
                        ui.add(egui::TextEdit::singleline(&mut e.repo).hint_text("owner/name"));
                        ui.end_row();
                    });
                ui.label(kit::caps("Body", p.muted));
                ui.add(egui::TextEdit::multiline(&mut e.body).desired_rows(6));
                tabs::ticket_gaps(ui, &domain::rules::ticket_issues(&e.title, &e.body));
                let ok = !e.title.trim().is_empty();
                let create = ui
                    .add_enabled_ui(ok, |ui| {
                        theme::primary_button(ui, &icons::label(icons::PLUS, "Create"))
                    })
                    .inner;
                if create.clicked() {
                    let draft = CardDraft {
                        title: e.title.clone(),
                        body: e.body.clone(),
                        status: e.status.clone(),
                        priority: e.priority,
                        labels: split_labels(&e.labels),
                        assignee: e.assignee.clone(),
                        repo: e.repo.clone(),
                        ..Default::default()
                    };
                    model.act(move |m| async move {
                        let c = m.core().create_card(draft, OWNER).await?;
                        m.go(crate::Screen::Board, Some(c.id.clone()));
                        Ok(Some(Notice::Info(format!("Created {}", c.key))))
                    });
                    done = true;
                }
            });
        if open && !done {
            self.new_card = Some(e);
        }
    }
}

fn send_ui(ui: &mut egui::Ui, model: &Model, d: &mut Drawer, card: &Card) {
    let p = palette(ui.ctx());
    let snap = model.snapshot();
    let rc: Vec<(String, String)> = snap
        .nodes
        .iter()
        .filter(|n| {
            n.revoked_at.is_none()
                && snap.online.contains(&n.id)
                && n.host.capabilities.remote_control
        })
        .map(|n| {
            let c = &n.host.capabilities;
            let mut caps = vec![n.host.os.clone(), "rc".to_string()];
            if c.gh_authenticated {
                caps.push("gh".into());
            }
            (n.id.clone(), format!("{} — {}", n.name, caps.join(", ")))
        })
        .collect();
    kit::field(ui, "Send to a node", |ui| {
        if rc.is_empty() {
            ui.label(
                RichText::new(icons::label(icons::PLUGS_CONNECTED, "No rc node")).color(p.muted),
            )
            .on_hover_text("Start `belai rc` on a joined host");
            return;
        }
        if d.node
            .as_ref()
            .is_none_or(|n| !rc.iter().any(|(id, _)| id == n))
        {
            d.node = Some(rc[0].0.clone());
        }
        ui.horizontal_wrapped(|ui| {
            let current = d.node.clone().unwrap_or_default();
            let name = rc
                .iter()
                .find(|(id, _)| *id == current)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("send-node")
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for (id, n) in &rc {
                        ui.selectable_value(&mut d.node, Some(id.clone()), n);
                    }
                });
            if theme::primary_button(ui, &icons::label(icons::PAPER_PLANE_TILT, "Start session"))
                .clicked()
            {
                let node = current;
                let card = card.clone();
                model.act(move |m| async move {
                    let spec = DispatchSpec {
                        kind: "session".into(),
                        repo: card.repo.clone(),
                        branch: card.branch.clone(),
                        prompt: domain::card_prompt(&card),
                        card_id: card.id.clone(),
                        mode: "agent".into(),
                        ..Default::default()
                    };
                    let core = m.core();
                    let dispatch = core.gateway()?.dispatch(&node, spec).await?;
                    core.note_card_dispatch(&card, &node, &dispatch).await?;
                    if matches!(card.status.as_str(), "triage" | "backlog" | "todo") {
                        // Best effort: a concurrent edit just leaves the column as it is.
                        let _ = core.move_card(&card.id, card.version, "doing", OWNER).await;
                    }
                    Ok(Some(Notice::Info(format!(
                        "Sent {} to a session",
                        card.key
                    ))))
                });
            }
        });
    });
}

/// A lane's accent (the line under its header).
fn lane_color(p: &theme::Palette, status: &str) -> egui::Color32 {
    match status {
        "todo" => p.blue,
        "doing" => p.cyan,
        "review" => p.violet,
        "done" => p.green,
        "blocked" => p.red,
        _ => p.muted,
    }
}

/// Priority colour: higher is more urgent.
fn priority_color(p: &theme::Palette, prio: i32) -> egui::Color32 {
    match prio {
        2.. => p.red,
        1 => p.amber,
        _ => p.muted,
    }
}

/// A title link in the text colour.
fn bits_link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui.ctx());
    ui.add(
        egui::Label::new(RichText::new(text).size(13.0).color(p.text))
            .truncate()
            .sense(egui::Sense::click()),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn move_act(model: &Model, c: &Card, to: &'static str) {
    let (id, v) = (c.id.clone(), c.version);
    model.act(move |m| async move {
        m.core().move_card(&id, v, to, OWNER).await?;
        Ok(None)
    });
}

fn dismiss_act(model: &Model, c: &Card) {
    let (id, v) = (c.id.clone(), c.version);
    model.act(move |m| async move {
        m.core().dismiss_card(&id, v, "", OWNER).await?;
        Ok(None)
    });
}

fn combo(ui: &mut egui::Ui, icon: &str, any: &str, value: &mut String, options: &[String]) {
    let text = if value.is_empty() {
        format!("{icon}  {any}")
    } else {
        format!("{icon}  {value}")
    };
    egui::ComboBox::from_id_salt(("board-filter", any))
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, String::new(), any);
            for o in options {
                ui.selectable_value(value, o.clone(), o);
            }
        });
}

fn status_combo(ui: &mut egui::Ui, id: &str, value: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(title_case(value))
        .show_ui(ui, |ui| {
            for s in STATUSES {
                ui.selectable_value(value, s.to_string(), title_case(s));
            }
        });
}

/// The right rail when no card is open: what waits on you on the board.
fn right_rail(ui: &mut egui::Ui, cards: &[Card]) -> Option<Card> {
    let p = palette(ui.ctx());
    let now = kls_store::now_ms();
    let mut open = None;
    egui::Frame::new().inner_margin(12).show(ui, |ui| {
        ui.label(kit::caps(
            &icons::label(icons::BELL, "Waiting on you"),
            p.gold_text,
        ));
        let groups: [(&str, &str, Vec<&Card>); 3] = [
            (
                icons::HAND_PALM,
                "Held",
                cards.iter().filter(|c| domain::cards::is_held(c)).collect(),
            ),
            (
                icons::LOCK_SIMPLE,
                "Claims lapsed",
                cards
                    .iter()
                    .filter(|c| domain::cards::claim(c).lapsed(now))
                    .collect(),
            ),
            (
                icons::EYE,
                "In review",
                cards.iter().filter(|c| c.status == "review").collect(),
            ),
        ];
        let mut any = false;
        for (icon, label, rows) in groups {
            if rows.is_empty() {
                continue;
            }
            any = true;
            ui.add_space(6.0);
            ui.label(
                RichText::new(icons::label(icon, &format!("{label} {}", rows.len()))).strong(),
            );
            for c in rows.into_iter().take(8) {
                if bits_link(ui, &format!("{} {}", c.key, c.title)).clicked() {
                    open = Some(c.clone());
                }
            }
        }
        if !any {
            ui.label(RichText::new(icons::label(icons::CHECK_CIRCLE, "Nothing")).color(p.muted));
        }
    });
    open
}

/// A done card's class (done-card classification), or Classify.
fn classify_line(ui: &mut egui::Ui, model: &Model, card: &Card) {
    use kls_core::classify;
    let p = palette(ui.ctx());
    if card.status != "done" || card.pr_url.trim().is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(kit::caps(&icons::label(icons::SPARKLE, "Class"), p.muted));
        match classify::class_of(card) {
            Some(c) => {
                kit::chip(ui, &c.tag, p.violet).on_hover_text(format!(
                    "{:.0}% · {} {} · node {} · {}",
                    c.confidence * 100.0,
                    c.provider,
                    c.model,
                    c.node_id,
                    c.origin
                ));
            }
            None if classify::eligible(card) => {
                if kit::ghost_button(ui, &icons::label(icons::SPARKLE, "Classify"))
                    .on_hover_text("Ask a node's model which kind of change this is")
                    .clicked()
                {
                    let id = card.id.clone();
                    model.act(move |m| async move {
                        let out = m.core().classify_card(&id, OWNER).await?;
                        Ok(Some(Notice::Info(match out {
                            classify::Outcome::Classified(c) => format!("Classified {}", c.tag),
                            classify::Outcome::Undecided { .. } => "Unsure; left as is".into(),
                            _ => "Nothing to do".into(),
                        })))
                    });
                }
            }
            None => {
                ui.label(RichText::new("labelled").color(p.muted));
            }
        }
    });
}

/// By-hand estimate of a card (governance `kls_core::estimates`).
pub(crate) fn estimate_line(ui: &mut egui::Ui, e: &kls_core::estimates::Estimate) {
    let p = palette(ui.ctx());
    if e.by_hand_hours <= 0.0 && e.model_cost.is_none() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(kit::caps(&icons::label(icons::COINS, "Estimate"), p.muted));
        ui.label(format!("{:.0} h by hand", e.by_hand_hours));
        if e.by_hand_value > 0.0 {
            ui.label(RichText::new(format!("${:.0}", e.by_hand_value)).color(p.muted));
        }
        if let Some(c) = e.model_cost {
            ui.label(RichText::new(format!("model ${c:.2}")).color(p.muted));
        }
        if let Some(r) = e.return_on_total {
            ui.label(RichText::new(format!("return {r:.1}×")).color(p.green));
        }
    })
    .response
    .on_hover_text(e.inputs.sources.join(", "));
}

/// The icon for a board status column.
pub fn status_icon(s: &str) -> &'static str {
    match s {
        "triage" => icons::FUNNEL,
        "backlog" => icons::TRAY,
        "todo" => icons::LIST_CHECKS,
        "blocked" => icons::PROHIBIT,
        "doing" => icons::PLAY,
        "review" => icons::EYE,
        "done" => icons::CHECK_CIRCLE,
        _ => icons::CIRCLE,
    }
}

pub fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(key: &str, title: &str, prio: i32, updated: i64) -> Card {
        Card {
            key: key.into(),
            title: title.into(),
            priority: prio,
            updated_at: updated,
            labels: vec!["bug".into()],
            assignee: "ana".into(),
            repo: "acme/api".into(),
            ..Default::default()
        }
    }

    #[test]
    fn filters_and_sort() {
        let a = card("K-1", "Alpha", 1, 30);
        let mut b = card("K-2", "beta login", 3, 10);
        b.labels = vec!["ui".into()];
        b.assignee = String::new();
        let c = card("K-3", "Gamma", -1, 20);
        let all = [&a, &b, &c];

        let f = Filters {
            label: "bug".into(),
            ..Default::default()
        };
        assert_eq!(all.iter().filter(|x| f.matches(x)).count(), 2);
        let f = Filters {
            text: "LOGIN".into(),
            ..Default::default()
        };
        assert!(f.matches(&b) && !f.matches(&a));
        let f = Filters {
            assignee: "ana".into(),
            repo: "acme/api".into(),
            ..Default::default()
        };
        assert_eq!(all.iter().filter(|x| f.matches(x)).count(), 2);

        let mut v = all.to_vec();
        sort_cards(&mut v, SortKey::Priority, true);
        assert_eq!(
            v.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
            ["K-2", "K-1", "K-3"]
        );
        sort_cards(&mut v, SortKey::Updated, false);
        assert_eq!(
            v.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
            ["K-2", "K-3", "K-1"]
        );
        sort_cards(&mut v, SortKey::Title, false);
        assert_eq!(v[0].title, "Alpha");
        assert_eq!(split_labels(" a, ,b "), vec!["a", "b"]);
        assert_eq!(title_case("review"), "Review");
    }
}
