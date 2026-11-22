//! Checkpoints screen: one queue of what waits on a person, across areas
//! (`Core::needs_you`): engine approvals, gates, steps waiting for a node
//! and paused runs (approve, reject, unblock, bulk actions), sessions with a
//! question, pull requests awaiting review or ready to merge, and cards in
//! review or blocked (opened where they live). The list sits beside an
//! inspector for the selected item. Owner: M3 engine.

use crate::fmt;
use crate::kit;
use crate::loader::Loader;
use crate::model::{Model, Notice};
use crate::{icons, theme};
use egui::RichText;
use kls_core::board::OWNER;
use kls_core::needs::{self, Checkpoint, CheckpointKind};
use kls_engine::ops;
use kls_github::sync::PullAction;
use kls_store::board::Card;
use kls_store::github::GhPull;
use kls_store::now_ms;
use std::collections::HashSet;
use std::sync::Arc;

pub const READY: bool = true;

const KINDS: &[CheckpointKind] = &CheckpointKind::ALL;

pub struct Screen {
    list: Loader<Arc<Vec<Checkpoint>>>,
    kind: Option<CheckpointKind>,
    q: String,
    ticked: HashSet<String>,
    note: String,
    reviewers: String,
    /// Rows shown (50 more each "Show more").
    limit: usize,
    /// Source stamp: when the queue was last read.
    loaded: (usize, i64),
    focus: Option<String>,
    /// The key shown in the inspector.
    sel: Option<String>,
    /// Repository and author filters ("" = any).
    repo_f: String,
    author_f: String,
    /// The selected pull request's facts, files and conflict card.
    pr: Option<(String, Loader<PrView>, Loader<Vec<String>>)>,
    /// The selected card's conflict ledger (card rows).
    card: Option<(String, Loader<Option<Card>>)>,
}

/// What the inspector shows for a pull request.
#[derive(Clone, Debug, Default)]
pub struct PrView {
    pub pull: Option<GhPull>,
    /// The open merge-conflict card of the pull request.
    pub conflict_card: Option<Card>,
    pub merge_method: String,
}

/// A pull request row (bulk PR actions apply).
fn is_pull(c: &Checkpoint) -> bool {
    c.key.starts_with("pull:") && c.number.is_some()
}

/// The Events filter that shows what a row is about.
pub(crate) fn events_filter(c: &Checkpoint) -> Option<String> {
    if !c.run_id.is_empty() {
        Some(format!("run:{}", c.run_id))
    } else if !c.card_id.is_empty() {
        Some(format!("card:{}", c.card_id))
    } else if let Some(s) = &c.session_id {
        Some(format!("session:{s}"))
    } else if !c.node_id.is_empty() {
        Some(format!("node:{}", c.node_id))
    } else if !c.repo.is_empty() {
        Some(format!("repo:{}", c.repo))
    } else {
        None
    }
}

/// Labelled pull request facts for the inspector.
fn pull_facts(p: &GhPull) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("Author", format!("@{}", p.author)),
        ("Checks", or_dash(&p.checks_state)),
        ("Mergeable", or_dash(&p.mergeable)),
        (
            "Review",
            or_dash(&p.review_decision.to_lowercase().replace('_', " ")),
        ),
        (
            "Branch",
            format!("{} ← {}", or_dash(&p.base_ref), or_dash(&p.head_ref)),
        ),
        (
            "Size",
            format!(
                "+{} −{} · {} files",
                p.additions, p.deletions, p.changed_files
            ),
        ),
    ];
    if !p.requested_reviewers.is_empty() {
        out.push(("Reviewers", p.requested_reviewers.join(", ")));
    }
    if p.is_fork() {
        out.push(("Fork", p.head_repo.clone()));
    }
    out
}

fn or_dash(s: &str) -> String {
    if s.is_empty() { "—".into() } else { s.into() }
}

