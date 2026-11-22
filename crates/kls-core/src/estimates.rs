//! Human-equivalent estimates and workforce performance.
//!
//! A port of the web factory's delivery estimate
//! (`website/src/shared/factory-delivery.ts` `estimate`,
//! `factory-delivery-settings.ts`). Agent work is priced as the role a
//! person would have done it in:
//!
//! - agent work = agent hours x role multiplier x role rate
//! - decisions = decision hours x technical-lead rate
//! - overhead = (engineer hours + decision hours) x overhead % x overhead rate
//! - factory cost = model use + people hours x people rate
//!
//! The conservative case takes the role multipliers at x0.7, half the
//! decisions and no overhead. The rates are the `delivery.*` settings (the
//! same keys as the web), edited in Settings > Estimates and audited like
//! every setting.
//!
//! Every estimate carries its inputs and the settings revision it was priced
//! with ([`Estimate::inputs`], [`Estimate::settings_revision`]). The audit
//! sweep records one `estimate.recorded` event per finished run and ended
//! session ([`record_for_event`]).
//!
//! Public APIs for other screens: [`for_session`], [`for_run`], [`for_card`],
//! [`for_range`] and [`performance`].

use crate::Core;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub mod kinds {
    pub const ESTIMATE_RECORDED: &str = "estimate.recorded";
}

/// The seven roles agent work is priced as: (id, name, work, rate USD/h,
/// engineer hours per agent hour).
pub const ROLES: [(&str, &str, &str, f64, f64); 7] = [
    (
        "sse",
        "Senior software engineer",
        "Pull requests: features, fixes, conflict resolution",
        95.0,
        2.4,
    ),
    (
        "security",
        "Security engineer",
        "Vulnerability fixes, triage, threat reviews",
        110.0,
        2.0,
    ),
    (
        "qa",
        "QA and test engineer",
        "Failing tests, test sweeps, new tests",
        70.0,
        2.2,
    ),
    (
        "devops",
        "DevOps and SRE",
        "CI fixes, dependency and container updates",
        90.0,
        2.0,
    ),
    (
        "reviewer",
        "Code reviewer",
        "Pull request reviews and verdicts",
        95.0,
        1.5,
    ),
    (
        "planner",
        "Technical lead",
        "Plans, issue breakdowns, design notes",
        120.0,
        1.6,
    ),
    (
        "writer",
        "Technical writer",
        "Docs, changelogs, runbooks",
        60.0,
        1.8,
    ),
];

/// Decision kinds: (id, name, minutes a person would take). `toolcall` is a
/// guard rail and never counts.
pub const DECISION_KINDS: [(&str, &str, f64); 7] = [
    ("toolcall", "Tool call checks", 0.0),
    ("mode", "Mode and request classification", 0.5),
    ("goal", "Goal evaluation", 3.0),
    ("plan", "Plan evaluation", 4.0),
    ("triage", "Triage and routing", 6.0),
    ("merge", "Merge readiness", 8.0),
    ("other", "Duplicate, scope and other", 2.0),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoleRate {
    pub enabled: bool,
    pub rate: f64,
    pub mult: f64,
}

/// The `delivery.*` settings, resolved (a missing or bad key reads as its
/// default).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateSettings {
    pub enabled: bool,
    pub value: bool,
    pub decisions: bool,
    pub compare: bool,
    pub multiplier: f64,
    pub rate: f64,
    pub people_rate: f64,
    pub overhead_pct: f64,
    pub overhead_rate: f64,
    pub hours_per_month: f64,
    pub manual_cycle_days: f64,
    pub manual_issue_days: f64,
    pub manual_card_days: f64,
    pub roles: BTreeMap<String, RoleRate>,
    /// Kind -> (counted, minutes).
    pub kinds: BTreeMap<String, (bool, f64)>,
}

