//! The external kanban binding (Settings "Kanban binding"; web Software
//! Factory: the kanban GitHub binding of `docs/belai-github-sync.md`, one
//! reconcile against a snapshot for every change).
//!
//! The factory board is mirrored to one outside board, `kanban.provider` +
//! `kanban.board`, both ways or one way (`kanban.sync`), with columns mapped
//! by `kanban.lists` (`factory list=board column`).
//!
//! - **Providers.** GitHub Projects and GitLab issue boards go through the
//!   forge CLI plane (`kls-forge` registers a [`Connector`]; Keleustes holds
//!   no forge token). Linear and Jira Cloud are called over HTTPS
//!   ([`linear`], [`jira`]) with a token sealed in the vault ([`TOKEN_BLOB`]),
//!   never written to an event, the ledger or a log line.
//! - **Merge.** Each link keeps the last value both sides agreed on per field
//!   (title, body, column). A field only the board changed is pushed, one only
//!   the outside board changed is taken, one both changed is a conflict that
//!   is never overwritten ([`plan`]); the owner picks a side
//!   ([`resolve_conflict`]), which moves the base so the next pass writes it.
//!   With sync `push` the factory's value wins; with `pull` a local edit is
//!   kept on the card and never silently replaced.
//! - **Untrusted.** Outside text is cleaned (control, bidi and zero-width
//!   characters dropped, capped) before it is compared or stored. A new
//!   outside item lands in triage, like an inbound issue, labelled
//!   `kanban:<provider>`, with its origin (provider, board, item id, URL and
//!   the SHA-256 of what was taken) in the card's meta. Items already done
//!   outside are not imported.
//! - **Rules.** Every pass checks the kill switch first (`job`; paused or
//!   stopped refuses, a maintenance window holds) and every create, push,
//!   take, conflict, refusal and failure is a ledger entry with its origin.

use crate::Core;
use crate::audit::config::Effective;
use crate::board::{CardDraft, CardPatch, LABELS_MAX, TITLE_MAX};
use anyhow::{Result, anyhow, bail};
use kls_store::audit::LedgerDraft;
use kls_store::board::Card;
use kls_store::board::kanban::{KanbanLink, KanbanRun};
use kls_vault::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub mod http;
pub mod jira;
pub mod linear;
#[cfg(test)]
mod tests;

/// Providers a binding may name (`off` binds nothing).
pub const PROVIDERS: [&str; 4] = ["github_projects", "gitlab_boards", "linear", "jira"];
/// Providers that need a token in the vault (the forges use their CLI sign-in).
pub const TOKEN_PROVIDERS: [&str; 2] = ["linear", "jira"];
/// The vault blob holding the Linear API key or the Jira `email:token`.
pub const TOKEN_BLOB: &str = "kanban.token";
/// How often [`start`] runs a pass.
pub const TICK_EVERY: Duration = Duration::from_secs(5 * 60);
/// New outside items created per pass (outside boards throttle creation).
pub const CREATE_MAX: usize = 30;
/// The synced fields.
pub const FIELDS: [&str; 3] = ["title", "body", "column"];

pub mod kinds {
    /// A pass finished: `{provider, board, origin, report}`.
    pub const KANBAN_SYNCED: &str = "kanban.synced";
    /// A field conflict opened: `{provider, board, card, fields}`.
    pub const KANBAN_CONFLICT: &str = "kanban.conflict";
    /// The token was sealed or cleared: `{provider, set, by}` (never the token).
    pub const KANBAN_TOKEN: &str = "kanban.token";
}

// ---- binding ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Factory to board.
    #[default]
    Push,
    /// Board to factory.
    Pull,
    Both,
}

impl Direction {
    pub fn parse(s: &str) -> Self {
        match s {
            "pull" => Direction::Pull,
            "both" => Direction::Both,
            _ => Direction::Push,
        }
    }
    pub fn pushes(self) -> bool {
        self != Direction::Pull
    }
    pub fn pulls(self) -> bool {
        self != Direction::Push
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Push => "push",
            Direction::Pull => "pull",
            Direction::Both => "both",
        }
    }
}

/// The binding as Settings hold it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub provider: String,
    pub board: String,
    pub sync: Direction,
    /// `(factory list, board column)` in order.
    pub lists: Vec<(String, String)>,
}

