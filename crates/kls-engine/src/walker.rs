//! The walker: a pure function from (graph, step history) to what starts
//! next or how the run ends. It stores nothing; which edges were taken is
//! derived from the steps (vdb-site docs/factory-engine.md, "The walker").
//!
//! - A node's latest step is the one with the highest attempt. A step is
//!   open while `pending`, `running` or `waiting`.
//! - An edge is taken when its source's latest step finished and the edge
//!   matches its route: a condition's outcome, a gate's pass (or, failed
//!   with `gate_failed`, its `fail`/loop edges), any plain edge otherwise.
//! - A taken edge is fresh when the target has no step created after the
//!   source step; only fresh edges activate (so a loop re-runs once).
//! - A node with several incoming edges starts when one is taken and no
//!   other predecessor has an open step.
//! - A failed step is retried up to the node's `maxAttempts`; past
//!   `stopTheLineAttempts` the line stops.

use crate::graph::{Graph, Node, OUTCOME_FAIL, OUTCOME_PASS};
use kls_store::engine::RunStep;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    pub node_id: String,
    pub attempt: i64,
    /// A retry of a failed attempt (the executor backs off).
    pub retry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    /// `succeeded`, `failed` or `cancelled`.
    pub status: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub start: Vec<Activation>,
    pub finish: Option<Finish>,
}

pub fn is_open(status: &str) -> bool {
    matches!(status, "pending" | "running" | "waiting")
}

/// Errors that a retry would not change.
fn retriable(error: &str) -> bool {
    !(error.starts_with("gate_failed")
        || error.starts_with("rejected")
        || error.starts_with("expired")
        || error.starts_with("cancelled")
        || error.starts_with("integration_missing")
        || error.starts_with("no_outcome")
        || error.starts_with("no_card")
        || error.starts_with("no_host_timeout")
        || error.starts_with("no_host_for_repo")
        || error.starts_with("prerequisite_missing")
        || error.starts_with("budget_rejected"))
}

/// The first word of an error (`timeout: ...` reads `timeout`).
fn short(error: &str) -> &str {
    error
        .split([':', ' '])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("failed")
}

fn latest_steps(steps: &[RunStep]) -> HashMap<&str, &RunStep> {
    let mut latest: HashMap<&str, &RunStep> = HashMap::new();
    for s in steps {
        match latest.get(s.node_id.as_str()) {
            Some(cur) if (cur.attempt, cur.seq) >= (s.attempt, s.seq) => {}
            _ => {
                latest.insert(s.node_id.as_str(), s);
            }
        }
    }
    latest
}

/// Whether `edge` leaves `src` given its latest step.
fn edge_taken(src: &Node, step: &RunStep, outcome: &str, is_loop: bool) -> bool {
    match step.status.as_str() {
        "succeeded" => match src.kind.as_str() {
            "condition" | "decision" => !outcome.is_empty() && outcome == step.route,
            "gate" => !is_loop && (outcome.is_empty() || outcome == OUTCOME_PASS),
            _ => outcome.is_empty() && !is_loop,
        },
        "failed" if src.kind == "gate" && step.error.starts_with("gate_failed") => {
            is_loop || outcome == OUTCOME_FAIL
        }
        _ => false,
    }
}

/// The trigger a run starts at: `trigger_node` when it names a trigger,
/// else the first trigger. An extra trigger (`<id>#<n>`, the trigger's
/// `also` entries) starts at its trigger.
pub fn start_node<'a>(g: &'a Graph, trigger_node: &str) -> Option<&'a Node> {
    g.node(crate::graph::trigger_of(trigger_node))
        .filter(|n| n.kind == "trigger")
        .or_else(|| g.triggers().next())
}

