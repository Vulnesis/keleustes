//! Store: M6: the hash-chained audit ledger, session deletion with a
//! tombstone, teleport bundles, the settings document's revisioned writes,
//! and the read-only facts the Trust page aggregates. Migration 0007.
//!
//! The ledger is one chain. Entry `n` stores `prev_hash` (entry `n-1`'s
//! hash, `""` for the first) and `hash`: `"sha256:"` and the hex SHA-256 of
//! `prev_hash` followed by the canonical JSON of the entry. The canonical JSON sorts object keys
//! and has no whitespace ([`canonical_json`]). Rows are never updated or
//! deleted (triggers refuse it); [`Store::ledger_verify`] recomputes the
//! chain and names the first entry that does not continue it.

use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// One writer at a time per process: an entry's seq and prev_hash come from
/// the current head, read in the same transaction.
static LEDGER_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// The setting that counts saves of the settings document.
pub const SETTINGS_REVISION: &str = "settings.revision";

/// Object keys sorted, no whitespace: the text the ledger hashes.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `"sha256:" + hex(SHA-256(prev_hash + canonical))`.
pub fn chain_hash(prev_hash: &str, canonical: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(canonical.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// An entry to append. The store stamps seq, prev_hash and hash.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LedgerDraft {
    /// 0 means now.
    pub at: i64,
    pub kind: String,
    pub actor: String,
    /// "user", "agent", "host", "system", "rule".
    pub actor_kind: String,
    pub msg: String,
    pub subject: Value,
    pub payload: Value,
    /// "factory" or "host:<nodeId>". Empty means "factory".
    pub origin: String,
    pub node_id: String,
    pub session_id: String,
    /// Source identity; a draft whose ref_key is already chained is skipped.
    pub ref_key: Option<String>,
}

/// A chained ledger entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub seq: i64,
    pub at: i64,
    pub kind: String,
    pub actor: String,
    pub actor_kind: String,
    pub msg: String,
    pub subject: Value,
    pub payload: Value,
    pub origin: String,
    pub node_id: String,
    pub session_id: String,
    #[serde(rename = "ref")]
    pub ref_key: Option<String>,
    pub prev_hash: String,
    pub hash: String,
}

impl LedgerEntry {
    /// The object the hash covers: everything but `prevHash` and `hash`.
    pub fn hashed_body(&self) -> Value {
        json!({
            "seq": self.seq,
            "at": self.at,
            "kind": self.kind,
            "actor": self.actor,
            "actorKind": self.actor_kind,
            "msg": self.msg,
            "subject": self.subject,
            "payload": self.payload,
            "origin": self.origin,
            "nodeId": self.node_id,
            "sessionId": self.session_id,
            "ref": self.ref_key,
        })
    }

    /// The hash this entry should carry given its prev_hash.
    pub fn expected_hash(&self) -> String {
        chain_hash(&self.prev_hash, &canonical_json(&self.hashed_body()))
    }
}

/// The last entry of the chain (`seq` 0 and an empty hash when empty).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChainHead {
    pub seq: i64,
    pub hash: String,
}

/// The first entry that does not continue the chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokenAt {
    /// The seq the walk expected next (for a gap) or the entry's own seq.
    pub seq: i64,
    /// 0-based position in the walk.
    pub index: i64,
    pub expected: String,
    pub found: String,
    /// "gap", "prev_hash", "hash".
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainReport {
    pub chain: String,
    pub ok: bool,
    pub entries: i64,
    pub head: String,
    pub broken_at: Option<BrokenAt>,
}

/// Filter for [`Store::ledger`]. Empty fields match everything.
#[derive(Clone, Debug, Default)]
pub struct LedgerQuery {
    pub kinds: Vec<String>,
    /// Case-insensitive substring of msg, kind, actor or subject.
    pub text: Option<String>,
    pub session_id: Option<String>,
    pub node_id: Option<String>,
    /// "factory", "host" (any host) or "host:<nodeId>".
    pub origin: Option<String>,
    pub from_at: Option<i64>,
    pub to_at: Option<i64>,
    pub before_seq: Option<i64>,
    pub after_seq: Option<i64>,
    /// Oldest first (export, verify) instead of newest first.
    pub ascending: bool,
    /// Values that must each appear in the subject, payload or message
    /// (card key, agent, repo, workflow or run id, vulnerability id).
    pub facts: Vec<String>,
    /// 0 means 200; at most 10,000.
    pub limit: i64,
}

/// What a session deletion removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPurge {
    pub entries: u64,
    pub inbox: u64,
    pub audit_lines: u64,
}

/// A session bundle moving between hosts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Teleport {
    pub id: String,
    pub session_id: String,
    pub from_node: String,
    pub to_node: String,
    pub title: String,
    /// "requested", "ready", "delivered", "expired", "failed".
    pub status: String,
    pub size: i64,
    pub sha256: String,
    pub error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub uploaded_at: Option<i64>,
    pub downloaded_at: Option<i64>,
    pub expires_at: i64,
}

/// Outcome of a revision-checked settings write.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsWrite {
    Saved {
        revision: i64,
        entries: Vec<LedgerEntry>,
    },
    /// The document changed since `expected`; nothing was written.
    Conflict { current: i64 },
}

/// Per-repository counts from sessions, runs, commits and resolutions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoCounts {
    pub repo: String,
    pub total: i64,
    /// Sessions linked to a run or card; runs linked to a card; signed
    /// commits; verified resolutions.
    pub good: i64,
    /// Commits recorded as unsigned (`signed = 0`); 0 elsewhere.
    pub bad: i64,
}

/// A host audit line with the repository of its session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoLine {
    pub repo: String,
    pub kind: String,
    pub json: String,
    pub at: i64,
}

const LEDGER_COLS: &str = "seq, at, kind, actor, actor_kind, msg, subject_json, payload_json, \
     origin, node_id, session_id, ref_key, prev_hash, hash";

const TELEPORT_COLS: &str = "id, session_id, from_node, to_node, title, status, size, sha256, \
     error, created_at, updated_at, uploaded_at, downloaded_at, expires_at";

fn parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

fn ledger_from_row(r: &AnyRow) -> Result<LedgerEntry> {
    let subject: String = r.try_get("subject_json")?;
    let payload: String = r.try_get("payload_json")?;
    Ok(LedgerEntry {
        seq: r.try_get("seq")?,
        at: r.try_get("at")?,
        kind: r.try_get("kind")?,
        actor: r.try_get("actor")?,
        actor_kind: r.try_get("actor_kind")?,
        msg: r.try_get("msg")?,
        subject: parse_json(&subject),
        payload: parse_json(&payload),
        origin: r.try_get("origin")?,
        node_id: r.try_get("node_id")?,
        session_id: r.try_get("session_id")?,
        ref_key: r.try_get("ref_key")?,
        prev_hash: r.try_get("prev_hash")?,
        hash: r.try_get("hash")?,
    })
}

