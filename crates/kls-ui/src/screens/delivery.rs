//! Delivery screen. Owner: M4 GitHub workstream.
//!
//! What was delivered in a period and the people it needed: outcomes,
//! decisions, people, contributors and one contributor, as tables and
//! charts, each exportable as CSV to a path on disk.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::theme;
use egui_extras::{Column, TableBuilder};
use egui_plot::{Bar, BarChart, Plot};
use kls_core::estimates::{self, Estimate, EstimateInputs, EstimateSettings, GroupBy, PerfRow};
use kls_core::github::{
    self as delivery, ContributorDetail, ContributorsReport, CoverageReport, DecisionsReport,
    LedgerFlow, OutcomeDetail, OutcomesReport, PeopleReport, Range, Scope,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const READY: bool = true;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Outcomes,
    Decisions,
    People,
    Contributors,
    Contributor,
    Estimate,
    Performance,
    Lifecycle,
    Outcome,
}

impl Tab {
    fn word(self) -> &'static str {
        match self {
            Tab::Outcomes => "outcomes",
            Tab::Decisions => "decisions",
            Tab::People => "people",
            Tab::Contributors => "contributors",
            Tab::Contributor => "contributor",
            Tab::Estimate => "estimate",
            Tab::Performance => "performance",
            Tab::Lifecycle => "lifecycle",
            Tab::Outcome => "outcome",
        }
    }
}

#[derive(Clone, Default, PartialEq)]
struct Params {
    days: i64,
    q: String,
    scope: ScopeSel,
    person: Option<String>,
    by: Option<GroupBy>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ScopeSel {
    #[default]
    All,
    Internal,
    External,
}

#[derive(Clone)]
struct Data {
    range: Range,
    outcomes: OutcomesReport,
    decisions: DecisionsReport,
    people: PeopleReport,
    contributors: ContributorsReport,
    detail: Option<ContributorDetail>,
    /// The range priced by hand (Settings > Estimates).
    estimate: Estimate,
    /// Outcome key -> (hours, value) a team would take by hand.
    by_hand: BTreeMap<String, (f64, f64)>,
    perf: Vec<PerfRow>,
}

pub struct Screen {
    tab: Tab,
    params: Arc<Mutex<Params>>,
    edit: Params,
    data: Loader<Data>,
    path: String,
    /// Board flow and unanswered GitHub events (engine `delivery`).
    flow: Loader<FlowData>,
    /// The outcome open in the Outcome tab.
    outcome_key: Arc<Mutex<Option<String>>>,
    outcome: Loader<Option<OutcomeDetail>>,
    /// Contributor work filtered to one part (started, decided, reviewed, signed).
    part: Option<String>,
    /// Stage flow read from the stored lifecycle ledger.
    ledger: Loader<Vec<LedgerFlow>>,
    /// The ledger against the source rows, per stage (7 days).
    coverage: Loader<CoverageReport>,
    /// Worker forge requests waiting for an answer (WP-H).
    forge: Loader<Vec<kls_core::belai::forge::ForgeRequest>>,
    /// The pull request link a "Took over" steer sends.
    steer_url: String,
}

/// Board stage flow, unanswered deliveries and the templates they suggest.
#[derive(Clone, Default)]
struct FlowData {
    stages: Vec<kls_engine::delivery::StageFlow>,
    unanswered: Vec<kls_engine::delivery::Unanswered>,
    suggested: Vec<kls_engine::delivery::Suggestion>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let params = Arc::new(Mutex::new(Params {
            days: 30,
            ..Default::default()
        }));
        let p2 = params.clone();
        let data = Loader::new(
            model,
            &[
                "github.delivery",
                "github.pull.merged",
                "github.issue.closed",
                "delivery.",
                "estimate.",
                "run.finished",
                "session.ended",
                "settings.saved",
            ],
            move |core| {
                let p = p2.lock().unwrap().clone();
                async move {
                    let range = range_of(p.days, kls_store::now_ms());
                    let s = core.store();
                    let scope = match p.scope {
                        ScopeSel::All => Scope::All,
                        ScopeSel::Internal => Scope::Internal,
                        ScopeSel::External => Scope::External,
                    };
                    let detail = match &p.person {
                        Some(k) => delivery::contributor(s, k, &range).await?,
                        None => None,
                    };
                    let outcomes = delivery::outcomes(s, &range, &p.q, p.person.as_deref()).await?;
                    let settings = EstimateSettings::load(&core).await?;
                    let by_hand = outcomes
                        .rows
                        .iter()
                        .map(|o| {
                            let e = estimates::estimate(&outcome_inputs(o), &settings);
                            (o.key.clone(), (e.by_hand_hours, e.by_hand_value))
                        })
                        .collect();
                    Ok(Data {
                        estimate: estimates::for_range(&core, range.from).await?,
                        perf: estimates::performance(
                            &core,
                            range.from,
                            p.by.unwrap_or(GroupBy::Agent),
                        )
                        .await?,
                        by_hand,
                        outcomes,
                        decisions: delivery::decisions(&core, &range).await?,
                        people: delivery::people(s, &range).await?,
                        contributors: delivery::contributors(s, &range, scope, &p.q).await?,
                        detail,
                        range,
                    })
                }
            },
        );
        let path = model
            .core()
            .data_dir()
            .join("delivery.csv")
            .display()
            .to_string();
        let initial = params.lock().unwrap().clone();
        let flow = Loader::new(
            model,
            &["card.", "github.", "schedule.fired"],
            |core| async move {
                let since = kls_store::now_ms() - 30 * 86_400_000;
                let unanswered = kls_engine::delivery::unanswered(&core, since, 10_000).await?;
                Ok(FlowData {
                    stages: kls_engine::delivery::flow(&core, since).await?,
                    suggested: kls_engine::delivery::rank_templates(&unanswered),
                    unanswered,
                })
            },
        );
        let outcome_key: Arc<Mutex<Option<String>>> = Arc::default();
        let k2 = outcome_key.clone();
        let outcome = Loader::new(
            model,
            &["github.delivery", "delivery.", "session.ended", "card."],
            move |core| {
                let k = k2.lock().unwrap().clone();
                async move {
                    match k {
                        Some(k) => delivery::outcome(&core, &k).await,
                        None => Ok(None),
                    }
                }
            },
        );
        let ledger = Loader::new(model, &["lifecycle.", "audit."], |core| async move {
            delivery::ledger_flow(&core, kls_store::now_ms() - 30 * 86_400_000).await
        });
        Self {
            tab: Tab::Outcomes,
            edit: initial,
            params,
            data,
            path,
            flow,
            outcome_key,
            outcome,
            part: None,
            ledger,
            coverage: Loader::new(
                model,
                &["lifecycle.", "audit.", "github."],
                |core| async move { delivery::lifecycle_coverage(&core, 7).await },
            ),
            forge: Loader::new(model, &["forge."], |core| async move {
                core.forge_pending(50).await
            }),
            steer_url: String::new(),
        }
    }

    /// `gh:<login>` (or any person key) opens that contributor.
    pub fn focus(&mut self, id: &str) {
        self.edit.person = Some(id.to_string());
        self.tab = Tab::Contributor;
        self.apply();
    }

    /// Opens one outcome (`pr:acme/api#7`) in the Outcome tab.
    pub fn open_outcome(&mut self, key: &str) {
        *self.outcome_key.lock().unwrap() = Some(key.to_string());
        self.outcome.reload();
        self.tab = Tab::Outcome;
    }

