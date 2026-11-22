//! Three separate status axes for a session and a run (Orca O-061): was
//! the work **delivered** to the node, is the **agent** done, and was the
//! result **verified**. Each axis has its own evidence and none is inferred
//! from another or from transcript prose:
//!
//! - `delivery`: the dispatch row (queued, acked by the node, bound to a
//!   session, refused or failed).
//! - `agent`: the session row (state, open ask) and the host's closed
//!   end-reason enum (belai `sessionsync.End*`).
//! - `verified`: the host's `gate.verified` audit lines only. With none it
//!   stays `unknown`: Keleustes never treats "the agent said it is done" as
//!   verification.

use crate::Core;
use anyhow::Result;
use kls_store::engine::{Run, RunStep};
use kls_store::sessions::{AuditLine, Dispatch, Session};
use serde::{Deserialize, Serialize};

/// `delivery` values.
pub const DELIVERY: [&str; 6] = [
    "queued",
    "delivered",
    "started",
    "refused",
    "failed",
    "unknown",
];
/// `agent` values.
pub const AGENT: [&str; 9] = [
    "pending",
    "running",
    "asking",
    "idle",
    "completed",
    "stopped",
    "failed",
    "lost",
    "ended",
];
/// `verified` values.
pub const VERIFIED: [&str; 3] = ["unknown", "pass", "fail"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusAxes {
    /// One of [`DELIVERY`].
    pub delivery: String,
    /// One of [`AGENT`].
    pub agent: String,
    /// One of [`VERIFIED`].
    pub verified: String,
}

impl StatusAxes {
    /// "delivered · agent done · verified" style one-liner for screens.
    pub fn label(&self) -> String {
        format!(
            "delivery {} · agent {} · verified {}",
            self.delivery, self.agent, self.verified
        )
    }
}

/// The agent axis for an ended session, from belai's closed end-reason
/// enum (`internal/sessionsync/client.go`) plus Keleustes' own `lost`.
pub fn agent_of_end_reason(reason: &str) -> &'static str {
    match reason {
        "completed" | "goal_complete" => "completed",
        "interrupted" | "user_stop" | "host_shutdown" | "superseded" | "goal_paused" => "stopped",
        "error" | "timeout" | "budget_exceeded" | "max_turns" | "rate_limited"
        | "quota_exceeded" | "auth_failed" => "failed",
        "lost" | "revoked" => "lost",
        _ => "ended",
    }
}

/// The delivery axis of a dispatch row.
pub fn delivery_of(d: Option<&Dispatch>) -> &'static str {
    let Some(d) = d else { return "unknown" };
    if !d.session_id.is_empty() {
        return "started";
    }
    match d.status.as_str() {
        "queued" if d.delivered_at.is_some() => "delivered",
        "queued" => "queued",
        "delivered" => "delivered",
        "started" => "started",
        "refused" => "refused",
        // The node's refusal is stored as a failure with this prefix
        // (`Core::complete`).
        "failed" if d.error.starts_with("refused") => "refused",
        "failed" => "failed",
        _ => "unknown",
    }
}

/// The verified axis from a session's host audit lines.
pub fn verified_of(lines: &[AuditLine]) -> &'static str {
    let mut pass = false;
    for l in lines.iter().filter(|l| l.kind == "gate.verified") {
        let v: serde_json::Value = serde_json::from_str(&l.json).unwrap_or_default();
        match v.get("outcome").and_then(|o| o.as_str()).unwrap_or("") {
            "regressed" | "failed" | "fail" | "unmet" => return "fail",
            "met" | "pass" | "passed" | "ok" | "verified" => pass = true,
            _ => {}
        }
    }
    if pass { "pass" } else { "unknown" }
}

/// The three axes of a session (or of a dispatch with no session yet).
pub fn session_axes(s: Option<&Session>, d: Option<&Dispatch>, lines: &[AuditLine]) -> StatusAxes {
    let agent = match s {
        None => match delivery_of(d) {
            "refused" | "failed" => "ended",
            _ => "pending",
        },
        Some(s) if !s.is_open() => agent_of_end_reason(&s.end_reason),
        Some(s) if !s.pending_question_id.is_empty() || s.state == "waiting" => "asking",
        Some(s) if s.state == "idle" => "idle",
        Some(_) => "running",
    };
    StatusAxes {
        delivery: delivery_of(d).into(),
        agent: agent.into(),
        verified: verified_of(lines).into(),
    }
}

