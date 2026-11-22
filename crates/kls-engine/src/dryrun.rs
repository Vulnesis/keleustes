//! Dry run: a static walk of a graph in topological order with no side
//! effects (no dispatch, no job, no row). `totals.max` adds every reachable
//! step; `totals.min` only the default path, where each condition takes the
//! outcome its rules would pick on the sample inputs (else its default).

use crate::exec::evaluate_condition;
use crate::graph::{Graph, Node};
use crate::validate::{self, Issue, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub min: i64,
    pub max: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub node_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub tokens: Range,
    /// On the default path (counted in `totals.min`).
    pub default_path: bool,
    /// What the step would do, in words.
    pub would: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulated: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_chars: Option<usize>,
    /// GitHub calls the step may make.
    #[serde(default)]
    pub calls: Range,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repositories {
    /// `scope`, `any` or `trigger`.
    pub mode: String,
    pub repos: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRun {
    pub valid: bool,
    pub issues: Vec<Issue>,
    pub plan: Vec<PlanStep>,
    pub totals: Range,
    /// GitHub calls over the run (min on the default path, max anywhere).
    #[serde(default)]
    pub calls: Range,
    pub repositories: Repositories,
}

/// GitHub calls a node may make (the web factory's per-node call costs).
pub fn node_calls(n: &Node) -> Range {
    let r = |min, max| Range { min, max };
    match n.kind.as_str() {
        "github" => match n.str("action") {
            "draft_pr" => r(2, 4),
            "pr_review" => r(1, 4),
            "approve_ready" => r(2, 4),
            "file_issues" => {
                let m = n.int("maxIssues").unwrap_or(1).max(1);
                r(1, 2 * m + 1)
            }
            "check_run" | "issue_comment" | "add_label" => r(1, 1),
            // Reads the mirror only; files board cards.
            "conflict_cards" => r(0, 0),
            _ => r(1, 2),
        },
        "gate" if n.str("check") == "forge" => r(1, 3),
        _ => r(0, 0),
    }
}

/// Token range a node may spend: its budget (at least 60% of it), else a
/// default by type.
pub fn node_tokens(n: &Node) -> Range {
    if let Some(b) = n.budget.filter(|b| b.tokens > 0) {
        return Range {
            min: b.tokens * 6 / 10,
            max: b.tokens,
        };
    }
    match n.kind.as_str() {
        "crew" => match n.str("dispatch") {
            "crew" => Range {
                min: 60_000,
                max: 300_000,
            },
            _ => Range {
                min: 20_000,
                max: 100_000,
            },
        },
        _ => Range::default(),
    }
}

/// The estimate a save stores: (min, max) tokens per run, capped at the run budget.
pub fn estimate(g: &Graph) -> (i64, i64) {
    let d = dry_run(g, &Value::Null);
    (d.totals.min, d.totals.max)
}

fn would(n: &Node) -> String {
    match n.kind.as_str() {
        "trigger" => format!("Starts the run ({})", n.summary()),
        "crew" => format!("Would dispatch {} to an online node", n.summary()),
        "job" => format!("Would run job {} on a node", n.str("job")),
        "condition" => "Picks a branch".into(),
        "wait" => format!("Would wait {}", n.summary()),
        "human" => "Would wait for your approval".into(),
        "gate" => format!("Would wait at the gate: {}", n.summary()),
        "card" => format!("Would {} the run's card", n.summary()),
        "notify" => format!("Would post: {}", n.str("message")),
        "github" => format!("Would call GitHub: {}", n.str("action")),
        _ => String::new(),
    }
}

/// Walks `g`. `sample` is the context conditions see (`{"event": ..,
/// "inputs": ..}`); `Value::Null` uses an empty one.
pub fn dry_run(g: &Graph, sample: &Value) -> DryRun {
    let issues = validate::validate(g, Mode::Publish);
    let valid = !validate::has_errors(&issues);
    let ctx = if sample.is_object() {
        sample.clone()
    } else {
        json!({ "event": {}, "inputs": {}, "steps": {}, "run": {} })
    };

    // Reachable set (all edges) and default path (conditions take one outcome).
    let mut reachable: HashSet<String> = g.triggers().map(|n| n.id.clone()).collect();
    let mut queue: Vec<String> = reachable.iter().cloned().collect();
    while let Some(id) = queue.pop() {
        for e in g.outgoing(&id) {
            if reachable.insert(e.to.clone()) {
                queue.push(e.to.clone());
            }
        }
    }
    let mut on_path: HashSet<String> = HashSet::new();
    let mut simulated: Vec<(String, String)> = Vec::new();
    if let Some(t) = g.triggers().next() {
        let mut queue = vec![t.id.clone()];
        while let Some(id) = queue.pop() {
            if !on_path.insert(id.clone()) {
                continue;
            }
            let Some(n) = g.node(&id) else { continue };
            let pick = if n.kind == "condition" {
                let o = evaluate_condition(n, &ctx).unwrap_or_default();
                simulated.push((id.clone(), o.clone()));
                Some(o)
            } else {
                None
            };
            for e in g.outgoing(&id).filter(|e| !e.is_loop) {
                let take = match (&pick, n.kind.as_str()) {
                    (Some(o), _) => &e.outcome == o,
                    (None, "gate") => e.outcome.is_empty() || e.outcome == "pass",
                    _ => e.outcome.is_empty(),
                };
                if take {
                    queue.push(e.to.clone());
                }
            }
        }
    }

    let mut plan = Vec::new();
    let mut totals = Range::default();
    let mut calls = Range::default();
    for id in validate::topo_order(g) {
        if !reachable.contains(&id) {
            continue;
        }
        let Some(n) = g.node(&id) else { continue };
        let tokens = node_tokens(n);
        let ncalls = node_calls(n);
        let default_path = on_path.contains(&id);
        totals.max += tokens.max;
        calls.max += ncalls.max;
        if default_path {
            totals.min += tokens.min;
            calls.min += ncalls.min;
        }
        let sim = simulated
            .iter()
            .find(|(nid, _)| *nid == id)
            .map(|(_, o)| json!({ "outcome": o, "source": "rules" }));
        let brief = n.str("brief").trim().chars().count();
        plan.push(PlanStep {
            node_id: id.clone(),
            kind: n.kind.clone(),
            title: n.title.clone(),
            tokens,
            default_path,
            would: would(n),
            simulated: sim,
            brief_chars: (n.kind == "crew" && brief > 0).then_some(brief),
            calls: ncalls,
        });
    }
    if let Some(cap) = g.limits.run_budget_tokens.filter(|v| *v > 0) {
        totals.min = totals.min.min(cap);
        totals.max = totals.max.min(cap);
    }

    let has_timer_or_manual = g
        .triggers()
        .any(|t| matches!(t.trigger_kind(), "cron" | "manual"));
    let repositories = if !g.scope.repos.is_empty() {
        Repositories {
            mode: "scope".into(),
            repos: g.scope.repos.clone(),
            text: format!("A run starts on {}.", g.scope.repos.join(", ")),
        }
    } else if has_timer_or_manual {
        Repositories {
            mode: "any".into(),
            repos: vec![],
            text: "A schedule or a manual start runs on any repository an online node has; an event starts it on the event's own repository.".into(),
        }
    } else {
        Repositories {
            mode: "trigger".into(),
            repos: vec![],
            text: "It runs on the repository of the event that starts it.".into(),
        }
    };

    DryRun {
        valid,
        issues,
        plan,
        totals,
        calls,
        repositories,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Budget, Edge};

    fn graph() -> Graph {
        serde_json::from_value(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Start","config":{"kind":"manual"}},
                {"id":"c","type":"condition","title":"Big?","config":{
                    "rules":[{"path":"inputs.size","op":"gt","value":10,"outcome":"big"}],
                    "default":"small","outcomes":[{"value":"big"},{"value":"small"}]}},
                {"id":"w","type":"crew","title":"Build","config":{"dispatch":"worker","agent":"belai:builder","brief":"Do it well"}},
                {"id":"s","type":"notify","title":"Say","config":{"message":"small","severity":"info"}}
            ],
            "edges": [
                {"from":"t","to":"c"},
                {"from":"c","to":"w","outcome":"big"},
                {"from":"c","to":"s","outcome":"small"}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn default_path_and_totals() {
        let d = dry_run(&graph(), &Value::Null);
        assert!(d.valid, "{:?}", d.issues);
        assert_eq!(d.plan.len(), 4);
        assert_eq!(d.plan[0].node_id, "t");
        // No inputs: the condition takes its default, "small"; the crew is off-path.
        let w = d.plan.iter().find(|p| p.node_id == "w").unwrap();
        assert!(!w.default_path);
        assert_eq!(w.brief_chars, Some(10));
        assert_eq!(
            d.totals,
            Range {
                min: 0,
                max: 100_000
            }
        );
        let c = d.plan.iter().find(|p| p.node_id == "c").unwrap();
        assert_eq!(c.simulated.as_ref().unwrap()["outcome"], "small");
        assert_eq!(d.repositories.mode, "any");
    }

    #[test]
    fn sample_inputs_steer_the_path() {
        let sample = json!({"inputs": {"size": 50}, "event": {}, "steps": {}, "run": {}});
        let d = dry_run(&graph(), &sample);
        assert!(
            d.plan
                .iter()
                .find(|p| p.node_id == "w")
                .unwrap()
                .default_path
        );
        assert_eq!(d.totals.min, 20_000);
    }

    #[test]
    fn budgets_and_cap() {
        let mut g = graph();
        g.node_mut("w").unwrap().budget = Some(Budget { tokens: 50_000 });
        g.limits.run_budget_tokens = Some(40_000);
        let d = dry_run(&g, &json!({"inputs":{"size":99}}));
        assert_eq!(
            d.totals,
            Range {
                min: 30_000,
                max: 40_000
            }
        );
        // The node budget over the run budget is a problem.
        assert!(!d.valid);
        assert_eq!(estimate(&graph()), (0, 100_000));
    }

    #[test]
    fn invalid_graph_still_plans() {
        let mut g = graph();
        g.edges.push(Edge {
            from: "s".into(),
            to: "c".into(),
            ..Default::default()
        });
        let d = dry_run(&g, &Value::Null);
        assert!(!d.valid);
        assert!(d.issues.iter().any(|i| i.code == "cycle"));
        assert_eq!(d.plan.len(), 4);
        let mut g = graph();
        g.nodes[0].config.insert("kind".into(), json!("event"));
        g.nodes[0]
            .config
            .insert("event".into(), json!("card.moved"));
        assert_eq!(dry_run(&g, &Value::Null).repositories.mode, "trigger");
    }
}
