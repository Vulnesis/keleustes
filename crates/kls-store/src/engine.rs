//! Store: M3 engine: workflows, versions, schedules (trigger rows), runs,
//! steps, approvals and template dismissals. Migration 0004.
//!
//! Ids are made by the caller (uuid v7 text). Graphs, configs, events and
//! step details are JSON text; this module never looks inside them.

use super::*;

// ---- records ---------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub template_id: String,
    /// `draft` (never published), `published`, `archived`.
    pub status: String,
    pub enabled: bool,
    /// 0 when none.
    pub published_version: i64,
    /// 0 when none.
    pub draft_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    /// `user`, or `system` for a seeded default.
    pub origin: String,
    /// The defaults catalogue key (`sys:<key>:<repo>`), empty for others.
    pub system_key: String,
    pub system_rev: i64,
    /// A person edited a seeded default: upgrades leave it alone.
    pub customised: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkflowVersion {
    pub workflow_id: String,
    pub version: i64,
    /// `draft`, `published` (the live one), `superseded`.
    pub status: String,
    pub graph_json: String,
    pub graph_sha256: String,
    pub note: String,
    pub est_tokens_min: i64,
    pub est_tokens_max: i64,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub published_at: Option<i64>,
}

/// A trigger row of a published workflow (cron, kanban or event).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub workflow_id: String,
    pub node_id: String,
    pub kind: String,
    pub cron: String,
    pub tz: String,
    pub config_json: String,
    pub enabled: bool,
    pub next_run_at: Option<i64>,
    pub last_fired_at: Option<i64>,
    pub last_run_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What publish writes for one trigger node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScheduleSpec {
    /// Used only when the row is new.
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub cron: String,
    pub tz: String,
    pub config_json: String,
    pub next_run_at: Option<i64>,
}

/// One trigger evaluation that passed its conditions (debounce, hourly cap, provenance).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerFire {
    pub schedule_id: String,
    pub subject: String,
    pub at: i64,
    pub event_id: Option<i64>,
    pub run_id: Option<String>,
    /// `started`, `duplicate` or `skipped:<reason>`.
    pub outcome: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub workflow_version: i64,
    pub schedule_id: Option<String>,
    pub trigger_kind: String,
    pub trigger_node_id: String,
    pub trigger_label: String,
    pub dedupe_key: Option<String>,
    pub repo: String,
    pub card_id: String,
    pub event_json: String,
    pub inputs_json: String,
    pub status: String,
    pub reason: String,
    pub dry_run: bool,
    pub tokens: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStep {
    pub id: String,
    pub run_id: String,
    /// Creation order within the run; set by [`Store::insert_step`].
    pub seq: i64,
    pub node_id: String,
    pub node_type: String,
    pub title: String,
    pub attempt: i64,
    pub status: String,
    pub route: String,
    pub error: String,
    pub details_json: String,
    pub dispatch_id: Option<String>,
    pub session_id: Option<String>,
    pub node_ref: Option<String>,
    pub wake_at: Option<i64>,
    pub tokens: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

/// A change to a step. `None` leaves the column alone; for the nullable
/// columns `Some(None)` clears it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepUpdate {
    pub status: Option<String>,
    pub route: Option<String>,
    pub error: Option<String>,
    pub details_json: Option<String>,
    pub dispatch_id: Option<Option<String>>,
    pub session_id: Option<Option<String>>,
    pub node_ref: Option<Option<String>>,
    pub wake_at: Option<Option<i64>>,
    pub add_tokens: i64,
    pub started: bool,
    pub finished: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub run_id: String,
    pub step_id: String,
    pub node_id: String,
    pub kind: String,
    pub state: String,
    pub title: String,
    pub approvers_json: String,
    pub context_json: String,
    pub note: String,
    pub decided_by: String,
    pub revision: i64,
    pub sla_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub decided_at: Option<i64>,
}

/// One System One decision with its rationale (WP2 migration 0200).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineDecision {
    pub id: String,
    pub at: i64,
    pub run_id: String,
    pub step_id: String,
    pub workflow_id: String,
    pub node_id: String,
    pub question: String,
    pub choice: String,
    pub outcome: String,
    /// `route`, `human`, `fail`.
    pub action: String,
    /// `chosen`, `below_threshold`, `abstained`, `daily_cap`.
    pub why: String,
    pub confidence: f64,
    pub threshold: f64,
    pub provider: String,
    pub model: String,
    /// The node that answered.
    pub node_ref: String,
    pub latency_ms: i64,
}

const DECISION_COLS: &str = "id, at, run_id, step_id, workflow_id, node_id, question, choice, outcome, \
     action, why, confidence, threshold, provider, model, node_ref, latency_ms";

