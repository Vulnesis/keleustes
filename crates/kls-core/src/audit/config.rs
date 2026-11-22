//! The settings document: one typed, revisioned view over the `settings`
//! table, with the sections of the web Settings page that make sense on a
//! desktop (factory-settings.ts), validation, YAML export and import.
//!
//! Values are `serde_json::Value`s in their canonical shape (string, bool,
//! number, list of strings or numbers). They are stored as text the way
//! the readers expect: lists comma separated (`node.advertise_addrs`),
//! booleans `true`/`false`, numbers in decimal. A save names the revision
//! it was based on; a stale one is refused with [`SettingsError::Conflict`].
//! Every changed key is chained to the audit ledger as a `setting` entry
//! (old and new value) in the same transaction.

use crate::Core;
use anyhow::Result;
use kls_store::audit::{LedgerDraft, SettingsWrite};
use kls_store::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Keys other areas read that the document also edits, plus its own.
pub mod keys {
    pub const THEME: &str = "ui.theme";
    pub const TOKEN_TTL_MINUTES: &str = "node.token_ttl_minutes";
    pub const NOTIFY_DESKTOP: &str = "notify.desktop";
    pub const NOTIFY_APPROVALS: &str = "notify.approvals";
    pub const NOTIFY_QUESTIONS: &str = "notify.questions";
    pub const NOTIFY_CONFLICTS: &str = "notify.conflicts";
    /// A time window without desktop notices (escalations still show).
    pub const NOTIFY_QUIET: &str = "notify.quiet";
    /// Mute: quiet hours around the clock (escalations still show).
    pub const NOTIFY_MUTED: &str = "notify.muted";
    /// Read-only: where the database is.
    pub const DATABASE: &str = "persistence.database";
    /// Chat channel notifications: off, slack, discord, generic.
    pub const CHAT: &str = "notify.chat";
    /// Which events reach the chat channel (a `notify.events` level).
    pub const CHAT_EVENTS: &str = "notify.chat_events";
    pub const KANBAN_PROVIDER: &str = "kanban.provider";
    pub const KANBAN_BOARD: &str = "kanban.board";
    pub const KANBAN_SYNC: &str = "kanban.sync";
    pub const KANBAN_LISTS: &str = "kanban.lists";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Enum,
    Bool,
    Int,
    /// Integer percent.
    Pct,
    Num,
    Str,
    /// Comma separated strings.
    StrList,
    /// Ascending whole numbers.
    IntList,
    /// Path globs.
    GlobList,
    /// An IP socket address to listen on.
    Listen,
    /// `host:port` entries.
    AddrList,
    /// An IANA zone name, or empty for the system zone.
    Tz,
    /// `<provider>/<model>` or empty.
    Model,
    /// Computed; never written.
    ReadOnly,
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// (stored value, label) for enums.
    pub options: &'static [(&'static str, &'static str)],
    pub min: f64,
    pub max: f64,
    /// The stored form a missing key reads as.
    pub default: &'static str,
    pub help: &'static str,
    /// Takes effect at the next start.
    pub restart: bool,
    /// An import that leaves it out keeps it (instead of the default).
    pub keep: bool,
}

const fn f(key: &'static str, label: &'static str, kind: Kind, default: &'static str) -> Field {
    Field {
        key,
        label,
        kind,
        options: &[],
        min: 0.0,
        max: 0.0,
        default,
        help: "",
        restart: false,
        keep: false,
    }
}

