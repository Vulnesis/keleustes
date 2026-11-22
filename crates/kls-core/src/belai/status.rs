//! Host status (WP-B): a node's live workers, crew runs and spend, as its
//! `belai rc` daemon reports them. The latest report per node is kept; each
//! worker's spend is charged to Budgets once (cumulative rows, raised only).

use crate::Core;
use anyhow::Result;
use kls_store::vault::UsageRow;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod kinds {
    /// A node reported its status. Subject: node id.
    pub const NODE_STATUS: &str = "node.status";
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    pub id: String,
    pub profile: String,
    pub crew: String,
    pub run: String,
    pub state: String,
    pub card_id: String,
    pub cwd: String,
    pub session_id: String,
    pub started_at: i64,
    pub beat_at: i64,
    pub reason: String,
    pub done: u32,
    pub failed: u32,
    pub branch: String,
    pub tokens: i64,
    pub cost_micro_usd: i64,
    pub cost_live_micro_usd: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewRun {
    pub id: String,
    pub crew: String,
    pub state: String,
    pub reason: String,
    pub scope_dirs: Vec<String>,
    pub members_json: String,
    pub max_workers: u32,
    pub stay: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub at: i64,
    pub running_sessions: u32,
    pub workers: Vec<Worker>,
    pub crew_runs: Vec<CrewRun>,
    /// Provider plan windows the host reported (row 48).
    #[serde(default)]
    pub plan_limits: Vec<crate::belai::plan::PlanLimit>,
}

/// Most workers or runs one report keeps.
const MAX_ROWS: usize = 500;

fn cap(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

impl Core {
    /// Stores a node's report and charges each worker's new spend.
    pub async fn node_status_apply(&self, node_id: &str, mut st: NodeStatus) -> Result<()> {
        st.workers.truncate(MAX_ROWS);
        st.crew_runs.truncate(MAX_ROWS);
        for w in &mut st.workers {
            for f in [
                &mut w.id,
                &mut w.profile,
                &mut w.crew,
                &mut w.run,
                &mut w.state,
                &mut w.card_id,
                &mut w.session_id,
                &mut w.branch,
            ] {
                *f = cap(f, 128);
            }
            w.cwd = cap(&w.cwd, 512);
            w.reason = cap(&w.reason, 300);
        }
        for r in &mut st.crew_runs {
            r.reason = cap(&r.reason, 300);
            if r.members_json.len() > 16 * 1024 {
                r.members_json.clear();
            }
        }
        let store = self.store();
        let prior_plan = self
            .node_statuses()
            .await?
            .into_iter()
            .find(|(n, _, _)| n == node_id)
            .map(|(_, _, s)| s.plan_limits)
            .unwrap_or_default();
        st.plan_limits =
            super::plan::clean(std::mem::take(&mut st.plan_limits), "", kls_store::now_ms());
        store
            .put_node_status(node_id, st.at, &serde_json::to_string(&st)?)
            .await?;
        self.plan_audit(node_id, &prior_plan, &st.plan_limits)
            .await?;
        for w in st.workers.iter().filter(|w| !w.id.is_empty()) {
            let (tokens, micro) = store
                .raise_worker_spend(node_id, &w.id, w.tokens, w.cost_micro_usd)
                .await?;
            if tokens == 0 && micro == 0 {
                continue;
            }
            let id = format!("worker:{node_id}:{}", w.id);
            let prev = store
                .usage_rows(0, i64::MAX)
                .await?
                .into_iter()
                .find(|u| u.id == id);
            let cost = micro as f64 / 1e6;
            if prev
                .as_ref()
                .is_some_and(|u| u.input_tokens == tokens && u.cost_usd == cost)
            {
                continue;
            }
            store
                .upsert_usage(&UsageRow {
                    id: id.clone(),
                    at: kls_store::now_ms(),
                    source: "worker".into(),
                    session_id: w.session_id.clone(),
                    node_id: node_id.into(),
                    agent: w.profile.clone(),
                    input_tokens: tokens,
                    cost_usd: cost,
                    ..Default::default()
                })
                .await?;
            self.emit(crate::vault::kinds::BUDGET_USAGE, &id, &json!({ "id": id }))
                .await?;
        }
        self.emit(
            kinds::NODE_STATUS,
            node_id,
            &json!({ "workers": st.workers.len(), "crewRuns": st.crew_runs.len(),
                     "runningSessions": st.running_sessions }),
        )
        .await?;
        Ok(())
    }

    /// Every node's latest status with the server time it arrived.
    pub async fn node_statuses(&self) -> Result<Vec<(String, i64, NodeStatus)>> {
        Ok(self
            .store()
            .node_statuses()
            .await?
            .into_iter()
            .filter_map(|(n, at, j)| serde_json::from_str(&j).ok().map(|s| (n, at, s)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spend_is_charged_once() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let st = |cost| NodeStatus {
            workers: vec![Worker {
                id: "w-1".into(),
                profile: "belai:patcher".into(),
                tokens: 1000,
                cost_micro_usd: cost,
                ..Default::default()
            }],
            ..Default::default()
        };
        c.node_status_apply("n1", st(500_000)).await.unwrap();
        c.node_status_apply("n1", st(500_000)).await.unwrap();
        // A stale (lower) report after a reconnect charges nothing back.
        c.node_status_apply("n1", st(100_000)).await.unwrap();
        let (tokens, usd) = c.store().usage_sum(&Default::default()).await.unwrap();
        assert_eq!(tokens, 1000);
        assert!((usd - 0.5).abs() < 1e-9);
        c.node_status_apply("n1", st(750_000)).await.unwrap();
        let (_, usd) = c.store().usage_sum(&Default::default()).await.unwrap();
        assert!((usd - 0.75).abs() < 1e-9);
        assert_eq!(c.node_statuses().await.unwrap()[0].2.workers.len(), 1);
    }
}
