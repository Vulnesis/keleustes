//! Domain: the hash-chained audit ledger, session deletion, teleports, and
//! (in submodules) Trust and the settings document. Persistence is
//! `kls_store::audit`. Owner: M6 finish workstream.
//!
//! The ledger is built from two sources and from direct records:
//!
//! - Keleustes' own significant events (vault grants and revocations,
//!   approvals, dispatches, the kill switch and A2A blackout, node joins and
//!   revocations, merges, resolutions, teleports...), folded from the
//!   `events` table by [`Core::audit_sync`];
//! - host audit lines from belai (`Sessions.Audit`, M1's `audit_lines`),
//!   folded the same way, one ledger entry per line;
//! - settings saves and session deletions, written in the same transaction
//!   as the change itself.
//!
//! Folding is idempotent (each source row has a `ref_key`), so a sync can
//! re-scan a window safely. [`start`] runs it in the background.

pub mod config;
pub mod facts;
pub mod notify;
pub mod overrides;
pub mod report;
pub mod review;
pub mod sign;
pub mod trust;

use crate::Core;
use crate::artifacts::{BlobRef, Blobs};
use anyhow::{Result, bail};
use kls_store::sessions::AuditLine;
use kls_store::{Event, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;

pub use kls_store::audit::{
    BrokenAt, ChainHead, ChainReport, LedgerDraft, LedgerEntry, LedgerQuery, SessionPurge,
    Teleport, canonical_json, chain_hash,
};

/// Event kinds of this area.
pub mod kinds {
    /// Entries were chained. Data: `{count, seq, hash}`.
    pub const AUDIT_APPENDED: &str = "audit.appended";
    /// The chain was verified. Data: `{ok, entries, brokenAt}`.
    pub const AUDIT_VERIFIED: &str = "audit.verified";
    /// A signed export was written: `entries`, `sha256`, `fromAt`.
    pub const AUDIT_EXPORTED: &str = "audit.exported";
    /// A session's data was deleted (tombstone chained). Subject: session id.
    pub const SESSION_DELETED: &str = "session.deleted";
    /// The settings document was saved. Data: `{revision, keys}`.
    pub const SETTINGS_SAVED: &str = "settings.saved";
    pub const TELEPORT_REQUESTED: &str = "teleport.requested";
    /// The bundle arrived and verified. Data: `{size, sha256, toNode}`.
    pub const TELEPORT_UPLOADED: &str = "teleport.uploaded";
    pub const TELEPORT_DOWNLOADED: &str = "teleport.downloaded";
    pub const TELEPORT_EXPIRED: &str = "teleport.expired";
    pub const TELEPORT_FAILED: &str = "teleport.failed";
    /// A teleport was refused before it started. Data: `{sessionId,
    /// fromNode, toNode, origin, reason}`.
    pub const TELEPORT_REFUSED: &str = "teleport.refused";
    /// What the Audit screen reloads on.
    pub const PREFIXES: [&str; 3] = ["audit.", "teleport.", "session.deleted"];
}

/// Settings keys of this area.
pub mod keys {
    /// Hours a teleport bundle is kept. Default 168 (7 days).
    pub const TELEPORT_RETENTION_HOURS: &str = "teleport.retention_hours";
    /// Highest event id folded into the ledger.
    pub const EVENT_CURSOR: &str = "audit.event_cursor";
    /// Highest `received_at` of a folded host audit line.
    pub const LINE_CURSOR: &str = "audit.line_cursor";
    /// The last verification, as JSON `{at, ok, entries}`.
    pub const LAST_VERIFY: &str = "audit.last_verify";
}

/// Why a third-party harness session is never teleported (harnesses.md C4.7).
pub const HARNESS_TELEPORT_REFUSAL: &str = "a third-party harness session cannot be teleported: \
its transcript and state live in the harness's own files on its host";

/// Largest teleport bundle accepted.
pub const TELEPORT_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_TELEPORT_RETENTION_HOURS: i64 = 168;
/// How long the origin node has to upload a requested bundle.
pub const TELEPORT_UPLOAD_TIMEOUT_MS: i64 = 10 * 60_000;

/// One sync at a time per process.
static SYNC_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Folding re-scans this far behind the cursors, so rows committed out of
/// order are not missed (duplicates are skipped by ref_key).
const EVENT_RESCAN: i64 = 200;
const LINE_RESCAN_MS: i64 = 10_000;
const PAGE: i64 = 1000;

/// A teleport request refused. Carried in `anyhow::Error`; the node server
/// maps it to a gRPC status with `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeleportRefusal {
    NotFound,
    /// Not this node's to upload or download.
    NotAddressed,
    /// Already uploaded, failed or expired.
    WrongState(String),
    Invalid(String),
}

impl std::fmt::Display for TeleportRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeleportRefusal::NotFound => write!(f, "unknown teleport"),
            TeleportRefusal::NotAddressed => write!(f, "teleport is not addressed to this node"),
            TeleportRefusal::WrongState(s) => write!(f, "teleport is {s}"),
            TeleportRefusal::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for TeleportRefusal {}

fn refuse(r: TeleportRefusal) -> anyhow::Error {
    anyhow::Error::new(r)
}

/// The header a node sends with the first part of an upload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TeleportHeader {
    pub session_id: String,
    pub from_node: String,
    pub to_node: String,
    pub title: String,
    pub size: i64,
    pub sha256: String,
}

/// Result of verifying every chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub ok: bool,
    pub verified_at: i64,
    pub ms: i64,
    /// The factory chain first, then each host stream.
    pub chains: Vec<ChainReport>,
}

impl VerifyReport {
    pub fn factory(&self) -> Option<&ChainReport> {
        self.chains.iter().find(|c| c.chain == "factory")
    }
}

