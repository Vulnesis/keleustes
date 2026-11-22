//! The SystemOne decision node (vdb-site `factory_engine_node_decision.go`,
//! docs/factory-systemone.md), run as a `model` job with `task: systemone`
//! on a node (belai `cmd/belai/factoryjobs.go` `modelJobSystemOne`).
//!
//! The engine builds the state from ids and counts only (never transcript
//! text), asks one `choice` question over the node's outcomes, and applies
//! the threshold and abstain rules here. belai answers `calibrated: false`,
//! so every threshold is treated as uncalibrated. A malformed answer fails
//! closed (the abstain action).

use crate::graph::Node;
use kls_store::engine::{Run, RunStep};
use serde_json::{Value, json};

/// The question key in the job payload.
pub const QUESTION: &str = "decide";
pub const BELOW_THRESHOLD: &str = "$below_threshold";
pub const UNKNOWN: &str = "$unknown";
pub const DEFAULT_THRESHOLD: f64 = 0.7;
/// What to do below the threshold.
pub const BELOW_ACTIONS: [&str; 4] = ["route", "safest", "human", "end_run"];
/// What to do when the model abstains (job failed or answer malformed).
pub const ABSTAIN_ACTIONS: [&str; 3] = ["route", "human", "end_run"];

/// What the engine does with an answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Take the edge with this outcome.
    Route(String),
    /// Ask a person, suggesting this outcome (may be empty).
    Human(String),
    /// Fail the step with this error.
    Fail(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub action: Action,
    /// The model's top choice ("" when it abstained).
    pub choice: String,
    pub confidence: f64,
    pub threshold: f64,
    /// `chosen`, `below_threshold`, `abstained`.
    pub why: &'static str,
    pub provider: String,
    pub model: String,
}

pub fn outcome_values(n: &Node) -> Vec<String> {
    n.outcomes()
        .into_iter()
        .map(|o| o.value)
        .filter(|v| !v.starts_with('$'))
        .collect()
}

pub fn threshold(n: &Node, setting: Option<f64>) -> f64 {
    n.config
        .get("threshold")
        .and_then(Value::as_f64)
        .or(setting)
        .unwrap_or(DEFAULT_THRESHOLD)
        .clamp(0.0, 1.0)
}

/// The job payload: `{task, state, questions}`.
pub fn payload(n: &Node, state: Value) -> Value {
    let descriptions = n
        .config
        .get("descriptions")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    json!({
        "task": "systemone",
        "state": state,
        "questions": {
            QUESTION: {
                "type": "choice",
                "instructions": n.str("question"),
                "options": outcome_values(n),
                "descriptions": descriptions,
            }
        }
    })
}

/// The decision state: ids, kinds, statuses and counts of the run so far.
pub fn state(run: &Run, steps: &[RunStep], workflow: &str) -> Value {
    let event: Value = serde_json::from_str(&run.event_json).unwrap_or(Value::Null);
    let mut by_node = serde_json::Map::new();
    for s in steps {
        by_node.insert(
            s.node_id.clone(),
            json!({ "type": s.node_type, "status": s.status, "route": s.route, "attempt": s.attempt, "error": s.error }),
        );
    }
    json!({
        "workflow": workflow,
        "trigger": run.trigger_kind,
        "repo": run.repo,
        "cardId": run.card_id,
        "event": { "kind": event.get("kind"), "subject": event.get("subject") },
        "steps": by_node,
        "stepCount": steps.len(),
    })
}

fn below(n: &Node, choice: &str) -> Action {
    match n.str("belowThreshold") {
        "route" => Action::Route(BELOW_THRESHOLD.into()),
        "safest" => match n.str("safest") {
            "" => Action::Fail("below_threshold".into()),
            s => Action::Route(s.into()),
        },
        "end_run" => Action::Fail("below_threshold".into()),
        // Default: a person decides, with the model's pick suggested.
        _ => Action::Human(choice.into()),
    }
}

fn abstain(n: &Node) -> Action {
    match n.str("onAbstain") {
        "route" => Action::Route(UNKNOWN.into()),
        "human" => Action::Human(String::new()),
        _ => Action::Fail("abstained".into()),
    }
}

