//! Governance background work: lost sessions, retention and estimate
//! records. Started once by the app ([`start`]); each step is also a plain
//! function for tests and the CLI.
//!
//! - **Lost sessions**: when a node goes offline and stays offline for
//!   [`LOST_GRACE_MS`], its open sessions end with reason `lost`
//!   (`session.ended`, so crew steps settle) and a `session.lost` event is
//!   audited.
//! - **Retention**: once a day, events older than `events.retention_days`
//!   (default 30) are pruned, but only those the audit ledger already folded.
//!   Ended sessions older than `persistence.retention` (400d / 90d / 30d /
//!   forever) are deleted with a ledger tombstone. Each prune is a
//!   `retention.pruned` event.
//! - **Session facts**: every `session.*` event folds what is new into the
//!   session fact index (`audit::facts`); a deleted session drops its facts.
//! - **Host action lines** older than 30 days are pruned with the retention run.
//! - **Estimates**: every `run.finished` and `session.ended` records an
//!   estimate with its inputs (`estimate.recorded`).
//! - **Delivery**: every five minutes, [`delivery_tick`] runs the lifecycle
//!   sweep and the roll-up (`github::lifecycle_sweep`, `github::rollup`,
//!   origin `sweep`; refused under the kill switch or a schedule blackout).

use crate::Core;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

pub mod kinds {
    pub const SESSION_LOST: &str = "session.lost";
    pub const RETENTION_PRUNED: &str = "retention.pruned";
}

pub mod keys {
    /// Days of the event stream kept (default 30).
    pub const EVENTS_RETENTION_DAYS: &str = "events.retention_days";
    /// Sessions and transcripts kept: "400d", "90d", "30d", "forever".
    pub const PERSISTENCE_RETENTION: &str = "persistence.retention";
    /// Last retention run (Unix ms).
    pub const LAST_RETENTION: &str = "retention.last_run";
}

pub const LOST_GRACE_MS: i64 = 10 * 60_000;
pub const DEFAULT_EVENT_DAYS: i64 = 30;
/// Host action output lines are kept this long.
pub const HOST_LINES_KEEP_MS: i64 = 30 * DAY_MS;
const DAY_MS: i64 = 86_400_000;

/// Whether `node_id` has come back online since `since` (an online event
/// later than it), or is online now through the gateway.
async fn back_online(core: &Core, node_id: &str, since_id: i64) -> Result<bool> {
    if let Ok(gw) = core.gateway()
        && gw.online().await.iter().any(|n| n == node_id)
    {
        return Ok(true);
    }
    let later = core.store().events_after(since_id, 10_000).await?;
    Ok(later
        .iter()
        .any(|e| e.kind == crate::kinds::NODE_ONLINE && e.subject == node_id))
}