    fn apply(&mut self) {
        let changed = *self.params.lock().unwrap() != self.edit;
        if changed {
            *self.params.lock().unwrap() = self.edit.clone();
            self.data.reload();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let p = theme::palette(ui.ctx());
        let mut export = false;
        kit::page_header(ui, icons::CHART_BAR, "Delivery", |ui| {
            // Right to left: export, then the tabs.
            export = kit::ghost_button(ui, &icons::label(icons::EXPORT, "Export CSV"))
                .on_hover_text(format!("Export {} as CSV", self.tab.word()))
                .clicked();
            ui.add_space(8.0);
            let open_outcome = self.outcome_key.lock().unwrap().clone();
            if let Some(k) = open_outcome {
                if kit::ghost_button(ui, icons::X)
                    .on_hover_text("Close outcome")
                    .clicked()
                {
                    *self.outcome_key.lock().unwrap() = None;
                    if self.tab == Tab::Outcome {
                        self.tab = Tab::Outcomes;
                    }
                }
                if kit::tab(
                    ui,
                    &icons::label(icons::TARGET, &k),
                    self.tab == Tab::Outcome,
                )
                .clicked()
                {
                    self.tab = Tab::Outcome;
                }
            }
            if let Some(k) = self.edit.person.clone() {
                if kit::ghost_button(ui, icons::X)
                    .on_hover_text("Clear person")
                    .clicked()
                {
                    self.edit.person = None;
                    if self.tab == Tab::Contributor {
                        self.tab = Tab::Contributors;
                    }
                }
                if kit::tab(
                    ui,
                    &icons::label(icons::USER, &k),
                    self.tab == Tab::Contributor,
                )
                .clicked()
                {
                    self.tab = Tab::Contributor;
                }
            }
            for t in [
                Tab::Lifecycle,
                Tab::Performance,
                Tab::Estimate,
                Tab::Contributors,
                Tab::People,
                Tab::Decisions,
                Tab::Outcomes,
            ] {
                let label = match t {
                    Tab::Outcomes => icons::label(icons::TARGET, "Outcomes"),
                    Tab::Decisions => icons::label(icons::SEAL_CHECK, "Decisions"),
                    Tab::People => icons::label(icons::USERS_THREE, "People"),
                    Tab::Contributors => icons::label(icons::USER_PLUS, "Contributors"),
                    Tab::Contributor | Tab::Outcome => String::new(),
                    Tab::Estimate => icons::label(icons::CURRENCY_DOLLAR, "Estimate"),
                    Tab::Performance => icons::label(icons::TREND_UP, "Performance"),
                    Tab::Lifecycle => icons::label(icons::FLOW_ARROW, "Lifecycle"),
                };
                if kit::tab(ui, &label, self.tab == t).clicked() {
                    self.tab = t;
                }
            }
        });
        // Filters: range, search, scope, CSV path.
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(icons::CALENDAR).color(p.muted))
                .on_hover_text("Range");
            for (d, w) in RANGES {
                if kit::pick_chip(ui, w, self.edit.days == d, p.muted).clicked() {
                    self.edit.days = d;
                }
            }
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.edit.q)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                    .desired_width(200.0),
            )
            .on_hover_text("Title, repo, key, person");
            if self.tab == Tab::Contributors {
                for (s, w) in [
                    (ScopeSel::All, "All"),
                    (ScopeSel::Internal, "Internal"),
                    (ScopeSel::External, "External"),
                ] {
                    if kit::pick_chip(ui, w, self.edit.scope == s, p.muted).clicked() {
                        self.edit.scope = s;
                    }
                }
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new(icons::FILE_TEXT).color(p.muted))
                .on_hover_text("CSV path");
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(300.0));
        });
        ui.add_space(10.0);
        self.apply();
        let data = match self.data.get() {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        if export {
            let csv = match self.tab {
                Tab::Outcomes | Tab::Contributor | Tab::Lifecycle | Tab::Outcome => {
                    delivery::outcomes_csv(&data.outcomes.rows)
                }
                Tab::Estimate => estimate_csv(&data.estimate),
                Tab::Performance => perf_csv(&data.perf),
                Tab::Decisions => delivery::decisions_csv(&data.decisions),
                Tab::People => delivery::people_csv(&data.people),
                Tab::Contributors => delivery::contributors_csv(&data.contributors),
            };
            let path = self.path.trim().to_string();
            if path.is_empty() {
                model.notify(Notice::Error("Enter a file path to save the CSV to".into()));
            } else {
                match std::fs::write(&path, csv) {
                    Ok(()) => model.notify(Notice::Info(format!("Saved {path}"))),
                    Err(e) => model.notify(Notice::Error(format!("Could not write {path}: {e}"))),
                }
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("delivery_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {} → {}",
                        icons::CALENDAR,
                        fmt::when(data.range.from),
                        fmt::when(data.range.to)
                    ))
                    .monospace()
                    .size(12.0)
                    .color(p.muted),
                )
                .on_hover_text("UTC days");
                ui.add_space(8.0);
                match self.tab {
                    Tab::Outcomes => self.outcomes(ui, &data.outcomes, &data.range, &data.by_hand),
                    Tab::Decisions => decisions(ui, &data.decisions, &p),
                    Tab::People => self.people(ui, &data.people, &p),
                    Tab::Contributors => self.contributors(ui, &data.contributors),
                    Tab::Estimate => {
                        estimate_view(ui, &data.estimate, &p);
                        ui.add_space(14.0);
                        role_table(ui, &p);
                    }
                    Tab::Performance => self.performance(ui, &data.perf, &p),
                    Tab::Lifecycle => {
                        lifecycle(ui, &data.outcomes, &p);
                        if let Some(Ok(f)) = self.flow.get() {
                            flow_panel(ui, model, &f, &p);
                        }
                        ui.add_space(14.0);
                        ledger_panel(ui, model, self.ledger.get(), &p);
                        ui.add_space(14.0);
                        coverage_panel(ui, self.coverage.get(), &p);
                        ui.add_space(14.0);
                        forge_panel(ui, model, self.forge.get(), &mut self.steer_url, &p);
                    }
                    Tab::Outcome => match self.outcome.get() {
                        Some(Ok(Some(d))) => outcome_view(ui, &d, &p),
                        Some(Ok(None)) => {
                            ui.label(
                                egui::RichText::new(icons::label(icons::TARGET, "No such outcome"))
                                    .color(p.muted),
                            );
                        }
                        Some(Err(e)) => {
                            ui.colored_label(p.red, e);
                        }
                        None => {
                            ui.spinner();
                        }
                    },
                    Tab::Contributor => match &data.detail {
                        Some(d) => contributor(ui, d, &data.outcomes, &mut self.part, &p),
                        None => {
                            ui.label(
                                egui::RichText::new(icons::label(icons::USER, "No such person"))
                                    .color(p.muted),
                            )
                            .on_hover_text("Not in range; bots hidden");
                        }
                    },
                }
            });
    }

    fn outcomes(
        &mut self,
        ui: &mut egui::Ui,
        r: &OutcomesReport,
        range: &Range,
        by_hand: &BTreeMap<String, (f64, f64)>,
    ) {
        let p = theme::palette(ui.ctx());
        let buckets = merged_buckets(&r.rows, range);
        let (agents, people) = buckets.iter().fold((0, 0), |(a, b), x| (a + x.1, b + x.2));
        kpis(ui, |ui| {
            kpi(
                ui,
                icons::GIT_MERGE,
                "PRs merged",
                r.merged_prs.to_string(),
                None,
            );
            kpi(
                ui,
                icons::TIMER,
                "Median cycle",
                dur(r.median_cycle_ms),
                None,
            );
            let share = if agents + people > 0 {
                format!("{:.0}%", agents as f64 * 100.0 / (agents + people) as f64)
            } else {
                "–".into()
            };
            kpi(
                ui,
                icons::ROBOT,
                "Agent share",
                share,
                Some("of merged PRs"),
            );
            kpi(
                ui,
                icons::CHECK_CIRCLE,
                "Closed issues",
                r.closed_issues.to_string(),
                None,
            );
            kpi(
                ui,
                icons::EYE,
                "Median 1st review",
                dur(r.median_first_review_ms),
                None,
            );
            kpi(
                ui,
                icons::HAND_PALM,
                "No person",
                r.no_person_needed.to_string(),
                Some("outcomes"),
            );
            let (h, v): (f64, f64) = by_hand
                .values()
                .fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
            kpi(
                ui,
                icons::HAND,
                "By hand",
                format!("{h:.0} h"),
                Some(format!("${v:.0}").as_str()),
            );
            let note = format!("{} other", r.other);
            kpi(
                ui,
                icons::TARGET,
                "Outcomes",
                r.total.to_string(),
                Some(note.as_str()),
            );
        });
        ui.add_space(14.0);
        let per = if buckets_daily(range) { "day" } else { "week" };
        kit::section(
            ui,
            icons::CHART_BAR,
            &format!("Merged pull requests per {per}"),
            false,
            |ui| {
                legend(ui, p.blue, "by people");
                legend(ui, p.gold, "by agents");
            },
            |ui| {
                stacked_bars(ui, "merged_per_week", &buckets, &p);
                if !r.by_repo.is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  {}",
                            icons::GIT_BRANCH,
                            r.by_repo
                                .iter()
                                .map(|(r, n)| format!("{r} ({n})"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .size(12.0)
                        .color(p.muted),
                    )
                    .on_hover_text("Top repositories");
                }
            },
        );
        ui.add_space(14.0);
        if r.rows.is_empty() {
            ui.label(
                egui::RichText::new(icons::label(icons::TARGET, "No outcomes")).color(p.muted),
            )
            .on_hover_text("Merged PRs and closed issues of mirrored repos");
            return;
        }
        let mut open: Option<String> = None;
        table_card(ui, |ui| {
            let mut lines = Vec::new();
            TableBuilder::new(ui)
                .id_salt("outcomes_table")
                .striped(false)
                .resizable(true)
                .vscroll(false)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().at_least(150.0))
                .column(Column::remainder().at_least(200.0))
                .column(Column::auto().at_least(90.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::auto().at_least(60.0))
                .column(Column::auto().at_least(120.0))
                .column(Column::auto().at_least(80.0))
                .header(34.0, |mut h| {
                    for t in [
                        "Key",
                        "Title",
                        "Resolved",
                        "Result",
                        "Cycle",
                        "1st review",
                        "People",
                        "Reviewers",
                        "By hand",
                    ] {
                        lines.push(h.col(|ui| kit::th(ui, t)).0);
                    }
                })
                .body(|mut body| {
                    for o in &r.rows {
                        body.row(30.0, |mut row| {
                            row.col(|ui| {
                                ui.label(egui::RichText::new(&o.key).monospace().size(12.0));
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&o.title).strong())
                                        .truncate(),
                                );
                            });
                            row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(
                                        o.resolved_at.map(fmt::when).unwrap_or_default(),
                                    )
                                    .color(p.muted),
                                );
                            });
                            row.col(|ui| {
                                let (icon, c) = match o.result.as_str() {
                                    "merged" => (icons::GIT_MERGE, p.violet),
                                    "closed" => (icons::CHECK, p.green),
                                    _ => (icons::CIRCLE, p.muted),
                                };
                                kit::chip(ui, &icons::label(icon, &o.result), c);
                            });
                            row.col(|ui| {
                                ui.label(egui::RichText::new(dur(o.cycle_ms)).monospace());
                            });
                            row.col(|ui| {
                                ui.label(egui::RichText::new(dur(o.first_review_ms)).monospace());
                            });
                            row.col(|ui| {
                                ui.label(egui::RichText::new(o.people.to_string()).monospace())
                                    .on_hover_text(format!("{} actions", o.people_actions));
                            });
                            row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(o.reviewers.join(", ")).color(p.muted),
                                );
                            });
                            row.col(|ui| {
                                if let Some((h, v)) = by_hand.get(&o.key) {
                                    ui.label(egui::RichText::new(format!("{h:.0} h")).monospace())
                                        .on_hover_text(format!(
                                            "${v:.0} by hand (Settings > Estimates)"
                                        ));
                                }
                            });
                            let resp = row.response();
                            if resp.clicked() {
                                open = Some(o.key.clone());
                            }
                            lines.push(resp.rect);
                        });
                    }
                });
            lines
        });
        if let Some(k) = open {
            self.open_outcome(&k);
        }
    }

    fn people(&mut self, ui: &mut egui::Ui, r: &PeopleReport, p: &theme::Palette) {
        kpis(ui, |ui| {
            kpi(ui, icons::USERS_THREE, "People", r.people.to_string(), None);
            kpi(
                ui,
                icons::USER,
                "Internal",
                r.known_members.to_string(),
                None,
            );
            kpi(ui, icons::LIGHTNING, "Actions", r.actions.to_string(), None);
            kpi(
                ui,
                icons::USER,
                "Per person",
                r.actions_per_person
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or("–".into()),
                Some("actions"),
            );
            kpi(ui, icons::CLOCK, "Person time", dur(Some(r.time_ms)), None);
            kpi(
                ui,
                icons::HOURGLASS,
                "Median wait",
                dur(r.waiting_on_person_ms),
                None,
            );
        });
        ui.add_space(14.0);
        let top: Vec<(String, f64)> = r
            .matrix
            .iter()
            .take(15)
            .map(|m| (m.name.clone(), m.time_ms as f64 / 3_600_000.0))
            .collect();
        if !top.is_empty() {
            kit::section(
                ui,
                icons::CLOCK,
                "Hours per person",
                false,
                |_| {},
                |ui| named_bars(ui, "people_time", "Hours", &top, p.violet),
            );
            ui.add_space(14.0);
        }
        kit::section(
            ui,
            icons::PLUGS_CONNECTED,
            "By source",
            false,
            |_| {},
            |ui| {
                let mut lines = Vec::new();
                TableBuilder::new(ui)
                    .id_salt("people_sources")
                    .striped(false)
                    .vscroll(false)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .columns(Column::auto().at_least(100.0), 4)
                    .header(30.0, |mut h| {
                        for t in ["Source", "Actions", "People", "Time"] {
                            lines.push(h.col(|ui| kit::th(ui, t)).0);
                        }
                    })
                    .body(|mut body| {
                        for s in &r.sources {
                            body.row(28.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(&s.id).strong());
                                });
                                row.col(|ui| {
                                    ui.label(
                                        egui::RichText::new(s.actions.to_string()).monospace(),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(s.people.to_string()).monospace());
                                });
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(dur(Some(s.time_ms))).monospace());
                                });
                                lines.push(row.response().rect);
                            });
                        }
                    });
                kit::row_lines(ui, &lines, p.line);
            },
        );
        ui.add_space(14.0);
        let mut open: Option<String> = None;
        kit::section(
            ui,
            icons::USERS_THREE,
            "People by source",
            false,
            |_| {},
            |ui| {
                let mut lines = Vec::new();
                egui::ScrollArea::horizontal()
                    .id_salt("people_matrix_h")
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .id_salt("people_matrix")
                            .striped(false)
                            .vscroll(false)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::auto().at_least(150.0))
                            .columns(Column::auto().at_least(64.0), delivery::SOURCES.len() + 1)
                            .header(30.0, |mut h| {
                                lines.push(h.col(|ui| kit::th(ui, "Person")).0);
                                for s in delivery::SOURCES {
                                    lines.push(h.col(|ui| kit::th(ui, s)).0);
                                }
                                lines.push(h.col(|ui| kit::th(ui, "Time")).0);
                            })
                            .body(|mut body| {
                                for m in &r.matrix {
                                    body.row(28.0, |mut row| {
                                        row.col(|ui| {
                                            ui.label(egui::RichText::new(&m.name).strong())
                                                .on_hover_text(&m.person_key);
                                        });
                                        for s in delivery::SOURCES {
                                            row.col(|ui| {
                                                ui.label(
                                                    egui::RichText::new(
                                                        m.counts
                                                            .get(s)
                                                            .copied()
                                                            .unwrap_or(0)
                                                            .to_string(),
                                                    )
                                                    .monospace(),
                                                );
                                            });
                                        }
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(dur(Some(m.time_ms)))
                                                    .monospace(),
                                            );
                                        });
                                        let resp = row.response();
                                        lines.push(resp.rect);
                                        if resp.clicked() {
                                            open = Some(m.person_key.clone());
                                        }
                                    });
                                }
                            });
                    });
                kit::row_lines(ui, &lines, p.line);
            },
        );
        if !r.bots.is_empty() {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}  {}",
                    icons::ROBOT,
                    r.bots
                        .iter()
                        .map(|b| format!("{} ({})", b.login, b.actions))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .size(12.0)
                .color(p.muted),
            )
            .on_hover_text("Bots, left out");
        }
        if let Some(k) = open {
            self.focus(&k);
        }
    }

    fn contributors(&mut self, ui: &mut egui::Ui, r: &ContributorsReport) {
        let p = theme::palette(ui.ctx());
        kpis(ui, |ui| {
            kpi(
                ui,
                icons::USERS_THREE,
                "Contributors",
                r.count_all.to_string(),
                None,
            );
            kpi(
                ui,
                icons::USER,
                "Internal",
                r.count_internal.to_string(),
                None,
            );
            kpi(
                ui,
                icons::GLOBE,
                "External",
                r.count_external.to_string(),
                None,
            );
            kpi(
                ui,
                icons::TARGET,
                "With person",
                format!("{}/{}", r.outcomes_with_person, r.outcomes_total),
                Some("outcomes"),
            );
            kpi(
                ui,
                icons::GIT_COMMIT,
                "Signed off",
                format!("{}/{}", r.commits_signed_off, r.commits_total),
                Some("commits"),
            );
            kpi(
                ui,
                icons::SEAL_CHECK,
                "Answered",
                r.decisions_answered.to_string(),
                Some("decisions"),
            );
            kpi(
                ui,
                icons::TIMER,
                "Median answer",
                dur(r.median_answer_ms),
                None,
            );
        });
        ui.add_space(14.0);
        let top: Vec<(String, f64)> = r
            .items
            .iter()
            .take(15)
            .map(|c| (c.name.clone(), c.outcomes as f64))
            .collect();
        kit::section(
            ui,
            icons::HAND_PALM,
            "Outcomes per contributor",
            false,
            |_| {},
            |ui| {
                named_bars(ui, "contrib_outcomes", "Outcomes", &top, p.cyan);
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::HAND_PALM, "Steps in"),
                        p.muted,
                    ))
                    .on_hover_text("Where people step in");
                    for s in &r.steps_in {
                        kit::chip(
                            ui,
                            &format!(
                                "{} {} ({}%)",
                                s.id,
                                s.count,
                                s.pct_of_outcomes
                                    .map(|v| format!("{v:.0}"))
                                    .unwrap_or("–".into())
                            ),
                            p.muted,
                        );
                    }
                });
            },
        );
        ui.add_space(14.0);
        let mut open: Option<String> = None;
        table_card(ui, |ui| {
            let mut lines = Vec::new();
            TableBuilder::new(ui)
                .id_salt("contributors_table")
                .striped(false)
                .vscroll(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().at_least(150.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::auto().at_least(160.0))
                .columns(Column::auto().at_least(64.0), 7)
                .header(34.0, |mut h| {
                    for t in [
                        "Contributor",
                        "Scope",
                        "Repos",
                        "Outcomes",
                        "Started",
                        "Decided",
                        "Reviews",
                        "Signed off",
                        "Signed %",
                        "Active",
                    ] {
                        lines.push(h.col(|ui| kit::th(ui, t)).0);
                    }
                })
                .body(|mut body| {
                    for c in &r.items {
                        body.row(30.0, |mut row| {
                            row.col(|ui| {
                                ui.label(egui::RichText::new(&c.name).strong())
                                    .on_hover_text(&c.person_key);
                            });
                            row.col(|ui| {
                                let (w, col) = match c.scope.as_str() {
                                    "internal" => ("internal", p.cyan),
                                    "external" => ("external", p.violet),
                                    _ => ("unknown", p.muted),
                                };
                                ui.label(egui::RichText::new(w).color(col));
                            });
                            row.col(|ui| {
                                let mut s = c.repos_top.join(", ");
                                if c.repos_more > 0 {
                                    s.push_str(&format!(" +{}", c.repos_more));
                                }
                                ui.label(egui::RichText::new(s).color(p.muted));
                            });
                            for v in [c.outcomes, c.started, c.decided, c.reviewed, c.signed_off] {
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(v.to_string()).monospace());
                                });
                            }
                            row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(
                                        c.signed_rate
                                            .map(|v| format!("{v:.0}%"))
                                            .unwrap_or("–".into()),
                                    )
                                    .monospace(),
                                );
                            });
                            row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(fmt::when(c.last_active_at)).color(p.muted),
                                );
                            });
                            let resp = row.response();
                            lines.push(resp.rect);
                            if resp.clicked() {
                                open = Some(c.person_key.clone());
                            }
                        });
                    }
                });
            lines
        });
        if let Some(k) = open {
            self.focus(&k);
        }
    }
}

