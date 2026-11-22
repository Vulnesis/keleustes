//! Provider plan limits (belai-counterparts row 48): the 5h / 7d / rpm / tpm
//! windows a host reports in `HostStatus.plan_limits` and each session in
//! `SessionUpsert.plan_limits` (belai `intel_state`). Facts only, from the
//! host: each row keeps where it came from (node, session, source, time).
//! Routing prefers nodes whose window for the work's provider still has room
//! ([`route_filter`]); a window that goes over the threshold is audited once
//! per reset.

use crate::Core;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod kinds {
    /// A node's plan window went over the routing threshold. Subject: node id.
    pub const PLAN_LIMIT_NEAR: &str = "belai.plan_limit.near";
}

pub mod settings {
    /// Used fraction (0..1) at which routing skips a node (default 0.9).
    pub const SKIP_AT: &str = "routing.plan_limit_skip";
    /// Prefix of the per-node store of session-reported windows.
    pub const SESSION_PREFIX: &str = "belai.plan_limits.";
}

/// Default routing threshold.
pub const DEFAULT_SKIP_AT: f64 = 0.9;
/// Most windows kept per node.
const MAX_ROWS: usize = 32;
/// Windows a host may report.
pub const WINDOWS: [&str; 4] = ["5h", "7d", "rpm", "tpm"];

/// One provider plan window (the protocol's `PlanLimit`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanLimit {
    pub provider: String,
    /// "5h", "7d", "rpm", "tpm".
    pub window: String,
    /// 0..1 of the window used.
    pub used_fraction: f64,
    #[serde(default)]
    pub reset_at: i64,
    #[serde(default)]
    pub pace: f64,
    #[serde(default)]
    pub runway_ms: i64,
    /// "exact" or "estimate".
    #[serde(default)]
    pub source: String,
    /// Provenance: the session that reported it ("" for the host status).
    #[serde(default)]
    pub session_id: String,
    /// Provenance: when Keleustes took it.
    #[serde(default)]
    pub at: i64,
}

impl PlanLimit {
    fn key(&self) -> (String, String) {
        (self.provider.clone(), self.window.clone())
    }

    /// Whether the window still counts at `now` (no reset time, or not yet reset).
    pub fn open_at(&self, now: i64) -> bool {
        self.reset_at <= 0 || self.reset_at > now
    }
}

fn ident(s: &str, n: usize) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        .take(n)
        .collect()
}

/// Cleans host-reported rows: known windows, a provider word, fractions in
/// 0..1, at most [`MAX_ROWS`], newest per provider and window.
pub fn clean(rows: Vec<PlanLimit>, session_id: &str, now: i64) -> Vec<PlanLimit> {
    let mut out: Vec<PlanLimit> = Vec::new();
    for mut r in rows {
        r.provider = ident(&r.provider.to_ascii_lowercase(), 64);
        if r.provider.is_empty() || !WINDOWS.contains(&r.window.as_str()) {
            continue;
        }
        if !r.used_fraction.is_finite() {
            continue;
        }
        r.used_fraction = r.used_fraction.clamp(0.0, 1.0);
        if !r.pace.is_finite() || r.pace < 0.0 {
            r.pace = 0.0;
        }
        r.runway_ms = r.runway_ms.max(0);
        r.source = match r.source.as_str() {
            "exact" => "exact".into(),
            _ => "estimate".into(),
        };
        r.session_id = ident(session_id, 64);
        r.at = now;
        out.retain(|o| o.key() != r.key());
        out.push(r);
        if out.len() >= MAX_ROWS {
            break;
        }
    }
    out
}

/// Merges `newer` over `older`, newest per provider and window.
fn merge(mut older: Vec<PlanLimit>, newer: &[PlanLimit]) -> Vec<PlanLimit> {
    for n in newer {
        older.retain(|o| o.key() != n.key());
        older.push(n.clone());
    }
    older.sort_by_key(|o| std::cmp::Reverse(o.at));
    older.truncate(MAX_ROWS);
    older
}

