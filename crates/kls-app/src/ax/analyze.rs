//! `keleustes analyze-usage` (O-078): mines Keleustes' own sessions,
//! dispatches, inbox and audit ledger for usage friction, sorts it into a
//! fixed taxonomy, and can append it to a JSONL fix queue deduplicated by
//! `findingId` (`kind:subject`). Read-only apart from that file.
//!
//! After Orca's `analyze-orca-usage`: only evidence Keleustes stored is
//! used, never transcript prose; each finding points at the command that
//! shows its evidence.

use anyhow::Result;
use kls_core::Core;
use kls_store::audit::LedgerQuery;
use kls_store::sessions::SessionQuery;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// (kind, axDefect, severity, surface, meaning). axDefect is true, false
/// or "review" (flagged for a person to read, not asserted).
pub const KINDS: [(&str, &str, &str, &str, &str); 13] = [
    (
        "interface_misread",
        "true",
        "medium",
        "contract",
        "a CLI call failed with usage_error, ambiguous, not_found or invalid_state: the contract or next[] did not convey the right call",
    ),
    (
        "cli_timeout",
        "review",
        "low",
        "contract",
        "a CLI wait hit its deadline (budget too short, or no liveness signal)",
    ),
    (
        "answer_refused",
        "true",
        "medium",
        "contract",
        "an answer reached the host and was refused (stale ask, wrong option)",
    ),
    (
        "dispatch_failed",
        "true",
        "high",
        "adapter",
        "the node could not start a dispatch",
    ),
    (
        "dispatch_stuck",
        "review",
        "medium",
        "delivery",
        "a dispatch was never bound to a session (node offline or frame lost)",
    ),
    (
        "stop_before_output",
        "true",
        "high",
        "observability",
        "a session was stopped or failed before any agent output: no liveness signal",
    ),
    (
        "stop_after_progress",
        "review",
        "low",
        "observability",
        "a session was stopped after real output (a misread or a deliberate steer)",
    ),
    (
        "empty_result",
        "true",
        "medium",
        "agent",
        "a session completed without any agent output",
    ),
    (
        "unanswered_ask",
        "review",
        "medium",
        "attention",
        "an ask stayed open with no answer queued",
    ),
    (
        "lost_session",
        "review",
        "medium",
        "observability",
        "a session ended as lost (its node went quiet)",
    ),
    (
        "agent_error",
        "false",
        "low",
        "agent",
        "the agent failed its own task and said so (endReason)",
    ),
    (
        "refused_kill_switch",
        "false",
        "low",
        "environment",
        "work refused by the kill switch or a maintenance window",
    ),
    (
        "refused_policy",
        "false",
        "low",
        "environment",
        "work refused by the A2A blackout or a node's policy",
    ),
];

fn kind_row(kind: &str) -> (&'static str, &'static str, &'static str) {
    let k = KINDS.iter().find(|k| k.0 == kind).expect("known kind");
    (k.1, k.2, k.3)
}

pub struct Options {
    pub since_ms: i64,
    pub ax_only: bool,
    pub session: Option<String>,
    /// Asks open longer than this count as unanswered.
    pub ask_after_ms: i64,
    /// Dispatches unbound longer than this count as stuck.
    pub stuck_after_ms: i64,
}

fn finding(
    kind: &str,
    subject_id: &str,
    at: i64,
    subject: Value,
    detail: String,
    pointer: String,
) -> Value {
    let (ax, severity, surface) = kind_row(kind);
    let ax = match ax {
        "true" => json!(true),
        "false" => json!(false),
        r => json!(r),
    };
    json!({
        "findingId": format!("{kind}:{subject_id}"),
        "kind": kind, "axDefect": ax, "severity": severity, "surface": surface,
        "at": at, "subject": subject, "detail": detail, "pointer": pointer,
    })
}