pub fn plan(g: &Graph, steps: &[RunStep], trigger_node: &str) -> Plan {
    if steps.is_empty() {
        return match start_node(g, trigger_node) {
            Some(n) => Plan {
                start: vec![Activation {
                    node_id: n.id.clone(),
                    attempt: 1,
                    retry: false,
                }],
                finish: None,
            },
            None => Plan {
                start: vec![],
                finish: Some(Finish {
                    status: "failed",
                    reason: "no_trigger".into(),
                }),
            },
        };
    }
    let latest = latest_steps(steps);
    let stop_line = g.stop_the_line();
    let any_open = steps.iter().any(|s| is_open(&s.status));
    // A step is stale when a predecessor ran again after it (a retry from
    // an earlier step): it is about to run again, so it decides nothing.
    let stale = |n: &Node, s: &RunStep| {
        g.incoming(&n.id)
            .any(|e| steps.iter().any(|t| t.node_id == e.from && t.seq > s.seq))
    };

    // Endings decided by one step.
    for n in &g.nodes {
        let Some(s) = latest.get(n.id.as_str()) else {
            continue;
        };
        if stale(n, s) {
            continue;
        }
        if (n.kind == "condition" || n.kind == "decision")
            && s.status == "succeeded"
            && n.outcomes().iter().any(|o| o.end && o.value == s.route)
        {
            return finish("succeeded", format!("ended:{}", n.id));
        }
        if n.kind == "gate"
            && s.status == "failed"
            && s.error.starts_with("gate_failed")
            && n.str("onFail") == "end_run"
            && !g
                .outgoing(&n.id)
                .any(|e| e.is_loop || e.outcome == OUTCOME_FAIL)
        {
            return finish("failed", format!("gate_failed:{}", n.id));
        }
        if s.status == "failed" && s.error.starts_with("rejected") && n.kind == "human" {
            return finish("cancelled", format!("declined:{}", n.id));
        }
    }

    let mut start: Vec<Activation> = Vec::new();
    let mut unhandled: Option<Finish> = None;

    // Retries of failed attempts.
    for n in &g.nodes {
        let Some(s) = latest.get(n.id.as_str()) else {
            continue;
        };
        if s.status != "failed" || stale(n, s) {
            continue;
        }
        let handled_by_route = n.kind == "gate"
            && s.error.starts_with("gate_failed")
            && g.outgoing(&n.id)
                .any(|e| e.is_loop || e.outcome == OUTCOME_FAIL);
        if handled_by_route {
            continue;
        }
        if retriable(&s.error) && s.attempt < n.max_attempts() {
            if s.attempt + 1 > stop_line {
                return finish("failed", format!("stopped_line:{}", n.id));
            }
            start.push(Activation {
                node_id: n.id.clone(),
                attempt: s.attempt + 1,
                retry: true,
            });
        } else if unhandled.is_none() {
            let reason = if n.max_attempts() > 1 && retriable(&s.error) {
                format!("stopped_line:{}", n.id)
            } else {
                format!("{}:{}", short(&s.error), n.id)
            };
            unhandled = Some(Finish {
                status: "failed",
                reason,
            });
        }
    }
    if let Some(f) = unhandled {
        return Plan {
            start: vec![],
            finish: Some(f),
        };
    }

    // Edges taken out of finished steps.
    let mut candidates: Vec<&str> = Vec::new();
    for e in &g.edges {
        let (Some(src), Some(s)) = (g.node(&e.from), latest.get(e.from.as_str())) else {
            continue;
        };
        if !edge_taken(src, s, &e.outcome, e.is_loop) {
            continue;
        }
        let fresh = !steps.iter().any(|t| t.node_id == e.to && t.seq > s.seq);
        if fresh && !candidates.contains(&e.to.as_str()) {
            candidates.push(e.to.as_str());
        }
    }
    for n in &g.nodes {
        if !candidates.contains(&n.id.as_str()) {
            continue;
        }
        if start.iter().any(|a| a.node_id == n.id) {
            continue;
        }
        let prev = latest.get(n.id.as_str());
        if prev.is_some_and(|s| is_open(&s.status)) {
            continue;
        }
        let waiting_on_peer = g.incoming(&n.id).any(|e| {
            latest
                .get(e.from.as_str())
                .is_some_and(|s| is_open(&s.status))
        });
        if waiting_on_peer {
            continue;
        }
        let attempt = prev.map(|s| s.attempt + 1).unwrap_or(1);
        if attempt > stop_line {
            return finish("failed", format!("stopped_line:{}", n.id));
        }
        start.push(Activation {
            node_id: n.id.clone(),
            attempt,
            retry: false,
        });
    }

    if !start.is_empty() || any_open {
        return Plan {
            start,
            finish: None,
        };
    }
    finish("succeeded", String::new())
}

