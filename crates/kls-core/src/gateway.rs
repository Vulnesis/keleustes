//! How the domain reaches belai nodes without depending on the node server.
//!
//! `kls-node`'s `Hub` implements [`NodeGateway`] and installs it with
//! [`Core::set_gateway`](crate::Core::set_gateway). The engine, the UI and
//! the GitHub plane use it to start work on a node or ask one for a job.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Work to start on a node (belai rc). Mirrors the protocol's `Dispatch`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DispatchSpec {
    /// "session", "worker", "crew".
    pub kind: String,
    pub cwd: String,
    pub repo: String,
    pub branch: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub agent: String,
    pub crew: String,
    pub card_id: String,
    pub run_id: String,
    /// "agent", "plan", "goal", "auto".
    pub mode: String,
    /// Attach a vault grant for these providers (M5).
    pub vault_providers: Vec<String>,
    // ---- WP-D (docs/gaps/belai-counterparts.md): every field below is
    // optional on the wire; a host that predates it ignores it.
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub git_sync: Option<bool>,
    #[serde(default)]
    pub ask: Option<bool>,
    /// What started it. The gateway fills one from `run_id`/`card_id` when
    /// unset, so every dispatch carries its provenance.
    #[serde(default)]
    pub trigger: Option<Trigger>,
    #[serde(default)]
    pub budget: Option<WorkerBudget>,
    #[serde(default)]
    pub fill: bool,
    #[serde(default)]
    pub stay: bool,
    #[serde(default)]
    pub stay_idle_max_s: u32,
    #[serde(default)]
    pub member_models: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub clone: Option<CloneSpec>,
    /// "pause"/"resume": the worker.
    #[serde(default)]
    pub worker_id: String,
    /// "stop": the session.
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub control: Option<CrewControl>,
    /// "" or "fast" (a budget downgrade).
    #[serde(default)]
    pub tier: String,
    /// Harness sessions (harnesses.md): registry id ("" = belai), mode,
    /// native session to resume, the resolved key mode and the origin JSON
    /// the node writes into its audit lines.
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub harness_mode: String,
    #[serde(default)]
    pub harness_resume: String,
    #[serde(default)]
    pub harness_keys: String,
    #[serde(default)]
    pub origin_json: String,
    /// kind "project_prefs": flat keys to set (string values) and to clear.
    #[serde(default)]
    pub prefs_set: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub prefs_unset: Vec<String>,
    /// MCP server credentials a session or worker needs (WP-F): each names an
    /// env-var secret in the vault and the (server, key) it feeds on the host.
    #[serde(default)]
    pub mcp: Vec<McpBinding>,
    /// "library_scan" / "library_import" (WP-K).
    #[serde(default)]
    pub item_kind: String,
    #[serde(default)]
    pub scan_dispatch: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub target: String,
    /// "steer" (WP-H): the forge coordinator's answer to a filed request
    /// (`worker_id` the worker, `card_id` the item).
    #[serde(default)]
    pub steer_request: String,
    #[serde(default)]
    pub steer_event: String,
    #[serde(default)]
    pub steer_pr: i32,
    #[serde(default)]
    pub steer_pr_url: String,
    #[serde(default)]
    pub steer_until_ms: i64,
    #[serde(default)]
    pub steer_reason: String,
    /// "library_install" / "library_backup": the items, by reference only.
    #[serde(default)]
    pub library: Vec<LibraryRef>,
    #[serde(default)]
    pub overwrite: bool,
    /// Crew and step settings (`Dispatch.options`), checked by the host.
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
    /// "budget_update" (#33): the change pushed to running work.
    #[serde(default)]
    pub budget_push: Option<BudgetPush>,
    /// #46, kind "session": a belai session of the node to continue in a new
    /// session, and the entry (belai entry id) to fork it at.
    #[serde(default)]
    pub resume_session: String,
    #[serde(default)]
    pub fork_entry: String,
    /// #52, kind "session": scripted answers (belai `-answers` JSON).
    #[serde(default)]
    pub answers_json: String,
    /// #55, kind "worker" with `agent` empty: a worker made from a prompt.
    #[serde(default)]
    pub adhoc: Option<AdhocWorker>,
}