/// The window that makes `limits` nearly used for `provider` ("" = any
/// provider), if one does.
pub fn nearly_used<'a>(
    limits: &'a [PlanLimit],
    provider: &str,
    threshold: f64,
    now: i64,
) -> Option<&'a PlanLimit> {
    let provider = provider.to_ascii_lowercase();
    limits
        .iter()
        .filter(|l| provider.is_empty() || l.provider == provider)
        .filter(|l| l.open_at(now) && l.used_fraction >= threshold)
        .max_by(|a, b| a.used_fraction.total_cmp(&b.used_fraction))
}

/// The routing threshold from settings.
pub async fn skip_at(core: &Core) -> f64 {
    core.store()
        .setting(settings::SKIP_AT)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
        .unwrap_or(DEFAULT_SKIP_AT)
}

/// `online` without the nodes whose plan window for `provider` is nearly
/// used. When every node is, the list is kept as it is: the work waits on
/// the provider rather than on Keleustes.
pub async fn route_filter(core: &Core, online: Vec<String>, provider: &str) -> Vec<String> {
    let threshold = skip_at(core).await;
    let now = kls_store::now_ms();
    let mut keep = Vec::new();
    for id in &online {
        let limits = core.node_plan_limits(id).await.unwrap_or_default();
        if nearly_used(&limits, provider, threshold, now).is_none() {
            keep.push(id.clone());
        }
    }
    if keep.is_empty() { online } else { keep }
}

impl Core {
    /// Every window a node reported (host status and its sessions), newest
    /// per provider and window, without the ones that already reset.
    pub async fn node_plan_limits(&self, node_id: &str) -> Result<Vec<PlanLimit>> {
        let now = kls_store::now_ms();
        let host = self
            .store()
            .node_statuses()
            .await?
            .into_iter()
            .find(|(n, _, _)| n == node_id)
            .and_then(|(_, _, j)| serde_json::from_str::<super::status::NodeStatus>(&j).ok())
            .map(|s| s.plan_limits)
            .unwrap_or_default();
        let sessions = self.session_plan_rows(node_id).await;
        let mut all = merge(sessions, &host);
        all.retain(|l| l.open_at(now));
        all.sort_by(|a, b| (&a.provider, &a.window).cmp(&(&b.provider, &b.window)));
        Ok(all)
    }

