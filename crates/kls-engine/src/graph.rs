//! The workflow graph: typed nodes, edges, limits and scope, stored as JSON
//! in a version's `graph_json` (the shape of the web factory's graph, see
//! vdb-site docs/factory.md 10.2, adapted to a single desktop owner).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const MAX_NODES: usize = 60;
pub const MAX_EDGES: usize = 120;
pub const MAX_BRIEF: usize = 2000;
pub const MAX_TITLE: usize = 200;
pub const MAX_MESSAGE: usize = 1000;
pub const MAX_EDGE_LABEL: usize = 200;
pub const DEFAULT_STOP_THE_LINE: i64 = 3;
pub const DEFAULT_CONCURRENCY: i64 = 4;

/// Special outcomes an edge may carry besides a condition's own.
pub const OUTCOME_PASS: &str = "pass";
pub const OUTCOME_FAIL: &str = "fail";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub scope: Scope,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub config: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Pos>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub tokens: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// The artifact name shown on the connection.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// A condition outcome, or `pass` / `fail` from a gate.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    /// A gate or condition sending work back to an earlier crew step.
    #[serde(default, rename = "loop", skip_serializing_if = "std::ops::Not::not")]
    pub is_loop: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_budget_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month_budget_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_the_line_attempts: Option<i64>,
    // Agent limits per card (override Settings > Agents, `agents.*`), sent
    // with each dispatch as `Dispatch.budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_card: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize_at_pct: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts_per_card: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_continuations: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_passes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    /// `owner/name`, `owner/*` or `*`; empty means "the repository of the
    /// trigger, or any".
    #[serde(default)]
    pub repos: Vec<String>,
}

/// A condition or gate outcome declaration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub value: String,
    /// The run ends (succeeded) when this outcome is taken.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub end: bool,
}

/// One rule of a condition: `path op value` picks `outcome`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub path: String,
    pub op: String,
    #[serde(default)]
    pub value: Value,
    pub outcome: String,
}

impl Graph {
    pub fn parse(json: &str) -> serde_json::Result<Graph> {
        if json.trim().is_empty() || json.trim() == "{}" {
            return Ok(Graph::default());
        }
        serde_json::from_str(json)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Hash of the canonical JSON (a version's `graph_sha256`).
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.to_json().as_bytes()))
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn triggers(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(|n| n.kind == "trigger")
    }

    pub fn outgoing<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Edge> + 'a {
        self.edges.iter().filter(move |e| e.from == id)
    }

    pub fn incoming<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Edge> + 'a {
        self.edges.iter().filter(move |e| e.to == id)
    }

    /// A fresh node id: `n1`, `n2`, ...
    pub fn next_id(&self) -> String {
        (1..)
            .map(|i| format!("n{i}"))
            .find(|id| self.node(id).is_none())
            .unwrap_or_else(|| "n".into())
    }

    /// Removes a node and every connection to it.
    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    pub fn stop_the_line(&self) -> i64 {
        self.limits
            .stop_the_line_attempts
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_STOP_THE_LINE)
    }

    pub fn concurrency(&self) -> i64 {
        self.limits
            .concurrency
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CONCURRENCY)
    }

    /// A graph with one manual trigger (what a new empty workflow starts with).
    pub fn starter() -> Graph {
        let mut n = Node::new("n1", "trigger", "Start by hand");
        n.config.insert("kind".into(), "manual".into());
        n.position = Some(Pos { x: 40.0, y: 80.0 });
        Graph {
            nodes: vec![n],
            ..Default::default()
        }
    }
}