/// Logins or `org/team` from a free-text list (1 to 15 are sent).
fn reviewer_list(s: &str) -> Vec<String> {
    s.split([',', ' '])
        .map(|s| s.trim().trim_start_matches('@').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// An (icon, label) button face.
type Face = (&'static str, &'static str);

/// The two actions a decidable row offers.
fn actions(k: CheckpointKind) -> Option<(Face, Face)> {
    match k {
        CheckpointKind::Approval => Some(((icons::CHECK, "Approve"), (icons::X, "Reject"))),
        CheckpointKind::Gate => Some(((icons::CHECK, "Pass"), (icons::X, "Fail"))),
        CheckpointKind::DeployGate => Some(((icons::ROCKET, "Pass"), (icons::X, "Fail"))),
        CheckpointKind::NoNode => Some((
            (icons::ARROW_CLOCKWISE, "Retry"),
            (icons::PROHIBIT, "Give up"),
        )),
        CheckpointKind::Paused => Some(((icons::PLAY, "Resume"), (icons::STOP, "Cancel"))),
        _ => None,
    }
}

/// The icon a checkpoint kind shows in its chip.
pub(crate) fn kind_icon(k: CheckpointKind) -> &'static str {
    match k {
        CheckpointKind::Approval => icons::SEAL_CHECK,
        CheckpointKind::Gate => icons::HAND_PALM,
        CheckpointKind::DeployGate => icons::ROCKET,
        CheckpointKind::NoNode => icons::WARNING,
        CheckpointKind::Paused => icons::PAUSE,
        CheckpointKind::Question => icons::CHAT_CIRCLE_DOTS,
        CheckpointKind::PullReady => icons::GIT_MERGE,
        CheckpointKind::PullReview => icons::GIT_PULL_REQUEST,
        CheckpointKind::PullDraft => icons::PENCIL_SIMPLE,
        CheckpointKind::IssueExternal => icons::CHAT_TEXT,
        CheckpointKind::CardBlocked => icons::PROHIBIT,
        CheckpointKind::CardReview => icons::EYE,
        CheckpointKind::CardHeld => icons::HAND_PALM,
        CheckpointKind::ClaimLapsed => icons::LOCK_SIMPLE,
        CheckpointKind::HostHandoff => icons::TERMINAL,
        CheckpointKind::HostUpdate => icons::HARD_DRIVES,
        CheckpointKind::NodeOffline => icons::PLUGS_CONNECTED,
        CheckpointKind::QuotaEscalated => icons::GAUGE,
    }
}

/// The colour of a checkpoint kind's chip.
pub(crate) fn kind_color(k: CheckpointKind, pal: &theme::Palette) -> egui::Color32 {
    match k {
        CheckpointKind::Approval => pal.gold_text,
        CheckpointKind::Gate => pal.amber,
        CheckpointKind::DeployGate => pal.amber,
        CheckpointKind::NoNode => pal.red,
        CheckpointKind::Paused => pal.muted,
        CheckpointKind::Question => pal.cyan,
        CheckpointKind::PullReady => pal.green,
        CheckpointKind::PullReview => pal.blue,
        CheckpointKind::PullDraft => pal.muted,
        CheckpointKind::IssueExternal => pal.cyan,
        CheckpointKind::CardBlocked => pal.red,
        CheckpointKind::CardReview => pal.violet,
        CheckpointKind::CardHeld => pal.gold_text,
        CheckpointKind::ClaimLapsed => pal.amber,
        CheckpointKind::HostHandoff => pal.amber,
        CheckpointKind::HostUpdate => pal.cyan,
        CheckpointKind::NodeOffline => pal.red,
        CheckpointKind::QuotaEscalated => pal.amber,
    }
}

/// "12m", "3h", "2d" (an age without "ago").
fn age(now: i64, ts: i64) -> String {
    fmt::ago(now, ts).trim_end_matches(" ago").to_string()
}

/// The approval's message, if its context has one.
fn message(c: &Checkpoint) -> Option<String> {
    c.approval
        .as_ref()
        .and_then(|a| serde_json::from_str::<serde_json::Value>(&a.context_json).ok())
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .filter(|m| !m.is_empty())
}

/// A budget approval (`escalate_human`): approve re-runs the step past the
/// gate, reject fails it `budget_rejected`.
fn is_budget(c: &Checkpoint) -> bool {
    c.kind == CheckpointKind::Approval && c.approval.as_ref().is_some_and(|a| a.kind == "budget")
}

/// A button face and its hover text ("" for none).
type Button = (Face, &'static str);

/// The decide buttons of a checkpoint, with their hover text.
fn decide_faces(c: &Checkpoint) -> Option<(Button, Button)> {
    let (yes, no) = actions(c.kind)?;
    if is_budget(c) {
        return Some((
            ((icons::PLAY, "Approve"), "Run the step past the budget"),
            ((icons::X, "Reject"), "Fail the step: budget_rejected"),
        ));
    }
    Some(((yes, ""), (no, "")))
}

/// The reason shown for a checkpoint (`budget` reads as a word).
fn reason_text(c: &Checkpoint) -> String {
    if is_budget(c) {
        return icons::label(icons::WALLET, "Budget");
    }
    c.reason.clone()
}

/// The muted line under a row's title.
fn detail_line(c: &Checkpoint) -> String {
    let reason = reason_text(c);
    [c.workflow.as_str(), c.repo.as_str(), reason.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Labelled facts for the inspector (only those that are set).
fn facts(c: &Checkpoint, now: i64) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if !c.workflow.is_empty() {
        out.push(("Workflow", c.workflow.clone()));
    }
    if !c.run_id.is_empty() {
        out.push(("Run", c.run_id.clone()));
    }
    if let Some(s) = &c.step_id {
        out.push(("Step", s.clone()));
    }
    if !c.repo.is_empty() {
        out.push((
            "Repository",
            match c.number {
                Some(n) => format!("{}#{n}", c.repo),
                None => c.repo.clone(),
            },
        ));
    }
    if let Some(sla) = c.sla_at {
        out.push((
            "Due",
            if sla < now {
                "overdue".into()
            } else {
                fmt::when(sla)
            },
        ));
    }
    if let Some(a) = &c.approval
        && !a.node_id.is_empty()
    {
        out.push(("Node", a.node_id.clone()));
    }
    out.push(("Waiting", age(now, c.ts)));
    out
}

/// A merge-conflict card's attempt ledger, with Retry and Open card.
fn ledger_ui(ui: &mut egui::Ui, model: &Model, card: &Card) {
    use kls_core::board::cards;
    let pal = theme::palette(ui.ctx());
    let l = cards::conflict_ledger(card);
    let is_conflict = card.labels.iter().any(|x| x == cards::CONFLICT_LABEL);
    if !is_conflict && l.attempts.is_empty() && l.final_note.is_empty() {
        return;
    }
    kit::card(ui).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(kit::caps(
                &icons::label(
                    icons::GIT_MERGE,
                    &format!("Conflict ledger · round {}", l.round),
                ),
                pal.muted,
            ));
            ui.label(kit::mono(&card.key, 12.0, pal.muted));
        });
        if l.attempts.is_empty() {
            ui.label(RichText::new("No attempts yet").color(pal.muted));
        }
        for a in &l.attempts {
            let ok = a.result == "passed";
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::mono(
                    &format!("{}/{}", a.attempt, a.of),
                    12.0,
                    pal.muted,
                ));
                ui.label(
                    RichText::new(icons::label(
                        if ok { icons::CHECK } else { icons::X },
                        &a.result,
                    ))
                    .color(if ok { pal.green } else { pal.red }),
                );
                ui.label(kit::mono(&a.fingerprint, 11.0, pal.muted));
                ui.label(RichText::new(&a.files).size(12.0).color(pal.muted));
            });
        }
        if !l.final_note.is_empty() {
            ui.label(RichText::new(&l.final_note).strong());
        }
        if l.earlier > 0 {
            ui.label(RichText::new(format!("{} earlier", l.earlier)).color(pal.muted));
        }
        ui.horizontal_wrapped(|ui| {
            let stuck = card.labels.iter().any(|x| x == cards::CONFLICTS_UNRESOLVED)
                || cards::is_blocked(card);
            if stuck
                && theme::primary_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Retry"))
                    .on_hover_text("A new round of attempts")
                    .clicked()
            {
                let (id, v) = (card.id.clone(), card.version);
                model.act(move |m| async move {
                    m.core().retry_card(&id, v, "conflict retry", OWNER).await?;
                    Ok(Some(Notice::Info("Retrying".into())))
                });
            }
            if kit::ghost_button(ui, &icons::label(icons::KANBAN, "Open card")).clicked() {
                model.go(crate::Screen::Board, Some(card.id.clone()));
            }
        });
    });
}

