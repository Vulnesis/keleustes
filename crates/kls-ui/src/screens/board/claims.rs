//! Board: claims, held cards, notes, send to workflow, and the Triage tab
//! (blocked cards by cause).

use crate::fmt;
use crate::icons;
use crate::kit;
use crate::model::{Model, Notice};
use crate::theme::{self, palette};
use egui::RichText;
use kls_core::board::OWNER;
use kls_core::board::cards::{self, Cause};
use kls_store::board::Card;
use kls_store::engine::Workflow;

/// Buffers of the drawer's claim, decision and note sections.
#[derive(Clone, Debug, Default)]
pub struct ClaimUi {
    pub why: String,
    pub note: String,
    pub workflow: String,
}

/// The default max attempts a triage cause counts against.
const MAX_ATTEMPTS: i64 = 3;

/// Small markers on a card tile: held, claimed (amber when lapsed), blocked.
pub fn tile_marks(ui: &mut egui::Ui, c: &Card) {
    let p = palette(ui.ctx());
    let now = kls_store::now_ms();
    let cl = cards::claim(c);
    let held = cards::checkpoint_reason(c).filter(|_| !cl.is_claimed());
    if held.is_none() && !cl.is_claimed() && !cards::is_blocked(c) {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(r) = held {
            kit::chip(ui, &icons::label(icons::HAND_PALM, &r), p.gold_text)
                .on_hover_text("Waiting for your decision");
        }
        if cl.is_claimed() {
            let lapsed = cl.lapsed(now);
            kit::chip(
                ui,
                &icons::label(icons::LOCK_SIMPLE, &cl.claimed_by),
                if lapsed { p.amber } else { p.muted },
            )
            .on_hover_text(if lapsed {
                format!("Claim lapsed {}", fmt::ago(now, cl.lease_until))
            } else if cl.lease_until > 0 {
                format!("Lease until {}", fmt::when(cl.lease_until))
            } else {
                "Claimed".into()
            });
        }
        if cards::is_blocked(c) {
            kit::chip(ui, &icons::label(icons::PROHIBIT, "Blocked"), p.red);
        }
    });
}

fn act_card(
    model: &Model,
    label: &'static str,
    f: impl FnOnce(kls_core::Core) -> futures_like::Fut + Send + 'static,
) {
    model.act(move |m| async move {
        f(m.core().clone()).await?;
        Ok(Some(Notice::Info(label.into())))
    });
}

/// A boxed future helper so actions read in one line.
mod futures_like {
    pub type Fut = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;
}

macro_rules! card_act {
    ($model:expr, $label:expr, $card:expr, |$core:ident, $id:ident, $v:ident| $body:expr) => {{
        let ($id, $v) = ($card.id.clone(), $card.version);
        act_card($model, $label, move |$core| {
            Box::pin(async move {
                $body.await?;
                Ok(())
            })
        });
    }};
}