impl Field {
    const fn opts(mut self, o: &'static [(&'static str, &'static str)]) -> Self {
        self.options = o;
        self
    }
    const fn range(mut self, min: f64, max: f64) -> Self {
        self.min = min;
        self.max = max;
        self
    }
    pub const fn help(mut self, h: &'static str) -> Self {
        self.help = h;
        self
    }
    const fn restart(mut self) -> Self {
        self.restart = true;
        self
    }
    const fn keep(mut self) -> Self {
        self.keep = true;
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub fields: &'static [Field],
}

const AUTONOMY: &[(&str, &str)] = &[
    (
        "observe",
        "Observe: reads and records, changes nothing on GitHub",
    ),
    ("suggest", "Suggest: comments and proposes, no branches"),
    ("draft_pr", "Draft PR: opens draft pull requests"),
    (
        "auto_merge_low_risk",
        "Approve when ready: approves low-risk pull requests",
    ),
];

const BUDGET_ACTIONS: &[(&str, &str)] = &[
    ("pause_run", "Pause the run"),
    ("downgrade_to_fast", "Downgrade to the fast model"),
    ("escalate_human", "Ask a person"),
    ("hard_stop", "Hard stop"),
];

pub static SECTIONS: &[Section] = &[
    Section {
        id: "appearance",
        title: "Appearance",
        desc: "How Keleustes looks on this machine.",
        fields: &[f(keys::THEME, "Theme", Kind::Enum, "dark")
            .opts(&[("system", "System"), ("light", "Light"), ("dark", "Dark")])
            .help("System follows the desktop's light or dark preference.")],
    },
    Section {
        id: "node",
        title: "Node listener",
        desc: "Where belai hosts reach Keleustes, and the join tokens that let a new host in.",
        fields: &[
            f(crate::settings::SERVER_NAME, "Server name", Kind::Str, "")
                .range(1.0, 80.0)
                .keep()
                .help("Shown to nodes when they join and link."),
            f(crate::settings::LISTEN_ADDR, "Listen address", Kind::Listen, crate::DEFAULT_LISTEN_ADDR)
                .restart()
                .help("IP and port the node server binds. 0.0.0.0 listens on every interface."),
            f(crate::settings::ADVERTISE_ADDRS, "Advertised addresses", Kind::AddrList, "")
                .help("host:port entries a join token tells nodes to dial, comma separated. Empty lists this machine's interfaces."),
            f(keys::TOKEN_TTL_MINUTES, "Join token lifetime (minutes)", Kind::Int, "15")
                .range(1.0, 1440.0)
                .help("A join token is single use and expires after this long."),
        ],
    },
    Section {
        id: "autonomy",
        title: "Autonomy",
        desc: "The most an automation may do without a person. Per-repo and per-workflow settings can lower this, never raise it.",
        fields: &[
            f("autonomy.max", "Maximum autonomy", Kind::Enum, "draft_pr").opts(AUTONOMY),
            f("belai.mode", "Default agent mode", Kind::Enum, "agent").opts(&[
                ("goal", "goal"),
                ("agent", "agent"),
                ("plan", "plan"),
                ("auto", "auto"),
            ]),
            f("autonomy.concurrency", "Max concurrent runs", Kind::Int, "4").range(1.0, 64.0),
            f("autonomy.max_attempts", "Stop the line after failed attempts", Kind::Int, "3")
                .range(1.0, 10.0),
            f("autonomy.claim_ttl_minutes", "Minutes a dispatched card may go unclaimed", Kind::Int, "60")
                .range(1.0, 1440.0),
            f("autonomy.no_host_ttl_hours", "Hours a step may wait for a node", Kind::Int, "6")
                .range(1.0, 168.0),
            f("belai.ask_permission", "Require permission asks in unattended sessions", Kind::Bool, "false")
                .help("Off: workers a workflow or schedule starts run tools without stopping for approval. Guardrails still apply."),
            f("autonomy.plan_approval", "Plan approval required for belai:feature", Kind::Bool, "false"),
        ],
    },
    Section {
        id: "defaults",
        title: "Defaults",
        desc: "The workflows that run for every managed repository from the start. None of them merges: a person merges.",
        fields: &[
            f("defaults.enabled", "Run the defaults", Kind::Bool, "true")
                .help("The master switch. Off means no default starts a run for any repository."),
            f("autonomy.default", "Autonomy of a repository with no settings of its own", Kind::Enum, "suggest")
                .opts(AUTONOMY),
            f("approve.sensitive_paths", "Paths that stop an approval", Kind::GlobList, "")
                .help("Path patterns, comma separated. A pull request that touches one is never approved by the factory."),
        ],
    },
    Section {
        id: "models",
        title: "Models",
        desc: "The model each crew and worker runs on when its step and agent name none.",
        fields: &[
            f("models.default", "Default model", Kind::Model, "")
                .help("<provider>/<model>. Empty leaves the choice to each host."),
            f("models.fast", "Fast model", Kind::Model, "")
                .help("Used when a budget downgrades a run to fast. Empty uses each host's fast model."),
            f(crate::vault::settings::MODELS_AGENTS, "Model per agent", Kind::StrList, "")
                .help("agent=provider/model, comma separated. Beats the agent profile; routing rules still win."),
        ],
    },
    Section {
        id: "github",
        title: "Forge",
        desc: "How Keleustes reads GitHub (through the local gh or a node) and how automations touch your repositories.",
        fields: &[
            f("github.poll", "Poll GitHub", Kind::Enum, "on").opts(&[("on", "On"), ("off", "Off")])
                .help("Conditional (ETag) reads of managed repositories, within the factory share below."),
            f("github.poll_interval_s", "Seconds between polls", Kind::Int, "120").range(30.0, 3600.0),
            f("forge.quota.factory_share", "Factory share of the hourly limit", Kind::Pct, "10")
                .range(0.0, 100.0),
            f("forge.quota.reserve", "Reserve for people and time-critical calls", Kind::Pct, "20")
                .range(0.0, 90.0),
            f("forge.branch_prefix", "Branch prefix", Kind::Str, "belai/K-").range(1.0, 40.0),
            f("forge.identity", "Factory identities", Kind::StrList, "")
                .help("Logins the factory acts as on the forge; their pulls, issues and comments never trigger a run."),
            f("forge.pr_state", "Pull request state", Kind::Enum, "draft")
                .opts(&[("draft", "Draft"), ("ready", "Ready for review")]),
            f("forge.protected", "Never push to protected branches", Kind::Bool, "true"),
            f("systemone.min_confidence", "Minimum decision confidence", Kind::Num, "0.8")
                .range(0.0, 1.0),
        ],
    },
    Section {
        id: "signing",
        title: "Commit signing",
        desc: "Commits made from Keleustes (a merge from the conflict editor) are made through the GitHub API, which signs them, or by a node's git.",
        fields: &[
            f("signing.mode", "Sign commits made from Keleustes", Kind::Enum, "preferred").opts(&[
                ("required", "Required"),
                ("preferred", "Preferred"),
                ("off", "Off"),
            ]),
            f("signing.email", "Commit email", Kind::Enum, "primary").opts(&[
                ("primary", "The primary verified email on GitHub"),
                ("noreply", "The GitHub noreply address"),
                ("ask", "Ask at commit time"),
            ]),
        ],
    },
    Section {
        id: "notifications",
        title: "Notifications",
        desc: "Desktop notifications for what needs you. Each one opens the screen it is about.",
        fields: &[
            f(keys::NOTIFY_DESKTOP, "Desktop notifications", Kind::Bool, "true"),
            f(keys::NOTIFY_APPROVALS, "Approvals waiting", Kind::Bool, "true"),
            f(keys::NOTIFY_QUESTIONS, "Session questions", Kind::Bool, "true"),
            f(keys::NOTIFY_CONFLICTS, "Merge conflicts", Kind::Bool, "true"),
            f("notify.events", "Factory events", Kind::Enum, "escalations_holds_budget").opts(&[
                ("escalations_holds_budget", "Escalations, holds, budget alerts"),
                ("everything", "Everything"),
                ("escalations_only", "Escalations only"),
            ]),
            f(keys::NOTIFY_QUIET, "Quiet hours", Kind::Str, "")
                .help("No desktop notices inside them, except escalations. Same form as a maintenance window: daily 22:00-07:00."),
            f(keys::NOTIFY_MUTED, "Mute", Kind::Bool, "false")
                .help("Quiet hours now, until unmuted."),
            f(keys::CHAT, "Chat channel", Kind::Enum, "off").opts(&[
                ("off", "Off"),
                ("slack", "Slack webhook"),
                ("discord", "Discord webhook"),
                ("generic", "JSON webhook"),
            ])
            .help("Posts factory events to a channel. The webhook address is kept sealed in the vault, set beside this."),
            f(keys::CHAT_EVENTS, "Chat events", Kind::Enum, "escalations_only").opts(&[
                ("escalations_only", "Escalations only"),
                ("escalations_holds_budget", "Escalations, holds, budget alerts"),
                ("everything", "Everything"),
            ])
            .help("Quiet hours hold all but escalations. Nothing is posted while the factory is paused or stopped."),
        ],
    },
    Section {
        id: "kanban",
        title: "Kanban binding",
        desc: "Mirror the board to an outside kanban.",
        fields: &[
            f(keys::KANBAN_PROVIDER, "Bound to", Kind::Enum, "off").opts(&[
                ("off", "Nothing (the factory board only)"),
                ("github_projects", "GitHub Projects"),
                ("gitlab_boards", "GitLab issue boards"),
                ("linear", "Linear"),
                ("jira", "Jira"),
            ]),
            f(keys::KANBAN_BOARD, "Board", Kind::Str, "")
                .range(0.0, 300.0)
                .help("The board's address or id: https://github.com/orgs/acme/projects/3, a Linear team key, a Jira project key."),
            f(keys::KANBAN_SYNC, "Sync", Kind::Enum, "push").opts(&[
                ("push", "Factory to board"),
                ("pull", "Board to factory"),
                ("both", "Both ways"),
            ]),
            f(keys::KANBAN_LISTS, "List mapping", Kind::StrList, "triage=Triage, todo=Todo, doing=In Progress, review=In Review, done=Done")
                .help("factory list=board column, comma separated."),
        ],
    },
    Section {
        id: "schedules",
        title: "Schedules",
        desc: "How workflow schedules read time.",
        fields: &[
            f("schedules.tz", "Timezone", Kind::Tz, "")
                .help("An IANA zone name such as Australia/Melbourne. Empty uses this machine's zone."),
            f("schedules.missed", "Missed runs", Kind::Enum, "run_once").opts(&[
                ("run_once", "Run once on resume"),
                ("skip", "Skip"),
                ("run_all", "Run all"),
            ]),
        ],
    },
    Section {
        id: "persistence",
        title: "Persistence",
        desc: "Where runs, sessions and the audit ledger are written. All of it is yours to read and export.",
        fields: &[
            f(keys::DATABASE, "Database", Kind::ReadOnly, "")
                .help("Pass --database-url postgres://… (or KELEUSTES_DATABASE_URL) to use Postgres instead of the SQLite file."),
            f("persistence.retention", "Artifact retention", Kind::Enum, "400d").opts(&[
                ("400d", "400 days"),
                ("90d", "90 days"),
                ("30d", "30 days"),
                ("forever", "Forever"),
            ]),
            f(super::keys::TELEPORT_RETENTION_HOURS, "Keep teleport bundles (hours)", Kind::Int, "168")
                .range(1.0, 720.0),
            f("audit.calls", "Log every model call with its input hash", Kind::Bool, "true"),
        ],
    },
    Section {
        id: "budgets",
        title: "Budgets",
        desc: "Factory-wide defaults. Per-workflow and per-node budgets live on the Budgets screen.",
        fields: &[
            f("budget.on_exceed", "At 100% of any budget", Kind::Enum, "pause_run").opts(BUDGET_ACTIONS),
            f("budget.alerts", "Alert thresholds (percent)", Kind::IntList, "50, 80, 100"),
        ],
    },
    Section {
        id: "crews",
        title: "Crews",
        desc: "Defaults every crew and worker the factory starts runs with, unless its agent says otherwise.",
        fields: &[
            f("belai.guardrails", "Guardrails", Kind::Enum, "standard").opts(&[
                ("strict", "Strict: ask before risky tools"),
                ("standard", "Standard"),
                ("relaxed", "Relaxed"),
            ]),
            f("belai.tests.post_end", "Run tests when a worker ends", Kind::Bool, "true"),
            f("belai.tests.on_fail", "When those tests fail", Kind::Enum, "fix").opts(&[
                ("fix", "Send it back to fix"),
                ("report", "Report and stop"),
                ("block", "Block the card"),
            ]),
            f("belai.show_internal_work", "Show internal work in transcripts", Kind::Bool, "false"),
            f("belai.auto_commit_per_task", "Commit after each task", Kind::Bool, "true"),
            f("a2a.show", "Show A2A messages in sessions", Kind::Bool, "true"),
            f("crews.lease", "Card lease (minutes)", Kind::Int, "30").range(1.0, 1440.0)
                .help("A claimed card returns to the pool when its worker stops renewing for this long."),
            f("agents.max_cost_usd_per_card", "Cost per card, USD", Kind::Num, "5").range(0.0, 10_000.0),
            f("agents.finalize_at", "Finalize at % of the card budget", Kind::Pct, "85").range(1.0, 100.0),
            f("agents.max_attempts", "Attempts per card", Kind::Int, "3").range(1.0, 20.0),
            f("agents.max_continuations", "Continuations per card", Kind::Int, "2").range(0.0, 20.0),
            f("agents.max_passes", "Review passes per card", Kind::Int, "3").range(1.0, 20.0),
            f("agents.max_wall_minutes", "Wall minutes per card", Kind::Int, "120").range(1.0, 10_080.0),
            f("agents.max_iterations", "Tool rounds per pass", Kind::Int, "60").range(1.0, 1000.0),
        ],
    },
    Section {
        id: "systemone",
        title: "System One",
        desc: "The fast model that answers the factory's small questions (triage, routing, merge readiness).",
        fields: &[
            f("systemone.model", "Decision model", Kind::Model, "")
                .help("<provider>/<model>. Empty uses the fast model."),
            f("systemone.fallback", "Fallback model", Kind::Model, ""),
            f("systemone.below_threshold", "Below the minimum confidence", Kind::Enum, "escalate").opts(&[
                ("escalate", "Ask a person"),
                ("proceed", "Proceed with the safest outcome"),
                ("abstain", "Abstain"),
            ]),
            f("systemone.on_abstain", "When it abstains", Kind::Enum, "escalate").opts(&[
                ("escalate", "Ask a person"),
                ("skip", "Skip the step"),
            ]),
            f("systemone.retention", "Keep rationales", Kind::Enum, "90d").opts(&[
                ("30d", "30 days"),
                ("90d", "90 days"),
                ("400d", "400 days"),
            ]),
            f("systemone.override", "A person may override a decision", Kind::Bool, "true"),
            f("systemone.daily_cap", "Decisions per day", Kind::Int, "500")
                .range(1.0, 100_000.0)
                .help("Past it a decision step asks a person."),
        ],
    },
    Section {
        id: "hosts",
        title: "Hosts",
        desc: "Where factory work runs when a step names no node, and when nothing may start.",
        fields: &[
            f("hosts.run_on", "Run on", Kind::Str, "any")
                .help("any, or node:<id> for one node."),
            f("hosts.run_on_fallback", "When that node is offline", Kind::Enum, "wait").opts(&[
                ("wait", "Wait for it"),
                ("any", "Use any online node"),
                ("fail", "Fail the step"),
            ]),
            f(crate::control::keys::MAINTENANCE, "Maintenance window", Kind::Str, "")
                .help("No run starts inside it. Day ranges and times, ; separated: Sat 02:00-06:00; Mon-Fri 23:00-01:00. Times read in the schedules timezone."),
            f(crate::control::keys::BLACKOUT, "Schedule blackout", Kind::Str, "")
                .help("Schedule and event triggers are held inside it and fire on release. Same form as the maintenance window."),
        ],
    },
    Section {
        id: "quota",
        title: "GitHub quota",
        desc: "How the factory shares the hourly GitHub limit with you.",
        fields: &[
            f("forge.quota.on_exhausted", "When the factory share runs out", Kind::Enum, "defer").opts(&[
                ("defer", "Defer until the reset"),
                ("use_cached", "Use cached reads"),
                ("escalate", "Ask a person"),
            ]),
            f("forge.calls_per_hour", "Factory calls per hour (0: share only)", Kind::Int, "0")
                .range(0.0, 15_000.0),
            f("forge.quota.proactive_writes_per_hour", "Proactive writes per hour (0: no cap)", Kind::Int, "60")
                .range(0.0, 5000.0)
                .help("Writes automation makes on its own (comments, labels, pushes). People's writes never count."),
        ],
    },
    Section {
        id: "vault",
        title: "Vault",
        desc: "Provider keys. A key test is an indicator only; it never blocks a save.",
        fields: &[
            f(crate::vault::probe::AUTO_TEST_SETTING, "Test a key after saving it", Kind::Bool, "true")
                .help("One free request (a models list) to the provider, in the background."),
            f(crate::vault::lineage::keys::GRACE_HOURS, "Rotation grace (hours)", Kind::Int, "24")
                .range(0.0, 2160.0)
                .help("A rotated-out key stays sealed this long so it can be rolled back, then it is shredded."),
            f(crate::sweeps::keys::EVENTS_RETENTION_DAYS, "Keep the event stream (days)", Kind::Int, "30")
                .range(1.0, 400.0)
                .help("Older events are pruned once the audit ledger holds them."),
        ],
    },
    Section {
        id: "estimates",
        title: "Estimates",
        desc: "Rates that price agent work as the people it stands in for. Every estimate records the rates it used.",
        fields: &[
            f("delivery.enabled", "Show estimates", Kind::Bool, "true"),
            f("delivery.value", "Value of agent work", Kind::Bool, "true"),
            f("delivery.decisions", "Decisions", Kind::Bool, "true"),
            f("delivery.compare", "Compare with a team", Kind::Bool, "true"),
            f("delivery.multiplier", "Default multiplier", Kind::Num, "2.4").range(0.5, 10.0),
            f("delivery.rate", "Default rate, USD/h", Kind::Num, "95").range(0.0, 10_000.0),
            f("delivery.peopleRate", "People time rate, USD/h", Kind::Num, "110").range(0.0, 10_000.0),
            f("delivery.overheadPct", "Overhead, %", Kind::Pct, "20").range(0.0, 100.0),
            f("delivery.overheadRate", "Overhead rate, USD/h", Kind::Num, "105").range(0.0, 10_000.0),
            f("delivery.hoursPerMonth", "Hours per person-month", Kind::Num, "150").range(1.0, 400.0),
            f("delivery.manualCycleDays", "Team days per pull request", Kind::Num, "4.5").range(0.0, 365.0),
            f("delivery.manualIssueDays", "Team days per issue", Kind::Num, "3").range(0.0, 365.0),
            f("delivery.manualCardDays", "Team days per card", Kind::Num, "5").range(0.0, 365.0),
            f("delivery.roles.sse.rate", "Senior engineer, USD/h", Kind::Num, "95").range(0.0, 10_000.0),
            f("delivery.roles.sse.mult", "Senior engineer, multiplier", Kind::Num, "2.4").range(0.5, 10.0),
            f("delivery.roles.security.rate", "Security engineer, USD/h", Kind::Num, "110").range(0.0, 10_000.0),
            f("delivery.roles.security.mult", "Security engineer, multiplier", Kind::Num, "2.0").range(0.5, 10.0),
            f("delivery.roles.qa.rate", "QA engineer, USD/h", Kind::Num, "70").range(0.0, 10_000.0),
            f("delivery.roles.qa.mult", "QA engineer, multiplier", Kind::Num, "2.2").range(0.5, 10.0),
            f("delivery.roles.devops.rate", "DevOps and SRE, USD/h", Kind::Num, "90").range(0.0, 10_000.0),
            f("delivery.roles.devops.mult", "DevOps and SRE, multiplier", Kind::Num, "2.0").range(0.5, 10.0),
            f("delivery.roles.reviewer.rate", "Code reviewer, USD/h", Kind::Num, "95").range(0.0, 10_000.0),
            f("delivery.roles.reviewer.mult", "Code reviewer, multiplier", Kind::Num, "1.5").range(0.5, 10.0),
            f("delivery.roles.planner.rate", "Technical lead, USD/h", Kind::Num, "120").range(0.0, 10_000.0),
            f("delivery.roles.planner.mult", "Technical lead, multiplier", Kind::Num, "1.6").range(0.5, 10.0),
            f("delivery.roles.writer.rate", "Technical writer, USD/h", Kind::Num, "60").range(0.0, 10_000.0),
            f("delivery.roles.writer.mult", "Technical writer, multiplier", Kind::Num, "1.8").range(0.5, 10.0),
            f("delivery.kinds.mode.minutes", "Classification, minutes", Kind::Num, "0.5").range(0.0, 480.0),
            f("delivery.kinds.goal.minutes", "Goal check, minutes", Kind::Num, "3").range(0.0, 480.0),
            f("delivery.kinds.plan.minutes", "Plan review, minutes", Kind::Num, "4").range(0.0, 480.0),
            f("delivery.kinds.triage.minutes", "Triage, minutes", Kind::Num, "6").range(0.0, 480.0),
            f("delivery.kinds.merge.minutes", "Merge readiness, minutes", Kind::Num, "8").range(0.0, 480.0),
            f("delivery.kinds.other.minutes", "Other decisions, minutes", Kind::Num, "2").range(0.0, 480.0),
        ],
    },
];

/// Settings that are saved and have a typed reader ([`Effective`]) but no
/// consumer yet: nothing outside this file reads them (a test walks the
/// workspace to prove it), so remove a key here in the change that wires
/// it. The Settings screen marks them.
pub const PENDING: &[&str] = &[
    "belai.mode",
    "belai.ask_permission",
    "autonomy.plan_approval",
    "audit.calls",
    "systemone.override",
    // The board sync that reads these is not built yet.
    "kanban.provider",
    "kanban.board",
    "kanban.sync",
    "kanban.lists",
];

/// Settings holding time windows ([`crate::control::parse_windows`]).
pub const WINDOW_KEYS: &[&str] = &[
    crate::control::keys::MAINTENANCE,
    crate::control::keys::BLACKOUT,
    keys::NOTIFY_QUIET,
];

/// Every setting, typed, with its default: the one reader other crates use
/// (`Effective::load(core)`), so no key is read with a different default in
/// two places.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effective {
    pub autonomy_max: String,
    pub autonomy_default: String,
    pub agent_mode: String,
    pub concurrency: i64,
    pub max_attempts: i64,
    pub claim_ttl_ms: i64,
    pub no_host_ttl_ms: i64,
    pub ask_permission: bool,
    pub plan_approval: bool,
    pub defaults_enabled: bool,
    pub sensitive_paths: Vec<String>,
    pub models_default: String,
    pub models_fast: String,
    pub github_poll: bool,
    pub poll_interval_s: i64,
    pub factory_share_pct: i64,
    pub reserve_pct: i64,
    pub branch_prefix: String,
    pub pr_ready: bool,
    pub protect_branches: bool,
    pub min_confidence: f64,
    pub signing_mode: String,
    pub signing_email: String,
    pub notify_desktop: bool,
    pub notify_approvals: bool,
    pub notify_questions: bool,
    pub notify_conflicts: bool,
    /// "escalations_holds_budget", "everything", "escalations_only".
    pub notify_events: String,
    pub schedules_tz: String,
    pub schedules_missed: String,
    pub retention: String,
    pub teleport_retention_hours: i64,
    pub audit_calls: bool,
    pub budget_on_exceed: String,
    pub budget_alerts: Vec<i64>,
    pub vault_auto_test: bool,
    pub events_retention_days: i64,
    pub models_agents: Vec<String>,
    // Crews and per-card agent budgets (sent with a dispatch).
    pub guardrails: String,
    pub tests_post_end: bool,
    pub tests_on_fail: String,
    pub show_internal_work: bool,
    pub auto_commit_per_task: bool,
    pub a2a_show: bool,
    pub crew_lease_ms: i64,
    pub card_max_cost_usd: f64,
    pub card_finalize_at_pct: i64,
    pub card_max_attempts: i64,
    pub card_max_continuations: i64,
    pub card_max_passes: i64,
    pub card_max_wall_ms: i64,
    pub card_max_iterations: i64,
    // System One.
    pub systemone_model: String,
    pub systemone_fallback: String,
    pub systemone_below_threshold: String,
    pub systemone_on_abstain: String,
    pub systemone_retention: String,
    pub systemone_override: bool,
    // Hosts and windows.
    pub run_on: String,
    pub run_on_fallback: String,
    pub maintenance: String,
    pub blackout: String,
    // GitHub quota.
    pub quota_on_exhausted: String,
    pub calls_per_hour: i64,
    pub proactive_writes_per_hour: i64,
    pub notify_quiet: String,
    pub notify_muted: bool,
    pub chat: String,
    pub chat_events: String,
    pub kanban_provider: String,
    pub kanban_board: String,
    pub kanban_sync: String,
    pub kanban_lists: Vec<String>,
}

