//! Board <-> GitHub sync for cards linked to mirrored issues (the board's
//! Sync tab, website `FactoryBoardConflicts.vue`, `FactoryIssueMirroring.vue`).
//!
//! Each card keeps the last synced value of every field (`keleustes.syncBase`
//! in its meta, in board form): title, open/closed state, body and labels.
//! A field is then a three-way compare, [`plan`]:
//!
//! - board and GitHub agree: nothing to do;
//! - only the board changed since the base: push the board value to the issue;
//! - only GitHub changed: take the issue's value onto the card;
//! - both changed (or no base yet) and they differ: a conflict, unless the
//!   project's merge rule for the field says which side wins. A conflict is
//!   never overwritten; the owner resolves it ([`Core::resolve_sync_conflict`]),
//!   which moves the base so the next pass pushes or takes.
//!
//! The pushes themselves (forge writes, with retry) are in `kls-github`
//! (`sync::sync_cards`); this module is pure planning plus the mirror config.

use crate::Core;
use anyhow::{Result, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::Card;
use kls_store::github::GhIssue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The synced fields, in display order.
pub const FIELDS: [&str; 4] = ["title", "state", "body", "labels"];

/// Card labels that stay on the board (never pushed to an issue).
pub const BOARD_ONLY_PREFIX: &str = "project:";

/// An open sync conflict older than its project's `escalateHours` was escalated.
pub const SYNC_ESCALATED: &str = "github.sync.escalated";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldConflict {
    pub field: String,
    pub board: String,
    pub github: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncConflict {
    pub card_id: String,
    pub key: String,
    pub repo: String,
    pub number: i64,
    pub url: String,
    pub fields: Vec<FieldConflict>,
}

/// The board title of a mirrored issue (`#<n> <title>`, kls-github `card_title`).
pub fn issue_title(i: &GhIssue) -> String {
    format!("#{} {}", i.number, i.title.trim())
        .chars()
        .take(super::TITLE_MAX)
        .collect()
}

/// The issue title a board title stands for (`#7 Fix` -> `Fix`).
pub fn title_to_issue(board: &str, number: i64) -> String {
    let p = format!("#{number} ");
    board
        .trim()
        .strip_prefix(&p)
        .unwrap_or(board.trim())
        .trim()
        .to_string()
}

/// A card body as created from an issue: the issue body, then its URL.
pub fn issue_body_on_card(i: &GhIssue) -> String {
    let mut body = i.body.trim().to_string();
    if !i.html_url.is_empty() {
        body.push_str(&format!("\n\n{}", i.html_url));
    }
    while body.len() > super::BODY_MAX {
        body.pop();
    }
    body.trim().to_string()
}

/// The body a card holds for an issue, without the trailing issue URL.
fn body_form(card_body: &str, url: &str) -> String {
    let b = card_body.trim();
    let b = if url.is_empty() {
        b
    } else {
        b.strip_suffix(url).unwrap_or(b)
    };
    b.trim().to_string()
}

fn label_form<'a>(labels: impl Iterator<Item = &'a String>) -> String {
    let mut v: Vec<String> = Vec::new();
    for l in labels {
        let l = l.trim();
        if !l.is_empty() && !l.starts_with(BOARD_ONLY_PREFIX) && !v.iter().any(|x| x == l) {
            v.push(l.to_string());
        }
    }
    v.sort();
    v.join("\n")
}

/// Field values on the board, in board form.
pub fn board_values(c: &Card, i: &GhIssue) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("title", c.title.trim().to_string()),
        (
            "state",
            if c.status == "done" { "closed" } else { "open" }.to_string(),
        ),
        ("body", body_form(&c.body, &i.html_url)),
        ("labels", label_form(c.labels.iter())),
    ])
}

/// Field values on GitHub, in board form.
pub fn github_values(i: &GhIssue) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("title", issue_title(i)),
        ("state", i.state.clone()),
        ("body", body_form(&issue_body_on_card(i), &i.html_url)),
        (
            "labels",
            label_form(i.labels.iter().take(super::LABELS_MAX)),
        ),
    ])
}

