//! How a session ended (the website's `notify` reasons and `Derive`), and
//! the role-manager decisions a transcript upload carries.
//!
//! A host that knows says why it ended; an older host sends nothing, and
//! the outcome is worked out from the transcript, strongest signal first:
//! the host's reason, the last turn's error text, a stop the owner asked
//! for, an interrupted last turn, the goal's state, else a clean end.

use super::{Entry, InboxItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One end reason and how it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// The end reason ("completed", "error", "timeout", ...).
    pub reason: &'static str,
    /// The outcome word ("succeeded", "failed", "timeout", ...).
    pub outcome: &'static str,
    /// "info", "notice", "warning".
    pub severity: &'static str,
    pub headline: &'static str,
}

const fn o(
    reason: &'static str,
    outcome: &'static str,
    severity: &'static str,
    headline: &'static str,
) -> Outcome {
    Outcome {
        reason,
        outcome,
        severity,
        headline,
    }
}

/// Every outcome, in a fixed order.
pub const OUTCOMES: [Outcome; 15] = [
    o("completed", "succeeded", "info", "Session completed"),
    o("error", "failed", "warning", "Session failed"),
    o("user_stop", "stopped", "info", "Session stopped"),
    o("interrupted", "interrupted", "info", "Session interrupted"),
    o("timeout", "timeout", "warning", "Session timed out"),
    o(
        "budget_exceeded",
        "budget_exceeded",
        "warning",
        "Token budget exhausted",
    ),
    o("max_turns", "max_turns", "notice", "Turn limit reached"),
    o(
        "rate_limited",
        "rate_limited",
        "warning",
        "Provider rate limit",
    ),
    o(
        "quota_exceeded",
        "quota_exceeded",
        "warning",
        "Provider quota exhausted",
    ),
    o(
        "auth_failed",
        "auth_failed",
        "warning",
        "Provider key rejected",
    ),
    o("goal_complete", "goal_complete", "info", "Goal complete"),
    o("goal_paused", "goal_paused", "notice", "Goal paused"),
    o("superseded", "superseded", "info", "Session replaced"),
    o("host_shutdown", "host_shutdown", "notice", "Host shut down"),
    o("lost", "lost", "warning", "Session lost"),
];

/// The outcome of `reason`; an unknown reason reads as completed, the
/// quietest, so a host typo never alarms.
pub fn outcome_of(reason: &str) -> Outcome {
    OUTCOMES
        .iter()
        .find(|o| o.reason == reason)
        .copied()
        .unwrap_or(OUTCOMES[0])
}

/// Host words Keleustes already uses for the same reasons.
fn normalise(reason: &str) -> &str {
    match reason.trim() {
        "stop" | "stopped" | "user_stop" | "owner_stop" | "killed" => "user_stop",
        "done" | "ok" | "completed" | "complete" | "exited" => "completed",
        "failed" | "error" => "error",
        "shutdown" | "host_shutdown" => "host_shutdown",
        r => r,
    }
}

/// The reason an error text names, or "" when none of the specific ones.
pub fn classify_error(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    if t.trim().is_empty() {
        return "";
    }
    let any = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if any(&[
        "quota",
        "credit balance",
        "billing",
        "exceeded your current",
    ]) {
        "quota_exceeded"
    } else if any(&[
        "429",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "overloaded",
        "529",
    ]) {
        "rate_limited"
    } else if any(&[
        "401",
        "403",
        "invalid api key",
        "invalid_api_key",
        "invalid key",
        "unauthorized",
        "unauthorised",
        "authentication",
    ]) {
        "auth_failed"
    } else if any(&[
        "timed out",
        "timeout",
        "time out",
        "deadline exceeded",
        "wall budget",
        "took too long",
    ]) {
        "timeout"
    } else if any(&["token budget", "budget exceeded", "budget exhausted"]) {
        "budget_exceeded"
    } else if any(&[
        "max turns",
        "max_turns",
        "maximum turns",
        "max passes",
        "max iterations",
        "turn limit",
    ]) {
        "max_turns"
    } else {
        ""
    }
}

fn meta(e: &Entry) -> Value {
    serde_json::from_str(&e.meta_json).unwrap_or(Value::Null)
}

fn meta_type(m: &Value) -> &str {
    m.get("type").and_then(Value::as_str).unwrap_or("")
}

/// What the transcript says about the end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndFacts {
    pub host_reason: String,
    /// The last turn's state: started, ended, error, interrupted, superseded.
    pub last_turn: String,
    pub turn_error: String,
    pub goal_status: String,
    pub stop_requested: bool,
    pub ended: bool,
}

impl EndFacts {
    /// Reads the facts from the transcript tail and the inbox.
    pub fn read(host_reason: &str, ended: bool, entries: &[Entry], inbox: &[InboxItem]) -> Self {
        let mut f = EndFacts {
            host_reason: host_reason.to_string(),
            ended,
            ..Default::default()
        };
        for e in entries {
            let m = meta(e);
            match (e.role.as_str(), meta_type(&m)) {
                (_, "turn_state") => {
                    f.last_turn = m
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                }
                (_, "goal_state") => {
                    f.goal_status = m
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                }
                ("error", _) => {
                    f.turn_error = e.text.clone();
                    f.last_turn = "error".into();
                }
                ("system", _) if e.text.starts_with("turn failed:") => {
                    f.turn_error = e.text.clone();
                    f.last_turn = "error".into();
                }
                _ => {}
            }
        }
        f.stop_requested = inbox.iter().any(|i| {
            i.kind == "command"
                && serde_json::from_str::<Value>(&i.payload_json)
                    .ok()
                    .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                    .is_some_and(|n| n == "stop")
        });
        f
    }
}

