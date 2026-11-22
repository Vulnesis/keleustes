//! The executor and the scheduler. One task per process advances runs: it
//! wakes on bus events (`"wake": true` from [`crate::ops`], dispatch and
//! session updates, card moves and any event a trigger or wait step names),
//! on step timers and on cron fires. It reaches nodes only through
//! `core.gateway()`.

use crate::cron;
use crate::graph::{Graph, Node};
use crate::guard::{self, Origin};
use crate::ops::{self, StartOutcome, StartRun, TriggerInfo, new_id};
use crate::triggers::Facts;
use crate::walker::{self, Finish};
use anyhow::Result;
use kls_core::Core;
use kls_core::engine::kinds;
use kls_core::gateway::{DispatchSpec, JobNeeds};
use kls_store::engine::{
    ACTIVE_RUN_STATUSES, Approval, Run, RunStep, Schedule, StepUpdate, TriggerFire,
};
use kls_store::{Event, now_ms};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast::error::RecvError;

// ---- pluggable integrations ------------------------------------------------

/// What a pluggable step sees.
#[derive(Clone, Debug)]
pub struct StepContext {
    pub run: Run,
    pub node: Node,
    pub step: RunStep,
    /// The condition context: `{run, event, inputs, steps}`.
    pub context: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateResult {
    Pass(Value),
    Fail(String),
    /// Checks still running; ask again later.
    Pending,
}

/// GitHub (M4): `github` output steps, `forge` gates and the webhook and
/// label triggers (which M4 fires by calling [`ops::start_run`]).
#[async_trait::async_trait]
pub trait Forge: Send + Sync {
    async fn action(&self, ctx: &StepContext, action: &str) -> Result<Value>;
    async fn gate(&self, ctx: &StepContext) -> Result<GateResult>;
}

/// Board writes for `card` steps.
#[async_trait::async_trait]
pub trait Board: Send + Sync {
    /// `action`: `move` (config `status`), `label`, `unlabel` (`label`),
    /// `assign` (`assignee`).
    async fn apply(&self, card_id: &str, action: &str, config: &Value) -> Result<Value>;
}

#[derive(Clone, Default)]
pub struct Integrations {
    pub forge: Option<Arc<dyn Forge>>,
    pub board: Option<Arc<dyn Board>>,
}

/// The board of this process (M2's cards in the core).
pub struct CoreBoard(pub Core);

#[async_trait::async_trait]
impl Board for CoreBoard {
    async fn apply(&self, card_id: &str, action: &str, config: &Value) -> Result<Value> {
        let core = &self.0;
        let card = core
            .card(card_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("card {card_id} not found"))?;
        let s = |k: &str| {
            config
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let actor = "engine";
        let c = match action {
            "" | "move" => {
                core.move_card(&card.id, card.version, &s("status"), actor)
                    .await?
            }
            "label" => {
                let mut labels = card.labels.clone();
                let l = s("label");
                if !labels.contains(&l) {
                    labels.push(l);
                }
                core.set_card_labels(&card.id, card.version, labels, actor)
                    .await?
            }
            "unlabel" => {
                let l = s("label");
                let labels: Vec<String> =
                    card.labels.iter().filter(|x| **x != l).cloned().collect();
                core.set_card_labels(&card.id, card.version, labels, actor)
                    .await?
            }
            "assign" => {
                core.assign_card(&card.id, card.version, &s("assignee"), actor)
                    .await?
            }
            other => anyhow::bail!("unknown card action {other}"),
        };
        Ok(
            json!({ "cardId": c.id, "key": c.key, "status": c.status, "labels": c.labels, "assignee": c.assignee }),
        )
    }
}

// ---- conditions ------------------------------------------------------------

/// `a.b.0.c` into a JSON value.
pub fn lookup<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        cur = match cur {
            Value::Object(m) => m.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::String(s), other) | (other, Value::String(s)) => match other {
            Value::Number(n) => s.parse::<f64>().ok() == n.as_f64(),
            Value::Bool(bl) => s == &bl.to_string(),
            _ => false,
        },
        _ => false,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn rule_matches(op: &str, got: Option<&Value>, want: &Value) -> bool {
    let present = got.is_some_and(|v| !v.is_null());
    match op {
        "exists" => present,
        "missing" => !present,
        _ => {
            let Some(got) = got else { return op == "ne" };
            match op {
                "eq" => loose_eq(got, want),
                "ne" => !loose_eq(got, want),
                "contains" => match (got, want) {
                    (Value::String(s), Value::String(w)) => s.contains(w.as_str()),
                    (Value::Array(a), w) => a.iter().any(|x| loose_eq(x, w)),
                    _ => false,
                },
                "in" => want
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| loose_eq(x, got))),
                "gt" | "gte" | "lt" | "lte" => match (num(got), num(want)) {
                    (Some(g), Some(w)) => match op {
                        "gt" => g > w,
                        "gte" => g >= w,
                        "lt" => g < w,
                        _ => g <= w,
                    },
                    _ => false,
                },
                _ => false,
            }
        }
    }
}

/// The outcome a condition picks in `ctx`: the first matching rule, else
/// its default; `None` when nothing applies.
pub fn evaluate_condition(n: &Node, ctx: &Value) -> Option<String> {
    for r in n.rules() {
        if rule_matches(&r.op, lookup(ctx, &r.path), &r.value) {
            return Some(r.outcome);
        }
    }
    Some(n.str("default").to_string()).filter(|d| !d.is_empty())
}

/// What a condition sees: `{run, event, inputs, steps: {nodeId: {...}}}`.
pub fn run_context(run: &Run, steps: &[RunStep]) -> Value {
    let mut by_node = serde_json::Map::new();
    let mut latest: HashMap<&str, &RunStep> = HashMap::new();
    for s in steps {
        match latest.get(s.node_id.as_str()) {
            Some(c) if c.seq > s.seq => {}
            _ => {
                latest.insert(&s.node_id, s);
            }
        }
    }
    for (id, s) in latest {
        let mut v: Value = serde_json::from_str(&s.details_json).unwrap_or_else(|_| json!({}));
        if !v.is_object() {
            v = json!({ "details": v });
        }
        v["status"] = json!(s.status);
        v["route"] = json!(s.route);
        v["error"] = json!(s.error);
        v["attempt"] = json!(s.attempt);
        by_node.insert(id.to_string(), v);
    }
    json!({
        "run": {
            "id": run.id, "repo": run.repo, "cardId": run.card_id,
            "trigger": run.trigger_kind, "workflowId": run.workflow_id,
        },
        "event": serde_json::from_str::<Value>(&run.event_json).unwrap_or(Value::Null),
        "inputs": serde_json::from_str::<Value>(&run.inputs_json).unwrap_or(Value::Null),
        "steps": Value::Object(by_node),
    })
}

/// Whether event `ev` matches a pattern (`session.ended`, `session.*`, `*`)
/// and an optional `match` object of path -> value over `{kind, subject, data}`.
pub fn event_matches(pattern: &str, filter: Option<&Value>, ev: &Event) -> bool {
    let p = pattern.trim();
    let kind_ok = p == "*"
        || p == ev.kind
        || p.strip_suffix('*')
            .is_some_and(|prefix| !prefix.is_empty() && ev.kind.starts_with(prefix));
    if !kind_ok {
        return false;
    }
    let Some(Value::Object(m)) = filter else {
        return true;
    };
    let ctx = json!({
        "kind": ev.kind, "subject": ev.subject,
        "data": serde_json::from_str::<Value>(&ev.data_json).unwrap_or(Value::Null),
    });
    m.iter()
        .all(|(path, want)| lookup(&ctx, path).is_some_and(|got| loose_eq(got, want)))
}

fn fill_message(tpl: &str, wf: &str, run: &Run, node: &Node, status: &str) -> String {
    tpl.replace("{workflow}", wf)
        .replace("{run}", &short_id(&run.id))
        .replace("{repo}", &run.repo)
        .replace("{node}", &node.title)
        .replace("{status}", status)
}

pub fn short_id(id: &str) -> String {
    format!(
        "run_{}",
        id.chars()
            .rev()
            .take(8)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    )
}

/// The crew and step settings a dispatch carries (`Dispatch.options`): the
/// Crews settings (`Effective`), overridden by the step's own config. The host
/// checks each key and never widens what it allows.
/// A step's start fields (`dirs`, `workspace`, `effort`, `gitSync`, `ask`,
/// `fill`, `stay`, `stayIdleMaxS`, `clone`, `mcp`), checked for `kind`.
pub fn start_fields(
    node: &Node,
    kind: &str,
) -> Result<kls_core::sessions::start::StartFields, String> {
    let f = kls_core::sessions::start::StartFields::from_config(&node.config)?;
    f.validate(kind)?;
    Ok(f)
}

pub fn crew_options(
    eff: Option<&kls_core::audit::config::Effective>,
    node: &Node,
) -> std::collections::BTreeMap<String, String> {
    let mut o = std::collections::BTreeMap::new();
    if let Some(e) = eff {
        if !e.guardrails.is_empty() {
            o.insert("guardrails".to_string(), e.guardrails.clone());
        }
        o.insert("tests_post_end".into(), e.tests_post_end.to_string());
        if !e.tests_on_fail.is_empty() {
            o.insert("tests_on_fail".into(), e.tests_on_fail.clone());
        }
        o.insert(
            "show_internal_work".into(),
            e.show_internal_work.to_string(),
        );
        o.insert(
            "auto_commit_per_task".into(),
            e.auto_commit_per_task.to_string(),
        );
        o.insert("a2a_show".into(), e.a2a_show.to_string());
        if e.crew_lease_ms > 0 {
            o.insert("crew_lease_ms".into(), e.crew_lease_ms.to_string());
        }
    }
    // Step overrides (camelCase in the node, snake_case on the wire).
    for (cfg, key) in [
        ("guardrails", "guardrails"),
        ("testsPostEnd", "tests_post_end"),
        ("testsOnFail", "tests_on_fail"),
        ("showInternalWork", "show_internal_work"),
        ("autoCommitPerTask", "auto_commit_per_task"),
        ("a2aShow", "a2a_show"),
        ("leaseMs", "crew_lease_ms"),
    ] {
        if let Some(v) = node.config.get(cfg) {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            if !s.is_empty() {
                o.insert(key.to_string(), s);
            }
        }
    }
    // In belai's words (`rc.ApplyFactoryOptions`), which refuses the whole
    // dispatch on a value it does not take; a value that only restates the
    // host's own setting is left out.
    o.into_iter()
        .filter_map(|(k, v)| wire_option(&k, &v).map(|v| (k, v)))
        .collect()
}

/// One crew option as belai takes it, or `None` to leave it to the host.
fn wire_option(key: &str, v: &str) -> Option<String> {
    let v = v.trim();
    let on = |v: &str| matches!(v, "true" | "1" | "on" | "yes");
    match key {
        // Remote work keeps guardrails on: strict, standard and relaxed
        // are all "on" to the host.
        "guardrails" => (!matches!(v, "" | "off" | "false")).then(|| "on".into()),
        // The setting is "run tests when a worker ends".
        "tests_post_end" => Some(match v {
            "goal" | "goal_plan" | "session" | "off" => v.into(),
            v if on(v) => "session".into(),
            _ => "off".into(),
        }),
        "tests_on_fail" => match v {
            "off" | "diagnose" | "fix" => Some(v.into()),
            "report" => Some("diagnose".into()),
            _ => None,
        },
        // A dispatch may only turn committing off.
        "auto_commit_per_task" => (!on(v)).then(|| "false".into()),
        "show_internal_work" | "a2a_show" => Some(on(v).to_string()),
        _ => Some(v.into()),
    }
}

