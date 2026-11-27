//! Domain: host management (docs/gaps/host-management.md). Persistence is
//! `kls_store::host`; the wire is `proto/keleustes/node/v1/host.proto`
//! (`kls-node/src/host.rs` converts).
//!
//! Every host action goes through [`request_action`], which checks, in
//! order: the kind is known, the origin is the console (never an agent or a
//! peer node), the kill switch, the A2A blackout, the node is online, the
//! host offers the kind, the host gate (`manage_level`) is high enough, the
//! per-class policy (typed confirmation, current preview, `if_match`). Every
//! outcome, refusals included, is stored as a `host_actions` row, emitted as
//! a `host.action.*` event and chained in the audit ledger with its origin.
//!
//! Decision O3: while the factory is paused or stopped, a read runs only when
//! a person clicked it in the Host view, and is audited with the control
//! state; every change and every automatic action is refused and audited.

use crate::Core;
use crate::control;
use anyhow::{Result, anyhow};
use kls_store::audit::LedgerDraft;
use kls_store::host::{HostActionPatch, HostActionRow, HostPlanRow, HostPlanRunRow, PatchRefusal};
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub use kls_store::host::{HostActionLine, HostInventoryRow};

/// Event kinds of this area.
pub mod kinds {
    /// A host reported an inventory that differs from its last one.
    pub const INVENTORY: &str = "host.inventory";
    pub const ACTION_REQUESTED: &str = "host.action.requested";
    pub const ACTION_REFUSED: &str = "host.action.refused";
    pub const ACTION_QUEUED: &str = "host.action.queued";
    pub const ACTION_ACCEPTED: &str = "host.action.accepted";
    /// Progress lines or a state change short of settling.
    pub const ACTION_PROGRESS: &str = "host.action.progress";
    pub const ACTION_NEEDS_HUMAN: &str = "host.action.needs_human";
    /// Settled: `data.state` is done, failed or cancelled.
    pub const ACTION_FINISHED: &str = "host.action.finished";
    /// A restart that expected a belai version came back on another one
    /// (B5). Data: `{actionId, expected, running}`.
    pub const VERSION_MISMATCH: &str = "host.action.version_mismatch";
    pub const PLAN_SAVED: &str = "host.plan.saved";
    pub const PLAN_DELETED: &str = "host.plan.deleted";
    pub const PLAN_STARTED: &str = "host.plan.started";
    pub const PLAN_FINISHED: &str = "host.plan.finished";
    /// A schedule or library change was refused. Data: `{event, code, origin}`.
    pub const CHANGE_REFUSED: &str = "host.change.refused";
    /// What the Host view reloads on.
    pub const PREFIXES: [&str; 1] = ["host."];
}

/// How long a dry run's preview stays confirmable.
pub const PREVIEW_TTL_MS: i64 = 30 * 60_000;
/// TTL of the vault grant a `product_auth` action carries.
pub const AUTH_GRANT_TTL_MS: i64 = 5 * 60_000;
/// The vault provider slug whose secrets `product_auth` lends: an ApiKey
/// (`VULNETIX_API_KEY`, `VULNETIX_ORG_ID`) or a SigV4 secret (`VVD_ORG`,
/// `VVD_SECRET`).
pub const VULNETIX_VAULT_PROVIDER: &str = "vulnetix";
/// Longest `Origin.reason` kept.
const MAX_REASON: usize = 200;
/// How long a plan step may run before the plan gives up on it.
const STEP_TIMEOUT_MS: i64 = 30 * 60_000;
/// How often an in-flight plan step re-reads the kill switch (another
/// process may flip it without an event reaching this bus).
const PLAN_CONTROL_POLL: std::time::Duration = std::time::Duration::from_millis(250);
/// How often a plan held by a maintenance window retries.
const PLAN_HOLD_POLL: std::time::Duration = std::time::Duration::from_secs(5);

// ---- classes and kinds -----------------------------------------------------------

/// Action class (§6.1). Joined with the host gate by AND.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Read,
    Config,
    Provision,
    Shell,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Read => "read",
            Class::Config => "config",
            Class::Provision => "provision",
            Class::Shell => "shell",
        }
    }

    /// Whether the action changes the host.
    pub fn mutating(self) -> bool {
        self != Class::Read
    }

    /// The lowest host gate that admits the class.
    fn needs_level(self) -> Level {
        match self {
            Class::Read => Level::Read,
            Class::Config => Level::Config,
            Class::Provision | Class::Shell => Level::Provision,
        }
    }
}

/// Host gate (`--keleustes-manage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off,
    Read,
    Config,
    Provision,
}

impl Level {
    pub fn parse(s: &str) -> Level {
        match s {
            "read" => Level::Read,
            "config" => Level::Config,
            "provision" => Level::Provision,
            _ => Level::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Read => "read",
            Level::Config => "config",
            Level::Provision => "provision",
        }
    }
}

/// The closed list of host action kinds and their class.
pub const KINDS: [(&str, Class); 21] = [
    ("inventory_refresh", Class::Read),
    ("product_probe", Class::Read),
    ("config_get", Class::Read),
    ("preflight", Class::Read),
    ("log_tail", Class::Read),
    ("config_set", Class::Config),
    ("project_prefs_set", Class::Config),
    ("repo_trust", Class::Config),
    ("repo_untrust", Class::Config),
    ("worktree_drop", Class::Config),
    ("session_prune", Class::Config),
    ("agents_set", Class::Config),
    ("product_install", Class::Provision),
    ("product_upgrade", Class::Provision),
    ("product_uninstall", Class::Provision),
    ("product_configure", Class::Provision),
    ("product_auth", Class::Provision),
    ("belai_upgrade", Class::Provision),
    ("rc_restart", Class::Provision),
    ("repo_clone", Class::Provision),
    ("shell_exec", Class::Shell),
];

pub fn class_of(kind: &str) -> Option<Class> {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, c)| *c)
}

/// Whether `kind` names a host action. `Fleet.Request` (agents and peer
/// nodes) refuses every one of these (rule 4: no agent manages a host).
pub fn is_host_kind(kind: &str) -> bool {
    kind.starts_with("host_") || class_of(kind).is_some()
}

/// Products the Products tab knows.
pub const PRODUCTS: [&str; 4] = ["cli", "package_firewall", "ai_firewall", "sensor"];

// ---- inventory (mirror of host.proto HostInventory) --------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inventory {
    pub at_ms: i64,
    pub belai: BelaiFacts,
    pub products: Vec<ProductStatus>,
    pub repos: Vec<HostRepo>,
    pub tools: Vec<ToolFact>,
    pub preflight: Vec<Check>,
    pub limits: HostLimits,
    pub gates: RcGates,
    pub disk: DiskUsage,
    pub settings_sha256: String,
    pub plugins: Vec<String>,
    pub mcp: Vec<McpServerFact>,
    pub providers: Vec<ProviderFact>,
    pub sandbox: SandboxFact,
    pub manage: HostManage,
    pub schedules: Vec<HostSchedule>,
    pub builtin_agents: Vec<String>,
    pub agents_enabled: bool,
    pub worktrees: Vec<WorktreeFact>,
    /// Keleustes's own facts about the link (never from the wire).
    pub link: LinkFacts,
}

/// HM-13: what the server saw when the inventory arrived. Not on the wire;
/// the node cannot set these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkFacts {
    /// Server clock when the report arrived.
    pub received_at_ms: i64,
    /// The node's client certificate expiry, from the TLS handshake.
    pub cert_not_after_ms: i64,
}

impl Inventory {
    /// Host clock minus server clock at the report (latency included).
    pub fn clock_skew_ms(&self) -> Option<i64> {
        (self.at_ms > 0 && self.link.received_at_ms > 0)
            .then(|| self.at_ms - self.link.received_at_ms)
    }

    /// Time left on the node's certificate at `now` (negative: expired).
    pub fn cert_left_ms(&self, now: i64) -> Option<i64> {
        (self.link.cert_not_after_ms > 0).then(|| self.link.cert_not_after_ms - now)
    }
}

/// The fastest action round trip among recent settled actions (delivery to
/// the last update): an upper bound on the link's rtt.
pub fn link_rtt_ms(actions: &[HostActionRow]) -> Option<i64> {
    actions
        .iter()
        .filter(|a| a.settled())
        .filter_map(|a| Some(a.finished_at? - a.delivered_at?))
        .filter(|d| *d >= 0)
        .min()
}