/// `["triage=Triage", "doing=In Progress"]` as pairs (blank or malformed
/// entries skipped).
pub fn parse_lists(v: &[String]) -> Vec<(String, String)> {
    v.iter()
        .filter_map(|e| {
            let (a, b) = e.split_once('=')?;
            let (a, b) = (a.trim(), b.trim());
            (crate::board::STATUSES.contains(&a) && !b.is_empty())
                .then(|| (a.to_string(), b.to_string()))
        })
        .collect()
}

impl Binding {
    /// The bound board, or `None` when nothing is bound.
    pub fn from_settings(e: &Effective) -> Option<Self> {
        let provider = e.kanban_provider.trim();
        let board = e.kanban_board.trim();
        if !PROVIDERS.contains(&provider) || board.is_empty() {
            return None;
        }
        Some(Self {
            provider: provider.into(),
            board: board.into(),
            sync: Direction::parse(e.kanban_sync.trim()),
            lists: parse_lists(&e.kanban_lists),
        })
    }

    pub async fn load(core: &Core) -> Result<Option<Self>> {
        Ok(Self::from_settings(&Effective::load(core).await?))
    }

    /// The outside column a factory list maps to.
    pub fn column_of(&self, status: &str) -> Option<&str> {
        self.lists
            .iter()
            .find(|(s, _)| s == status)
            .map(|(_, c)| c.as_str())
    }

    /// The factory list an outside column maps to (first match, any case).
    pub fn status_of(&self, column: &str) -> Option<&str> {
        let c = column.trim();
        self.lists
            .iter()
            .find(|(_, b)| b.eq_ignore_ascii_case(c))
            .map(|(s, _)| s.as_str())
    }

    /// Every mapped outside column, in order, without repeats.
    pub fn columns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, c) in &self.lists {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                out.push(c.clone());
            }
        }
        out
    }

    /// The actor on card activity and ledger entries.
    pub fn actor(&self) -> String {
        format!("kanban:{}", self.provider)
    }

    fn target(&self) -> String {
        format!("kanban:{}:{}", self.provider, self.board)
    }
}

// ---- the outside board -----------------------------------------------------------

/// One item on the outside board, cleaned ([`clean_title`], [`clean_body`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub url: String,
    pub title: String,
    pub body: String,
    /// The column it sits in ("" when none).
    pub column: String,
    /// Title and body belong to something else (a repository issue on a
    /// GitHub project): only the column is synced.
    #[serde(default)]
    pub locked: bool,
    /// Whether the item counts as on the board for import (a GitLab issue
    /// with no list label is not).
    #[serde(default = "yes")]
    pub on_board: bool,
    /// Provider handle the adapter needs to write it (a draft issue id).
    #[serde(default)]
    pub handle: String,
}

fn yes() -> bool {
    true
}

/// The fields to write to an item (`None`: leave it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub title: Option<String>,
    pub body: Option<String>,
    pub column: Option<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.body.is_none() && self.column.is_none()
    }
    fn fields(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.title.is_some() {
            v.push("title");
        }
        if self.body.is_some() {
            v.push("body");
        }
        if self.column.is_some() {
            v.push("column");
        }
        v
    }
}

/// One bound outside board.
#[async_trait::async_trait]
pub trait Remote: Send + Sync {
    /// Every item on it (cleaned).
    async fn list(&self) -> Result<Vec<Item>>;
    /// Creates an item, in `column` when given.
    async fn create(&self, title: &str, body: &str, column: Option<&str>) -> Result<Item>;
    /// Writes the changed fields of an item.
    async fn update(&self, item: &Item, change: &Change) -> Result<()>;
}

/// Builds the [`Remote`] for a binding a crate outside `kls-core` serves
/// (the forge providers, in `kls-forge`). `Ok(None)`: not this one's.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn remote(&self, core: &Core, b: &Binding) -> Result<Option<Arc<dyn Remote>>>;
}

type Registry = Mutex<Vec<(PathBuf, Arc<dyn Connector>)>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Adds a connector for this core's data directory (consulted before the
/// built-in Linear and Jira clients, newest first).
pub fn register_connector(core: &Core, c: Arc<dyn Connector>) {
    registry()
        .lock()
        .unwrap()
        .push((core.data_dir().to_path_buf(), c));
}