/// Applies the threshold and abstain rules to the job's output (`None`
/// when the job failed).
pub fn apply(n: &Node, output: Option<&Value>, threshold: f64) -> Applied {
    let id = |k: &str| {
        output
            .and_then(|o| o.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut a = Applied {
        action: abstain(n),
        choice: String::new(),
        confidence: 0.0,
        threshold,
        why: "abstained",
        provider: id("provider"),
        model: id("model"),
    };
    let options = outcome_values(n);
    let Some(ans) = output.and_then(|o| o.pointer(&format!("/answers/{QUESTION}"))) else {
        return a;
    };
    let choice = ans.get("choice").and_then(Value::as_str).unwrap_or("");
    let p = ans
        .get("probabilities")
        .and_then(|m| m.get(choice))
        .and_then(Value::as_f64);
    let (Some(p), true) = (p, options.iter().any(|o| o == choice)) else {
        return a;
    };
    if !(0.0..=1.0).contains(&p) {
        return a;
    }
    a.choice = choice.into();
    a.confidence = p;
    if p >= threshold {
        a.action = Action::Route(choice.into());
        a.why = "chosen";
    } else {
        a.action = below(n, choice);
        a.why = "below_threshold";
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(cfg: Value) -> Node {
        let mut n = Node::new("d", "decision", "Decide");
        let mut base = json!({
            "question": "Is this a security fix?",
            "outcomes": [{"value": "yes"}, {"value": "no"}],
        });
        for (k, v) in cfg.as_object().unwrap() {
            base[k] = v.clone();
        }
        n.config = base.as_object().unwrap().clone();
        n
    }

    fn answer(choice: &str, p: f64) -> Value {
        json!({ "answers": { "decide": { "type": "choice", "choice": choice,
            "probabilities": { choice: p } } }, "calibrated": false, "provider": "anthropic", "model": "m" })
    }

    #[test]
    fn payload_is_one_choice_question() {
        let n = node(json!({}));
        let p = payload(&n, json!({"x": 1}));
        assert_eq!(p["task"], "systemone");
        assert_eq!(p["questions"]["decide"]["options"], json!(["yes", "no"]));
        assert_eq!(p["questions"]["decide"]["type"], "choice");
    }

    #[test]
    fn threshold_and_abstain_actions() {
        let n = node(json!({}));
        let a = apply(&n, Some(&answer("yes", 0.9)), 0.7);
        assert_eq!(
            (a.action, a.why, a.provider.as_str()),
            (Action::Route("yes".into()), "chosen", "anthropic")
        );
        // Below: default asks a person with the pick suggested.
        let a = apply(&n, Some(&answer("no", 0.5)), 0.7);
        assert_eq!(a.action, Action::Human("no".into()));
        assert_eq!(
            apply(
                &node(json!({"belowThreshold": "route"})),
                Some(&answer("no", 0.5)),
                0.7
            )
            .action,
            Action::Route(BELOW_THRESHOLD.into())
        );
        assert_eq!(
            apply(
                &node(json!({"belowThreshold": "safest", "safest": "no"})),
                Some(&answer("yes", 0.1)),
                0.7
            )
            .action,
            Action::Route("no".into())
        );
        assert_eq!(
            apply(
                &node(json!({"belowThreshold": "end_run"})),
                Some(&answer("yes", 0.1)),
                0.7
            )
            .action,
            Action::Fail("below_threshold".into())
        );
        // Abstain: a failed job or a malformed answer fails closed by default.
        assert_eq!(
            apply(&n, None, 0.7).action,
            Action::Fail("abstained".into())
        );
        assert_eq!(
            apply(&n, Some(&json!({"answers": {}})), 0.7).why,
            "abstained"
        );
        assert_eq!(
            apply(&n, Some(&answer("maybe", 0.99)), 0.7).why,
            "abstained",
            "unknown option"
        );
        assert_eq!(apply(&n, Some(&answer("yes", 1.5)), 0.7).why, "abstained");
        assert_eq!(
            apply(&node(json!({"onAbstain": "route"})), None, 0.7).action,
            Action::Route(UNKNOWN.into())
        );
        assert_eq!(
            apply(&node(json!({"onAbstain": "human"})), None, 0.7).action,
            Action::Human(String::new())
        );
        assert_eq!(threshold(&node(json!({"threshold": 0.4})), Some(0.9)), 0.4);
        assert_eq!(threshold(&n, Some(0.9)), 0.9);
        assert_eq!(threshold(&n, None), DEFAULT_THRESHOLD);
    }

    #[test]
    fn state_has_no_text() {
        let run = Run {
            id: "r".into(),
            event_json:
                r#"{"kind":"github.pull.opened","subject":"a/b#1","data":{"title":"secret words"}}"#
                    .into(),
            ..Default::default()
        };
        let s = state(&run, &[], "WF");
        assert!(!s.to_string().contains("secret words"));
        assert_eq!(s["event"]["subject"], "a/b#1");
    }
}