/// The findings, oldest first.
pub async fn analyze(core: &Core, o: &Options, now: i64) -> Result<Vec<Value>> {
    let store = core.store();
    let mut out: Vec<Value> = Vec::new();
    let from = now - o.since_ms;
    let in_scope = |sid: &str| o.session.as_deref().is_none_or(|s| s == sid);

    // Sessions: how they ended, asks left open, answers refused.
    let sessions = store
        .sessions(&SessionQuery {
            limit: 5000,
            ..Default::default()
        })
        .await?;
    for s in sessions
        .iter()
        .filter(|s| s.seen_at.max(s.created_at) >= from && in_scope(&s.id))
    {
        let sub = json!({ "session": s.id, "node": s.node_id, "dispatch": s.dispatch_id });
        let pointer = format!("keleustes sessions transcript {}", s.id);
        let entries = store.entries(&s.id, 0, 50).await?;
        let output = entries.iter().any(|e| e.role != "user") || s.entry_count > 50;
        if let Some(at) = s.ended_at {
            let agent = kls_core::sessions::axes::agent_of_end_reason(&s.end_reason);
            let kind = match (agent, output) {
                ("lost", _) => Some("lost_session"),
                ("stopped", false) | ("failed", false) => Some("stop_before_output"),
                ("stopped", true) => Some("stop_after_progress"),
                ("failed", true) => Some("agent_error"),
                ("completed", false) => Some("empty_result"),
                _ => None,
            };
            if let Some(k) = kind {
                out.push(finding(
                    k,
                    &s.id,
                    at,
                    sub.clone(),
                    format!(
                        "ended {} ({agent}), {} entries",
                        s.end_reason, s.entry_count
                    ),
                    pointer.clone(),
                ));
            }
        }
        let items = store.inbox_items(&s.id, 200).await?;
        for i in items
            .iter()
            .filter(|i| i.kind == "answer" && i.status == "refused")
        {
            out.push(finding(
                "answer_refused",
                &i.id,
                i.acked_at.unwrap_or(i.created_at),
                json!({ "session": s.id, "item": i.id }),
                format!("the host refused the answer: {}", i.detail),
                format!("keleustes sessions show {}", s.id),
            ));
        }
        if s.is_open() && !s.pending_question_id.is_empty() {
            let x = store.session_extras(&s.id).await?.unwrap_or_default();
            let asked = x
                .pending_ask
                .as_ref()
                .map(|a| a.asked_at)
                .filter(|a| *a > 0)
                .unwrap_or(s.updated_at);
            let queued = items.iter().any(|i| {
                i.kind == "answer"
                    && matches!(i.status.as_str(), "pending" | "delivered")
                    && i.payload_json.contains(&s.pending_question_id)
            });
            if !queued && asked > 0 && now - asked > o.ask_after_ms {
                out.push(finding(
                    "unanswered_ask",
                    &format!("{}/{}", s.id, s.pending_question_id),
                    asked,
                    json!({ "session": s.id, "ask": s.pending_question_id }),
                    format!(
                        "open {} min: {}",
                        (now - asked) / 60_000,
                        s.pending_question
                    ),
                    format!("keleustes sessions show {}", s.id),
                ));
            }
        }
    }

    // Dispatches: failed, refused by the node, or never bound.
    if o.session.is_none() {
        for d in store
            .dispatches(None, 5000)
            .await?
            .iter()
            .filter(|d| d.created_at >= from)
        {
            let sub = json!({ "dispatch": d.id, "node": d.node_id, "kind": d.kind });
            let pointer = format!("keleustes sessions show {}", d.id);
            match kls_core::sessions::axes::delivery_of(Some(d)) {
                "refused" => out.push(finding(
                    "refused_policy",
                    &d.id,
                    d.updated_at,
                    sub,
                    d.error.clone(),
                    pointer,
                )),
                "failed" => out.push(finding(
                    "dispatch_failed",
                    &d.id,
                    d.updated_at,
                    sub,
                    d.error.clone(),
                    pointer,
                )),
                "queued" | "delivered"
                    if d.kind == "session" && now - d.created_at > o.stuck_after_ms =>
                {
                    out.push(finding(
                        "dispatch_stuck",
                        &d.id,
                        d.created_at,
                        sub,
                        format!("{} for {} min", d.status, (now - d.created_at) / 60_000),
                        pointer,
                    ))
                }
                _ => {}
            }
        }
    }

    // The ledger: CLI failures and refusals.
    let ledger = core
        .audit_entries(&LedgerQuery {
            kinds: vec!["cli".into(), "kill_switch".into(), "a2a_withheld".into()],
            from_at: Some(from),
            session_id: o.session.clone(),
            limit: 10_000,
            ..Default::default()
        })
        .await?;
    for e in ledger {
        let id = format!("ledger{}", e.seq);
        let pointer = if e.session_id.is_empty() {
            format!("keleustes host verify  # ledger seq {}", e.seq)
        } else {
            format!("keleustes sessions show {}", e.session_id)
        };
        let sub = json!({ "ledger": e.seq, "session": e.session_id, "node": e.node_id });
        let kind = match e.kind.as_str() {
            "kill_switch" => Some("refused_kill_switch"),
            "a2a_withheld" => Some("refused_policy"),
            "cli" => match e.payload["code"].as_str().unwrap_or("") {
                "usage_error" | "ambiguous" | "not_found" | "invalid_state" => {
                    Some("interface_misread")
                }
                "timeout" => Some("cli_timeout"),
                "refused_paused" | "refused_stopped" | "refused_window" => {
                    Some("refused_kill_switch")
                }
                "refused_a2a" | "refused_node" | "refused_policy" => Some("refused_policy"),
                _ => None,
            },
            _ => None,
        };
        if let Some(k) = kind {
            out.push(finding(k, &id, e.at, sub, e.msg.clone(), pointer));
        }
    }
    if o.ax_only {
        out.retain(|f| f["axDefect"] == json!(true));
    }
    out.sort_by_key(|f| f["at"].as_i64());
    Ok(out)
}