/// A filter combo: "any" (empty) or one of `options`.
fn pick(
    ui: &mut egui::Ui,
    id: &str,
    icon: &str,
    any: &str,
    value: &mut String,
    options: &[String],
) {
    if options.is_empty() && value.is_empty() {
        return;
    }
    let text = if value.is_empty() {
        any
    } else {
        value.as_str()
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(icons::label(icon, text))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, String::new(), any);
            for o in options {
                ui.selectable_value(value, o.clone(), o);
            }
        });
}

fn decide(model: &Model, key: &str, approve: bool, note: &str) {
    let (key, note) = (key.to_string(), note.to_string());
    model.act(move |m| async move {
        let e = ops::bulk_decide(m.core(), &[key], approve, &note).await;
        match e.into_iter().next() {
            Some(err) => Ok(Some(Notice::Error(err))),
            None => Ok(None),
        }
    });
}

impl Screen {
    pub fn new(model: &Model) -> Self {
        let list = Loader::new(model, &needs::RELOAD_ON, |core| async move {
            Ok(Arc::new(core.needs_you().await?))
        });
        Self {
            list,
            kind: None,
            q: String::new(),
            ticked: HashSet::new(),
            note: String::new(),
            reviewers: String::new(),
            limit: 50,
            loaded: (0, 0),
            focus: None,
            sel: None,
            repo_f: String::new(),
            author_f: String::new(),
            pr: None,
            card: None,
        }
    }

