//! Board pieces: the drawer's Sessions and Run tabs and its ticket gaps,
//! the Stale lens rule, Done grouped by day, the three-way Sync tab and the
//! Review tab's keys (J/K move, C confirm, X dismiss).

use crate::events::clock;
use crate::fmt;
use crate::icons;
use crate::kit;
use crate::loader::Loader;
use crate::model::Model;
use crate::theme::palette;
use egui::RichText;
use kls_core::board::rules::TicketIssue;
use kls_store::board::Card;
use kls_store::engine::{Run, RunFilter};
use kls_store::sessions::{Session, SessionQuery};

/// A card is stale when nothing touched it for this long (not done).
pub const STALE_DAYS: i64 = 14;

pub fn is_stale(c: &Card, now: i64) -> bool {
    c.status != "done" && now - c.updated_at > STALE_DAYS * 86_400_000
}

/// The drawer's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrawerTab {
    #[default]
    Card,
    GitHub,
    Sessions,
    Run,
    Activity,
}

impl DrawerTab {
    pub const ALL: [DrawerTab; 5] = [
        DrawerTab::Card,
        DrawerTab::GitHub,
        DrawerTab::Sessions,
        DrawerTab::Run,
        DrawerTab::Activity,
    ];
    pub fn face(self) -> (&'static str, &'static str) {
        match self {
            DrawerTab::Card => (icons::KANBAN, "Card"),
            DrawerTab::GitHub => (icons::GITHUB_LOGO, "GitHub"),
            DrawerTab::Sessions => (icons::TERMINAL_WINDOW, "Sessions"),
            DrawerTab::Run => (icons::PLAY_CIRCLE, "Run"),
            DrawerTab::Activity => (icons::PULSE, "Activity"),
        }
    }
}

/// A card's sessions and runs, newest first.
#[derive(Clone, Debug, Default)]
pub struct Work {
    pub sessions: Vec<Session>,
    pub runs: Vec<Run>,
}

pub fn work_loader(model: &Model, card_id: &str) -> Loader<Work> {
    let id = card_id.to_string();
    Loader::new(model, &["session.", "run.", "card."], move |core| {
        let id = id.clone();
        async move {
            let s = core.store();
            let sessions = s
                .sessions(&SessionQuery {
                    card_id: Some(id.clone()),
                    limit: 50,
                    ..Default::default()
                })
                .await?;
            let mut runs: Vec<Run> = s
                .runs(&RunFilter {
                    q: Some(id.clone()),
                    limit: 50,
                    ..Default::default()
                })
                .await?
                .into_iter()
                .filter(|r| r.card_id == id)
                .collect();
            runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
            Ok(Work { sessions, runs })
        }
    })
}

pub fn sessions_tab(ui: &mut egui::Ui, model: &Model, work: &Loader<Work>) {
    let p = palette(ui.ctx());
    let Some(Ok(w)) = work.get() else {
        ui.spinner();
        return;
    };
    if w.sessions.is_empty() {
        ui.label(RichText::new(icons::label(icons::TERMINAL_WINDOW, "No sessions")).color(p.muted));
        return;
    }
    let now = kls_store::now_ms();
    for s in &w.sessions {
        let (r, _) = kit::list_row(ui, ("card-session", &s.id), false, |ui| {
            ui.horizontal(|ui| {
                kit::dot(ui, kit::status_color(&p, &s.state));
                let t = if s.title.is_empty() { &s.id } else { &s.title };
                ui.add(egui::Label::new(RichText::new(t).strong()).truncate());
            });
            let meta: Vec<String> = [
                s.state.clone(),
                s.agent.clone(),
                s.model.clone(),
                format!("{} tok", s.input_tokens + s.output_tokens),
                fmt::ago(now, s.seen_at),
            ]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect();
            ui.label(kit::mono(&meta.join(" · "), 12.0, p.muted));
        });
        if r.clicked() {
            model.go(crate::Screen::Sessions, Some(s.id.clone()));
        }
    }
}

pub fn run_tab(ui: &mut egui::Ui, model: &Model, work: &Loader<Work>) {
    let p = palette(ui.ctx());
    let Some(Ok(w)) = work.get() else {
        ui.spinner();
        return;
    };
    if w.runs.is_empty() {
        ui.label(RichText::new(icons::label(icons::PLAY_CIRCLE, "No runs")).color(p.muted));
        return;
    }
    let now = kls_store::now_ms();
    for r in &w.runs {
        let (row, _) = kit::list_row(ui, ("card-run", &r.id), false, |ui| {
            ui.horizontal(|ui| {
                kit::dot(ui, kit::status_color(&p, &r.status));
                ui.label(RichText::new(&r.status).strong());
                ui.label(kit::mono(
                    &r.id.chars().take(13).collect::<String>(),
                    12.0,
                    p.muted,
                ));
            });
            let meta: Vec<String> = [
                r.trigger_kind.clone(),
                r.reason.clone(),
                format!("{} tok", r.tokens),
                clock(now, r.created_at),
            ]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect();
            ui.label(kit::mono(&meta.join(" · "), 12.0, p.muted));
        });
        if row.clicked() {
            model.go(crate::Screen::Runs, Some(r.id.clone()));
        }
    }
}

