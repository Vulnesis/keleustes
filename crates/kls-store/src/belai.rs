//! Store: belai counterparts (docs/gaps/belai-counterparts.md). Session
//! extras (structured ask, accepts, git, per-model usage). Migrations 0101+.

use super::*;

/// One decision an open ask accepts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AskOption {
    pub id: String,
    pub label: String,
}

/// The ask a session waits on, as the host described it. Untrusted text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingAsk {
    pub id: String,
    /// "permission", "clarify", "mode_choice", "plan_review".
    pub kind: String,
    pub title: String,
    pub tool: String,
    pub options: Vec<AskOption>,
    /// clarify / mode_choice: webask's questionnaire JSON.
    pub questions_json: String,
    pub asked_at: i64,
    /// relay_review (#45): the handoff draft, `{"text","direction"}`.
    #[serde(default)]
    pub relay_json: String,
}

/// What remote input a session admits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Accepts {
    pub prompts: bool,
    pub answers: bool,
    pub controls: bool,
    pub shell: bool,
    pub controls_allowed: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GitFacts {
    pub commit: String,
    pub dirty: bool,
    pub ahead: i32,
    pub behind: i32,
    pub detached: bool,
}

/// One provider/model's cumulative usage in a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
}

/// The harness that ran a session (SessionUpsert.harness*).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionHarness {
    pub harness: String,
    pub version: String,
    pub session_id: String,
    /// "native" or "screen".
    pub transcript_source: String,
    /// How it ended (SessionEnd, OP-2); `None` while it runs.
    #[serde(default)]
    pub end: Option<HarnessEnd>,
}

/// A harness session's end facts: the process group is gone, no reply and
/// no change in the work tree, a typed prompt got no final turn, timing (ms).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessEnd {
    pub termination_verified: bool,
    pub empty_result: bool,
    pub adapter_error: bool,
    pub startup_ms: i64,
    pub ready_ms: i64,
    pub first_reply_ms: i64,
    pub wall_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionExtras {
    pub session_id: String,
    /// Set for a third-party harness session.
    pub harness: Option<SessionHarness>,
    pub pending_ask: Option<PendingAsk>,
    pub accepts: Option<Accepts>,
    pub git: Option<GitFacts>,
    pub usage: Vec<ModelUsage>,
    pub turns: i64,
    pub tool_calls: i64,
    pub updated_at: i64,
    /// File checkpoints the session can rewind to (#45), newest first.
    #[serde(default)]
    pub checkpoints: Vec<Checkpoint>,
    /// The plan file a plan-mode session wrote (#52).
    #[serde(default)]
    pub plan_sha256: String,
}

/// One file checkpoint a session listed (SessionUpsert.checkpoints).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Checkpoint {
    pub id: String,
    pub at_ms: i64,
    /// The prompt's first line (untrusted).
    pub label: String,
    pub entry_index: i64,
    pub entry_id: String,
    pub files: u32,
}

fn opt_json<T: Serialize>(v: &Option<T>) -> String {
    v.as_ref()
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_default()
}

fn from_json<T: for<'de> Deserialize<'de>>(s: &str) -> Option<T> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

impl Store {
    pub async fn session_extras(&self, session_id: &str) -> Result<Option<SessionExtras>> {
        let row = sqlx::query(self.sql(
            "SELECT session_id, pending_ask_json, accepts_json, git_json, usage_json, turns, \
             tool_calls, updated_at, harness_json, checkpoints_json, plan_sha256 FROM session_extras \
             WHERE session_id = ?",
        ))
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        let usage: String = r.try_get("usage_json")?;
        Ok(Some(SessionExtras {
            session_id: r.try_get("session_id")?,
            harness: from_json(&r.try_get::<String, _>("harness_json")?),
            pending_ask: from_json(&r.try_get::<String, _>("pending_ask_json")?),
            accepts: from_json(&r.try_get::<String, _>("accepts_json")?),
            git: from_json(&r.try_get::<String, _>("git_json")?),
            usage: from_json(&usage).unwrap_or_default(),
            turns: r.try_get("turns")?,
            tool_calls: r.try_get("tool_calls")?,
            updated_at: r.try_get("updated_at")?,
            checkpoints: from_json(&r.try_get::<String, _>("checkpoints_json")?)
                .unwrap_or_default(),
            plan_sha256: r.try_get("plan_sha256")?,
        }))
    }

