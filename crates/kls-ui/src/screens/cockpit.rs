//! Cockpit: KPI tiles, Needs you, Running now, the Pause all and A2A
//! blackout switches, a fleet overview and the latest events.

use crate::events::{clock, kind_color};
use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::screens::checkpoints::{kind_color as need_color, kind_icon as need_icon};
use crate::theme::{self, palette};
use egui::RichText;
use kls_core::board::REASON_MAX;
use kls_core::needs::{self, Checkpoint, CheckpointKind, Counts};
use kls_store::board::{Card, FactoryControl};
use std::sync::Arc;

pub const READY: bool = true;

/// Which switch a confirm dialog is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Switch {
    Pause,
    Blackout,
}

pub struct Screen {
    cards: Loader<Vec<Card>>,
    control: Loader<FactoryControl>,
    needs: Loader<Arc<Vec<Checkpoint>>>,
    counts: Loader<Counts>,
    live: Loader<Vec<kls_store::sessions::Session>>,
    confirm: Option<(Switch, String)>,
    kpis: Loader<kls_core::estimates::Kpis>,
    mode: Loader<kls_core::control::Mode>,
    /// KPI range in hours (24, 168, 720).
    range_h: Arc<std::sync::atomic::AtomicI64>,
    hud: Loader<Hud>,
    /// The run graph's show filter and search.
    graph_show: GraphShow,
    graph_find: String,
    /// The repository the eligibility list is for.
    elig_repo: String,
}

/// Where a "Needs you" row opens.
pub fn target(c: &Checkpoint) -> (crate::Screen, Option<String>) {
    match c.kind {
        CheckpointKind::Question => (crate::Screen::Sessions, c.session_id.clone()),
        CheckpointKind::PullReview | CheckpointKind::PullReady => (
            crate::Screen::Repos,
            c.number.map(|n| format!("{}#{n}", c.repo)),
        ),
        CheckpointKind::CardReview | CheckpointKind::CardBlocked => {
            (crate::Screen::Board, Some(c.card_id.clone()))
        }
        CheckpointKind::HostHandoff | CheckpointKind::HostUpdate | CheckpointKind::NodeOffline => (
            crate::Screen::Fleet,
            c.url.strip_prefix("keleustes://fleet/").map(str::to_string),
        ),
        CheckpointKind::QuotaEscalated => (crate::Screen::Budgets, Some("usage".into())),
        _ => (crate::Screen::Checkpoints, Some(c.key.clone())),
    }
}

/// A KPI tile: icon, label, value, note, value colour, the screen it opens.
type Tile = (
    &'static str,
    &'static str,
    String,
    String,
    Option<egui::Color32>,
    crate::Screen,
);

/// The action button a "Needs you" row shows.
fn need_action(k: CheckpointKind) -> &'static str {
    match k {
        CheckpointKind::Question => "Answer",
        CheckpointKind::Approval
        | CheckpointKind::Gate
        | CheckpointKind::PullReview
        | CheckpointKind::CardReview => "Review",
        _ => "Open",
    }
}