impl Default for EstimateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            value: true,
            decisions: true,
            compare: true,
            multiplier: 2.4,
            rate: 95.0,
            people_rate: 110.0,
            overhead_pct: 20.0,
            overhead_rate: 105.0,
            hours_per_month: 150.0,
            manual_cycle_days: 4.5,
            manual_issue_days: 3.0,
            manual_card_days: 5.0,
            roles: ROLES
                .iter()
                .map(|(id, _, _, rate, mult)| {
                    (
                        id.to_string(),
                        RoleRate {
                            enabled: true,
                            rate: *rate,
                            mult: *mult,
                        },
                    )
                })
                .collect(),
            kinds: DECISION_KINDS
                .iter()
                .map(|(id, _, m)| (id.to_string(), (*id != "toolcall", *m)))
                .collect(),
        }
    }
}

pub fn role_key(id: &str, part: &str) -> String {
    format!("delivery.roles.{id}.{part}")
}

pub fn kind_key(id: &str, part: &str) -> String {
    format!("delivery.kinds.{id}.{part}")
}

impl EstimateSettings {
    /// Resolves the settings from a key lookup.
    pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        let d = Self::default();
        let b = |k: &str, dv: bool| match get(k).as_deref().map(str::trim) {
            Some("true" | "1" | "on" | "yes") => true,
            Some("false" | "0" | "off" | "no") => false,
            _ => dv,
        };
        let n = |k: &str, dv: f64| {
            get(k)
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(dv)
        };
        let roles = d
            .roles
            .iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    RoleRate {
                        enabled: b(&role_key(id, "enabled"), r.enabled),
                        rate: n(&role_key(id, "rate"), r.rate),
                        mult: n(&role_key(id, "mult"), r.mult),
                    },
                )
            })
            .collect();
        let kinds = d
            .kinds
            .iter()
            .map(|(id, (on, m))| {
                let on = *id != "toolcall" && b(&kind_key(id, "enabled"), *on);
                (id.clone(), (on, n(&kind_key(id, "minutes"), *m)))
            })
            .collect();
        Self {
            enabled: b("delivery.enabled", d.enabled),
            value: b("delivery.value", d.value),
            decisions: b("delivery.decisions", d.decisions),
            compare: b("delivery.compare", d.compare),
            multiplier: n("delivery.multiplier", d.multiplier),
            rate: n("delivery.rate", d.rate),
            people_rate: n("delivery.peopleRate", d.people_rate),
            overhead_pct: n("delivery.overheadPct", d.overhead_pct),
            overhead_rate: n("delivery.overheadRate", d.overhead_rate),
            hours_per_month: n("delivery.hoursPerMonth", d.hours_per_month).max(1.0),
            manual_cycle_days: n("delivery.manualCycleDays", d.manual_cycle_days),
            manual_issue_days: n("delivery.manualIssueDays", d.manual_issue_days),
            manual_card_days: n("delivery.manualCardDays", d.manual_card_days),
            roles,
            kinds,
        }
    }

    pub async fn load(core: &Core) -> Result<Self> {
        let all: BTreeMap<String, String> = core
            .store()
            .all_settings()
            .await?
            .into_iter()
            .filter(|(k, _)| k.starts_with("delivery."))
            .collect();
        Ok(Self::resolve(|k| all.get(k).cloned()))
    }
}