fn decisions(ui: &mut egui::Ui, r: &DecisionsReport, p: &theme::Palette) {
    kpis(ui, |ui| {
        kpi(
            ui,
            icons::SEAL_CHECK,
            "Decisions",
            r.total.to_string(),
            None,
        );
        kpi(
            ui,
            icons::CALENDAR,
            "Per day",
            format!("{:.1}", r.per_day),
            None,
        );
        kpi(ui, icons::CHECK, "Proceeded", r.proceeded.to_string(), None);
        kpi(
            ui,
            icons::LIST_CHECKS,
            "By rule",
            r.rule_handled.to_string(),
            None,
        );
        kpi(
            ui,
            icons::WARNING,
            "Escalated",
            r.escalated.to_string(),
            None,
        );
        kpi(
            ui,
            icons::ROBOT,
            "No person",
            r.never_needed_person_pct
                .map(|v| format!("{v:.1}%"))
                .unwrap_or("–".into()),
            None,
        );
    });
    ui.add_space(14.0);
    if r.total == 0 && r.asked.total == 0 {
        ui.label(
            egui::RichText::new(icons::label(icons::SEAL_CHECK, "No decisions")).color(p.muted),
        );
        return;
    }
    let mk = |name: &str, f: fn(&delivery::DecisionDayRow) -> i64, c: egui::Color32| {
        BarChart::new(
            name,
            r.days
                .iter()
                .enumerate()
                .map(|(i, d)| Bar::new(i as f64, f(d) as f64).width(0.7))
                .collect(),
        )
        .color(c)
    };
    kit::section(
        ui,
        icons::CHART_BAR,
        "Decisions per day",
        false,
        |ui| {
            legend(ui, p.amber, "Escalated");
            legend(ui, p.cyan, "By rule");
            legend(ui, p.green, "Proceeded");
        },
        |ui| {
            let a = mk("Proceeded", |d| d.proceeded, p.green);
            let b = mk("By rule", |d| d.rule_handled, p.cyan).stack_on(&[&a]);
            let c = mk("Escalated", |d| d.escalated, p.amber).stack_on(&[&a, &b]);
            quiet_plot("decisions_days", 170.0).show(ui, |pu| {
                pu.bar_chart(a);
                pu.bar_chart(b);
                pu.bar_chart(c);
            });
        },
    );
    ui.add_space(14.0);
    kit::section(
        ui,
        icons::CHART_BAR,
        "Confidence",
        false,
        |ui| {
            ui.label(
                egui::RichText::new(format!("threshold {:.2}", r.threshold))
                    .monospace()
                    .size(12.0)
                    .color(p.muted),
            );
        },
        |ui| {
            let hist = BarChart::new(
                "Decisions",
                r.histogram
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        let x = i as f64 / 10.0 + 0.05;
                        let bar = Bar::new(x, *n as f64).width(0.08);
                        if x < r.threshold {
                            bar.fill(p.amber)
                        } else {
                            bar.fill(p.violet)
                        }
                    })
                    .collect(),
            );
            quiet_plot("decisions_conf", 140.0).show(ui, |pu| pu.bar_chart(hist));
        },
    );
    ui.add_space(14.0);
    kit::Split::new(320.0, 320.0, kit::Widths::Halves).show(ui, |ui, side| {
        if side == 0 {
            kit::section(
                ui,
                icons::CPU,
                "By model",
                false,
                |_| {},
                |ui| {
                    for m in &r.models {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(if m.model.is_empty() {
                                    "(none)"
                                } else {
                                    &m.model
                                })
                                .strong(),
                            );
                            ui.label(egui::RichText::new(m.total.to_string()).monospace());
                            kit::chip(
                                ui,
                                &icons::label(icons::WARNING, &format!("{:.1}%", m.escalated_pct)),
                                p.amber,
                            )
                            .on_hover_text("Escalated");
                            if let Some(l) = m.mean_latency_ms {
                                ui.label(
                                    egui::RichText::new(icons::label(
                                        icons::TIMER,
                                        &format!("{l} ms"),
                                    ))
                                    .color(p.muted),
                                )
                                .on_hover_text("Mean latency");
                            }
                        });
                    }
                },
            );
        } else {
            kit::section(
                ui,
                icons::HAND_PALM,
                "Asked a person",
                false,
                |_| {},
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        kit::chip(
                            ui,
                            &icons::label(icons::SEAL_CHECK, &r.asked.total.to_string()),
                            p.muted,
                        )
                        .on_hover_text("Checkpoints");
                        kit::chip(
                            ui,
                            &icons::label(icons::CHECK, &r.asked.approved.to_string()),
                            p.green,
                        )
                        .on_hover_text("Approved");
                        kit::chip(
                            ui,
                            &icons::label(icons::ARROW_U_UP_LEFT, &r.asked.sent_back.to_string()),
                            p.amber,
                        )
                        .on_hover_text("Sent back");
                        kit::chip(
                            ui,
                            &icons::label(icons::CHAT_TEXT, &r.asked.answered.to_string()),
                            p.cyan,
                        )
                        .on_hover_text("Answered");
                        kit::chip(
                            ui,
                            &icons::label(icons::CLOCK, &dur(r.asked.median_wait_ms)),
                            p.muted,
                        )
                        .on_hover_text("Median wait");
                    });
                    for k in &r.asked.by_kind {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}: {}  {} {}",
                                k.kind,
                                k.count,
                                icons::CLOCK,
                                dur(k.median_wait_ms)
                            ))
                            .color(p.muted),
                        );
                    }
                },
            );
        }
    });
}