/// "18m", "1h 02m", "3d 4h".
fn duration(ms: i64) -> String {
    let m = (ms / 60_000).max(0);
    match m {
        0..=59 => format!("{m}m"),
        60..=1439 => format!("{}h {:02}m", m / 60, m % 60),
        _ => format!("{}d {}h", m / 1440, (m % 1440) / 60),
    }
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let range_h = Arc::new(std::sync::atomic::AtomicI64::new(24));
        let r2 = range_h.clone();
        Self {
            cards: Loader::new(model, &["card."], |core| async move { core.cards().await }),
            control: Loader::new(model, &["factory."], |core| async move {
                core.factory_control().await
            }),
            needs: Loader::new(model, &needs::RELOAD_ON, |core| async move {
                Ok(Arc::new(core.needs_you().await?))
            }),
            counts: Loader::new(
                model,
                &["session.", "approval.", "run."],
                |core| async move { core.counts().await },
            ),
            live: Loader::new(model, &["session."], |core| async move {
                core.store()
                    .sessions(&kls_store::sessions::SessionQuery {
                        open_only: true,
                        limit: 50,
                        ..Default::default()
                    })
                    .await
            }),
            kpis: Loader::new(
                model,
                &["session.", "run.", "card.", "github.pull", "settings."],
                move |core| {
                    let h = r2.load(std::sync::atomic::Ordering::Relaxed);
                    async move {
                        kls_core::estimates::kpis(&core, kls_store::now_ms() - h * 3_600_000).await
                    }
                },
            ),
            mode: Loader::new(model, &["factory."], |core| async move {
                Ok(kls_core::control::state(&core).await?.mode)
            }),
            range_h,
            hud: Loader::new(
                model,
                &[
                    "run.",
                    "decision.",
                    "factory.",
                    "a2a.",
                    "node.",
                    "approval.",
                    "session.",
                ],
                |core| async move { load_hud(core).await },
            ),
            confirm: None,
            graph_show: GraphShow::All,
            graph_find: String::new(),
            elig_repo: String::new(),
        }
    }

    pub fn focus(&mut self, _id: &str) {}

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        egui::ScrollArea::vertical()
            .id_salt("cockpit-page")
            .auto_shrink([false, false])
            .show(ui, |ui| self.page(ui, model));
    }

    fn page(&mut self, ui: &mut egui::Ui, model: &Model) {
        let snap = model.snapshot();
        let p = palette(ui.ctx());
        let cards = match self.cards.get() {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                ui.colored_label(p.red, e);
                Vec::new()
            }
            None => Vec::new(),
        };
        let control = self.control.get();

        let now0 = kls_store::now_ms();
        kit::page_header(ui, icons::GAUGE, "Cockpit", |ui| {
            match &control {
                Some(Ok(c)) => self.switches(ui, model, c),
                Some(Err(e)) => {
                    ui.colored_label(p.red, icons::label(icons::WARNING, &e.to_string()));
                }
                None => {
                    ui.spinner();
                }
            }
            ui.add_space(8.0);
            stream_status(ui, model, &snap, now0);
        });
        if let Some(Ok(c)) = &control
            && (c.stopped || c.a2a_blackout)
        {
            ui.horizontal_wrapped(|ui| {
                let stopped = matches!(self.mode.get(), Some(Ok(kls_core::control::Mode::Stop)));
                if c.stopped {
                    kit::chip(
                        ui,
                        &format!(
                            "{}  {}{}{}",
                            if stopped { icons::STOP } else { icons::PAUSE },
                            if stopped {
                                "Stopped"
                            } else {
                                "Paused · fires held"
                            },
                            c.stopped_since
                                .map(|t| format!(" since {}", fmt::when(t)))
                                .unwrap_or_default(),
                            if c.stopped_reason.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", c.stopped_reason)
                            }
                        ),
                        p.amber,
                    );
                }
                if c.a2a_blackout {
                    kit::chip(
                        ui,
                        &format!(
                            "{}  A2A blackout{}",
                            icons::PROHIBIT,
                            if c.a2a_reason.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", c.a2a_reason)
                            }
                        ),
                        p.amber,
                    );
                }
            });
            ui.add_space(12.0);
        }

        let needs: Arc<Vec<Checkpoint>> = match self.needs.get() {
            Some(Ok(n)) => n,
            _ => Arc::new(Vec::new()),
        };
        let counts = self.counts.get().and_then(Result::ok);
        let live = match self.live.get() {
            Some(Ok(v)) => v,
            _ => Vec::new(),
        };
        let now = kls_store::now_ms();
        let num = |v: Option<usize>| v.map(|n| n.to_string()).unwrap_or_else(|| "…".into());

        // KPI tiles.
        let joined: Vec<_> = snap
            .nodes
            .iter()
            .filter(|n| n.revoked_at.is_none())
            .collect();
        let offline: Vec<_> = joined
            .iter()
            .filter(|n| !snap.online.contains(&n.id))
            .collect();
        let nodes_note = match offline.first() {
            None if joined.is_empty() => "none joined".to_string(),
            None => "all online".to_string(),
            Some(n) => {
                let seen = n
                    .last_seen_at
                    .map(|t| format!(" {}", duration(now - t)))
                    .unwrap_or_default();
                if offline.len() > 1 {
                    format!("{} +{} offline", n.name, offline.len() - 1)
                } else {
                    format!("{} offline{seen}", n.name)
                }
            }
        };
        let per_status = |s: &str| cards.iter().filter(|c| c.status == s).count();
        let oldest = needs.iter().map(|c| c.ts).min();
        let open_cards = cards.iter().filter(|c| c.status != "done").count();
        let board_note = ["triage", "backlog", "todo"]
            .iter()
            .map(|s| format!("{} {s}", per_status(s)))
            .collect::<Vec<_>>()
            .join(" · ");
        let k = self.kpis.get().and_then(Result::ok);
        let kn = |f: fn(&kls_core::estimates::Kpis) -> String| {
            k.as_ref().map(f).unwrap_or_else(|| "…".into())
        };
        let range_h = self.range_h.load(std::sync::atomic::Ordering::Relaxed);
        let range_label = match range_h {
            24 => "24 h",
            168 => "7 d",
            _ => "30 d",
        };
        ui.horizontal(|ui| {
            ui.label(kit::caps(&icons::label(icons::CLOCK, "Range"), p.muted));
            for (h, l) in [(24, "24 h"), (168, "7 d"), (720, "30 d")] {
                if kit::pill(ui, l, range_h == h).clicked() && range_h != h {
                    self.range_h.store(h, std::sync::atomic::Ordering::Relaxed);
                    self.kpis.reload();
                }
            }
        });
        ui.add_space(8.0);
        let mut tiles: Vec<Tile> = vec![
            (
                icons::COINS,
                "Tokens",
                kn(|k| crate::screens::budgets::tokens(k.tokens)),
                format!(
                    "{range_label} · {}",
                    kn(|k| format!("{} sessions", k.sessions))
                ),
                None,
                crate::Screen::Budgets,
            ),
            (
                icons::CURRENCY_DOLLAR,
                "Spend",
                kn(|k| format!("${:.2}", k.spend_usd)),
                kn(|k| format!("≈ ${} by hand", k.by_hand_value.round())),
                None,
                crate::Screen::Budgets,
            ),
            (
                icons::CHECK_CIRCLE,
                "Done",
                kn(|k| k.cards_done.to_string()),
                kn(|k| format!("{} runs ok · {} failed", k.runs_succeeded, k.runs_failed)),
                Some(p.green),
                crate::Screen::Runs,
            ),
            (
                icons::GIT_PULL_REQUEST,
                "Draft PRs",
                kn(|k| k.draft_prs.to_string()),
                kn(|k| format!("{} conflicting", k.conflicting_prs)),
                None,
                crate::Screen::Checkpoints,
            ),
        ];
        tiles.extend(vec![
            (
                icons::USERS_THREE,
                "Free slots",
                kn(|k| {
                    k.free_slots
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "—".into())
                }),
                "on online nodes".into(),
                None,
                crate::Screen::Fleet,
            ),
            (
                icons::GITHUB_LOGO,
                "GitHub quota",
                kn(|k| match k.quota {
                    Some((r, l)) => format!("{:.0}%", r as f64 * 100.0 / l.max(1) as f64),
                    None => "—".into(),
                }),
                kn(|k| match k.quota {
                    Some((r, l)) => format!("{r} of {l} left"),
                    None => "not seen yet".into(),
                }),
                None,
                crate::Screen::Repos,
            ),
            (
                icons::DATABASE,
                "Cache hits",
                kn(|k| {
                    k.cache_hit
                        .map(|c| format!("{:.0}%", c * 100.0))
                        .unwrap_or_else(|| "—".into())
                }),
                "GitHub reads this hour".into(),
                None,
                crate::Screen::Repos,
            ),
        ]);
        tiles.extend(vec![
            (
                icons::HARD_DRIVES,
                "Nodes online",
                format!("{} / {}", snap.online.len(), joined.len()),
                nodes_note,
                None,
                crate::Screen::Fleet,
            ),
            (
                icons::TERMINAL_WINDOW,
                "Live sessions",
                num(counts.as_ref().map(|c| c.live_sessions)),
                format!(
                    "{} open · {} asking",
                    num(counts.as_ref().map(|c| c.open_sessions)),
                    num(counts.as_ref().map(|c| c.questions)),
                ),
                Some(p.cyan),
                crate::Screen::Sessions,
            ),
            (
                icons::BELL,
                "Needs you",
                needs.len().to_string(),
                oldest
                    .map(|t| format!("oldest {}", duration(now - t)))
                    .unwrap_or_else(|| "all clear".into()),
                Some(p.gold_text),
                crate::Screen::Checkpoints,
            ),
            (
                icons::SEAL_CHECK,
                "Approvals",
                num(counts.as_ref().map(|c| c.pending_approvals)),
                "pending".into(),
                None,
                crate::Screen::Checkpoints,
            ),
            (
                crate::screens::board::status_icon("review"),
                "In review",
                per_status("review").to_string(),
                format!("{} doing", per_status("doing")),
                Some(p.violet),
                crate::Screen::Board,
            ),
            (
                icons::KANBAN,
                "Open cards",
                open_cards.to_string(),
                board_note,
                None,
                crate::Screen::Board,
            ),
        ]);
        kit::Grid::new(170.0)
            .tight_rows()
            .show(ui, tiles.len(), |ui, i, w| {
                let (icon, label, value, note, color, to) = &tiles[i];
                if kit::kpi_tile(ui, w, icon, label, value, Some(note), *color).clicked() {
                    model.go(*to, None);
                }
            });
        ui.add_space(12.0);
        self.hud_panels(ui, model, k.as_ref());
        ui.add_space(20.0);

        // Needs you | Running now.
        kit::Split::new(520.0, 380.0, kit::Widths::Flex)
            .gap(20.0)
            .fit_height()
            .show(ui, |ui, side| {
                if side == 0 {
                    kit::section(
                        ui,
                        icons::BELL,
                        "Needs you",
                        true,
                        |ui| {
                            if kit::link(ui, "Open checkpoints", p.gold_text).clicked() {
                                model.go(crate::Screen::Checkpoints, None);
                            }
                            // Counts by kind, most first.
                            let mut by: Vec<(CheckpointKind, usize)> = Vec::new();
                            for c in needs.iter() {
                                match by.iter_mut().find(|(k, _)| *k == c.kind) {
                                    Some((_, n)) => *n += 1,
                                    None => by.push((c.kind, 1)),
                                }
                            }
                            by.sort_by_key(|x| std::cmp::Reverse(x.1));
                            for (k, n) in by.into_iter() {
                                if kit::pick_chip(
                                    ui,
                                    &format!("{} {n}", need_icon(k)),
                                    false,
                                    need_color(k, &p),
                                )
                                .on_hover_text(format!("{} · {n}", k.words()))
                                .clicked()
                                {
                                    model.go(
                                        crate::Screen::Checkpoints,
                                        Some(format!("kind:{}", k.key())),
                                    );
                                }
                            }
                        },
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("cockpit-needs")
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    if needs.is_empty() {
                                        ui.label(
                                            RichText::new(icons::label(
                                                icons::CHECK_CIRCLE,
                                                "All clear",
                                            ))
                                            .color(p.muted),
                                        );
                                    }
                                    for c in needs.iter().take(30) {
                                        need_row(ui, model, c, now);
                                    }
                                    if needs.len() > 30 {
                                        ui.add_space(8.0);
                                        if kit::link(
                                            ui,
                                            &format!("All {}", needs.len()),
                                            p.gold_text,
                                        )
                                        .clicked()
                                        {
                                            model.go(crate::Screen::Checkpoints, None);
                                        }
                                    }
                                });
                        },
                    );
                } else {
                    kit::section(
                        ui,
                        icons::PLAY,
                        "Running now",
                        false,
                        |ui| {
                            if kit::link(ui, "All sessions", p.cyan).clicked() {
                                model.go(crate::Screen::Sessions, None);
                            }
                        },
                        |ui| {
                            let running: Vec<&Card> =
                                cards.iter().filter(|c| c.status == "doing").collect();
                            egui::ScrollArea::vertical()
                                .id_salt("cockpit-running")
                                .max_height(360.0)
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    let lanes =
                                        k.as_ref().map(|k| k.lanes.as_slice()).unwrap_or(&[]);
                                    for lane in lanes {
                                        lane_row(ui, model, lane, now);
                                    }
                                    if running.is_empty() && live.is_empty() && lanes.is_empty() {
                                        ui.label(
                                            RichText::new(icons::label(icons::MINUS, "Idle"))
                                                .color(p.muted),
                                        );
                                    }
                                    for s in &live {
                                        session_row(ui, model, s, &snap, &cards, now);
                                    }
                                    for c in running {
                                        card_row(ui, model, c);
                                    }
                                });
                        },
                    );
                }
            });
        ui.add_space(20.0);

        // Fleet | Latest events.
        kit::Split::new(420.0, 420.0, kit::Widths::Flex)
            .gap(20.0)
            .fit_height()
            .show(ui, |ui, side| {
                if side == 0 {
                    kit::section(
                        ui,
                        icons::HARD_DRIVES,
                        "Fleet",
                        false,
                        |ui| {
                            if kit::link(ui, "Manage nodes", p.cyan).clicked() {
                                model.go(crate::Screen::Fleet, None);
                            }
                        },
                        |ui| {
                            if joined.is_empty() {
                                ui.label(
                                    RichText::new(icons::label(icons::HARD_DRIVES, "No nodes"))
                                        .color(p.muted),
                                );
                                return;
                            }
                            kit::cell_grid(ui, joined.len(), 180.0, 6, |ui, i| {
                                let n = joined[i];
                                let on = snap.online.contains(&n.id);
                                ui.horizontal(|ui| {
                                    kit::dot(ui, if on { p.green } else { p.muted });
                                    ui.label(RichText::new(&n.name).strong());
                                });
                                let caps = &n.host.capabilities;
                                let mut parts = vec![n.host.os.clone()];
                                if on {
                                    if caps.remote_control {
                                        parts.push("rc".into());
                                    }
                                    if caps.gh_authenticated {
                                        parts.push("gh".into());
                                    }
                                    if caps.terminal {
                                        parts.push("terminal".into());
                                    }
                                    let k = live.iter().filter(|s| s.node_id == n.id).count();
                                    if k > 0 {
                                        parts.push(format!(
                                            "{k} session{}",
                                            if k == 1 { "" } else { "s" }
                                        ));
                                    }
                                } else {
                                    parts.push(match n.last_seen_at {
                                        Some(t) => format!("offline {}", duration(now - t)),
                                        None => "never seen".into(),
                                    });
                                }
                                ui.label(kit::mono(&parts.join(" · "), 12.0, p.muted));
                            });
                        },
                    );
                } else {
                    kit::section(
                        ui,
                        icons::PULSE,
                        "Latest events",
                        false,
                        |ui| {
                            if kit::link(ui, "Event stream", p.cyan).clicked() {
                                model.go(crate::Screen::Events, None);
                            }
                        },
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            if snap.events.is_empty() {
                                ui.label(
                                    RichText::new(icons::label(icons::PULSE, "No events"))
                                        .color(p.muted),
                                );
                            }
                            for e in snap.events.iter().rev().take(8) {
                                let (r, _) = kit::list_row(ui, ("cockpit-ev", e.id), false, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [64.0, 16.0],
                                            egui::Label::new(kit::mono(
                                                &clock(now, e.at),
                                                13.0,
                                                p.muted,
                                            )),
                                        )
                                        .on_hover_text(fmt::when(e.at));
                                        ui.add_sized(
                                            [130.0, 16.0],
                                            egui::Label::new(kit::mono(
                                                &e.kind,
                                                13.0,
                                                kind_color(&p, &e.kind),
                                            ))
                                            .truncate(),
                                        );
                                        ui.add(
                                            egui::Label::new(RichText::new(&e.subject).size(13.0))
                                                .truncate(),
                                        );
                                    });
                                });
                                if r.clicked() {
                                    model.go(
                                        crate::Screen::Events,
                                        Some(format!("kind:{}", e.kind)),
                                    );
                                }
                            }
                        },
                    );
                }
            });

        ui.add_space(20.0);
        self.health(ui, model, &cards, now);

        // "ago" labels age without events.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(10));
        self.confirm_dialog(ui.ctx(), model);
    }

    fn switches(&mut self, ui: &mut egui::Ui, model: &Model, c: &FactoryControl) {
        // Right to left: the A2A switch, then Pause all.
        if c.a2a_blackout {
            if theme::primary_button(ui, &icons::label(icons::BROADCAST, "Lift blackout")).clicked()
            {
                model.act(|m| async move {
                    m.core().set_a2a_blackout(false, "").await?;
                    Ok(Some(Notice::Info("A2A relay back on".into())))
                });
            }
        } else if kit::ghost_button(ui, &icons::label(icons::BROADCAST, "A2A: on"))
            .on_hover_text("Stop relaying agent messages (blackout)")
            .clicked()
        {
            self.confirm = Some((Switch::Blackout, String::new()));
        }
        if c.stopped {
            if theme::primary_button(ui, &icons::label(icons::PLAY, "Resume")).clicked() {
                model.act(|m| async move {
                    kls_core::control::resume(m.core()).await?;
                    Ok(Some(Notice::Info("Automations resumed".into())))
                });
            }
        } else if kit::danger_button(ui, &icons::label(icons::PAUSE, "Pause all"))
            .on_hover_text("Pause all automations")
            .clicked()
        {
            self.confirm = Some((Switch::Pause, String::new()));
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, model: &Model) {
        let Some((which, mut reason)) = self.confirm.take() else {
            return;
        };
        let (title, text) = match which {
            Switch::Pause => (
                "Pause all automations",
                "Holds triggers and new steps. Sessions unaffected.",
            ),
            Switch::Blackout => (
                "A2A blackout",
                "Stops agent relay. Local talk and memory continue.",
            ),
        };
        let mut keep = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(text);
                ui.add(
                    egui::TextEdit::singleline(&mut reason)
                        .hint_text("reason (optional)")
                        .char_limit(REASON_MAX),
                );
                ui.horizontal(|ui| {
                    if kit::danger_button(ui, &icons::label(icons::POWER, "Turn on")).clicked() {
                        let r = reason.trim().to_string();
                        model.act(move |m| async move {
                            match which {
                                Switch::Pause => {
                                    kls_core::control::pause(m.core(), &r).await?;
                                }
                                Switch::Blackout => {
                                    m.core().set_a2a_blackout(true, &r).await?;
                                }
                            };
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
            self.confirm = Some((which, reason));
        }
    }
}

fn need_row(ui: &mut egui::Ui, model: &Model, c: &Checkpoint, now: i64) {
    let p = palette(ui.ctx());
    let (row, clicked) = kit::list_row(ui, ("cockpit-need", &c.key), false, |ui| {
        let mut go = false;
        ui.horizontal(|ui| {
            kit::chip(
                ui,
                &format!("{} {}", need_icon(c.kind), c.kind.words().to_uppercase()),
                need_color(c.kind, &p),
            );
            let btn_w = 90.0;
            let text_w = (ui.available_width() - btn_w).max(120.0);
            ui.allocate_ui_with_layout(
                egui::vec2(text_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&c.title).size(14.0).strong()).truncate(),
                    );
                    let mut detail: Vec<String> = Vec::new();
                    for s in [&c.workflow, &c.repo, &c.reason] {
                        if !s.is_empty() {
                            detail.push(s.clone());
                        }
                    }
                    detail.push(format!("waiting {}", duration(now - c.ts)));
                    ui.add(
                        egui::Label::new(
                            RichText::new(detail.join(" · ")).size(12.0).color(p.muted),
                        )
                        .truncate(),
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::primary_button(ui, need_action(c.kind)).clicked() {
                    go = true;
                }
            });
        });
        go
    });
    if clicked || row.clicked() {
        let (screen, id) = target(c);
        model.go(screen, id);
    }
}

fn session_row(
    ui: &mut egui::Ui,
    model: &Model,
    s: &kls_store::sessions::Session,
    snap: &crate::model::Snapshot,
    cards: &[Card],
    now: i64,
) {
    let p = palette(ui.ctx());
    let (r, _) = kit::list_row(ui, ("cockpit-sess", &s.id), false, |ui| {
        ui.horizontal(|ui| {
            kit::dot(ui, kit::status_color(&p, &s.state));
            let title = if s.title.is_empty() { &s.id } else { &s.title };
            ui.add(egui::Label::new(RichText::new(title).size(14.0).strong()).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(kit::mono(&duration(now - s.started_at), 12.0, p.muted));
            });
        });
        let node = snap
            .nodes
            .iter()
            .find(|n| n.id == s.node_id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let card = cards
            .iter()
            .find(|c| !s.card_id.is_empty() && c.id == s.card_id)
            .map(|c| c.key.clone())
            .unwrap_or_default();
        let meta: Vec<&str> = [node.as_str(), card.as_str(), s.model.as_str(), &s.state]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect();
        ui.label(kit::mono(&meta.join(" · "), 12.0, p.muted));
    });
    if r.clicked() {
        model.go(crate::Screen::Sessions, Some(s.id.clone()));
    }
}

/// One open workflow run: workflow and repo, a glyph per step state, the
/// progress and tokens; opens the run.
fn lane_row(ui: &mut egui::Ui, model: &Model, l: &kls_core::estimates::RunLane, now: i64) {
    let p = palette(ui.ctx());
    let r = ui.horizontal(|ui| {
        ui.label(RichText::new(icons::TREE_STRUCTURE).color(p.gold_text));
        ui.label(RichText::new(l.workflow_id.chars().take(18).collect::<String>()).strong());
        if !l.repo.is_empty() {
            ui.label(kit::mono(&l.repo, 12.0, p.muted));
        }
        for (name, st) in &l.steps {
            let (g, c) = match st.as_str() {
                "succeeded" | "skipped" => (icons::CHECK_CIRCLE, p.green),
                "running" => (icons::CIRCLE_NOTCH, p.cyan),
                "waiting" => (icons::HOURGLASS, p.gold_text),
                "failed" => (icons::X_CIRCLE, p.red),
                "pending" => (icons::DOT_OUTLINE, p.muted),
                _ => (icons::CIRCLE, p.muted),
            };
            ui.label(RichText::new(g).color(c))
                .on_hover_text(format!("{name}: {st}"));
        }
        ui.label(kit::mono(
            &format!(
                "{:.0}% · {}{}",
                l.progress * 100.0,
                crate::screens::budgets::tokens(l.tokens),
                l.started_at
                    .map(|t| format!(" · {}", duration(now - t)))
                    .unwrap_or_default()
            ),
            12.0,
            p.muted,
        ));
    });
    if r.response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        model.go(crate::Screen::Runs, Some(l.run_id.clone()));
    }
}

fn card_row(ui: &mut egui::Ui, model: &Model, c: &Card) {
    let p = palette(ui.ctx());
    let (r, _) = kit::list_row(ui, ("cockpit-card", &c.id), false, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::screens::board::status_icon(&c.status)).color(p.cyan));
            ui.add(egui::Label::new(RichText::new(&c.title).size(14.0).strong()).truncate());
        });
        let mut meta = vec![c.key.clone()];
        if !c.assignee.is_empty() {
            meta.push(format!("@{}", c.assignee));
        }
        if !c.repo.is_empty() {
            meta.push(c.repo.clone());
        }
        ui.label(kit::mono(&meta.join(" · "), 12.0, p.muted));
    });
    if r.clicked() {
        model.go(crate::Screen::Board, Some(c.id.clone()));
    }
}