/// A worker with no stored profile (the protocol's `AdhocWorker`, #55).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AdhocWorker {
    pub prompt: String,
    pub tools: Vec<String>,
    pub claim_lists: Vec<String>,
    pub claim_labels: Vec<String>,
    pub to: String,
    pub publish: String,
    pub read_only: bool,
}

/// A budget change for running work (the protocol's `BudgetPush`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BudgetPush {
    /// "limits", "downgrade", "pause", "resume".
    pub action: String,
    /// With "downgrade": the tier to move to ("fast").
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub budget_id: String,
    #[serde(default)]
    pub budget_name: String,
    #[serde(default)]
    pub reason: String,
    /// Sessions to narrow to (empty: all running work of the host).
    #[serde(default)]
    pub session_ids: Vec<String>,
}

/// The actions a budget push takes.
pub const BUDGET_PUSH_ACTIONS: [&str; 4] = ["limits", "downgrade", "pause", "resume"];

/// Dispatch kinds Keleustes knows; a node must also list the kind in its
/// capabilities (an older node takes session, worker and crew only).
pub const DISPATCH_KINDS: [&str; 15] = [
    "session",
    "worker",
    "crew",
    "stop",
    "pause",
    "resume",
    "crew_control",
    "project_prefs",
    "library_install",
    "library_backup",
    "steer",
    "library_scan",
    "library_import",
    "budget_update",
    "knowledge_reindex",
];

/// One MCP credential binding: vault env secret -> (server, key).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct McpBinding {
    pub server: String,
    pub key: String,
    /// The vault env-var secret's name.
    pub secret: String,
}

/// A library item by reference (the protocol's `LibraryRef`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LibraryRef {
    /// "profile", "crew" or an item kind.
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub sha256: String,
}

/// Provenance of a dispatch (the protocol's `Trigger`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Trigger {
    /// "console", "workflow", "schedule", "card", "node".
    pub source: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub workflow_id: String,
    #[serde(default)]
    pub workflow_name: String,
    #[serde(default)]
    pub card_id: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub origin_node: String,
    #[serde(default)]
    pub origin_session: String,
    #[serde(default)]
    pub origin_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkerBudget {
    pub max_cost_usd: f64,
    pub finalize_at: f64,
    pub max_continuations: u32,
    pub max_attempts: u32,
    pub max_wall: String,
    pub max_passes: u32,
    pub max_tokens: u32,
    pub max_iterations: u32,
    pub price_input_per_mtok: f64,
    pub price_output_per_mtok: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CloneSpec {
    pub owner: String,
    pub name: String,
    pub branch: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CrewControl {
    pub run: String,
    pub op: String,
    pub worker: String,
    pub dir: String,
    pub profile: String,
    pub weight: i32,
    pub paused: bool,
    pub replicas: u32,
}

/// What a job needs from the node that runs it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct JobNeeds {
    pub gh: bool,
    pub provider: Option<String>,
    /// (cli, host) the node must advertise as signed in (forges.md WP-9).
    #[serde(default)]
    pub forge: Option<(String, String)>,
}

/// Outcome of a job or dispatch, as the node reported it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct JobOutcome {
    /// "ok", "failed", "refused", "timeout".
    pub status: String,
    pub output_json: String,
    pub error: String,
    pub session_id: String,
    pub node_id: String,
}

#[async_trait::async_trait]
pub trait NodeGateway: Send + Sync {
    /// Node ids with an open link.
    async fn online(&self) -> Vec<String>;
    /// Queues a dispatch for `node_id` and returns the dispatch id.
    async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String>;
    /// Runs a job on `node_id`, or on any online node meeting `needs`, and
    /// waits up to `timeout_ms` for its outcome.
    async fn job(
        &self,
        node_id: Option<&str>,
        kind: &str,
        payload_json: &str,
        needs: JobNeeds,
        timeout_ms: i64,
    ) -> Result<JobOutcome>;
}