fn contributor(
    ui: &mut egui::Ui,
    d: &ContributorDetail,
    o: &OutcomesReport,
    part: &mut Option<String>,
    p: &theme::Palette,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(icons::USER)
                .size(20.0)
                .color(p.gold_text),
        );
        ui.label(egui::RichText::new(&d.name).size(20.0).strong());
        ui.label(
            egui::RichText::new(icons::label(icons::FINGERPRINT, &d.person.person_key))
                .monospace()
                .size(12.0)
                .color(p.muted),
        );
        if !d.scope.is_empty() {
            kit::chip(ui, &icons::label(icons::TAG, &d.scope), p.cyan);
        }
        for i in &d.identities {
            ui.label(egui::RichText::new(format!("{}: {}", i.kind, i.value)).color(p.muted))
                .on_hover_text(&i.note);
        }
    });
    ui.add_space(10.0);
    kpis(ui, |ui| {
        kpi(
            ui,
            icons::TARGET,
            "Outcomes",
            format!("{}/{}", d.outcomes_touched, d.outcomes_total),
            None,
        );
        kpi(ui, icons::PLAY, "Started", d.started.to_string(), None);
        kpi(
            ui,
            icons::SEAL_CHECK,
            "Decisions",
            d.decisions_answered.to_string(),
            None,
        );
        kpi(
            ui,
            icons::TIMER,
            "Median answer",
            dur(d.median_answer_ms),
            None,
        );
        kpi(
            ui,
            icons::GIT_COMMIT,
            "Commits",
            d.commits_signed_off.to_string(),
            Some("signed off"),
        );
        kpi(
            ui,
            icons::SHIELD_CHECK,
            "Signed",
            match d.signed_rate {
                Some(r) => format!("{}/{} ({r:.0}%)", d.signed_verified, d.signed_total),
                None => "–".into(),
            },
            None,
        );
        kpi(ui, icons::CLOCK, "Time", dur(Some(d.time_ms)), None);
        kpi(ui, icons::LIGHTNING, "Actions", d.actions.to_string(), None);
    });
    ui.add_space(14.0);
    kit::section(
        ui,
        icons::CHART_BAR,
        "Actions per day",
        false,
        |_| {},
        |ui| day_bars(ui, "contributor_days", "Actions", &d.days, p.gold),
    );
    ui.add_space(14.0);
    kit::Split::new(320.0, 320.0, kit::Widths::Halves).show(ui, |ui, side| {
        if side == 0 {
            let shown = part.clone();
            let mut pick: Option<Option<String>> = None;
            kit::section(
                ui,
                icons::LIST_CHECKS,
                &format!("Latest work · {}", d.work_total),
                false,
                |ui| {
                    for w in ["signed", "reviewed", "decided", "started"] {
                        let on = shown.as_deref() == Some(w);
                        if kit::pick_chip(ui, w, on, p.muted)
                            .on_hover_text("Filter by part")
                            .clicked()
                        {
                            pick = Some(if on { None } else { Some(w.to_string()) });
                        }
                    }
                },
                |ui| {
                    for w in d
                        .work
                        .iter()
                        .filter(|w| shown.as_ref().is_none_or(|x| w.parts.contains(x)))
                    {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(&w.outcome_key)
                                    .monospace()
                                    .size(12.0)
                                    .color(p.muted),
                            );
                            ui.label(egui::RichText::new(&w.title).strong());
                            ui.label(
                                egui::RichText::new(format!(
                                    "[{}]{}",
                                    w.parts.join(", "),
                                    if w.others.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" with {}", w.others.join(", "))
                                    }
                                ))
                                .color(p.muted),
                            );
                        });
                    }
                },
            );
            if let Some(x) = pick {
                *part = x;
            }
        } else {
            kit::section(
                ui,
                icons::PLUGS_CONNECTED,
                "By source",
                false,
                |_| {},
                |ui| {
                    for (s, n, t) in d.parts.iter().filter(|x| x.1 > 0) {
                        ui.label(format!("{s}: {n} ({})", dur(Some(*t))));
                    }
                    ui.add_space(8.0);
                    ui.label(kit::caps(
                        &icons::label(icons::GIT_BRANCH, "Repositories"),
                        p.muted,
                    ));
                    for (r, outs, commits, _) in &d.repos {
                        ui.label(format!(
                            "{r}  {} {outs}  {} {commits}",
                            icons::TARGET,
                            icons::GIT_COMMIT
                        ));
                    }
                },
            );
        }
    });
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(icons::label(
            icons::EXPORT,
            &format!("{} outcomes", o.rows.len()),
        ))
        .size(12.0)
        .color(p.muted),
    )
    .on_hover_text("Rows the CSV export writes");
}

