//! Budget pushes to running work (belai-counterparts #33): when a budget's
//! limits change, or a gate downgrades or pauses work, Keleustes sends a
//! `budget_update` dispatch to every online node with live work, and belai
//! applies it to the workers it runs (new caps tighten only, a downgrade
//! moves to the fast model, a pause finishes the card in hand).
//!
//! Rules: every push records its provenance (the budget, the event or
//! person that caused it, the run it narrows to) in the dispatch trigger and
//! in an audit entry; the kill switch refuses it (`control::check`, audited
//! as `factory.refused`); a push never crosses from one host to another, so
//! the A2A blackout does not apply. The events come from
//! `vault::budget_gate_scoped` (the engine's gate before model work) and
//! `vault::save_budget` (the Budgets screen); [`on_event`] runs in the
//! process that serves nodes (`sweeps::start`).

use crate::Core;
use crate::gateway::{BUDGET_PUSH_ACTIONS, BudgetPush, DispatchSpec, Trigger, WorkerBudget};
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub mod kinds {
    /// A budget change went out to running work. Subject: budget id.
    pub const BUDGET_PUSHED: &str = "budget.pushed";
}

/// A gate's downgrade or pause for the same budget and run is sent at most
/// once in this window (the gate runs before every step).
const REPEAT_MS: i64 = 10 * 60 * 1000;

/// What to push and why.
#[derive(Clone, Debug, Default)]
pub struct PushRequest {
    pub push: BudgetPush,
    /// "limits": the new caps per item.
    pub budget: Option<WorkerBudget>,
    /// "downgrade": the fast model to move to.
    pub provider: String,
    pub model: String,
    /// The engine run the push narrows to ("" for all running work).
    pub run_id: String,
    /// Only this node ("" for every node with live work).
    pub node_id: String,
}

/// What a push did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PushReport {
    /// (node, dispatch id) per dispatch sent.
    pub sent: Vec<(String, String)>,
    /// (node, why) per node that took nothing.
    pub skipped: Vec<(String, String)>,
    /// The kill switch's refusal, when it refused.
    pub refused: Option<String>,
}

fn recent() -> &'static Mutex<HashMap<String, i64>> {
    static R: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Whether the same push went out within [`REPEAT_MS`]; records it if not.
fn repeated(key: &str, now: i64) -> bool {
    let mut m = recent().lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|_, at| now - *at < REPEAT_MS);
    if m.contains_key(key) {
        return true;
    }
    m.insert(key.to_string(), now);
    false
}

/// Whether the node reports live work (sessions or workers), or reports
/// nothing yet.
async fn has_live_work(core: &Core, node_id: &str) -> bool {
    match core
        .node_statuses()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|(n, _, _)| n == node_id)
    {
        None => true,
        Some((_, _, s)) => {
            s.running_sessions > 0
                || s.workers
                    .iter()
                    .any(|w| !matches!(w.state.as_str(), "stopped" | "failed"))
        }
    }
}