impl Effective {
    /// Reads from a key lookup (missing or malformed keys read as defaults).
    pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        let v = |k: &str| -> Value {
            let f = field(k).expect("a declared setting");
            match get(k) {
                Some(raw) => decode(f, &raw),
                None => default_value(f),
            }
        };
        let s = |k: &str| v(k).as_str().unwrap_or("").to_string();
        let b = |k: &str| v(k).as_bool().unwrap_or(false);
        let i = |k: &str| v(k).as_i64().unwrap_or(0);
        let n = |k: &str| v(k).as_f64().unwrap_or(0.0);
        let list = |k: &str| -> Vec<String> {
            v(k).as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            autonomy_max: s("autonomy.max"),
            autonomy_default: s("autonomy.default"),
            agent_mode: s("belai.mode"),
            concurrency: i("autonomy.concurrency"),
            max_attempts: i("autonomy.max_attempts"),
            claim_ttl_ms: i("autonomy.claim_ttl_minutes") * 60_000,
            no_host_ttl_ms: i("autonomy.no_host_ttl_hours") * 3_600_000,
            ask_permission: b("belai.ask_permission"),
            plan_approval: b("autonomy.plan_approval"),
            defaults_enabled: b("defaults.enabled"),
            sensitive_paths: list("approve.sensitive_paths"),
            models_default: s("models.default"),
            models_fast: s("models.fast"),
            github_poll: s("github.poll") != "off",
            poll_interval_s: i("github.poll_interval_s"),
            factory_share_pct: i("forge.quota.factory_share"),
            reserve_pct: i("forge.quota.reserve"),
            branch_prefix: s("forge.branch_prefix"),
            pr_ready: s("forge.pr_state") == "ready",
            protect_branches: b("forge.protected"),
            min_confidence: n("systemone.min_confidence"),
            signing_mode: s("signing.mode"),
            signing_email: s("signing.email"),
            notify_desktop: b(keys::NOTIFY_DESKTOP),
            notify_approvals: b(keys::NOTIFY_APPROVALS),
            notify_questions: b(keys::NOTIFY_QUESTIONS),
            notify_conflicts: b(keys::NOTIFY_CONFLICTS),
            notify_events: s("notify.events"),
            schedules_tz: s("schedules.tz"),
            schedules_missed: s("schedules.missed"),
            retention: s("persistence.retention"),
            teleport_retention_hours: i(super::keys::TELEPORT_RETENTION_HOURS),
            audit_calls: b("audit.calls"),
            budget_on_exceed: s("budget.on_exceed"),
            budget_alerts: v("budget.alerts")
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default(),
            vault_auto_test: b(crate::vault::probe::AUTO_TEST_SETTING),
            events_retention_days: i(crate::sweeps::keys::EVENTS_RETENTION_DAYS),
            models_agents: list(crate::vault::settings::MODELS_AGENTS),
            guardrails: s("belai.guardrails"),
            tests_post_end: b("belai.tests.post_end"),
            tests_on_fail: s("belai.tests.on_fail"),
            show_internal_work: b("belai.show_internal_work"),
            auto_commit_per_task: b("belai.auto_commit_per_task"),
            a2a_show: b("a2a.show"),
            crew_lease_ms: i("crews.lease") * 60_000,
            card_max_cost_usd: n("agents.max_cost_usd_per_card"),
            card_finalize_at_pct: i("agents.finalize_at"),
            card_max_attempts: i("agents.max_attempts"),
            card_max_continuations: i("agents.max_continuations"),
            card_max_passes: i("agents.max_passes"),
            card_max_wall_ms: i("agents.max_wall_minutes") * 60_000,
            card_max_iterations: i("agents.max_iterations"),
            systemone_model: s("systemone.model"),
            systemone_fallback: s("systemone.fallback"),
            systemone_below_threshold: s("systemone.below_threshold"),
            systemone_on_abstain: s("systemone.on_abstain"),
            systemone_retention: s("systemone.retention"),
            systemone_override: b("systemone.override"),
            run_on: s("hosts.run_on"),
            run_on_fallback: s("hosts.run_on_fallback"),
            maintenance: s(crate::control::keys::MAINTENANCE),
            blackout: s(crate::control::keys::BLACKOUT),
            quota_on_exhausted: s("forge.quota.on_exhausted"),
            calls_per_hour: i("forge.calls_per_hour"),
            proactive_writes_per_hour: i("forge.quota.proactive_writes_per_hour"),
            notify_quiet: s(keys::NOTIFY_QUIET),
            notify_muted: b(keys::NOTIFY_MUTED),
            chat: s(keys::CHAT),
            chat_events: s(keys::CHAT_EVENTS),
            kanban_provider: s("kanban.provider"),
            kanban_board: s("kanban.board"),
            kanban_sync: s("kanban.sync"),
            kanban_lists: list("kanban.lists"),
        }
    }

    pub async fn load(core: &Core) -> Result<Self> {
        let all: BTreeMap<String, String> =
            core.store().all_settings().await?.into_iter().collect();
        Ok(Self::resolve(|k| all.get(k).cloned()))
    }
}

