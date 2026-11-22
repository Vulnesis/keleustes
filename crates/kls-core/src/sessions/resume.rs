//! Resume or fork a belai session on its host (belai-counterparts #46), and
//! run an approved plan (#52). Each sends a `session` dispatch whose
//! `resume_session` names the stored session; the host starts a new session
//! that continues its conversation (from `fork_entry` when set).
//!
//! The kill switch is checked first (pause and stop refuse: resuming starts
//! work), an agent's request also passes the A2A blackout, the trigger names
//! the console with the session as its subject, and the outcome is audited
//! either way. The gateway refuses a node that does not advertise the
//! `resume` / `fork` dispatch feature.

use super::start::{audit, kinds, refuse};
use crate::Core;
use crate::control;
use crate::gateway::{DispatchSpec, Trigger};
use crate::hosts::Origin;
use anyhow::{Result, bail};
use serde_json::json;

/// The prompt a resume sends when the person typed none.
pub const DEFAULT_PROMPT: &str = "Continue.";
/// The prompt that carries out an approved plan.
pub const EXECUTE_PROMPT: &str = "Execute the approved plan.";

/// What a resume asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resume {
    /// The stored (belai) session id.
    pub session_id: String,
    /// A belai entry id (`Entry.meta_json` "id") to fork at; empty resumes.
    pub fork_entry: String,
    /// The new session's first prompt ("" = [`DEFAULT_PROMPT`]).
    pub prompt: String,
    /// Overrides; empty copies the stored session's.
    pub mode: String,
    pub provider: String,
    pub model: String,
    /// The trigger subject; empty derives `resume:<sid>` / `fork:<sid>@<entry>`.
    pub subject: String,
}

/// A belai id as the host takes it: short, plain.
fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Whether an origin is an agent's rather than a person's.
pub(crate) fn by_agent(origin: &Origin) -> bool {
    !(origin.actor == "owner" || origin.actor.starts_with("owner:"))
}

/// Sends the resume (or fork) of `r.session_id`. Returns the dispatch id.
pub async fn resume(core: &Core, r: Resume, origin: &Origin) -> Result<String> {
    let s = core
        .store()
        .session(&r.session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown session"))?;
    let node = s.node_id.clone();
    let fork = !r.fork_entry.is_empty();
    let subject = if !r.subject.is_empty() {
        r.subject.clone()
    } else if fork {
        format!("fork:{}@{}", s.id, r.fork_entry)
    } else {
        format!("resume:{}", s.id)
    };
    let mut spec = DispatchSpec {
        kind: "session".into(),
        cwd: s.cwd.clone(),
        repo: s.repo.clone(),
        prompt: if r.prompt.trim().is_empty() {
            DEFAULT_PROMPT.into()
        } else {
            r.prompt.trim().into()
        },
        provider: if r.provider.is_empty() {
            s.provider.clone()
        } else {
            r.provider.clone()
        },
        model: if r.model.is_empty() {
            s.model.clone()
        } else {
            r.model.clone()
        },
        mode: if r.mode.is_empty() {
            s.mode.clone()
        } else {
            r.mode.clone()
        },
        card_id: s.card_id.clone(),
        resume_session: s.id.clone(),
        fork_entry: r.fork_entry.clone(),
        trigger: Some(Trigger {
            source: "console".into(),
            card_id: s.card_id.clone(),
            subject: subject.clone(),
            ..Default::default()
        }),
        origin_json: json!({
            "actor": origin.actor, "surface": origin.surface, "runId": origin.run_id,
            "cardId": s.card_id, "resumeOf": s.id, "forkEntry": r.fork_entry,
        })
        .to_string(),
        ..Default::default()
    };
    if !matches!(
        spec.mode.as_str(),
        "" | "agent" | "plan" | "goal" | "code" | "auto"
    ) {
        spec.mode.clear();
    }
    let why = if let Err(e) = control::check(core, "dispatch", &node, &origin.label()).await {
        match e.downcast_ref::<control::Blocked>() {
            Some(b) if b.window => Some("held".to_string()),
            Some(b) => Some(format!("kill switch: {}", b.mode.as_str())),
            None => return Err(e),
        }
    } else if by_agent(origin)
        && let Err(e) = control::check_a2a(core, &origin.actor, &node, &origin.label()).await
    {
        Some(format!("{e:#}"))
    } else if !s.harness_free() {
        Some("a harness session resumes through its own harness".into())
    } else if s.node_id.is_empty() {
        Some("the session has no node".into())
    } else if !id_ok(&s.id) {
        Some("that is not a belai session id".into())
    } else if fork && !id_ok(&r.fork_entry) {
        Some("that is not a belai entry id".into())
    } else if s.is_open() {
        Some("the session is still running; resume an ended session".into())
    } else {
        None
    };
    if let Some(why) = why {
        refuse(core, &node, &spec, origin, &why).await;
        bail!("refused: {why}");
    }
    let id = match core.gateway()?.dispatch(&node, spec.clone()).await {
        Ok(id) => id,
        Err(e) => {
            // The gateway audited its own refusal; this line names the
            // console's request.
            refuse(core, &node, &spec, origin, &format!("{e:#}")).await;
            return Err(e);
        }
    };
    let what = if fork { "fork" } else { "resume" };
    let data = json!({
        "nodeId": node, "kind": "session", "resumeOf": s.id, "forkEntry": r.fork_entry,
        "subject": subject, "origin": origin, "dispatchId": id,
    });
    core.emit(kinds::START_DISPATCHED, &id, &data).await?;
    audit(
        core,
        "dispatch",
        "user",
        format!(
            "{what} of session {} as dispatch {id} by {}",
            s.id,
            origin.label()
        ),
        json!({ "node": node, "kind": "session", "dispatch": id, "session": s.id }),
        json!({ "outcome": "dispatched", "action": what, "forkEntry": r.fork_entry,
                "subject": subject, "origin": origin }),
        &node,
    )
    .await;
    Ok(id)
}

/// Runs the plan an ended plan-mode session wrote, after a person approved
/// it: a resume in agent mode whose trigger subject is `plan:<sha256>`.
pub async fn execute_plan(core: &Core, session_id: &str, origin: &Origin) -> Result<String> {
    let sha = core
        .store()
        .session_extras(session_id)
        .await?
        .map(|x| x.plan_sha256)
        .unwrap_or_default();
    if sha.is_empty() {
        bail!("the session wrote no plan file");
    }
    resume(
        core,
        Resume {
            session_id: session_id.into(),
            prompt: EXECUTE_PROMPT.into(),
            mode: "agent".into(),
            subject: format!("plan:{sha}"),
            ..Default::default()
        },
        origin,
    )
    .await
}

trait HarnessFree {
    fn harness_free(&self) -> bool;
}

impl HarnessFree for kls_store::sessions::Session {
    fn harness_free(&self) -> bool {
        self.kind != "harness"
    }
}

#[cfg(test)]
mod tests;