fn connectors(core: &Core) -> Vec<Arc<dyn Connector>> {
    let r = registry().lock().unwrap();
    r.iter()
        .rev()
        .filter(|(d, _)| d == core.data_dir())
        .map(|(_, c)| c.clone())
        .collect()
}

/// The remote for a binding: a registered connector's, else the built-in
/// HTTP client with the sealed token.
pub async fn remote_for(core: &Core, b: &Binding) -> Result<Arc<dyn Remote>> {
    for c in connectors(core) {
        if let Some(r) = c.remote(core, b).await? {
            return Ok(r);
        }
    }
    let web: Arc<dyn http::Http> = Arc::new(http::Web::new()?);
    match b.provider.as_str() {
        "linear" => {
            let t = token(core)
                .await?
                .ok_or_else(|| anyhow!("no Linear key sealed"))?;
            Ok(Arc::new(linear::Linear::new(web, t, b)?))
        }
        "jira" => {
            let t = token(core)
                .await?
                .ok_or_else(|| anyhow!("no Jira token sealed"))?;
            Ok(Arc::new(jira::Jira::new(web, t, b)?))
        }
        p => bail!("{p} needs the forge plane (no forge connection is serving it)"),
    }
}

// ---- token -------------------------------------------------------------------------

/// Seals the provider token in the vault ("" clears it). The event names
/// the provider only.
pub async fn set_token(core: &Core, provider: &str, value: SecretString) -> Result<bool> {
    let v = crate::vault::vault(core).await?;
    let raw = value.expose().trim().to_string();
    if !raw.is_empty() {
        if provider == "jira" && !raw.contains(':') {
            bail!("write the Jira token as email:api-token");
        }
        if raw.len() > 4096 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("not a token");
        }
    }
    v.put_blob(TOKEN_BLOB, raw.as_bytes()).await?;
    let set = !raw.is_empty();
    core.emit(
        kinds::KANBAN_TOKEN,
        "kanban",
        &json!({ "provider": provider, "set": set, "by": "owner" }),
    )
    .await?;
    record(
        core,
        "owner",
        "user",
        "kanban.token",
        "kanban",
        if set { "sealed" } else { "cleared" },
        json!({ "provider": provider }),
    )
    .await;
    Ok(set)
}

async fn token(core: &Core) -> Result<Option<SecretString>> {
    let v = crate::vault::vault(core).await?;
    let Some(b) = v.get_blob(TOKEN_BLOB).await? else {
        return Ok(None);
    };
    let s = String::from_utf8(b.to_vec()).unwrap_or_default();
    Ok((!s.trim().is_empty()).then(|| SecretString::new(s.trim())))
}

/// Whether a token is sealed (Settings shows only this).
pub async fn token_set(core: &Core) -> bool {
    token(core).await.ok().flatten().is_some()
}

// ---- untrusted text --------------------------------------------------------------

fn dropped(c: char) -> bool {
    matches!(c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        || (c.is_control() && c != '\n' && c != '\t')
}

/// Outside text for a card body: no control, bidi or zero-width characters
/// (newlines and tabs kept), `\r\n` as `\n`, trimmed, at most `BODY_MAX`.
pub fn clean_body(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let mut out: String = s.chars().filter(|c| !dropped(*c)).collect();
    let max = crate::board::BODY_MAX;
    if out.len() > max {
        let mut cut = max;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out.trim().to_string()
}

/// Outside text for a card title: one line, at most `TITLE_MAX` characters.
pub fn clean_title(s: &str) -> String {
    let one: String = s
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !dropped(*c))
        .collect();
    one.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TITLE_MAX)
        .collect()
}

/// Cleans every text field of an item (adapters call it on what they read).
pub fn clean_item(mut i: Item) -> Item {
    i.title = clean_title(&i.title);
    i.body = clean_body(&i.body);
    i.column = clean_title(&i.column);
    i.url = clean_title(&i.url);
    i
}

fn sha256(i: &Item) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(i.title.as_bytes());
    h.update([0]);
    h.update(i.body.as_bytes());
    h.update([0]);
    h.update(i.column.as_bytes());
    hex::encode(h.finalize())
}

// ---- merge -----------------------------------------------------------------------

/// One held field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Held {
    pub base: String,
    pub board: String,
    pub remote: String,
}