/// Whether a field can be written to the issue without losing data (a body
/// or label set the card holds cut short is never pushed).
pub fn pushable(i: &GhIssue, field: &str) -> bool {
    match field {
        "body" => i.body.len() + i.html_url.len() + 2 < super::BODY_MAX,
        "labels" => i.labels.len() <= super::LABELS_MAX,
        _ => true,
    }
}

fn meta_of(c: &Card) -> Value {
    let m: Value = serde_json::from_str(&c.meta_json).unwrap_or_else(|_| json!({}));
    if m.is_object() { m } else { json!({}) }
}

/// The last synced values (`keleustes.syncBase`; a legacy `syncKeep` value
/// counts as the base of its field).
pub fn base_of(c: &Card) -> BTreeMap<String, String> {
    let m = meta_of(c);
    let mut out = BTreeMap::new();
    for key in ["/keleustes/syncKeep", "/keleustes/syncBase"] {
        if let Some(o) = m.pointer(key).and_then(Value::as_object) {
            for (k, v) in o {
                if let Some(s) = v.as_str() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    out
}

fn keleustes_mut(m: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !m.is_object() {
        *m = json!({});
    }
    if !m.get("keleustes").is_some_and(Value::is_object) {
        m["keleustes"] = json!({});
    }
    m["keleustes"].as_object_mut().expect("object")
}

/// `meta_json` with the base replaced (and a legacy `syncKeep` dropped).
pub fn with_base(meta_json: &str, base: &BTreeMap<String, String>) -> String {
    let mut m: Value = serde_json::from_str(meta_json).unwrap_or_else(|_| json!({}));
    let k = keleustes_mut(&mut m);
    k.remove("syncKeep");
    k.insert("syncBase".into(), json!(base));
    m.to_string()
}

/// A failed push waiting for its retry (`keleustes.syncRetry`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    pub attempt: i64,
    pub retry_at: i64,
    pub error: String,
    /// The forge cannot update issues: no more attempts.
    #[serde(default)]
    pub unsupported: bool,
}

pub fn retry_of(c: &Card) -> Option<Retry> {
    meta_of(c)
        .pointer("/keleustes/syncRetry")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// `meta_json` with the retry state set or cleared.
pub fn with_retry(meta_json: &str, r: Option<&Retry>) -> String {
    let mut m: Value = serde_json::from_str(meta_json).unwrap_or_else(|_| json!({}));
    let k = keleustes_mut(&mut m);
    match r {
        Some(r) => k.insert("syncRetry".into(), json!(r)),
        None => k.remove("syncRetry"),
    };
    m.to_string()
}

/// When each open conflict was first seen, and whether it was escalated
/// (`keleustes.syncConflicts`: `{field: since_ms}`, `escalated`).
pub fn conflict_since(c: &Card) -> (BTreeMap<String, i64>, bool) {
    let m = meta_of(c);
    let since = m
        .pointer("/keleustes/syncConflicts/since")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
                .collect()
        })
        .unwrap_or_default();
    let esc = m
        .pointer("/keleustes/syncConflicts/escalated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (since, esc)
}

pub fn with_conflicts(meta_json: &str, since: &BTreeMap<String, i64>, escalated: bool) -> String {
    let mut m: Value = serde_json::from_str(meta_json).unwrap_or_else(|_| json!({}));
    let k = keleustes_mut(&mut m);
    if since.is_empty() {
        k.remove("syncConflicts");
    } else {
        k.insert(
            "syncConflicts".into(),
            json!({ "since": since, "escalated": escalated }),
        );
    }
    m.to_string()
}

/// Which side wins a field both sides changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rule {
    /// The owner picks (a conflict on the Sync tab).
    #[default]
    Ask,
    Board,
    Github,
}

impl Rule {
    pub fn parse(s: &str) -> Self {
        match s {
            "board" => Rule::Board,
            "github" => Rule::Github,
            _ => Rule::Ask,
        }
    }
}

/// What one pass does for a card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Field, board value (board form) to write to the issue.
    pub push: Vec<(String, String)>,
    /// Field, GitHub value (board form) to put on the card.
    pub take: Vec<(String, String)>,
    pub conflicts: Vec<FieldConflict>,
    /// The base once every push and take succeeded.
    pub base: BTreeMap<String, String>,
    /// The base differs from the stored one even without a push.
    pub base_changed: bool,
}