    /// Replaces a session's extras and its per-model usage rows.
    pub async fn save_session_extras(&self, x: &SessionExtras) -> Result<()> {
        let now = now_ms();
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql(
            "INSERT INTO session_extras (session_id, pending_ask_json, accepts_json, git_json, \
             usage_json, turns, tool_calls, updated_at, harness_json, checkpoints_json, plan_sha256) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (session_id) DO UPDATE SET pending_ask_json = excluded.pending_ask_json, \
             harness_json = excluded.harness_json, \
             checkpoints_json = excluded.checkpoints_json, plan_sha256 = excluded.plan_sha256, \
             accepts_json = excluded.accepts_json, git_json = excluded.git_json, \
             usage_json = excluded.usage_json, turns = excluded.turns, \
             tool_calls = excluded.tool_calls, updated_at = excluded.updated_at",
        ))
        .bind(&x.session_id)
        .bind(opt_json(&x.pending_ask))
        .bind(opt_json(&x.accepts))
        .bind(opt_json(&x.git))
        .bind(serde_json::to_string(&x.usage)?)
        .bind(x.turns)
        .bind(x.tool_calls)
        .bind(now)
        .bind(opt_json(&x.harness))
        .bind(if x.checkpoints.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&x.checkpoints)?
        })
        .bind(&x.plan_sha256)
        .execute(&mut *tx)
        .await?;
        sqlx::query(self.sql("DELETE FROM session_usage WHERE session_id = ?"))
            .bind(&x.session_id)
            .execute(&mut *tx)
            .await?;
        for u in &x.usage {
            sqlx::query(self.sql(
                "INSERT INTO session_usage (session_id, provider, model, input_tokens, \
                 output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(&x.session_id)
            .bind(&u.provider)
            .bind(&u.model)
            .bind(u.input_tokens)
            .bind(u.output_tokens)
            .bind(u.cache_read_tokens)
            .bind(u.cache_write_tokens)
            .bind(u.cost_usd)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Per-model usage rows of a session.
    pub async fn session_usage(&self, session_id: &str) -> Result<Vec<ModelUsage>> {
        let rows = sqlx::query(self.sql(
            "SELECT provider, model, input_tokens, output_tokens, cache_read_tokens, \
             cache_write_tokens, cost_usd FROM session_usage WHERE session_id = ? \
             ORDER BY provider, model",
        ))
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ModelUsage {
                    provider: r.try_get("provider")?,
                    model: r.try_get("model")?,
                    input_tokens: r.try_get("input_tokens")?,
                    output_tokens: r.try_get("output_tokens")?,
                    cache_read_tokens: r.try_get("cache_read_tokens")?,
                    cache_write_tokens: r.try_get("cache_write_tokens")?,
                    cost_usd: r.try_get("cost_usd")?,
                })
            })
            .collect()
    }
}

/// Work one node asked for on another (WP-I).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeRequest {
    pub id: String,
    pub origin_node: String,
    pub origin_session: String,
    /// "user" or "agent".
    pub initiated_by: String,
    pub target_node: String,
    /// "session", "worker", "crew", "prompt".
    pub kind: String,
    pub cwd: String,
    pub spec_json: String,
    /// "awaiting_approval", "queued", "refused".
    pub state: String,
    pub reason: String,
    pub dispatch_id: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: String,
}

const NODE_REQUEST_COLS: &str = "id, origin_node, origin_session, initiated_by, target_node, kind, cwd, \
     spec_json, state, reason, dispatch_id, created_at, decided_at, decided_by";

fn node_request_from_row(r: &AnyRow) -> Result<NodeRequest> {
    Ok(NodeRequest {
        id: r.try_get("id")?,
        origin_node: r.try_get("origin_node")?,
        origin_session: r.try_get("origin_session")?,
        initiated_by: r.try_get("initiated_by")?,
        target_node: r.try_get("target_node")?,
        kind: r.try_get("kind")?,
        cwd: r.try_get("cwd")?,
        spec_json: r.try_get("spec_json")?,
        state: r.try_get("state")?,
        reason: r.try_get("reason")?,
        dispatch_id: r.try_get("dispatch_id")?,
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
        decided_by: r.try_get("decided_by")?,
    })
}