/// The drawer's sections: waiting on you, claim, notes, send to workflow.
pub fn drawer_sections(
    ui: &mut egui::Ui,
    model: &Model,
    s: &mut ClaimUi,
    card: &Card,
    workflows: &[Workflow],
) {
    let p = palette(ui.ctx());
    let now = kls_store::now_ms();
    let cl = cards::claim(card);

    if cards::is_held(card) {
        let reason = cards::checkpoint_reason(card).unwrap_or_default();
        kit::glow(ui).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(icons::label(icons::HAND_PALM, "Waiting on you"))
                    .strong()
                    .color(p.gold_text),
            );
            ui.label(kit::mono(&reason, 12.0, p.muted));
            ui.add(
                egui::TextEdit::singleline(&mut s.why)
                    .hint_text("Why (required to reject)")
                    .char_limit(cards::NOTE_MAX)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if theme::primary_button(ui, &icons::label(icons::CHECK, "Approve")).clicked() {
                    let why = std::mem::take(&mut s.why);
                    card_act!(model, "Approved", card, |core, id, v| core
                        .decide_held_card(&id, v, true, &why, OWNER));
                }
                let can_reject = !s.why.trim().is_empty();
                if ui
                    .add_enabled_ui(can_reject, |ui| {
                        kit::danger_button(ui, &icons::label(icons::X, "Reject"))
                    })
                    .inner
                    .clicked()
                {
                    let why = std::mem::take(&mut s.why);
                    card_act!(model, "Rejected", card, |core, id, v| core
                        .decide_held_card(&id, v, false, &why, OWNER));
                }
            });
        });
    }

    if cl.is_claimed() || cl.attempts > 0 || !cl.depends_on.is_empty() || !cl.parent.is_empty() {
        ui.label(kit::caps(
            &icons::label(icons::LOCK_SIMPLE, "Claim"),
            p.muted,
        ));
        egui::Grid::new("card-claim")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).color(p.muted));
                    ui.label(kit::mono(&v, 12.0, p.text));
                    ui.end_row();
                };
                if cl.is_claimed() {
                    row(ui, "By", cl.claimed_by.clone());
                    if !cl.claim_host.is_empty() {
                        row(ui, "Host", cl.claim_host.clone());
                    }
                    if cl.lease_until > 0 {
                        let lease = if cl.lapsed(now) {
                            format!("lapsed {}", fmt::ago(now, cl.lease_until))
                        } else {
                            fmt::when(cl.lease_until)
                        };
                        row(ui, "Lease", lease);
                    }
                }
                if cl.attempts > 0 {
                    row(ui, "Attempts", cl.attempts.to_string());
                }
                if cl.continuations > 0 {
                    row(ui, "Continuations", cl.continuations.to_string());
                }
                if !cl.depends_on.is_empty() {
                    row(ui, "Depends on", cl.depends_on.join(", "));
                }
                if !cl.parent.is_empty() {
                    row(ui, "Parent", cl.parent.clone());
                }
            });
        if cl.is_claimed()
            && kit::ghost_button(ui, &icons::label(icons::LOCK_SIMPLE, "Release claim")).clicked()
        {
            card_act!(model, "Claim released", card, |core, id, v| core
                .release_card_claim(&id, v, OWNER));
        }
    }

    let ledger = cards::conflict_ledger(card);
    if !ledger.attempts.is_empty() || !ledger.final_note.is_empty() {
        ui.label(kit::caps(
            &icons::label(
                icons::GIT_MERGE,
                &format!("Conflict attempts · round {}", ledger.round),
            ),
            p.muted,
        ));
        for a in &ledger.attempts {
            let ok = a.result == "passed";
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::mono(&format!("{}/{}", a.attempt, a.of), 12.0, p.muted));
                ui.label(
                    RichText::new(icons::label(
                        if ok { icons::CHECK } else { icons::X },
                        &a.result,
                    ))
                    .color(if ok { p.green } else { p.red }),
                );
                ui.label(RichText::new(&a.files).size(12.0).color(p.muted));
            });
        }
        if !ledger.final_note.is_empty() {
            ui.label(RichText::new(&ledger.final_note).strong());
        }
        if ledger.earlier > 0 {
            ui.label(RichText::new(format!("{} earlier", ledger.earlier)).color(p.muted));
        }
        if card.labels.iter().any(|l| l == cards::CONFLICTS_UNRESOLVED)
            && theme::primary_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Retry")).clicked()
        {
            card_act!(model, "Retrying", card, |core, id, v| core.retry_card(
                &id,
                v,
                "conflict retry",
                OWNER
            ));
        }
    }

    if cards::is_blocked(card) {
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Retry")).clicked() {
                card_act!(model, "Back to backlog", card, |core, id, v| core
                    .retry_card(&id, v, "", OWNER));
            }
            if kit::ghost_button(ui, &icons::label(icons::CLOCK, "Snooze 7d")).clicked() {
                card_act!(model, "Snoozed", card, |core, id, v| core.snooze_card(
                    &id,
                    v,
                    cards::SNOOZE_DAYS,
                    OWNER
                ));
            }
        });
    } else if card.status != "done"
        && kit::ghost_button(ui, &icons::label(icons::PROHIBIT, "Block")).clicked()
    {
        let note = s.note.trim().to_string();
        card_act!(model, "Blocked", card, |core, id, v| core
            .block_card(&id, v, &note, OWNER));
    }

    ui.label(kit::caps(&icons::label(icons::NOTE, "Notes"), p.muted));
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 40.0).max(80.0);
        ui.add(
            egui::TextEdit::singleline(&mut s.note)
                .hint_text("Add a note")
                .char_limit(cards::NOTE_MAX)
                .desired_width(w),
        );
        if ui
            .add_enabled(
                !s.note.trim().is_empty(),
                egui::Button::new(icons::PAPER_PLANE_TILT),
            )
            .on_hover_text("Add note")
            .clicked()
        {
            let note = std::mem::take(&mut s.note);
            card_act!(model, "Note added", card, |core, id, v| core
                .add_card_note(&id, v, &note, OWNER));
        }
    });
    for h in cards::history(card).iter().rev().take(20) {
        if h.note.trim().is_empty() {
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::mono(&fmt::ago(now, h.at), 11.0, p.muted))
                .on_hover_text(fmt::when(h.at));
            ui.label(RichText::new(&h.note).size(13.0));
        });
    }

    let live: Vec<&Workflow> = workflows
        .iter()
        .filter(|w| w.status == "published" && w.enabled)
        .collect();
    if !live.is_empty() {
        ui.label(kit::caps(
            &icons::label(icons::FLOW_ARROW, "Send to workflow"),
            p.muted,
        ));
        ui.horizontal(|ui| {
            let name = live
                .iter()
                .find(|w| w.id == s.workflow)
                .map(|w| w.name.clone())
                .unwrap_or_else(|| "Workflow".into());
            egui::ComboBox::from_id_salt("card-send-wf")
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for w in &live {
                        ui.selectable_value(&mut s.workflow, w.id.clone(), &w.name);
                    }
                });
            let can = !s.workflow.is_empty() && !card.repo.is_empty();
            if ui
                .add_enabled_ui(can, |ui| {
                    theme::primary_button(ui, &icons::label(icons::PLAY, "Run"))
                })
                .inner
                .on_disabled_hover_text("Pick a workflow; the card needs a repository")
                .clicked()
            {
                let (wf, card_id, repo) = (s.workflow.clone(), card.id.clone(), card.repo.clone());
                model.act(move |m| async move {
                    let out = kls_engine::ops::start_run(
                        m.core(),
                        kls_engine::ops::StartRun {
                            workflow_id: wf,
                            trigger: kls_engine::ops::TriggerInfo {
                                kind: "manual".into(),
                                label: "from the board".into(),
                                ..Default::default()
                            },
                            repo,
                            card_id,
                            ..Default::default()
                        },
                    )
                    .await?;
                    Ok(Some(match out {
                        kls_engine::ops::StartOutcome::Skipped(why) => {
                            Notice::Info(format!("Skipped: {why}"))
                        }
                        _ => Notice::Info("Run queued".into()),
                    }))
                });
            }
        });
    }
}

