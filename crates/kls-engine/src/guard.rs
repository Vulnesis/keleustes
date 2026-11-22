//! The non-negotiable rules for engine work (docs/WORKSTREAMS.md): the kill
//! switch is checked first, a refusal emits `engine.refused` and is audited,
//! and every engine action writes a hash-chained ledger entry with its
//! origin.
//!
//! [`halted`] is the one place the engine reads the factory kill switch.

use anyhow::Result;
use kls_core::Core;
use kls_core::engine::kinds;
use kls_store::audit::LedgerDraft;
use serde_json::{Value, json};

/// Why the factory kill switch (paused or stopped, `kls_core::control`)
/// holds new work back now, if it does. Read-only: no event.
pub async fn halted(core: &Core) -> Option<String> {
    match kls_core::control::state(core).await {
        Ok(s) if s.mode.running() => None,
        Ok(s) if s.reason.trim().is_empty() => Some(format!("kill_switch:{}", s.mode.as_str())),
        Ok(s) => Some(format!(
            "kill_switch:{}: {}",
            s.mode.as_str(),
            s.reason.trim()
        )),
        // Fail closed: a control row that cannot be read stops automation.
        Err(e) => Some(format!("kill_switch_unreadable: {e:#}")),
    }
}

/// The maintenance window or schedule blackout (`kls_core::control`
/// windows, read in `schedules.tz`) that holds `action` now, if any.
/// Read-only: no event. Fails closed on an unreadable store.
pub async fn window(core: &Core, action: &str) -> Option<String> {
    match kls_core::control::window_hold(core, action, kls_store::now_ms()).await {
        Ok(w) => w,
        Err(e) => Some(format!("window_unreadable: {e:#}")),
    }
}

/// [`halted`], then [`window`] for `action` ("trigger", "schedule",
/// "dispatch", "job"...): why `action` may not start now, if anything.
pub async fn held(core: &Core, action: &str) -> Option<String> {
    match halted(core).await {
        Some(w) => Some(w),
        None => window(core, action).await,
    }
}

/// The A2A check (`kls_core::control::check_a2a`): refuses while the A2A
/// blackout or the kill switch is on, emitting `a2a.withheld`. No bypass for
/// built-ins, crews or host-to-host work. `Some(reason)` when withheld; the
/// refusal is audited here.
pub async fn a2a(core: &Core, from: &str, to: &str, origin: &str) -> Option<String> {
    match kls_core::control::check_a2a(core, from, to, origin).await {
        Ok(()) => None,
        Err(e) => {
            let why = format!("{e:#}");
            let _ = audit(
                core,
                "a2a",
                "a2a",
                json!({ "from": from, "to": to }),
                &Origin::engine(json!({ "origin": origin })),
                &format!("refused: {why}"),
                json!({}),
            )
            .await;
            Some(why)
        }
    }
}

/// **The** engine-side kill-switch check: `kls_core::control::check`, which
/// refuses while paused or stopped and emits the audited `factory.refused`
/// event. `Some(reason)` when refused.
pub async fn blocked(core: &Core, action: &str, target: &str, origin: &str) -> Option<String> {
    match kls_core::control::check(core, action, target, origin).await {
        Ok(()) => None,
        Err(e) if kls_core::control::is_blocked(&e) => Some(format!("kill_switch: {e}")),
        Err(e) => Some(format!("kill_switch_unreadable: {e:#}")),
    }
}

/// Whether the A2A blackout is on (crew messages and A2A handoffs wait).
pub async fn a2a_blackout(core: &Core) -> bool {
    core.factory_control()
        .await
        .map(|c| c.a2a_blackout)
        .unwrap_or(true)
}

/// Who did something: the ledger's actor and actor kind.
#[derive(Clone, Debug)]
pub struct Origin {
    /// `owner`, `engine`, an agent profile, a node id.
    pub actor: String,
    /// `user`, `agent`, `host`, `system`, `rule`.
    pub kind: &'static str,
    /// Workflow / run / trigger / event provenance.
    pub detail: Value,
}

impl Origin {
    pub fn engine(detail: Value) -> Self {
        Self {
            actor: "engine".into(),
            kind: "rule",
            detail,
        }
    }

    pub fn person(detail: Value) -> Self {
        Self {
            actor: kls_core::board::OWNER.into(),
            kind: "user",
            detail,
        }
    }
}

