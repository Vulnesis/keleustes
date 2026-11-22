//! The session view every session verb returns (`session` in the
//! envelope), its next[] commands, and the waits (`--wait-for`,
//! `sessions wait`).

use super::{AxError, Code, q};
use anyhow::Result;
use kls_core::{Core, DispatchSpec};
use serde_json::{Value, json};
use std::time::Duration;

/// A session (or a dispatch not yet bound to one) at one moment.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub view: Value,
    pub session_id: String,
    pub agent: String,
    pub delivery: String,
    /// Not open any more.
    pub terminal: bool,
    /// The open ask has no answer queued for it yet.
    pub ask_live: bool,
    /// Idle with nothing of the caller's waiting to be read: the turn is over.
    pub turn_done: bool,
}

/// What a wait waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Want {
    /// The node started a session (or it ended without one).
    Started,
    /// The session needs the caller: an open ask, a finished turn, or the end.
    Ask,
    /// The turn finished or the session ended (an open ask keeps waiting:
    /// someone else may answer it).
    Done,
    /// The session ended.
    Ended,
}

impl Want {
    pub fn name(self) -> &'static str {
        match self {
            Want::Started => "started",
            Want::Ask => "ask",
            Want::Done => "done",
            Want::Ended => "ended",
        }
    }
}