impl Store {
    pub async fn insert_node_request(&self, n: &NodeRequest) -> Result<()> {
        let q = format!(
            "INSERT INTO node_requests ({NODE_REQUEST_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        sqlx::query(self.sql(&q))
            .bind(&n.id)
            .bind(&n.origin_node)
            .bind(&n.origin_session)
            .bind(&n.initiated_by)
            .bind(&n.target_node)
            .bind(&n.kind)
            .bind(&n.cwd)
            .bind(&n.spec_json)
            .bind(&n.state)
            .bind(&n.reason)
            .bind(&n.dispatch_id)
            .bind(n.created_at)
            .bind(n.decided_at)
            .bind(&n.decided_by)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn node_request(&self, id: &str) -> Result<Option<NodeRequest>> {
        let q = format!("SELECT {NODE_REQUEST_COLS} FROM node_requests WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(node_request_from_row).transpose()
    }

    /// Requests, newest first; `state` empty matches every state.
    pub async fn node_requests(&self, state: &str, limit: i64) -> Result<Vec<NodeRequest>> {
        let q = format!(
            "SELECT {NODE_REQUEST_COLS} FROM node_requests WHERE (? = '' OR state = ?) \
             ORDER BY created_at DESC, id DESC LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(state)
            .bind(state)
            .bind(if limit > 0 { limit } else { 200 })
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(node_request_from_row).collect()
    }

    /// How many requests `origin` made since `since` (the rate limit).
    pub async fn count_node_requests_since(&self, origin: &str, since: i64) -> Result<i64> {
        let r = sqlx::query(self.sql(
            "SELECT COUNT(*) AS n FROM node_requests WHERE origin_node = ? AND created_at >= ?",
        ))
        .bind(origin)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(r.try_get("n")?)
    }

    /// Settles a request still in `from` state. False when it was not.
    #[allow(clippy::too_many_arguments)]
    pub async fn settle_node_request(
        &self,
        id: &str,
        from: &str,
        state: &str,
        reason: &str,
        dispatch_id: &str,
        target_node: &str,
        by: &str,
    ) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE node_requests SET state = ?, reason = ?, dispatch_id = ?, \
             target_node = CASE WHEN ? <> '' THEN ? ELSE target_node END, \
             decided_at = ?, decided_by = ? WHERE id = ? AND state = ?",
        ))
        .bind(state)
        .bind(reason)
        .bind(dispatch_id)
        .bind(target_node)
        .bind(target_node)
        .bind(now_ms())
        .bind(by)
        .bind(id)
        .bind(from)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }
}

impl Store {
    /// Replaces a node's live status (JSON as kls-core shapes it).
    pub async fn put_node_status(&self, node_id: &str, at: i64, json: &str) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO node_status (node_id, at, status_json, received_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (node_id) DO UPDATE SET at = excluded.at, status_json = excluded.status_json, \
             received_at = excluded.received_at",
        ))
        .bind(node_id)
        .bind(at)
        .bind(json)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node's latest status: (node id, received at, JSON).
    pub async fn node_statuses(&self) -> Result<Vec<(String, i64, String)>> {
        let rows = sqlx::query(
            self.sql("SELECT node_id, received_at, status_json FROM node_status ORDER BY node_id"),
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("node_id")?,
                    r.try_get("received_at")?,
                    r.try_get("status_json")?,
                ))
            })
            .collect()
    }

    /// Raises a worker's charged spend to (tokens, cost) and returns the
    /// new totals; a lower report (a repeat, a replay) changes nothing.
    pub async fn raise_worker_spend(
        &self,
        node_id: &str,
        worker_id: &str,
        tokens: i64,
        cost_micro_usd: i64,
    ) -> Result<(i64, i64)> {
        sqlx::query(self.sql(
            "INSERT INTO worker_spend (node_id, worker_id, tokens, cost_micro_usd, updated_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (node_id, worker_id) DO UPDATE SET \
             tokens = CASE WHEN excluded.tokens > worker_spend.tokens THEN excluded.tokens ELSE worker_spend.tokens END, \
             cost_micro_usd = CASE WHEN excluded.cost_micro_usd > worker_spend.cost_micro_usd \
                              THEN excluded.cost_micro_usd ELSE worker_spend.cost_micro_usd END, \
             updated_at = excluded.updated_at",
        ))
        .bind(node_id)
        .bind(worker_id)
        .bind(tokens.max(0))
        .bind(cost_micro_usd.max(0))
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        let r = sqlx::query(self.sql(
            "SELECT tokens, cost_micro_usd FROM worker_spend WHERE node_id = ? AND worker_id = ?",
        ))
        .bind(node_id)
        .bind(worker_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((r.try_get("tokens")?, r.try_get("cost_micro_usd")?))
    }
}

