//! Library search domain: search (local index plus live sources), preview
//! (fetch, convert, scan), import (review, provenance, audit), update
//! checks and source refreshes. Spec: `docs/gaps/library-search.md` §4.4
//! and §6a.
//!
//! Rules (WORKSTREAMS "Non-negotiable rules"):
//! 1. Provenance: every imported version gets a `library_provenance` row
//!    with the exact URLs, the source-file hash and the canonical hash.
//! 2. Audit: imports, updates, refusals, update checks that find something,
//!    source changes and refreshes that change rows are chained in the
//!    ledger ([`Core::audit_record`]).
//! 3. Kill switch: [`guard`] runs before every network fetch and every
//!    library write; the fetcher's gate checks it again before each request
//!    so an in-flight refresh stops at the next boundary.
//! 4. A2A: search and import are person-only. Nothing here takes an agent,
//!    session or run context; [`guard_actor`] refuses any other actor.
//!    Imported agents never carry A2A or peer settings.
//!
//! Nothing fetched is executed, sourced or installed. Hooks and MCP stdio
//! servers are stored only.

use crate::{Core, control, vault};
use anyhow::{Result, anyhow, bail};
use kls_libsearch::convert::{self as conv, Converted, Target};
use kls_libsearch::fetch::{self, Ctx, Fetcher, Gate, GhApi, HttpFetcher};
use kls_libsearch::sources::{self, Config};
use kls_libsearch::{
    Ecosystem, Facets, Fetched, Flag, Hit, Index, ItemType, Locator, Query, Source, SourceState,
};
use kls_store::audit::LedgerDraft;
use kls_store::libsearch::{Provenance, UpdateState};
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

pub use kls_libsearch;

pub mod kinds {
    /// `{source, items, added, retired, notModified}`
    pub const LIBSEARCH_REFRESHED: &str = "libsearch.refreshed";
    /// `{host, until}`
    pub const LIBSEARCH_LIMITED: &str = "libsearch.limited";
    /// `{id, on|added|removed}`
    pub const LIBSEARCH_SOURCE_CHANGED: &str = "libsearch.source_changed";
    /// `{kind, id, version, source}`
    pub const LIBRARY_IMPORTED_FROM: &str = "library.imported_from";
    /// `{kind, id, state}`
    pub const LIBRARY_UPDATE: &str = "library.update";
    /// `{action, target, reason}`
    pub const LIBRARY_REFUSED: &str = "library.refused";
    /// Every kind this module emits, for screen loaders.
    pub const PREFIXES: [&str; 2] = ["libsearch.", "library."];
}

/// Remote sources get this long per query.
pub const REMOTE_DEADLINE: Duration = Duration::from_secs(4);
/// Background refresh runs at most this often per source.
pub const REFRESH_EVERY_MS: i64 = 6 * 3_600_000;

// ---- the engine ------------------------------------------------------------------

/// The per-data-dir search state: the index, the fetcher and `gh`.
pub struct Engine {
    pub index: Index,
    pub http: Arc<dyn Fetcher>,
    gh: RwLock<Option<Arc<dyn GhApi>>>,
    /// Sources added in process (tests, a FakeSource), always on.
    extra: RwLock<Vec<Arc<dyn Source>>>,
    /// Remote sources that failed recently (offline chips): id → when.
    offline: Mutex<BTreeMap<String, i64>>,
}

type Registry = Mutex<Vec<(PathBuf, Arc<Engine>)>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn lookup(core: &Core) -> Option<Arc<Engine>> {
    registry()
        .lock()
        .unwrap()
        .iter()
        .find(|(d, _)| d == core.data_dir())
        .map(|(_, e)| e.clone())
}

/// Installs an engine over `http` (tests use a `FakeFetcher`).
pub async fn install(core: &Core, http: Arc<dyn Fetcher>) -> Result<Arc<Engine>> {
    let index = Index::open(&core.data_dir().join("libsearch.db")).await?;
    let e = Arc::new(Engine {
        index,
        http,
        gh: RwLock::new(None),
        extra: Default::default(),
        offline: Default::default(),
    });
    let mut r = registry().lock().unwrap();
    r.retain(|(d, _)| d != core.data_dir());
    r.push((core.data_dir().to_path_buf(), e.clone()));
    Ok(e)
}

/// The engine for this core: the real fetcher (allowlist, cache, budgets)
/// on first use.
pub async fn engine(core: &Core) -> Result<Arc<Engine>> {
    if let Some(e) = lookup(core) {
        return Ok(e);
    }
    let index = Index::open(&core.data_dir().join("libsearch.db")).await?;
    let http: Arc<dyn Fetcher> = Arc::new(HttpFetcher::new(Some(index.cache()))?);
    let e = Arc::new(Engine {
        index,
        http,
        gh: RwLock::new(None),
        extra: Default::default(),
        offline: Default::default(),
    });
    let mut r = registry().lock().unwrap();
    if let Some((_, e)) = r.iter().find(|(d, _)| d == core.data_dir()) {
        return Ok(e.clone());
    }
    r.push((core.data_dir().to_path_buf(), e.clone()));
    Ok(e)
}

/// Sets (or clears) the signed-in `gh` the gh boost uses (WP8). The UI
/// installs one built on `kls_github` when `gh auth status` is ok.
pub async fn set_gh_api(core: &Core, gh: Option<Arc<dyn GhApi>>) -> Result<()> {
    *engine(core).await?.gh.write().unwrap() = gh;
    Ok(())
}

pub async fn gh_api(core: &Core) -> Option<Arc<dyn GhApi>> {
    engine(core).await.ok()?.gh.read().unwrap().clone()
}

/// The kill switch as the fetcher's gate.
struct CoreGate(Core);

#[async_trait::async_trait]
impl Gate for CoreGate {
    async fn open(&self) -> bool {
        !stopped(&self.0).await
    }
}

/// Whether the kill switch blocks network fetches and library writes: the
/// factory stopped or paused. A store error counts as stopped.
pub async fn stopped(core: &Core) -> bool {
    let c = core.factory_control().await;
    let s = control::state(core).await;
    match (c, s) {
        (Ok(c), Ok(s)) => c.stopped || !s.mode.running(),
        _ => true,
    }
}

async fn ctx(core: &Core, e: &Engine, background: bool) -> Ctx {
    let mut cx = Ctx::new(e.http.clone());
    cx.gate = Some(Arc::new(CoreGate(core.clone())));
    cx.gh = e.gh.read().unwrap().clone();
    if node_gh_on(core).await {
        cx.node_gh = Some(Arc::new(NodeGhRoute(core.clone())));
    }
    if background {
        cx = cx.background();
    }
    cx
}

// ---- WP8: a belai node's gh as the GitHub path (off by default) ------------------

/// The node job that runs `gh` (belai `rc/factoryjobs.go` `jobKindGitHub`).
pub const NODE_GH_JOB_KIND: &str = "github";
/// The setting row that turns the node fallback on (off by default).
pub const NODE_GH_SETTING: &str = "setting:node_gh";
const NODE_GH_TIMEOUT_MS: i64 = 90_000;

/// Whether the person turned the node `gh` fallback on.
pub async fn node_gh_on(core: &Core) -> bool {
    core.store()
        .library_sources()
        .await
        .map(|rows| rows.iter().any(|r| r.id == NODE_GH_SETTING && r.enabled))
        .unwrap_or(false)
}