fn finish(status: &'static str, reason: String) -> Plan {
    Plan {
        start: vec![],
        finish: Some(Finish { status, reason }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Node};
    use serde_json::json;

    fn node(id: &str, kind: &str, config: serde_json::Value) -> Node {
        Node {
            id: id.into(),
            kind: kind.into(),
            title: id.into(),
            config: config.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }
    }

    fn edge(from: &str, to: &str, outcome: &str, is_loop: bool) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            outcome: outcome.into(),
            is_loop,
            ..Default::default()
        }
    }

    struct H {
        steps: Vec<RunStep>,
    }

    impl H {
        fn new() -> Self {
            H { steps: vec![] }
        }
        fn add(&mut self, node: &str, attempt: i64, status: &str, route: &str, error: &str) {
            let seq = self.steps.len() as i64 + 1;
            self.steps.push(RunStep {
                id: format!("s{seq}"),
                seq,
                node_id: node.into(),
                attempt,
                status: status.into(),
                route: route.into(),
                error: error.into(),
                ..Default::default()
            });
        }
        fn starts(&self, g: &Graph) -> Vec<(String, i64)> {
            plan(g, &self.steps, "")
                .start
                .into_iter()
                .map(|a| (a.node_id, a.attempt))
                .collect()
        }
    }

    fn linear() -> Graph {
        Graph {
            nodes: vec![
                node("t", "trigger", json!({})),
                node("a", "crew", json!({"maxAttempts": 2})),
                node("b", "notify", json!({})),
            ],
            edges: vec![edge("t", "a", "", false), edge("a", "b", "", false)],
            ..Default::default()
        }
    }

    #[test]
    fn walks_a_line() {
        let g = linear();
        let mut h = H::new();
        assert_eq!(h.starts(&g), [("t".into(), 1)]);
        h.add("t", 1, "succeeded", "", "");
        assert_eq!(h.starts(&g), [("a".into(), 1)]);
        h.add("a", 1, "running", "", "");
        assert_eq!(plan(&g, &h.steps, ""), Plan::default());
        h.steps[1].status = "succeeded".into();
        assert_eq!(h.starts(&g), [("b".into(), 1)]);
        h.add("b", 1, "succeeded", "", "");
        assert_eq!(plan(&g, &h.steps, "").finish.unwrap().status, "succeeded");
    }

    #[test]
    fn retries_then_stops() {
        let g = linear();
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("a", 1, "failed", "", "timeout");
        let p = plan(&g, &h.steps, "");
        assert_eq!(
            p.start[0],
            Activation {
                node_id: "a".into(),
                attempt: 2,
                retry: true
            }
        );
        h.add("a", 2, "failed", "", "timeout");
        let f = plan(&g, &h.steps, "").finish.unwrap();
        assert_eq!((f.status, f.reason.as_str()), ("failed", "stopped_line:a"));
        // A step without retries fails the run by its error.
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("a", 1, "succeeded", "", "");
        h.add("b", 1, "failed", "", "integration_missing:board");
        let f = plan(&g, &h.steps, "").finish.unwrap();
        assert_eq!(f.reason, "integration_missing:b");
    }

    #[test]
    fn conditions_route_and_end() {
        let g = Graph {
            nodes: vec![
                node("t", "trigger", json!({})),
                node(
                    "c",
                    "condition",
                    json!({"outcomes":[{"value":"go"},{"value":"stop","end":true}]}),
                ),
                node("x", "notify", json!({})),
            ],
            edges: vec![edge("t", "c", "", false), edge("c", "x", "go", false)],
            ..Default::default()
        };
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("c", 1, "succeeded", "go", "");
        assert_eq!(h.starts(&g), [("x".into(), 1)]);
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("c", 1, "succeeded", "stop", "");
        let f = plan(&g, &h.steps, "").finish.unwrap();
        assert_eq!((f.status, f.reason.as_str()), ("succeeded", "ended:c"));
    }

    #[test]
    fn gate_loop_reruns_crew_once() {
        let g = Graph {
            nodes: vec![
                node("t", "trigger", json!({})),
                node("a", "crew", json!({})),
                node("g", "gate", json!({"onFail":"return_to_crew"})),
                node("o", "notify", json!({})),
            ],
            edges: vec![
                edge("t", "a", "", false),
                edge("a", "g", "", false),
                edge("g", "a", "fail", true),
                edge("g", "o", "pass", false),
            ],
            ..Default::default()
        };
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("a", 1, "succeeded", "", "");
        h.add("g", 1, "failed", "", "gate_failed");
        assert_eq!(h.starts(&g), [("a".into(), 2)]);
        h.add("a", 2, "succeeded", "", "");
        // The gate runs again (its step is older than a's new one).
        assert_eq!(h.starts(&g), [("g".into(), 2)]);
        h.add("g", 2, "succeeded", "pass", "");
        assert_eq!(h.starts(&g), [("o".into(), 1)]);
        // A third failure stops the line (default 3).
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        for i in 1..=3 {
            h.add("a", i, "succeeded", "", "");
            h.add("g", i, "failed", "", "gate_failed");
        }
        let f = plan(&g, &h.steps, "").finish.unwrap();
        assert_eq!(f.reason, "stopped_line:a");
    }

    #[test]
    fn gate_end_run_and_rejection() {
        let g = Graph {
            nodes: vec![
                node("t", "trigger", json!({})),
                node("g", "gate", json!({"onFail":"end_run"})),
                node("h", "human", json!({})),
            ],
            edges: vec![edge("t", "g", "", false), edge("g", "h", "", false)],
            ..Default::default()
        };
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("g", 1, "failed", "", "gate_failed");
        assert_eq!(
            plan(&g, &h.steps, "").finish.unwrap().reason,
            "gate_failed:g"
        );
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("g", 1, "succeeded", "pass", "");
        h.add("h", 1, "failed", "", "rejected");
        let f = plan(&g, &h.steps, "").finish.unwrap();
        assert_eq!((f.status, f.reason.as_str()), ("cancelled", "declined:h"));
    }

    #[test]
    fn join_waits_for_open_peer() {
        let g = Graph {
            nodes: vec![
                node("t", "trigger", json!({})),
                node("a", "job", json!({})),
                node("b", "job", json!({})),
                node("j", "notify", json!({})),
            ],
            edges: vec![
                edge("t", "a", "", false),
                edge("t", "b", "", false),
                edge("a", "j", "", false),
                edge("b", "j", "", false),
            ],
            ..Default::default()
        };
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        assert_eq!(h.starts(&g), [("a".into(), 1), ("b".into(), 1)]);
        h.add("a", 1, "succeeded", "", "");
        h.add("b", 1, "running", "", "");
        assert!(h.starts(&g).is_empty());
        h.steps[2].status = "succeeded".into();
        assert_eq!(h.starts(&g), [("j".into(), 1)]);
    }

    #[test]
    fn starts_at_named_trigger() {
        let g = Graph {
            nodes: vec![
                node("t1", "trigger", json!({})),
                node("t2", "trigger", json!({})),
            ],
            ..Default::default()
        };
        assert_eq!(plan(&g, &[], "t2").start[0].node_id, "t2");
        assert_eq!(plan(&g, &[], "nope").start[0].node_id, "t1");
        assert_eq!(
            plan(&Graph::default(), &[], "").finish.unwrap().reason,
            "no_trigger"
        );
    }

    #[test]
    fn manual_retry_reruns_downstream() {
        // Retry creates a newer attempt of `a`; b's old step is older, so b runs again.
        let g = linear();
        let mut h = H::new();
        h.add("t", 1, "succeeded", "", "");
        h.add("a", 1, "succeeded", "", "");
        h.add("b", 1, "failed", "", "cancelled");
        h.add("a", 2, "succeeded", "", "");
        assert_eq!(h.starts(&g), [("b".into(), 2)]);
    }
}