// ---- HUD: System One, token economy, runs by hour, gates and provenance,
// ---- and Fleet / Health (workers, heartbeats, versions, friction, flow) --

/// A worker heartbeat older than this is late.
pub const LATE_BEAT_MS: i64 = 5 * 60_000;
const HUD_WINDOW_MS: i64 = 24 * 3_600_000;
/// Days the runs heatmap covers.
const HEAT_DAYS: i64 = 7;

/// SystemOne decisions in the window (`decision.made`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemOne {
    pub decisions: usize,
    /// Routed by the answer (not to a person, not failed).
    pub routed: usize,
    /// Sent to a person (below the threshold, abstained).
    pub human: usize,
    pub failed: usize,
    pub avg_confidence: Option<f64>,
}

pub fn system_one(events: &[kls_store::Event]) -> SystemOne {
    let mut s = SystemOne::default();
    let mut sum = 0.0;
    let mut n = 0usize;
    for e in events.iter().filter(|e| e.kind == "decision.made") {
        let v: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
        s.decisions += 1;
        match v.get("action").and_then(|x| x.as_str()).unwrap_or("") {
            "human" => s.human += 1,
            "fail" => s.failed += 1,
            _ => s.routed += 1,
        }
        if let Some(c) = v.get("confidence").and_then(|x| x.as_f64()) {
            sum += c;
            n += 1;
        }
    }
    s.avg_confidence = (n > 0).then(|| sum / n as f64);
    s
}