fn teleport_from_row(r: &AnyRow) -> Result<Teleport> {
    Ok(Teleport {
        id: r.try_get("id")?,
        session_id: r.try_get("session_id")?,
        from_node: r.try_get("from_node")?,
        to_node: r.try_get("to_node")?,
        title: r.try_get("title")?,
        status: r.try_get("status")?,
        size: r.try_get("size")?,
        sha256: r.try_get("sha256")?,
        error: r.try_get("error")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        uploaded_at: r.try_get("uploaded_at")?,
        downloaded_at: r.try_get("downloaded_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

fn is_unique_violation(e: &anyhow::Error) -> bool {
    e.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .is_some_and(|d| d.is_unique_violation())
}

/// Normalises an empty value to the shape the hash covers.
fn obj_or_empty(v: &Value) -> Value {
    if v.is_null() { json!({}) } else { v.clone() }
}

type Tx = sqlx::Transaction<'static, sqlx::Any>;

#[derive(Clone)]
enum Arg {
    I(i64),
    S(String),
}

/// A window of events for the Events screen and activity logs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventQuery {
    /// Kinds starting with any of these ("" or none: all).
    pub prefixes: Vec<String>,
    /// Only ids below this (paging back: "Load older").
    pub before_id: Option<i64>,
    pub from_at: Option<i64>,
    pub to_at: Option<i64>,
    /// Case-insensitive substring of kind, subject or payload.
    pub text: Option<String>,
    /// 0 means 500.
    pub limit: i64,
}

fn session_fact_row(r: &AnyRow) -> Result<SessionFact> {
    Ok(SessionFact {
        session_id: r.try_get("session_id")?,
        kind: r.try_get("kind")?,
        value: r.try_get("value")?,
        first_at: r.try_get("first_at")?,
        last_at: r.try_get("last_at")?,
        hits: r.try_get("hits")?,
    })
}
fn event_row(r: &AnyRow) -> Result<Event> {
    Ok(Event {
        id: r.try_get("id")?,
        at: r.try_get("at")?,
        kind: r.try_get("kind")?,
        subject: r.try_get("subject")?,
        data_json: r.try_get("data_json")?,
    })
}

/// One indexed fact of a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFact {
    pub session_id: String,
    /// "repo", "pr", "card", "agent", "model", "node", "run", "vuln", "file".
    pub kind: String,
    pub value: String,
    pub first_at: i64,
    pub last_at: i64,
    pub hits: i64,
}

impl Store {
    // ---- session facts -----------------------------------------------------

    /// Adds facts for a session and moves its fold cursor, in one write
    /// transaction. Repeats raise `hits` and `last_at`.
    pub async fn session_facts_add(
        &self,
        session_id: &str,
        facts: &[(String, String, i64)],
        entry_idx: i64,
        line_idx: i64,
    ) -> Result<()> {
        let mut tx = self.begin_write().await?;
        for (kind, value, at) in facts {
            sqlx::query(self.sql(
                "INSERT INTO session_facts (session_id, kind, value, first_at, last_at, hits) \
                 VALUES (?, ?, ?, ?, ?, 1) ON CONFLICT (session_id, kind, value) DO UPDATE SET \
                 hits = session_facts.hits + 1, last_at = excluded.last_at",
            ))
            .bind(session_id)
            .bind(kind)
            .bind(value)
            .bind(at)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(self.sql(
            "INSERT INTO session_fact_cursor (session_id, entry_idx, line_idx) VALUES (?, ?, ?) \
             ON CONFLICT (session_id) DO UPDATE SET entry_idx = excluded.entry_idx, \
             line_idx = excluded.line_idx",
        ))
        .bind(session_id)
        .bind(entry_idx)
        .bind(line_idx)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// (entry idx, audit line idx) folded so far for a session (-1, -1 when new).
    pub async fn session_fact_cursor(&self, session_id: &str) -> Result<(i64, i64)> {
        let r = sqlx::query(
            self.sql("SELECT entry_idx, line_idx FROM session_fact_cursor WHERE session_id = ?"),
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match r {
            Some(r) => (r.try_get("entry_idx")?, r.try_get("line_idx")?),
            None => (-1, -1),
        })
    }

    /// A session's facts, by kind then value.
    pub async fn session_facts(&self, session_id: &str) -> Result<Vec<SessionFact>> {
        let rows = sqlx::query(self.sql(
            "SELECT session_id, kind, value, first_at, last_at, hits FROM session_facts \
             WHERE session_id = ? ORDER BY kind, value",
        ))
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(session_fact_row).collect()
    }

    /// Sessions holding every one of `facts` ((kind, value), case-insensitive),
    /// most recently touched first.
    pub async fn sessions_with_facts(
        &self,
        facts: &[(String, String)],
        limit: i64,
    ) -> Result<Vec<String>> {
        let mut out: Option<Vec<(String, i64)>> = None;
        for (kind, value) in facts {
            let rows = sqlx::query(self.sql(
                "SELECT session_id, MAX(last_at) AS at FROM session_facts \
                 WHERE kind = ? AND LOWER(value) = ? GROUP BY session_id",
            ))
            .bind(kind)
            .bind(value.to_lowercase())
            .fetch_all(&self.pool)
            .await?;
            let got: Vec<(String, i64)> = rows
                .iter()
                .map(|r| Ok((r.try_get("session_id")?, r.try_get("at")?)))
                .collect::<Result<_>>()?;
            out = Some(match out {
                None => got,
                Some(prev) => prev
                    .into_iter()
                    .filter(|(s, _)| got.iter().any(|(g, _)| g == s))
                    .collect(),
            });
        }
        let mut v = out.unwrap_or_default();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        Ok(v.into_iter()
            .take(if limit > 0 { limit as usize } else { 200 })
            .map(|(s, _)| s)
            .collect())
    }

    /// Value counts of one fact kind across sessions (for filter facets).
    pub async fn session_fact_values(&self, kind: &str, limit: i64) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(self.sql(
            "SELECT value, COUNT(*) AS n FROM session_facts WHERE kind = ? \
             GROUP BY value ORDER BY n DESC, value LIMIT ?",
        ))
        .bind(kind)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("value")?, r.try_get("n")?)))
            .collect()
    }

    /// Drops a session's facts (the session was deleted).
    pub async fn session_facts_delete(&self, session_id: &str) -> Result<()> {
        let mut tx = self.begin_write().await?;
        for t in ["session_facts", "session_fact_cursor"] {
            sqlx::query(self.sql(&format!("DELETE FROM {t} WHERE session_id = ?")))
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---- events (queries beyond the M0 helpers) ---------------------------

    /// Events matching `q`, newest first.
    pub async fn events_query(&self, q: &EventQuery) -> Result<Vec<Event>> {
        let mut sql =
            String::from("SELECT id, at, kind, subject, data_json FROM events WHERE 1 = 1");
        let prefixes: Vec<&String> = q.prefixes.iter().filter(|p| !p.is_empty()).collect();
        if !prefixes.is_empty() {
            sql.push_str(" AND (");
            sql.push_str(&vec!["kind LIKE ?"; prefixes.len()].join(" OR "));
            sql.push(')');
        }
        if q.before_id.is_some() {
            sql.push_str(" AND id < ?");
        }
        if q.from_at.is_some() {
            sql.push_str(" AND at >= ?");
        }
        if q.to_at.is_some() {
            sql.push_str(" AND at < ?");
        }
        let text = q
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| format!("%{}%", t.to_lowercase().replace('%', "")));
        if text.is_some() {
            sql.push_str(
                " AND (LOWER(kind) LIKE ? OR LOWER(subject) LIKE ? OR LOWER(data_json) LIKE ?)",
            );
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        let mut query = sqlx::query(self.sql(&sql));
        for p in prefixes {
            query = query.bind(format!("{}%", p.replace('%', "")));
        }
        if let Some(b) = q.before_id {
            query = query.bind(b);
        }
        if let Some(f) = q.from_at {
            query = query.bind(f);
        }
        if let Some(t) = q.to_at {
            query = query.bind(t);
        }
        if let Some(t) = &text {
            query = query.bind(t.clone()).bind(t.clone()).bind(t.clone());
        }
        let limit = if q.limit > 0 { q.limit } else { 500 };
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        // `_` in a LIKE pattern matches any one character: keep exact prefixes only.
        let evs: Vec<Event> = rows.iter().map(event_row).collect::<Result<_>>()?;
        let wanted: Vec<&String> = q.prefixes.iter().filter(|p| !p.is_empty()).collect();
        Ok(evs
            .into_iter()
            .filter(|e| wanted.is_empty() || wanted.iter().any(|p| e.kind.starts_with(p.as_str())))
            .collect())
    }

    /// Events whose kind starts with `prefix`, newest first.
    pub async fn events_with_prefix(
        &self,
        prefix: &str,
        before_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Event>> {
        self.events_query(&EventQuery {
            prefixes: vec![prefix.to_string()],
            before_id,
            limit,
            ..Default::default()
        })
        .await
    }

    /// Deletes events older than `before_at` whose ids the audit fold has
    /// already passed (`max_id`), so nothing unledgered is lost. Returns
    /// how many went.
    pub async fn prune_events(&self, before_at: i64, max_id: i64) -> Result<u64> {
        let res = sqlx::query(self.sql("DELETE FROM events WHERE at < ? AND id <= ?"))
            .bind(before_at)
            .bind(max_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    // ---- the ledger ----------------------------------------------------------

    /// Appends one entry in `tx`. `None` when its ref_key is already chained.
    async fn ledger_append_tx(&self, tx: &mut Tx, d: &LedgerDraft) -> Result<Option<LedgerEntry>> {
        if let Some(r) = &d.ref_key {
            let seen = sqlx::query(self.sql("SELECT seq FROM audit_ledger WHERE ref_key = ?"))
                .bind(r)
                .fetch_optional(&mut **tx)
                .await?;
            if seen.is_some() {
                return Ok(None);
            }
        }
        let head =
            sqlx::query(self.sql("SELECT seq, hash FROM audit_ledger ORDER BY seq DESC LIMIT 1"))
                .fetch_optional(&mut **tx)
                .await?;
        let (seq, prev) = match head {
            Some(r) => (
                r.try_get::<i64, _>("seq")? + 1,
                r.try_get::<String, _>("hash")?,
            ),
            None => (1, String::new()),
        };
        let mut e = LedgerEntry {
            seq,
            at: if d.at > 0 { d.at } else { now_ms() },
            kind: d.kind.clone(),
            actor: d.actor.clone(),
            actor_kind: d.actor_kind.clone(),
            msg: d.msg.clone(),
            subject: obj_or_empty(&d.subject),
            payload: obj_or_empty(&d.payload),
            origin: if d.origin.is_empty() {
                "factory".into()
            } else {
                d.origin.clone()
            },
            node_id: d.node_id.clone(),
            session_id: d.session_id.clone(),
            ref_key: d.ref_key.clone(),
            prev_hash: prev,
            hash: String::new(),
        };
        e.hash = e.expected_hash();
        let q = format!(
            "INSERT INTO audit_ledger ({LEDGER_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        sqlx::query(self.sql(&q))
            .bind(e.seq)
            .bind(e.at)
            .bind(&e.kind)
            .bind(&e.actor)
            .bind(&e.actor_kind)
            .bind(&e.msg)
            .bind(canonical_json(&e.subject))
            .bind(canonical_json(&e.payload))
            .bind(&e.origin)
            .bind(&e.node_id)
            .bind(&e.session_id)
            .bind(e.ref_key.clone())
            .bind(&e.prev_hash)
            .bind(&e.hash)
            .execute(&mut **tx)
            .await?;
        Ok(Some(e))
    }

    /// Appends entries in order, in one transaction. Drafts whose ref_key
    /// is already chained are skipped; the result holds the new entries.
    pub async fn ledger_append_many(&self, drafts: &[LedgerDraft]) -> Result<Vec<LedgerEntry>> {
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = LEDGER_LOCK.lock().await;
        let mut attempt = 0;
        loop {
            let r: Result<Vec<LedgerEntry>> = async {
                let mut tx = self.begin_write().await?;
                let mut out = Vec::new();
                for d in drafts {
                    if let Some(e) = self.ledger_append_tx(&mut tx, d).await? {
                        out.push(e);
                    }
                }
                tx.commit().await?;
                Ok(out)
            }
            .await;
            match r {
                // Another process took the seq (Postgres shared by two apps).
                Err(e) if attempt < 5 && is_unique_violation(&e) => attempt += 1,
                other => return other,
            }
        }
    }

    /// Appends one entry. `None` when its ref_key is already chained.
    pub async fn ledger_append(&self, d: &LedgerDraft) -> Result<Option<LedgerEntry>> {
        Ok(self
            .ledger_append_many(std::slice::from_ref(d))
            .await?
            .pop())
    }

    pub async fn ledger_head(&self) -> Result<ChainHead> {
        let row =
            sqlx::query(self.sql("SELECT seq, hash FROM audit_ledger ORDER BY seq DESC LIMIT 1"))
                .fetch_optional(&self.pool)
                .await?;
        Ok(match row {
            Some(r) => ChainHead {
                seq: r.try_get("seq")?,
                hash: r.try_get("hash")?,
            },
            None => ChainHead::default(),
        })
    }

    pub async fn ledger_has_ref(&self, ref_key: &str) -> Result<bool> {
        Ok(
            sqlx::query(self.sql("SELECT seq FROM audit_ledger WHERE ref_key = ?"))
                .bind(ref_key)
                .fetch_optional(&self.pool)
                .await?
                .is_some(),
        )
    }

    pub async fn ledger_entry(&self, seq: i64) -> Result<Option<LedgerEntry>> {
        let q = format!("SELECT {LEDGER_COLS} FROM audit_ledger WHERE seq = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(seq)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(ledger_from_row).transpose()
    }

    fn ledger_where(q: &LedgerQuery) -> (String, Vec<Arg>) {
        let mut w = String::from(" WHERE 1 = 1");
        let mut args = Vec::new();
        let kinds: Vec<&String> = q.kinds.iter().filter(|k| !k.is_empty()).collect();
        if !kinds.is_empty() {
            w.push_str(" AND kind IN (");
            for (i, k) in kinds.iter().enumerate() {
                w.push_str(if i == 0 { "?" } else { ", ?" });
                args.push(Arg::S((*k).clone()));
            }
            w.push(')');
        }
        for (col, v) in [("session_id", &q.session_id), ("node_id", &q.node_id)] {
            if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
                w.push_str(&format!(" AND {col} = ?"));
                args.push(Arg::S(v.clone()));
            }
        }
        match q.origin.as_deref() {
            None | Some("") => {}
            Some("host") => w.push_str(" AND origin <> 'factory'"),
            Some(o) => {
                w.push_str(" AND origin = ?");
                args.push(Arg::S(o.into()));
            }
        }
        if let Some(t) = q.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            w.push_str(
                " AND (LOWER(msg) LIKE ? OR LOWER(kind) LIKE ? OR LOWER(actor) LIKE ? \
                 OR LOWER(subject_json) LIKE ?)",
            );
            let pat = format!("%{}%", t.to_lowercase());
            args.extend(std::iter::repeat_n(Arg::S(pat), 4));
        }
        for f in q.facts.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
            w.push_str(
                " AND (LOWER(subject_json) LIKE ? OR LOWER(payload_json) LIKE ? OR LOWER(msg) LIKE ?)",
            );
            let pat = format!("%{}%", f.to_lowercase().replace('%', ""));
            args.extend(std::iter::repeat_n(Arg::S(pat), 3));
        }
        for (cond, v) in [
            (" AND at >= ?", q.from_at),
            (" AND at < ?", q.to_at),
            (" AND seq < ?", q.before_seq),
            (" AND seq > ?", q.after_seq),
        ] {
            if let Some(v) = v {
                w.push_str(cond);
                args.push(Arg::I(v));
            }
        }
        (w, args)
    }

    /// Ledger entries matching `q`, newest first unless `q.ascending`.
    pub async fn ledger(&self, q: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        let (w, args) = Self::ledger_where(q);
        let order = if q.ascending { "ASC" } else { "DESC" };
        let sql = format!("SELECT {LEDGER_COLS} FROM audit_ledger{w} ORDER BY seq {order} LIMIT ?");
        let mut query = sqlx::query(self.sql(&sql));
        for a in args {
            query = match a {
                Arg::I(i) => query.bind(i),
                Arg::S(s) => query.bind(s),
            };
        }
        let limit = match q.limit {
            n if n <= 0 => 200,
            n => n.min(10_000),
        };
        let rows = query.bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(ledger_from_row).collect()
    }

    /// How many entries match `q` (limit and order ignored).
    pub async fn ledger_count(&self, q: &LedgerQuery) -> Result<i64> {
        let (w, args) = Self::ledger_where(q);
        let sql = format!("SELECT COUNT(*) AS n FROM audit_ledger{w}");
        let mut query = sqlx::query(self.sql(&sql));
        for a in args {
            query = match a {
                Arg::I(i) => query.bind(i),
                Arg::S(s) => query.bind(s),
            };
        }
        Ok(query.fetch_one(&self.pool).await?.try_get("n")?)
    }

    /// Entry counts by kind, most common first.
    pub async fn ledger_kinds(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query(self.sql(
            "SELECT kind, COUNT(*) AS n FROM audit_ledger GROUP BY kind ORDER BY n DESC, kind",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("kind")?, r.try_get("n")?)))
            .collect()
    }

    /// Recomputes the whole chain from the first entry.
    pub async fn ledger_verify(&self) -> Result<ChainReport> {
        const PAGE: i64 = 2000;
        let mut report = ChainReport {
            chain: "factory".into(),
            ok: true,
            ..Default::default()
        };
        let mut prev = String::new();
        let mut want_seq = 1;
        let mut index = 0;
        let mut after = 0;
        loop {
            let q = format!(
                "SELECT {LEDGER_COLS} FROM audit_ledger WHERE seq > ? ORDER BY seq LIMIT ?"
            );
            let rows = sqlx::query(self.sql(&q))
                .bind(after)
                .bind(PAGE)
                .fetch_all(&self.pool)
                .await?;
            if rows.is_empty() {
                break;
            }
            for r in &rows {
                let e = ledger_from_row(r)?;
                after = e.seq;
                let broken = if e.seq != want_seq {
                    Some(BrokenAt {
                        seq: want_seq,
                        index,
                        expected: format!("seq {want_seq}"),
                        found: format!("seq {}", e.seq),
                        reason: "gap".into(),
                    })
                } else if e.prev_hash != prev {
                    Some(BrokenAt {
                        seq: e.seq,
                        index,
                        expected: prev.clone(),
                        found: e.prev_hash.clone(),
                        reason: "prev_hash".into(),
                    })
                } else {
                    let want = e.expected_hash();
                    (want != e.hash).then(|| BrokenAt {
                        seq: e.seq,
                        index,
                        expected: want,
                        found: e.hash.clone(),
                        reason: "hash".into(),
                    })
                };
                report.entries += 1;
                if report.broken_at.is_none()
                    && let Some(b) = broken
                {
                    report.ok = false;
                    report.broken_at = Some(b);
                }
                prev = e.hash.clone();
                want_seq = e.seq + 1;
                index += 1;
            }
        }
        report.head = prev;
        Ok(report)
    }

    // ---- host audit lines, as the ledger folds them in ----------------------

    /// Audit lines received at or after `from_received`, ordered by
    /// (received_at, node, session, index) and strictly after the key
    /// `after` when given (keyset paging), at most `limit`.
    pub async fn audit_lines_received_since(
        &self,
        from_received: i64,
        after: Option<&sessions::AuditLine>,
        limit: i64,
    ) -> Result<Vec<sessions::AuditLine>> {
        const COLS: &str =
            "SELECT node_id, session_id, idx, at, kind, json, received_at FROM audit_lines";
        const ORDER: &str = " ORDER BY received_at, node_id, session_id, idx LIMIT ?";
        let query = match after {
            None => {
                let q = format!("{COLS} WHERE received_at >= ?{ORDER}");
                sqlx::query(self.sql(&q)).bind(from_received)
            }
            Some(a) => {
                let q = format!(
                    "{COLS} WHERE received_at >= ? AND (received_at > ? OR (received_at = ? AND \
                     (node_id > ? OR (node_id = ? AND (session_id > ? OR (session_id = ? AND idx > ?))))))\
                     {ORDER}"
                );
                sqlx::query(self.sql(&q))
                    .bind(from_received)
                    .bind(a.received_at)
                    .bind(a.received_at)
                    .bind(a.node_id.clone())
                    .bind(a.node_id.clone())
                    .bind(a.session_id.clone())
                    .bind(a.session_id.clone())
                    .bind(a.idx)
            }
        };
        let rows = query.bind(limit.max(1)).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(sessions::AuditLine {
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

    /// Every (node, session) that has host audit lines, with its line count.
    pub async fn audit_line_streams(&self) -> Result<Vec<(String, String, i64)>> {
        let rows = sqlx::query(self.sql(
            "SELECT node_id, session_id, COUNT(*) AS n FROM audit_lines \
             GROUP BY node_id, session_id ORDER BY node_id, session_id",
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("node_id")?,
                    r.try_get("session_id")?,
                    r.try_get("n")?,
                ))
            })
            .collect()
    }

    // ---- session deletion ----------------------------------------------------

    /// Deletes a session's mirrored data (metadata, transcript, inbox, raw
    /// audit lines) and appends `tombstone` to the chain in the same
    /// transaction. `None` when there is no such session.
    pub async fn delete_session(
        &self,
        id: &str,
        tombstone: &LedgerDraft,
    ) -> Result<Option<(LedgerEntry, SessionPurge)>> {
        let _guard = LEDGER_LOCK.lock().await;
        let mut tx = self.begin_write().await?;
        let gone = sqlx::query(self.sql("DELETE FROM sessions WHERE id = ?"))
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if gone == 0 {
            return Ok(None);
        }
        let mut n = [0u64; 3];
        for (i, t) in ["session_entries", "session_inbox", "audit_lines"]
            .iter()
            .enumerate()
        {
            let q = format!("DELETE FROM {t} WHERE session_id = ?");
            n[i] = sqlx::query(self.sql(&q))
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        let entry = self
            .ledger_append_tx(&mut tx, tombstone)
            .await?
            .context("tombstone ref_key already chained")?;
        tx.commit().await?;
        Ok(Some((
            entry,
            SessionPurge {
                entries: n[0],
                inbox: n[1],
                audit_lines: n[2],
            },
        )))
    }

    // ---- settings document ---------------------------------------------------

    /// Every stored setting, by key.
    pub async fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(self.sql("SELECT key, value FROM settings ORDER BY key"))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("key")?, r.try_get("value")?)))
            .collect()
    }

    pub async fn settings_revision(&self) -> Result<i64> {
        Ok(self
            .setting(SETTINGS_REVISION)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Writes `changes` (a `None` value removes the key, so it reads as its
    /// default) and chains `entries`, in one transaction, if the document is
    /// still at revision `expected`. Bumps the revision.
    pub async fn save_settings(
        &self,
        expected: i64,
        changes: &[(String, Option<String>)],
        entries: &[LedgerDraft],
    ) -> Result<SettingsWrite> {
        let _guard = LEDGER_LOCK.lock().await;
        let mut tx = self.begin_write().await?;
        let current = sqlx::query(self.sql("SELECT value FROM settings WHERE key = ?"))
            .bind(SETTINGS_REVISION)
            .fetch_optional(&mut *tx)
            .await?
            .and_then(|r| r.try_get::<String, _>("value").ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if current != expected {
            return Ok(SettingsWrite::Conflict { current });
        }
        let now = now_ms();
        let upsert = self.sql(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        );
        for (k, v) in changes {
            match v {
                Some(v) => {
                    sqlx::query(AssertSqlSafe(upsert.0.clone()))
                        .bind(k)
                        .bind(v)
                        .bind(now)
                        .execute(&mut *tx)
                        .await?;
                }
                None => {
                    sqlx::query(self.sql("DELETE FROM settings WHERE key = ?"))
                        .bind(k)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        let revision = current + 1;
        sqlx::query(AssertSqlSafe(upsert.0.clone()))
            .bind(SETTINGS_REVISION)
            .bind(revision.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        let mut out = Vec::new();
        for d in entries {
            if let Some(e) = self.ledger_append_tx(&mut tx, d).await? {
                out.push(e);
            }
        }
        tx.commit().await?;
        Ok(SettingsWrite::Saved {
            revision,
            entries: out,
        })
    }

    // ---- teleports -----------------------------------------------------------

    pub async fn insert_teleport(&self, t: &Teleport) -> Result<()> {
        let q = format!(
            "INSERT INTO teleports ({TELEPORT_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        let now = now_ms();
        sqlx::query(self.sql(&q))
            .bind(&t.id)
            .bind(&t.session_id)
            .bind(&t.from_node)
            .bind(&t.to_node)
            .bind(&t.title)
            .bind(if t.status.is_empty() {
                "requested"
            } else {
                t.status.as_str()
            })
            .bind(t.size)
            .bind(&t.sha256)
            .bind(&t.error)
            .bind(if t.created_at > 0 { t.created_at } else { now })
            .bind(now)
            .bind(t.uploaded_at)
            .bind(t.downloaded_at)
            .bind(t.expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn teleport(&self, id: &str) -> Result<Option<Teleport>> {
        let q = format!("SELECT {TELEPORT_COLS} FROM teleports WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(teleport_from_row).transpose()
    }

    /// Most recent first.
    pub async fn teleports(&self, limit: i64) -> Result<Vec<Teleport>> {
        let q =
            format!("SELECT {TELEPORT_COLS} FROM teleports ORDER BY created_at DESC, id LIMIT ?");
        let rows = sqlx::query(self.sql(&q))
            .bind(if limit > 0 { limit } else { 200 })
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(teleport_from_row).collect()
    }

    /// A requested teleport received its bundle. False unless it was
    /// `requested`. `title` fills an empty title.
    pub async fn teleport_ready(
        &self,
        id: &str,
        size: i64,
        sha256: &str,
        expires_at: i64,
        title: &str,
    ) -> Result<bool> {
        let now = now_ms();
        let r = sqlx::query(self.sql(
            "UPDATE teleports SET status = 'ready', \
             title = CASE WHEN title = '' THEN ? ELSE title END, size = ?, sha256 = ?, uploaded_at = ?, \
             updated_at = ?, expires_at = ?, error = '' WHERE id = ? AND status = 'requested'",
        ))
        .bind(title)
        .bind(size)
        .bind(sha256)
        .bind(now)
        .bind(now)
        .bind(expires_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// The addressed node downloaded the whole bundle (again, possibly).
    pub async fn teleport_delivered(&self, id: &str) -> Result<bool> {
        let now = now_ms();
        let r = sqlx::query(self.sql(
            "UPDATE teleports SET status = 'delivered', downloaded_at = ?, updated_at = ? \
             WHERE id = ? AND status IN ('ready', 'delivered')",
        ))
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Marks a teleport that has not delivered yet as failed.
    pub async fn teleport_failed(&self, id: &str, error: &str) -> Result<bool> {
        let r = sqlx::query(self.sql(
            "UPDATE teleports SET status = 'failed', error = ?, updated_at = ? \
             WHERE id = ? AND status IN ('requested', 'ready')",
        ))
        .bind(error)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Expires every live teleport whose `expires_at` has passed and
    /// returns them.
    pub async fn expire_teleports(&self, now: i64) -> Result<Vec<Teleport>> {
        let q = format!(
            "SELECT {TELEPORT_COLS} FROM teleports WHERE status IN ('requested', 'ready', 'delivered') \
             AND expires_at > 0 AND expires_at <= ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        let due: Vec<Teleport> = rows.iter().map(teleport_from_row).collect::<Result<_>>()?;
        let mut out = Vec::new();
        for mut t in due {
            let r = sqlx::query(self.sql(
                "UPDATE teleports SET status = 'expired', updated_at = ? WHERE id = ? \
                 AND status IN ('requested', 'ready', 'delivered')",
            ))
            .bind(now)
            .bind(&t.id)
            .execute(&self.pool)
            .await?;
            if r.rows_affected() == 1 {
                t.status = "expired".into();
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Whether a live teleport still needs the blob `sha256`.
    pub async fn teleport_blob_in_use(&self, sha256: &str) -> Result<bool> {
        Ok(sqlx::query(self.sql(
            "SELECT id FROM teleports WHERE sha256 = ? AND status IN ('ready', 'delivered') LIMIT 1",
        ))
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?
        .is_some())
    }

    // ---- facts for the Trust page (read-only) --------------------------------

    /// Managed repositories: (full name, archived).
    pub async fn trust_managed_repos(&self) -> Result<Vec<(String, bool)>> {
        let rows =
            sqlx::query(self.sql(
                "SELECT full_name, archived FROM gh_repos WHERE managed = 1 ORDER BY full_name",
            ))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("full_name")?,
                    r.try_get::<i64, _>("archived")? != 0,
                ))
            })
            .collect()
    }

    async fn repo_counts(&self, q: &str) -> Result<Vec<RepoCounts>> {
        let rows = sqlx::query(self.sql(q)).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(RepoCounts {
                    repo: r.try_get("repo")?,
                    total: r.try_get("total")?,
                    good: r.try_get("good")?,
                    bad: r.try_get("bad")?,
                })
            })
            .collect()
    }

    /// Sessions per repo; `good` counts those linked to a run or a card.
    pub async fn trust_session_counts(&self) -> Result<Vec<RepoCounts>> {
        self.repo_counts(
            "SELECT repo, COUNT(*) AS total, \
             SUM(CASE WHEN run_id <> '' OR card_id <> '' THEN 1 ELSE 0 END) AS good, \
             0 AS bad FROM sessions WHERE repo <> '' GROUP BY repo",
        )
        .await
    }

    /// Runs per repo; `good` counts those linked to a card.
    pub async fn trust_run_counts(&self) -> Result<Vec<RepoCounts>> {
        self.repo_counts(
            "SELECT repo, COUNT(*) AS total, \
             SUM(CASE WHEN card_id <> '' THEN 1 ELSE 0 END) AS good, \
             0 AS bad FROM runs WHERE repo <> '' GROUP BY repo",
        )
        .await
    }

    /// Commits per repo; `good` signed, `bad` recorded as unsigned.
    pub async fn trust_commit_counts(&self) -> Result<Vec<RepoCounts>> {
        self.repo_counts(
            "SELECT repo, COUNT(*) AS total, \
             SUM(CASE WHEN signed = 1 THEN 1 ELSE 0 END) AS good, \
             SUM(CASE WHEN signed = 0 THEN 1 ELSE 0 END) AS bad \
             FROM delivery_commits GROUP BY repo",
        )
        .await
    }

    /// Conflict resolutions per repo; `good` verified by GitHub.
    pub async fn trust_resolution_counts(&self) -> Result<Vec<RepoCounts>> {
        self.repo_counts(
            "SELECT repo, COUNT(*) AS total, \
             SUM(CASE WHEN verified = 1 THEN 1 ELSE 0 END) AS good, \
             SUM(CASE WHEN verified = 0 THEN 1 ELSE 0 END) AS bad \
             FROM gh_resolutions GROUP BY repo",
        )
        .await
    }

    /// Host audit lines of the given kinds at or after `since`, with their
    /// session's repository (sessions without one are left out).
    pub async fn trust_repo_lines(&self, kinds: &[&str], since: i64) -> Result<Vec<RepoLine>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; kinds.len()].join(", ");
        let q = format!(
            "SELECT s.repo AS repo, l.kind AS kind, l.json AS json, l.at AS at FROM audit_lines l \
             JOIN sessions s ON s.id = l.session_id AND s.node_id = l.node_id \
             WHERE s.repo <> '' AND l.at >= ? AND l.kind IN ({marks}) ORDER BY l.at"
        );
        let mut query = sqlx::query(self.sql(&q)).bind(since);
        for k in kinds {
            query = query.bind(*k);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(RepoLine {
                    repo: r.try_get("repo")?,
                    kind: r.try_get("kind")?,
                    json: r.try_get("json")?,
                    at: r.try_get("at")?,
                })
            })
            .collect()
    }

    /// Mirrored pull requests updated at or after `since`:
    /// (repo, checks_state, checks_json, updated_at).
    pub async fn trust_pull_checks(
        &self,
        since: i64,
    ) -> Result<Vec<(String, String, String, i64)>> {
        let rows = sqlx::query(self.sql(
            "SELECT repo, checks_state, checks_json, updated_at FROM gh_pulls \
             WHERE updated_at >= ? ORDER BY updated_at",
        ))
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("repo")?,
                    r.try_get("checks_state")?,
                    r.try_get("checks_json")?,
                    r.try_get("updated_at")?,
                ))
            })
            .collect()
    }

    /// Host audit line counts per (node, kind) for the given kinds since `since`.
    pub async fn audit_line_counts(
        &self,
        kinds: &[&str],
        since: i64,
    ) -> Result<Vec<(String, String, i64)>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; kinds.len()].join(", ");
        let q = format!(
            "SELECT node_id, kind, COUNT(*) AS n FROM audit_lines WHERE at >= ? AND kind IN ({marks}) \
             GROUP BY node_id, kind ORDER BY node_id, kind"
        );
        let mut query = sqlx::query(self.sql(&q)).bind(since);
        for k in kinds {
            query = query.bind(*k);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| Ok((r.try_get("node_id")?, r.try_get("kind")?, r.try_get("n")?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{AuditLine, Session};

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut out = vec![(s, Some(dir))];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            let s = Store::open(&url).await.unwrap();
            // TRUNCATE skips the append-only row triggers.
            for t in [
                "audit_ledger",
                "teleports",
                "sessions",
                "session_entries",
                "session_inbox",
                "audit_lines",
                "settings",
                "gh_repos",
                "gh_pulls",
                "gh_resolutions",
                "delivery_commits",
                "runs",
            ] {
                sqlx::query(AssertSqlSafe(if t == "audit_ledger" {
                    format!("TRUNCATE {t}")
                } else {
                    format!("DELETE FROM {t}")
                }))
                .execute(&s.pool)
                .await
                .unwrap();
            }
            out.push((s, None));
        }
        out
    }

    fn draft(kind: &str, msg: &str) -> LedgerDraft {
        LedgerDraft {
            kind: kind.into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: msg.into(),
            subject: json!({ "setting": "ui.theme" }),
            payload: json!({ "old": "dark", "new": "light", "n": 1.5 }),
            ..Default::default()
        }
    }

    /// Lets a test play the attacker with write access to the database.
    async fn allow_tamper(s: &Store) {
        let stmts: &[&str] = match s.dialect() {
            Dialect::Sqlite => &[
                "DROP TRIGGER audit_ledger_no_update",
                "DROP TRIGGER audit_ledger_no_delete",
            ],
            Dialect::Postgres => {
                &["ALTER TABLE audit_ledger DISABLE TRIGGER audit_ledger_no_change"]
            }
        };
        for q in stmts {
            sqlx::query(AssertSqlSafe(q.to_string()))
                .execute(&s.pool)
                .await
                .unwrap();
        }
    }

    async fn restore_triggers(s: &Store) {
        if s.dialect() == Dialect::Postgres {
            sqlx::query("ALTER TABLE audit_ledger ENABLE TRIGGER audit_ledger_no_change")
                .execute(&s.pool)
                .await
                .unwrap();
        }
    }

    #[test]
    fn canonical_sorts_keys_without_whitespace() {
        let v = json!({ "b": [1, { "z": true, "a": null }], "a": "x\"y", "c": 2.5 });
        assert_eq!(
            canonical_json(&v),
            r#"{"a":"x\"y","b":[1,{"a":null,"z":true}],"c":2.5}"#
        );
        // Golden vector: pins the chain format.
        assert_eq!(
            chain_hash("", "{}"),
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[tokio::test]
    async fn append_chains_and_verifies() {
        for (s, _d) in stores().await {
            assert_eq!(s.ledger_head().await.unwrap(), ChainHead::default());
            let r = s.ledger_verify().await.unwrap();
            assert!(r.ok && r.entries == 0);

            let a = s
                .ledger_append(&draft("setting", "a"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!((a.seq, a.prev_hash.as_str()), (1, ""));
            assert!(a.hash.starts_with("sha256:"));
            let many = s
                .ledger_append_many(&[draft("approval", "b"), draft("dispatch", "c")])
                .await
                .unwrap();
            assert_eq!(many.len(), 2);
            assert_eq!(many[0].prev_hash, a.hash);
            assert_eq!(many[1].prev_hash, many[0].hash);
            assert_eq!(s.ledger_head().await.unwrap().seq, 3);
            let stored = s.ledger_entry(2).await.unwrap().unwrap();
            assert_eq!(stored, many[0]);

            let r = s.ledger_verify().await.unwrap();
            assert!(r.ok, "{r:?}");
            assert_eq!(r.entries, 3);
            assert_eq!(r.head, many[1].hash);
        }
    }

    #[tokio::test]
    async fn ref_key_makes_folding_idempotent() {
        for (s, _d) in stores().await {
            let mut d = draft("host", "line");
            d.ref_key = Some("line:n1:s1:0".into());
            assert!(s.ledger_append(&d).await.unwrap().is_some());
            assert!(s.ledger_append(&d).await.unwrap().is_none());
            assert!(s.ledger_has_ref("line:n1:s1:0").await.unwrap());
            assert!(!s.ledger_has_ref("line:n1:s1:1").await.unwrap());
            assert_eq!(s.ledger_head().await.unwrap().seq, 1);
        }
    }

    #[tokio::test]
    async fn concurrent_appends_keep_one_chain() {
        for (s, _d) in stores().await {
            let mut tasks = Vec::new();
            for i in 0..8 {
                let s = s.clone();
                tasks.push(tokio::spawn(async move {
                    s.ledger_append(&draft("dispatch", &format!("d{i}")))
                        .await
                        .unwrap()
                }));
            }
            for t in tasks {
                t.await.unwrap();
            }
            let r = s.ledger_verify().await.unwrap();
            assert!(r.ok && r.entries == 8, "{r:?}");
        }
    }

    #[tokio::test]
    async fn ledger_is_append_only() {
        for (s, _d) in stores().await {
            s.ledger_append(&draft("setting", "a")).await.unwrap();
            assert!(
                sqlx::query("UPDATE audit_ledger SET msg = 'x'")
                    .execute(&s.pool)
                    .await
                    .is_err()
            );
            assert!(
                sqlx::query("DELETE FROM audit_ledger")
                    .execute(&s.pool)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn verify_detects_tampering() {
        for (s, _d) in stores().await {
            for i in 0..5 {
                s.ledger_append(&draft("setting", &format!("e{i}")))
                    .await
                    .unwrap();
            }
            allow_tamper(&s).await;

            // An edited payload breaks that entry's hash.
            sqlx::query(s.sql("UPDATE audit_ledger SET payload_json = ? WHERE seq = 3"))
                .bind(r#"{"new":"evil","n":1.5,"old":"dark"}"#)
                .execute(&s.pool)
                .await
                .unwrap();
            let r = s.ledger_verify().await.unwrap();
            assert!(!r.ok);
            let b = r.broken_at.unwrap();
            assert_eq!((b.seq, b.index, b.reason.as_str()), (3, 2, "hash"));

            // Re-hashing the edited entry moves the break to the next link.
            let e3 = s.ledger_entry(3).await.unwrap().unwrap();
            sqlx::query(s.sql("UPDATE audit_ledger SET hash = ? WHERE seq = 3"))
                .bind(e3.expected_hash())
                .execute(&s.pool)
                .await
                .unwrap();
            let b = s.ledger_verify().await.unwrap().broken_at.unwrap();
            assert_eq!((b.seq, b.reason.as_str()), (4, "prev_hash"));

            // A removed entry is a gap.
            sqlx::query("DELETE FROM audit_ledger WHERE seq >= 3")
                .execute(&s.pool)
                .await
                .unwrap();
            s.ledger_append(&draft("setting", "after")).await.unwrap();
            assert!(s.ledger_verify().await.unwrap().ok);
            sqlx::query("DELETE FROM audit_ledger WHERE seq = 2")
                .execute(&s.pool)
                .await
                .unwrap();
            let b = s.ledger_verify().await.unwrap().broken_at.unwrap();
            assert_eq!((b.seq, b.reason.as_str()), (2, "gap"));
            restore_triggers(&s).await;
        }
    }

    #[tokio::test]
    async fn query_filters() {
        for (s, _d) in stores().await {
            let mut h = draft("gate", "sca passed");
            h.origin = "host:n1".into();
            h.node_id = "n1".into();
            h.session_id = "s1".into();
            s.ledger_append_many(&[draft("setting", "theme"), h, draft("approval", "ok")])
                .await
                .unwrap();
            let all = s.ledger(&LedgerQuery::default()).await.unwrap();
            assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [3, 2, 1]);
            let q = |f: LedgerQuery| {
                let s = s.clone();
                async move { s.ledger(&f).await.unwrap() }
            };
            let got = q(LedgerQuery {
                kinds: vec!["gate".into(), "approval".into()],
                ascending: true,
                ..Default::default()
            })
            .await;
            assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), [2, 3]);
            assert_eq!(
                q(LedgerQuery {
                    origin: Some("host".into()),
                    ..Default::default()
                })
                .await
                .len(),
                1
            );
            assert_eq!(
                q(LedgerQuery {
                    session_id: Some("s1".into()),
                    ..Default::default()
                })
                .await[0]
                    .seq,
                2
            );
            assert_eq!(
                q(LedgerQuery {
                    text: Some("SCA PASSED".into()),
                    ..Default::default()
                })
                .await
                .len(),
                1
            );
            assert_eq!(
                q(LedgerQuery {
                    before_seq: Some(3),
                    limit: 1,
                    ..Default::default()
                })
                .await[0]
                    .seq,
                2
            );
            let n = s
                .ledger_count(&LedgerQuery {
                    after_seq: Some(1),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(n, 2);
            // Fact filters match the message (and subject/payload), all of them.
            assert_eq!(
                q(LedgerQuery {
                    facts: vec!["sca".into()],
                    ..Default::default()
                })
                .await
                .len(),
                1
            );
            assert!(
                q(LedgerQuery {
                    facts: vec!["sca".into(), "nope".into()],
                    ..Default::default()
                })
                .await
                .is_empty()
            );
            let kinds = s.ledger_kinds().await.unwrap();
            assert_eq!(kinds.len(), 3);
        }
    }

    #[tokio::test]
    async fn delete_session_leaves_tombstone() {
        for (s, _d) in stores().await {
            s.upsert_session(&Session {
                id: "s1".into(),
                node_id: "n1".into(),
                title: "t".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            s.append_entries(
                "s1",
                &[crate::sessions::Entry {
                    idx: 0,
                    role: "user".into(),
                    text: "hi".into(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
            s.append_audit_lines(
                "n1",
                "s1",
                &[AuditLine {
                    idx: 0,
                    kind: "tool".into(),
                    json: "{}".into(),
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
            let mut t = draft("session_deleted", "deleted s1");
            t.session_id = "s1".into();
            let (e, purge) = s.delete_session("s1", &t).await.unwrap().unwrap();
            assert_eq!(e.kind, "session_deleted");
            assert_eq!(
                purge,
                SessionPurge {
                    entries: 1,
                    inbox: 0,
                    audit_lines: 1
                }
            );
            assert!(s.session("s1").await.unwrap().is_none());
            assert!(s.entries("s1", 0, 0).await.unwrap().is_empty());
            assert!(s.audit_lines("n1", "s1", 0, 0).await.unwrap().is_empty());
            assert!(s.delete_session("s1", &t).await.unwrap().is_none());
            assert!(s.ledger_verify().await.unwrap().ok);
        }
    }

    #[tokio::test]
    async fn settings_revision_check() {
        for (s, _d) in stores().await {
            assert_eq!(s.settings_revision().await.unwrap(), 0);
            let w = s
                .save_settings(
                    0,
                    &[
                        ("ui.theme".into(), Some("light".into())),
                        ("github.poll".into(), None),
                    ],
                    &[draft("setting", "ui.theme dark to light")],
                )
                .await
                .unwrap();
            let SettingsWrite::Saved { revision, entries } = w else {
                panic!("conflict")
            };
            assert_eq!((revision, entries.len()), (1, 1));
            assert_eq!(
                s.setting("ui.theme").await.unwrap().as_deref(),
                Some("light")
            );
            assert_eq!(
                s.save_settings(0, &[("ui.theme".into(), Some("dark".into()))], &[])
                    .await
                    .unwrap(),
                SettingsWrite::Conflict { current: 1 }
            );
            assert_eq!(
                s.setting("ui.theme").await.unwrap().as_deref(),
                Some("light")
            );
            s.save_settings(1, &[("ui.theme".into(), None)], &[])
                .await
                .unwrap();
            assert_eq!(s.setting("ui.theme").await.unwrap(), None);
            assert_eq!(s.settings_revision().await.unwrap(), 2);
            let all = s.all_settings().await.unwrap();
            assert!(all.iter().any(|(k, v)| k == SETTINGS_REVISION && v == "2"));
        }
    }

    #[tokio::test]
    async fn teleport_lifecycle() {
        for (s, _d) in stores().await {
            let t = Teleport {
                id: "t1".into(),
                session_id: "s1".into(),
                from_node: "n1".into(),
                to_node: "n2".into(),
                expires_at: 1_000,
                ..Default::default()
            };
            s.insert_teleport(&t).await.unwrap();
            let got = s.teleport("t1").await.unwrap().unwrap();
            assert_eq!(got.status, "requested");
            assert!(!s.teleport_delivered("t1").await.unwrap());
            assert!(s.teleport_ready("t1", 10, "ab", 5_000, "").await.unwrap());
            assert!(!s.teleport_ready("t1", 10, "ab", 5_000, "").await.unwrap());
            assert!(s.teleport_blob_in_use("ab").await.unwrap());
            assert!(s.teleport_delivered("t1").await.unwrap());
            assert!(s.teleport_delivered("t1").await.unwrap());
            assert!(!s.teleport_failed("t1", "late").await.unwrap());
            assert!(s.expire_teleports(4_999).await.unwrap().is_empty());
            let gone = s.expire_teleports(5_000).await.unwrap();
            assert_eq!(gone.len(), 1);
            assert_eq!(gone[0].status, "expired");
            assert!(!s.teleport_blob_in_use("ab").await.unwrap());

            s.insert_teleport(&Teleport {
                id: "t2".into(),
                from_node: "n1".into(),
                to_node: "n2".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            assert!(s.teleport_failed("t2", "origin offline").await.unwrap());
            let list = s.teleports(10).await.unwrap();
            assert_eq!(list.len(), 2);
            assert!(list.iter().any(|t| t.error == "origin offline"));
        }
    }

    #[tokio::test]
    async fn line_and_trust_queries() {
        for (s, _d) in stores().await {
            s.upsert_session(&Session {
                id: "s1".into(),
                node_id: "n1".into(),
                repo: "acme/api".into(),
                run_id: "r1".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            s.upsert_session(&Session {
                id: "s2".into(),
                node_id: "n1".into(),
                repo: "acme/api".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            let line = |idx, kind: &str| AuditLine {
                idx,
                at: 100 + idx,
                kind: kind.into(),
                json: r#"{"data":{"gate":"sca","exit":0}}"#.into(),
                ..Default::default()
            };
            s.append_audit_lines(
                "n1",
                "s1",
                &[line(0, "gate.verified"), line(1, "hazard.denied")],
            )
            .await
            .unwrap();
            let since = s.audit_lines_received_since(0, None, 10).await.unwrap();
            assert_eq!(since.len(), 2);
            let page = s.audit_lines_received_since(0, None, 1).await.unwrap();
            assert_eq!(page[0].idx, 0);
            let next = s
                .audit_lines_received_since(0, Some(&page[0]), 10)
                .await
                .unwrap();
            assert_eq!(next.iter().map(|l| l.idx).collect::<Vec<_>>(), [1]);
            assert!(
                s.audit_lines_received_since(now_ms() + 60_000, None, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                s.audit_line_streams().await.unwrap(),
                vec![("n1".into(), "s1".into(), 2)]
            );
            let gates = s.trust_repo_lines(&["gate.verified"], 0).await.unwrap();
            assert_eq!(gates.len(), 1);
            assert_eq!(gates[0].repo, "acme/api");
            let counts = s
                .audit_line_counts(&["hazard.denied", "egress.denied"], 0)
                .await
                .unwrap();
            assert_eq!(counts, vec![("n1".into(), "hazard.denied".into(), 1)]);
            let sc = s.trust_session_counts().await.unwrap();
            assert_eq!((sc[0].total, sc[0].good), (2, 1));
            assert!(s.trust_run_counts().await.unwrap().is_empty());
            assert!(s.trust_commit_counts().await.unwrap().is_empty());
            assert!(s.trust_resolution_counts().await.unwrap().is_empty());
            assert!(s.trust_managed_repos().await.unwrap().is_empty());
            assert!(s.trust_pull_checks(0).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn events_query_pages_filters_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let a = s
            .append_event("vault.secret_saved", "k1", r#"{"provider":"openai"}"#)
            .await
            .unwrap();
        let b = s
            .append_event("run.finished", "r1", r#"{"status":"failed"}"#)
            .await
            .unwrap();
        let c = s.append_event("vault.granted", "g1", "{}").await.unwrap();
        let v = s.events_with_prefix("vault.", None, 10).await.unwrap();
        assert_eq!(v.iter().map(|e| e.id).collect::<Vec<_>>(), [c.id, a.id]);
        let older = s
            .events_with_prefix("vault.", Some(c.id), 10)
            .await
            .unwrap();
        assert_eq!(older.len(), 1);
        let t = s
            .events_query(&EventQuery {
                text: Some("FAILED".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(t[0].id, b.id);
        let all = s.events_query(&EventQuery::default()).await.unwrap();
        assert_eq!(all.len(), 3);
        // Only events at or below the folded cursor go.
        assert_eq!(s.prune_events(i64::MAX, b.id).await.unwrap(), 2);
        assert_eq!(
            s.events_query(&EventQuery::default()).await.unwrap().len(),
            1
        );
    }
}