fn dur(ms: Option<i64>) -> String {
    match ms {
        None => "–".into(),
        Some(ms) => {
            let s = ms / 1000;
            match s {
                0..=59 => format!("{s}s"),
                60..=3599 => format!("{}m", s / 60),
                3600..=86_399 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
                _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
            }
        }
    }
}

const DAY_MS: i64 = 86_400_000;

/// Short ranges are bucketed by day, longer ones by week.
fn buckets_daily(range: &Range) -> bool {
    range.days() <= 14
}

/// Monday 00:00 UTC of the week holding `ms`.
fn week_start(ms: i64) -> i64 {
    let day = ms.div_euclid(DAY_MS);
    // 1970-01-01 was a Thursday: Monday-based weekday is (day + 3) mod 7.
    (day - (day + 3).rem_euclid(7)) * DAY_MS
}

/// Merged pull requests per day or week of `range`, zero filled:
/// `(label, by agents, by people)`. A pull request counts as an agent's
/// when agent work was recorded on it.
fn merged_buckets(rows: &[delivery::OutcomeRow], range: &Range) -> Vec<(String, i64, i64)> {
    let daily = buckets_daily(range);
    let key = |ms: i64| {
        if daily {
            ms.div_euclid(DAY_MS) * DAY_MS
        } else {
            week_start(ms)
        }
    };
    let step = if daily { DAY_MS } else { 7 * DAY_MS };
    let mut out: Vec<(i64, i64, i64)> = Vec::new();
    let mut k = key(range.from);
    let last = key((range.to - 1).max(range.from));
    while k <= last {
        out.push((k, 0, 0));
        k += step;
    }
    for r in rows {
        if r.kind != "pr" || r.result != "merged" {
            continue;
        }
        let Some(at) = r.resolved_at else { continue };
        let k = key(at);
        if let Some(b) = out.iter_mut().find(|b| b.0 == k) {
            if r.agent_ms.is_some_and(|m| m > 0) {
                b.1 += 1;
            } else {
                b.2 += 1;
            }
        }
    }
    out.into_iter()
        .map(|(k, a, p)| (bucket_label(k, daily), a, p))
        .collect()
}

fn bucket_label(ms: i64, daily: bool) -> String {
    match jiff::Timestamp::from_millisecond(ms) {
        Ok(ts) => {
            let d = ts.to_zoned(jiff::tz::TimeZone::UTC).date();
            if daily {
                format!("{:02}-{:02}", d.month(), d.day())
            } else {
                format!("W{:02}", d.iso_week_date().week())
            }
        }
        Err(_) => "?".into(),
    }
}

/// Stacked bars: people (blue) at the base, agents (gold) on top.
fn stacked_bars(ui: &mut egui::Ui, id: &str, buckets: &[(String, i64, i64)], p: &theme::Palette) {
    if buckets.iter().all(|(_, a, b)| *a == 0 && *b == 0) {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(crate::icons::label(
                crate::icons::CHART_BAR,
                "No merged pull requests",
            ))
            .color(p.muted),
        );
        ui.add_space(8.0);
        return;
    }
    let people = BarChart::new(
        "by people",
        buckets
            .iter()
            .enumerate()
            .map(|(i, (l, _, n))| Bar::new(i as f64, *n as f64).width(0.6).name(l))
            .collect(),
    )
    .color(p.blue);
    let agents = BarChart::new(
        "by agents",
        buckets
            .iter()
            .enumerate()
            .map(|(i, (l, n, _))| Bar::new(i as f64, *n as f64).width(0.6).name(l))
            .collect(),
    )
    .color(p.gold)
    .stack_on(&[&people]);
    let labels: Vec<String> = buckets.iter().map(|b| b.0.clone()).collect();
    quiet_plot(id, 200.0)
        .x_grid_spacer(egui_plot::uniform_grid_spacer(|_| [1.0, 5.0, 10.0]))
        .x_axis_formatter(move |m, _| {
            let i = m.value.round();
            if (m.value - i).abs() < 1e-6 && i >= 0.0 {
                labels.get(i as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        })
        .show(ui, |pu| {
            pu.bar_chart(people);
            pu.bar_chart(agents);
        });
}

/// A chart frame without background, drag or zoom; the page scrolls.
fn quiet_plot(id: &str, height: f32) -> Plot<'static> {
    Plot::new(id)
        .height(height)
        .show_background(false)
        .show_grid([false, true])
        .allow_scroll(false)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        // Counts never go below zero; with no or tiny data egui_plot would
        // otherwise centre the axis on zero.
        .include_y(0.0)
        .include_y(1.0)
}

fn day_bars(ui: &mut egui::Ui, id: &str, name: &str, days: &[(i64, i64)], color: egui::Color32) {
    let chart = BarChart::new(
        name,
        days.iter()
            .enumerate()
            .map(|(i, (day, n))| {
                Bar::new(i as f64, *n as f64)
                    .width(0.7)
                    .name(fmt::when(*day))
            })
            .collect(),
    )
    .color(color);
    quiet_plot(id, 140.0).show(ui, |pu| pu.bar_chart(chart));
}

fn named_bars(
    ui: &mut egui::Ui,
    id: &str,
    name: &str,
    rows: &[(String, f64)],
    color: egui::Color32,
) {
    if rows.is_empty() {
        return;
    }
    let chart = BarChart::new(
        name,
        rows.iter()
            .enumerate()
            .map(|(i, (n, v))| Bar::new(i as f64, *v).width(0.6).name(n))
            .collect(),
    )
    .color(color);
    quiet_plot(id, 150.0).show(ui, |pu| pu.bar_chart(chart));
}

/// A wrapping row of KPI tiles.
fn kpis(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
        add(ui);
    });
}

fn kpi(ui: &mut egui::Ui, icon: &str, label: &str, value: String, note: Option<&str>) {
    kit::kpi(ui, icon, label, &value, note, None);
}

/// A square legend swatch and its label.
fn legend(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.label(egui::RichText::new(text).size(12.0).color(p.muted));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

/// A table on a plain panel, scrolling sideways when narrow; `add` returns
/// the row rects to draw the row lines under.
fn table_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> Vec<egui::Rect>) {
    let p = theme::palette(ui.ctx());
    kit::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let lines = egui::ScrollArea::horizontal()
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(8, 0))
                    .show(ui, add)
                    .inner
            })
            .inner;
        kit::row_lines(ui, &lines, p.line);
    });
}

// ---- estimate, performance, lifecycle ------------------------------------------

/// What an outcome is priced from: its agent time as senior engineering
/// work and its decisions as merge-readiness checks.
fn outcome_inputs(o: &delivery::OutcomeRow) -> EstimateInputs {
    let mut i = EstimateInputs::default();
    if let Some(ms) = o.agent_ms.filter(|m| *m > 0) {
        i.agent_hours.insert("sse".into(), ms as f64 / 3_600_000.0);
    }
    if let Some(n) = o.decisions.filter(|n| *n > 0) {
        i.decisions.insert("merge".into(), n);
    }
    i.model_cost_usd = o.cost_micro_usd.map(|c| c as f64 / 1_000_000.0);
    i.sources = vec![format!("outcome:{}", o.key)];
    i
}

fn money(v: Option<f64>) -> String {
    v.map(|x| format!("${x:.0}")).unwrap_or_else(|| "–".into())
}

fn times(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}×")).unwrap_or_else(|| "–".into())
}

fn estimate_csv(e: &Estimate) -> String {
    let mut out = String::from("role,agent_hours,counted,mult,rate,engineer_hours,value\n");
    for r in &e.roles {
        out.push_str(&format!(
            "{},{:.2},{},{},{},{:.2},{:.2}\n",
            r.role, r.agent_hours, r.counted, r.mult, r.rate, r.engineer_hours, r.value
        ));
    }
    out.push_str(&format!(
        "total,{:.2},,,,{:.2},{:.2}\nby_hand,,,,,{:.0},{:.2}\n",
        e.agent_hours, e.engineer_hours, e.agent_value, e.by_hand_hours, e.by_hand_value
    ));
    out
}