/// Which notices `notify.events` lets through, by event kind.
pub fn notify_allows(level: &str, kind: &str) -> bool {
    let escalation = matches!(
        kind,
        "approval.requested"
            | "approval.renotified"
            | "session.question"
            | "budget.blocked"
            | "factory.mode"
            | "session.lost"
            | "github.quota.escalated"
    );
    let hold_or_budget = kind.starts_with("budget.")
        || matches!(kind, "run.held" | "factory.refused" | "run.notify")
        || kind == "run.failed";
    match level {
        "everything" => true,
        "escalations_only" => escalation,
        _ => escalation || hold_or_budget,
    }
}

/// Sections other crates register at start-up ([`register_extra`]).
static EXTRA: std::sync::RwLock<Vec<&'static Section>> = std::sync::RwLock::new(Vec::new());

impl Field {
    /// A field another crate registers ([`register_extra`]).
    pub const fn new(
        key: &'static str,
        label: &'static str,
        kind: Kind,
        default: &'static str,
    ) -> Field {
        f(key, label, kind, default)
    }
}

/// Registers a section of settings owned by another crate (the engine's
/// `defaults.<key>.enabled` switches), so the settings document, its saves
/// (audited like every key), YAML import and export, and the Settings
/// screen know them. A section id that exists (built in or registered)
/// gets the fields appended (its title and description stay); keys already
/// known are skipped, so a second registration is a no-op. Call at start-up
/// (registered fields are kept for the life of the process). Returns
/// whether any field was added.
pub fn register_extra(
    id: &'static str,
    title: &'static str,
    desc: &'static str,
    new: Vec<Field>,
) -> bool {
    let mut extra = EXTRA.write().unwrap_or_else(|e| e.into_inner());
    let known = |k: &str| {
        SECTIONS
            .iter()
            .chain(extra.iter().copied())
            .any(|s| s.fields.iter().any(|f| f.key == k))
    };
    let mut add: Vec<Field> = Vec::new();
    for f in new {
        if !known(f.key) && !add.iter().any(|a| a.key == f.key) {
            add.push(f);
        }
    }
    if add.is_empty() {
        return false;
    }
    let base = extra
        .iter()
        .copied()
        .find(|s| s.id == id)
        .or_else(|| SECTIONS.iter().find(|s| s.id == id));
    let (title, desc, mut fields) = match base {
        Some(s) => (s.title, s.desc, s.fields.to_vec()),
        None => (title, desc, Vec::new()),
    };
    fields.extend(add);
    let merged: &'static Section = Box::leak(Box::new(Section {
        id,
        title,
        desc,
        fields: Box::leak(fields.into_boxed_slice()),
    }));
    extra.retain(|s| s.id != id);
    extra.push(merged);
    true
}