/// What one link's pass does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub push: Change,
    pub take: Change,
    pub held: BTreeMap<String, Held>,
    /// The base after the writes succeed.
    pub base: BTreeMap<String, String>,
}

/// The board side of each field. The column is `None` when the card's list
/// maps to no outside column.
pub fn board_values(b: &Binding, c: &Card) -> BTreeMap<&'static str, Option<String>> {
    BTreeMap::from([
        ("title", Some(c.title.trim().to_string())),
        ("body", Some(c.body.trim().to_string())),
        ("column", b.column_of(&c.status).map(String::from)),
    ])
}

fn same(field: &str, a: &str, b: &str) -> bool {
    if field == "column" {
        a.trim().eq_ignore_ascii_case(b.trim())
    } else {
        a.trim() == b.trim()
    }
}

/// The three-way compare of one card and its item against the base.
pub fn plan(b: &Binding, card: &Card, item: &Item, base: &BTreeMap<String, String>) -> Plan {
    let mut p = Plan {
        base: base.clone(),
        ..Default::default()
    };
    let local = board_values(b, card);
    for f in FIELDS {
        if item.locked && f != "column" {
            continue;
        }
        let r = match f {
            "title" => item.title.as_str(),
            "body" => item.body.as_str(),
            _ => item.column.as_str(),
        };
        let bv = base.get(f).cloned().unwrap_or_default();
        // An unmapped list counts as unchanged on the board.
        let l = local[f].clone().unwrap_or_else(|| bv.clone());
        if same(f, &l, r) {
            p.base.insert(f.into(), r.to_string());
            continue;
        }
        let lc = !same(f, &l, &bv) && local[f].is_some();
        let rc = !same(f, r, &bv);
        let push = |p: &mut Plan| match f {
            "title" => p.push.title = Some(l.clone()),
            "body" => p.push.body = Some(l.clone()),
            _ => p.push.column = Some(l.clone()),
        };
        match (lc, rc) {
            (true, false) if b.sync.pushes() => push(&mut p),
            // Pull only: the local edit stays on the card.
            (true, false) => {}
            (false, true) if b.sync.pulls() => {
                // A column that maps to no factory list is left alone.
                if f == "column" && b.status_of(r).is_none() {
                    continue;
                }
                match f {
                    "title" => p.take.title = Some(r.to_string()),
                    "body" => p.take.body = Some(r.to_string()),
                    _ => p.take.column = Some(r.to_string()),
                }
            }
            // Push only: the factory's value wins.
            (false, true) => push(&mut p),
            (true, true) if b.sync == Direction::Push => push(&mut p),
            _ => {
                p.held.insert(
                    f.into(),
                    Held {
                        base: bv,
                        board: l.clone(),
                        remote: r.to_string(),
                    },
                );
            }
        }
    }
    p
}

fn base_of(l: &KanbanLink) -> BTreeMap<String, String> {
    serde_json::from_str(&l.base_json).unwrap_or_default()
}

fn held_of(l: &KanbanLink) -> BTreeMap<String, Held> {
    if l.conflict_json.trim().is_empty() {
        return BTreeMap::new();
    }
    serde_json::from_str(&l.conflict_json).unwrap_or_default()
}

// ---- audit ---------------------------------------------------------------------

async fn record(
    core: &Core,
    actor: &str,
    actor_kind: &str,
    action: &str,
    target: &str,
    outcome: &str,
    data: Value,
) {
    let d = LedgerDraft {
        at: 0,
        kind: "kanban".into(),
        actor: actor.into(),
        actor_kind: actor_kind.into(),
        msg: format!("{action} {target} {outcome}"),
        subject: json!({ "action": action, "target": target }),
        payload: json!({ "outcome": outcome, "data": data }),
        origin: "factory".into(),
        node_id: String::new(),
        session_id: String::new(),
        ref_key: None,
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(action, "kanban audit: {e:#}");
    }
}

async fn audit(core: &Core, b: &Binding, action: &str, target: &str, outcome: &str, data: Value) {
    let mut data = data;
    data["provider"] = json!(b.provider);
    data["board"] = json!(b.board);
    record(core, &b.actor(), "system", action, target, outcome, data).await;
}