/// The three-way plan for a card and its issue (pure).
pub fn plan(c: &Card, i: &GhIssue, rules: &BTreeMap<String, String>) -> Plan {
    let board = board_values(c, i);
    let gh = github_values(i);
    let old = base_of(c);
    let mut p = Plan {
        base: old.clone(),
        ..Default::default()
    };
    for f in FIELDS {
        let (b, g) = (&board[f], &gh[f]);
        let base = old.get(f);
        if b == g {
            if base != Some(g) {
                p.base.insert(f.into(), g.clone());
            }
            continue;
        }
        let board_moved = base != Some(b);
        let gh_moved = base != Some(g);
        let side = match (board_moved, gh_moved, base.is_some()) {
            (true, false, true) => Rule::Board,
            (false, true, true) => Rule::Github,
            _ => Rule::parse(rules.get(f).map(String::as_str).unwrap_or("")),
        };
        match side {
            Rule::Board if pushable(i, f) => p.push.push((f.into(), b.clone())),
            Rule::Github => p.take.push((f.into(), g.clone())),
            _ => p.conflicts.push(FieldConflict {
                field: f.into(),
                board: b.clone(),
                github: g.clone(),
            }),
        }
    }
    p.base_changed = p.base != old;
    p
}

/// The field conflicts between a card and its issue (pure; no merge rules).
pub fn conflicts_of(c: &Card, i: &GhIssue) -> Vec<FieldConflict> {
    plan(c, i, &BTreeMap::new()).conflicts
}

/// The card fields for GitHub values (board form) taken onto a card.
pub fn take_patch(c: &Card, take: &[(String, String)]) -> super::CardPatch {
    let mut p = super::CardPatch::default();
    for (f, v) in take {
        match f.as_str() {
            "title" => p.title = Some(v.clone()),
            "state" => {
                if v == "closed" && c.status != "done" {
                    p.status = Some("done".into());
                } else if v == "open" && c.status == "done" {
                    p.status = Some("triage".into());
                }
            }
            "body" => {
                let url = c
                    .body
                    .lines()
                    .last()
                    .filter(|l| l.starts_with("http"))
                    .unwrap_or("");
                let mut b = v.clone();
                if !url.is_empty() {
                    b.push_str(&format!("\n\n{url}"));
                }
                while b.len() > super::BODY_MAX {
                    b.pop();
                }
                p.body = Some(b);
            }
            "labels" => {
                let mut l: Vec<String> = c
                    .labels
                    .iter()
                    .filter(|l| l.starts_with(BOARD_ONLY_PREFIX))
                    .cloned()
                    .collect();
                l.extend(v.lines().map(str::to_string));
                l.truncate(super::LABELS_MAX);
                p.labels = Some(l);
            }
            _ => {}
        }
    }
    p
}

// ---- issue mirroring per board project ------------------------------------

/// One board project mirrored to a repository's issues
/// (setting `github.issue_mirror`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorRow {
    /// The board project (cards carry the label `project:<project>`).
    pub project: String,
    pub repo: String,
    /// The list a new issue's card lands in: `triage` (default), `review`,
    /// or `off` (no new cards; linked cards still sync).
    #[serde(default = "default_inbound")]
    pub inbound: String,
    /// Push card edits to the issue (default on).
    #[serde(default = "yes")]
    pub outbound: bool,
    /// Field -> `ask` (default), `board` or `github`, for a field both changed.
    #[serde(default)]
    pub rules: BTreeMap<String, String>,
    /// Escalate a conflict open this long (0: never).
    #[serde(default)]
    pub escalate_hours: i64,
}

fn default_inbound() -> String {
    "triage".into()
}

fn yes() -> bool {
    true
}

impl MirrorRow {
    pub fn label(&self) -> String {
        format!("{BOARD_ONLY_PREFIX}{}", self.project.trim())
    }
}

