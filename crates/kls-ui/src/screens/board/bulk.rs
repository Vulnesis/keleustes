//! Board: lenses, swimlanes, multi-select with a bulk bar, and the assign
//! sheet (route a card to a worker or crew so it claims it).

use crate::icons;
use crate::kit;
use crate::model::{Model, Notice};
use crate::theme::palette;
use egui::RichText;
use kls_core::board::cards;
use kls_core::board::{CardPatch, OWNER, STATUSES};
use kls_store::board::Card;
use kls_store::engine::Workflow;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lens {
    #[default]
    All,
    NeedsMe,
    Unrouted,
    /// Not touched for `tabs::STALE_DAYS` days (and not done).
    Stale,
    P1,
    FromWorkflow,
}

impl Lens {
    pub const ALL: [Lens; 6] = [
        Lens::All,
        Lens::NeedsMe,
        Lens::Unrouted,
        Lens::Stale,
        Lens::P1,
        Lens::FromWorkflow,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Lens::All => "All",
            Lens::NeedsMe => "Needs me",
            Lens::Unrouted => "Unrouted",
            Lens::Stale => "Stale",
            Lens::P1 => "P1",
            Lens::FromWorkflow => "From a workflow",
        }
    }
}

fn meta(c: &Card) -> Value {
    serde_json::from_str(&c.meta_json).unwrap_or(Value::Null)
}

