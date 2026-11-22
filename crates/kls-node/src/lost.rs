//! Lost sessions: a session whose node has had no link for longer than the
//! grace period (or whose node was revoked) is ended with reason `lost`, so
//! crew steps and runs waiting on it settle (`session.ended`).

use crate::server::Hub;
use anyhow::Result;
use kls_store::now_ms;
use kls_store::sessions::SessionQuery;
use std::time::Duration;

/// Setting: minutes a node may be away before its sessions are lost.
pub const GRACE_SETTING: &str = "node.lost_grace_minutes";
pub const DEFAULT_GRACE_MINUTES: i64 = 10;
/// The end reason a lost session gets.
/// The same reason the governance sweep (`kls_core::sweeps`) writes, so
/// consumers see one word; this sweep adds revoked nodes and nodes that
/// never came back after a server restart.
pub const LOST: &str = "lost";
pub const REVOKED: &str = "lost";

const SWEEP: Duration = Duration::from_secs(60);

pub fn start(hub: &Hub) {
    let hub = hub.clone();
    let started = now_ms();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(SWEEP);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            if let Err(e) = sweep(&hub, started, now_ms()).await {
                tracing::warn!("lost-session sweep: {e:#}");
            }
        }
    });
}

async fn grace_ms(hub: &Hub) -> i64 {
    hub.core()
        .store()
        .setting(GRACE_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|m| *m >= 0)
        .unwrap_or(DEFAULT_GRACE_MINUTES)
        * 60_000
}

/// Ends the open sessions of nodes that are revoked, or offline and unseen
/// for longer than the grace (counted from `started` at the earliest). Returns how many were ended.
pub async fn sweep(hub: &Hub, started: i64, now: i64) -> Result<usize> {
    let core = hub.core();
    let store = core.store();
    let online = hub.online().await;
    let grace = grace_ms(hub).await;
    let nodes = store.nodes().await?;
    let mut ended = 0;
    for s in store
        .sessions(&SessionQuery {
            open_only: true,
            limit: 10_000,
            ..Default::default()
        })
        .await?
    {
        let node = nodes.iter().find(|n| n.id == s.node_id);
        let (reason, why) = match node {
            Some(n) if n.revoked_at.is_some() => (REVOKED, "node revoked"),
            None => (REVOKED, "node revoked"),
            Some(_) if online.contains(&s.node_id) => continue,
            Some(n) => {
                // Nodes get the grace again after this server starts.
                let last = s.seen_at.max(n.last_seen_at.unwrap_or(0)).max(started);
                if now - last < grace {
                    continue;
                }
                (LOST, "node offline")
            }
        };
        core.session_end(&s.node_id, &s.id, now, reason).await?;
        core.emit(
            kls_core::sweeps::kinds::SESSION_LOST,
            &s.id,
            &serde_json::json!({ "nodeId": s.node_id, "sessionId": s.id, "runId": s.run_id,
                                 "cardId": s.card_id, "reason": why }),
        )
        .await?;
        ended += 1;
    }
    Ok(ended)
}