/// The ticket standard's gaps for what is being edited (shown live).
pub fn ticket_gaps(ui: &mut egui::Ui, gaps: &[TicketIssue]) {
    let p = palette(ui.ctx());
    if gaps.is_empty() {
        ui.label(
            RichText::new(icons::label(
                icons::CHECK_CIRCLE,
                "Meets the ticket standard",
            ))
            .size(12.0)
            .color(p.green),
        );
        return;
    }
    egui::CollapsingHeader::new(
        RichText::new(icons::label(
            icons::LIST_CHECKS,
            &format!("Ticket gaps {}", gaps.len()),
        ))
        .size(12.0)
        .color(p.amber),
    )
    .id_salt("ticket-gaps")
    .default_open(false)
    .show(ui, |ui| {
        for g in gaps {
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::mono(&g.code, 11.0, p.muted));
                ui.label(RichText::new(&g.message).size(12.0));
            });
        }
    });
}

/// The local calendar day of a time, `YYYY-MM-DD`.
pub fn day_of(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| "?".into())
}

/// Done cards by day, newest day first; each day's cards newest first.
pub fn by_day<'a>(rows: &[&'a Card]) -> Vec<(String, Vec<&'a Card>)> {
    let mut out: Vec<(String, Vec<&'a Card>)> = Vec::new();
    let mut sorted: Vec<&Card> = rows.to_vec();
    sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.key.cmp(&b.key)));
    for c in sorted {
        let d = day_of(c.updated_at);
        match out.iter_mut().find(|(k, _)| *k == d) {
            Some((_, v)) => v.push(c),
            None => out.push((d, vec![c])),
        }
    }
    out
}

/// The Done tab: cards grouped by the day they finished, each with Reopen.
pub fn done_by_day(ui: &mut egui::Ui, model: &Model, rows: &[&Card], open: &mut Option<Card>) {
    let p = palette(ui.ctx());
    if rows.is_empty() {
        kit::empty(ui, icons::CHECK_CIRCLE, "Nothing done yet");
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("done-days")
        .show(ui, |ui| {
            for (day, cards) in by_day(rows) {
                egui::CollapsingHeader::new(
                    RichText::new(icons::label(
                        icons::CALENDAR,
                        &format!("{day} · {}", cards.len()),
                    ))
                    .strong(),
                )
                .id_salt(("done-day", &day))
                .default_open(true)
                .show(ui, |ui| {
                    for c in cards {
                        ui.horizontal(|ui| {
                            ui.label(kit::mono(&c.key, 12.0, p.muted));
                            if ui
                                .add(
                                    egui::Label::new(RichText::new(&c.title).size(13.0))
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                *open = Some(c.clone());
                            }
                            for l in c.labels.iter().take(3) {
                                kit::tag(ui, l);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if kit::ghost_button(
                                        ui,
                                        &icons::label(icons::ARROW_U_UP_LEFT, "Reopen"),
                                    )
                                    .on_hover_text("Back to review")
                                    .clicked()
                                    {
                                        super::move_act(model, c, "review");
                                    }
                                    ui.label(kit::mono(
                                        &clock(kls_store::now_ms(), c.updated_at),
                                        11.0,
                                        p.muted,
                                    ));
                                },
                            );
                        });
                    }
                });
            }
        });
}

