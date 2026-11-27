//! Domain: the belai counterparts (docs/gaps/belai-counterparts.md) that do
//! not belong to one older area: a session's structured ask and what it
//! accepts (WP-A). The node server feeds it; the UI answers through it.
//!
//! Every string a node sends is untrusted. Nothing here interprets a
//! title, a tool name or a questionnaire beyond storing and checking ids.

use crate::Core;
use crate::sessions::{InboxBody, InboxItem, Refusal};
use anyhow::Result;
use kls_store::audit::LedgerDraft;
use kls_store::vault::UsageRow;
use serde_json::json;

pub use kls_store::belai::{
    Accepts, AskOption, Checkpoint, GitFacts, ModelUsage, PendingAsk, SessionExtras, SessionHarness,
};

pub mod attention;
pub mod budget;
pub mod fleet;
pub mod forge;
pub mod initdraft;
pub mod knowledge;
pub mod library;
pub mod plan;
pub mod review;
pub mod schedules;
pub mod status;

pub mod kinds {
    /// A session's ask, accepts, git facts or usage changed. Subject: session id.
    pub const SESSION_EXTRAS: &str = "session.extras";
    /// A structured answer was queued (or refused). Subject: session id.
    pub const SESSION_ANSWER: &str = "session.answer";
}

/// Whether `host` offers agent profile `name`. belai advertises session
/// agents in `agents` and worker profiles (every built-in worker) only as
/// `capabilities.agent_routes`, so both count.
pub fn offers_agent(host: &kls_store::HostInfo, name: &str) -> bool {
    host.agents.iter().any(|a| a == name)
        || host
            .capabilities
            .agent_routes
            .iter()
            .any(|r| r.name == name)
}

/// Whether `host` advertised any agent profile at all.
pub fn advertises_agents(host: &kls_store::HostInfo) -> bool {
    !host.agents.is_empty() || !host.capabilities.agent_routes.is_empty()
}

/// Longest title or tool name kept from a node.
const MAX_TITLE: usize = 300;
/// Most options one ask may list.
const MAX_OPTIONS: usize = 16;
/// Largest questionnaire kept.
const MAX_QUESTIONS: usize = 64 * 1024;
/// Largest structured answer payload queued.
const MAX_PAYLOAD: usize = 32 * 1024;
/// Largest relay draft kept, and the largest edited note queued (#45).
const MAX_RELAY: usize = 64 * 1024;

/// What one upsert (or end) says beyond the session row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtrasUpdate {
    /// The open ask; `None` means none is open now.
    pub pending_ask: Option<PendingAsk>,
    /// `None` leaves the stored value (an older host sends none).
    pub accepts: Option<Accepts>,
    pub git: Option<GitFacts>,
    /// Cumulative per-model usage; empty leaves the stored rows.
    pub usage: Vec<ModelUsage>,
    /// An end's rollup counts; `None` on an upsert.
    pub counts: Option<(i64, i64)>,
    /// The session-level totals an older host sends instead of `usage`.
    pub legacy: Option<(i64, i64, f64)>,
    /// The harness that runs the session; `None` leaves the stored value.
    pub harness: Option<SessionHarness>,
    /// A harness session's end facts (SessionEnd); `None` leaves them.
    pub harness_end: Option<kls_store::belai::HarnessEnd>,
    /// File checkpoints (#45); `None` leaves the stored list.
    pub checkpoints: Option<Vec<Checkpoint>>,
    /// The plan file's SHA-256 (#52, SessionEnd); `None` leaves it.
    pub plan_sha256: Option<String>,
}

fn cap(s: &str, n: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(n)
        .collect::<String>()
}