/// The end reason from the strongest signal (the website's `Derive`).
pub fn derive(f: &EndFacts) -> &'static str {
    let host = normalise(&f.host_reason);
    if f.stop_requested && matches!(host, "interrupted" | "host_shutdown") {
        return "user_stop";
    }
    if let Some(o) = OUTCOMES.iter().find(|o| o.reason == host && host != "lost") {
        if host == "error" {
            let c = classify_error(&f.turn_error);
            if !c.is_empty() {
                return c;
            }
        }
        return o.reason;
    }
    if host == "lost" {
        return "lost";
    }
    match f.last_turn.as_str() {
        "error" => {
            let c = classify_error(&f.turn_error);
            return if c.is_empty() { "error" } else { c };
        }
        "superseded" => return "superseded",
        _ => {}
    }
    if f.stop_requested {
        return "user_stop";
    }
    if f.last_turn == "interrupted" {
        return "interrupted";
    }
    match f.goal_status.as_str() {
        "budget_limited" => return "budget_exceeded",
        "complete" => return "goal_complete",
        "paused" => return "goal_paused",
        _ => {}
    }
    let c = classify_error(&f.host_reason);
    if !c.is_empty() {
        return c;
    }
    if f.ended && f.last_turn == "started" {
        return "interrupted";
    }
    if !f.ended {
        return "lost";
    }
    "completed"
}

/// Session decisions one upload emits at most (the rest stay in the
/// transcript; the last event says how many were left out).
pub const DECISIONS_PER_UPLOAD: usize = 10;

/// A role-manager System One call in a transcript.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionDecision {
    pub entry: i64,
    pub at: i64,
    pub activity: String,
    pub outcome: String,
    pub model: String,
    pub provider: String,
    /// 0..1, when the entry has a score.
    pub confidence: Option<f64>,
}

/// The decision a role-manager entry records, when the feed shows it
/// (not hidden bookkeeping, not a harness entry with no model call).
pub fn decision_of(e: &Entry) -> Option<SessionDecision> {
    let m = meta(e);
    if e.role != "rolemanager" && meta_type(&m) != "rolemanager" {
        return None;
    }
    let s = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    if m.get("hidden").and_then(Value::as_bool) == Some(true) || s("actor_kind") == "harness" {
        return None;
    }
    let outcome = [s("outcome"), s("verdict"), s("activity")]
        .into_iter()
        .find(|x| !x.is_empty())
        .unwrap_or_else(|| "decided".into());
    Some(SessionDecision {
        entry: e.idx,
        at: e.at,
        activity: s("activity"),
        outcome: outcome.chars().take(200).collect(),
        model: s("model"),
        provider: s("provider"),
        confidence: m
            .get("score_pct")
            .and_then(Value::as_f64)
            .filter(|p| (0.0..=100.0).contains(p))
            .map(|p| p / 100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(idx: i64, role: &str, text: &str, meta: &str) -> Entry {
        Entry {
            idx,
            role: role.into(),
            text: text.into(),
            at: idx,
            meta_json: meta.into(),
        }
    }

    #[test]
    fn derive_follows_the_strongest_signal() {
        let f = |host: &str, entries: &[Entry]| derive(&EndFacts::read(host, true, entries, &[]));
        assert_eq!(f("completed", &[]), "completed");
        assert_eq!(f("", &[]), "completed");
        assert_eq!(f("timeout", &[]), "timeout");
        assert_eq!(
            f("error", &[e(1, "error", "HTTP 429 Too Many Requests", "")]),
            "rate_limited"
        );
        assert_eq!(
            f(
                "",
                &[e(1, "system", "turn failed: credit balance too low", "")]
            ),
            "quota_exceeded"
        );
        assert_eq!(
            f(
                "",
                &[e(
                    1,
                    "system",
                    "",
                    r#"{"type":"turn_state","state":"started"}"#
                )]
            ),
            "interrupted"
        );
        assert_eq!(
            f(
                "",
                &[e(
                    1,
                    "system",
                    "",
                    r#"{"type":"goal_state","status":"budget_limited"}"#
                )]
            ),
            "budget_exceeded"
        );
        assert_eq!(f("lost", &[]), "lost");
        assert_eq!(derive(&EndFacts::read("", false, &[], &[])), "lost");
        let stop = InboxItem {
            kind: "command".into(),
            payload_json: r#"{"name":"stop","args":[]}"#.into(),
            ..Default::default()
        };
        assert_eq!(
            derive(&EndFacts::read("interrupted", true, &[], &[stop])),
            "user_stop"
        );
        assert_eq!(outcome_of("budget_exceeded").severity, "warning");
        assert_eq!(outcome_of("nonsense").outcome, "succeeded");
    }

    #[test]
    fn role_manager_entries_are_decisions_unless_hidden() {
        let d = decision_of(&e(
            3,
            "rolemanager",
            "",
            r#"{"activity":"tool check","verdict":"allow","model":"m","score_pct":80}"#,
        ))
        .unwrap();
        assert_eq!((d.outcome.as_str(), d.confidence), ("allow", Some(0.8)));
        assert!(decision_of(&e(4, "rolemanager", "", r#"{"hidden":true}"#)).is_none());
        assert!(decision_of(&e(5, "rolemanager", "", r#"{"actor_kind":"harness"}"#)).is_none());
        assert!(decision_of(&e(6, "assistant", "", "{}")).is_none());
        assert_eq!(
            decision_of(&e(7, "system", "", r#"{"type":"rolemanager"}"#))
                .unwrap()
                .outcome,
            "decided"
        );
    }
}
