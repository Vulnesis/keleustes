//! Card claims, held cards, the Blocked list and triage by cause.
//!
//! belai keeps a card's fleet routing and claim in `meta_json` under
//! `belai` (its `factorylink` belaiCard: `list`, `history`, `agent` with
//! `claimedBy`, `claimHost`, `leaseUntil`, `attempts`, `dependsOn` ...).
//! Keleustes has no `blocked` status: a blocked card is stored `todo` with
//! `belai.list = "blocked"` (belai `statusFor`), and the board shows it in
//! a Blocked list. These helpers read and write that part in place, keeping
//! every key they do not know.
//!
//! A held card carries a `checkpoint:<reason>` label and no claim
//! (website `.repo/belai-checkpoints.md`): Approve removes the label and
//! notes `checkpoint <reason> approved[: why]`; Reject removes it, moves the
//! card to blocked and notes `checkpoint <reason> rejected: why`.
//!
//! Every action here writes a ledger entry with its actor and outcome.

use super::{CardPatch, OWNER, kinds};
use crate::Core;
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::Card;
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CHECKPOINT_PREFIX: &str = "checkpoint:";
/// The label a conflict card holds after five failed attempts.
pub const CONFLICTS_UNRESOLVED: &str = "checkpoint:conflicts-unresolved";
pub const SNOOZE_DAYS: i64 = 7;
/// A note on a card is at most this long.
pub const NOTE_MAX: usize = 2000;
/// History notes kept on a card (belai caps its history too).
pub const HISTORY_MAX: usize = 200;

/// A worker's claim on a card.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub claimed_by: String,
    pub claim_host: String,
    pub claim_from: String,
    /// Unix ms; 0 when none.
    pub lease_until: i64,
    pub attempts: i64,
    pub bounces: i64,
    pub continuations: i64,
    pub hops: i64,
    pub parent: String,
    pub depends_on: Vec<String>,
    pub pin_host: String,
}

impl Claim {
    pub fn is_claimed(&self) -> bool {
        !self.claimed_by.is_empty()
    }

    /// A claim whose lease ran out.
    pub fn lapsed(&self, now: i64) -> bool {
        self.is_claimed() && self.lease_until > 0 && self.lease_until < now
    }
}

/// One entry of a card's history (belai `KanbanMove`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryNote {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub at: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn meta(c: &Card) -> Value {
    let v: Value = serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
    if v.is_object() { v } else { json!({}) }
}

fn agent(m: &Value) -> &Value {
    m.pointer("/belai/agent").unwrap_or(&Value::Null)
}

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