fn clean_ask(mut a: PendingAsk) -> Option<PendingAsk> {
    a.id = cap(a.id.trim(), 64);
    if a.id.is_empty() {
        return None;
    }
    a.kind = cap(a.kind.trim(), 32);
    a.title = cap(&a.title, MAX_TITLE);
    a.tool = cap(&a.tool, 120);
    a.options.truncate(MAX_OPTIONS);
    a.options.retain(|o| !o.id.trim().is_empty());
    for o in &mut a.options {
        o.id = cap(o.id.trim(), 64);
        o.label = cap(&o.label, 120);
    }
    if a.questions_json.len() > MAX_QUESTIONS
        || (!a.questions_json.is_empty()
            && serde_json::from_str::<serde_json::Value>(&a.questions_json).is_err())
    {
        a.questions_json.clear();
    }
    if a.relay_json.len() > MAX_RELAY
        || (!a.relay_json.is_empty()
            && serde_json::from_str::<serde_json::Value>(&a.relay_json).is_err())
    {
        a.relay_json.clear();
    }
    Some(a)
}

/// A checkpoint list as kept: at most 50, ids and labels capped.
fn clean_checkpoints(list: Vec<Checkpoint>) -> Vec<Checkpoint> {
    list.into_iter()
        .filter(|c| !c.id.trim().is_empty())
        .take(50)
        .map(|c| Checkpoint {
            id: cap(c.id.trim(), 64),
            label: cap(&c.label, 120),
            entry_id: cap(c.entry_id.trim(), 64),
            entry_index: c.entry_index.max(0),
            ..c
        })
        .collect()
}

fn clean_usage(u: &[ModelUsage]) -> Vec<ModelUsage> {
    u.iter()
        .take(64)
        .map(|u| ModelUsage {
            provider: cap(&u.provider, 64),
            model: cap(&u.model, 128),
            input_tokens: u.input_tokens.max(0),
            output_tokens: u.output_tokens.max(0),
            cache_read_tokens: u.cache_read_tokens.max(0),
            cache_write_tokens: u.cache_write_tokens.max(0),
            cost_usd: if u.cost_usd.is_finite() && u.cost_usd > 0.0 {
                u.cost_usd
            } else {
                0.0
            },
        })
        .collect()
}

fn refuse(why: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refusal::Invalid(why.into()))
}

/// Decisions that let something run on the host (everything but a "no").
fn permits(kind: &str, decision: &str) -> bool {
    match kind {
        "permission" => decision != "deny",
        "plan_review" => decision.starts_with("approve"),
        // A relay goes out on approve or an edited note (#45).
        "relay_review" => matches!(decision, "approve" | "edit"),
        // A harness question's answer is typed as the next prompt.
        "question" => decision == "answer",
        _ => false,
    }
}