// ---- the pass --------------------------------------------------------------------

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Cards made from new outside items.
    pub imported: usize,
    /// Outside items made from cards.
    pub created: usize,
    /// Items that took fields from their card.
    pub pushed: usize,
    /// Cards that took fields from their item.
    pub taken: usize,
    /// Links with an open conflict.
    pub conflicts: usize,
    /// Links whose card or item went away this pass.
    pub gone: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

fn short(e: &anyhow::Error) -> String {
    format!("{e:#}").chars().take(200).collect()
}

/// One pass of `b` against `remote`. `origin` is what asked (`tick`,
/// `owner`). The kill switch is checked first; a refusal is audited and
/// returned as the error.
pub async fn sync_with(
    core: &Core,
    b: &Binding,
    remote: &dyn Remote,
    origin: &str,
) -> Result<Report> {
    let target = b.target();
    if let Err(e) = crate::control::check(core, "job", &target, &format!("kanban:{origin}")).await {
        audit(
            core,
            b,
            "kanban.sync",
            &target,
            "refused",
            json!({ "origin": origin, "why": short(&e) }),
        )
        .await;
        let _ = core
            .store()
            .kanban_run_put(&b.provider, &b.board, "refused", "{}", false)
            .await;
        return Err(e);
    }
    let items = match remote.list().await {
        Ok(v) => v,
        Err(e) => {
            audit(
                core,
                b,
                "kanban.list",
                &target,
                "failed",
                json!({ "origin": origin, "why": short(&e) }),
            )
            .await;
            let rep = json!({ "errors": [short(&e)] }).to_string();
            let _ = core
                .store()
                .kanban_run_put(&b.provider, &b.board, "failed", &rep, false)
                .await;
            return Err(e);
        }
    };
    let mut rep = Report::default();
    let items: HashMap<String, Item> = items
        .into_iter()
        .map(clean_item)
        .map(|i| (i.id.clone(), i))
        .collect();
    let links = core.store().kanban_links(&b.provider, &b.board).await?;
    let cards: HashMap<String, Card> = core
        .store()
        .cards(true)
        .await?
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();
    let mut seen_remote: HashSet<String> = HashSet::new();
    let mut linked_cards: HashSet<String> = HashSet::new();
    for l in &links {
        seen_remote.insert(l.remote_id.clone());
        linked_cards.insert(l.card_id.clone());
        if l.state == "gone" {
            continue;
        }
        let card = cards.get(&l.card_id).filter(|c| !c.deleted);
        let item = items.get(&l.remote_id);
        let (Some(card), Some(item)) = (card, item) else {
            // One side went away: the other is left as it is.
            let why = if card.is_none() {
                "card_gone"
            } else {
                "item_gone"
            };
            let mut g = l.clone();
            g.state = "gone".into();
            core.store().kanban_link_put(&g).await?;
            rep.gone += 1;
            audit(
                core,
                b,
                "kanban.unlink",
                &l.remote_id,
                why,
                json!({ "card": l.card_id, "url": l.remote_url, "origin": origin }),
            )
            .await;
            continue;
        };
        if let Err(e) = reconcile(core, b, remote, l, card, item, origin, &mut rep).await {
            rep.failed += 1;
            rep.errors.push(short(&e));
        }
    }
    // New outside items become triage cards.
    if b.sync.pulls() {
        let mut fresh: Vec<&Item> = items
            .values()
            .filter(|i| i.on_board && !seen_remote.contains(&i.id))
            .filter(|i| b.status_of(&i.column) != Some("done"))
            .collect();
        fresh.sort_by(|a, b| a.id.cmp(&b.id));
        for i in fresh {
            if let Err(e) = import(core, b, i, origin).await {
                rep.failed += 1;
                rep.errors.push(short(&e));
            } else {
                rep.imported += 1;
            }
        }
    }
    // Cards not on the outside board yet get an item (not ones done already).
    if b.sync.pushes() {
        let mut todo: Vec<&Card> = cards
            .values()
            .filter(|c| !c.deleted && c.status != "done" && !linked_cards.contains(&c.id))
            .collect();
        todo.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        for c in todo.into_iter().take(CREATE_MAX) {
            match export(core, b, remote, c, origin).await {
                Ok(()) => rep.created += 1,
                Err(e) => {
                    rep.failed += 1;
                    rep.errors.push(short(&e));
                }
            }
        }
    }
    rep.errors.truncate(20);
    let outcome = if rep.failed == 0 { "ok" } else { "partial" };
    let rj = serde_json::to_string(&rep)?;
    core.store()
        .kanban_run_put(&b.provider, &b.board, outcome, &rj, rep.failed == 0)
        .await?;
    core.emit(
        kinds::KANBAN_SYNCED,
        &target,
        &json!({ "provider": b.provider, "board": b.board, "origin": origin, "report": rep }),
    )
    .await?;
    audit(
        core,
        b,
        "kanban.sync",
        &target,
        outcome,
        json!({ "origin": origin, "report": rep }),
    )
    .await;
    Ok(rep)
}