fn decision_from_row(r: &AnyRow) -> Result<EngineDecision> {
    Ok(EngineDecision {
        id: r.try_get("id")?,
        at: r.try_get("at")?,
        run_id: r.try_get("run_id")?,
        step_id: r.try_get("step_id")?,
        workflow_id: r.try_get("workflow_id")?,
        node_id: r.try_get("node_id")?,
        question: r.try_get("question")?,
        choice: r.try_get("choice")?,
        outcome: r.try_get("outcome")?,
        action: r.try_get("action")?,
        why: r.try_get("why")?,
        confidence: r.try_get("confidence")?,
        threshold: r.try_get("threshold")?,
        provider: r.try_get("provider")?,
        model: r.try_get("model")?,
        node_ref: r.try_get("node_ref")?,
        latency_ms: r.try_get("latency_ms")?,
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunFilter {
    pub statuses: Vec<String>,
    pub workflow_id: Option<String>,
    pub trigger_kind: Option<String>,
    /// Substring of the run id, repo, card id or reason.
    pub q: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: i64,
}

/// Run statuses that still need the engine. `held`: a fire a schedule
/// blackout or maintenance window holds; it is queued when the window ends.
pub const ACTIVE_RUN_STATUSES: [&str; 5] =
    ["queued", "running", "waiting_approval", "paused", "held"];
/// Step statuses that keep a run open.
pub const OPEN_STEP_STATUSES: [&str; 3] = ["pending", "running", "waiting"];

const WF_COLS: &str = "id, slug, name, description, template_id, status, enabled, published_version, \
     draft_version, created_at, updated_at, archived_at, origin, system_key, system_rev, customised";
const VER_COLS: &str = "workflow_id, version, status, graph_json, graph_sha256, note, est_tokens_min, \
     est_tokens_max, revision, created_at, updated_at, published_at";
const SCHED_COLS: &str = "id, workflow_id, node_id, kind, cron, tz, config_json, enabled, next_run_at, \
     last_fired_at, last_run_id, created_at, updated_at";
const RUN_COLS: &str = "id, workflow_id, workflow_version, schedule_id, trigger_kind, trigger_node_id, \
     trigger_label, dedupe_key, repo, card_id, event_json, inputs_json, status, reason, dry_run, tokens, \
     created_at, updated_at, started_at, finished_at";
const STEP_COLS: &str = "id, run_id, seq, node_id, node_type, title, attempt, status, route, error, \
     details_json, dispatch_id, session_id, node_ref, wake_at, tokens, created_at, updated_at, \
     started_at, finished_at";
const APPROVAL_COLS: &str = "id, run_id, step_id, node_id, kind, state, title, approvers_json, \
     context_json, note, decided_by, revision, sla_at, created_at, updated_at, decided_at";

fn wf_from_row(r: &AnyRow) -> Result<Workflow> {
    Ok(Workflow {
        id: r.try_get("id")?,
        slug: r.try_get("slug")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        template_id: r.try_get("template_id")?,
        status: r.try_get("status")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        published_version: r.try_get("published_version")?,
        draft_version: r.try_get("draft_version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        archived_at: r.try_get("archived_at")?,
        origin: r.try_get("origin")?,
        system_key: r.try_get("system_key")?,
        system_rev: r.try_get("system_rev")?,
        customised: r.try_get::<i64, _>("customised")? != 0,
    })
}

fn ver_from_row(r: &AnyRow) -> Result<WorkflowVersion> {
    Ok(WorkflowVersion {
        workflow_id: r.try_get("workflow_id")?,
        version: r.try_get("version")?,
        status: r.try_get("status")?,
        graph_json: r.try_get("graph_json")?,
        graph_sha256: r.try_get("graph_sha256")?,
        note: r.try_get("note")?,
        est_tokens_min: r.try_get("est_tokens_min")?,
        est_tokens_max: r.try_get("est_tokens_max")?,
        revision: r.try_get("revision")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        published_at: r.try_get("published_at")?,
    })
}

fn sched_from_row(r: &AnyRow) -> Result<Schedule> {
    Ok(Schedule {
        id: r.try_get("id")?,
        workflow_id: r.try_get("workflow_id")?,
        node_id: r.try_get("node_id")?,
        kind: r.try_get("kind")?,
        cron: r.try_get("cron")?,
        tz: r.try_get("tz")?,
        config_json: r.try_get("config_json")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        next_run_at: r.try_get("next_run_at")?,
        last_fired_at: r.try_get("last_fired_at")?,
        last_run_id: r.try_get("last_run_id")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn run_from_row(r: &AnyRow) -> Result<Run> {
    Ok(Run {
        id: r.try_get("id")?,
        workflow_id: r.try_get("workflow_id")?,
        workflow_version: r.try_get("workflow_version")?,
        schedule_id: r.try_get("schedule_id")?,
        trigger_kind: r.try_get("trigger_kind")?,
        trigger_node_id: r.try_get("trigger_node_id")?,
        trigger_label: r.try_get("trigger_label")?,
        dedupe_key: r.try_get("dedupe_key")?,
        repo: r.try_get("repo")?,
        card_id: r.try_get("card_id")?,
        event_json: r.try_get("event_json")?,
        inputs_json: r.try_get("inputs_json")?,
        status: r.try_get("status")?,
        reason: r.try_get("reason")?,
        dry_run: r.try_get::<i64, _>("dry_run")? != 0,
        tokens: r.try_get("tokens")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
    })
}

fn step_from_row(r: &AnyRow) -> Result<RunStep> {
    Ok(RunStep {
        id: r.try_get("id")?,
        run_id: r.try_get("run_id")?,
        seq: r.try_get("seq")?,
        node_id: r.try_get("node_id")?,
        node_type: r.try_get("node_type")?,
        title: r.try_get("title")?,
        attempt: r.try_get("attempt")?,
        status: r.try_get("status")?,
        route: r.try_get("route")?,
        error: r.try_get("error")?,
        details_json: r.try_get("details_json")?,
        dispatch_id: r.try_get("dispatch_id")?,
        session_id: r.try_get("session_id")?,
        node_ref: r.try_get("node_ref")?,
        wake_at: r.try_get("wake_at")?,
        tokens: r.try_get("tokens")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
    })
}

fn approval_from_row(r: &AnyRow) -> Result<Approval> {
    Ok(Approval {
        id: r.try_get("id")?,
        run_id: r.try_get("run_id")?,
        step_id: r.try_get("step_id")?,
        node_id: r.try_get("node_id")?,
        kind: r.try_get("kind")?,
        state: r.try_get("state")?,
        title: r.try_get("title")?,
        approvers_json: r.try_get("approvers_json")?,
        context_json: r.try_get("context_json")?,
        note: r.try_get("note")?,
        decided_by: r.try_get("decided_by")?,
        revision: r.try_get("revision")?,
        sla_at: r.try_get("sla_at")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        decided_at: r.try_get("decided_at")?,
    })
}

/// `?, ?, ?` for `n` values.
fn marks(n: usize) -> String {
    vec!["?"; n.max(1)].join(", ")
}

fn is_terminal_run(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled")
}

impl Store {
    // ---- workflows ---------------------------------------------------------

    /// Inserts a workflow and its first version. `false` when the slug is taken.
    pub async fn insert_workflow(&self, w: &Workflow, v: &WorkflowVersion) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        let res = sqlx::query(self.sql(
            "INSERT INTO workflows (id, slug, name, description, template_id, status, enabled, \
             published_version, draft_version, created_at, updated_at, origin, system_key, system_rev, \
             customised) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        ))
        .bind(&w.id)
        .bind(&w.slug)
        .bind(&w.name)
        .bind(&w.description)
        .bind(&w.template_id)
        .bind(&w.status)
        .bind(w.enabled as i64)
        .bind(w.published_version)
        .bind(w.draft_version)
        .bind(w.created_at)
        .bind(w.updated_at)
        .bind(if w.origin.is_empty() { "user" } else { w.origin.as_str() })
        .bind(&w.system_key)
        .bind(w.system_rev)
        .bind(w.customised as i64)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            return Ok(false);
        }
        self.insert_version_in(&mut tx, v).await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn insert_version_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Any>,
        v: &WorkflowVersion,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO workflow_versions (workflow_id, version, status, graph_json, graph_sha256, note, \
             est_tokens_min, est_tokens_max, revision, created_at, updated_at, published_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&v.workflow_id)
        .bind(v.version)
        .bind(&v.status)
        .bind(&v.graph_json)
        .bind(&v.graph_sha256)
        .bind(&v.note)
        .bind(v.est_tokens_min)
        .bind(v.est_tokens_max)
        .bind(v.revision)
        .bind(v.created_at)
        .bind(v.updated_at)
        .bind(v.published_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// By id or slug.
    pub async fn workflow(&self, id_or_slug: &str) -> Result<Option<Workflow>> {
        let q = format!("SELECT {WF_COLS} FROM workflows WHERE id = ? OR slug = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id_or_slug)
            .bind(id_or_slug)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(wf_from_row).transpose()
    }

    /// Newest change first.
    pub async fn workflows(&self, include_archived: bool) -> Result<Vec<Workflow>> {
        let q = if include_archived {
            format!("SELECT {WF_COLS} FROM workflows ORDER BY updated_at DESC")
        } else {
            format!(
                "SELECT {WF_COLS} FROM workflows WHERE status <> 'archived' ORDER BY updated_at DESC"
            )
        };
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(wf_from_row).collect()
    }

    pub async fn update_workflow_meta(
        &self,
        id: &str,
        name: &str,
        description: &str,
    ) -> Result<()> {
        sqlx::query(
            self.sql("UPDATE workflows SET name = ?, description = ?, updated_at = ? WHERE id = ?"),
        )
        .bind(name)
        .bind(description)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_workflow_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE workflows SET enabled = ?, updated_at = ? WHERE id = ? AND status <> 'archived'",
        ))
        .bind(enabled as i64)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Archives the workflow and removes its trigger rows. History is kept.
    pub async fn archive_workflow(&self, id: &str) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        let now = now_ms();
        let res = sqlx::query(self.sql(
            "UPDATE workflows SET status = 'archived', enabled = 0, archived_at = ?, updated_at = ? \
             WHERE id = ? AND status <> 'archived'",
        ))
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(self.sql("DELETE FROM schedules WHERE workflow_id = ?"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- versions ----------------------------------------------------------

    /// Oldest first.
    pub async fn workflow_versions(&self, workflow_id: &str) -> Result<Vec<WorkflowVersion>> {
        let q = format!(
            "SELECT {VER_COLS} FROM workflow_versions WHERE workflow_id = ? ORDER BY version"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(workflow_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(ver_from_row).collect()
    }

    pub async fn workflow_version(
        &self,
        workflow_id: &str,
        version: i64,
    ) -> Result<Option<WorkflowVersion>> {
        let q = format!(
            "SELECT {VER_COLS} FROM workflow_versions WHERE workflow_id = ? AND version = ?"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(workflow_id)
            .bind(version)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(ver_from_row).transpose()
    }

    /// Creates the next draft version (one above the highest) when the
    /// workflow has none. `None` when a draft already exists.
    pub async fn create_draft_version(
        &self,
        workflow_id: &str,
        graph_json: &str,
        graph_sha256: &str,
        est: (i64, i64),
    ) -> Result<Option<WorkflowVersion>> {
        let mut tx = self.begin_write().await?;
        let has_draft = sqlx::query(self.sql("SELECT draft_version FROM workflows WHERE id = ?"))
            .bind(workflow_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get::<i64, _>("draft_version"))
            .context("unknown workflow")?;
        if has_draft != 0 {
            return Ok(None);
        }
        let max = sqlx::query(self.sql(
            "SELECT COALESCE(MAX(version), 0) AS m FROM workflow_versions WHERE workflow_id = ?",
        ))
        .bind(workflow_id)
        .fetch_one(&mut *tx)
        .await?
        .get::<i64, _>("m");
        let now = now_ms();
        let v = WorkflowVersion {
            workflow_id: workflow_id.into(),
            version: max + 1,
            status: "draft".into(),
            graph_json: graph_json.into(),
            graph_sha256: graph_sha256.into(),
            note: String::new(),
            est_tokens_min: est.0,
            est_tokens_max: est.1,
            revision: 1,
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        self.insert_version_in(&mut tx, &v).await?;
        sqlx::query(
            self.sql("UPDATE workflows SET draft_version = ?, updated_at = ? WHERE id = ?"),
        )
        .bind(v.version)
        .bind(now)
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(v))
    }

    /// Saves the draft graph when `revision` is still current; returns the
    /// new revision, or `None` on a conflict (or when it is not a draft).
    pub async fn save_draft(
        &self,
        workflow_id: &str,
        version: i64,
        revision: i64,
        graph_json: &str,
        graph_sha256: &str,
        est: (i64, i64),
    ) -> Result<Option<i64>> {
        let mut tx = self.begin_write().await?;
        let now = now_ms();
        let row = sqlx::query(self.sql(
            "UPDATE workflow_versions SET graph_json = ?, graph_sha256 = ?, est_tokens_min = ?, \
             est_tokens_max = ?, revision = revision + 1, updated_at = ? \
             WHERE workflow_id = ? AND version = ? AND status = 'draft' AND revision = ? \
             RETURNING revision",
        ))
        .bind(graph_json)
        .bind(graph_sha256)
        .bind(est.0)
        .bind(est.1)
        .bind(now)
        .bind(workflow_id)
        .bind(version)
        .bind(revision)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        sqlx::query(self.sql("UPDATE workflows SET updated_at = ? WHERE id = ?"))
            .bind(now)
            .bind(workflow_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(row.try_get("revision")?))
    }

    /// Drops the draft version. `false` when there was none.
    pub async fn delete_draft(&self, workflow_id: &str) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        let res = sqlx::query(
            self.sql("DELETE FROM workflow_versions WHERE workflow_id = ? AND status = 'draft'"),
        )
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            self.sql("UPDATE workflows SET draft_version = 0, updated_at = ? WHERE id = ?"),
        )
        .bind(now_ms())
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }

    /// Freezes draft `version`, supersedes the previous published version
    /// and reconciles the trigger rows with `triggers` (rows keep their
    /// enabled flag and fire history). The first publish enables the
    /// workflow. `false` when `version` is not the draft.
    pub async fn publish_version(
        &self,
        workflow_id: &str,
        version: i64,
        note: &str,
        triggers: &[ScheduleSpec],
    ) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        let now = now_ms();
        let res = sqlx::query(self.sql(
            "UPDATE workflow_versions SET status = 'published', note = ?, published_at = ?, updated_at = ? \
             WHERE workflow_id = ? AND version = ? AND status = 'draft'",
        ))
        .bind(note)
        .bind(now)
        .bind(now)
        .bind(workflow_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() != 1 {
            return Ok(false);
        }
        sqlx::query(self.sql(
            "UPDATE workflow_versions SET status = 'superseded', updated_at = ? \
             WHERE workflow_id = ? AND status = 'published' AND version <> ?",
        ))
        .bind(now)
        .bind(workflow_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        sqlx::query(self.sql(
            "UPDATE workflows SET enabled = CASE WHEN published_version = 0 THEN 1 ELSE enabled END, \
             published_version = ?, draft_version = 0, status = 'published', updated_at = ? WHERE id = ?",
        ))
        .bind(version)
        .bind(now)
        .bind(workflow_id)
        .execute(&mut *tx)
        .await?;

        let existing: Vec<String> =
            sqlx::query(self.sql("SELECT node_id FROM schedules WHERE workflow_id = ?"))
                .bind(workflow_id)
                .fetch_all(&mut *tx)
                .await?
                .iter()
                .map(|r| r.get::<String, _>("node_id"))
                .collect();
        for node in existing {
            if !triggers.iter().any(|t| t.node_id == node) {
                sqlx::query(
                    self.sql("DELETE FROM schedules WHERE workflow_id = ? AND node_id = ?"),
                )
                .bind(workflow_id)
                .bind(&node)
                .execute(&mut *tx)
                .await?;
            }
        }
        for t in triggers {
            sqlx::query(self.sql(
                "INSERT INTO schedules (id, workflow_id, node_id, kind, cron, tz, config_json, enabled, \
                 next_run_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?) \
                 ON CONFLICT (workflow_id, node_id) DO UPDATE SET kind = excluded.kind, \
                 cron = excluded.cron, tz = excluded.tz, config_json = excluded.config_json, \
                 next_run_at = excluded.next_run_at, updated_at = excluded.updated_at",
            ))
            .bind(&t.id)
            .bind(workflow_id)
            .bind(&t.node_id)
            .bind(&t.kind)
            .bind(&t.cron)
            .bind(&t.tz)
            .bind(&t.config_json)
            .bind(t.next_run_at)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    // ---- schedules ---------------------------------------------------------

    /// Every trigger row, soonest first, disabled last.
    pub async fn schedules(&self) -> Result<Vec<Schedule>> {
        let q = format!(
            "SELECT {SCHED_COLS} FROM schedules ORDER BY enabled DESC, \
             CASE WHEN next_run_at IS NULL THEN 1 ELSE 0 END, next_run_at, created_at"
        );
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(sched_from_row).collect()
    }

    pub async fn schedules_for_workflow(&self, workflow_id: &str) -> Result<Vec<Schedule>> {
        let q =
            format!("SELECT {SCHED_COLS} FROM schedules WHERE workflow_id = ? ORDER BY node_id");
        let rows = sqlx::query(self.sql(&q))
            .bind(workflow_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(sched_from_row).collect()
    }

    pub async fn schedule(&self, id: &str) -> Result<Option<Schedule>> {
        let q = format!("SELECT {SCHED_COLS} FROM schedules WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(sched_from_row).transpose()
    }

    pub async fn set_schedule_enabled(
        &self,
        id: &str,
        enabled: bool,
        next_run_at: Option<i64>,
    ) -> Result<bool> {
        let res =
            sqlx::query(self.sql(
                "UPDATE schedules SET enabled = ?, next_run_at = ?, updated_at = ? WHERE id = ?",
            ))
            .bind(enabled as i64)
            .bind(next_run_at)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn set_schedule_next(&self, id: &str, next_run_at: Option<i64>) -> Result<()> {
        sqlx::query(self.sql("UPDATE schedules SET next_run_at = ? WHERE id = ?"))
            .bind(next_run_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a fire and moves the timer on. Guarded on the previous
    /// `next_run_at` so a fire is recorded once.
    pub async fn record_schedule_fire(
        &self,
        id: &str,
        expected_next: Option<i64>,
        fired_at: i64,
        next_run_at: Option<i64>,
        run_id: Option<&str>,
    ) -> Result<bool> {
        let q = if expected_next.is_some() {
            "UPDATE schedules SET last_fired_at = ?, next_run_at = ?, \
             last_run_id = COALESCE(?, last_run_id) WHERE id = ? AND next_run_at = ?"
        } else {
            "UPDATE schedules SET last_fired_at = ?, next_run_at = ?, \
             last_run_id = COALESCE(?, last_run_id) WHERE id = ? AND next_run_at IS NULL"
        };
        let mut query = sqlx::query(self.sql(q))
            .bind(fired_at)
            .bind(next_run_at)
            .bind(run_id)
            .bind(id);
        if let Some(e) = expected_next {
            query = query.bind(e);
        }
        let res = query.execute(&self.pool).await?;
        Ok(res.rows_affected() == 1)
    }

    /// Enabled rows of enabled, published workflows (what may fire).
    pub async fn live_schedules(&self) -> Result<Vec<Schedule>> {
        let q = "SELECT s.id, s.workflow_id, s.node_id, s.kind, s.cron, s.tz, s.config_json, s.enabled, \
             s.next_run_at, s.last_fired_at, s.last_run_id, s.created_at, s.updated_at \
             FROM schedules s JOIN workflows w ON w.id = s.workflow_id \
             WHERE s.enabled = 1 AND w.enabled = 1 AND w.status = 'published' ORDER BY s.created_at";
        let rows = sqlx::query(self.sql(q)).fetch_all(&self.pool).await?;
        rows.iter().map(sched_from_row).collect()
    }

    /// Earliest timer among live cron rows.
    pub async fn next_schedule_fire(&self) -> Result<Option<i64>> {
        let row = sqlx::query(self.sql(
            "SELECT MIN(s.next_run_at) AS m FROM schedules s JOIN workflows w ON w.id = s.workflow_id \
             WHERE s.enabled = 1 AND w.enabled = 1 AND w.status = 'published' AND s.next_run_at IS NOT NULL",
        ))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("m")?)
    }

    // ---- runs --------------------------------------------------------------

    /// `false` when a run with the same dedupe key exists.
    pub async fn insert_run(&self, r: &Run) -> Result<bool> {
        let q = format!(
            "INSERT INTO runs ({RUN_COLS}) VALUES ({}) ON CONFLICT DO NOTHING",
            marks(20)
        );
        let res = sqlx::query(self.sql(&q))
            .bind(&r.id)
            .bind(&r.workflow_id)
            .bind(r.workflow_version)
            .bind(&r.schedule_id)
            .bind(&r.trigger_kind)
            .bind(&r.trigger_node_id)
            .bind(&r.trigger_label)
            .bind(&r.dedupe_key)
            .bind(&r.repo)
            .bind(&r.card_id)
            .bind(&r.event_json)
            .bind(&r.inputs_json)
            .bind(&r.status)
            .bind(&r.reason)
            .bind(r.dry_run as i64)
            .bind(r.tokens)
            .bind(r.created_at)
            .bind(r.updated_at)
            .bind(r.started_at)
            .bind(r.finished_at)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn run(&self, id: &str) -> Result<Option<Run>> {
        let q = format!("SELECT {RUN_COLS} FROM runs WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    pub async fn run_by_dedupe(&self, key: &str) -> Result<Option<Run>> {
        let q = format!("SELECT {RUN_COLS} FROM runs WHERE dedupe_key = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    /// Newest first.
    pub async fn runs(&self, f: &RunFilter) -> Result<Vec<Run>> {
        let mut q = format!("SELECT {RUN_COLS} FROM runs WHERE 1 = 1");
        if !f.statuses.is_empty() {
            q.push_str(&format!(" AND status IN ({})", marks(f.statuses.len())));
        }
        if f.workflow_id.is_some() {
            q.push_str(" AND workflow_id = ?");
        }
        if f.trigger_kind.is_some() {
            q.push_str(" AND trigger_kind = ?");
        }
        if f.q.is_some() {
            q.push_str(
                " AND (LOWER(id) LIKE ? OR LOWER(repo) LIKE ? OR LOWER(card_id) LIKE ? OR LOWER(reason) LIKE ?)",
            );
        }
        if f.from.is_some() {
            q.push_str(" AND created_at >= ?");
        }
        if f.to.is_some() {
            q.push_str(" AND created_at <= ?");
        }
        q.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql);
        for s in &f.statuses {
            query = query.bind(s.clone());
        }
        if let Some(w) = &f.workflow_id {
            query = query.bind(w.clone());
        }
        if let Some(t) = &f.trigger_kind {
            query = query.bind(t.clone());
        }
        if let Some(text) = &f.q {
            let like = format!("%{}%", text.to_lowercase());
            for _ in 0..4 {
                query = query.bind(like.clone());
            }
        }
        if let Some(v) = f.from {
            query = query.bind(v);
        }
        if let Some(v) = f.to {
            query = query.bind(v);
        }
        let limit = if f.limit <= 0 { 200 } else { f.limit };
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(run_from_row).collect()
    }

    /// Runs of a workflow in `statuses`, oldest first (optionally one repo).
    pub async fn workflow_runs_in(
        &self,
        workflow_id: &str,
        statuses: &[&str],
        repo: Option<&str>,
    ) -> Result<Vec<Run>> {
        let mut q = format!(
            "SELECT {RUN_COLS} FROM runs WHERE workflow_id = ? AND status IN ({})",
            marks(statuses.len())
        );
        if repo.is_some() {
            q.push_str(" AND repo = ?");
        }
        q.push_str(" ORDER BY created_at, id");
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql).bind(workflow_id);
        for s in statuses {
            query = query.bind(*s);
        }
        if let Some(r) = repo {
            query = query.bind(r);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(run_from_row).collect()
    }

    /// Real (not dry) runs created at or after `since`, per workflow id.
    pub async fn run_counts_since(
        &self,
        since: i64,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let rows = sqlx::query(self.sql(
            "SELECT workflow_id, COUNT(*) AS n FROM runs \
             WHERE created_at >= ? AND dry_run = 0 GROUP BY workflow_id",
        ))
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("workflow_id")?, r.try_get("n")?)))
            .collect()
    }

    /// Every run in `statuses`, oldest first.
    pub async fn runs_in(&self, statuses: &[&str]) -> Result<Vec<Run>> {
        let q = format!(
            "SELECT {RUN_COLS} FROM runs WHERE status IN ({}) ORDER BY created_at, id",
            marks(statuses.len())
        );
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql);
        for s in statuses {
            query = query.bind(*s);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(run_from_row).collect()
    }

    /// Guarded status change: only from one of `from`. Sets `started_at` on
    /// the first move to running and `finished_at` on a terminal status.
    pub async fn set_run_status(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        reason: &str,
    ) -> Result<bool> {
        let now = now_ms();
        let started = if to == "running" { Some(now) } else { None };
        let finished = if is_terminal_run(to) { Some(now) } else { None };
        let q = format!(
            "UPDATE runs SET status = ?, reason = ?, updated_at = ?, \
             started_at = COALESCE(started_at, ?), finished_at = COALESCE(?, finished_at) \
             WHERE id = ? AND status IN ({})",
            marks(from.len())
        );
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql)
            .bind(to)
            .bind(reason)
            .bind(now)
            .bind(started)
            .bind(finished)
            .bind(id);
        for s in from {
            query = query.bind(*s);
        }
        let res = query.execute(&self.pool).await?;
        Ok(res.rows_affected() == 1)
    }

    /// Clears `finished_at` (a retry reopens a run).
    pub async fn reopen_run(&self, id: &str, from: &[&str]) -> Result<bool> {
        let q = format!(
            "UPDATE runs SET status = 'queued', reason = '', finished_at = NULL, updated_at = ? \
             WHERE id = ? AND status IN ({})",
            marks(from.len())
        );
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql).bind(now_ms()).bind(id);
        for s in from {
            query = query.bind(*s);
        }
        Ok(query.execute(&self.pool).await?.rows_affected() == 1)
    }

    pub async fn add_run_tokens(&self, id: &str, tokens: i64) -> Result<()> {
        sqlx::query(self.sql("UPDATE runs SET tokens = tokens + ? WHERE id = ?"))
            .bind(tokens)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- steps -------------------------------------------------------------

    /// Inserts a step with the next `seq` of its run. `false` when the
    /// (run, node, attempt) already exists.
    pub async fn insert_step(&self, s: &RunStep) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        let seq = sqlx::query(
            self.sql("SELECT COALESCE(MAX(seq), 0) AS m FROM run_steps WHERE run_id = ?"),
        )
        .bind(&s.run_id)
        .fetch_one(&mut *tx)
        .await?
        .get::<i64, _>("m")
            + 1;
        let q = format!(
            "INSERT INTO run_steps ({STEP_COLS}) VALUES ({}) ON CONFLICT DO NOTHING",
            marks(20)
        );
        let res = sqlx::query(self.sql(&q))
            .bind(&s.id)
            .bind(&s.run_id)
            .bind(seq)
            .bind(&s.node_id)
            .bind(&s.node_type)
            .bind(&s.title)
            .bind(s.attempt)
            .bind(&s.status)
            .bind(&s.route)
            .bind(&s.error)
            .bind(&s.details_json)
            .bind(&s.dispatch_id)
            .bind(&s.session_id)
            .bind(&s.node_ref)
            .bind(s.wake_at)
            .bind(s.tokens)
            .bind(s.created_at)
            .bind(s.updated_at)
            .bind(s.started_at)
            .bind(s.finished_at)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() == 1)
    }

    /// In creation order.
    pub async fn run_steps(&self, run_id: &str) -> Result<Vec<RunStep>> {
        let q = format!("SELECT {STEP_COLS} FROM run_steps WHERE run_id = ? ORDER BY seq");
        let rows = sqlx::query(self.sql(&q))
            .bind(run_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(step_from_row).collect()
    }

    pub async fn step(&self, id: &str) -> Result<Option<RunStep>> {
        let q = format!("SELECT {STEP_COLS} FROM run_steps WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(step_from_row).transpose()
    }

    /// Applies `u` when the step's status is one of `from` (any status when
    /// `from` is empty). Adds `u.add_tokens` to the run as well.
    pub async fn update_step(&self, id: &str, from: &[&str], u: &StepUpdate) -> Result<bool> {
        let now = now_ms();
        let mut set = vec![
            "updated_at = ?".to_string(),
            "tokens = tokens + ?".to_string(),
        ];
        if u.status.is_some() {
            set.push("status = ?".into());
        }
        if u.route.is_some() {
            set.push("route = ?".into());
        }
        if u.error.is_some() {
            set.push("error = ?".into());
        }
        if u.details_json.is_some() {
            set.push("details_json = ?".into());
        }
        if u.dispatch_id.is_some() {
            set.push("dispatch_id = ?".into());
        }
        if u.session_id.is_some() {
            set.push("session_id = ?".into());
        }
        if u.node_ref.is_some() {
            set.push("node_ref = ?".into());
        }
        if u.wake_at.is_some() {
            set.push("wake_at = ?".into());
        }
        if u.started {
            set.push("started_at = COALESCE(started_at, ?)".into());
        }
        if u.finished {
            set.push("finished_at = ?".into());
        }
        let mut q = format!("UPDATE run_steps SET {} WHERE id = ?", set.join(", "));
        if !from.is_empty() {
            q.push_str(&format!(" AND status IN ({})", marks(from.len())));
        }
        q.push_str(" RETURNING run_id");
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql).bind(now).bind(u.add_tokens);
        if let Some(v) = &u.status {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.route {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.error {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.details_json {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.dispatch_id {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.session_id {
            query = query.bind(v.clone());
        }
        if let Some(v) = &u.node_ref {
            query = query.bind(v.clone());
        }
        if let Some(v) = u.wake_at {
            query = query.bind(v);
        }
        if u.started {
            query = query.bind(now);
        }
        if u.finished {
            query = query.bind(now);
        }
        query = query.bind(id);
        for s in from {
            query = query.bind(*s);
        }
        let row = query.fetch_optional(&self.pool).await?;
        let Some(row) = row else {
            return Ok(false);
        };
        if u.add_tokens != 0 {
            let run_id: String = row.try_get("run_id")?;
            self.add_run_tokens(&run_id, u.add_tokens).await?;
        }
        Ok(true)
    }

    /// Steps in `statuses` across every run (recovery, event matching).
    pub async fn steps_in(&self, statuses: &[&str]) -> Result<Vec<RunStep>> {
        let q = format!(
            "SELECT {STEP_COLS} FROM run_steps WHERE status IN ({}) ORDER BY created_at, seq",
            marks(statuses.len())
        );
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql);
        for s in statuses {
            query = query.bind(*s);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(step_from_row).collect()
    }

    pub async fn step_by_dispatch(&self, dispatch_id: &str) -> Result<Option<RunStep>> {
        let q = format!(
            "SELECT {STEP_COLS} FROM run_steps WHERE dispatch_id = ? ORDER BY seq DESC LIMIT 1"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(dispatch_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(step_from_row).transpose()
    }

    /// Run ids with a pending step due or a waiting step whose wake passed,
    /// among runs that are not paused or over.
    pub async fn due_step_runs(&self, now: i64) -> Result<Vec<String>> {
        let rows = sqlx::query(self.sql(
            "SELECT DISTINCT s.run_id AS run_id FROM run_steps s JOIN runs r ON r.id = s.run_id \
             WHERE r.status IN ('queued', 'running', 'waiting_approval') AND s.wake_at IS NOT NULL \
             AND s.wake_at <= ? AND s.status IN ('pending', 'waiting')",
        ))
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("run_id")).collect())
    }

    /// Earliest step wake among runs that can move.
    pub async fn next_step_wake(&self) -> Result<Option<i64>> {
        let row = sqlx::query(self.sql(
            "SELECT MIN(s.wake_at) AS m FROM run_steps s JOIN runs r ON r.id = s.run_id \
             WHERE r.status IN ('queued', 'running', 'waiting_approval') AND s.wake_at IS NOT NULL \
             AND s.status IN ('pending', 'waiting')",
        ))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("m")?)
    }

    /// Cancels every open step of a run; returns how many.
    pub async fn cancel_open_steps(&self, run_id: &str, error: &str) -> Result<u64> {
        let now = now_ms();
        let res = sqlx::query(self.sql(
            "UPDATE run_steps SET status = 'cancelled', error = ?, finished_at = ?, updated_at = ?, wake_at = NULL \
             WHERE run_id = ? AND status IN ('pending', 'running', 'waiting')",
        ))
        .bind(error)
        .bind(now)
        .bind(now)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    // ---- approvals ---------------------------------------------------------

    pub async fn insert_approval(&self, a: &Approval) -> Result<()> {
        let q = format!(
            "INSERT INTO approvals ({APPROVAL_COLS}) VALUES ({})",
            marks(16)
        );
        sqlx::query(self.sql(&q))
            .bind(&a.id)
            .bind(&a.run_id)
            .bind(&a.step_id)
            .bind(&a.node_id)
            .bind(&a.kind)
            .bind(&a.state)
            .bind(&a.title)
            .bind(&a.approvers_json)
            .bind(&a.context_json)
            .bind(&a.note)
            .bind(&a.decided_by)
            .bind(a.revision)
            .bind(a.sla_at)
            .bind(a.created_at)
            .bind(a.updated_at)
            .bind(a.decided_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn approval(&self, id: &str) -> Result<Option<Approval>> {
        let q = format!("SELECT {APPROVAL_COLS} FROM approvals WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(approval_from_row).transpose()
    }

    /// Newest first. `state` `None` is every state.
    pub async fn approvals(
        &self,
        state: Option<&str>,
        run_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Approval>> {
        let mut q = format!("SELECT {APPROVAL_COLS} FROM approvals WHERE 1 = 1");
        if state.is_some() {
            q.push_str(" AND state = ?");
        }
        if run_id.is_some() {
            q.push_str(" AND run_id = ?");
        }
        q.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql);
        if let Some(s) = state {
            query = query.bind(s);
        }
        if let Some(r) = run_id {
            query = query.bind(r);
        }
        let rows = query
            .bind(if limit <= 0 { 500 } else { limit })
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(approval_from_row).collect()
    }

    /// The pending approval of a step, if any.
    pub async fn step_approval(&self, step_id: &str) -> Result<Option<Approval>> {
        let q = format!(
            "SELECT {APPROVAL_COLS} FROM approvals WHERE step_id = ? ORDER BY created_at DESC LIMIT 1"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(step_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(approval_from_row).transpose()
    }

    /// Moves a pending approval to `state` (approved, rejected, expired,
    /// cancelled). With `revision`, only while it is still that revision.
    /// `None` when it was already decided or the revision moved.
    pub async fn decide_approval(
        &self,
        id: &str,
        state: &str,
        decided_by: &str,
        note: &str,
        revision: Option<i64>,
    ) -> Result<Option<Approval>> {
        let now = now_ms();
        let mut q = String::from(
            "UPDATE approvals SET state = ?, decided_by = ?, note = ?, decided_at = ?, updated_at = ?, \
             revision = revision + 1 WHERE id = ? AND state = 'pending'",
        );
        if revision.is_some() {
            q.push_str(" AND revision = ?");
        }
        q.push_str(&format!(" RETURNING {APPROVAL_COLS}"));
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql)
            .bind(state)
            .bind(decided_by)
            .bind(note)
            .bind(now)
            .bind(now)
            .bind(id);
        if let Some(r) = revision {
            query = query.bind(r);
        }
        let row = query.fetch_optional(&self.pool).await?;
        row.as_ref().map(approval_from_row).transpose()
    }

    /// Renotify: a new SLA for a pending approval.
    pub async fn set_approval_sla(&self, id: &str, sla_at: Option<i64>) -> Result<()> {
        sqlx::query(self.sql(
            "UPDATE approvals SET sla_at = ?, updated_at = ? WHERE id = ? AND state = 'pending'",
        ))
        .bind(sla_at)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Cancels a run's pending approvals; returns how many.
    pub async fn cancel_run_approvals(&self, run_id: &str) -> Result<u64> {
        let now = now_ms();
        let res = sqlx::query(self.sql(
            "UPDATE approvals SET state = 'cancelled', decided_at = ?, updated_at = ?, revision = revision + 1 \
             WHERE run_id = ? AND state = 'pending'",
        ))
        .bind(now)
        .bind(now)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    // ---- templates ---------------------------------------------------------

    pub async fn dismiss_template(&self, template_id: &str) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO template_dismissals (template_id, dismissed_at) VALUES (?, ?) \
             ON CONFLICT (template_id) DO UPDATE SET dismissed_at = excluded.dismissed_at",
        ))
        .bind(template_id)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn undismiss_template(&self, template_id: &str) -> Result<()> {
        sqlx::query(self.sql("DELETE FROM template_dismissals WHERE template_id = ?"))
            .bind(template_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn dismissed_templates(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            self.sql("SELECT template_id FROM template_dismissals ORDER BY template_id"),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("template_id"))
            .collect())
    }

    // ---- trigger fires (safeguards) ------------------------------------------

    /// Records one trigger evaluation that passed its conditions.
    pub async fn insert_trigger_fire(&self, f: &TriggerFire) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO trigger_fires (schedule_id, subject, at, event_id, run_id, outcome) \
             VALUES (?, ?, ?, ?, ?, ?)",
        ))
        .bind(&f.schedule_id)
        .bind(&f.subject)
        .bind(if f.at == 0 { now_ms() } else { f.at })
        .bind(f.event_id)
        .bind(&f.run_id)
        .bind(&f.outcome)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fires of a trigger at or after `since`, newest first.
    pub async fn trigger_fires(&self, schedule_id: &str, since: i64) -> Result<Vec<TriggerFire>> {
        let rows = sqlx::query(self.sql(
            "SELECT schedule_id, subject, at, event_id, run_id, outcome FROM trigger_fires \
             WHERE schedule_id = ? AND at >= ? ORDER BY at DESC, id DESC",
        ))
        .bind(schedule_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(TriggerFire {
                    schedule_id: r.try_get("schedule_id")?,
                    subject: r.try_get("subject")?,
                    at: r.try_get("at")?,
                    event_id: r.try_get("event_id")?,
                    run_id: r.try_get("run_id")?,
                    outcome: r.try_get("outcome")?,
                })
            })
            .collect()
    }

    /// Drops fire rows older than `before` (they only feed hourly windows).
    pub async fn prune_trigger_fires(&self, before: i64) -> Result<u64> {
        let res = sqlx::query(self.sql("DELETE FROM trigger_fires WHERE at < ?"))
            .bind(before)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Sets the defaults-catalogue fields of a workflow.
    pub async fn set_workflow_system(
        &self,
        id: &str,
        origin: &str,
        system_key: &str,
        system_rev: i64,
        customised: bool,
    ) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE workflows SET origin = ?, system_key = ?, system_rev = ?, customised = ?, \
             updated_at = ? WHERE id = ?",
        ))
        .bind(origin)
        .bind(system_key)
        .bind(system_rev)
        .bind(customised as i64)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- System One decisions (WP2) ------------------------------------------

    pub async fn insert_engine_decision(&self, d: &EngineDecision) -> Result<()> {
        let q = format!(
            "INSERT INTO engine_decisions ({DECISION_COLS}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        sqlx::query(self.sql(&q))
            .bind(&d.id)
            .bind(if d.at == 0 { now_ms() } else { d.at })
            .bind(&d.run_id)
            .bind(&d.step_id)
            .bind(&d.workflow_id)
            .bind(&d.node_id)
            .bind(&d.question)
            .bind(&d.choice)
            .bind(&d.outcome)
            .bind(&d.action)
            .bind(&d.why)
            .bind(d.confidence)
            .bind(d.threshold)
            .bind(&d.provider)
            .bind(&d.model)
            .bind(&d.node_ref)
            .bind(d.latency_ms)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Decisions, newest first: one run's, or every run's when `run_id` is `None`.
    pub async fn engine_decisions(
        &self,
        run_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<EngineDecision>> {
        let lim = limit.clamp(1, 10_000);
        let rows = match run_id {
            Some(r) => {
                let q = format!(
                    "SELECT {DECISION_COLS} FROM engine_decisions WHERE run_id = ? \
                     ORDER BY at DESC, id DESC LIMIT {lim}"
                );
                sqlx::query(self.sql(&q))
                    .bind(r)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let q = format!(
                    "SELECT {DECISION_COLS} FROM engine_decisions ORDER BY at DESC, id DESC LIMIT {lim}"
                );
                sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?
            }
        };
        rows.iter().map(decision_from_row).collect()
    }

    /// How many decisions were made at or after `since` (the daily cap).
    pub async fn count_engine_decisions(&self, since: i64) -> Result<i64> {
        let row = sqlx::query(self.sql("SELECT COUNT(*) AS n FROM engine_decisions WHERE at >= ?"))
            .bind(since)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    /// Drops decisions older than `before` (`systemone.retention`).
    pub async fn prune_engine_decisions(&self, before: i64) -> Result<u64> {
        let res = sqlx::query(self.sql("DELETE FROM engine_decisions WHERE at < ?"))
            .bind(before)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// The workflow seeded for a catalogue key, if any.
    pub async fn workflow_by_system_key(&self, key: &str) -> Result<Option<Workflow>> {
        let q =
            format!("SELECT {WF_COLS} FROM workflows WHERE system_key = ? AND system_key <> ''");
        let row = sqlx::query(self.sql(&q))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(wf_from_row).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut out = vec![(s, Some(dir))];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            let s = Store::open(&url).await.unwrap();
            for t in [
                "approvals",
                "run_steps",
                "runs",
                "schedules",
                "workflow_versions",
                "workflows",
                "template_dismissals",
                "trigger_fires",
                "engine_decisions",
            ] {
                sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                    .execute(&s.pool)
                    .await
                    .unwrap();
            }
            out.push((s, None));
        }
        out
    }

    fn wf(id: &str, slug: &str) -> (Workflow, WorkflowVersion) {
        let now = now_ms();
        (
            Workflow {
                id: id.into(),
                slug: slug.into(),
                name: slug.into(),
                status: "draft".into(),
                draft_version: 1,
                created_at: now,
                updated_at: now,
                ..Default::default()
            },
            WorkflowVersion {
                workflow_id: id.into(),
                version: 1,
                status: "draft".into(),
                graph_json: "{}".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
                ..Default::default()
            },
        )
    }

    fn run(id: &str, wf: &str, dedupe: Option<&str>) -> Run {
        let now = now_ms();
        Run {
            id: id.into(),
            workflow_id: wf.into(),
            workflow_version: 1,
            trigger_kind: "manual".into(),
            dedupe_key: dedupe.map(Into::into),
            event_json: "{}".into(),
            inputs_json: "{}".into(),
            status: "queued".into(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    fn step(id: &str, run: &str, node: &str, attempt: i64) -> RunStep {
        let now = now_ms();
        RunStep {
            id: id.into(),
            run_id: run.into(),
            node_id: node.into(),
            node_type: "trigger".into(),
            attempt,
            status: "pending".into(),
            details_json: "{}".into(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn workflow_lifecycle() {
        for (s, _d) in stores().await {
            let (w, v) = wf("w1", "patch");
            assert!(s.insert_workflow(&w, &v).await.unwrap());
            let (w2, v2) = wf("w2", "patch");
            assert!(!s.insert_workflow(&w2, &v2).await.unwrap(), "slug taken");
            assert_eq!(s.workflow("patch").await.unwrap().unwrap().id, "w1");
            s.update_workflow_meta("w1", "Patch train", "d")
                .await
                .unwrap();

            // Save with the right revision, then a stale one.
            let r = s
                .save_draft("w1", 1, 1, r#"{"nodes":[]}"#, "sha", (1, 2))
                .await
                .unwrap();
            assert_eq!(r, Some(2));
            assert_eq!(
                s.save_draft("w1", 1, 1, "{}", "", (0, 0)).await.unwrap(),
                None
            );
            assert!(
                s.create_draft_version("w1", "{}", "", (0, 0))
                    .await
                    .unwrap()
                    .is_none(),
                "draft exists"
            );

            let trig = ScheduleSpec {
                id: "s1".into(),
                node_id: "n1".into(),
                kind: "cron".into(),
                cron: "0 3 * * *".into(),
                tz: "UTC".into(),
                config_json: "{}".into(),
                next_run_at: Some(1000),
            };
            assert!(
                s.publish_version("w1", 1, "first", std::slice::from_ref(&trig))
                    .await
                    .unwrap()
            );
            assert!(!s.publish_version("w1", 1, "again", &[]).await.unwrap());
            let w = s.workflow("w1").await.unwrap().unwrap();
            assert_eq!(w.status, "published");
            assert!(w.enabled);
            assert_eq!((w.published_version, w.draft_version), (1, 0));
            assert_eq!(s.next_schedule_fire().await.unwrap(), Some(1000));

            // Second version: schedule kept (enabled flag survives), then dropped.
            assert!(s.set_schedule_enabled("s1", false, None).await.unwrap());
            let v2 = s
                .create_draft_version("w1", "{}", "", (0, 0))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(v2.version, 2);
            let mut t2 = trig.clone();
            t2.id = "s-new".into();
            t2.cron = "0 4 * * *".into();
            assert!(s.publish_version("w1", 2, "", &[t2]).await.unwrap());
            let rows = s.schedules_for_workflow("w1").await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, "s1");
            assert_eq!(rows[0].cron, "0 4 * * *");
            assert!(!rows[0].enabled);
            let vs = s.workflow_versions("w1").await.unwrap();
            assert_eq!(vs[0].status, "superseded");
            assert_eq!(vs[1].status, "published");

            s.create_draft_version("w1", "{}", "", (0, 0))
                .await
                .unwrap();
            assert!(s.delete_draft("w1").await.unwrap());
            assert_eq!(s.workflow("w1").await.unwrap().unwrap().draft_version, 0);

            assert!(s.set_workflow_enabled("w1", false).await.unwrap());
            assert!(s.archive_workflow("w1").await.unwrap());
            assert!(s.schedules().await.unwrap().is_empty());
            assert!(s.workflows(false).await.unwrap().is_empty());
            assert_eq!(s.workflows(true).await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn schedule_fire_guard() {
        for (s, _d) in stores().await {
            let (w, v) = wf("w1", "a");
            s.insert_workflow(&w, &v).await.unwrap();
            let t = ScheduleSpec {
                id: "s1".into(),
                node_id: "n1".into(),
                kind: "cron".into(),
                next_run_at: Some(500),
                ..Default::default()
            };
            s.publish_version("w1", 1, "", &[t]).await.unwrap();
            assert_eq!(s.live_schedules().await.unwrap().len(), 1);
            assert!(
                s.record_schedule_fire("s1", Some(500), 501, Some(900), Some("r1"))
                    .await
                    .unwrap()
            );
            assert!(
                !s.record_schedule_fire("s1", Some(500), 502, Some(900), None)
                    .await
                    .unwrap(),
                "fired once"
            );
            let row = s.schedule("s1").await.unwrap().unwrap();
            assert_eq!(row.next_run_at, Some(900));
            assert_eq!(row.last_run_id.as_deref(), Some("r1"));
            s.set_schedule_next("s1", Some(950)).await.unwrap();
            assert_eq!(s.next_schedule_fire().await.unwrap(), Some(950));
            s.set_workflow_enabled("w1", false).await.unwrap();
            assert!(s.live_schedules().await.unwrap().is_empty());
            assert_eq!(s.next_schedule_fire().await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn run_counts_since_skip_old_and_dry_runs() {
        for (s, _d) in stores().await {
            for (id, slug) in [("w1", "a"), ("w2", "b")] {
                let (w, v) = wf(id, slug);
                s.insert_workflow(&w, &v).await.unwrap();
            }
            let now = now_ms();
            s.insert_run(&run("r1", "w1", None)).await.unwrap();
            s.insert_run(&run("r2", "w1", None)).await.unwrap();
            s.insert_run(&run("r3", "w2", None)).await.unwrap();
            let mut old = run("r4", "w2", None);
            old.created_at = now - 8 * 86_400_000;
            s.insert_run(&old).await.unwrap();
            let mut dry = run("r5", "w2", None);
            dry.dry_run = true;
            s.insert_run(&dry).await.unwrap();
            let n = s.run_counts_since(now - 7 * 86_400_000).await.unwrap();
            assert_eq!(n.get("w1"), Some(&2));
            assert_eq!(n.get("w2"), Some(&1));
            assert_eq!(n.len(), 2);
        }
    }

    #[tokio::test]
    async fn runs_steps_approvals() {
        for (s, _d) in stores().await {
            let (w, v) = wf("w1", "a");
            s.insert_workflow(&w, &v).await.unwrap();
            assert!(s.insert_run(&run("r1", "w1", Some("k1"))).await.unwrap());
            assert!(
                !s.insert_run(&run("r2", "w1", Some("k1"))).await.unwrap(),
                "dedupe"
            );
            assert!(s.insert_run(&run("r3", "w1", None)).await.unwrap());
            assert!(s.insert_run(&run("r4", "w1", None)).await.unwrap());
            assert_eq!(s.run_by_dedupe("k1").await.unwrap().unwrap().id, "r1");

            assert!(
                s.set_run_status("r1", &["queued"], "running", "")
                    .await
                    .unwrap()
            );
            assert!(
                !s.set_run_status("r1", &["queued"], "running", "")
                    .await
                    .unwrap()
            );
            let r = s.run("r1").await.unwrap().unwrap();
            assert!(r.started_at.is_some() && r.finished_at.is_none());

            let f = RunFilter {
                statuses: vec!["running".into()],
                ..Default::default()
            };
            assert_eq!(s.runs(&f).await.unwrap().len(), 1);
            let f = RunFilter {
                q: Some("R3".into()),
                workflow_id: Some("w1".into()),
                trigger_kind: Some("manual".into()),
                ..Default::default()
            };
            assert_eq!(s.runs(&f).await.unwrap()[0].id, "r3");
            assert_eq!(
                s.workflow_runs_in("w1", &ACTIVE_RUN_STATUSES, None)
                    .await
                    .unwrap()
                    .len(),
                3
            );
            assert_eq!(s.runs_in(&["queued"]).await.unwrap().len(), 2);

            assert!(s.insert_step(&step("st1", "r1", "n1", 1)).await.unwrap());
            assert!(!s.insert_step(&step("st1b", "r1", "n1", 1)).await.unwrap());
            assert!(s.insert_step(&step("st2", "r1", "n2", 1)).await.unwrap());
            let steps = s.run_steps("r1").await.unwrap();
            assert_eq!(steps.iter().map(|x| x.seq).collect::<Vec<_>>(), [1, 2]);

            let u = StepUpdate {
                status: Some("waiting".into()),
                dispatch_id: Some(Some("d1".into())),
                wake_at: Some(Some(100)),
                add_tokens: 7,
                started: true,
                ..Default::default()
            };
            assert!(s.update_step("st2", &["pending"], &u).await.unwrap());
            assert!(!s.update_step("st2", &["pending"], &u).await.unwrap());
            assert_eq!(s.run("r1").await.unwrap().unwrap().tokens, 7);
            assert_eq!(s.step_by_dispatch("d1").await.unwrap().unwrap().id, "st2");
            assert_eq!(s.next_step_wake().await.unwrap(), Some(100));
            assert_eq!(s.due_step_runs(100).await.unwrap(), vec!["r1".to_string()]);
            assert!(s.due_step_runs(99).await.unwrap().is_empty());
            assert_eq!(s.steps_in(&["waiting"]).await.unwrap().len(), 1);

            let now = now_ms();
            let a = Approval {
                id: "a1".into(),
                run_id: "r1".into(),
                step_id: "st2".into(),
                node_id: "n2".into(),
                kind: "human".into(),
                state: "pending".into(),
                approvers_json: "[]".into(),
                context_json: "{}".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
                ..Default::default()
            };
            s.insert_approval(&a).await.unwrap();
            s.set_approval_sla("a1", Some(5)).await.unwrap();
            assert_eq!(
                s.approvals(Some("pending"), None, 0).await.unwrap().len(),
                1
            );
            assert_eq!(s.step_approval("st2").await.unwrap().unwrap().id, "a1");
            assert!(
                s.decide_approval("a1", "approved", "me", "ok", Some(9))
                    .await
                    .unwrap()
                    .is_none(),
                "stale revision"
            );
            let d = s
                .decide_approval("a1", "approved", "me", "ok", Some(1))
                .await
                .unwrap()
                .unwrap();
            assert_eq!((d.state.as_str(), d.revision), ("approved", 2));
            assert!(
                s.decide_approval("a1", "rejected", "me", "", None)
                    .await
                    .unwrap()
                    .is_none()
            );
            let mut a2 = a.clone();
            a2.id = "a2".into();
            s.insert_approval(&a2).await.unwrap();
            assert_eq!(s.cancel_run_approvals("r1").await.unwrap(), 1);
            assert_eq!(s.approvals(None, Some("r1"), 0).await.unwrap().len(), 2);

            assert_eq!(s.cancel_open_steps("r1", "cancelled").await.unwrap(), 2);
            assert!(
                s.set_run_status("r1", &["running"], "failed", "x")
                    .await
                    .unwrap()
            );
            assert!(s.run("r1").await.unwrap().unwrap().finished_at.is_some());
            assert!(s.reopen_run("r1", &["failed"]).await.unwrap());
            let r = s.run("r1").await.unwrap().unwrap();
            assert_eq!((r.status.as_str(), r.finished_at), ("queued", None));
        }
    }

    #[tokio::test]
    async fn template_dismissals() {
        for (s, _d) in stores().await {
            s.dismiss_template("a").await.unwrap();
            s.dismiss_template("a").await.unwrap();
            s.dismiss_template("b").await.unwrap();
            assert_eq!(s.dismissed_templates().await.unwrap(), ["a", "b"]);
            s.undismiss_template("a").await.unwrap();
            assert_eq!(s.dismissed_templates().await.unwrap(), ["b"]);
        }
    }

    #[tokio::test]
    async fn trigger_fires_and_system_fields() {
        for (s, _d) in stores().await {
            for (i, outcome) in ["started", "skipped:debounced", "started"]
                .iter()
                .enumerate()
            {
                s.insert_trigger_fire(&TriggerFire {
                    schedule_id: "s1".into(),
                    subject: "acme/api#7".into(),
                    at: 1_000 + i as i64,
                    event_id: Some(i as i64),
                    run_id: None,
                    outcome: outcome.to_string(),
                })
                .await
                .unwrap();
            }
            let all = s.trigger_fires("s1", 0).await.unwrap();
            assert_eq!(all.len(), 3);
            assert_eq!(all[0].at, 1_002, "newest first");
            assert_eq!(s.trigger_fires("s1", 1_001).await.unwrap().len(), 2);
            assert_eq!(s.prune_trigger_fires(1_002).await.unwrap(), 2);

            let (w, v) = wf("w-sys", "sys-pr-review");
            assert!(s.insert_workflow(&w, &v).await.unwrap());
            assert_eq!(s.workflow("w-sys").await.unwrap().unwrap().origin, "user");
            assert!(
                s.set_workflow_system("w-sys", "system", "sys:pr-review:acme/api", 3, false)
                    .await
                    .unwrap()
            );
            let got = s
                .workflow_by_system_key("sys:pr-review:acme/api")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (got.origin.as_str(), got.system_rev, got.customised),
                ("system", 3, false)
            );
            assert!(s.workflow_by_system_key("").await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn engine_decisions_insert_count_prune() {
        for (s, _d) in stores().await {
            for (i, at) in [1_000, 2_000, 3_000].into_iter().enumerate() {
                s.insert_engine_decision(&EngineDecision {
                    id: format!("d{i}"),
                    at,
                    run_id: if i == 2 { "r2".into() } else { "r1".into() },
                    outcome: "go".into(),
                    confidence: 0.91,
                    threshold: 0.8,
                    why: "chosen".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            let all = s.engine_decisions(None, 10).await.unwrap();
            assert_eq!(all.len(), 3);
            assert_eq!(all[0].id, "d2", "newest first");
            assert!((all[0].confidence - 0.91).abs() < 1e-9);
            assert_eq!(s.engine_decisions(Some("r1"), 10).await.unwrap().len(), 2);
            assert_eq!(s.count_engine_decisions(2_000).await.unwrap(), 2);
            assert_eq!(s.prune_engine_decisions(2_000).await.unwrap(), 1);
            assert_eq!(s.count_engine_decisions(0).await.unwrap(), 2);
        }
    }
}