/// Runs started per (weekday Mon = 0, hour) in local time over the last
/// `HEAT_DAYS` days.
pub fn runs_heatmap(starts: &[i64], now: i64) -> [[u32; 24]; 7] {
    let mut out = [[0u32; 24]; 7];
    let tz = jiff::tz::TimeZone::system();
    for &t in starts {
        if now - t > HEAT_DAYS * 86_400_000 || t > now {
            continue;
        }
        if let Ok(ts) = jiff::Timestamp::from_millisecond(t) {
            let z = ts.to_zoned(tz.clone());
            let d = z.weekday().to_monday_zero_offset() as usize;
            let h = z.hour() as usize;
            out[d][h] += 1;
        }
    }
    out
}

/// One worker row of the Fleet / Health panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkerRow {
    pub node: String,
    pub profile: String,
    pub state: String,
    pub card_id: String,
    pub beat_at: i64,
    pub late: bool,
    pub tokens: i64,
}

pub fn worker_rows(
    statuses: &[(String, i64, kls_core::belai::status::NodeStatus)],
    names: &std::collections::HashMap<String, String>,
    now: i64,
) -> Vec<WorkerRow> {
    let mut out = Vec::new();
    for (node, at, st) in statuses {
        for w in &st.workers {
            let beat = if w.beat_at > 0 { w.beat_at } else { *at };
            let live = !matches!(w.state.as_str(), "done" | "stopped" | "failed" | "ended");
            out.push(WorkerRow {
                node: names.get(node).cloned().unwrap_or_else(|| node.clone()),
                profile: if w.profile.is_empty() {
                    w.id.clone()
                } else {
                    w.profile.clone()
                },
                state: w.state.clone(),
                card_id: w.card_id.clone(),
                beat_at: beat,
                late: live && now - beat > LATE_BEAT_MS,
                tokens: w.tokens,
            });
        }
    }
    out.sort_by(|a, b| b.late.cmp(&a.late).then(a.beat_at.cmp(&b.beat_at)));
    out
}