#[allow(clippy::too_many_arguments)]
async fn reconcile(
    core: &Core,
    b: &Binding,
    remote: &dyn Remote,
    l: &KanbanLink,
    card: &Card,
    item: &Item,
    origin: &str,
    rep: &mut Report,
) -> Result<()> {
    let base = base_of(l);
    let p = plan(b, card, item, &base);
    let mut link = l.clone();
    // Agreed fields move the base at once; written ones once the write lands.
    let mut new_base = p.base.clone();
    if !p.push.is_empty() {
        match remote.update(item, &p.push).await {
            Ok(()) => {
                for (f, v) in [
                    ("title", &p.push.title),
                    ("body", &p.push.body),
                    ("column", &p.push.column),
                ] {
                    if let Some(v) = v {
                        new_base.insert(f.into(), v.clone());
                    }
                }
                rep.pushed += 1;
                audit(core, b, "kanban.push", &item.id, "ok", json!({
                    "card": card.key, "cardId": card.id, "url": item.url, "fields": p.push.fields(), "origin": origin,
                })).await;
            }
            Err(e) => {
                audit(core, b, "kanban.push", &item.id, "failed", json!({
                    "card": card.key, "url": item.url, "fields": p.push.fields(), "why": short(&e), "origin": origin,
                })).await;
                return Err(e);
            }
        }
    }
    if !p.take.is_empty() {
        let status = p
            .take
            .column
            .as_deref()
            .and_then(|c| b.status_of(c))
            .map(String::from);
        let patch = CardPatch {
            title: p.take.title.clone().filter(|t| !t.is_empty()),
            body: p.take.body.clone(),
            status: status.clone().filter(|s| *s != card.status),
            meta_json: Some(with_origin(&card.meta_json, b, item)),
            ..Default::default()
        };
        // A card edited meanwhile keeps its edit: the next pass compares again.
        match core
            .update_card(&card.id, card.version, patch, &b.actor())
            .await
        {
            Ok(_) => {
                for (f, v) in [
                    ("title", &p.take.title),
                    ("body", &p.take.body),
                    ("column", &p.take.column),
                ] {
                    if let Some(v) = v {
                        new_base.insert(f.into(), v.clone());
                    }
                }
                link.source_sha256 = sha256(item);
                rep.taken += 1;
                audit(core, b, "kanban.take", &item.id, "ok", json!({
                    "card": card.key, "cardId": card.id, "url": item.url, "sha256": link.source_sha256,
                    "fields": p.take.fields(), "status": status, "origin": origin,
                })).await;
            }
            Err(e) => {
                audit(core, b, "kanban.take", &item.id, "failed", json!({
                    "card": card.key, "url": item.url, "fields": p.take.fields(), "why": short(&e), "origin": origin,
                })).await;
                link.base_json = serde_json::to_string(&new_base)?;
                core.store().kanban_link_put(&link).await?;
                return Err(e);
            }
        }
    }
    let was = held_of(l);
    if !p.held.is_empty() {
        rep.conflicts += 1;
        let opened: Vec<&String> = p.held.keys().filter(|k| !was.contains_key(*k)).collect();
        if !opened.is_empty() {
            core.emit(
                kinds::KANBAN_CONFLICT,
                &card.id,
                &json!({ "provider": b.provider, "board": b.board, "card": card.key, "fields": opened, "url": item.url }),
            )
            .await?;
            audit(core, b, "kanban.conflict", &item.id, "held", json!({
                "card": card.key, "cardId": card.id, "url": item.url, "fields": opened, "origin": origin,
            })).await;
        }
    }
    link.conflict_json = if p.held.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&p.held)?
    };
    link.base_json = serde_json::to_string(&new_base)?;
    link.remote_url = item.url.clone();
    if link != *l {
        core.store().kanban_link_put(&link).await?;
    }
    Ok(())
}

