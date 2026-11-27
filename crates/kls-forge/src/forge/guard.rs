//! The non-negotiable rules on the forge plane (docs/WORKSTREAMS.md):
//! every write, sign-in, connection change, conflict commit and poll pass
//! records its origin in the hash-chained audit ledger, and automation
//! (the poller, workflow runs) is refused while the factory is stopped.
//!
//! A person's click is not automation: it runs under the kill switch, and
//! is audited with the person as the actor. A2A does not apply here: no
//! forge path carries a message between agents.

use crate::GhError;
use anyhow::Result;
use kls_core::Core;
use kls_core::github::kinds;
use kls_store::audit::LedgerDraft;
use serde_json::{Value, json};

/// The actor kind and actor of a caller tag (`user`, `cli`, `poller`,
/// `factory:<workflow>`).
pub fn actor(caller: &str) -> (&'static str, String) {
    match caller {
        "user" => ("user", "owner".into()),
        "cli" => ("user", "owner (cli)".into()),
        "poller" => ("system", "poller".into()),
        c if c.starts_with("factory:") => ("rule", format!("workflow {}", &c[8..])),
        c => ("system", c.into()),
    }
}

/// Whether a caller is automation (refused while the factory is stopped).
pub fn automated(caller: &str) -> bool {
    !matches!(caller, "user" | "cli")
}

/// Writes one ledger entry: actor, action, target, origin and outcome.
pub async fn audit(
    core: &Core,
    caller: &str,
    action: &str,
    target: &str,
    connection: &str,
    outcome: &str,
    detail: Value,
) {
    let (actor_kind, actor) = actor(caller);
    let d = LedgerDraft {
        at: 0,
        kind: "forge".into(),
        actor,
        actor_kind: actor_kind.into(),
        msg: format!("forge {action} {target} {outcome}"),
        subject: json!({ "action": action, "target": target, "connection": connection }),
        payload: json!({ "caller": caller, "outcome": outcome, "detail": detail }),
        origin: "factory".into(),
        node_id: String::new(),
        session_id: String::new(),
        ref_key: None,
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(action, target, "forge audit: {e:#}");
    }
}

/// Refuses automation while the factory is stopped: emits `forge.refused`,
/// audits the refusal and returns [`GhError::Stopped`].
pub async fn check_kill(
    core: &Core,
    caller: &str,
    action: &str,
    target: &str,
    connection: &str,
) -> Result<()> {
    if !automated(caller) {
        return Ok(());
    }
    // The one kill-switch API (pause and stop; emits the audited
    // `factory.refused`). The stopped flag is read too, so a store that
    // predates the mode still refuses.
    let blocked = kls_core::control::check(core, "provision", target, caller).await;
    let c = core.factory_control().await?;
    if blocked.is_ok() && !c.stopped {
        return Ok(());
    }
    let why = match (&blocked, c.stopped_reason.is_empty()) {
        (_, false) => c.stopped_reason.clone(),
        (Err(e), true) => format!("{e:#}"),
        (Ok(()), true) => "kill switch on".to_string(),
    };
    let _ = core
        .emit(
            kinds::REFUSED,
            target,
            &json!({ "action": action, "caller": caller, "connection": connection,
                     "reason": "factory_stopped", "why": why }),
        )
        .await;
    audit(
        core,
        caller,
        action,
        target,
        connection,
        "refused",
        json!({ "reason": "factory_stopped", "why": why }),
    )
    .await;
    Err(GhError::Stopped(why).into())
}

/// Whether branch protection applies to `repo`: Settings `forge.protected`
/// and the repository's own "Protect branches" switch (both default on).
pub async fn protection_on(core: &Core, repo: &str) -> bool {
    let global = kls_core::audit::config::Effective::load(core)
        .await
        .map(|e| e.protect_branches)
        .unwrap_or(true);
    let own = match core.store().gh_repo(repo).await {
        Ok(Some(r)) => r.settings.protected_branches,
        _ => true,
    };
    global && own
}

/// Why the factory must not write to `branch` of `repo` ("" when it may):
/// with protection on, the default branch and any branch the forge marks
/// protected are refused. A forge that cannot say counts the default only.
pub async fn protected_branch(c: &crate::Client, repo: &str, branch: &str, caller: &str) -> String {
    let core = c.core();
    if branch.trim().is_empty() || !protection_on(core, repo).await {
        return String::new();
    }
    let default = core
        .store()
        .gh_repo(repo)
        .await
        .ok()
        .flatten()
        .map(|r| r.default_branch)
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "main".into());
    if branch == default {
        return format!("{branch} is the default branch (forge.protected)");
    }
    match c.api().branch_protected(c, repo, branch, caller).await {
        Ok(Some(true)) => format!("{branch} is a protected branch (forge.protected)"),
        _ => String::new(),
    }
}

/// Audits and emits a refusal that is not the kill switch (a protected
/// branch, a capability the forge lacks, a readiness rule).
pub async fn refuse(
    core: &Core,
    caller: &str,
    action: &str,
    target: &str,
    connection: &str,
    reason: &str,
    why: &str,
) {
    let _ = core
        .emit(
            kinds::REFUSED,
            target,
            &json!({ "action": action, "caller": caller, "connection": connection,
                     "reason": reason, "why": why }),
        )
        .await;
    audit(
        core,
        caller,
        action,
        target,
        connection,
        "refused",
        json!({ "reason": reason, "why": why }),
    )
    .await;
}

/// "ok" or the error's first line, for an audit outcome.
pub fn outcome<T>(r: &Result<T>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => {
            let s = format!("error: {e:#}");
            s.chars().take(200).collect()
        }
    }
}
