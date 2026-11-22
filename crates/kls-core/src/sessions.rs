//! Domain: belai sessions mirrored from nodes, their inbox (prompts,
//! answers, commands), dispatches and jobs. Persistence is
//! `kls_store::sessions`; the node server (`kls-node`) calls the `session_*`
//! and `complete` functions, the UI and engine the `queue_*` ones and
//! `core.gateway()?` for dispatches and jobs.

use crate::Core;
use anyhow::Result;
use kls_store::sessions::{AppendOutcome, UpsertOutcome};
use serde::{Deserialize, Serialize};

pub use kls_store::sessions::{AuditLine, Dispatch, Entry, InboxItem, Job, Session, SessionQuery};

/// Console session actions: compact, relay, undo, rewind, commit, btw, rename (#45).
pub mod actions;
/// Delivered / agent done / verified, kept apart (O-061).
pub mod axes;
/// Run step, budgets, PR checks, cards and todo around a session (WP5).
pub mod context;
/// How a session ended, and role-manager decisions in transcripts (WP5).
pub mod outcome;
/// Resume or fork a belai session, run an approved plan (#46, #52).
pub mod resume;
/// Start fields of session, worker and crew dispatches and MCP bindings (P3).
pub mod start;

/// A git strip action on a session (WP5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitAction {
    /// Switch the session's work tree to a branch.
    Branch(String),
    /// Turn the session's remote sync on or off.
    Sync(bool),
    /// Move the session to another node.
    Teleport(String),
}

/// Session commands that only wind work down: allowed under the kill switch.
pub const WIND_DOWN: [&str; 3] = ["stop", "interrupt", "cancel"];

/// Event kinds of this area. Subjects: the session id for `session.*`, the
/// dispatch id for `dispatch.updated`, the job id for `job.updated`.
pub mod kinds {
    /// A session was created or its metadata changed.
    pub const SESSION_UPSERTED: &str = "session.upserted";
    /// Transcript entries were stored. Data: `{nextIndex, inserted}`.
    pub const SESSION_ENTRIES: &str = "session.entries";
    pub const SESSION_ENDED: &str = "session.ended";
    /// How it ended. Data: `{reason, outcome, severity, headline, hostReason}`.
    pub const SESSION_FINISHED: &str = "session.finished";
    /// A role-manager System One call in a transcript upload. Data: the
    /// [`super::outcome::SessionDecision`] plus `sessionId`, `nodeId`, `more`.
    pub const DECISION_SESSION: &str = "decision.session";
    /// A composer line went to a session as a slash command or `!` shell.
    pub const SESSION_COMPOSED: &str = "session.composed";
    /// An inbox item was queued, delivered or acked. Data: `{itemId, seq, kind, status}`.
    pub const SESSION_INBOX: &str = "session.inbox";
    /// Data: `{nodeId, status, sessionId, error}`.
    pub const DISPATCH_UPDATED: &str = "dispatch.updated";
    /// Data: `{nodeId, kind, status}`.
    pub const JOB_UPDATED: &str = "job.updated";
}

/// Why a session request was refused. Carried inside `anyhow::Error`; the
/// node server maps it to a gRPC status with `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotFound(&'static str),
    /// The session, dispatch or job belongs to another node.
    NotOwner,
    /// The session has ended.
    Ended,
    Invalid(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotFound(what) => write!(f, "unknown {what}"),
            Refusal::NotOwner => write!(f, "belongs to another node"),
            Refusal::Ended => write!(f, "session has ended"),
            Refusal::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// What a session inbox item asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InboxBody {
    Prompt {
        text: String,
    },
    /// `decision` and `payload_json` make it structured (WP-A): one of the
    /// open ask's option ids, or webask's clarify answer JSON. Empty for a
    /// free-text answer.
    Answer {
        question_id: String,
        text: String,
        #[serde(default)]
        decision: String,
        #[serde(default)]
        payload_json: String,
    },
    Command {
        name: String,
        args: Vec<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptJson {
    text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnswerJson {
    question_id: String,
    text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    decision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    payload_json: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommandJson {
    name: String,
    #[serde(default)]
    args: Vec<String>,
}

impl InboxBody {
    /// "prompt", "answer" or "command" (the stored kind).
    pub fn kind(&self) -> &'static str {
        match self {
            InboxBody::Prompt { .. } => "prompt",
            InboxBody::Answer { .. } => "answer",
            InboxBody::Command { .. } => "command",
        }
    }

    /// The stored payload JSON.
    pub fn payload_json(&self) -> String {
        match self {
            InboxBody::Prompt { text } => serde_json::to_string(&PromptJson { text: text.clone() }),
            InboxBody::Answer {
                question_id,
                text,
                decision,
                payload_json,
            } => serde_json::to_string(&AnswerJson {
                question_id: question_id.clone(),
                text: text.clone(),
                decision: decision.clone(),
                payload_json: payload_json.clone(),
            }),
            InboxBody::Command { name, args } => serde_json::to_string(&CommandJson {
                name: name.clone(),
                args: args.clone(),
            }),
        }
        .unwrap_or_default()
    }

    /// Reads a stored item back.
    pub fn of(item: &InboxItem) -> Result<Self> {
        Ok(match item.kind.as_str() {
            "prompt" => {
                let p: PromptJson = serde_json::from_str(&item.payload_json)?;
                InboxBody::Prompt { text: p.text }
            }
            "answer" => {
                let a: AnswerJson = serde_json::from_str(&item.payload_json)?;
                InboxBody::Answer {
                    question_id: a.question_id,
                    text: a.text,
                    decision: a.decision,
                    payload_json: a.payload_json,
                }
            }
            "command" => {
                let c: CommandJson = serde_json::from_str(&item.payload_json)?;
                InboxBody::Command {
                    name: c.name,
                    args: c.args,
                }
            }
            k => anyhow::bail!("unknown inbox kind {k:?}"),
        })
    }
}

/// A node's report on a job or dispatch (the protocol's `JobResult`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub job_id: String,
    /// "ok", "failed", "refused".
    pub status: String,
    pub output_json: String,
    pub error: String,
    /// For a dispatch: the session it started.
    pub session_id: String,
}

/// What a [`Completion`] resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Completed {
    Job(Job),
    Dispatch(Dispatch),
    /// Already resolved earlier (a retry); nothing changed.
    Duplicate,
}

/// What a composer line is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composed {
    Prompt,
    /// `/name args...`
    Command(String, Vec<String>),
    /// `!cmd` (the text after `!`).
    Shell(String),
}