fn perf_csv(rows: &[PerfRow]) -> String {
    let mut out = String::from(
        "key,items,succeeded,failed,open,success_rate,tokens,cost_usd,cost_per_item,agent_hours,median_cycle_ms,per_day,value_usd\n",
    );
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.4},{},{:.2},{},{:.2},{:.0}\n",
            r.key.replace(',', " "),
            r.items,
            r.succeeded,
            r.failed,
            r.open,
            r.success_rate
                .map(|x| format!("{x:.3}"))
                .unwrap_or_default(),
            r.tokens,
            r.cost_usd,
            r.cost_per_item
                .map(|x| format!("{x:.4}"))
                .unwrap_or_default(),
            r.agent_hours,
            r.median_cycle_ms.map(|x| x.to_string()).unwrap_or_default(),
            r.per_day,
            r.value_usd
        ));
    }
    out
}

/// The range priced the way the web factory prices it, with its inputs.
fn estimate_view(ui: &mut egui::Ui, e: &Estimate, p: &theme::Palette) {
    kpis(ui, |ui| {
        kpi(
            ui,
            icons::HAND,
            "By hand",
            format!("{:.0} h", e.by_hand_hours),
            Some(&money(Some(e.by_hand_value))),
        );
        kpi(
            ui,
            icons::ROBOT,
            "Agent hours",
            format!("{:.1}", e.agent_hours),
            None,
        );
        kpi(
            ui,
            icons::USER,
            "Engineer hours",
            format!("{:.0}", e.engineer_hours),
            None,
        );
        kpi(
            ui,
            icons::SEAL_CHECK,
            "Decisions",
            format!("{:.1} h", e.decision_hours),
            Some(&money(Some(e.decision_value))),
        );
        kpi(ui, icons::COINS, "Model cost", money(e.model_cost), None);
        kpi(
            ui,
            icons::WALLET,
            "Factory cost",
            money(e.factory_cost),
            None,
        );
        kpi(
            ui,
            icons::TREND_UP,
            "Return",
            times(e.return_on_total),
            Some(&format!("{} on model", times(e.return_on_model))),
        );
        if let Some(h) = e.hours_given_back {
            kpi(ui, icons::CLOCK, "Given back", format!("{h:.0} h"), None);
        }
    });
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(format!(
            "{}  conservative {} · {} sources · settings rev {}",
            icons::INFO,
            money(Some(e.conservative.value)),
            e.inputs.sources.len(),
            e.settings_revision
        ))
        .size(12.0)
        .color(p.muted),
    )
    .on_hover_text(e.inputs.sources.join("\n"));
    ui.add_space(12.0);
    if e.roles.is_empty() {
        kit::empty(ui, icons::CURRENCY_DOLLAR, "No agent work in range");
        return;
    }
    table_card(ui, |ui| {
        let mut lines = Vec::new();
        TableBuilder::new(ui)
            .id_salt("estimate_roles")
            .striped(false)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(180.0))
            .columns(Column::auto().at_least(90.0), 5)
            .header(34.0, |mut h| {
                for t in ["Role", "Agent h", "×", "Rate", "Engineer h", "Value"] {
                    lines.push(h.col(|ui| kit::th(ui, t)).0);
                }
            })
            .body(|mut body| {
                for r in &e.roles {
                    body.row(28.0, |mut row| {
                        row.col(|ui| {
                            let c = if r.counted { p.text } else { p.muted };
                            ui.label(egui::RichText::new(&r.role).color(c))
                                .on_hover_text(if r.counted {
                                    "Counted"
                                } else {
                                    "Off in Settings > Estimates"
                                });
                        });
                        for v in [
                            format!("{:.1}", r.agent_hours),
                            format!("{:.1}", r.mult),
                            format!("${:.0}", r.rate),
                            format!("{:.1}", r.engineer_hours),
                            format!("${:.0}", r.value),
                        ] {
                            row.col(|ui| {
                                ui.label(egui::RichText::new(v).monospace());
                            });
                        }
                        lines.push(row.response().rect);
                    });
                }
            });
        lines
    });
}

impl Screen {
    /// Throughput, success, cost and value per agent, node, model, repo or workflow.
    fn performance(&mut self, ui: &mut egui::Ui, rows: &[PerfRow], p: &theme::Palette) {
        let by = self.edit.by.unwrap_or(GroupBy::Agent);
        ui.horizontal_wrapped(|ui| {
            for (g, icon, w) in [
                (GroupBy::Agent, icons::ROBOT, "Agent"),
                (GroupBy::Node, icons::HARD_DRIVES, "Node"),
                (GroupBy::Model, icons::CPU, "Model"),
                (GroupBy::Repo, icons::GIT_BRANCH, "Repo"),
                (GroupBy::Workflow, icons::FLOW_ARROW, "Workflow"),
            ] {
                if kit::pick_chip(ui, &icons::label(icon, w), by == g, p.cyan).clicked() {
                    self.edit.by = Some(g);
                }
            }
        });
        ui.add_space(10.0);
        if rows.is_empty() {
            kit::empty(ui, icons::TREND_UP, "No work in range");
            return;
        }
        table_card(ui, |ui| {
            let mut lines = Vec::new();
            TableBuilder::new(ui)
                .id_salt("perf_table")
                .striped(false)
                .vscroll(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(180.0))
                .columns(Column::auto().at_least(80.0), 7)
                .header(34.0, |mut h| {
                    for t in [
                        "Key",
                        "Items",
                        "Success",
                        "Open",
                        "Cost/item",
                        "Cycle",
                        "Per day",
                        "Value",
                    ] {
                        lines.push(h.col(|ui| kit::th(ui, t)).0);
                    }
                })
                .body(|mut body| {
                    for r in rows {
                        body.row(28.0, |mut row| {
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&r.key).strong())
                                        .truncate(),
                                );
                            });
                            let success = r
                                .success_rate
                                .map(|x| format!("{:.0}%", x * 100.0))
                                .unwrap_or_else(|| "–".into());
                            for v in [
                                r.items.to_string(),
                                success,
                                r.open.to_string(),
                                r.cost_per_item
                                    .map(|x| format!("${x:.2}"))
                                    .unwrap_or_else(|| "–".into()),
                                dur(r.median_cycle_ms),
                                format!("{:.1}", r.per_day),
                                format!("${:.0}", r.value_usd),
                            ] {
                                row.col(|ui| {
                                    ui.label(egui::RichText::new(v).monospace());
                                });
                            }
                            lines.push(row.response().rect);
                        });
                    }
                });
            lines
        });
    }
}

/// Where the range's outcomes spent their time: opened → agent start →
/// first review → resolved, with how many reached each stage.
/// Board flow per stage (30 days) and GitHub events that started no run,
/// with the templates that would have answered them.
fn flow_panel(ui: &mut egui::Ui, model: &Model, f: &FlowData, p: &theme::Palette) {
    let now = kls_store::now_ms();
    ui.add_space(14.0);
    ui.label(kit::caps(
        &icons::label(icons::KANBAN, "Board flow · 30 days"),
        p.muted,
    ));
    egui::Grid::new("board_flow")
        .num_columns(5)
        .striped(false)
        .show(ui, |ui| {
            for h in ["Stage", "Open", "In", "Out", "Oldest open"] {
                ui.label(egui::RichText::new(h).color(p.muted));
            }
            ui.end_row();
            for s in &f.stages {
                ui.label(egui::RichText::new(&s.stage).strong());
                let growing = s.arrivals > s.completions;
                ui.label(egui::RichText::new(s.open.to_string()).color(if growing {
                    p.amber
                } else {
                    p.text
                }))
                .on_hover_text(if growing {
                    "Growing: more in than out"
                } else {
                    ""
                });
                ui.label(s.arrivals.to_string());
                ui.label(s.completions.to_string());
                ui.label(
                    s.oldest_open_at
                        .map(|t| fmt::ago(now, t))
                        .unwrap_or_else(|| "—".into()),
                );
                ui.end_row();
            }
        });
    if f.unanswered.is_empty() {
        return;
    }
    ui.add_space(14.0);
    ui.label(kit::caps(
        &icons::label(icons::BELL, "Unanswered deliveries · 30 days"),
        p.muted,
    ));
    egui::Grid::new("unanswered").num_columns(3).show(ui, |ui| {
        for u in f.unanswered.iter().take(20) {
            ui.label(kit::mono(&u.name, 12.0, p.text));
            ui.label(u.count.to_string());
            ui.label(egui::RichText::new(u.repos.join(", ")).color(p.muted));
            ui.end_row();
        }
    });
    if !f.suggested.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(icons::label(icons::LIGHTNING, "Templates")).color(p.gold_text),
            );
            for s in f.suggested.iter().take(5) {
                if kit::link(ui, &format!("{} · {}", s.template_id, s.score), p.cyan)
                    .on_hover_text(s.reasons.join(", "))
                    .clicked()
                {
                    model.go(crate::Screen::Templates, Some(s.template_id.clone()));
                }
            }
        });
    }
}