/// The belai versions of the fleet and the nodes behind the newest one.
pub fn version_skew(versions: &[(String, String)]) -> (Option<String>, Vec<String>) {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let newest = versions
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .max_by(|a, b| parse(&a.1).cmp(&parse(&b.1)))
        .map(|(_, v)| v.clone());
    let behind = match &newest {
        Some(n) => versions
            .iter()
            .filter(|(_, v)| !v.is_empty() && v != n)
            .map(|(name, v)| format!("{name} {v}"))
            .collect(),
        None => Vec::new(),
    };
    (newest, behind)
}

/// Cards waiting per board column, with the oldest wait.
pub fn flow_queues(cards: &[Card], now: i64) -> Vec<(&'static str, usize, i64)> {
    ["triage", "backlog", "todo", "doing", "review"]
        .into_iter()
        .map(|s| {
            let in_col: Vec<&Card> = cards
                .iter()
                .filter(|c| !c.deleted && c.status == s)
                .collect();
            let oldest = in_col.iter().map(|c| now - c.updated_at).max().unwrap_or(0);
            (s, in_col.len(), oldest)
        })
        .collect()
}

/// A HUD panel: icon, title, (label, value) rows, the screen it opens and
/// the focus there.
type HudTile = (
    &'static str,
    &'static str,
    Vec<(String, String)>,
    crate::Screen,
    String,
);

/// What the HUD panels read, refreshed on events.
#[derive(Clone, Debug, Default)]
pub struct Hud {
    pub s1: SystemOne,
    pub heat: [[u32; 24]; 7],
    pub runs_24h: usize,
    pub refused_24h: usize,
    pub withheld_24h: usize,
    pub audited_24h: i64,
    pub gates_waiting: usize,
    pub failed_runs_24h: usize,
    pub workers: Vec<WorkerRow>,
    pub newest_belai: Option<String>,
    pub behind: Vec<String>,
    /// Open runs, each step in its state and on its node.
    pub graph: Vec<GraphRow>,
    /// What the eligibility list judges by: every node, who is online,
    /// each node's open dispatched sessions, and the managed repositories.
    pub nodes: Vec<kls_store::Node>,
    pub online: Vec<String>,
    pub busy: std::collections::HashMap<String, i64>,
    pub repos: Vec<String>,
}

/// One step of the run graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphNode {
    pub title: String,
    pub state: String,
    /// The node it runs on (by name), when it was placed.
    pub host: String,
}

/// One open run of the run graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphRow {
    pub run_id: String,
    pub workflow: String,
    pub repo: String,
    pub nodes: Vec<GraphNode>,
}

/// The run graph's show filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphShow {
    All,
    Running,
    Waiting,
    Failed,
}

impl GraphShow {
    const ALL: [GraphShow; 4] = [
        GraphShow::All,
        GraphShow::Running,
        GraphShow::Waiting,
        GraphShow::Failed,
    ];

    fn label(self) -> &'static str {
        match self {
            GraphShow::All => "All",
            GraphShow::Running => "Running",
            GraphShow::Waiting => "Waiting",
            GraphShow::Failed => "Failed",
        }
    }

    fn keeps(self, state: &str) -> bool {
        match self {
            GraphShow::All => true,
            GraphShow::Running => state == "running",
            GraphShow::Waiting => matches!(state, "waiting" | "pending"),
            GraphShow::Failed => state == "failed",
        }
    }
}

/// The run graph after the show filter and the search (workflow, repo,
/// run, step or node); a run with no step left is dropped.
pub fn filter_graph(rows: &[GraphRow], show: GraphShow, find: &str) -> Vec<GraphRow> {
    let q = find.trim().to_lowercase();
    rows.iter()
        .filter_map(|r| {
            let row_hit = [&r.workflow, &r.repo, &r.run_id]
                .iter()
                .any(|s| s.to_lowercase().contains(&q));
            let nodes: Vec<GraphNode> = r
                .nodes
                .iter()
                .filter(|n| show.keeps(&n.state))
                .filter(|n| {
                    q.is_empty()
                        || row_hit
                        || n.title.to_lowercase().contains(&q)
                        || n.host.to_lowercase().contains(&q)
                })
                .cloned()
                .collect();
            (!nodes.is_empty()).then(|| GraphRow { nodes, ..r.clone() })
        })
        .collect()
}

/// A step state's glyph and colour.
fn step_glyph(p: &theme::Palette, st: &str) -> (&'static str, egui::Color32) {
    match st {
        "succeeded" | "skipped" => (icons::CHECK_CIRCLE, p.green),
        "running" => (icons::CIRCLE_NOTCH, p.cyan),
        "waiting" => (icons::HOURGLASS, p.gold_text),
        "failed" => (icons::X_CIRCLE, p.red),
        "pending" => (icons::DOT_OUTLINE, p.muted),
        _ => (icons::CIRCLE, p.muted),
    }
}

/// An eligibility state's colour (ready and can clone take the run).
fn eligibility_color(p: &theme::Palette, state: &str) -> egui::Color32 {
    match state {
        "ready" | "can_clone" => p.green,
        "full" => p.gold_text,
        "no_profile" => p.amber,
        "offline" => p.red,
        _ => p.muted,
    }
}