/// A host's attention request (WP-J).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attention {
    pub node_id: String,
    pub id: String,
    pub kind: String,
    pub reason: String,
    pub subject: String,
    pub state: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

fn attention_from_row(r: &AnyRow) -> Result<Attention> {
    Ok(Attention {
        node_id: r.try_get("node_id")?,
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        reason: r.try_get("reason")?,
        subject: r.try_get("subject")?,
        state: r.try_get("state")?,
        created_at: r.try_get("created_at")?,
        decided_at: r.try_get("decided_at")?,
    })
}

impl Store {
    /// Stores a new request; false when (node, id) already exists.
    pub async fn insert_attention(&self, a: &Attention) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "INSERT INTO attention_requests (node_id, id, kind, reason, subject, state, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (node_id, id) DO NOTHING",
        ))
        .bind(&a.node_id)
        .bind(&a.id)
        .bind(&a.kind)
        .bind(&a.reason)
        .bind(&a.subject)
        .bind(&a.state)
        .bind(a.created_at)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Requests in `state` (empty: all), newest first.
    pub async fn attention(&self, state: &str, limit: i64) -> Result<Vec<Attention>> {
        let rows = sqlx::query(self.sql(
            "SELECT node_id, id, kind, reason, subject, state, created_at, decided_at \
             FROM attention_requests WHERE (? = '' OR state = ?) ORDER BY created_at DESC LIMIT ?",
        ))
        .bind(state)
        .bind(state)
        .bind(if limit > 0 { limit } else { 200 })
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(attention_from_row).collect()
    }

    /// Decides an open request. False when it was not open.
    pub async fn decide_attention(&self, node_id: &str, id: &str, state: &str) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE attention_requests SET state = ?, decided_at = ? \
             WHERE node_id = ? AND id = ? AND state = 'open'",
        ))
        .bind(state)
        .bind(now_ms())
        .bind(node_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }
}

impl Store {
    /// The highest audit line index stored for (node, session or stream key),
    /// -1 for none (WP-C).
    pub async fn audit_head(&self, node_id: &str, key: &str) -> Result<i64> {
        let r = sqlx::query(self.sql(
            "SELECT COALESCE(MAX(idx), -1) AS n FROM audit_lines WHERE node_id = ? AND session_id = ?",
        ))
        .bind(node_id)
        .bind(key)
        .fetch_one(&self.pool)
        .await?;
        Ok(r.try_get("n")?)
    }
}

/// One scheduled worker on a node (WP-G).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostSchedule {
    pub node_id: String,
    pub id: String,
    pub profile: String,
    pub crew: String,
    pub cron: String,
    pub tz: String,
    pub cwd: String,
    pub enabled: bool,
    pub last_fired: i64,
    pub last_status: String,
    pub next_run: i64,
    pub version: i64,
    pub deleted: bool,
    pub meta_json: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub changed_at: i64,
}

const SCHEDULE_COLS: &str = "node_id, id, profile, crew, cron, tz, cwd, enabled, last_fired, last_status, \
     next_run, version, deleted, meta_json, created_at, updated_at, changed_at";

fn schedule_from_row(r: &AnyRow) -> Result<HostSchedule> {
    Ok(HostSchedule {
        node_id: r.try_get("node_id")?,
        id: r.try_get("id")?,
        profile: r.try_get("profile")?,
        crew: r.try_get("crew")?,
        cron: r.try_get("cron")?,
        tz: r.try_get("tz")?,
        cwd: r.try_get("cwd")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        last_fired: r.try_get("last_fired")?,
        last_status: r.try_get("last_status")?,
        next_run: r.try_get("next_run")?,
        version: r.try_get("version")?,
        deleted: r.try_get::<i64, _>("deleted")? != 0,
        meta_json: r.try_get("meta_json")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        changed_at: r.try_get("changed_at")?,
    })
}

