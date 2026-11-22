//! Domain: board (kanban), repository memory, A2A relay and factory
//! control (stop/resume, A2A blackout). Persistence is `kls_store::board`.
//!
//! Every change emits an event (`kinds`). `factory.control` is also what
//! kls-node listens for to push `FactoryControl` frames to the nodes.

use crate::Core;
use anyhow::{Result, bail};
use kls_store::board::{
    A2aMessage, A2aThread, Card, CardActivity, CardWrite, FactoryControl, MemoryNote, MemoryScope,
};
use kls_store::github::GhIssue;
use kls_store::now_ms;
use serde_json::json;

pub use kls_store::board as model;

pub mod cards;
/// The external kanban binding (agent A).
pub mod kanban;
pub mod rules;
pub mod sync;
pub use cards::{Cause, Claim, HistoryNote};

pub mod kinds {
    pub const CARD_CREATED: &str = "card.created";
    pub const CARD_UPDATED: &str = "card.updated";
    pub const CARD_MOVED: &str = "card.moved";
    pub const CARD_ASSIGNED: &str = "card.assigned";
    pub const CARD_LABELED: &str = "card.labeled";
    pub const CARD_DELETED: &str = "card.deleted";
    pub const CARD_DISPATCHED: &str = "card.dispatched";
    pub const MEMORY_CREATED: &str = "memory.created";
    pub const MEMORY_UPDATED: &str = "memory.updated";
    pub const MEMORY_DELETED: &str = "memory.deleted";
    pub const MEMORY_CLEARED: &str = "memory.cleared";
    /// Data `{seq, from, to, threadId}`; never the text.
    pub const A2A_SENT: &str = "a2a.sent";
    pub const A2A_WITHHELD: &str = "a2a.withheld";
    /// Data `{stopped, a2aBlackout, reason}`.
    pub const FACTORY_CONTROL: &str = "factory.control";
}

/// Board columns, in display order. The web factory's lists map onto them
/// as belai does (`factorylink.statusFor`): backlog -> backlog,
/// in_progress -> doing, review -> review, done -> done, and blocked ->
/// todo with `belai.list = "blocked"` in the card's meta (shown as the
/// Blocked list, see [`cards::is_blocked`]); triage is Keleustes' intake.
pub const STATUSES: [&str; 6] = ["triage", "backlog", "todo", "doing", "review", "done"];
pub const PRIORITY_MIN: i32 = -2;
pub const PRIORITY_MAX: i32 = 3;
pub const TITLE_MAX: usize = 300;
pub const BODY_MAX: usize = 64 * 1024;
pub const NOTE_TEXT_MAX: usize = 4000;
pub const NOTE_TAGS_MAX: usize = 20;
pub const TAG_MAX: usize = 64;
pub const SCOPE_MAX: usize = 200;
pub const REASON_MAX: usize = 500;
pub const A2A_TEXT_MAX: usize = 4096;
pub const LABELS_MAX: usize = 32;
/// The console author on notes and activity written from this app.
pub const OWNER: &str = "owner";

/// An edit based on a stale version. Carries the stored copy; get it with
/// `err.downcast_ref::<VersionConflict>()`.
#[derive(Debug, Clone)]
pub struct VersionConflict {
    pub current: Card,
}

impl std::fmt::Display for VersionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "card {} changed meanwhile (now version {}); reload and try again",
            self.current.key, self.current.version
        )
    }
}
impl std::error::Error for VersionConflict {}

/// Fields for a new card. Empty status means "triage".
#[derive(Clone, Debug, Default)]
pub struct CardDraft {
    pub title: String,
    pub body: String,
    pub status: String,
    pub priority: i32,
    pub labels: Vec<String>,
    pub assignee: String,
    pub repo: String,
    pub branch: String,
    pub pr_url: String,
    pub meta_json: String,
}

/// A partial card edit; `None` leaves a field as it is.
#[derive(Clone, Debug, Default)]
pub struct CardPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<String>,
    pub priority: Option<i32>,
    pub labels: Option<Vec<String>>,
    pub assignee: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub meta_json: Option<String>,
}

/// Outcome of a node's card push.
#[derive(Clone, Debug, PartialEq)]
pub enum PushOutcome {
    Accepted(Card),
    /// Stale base version: the server copy.
    Conflict(Card),
}

/// What an A2A send came to.
#[derive(Clone, Debug, PartialEq)]
pub struct A2aSend {
    pub message: Option<A2aMessage>,
    pub delivered: bool,
    /// Why it was refused ("" when it was not).
    pub refused: String,
}

fn clean_labels(labels: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in labels {
        let l = l.trim().to_string();
        if !l.is_empty() && !out.contains(&l) {
            out.push(l);
        }
    }
    if out.len() > LABELS_MAX {
        bail!("a card holds at most {LABELS_MAX} labels");
    }
    Ok(out)
}

fn check_card(c: &Card) -> Result<()> {
    if c.title.trim().is_empty() {
        bail!("a card needs a title");
    }
    if c.title.chars().count() > TITLE_MAX {
        bail!("title is longer than {TITLE_MAX} characters");
    }
    if c.body.len() > BODY_MAX {
        bail!("body is longer than {BODY_MAX} bytes");
    }
    if !STATUSES.contains(&c.status.as_str()) {
        bail!("unknown status {:?}", c.status);
    }
    if !(PRIORITY_MIN..=PRIORITY_MAX).contains(&c.priority) {
        bail!("priority is {PRIORITY_MIN} to {PRIORITY_MAX}");
    }
    Ok(())
}