/// GitHub through a connected node that advertises a signed-in `gh`
/// (`JobNeeds.gh`), when this process has no usable path. Read-only `gh
/// api` calls only ([`fetch::gh_args_for`]); held by the kill switch; each
/// fetch is chained as `libsearch.node_fetch` with origin `host:<node>`.
struct NodeGhRoute(Core);

#[async_trait::async_trait]
impl fetch::NodeGh for NodeGhRoute {
    async fn gh(
        &self,
        args: Vec<String>,
        url: &str,
        class: fetch::Class,
    ) -> Result<Option<bytes::Bytes>> {
        let core = &self.0;
        if stopped(core).await {
            return Err(fetch::Stopped.into());
        }
        if args.first().map(String::as_str) != Some("api") {
            bail!("the node fallback runs gh api only");
        }
        let payload = json!({ "args": args, "stdin": Value::Null });
        let needs = crate::JobNeeds {
            gh: true,
            ..Default::default()
        };
        let out = core
            .gateway()?
            .job(
                None,
                NODE_GH_JOB_KIND,
                &payload.to_string(),
                needs,
                NODE_GH_TIMEOUT_MS,
            )
            .await;
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct GhOut {
            stdout: String,
            stderr: String,
            exit: i64,
            truncated: bool,
        }
        let (node, result) = match out {
            Ok(o) if o.status == "ok" => {
                let g: GhOut = serde_json::from_str(&o.output_json).unwrap_or_default();
                let r = if g.truncated {
                    Err(anyhow!("{url}: the node cut the answer"))
                } else if g.exit == 0 {
                    Ok(Some(bytes::Bytes::from(g.stdout)))
                } else if g.stderr.contains("404") || g.stderr.contains("Not Found") {
                    Ok(None)
                } else {
                    Err(anyhow!("{url}: gh on the node exited {}", g.exit))
                };
                (o.node_id, r)
            }
            Ok(o) => (o.node_id, Err(anyhow!("{url}: node job {}", o.status))),
            Err(e) => (String::new(), Err(e.context(format!("{url}: no node gh")))),
        };
        let outcome = match &result {
            Ok(Some(b)) => format!("ok:{}", b.len()),
            Ok(None) => "not_found".into(),
            Err(_) => "failed".into(),
        };
        let actor = if node.is_empty() {
            Actor::System
        } else {
            Actor::Node(node.clone())
        };
        let r = core
            .audit_record(LedgerDraft {
                kind: "libsearch.node_fetch".into(),
                actor: actor.as_str(),
                actor_kind: if node.is_empty() { "system" } else { "host" }.into(),
                msg: format!("libsearch.node_fetch {url} {outcome}"),
                subject: json!({ "url": url }),
                payload: json!({ "url": url, "node": node, "outcome": outcome,
                                 "class": format!("{class:?}"), "job": NODE_GH_JOB_KIND }),
                origin: if node.is_empty() {
                    "factory".into()
                } else {
                    format!("host:{node}")
                },
                node_id: node,
                ref_key: Some(format!("libsearch:node_fetch:{}", uuid::Uuid::now_v7())),
                ..Default::default()
            })
            .await;
        if let Err(e) = r {
            tracing::warn!("libsearch.node_fetch ledger: {e:#}");
        }
        result
    }
}

// ---- guard (rules 1-4) -----------------------------------------------------------