impl Core {
    /// Stores what an upsert or end of `session_id` (already owned by
    /// `node_id`) says beyond the session row, and charges its usage.
    pub async fn session_extras_apply(
        &self,
        node_id: &str,
        session_id: &str,
        upd: ExtrasUpdate,
    ) -> Result<SessionExtras> {
        let store = self.store();
        let Some(s) = store.session(session_id).await? else {
            return Err(anyhow::Error::new(Refusal::NotFound("session")));
        };
        if s.node_id != node_id {
            return Err(anyhow::Error::new(Refusal::NotOwner));
        }
        let mut x = store
            .session_extras(session_id)
            .await?
            .unwrap_or_else(|| SessionExtras {
                session_id: session_id.into(),
                ..Default::default()
            });
        let before = x.clone();
        x.pending_ask = upd.pending_ask.and_then(clean_ask);
        if let Some(a) = upd.accepts {
            x.accepts = Some(Accepts {
                controls_allowed: a
                    .controls_allowed
                    .iter()
                    .take(64)
                    .map(|c| cap(c, 64))
                    .collect(),
                commands: a.commands.iter().take(128).map(|c| cap(c, 64)).collect(),
                ..a
            });
        }
        if let Some(h) = upd.harness {
            x.harness = Some(SessionHarness {
                harness: cap(&h.harness, 64),
                version: cap(&h.version, 64),
                session_id: cap(&h.session_id, 128),
                transcript_source: cap(&h.transcript_source, 16),
                end: x.harness.as_ref().and_then(|p| p.end.clone()),
            });
        }
        if let Some(e) = upd.harness_end {
            let h = x.harness.get_or_insert_with(Default::default);
            h.end = Some(kls_store::belai::HarnessEnd {
                startup_ms: e.startup_ms.max(0),
                ready_ms: e.ready_ms.max(0),
                first_reply_ms: e.first_reply_ms.max(0),
                wall_ms: e.wall_ms.max(0),
                ..e
            });
        }
        if let Some(mut g) = upd.git {
            g.commit = cap(&g.commit, 64);
            x.git = Some(g);
        }
        let usage = clean_usage(&upd.usage);
        if !usage.is_empty() {
            x.usage = usage;
        }
        if let Some((turns, tools)) = upd.counts {
            x.turns = turns.max(0);
            x.tool_calls = tools.max(0);
        }
        if let Some(c) = upd.checkpoints {
            x.checkpoints = clean_checkpoints(c);
        }
        if let Some(p) = upd.plan_sha256 {
            let p = p.trim().to_ascii_lowercase();
            if p.len() == 64 && p.chars().all(|c| c.is_ascii_hexdigit()) {
                x.plan_sha256 = p;
            }
        }
        let usage_changed = x.usage != before.usage;
        x.updated_at = before.updated_at;
        if x != before {
            store.save_session_extras(&x).await?;
            self.emit(
                kinds::SESSION_EXTRAS,
                session_id,
                &json!({ "nodeId": node_id, "ask": x.pending_ask.as_ref().map(|a| &a.id) }),
            )
            .await?;
        }
        // Usage rows for Budgets: one per model; an older host's session
        // totals as one row, only while no per-model rows exist.
        let at = kls_store::now_ms();
        let row = |id: String, provider: &str, model: &str, i, o, c| UsageRow {
            id,
            at,
            source: "session".into(),
            session_id: session_id.into(),
            node_id: node_id.into(),
            provider: provider.into(),
            model: model.into(),
            agent: s.agent.clone(),
            repo: s.repo.clone(),
            input_tokens: i,
            output_tokens: o,
            cost_usd: c,
        };
        let mut rows = Vec::new();
        if usage_changed {
            for u in &x.usage {
                rows.push(row(
                    format!("session:{session_id}:{}/{}", u.provider, u.model),
                    &u.provider,
                    &u.model,
                    u.input_tokens + u.cache_read_tokens + u.cache_write_tokens,
                    u.output_tokens,
                    u.cost_usd,
                ));
            }
        } else if x.usage.is_empty()
            && let Some((i, o, c)) = upd.legacy
            && (i > 0 || o > 0 || c > 0.0)
        {
            rows.push(row(
                format!("session:{session_id}"),
                &s.provider,
                &s.model,
                i.max(0),
                o.max(0),
                if c.is_finite() { c.max(0.0) } else { 0.0 },
            ));
        }
        for r in rows {
            store.upsert_usage(&r).await?;
            self.emit(
                crate::vault::kinds::BUDGET_USAGE,
                &r.id,
                &json!({"id": r.id}),
            )
            .await?;
        }
        Ok(x)
    }

    /// What the session admits from Keleustes, when its host said.
    pub async fn session_accepts(&self, session_id: &str) -> Result<Option<Accepts>> {
        Ok(self
            .store()
            .session_extras(session_id)
            .await?
            .and_then(|x| x.accepts))
    }