/// Every section: the built-in [`SECTIONS`] (with registered fields
/// appended), then the sections only registered ones name.
pub fn sections() -> Vec<&'static Section> {
    let extra = EXTRA.read().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<&'static Section> = SECTIONS
        .iter()
        .map(|s| extra.iter().copied().find(|x| x.id == s.id).unwrap_or(s))
        .collect();
    out.extend(
        extra
            .iter()
            .copied()
            .filter(|x| !SECTIONS.iter().any(|s| s.id == x.id)),
    );
    out
}

pub fn fields() -> impl Iterator<Item = &'static Field> {
    sections().into_iter().flat_map(|s| s.fields.iter())
}

pub fn field(key: &str) -> Option<&'static Field> {
    fields().find(|f| f.key == key)
}

pub fn section_of(key: &str) -> Option<&'static Section> {
    sections()
        .into_iter()
        .find(|s| s.fields.iter().any(|f| f.key == key))
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// The value a stored text reads as.
pub fn decode(f: &Field, raw: &str) -> Value {
    let raw = raw.trim();
    match f.kind {
        Kind::Bool => Value::Bool(matches!(raw, "true" | "1" | "on" | "yes")),
        Kind::Int | Kind::Pct => raw
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| decode(f, f.default)),
        Kind::Num => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| decode(f, f.default)),
        Kind::StrList | Kind::GlobList | Kind::AddrList => {
            Value::Array(split_list(raw).into_iter().map(Value::String).collect())
        }
        Kind::IntList => Value::Array(
            split_list(raw)
                .iter()
                .filter_map(|x| x.parse::<i64>().ok())
                .map(Value::from)
                .collect(),
        ),
        _ => Value::String(raw.to_string()),
    }
}

/// The stored text for a canonical value.
pub fn encode(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                o => o.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}

/// The value a field reads as when nothing is stored.
pub fn default_value(f: &Field) -> Value {
    decode(f, f.default)
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

fn as_list(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(a) => a.iter().map(as_text).collect(),
        Value::Null => Some(Vec::new()),
        other => as_text(other).map(|s| split_list(&s)),
    }
}

fn check_range(f: &Field, n: f64) -> Result<(), String> {
    if f.max > f.min && (n < f.min || n > f.max) {
        return Err(format!("must be between {} and {}", f.min, f.max));
    }
    Ok(())
}

fn valid_host_port(s: &str) -> bool {
    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c))
        && port.parse::<u16>().is_ok_and(|p| p > 0)
}

fn valid_model_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c))
}