/// Sends `req` to running work. `origin` is who asked: `budget:<id>` for a
/// saved budget, `run:<id>` for the engine's gate, `owner` for a person.
pub async fn push(core: &Core, mut req: PushRequest, origin: &str) -> Result<PushReport> {
    let mut report = PushReport::default();
    let action = req.push.action.clone();
    if !BUDGET_PUSH_ACTIONS.contains(&action.as_str()) {
        bail!("unknown budget push {action:?}");
    }
    if action == "downgrade" && (req.model.is_empty() || req.push.tier.is_empty()) {
        bail!("a downgrade names the fast tier and its model");
    }
    let target = if req.node_id.is_empty() {
        "budget_update".to_string()
    } else {
        req.node_id.clone()
    };
    if let Err(e) = crate::control::check(core, "dispatch", &target, origin).await {
        report.refused = Some(e.to_string());
        return Ok(report);
    }
    let Ok(gw) = core.gateway() else {
        return Ok(report);
    };
    // A run narrows to its open sessions, on the nodes that run them.
    let mut run_nodes: Option<Vec<String>> = None;
    if !req.run_id.is_empty() {
        let open = core
            .store()
            .sessions(&kls_store::sessions::SessionQuery {
                run_id: Some(req.run_id.clone()),
                open_only: true,
                ..Default::default()
            })
            .await?;
        for s in &open {
            if !req.push.session_ids.contains(&s.id) {
                req.push.session_ids.push(s.id.clone());
            }
        }
        run_nodes = Some(open.into_iter().map(|s| s.node_id).collect());
    }
    req.push.session_ids.truncate(256);
    let mut online = gw.online().await;
    online.sort();
    online.dedup();
    for node in online {
        if !req.node_id.is_empty() && node != req.node_id {
            continue;
        }
        if let Some(rn) = &run_nodes
            && !rn.contains(&node)
        {
            report
                .skipped
                .push((node, "runs none of the run's work".into()));
            continue;
        }
        let takes = core.store().node(&node).await?.is_some_and(|n| {
            n.revoked_at.is_none() && n.host.capabilities.takes_dispatch("budget_update")
        });
        if !takes {
            report
                .skipped
                .push((node, "does not take budget updates (update Belai)".into()));
            continue;
        }
        if run_nodes.is_none() && !has_live_work(core, &node).await {
            report.skipped.push((node, "no live work".into()));
            continue;
        }
        let spec = DispatchSpec {
            kind: "budget_update".into(),
            provider: req.provider.clone(),
            model: req.model.clone(),
            run_id: req.run_id.clone(),
            budget: req.budget.clone(),
            budget_push: Some(req.push.clone()),
            trigger: Some(Trigger {
                source: if req.run_id.is_empty() {
                    "console".into()
                } else {
                    "workflow".into()
                },
                run_id: req.run_id.clone(),
                subject: format!("budget:{}", req.push.budget_id),
                ..Default::default()
            }),
            origin_json:
                json!({ "actor": origin, "runId": req.run_id, "keyId": req.push.budget_id })
                    .to_string(),
            ..Default::default()
        };
        match gw.dispatch(&node, spec).await {
            Ok(id) => report.sent.push((node, id)),
            Err(e) => report.skipped.push((node, format!("{e:#}"))),
        }
    }
    let data = json!({
        "budgetId": req.push.budget_id, "name": req.push.budget_name, "action": action,
        "tier": req.push.tier, "reason": req.push.reason, "runId": req.run_id,
        "sessionIds": req.push.session_ids, "origin": origin,
        "sent": report.sent, "skipped": report.skipped,
    });
    core.emit(kinds::BUDGET_PUSHED, &req.push.budget_id, &data)
        .await?;
    core.audit_record(kls_store::audit::LedgerDraft {
        kind: "budget_push".into(),
        actor: origin.into(),
        actor_kind: if origin.starts_with("run:") || origin.starts_with("budget:") {
            "system"
        } else {
            "user"
        }
        .into(),
        msg: format!(
            "budget {} {action}: {} node(s), {} skipped",
            req.push.budget_name,
            report.sent.len(),
            report.skipped.len()
        ),
        subject: json!({ "budget": req.push.budget_id, "run": req.run_id }),
        payload: data,
        origin: "factory".into(),
        ..Default::default()
    })
    .await?;
    Ok(report)
}

/// The per-item caps a budget leaves: what is left of its token and USD
/// limits this period (`None` when it sets neither).
pub fn caps_left(st: &crate::vault::BudgetStatus) -> Option<WorkerBudget> {
    let b = &st.budget;
    let usd = b.limit_usd.map(|l| (l - st.spent_usd).max(0.0));
    let tokens = b
        .limit_tokens
        .map(|l| (l - st.spent_tokens).clamp(0, u32::MAX as i64) as u32);
    if usd.is_none() && tokens.is_none() {
        return None;
    }
    Some(WorkerBudget {
        max_cost_usd: usd.unwrap_or(0.0),
        max_tokens: tokens.unwrap_or(0),
        ..Default::default()
    })
}

fn run_of(origin: &str) -> String {
    origin.strip_prefix("run:").unwrap_or("").to_string()
}

/// Turns budget events into pushes: a saved budget sends its new caps (and
/// a resume when it has room again after a pause), a gate's downgrade or
/// pause sends that to the run's work.
pub async fn on_event(core: &Core, ev: &kls_store::Event) {
    use crate::vault::kinds as vk;
    if ![vk::BUDGET_SAVED, vk::BUDGET_LIMIT, vk::BUDGET_BLOCKED].contains(&ev.kind.as_str()) {
        return;
    }
    if let Err(e) = on_budget_event(core, ev).await {
        tracing::warn!(budget = %ev.subject, "budget push: {e:#}");
    }
}