/// What an estimate is computed from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateInputs {
    /// Agent hours per role id.
    pub agent_hours: BTreeMap<String, f64>,
    /// Decision counts per kind id.
    pub decisions: BTreeMap<String, i64>,
    /// Hours people spent (observed); `None` when not recorded.
    pub people_hours: Option<f64>,
    /// Model use, USD; `None` when not recorded.
    pub model_cost_usd: Option<f64>,
    /// Where the numbers came from ("session:<id>", "run:<id>", "card:<key>").
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleLine {
    pub role: String,
    pub agent_hours: f64,
    pub counted: bool,
    pub mult: f64,
    pub rate: f64,
    pub engineer_hours: f64,
    pub value: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conservative {
    pub agent_value: f64,
    pub decision_value: f64,
    pub value: f64,
    pub return_x: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    pub roles: Vec<RoleLine>,
    pub agent_hours: f64,
    pub engineer_hours: f64,
    pub agent_value: f64,
    pub decision_hours: f64,
    pub decision_value: f64,
    pub overhead_hours: f64,
    pub overhead_value: f64,
    /// Hours a team would take, whole hours.
    pub by_hand_hours: f64,
    pub by_hand_value: f64,
    pub people_hours: Option<f64>,
    pub people_cost: Option<f64>,
    pub model_cost: Option<f64>,
    pub factory_cost: Option<f64>,
    pub return_on_model: Option<f64>,
    pub return_on_total: Option<f64>,
    pub conservative: Conservative,
    pub hours_given_back: Option<f64>,
    pub baseline_people: f64,
    pub given_back_people: Option<f64>,
    /// Provenance: what it was priced from, and with which settings.
    pub inputs: EstimateInputs,
    pub settings_revision: i64,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn ratio(value: f64, over: Option<f64>) -> Option<f64> {
    over.filter(|o| *o > 0.0).map(|o| round1(value / o))
}

/// The estimate (see the module docs for the arithmetic).
pub fn estimate(inputs: &EstimateInputs, s: &EstimateSettings) -> Estimate {
    let mut roles = Vec::new();
    let (mut agent_hours, mut eng_sum, mut agent_value) = (0.0, 0.0, 0.0);
    for (id, ..) in ROLES {
        let set = s.roles.get(id).cloned().unwrap_or(RoleRate {
            enabled: false,
            rate: s.rate,
            mult: s.multiplier,
        });
        let raw = inputs.agent_hours.get(id).copied().unwrap_or(0.0).max(0.0);
        let hours = if s.value { raw } else { 0.0 };
        let (mult, rate) = if set.enabled {
            (set.mult, set.rate)
        } else {
            (s.multiplier, s.rate)
        };
        let value = (hours * mult * rate).round();
        roles.push(RoleLine {
            role: id.into(),
            agent_hours: raw,
            counted: set.enabled,
            mult,
            rate,
            engineer_hours: round1(hours * mult),
            value,
        });
        agent_hours += raw;
        eng_sum += hours * mult;
        agent_value += value;
    }
    let planner_rate = s
        .roles
        .get("planner")
        .filter(|r| r.enabled)
        .map(|r| r.rate)
        .unwrap_or(s.rate);
    let raw_decisions: f64 = inputs
        .decisions
        .iter()
        .filter_map(|(k, n)| {
            s.kinds
                .get(k)
                .filter(|(on, _)| *on && k != "toolcall")
                .map(|(_, m)| *n as f64 * m / 60.0)
        })
        .sum();
    let decision_hours = if s.decisions {
        round1(raw_decisions)
    } else {
        0.0
    };
    let decision_value = (decision_hours * planner_rate).round();
    let overhead_hours = if s.compare {
        round1((eng_sum + decision_hours) * s.overhead_pct / 100.0)
    } else {
        0.0
    };
    let overhead_value = (overhead_hours * s.overhead_rate).round();
    let by_hand_value = agent_value + decision_value + overhead_value;
    let by_hand_hours = (eng_sum + decision_hours + overhead_hours).round();
    let model_cost = inputs.model_cost_usd;
    let people_cost = inputs.people_hours.map(|h| h * s.people_rate);
    let factory_cost = match (model_cost, people_cost) {
        (Some(m), Some(p)) => Some((m + p).round()),
        _ => None,
    };
    let cons_agent = (agent_value * 0.7).round();
    let cons_dec = (decision_value * 0.5).round();
    let given = inputs.people_hours.map(|p| by_hand_hours - p.round());
    Estimate {
        roles,
        agent_hours,
        engineer_hours: round1(eng_sum),
        agent_value,
        decision_hours,
        decision_value,
        overhead_hours,
        overhead_value,
        by_hand_hours,
        by_hand_value,
        people_hours: inputs.people_hours,
        people_cost,
        model_cost,
        factory_cost,
        return_on_model: ratio(by_hand_value, model_cost),
        return_on_total: ratio(by_hand_value, factory_cost),
        conservative: Conservative {
            agent_value: cons_agent,
            decision_value: cons_dec,
            value: cons_agent + cons_dec,
            return_x: ratio(cons_agent + cons_dec, factory_cost),
        },
        hours_given_back: given,
        baseline_people: (by_hand_hours / s.hours_per_month).floor(),
        given_back_people: given.map(|g| (g / s.hours_per_month).floor()),
        inputs: inputs.clone(),
        settings_revision: 0,
    }
}

/// The role agent work counts as, from the agent profile name, the card's
/// labels and the work's title: the weighted rule table
/// ([`crate::github::classify_weighted`], shown in Delivery > Estimate).
pub fn classify_role(agent: &str, labels: &[String], title: &str) -> &'static str {
    let role = crate::github::classify_weighted(agent, labels, title).role;
    crate::github::ROLE_TIE_ORDER
        .iter()
        .find(|r| **r == role)
        .copied()
        .unwrap_or("sse")
}

fn session_hours(s: &kls_store::sessions::Session) -> f64 {
    let end = s.ended_at.unwrap_or(s.seen_at.max(s.updated_at));
    ((end - s.started_at).max(0) as f64) / 3_600_000.0
}

async fn priced(core: &Core, inputs: EstimateInputs) -> Result<Estimate> {
    let s = EstimateSettings::load(core).await?;
    let mut e = estimate(&inputs, &s);
    e.settings_revision = core.store().settings_revision().await.unwrap_or(0);
    Ok(e)
}

fn add_session(inputs: &mut EstimateInputs, s: &kls_store::sessions::Session, labels: &[String]) {
    let role = classify_role(&s.agent, labels, &s.title);
    *inputs.agent_hours.entry(role.into()).or_default() += session_hours(s);
    let c = inputs.model_cost_usd.get_or_insert(0.0);
    *c += s.cost_usd.max(0.0);
    inputs.sources.push(format!("session:{}", s.id));
}

/// The estimate for one session.
pub async fn for_session(core: &Core, id: &str) -> Result<Estimate> {
    let s = core
        .store()
        .session(id)
        .await?
        .ok_or_else(|| anyhow!("no such session"))?;
    let labels = match core.store().card(&s.card_id).await? {
        Some(c) if !s.card_id.is_empty() => c.labels,
        _ => Vec::new(),
    };
    let mut inputs = EstimateInputs::default();
    add_session(&mut inputs, &s, &labels);
    priced(core, inputs).await
}

async fn sessions_where(
    core: &Core,
    q: kls_store::sessions::SessionQuery,
) -> Result<Vec<kls_store::sessions::Session>> {
    core.store()
        .sessions(&kls_store::sessions::SessionQuery { limit: 1000, ..q })
        .await
}

/// The estimate for one workflow run: its sessions, plus one decision per
/// approval step (plan, merge readiness or other).
pub async fn for_run(core: &Core, id: &str) -> Result<Estimate> {
    let run = core
        .store()
        .run(id)
        .await?
        .ok_or_else(|| anyhow!("no such run"))?;
    let mut inputs = EstimateInputs::default();
    for s in sessions_where(
        core,
        kls_store::sessions::SessionQuery {
            run_id: Some(run.id.clone()),
            ..Default::default()
        },
    )
    .await?
    {
        add_session(&mut inputs, &s, &[]);
    }
    for st in core.store().run_steps(&run.id).await? {
        let kind = match st.node_type.as_str() {
            "approval" | "human" => "plan",
            "gate" => "merge",
            "condition" | "decision" => "triage",
            _ => continue,
        };
        *inputs.decisions.entry(kind.into()).or_default() += 1;
    }
    inputs.sources.insert(0, format!("run:{}", run.id));
    priced(core, inputs).await
}

/// The estimate for one board card: every session that worked it.
pub async fn for_card(core: &Core, id_or_key: &str) -> Result<Estimate> {
    let card = core
        .store()
        .card(id_or_key)
        .await?
        .ok_or_else(|| anyhow!("no such card"))?;
    let mut inputs = EstimateInputs::default();
    for s in sessions_where(
        core,
        kls_store::sessions::SessionQuery {
            card_id: Some(card.id.clone()),
            ..Default::default()
        },
    )
    .await?
    {
        add_session(&mut inputs, &s, &card.labels);
    }
    inputs.sources.insert(0, format!("card:{}", card.key));
    priced(core, inputs).await
}

/// The estimate for every session that started since `since` (Unix ms).
pub async fn for_range(core: &Core, since: i64) -> Result<Estimate> {
    let mut inputs = EstimateInputs::default();
    for s in sessions_where(core, Default::default()).await? {
        if s.started_at >= since {
            add_session(&mut inputs, &s, &[]);
        }
    }
    inputs.sources = vec![format!("sessions_since:{since}")];
    priced(core, inputs).await
}

// ---- performance ---------------------------------------------------------------

/// How performance rows are grouped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    Agent,
    Node,
    Model,
    Repo,
    Workflow,
}