/// The workflow (run) a card came from, if any.
pub fn workflow_of(c: &Card) -> Option<String> {
    meta(c)
        .pointer("/keleustes/workflowId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub fn lens_matches(l: Lens, c: &Card) -> bool {
    match l {
        Lens::All => true,
        Lens::NeedsMe => cards::is_held(c) || cards::is_blocked(c) || c.status == "review",
        Lens::Unrouted => {
            c.status != "done"
                && c.assignee.is_empty()
                && c.labels.is_empty()
                && !cards::claim(c).is_claimed()
        }
        Lens::Stale => super::tabs::is_stale(c, kls_store::now_ms()),
        Lens::P1 => c.priority >= 2,
        Lens::FromWorkflow => workflow_of(c).is_some(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Group {
    #[default]
    None,
    Repo,
    Workflow,
    Crew,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::None, Group::Repo, Group::Workflow, Group::Crew];
    pub fn title(self) -> &'static str {
        match self {
            Group::None => "No lanes",
            Group::Repo => "By repo",
            Group::Workflow => "By workflow",
            Group::Crew => "By crew",
        }
    }
}

/// The swimlane a card sits in.
pub fn group_key(g: Group, c: &Card, workflows: &[Workflow]) -> String {
    let or_none = |s: String| if s.is_empty() { "—".to_string() } else { s };
    match g {
        Group::None => String::new(),
        Group::Repo => or_none(c.repo.clone()),
        Group::Workflow => or_none(
            workflow_of(c)
                .map(|id| {
                    workflows
                        .iter()
                        .find(|w| w.id == id)
                        .map(|w| w.name.clone())
                        .unwrap_or(id)
                })
                .unwrap_or_default(),
        ),
        Group::Crew => {
            let m = meta(c);
            let agent = m
                .pointer("/keleustes/agent")
                .and_then(Value::as_str)
                .unwrap_or("");
            let claimed = cards::claim(c).claimed_by;
            or_none(if !claimed.is_empty() {
                claimed
            } else if !agent.is_empty() {
                agent.to_string()
            } else {
                c.assignee.clone()
            })
        }
    }
}

/// Lens chips, lanes and select mode.
pub fn toolbar(
    ui: &mut egui::Ui,
    lens: &mut Lens,
    group: &mut Group,
    select: &mut bool,
    cards: &[Card],
) {
    ui.horizontal_wrapped(|ui| {
        for l in Lens::ALL {
            let n = cards.iter().filter(|c| lens_matches(l, c)).count();
            if kit::toggle_chip(ui, &format!("{} {n}", l.title()), *lens == l).clicked() {
                *lens = l;
            }
        }
        ui.add_space(8.0);
        egui::ComboBox::from_id_salt("board-group")
            .selected_text(icons::label(icons::LIST, group.title()))
            .show_ui(ui, |ui| {
                for g in Group::ALL {
                    ui.selectable_value(group, g, g.title());
                }
            });
        ui.checkbox(select, icons::label(icons::CHECKS, "Select"));
    });
}

/// The bulk bar for selected cards.
pub fn bulk_bar(
    ui: &mut egui::Ui,
    model: &Model,
    selected: &mut HashSet<String>,
    workflows: &[Workflow],
    send_wf: &mut String,
) {
    if selected.is_empty() {
        return;
    }
    let p = palette(ui.ctx());
    let ids: Vec<String> = selected.iter().cloned().collect();
    kit::card(ui).inner_margin(8).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} selected", ids.len()))
                    .strong()
                    .color(p.gold_text),
            );
            ui.menu_button(icons::label(icons::ARROW_RIGHT, "Move"), |ui| {
                for s in STATUSES {
                    if ui.button(s).clicked() {
                        let (ids, to) = (ids.clone(), s.to_string());
                        model.act(move |m| async move {
                            let core = m.core();
                            for id in ids {
                                if let Some(c) = core.card(&id).await? {
                                    core.move_card(&c.id, c.version, &to, OWNER).await?;
                                }
                            }
                            Ok(None)
                        });
                        ui.close();
                    }
                }
            });
            ui.menu_button(icons::label(icons::FLAG, "Priority"), |ui| {
                for pr in (kls_core::board::PRIORITY_MIN..=kls_core::board::PRIORITY_MAX).rev() {
                    if ui.button(pr.to_string()).clicked() {
                        let ids = ids.clone();
                        model.act(move |m| async move {
                            let core = m.core();
                            for id in ids {
                                if let Some(c) = core.card(&id).await? {
                                    let p = CardPatch {
                                        priority: Some(pr),
                                        ..Default::default()
                                    };
                                    core.update_card(&c.id, c.version, p, OWNER).await?;
                                }
                            }
                            Ok(None)
                        });
                        ui.close();
                    }
                }
            });
            if kit::ghost_button(ui, &icons::label(icons::LOCK_SIMPLE, "Release claims")).clicked()
            {
                let ids = ids.clone();
                model.act(move |m| async move {
                    let core = m.core();
                    let mut n = 0;
                    for id in ids {
                        if let Some(c) = core.card(&id).await?
                            && cards::claim(&c).is_claimed()
                        {
                            core.release_card_claim(&c.id, c.version, OWNER).await?;
                            n += 1;
                        }
                    }
                    Ok(Some(Notice::Info(format!("Released {n}"))))
                });
            }
            let live: Vec<&Workflow> = workflows
                .iter()
                .filter(|w| w.status == "published" && w.enabled)
                .collect();
            if !live.is_empty() {
                egui::ComboBox::from_id_salt("bulk-send-wf")
                    .selected_text(
                        live.iter()
                            .find(|w| w.id == *send_wf)
                            .map(|w| w.name.clone())
                            .unwrap_or_else(|| "Workflow".into()),
                    )
                    .show_ui(ui, |ui| {
                        for w in &live {
                            ui.selectable_value(send_wf, w.id.clone(), &w.name);
                        }
                    });
                if ui
                    .add_enabled_ui(!send_wf.is_empty(), |ui| {
                        kit::ghost_button(ui, &icons::label(icons::PLAY, "Send"))
                    })
                    .inner
                    .on_hover_text("One run per card; cards without a repository are skipped")
                    .clicked()
                {
                    let (ids, wf) = (ids.clone(), send_wf.clone());
                    model.act(move |m| async move {
                        let core = m.core();
                        let (mut started, mut skipped) = (0, 0);
                        for id in ids {
                            let Some(c) = core.card(&id).await? else {
                                continue;
                            };
                            if c.repo.is_empty() {
                                skipped += 1;
                                continue;
                            }
                            let out = kls_engine::ops::start_run(
                                core,
                                kls_engine::ops::StartRun {
                                    workflow_id: wf.clone(),
                                    trigger: kls_engine::ops::TriggerInfo {
                                        kind: "manual".into(),
                                        label: "from the board".into(),
                                        ..Default::default()
                                    },
                                    repo: c.repo.clone(),
                                    card_id: c.id.clone(),
                                    ..Default::default()
                                },
                            )
                            .await?;
                            match out {
                                kls_engine::ops::StartOutcome::Started(_) => started += 1,
                                _ => skipped += 1,
                            }
                        }
                        Ok(Some(Notice::Info(format!(
                            "{started} runs, {skipped} skipped"
                        ))))
                    });
                }
            }
            if kit::danger_button(ui, &icons::label(icons::TRASH, "Delete")).clicked() {
                let ids = ids.clone();
                model.act(move |m| async move {
                    let core = m.core();
                    for id in ids {
                        if let Some(c) = core.card(&id).await?
                            && !c.deleted
                        {
                            core.delete_card(&c.id, c.version, OWNER).await?;
                        }
                    }
                    Ok(Some(Notice::Info("Deleted".into())))
                });
                selected.clear();
            }
            if kit::ghost_button(ui, &icons::label(icons::X, "Clear")).clicked() {
                selected.clear();
            }
        });
    });
}