fn clean_tags(tags: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_string();
        if t.is_empty() || out.contains(&t) {
            continue;
        }
        if t.chars().count() > TAG_MAX {
            bail!("a tag is at most {TAG_MAX} characters");
        }
        out.push(t);
    }
    if out.len() > NOTE_TAGS_MAX {
        bail!("a note holds at most {NOTE_TAGS_MAX} tags");
    }
    Ok(out)
}

fn check_note_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("a note needs text");
    }
    if text.chars().count() > NOTE_TEXT_MAX {
        bail!("a note is at most {NOTE_TEXT_MAX} characters");
    }
    Ok(())
}

fn check_scope(scope: &str) -> Result<()> {
    if scope.trim().is_empty() || scope.chars().count() > SCOPE_MAX {
        bail!("a scope is 1 to {SCOPE_MAX} characters");
    }
    Ok(())
}

fn check_reason(reason: &str) -> Result<()> {
    if reason.chars().count() > REASON_MAX {
        bail!("a reason is at most {REASON_MAX} characters");
    }
    Ok(())
}

/// The protocol's single `reason`: the stop reason while stopped, else the
/// blackout reason.
pub fn control_reason(c: &FactoryControl) -> String {
    if c.stopped {
        c.stopped_reason.clone()
    } else {
        c.a2a_reason.clone()
    }
}

/// The card's prompt for "Send to session".
pub fn card_prompt(c: &Card) -> String {
    let mut p = format!("Work on board card {}: {}", c.key, c.title);
    if !c.body.trim().is_empty() {
        p.push_str("\n\n");
        p.push_str(c.body.trim());
    }
    if !c.labels.is_empty() {
        p.push_str(&format!("\n\nLabels: {}", c.labels.join(", ")));
    }
    p
}

/// Whether an envelope is for one of `agents` (ids `<nodeId>:<sessionId>`):
/// addressed to one of them, to their node (`<nodeId>` or `<nodeId>:*`), or
/// a broadcast (empty `to`) not sent by one of them.
pub fn a2a_for(m: &A2aMessage, agents: &[String]) -> bool {
    if m.to.is_empty() || m.to == "*" {
        return !agents.iter().any(|a| a == &m.from);
    }
    agents.iter().any(|a| {
        a == &m.to || {
            let node = a.split(':').next().unwrap_or("");
            !node.is_empty() && (m.to == node || m.to == format!("{node}:*"))
        }
    })
}

/// The mirror row of an issue a person unlinked from its card.
pub const UNLINKED_PREFIX: &str = "unlinked:";

/// A mirrored issue comment, as the GitHub plane emitted it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueComment {
    pub at: i64,
    pub author: String,
    pub association: String,
    /// From the team (`None` unknown).
    pub internal: Option<bool>,
    pub body: String,
    pub url: String,
}

/// One Sync tab row: the conflict and, per field, the base value both sides
/// last agreed on (`None` when unknown).
#[derive(Clone, Debug, PartialEq)]
pub struct SyncRow {
    pub conflict: sync::SyncConflict,
    pub base: Vec<Option<String>>,
}

/// `(repo, number)` from a card's `github` meta.
pub fn github_ref(c: &Card) -> Option<(String, i64)> {
    let m: serde_json::Value = serde_json::from_str(&c.meta_json).ok()?;
    let g = m.get("github")?;
    let repo = g.get("repo")?.as_str()?.to_string();
    let n = g.get("number")?.as_i64()?;
    (!repo.is_empty() && n > 0).then_some((repo, n))
}