async fn on_budget_event(core: &Core, ev: &kls_store::Event) -> Result<()> {
    use crate::vault::kinds as vk;
    let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(json!({}));
    let s = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let now = kls_store::now_ms();
    if ev.kind == vk::BUDGET_SAVED {
        let Some(st) = crate::vault::budget_statuses(core)
            .await?
            .into_iter()
            .find(|st| st.budget.id == ev.subject)
        else {
            return Ok(());
        };
        let b = &st.budget;
        let node_id = if b.applies_to == "node" {
            b.subject.clone()
        } else {
            String::new()
        };
        let base = BudgetPush {
            budget_id: b.id.clone(),
            budget_name: b.name.clone(),
            ..Default::default()
        };
        let origin = format!("budget:{}", b.id);
        push(
            core,
            PushRequest {
                push: BudgetPush {
                    action: "limits".into(),
                    reason: format!("budget {} saved", b.name),
                    ..base.clone()
                },
                budget: caps_left(&st),
                node_id: node_id.clone(),
                ..Default::default()
            },
            &origin,
        )
        .await?;
        // A budget that has room again lifts the pause its limit caused.
        let paused_key = format!("pause:{}", b.id);
        let was_paused = recent()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&paused_key);
        if was_paused && st.pct < 100.0 {
            recent()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&paused_key);
            push(
                core,
                PushRequest {
                    push: BudgetPush {
                        action: "resume".into(),
                        reason: format!("budget {} has room again", b.name),
                        ..base
                    },
                    node_id,
                    ..Default::default()
                },
                &origin,
            )
            .await?;
        }
        return Ok(());
    }
    let outcome = s("outcome");
    let origin = match s("origin") {
        o if o.is_empty() => "budget".to_string(),
        o => o,
    };
    let run_id = run_of(&origin);
    let base = BudgetPush {
        budget_id: s("budgetId"),
        budget_name: s("name"),
        reason: match s("reason") {
            r if r.is_empty() => format!("budget {} at its limit", s("name")),
            r => r,
        },
        ..Default::default()
    };
    match outcome.as_str() {
        "downgrade" => {
            if repeated(&format!("downgrade:{}:{run_id}", base.budget_id), now) {
                return Ok(());
            }
            let fast = core
                .store()
                .setting(crate::vault::settings::MODELS_FAST)
                .await?
                .unwrap_or_default();
            let Some((provider, model)) = fast.split_once('/') else {
                return Ok(());
            };
            push(
                core,
                PushRequest {
                    push: BudgetPush {
                        action: "downgrade".into(),
                        tier: "fast".into(),
                        ..base
                    },
                    provider: provider.into(),
                    model: model.into(),
                    run_id,
                    node_id: s("nodeId"),
                    ..Default::default()
                },
                &origin,
            )
            .await?;
        }
        "pause" => {
            if repeated(&format!("pause:{}:{run_id}", base.budget_id), now) {
                return Ok(());
            }
            recent()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(format!("pause:{}", base.budget_id), now);
            push(
                core,
                PushRequest {
                    push: BudgetPush {
                        action: "pause".into(),
                        ..base
                    },
                    run_id,
                    node_id: s("nodeId"),
                    ..Default::default()
                },
                &origin,
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{JobNeeds, JobOutcome, NodeGateway};
    use std::sync::Arc;

    #[derive(Default)]
    struct Gw {
        sent: Mutex<Vec<(String, DispatchSpec)>>,
    }

    #[async_trait::async_trait]
    impl NodeGateway for Gw {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into(), "n2".into()]
        }
        async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
            let mut s = self.sent.lock().unwrap();
            s.push((node_id.into(), spec));
            Ok(format!("d{}", s.len()))
        }
        async fn job(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: JobNeeds,
            _: i64,
        ) -> Result<JobOutcome> {
            bail!("no jobs")
        }
    }

    async fn setup() -> (Core, Arc<Gw>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let gw = Arc::new(Gw::default());
        c.set_gateway(gw.clone());
        let mut new = kls_store::HostInfo::default();
        new.capabilities.dispatch_kinds = vec!["session".into(), "budget_update".into()];
        c.store()
            .insert_node("n1", "one", &new, "s1", "t1")
            .await
            .unwrap();
        // n2 predates budget updates.
        c.store()
            .insert_node("n2", "two", &kls_store::HostInfo::default(), "s2", "t2")
            .await
            .unwrap();
        (c, gw, dir)
    }

    fn budget(id: &str) -> kls_store::vault::Budget {
        kls_store::vault::Budget {
            id: id.into(),
            name: "Monthly".into(),
            applies_to: "all".into(),
            provider: "all".into(),
            period: "month".into(),
            limit_usd: Some(10.0),
            action_at_limit: "pause_run".into(),
            ..Default::default()
        }
    }

    async fn ledger(c: &Core, kind: &str) -> Vec<kls_store::audit::LedgerEntry> {
        c.store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec![kind.into()],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn saving_a_budget_pushes_its_caps_with_provenance_and_audit() {
        let (c, gw, _d) = setup().await;
        let b = crate::vault::save_budget(&c, &budget("")).await.unwrap();
        let ev = c
            .store()
            .events_after(0, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.kind == crate::vault::kinds::BUDGET_SAVED)
            .unwrap();
        on_event(&c, &ev).await;
        let sent = gw.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "only the node that takes budget updates");
        let (node, spec) = &sent[0];
        assert_eq!(node, "n1");
        assert_eq!(spec.kind, "budget_update");
        let p = spec.budget_push.as_ref().unwrap();
        assert_eq!(p.action, "limits");
        assert_eq!(p.budget_id, b.id);
        assert_eq!(spec.budget.as_ref().unwrap().max_cost_usd, 10.0);
        let t = spec.trigger.as_ref().unwrap();
        assert_eq!(t.subject, format!("budget:{}", b.id));
        assert!(spec.origin_json.contains(&format!("budget:{}", b.id)));
        let e = ledger(&c, "budget_push").await;
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].payload["action"], "limits");
        assert_eq!(e[0].payload["skipped"][0][0], "n2");
    }

    #[tokio::test]
    async fn a_gate_pause_narrows_to_the_run_once_and_resumes_on_room() {
        let (c, gw, _d) = setup().await;
        let b = crate::vault::save_budget(&c, &budget("b-pause"))
            .await
            .unwrap();
        c.store()
            .upsert_session(&kls_store::sessions::Session {
                id: "s-1".into(),
                node_id: "n1".into(),
                run_id: "r-1".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let ev = c
            .emit(
                crate::vault::kinds::BUDGET_BLOCKED,
                &b.id,
                &json!({ "budgetId": b.id, "name": b.name, "outcome": "pause", "origin": "run:r-1" }),
            )
            .await
            .unwrap();
        on_event(&c, &ev).await;
        on_event(&c, &ev).await;
        let sent = gw.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "the gate repeats; the push does not");
        let p = sent[0].1.budget_push.clone().unwrap();
        assert_eq!(p.action, "pause");
        assert_eq!(p.session_ids, vec!["s-1".to_string()]);
        assert_eq!(sent[0].1.run_id, "r-1");
        assert_eq!(sent[0].1.trigger.as_ref().unwrap().source, "workflow");
        // Saved with room: new caps, then a resume.
        let saved = c
            .emit(
                crate::vault::kinds::BUDGET_SAVED,
                &b.id,
                &json!({ "id": b.id }),
            )
            .await
            .unwrap();
        on_event(&c, &saved).await;
        let actions: Vec<String> = gw
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| s.budget_push.clone().unwrap().action)
            .collect();
        assert_eq!(actions, vec!["pause", "limits", "resume"]);
    }

    #[tokio::test]
    async fn the_kill_switch_refuses_a_push_and_audits_it() {
        let (c, gw, _d) = setup().await;
        crate::control::pause(&c, "test").await.unwrap();
        let r = push(
            &c,
            PushRequest {
                push: BudgetPush {
                    action: "pause".into(),
                    budget_id: "b1".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap();
        assert!(r.refused.is_some());
        assert!(gw.sent.lock().unwrap().is_empty());
        assert!(!ledger(&c, "budget_push").await.iter().any(|_| true));
        let refused = c
            .store()
            .events_after(0, 500)
            .await
            .unwrap()
            .into_iter()
            .any(|e| e.kind == crate::control::kinds::FACTORY_REFUSED);
        assert!(
            refused,
            "the refusal is a factory.refused event (folded into the ledger)"
        );
    }

    #[tokio::test]
    async fn a_downgrade_needs_the_fast_model() {
        let (c, gw, _d) = setup().await;
        let ev = c
            .emit(
                crate::vault::kinds::BUDGET_LIMIT,
                "b9",
                &json!({ "budgetId": "b9", "name": "x", "outcome": "downgrade", "origin": "owner" }),
            )
            .await
            .unwrap();
        on_event(&c, &ev).await;
        assert!(
            gw.sent.lock().unwrap().is_empty(),
            "no models.fast: nothing to move to"
        );
        c.store()
            .set_setting(
                crate::vault::settings::MODELS_FAST,
                "anthropic/claude-haiku",
            )
            .await
            .unwrap();
        let ev = c
            .emit(
                crate::vault::kinds::BUDGET_LIMIT,
                "b10",
                &json!({ "budgetId": "b10", "name": "x", "outcome": "downgrade", "origin": "owner" }),
            )
            .await
            .unwrap();
        on_event(&c, &ev).await;
        let sent = gw.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1.model, "claude-haiku");
        assert_eq!(sent[0].1.budget_push.as_ref().unwrap().tier, "fast");
    }
}