fn st(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The card's claim (empty when nobody holds it).
pub fn claim(c: &Card) -> Claim {
    let m = meta(c);
    let a = agent(&m);
    Claim {
        claimed_by: st(a, "claimedBy"),
        claim_host: st(a, "claimHost"),
        claim_from: st(a, "claimFrom"),
        lease_until: i(a, "leaseUntil"),
        attempts: i(a, "attempts"),
        bounces: i(a, "bounces"),
        continuations: i(a, "continuations"),
        hops: i(a, "hops"),
        parent: st(a, "parent"),
        depends_on: a
            .get("dependsOn")
            .and_then(Value::as_array)
            .map(|x| {
                x.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        pin_host: st(a, "pinHost"),
    }
}

/// The belai list the card reads as (`blocked` included).
pub fn belai_list(c: &Card) -> String {
    meta(c)
        .pointer("/belai/list")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// On the Blocked list: belai says blocked, or a `blocked` label.
pub fn is_blocked(c: &Card) -> bool {
    (c.status == "todo" && belai_list(c) == "blocked")
        || c.labels.iter().any(|l| crate::needs::is_blocked_label(l))
}

/// The `checkpoint:<reason>` a card holds for a person, if any.
pub fn checkpoint_reason(c: &Card) -> Option<String> {
    c.labels.iter().find_map(|l| {
        l.strip_prefix(CHECKPOINT_PREFIX)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    })
}

/// Held for a person: a checkpoint label and no worker's claim.
pub fn is_held(c: &Card) -> bool {
    !c.deleted && checkpoint_reason(c).is_some() && !claim(c).is_claimed()
}

pub fn history(c: &Card) -> Vec<HistoryNote> {
    meta(c)
        .pointer("/belai/history")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Snoozed off the triage queue until (Unix ms).
pub fn snoozed_until(c: &Card) -> Option<i64> {
    meta(c)
        .pointer("/keleustes/snoozedUntil")
        .and_then(Value::as_i64)
}

/// Why a blocked card stopped (the Triage tab's groups).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    WaitingOnHuman,
    ClaimLapsed,
    OutOfPasses,
    ToolFailed,
    Errored,
    Other,
}

impl Cause {
    pub const ALL: [Cause; 6] = [
        Cause::WaitingOnHuman,
        Cause::ClaimLapsed,
        Cause::OutOfPasses,
        Cause::ToolFailed,
        Cause::Errored,
        Cause::Other,
    ];

    pub fn words(self) -> &'static str {
        match self {
            Cause::WaitingOnHuman => "Waiting on you",
            Cause::ClaimLapsed => "Claim lapsed",
            Cause::OutOfPasses => "Out of passes",
            Cause::ToolFailed => "Tool failed",
            Cause::Errored => "Errored",
            Cause::Other => "Other",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Cause::WaitingOnHuman => "waiting_on_human",
            Cause::ClaimLapsed => "claim_lapsed",
            Cause::OutOfPasses => "out_of_passes",
            Cause::ToolFailed => "tool_failed",
            Cause::Errored => "errored",
            Cause::Other => "other",
        }
    }

    /// The suggested fix, in a few words.
    pub fn fix(self) -> &'static str {
        match self {
            Cause::WaitingOnHuman => "Decide the checkpoint",
            Cause::ClaimLapsed => "Release the claim and retry",
            Cause::OutOfPasses => "Edit the card, then retry",
            Cause::ToolFailed => "Check the node's tools, then retry",
            Cause::Errored => "Read the last note, then retry",
            Cause::Other => "Edit or reassign",
        }
    }
}

/// The last note that is not a decision note.
pub fn last_note(c: &Card) -> Option<HistoryNote> {
    history(c)
        .into_iter()
        .rev()
        .find(|h| !h.note.trim().is_empty())
}

/// The cause of a blocked card, from its labels, claim and newest note.
pub fn triage_cause(c: &Card, now: i64, max_attempts: i64) -> Cause {
    if checkpoint_reason(c).is_some() {
        return Cause::WaitingOnHuman;
    }
    let cl = claim(c);
    if cl.lapsed(now) {
        return Cause::ClaimLapsed;
    }
    let note = last_note(c)
        .map(|h| h.note.to_ascii_lowercase())
        .unwrap_or_default();
    if (max_attempts > 0 && cl.attempts >= max_attempts)
        || [
            "out of passes",
            "attempts exhausted",
            "max attempts",
            "budget",
        ]
        .iter()
        .any(|w| note.contains(w))
    {
        return Cause::OutOfPasses;
    }
    if ["tool", "permission denied", "command not found"]
        .iter()
        .any(|w| note.contains(w))
    {
        return Cause::ToolFailed;
    }
    if ["error", "failed", "panic", "crash"]
        .iter()
        .any(|w| note.contains(w))
    {
        return Cause::Errored;
    }
    Cause::Other
}

/// One `conflict attempt N/M <fp>: <files> -> <result>` note.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConflictAttempt {
    pub attempt: i64,
    pub of: i64,
    pub fingerprint: String,
    pub files: String,
    pub result: String,
    pub at: i64,
}

/// The attempt ledger of a merge-conflict card (website
/// `shared/conflict-ledger.ts`): the latest round's attempts and its final note.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConflictLedger {
    pub attempts: Vec<ConflictAttempt>,
    /// `resolved on attempt N, pushed <sha>` / `not pushed: ...` / `unresolved after N attempts`.
    pub final_note: String,
    pub round: i64,
    /// Attempts made in earlier rounds.
    pub earlier: i64,
}