/// Ends the open sessions of `node_id` as lost, unless it came back after
/// the offline event `offline_id`. Returns how many ended.
pub async fn mark_lost(core: &Core, node_id: &str, offline_id: i64) -> Result<usize> {
    if back_online(core, node_id, offline_id).await? {
        return Ok(0);
    }
    let open = core
        .store()
        .sessions(&kls_store::sessions::SessionQuery {
            node_id: Some(node_id.to_string()),
            open_only: true,
            limit: 1000,
            ..Default::default()
        })
        .await?;
    let mut n = 0;
    for s in open {
        core.session_end(node_id, &s.id, kls_store::now_ms(), "lost")
            .await?;
        core.emit(
            kinds::SESSION_LOST,
            &s.id,
            &json!({
                "nodeId": node_id, "sessionId": s.id, "runId": s.run_id,
                "cardId": s.card_id, "reason": "node offline", "graceMs": LOST_GRACE_MS,
            }),
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

fn retention_days(v: &str) -> Option<i64> {
    match v.trim() {
        "forever" => None,
        s => s
            .trim_end_matches('d')
            .parse::<i64>()
            .ok()
            .filter(|d| *d > 0),
    }
}

/// Runs retention once at `now`. Returns (events pruned, sessions deleted).
pub async fn retention_at(core: &Core, now: i64) -> Result<(u64, usize)> {
    let s = core.store();
    // Fold first so nothing is pruned before the ledger has it.
    core.audit_sync().await?;
    let cursor: i64 = s
        .setting(crate::audit::keys::EVENT_CURSOR)
        .await?
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let days = s
        .setting(keys::EVENTS_RETENTION_DAYS)
        .await?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d >= 1)
        .unwrap_or(DEFAULT_EVENT_DAYS);
    let events = s.prune_events(now - days * DAY_MS, cursor).await?;
    // Host action output lines (host-management R6), 30 days.
    let lines = s.prune_host_action_lines(now - HOST_LINES_KEEP_MS).await?;
    let mut sessions = 0;
    let keep = s
        .setting(keys::PERSISTENCE_RETENTION)
        .await?
        .unwrap_or_else(|| "400d".into());
    if let Some(d) = retention_days(&keep) {
        let cut = now - d * DAY_MS;
        let ended = s
            .sessions(&kls_store::sessions::SessionQuery {
                limit: 5000,
                ..Default::default()
            })
            .await?;
        for x in ended
            .into_iter()
            .filter(|x| x.ended_at.is_some_and(|e| e < cut))
        {
            core.delete_session(&x.id, &format!("retention {keep}"))
                .await?;
            sessions += 1;
        }
    }
    s.set_setting(keys::LAST_RETENTION, &now.to_string())
        .await?;
    if events > 0 || sessions > 0 || lines > 0 {
        core.emit(
            kinds::RETENTION_PRUNED,
            "retention",
            &json!({"events": events, "sessions": sessions, "actionLines": lines, "eventDays": days, "keep": keep}),
        )
        .await?;
    }
    Ok((events, sessions))
}

/// How often [`start`] runs the delivery sweeps.
pub const DELIVERY_SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);

/// One delivery tick (origin `sweep`): the lifecycle sweep, then outcome
/// linking and the roll-up. Each checks the kill switch and the schedule
/// blackout first (a refused report, nothing written); every write is
/// audited with its origin. Errors are logged at warn.
pub async fn delivery_tick(
    core: &Core,
) -> (
    Option<crate::github::SweepReport>,
    Option<crate::github::SweepReport>,
) {
    let life = match crate::github::lifecycle_sweep(core, "sweep").await {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!("delivery lifecycle sweep: {e:#}");
            None
        }
    };
    let roll = match crate::github::rollup(core, "sweep").await {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!("delivery roll-up: {e:#}");
            None
        }
    };
    (life, roll)
}

/// Starts the sweeps. Lives as long as the process.
pub fn start(core: &Core) -> tokio::task::JoinHandle<()> {
    let core = core.clone();
    tokio::spawn(async move {
        let mut bus = core.subscribe();
        let mut daily = tokio::time::interval(Duration::from_secs(3600));
        let mut delivery = tokio::time::interval_at(
            tokio::time::Instant::now() + DELIVERY_SWEEP_EVERY,
            DELIVERY_SWEEP_EVERY,
        );
        delivery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Backfill the session fact index once at start.
        if let Err(e) = crate::audit::facts::fold_all(&core).await {
            tracing::debug!("session facts backfill: {e:#}");
        }
        loop {
            tokio::select! {
                ev = bus.recv() => match ev {
                    Ok(ev) => on_event(&core, &ev).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                },
                _ = delivery.tick() => {
                    delivery_tick(&core).await;
                }
                _ = daily.tick() => {
                    let now = kls_store::now_ms();
                    let last: i64 = core.store().setting(keys::LAST_RETENTION).await.ok().flatten()
                        .and_then(|v| v.parse().ok()).unwrap_or(0);
                    if now - last >= DAY_MS
                        && let Err(e) = retention_at(&core, now).await
                    {
                        tracing::warn!("retention: {e:#}");
                    }
                }
            }
        }
    })
}