    /// Queues a structured answer to the ask open on `session_id`. The ask
    /// must be the open one, the decision one it lists (clarify answers
    /// carry webask JSON instead), and the host must take answers. While
    /// the factory is stopped only a refusal goes out. Audited either way.
    pub async fn queue_ask_answer(
        &self,
        session_id: &str,
        ask_id: &str,
        decision: &str,
        payload_json: &str,
        note: &str,
    ) -> Result<InboxItem> {
        let out = self
            .check_ask_answer(session_id, ask_id, decision, payload_json, note)
            .await;
        let s = self.store().session(session_id).await?;
        let node = s.as_ref().map(|s| s.node_id.clone()).unwrap_or_default();
        let (outcome, why) = match &out {
            Ok(_) => ("queued", String::new()),
            Err(e) => ("refused", e.to_string()),
        };
        self.audit_record(LedgerDraft {
            kind: "answer".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!("answer to ask {ask_id}: {outcome}"),
            subject: json!({ "session": session_id, "ask": ask_id }),
            payload: json!({ "decision": decision, "structured": !payload_json.is_empty(),
                             "outcome": outcome, "reason": why }),
            origin: "factory".into(),
            node_id: node,
            session_id: session_id.into(),
            ..Default::default()
        })
        .await?;
        let kind = out?;
        let item = self
            .queue_inbox(
                session_id,
                InboxBody::Answer {
                    question_id: ask_id.into(),
                    text: if kind == "relay_review" && decision == "edit" {
                        // The edited handoff keeps its lines; the host admits it.
                        note.trim()
                            .chars()
                            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                            .take(MAX_RELAY)
                            .collect()
                    } else {
                        cap(note.trim(), 2000)
                    },
                    decision: decision.into(),
                    payload_json: payload_json.into(),
                },
            )
            .await?;
        self.emit(
            kinds::SESSION_ANSWER,
            session_id,
            &json!({ "ask": ask_id, "kind": kind, "decision": decision, "item": item.id }),
        )
        .await?;
        Ok(item)
    }

