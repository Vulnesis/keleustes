//! Re-capture of missed delivery facts (builtin-agents 2.6, roll-up sweep).
//!
//! A merged pull whose capture was missed (the poller was off, the read
//! failed, the factory was stopped when it merged) is counted by
//! `kls_core::github::rollup`. kls-core cannot reach the forge, so this
//! module registers [`Recapturer`] as its hook: each missed pull is read
//! once more and captured (`sync::refresh_pull`, which captures a merged
//! pull not yet captured). The kill switch and schedule blackout hold the
//! whole pass (`control::check`, audited `factory.refused`), the client
//! holds every call (caller `poller`, deferrable), and each pull's outcome
//! is audited (`forge` ledger, action `delivery.recapture`, origin noted).

use crate::forge::guard;
use crate::{Client, Service, sync};
use anyhow::Result;
use kls_core::Core;
use serde_json::json;
use std::sync::Arc;

/// The caller re-capture reads as: deferrable, held by the kill switch.
pub const CALLER: &str = crate::poller::CALLER;

/// Re-captures `pulls` over the client `client_for` picks per repository.
pub async fn recapture_with<F, Fut>(
    core: &Core,
    pulls: &[(String, i64)],
    origin: &str,
    client_for: F,
) -> Result<usize>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Client>>,
{
    if let Err(e) = kls_core::control::check(core, "schedule", "delivery.recapture", origin).await {
        tracing::debug!("delivery re-capture held: {e:#}");
        return Ok(0);
    }
    let mut n = 0;
    for (repo, number) in pulls {
        let target = format!("{repo}#{number}");
        let res = match client_for(repo.clone()).await {
            Ok(c) => sync::refresh_pull(&c, repo, *number, CALLER)
                .await
                .map(|p| (c.conn().id.clone(), p.delivery_captured)),
            Err(e) => Err(e),
        };
        let (conn, outcome) = match &res {
            Ok((conn, true)) => (conn.clone(), "captured".to_string()),
            Ok((conn, false)) => (conn.clone(), "not_merged".to_string()),
            Err(e) => (String::new(), format!("failed: {e:#}")),
        };
        if matches!(res, Ok((_, true))) {
            n += 1;
        }
        guard::audit(
            core,
            CALLER,
            "delivery.recapture",
            &target,
            &conn,
            if outcome.starts_with("failed") {
                "failed"
            } else {
                outcome.as_str()
            },
            json!({ "repo": repo, "number": number, "origin": origin, "outcome": outcome }),
        )
        .await;
        // A stop mid-pass ends it; the rest wait for the next pass.
        if let Err(e) = &res
            && format!("{e:#}").contains("stopped")
        {
            break;
        }
    }
    Ok(n)
}

/// The roll-up's re-capture hook over the process's forge plane.
pub struct Recapturer(pub Arc<Service>);

#[async_trait::async_trait]
impl kls_core::github::Recapture for Recapturer {
    async fn recapture(&self, core: &Core, pulls: &[(String, i64)], origin: &str) -> Result<usize> {
        let svc = self.0.clone();
        recapture_with(core, pulls, origin, |repo| {
            let svc = svc.clone();
            async move { svc.client_for(&repo).await }
        })
        .await
    }
}

/// Registers [`Recapturer`] for the service's core (at start).
pub fn start(svc: &Arc<Service>) {
    kls_core::github::register_recapture(svc.core(), Arc::new(Recapturer(svc.clone())));
}