    async fn session_plan_rows(&self, node_id: &str) -> Vec<PlanLimit> {
        self.store()
            .setting(&format!("{}{node_id}", settings::SESSION_PREFIX))
            .await
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// Stores the windows one session reported (`SessionUpsert.plan_limits`).
    pub async fn session_plan_limits_apply(
        &self,
        node_id: &str,
        session_id: &str,
        rows: Vec<PlanLimit>,
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = kls_store::now_ms();
        let rows = clean(rows, session_id, now);
        let prior = self.session_plan_rows(node_id).await;
        let merged = merge(prior.clone(), &rows);
        self.store()
            .set_setting(
                &format!("{}{node_id}", settings::SESSION_PREFIX),
                &serde_json::to_string(&merged)?,
            )
            .await?;
        self.plan_audit(node_id, &prior, &rows).await
    }

    /// Audits each window that went over the routing threshold, once per
    /// reset: `prior` are the rows before this report.
    pub(crate) async fn plan_audit(
        &self,
        node_id: &str,
        prior: &[PlanLimit],
        rows: &[PlanLimit],
    ) -> Result<()> {
        let threshold = skip_at(self).await;
        let now = kls_store::now_ms();
        for r in rows {
            if r.used_fraction < threshold || !r.open_at(now) {
                continue;
            }
            let was = prior.iter().any(|p| {
                p.key() == r.key() && p.used_fraction >= threshold && p.reset_at == r.reset_at
            });
            if was {
                continue;
            }
            let data = json!({
                "nodeId": node_id, "provider": r.provider, "window": r.window,
                "usedFraction": r.used_fraction, "resetAt": r.reset_at, "source": r.source,
                "sessionId": r.session_id, "threshold": threshold,
            });
            self.emit(kinds::PLAN_LIMIT_NEAR, node_id, &data).await?;
            self.audit_record(kls_store::audit::LedgerDraft {
                kind: "plan_limit".into(),
                actor: node_id.into(),
                actor_kind: "host".into(),
                msg: format!(
                    "plan window {} {} at {:.0}%: routing skips this node",
                    r.provider,
                    r.window,
                    r.used_fraction * 100.0
                ),
                subject: json!({ "node": node_id, "provider": r.provider, "window": r.window }),
                payload: data,
                origin: format!("host:{node_id}"),
                node_id: node_id.into(),
                session_id: r.session_id.clone(),
                ref_key: Some(format!(
                    "plan:{node_id}:{}:{}:{}",
                    r.provider, r.window, r.reset_at
                )),
                ..Default::default()
            })
            .await?;
        }
        Ok(())
    }

    /// Every node's windows, for the Host and Fleet screens.
    pub async fn plan_limits_all(&self) -> Result<Vec<(String, Vec<PlanLimit>)>> {
        let mut out = Vec::new();
        for n in self.store().nodes().await? {
            let l = self.node_plan_limits(&n.id).await?;
            if !l.is_empty() {
                out.push((n.id, l));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(p: &str, w: &str, f: f64) -> PlanLimit {
        PlanLimit {
            provider: p.into(),
            window: w.into(),
            used_fraction: f,
            // A window resets at a fixed time (hosts repeat it).
            reset_at: 4_102_444_800_000,
            source: "exact".into(),
            ..Default::default()
        }
    }

    #[test]
    fn clean_keeps_known_windows_and_clamps() {
        let rows = clean(
            vec![
                row("Anthropic", "5h", 1.7),
                row("openai", "week", 0.2),
                row("", "5h", 0.2),
                row("openai", "7d", f64::NAN),
                row("anthropic", "5h", 0.4),
            ],
            "s-1",
            7,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "anthropic");
        assert_eq!(rows[0].used_fraction, 0.4);
        assert_eq!(rows[0].session_id, "s-1");
        assert_eq!(rows[0].at, 7);
    }

    #[test]
    fn nearly_used_per_provider() {
        let now = kls_store::now_ms();
        let mut reset = row("openai", "5h", 0.95);
        reset.reset_at = now - 1;
        let l = vec![
            row("anthropic", "5h", 0.95),
            reset,
            row("openai", "7d", 0.5),
        ];
        assert!(nearly_used(&l, "anthropic", 0.9, now).is_some());
        assert!(nearly_used(&l, "openai", 0.9, now).is_none());
        assert!(nearly_used(&l, "", 0.9, now).is_some());
    }

    #[tokio::test]
    async fn routing_skips_a_nearly_used_node_and_audits_once() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        c.node_status_apply(
            "n1",
            super::super::status::NodeStatus {
                plan_limits: vec![row("anthropic", "5h", 0.97)],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        c.session_plan_limits_apply("n2", "s-9", vec![row("anthropic", "7d", 0.3)])
            .await
            .unwrap();
        let online = vec!["n1".to_string(), "n2".to_string()];
        assert_eq!(
            route_filter(&c, online.clone(), "anthropic").await,
            vec!["n2"]
        );
        assert_eq!(route_filter(&c, online.clone(), "openai").await, online);
        // Every node nearly used: the list stays (work waits on the provider).
        c.session_plan_limits_apply("n2", "s-9", vec![row("anthropic", "7d", 0.99)])
            .await
            .unwrap();
        c.session_plan_limits_apply("n2", "s-9", vec![row("anthropic", "7d", 0.995)])
            .await
            .unwrap();
        assert_eq!(route_filter(&c, online.clone(), "anthropic").await, online);
        let n2 = c.node_plan_limits("n2").await.unwrap();
        assert_eq!(n2[0].session_id, "s-9");
        let entries = c
            .store()
            .ledger(&kls_store::audit::LedgerQuery {
                kinds: vec!["plan_limit".into()],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .len();
        // n1 (host status) and n2 (one crossing; the second report repeats it).
        assert_eq!(entries, 2);
        assert_eq!(
            c.plan_limits_all().await.unwrap().len(),
            0,
            "no node rows yet"
        );
    }
}