/// The ledger kind and actor kind for a Keleustes event, if it is one the
/// ledger records.
pub fn ledger_kind_for_event(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "vault.granted" | "vault.grant_revoked" | "vault.secret_saved" | "vault.secret_deleted" => {
            ("vault", "system")
        }
        "vault.secret_tested" | "vault.secret_verified" | "vault.secret_label" => ("vault", "user"),
        "vault.secret_rollback" | "vault.secret_policy" | "vault.models_applied" => {
            ("vault", "user")
        }
        "vault.secret_shredded" => ("vault", "system"),
        "vault.grant_refused" | "vault.rotate_failed" => ("vault", "rule"),
        "factory.mode" => ("factory_control", "user"),
        "factory.refused" => ("kill_switch", "rule"),
        "budget.alert" | "budget.blocked" => ("budget", "rule"),
        "retention.pruned" => ("retention", "system"),
        "audit.exported" => ("export", "user"),
        "audit.report" => ("export", "system"),
        "notify.chat_webhook" => ("setting", "user"),
        "settings.project_prefs" => ("setting", "user"),
        "library.saved" | "library.deleted" | "library.imported" | "library.installed" => {
            ("library", "user")
        }
        "session.lost" => ("session", "system"),
        "estimate.recorded" => ("estimate", "system"),
        "approval.requested" => ("approval", "rule"),
        "approval.decided" => ("approval", "user"),
        "dispatch.updated" => ("dispatch", "system"),
        "card.dispatched" => ("card", "system"),
        "factory.control" => ("factory_control", "user"),
        "memory.created" | "memory.updated" | "memory.deleted" | "memory.cleared" => {
            ("memory", "user")
        }
        "a2a.withheld" => ("a2a_withheld", "rule"),
        "node.joined" => ("node", "host"),
        "node.revoked" => ("node", "user"),
        "token.minted" => ("token", "user"),
        "github.conflict.resolved" => ("commit", "user"),
        "github.pull.merged" => ("pull_request", "system"),
        "github.pull.action" => ("pull_request", "user"),
        "run.finished" => ("run", "system"),
        "workflow.published" | "workflow.archived" => ("workflow", "user"),
        "budget.saved" | "budget.deleted" | "budget.limit" => ("budget", "user"),
        "agent.saved" | "agent.deleted" | "crew.saved" | "crew.deleted" => ("workforce", "user"),
        "session.ended" => ("session", "host"),
        k if k.starts_with("teleport.") => ("teleport", "system"),
        _ => return None,
    })
}

/// The ledger kind for a belai host audit line kind (belai docs/audit.md).
pub fn ledger_kind_for_line(kind: &str) -> &'static str {
    match kind {
        "card.lease_lapsed" => "claim_lapsed",
        "repo.commit" => "commit",
        "repo.publish" => "pull_request",
        "gate.verified" => "gate",
        "vex.written" => "vex",
        "worker.fault" => "fault",
        "worker.fallback" => "provider_switch",
        "crew.control" => "crew_control",
        "a2a.sent" | "a2a.received" => "a2a",
        "a2a.blackout" => "a2a_blackout",
        "a2a.withheld" => "a2a_withheld",
        "egress.denied" => "egress",
        "run_cap" => "run_cap",
        k if k.starts_with("hazard.") => "hazard",
        k if k.starts_with("finding.") => "finding",
        k if k.starts_with("card.") => "card",
        k if k.starts_with("worker.") => "worker",
        _ => "host",
    }
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .filter(|s| !s.is_empty())
}

