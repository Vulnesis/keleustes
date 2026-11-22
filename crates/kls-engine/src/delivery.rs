//! Delivery and lifecycle reads (builtin-agents P12, engine side):
//!
//! - **Unanswered deliveries** (vdb-site `factory_events_unanswered.go`):
//!   GitHub events that started no run, grouped by webhook name.
//! - **Template ranking** (`factory_templates_signals.go`): templates whose
//!   trigger would have answered those events, most first.
//! - **Lifecycle** (`factory_lifecycle_sweep.go`): a card's stage episodes
//!   from its moves, and per-stage flow (open, arrivals, completions,
//!   oldest open).
//! - **Roles** (`factory_delivery_roles.go`): the delivery role of a builtin
//!   agent, from its claim rules.
//!
//! All computed on read from the event log, trigger fires and card
//! activity, so a sweep has nothing to store and is idempotent.

use crate::catalogue;
use anyhow::Result;
use kls_core::Core;
use kls_store::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// GitHub events of one webhook name that started nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unanswered {
    pub name: String,
    pub count: i64,
    pub last_at: i64,
    pub repos: Vec<String>,
}

/// Reads events after `after_id` (at most `limit`) and groups the GitHub
/// ones no trigger started a run for.
pub async fn unanswered(core: &Core, since: i64, limit: i64) -> Result<Vec<Unanswered>> {
    let s = core.store();
    let events: Vec<Event> = s
        .events_after(0, limit.max(1))
        .await?
        .into_iter()
        .filter(|e| e.at >= since && e.kind.starts_with("github."))
        .collect();
    let mut answered: HashSet<i64> = HashSet::new();
    for sch in s.schedules().await? {
        for f in s.trigger_fires(&sch.id, since).await? {
            if f.outcome == "started"
                && let Some(id) = f.event_id
            {
                answered.insert(id);
            }
        }
    }
    let mut by: BTreeMap<String, Unanswered> = BTreeMap::new();
    for ev in events {
        if answered.contains(&ev.id) {
            continue;
        }
        let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(Value::Null);
        let repo = data
            .get("repo")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        for name in crate::forge::webhook_events(&ev, &data) {
            let u = by.entry(name.clone()).or_insert_with(|| Unanswered {
                name,
                ..Default::default()
            });
            u.count += 1;
            u.last_at = u.last_at.max(ev.at);
            if !repo.is_empty() && !u.repos.contains(&repo) {
                u.repos.push(repo.clone());
            }
        }
    }
    let mut out: Vec<Unanswered> = by.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// A template suggested by unanswered events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub template_id: String,
    pub score: i64,
    pub reasons: Vec<String>,
}

/// Templates whose triggers match the unanswered events, best first.
pub fn rank_templates(unanswered: &[Unanswered]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for t in crate::templates::templates() {
        let mut score = 0;
        let mut reasons = Vec::new();
        for n in t.graph.triggers() {
            let want = match n.trigger_kind() {
                "webhook" => n.str("event").to_string(),
                "label" => "issues.labeled".to_string(),
                _ => continue,
            };
            for u in unanswered {
                if crate::forge::webhook_matches(&want, std::slice::from_ref(&u.name)) {
                    score += u.count;
                    reasons.push(format!("{} × {}", u.count, u.name));
                }
            }
        }
        if score > 0 {
            out.push(Suggestion {
                template_id: t.id.clone(),
                score,
                reasons,
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.template_id.cmp(&b.template_id))
    });
    out
}

/// One stay of a card in a stage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub stage: String,
    pub entered_at: i64,
    /// `None` while the card is still there.
    pub exited_at: Option<i64>,
}

/// Stage episodes from a card's creation and moves (oldest first).
pub fn episodes(
    created_at: i64,
    first_status: &str,
    moves: &[(i64, String, String)],
) -> Vec<Episode> {
    let mut out = vec![Episode {
        stage: first_status.to_string(),
        entered_at: created_at,
        exited_at: None,
    }];
    for (at, _from, to) in moves {
        if let Some(last) = out.last_mut() {
            if last.stage == *to {
                continue;
            }
            last.exited_at = Some(*at);
        }
        out.push(Episode {
            stage: to.clone(),
            entered_at: *at,
            exited_at: None,
        });
    }
    out
}

/// A card's lifecycle from its activity log.
pub async fn card_lifecycle(core: &Core, card_id: &str) -> Result<Vec<Episode>> {
    let Some(card) = core.card(card_id).await? else {
        return Ok(Vec::new());
    };
    let mut acts = core.card_activity(&card.id).await?;
    acts.reverse();
    let mut first = card.status.clone();
    let mut moves = Vec::new();
    for a in &acts {
        let d: Value = serde_json::from_str(&a.detail_json).unwrap_or(Value::Null);
        match a.kind.as_str() {
            "created" => {
                if let Some(s) = d.get("status").and_then(Value::as_str) {
                    first = s.to_string();
                }
            }
            "moved" => {
                let from = d
                    .get("from")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let to = d
                    .get("to")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !to.is_empty() {
                    moves.push((a.at, from, to));
                }
            }
            _ => {}
        }
    }
    Ok(episodes(card.created_at, &first, &moves))
}