/// The card's meta with the outside origin recorded (`kanban` key).
fn with_origin(meta: &str, b: &Binding, i: &Item) -> String {
    let mut m: Value = serde_json::from_str(meta).unwrap_or_else(|_| json!({}));
    if !m.is_object() {
        m = json!({});
    }
    m["kanban"] = json!({
        "provider": b.provider, "board": b.board, "id": i.id, "url": i.url,
        "sha256": sha256(i), "trust": "external", "at": kls_store::now_ms(),
    });
    m.to_string()
}

async fn import(core: &Core, b: &Binding, i: &Item, origin: &str) -> Result<()> {
    let title = if i.title.is_empty() {
        format!("{} item {}", b.provider, i.id)
            .chars()
            .take(TITLE_MAX)
            .collect()
    } else {
        i.title.clone()
    };
    let mut labels = vec![format!("kanban:{}", b.provider)];
    labels.truncate(LABELS_MAX);
    let card = core
        .create_card(
            CardDraft {
                title: title.clone(),
                body: i.body.clone(),
                status: "triage".into(),
                labels,
                meta_json: with_origin("{}", b, i),
                ..Default::default()
            },
            &b.actor(),
        )
        .await?;
    // The base is the item as read: the card's triage list then reads as a
    // board-side move, pushed back when the binding pushes.
    let base = BTreeMap::from([
        ("title".to_string(), title),
        ("body".to_string(), i.body.clone()),
        ("column".to_string(), i.column.clone()),
    ]);
    let sha = sha256(i);
    core.store()
        .kanban_link_put(&KanbanLink {
            provider: b.provider.clone(),
            board: b.board.clone(),
            card_id: card.id.clone(),
            remote_id: i.id.clone(),
            remote_url: i.url.clone(),
            base_json: serde_json::to_string(&base)?,
            source_sha256: sha.clone(),
            ..Default::default()
        })
        .await?;
    audit(core, b, "kanban.import", &i.id, "ok", json!({
        "card": card.key, "cardId": card.id, "url": i.url, "sha256": sha, "column": i.column, "origin": origin,
    }))
    .await;
    Ok(())
}

async fn export(
    core: &Core,
    b: &Binding,
    remote: &dyn Remote,
    c: &Card,
    origin: &str,
) -> Result<()> {
    let column = b.column_of(&c.status);
    let item = match remote.create(c.title.trim(), c.body.trim(), column).await {
        Ok(i) => clean_item(i),
        Err(e) => {
            audit(
                core,
                b,
                "kanban.create",
                &c.key,
                "failed",
                json!({ "card": c.key, "why": short(&e), "origin": origin }),
            )
            .await;
            return Err(e);
        }
    };
    let base = BTreeMap::from([
        ("title".to_string(), c.title.trim().to_string()),
        ("body".to_string(), c.body.trim().to_string()),
        (
            "column".to_string(),
            column.unwrap_or(&item.column).to_string(),
        ),
    ]);
    core.store()
        .kanban_link_put(&KanbanLink {
            provider: b.provider.clone(),
            board: b.board.clone(),
            card_id: c.id.clone(),
            remote_id: item.id.clone(),
            remote_url: item.url.clone(),
            base_json: serde_json::to_string(&base)?,
            ..Default::default()
        })
        .await?;
    audit(
        core,
        b,
        "kanban.create",
        &item.id,
        "ok",
        json!({
            "card": c.key, "cardId": c.id, "url": item.url, "column": column, "origin": origin,
        }),
    )
    .await;
    Ok(())
}

// ---- conflicts -------------------------------------------------------------------

/// An open conflict as the Settings list shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ConflictRow {
    pub card_id: String,
    pub card_key: String,
    pub url: String,
    pub fields: BTreeMap<String, Held>,
}

/// The bound board's open conflicts.
pub async fn conflicts(core: &Core) -> Result<Vec<ConflictRow>> {
    let Some(b) = Binding::load(core).await? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for l in core.store().kanban_links(&b.provider, &b.board).await? {
        let held = held_of(&l);
        if held.is_empty() || l.state != "linked" {
            continue;
        }
        let key = core
            .card(&l.card_id)
            .await?
            .map(|c| c.key)
            .unwrap_or_default();
        out.push(ConflictRow {
            card_id: l.card_id.clone(),
            card_key: key,
            url: l.remote_url.clone(),
            fields: held,
        });
    }
    Ok(out)
}