/// Blocked cards grouped by cause, with a suggested fix and one-click actions.
pub fn triage_tab(ui: &mut egui::Ui, model: &Model, rows: &[&Card], open: &mut Option<Card>) {
    let p = palette(ui.ctx());
    let now = kls_store::now_ms();
    let (snoozed, live): (Vec<&&Card>, Vec<&&Card>) = rows
        .iter()
        .filter(|c| cards::is_blocked(c) || cards::is_held(c))
        .partition(|c| cards::snoozed_until(c).is_some_and(|t| t > now));
    if live.is_empty() {
        kit::empty(ui, icons::CHECK_CIRCLE, "Nothing blocked");
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for cause in Cause::ALL {
            let group: Vec<&&&Card> = live
                .iter()
                .filter(|c| cards::triage_cause(c, now, MAX_ATTEMPTS) == cause)
                .collect();
            if group.is_empty() {
                continue;
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(cause.words()).strong().size(15.0));
                ui.label(kit::mono(&group.len().to_string(), 12.0, p.muted));
                ui.label(RichText::new(icons::label(icons::WRENCH, cause.fix())).color(p.muted));
            });
            kit::card(ui).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for c in group {
                    let c: &Card = c;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(kit::mono(&c.key, 12.0, p.muted));
                        if ui
                            .add(
                                egui::Label::new(RichText::new(&c.title).strong())
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            *open = Some(c.clone());
                        }
                        if let Some(n) = cards::last_note(c) {
                            ui.label(
                                RichText::new(n.note.chars().take(120).collect::<String>())
                                    .color(p.muted),
                            );
                        }
                        ui.label(RichText::new(fmt::ago(now, c.updated_at)).color(p.muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::ghost_button(ui, icons::CLOCK)
                                .on_hover_text("Snooze 7 days")
                                .clicked()
                            {
                                card_act!(model, "Snoozed", c, |core, id, v| core.snooze_card(
                                    &id,
                                    v,
                                    cards::SNOOZE_DAYS,
                                    OWNER
                                ));
                            }
                            if kit::ghost_button(ui, icons::X)
                                .on_hover_text("Dismiss")
                                .clicked()
                            {
                                card_act!(model, "Dismissed", c, |core, id, v| core
                                    .dismiss_card(&id, v, "", OWNER));
                            }
                            if kit::ghost_button(ui, icons::PENCIL_SIMPLE)
                                .on_hover_text("Edit or reassign")
                                .clicked()
                            {
                                *open = Some(c.clone());
                            }
                            if cause == Cause::WaitingOnHuman {
                                if theme::primary_button(
                                    ui,
                                    &icons::label(icons::HAND_PALM, "Decide"),
                                )
                                .clicked()
                                {
                                    *open = Some(c.clone());
                                }
                            } else {
                                if cards::claim(c).is_claimed()
                                    && kit::ghost_button(ui, icons::LOCK_SIMPLE)
                                        .on_hover_text("Release claim")
                                        .clicked()
                                {
                                    card_act!(model, "Claim released", c, |core, id, v| core
                                        .release_card_claim(&id, v, OWNER));
                                }
                                if theme::primary_button(
                                    ui,
                                    &icons::label(icons::ARROW_CLOCKWISE, "Retry"),
                                )
                                .clicked()
                                {
                                    card_act!(model, "Back to backlog", c, |core, id, v| core
                                        .retry_card(&id, v, "", OWNER));
                                }
                            }
                        });
                    });
                }
            });
        }
        if !snoozed.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new(icons::label(
                    icons::CLOCK,
                    &format!("{} snoozed", snoozed.len()),
                ))
                .color(p.muted),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_ui_defaults_empty() {
        let c = ClaimUi::default();
        assert!(c.why.is_empty() && c.note.is_empty() && c.workflow.is_empty());
    }
}