/// Per-stage flow over a window (the cockpit Flow panel).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageFlow {
    pub stage: String,
    pub open: i64,
    pub arrivals: i64,
    pub completions: i64,
    pub oldest_open_at: Option<i64>,
}

pub async fn flow(core: &Core, since: i64) -> Result<Vec<StageFlow>> {
    let mut by: BTreeMap<String, StageFlow> = BTreeMap::new();
    for c in core.cards().await? {
        for e in card_lifecycle(core, &c.id).await? {
            let f = by.entry(e.stage.clone()).or_insert_with(|| StageFlow {
                stage: e.stage.clone(),
                ..Default::default()
            });
            if e.entered_at >= since {
                f.arrivals += 1;
            }
            match e.exited_at {
                Some(x) if x >= since => f.completions += 1,
                None => {
                    f.open += 1;
                    f.oldest_open_at = Some(
                        f.oldest_open_at
                            .map_or(e.entered_at, |o| o.min(e.entered_at)),
                    );
                }
                _ => {}
            }
        }
    }
    let order = kls_core::board::STATUSES;
    let mut out: Vec<StageFlow> = by.into_values().collect();
    out.sort_by_key(|f| order.iter().position(|s| *s == f.stage).unwrap_or(99));
    Ok(out)
}

/// The delivery role of an agent: from its builtin claim rules, else its name.
pub fn role_of(agent: &str) -> &'static str {
    let bare = agent.strip_prefix("belai:").unwrap_or(agent);
    match bare {
        "scout" | "vuln-scout" | "repo-onboarder" => return "scout",
        "planner" | "designer" => return "planner",
        "repro" | "github-investigator" | "ci-explainer" | "flaky-hunter" => return "investigator",
        "reviewer" | "pr-reviewer" | "pr-steward" => return "reviewer",
        "verifier" => return "verifier",
        "cli-runner" => return "scanner",
        _ => {}
    }
    if bare.ends_with("triager") || bare == "ticket-writer" || bare.ends_with("reviewer") {
        return "triager";
    }
    match catalogue::profile(agent) {
        Some(p) if p.is_worker() && !p.read_only() => "builder",
        Some(p) if p.is_worker() => "triager",
        Some(_) => "background",
        None => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn episodes_follow_moves() {
        let m = vec![
            (10, "backlog".to_string(), "doing".to_string()),
            (20, "doing".to_string(), "doing".to_string()),
            (30, "doing".to_string(), "done".to_string()),
        ];
        let e = episodes(1, "backlog", &m);
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].entered_at, e[0].exited_at), (1, Some(10)));
        assert_eq!(e[2].stage, "done");
        assert!(e[2].exited_at.is_none());
    }

    #[test]
    fn roles() {
        assert_eq!(role_of("belai:builder"), "builder");
        assert_eq!(role_of("belai:patcher"), "builder");
        assert_eq!(role_of("belai:reviewer"), "reviewer");
        assert_eq!(role_of("belai:scout"), "scout");
        assert_eq!(role_of("belai:secret-triager"), "triager");
        assert_eq!(role_of("belai:deps-go"), "background");
        assert_eq!(role_of("acme:thing"), "other");
    }

    #[tokio::test]
    async fn unanswered_and_ranked() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        for n in 1..=3 {
            core.emit(
                "github.pull.opened",
                &format!("a/b#{n}"),
                &json!({"repo": "a/b", "number": n}),
            )
            .await
            .unwrap();
        }
        core.emit("card.moved", "c1", &json!({})).await.unwrap();
        let u = unanswered(&core, 0, 1000).await.unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!((u[0].name.as_str(), u[0].count), ("pull_request.opened", 3));
        let r = rank_templates(&u);
        assert!(
            r.iter()
                .any(|s| s.template_id == "pr-reviewer" && s.score == 3)
        );

        let c = core
            .create_card(
                kls_core::board::CardDraft {
                    title: "T".into(),
                    status: "backlog".into(),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        let c = core
            .move_card(&c.id, c.version, "doing", "owner")
            .await
            .unwrap();
        let l = card_lifecycle(&core, &c.id).await.unwrap();
        assert_eq!(
            l.iter().map(|e| e.stage.as_str()).collect::<Vec<_>>(),
            ["backlog", "doing"]
        );
        let f = flow(&core, 0).await.unwrap();
        let doing = f.iter().find(|x| x.stage == "doing").unwrap();
        assert_eq!((doing.open, doing.arrivals), (1, 1));
    }
}