impl Core {
    /// The project <-> repository mirror rows.
    pub async fn issue_mirrors(&self) -> Result<Vec<MirrorRow>> {
        let raw = self
            .store()
            .setting(crate::github::settings::ISSUE_MIRROR)
            .await?
            .unwrap_or_default();
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    /// The mirror row of a repository, if a project mirrors it.
    pub async fn issue_mirror_of(&self, repo: &str) -> Result<Option<MirrorRow>> {
        Ok(self
            .issue_mirrors()
            .await?
            .into_iter()
            .find(|r| r.repo == repo))
    }

    /// Saves the mirror rows (one per repository), audited.
    pub async fn save_issue_mirrors(&self, rows: &[MirrorRow], actor: &str) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for r in rows {
            if r.project.trim().is_empty() || r.repo.trim().is_empty() {
                bail!("a mirror row needs a project and a repository");
            }
            if !seen.insert(r.repo.clone()) {
                bail!("{} is mirrored by two projects", r.repo);
            }
            if !matches!(r.inbound.as_str(), "triage" | "review" | "off") {
                bail!("inbound must be triage, review or off");
            }
        }
        self.store()
            .set_setting(
                crate::github::settings::ISSUE_MIRROR,
                &serde_json::to_string(rows)?,
            )
            .await?;
        self.audit_record(LedgerDraft {
            at: 0,
            kind: "setting".into(),
            actor: actor.into(),
            actor_kind: "user".into(),
            msg: format!("issue mirror saved ({} rows)", rows.len()),
            subject: json!({ "action": "issue_mirror.save", "key": crate::github::settings::ISSUE_MIRROR }),
            payload: json!({ "rows": rows }),
            origin: "factory".into(),
            node_id: String::new(),
            session_id: String::new(),
            ref_key: None,
        })
        .await?;
        Ok(())
    }

    /// Every card whose fields conflict with its mirrored issue.
    pub async fn sync_conflicts(&self) -> Result<Vec<SyncConflict>> {
        let mut out = Vec::new();
        let rows = self.issue_mirrors().await?;
        for i in self.store().gh_issues(None, None).await? {
            let Some(id) = i.card_id.clone() else {
                continue;
            };
            let Some(c) = self.store().card(&id).await? else {
                continue;
            };
            if c.deleted {
                continue;
            }
            let rules = rows
                .iter()
                .find(|r| r.repo == i.repo)
                .map(|r| r.rules.clone())
                .unwrap_or_default();
            let fields = plan(&c, &i, &rules).conflicts;
            if !fields.is_empty() {
                out.push(SyncConflict {
                    card_id: c.id.clone(),
                    key: c.key.clone(),
                    repo: i.repo.clone(),
                    number: i.number,
                    url: i.html_url.clone(),
                    fields,
                });
            }
        }
        Ok(out)
    }