/// What is about to happen.
#[derive(Clone, Debug)]
pub enum Action<'a> {
    Refresh(&'a str),
    RemoteSearch(&'a str),
    Preview(&'a Locator),
    Import(&'a Locator),
    Update(&'a str),
    CheckUpdates,
    NodeConvert,
    SourceChange(&'a str),
}

impl Action<'_> {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Refresh(_) => "refresh",
            Action::RemoteSearch(_) => "remote_search",
            Action::Preview(_) => "preview",
            Action::Import(_) => "import",
            Action::Update(_) => "update",
            Action::CheckUpdates => "check_updates",
            Action::NodeConvert => "node_convert",
            Action::SourceChange(_) => "source_change",
        }
    }

    pub fn target(&self) -> String {
        match self {
            Action::Refresh(s) | Action::RemoteSearch(s) | Action::Update(s) => s.to_string(),
            Action::SourceChange(s) => s.to_string(),
            Action::Preview(l) | Action::Import(l) => l.to_json(),
            Action::CheckUpdates | Action::NodeConvert => "library".into(),
        }
    }

    /// Source changes are config, not fetches or writes from search: the
    /// kill switch does not hold them.
    fn held_by_kill_switch(&self) -> bool {
        !matches!(self, Action::SourceChange(_))
    }
}

/// Who asked. Only a person may search, preview or import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    User,
    /// Background refresh (still started by a person opening the Library).
    System,
    Agent(String),
    Run(String),
    Node(String),
}

impl Actor {
    pub fn as_str(&self) -> String {
        match self {
            Actor::User => "user".into(),
            Actor::System => "system".into(),
            Actor::Agent(a) => format!("agent:{a}"),
            Actor::Run(r) => format!("run:{r}"),
            Actor::Node(n) => format!("node:{n}"),
        }
    }
}

/// Writes one ledger entry and returns its seq (an already-chained
/// `ref_key` returns the existing nothing: `None`).
async fn ledger(
    core: &Core,
    kind: &str,
    actor: &Actor,
    msg: String,
    subject: Value,
    payload: Value,
    ref_key: String,
) -> Result<Option<i64>> {
    let actor_kind = match actor {
        Actor::User => "user",
        Actor::System => "system",
        Actor::Agent(_) | Actor::Run(_) => "agent",
        Actor::Node(_) => "host",
    };
    let e = core
        .audit_record(LedgerDraft {
            at: 0,
            kind: kind.into(),
            actor: actor.as_str(),
            actor_kind: actor_kind.into(),
            msg,
            subject,
            payload,
            origin: "factory".into(),
            node_id: String::new(),
            session_id: String::new(),
            ref_key: Some(ref_key),
        })
        .await?;
    Ok(e.map(|e| e.seq))
}

/// Refuses: emits `library.refused` and chains `library.refused` with the
/// outcome. Returns the error to hand back.
async fn refuse(
    core: &Core,
    actor: &Actor,
    a: &Action<'_>,
    outcome: &str,
    detail: &str,
) -> anyhow::Error {
    let target = a.target();
    let data = json!({
        "action": a.name(), "target": target, "reason": outcome, "detail": detail,
        "actor": actor.as_str(), "outcome": outcome,
    });
    if let Err(e) = core.emit(kinds::LIBRARY_REFUSED, &target, &data).await {
        tracing::warn!("library.refused event: {e:#}");
    }
    let window = if outcome == "kill_switch" {
        stop_window(core).await
    } else {
        None
    };
    let r = ledger(
        core,
        "library.refused",
        actor,
        format!("library.refused {} {outcome}", a.name()),
        json!({"action": a.name(), "target": target}),
        data,
        refused_key(a, window),
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("library.refused ledger: {e:#}");
    }
    let what = match outcome {
        "kill_switch" => "the factory is stopped".to_string(),
        "refused:agent_actor" => "library search and import are for people only".to_string(),
        "review" => format!("review incomplete: {detail}"),
        "hash_mismatch" => "the upstream changed since the preview".to_string(),
        o => format!("{o}: {detail}"),
    };
    anyhow!("{} refused: {what}", a.name())
}

/// When the factory stopped or paused (the kill-switch window a refusal
/// belongs to); `None` while running or unknown.
async fn stop_window(core: &Core) -> Option<i64> {
    let c = core.factory_control().await.ok()?;
    if c.stopped { c.stopped_since } else { None }
}

/// The ledger key of a refusal. Inside a stop window a remote search is
/// refused once (whatever the query) and any other action once per target;
/// every other refusal is its own entry.
fn refused_key(a: &Action<'_>, window: Option<i64>) -> String {
    match (window, a) {
        (Some(w), Action::RemoteSearch(_)) => format!("libsearch:refused:remote_search:stop:{w}"),
        (Some(w), _) => format!("libsearch:refused:{}:{}:stop:{w}", a.name(), a.target()),
        (None, _) => format!(
            "libsearch:refused:{}:{}:{}",
            a.name(),
            a.target(),
            uuid::Uuid::now_v7()
        ),
    }
}

/// Whether this stop window's remote-search refusal is already chained.
async fn search_refused_this_window(core: &Core) -> bool {
    let Some(w) = stop_window(core).await else {
        return false;
    };
    core.store()
        .ledger_has_ref(&refused_key(&Action::RemoteSearch(""), Some(w)))
        .await
        .unwrap_or(false)
}

/// Refuses any actor but a person (or the system's own refresh).
pub async fn guard_actor(core: &Core, actor: &Actor, a: Action<'_>) -> Result<()> {
    let ok = match actor {
        Actor::User => true,
        Actor::System => matches!(a, Action::Refresh(_) | Action::CheckUpdates),
        _ => false,
    };
    if ok {
        return Ok(());
    }
    Err(refuse(core, actor, &a, "refused:agent_actor", "").await)
}

/// Rules 1-3 before any network fetch or library write: the actor must be
/// a person and the kill switch off. A refusal is emitted and chained.
pub async fn guard(core: &Core, a: Action<'_>) -> Result<()> {
    guard_as(core, &Actor::User, a).await
}

pub async fn guard_as(core: &Core, actor: &Actor, a: Action<'_>) -> Result<()> {
    guard_actor(core, actor, a.clone()).await?;
    if a.held_by_kill_switch() {
        // The governance check first (it emits the audited factory.refused).
        let checked = control::check(core, "library", &a.target(), &actor.as_str()).await;
        if checked.is_err() || stopped(core).await {
            return Err(refuse(core, actor, &a, "kill_switch", "").await);
        }
    }
    Ok(())
}

// ---- sources ----------------------------------------------------------------------

/// The source configuration from the store.
pub async fn config(core: &Core) -> Result<Config> {
    let mut cfg = Config::default();
    for r in core.store().library_sources().await? {
        let spec: Value = serde_json::from_str(&r.spec_json).unwrap_or(json!({}));
        match r.kind.as_str() {
            "builtin" => {
                if r.enabled {
                    cfg.on.insert(r.id.clone());
                } else {
                    cfg.off.insert(r.id.clone());
                }
            }
            "tap" if r.enabled => {
                let globs: Vec<String> = spec
                    .get("globs")
                    .and_then(|g| serde_json::from_value(g.clone()).ok())
                    .unwrap_or_default();
                let s = spec.get("spec").and_then(Value::as_str).unwrap_or_default();
                if let Ok(mut t) = kls_libsearch::catalogue::Tap::parse(s, &globs) {
                    t.item_type = spec
                        .get("type")
                        .and_then(Value::as_str)
                        .and_then(ItemType::parse);
                    t.ecosystem = spec
                        .get("eco")
                        .and_then(Value::as_str)
                        .and_then(Ecosystem::parse);
                    cfg.taps.push(t);
                }
            }
            "well_known" if r.enabled => {
                if let Some(u) = spec.get("url").and_then(Value::as_str) {
                    cfg.well_known.push(u.to_string());
                }
            }
            "setting" => match r.id.as_str() {
                "setting:gh" => cfg.gh = r.enabled,
                "setting:skills_sh" => cfg.skills_sh = r.enabled,
                _ => {}
            },
            _ => {}
        }
    }
    Ok(cfg)
}

async fn all_sources(core: &Core, e: &Engine) -> Result<(Config, Vec<Arc<dyn Source>>)> {
    let cfg = config(core).await?;
    for u in &cfg.well_known {
        if let Ok(h) = fetch::https_host(u) {
            e.http.allow_host(&h);
        }
    }
    let mut all = sources::all(&cfg);
    all.extend(e.extra.read().unwrap().iter().cloned());
    Ok((cfg, all))
}

/// Adds an in-process source (tests drive the domain with a fake one).
pub async fn add_source(core: &Core, s: Arc<dyn Source>) -> Result<()> {
    engine(core).await?.extra.write().unwrap().push(s);
    Ok(())
}

/// One source as the Sources panel shows it.
#[derive(Clone, Debug)]
pub struct SourceView {
    pub id: String,
    pub label: String,
    pub types: Vec<ItemType>,
    pub mode: kls_libsearch::Mode,
    pub on: bool,
    pub needs_gh: bool,
    /// `tap`, `well_known` or `builtin`.
    pub kind: String,
    pub state: Option<SourceState>,
    /// When a remote source last failed (offline chip).
    pub offline_at: Option<i64>,
}

/// Everything the Discover tab shows besides hits.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub sources: Vec<SourceView>,
    pub budgets: Vec<fetch::BudgetView>,
    pub gh_on: bool,
    pub gh_signed_in: bool,
    pub gh_quota: Option<(i64, i64)>,
    pub stopped: bool,
    pub skills_sh: bool,
    /// WP8 node `gh` fallback (off by default).
    pub node_gh: bool,
}

pub async fn status(core: &Core) -> Result<Status> {
    let e = engine(core).await?;
    let (cfg, all) = all_sources(core, &e).await?;
    let states = e.index.state().await?;
    let rows = core.store().library_sources().await?;
    let offline = e.offline.lock().unwrap().clone();
    let gh = e.gh.read().unwrap().clone();
    let sources = all
        .iter()
        .map(|s| SourceView {
            id: s.id().to_string(),
            label: s.label().to_string(),
            types: s.types().to_vec(),
            mode: s.mode(),
            on: sources::is_on(s.as_ref(), &cfg),
            needs_gh: s.needs_gh(),
            kind: rows
                .iter()
                .find(|r| r.id == s.id())
                .map(|r| r.kind.clone())
                .filter(|k| k != "setting")
                .unwrap_or_else(|| "builtin".into()),
            state: states.iter().find(|st| st.source == s.id()).cloned(),
            offline_at: offline.get(s.id()).copied(),
        })
        .collect();
    Ok(Status {
        sources,
        budgets: e.http.budgets(),
        gh_on: cfg.gh,
        gh_signed_in: gh.is_some(),
        gh_quota: gh.as_ref().and_then(|g| g.quota()),
        stopped: stopped(core).await,
        skills_sh: cfg.skills_sh,
        node_gh: rows.iter().any(|r| r.id == NODE_GH_SETTING && r.enabled),
    })
}

async fn source_changed(core: &Core, id: &str, outcome: &str) -> Result<()> {
    core.emit(
        kinds::LIBSEARCH_SOURCE_CHANGED,
        id,
        &json!({"id": id, "outcome": outcome, "by": "user"}),
    )
    .await?;
    ledger(
        core,
        "libsearch.source_changed",
        &Actor::User,
        format!("libsearch.source_changed {id} {outcome}"),
        json!({"source": id}),
        json!({"source": id, "outcome": outcome}),
        format!("libsearch:source_changed:{id}:{}", uuid::Uuid::now_v7()),
    )
    .await?;
    Ok(())
}

