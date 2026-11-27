//! Workflow validation: machine codes plus plain messages that name the
//! step by its title and say what to do (vdb-site docs/factory-validation.md).
//!
//! A save checks shape only ([`Mode::Save`]); publish and dry run add the
//! graph checks ([`Mode::Publish`]). Warnings and notes never block.

use crate::cron;
use crate::graph::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub code: String,
    pub level: Level,
    pub message: String,
    /// Where in the document: `nodes[2].config.cron`, `edges[0]`, `limits.concurrency`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Shape only (draft saves).
    Save,
    /// Everything (publish, dry run, templates).
    Publish,
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.level == Level::Error)
}

/// The 422 of the web factory: `{ "code": "invalid", "issues": [...] }`.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{}", summary(.0))]
pub struct Invalid(pub Vec<Issue>);

fn summary(issues: &[Issue]) -> String {
    let errs: Vec<&Issue> = issues.iter().filter(|i| i.level == Level::Error).collect();
    match errs.as_slice() {
        [] => "the workflow is valid".into(),
        [one] => one.message.clone(),
        [first, rest @ ..] => format!("{} (and {} more problems)", first.message, rest.len()),
    }
}

struct Ctx<'a> {
    g: &'a Graph,
    out: Vec<Issue>,
}

impl Ctx<'_> {
    fn push(&mut self, level: Level, code: &str, path: String, node: Option<&Node>, msg: String) {
        self.out.push(Issue {
            code: code.into(),
            level,
            message: msg,
            path,
            node_id: node.map(|n| n.id.clone()),
        });
    }
    fn err(&mut self, code: &str, path: String, node: Option<&Node>, msg: String) {
        self.push(Level::Error, code, path, node, msg)
    }
}

/// How a step is named in a message.
fn name(n: &Node) -> String {
    if n.title.trim().is_empty() {
        format!("step {}", n.id)
    } else {
        n.title.trim().to_string()
    }
}

fn title_of(g: &Graph, id: &str) -> String {
    g.node(id)
        .map(|n| format!("\"{}\"", name(n)))
        .unwrap_or_else(|| format!("\"{id}\""))
}

fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_outcome_name(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase())
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn valid_repo(r: &str) -> bool {
    if r == "*" {
        return true;
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match r.split_once('/') {
        Some((o, n)) => ok(o) && (n == "*" || ok(n)),
        None => false,
    }
}

pub fn validate(g: &Graph, mode: Mode) -> Vec<Issue> {
    let mut c = Ctx { g, out: Vec::new() };
    if g.nodes.len() > MAX_NODES {
        c.err(
            "too_many_nodes",
            "nodes".into(),
            None,
            format!("A workflow holds at most {MAX_NODES} steps. Remove some steps or split the work into two workflows."),
        );
    }
    if g.edges.len() > MAX_EDGES {
        c.err(
            "too_many_edges",
            "edges".into(),
            None,
            format!("A workflow holds at most {MAX_EDGES} connections between steps. Remove some connections or split the work into two workflows."),
        );
    }
    check_limits(&mut c, mode);
    for (i, r) in g.scope.repos.iter().enumerate() {
        if !valid_repo(r) {
            c.err(
                "invalid_repo",
                format!("scope.repos[{i}]"),
                None,
                format!("\"{r}\" is not a repository name. Write owner/name, owner/* for every repository of an owner, or * for all of them."),
            );
        }
    }
    let mut seen = HashSet::new();
    for (i, n) in g.nodes.iter().enumerate() {
        let p = format!("nodes[{i}]");
        if !valid_id(&n.id) {
            c.err(
                "invalid_id",
                format!("{p}.id"),
                Some(n),
                format!("The id of \"{}\" cannot be used. An id is 1 to 40 letters, digits, dashes or underscores.", name(n)),
            );
        }
        if !seen.insert(n.id.as_str()) {
            c.err(
                "duplicate_id",
                format!("{p}.id"),
                Some(n),
                format!("\"{}\" has the id \"{}\", which another step already uses. Give every step its own id.", name(n), n.id),
            );
        }
        if n.title.trim().is_empty() {
            c.err(
                "required",
                format!("{p}.title"),
                Some(n),
                format!("Give the step \"{}\" a title.", n.id),
            );
        } else if n.title.chars().count() > MAX_TITLE {
            c.err(
                "too_long",
                format!("{p}.title"),
                Some(n),
                format!(
                    "The title of \"{}\" is too long. Use at most {MAX_TITLE} characters.",
                    name(n)
                ),
            );
        }
        if let Some(b) = n.budget
            && b.tokens < 0
        {
            c.err(
                "out_of_range",
                format!("{p}.budget.tokens"),
                Some(n),
                format!(
                    "The token budget of \"{}\" cannot be negative. Enter 0 or more.",
                    name(n)
                ),
            );
        }
        if !NODE_TYPES.contains(&n.kind.as_str()) {
            c.err(
                "invalid_type",
                format!("{p}.type"),
                Some(n),
                format!(
                    "\"{}\" has a type that does not exist. Choose one of: {}.",
                    name(n),
                    NODE_TYPES.join(", ")
                ),
            );
            continue;
        }
        check_node(&mut c, n, &p, mode);
    }
    check_edges(&mut c, mode);
    if mode == Mode::Publish {
        check_graph(&mut c);
    }
    c.out
}

fn check_limits(c: &mut Ctx, mode: Mode) {
    let l = &c.g.limits;
    for (v, key) in [
        (l.run_budget_tokens, "runBudgetTokens"),
        (l.month_budget_tokens, "monthBudgetTokens"),
    ] {
        if v.is_some_and(|v| v < 0) {
            c.err(
                "out_of_range",
                format!("limits.{key}"),
                None,
                "A budget cannot be negative. Enter 0 or more for the run and month budgets."
                    .into(),
            );
        }
    }
    if l.concurrency.is_some_and(|v| !(1..=50).contains(&v)) {
        c.err(
            "out_of_range",
            "limits.concurrency".into(),
            None,
            "Concurrency is the number of runs at once, from 1 to 50.".into(),
        );
    }
    if l.stop_the_line_attempts
        .is_some_and(|v| !(1..=10).contains(&v))
    {
        c.err(
            "out_of_range",
            "limits.stopTheLineAttempts".into(),
            None,
            "Stop the line after is a number of failed attempts, from 1 to 10.".into(),
        );
    }
    if l.max_cost_usd_per_card
        .is_some_and(|v| !(0.0..=10_000.0).contains(&v))
    {
        c.err(
            "out_of_range",
            "limits.maxCostUsdPerCard".into(),
            None,
            "Cost per card is from 0 to 10,000 USD.".into(),
        );
    }
    for (v, key, lo, hi, what) in [
        (l.finalize_at_pct, "finalizeAtPct", 1, 100, "Finalize at"),
        (
            l.max_attempts_per_card,
            "maxAttemptsPerCard",
            1,
            20,
            "Attempts per card",
        ),
        (
            l.max_continuations,
            "maxContinuations",
            0,
            20,
            "Continuations per card",
        ),
        (l.max_passes, "maxPasses", 1, 20, "Review passes per card"),
        (
            l.max_wall_minutes,
            "maxWallMinutes",
            1,
            10_080,
            "Wall minutes per card",
        ),
        (
            l.max_iterations,
            "maxIterations",
            1,
            1000,
            "Tool rounds per pass",
        ),
    ] {
        if v.is_some_and(|v| !(lo..=hi).contains(&v)) {
            c.err(
                "out_of_range",
                format!("limits.{key}"),
                None,
                format!("{what} is from {lo} to {hi}."),
            );
        }
    }
    if mode == Mode::Publish {
        if let (Some(run), Some(month)) = (l.run_budget_tokens, l.month_budget_tokens)
            && month > 0
            && run > month
        {
            c.err(
                "budget_exceeds_parent",
                "limits.runBudgetTokens".into(),
                None,
                "The run budget is larger than the month budget. Lower the run budget or raise the month budget.".into(),
            );
        }
        if let Some(run) = l.run_budget_tokens.filter(|v| *v > 0) {
            for (i, n) in c.g.nodes.iter().enumerate() {
                if n.budget.is_some_and(|b| b.tokens > run) {
                    c.err(
                        "budget_exceeds_parent",
                        format!("nodes[{i}].budget.tokens"),
                        Some(n),
                        format!("The token budget of \"{}\" is larger than the whole run budget. Lower it or raise the run budget.", name(n)),
                    );
                }
            }
        }
    }
}

// ---- per-node config -------------------------------------------------------

fn enum_check(c: &mut Ctx, n: &Node, p: &str, key: &str, allowed: &[&str], label: &str) {
    match n.config.get(key) {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s.is_empty() || allowed.contains(&s.as_str()) => {}
        Some(Value::String(s)) => c.err(
            "invalid_enum",
            format!("{p}.config.{key}"),
            Some(n),
            format!(
                "In \"{}\", \"{s}\" is not accepted for {label}. Choose from: {}.",
                name(n),
                allowed.join(", ")
            ),
        ),
        Some(_) => c.err(
            "invalid_type",
            format!("{p}.config.{key}"),
            Some(n),
            format!(
                "In \"{}\", {label} is not set up correctly. Clear it and set it again.",
                name(n)
            ),
        ),
    }
}

fn range_check(c: &mut Ctx, n: &Node, p: &str, key: &str, lo: i64, hi: i64, label: &str) {
    match n.config.get(key) {
        None | Some(Value::Null) => {}
        Some(v) => match v.as_i64() {
            Some(x) if (lo..=hi).contains(&x) => {}
            _ => c.err(
                "out_of_range",
                format!("{p}.config.{key}"),
                Some(n),
                format!(
                    "In \"{}\", {label} must be a whole number from {lo} to {hi}.",
                    name(n)
                ),
            ),
        },
    }
}

fn text_check(c: &mut Ctx, n: &Node, p: &str, key: &str, max: usize, label: &str) {
    match n.config.get(key) {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => {
            if s.trim().chars().count() > max {
                c.err(
                    "too_long",
                    format!("{p}.config.{key}"),
                    Some(n),
                    format!(
                        "The {label} of \"{}\" is too long. Use at most {max} characters.",
                        name(n)
                    ),
                );
            }
        }
        Some(_) => c.err(
            "invalid_type",
            format!("{p}.config.{key}"),
            Some(n),
            format!(
                "In \"{}\", the {label} must be text. Clear it and write it again.",
                name(n)
            ),
        ),
    }
}

fn required(c: &mut Ctx, n: &Node, p: &str, key: &str, label: &str) {
    if n.str(key).trim().is_empty() {
        c.err(
            "required",
            format!("{p}.config.{key}"),
            Some(n),
            format!("In \"{}\", choose a value for {label}.", name(n)),
        );
    }
}

fn run_on_check(c: &mut Ctx, n: &Node, p: &str) {
    let v = n.str("runOn");
    let ok = v.is_empty()
        || v == "auto"
        || v.strip_prefix("node:")
            .is_some_and(|id| !id.trim().is_empty());
    if !ok {
        c.err(
            "runs_on_invalid",
            format!("{p}.config.runOn"),
            Some(n),
            format!(
                "In \"{}\", where the step runs is not valid. Choose Any node or one node.",
                name(n)
            ),
        );
    }
}

/// The start-field problems of a step (`answers`, `gates`, `adhoc` and the
/// other `kls_core::sessions::start::StartFields` keys) as
/// `(path under config, why)`. The engine refuses the same fields at
/// dispatch (`exec::start_fields`) and the workflow editor shows these
/// beside its form, so the form, a dry run and a run agree.
pub fn start_problems(n: &Node) -> Vec<(String, String)> {
    use kls_core::sessions::start::{StartFields, check_adhoc, check_answers, check_gate};
    let kind = match n.str("dispatch") {
        "" => "worker",
        k => k,
    };
    let mut f = match StartFields::from_config(&n.config) {
        Ok(f) => f,
        Err(why) => return vec![(String::new(), why)],
    };
    let mut out = Vec::new();
    let session = kind == "session";
    if !f.answers_json.is_empty() {
        let why = if session {
            check_answers(&f.answers_json).err()
        } else {
            Some("answers apply to a session".to_string())
        };
        if let Some(why) = why {
            out.push((".answers".to_string(), why));
        }
    }
    if let Some((k, why)) = f.gates.iter().find_map(|(k, v)| {
        let why = if session {
            check_gate(k, v).err()
        } else {
            Some("gates apply to a session".to_string())
        };
        why.map(|w| (k.clone(), w))
    }) {
        out.push((format!(".gates.{k}"), why));
    }
    if let Some(a) = &f.adhoc {
        let why = if kind != "worker" {
            Some("a one-off worker is a worker dispatch".to_string())
        } else if !n.str("agent").trim().is_empty() || !n.str("crew").trim().is_empty() {
            Some("a one-off worker names no agent profile".to_string())
        } else {
            check_adhoc(a).err()
        };
        if let Some(why) = why {
            out.push((".adhoc".to_string(), why));
        }
    }
    // Every other key: the same check the dispatch runs.
    f.answers_json.clear();
    f.gates.clear();
    f.adhoc = None;
    if let Err(why) = f.validate(kind) {
        out.push((String::new(), why));
    }
    out
}

/// A trigger's extra triggers (`also`, docs/factory-validation.md): a list
/// of trigger configs, none of them nested, each of a kind an extra
/// trigger may be and, on publish, with its one detail set.
fn also_check(c: &mut Ctx, n: &Node, p: &str, full: bool) {
    let list = match n.config.get("also") {
        None | Some(Value::Null) => return,
        Some(Value::Array(a)) => a,
        Some(_) => {
            return c.err(
                "invalid_type",
                format!("{p}.config.also"),
                Some(n),
                format!("In \"{}\", the extra triggers must be a list. Add them one by one under Also start on.", name(n)),
            );
        }
    };
    for (i, v) in list.iter().enumerate() {
        let at = format!("{p}.config.also[{i}]");
        let Some(o) = v.as_object() else {
            c.err(
                "invalid_type",
                at,
                Some(n),
                format!("In \"{}\", extra trigger {} is not set up correctly. Remove it and add it again.", name(n), i + 1),
            );
            continue;
        };
        if o.contains_key("also") {
            c.err(
                "invalid_type",
                format!("{at}.also"),
                Some(n),
                format!("In \"{}\", an extra trigger cannot have extra triggers of its own. Move them up to the trigger.", name(n)),
            );
        }
        let kind = o.get("kind").and_then(Value::as_str).unwrap_or("manual");
        if !ALSO_KINDS.contains(&kind) {
            c.err(
                "invalid_enum",
                format!("{at}.kind"),
                Some(n),
                format!(
                    "In \"{}\", extra trigger {} cannot be \"{kind}\". Choose from: {}.",
                    name(n),
                    i + 1,
                    ALSO_KINDS.join(", ")
                ),
            );
            continue;
        }
        let key = also_detail_key(kind);
        let detail = o.get(key).and_then(Value::as_str).unwrap_or("").trim();
        if full && !key.is_empty() && detail.is_empty() {
            c.err(
                "required",
                format!("{at}.{key}"),
                Some(n),
                format!(
                    "In \"{}\", extra trigger {} needs its {key}.",
                    name(n),
                    i + 1
                ),
            );
        }
        if kind == "kanban" && !detail.is_empty() && !CARD_STATUSES.contains(&detail) {
            c.err(
                "invalid_enum",
                format!("{at}.toList"),
                Some(n),
                format!(
                    "In \"{}\", \"{detail}\" is not accepted for the list the card enters. Choose from: {}.",
                    name(n),
                    CARD_STATUSES.join(", ")
                ),
            );
        }
    }
}

fn check_node(c: &mut Ctx, n: &Node, p: &str, mode: Mode) {
    let full = mode == Mode::Publish;
    match n.kind.as_str() {
        "trigger" => {
            enum_check(c, n, p, "kind", &TRIGGER_KINDS, "the trigger kind");
            for why in crate::triggers::problems(&Value::Object(n.config.clone())) {
                c.err(
                    "trigger_conditions",
                    format!("{p}.config.conditions"),
                    Some(n),
                    format!("In \"{}\", {why}.", name(n)),
                );
            }
            enum_check(
                c,
                n,
                p,
                "overlap",
                &OVERLAP,
                "what happens when a run is already going",
            );
            enum_check(c, n, p, "catchUp", &CATCH_UP, "missed schedules");
            also_check(c, n, p, full);
            if !full {
                return;
            }
            match n.trigger_kind() {
                "cron" => {
                    if let Err(e) = cron::parse(n.str("cron")) {
                        let msg = match e {
                            cron::CronError::Invalid => format!(
                                "In \"{}\", the schedule is not a valid cron expression. Use five fields: minute, hour, day of month, month and day of week.",
                                name(n)
                            ),
                            cron::CronError::Unsupported => format!(
                                "In \"{}\", the schedule sets both a day of the month and a day of the week. Set only one of them and leave the other as *.",
                                name(n)
                            ),
                            cron::CronError::Granularity => format!(
                                "In \"{}\", a schedule runs at most once a minute, so it has no seconds field. Use five fields: minute, hour, day of month, month and day of week.",
                                name(n)
                            ),
                        };
                        c.err(e.code(), format!("{p}.config.cron"), Some(n), msg);
                    }
                    if cron::zone(n.str("tz")).is_none() {
                        c.err(
                            "tz_invalid",
                            format!("{p}.config.tz"),
                            Some(n),
                            format!("In \"{}\", that is not a timezone name. Use a name such as UTC or Australia/Melbourne.", name(n)),
                        );
                    }
                }
                "kanban" => {
                    let lists: Vec<&str> = CARD_STATUSES.to_vec();
                    enum_check(c, n, p, "fromList", &lists, "the list the card leaves");
                    enum_check(c, n, p, "toList", &lists, "the list the card enters");
                }
                "event" => required(c, n, p, "event", "the event"),
                "webhook" => required(c, n, p, "event", "the GitHub event"),
                "label" if n.str("label").trim().is_empty() => {
                    c.err(
                        "required",
                        format!("{p}.config.label"),
                        Some(n),
                        format!(
                            "In \"{}\", name the label that starts the workflow.",
                            name(n)
                        ),
                    );
                }
                _ => {}
            }
        }
        "crew" => {
            enum_check(c, n, p, "dispatch", &DISPATCH_KINDS, "what to start");
            enum_check(c, n, p, "mode", &MODES, "the mode");
            enum_check(c, n, p, "await", &AWAIT, "what the step waits for");
            range_check(c, n, p, "maxAttempts", 1, 10, "attempts");
            range_check(
                c,
                n,
                p,
                "timeoutMinutes",
                1,
                10_080,
                "the time limit in minutes",
            );
            text_check(c, n, p, "brief", MAX_BRIEF, "brief");
            run_on_check(c, n, p);
            for (path, why) in start_problems(n) {
                c.err(
                    "invalid_start_fields",
                    format!("{p}.config{path}"),
                    Some(n),
                    format!("In \"{}\", {why}.", name(n)),
                );
            }
            let adhoc = n.config.get("adhoc").is_some_and(|a| !a.is_null());
            let crew = n.str("crew").trim().to_string();
            let agent = n.str("agent").trim().to_string();
            if !crew.is_empty() && !agent.is_empty() {
                c.err(
                    "invalid_type",
                    format!("{p}.config"),
                    Some(n),
                    format!(
                        "\"{}\" names both a crew and an agent. Choose one of them.",
                        name(n)
                    ),
                );
            }
            for (key, v) in [("crew", &crew), ("agent", &agent)] {
                if is_background_agent(v) {
                    c.push(
                        Level::Warn,
                        "agent_not_dispatchable",
                        format!("{p}.config.{key}"),
                        Some(n),
                        format!("In \"{}\", \"{v}\" runs as a background agent, not a worker, so a workflow cannot dispatch it. Choose a worker such as belai:builder or belai:reviewer.", name(n)),
                    );
                }
            }
            if !full {
                return;
            }
            match n.str("dispatch") {
                "session" => {
                    if n.str("brief").trim().is_empty() {
                        c.err(
                            "required",
                            format!("{p}.config.brief"),
                            Some(n),
                            format!(
                                "In \"{}\", write the brief: it is the session's prompt.",
                                name(n)
                            ),
                        );
                    }
                }
                "crew" if crew.is_empty() => c.err(
                    "crew_not_found",
                    format!("{p}.config.crew"),
                    Some(n),
                    format!(
                        "Choose the crew or agent that \"{}\" hands the work to.",
                        name(n)
                    ),
                ),
                "crew" => {}
                _ if crew.is_empty() && agent.is_empty() && !adhoc => c.err(
                    "crew_not_found",
                    format!("{p}.config.agent"),
                    Some(n),
                    format!(
                        "Choose the crew or agent that \"{}\" hands the work to.",
                        name(n)
                    ),
                ),
                _ => {}
            }
        }
        "job" => {
            range_check(
                c,
                n,
                p,
                "timeoutSeconds",
                1,
                86_400,
                "the time limit in seconds",
            );
            range_check(c, n, p, "maxAttempts", 1, 10, "attempts");
            run_on_check(c, n, p);
            if let Some(v) = n.config.get("payload")
                && !v.is_object()
                && !v.is_null()
            {
                c.err(
                    "invalid_type",
                    format!("{p}.config.payload"),
                    Some(n),
                    format!("In \"{}\", the payload is not set up correctly. Clear it and set it again.", name(n)),
                );
            }
            if full {
                required(c, n, p, "job", "the job");
            }
        }
        "provision" => {
            if full {
                required(c, n, p, "planId", "the host plan");
                required(c, n, p, "node", "the node");
                // A running workflow has no person to type the host name: it
                // is captured when the workflow is published.
                required(c, n, p, "confirmName", "the host name, typed to confirm");
            }
        }
        "scan" => {
            let job = n.str("job");
            if full && job.is_empty() {
                required(c, n, p, "job", "the CLI job");
            } else if !job.is_empty() && !crate::catalogue::cli_jobs().contains(&job) {
                c.err(
                    "invalid_enum",
                    format!("{p}.config.job"),
                    Some(n),
                    format!(
                        "In \"{}\", \"{job}\" is not a Vulnetix CLI job a runner claims.",
                        name(n)
                    ),
                );
            }
            run_on_check(c, n, p);
        }
        "condition" => check_condition(c, n, p, full),
        "decision" => {
            check_condition(c, n, p, full);
            let values = crate::decision::outcome_values(n);
            if full && values.len() < 2 {
                c.err(
                    "decision_outcomes",
                    format!("{p}.config.outcomes"),
                    Some(n),
                    format!(
                        "In \"{}\", give the decision at least two outcomes.",
                        name(n)
                    ),
                );
            }
            if full {
                required(c, n, p, "question", "the question");
            }
            if let Some(t) = n.config.get("threshold")
                && !t.as_f64().is_some_and(|t| (0.0..=1.0).contains(&t))
            {
                c.err(
                    "invalid_range",
                    format!("{p}.config.threshold"),
                    Some(n),
                    format!("In \"{}\", the threshold is a share from 0 to 1.", name(n)),
                );
            }
            enum_check(
                c,
                n,
                p,
                "belowThreshold",
                &crate::decision::BELOW_ACTIONS,
                "what happens below the threshold",
            );
            enum_check(
                c,
                n,
                p,
                "onAbstain",
                &crate::decision::ABSTAIN_ACTIONS,
                "what happens when the model abstains",
            );
            if n.str("belowThreshold") == "safest" && !values.iter().any(|v| v == n.str("safest")) {
                c.err(
                    "decision_safest",
                    format!("{p}.config.safest"),
                    Some(n),
                    format!("In \"{}\", choose which outcome is the safest.", name(n)),
                );
            }
            run_on_check(c, n, p);
        }
        "wait" => {
            range_check(c, n, p, "minutes", 0, 10_080, "the minutes to wait");
            range_check(
                c,
                n,
                p,
                "timeoutMinutes",
                1,
                10_080,
                "the time limit in minutes",
            );
            if full && n.int("minutes").is_none() && n.str("event").trim().is_empty() {
                c.err(
                    "required",
                    format!("{p}.config.minutes"),
                    Some(n),
                    format!(
                        "In \"{}\", choose how long to wait or the event to wait for.",
                        name(n)
                    ),
                );
            }
        }
        "human" => {
            range_check(c, n, p, "slaHours", 1, 720, "the hours to answer in");
            enum_check(
                c,
                n,
                p,
                "onTimeout",
                &ON_TIMEOUT,
                "what happens when nobody answers",
            );
            match n.config.get("approvers") {
                None | Some(Value::Null) => {
                    if full {
                        c.err(
                            "approver_missing",
                            format!("{p}.config.approvers"),
                            Some(n),
                            format!(
                                "Name who can approve in \"{}\". Add owner or a name.",
                                name(n)
                            ),
                        );
                    }
                }
                Some(Value::Array(a)) => {
                    if a.is_empty() && full {
                        c.err(
                            "approver_missing",
                            format!("{p}.config.approvers"),
                            Some(n),
                            format!(
                                "Name who can approve in \"{}\". Add owner or a name.",
                                name(n)
                            ),
                        );
                    }
                    for (i, v) in a.iter().enumerate() {
                        match v.as_str() {
                            Some(s) if !s.trim().is_empty() => {}
                            Some(_) => c.err(
                                "approver_missing",
                                format!("{p}.config.approvers[{i}]"),
                                Some(n),
                                format!("In \"{}\", approver {} is blank. Fill it in or remove it.", name(n), i + 1),
                            ),
                            None => c.err(
                                "invalid_type",
                                format!("{p}.config.approvers[{i}]"),
                                Some(n),
                                format!("In \"{}\", approver {} is not set up correctly. Remove it and add it again.", name(n), i + 1),
                            ),
                        }
                    }
                }
                Some(_) => c.err(
                    "invalid_type",
                    format!("{p}.config.approvers"),
                    Some(n),
                    format!("In \"{}\", approvers must be a list of names.", name(n)),
                ),
            }
        }
        "gate" => {
            enum_check(c, n, p, "check", &GATE_CHECKS, "how the gate is checked");
            enum_check(
                c,
                n,
                p,
                "onFail",
                &GATE_ON_FAIL,
                "what happens when it fails",
            );
        }
        "card" => {
            enum_check(c, n, p, "action", &CARD_ACTIONS, "the card action");
            enum_check(c, n, p, "status", &CARD_STATUSES, "the list");
            if full {
                match n.str("action") {
                    "" | "move" => required(c, n, p, "status", "the list"),
                    "label" | "unlabel" => required(c, n, p, "label", "the label"),
                    "assign" => required(c, n, p, "assignee", "who to assign"),
                    _ => {}
                }
            }
        }
        "read" => {
            enum_check(c, n, p, "source", &READ_SOURCES, "where it reads from");
            enum_check(c, n, p, "what", &READ_WHAT, "what it reads");
            enum_check(
                c,
                n,
                p,
                "onQuotaLow",
                &ON_QUOTA_LOW,
                "what happens when the live read fails",
            );
            if read_source(n) == "api" && read_what(n) != "pull" {
                c.err(
                    "invalid_enum",
                    format!("{p}.config.what"),
                    Some(n),
                    format!(
                        "In \"{}\", a live read refreshes the pull request only. Read the repository or issues from the mirror.",
                        name(n)
                    ),
                );
            }
        }
        "notify" => {
            enum_check(c, n, p, "severity", &SEVERITIES, "the severity");
            match n.config.get("message") {
                Some(Value::String(s)) if s.chars().count() > MAX_MESSAGE => c.err(
                    "too_long",
                    format!("{p}.config.message"),
                    Some(n),
                    format!(
                        "In \"{}\", the message must be text of at most {MAX_MESSAGE} characters.",
                        name(n)
                    ),
                ),
                Some(Value::String(_)) | None | Some(Value::Null) => {}
                Some(_) => c.err(
                    "too_long",
                    format!("{p}.config.message"),
                    Some(n),
                    format!(
                        "In \"{}\", the message must be text of at most {MAX_MESSAGE} characters.",
                        name(n)
                    ),
                ),
            }
            if full {
                required(c, n, p, "message", "the message");
            }
        }
        "github" => {
            enum_check(c, n, p, "action", &GITHUB_ACTIONS, "the GitHub action");
            if full {
                required(c, n, p, "action", "the GitHub action");
            }
        }
        _ => {}
    }
}

fn check_condition(c: &mut Ctx, n: &Node, p: &str, full: bool) {
    let outcomes = match n.config.get("outcomes") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => match serde_json::from_value::<Vec<Outcome>>(v.clone()) {
            Ok(o) => o,
            Err(_) => {
                c.err(
                    "invalid_type",
                    format!("{p}.config.outcomes"),
                    Some(n),
                    format!("In \"{}\", the outcomes are not set up correctly. Clear them and set them again.", name(n)),
                );
                return;
            }
        },
    };
    let mut names = HashSet::new();
    for (i, o) in outcomes.iter().enumerate() {
        if !valid_outcome_name(&o.value) {
            c.err(
                "invalid_outcome",
                format!("{p}.config.outcomes[{i}].value"),
                Some(n),
                format!("In \"{}\", an outcome name uses lowercase letters, digits and underscores, and starts with a letter. For example needs_review.", name(n)),
            );
        }
        if !names.insert(o.value.as_str()) {
            c.err(
                "duplicate_outcome",
                format!("{p}.config.outcomes[{i}].value"),
                Some(n),
                format!(
                    "In \"{}\", two outcomes are called \"{}\". Rename one of them.",
                    name(n),
                    o.value
                ),
            );
        }
    }
    let rules = match n.config.get("rules") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => match serde_json::from_value::<Vec<Rule>>(v.clone()) {
            Ok(r) => r,
            Err(_) => {
                c.err(
                    "invalid_type",
                    format!("{p}.config.rules"),
                    Some(n),
                    format!("In \"{}\", the rules are not set up correctly. Clear them and set them again.", name(n)),
                );
                Vec::new()
            }
        },
    };
    for (i, r) in rules.iter().enumerate() {
        if !RULE_OPS.contains(&r.op.as_str()) {
            c.err(
                "invalid_enum",
                format!("{p}.config.rules[{i}].op"),
                Some(n),
                format!(
                    "In \"{}\", \"{}\" is not accepted for the comparison. Choose from: {}.",
                    name(n),
                    r.op,
                    RULE_OPS.join(", ")
                ),
            );
        }
        if full && r.path.trim().is_empty() {
            c.err(
                "required",
                format!("{p}.config.rules[{i}].path"),
                Some(n),
                format!(
                    "In \"{}\", choose a value for what rule {} looks at.",
                    name(n),
                    i + 1
                ),
            );
        }
        if full && !names.contains(r.outcome.as_str()) {
            c.err(
                "invalid_outcome",
                format!("{p}.config.rules[{i}].outcome"),
                Some(n),
                format!(
                    "\"{}\" has no outcome called \"{}\". Choose one of its outcomes for rule {}.",
                    name(n),
                    r.outcome,
                    i + 1
                ),
            );
        }
    }
    if !full {
        return;
    }
    if outcomes.is_empty() {
        c.err(
            "required",
            format!("{p}.config.outcomes"),
            Some(n),
            format!("Add at least one outcome to \"{}\". An outcome is one of the answers the condition can give.", name(n)),
        );
    }
    let default = n.str("default");
    if n.kind == "decision" {
        // A decision has no rules or default: the model picks.
        return;
    }
    if default.is_empty() {
        c.err(
            "required",
            format!("{p}.config.default"),
            Some(n),
            format!(
                "In \"{}\", choose a value for the outcome when no rule matches.",
                name(n)
            ),
        );
    } else if !names.contains(default) {
        c.err(
            "invalid_outcome",
            format!("{p}.config.default"),
            Some(n),
            format!("\"{}\" has no outcome called \"{default}\". Choose one of its outcomes for when no rule matches.", name(n)),
        );
    }
}