/// The three-way Sync tab: per field the base (last agreed), the board's
/// and GitHub's value, with Keep board / Take GitHub.
pub fn sync_tab(ui: &mut egui::Ui, model: &Model, rows: &[kls_core::board::SyncRow]) {
    let p = palette(ui.ctx());
    if rows.is_empty() {
        kit::empty(ui, icons::CHECK_CIRCLE, "Board and GitHub agree");
        return;
    }
    for row in rows {
        let r = &row.conflict;
        kit::card(ui).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(kit::mono(&r.key, 12.0, p.muted));
                ui.label(RichText::new(format!("{}#{}", r.repo, r.number)).strong());
                if !r.url.is_empty() && kit::ghost_button(ui, icons::ARROW_SQUARE_OUT).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&r.url));
                }
            });
            egui::Grid::new(("sync3", &r.card_id))
                .num_columns(5)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    for t in ["Field", "Base", "Board", "GitHub", ""] {
                        ui.label(kit::caps(t, p.muted));
                    }
                    ui.end_row();
                    for (i, f) in r.fields.iter().enumerate() {
                        let base = row.base.get(i).cloned().flatten();
                        let (board_moved, gh_moved) = match &base {
                            Some(b) => (f.board != *b, f.github != *b),
                            None => (false, false),
                        };
                        ui.label(kit::caps(&f.field, p.muted));
                        ui.label(
                            RichText::new(base.clone().unwrap_or_else(|| "—".into()))
                                .color(p.muted),
                        )
                        .on_hover_text("Last value both sides agreed on");
                        let mark = |moved: bool| if moved { " •" } else { "" };
                        ui.label(RichText::new(format!("{}{}", f.board, mark(board_moved))))
                            .on_hover_text(if board_moved {
                                "Changed on the board"
                            } else {
                                "Board"
                            });
                        ui.label(
                            RichText::new(format!("{}{}", f.github, mark(gh_moved))).color(p.cyan),
                        )
                        .on_hover_text(if gh_moved {
                            "Changed on GitHub"
                        } else {
                            "GitHub"
                        });
                        ui.horizontal(|ui| {
                            for (label, take) in [
                                (icons::label(icons::KANBAN, "Keep board"), false),
                                (icons::label(icons::GITHUB_LOGO, "Take GitHub"), true),
                            ] {
                                if kit::ghost_button(ui, &label).clicked() {
                                    let (id, field) = (r.card_id.clone(), f.field.clone());
                                    model.act(move |m| async move {
                                        m.core().resolve_sync_field(&id, &field, take).await?;
                                        Ok(None)
                                    });
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
        });
        ui.add_space(6.0);
    }
}

/// What a key press on the Review tab asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewKey {
    Next,
    Prev,
    Confirm,
    Dismiss,
}

/// The Review tab's keys, unless a text field has focus.
pub fn review_key(ctx: &egui::Context) -> Option<ReviewKey> {
    if ctx.egui_wants_keyboard_input() {
        return None;
    }
    ctx.input(|i| {
        if i.modifiers.any() {
            return None;
        }
        [
            (egui::Key::J, ReviewKey::Next),
            (egui::Key::K, ReviewKey::Prev),
            (egui::Key::C, ReviewKey::Confirm),
            (egui::Key::X, ReviewKey::Dismiss),
        ]
        .into_iter()
        .find(|(k, _)| i.key_pressed(*k))
        .map(|(_, r)| r)
    })
}

/// The cursor after a move key over `n` rows (`None` = no row yet).
pub fn step_cursor(cur: Option<usize>, n: usize, key: ReviewKey) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let c = cur.unwrap_or(0).min(n - 1);
    Some(match (cur, key) {
        (None, _) => 0,
        (_, ReviewKey::Next) => (c + 1).min(n - 1),
        (_, ReviewKey::Prev) => c.saturating_sub(1),
        _ => c,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_and_days() {
        let now = 100 * 86_400_000;
        let mut c = Card {
            status: "backlog".into(),
            updated_at: now - 15 * 86_400_000,
            ..Default::default()
        };
        assert!(is_stale(&c, now));
        c.status = "done".into();
        assert!(!is_stale(&c, now));
        c.status = "todo".into();
        c.updated_at = now - 86_400_000;
        assert!(!is_stale(&c, now));

        let day = 86_400_000;
        let a = Card {
            key: "a".into(),
            updated_at: 10 * day + 3_600_000 * 12,
            ..Default::default()
        };
        let b = Card {
            key: "b".into(),
            updated_at: 10 * day + 3_600_000 * 12 + 60_000,
            ..Default::default()
        };
        let old = Card {
            key: "o".into(),
            updated_at: 3 * day + 3_600_000 * 12,
            ..Default::default()
        };
        let g = by_day(&[&old, &a, &b]);
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[0].1.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(g[1].1[0].key, "o");
        assert!(g[0].0 > g[1].0);
    }

    #[test]
    fn review_cursor() {
        use ReviewKey::*;
        assert_eq!(step_cursor(None, 3, Next), Some(0));
        assert_eq!(step_cursor(Some(0), 3, Next), Some(1));
        assert_eq!(step_cursor(Some(2), 3, Next), Some(2));
        assert_eq!(step_cursor(Some(0), 3, Prev), Some(0));
        assert_eq!(step_cursor(Some(5), 3, Confirm), Some(2));
        assert_eq!(step_cursor(Some(1), 0, Next), None);
    }

    #[test]
    fn review_keys_through_egui() {
        let ctx = egui::Context::default();
        let mut got = None;
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::J,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            got = review_key(ui.ctx());
        });
        out.textures_delta.clear();
        assert_eq!(got, Some(ReviewKey::Next));
    }
}