/// Turns a source on or off.
pub async fn set_source(core: &Core, id: &str, on: bool) -> Result<()> {
    guard(core, Action::SourceChange(id)).await?;
    let row = core
        .store()
        .library_sources()
        .await?
        .into_iter()
        .find(|r| r.id == id);
    let (kind, spec) = row
        .map(|r| (r.kind, r.spec_json))
        .unwrap_or_else(|| ("builtin".into(), "{}".into()));
    core.store()
        .save_library_source(id, &kind, &spec, on)
        .await?;
    source_changed(core, id, if on { "on" } else { "off" }).await
}

/// The gh boost (WP8), the node gh fallback (`node_gh`, off by default)
/// and the skills.sh feature flag.
pub async fn set_setting(core: &Core, key: &str, on: bool) -> Result<()> {
    let id = format!("setting:{key}");
    guard(core, Action::SourceChange(&id)).await?;
    if !matches!(key, "gh" | "skills_sh" | "node_gh") {
        bail!("unknown library search setting {key}");
    }
    core.store()
        .save_library_source(&id, "setting", "{}", on)
        .await?;
    source_changed(core, &id, if on { "on" } else { "off" }).await
}

/// Adds a tap: `owner/repo[@ref]` with optional globs, type and ecosystem.
pub async fn add_tap(
    core: &Core,
    spec: &str,
    globs: &[String],
    item_type: Option<ItemType>,
    eco: Option<Ecosystem>,
) -> Result<String> {
    let tap = kls_libsearch::catalogue::Tap::parse(spec.trim(), globs)?;
    let id = tap.id();
    guard(core, Action::SourceChange(&id)).await?;
    let spec_json = json!({
        "spec": spec.trim(), "globs": globs,
        "type": item_type.map(|t| t.as_str()), "eco": eco.map(|e| e.as_str()),
    });
    core.store()
        .save_library_source(&id, "tap", &spec_json.to_string(), true)
        .await?;
    source_changed(core, &id, "added").await?;
    Ok(id)
}

/// Adds a `/.well-known/skills/index.json` site (https only).
pub async fn add_well_known(core: &Core, url: &str) -> Result<String> {
    let url = url.trim();
    let host = fetch::https_host(url)?;
    let url = if url.ends_with("index.json") {
        url.to_string()
    } else {
        format!(
            "{}/.well-known/skills/index.json",
            url.trim_end_matches('/')
        )
    };
    let id = format!("well-known:{host}");
    guard(core, Action::SourceChange(&id)).await?;
    core.store()
        .save_library_source(&id, "well_known", &json!({"url": url}).to_string(), true)
        .await?;
    source_changed(core, &id, "added").await?;
    Ok(id)
}

/// Removes a tap or well-known site and its index rows.
pub async fn remove_source(core: &Core, id: &str) -> Result<bool> {
    guard(core, Action::SourceChange(id)).await?;
    let gone = core.store().delete_library_source(id).await?;
    engine(core).await?.index.forget(id).await?;
    if gone {
        source_changed(core, id, "removed").await?;
    }
    Ok(gone)
}

// ---- refresh ------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RefreshReport {
    pub refreshed: usize,
    pub not_modified: usize,
    pub failed: Vec<(String, String)>,
    pub added: u64,
    pub retired: u64,
}

/// Refreshes one indexed source, or every enabled one (`None`).
pub async fn refresh(core: &Core, source: Option<&str>) -> Result<RefreshReport> {
    refresh_as(core, &Actor::User, source).await
}

/// Background refresh: only sources older than [`REFRESH_EVERY_MS`], and
/// nothing while the kill switch is on (not even a refusal).
pub async fn refresh_stale(core: &Core) -> Result<RefreshReport> {
    if stopped(core).await {
        return Ok(RefreshReport::default());
    }
    let e = engine(core).await?;
    let (cfg, all) = all_sources(core, &e).await?;
    let states = e.index.state().await?;
    let now = now_ms();
    let mut report = RefreshReport::default();
    for s in all
        .iter()
        .filter(|s| s.mode().indexed() && sources::is_on(s.as_ref(), &cfg))
    {
        let st = states.iter().find(|x| x.source == s.id());
        let due = st.is_none_or(|st| {
            st.refreshed_at.unwrap_or(0) + REFRESH_EVERY_MS < now
                && st.parked_until.unwrap_or(0) < now
        });
        if due {
            let r = refresh_as(core, &Actor::System, Some(s.id())).await?;
            report.refreshed += r.refreshed;
            report.not_modified += r.not_modified;
            report.failed.extend(r.failed);
            report.added += r.added;
            report.retired += r.retired;
        }
    }
    Ok(report)
}

pub async fn refresh_as(core: &Core, actor: &Actor, source: Option<&str>) -> Result<RefreshReport> {
    guard_as(core, actor, Action::Refresh(source.unwrap_or("all"))).await?;
    let e = engine(core).await?;
    let (cfg, all) = all_sources(core, &e).await?;
    let cx = ctx(core, &e, *actor != Actor::User).await;
    let mut report = RefreshReport::default();
    for s in all.iter().filter(|s| {
        s.mode().indexed()
            && match source {
                Some(id) => s.id() == id,
                None => sources::is_on(s.as_ref(), &cfg),
            }
    }) {
        let id = s.id().to_string();
        let st = e.index.source_state(&id).await?;
        let now = now_ms();
        let before = e.index.count(&id).await?;
        match s
            .refresh(&cx, st.as_ref().and_then(|x| x.cursor.as_deref()))
            .await
        {
            Ok(r) if r.not_modified => {
                report.not_modified += 1;
                e.index
                    .set_state(&id, r.cursor.as_deref(), None, now, None, None)
                    .await?;
                core.emit(
                    kinds::LIBSEARCH_REFRESHED,
                    &id,
                    &json!({"source": id, "items": before, "notModified": true}),
                )
                .await?;
            }
            Ok(r) => {
                e.index.upsert_at(&id, &r.hits, now).await?;
                let retired = if r.partial {
                    0
                } else {
                    e.index.retire(&id, now).await?
                };
                let after = e.index.count(&id).await?;
                let added = (after + retired as i64 - before).max(0) as u64;
                e.index
                    .set_state(&id, r.cursor.as_deref(), None, now, None, None)
                    .await?;
                report.refreshed += 1;
                report.added += added;
                report.retired += retired;
                let data = json!({
                    "source": id, "items": after, "added": added, "retired": retired,
                    "notModified": false,
                });
                core.emit(kinds::LIBSEARCH_REFRESHED, &id, &data).await?;
                if added > 0 || retired > 0 {
                    ledger(
                        core,
                        "libsearch.refreshed",
                        actor,
                        format!("libsearch.refreshed {id} items={after}"),
                        json!({"source": id}),
                        data,
                        format!("libsearch:refreshed:{id}:{now}"),
                    )
                    .await?;
                }
            }
            Err(err) => {
                if fetch::is_stopped(&err) {
                    // The switch went on mid-refresh: stop at this boundary.
                    let _ = refuse(
                        core,
                        actor,
                        &Action::Refresh(&id),
                        "kill_switch",
                        "in flight",
                    )
                    .await;
                    break;
                }
                let parked = fetch::is_limited(&err).map(|l| l.until);
                if let Some(l) = fetch::is_limited(&err) {
                    core.emit(
                        kinds::LIBSEARCH_LIMITED,
                        &id,
                        &json!({"host": l.host, "until": l.until, "source": id}),
                    )
                    .await?;
                }
                let msg = format!("{err:#}");
                e.index
                    .set_state(&id, None, None, now, Some(&msg), parked)
                    .await?;
                report.failed.push((id, msg));
            }
        }
    }
    Ok(report)
}