async fn load_hud(core: kls_core::Core) -> anyhow::Result<Hud> {
    let now = kls_store::now_ms();
    let s = core.store();
    let since = now - HUD_WINDOW_MS;
    let recent = s
        .events_query(&kls_store::audit::EventQuery {
            from_at: Some(since),
            prefixes: vec![
                "decision.".into(),
                "factory.refused".into(),
                "a2a.withheld".into(),
            ],
            limit: 5000,
            ..Default::default()
        })
        .await?;
    let runs = s
        .runs(&kls_store::engine::RunFilter {
            from: Some(now - HEAT_DAYS * 86_400_000),
            limit: 5000,
            ..Default::default()
        })
        .await?;
    let starts: Vec<i64> = runs
        .iter()
        .map(|r| r.started_at.unwrap_or(r.created_at))
        .collect();
    let audited_24h = s
        .ledger_count(&kls_store::audit::LedgerQuery {
            from_at: Some(since),
            ..Default::default()
        })
        .await
        .unwrap_or(0);
    let gates_waiting = core
        .engine_checkpoints()
        .await?
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                CheckpointKind::Gate | CheckpointKind::DeployGate | CheckpointKind::Approval
            )
        })
        .count();
    let nodes = s.nodes().await?;
    let names: std::collections::HashMap<String, String> = nodes
        .iter()
        .map(|n| (n.id.clone(), n.name.clone()))
        .collect();
    let statuses = core.node_statuses().await.unwrap_or_default();
    let versions: Vec<(String, String)> = nodes
        .iter()
        .filter(|n| n.revoked_at.is_none())
        .map(|n| (n.name.clone(), n.host.belai_version.clone()))
        .collect();
    let (newest_belai, behind) = version_skew(&versions);
    let online = match core.gateway() {
        Ok(gw) => gw.online().await,
        Err(_) => Vec::new(),
    };
    let open = s
        .sessions(&kls_store::sessions::SessionQuery {
            open_only: true,
            limit: 2000,
            ..Default::default()
        })
        .await?;
    let mut busy: std::collections::HashMap<String, i64> = Default::default();
    for x in open.iter().filter(|x| !x.dispatch_id.is_empty()) {
        *busy.entry(x.node_id.clone()).or_default() += 1;
    }
    // The run graph: each open run's steps and the node each was placed on.
    let workflows: std::collections::HashMap<String, String> = s
        .workflows(false)
        .await?
        .into_iter()
        .map(|w| (w.id, w.name))
        .collect();
    let mut graph = Vec::new();
    for r in s
        .runs_in(&["running", "waiting_approval", "queued"])
        .await?
        .into_iter()
        .take(30)
    {
        let mut nodes = Vec::new();
        for st in s.run_steps(&r.id).await? {
            let mut host = String::new();
            if let Some(d) = st.dispatch_id.as_deref().filter(|d| !d.is_empty())
                && let Some(d) = s.dispatch(d).await?
            {
                host = d.node_id;
            } else if let Some(sid) = st.session_id.as_deref().filter(|x| !x.is_empty())
                && let Some(x) = s.session(sid).await?
            {
                host = x.node_id;
            } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(&st.details_json) {
                host = v["nodeId"].as_str().unwrap_or("").to_string();
            }
            nodes.push(GraphNode {
                title: if st.title.is_empty() {
                    st.node_id.clone()
                } else {
                    st.title.clone()
                },
                state: st.status.clone(),
                host: names.get(&host).cloned().unwrap_or(host),
            });
        }
        graph.push(GraphRow {
            workflow: workflows
                .get(&r.workflow_id)
                .cloned()
                .unwrap_or_else(|| r.workflow_id.clone()),
            run_id: r.id,
            repo: r.repo,
            nodes,
        });
    }
    let repos = s
        .gh_managed_repos()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.full_name)
        .collect();
    Ok(Hud {
        graph,
        nodes,
        online,
        busy,
        repos,
        s1: system_one(&recent),
        heat: runs_heatmap(&starts, now),
        runs_24h: starts.iter().filter(|t| **t >= since).count(),
        refused_24h: recent
            .iter()
            .filter(|e| e.kind == "factory.refused")
            .count(),
        withheld_24h: recent.iter().filter(|e| e.kind == "a2a.withheld").count(),
        audited_24h,
        gates_waiting,
        failed_runs_24h: runs
            .iter()
            .filter(|r| r.status == "failed" && r.updated_at >= since)
            .count(),
        workers: worker_rows(&statuses, &names, now),
        newest_belai,
        behind,
    })
}