fn parse_attempt(note: &str) -> Option<ConflictAttempt> {
    let rest = note.trim().strip_prefix("conflict attempt ")?;
    let (nm, rest) = rest.split_once(' ')?;
    let (n, m) = nm.split_once('/')?;
    let (fp, rest) = rest.split_once(':')?;
    if fp.len() != 8 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let (files, result) = rest.rsplit_once("-> ")?;
    Some(ConflictAttempt {
        attempt: n.parse().ok()?,
        of: m.parse().ok()?,
        fingerprint: fp.to_lowercase(),
        files: files.trim().to_string(),
        result: result.trim().to_string(),
        at: 0,
    })
}

/// The card's conflict ledger, from its history notes.
pub fn conflict_ledger(c: &Card) -> ConflictLedger {
    let mut l = ConflictLedger {
        round: 1,
        ..Default::default()
    };
    for h in history(c) {
        let n = h.note.trim();
        if let Some(r) = n.strip_prefix("conflict retry: round ") {
            l.earlier += l.attempts.len() as i64;
            l.attempts.clear();
            l.final_note.clear();
            l.round = r.trim().parse().unwrap_or(l.round + 1);
        } else if let Some(mut a) = parse_attempt(n) {
            a.at = h.at;
            l.attempts.push(a);
        } else if n.starts_with("conflicts resolved on attempt ")
            || n.starts_with("conflicts unresolved after ")
        {
            l.final_note = n.to_string();
        }
    }
    l
}

/// The card's meta with `f` applied to the belai part (created if missing).
fn with_belai(c: &Card, f: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
    let mut m = meta(c);
    if !m.get("belai").is_some_and(Value::is_object) {
        m["belai"] = json!({});
    }
    if let Some(b) = m.get_mut("belai").and_then(Value::as_object_mut) {
        f(b);
    }
    m.to_string()
}

fn push_note(b: &mut serde_json::Map<String, Value>, from: &str, to: &str, note: &str) {
    let mut h: Vec<Value> = b
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    h.push(json!({
        "id": uuid::Uuid::now_v7().to_string(), "from": from, "to": to,
        "at": now_ms(), "note": note,
    }));
    if h.len() > HISTORY_MAX {
        let drop = h.len() - HISTORY_MAX;
        h.drain(..drop);
    }
    b.insert("history".into(), Value::Array(h));
}

fn list_of(c: &Card) -> String {
    match belai_list(c).as_str() {
        "" => match c.status.as_str() {
            "backlog" | "todo" => "backlog".into(),
            "doing" => "in_progress".into(),
            "done" => "done".into(),
            _ => "review".into(),
        },
        l => l.to_string(),
    }
}

fn clear_claim(a: &mut serde_json::Map<String, Value>) {
    for k in ["claimedBy", "claimHost", "claimFrom", "leaseUntil"] {
        a.remove(k);
    }
}

fn agent_mut(b: &mut serde_json::Map<String, Value>) -> &mut serde_json::Map<String, Value> {
    if !b.get("agent").is_some_and(Value::is_object) {
        b.insert("agent".into(), json!({}));
    }
    b.get_mut("agent").and_then(Value::as_object_mut).unwrap()
}

fn check_note(note: &str) -> Result<()> {
    if note.chars().count() > NOTE_MAX {
        bail!("a note is at most {NOTE_MAX} characters");
    }
    Ok(())
}