// ---- search -------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub facets: Facets,
    /// The kill switch is on: local index only.
    pub stopped: bool,
    /// Remote sources that could not answer (offline / limited).
    pub offline: Vec<String>,
    /// Remote sources that answered live.
    pub live: Vec<String>,
}

/// Index ∪ live remote sources. Under the kill switch: index only (the
/// remote query is refused and audited once per stop window, not once per
/// query typed).
pub async fn search(core: &Core, q: &Query) -> Result<SearchResult> {
    let e = engine(core).await?;
    let mut out = SearchResult::default();
    let remote_wanted = !q.text.trim().is_empty();
    if remote_wanted {
        if stopped(core).await {
            // The window's first query goes through the guard (the kill
            // switch check and the chained refusal); later ones in the same
            // window are already refused and audited.
            if !search_refused_this_window(core).await {
                let _ = guard(core, Action::RemoteSearch(&q.text)).await;
            }
            out.stopped = true;
        } else {
            guard(core, Action::RemoteSearch(&q.text)).await?;
            let (cfg, all) = all_sources(core, &e).await?;
            let cx = ctx(core, &e, false).await.with_deadline(REMOTE_DEADLINE);
            let remotes: Vec<_> = all
                .iter()
                .filter(|s| {
                    s.mode().remote()
                        && sources::is_on(s.as_ref(), &cfg)
                        && (q.sources.is_empty() || q.sources.iter().any(|x| x == s.id()))
                        && (q.types.is_empty() || q.types.iter().any(|t| s.types().contains(t)))
                })
                .cloned()
                .collect();
            let futs = remotes.iter().map(|s| {
                let cx = cx.clone();
                async move { (s.id().to_string(), s.search(&cx, q).await) }
            });
            for (id, r) in fetch::join_all(futs).await {
                match r {
                    Ok(hits) => {
                        // Seen rows: they answer offline later.
                        if !hits.is_empty() {
                            e.index.upsert(&id, &hits).await?;
                        }
                        e.offline.lock().unwrap().remove(&id);
                        out.live.push(id);
                    }
                    Err(err) => {
                        if let Some(l) = fetch::is_limited(&err) {
                            let _ = core
                                .emit(
                                    kinds::LIBSEARCH_LIMITED,
                                    &id,
                                    &json!({"host": l.host, "until": l.until, "source": id}),
                                )
                                .await;
                        }
                        tracing::debug!("library search {id}: {err:#}");
                        e.offline.lock().unwrap().insert(id.clone(), now_ms());
                        out.offline.push(id);
                    }
                }
            }
        }
    }
    out.hits = e.index.search(q).await?;
    out.facets = e.index.facets(q).await?;
    Ok(out)
}

// ---- preview ------------------------------------------------------------------------

/// What the review shows.
#[derive(Clone, Debug)]
pub struct Preview {
    pub hit: Hit,
    /// Upstream text per source file (read-only tab).
    pub source_files: Vec<(String, String)>,
    pub converted: Converted,
    /// An item of the same kind and name: (id, version, body).
    pub existing: Option<(String, i64, String)>,
    /// The fetched source (import re-checks its hash).
    pub fetched: Fetched,
    /// Set when this preview updates an imported item: its provenance.
    pub update_of: Option<Provenance>,
}

impl Preview {
    /// The text of the existing version, or of the provenance version on
    /// update (the Diff tab's left side).
    pub fn diff_base(&self) -> Option<&str> {
        self.existing.as_ref().map(|(_, _, b)| b.as_str())
    }
}

/// Fetch, convert and scan one hit. Changes no state.
pub async fn preview(core: &Core, hit: &Hit) -> Result<Preview> {
    guard(core, Action::Preview(&hit.locator)).await?;
    let e = engine(core).await?;
    let (_, all) = all_sources(core, &e).await?;
    let src = sources::find(&all, &hit.source)
        .ok_or_else(|| anyhow!("the source {} is not configured", hit.source))?;
    let cx = ctx(core, &e, false).await;
    let fetched = src.fetch(&cx, hit).await?;
    tracing::debug!(
        "library preview {}: {:?}",
        hit.source,
        fetched.upstream.urls
    );
    let opts = conv::Opts {
        rename: None,
        licence_hint: fetched
            .licence_hint
            .clone()
            .or_else(|| (!hit.licence.spdx.is_empty()).then(|| hit.licence.spdx.clone())),
    };
    let mut fetched = fetched;
    for f in &hit.flags {
        if !fetched.flags.contains(f) {
            fetched.flags.push(f.clone());
        }
    }
    let converted = conv::convert(hit.item_type, hit.ecosystem, &fetched, &opts)?;
    let existing = existing_for(core, &converted).await?;
    let source_files = fetched
        .files
        .iter()
        .map(|(p, b)| {
            let t = std::str::from_utf8(b)
                .map(str::to_string)
                .unwrap_or_else(|_| format!("({} bytes, not text)", b.len()));
            (p.clone(), t)
        })
        .collect();
    Ok(Preview {
        hit: hit.clone(),
        source_files,
        converted,
        existing,
        fetched,
        update_of: None,
    })
}

async fn existing_for(core: &Core, c: &Converted) -> Result<Option<(String, i64, String)>> {
    Ok(match c.target {
        Target::Library { kind } => match core.store().library_item_by_name(kind, &c.name).await? {
            Some(it) => {
                let v = core
                    .store()
                    .library_latest(&it.id)
                    .await?
                    .unwrap_or_default();
                Some((it.id, v.version, v.body))
            }
            None => None,
        },
        Target::Agent => core
            .store()
            .agent_profile_by_name(&c.name)
            .await?
            .map(|a| (a.id.clone(), a.version, a.prompt.clone())),
    })
}

/// Replaces the converted doc with the person's edit (re-validated,
/// re-hashed, re-scanned).
pub fn edit_converted(p: &mut Preview, text: &str) -> Result<()> {
    p.converted = conv::from_edited(&p.converted, text)?;
    Ok(())
}

// ---- import ---------------------------------------------------------------------------

/// The review's answer: the flags ticked, the licence acknowledged, an
/// optional new name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ack {
    pub flags: Vec<Flag>,
    pub licence: bool,
    pub rename: Option<String>,
}

/// Keys an imported profile must never carry (rule 4).
const A2A_KEYS: [&str; 10] = [
    "a2a",
    "peers",
    "allow_agents",
    "allowAgents",
    "crew",
    "subagents",
    "workers",
    "worker",
    "schedule",
    "schedules",
];

/// Strips A2A / peer / worker / schedule keys from a profile's extras.
pub fn strip_a2a(meta_json: &str) -> (String, Vec<String>) {
    let mut dropped = Vec::new();
    let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(meta_json) else {
        return ("{}".into(), dropped);
    };
    for k in A2A_KEYS {
        if m.remove(k).is_some() {
            dropped.push(k.to_string());
        }
    }
    (Value::Object(m).to_string(), dropped)
}