/// The value of `field` the board and GitHub last agreed on.
pub fn sync_base(c: &Card, field: &str) -> Option<String> {
    let m: serde_json::Value = serde_json::from_str(&c.meta_json).ok()?;
    m.pointer(&format!("/keleustes/syncBase/{field}"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

impl Core {
    // ---- cards -------------------------------------------------------------

    pub async fn cards(&self) -> Result<Vec<Card>> {
        self.store().cards(false).await
    }

    pub async fn card(&self, id_or_key: &str) -> Result<Option<Card>> {
        self.store().card(id_or_key).await
    }

    pub async fn card_activity(&self, card_id: &str) -> Result<Vec<CardActivity>> {
        self.store().card_activity(card_id, 200).await
    }

    async fn new_card_key(&self, id: &str) -> Result<String> {
        // uuid v7 starts with the clock; take the random tail instead.
        let hex: String = id.chars().filter(|c| *c != '-').collect();
        for i in 0..(hex.len() - 6) {
            let start = hex.len() - 6 - i;
            let k = format!("K-{}", &hex[start..start + 6]);
            if !self.store().card_key_taken(&k).await? {
                return Ok(k);
            }
        }
        Ok(format!("K-{hex}"))
    }

    pub async fn create_card(&self, d: CardDraft, actor: &str) -> Result<Card> {
        let id = uuid::Uuid::now_v7().to_string();
        let card = Card {
            key: self.new_card_key(&id).await?,
            id,
            title: d.title.trim().to_string(),
            body: d.body,
            status: if d.status.is_empty() {
                "triage".into()
            } else {
                d.status
            },
            priority: d.priority,
            labels: clean_labels(d.labels)?,
            assignee: d.assignee.trim().to_string(),
            repo: d.repo.trim().to_string(),
            branch: d.branch.trim().to_string(),
            pr_url: d.pr_url.trim().to_string(),
            meta_json: d.meta_json,
            ..Default::default()
        };
        check_card(&card)?;
        let card = self.store().insert_card(&card).await?;
        self.card_changed(
            &card,
            kinds::CARD_CREATED,
            actor,
            json!({ "status": card.status, "ticket": rules::gap_codes(&card) }),
        )
        .await?;
        Ok(card)
    }

    async fn card_changed(
        &self,
        card: &Card,
        kind: &str,
        actor: &str,
        detail: serde_json::Value,
    ) -> Result<()> {
        let what = kind.trim_start_matches("card.");
        self.store()
            .add_card_activity(&card.id, actor, what, &detail.to_string())
            .await?;
        self.emit(
            kind,
            &card.id,
            &json!({ "key": card.key, "version": card.version, "actor": actor, "detail": detail }),
        )
        .await?;
        Ok(())
    }

    async fn write_card(&self, card: Card, base_version: i64) -> Result<Card> {
        check_card(&card)?;
        match self.store().update_card(&card, base_version).await? {
            CardWrite::Written(c) => Ok(c),
            CardWrite::Conflict(Some(current)) => Err(VersionConflict { current }.into()),
            CardWrite::Conflict(None) => bail!("no such card"),
        }
    }

    async fn load_live(&self, id: &str) -> Result<Card> {
        match self.store().card(id).await? {
            Some(c) if !c.deleted => Ok(c),
            Some(_) => bail!("card {id} is deleted"),
            None => bail!("no such card {id}"),
        }
    }

    /// Applies a partial edit made against `base_version`. Emits the most
    /// specific kind (moved, assigned, labeled, else updated).
    pub async fn update_card(
        &self,
        id: &str,
        base_version: i64,
        p: CardPatch,
        actor: &str,
    ) -> Result<Card> {
        let old = self.load_live(id).await?;
        let mut c = old.clone();
        if let Some(v) = p.title {
            c.title = v.trim().to_string();
        }
        if let Some(v) = p.body {
            c.body = v;
        }
        if let Some(v) = p.status {
            c.status = v;
        }
        if let Some(v) = p.priority {
            c.priority = v;
        }
        if let Some(v) = p.labels {
            c.labels = clean_labels(v)?;
        }
        if let Some(v) = p.assignee {
            c.assignee = v.trim().to_string();
        }
        if let Some(v) = p.repo {
            c.repo = v.trim().to_string();
        }
        if let Some(v) = p.branch {
            c.branch = v.trim().to_string();
        }
        if let Some(v) = p.pr_url {
            c.pr_url = v.trim().to_string();
        }
        if let Some(v) = p.meta_json {
            c.meta_json = v;
        }
        let new = self.write_card(c, base_version).await?;
        let mut changed = Vec::new();
        for (name, diff) in [
            ("title", old.title != new.title),
            ("body", old.body != new.body),
            ("status", old.status != new.status),
            ("priority", old.priority != new.priority),
            ("labels", old.labels != new.labels),
            ("assignee", old.assignee != new.assignee),
            ("repo", old.repo != new.repo),
            ("branch", old.branch != new.branch),
            ("prUrl", old.pr_url != new.pr_url),
            ("meta", old.meta_json != new.meta_json),
        ] {
            if diff {
                changed.push(name);
            }
        }
        let (kind, detail) = match changed.as_slice() {
            ["status"] => (
                kinds::CARD_MOVED,
                json!({ "from": old.status, "to": new.status }),
            ),
            ["assignee"] => (
                kinds::CARD_ASSIGNED,
                json!({ "from": old.assignee, "to": new.assignee }),
            ),
            ["labels"] => (kinds::CARD_LABELED, json!({ "labels": new.labels })),
            _ if changed.contains(&"status") => (
                kinds::CARD_MOVED,
                json!({ "from": old.status, "to": new.status, "fields": changed }),
            ),
            _ => (kinds::CARD_UPDATED, json!({ "fields": changed })),
        };
        let mut detail = detail;
        if old.title != new.title || old.body != new.body {
            detail["ticket"] = json!(rules::gap_codes(&new));
        }
        self.card_changed(&new, kind, actor, detail).await?;
        crate::classify::after_write(self, &old, &new);
        Ok(new)
    }

    pub async fn move_card(
        &self,
        id: &str,
        base_version: i64,
        status: &str,
        actor: &str,
    ) -> Result<Card> {
        let p = CardPatch {
            status: Some(status.into()),
            ..Default::default()
        };
        self.update_card(id, base_version, p, actor).await
    }

    pub async fn assign_card(
        &self,
        id: &str,
        base_version: i64,
        assignee: &str,
        actor: &str,
    ) -> Result<Card> {
        let p = CardPatch {
            assignee: Some(assignee.into()),
            ..Default::default()
        };
        self.update_card(id, base_version, p, actor).await
    }

    pub async fn set_card_labels(
        &self,
        id: &str,
        base_version: i64,
        labels: Vec<String>,
        actor: &str,
    ) -> Result<Card> {
        let p = CardPatch {
            labels: Some(labels),
            ..Default::default()
        };
        self.update_card(id, base_version, p, actor).await
    }

    /// Soft-deletes a card (nodes pull the tombstone).
    pub async fn delete_card(&self, id: &str, base_version: i64, actor: &str) -> Result<Card> {
        let mut c = self.load_live(id).await?;
        c.deleted = true;
        let c = self.write_card(c, base_version).await?;
        self.card_changed(&c, kinds::CARD_DELETED, actor, json!({}))
            .await?;
        Ok(c)
    }

    /// Records that a card was sent to a node (activity + event).
    pub async fn note_card_dispatch(
        &self,
        card: &Card,
        node_id: &str,
        dispatch_id: &str,
    ) -> Result<()> {
        self.card_changed(
            card,
            kinds::CARD_DISPATCHED,
            OWNER,
            json!({ "nodeId": node_id, "dispatchId": dispatch_id }),
        )
        .await
    }

    /// A node's edit of a whole item, based on `item.version`. A new id is
    /// created (keeping the node's key when free); a stale version is a
    /// conflict carrying the server copy.
    pub async fn push_card(&self, mut item: Card, actor: &str) -> Result<PushOutcome> {
        item.labels = clean_labels(std::mem::take(&mut item.labels))?;
        match self.store().card(&item.id).await? {
            None => {
                if item.id.is_empty() {
                    item.id = uuid::Uuid::now_v7().to_string();
                }
                if item.key.is_empty() || self.store().card_key_taken(&item.key).await? {
                    item.key = self.new_card_key(&item.id).await?;
                }
                if item.status.is_empty() {
                    item.status = "triage".into();
                }
                check_card(&item)?;
                let c = self.store().insert_card(&item).await?;
                self.card_changed(
                    &c,
                    kinds::CARD_CREATED,
                    actor,
                    json!({ "status": c.status, "ticket": rules::gap_codes(&c) }),
                )
                .await?;
                Ok(PushOutcome::Accepted(c))
            }
            Some(cur) if cur.version != item.version || cur.deleted => {
                Ok(PushOutcome::Conflict(cur))
            }
            Some(cur) => {
                item.key = cur.key.clone();
                check_card(&item)?;
                match self.store().update_card(&item, cur.version).await? {
                    CardWrite::Written(c) => {
                        let (kind, detail) = if c.deleted {
                            (kinds::CARD_DELETED, json!({}))
                        } else if c.status != cur.status {
                            (
                                kinds::CARD_MOVED,
                                json!({ "from": cur.status, "to": c.status }),
                            )
                        } else {
                            (kinds::CARD_UPDATED, json!({ "source": "node" }))
                        };
                        self.card_changed(&c, kind, actor, detail).await?;
                        crate::classify::after_write(self, &cur, &c);
                        Ok(PushOutcome::Accepted(c))
                    }
                    CardWrite::Conflict(Some(c)) => Ok(PushOutcome::Conflict(c)),
                    CardWrite::Conflict(None) => bail!("card vanished"),
                }
            }
        }
    }

    // ---- GitHub link (the drawer's GitHub tab) ------------------------------

    /// The mirrored issue a card is linked to: by its `github` meta, else by
    /// the mirror's `card_id`.
    pub async fn card_issue(&self, card: &Card) -> Result<Option<GhIssue>> {
        if let Some((repo, n)) = github_ref(card)
            && let Some(i) = self.store().gh_issue(&repo, n).await?
        {
            return Ok(Some(i));
        }
        let repo = (!card.repo.is_empty()).then_some(card.repo.as_str());
        Ok(self
            .store()
            .gh_issues(repo, None)
            .await?
            .into_iter()
            .find(|i| i.card_id.as_deref() == Some(card.id.as_str())))
    }

    /// The newest mirrored comments of an issue (`github.issue.comment`
    /// events), newest first.
    pub async fn issue_comments(
        &self,
        repo: &str,
        number: i64,
        limit: usize,
    ) -> Result<Vec<IssueComment>> {
        let subject = format!("{repo}#{number}");
        let evs = self
            .store()
            .events_query(&kls_store::audit::EventQuery {
                prefixes: vec![crate::github::kinds::ISSUE_COMMENT.to_string()],
                text: Some(subject.clone()),
                limit: 500,
                ..Default::default()
            })
            .await?;
        Ok(evs
            .into_iter()
            .filter(|e| e.subject == subject)
            .take(limit)
            .map(|e| {
                let v: serde_json::Value = serde_json::from_str(&e.data_json).unwrap_or_default();
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                IssueComment {
                    at: e.at,
                    author: s("author"),
                    association: s("authorAssociation"),
                    internal: v.get("internal").and_then(|x| x.as_bool()),
                    body: s("body"),
                    url: s("url"),
                }
            })
            .collect())
    }

    /// Links a card to an issue (one the owner just opened from the card, or
    /// an existing one): the mirror row points at the card and the card's
    /// meta names the issue. Audited.
    pub async fn link_card_issue(
        &self,
        card_id: &str,
        repo: &str,
        number: i64,
        url: &str,
        actor: &str,
    ) -> Result<Card> {
        if repo.trim().is_empty() || number <= 0 {
            bail!("name the repository and the issue number");
        }
        let c = self.load_live(card_id).await?;
        match self.store().gh_issue(repo, number).await? {
            Some(_) => self.store().gh_link_issue_card(repo, number, &c.id).await?,
            None => {
                // A placeholder row the next sync fills in; updated_at 0
                // keeps the sync cursor where it is.
                self.store()
                    .gh_upsert_issue(&GhIssue {
                        repo: repo.into(),
                        number,
                        title: c.title.clone(),
                        state: "open".into(),
                        html_url: url.into(),
                        card_id: Some(c.id.clone()),
                        synced_at: now_ms(),
                        ..Default::default()
                    })
                    .await?;
            }
        }
        let mut m: serde_json::Value =
            serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
        if !m.is_object() {
            m = json!({});
        }
        m["github"] = json!({ "repo": repo, "number": number, "url": url });
        let p = CardPatch {
            meta_json: Some(m.to_string()),
            repo: c.repo.is_empty().then(|| repo.to_string()),
            ..Default::default()
        };
        let out = self.update_card(&c.id, c.version, p, actor).await?;
        self.card_audit(
            &out,
            "card.github.link",
            actor,
            "linked",
            json!({ "repo": repo, "number": number, "url": url }),
        )
        .await?;
        Ok(out)
    }

    /// Unlinks a card from its issue: the card stops following it. The mirror
    /// row keeps an `unlinked:<card id>` marker so the link is not guessed
    /// again. Audited.
    pub async fn unlink_card_issue(&self, card_id: &str, actor: &str) -> Result<Card> {
        let c = self.load_live(card_id).await?;
        let Some(i) = self.card_issue(&c).await? else {
            bail!("{} is not linked to an issue", c.key);
        };
        self.store()
            .gh_link_issue_card(&i.repo, i.number, &format!("{UNLINKED_PREFIX}{}", c.id))
            .await?;
        let mut m: serde_json::Value =
            serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
        if let Some(o) = m.as_object_mut() {
            o.remove("github");
        }
        let p = CardPatch {
            meta_json: Some(m.to_string()),
            ..Default::default()
        };
        let out = self.update_card(&c.id, c.version, p, actor).await?;
        self.card_audit(
            &out,
            "card.github.unlink",
            actor,
            "unlinked",
            json!({ "repo": i.repo, "number": i.number }),
        )
        .await?;
        Ok(out)
    }

    // ---- three-way sync conflicts -------------------------------------------

    /// The Sync tab's rows with the base (last agreed) value of each field.
    pub async fn sync_conflicts_3way(&self) -> Result<Vec<SyncRow>> {
        let mut out = Vec::new();
        for c in self.sync_conflicts().await? {
            let card = self.store().card(&c.card_id).await?;
            let base = c
                .fields
                .iter()
                .map(|f| card.as_ref().and_then(|x| sync_base(x, &f.field)))
                .collect();
            out.push(SyncRow { conflict: c, base });
        }
        Ok(out)
    }

    /// Resolves one field (see [`Core::resolve_sync_conflict`]) and records
    /// the value both sides now agree on as the field's base.
    pub async fn resolve_sync_field(
        &self,
        card_id: &str,
        field: &str,
        take_github: bool,
    ) -> Result<Card> {
        let before = self
            .sync_conflicts()
            .await?
            .into_iter()
            .find(|x| x.card_id == card_id)
            .and_then(|x| x.fields.into_iter().find(|f| f.field == field));
        let c = self
            .resolve_sync_conflict(card_id, field, take_github)
            .await?;
        let Some(f) = before else {
            return Ok(c);
        };
        let agreed = if take_github { f.github } else { f.board };
        let mut m: serde_json::Value =
            serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
        if !m.is_object() {
            m = json!({});
        }
        if !m.get("keleustes").is_some_and(|v| v.is_object()) {
            m["keleustes"] = json!({});
        }
        if !m["keleustes"]
            .get("syncBase")
            .is_some_and(|v| v.is_object())
        {
            m["keleustes"]["syncBase"] = json!({});
        }
        m["keleustes"]["syncBase"][field] = json!(agreed);
        let p = CardPatch {
            meta_json: Some(m.to_string()),
            ..Default::default()
        };
        self.update_card(&c.id, c.version, p, OWNER).await
    }

    // ---- memory ------------------------------------------------------------

    pub async fn memory_scopes(&self) -> Result<Vec<MemoryScope>> {
        self.store().memory_scopes().await
    }

    pub async fn notes(
        &self,
        scope: Option<&str>,
        q: &str,
        include_deleted: bool,
    ) -> Result<Vec<MemoryNote>> {
        self.store().notes(scope, q, include_deleted, 500).await
    }

    async fn note_event(&self, kind: &str, n: &MemoryNote) -> Result<()> {
        // Ids and scope only, never the text.
        self.emit(
            kind,
            &n.id,
            &json!({ "scope": n.scope, "author": n.author }),
        )
        .await?;
        Ok(())
    }

    pub async fn add_note(
        &self,
        scope: &str,
        text: &str,
        tags: Vec<String>,
        author: &str,
    ) -> Result<MemoryNote> {
        check_scope(scope)?;
        check_note_text(text)?;
        let now = now_ms();
        let n = MemoryNote {
            id: uuid::Uuid::now_v7().to_string(),
            scope: scope.trim().into(),
            text: text.into(),
            tags: clean_tags(tags)?,
            author: author.into(),
            created_at: now,
            updated_at: now,
            ..Default::default()
        };
        let (n, _) = self.store().upsert_note(&n).await?;
        self.note_event(kinds::MEMORY_CREATED, &n).await?;
        self.enforce_memory_cap(&n.scope).await?;
        Ok(n)
    }

    /// Edits a live note; bumps `updated_at` past any host copy.
    pub async fn edit_note(
        &self,
        id: &str,
        text: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<MemoryNote> {
        let Some(mut n) = self.store().note(id).await? else {
            bail!("no such note");
        };
        if n.deleted {
            bail!("the note is deleted");
        }
        if let Some(t) = text {
            check_note_text(t)?;
            n.text = t.into();
        }
        if let Some(t) = tags {
            n.tags = clean_tags(t)?;
        }
        n.updated_at = now_ms().max(n.updated_at + 1);
        let (n, _) = self.store().upsert_note(&n).await?;
        self.note_event(kinds::MEMORY_UPDATED, &n).await?;
        Ok(n)
    }

    pub async fn delete_note(&self, id: &str) -> Result<MemoryNote> {
        let Some(mut n) = self.store().note(id).await? else {
            bail!("no such note");
        };
        if n.deleted {
            bail!("the note is already deleted");
        }
        n.deleted = true;
        n.updated_at = now_ms().max(n.updated_at + 1);
        let (n, _) = self.store().upsert_note(&n).await?;
        self.note_event(kinds::MEMORY_DELETED, &n).await?;
        Ok(n)
    }

    /// Tombstones every live note of a scope; hosts drop them at their next pull.
    pub async fn clear_memory_scope(&self, scope: &str) -> Result<u64> {
        let n = self.store().clear_memory_scope(scope).await?;
        self.emit(
            kinds::MEMORY_CLEARED,
            scope,
            &json!({ "scope": scope, "cleared": n }),
        )
        .await?;
        Ok(n)
    }

    /// A host's copy of a note (replica rule: newest `updated_at` wins, a
    /// tombstone is final). Not stopped by the A2A blackout. Returns the
    /// stored copy.
    pub async fn push_note(&self, mut n: MemoryNote) -> Result<MemoryNote> {
        check_scope(&n.scope)?;
        if !n.deleted {
            check_note_text(&n.text)?;
        }
        n.tags = clean_tags(std::mem::take(&mut n.tags))?;
        if n.id.is_empty() {
            n.id = uuid::Uuid::now_v7().to_string();
        }
        let now = now_ms();
        if n.created_at == 0 {
            n.created_at = now;
        }
        if n.updated_at == 0 {
            n.updated_at = n.created_at;
        }
        let (stored, changed) = self.store().upsert_note(&n).await?;
        if changed {
            let kind = if stored.deleted {
                kinds::MEMORY_DELETED
            } else if stored.created_at == stored.updated_at {
                kinds::MEMORY_CREATED
            } else {
                kinds::MEMORY_UPDATED
            };
            self.note_event(kind, &stored).await?;
            if !stored.deleted {
                self.enforce_memory_cap(&stored.scope).await?;
            }
        }
        Ok(stored)
    }

    // ---- A2A ---------------------------------------------------------------

    /// Relays an envelope. Refused (nothing stored) while the A2A blackout
    /// is on. Idempotent on `id`.
    pub async fn send_a2a(&self, mut m: A2aMessage) -> Result<A2aSend> {
        if self.factory_control().await?.a2a_blackout {
            self.emit(
                kinds::A2A_WITHHELD,
                &m.id,
                &json!({ "from": m.from, "to": m.to }),
            )
            .await?;
            return Ok(A2aSend {
                message: None,
                delivered: false,
                refused: "a2a blackout".into(),
            });
        }
        if m.from.trim().is_empty() {
            bail!("an envelope needs a sender");
        }
        if m.text.len() > A2A_TEXT_MAX {
            bail!("an envelope carries at most {A2A_TEXT_MAX} bytes of text");
        }
        if m.id.is_empty() {
            m.id = format!("a2a-{}", uuid::Uuid::now_v7().simple());
        }
        if m.thread_id.is_empty() {
            m.thread_id = m.id.clone();
        }
        if m.at == 0 {
            m.at = now_ms();
        }
        let (stored, new) = self.store().insert_a2a(&m).await?;
        if new {
            self.emit(
                kinds::A2A_SENT,
                &stored.id,
                &json!({ "seq": stored.seq, "from": stored.from, "to": stored.to, "threadId": stored.thread_id }),
            )
            .await?;
        }
        Ok(A2aSend {
            message: Some(stored),
            delivered: true,
            refused: String::new(),
        })
    }

    /// Envelopes after `after` addressed to any of `agents` (see [`a2a_for`]),
    /// oldest first, plus the cursor to pass next time. Empty during the
    /// blackout (the cursor stays).
    pub async fn a2a_inbox(&self, agents: &[String], after: i64) -> Result<(Vec<A2aMessage>, i64)> {
        if self.factory_control().await?.a2a_blackout {
            return Ok((Vec::new(), after));
        }
        let rows = self.store().a2a_after(after, 500).await?;
        let cursor = rows.last().map(|m| m.seq).unwrap_or(after);
        Ok((
            rows.into_iter().filter(|m| a2a_for(m, agents)).collect(),
            cursor,
        ))
    }

    pub async fn a2a_threads(&self, limit: i64) -> Result<Vec<A2aThread>> {
        self.store().a2a_threads(limit).await
    }

    pub async fn a2a_thread(&self, thread_id: &str) -> Result<Vec<A2aMessage>> {
        self.store().a2a_thread(thread_id).await
    }

    // ---- factory control ---------------------------------------------------

    pub async fn factory_control(&self) -> Result<FactoryControl> {
        self.store().factory_control().await
    }

    /// `by` is the actor the ledger records (`owner`, `owner:tray`).
    async fn control_changed(&self, by: &str) -> Result<FactoryControl> {
        let c = self.factory_control().await?;
        self.emit(
            kinds::FACTORY_CONTROL,
            "factory",
            &json!({ "stopped": c.stopped, "a2aBlackout": c.a2a_blackout, "reason": control_reason(&c), "by": by }),
        )
        .await?;
        Ok(c)
    }

    /// Stop (true) or resume (false) every automation. Errors when already
    /// in that state.
    pub async fn set_factory_stopped(&self, stopped: bool, reason: &str) -> Result<FactoryControl> {
        self.set_factory_stopped_by(stopped, reason, "owner").await
    }

    /// [`Self::set_factory_stopped`] recording the actor `by`.
    pub async fn set_factory_stopped_by(
        &self,
        stopped: bool,
        reason: &str,
        by: &str,
    ) -> Result<FactoryControl> {
        check_reason(reason)?;
        if !self.store().set_factory_stopped(stopped, reason).await? {
            bail!(
                "the factory is already {}",
                if stopped { "stopped" } else { "running" }
            );
        }
        self.control_changed(by).await
    }

    /// Turns the A2A blackout on or off. Errors when already in that state.
    pub async fn set_a2a_blackout(&self, on: bool, reason: &str) -> Result<FactoryControl> {
        self.set_a2a_blackout_by(on, reason, "owner").await
    }

    /// [`Self::set_a2a_blackout`] recording the actor `by`.
    pub async fn set_a2a_blackout_by(
        &self,
        on: bool,
        reason: &str,
        by: &str,
    ) -> Result<FactoryControl> {
        check_reason(reason)?;
        if !self.store().set_a2a_blackout(on, reason).await? {
            bail!(
                "the A2A blackout is already {}",
                if on { "on" } else { "off" }
            );
        }
        self.control_changed(by).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn draft(title: &str) -> CardDraft {
        CardDraft {
            title: title.into(),
            status: "todo".into(),
            labels: vec!["bug".into(), " bug ".into(), "".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn card_lifecycle_with_versions_and_events() {
        let (core, _d) = core().await;
        let mut rx = core.subscribe();
        let c = core.create_card(draft("Fix login"), OWNER).await.unwrap();
        assert_eq!(c.labels, vec!["bug".to_string()]);
        assert!(c.key.starts_with("K-") && c.key.len() == 8);
        assert_eq!(rx.recv().await.unwrap().kind, kinds::CARD_CREATED);

        let c2 = core.move_card(&c.id, 1, "doing", OWNER).await.unwrap();
        assert_eq!(c2.version, 2);
        assert_eq!(rx.recv().await.unwrap().kind, kinds::CARD_MOVED);

        // Stale version is a typed conflict with the server copy.
        let err = core.assign_card(&c.id, 1, "ana", OWNER).await.unwrap_err();
        let conflict = err.downcast_ref::<VersionConflict>().unwrap();
        assert_eq!(conflict.current.version, 2);

        let c3 = core.assign_card(&c.id, 2, "ana", OWNER).await.unwrap();
        assert_eq!(rx.recv().await.unwrap().kind, kinds::CARD_ASSIGNED);
        let c4 = core
            .set_card_labels(&c.id, c3.version, vec!["ui".into()], OWNER)
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap().kind, kinds::CARD_LABELED);
        assert!(
            core.move_card(&c.id, c4.version, "nope", OWNER)
                .await
                .is_err()
        );
        let p = CardPatch {
            priority: Some(9),
            ..Default::default()
        };
        assert!(core.update_card(&c.id, c4.version, p, OWNER).await.is_err());

        core.delete_card(&c.id, c4.version, OWNER).await.unwrap();
        assert!(core.cards().await.unwrap().is_empty());
        let log = core.card_activity(&c.id).await.unwrap();
        let kinds: Vec<_> = log.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["deleted", "labeled", "assigned", "moved", "created"]
        );
        assert!(core.create_card(draft("  "), OWNER).await.is_err());
    }

    #[tokio::test]
    async fn node_push_accepts_or_conflicts() {
        let (core, _d) = core().await;
        let c = core.create_card(draft("A"), OWNER).await.unwrap();
        let mut edit = c.clone();
        edit.status = "review".into();
        let PushOutcome::Accepted(a) = core.push_card(edit.clone(), "n1").await.unwrap() else {
            panic!("accepted");
        };
        assert_eq!((a.version, a.status.as_str()), (2, "review"));
        // Same base again is stale now.
        assert!(matches!(
            core.push_card(edit, "n1").await.unwrap(),
            PushOutcome::Conflict(ref s) if s.version == 2
        ));
        // A new card from a node.
        let fresh = Card {
            title: "From node".into(),
            key: c.key.clone(),
            ..Default::default()
        };
        let PushOutcome::Accepted(f) = core.push_card(fresh, "n1").await.unwrap() else {
            panic!("accepted");
        };
        assert_ne!(f.key, c.key, "a taken key is replaced");
        assert_eq!(f.status, "triage");
    }

    #[tokio::test]
    async fn memory_limits_and_clear() {
        let (core, _d) = core().await;
        let n = core
            .add_note("acme/api", "use just check", vec!["build".into()], OWNER)
            .await
            .unwrap();
        assert!(
            core.add_note("acme/api", &"x".repeat(NOTE_TEXT_MAX + 1), vec![], OWNER)
                .await
                .is_err()
        );
        let many: Vec<String> = (0..=NOTE_TAGS_MAX).map(|i| format!("t{i}")).collect();
        assert!(core.add_note("acme/api", "x", many, OWNER).await.is_err());
        assert!(core.add_note("", "x", vec![], OWNER).await.is_err());

        let e = core
            .edit_note(&n.id, Some("cargo test"), None)
            .await
            .unwrap();
        assert!(e.updated_at > n.updated_at);
        core.delete_note(&n.id).await.unwrap();
        assert!(core.delete_note(&n.id).await.is_err());
        assert!(core.edit_note(&n.id, Some("y"), None).await.is_err());

        core.add_note("acme/api", "a", vec![], OWNER).await.unwrap();
        core.add_note("acme/api", "b", vec![], OWNER).await.unwrap();
        assert_eq!(core.clear_memory_scope("acme/api").await.unwrap(), 2);
        assert!(
            core.notes(Some("acme/api"), "", false)
                .await
                .unwrap()
                .is_empty()
        );

        // Host replica: an older copy does not win.
        let stored = core
            .push_note(MemoryNote {
                id: "h1".into(),
                scope: "global".into(),
                text: "new".into(),
                created_at: 10,
                updated_at: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(stored.text, "new");
        let stored = core
            .push_note(MemoryNote {
                id: "h1".into(),
                scope: "global".into(),
                text: "old".into(),
                created_at: 10,
                updated_at: 15,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(stored.text, "new");
    }

    #[tokio::test]
    async fn a2a_blackout_and_control_events() {
        let (core, _d) = core().await;
        let m = A2aMessage {
            from: "n1:s1".into(),
            to: "n2:s2".into(),
            text: "hello".into(),
            ..Default::default()
        };
        let sent = core.send_a2a(m.clone()).await.unwrap();
        assert!(sent.delivered);
        let first = sent.message.unwrap();
        assert_eq!(first.thread_id, first.id);

        let mut rx = core.subscribe();
        let c = core.set_a2a_blackout(true, "incident").await.unwrap();
        assert!(c.a2a_blackout);
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.kind, kinds::FACTORY_CONTROL);
        assert!(ev.data_json.contains("\"a2aBlackout\":true"));
        assert!(core.set_a2a_blackout(true, "").await.is_err());

        let refused = core.send_a2a(m.clone()).await.unwrap();
        assert!(!refused.delivered);
        assert_eq!(refused.refused, "a2a blackout");
        let (rows, cur) = core.a2a_inbox(&["n2:s2".into()], 0).await.unwrap();
        assert!(rows.is_empty());
        assert_eq!(cur, 0);

        core.set_a2a_blackout(false, "").await.unwrap();
        let (rows, cur) = core.a2a_inbox(&["n2:s2".into()], 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(cur, first.seq);
        let (rows, _) = core.a2a_inbox(&["n3:s3".into()], 0).await.unwrap();
        assert!(rows.is_empty());

        let s = core.set_factory_stopped(true, "freeze").await.unwrap();
        assert!(s.stopped);
        assert_eq!(control_reason(&s), "freeze");
        assert!(core.set_factory_stopped(true, "").await.is_err());
        assert!(core.set_factory_stopped(false, "").await.is_ok());
        assert!(
            core.set_factory_stopped(true, &"r".repeat(REASON_MAX + 1))
                .await
                .is_err()
        );
    }

    #[test]
    fn a2a_addressing() {
        let m = |from: &str, to: &str| A2aMessage {
            from: from.into(),
            to: to.into(),
            ..Default::default()
        };
        let me = vec!["n1:s1".to_string()];
        assert!(a2a_for(&m("n2:s2", "n1:s1"), &me));
        assert!(a2a_for(&m("n2:s2", "n1"), &me));
        assert!(a2a_for(&m("n2:s2", "n1:*"), &me));
        assert!(a2a_for(&m("n2:s2", ""), &me), "broadcast");
        assert!(!a2a_for(&m("n1:s1", ""), &me), "own broadcast skipped");
        assert!(!a2a_for(&m("n2:s2", "n3:s3"), &me));
    }

    #[tokio::test]
    async fn link_unlink_and_three_way_sync() {
        let (core, _d) = core().await;
        let c = core.create_card(draft("Fix login"), OWNER).await.unwrap();
        let c = core
            .link_card_issue(
                &c.id,
                "acme/api",
                7,
                "https://github.com/acme/api/issues/7",
                OWNER,
            )
            .await
            .unwrap();
        assert_eq!(github_ref(&c), Some(("acme/api".into(), 7)));
        assert_eq!(c.repo, "acme/api");
        let i = core.card_issue(&c).await.unwrap().unwrap();
        assert_eq!(i.card_id.as_deref(), Some(c.id.as_str()));
        core.emit(
            crate::github::kinds::ISSUE_COMMENT,
            "acme/api#7",
            &json!({"author": "ana", "body": "on it", "internal": true}),
        )
        .await
        .unwrap();
        let cm = core.issue_comments("acme/api", 7, 10).await.unwrap();
        assert_eq!((cm.len(), cm[0].author.as_str()), (1, "ana"));

        // The issue was renamed on GitHub: a title conflict, base unknown.
        let mut gi = i.clone();
        gi.title = "Fix the login".into();
        gi.updated_at = 5;
        core.store().gh_upsert_issue(&gi).await.unwrap();
        let rows = core.sync_conflicts_3way().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].base.iter().all(Option::is_none));
        let c = core.resolve_sync_field(&c.id, "title", true).await.unwrap();
        assert_eq!(c.title, "#7 Fix the login");
        assert_eq!(sync_base(&c, "title").as_deref(), Some("#7 Fix the login"));
        assert!(
            core.sync_conflicts_3way().await.unwrap().iter().all(|r| r
                .conflict
                .fields
                .iter()
                .all(|f| f.field != "title")),
            "the title agrees now"
        );

        let c = core.unlink_card_issue(&c.id, OWNER).await.unwrap();
        assert!(github_ref(&c).is_none());
        assert!(core.card_issue(&c).await.unwrap().is_none());
        let row = core.store().gh_issue("acme/api", 7).await.unwrap().unwrap();
        assert!(row.card_id.unwrap().starts_with(UNLINKED_PREFIX));
        let n = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["card".into()],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                e.subject["action"] == "card.github.link"
                    || e.subject["action"] == "card.github.unlink"
            })
            .count();
        assert_eq!(n, 2);
    }
}