/// Writes one ledger entry: `action` on `target` by `origin`, with its outcome.
pub async fn audit(
    core: &Core,
    ledger_kind: &str,
    action: &str,
    target: Value,
    origin: &Origin,
    outcome: &str,
    payload: Value,
) -> Result<()> {
    let msg = format!("{action} {outcome}");
    core.audit_record(LedgerDraft {
        at: 0,
        kind: ledger_kind.into(),
        actor: origin.actor.clone(),
        actor_kind: origin.kind.into(),
        msg,
        subject: json!({ "action": action, "target": target }),
        payload: json!({ "origin": origin.detail, "outcome": outcome, "data": payload }),
        origin: "factory".into(),
        node_id: payload
            .get("nodeId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        session_id: String::new(),
        ref_key: None,
    })
    .await?;
    Ok(())
}

/// Rank of an autonomy level (repo levels and the settings' levels on one scale).
pub fn autonomy_rank(level: &str) -> i32 {
    match level {
        "observe" => 0,
        "suggest" => 1,
        "draft_pr" => 2,
        "pr" | "auto_merge_low_risk" => 3,
        "merge" => 4,
        _ => 0,
    }
}

/// The autonomy a `github` output action needs. The factory never merges
/// on its own: `merge` needs a level above any the settings allow.
pub fn required_autonomy(action: &str) -> &'static str {
    match action {
        "draft_pr" | "conflict_cards" => "draft_pr",
        "approve_ready" => "pr",
        "merge" => "merge",
        _ => "suggest",
    }
}

/// The effective autonomy for `repo`: its own level (or `autonomy.default`
/// when it has no settings), capped by `autonomy.max`.
pub async fn effective_autonomy(core: &Core, repo: &str) -> String {
    let eff = kls_core::audit::config::Effective::load(core).await.ok();
    let own = match core.store().gh_repo(repo).await {
        Ok(Some(r)) => r.settings.autonomy,
        _ => eff
            .as_ref()
            .map(|e| e.autonomy_default.clone())
            .unwrap_or_else(|| "observe".into()),
    };
    // Fail closed: unreadable settings cap at observe.
    let max = eff
        .map(|e| e.autonomy_max)
        .unwrap_or_else(|| "observe".into());
    if autonomy_rank(&own) <= autonomy_rank(&max) {
        own
    } else {
        max
    }
}

/// Records a kill-switch refusal through `kls_core::control::check` (the
/// audited `factory.refused` event), naming the target and origin from
/// `data` (`runId` / `scheduleId` / `workflowId`).
pub async fn refuse_switch(core: &Core, action: &str, data: Value) -> Result<()> {
    let s = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let target = [s("runId"), s("scheduleId"), s("workflowId")]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| action.to_string());
    let origin = match s("workflowId") {
        w if !w.is_empty() => format!("workflow:{w}"),
        _ => "engine".to_string(),
    };
    // Err is the refusal itself (already evented and audited).
    let _ = kls_core::control::check(core, action, &target, &origin).await;
    Ok(())
}

/// Records a refusal: an `engine.refused` event and a ledger entry.
pub async fn refuse(core: &Core, what: &str, reason: &str, data: Value) -> Result<()> {
    let mut d = data.clone();
    if !d.is_object() {
        d = json!({});
    }
    d["what"] = json!(what);
    d["reason"] = json!(reason);
    core.emit(kinds::ENGINE_REFUSED, what, &d).await?;
    audit(
        core,
        "refused",
        what,
        data.clone(),
        &Origin::engine(data),
        &format!("refused: {reason}"),
        json!({}),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn audited(core: &Core, kind: &str) -> Vec<kls_store::audit::LedgerEntry> {
    let q = kls_store::audit::LedgerQuery {
        kinds: vec![kind.into()],
        limit: 500,
        ..Default::default()
    };
    core.audit_entries(&q).await.unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refusal_is_evented_and_audited() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        assert!(halted(&core).await.is_none());
        core.set_factory_stopped(true, "drill").await.unwrap();
        assert_eq!(
            halted(&core).await.as_deref(),
            Some("kill_switch:stop: drill")
        );
        let mut rx = core.subscribe();
        refuse(&core, "dispatch", "kill_switch", json!({ "runId": "r1" }))
            .await
            .unwrap();
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.kind, kinds::ENGINE_REFUSED);
        let e = audited(&core, "refused").await.pop().unwrap();
        assert!(e.msg.contains("refused: kill_switch"));
        assert_eq!(e.actor, "engine");
    }
}