/// Saves a reviewed preview. Refuses (and audits) when the kill switch is
/// on, a flag is refused or unticked, the licence is not acknowledged, the
/// upstream no longer hashes the same, or the item is invalid. Returns
/// `(id, version)`.
pub async fn import(core: &Core, p: &Preview, ack: &Ack) -> Result<(String, i64)> {
    let loc = &p.hit.locator;
    guard(core, Action::Import(loc)).await?;
    let a = Action::Import(loc);
    let c = &p.converted;
    if let Some(r) = c.refused() {
        return Err(refuse(core, &Actor::User, &a, &format!("refused:{r}"), &r).await);
    }
    let missing: Vec<String> = c
        .review_flags()
        .into_iter()
        .filter(|f| !ack.flags.iter().any(|a| a.key() == f.key()))
        .map(|f| f.key())
        .collect();
    if !missing.is_empty() {
        return Err(refuse(core, &Actor::User, &a, "review", &missing.join(", ")).await);
    }
    if c.licence.needs_ack() && !ack.licence {
        return Err(refuse(core, &Actor::User, &a, "review", "licence not acknowledged").await);
    }
    // Re-fetch (from the cache when immutable) and compare.
    let e = engine(core).await?;
    let (_, all) = all_sources(core, &e).await?;
    let src = sources::find(&all, &p.hit.source)
        .ok_or_else(|| anyhow!("the source {} is not configured", p.hit.source))?;
    let cx = ctx(core, &e, false).await;
    let again = match src.fetch(&cx, &p.hit).await {
        Ok(f) => f,
        Err(err) if fetch::is_stopped(&err) => {
            return Err(refuse(core, &Actor::User, &a, "kill_switch", "").await);
        }
        Err(err) => return Err(err),
    };
    if again.files_sha256() != c.upstream.sha256 || p.fetched.files_sha256() != c.upstream.sha256 {
        return Err(refuse(core, &Actor::User, &a, "hash_mismatch", "").await);
    }
    // Re-validate what will be saved (the person may have edited it).
    let c = match conv::from_edited(c, &c.doc_text()) {
        Ok(c) => c,
        Err(err) => {
            return Err(refuse(core, &Actor::User, &a, &format!("invalid:{err}"), "").await);
        }
    };
    let name = ack
        .rename
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| c.name.clone());
    if !vault::valid_name(&name) {
        return Err(refuse(core, &Actor::User, &a, "invalid:name", &name).await);
    }
    let first_url = c.upstream.urls.first().cloned().unwrap_or_default();
    let at = c
        .upstream
        .commit
        .clone()
        .or_else(|| c.upstream.version.clone())
        .unwrap_or_default();
    let note = match &p.update_of {
        Some(prev) => format!("updated from {}", prev.short_ref()),
        None => format!("imported from {first_url}@{at}"),
    };
    let kind = c.target.kind();
    let before = existing_for(
        core,
        &Converted {
            name: name.clone(),
            ..c.clone()
        },
    )
    .await?;
    let (id, version) = match c.target {
        Target::Library { kind } => {
            let id = before.as_ref().map(|b| b.0.clone()).unwrap_or_default();
            let (id, v) =
                vault::save_library(core, &id, kind, &name, &c.description, &c.doc_text(), &note)
                    .await?;
            if kind == "hook" && !c.files.is_empty() {
                let files: Vec<(String, Vec<u8>)> = c
                    .files
                    .iter()
                    .map(|f| (f.path.clone(), f.data.clone()))
                    .collect();
                core.store().save_bundle_files(&id, v, &files).await?;
            }
            (id, v)
        }
        Target::Agent => {
            let mut prof = c
                .profile
                .clone()
                .ok_or_else(|| anyhow!("the converter returned no profile"))?;
            prof.name = name.clone();
            let (meta, dropped) = strip_a2a(if prof.meta_json.is_empty() {
                "{}"
            } else {
                &prof.meta_json
            });
            if !dropped.is_empty() {
                tracing::warn!("dropped A2A keys from {name}: {dropped:?}");
            }
            prof.meta_json = meta;
            prof.id = before.as_ref().map(|b| b.0.clone()).unwrap_or_default();
            let saved = vault::save_agent(core, &prof).await?;
            // Bundled skills, only where no skill of that name exists.
            for s in &c.extra_skills {
                if core
                    .store()
                    .library_item_by_name("skill", &s.name)
                    .await?
                    .is_none()
                {
                    vault::save_library(
                        core,
                        "",
                        "skill",
                        &s.name,
                        &s.description,
                        &s.doc_text(),
                        &note,
                    )
                    .await?;
                }
            }
            (saved.id, saved.version)
        }
    };
    let outcome = match &before {
        None => "created",
        Some((_, v, _)) if *v == version => "unchanged",
        Some(_) => "new_version",
    };
    let ledger_kind = if p.update_of.is_some() {
        "library.updated"
    } else {
        "library.imported"
    };
    let flags: Vec<String> = c.flags.iter().map(|f| f.key()).collect();
    let payload = json!({
        "urls": c.upstream.urls, "upstream_sha256": c.upstream.sha256, "doc_sha256": c.sha256,
        "flags": flags, "ack": ack, "licence": c.licence.spdx, "outcome": outcome,
        "source": p.hit.source, "name": name, "kind": kind, "version": version,
    });
    let seq = ledger(
        core,
        ledger_kind,
        &Actor::User,
        format!("{ledger_kind} {kind} {name} v{version} outcome={outcome}"),
        json!({"kind": kind, "id": id, "version": version}),
        payload,
        format!(
            "libsearch:{}:{id}:{version}",
            ledger_kind.trim_start_matches("library.")
        ),
    )
    .await?;
    let seq = match seq {
        Some(s) => s,
        // A retry of the same version: keep the first entry's seq.
        None => core
            .store()
            .latest_provenance(kind, &id)
            .await?
            .map(|p| p.audit_seq)
            .unwrap_or(0),
    };
    let files_json: Vec<Value> = c
        .source_files
        .iter()
        .map(|f| json!({"path": f.path, "sha256": f.sha256, "url": f.url}))
        .collect();
    core.store()
        .save_provenance(&Provenance {
            item_kind: kind.into(),
            item_id: id.clone(),
            version,
            source: p.hit.source.clone(),
            ecosystem: p.hit.ecosystem.as_str().into(),
            locator: pinned_locator(loc, &c.upstream).to_json(),
            upstream_commit: c.upstream.commit.clone().unwrap_or_default(),
            upstream_version: c.upstream.version.clone().unwrap_or_default(),
            source_urls_json: serde_json::to_string(&c.upstream.urls)?,
            upstream_sha256: c.upstream.sha256.clone(),
            doc_sha256: c.sha256.clone(),
            files_json: Value::Array(files_json).to_string(),
            licence: c.licence.spdx.clone(),
            notes_json: serde_json::to_string(&c.notes)?,
            flags_json: serde_json::to_string(&c.flags)?,
            ack_json: serde_json::to_string(ack)?,
            actor: "user".into(),
            converted_by: converted_by(&c),
            audit_seq: seq,
            reviewed_at: now_ms(),
        })
        .await?;
    core.store()
        .save_update_state(&UpdateState {
            item_kind: kind.into(),
            item_id: id.clone(),
            latest_commit: c.upstream.commit.clone().unwrap_or_default(),
            latest_version: c.upstream.version.clone().unwrap_or_default(),
            latest_sha256: c.upstream.sha256.clone(),
            checked_at: now_ms(),
            state: "current".into(),
        })
        .await?;
    core.emit(
        kinds::LIBRARY_IMPORTED_FROM,
        &id,
        &json!({"kind": kind, "id": id, "version": version, "source": p.hit.source,
                "outcome": outcome, "auditSeq": seq}),
    )
    .await?;
    Ok((id, version))
}