/// The three axes of a workflow run: delivery over its dispatching steps,
/// the run status for the agent axis, and its `gate` steps for verified.
pub fn run_axes(run: &Run, steps: &[RunStep], dispatches: &[Dispatch]) -> StatusAxes {
    let with_dispatch: Vec<_> = steps.iter().filter(|s| s.dispatch_id.is_some()).collect();
    let delivery = if with_dispatch.is_empty() {
        "unknown"
    } else {
        let of = |s: &&RunStep| {
            delivery_of(
                dispatches
                    .iter()
                    .find(|d| Some(&d.id) == s.dispatch_id.as_ref()),
            )
        };
        let all: Vec<&str> = with_dispatch.iter().map(of).collect();
        if all.contains(&"refused") {
            "refused"
        } else if all.contains(&"failed") {
            "failed"
        } else if all.contains(&"queued") {
            "queued"
        } else if all.iter().all(|d| *d == "started") {
            "started"
        } else if all.iter().all(|d| matches!(*d, "started" | "delivered")) {
            "delivered"
        } else {
            "unknown"
        }
    };
    let agent = match run.status.as_str() {
        "queued" | "pending" => "pending",
        "running" => "running",
        "waiting_approval" | "paused" => "asking",
        "succeeded" | "completed" => "completed",
        "cancelled" => "stopped",
        "failed" => "failed",
        _ if run.finished_at.is_some() => "ended",
        _ => "running",
    };
    let gates: Vec<_> = steps.iter().filter(|s| s.node_type == "gate").collect();
    let verified = if gates.iter().any(|g| g.status == "failed") {
        "fail"
    } else if !gates.is_empty() && gates.iter().all(|g| g.status == "succeeded") {
        "pass"
    } else {
        "unknown"
    };
    StatusAxes {
        delivery: delivery.into(),
        agent: agent.into(),
        verified: verified.into(),
    }
}

impl Core {
    /// The axes of a stored session.
    pub async fn session_axes(&self, session_id: &str) -> Result<StatusAxes> {
        let s = self.store().session(session_id).await?;
        let d = match s.as_ref().filter(|s| !s.dispatch_id.is_empty()) {
            Some(s) => self.store().dispatch(&s.dispatch_id).await?,
            None => None,
        };
        let lines = match &s {
            Some(s) => self.store().audit_lines(&s.node_id, &s.id, 0, 0).await?,
            None => Vec::new(),
        };
        Ok(session_axes(s.as_ref(), d.as_ref(), &lines))
    }

    /// The axes of a dispatch, following it to its session once bound.
    pub async fn dispatch_axes(&self, dispatch_id: &str) -> Result<StatusAxes> {
        let d = self.store().dispatch(dispatch_id).await?;
        let s = match d.as_ref().filter(|d| !d.session_id.is_empty()) {
            Some(d) => self.store().session(&d.session_id).await?,
            None => None,
        };
        let lines = match &s {
            Some(s) => self.store().audit_lines(&s.node_id, &s.id, 0, 0).await?,
            None => Vec::new(),
        };
        Ok(session_axes(s.as_ref(), d.as_ref(), &lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(outcome: &str) -> AuditLine {
        AuditLine {
            kind: "gate.verified".into(),
            json: serde_json::json!({ "outcome": outcome }).to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn axes_are_independent() {
        let d = Dispatch {
            status: "queued".into(),
            ..Default::default()
        };
        let a = session_axes(None, Some(&d), &[]);
        assert_eq!(
            (a.delivery.as_str(), a.agent.as_str()),
            ("queued", "pending")
        );
        assert_eq!(a.verified, "unknown");

        let d = Dispatch {
            status: "started".into(),
            session_id: "s1".into(),
            ..Default::default()
        };
        let mut s = Session {
            id: "s1".into(),
            state: "waiting".into(),
            pending_question_id: "q1".into(),
            ..Default::default()
        };
        assert_eq!(session_axes(Some(&s), Some(&d), &[]).agent, "asking");
        s.pending_question_id.clear();
        s.state = "ended".into();
        s.ended_at = Some(1);
        s.end_reason = "completed".into();
        // Agent done is not verified.
        let a = session_axes(Some(&s), Some(&d), &[]);
        assert_eq!(
            (a.delivery.as_str(), a.agent.as_str(), a.verified.as_str()),
            ("started", "completed", "unknown")
        );
        assert_eq!(
            session_axes(Some(&s), Some(&d), &[line("met")]).verified,
            "pass"
        );
        assert_eq!(
            session_axes(Some(&s), Some(&d), &[line("met"), line("regressed")]).verified,
            "fail"
        );
        s.end_reason = "user_stop".into();
        assert_eq!(session_axes(Some(&s), None, &[]).agent, "stopped");
        assert_eq!(session_axes(Some(&s), None, &[]).delivery, "unknown");
        let refused = Dispatch {
            status: "failed".into(),
            error: "refused: policy".into(),
            ..Default::default()
        };
        let a = session_axes(None, Some(&refused), &[]);
        assert_eq!(
            (a.delivery.as_str(), a.agent.as_str()),
            ("refused", "ended")
        );
        for v in [a.delivery, a.agent, a.verified] {
            assert!(
                DELIVERY.contains(&v.as_str())
                    || AGENT.contains(&v.as_str())
                    || VERIFIED.contains(&v.as_str())
            );
        }
    }

    #[test]
    fn run_axes_use_steps() {
        let run = Run {
            status: "succeeded".into(),
            ..Default::default()
        };
        let steps = vec![
            RunStep {
                node_type: "agent".into(),
                dispatch_id: Some("d1".into()),
                status: "succeeded".into(),
                ..Default::default()
            },
            RunStep {
                node_type: "gate".into(),
                status: "succeeded".into(),
                ..Default::default()
            },
        ];
        let d = vec![Dispatch {
            id: "d1".into(),
            status: "started".into(),
            session_id: "s".into(),
            ..Default::default()
        }];
        let a = run_axes(&run, &steps, &d);
        assert_eq!(
            (a.delivery.as_str(), a.agent.as_str(), a.verified.as_str()),
            ("started", "completed", "pass")
        );
        let a = run_axes(&run, &steps[..1], &d);
        assert_eq!(a.verified, "unknown");
    }
}
