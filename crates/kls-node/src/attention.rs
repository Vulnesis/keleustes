//! Attention requests on the wire (WP-J): filing (Board.Attention or a
//! NodeFrame) and sending the owner's decision back as a replayable
//! `AttentionDecided` frame. Open requests lapse after a day.

use crate::server::Hub;
use kls_core::belai::attention::kinds;
use kls_proto::v1::{self as pb, server_frame};
use tokio::sync::broadcast::error::RecvError;

/// Files a request; refusals are answered, never errors.
pub async fn file(hub: &Hub, node_id: &str, a: pb::AttentionRequest) -> pb::AttentionReceipt {
    match hub
        .core()
        .attention_file(node_id, &a.id, &a.kind, &a.reason, &a.subject)
        .await
    {
        Ok(()) => pb::AttentionReceipt {
            id: a.id,
            state: "open".into(),
            reason: String::new(),
        },
        Err(e) => pb::AttentionReceipt {
            id: a.id,
            state: "refused".into(),
            reason: e.to_string(),
        },
    }
}

/// Sends each decision to its node (queued on the outbox, so a node that
/// is offline gets it on reconnect) and lapses old requests hourly.
pub fn start(hub: &Hub) {
    let hub = hub.clone();
    // Subscribed before the task runs, so no decision made meanwhile is missed.
    let mut bus = hub.core().subscribe();
    tokio::spawn(async move {
        let mut lapse = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = lapse.tick() => {
                    if let Err(e) = hub.core().attention_lapse(kls_store::now_ms()).await {
                        tracing::warn!("attention lapse: {e:#}");
                    }
                }
                ev = bus.recv() => match ev {
                    Ok(ev) if ev.kind == kinds::ATTENTION_DECIDED => {
                        let d: serde_json::Value = serde_json::from_str(&ev.data_json).unwrap_or_default();
                        let s = |k: &str| d[k].as_str().unwrap_or_default().to_string();
                        let (node, id, state) = (s("nodeId"), s("id"), s("state"));
                        if node.is_empty() || id.is_empty() {
                            continue;
                        }
                        let frame = server_frame::Body::AttentionDecided(pb::AttentionDecided { id, state });
                        // SQLite answers a write racing another with "database is
                        // locked"; the decision must still reach the node.
                        let mut tries = 0;
                        while let Err(e) = hub.send(&node, frame.clone()).await {
                            tries += 1;
                            if tries >= 10 {
                                tracing::warn!("attention decision for {node}: {e:#}");
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(50 * tries)).await;
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                },
            }
        }
    });
}