/// A short "key=value" summary of the scalar fields worth reading.
fn summary(v: &Value) -> String {
    const SHOWN: [&str; 14] = [
        "status",
        "state",
        "outcome",
        "decision",
        "gate",
        "exit",
        "reason",
        "provider",
        "title",
        "name",
        "repo",
        "stopped",
        "a2aBlackout",
        "label",
    ];
    let data = v.get("data").unwrap_or(v);
    let mut parts = Vec::new();
    for k in SHOWN {
        if let Some(x) = data.get(k) {
            let s = match x {
                Value::String(s) if !s.is_empty() => s.clone(),
                Value::Bool(_) | Value::Number(_) => x.to_string(),
                _ => continue,
            };
            let s: String = s.chars().take(80).collect();
            parts.push(format!("{k}={s}"));
        }
    }
    parts.join(" ")
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The ledger draft for an event the ledger records.
pub fn fold_event(ev: &Event) -> Option<LedgerDraft> {
    let (kind, actor_kind) = ledger_kind_for_event(&ev.kind)?;
    let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(json!({}));
    let actor = str_field(&data, &["by", "actor", "decidedBy"])
        .map(str::to_string)
        .unwrap_or_else(|| match actor_kind {
            "user" => "owner".into(),
            "host" => str_field(&data, &["nodeId"])
                .unwrap_or(&ev.subject)
                .to_string(),
            _ => "keleustes".into(),
        });
    let detail = summary(&data);
    let msg = if detail.is_empty() {
        format!("{} {}", ev.kind, ev.subject)
    } else {
        format!("{} {} {}", ev.kind, ev.subject, detail)
    };
    Some(LedgerDraft {
        at: ev.at,
        kind: kind.into(),
        actor,
        actor_kind: actor_kind.into(),
        msg,
        subject: json!({ "event": ev.kind, "id": ev.subject, "eventId": ev.id }),
        payload: data.clone(),
        origin: "factory".into(),
        node_id: str_field(&data, &["nodeId"]).unwrap_or("").into(),
        session_id: str_field(&data, &["sessionId"])
            .map(str::to_string)
            .or_else(|| ev.kind.starts_with("session.").then(|| ev.subject.clone()))
            .unwrap_or_default(),
        ref_key: Some(format!("event:{}", ev.id)),
    })
}

/// The ledger draft for a host audit line.
pub fn fold_line(l: &AuditLine) -> LedgerDraft {
    let v: Value =
        serde_json::from_str(&l.json).unwrap_or_else(|_| json!({ "raw": l.json.clone() }));
    let actor_kind = if l.kind.starts_with("host.") {
        "host"
    } else {
        "agent"
    };
    let actor = str_field(&v, &["worker", "agent", "hostname", "host"])
        .unwrap_or(&l.node_id)
        .to_string();
    let detail = summary(&v);
    let msg = format!(
        "{} on session {}{}{}",
        l.kind,
        short(&l.session_id),
        if detail.is_empty() { "" } else { " " },
        detail
    );
    LedgerDraft {
        at: if l.at > 0 { l.at } else { l.received_at },
        kind: ledger_kind_for_line(&l.kind).into(),
        actor,
        actor_kind: actor_kind.into(),
        msg,
        subject: json!({ "node": l.node_id, "session": l.session_id, "line": l.idx, "hostKind": l.kind }),
        payload: v,
        origin: format!("host:{}", l.node_id),
        node_id: l.node_id.clone(),
        session_id: l.session_id.clone(),
        ref_key: Some(format!("line:{}:{}:{}", l.node_id, l.session_id, l.idx)),
    }
}

/// Checks a host stream's own chain links (`prev` of line n is `hash` of
/// line n-1, indexes contiguous). Lines without the fields are accepted.
pub fn verify_host_stream(chain: &str, lines: &[AuditLine]) -> ChainReport {
    let mut r = ChainReport {
        chain: chain.into(),
        ok: true,
        entries: lines.len() as i64,
        ..Default::default()
    };
    let mut prev: Option<String> = None;
    let mut want_idx = lines.first().map(|l| l.idx).unwrap_or(0);
    for (i, l) in lines.iter().enumerate() {
        let v: Value = serde_json::from_str(&l.json).unwrap_or(Value::Null);
        let hash = v.get("hash").and_then(|x| x.as_str()).map(str::to_string);
        let linked = v.get("prev").and_then(|x| x.as_str()).map(str::to_string);
        let broken = if l.idx != want_idx {
            Some(BrokenAt {
                seq: want_idx,
                index: i as i64,
                expected: format!("line {want_idx}"),
                found: format!("line {}", l.idx),
                reason: "gap".into(),
            })
        } else {
            match (&prev, &linked) {
                (Some(p), Some(got)) if p != got => Some(BrokenAt {
                    seq: l.idx,
                    index: i as i64,
                    expected: p.clone(),
                    found: got.clone(),
                    reason: "prev_hash".into(),
                }),
                _ => None,
            }
        };
        if r.broken_at.is_none() && broken.is_some() {
            r.ok = false;
            r.broken_at = broken;
        }
        want_idx = l.idx + 1;
        if let Some(h) = hash {
            r.head = h.clone();
            prev = Some(h);
        } else {
            prev = None;
        }
    }
    r
}

impl Core {
    /// The content-addressed blob store under `<data>/artifacts/`.
    pub fn blobs(&self) -> Blobs {
        Blobs::in_data_dir(self.data_dir())
    }

    /// Chains one entry now (for direct records) and announces it.
    pub async fn audit_record(&self, d: LedgerDraft) -> Result<Option<LedgerEntry>> {
        let e = self.store().ledger_append(&d).await?;
        if let Some(e) = &e {
            self.emit_appended(1, e).await?;
        }
        Ok(e)
    }

    async fn emit_appended(&self, count: usize, last: &LedgerEntry) -> Result<()> {
        self.emit(
            kinds::AUDIT_APPENDED,
            "factory",
            &json!({ "count": count, "seq": last.seq, "hash": last.hash }),
        )
        .await?;
        Ok(())
    }

    /// Folds new events and host audit lines into the ledger. Returns how
    /// many entries were chained.
    pub async fn audit_sync(&self) -> Result<usize> {
        let _g = SYNC_LOCK.lock().await;
        let store = self.store();
        let mut added: Vec<LedgerEntry> = Vec::new();

        // Events.
        let cursor: i64 = store
            .setting(keys::EVENT_CURSOR)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut after = (cursor - EVENT_RESCAN).max(0);
        let mut high = cursor;
        loop {
            let evs = store.events_after(after, PAGE).await?;
            let Some(last) = evs.last() else { break };
            after = last.id;
            high = high.max(last.id);
            let mut drafts = Vec::new();
            for ev in &evs {
                if ev.id <= cursor && store.ledger_has_ref(&format!("event:{}", ev.id)).await? {
                    continue;
                }
                if let Some(d) = fold_event(ev) {
                    drafts.push(d);
                }
            }
            added.extend(store.ledger_append_many(&drafts).await?);
            if (evs.len() as i64) < PAGE {
                break;
            }
        }
        if high != cursor {
            store
                .set_setting(keys::EVENT_CURSOR, &high.to_string())
                .await?;
        }

        // Host audit lines.
        let lcursor: i64 = store
            .setting(keys::LINE_CURSOR)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let from = (lcursor - LINE_RESCAN_MS).max(0);
        let mut key: Option<AuditLine> = None;
        let mut lhigh = lcursor;
        loop {
            let lines = store
                .audit_lines_received_since(from, key.as_ref(), PAGE)
                .await?;
            let Some(last) = lines.last().cloned() else {
                break;
            };
            lhigh = lhigh.max(last.received_at);
            let mut drafts = Vec::new();
            for l in &lines {
                let d = fold_line(l);
                if let Some(r) = &d.ref_key
                    && store.ledger_has_ref(r).await?
                {
                    continue;
                }
                drafts.push(d);
            }
            added.extend(store.ledger_append_many(&drafts).await?);
            let full = lines.len() as i64 == PAGE;
            key = Some(last);
            if !full {
                break;
            }
        }
        if lhigh != lcursor {
            store
                .set_setting(keys::LINE_CURSOR, &lhigh.to_string())
                .await?;
        }

        if let Some(last) = added.last() {
            self.emit_appended(added.len(), last).await?;
        }
        Ok(added.len())
    }

    /// Ledger entries matching `q`.
    pub async fn audit_entries(&self, q: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        self.store().ledger(q).await
    }

    /// Recomputes the factory chain and checks each host stream's links.
    pub async fn audit_verify(&self) -> Result<VerifyReport> {
        let started = std::time::Instant::now();
        self.audit_sync().await?;
        let store = self.store();
        let mut chains = vec![store.ledger_verify().await?];
        for (node, session, _) in store.audit_line_streams().await? {
            let lines = store.audit_lines(&node, &session, 0, 0).await?;
            chains.push(verify_host_stream(
                &format!("host:{node}:{}", short(&session)),
                &lines,
            ));
        }
        let report = VerifyReport {
            ok: chains.iter().all(|c| c.ok),
            verified_at: now_ms(),
            ms: started.elapsed().as_millis() as i64,
            chains,
        };
        let f = report.factory().cloned().unwrap_or_default();
        let summary = json!({ "at": report.verified_at, "ok": report.ok, "entries": f.entries,
                              "brokenAt": f.broken_at });
        store
            .set_setting(keys::LAST_VERIFY, &summary.to_string())
            .await?;
        self.emit(kinds::AUDIT_VERIFIED, "factory", &summary)
            .await?;
        Ok(report)
    }

    /// The last verification `{at, ok, entries, brokenAt}`, if any.
    pub async fn audit_last_verify(&self) -> Result<Option<Value>> {
        Ok(self
            .store()
            .setting(keys::LAST_VERIFY)
            .await?
            .and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// Writes the entries matching `q` (oldest first, `limit` ignored) as
    /// JSON lines to `out`, ending with a head line
    /// `{"type":"head","chain":"factory","seq","hash","entries","exportedAt"}`.
    /// Returns how many entries were written.
    pub async fn audit_export_jsonl(
        &self,
        q: &LedgerQuery,
        out: &mut (dyn std::io::Write + Send),
    ) -> Result<i64> {
        let mut page = q.clone();
        page.ascending = true;
        page.limit = 2000;
        let mut n = 0;
        loop {
            let rows = self.store().ledger(&page).await?;
            let Some(last) = rows.last() else { break };
            page.after_seq = Some(last.seq);
            for e in &rows {
                let mut v = serde_json::to_value(e)?;
                v["type"] = json!("entry");
                writeln!(out, "{}", canonical_json(&v))?;
                n += 1;
            }
            if rows.len() < 2000 {
                break;
            }
        }
        let head = self.store().ledger_head().await?;
        let line = json!({ "type": "head", "chain": "factory", "seq": head.seq, "hash": head.hash,
                           "entries": n, "exportedAt": now_ms() });
        writeln!(out, "{}", canonical_json(&line))?;
        Ok(n)
    }

    /// Exports to `<data>/exports/audit-<ms>.jsonl` and returns the path.
    pub async fn audit_export_file(&self, q: &LedgerQuery) -> Result<(std::path::PathBuf, i64)> {
        let dir = self.data_dir().join("exports");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("audit-{}.jsonl", now_ms()));
        let mut buf: Vec<u8> = Vec::new();
        let n = self.audit_export_jsonl(q, &mut buf).await?;
        tokio::fs::write(&path, buf).await?;
        Ok((path, n))
    }

    /// Deletes a session's mirrored data (metadata, transcript, inbox, raw
    /// audit lines) and chains a `session_deleted` tombstone. The session's
    /// host audit lines are folded into the ledger first, so they survive.
    pub async fn delete_session(&self, id: &str, reason: &str) -> Result<LedgerEntry> {
        self.audit_sync().await?;
        let Some(s) = self.store().session(id).await? else {
            bail!("unknown session {id}");
        };
        let reason: String = reason.trim().chars().take(500).collect();
        let tomb = LedgerDraft {
            kind: "session_deleted".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!(
                "Session {} deleted{}",
                if s.title.is_empty() {
                    short(&s.id)
                } else {
                    s.title.as_str()
                },
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            ),
            subject: json!({ "session": s.id, "node": s.node_id, "repo": s.repo }),
            payload: json!({ "reason": reason, "entries": s.entry_count, "startedAt": s.started_at,
                             "endedAt": s.ended_at, "runId": s.run_id, "cardId": s.card_id }),
            node_id: s.node_id.clone(),
            session_id: s.id.clone(),
            ..Default::default()
        };
        let Some((entry, purge)) = self.store().delete_session(id, &tomb).await? else {
            bail!("unknown session {id}");
        };
        self.emit(
            kinds::SESSION_DELETED,
            id,
            &json!({ "nodeId": s.node_id, "seq": entry.seq, "purged": purge }),
        )
        .await?;
        self.emit_appended(1, &entry).await?;
        Ok(entry)
    }

    // ---- teleports -----------------------------------------------------------

    async fn teleport_retention_ms(&self) -> Result<i64> {
        let h = self
            .store()
            .setting(keys::TELEPORT_RETENTION_HOURS)
            .await?
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|h| *h > 0)
            .unwrap_or(DEFAULT_TELEPORT_RETENTION_HOURS);
        Ok(h * 3_600_000)
    }

    async fn live_node(&self, id: &str) -> Result<kls_store::Node> {
        match self.store().node(id).await? {
            Some(n) if n.revoked_at.is_none() => Ok(n),
            Some(_) => Err(refuse(TeleportRefusal::Invalid(format!(
                "node {id} is revoked"
            )))),
            None => Err(refuse(TeleportRefusal::Invalid(format!(
                "unknown node {id}"
            )))),
        }
    }

    async fn emit_teleport(&self, kind: &str, t: &Teleport) -> Result<()> {
        self.emit(
            kind,
            &t.id,
            &json!({ "sessionId": t.session_id, "fromNode": t.from_node, "toNode": t.to_node,
                     "status": t.status, "size": t.size, "sha256": t.sha256, "error": t.error }),
        )
        .await?;
        Ok(())
    }

    /// Asks `from_node` to bundle `session_id` for `to_node`. The request
    /// goes to the origin as a `teleport` job (payload
    /// `{"task":"teleport_upload","teleportId","sessionId","toNode"}`) when a
    /// node server runs in this process; the origin then calls
    /// `Teleport.Upload` with the teleport id.
    pub async fn teleport_request(
        &self,
        session_id: &str,
        from_node: &str,
        to_node: &str,
    ) -> Result<Teleport> {
        if from_node == to_node {
            return Err(refuse(TeleportRefusal::Invalid(
                "a teleport moves a session to another node".into(),
            )));
        }
        // New work on two nodes: refused (and audited) under the kill switch.
        crate::control::check(
            self,
            "job",
            session_id,
            &format!("teleport:{from_node}->{to_node}"),
        )
        .await?;
        self.live_node(from_node).await?;
        self.live_node(to_node).await?;
        let session = self.store().session(session_id).await?;
        if let Some(s) = &session
            && s.node_id != from_node
        {
            return Err(refuse(TeleportRefusal::Invalid(format!(
                "session {session_id} ran on another node"
            ))));
        }
        if let Some(s) = &session
            && s.kind == "harness"
        {
            return Err(self
                .teleport_refuse_harness(session_id, from_node, to_node, "owner")
                .await);
        }
        let now = now_ms();
        let t = Teleport {
            id: uuid::Uuid::now_v7().to_string(),
            session_id: session_id.into(),
            from_node: from_node.into(),
            to_node: to_node.into(),
            title: session.map(|s| s.title).unwrap_or_default(),
            status: "requested".into(),
            created_at: now,
            expires_at: now + self.teleport_retention_ms().await?,
            ..Default::default()
        };
        self.store().insert_teleport(&t).await?;
        self.emit_teleport(kinds::TELEPORT_REQUESTED, &t).await?;
        if let Ok(g) = self.gateway() {
            let core = self.clone();
            let t2 = t.clone();
            tokio::spawn(async move {
                let payload = json!({ "task": "teleport_upload", "teleportId": t2.id,
                                      "sessionId": t2.session_id, "toNode": t2.to_node });
                let r = g
                    .job(
                        Some(&t2.from_node),
                        "teleport",
                        &payload.to_string(),
                        crate::JobNeeds::default(),
                        TELEPORT_UPLOAD_TIMEOUT_MS,
                    )
                    .await;
                let err = match r {
                    Ok(o) if o.status == "ok" => return,
                    Ok(o) if !o.error.is_empty() => format!("{}: {}", o.status, o.error),
                    Ok(o) => o.status,
                    Err(e) => format!("{e:#}"),
                };
                // Only a teleport still waiting for its bundle fails.
                if let Ok(Some(cur)) = core.store().teleport(&t2.id).await
                    && cur.status == "requested"
                {
                    let _ = core.teleport_fail(&t2.id, &err).await;
                }
            });
        }
        Ok(t)
    }

    /// Refuses a teleport of a third-party harness session (harnesses.md
    /// C4.7): its transcript and state live in the harness's own files,
    /// which a belai bundle does not carry. Emits and audits the refusal
    /// with its origin, and returns the error to hand back.
    pub async fn teleport_refuse_harness(
        &self,
        session_id: &str,
        from_node: &str,
        to_node: &str,
        origin: &str,
    ) -> anyhow::Error {
        let why = HARNESS_TELEPORT_REFUSAL;
        let data = json!({ "sessionId": session_id, "fromNode": from_node, "toNode": to_node,
                           "origin": origin, "reason": why, "outcome": "refused" });
        let ev = self.emit(kinds::TELEPORT_REFUSED, session_id, &data).await;
        // Chained now, under the event's own ref key so the fold skips it.
        let _ = self
            .audit_record(LedgerDraft {
                ref_key: ev.ok().map(|e| format!("event:{}", e.id)),
                kind: "teleport".into(),
                actor: origin.into(),
                actor_kind: if origin.starts_with("node:") {
                    "host"
                } else {
                    "user"
                }
                .into(),
                msg: format!("teleport of harness session {session_id} refused: {why}"),
                subject: json!({ "session": session_id }),
                payload: data,
                node_id: from_node.into(),
                session_id: session_id.into(),
                ..Default::default()
            })
            .await;
        refuse(TeleportRefusal::Invalid(why.into()))
    }

    /// Most recent teleports first.
    pub async fn teleports(&self, limit: i64) -> Result<Vec<Teleport>> {
        self.store().teleports(limit).await
    }

    /// Checks an upload's header from `node_id` and returns the teleport it
    /// fills: the requested one named `teleport_id`, or a new one when the
    /// node starts a teleport itself (empty id).
    pub async fn teleport_begin_upload(
        &self,
        node_id: &str,
        teleport_id: &str,
        h: &TeleportHeader,
    ) -> Result<Teleport> {
        if h.size < 0 || h.size as u64 > TELEPORT_MAX_BYTES {
            return Err(refuse(TeleportRefusal::Invalid(format!(
                "bundle size must be 0..{TELEPORT_MAX_BYTES} bytes"
            ))));
        }
        if !h.sha256.is_empty() && !crate::artifacts::is_sha256(&h.sha256.to_ascii_lowercase()) {
            return Err(refuse(TeleportRefusal::Invalid(
                "sha256 must be 64 hex digits".into(),
            )));
        }
        if !h.from_node.is_empty() && h.from_node != node_id {
            return Err(refuse(TeleportRefusal::NotAddressed));
        }
        if teleport_id.is_empty() {
            if h.to_node.is_empty() {
                return Err(refuse(TeleportRefusal::Invalid(
                    "to_node is required".into(),
                )));
            }
            if h.to_node == node_id {
                return Err(refuse(TeleportRefusal::Invalid(
                    "a teleport moves a session to another node".into(),
                )));
            }
            self.live_node(&h.to_node).await?;
            let now = now_ms();
            let t = Teleport {
                id: uuid::Uuid::now_v7().to_string(),
                session_id: h.session_id.chars().take(200).collect(),
                from_node: node_id.into(),
                to_node: h.to_node.clone(),
                title: h.title.chars().take(300).collect(),
                status: "requested".into(),
                created_at: now,
                expires_at: now + self.teleport_retention_ms().await?,
                ..Default::default()
            };
            self.store().insert_teleport(&t).await?;
            self.emit_teleport(kinds::TELEPORT_REQUESTED, &t).await?;
            return Ok(t);
        }
        let t = self
            .store()
            .teleport(teleport_id)
            .await?
            .ok_or_else(|| refuse(TeleportRefusal::NotFound))?;
        if t.from_node != node_id {
            return Err(refuse(TeleportRefusal::NotAddressed));
        }
        if !h.to_node.is_empty() && h.to_node != t.to_node {
            return Err(refuse(TeleportRefusal::Invalid(
                "to_node does not match the request".into(),
            )));
        }
        if t.status != "requested" {
            return Err(refuse(TeleportRefusal::WrongState(t.status)));
        }
        if t.expires_at > 0 && t.expires_at <= now_ms() {
            return Err(refuse(TeleportRefusal::WrongState("expired".into())));
        }
        Ok(t)
    }

    /// The bundle of `id` is stored as `blob`.
    pub async fn teleport_uploaded(
        &self,
        id: &str,
        blob: &BlobRef,
        title: &str,
    ) -> Result<Teleport> {
        let expires = now_ms() + self.teleport_retention_ms().await?;
        if !self
            .store()
            .teleport_ready(
                id,
                blob.size as i64,
                &blob.sha256,
                expires,
                &title.chars().take(300).collect::<String>(),
            )
            .await?
        {
            let st = self
                .store()
                .teleport(id)
                .await?
                .map(|t| t.status)
                .unwrap_or_else(|| "gone".into());
            return Err(refuse(TeleportRefusal::WrongState(st)));
        }
        let t = self
            .store()
            .teleport(id)
            .await?
            .ok_or_else(|| refuse(TeleportRefusal::NotFound))?;
        self.emit_teleport(kinds::TELEPORT_UPLOADED, &t).await?;
        Ok(t)
    }

    /// The teleport `node_id` may download now.
    pub async fn teleport_for_download(&self, node_id: &str, id: &str) -> Result<Teleport> {
        let t = self
            .store()
            .teleport(id)
            .await?
            .ok_or_else(|| refuse(TeleportRefusal::NotFound))?;
        if t.to_node != node_id {
            return Err(refuse(TeleportRefusal::NotAddressed));
        }
        if t.expires_at > 0 && t.expires_at <= now_ms() {
            return Err(refuse(TeleportRefusal::WrongState("expired".into())));
        }
        if !matches!(t.status.as_str(), "ready" | "delivered") {
            return Err(refuse(TeleportRefusal::WrongState(t.status)));
        }
        Ok(t)
    }

    /// The addressed node received the whole bundle.
    pub async fn teleport_downloaded(&self, id: &str) -> Result<()> {
        if self.store().teleport_delivered(id).await?
            && let Some(t) = self.store().teleport(id).await?
        {
            self.emit_teleport(kinds::TELEPORT_DOWNLOADED, &t).await?;
        }
        Ok(())
    }

    /// Marks a teleport that has not delivered as failed.
    pub async fn teleport_fail(&self, id: &str, error: &str) -> Result<()> {
        let error: String = error.chars().take(500).collect();
        if self.store().teleport_failed(id, &error).await?
            && let Some(t) = self.store().teleport(id).await?
        {
            self.emit_teleport(kinds::TELEPORT_FAILED, &t).await?;
        }
        Ok(())
    }

    /// Expires teleports past retention and removes bundles nothing needs.
    /// Returns how many teleports expired.
    pub async fn teleport_sweep(&self) -> Result<usize> {
        self.teleport_sweep_at(now_ms()).await
    }

    /// [`Core::teleport_sweep`] as of `now`.
    pub async fn teleport_sweep_at(&self, now: i64) -> Result<usize> {
        let gone = self.store().expire_teleports(now).await?;
        let blobs = self.blobs();
        for t in &gone {
            if !t.sha256.is_empty() && !self.store().teleport_blob_in_use(&t.sha256).await? {
                let _ = blobs.remove(&t.sha256).await;
            }
            self.emit_teleport(kinds::TELEPORT_EXPIRED, t).await?;
        }
        let _ = blobs.clean_tmp(std::time::Duration::from_secs(3600)).await;
        Ok(gone.len())
    }
}

/// The periodic governance work (every 10 min): the vault's grace sweep
/// and auto-rotation, and last week's report once the week has ended.
pub async fn governance_tick(core: &Core) {
    let now = now_ms();
    match crate::vault::lineage::tick(core, now, &crate::vault::lineage::minter_for).await {
        Ok(r) if !r.errors.is_empty() => tracing::warn!(errors = ?r.errors, "vault tick"),
        Ok(_) => {}
        Err(e) => tracing::debug!("vault tick: {e:#}"),
    }
    if let Err(e) = report::weekly_tick(core, now).await {
        tracing::warn!("weekly report: {e:#}");
    }
}

/// Runs the ledger fold in the background for the life of the process:
/// shortly after relevant events, and every 30 s for host audit lines
/// (they arrive without an event). Sweeps expired teleports and runs
/// [`governance_tick`] every 10 min, and follows events for the chat
/// channel ([`notify::start`]).
pub fn start(core: &Core) -> tokio::task::JoinHandle<()> {
    let core = core.clone();
    notify::start(&core);
    tokio::spawn(async move {
        let mut bus = core.subscribe();
        let mut lines = tokio::time::interval(std::time::Duration::from_secs(30));
        let mut sweep = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tokio::select! {
                ev = bus.recv() => match ev {
                    Ok(ev) if ledger_kind_for_event(&ev.kind).is_none() => continue,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        while bus.try_recv().is_ok() {}
                    }
                    Err(_) => return,
                },
                _ = lines.tick() => {}
                _ = sweep.tick() => {
                    if let Err(e) = core.teleport_sweep().await {
                        tracing::warn!("teleport sweep: {e:#}");
                    }
                    governance_tick(&core).await;
                }
            }
            if let Err(e) = core.audit_sync().await {
                tracing::warn!("audit sync: {e:#}");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::sessions::Session;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn line(idx: i64, kind: &str, json: &str) -> AuditLine {
        AuditLine {
            idx,
            at: 1000 + idx,
            kind: kind.into(),
            json: json.into(),
            ..Default::default()
        }
    }

    async fn node(c: &Core, id: &str) {
        c.store()
            .insert_node(id, id, &Default::default(), "serial", "tok")
            .await
            .unwrap();
    }

    #[test]
    fn kind_maps() {
        assert_eq!(
            ledger_kind_for_event("vault.granted"),
            Some(("vault", "system"))
        );
        assert_eq!(
            ledger_kind_for_event("approval.decided").unwrap().0,
            "approval"
        );
        assert_eq!(
            ledger_kind_for_event("teleport.uploaded").unwrap().0,
            "teleport"
        );
        assert_eq!(ledger_kind_for_event("audit.appended"), None);
        assert_eq!(ledger_kind_for_event("session.entries"), None);
        assert_eq!(ledger_kind_for_line("gate.verified"), "gate");
        assert_eq!(ledger_kind_for_line("card.claimed"), "card");
        assert_eq!(ledger_kind_for_line("card.lease_lapsed"), "claim_lapsed");
        assert_eq!(ledger_kind_for_line("hazard.denied"), "hazard");
        assert_eq!(ledger_kind_for_line("host.rc_online"), "host");
    }

    #[test]
    fn fold_event_and_line() {
        let ev = Event {
            id: 7,
            at: 99,
            kind: "dispatch.updated".into(),
            subject: "d1".into(),
            data_json: r#"{"nodeId":"n1","status":"started","sessionId":"s1"}"#.into(),
        };
        let d = fold_event(&ev).unwrap();
        assert_eq!(d.kind, "dispatch");
        assert_eq!(d.ref_key.as_deref(), Some("event:7"));
        assert_eq!((d.node_id.as_str(), d.session_id.as_str()), ("n1", "s1"));
        assert!(d.msg.contains("status=started"), "{}", d.msg);

        let mut l = line(3, "gate.verified", r#"{"data":{"gate":"sca","exit":0}}"#);
        l.node_id = "n1".into();
        l.session_id = "s1".into();
        let d = fold_line(&l);
        assert_eq!((d.kind.as_str(), d.origin.as_str()), ("gate", "host:n1"));
        assert_eq!(d.ref_key.as_deref(), Some("line:n1:s1:3"));
        assert!(d.msg.contains("gate=sca"), "{}", d.msg);
        let bad = fold_line(&line(0, "x", "not json"));
        assert_eq!(bad.payload["raw"], "not json");
    }

    #[test]
    fn host_stream_links() {
        let l = |i, prev: &str, hash: &str| {
            line(
                i,
                "worker.started",
                &format!(r#"{{"prev":"{prev}","hash":"{hash}"}}"#),
            )
        };
        let ok = verify_host_stream("h", &[l(0, "", "a"), l(1, "a", "b"), l(2, "b", "c")]);
        assert!(ok.ok && ok.head == "c");
        let bad = verify_host_stream("h", &[l(0, "", "a"), l(1, "x", "b")]);
        assert_eq!(bad.broken_at.unwrap().reason, "prev_hash");
        let gap = verify_host_stream("h", &[l(0, "", "a"), l(2, "a", "b")]);
        assert_eq!(gap.broken_at.unwrap().reason, "gap");
        let plain = verify_host_stream("h", &[line(0, "k", "{}"), line(1, "k", "{}")]);
        assert!(plain.ok);
    }

    #[tokio::test]
    async fn sync_folds_events_and_lines_once() {
        let (c, _d) = core().await;
        c.emit("vault.granted", "g1", &json!({ "provider": "anthropic" }))
            .await
            .unwrap();
        c.emit("session.entries", "s1", &json!({})).await.unwrap();
        c.record_audit(
            "n1",
            "s1",
            &[
                line(0, "gate.verified", r#"{"data":{"gate":"sca","exit":0}}"#),
                line(1, "card.claimed", "{}"),
            ],
        )
        .await
        .unwrap();
        let mut rx = c.subscribe();
        assert_eq!(c.audit_sync().await.unwrap(), 3);
        assert_eq!(rx.recv().await.unwrap().kind, kinds::AUDIT_APPENDED);
        assert_eq!(c.audit_sync().await.unwrap(), 0);
        c.emit(
            "approval.decided",
            "a1",
            &json!({ "decision": "approve", "by": "chris" }),
        )
        .await
        .unwrap();
        assert_eq!(c.audit_sync().await.unwrap(), 1);
        let all = c.audit_entries(&LedgerQuery::default()).await.unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].kind, "approval");
        assert_eq!(all[0].actor, "chris");
        let r = c.audit_verify().await.unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(r.chains.len(), 2);
        assert_eq!(r.factory().unwrap().entries, 4);
        assert_eq!(c.audit_last_verify().await.unwrap().unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn export_jsonl_ends_with_head() {
        let (c, _d) = core().await;
        for i in 0..3 {
            c.audit_record(LedgerDraft {
                kind: "setting".into(),
                msg: format!("m{i}"),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        let mut buf = Vec::new();
        let n = c
            .audit_export_jsonl(&LedgerQuery::default(), &mut buf)
            .await
            .unwrap();
        assert_eq!(n, 3);
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["seq"], 1);
        assert_eq!(lines[0]["type"], "entry");
        // An exported entry re-hashes to its own hash.
        let e: LedgerEntry = serde_json::from_value(lines[1].clone()).unwrap();
        assert_eq!(e.expected_hash(), e.hash);
        assert_eq!(lines[3]["type"], "head");
        assert_eq!(lines[3]["hash"], lines[2]["hash"]);
        let (path, n) = c.audit_export_file(&LedgerQuery::default()).await.unwrap();
        assert_eq!(n, 3);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn delete_session_tombstone_keeps_lines() {
        let (c, _d) = core().await;
        c.store()
            .upsert_session(&Session {
                id: "s1".into(),
                node_id: "n1".into(),
                title: "fix the build".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        c.record_audit("n1", "s1", &[line(0, "card.claimed", "{}")])
            .await
            .unwrap();
        let mut rx = c.subscribe();
        let e = c.delete_session("s1", "cleanup").await.unwrap();
        assert_eq!(e.kind, "session_deleted");
        assert!(e.msg.contains("fix the build"));
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev.kind);
        }
        assert!(seen.contains(&kinds::SESSION_DELETED.to_string()));
        // The host line was chained before the raw rows went.
        let q = LedgerQuery {
            session_id: Some("s1".into()),
            ..Default::default()
        };
        let kinds: Vec<String> = c
            .audit_entries(&q)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["session_deleted", "card"]);
        assert!(c.store().session("s1").await.unwrap().is_none());
        assert!(c.delete_session("s1", "").await.is_err());
        assert!(c.audit_verify().await.unwrap().ok);
    }

    #[tokio::test]
    async fn teleport_of_a_harness_session_is_refused_and_audited() {
        let (c, _d) = core().await;
        node(&c, "n1").await;
        node(&c, "n2").await;
        c.store()
            .upsert_session(&Session {
                id: "h1".into(),
                node_id: "n1".into(),
                kind: "harness".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // The kill switch still refuses first.
        crate::control::pause(&c, "drill").await.unwrap();
        let e = c.teleport_request("h1", "n1", "n2").await.unwrap_err();
        assert!(crate::control::is_blocked(&e));
        crate::control::resume(&c).await.unwrap();
        let e = c.teleport_request("h1", "n1", "n2").await.unwrap_err();
        assert!(matches!(
            e.downcast_ref::<TeleportRefusal>(),
            Some(TeleportRefusal::Invalid(w)) if w == HARNESS_TELEPORT_REFUSAL
        ));
        assert!(c.store().teleports(10).await.unwrap().is_empty());
        let ledger = c
            .store()
            .ledger(&LedgerQuery {
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        let hits: Vec<_> = ledger
            .iter()
            .filter(|e| e.kind == "teleport" && e.msg.contains("harness session h1 refused"))
            .collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].actor, "owner");
        // Folding the event later does not chain it twice.
        c.audit_sync().await.unwrap();
        let again = c
            .store()
            .ledger(&LedgerQuery {
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        let refused = |l: &[LedgerEntry]| {
            l.iter()
                .filter(|e| e.payload["reason"] == HARNESS_TELEPORT_REFUSAL)
                .count()
        };
        assert_eq!(refused(&again), refused(&ledger));
        assert_eq!(hits[0].payload["origin"], "owner");
        assert!(c.audit_verify().await.unwrap().ok);
    }

    #[tokio::test]
    async fn teleport_rules() {
        let (c, _d) = core().await;
        node(&c, "n1").await;
        node(&c, "n2").await;
        node(&c, "n3").await;
        let refusal = |e: anyhow::Error| e.downcast_ref::<TeleportRefusal>().cloned();
        assert!(c.teleport_request("s1", "n1", "n1").await.is_err());
        assert!(c.teleport_request("s1", "n1", "zz").await.is_err());
        // Refused and audited under the kill switch.
        crate::control::pause(&c, "drill").await.unwrap();
        let e = c.teleport_request("s1", "n1", "n2").await.unwrap_err();
        assert!(crate::control::is_blocked(&e));
        assert!(
            c.store()
                .recent_events(20)
                .await
                .unwrap()
                .iter()
                .any(|e| e.kind == crate::control::kinds::FACTORY_REFUSED
                    && e.data_json.contains("teleport:n1->n2"))
        );
        crate::control::resume(&c).await.unwrap();
        let t = c.teleport_request("s1", "n1", "n2").await.unwrap();
        assert_eq!(t.status, "requested");
        assert!(t.expires_at > now_ms());

        let h = TeleportHeader {
            to_node: "n2".into(),
            size: 3,
            ..Default::default()
        };
        assert_eq!(
            refusal(c.teleport_begin_upload("n3", &t.id, &h).await.unwrap_err()),
            Some(TeleportRefusal::NotAddressed)
        );
        assert_eq!(
            refusal(c.teleport_begin_upload("n1", "nope", &h).await.unwrap_err()),
            Some(TeleportRefusal::NotFound)
        );
        let big = TeleportHeader {
            size: TELEPORT_MAX_BYTES as i64 + 1,
            ..h.clone()
        };
        assert!(c.teleport_begin_upload("n1", &t.id, &big).await.is_err());
        assert_eq!(
            c.teleport_begin_upload("n1", &t.id, &h).await.unwrap().id,
            t.id
        );
        // Not downloadable before upload.
        assert!(matches!(
            refusal(c.teleport_for_download("n2", &t.id).await.unwrap_err()),
            Some(TeleportRefusal::WrongState(_))
        ));
        let blob = c.blobs().put(b"abc").await.unwrap();
        let ready = c.teleport_uploaded(&t.id, &blob, "").await.unwrap();
        assert_eq!((ready.status.as_str(), ready.size), ("ready", 3));
        assert!(c.teleport_uploaded(&t.id, &blob, "").await.is_err());
        assert_eq!(
            refusal(c.teleport_for_download("n3", &t.id).await.unwrap_err()),
            Some(TeleportRefusal::NotAddressed)
        );
        c.teleport_for_download("n2", &t.id).await.unwrap();
        c.teleport_downloaded(&t.id).await.unwrap();
        assert_eq!(
            c.store().teleport(&t.id).await.unwrap().unwrap().status,
            "delivered"
        );

        // A node may start one itself.
        let own = c.teleport_begin_upload("n2", "", &h).await;
        assert!(own.is_err(), "to itself");
        let own = c
            .teleport_begin_upload(
                "n2",
                "",
                &TeleportHeader {
                    to_node: "n3".into(),
                    ..h.clone()
                },
            )
            .await
            .unwrap();
        assert_eq!((own.from_node.as_str(), own.to_node.as_str()), ("n2", "n3"));
        c.teleport_fail(&own.id, "origin gave up").await.unwrap();
        assert_eq!(c.teleports(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sweep_expires_and_removes_blobs() {
        let (c, _d) = core().await;
        node(&c, "n1").await;
        node(&c, "n2").await;
        c.store()
            .set_setting(keys::TELEPORT_RETENTION_HOURS, "1")
            .await
            .unwrap();
        let t = c.teleport_request("", "n1", "n2").await.unwrap();
        let blob = c.blobs().put(b"bundle").await.unwrap();
        c.teleport_uploaded(&t.id, &blob, "").await.unwrap();
        assert_eq!(c.teleport_sweep().await.unwrap(), 0);
        assert!(c.blobs().exists(&blob.sha256).await);
        let mut rx = c.subscribe();
        assert_eq!(
            c.teleport_sweep_at(now_ms() + 2 * 3_600_000).await.unwrap(),
            1
        );
        assert_eq!(rx.recv().await.unwrap().kind, kinds::TELEPORT_EXPIRED);
        assert!(!c.blobs().exists(&blob.sha256).await);
        assert_eq!(
            c.store().teleport(&t.id).await.unwrap().unwrap().status,
            "expired"
        );
        assert!(c.teleport_for_download("n2", &t.id).await.is_err());
    }
}