// ---- edges and graph -------------------------------------------------------

fn check_edges(c: &mut Ctx, mode: Mode) {
    let g = c.g;
    let mut seen = HashSet::new();
    for (i, e) in g.edges.iter().enumerate() {
        let p = format!("edges[{i}]");
        let from = g.node(&e.from);
        let to = g.node(&e.to);
        if from.is_none() {
            c.err(
                "unknown_node",
                format!("{p}.from"),
                None,
                format!("A connection starts at a step that is not in the workflow (\"{}\"). Remove the connection.", e.from),
            );
        }
        if to.is_none() {
            c.err(
                "unknown_node",
                format!("{p}.to"),
                None,
                format!("A connection ends at a step that is not in the workflow (\"{}\"). Remove the connection.", e.to),
            );
        }
        if e.from == e.to {
            c.err(
                "cycle",
                p.clone(),
                from,
                format!(
                    "{} is connected to itself. Remove that connection.",
                    title_of(g, &e.from)
                ),
            );
        }
        if e.label.chars().count() > MAX_EDGE_LABEL {
            c.err(
                "too_long",
                format!("{p}.label"),
                from,
                format!("The label on the connection from {} to {} is too long. Use at most {MAX_EDGE_LABEL} characters.", title_of(g, &e.from), title_of(g, &e.to)),
            );
        }
        if !seen.insert((e.from.as_str(), e.to.as_str(), e.outcome.as_str())) {
            c.err(
                "duplicate_edge",
                p.clone(),
                from,
                format!("{} is connected to {} twice for the same outcome. Remove one of the connections.", title_of(g, &e.from), title_of(g, &e.to)),
            );
        }
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        if !e.outcome.is_empty() {
            match from.kind.as_str() {
                "condition" | "gate" | "decision" => {
                    if mode == Mode::Publish && !from.outcomes().iter().any(|o| o.value == e.outcome) {
                        c.err(
                            "invalid_outcome",
                            format!("{p}.outcome"),
                            Some(from),
                            format!("{} has no outcome called \"{}\". Choose one of its outcomes for the connection to {}.", title_of(g, &e.from), e.outcome, title_of(g, &e.to)),
                        );
                    }
                }
                _ => c.err(
                    "invalid_outcome",
                    format!("{p}.outcome"),
                    Some(from),
                    format!("The connection from {} to {} names an outcome, but only a condition or a gate has outcomes. Clear the outcome.", title_of(g, &e.from), title_of(g, &e.to)),
                ),
            }
        }
        if mode == Mode::Publish {
            if to.kind == "trigger" {
                c.err(
                    "invalid_edge",
                    p.clone(),
                    Some(to),
                    format!(
                        "Nothing can lead into the trigger {}. Remove the connection from {}.",
                        title_of(g, &e.to),
                        title_of(g, &e.from)
                    ),
                );
            }
            if e.is_loop
                && !(matches!(from.kind.as_str(), "gate" | "condition") && to.kind == "crew")
            {
                c.err(
                    "invalid_loop",
                    p.clone(),
                    Some(from),
                    format!("Only a gate or a condition can send work back to a crew. The connection from {} to {} is not one of those, so it cannot be a loop.", title_of(g, &e.from), title_of(g, &e.to)),
                );
            }
        }
    }
}