impl Store {
    pub async fn host_schedule(&self, node_id: &str, id: &str) -> Result<Option<HostSchedule>> {
        let q = format!("SELECT {SCHEDULE_COLS} FROM host_schedules WHERE node_id = ? AND id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(node_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(schedule_from_row).transpose()
    }

    /// A node's schedules changed after `since` (server clock), oldest first,
    /// with the cursor to pass next and whether more remain.
    pub async fn host_schedules_since(
        &self,
        node_id: &str,
        since: i64,
        limit: i64,
    ) -> Result<(Vec<HostSchedule>, i64, bool)> {
        let limit = if limit <= 0 { 500 } else { limit.min(500) };
        let q = format!(
            "SELECT {SCHEDULE_COLS} FROM host_schedules WHERE node_id = ? AND changed_at > ? \
             ORDER BY changed_at, id LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(node_id)
            .bind(since)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?;
        let mut items: Vec<HostSchedule> =
            rows.iter().map(schedule_from_row).collect::<Result<_>>()?;
        let more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next = items.last().map(|s| s.changed_at).unwrap_or(since);
        Ok((items, next, more))
    }

    /// Every schedule (of one node when given), live ones only.
    pub async fn host_schedules(&self, node_id: Option<&str>) -> Result<Vec<HostSchedule>> {
        let q = format!(
            "SELECT {SCHEDULE_COLS} FROM host_schedules WHERE deleted = 0 AND (? = '' OR node_id = ?) \
             ORDER BY node_id, profile, id"
        );
        let n = node_id.unwrap_or("");
        let rows = sqlx::query(self.sql(&q))
            .bind(n)
            .bind(n)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(schedule_from_row).collect()
    }

    /// Writes a schedule as given, stamping `changed_at` past every earlier
    /// change of the node so a pull cursor never skips it.
    pub async fn put_host_schedule(&self, s: &HostSchedule) -> Result<HostSchedule> {
        let mut tx = self.begin_write().await?;
        let prev: i64 =
            sqlx::query(self.sql(
                "SELECT COALESCE(MAX(changed_at), 0) AS m FROM host_schedules WHERE node_id = ?",
            ))
            .bind(&s.node_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("m")?;
        let changed = now_ms().max(prev + 1);
        let q = format!(
            "INSERT INTO host_schedules ({SCHEDULE_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (node_id, id) DO UPDATE SET profile = excluded.profile, crew = excluded.crew, \
             cron = excluded.cron, tz = excluded.tz, cwd = excluded.cwd, enabled = excluded.enabled, \
             last_fired = excluded.last_fired, last_status = excluded.last_status, \
             next_run = excluded.next_run, version = excluded.version, deleted = excluded.deleted, \
             meta_json = excluded.meta_json, created_at = excluded.created_at, \
             updated_at = excluded.updated_at, changed_at = excluded.changed_at"
        );
        sqlx::query(self.sql(&q))
            .bind(&s.node_id)
            .bind(&s.id)
            .bind(&s.profile)
            .bind(&s.crew)
            .bind(&s.cron)
            .bind(&s.tz)
            .bind(&s.cwd)
            .bind(s.enabled as i64)
            .bind(s.last_fired)
            .bind(&s.last_status)
            .bind(s.next_run)
            .bind(s.version)
            .bind(s.deleted as i64)
            .bind(&s.meta_json)
            .bind(s.created_at)
            .bind(s.updated_at)
            .bind(changed)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(HostSchedule {
            changed_at: changed,
            ..s.clone()
        })
    }
}

/// A forge request a node's worker filed (WP-H).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ForgeRequest {
    pub node_id: String,
    pub request_id: String,
    pub item_id: String,
    pub worker_id: String,
    pub kind: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub branch: String,
    pub base_sha: String,
    pub head_sha: String,
    pub bundle_sha256: String,
    pub bundle_bytes: i64,
    pub failure: String,
    pub title: String,
    pub state: String,
    pub steer_event: String,
    pub steer_pr_url: String,
    pub dispatch_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

const FORGE_COLS: &str = "node_id, request_id, item_id, worker_id, kind, repo_owner, repo_name, branch, \
     base_sha, head_sha, bundle_sha256, bundle_bytes, failure, title, state, steer_event, steer_pr_url, \
     dispatch_id, created_at, updated_at";

fn forge_from_row(r: &AnyRow) -> Result<ForgeRequest> {
    Ok(ForgeRequest {
        node_id: r.try_get("node_id")?,
        request_id: r.try_get("request_id")?,
        item_id: r.try_get("item_id")?,
        worker_id: r.try_get("worker_id")?,
        kind: r.try_get("kind")?,
        repo_owner: r.try_get("repo_owner")?,
        repo_name: r.try_get("repo_name")?,
        branch: r.try_get("branch")?,
        base_sha: r.try_get("base_sha")?,
        head_sha: r.try_get("head_sha")?,
        bundle_sha256: r.try_get("bundle_sha256")?,
        bundle_bytes: r.try_get("bundle_bytes")?,
        failure: r.try_get("failure")?,
        title: r.try_get("title")?,
        state: r.try_get("state")?,
        steer_event: r.try_get("steer_event")?,
        steer_pr_url: r.try_get("steer_pr_url")?,
        dispatch_id: r.try_get("dispatch_id")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

impl Store {
    /// Stores a filed request; false when (node, request id) exists already.
    pub async fn insert_forge_request(&self, f: &ForgeRequest) -> Result<bool> {
        let q = format!(
            "INSERT INTO forge_requests ({FORGE_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (node_id, request_id) DO NOTHING"
        );
        let now = now_ms();
        let res = sqlx::query(self.sql(&q))
            .bind(&f.node_id)
            .bind(&f.request_id)
            .bind(&f.item_id)
            .bind(&f.worker_id)
            .bind(&f.kind)
            .bind(&f.repo_owner)
            .bind(&f.repo_name)
            .bind(&f.branch)
            .bind(&f.base_sha)
            .bind(&f.head_sha)
            .bind(&f.bundle_sha256)
            .bind(f.bundle_bytes)
            .bind(&f.failure)
            .bind(&f.title)
            .bind("filed")
            .bind("")
            .bind("")
            .bind("")
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn forge_request(
        &self,
        node_id: &str,
        request_id: &str,
    ) -> Result<Option<ForgeRequest>> {
        let q =
            format!("SELECT {FORGE_COLS} FROM forge_requests WHERE node_id = ? AND request_id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(node_id)
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(forge_from_row).transpose()
    }

    /// Requests in `state` (empty: all), newest first.
    pub async fn forge_requests(&self, state: &str, limit: i64) -> Result<Vec<ForgeRequest>> {
        let q = format!(
            "SELECT {FORGE_COLS} FROM forge_requests WHERE (? = '' OR state = ?) ORDER BY created_at DESC LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(state)
            .bind(state)
            .bind(if limit > 0 { limit } else { 200 })
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(forge_from_row).collect()
    }

    /// Records the coordinator's answer.
    pub async fn steer_forge_request(
        &self,
        node_id: &str,
        request_id: &str,
        event: &str,
        pr_url: &str,
        dispatch_id: &str,
    ) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE forge_requests SET state = 'steered', steer_event = ?, steer_pr_url = ?, dispatch_id = ?, \
             updated_at = ? WHERE node_id = ? AND request_id = ?",
        ))
        .bind(event)
        .bind(pr_url)
        .bind(dispatch_id)
        .bind(now_ms())
        .bind(node_id)
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }
}

/// One item a library scan found (WP-K).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanItem {
    pub dispatch_id: String,
    pub node_id: String,
    pub idx: i64,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub harness: String,
    pub verdict: String,
    pub created_at: i64,
}

impl Store {
    /// Replaces a scan's items.
    pub async fn put_scan(
        &self,
        dispatch_id: &str,
        node_id: &str,
        items: &[ScanItem],
    ) -> Result<()> {
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM library_scans WHERE dispatch_id = ?"))
            .bind(dispatch_id)
            .execute(&mut *tx)
            .await?;
        let now = now_ms();
        for (i, it) in items.iter().enumerate() {
            sqlx::query(self.sql(
                "INSERT INTO library_scans (dispatch_id, node_id, idx, kind, name, path, sha256, harness, verdict, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(dispatch_id)
            .bind(node_id)
            .bind(i as i64)
            .bind(&it.kind)
            .bind(&it.name)
            .bind(&it.path)
            .bind(&it.sha256)
            .bind(&it.harness)
            .bind(&it.verdict)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The latest scan of a node, in report order.
    pub async fn latest_scan(&self, node_id: &str) -> Result<Vec<ScanItem>> {
        let last: Option<String> = sqlx::query(self.sql(
            "SELECT dispatch_id FROM library_scans WHERE node_id = ? ORDER BY created_at DESC LIMIT 1",
        ))
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?
        .map(|r| r.try_get("dispatch_id"))
        .transpose()?;
        match last {
            Some(d) => self.scan_items(&d).await,
            None => Ok(Vec::new()),
        }
    }

    /// A scan's items in report order.
    pub async fn scan_items(&self, dispatch_id: &str) -> Result<Vec<ScanItem>> {
        let rows = sqlx::query(self.sql(
            "SELECT dispatch_id, node_id, idx, kind, name, path, sha256, harness, verdict, created_at \
             FROM library_scans WHERE dispatch_id = ? ORDER BY idx",
        ))
        .bind(dispatch_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ScanItem {
                    dispatch_id: r.try_get("dispatch_id")?,
                    node_id: r.try_get("node_id")?,
                    idx: r.try_get("idx")?,
                    kind: r.try_get("kind")?,
                    name: r.try_get("name")?,
                    path: r.try_get("path")?,
                    sha256: r.try_get("sha256")?,
                    harness: r.try_get("harness")?,
                    verdict: r.try_get("verdict")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        (s, dir)
    }

    #[tokio::test]
    async fn session_extras_round_trip() {
        let (s, _d) = store().await;
        assert!(s.session_extras("s1").await.unwrap().is_none());
        let mut x = SessionExtras {
            session_id: "s1".into(),
            harness: Some(SessionHarness {
                harness: "claude-code".into(),
                session_id: "uuid-1".into(),
                ..Default::default()
            }),
            pending_ask: Some(PendingAsk {
                id: "a1".into(),
                kind: "permission".into(),
                title: "permission: Bash ls".into(),
                tool: "Bash".into(),
                options: vec![AskOption {
                    id: "allow_once".into(),
                    label: "Allow once".into(),
                }],
                ..Default::default()
            }),
            accepts: Some(Accepts {
                answers: true,
                ..Default::default()
            }),
            git: Some(GitFacts {
                commit: "abc1234".into(),
                dirty: true,
                ahead: 2,
                ..Default::default()
            }),
            usage: vec![ModelUsage {
                provider: "anthropic".into(),
                model: "m".into(),
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: 0.25,
                ..Default::default()
            }],
            turns: 3,
            tool_calls: 4,
            updated_at: 0,
            checkpoints: vec![Checkpoint {
                id: "cp1".into(),
                label: "write the marker".into(),
                entry_id: "e1".into(),
                files: 2,
                ..Default::default()
            }],
            plan_sha256: "ab".repeat(32),
        };
        s.save_session_extras(&x).await.unwrap();
        let got = s.session_extras("s1").await.unwrap().unwrap();
        assert_eq!(got.checkpoints, x.checkpoints);
        assert_eq!(got.plan_sha256, x.plan_sha256);
        assert_eq!(got.pending_ask, x.pending_ask);
        assert_eq!(got.accepts, x.accepts);
        assert_eq!(got.git, x.git);
        assert_eq!(got.harness, x.harness);
        assert_eq!((got.turns, got.tool_calls), (3, 4));
        assert_eq!(s.session_usage("s1").await.unwrap(), x.usage);
        x.pending_ask = None;
        x.usage[0].input_tokens = 20;
        s.save_session_extras(&x).await.unwrap();
        let got = s.session_extras("s1").await.unwrap().unwrap();
        assert!(got.pending_ask.is_none());
        assert_eq!(s.session_usage("s1").await.unwrap()[0].input_tokens, 20);
    }

    #[tokio::test]
    async fn audit_head_of_a_stream() {
        let (s, _d) = store().await;
        assert_eq!(s.audit_head("n", "stream:a").await.unwrap(), -1);
        let l = |idx| crate::sessions::AuditLine {
            node_id: "n".into(),
            idx,
            ..Default::default()
        };
        s.append_audit_lines("n", "stream:a", &[l(0), l(1), l(1)])
            .await
            .unwrap();
        assert_eq!(s.audit_head("n", "stream:a").await.unwrap(), 1);
        assert_eq!(s.audit_head("m", "stream:a").await.unwrap(), -1);
    }

    #[tokio::test]
    async fn host_schedules_cursor() {
        let (s, _d) = store().await;
        let mk = |id: &str| HostSchedule {
            node_id: "n".into(),
            id: id.into(),
            profile: "belai:scout".into(),
            cron: "".into(),
            version: 1,
            ..Default::default()
        };
        let a = s.put_host_schedule(&mk("a")).await.unwrap();
        let b = s.put_host_schedule(&mk("b")).await.unwrap();
        assert!(b.changed_at > a.changed_at);
        let (items, next, more) = s.host_schedules_since("n", 0, 1).await.unwrap();
        assert_eq!((items.len(), more, next), (1, true, a.changed_at));
        let (items, _, more) = s.host_schedules_since("n", next, 10).await.unwrap();
        assert_eq!((items[0].id.as_str(), more), ("b", false));
        assert_eq!(s.host_schedules(Some("n")).await.unwrap().len(), 2);
        assert!(
            s.host_schedules_since("m", 0, 10)
                .await
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[tokio::test]
    async fn forge_requests_round_trip() {
        let (s, _d) = store().await;
        let f = ForgeRequest {
            node_id: "n".into(),
            request_id: "0123456789abcdef".into(),
            kind: "pull_request".into(),
            ..Default::default()
        };
        assert!(s.insert_forge_request(&f).await.unwrap());
        assert!(!s.insert_forge_request(&f).await.unwrap());
        assert_eq!(s.forge_requests("filed", 0).await.unwrap().len(), 1);
        assert!(
            s.steer_forge_request("n", &f.request_id, "took_over", "https://x/pr/1", "d1")
                .await
                .unwrap()
        );
        let got = s.forge_request("n", &f.request_id).await.unwrap().unwrap();
        assert_eq!(
            (got.state.as_str(), got.steer_event.as_str()),
            ("steered", "took_over")
        );
    }

    #[tokio::test]
    async fn scans_replace_and_list() {
        let (s, _d) = store().await;
        let it = |n: &str| ScanItem {
            kind: "skill".into(),
            name: n.into(),
            ..Default::default()
        };
        s.put_scan("d1", "n", &[it("a"), it("b")]).await.unwrap();
        s.put_scan("d1", "n", &[it("c")]).await.unwrap();
        let got = s.scan_items("d1").await.unwrap();
        assert_eq!((got.len(), got[0].name.as_str()), (1, "c"));
        assert_eq!(s.latest_scan("n").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn attention_round_trip() {
        let (s, _d) = store().await;
        let a = Attention {
            node_id: "n".into(),
            id: "r1".into(),
            kind: "gh_login".into(),
            reason: "reauth_needed".into(),
            state: "open".into(),
            created_at: 1,
            ..Default::default()
        };
        assert!(s.insert_attention(&a).await.unwrap());
        assert!(!s.insert_attention(&a).await.unwrap());
        assert_eq!(s.attention("open", 0).await.unwrap(), vec![a]);
        assert!(s.decide_attention("n", "r1", "approved").await.unwrap());
        assert!(!s.decide_attention("n", "r1", "declined").await.unwrap());
        assert_eq!(s.attention("approved", 0).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn node_status_and_worker_spend() {
        let (s, _d) = store().await;
        s.put_node_status("n1", 5, "{}").await.unwrap();
        s.put_node_status("n1", 6, "{\"a\":1}").await.unwrap();
        let all = s.node_statuses().await.unwrap();
        assert_eq!((all.len(), all[0].2.as_str()), (1, "{\"a\":1}"));
        assert_eq!(
            s.raise_worker_spend("n1", "w", 10, 500).await.unwrap(),
            (10, 500)
        );
        assert_eq!(
            s.raise_worker_spend("n1", "w", 8, 400).await.unwrap(),
            (10, 500)
        );
        assert_eq!(
            s.raise_worker_spend("n1", "w", 20, 900).await.unwrap(),
            (20, 900)
        );
    }

    #[tokio::test]
    async fn node_requests_round_trip() {
        let (s, _d) = store().await;
        let r = NodeRequest {
            id: "r1".into(),
            origin_node: "a".into(),
            initiated_by: "user".into(),
            kind: "session".into(),
            state: "awaiting_approval".into(),
            created_at: 10,
            ..Default::default()
        };
        s.insert_node_request(&r).await.unwrap();
        assert_eq!(s.node_request("r1").await.unwrap().unwrap(), r);
        assert_eq!(s.count_node_requests_since("a", 5).await.unwrap(), 1);
        assert_eq!(s.count_node_requests_since("a", 11).await.unwrap(), 0);
        assert_eq!(
            s.node_requests("awaiting_approval", 0).await.unwrap().len(),
            1
        );
        assert!(
            s.settle_node_request("r1", "awaiting_approval", "queued", "", "d1", "b", "owner")
                .await
                .unwrap()
        );
        assert!(
            !s.settle_node_request("r1", "awaiting_approval", "refused", "", "", "", "owner")
                .await
                .unwrap()
        );
        let got = s.node_request("r1").await.unwrap().unwrap();
        assert_eq!(
            (
                got.state.as_str(),
                got.dispatch_id.as_str(),
                got.target_node.as_str()
            ),
            ("queued", "d1", "b")
        );
        assert!(s.node_requests("refused", 0).await.unwrap().is_empty());
    }
}