/// Removes terminal escapes, control and bidi characters, and masks
/// credential-shaped words from a host's progress line. The host scrubs
/// with vaultenv first; this is the console's own pass before storing.
pub fn scrub_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.peek() {
                // CSI: parameters, then one final byte in @..~.
                Some('[') => {
                    it.next();
                    for d in it.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    it.next();
                    while let Some(d) = it.next() {
                        if d == '\u{7}' || (d == '\u{1b}' && it.peek() == Some(&'\\')) {
                            if d == '\u{1b}' {
                                it.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
            continue;
        }
        let bidi = matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if bidi || (c.is_control() && c != '\t') {
            continue;
        }
        out.push(c);
    }
    out.split(' ')
        .map(|w| if secret_shaped(w) { "[redacted]" } else { w })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A word that looks like a credential: a known token prefix, or a
/// `key=value` whose key names a secret.
fn secret_shaped(w: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "AKIA",
        "glpat-",
    ];
    if PREFIXES
        .iter()
        .any(|p| w.starts_with(p) && w.len() >= p.len() + 12)
    {
        return true;
    }
    match w.split_once('=') {
        Some((k, v)) if !v.is_empty() => {
            let k = k.trim_start_matches('-').to_ascii_lowercase();
            ["token", "secret", "password", "api_key", "apikey", "key"]
                .iter()
                .any(|s| k == *s || k.ends_with(&format!("_{s}")))
        }
        _ => false,
    }
}

impl Inventory {
    pub fn product(&self, id: &str) -> Option<&ProductStatus> {
        self.products.iter().find(|p| p.product == id)
    }

    /// Whether the host executes `kind`.
    pub fn offers(&self, kind: &str) -> bool {
        self.manage.host_actions.iter().any(|k| k == kind)
    }

    pub fn level(&self) -> Level {
        Level::parse(&self.manage.manage_level)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostManage {
    pub host_actions: Vec<String>,
    pub manage_level: String,
    pub manage_shell: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BelaiFacts {
    pub version: String,
    pub install_method: String,
    pub latest: String,
    pub update_available: bool,
    pub root: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProductStatus {
    pub product: String,
    pub state: String,
    pub version: String,
    pub latest: String,
    pub path: String,
    pub install_method: String,
    pub authenticated: bool,
    pub org_id: String,
    pub auth_source: String,
    pub running: bool,
    pub enabled: bool,
    pub mode: String,
    pub parts: Vec<Toggle>,
    pub checks: Vec<Check>,
    pub probed_at_ms: i64,
    pub degraded: Vec<String>,
    pub units: Vec<String>,
    pub entitlement: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Toggle {
    pub id: String,
    pub label: String,
    pub scope: String,
    pub on: bool,
    pub path: String,
    pub state: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    pub id: String,
    pub status: String,
    pub hint: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitFacts {
    pub commit: String,
    pub dirty: bool,
    pub ahead: i32,
    pub behind: i32,
    pub detached: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostRepo {
    pub path: String,
    pub remote: String,
    pub forge_host: String,
    pub owner: String,
    pub name: String,
    pub branch: String,
    pub git: Option<GitFacts>,
    pub trusted: bool,
    pub registered: bool,
    pub last_used_ms: i64,
    pub detections: RepoDetections,
    pub agents: Vec<Toggle>,
    pub prefs: BTreeMap<String, String>,
}

impl HostRepo {
    /// `keleustes://repos/<owner>/<name>`, when the remote parsed.
    pub fn link(&self) -> Option<String> {
        repo_link(&self.owner, &self.name)
    }
}

/// The Repos screen deep link for a repository.
pub fn repo_link(owner: &str, name: &str) -> Option<String> {
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (ok(owner) && ok(name)).then(|| format!("keleustes://repos/{owner}/{name}"))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoDetections {
    pub build: Vec<String>,
    pub lint: Vec<String>,
    pub test: Vec<String>,
    pub languages: Vec<String>,
    pub scans: Vec<String>,
    pub dep_watch: bool,
    pub in_sweep: bool,
    pub last_scan_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolFact {
    pub name: String,
    pub version: String,
    pub ok: bool,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostLimits {
    pub max_sessions: u32,
    pub max_workers: u32,
    pub reserved_workers: u32,
    pub max_jobs: u32,
    pub idle_s: u32,
    pub sessions: u32,
    pub workers: u32,
    pub jobs: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RcGates {
    pub web_controls: bool,
    pub web_shell: bool,
    pub web_project_settings: bool,
    pub allow_guardrails_off: bool,
    pub no_clone: bool,
    pub allow_private_cidr: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskUsage {
    pub sessions: i64,
    pub worktrees: i64,
    pub logs: i64,
    pub free: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerFact {
    pub name: String,
    pub transport: String,
    pub builtin: bool,
    pub keys_bound: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderFact {
    pub provider: String,
    pub key_present: bool,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxFact {
    pub mode: String,
    pub network: String,
    pub backend: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSchedule {
    pub id: String,
    pub target: String,
    pub cron: String,
    pub tz: String,
    pub dir: String,
    pub enabled: bool,
    pub last_run_ms: i64,
    pub last_status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreeFact {
    pub path: String,
    pub repo: String,
    pub branch: String,
    pub bytes: i64,
    pub stale: bool,
}

// ---- origin, requests, refusals ---------------------------------------------------------

/// Who or what asked (rule 1). Goes to the node with the action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Origin {
    /// "owner" (the desktop owner).
    pub actor: String,
    /// "fleet.host/<tab>", "plan:<id>", "workflow:<id>".
    pub surface: String,
    pub run_id: String,
    pub workflow_id: String,
    pub plan_id: String,
    /// Typed by the person; audit only.
    pub reason: String,
}

impl Origin {
    /// The owner clicking in the Host view's `tab`.
    pub fn owner(tab: &str) -> Self {
        Origin {
            actor: "owner".into(),
            surface: format!("fleet.host/{tab}"),
            ..Default::default()
        }
    }

    /// The owner clicking in a Fleet section (the cross-host rows).
    pub fn fleet(section: &str) -> Self {
        Origin {
            actor: "owner".into(),
            surface: format!("fleet/{section}"),
            ..Default::default()
        }
    }

    /// An explicit click by a person (no plan, run or workflow behind it).
    pub fn is_click(&self) -> bool {
        self.actor == "owner"
            && (self.surface.starts_with("fleet.host/") || self.surface.starts_with("fleet/"))
            && self.run_id.is_empty()
            && self.workflow_id.is_empty()
            && self.plan_id.is_empty()
    }

    /// The console (a person, their plan or their workflow): the only
    /// sources a host action may come from.
    fn is_console(&self) -> bool {
        self.actor == "owner"
            && (self.surface.starts_with("fleet.host/")
                || self.surface.starts_with("fleet/")
                || self.surface.starts_with("plan:")
                || self.surface.starts_with("workflow:")
                || self.surface.starts_with("teleport:")
                // The owner's automatic library sync (#20), on unless
                // `library.auto_sync` is off.
                || self.surface == "library.sync")
    }

    /// One-line form for refusal events.
    pub fn label(&self) -> String {
        format!("{}@{}", self.actor, self.surface)
    }
}

/// What the console asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionRequest {
    pub node_id: String,
    pub kind: String,
    pub params: BTreeMap<String, String>,
    pub dry_run: bool,
    /// Non-dry provision/shell: the preview the person confirmed.
    pub preview_sha256: String,
    /// Non-dry provision/shell: the host name the person typed.
    pub confirm_name: String,
    /// `config_set`: the settings hash the edit was made against.
    pub if_match: String,
    pub origin: Origin,
    pub plan_run_id: String,
    pub step: i64,
}

impl ActionRequest {
    pub fn new(node_id: &str, kind: &str, origin: Origin) -> Self {
        ActionRequest {
            node_id: node_id.into(),
            kind: kind.into(),
            origin,
            ..Default::default()
        }
    }

    pub fn param(mut self, k: &str, v: &str) -> Self {
        self.params.insert(k.into(), v.into());
        self
    }

    pub fn dry(mut self) -> Self {
        self.dry_run = true;
        self
    }
}

/// Why Keleustes refused a host action. Carried in `anyhow::Error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnknownKind,
    /// Not from the console (an agent, a peer node, an automatic source).
    NotConsole,
    /// The kill switch: "paused" or "stopped".
    KillSwitch(String),
    A2aBlackout,
    Offline,
    /// The host does not list the kind (older belai, or not built in).
    NotOffered,
    /// The host gate is below what the class needs.
    Gate {
        need: String,
        have: String,
    },
    ShellOff,
    /// The typed host name did not match.
    Confirm,
    /// No current preview with this hash.
    PreviewStale,
    /// `config_set` against an old settings hash.
    SettingsChanged,
    Invalid(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::UnknownKind => write!(f, "unknown host action"),
            Refusal::NotConsole => write!(f, "host actions come from the console only"),
            Refusal::KillSwitch(m) => write!(f, "the factory is {m}: host action refused"),
            Refusal::A2aBlackout => write!(f, "A2A blackout: refused"),
            Refusal::Offline => write!(f, "host offline"),
            Refusal::NotOffered => write!(f, "the host does not offer this action"),
            Refusal::Gate { need, have } => write!(f, "host gate: {have} (needs {need})"),
            Refusal::ShellOff => write!(f, "shell is off on the host"),
            Refusal::Confirm => write!(f, "type the host name to confirm"),
            Refusal::PreviewStale => write!(f, "preview stale: preview again"),
            Refusal::SettingsChanged => write!(f, "settings changed on host: reload"),
            Refusal::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Refusal {}

impl Refusal {
    /// Short machine tag for events and the ledger.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::UnknownKind => "unknown_kind",
            Refusal::NotConsole => "not_console",
            Refusal::KillSwitch(_) => "kill_switch",
            Refusal::A2aBlackout => "a2a_blackout",
            Refusal::Offline => "offline",
            Refusal::NotOffered => "not_offered",
            Refusal::Gate { .. } => "host_gate",
            Refusal::ShellOff => "shell_off",
            Refusal::Confirm => "confirm",
            Refusal::PreviewStale => "preview_stale",
            Refusal::SettingsChanged => "settings_changed",
            Refusal::Invalid(_) => "invalid",
        }
    }
}

pub fn refusal_of(e: &anyhow::Error) -> Option<&Refusal> {
    e.downcast_ref::<Refusal>()
}

/// What a needs-human update asks a person to do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Handoff {
    pub command: String,
    /// "needs_root", "needs_browser".
    pub reason: String,
    /// Set once the person clicked Done and a re-probe was asked for.
    pub check_action: String,
}

impl HostActionRowExt for HostActionRow {
    fn origin(&self) -> Origin {
        serde_json::from_str(&self.origin_json).unwrap_or_default()
    }
    fn params(&self) -> BTreeMap<String, String> {
        serde_json::from_str(&self.params_json).unwrap_or_default()
    }
    fn handoff(&self) -> Option<Handoff> {
        serde_json::from_str(&self.handoff_json).ok()
    }
    fn class_enum(&self) -> Option<Class> {
        class_of(&self.kind)
    }
}

/// Typed accessors over the stored JSON columns.
pub trait HostActionRowExt {
    fn origin(&self) -> Origin;
    fn params(&self) -> BTreeMap<String, String>;
    fn handoff(&self) -> Option<Handoff>;
    fn class_enum(&self) -> Option<Class>;
}

// ---- helpers ----------------------------------------------------------------------

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(b))
}

/// Digest of the params as sent (sorted keys).
pub fn params_digest(params: &BTreeMap<String, String>) -> String {
    sha256_hex(serde_json::to_string(params).unwrap_or_default().as_bytes())
}

fn clean_origin(mut o: Origin) -> Origin {
    o.reason = o
        .reason
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON)
        .collect();
    o
}

/// The OpenTelemetry keys a console sets on a host with `config_set`
/// (belai docs/telemetry.md, row 50), besides `telemetry.headers.<Name>`.
pub const TELEMETRY_KEYS: [&str; 3] = [
    "telemetry.otlp_endpoint",
    "telemetry.traces",
    "telemetry.metrics",
];

/// Checks a `telemetry.*` `config_set` before it is queued (the host checks
/// again). The endpoint is an `http(s)://host[:port][/path]` URL with no
/// credentials, query or fragment, or empty (export off). A header names a
/// host environment variable (`env:NAME`) or is empty (removed): a secret
/// value never goes through Keleustes or into the host's settings file.
/// Keys outside `telemetry.` pass unchanged.
pub fn telemetry_value(key: &str, value: &str) -> std::result::Result<(), &'static str> {
    let Some(rest) = key.strip_prefix("telemetry.") else {
        return Ok(());
    };
    if let Some(name) = rest.strip_prefix("headers.") {
        let name_ok = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !name_ok {
            return Err("a telemetry header name takes letters, digits and '-'");
        }
        let env_ok = value.is_empty()
            || value.strip_prefix("env:").is_some_and(|n| {
                !n.is_empty()
                    && n.len() <= 64
                    && !n.starts_with(|c: char| c.is_ascii_digit())
                    && n.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            });
        if !env_ok {
            return Err("a telemetry header names a host environment variable (env:NAME)");
        }
        return Ok(());
    }
    match rest {
        "traces" | "metrics" => {
            if matches!(value, "true" | "false") {
                Ok(())
            } else {
                Err("takes true or false")
            }
        }
        "otlp_endpoint" => {
            if value.is_empty() {
                return Ok(());
            }
            let after = value
                .strip_prefix("https://")
                .or_else(|| value.strip_prefix("http://"))
                .ok_or("the endpoint is an http:// or https:// URL")?;
            let host = after.split('/').next().unwrap_or("");
            let ok = value.len() <= 128
                && !host.is_empty()
                && !value.contains(['@', '?', '#', ' ', '\\'])
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
                && after.chars().all(|c| c.is_ascii_graphic());
            if ok {
                Ok(())
            } else {
                Err("the endpoint is a collector URL without credentials, query or fragment")
            }
        }
        _ => Err("not a telemetry key Keleustes sets"),
    }
}

/// The factory-wide OTLP endpoint (row 50): a `config_set` of
/// `telemetry.otlp_endpoint` to every node that offers it, each through
/// [`request_action`] (kill switch, gate, if-match, audit). Returns each
/// node's queued action id or its refusal.
pub async fn telemetry_everywhere(
    core: &Core,
    endpoint: &str,
    origin: Origin,
) -> Result<Vec<(String, std::result::Result<String, String>)>> {
    telemetry_value("telemetry.otlp_endpoint", endpoint).map_err(|e| anyhow::anyhow!(e))?;
    let mut out = Vec::new();
    for n in core.store().nodes().await? {
        if n.revoked_at.is_some() {
            continue;
        }
        let Some((_, inv)) = inventory(core, &n.id).await? else {
            continue;
        };
        if !inv.offers("config_set") {
            continue;
        }
        let mut req = ActionRequest::new(&n.id, "config_set", origin.clone())
            .param("key", "telemetry.otlp_endpoint")
            .param("value", endpoint);
        req.if_match = inv.settings_sha256.clone();
        let r = request_action(core, req)
            .await
            .map(|row| row.id)
            .map_err(|e| format!("{e:#}"));
        out.push((n.id, r));
    }
    Ok(out)
}

/// Whether the action touches what the A2A blackout freezes: A2A settings,
/// remote sync of prompts and hooks, and installs of agents or MCP servers.
pub fn touches_a2a(kind: &str, params: &BTreeMap<String, String>) -> bool {
    let key = params.get("key").map(String::as_str).unwrap_or("");
    match kind {
        "config_set" | "project_prefs_set" => {
            key.starts_with("a2a.") || key.starts_with("sync.remote_") || key == "sync.hooks"
        }
        "library_install" => matches!(
            params.get("kind").map(String::as_str),
            Some("profile" | "crew" | "agent" | "mcp")
        ),
        _ => false,
    }
}

/// The last inventory a node reported.
pub async fn inventory(core: &Core, node_id: &str) -> Result<Option<(i64, Inventory)>> {
    Ok(core
        .store()
        .host_inventory(node_id)
        .await?
        .map(|r| (r.at, serde_json::from_str(&r.json).unwrap_or_default())))
}

/// Whether the node has an open link. A link this process does not hold (a
/// `keleustes host` command beside the running app, whose own hub has no
/// links) counts by its last heartbeat; the app's action stream delivers
/// what is queued.
async fn online(core: &Core, node_id: &str) -> bool {
    if let Ok(g) = core.gateway()
        && g.online().await.iter().any(|n| n == node_id)
    {
        return true;
    }
    core.store()
        .node(node_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|n| {
            n.revoked_at.is_none()
                && n.last_seen_at
                    .is_some_and(|t| now_ms() - t < ONLINE_GRACE_MS)
        })
}

/// How recent a heartbeat counts as online for [`online`]'s fallback
/// (three 15 s heartbeats).
const ONLINE_GRACE_MS: i64 = 45_000;

/// Chains a ledger entry. The ledger append reads the chain head and then
/// writes, so on SQLite a concurrent writer (the action stream marking a
/// delivery) can fail it with a stale snapshot; it is retried, since an
/// unrecorded action would break rule 2.
async fn record(core: &Core, d: LedgerDraft) -> Result<()> {
    let mut wait = 10;
    for _ in 0..5 {
        match core.audit_record(d.clone()).await {
            Ok(_) => return Ok(()),
            Err(e) if format!("{e:#}").contains("database is locked") => {
                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                wait *= 3;
            }
            Err(e) => return Err(e),
        }
    }
    core.audit_record(d).await.map(|_| ())
}

/// Chains one ledger entry for an action event (rule 2).
async fn audit(
    core: &Core,
    row: &HostActionRow,
    event: &str,
    actor_kind: &str,
    outcome: &str,
    reason: &str,
    extra: Value,
) -> Result<()> {
    let origin = row.origin();
    let params = row.params();
    let actor = if actor_kind == "host" {
        row.node_id.clone()
    } else if origin.actor.is_empty() {
        "keleustes".into()
    } else {
        origin.actor.clone()
    };
    let mut payload = json!({
        "actionId": row.id, "node": row.node_id, "nodeId": row.node_id,
        "kind": row.kind, "class": row.class, "dryRun": row.dry_run,
        "origin": origin, "params": params, "paramsDigest": params_digest(&params),
        "previewSha256": row.preview_sha256, "outcome": outcome, "reason": reason,
        "planRunId": row.plan_run_id, "step": row.step,
    });
    if let (Value::Object(m), Value::Object(x)) = (&mut payload, extra) {
        m.extend(x);
    }
    record(
        core,
        LedgerDraft {
            at: 0,
            kind: "host_action".into(),
            actor,
            actor_kind: actor_kind.into(),
            msg: format!(
                "{event} {} on {} by {} outcome={outcome}{}",
                row.kind,
                row.node_id,
                origin.label(),
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(" reason={reason}")
                }
            ),
            subject: json!({ "event": event, "actionId": row.id, "node": row.node_id,
                         "kind": row.kind, "origin": origin.label() }),
            payload,
            origin: "factory".into(),
            node_id: row.node_id.clone(),
            session_id: String::new(),
            ref_key: Some(format!("host_action:{}:{event}:{}", row.id, row.last_seq)),
        },
    )
    .await?;
    Ok(())
}

async fn emit_row(core: &Core, kind: &str, row: &HostActionRow, extra: Value) -> Result<()> {
    let mut data = json!({
        "actionId": row.id, "nodeId": row.node_id, "kind": row.kind,
        "class": row.class, "state": row.state, "dryRun": row.dry_run,
        "origin": row.origin(), "error": row.error,
    });
    if let (Value::Object(m), Value::Object(x)) = (&mut data, extra) {
        m.extend(x);
    }
    core.emit(kind, &row.node_id, &data).await?;
    Ok(())
}

fn draft_row(req: &ActionRequest, class: &str, state: &str) -> HostActionRow {
    HostActionRow {
        id: uuid::Uuid::now_v7().to_string(),
        node_id: req.node_id.clone(),
        kind: req.kind.clone(),
        class: class.into(),
        params_json: serde_json::to_string(&req.params).unwrap_or_else(|_| "{}".into()),
        dry_run: req.dry_run,
        preview_sha256: req.preview_sha256.clone(),
        if_match: req.if_match.clone(),
        origin_json: serde_json::to_string(&req.origin).unwrap_or_else(|_| "{}".into()),
        plan_run_id: req.plan_run_id.clone(),
        step: req.step,
        state: state.into(),
        requested_at: now_ms(),
        ..Default::default()
    }
}

/// Stores, emits and audits a refusal, and returns it as the error.
async fn refuse(
    core: &Core,
    req: &ActionRequest,
    class: &str,
    r: Refusal,
    control_mode: &str,
) -> anyhow::Error {
    let mut row = draft_row(req, class, "refused");
    row.error = r.to_string();
    row.finished_at = Some(row.requested_at);
    let store_res = core.store().insert_host_action(&row).await;
    let extra = json!({ "code": r.code(), "control": control_mode });
    let emitted = emit_row(core, kinds::ACTION_REFUSED, &row, extra.clone()).await;
    let audited = audit(
        core,
        &row,
        kinds::ACTION_REFUSED,
        "user",
        "refused",
        r.code(),
        extra,
    )
    .await;
    if let Err(e) = store_res.and(emitted).and(audited) {
        tracing::warn!("recording a host action refusal: {e:#}");
    }
    anyhow::Error::new(r)
}

fn control_word(m: control::Mode) -> &'static str {
    match m {
        control::Mode::Running => "",
        control::Mode::Pause => "paused",
        control::Mode::Stop => "stopped",
    }
}

// ---- requesting -------------------------------------------------------------------------

/// **The host action check**, then queue. See the module docs for the order.
/// Returns the queued row; a refusal is stored, emitted, audited and
/// returned as an error carrying [`Refusal`].
pub async fn request_action(core: &Core, mut req: ActionRequest) -> Result<HostActionRow> {
    req.origin = clean_origin(req.origin);
    let Some(class) = class_of(&req.kind) else {
        return Err(refuse(core, &req, "", Refusal::UnknownKind, "").await);
    };
    let cls = class.as_str();
    if !req.origin.is_console() {
        return Err(refuse(core, &req, cls, Refusal::NotConsole, "").await);
    }

    // 1. Kill switch (rule 3; decision O3 for reads).
    let st = control::state(core).await?;
    let mode = control_word(st.mode);
    if class.mutating() || !req.origin.is_click() {
        // Emits the audited factory.refused event when it blocks, and holds
        // changes inside a maintenance window (the switch itself off).
        if let Err(e) = control::check(core, "provision", &req.node_id, &req.origin.label()).await {
            let Some(b) = e.downcast_ref::<control::Blocked>() else {
                return Err(e);
            };
            let why = if b.window { "held" } else { mode };
            return Err(refuse(core, &req, cls, Refusal::KillSwitch(why.into()), why).await);
        }
    }

    // 2. A2A blackout: tighten-only areas are frozen.
    if touches_a2a(&req.kind, &req.params) && st.a2a_blackout {
        let _ = control::check_a2a(core, "keleustes", &req.node_id, &req.origin.label()).await;
        return Err(refuse(core, &req, cls, Refusal::A2aBlackout, mode).await);
    }

    // 3. Online.
    if !online(core, &req.node_id).await {
        return Err(refuse(core, &req, cls, Refusal::Offline, mode).await);
    }

    // 4-5. The host offers the kind, and its gate admits the class.
    let inv = inventory(core, &req.node_id)
        .await?
        .map(|(_, i)| i)
        .unwrap_or_default();
    if !inv.offers(&req.kind) {
        return Err(refuse(core, &req, cls, Refusal::NotOffered, mode).await);
    }
    let need = class.needs_level();
    if inv.level() < need {
        let r = Refusal::Gate {
            need: need.as_str().into(),
            have: inv.level().as_str().into(),
        };
        return Err(refuse(core, &req, cls, r, mode).await);
    }
    if class == Class::Shell && !inv.manage.manage_shell {
        return Err(refuse(core, &req, cls, Refusal::ShellOff, mode).await);
    }

    // 6-7. Per-class policy.
    if let Err(r) = policy(core, &req, class, &inv).await {
        return Err(refuse(core, &req, cls, r, mode).await);
    }

    let mut row = draft_row(&req, cls, "queued");
    if req.kind == "config_set" && row.if_match.is_empty() {
        row.if_match = inv.settings_sha256.clone();
    }
    core.store().insert_host_action(&row).await?;
    let extra = json!({ "control": mode });
    emit_row(core, kinds::ACTION_REQUESTED, &row, extra.clone()).await?;
    audit(
        core,
        &row,
        kinds::ACTION_REQUESTED,
        "user",
        "requested",
        "",
        extra.clone(),
    )
    .await?;
    emit_row(core, kinds::ACTION_QUEUED, &row, extra).await?;
    Ok(row)
}

async fn policy(
    core: &Core,
    req: &ActionRequest,
    class: Class,
    inv: &Inventory,
) -> std::result::Result<(), Refusal> {
    for (k, v) in &req.params {
        if k.len() > 64 || v.len() > 4096 || v.chars().any(|c| c.is_control() && c != '\n') {
            return Err(Refusal::Invalid(format!(
                "parameter {k:?} is not acceptable"
            )));
        }
    }
    if req.kind == "product_probe"
        || matches!(
            req.kind.as_str(),
            "product_install" | "product_upgrade" | "product_uninstall" | "product_configure"
        )
    {
        let p = req.params.get("product").map(String::as_str).unwrap_or("");
        if !PRODUCTS.contains(&p) {
            return Err(Refusal::Invalid("unknown product".into()));
        }
    }
    if req.kind == "product_auth" && req.params.get("product").map(String::as_str) != Some("cli") {
        return Err(Refusal::Invalid("only the CLI signs in".into()));
    }
    if req.kind == "config_set" {
        let key = req.params.get("key").map(String::as_str).unwrap_or("");
        let value = req.params.get("value").map(String::as_str).unwrap_or("");
        if let Err(why) = telemetry_value(key, value) {
            return Err(Refusal::Invalid(why.into()));
        }
    }
    if req.kind == "config_set" && !req.dry_run {
        let want = &inv.settings_sha256;
        if !req.if_match.is_empty() && !want.is_empty() && &req.if_match != want {
            return Err(Refusal::SettingsChanged);
        }
    }
    if matches!(class, Class::Provision | Class::Shell) && !req.dry_run {
        let name = core
            .store()
            .node(&req.node_id)
            .await
            .ok()
            .flatten()
            .map(|n| n.name)
            .unwrap_or_default();
        // A plan's steps carry the confirmation of the plan run.
        let typed_ok = !name.is_empty() && req.confirm_name.trim() == name;
        if !typed_ok {
            return Err(Refusal::Confirm);
        }
        if !preview_current(core, req).await {
            return Err(Refusal::PreviewStale);
        }
    }
    Ok(())
}

/// Whether a dry run of the same kind and params on the same node finished
/// within [`PREVIEW_TTL_MS`] with the hash the person confirmed.
async fn preview_current(core: &Core, req: &ActionRequest) -> bool {
    if req.preview_sha256.is_empty() {
        return false;
    }
    let digest = params_digest(&req.params);
    let now = now_ms();
    let Ok(rows) = core.store().host_actions(&req.node_id, 500).await else {
        return false;
    };
    rows.iter().any(|r| {
        r.dry_run
            && r.kind == req.kind
            && r.state == "done"
            && r.preview_sha256 == req.preview_sha256
            && now - r.requested_at <= PREVIEW_TTL_MS
            && params_digest(&r.params()) == digest
    })
}

/// Turns a finished dry run into the real action: same kind and params, its
/// preview hash, and the host name the person typed.
pub async fn confirm(
    core: &Core,
    preview_action_id: &str,
    typed_name: &str,
    origin: Origin,
) -> Result<HostActionRow> {
    let p = core
        .store()
        .host_action(preview_action_id)
        .await?
        .ok_or_else(|| anyhow!("unknown preview"))?;
    if !p.dry_run || p.state != "done" {
        return Err(anyhow::Error::new(Refusal::PreviewStale));
    }
    let req = ActionRequest {
        node_id: p.node_id.clone(),
        kind: p.kind.clone(),
        params: p.params(),
        dry_run: false,
        preview_sha256: p.preview_sha256.clone(),
        confirm_name: typed_name.into(),
        if_match: p.if_match.clone(),
        origin,
        plan_run_id: p.plan_run_id.clone(),
        step: p.step,
    };
    request_action(core, req).await
}

/// Cancels an action that has not settled. A queued one never leaves; one
/// the host runs is told through its next delivery check.
pub async fn cancel(core: &Core, action_id: &str, origin: Origin) -> Result<bool> {
    let Some(row) = core.store().host_action(action_id).await? else {
        return Ok(false);
    };
    if !core
        .store()
        .settle_host_action(action_id, "cancelled", "cancelled from Keleustes")
        .await?
    {
        return Ok(false);
    }
    let row = core.store().host_action(action_id).await?.unwrap_or(row);
    let extra = json!({ "by": origin.label() });
    emit_row(core, kinds::ACTION_FINISHED, &row, extra.clone()).await?;
    audit(
        core,
        &row,
        kinds::ACTION_FINISHED,
        "user",
        "cancelled",
        "",
        extra,
    )
    .await?;
    Ok(true)
}

// ---- delivery (called by kls-node) ---------------------------------------------------------

/// The kill-switch check for one outbound action frame (rule 3). An action
/// queued before the switch went on is refused here (stored, emitted,
/// audited) instead of leaving. Returns whether to send it.
pub async fn may_deliver(core: &Core, row: &HostActionRow) -> Result<bool> {
    if row.state != "queued" {
        return Ok(false);
    }
    let st = control::state(core).await?;
    if st.mode.running() {
        return Ok(true);
    }
    let class = row.class_enum().unwrap_or(Class::Shell);
    if !class.mutating() && row.origin().is_click() {
        return Ok(true);
    }
    let mode = control_word(st.mode);
    let _ = control::check(core, "node_frame", &row.node_id, &row.origin().label()).await;
    let why = Refusal::KillSwitch(mode.into());
    if core
        .store()
        .settle_host_action(&row.id, "refused", &why.to_string())
        .await?
    {
        let row = core.store().host_action(&row.id).await?.unwrap_or_default();
        let extra = json!({ "code": why.code(), "control": mode, "at": "delivery" });
        emit_row(core, kinds::ACTION_REFUSED, &row, extra.clone()).await?;
        audit(
            core,
            &row,
            kinds::ACTION_REFUSED,
            "rule",
            "refused",
            why.code(),
            extra,
        )
        .await?;
    }
    Ok(false)
}

/// The vault grant a `product_auth` action carries: minted at send time,
/// never stored. `None` when the vault holds no `vulnetix` secrets (the host
/// then hands off to a browser sign-in).
pub async fn grant_for(
    core: &Core,
    row: &HostActionRow,
) -> Result<Option<crate::vault::GrantPayload>> {
    if row.kind != "product_auth" || row.dry_run {
        return Ok(None);
    }
    crate::vault::grant_for_purpose(
        core,
        &[VULNETIX_VAULT_PROVIDER.to_string()],
        AUTH_GRANT_TTL_MS,
        "vulnetix_auth",
    )
    .await
}

/// A node's update, as the wire carries it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update {
    pub action_id: String,
    pub seq: u32,
    pub state: String,
    pub step: String,
    pub pct: u32,
    pub lines: Vec<String>,
    pub preview_json: String,
    pub preview_sha256: String,
    pub result_json: String,
    pub error: String,
    pub handoff_command: String,
    pub handoff_reason: String,
}

const STATES: [&str; 7] = [
    "accepted",
    "refused",
    "running",
    "needs_human",
    "done",
    "failed",
    "cancelled",
];

fn cap(s: &str, n: usize) -> String {
    s.chars().filter(|c| *c != '\0').take(n).collect()
}

/// Applies a node's update to an action addressed to it. Errors carry a
/// [`PatchRefusal`] for the node server to map (not addressed, stale seq,
/// settled, unknown).
pub async fn apply_update(core: &Core, node_id: &str, u: Update) -> Result<HostActionRow> {
    if !STATES.contains(&u.state.as_str()) {
        anyhow::bail!(PatchRefusal::Stale { last: -1 });
    }
    let reason = match u.handoff_reason.as_str() {
        "" | "needs_root" | "needs_browser" => u.handoff_reason.clone(),
        _ => "needs_root".into(),
    };
    let handoff = if u.state == "needs_human" {
        serde_json::to_string(&Handoff {
            command: cap(&u.handoff_command, 1000),
            reason,
            check_action: String::new(),
        })?
    } else {
        String::new()
    };
    let patch = HostActionPatch {
        seq: u.seq as i64,
        state: u.state.clone(),
        step_label: cap(&u.step, 200),
        pct: u.pct.min(100) as i64,
        lines: u
            .lines
            .iter()
            .take(200)
            .map(|l| cap(&scrub_line(l), 2000))
            .collect(),
        preview_json: cap(&u.preview_json, 256 * 1024),
        preview_sha256: cap(&u.preview_sha256, 64),
        result_json: cap(&u.result_json, 1024 * 1024),
        error: cap(&u.error, 1000),
        handoff_json: handoff,
    };
    let before = core.store().host_action(&u.action_id).await?;
    let row = match core
        .store()
        .apply_host_action_update(node_id, &u.action_id, &patch)
        .await?
    {
        Ok(r) => r,
        Err(r) => return Err(anyhow::Error::new(r)),
    };
    let changed = before.as_ref().map(|b| b.state.as_str()) != Some(row.state.as_str());
    let (kind, outcome) = match row.state.as_str() {
        "accepted" => (kinds::ACTION_ACCEPTED, "accepted"),
        "refused" => (kinds::ACTION_REFUSED, "refused"),
        "needs_human" => (kinds::ACTION_NEEDS_HUMAN, "needs_human"),
        "done" | "failed" | "cancelled" => (kinds::ACTION_FINISHED, row.state.as_str()),
        _ => (kinds::ACTION_PROGRESS, "running"),
    };
    let extra = json!({ "seq": row.last_seq, "pct": row.pct, "step": row.step_label,
                        "by": "host" });
    emit_row(core, kind, &row, extra.clone()).await?;
    if changed {
        audit(core, &row, kind, "host", outcome, &row.error, extra).await?;
    }
    Ok(row)
}

// ---- inventory reports ---------------------------------------------------------------------

/// Stores a node's inventory; emits `host.inventory` when it changed, and
/// settles hand-offs the new inventory confirms.
pub async fn report_inventory(core: &Core, node_id: &str, inv: &Inventory) -> Result<bool> {
    report_inventory_link(core, node_id, inv, 0).await
}

/// [`report_inventory`] with the certificate expiry the node server read
/// from the TLS handshake (0: unknown). The link facts are stamped here and
/// do not count as a change.
pub async fn report_inventory_link(
    core: &Core,
    node_id: &str,
    inv: &Inventory,
    cert_not_after_ms: i64,
) -> Result<bool> {
    let bare = |i: &Inventory| Inventory {
        link: LinkFacts::default(),
        ..i.clone()
    };
    let before = inventory(core, node_id).await?.map(|(_, i)| bare(&i));
    let mut stamped = inv.clone();
    stamped.link = LinkFacts {
        received_at_ms: now_ms(),
        cert_not_after_ms,
    };
    let inv = &stamped;
    let json = serde_json::to_string(inv)?;
    let at = if inv.at_ms > 0 { inv.at_ms } else { now_ms() };
    core.store()
        .upsert_host_inventory(node_id, at, &json)
        .await?;
    let changed = before.as_ref() != Some(&bare(inv));
    if changed {
        core.emit(
            kinds::INVENTORY,
            node_id,
            &json!({ "nodeId": node_id, "at": at, "products": inv.products.len(),
                     "repos": inv.repos.len(), "level": inv.manage.manage_level }),
        )
        .await?;
    }
    settle_handoffs(core, node_id, inv).await?;
    check_restart_version(core, node_id, inv).await?;
    Ok(changed)
}

/// How long after a restart its version is still checked.
const RESTART_CHECK_MS: i64 = 30 * 60_000;

/// B5, the Keleustes side: a restart that named `expect_version` must come
/// back on it. The host checks too (and rolls back); this records what the
/// console saw, once per restart, as an audited event.
async fn check_restart_version(core: &Core, node_id: &str, inv: &Inventory) -> Result<()> {
    let running = inv.belai.version.trim();
    if running.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    for r in core.store().host_actions(node_id, 100).await? {
        if r.kind != "rc_restart" || r.dry_run || r.state != "done" {
            continue;
        }
        if now - r.finished_at.unwrap_or(r.requested_at) > RESTART_CHECK_MS {
            continue;
        }
        let Some(expected) = r.params().get("expect_version").cloned() else {
            continue;
        };
        if expected.trim_start_matches('v') == running.trim_start_matches('v') {
            continue;
        }
        let event = kinds::VERSION_MISMATCH;
        if core
            .store()
            .ledger_has_ref(&format!("host_action:{}:{event}:{}", r.id, r.last_seq))
            .await?
        {
            continue;
        }
        let extra = json!({ "expected": expected, "running": running });
        emit_row(core, event, &r, extra.clone()).await?;
        audit(
            core,
            &r,
            event,
            "rule",
            "version_mismatch",
            "version_mismatch",
            extra,
        )
        .await?;
    }
    Ok(())
}

/// The product state a hand-off expects once the person ran the command.
fn expected(row: &HostActionRow) -> Option<(String, &'static [&'static str])> {
    let product = row.params().get("product").cloned().unwrap_or_default();
    let states: &'static [&'static str] = match row.kind.as_str() {
        "product_install" | "product_upgrade" if product == "sensor" => &["active"],
        "product_install" | "product_upgrade" | "product_configure" => {
            &["installed", "configured", "active"]
        }
        "product_auth" => &["installed", "configured", "active"],
        "product_uninstall" => &["absent"],
        _ => return None,
    };
    Some((product, states))
}

async fn settle_handoffs(core: &Core, node_id: &str, inv: &Inventory) -> Result<()> {
    let waiting = core.store().host_actions_in_state("needs_human").await?;
    for row in waiting.into_iter().filter(|r| r.node_id == node_id) {
        let Some(h) = row.handoff() else { continue };
        if h.check_action.is_empty() {
            continue;
        }
        // Only a probe asked after the click counts.
        let Some(check) = core.store().host_action(&h.check_action).await? else {
            continue;
        };
        if check.state != "done" {
            if check.settled() {
                settle_handoff(core, &row, false, "re-probe failed").await?;
            }
            continue;
        }
        let Some((product, states)) = expected(&row) else {
            settle_handoff(core, &row, true, "").await?;
            continue;
        };
        let ok = inv.product(&product).is_some_and(|p| {
            states.contains(&p.state.as_str()) && (row.kind != "product_auth" || p.authenticated)
        });
        let why = if ok { "" } else { "probe does not confirm" };
        settle_handoff(core, &row, ok, why).await?;
    }
    Ok(())
}

async fn settle_handoff(core: &Core, row: &HostActionRow, ok: bool, why: &str) -> Result<()> {
    let state = if ok { "done" } else { "failed" };
    if core.store().settle_host_action(&row.id, state, why).await? {
        let row = core
            .store()
            .host_action(&row.id)
            .await?
            .unwrap_or_else(|| row.clone());
        let extra = json!({ "handoff": "checked" });
        emit_row(core, kinds::ACTION_FINISHED, &row, extra.clone()).await?;
        audit(
            core,
            &row,
            kinds::ACTION_FINISHED,
            "rule",
            state,
            why,
            extra,
        )
        .await?;
    }
    Ok(())
}

/// The person says they ran a hand-off command: re-probe the product. The
/// action settles `done` only when the probe confirms the expected state.
pub async fn handoff_done(core: &Core, action_id: &str, origin: Origin) -> Result<HostActionRow> {
    let row = core
        .store()
        .host_action(action_id)
        .await?
        .ok_or_else(|| anyhow!("unknown host action"))?;
    let Some(mut h) = row.handoff().filter(|_| row.state == "needs_human") else {
        anyhow::bail!("nothing to confirm");
    };
    let product = row.params().get("product").cloned().unwrap_or_default();
    let req = ActionRequest::new(&row.node_id, "product_probe", origin).param("product", &product);
    let check = request_action(core, req).await?;
    h.check_action = check.id.clone();
    // The hand-off keeps waiting; only the probe settles it.
    let patch = HostActionPatch {
        seq: row.last_seq + 1,
        state: "needs_human".into(),
        handoff_json: serde_json::to_string(&h)?,
        ..Default::default()
    };
    let _ = core
        .store()
        .apply_host_action_update(&row.node_id, &row.id, &patch)
        .await?;
    Ok(check)
}

// ---- views ----------------------------------------------------------------------------------

/// Everything the Host view shows for one node.
#[derive(Clone, Debug, Default)]
pub struct HostView {
    pub node: Option<kls_store::Node>,
    pub online: bool,
    pub inventory: Option<(i64, Inventory)>,
    pub actions: Vec<HostActionRow>,
    pub plans: Vec<Plan>,
    pub plan_runs: Vec<HostPlanRunRow>,
    pub control: control::ControlState,
    /// Live workers and crew runs (WP-B `HostStatus`), when reported.
    pub status: Option<(i64, crate::belai::status::NodeStatus)>,
    /// Host schedules (WP-G) and factory schedules that target this node.
    pub schedules: Vec<ScheduleRow>,
    pub factory_schedules: Vec<kls_store::engine::Schedule>,
    /// Library items held, offered and found by the last scan (WP-E, WP-K).
    pub library: LibraryView,
    /// Provider plan windows the host and its sessions reported (row 48).
    pub plan_limits: Vec<crate::belai::plan::PlanLimit>,
}

pub async fn view(core: &Core, node_id: &str) -> Result<HostView> {
    let node = core.store().node(node_id).await?;
    let library = match &node {
        Some(n) => library_view(core, n).await?,
        None => LibraryView::default(),
    };
    Ok(HostView {
        node,
        online: online(core, node_id).await,
        inventory: inventory(core, node_id).await?,
        actions: core.store().host_actions(node_id, 200).await?,
        plans: plans(core).await?,
        plan_runs: core.store().host_plan_runs(node_id, 50).await?,
        control: control::state(core).await?,
        status: core
            .node_statuses()
            .await?
            .into_iter()
            .find(|(n, _, _)| n == node_id)
            .map(|(_, at, s)| (at, s)),
        schedules: core
            .store()
            .host_schedules(Some(node_id))
            .await?
            .into_iter()
            .filter(|s| !s.deleted)
            .collect(),
        factory_schedules: core
            .store()
            .live_schedules()
            .await?
            .into_iter()
            .filter(|s| s.node_id == node_id)
            .collect(),
        library,
        plan_limits: core.node_plan_limits(node_id).await?,
    })
}

impl HostView {
    /// HM-9: the real (non-preview) actions of a plan run, by step.
    pub fn run_actions(&self, run_id: &str) -> Vec<HostActionRow> {
        let mut v: Vec<HostActionRow> = self
            .actions
            .iter()
            .filter(|a| a.plan_run_id == run_id && !a.dry_run)
            .cloned()
            .collect();
        v.sort_by_key(|a| (a.step, a.requested_at));
        v
    }
}

/// Longest node name the console sets.
pub const NODE_NAME_MAX: usize = 64;

/// §3.2 Rename: a console change, refused under the kill switch, audited
/// with its origin and both names.
pub async fn rename_node(core: &Core, node_id: &str, name: &str, origin: Origin) -> Result<()> {
    let origin = clean_origin(origin);
    let event = "host.node.renamed";
    change_allowed(core, "node", node_id, event, "config", &origin).await?;
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > NODE_NAME_MAX
        || name.chars().any(|c| c.is_control())
    {
        return Err(refuse_change(
            core,
            "node",
            node_id,
            event,
            &origin,
            Refusal::Invalid("a name is 1 to 64 characters".into()),
        )
        .await);
    }
    let Some(node) = core.store().node(node_id).await? else {
        return Err(refuse_change(
            core,
            "node",
            node_id,
            event,
            &origin,
            Refusal::Invalid("unknown node".into()),
        )
        .await);
    };
    core.store().rename_node(node_id, name).await?;
    core.emit(
        crate::kinds::NODE_UPDATED,
        node_id,
        &json!({ "name": name, "origin": origin }),
    )
    .await?;
    audit_change(
        core,
        "node",
        node_id,
        event,
        &origin,
        "renamed",
        "",
        json!({ "from": node.name, "to": name }),
    )
    .await
}

/// §3.2 Revoke: records who revoked a node from the Host view and why. A
/// revoke only takes access away, so the kill switch never holds it; the
/// node server does the revoke itself (`Hub::revoke`).
pub async fn record_revoke(
    core: &Core,
    node_id: &str,
    changed: bool,
    origin: Origin,
) -> Result<()> {
    let origin = clean_origin(origin);
    audit_change(
        core,
        "node",
        node_id,
        "host.node.revoked",
        &origin,
        if changed { "revoked" } else { "unchanged" },
        "",
        json!({}),
    )
    .await
}

pub async fn lines(core: &Core, action_id: &str) -> Result<Vec<HostActionLine>> {
    core.store().host_action_lines(action_id).await
}

/// Deletes action lines older than `keep_ms` (rows stay as the audit index).
pub async fn prune_lines(core: &Core, keep_ms: i64) -> Result<u64> {
    core.store()
        .prune_host_action_lines(now_ms() - keep_ms)
        .await
}

/// Something on a host waiting on a person (HM-14).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostNeed {
    pub node_id: String,
    pub node_name: String,
    /// "handoff", "belai_update".
    pub kind: String,
    pub action_id: String,
    pub title: String,
    pub command: String,
    pub reason: String,
    /// `keleustes://fleet/<node>/<tab>`.
    pub link: String,
    pub at: i64,
}

/// Hand-offs waiting for a person, and hosts with a belai update.
pub async fn needs(core: &Core) -> Result<Vec<HostNeed>> {
    let mut out = Vec::new();
    let names: BTreeMap<String, String> = core
        .store()
        .nodes()
        .await?
        .into_iter()
        .filter(|n| n.revoked_at.is_none())
        .map(|n| (n.id, n.name))
        .collect();
    for row in core.store().host_actions_in_state("needs_human").await? {
        let Some(name) = names.get(&row.node_id) else {
            continue;
        };
        let h = row.handoff().unwrap_or_default();
        let product = row.params().get("product").cloned().unwrap_or_default();
        out.push(HostNeed {
            node_id: row.node_id.clone(),
            node_name: name.clone(),
            kind: "handoff".into(),
            action_id: row.id.clone(),
            title: format!("{} {product}", row.kind.replace('_', " ")),
            command: h.command,
            reason: h.reason,
            link: format!("keleustes://fleet/{}/overview", row.node_id),
            at: row.requested_at,
        });
    }
    for (id, name) in &names {
        if let Some((at, inv)) = inventory(core, id).await?
            && inv.belai.update_available
        {
            out.push(HostNeed {
                node_id: id.clone(),
                node_name: name.clone(),
                kind: "belai_update".into(),
                title: format!("belai {} available", inv.belai.latest),
                link: format!("keleustes://fleet/{id}/provision"),
                at,
                ..Default::default()
            });
        }
    }
    Ok(out)
}

// ---- plans ------------------------------------------------------------------------------------

/// One provision plan step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanStep {
    pub kind: String,
    pub params: BTreeMap<String, String>,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub version: i64,
    pub steps: Vec<PlanStep>,
    pub updated_at: i64,
}

fn plan_of(r: HostPlanRow) -> Plan {
    Plan {
        steps: serde_json::from_str(&r.steps_json).unwrap_or_default(),
        id: r.id,
        name: r.name,
        version: r.version,
        updated_at: r.updated_at,
    }
}

pub async fn plans(core: &Core) -> Result<Vec<Plan>> {
    Ok(core
        .store()
        .host_plans()
        .await?
        .into_iter()
        .map(plan_of)
        .collect())
}

pub async fn plan(core: &Core, id: &str) -> Result<Option<Plan>> {
    Ok(core.store().host_plan(id).await?.map(plan_of))
}

/// Creates (empty `id`) or updates a plan. Every step must be a known kind.
pub async fn save_plan(core: &Core, id: &str, name: &str, steps: &[PlanStep]) -> Result<Plan> {
    for s in steps {
        if class_of(&s.kind).is_none() {
            anyhow::bail!("unknown step kind {:?}", s.kind);
        }
    }
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("name the plan");
    }
    let id = if id.is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        id.to_string()
    };
    let row = core
        .store()
        .save_host_plan(&id, name, &serde_json::to_string(steps)?)
        .await?;
    core.emit(
        kinds::PLAN_SAVED,
        &id,
        &json!({ "planId": id, "name": name, "version": row.version, "steps": steps.len() }),
    )
    .await?;
    audit_plan(
        core,
        &id,
        kinds::PLAN_SAVED,
        "",
        "saved",
        json!({ "name": name }),
    )
    .await?;
    Ok(plan_of(row))
}

pub async fn delete_plan(core: &Core, id: &str) -> Result<bool> {
    let ok = core.store().delete_host_plan(id).await?;
    if ok {
        core.emit(kinds::PLAN_DELETED, id, &json!({ "planId": id }))
            .await?;
        audit_plan(core, id, kinds::PLAN_DELETED, "", "deleted", json!({})).await?;
    }
    Ok(ok)
}

async fn audit_plan(
    core: &Core,
    plan_id: &str,
    event: &str,
    node_id: &str,
    outcome: &str,
    extra: Value,
) -> Result<()> {
    let mut payload = json!({ "planId": plan_id, "node": node_id, "outcome": outcome });
    if let (Value::Object(m), Value::Object(x)) = (&mut payload, extra) {
        m.extend(x);
    }
    record(
        core,
        LedgerDraft {
            at: 0,
            kind: "host_plan".into(),
            actor: "owner".into(),
            actor_kind: "user".into(),
            msg: format!("{event} {plan_id} {node_id} outcome={outcome}"),
            subject: json!({ "event": event, "planId": plan_id, "node": node_id }),
            payload,
            origin: "factory".into(),
            node_id: node_id.into(),
            session_id: String::new(),
            ref_key: None,
        },
    )
    .await?;
    Ok(())
}

fn plan_origin(plan_id: &str, base: &Origin) -> Origin {
    Origin {
        actor: "owner".into(),
        surface: if base.workflow_id.is_empty() {
            format!("plan:{plan_id}")
        } else {
            format!("workflow:{}", base.workflow_id)
        },
        // The workflow run that asked, if any; the plan run is the
        // action's own `plan_run_id`.
        run_id: base.run_id.clone(),
        workflow_id: base.workflow_id.clone(),
        plan_id: plan_id.into(),
        reason: base.reason.clone(),
    }
}

/// Asks the host for a dry run of every step. Returns the preview actions in
/// step order; `run_plan` needs them all done.
pub async fn preview_plan(
    core: &Core,
    plan_id: &str,
    node_id: &str,
    origin: Origin,
) -> Result<Vec<HostActionRow>> {
    let p = plan(core, plan_id)
        .await?
        .ok_or_else(|| anyhow!("unknown plan"))?;
    let o = plan_origin(plan_id, &origin);
    let mut out = Vec::new();
    for (i, s) in p.steps.iter().enumerate() {
        let req = ActionRequest {
            node_id: node_id.into(),
            kind: s.kind.clone(),
            params: s.params.clone(),
            dry_run: true,
            origin: o.clone(),
            step: i as i64 + 1,
            ..Default::default()
        };
        out.push(request_action(core, req).await?);
    }
    Ok(out)
}

/// Starts a plan run: the typed host name and the finished previews (one per
/// step, from [`preview_plan`]). Steps run one at a time; the kill switch is
/// checked again before every step. Returns the run id; the run continues in
/// the background.
pub async fn run_plan(
    core: &Core,
    plan_id: &str,
    node_id: &str,
    typed_name: &str,
    previews: &[String],
    origin: Origin,
) -> Result<String> {
    let p = plan(core, plan_id)
        .await?
        .ok_or_else(|| anyhow!("unknown plan"))?;
    let run_id = uuid::Uuid::now_v7().to_string();
    let o = plan_origin(plan_id, &origin);
    let run = HostPlanRunRow {
        id: run_id.clone(),
        plan_id: plan_id.into(),
        plan_version: p.version,
        node_id: node_id.into(),
        origin_json: serde_json::to_string(&o)?,
        state: "running".into(),
        step: 0,
        started_at: now_ms(),
        ..Default::default()
    };
    core.store().insert_host_plan_run(&run).await?;
    let st = control::state(core).await?;
    if !st.mode.running() {
        let _ = control::check(core, "provision", node_id, &o.label()).await;
        finish_run(core, &run, "refused", 0, "kill switch").await?;
        anyhow::bail!(Refusal::KillSwitch(control_word(st.mode).into()));
    }
    let mut pre = Vec::new();
    for id in previews {
        let r = core.store().host_action(id).await?;
        pre.push(r.filter(|r| r.dry_run && r.state == "done" && r.node_id == node_id));
    }
    if pre.len() != p.steps.len() || pre.iter().any(Option::is_none) {
        finish_run(core, &run, "refused", 0, "preview every step first").await?;
        anyhow::bail!(Refusal::PreviewStale);
    }
    core.emit(
        kinds::PLAN_STARTED,
        node_id,
        &json!({ "runId": run_id, "planId": plan_id, "nodeId": node_id,
                 "steps": p.steps.len(), "origin": o }),
    )
    .await?;
    audit_plan(
        core,
        plan_id,
        kinds::PLAN_STARTED,
        node_id,
        "started",
        json!({ "runId": run_id, "origin": o }),
    )
    .await?;
    let core2 = core.clone();
    let typed = typed_name.to_string();
    let pre: Vec<HostActionRow> = pre.into_iter().flatten().collect();
    tokio::spawn(async move {
        if let Err(e) = drive_plan(&core2, run, p, pre, typed, o).await {
            tracing::warn!("plan run: {e:#}");
        }
    });
    Ok(run_id)
}

async fn finish_run(
    core: &Core,
    run: &HostPlanRunRow,
    state: &str,
    step: i64,
    error: &str,
) -> Result<()> {
    // Audit first: a run is never seen settled without its ledger entry.
    audit_plan(
        core,
        &run.plan_id,
        kinds::PLAN_FINISHED,
        &run.node_id,
        state,
        json!({ "runId": run.id, "step": step, "error": error }),
    )
    .await?;
    core.store()
        .update_host_plan_run(&run.id, state, step, error)
        .await?;
    core.emit(
        kinds::PLAN_FINISHED,
        &run.node_id,
        &json!({ "runId": run.id, "planId": run.plan_id, "nodeId": run.node_id,
                 "state": state, "step": step, "error": error }),
    )
    .await?;
    Ok(())
}

/// Cancels a plan run after its current step.
pub async fn cancel_plan_run(core: &Core, run_id: &str) -> Result<()> {
    if let Some(r) = core.store().host_plan_run(run_id).await?
        && r.state == "running"
    {
        core.store()
            .update_host_plan_run(run_id, "cancelling", r.step, "")
            .await?;
        core.emit(
            kinds::PLAN_FINISHED,
            &r.node_id,
            &json!({ "runId": run_id, "state": "cancelling" }),
        )
        .await?;
    }
    Ok(())
}

async fn drive_plan(
    core: &Core,
    run: HostPlanRunRow,
    plan: Plan,
    previews: Vec<HostActionRow>,
    typed: String,
    origin: Origin,
) -> Result<()> {
    let mut rx = core.subscribe();
    for (i, (step, pre)) in plan.steps.iter().zip(previews.iter()).enumerate() {
        let n = i as i64 + 1;
        let cur = core.store().host_plan_run(&run.id).await?;
        if cur.as_ref().is_some_and(|r| r.state == "cancelling") {
            return finish_run(core, &run, "cancelled", n - 1, "").await;
        }
        // Rule 3: before every step, not only at start. A maintenance
        // window holds the plan (wait and retry); the kill switch ends it.
        loop {
            match control::check(core, "provision", &run.node_id, &origin.label()).await {
                Ok(()) => break,
                Err(e) => match e.downcast_ref::<control::Blocked>() {
                    Some(b) if b.window => {
                        let r = core.store().host_plan_run(&run.id).await?;
                        if r.as_ref().is_some_and(|r| r.state == "cancelling") {
                            return finish_run(core, &run, "cancelled", n - 1, "").await;
                        }
                        tokio::time::sleep(PLAN_HOLD_POLL).await;
                    }
                    Some(_) => {
                        return finish_run(core, &run, "refused", n - 1, "kill switch").await;
                    }
                    None => return Err(e),
                },
            }
        }
        core.store()
            .update_host_plan_run(&run.id, "running", n, "")
            .await?;
        let req = ActionRequest {
            node_id: run.node_id.clone(),
            kind: step.kind.clone(),
            params: step.params.clone(),
            dry_run: false,
            preview_sha256: pre.preview_sha256.clone(),
            confirm_name: typed.clone(),
            if_match: String::new(),
            origin: origin.clone(),
            plan_run_id: run.id.clone(),
            step: n,
        };
        let row = match request_action(core, req).await {
            Ok(r) => r,
            Err(e) => {
                let state = if refusal_of(&e).is_some_and(|r| r.code() == "kill_switch") {
                    "refused"
                } else {
                    "failed"
                };
                return finish_run(core, &run, state, n, &e.to_string()).await;
            }
        };
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(STEP_TIMEOUT_MS as u64);
        loop {
            let r = core.store().host_action(&row.id).await?.unwrap_or_default();
            if r.settled() {
                if r.state != "done" {
                    return finish_run(core, &run, "failed", n, &r.error).await;
                }
                break;
            }
            // Rule 3 while the step is in flight too: a stop cancels it (a
            // queued action never leaves; the host refuses on its own
            // control.json) and ends the plan at once.
            if !control::state(core).await?.mode.running() {
                let _ = control::check(core, "provision", &run.node_id, &origin.label()).await;
                cancel(core, &row.id, origin.clone()).await?;
                return finish_run(core, &run, "refused", n, "kill switch").await;
            }
            if tokio::time::Instant::now() >= deadline {
                return finish_run(core, &run, "failed", n, "step timed out").await;
            }
            tokio::select! {
                ev = rx.recv() => {
                    if let Err(tokio::sync::broadcast::error::RecvError::Closed) = ev {
                        anyhow::bail!("event bus closed")
                    }
                }
                _ = tokio::time::sleep(PLAN_CONTROL_POLL) => {}
            }
        }
    }
    finish_run(core, &run, "done", plan.steps.len() as i64, "").await
}

/// The built-in plan offered when none exists: a baseline dev host.
pub fn baseline_steps() -> Vec<PlanStep> {
    let s = |kind: &str, params: &[(&str, &str)], label: &str| PlanStep {
        kind: kind.into(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        label: label.into(),
    };
    vec![
        s("product_install", &[("product", "cli")], "CLI install"),
        s("product_auth", &[("product", "cli")], "CLI sign in"),
        s(
            "product_configure",
            &[("product", "package_firewall"), ("on", "npm,pypi")],
            "Package firewall: npm, pypi",
        ),
        s(
            "product_configure",
            &[
                ("product", "ai_firewall"),
                ("on", "claude-code"),
                ("scope", "user"),
            ],
            "AI firewall: claude-code",
        ),
    ]
}

// ---- schedules and library (HM-10, HM-11) ------------------------------------------

/// Chains a ledger entry for a host change that is not a host action (a
/// schedule edit, a library dispatch), refusals included.
#[allow(clippy::too_many_arguments)]
async fn audit_change(
    core: &Core,
    kind: &str,
    node_id: &str,
    event: &str,
    origin: &Origin,
    outcome: &str,
    reason: &str,
    payload: Value,
) -> Result<()> {
    let mut p = json!({ "node": node_id, "nodeId": node_id, "origin": origin,
                        "outcome": outcome, "reason": reason });
    if let (Value::Object(m), Value::Object(x)) = (&mut p, payload) {
        m.extend(x);
    }
    record(
        core,
        LedgerDraft {
            at: 0,
            kind: kind.into(),
            actor: if origin.actor.is_empty() {
                "keleustes".into()
            } else {
                origin.actor.clone()
            },
            actor_kind: if outcome == "refused" { "rule" } else { "user" }.into(),
            msg: format!(
                "{event} on {node_id} by {} outcome={outcome}{}",
                origin.label(),
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(" reason={reason}")
                }
            ),
            subject: json!({ "event": event, "node": node_id, "origin": origin.label() }),
            payload: p,
            origin: "factory".into(),
            node_id: node_id.into(),
            session_id: String::new(),
            ref_key: None,
        },
    )
    .await
}

/// Refuses a schedule or library change: emitted and audited (rule 2).
async fn refuse_change(
    core: &Core,
    kind: &str,
    node_id: &str,
    event: &str,
    origin: &Origin,
    r: Refusal,
) -> anyhow::Error {
    let _ = core
        .emit(
            kinds::CHANGE_REFUSED,
            node_id,
            &json!({ "nodeId": node_id, "event": event, "code": r.code(),
                     "error": r.to_string(), "origin": origin }),
        )
        .await;
    if let Err(e) = audit_change(
        core,
        kind,
        node_id,
        event,
        origin,
        "refused",
        r.code(),
        json!({}),
    )
    .await
    {
        tracing::warn!("recording a host change refusal: {e:#}");
    }
    anyhow::Error::new(r)
}

/// The checks every schedule and library change takes: from the console,
/// and not while the kill switch is on (which also emits `factory.refused`).
async fn change_allowed(
    core: &Core,
    kind: &str,
    node_id: &str,
    event: &str,
    action: &str,
    origin: &Origin,
) -> Result<()> {
    if !origin.is_console() {
        return Err(refuse_change(core, kind, node_id, event, origin, Refusal::NotConsole).await);
    }
    if let Err(e) = control::check(core, action, node_id, &origin.label()).await {
        let Some(b) = e.downcast_ref::<control::Blocked>() else {
            return Err(e);
        };
        let why = if b.window {
            "held"
        } else {
            control_word(b.mode)
        };
        return Err(refuse_change(
            core,
            kind,
            node_id,
            event,
            origin,
            Refusal::KillSwitch(why.into()),
        )
        .await);
    }
    Ok(())
}

pub use kls_store::belai::HostSchedule as ScheduleRow;

/// HM-10: add, retime, pause or delete (`deleted`) a host schedule through
/// WP-G (`Core::save_host_schedule`, which validates the cron, profile and
/// directory against the node). The host still checks it before it fires.
pub async fn save_schedule(core: &Core, s: ScheduleRow, origin: Origin) -> Result<ScheduleRow> {
    let origin = clean_origin(origin);
    let node = s.node_id.clone();
    let event = if s.deleted {
        "host.schedule.deleted"
    } else {
        "host.schedule.saved"
    };
    change_allowed(core, "host_schedule", &node, event, "schedule", &origin).await?;
    let saved = core.save_host_schedule(s).await?;
    audit_change(
        core,
        "host_schedule",
        &node,
        event,
        &origin,
        "saved",
        "",
        json!({ "schedule": saved.id, "profile": saved.profile, "cron": saved.cron,
                "cwd": saved.cwd, "enabled": saved.enabled, "deleted": saved.deleted,
                "version": saved.version }),
    )
    .await?;
    Ok(saved)
}

/// HM-10 "Run now": a WP-G scheduled dispatch on this node
/// (`trigger.source = "schedule"`); the gateway checks the kill switch.
pub async fn run_schedule_now(core: &Core, s: &ScheduleRow, origin: Origin) -> Result<String> {
    let origin = clean_origin(origin);
    change_allowed(
        core,
        "host_schedule",
        &s.node_id,
        "host.schedule.run",
        "schedule",
        &origin,
    )
    .await?;
    let (_, id) = core
        .dispatch_scheduled(Some(&s.node_id), &s.profile, &s.cwd, &s.id)
        .await?;
    audit_change(
        core,
        "host_schedule",
        &s.node_id,
        "host.schedule.run",
        &origin,
        "dispatched",
        "",
        json!({ "schedule": s.id, "dispatchId": id }),
    )
    .await?;
    Ok(id)
}

/// Library dispatch kinds a Host view or Fleet sends (WP-E, WP-K).
pub const LIBRARY_DISPATCHES: [&str; 3] = ["library_install", "library_backup", "library_scan"];

/// HM-11 (and the Fleet Library row): install, back up or scan library
/// items on a node through WP-E/WP-K dispatches. A hook or an MCP server
/// runs code on the host, so it needs `code_confirmed`. Under the A2A
/// blackout, installs of agents, crews and MCP servers are refused (§6.3).
pub async fn library_dispatch(
    core: &Core,
    node_id: &str,
    kind: &str,
    items: Vec<crate::gateway::LibraryRef>,
    code_confirmed: bool,
    origin: Origin,
) -> Result<String> {
    library_dispatch_with(core, node_id, kind, items, code_confirmed, false, origin).await
}

/// [`library_dispatch`] with `Dispatch.overwrite`: an install replaces the
/// host's copy (the automatic sync's "install" verdict, where the host
/// holds an older library version unchanged).
pub async fn library_dispatch_with(
    core: &Core,
    node_id: &str,
    kind: &str,
    items: Vec<crate::gateway::LibraryRef>,
    code_confirmed: bool,
    overwrite: bool,
    origin: Origin,
) -> Result<String> {
    let origin = clean_origin(origin);
    let event = format!("host.{kind}");
    if !LIBRARY_DISPATCHES.contains(&kind) {
        return Err(refuse_change(
            core,
            "host_library",
            node_id,
            &event,
            &origin,
            Refusal::UnknownKind,
        )
        .await);
    }
    change_allowed(core, "host_library", node_id, &event, "dispatch", &origin).await?;
    let node = core.store().node(node_id).await?;
    if !node
        .as_ref()
        .is_some_and(|n| n.revoked_at.is_none() && n.host.capabilities.takes_dispatch(kind))
    {
        return Err(refuse_change(
            core,
            "host_library",
            node_id,
            &event,
            &origin,
            Refusal::NotOffered,
        )
        .await);
    }
    if kind == "library_install" {
        let agentish = items
            .iter()
            .any(|i| matches!(i.kind.as_str(), "profile" | "agent" | "crew" | "mcp"));
        if agentish && control::state(core).await?.a2a_blackout {
            let _ = control::check_a2a(core, "keleustes", node_id, &origin.label()).await;
            return Err(refuse_change(
                core,
                "host_library",
                node_id,
                &event,
                &origin,
                Refusal::A2aBlackout,
            )
            .await);
        }
        let code = items
            .iter()
            .any(|i| matches!(i.kind.as_str(), "hook" | "mcp"));
        if code && !code_confirmed {
            return Err(refuse_change(
                core,
                "host_library",
                node_id,
                &event,
                &origin,
                Refusal::Confirm,
            )
            .await);
        }
    }
    let names: Vec<String> = items
        .iter()
        .map(|i| format!("{} {}", i.kind, i.name))
        .collect();
    let id = core
        .gateway()?
        .dispatch(
            node_id,
            crate::gateway::DispatchSpec {
                kind: kind.into(),
                library: items,
                overwrite,
                trigger: Some(crate::gateway::Trigger {
                    source: "console".into(),
                    subject: origin.surface.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    audit_change(
        core,
        "host_library",
        node_id,
        &event,
        &origin,
        "dispatched",
        "",
        json!({ "dispatchId": id, "items": names, "overwrite": overwrite }),
    )
    .await?;
    Ok(id)
}

/// HM-11 import: one item of a node's latest WP-K scan into the library
/// (`Core::library_import`; the host re-reads it and checks the hash).
pub async fn library_import(
    core: &Core,
    node_id: &str,
    scan_dispatch: &str,
    idx: i64,
    origin: Origin,
) -> Result<String> {
    let origin = clean_origin(origin);
    change_allowed(
        core,
        "host_library",
        node_id,
        "host.library_import",
        "dispatch",
        &origin,
    )
    .await?;
    let id = core.library_import(scan_dispatch, idx, "").await?;
    audit_change(
        core,
        "host_library",
        node_id,
        "host.library_import",
        &origin,
        "dispatched",
        "",
        json!({ "dispatchId": id, "scan": scan_dispatch, "idx": idx }),
    )
    .await?;
    Ok(id)
}

/// One item the host holds and how it compares with the library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeldItem {
    pub item: kls_store::InventoryRef,
    /// "current", "push" (the library lacks it), "diverged".
    pub verdict: String,
}

/// Everything the Library tab shows for a node.
#[derive(Clone, Debug, Default)]
pub struct LibraryView {
    pub held: Vec<HeldItem>,
    /// Library items that can go to the node.
    pub items: Vec<kls_store::vault::LibraryItem>,
    /// The node's latest scan.
    pub scan: Vec<kls_store::belai::ScanItem>,
}

pub async fn library_view(core: &Core, node: &kls_store::Node) -> Result<LibraryView> {
    let inv = &node.host.capabilities.inventory;
    let refs: Vec<crate::gateway::LibraryRef> = inv
        .iter()
        .map(|i| crate::gateway::LibraryRef {
            kind: i.kind.clone(),
            id: i.id.clone(),
            name: i.name.clone(),
            version: i.version,
            sha256: i.sha256.clone(),
        })
        .collect();
    let verdicts = core.library_reconcile(&refs).await?;
    Ok(LibraryView {
        held: inv
            .iter()
            .zip(verdicts)
            .map(|(i, (_, v))| HeldItem {
                item: i.clone(),
                verdict: v,
            })
            .collect(),
        items: core.store().library_items("").await?,
        scan: core.store().latest_scan(&node.id).await?,
    })
}

#[cfg(test)]
mod tests;