fn lifecycle(ui: &mut egui::Ui, r: &OutcomesReport, p: &theme::Palette) {
    let rows = &r.rows;
    let n = rows.len().max(1) as f32;
    let med = |v: Vec<i64>| -> Option<i64> {
        let mut v = v;
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2])
    };
    let started: Vec<i64> = rows
        .iter()
        .filter_map(|o| Some(o.started_at? - o.opened_at?))
        .filter(|d| *d >= 0)
        .collect();
    let reviewed: Vec<i64> = rows.iter().filter_map(|o| o.first_review_ms).collect();
    let resolved: Vec<i64> = rows.iter().filter_map(|o| o.cycle_ms).collect();
    let agent: Vec<i64> = rows.iter().filter_map(|o| o.agent_ms).collect();
    let waits: Vec<i64> = rows.iter().filter_map(|o| o.wait_ms).collect();
    let unanswered = rows
        .iter()
        .filter(|o| o.started_at.is_none() && o.agent_ms.is_none_or(|m| m == 0))
        .count();
    kpis(ui, |ui| {
        kpi(ui, icons::TARGET, "Outcomes", rows.len().to_string(), None);
        kpi(ui, icons::ROBOT, "Median agent", dur(med(agent)), None);
        kpi(
            ui,
            icons::HOURGLASS,
            "Median wait",
            dur(med(waits)),
            Some("for people"),
        );
        kpi(
            ui,
            icons::QUESTION,
            "No agent work",
            unanswered.to_string(),
            Some("outcomes"),
        );
    });
    ui.add_space(14.0);
    let stages = [
        (
            icons::PLUS_CIRCLE,
            "Opened",
            rows.iter().filter(|o| o.opened_at.is_some()).count(),
            None,
        ),
        (
            icons::PLAY_CIRCLE,
            "Agent started",
            started.len(),
            med(started),
        ),
        (icons::EYE, "First review", reviewed.len(), med(reviewed)),
        (
            icons::CHECK_CIRCLE,
            "Resolved",
            resolved.len(),
            med(resolved),
        ),
    ];
    kit::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                for (icon, name, count, after) in stages {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [160.0, 20.0],
                            egui::Label::new(
                                egui::RichText::new(icons::label(icon, name)).strong(),
                            ),
                        );
                        ui.label(egui::RichText::new(count.to_string()).monospace());
                        ui.label(
                            egui::RichText::new(
                                after
                                    .map(|m| format!("median {}", dur(Some(m))))
                                    .unwrap_or_default(),
                            )
                            .size(12.0)
                            .color(p.muted),
                        )
                        .on_hover_text("From opened");
                    });
                    kit::meter(ui, count as f32 / n, p.cyan);
                    ui.add_space(6.0);
                }
            });
    });
}

/// Range chips: `days` > 0 is the last that many days, 0 today, -1 month to date.
const RANGES: [(i64, &str); 5] = [
    (0, "Today"),
    (7, "7d"),
    (30, "30d"),
    (-1, "MTD"),
    (90, "90d"),
];

fn range_of(days: i64, now: i64) -> Range {
    match days {
        0 => Range::today(now),
        d if d < 0 => Range::month_to_date(now),
        d => Range::last_days(d, now),
    }
}

/// One outcome: its facts, timeline, model cost per model, people and links.
fn outcome_view(ui: &mut egui::Ui, d: &OutcomeDetail, p: &theme::Palette) {
    let o = &d.outcome;
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(icons::TARGET)
                .size(20.0)
                .color(p.gold_text),
        );
        ui.label(egui::RichText::new(&o.title).size(20.0).strong());
        ui.label(
            egui::RichText::new(&o.outcome_key)
                .monospace()
                .size(12.0)
                .color(p.muted),
        );
        if !o.result.is_empty() {
            kit::chip(ui, &o.result, p.violet);
        }
    });
    ui.add_space(10.0);
    kpis(ui, |ui| {
        kpi(ui, icons::TIMER, "Cycle", dur(o.cycle_ms), None);
        kpi(ui, icons::EYE, "1st review", dur(o.first_review_ms), None);
        kpi(ui, icons::ROBOT, "Agent", dur(Some(d.agent_ms)), None);
        kpi(
            ui,
            icons::COINS,
            "Model cost",
            format!("${:.2}", d.cost_usd),
            None,
        );
        kpi(
            ui,
            icons::USERS_THREE,
            "People",
            d.people.len().to_string(),
            None,
        );
    });
    ui.add_space(14.0);
    kit::section(
        ui,
        icons::LINK,
        "Links",
        false,
        |_| {},
        |ui| {
            let l = &d.links;
            ui.horizontal_wrapped(|ui| {
                if !l.card_key.is_empty() {
                    kit::chip(ui, &icons::label(icons::KANBAN, &l.card_key), p.cyan)
                        .on_hover_text(&l.card_title);
                }
                if !l.pr_ref.is_empty() {
                    if l.pr_url.is_empty() {
                        kit::chip(
                            ui,
                            &icons::label(icons::GIT_PULL_REQUEST, &l.pr_ref),
                            p.violet,
                        );
                    } else {
                        ui.hyperlink_to(
                            icons::label(icons::GIT_PULL_REQUEST, &l.pr_ref),
                            &l.pr_url,
                        );
                    }
                }
                for i in &l.issues {
                    kit::chip(ui, &icons::label(icons::CHECK_CIRCLE, i), p.green);
                }
                for r in &l.runs {
                    kit::chip(ui, &icons::label(icons::FLOW_ARROW, &fmt_id(r)), p.muted)
                        .on_hover_text(r);
                }
                ui.label(
                    egui::RichText::new(icons::label(
                        icons::ROBOT,
                        &format!("{} sessions", l.sessions.len()),
                    ))
                    .color(p.muted),
                )
                .on_hover_text(l.sessions.join("\n"));
            });
        },
    );
    ui.add_space(14.0);
    kit::Split::new(320.0, 320.0, kit::Widths::Halves).show(ui, |ui, side| {
        if side == 0 {
            kit::section(
                ui,
                icons::CLOCK,
                "Timeline",
                false,
                |_| {},
                |ui| {
                    if d.timeline.is_empty() {
                        kit::empty(ui, icons::CLOCK, "No events");
                    }
                    for t in &d.timeline {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(fmt::when(t.at))
                                    .monospace()
                                    .size(12.0)
                                    .color(p.muted),
                            );
                            ui.label(egui::RichText::new(&t.what).strong());
                            if !t.who.is_empty() {
                                ui.label(egui::RichText::new(&t.who).color(p.cyan));
                            }
                            if !t.note.is_empty() {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&t.note).color(p.muted))
                                        .truncate(),
                                );
                            }
                        })
                        .response
                        .on_hover_text(&t.source);
                    }
                },
            );
        } else {
            kit::section(
                ui,
                icons::COINS,
                "Cost by model",
                false,
                |_| {},
                |ui| {
                    if d.cost_by_model.is_empty() {
                        kit::empty(ui, icons::COINS, "No sessions");
                    }
                    for m in &d.cost_by_model {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(icons::label(icons::CPU, &m.model)).strong(),
                            )
                            .on_hover_text(&m.provider);
                            ui.label(
                                egui::RichText::new(format!(
                                    "${:.2} · {} tok · {} · {}×",
                                    m.cost_usd,
                                    m.tokens,
                                    dur(Some(m.agent_ms)),
                                    m.sessions
                                ))
                                .monospace()
                                .color(p.muted),
                            );
                        });
                    }
                },
            );
            ui.add_space(10.0);
            kit::section(
                ui,
                icons::USERS_THREE,
                "People",
                false,
                |_| {},
                |ui| {
                    if d.people.is_empty() {
                        kit::empty(ui, icons::HAND, "No person needed");
                    }
                    for x in &d.people {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(icons::label(icons::USER, &x.name)).strong(),
                            )
                            .on_hover_text(&x.key);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {} · {}",
                                    x.actions,
                                    dur(Some(x.time_ms)),
                                    x.parts.join(", ")
                                ))
                                .color(p.muted),
                            );
                        });
                    }
                },
            );
        }
    });
}

fn fmt_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The weighted role table agent work is classified by.
fn role_table(ui: &mut egui::Ui, p: &theme::Palette) {
    kit::section(
        ui,
        icons::LIST,
        "Role rules",
        false,
        |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "min {} · margin {} · dedicated {}",
                    delivery::ROLE_MIN_SCORE,
                    delivery::ROLE_MARGIN,
                    delivery::ROLE_DEDICATED
                ))
                .size(12.0)
                .color(p.muted),
            )
            .on_hover_text("Score: heaviest rule per evidence kind, summed");
        },
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt("role_rules")
                .max_height(260.0)
                .show(ui, |ui| {
                    egui::Grid::new("role_rules_grid")
                        .num_columns(4)
                        .striped(true)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            for t in ["Role", "Evidence", "Match", "Weight"] {
                                kit::th(ui, t);
                            }
                            ui.end_row();
                            let mut rules: Vec<&delivery::RoleRule> =
                                delivery::ROLE_RULES.iter().collect();
                            rules.sort_by(|a, b| a.role.cmp(b.role).then(b.weight.cmp(&a.weight)));
                            for r in rules {
                                ui.label(r.role);
                                ui.label(egui::RichText::new(r.kind).color(p.muted));
                                ui.label(egui::RichText::new(r.value).monospace());
                                ui.label(egui::RichText::new(r.weight.to_string()).monospace());
                                ui.end_row();
                            }
                        });
                });
        },
    );
}

