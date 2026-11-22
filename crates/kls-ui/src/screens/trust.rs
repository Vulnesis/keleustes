//! Trust screen: SLSA-style levels per repository from the provenance
//! Keleustes can show, the scanner matrix, and the guardrails each node
//! reported, and the egress review strip (a refused host becomes a review
//! card or is dismissed). Owner: M6 finish workstream; egress review WP1.

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::Model;
use crate::theme::{self, Palette};
use egui::{Color32, Margin, RichText, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use kls_core::audit::trust::{self, SCANNERS, TrustReport};
use std::sync::Arc;

pub const READY: bool = true;

/// Space between panels.
const GAP: f32 = 16.0;

/// Guardrail counters: key, tile name, note.
const GUARDS: [(&str, &str, &str); 7] = [
    ("hazard.denied", "Hazards denied", "denied · 30 days"),
    ("hazard.asked", "Hazard asks", "asks · 30 days"),
    ("egress.denied", "Egress denied", "blocked · 30 days"),
    ("a2a.withheld", "A2A withheld", "withheld · 30 days"),
    ("run_cap", "Run caps", "caps hit · 30 days"),
    ("worker.fault", "Faults", "faults · 30 days"),
    ("gate.verified", "Gates run", "gates · 30 days"),
];

pub struct Screen {
    data: Loader<Arc<TrustReport>>,
    filter: String,
    focus: Option<String>,
    egress: Loader<Vec<kls_core::audit::review::EgressSighting>>,
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let data = Loader::new(
            model,
            &["session.", "run.", "github.", "node.", "audit.", "card."],
            |core| async move { Ok(Arc::new(core.trust().await?)) },
        );
        Self {
            data,
            filter: String::new(),
            focus: None,
            egress: Loader::new(model, &["audit.", "trust.", "card."], |core| async move {
                kls_core::audit::review::egress_review(&core).await
            }),
        }
    }

    /// Focuses a repository (`owner/name`).
    pub fn focus(&mut self, id: &str) {
        self.focus = Some(id.to_string());
        self.filter = id.to_string();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        let loaded = self.data.get();
        kit::page_header(ui, icons::SHIELD_CHECK, "Trust", |ui| {
            if kit::ghost_button(ui, icons::ARROW_CLOCKWISE)
                .on_hover_text("Refresh")
                .clicked()
            {
                self.data.reload();
            }
            if kit::ghost_button(ui, &icons::label(icons::EXPORT, "Evidence"))
                .on_hover_text("Export the evidence index (report, ledger head, signing key)")
                .clicked()
            {
                model.act(|m| async move {
                    let p = m.core().export_evidence_index().await?;
                    Ok(Some(crate::model::Notice::Info(format!(
                        "Evidence index in {}",
                        p.display()
                    ))))
                });
            }
            if kit::ghost_button(ui, &icons::label(icons::SCROLL, "Audit"))
                .on_hover_text("Gate verdicts in the hash-chained ledger")
                .clicked()
            {
                model.go(crate::Screen::Audit, Some("kind:gate".into()));
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(icons::label(icons::FUNNEL, "Filter"))
                    .desired_width(200.0),
            );
            if let Some(Ok(r)) = &loaded {
                ui.label(
                    RichText::new(icons::label(icons::CLOCK, &fmt::when(r.as_of)))
                        .color(pal.muted)
                        .size(12.0),
                )
                .on_hover_text("Read-only; from facts Keleustes holds");
            }
        });
        let r = match loaded {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        egui::ScrollArea::vertical()
            .id_salt("trust_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                kit::Split::new(320.0, 520.0, kit::Widths::First(1.0 / 3.0))
                    .fit_height()
                    .show(ui, |ui, side| {
                        if side == 0 {
                            level_card(ui, &r);
                        } else {
                            kit::section(
                                ui,
                                icons::MAGNIFYING_GLASS,
                                "Scanner matrix",
                                false,
                                |_| {},
                                |ui| self.scanners_ui(ui, &r),
                            );
                        }
                    });
                ui.add_space(GAP);
                guard_tiles(ui, &r, &pal);
                ui.add_space(GAP);
                kit::section(
                    ui,
                    icons::GIT_BRANCH,
                    "Repositories",
                    false,
                    |_| {},
                    |ui| self.levels_ui(ui, &r),
                );
                ui.add_space(GAP);
                kit::section(
                    ui,
                    icons::HAND_PALM,
                    "Guardrails by node",
                    false,
                    |_| {},
                    |ui| guardrails_ui(ui, &r),
                );
                ui.add_space(GAP);
                let egress = self.egress.get().and_then(Result::ok).unwrap_or_default();
                let pending = egress.iter().filter(|h| h.pending).count();
                kit::section(
                    ui,
                    icons::GLOBE,
                    &format!("Egress review · {pending} to review"),
                    pending > 0,
                    |_| {},
                    |ui| {
                        if egress.is_empty() {
                            ui.weak("No refusals in 30 days");
                        }
                        for h in egress.iter().take(50) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(kit::mono(&h.host, 13.0, pal.text));
                                kit::chip(ui, &h.count.to_string(), pal.red)
                                    .on_hover_text("Refusals in 30 days");
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} node(s) · last {}",
                                        h.nodes.len(),
                                        crate::fmt::when(h.last_at)
                                    ))
                                    .size(12.0)
                                    .color(pal.muted),
                                )
                                .on_hover_text(h.nodes.join(", "));
                                if let Some(r) = &h.review {
                                    if r.decision == "card" {
                                        if kit::ghost_button(
                                            ui,
                                            &icons::label(icons::KANBAN, &r.card_key),
                                        )
                                        .on_hover_text("The review card")
                                        .clicked()
                                        {
                                            model.go(crate::Screen::Board, Some(r.card_id.clone()));
                                        }
                                    } else {
                                        kit::chip(
                                            ui,
                                            &icons::label(icons::EYE_SLASH, "Dismissed"),
                                            pal.muted,
                                        )
                                        .on_hover_text(
                                            if r.note.is_empty() {
                                                crate::fmt::when(r.at)
                                            } else {
                                                r.note.clone()
                                            },
                                        );
                                    }
                                }
                                if h.pending {
                                    kit::chip(
                                        ui,
                                        &icons::label(icons::HOURGLASS, "To review"),
                                        pal.amber,
                                    );
                                    let host = h.host.clone();
                                    if kit::ghost_button(ui, &icons::label(icons::PLUS, "Card"))
                                        .on_hover_text("File a review card with this sighting")
                                        .clicked()
                                    {
                                        let host = host.clone();
                                        model.act(move |m| async move {
                                            let c =
                                                kls_core::audit::review::file_card(m.core(), &host)
                                                    .await?;
                                            Ok(Some(crate::model::Notice::Info(format!(
                                                "Review card {}",
                                                c.key
                                            ))))
                                        });
                                    }
                                    if kit::ghost_button(ui, icons::EYE_SLASH)
                                        .on_hover_text("Dismiss until refused again")
                                        .clicked()
                                    {
                                        model.act(move |m| async move {
                                            kls_core::audit::review::dismiss(m.core(), &host, "")
                                                .await?;
                                            Ok(None)
                                        });
                                    }
                                }
                            });
                        }
                    },
                );
            });
    }

    fn matches(&self, repo: &str) -> bool {
        let f = self.filter.trim().to_lowercase();
        f.is_empty() || repo.to_lowercase().contains(&f)
    }

    fn levels_ui(&mut self, ui: &mut egui::Ui, r: &TrustReport) {
        let pal = theme::palette(ui.ctx());
        let rows: Vec<_> = r
            .repos
            .iter()
            .filter(|x| self.matches(&x.facts.repo))
            .collect();
        if rows.is_empty() {
            ui.weak(if r.repos.is_empty() {
                "No repositories"
            } else {
                "No matches"
            });
            return;
        }
        TableBuilder::new(ui)
            .id_salt("trust_repos")
            .striped(false)
            .resizable(true)
            .sense(egui::Sense::click())
            .max_scroll_height(420.0)
            .column(Column::initial(220.0).at_least(120.0).clip(true))
            .column(Column::exact(56.0))
            .column(Column::initial(100.0))
            .column(Column::initial(100.0))
            .column(Column::initial(110.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .header(26.0, |mut h| {
                for t in [
                    "Repository",
                    "Level",
                    "Sessions",
                    "Runs",
                    "Signed",
                    "Reason",
                ] {
                    h.col(|ui| kit::th(ui, t));
                }
            })
            .body(|body| {
                body.rows(30.0, rows.len(), |mut row| {
                    let t = rows[row.index()];
                    let f = &t.facts;
                    row.set_selected(self.focus.as_deref() == Some(f.repo.as_str()));
                    row.col(|ui| {
                        kit::rule(ui);
                        let mut txt = RichText::new(&f.repo);
                        if f.managed {
                            txt = txt.strong();
                        }
                        ui.add(egui::Label::new(txt).truncate());
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        kit::chip(ui, &format!("L{}", t.level), level_color(&pal, t.level));
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        ui.label(
                            RichText::new(format!("{}/{}", f.linked_sessions, f.sessions))
                                .monospace(),
                        )
                        .on_hover_text("Linked / total");
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        ui.label(
                            RichText::new(format!("{}/{}", f.linked_runs, f.runs)).monospace(),
                        )
                        .on_hover_text("Linked / total");
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        let signed = f.signed_commits + f.verified_resolutions;
                        let total = f.commits + f.resolutions;
                        ui.label(RichText::new(format!("{signed}/{total}")).monospace());
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        ui.add(
                            egui::Label::new(RichText::new(&t.reason).monospace().color(pal.muted))
                                .truncate(),
                        )
                        .on_hover_text(trust::reason_words(&t.reason));
                    });
                });
            });
    }

    fn scanners_ui(&mut self, ui: &mut egui::Ui, r: &TrustReport) {
        let pal = theme::palette(ui.ctx());
        let t = kls_core::audit::trust::matrix_totals(&r.scanners);
        ui.label(
            egui::RichText::new(format!(
                "{} repos · {} clean · {} findings in {} repos · {} not run",
                t.repos, t.clean, t.findings, t.repos_with_findings, t.not_run
            ))
            .size(12.0)
            .color(pal.muted),
        );
        let rows: Vec<_> = r
            .scanners
            .iter()
            .filter(|x| self.matches(&x.repo))
            .collect();
        if rows.is_empty() {
            ui.weak("No scanner results");
            return;
        }
        egui::ScrollArea::horizontal()
            .id_salt("trust_scanners_h")
            .show(ui, |ui| {
                let mut tb = TableBuilder::new(ui)
                    .id_salt("trust_scanners")
                    .striped(false)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .max_scroll_height(360.0)
                    .column(Column::initial(180.0).at_least(120.0).clip(true))
                    .column(Column::exact(40.0));
                for _ in SCANNERS {
                    tb = tb.column(Column::initial(84.0).at_least(64.0));
                }
                tb.header(26.0, |mut h| {
                    h.col(|ui| kit::th(ui, "Repository"));
                    h.col(|ui| kit::th(ui, "L"));
                    for s in SCANNERS {
                        h.col(|ui| kit::th(ui, s));
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let x = rows[row.index()];
                        row.set_selected(self.focus.as_deref() == Some(x.repo.as_str()));
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.add(egui::Label::new(RichText::new(&x.repo).strong()).truncate());
                        });
                        row.col(|ui| {
                            kit::rule(ui);
                            ui.label(
                                RichText::new(format!("L{}", x.level))
                                    .monospace()
                                    .color(level_color(&pal, x.level)),
                            );
                        });
                        for s in SCANNERS {
                            row.col(|ui| {
                                kit::rule(ui);
                                let cell = x.cells.get(s);
                                let state = cell.map(|c| c.state.as_str()).unwrap_or("not_run");
                                let (words, color) = match state {
                                    "clean" => ("clean", pal.green),
                                    "findings" => ("findings", pal.red),
                                    _ => ("not run", pal.muted),
                                };
                                let resp = kit::chip(ui, words, color);
                                match cell.filter(|c| c.at > 0) {
                                    Some(c) => resp.on_hover_text(format!(
                                        "{s}: {words} · {} · {}",
                                        c.source,
                                        fmt::when(c.at)
                                    )),
                                    None => resp.on_hover_text(format!("{s}: {words}")),
                                };
                            });
                        }
                    });
                });
            });
    }
}