/// The session (by session id, or by the dispatch id from the handle line).
pub async fn snapshot(core: &Core, id: &str) -> Result<Snapshot> {
    let store = core.store();
    let (session, dispatch) = match store.session(id).await? {
        Some(s) => {
            let d = if s.dispatch_id.is_empty() {
                None
            } else {
                store.dispatch(&s.dispatch_id).await?
            };
            (Some(s), d)
        }
        None => match store.dispatch(id).await? {
            Some(d) => {
                let s = if d.session_id.is_empty() {
                    None
                } else {
                    store.session(&d.session_id).await?
                };
                (s, Some(d))
            }
            None => {
                return Err(super::fail(
                    Code::NotFound,
                    format!("no session or dispatch {id}"),
                ));
            }
        },
    };
    let lines = match &session {
        Some(s) => store.audit_lines(&s.node_id, &s.id, 0, 0).await?,
        None => Vec::new(),
    };
    let axes = kls_core::sessions::axes::session_axes(session.as_ref(), dispatch.as_ref(), &lines);
    let spec = dispatch
        .as_ref()
        .and_then(|d| serde_json::from_str::<DispatchSpec>(&d.spec_json).ok());
    let label = spec
        .as_ref()
        .and_then(|s| s.trigger.as_ref())
        .map(|t| t.subject.clone())
        .unwrap_or_default();
    let extras = match &session {
        Some(s) => store.session_extras(&s.id).await?.unwrap_or_default(),
        None => Default::default(),
    };
    // Asks already answered (an answer queued or applied; a refused one
    // leaves the ask live), and input the host has not reported on since:
    // pending or delivered, or acked after the host's last upsert or append
    // (both server clocks).
    let mut answered: Vec<String> = Vec::new();
    let mut unsettled_prompt = false;
    if let Some(s) = &session {
        for i in store.inbox_items(&s.id, 50).await? {
            let open = matches!(i.status.as_str(), "pending" | "delivered");
            if (open || i.acked_at.is_some_and(|a| a > s.seen_at))
                && matches!(i.kind.as_str(), "prompt" | "answer")
            {
                unsettled_prompt = true;
            }
            if (open || i.status == "done") && i.kind == "answer" {
                let v: Value = serde_json::from_str(&i.payload_json).unwrap_or_default();
                answered.push(v["questionId"].as_str().unwrap_or("").to_string());
            }
        }
    }
    let last_role = match &session {
        Some(s) if s.next_index > 0 => store
            .entries(&s.id, s.next_index - 1, 1)
            .await?
            .pop()
            .map(|e| e.role)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let ask = extras.pending_ask.as_ref().map(|a| {
        json!({
            "id": a.id, "kind": a.kind, "title": a.title, "tool": a.tool,
            "options": a.options.iter().map(|o| json!({"id": o.id, "label": o.label})).collect::<Vec<_>>(),
            "questionsJson": a.questions_json,
            "answerQueued": answered.contains(&a.id),
        })
    });
    let ask_id = extras
        .pending_ask
        .as_ref()
        .map(|a| a.id.clone())
        .or_else(|| {
            session
                .as_ref()
                .map(|s| s.pending_question_id.clone())
                .filter(|q| !q.is_empty())
        });
    let terminal = session.as_ref().is_some_and(|s| !s.is_open())
        || (session.is_none() && matches!(axes.delivery.as_str(), "refused" | "failed"));
    let ask_live = axes.agent == "asking" && ask_id.is_some_and(|id| !answered.contains(&id));
    let turn_done = axes.agent == "idle" && !unsettled_prompt && last_role != "user";
    let harness = extras.harness.clone().unwrap_or_default();
    let view = json!({
        "id": session.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
        "dispatchId": dispatch.as_ref().map(|d| d.id.clone()).or_else(|| session.as_ref().map(|s| s.dispatch_id.clone())).unwrap_or_default(),
        "nodeId": session.as_ref().map(|s| s.node_id.clone()).or_else(|| dispatch.as_ref().map(|d| d.node_id.clone())).unwrap_or_default(),
        "label": label,
        "title": session.as_ref().map(|s| s.title.clone()).unwrap_or_default(),
        "cwd": session.as_ref().map(|s| s.cwd.clone()).or_else(|| spec.as_ref().map(|s| s.cwd.clone())).unwrap_or_default(),
        "state": session.as_ref().map(|s| s.state.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| if session.is_some() { "live".into() } else { "pending".into() }),
        "endReason": session.as_ref().map(|s| s.end_reason.clone()).unwrap_or_default(),
        "dispatchError": dispatch.as_ref().map(|d| d.error.clone()).unwrap_or_default(),
        "nextIndex": session.as_ref().map(|s| s.next_index).unwrap_or(0),
        "ask": ask,
        "delivery": axes.delivery,
        "agent": axes.agent,
        "verified": axes.verified,
        "harness": harness.harness,
        "harnessSessionId": harness.session_id,
    });
    Ok(Snapshot {
        session_id: view["id"].as_str().unwrap_or("").to_string(),
        agent: axes.agent,
        delivery: axes.delivery,
        terminal,
        ask_live,
        turn_done,
        view,
    })
}

/// The literal follow-up commands for a session view. `'<text>'` and
/// `'<json>'` mark content the caller writes; everything else is literal.
pub fn next_for(v: &Value) -> Vec<String> {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let (sid, did) = (s("id"), s("dispatchId"));
    let target = if sid.is_empty() {
        did.clone()
    } else {
        sid.clone()
    };
    let since = match v["nextIndex"].as_i64().unwrap_or(0) {
        n if n > 0 => format!(" --since {}", n - 1),
        _ => String::new(),
    };
    let wait = format!("keleustes sessions wait {target} --for ask --timeout 10m");
    match v["agent"].as_str().unwrap_or("") {
        "pending" => vec![wait, format!("keleustes sessions show {target}")],
        "running" => vec![wait, format!("keleustes sessions transcript {sid}{since}")],
        "asking" => {
            let ask = &v["ask"];
            let id = ask["id"].as_str().unwrap_or("");
            if ask.is_null() || ask["answerQueued"].as_bool() == Some(true) || id.is_empty() {
                return vec![wait, format!("keleustes sessions show {sid}")];
            }
            let mut out: Vec<String> = match ask["kind"].as_str().unwrap_or("") {
                "clarify" | "mode_choice" => vec![format!(
                    "keleustes sessions answer {sid} {} --payload '<json>'",
                    q(id)
                )],
                _ => ask["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o["id"].as_str())
                    .map(|o| format!("keleustes sessions answer {sid} {} {}", q(id), q(o)))
                    .collect(),
            };
            out.push(format!("keleustes sessions show {sid}"));
            out
        }
        "idle" => vec![
            format!("keleustes sessions prompt {sid} '<text>'"),
            format!("keleustes sessions transcript {sid}{since}"),
            format!("keleustes sessions command {sid} stop"),
        ],
        _ => {
            let mut out = Vec::new();
            if !sid.is_empty() {
                out.push(format!("keleustes sessions transcript {sid}"));
            }
            let (h, hs) = (s("harness"), s("harnessSessionId"));
            if !h.is_empty() && !hs.is_empty() {
                out.push(format!(
                    "keleustes sessions dispatch --node {} --cwd {} --harness {} --resume {} --prompt '<text>'",
                    q(&s("nodeId")),
                    q(&s("cwd")),
                    q(&h),
                    q(&hs)
                ));
            }
            out
        }
    }
}

/// Waits until `want` holds for `id` (a session or dispatch id) or the
/// deadline passes. `retry` is the command that waits again.
pub async fn wait(
    core: &Core,
    id: &str,
    want: Want,
    deadline: tokio::time::Instant,
    retry: &str,
) -> Result<Snapshot> {
    loop {
        let snap = snapshot(core, id).await?;
        if snap.session_id.is_empty() {
            match snap.delivery.as_str() {
                "refused" => {
                    return Err(AxError::new(
                        Code::RefusedNode,
                        format!(
                            "the node refused the dispatch: {}",
                            snap.view["dispatchError"].as_str().unwrap_or("")
                        ),
                    )
                    .session(snap.view.clone())
                    .into());
                }
                "failed" => {
                    return Err(AxError::new(
                        Code::DispatchFailed,
                        format!(
                            "the dispatch failed: {}",
                            snap.view["dispatchError"].as_str().unwrap_or("")
                        ),
                    )
                    .session(snap.view.clone())
                    .into());
                }
                _ => {}
            }
        }
        let bound = !snap.session_id.is_empty();
        let satisfied = match want {
            Want::Started => bound || snap.terminal,
            Want::Ask => snap.ask_live || snap.turn_done || snap.terminal,
            Want::Done => snap.turn_done || snap.terminal,
            Want::Ended => snap.terminal,
        };
        if satisfied {
            if snap.terminal && matches!(snap.agent.as_str(), "failed" | "lost") {
                return Err(AxError::new(
                    Code::AgentFailed,
                    format!(
                        "the session ended: {} ({})",
                        snap.agent,
                        snap.view["endReason"].as_str().unwrap_or("")
                    ),
                )
                .next(next_for(&snap.view))
                .session(snap.view.clone())
                .into());
            }
            return Ok(snap);
        }
        if tokio::time::Instant::now() >= deadline {
            let mut next = vec![retry.to_string()];
            next.extend(next_for(&snap.view));
            // A timeout never stops the session; stopping is the caller's call.
            if !snap.session_id.is_empty() && !snap.terminal {
                next.push(format!(
                    "keleustes sessions command {} stop",
                    snap.session_id
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            next.retain(|n| seen.insert(n.clone()));
            return Err(AxError::new(
                Code::Timeout,
                format!(
                    "still {} (delivery {}) when the deadline passed waiting for {}",
                    snap.agent,
                    snap.delivery,
                    want.name()
                ),
            )
            .next(next)
            .session(snap.view.clone())
            .into());
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(left.min(Duration::from_millis(250))).await;
    }
}