/// One row of the workforce performance table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfRow {
    pub key: String,
    /// Items finished (sessions ended, or runs finished for workflows).
    pub items: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub open: i64,
    /// succeeded / (succeeded + failed), 0..1; `None` with nothing settled.
    pub success_rate: Option<f64>,
    pub tokens: i64,
    pub cost_usd: f64,
    pub cost_per_item: Option<f64>,
    pub agent_hours: f64,
    /// Median start-to-end of finished items, ms.
    pub median_cycle_ms: Option<i64>,
    /// Items finished per day over the range.
    pub per_day: f64,
    /// Human-equivalent value of the agent hours (role-priced).
    pub value_usd: f64,
}

const FAIL_WORDS: [&str; 8] = [
    "fail", "error", "timeout", "crash", "killed", "lost", "budget", "refused",
];

fn failed_reason(r: &str) -> bool {
    let r = r.to_lowercase();
    FAIL_WORDS.iter().any(|w| r.contains(w))
}

fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// Throughput, success rate, cost per item, cycle time and value per
/// agent, node, model, repo (sessions) or workflow (runs) since `since`.
pub async fn performance(core: &Core, since: i64, by: GroupBy) -> Result<Vec<PerfRow>> {
    let now = kls_store::now_ms();
    let days = ((now - since).max(1) as f64 / 86_400_000.0).max(1.0);
    let settings = EstimateSettings::load(core).await?;
    let mut rows: BTreeMap<String, (PerfRow, Vec<i64>)> = BTreeMap::new();
    if by == GroupBy::Workflow {
        let runs = core
            .store()
            .runs(&kls_store::engine::RunFilter {
                from: Some(since),
                limit: 5000,
                ..Default::default()
            })
            .await?;
        for r in runs {
            let (row, cycles) = rows.entry(r.workflow_id.clone()).or_default();
            row.key = r.workflow_id.clone();
            row.tokens += r.tokens;
            match r.status.as_str() {
                "succeeded" | "completed" | "done" => row.succeeded += 1,
                "failed" | "cancelled" | "expired" => row.failed += 1,
                _ => row.open += 1,
            }
            if let (Some(a), Some(b)) = (r.started_at, r.finished_at) {
                row.items += 1;
                cycles.push(b - a);
            }
        }
    } else {
        for s in sessions_where(core, Default::default()).await? {
            if s.started_at < since {
                continue;
            }
            let key = match by {
                GroupBy::Agent => s.agent.clone(),
                GroupBy::Node => s.node_id.clone(),
                GroupBy::Model => format!("{}/{}", s.provider, s.model),
                GroupBy::Repo => s.repo.clone(),
                GroupBy::Workflow => unreachable!(),
            };
            let key = if key.trim_matches('/').is_empty() {
                "(none)".to_string()
            } else {
                key
            };
            let (row, cycles) = rows.entry(key.clone()).or_default();
            row.key = key;
            row.tokens += s.input_tokens + s.output_tokens;
            row.cost_usd += s.cost_usd.max(0.0);
            let h = session_hours(&s);
            row.agent_hours += h;
            let role = classify_role(&s.agent, &[], &s.title);
            let rr = settings.roles.get(role).cloned().unwrap_or(RoleRate {
                enabled: true,
                rate: settings.rate,
                mult: settings.multiplier,
            });
            row.value_usd += h * rr.mult * rr.rate;
            match s.ended_at {
                None => row.open += 1,
                Some(end) => {
                    row.items += 1;
                    cycles.push(end - s.started_at);
                    if failed_reason(&s.end_reason) {
                        row.failed += 1;
                    } else {
                        row.succeeded += 1;
                    }
                }
            }
        }
    }
    let mut out: Vec<PerfRow> = rows
        .into_values()
        .map(|(mut r, cycles)| {
            let settled = r.succeeded + r.failed;
            r.success_rate = (settled > 0).then(|| r.succeeded as f64 / settled as f64);
            r.cost_per_item = (r.items > 0).then(|| r.cost_usd / r.items as f64);
            r.median_cycle_ms = median(cycles);
            r.per_day = r.items as f64 / days;
            r.value_usd = r.value_usd.round();
            r
        })
        .collect();
    out.sort_by(|a, b| b.items.cmp(&a.items).then(a.key.cmp(&b.key)));
    Ok(out)
}

