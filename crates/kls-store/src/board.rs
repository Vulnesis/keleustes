//! Store: M2 coordination: kanban, memory, A2A, factory control. Migration 0003.
//!
//! Cards and notes carry a `changed_at` server cursor that strictly increases
//! across the table (the later of now and the previous maximum plus one), so
//! a node that pulls "changed after X" never skips a row on a page boundary.

use super::*;

/// A board card (the protocol's `KanbanItem`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    /// Display key, e.g. `K-7f21ab`.
    pub key: String,
    pub title: String,
    pub body: String,
    /// "triage", "backlog", "todo", "doing", "review", "done".
    pub status: String,
    pub priority: i32,
    pub labels: Vec<String>,
    pub assignee: String,
    pub repo: String,
    pub branch: String,
    pub pr_url: String,
    pub meta_json: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub version: i64,
    pub deleted: bool,
    /// Server pull cursor.
    pub changed_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CardWrite {
    Written(Card),
    /// The stored version differs from the one the edit was based on: the
    /// stored copy (None when the card does not exist).
    Conflict(Option<Card>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardActivity {
    pub id: i64,
    pub card_id: String,
    pub at: i64,
    pub actor: String,
    pub kind: String,
    pub detail_json: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    /// "github.com/org/repo", "profile:<name>" or "global".
    pub scope: String,
    pub text: String,
    pub tags: Vec<String>,
    pub author: String,
    pub created_at: i64,
    /// The writer's clock: the newest copy wins.
    pub updated_at: i64,
    pub deleted: bool,
    /// Server pull cursor.
    pub changed_at: i64,
    /// The node (host) the note came from; empty when written here.
    #[serde(default)]
    pub host: String,
    /// The A2A message (`<thread>/<id>`) it was written from, if any.
    #[serde(default)]
    pub a2a_ref: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryScope {
    pub scope: String,
    pub live: i64,
    pub total: i64,
    pub last_write_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct A2aMessage {
    /// Assigned by the store.
    pub seq: i64,
    pub id: String,
    /// Agent ids are `<nodeId>:<sessionId>`.
    pub from: String,
    /// Empty for a broadcast.
    pub to: String,
    pub thread_id: String,
    pub text: String,
    pub meta_json: String,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct A2aThread {
    pub thread_id: String,
    pub count: i64,
    pub first_at: i64,
    pub last_at: i64,
    pub last_seq: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FactoryControl {
    pub stopped: bool,
    pub stopped_since: Option<i64>,
    pub stopped_reason: String,
    pub a2a_blackout: bool,
    pub a2a_since: Option<i64>,
    pub a2a_reason: String,
}

const CARD_COLS: &str = "id, key, title, body, status, priority, labels_json, assignee, repo, branch, \
     pr_url, meta_json, created_at, updated_at, version, deleted, changed_at";
const NOTE_COLS: &str = "id, scope, text, tags_json, author, created_at, updated_at, deleted, changed_at, host, a2a_ref";
const A2A_COLS: &str = "seq, id, from_agent, to_agent, thread_id, text, meta_json, at";

fn card_from_row(r: &AnyRow) -> Result<Card> {
    let labels: String = r.try_get("labels_json")?;
    Ok(Card {
        id: r.try_get("id")?,
        key: r.try_get("key")?,
        title: r.try_get("title")?,
        body: r.try_get("body")?,
        status: r.try_get("status")?,
        priority: r.try_get::<i64, _>("priority")? as i32,
        labels: serde_json::from_str(&labels).unwrap_or_default(),
        assignee: r.try_get("assignee")?,
        repo: r.try_get("repo")?,
        branch: r.try_get("branch")?,
        pr_url: r.try_get("pr_url")?,
        meta_json: r.try_get("meta_json")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        version: r.try_get("version")?,
        deleted: r.try_get::<i64, _>("deleted")? != 0,
        changed_at: r.try_get("changed_at")?,
    })
}

fn note_from_row(r: &AnyRow) -> Result<MemoryNote> {
    let tags: String = r.try_get("tags_json")?;
    Ok(MemoryNote {
        id: r.try_get("id")?,
        scope: r.try_get("scope")?,
        text: r.try_get("text")?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        author: r.try_get("author")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        deleted: r.try_get::<i64, _>("deleted")? != 0,
        changed_at: r.try_get("changed_at")?,
        host: r.try_get("host")?,
        a2a_ref: r.try_get("a2a_ref")?,
    })
}

fn a2a_from_row(r: &AnyRow) -> Result<A2aMessage> {
    Ok(A2aMessage {
        seq: r.try_get("seq")?,
        id: r.try_get("id")?,
        from: r.try_get("from_agent")?,
        to: r.try_get("to_agent")?,
        thread_id: r.try_get("thread_id")?,
        text: r.try_get("text")?,
        meta_json: r.try_get("meta_json")?,
        at: r.try_get("at")?,
    })
}

fn json_or(s: &str, default: &str) -> String {
    if s.trim().is_empty() {
        default.into()
    } else {
        s.into()
    }
}

impl Store {
    /// Next strictly increasing cursor for `table` (a constant name).
    async fn next_changed(
        &self,
        conn: &mut sqlx::AnyConnection,
        table: &'static str,
    ) -> Result<i64> {
        let q = format!("SELECT COALESCE(MAX(changed_at), 0) AS m FROM {table}");
        let row = sqlx::query(self.sql(&q)).fetch_one(&mut *conn).await?;
        Ok(now_ms().max(row.try_get::<i64, _>("m")? + 1))
    }

    // ---- kanban ------------------------------------------------------------

    /// Inserts a new card. `created_at`, `updated_at`, `version` and
    /// `changed_at` are set here; the card as stored is returned.
    pub async fn insert_card(&self, card: &Card) -> Result<Card> {
        let mut tx = self.begin_write().await?;
        let changed = self.next_changed(&mut tx, "kanban_items").await?;
        let now = now_ms();
        let q = format!(
            "INSERT INTO kanban_items ({CARD_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?) \
             RETURNING {CARD_COLS}"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(&card.id)
            .bind(&card.key)
            .bind(&card.title)
            .bind(&card.body)
            .bind(&card.status)
            .bind(card.priority as i64)
            .bind(serde_json::to_string(&card.labels)?)
            .bind(&card.assignee)
            .bind(&card.repo)
            .bind(&card.branch)
            .bind(&card.pr_url)
            .bind(json_or(&card.meta_json, "{}"))
            .bind(now)
            .bind(now)
            .bind(card.deleted as i64)
            .bind(changed)
            .fetch_one(&mut *tx)
            .await?;
        let out = card_from_row(&row)?;
        tx.commit().await?;
        Ok(out)
    }

    /// A card by id or display key (deleted ones included).
    pub async fn card(&self, id_or_key: &str) -> Result<Option<Card>> {
        let q = format!("SELECT {CARD_COLS} FROM kanban_items WHERE id = ? OR key = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id_or_key)
            .bind(id_or_key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(card_from_row).transpose()
    }

    pub async fn card_key_taken(&self, key: &str) -> Result<bool> {
        let row = sqlx::query(self.sql("SELECT id FROM kanban_items WHERE key = ?"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Live cards (or all, with tombstones), most recently updated first.
    pub async fn cards(&self, include_deleted: bool) -> Result<Vec<Card>> {
        let q = format!(
            "SELECT {CARD_COLS} FROM kanban_items WHERE deleted = 0 OR ? = 1 ORDER BY updated_at DESC, id"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(include_deleted as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(card_from_row).collect()
    }

    /// Cards changed after `since` (tombstones included), oldest change
    /// first, at most `limit`; `true` when more follow.
    pub async fn cards_changed_since(&self, since: i64, limit: i64) -> Result<(Vec<Card>, bool)> {
        let q = format!(
            "SELECT {CARD_COLS} FROM kanban_items WHERE changed_at > ? ORDER BY changed_at LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(since)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?;
        let mut out: Vec<Card> = rows.iter().map(card_from_row).collect::<Result<_>>()?;
        let more = out.len() as i64 > limit;
        out.truncate(limit.max(0) as usize);
        Ok((out, more))
    }

    /// Writes every mutable field of `card` if the stored version is
    /// `base_version`; bumps the version, `updated_at` and the cursor.
    pub async fn update_card(&self, card: &Card, base_version: i64) -> Result<CardWrite> {
        let mut tx = self.begin_write().await?;
        let changed = self.next_changed(&mut tx, "kanban_items").await?;
        let q = format!(
            "UPDATE kanban_items SET title = ?, body = ?, status = ?, priority = ?, labels_json = ?, \
             assignee = ?, repo = ?, branch = ?, pr_url = ?, meta_json = ?, deleted = ?, \
             updated_at = ?, version = version + 1, changed_at = ? \
             WHERE id = ? AND version = ? RETURNING {CARD_COLS}"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(&card.title)
            .bind(&card.body)
            .bind(&card.status)
            .bind(card.priority as i64)
            .bind(serde_json::to_string(&card.labels)?)
            .bind(&card.assignee)
            .bind(&card.repo)
            .bind(&card.branch)
            .bind(&card.pr_url)
            .bind(json_or(&card.meta_json, "{}"))
            .bind(card.deleted as i64)
            .bind(now_ms())
            .bind(changed)
            .bind(&card.id)
            .bind(base_version)
            .fetch_optional(&mut *tx)
            .await?;
        let out = match row {
            Some(r) => CardWrite::Written(card_from_row(&r)?),
            None => {
                let q = format!("SELECT {CARD_COLS} FROM kanban_items WHERE id = ?");
                let cur = sqlx::query(self.sql(&q))
                    .bind(&card.id)
                    .fetch_optional(&mut *tx)
                    .await?;
                CardWrite::Conflict(cur.as_ref().map(card_from_row).transpose()?)
            }
        };
        tx.commit().await?;
        Ok(out)
    }

    pub async fn add_card_activity(
        &self,
        card_id: &str,
        actor: &str,
        kind: &str,
        detail_json: &str,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO kanban_activity (card_id, at, actor, kind, detail_json) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(card_id)
        .bind(now_ms())
        .bind(actor)
        .bind(kind)
        .bind(json_or(detail_json, "{}"))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A card's activity log, newest first.
    pub async fn card_activity(&self, card_id: &str, limit: i64) -> Result<Vec<CardActivity>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, card_id, at, actor, kind, detail_json FROM kanban_activity \
             WHERE card_id = ? ORDER BY id DESC LIMIT ?",
        ))
        .bind(card_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(CardActivity {
                    id: r.try_get("id")?,
                    card_id: r.try_get("card_id")?,
                    at: r.try_get("at")?,
                    actor: r.try_get("actor")?,
                    kind: r.try_get("kind")?,
                    detail_json: r.try_get("detail_json")?,
                })
            })
            .collect()
    }

    // ---- memory ------------------------------------------------------------

    pub async fn note(&self, id: &str) -> Result<Option<MemoryNote>> {
        let q = format!("SELECT {NOTE_COLS} FROM memory_notes WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(note_from_row).transpose()
    }

    /// Stores a copy of a note under the replica rule: a new note is
    /// inserted; a stored tombstone never changes again; an incoming
    /// tombstone or a strictly newer `updated_at` replaces the stored copy;
    /// anything else is ignored. Returns the stored note and whether it changed.
    pub async fn upsert_note(&self, n: &MemoryNote) -> Result<(MemoryNote, bool)> {
        let mut tx = self.begin_write().await?;
        let q = format!("SELECT {NOTE_COLS} FROM memory_notes WHERE id = ?");
        let cur = sqlx::query(self.sql(&q))
            .bind(&n.id)
            .fetch_optional(&mut *tx)
            .await?
            .as_ref()
            .map(note_from_row)
            .transpose()?;
        let tags = serde_json::to_string(&n.tags)?;
        let row = match &cur {
            None => {
                let changed = self.next_changed(&mut tx, "memory_notes").await?;
                let q = format!(
                    "INSERT INTO memory_notes ({NOTE_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING {NOTE_COLS}"
                );
                sqlx::query(self.sql(&q))
                    .bind(&n.id)
                    .bind(&n.scope)
                    .bind(&n.text)
                    .bind(&tags)
                    .bind(&n.author)
                    .bind(n.created_at)
                    .bind(n.updated_at.max(n.created_at))
                    .bind(n.deleted as i64)
                    .bind(changed)
                    .bind(&n.host)
                    .bind(&n.a2a_ref)
                    .fetch_one(&mut *tx)
                    .await?
            }
            Some(c) if c.deleted || (!n.deleted && n.updated_at <= c.updated_at) => {
                let out = c.clone();
                tx.commit().await?;
                return Ok((out, false));
            }
            Some(c) => {
                let changed = self.next_changed(&mut tx, "memory_notes").await?;
                let q = format!(
                    "UPDATE memory_notes SET text = ?, tags_json = ?, author = ?, updated_at = ?, \
                     deleted = ?, changed_at = ? WHERE id = ? RETURNING {NOTE_COLS}"
                );
                // A tombstone keeps the stored text unless it brings its own.
                let (text, tags) = if n.deleted && n.text.is_empty() {
                    (c.text.clone(), serde_json::to_string(&c.tags)?)
                } else {
                    (n.text.clone(), tags)
                };
                sqlx::query(self.sql(&q))
                    .bind(text)
                    .bind(tags)
                    .bind(&n.author)
                    .bind(n.updated_at.max(c.updated_at))
                    .bind(n.deleted as i64)
                    .bind(changed)
                    .bind(&n.id)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        let out = note_from_row(&row)?;
        tx.commit().await?;
        Ok((out, true))
    }

    /// Notes for the console, newest first. `scope` None is every scope;
    /// `q` is a case-insensitive substring of the text or tags.
    pub async fn notes(
        &self,
        scope: Option<&str>,
        q: &str,
        include_deleted: bool,
        limit: i64,
    ) -> Result<Vec<MemoryNote>> {
        let sql = format!(
            "SELECT {NOTE_COLS} FROM memory_notes WHERE (? = '' OR scope = ?) AND (deleted = 0 OR ? = 1) \
             AND (? = '' OR LOWER(text) LIKE ? OR LOWER(tags_json) LIKE ?) \
             ORDER BY updated_at DESC, id LIMIT ?"
        );
        let scope = scope.unwrap_or("");
        let needle = q.trim().to_lowercase();
        let like = format!("%{needle}%");
        let rows = sqlx::query(self.sql(&sql))
            .bind(scope)
            .bind(scope)
            .bind(include_deleted as i64)
            .bind(&needle)
            .bind(&like)
            .bind(&like)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(note_from_row).collect()
    }

    /// Notes changed after `since` (tombstones included), oldest change
    /// first. An empty `scope` is every scope.
    pub async fn notes_changed_since(
        &self,
        scope: &str,
        since: i64,
        limit: i64,
    ) -> Result<(Vec<MemoryNote>, bool)> {
        let q = format!(
            "SELECT {NOTE_COLS} FROM memory_notes WHERE (? = '' OR scope = ?) AND changed_at > ? \
             ORDER BY changed_at LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(scope)
            .bind(scope)
            .bind(since)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?;
        let mut out: Vec<MemoryNote> = rows.iter().map(note_from_row).collect::<Result<_>>()?;
        let more = out.len() as i64 > limit;
        out.truncate(limit.max(0) as usize);
        Ok((out, more))
    }

    /// Every scope with its live and total counts, most recently written first.
    pub async fn memory_scopes(&self) -> Result<Vec<MemoryScope>> {
        let rows = sqlx::query(self.sql(
            "SELECT scope, SUM(CASE WHEN deleted = 0 THEN 1 ELSE 0 END) AS live, COUNT(*) AS total, \
             MAX(updated_at) AS last FROM memory_notes GROUP BY scope ORDER BY last DESC, scope",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MemoryScope {
                    scope: r.try_get("scope")?,
                    live: r.try_get("live")?,
                    total: r.try_get("total")?,
                    last_write_at: r.try_get("last")?,
                })
            })
            .collect()
    }

    /// Tombstones every live note of `scope`; returns how many.
    pub async fn clear_memory_scope(&self, scope: &str) -> Result<u64> {
        let mut tx = self.begin_write().await?;
        let ids: Vec<String> = sqlx::query(
            self.sql("SELECT id FROM memory_notes WHERE scope = ? AND deleted = 0 ORDER BY id"),
        )
        .bind(scope)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(|r| r.try_get::<String, _>("id"))
        .collect::<std::result::Result<_, _>>()?;
        let now = now_ms();
        let mut changed = self.next_changed(&mut tx, "memory_notes").await?;
        for id in &ids {
            sqlx::query(self.sql(
                "UPDATE memory_notes SET deleted = 1, updated_at = CASE WHEN updated_at < ? THEN ? ELSE updated_at + 1 END, \
                 changed_at = ? WHERE id = ?",
            ))
            .bind(now)
            .bind(now)
            .bind(changed)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            changed += 1;
        }
        tx.commit().await?;
        Ok(ids.len() as u64)
    }

    // ---- A2A ---------------------------------------------------------------

    /// Stores an envelope, idempotently on `id`. Returns the stored copy
    /// (with its seq) and whether it was new.
    pub async fn insert_a2a(&self, m: &A2aMessage) -> Result<(A2aMessage, bool)> {
        let q = format!(
            "INSERT INTO a2a_messages (id, from_agent, to_agent, thread_id, text, meta_json, at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (id) DO NOTHING RETURNING {A2A_COLS}"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(&m.id)
            .bind(&m.from)
            .bind(&m.to)
            .bind(&m.thread_id)
            .bind(&m.text)
            .bind(json_or(&m.meta_json, "{}"))
            .bind(m.at)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(r) = row {
            return Ok((a2a_from_row(&r)?, true));
        }
        let q = format!("SELECT {A2A_COLS} FROM a2a_messages WHERE id = ?");
        let r = sqlx::query(self.sql(&q))
            .bind(&m.id)
            .fetch_one(&self.pool)
            .await?;
        Ok((a2a_from_row(&r)?, false))
    }

    /// Envelopes after `after`, oldest first.
    pub async fn a2a_after(&self, after: i64, limit: i64) -> Result<Vec<A2aMessage>> {
        let q = format!("SELECT {A2A_COLS} FROM a2a_messages WHERE seq > ? ORDER BY seq LIMIT ?");
        let rows = sqlx::query(self.sql(&q))
            .bind(after)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(a2a_from_row).collect()
    }

    /// The highest seq stored (0 when empty).
    pub async fn a2a_head(&self) -> Result<i64> {
        let r = sqlx::query(self.sql("SELECT COALESCE(MAX(seq), 0) AS m FROM a2a_messages"))
            .fetch_one(&self.pool)
            .await?;
        Ok(r.try_get("m")?)
    }

    /// Threads, most recently active first.
    pub async fn a2a_threads(&self, limit: i64) -> Result<Vec<A2aThread>> {
        let rows = sqlx::query(self.sql(
            "SELECT thread_id, COUNT(*) AS n, MIN(at) AS first_at, MAX(at) AS last_at, MAX(seq) AS last_seq \
             FROM a2a_messages GROUP BY thread_id ORDER BY last_seq DESC LIMIT ?",
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(A2aThread {
                    thread_id: r.try_get("thread_id")?,
                    count: r.try_get("n")?,
                    first_at: r.try_get("first_at")?,
                    last_at: r.try_get("last_at")?,
                    last_seq: r.try_get("last_seq")?,
                })
            })
            .collect()
    }

    /// One thread, oldest first (at most 200).
    pub async fn a2a_thread(&self, thread_id: &str) -> Result<Vec<A2aMessage>> {
        let q = format!(
            "SELECT {A2A_COLS} FROM a2a_messages WHERE thread_id = ? ORDER BY seq LIMIT 200"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(thread_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(a2a_from_row).collect()
    }

    // ---- factory control ---------------------------------------------------

    pub async fn factory_control(&self) -> Result<FactoryControl> {
        let r = sqlx::query(self.sql(
            "SELECT stopped, stopped_since, stopped_reason, a2a_blackout, a2a_since, a2a_reason \
             FROM factory_control WHERE id = 1",
        ))
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else {
            return Ok(FactoryControl::default());
        };
        Ok(FactoryControl {
            stopped: r.try_get::<i64, _>("stopped")? != 0,
            stopped_since: r.try_get("stopped_since")?,
            stopped_reason: r.try_get("stopped_reason")?,
            a2a_blackout: r.try_get::<i64, _>("a2a_blackout")? != 0,
            a2a_since: r.try_get("a2a_since")?,
            a2a_reason: r.try_get("a2a_reason")?,
        })
    }

    /// Sets the stop switch. Returns false (and changes nothing) when it is
    /// already in that state.
    pub async fn set_factory_stopped(&self, stopped: bool, reason: &str) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE factory_control SET stopped = ?, stopped_since = ?, stopped_reason = ? \
             WHERE id = 1 AND stopped <> ?",
        ))
        .bind(stopped as i64)
        .bind(now_ms())
        .bind(if stopped { reason } else { "" })
        .bind(stopped as i64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Sets the A2A blackout. Returns false when it is already in that state.
    pub async fn set_a2a_blackout(&self, on: bool, reason: &str) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE factory_control SET a2a_blackout = ?, a2a_since = ?, a2a_reason = ? \
             WHERE id = 1 AND a2a_blackout <> ?",
        ))
        .bind(on as i64)
        .bind(now_ms())
        .bind(if on { reason } else { "" })
        .bind(on as i64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
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
                "kanban_items",
                "kanban_activity",
                "memory_notes",
                "a2a_messages",
            ] {
                sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                    .execute(&s.pool)
                    .await
                    .unwrap();
            }
            sqlx::query(
                "UPDATE factory_control SET stopped = 0, a2a_blackout = 0, stopped_reason = '', a2a_reason = ''",
            )
            .execute(&s.pool)
            .await
            .unwrap();
            out.push((s, None));
        }
        out
    }

    fn card(id: &str, key: &str) -> Card {
        Card {
            id: id.into(),
            key: key.into(),
            title: "Fix it".into(),
            status: "todo".into(),
            labels: vec!["bug".into()],
            priority: 2,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn card_insert_update_conflict_and_cursor() {
        for (s, _d) in stores().await {
            let a = s.insert_card(&card("c1", "K-000001")).await.unwrap();
            assert_eq!(a.version, 1);
            assert_eq!(a.labels, vec!["bug".to_string()]);
            assert!(s.card_key_taken("K-000001").await.unwrap());
            let b = s.insert_card(&card("c2", "K-000002")).await.unwrap();
            assert!(b.changed_at > a.changed_at, "cursor strictly increases");
            assert_eq!(s.card("K-000002").await.unwrap().unwrap().id, "c2");

            let mut edit = a.clone();
            edit.status = "doing".into();
            let CardWrite::Written(a2) = s.update_card(&edit, 1).await.unwrap() else {
                panic!("expected write");
            };
            assert_eq!((a2.version, a2.status.as_str()), (2, "doing"));
            // Stale version.
            let CardWrite::Conflict(Some(cur)) = s.update_card(&edit, 1).await.unwrap() else {
                panic!("expected conflict");
            };
            assert_eq!(cur.version, 2);
            let mut ghost = edit.clone();
            ghost.id = "nope".into();
            assert_eq!(
                s.update_card(&ghost, 1).await.unwrap(),
                CardWrite::Conflict(None)
            );

            // Cursor paging: c2 then c1 (moved last).
            let (p1, more) = s.cards_changed_since(0, 1).await.unwrap();
            assert!(more);
            assert_eq!(p1[0].id, "c2");
            let (p2, more) = s.cards_changed_since(p1[0].changed_at, 10).await.unwrap();
            assert!(!more);
            assert_eq!(p2.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c1"]);

            // Soft delete.
            let mut del = a2.clone();
            del.deleted = true;
            assert!(matches!(
                s.update_card(&del, 2).await.unwrap(),
                CardWrite::Written(_)
            ));
            assert_eq!(s.cards(false).await.unwrap().len(), 1);
            assert_eq!(s.cards(true).await.unwrap().len(), 2);

            s.add_card_activity("c1", "me", "moved", r#"{"to":"doing"}"#)
                .await
                .unwrap();
            s.add_card_activity("c1", "me", "deleted", "")
                .await
                .unwrap();
            let log = s.card_activity("c1", 10).await.unwrap();
            assert_eq!(log.len(), 2);
            assert_eq!(log[0].kind, "deleted");
            assert_eq!(log[0].detail_json, "{}");
        }
    }

    fn note(id: &str, scope: &str, text: &str, updated: i64) -> MemoryNote {
        MemoryNote {
            id: id.into(),
            scope: scope.into(),
            text: text.into(),
            tags: vec!["build".into()],
            author: "h:s".into(),
            created_at: 100,
            updated_at: updated,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn memory_newest_wins_tombstones_and_scopes() {
        for (s, _d) in stores().await {
            let (n, changed) = s
                .upsert_note(&note("n1", "acme/api", "just check", 100))
                .await
                .unwrap();
            assert!(changed);
            assert_eq!(n.tags, vec!["build".to_string()]);
            // Older copy ignored, newer applied.
            let (n, changed) = s
                .upsert_note(&note("n1", "acme/api", "old", 50))
                .await
                .unwrap();
            assert!(!changed);
            assert_eq!(n.text, "just check");
            let (n, _) = s
                .upsert_note(&note("n1", "acme/api", "cargo test", 200))
                .await
                .unwrap();
            assert_eq!(n.text, "cargo test");

            s.upsert_note(&note("n2", "acme/api", "never push main", 100))
                .await
                .unwrap();
            s.upsert_note(&note("n3", "global", "be kind", 100))
                .await
                .unwrap();

            assert_eq!(
                s.notes(Some("acme/api"), "", false, 50)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(s.notes(None, "KIND", false, 50).await.unwrap().len(), 1);
            assert_eq!(s.notes(None, "build", false, 50).await.unwrap().len(), 3);

            let scopes = s.memory_scopes().await.unwrap();
            let api = scopes.iter().find(|x| x.scope == "acme/api").unwrap();
            assert_eq!((api.live, api.total), (2, 2));

            assert_eq!(s.clear_memory_scope("acme/api").await.unwrap(), 2);
            assert!(
                s.notes(Some("acme/api"), "", false, 50)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                s.notes(Some("acme/api"), "", true, 50).await.unwrap().len(),
                2
            );
            // A tombstone never changes again.
            let (n, changed) = s
                .upsert_note(&note("n1", "acme/api", "revived", 9_999_999_999_999))
                .await
                .unwrap();
            assert!(!changed);
            assert!(n.deleted);

            // Pull cursor sees every change once.
            let (all, more) = s.notes_changed_since("", 0, 100).await.unwrap();
            assert!(!more);
            assert_eq!(all.len(), 3);
            let (scoped, _) = s.notes_changed_since("global", 0, 100).await.unwrap();
            assert_eq!(scoped.len(), 1);
            let last = all.last().unwrap().changed_at;
            assert!(
                s.notes_changed_since("", last, 100)
                    .await
                    .unwrap()
                    .0
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn a2a_idempotent_and_threads() {
        for (s, _d) in stores().await {
            let m = A2aMessage {
                id: "a2a-1".into(),
                from: "n1:s1".into(),
                to: "n2:s2".into(),
                thread_id: "a2a-1".into(),
                text: "are you editing src/a.go?".into(),
                at: 10,
                ..Default::default()
            };
            let (a, new) = s.insert_a2a(&m).await.unwrap();
            assert!(new);
            assert!(a.seq > 0);
            assert_eq!(a.meta_json, "{}");
            let (again, new) = s.insert_a2a(&m).await.unwrap();
            assert!(!new);
            assert_eq!(again.seq, a.seq);
            let reply = A2aMessage {
                id: "a2a-2".into(),
                from: "n2:s2".into(),
                to: "n1:s1".into(),
                thread_id: "a2a-1".into(),
                text: "no".into(),
                at: 20,
                ..Default::default()
            };
            let (b, _) = s.insert_a2a(&reply).await.unwrap();
            assert_eq!(s.a2a_head().await.unwrap(), b.seq);
            assert_eq!(s.a2a_after(a.seq, 10).await.unwrap(), vec![b.clone()]);
            let threads = s.a2a_threads(10).await.unwrap();
            assert_eq!(threads.len(), 1);
            assert_eq!(
                (threads[0].count, threads[0].first_at, threads[0].last_at),
                (2, 10, 20)
            );
            assert_eq!(s.a2a_thread("a2a-1").await.unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn factory_control_switches() {
        for (s, _d) in stores().await {
            let c = s.factory_control().await.unwrap();
            assert!(!c.stopped && !c.a2a_blackout);
            assert!(s.set_factory_stopped(true, "incident").await.unwrap());
            assert!(
                !s.set_factory_stopped(true, "again").await.unwrap(),
                "already stopped"
            );
            assert!(s.set_a2a_blackout(true, "noise").await.unwrap());
            let c = s.factory_control().await.unwrap();
            assert!(c.stopped && c.a2a_blackout);
            assert_eq!(c.stopped_reason, "incident");
            assert_eq!(c.a2a_reason, "noise");
            assert!(c.stopped_since.is_some());
            assert!(s.set_factory_stopped(false, "").await.unwrap());
            assert!(s.set_a2a_blackout(false, "").await.unwrap());
            let c = s.factory_control().await.unwrap();
            assert!(!c.stopped && !c.a2a_blackout && c.a2a_reason.is_empty());
        }
    }

    #[tokio::test]
    async fn note_keeps_host_and_a2a_ref() {
        for (s, _d) in stores().await {
            let mut n = note("h1", "acme/api", "x", 10);
            n.host = "node-a".into();
            n.a2a_ref = "t1/m2".into();
            let (got, _) = s.upsert_note(&n).await.unwrap();
            assert_eq!(
                (got.host.as_str(), got.a2a_ref.as_str()),
                ("node-a", "t1/m2")
            );
        }
    }
}