/// The locator pinned at the fetched commit (git) so an update compares
/// against exactly what was imported.
fn pinned_locator(loc: &Locator, up: &kls_libsearch::Upstream) -> Locator {
    match (loc, &up.commit) {
        (
            Locator::GitHub {
                owner, repo, path, ..
            },
            Some(sha),
        ) => Locator::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
            git_ref: Some(sha.clone()),
            path: path.clone(),
        },
        (Locator::ClawHub { owner, slug, .. }, None) if up.version.is_some() => Locator::ClawHub {
            owner: owner.clone(),
            slug: slug.clone(),
            version: up.version.clone(),
        },
        _ => loc.clone(),
    }
}

// ---- updates -----------------------------------------------------------------------

/// The newest upstream per imported item; flips `behind` / `gone`. Never
/// updates anything. Returns how many are behind.
pub async fn check_updates(core: &Core) -> Result<usize> {
    check_updates_as(core, &Actor::User).await
}

pub async fn check_updates_as(core: &Core, actor: &Actor) -> Result<usize> {
    guard_as(core, actor, Action::CheckUpdates).await?;
    let e = engine(core).await?;
    let (_, all) = all_sources(core, &e).await?;
    let cx = ctx(core, &e, *actor != Actor::User).await;
    let mut behind = 0;
    for p in core.store().provenance_latest_all().await? {
        let Some(src) = sources::find(&all, &p.source) else {
            continue;
        };
        let Ok(loc) = serde_json::from_str::<Locator>(&p.locator) else {
            continue;
        };
        let latest = src.latest(&cx, &unpinned(&loc)).await;
        let now = now_ms();
        let (state, commit, version) = match &latest {
            Ok(Some(up)) => {
                let same = match (&up.commit, &up.version) {
                    (Some(c), _) if !p.upstream_commit.is_empty() => *c == p.upstream_commit,
                    (_, Some(v)) if !p.upstream_version.is_empty() => *v == p.upstream_version,
                    _ => !up.sha256.is_empty() && up.sha256 == p.upstream_sha256,
                };
                (
                    if same { "current" } else { "behind" },
                    up.commit.clone().unwrap_or_default(),
                    up.version.clone().unwrap_or_default(),
                )
            }
            Ok(None) => ("gone", String::new(), String::new()),
            Err(err) if fetch::is_stopped(err) => {
                let _ = refuse(
                    core,
                    actor,
                    &Action::CheckUpdates,
                    "kill_switch",
                    "in flight",
                )
                .await;
                break;
            }
            // Offline or limited: leave the row as it was.
            Err(err) if fetch::is_offline(err) || fetch::is_limited(err).is_some() => continue,
            Err(_) => ("error", String::new(), String::new()),
        };
        core.store()
            .save_update_state(&UpdateState {
                item_kind: p.item_kind.clone(),
                item_id: p.item_id.clone(),
                latest_commit: commit.clone(),
                latest_version: version.clone(),
                latest_sha256: latest
                    .as_ref()
                    .ok()
                    .and_then(|u| u.as_ref())
                    .map(|u| u.sha256.clone())
                    .unwrap_or_default(),
                checked_at: now,
                state: state.into(),
            })
            .await?;
        core.emit(
            kinds::LIBRARY_UPDATE,
            &p.item_id,
            &json!({"kind": p.item_kind, "id": p.item_id, "state": state}),
        )
        .await?;
        if state == "behind" || state == "gone" {
            if state == "behind" {
                behind += 1;
            }
            let latest_ref = if commit.is_empty() { &version } else { &commit };
            ledger(
                core,
                "library.update_checked",
                actor,
                format!(
                    "library.update_checked {} {} {state}",
                    p.item_kind, p.item_id
                ),
                json!({"kind": p.item_kind, "id": p.item_id, "version": p.version}),
                json!({"outcome": state, "latest": latest_ref, "source": p.source}),
                format!(
                    "libsearch:update_checked:{}:{}:{latest_ref}",
                    p.item_id, p.version
                ),
            )
            .await?;
        }
    }
    Ok(behind)
}

/// A git locator without its pinned commit (so `latest` resolves HEAD or
/// the tap's ref).
fn unpinned(loc: &Locator) -> Locator {
    match loc {
        Locator::GitHub {
            owner, repo, path, ..
        } => Locator::GitHub {
            owner: owner.clone(),
            repo: repo.clone(),
            git_ref: None,
            path: path.clone(),
        },
        Locator::ClawHub { owner, slug, .. } => Locator::ClawHub {
            owner: owner.clone(),
            slug: slug.clone(),
            version: None,
        },
        l => l.clone(),
    }
}

/// Starts an update of an imported item: a fresh preview at the latest
/// upstream with the diff base set to the imported version. The person
/// reviews and imports it as usual (ledger `library.updated`).
pub async fn update_preview(core: &Core, item_kind: &str, item_id: &str) -> Result<Preview> {
    guard(core, Action::Update(item_id)).await?;
    let prov = core
        .store()
        .latest_provenance(item_kind, item_id)
        .await?
        .ok_or_else(|| anyhow!("this item was not imported from a source"))?;
    let loc: Locator = serde_json::from_str(&prov.locator)?;
    let item_type = ItemType::parse(item_kind).unwrap_or(ItemType::Skill);
    let hit = Hit {
        source: prov.source.clone(),
        locator: unpinned(&loc),
        item_type,
        ecosystem: Ecosystem::parse(&prov.ecosystem).unwrap_or(Ecosystem::Generic),
        name: String::new(),
        title: String::new(),
        description: String::new(),
        tags: vec![],
        licence: kls_libsearch::Licence::from_id(&prov.licence, "provenance"),
        trust: kls_libsearch::Trust::Community,
        popularity: None,
        updated_at: None,
        flags: vec![],
    };
    let mut p = preview(core, &hit).await?;
    // The diff is against the imported version's body.
    let base = match item_kind {
        "agent" => core
            .store()
            .agent_profile(item_id)
            .await?
            .map(|a| (a.id.clone(), a.version, a.prompt.clone())),
        _ => match core.store().library_item(item_id).await? {
            Some(it) => {
                let vs = core.store().library_versions(&it.id).await?;
                let v = vs
                    .iter()
                    .find(|v| v.version == prov.version)
                    .or(vs.first())
                    .cloned()
                    .unwrap_or_default();
                Some((it.id, v.version, v.body))
            }
            None => None,
        },
    };
    if base.is_some() {
        p.existing = base;
    }
    // Keep the imported name so the update lands on the same item.
    if let Some((id, _, _)) = &p.existing {
        let name = match item_kind {
            "agent" => core.store().agent_profile(id).await?.map(|a| a.name),
            _ => core.store().library_item(id).await?.map(|i| i.name),
        };
        if let Some(n) = name {
            p.converted.name = n;
        }
    }
    p.update_of = Some(prov);
    Ok(p)
}

/// The provenance of an item's latest version, for origin chips.
pub async fn origin(core: &Core, item_kind: &str, item_id: &str) -> Result<Option<Provenance>> {
    core.store().latest_provenance(item_kind, item_id).await
}

/// Items-tab rows: provenance and update state per imported item.
pub async fn origins(core: &Core) -> Result<Vec<(Provenance, Option<UpdateState>)>> {
    let ups = core.store().update_states().await?;
    Ok(core
        .store()
        .provenance_latest_all()
        .await?
        .into_iter()
        .map(|p| {
            let u = ups
                .iter()
                .find(|u| u.item_kind == p.item_kind && u.item_id == p.item_id)
                .cloned();
            (p, u)
        })
        .collect())
}

// ---- node conversion (WP9) --------------------------------------------------------

/// The job kind a belai node advertises in `Capabilities.job_kinds` when it
/// converts agent definitions (belai `internal/libraryjob`).
pub const LIBRARY_JOB_KIND: &str = "library";

