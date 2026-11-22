//! Store: host management (docs/gaps/host-management.md §6.5). The last
//! inventory each belai host reported, the host actions Keleustes asked for
//! with their progress lines, and provision plans and their runs.

use super::*;

/// The inventory a host last reported (JSON of `kls_core::hosts::Inventory`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostInventoryRow {
    pub node_id: String,
    pub at: i64,
    pub json: String,
    /// sha256 of `json`, to tell a change from a repeat.
    pub sha256: String,
}

/// One host action: what was asked, by whom, and how it went.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostActionRow {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    /// "read", "config", "provision", "shell".
    pub class: String,
    pub params_json: String,
    pub dry_run: bool,
    /// The preview a real action was confirmed against, or the hash a dry
    /// run produced.
    pub preview_sha256: String,
    pub preview_json: String,
    pub if_match: String,
    pub origin_json: String,
    pub plan_run_id: String,
    pub step: i64,
    /// "requested", "refused", "queued", "accepted", "running",
    /// "needs_human", "done", "failed", "cancelled".
    pub state: String,
    pub requested_at: i64,
    pub delivered_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: String,
    pub result_json: String,
    pub handoff_json: String,
    /// Highest update seq applied.
    pub last_seq: i64,
    pub step_label: String,
    pub pct: i64,
}

impl HostActionRow {
    /// Whether the action has settled (no further update is expected).
    pub fn settled(&self) -> bool {
        matches!(
            self.state.as_str(),
            "refused" | "done" | "failed" | "cancelled"
        )
    }
}

/// One scrubbed progress line of an action.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostActionLine {
    pub action_id: String,
    pub seq: i64,
    pub at: i64,
    pub line: String,
}

/// A provision plan: named steps, versioned on every save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostPlanRow {
    pub id: String,
    pub name: String,
    pub steps_json: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One run of a plan on a node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostPlanRunRow {
    pub id: String,
    pub plan_id: String,
    pub plan_version: i64,
    pub node_id: String,
    pub origin_json: String,
    /// "running", "done", "failed", "cancelled", "refused".
    pub state: String,
    pub step: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub error: String,
}

/// The update a host filed, as the store applies it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostActionPatch {
    pub seq: i64,
    pub state: String,
    pub step_label: String,
    pub pct: i64,
    pub lines: Vec<String>,
    pub preview_json: String,
    pub preview_sha256: String,
    pub result_json: String,
    pub error: String,
    pub handoff_json: String,
}

/// Why the store refused an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatchRefusal {
    NotFound,
    /// The action is addressed to another node.
    NotAddressed,
    /// seq is not above the last applied one.
    Stale {
        last: i64,
    },
    /// The action already settled.
    Settled,
}

impl std::fmt::Display for PatchRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchRefusal::NotFound => write!(f, "unknown host action"),
            PatchRefusal::NotAddressed => write!(f, "host action is not addressed to this node"),
            PatchRefusal::Stale { last } => write!(f, "update seq must be above {last}"),
            PatchRefusal::Settled => write!(f, "host action already settled"),
        }
    }
}

impl std::error::Error for PatchRefusal {}

/// Most bytes of lines kept per action.
pub const MAX_LINE_BYTES_PER_ACTION: i64 = 64 * 1024;

const ACTION_COLS: &str = "id, node_id, kind, class, params_json, dry_run, preview_sha256, \
     preview_json, if_match, origin_json, plan_run_id, step, state, requested_at, delivered_at, \
     finished_at, error, result_json, handoff_json, last_seq, step_label, pct";