/// Reads a composer line: `/name args` (not `//`, which is a prompt that
/// starts with a slash), `!cmd`, or a prompt.
pub fn parse_composer(line: &str) -> Composed {
    let line = line.trim();
    if let Some(cmd) = line.strip_prefix('!') {
        return Composed::Shell(cmd.trim().to_string());
    }
    if let Some(rest) = line.strip_prefix('/')
        && !rest.starts_with('/')
        && rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        let mut it = rest.split_whitespace();
        let name = it.next().unwrap_or_default().to_string();
        return Composed::Command(name, it.map(str::to_string).collect());
    }
    Composed::Prompt
}

/// Commands a composer offers after `/`: the session's own and its
/// controls, plus the wind-down ones, filtered by what is typed.
pub fn composer_completions(a: Option<&kls_store::belai::Accepts>, typed: &str) -> Vec<String> {
    let typed = typed.trim_start_matches('/');
    let mut out: Vec<String> = WIND_DOWN.iter().map(|s| s.to_string()).collect();
    if let Some(a) = a {
        for c in a.commands.iter().chain(&a.controls_allowed) {
            let c = c.trim_start_matches('/').to_string();
            if !out.contains(&c) && (c != "shell" || a.shell) {
                out.push(c);
            }
        }
    }
    out.retain(|c| c.starts_with(typed));
    out.sort();
    out
}

fn refuse(r: Refusal) -> anyhow::Error {
    anyhow::Error::new(r)
}

impl Core {
    /// A session that `node_id` owns.
    pub async fn owned_session(&self, node_id: &str, session_id: &str) -> Result<Session> {
        let s = self
            .store()
            .session(session_id)
            .await?
            .ok_or_else(|| refuse(Refusal::NotFound("session")))?;
        if s.node_id != node_id {
            return Err(refuse(Refusal::NotOwner));
        }
        Ok(s)
    }

    /// A node's session upsert (see `Store::upsert_session` for the merge
    /// rules). A session naming its dispatch resolves that dispatch as
    /// started.
    pub async fn session_upsert(&self, node_id: &str, mut s: Session) -> Result<Session> {
        if s.id.trim().is_empty() {
            return Err(refuse(Refusal::Invalid("session id is required".into())));
        }
        s.node_id = node_id.to_string();
        if self.store().upsert_session(&s).await? == UpsertOutcome::NotOwner {
            return Err(refuse(Refusal::NotOwner));
        }
        if !s.dispatch_id.is_empty() {
            self.dispatch_started(node_id, &s.dispatch_id, &s.id)
                .await?;
        }
        let stored = self
            .store()
            .session(&s.id)
            .await?
            .ok_or_else(|| refuse(Refusal::NotFound("session")))?;
        self.emit(
            kinds::SESSION_UPSERTED,
            &stored.id,
            &serde_json::json!({ "nodeId": node_id, "state": stored.state }),
        )
        .await?;
        Ok(stored)
    }

    /// Stores transcript entries (idempotent by index) and returns the next
    /// index the server expects. A session seen first through its entries
    /// is created bare.
    pub async fn session_append(
        &self,
        node_id: &str,
        session_id: &str,
        entries: &[Entry],
    ) -> Result<AppendOutcome> {
        let mut known = 0;
        match self.owned_session(node_id, session_id).await {
            Ok(s) => known = s.next_index,
            Err(e) if e.downcast_ref::<Refusal>() == Some(&Refusal::NotFound("session")) => {
                self.session_upsert(
                    node_id,
                    Session {
                        id: session_id.to_string(),
                        ..Default::default()
                    },
                )
                .await?;
            }
            Err(e) => return Err(e),
        }
        let out = self.store().append_entries(session_id, entries).await?;
        if out.inserted > 0 {
            self.emit(
                kinds::SESSION_ENTRIES,
                session_id,
                &serde_json::json!({ "nextIndex": out.next_index, "inserted": out.inserted }),
            )
            .await?;
            self.emit_session_decisions(node_id, session_id, entries, known)
                .await?;
        }
        Ok(out)
    }

    /// Each newly stored role-manager decision becomes one `decision.session`
    /// event, oldest first, at most [`outcome::DECISIONS_PER_UPLOAD`] per
    /// upload; the last says how many more the transcript holds. A resent
    /// entry (below the index already stored) is never emitted again.
    async fn emit_session_decisions(
        &self,
        node_id: &str,
        session_id: &str,
        entries: &[Entry],
        known: i64,
    ) -> Result<usize> {
        let picks: Vec<_> = entries
            .iter()
            .filter(|e| e.idx >= known)
            .filter_map(outcome::decision_of)
            .collect();
        let left = picks.len().saturating_sub(outcome::DECISIONS_PER_UPLOAD);
        let n = picks.len().min(outcome::DECISIONS_PER_UPLOAD);
        for (i, d) in picks.into_iter().take(n).enumerate() {
            let mut data = serde_json::to_value(&d)?;
            data["sessionId"] = session_id.into();
            data["nodeId"] = node_id.into();
            data["origin"] = format!("host:{node_id}").into();
            if i + 1 == n && left > 0 {
                data["more"] = left.into();
            }
            self.emit(kinds::DECISION_SESSION, session_id, &data)
                .await?;
        }
        Ok(n)
    }