/// The formats only belai's `agentimport` reads.
pub const NODE_FORMATS: [&str; 4] = ["hermes", "claws", "nemoclaw", "mini-swe"];

/// The note field that records who converted a preview (provenance
/// `converted_by`).
const CONVERTED_BY: &str = "converted_by";

/// Who converted the item: `node:<id>` for a node conversion, else `kls`.
fn converted_by(c: &Converted) -> String {
    c.notes
        .iter()
        .find(|n| n.field == CONVERTED_BY && n.text.starts_with("node:"))
        .map(|n| n.text.clone())
        .unwrap_or_else(|| "kls".into())
}

/// The belai format an agent preview needs a node to convert (the Rust
/// fallback reads only part of it), or `None`: Hermes and OpenClaw by their
/// ecosystem, NemoClaw and mini-SWE by their files. A preview a node already
/// converted needs nothing more.
pub fn node_format(p: &Preview) -> Option<&'static str> {
    if p.converted.target != Target::Agent || converted_by(&p.converted) != "kls" {
        return None;
    }
    match p.hit.ecosystem {
        Ecosystem::Hermes => return Some("hermes"),
        Ecosystem::OpenClaw => return Some("claws"),
        _ => {}
    }
    let has = |needles: &[&str]| {
        p.fetched.files.keys().any(|k| {
            let k = k.to_ascii_lowercase();
            needles.iter().any(|n| k.contains(n))
        })
    };
    if has(&["nemoclaw"]) {
        Some("nemoclaw")
    } else if has(&["mini-swe", "mini_swe", "minisweagent"]) {
        Some("mini-swe")
    } else {
        None
    }
}

/// An online, unrevoked node that advertises the `library` job, by name.
async fn library_node(core: &Core) -> Result<(String, String)> {
    let online = core.gateway()?.online().await;
    let mut nodes: Vec<(String, String)> = core
        .store()
        .nodes()
        .await?
        .into_iter()
        .filter(|n| {
            n.revoked_at.is_none()
                && online.contains(&n.id)
                && n.host
                    .capabilities
                    .job_kinds
                    .iter()
                    .any(|k| k == LIBRARY_JOB_KIND)
        })
        .map(|n| (n.name, n.id))
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .next()
        .map(|(name, id)| (id, name))
        .ok_or_else(|| anyhow!("no online node runs the belai `library` job"))
}

/// Converts an agent preview's fetched files on a belai node with belai's
/// own `agentimport` (Hermes, Claws/OpenClaw, Nemoclaw, mini-SWE), and
/// returns the preview with belai's profile, skills and notes in place of the
/// Rust fallback's. Nothing is saved: the person reviews and imports the
/// result like any preview, and the import's provenance records
/// `converted_by = node:<id>`. Refused (and audited) under the kill switch,
/// for a non-agent preview or an unknown format, and when no online node
/// advertises the job; a node's refusal is audited too. The conversion is
/// chained as `library.converted` with origin `host:<node>`.
pub async fn node_convert(core: &Core, p: &Preview, format: &str) -> Result<Preview> {
    use base64::Engine as _;
    let a = Action::NodeConvert;
    guard(core, a.clone()).await?;
    if !NODE_FORMATS.contains(&format) {
        return Err(refuse(core, &Actor::User, &a, "invalid:format", format).await);
    }
    if p.converted.target != Target::Agent {
        return Err(refuse(core, &Actor::User, &a, "invalid:kind", "not an agent").await);
    }
    let (node, node_name) = match library_node(core).await {
        Ok(n) => n,
        Err(e) => return Err(refuse(core, &Actor::User, &a, "no_node", &e.to_string()).await),
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let files: BTreeMap<&str, String> = p
        .fetched
        .files
        .iter()
        .map(|(path, body)| (path.as_str(), b64.encode(body)))
        .collect();
    let payload = json!({
        "task": "convert", "kind": "agent", "format": format,
        "name": p.converted.name, "files": files,
    });
    let out = core
        .gateway()?
        .job(
            Some(&node),
            LIBRARY_JOB_KIND,
            &payload.to_string(),
            crate::JobNeeds::default(),
            120_000,
        )
        .await;
    let out = match out {
        Ok(o) if o.status == "ok" => o,
        Ok(o) => {
            let why = format!("{}: {}", o.status, o.error);
            return Err(refuse(core, &Actor::User, &a, "node_refused", &why).await);
        }
        Err(e) => {
            return Err(refuse(core, &Actor::User, &a, "node_failed", &format!("{e:#}")).await);
        }
    };
    #[derive(Deserialize)]
    struct NodeNote {
        kind: String,
        field: String,
        text: String,
    }
    #[derive(Deserialize)]
    struct NodeSkill {
        doc: String,
    }
    #[derive(Deserialize)]
    struct NodeResult {
        doc: String,
        sha256: String,
        #[serde(default)]
        notes: Vec<NodeNote>,
        #[serde(default)]
        skills: Vec<NodeSkill>,
    }
    let r: NodeResult = serde_json::from_str(&out.output_json)
        .map_err(|e| anyhow!("node {node_name}: bad library job answer: {e}"))?;
    // belai's profile is validated here as any edited profile is.
    let mut c = match conv::from_edited(&p.converted, &r.doc) {
        Ok(c) => c,
        Err(e) => {
            return Err(refuse(core, &Actor::User, &a, "invalid:node_doc", &e.to_string()).await);
        }
    };
    let mut skills = Vec::new();
    for s in &r.skills {
        let base = Converted {
            target: Target::Library { kind: "skill" },
            profile: None,
            extra_skills: vec![],
            files: vec![],
            ..p.converted.clone()
        };
        match conv::from_edited(&base, &s.doc) {
            Ok(sk) => skills.push(sk),
            Err(e) => tracing::warn!("node {node_name}: skill dropped: {e:#}"),
        }
    }
    c.extra_skills = skills;
    let note_kind = |k: &str| match k {
        "mapped" => conv::NoteKind::Mapped,
        "metadata" => conv::NoteKind::Metadata,
        "dropped" => conv::NoteKind::Dropped,
        _ => conv::NoteKind::Warning,
    };
    c.notes = r
        .notes
        .iter()
        .take(200)
        .map(|n| {
            conv::Note::new(
                note_kind(&n.kind),
                &n.field.chars().take(80).collect::<String>(),
                n.text.chars().take(500).collect::<String>(),
            )
        })
        .collect();
    c.notes.push(conv::Note::new(
        conv::NoteKind::Metadata,
        CONVERTED_BY,
        format!("node:{node}"),
    ));
    let e = core
        .audit_record(LedgerDraft {
            kind: "library.converted".into(),
            actor: Actor::Node(node.clone()).as_str(),
            actor_kind: "host".into(),
            msg: format!("library.converted agent {} on node {node_name}", c.name),
            subject: json!({ "kind": "agent", "name": c.name }),
            payload: json!({
                "format": format, "node": node, "doc_sha256": c.sha256,
                "node_sha256": r.sha256, "upstream_sha256": c.upstream.sha256,
                "skills": c.extra_skills.len(), "requested_by": "user",
            }),
            origin: format!("host:{node}"),
            node_id: node.clone(),
            ref_key: Some(format!(
                "libsearch:converted:{node}:{}",
                uuid::Uuid::now_v7()
            )),
            ..Default::default()
        })
        .await?;
    tracing::debug!("library.converted seq {:?}", e.map(|e| e.seq));
    Ok(Preview {
        converted: c,
        ..p.clone()
    })
}

#[cfg(test)]
mod tests;