// ---- cockpit ---------------------------------------------------------------

/// One open workflow run and its steps' states, for "Running now".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLane {
    pub run_id: String,
    pub workflow_id: String,
    pub repo: String,
    pub status: String,
    pub started_at: Option<i64>,
    pub tokens: i64,
    /// (step title or node id, status) in order.
    pub steps: Vec<(String, String)>,
    /// Steps settled / all steps, 0..1.
    pub progress: f64,
}

/// The cockpit's range KPIs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kpis {
    pub since: i64,
    pub tokens: i64,
    pub spend_usd: f64,
    pub cards_done: i64,
    pub runs_succeeded: i64,
    pub runs_failed: i64,
    pub draft_prs: i64,
    pub conflicting_prs: i64,
    pub sessions: i64,
    /// Human-equivalent value of the range's agent work.
    pub by_hand_value: f64,
    pub lanes: Vec<RunLane>,
    /// Free worker slots on online nodes; `None` without a node server.
    pub free_slots: Option<i64>,
    /// (remaining, limit) of the tightest GitHub core quota.
    pub quota: Option<(i64, i64)>,
    /// Share of this hour's GitHub reads answered 304 (cache), 0..1.
    pub cache_hit: Option<f64>,
}

/// KPIs since `since` and the open runs.
pub async fn kpis(core: &Core, since: i64) -> Result<Kpis> {
    let s = core.store();
    let mut k = Kpis {
        since,
        ..Default::default()
    };
    for x in sessions_where(core, Default::default()).await? {
        if x.started_at >= since || x.seen_at >= since {
            k.tokens += x.input_tokens + x.output_tokens;
            k.spend_usd += x.cost_usd.max(0.0);
            k.sessions += 1;
        }
    }
    k.cards_done = s
        .cards(false)
        .await?
        .iter()
        .filter(|c| c.status == "done" && c.updated_at >= since)
        .count() as i64;
    for r in s
        .runs(&kls_store::engine::RunFilter {
            from: Some(since),
            limit: 5000,
            ..Default::default()
        })
        .await?
    {
        match r.status.as_str() {
            "succeeded" => k.runs_succeeded += 1,
            "failed" => k.runs_failed += 1,
            _ => {}
        }
    }
    let pulls = s.gh_pulls(None, Some("open")).await.unwrap_or_default();
    k.draft_prs = pulls.iter().filter(|p| p.draft).count() as i64;
    k.conflicting_prs = pulls.iter().filter(|p| p.mergeable == "conflict").count() as i64;
    k.by_hand_value = for_range(core, since).await?.by_hand_value;
    // Free worker slots: each online node's max sessions (0: host default,
    // counted as 4) minus its open dispatched sessions.
    if let Ok(gw) = core.gateway() {
        let online = gw.online().await;
        let open = sessions_where(
            core,
            kls_store::sessions::SessionQuery {
                open_only: true,
                ..Default::default()
            },
        )
        .await?;
        let mut free = 0i64;
        for n in s.nodes().await?.iter().filter(|n| online.contains(&n.id)) {
            let cap = match n.host.capabilities.max_sessions {
                0 => 4,
                c => c as i64,
            };
            let busy = open
                .iter()
                .filter(|x| x.node_id == n.id && !x.dispatch_id.is_empty())
                .count() as i64;
            free += (cap - busy).max(0);
        }
        k.free_slots = Some(free);
    }
    // GitHub quota: the lowest core remaining, and this hour's cache hits.
    let quotas = s.gh_quotas().await.unwrap_or_default();
    if let Some(q) = quotas
        .iter()
        .filter(|q| q.resource == "core" && q.limit > 0)
        .min_by_key(|q| q.remaining)
    {
        k.quota = Some((q.remaining, q.limit));
    }
    let mut routes: Vec<String> = quotas.iter().map(|q| q.route.clone()).collect();
    routes.dedup();
    let (mut calls, mut cached) = (0i64, 0i64);
    for r in routes {
        for c in s
            .gh_calls_in_hour(&r, kls_store::now_ms())
            .await
            .unwrap_or_default()
        {
            calls += c.calls;
            cached += c.not_modified;
        }
    }
    k.cache_hit = (calls > 0).then(|| cached as f64 / calls as f64);
    for r in s
        .runs_in(&["running", "waiting_approval", "queued"])
        .await?
    {
        let steps = s.run_steps(&r.id).await?;
        let settled = steps
            .iter()
            .filter(|st| !kls_store::engine::OPEN_STEP_STATUSES.contains(&st.status.as_str()))
            .count();
        k.lanes.push(RunLane {
            progress: if steps.is_empty() {
                0.0
            } else {
                settled as f64 / steps.len() as f64
            },
            steps: steps
                .iter()
                .map(|st| {
                    let name = if st.title.is_empty() {
                        st.node_id.clone()
                    } else {
                        st.title.clone()
                    };
                    (name, st.status.clone())
                })
                .collect(),
            run_id: r.id,
            workflow_id: r.workflow_id,
            repo: r.repo,
            status: r.status,
            started_at: r.started_at,
            tokens: r.tokens,
        });
    }
    Ok(k)
}