/// The fleet's build provenance level: the lowest level among managed
/// repositories (all repositories when none is managed).
fn fleet_level(r: &TrustReport) -> Option<u8> {
    let managed: Vec<u8> = r
        .repos
        .iter()
        .filter(|x| x.facts.managed)
        .map(|x| x.level)
        .collect();
    if managed.is_empty() {
        r.repos.iter().map(|x| x.level).min()
    } else {
        managed.into_iter().min()
    }
}

fn level_color(pal: &Palette, level: u8) -> Color32 {
    match level {
        0 => pal.red,
        1 => pal.amber,
        2 => pal.green,
        _ => pal.muted,
    }
}

fn level_words(level: u8) -> (&'static str, &'static str) {
    match level {
        0 => ("No build", trust::reason_words("no_factory_build")),
        1 => (
            "Recorded",
            "Factory work recorded; links or signatures missing",
        ),
        2 => ("Linked + signed", trust::reason_words("hosted_signed")),
        _ => ("Isolated", "Needs isolated builders; Keleustes has none"),
    }
}

/// The big SLSA level panel: level, level bars, per-level counts and the
/// provenance totals behind them.
fn level_card(ui: &mut Ui, r: &TrustReport) {
    let pal = theme::palette(ui.ctx());
    let level = fleet_level(r);
    kit::glow(ui).inner_margin(Margin::same(20)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(kit::caps("Build provenance", pal.muted));
        ui.add_space(6.0);
        let value = match level {
            Some(l) => format!("SLSA L{l}"),
            None => "SLSA —".to_string(),
        };
        let resp = ui.label(
            RichText::new(value)
                .size(52.0)
                .strong()
                .color(pal.gold_text),
        );
        if let Some(l) = level {
            resp.on_hover_text(format!(
                "Lowest level across {} repositories · {}",
                if r.repos.iter().any(|x| x.facts.managed) {
                    "managed"
                } else {
                    "all"
                },
                level_words(l).1
            ));
        }
        ui.add_space(8.0);
        // One bar per level above L0.
        let top = r.levels.iter().map(|b| b.level).max().unwrap_or(3).max(3);
        let reached = level.unwrap_or(0);
        let w = ui.available_width();
        let gap = 6.0;
        let bar_w = (w - gap * (top as f32 - 1.0)) / top as f32;
        let (rect, _) = ui.allocate_exact_size(vec2(w, 8.0), egui::Sense::hover());
        for i in 0..top {
            let x = rect.left() + i as f32 * (bar_w + gap);
            let bar = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(bar_w, 8.0));
            let c = if i < reached { pal.gold } else { pal.line };
            ui.painter().rect_filled(bar, 0.0, c);
        }
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            for b in &r.levels {
                let (words, hover) = level_words(b.level);
                let color = level_color(&pal, b.level);
                kit::chip(ui, &format!("L{} · {}", b.level, b.count), color)
                    .on_hover_text(format!("{words}: {hover}"));
            }
        });
        ui.add_space(8.0);
        let p = &r.provenance;
        ui.horizontal_wrapped(|ui| {
            kit::chip(
                ui,
                &icons::label(
                    icons::TERMINAL_WINDOW,
                    &format!("{}/{}", p.linked_sessions, p.sessions),
                ),
                pal.text,
            )
            .on_hover_text("Sessions linked to a run or card");
            kit::chip(
                ui,
                &icons::label(icons::PLAY_CIRCLE, &format!("{}/{}", p.linked_runs, p.runs)),
                pal.text,
            )
            .on_hover_text("Runs linked to a card");
            kit::chip(
                ui,
                &icons::label(
                    icons::FINGERPRINT,
                    &format!("{}/{}", p.signed_commits, p.commits),
                ),
                pal.text,
            )
            .on_hover_text("Recorded commits signed");
            for (k, v) in &r.settings {
                kit::chip(ui, &icons::label(icons::GEAR_SIX, v), pal.muted).on_hover_text(k);
            }
        });
    });
}