fn action_row(r: &AnyRow) -> Result<HostActionRow> {
    Ok(HostActionRow {
        id: r.try_get("id")?,
        node_id: r.try_get("node_id")?,
        kind: r.try_get("kind")?,
        class: r.try_get("class")?,
        params_json: r.try_get("params_json")?,
        dry_run: r.try_get::<i64, _>("dry_run")? != 0,
        preview_sha256: r.try_get("preview_sha256")?,
        preview_json: r.try_get("preview_json")?,
        if_match: r.try_get("if_match")?,
        origin_json: r.try_get("origin_json")?,
        plan_run_id: r.try_get("plan_run_id")?,
        step: r.try_get("step")?,
        state: r.try_get("state")?,
        requested_at: r.try_get("requested_at")?,
        delivered_at: r.try_get("delivered_at")?,
        finished_at: r.try_get("finished_at")?,
        error: r.try_get("error")?,
        result_json: r.try_get("result_json")?,
        handoff_json: r.try_get("handoff_json")?,
        last_seq: r.try_get("last_seq")?,
        step_label: r.try_get("step_label")?,
        pct: r.try_get("pct")?,
    })
}

fn plan_run_row(r: &AnyRow) -> Result<HostPlanRunRow> {
    Ok(HostPlanRunRow {
        id: r.try_get("id")?,
        plan_id: r.try_get("plan_id")?,
        plan_version: r.try_get("plan_version")?,
        node_id: r.try_get("node_id")?,
        origin_json: r.try_get("origin_json")?,
        state: r.try_get("state")?,
        step: r.try_get("step")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
        error: r.try_get("error")?,
    })
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
}

impl Store {
    // ---- inventory ------------------------------------------------------------

    /// Stores a host's inventory. Returns true when it differs from the last
    /// one (a repeat only moves `at`).
    pub async fn upsert_host_inventory(&self, node_id: &str, at: i64, json: &str) -> Result<bool> {
        let sha = sha256_hex(json);
        let before: Option<String> =
            sqlx::query_scalar(self.sql("SELECT sha256 FROM host_inventory WHERE node_id = ?"))
                .bind(node_id)
                .fetch_optional(&self.pool)
                .await?;
        sqlx::query(self.sql(
            "INSERT INTO host_inventory (node_id, at, json, sha256) VALUES (?, ?, ?, ?) \
             ON CONFLICT (node_id) DO UPDATE SET at = excluded.at, json = excluded.json, \
             sha256 = excluded.sha256",
        ))
        .bind(node_id)
        .bind(at)
        .bind(json)
        .bind(&sha)
        .execute(&self.pool)
        .await?;
        Ok(before.as_deref() != Some(sha.as_str()))
    }

    pub async fn host_inventory(&self, node_id: &str) -> Result<Option<HostInventoryRow>> {
        let r = sqlx::query(
            self.sql("SELECT node_id, at, json, sha256 FROM host_inventory WHERE node_id = ?"),
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(HostInventoryRow {
            node_id: r.try_get("node_id")?,
            at: r.try_get("at")?,
            json: r.try_get("json")?,
            sha256: r.try_get("sha256")?,
        }))
    }

    // ---- actions ----------------------------------------------------------------