fn check_graph(c: &mut Ctx) {
    let g = c.g;
    if g.triggers().next().is_none() {
        c.err(
            "no_trigger",
            "nodes".into(),
            None,
            "Add a trigger. A workflow needs something to start it.".into(),
        );
    }
    // Reachability from the triggers, loop edges included.
    let mut reached: HashSet<&str> = g.triggers().map(|n| n.id.as_str()).collect();
    let mut queue: VecDeque<&str> = reached.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        for e in g.outgoing(id) {
            if reached.insert(e.to.as_str()) {
                queue.push_back(e.to.as_str());
            }
        }
    }
    for (i, n) in g.nodes.iter().enumerate() {
        if n.kind != "trigger" && !reached.contains(n.id.as_str()) && g.triggers().next().is_some()
        {
            c.err(
                "unreachable_node",
                format!("nodes[{i}]"),
                Some(n),
                format!("No trigger leads to \"{}\". Connect it to a step that a trigger reaches, or remove it.", name(n)),
            );
        }
    }
    // Acyclic without loop edges.
    if let Some(at) = find_cycle(g) {
        let n = g.node(&at);
        c.err(
            "cycle",
            "edges".into(),
            n,
            format!("The steps loop back on themselves around {}, so a run would never finish. Only a gate or a condition may send work back to a crew. Remove the connection that closes the loop.", title_of(g, &at)),
        );
    }
    // Every outcome needs a route unless it ends the run.
    for (i, n) in g.nodes.iter().enumerate() {
        if n.kind == "condition" || n.kind == "decision" {
            for (j, o) in n.outcomes().iter().enumerate() {
                if o.end || !valid_outcome_name(&o.value) {
                    continue;
                }
                if !g.outgoing(&n.id).any(|e| e.outcome == o.value) {
                    c.err(
                        "outcome_without_route",
                        format!("nodes[{i}].config.outcomes[{j}]"),
                        Some(n),
                        format!("In \"{}\", the outcome \"{}\" has no next step. Choose a step for it to continue to, or choose \"End the run here\".", name(n), o.value),
                    );
                }
            }
        }
        if n.kind == "gate"
            && n.str("onFail") == "return_to_crew"
            && !g
                .outgoing(&n.id)
                .any(|e| e.is_loop || e.outcome == OUTCOME_FAIL)
        {
            c.err(
                "outcome_without_route",
                format!("nodes[{i}].config.onFail"),
                Some(n),
                format!("In \"{}\", the outcome \"fail\" has no next step. Connect it back to a crew as a loop, or choose to end the run.", name(n)),
            );
        }
    }
}