/// Live stream status: events in the last minute, or the snapshot's error.
fn stream_status(ui: &mut egui::Ui, model: &Model, snap: &crate::model::Snapshot, now: i64) {
    let p = palette(ui.ctx());
    let per_min = snap.events.iter().filter(|e| now - e.at < 60_000).count();
    let (text, c) = match &snap.error {
        Some(_) => ("Stream error".to_string(), p.red),
        None => (format!("Live · {per_min}/min"), p.green),
    };
    let r = ui
        .horizontal(|ui| {
            kit::dot(ui, c);
            ui.label(
                RichText::new(icons::label(icons::BROADCAST, &text))
                    .size(12.0)
                    .color(p.muted),
            );
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_text(snap.error.clone().unwrap_or_else(|| "Event stream".into()));
    if r.clicked() {
        model.go(crate::Screen::Events, None);
    }
}

/// The runs-by-hour heatmap: a row per weekday, a cell per hour.
fn heatmap(ui: &mut egui::Ui, heat: &[[u32; 24]; 7]) {
    let p = palette(ui.ctx());
    let max = heat.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
    let label_w = 30.0;
    let w = ui.available_width().max(200.0);
    let cell = ((w - label_w) / 24.0).clamp(6.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(label_w + cell * 24.0, cell * 7.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let days = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    let mut hovered = None;
    for (d, row) in heat.iter().enumerate() {
        painter.text(
            egui::pos2(rect.left(), rect.top() + d as f32 * cell + cell / 2.0),
            egui::Align2::LEFT_CENTER,
            days[d],
            egui::FontId::proportional(10.0),
            p.muted,
        );
        for (h, n) in row.iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + label_w + h as f32 * cell,
                    rect.top() + d as f32 * cell,
                ),
                egui::vec2(cell - 1.5, cell - 1.5),
            );
            let fill = if *n == 0 {
                p.line
            } else {
                p.gold.gamma_multiply(0.25 + 0.75 * (*n as f32 / max))
            };
            painter.rect_filled(r, 2.0, fill);
            if resp.hover_pos().is_some_and(|pos| r.contains(pos)) {
                hovered = Some((d, h, *n));
            }
        }
    }
    if let Some((d, h, n)) = hovered {
        resp.on_hover_text(format!("{} {h:02}:00 · {n} runs", days[d]));
    }
}

impl Screen {
    fn hud_panels(
        &mut self,
        ui: &mut egui::Ui,
        model: &Model,
        k: Option<&kls_core::estimates::Kpis>,
    ) {
        let p = palette(ui.ctx());
        let Some(Ok(h)) = self.hud.get() else {
            return;
        };
        let k_done = k.map(|k| k.cards_done).unwrap_or(0);
        let tiles: Vec<HudTile> = vec![
            (
                icons::BRAIN,
                "System One",
                vec![
                    ("decisions 24 h".into(), h.s1.decisions.to_string()),
                    ("routed".into(), h.s1.routed.to_string()),
                    ("to a person".into(), h.s1.human.to_string()),
                    (
                        "avg confidence".into(),
                        h.s1.avg_confidence
                            .map(|c| format!("{:.0}%", c * 100.0))
                            .unwrap_or_else(|| "—".into()),
                    ),
                ],
                crate::Screen::Events,
                "kind:decision.".into(),
            ),
            (
                icons::COINS,
                "Token economy",
                vec![
                    (
                        "tokens".into(),
                        k.map(|k| crate::screens::budgets::tokens(k.tokens))
                            .unwrap_or_else(|| "…".into()),
                    ),
                    (
                        "spend".into(),
                        k.map(|k| format!("${:.2}", k.spend_usd))
                            .unwrap_or_else(|| "…".into()),
                    ),
                    (
                        "per done card".into(),
                        k.filter(|_| k_done > 0)
                            .map(|k| format!("${:.2}", k.spend_usd / k_done as f64))
                            .unwrap_or_else(|| "—".into()),
                    ),
                    (
                        "by hand".into(),
                        k.map(|k| format!("${}", k.by_hand_value.round()))
                            .unwrap_or_else(|| "…".into()),
                    ),
                ],
                crate::Screen::Budgets,
                String::new(),
            ),
            (
                icons::SHIELD_CHECK,
                "Gates and provenance",
                vec![
                    ("gates waiting".into(), h.gates_waiting.to_string()),
                    ("refused 24 h".into(), h.refused_24h.to_string()),
                    ("A2A withheld".into(), h.withheld_24h.to_string()),
                    ("audited 24 h".into(), h.audited_24h.to_string()),
                ],
                crate::Screen::Events,
                "needs".into(),
            ),
        ];
        kit::Grid::new(260.0).show(ui, tiles.len(), |ui, i, _w| {
            let (icon, title, rows, to, filter) = &tiles[i];
            kit::section(
                ui,
                icon,
                title,
                false,
                |ui| {
                    if kit::link(ui, &icons::label(icons::ARROW_RIGHT, "Open"), p.cyan).clicked() {
                        model.go(*to, (!filter.is_empty()).then(|| filter.clone()));
                    }
                },
                |ui| {
                    egui::Grid::new(("hud", i)).num_columns(2).show(ui, |ui| {
                        for (k, v) in rows {
                            ui.label(RichText::new(k).size(12.0).color(p.muted));
                            ui.label(kit::mono(v, 13.0, p.text));
                            ui.end_row();
                        }
                    });
                },
            );
        });
        ui.add_space(12.0);
        kit::section(
            ui,
            icons::CHART_BAR,
            "Runs by hour",
            false,
            |ui| {
                ui.label(kit::mono(
                    &format!("{} in 24 h · 7 d shown", h.runs_24h),
                    12.0,
                    p.muted,
                ));
            },
            |ui| heatmap(ui, &h.heat),
        );
    }

    fn health(&mut self, ui: &mut egui::Ui, model: &Model, cards: &[Card], now: i64) {
        let p = palette(ui.ctx());
        let Some(Ok(h)) = self.hud.get() else {
            return;
        };
        let late = h.workers.iter().filter(|w| w.late).count();
        kit::section(
            ui,
            icons::HARD_DRIVES,
            "Fleet / Health",
            late > 0 || !h.behind.is_empty(),
            |ui| {
                if kit::link(ui, &icons::label(icons::PULSE, "Node events"), p.cyan).clicked() {
                    model.go(crate::Screen::Events, Some("kind:node.".into()));
                }
            },
            |ui| {
                // Friction, versions and flow on one line each.
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::WARNING, "Friction 24 h"),
                        p.muted,
                    ));
                    let blocked = cards
                        .iter()
                        .filter(|c| kls_core::board::cards::is_blocked(c))
                        .count();
                    for (label, n, color) in [
                        ("failed runs", h.failed_runs_24h, p.red),
                        ("refusals", h.refused_24h, p.amber),
                        ("blocked cards", blocked, p.red),
                        ("late heartbeats", late, p.amber),
                    ] {
                        kit::chip(
                            ui,
                            &format!("{label} {n}"),
                            if n > 0 { color } else { p.muted },
                        );
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps(
                        &icons::label(icons::GIT_BRANCH, "belai"),
                        p.muted,
                    ));
                    match &h.newest_belai {
                        Some(v) => {
                            ui.label(kit::mono(v, 12.0, p.text));
                        }
                        None => {
                            ui.label(RichText::new("—").color(p.muted));
                        }
                    }
                    if h.behind.is_empty() {
                        kit::chip(ui, "no skew", p.green);
                    } else {
                        for b in &h.behind {
                            kit::chip(ui, &format!("behind: {b}"), p.amber);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(kit::caps(&icons::label(icons::FLOW_ARROW, "Flow"), p.muted));
                    for (s, n, oldest) in flow_queues(cards, now) {
                        let text = if n > 0 {
                            format!("{s} {n} · {}", duration(oldest))
                        } else {
                            format!("{s} 0")
                        };
                        if kit::pick_chip(ui, &text, false, p.muted).clicked() {
                            model.go(crate::Screen::Board, None);
                        }
                    }
                });
                ui.add_space(6.0);
                if h.workers.is_empty() {
                    ui.label(
                        RichText::new(icons::label(icons::ROBOT, "No workers reported"))
                            .color(p.muted),
                    );
                    return;
                }
                egui::Grid::new("hud-workers")
                    .num_columns(5)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for t in ["Worker", "Node", "State", "Beat", "Tokens"] {
                            ui.label(kit::caps(t, p.muted));
                        }
                        ui.end_row();
                        for w in h.workers.iter().take(20) {
                            ui.label(RichText::new(&w.profile).strong());
                            ui.label(kit::mono(&w.node, 12.0, p.muted));
                            ui.horizontal(|ui| {
                                kit::dot(ui, kit::status_color(&p, &w.state));
                                ui.label(&w.state);
                            });
                            ui.label(
                                RichText::new(if w.late {
                                    icons::label(icons::WARNING, &duration(now - w.beat_at))
                                } else {
                                    duration(now - w.beat_at)
                                })
                                .color(if w.late {
                                    p.amber
                                } else {
                                    p.muted
                                }),
                            );
                            ui.label(kit::mono(
                                &crate::screens::budgets::tokens(w.tokens),
                                12.0,
                                p.muted,
                            ));
                            ui.end_row();
                        }
                    });
            },
        );
        ui.add_space(12.0);
        run_graph(ui, model, &h, &mut self.graph_show, &mut self.graph_find);
        ui.add_space(12.0);
        eligibility(ui, &h, &mut self.elig_repo);
    }
}

/// Workflow runs: one row per open run, each step in its state and on its
/// node; a run opens in Runs.
fn run_graph(ui: &mut egui::Ui, model: &Model, h: &Hud, show: &mut GraphShow, find: &mut String) {
    let p = palette(ui.ctx());
    let rows = filter_graph(&h.graph, *show, find);
    kit::section(
        ui,
        icons::TREE_STRUCTURE,
        "Workflow runs",
        false,
        |ui| {
            if kit::link(ui, &icons::label(icons::ARROW_RIGHT, "Runs"), p.cyan).clicked() {
                model.go(crate::Screen::Runs, None);
            }
        },
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(find)
                        .desired_width(200.0)
                        .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Find")),
                );
                for s in GraphShow::ALL {
                    if kit::pick_chip(ui, s.label(), *show == s, p.cyan).clicked() {
                        *show = s;
                    }
                }
            });
            ui.add_space(6.0);
            if rows.is_empty() {
                ui.label(
                    RichText::new(icons::label(icons::TREE_STRUCTURE, "No open runs"))
                        .color(p.muted),
                );
                return;
            }
            for r in &rows {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .link(RichText::new(&r.workflow).strong())
                        .on_hover_text(&r.run_id)
                        .clicked()
                    {
                        model.go(crate::Screen::Runs, Some(r.run_id.clone()));
                    }
                    if !r.repo.is_empty() {
                        ui.label(kit::mono(&r.repo, 12.0, p.muted));
                    }
                    for (i, n) in r.nodes.iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new(icons::CARET_RIGHT).color(p.muted));
                        }
                        let (g, c) = step_glyph(&p, &n.state);
                        let text = if n.host.is_empty() {
                            format!("{g} {}", n.title)
                        } else {
                            format!("{g} {} · {}", n.title, n.host)
                        };
                        ui.label(RichText::new(text).size(12.0).color(c))
                            .on_hover_text(&n.state);
                    }
                });
            }
        },
    );
}