    /// How the session ended (the outcome taxonomy), from the host's
    /// reason and, for an older host, the transcript.
    pub async fn session_outcome(&self, session_id: &str) -> Result<outcome::Outcome> {
        let s = self
            .store()
            .session(session_id)
            .await?
            .ok_or_else(|| refuse(Refusal::NotFound("session")))?;
        let tail = self
            .store()
            .entries(session_id, (s.next_index - 200).max(0), 200)
            .await?;
        let inbox = self.store().inbox_items(session_id, 200).await?;
        let f = outcome::EndFacts::read(&s.end_reason, s.ended_at.is_some(), &tail, &inbox);
        Ok(outcome::outcome_of(outcome::derive(&f)))
    }

    /// The host ended the session. Repeats are fine.
    ///
    /// **First writer wins (O-073):** the first end settles `ended_at` and
    /// `end_reason`; a later end (the host's `completed` after a `lost`
    /// sweep or a stop) changes nothing and emits nothing. Only a new live
    /// upsert from the owning host reopens it (a resume).
    pub async fn session_end(
        &self,
        node_id: &str,
        session_id: &str,
        ended_at: i64,
        reason: &str,
    ) -> Result<()> {
        self.owned_session(node_id, session_id).await?;
        if self
            .store()
            .end_session(session_id, ended_at, reason)
            .await?
        {
            let o = self.session_outcome(session_id).await?;
            self.emit(
                kinds::SESSION_ENDED,
                session_id,
                &serde_json::json!({ "reason": reason, "outcome": o.outcome }),
            )
            .await?;
            self.emit(
                kinds::SESSION_FINISHED,
                session_id,
                &serde_json::json!({
                    "reason": o.reason, "outcome": o.outcome, "severity": o.severity,
                    "headline": o.headline, "hostReason": reason, "nodeId": node_id,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Queues an inbox item for an open session.
    pub async fn queue_inbox(&self, session_id: &str, body: InboxBody) -> Result<InboxItem> {
        let s = self
            .store()
            .session(session_id)
            .await?
            .ok_or_else(|| refuse(Refusal::NotFound("session")))?;
        if !s.is_open() {
            return Err(refuse(Refusal::Ended));
        }
        match &body {
            InboxBody::Prompt { text } if text.trim().is_empty() => {
                return Err(refuse(Refusal::Invalid("prompt is empty".into())));
            }
            InboxBody::Answer { question_id, .. } if question_id.is_empty() => {
                return Err(refuse(Refusal::Invalid(
                    "answer needs a question id".into(),
                )));
            }
            InboxBody::Command { name, .. } if name.trim().is_empty() => {
                return Err(refuse(Refusal::Invalid("command needs a name".into())));
            }
            _ => {}
        }
        let id = uuid::Uuid::now_v7().to_string();
        let item = self
            .store()
            .inbox_push(session_id, &id, body.kind(), &body.payload_json())
            .await?;
        self.emit_inbox(&item).await?;
        Ok(item)
    }

    /// Queues a prompt as the owner. Refused under the kill switch (pause
    /// and stop) like every outbound frame; see [`Core::queue_prompt_from`].
    pub async fn queue_prompt(&self, session_id: &str, text: &str) -> Result<InboxItem> {
        self.queue_prompt_from(session_id, text, "owner").await
    }

    /// Queues a prompt for `origin` (the owner, `owner:cli`, a run, a node).
    /// The kill switch is checked first: a refusal emits `factory.refused`
    /// (folded into the audit ledger) and returns its `Blocked`.
    pub async fn queue_prompt_from(
        &self,
        session_id: &str,
        text: &str,
        origin: &str,
    ) -> Result<InboxItem> {
        crate::control::check(self, "prompt", session_id, origin).await?;
        self.queue_inbox(
            session_id,
            InboxBody::Prompt {
                text: text.to_string(),
            },
        )
        .await
    }

    pub async fn queue_answer(
        &self,
        session_id: &str,
        question_id: &str,
        text: &str,
    ) -> Result<InboxItem> {
        self.queue_inbox(
            session_id,
            InboxBody::Answer {
                question_id: question_id.to_string(),
                text: text.to_string(),
                decision: String::new(),
                payload_json: String::new(),
            },
        )
        .await
    }

    /// Session controls: "stop", "interrupt", "mode", "model", ... as the
    /// owner; see [`Core::queue_command_from`].
    pub async fn queue_command(
        &self,
        session_id: &str,
        name: &str,
        args: &[String],
    ) -> Result<InboxItem> {
        self.queue_command_from(session_id, name, args, "owner")
            .await
    }

    /// A session control for `origin`. Winding down ([`WIND_DOWN`]) is always
    /// allowed; every other command is new work and is refused under the
    /// kill switch (pause and stop), emitting the audited `factory.refused`.
    pub async fn queue_command_from(
        &self,
        session_id: &str,
        name: &str,
        args: &[String],
        origin: &str,
    ) -> Result<InboxItem> {
        if actions::is_action(name) {
            // A host custom command of the same name (listed in `commands`
            // only) keeps the custom command path.
            let custom = self
                .store()
                .session_extras(session_id)
                .await?
                .and_then(|x| x.accepts)
                .is_some_and(|a| {
                    let listed = |l: &[String]| l.iter().any(|c| c.trim_start_matches('/') == name);
                    listed(&a.commands) && !listed(&a.controls_allowed)
                });
            if !custom {
                return self.queue_action(session_id, name, args, origin).await;
            }
        }
        if !WIND_DOWN.contains(&name) {
            crate::control::check(self, "command", session_id, origin).await?;
        }
        self.queue_inbox(
            session_id,
            InboxBody::Command {
                name: name.to_string(),
                args: args.to_vec(),
            },
        )
        .await
    }

    /// One composer line for `origin`: `/name args` is a session command,
    /// `!cmd` runs `cmd` in the session's shell (only when the session
    /// accepts shell), anything else is a prompt. Every form checks the kill
    /// switch first (a `/stop`, `/interrupt` or `/cancel` still winds down);
    /// a command the session does not list is refused. Commands and shell
    /// lines are audited with their origin.
    pub async fn queue_composer(
        &self,
        session_id: &str,
        line: &str,
        origin: &str,
    ) -> Result<InboxItem> {
        let line = line.trim();
        let parsed = parse_composer(line);
        let (name, args) = match parsed {
            Composed::Prompt => return self.queue_prompt_from(session_id, line, origin).await,
            Composed::Shell(cmd) => ("shell".to_string(), vec![cmd]),
            Composed::Command(n, a) => (n, a),
        };
        let accepts = self
            .store()
            .session_extras(session_id)
            .await?
            .and_then(|x| x.accepts);
        // A console session action the session lists (#45) takes its checked
        // path (its own kill-switch rule); a custom command of the same name
        // stays a custom command.
        if actions::is_action(&name)
            && accepts.as_ref().is_some_and(|a| {
                a.controls_allowed
                    .iter()
                    .any(|c| c.trim_start_matches('/') == name)
            })
        {
            return self.queue_action(session_id, &name, &args, origin).await;
        }
        if !WIND_DOWN.contains(&name.as_str()) || name == "shell" {
            crate::control::check(self, "command", session_id, origin).await?;
        }
        if name == "shell" {
            if !accepts.as_ref().is_some_and(|a| a.shell) {
                return Err(refuse(Refusal::Invalid(
                    "this session takes no shell commands".into(),
                )));
            }
            if args[0].is_empty() {
                return Err(refuse(Refusal::Invalid("type a command after !".into())));
            }
        } else if let Some(a) = &accepts
            && !WIND_DOWN.contains(&name.as_str())
            && !a
                .commands
                .iter()
                .chain(&a.controls_allowed)
                .any(|c| c.trim_start_matches('/') == name)
        {
            return Err(refuse(Refusal::Invalid(format!(
                "the session does not take /{name}"
            ))));
        }
        let item = self
            .queue_inbox(
                session_id,
                InboxBody::Command {
                    name: name.clone(),
                    args: args.clone(),
                },
            )
            .await?;
        let data = serde_json::json!({
            "sessionId": session_id, "name": name, "args": args, "origin": origin,
            "itemId": item.id,
        });
        self.emit(kinds::SESSION_COMPOSED, session_id, &data)
            .await?;
        self.audit_record(kls_store::audit::LedgerDraft {
            kind: "session".into(),
            actor: origin.into(),
            actor_kind: "user".into(),
            msg: if name == "shell" {
                format!("shell line queued for session {session_id} by {origin}")
            } else {
                format!("/{name} queued for session {session_id} by {origin}")
            },
            subject: serde_json::json!({ "session": session_id }),
            payload: data,
            session_id: session_id.into(),
            ..Default::default()
        })
        .await?;
        Ok(item)
    }

    /// A git strip action for `origin`: switch branch or toggle sync (as a
    /// session command the session must list), or teleport to another
    /// node. Refused under the kill switch; audited with its origin.
    /// Returns the inbox item or teleport id.
    pub async fn session_git(
        &self,
        session_id: &str,
        action: GitAction,
        origin: &str,
    ) -> Result<String> {
        crate::control::check(self, "command", session_id, origin).await?;
        let s = self
            .store()
            .session(session_id)
            .await?
            .ok_or_else(|| refuse(Refusal::NotFound("session")))?;
        let (what, id) = match &action {
            GitAction::Teleport(to) => {
                let t = self.teleport_request(session_id, &s.node_id, to).await?;
                (format!("teleport to {to}"), t.id)
            }
            GitAction::Branch(_) | GitAction::Sync(_) => {
                let (name, args) = match &action {
                    GitAction::Branch(b) if !b.trim().is_empty() => {
                        ("branch", vec![b.trim().to_string()])
                    }
                    GitAction::Sync(on) => ("sync", vec![if *on { "on" } else { "off" }.into()]),
                    _ => return Err(refuse(Refusal::Invalid("name a branch".into()))),
                };
                let listed = self
                    .store()
                    .session_extras(session_id)
                    .await?
                    .and_then(|x| x.accepts)
                    .is_some_and(|a| {
                        a.controls_allowed
                            .iter()
                            .chain(&a.commands)
                            .any(|c| c.trim_start_matches('/') == name)
                    });
                if !listed {
                    return Err(refuse(Refusal::Invalid(format!(
                        "the session does not take {name}"
                    ))));
                }
                let item = self
                    .queue_inbox(
                        session_id,
                        InboxBody::Command {
                            name: name.into(),
                            args: args.clone(),
                        },
                    )
                    .await?;
                (format!("{name} {}", args.join(" ")), item.id)
            }
        };
        self.audit_record(kls_store::audit::LedgerDraft {
            kind: "session".into(),
            actor: origin.into(),
            actor_kind: "user".into(),
            msg: format!("{what} for session {session_id} by {origin}"),
            subject: serde_json::json!({ "session": session_id }),
            payload: serde_json::json!({ "action": what, "id": id, "origin": origin }),
            session_id: session_id.into(),
            node_id: s.node_id.clone(),
            ..Default::default()
        })
        .await?;
        Ok(id)
    }

    async fn emit_inbox(&self, item: &InboxItem) -> Result<()> {
        self.emit(
            kinds::SESSION_INBOX,
            &item.session_id,
            &serde_json::json!({
                "itemId": item.id, "seq": item.seq, "kind": item.kind, "status": item.status,
            }),
        )
        .await?;
        Ok(())
    }

    /// The node server streamed the item for the first time.
    pub async fn inbox_delivered(&self, session_id: &str, seq: i64) -> Result<()> {
        if self.store().inbox_mark_delivered(session_id, seq).await? {
            self.emit(
                kinds::SESSION_INBOX,
                session_id,
                &serde_json::json!({ "seq": seq, "status": "delivered" }),
            )
            .await?;
        }
        Ok(())
    }

    /// The session applied (or refused) an item. Repeats are fine.
    pub async fn inbox_ack(
        &self,
        node_id: &str,
        session_id: &str,
        item_id: &str,
        status: &str,
        detail: &str,
    ) -> Result<()> {
        self.owned_session(node_id, session_id).await?;
        let status = match status {
            "" | "done" => "done",
            "failed" | "refused" => status,
            other => {
                return Err(refuse(Refusal::Invalid(format!(
                    "unknown ack status {other:?}"
                ))));
            }
        };
        let detail: String = detail.chars().take(2000).collect();
        match self
            .store()
            .inbox_ack(session_id, item_id, status, &detail)
            .await?
        {
            Some(item) => self.emit_inbox(&item).await,
            None => match self.store().inbox_item(item_id).await? {
                Some(i) if i.session_id == session_id => Ok(()),
                _ => Err(refuse(Refusal::NotFound("inbox item"))),
            },
        }
    }

    /// Raw host audit lines, kept for M6's ledger. Returns how many were new.
    pub async fn record_audit(
        &self,
        node_id: &str,
        session_id: &str,
        lines: &[AuditLine],
    ) -> Result<u64> {
        let stored = self
            .store()
            .append_audit_lines(node_id, session_id, lines)
            .await?;
        // Harness sessions: their prompt and stop facts become events
        // (ids and hashes only), once the lines are safely in the ledger.
        if stored > 0
            && let Err(e) = self.harness_lines(node_id, session_id, lines).await
        {
            tracing::warn!(error = %e, session_id, "harness audit events");
        }
        Ok(stored)
    }

    // ---- dispatches and jobs (used by the gateway in kls-node) --------------

    /// Persists a new dispatch row and announces it.
    pub async fn dispatch_queued(&self, d: &Dispatch) -> Result<()> {
        self.store().insert_dispatch(d).await?;
        self.emit_dispatch(&self.store().dispatch(&d.id).await?.unwrap_or(d.clone()))
            .await
    }

    /// The dispatch frame is on the node's outbox with this seq.
    pub async fn dispatch_sent(&self, id: &str, outbox_seq: i64) -> Result<()> {
        self.store().set_dispatch_outbox_seq(id, outbox_seq).await?;
        if let Some(d) = self.store().dispatch(id).await? {
            self.emit_dispatch(&d).await?;
        }
        Ok(())
    }

    /// Marks queued dispatches whose frames the nodes acked as delivered.
    /// Returns how many changed.
    pub async fn sweep_delivered_dispatches(&self) -> Result<usize> {
        let changed = self.store().mark_dispatches_delivered().await?;
        for d in &changed {
            self.emit_dispatch(d).await?;
        }
        Ok(changed.len())
    }

    async fn dispatch_started(&self, node_id: &str, id: &str, session_id: &str) -> Result<()> {
        if let Some(d) = self.store().dispatch(id).await?
            && d.node_id == node_id
            && d.resolved_at.is_none()
            && let Some(d) = self
                .store()
                .resolve_dispatch(id, "started", session_id, "")
                .await?
        {
            self.emit_dispatch(&d).await?;
        }
        Ok(())
    }

    async fn emit_dispatch(&self, d: &Dispatch) -> Result<()> {
        self.emit(
            kinds::DISPATCH_UPDATED,
            &d.id,
            &serde_json::json!({
                "nodeId": d.node_id, "status": d.status, "sessionId": d.session_id, "error": d.error,
            }),
        )
        .await?;
        Ok(())
    }

    /// Persists a new job row (status "queued") and announces it.
    pub async fn job_queued(
        &self,
        id: &str,
        node_id: &str,
        kind: &str,
        payload_json: &str,
        deadline_at: i64,
    ) -> Result<()> {
        self.store()
            .insert_job(id, node_id, kind, payload_json, deadline_at)
            .await?;
        self.emit(
            kinds::JOB_UPDATED,
            id,
            &serde_json::json!({ "nodeId": node_id, "kind": kind, "status": "queued" }),
        )
        .await?;
        Ok(())
    }

    /// The job's deadline passed without a result. `None` when it resolved
    /// in the meantime.
    pub async fn job_timed_out(&self, id: &str) -> Result<Option<Job>> {
        let j = self
            .store()
            .resolve_job(id, "timeout", "", "no result before the deadline")
            .await?;
        if let Some(j) = &j {
            self.emit_job(j).await?;
        }
        Ok(j)
    }

    async fn emit_job(&self, j: &Job) -> Result<()> {
        self.emit(
            kinds::JOB_UPDATED,
            &j.id,
            &serde_json::json!({ "nodeId": j.node_id, "kind": j.kind, "status": j.status }),
        )
        .await?;
        Ok(())
    }

    /// A node reports the result of a job, or the outcome of a dispatch
    /// (`job_id` is then the dispatch id). Only the node the work was sent
    /// to may report it; a repeat is a no-op.
    pub async fn complete(&self, node_id: &str, c: &Completion) -> Result<Completed> {
        let store = self.store();
        let error: String = c.error.chars().take(4000).collect();
        if let Some(job) = store.job(&c.job_id).await? {
            if job.node_id != node_id {
                return Err(refuse(Refusal::NotOwner));
            }
            let status = match c.status.as_str() {
                "ok" | "failed" | "refused" => c.status.as_str(),
                _ => "failed",
            };
            return Ok(
                match store
                    .resolve_job(&c.job_id, status, &c.output_json, &error)
                    .await?
                {
                    Some(j) => {
                        self.emit_job(&j).await?;
                        Completed::Job(j)
                    }
                    None => Completed::Duplicate,
                },
            );
        }
        let Some(d) = store.dispatch(&c.job_id).await? else {
            return Err(refuse(Refusal::NotFound("job or dispatch")));
        };
        if d.node_id != node_id {
            return Err(refuse(Refusal::NotOwner));
        }
        let (status, error) = match c.status.as_str() {
            "ok" => ("started", String::new()),
            "refused" if error.is_empty() => ("failed", "refused by the node".to_string()),
            "refused" => ("failed", format!("refused: {error}")),
            _ if error.is_empty() => ("failed", "failed on the node".to_string()),
            _ => ("failed", error),
        };
        Ok(
            match store
                .resolve_dispatch(&d.id, status, &c.session_id, &error)
                .await?
            {
                Some(d) => {
                    self.emit_dispatch(&d).await?;
                    Completed::Dispatch(d)
                }
                None => Completed::Duplicate,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn sess(id: &str) -> Session {
        Session {
            id: id.into(),
            title: "t".into(),
            state: "live".into(),
            dispatch_id: String::new(),
            ..Default::default()
        }
    }

    fn refusal(e: &anyhow::Error) -> Option<Refusal> {
        e.downcast_ref::<Refusal>().cloned()
    }

    #[tokio::test]
    async fn upsert_append_end_emit() {
        let (c, _d) = core().await;
        let mut rx = c.subscribe();
        let s = c.session_upsert("n1", sess("s1")).await.unwrap();
        assert_eq!(s.node_id, "n1");
        assert_eq!(rx.recv().await.unwrap().kind, kinds::SESSION_UPSERTED);
        let e = c.session_upsert("n2", sess("s1")).await.unwrap_err();
        assert_eq!(refusal(&e), Some(Refusal::NotOwner));
        assert!(c.session_upsert("n1", sess(" ")).await.is_err());

        let entry = |idx| Entry {
            idx,
            role: "user".into(),
            text: "hi".into(),
            ..Default::default()
        };
        let out = c
            .session_append("n1", "s1", &[entry(0), entry(1)])
            .await
            .unwrap();
        assert_eq!(out.next_index, 2);
        assert_eq!(rx.recv().await.unwrap().kind, kinds::SESSION_ENTRIES);
        // A replay emits nothing.
        c.session_append("n1", "s1", &[entry(1)]).await.unwrap();
        assert!(rx.try_recv().is_err());
        assert!(c.session_append("n2", "s1", &[entry(2)]).await.is_err());
        // Entries for an unseen session create it.
        let out = c.session_append("n1", "s2", &[entry(0)]).await.unwrap();
        assert_eq!(out.next_index, 1);
        assert_eq!(
            c.store().session("s2").await.unwrap().unwrap().node_id,
            "n1"
        );

        c.session_end("n1", "s1", 0, "completed").await.unwrap();
        c.session_end("n1", "s1", 0, "completed").await.unwrap();
        assert!(c.session_end("n2", "s1", 0, "").await.is_err());
        let e = c.queue_prompt("s1", "more").await.unwrap_err();
        assert_eq!(refusal(&e), Some(Refusal::Ended));
    }

    /// The kill switch holds prompts and new-work commands on every path
    /// (the UI calls `queue_prompt`/`queue_command`; the CLI and nodes the
    /// `_from` forms); stop and interrupt still go out. Refusals are evented
    /// and land on the audit ledger.
    #[tokio::test]
    async fn kill_switch_refuses_prompts_and_commands() {
        let (c, _d) = core().await;
        c.session_upsert("n1", sess("s1")).await.unwrap();
        for mode in [crate::control::Mode::Pause, crate::control::Mode::Stop] {
            crate::control::set_mode(&c, mode, "drill", "owner")
                .await
                .unwrap();
            let mut rx = c.subscribe();
            // UI path.
            let e = c.queue_prompt("s1", "more").await.unwrap_err();
            assert!(crate::control::is_blocked(&e), "{e:#}");
            let e = c
                .queue_command("s1", "mode", &["plan".to_string()])
                .await
                .unwrap_err();
            assert!(crate::control::is_blocked(&e));
            // CLI and cross-host paths.
            let e = c
                .queue_prompt_from("s1", "more", "owner:cli")
                .await
                .unwrap_err();
            assert!(crate::control::is_blocked(&e));
            let e = c
                .queue_command_from("s1", "model", &[], "node:n2")
                .await
                .unwrap_err();
            assert!(crate::control::is_blocked(&e));
            let mut refused = 0;
            while let Ok(ev) = rx.try_recv() {
                if ev.kind == crate::control::kinds::FACTORY_REFUSED {
                    refused += 1;
                }
            }
            assert_eq!(refused, 4);
            // Winding down still goes out.
            c.queue_command("s1", "stop", &[]).await.unwrap();
            c.queue_command_from("s1", "interrupt", &[], "owner:cli")
                .await
                .unwrap();
        }
        let items = c.store().inbox_items("s1", 50).await.unwrap();
        assert!(items.iter().all(|i| i.kind == "command"));
        assert_eq!(items.len(), 4);
        c.audit_sync().await.unwrap();
        let led = c
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["kill_switch".into()],
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(led.len() >= 8, "refusals audited: {}", led.len());
        crate::control::resume(&c).await.unwrap();
        c.queue_prompt("s1", "go on").await.unwrap();
    }

    /// O-073: the first terminal settlement cannot be overwritten.
    #[tokio::test]
    async fn session_end_is_first_writer_wins() {
        let (c, _d) = core().await;
        c.session_upsert("n1", sess("s1")).await.unwrap();
        let mut rx = c.subscribe();
        // The lost sweep settles first...
        c.session_end("n1", "s1", 100, "lost").await.unwrap();
        assert_eq!(rx.recv().await.unwrap().kind, kinds::SESSION_ENDED);
        // ...then the host's late "completed" and an ended upsert arrive.
        c.session_end("n1", "s1", 200, "completed").await.unwrap();
        let mut ended = sess("s1");
        ended.state = "ended".into();
        c.store()
            .upsert_session(&Session {
                node_id: "n1".into(),
                ..ended
            })
            .await
            .unwrap();
        let s = c.store().session("s1").await.unwrap().unwrap();
        assert_eq!((s.ended_at, s.end_reason.as_str()), (Some(100), "lost"));
        assert_eq!(c.session_axes("s1").await.unwrap().agent, "lost");
        // No second session.ended.
        while let Ok(ev) = rx.try_recv() {
            assert_ne!(ev.kind, kinds::SESSION_ENDED);
        }
    }

    #[tokio::test]
    async fn inbox_queue_and_ack() {
        let (c, _d) = core().await;
        c.session_upsert("n1", sess("s1")).await.unwrap();
        assert!(c.queue_prompt("s1", "  ").await.is_err());
        assert!(c.queue_answer("s1", "", "yes").await.is_err());
        assert!(c.queue_prompt("nope", "x").await.is_err());
        let p = c.queue_prompt("s1", "go on").await.unwrap();
        let a = c.queue_answer("s1", "q1", "yes").await.unwrap();
        let k = c
            .queue_command("s1", "mode", &["plan".to_string()])
            .await
            .unwrap();
        assert_eq!((p.seq, a.seq, k.seq), (1, 2, 3));
        assert_eq!(
            InboxBody::of(&a).unwrap(),
            InboxBody::Answer {
                question_id: "q1".into(),
                text: "yes".into(),
                decision: String::new(),
                payload_json: String::new(),
            }
        );
        assert_eq!(
            InboxBody::of(&k).unwrap(),
            InboxBody::Command {
                name: "mode".into(),
                args: vec!["plan".into()]
            }
        );
        c.inbox_delivered("s1", 1).await.unwrap();
        assert_eq!(
            c.store().inbox_item(&p.id).await.unwrap().unwrap().status,
            "delivered"
        );
        assert!(c.inbox_ack("n2", "s1", &p.id, "done", "").await.is_err());
        assert!(c.inbox_ack("n1", "s1", &p.id, "weird", "").await.is_err());
        c.inbox_ack("n1", "s1", &p.id, "", "").await.unwrap();
        c.inbox_ack("n1", "s1", &p.id, "done", "").await.unwrap();
        c.inbox_ack("n1", "s1", &a.id, "refused", "turn moved on")
            .await
            .unwrap();
        let e = c
            .inbox_ack("n1", "s1", "nope", "done", "")
            .await
            .unwrap_err();
        assert_eq!(refusal(&e), Some(Refusal::NotFound("inbox item")));
        let left = c.store().inbox_unacked_after("s1", 0).await.unwrap();
        assert_eq!(left.len(), 1);
        let a = c.store().inbox_item(&a.id).await.unwrap().unwrap();
        assert_eq!(
            (a.status.as_str(), a.detail.as_str()),
            ("refused", "turn moved on")
        );
    }

    #[tokio::test]
    async fn complete_jobs_and_dispatches() {
        let (c, _d) = core().await;
        c.job_queued("j1", "n1", "github", "{}", 0).await.unwrap();
        let done = Completion {
            job_id: "j1".into(),
            status: "ok".into(),
            output_json: "{}".into(),
            ..Default::default()
        };
        assert_eq!(
            refusal(&c.complete("n2", &done).await.unwrap_err()),
            Some(Refusal::NotOwner)
        );
        assert!(matches!(
            c.complete("n1", &done).await.unwrap(),
            Completed::Job(j) if j.status == "ok"
        ));
        assert_eq!(c.complete("n1", &done).await.unwrap(), Completed::Duplicate);
        c.job_queued("j2", "n1", "model", "{}", 0).await.unwrap();
        assert_eq!(
            c.job_timed_out("j2").await.unwrap().unwrap().status,
            "timeout"
        );
        assert!(c.job_timed_out("j2").await.unwrap().is_none());

        let d = |id: &str| Dispatch {
            id: id.into(),
            node_id: "n1".into(),
            kind: "session".into(),
            spec_json: "{}".into(),
            ..Default::default()
        };
        c.dispatch_queued(&d("d1")).await.unwrap();
        let started = Completion {
            job_id: "d1".into(),
            status: "ok".into(),
            session_id: "s9".into(),
            ..Default::default()
        };
        match c.complete("n1", &started).await.unwrap() {
            Completed::Dispatch(d) => {
                assert_eq!(
                    (d.status.as_str(), d.session_id.as_str()),
                    ("started", "s9")
                )
            }
            other => panic!("{other:?}"),
        }
        c.dispatch_queued(&d("d2")).await.unwrap();
        let refused = Completion {
            job_id: "d2".into(),
            status: "refused".into(),
            error: "cwd not trusted".into(),
            ..Default::default()
        };
        match c.complete("n1", &refused).await.unwrap() {
            Completed::Dispatch(d) => {
                assert_eq!(d.status, "failed");
                assert_eq!(d.error, "refused: cwd not trusted");
            }
            other => panic!("{other:?}"),
        }
        let unknown = Completion {
            job_id: "zz".into(),
            ..Default::default()
        };
        assert!(c.complete("n1", &unknown).await.is_err());

        // A session naming its dispatch resolves it as started.
        c.dispatch_queued(&d("d3")).await.unwrap();
        let mut s = sess("s3");
        s.dispatch_id = "d3".into();
        c.session_upsert("n1", s).await.unwrap();
        let d3 = c.store().dispatch("d3").await.unwrap().unwrap();
        assert_eq!(
            (d3.status.as_str(), d3.session_id.as_str()),
            ("started", "s3")
        );
    }

    async fn accepting(c: &Core, id: &str, shell: bool) {
        c.session_upsert("n1", sess(id)).await.unwrap();
        c.session_extras_apply(
            "n1",
            id,
            crate::belai::ExtrasUpdate {
                accepts: Some(kls_store::belai::Accepts {
                    prompts: true,
                    controls: true,
                    shell,
                    commands: vec!["/compact".into(), "review".into()],
                    controls_allowed: vec!["mode".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn composer_lines_parse() {
        assert_eq!(parse_composer("hello"), Composed::Prompt);
        assert_eq!(parse_composer("//not a command"), Composed::Prompt);
        assert_eq!(parse_composer("/ x"), Composed::Prompt);
        assert_eq!(
            parse_composer("/mode plan now"),
            Composed::Command("mode".into(), vec!["plan".into(), "now".into()])
        );
        assert_eq!(parse_composer("! ls -la"), Composed::Shell("ls -la".into()));
        let a = kls_store::belai::Accepts {
            commands: vec!["/compact".into(), "shell".into()],
            ..Default::default()
        };
        assert_eq!(composer_completions(Some(&a), "/c"), ["cancel", "compact"]);
    }

    #[tokio::test]
    async fn slash_and_shell_commands() {
        let (c, _d) = core().await;
        accepting(&c, "s1", true).await;
        accepting(&c, "s2", false).await;
        let i = c.queue_composer("s1", "/compact", "owner").await.unwrap();
        assert_eq!(i.kind, "command");
        let i = c
            .queue_composer("s1", "!cargo test", "owner")
            .await
            .unwrap();
        assert!(i.payload_json.contains("\"shell\"") && i.payload_json.contains("cargo test"));
        let i = c
            .queue_composer("s1", "plain words", "owner")
            .await
            .unwrap();
        assert_eq!(i.kind, "prompt");
        // Not offered: an unknown command, shell on a session without it.
        let e = c
            .queue_composer("s1", "/format-disk", "owner")
            .await
            .unwrap_err();
        assert!(matches!(refusal(&e), Some(Refusal::Invalid(_))));
        let e = c.queue_composer("s2", "!ls", "owner").await.unwrap_err();
        assert!(matches!(refusal(&e), Some(Refusal::Invalid(_))));
        // Audited with the origin.
        let led = c.audit_entries(&Default::default()).await.unwrap();
        assert!(led.iter().any(|e| e.kind == "session"
            && e.payload["name"] == "shell"
            && e.payload["origin"] == "owner"));
    }

    #[tokio::test]
    async fn slash_and_shell_refused_under_the_kill_switch() {
        let (c, _d) = core().await;
        accepting(&c, "s1", true).await;
        for mode in [crate::control::Mode::Pause, crate::control::Mode::Stop] {
            crate::control::set_mode(&c, mode, "drill", "owner")
                .await
                .unwrap();
            for line in ["/compact", "/mode plan", "!rm -rf build", "hello"] {
                let e = c.queue_composer("s1", line, "owner").await.unwrap_err();
                assert!(crate::control::is_blocked(&e), "{line}: {e:#}");
            }
            // Winding down still goes out.
            c.queue_composer("s1", "/stop", "owner").await.unwrap();
        }
        let items = c.store().inbox_items("s1", 50).await.unwrap();
        assert_eq!(items.len(), 2);
        c.audit_sync().await.unwrap();
        let refused = c
            .store()
            .recent_events(200)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == crate::control::kinds::FACTORY_REFUSED)
            .count();
        assert_eq!(refused, 8);
    }

    #[tokio::test]
    async fn end_records_the_outcome_and_decisions_are_folded() {
        let (c, _d) = core().await;
        c.session_upsert("n1", sess("s1")).await.unwrap();
        let rm = |idx: i64| Entry {
            idx,
            role: "rolemanager".into(),
            text: String::new(),
            at: 1,
            meta_json: r#"{"activity":"tool check","verdict":"allow"}"#.into(),
        };
        let batch: Vec<Entry> = (0..12).map(rm).collect();
        c.session_append("n1", "s1", &batch).await.unwrap();
        // Resending the same entries emits nothing new.
        c.session_append("n1", "s1", &batch).await.unwrap();
        let evs = c.store().recent_events(500).await.unwrap();
        let d: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == kinds::DECISION_SESSION)
            .collect();
        assert_eq!(d.len(), outcome::DECISIONS_PER_UPLOAD);
        assert!(d.iter().any(|e| e.data_json.contains("\"more\":2")));
        c.session_append(
            "n1",
            "s1",
            &[Entry {
                idx: 12,
                role: "error".into(),
                text: "429 Too Many Requests".into(),
                at: 2,
                meta_json: String::new(),
            }],
        )
        .await
        .unwrap();
        c.session_end("n1", "s1", 5, "").await.unwrap();
        let fin = c
            .store()
            .recent_events(500)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.kind == kinds::SESSION_FINISHED)
            .unwrap();
        assert!(
            fin.data_json.contains("\"outcome\":\"rate_limited\""),
            "{}",
            fin.data_json
        );
        assert_eq!(
            c.session_outcome("s1").await.unwrap().reason,
            "rate_limited"
        );
    }

    #[tokio::test]
    async fn git_strip_actions_are_listed_checked_and_audited() {
        let (c, _d) = core().await;
        accepting(&c, "s1", false).await;
        // The session lists neither branch nor sync.
        let e = c
            .session_git("s1", GitAction::Branch("fix".into()), "owner")
            .await
            .unwrap_err();
        assert!(matches!(refusal(&e), Some(Refusal::Invalid(_))));
        c.session_extras_apply(
            "n1",
            "s1",
            crate::belai::ExtrasUpdate {
                accepts: Some(kls_store::belai::Accepts {
                    controls: true,
                    controls_allowed: vec!["branch".into(), "sync".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        c.session_git("s1", GitAction::Branch("fix".into()), "owner")
            .await
            .unwrap();
        c.session_git("s1", GitAction::Sync(false), "owner")
            .await
            .unwrap();
        let led = c.audit_entries(&Default::default()).await.unwrap();
        assert!(
            led.iter()
                .any(|e| e.kind == "session" && e.msg.contains("branch fix"))
        );
        crate::control::stop(&c, "x").await.unwrap();
        let e = c
            .session_git("s1", GitAction::Sync(true), "owner")
            .await
            .unwrap_err();
        assert!(crate::control::is_blocked(&e));
    }
}