/// The assign sheet: route the card to a worker or crew (its claim list
/// and labels), or assign a person.
pub fn assign_menu(ui: &mut egui::Ui, model: &Model, card: &Card, saved: &[String]) {
    ui.menu_button(icons::label(icons::USER_PLUS, "Assign"), |ui| {
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                ui.label(kit::caps("Crews", palette(ui.ctx()).muted));
                for c in kls_engine::graph::crews() {
                    if ui.button(icons::label(icons::USERS_THREE, c)).clicked() {
                        route(model, card, "crew", c);
                        ui.close();
                    }
                }
                ui.label(kit::caps("Workers", palette(ui.ctx()).muted));
                let mut names: Vec<String> = kls_engine::graph::workers()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                for s in saved {
                    if !names.contains(s) {
                        names.push(s.clone());
                    }
                }
                for w in names {
                    if ui.button(icons::label(icons::ROBOT, &w)).clicked() {
                        route(model, card, "worker", &w);
                        ui.close();
                    }
                }
            });
    });
}

fn route(model: &Model, card: &Card, kind: &'static str, name: &str) {
    let (id, name) = (card.id.clone(), name.to_string());
    model.act(move |m| async move {
        let core = m.core();
        let Some(c) = core.card(&id).await? else {
            anyhow::bail!("the card is gone");
        };
        let mut saved = std::collections::HashMap::new();
        for p in core.store().agent_profiles().await.unwrap_or_default() {
            saved.insert(
                p.name.clone(),
                kls_engine::catalogue::ProfileSpec::parse(&p.meta_json),
            );
        }
        let lookup = |n: &str| saved.get(n).cloned();
        let (agent, crew) = if kind == "crew" {
            ("", name.as_str())
        } else {
            (name.as_str(), "")
        };
        let Some(r) = kls_engine::route::route_for(kind, agent, crew, None, &lookup) else {
            anyhow::bail!("{name} claims no cards");
        };
        let mut labels = c.labels.clone();
        for l in &r.labels {
            if !labels.iter().any(|x| x.eq_ignore_ascii_case(l)) {
                labels.push(l.clone());
            }
        }
        let mut meta: Value =
            serde_json::from_str(&c.meta_json).unwrap_or_else(|_| serde_json::json!({}));
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        if !meta.get("belai").is_some_and(Value::is_object) {
            meta["belai"] = serde_json::json!({});
        }
        meta["belai"]["list"] = serde_json::json!(r.list);
        let p = CardPatch {
            status: Some(r.status.clone()),
            labels: Some(labels),
            meta_json: Some(meta.to_string()),
            ..Default::default()
        };
        core.update_card(&c.id, c.version, p, OWNER).await?;
        Ok(Some(Notice::Info(format!("Routed to {}", r.agent))))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(status: &str, labels: &[&str], prio: i32, meta: &str) -> Card {
        Card {
            status: status.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            priority: prio,
            meta_json: meta.into(),
            ..Default::default()
        }
    }

    #[test]
    fn lenses_and_lanes() {
        let a = card("review", &[], 0, "{}");
        let b = card("backlog", &[], 0, "{}");
        let c = card(
            "backlog",
            &["build"],
            3,
            r#"{"keleustes":{"workflowId":"w1","agent":"belai:builder"}}"#,
        );
        assert!(lens_matches(Lens::NeedsMe, &a));
        assert!(lens_matches(Lens::Unrouted, &b) && !lens_matches(Lens::Unrouted, &c));
        assert!(lens_matches(Lens::P1, &c) && !lens_matches(Lens::P1, &b));
        assert!(lens_matches(Lens::FromWorkflow, &c));
        let wf = vec![Workflow {
            id: "w1".into(),
            name: "Fix".into(),
            ..Default::default()
        }];
        assert_eq!(group_key(Group::Workflow, &c, &wf), "Fix");
        assert_eq!(group_key(Group::Crew, &c, &wf), "belai:builder");
        assert_eq!(group_key(Group::Repo, &b, &wf), "—");
    }
}