/// Records an estimate (with its inputs) for a finished run or ended
/// session: one `estimate.recorded` event, folded into the audit ledger.
pub async fn record_for_event(core: &Core, ev: &kls_store::Event) -> Result<Option<Estimate>> {
    let e = match ev.kind.as_str() {
        "run.finished" => for_run(core, &ev.subject).await?,
        "session.ended" => for_session(core, &ev.subject).await?,
        _ => return Ok(None),
    };
    core.emit(
        kinds::ESTIMATE_RECORDED,
        &ev.subject,
        &json!({
            "for": ev.kind, "subject": ev.subject, "byHandHours": e.by_hand_hours,
            "byHandValue": e.by_hand_value, "modelCost": e.model_cost,
            "inputs": e.inputs, "settingsRevision": e.settings_revision,
        }),
    )
    .await?;
    Ok(Some(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_matches_the_web_arithmetic() {
        let s = EstimateSettings::default();
        let mut i = EstimateInputs::default();
        i.agent_hours.insert("sse".into(), 10.0);
        i.decisions.insert("merge".into(), 15); // 2 h
        i.decisions.insert("toolcall".into(), 1000); // never counted
        i.people_hours = Some(2.0);
        i.model_cost_usd = Some(30.0);
        let e = estimate(&i, &s);
        assert_eq!(e.agent_value, 2280.0); // 10 x 2.4 x 95
        assert_eq!(e.engineer_hours, 24.0);
        assert_eq!(e.decision_hours, 2.0);
        assert_eq!(e.decision_value, 240.0); // planner 120
        assert_eq!(e.overhead_hours, 5.2); // (24 + 2) x 20%
        assert_eq!(e.overhead_value, 546.0); // 5.2 x 105
        assert_eq!(e.by_hand_value, 3066.0);
        assert_eq!(e.by_hand_hours, 31.0);
        assert_eq!(e.people_cost, Some(220.0));
        assert_eq!(e.factory_cost, Some(250.0));
        assert_eq!(e.return_on_total, Some(12.3));
        assert_eq!(e.conservative.value, 1596.0 + 120.0);
        assert_eq!(e.hours_given_back, Some(29.0));
        assert_eq!(e.inputs, i, "the estimate records its inputs");
    }

    #[test]
    fn settings_resolve_with_defaults_and_role_off() {
        let s = EstimateSettings::resolve(|k| match k {
            "delivery.roles.sse.rate" => Some("100".into()),
            "delivery.roles.qa.enabled" => Some("false".into()),
            "delivery.rate" => Some("bad".into()),
            "delivery.kinds.toolcall.enabled" => Some("true".into()),
            _ => None,
        });
        assert_eq!(s.roles["sse"].rate, 100.0);
        assert!(!s.roles["qa"].enabled);
        assert_eq!(s.rate, 95.0, "a bad value reads as the default");
        assert!(!s.kinds["toolcall"].0, "the guard rail never counts");
        let mut i = EstimateInputs::default();
        i.agent_hours.insert("qa".into(), 1.0);
        let e = estimate(&i, &s);
        let qa = e.roles.iter().find(|r| r.role == "qa").unwrap();
        assert!(!qa.counted);
        assert_eq!((qa.mult, qa.rate), (2.4, 95.0));
    }

    #[test]
    fn roles_from_agent_and_labels() {
        // The weighted table decides (`github::classify_weighted`).
        assert_eq!(classify_role("belai:patcher", &[], ""), "security");
        assert_eq!(classify_role("", &["docs".into()], ""), "writer");
        assert_eq!(classify_role("belai:test-fixer", &[], ""), "qa");
        assert_eq!(classify_role("", &[], "Fix the CI pipeline"), "devops");
        assert_eq!(classify_role("builder", &[], "Add login"), "sse");
        for (agent, labels, title) in [
            ("belai:reviewer", vec![], "Review"),
            ("", vec!["security".to_string()], "Bump deps"),
            ("", vec![], "Investigate the triage plan"),
            ("", vec![], ""),
        ] {
            assert_eq!(
                classify_role(agent, &labels, title),
                crate::github::classify_weighted(agent, &labels, title).role
            );
        }
    }

    #[tokio::test]
    async fn performance_groups_and_rates() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        assert!(
            performance(&core, 0, GroupBy::Agent)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            performance(&core, 0, GroupBy::Workflow)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(median(vec![5, 1, 3]), Some(3));
        assert!(failed_reason("budget_exceeded"));
        assert!(!failed_reason("completed"));
    }
}