/// The stored lifecycle ledger: per-stage flow, and the sweeps a person can run.
fn ledger_panel(
    ui: &mut egui::Ui,
    model: &Model,
    flow: Option<Result<Vec<LedgerFlow>, String>>,
    p: &theme::Palette,
) {
    kit::section(
        ui,
        icons::STACK,
        "Lifecycle ledger · 30d",
        false,
        |ui| {
            if kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Record"))
                .on_hover_text("Write new stage entries and exits to the ledger")
                .clicked()
            {
                model.act(|m| async move {
                    let r = delivery::lifecycle_sweep(m.core(), "owner").await?;
                    Ok(Some(match r.refused {
                        Some(why) => Notice::Error(format!("Refused: {why}")),
                        None => Notice::Info(format!("{} entered, {} exited", r.entered, r.exited)),
                    }))
                });
            }
            if kit::ghost_button(ui, &icons::label(icons::LINK, "Link + roll up"))
                .on_hover_text("Link outcomes to cards, PRs and sessions; fill agent time and cost")
                .clicked()
            {
                model.act(|m| async move {
                    let r = delivery::rollup(m.core(), "owner").await?;
                    Ok(Some(match r.refused {
                        Some(why) => Notice::Error(format!("Refused: {why}")),
                        None => Notice::Info(format!(
                            "{} linked, {} rolled up, {} re-captured, {} missed",
                            r.linked, r.rolled_up, r.recaptured, r.missed
                        )),
                    }))
                });
            }
        },
        |ui| match flow {
            Some(Ok(rows)) if !rows.is_empty() => {
                egui::Grid::new("ledger_flow")
                    .num_columns(5)
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for t in ["Stage", "Open", "In", "Out", "Median wait"] {
                            kit::th(ui, t);
                        }
                        ui.end_row();
                        for f in rows {
                            ui.label(egui::RichText::new(&f.stage).monospace());
                            for v in [f.open, f.arrivals, f.completions] {
                                ui.label(egui::RichText::new(v.to_string()).monospace());
                            }
                            ui.label(egui::RichText::new(dur(f.median_wait_ms)).monospace());
                            ui.end_row();
                        }
                    });
            }
            Some(Ok(_)) => kit::empty(ui, icons::STACK, "Nothing recorded"),
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
            }
            None => {
                ui.spinner();
            }
        },
    );
}

/// Ledger coverage per stage (web `GET /lifecycle/coverage`): the ledger's
/// entered / exited / open against the source rows, up to the last pass.
fn coverage_panel(
    ui: &mut egui::Ui,
    report: Option<Result<CoverageReport, String>>,
    p: &theme::Palette,
) {
    kit::section(
        ui,
        icons::SEAL_CHECK,
        "Ledger coverage · 7d",
        false,
        |_| {},
        |ui| match report {
            Some(Ok(r)) if !r.stages.is_empty() => {
                let n = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
                egui::Grid::new("ledger_coverage")
                    .num_columns(6)
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for t in ["Stage", "In", "Out", "Open", "Source", "Status"] {
                            kit::th(ui, t);
                        }
                        ui.end_row();
                        for s in &r.stages {
                            ui.label(&s.label).on_hover_text(&s.stage);
                            for v in [s.ledger.entered, s.ledger.exited, s.ledger.open] {
                                ui.label(egui::RichText::new(n(v)).monospace());
                            }
                            let t = s.truth.unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} / {} / {}",
                                    n(t.entered),
                                    n(t.exited),
                                    n(t.open)
                                ))
                                .monospace()
                                .color(p.muted),
                            );
                            let (icon, word, color) = match s.status {
                                "ok" => (icons::CHECK_CIRCLE, "ok".to_string(), p.green),
                                "gap" => (icons::WARNING, format!("gap {}", s.gap), p.amber),
                                _ => (icons::QUESTION, "not captured".to_string(), p.muted),
                            };
                            ui.label(egui::RichText::new(icons::label(icon, &word)).color(color));
                            ui.end_row();
                        }
                    });
            }
            Some(Ok(_)) => kit::empty(ui, icons::SEAL_CHECK, "Nothing to compare"),
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
            }
            None => {
                ui.spinner();
            }
        },
    );
}

/// Worker forge requests waiting for an answer, each steered by a person:
/// took over (with the pull request link), wait an hour, or failed. The
/// steer is a `steer` dispatch, audited with the person as actor and refused
/// under the kill switch (`Core::forge_steer_by`).
fn forge_panel(
    ui: &mut egui::Ui,
    model: &Model,
    reqs: Option<Result<Vec<kls_core::belai::forge::ForgeRequest>, String>>,
    url: &mut String,
    p: &theme::Palette,
) {
    kit::section(
        ui,
        icons::GIT_PULL_REQUEST,
        "Forge requests",
        false,
        |_| {},
        |ui| match reqs {
            Some(Ok(rows)) if !rows.is_empty() => {
                ui.add(
                    egui::TextEdit::singleline(url)
                        .hint_text(icons::label(icons::LINK, "Pull request URL"))
                        .desired_width(260.0),
                )
                .on_hover_text("Sent with Took over");
                egui::Grid::new("forge_requests")
                    .num_columns(5)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for t in ["Kind", "Repository", "Branch", "Node", ""] {
                            kit::th(ui, t);
                        }
                        ui.end_row();
                        for r in rows {
                            ui.label(egui::RichText::new(&r.kind).monospace())
                                .on_hover_text(if r.failure.is_empty() {
                                    r.title.clone()
                                } else {
                                    format!("{} · {}", r.failure, r.title)
                                });
                            ui.label(
                                egui::RichText::new(format!("{}/{}", r.repo_owner, r.repo_name))
                                    .monospace(),
                            );
                            ui.label(egui::RichText::new(&r.branch).monospace().color(p.muted));
                            ui.label(egui::RichText::new(&r.node_id).monospace().color(p.muted));
                            ui.horizontal(|ui| {
                                let link = url.trim().to_string();
                                let took = ui
                                    .add_enabled_ui(link.starts_with("https://"), |ui| {
                                        kit::ghost_button(
                                            ui,
                                            &icons::label(icons::CHECK, "Took over"),
                                        )
                                    })
                                    .inner
                                    .on_hover_text("Tell the worker its pull request is open");
                                let wait =
                                    kit::ghost_button(ui, &icons::label(icons::CLOCK, "Wait 1h"))
                                        .on_hover_text("Tell the worker to retry in an hour");
                                let fail =
                                    kit::ghost_button(ui, &icons::label(icons::X_CIRCLE, "Failed"))
                                        .on_hover_text("Tell the worker it will not be published");
                                let pick = if took.clicked() {
                                    Some("took_over")
                                } else if wait.clicked() {
                                    Some("waiting")
                                } else if fail.clicked() {
                                    Some("failed")
                                } else {
                                    None
                                };
                                if let Some(event) = pick {
                                    let (node, req) = (r.node_id.clone(), r.request_id.clone());
                                    let pr_url = if event == "took_over" {
                                        link
                                    } else {
                                        String::new()
                                    };
                                    let pr = kls_forge::forge::pull_ref_of(&pr_url)
                                        .map(|x| x.number as i32)
                                        .unwrap_or(0);
                                    let until = if event == "waiting" {
                                        kls_store::now_ms() + 3_600_000
                                    } else {
                                        0
                                    };
                                    let reason = if event == "failed" { "by_person" } else { "" };
                                    model.act(move |m| async move {
                                        let r = m
                                            .core()
                                            .forge_steer_by(
                                                "user", &node, &req, event, pr, &pr_url, until,
                                                reason,
                                            )
                                            .await;
                                        Ok(Some(match r {
                                            Ok(_) => {
                                                Notice::Info(format!("Steered {req}: {event}"))
                                            }
                                            Err(e) => Notice::Error(format!("Refused: {e:#}")),
                                        }))
                                    });
                                }
                            });
                            ui.end_row();
                        }
                    });
            }
            Some(Ok(_)) => kit::empty(ui, icons::GIT_PULL_REQUEST, "No forge requests"),
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
            }
            None => {
                ui.spinner();
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_chips_map_to_ranges() {
        let now = 1_795_341_600_000;
        assert_eq!(range_of(0, now).label, "today");
        assert_eq!(range_of(-1, now).label, "mtd");
        assert_eq!(range_of(7, now).label, "7d");
    }

    fn merged(at: i64, agent: bool) -> delivery::OutcomeRow {
        delivery::OutcomeRow {
            kind: "pr".into(),
            result: "merged".into(),
            resolved_at: Some(at),
            agent_ms: agent.then_some(60_000),
            ..Default::default()
        }
    }

    #[test]
    fn weeks_start_on_monday() {
        // 2026-11-16 is a Monday, 2026-11-22 a Sunday.
        let d = jiff::civil::date(2026, 11, 22)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond();
        let m = jiff::civil::date(2026, 11, 16)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond();
        assert_eq!(week_start(d + 5 * 3_600_000), m);
        assert_eq!(week_start(m), m);
    }

    #[test]
    fn merged_prs_bucket_by_week_and_kind() {
        let now = jiff::civil::date(2026, 11, 21)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond();
        let range = Range::last_days(30, now);
        let mut rows = vec![
            merged(now - DAY_MS, true),
            merged(now - DAY_MS, false),
            merged(now - 2 * DAY_MS, true),
        ];
        rows.push(delivery::OutcomeRow {
            kind: "issue".into(),
            result: "closed".into(),
            resolved_at: Some(now),
            ..Default::default()
        });
        let b = merged_buckets(&rows, &range);
        assert!(b.len() >= 5 && b.len() <= 6, "{b:?}");
        let last = b.last().unwrap();
        assert_eq!(last.0, "W47");
        assert_eq!((last.1, last.2), (2, 1));
        assert_eq!(b.iter().map(|x| x.1 + x.2).sum::<i64>(), 3);
    }

    #[test]
    fn short_ranges_bucket_by_day() {
        let now = jiff::civil::date(2026, 11, 21)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond();
        let b = merged_buckets(&[merged(now, false)], &Range::last_days(7, now));
        assert_eq!(b.len(), 7);
        assert_eq!(b.last().unwrap().0, "11-21");
        assert_eq!(b.last().unwrap().2, 1);
    }
}