/// Checks a value and returns its canonical form.
pub fn validate(f: &Field, v: &Value) -> Result<Value, String> {
    match f.kind {
        Kind::ReadOnly => Err("is read-only".into()),
        Kind::Bool => match v {
            Value::Bool(b) => Ok(Value::Bool(*b)),
            Value::String(s) => match s.trim() {
                "true" | "on" | "yes" | "1" => Ok(Value::Bool(true)),
                "false" | "off" | "no" | "0" => Ok(Value::Bool(false)),
                _ => Err("must be true or false".into()),
            },
            _ => Err("must be true or false".into()),
        },
        Kind::Int | Kind::Pct => {
            let n = match v {
                Value::Number(n) => n.as_i64().ok_or("must be a whole number")?,
                Value::String(s) => s
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| "must be a whole number")?,
                _ => return Err("must be a whole number".into()),
            };
            check_range(f, n as f64)?;
            Ok(Value::from(n))
        }
        Kind::Num => {
            let n = match v {
                Value::Number(n) => n.as_f64().ok_or("must be a number")?,
                Value::String(s) => s.trim().parse::<f64>().map_err(|_| "must be a number")?,
                _ => return Err("must be a number".into()),
            };
            if !n.is_finite() {
                return Err("must be a number".into());
            }
            check_range(f, n)?;
            serde_json::Number::from_f64(n)
                .map(Value::Number)
                .ok_or_else(|| "must be a number".into())
        }
        Kind::Enum => {
            let s = as_text(v).ok_or("must be one of the choices")?;
            if f.options.iter().any(|(o, _)| *o == s) {
                Ok(Value::String(s))
            } else {
                Err(format!(
                    "must be one of {}",
                    f.options
                        .iter()
                        .map(|(o, _)| *o)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        Kind::Str => {
            let s = as_text(v).ok_or("must be text")?;
            let n = s.chars().count() as f64;
            if f.max > 0.0 && (n < f.min || n > f.max) {
                return Err(format!("must be {} to {} characters", f.min, f.max));
            }
            if s.contains(['\n', '\r', ',']) && f.key == "forge.branch_prefix" {
                return Err("must be one line without commas".into());
            }
            if f.key == "forge.branch_prefix"
                && !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
            {
                return Err("may hold letters, digits and / _ . - only".into());
            }
            if s.contains(['\n', '\r']) {
                return Err("must be one line".into());
            }
            if WINDOW_KEYS.contains(&f.key) {
                crate::control::parse_windows(&s)?;
            }
            if f.key == "hosts.run_on" && s != "any" && !s.starts_with("node:") {
                return Err("must be any or node:<id>".into());
            }
            Ok(Value::String(s))
        }
        Kind::Listen => {
            let s = as_text(v).ok_or("must be ip:port")?;
            s.parse::<std::net::SocketAddr>()
                .map_err(|_| "must be ip:port, for example 0.0.0.0:7443".to_string())?;
            Ok(Value::String(s))
        }
        Kind::AddrList => {
            let l = as_list(v).ok_or("must be host:port entries")?;
            if l.len() > 16 {
                return Err("at most 16 addresses".into());
            }
            if let Some(bad) = l.iter().find(|a| !valid_host_port(a)) {
                return Err(format!("{bad:?} is not host:port"));
            }
            Ok(Value::Array(l.into_iter().map(Value::String).collect()))
        }
        Kind::StrList | Kind::GlobList => {
            let l = as_list(v).ok_or("must be a list")?;
            if l.len() > 100 {
                return Err("at most 100 entries".into());
            }
            if l.iter().any(|x| x.chars().count() > 200) {
                return Err("an entry is longer than 200 characters".into());
            }
            Ok(Value::Array(l.into_iter().map(Value::String).collect()))
        }
        Kind::IntList => {
            let l = as_list(v).ok_or("must be whole numbers")?;
            let mut nums = Vec::new();
            for x in &l {
                nums.push(
                    x.parse::<i64>()
                        .map_err(|_| format!("{x:?} is not a whole number"))?,
                );
            }
            if nums.is_empty() || nums.windows(2).any(|w| w[1] <= w[0]) {
                return Err("must be ascending whole numbers, for example 50, 80, 100".into());
            }
            Ok(Value::Array(nums.into_iter().map(Value::from).collect()))
        }
        Kind::Tz => {
            let s = as_text(v).ok_or("must be a zone name")?;
            if !s.is_empty() && jiff::tz::TimeZone::get(&s).is_err() {
                return Err(format!("{s:?} is not an IANA zone name"));
            }
            Ok(Value::String(s))
        }
        Kind::Model => {
            let s = as_text(v).ok_or("must be <provider>/<model>")?;
            if s.is_empty() {
                return Ok(Value::String(s));
            }
            match s.split_once('/') {
                Some((p, m)) if valid_model_part(p) && valid_model_part(m) => Ok(Value::String(s)),
                _ => Err(
                    "must be <provider>/<model>, for example anthropic/claude-sonnet-5-5".into(),
                ),
            }
        }
    }
}

/// The settings document as stored now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsDoc {
    pub revision: i64,
    /// Every field's value (stored or default).
    pub values: BTreeMap<String, Value>,
    /// Keys with a stored value.
    pub stored: BTreeSet<String>,
    pub as_of: i64,
}

/// One field that failed validation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub key: String,
    pub message: String,
}

/// Why a save was refused. Carried in `anyhow::Error`.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsError {
    /// Someone saved since `revision` was read.
    Conflict {
        current: i64,
    },
    Unknown(Vec<String>),
    ReadOnly(Vec<String>),
    Invalid(Vec<Issue>),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Conflict { current } => write!(
                f,
                "the settings changed since you opened them (now revision {current}); reload to see them"
            ),
            SettingsError::Unknown(k) => write!(f, "unknown setting: {}", k.join(", ")),
            SettingsError::ReadOnly(k) => write!(f, "read-only setting: {}", k.join(", ")),
            SettingsError::Invalid(issues) => {
                let parts: Vec<String> = issues
                    .iter()
                    .map(|i| format!("{} {}", i.key, i.message))
                    .collect();
                write!(f, "{}", parts.join("; "))
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// A successful save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub revision: i64,
    /// Keys whose value changed, with the new value.
    pub changed: BTreeMap<String, Value>,
    /// Changed keys that take effect at the next start.
    pub restart: Vec<String>,
}

/// Keys whose draft value differs from `server`.
pub fn dirty_keys(
    server: &BTreeMap<String, Value>,
    draft: &BTreeMap<String, Value>,
) -> Vec<String> {
    draft
        .iter()
        .filter(|(k, v)| server.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect()
}

/// Checks `changes`: every key known and writable and every value valid.
/// Returns the canonical values.
pub fn check_changes(
    changes: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, SettingsError> {
    let unknown: Vec<String> = changes
        .keys()
        .filter(|k| field(k).is_none())
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(SettingsError::Unknown(unknown));
    }
    let ro: Vec<String> = changes
        .keys()
        .filter(|k| field(k).is_some_and(|f| f.kind == Kind::ReadOnly))
        .cloned()
        .collect();
    if !ro.is_empty() {
        return Err(SettingsError::ReadOnly(ro));
    }
    let mut out = BTreeMap::new();
    let mut issues = Vec::new();
    for (k, v) in changes {
        let f = field(k).expect("checked");
        match validate(f, v) {
            Ok(v) => {
                out.insert(k.clone(), v);
            }
            Err(message) => issues.push(Issue {
                key: k.clone(),
                message,
            }),
        }
    }
    if !issues.is_empty() {
        return Err(SettingsError::Invalid(issues));
    }
    Ok(out)
}

pub const YAML_API_VERSION: &str = "keleustes.vulnes.is/v1";
pub const YAML_KIND: &str = "FactorySettings";

/// Reads a YAML import: the document [`Core::settings_yaml`] writes
/// (`spec.<section>.<key>`), or a flat map of dotted keys. Read-only keys
/// are dropped. Returns key → raw value (not yet validated).
pub fn parse_yaml(text: &str) -> Result<BTreeMap<String, Value>> {
    let doc: Value = serde_norway::from_str(text)?;
    let Value::Object(top) = doc else {
        anyhow::bail!("the YAML must be a mapping");
    };
    let mut out = BTreeMap::new();
    let flat = |m: &serde_json::Map<String, Value>, out: &mut BTreeMap<String, Value>| {
        for (k, v) in m {
            match (field(k), v) {
                (Some(_), _) => {
                    out.insert(k.clone(), v.clone());
                }
                // A section: its keys.
                (None, Value::Object(inner)) if sections().iter().any(|s| s.id == k) => {
                    for (k2, v2) in inner {
                        out.insert(k2.clone(), v2.clone());
                    }
                }
                (None, _) => {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
    };
    match top.get("spec") {
        Some(Value::Object(spec)) => {
            if let Some(kind) = top.get("kind").and_then(|k| k.as_str())
                && kind != YAML_KIND
            {
                anyhow::bail!("kind must be {YAML_KIND}, not {kind}");
            }
            flat(spec, &mut out);
        }
        Some(_) => anyhow::bail!("spec must be a mapping"),
        None => {
            let mut m = top.clone();
            for k in ["apiVersion", "kind", "metadata"] {
                m.remove(k);
            }
            flat(&m, &mut out);
        }
    }
    out.retain(|k, _| field(k).is_none_or(|f| f.kind != Kind::ReadOnly));
    Ok(out)
}

impl Core {
    fn database_note(&self) -> String {
        match self.store().dialect() {
            kls_store::Dialect::Sqlite => format!(
                "SQLite at {}",
                self.data_dir().join("keleustes.db").display()
            ),
            kls_store::Dialect::Postgres => "Postgres (from --database-url)".into(),
        }
    }

    /// The settings document as stored now.
    pub async fn settings_doc(&self) -> Result<SettingsDoc> {
        let stored: BTreeMap<String, String> =
            self.store().all_settings().await?.into_iter().collect();
        let mut doc = SettingsDoc {
            revision: stored
                .get(kls_store::audit::SETTINGS_REVISION)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            as_of: now_ms(),
            ..Default::default()
        };
        for f in fields() {
            let v = if f.kind == Kind::ReadOnly {
                if f.key == keys::DATABASE {
                    Value::String(self.database_note())
                } else {
                    default_value(f)
                }
            } else if let Some(raw) = stored.get(f.key) {
                doc.stored.insert(f.key.to_string());
                decode(f, raw)
            } else {
                default_value(f)
            };
            doc.values.insert(f.key.to_string(), v);
        }
        Ok(doc)
    }

    /// Saves `changes` on top of `revision`. Unchanged values are skipped;
    /// each changed key is chained to the audit ledger.
    pub async fn settings_save(
        &self,
        revision: i64,
        changes: BTreeMap<String, Value>,
    ) -> Result<Saved> {
        self.settings_write(revision, changes, &BTreeSet::new(), "owner")
            .await
    }

    /// [`Self::settings_save`] recording `actor` (`owner:tray`) in the ledger.
    pub async fn settings_save_as(
        &self,
        revision: i64,
        changes: BTreeMap<String, Value>,
        actor: &str,
    ) -> Result<Saved> {
        self.settings_write(revision, changes, &BTreeSet::new(), actor)
            .await
    }

    /// `reset`: keys to remove (so they read as their default).
    async fn settings_write(
        &self,
        revision: i64,
        changes: BTreeMap<String, Value>,
        reset: &BTreeSet<String>,
        actor: &str,
    ) -> Result<Saved> {
        let changes = check_changes(&changes).map_err(anyhow::Error::new)?;
        let doc = self.settings_doc().await?;
        let mut writes = Vec::new();
        let mut drafts = Vec::new();
        let mut saved = Saved::default();
        for (k, new) in &changes {
            let old = doc.values.get(k).cloned().unwrap_or(Value::Null);
            let is_reset = reset.contains(k);
            // A reset removes a stored row; a value is written when it differs.
            let write = if is_reset {
                doc.stored.contains(k)
            } else {
                old != *new
            };
            if !write {
                continue;
            }
            writes.push((k.clone(), if is_reset { None } else { Some(encode(new)) }));
            if old != *new {
                let f = field(k).expect("checked");
                saved.changed.insert(k.clone(), new.clone());
                if f.restart {
                    saved.restart.push(k.clone());
                }
                drafts.push(LedgerDraft {
                    kind: "setting".into(),
                    actor: actor.into(),
                    actor_kind: "user".into(),
                    msg: format!("{k} {} to {}", show(&old), show(new)),
                    subject: json!({ "setting": k }),
                    payload: json!({ "old": old, "new": new, "revision": revision + 1 }),
                    ..Default::default()
                });
            }
        }
        if writes.is_empty() {
            saved.revision = doc.revision;
            if doc.revision != revision {
                return Err(SettingsError::Conflict {
                    current: doc.revision,
                }
                .into());
            }
            return Ok(saved);
        }
        match self
            .store()
            .save_settings(revision, &writes, &drafts)
            .await?
        {
            SettingsWrite::Conflict { current } => Err(SettingsError::Conflict { current }.into()),
            SettingsWrite::Saved { revision, entries } => {
                saved.revision = revision;
                self.emit(
                    super::kinds::SETTINGS_SAVED,
                    "settings",
                    &json!({ "revision": revision, "keys": saved.changed.keys().collect::<Vec<_>>() }),
                )
                .await?;
                if let Some(last) = entries.last() {
                    self.emit(
                        super::kinds::AUDIT_APPENDED,
                        "factory",
                        &json!({ "count": entries.len(), "seq": last.seq, "hash": last.hash }),
                    )
                    .await?;
                }
                Ok(saved)
            }
        }
    }

    /// The document as YAML: `apiVersion`, `kind`, `metadata`, and `spec`
    /// with one mapping per section. Read-only keys are left out.
    pub async fn settings_yaml(&self) -> Result<String> {
        let doc = self.settings_doc().await?;
        let mut spec = serde_json::Map::new();
        for s in sections() {
            let mut m = serde_json::Map::new();
            for f in s.fields.iter().filter(|f| f.kind != Kind::ReadOnly) {
                m.insert(f.key.into(), doc.values[f.key].clone());
            }
            if !m.is_empty() {
                spec.insert(s.id.into(), Value::Object(m));
            }
        }
        let v = json!({
            "apiVersion": YAML_API_VERSION,
            "kind": YAML_KIND,
            "metadata": { "server": self.server_name().await?, "revision": doc.revision, "exportedAt": now_ms() },
            "spec": spec,
        });
        Ok(serde_norway::to_string(&v)?)
    }

    /// What importing `yaml` would change: key → (current, imported).
    /// The import is a whole document: a key it leaves out goes back to its
    /// default (except the server name).
    pub async fn settings_import_plan(
        &self,
        yaml: &str,
    ) -> Result<BTreeMap<String, (Value, Value)>> {
        let (full, _) = self.import_values(yaml)?;
        let doc = self.settings_doc().await?;
        Ok(full
            .into_iter()
            .filter_map(|(k, v)| {
                let old = doc.values.get(&k).cloned().unwrap_or(Value::Null);
                (old != v).then_some((k, (old, v)))
            })
            .collect())
    }

    fn import_values(&self, yaml: &str) -> Result<(BTreeMap<String, Value>, BTreeSet<String>)> {
        let given = parse_yaml(yaml)?;
        let given = check_changes(&given).map_err(anyhow::Error::new)?;
        let mut full = BTreeMap::new();
        let mut reset = BTreeSet::new();
        for f in fields().filter(|f| f.kind != Kind::ReadOnly) {
            match given.get(f.key) {
                Some(v) => {
                    full.insert(f.key.to_string(), v.clone());
                }
                None if f.keep => {}
                None => {
                    full.insert(f.key.to_string(), default_value(f));
                    reset.insert(f.key.to_string());
                }
            }
        }
        Ok((full, reset))
    }

    /// Applies a YAML import on top of `revision`.
    pub async fn settings_import_yaml(&self, revision: i64, yaml: &str) -> Result<Saved> {
        let (full, reset) = self.import_values(yaml)?;
        self.settings_write(revision, full, &reset, "owner").await
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) if s.is_empty() => "(empty)".into(),
        Value::String(s) => s.clone(),
        o => encode(o),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::LedgerQuery;

    #[test]
    fn effective_reads_every_setting_with_its_default() {
        let d = Effective::resolve(|_| None);
        assert_eq!(d.autonomy_max, "draft_pr");
        assert_eq!(d.autonomy_default, "suggest");
        assert_eq!(d.agent_mode, "agent");
        assert_eq!(d.concurrency, 4);
        assert_eq!(d.max_attempts, 3);
        assert_eq!(d.claim_ttl_ms, 3_600_000);
        assert_eq!(d.no_host_ttl_ms, 6 * 3_600_000);
        assert!(!d.ask_permission && !d.plan_approval && d.defaults_enabled);
        assert!(d.sensitive_paths.is_empty());
        assert!(d.github_poll);
        assert_eq!(
            (d.poll_interval_s, d.factory_share_pct, d.reserve_pct),
            (120, 10, 20)
        );
        assert_eq!(d.branch_prefix, "belai/K-");
        assert!(!d.pr_ready && d.protect_branches);
        assert_eq!(d.min_confidence, 0.8);
        assert_eq!(
            (d.signing_mode.as_str(), d.signing_email.as_str()),
            ("preferred", "primary")
        );
        assert!(d.notify_desktop && d.notify_approvals && d.notify_questions && d.notify_conflicts);
        assert_eq!(d.notify_events, "escalations_holds_budget");
        assert_eq!(
            (d.schedules_tz.as_str(), d.schedules_missed.as_str()),
            ("", "run_once")
        );
        assert_eq!(d.retention, "400d");
        assert_eq!(d.teleport_retention_hours, 168);
        assert!(d.audit_calls);
        assert_eq!(d.budget_on_exceed, "pause_run");
        assert_eq!(d.budget_alerts, vec![50, 80, 100]);
        assert!(d.vault_auto_test);
        assert_eq!(d.events_retention_days, 30);

        let set: BTreeMap<&str, &str> = [
            ("autonomy.max", "observe"),
            ("autonomy.concurrency", "9"),
            ("autonomy.no_host_ttl_hours", "2"),
            ("approve.sensitive_paths", "infra/**, .github/**"),
            ("forge.pr_state", "ready"),
            ("github.poll", "off"),
            ("notify.events", "everything"),
            ("budget.alerts", "10, 90"),
            ("vault.auto_test", "false"),
            ("autonomy.concurrency", "9"),
        ]
        .into_iter()
        .collect();
        let e = Effective::resolve(|k| set.get(k).map(|s| s.to_string()));
        assert_eq!(e.autonomy_max, "observe");
        assert_eq!(e.concurrency, 9);
        assert_eq!(e.no_host_ttl_ms, 2 * 3_600_000);
        assert_eq!(e.sensitive_paths, vec!["infra/**", ".github/**"]);
        assert!(e.pr_ready && !e.github_poll && !e.vault_auto_test);
        assert_eq!(e.notify_events, "everything");
        assert_eq!(e.budget_alerts, vec![10, 90]);
        // Every pending key is a declared setting.
        for k in PENDING {
            assert!(field(k).is_some(), "{k}");
        }
    }

    /// The `Effective` field a key resolves into, read from this file's
    /// `resolve` body (`field: s("key")`).
    fn effective_field(src: &str, key: &str) -> Option<String> {
        let needle = format!("(\"{key}\")");
        src.lines()
            .find(|l| l.contains(&needle) && l.contains(':'))
            .and_then(|l| l.split(':').next())
            .map(|f| f.trim().to_string())
            .filter(|f| {
                !f.is_empty()
                    && f.chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            })
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    rust_files(&p, out);
                }
            } else if name.ends_with(".rs") {
                out.push(p);
            }
        }
    }

    /// Whether `text` uses `.field` as a whole identifier.
    fn uses_field(text: &str, field: &str) -> bool {
        let pat = format!(".{field}");
        text.match_indices(&pat).any(|(i, _)| {
            text[i + pat.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        })
    }

    /// `PENDING` is exactly "saved, no reader": no source file outside this
    /// one names the key or reads its `Effective` field.
    #[test]
    fn pending_keys_have_no_reader_outside_config() {
        let src = include_str!("config.rs");
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let mut files = Vec::new();
        rust_files(&crates, &mut files);
        assert!(files.len() > 50, "the workspace sources are readable");
        let me = std::path::Path::new("kls-core/src/audit/config.rs");
        let others: Vec<(String, String)> = files
            .iter()
            .filter(|p| !p.ends_with(me))
            .filter_map(|p| {
                Some((
                    p.strip_prefix(&crates).ok()?.display().to_string(),
                    std::fs::read_to_string(p).ok()?,
                ))
            })
            .collect();
        let mut read = Vec::new();
        for k in PENDING {
            let f = effective_field(src, k).unwrap_or_else(|| panic!("{k} has no Effective field"));
            let lit = format!("\"{k}\"");
            for (path, text) in &others {
                if text.contains(&lit) || (text.contains("Effective") && uses_field(text, &f)) {
                    read.push(format!("{k} ({f}) is read in {path}"));
                }
            }
        }
        assert!(read.is_empty(), "remove from PENDING:\n{}", read.join("\n"));
        // And the keys this list once flagged wrongly are wired.
        for k in [
            "autonomy.claim_ttl_minutes",
            "forge.quota.on_exhausted",
            "hosts.run_on",
        ] {
            assert!(!PENDING.contains(&k), "{k}");
        }
    }

    #[test]
    fn registered_sections_join_the_document_once() {
        let a = Field::new("test.extra.one", "One", Kind::Bool, "true");
        let b = Field::new("test.extra.two", "Two", Kind::Int, "3").help("Count");
        assert!(register_extra("test-extra", "Extra", "Test", vec![a]));
        assert!(!register_extra("test-extra", "Extra", "Test", vec![a]));
        assert!(register_extra(
            "test-extra",
            "Ignored",
            "Ignored",
            vec![a, b]
        ));
        let s = section_of("test.extra.two").unwrap();
        assert_eq!((s.id, s.title), ("test-extra", "Extra"));
        assert_eq!(s.fields.len(), 2);
        assert_eq!(
            sections().iter().filter(|s| s.id == "test-extra").count(),
            1
        );
        // A built-in key is never registered twice.
        let dup = Field::new("defaults.enabled", "Dup", Kind::Bool, "false");
        assert!(!register_extra("test-extra", "", "", vec![dup]));
        assert_eq!(field("defaults.enabled").unwrap().label, "Run the defaults");
        assert_eq!(decode(field("test.extra.two").unwrap(), "5"), json!(5));
    }

    #[test]
    fn notify_events_levels() {
        assert!(notify_allows("everything", "run.finished"));
        assert!(notify_allows("escalations_only", "approval.requested"));
        assert!(!notify_allows("escalations_only", "budget.alert"));
        assert!(notify_allows("escalations_holds_budget", "budget.alert"));
        assert!(!notify_allows("escalations_holds_budget", "session.ended"));
    }

    #[tokio::test]
    async fn rate_changes_are_audited_and_read_by_estimates() {
        let (c, _d) = core().await;
        let doc = c.settings_doc().await.unwrap();
        let mut ch = BTreeMap::new();
        ch.insert("delivery.roles.sse.rate".to_string(), json!(150));
        c.settings_save(doc.revision, ch).await.unwrap();
        let s = crate::estimates::EstimateSettings::load(&c).await.unwrap();
        assert_eq!(s.roles["sse"].rate, 150.0);
        let led = c
            .audit_entries(&LedgerQuery {
                kinds: vec!["setting".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            led.iter()
                .any(|e| e.subject["setting"] == "delivery.roles.sse.rate")
        );
    }

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    fn changes(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn schema_is_sound() {
        let mut seen = BTreeSet::new();
        for f in fields() {
            assert!(seen.insert(f.key), "duplicate key {}", f.key);
            if f.kind != Kind::ReadOnly && f.key != crate::settings::SERVER_NAME {
                let d = default_value(f);
                assert_eq!(validate(f, &d), Ok(d.clone()), "default of {}", f.key);
                assert_eq!(decode(f, &encode(&d)), d, "round trip of {}", f.key);
            }
        }
        assert!(field("ui.theme").is_some());
        assert_eq!(section_of("github.poll").unwrap().id, "github");
    }

    #[test]
    fn validation() {
        let v = |k: &str, x: Value| validate(field(k).unwrap(), &x);
        assert_eq!(v("ui.theme", json!("light")), Ok(json!("light")));
        assert!(v("ui.theme", json!("pink")).is_err());
        assert_eq!(v("github.poll_interval_s", json!("300")), Ok(json!(300)));
        assert!(v("github.poll_interval_s", json!(5)).is_err());
        assert!(v("forge.quota.factory_share", json!(1.5)).is_err());
        assert_eq!(v("belai.ask_permission", json!("on")), Ok(json!(true)));
        assert!(v("node.listen_addr", json!("nope")).is_err());
        assert!(v("node.listen_addr", json!("127.0.0.1:7443")).is_ok());
        assert_eq!(
            v("node.advertise_addrs", json!("10.0.0.5:7443, box.lan:7443")),
            Ok(json!(["10.0.0.5:7443", "box.lan:7443"]))
        );
        assert!(v("node.advertise_addrs", json!(["10.0.0.5"])).is_err());
        assert!(v("schedules.tz", json!("Australia/Melbourne")).is_ok());
        assert!(v("schedules.tz", json!("Mars/Olympus")).is_err());
        assert!(v("models.default", json!("anthropic/claude-sonnet-5-5")).is_ok());
        assert!(v("models.default", json!("claude")).is_err());
        assert_eq!(v("budget.alerts", json!("50 , 90")), Ok(json!([50, 90])));
        assert!(v("budget.alerts", json!("90, 50")).is_err());
        assert!(v("forge.branch_prefix", json!("bad prefix")).is_err());
        assert!(v(keys::DATABASE, json!("x")).is_err());
        assert_eq!(
            check_changes(&changes(&[("nope", json!(1))])),
            Err(SettingsError::Unknown(vec!["nope".into()]))
        );
    }

    #[tokio::test]
    async fn doc_save_revision_and_ledger() {
        let (c, _d) = core().await;
        let doc = c.settings_doc().await.unwrap();
        assert_eq!(doc.revision, 0);
        assert_eq!(doc.values["ui.theme"], json!("dark"));
        assert_eq!(doc.values["github.poll_interval_s"], json!(120));
        assert!(
            doc.values[keys::DATABASE]
                .as_str()
                .unwrap()
                .contains("SQLite")
        );
        assert!(doc.stored.contains("server.name"));

        let saved = c
            .settings_save(
                0,
                changes(&[
                    ("ui.theme", json!("light")),
                    ("node.advertise_addrs", json!("10.0.0.5:7443")),
                    ("node.listen_addr", json!("127.0.0.1:9000")),
                    ("github.poll", json!("on")),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(saved.revision, 1);
        // github.poll was already "on" (the default): not a change.
        assert_eq!(saved.changed.len(), 3);
        assert_eq!(saved.restart, vec!["node.listen_addr".to_string()]);
        let s = c.store();
        assert_eq!(
            s.setting("ui.theme").await.unwrap().as_deref(),
            Some("light")
        );
        // The format the node server reads.
        assert_eq!(
            s.setting("node.advertise_addrs").await.unwrap().as_deref(),
            Some("10.0.0.5:7443")
        );
        let entries = c
            .audit_entries(&LedgerQuery {
                kinds: vec!["setting".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);
        let theme = entries
            .iter()
            .find(|e| e.subject["setting"] == "ui.theme")
            .unwrap();
        assert_eq!(theme.payload["old"], "dark");
        assert_eq!(theme.payload["new"], "light");

        // Stale revision.
        let e = c
            .settings_save(0, changes(&[("ui.theme", json!("dark"))]))
            .await
            .unwrap_err();
        assert_eq!(
            e.downcast_ref::<SettingsError>(),
            Some(&SettingsError::Conflict { current: 1 })
        );
        // Invalid values name their key.
        let e = c
            .settings_save(1, changes(&[("github.poll_interval_s", json!(1))]))
            .await
            .unwrap_err();
        match e.downcast_ref::<SettingsError>() {
            Some(SettingsError::Invalid(i)) => assert_eq!(i[0].key, "github.poll_interval_s"),
            other => panic!("{other:?}"),
        }
        // Nothing changed: same revision.
        let same = c
            .settings_save(1, changes(&[("ui.theme", json!("light"))]))
            .await
            .unwrap();
        assert_eq!((same.revision, same.changed.len()), (1, 0));
        assert!(c.audit_verify().await.unwrap().ok);
    }

    #[tokio::test]
    async fn yaml_round_trip_and_import() {
        let (c, _d) = core().await;
        c.settings_save(
            0,
            changes(&[
                ("ui.theme", json!("light")),
                ("budget.alerts", json!("25, 75")),
            ]),
        )
        .await
        .unwrap();
        let yaml = c.settings_yaml().await.unwrap();
        assert!(yaml.contains("kind: FactorySettings"), "{yaml}");
        assert!(yaml.contains("ui.theme: light"), "{yaml}");
        assert!(!yaml.contains(keys::DATABASE));
        let parsed = parse_yaml(&yaml).unwrap();
        assert_eq!(parsed["ui.theme"], json!("light"));
        assert_eq!(parsed["budget.alerts"], json!([25, 75]));
        // Importing what was exported changes nothing.
        assert!(c.settings_import_plan(&yaml).await.unwrap().is_empty());

        // A flat import is a whole document: what it leaves out resets.
        let flat = "ui.theme: system\ngithub.poll_interval_s: 600\n";
        let plan = c.settings_import_plan(flat).await.unwrap();
        assert_eq!(plan["ui.theme"], (json!("light"), json!("system")));
        assert_eq!(plan["budget.alerts"].1, json!([50, 80, 100]));
        assert!(!plan.contains_key("server.name"));
        let saved = c.settings_import_yaml(1, flat).await.unwrap();
        assert_eq!(saved.revision, 2);
        let s = c.store();
        assert_eq!(
            s.setting("ui.theme").await.unwrap().as_deref(),
            Some("system")
        );
        assert_eq!(s.setting("budget.alerts").await.unwrap(), None);
        assert!(s.setting("server.name").await.unwrap().is_some());

        assert!(parse_yaml("- a\n- b\n").is_err());
        assert!(c.settings_import_yaml(2, "nope.key: 1\n").await.is_err());
        assert!(parse_yaml("kind: Other\nspec: {}\n").is_err());
        // Read-only keys in an import are ignored.
        assert!(parse_yaml("persistence.database: x\n").unwrap().is_empty());
    }

    #[test]
    fn dirty() {
        let server = changes(&[("a", json!(1)), ("b", json!("x"))]);
        let draft = changes(&[("a", json!(1)), ("b", json!("y"))]);
        assert_eq!(dirty_keys(&server, &draft), vec!["b".to_string()]);
    }
}