impl Core {
    pub(crate) async fn card_audit(
        &self,
        card: &Card,
        action: &str,
        actor: &str,
        outcome: &str,
        data: Value,
    ) -> Result<()> {
        self.audit_record(LedgerDraft {
            at: 0,
            kind: "card".into(),
            actor: actor.into(),
            actor_kind: if actor == OWNER { "user" } else { "rule" }.into(),
            msg: format!("{action} {} {outcome}", card.key),
            subject: json!({ "action": action, "cardId": card.id, "key": card.key }),
            payload: json!({ "outcome": outcome, "data": data }),
            origin: "factory".into(),
            node_id: String::new(),
            session_id: String::new(),
            ref_key: None,
        })
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn card_meta_write(
        &self,
        c: &Card,
        base_version: i64,
        p: CardPatch,
        action: &str,
        actor: &str,
        outcome: &str,
        data: Value,
    ) -> Result<Card> {
        let out = self.update_card(&c.id, base_version, p, actor).await?;
        self.card_audit(&out, action, actor, outcome, data).await?;
        Ok(out)
    }

    /// Releases a worker's claim: the card can be claimed again.
    pub async fn release_card_claim(
        &self,
        id: &str,
        base_version: i64,
        actor: &str,
    ) -> Result<Card> {
        let c = self.load_live(id).await?;
        let was = claim(&c);
        if !was.is_claimed() {
            bail!("{} has no claim", c.key);
        }
        let list = list_of(&c);
        let meta = with_belai(&c, |b| {
            clear_claim(agent_mut(b));
            push_note(b, &list, &list, &format!("claim released by {actor}"));
        });
        let p = CardPatch {
            meta_json: Some(meta),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "claim.release",
            actor,
            "released",
            json!({ "claimedBy": was.claimed_by, "claimHost": was.claim_host }),
        )
        .await
    }

    /// Releases every claim (the kill switch's `stop`). Returns how many.
    pub async fn release_all_claims(&self, actor: &str) -> Result<usize> {
        let mut n = 0;
        for c in self.cards().await? {
            if claim(&c).is_claimed()
                && self
                    .release_card_claim(&c.id, c.version, actor)
                    .await
                    .is_ok()
            {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Decides a held card. Approve keeps it in its list; reject (a reason
    /// is required) moves it to blocked.
    pub async fn decide_held_card(
        &self,
        id: &str,
        base_version: i64,
        approve: bool,
        why: &str,
        actor: &str,
    ) -> Result<Card> {
        check_note(why)?;
        let c = self.load_live(id).await?;
        let Some(reason) = checkpoint_reason(&c) else {
            bail!("{} is not held for a decision", c.key);
        };
        if claim(&c).is_claimed() {
            bail!("{} is claimed by a worker", c.key);
        }
        let why = why.trim();
        if !approve && why.is_empty() {
            bail!("a rejection needs a reason");
        }
        let note = match (approve, why.is_empty()) {
            (true, true) => format!("checkpoint {reason} approved"),
            (true, false) => format!("checkpoint {reason} approved: {why}"),
            (false, _) => format!("checkpoint {reason} rejected: {why}"),
        };
        let label = format!("{CHECKPOINT_PREFIX}{reason}");
        let labels: Vec<String> = c.labels.iter().filter(|l| **l != label).cloned().collect();
        let from = list_of(&c);
        let to = if approve {
            from.clone()
        } else {
            "blocked".into()
        };
        let meta = with_belai(&c, |b| {
            if !approve {
                b.insert("list".into(), json!("blocked"));
            }
            push_note(b, &from, &to, &note);
        });
        let p = CardPatch {
            labels: Some(labels),
            status: (!approve).then(|| "todo".to_string()),
            meta_json: Some(meta),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "checkpoint.decide",
            actor,
            if approve { "approved" } else { "rejected" },
            json!({ "reason": reason, "why": why }),
        )
        .await
    }

    /// Moves a card to the Blocked list with a note.
    pub async fn block_card(
        &self,
        id: &str,
        base_version: i64,
        note: &str,
        actor: &str,
    ) -> Result<Card> {
        check_note(note)?;
        let c = self.load_live(id).await?;
        let from = list_of(&c);
        let meta = with_belai(&c, |b| {
            b.insert("list".into(), json!("blocked"));
            clear_claim(agent_mut(b));
            push_note(b, &from, "blocked", note.trim());
        });
        let p = CardPatch {
            status: Some("todo".into()),
            meta_json: Some(meta),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "card.block",
            actor,
            "blocked",
            json!({ "note": note }),
        )
        .await
    }

    /// Retry a blocked card: back to the backlog with its claim, attempts and
    /// the conflicts hold cleared, and a `blocked` label removed.
    pub async fn retry_card(
        &self,
        id: &str,
        base_version: i64,
        note: &str,
        actor: &str,
    ) -> Result<Card> {
        check_note(note)?;
        let c = self.load_live(id).await?;
        let from = list_of(&c);
        let labels: Vec<String> = c
            .labels
            .iter()
            .filter(|l| !crate::needs::is_blocked_label(l) && l.as_str() != CONFLICTS_UNRESOLVED)
            .cloned()
            .collect();
        let text = if note.trim().is_empty() {
            format!("retry by {actor}")
        } else {
            format!("retry by {actor}: {}", note.trim())
        };
        let meta = with_belai(&c, |b| {
            b.insert("list".into(), json!("backlog"));
            let a = agent_mut(b);
            clear_claim(a);
            a.remove("attempts");
            a.remove("bounces");
            push_note(b, &from, "backlog", &text);
        });
        let mut m: Value = serde_json::from_str(&meta).unwrap_or_else(|_| json!({}));
        if let Some(k) = m.get_mut("keleustes").and_then(Value::as_object_mut) {
            k.remove("snoozedUntil");
        }
        let p = CardPatch {
            status: Some("backlog".into()),
            labels: Some(labels),
            meta_json: Some(m.to_string()),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "card.retry",
            actor,
            "queued",
            json!({ "note": note }),
        )
        .await
    }

    /// Dismiss a blocked card: done, with the note `dismissed[: why]`.
    pub async fn dismiss_card(
        &self,
        id: &str,
        base_version: i64,
        why: &str,
        actor: &str,
    ) -> Result<Card> {
        check_note(why)?;
        let c = self.load_live(id).await?;
        let from = list_of(&c);
        let note = if why.trim().is_empty() {
            "dismissed".to_string()
        } else {
            format!("dismissed: {}", why.trim())
        };
        let meta = with_belai(&c, |b| {
            b.insert("list".into(), json!("done"));
            clear_claim(agent_mut(b));
            push_note(b, &from, "done", &note);
        });
        let mut labels = c.labels.clone();
        if !labels.iter().any(|l| l == "not-planned") {
            labels.push("not-planned".into());
        }
        let p = CardPatch {
            status: Some("done".into()),
            labels: Some(labels),
            meta_json: Some(meta),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "card.dismiss",
            actor,
            "dismissed",
            json!({ "why": why }),
        )
        .await
    }

    /// Keeps a blocked card off the triage queue for `days`.
    pub async fn snooze_card(
        &self,
        id: &str,
        base_version: i64,
        days: i64,
        actor: &str,
    ) -> Result<Card> {
        let c = self.load_live(id).await?;
        let until = now_ms() + days.clamp(1, 90) * 86_400_000;
        let mut m = meta(&c);
        if !m.get("keleustes").is_some_and(Value::is_object) {
            m["keleustes"] = json!({});
        }
        m["keleustes"]["snoozedUntil"] = json!(until);
        let p = CardPatch {
            meta_json: Some(m.to_string()),
            ..Default::default()
        };
        self.card_meta_write(
            &c,
            base_version,
            p,
            "card.snooze",
            actor,
            "snoozed",
            json!({ "until": until }),
        )
        .await
    }

    /// Adds a note to the card's history (what the host and workers read).
    pub async fn add_card_note(
        &self,
        id: &str,
        base_version: i64,
        note: &str,
        actor: &str,
    ) -> Result<Card> {
        if note.trim().is_empty() {
            bail!("a note needs text");
        }
        check_note(note)?;
        let c = self.load_live(id).await?;
        let list = list_of(&c);
        let text = note.trim().to_string();
        let meta = with_belai(&c, |b| push_note(b, &list, &list, &text));
        let p = CardPatch {
            meta_json: Some(meta),
            ..Default::default()
        };
        let out = self.update_card(&c.id, base_version, p, actor).await?;
        self.emit(
            kinds::CARD_UPDATED,
            &out.id,
            &json!({ "key": out.key, "version": out.version, "actor": actor, "detail": { "note": true } }),
        )
        .await?;
        self.card_audit(
            &out,
            "card.note",
            actor,
            "noted",
            json!({ "chars": text.chars().count() }),
        )
        .await?;
        Ok(out)
    }

    /// The open merge-conflict card of a pull request, if any.
    pub async fn conflict_card_of(&self, repo: &str, number: i64) -> Result<Option<Card>> {
        Ok(self
            .cards()
            .await?
            .into_iter()
            .find(|c| c.status != "done" && names_conflict(c, repo, number)))
    }

    /// "Resolve conflicts": files a `merge-conflict` card for a pull request
    /// (a fork's card is held for a person). An open card for it is reused.
    /// `body` is the resolver's block (kls-engine `conflicts::card_body`).
    /// Returns the card and whether it was new. Audited.
    pub async fn file_conflict_card(
        &self,
        repo: &str,
        number: i64,
        body: &str,
        fork: bool,
        actor: &str,
    ) -> Result<(Card, bool)> {
        if repo.trim().is_empty() || number <= 0 {
            bail!("name the pull request");
        }
        if let Some(c) = self.conflict_card_of(repo, number).await? {
            return Ok((c, false));
        }
        let mut labels = vec![CONFLICT_LABEL.to_string()];
        if fork {
            labels.push(FORK_HOLD.to_string());
        }
        let c = self
            .create_card(
                super::CardDraft {
                    title: format!("Resolve merge conflicts in {repo}#{number}"),
                    body: body.to_string(),
                    status: "backlog".into(),
                    priority: 1,
                    labels,
                    repo: repo.into(),
                    meta_json: json!({ "keleustes": {
                        "conflictPr": format!("{repo}#{number}"),
                        "agent": "belai:conflict-resolver",
                        "origin": { "kind": "person", "actor": actor },
                    } })
                    .to_string(),
                    ..Default::default()
                },
                actor,
            )
            .await?;
        self.card_audit(
            &c,
            "conflict.card",
            actor,
            "filed",
            json!({ "pull": format!("{repo}#{number}"), "fork": fork }),
        )
        .await?;
        Ok((c, true))
    }
}

/// The label of a merge-conflict card.
pub const CONFLICT_LABEL: &str = "merge-conflict";
/// A fork's conflict card waits for a person.
pub const FORK_HOLD: &str = "checkpoint:fork_code";

/// Whether a card is the merge-conflict card of `repo#number` (its meta, or
/// the `repo:` / `pull:` lines of its body).
pub fn names_conflict(c: &Card, repo: &str, number: i64) -> bool {
    if c.deleted || !c.labels.iter().any(|l| l == CONFLICT_LABEL) {
        return false;
    }
    let want = format!("{repo}#{number}");
    if meta(c)
        .pointer("/keleustes/conflictPr")
        .and_then(Value::as_str)
        == Some(want.as_str())
    {
        return true;
    }
    let line = |k: &str| {
        c.body
            .lines()
            .find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string()))
    };
    line("repo:").as_deref() == Some(repo) && line("pull:") == Some(number.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::CardDraft;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    async fn card(core: &Core, labels: &[&str], meta: Value) -> Card {
        core.create_card(
            CardDraft {
                title: "T".into(),
                status: "backlog".into(),
                labels: labels.iter().map(|s| s.to_string()).collect(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            OWNER,
        )
        .await
        .unwrap()
    }

    async fn ledger(core: &Core, action: &str) -> usize {
        let q = kls_store::audit::LedgerQuery {
            kinds: vec!["card".into()],
            limit: 500,
            ..Default::default()
        };
        core.audit_entries(&q)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.subject["action"] == action)
            .count()
    }

    #[test]
    fn conflict_ledger_latest_round() {
        let notes = json!({"belai": {"history": [
            {"id": "1", "to": "backlog", "at": 1, "note": "conflict attempt 1/5 0123abcd: a.rs=both -> build failed (cargo build)"},
            {"id": "2", "to": "backlog", "at": 2, "note": "conflicts unresolved after 5 attempts"},
            {"id": "3", "to": "backlog", "at": 3, "note": "conflict retry: round 2"},
            {"id": "4", "to": "backlog", "at": 4, "note": "conflict attempt 1/5 89abcdef: a.rs=current, b.rs=incoming -> passed"},
            {"id": "5", "to": "review", "at": 5, "note": "conflicts resolved on attempt 1, pushed 0123456789ab"},
            {"id": "6", "to": "review", "at": 6, "note": "unrelated"}
        ]}});
        let c = Card {
            meta_json: notes.to_string(),
            ..Default::default()
        };
        let l = conflict_ledger(&c);
        assert_eq!((l.round, l.earlier, l.attempts.len()), (2, 1, 1));
        assert_eq!(l.attempts[0].result, "passed");
        assert_eq!(l.attempts[0].files, "a.rs=current, b.rs=incoming");
        assert!(l.final_note.starts_with("conflicts resolved on attempt 1"));
        assert!(parse_attempt("conflict attempt 1/5 nothex!!: x -> y").is_none());
    }

    #[tokio::test]
    async fn claims_lapse_and_release() {
        let (core, _d) = core().await;
        let now = now_ms();
        let c = card(
            &core,
            &["build"],
            json!({"belai": {"hostId": "h1", "agent": {"claimedBy": "belai:builder", "claimHost": "n1", "leaseUntil": now - 1, "attempts": 2, "dependsOn": ["K-1"]}}, "x": 1}),
        )
        .await;
        let cl = claim(&c);
        assert!(cl.lapsed(now));
        assert_eq!(cl.depends_on, ["K-1"]);
        assert!(!is_held(&c));
        let r = core
            .release_card_claim(&c.id, c.version, OWNER)
            .await
            .unwrap();
        assert!(!claim(&r).is_claimed());
        assert_eq!(claim(&r).attempts, 2, "attempts stay");
        let m = meta(&r);
        assert_eq!(m["x"], 1);
        assert_eq!(m["belai"]["hostId"], "h1", "unknown keys kept");
        assert_eq!(history(&r).last().unwrap().note, "claim released by owner");
        assert!(
            core.release_card_claim(&r.id, r.version, OWNER)
                .await
                .is_err()
        );
        assert_eq!(ledger(&core, "claim.release").await, 1);
    }

    #[tokio::test]
    async fn held_card_approve_and_reject() {
        let (core, _d) = core().await;
        let c = card(&core, &["build", "checkpoint:plan_approval"], json!({})).await;
        assert!(is_held(&c));
        assert_eq!(checkpoint_reason(&c).as_deref(), Some("plan_approval"));
        let a = core
            .decide_held_card(&c.id, c.version, true, "", OWNER)
            .await
            .unwrap();
        assert_eq!(a.labels, ["build"]);
        assert_eq!(a.status, "backlog");
        assert_eq!(
            history(&a).last().unwrap().note,
            "checkpoint plan_approval approved"
        );

        let c = card(&core, &["checkpoint:root_cause"], json!({})).await;
        assert!(
            core.decide_held_card(&c.id, c.version, false, " ", OWNER)
                .await
                .is_err()
        );
        let r = core
            .decide_held_card(&c.id, c.version, false, "wrong cause", OWNER)
            .await
            .unwrap();
        assert!(is_blocked(&r));
        assert_eq!(r.status, "todo");
        assert_eq!(belai_list(&r), "blocked");
        assert_eq!(
            history(&r).last().unwrap().note,
            "checkpoint root_cause rejected: wrong cause"
        );
        assert_eq!(ledger(&core, "checkpoint.decide").await, 2);

        // A claimed card is not held.
        let c = card(
            &core,
            &["checkpoint:visual"],
            json!({"belai": {"agent": {"claimedBy": "w"}}}),
        )
        .await;
        assert!(!is_held(&c));
        assert!(
            core.decide_held_card(&c.id, c.version, true, "", OWNER)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn triage_causes_and_actions() {
        let (core, _d) = core().await;
        let now = now_ms();
        let note = |n: &str| json!({"belai": {"list": "blocked", "history": [{"id": "1", "to": "blocked", "at": 1, "note": n}]}});
        let mk = |labels: Vec<String>, m: Value| Card {
            status: "todo".into(),
            labels,
            meta_json: m.to_string(),
            ..Default::default()
        };
        assert_eq!(
            triage_cause(&mk(vec!["checkpoint:x".into()], json!({})), now, 3),
            Cause::WaitingOnHuman
        );
        assert_eq!(
            triage_cause(
                &mk(
                    vec![],
                    json!({"belai": {"agent": {"claimedBy": "w", "leaseUntil": 1}}})
                ),
                now,
                3
            ),
            Cause::ClaimLapsed
        );
        assert_eq!(
            triage_cause(
                &mk(vec![], json!({"belai": {"agent": {"attempts": 3}}})),
                now,
                3
            ),
            Cause::OutOfPasses
        );
        assert_eq!(
            triage_cause(&mk(vec![], note("Tool Bash failed")), now, 3),
            Cause::ToolFailed
        );
        assert_eq!(
            triage_cause(&mk(vec![], note("build error")), now, 3),
            Cause::Errored
        );
        assert_eq!(triage_cause(&mk(vec![], note("hm")), now, 3), Cause::Other);
        assert!(is_blocked(&mk(vec![], note("x"))));
        assert!(is_blocked(&mk(vec!["sf:blocked".into()], json!({}))));

        let c = card(
            &core,
            &["blocked", CONFLICTS_UNRESOLVED],
            json!({"belai": {"agent": {"attempts": 5, "claimedBy": "w"}}}),
        )
        .await;
        let b = core
            .block_card(&c.id, c.version, "stuck", OWNER)
            .await
            .unwrap();
        assert!(is_blocked(&b) && !claim(&b).is_claimed());
        let s = core.snooze_card(&b.id, b.version, 7, OWNER).await.unwrap();
        assert!(snoozed_until(&s).unwrap() > now);
        let r = core.retry_card(&s.id, s.version, "", OWNER).await.unwrap();
        assert_eq!(r.status, "backlog");
        assert!(r.labels.is_empty());
        assert_eq!(claim(&r).attempts, 0);
        assert!(snoozed_until(&r).is_none());
        assert!(!is_blocked(&r));
        let n = core
            .add_card_note(&r.id, r.version, "looked at it", OWNER)
            .await
            .unwrap();
        assert_eq!(history(&n).last().unwrap().note, "looked at it");
        let d = core
            .dismiss_card(&n.id, n.version, "dup", OWNER)
            .await
            .unwrap();
        assert_eq!(
            (
                d.status.as_str(),
                d.labels.contains(&"not-planned".to_string())
            ),
            ("done", true)
        );
        for a in [
            "card.block",
            "card.snooze",
            "card.retry",
            "card.note",
            "card.dismiss",
        ] {
            assert_eq!(ledger(&core, a).await, 1, "{a}");
        }
        assert_eq!(core.release_all_claims(OWNER).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn resolve_conflicts_files_one_card() {
        let (core, _d) = core().await;
        let body = "```conflict\nrepo: acme/api\npull: 7\n```";
        let (c, new) = core
            .file_conflict_card("acme/api", 7, body, false, OWNER)
            .await
            .unwrap();
        assert!(new && c.labels.contains(&CONFLICT_LABEL.to_string()));
        assert!(names_conflict(&c, "acme/api", 7) && !names_conflict(&c, "acme/api", 8));
        let (again, new) = core
            .file_conflict_card("acme/api", 7, body, false, OWNER)
            .await
            .unwrap();
        assert!(!new && again.id == c.id);
        let (fork, _) = core
            .file_conflict_card("acme/api", 9, "", true, OWNER)
            .await
            .unwrap();
        assert!(is_held(&fork));
        assert_eq!(ledger(&core, "conflict.card").await, 2);
        assert_eq!(
            core.conflict_card_of("acme/api", 7)
                .await
                .unwrap()
                .map(|x| x.id),
            Some(c.id)
        );
    }
}