async fn on_event(core: &Core, ev: &kls_store::Event) {
    // #33: budget changes go out to running work (belai::budget).
    crate::belai::budget::on_event(core, ev).await;
    if ev.kind == crate::kinds::NODE_OFFLINE {
        let (core, node, id) = (core.clone(), ev.subject.clone(), ev.id);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(LOST_GRACE_MS as u64)).await;
            if let Err(e) = mark_lost(&core, &node, id).await {
                tracing::warn!(node = %node, "lost sessions: {e:#}");
            }
        });
    }
    // The session fact index (audit::facts).
    if ev.kind == "session.deleted" {
        let _ = core.store().session_facts_delete(&ev.subject).await;
    } else if ev.kind.starts_with("session.")
        && let Err(e) = crate::audit::facts::fold_session(core, &ev.subject).await
    {
        tracing::debug!(session = %ev.subject, "session facts: {e:#}");
    }
    if ev.kind == crate::hosts::kinds::INVENTORY
        && let Err(e) = crate::audit::overrides::settle_acks(core, &ev.subject).await
    {
        tracing::debug!(node = %ev.subject, "project prefs acks: {e:#}");
    }
    if matches!(ev.kind.as_str(), "run.finished" | "session.ended")
        && let Err(e) = crate::estimates::record_for_event(core, ev).await
    {
        tracing::debug!("estimate for {}: {e:#}", ev.subject);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_values() {
        assert_eq!(retention_days("400d"), Some(400));
        assert_eq!(retention_days("30d"), Some(30));
        assert_eq!(retention_days("forever"), None);
    }

    #[tokio::test]
    async fn events_pruned_only_after_the_ledger_has_them() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        core.emit("vault.secret_saved", "s1", &json!({"provider": "x"}))
            .await
            .unwrap();
        core.emit("misc.thing", "a", &json!({})).await.unwrap();
        // Nothing is old yet.
        assert_eq!(retention_at(&core, kls_store::now_ms()).await.unwrap().0, 0);
        // Forty days later both go; the ledger kept the vault one.
        let later = kls_store::now_ms() + 40 * DAY_MS;
        let (pruned, _) = retention_at(&core, later).await.unwrap();
        assert!(pruned >= 2, "{pruned}");
        let ledger = core
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(ledger.iter().any(|e| e.msg.contains("vault.secret_saved")));
        assert!(
            core.store()
                .setting(keys::LAST_RETENTION)
                .await
                .unwrap()
                .is_some()
        );
        // events.retention_days is read.
        core.store()
            .set_setting(keys::EVENTS_RETENTION_DAYS, "90")
            .await
            .unwrap();
        core.emit("misc.thing", "b", &json!({})).await.unwrap();
        assert_eq!(retention_at(&core, later).await.unwrap().0, 0);
    }

    #[tokio::test]
    async fn delivery_tick_runs_both_sweeps_and_is_refused_under_the_kill_switch() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let (life, roll) = delivery_tick(&core).await;
        assert!(life.unwrap().refused.is_none());
        assert!(roll.unwrap().refused.is_none());
        crate::control::pause(&core, "drill").await.unwrap();
        let (life, roll) = delivery_tick(&core).await;
        assert!(life.unwrap().refused.is_some(), "lifecycle refused");
        assert!(roll.unwrap().refused.is_some(), "roll-up refused");
        // The refusals are on the record (the kill switch's audited event).
        let refused = core
            .store()
            .events_query(&kls_store::audit::EventQuery {
                prefixes: vec!["factory.refused".into()],
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(refused.len() >= 2, "{refused:?}");
        assert!(DELIVERY_SWEEP_EVERY == Duration::from_secs(300));
    }
    #[tokio::test]
    async fn lost_sessions_skip_a_node_that_came_back() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let off = core
            .emit(crate::kinds::NODE_OFFLINE, "n1", &json!({}))
            .await
            .unwrap();
        // No sessions: nothing to end.
        assert_eq!(mark_lost(&core, "n1", off.id).await.unwrap(), 0);
        core.emit(crate::kinds::NODE_ONLINE, "n1", &json!({}))
            .await
            .unwrap();
        assert!(back_online(&core, "n1", off.id).await.unwrap());
        assert!(!back_online(&core, "n2", off.id).await.unwrap());
    }
}