    pub async fn insert_host_action(&self, a: &HostActionRow) -> Result<()> {
        sqlx::query(self.sql(&format!(
            "INSERT INTO host_actions ({ACTION_COLS}) VALUES \
             (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )))
        .bind(&a.id)
        .bind(&a.node_id)
        .bind(&a.kind)
        .bind(&a.class)
        .bind(&a.params_json)
        .bind(a.dry_run as i64)
        .bind(&a.preview_sha256)
        .bind(&a.preview_json)
        .bind(&a.if_match)
        .bind(&a.origin_json)
        .bind(&a.plan_run_id)
        .bind(a.step)
        .bind(&a.state)
        .bind(a.requested_at)
        .bind(a.delivered_at)
        .bind(a.finished_at)
        .bind(&a.error)
        .bind(&a.result_json)
        .bind(&a.handoff_json)
        .bind(a.last_seq)
        .bind(&a.step_label)
        .bind(a.pct)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn host_action(&self, id: &str) -> Result<Option<HostActionRow>> {
        let r = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.as_ref().map(action_row).transpose()
    }

    /// A node's actions, newest first.
    pub async fn host_actions(&self, node_id: &str, limit: i64) -> Result<Vec<HostActionRow>> {
        let rows = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE node_id = ? \
             ORDER BY requested_at DESC, id DESC LIMIT ?"
        )))
        .bind(node_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(action_row).collect()
    }

    /// Actions in a state, across nodes, oldest first.
    pub async fn host_actions_in_state(&self, state: &str) -> Result<Vec<HostActionRow>> {
        let rows = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE state = ? ORDER BY requested_at, id"
        )))
        .bind(state)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(action_row).collect()
    }

    /// Actions the node has not answered yet, oldest first: what
    /// `Host.Actions` sends (and re-sends after a reconnect).
    pub async fn host_actions_unanswered(&self, node_id: &str) -> Result<Vec<HostActionRow>> {
        let rows = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE node_id = ? AND state = 'queued' \
             ORDER BY requested_at, id"
        )))
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(action_row).collect()
    }

    /// A plan run's actions in step order.
    pub async fn host_actions_of_run(&self, plan_run_id: &str) -> Result<Vec<HostActionRow>> {
        let rows = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE plan_run_id = ? ORDER BY step, requested_at"
        )))
        .bind(plan_run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(action_row).collect()
    }

    /// Records that an action went out on the node's stream (first time only).
    pub async fn mark_host_action_delivered(&self, id: &str, at: i64) -> Result<()> {
        sqlx::query(
            self.sql(
                "UPDATE host_actions SET delivered_at = ? WHERE id = ? AND delivered_at IS NULL",
            ),
        )
        .bind(at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Settles an action Keleustes itself decided (refused before sending,
    /// cancelled, or a hand-off a person closed).
    pub async fn settle_host_action(&self, id: &str, state: &str, error: &str) -> Result<bool> {
        let n = sqlx::query(self.sql(
            "UPDATE host_actions SET state = ?, error = ?, finished_at = ? WHERE id = ? \
             AND state NOT IN ('refused', 'done', 'failed', 'cancelled')",
        ))
        .bind(state)
        .bind(error)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Applies a node's update: it must be addressed to `node_id`, its seq
    /// must be above the last one, and the action must not have settled.
    /// Lines past the per-action cap are dropped.
    pub async fn apply_host_action_update(
        &self,
        node_id: &str,
        action_id: &str,
        p: &HostActionPatch,
    ) -> Result<std::result::Result<HostActionRow, PatchRefusal>> {
        let mut tx = self.begin_write().await?;
        let r = sqlx::query(self.sql(&format!(
            "SELECT {ACTION_COLS} FROM host_actions WHERE id = ?"
        )))
        .bind(action_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(cur) = r.as_ref().map(action_row).transpose()? else {
            return Ok(Err(PatchRefusal::NotFound));
        };
        if cur.node_id != node_id {
            return Ok(Err(PatchRefusal::NotAddressed));
        }
        if p.seq <= cur.last_seq {
            return Ok(Err(PatchRefusal::Stale { last: cur.last_seq }));
        }
        if cur.settled() {
            return Ok(Err(PatchRefusal::Settled));
        }
        let now = now_ms();
        let settles = matches!(
            p.state.as_str(),
            "refused" | "done" | "failed" | "cancelled"
        );
        let keep = |new: &str, old: &str| {
            if new.is_empty() {
                old.to_string()
            } else {
                new.to_string()
            }
        };
        let next = HostActionRow {
            state: keep(&p.state, &cur.state),
            step_label: keep(&p.step_label, &cur.step_label),
            pct: if p.pct > 0 { p.pct.min(100) } else { cur.pct },
            preview_json: keep(&p.preview_json, &cur.preview_json),
            preview_sha256: if cur.dry_run {
                keep(&p.preview_sha256, &cur.preview_sha256)
            } else {
                cur.preview_sha256.clone()
            },
            result_json: keep(&p.result_json, &cur.result_json),
            error: keep(&p.error, &cur.error),
            handoff_json: keep(&p.handoff_json, &cur.handoff_json),
            last_seq: p.seq,
            finished_at: if settles { Some(now) } else { cur.finished_at },
            ..cur.clone()
        };
        sqlx::query(self.sql(
            "UPDATE host_actions SET state = ?, step_label = ?, pct = ?, preview_json = ?, \
             preview_sha256 = ?, result_json = ?, error = ?, handoff_json = ?, last_seq = ?, \
             finished_at = ? WHERE id = ?",
        ))
        .bind(&next.state)
        .bind(&next.step_label)
        .bind(next.pct)
        .bind(&next.preview_json)
        .bind(&next.preview_sha256)
        .bind(&next.result_json)
        .bind(&next.error)
        .bind(&next.handoff_json)
        .bind(next.last_seq)
        .bind(next.finished_at)
        .bind(action_id)
        .execute(&mut *tx)
        .await?;
        if !p.lines.is_empty() {
            let used: i64 = sqlx::query_scalar(self.sql(
                "SELECT COALESCE(SUM(LENGTH(line)), 0) FROM host_action_lines WHERE action_id = ?",
            ))
            .bind(action_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
            let mut budget = MAX_LINE_BYTES_PER_ACTION - used;
            let mut n: i64 = sqlx::query_scalar(
                self.sql("SELECT COALESCE(MAX(seq), 0) FROM host_action_lines WHERE action_id = ?"),
            )
            .bind(action_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
            for line in &p.lines {
                let len = line.len() as i64;
                if len > budget {
                    break;
                }
                budget -= len;
                n += 1;
                sqlx::query(self.sql(
                    "INSERT INTO host_action_lines (action_id, seq, at, line) VALUES (?, ?, ?, ?)",
                ))
                .bind(action_id)
                .bind(n)
                .bind(now)
                .bind(line)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(Ok(next))
    }

    pub async fn host_action_lines(&self, action_id: &str) -> Result<Vec<HostActionLine>> {
        let rows = sqlx::query(self.sql(
            "SELECT action_id, seq, at, line FROM host_action_lines WHERE action_id = ? ORDER BY seq",
        ))
        .bind(action_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(HostActionLine {
                    action_id: r.try_get("action_id")?,
                    seq: r.try_get("seq")?,
                    at: r.try_get("at")?,
                    line: r.try_get("line")?,
                })
            })
            .collect()
    }

    /// Deletes action lines older than `before` (ms). Rows stay forever as
    /// the audit index.
    pub async fn prune_host_action_lines(&self, before: i64) -> Result<u64> {
        Ok(
            sqlx::query(self.sql("DELETE FROM host_action_lines WHERE at < ?"))
                .bind(before)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    // ---- plans ------------------------------------------------------------------

    /// Creates or updates a plan; every save bumps its version.
    pub async fn save_host_plan(
        &self,
        id: &str,
        name: &str,
        steps_json: &str,
    ) -> Result<HostPlanRow> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO host_plans (id, name, steps_json, version, created_at, updated_at) \
             VALUES (?, ?, ?, 1, ?, ?) ON CONFLICT (id) DO UPDATE SET name = excluded.name, \
             steps_json = excluded.steps_json, version = host_plans.version + 1, \
             updated_at = excluded.updated_at",
        ))
        .bind(id)
        .bind(name)
        .bind(steps_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.host_plan(id)
            .await?
            .context("plan vanished after save")
    }

    pub async fn host_plan(&self, id: &str) -> Result<Option<HostPlanRow>> {
        let r = sqlx::query(self.sql(
            "SELECT id, name, steps_json, version, created_at, updated_at FROM host_plans WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(HostPlanRow {
            id: r.try_get("id")?,
            name: r.try_get("name")?,
            steps_json: r.try_get("steps_json")?,
            version: r.try_get("version")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
        }))
    }

    pub async fn host_plans(&self) -> Result<Vec<HostPlanRow>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, name, steps_json, version, created_at, updated_at FROM host_plans ORDER BY name",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(HostPlanRow {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    steps_json: r.try_get("steps_json")?,
                    version: r.try_get("version")?,
                    created_at: r.try_get("created_at")?,
                    updated_at: r.try_get("updated_at")?,
                })
            })
            .collect()
    }

    pub async fn delete_host_plan(&self, id: &str) -> Result<bool> {
        Ok(sqlx::query(self.sql("DELETE FROM host_plans WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn insert_host_plan_run(&self, r: &HostPlanRunRow) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO host_plan_runs (id, plan_id, plan_version, node_id, origin_json, state, \
             step, started_at, finished_at, error) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&r.id)
        .bind(&r.plan_id)
        .bind(r.plan_version)
        .bind(&r.node_id)
        .bind(&r.origin_json)
        .bind(&r.state)
        .bind(r.step)
        .bind(r.started_at)
        .bind(r.finished_at)
        .bind(&r.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves a run to `step`, or settles it when `state` is not "running".
    pub async fn update_host_plan_run(
        &self,
        id: &str,
        state: &str,
        step: i64,
        error: &str,
    ) -> Result<()> {
        let fin = (state != "running").then(now_ms);
        sqlx::query(self.sql(
            "UPDATE host_plan_runs SET state = ?, step = ?, error = ?, \
             finished_at = COALESCE(?, finished_at) WHERE id = ?",
        ))
        .bind(state)
        .bind(step)
        .bind(error)
        .bind(fin)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn host_plan_run(&self, id: &str) -> Result<Option<HostPlanRunRow>> {
        let r = sqlx::query(self.sql(
            "SELECT id, plan_id, plan_version, node_id, origin_json, state, step, started_at, \
             finished_at, error FROM host_plan_runs WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.as_ref().map(plan_run_row).transpose()
    }

    /// A node's plan runs, newest first.
    pub async fn host_plan_runs(&self, node_id: &str, limit: i64) -> Result<Vec<HostPlanRunRow>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, plan_id, plan_version, node_id, origin_json, state, step, started_at, \
             finished_at, error FROM host_plan_runs WHERE node_id = ? \
             ORDER BY started_at DESC, id DESC LIMIT ?",
        ))
        .bind(node_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(plan_run_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let mut out = vec![(
            Store::open_sqlite(&dir.path().join("t.db")).await.unwrap(),
            Some(dir),
        )];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            out.push((Store::open(&url).await.unwrap(), None));
        }
        out
    }

    fn action(id: &str, node: &str) -> HostActionRow {
        HostActionRow {
            id: id.into(),
            node_id: node.into(),
            kind: "product_probe".into(),
            class: "read".into(),
            params_json: r#"{"product":"cli"}"#.into(),
            origin_json: r#"{"actor":"owner","surface":"fleet.host/products"}"#.into(),
            state: "queued".into(),
            requested_at: now_ms(),
            ..Default::default()
        }
    }

    fn uid() -> String {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("t-{}-{}-{n}", std::process::id(), now_ms())
    }

    #[tokio::test]
    async fn inventory_upsert_reports_change_only() {
        for (s, _d) in stores().await {
            let n = uid();
            assert!(s.upsert_host_inventory(&n, 1, r#"{"a":1}"#).await.unwrap());
            assert!(!s.upsert_host_inventory(&n, 2, r#"{"a":1}"#).await.unwrap());
            assert!(s.upsert_host_inventory(&n, 3, r#"{"a":2}"#).await.unwrap());
            let r = s.host_inventory(&n).await.unwrap().unwrap();
            assert_eq!((r.at, r.json.as_str()), (3, r#"{"a":2}"#));
        }
    }

    #[tokio::test]
    async fn action_lifecycle_monotonic_and_addressed() {
        for (s, _d) in stores().await {
            let (id, node) = (uid(), uid());
            s.insert_host_action(&action(&id, &node)).await.unwrap();
            assert_eq!(s.host_actions_unanswered(&node).await.unwrap().len(), 1);
            s.mark_host_action_delivered(&id, 5).await.unwrap();
            s.mark_host_action_delivered(&id, 9).await.unwrap();
            assert_eq!(
                s.host_action(&id).await.unwrap().unwrap().delivered_at,
                Some(5)
            );

            let p = HostActionPatch {
                seq: 1,
                state: "accepted".into(),
                lines: vec!["probing".into()],
                ..Default::default()
            };
            let other = s.apply_host_action_update(&uid(), &id, &p).await.unwrap();
            assert_eq!(other, Err(PatchRefusal::NotAddressed));
            let ok = s.apply_host_action_update(&node, &id, &p).await.unwrap();
            assert_eq!(ok.unwrap().state, "accepted");
            assert!(s.host_actions_unanswered(&node).await.unwrap().is_empty());
            let again = s.apply_host_action_update(&node, &id, &p).await.unwrap();
            assert_eq!(again, Err(PatchRefusal::Stale { last: 1 }));

            let done = HostActionPatch {
                seq: 2,
                state: "done".into(),
                result_json: r#"{"ok":true}"#.into(),
                lines: vec!["ok".into()],
                ..Default::default()
            };
            let r = s
                .apply_host_action_update(&node, &id, &done)
                .await
                .unwrap()
                .unwrap();
            assert!(r.settled() && r.finished_at.is_some());
            let late = HostActionPatch {
                seq: 3,
                state: "failed".into(),
                ..Default::default()
            };
            assert_eq!(
                s.apply_host_action_update(&node, &id, &late).await.unwrap(),
                Err(PatchRefusal::Settled)
            );
            let lines = s.host_action_lines(&id).await.unwrap();
            assert_eq!(
                lines.iter().map(|l| l.line.as_str()).collect::<Vec<_>>(),
                ["probing", "ok"]
            );
            assert_eq!(s.host_actions(&node, 10).await.unwrap().len(), 1);
            assert_eq!(s.prune_host_action_lines(now_ms() + 1).await.unwrap(), 2);
        }
    }

    #[tokio::test]
    async fn lines_capped_per_action() {
        for (s, _d) in stores().await {
            let (id, node) = (uid(), uid());
            s.insert_host_action(&action(&id, &node)).await.unwrap();
            let big = "x".repeat(40 * 1024);
            let p = HostActionPatch {
                seq: 1,
                state: "running".into(),
                lines: vec![big.clone(), big],
                ..Default::default()
            };
            s.apply_host_action_update(&node, &id, &p)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(s.host_action_lines(&id).await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn dry_run_keeps_its_preview_hash_real_run_keeps_confirmed() {
        for (s, _d) in stores().await {
            let node = uid();
            let dry = HostActionRow {
                dry_run: true,
                ..action(&uid(), &node)
            };
            s.insert_host_action(&dry).await.unwrap();
            let p = HostActionPatch {
                seq: 1,
                state: "done".into(),
                preview_sha256: "abc".into(),
                preview_json: "{}".into(),
                ..Default::default()
            };
            let r = s
                .apply_host_action_update(&node, &dry.id, &p)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(r.preview_sha256, "abc");
            let real = HostActionRow {
                preview_sha256: "abc".into(),
                ..action(&uid(), &node)
            };
            s.insert_host_action(&real).await.unwrap();
            let p2 = HostActionPatch {
                preview_sha256: "zzz".into(),
                ..p
            };
            let r = s
                .apply_host_action_update(&node, &real.id, &p2)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(r.preview_sha256, "abc");
            assert!(
                s.settle_host_action(&real.id, "cancelled", "")
                    .await
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn plans_version_and_runs() {
        for (s, _d) in stores().await {
            let id = uid();
            let p = s.save_host_plan(&id, "Baseline", "[]").await.unwrap();
            assert_eq!(p.version, 1);
            let p = s.save_host_plan(&id, "Baseline", "[{}]").await.unwrap();
            assert_eq!(p.version, 2);
            assert!(s.host_plans().await.unwrap().iter().any(|x| x.id == id));
            let node = uid();
            let run = HostPlanRunRow {
                id: uid(),
                plan_id: id.clone(),
                plan_version: 2,
                node_id: node.clone(),
                origin_json: "{}".into(),
                state: "running".into(),
                started_at: now_ms(),
                ..Default::default()
            };
            s.insert_host_plan_run(&run).await.unwrap();
            s.update_host_plan_run(&run.id, "done", 1, "")
                .await
                .unwrap();
            let r = s.host_plan_run(&run.id).await.unwrap().unwrap();
            assert_eq!((r.state.as_str(), r.step), ("done", 1));
            assert!(r.finished_at.is_some());
            assert_eq!(s.host_plan_runs(&node, 5).await.unwrap().len(), 1);
            assert!(s.delete_host_plan(&id).await.unwrap());
        }
    }
}
