//! What a kill-switch stop undoes in this process:
//!
//! - every open workflow run is cancelled through the engine
//!   (`kls_engine::ops::stop_all_runs`: steps closed, sessions told to stop,
//!   one audited `run.cancel` each);
//! - every card claim is released (`Core::release_all_claims`, one audited
//!   `claim.release` each), so no worker keeps a lease through a stop;
//! - sessions the factory dispatched for a card outside any run are told to
//!   stop, one audited `session.stop` each.
//!
//! Pause undoes nothing: the stopper runs only on stop (see
//! `kls_core::control::set_mode`). The stop itself is the audited
//! `factory.mode` entry, carrying these counts.

use kls_core::Core;
use kls_core::audit::LedgerDraft;
use kls_core::control::{StopReport, Stopper};
use serde_json::json;

/// The actor the ledger records for what a stop undoes.
pub const ACTOR: &str = "kill_switch";

pub struct EngineStopper;

#[async_trait::async_trait]
impl Stopper for EngineStopper {
    async fn stop(&self, core: &Core, reason: &str) -> StopReport {
        let mut r = StopReport::default();
        // Sessions the runs own are stopped by the engine; remember them so
        // the card-dispatch sweep below does not count them twice.
        let mut run_sessions: Vec<String> = Vec::new();
        if let Ok(runs) = core
            .store()
            .runs_in(&kls_store::engine::ACTIVE_RUN_STATUSES)
            .await
        {
            for run in runs {
                if let Ok(s) = kls_engine::ops::run_sessions(core, &run.id).await {
                    run_sessions.extend(s);
                }
            }
        }
        match kls_engine::ops::stop_all_runs(core, reason).await {
            Ok(e) => {
                r.runs_cancelled += e.runs_cancelled;
                r.sessions_stopped += e.sessions_stopped;
            }
            Err(e) => r.errors.push(format!("runs: {e:#}")),
        }
        match core.release_all_claims(ACTOR).await {
            Ok(n) => r.claims_released += n,
            Err(e) => r.errors.push(format!("claims: {e:#}")),
        }
        let open = core
            .store()
            .sessions(&kls_store::sessions::SessionQuery {
                open_only: true,
                limit: 1000,
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        for s in open
            .into_iter()
            .filter(|s| !s.dispatch_id.is_empty() && !run_sessions.contains(&s.id))
        {
            let outcome = match core.queue_command_from(&s.id, "stop", &[], ACTOR).await {
                Ok(_) => {
                    r.sessions_stopped += 1;
                    "stopped".to_string()
                }
                Err(e) => {
                    r.errors.push(format!("session {}: {e:#}", s.id));
                    format!("failed: {e:#}")
                }
            };
            let _ = core
                .audit_record(LedgerDraft {
                    at: 0,
                    kind: "session".into(),
                    actor: ACTOR.into(),
                    actor_kind: "rule".into(),
                    msg: format!("session.stop {} {outcome}", s.id),
                    subject: json!({ "action": "session.stop", "sessionId": s.id }),
                    payload: json!({
                        "outcome": outcome, "reason": reason,
                        "dispatchId": s.dispatch_id, "origin": "factory.mode:stop",
                    }),
                    origin: "factory".into(),
                    node_id: s.node_id.clone(),
                    session_id: s.id.clone(),
                    ref_key: None,
                })
                .await;
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_core::board::CardDraft;
    use kls_core::control::{self, Mode};
    use kls_store::audit::LedgerQuery;
    use kls_store::engine::Run;
    use std::sync::Arc;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        control::register_stopper(&c, Arc::new(EngineStopper));
        (c, dir)
    }

    async fn claimed_card(c: &Core, title: &str) -> String {
        let meta = json!({"belai": {"list": "doing", "agent": {
            "claimedBy": "belai:builder", "claimHost": "node-a",
            "leaseUntil": kls_store::now_ms() + 600_000,
        }}});
        c.create_card(
            CardDraft {
                title: title.into(),
                status: "doing".into(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            "owner",
        )
        .await
        .unwrap()
        .id
    }

    async fn open_run(c: &Core, id: &str, status: &str) {
        let now = kls_store::now_ms();
        let wf = match c.store().workflows(false).await.unwrap().into_iter().next() {
            Some(w) => w,
            None => kls_engine::ops::create_workflow(
                c,
                kls_engine::ops::NewWorkflow {
                    name: "Drill".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        };
        c.store()
            .insert_run(&Run {
                id: id.into(),
                workflow_id: wf.id,
                workflow_version: wf.draft_version.max(1),
                trigger_kind: "manual".into(),
                status: status.into(),
                event_json: "{}".into(),
                inputs_json: "{}".into(),
                created_at: now,
                updated_at: now,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    async fn ledger(c: &Core, action: &str) -> usize {
        c.audit_sync().await.unwrap();
        c.audit_entries(&LedgerQuery {
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap()
        .iter()
        .filter(|e| e.subject["action"] == action)
        .count()
    }

    fn claimed(c: &kls_store::board::Card) -> bool {
        kls_core::board::cards::claim(c).is_claimed()
    }

    #[tokio::test]
    async fn stop_drill_cancels_runs_releases_claims_and_audits_each() {
        let (c, _d) = core().await;
        claimed_card(&c, "one").await;
        claimed_card(&c, "two").await;
        open_run(&c, "run-a", "running").await;
        open_run(&c, "run-b", "queued").await;
        open_run(&c, "run-done", "succeeded").await;

        // Pause holds new work but undoes nothing.
        let p = control::pause(&c, "look").await.unwrap();
        assert_eq!(
            (p.runs_cancelled, p.claims_released, p.sessions_stopped),
            (0, 0, 0)
        );
        assert_eq!(
            c.cards()
                .await
                .unwrap()
                .iter()
                .filter(|x| claimed(x))
                .count(),
            2
        );
        assert_eq!(
            c.store().run("run-a").await.unwrap().unwrap().status,
            "running"
        );
        assert_eq!(ledger(&c, "claim.release").await, 0);

        // Stop (escalating from pause) cancels and releases everything.
        let r = control::stop(&c, "drill").await.unwrap();
        assert_eq!(r.runs_cancelled, 2, "{r:?}");
        assert_eq!(r.claims_released, 2, "{r:?}");
        assert!(r.errors.is_empty(), "{r:?}");
        assert_eq!(control::state(&c).await.unwrap().mode, Mode::Stop);
        for id in ["run-a", "run-b"] {
            assert_eq!(
                c.store().run(id).await.unwrap().unwrap().status,
                "cancelled"
            );
        }
        assert_eq!(
            c.store().run("run-done").await.unwrap().unwrap().status,
            "succeeded"
        );
        assert!(c.cards().await.unwrap().iter().all(|x| !claimed(x)));

        // One ledger entry per cancelled run and per released claim, and the
        // stop itself carries the counts.
        assert_eq!(ledger(&c, "run.cancel").await, 2);
        assert_eq!(ledger(&c, "claim.release").await, 2);
        let modes: Vec<_> = c
            .audit_entries(&LedgerQuery {
                limit: 1000,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "factory_control" && e.msg.contains("factory.mode"))
            .collect();
        assert_eq!(modes.len(), 2, "pause and stop");
        assert!(
            modes
                .iter()
                .any(|e| e.payload["claimsReleased"] == 2 && e.payload["runsCancelled"] == 2)
        );
        // New work is refused while stopped.
        assert!(control::check(&c, "dispatch", "x", "drill").await.is_err());
    }

    #[tokio::test]
    async fn stop_with_nothing_open_is_clean() {
        let (c, _d) = core().await;
        let r = control::stop(&c, "").await.unwrap();
        assert_eq!(r, StopReport::default());
        control::resume(&c).await.unwrap();
        assert!(control::check(&c, "dispatch", "x", "drill").await.is_ok());
    }
}