/// Appends the findings not already in the JSONL file; returns how many.
pub fn append_findings(path: &std::path::Path, findings: &[Value]) -> Result<usize> {
    let mut have = std::collections::BTreeSet::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for l in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(l)
                && let Some(id) = v["findingId"].as_str()
            {
                have.insert(id.to_string());
            }
        }
    }
    let mut add = String::new();
    let mut n = 0;
    for f in findings {
        let id = f["findingId"].as_str().unwrap_or("").to_string();
        if have.insert(id) {
            add.push_str(&f.to_string());
            add.push('\n');
            n += 1;
        }
    }
    if n > 0 {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(add.as_bytes())?;
    }
    Ok(n)
}

/// Findings per kind.
pub fn counts(findings: &[Value]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for f in findings {
        *m.entry(f["kind"].as_str().unwrap_or("").to_string())
            .or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::sessions::{Dispatch, Entry, Session};

    #[tokio::test]
    async fn sorts_friction_into_the_taxonomy() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let now = kls_store::now_ms();
        let s = |id: &str| Session {
            id: id.into(),
            node_id: "n1".into(),
            state: "live".into(),
            ..Default::default()
        };
        for id in ["quiet", "worked", "lost"] {
            core.session_upsert("n1", s(id)).await.unwrap();
        }
        core.session_append(
            "n1",
            "worked",
            &[Entry {
                idx: 0,
                role: "assistant".into(),
                text: "did it".into(),
                ..Default::default()
            }],
        )
        .await
        .unwrap();
        core.session_end("n1", "quiet", 0, "user_stop")
            .await
            .unwrap();
        core.session_end("n1", "worked", 0, "user_stop")
            .await
            .unwrap();
        core.session_end("n1", "lost", 0, "lost").await.unwrap();
        core.store()
            .insert_dispatch(&Dispatch {
                id: "d1".into(),
                node_id: "n1".into(),
                kind: "session".into(),
                status: "queued".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        core.store()
            .resolve_dispatch("d1", "failed", "", "no such cwd")
            .await
            .unwrap();
        let o = Options {
            since_ms: 3_600_000,
            ax_only: false,
            session: None,
            ask_after_ms: 900_000,
            stuck_after_ms: 600_000,
        };
        let f = analyze(&core, &o, now + 1000).await.unwrap();
        let schema = super::super::contract::finding_schema();
        for x in &f {
            super::super::schema::validate(&schema, x).unwrap();
        }
        let ids: Vec<&str> = f.iter().filter_map(|x| x["findingId"].as_str()).collect();
        assert!(ids.contains(&"stop_before_output:quiet"), "{ids:?}");
        assert!(ids.contains(&"stop_after_progress:worked"));
        assert!(ids.contains(&"lost_session:lost"));
        assert!(ids.contains(&"dispatch_failed:d1"));

        // The fix queue dedups by findingId.
        let q = dir.path().join("queue.jsonl");
        assert_eq!(append_findings(&q, &f).unwrap(), f.len());
        assert_eq!(append_findings(&q, &f).unwrap(), 0);
        let ax = analyze(&core, &Options { ax_only: true, ..o }, now + 1000)
            .await
            .unwrap();
        assert!(ax.iter().all(|x| x["axDefect"] == json!(true)));
        assert!(!ax.is_empty());
    }
}