/// A node on a cycle of the graph without loop edges, if any.
pub fn find_cycle(g: &Graph) -> Option<String> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in g.edges.iter().filter(|e| !e.is_loop && e.from != e.to) {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    // 0 = new, 1 = on stack, 2 = done
    let mut state: HashMap<&str, u8> = HashMap::new();
    for n in &g.nodes {
        if state.get(n.id.as_str()).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(n.id.as_str(), 0)];
        state.insert(n.id.as_str(), 1);
        while let Some((id, idx)) = stack.pop() {
            let next = adj.get(id).and_then(|v| v.get(idx)).copied();
            match next {
                Some(to) => {
                    stack.push((id, idx + 1));
                    match state.get(to).copied().unwrap_or(0) {
                        0 => {
                            state.insert(to, 1);
                            stack.push((to, 0));
                        }
                        1 => return Some(to.to_string()),
                        _ => {}
                    }
                }
                None => {
                    state.insert(id, 2);
                }
            }
        }
    }
    None
}

/// Node ids in topological order (loop edges ignored); nodes on a cycle
/// come last in graph order.
pub fn topo_order(g: &Graph) -> Vec<String> {
    let mut indeg: HashMap<&str, usize> = g.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    for e in g.edges.iter().filter(|e| !e.is_loop) {
        if let Some(d) = indeg.get_mut(e.to.as_str()) {
            *d += 1;
        }
    }
    let mut ready: VecDeque<&str> = g
        .nodes
        .iter()
        .filter(|n| indeg[n.id.as_str()] == 0)
        .map(|n| n.id.as_str())
        .collect();
    let mut out = Vec::new();
    let mut done = HashSet::new();
    while let Some(id) = ready.pop_front() {
        if !done.insert(id) {
            continue;
        }
        out.push(id.to_string());
        for e in g.outgoing(id).filter(|e| !e.is_loop) {
            if let Some(d) = indeg.get_mut(e.to.as_str()) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    ready.push_back(e.to.as_str());
                }
            }
        }
    }
    for n in &g.nodes {
        if !done.contains(n.id.as_str()) {
            out.push(n.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, kind: &str, config: Value) -> Node {
        Node {
            id: id.into(),
            kind: kind.into(),
            title: format!("Step {id}"),
            config: config.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            ..Default::default()
        }
    }

    fn codes(g: &Graph, mode: Mode) -> Vec<String> {
        let mut v: Vec<String> = validate(g, mode)
            .into_iter()
            .filter(|i| i.level == Level::Error)
            .map(|i| i.code)
            .collect();
        v.sort();
        v
    }

    fn good() -> Graph {
        Graph {
            nodes: vec![
                node(
                    "n1",
                    "trigger",
                    json!({"kind":"cron","cron":"0 3 * * 1-5","tz":"Australia/Melbourne"}),
                ),
                node(
                    "n2",
                    "crew",
                    json!({"dispatch":"worker","agent":"belai:builder","maxAttempts":2}),
                ),
                node(
                    "n3",
                    "condition",
                    json!({
                    "rules":[{"path":"steps.n2.status","op":"eq","value":"ok","outcome":"ship"}],
                    "default":"stop",
                    "outcomes":[{"value":"ship"},{"value":"stop","end":true}]}),
                ),
                node("n4", "human", json!({"approvers":["owner"],"slaHours":24})),
                node("n5", "notify", json!({"message":"done","severity":"info"})),
            ],
            edges: vec![
                edge("n1", "n2"),
                edge("n2", "n3"),
                Edge {
                    outcome: "ship".into(),
                    ..edge("n3", "n4")
                },
                edge("n4", "n5"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_good_graph_is_clean() {
        assert!(
            validate(&good(), Mode::Publish).is_empty(),
            "{:?}",
            validate(&good(), Mode::Publish)
        );
        assert!(validate(&Graph::starter(), Mode::Publish).is_empty());
    }

    #[test]
    fn no_trigger_and_unreachable() {
        let mut g = good();
        g.nodes.remove(0);
        g.edges.remove(0);
        assert_eq!(codes(&g, Mode::Publish), ["no_trigger"]);
        let mut g = good();
        g.edges.remove(0);
        // n2 unreachable, and so is everything after it.
        assert_eq!(
            codes(&g, Mode::Publish),
            [
                "unreachable_node",
                "unreachable_node",
                "unreachable_node",
                "unreachable_node"
            ]
        );
        // Save checks shape only.
        assert!(codes(&g, Mode::Save).is_empty());
    }

    #[test]
    fn cycles_and_loops() {
        let mut g = good();
        g.edges.push(edge("n5", "n2"));
        assert_eq!(codes(&g, Mode::Publish), ["cycle"]);
        // A loop edge from a condition to a crew is allowed.
        let mut g = good();
        g.edges.push(Edge {
            outcome: "stop".into(),
            is_loop: true,
            ..edge("n3", "n2")
        });
        assert!(codes(&g, Mode::Publish).is_empty());
        // From a notify it is not.
        let mut g = good();
        g.edges.push(Edge {
            is_loop: true,
            ..edge("n5", "n2")
        });
        assert_eq!(codes(&g, Mode::Publish), ["invalid_loop"]);
        // Self edge is a shape problem.
        let mut g = good();
        g.edges.push(edge("n2", "n2"));
        assert_eq!(codes(&g, Mode::Save), ["cycle"]);
        assert!(find_cycle(&good()).is_none());
    }

    #[test]
    fn outcomes() {
        let mut g = good();
        // "ship" loses its route.
        g.edges.retain(|e| e.outcome != "ship");
        let c = codes(&g, Mode::Publish);
        assert!(c.contains(&"outcome_without_route".to_string()), "{c:?}");
        let mut g = good();
        g.edges.push(Edge {
            outcome: "nope".into(),
            ..edge("n3", "n5")
        });
        assert_eq!(codes(&g, Mode::Publish), ["invalid_outcome"]);
        let mut g = good();
        g.edges.push(Edge {
            outcome: "x".into(),
            ..edge("n2", "n5")
        });
        assert_eq!(codes(&g, Mode::Save), ["invalid_outcome"]);
        let mut g = good();
        g.nodes[2].config.insert(
            "outcomes".into(),
            json!([{"value":"Ship"},{"value":"stop","end":true},{"value":"stop"}]),
        );
        let c = codes(&g, Mode::Publish);
        assert!(c.contains(&"duplicate_outcome".into()));
        assert!(c.contains(&"invalid_outcome".into()));
        let mut g = good();
        g.nodes[2].config.insert("default".into(), json!("maybe"));
        assert_eq!(codes(&g, Mode::Publish), ["invalid_outcome"]);
    }

    #[test]
    fn cron_codes() {
        let set = |cron: &str, tz: &str| {
            let mut g = good();
            g.nodes[0].config.insert("cron".into(), json!(cron));
            g.nodes[0].config.insert("tz".into(), json!(tz));
            codes(&g, Mode::Publish)
        };
        assert_eq!(set("bad", "UTC"), ["cron_invalid"]);
        assert_eq!(set("0 0 3 * * *", "UTC"), ["cron_granularity"]);
        assert_eq!(set("0 3 1 * 1", "UTC"), ["cron_unsupported"]);
        assert_eq!(set("0 3 * * *", "Mars/Base"), ["tz_invalid"]);
    }

    #[test]
    fn shape_codes() {
        let mut g = good();
        g.nodes.push(node("n2", "teleport", json!({})));
        g.nodes
            .push(node("bad id!", "notify", json!({"severity":"loud"})));
        g.edges.push(edge("n1", "ghost"));
        g.limits.concurrency = Some(0);
        g.scope.repos = vec!["not a repo".into(), "acme/*".into()];
        let c = codes(&g, Mode::Save);
        for want in [
            "duplicate_id",
            "invalid_type",
            "invalid_id",
            "invalid_enum",
            "unknown_node",
            "out_of_range",
            "invalid_repo",
        ] {
            assert!(c.contains(&want.to_string()), "missing {want} in {c:?}");
        }
        let issues = validate(&g, Mode::Save);
        let enum_issue = issues.iter().find(|i| i.code == "invalid_enum").unwrap();
        assert_eq!(enum_issue.path, "nodes[6].config.severity");
        assert_eq!(enum_issue.node_id.as_deref(), Some("bad id!"));
    }

    #[test]
    fn step_rules() {
        let mut g = good();
        g.nodes[1]
            .config
            .insert("crew".into(), json!("belai:feature"));
        assert_eq!(codes(&g, Mode::Publish), ["invalid_type"]);
        let mut g = good();
        g.nodes[1].config.remove("agent");
        assert_eq!(codes(&g, Mode::Publish), ["crew_not_found"]);
        let mut g = good();
        g.nodes[1]
            .config
            .insert("agent".into(), json!("belai:triage"));
        assert!(codes(&g, Mode::Publish).is_empty(), "warning only");
        assert!(
            validate(&g, Mode::Publish)
                .iter()
                .any(|i| i.code == "agent_not_dispatchable" && i.level == Level::Warn)
        );
        let mut g = good();
        g.nodes[1]
            .config
            .insert("brief".into(), json!("x".repeat(MAX_BRIEF + 1)));
        assert_eq!(codes(&g, Mode::Save), ["too_long"]);
        let mut g = good();
        g.nodes[3].config.insert("approvers".into(), json!([]));
        assert_eq!(codes(&g, Mode::Publish), ["approver_missing"]);
        let mut g = good();
        g.nodes[3].config.insert("approvers".into(), json!(["  "]));
        assert_eq!(codes(&g, Mode::Publish), ["approver_missing"]);
        let mut g = good();
        g.nodes[1].config.insert("runOn".into(), json!("mars"));
        assert_eq!(codes(&g, Mode::Save), ["runs_on_invalid"]);
        let mut g = good();
        g.limits.run_budget_tokens = Some(10);
        g.limits.month_budget_tokens = Some(5);
        g.nodes[1].budget = Some(Budget { tokens: 20 });
        assert_eq!(
            codes(&g, Mode::Publish),
            ["budget_exceeds_parent", "budget_exceeds_parent"]
        );
        let mut g = good();
        g.edges.push(edge("n5", "n1"));
        let c = codes(&g, Mode::Publish);
        assert!(c.contains(&"invalid_edge".to_string()), "{c:?}");
    }

    #[test]
    fn gate_return_needs_loop() {
        let mut g = good();
        g.nodes.push(node(
            "n6",
            "gate",
            json!({"check":"manual","onFail":"return_to_crew"}),
        ));
        g.edges.push(edge("n4", "n6"));
        assert_eq!(codes(&g, Mode::Publish), ["outcome_without_route"]);
        g.edges.push(Edge {
            outcome: "fail".into(),
            is_loop: true,
            ..edge("n6", "n2")
        });
        assert!(codes(&g, Mode::Publish).is_empty());
    }

    /// Answers, gates and one-off workers: the validator refuses what the
    /// dispatch would refuse, at the key's path, so dry run and run agree.
    #[test]
    fn start_fields_answers_gates_adhoc() {
        let with = |cfg: Value| {
            let mut g = good();
            g.nodes[1].config = cfg.as_object().cloned().unwrap();
            g
        };
        let errs = |g: &Graph| -> Vec<(String, String)> {
            validate(g, Mode::Publish)
                .into_iter()
                .filter(|i| i.code == "invalid_start_fields")
                .map(|i| (i.path, i.message))
                .collect()
        };
        // A session with scripted answers and gates is fine.
        let ok = with(json!({"dispatch":"session","brief":"go",
            "answers":[{"match":"Proceed?","choose":1},{"skip":true},{"note":"use main"}],
            "gates":{"finish_review":"on","commit_mode":"ask"}}));
        assert!(errs(&ok).is_empty(), "{:?}", errs(&ok));
        // Answers and gates apply to a session only.
        let bad = with(json!({"dispatch":"worker","agent":"belai:builder",
            "answers":[{"choose":1}],"gates":{"lint_loop":"on"}}));
        let e = errs(&bad);
        assert!(
            e.iter().any(|(p, _)| p == "nodes[1].config.answers"),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|(p, _)| p == "nodes[1].config.gates.lint_loop"),
            "{e:?}"
        );
        // An unknown answer key and a loosening gate value.
        let bad = with(json!({"dispatch":"session","brief":"go",
            "answers":[{"pick":1}],"gates":{"code_mode":"on"}}));
        let e = errs(&bad);
        assert!(e.iter().any(|(_, m)| m.contains("unknown key")), "{e:?}");
        assert!(
            e.iter().any(|(_, m)| m.contains("code_mode takes off")),
            "{e:?}"
        );
        // A one-off worker needs no agent, and names none.
        let ok = with(json!({"dispatch":"worker",
            "adhoc":{"prompt":"triage the inbox","tools":["read","grep"],"claim_lists":["triage"],
                     "publish":"none","read_only":true}}));
        assert!(
            codes(&ok, Mode::Publish).is_empty(),
            "{:?}",
            codes(&ok, Mode::Publish)
        );
        let both = with(json!({"dispatch":"worker","agent":"belai:builder",
            "adhoc":{"prompt":"x"}}));
        assert!(
            errs(&both)
                .iter()
                .any(|(p, m)| p == "nodes[1].config.adhoc" && m.contains("names no agent"))
        );
        let empty = with(json!({"dispatch":"worker","adhoc":{"prompt":" "}}));
        assert!(
            errs(&empty)
                .iter()
                .any(|(_, m)| m.contains("needs a prompt"))
        );
        let crew = with(json!({"dispatch":"crew","crew":"c","adhoc":{"prompt":"x"}}));
        assert!(
            errs(&crew)
                .iter()
                .any(|(_, m)| m.contains("worker dispatch"))
        );
        // The validator and the dispatch check agree on every case.
        for g in [&ok, &both, &empty, &crew] {
            let n = &g.nodes[1];
            let kind = match n.str("dispatch") {
                "" => "worker",
                k => k,
            };
            let run = crate::exec::start_fields(n, kind).is_ok()
                && !(n.config.contains_key("adhoc") && !n.str("agent").is_empty());
            assert_eq!(run, start_problems(n).is_empty(), "{:?}", n.config);
        }
    }

    #[test]
    fn topo() {
        let order = topo_order(&good());
        assert_eq!(order, ["n1", "n2", "n3", "n4", "n5"]);
        let err = Invalid(validate(&Graph::default(), Mode::Publish));
        assert!(err.to_string().starts_with("Add a trigger"));
    }
}