    /// The open ask's kind when the answer fits it, else why not.
    async fn check_ask_answer(
        &self,
        session_id: &str,
        ask_id: &str,
        decision: &str,
        payload_json: &str,
        note: &str,
    ) -> Result<String> {
        let x = self.store().session_extras(session_id).await?;
        let Some(ask) = x.as_ref().and_then(|x| x.pending_ask.clone()) else {
            return Err(refuse("the session has no open ask"));
        };
        if ask.id != ask_id {
            return Err(refuse("that ask is no longer open"));
        }
        if x.as_ref()
            .and_then(|x| x.accepts.as_ref())
            .is_some_and(|a| !a.answers)
        {
            return Err(refuse("the host does not take answers from Keleustes"));
        }
        if payload_json.len() > MAX_PAYLOAD {
            return Err(refuse("the answer is too large"));
        }
        match ask.kind.as_str() {
            "clarify" | "mode_choice" => {
                let v: serde_json::Value = serde_json::from_str(payload_json)
                    .map_err(|_| refuse("a clarify answer is the questionnaire's answer JSON"))?;
                if !v.is_object() {
                    return Err(refuse("a clarify answer is a JSON object"));
                }
            }
            _ => {
                if !ask.options.iter().any(|o| o.id == decision) {
                    return Err(refuse(format!(
                        "{decision:?} is not one of the ask's options"
                    )));
                }
                if !payload_json.is_empty() {
                    return Err(refuse("this ask takes a decision, not a payload"));
                }
                if ask.kind == "relay_review" && decision == "edit" && note.trim().is_empty() {
                    return Err(refuse("an edited handoff needs the note"));
                }
                if note.len() > MAX_RELAY {
                    return Err(refuse("the note is too large"));
                }
            }
        }
        if permits(&ask.kind, decision)
            && !crate::control::allowed(self, "answer", session_id, "owner").await
        {
            return Err(refuse("the factory is stopped: only a refusal can be sent"));
        }
        Ok(ask.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Session;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    #[test]
    fn worker_routes_count_as_offered_agents() {
        let mut h = kls_store::HostInfo::default();
        assert!(!advertises_agents(&h) && !offers_agent(&h, "belai:scout"));
        h.agents = vec!["belai:debug".into()];
        h.capabilities.agent_routes = vec![kls_store::AgentRoute {
            name: "belai:scout".into(),
            mode: "worker".into(),
            ..Default::default()
        }];
        assert!(advertises_agents(&h));
        assert!(offers_agent(&h, "belai:scout") && offers_agent(&h, "belai:debug"));
        assert!(!offers_agent(&h, "belai:builder"));
    }

    fn ask() -> PendingAsk {
        PendingAsk {
            id: "a1".into(),
            kind: "permission".into(),
            title: "permission: Bash ls".into(),
            tool: "Bash".into(),
            options: vec![
                AskOption {
                    id: "allow_once".into(),
                    label: "Allow once".into(),
                },
                AskOption {
                    id: "deny".into(),
                    label: "Deny".into(),
                },
            ],
            ..Default::default()
        }
    }

    async fn live(c: &Core) {
        c.session_upsert(
            "n1",
            Session {
                id: "s1".into(),
                state: "waiting".into(),
                provider: "anthropic".into(),
                model: "m".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn extras_and_usage_rows() {
        let (c, _d) = core().await;
        live(&c).await;
        assert!(
            c.session_extras_apply("n2", "s1", ExtrasUpdate::default())
                .await
                .is_err()
        );
        let upd = ExtrasUpdate {
            pending_ask: Some(ask()),
            accepts: Some(Accepts {
                answers: true,
                ..Default::default()
            }),
            usage: vec![ModelUsage {
                provider: "anthropic".into(),
                model: "m".into(),
                input_tokens: 100,
                output_tokens: 10,
                cost_usd: 0.5,
                ..Default::default()
            }],
            ..Default::default()
        };
        let x = c
            .session_extras_apply("n1", "s1", upd.clone())
            .await
            .unwrap();
        assert_eq!(x.pending_ask.unwrap().id, "a1");
        let (tokens, _) = c.store().usage_sum(&Default::default()).await.unwrap();
        assert_eq!(tokens, 110);
        // A repeat charges nothing new (the row is cumulative).
        c.session_extras_apply("n1", "s1", upd).await.unwrap();
        let (again, _) = c.store().usage_sum(&Default::default()).await.unwrap();
        assert_eq!(again, 110);
    }

    #[tokio::test]
    async fn structured_answer_checks_and_audit() {
        let (c, _d) = core().await;
        live(&c).await;
        let e = c.queue_ask_answer("s1", "a1", "deny", "", "").await;
        assert!(e.is_err(), "no open ask");
        c.session_extras_apply(
            "n1",
            "s1",
            ExtrasUpdate {
                pending_ask: Some(ask()),
                accepts: Some(Accepts {
                    answers: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            c.queue_ask_answer("s1", "a1", "allow_always", "", "")
                .await
                .is_err()
        );
        assert!(
            c.queue_ask_answer("s1", "old", "deny", "", "")
                .await
                .is_err()
        );
        let item = c
            .queue_ask_answer("s1", "a1", "allow_once", "", "ok")
            .await
            .unwrap();
        assert_eq!(
            InboxBody::of(&item).unwrap(),
            InboxBody::Answer {
                question_id: "a1".into(),
                text: "ok".into(),
                decision: "allow_once".into(),
                payload_json: String::new(),
            }
        );
        // Kill switch: only a refusal goes out.
        c.set_factory_stopped(true, "maintenance").await.unwrap();
        assert!(
            c.queue_ask_answer("s1", "a1", "allow_once", "", "")
                .await
                .is_err()
        );
        c.queue_ask_answer("s1", "a1", "deny", "", "")
            .await
            .unwrap();
        let entries = c
            .audit_entries(&Default::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "answer")
            .count();
        assert_eq!(entries, 6);
        // A host that does not take answers.
        c.session_extras_apply(
            "n1",
            "s1",
            ExtrasUpdate {
                pending_ask: Some(ask()),
                accepts: Some(Accepts::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            c.queue_ask_answer("s1", "a1", "deny", "", "")
                .await
                .is_err()
        );
    }
}
