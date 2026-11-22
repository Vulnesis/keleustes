//! Store: M1 sessions: sessions, entries, session inbox, dispatches, jobs,
//! and raw host audit lines. Migration 0002.

use super::*;

/// A belai session mirrored from a node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub node_id: String,
    pub title: String,
    pub cwd: String,
    pub repo: String,
    pub branch: String,
    pub provider: String,
    pub model: String,
    /// "tui", "worker", "rc", "acp", "headless".
    pub kind: String,
    /// "live", "idle", "waiting", "ended".
    pub state: String,
    pub mode: String,
    pub agent: String,
    pub dispatch_id: String,
    pub card_id: String,
    pub run_id: String,
    pub started_at: i64,
    /// Host clock of the last change.
    pub updated_at: i64,
    /// Server clock of the last upsert or append.
    pub seen_at: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
    pub pending_question_id: String,
    pub pending_question: String,
    pub entry_count: i64,
    pub next_index: i64,
    pub ended_at: Option<i64>,
    pub end_reason: String,
    pub created_at: i64,
}

impl Session {
    /// Not ended (the host may still have gone quiet; see `seen_at`).
    pub fn is_open(&self) -> bool {
        self.ended_at.is_none() && self.state != "ended"
    }
}

/// One transcript entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub idx: i64,
    /// "user", "assistant", "tool", "tool_result", "system", "a2a", "shell", "error".
    pub role: String,
    pub text: String,
    pub at: i64,
    pub meta_json: String,
}

/// A prompt, answer or command waiting for a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
    pub session_id: String,
    pub seq: i64,
    pub id: String,
    /// "prompt", "answer", "command".
    pub kind: String,
    /// prompt: {"text"}; answer: {"questionId","text"}; command: {"name","args"}.
    pub payload_json: String,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    pub acked_at: Option<i64>,
    /// "pending", "delivered", "done", "failed", "refused".
    pub status: String,
    pub detail: String,
}

/// Work sent to a node on its Link.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dispatch {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub spec_json: String,
    /// "queued", "delivered", "started", "failed".
    pub status: String,
    pub session_id: String,
    pub error: String,
    pub outbox_seq: i64,
    pub card_id: String,
    pub run_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub delivered_at: Option<i64>,
    pub resolved_at: Option<i64>,
}

/// A job a node runs on Keleustes' behalf.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub node_id: String,
    pub kind: String,
    pub payload_json: String,
    /// "queued", "ok", "failed", "refused", "timeout".
    pub status: String,
    pub output_json: String,
    pub error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deadline_at: i64,
    pub resolved_at: Option<i64>,
}

/// A raw host audit line (Sessions.Audit).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditLine {
    pub node_id: String,
    pub session_id: String,
    pub idx: i64,
    pub at: i64,
    pub kind: String,
    pub json: String,
    pub received_at: i64,
}