/// Settles one held field: `keep_board` keeps the card's value (the next
/// pass pushes it), otherwise the outside value (the next pass takes it).
/// Neither side is written here; audited with the person as the actor.
pub async fn resolve_conflict(
    core: &Core,
    card_id: &str,
    field: &str,
    keep_board: bool,
) -> Result<()> {
    let Some(b) = Binding::load(core).await? else {
        bail!("nothing is bound");
    };
    let mut l = core
        .store()
        .kanban_links_of_card(card_id)
        .await?
        .into_iter()
        .find(|l| l.provider == b.provider && l.board == b.board)
        .ok_or_else(|| anyhow!("the card is not on the bound board"))?;
    let mut held = held_of(&l);
    let h = held
        .remove(field)
        .ok_or_else(|| anyhow!("no conflict on {field}"))?;
    let mut base = base_of(&l);
    // The base becomes the side that lost, so the winner reads as the change.
    base.insert(
        field.into(),
        if keep_board {
            h.remote.clone()
        } else {
            h.board.clone()
        },
    );
    l.base_json = serde_json::to_string(&base)?;
    l.conflict_json = if held.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&held)?
    };
    core.store().kanban_link_put(&l).await?;
    record(
        core,
        "owner",
        "user",
        "kanban.resolve",
        &l.remote_id,
        "ok",
        json!({
            "provider": b.provider, "board": b.board, "cardId": card_id, "field": field,
            "kept": if keep_board { "board" } else { "remote" },
        }),
    )
    .await;
    Ok(())
}

// ---- status and the tick -------------------------------------------------------------

/// The Settings status line.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    pub binding: Option<Binding>,
    pub run: Option<KanbanRun>,
    pub linked: usize,
    pub conflicts: usize,
    pub token_needed: bool,
    pub token_set: bool,
}

pub async fn status(core: &Core) -> Result<Status> {
    let Some(b) = Binding::load(core).await? else {
        return Ok(Status::default());
    };
    let links = core.store().kanban_links(&b.provider, &b.board).await?;
    let token_needed = TOKEN_PROVIDERS.contains(&b.provider.as_str());
    Ok(Status {
        run: core.store().kanban_run(&b.provider, &b.board).await?,
        linked: links.iter().filter(|l| l.state == "linked").count(),
        conflicts: links.iter().filter(|l| !held_of(l).is_empty()).count(),
        token_needed,
        token_set: token_needed && token_set(core).await,
        binding: Some(b),
    })
}

/// One pass of the bound board (`None` when nothing is bound). `origin`:
/// `tick` or `owner` (the Settings Sync now).
pub async fn sync_now(core: &Core, origin: &str) -> Result<Option<Report>> {
    let Some(b) = Binding::load(core).await? else {
        return Ok(None);
    };
    let remote = match remote_for(core, &b).await {
        Ok(r) => r,
        Err(e) => {
            audit(
                core,
                &b,
                "kanban.sync",
                &b.target(),
                "unavailable",
                json!({ "origin": origin, "why": short(&e) }),
            )
            .await;
            let rep = json!({ "errors": [short(&e)] }).to_string();
            core.store()
                .kanban_run_put(&b.provider, &b.board, "unavailable", &rep, false)
                .await?;
            return Err(e);
        }
    };
    sync_with(core, &b, remote.as_ref(), origin).await.map(Some)
}

/// The periodic pass (every [`TICK_EVERY`]): errors are logged, refusals
/// are already audited.
pub async fn tick(core: &Core) -> Option<Report> {
    match sync_now(core, "tick").await {
        Ok(r) => r,
        Err(e) if crate::control::is_blocked(&e) => None,
        Err(e) => {
            tracing::warn!("kanban sync: {e:#}");
            None
        }
    }
}

/// Runs [`tick`] for the life of the process, first after one period.
pub fn start(core: &Core) -> tokio::task::JoinHandle<()> {
    let core = core.clone();
    tokio::spawn(async move {
        let mut every =
            tokio::time::interval_at(tokio::time::Instant::now() + TICK_EVERY, TICK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&core).await;
        }
    })
}