/// A repository's fixed offset inside a stagger window (stable across fires).
pub fn stagger_offset(repo: &str, spread_ms: i64) -> i64 {
    if spread_ms <= 0 {
        return 0;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(repo.to_lowercase().as_bytes());
    let n = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
    (n % spread_ms as u64) as i64
}

/// The profile the repo scout stored for `repo`, if any.
pub async fn repo_profile(core: &Core, repo: &str) -> Option<crate::triggers::RepoProfile> {
    let raw = core
        .store()
        .setting(&crate::triggers::RepoProfile::key(repo))
        .await
        .ok()
        .flatten()?;
    serde_json::from_str(&raw).ok()
}

/// The branch a run is about, for `base: pr_head`: the trigger event's
/// pull request head (`head`), or the branch it names (`headBranch`,
/// `branch`); else the run's pull request in its inputs. "" when none.
pub fn pr_head_of(run: &Run) -> String {
    let event: Value = serde_json::from_str(&run.event_json).unwrap_or(Value::Null);
    let d = event.get("data").unwrap_or(&Value::Null);
    if let Some(h) = data_str(d, &["head", "headRef", "headBranch", "branch"]) {
        return h.to_string();
    }
    let inputs: Value = serde_json::from_str(&run.inputs_json).unwrap_or(Value::Null);
    data_str(&inputs, &["prHead", "head"])
        .unwrap_or("")
        .to_string()
}

/// The error prefix of an `approve_ready` step waiting on readiness.
pub const READINESS_WAIT: &str = "deferred:readiness";
/// How long an `approve_ready` step waits for readiness.
pub const READINESS_MAX_MS: i64 = 72 * 3_600_000;

/// Settings key: logins the factory acts as (comma-separated), beyond
/// `keleustes[bot]` / `belai[bot]`.
pub const FACTORY_IDENTITY: &str = "forge.identity";

/// The logins the factory acts as (`forge.identity`).
pub async fn factory_identities(core: &Core) -> Vec<String> {
    core.store()
        .setting(FACTORY_IDENTITY)
        .await
        .ok()
        .flatten()
        .map(|v| {
            v.trim_matches('"')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A live trigger an event hit.
struct Hit {
    sch: Schedule,
    config: Value,
    card_id: String,
    names: Vec<String>,
}

// ---- the engine ------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// First retry backoff (doubles per attempt, capped at 10 minutes).
    pub retry_backoff_ms: i64,
    /// How soon a step that found no node tries again.
    pub no_host_retry_ms: i64,
    /// How long a step waits for a node before it fails `no_host_timeout`.
    pub no_host_ttl_ms: i64,
    /// How soon a forge gate with pending checks asks again.
    pub gate_recheck_ms: i64,
    /// A cron fire later than this is a missed fire (the catch-up policy applies).
    pub late_grace_ms: i64,
    /// The loop never sleeps longer than this.
    pub max_sleep_ms: i64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            retry_backoff_ms: 30_000,
            no_host_retry_ms: 60_000,
            no_host_ttl_ms: 3_600_000,
            gate_recheck_ms: 300_000,
            late_grace_ms: 90_000,
            max_sleep_ms: 300_000,
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    core: Core,
    integrations: Integrations,
    config: EngineConfig,
    lock: tokio::sync::Mutex<()>,
    graphs: Mutex<HashMap<(String, i64), Graph>>,
    /// Cron fires the kill switch refused (refused once each).
    held: Mutex<std::collections::HashSet<(String, i64)>>,
    /// Gated defaults that noted `trigger.skipped` (workflow id -> when).
    skipped: Mutex<HashMap<String, i64>>,
}

/// Runs the engine loop; dropping it (or `stop`) ends the loop.
pub struct EngineHandle {
    engine: Engine,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl EngineHandle {
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub async fn stop(mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

/// Starts the scheduler and executor with the default integrations: the
/// core's board and GitHub through the process's GitHub plane
/// (`kls_github::service`, the same instance `kls_github::start` polls with).
pub async fn start(core: Core) -> Result<EngineHandle> {
    let integrations = Integrations {
        forge: Some(Arc::new(crate::forge::GitHubForge::new(
            kls_github::service(&core),
        ))),
        board: Some(Arc::new(CoreBoard(core.clone()))),
    };
    start_with(core, integrations, EngineConfig::default()).await
}

pub async fn start_with(
    core: Core,
    integrations: Integrations,
    config: EngineConfig,
) -> Result<EngineHandle> {
    // The per-default switches (`defaults.<key>.enabled`) in Settings.
    crate::defaults::register_settings();
    let engine = Engine::new(core, integrations, config);
    // Subscribe before recovery so nothing emitted meanwhile is missed.
    let bus = engine.inner.core.subscribe();
    engine.recover().await?;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let e2 = engine.clone();
    let task = tokio::spawn(async move { e2.run_loop(bus, stop_rx).await });
    Ok(EngineHandle {
        engine,
        stop: Some(stop_tx),
        task,
    })
}

fn backoff(base: i64, attempt: i64) -> i64 {
    let n = (attempt - 2).clamp(0, 10) as u32;
    (base.saturating_mul(1 << n)).min(600_000)
}

fn wake_flag(data: &Value) -> bool {
    data.get("wake").and_then(Value::as_bool).unwrap_or(false)
}

fn data_str<'a>(data: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| data.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

/// How often a provision step looks at its previews and plan run.
pub const PROVISION_POLL_MS: i64 = 15_000;

/// How often the loop does a full sweep.
pub const FULL_SWEEP_MS: i64 = 15 * 60_000;

const FAILURE_WORDS: [&str; 9] = [
    "failed",
    "refused",
    "expired",
    "stopped",
    "error",
    "timeout",
    "cancelled",
    "killed",
    "crashed",
];
const SUCCESS_WORDS: [&str; 7] = [
    "ended",
    "ok",
    "done",
    "succeeded",
    "completed",
    "success",
    "finished",
];

impl Engine {
    pub fn new(core: Core, integrations: Integrations, config: EngineConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                core,
                integrations,
                config,
                lock: tokio::sync::Mutex::new(()),
                graphs: Mutex::new(HashMap::new()),
                held: Mutex::new(Default::default()),
                skipped: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn core(&self) -> &Core {
        &self.inner.core
    }

    async fn run_loop(
        &self,
        mut bus: tokio::sync::broadcast::Receiver<Event>,
        mut stop: tokio::sync::oneshot::Receiver<()>,
    ) {
        // A full sweep every 15 minutes catches lost wakes and queued runs
        // the factory-wide cap held back.
        let mut last_sweep = now_ms();
        loop {
            let now = now_ms();
            if now - last_sweep >= FULL_SWEEP_MS {
                last_sweep = now;
                if !self.stopped().await
                    && let Err(e) = self.sweep().await
                {
                    tracing::warn!("engine sweep: {e:#}");
                }
            }
            // While the kill switch holds, due timers stay due: do not spin on them.
            let next = if self.stopped().await {
                None
            } else {
                self.next_wake().await.unwrap_or(None)
            };
            let wait = next
                .map(|t| (t - now).clamp(0, self.inner.config.max_sleep_ms))
                .unwrap_or(self.inner.config.max_sleep_ms);
            let wait = wait.min((FULL_SWEEP_MS - (now - last_sweep)).max(0));
            tokio::select! {
                _ = &mut stop => break,
                ev = bus.recv() => match ev {
                    Ok(ev) => {
                        if let Err(e) = self.on_event(&ev).await {
                            tracing::warn!(kind = %ev.kind, "engine event: {e:#}");
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if let Err(e) = self.sweep().await {
                            tracing::warn!("engine sweep: {e:#}");
                        }
                    }
                    Err(RecvError::Closed) => break,
                },
                _ = tokio::time::sleep(std::time::Duration::from_millis(wait as u64)) => {
                    if let Err(e) = self.tick(now_ms()).await {
                        tracing::warn!("engine tick: {e:#}");
                    }
                }
            }
        }
    }

    /// Earliest step timer or cron fire.
    pub async fn next_wake(&self) -> Result<Option<i64>> {
        let s = self.core().store();
        let a = s.next_step_wake().await?;
        let b = s.next_schedule_fire().await?;
        Ok(match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    /// Startup: steps that were running when the process stopped run again,
    /// cron rows get a timer, missed fires follow their catch-up policy and
    /// every active run is advanced.
    pub async fn recover(&self) -> Result<()> {
        let s = self.core().store();
        for st in s.steps_in(&["running"]).await? {
            let u = StepUpdate {
                status: Some("pending".into()),
                error: Some(String::new()),
                ..Default::default()
            };
            s.update_step(&st.id, &["running"], &u).await?;
        }
        self.sweep().await
    }

    /// One pass over everything: schedules, then every active run.
    pub async fn sweep(&self) -> Result<()> {
        self.tick(now_ms()).await?;
        for r in self
            .core()
            .store()
            .runs_in(&["queued", "running", "waiting_approval"])
            .await?
        {
            self.advance(&r.id).await?;
        }
        Ok(())
    }

    /// Fires due schedules and advances runs whose step timers passed.
    pub async fn tick(&self, now: i64) -> Result<()> {
        self.fire_schedules(now).await?;
        for run_id in self.core().store().due_step_runs(now).await? {
            self.advance(&run_id).await?;
        }
        Ok(())
    }

    // ---- scheduler ---------------------------------------------------------

    async fn fire_schedules(&self, now: i64) -> Result<()> {
        let core = self.core();
        let s = core.store();
        if let Some(_why) = guard::halted(core).await {
            // Held, not dropped: each due fire is refused (and audited) once.
            for sch in s.live_schedules().await? {
                let Some(next) = sch.next_run_at.filter(|n| *n <= now && sch.kind == "cron") else {
                    continue;
                };
                if self
                    .inner
                    .held
                    .lock()
                    .unwrap()
                    .insert((sch.id.clone(), next))
                {
                    guard::refuse_switch(core, "schedule", json!({ "scheduleId": sch.id, "workflowId": sch.workflow_id, "scheduledFor": next }),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
        self.inner.held.lock().unwrap().clear();
        // Held runs whose window ended move on.
        for id in ops::release_held(core).await? {
            self.advance(&id).await?;
        }
        // `schedules.tz` and `schedules.missed` fill what a trigger leaves unset.
        let eff = kls_core::audit::config::Effective::load(core).await.ok();
        let settings_tz = eff
            .as_ref()
            .map(|e| e.schedules_tz.clone())
            .unwrap_or_default();
        let missed_default = match eff.as_ref().map(|e| e.schedules_missed.as_str()) {
            Some("skip") => "skip",
            Some("run_all") => "all",
            _ => "once",
        };
        // Inside a schedule blackout (or maintenance window) a due fire is
        // held: its run is created `held` and queued when the window ends.
        let window = guard::window(core, "schedule").await;
        for sch in s.live_schedules().await? {
            if sch.kind != "cron" {
                continue;
            }
            let tz = ops::schedule_zone(&sch.tz, &settings_tz);
            let Some(next) = sch.next_run_at else {
                let n = cron::next_after(&sch.cron, &tz, now);
                s.set_schedule_next(&sch.id, n).await?;
                continue;
            };
            if next > now {
                continue;
            }
            let config: Value = serde_json::from_str(&sch.config_json).unwrap_or(Value::Null);
            let policy = config
                .get("catchUp")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .unwrap_or(missed_default);
            let late = now - next > self.inner.config.late_grace_ms;
            let mut missed = vec![next];
            missed.extend(cron::between(&sch.cron, &tz, next, now, 24));
            let fires: Vec<i64> = if !late {
                vec![next]
            } else {
                match policy {
                    "skip" => vec![],
                    "all" => missed.clone(),
                    _ => missed.last().copied().into_iter().collect(),
                }
            };
            let new_next = cron::next_after(&sch.cron, &tz, now);
            let mut run_ids = Vec::new();
            let mut skipped = Vec::new();
            for t in &fires {
                match self.fire_one(&sch, *t, late, window.as_deref()).await {
                    Ok(StartOutcome::Started(r)) => run_ids.push(r.id),
                    Ok(StartOutcome::Duplicate(_)) => {}
                    Ok(StartOutcome::Skipped(why)) => skipped.push(why),
                    Err(e) => skipped.push(format!("{e:#}")),
                }
            }
            s.record_schedule_fire(
                &sch.id,
                Some(next),
                now,
                new_next,
                run_ids.last().map(String::as_str),
            )
            .await?;
            if late && fires.len() < missed.len() {
                core.emit(
                    kinds::SCHEDULE_SKIPPED,
                    &sch.id,
                    &json!({ "reason": "missed", "missed": missed.len(), "fired": fires.len(), "policy": policy }),
                )
                .await?;
            }
            for why in skipped {
                core.emit(
                    kinds::SCHEDULE_SKIPPED,
                    &sch.id,
                    &json!({ "reason": why, "workflowId": sch.workflow_id }),
                )
                .await?;
            }
            core.emit(
                kinds::SCHEDULE_FIRED,
                &sch.id,
                &json!({
                    "runIds": run_ids, "scheduledFor": fires, "catchUp": late,
                    "nextRunAt": new_next, "workflowId": sch.workflow_id,
                    "held": window, "tz": tz, "policy": policy,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// One cron fire. A workflow scoped to one repository runs on it; a
    /// wider scope fans out to one run per managed repository in scope. A
    /// profile condition (defaults catalogue) needs the repo scout's profile.
    async fn fire_one(
        &self,
        sch: &Schedule,
        at: i64,
        late: bool,
        hold: Option<&str>,
    ) -> Result<StartOutcome> {
        let core = self.core();
        let mut repos: Vec<String> = vec![String::new()];
        if let Some(w) = core.store().workflow(&sch.workflow_id).await?
            && let Some(v) = core
                .store()
                .workflow_version(&w.id, w.published_version)
                .await?
        {
            let g = Graph::parse(&v.graph_json).unwrap_or_default();
            match g.scope.repos.as_slice() {
                [] => {}
                [only] if !only.contains('*') => repos = vec![only.clone()],
                scope => {
                    repos = core
                        .store()
                        .gh_managed_repos()
                        .await?
                        .into_iter()
                        .map(|r| r.full_name)
                        .filter(|r| ops::repo_in_scope(scope, r))
                        .collect();
                    if repos.is_empty() {
                        return Ok(StartOutcome::Skipped("no_repo_in_scope".into()));
                    }
                }
            }
        }
        let config: Value = serde_json::from_str(&sch.config_json).unwrap_or(Value::Null);
        let profile = crate::triggers::Conditions::from_config(&config).profile;
        let fan = repos.len() > 1;
        let mut first: Option<StartOutcome> = None;
        for repo in repos {
            if let Some(m) = &profile {
                let skip = match repo_profile(core, &repo).await {
                    None => Some("profile_unknown"),
                    Some(p) if !m.matches(&p) => Some("profile"),
                    _ => None,
                };
                if let Some(why) = skip {
                    first.get_or_insert(StartOutcome::Skipped(why.into()));
                    continue;
                }
            }
            let dedupe = if fan {
                format!("{}:{}:{}", sch.id, at / 60_000, repo)
            } else {
                format!("{}:{}", sch.id, at / 60_000)
            };
            // A fan-out spreads its runs over `staggerMinutes` (default 6 h),
            // each repository at a fixed offset (vdb-site `factory_cron_stagger`).
            let inputs = if fan {
                let spread = config
                    .get("staggerMinutes")
                    .and_then(Value::as_i64)
                    .unwrap_or(360)
                    .clamp(0, 1440)
                    * 60_000;
                json!({ "notBefore": at + stagger_offset(&repo, spread) })
            } else {
                Value::Null
            };
            let out = ops::start_run(
                core,
                StartRun {
                    workflow_id: sch.workflow_id.clone(),
                    trigger: TriggerInfo {
                        kind: "cron".into(),
                        node_id: sch.node_id.clone(),
                        schedule_id: Some(sch.id.clone()),
                        label: if late {
                            format!("cron {} (missed)", sch.cron)
                        } else {
                            format!("cron {}", sch.cron)
                        },
                    },
                    repo,
                    inputs,
                    event: json!({ "scheduledFor": at, "catchUp": late }),
                    dedupe_key: Some(dedupe),
                    held: hold.map(str::to_string),
                    ..Default::default()
                },
            )
            .await?;
            if let StartOutcome::Started(r) = &out {
                self.advance(&r.id).await?;
            }
            match (&first, &out) {
                (None, _) | (Some(StartOutcome::Skipped(_)), StartOutcome::Started(_)) => {
                    first = Some(out)
                }
                _ => {}
            }
        }
        Ok(first.unwrap_or(StartOutcome::Skipped("nothing to run".into())))
    }

    // ---- events ------------------------------------------------------------

    pub async fn on_event(&self, ev: &Event) -> Result<()> {
        let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(Value::Null);
        if wake_flag(&data) {
            let run_id = data_str(&data, &["runId"])
                .map(str::to_string)
                .unwrap_or_else(|| ev.subject.clone());
            self.advance(&run_id).await?;
        }
        if ev.kind.starts_with("dispatch.") || ev.kind.starts_with("session.") {
            self.settle_dispatch(ev, &data).await?;
        }
        if ev.kind == crate::conflicts::EVENT
            && let Err(e) = crate::conflicts::on_conflict(self.core(), &data).await
        {
            tracing::warn!("conflict card: {e:#}");
        }
        // Managed repositories, their settings or the factory settings changed:
        // the defaults catalogue follows.
        if matches!(
            ev.kind.as_str(),
            kls_core::github::kinds::REPO_UPDATED
                | kls_core::github::kinds::ORG_UPDATED
                | kls_core::audit::kinds::SETTINGS_SAVED
        ) && !self.stopped().await
            && let Err(e) = crate::defaults::seed(self.core()).await
        {
            tracing::warn!("defaults seed: {e:#}");
        }
        // The kill switch lifted: held work moves again.
        if ev.kind == kls_core::board::kinds::FACTORY_CONTROL && !self.stopped().await {
            return self.sweep().await;
        }
        self.fire_event_triggers(ev, &data).await?;
        self.note_gated_defaults(ev, &data).await?;
        self.settle_waits(ev).await?;
        self.settle_cards(ev).await?;
        self.wake_gates(ev, &data).await?;
        if ev.kind == kls_core::hosts::kinds::PLAN_FINISHED {
            // Wake the provision step that runs this plan run now.
            let id = data_str(&data, &["runId"]).unwrap_or("").to_string();
            let s = self.core().store();
            for st in s.steps_in(&["waiting"]).await? {
                let d: Value = serde_json::from_str(&st.details_json).unwrap_or(Value::Null);
                if st.node_type == "provision"
                    && d.get("planRunId").and_then(Value::as_str) == Some(id.as_str())
                {
                    s.update_step(
                        &st.id,
                        &["waiting"],
                        &StepUpdate {
                            wake_at: Some(Some(now_ms())),
                            ..Default::default()
                        },
                    )
                    .await?;
                    self.advance(&st.run_id).await?;
                }
            }
        }
        Ok(())
    }

    /// Forge gates waiting on checks look again when their pull request or
    /// its checks change (`github.pull.*`, `github.workflow_run`,
    /// `github.review.submitted`), instead of waiting for the recheck timer.
    /// `approve_ready` steps waiting on readiness wake the same way.
    async fn wake_gates(&self, ev: &Event, data: &Value) -> Result<()> {
        if !(ev.kind.starts_with("github.pull.")
            || ev.kind.starts_with("github.review.")
            || ev.kind == kls_core::github::kinds::WORKFLOW_RUN)
        {
            return Ok(());
        }
        let repo = data_str(data, &["repo"]).unwrap_or("");
        if repo.is_empty() {
            return Ok(());
        }
        let number = data.get("number").and_then(Value::as_i64);
        let head = data_str(data, &["head", "headBranch", "branch"]).unwrap_or("");
        let s = self.core().store();
        for st in s.steps_in(&["waiting"]).await? {
            let gate = st.node_type == "gate" && st.error == "deferred:checks";
            let ready = st.node_type == "github" && st.error.starts_with(READINESS_WAIT);
            if !(gate || ready) {
                continue;
            }
            let Some(run) = s.run(&st.run_id).await? else {
                continue;
            };
            if !run.repo.eq_ignore_ascii_case(repo) {
                continue;
            }
            // Only the run's own pull request (or branch), when it has one.
            let event: Value = serde_json::from_str(&run.event_json).unwrap_or(Value::Null);
            let d = event.get("data").unwrap_or(&Value::Null);
            let run_number = d.get("number").and_then(Value::as_i64);
            let run_head = data_str(d, &["head", "headBranch", "branch"]).unwrap_or("");
            let same = match (run_number, number) {
                (Some(a), Some(b)) => a == b,
                _ => run_head.is_empty() || head.is_empty() || run_head == head,
            };
            if !same {
                continue;
            }
            let mut details: Value = serde_json::from_str(&st.details_json).unwrap_or(json!({}));
            details["wokenBy"] = json!({ "id": ev.id, "kind": ev.kind, "subject": ev.subject });
            if s.update_step(
                &st.id,
                &["waiting"],
                &StepUpdate {
                    wake_at: Some(Some(now_ms())),
                    details_json: Some(details.to_string()),
                    ..Default::default()
                },
            )
            .await?
            {
                self.advance(&st.run_id).await?;
            }
        }
        Ok(())
    }

    /// An event a gated default would have taken: `trigger.skipped` with
    /// the gate's reason, at most once an hour per seeded workflow, audited.
    async fn note_gated_defaults(&self, ev: &Event, data: &Value) -> Result<()> {
        if !ev.kind.starts_with("github.") {
            return Ok(());
        }
        let core = self.core();
        let s = core.store();
        let names = crate::forge::webhook_events(ev, data);
        if names.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        for sch in s.schedules().await? {
            if sch.kind != "webhook" || !sch.enabled {
                continue;
            }
            let config: Value = serde_json::from_str(&sch.config_json).unwrap_or(Value::Null);
            let want = config.get("event").and_then(Value::as_str).unwrap_or("");
            if !crate::forge::webhook_matches(want, &names) {
                continue;
            }
            let Some(w) = s.workflow(&sch.workflow_id).await? else {
                continue;
            };
            if w.enabled || w.system_key.is_empty() || w.archived_at.is_some() {
                continue;
            }
            let Some((key, repo)) = crate::defaults::parse_system_key(&w.system_key) else {
                continue;
            };
            let ev_repo = data_str(data, &["repo"]).unwrap_or("");
            if !ev_repo.eq_ignore_ascii_case(repo) {
                continue;
            }
            let Some(reason) = crate::defaults::gate_reason(core, key, repo).await else {
                continue;
            };
            {
                let mut seen = self.inner.skipped.lock().unwrap();
                if seen.get(&w.id).is_some_and(|t| now - *t < 3_600_000) {
                    continue;
                }
                seen.insert(w.id.clone(), now);
            }
            let d = json!({
                "reason": reason, "key": key, "repo": repo, "workflowId": w.id,
                "scheduleId": sch.id, "event": ev.kind, "subject": ev.subject,
            });
            core.emit(ops::TRIGGER_SKIPPED, &w.id, &d).await?;
            guard::audit(
                core,
                "trigger",
                "trigger.skipped",
                json!({ "workflowId": w.id, "scheduleId": sch.id, "key": key }),
                &Origin::engine(json!({ "event": { "id": ev.id, "kind": ev.kind, "subject": ev.subject }, "repo": repo })),
                &format!("skipped: {reason}"),
                json!({}),
            )
            .await?;
        }
        Ok(())
    }

    /// The factory's kill switch (M2): while it is on nothing starts or
    /// moves; cron fires stay due and run when it lifts.
    pub async fn stopped(&self) -> bool {
        guard::halted(self.core()).await.is_some()
    }

    /// Which live event, webhook and label triggers `ev` hits, with the card
    /// the run would carry and the event's webhook names.
    async fn trigger_hits(&self, ev: &Event, data: &Value) -> Result<Vec<Hit>> {
        let core = self.core();
        let mut out = Vec::new();
        for sch in core.store().live_schedules().await? {
            if !matches!(sch.kind.as_str(), "kanban" | "event" | "webhook" | "label") {
                continue;
            }
            // A workflow is never started by its own runs.
            if data_str(data, &["workflowId"]) == Some(sch.workflow_id.as_str()) {
                continue;
            }
            let config: Value = serde_json::from_str(&sch.config_json).unwrap_or(Value::Null);
            let cfg = |k: &str| config.get(k).and_then(Value::as_str).unwrap_or("");
            let mut names = Vec::new();
            let (hit, card_id) = match sch.kind.as_str() {
                "kanban" => {
                    if ev.kind != "card.moved" {
                        (false, String::new())
                    } else {
                        let detail = data.get("detail").unwrap_or(data);
                        let from = detail.get("from").and_then(Value::as_str).unwrap_or("");
                        let to = detail.get("to").and_then(Value::as_str).unwrap_or("");
                        let ok = (cfg("fromList").is_empty() || cfg("fromList") == from)
                            && (cfg("toList").is_empty() || cfg("toList") == to);
                        (ok, ev.subject.clone())
                    }
                }
                // GitHub (M4 mirrors it; there are no webhooks): the
                // mirror's `github.*` events stand for the webhook names.
                "webhook" | "label" => {
                    let card = data_str(data, &["cardId", "card_id"])
                        .unwrap_or("")
                        .to_string();
                    if !ev.kind.starts_with("github.") {
                        (false, String::new())
                    } else if sch.kind == "webhook" {
                        names = crate::forge::webhook_events(ev, data);
                        (crate::forge::webhook_matches(cfg("event"), &names), card)
                    } else {
                        let want = cfg("label").trim();
                        let by_comment = crate::triggers::command_of(data);
                        let ok = !want.is_empty()
                            && (crate::forge::labels_added(data)
                                .iter()
                                .any(|l| l.eq_ignore_ascii_case(want))
                                || by_comment
                                    .as_deref()
                                    .is_some_and(|c| c.eq_ignore_ascii_case(want)));
                        (ok, card)
                    }
                }
                _ => {
                    if ev.kind.starts_with("workflow.")
                        || ev.kind.starts_with("schedule.")
                        || ev.kind.starts_with("engine.")
                        || ev.kind.starts_with("audit.")
                    {
                        (false, String::new())
                    } else {
                        let ok = event_matches(cfg("event"), config.get("match"), ev);
                        let card = if ev.kind.starts_with("card.") {
                            ev.subject.clone()
                        } else {
                            data_str(data, &["cardId", "card_id"])
                                .unwrap_or("")
                                .to_string()
                        };
                        (ok, card)
                    }
                }
            };
            if hit {
                out.push(Hit {
                    sch,
                    config,
                    card_id,
                    names,
                });
            }
        }
        // Fan-out order: highest priority first, then oldest trigger.
        out.sort_by_key(|h| crate::triggers::priority(&h.config));
        Ok(out)
    }

    /// The facts of a GitHub event, with what the mirror knows about its pull.
    async fn event_facts(&self, ev: &Event, data: &Value) -> Facts {
        let mut f = Facts::from_event(ev, data);
        if !f.repo.is_empty() {
            let s = self.core().store();
            if f.default_branch.is_empty()
                && let Ok(Some(r)) = s.gh_repo(&f.repo).await
            {
                f.default_branch = r.default_branch;
            }
            f.profile = repo_profile(self.core(), &f.repo).await;
        }
        if ev.kind.starts_with("github.pull.")
            && let Some(n) = f.number
            && let Ok(Some(p)) = self.core().store().gh_pull(&f.repo, n).await
        {
            if f.base.is_empty() {
                f.base = p.base_ref.clone();
            }
            if f.head.is_empty() {
                f.head = p.head_ref.clone();
            }
            if f.head_sha.is_empty() {
                f.head_sha = p.head_sha.clone();
            }
            if f.author.is_empty() {
                f.author = p.author.clone();
            }
            if f.draft.is_none() {
                f.draft = Some(p.draft);
            }
            if f.fork.is_none() && !p.head_repo.is_empty() {
                f.fork = Some(p.is_fork());
            }
            if f.review.is_empty() {
                f.review = p.review_decision.clone();
            }
        }
        f
    }

    /// Why a hit that matched its event must not start a run, if anything.
    async fn hold_reason(
        &self,
        h: &Hit,
        facts: &Facts,
        fanout_rank: usize,
    ) -> Result<Option<String>> {
        let core = self.core();
        let github = h.sch.kind == "webhook" || h.sch.kind == "label";
        if github {
            if let Err(why) = crate::triggers::check(
                &crate::triggers::Conditions::from_config(&h.config),
                &h.names,
                facts,
            ) {
                // A condition that does not match is not a fire at all.
                return Ok(Some(format!("condition:{why}")));
            }
            if h.sch.kind == "label" {
                let allow_factory = h
                    .config
                    .get("fromFactory")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let allow_bots = h
                    .config
                    .get("allowBots")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let want = h.config.get("label").and_then(Value::as_str).unwrap_or("");
                let by_comment = facts
                    .command
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(want));
                if by_comment && !crate::triggers::trusted_association(&facts.association) {
                    return Ok(Some("comment_untrusted_sender".into()));
                }
                if !by_comment && facts.from_factory && !allow_factory {
                    return Ok(Some("from_factory".into()));
                }
                // Who added the label: the forge's `labeledBy` map, else the field.
                let labeler = facts
                    .labeled_map
                    .get(&want.to_lowercase())
                    .cloned()
                    .unwrap_or_else(|| facts.labeled_by.clone());
                let bot = labeler.to_ascii_lowercase().ends_with("[bot]");
                if !by_comment && bot && !allow_bots && !facts.from_factory {
                    return Ok(Some("label_bot_sender".into()));
                }
                if !by_comment
                    && !facts.labeled_by_association.is_empty()
                    && !crate::triggers::trusted_association(&facts.labeled_by_association)
                    && !(bot && allow_bots)
                {
                    return Ok(Some("label_untrusted_sender".into()));
                }
            }
            let prefix = kls_core::audit::config::Effective::load(core)
                .await
                .map(|e| e.branch_prefix)
                .unwrap_or_else(|_| "belai/K-".into());
            if crate::triggers::own_branch(&prefix, &facts.head) {
                return Ok(Some("own_branch".into()));
            }
            // Own identity (P0 #7): what the factory itself authored never
            // starts a webhook trigger (a label trigger opts in with
            // `fromFactory`, checked above).
            if h.sch.kind == "webhook" {
                let ids = factory_identities(core).await;
                if crate::triggers::factory_authored(facts, &ids) {
                    return Ok(Some("own_identity".into()));
                }
            }
            if facts.fork == Some(true) {
                let has_human = match core.store().workflow(&h.sch.workflow_id).await? {
                    Some(w) => core
                        .store()
                        .workflow_version(&w.id, w.published_version)
                        .await?
                        .map(|v| Graph::parse(&v.graph_json).unwrap_or_default())
                        .is_some_and(|g| g.nodes.iter().any(|n| n.kind == "human")),
                    None => false,
                };
                if !has_human {
                    return Ok(Some("fork_needs_human".into()));
                }
            }
        }
        let cap = core
            .settings_doc()
            .await
            .ok()
            .and_then(|d| d.values.get("factory.fanoutCap").and_then(Value::as_i64))
            .map(|n| n.clamp(1, 100) as usize)
            .unwrap_or(crate::triggers::DEFAULT_FANOUT_CAP);
        if fanout_rank >= cap {
            return Ok(Some("fanout_cap".into()));
        }
        let now = now_ms();
        let recent = core
            .store()
            .trigger_fires(&h.sch.id, now - 3_600_000)
            .await?;
        if let Err(why) = crate::triggers::rate_check(
            &recent,
            &facts.subject,
            now,
            crate::triggers::debounce_seconds(&h.config),
            crate::triggers::max_runs_per_hour(&h.config),
        ) {
            return Ok(Some(why));
        }
        Ok(None)
    }

    /// Records a fire's outcome (provenance) and audits it.
    async fn note_fire(
        &self,
        h: &Hit,
        ev: &Event,
        facts: &Facts,
        run_id: Option<&str>,
        outcome: &str,
    ) -> Result<()> {
        let core = self.core();
        core.store()
            .insert_trigger_fire(&TriggerFire {
                schedule_id: h.sch.id.clone(),
                subject: facts.subject.clone(),
                at: now_ms(),
                event_id: Some(ev.id),
                run_id: run_id.map(str::to_string),
                outcome: outcome.to_string(),
            })
            .await?;
        guard::audit(
            core,
            "trigger",
            &format!("trigger.{}", h.sch.kind),
            json!({ "scheduleId": h.sch.id, "workflowId": h.sch.workflow_id, "nodeId": h.sch.node_id }),
            &Origin::engine(json!({
                "event": { "id": ev.id, "kind": ev.kind, "subject": ev.subject },
                "repo": facts.repo, "author": facts.author,
            })),
            outcome,
            json!({ "runId": run_id }),
        )
        .await
    }

    /// Label controls on a GitHub event: a removed label cancels the queued
    /// runs it started on that subject; `sf:stop` cancels every open run on
    /// the subject.
    async fn label_controls(&self, ev: &Event, data: &Value, facts: &Facts) -> Result<()> {
        if !ev.kind.starts_with("github.") {
            return Ok(());
        }
        let core = self.core();
        let added = crate::forge::labels_added(data);
        let stop = added.iter().any(|l| l.eq_ignore_ascii_case("sf:stop"));
        if !stop && facts.labels_removed.is_empty() {
            return Ok(());
        }
        let runs = core.store().runs_in(&ACTIVE_RUN_STATUSES).await?;
        for r in runs {
            let event: Value = serde_json::from_str(&r.event_json).unwrap_or(Value::Null);
            if event.get("subject").and_then(Value::as_str) != Some(facts.subject.as_str()) {
                continue;
            }
            let removed_label = r.trigger_kind == "label"
                && r.status == "queued"
                && facts.labels_removed.iter().any(|l| {
                    event
                        .pointer("/data/labelsAdded")
                        .and_then(Value::as_array)
                        .is_some_and(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .any(|x| x.eq_ignore_ascii_case(l))
                        })
                });
            if !(stop || removed_label) {
                continue;
            }
            let why = if stop {
                "sf:stop on the subject"
            } else {
                "its label was removed"
            };
            if ops::cancel_run(core, &r.id, why).await.is_ok() {
                guard::audit(
                    core,
                    "run",
                    "run.cancel",
                    json!({ "runId": r.id, "workflowId": r.workflow_id }),
                    &Origin::engine(
                        json!({ "event": { "id": ev.id, "kind": ev.kind, "subject": ev.subject } }),
                    ),
                    why,
                    json!({}),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn fire_event_triggers(&self, ev: &Event, data: &Value) -> Result<()> {
        let core = self.core();
        let hits = self.trigger_hits(ev, data).await?;
        if hits.is_empty() {
            return Ok(());
        }
        let facts = self.event_facts(ev, data).await;
        if let Some(why) = guard::halted(core).await {
            for h in &hits {
                self.note_fire(h, ev, &facts, None, &format!("skipped:{why}"))
                    .await?;
                guard::refuse_switch(core, "trigger", json!({ "scheduleId": h.sch.id, "workflowId": h.sch.workflow_id, "eventId": ev.id }),
                )
                .await?;
            }
            return Ok(());
        }
        self.label_controls(ev, data, &facts).await?;
        // A schedule blackout holds event triggers too: the run is `held`.
        let window = guard::window(core, "trigger").await;
        let mut rank = 0;
        for h in &hits {
            if let Some(why) = self.hold_reason(h, &facts, rank).await? {
                if why.starts_with("condition:") {
                    continue;
                }
                self.note_fire(h, ev, &facts, None, &format!("skipped:{why}"))
                    .await?;
                core.emit(
                    kinds::SCHEDULE_SKIPPED,
                    &h.sch.id,
                    &json!({ "reason": why, "event": ev.kind, "workflowId": h.sch.workflow_id, "subject": facts.subject }),
                )
                .await?;
                continue;
            }
            rank += 1;
            let sch = &h.sch;
            let repo = data_str(data, &["repo"]).unwrap_or("").to_string();
            // A default's run key: one run per subject (and commit).
            let system_key = core
                .store()
                .workflow(&sch.workflow_id)
                .await?
                .map(|w| w.system_key)
                .unwrap_or_default();
            let dedupe = crate::triggers::run_key(&system_key, &facts)
                .unwrap_or_else(|| format!("{}:ev{}", sch.id, ev.id));
            let out = ops::start_run(
                core,
                StartRun {
                    workflow_id: sch.workflow_id.clone(),
                    trigger: TriggerInfo {
                        kind: sch.kind.clone(),
                        node_id: sch.node_id.clone(),
                        schedule_id: Some(sch.id.clone()),
                        label: ev.kind.clone(),
                    },
                    repo,
                    card_id: h.card_id.clone(),
                    event: json!({ "id": ev.id, "kind": ev.kind, "subject": ev.subject, "data": data }),
                    dedupe_key: Some(dedupe),
                    held: window.clone(),
                    ..Default::default()
                },
            )
            .await;
            match out {
                Ok(StartOutcome::Started(r)) => {
                    self.note_fire(h, ev, &facts, Some(&r.id), "started")
                        .await?;
                    core.store()
                        .record_schedule_fire(
                            &sch.id,
                            sch.next_run_at,
                            now_ms(),
                            sch.next_run_at,
                            Some(&r.id),
                        )
                        .await?;
                    core.emit(
                        kinds::SCHEDULE_FIRED,
                        &sch.id,
                        &json!({ "runIds": [r.id], "event": ev.kind, "workflowId": sch.workflow_id }),
                    )
                    .await?;
                    self.advance(&r.id).await?;
                }
                Ok(StartOutcome::Duplicate(r)) => {
                    self.note_fire(h, ev, &facts, Some(&r.id), "duplicate")
                        .await?;
                }
                Ok(StartOutcome::Skipped(why)) => {
                    self.note_fire(h, ev, &facts, None, &format!("skipped:{why}"))
                        .await?;
                    core.emit(
                        kinds::SCHEDULE_SKIPPED,
                        &sch.id,
                        &json!({ "reason": why, "event": ev.kind, "workflowId": sch.workflow_id }),
                    )
                    .await?;
                }
                Err(e) => {
                    self.note_fire(h, ev, &facts, None, &format!("skipped:error: {e:#}"))
                        .await?;
                    tracing::warn!(schedule = %sch.id, "event trigger: {e:#}");
                }
            }
        }
        Ok(())
    }

    /// `dispatch.*` and `session.*` events settle crew steps.
    async fn settle_dispatch(&self, ev: &Event, data: &Value) -> Result<()> {
        let s = self.core().store();
        let dispatch_id = if ev.kind.starts_with("dispatch.") {
            Some(ev.subject.clone())
        } else {
            data_str(data, &["dispatchId", "dispatch_id"]).map(str::to_string)
        };
        let session_id = if ev.kind.starts_with("session.") {
            Some(ev.subject.clone())
        } else {
            data_str(data, &["sessionId", "session_id"]).map(str::to_string)
        };
        let mut step = None;
        if let Some(d) = &dispatch_id {
            step = s
                .step_by_dispatch(d)
                .await?
                .filter(|x| x.status == "waiting");
        }
        if step.is_none()
            && let Some(sid) = &session_id
        {
            step =
                s.steps_in(&["waiting"]).await?.into_iter().find(|x| {
                    x.node_type == "crew" && x.session_id.as_deref() == Some(sid.as_str())
                });
        }
        let Some(step) = step else {
            return Ok(());
        };
        let words: Vec<String> = [
            "status",
            "state",
            "endState",
            "end_state",
            "reason",
            "outcome",
        ]
        .iter()
        .filter_map(|k| data.get(*k).and_then(Value::as_str))
        .map(|w| w.to_lowercase())
        .collect();
        let has = |set: &[&str]| words.iter().any(|w| set.iter().any(|x| w.contains(x)));
        let mut details: Value =
            serde_json::from_str(&step.details_json).unwrap_or_else(|_| json!({}));
        let mut u = StepUpdate::default();
        if step.session_id.is_none()
            && let Some(sid) = data_str(data, &["sessionId", "session_id"])
                .map(str::to_string)
                .or_else(|| {
                    session_id
                        .clone()
                        .filter(|_| ev.kind.starts_with("session."))
                })
        {
            details["sessionId"] = json!(sid);
            u.session_id = Some(Some(sid));
        }
        let ended = ev.kind == "session.ended";
        if has(&FAILURE_WORDS) && (ended || ev.kind.starts_with("dispatch.")) {
            let err = data_str(data, &["error"]).unwrap_or("");
            details["failure"] = json!(err);
            u.status = Some("failed".into());
            u.error = Some(if ended {
                "session_failed".into()
            } else {
                "dispatch_failed".into()
            });
            u.finished = true;
            u.wake_at = Some(None);
        } else if (ended || (ev.kind.starts_with("dispatch.") && has(&SUCCESS_WORDS)))
            && (step.node_type == "scan" || self.handoffs_open(&details).await)
        {
            // A scan settles on its card; a step whose worker handed work on
            // waits for those cards (handoff following).
            if step.node_type != "scan" {
                details["handoffWait"] = json!(true);
            }
            if u.session_id.is_none() && step.node_type == "scan" {
                return Ok(());
            }
        } else if ended || (ev.kind.starts_with("dispatch.") && has(&SUCCESS_WORDS)) {
            u.status = Some("succeeded".into());
            u.error = Some(String::new());
            u.finished = true;
            u.wake_at = Some(None);
        } else if u.session_id.is_none() {
            return Ok(());
        }
        if let Some(t) = data.get("tokens").and_then(Value::as_i64) {
            u.add_tokens = (t - step.tokens).max(0);
        }
        u.details_json = Some(details.to_string());
        if s.update_step(&step.id, &["waiting"], &u).await? {
            self.step_event(&step, u.status.as_deref().unwrap_or("waiting"))
                .await?;
            if u.status.is_some() {
                self.advance(&step.run_id).await?;
            }
        }
        Ok(())
    }

    /// Cards a routing card handed work to (`belai.agent.parent`) that are not done.
    async fn open_children(&self, card_id: &str) -> Vec<kls_store::board::Card> {
        let Ok(Some(parent)) = self.core().card(card_id).await else {
            return Vec::new();
        };
        self.core()
            .cards()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|c| {
                let p = kls_core::board::cards::claim(c).parent;
                !p.is_empty() && (p == parent.id || p == parent.key) && c.status != "done"
            })
            .collect()
    }

    /// Whether the step's routing card handed work on that is still open.
    async fn handoffs_open(&self, details: &Value) -> bool {
        match details.get("routeCardId").and_then(Value::as_str) {
            Some(id) => !self.open_children(id).await.is_empty(),
            None => false,
        }
    }

    /// Card events settle scan steps (their card reached its done list or
    /// was blocked) and steps waiting on handed-off cards.
    async fn settle_cards(&self, ev: &Event) -> Result<()> {
        if !ev.kind.starts_with("card.") {
            return Ok(());
        }
        let s = self.core().store();
        for st in s.steps_in(&["waiting"]).await? {
            if st.node_type != "scan" && st.node_type != "crew" {
                continue;
            }
            let mut details: Value = serde_json::from_str(&st.details_json).unwrap_or(Value::Null);
            let Some(card_id) = details
                .get("routeCardId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            let Some(card) = self.core().card(&card_id).await? else {
                continue;
            };
            let blocked = kls_core::board::cards::is_blocked(&card);
            if st.node_type == "scan" {
                if card.id != ev.subject {
                    continue;
                }
                if blocked {
                    self.fail(&st, "scan_failed", details).await?;
                } else if card.status == "done" || card.status == "review" {
                    let meta: Value = serde_json::from_str(&card.meta_json).unwrap_or(Value::Null);
                    let result = meta
                        .pointer("/belai/agent/cliResult")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let not_eval = result
                        .get("notEvaluable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    details["result"] = result;
                    details["cardStatus"] = json!(card.status);
                    if not_eval {
                        details["notEvaluable"] = json!(true);
                    }
                    self.succeed(&st, "", details).await?;
                } else {
                    continue;
                }
                self.advance(&st.run_id).await?;
                continue;
            }
            if !details
                .get("handoffWait")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                // A worker that claims the routing card stamps no session on
                // the step and sends no "finished": the card says (web
                // factory §5.6). Review or done succeeds, blocked fails, and
                // work handed on waits for its cards.
                if card.id != ev.subject {
                    continue;
                }
                if blocked {
                    self.fail(&st, "card_blocked", details).await?;
                } else if card.status == "done" || card.status == "review" {
                    details["cardStatus"] = json!(card.status);
                    if !self.open_children(&card_id).await.is_empty() {
                        details["handoffWait"] = json!(true);
                        s.update_step(
                            &st.id,
                            &["waiting"],
                            &StepUpdate {
                                details_json: Some(details.to_string()),
                                ..Default::default()
                            },
                        )
                        .await?;
                        continue;
                    }
                    details["branch"] = json!(card.branch);
                    details["pr"] = json!(card.pr_url);
                    self.succeed(&st, "", details).await?;
                } else {
                    continue;
                }
                self.advance(&st.run_id).await?;
                continue;
            }
            if !self.open_children(&card_id).await.is_empty() {
                continue;
            }
            // Every handed-off card is done: take the newest one's PR and branch.
            let newest = self
                .core()
                .cards()
                .await?
                .into_iter()
                .filter(|c| {
                    let p = kls_core::board::cards::claim(c).parent;
                    p == card.id || p == card.key
                })
                .max_by_key(|c| c.updated_at);
            if let Some(c) = newest {
                details["handoff"] =
                    json!({ "cardId": c.id, "key": c.key, "pr": c.pr_url, "branch": c.branch });
            }
            self.succeed(&st, "", details).await?;
            self.advance(&st.run_id).await?;
        }
        Ok(())
    }

    /// A `profile` step: stores what the latest scan found as the
    /// repository's profile (the repo scout's `repo_profile` output).
    async fn store_profile(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        steps: &[RunStep],
        ctx: Value,
    ) -> Result<()> {
        if run.repo.is_empty() {
            return self.fail(st, "no_repo", json!({})).await;
        }
        let result = steps
            .iter()
            .filter(|s| s.node_type == "scan" && s.status == "succeeded")
            .max_by_key(|s| s.seq)
            .and_then(|s| serde_json::from_str::<Value>(&s.details_json).ok())
            .and_then(|d| d.get("result").cloned())
            .unwrap_or(Value::Null);
        let p = crate::triggers::RepoProfile::from_result(&result, now_ms());
        let key = crate::triggers::RepoProfile::key(&run.repo);
        self.core()
            .store()
            .set_setting(&key, &serde_json::to_string(&p)?)
            .await?;
        guard::audit(
            self.core(),
            "repo",
            "repo.profile",
            json!({ "repo": run.repo }),
            &Origin::engine(json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id })),
            "stored",
            json!({ "signals": p.signals, "packageManagers": p.package_managers, "languages": p.languages }),
        )
        .await?;
        self.core()
            .emit(
                "engine.profiled",
                &run.repo,
                &json!({ "repo": run.repo, "signals": p.signals, "runId": run.id }),
            )
            .await?;
        let mut details = json!({ "profile": p });
        // The forge keeps the CLI labels and the one "Software Factory
        // profile" issue current (WP3 `repo_profile`), at `suggest` or above
        // and never under the kill switch. A failure is noted, not fatal.
        let have = guard::effective_autonomy(self.core(), &run.repo).await;
        if let Some(forge) = self.inner.integrations.forge.clone()
            && guard::autonomy_rank(&have) >= guard::autonomy_rank("suggest")
            && guard::held(self.core(), "step").await.is_none()
        {
            let sc = StepContext {
                run: run.clone(),
                node: node.clone(),
                step: st.clone(),
                context: ctx,
            };
            let res = forge.action(&sc, "repo_profile").await;
            let outcome = match &res {
                Ok(_) => "done".to_string(),
                Err(e) => format!("failed: {e:#}"),
            };
            guard::audit(
                self.core(),
                "github",
                "repo_profile",
                json!({ "repo": run.repo, "action": "repo_profile" }),
                &Origin::engine(
                    json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id }),
                ),
                &outcome,
                json!({}),
            )
            .await?;
            details["profileIssue"] = match res {
                Ok(v) => v,
                Err(e) => json!(format!("error: {e:#}")),
            };
        }
        self.succeed(st, "", details).await
    }

    /// Wait steps that wait for an event.
    async fn settle_waits(&self, ev: &Event) -> Result<()> {
        if ev.kind.starts_with("run.step") {
            return Ok(());
        }
        let s = self.core().store();
        for st in s.steps_in(&["waiting"]).await? {
            if st.node_type != "wait" {
                continue;
            }
            let details: Value = serde_json::from_str(&st.details_json).unwrap_or(Value::Null);
            let Some(pattern) = details.get("waitEvent").and_then(Value::as_str) else {
                continue;
            };
            if !event_matches(pattern, details.get("match"), ev) {
                continue;
            }
            // Only events about this run's card, when it has one and the event is a card event.
            if ev.kind.starts_with("card.")
                && let Some(card) = details.get("cardId").and_then(Value::as_str)
                && !card.is_empty()
                && card != ev.subject
            {
                continue;
            }
            let mut d = details.clone();
            d["event"] = json!({ "id": ev.id, "kind": ev.kind, "subject": ev.subject });
            let u = StepUpdate {
                status: Some("succeeded".into()),
                details_json: Some(d.to_string()),
                wake_at: Some(None),
                finished: true,
                ..Default::default()
            };
            if s.update_step(&st.id, &["waiting"], &u).await? {
                self.step_event(&st, "succeeded").await?;
                self.advance(&st.run_id).await?;
            }
        }
        Ok(())
    }

    // ---- advancing runs ----------------------------------------------------

    /// Moves a run as far as it can go now. Serialized across the engine.
    pub fn advance<'a>(
        &'a self,
        run_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        // Boxed: a job step's task advances its run again, so the future
        // type is recursive.
        Box::pin(self.advance_inner(run_id))
    }

    async fn advance_inner(&self, run_id: &str) -> Result<()> {
        let mut queue = vec![run_id.to_string()];
        let mut guard = 0;
        while let Some(id) = queue.pop() {
            guard += 1;
            if guard > 200 {
                break;
            }
            let more = {
                let _g = self.inner.lock.lock().await;
                self.advance_locked(&id).await?
            };
            queue.extend(more);
        }
        Ok(())
    }

    fn graph(&self, run: &Run, json: &str) -> Graph {
        let key = (run.workflow_id.clone(), run.workflow_version);
        if let Some(g) = self.inner.graphs.lock().unwrap().get(&key) {
            return g.clone();
        }
        let g = Graph::parse(json).unwrap_or_default();
        self.inner.graphs.lock().unwrap().insert(key, g.clone());
        g
    }

    async fn load_graph(&self, run: &Run) -> Result<Graph> {
        let key = (run.workflow_id.clone(), run.workflow_version);
        if let Some(g) = self.inner.graphs.lock().unwrap().get(&key) {
            return Ok(g.clone());
        }
        let v = self
            .core()
            .store()
            .workflow_version(&run.workflow_id, run.workflow_version)
            .await?;
        Ok(self.graph(
            run,
            v.as_ref().map(|v| v.graph_json.as_str()).unwrap_or("{}"),
        ))
    }

    /// One run; returns other runs to advance next (queued runs of the same
    /// workflow when this one finished).
    async fn advance_locked(&self, run_id: &str) -> Result<Vec<String>> {
        let core = self.core();
        let s = core.store();
        if self.stopped().await {
            return Ok(vec![]);
        }
        // `autonomy.max_attempts`: the factory's stop-the-line limit for a
        // workflow that sets none of its own.
        let stop_line = kls_core::audit::config::Effective::load(core)
            .await
            .ok()
            .map(|e| e.max_attempts)
            .filter(|n| *n > 0);
        for _round in 0..100 {
            let Some(run) = s.run(run_id).await? else {
                return Ok(vec![]);
            };
            if !matches!(
                run.status.as_str(),
                "queued" | "running" | "waiting_approval"
            ) {
                return Ok(vec![]);
            }
            let mut g = self.load_graph(&run).await?;
            if g.limits.stop_the_line_attempts.is_none() {
                g.limits.stop_the_line_attempts = stop_line;
            }
            if run.status == "queued" {
                let active = s
                    .workflow_runs_in(&run.workflow_id, &["running", "waiting_approval"], None)
                    .await?;
                if active.len() as i64 >= g.concurrency() {
                    return Ok(vec![]);
                }
                // Factory-wide cap (`autonomy.concurrency`).
                if let Ok(e) = kls_core::audit::config::Effective::load(core).await
                    && e.concurrency > 0
                    && s.runs_in(&["running", "waiting_approval"]).await?.len() as i64
                        >= e.concurrency
                {
                    return Ok(vec![]);
                }
                if s.set_run_status(&run.id, &["queued"], "running", "")
                    .await?
                {
                    core.emit(
                        kinds::RUN_UPDATED,
                        &run.id,
                        &json!({ "status": "running", "action": "start" }),
                    )
                    .await?;
                }
                continue;
            }
            let steps = s.run_steps(&run.id).await?;
            let now = now_ms();

            // Due work first.
            let mut progressed = false;
            for st in &steps {
                let due = st.wake_at.is_none_or(|w| w <= now);
                if st.status == "pending" && due {
                    self.execute(&run, &g, st, &steps).await?;
                    progressed = true;
                } else if st.status == "waiting" && st.wake_at.is_some_and(|w| w <= now) {
                    self.on_wake(&run, &g, st, &steps).await?;
                    progressed = true;
                }
            }
            if progressed {
                continue;
            }

            let plan = walker::plan(&g, &steps, &run.trigger_node_id);
            if let Some(f) = plan.finish {
                return self.finish_run(&run, f).await;
            }
            if plan.start.is_empty() {
                self.settle_run_status(&run).await?;
                return Ok(vec![]);
            }
            for a in plan.start {
                let Some(n) = g.node(&a.node_id) else {
                    continue;
                };
                let wake = if a.retry {
                    Some(now + backoff(self.inner.config.retry_backoff_ms, a.attempt))
                } else if n.kind == "trigger" {
                    // A staggered cron fan-out starts this run later.
                    serde_json::from_str::<Value>(&run.inputs_json)
                        .ok()
                        .and_then(|i| i.get("notBefore").and_then(Value::as_i64))
                        .filter(|t| *t > now)
                } else {
                    None
                };
                let st = RunStep {
                    id: new_id(),
                    run_id: run.id.clone(),
                    node_id: n.id.clone(),
                    node_type: n.kind.clone(),
                    title: n.title.clone(),
                    attempt: a.attempt,
                    status: "pending".into(),
                    details_json: "{}".into(),
                    wake_at: wake,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                };
                if s.insert_step(&st).await? {
                    self.step_event(&st, "pending").await?;
                }
            }
        }
        Ok(vec![])
    }

    async fn settle_run_status(&self, run: &Run) -> Result<()> {
        let s = self.core().store();
        let pending = !s
            .approvals(Some("pending"), Some(&run.id), 1)
            .await?
            .is_empty();
        let (from, to) = if pending {
            ("running", "waiting_approval")
        } else {
            ("waiting_approval", "running")
        };
        if run.status == from && s.set_run_status(&run.id, &[from], to, "").await? {
            self.core()
                .emit(kinds::RUN_UPDATED, &run.id, &json!({ "status": to }))
                .await?;
        }
        Ok(())
    }

    async fn finish_run(&self, run: &Run, f: Finish) -> Result<Vec<String>> {
        let s = self.core().store();
        if !s
            .set_run_status(
                &run.id,
                &["queued", "running", "waiting_approval"],
                f.status,
                &f.reason,
            )
            .await?
        {
            return Ok(vec![]);
        }
        s.cancel_open_steps(&run.id, "run_ended").await?;
        s.cancel_run_approvals(&run.id).await?;
        self.core()
            .emit(
                kinds::RUN_FINISHED,
                &run.id,
                &json!({ "status": f.status, "reason": f.reason, "workflowId": run.workflow_id }),
            )
            .await?;
        // A slot is free: the oldest queued runs of the workflow may start,
        // then any queued run the factory-wide cap held back.
        let mut next: Vec<String> = s
            .workflow_runs_in(&run.workflow_id, &["queued"], None)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect();
        for r in s.runs_in(&["queued"]).await?.into_iter().take(20) {
            if !next.contains(&r.id) {
                next.push(r.id);
            }
        }
        Ok(next)
    }

    async fn step_event(&self, st: &RunStep, status: &str) -> Result<()> {
        self.core()
            .emit(
                kinds::RUN_STEP,
                &st.run_id,
                &json!({
                    "stepId": st.id, "nodeId": st.node_id, "status": status,
                    "attempt": st.attempt, "type": st.node_type,
                }),
            )
            .await?;
        Ok(())
    }

    async fn update(&self, st: &RunStep, from: &[&str], u: StepUpdate) -> Result<bool> {
        let ok = self.core().store().update_step(&st.id, from, &u).await?;
        if ok {
            self.step_event(st, u.status.as_deref().unwrap_or(&st.status))
                .await?;
        }
        Ok(ok)
    }

    async fn succeed(&self, st: &RunStep, route: &str, details: Value) -> Result<()> {
        self.update(
            st,
            &["running", "waiting", "pending"],
            StepUpdate {
                status: Some("succeeded".into()),
                route: Some(route.into()),
                error: Some(String::new()),
                details_json: Some(details.to_string()),
                wake_at: Some(None),
                finished: true,
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn fail(&self, st: &RunStep, error: &str, details: Value) -> Result<()> {
        self.update(
            st,
            &["running", "waiting", "pending"],
            StepUpdate {
                status: Some("failed".into()),
                error: Some(error.into()),
                details_json: Some(details.to_string()),
                wake_at: Some(None),
                finished: true,
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn wait(
        &self,
        st: &RunStep,
        error: &str,
        details: Value,
        wake_at: Option<i64>,
    ) -> Result<()> {
        self.update(
            st,
            &["running", "pending", "waiting"],
            StepUpdate {
                status: Some("waiting".into()),
                error: Some(error.into()),
                details_json: Some(details.to_string()),
                wake_at: Some(wake_at),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    async fn workflow_name(&self, run: &Run) -> String {
        self.core()
            .store()
            .workflow(&run.workflow_id)
            .await
            .ok()
            .flatten()
            .map(|w| w.name)
            .unwrap_or_default()
    }

    /// Runs one pending step.
    async fn execute(&self, run: &Run, g: &Graph, st: &RunStep, steps: &[RunStep]) -> Result<()> {
        let s = self.core().store();
        // Take the step (exactly once).
        let took = s
            .update_step(
                &st.id,
                &["pending"],
                &StepUpdate {
                    status: Some("running".into()),
                    wake_at: Some(None),
                    started: true,
                    ..Default::default()
                },
            )
            .await?;
        if !took {
            return Ok(());
        }
        self.step_event(st, "running").await?;
        let Some(node) = g.node(&st.node_id).cloned() else {
            return self.fail(st, "unknown_node", json!({})).await;
        };
        let mut st = st.clone();
        st.status = "running".into();
        let prior: Value = serde_json::from_str(&st.details_json).unwrap_or_else(|_| json!({}));
        let ctx = run_context(run, steps);
        match node.kind.as_str() {
            "trigger" => {
                let event: Value = serde_json::from_str(&run.event_json).unwrap_or(Value::Null);
                self.succeed(
                    &st,
                    "",
                    json!({
                        "trigger": run.trigger_kind, "label": run.trigger_label,
                        "workflowVersion": run.workflow_version, "event": event,
                    }),
                )
                .await
            }
            "condition" => match evaluate_condition(&node, &ctx) {
                Some(o) => {
                    let rule = node.rules().iter().position(|r| {
                        r.outcome == o && rule_matches(&r.op, lookup(&ctx, &r.path), &r.value)
                    });
                    self.succeed(
                        &st,
                        &o,
                        json!({ "outcome": o, "rule": rule, "decision": "rules" }),
                    )
                    .await
                }
                None => self.fail(&st, "no_outcome", json!({})).await,
            },
            "notify" => {
                let wf = self.workflow_name(run).await;
                let msg = fill_message(node.str("message"), &wf, run, &node, &run.status);
                let severity = match node.str("severity") {
                    "" => "info",
                    v => v,
                };
                self.core()
                    .emit(
                        kinds::RUN_NOTIFY,
                        &run.id,
                        &json!({ "message": msg, "severity": severity, "nodeId": node.id, "workflowId": run.workflow_id }),
                    )
                    .await?;
                self.succeed(&st, "", json!({ "message": msg, "severity": severity }))
                    .await
            }
            "wait" => {
                let event = node.str("event").trim().to_string();
                if !event.is_empty() {
                    let timeout = node.int("timeoutMinutes").map(|m| now_ms() + m * 60_000);
                    let mut d = json!({ "waitEvent": event, "cardId": run.card_id });
                    if let Some(m) = node.config.get("match") {
                        d["match"] = m.clone();
                    }
                    self.wait(&st, "", d, timeout).await
                } else {
                    let minutes = node.int("minutes").unwrap_or(0).max(0);
                    if minutes == 0 {
                        self.succeed(&st, "", json!({ "waited": 0 })).await
                    } else {
                        self.wait(
                            &st,
                            "",
                            json!({ "waitMinutes": minutes }),
                            Some(now_ms() + minutes * 60_000),
                        )
                        .await
                    }
                }
            }
            "human" => {
                self.ask_person(run, &node, &st, "human", node.title.clone())
                    .await
            }
            "gate" => self.run_gate(run, &node, &st, ctx).await,
            "decision" => self.run_decision(run, &node, &st, steps, prior).await,
            "crew" => self.dispatch(run, &node, &st, prior).await,
            "scan" => {
                // A CLI job runs as a card the CLI runner claims (`cli-job:<id>`).
                let job = node.str("job").to_string();
                let mut n = node.clone();
                n.kind = "crew".into();
                let agent = if crate::catalogue::cli_write_job(&job) {
                    "belai:cli-runner-write"
                } else {
                    "belai:cli-runner"
                };
                n.config.insert("dispatch".into(), json!("worker"));
                n.config.insert("agent".into(), json!(agent));
                n.config.insert("cliJob".into(), json!(job));
                if n.str("brief").is_empty() {
                    n.config.insert(
                        "brief".into(),
                        json!(format!("Run the Vulnetix CLI {job} job on this repository and attach its result to this card.")),
                    );
                }
                self.dispatch(run, &n, &st, prior).await
            }
            "profile" => self.store_profile(run, &node, &st, steps, ctx).await,
            "provision" => self.run_provision(run, &node, &st, prior).await,
            "job" => self.start_job(run, &node, &st, prior).await,
            "card" => {
                let card = match node.str("card") {
                    "" => run.card_id.clone(),
                    c => c.to_string(),
                };
                if card.is_empty() {
                    return self
                        .fail(&st, "no_card", json!({ "why": "the run has no card" }))
                        .await;
                }
                let Some(board) = self.inner.integrations.board.clone() else {
                    return self.fail(&st, "integration_missing:board", json!({})).await;
                };
                let action = match node.str("action") {
                    "" => "move",
                    a => a,
                };
                match board
                    .apply(&card, action, &Value::Object(node.config.clone()))
                    .await
                {
                    Ok(v) => self.succeed(&st, "", v).await,
                    Err(e) => {
                        self.fail(&st, "card_error", json!({ "error": format!("{e:#}") }))
                            .await
                    }
                }
            }
            "github" if node.str("action") == "conflict_cards" => {
                self.conflict_cards(run, &node, &st, prior).await
            }
            "github" => {
                // `base: pr_head` stacks the pull request onto the branch the
                // run is about (the failing pull request's head, or the
                // branch a workflow run failed on), not a branch called
                // "pr_head".
                let mut node = node.clone();
                if node.str("base") == "pr_head" {
                    let base = pr_head_of(run);
                    node.config.insert(
                        "base".into(),
                        json!(if base.is_empty() { "default" } else { &base }),
                    );
                }
                let Some(forge) = self.inner.integrations.forge.clone() else {
                    return self
                        .fail(
                            &st,
                            "integration_missing:github",
                            json!({ "why": "GitHub is not connected" }),
                        )
                        .await;
                };
                let action = node.str("action");
                let need = guard::required_autonomy(action);
                let have = guard::effective_autonomy(self.core(), &run.repo).await;
                let origin = Origin::engine(
                    json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id, "node": node.id }),
                );
                let target = json!({ "repo": run.repo, "action": action });
                if guard::autonomy_rank(&have) < guard::autonomy_rank(need) {
                    let why = format!("autonomy {have} is below {need}");
                    guard::refuse(self.core(), "github", &why, json!({ "runId": run.id, "stepId": st.id, "repo": run.repo, "action": action }),
                    )
                    .await?;
                    // Skipped, not failed: the rest of the run still goes on.
                    return self
                        .succeed(
                            &st,
                            "",
                            json!({ "skipped": "autonomy", "required": need, "autonomy": have, "action": action }),
                        )
                        .await;
                }
                let sc = StepContext {
                    run: run.clone(),
                    node: node.clone(),
                    step: st.clone(),
                    context: ctx,
                };
                let res = forge.action(&sc, action).await;
                let outcome = match &res {
                    Ok(_) => "done".to_string(),
                    Err(e) => format!("failed: {e:#}"),
                };
                guard::audit(
                    self.core(),
                    "github",
                    action,
                    target,
                    &origin,
                    &outcome,
                    json!({}),
                )
                .await?;
                match res {
                    // `approve_ready` not ready yet (and not for good): wait
                    // like a forge gate, woken by pull, review and check
                    // events or the recheck timer, for at most 72 hours.
                    Ok(v)
                        if action == "approve_ready"
                            && v.get("approved").and_then(Value::as_bool) == Some(false)
                            && !v.get("terminal").and_then(Value::as_bool).unwrap_or(false) =>
                    {
                        let since = prior
                            .get("readinessSince")
                            .and_then(Value::as_i64)
                            .unwrap_or_else(now_ms);
                        if now_ms() - since > READINESS_MAX_MS {
                            let mut v = v;
                            v["skipped"] = json!("readiness_timeout");
                            return self.succeed(&st, "", v).await;
                        }
                        let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
                        let mut d = v.clone();
                        d["readinessSince"] = json!(since);
                        self.wait(
                            &st,
                            &format!("{READINESS_WAIT}:{reason}"),
                            d,
                            Some(now_ms() + self.inner.config.gate_recheck_ms),
                        )
                        .await
                    }
                    Ok(v) => self.succeed(&st, "", v).await,
                    Err(e) => {
                        self.fail(&st, "github_error", json!({ "error": format!("{e:#}") }))
                            .await
                    }
                }
            }
            other => {
                self.fail(&st, &format!("unsupported:{other}"), json!({}))
                    .await
            }
        }
    }

    async fn ask_person(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        kind: &str,
        title: String,
    ) -> Result<()> {
        let core = self.core();
        let now = now_ms();
        let sla_hours = node.int("slaHours").unwrap_or(24).clamp(1, 720);
        let sla_at = now + sla_hours * 3_600_000;
        let approvers = node
            .config
            .get("approvers")
            .cloned()
            .unwrap_or_else(|| json!(["owner"]));
        let wf = self.workflow_name(run).await;
        let a = Approval {
            id: new_id(),
            run_id: run.id.clone(),
            step_id: st.id.clone(),
            node_id: node.id.clone(),
            kind: kind.into(),
            state: "pending".into(),
            title: if title.trim().is_empty() {
                format!("{wf}: approval")
            } else {
                title
            },
            approvers_json: approvers.to_string(),
            context_json: json!({
                "workflow": wf, "repo": run.repo, "cardId": run.card_id,
                "message": node.str("message"), "nodeTitle": node.title,
            })
            .to_string(),
            note: String::new(),
            decided_by: String::new(),
            revision: 1,
            sla_at: Some(sla_at),
            created_at: now,
            updated_at: now,
            decided_at: None,
        };
        core.store().insert_approval(&a).await?;
        let mut d: Value = serde_json::from_str(&st.details_json).unwrap_or_else(|_| json!({}));
        if !d.is_object() {
            d = json!({});
        }
        d["approvalId"] = json!(a.id);
        d["slaAt"] = json!(sla_at);
        self.wait(st, "", d, Some(sla_at)).await?;
        core.emit(
            kinds::APPROVAL_REQUESTED,
            &a.id,
            &json!({ "runId": run.id, "stepId": st.id, "nodeId": node.id, "title": a.title }),
        )
        .await?;
        Ok(())
    }

    async fn run_gate(&self, run: &Run, node: &Node, st: &RunStep, ctx: Value) -> Result<()> {
        let forge = self.inner.integrations.forge.clone();
        if node.str("check") != "forge" || forge.is_none() {
            let note = if node.str("check") == "forge" {
                "GitHub is not connected, so a person passes or fails this gate"
            } else {
                "a person passes or fails this gate"
            };
            return self
                .wait(st, "deferred:gate", json!({ "note": note }), None)
                .await;
        }
        let sc = StepContext {
            run: run.clone(),
            node: node.clone(),
            step: st.clone(),
            context: ctx,
        };
        match forge.unwrap().gate(&sc).await {
            Ok(GateResult::Pass(v)) => self.succeed(st, "pass", v).await,
            Ok(GateResult::Pending) => {
                self.wait(
                    st,
                    "deferred:checks",
                    json!({ "note": "checks are still running" }),
                    Some(now_ms() + self.inner.config.gate_recheck_ms),
                )
                .await
            }
            Ok(GateResult::Fail(why)) => {
                if node.str("onFail") == "human_approval" {
                    self.ask_person(
                        run,
                        node,
                        st,
                        "gate",
                        format!("{} failed: {why}", node.title),
                    )
                    .await
                } else {
                    self.fail(st, "gate_failed", json!({ "why": why })).await
                }
            }
            Err(e) => {
                self.wait(
                    st,
                    "deferred:checks",
                    json!({ "error": format!("{e:#}") }),
                    Some(now_ms() + self.inner.config.gate_recheck_ms),
                )
                .await
            }
        }
    }

    /// A node to run on, or why there is none (the step then waits).
    async fn pick_node(&self, node: &Node, run: &Run) -> Result<String, String> {
        let Ok(gw) = self.core().gateway() else {
            return Err("no node server runs in this process".into());
        };
        let mut online = gw.online().await;
        online.sort();
        // A step with no run-on of its own takes the settings' default.
        let eff = kls_core::audit::config::Effective::load(self.core())
            .await
            .ok();
        let run_on = match node.str("runOn") {
            "" => eff.as_ref().map(|e| e.run_on.clone()).unwrap_or_default(),
            r => r.to_string(),
        };
        let fallback = match node.str("runOnFallback") {
            "" => eff
                .as_ref()
                .map(|e| e.run_on_fallback.clone())
                .unwrap_or_default(),
            f => f.to_string(),
        };
        match run_on.strip_prefix("node:") {
            Some(id) if online.iter().any(|o| o == id) => Ok(id.to_string()),
            Some(_) if fallback == "fail" => Err("fail:the chosen node is offline".into()),
            Some(_) if fallback != "any" => Err("the chosen node is offline".into()),
            _ => {
                let nodes = self.core().store().nodes().await.unwrap_or_default();
                let repo = match node.str("repo") {
                    "" => run.repo.as_str(),
                    r => r,
                };
                let crew = if node.str("dispatch") == "crew" {
                    node.str("crew")
                } else {
                    ""
                };
                // Row 48: skip nodes whose plan window for the step's provider
                // is nearly used (kept when every node is).
                let online =
                    kls_core::belai::plan::route_filter(self.core(), online, node.str("provider"))
                        .await;
                crate::route::pick_node(&nodes, &online, node.str("agent"), crew, repo)
            }
        }
    }

    async fn no_host(&self, st: &RunStep, why: &str, mut prior: Value) -> Result<()> {
        let first = prior
            .get("waitingSince")
            .and_then(Value::as_i64)
            .unwrap_or_else(now_ms);
        let ttl = kls_core::audit::config::Effective::load(self.core())
            .await
            .map(|e| e.no_host_ttl_ms)
            .ok()
            .filter(|t| *t > 0)
            .unwrap_or(self.inner.config.no_host_ttl_ms);
        if now_ms() - first > ttl {
            return self
                .fail(st, "no_host_timeout", json!({ "why": why }))
                .await;
        }
        if !prior.is_object() {
            prior = json!({});
        }
        prior["waitingSince"] = json!(first);
        prior["why"] = json!(why);
        self.wait(
            st,
            "deferred:no_host",
            prior,
            Some(now_ms() + self.inner.config.no_host_retry_ms),
        )
        .await
    }

    /// Files (or routes) the card a worker step's agent will claim, and
    /// returns its id; `None` for plain sessions and agents with no route.
    /// The run's own card is routed when it has one; a retry reuses the
    /// card the step filed before.
    async fn file_route_card(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        wf: &str,
        brief: &str,
    ) -> Result<Option<String>> {
        let core = self.core();
        let dispatch = match node.str("dispatch") {
            "" => "worker",
            d => d,
        };
        let mut saved = HashMap::new();
        for p in core.store().agent_profiles().await.unwrap_or_default() {
            saved.insert(
                p.name.clone(),
                crate::catalogue::ProfileSpec::parse(&p.meta_json),
            );
        }
        let lookup = |n: &str| saved.get(n).cloned();
        let cli_job = Some(node.str("cliJob")).filter(|j| !j.is_empty());
        let Some(r) = crate::route::route_for(
            dispatch,
            node.str("agent"),
            node.str("crew"),
            cli_job,
            &lookup,
        ) else {
            return Ok(None);
        };
        let tag = |c: &kls_store::board::Card| {
            let m: Value = serde_json::from_str(&c.meta_json).unwrap_or(Value::Null);
            m.pointer("/keleustes/runId").and_then(Value::as_str) == Some(run.id.as_str())
                && m.pointer("/keleustes/nodeId").and_then(Value::as_str) == Some(node.id.as_str())
        };
        let existing = if !run.card_id.is_empty() {
            core.card(&run.card_id).await?.filter(|c| !c.deleted)
        } else {
            core.cards().await?.into_iter().find(|c| tag(c))
        };
        let meta_for = |prior: &str| {
            let m = crate::route::card_meta(&r, &run.id, &st.id, &run.workflow_id, prior);
            let mut v: Value = serde_json::from_str(&m).unwrap_or_else(|_| json!({}));
            v["keleustes"]["nodeId"] = json!(node.id);
            v.to_string()
        };
        let card = match existing {
            Some(c) => {
                let mut labels = c.labels.clone();
                for l in &r.labels {
                    if !labels.iter().any(|x| x.eq_ignore_ascii_case(l)) {
                        labels.push(l.clone());
                    }
                }
                let p = kls_core::board::CardPatch {
                    status: Some(r.status.clone()),
                    labels: Some(labels),
                    meta_json: Some(meta_for(&c.meta_json)),
                    ..Default::default()
                };
                core.update_card(&c.id, c.version, p, "engine").await?
            }
            None => {
                let d = kls_core::board::CardDraft {
                    title: format!("{wf}: {}", node.title)
                        .chars()
                        .take(kls_core::board::TITLE_MAX)
                        .collect(),
                    body: crate::route::card_body(brief, wf, &run.id, &node.title, &run.repo),
                    status: r.status.clone(),
                    labels: r.labels.clone(),
                    repo: run.repo.clone(),
                    meta_json: meta_for("{}"),
                    ..Default::default()
                };
                core.create_card(d, "engine").await?
            }
        };
        core.emit(
            kinds::ROUTE_FILED,
            &card.id,
            &json!({ "runId": run.id, "stepId": st.id, "cardId": card.id, "agent": r.agent, "labels": r.labels, "list": r.list }),
        )
        .await?;
        guard::audit(
            core,
            "card",
            "route.file",
            json!({ "cardId": card.id, "key": card.key, "agent": r.agent }),
            &Origin::engine(json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id, "node": node.id })),
            "filed",
            json!({ "labels": r.labels, "list": r.list }),
        )
        .await?;
        Ok(Some(card.id))
    }

    async fn dispatch(&self, run: &Run, node: &Node, st: &RunStep, prior: Value) -> Result<()> {
        // The kill switch, then a maintenance window, hold the dispatch.
        if let Some(_why) = guard::held(self.core(), "dispatch").await {
            guard::refuse_switch(
                self.core(),
                "dispatch",
                json!({ "runId": run.id, "stepId": st.id, "nodeId": node.id }),
            )
            .await?;
            return self
                .wait(st, "deferred:kill_switch", prior, Some(now_ms() + 60_000))
                .await;
        }
        let kind = match node.str("dispatch") {
            "" => "worker",
            k => k,
        };
        // A crew's members talk to each other (A2A): the blackout holds it.
        if kind == "crew"
            && let Some(why) = guard::a2a(
                self.core(),
                &format!("workflow:{}", run.workflow_id),
                &format!("crew:{}", node.str("crew")),
                &format!("run:{}", run.id),
            )
            .await
        {
            let mut d = if prior.is_object() { prior } else { json!({}) };
            d["why"] = json!(why);
            return self
                .wait(st, "deferred:a2a", d, Some(now_ms() + 60_000))
                .await;
        }
        let node_id = match self.pick_node(node, run).await {
            Ok(n) => n,
            Err(why) if why.starts_with("fail:") => {
                return self
                    .fail(
                        st,
                        "node_offline",
                        json!({ "why": why.trim_start_matches("fail:") }),
                    )
                    .await;
            }
            Err(why) if why.starts_with(crate::route::NO_HOST_FOR_REPO) => {
                // No node holds the repository: refused, not waited on.
                guard::refuse(
                    self.core(),
                    "dispatch",
                    crate::route::NO_HOST_FOR_REPO,
                    json!({ "runId": run.id, "stepId": st.id, "repo": run.repo, "why": why }),
                )
                .await?;
                return self
                    .fail(
                        st,
                        crate::route::NO_HOST_FOR_REPO,
                        json!({ "why": why, "repo": run.repo }),
                    )
                    .await;
            }
            Err(why) => return self.no_host(st, &why, prior).await,
        };
        let wf = self.workflow_name(run).await;
        let brief = node.str("brief").trim();
        let mut prompt = String::new();
        if !brief.is_empty() {
            prompt.push_str(brief);
            prompt.push_str("\n\n");
        }
        prompt.push_str(&format!(
            "Started by the Keleustes workflow \"{wf}\" (run {}, step \"{}\").",
            short_id(&run.id),
            node.title
        ));
        let (provider, model, route_source) = self.step_model(run, node).await;
        let planned = node.budget.map(|b| b.tokens).unwrap_or(0).max(0);
        let Some((provider, model, tier)) = self
            .budget_check(run, node, st, &prior, &provider, &model, &node_id, planned)
            .await?
        else {
            return Ok(());
        };
        let eff = kls_core::audit::config::Effective::load(self.core())
            .await
            .ok();
        let g = self.load_graph(run).await?;
        let budget = policy::worker_budget(eff.as_ref(), &g.limits, planned);
        let member_models = if kind == "crew" {
            self.member_models(node).await
        } else {
            Default::default()
        };
        // Start fields (#7, #8, #26): checked against the kind before
        // anything is filed or sent.
        let fields = match start_fields(node, kind) {
            Ok(f) => f,
            Err(why) => {
                guard::refuse(
                    self.core(),
                    "dispatch",
                    "invalid_start_fields",
                    json!({ "runId": run.id, "stepId": st.id, "why": why }),
                )
                .await?;
                return self
                    .fail(st, "invalid_start_fields", json!({ "why": why }))
                    .await;
            }
        };
        let route_card = match self.file_route_card(run, node, st, &wf, brief).await {
            Ok(c) => c,
            Err(e) => {
                return self
                    .fail(st, "route_error", json!({ "error": format!("{e:#}") }))
                    .await;
            }
        };
        let mut spec = DispatchSpec {
            kind: kind.into(),
            cwd: node.str("cwd").into(),
            repo: match node.str("repo") {
                "" => run.repo.clone(),
                r => r.into(),
            },
            branch: node.str("branch").into(),
            prompt,
            provider,
            model,
            agent: node.str("agent").into(),
            crew: node.str("crew").into(),
            card_id: route_card.clone().unwrap_or_else(|| run.card_id.clone()),
            run_id: run.id.clone(),
            mode: match node.str("mode") {
                "" => "agent".into(),
                m => m.into(),
            },
            vault_providers: node
                .config
                .get("vaultProviders")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            options: crew_options(eff.as_ref(), node),
            budget: Some(budget),
            tier,
            member_models,
            ..Default::default()
        };
        fields.apply(&mut spec);
        let gw = self.core().gateway()?;
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            gw.dispatch(&node_id, spec.clone()),
        )
        .await;
        let origin = Origin::engine(json!({
            "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id, "node": node.id,
        }));
        let target = json!({ "nodeId": node_id, "agent": spec.agent, "crew": spec.crew, "kind": spec.kind, "cardId": spec.card_id });
        let dispatch_id = match res {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                let err = format!("{e:#}");
                guard::audit(
                    self.core(),
                    "dispatch",
                    "dispatch",
                    target,
                    &origin,
                    &format!("refused: {err}"),
                    json!({ "nodeId": node_id }),
                )
                .await?;
                // A full node is a wait, not a spent attempt.
                let lower = err.to_ascii_lowercase();
                if ["capacity", "worker cap", "busy", "full"]
                    .iter()
                    .any(|w| lower.contains(w))
                {
                    let mut d = prior.clone();
                    if !d.is_object() {
                        d = json!({});
                    }
                    d["why"] = json!(err);
                    return self
                        .wait(st, "deferred:capacity", d, Some(now_ms() + 600_000))
                        .await;
                }
                return self
                    .fail(
                        st,
                        "dispatch_error",
                        json!({ "error": err, "nodeId": node_id }),
                    )
                    .await;
            }
            Err(_) => {
                return self
                    .fail(
                        st,
                        "dispatch_error",
                        json!({ "error": "the node did not answer in time", "nodeId": node_id }),
                    )
                    .await;
            }
        };
        guard::audit(
            self.core(),
            "dispatch",
            "dispatch",
            target,
            &origin,
            "sent",
            json!({ "nodeId": node_id, "dispatchId": dispatch_id, "provider": spec.provider,
                    "model": spec.model, "tier": spec.tier, "modelFrom": route_source,
                    "fields": fields.audit_json() }),
        )
        .await?;
        let details = json!({
            "dispatchId": dispatch_id, "nodeId": node_id, "kind": spec.kind,
            "agent": spec.agent, "crew": spec.crew, "briefChars": brief.chars().count(),
            "routeCardId": route_card, "provider": spec.provider, "model": spec.model,
            "tier": spec.tier, "modelFrom": route_source, "budget": spec.budget,
            "memberModels": spec.member_models, "fields": fields.audit_json(),
        });
        let s = self.core().store();
        s.update_step(
            &st.id,
            &[],
            &StepUpdate {
                dispatch_id: Some(Some(dispatch_id.clone())),
                node_ref: Some(Some(node_id.clone())),
                ..Default::default()
            },
        )
        .await?;
        if node.str("await") == "none" {
            return self.succeed(st, "", details).await;
        }
        let timeout = node.int("timeoutMinutes").unwrap_or(240).clamp(1, 10_080);
        let timeout_at = now_ms() + timeout * 60_000;
        let mut details = details;
        details["timeoutAt"] = json!(timeout_at);
        // A routing card nobody claims within `autonomy.claim_ttl_minutes`
        // fails the attempt (`card_unclaimed`).
        let mut wake = timeout_at;
        if route_card.is_some() {
            let ttl = kls_core::audit::config::Effective::load(self.core())
                .await
                .map(|e| e.claim_ttl_ms)
                .unwrap_or(3_600_000);
            if ttl > 0 {
                wake = wake.min(now_ms() + ttl);
            }
        }
        self.wait(st, "", details, Some(wake)).await
    }

    /// A SystemOne decision: a `model` job (`task: systemone`) on a node;
    /// the threshold and abstain rules are applied when it answers.
    async fn run_decision(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        steps: &[RunStep],
        prior: Value,
    ) -> Result<()> {
        let core = self.core();
        if let Some(_why) = guard::held(core, "job").await {
            guard::refuse_switch(core, "job", json!({ "runId": run.id, "stepId": st.id })).await?;
            return self
                .wait(st, "deferred:kill_switch", prior, Some(now_ms() + 60_000))
                .await;
        }
        let Ok(gw) = core.gateway() else {
            return self
                .no_host(st, "no node server runs in this process", prior)
                .await;
        };
        let target = node.str("runOn").strip_prefix("node:").map(str::to_string);
        let online = gw.online().await;
        match &target {
            Some(id) if !online.contains(id) => {
                return self.no_host(st, "the chosen node is offline", prior).await;
            }
            None if online.is_empty() => return self.no_host(st, "no node is online", prior).await,
            _ => {}
        }
        let wf = self.workflow_name(run).await;
        let payload = crate::decision::payload(node, crate::decision::state(run, steps, &wf));
        let eff = kls_core::audit::config::Effective::load(core).await.ok();
        let setting = eff.as_ref().map(|e| e.min_confidence).filter(|c| *c > 0.0);
        let threshold = crate::decision::threshold(node, setting);
        // The daily decision cap: past it, a person decides.
        let (cap, capped) = self.decision_capped().await;
        if capped {
            let a = crate::decision::Applied {
                action: crate::decision::Action::Human(String::new()),
                choice: String::new(),
                confidence: 0.0,
                threshold,
                why: "daily_cap",
                provider: String::new(),
                model: String::new(),
            };
            guard::refuse(
                core,
                "decision",
                &format!("daily_cap: {cap} decisions today"),
                json!({ "runId": run.id, "stepId": st.id, "cap": cap }),
            )
            .await?;
            return self.settle_decision(run, node, st, &a, "", 0).await;
        }
        // The budgets before the model call.
        let (sp, sm) = eff
            .as_ref()
            .and_then(|e| e.systemone_model.split_once('/'))
            .map(|(p, m)| (p.to_string(), m.to_string()))
            .unwrap_or_default();
        let planned = node.budget.map(|b| b.tokens).unwrap_or(0).max(0);
        let job_node = target.clone().unwrap_or_default();
        if self
            .budget_check(run, node, st, &prior, &sp, &sm, &job_node, planned)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let timeout_ms = node.int("timeoutSeconds").unwrap_or(120).clamp(5, 3_600) * 1000;
        let engine = self.clone();
        let st = st.clone();
        // The System One settings fill what the node leaves unset.
        let mut node = node.clone();
        if let Some(e) = &eff {
            for (k, v) in [
                ("belowThreshold", &e.systemone_below_threshold),
                ("onAbstain", &e.systemone_on_abstain),
            ] {
                if node.str(k).is_empty() && !v.is_empty() {
                    node.config.insert(k.into(), json!(v));
                }
            }
        }
        let mut payload = payload;
        if let Some(e) = &eff {
            if !e.systemone_model.is_empty() {
                payload["model"] = json!(e.systemone_model);
            }
            if !e.systemone_fallback.is_empty() {
                payload["fallback"] = json!(e.systemone_fallback);
            }
        }
        let run = run.clone();
        tokio::spawn(async move {
            let t0 = now_ms();
            let out = gw
                .job(
                    target.as_deref(),
                    "model",
                    &payload.to_string(),
                    JobNeeds::default(),
                    timeout_ms,
                )
                .await;
            let latency = now_ms() - t0;
            let (output, node_ref) = match &out {
                Ok(o) if o.status == "ok" => (
                    serde_json::from_str::<Value>(&o.output_json).ok(),
                    o.node_id.clone(),
                ),
                Ok(o) => (None, o.node_id.clone()),
                Err(_) => (None, String::new()),
            };
            let a = crate::decision::apply(&node, output.as_ref(), threshold);
            if let Err(e) = engine
                .settle_decision(&run, &node, &st, &a, &node_ref, latency)
                .await
            {
                tracing::warn!("decision step: {e:#}");
            }
            if let Err(e) = engine.advance(&run.id).await {
                tracing::warn!("advance after decision: {e:#}");
            }
        });
        Ok(())
    }

    async fn settle_decision(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        a: &crate::decision::Applied,
        node_ref: &str,
        latency_ms: i64,
    ) -> Result<()> {
        use crate::decision::Action;
        let core = self.core();
        let (action, outcome) = match &a.action {
            Action::Route(o) => ("route", o.clone()),
            Action::Human(o) => ("human", o.clone()),
            Action::Fail(e) => ("fail", e.clone()),
        };
        // The decision row (rationale, retention) and the day's stats.
        if let Err(e) = self
            .record_decision(run, node, st, a, action, &outcome, node_ref, latency_ms)
            .await
        {
            tracing::warn!("decision record: {e:#}");
        }
        let details = json!({
            "decision": "systemone", "question": node.str("question"), "choice": a.choice,
            "confidence": a.confidence, "threshold": a.threshold, "why": a.why,
            "action": action, "outcome": outcome, "suggested": outcome,
            "provider": a.provider, "model": a.model, "nodeId": node_ref, "calibrated": false,
        });
        core.emit(
            kinds::DECISION_MADE,
            &run.id,
            &json!({
                "runId": run.id, "stepId": st.id, "nodeId": node.id, "workflowId": run.workflow_id,
                "question": node.str("question"), "outcome": outcome, "confidence": a.confidence,
                "threshold": a.threshold, "action": action, "why": a.why,
                "provider": a.provider, "model": a.model,
            }),
        )
        .await?;
        guard::audit(
            core,
            "decision",
            "decision",
            json!({ "runId": run.id, "stepId": st.id, "node": node.id }),
            &Origin {
                actor: if a.model.is_empty() { "systemone".into() } else { format!("systemone:{}", a.model) },
                kind: "agent",
                detail: json!({ "workflowId": run.workflow_id, "provider": a.provider, "nodeId": node_ref }),
            },
            &format!("{} {outcome} ({:.2} vs {:.2})", a.why, a.confidence, a.threshold),
            json!({ "nodeId": node_ref }),
        )
        .await?;
        match &a.action {
            Action::Route(o) => self.succeed(st, o, details).await,
            Action::Fail(e) => self.fail(st, e, details).await,
            Action::Human(o) => {
                self.update(
                    st,
                    &["running"],
                    StepUpdate {
                        details_json: Some(details.to_string()),
                        ..Default::default()
                    },
                )
                .await?;
                let mut st = st.clone();
                st.details_json = details.to_string();
                let title = if o.is_empty() {
                    format!("Decide: {}", node.str("question"))
                } else {
                    format!("Decide: {} (suggested {o})", node.str("question"))
                };
                self.ask_person(run, node, &st, "decision", title).await
            }
        }
    }

    /// The workflow's host origin for a provision step (host-proto R8).
    fn provision_origin(run: &Run) -> kls_core::hosts::Origin {
        kls_core::hosts::Origin {
            actor: "owner".into(),
            surface: format!("workflow:{}", run.workflow_id),
            run_id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            plan_id: String::new(),
            reason: format!("workflow run {}", short_id(&run.id)),
        }
    }

    /// A `provision` step: previews every step of a host plan, then runs it
    /// with the host name captured when the workflow was published, and
    /// waits for `host.plan.finished`. Kill switch first; the hosts API
    /// checks it again before every plan step.
    async fn run_provision(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        prior: Value,
    ) -> Result<()> {
        let core = self.core();
        let target = node.str("node").trim_start_matches("node:").to_string();
        if guard::blocked(
            core,
            "provision",
            &target,
            &format!("workflow:{}", run.workflow_id),
        )
        .await
        .is_some()
        {
            return self
                .wait(st, "deferred:kill_switch", prior, Some(now_ms() + 60_000))
                .await;
        }
        let plan = node.str("planId").to_string();
        match kls_core::hosts::preview_plan(core, &plan, &target, Self::provision_origin(run)).await
        {
            Ok(rows) => {
                let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
                guard::audit(
                    core,
                    "provision",
                    "provision.preview",
                    json!({ "planId": plan, "nodeId": target }),
                    &Origin::engine(
                        json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id }),
                    ),
                    "requested",
                    json!({ "previews": ids, "nodeId": target }),
                )
                .await?;
                self.wait(
                    st,
                    "",
                    json!({ "phase": "preview", "planId": plan, "nodeId": target, "previews": ids,
                            "timeoutAt": now_ms() + 3_600_000 }),
                    Some(now_ms() + PROVISION_POLL_MS),
                )
                .await
            }
            Err(e) => {
                self.fail(
                    st,
                    "provision_refused",
                    json!({ "error": format!("{e:#}") }),
                )
                .await
            }
        }
    }

    /// A provision step's timer: previews done -> run the plan; a run
    /// finished (also settled by `host.plan.finished`) -> settle.
    async fn wake_provision(
        &self,
        run: &Run,
        node: &Node,
        st: &RunStep,
        mut d: Value,
    ) -> Result<()> {
        let core = self.core();
        if d.get("timeoutAt")
            .and_then(Value::as_i64)
            .is_some_and(|t| t < now_ms())
        {
            return self.fail(st, "timeout", d).await;
        }
        let target = d
            .get("nodeId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let plan = d
            .get("planId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if d.get("phase").and_then(Value::as_str) == Some("preview") {
            let ids: Vec<String> = d
                .get("previews")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let mut done = 0;
            for id in &ids {
                match core.store().host_action(id).await? {
                    Some(r) if r.state == "done" => done += 1,
                    Some(r) if matches!(r.state.as_str(), "failed" | "cancelled" | "refused") => {
                        d["failedPreview"] = json!(id);
                        return self.fail(st, "provision_preview_failed", d).await;
                    }
                    _ => {}
                }
            }
            if done < ids.len() {
                return self
                    .wait(st, "", d, Some(now_ms() + PROVISION_POLL_MS))
                    .await;
            }
            if guard::blocked(
                core,
                "provision",
                &target,
                &format!("workflow:{}", run.workflow_id),
            )
            .await
            .is_some()
            {
                return self.wait(st, "", d, Some(now_ms() + 60_000)).await;
            }
            let typed = node.str("confirmName").to_string();
            return match kls_core::hosts::run_plan(
                core,
                &plan,
                &target,
                &typed,
                &ids,
                Self::provision_origin(run),
            )
            .await
            {
                Ok(plan_run) => {
                    d["phase"] = json!("run");
                    d["planRunId"] = json!(plan_run);
                    guard::audit(
                        core,
                        "provision",
                        "provision.run",
                        json!({ "planId": plan, "nodeId": target, "planRunId": plan_run }),
                        &Origin::engine(json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id })),
                        "started",
                        json!({ "nodeId": target }),
                    )
                    .await?;
                    self.wait(st, "", d, Some(now_ms() + PROVISION_POLL_MS))
                        .await
                }
                Err(e) => {
                    d["error"] = json!(format!("{e:#}"));
                    self.fail(st, "provision_refused", d).await
                }
            };
        }
        let id = d
            .get("planRunId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match core.store().host_plan_run(&id).await? {
            Some(r) if r.state == "done" || r.state == "succeeded" => self.succeed(st, "", d).await,
            Some(r) if r.state != "running" => {
                d["planState"] = json!(r.state);
                d["error"] = json!(r.error);
                self.fail(st, "provision_failed", d).await
            }
            _ => {
                self.wait(st, "", d, Some(now_ms() + PROVISION_POLL_MS))
                    .await
            }
        }
    }

    async fn start_job(&self, run: &Run, node: &Node, st: &RunStep, prior: Value) -> Result<()> {
        if let Some(_why) = guard::held(self.core(), "job").await {
            guard::refuse_switch(
                self.core(),
                "job",
                json!({ "runId": run.id, "stepId": st.id }),
            )
            .await?;
            return self
                .wait(st, "deferred:kill_switch", prior, Some(now_ms() + 60_000))
                .await;
        }
        let Ok(gw) = self.core().gateway() else {
            return self
                .no_host(st, "no node server runs in this process", prior)
                .await;
        };
        let target = node.str("runOn").strip_prefix("node:").map(str::to_string);
        let online = gw.online().await;
        match &target {
            Some(id) if !online.contains(id) => {
                return self.no_host(st, "the chosen node is offline", prior).await;
            }
            None if online.is_empty() => return self.no_host(st, "no node is online", prior).await,
            _ => {}
        }
        // The budgets before the job (a job may call a model).
        let (jp, jm) = match node.str("model").split_once('/') {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => (
                node.str("provider").to_string(),
                node.str("model").to_string(),
            ),
        };
        let planned = node.budget.map(|b| b.tokens).unwrap_or(0).max(0);
        let job_node = target.clone().unwrap_or_default();
        if self
            .budget_check(run, node, st, &prior, &jp, &jm, &job_node, planned)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let kind = node.str("job").to_string();
        let mut payload = node
            .config
            .get("payload")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if payload.is_object() {
            payload["runId"] = json!(run.id);
            if !run.repo.is_empty() {
                payload["repo"] = json!(run.repo);
            }
        }
        let needs = JobNeeds {
            gh: node.bool("needsGh"),
            provider: Some(node.str("provider").to_string()).filter(|p| !p.is_empty()),
            ..Default::default()
        };
        let timeout_ms = node.int("timeoutSeconds").unwrap_or(600).clamp(1, 86_400) * 1000;
        let engine = self.clone();
        let st = st.clone();
        let run_id = run.id.clone();
        // The job may take minutes: the step stays running and the run is
        // advanced again when it answers.
        tokio::spawn(async move {
            let out = gw
                .job(
                    target.as_deref(),
                    &kind,
                    &payload.to_string(),
                    needs,
                    timeout_ms,
                )
                .await;
            let r = match out {
                Ok(o) if o.status == "ok" => {
                    let output: Value = serde_json::from_str(&o.output_json)
                        .unwrap_or(Value::String(o.output_json.clone()));
                    engine
                        .succeed(&st, "", json!({ "job": kind, "output": output, "nodeId": o.node_id, "sessionId": o.session_id }))
                        .await
                }
                Ok(o) => {
                    let status = if o.status.is_empty() {
                        "failed".to_string()
                    } else {
                        o.status.clone()
                    };
                    engine
                        .fail(
                            &st,
                            &format!("job_{status}"),
                            json!({ "job": kind, "error": o.error, "nodeId": o.node_id }),
                        )
                        .await
                }
                Err(e) => {
                    engine
                        .fail(
                            &st,
                            "job_error",
                            json!({ "job": kind, "error": format!("{e:#}") }),
                        )
                        .await
                }
            };
            if let Err(e) = r {
                tracing::warn!("job step: {e:#}");
            }
            if let Err(e) = engine.advance(&run_id).await {
                tracing::warn!("advance after job: {e:#}");
            }
        });
        Ok(())
    }

    /// A waiting step whose timer passed.
    async fn on_wake(&self, run: &Run, g: &Graph, st: &RunStep, _steps: &[RunStep]) -> Result<()> {
        let s = self.core().store();
        let node = g.node(&st.node_id).cloned().unwrap_or_default();
        let details: Value = serde_json::from_str(&st.details_json).unwrap_or_else(|_| json!({}));
        if matches!(
            st.error.as_str(),
            "deferred:no_host"
                | "deferred:checks"
                | "deferred:kill_switch"
                | "deferred:capacity"
                | "deferred:budget"
                | "deferred:a2a"
                | "deferred:recheck"
        ) || st.error.starts_with(READINESS_WAIT)
        {
            // Try again: back to pending, executed on this round.
            s.update_step(
                &st.id,
                &["waiting"],
                &StepUpdate {
                    status: Some("pending".into()),
                    wake_at: Some(None),
                    ..Default::default()
                },
            )
            .await?;
            return Ok(());
        }
        if let Some(a) = s
            .step_approval(&st.id)
            .await?
            .filter(|a| a.state == "pending")
        {
            if node.str("onTimeout") == "end_run" {
                s.decide_approval(&a.id, "expired", "engine", "nobody answered in time", None)
                    .await?;
                self.core()
                    .emit(
                        kinds::APPROVAL_DECIDED,
                        &a.id,
                        &json!({ "runId": run.id, "state": "expired", "decidedBy": "engine" }),
                    )
                    .await?;
                return self.fail(st, "expired", details).await;
            }
            let hours = node.int("slaHours").unwrap_or(24).clamp(1, 720);
            let sla = now_ms() + hours * 3_600_000;
            s.set_approval_sla(&a.id, Some(sla)).await?;
            s.update_step(
                &st.id,
                &["waiting"],
                &StepUpdate {
                    wake_at: Some(Some(sla)),
                    ..Default::default()
                },
            )
            .await?;
            self.core()
                .emit(
                    kinds::APPROVAL_RENOTIFIED,
                    &a.id,
                    &json!({ "runId": run.id, "slaAt": sla, "title": a.title }),
                )
                .await?;
            return Ok(());
        }
        if st.node_type == "provision" {
            return self.wake_provision(run, &node, st, details).await;
        }
        if let Some(card_id) = details.get("routeCardId").and_then(Value::as_str)
            && !details
                .get("claimed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            let timeout_at = details
                .get("timeoutAt")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let claimed = st.session_id.is_some()
                || self.core().card(card_id).await?.is_some_and(|c| {
                    kls_core::board::cards::claim(&c).is_claimed()
                        || c.status == "doing"
                        || c.status == "done"
                });
            if claimed && timeout_at > now_ms() {
                let mut d = details.clone();
                d["claimed"] = json!(true);
                s.update_step(
                    &st.id,
                    &["waiting"],
                    &StepUpdate {
                        details_json: Some(d.to_string()),
                        wake_at: Some(Some(timeout_at)),
                        ..Default::default()
                    },
                )
                .await?;
                return Ok(());
            }
            if !claimed && timeout_at > now_ms() {
                // Nobody claimed it within the claim TTL: the workers may all
                // be busy. Wait (capacity wait) and look again each TTL until
                // the step's own timeout, instead of spending the attempt.
                let ttl = kls_core::audit::config::Effective::load(self.core())
                    .await
                    .map(|e| e.claim_ttl_ms)
                    .ok()
                    .filter(|t| *t > 0)
                    .unwrap_or(3_600_000);
                let mut d = details.clone();
                let first = d.get("unclaimedSince").is_none();
                if first {
                    d["unclaimedSince"] = json!(now_ms());
                }
                s.update_step(
                    &st.id,
                    &["waiting"],
                    &StepUpdate {
                        error: Some("waiting:unclaimed".into()),
                        details_json: Some(d.to_string()),
                        wake_at: Some(Some((now_ms() + ttl).min(timeout_at))),
                        ..Default::default()
                    },
                )
                .await?;
                if first {
                    self.core()
                        .emit(
                            "route.unclaimed",
                            card_id,
                            &json!({ "runId": run.id, "stepId": st.id, "cardId": card_id, "timeoutAt": timeout_at }),
                        )
                        .await?;
                    guard::audit(
                        self.core(),
                        "card",
                        "route.unclaimed",
                        json!({ "cardId": card_id }),
                        &Origin::engine(json!({ "runId": run.id, "stepId": st.id, "workflowId": run.workflow_id })),
                        "waiting",
                        json!({ "timeoutAt": timeout_at }),
                    )
                    .await?;
                }
                return Ok(());
            }
            if !claimed {
                return self.fail(st, "card_unclaimed", details).await;
            }
        }
        match st.node_type.as_str() {
            "wait" if details.get("waitEvent").is_none() => self.succeed(st, "", details).await,
            _ => self.fail(st, "timeout", details).await,
        }
    }
}

mod policy;
#[cfg(test)]
mod tests;