impl Node {
    pub fn new(id: &str, kind: &str, title: &str) -> Node {
        Node {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    pub fn str(&self, key: &str) -> &str {
        self.config.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.config.get(key).and_then(Value::as_i64)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.config
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// A trigger's extra triggers (`also`), each a trigger config of its
    /// own, numbered from 1 (its row is `<id>#<n>`). Entries that are not
    /// objects are skipped.
    pub fn also(&self) -> Vec<(usize, Map<String, Value>)> {
        self.config
            .get("also")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .enumerate()
                    .filter_map(|(i, v)| v.as_object().map(|o| (i + 1, o.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The trigger kind of a trigger node (`manual` when unset).
    pub fn trigger_kind(&self) -> &str {
        match self.str("kind") {
            "" => "manual",
            k => k,
        }
    }

    /// Declared outcomes of a condition (or `pass`/`fail` for a gate).
    pub fn outcomes(&self) -> Vec<Outcome> {
        match self.kind.as_str() {
            "condition" => self
                .config
                .get("outcomes")
                .and_then(|v| serde_json::from_value::<Vec<Outcome>>(v.clone()).ok())
                .unwrap_or_default(),
            "decision" => {
                let mut o: Vec<Outcome> = self
                    .config
                    .get("outcomes")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                if self.str("belowThreshold") == "route" {
                    o.push(Outcome {
                        value: crate::decision::BELOW_THRESHOLD.into(),
                        end: false,
                    });
                }
                if self.str("onAbstain") == "route" {
                    o.push(Outcome {
                        value: crate::decision::UNKNOWN.into(),
                        end: false,
                    });
                }
                o
            }
            "gate" => vec![
                Outcome {
                    value: OUTCOME_PASS.into(),
                    end: false,
                },
                Outcome {
                    value: OUTCOME_FAIL.into(),
                    end: false,
                },
            ],
            _ => Vec::new(),
        }
    }

    pub fn rules(&self) -> Vec<Rule> {
        self.config
            .get("rules")
            .and_then(|v| serde_json::from_value::<Vec<Rule>>(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Attempts a crew or job step gets before the line stops.
    pub fn max_attempts(&self) -> i64 {
        match self.kind.as_str() {
            "crew" | "job" | "scan" => self.int("maxAttempts").unwrap_or(1).clamp(1, 10),
            _ => 1,
        }
    }

    /// Short words for the node's sub-line ("cron 0 3 * * 1-5", "belai:builder").
    pub fn summary(&self) -> String {
        match self.kind.as_str() {
            "trigger" => match self.trigger_kind() {
                "cron" => {
                    let tz = match self.str("tz") {
                        "" => "UTC",
                        t => t,
                    };
                    crate::cron::describe(self.str("cron"), tz)
                }
                "kanban" => format!(
                    "card {} → {}",
                    or_any(self.str("fromList")),
                    or_any(self.str("toList"))
                ),
                "event" => format!("on {}", or_any(self.str("event"))),
                "webhook" => format!("GitHub {}", or_any(self.str("event"))),
                "label" => format!("label {}", self.str("label")),
                k => k.to_string(),
            },
            "crew" => {
                let who = if !self.str("crew").is_empty() {
                    self.str("crew")
                } else {
                    self.str("agent")
                };
                let brief = self.str("brief").trim().chars().count();
                if brief > 0 {
                    format!("{} {who} · brief, {brief} characters", dispatch_kind(self))
                } else {
                    format!("{} {who}", dispatch_kind(self))
                }
            }
            "job" => format!("job {}", self.str("job")),
            "scan" => format!("Vulnetix CLI {}", self.str("job")),
            "read" => format!("{} from the {}", read_what(self), read_source(self)),
            "profile" => "stores the repository profile".into(),
            "provision" => format!("host plan {} on {}", self.str("planId"), self.str("node")),
            "condition" => {
                let o: Vec<String> = self.outcomes().into_iter().map(|o| o.value).collect();
                o.join(" / ")
            }
            "decision" => format!(
                "SystemOne · {}",
                crate::decision::outcome_values(self).join(" / ")
            ),
            "human" => "waits for a person".into(),
            "gate" => match self.str("check") {
                "forge" => "checks on GitHub".into(),
                _ => "a person passes or fails it".into(),
            },
            "wait" => match self.int("minutes") {
                Some(m) if self.str("event").is_empty() => format!("{m} min"),
                _ => format!("until {}", or_any(self.str("event"))),
            },
            "card" => format!("{} card", or_any(self.str("action"))),
            "notify" => self.str("severity").to_string(),
            "github" => self.str("action").to_string(),
            _ => String::new(),
        }
    }
}

impl Node {
    /// A trigger in a few words for the workflow list: `cron 0 3 * * *`,
    /// `webhook pull_request`, `manual`.
    pub fn trigger_short(&self) -> String {
        let k = self.trigger_kind();
        let arg = match k {
            "cron" => self.str("cron").trim().to_string(),
            "webhook" | "event" => self.str("event").trim().to_string(),
            "label" => self.str("label").trim().to_string(),
            "kanban" => match self.str("toList") {
                "" => String::new(),
                to => format!("→ {to}"),
            },
            _ => String::new(),
        };
        if arg.is_empty() {
            k.to_string()
        } else {
            format!("{k} {arg}")
        }
    }
}

impl Graph {
    /// The first trigger in a few words, `+N` for the others; empty when the
    /// graph has none.
    pub fn trigger_summary(&self) -> String {
        let mut t = self.triggers();
        let Some(first) = t.next() else {
            return String::new();
        };
        let more = t.count();
        if more == 0 {
            first.trigger_short()
        } else {
            format!("{} +{more}", first.trigger_short())
        }
    }
}

fn dispatch_kind(n: &Node) -> &str {
    match n.str("dispatch") {
        "" => "worker",
        k => k,
    }
}

fn or_any(s: &str) -> &str {
    if s.is_empty() { "any" } else { s }
}

// ---- the node catalogue -----------------------------------------------------

/// One choice the builder's palette offers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PaletteItem {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub node_type: &'static str,
    /// Config the node starts with (JSON object text).
    pub preset: &'static str,
    pub title: &'static str,
    /// Needs a pluggable integration that may not be installed (`github`).
    pub requires: Option<&'static str>,
    pub help: &'static str,
}

pub const PALETTE_GROUPS: [(&str, &str); 7] = [
    ("triggers", "Triggers"),
    ("github_reads", "GitHub reads"),
    ("agents", "Agents and jobs"),
    ("logic", "Logic"),
    ("people", "People"),
    ("board", "Board"),
    ("outputs", "Outputs"),
];

pub const PALETTE: &[PaletteItem] = &[
    PaletteItem {
        id: "manual",
        group: "triggers",
        label: "Manual start",
        node_type: "trigger",
        preset: r#"{"kind":"manual"}"#,
        title: "Start by hand",
        requires: None,
        help: "Runs when you press Run.",
    },
    PaletteItem {
        id: "cron",
        group: "triggers",
        label: "Schedule",
        node_type: "trigger",
        preset: r#"{"kind":"cron","cron":"0 9 * * 1-5","tz":"UTC","overlap":"skip_if_running","catchUp":"once"}"#,
        title: "Schedule",
        requires: None,
        help: "Runs on a cron schedule in a time zone.",
    },
    PaletteItem {
        id: "kanban",
        group: "triggers",
        label: "Card moved",
        node_type: "trigger",
        preset: r#"{"kind":"kanban","fromList":"","toList":"todo"}"#,
        title: "Card moved",
        requires: None,
        help: "Runs when a board card changes status.",
    },
    PaletteItem {
        id: "event",
        group: "triggers",
        label: "Factory event",
        node_type: "trigger",
        preset: r#"{"kind":"event","event":"session.ended"}"#,
        title: "On event",
        requires: None,
        help: "Runs on an event of the factory (session.ended, run.finished, ...).",
    },
    PaletteItem {
        id: "webhook",
        group: "triggers",
        label: "GitHub event",
        node_type: "trigger",
        preset: r#"{"kind":"webhook","event":"pull_request.opened"}"#,
        title: "Pull request opened",
        requires: Some("github"),
        help: "Runs on a GitHub webhook event.",
    },
    PaletteItem {
        id: "label",
        group: "triggers",
        label: "GitHub label",
        node_type: "trigger",
        preset: r#"{"kind":"label","label":"belai:fix"}"#,
        title: "Label added",
        requires: Some("github"),
        help: "Runs when a label is added on GitHub.",
    },
    PaletteItem {
        id: "worker",
        group: "agents",
        label: "Dispatch worker",
        node_type: "crew",
        preset: r#"{"dispatch":"worker","agent":"belai:builder","mode":"agent","await":"session","maxAttempts":2}"#,
        title: "Build it",
        requires: None,
        help: "Starts a worker agent on a node and waits for its session to end.",
    },
    PaletteItem {
        id: "crew",
        group: "agents",
        label: "Dispatch crew",
        node_type: "crew",
        preset: r#"{"dispatch":"crew","crew":"belai:feature","mode":"goal","await":"session","maxAttempts":2}"#,
        title: "Crew",
        requires: None,
        help: "Starts a crew of workers on a node.",
    },
    PaletteItem {
        id: "session",
        group: "agents",
        label: "Start session",
        node_type: "crew",
        preset: r#"{"dispatch":"session","mode":"agent","await":"session"}"#,
        title: "Session",
        requires: None,
        help: "Starts a plain belai session with the brief as its prompt.",
    },
    PaletteItem {
        id: "read_mirror",
        group: "github_reads",
        label: "Read the mirror",
        node_type: "read",
        preset: r#"{"source":"mirror","what":"pull"}"#,
        title: "Read the pull request",
        requires: None,
        help: "Reads the run's pull request, repository or issues from the mirror. 0 GitHub calls.",
    },
    PaletteItem {
        id: "read_webhook",
        group: "github_reads",
        label: "Read the webhook payload",
        node_type: "read",
        preset: r#"{"source":"webhook","what":"pull"}"#,
        title: "Read the event",
        requires: None,
        help: "Reads what the triggering event carried. 0 GitHub calls.",
    },
    PaletteItem {
        id: "read_api",
        group: "github_reads",
        label: "Read live from GitHub",
        node_type: "read",
        preset: r#"{"source":"api","what":"pull","onQuotaLow":"use_cached"}"#,
        title: "Read live",
        requires: Some("github"),
        help: "Refreshes the pull request from GitHub (1 call); the mirror when the call fails.",
    },
    PaletteItem {
        id: "scan",
        group: "agents",
        label: "Vulnetix CLI scan",
        node_type: "scan",
        preset: r#"{"source":"cli","job":"scan"}"#,
        title: "Scan",
        requires: None,
        help: "Files a cli-job card the CLI runner claims and waits for its result.",
    },
    PaletteItem {
        id: "provision",
        group: "agents",
        label: "Provision a host",
        node_type: "provision",
        preset: r#"{"planId":"","node":"","confirmName":""}"#,
        title: "Provision",
        requires: None,
        help: "Previews and runs a host plan on a node; the host name is confirmed when you publish.",
    },
    PaletteItem {
        id: "profile",
        group: "outputs",
        label: "Store repo profile",
        node_type: "profile",
        preset: r#"{"output":"repo_profile"}"#,
        title: "Store the profile",
        requires: None,
        help: "Stores what the scout found as the repository's profile.",
    },
    PaletteItem {
        id: "job",
        group: "agents",
        label: "Run job",
        node_type: "job",
        preset: r#"{"job":"","payload":{},"timeoutSeconds":600}"#,
        title: "Job",
        requires: None,
        help: "Runs a job on a node and waits for its result.",
    },
    PaletteItem {
        id: "condition",
        group: "logic",
        label: "Condition",
        node_type: "condition",
        preset: r#"{"rules":[],"default":"yes","outcomes":[{"value":"yes"},{"value":"no"}]}"#,
        title: "Check",
        requires: None,
        help: "Picks a branch from the trigger, inputs and earlier steps.",
    },
    PaletteItem {
        id: "decision",
        group: "logic",
        label: "SystemOne decision",
        node_type: "decision",
        preset: r#"{"question":"","outcomes":[{"value":"yes"},{"value":"no"}],"threshold":0.7,"belowThreshold":"human","onAbstain":"end_run"}"#,
        title: "Decide",
        requires: None,
        help: "A node's decision model picks a branch; below the threshold a person decides.",
    },
    PaletteItem {
        id: "wait",
        group: "logic",
        label: "Wait",
        node_type: "wait",
        preset: r#"{"minutes":10}"#,
        title: "Wait",
        requires: None,
        help: "Waits a while, or until an event arrives.",
    },
    PaletteItem {
        id: "human",
        group: "people",
        label: "Approval",
        node_type: "human",
        preset: r#"{"approvers":["owner"],"slaHours":24,"onTimeout":"park_and_renotify"}"#,
        title: "Approve",
        requires: None,
        help: "Waits for you to approve or reject.",
    },
    PaletteItem {
        id: "gate",
        group: "people",
        label: "Gate",
        node_type: "gate",
        preset: r#"{"check":"manual","onFail":"end_run"}"#,
        title: "Quality gate",
        requires: None,
        help: "Passes or fails; a failed gate can send work back to a crew.",
    },
    PaletteItem {
        id: "card",
        group: "board",
        label: "Move or label card",
        node_type: "card",
        preset: r#"{"action":"move","status":"review"}"#,
        title: "Move card",
        requires: None,
        help: "Moves, labels or assigns the run's card.",
    },
    PaletteItem {
        id: "notify",
        group: "outputs",
        label: "Notify",
        node_type: "notify",
        preset: r#"{"message":"{workflow}: {status}","severity":"info"}"#,
        title: "Notify",
        requires: None,
        help: "Posts a notice to the factory's event stream.",
    },
    PaletteItem {
        id: "draft_pr",
        group: "outputs",
        label: "Draft pull request",
        node_type: "github",
        preset: r#"{"action":"draft_pr","prState":"draft"}"#,
        title: "Draft PR",
        requires: Some("github"),
        help: "Opens a pull request from the card's branch.",
    },
    PaletteItem {
        id: "issue_comment",
        group: "outputs",
        label: "Comment on GitHub",
        node_type: "github",
        preset: r#"{"action":"issue_comment","message":""}"#,
        title: "Comment",
        requires: Some("github"),
        help: "Comments on the issue or pull request of the run.",
    },
];

pub fn palette_item(id: &str) -> Option<&'static PaletteItem> {
    PALETTE.iter().find(|p| p.id == id)
}

/// A node made from a palette item, positioned at `pos`.
pub fn node_from_palette(g: &Graph, item: &PaletteItem, pos: Pos) -> Node {
    let mut n = Node::new(&g.next_id(), item.node_type, item.title);
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(item.preset) {
        n.config = m;
    }
    n.position = Some(pos);
    n
}

/// Every node type the engine knows.
pub const NODE_TYPES: [&str; 15] = [
    "trigger",
    "read",
    "provision",
    "crew",
    "scan",
    "profile",
    "job",
    "condition",
    "decision",
    "wait",
    "human",
    "gate",
    "card",
    "notify",
    "github",
];

pub const TRIGGER_KINDS: [&str; 6] = ["manual", "cron", "kanban", "event", "webhook", "label"];
pub const OVERLAP: [&str; 3] = ["skip_if_running", "queue", "cancel_previous"];
pub const CATCH_UP: [&str; 3] = ["skip", "once", "all"];
pub const DISPATCH_KINDS: [&str; 3] = ["session", "worker", "crew"];
pub const MODES: [&str; 4] = ["agent", "plan", "goal", "auto"];
pub const AWAIT: [&str; 2] = ["session", "none"];
pub const RULE_OPS: [&str; 10] = [
    "eq", "ne", "contains", "exists", "missing", "gt", "gte", "lt", "lte", "in",
];
pub const ON_TIMEOUT: [&str; 2] = ["park_and_renotify", "end_run"];
pub const GATE_CHECKS: [&str; 2] = ["manual", "forge"];
pub const GATE_ON_FAIL: [&str; 3] = ["return_to_crew", "end_run", "human_approval"];
pub const CARD_ACTIONS: [&str; 4] = ["move", "label", "unlabel", "assign"];
pub const SEVERITIES: [&str; 4] = ["info", "notice", "warning", "critical"];
/// The factory never merges: there is no `merge` output. `conflict_cards`
/// runs in the engine (`kls_engine::conflicts`), not through the forge.
pub const GITHUB_ACTIONS: [&str; 8] = [
    "draft_pr",
    "issue_comment",
    "pr_review",
    "approve_ready",
    "check_run",
    "file_issues",
    "add_label",
    "conflict_cards",
];
/// Board statuses (M2's lists).
pub const CARD_STATUSES: [&str; 6] = ["triage", "backlog", "todo", "doing", "review", "done"];

/// The trigger node an extra trigger's row id (`<id>#<n>`) belongs to.
pub fn trigger_of(node_id: &str) -> &str {
    node_id.split('#').next().unwrap_or(node_id)
}

/// The kinds an extra trigger (`also`) may be: a schedule stays one per
/// trigger node.
pub const ALSO_KINDS: [&str; 5] = ["manual", "kanban", "event", "webhook", "label"];

/// The config key an extra trigger's one detail is written under.
pub fn also_detail_key(kind: &str) -> &'static str {
    match kind {
        "webhook" | "event" => "event",
        "label" => "label",
        "kanban" => "toList",
        _ => "",
    }
}

/// Where a `read` node reads from: the mirror (0 calls), the trigger's
/// webhook payload (0 calls) or the live API (1 call, the mirror when the
/// call fails and `onQuotaLow` is `use_cached`).
pub const READ_SOURCES: [&str; 3] = ["mirror", "webhook", "api"];
/// What a `read` node reads: the run's pull request, the repository, or
/// its open issues.
pub const READ_WHAT: [&str; 3] = ["pull", "repo", "issues"];
pub const ON_QUOTA_LOW: [&str; 2] = ["use_cached", "fail"];

/// A `read` node's source (`mirror` when unset).
pub fn read_source(n: &Node) -> &str {
    match n.str("source") {
        "" => "mirror",
        s => s,
    }
}

/// What a `read` node reads (`pull` when unset).
pub fn read_what(n: &Node) -> &str {
    match n.str("what") {
        "" => "pull",
        s => s,
    }
}

/// Builtin belai profiles that run as background agents, not workers: a
/// step that names one finds no host (`agent_not_dispatchable`). From the
/// bundled catalogue ([`crate::catalogue`]).
pub fn background_agents() -> Vec<&'static str> {
    crate::catalogue::background_names()
}

/// Builtin worker profiles (what the builder suggests).
pub fn workers() -> Vec<&'static str> {
    crate::catalogue::worker_names()
}

/// Builtin crews.
pub fn crews() -> Vec<&'static str> {
    crate::catalogue::crew_names()
}

pub fn is_background_agent(name: &str) -> bool {
    crate::catalogue::profile(name).is_some_and(|p| p.is_background())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let g = Graph::starter();
        let back = Graph::parse(&g.to_json()).unwrap();
        assert_eq!(g, back);
        assert_eq!(g.sha256(), back.sha256());
        assert_eq!(Graph::parse("{}").unwrap(), Graph::default());
        assert_eq!(g.next_id(), "n2");
        assert_eq!(g.stop_the_line(), 3);
        let e: Edge = serde_json::from_str(r#"{"from":"a","to":"b","loop":true}"#).unwrap();
        assert!(e.is_loop);
        assert_eq!(
            serde_json::to_string(&Edge {
                from: "a".into(),
                to: "b".into(),
                ..Default::default()
            })
            .unwrap(),
            r#"{"from":"a","to":"b"}"#
        );
    }

    #[test]
    fn trigger_summary_names_first_and_counts_rest() {
        let mut g = Graph::default();
        assert_eq!(g.trigger_summary(), "");
        g.nodes.push(Node::new("x", "crew", ""));
        assert_eq!(g.trigger_summary(), "");
        let mut cron = Node::new("n1", "trigger", "");
        cron.config.insert("kind".into(), "cron".into());
        cron.config.insert("cron".into(), "0 3 * * *".into());
        g.nodes.push(cron);
        assert_eq!(g.trigger_summary(), "cron 0 3 * * *");
        let mut hook = Node::new("n2", "trigger", "");
        hook.config.insert("kind".into(), "webhook".into());
        hook.config.insert("event".into(), "pull_request".into());
        assert_eq!(hook.trigger_short(), "webhook pull_request");
        g.nodes.push(hook);
        g.nodes.push(Node::new("n3", "trigger", ""));
        assert_eq!(g.trigger_summary(), "cron 0 3 * * * +2");
        assert_eq!(Graph::starter().trigger_summary(), "manual");
        let mut k = Node::new("n4", "trigger", "");
        k.config.insert("kind".into(), "kanban".into());
        k.config.insert("toList".into(), "doing".into());
        assert_eq!(k.trigger_short(), "kanban → doing");
    }

    #[test]
    fn palette_presets_parse() {
        let g = Graph::default();
        for p in PALETTE {
            let n = node_from_palette(&g, p, Pos::default());
            assert_eq!(n.kind, p.node_type, "{}", p.id);
            assert!(!n.config.is_empty(), "{}", p.id);
            assert!(NODE_TYPES.contains(&p.node_type));
            assert!(PALETTE_GROUPS.iter().any(|(g, _)| *g == p.group));
        }
        assert!(is_background_agent("belai:triage"));
        assert!(!is_background_agent("belai:builder"));
    }
}