/// Who can run a repository: each node's eligibility and why not.
fn eligibility(ui: &mut egui::Ui, h: &Hud, repo: &mut String) {
    let p = palette(ui.ctx());
    let rows = kls_engine::route::host_eligibility(&h.nodes, &h.online, &h.busy, repo, "", "");
    kit::section(
        ui,
        icons::HARD_DRIVES,
        "Who can run",
        false,
        |ui| {
            egui::ComboBox::from_id_salt("cockpit-elig-repo")
                .selected_text(icons::label(
                    icons::GIT_BRANCH,
                    if repo.is_empty() {
                        "any repository"
                    } else {
                        repo.as_str()
                    },
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(repo, String::new(), "any repository");
                    for r in &h.repos {
                        ui.selectable_value(repo, r.clone(), r);
                    }
                });
        },
        |ui| {
            if rows.is_empty() {
                ui.label(
                    RichText::new(icons::label(icons::HARD_DRIVES, "No nodes")).color(p.muted),
                );
                return;
            }
            egui::Grid::new("cockpit-elig")
                .num_columns(3)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for e in &rows {
                        ui.label(RichText::new(&e.name).strong());
                        kit::chip(ui, e.state, eligibility_color(&p, e.state));
                        ui.label(RichText::new(&e.detail).size(12.0).color(p.muted));
                        ui.end_row();
                    }
                });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(kind: CheckpointKind) -> Checkpoint {
        Checkpoint {
            key: "k".into(),
            kind,
            ts: 0,
            title: String::new(),
            workflow: String::new(),
            run_id: String::new(),
            repo: "acme/api".into(),
            card_id: "c1".into(),
            step_id: None,
            approval: None,
            session_id: Some("s1".into()),
            number: Some(7),
            url: String::new(),
            reason: String::new(),
            sla_at: None,
            author: String::new(),
            node_id: String::new(),
            conflict: false,
        }
    }

    #[test]
    fn needs_rows_open_where_they_live() {
        assert_eq!(
            target(&cp(CheckpointKind::Question)),
            (crate::Screen::Sessions, Some("s1".into()))
        );
        assert_eq!(
            target(&cp(CheckpointKind::PullReady)),
            (crate::Screen::Repos, Some("acme/api#7".into()))
        );
        assert_eq!(
            target(&cp(CheckpointKind::CardBlocked)),
            (crate::Screen::Board, Some("c1".into()))
        );
        assert_eq!(
            target(&cp(CheckpointKind::Approval)),
            (crate::Screen::Checkpoints, Some("k".into()))
        );
    }

    #[test]
    fn host_needs_open_the_host_view() {
        let mut c = cp(CheckpointKind::HostHandoff);
        c.url = "keleustes://fleet/n1/overview".into();
        assert_eq!(
            target(&c),
            (crate::Screen::Fleet, Some("n1/overview".into()))
        );
        c.kind = CheckpointKind::HostUpdate;
        c.url = "keleustes://fleet/n1/provision".into();
        assert_eq!(
            target(&c),
            (crate::Screen::Fleet, Some("n1/provision".into()))
        );
    }

    #[test]
    fn durations() {
        assert_eq!(duration(18 * 60_000), "18m");
        assert_eq!(duration(62 * 60_000), "1h 02m");
        assert_eq!(duration(-5), "0m");
        assert_eq!(duration((24 * 60 + 61) * 60_000), "1d 1h");
    }

    #[test]
    fn hud_rules() {
        let ev = |kind: &str, data: &str| kls_store::Event {
            id: 1,
            at: 0,
            kind: kind.into(),
            subject: "r".into(),
            data_json: data.into(),
        };
        let s = system_one(&[
            ev("decision.made", r#"{"action":"route","confidence":0.9}"#),
            ev("decision.made", r#"{"action":"human","confidence":0.5}"#),
            ev("run.started", "{}"),
        ]);
        assert_eq!((s.decisions, s.routed, s.human), (2, 1, 1));
        assert!((s.avg_confidence.unwrap() - 0.7).abs() < 1e-9);

        let now = 30 * 86_400_000;
        let h = runs_heatmap(&[now - 1000, now - 3_600_000, now - 9 * 86_400_000], now);
        assert_eq!(
            h.iter().flatten().sum::<u32>(),
            2,
            "older than 7 days dropped"
        );

        let (newest, behind) = version_skew(&[
            ("a".into(), "1.4.0".into()),
            ("b".into(), "1.10.2".into()),
            ("c".into(), "".into()),
        ]);
        assert_eq!(newest.as_deref(), Some("1.10.2"));
        assert_eq!(behind, vec!["a 1.4.0".to_string()]);

        let st = kls_core::belai::status::NodeStatus {
            workers: vec![
                kls_core::belai::status::Worker {
                    id: "w1".into(),
                    profile: "builder".into(),
                    state: "running".into(),
                    beat_at: now - 10 * 60_000,
                    ..Default::default()
                },
                kls_core::belai::status::Worker {
                    id: "w2".into(),
                    state: "running".into(),
                    beat_at: now - 1000,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let names = [("n1".to_string(), "atlas".to_string())]
            .into_iter()
            .collect();
        let rows = worker_rows(&[("n1".into(), now, st)], &names, now);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].late && rows[0].profile == "builder" && rows[0].node == "atlas");
        assert!(!rows[1].late && rows[1].profile == "w2");

        let cards = vec![
            Card {
                status: "review".into(),
                updated_at: now - 60_000,
                ..Default::default()
            },
            Card {
                status: "review".into(),
                updated_at: now - 120_000,
                ..Default::default()
            },
        ];
        let q = flow_queues(&cards, now);
        assert_eq!(q[4], ("review", 2, 120_000));
        assert_eq!(q[0].1, 0);
    }

    #[test]
    fn quota_escalations_open_budget_usage() {
        let c = cp(CheckpointKind::QuotaEscalated);
        assert_eq!(target(&c), (crate::Screen::Budgets, Some("usage".into())));
    }

    #[test]
    fn run_graph_filters_by_state_and_search() {
        let node = |title: &str, state: &str, host: &str| GraphNode {
            title: title.into(),
            state: state.into(),
            host: host.into(),
        };
        let rows = vec![
            GraphRow {
                run_id: "r1".into(),
                workflow: "Fix CI".into(),
                repo: "acme/api".into(),
                nodes: vec![
                    node("Go", "succeeded", ""),
                    node("Build", "running", "box-a"),
                ],
            },
            GraphRow {
                run_id: "r2".into(),
                workflow: "Triage".into(),
                repo: "acme/web".into(),
                nodes: vec![node("Approve", "waiting", ""), node("Ship", "failed", "")],
            },
        ];
        assert_eq!(filter_graph(&rows, GraphShow::All, ""), rows);
        let r = filter_graph(&rows, GraphShow::Running, "");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].nodes, vec![node("Build", "running", "box-a")]);
        assert_eq!(filter_graph(&rows, GraphShow::Waiting, "")[0].run_id, "r2");
        assert_eq!(
            filter_graph(&rows, GraphShow::Failed, "")[0].nodes[0].title,
            "Ship"
        );
        // A host or a step matches its steps; a workflow or repo, the whole run.
        let r = filter_graph(&rows, GraphShow::All, "BOX-A");
        assert_eq!((r.len(), r[0].nodes.len()), (1, 1));
        assert_eq!(
            filter_graph(&rows, GraphShow::All, "acme/web")[0]
                .nodes
                .len(),
            2
        );
        assert!(filter_graph(&rows, GraphShow::All, "nothing").is_empty());
        assert!(GraphShow::ALL.iter().all(|s| !s.label().is_empty()));
    }

    #[test]
    fn offline_nodes_open_the_host_view() {
        let mut c = cp(CheckpointKind::NodeOffline);
        c.url = "keleustes://fleet/n9/overview".into();
        assert_eq!(
            target(&c),
            (crate::Screen::Fleet, Some("n9/overview".into()))
        );
    }
}