/// Guardrail tiles: one per counter, summed over every node, each with a
/// coloured top strip.
fn guard_tiles(ui: &mut Ui, r: &TrustReport, pal: &Palette) {
    let total = |k: &str| -> i64 {
        r.guardrails
            .iter()
            .map(|g| g.counts.get(k).copied().unwrap_or(0))
            .sum()
    };
    let nodes = r.guardrails.len();
    let tiles: Vec<(&str, i64, &str, Color32)> = GUARDS
        .iter()
        .map(|(k, name, note)| {
            let n = total(k);
            let color = match *k {
                "hazard.denied" | "egress.denied" | "worker.fault" => {
                    if n > 0 {
                        pal.red
                    } else {
                        pal.green
                    }
                }
                "hazard.asked" => pal.gold,
                "a2a.withheld" => pal.violet,
                "run_cap" => pal.amber,
                _ => pal.cyan,
            };
            (*name, n, *note, color)
        })
        .collect();
    kit::Grid::new(220.0).show(ui, tiles.len(), |ui, i, _| {
        let (name, n, note, color) = tiles[i];
        kit::strip_tile(ui, color, name, &n.to_string(), note)
            .on_hover_text(format!("Reported by {nodes} node(s)' belai"));
    });
}

fn guardrails_ui(ui: &mut egui::Ui, r: &TrustReport) {
    let pal = theme::palette(ui.ctx());
    if r.guardrails.is_empty() {
        ui.weak("No nodes");
        return;
    }
    const COLS: [(&str, &str); 7] = [
        ("hazard.asked", "Hazard asks"),
        ("hazard.denied", "Hazards denied"),
        ("egress.denied", "Egress denied"),
        ("a2a.withheld", "A2A withheld"),
        ("run_cap", "Run caps"),
        ("worker.fault", "Faults"),
        ("gate.verified", "Gates run"),
    ];
    egui::ScrollArea::horizontal()
        .id_salt("trust_guardrails_h")
        .show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .id_salt("trust_guardrails")
                .striped(false)
                .resizable(true)
                .sense(egui::Sense::click())
                .max_scroll_height(360.0)
                .column(Column::initial(180.0).at_least(100.0).clip(true))
                .column(Column::initial(90.0));
            for _ in COLS {
                tb = tb.column(Column::initial(110.0));
            }
            tb.header(26.0, |mut h| {
                h.col(|ui| kit::th(ui, "Node"));
                h.col(|ui| kit::th(ui, "State"));
                for (_, t) in COLS {
                    h.col(|ui| kit::th(ui, t));
                }
            })
            .body(|body| {
                body.rows(30.0, r.guardrails.len(), |mut row| {
                    let g = &r.guardrails[row.index()];
                    row.col(|ui| {
                        kit::rule(ui);
                        ui.add(egui::Label::new(RichText::new(&g.name).strong()).truncate())
                            .on_hover_text(&g.node_id);
                    });
                    row.col(|ui| {
                        kit::rule(ui);
                        let state = if g.revoked {
                            "revoked"
                        } else if g.online {
                            "online"
                        } else {
                            "offline"
                        };
                        ui.horizontal(|ui| {
                            kit::dot(ui, kit::status_color(&pal, state));
                            ui.label(RichText::new(state).size(12.0).color(pal.muted));
                        });
                    });
                    for (k, _) in COLS {
                        row.col(|ui| {
                            kit::rule(ui);
                            let n = g.counts.get(k).copied().unwrap_or(0);
                            let t = RichText::new(n.to_string()).monospace();
                            if n > 0 && k.ends_with("denied") {
                                ui.label(t.color(pal.amber));
                            } else {
                                ui.label(t);
                            }
                        });
                    }
                });
            });
        });
}