    /// Focuses a checkpoint key, a run id or an approval id.
    /// `kind:<snake_case kind>` filters the list to that kind instead.
    pub fn focus(&mut self, id: &str) {
        if let Some(k) = id.strip_prefix("kind:") {
            self.kind = KINDS.iter().copied().find(|x| x.key() == k);
            self.focus = None;
            return;
        }
        self.focus = Some(id.to_string());
        self.kind = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, model: &Model) {
        let pal = theme::palette(ui.ctx());
        let items = match self.list.get() {
            Some(Ok(i)) => i,
            Some(Err(e)) => {
                kit::page_header(ui, icons::SEAL_CHECK, "Checkpoints", |_| {});
                ui.colored_label(pal.red, e);
                return;
            }
            None => {
                kit::page_header(ui, icons::SEAL_CHECK, "Checkpoints", |_| {});
                ui.spinner();
                return;
            }
        };
        let ptr = Arc::as_ptr(&items) as usize;
        if ptr != self.loaded.0 {
            self.loaded = (ptr, now_ms());
        }
        let q = self.q.to_lowercase();
        let shown: Vec<&Checkpoint> = items
            .iter()
            .filter(|c| self.kind.is_none_or(|k| c.kind == k))
            .filter(|c| self.repo_f.is_empty() || c.repo == self.repo_f)
            .filter(|c| self.author_f.is_empty() || c.author == self.author_f)
            .filter(|c| {
                q.is_empty()
                    || c.title.to_lowercase().contains(&q)
                    || c.workflow.to_lowercase().contains(&q)
                    || c.repo.to_lowercase().contains(&q)
                    || c.author.to_lowercase().contains(&q)
                    || c.run_id.contains(&q)
            })
            .collect();
        let keys: HashSet<&str> = items.iter().map(|c| c.key.as_str()).collect();
        self.ticked.retain(|k| keys.contains(k.as_str()));

        // A focus request selects the item it names.
        let focused_key = self.focus.as_deref().and_then(|f| {
            items
                .iter()
                .find(|c| {
                    f == c.key || f == c.run_id || c.approval.as_ref().is_some_and(|a| a.id == f)
                })
                .map(|c| c.key.clone())
        });
        if let Some(k) = &focused_key {
            self.sel = Some(k.clone());
        }
        if self
            .sel
            .as_ref()
            .is_none_or(|s| !shown.iter().any(|c| &c.key == s))
        {
            self.sel = shown.first().map(|c| c.key.clone());
        }

        // Title and bulk actions.
        kit::page_header(ui, icons::SEAL_CHECK, "Checkpoints", |ui| {
            let n = self.ticked.len();
            if n > 0 {
                if kit::ghost_button(ui, &icons::label(icons::X, "Clear")).clicked() {
                    self.ticked.clear();
                }
                let pulls: Vec<(String, i64)> = items
                    .iter()
                    .filter(|c| self.ticked.contains(&c.key) && is_pull(c))
                    .filter_map(|c| c.number.map(|n| (c.repo.clone(), n)))
                    .collect();
                let decide_keys: Vec<String> = items
                    .iter()
                    .filter(|c| self.ticked.contains(&c.key) && c.decidable())
                    .map(|c| c.key.clone())
                    .collect();
                if !pulls.is_empty() {
                    let np = pulls.len();
                    for (icon, label, action) in [
                        (
                            icons::ARROWS_CLOCKWISE,
                            "Update branch",
                            PullAction::UpdateBranch,
                        ),
                        (icons::PENCIL_SIMPLE, "Draft", PullAction::Draft),
                        (icons::EYE, "Ready", PullAction::Ready),
                    ] {
                        if kit::ghost_button(ui, &icons::label(icon, &format!("{label} ({np})")))
                            .clicked()
                        {
                            let targets = pulls.clone();
                            model.act(move |m| async move {
                                let res = kls_github::service(m.core())
                                    .bulk_pull_action(&targets, action)
                                    .await?;
                                let failed: Vec<String> = res
                                    .iter()
                                    .filter(|r| !r.ok)
                                    .map(|r| format!("{}#{}: {}", r.repo, r.number, r.error))
                                    .collect();
                                Ok(Some(if failed.is_empty() {
                                    Notice::Info(format!("{} done", res.len()))
                                } else {
                                    Notice::Error(failed.join("\n"))
                                }))
                            });
                            self.ticked.clear();
                        }
                    }
                    // Request reviewers on every ticked pull request.
                    let who = reviewer_list(&self.reviewers);
                    if ui
                        .add_enabled_ui((1..=15).contains(&who.len()), |ui| {
                            kit::ghost_button(
                                ui,
                                &icons::label(icons::USER_PLUS, &format!("Request ({np})")),
                            )
                        })
                        .inner
                        .on_hover_text("Request reviewers (1 to 15) on the ticked pull requests")
                        .clicked()
                    {
                        let targets = pulls.clone();
                        self.reviewers.clear();
                        self.ticked.clear();
                        model.act(move |m| async move {
                            let res = kls_github::service(m.core())
                                .bulk_pull_action(&targets, PullAction::RequestReviewers(who))
                                .await?;
                            let failed: Vec<String> = res
                                .iter()
                                .filter(|r| !r.ok)
                                .map(|r| format!("{}#{}: {}", r.repo, r.number, r.error))
                                .collect();
                            Ok(Some(if failed.is_empty() {
                                Notice::Info(format!("Reviewers requested on {}", res.len()))
                            } else {
                                Notice::Error(failed.join("\n"))
                            }))
                        });
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.reviewers)
                            .hint_text("login, org/team")
                            .desired_width(140.0),
                    );
                }
                let nd = decide_keys.len();
                if nd > 0 {
                    for (icon, label, approve) in
                        [(icons::X, "Reject", false), (icons::CHECK, "Approve", true)]
                    {
                        let text = icons::label(icon, &format!("{label} ({nd})"));
                        let r = if approve {
                            theme::primary_button(ui, &text)
                        } else {
                            kit::danger_button(ui, &text)
                        };
                        if r.on_hover_text(format!("{label} ticked")).clicked() {
                            let keys = decide_keys.clone();
                            self.ticked.clear();
                            let note = self.note.clone();
                            model.act(move |m| async move {
                                let errors =
                                    ops::bulk_decide(m.core(), &keys, approve, &note).await;
                                if errors.is_empty() {
                                    Ok(Some(Notice::Info(format!("{} done", keys.len()))))
                                } else {
                                    Ok(Some(Notice::Error(errors.join("\n"))))
                                }
                            });
                        }
                    }
                }
            } else if shown.iter().any(|c| c.decidable() || is_pull(c))
                && kit::ghost_button(ui, &icons::label(icons::CHECKS, "Select all")).clicked()
            {
                self.ticked = shown
                    .iter()
                    .filter(|c| c.decidable() || is_pull(c))
                    .map(|c| c.key.clone())
                    .collect();
            }
        });

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::label(
                    icons::BROADCAST,
                    &format!("live · read {}", fmt::ago(now_ms(), self.loaded.1)),
                ))
                .size(12.0)
                .color(pal.muted),
            )
            .on_hover_text(
                "Local store; reloads on card, run, approval, session and GitHub events",
            );
            if ui
                .small_button(icons::ARROWS_CLOCKWISE)
                .on_hover_text("Read again")
                .clicked()
            {
                self.list.reload();
            }
        });
        // Kind filters and search.
        ui.horizontal_wrapped(|ui| {
            if kit::toggle_chip(ui, &format!("All {}", items.len()), self.kind.is_none()).clicked()
            {
                self.kind = None;
            }
            for &k in KINDS {
                let n = items.iter().filter(|c| c.kind == k).count();
                if n > 0
                    && kit::toggle_chip(
                        ui,
                        &format!("{}  {} {n}", kind_icon(k), k.words()),
                        self.kind == Some(k),
                    )
                    .clicked()
                {
                    self.kind = Some(k);
                }
            }
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.q)
                    .hint_text(icons::label(icons::MAGNIFYING_GLASS, "Search"))
                    .desired_width(200.0),
            );
            let mut repos: Vec<String> = items.iter().map(|c| c.repo.clone()).collect();
            let mut authors: Vec<String> = items.iter().map(|c| c.author.clone()).collect();
            for v in [&mut repos, &mut authors] {
                v.retain(|s| !s.is_empty());
                v.sort();
                v.dedup();
            }
            pick(
                ui,
                "cp-repo",
                icons::GIT_BRANCH,
                "Any repo",
                &mut self.repo_f,
                &repos,
            );
            pick(
                ui,
                "cp-author",
                icons::USER,
                "Anyone",
                &mut self.author_f,
                &authors,
            );
        });
        ui.add_space(14.0);

        if shown.is_empty() {
            kit::card(ui)
                .inner_margin(egui::Margin::same(24))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(icons::label(icons::CHECK_CIRCLE, "All clear"))
                                .size(15.0)
                                .color(pal.muted),
                        );
                    });
                });
            self.focus = None;
            return;
        }
        let now = now_ms();
        let selected = shown
            .iter()
            .find(|c| self.sel.as_ref() == Some(&c.key))
            .copied();
        // Side by side, or stacked (list capped, page scrolls) when narrow.
        let stacked = ui.available_width() < 560.0 + 360.0 + 20.0;
        let list_h = if stacked {
            320.0
        } else {
            ui.available_height().max(320.0)
        };
        let mut both = |ui: &mut egui::Ui| {
            kit::Split::new(560.0, 360.0, kit::Widths::Flex)
                .gap(20.0)
                .fit_height()
                .show(ui, |ui, side| {
                    if side == 0 {
                        self.list_ui(ui, &shown, focused_key.as_deref(), now, list_h);
                    } else if let Some(c) = selected {
                        self.inspector(ui, model, c, now);
                    }
                })
        };
        if stacked {
            egui::ScrollArea::vertical()
                .id_salt("checkpoints-page")
                .show(ui, both);
        } else {
            both(ui);
        }
    }

    fn list_ui(
        &mut self,
        ui: &mut egui::Ui,
        shown: &[&Checkpoint],
        focused: Option<&str>,
        now: i64,
        list_h: f32,
    ) {
        let pal = theme::palette(ui.ctx());
        kit::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("checkpoints-list")
                .max_height(list_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let total = shown.len();
                    for c in shown.iter().copied().take(self.limit) {
                        let on = self.sel.as_ref() == Some(&c.key);
                        let (row, _) = kit::list_row(ui, ("checkpoint", &c.key), on, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                if c.decidable() || is_pull(c) {
                                    let mut t = self.ticked.contains(&c.key);
                                    if ui.checkbox(&mut t, "").changed() {
                                        if t {
                                            self.ticked.insert(c.key.clone());
                                        } else {
                                            self.ticked.remove(&c.key);
                                        }
                                    }
                                }
                                kit::chip(
                                    ui,
                                    &format!(
                                        "{} {}",
                                        kind_icon(c.kind),
                                        c.kind.words().to_uppercase()
                                    ),
                                    kind_color(c.kind, &pal),
                                );
                                let right = 90.0;
                                let w = (ui.available_width() - right).max(140.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(w, 0.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&c.title).size(14.0).strong(),
                                            )
                                            .truncate(),
                                        );
                                        let d = detail_line(c);
                                        if !d.is_empty() {
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(d).size(12.0).color(pal.muted),
                                                )
                                                .truncate(),
                                            );
                                        }
                                    },
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.add_space(8.0);
                                        ui.label(kit::mono(&age(now, c.ts), 12.0, pal.muted));
                                        if c.sla_at.is_some_and(|s| s < now) {
                                            ui.label(RichText::new(icons::WARNING).color(pal.red))
                                                .on_hover_text("Overdue");
                                        }
                                    },
                                );
                            });
                        });
                        if row.clicked() {
                            self.sel = Some(c.key.clone());
                        }
                        if focused == Some(c.key.as_str()) && self.focus.is_some() {
                            row.scroll_to_me(Some(egui::Align::Center));
                            self.focus = None;
                        }
                    }
                    if total > self.limit
                        && kit::ghost_button(
                            ui,
                            &icons::label(
                                icons::CARET_DOWN,
                                &format!("Show more ({})", total - self.limit),
                            ),
                        )
                        .clicked()
                    {
                        self.limit += 50;
                    }
                });
        });
    }

    /// Loads what the inspector shows for `c` (once per selection).
    fn load_detail(&mut self, model: &Model, c: &Checkpoint) {
        match (is_pull(c), c.number) {
            (true, Some(n)) if self.pr.as_ref().is_none_or(|(k, _, _)| *k != c.key) => {
                let repo = c.repo.clone();
                let r2 = repo.clone();
                let view = Loader::new(
                    model,
                    &["github.pull.", "card.", "github.repo."],
                    move |core| {
                        let repo = repo.clone();
                        async move {
                            Ok(PrView {
                                pull: core.store().gh_pull(&repo, n).await?,
                                conflict_card: core.conflict_card_of(&repo, n).await?,
                                merge_method: core
                                    .store()
                                    .gh_repo(&repo)
                                    .await?
                                    .map(|r| r.settings.merge_method)
                                    .unwrap_or_default(),
                            })
                        }
                    },
                );
                // The file list is one GitHub read per selection.
                let files = Loader::new(model, &[], move |core| {
                    let repo = r2.clone();
                    async move {
                        let gh = kls_github::service(&core);
                        let cl = gh.client_for(&repo).await?;
                        cl.api().pull_files(&cl, &repo, n, "user").await
                    }
                });
                self.pr = Some((c.key.clone(), view, files));
            }
            (true, _) => {}
            _ => self.pr = None,
        }
        if c.card_id.is_empty() {
            self.card = None;
        } else if self.card.as_ref().is_none_or(|(k, _)| *k != c.card_id) {
            let id = c.card_id.clone();
            let l = Loader::new(model, &["card."], move |core| {
                let id = id.clone();
                async move { core.card(&id).await }
            });
            self.card = Some((c.card_id.clone(), l));
        }
    }

    /// Pull request facts, its files and the merge-conflict ledger.
    fn pull_detail(&mut self, ui: &mut egui::Ui, model: &Model, c: &Checkpoint) {
        let pal = theme::palette(ui.ctx());
        let Some((_, view, files)) = &self.pr else {
            return;
        };
        let Some(Ok(v)) = view.get() else {
            ui.spinner();
            return;
        };
        if let Some(p) = &v.pull {
            let mut facts = pull_facts(p);
            facts.push((
                "Merge method",
                if v.merge_method.is_empty() {
                    "squash".into()
                } else {
                    v.merge_method.clone()
                },
            ));
            kit::card(ui).show(ui, |ui| {
                kit::cell_grid(ui, facts.len(), 140.0, 2, |ui, i| {
                    let (k, val) = &facts[i];
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(kit::caps(k, pal.muted));
                    ui.add(egui::Label::new(RichText::new(val).size(13.0).strong()).truncate())
                        .on_hover_text(val);
                });
            });
        }
        egui::CollapsingHeader::new(
            RichText::new(icons::label(icons::FILE_TEXT, "Files")).strong(),
        )
        .id_salt(("cp-files", &c.key))
        .default_open(false)
        .show(ui, |ui| match files.get() {
            None => {
                ui.spinner();
            }
            Some(Err(e)) => {
                ui.colored_label(pal.red, e);
            }
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("No files").color(pal.muted));
            }
            Some(Ok(list)) => {
                egui::ScrollArea::vertical()
                    .id_salt(("cp-files-list", &c.key))
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for f in &list {
                            ui.label(kit::mono(f, 12.0, pal.text));
                        }
                    });
            }
        });
        let conflicted = c.conflict || v.pull.as_ref().is_some_and(|p| p.mergeable == "conflict");
        if let Some(card) = &v.conflict_card {
            ledger_ui(ui, model, card);
        } else if conflicted
            && theme::primary_button(ui, &icons::label(icons::GIT_MERGE, "Resolve conflicts"))
                .on_hover_text("File a merge-conflict card for the conflict resolver")
                .clicked()
            && let Some(n) = c.number
        {
            let (repo, url) = (c.repo.clone(), c.url.clone());
            let pull = v.pull.clone().unwrap_or_default();
            model.act(move |m| async move {
                let data = serde_json::json!({
                    "repo": repo, "number": n, "url": url, "head": pull.head_ref,
                    "headSha": pull.head_sha, "base": pull.base_ref, "fork": pull.is_fork(),
                });
                let body = kls_engine::conflicts::card_body(&data);
                let (card, new) = m
                    .core()
                    .file_conflict_card(&repo, n, &body, pull.is_fork(), OWNER)
                    .await?;
                Ok(Some(Notice::Info(if new {
                    format!("Filed {}", card.key)
                } else {
                    format!("{} is open already", card.key)
                })))
            });
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui, model: &Model, c: &Checkpoint, now: i64) {
        self.load_detail(model, c);
        let pal = theme::palette(ui.ctx());
        kit::glow(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(kit::mono(
                        &format!(
                            "{}  {} · {}",
                            kind_icon(c.kind),
                            c.kind.words(),
                            age(now, c.ts)
                        ),
                        12.0,
                        pal.gold_text,
                    ));
                });
            let r = ui.min_rect();
            ui.painter()
                .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, pal.line));
            egui::Frame::new()
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.add(egui::Label::new(RichText::new(&c.title).size(20.0).strong()).wrap());
                    let mut lines: Vec<String> = Vec::new();
                    if !c.reason.is_empty() {
                        lines.push(reason_text(c));
                    }
                    if let Some(m) = message(c) {
                        lines.push(m);
                    }
                    for l in lines {
                        ui.label(RichText::new(l).size(14.0).color(pal.muted));
                    }
                    let facts = facts(c, now);
                    kit::card(ui).show(ui, |ui| {
                        kit::cell_grid(ui, facts.len(), 140.0, 2, |ui, i| {
                            let (k, v) = &facts[i];
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(kit::caps(k, pal.muted));
                            let color = if *v == "overdue" { pal.red } else { pal.text };
                            ui.add(
                                egui::Label::new(RichText::new(v).size(13.0).strong().color(color))
                                    .truncate(),
                            )
                            .on_hover_text(v);
                        });
                    });
                    if is_pull(c) {
                        self.pull_detail(ui, model, c);
                    } else if let Some((_, l)) = &self.card
                        && let Some(Ok(Some(card))) = l.get()
                    {
                        ledger_ui(ui, model, &card);
                    }
                    if c.decidable() {
                        kit::field(ui, "Note", |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.note)
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY),
                            )
                            .on_hover_text("Optional note recorded with the decision");
                        });
                    }
                    ui.horizontal_wrapped(|ui| self.inspector_actions(ui, model, c));
                });
        });
    }

    fn inspector_actions(&mut self, ui: &mut egui::Ui, model: &Model, c: &Checkpoint) {
        if c.decidable()
            && let Some((((yi, yes), yh), ((ni, no), nh))) = decide_faces(c)
        {
            let y = theme::primary_button(ui, &icons::label(yi, yes));
            let y = if yh.is_empty() {
                y
            } else {
                y.on_hover_text(yh)
            };
            if y.clicked() {
                decide(model, &c.key, true, &self.note);
            }
            let n = kit::danger_button(ui, &icons::label(ni, no));
            let n = if nh.is_empty() {
                n
            } else {
                n.on_hover_text(nh)
            };
            if n.clicked() {
                decide(model, &c.key, false, &self.note);
            }
        }
        if is_pull(c)
            && let Some(n) = c.number
        {
            let repo = c.repo.clone();
            let pr = |label: &'static str, action: PullAction| {
                let repo = repo.clone();
                model.act(move |m| async move {
                    kls_github::service(m.core())
                        .pull_action(&repo, n, action)
                        .await?;
                    Ok(Some(Notice::Info(format!("{repo}#{n}: {label}"))))
                });
            };
            if c.kind == CheckpointKind::PullReady
                && theme::primary_button(ui, &icons::label(icons::GIT_MERGE, "Merge"))
                    .on_hover_text("With the repository's merge method")
                    .clicked()
            {
                let repo = repo.clone();
                model.act(move |m| async move {
                    let method = m
                        .core()
                        .store()
                        .gh_repo(&repo)
                        .await?
                        .map(|r| r.settings.merge_method)
                        .filter(|x| !x.is_empty())
                        .unwrap_or_else(|| "squash".into());
                    kls_github::service(m.core())
                        .pull_action(&repo, n, PullAction::Merge(method.clone()))
                        .await?;
                    Ok(Some(Notice::Info(format!("{repo}#{n} merged ({method})"))))
                });
            }
            if kit::ghost_button(ui, &icons::label(icons::EYE, "Ready")).clicked() {
                pr("ready", PullAction::Ready);
            }
            if kit::ghost_button(ui, &icons::label(icons::PENCIL_SIMPLE, "Draft")).clicked() {
                pr("draft", PullAction::Draft);
            }
            if kit::ghost_button(ui, &icons::label(icons::ARROWS_CLOCKWISE, "Update branch"))
                .clicked()
            {
                pr("branch updated", PullAction::UpdateBranch);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.reviewers)
                    .hint_text("login, org/team")
                    .desired_width(150.0),
            );
            let who: Vec<String> = self
                .reviewers
                .split([',', ' '])
                .map(|s| s.trim().trim_start_matches('@').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if ui
                .add_enabled_ui((1..=15).contains(&who.len()), |ui| {
                    kit::ghost_button(ui, &icons::label(icons::USER_PLUS, "Request"))
                })
                .inner
                .on_hover_text("Request reviewers (1 to 15)")
                .clicked()
            {
                self.reviewers.clear();
                pr("reviewers requested", PullAction::RequestReviewers(who));
            }
        }
        let card_id = c.card_id.clone();
        let card_act = |label: &'static str, kind: u8, note: String| {
            let id = card_id.clone();
            model.act(move |m| async move {
                let core = m.core();
                let card = core
                    .card(&id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the card is gone"))?;
                match kind {
                    0 => {
                        core.decide_held_card(&card.id, card.version, true, &note, OWNER)
                            .await?
                    }
                    1 => {
                        core.decide_held_card(&card.id, card.version, false, &note, OWNER)
                            .await?
                    }
                    2 => {
                        core.release_card_claim(&card.id, card.version, OWNER)
                            .await?
                    }
                    _ => {
                        core.retry_card(&card.id, card.version, &note, OWNER)
                            .await?
                    }
                };
                Ok(Some(Notice::Info(label.into())))
            });
        };
        match c.kind {
            CheckpointKind::CardHeld => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.note)
                        .hint_text("Why (required to reject)")
                        .desired_width(200.0),
                );
                if theme::primary_button(ui, &icons::label(icons::CHECK, "Approve")).clicked() {
                    card_act("Approved", 0, std::mem::take(&mut self.note));
                }
                if ui
                    .add_enabled_ui(!self.note.trim().is_empty(), |ui| {
                        kit::danger_button(ui, &icons::label(icons::X, "Reject"))
                    })
                    .inner
                    .clicked()
                {
                    card_act("Rejected", 1, std::mem::take(&mut self.note));
                }
            }
            CheckpointKind::ClaimLapsed
                if theme::primary_button(
                    ui,
                    &icons::label(icons::LOCK_SIMPLE, "Release claim"),
                )
                .clicked() =>
            {
                card_act("Claim released", 2, String::new());
            }
            CheckpointKind::CardBlocked
                if theme::primary_button(ui, &icons::label(icons::ARROW_CLOCKWISE, "Unblock"))
                    .on_hover_text("Back to the backlog")
                    .clicked() =>
            {
                card_act("Back to backlog", 3, std::mem::take(&mut self.note));
            }
            CheckpointKind::HostHandoff
            | CheckpointKind::HostUpdate
            | CheckpointKind::NodeOffline => {
                if c.kind == CheckpointKind::HostHandoff
                    && !c.reason.is_empty()
                    && kit::ghost_button(ui, &icons::label(icons::COPY, "Copy command")).clicked()
                {
                    ui.ctx().copy_text(c.reason.clone());
                }
                if theme::primary_button(ui, &icons::label(icons::HARD_DRIVES, "Open host"))
                    .clicked()
                {
                    let (screen, focus) = crate::screens::cockpit::target(c);
                    model.go(screen, focus);
                }
            }
            CheckpointKind::QuotaEscalated
                if theme::primary_button(ui, &icons::label(icons::GAUGE, "Usage")).clicked() =>
            {
                let (screen, focus) = crate::screens::cockpit::target(c);
                model.go(screen, focus);
            }
            _ => {}
        }
        if let Some(sid) = &c.session_id {
            let (label, primary) = if c.kind == CheckpointKind::Question {
                (icons::label(icons::CHAT_CIRCLE_DOTS, "Answer"), true)
            } else {
                (icons::label(icons::TERMINAL_WINDOW, "Open session"), false)
            };
            let r = if primary {
                theme::primary_button(ui, &label)
            } else {
                kit::ghost_button(ui, &label)
            };
            if r.clicked() {
                model.go(crate::Screen::Sessions, Some(sid.clone()));
            }
        }
        if !c.run_id.is_empty()
            && kit::ghost_button(ui, &icons::label(icons::PLAY_CIRCLE, "Open run")).clicked()
        {
            model.go(crate::Screen::Runs, Some(c.run_id.clone()));
        }
        if is_pull(c)
            && let Some(n) = c.number
            && kit::ghost_button(ui, &icons::label(icons::GIT_PULL_REQUEST, "Open pull")).clicked()
        {
            model.go(crate::Screen::Repos, Some(format!("{}#{n}", c.repo)));
        }
        if !c.card_id.is_empty()
            && kit::ghost_button(ui, &icons::label(icons::KANBAN, "Open card")).clicked()
        {
            model.go(crate::Screen::Board, Some(c.card_id.clone()));
        }
        if c.url.starts_with("https://")
            && kit::ghost_button(ui, &icons::label(icons::GITHUB_LOGO, "GitHub"))
                .on_hover_text(&c.url)
                .clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&c.url));
        }
        if let Some(f) = events_filter(c)
            && kit::ghost_button(ui, &icons::label(icons::PULSE, "Events"))
                .on_hover_text(&f)
                .clicked()
        {
            model.go(crate::Screen::Events, Some(f));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp() -> Checkpoint {
        Checkpoint {
            key: "approval:a1".into(),
            kind: CheckpointKind::Approval,
            ts: 0,
            title: "Deploy".into(),
            workflow: "Release".into(),
            run_id: "r1".into(),
            repo: "acme/api".into(),
            card_id: String::new(),
            step_id: Some("s6".into()),
            approval: None,
            session_id: None,
            number: Some(4),
            url: String::new(),
            reason: "gate".into(),
            sla_at: Some(10),
            author: "ana".into(),
            node_id: String::new(),
            conflict: false,
        }
    }

    #[test]
    fn budget_approvals_read_as_budget() {
        let mut c = cp();
        c.reason = "budget".into();
        c.approval = Some(kls_store::engine::Approval {
            kind: "budget".into(),
            ..Default::default()
        });
        assert!(is_budget(&c));
        assert!(detail_line(&c).ends_with("Budget"));
        let (((_, yes), yh), ((_, no), nh)) = decide_faces(&c).unwrap();
        assert_eq!((yes, no), ("Approve", "Reject"));
        assert!(yh.contains("past the budget") && nh.contains("budget_rejected"));
        // Other approvals keep their faces and no hover text.
        c.approval.as_mut().unwrap().kind = "human".into();
        assert!(!is_budget(&c));
        let ((_, yh), (_, nh)) = decide_faces(&c).unwrap();
        assert_eq!((yh, nh), ("", ""));
        assert_eq!(kind_icon(CheckpointKind::QuotaEscalated), icons::GAUGE);
    }

    #[test]
    fn inspector_facts_and_detail() {
        let c = cp();
        let f = facts(&c, 120_000);
        let keys: Vec<&str> = f.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            ["Workflow", "Run", "Step", "Repository", "Due", "Waiting"]
        );
        assert_eq!(f[3].1, "acme/api#4");
        assert_eq!(f[4].1, "overdue");
        assert_eq!(f[5].1, "2m");
        assert_eq!(detail_line(&c), "Release · acme/api · gate");
    }

    #[test]
    fn pulls_events_and_reviewers() {
        let mut c = cp();
        assert!(!is_pull(&c), "an approval is not a pull row");
        assert_eq!(events_filter(&c).as_deref(), Some("run:r1"));
        c.key = "pull:acme/api#4".into();
        c.run_id.clear();
        assert!(is_pull(&c));
        assert_eq!(events_filter(&c).as_deref(), Some("repo:acme/api"));
        c.key = "issue:acme/api#4".into();
        assert!(!is_pull(&c), "issues take no PR actions");
        assert_eq!(
            reviewer_list("@ana, acme/ops  bo"),
            vec!["ana", "acme/ops", "bo"]
        );
        let p = GhPull {
            author: "ana".into(),
            checks_state: "success".into(),
            mergeable: "conflict".into(),
            review_decision: "REVIEW_REQUIRED".into(),
            base_ref: "main".into(),
            head_ref: "fix".into(),
            additions: 3,
            deletions: 1,
            changed_files: 2,
            ..Default::default()
        };
        let f = pull_facts(&p);
        assert_eq!(f[0], ("Author", "@ana".to_string()));
        assert_eq!(f[3].1, "review required");
        assert_eq!(f[4].1, "main ← fix");
        assert_eq!(f[5].1, "+3 −1 · 2 files");
    }
}