/// Filter for [`Store::sessions`]. Empty strings and `None` match all.
#[derive(Clone, Debug, Default)]
pub struct SessionQuery {
    pub node_id: Option<String>,
    pub card_id: Option<String>,
    pub run_id: Option<String>,
    /// Only sessions that have not ended.
    pub open_only: bool,
    /// Case-insensitive substring of title, cwd, repo, agent or model.
    pub text: Option<String>,
    /// Only sessions seen before this server time (paging).
    pub before_seen: Option<i64>,
    /// 0 means 200.
    pub limit: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpsertOutcome {
    Created,
    Updated,
    /// The session belongs to another node; nothing changed.
    NotOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendOutcome {
    /// Entries actually stored (duplicates are skipped).
    pub inserted: u64,
    /// The next index the server expects.
    pub next_index: i64,
}

const SESSION_COLS: &str = "id, node_id, title, cwd, repo, branch, provider, model, kind, state, \
     mode, agent, dispatch_id, card_id, run_id, started_at, updated_at, seen_at, input_tokens, \
     output_tokens, cost_usd, pending_question_id, pending_question, entry_count, next_index, \
     ended_at, end_reason, created_at";

const INBOX_COLS: &str = "session_id, seq, id, kind, payload_json, created_at, delivered_at, \
     acked_at, status, detail";

const DISPATCH_COLS: &str = "id, node_id, kind, spec_json, status, session_id, error, outbox_seq, \
     card_id, run_id, created_at, updated_at, delivered_at, resolved_at";

const JOB_COLS: &str = "id, node_id, kind, payload_json, status, output_json, error, created_at, \
     updated_at, deadline_at, resolved_at";

fn session_from_row(r: &AnyRow) -> Result<Session> {
    Ok(Session {
        id: r.try_get("id")?,
        node_id: r.try_get("node_id")?,
        title: r.try_get("title")?,
        cwd: r.try_get("cwd")?,
        repo: r.try_get("repo")?,
        branch: r.try_get("branch")?,
        provider: r.try_get("provider")?,
        model: r.try_get("model")?,
        kind: r.try_get("kind")?,
        state: r.try_get("state")?,
        mode: r.try_get("mode")?,
        agent: r.try_get("agent")?,
        dispatch_id: r.try_get("dispatch_id")?,
        card_id: r.try_get("card_id")?,
        run_id: r.try_get("run_id")?,
        started_at: r.try_get("started_at")?,
        updated_at: r.try_get("updated_at")?,
        seen_at: r.try_get("seen_at")?,
        input_tokens: r.try_get("input_tokens")?,
        output_tokens: r.try_get("output_tokens")?,
        cost_usd: r.try_get("cost_usd")?,
        pending_question_id: r.try_get("pending_question_id")?,
        pending_question: r.try_get("pending_question")?,
        entry_count: r.try_get("entry_count")?,
        next_index: r.try_get("next_index")?,
        ended_at: r.try_get("ended_at")?,
        end_reason: r.try_get("end_reason")?,
        created_at: r.try_get("created_at")?,
    })
}

fn inbox_from_row(r: &AnyRow) -> Result<InboxItem> {
    Ok(InboxItem {
        session_id: r.try_get("session_id")?,
        seq: r.try_get("seq")?,
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        payload_json: r.try_get("payload_json")?,
        created_at: r.try_get("created_at")?,
        delivered_at: r.try_get("delivered_at")?,
        acked_at: r.try_get("acked_at")?,
        status: r.try_get("status")?,
        detail: r.try_get("detail")?,
    })
}

fn dispatch_from_row(r: &AnyRow) -> Result<Dispatch> {
    Ok(Dispatch {
        id: r.try_get("id")?,
        node_id: r.try_get("node_id")?,
        kind: r.try_get("kind")?,
        spec_json: r.try_get("spec_json")?,
        status: r.try_get("status")?,
        session_id: r.try_get("session_id")?,
        error: r.try_get("error")?,
        outbox_seq: r.try_get("outbox_seq")?,
        card_id: r.try_get("card_id")?,
        run_id: r.try_get("run_id")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        delivered_at: r.try_get("delivered_at")?,
        resolved_at: r.try_get("resolved_at")?,
    })
}

fn job_from_row(r: &AnyRow) -> Result<Job> {
    Ok(Job {
        id: r.try_get("id")?,
        node_id: r.try_get("node_id")?,
        kind: r.try_get("kind")?,
        payload_json: r.try_get("payload_json")?,
        status: r.try_get("status")?,
        output_json: r.try_get("output_json")?,
        error: r.try_get("error")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        deadline_at: r.try_get("deadline_at")?,
        resolved_at: r.try_get("resolved_at")?,
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

/// String columns an empty upsert value leaves as stored.
const KEEP_IF_EMPTY: [&str; 13] = [
    "title",
    "cwd",
    "repo",
    "branch",
    "provider",
    "model",
    "kind",
    "state",
    "mode",
    "agent",
    "dispatch_id",
    "card_id",
    "run_id",
];

impl Store {
    // ---- sessions ----------------------------------------------------------

    /// Creates or updates a session from a host upsert. Empty strings and
    /// zero usage keep the stored value; the pending question is always
    /// replaced (empty clears it). A state other than "ended" reopens an
    /// ended session (a resume). `seen_at` is set to now. A session owned
    /// by another node is left alone.
    pub async fn upsert_session(&self, s: &Session) -> Result<UpsertOutcome> {
        let existed = self.session(&s.id).await?.is_some();
        let now = now_ms();
        let mut set = String::new();
        for c in KEEP_IF_EMPTY {
            set.push_str(&format!(
                "{c} = CASE WHEN excluded.{c} <> '' THEN excluded.{c} ELSE sessions.{c} END, "
            ));
        }
        for c in ["input_tokens", "output_tokens", "cost_usd"] {
            set.push_str(&format!(
                "{c} = CASE WHEN excluded.{c} > 0 THEN excluded.{c} ELSE sessions.{c} END, "
            ));
        }
        let q = format!(
            "INSERT INTO sessions (id, node_id, title, cwd, repo, branch, provider, model, kind, state, \
             mode, agent, dispatch_id, card_id, run_id, started_at, updated_at, seen_at, input_tokens, \
             output_tokens, cost_usd, pending_question_id, pending_question, ended_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET {set}\
             started_at = CASE WHEN sessions.started_at = 0 THEN excluded.started_at ELSE sessions.started_at END, \
             updated_at = CASE WHEN excluded.updated_at > sessions.updated_at THEN excluded.updated_at ELSE sessions.updated_at END, \
             seen_at = excluded.seen_at, \
             pending_question_id = excluded.pending_question_id, \
             pending_question = excluded.pending_question, \
             ended_at = CASE WHEN excluded.state = 'ended' THEN COALESCE(sessions.ended_at, excluded.ended_at) \
                             WHEN excluded.state <> '' THEN NULL ELSE sessions.ended_at END, \
             end_reason = CASE WHEN excluded.state <> '' AND excluded.state <> 'ended' THEN '' ELSE sessions.end_reason END \
             WHERE sessions.node_id = excluded.node_id"
        );
        let ended_at = (s.state == "ended").then_some(now);
        let res = sqlx::query(self.sql(&q))
            .bind(&s.id)
            .bind(&s.node_id)
            .bind(&s.title)
            .bind(&s.cwd)
            .bind(&s.repo)
            .bind(&s.branch)
            .bind(&s.provider)
            .bind(&s.model)
            .bind(&s.kind)
            .bind(&s.state)
            .bind(&s.mode)
            .bind(&s.agent)
            .bind(&s.dispatch_id)
            .bind(&s.card_id)
            .bind(&s.run_id)
            .bind(s.started_at)
            .bind(s.updated_at)
            .bind(now)
            .bind(s.input_tokens)
            .bind(s.output_tokens)
            .bind(s.cost_usd)
            .bind(&s.pending_question_id)
            .bind(&s.pending_question)
            .bind(ended_at)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(match (res.rows_affected(), existed) {
            (0, _) => UpsertOutcome::NotOwner,
            (_, false) => UpsertOutcome::Created,
            (_, true) => UpsertOutcome::Updated,
        })
    }

    pub async fn session(&self, id: &str) -> Result<Option<Session>> {
        let q = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(session_from_row).transpose()
    }

    /// Sessions, most recently seen first.
    pub async fn sessions(&self, f: &SessionQuery) -> Result<Vec<Session>> {
        let mut q = format!("SELECT {SESSION_COLS} FROM sessions WHERE 1 = 1");
        let mut binds: Vec<String> = Vec::new();
        for (col, v) in [
            ("node_id", &f.node_id),
            ("card_id", &f.card_id),
            ("run_id", &f.run_id),
        ] {
            if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
                q.push_str(&format!(" AND {col} = ?"));
                binds.push(v.clone());
            }
        }
        if f.open_only {
            q.push_str(" AND ended_at IS NULL AND state <> 'ended'");
        }
        if let Some(t) = f.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            q.push_str(
                " AND (LOWER(title) LIKE ? OR LOWER(cwd) LIKE ? OR LOWER(repo) LIKE ? \
                 OR LOWER(agent) LIKE ? OR LOWER(model) LIKE ?)",
            );
            let pat = format!("%{}%", t.to_lowercase());
            binds.extend(std::iter::repeat_n(pat, 5));
        }
        if f.before_seen.is_some() {
            q.push_str(" AND seen_at < ?");
        }
        q.push_str(" ORDER BY seen_at DESC, id LIMIT ?");
        let sql = self.sql(&q);
        let mut query = sqlx::query(sql);
        for b in binds {
            query = query.bind(b);
        }
        if let Some(b) = f.before_seen {
            query = query.bind(b);
        }
        let limit = if f.limit > 0 { f.limit } else { 200 };
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(session_from_row).collect()
    }

    /// Marks a session ended. False when it is unknown or already ended.
    pub async fn end_session(&self, id: &str, ended_at: i64, reason: &str) -> Result<bool> {
        let at = if ended_at > 0 { ended_at } else { now_ms() };
        let res = sqlx::query(self.sql(
            "UPDATE sessions SET ended_at = ?, end_reason = ?, state = 'ended', \
             pending_question_id = '', pending_question = '', seen_at = ? \
             WHERE id = ? AND ended_at IS NULL",
        ))
        .bind(at)
        .bind(reason)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- entries -----------------------------------------------------------

    /// Stores transcript entries, skipping indexes already stored, and
    /// returns the next index the server expects. The session must exist.
    pub async fn append_entries(
        &self,
        session_id: &str,
        entries: &[Entry],
    ) -> Result<AppendOutcome> {
        let mut tx = self.begin_write().await?;
        let mut inserted = 0u64;
        let mut high = -1i64;
        for e in entries {
            if e.idx < 0 {
                bail!("entry index must not be negative");
            }
            let r = sqlx::query(self.sql(
                "INSERT INTO session_entries (session_id, idx, role, text, at, meta_json) \
                 VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (session_id, idx) DO NOTHING",
            ))
            .bind(session_id)
            .bind(e.idx)
            .bind(&e.role)
            .bind(&e.text)
            .bind(e.at)
            .bind(&e.meta_json)
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected();
            high = high.max(e.idx);
        }
        let next = high + 1;
        sqlx::query(self.sql(
            "UPDATE sessions SET next_index = CASE WHEN next_index < ? THEN ? ELSE next_index END, \
             entry_count = entry_count + ?, seen_at = ? WHERE id = ?",
        ))
        .bind(next)
        .bind(next)
        .bind(inserted as i64)
        .bind(now_ms())
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(self.sql("SELECT next_index FROM sessions WHERE id = ?"))
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("unknown session")?;
        tx.commit().await?;
        Ok(AppendOutcome {
            inserted,
            next_index: row.try_get("next_index")?,
        })
    }

    /// Entries with index >= `from`, in order, at most `limit` (0 = all).
    pub async fn entries(&self, session_id: &str, from: i64, limit: i64) -> Result<Vec<Entry>> {
        let rows = sqlx::query(self.sql(
            "SELECT idx, role, text, at, meta_json FROM session_entries \
             WHERE session_id = ? AND idx >= ? ORDER BY idx LIMIT ?",
        ))
        .bind(session_id)
        .bind(from)
        .bind(if limit > 0 { limit } else { i64::MAX })
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Entry {
                    idx: r.try_get("idx")?,
                    role: r.try_get("role")?,
                    text: r.try_get("text")?,
                    at: r.try_get("at")?,
                    meta_json: r.try_get("meta_json")?,
                })
            })
            .collect()
    }

    // ---- inbox -------------------------------------------------------------

    /// Queues an inbox item (`id` is a fresh uuid v7) with the session's
    /// next seq.
    pub async fn inbox_push(
        &self,
        session_id: &str,
        id: &str,
        kind: &str,
        payload_json: &str,
    ) -> Result<InboxItem> {
        let now = now_ms();
        let q = format!(
            "INSERT INTO session_inbox (session_id, seq, id, kind, payload_json, created_at) \
             SELECT ?, COALESCE(MAX(seq), 0) + 1, ?, ?, ?, ? FROM session_inbox WHERE session_id = ? \
             RETURNING {INBOX_COLS}"
        );
        let mut attempt = 0;
        loop {
            let r = sqlx::query(self.sql(&q))
                .bind(session_id)
                .bind(id)
                .bind(kind)
                .bind(payload_json)
                .bind(now)
                .bind(session_id)
                .fetch_one(&self.pool)
                .await;
            match r {
                Ok(row) => return inbox_from_row(&row),
                Err(e) if is_unique_violation(&e) && attempt < 8 => attempt += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Unacked items with seq > `after`, in order.
    pub async fn inbox_unacked_after(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<Vec<InboxItem>> {
        let q = format!(
            "SELECT {INBOX_COLS} FROM session_inbox \
             WHERE session_id = ? AND seq > ? AND acked_at IS NULL ORDER BY seq"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(session_id)
            .bind(after)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(inbox_from_row).collect()
    }

    /// The session's newest `limit` items, oldest first.
    pub async fn inbox_items(&self, session_id: &str, limit: i64) -> Result<Vec<InboxItem>> {
        let q = format!(
            "SELECT {INBOX_COLS} FROM session_inbox WHERE session_id = ? ORDER BY seq DESC LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(session_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut out: Vec<InboxItem> = rows.iter().map(inbox_from_row).collect::<Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    pub async fn inbox_item(&self, id: &str) -> Result<Option<InboxItem>> {
        let q = format!("SELECT {INBOX_COLS} FROM session_inbox WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(inbox_from_row).transpose()
    }

    /// First delivery of a pending item. False when already delivered or acked.
    pub async fn inbox_mark_delivered(&self, session_id: &str, seq: i64) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE session_inbox SET delivered_at = ?, status = 'delivered' \
             WHERE session_id = ? AND seq = ? AND acked_at IS NULL AND delivered_at IS NULL",
        ))
        .bind(now_ms())
        .bind(session_id)
        .bind(seq)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Acks an item once; `None` when it is unknown or already acked.
    pub async fn inbox_ack(
        &self,
        session_id: &str,
        item_id: &str,
        status: &str,
        detail: &str,
    ) -> Result<Option<InboxItem>> {
        let q = format!(
            "UPDATE session_inbox SET acked_at = ?, status = ?, detail = ?, \
             delivered_at = COALESCE(delivered_at, ?) \
             WHERE session_id = ? AND id = ? AND acked_at IS NULL RETURNING {INBOX_COLS}"
        );
        let now = now_ms();
        let row = sqlx::query(self.sql(&q))
            .bind(now)
            .bind(status)
            .bind(detail)
            .bind(now)
            .bind(session_id)
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(inbox_from_row).transpose()
    }

    // ---- dispatches --------------------------------------------------------

    /// Inserts a dispatch row (status as given, normally "queued").
    pub async fn insert_dispatch(&self, d: &Dispatch) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO dispatches (id, node_id, kind, spec_json, status, session_id, error, \
             outbox_seq, card_id, run_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&d.id)
        .bind(&d.node_id)
        .bind(&d.kind)
        .bind(&d.spec_json)
        .bind(if d.status.is_empty() {
            "queued"
        } else {
            &d.status
        })
        .bind(&d.session_id)
        .bind(&d.error)
        .bind(d.outbox_seq)
        .bind(&d.card_id)
        .bind(&d.run_id)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the outbox seq that carries the dispatch.
    pub async fn set_dispatch_outbox_seq(&self, id: &str, seq: i64) -> Result<()> {
        sqlx::query(self.sql("UPDATE dispatches SET outbox_seq = ?, updated_at = ? WHERE id = ?"))
            .bind(seq)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn dispatch(&self, id: &str) -> Result<Option<Dispatch>> {
        let q = format!("SELECT {DISPATCH_COLS} FROM dispatches WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(dispatch_from_row).transpose()
    }

    /// Newest first, optionally for one node.
    pub async fn dispatches(&self, node_id: Option<&str>, limit: i64) -> Result<Vec<Dispatch>> {
        let rows = match node_id {
            Some(n) => {
                let q = format!(
                    "SELECT {DISPATCH_COLS} FROM dispatches WHERE node_id = ? \
                     ORDER BY created_at DESC, id DESC LIMIT ?"
                );
                sqlx::query(self.sql(&q))
                    .bind(n)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let q = format!(
                    "SELECT {DISPATCH_COLS} FROM dispatches ORDER BY created_at DESC, id DESC LIMIT ?"
                );
                sqlx::query(self.sql(&q))
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        rows.iter().map(dispatch_from_row).collect()
    }

    pub async fn count_queued_dispatches(&self) -> Result<i64> {
        let row = sqlx::query(self.sql(
            "SELECT COUNT(*) AS n FROM dispatches WHERE status = 'queued' AND outbox_seq > 0",
        ))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// Queued dispatches whose outbox frame the node has acked become
    /// "delivered". Returns the rows that changed.
    pub async fn mark_dispatches_delivered(&self) -> Result<Vec<Dispatch>> {
        let now = now_ms();
        let q = format!(
            "UPDATE dispatches SET status = 'delivered', delivered_at = ?, updated_at = ? \
             WHERE status = 'queued' AND outbox_seq > 0 \
             AND outbox_seq <= (SELECT server_acked FROM nodes WHERE nodes.id = dispatches.node_id) \
             RETURNING {DISPATCH_COLS}"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(now)
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(dispatch_from_row).collect()
    }

    /// Records a dispatch outcome ("started" or "failed") once. `None` when
    /// it is unknown or already resolved.
    pub async fn resolve_dispatch(
        &self,
        id: &str,
        status: &str,
        session_id: &str,
        error: &str,
    ) -> Result<Option<Dispatch>> {
        let now = now_ms();
        let q = format!(
            "UPDATE dispatches SET status = ?, \
             session_id = CASE WHEN ? <> '' THEN ? ELSE session_id END, error = ?, \
             updated_at = ?, resolved_at = ?, delivered_at = COALESCE(delivered_at, ?) \
             WHERE id = ? AND resolved_at IS NULL RETURNING {DISPATCH_COLS}"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(status)
            .bind(session_id)
            .bind(session_id)
            .bind(error)
            .bind(now)
            .bind(now)
            .bind(now)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(dispatch_from_row).transpose()
    }

    // ---- jobs --------------------------------------------------------------

    pub async fn insert_job(
        &self,
        id: &str,
        node_id: &str,
        kind: &str,
        payload_json: &str,
        deadline_at: i64,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO jobs (id, node_id, kind, payload_json, status, created_at, updated_at, deadline_at) \
             VALUES (?, ?, ?, ?, 'queued', ?, ?, ?)",
        ))
        .bind(id)
        .bind(node_id)
        .bind(kind)
        .bind(payload_json)
        .bind(now)
        .bind(now)
        .bind(deadline_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn job(&self, id: &str) -> Result<Option<Job>> {
        let q = format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(job_from_row).transpose()
    }

    /// Newest first, optionally for one node.
    pub async fn jobs(&self, node_id: Option<&str>, limit: i64) -> Result<Vec<Job>> {
        let rows = match node_id {
            Some(n) => {
                let q = format!(
                    "SELECT {JOB_COLS} FROM jobs WHERE node_id = ? ORDER BY created_at DESC, id DESC LIMIT ?"
                );
                sqlx::query(self.sql(&q))
                    .bind(n)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let q = format!(
                    "SELECT {JOB_COLS} FROM jobs ORDER BY created_at DESC, id DESC LIMIT ?"
                );
                sqlx::query(self.sql(&q))
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        rows.iter().map(job_from_row).collect()
    }

    /// Records a job outcome once. `None` when unknown or already resolved.
    pub async fn resolve_job(
        &self,
        id: &str,
        status: &str,
        output_json: &str,
        error: &str,
    ) -> Result<Option<Job>> {
        let now = now_ms();
        let q = format!(
            "UPDATE jobs SET status = ?, output_json = ?, error = ?, updated_at = ?, resolved_at = ? \
             WHERE id = ? AND resolved_at IS NULL RETURNING {JOB_COLS}"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(status)
            .bind(output_json)
            .bind(error)
            .bind(now)
            .bind(now)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(job_from_row).transpose()
    }

    /// Unresolved job count per node.
    pub async fn open_jobs_by_node(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(self.sql(
            "SELECT node_id, COUNT(*) AS n FROM jobs WHERE resolved_at IS NULL GROUP BY node_id",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("node_id")?, r.try_get("n")?)))
            .collect()
    }

    // ---- audit lines -------------------------------------------------------

    /// Stores raw host audit lines, skipping indexes already stored.
    /// Returns how many were new.
    pub async fn append_audit_lines(
        &self,
        node_id: &str,
        session_id: &str,
        lines: &[AuditLine],
    ) -> Result<u64> {
        let mut tx = self.begin_write().await?;
        let now = now_ms();
        let mut n = 0;
        for l in lines {
            n += sqlx::query(self.sql(
                "INSERT INTO audit_lines (node_id, session_id, idx, at, kind, json, received_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (node_id, session_id, idx) DO NOTHING",
            ))
            .bind(node_id)
            .bind(session_id)
            .bind(l.idx)
            .bind(l.at)
            .bind(&l.kind)
            .bind(&l.json)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        tx.commit().await?;
        Ok(n)
    }

    /// A session's audit lines with index >= `from`, in order.
    pub async fn audit_lines(
        &self,
        node_id: &str,
        session_id: &str,
        from: i64,
        limit: i64,
    ) -> Result<Vec<AuditLine>> {
        let rows = sqlx::query(self.sql(
            "SELECT node_id, session_id, idx, at, kind, json, received_at FROM audit_lines \
             WHERE node_id = ? AND session_id = ? AND idx >= ? ORDER BY idx LIMIT ?",
        ))
        .bind(node_id)
        .bind(session_id)
        .bind(from)
        .bind(if limit > 0 { limit } else { i64::MAX })
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(AuditLine {
                    node_id: r.try_get("node_id")?,
                    session_id: r.try_get("session_id")?,
                    idx: r.try_get("idx")?,
                    at: r.try_get("at")?,
                    kind: r.try_get("kind")?,
                    json: r.try_get("json")?,
                    received_at: r.try_get("received_at")?,
                })
            })
            .collect()
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
                "sessions",
                "session_entries",
                "session_inbox",
                "dispatches",
                "jobs",
                "audit_lines",
                "node_outbox",
                "nodes",
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

    fn sess(id: &str, node: &str) -> Session {
        Session {
            id: id.into(),
            node_id: node.into(),
            title: "fix the build".into(),
            cwd: "/work/api".into(),
            model: "opus".into(),
            kind: "rc".into(),
            state: "live".into(),
            started_at: 100,
            updated_at: 100,
            input_tokens: 10,
            ..Default::default()
        }
    }

    fn entry(idx: i64, text: &str) -> Entry {
        Entry {
            idx,
            role: "assistant".into(),
            text: text.into(),
            at: idx,
            meta_json: String::new(),
        }
    }

    #[tokio::test]
    async fn upsert_keeps_and_owns() {
        for (s, _d) in stores().await {
            assert_eq!(
                s.upsert_session(&sess("s1", "n1")).await.unwrap(),
                UpsertOutcome::Created
            );
            // Empty fields keep the stored value; the question is replaced.
            let upd = Session {
                id: "s1".into(),
                node_id: "n1".into(),
                updated_at: 200,
                output_tokens: 5,
                pending_question_id: "q1".into(),
                pending_question: "Proceed?".into(),
                state: "waiting".into(),
                ..Default::default()
            };
            assert_eq!(
                s.upsert_session(&upd).await.unwrap(),
                UpsertOutcome::Updated
            );
            let got = s.session("s1").await.unwrap().unwrap();
            assert_eq!(got.title, "fix the build");
            assert_eq!(got.state, "waiting");
            assert_eq!(got.input_tokens, 10);
            assert_eq!(got.output_tokens, 5);
            assert_eq!(got.updated_at, 200);
            assert_eq!(got.started_at, 100);
            assert_eq!(got.pending_question, "Proceed?");
            assert!(got.seen_at > 0);

            // Another node cannot take it over.
            let mut other = sess("s1", "n2");
            other.title = "hijack".into();
            assert_eq!(
                s.upsert_session(&other).await.unwrap(),
                UpsertOutcome::NotOwner
            );
            assert_eq!(
                s.session("s1").await.unwrap().unwrap().title,
                "fix the build"
            );
        }
    }

    #[tokio::test]
    async fn end_and_resume() {
        for (s, _d) in stores().await {
            s.upsert_session(&sess("s1", "n1")).await.unwrap();
            assert!(s.end_session("s1", 500, "completed").await.unwrap());
            assert!(!s.end_session("s1", 600, "again").await.unwrap());
            let got = s.session("s1").await.unwrap().unwrap();
            assert_eq!(got.ended_at, Some(500));
            assert_eq!(got.end_reason, "completed");
            assert_eq!(got.state, "ended");
            assert!(!got.is_open());
            // A live upsert reopens it (a resume).
            s.upsert_session(&sess("s1", "n1")).await.unwrap();
            let got = s.session("s1").await.unwrap().unwrap();
            assert!(got.is_open());
            assert_eq!(got.end_reason, "");
            assert!(!s.end_session("nope", 1, "").await.unwrap());
        }
    }

    #[tokio::test]
    async fn list_sessions_filters() {
        for (s, _d) in stores().await {
            s.upsert_session(&sess("s1", "n1")).await.unwrap();
            let mut b = sess("s2", "n2");
            b.title = "Write DOCS".into();
            b.card_id = "c1".into();
            s.upsert_session(&b).await.unwrap();
            s.end_session("s1", 0, "").await.unwrap();
            let all = s.sessions(&SessionQuery::default()).await.unwrap();
            assert_eq!(all.len(), 2);
            assert_eq!(all[0].id, "s1"); // seen most recently (end)
            let open = s
                .sessions(&SessionQuery {
                    open_only: true,
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(open.len(), 1);
            assert_eq!(open[0].id, "s2");
            let by_text = s
                .sessions(&SessionQuery {
                    text: Some("docs".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(by_text.len(), 1);
            let by = |n: &str, c: Option<&str>| SessionQuery {
                node_id: Some(n.into()),
                card_id: c.map(Into::into),
                limit: 10,
                ..Default::default()
            };
            assert_eq!(s.sessions(&by("n1", None)).await.unwrap().len(), 1);
            assert_eq!(s.sessions(&by("n2", Some("c1"))).await.unwrap().len(), 1);
            assert_eq!(s.sessions(&by("n1", Some("c1"))).await.unwrap().len(), 0);
            let page = s
                .sessions(&SessionQuery {
                    before_seen: Some(all[0].seen_at),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert!(page.iter().all(|x| x.seen_at < all[0].seen_at));
        }
    }

    #[tokio::test]
    async fn append_is_idempotent() {
        for (s, _d) in stores().await {
            s.upsert_session(&sess("s1", "n1")).await.unwrap();
            let r = s
                .append_entries("s1", &[entry(0, "a"), entry(1, "b")])
                .await
                .unwrap();
            assert_eq!(
                r,
                AppendOutcome {
                    inserted: 2,
                    next_index: 2
                }
            );
            // Replay plus one new: only the new one counts; old text kept.
            let r = s
                .append_entries("s1", &[entry(1, "changed"), entry(2, "c")])
                .await
                .unwrap();
            assert_eq!(
                r,
                AppendOutcome {
                    inserted: 1,
                    next_index: 3
                }
            );
            let r = s.append_entries("s1", &[]).await.unwrap();
            assert_eq!(r.next_index, 3);
            let all = s.entries("s1", 0, 0).await.unwrap();
            assert_eq!(
                all.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
                ["a", "b", "c"]
            );
            assert_eq!(s.entries("s1", 2, 0).await.unwrap().len(), 1);
            assert_eq!(s.entries("s1", 0, 1).await.unwrap().len(), 1);
            assert_eq!(s.session("s1").await.unwrap().unwrap().entry_count, 3);
            assert!(s.append_entries("nope", &[entry(0, "x")]).await.is_err());
            assert!(s.append_entries("s1", &[entry(-1, "x")]).await.is_err());
        }
    }

    #[tokio::test]
    async fn inbox_lifecycle() {
        for (s, _d) in stores().await {
            let a = s
                .inbox_push("s1", "i1", "prompt", r#"{"text":"hi"}"#)
                .await
                .unwrap();
            let b = s
                .inbox_push("s1", "i2", "command", r#"{"name":"stop"}"#)
                .await
                .unwrap();
            let other = s.inbox_push("s2", "i3", "prompt", "{}").await.unwrap();
            assert_eq!((a.seq, b.seq, other.seq), (1, 2, 1));
            assert_eq!(a.status, "pending");
            assert_eq!(s.inbox_unacked_after("s1", 0).await.unwrap().len(), 2);
            assert_eq!(s.inbox_unacked_after("s1", 1).await.unwrap().len(), 1);

            assert!(s.inbox_mark_delivered("s1", 1).await.unwrap());
            assert!(!s.inbox_mark_delivered("s1", 1).await.unwrap());
            let got = s.inbox_item(&a.id).await.unwrap().unwrap();
            assert_eq!(got.status, "delivered");
            // Delivered but unacked items are still replayed.
            assert_eq!(s.inbox_unacked_after("s1", 0).await.unwrap().len(), 2);

            let acked = s.inbox_ack("s1", &a.id, "done", "").await.unwrap().unwrap();
            assert_eq!(acked.status, "done");
            assert!(acked.acked_at.is_some());
            assert!(
                s.inbox_ack("s1", &a.id, "done", "")
                    .await
                    .unwrap()
                    .is_none()
            );
            // Wrong session cannot ack.
            assert!(
                s.inbox_ack("s2", &b.id, "done", "")
                    .await
                    .unwrap()
                    .is_none()
            );
            let left = s.inbox_unacked_after("s1", 0).await.unwrap();
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].id, b.id);
            assert!(!s.inbox_mark_delivered("s1", 1).await.unwrap());

            let items = s.inbox_items("s1", 10).await.unwrap();
            assert_eq!(items.iter().map(|i| i.seq).collect::<Vec<_>>(), [1, 2]);
            assert_eq!(s.inbox_items("s1", 1).await.unwrap()[0].seq, 2);
        }
    }

    #[tokio::test]
    async fn dispatch_lifecycle() {
        for (s, _d) in stores().await {
            s.insert_node("n1", "n", &HostInfo::default(), "serial", "tok")
                .await
                .unwrap();
            let d = Dispatch {
                id: "d1".into(),
                node_id: "n1".into(),
                kind: "session".into(),
                spec_json: "{}".into(),
                card_id: "c1".into(),
                ..Default::default()
            };
            s.insert_dispatch(&d).await.unwrap();
            let got = s.dispatch("d1").await.unwrap().unwrap();
            assert_eq!(got.status, "queued");
            assert_eq!(got.card_id, "c1");
            // Not on the outbox yet: nothing to deliver.
            assert_eq!(s.count_queued_dispatches().await.unwrap(), 0);
            let seq = s.outbox_push("n1", b"frame").await.unwrap();
            s.set_dispatch_outbox_seq("d1", seq).await.unwrap();
            assert_eq!(s.count_queued_dispatches().await.unwrap(), 1);
            assert!(s.mark_dispatches_delivered().await.unwrap().is_empty());
            s.outbox_ack("n1", seq).await.unwrap();
            let delivered = s.mark_dispatches_delivered().await.unwrap();
            assert_eq!(delivered.len(), 1);
            assert_eq!(delivered[0].status, "delivered");
            assert!(delivered[0].delivered_at.is_some());
            assert!(s.mark_dispatches_delivered().await.unwrap().is_empty());

            let r = s
                .resolve_dispatch("d1", "started", "s9", "")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (r.status.as_str(), r.session_id.as_str()),
                ("started", "s9")
            );
            assert!(r.resolved_at.is_some());
            assert!(
                s.resolve_dispatch("d1", "failed", "", "late")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                s.resolve_dispatch("zz", "failed", "", "")
                    .await
                    .unwrap()
                    .is_none()
            );

            s.insert_dispatch(&Dispatch {
                id: "d2".into(),
                node_id: "n2".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            assert_eq!(s.dispatches(None, 10).await.unwrap().len(), 2);
            assert_eq!(s.dispatches(Some("n2"), 10).await.unwrap()[0].id, "d2");
        }
    }

    #[tokio::test]
    async fn job_lifecycle() {
        for (s, _d) in stores().await {
            s.insert_job("j1", "n1", "github", r#"{"args":[]}"#, 99)
                .await
                .unwrap();
            s.insert_job("j2", "n1", "model", "{}", 99).await.unwrap();
            s.insert_job("j3", "n2", "model", "{}", 99).await.unwrap();
            let j = s.job("j1").await.unwrap().unwrap();
            assert_eq!((j.status.as_str(), j.deadline_at), ("queued", 99));
            let mut open = s.open_jobs_by_node().await.unwrap();
            open.sort();
            assert_eq!(open, vec![("n1".into(), 2), ("n2".into(), 1)]);
            let r = s
                .resolve_job("j1", "ok", r#"{"x":1}"#, "")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(r.status, "ok");
            assert_eq!(r.output_json, r#"{"x":1}"#);
            assert!(
                s.resolve_job("j1", "timeout", "", "")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(s.jobs(None, 10).await.unwrap().len(), 3);
            assert_eq!(s.jobs(Some("n2"), 10).await.unwrap().len(), 1);
            let open = s.open_jobs_by_node().await.unwrap();
            assert!(open.contains(&("n1".into(), 1)));
        }
    }

    #[tokio::test]
    async fn audit_lines_idempotent() {
        for (s, _d) in stores().await {
            let line = |idx| AuditLine {
                idx,
                at: idx,
                kind: "tool".into(),
                json: "{}".into(),
                ..Default::default()
            };
            assert_eq!(
                s.append_audit_lines("n1", "s1", &[line(0), line(1)])
                    .await
                    .unwrap(),
                2
            );
            assert_eq!(
                s.append_audit_lines("n1", "s1", &[line(1), line(2)])
                    .await
                    .unwrap(),
                1
            );
            let got = s.audit_lines("n1", "s1", 1, 0).await.unwrap();
            assert_eq!(got.iter().map(|l| l.idx).collect::<Vec<_>>(), [1, 2]);
            assert_eq!(got[0].node_id, "n1");
            assert!(s.audit_lines("n2", "s1", 0, 0).await.unwrap().is_empty());
        }
    }
}