    /// Resolves one field: take GitHub's value onto the card, or keep the
    /// board's (the next sync pass pushes it to the issue).
    pub async fn resolve_sync_conflict(
        &self,
        card_id: &str,
        field: &str,
        take_github: bool,
    ) -> Result<Card> {
        let Some(c) = self.store().card(card_id).await? else {
            bail!("no such card");
        };
        let Some(c_conf) = self
            .sync_conflicts()
            .await?
            .into_iter()
            .find(|x| x.card_id == c.id)
        else {
            bail!("{} has no conflict", c.key);
        };
        let Some(f) = c_conf.fields.iter().find(|f| f.field == field) else {
            bail!("{} has no {field} conflict", c.key);
        };
        if !FIELDS.contains(&field) {
            bail!("unknown field {field}");
        }
        // Either way the base becomes GitHub's value: taking makes both sides
        // equal; keeping leaves only the board moved, so it is pushed.
        let mut base = base_of(&c);
        base.insert(field.into(), f.github.clone());
        let mut p = if take_github {
            take_patch(&c, &[(field.to_string(), f.github.clone())])
        } else {
            super::CardPatch::default()
        };
        let (mut since, esc) = conflict_since(&c);
        since.remove(field);
        p.meta_json = Some(with_conflicts(
            &with_base(&c.meta_json, &base),
            &since,
            esc && !since.is_empty(),
        ));
        let out = self.update_card(&c.id, c.version, p, super::OWNER).await?;
        self.audit_record(LedgerDraft {
            at: 0,
            kind: "card".into(),
            actor: super::OWNER.into(),
            actor_kind: "user".into(),
            msg: format!("sync.resolve {} {field} {}", c.key, if take_github { "github" } else { "board" }),
            subject: json!({ "action": "sync.resolve", "cardId": c.id, "field": field }),
            payload: json!({ "board": f.board, "github": f.github, "took": if take_github { "github" } else { "board" } }),
            origin: "factory".into(),
            node_id: String::new(),
            session_id: String::new(),
            ref_key: None,
        })
        .await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_state_conflicts_and_keep() {
        let i = GhIssue {
            repo: "a/b".into(),
            number: 7,
            title: "Fix login".into(),
            state: "closed".into(),
            ..Default::default()
        };
        let c = Card {
            title: "#7 Fix the login".into(),
            status: "backlog".into(),
            ..Default::default()
        };
        let f = conflicts_of(&c, &i);
        assert_eq!(
            f.iter().map(|x| x.field.as_str()).collect::<Vec<_>>(),
            ["title", "state"]
        );
        let kept = Card {
            meta_json: json!({"keleustes":{"syncKeep":{"title":"#7 Fix login"}}}).to_string(),
            ..c.clone()
        };
        assert_eq!(
            conflicts_of(&kept, &i).len(),
            1,
            "a kept field stops showing"
        );
        let same = Card {
            title: "#7 Fix login".into(),
            status: "done".into(),
            ..Default::default()
        };
        assert!(conflicts_of(&same, &i).is_empty());
    }

    fn based(c: Card, i: &GhIssue) -> Card {
        let base: BTreeMap<String, String> = github_values(i)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Card {
            meta_json: with_base(&c.meta_json, &base),
            ..c
        }
    }

    #[test]
    fn three_way_push_take_and_conflict() {
        let i = GhIssue {
            repo: "a/b".into(),
            number: 7,
            title: "Fix login".into(),
            state: "open".into(),
            body: "It breaks.".into(),
            html_url: "https://x/a/b/issues/7".into(),
            labels: vec!["bug".into()],
            ..Default::default()
        };
        let synced = based(
            Card {
                title: "#7 Fix login".into(),
                status: "triage".into(),
                body: issue_body_on_card(&i),
                labels: vec!["bug".into(), "project:web".into()],
                ..Default::default()
            },
            &i,
        );
        let p = plan(&synced, &i, &BTreeMap::new());
        assert!(p.push.is_empty() && p.take.is_empty() && p.conflicts.is_empty());
        assert!(!p.base_changed);

        // The board moved the card to done and retitled it: both pushed.
        let edited = Card {
            title: "#7 Fix the login".into(),
            status: "done".into(),
            ..synced.clone()
        };
        let p = plan(&edited, &i, &BTreeMap::new());
        assert_eq!(
            p.push,
            vec![
                ("title".to_string(), "#7 Fix the login".to_string()),
                ("state".to_string(), "closed".to_string())
            ]
        );
        assert_eq!(title_to_issue("#7 Fix the login", 7), "Fix the login");

        // GitHub changed the title too: a conflict, not an overwrite.
        let moved = GhIssue {
            title: "Login fails".into(),
            ..i.clone()
        };
        let p = plan(&edited, &moved, &BTreeMap::new());
        assert_eq!(p.conflicts.len(), 1);
        assert_eq!(p.conflicts[0].field, "title");
        assert_eq!(p.push.len(), 1, "state still pushes");
        // A merge rule decides it.
        let rules = BTreeMap::from([("title".to_string(), "github".to_string())]);
        let p = plan(&edited, &moved, &rules);
        assert!(p.conflicts.is_empty());
        assert_eq!(p.take[0].0, "title");

        // Only GitHub moved: taken.
        let p = plan(&synced, &moved, &BTreeMap::new());
        assert_eq!(
            p.take,
            vec![("title".to_string(), "#7 Login fails".to_string())]
        );
        let patch = take_patch(&synced, &p.take);
        assert_eq!(patch.title.as_deref(), Some("#7 Login fails"));
    }
}
