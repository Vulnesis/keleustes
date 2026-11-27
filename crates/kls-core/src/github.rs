//! Domain: github. The GitHub plane's event kinds and settings, and the
//! delivery analytics (outcomes, decisions, people, contributors) read from
//! the delivery facts in kls-store's `github` module. The client, the
//! poller and the conflict editor's back end are in the `kls-github` crate.

use crate::Core;
use anyhow::Result;
use kls_store::Store;
use kls_store::github::{DeliveryCommit, HumanAction, Outcome, Person};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Event kinds. The UI reloads GitHub screens on the `github.` prefix.
pub mod kinds {
    /// Which route reaches GitHub changed (local gh, a node, none).
    pub const ROUTE: &str = "github.route";
    pub const ORG_UPDATED: &str = "github.org.updated";
    pub const REPO_UPDATED: &str = "github.repo.updated";
    pub const REPO_SETTINGS: &str = "github.repo.settings";
    pub const PULL_OPENED: &str = "github.pull.opened";
    pub const PULL_UPDATED: &str = "github.pull.updated";
    pub const PULL_CLOSED: &str = "github.pull.closed";
    pub const PULL_MERGED: &str = "github.pull.merged";
    /// A person's pull request action (ready, draft, update branch, reviewers, merge).
    pub const PULL_ACTION: &str = "github.pull.action";
    pub const ISSUE_OPENED: &str = "github.issue.opened";
    pub const ISSUE_UPDATED: &str = "github.issue.updated";
    pub const ISSUE_CLOSED: &str = "github.issue.closed";
    /// A new comment on an issue or pull request (`issue_comment.created`; `/sf` commands).
    pub const ISSUE_COMMENT: &str = "github.issue.comment";
    pub const LABELS_SYNCED: &str = "github.labels.synced";
    pub const QUOTA_OBSERVED: &str = "github.quota.observed";
    /// Remaining dropped below the reserve, or a 403/429 backed the poller off.
    pub const QUOTA_LIMITED: &str = "github.quota.limited";
    pub const CONFLICT_RESOLVED: &str = "github.conflict.resolved";
    /// One poller pass finished (data says what it did).
    pub const SYNCED: &str = "github.sync";
    /// Delivery facts written for a merged pull request or a closed issue.
    pub const DELIVERY_CAPTURED: &str = "github.delivery.captured";
    /// A forge CLI was signed in (any forge; the subject is the connection id).
    pub const LOGIN: &str = "github.login";
    /// New commits on a managed repository's default branch (webhook `push`).
    pub const PUSH: &str = "github.push";
    /// A CI run finished (`workflow_run.completed`; data has `conclusion`).
    pub const WORKFLOW_RUN: &str = "github.workflow_run";
    /// A workflow run waits for an environment review (`deployment_review.requested`;
    /// subject `<repo>:<run id>:<environment>`, data `{repo, runId, environment,
    /// url, requester}`; read by `needs::deploy_reviews`).
    pub const DEPLOY_REVIEW_REQUESTED: &str = "github.deployment_review.requested";
    /// The review was approved (data adds `reviewer`).
    pub const DEPLOY_REVIEW_APPROVED: &str = "github.deployment_review.approved";
    /// The review was rejected, or the run ended without one (data `reason`).
    pub const DEPLOY_REVIEW_REJECTED: &str = "github.deployment_review.rejected";
    /// A deployment reached a new state (`deployment_status.created`; subject
    /// `<repo>:<deployment id>`, data `{repo, deploymentId, environment,
    /// state, ref, sha, url, creator}`).
    pub const DEPLOYMENT_STATUS: &str = "github.deployment.status";
    /// A release was published (`release.published`).
    pub const RELEASE: &str = "github.release";
    /// A security alert appeared (data `alert`: dependabot, code_scanning, secret_scanning).
    pub const ALERT: &str = "github.alert";
    /// A repository appeared in a managed org (`installation_repositories.added`).
    pub const REPO_ADDED: &str = "github.repo.added";
    /// An open pull request turned conflicted (input to the conflict-fix automation).
    pub const CONFLICT_DETECTED: &str = "github.conflict.detected";
    /// A review was submitted on a pull request (`pull_request_review.submitted`;
    /// data `state`: approved, changes_requested, commented; `author`).
    pub const REVIEW_SUBMITTED: &str = "github.review.submitted";
    /// A package or container image was published (`registry_package.published`;
    /// data `package`, `ecosystem`, `version`).
    pub const PACKAGE_PUBLISHED: &str = "github.package.published";
    /// A GitHub risk signal (org audit log or repository posture change; data
    /// `source`: github-risk | github-posture, `rule`, `severity`, `summary`).
    pub const RISK: &str = "github.risk";
    /// A card's fields were written to its mirrored issue (data `fields`, `attempt`).
    pub const ISSUE_PUSHED: &str = "github.issue.pushed";
    /// A card-to-issue push failed and will be retried (data `attempt`, `retryAt`, `error`).
    pub const ISSUE_PUSH_FAILED: &str = "github.issue.push_failed";
    /// A forge connection was added, changed, disabled or removed.
    pub const CONNECTION: &str = "forge.connection";
    /// A forge action was refused (kill switch, capability); data says why.
    pub const REFUSED: &str = "forge.refused";
}

/// Settings keys read by the GitHub plane.
pub mod settings {
    /// Percent of the hourly limit the poller (and other factory reads) may use. Default 10.
    pub const FACTORY_SHARE: &str = "forge.quota.factory_share";
    /// Percent of the limit kept back for people. Default 20.
    pub const RESERVE: &str = "forge.quota.reserve";
    /// Seconds between poller passes. Default 120.
    pub const POLL_INTERVAL_S: &str = "github.poll_interval_s";
    /// "on" (default) or "off".
    pub const POLL_ENABLED: &str = "github.poll";
    /// Calls per hour counted against a forge that sends no rate headers. Default 5000.
    pub const CALLS_PER_HOUR: &str = "forge.calls_per_hour";
    /// Confidence threshold drawn on the decisions histogram. Default 0.8.
    pub const MIN_CONFIDENCE: &str = "systemone.min_confidence";
    /// Issue mirroring per board project: a JSON list of
    /// `{project, repo, inbound, outbound, rules, escalateHours}`
    /// (`kls_core::board::sync::MirrorRow`).
    pub const ISSUE_MIRROR: &str = "github.issue_mirror";
    /// Poll org audit logs and repository posture ("on" default, "off").
    pub const RISK_POLL: &str = "github.risk_poll";
}

pub const DEFAULT_FACTORY_SHARE: i64 = 10;
pub const DEFAULT_RESERVE: i64 = 20;
pub const DEFAULT_POLL_INTERVAL_S: i64 = 120;

/// The eight human action sources, in display order.
pub const SOURCES: [&str; 8] = [
    "checkpoint",
    "commit",
    "comment",
    "review",
    "chat",
    "notification",
    "admin",
    "pixcall",
];

/// Time rules (minutes and seconds as in the web factory's people page).
pub mod rules {
    pub const IDLE_GAP_MS: i64 = 5 * 60_000;
    pub const REVIEW_MS_PER_100_LINES: i64 = 4 * 60_000;
    pub const REVIEW_COMMENT_MS: i64 = 90_000;
    pub const REVIEW_CAP_MS: i64 = 90 * 60_000;
    pub const CHECKPOINT_GAP_MS: i64 = 30 * 60_000;
    pub const SINGLE_ACTION_MS: i64 = 30_000;
}

const DAY_MS: i64 = 86_400_000;

/// A reporting window `[from, to)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Range {
    pub from: i64,
    pub to: i64,
    /// "7d", "30d", "90d".
    pub label: String,
}

impl Range {
    /// The last `days` days up to `now`, from UTC midnight.
    pub fn last_days(days: i64, now: i64) -> Self {
        let start = kls_store::github::day_start(now) - (days - 1) * DAY_MS;
        Self {
            from: start,
            to: now + 1,
            label: format!("{days}d"),
        }
    }

    pub fn days(&self) -> i64 {
        ((self.to - self.from + DAY_MS - 1) / DAY_MS).max(1)
    }

    /// UTC day starts covered by the range.
    pub fn day_starts(&self) -> Vec<i64> {
        let mut d = kls_store::github::day_start(self.from);
        let mut out = Vec::new();
        while d < self.to {
            out.push(d);
            d += DAY_MS;
        }
        out
    }
}

fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2
    })
}

fn units(a: &HumanAction) -> serde_json::Value {
    serde_json::from_str(&a.raw_units_json).unwrap_or(serde_json::Value::Null)
}

fn unit_i64(u: &serde_json::Value, k: &str) -> Option<i64> {
    u.get(k).and_then(|v| v.as_i64())
}

fn started(a: &HumanAction) -> bool {
    units(a).get("started").and_then(|v| v.as_bool()) == Some(true)
        || matches!(
            a.action_kind.as_str(),
            "session.started"
                | "card_filed"
                | "card.created"
                | "issue_opened"
                | "workflow_triggered"
        )
        || a.source == "pixcall"
}

/// Checkpoint wait (`decidedAt - openedAt`), when recorded.
fn checkpoint_wait(a: &HumanAction) -> Option<i64> {
    let u = units(a);
    Some(unit_i64(&u, "decidedAt")? - unit_i64(&u, "openedAt")?).filter(|w| *w >= 0)
}

/// Person time of each action by the time rules: checkpoints count their
/// wait up to 30 minutes (else the 30 s floor), reviews their lines and
/// comments, Strix calls their duration, the rest the sitting rule (gaps of
/// at most 5 minutes between one person's actions, 30 s to start a sitting).
pub fn action_times(actions: &[&HumanAction]) -> Vec<i64> {
    let mut out = vec![0; actions.len()];
    let mut sitting: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, a) in actions.iter().enumerate() {
        match a.source.as_str() {
            "checkpoint" => {
                out[i] = match checkpoint_wait(a) {
                    Some(w) if w <= rules::CHECKPOINT_GAP_MS => w.max(rules::SINGLE_ACTION_MS),
                    _ => rules::SINGLE_ACTION_MS,
                }
            }
            "review" => {
                let u = units(a);
                let lines = unit_i64(&u, "lines").unwrap_or(0);
                let comments = unit_i64(&u, "comments").unwrap_or(0);
                out[i] = (lines * rules::REVIEW_MS_PER_100_LINES / 100
                    + comments * rules::REVIEW_COMMENT_MS)
                    .clamp(rules::SINGLE_ACTION_MS, rules::REVIEW_CAP_MS);
            }
            "pixcall" => {
                let u = units(a);
                out[i] = match (unit_i64(&u, "startedAt"), unit_i64(&u, "endedAt")) {
                    (Some(s), Some(e)) if e >= s => e - s,
                    _ => unit_i64(&u, "durationMs").unwrap_or(rules::SINGLE_ACTION_MS),
                };
            }
            _ => sitting.entry(a.actor_key.as_str()).or_default().push(i),
        }
    }
    for idx in sitting.values_mut() {
        idx.sort_by_key(|&i| actions[i].occurred_at);
        let mut prev: Option<i64> = None;
        for &i in idx.iter() {
            let at = actions[i].occurred_at;
            out[i] = match prev {
                Some(p) if at - p <= rules::IDLE_GAP_MS => (at - p).max(0),
                _ => rules::SINGLE_ACTION_MS,
            };
            prev = Some(at);
        }
    }
    out
}

// ---- outcomes --------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomeRow {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub repo: String,
    pub result: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub author: String,
    pub opened_at: Option<i64>,
    pub started_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub cycle_ms: Option<i64>,
    pub first_review_ms: Option<i64>,
    pub agent_ms: Option<i64>,
    pub wait_ms: Option<i64>,
    pub wall_ms: Option<i64>,
    pub people: i64,
    pub people_actions: i64,
    pub decisions: Option<i64>,
    pub cost_micro_usd: Option<i64>,
    pub reviewers: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomesReport {
    pub range: Option<Range>,
    pub rows: Vec<OutcomeRow>,
    pub total: i64,
    pub merged_prs: i64,
    pub closed_issues: i64,
    pub other: i64,
    pub median_cycle_ms: Option<i64>,
    pub median_first_review_ms: Option<i64>,
    pub median_wall_ms: Option<i64>,
    pub no_person_needed: i64,
    /// `(day start, outcomes resolved that day)`, zero filled.
    pub by_day: Vec<(i64, i64)>,
    /// `(repo, outcomes)`, most first, top 10.
    pub by_repo: Vec<(String, i64)>,
}

fn decisions_total(o: &Outcome) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(o.decisions_json.as_deref()?).ok()?;
    Some(v.as_object()?.values().filter_map(|n| n.as_i64()).sum())
}

fn q_matches(o: &Outcome, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    [
        &o.title,
        &o.repo,
        &o.outcome_key,
        &o.trigger_ref,
        &o.pr_ref,
        &o.card_id,
    ]
    .iter()
    .any(|s| s.to_lowercase().contains(&q))
}

/// Outcomes resolved in the range, newest first, filtered by `q` (title,
/// repo, key, trigger ref, PR ref, card) and `person` (outcomes that person
/// acted on).
pub async fn outcomes(
    store: &Store,
    range: &Range,
    q: &str,
    person: Option<&str>,
) -> Result<OutcomesReport> {
    let all = store.outcomes_between(range.from, range.to).await?;
    // Actions may happen before an outcome resolves: look back 45 days.
    let actions = store
        .actions_between(range.from - 45 * DAY_MS, range.to)
        .await?;
    let mut by_outcome: HashMap<&str, Vec<&HumanAction>> = HashMap::new();
    for a in actions.iter().filter(|a| !a.is_bot) {
        if let Some(k) = &a.outcome_key {
            by_outcome.entry(k.as_str()).or_default().push(a);
        }
    }
    let q = q.trim();
    let mut rep = OutcomesReport {
        range: Some(range.clone()),
        ..Default::default()
    };
    let mut cycles = Vec::new();
    let mut reviews = Vec::new();
    let mut walls = Vec::new();
    let mut day_counts: BTreeMap<i64, i64> =
        range.day_starts().into_iter().map(|d| (d, 0)).collect();
    let mut repo_counts: HashMap<String, i64> = HashMap::new();
    for o in &all {
        if !q_matches(o, q) {
            continue;
        }
        let acts = by_outcome
            .get(o.outcome_key.as_str())
            .cloned()
            .unwrap_or_default();
        if let Some(p) = person
            && !acts.iter().any(|a| a.actor_key == p)
        {
            continue;
        }
        let people: BTreeSet<&str> = acts.iter().map(|a| a.actor_key.as_str()).collect();
        rep.total += 1;
        match (o.kind.as_str(), o.result.as_str()) {
            ("pr", "merged") => rep.merged_prs += 1,
            ("issue", "closed") => rep.closed_issues += 1,
            _ => rep.other += 1,
        }
        if people.is_empty() {
            rep.no_person_needed += 1;
        }
        cycles.extend(o.cycle_ms);
        reviews.extend(o.first_review_ms);
        walls.extend(o.wall_ms);
        if let Some(r) = o.resolved_at {
            *day_counts
                .entry(kls_store::github::day_start(r))
                .or_default() += 1;
        }
        if !o.repo.is_empty() {
            *repo_counts.entry(o.repo.clone()).or_default() += 1;
        }
        rep.rows.push(OutcomeRow {
            key: o.outcome_key.clone(),
            kind: o.kind.clone(),
            title: o.title.clone(),
            repo: o.repo.clone(),
            result: o.result.clone(),
            trigger_kind: o.trigger_kind.clone(),
            trigger_ref: o.trigger_ref.clone(),
            author: o.author.clone(),
            opened_at: o.opened_at,
            started_at: o.started_at,
            resolved_at: o.resolved_at,
            cycle_ms: o.cycle_ms,
            first_review_ms: o.first_review_ms,
            agent_ms: o.active_ms,
            wait_ms: o.wait_ms,
            wall_ms: o.wall_ms,
            people: people.len() as i64,
            people_actions: acts.len() as i64,
            decisions: decisions_total(o),
            cost_micro_usd: o.cost_micro_usd,
            reviewers: o.reviewers.clone(),
        });
    }
    rep.median_cycle_ms = median(cycles);
    rep.median_first_review_ms = median(reviews);
    rep.median_wall_ms = median(walls);
    rep.by_day = day_counts.into_iter().collect();
    let mut by_repo: Vec<(String, i64)> = repo_counts.into_iter().collect();
    by_repo.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    by_repo.truncate(10);
    rep.by_repo = by_repo;
    Ok(rep)
}

// ---- decisions -------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionDayRow {
    pub day_start: i64,
    pub total: i64,
    pub proceeded: i64,
    pub rule_handled: i64,
    pub escalated: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionModelRow {
    pub model: String,
    pub kinds: Vec<String>,
    pub total: i64,
    pub escalated: i64,
    pub escalated_pct: f64,
    pub mean_latency_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionKindRow {
    pub kind: String,
    pub total: i64,
    pub escalated: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AskedKind {
    /// "plan", "question", "merge", "card", "other".
    pub kind: String,
    pub count: i64,
    pub median_wait_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Asked {
    pub total: i64,
    pub by_kind: Vec<AskedKind>,
    pub approved: i64,
    pub sent_back: i64,
    pub answered: i64,
    pub median_wait_ms: Option<i64>,
    pub people: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionsReport {
    pub total: i64,
    pub per_day: f64,
    pub proceeded: i64,
    pub rule_handled: i64,
    pub escalated: i64,
    /// Percent of decisions that never needed a person.
    pub never_needed_person_pct: Option<f64>,
    pub days: Vec<DecisionDayRow>,
    /// Ten buckets, 0.1 wide.
    pub histogram: Vec<i64>,
    pub threshold: f64,
    pub models: Vec<DecisionModelRow>,
    pub kinds: Vec<DecisionKindRow>,
    pub asked: Asked,
}

fn asked_kind(action_kind: &str) -> &'static str {
    match action_kind {
        "plan" => "plan",
        "question" => "question",
        "merge" => "merge",
        "escalation" => "card",
        _ => "other",
    }
}

fn asked(actions: &[&HumanAction]) -> Asked {
    let cps: Vec<&&HumanAction> = actions
        .iter()
        .filter(|a| a.source == "checkpoint" && !a.is_bot)
        .collect();
    let mut a = Asked {
        total: cps.len() as i64,
        ..Default::default()
    };
    let mut waits = Vec::new();
    let mut people = BTreeSet::new();
    for k in ["plan", "question", "merge", "card", "other"] {
        let mine: Vec<&&&HumanAction> = cps
            .iter()
            .filter(|c| asked_kind(&c.action_kind) == k)
            .collect();
        a.by_kind.push(AskedKind {
            kind: k.into(),
            count: mine.len() as i64,
            median_wait_ms: median(mine.iter().filter_map(|c| checkpoint_wait(c)).collect()),
        });
    }
    for c in &cps {
        people.insert(c.actor_key.as_str());
        waits.extend(checkpoint_wait(c));
        match units(c).get("verdict").and_then(|v| v.as_str()) {
            Some("approved") => a.approved += 1,
            Some("sent_back") => a.sent_back += 1,
            Some("answered") => a.answered += 1,
            _ => {}
        }
    }
    a.median_wait_ms = median(waits);
    a.people = people.len() as i64;
    a
}

pub async fn decisions(core: &Core, range: &Range) -> Result<DecisionsReport> {
    let store = core.store();
    let rows = store.decision_days(range.from, range.to).await?;
    let actions = store.actions_between(range.from, range.to).await?;
    let threshold = store
        .setting(settings::MIN_CONFIDENCE)
        .await?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.8);
    let mut rep = DecisionsReport {
        histogram: vec![0; 10],
        threshold,
        ..Default::default()
    };
    let mut days: BTreeMap<i64, DecisionDayRow> = BTreeMap::new();
    if !rows.is_empty() {
        for d in range.day_starts() {
            days.insert(
                d,
                DecisionDayRow {
                    day_start: d,
                    ..Default::default()
                },
            );
        }
    }
    let mut models: BTreeMap<String, (DecisionModelRow, i64, i64)> = BTreeMap::new();
    let mut kinds: BTreeMap<String, DecisionKindRow> = BTreeMap::new();
    for r in &rows {
        rep.total += r.total;
        rep.proceeded += r.proceeded;
        rep.rule_handled += r.rule_handled;
        rep.escalated += r.escalated;
        let d = days.entry(r.day_start).or_insert_with(|| DecisionDayRow {
            day_start: r.day_start,
            ..Default::default()
        });
        d.total += r.total;
        d.proceeded += r.proceeded;
        d.rule_handled += r.rule_handled;
        d.escalated += r.escalated;
        for (i, n) in r.conf_buckets.iter().enumerate().take(10) {
            rep.histogram[i] += n;
        }
        let m = models.entry(r.model.clone()).or_insert_with(|| {
            (
                DecisionModelRow {
                    model: r.model.clone(),
                    ..Default::default()
                },
                0,
                0,
            )
        });
        m.0.total += r.total;
        m.0.escalated += r.escalated;
        if !m.0.kinds.contains(&r.kind) {
            m.0.kinds.push(r.kind.clone());
        }
        m.1 += r.latency_sum_ms;
        m.2 += r.latency_n;
        let k = kinds
            .entry(r.kind.clone())
            .or_insert_with(|| DecisionKindRow {
                kind: r.kind.clone(),
                ..Default::default()
            });
        k.total += r.total;
        k.escalated += r.escalated;
    }
    rep.per_day = rep.total as f64 / range.days() as f64;
    rep.never_needed_person_pct =
        (rep.total > 0).then(|| 100.0 * (rep.total - rep.escalated) as f64 / rep.total as f64);
    rep.days = days.into_values().collect();
    rep.models = models
        .into_values()
        .map(|(mut m, sum, n)| {
            m.escalated_pct = if m.total > 0 {
                100.0 * m.escalated as f64 / m.total as f64
            } else {
                0.0
            };
            m.mean_latency_ms = (n > 0).then(|| sum / n);
            m
        })
        .collect();
    rep.models
        .sort_by(|a, b| b.total.cmp(&a.total).then(a.model.cmp(&b.model)));
    rep.kinds = kinds.into_values().collect();
    let refs: Vec<&HumanAction> = actions.iter().collect();
    rep.asked = asked(&refs);
    Ok(rep)
}

// ---- people ----------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SourceRow {
    pub id: String,
    pub actions: i64,
    pub people: i64,
    pub time_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PersonRow {
    pub person_key: String,
    pub name: String,
    pub login: String,
    pub counts: BTreeMap<String, i64>,
    pub actions: i64,
    pub time_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BotRow {
    pub login: String,
    pub bot_kind: String,
    pub actions: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PeopleReport {
    pub people: i64,
    pub known_members: i64,
    pub actions: i64,
    pub actions_per_person: Option<f64>,
    pub time_ms: i64,
    pub waiting_on_person_ms: Option<i64>,
    pub sources: Vec<SourceRow>,
    pub matrix: Vec<PersonRow>,
    pub bots: Vec<BotRow>,
}

fn display_name(people: &HashMap<String, Person>, key: &str, login: &str) -> String {
    let p = people.get(key);
    p.map(|p| p.display_name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| p.map(|p| p.login.clone()).filter(|n| !n.is_empty()))
        .or_else(|| (!login.is_empty()).then(|| login.to_string()))
        .unwrap_or_else(|| key.to_string())
}

async fn people_map(store: &Store) -> Result<HashMap<String, Person>> {
    Ok(store
        .people()
        .await?
        .into_iter()
        .map(|p| (p.person_key.clone(), p))
        .collect())
}

pub async fn people(store: &Store, range: &Range) -> Result<PeopleReport> {
    let actions = store.actions_between(range.from, range.to).await?;
    let pm = people_map(store).await?;
    let humans: Vec<&HumanAction> = actions.iter().filter(|a| !a.is_bot).collect();
    let times = action_times(&humans);
    let mut rep = PeopleReport {
        actions: humans.len() as i64,
        known_members: pm
            .values()
            .filter(|p| !p.is_bot && p.is_internal == Some(true))
            .count() as i64,
        ..Default::default()
    };
    let mut matrix: BTreeMap<&str, PersonRow> = BTreeMap::new();
    let mut src: BTreeMap<&str, (i64, BTreeSet<&str>, i64)> = BTreeMap::new();
    for (a, t) in humans.iter().zip(&times) {
        let row = matrix
            .entry(a.actor_key.as_str())
            .or_insert_with(|| PersonRow {
                person_key: a.actor_key.clone(),
                name: display_name(&pm, &a.actor_key, &a.actor_login),
                login: a.actor_login.clone(),
                counts: SOURCES.iter().map(|s| (s.to_string(), 0)).collect(),
                ..Default::default()
            });
        *row.counts.entry(a.source.clone()).or_default() += 1;
        row.actions += 1;
        row.time_ms += t;
        let s = src.entry(a.source.as_str()).or_default();
        s.0 += 1;
        s.1.insert(a.actor_key.as_str());
        s.2 += t;
    }
    rep.sources = SOURCES
        .iter()
        .map(|id| {
            let s = src.get(id);
            SourceRow {
                id: id.to_string(),
                actions: s.map(|s| s.0).unwrap_or(0),
                people: s.map(|s| s.1.len() as i64).unwrap_or(0),
                time_ms: s.map(|s| s.2).unwrap_or(0),
            }
        })
        .collect();
    rep.matrix = matrix.into_values().collect();
    rep.matrix
        .sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then(a.name.cmp(&b.name)));
    rep.people = rep.matrix.len() as i64;
    rep.time_ms = times.iter().sum();
    rep.actions_per_person = (rep.people > 0).then(|| rep.actions as f64 / rep.people as f64);
    rep.waiting_on_person_ms = median(
        humans
            .iter()
            .filter(|a| a.source == "checkpoint")
            .filter_map(|a| checkpoint_wait(a))
            .collect(),
    );
    let mut bots: BTreeMap<(String, String), i64> = BTreeMap::new();
    for a in actions.iter().filter(|a| a.is_bot) {
        let login = if a.actor_login.is_empty() {
            a.actor_key.clone()
        } else {
            a.actor_login.clone()
        };
        *bots.entry((login, a.bot_kind.clone())).or_default() += 1;
    }
    rep.bots = bots
        .into_iter()
        .map(|((login, bot_kind), actions)| BotRow {
            login,
            bot_kind,
            actions,
        })
        .collect();
    rep.bots
        .sort_by(|a, b| b.actions.cmp(&a.actions).then(a.login.cmp(&b.login)));
    rep.bots.truncate(50);
    Ok(rep)
}

// ---- contributors ----------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ContributorRow {
    pub person_key: String,
    pub name: String,
    pub login: String,
    /// "internal", "external" or "" (unknown).
    pub scope: String,
    pub repos_top: Vec<String>,
    pub repos_more: i64,
    pub outcomes: i64,
    pub started: i64,
    pub decided: i64,
    pub reviewed: i64,
    pub signed_off: i64,
    /// Verified signatures over commits with a signature signal, percent.
    pub signed_rate: Option<f64>,
    pub time_ms: i64,
    pub last_active_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StepIn {
    /// "plan", "merge", "pr_review", "signed_off", "question", "opened".
    pub id: String,
    pub count: i64,
    pub outcomes: i64,
    pub pct_of_outcomes: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ContributorsReport {
    pub count_all: i64,
    pub count_internal: i64,
    pub count_external: i64,
    pub outcomes_with_person: i64,
    pub outcomes_total: i64,
    pub no_person_needed: i64,
    pub commits_signed_off: i64,
    pub commits_total: i64,
    pub decisions_answered: i64,
    pub median_answer_ms: Option<i64>,
    pub items: Vec<ContributorRow>,
    /// `(repo, outcomes, contributors)`, top 10.
    pub by_repo: Vec<(String, i64, i64)>,
    pub steps_in: Vec<StepIn>,
}

/// "contributors" scope filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    All,
    Internal,
    External,
}

fn scope_word(p: Option<&Person>) -> &'static str {
    match p.and_then(|p| p.is_internal) {
        Some(true) => "internal",
        Some(false) => "external",
        None => "",
    }
}

fn step_of(a: &HumanAction) -> Option<&'static str> {
    match a.source.as_str() {
        "checkpoint" => match a.action_kind.as_str() {
            "plan" => Some("plan"),
            "merge" => Some("merge"),
            "question" => Some("question"),
            _ => None,
        },
        "review" => Some("pr_review"),
        "commit" => Some("signed_off"),
        _ if started(a) => Some("opened"),
        _ => None,
    }
}

fn signed_rate(commits: &[&DeliveryCommit]) -> (i64, i64, Option<f64>) {
    let with: Vec<&&DeliveryCommit> = commits.iter().filter(|c| c.signed.is_some()).collect();
    let verified = with.iter().filter(|c| c.signed == Some(true)).count() as i64;
    let total = with.len() as i64;
    (
        verified,
        total,
        (total > 0).then(|| 100.0 * verified as f64 / total as f64),
    )
}

pub async fn contributors(
    store: &Store,
    range: &Range,
    scope: Scope,
    q: &str,
) -> Result<ContributorsReport> {
    let actions = store.actions_between(range.from, range.to).await?;
    let outcomes = store.outcomes_between(range.from, range.to).await?;
    let commits = store.commits_between(range.from, range.to).await?;
    let pm = people_map(store).await?;
    let humans: Vec<&HumanAction> = actions.iter().filter(|a| !a.is_bot).collect();
    let times = action_times(&humans);
    let q = q.trim().to_lowercase();

    struct Acc<'a> {
        login: String,
        outcomes: BTreeSet<&'a str>,
        started: BTreeSet<&'a str>,
        repos: HashMap<&'a str, BTreeSet<&'a str>>,
        decided: i64,
        reviewed: i64,
        signed_off: i64,
        time_ms: i64,
        last: i64,
    }
    let mut acc: BTreeMap<&str, Acc> = BTreeMap::new();
    let mut with_person: BTreeSet<&str> = BTreeSet::new();
    let mut steps: BTreeMap<&str, (i64, BTreeSet<&str>)> = BTreeMap::new();
    let mut repo_people: HashMap<&str, (BTreeSet<&str>, BTreeSet<&str>)> = HashMap::new();
    let mut answers = Vec::new();
    let mut decisions_answered = 0;
    for (a, t) in humans.iter().zip(&times) {
        let e = acc.entry(a.actor_key.as_str()).or_insert_with(|| Acc {
            login: a.actor_login.clone(),
            outcomes: BTreeSet::new(),
            started: BTreeSet::new(),
            repos: HashMap::new(),
            decided: 0,
            reviewed: 0,
            signed_off: 0,
            time_ms: 0,
            last: 0,
        });
        e.time_ms += t;
        e.last = e.last.max(a.occurred_at);
        match a.source.as_str() {
            "checkpoint" => {
                e.decided += 1;
                decisions_answered += 1;
                answers.extend(checkpoint_wait(a));
            }
            "review" => e.reviewed += 1,
            "commit" => e.signed_off += 1,
            _ => {}
        }
        if let Some(k) = a.outcome_key.as_deref() {
            e.outcomes.insert(k);
            with_person.insert(k);
            if started(a) {
                e.started.insert(k);
            }
            if let Some(r) = a.repo.as_deref() {
                e.repos.entry(r).or_default().insert(k);
                let rp = repo_people.entry(r).or_default();
                rp.0.insert(k);
                rp.1.insert(a.actor_key.as_str());
            }
        }
        if let Some(s) = step_of(a) {
            let st = steps.entry(s).or_default();
            st.0 += 1;
            if let Some(k) = a.outcome_key.as_deref() {
                st.1.insert(k);
            }
        }
    }
    let mut rep = ContributorsReport {
        outcomes_total: outcomes.len() as i64,
        commits_total: commits.len() as i64,
        commits_signed_off: commits
            .iter()
            .filter(|c| !c.is_bot && !c.author_key.is_empty())
            .count() as i64,
        decisions_answered,
        median_answer_ms: median(answers),
        ..Default::default()
    };
    let in_range: BTreeSet<&str> = outcomes.iter().map(|o| o.outcome_key.as_str()).collect();
    rep.outcomes_with_person = with_person.intersection(&in_range).count() as i64;
    rep.no_person_needed = rep.outcomes_total - rep.outcomes_with_person;
    for (key, a) in &acc {
        let p = pm.get(*key);
        let word = scope_word(p);
        rep.count_all += 1;
        match word {
            "internal" => rep.count_internal += 1,
            "external" => rep.count_external += 1,
            _ => {}
        }
        if (scope == Scope::Internal && word != "internal")
            || (scope == Scope::External && word != "external")
        {
            continue;
        }
        let name = display_name(&pm, key, &a.login);
        let mut repos: Vec<(&str, usize)> = a.repos.iter().map(|(r, s)| (*r, s.len())).collect();
        repos.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(y.0)));
        if !q.is_empty() {
            let emails = p.map(|p| p.emails.join(" ")).unwrap_or_default();
            let hay = format!(
                "{name} {} {emails} {}",
                a.login,
                repos.iter().map(|r| r.0).collect::<Vec<_>>().join(" ")
            )
            .to_lowercase();
            if !hay.contains(&q) {
                continue;
            }
        }
        let mine: Vec<&DeliveryCommit> = commits.iter().filter(|c| c.author_key == *key).collect();
        rep.items.push(ContributorRow {
            person_key: key.to_string(),
            name,
            login: a.login.clone(),
            scope: word.into(),
            repos_top: repos.iter().take(3).map(|r| r.0.to_string()).collect(),
            repos_more: repos.len().saturating_sub(3) as i64,
            outcomes: a.outcomes.len() as i64,
            started: a.started.len() as i64,
            decided: a.decided,
            reviewed: a.reviewed,
            signed_off: a.signed_off,
            signed_rate: signed_rate(&mine).2,
            time_ms: a.time_ms,
            last_active_at: a.last,
        });
    }
    rep.items
        .sort_by(|a, b| b.outcomes.cmp(&a.outcomes).then(a.name.cmp(&b.name)));
    rep.items.truncate(500);
    let mut by_repo: Vec<(String, i64, i64)> = repo_people
        .into_iter()
        .map(|(r, (o, p))| (r.to_string(), o.len() as i64, p.len() as i64))
        .collect();
    by_repo.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    by_repo.truncate(10);
    rep.by_repo = by_repo;
    rep.steps_in = [
        "plan",
        "merge",
        "pr_review",
        "signed_off",
        "question",
        "opened",
    ]
    .iter()
    .map(|id| {
        let s = steps.get(id);
        let outs = s.map(|s| s.1.len() as i64).unwrap_or(0);
        StepIn {
            id: id.to_string(),
            count: s.map(|s| s.0).unwrap_or(0),
            outcomes: outs,
            pct_of_outcomes: (rep.outcomes_with_person > 0)
                .then(|| 100.0 * outs as f64 / rep.outcomes_with_person as f64),
        }
    })
    .collect();
    Ok(rep)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Identity {
    /// "github", "email", "email_alias".
    pub kind: String,
    pub value: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorkRow {
    pub outcome_key: String,
    pub title: String,
    pub resolved_at: Option<i64>,
    /// "started", "decided", "reviewed", "signed".
    pub parts: Vec<String>,
    pub did: BTreeMap<String, i64>,
    pub time_ms: i64,
    pub others: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ContributorDetail {
    pub person: Person,
    pub name: String,
    pub scope: String,
    pub identities: Vec<Identity>,
    pub outcomes_touched: i64,
    pub outcomes_total: i64,
    pub started: i64,
    pub decisions_answered: i64,
    pub median_answer_ms: Option<i64>,
    pub commits_signed_off: i64,
    pub signed_verified: i64,
    pub signed_total: i64,
    pub signed_rate: Option<f64>,
    pub time_ms: i64,
    pub actions: i64,
    pub agent_ms_beside: Option<i64>,
    pub work: Vec<WorkRow>,
    pub work_total: i64,
    /// `(source, actions, time)` for all eight sources, most first.
    pub parts: Vec<(String, i64, i64)>,
    /// `(repo, outcomes, commits, last seen)`.
    pub repos: Vec<(String, i64, i64, i64)>,
    /// `(day start, actions)`, zero filled.
    pub days: Vec<(i64, i64)>,
}

/// One person; `None` for an unknown key or a bot.
pub async fn contributor(
    store: &Store,
    key: &str,
    range: &Range,
) -> Result<Option<ContributorDetail>> {
    let Some(person) = store.person(key).await? else {
        return Ok(None);
    };
    if person.is_bot {
        return Ok(None);
    }
    let actions = store.actions_between(range.from, range.to).await?;
    let outcomes = store.outcomes_between(range.from, range.to).await?;
    let commits = store.commits_between(range.from, range.to).await?;
    let pm = people_map(store).await?;
    let humans: Vec<&HumanAction> = actions.iter().filter(|a| !a.is_bot).collect();
    let times = action_times(&humans);
    let mine: Vec<(&HumanAction, i64)> = humans
        .iter()
        .zip(&times)
        .filter(|(a, _)| a.actor_key == key)
        .map(|(a, t)| (*a, *t))
        .collect();
    let by_key: HashMap<&str, &Outcome> = outcomes
        .iter()
        .map(|o| (o.outcome_key.as_str(), o))
        .collect();
    let mut d = ContributorDetail {
        name: display_name(&pm, key, &person.login),
        scope: scope_word(Some(&person)).into(),
        outcomes_total: outcomes.len() as i64,
        actions: mine.len() as i64,
        ..Default::default()
    };
    if !person.login.is_empty() {
        d.identities.push(Identity {
            kind: "github".into(),
            value: person.login.clone(),
            note: "commits, comments, reviews".into(),
        });
    }
    for (i, e) in person.emails.iter().enumerate() {
        let alias = i > 0 || e.ends_with("@users.noreply.github.com");
        d.identities.push(Identity {
            kind: if alias { "email_alias" } else { "email" }.into(),
            value: e.clone(),
            note: if alias { "commits" } else { "Signed-off-by" }.into(),
        });
    }
    let mut work: BTreeMap<&str, WorkRow> = BTreeMap::new();
    let mut parts: BTreeMap<&str, (i64, i64)> = SOURCES.iter().map(|s| (*s, (0, 0))).collect();
    let mut repos: BTreeMap<&str, (BTreeSet<&str>, i64, i64)> = BTreeMap::new();
    let mut days: BTreeMap<i64, i64> = range.day_starts().into_iter().map(|d| (d, 0)).collect();
    let mut answers = Vec::new();
    let mut started_set = BTreeSet::new();
    for (a, t) in &mine {
        d.time_ms += t;
        let p = parts.entry(a.source.as_str()).or_default();
        p.0 += 1;
        p.1 += t;
        *days
            .entry(kls_store::github::day_start(a.occurred_at))
            .or_default() += 1;
        match a.source.as_str() {
            "checkpoint" => {
                d.decisions_answered += 1;
                answers.extend(checkpoint_wait(a));
            }
            "commit" => d.commits_signed_off += 1,
            _ => {}
        }
        if let Some(r) = a.repo.as_deref() {
            let e = repos.entry(r).or_default();
            if let Some(k) = a.outcome_key.as_deref() {
                e.0.insert(k);
            }
            if a.source == "commit" {
                e.1 += 1;
            }
            e.2 = e.2.max(a.occurred_at);
        }
        let Some(k) = a.outcome_key.as_deref() else {
            continue;
        };
        if started(a) {
            started_set.insert(k);
        }
        let w = work.entry(k).or_insert_with(|| WorkRow {
            outcome_key: k.to_string(),
            title: by_key.get(k).map(|o| o.title.clone()).unwrap_or_default(),
            resolved_at: by_key.get(k).and_then(|o| o.resolved_at),
            did: SOURCES.iter().map(|s| (s.to_string(), 0)).collect(),
            ..Default::default()
        });
        *w.did.entry(a.source.clone()).or_default() += 1;
        w.time_ms += t;
        let part = match a.source.as_str() {
            _ if started(a) => Some("started"),
            "checkpoint" => Some("decided"),
            "review" => Some("reviewed"),
            "commit" => Some("signed"),
            _ => None,
        };
        if let Some(p) = part
            && !w.parts.iter().any(|x| x == p)
        {
            w.parts.push(p.into());
        }
    }
    for w in work.values_mut() {
        let others: BTreeSet<&str> = humans
            .iter()
            .filter(|a| a.outcome_key.as_deref() == Some(&w.outcome_key) && a.actor_key != key)
            .map(|a| a.actor_key.as_str())
            .collect();
        w.others = others
            .into_iter()
            .take(4)
            .map(|k| display_name(&pm, k, ""))
            .collect();
    }
    d.outcomes_touched = work.keys().filter(|k| by_key.contains_key(*k)).count() as i64;
    d.started = started_set.len() as i64;
    d.median_answer_ms = median(answers);
    let beside: Vec<i64> = work
        .keys()
        .filter_map(|k| by_key.get(k).and_then(|o| o.active_ms))
        .collect();
    d.agent_ms_beside = (!beside.is_empty()).then(|| beside.iter().sum());
    let mut wl: Vec<WorkRow> = work.into_values().collect();
    wl.sort_by(|a, b| {
        b.resolved_at
            .cmp(&a.resolved_at)
            .then(a.outcome_key.cmp(&b.outcome_key))
    });
    d.work_total = wl.len() as i64;
    wl.truncate(10);
    d.work = wl;
    let mut pl: Vec<(String, i64, i64)> = parts
        .into_iter()
        .map(|(s, (n, t))| (s.to_string(), n, t))
        .collect();
    pl.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    d.parts = pl;
    d.repos = repos
        .into_iter()
        .map(|(r, (o, c, last))| (r.to_string(), o.len() as i64, c, last))
        .collect();
    d.repos.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    d.days = days.into_iter().collect();
    let authored: Vec<&DeliveryCommit> = commits.iter().filter(|c| c.author_key == key).collect();
    let (v, t, rate) = signed_rate(&authored);
    d.signed_verified = v;
    d.signed_total = t;
    d.signed_rate = rate;
    d.person = person;
    Ok(Some(d))
}

// ---- CSV -------------------------------------------------------------------

/// One CSV cell: RFC 4180 quoting, and a leading `'` on text that a
/// spreadsheet would run as a formula.
pub fn csv_cell(s: &str, text: bool) -> String {
    let mut v = s.to_string();
    if text {
        let first = v.trim_start_matches([' ', '\t']).chars().next();
        if matches!(first, Some('=' | '+' | '-' | '@' | '\t' | '\r' | '\n'))
            || v.starts_with(['\t', '\r'])
        {
            v.insert(0, '\'');
        }
    }
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

/// A cell of a CSV row: text (sanitised) or a number the server wrote.
pub enum Cell {
    Text(String),
    Num(String),
}

impl Cell {
    pub fn opt<T: ToString>(v: Option<T>) -> Cell {
        Cell::Num(v.map(|v| v.to_string()).unwrap_or_default())
    }
}

pub fn to_csv(headers: &[&str], rows: &[Vec<Cell>]) -> String {
    let mut out = headers.join(",");
    out.push_str("\r\n");
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .map(|c| match c {
                Cell::Text(s) => csv_cell(s, true),
                Cell::Num(s) => csv_cell(s, false),
            })
            .collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

/// `2026-11-21T08:30:00Z` for a Unix-ms time.
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The outcomes ledger as CSV (columns of the web factory's export, plus
/// the GitHub cycle times).
pub fn outcomes_csv(rows: &[OutcomeRow]) -> String {
    let headers = [
        "key",
        "kind",
        "title",
        "repo",
        "result",
        "trigger_kind",
        "trigger_ref",
        "started_at",
        "resolved_at",
        "agent_ms",
        "wall_ms",
        "wait_ms",
        "people",
        "people_actions",
        "decisions",
        "cost_usd",
        "by_hand_hours",
        "value_usd",
        "cycle_ms",
        "first_review_ms",
        "reviewers",
    ];
    let rows: Vec<Vec<Cell>> = rows
        .iter()
        .take(10_000)
        .map(|r| {
            vec![
                Cell::Text(r.key.clone()),
                Cell::Text(r.kind.clone()),
                Cell::Text(r.title.clone()),
                Cell::Text(r.repo.clone()),
                Cell::Text(r.result.clone()),
                Cell::Text(r.trigger_kind.clone()),
                Cell::Text(r.trigger_ref.clone()),
                Cell::opt(r.started_at.map(rfc3339)),
                Cell::opt(r.resolved_at.map(rfc3339)),
                Cell::opt(r.agent_ms),
                Cell::opt(r.wall_ms),
                Cell::opt(r.wait_ms),
                Cell::Num(r.people.to_string()),
                Cell::Num(r.people_actions.to_string()),
                Cell::opt(r.decisions),
                Cell::opt(r.cost_micro_usd.map(|c| format!("{:.6}", c as f64 / 1e6))),
                Cell::Num(String::new()),
                Cell::Num(String::new()),
                Cell::opt(r.cycle_ms),
                Cell::opt(r.first_review_ms),
                Cell::Text(r.reviewers.join(" ")),
            ]
        })
        .collect();
    to_csv(&headers, &rows)
}

pub fn decisions_csv(r: &DecisionsReport) -> String {
    let rows: Vec<Vec<Cell>> = r
        .days
        .iter()
        .map(|d| {
            vec![
                Cell::Num(rfc3339(d.day_start)),
                Cell::Num(d.total.to_string()),
                Cell::Num(d.proceeded.to_string()),
                Cell::Num(d.rule_handled.to_string()),
                Cell::Num(d.escalated.to_string()),
            ]
        })
        .collect();
    to_csv(
        &["day", "total", "proceeded", "rule_handled", "escalated"],
        &rows,
    )
}

pub fn people_csv(r: &PeopleReport) -> String {
    let mut headers = vec!["person_key", "name", "login"];
    headers.extend(SOURCES);
    headers.extend(["actions", "time_ms"]);
    let rows: Vec<Vec<Cell>> = r
        .matrix
        .iter()
        .map(|p| {
            let mut row = vec![
                Cell::Text(p.person_key.clone()),
                Cell::Text(p.name.clone()),
                Cell::Text(p.login.clone()),
            ];
            for s in SOURCES {
                row.push(Cell::Num(p.counts.get(s).copied().unwrap_or(0).to_string()));
            }
            row.push(Cell::Num(p.actions.to_string()));
            row.push(Cell::Num(p.time_ms.to_string()));
            row
        })
        .collect();
    to_csv(&headers, &rows)
}

pub fn contributors_csv(r: &ContributorsReport) -> String {
    let rows: Vec<Vec<Cell>> = r
        .items
        .iter()
        .map(|c| {
            vec![
                Cell::Text(c.person_key.clone()),
                Cell::Text(c.name.clone()),
                Cell::Text(c.login.clone()),
                Cell::Text(c.scope.clone()),
                Cell::Text(c.repos_top.join(" ")),
                Cell::Num(c.outcomes.to_string()),
                Cell::Num(c.started.to_string()),
                Cell::Num(c.decided.to_string()),
                Cell::Num(c.reviewed.to_string()),
                Cell::Num(c.signed_off.to_string()),
                Cell::opt(c.signed_rate.map(|v| format!("{v:.1}"))),
                Cell::Num(c.time_ms.to_string()),
                Cell::Num(rfc3339(c.last_active_at)),
            ]
        })
        .collect();
    to_csv(
        &[
            "person_key",
            "name",
            "login",
            "scope",
            "repos",
            "outcomes",
            "started",
            "decided",
            "reviewed",
            "signed_off",
            "signed_rate_pct",
            "time_ms",
            "last_active_at",
        ],
        &rows,
    )
}

// ---- ranges: today and month to date -----------------------------------------

/// Days since 1970-01-01 -> (year, month 1..12, day 1..31), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Range {
    /// Today (UTC) up to `now`.
    pub fn today(now: i64) -> Self {
        Self {
            from: kls_store::github::day_start(now),
            to: now + 1,
            label: "today".into(),
        }
    }

    /// The calendar month (UTC) up to `now`.
    pub fn month_to_date(now: i64) -> Self {
        let day = kls_store::github::day_start(now);
        let (_, _, d) = civil_from_days(day.div_euclid(DAY_MS));
        Self {
            from: day - (d - 1) * DAY_MS,
            to: now + 1,
            label: "mtd".into(),
        }
    }
}

// ---- one outcome -----------------------------------------------------------------

/// One line of an outcome's timeline.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TimelineItem {
    pub at: i64,
    /// "opened", "started", "first_review", "resolved", an action kind, "session".
    pub what: String,
    pub who: String,
    /// "outcome", an action source, "session".
    pub source: String,
    pub note: String,
}

/// Model use behind an outcome (its sessions), per model.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ModelCost {
    pub model: String,
    pub provider: String,
    pub sessions: i64,
    pub tokens: i64,
    pub cost_usd: f64,
    pub agent_ms: i64,
}

/// A person who acted on an outcome.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomePerson {
    pub key: String,
    pub name: String,
    pub actions: i64,
    pub time_ms: i64,
    /// Sources the person acted through.
    pub parts: Vec<String>,
}

/// What an outcome links to: card, pull request, issue, sessions, runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomeLinks {
    pub card_id: String,
    pub card_key: String,
    pub card_title: String,
    /// `repo#n`.
    pub pr_ref: String,
    pub pr_url: String,
    pub issues: Vec<String>,
    pub sessions: Vec<String>,
    pub runs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomeDetail {
    pub outcome: Outcome,
    pub timeline: Vec<TimelineItem>,
    pub cost_by_model: Vec<ModelCost>,
    pub people: Vec<OutcomePerson>,
    pub links: OutcomeLinks,
    pub agent_ms: i64,
    pub cost_usd: f64,
}

/// `https://host/acme/api/pull/7` or `…/-/merge_requests/7` -> 7.
pub fn pull_number(url: &str) -> Option<i64> {
    let tail = url
        .rsplit_once("/pull/")
        .or_else(|| url.rsplit_once("/merge_requests/"))
        .or_else(|| url.rsplit_once("/pulls/"))?
        .1;
    tail.split(['/', '?', '#']).next()?.parse().ok()
}

/// `acme/api#7` -> ("acme/api", 7).
fn split_ref(r: &str) -> Option<(&str, i64)> {
    let (repo, n) = r.rsplit_once('#')?;
    Some((repo, n.parse().ok()?))
}

/// The card an outcome belongs to: its `card_id`, or the card whose pull
/// request is the outcome's.
fn card_for<'a>(
    o: &Outcome,
    cards: &'a [kls_store::board::Card],
) -> Option<&'a kls_store::board::Card> {
    if !o.card_id.is_empty() {
        return cards
            .iter()
            .find(|c| c.id == o.card_id || c.key == o.card_id);
    }
    let (repo, n) = split_ref(&o.pr_ref)?;
    cards.iter().find(|c| {
        !c.deleted && c.repo.eq_ignore_ascii_case(repo) && pull_number(&c.pr_url) == Some(n)
    })
}

fn session_ms(s: &kls_store::sessions::Session) -> i64 {
    let end = s.ended_at.unwrap_or(s.seen_at.max(s.updated_at));
    (end - s.started_at).max(0)
}

async fn card_sessions(core: &Core, card_id: &str) -> Result<Vec<kls_store::sessions::Session>> {
    if card_id.is_empty() {
        return Ok(Vec::new());
    }
    core.store()
        .sessions(&kls_store::sessions::SessionQuery {
            card_id: Some(card_id.to_string()),
            limit: 1000,
            ..Default::default()
        })
        .await
}

/// One outcome in detail: its timeline (outcome times, people's and bots'
/// actions, sessions), model cost per model, the people who acted and what
/// it links to. `None` when the key is unknown.
pub async fn outcome(core: &Core, key: &str) -> Result<Option<OutcomeDetail>> {
    let store = core.store();
    let Some(o) = store.outcome(key).await? else {
        return Ok(None);
    };
    let now = kls_store::now_ms();
    let end = o.resolved_at.unwrap_or(now) + 1;
    let start = o.opened_at.or(o.started_at).unwrap_or(end) - 45 * DAY_MS;
    let actions: Vec<HumanAction> = store
        .actions_between(start, end + DAY_MS)
        .await?
        .into_iter()
        .filter(|a| a.outcome_key.as_deref() == Some(key))
        .collect();
    let people_map = people_map(store).await?;
    let mut tl = Vec::new();
    let mut at = |at: Option<i64>, what: &str, note: String| {
        if let Some(at) = at {
            tl.push(TimelineItem {
                at,
                what: what.into(),
                source: "outcome".into(),
                note,
                ..Default::default()
            });
        }
    };
    at(o.opened_at, "opened", o.author.clone());
    at(o.started_at, "started", String::new());
    at(
        o.opened_at.zip(o.first_review_ms).map(|(a, b)| a + b),
        "first_review",
        String::new(),
    );
    at(o.resolved_at, "resolved", o.result.clone());
    for a in &actions {
        tl.push(TimelineItem {
            at: a.occurred_at,
            what: a.action_kind.clone(),
            who: display_name(&people_map, &a.actor_key, &a.actor_login),
            source: a.source.clone(),
            note: if a.is_bot {
                "bot".into()
            } else {
                String::new()
            },
        });
    }
    let human: Vec<&HumanAction> = actions.iter().filter(|a| !a.is_bot).collect();
    let times = action_times(&human);
    let mut by_person: BTreeMap<String, OutcomePerson> = BTreeMap::new();
    for (a, t) in human.iter().zip(times) {
        let p = by_person
            .entry(a.actor_key.clone())
            .or_insert_with(|| OutcomePerson {
                key: a.actor_key.clone(),
                name: display_name(&people_map, &a.actor_key, &a.actor_login),
                ..Default::default()
            });
        p.actions += 1;
        p.time_ms += t;
        if !p.parts.contains(&a.source) {
            p.parts.push(a.source.clone());
        }
    }
    let mut people: Vec<OutcomePerson> = by_person.into_values().collect();
    people.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then(a.key.cmp(&b.key)));

    let cards = core.cards().await?;
    let mut links = OutcomeLinks {
        pr_ref: o.pr_ref.clone(),
        ..Default::default()
    };
    if let Some(c) = card_for(&o, &cards) {
        links.card_id = c.id.clone();
        links.card_key = c.key.clone();
        links.card_title = c.title.clone();
        if links.pr_ref.is_empty()
            && let Some(n) = pull_number(&c.pr_url)
        {
            links.pr_ref = format!("{}#{n}", c.repo.to_lowercase());
        }
        links.pr_url = c.pr_url.clone();
    }
    if let Some((repo, n)) = split_ref(&links.pr_ref)
        && links.pr_url.is_empty()
    {
        links.pr_url = store
            .gh_pulls(None, None)
            .await?
            .into_iter()
            .find(|p| p.repo.eq_ignore_ascii_case(repo) && p.number == n)
            .map(|p| p.html_url)
            .unwrap_or_default();
    }
    if o.kind == "issue" {
        links
            .issues
            .push(key.trim_start_matches("issue:").to_string());
    }
    if !links.card_id.is_empty() {
        for i in store.gh_issues(None, None).await? {
            let r = format!("{}#{}", i.repo.to_lowercase(), i.number);
            if i.card_id.as_deref() == Some(links.card_id.as_str()) && !links.issues.contains(&r) {
                links.issues.push(r);
            }
        }
    }
    let sessions = card_sessions(core, &links.card_id).await?;
    let mut models: BTreeMap<(String, String), ModelCost> = BTreeMap::new();
    let (mut agent_ms, mut cost) = (0, 0.0);
    for s in &sessions {
        let ms = session_ms(s);
        agent_ms += ms;
        cost += s.cost_usd.max(0.0);
        let m = models
            .entry((s.model.clone(), s.provider.clone()))
            .or_insert_with(|| ModelCost {
                model: if s.model.is_empty() {
                    "unknown".into()
                } else {
                    s.model.clone()
                },
                provider: s.provider.clone(),
                ..Default::default()
            });
        m.sessions += 1;
        m.tokens += s.input_tokens + s.output_tokens;
        m.cost_usd += s.cost_usd.max(0.0);
        m.agent_ms += ms;
        links.sessions.push(s.id.clone());
        if !s.run_id.is_empty() && !links.runs.contains(&s.run_id) {
            links.runs.push(s.run_id.clone());
        }
        tl.push(TimelineItem {
            at: s.started_at,
            what: "session".into(),
            who: if s.agent.is_empty() {
                s.model.clone()
            } else {
                s.agent.clone()
            },
            source: "session".into(),
            note: s.title.clone(),
        });
    }
    tl.sort_by(|a, b| a.at.cmp(&b.at).then(a.what.cmp(&b.what)));
    let mut cost_by_model: Vec<ModelCost> = models.into_values().collect();
    cost_by_model.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then(a.model.cmp(&b.model))
    });
    Ok(Some(OutcomeDetail {
        outcome: o,
        timeline: tl,
        cost_by_model,
        people,
        links,
        agent_ms,
        cost_usd: cost,
    }))
}

// ---- sweeps: outcome linking, rollup, lifecycle ledger ----------------------------

/// What a delivery sweep did; `refused` is set (and nothing written) when the
/// kill switch held it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SweepReport {
    pub linked: i64,
    pub rolled_up: i64,
    /// Merged pulls whose delivery facts were never captured (missed hooks).
    pub missed: i64,
    /// Missed captures read again through the forge this pass.
    pub recaptured: i64,
    pub entered: i64,
    pub exited: i64,
    pub refused: Option<String>,
}

/// Ledger entry of a delivery sweep write (provenance: `origin` is the
/// person, `owner`, or the timer, `sweep`).
async fn sweep_audit(
    core: &Core,
    action: &str,
    target: &str,
    origin: &str,
    payload: serde_json::Value,
) {
    let (actor, kind) = if origin == "owner" {
        ("owner".to_string(), "user")
    } else {
        (format!("delivery {origin}"), "system")
    };
    let d = kls_store::audit::LedgerDraft {
        kind: "delivery".into(),
        actor,
        actor_kind: kind.into(),
        msg: format!("delivery {action} {target}"),
        subject: serde_json::json!({ "action": action, "target": target }),
        payload,
        origin: "factory".into(),
        ..Default::default()
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(action, target, "delivery audit: {e:#}");
    }
}

async fn sweep_gate(core: &Core, what: &str, origin: &str) -> Option<String> {
    match crate::control::check(core, "schedule", what, origin).await {
        Ok(()) => None,
        Err(e) => Some(format!("{e:#}")),
    }
}

/// Outcome linking (session ↔ card ↔ PR ↔ issue): an outcome with no card
/// takes the card whose pull request it is; an outcome with a card and no
/// pull request takes the card's. Each link is audited.
pub async fn link_outcomes(core: &Core, origin: &str) -> Result<SweepReport> {
    let mut rep = SweepReport::default();
    if let Some(why) = sweep_gate(core, "delivery.link", origin).await {
        rep.refused = Some(why);
        return Ok(rep);
    }
    let store = core.store();
    let cards = core.cards().await?;
    for mut o in store.outcomes_between(0, i64::MAX).await? {
        let before = (o.card_id.clone(), o.pr_ref.clone());
        if let Some(c) = card_for(&o, &cards) {
            o.card_id = c.id.clone();
            if o.pr_ref.is_empty()
                && let Some(n) = pull_number(&c.pr_url)
            {
                o.pr_ref = format!("{}#{n}", c.repo.to_lowercase());
            }
        }
        if (o.card_id.clone(), o.pr_ref.clone()) == before {
            continue;
        }
        store.upsert_outcome(&o).await?;
        rep.linked += 1;
        sweep_audit(
            core,
            "outcome.link",
            &o.outcome_key,
            origin,
            serde_json::json!({ "cardId": o.card_id, "prRef": o.pr_ref,
                                "was": { "cardId": before.0, "prRef": before.1 } }),
        )
        .await;
    }
    Ok(rep)
}

/// The rollup sweep: links outcomes, fills agent time, model cost and the
/// start of each linked outcome from its card's sessions (fields capture left
/// empty only), and counts merged pulls whose capture was missed. One
/// summary entry is audited and `delivery.rollup` emitted.
pub async fn rollup(core: &Core, origin: &str) -> Result<SweepReport> {
    let mut rep = link_outcomes(core, origin).await?;
    if rep.refused.is_some() {
        return Ok(rep);
    }
    let store = core.store();
    let mut keys = Vec::new();
    for mut o in store.outcomes_between(0, i64::MAX).await? {
        if o.card_id.is_empty() || (o.active_ms.is_some() && o.cost_micro_usd.is_some()) {
            continue;
        }
        let sessions = card_sessions(core, &o.card_id).await?;
        if sessions.is_empty() {
            continue;
        }
        let ms: i64 = sessions.iter().map(session_ms).sum();
        let usd: f64 = sessions.iter().map(|s| s.cost_usd.max(0.0)).sum();
        o.active_ms = o.active_ms.or(Some(ms));
        o.cost_micro_usd = o.cost_micro_usd.or(Some((usd * 1e6).round() as i64));
        o.started_at = o
            .started_at
            .or_else(|| sessions.iter().map(|s| s.started_at).min());
        store.upsert_outcome(&o).await?;
        rep.rolled_up += 1;
        keys.push(o.outcome_key);
    }
    let missed = |pulls: &[kls_store::github::GhPull]| {
        pulls
            .iter()
            .filter(|p| p.merged && !p.delivery_captured)
            .map(|p| (p.repo.clone(), p.number))
            .collect::<Vec<_>>()
    };
    let mut gone = missed(&store.gh_pulls(None, None).await?);
    // Missed captures are read again through the forge (kls-github hook),
    // a few per pass, then counted again.
    if !gone.is_empty() {
        gone.truncate(RECAPTURE_PER_PASS);
        for hook in recapturers(core) {
            match hook.recapture(core, &gone, origin).await {
                Ok(n) => rep.recaptured += n as i64,
                Err(e) => tracing::warn!("delivery re-capture: {e:#}"),
            }
        }
        gone = missed(&store.gh_pulls(None, None).await?);
    }
    rep.missed = gone.len() as i64;
    keys.truncate(50);
    sweep_audit(
        core,
        "rollup",
        "delivery",
        origin,
        serde_json::json!({ "linked": rep.linked, "rolledUp": rep.rolled_up,
                            "missed": rep.missed, "recaptured": rep.recaptured,
                            "keys": keys }),
    )
    .await;
    core.emit(
        "delivery.rollup",
        "delivery",
        &serde_json::json!({ "linked": rep.linked, "rolledUp": rep.rolled_up,
                             "missed": rep.missed, "recaptured": rep.recaptured,
                             "origin": origin }),
    )
    .await?;
    Ok(rep)
}

/// Merged pulls re-captured per roll-up pass at most.
pub const RECAPTURE_PER_PASS: usize = 10;

/// Reads a merged pull again and captures its delivery facts: kls-core
/// cannot call the forge client (kls-github depends on kls-core), so
/// kls-github registers this at start ([`register_recapture`]). The hook
/// checks the kill switch and audits each pull.
#[async_trait::async_trait]
pub trait Recapture: Send + Sync {
    /// Re-captures `pulls` (`(repo, number)`); how many were captured.
    async fn recapture(&self, core: &Core, pulls: &[(String, i64)], origin: &str) -> Result<usize>;
}

type RecaptureRegistry = std::sync::Mutex<Vec<(std::path::PathBuf, std::sync::Arc<dyn Recapture>)>>;

fn recapture_registry() -> &'static RecaptureRegistry {
    static R: std::sync::OnceLock<RecaptureRegistry> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// Installs the re-capture hook for this core's data directory (once).
pub fn register_recapture(core: &Core, hook: std::sync::Arc<dyn Recapture>) {
    let mut r = recapture_registry().lock().unwrap();
    r.retain(|(d, _)| d != core.data_dir());
    r.push((core.data_dir().to_path_buf(), hook));
}

fn recapturers(core: &Core) -> Vec<std::sync::Arc<dyn Recapture>> {
    recapture_registry()
        .lock()
        .unwrap()
        .iter()
        .filter(|(d, _)| d == core.data_dir())
        .map(|(_, h)| h.clone())
        .collect()
}

/// One episode of the lifecycle ledger: a subject in a stage.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Episode {
    /// `card.<list>`, `pr.open`, `issue.open`.
    pub stage: String,
    /// `card:<id>`, `pr:<repo>#<n>`, `issue:<repo>#<n>`.
    pub subject: String,
    pub repo: String,
    pub title: String,
    pub entered_at: i64,
    pub exited_at: Option<i64>,
    pub reason: String,
}

/// A card's episodes from its activity log (created, then each move).
fn card_episodes(
    c: &kls_store::board::Card,
    acts: &[kls_store::board::CardActivity],
) -> Vec<Episode> {
    let mut acts: Vec<&kls_store::board::CardActivity> = acts.iter().collect();
    acts.sort_by_key(|a| (a.at, a.id));
    let mut first = None;
    let mut moves = Vec::new();
    for a in acts {
        let d: serde_json::Value = serde_json::from_str(&a.detail_json).unwrap_or_default();
        let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match a.kind.as_str() {
            "created" if !s("status").is_empty() => first = Some(s("status")),
            "moved" if !s("to").is_empty() => {
                if first.is_none() && !s("from").is_empty() {
                    first = Some(s("from"));
                }
                moves.push((a.at, s("to")));
            }
            _ => {}
        }
    }
    let base = Episode {
        subject: format!("card:{}", c.id),
        repo: c.repo.to_lowercase(),
        title: c.title.chars().take(200).collect(),
        ..Default::default()
    };
    let mut out: Vec<Episode> = vec![Episode {
        stage: format!("card.{}", first.unwrap_or_else(|| c.status.clone())),
        entered_at: c.created_at,
        ..base.clone()
    }];
    for (at, to) in moves {
        let last = out.last_mut().expect("one episode");
        if last.stage == format!("card.{to}") {
            continue;
        }
        last.exited_at = Some(at);
        last.reason = format!("moved:{to}");
        out.push(Episode {
            stage: format!("card.{to}"),
            entered_at: at,
            ..base.clone()
        });
    }
    if c.deleted
        && let Some(last) = out.last_mut()
        && last.exited_at.is_none()
    {
        last.exited_at = Some(c.updated_at);
        last.reason = "deleted".into();
    }
    out
}

/// Every episode the sources hold now: cards, pull requests, issues.
pub async fn lifecycle_episodes(core: &Core) -> Result<Vec<Episode>> {
    let store = core.store();
    let mut out = Vec::new();
    for c in core.cards().await? {
        let acts = core.card_activity(&c.id).await?;
        out.extend(card_episodes(&c, &acts));
    }
    for p in store.gh_pulls(None, None).await? {
        let exit = p.merged_at.or(p.closed_at);
        out.push(Episode {
            stage: "pr.open".into(),
            subject: format!("pr:{}#{}", p.repo.to_lowercase(), p.number),
            repo: p.repo.to_lowercase(),
            title: p.title.chars().take(200).collect(),
            entered_at: p.created_at,
            exited_at: exit,
            reason: match (exit, p.merged) {
                (None, _) => String::new(),
                (Some(_), true) => "merged".into(),
                (Some(_), false) => "abandoned".into(),
            },
        });
    }
    for i in store.gh_issues(None, None).await? {
        out.push(Episode {
            stage: "issue.open".into(),
            subject: format!("issue:{}#{}", i.repo.to_lowercase(), i.number),
            repo: i.repo.to_lowercase(),
            title: i.title.chars().take(200).collect(),
            entered_at: i.created_at,
            exited_at: i.closed_at,
            reason: match (i.closed_at, i.state_reason.as_str()) {
                (None, _) => String::new(),
                (Some(_), "") => "closed".into(),
                (Some(_), r) => r.to_string(),
            },
        });
    }
    Ok(out)
}

fn ledger_draft(e: &Episode, kind: &str, origin: &str) -> kls_store::audit::LedgerDraft {
    let (at, wait) = match kind {
        "exited" => (
            e.exited_at.unwrap_or(0),
            e.exited_at.map(|x| x - e.entered_at),
        ),
        _ => (e.entered_at, None),
    };
    kls_store::audit::LedgerDraft {
        kind: "lifecycle".into(),
        actor: format!("lifecycle {origin}"),
        actor_kind: if origin == "owner" { "user" } else { "system" }.into(),
        msg: format!("lifecycle {kind} {} {}", e.stage, e.subject),
        subject: serde_json::json!({ "stage": e.stage, "key": e.subject, "repo": e.repo }),
        payload: serde_json::json!({ "type": kind, "stage": e.stage, "key": e.subject,
            "title": e.title, "at": at, "enteredAt": e.entered_at, "reason": e.reason,
            "waitMs": wait, "origin": origin, "source": "derive" }),
        origin: "factory".into(),
        ref_key: Some(format!(
            "lifecycle:{kind}:{}:{}:{}",
            e.stage, e.subject, e.entered_at
        )),
        ..Default::default()
    }
}

/// The lifecycle sweep: writes each episode's entry and exit into the
/// hash-chained ledger once (kind `lifecycle`, deduplicated by ref), and emits
/// `lifecycle.entered` / `lifecycle.exited` for changes within the last hour.
pub async fn lifecycle_sweep(core: &Core, origin: &str) -> Result<SweepReport> {
    let mut rep = SweepReport::default();
    if let Some(why) = sweep_gate(core, "delivery.lifecycle", origin).await {
        rep.refused = Some(why);
        return Ok(rep);
    }
    let now = kls_store::now_ms();
    for e in lifecycle_episodes(core).await? {
        let mut kinds = vec!["entered"];
        if e.exited_at.is_some() {
            kinds.push("exited");
        }
        for k in kinds {
            let d = ledger_draft(&e, k, origin);
            let at = d.payload["at"].as_i64().unwrap_or(0);
            if core.audit_record(d.clone()).await?.is_none() {
                continue;
            }
            if k == "entered" {
                rep.entered += 1;
            } else {
                rep.exited += 1;
            }
            if now - at <= 3_600_000 {
                core.emit(&format!("lifecycle.{k}"), &e.repo, &d.payload)
                    .await?;
            }
        }
    }
    // The pass markers coverage compares up to (web lifecycle markers).
    let store = core.store();
    if store.setting(LIFECYCLE_FIRST_PASS).await?.is_none() {
        store
            .set_setting(LIFECYCLE_FIRST_PASS, &now.to_string())
            .await?;
    }
    store
        .set_setting(LIFECYCLE_LAST_PASS, &now.to_string())
        .await?;
    Ok(rep)
}

/// When the lifecycle sweep first and last completed a pass (unix ms).
pub const LIFECYCLE_FIRST_PASS: &str = "delivery.lifecycle.first_pass";
pub const LIFECYCLE_LAST_PASS: &str = "delivery.lifecycle.last_pass";

/// Entered, exited and open for a window; `None` where the source cannot
/// count it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub entered: Option<i64>,
    pub exited: Option<i64>,
    pub open: Option<i64>,
}

/// One stage of the coverage report (web `GET /lifecycle/coverage`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CoverageRow {
    pub stage: String,
    pub label: String,
    /// `derive` (from source rows) or `snapshot` (what is open now).
    pub capture: &'static str,
    pub ledger: Counts,
    /// What the source tables say for the same window.
    pub truth: Option<Counts>,
    /// `ok`, `gap` or `not_captured`.
    pub status: &'static str,
    /// The largest difference.
    pub gap: i64,
    pub last_pass_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CoverageReport {
    pub as_of: i64,
    pub days: i64,
    /// The first pass: the ledger holds nothing captured before it.
    pub capture_from: Option<i64>,
    pub stages: Vec<CoverageRow>,
}

/// ok within one or one per cent of the largest truth count; gap beyond;
/// not_captured when the ledger has nothing where the source has something
/// (web `flcCoverageStatus`).
pub fn coverage_status(ledger: Counts, truth: Option<Counts>) -> (&'static str, i64) {
    let Some(t) = truth else {
        return ("ok", 0);
    };
    let (mut gap, mut scale, mut have, mut any) = (0i64, 0i64, false, false);
    for (l, t) in [
        (ledger.entered, t.entered),
        (ledger.exited, t.exited),
        (ledger.open, t.open),
    ] {
        let Some(t) = t else { continue };
        let l = l.unwrap_or(0);
        have |= l != 0;
        any |= t != 0;
        gap = gap.max((l - t).abs());
        scale = scale.max(t);
    }
    let tol = ((scale as f64) * 0.01).ceil().max(1.0) as i64;
    if !have && any {
        ("not_captured", gap)
    } else if gap <= tol {
        ("ok", gap)
    } else {
        ("gap", gap)
    }
}

fn stage_label(stage: &str) -> String {
    match stage {
        "pr.open" => "Pull requests open".into(),
        "issue.open" => "Issues open".into(),
        s => match s.strip_prefix("card.") {
            Some(list) => format!("Cards in {list}"),
            None => s.to_string(),
        },
    }
}

/// Per stage: `(key, entered at)` → (entry recorded, exit at).
type StageEpisodes = BTreeMap<(String, i64), (bool, Option<i64>)>;

/// Per stage, the stored ledger against the source rows for the last
/// `days` up to the last pass (so a change after it is no gap), from the
/// first pass on. Pull requests and issues compare entered, exited and
/// open; card lists compare what is open.
pub async fn lifecycle_coverage(core: &Core, days: i64) -> Result<CoverageReport> {
    let store = core.store();
    let now = kls_store::now_ms();
    let mark = |v: Option<String>| v.and_then(|s| s.trim().parse::<i64>().ok());
    let first = mark(store.setting(LIFECYCLE_FIRST_PASS).await?);
    let last = mark(store.setting(LIFECYCLE_LAST_PASS).await?);
    let to = last.unwrap_or(now);
    let from = (to - days.max(1) * 86_400_000).max(first.unwrap_or(i64::MIN));
    let within = |at: i64| at >= from && at < to;

    // The ledger: per (stage, key, entered) its entry and exit times.
    let rows = core
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["lifecycle".into()],
            limit: 50_000,
            ..Default::default()
        })
        .await?;
    let mut episodes: BTreeMap<String, StageEpisodes> = BTreeMap::new();
    for r in &rows {
        let p = &r.payload;
        let stage = p["stage"].as_str().unwrap_or("").to_string();
        let key = p["key"].as_str().unwrap_or("").to_string();
        let entered = p["enteredAt"].as_i64().unwrap_or(0);
        let e = episodes
            .entry(stage)
            .or_default()
            .entry((key, entered))
            .or_insert((false, None));
        match p["type"].as_str() {
            Some("entered") => e.0 = true,
            Some("exited") => e.1 = p["at"].as_i64(),
            _ => {}
        }
    }
    let ledger_counts = |stage: &str| {
        let mut c = Counts {
            entered: Some(0),
            exited: Some(0),
            open: Some(0),
        };
        for ((_, entered), (has_entry, exit)) in episodes.get(stage).into_iter().flatten() {
            if *has_entry && within(*entered) {
                *c.entered.as_mut().unwrap() += 1;
            }
            match exit {
                Some(x) if within(*x) => *c.exited.as_mut().unwrap() += 1,
                Some(_) => {}
                None if *has_entry => *c.open.as_mut().unwrap() += 1,
                None => {}
            }
        }
        c
    };

    // The source rows.
    let mut stages: BTreeMap<String, (&'static str, Counts)> = BTreeMap::new();
    let pulls = store.gh_pulls(None, None).await?;
    let count = |it: &mut dyn Iterator<Item = bool>| Some(it.filter(|b| *b).count() as i64);
    stages.insert(
        "pr.open".into(),
        (
            "derive",
            Counts {
                entered: count(&mut pulls.iter().map(|p| within(p.created_at))),
                exited: count(
                    &mut pulls
                        .iter()
                        .map(|p| p.merged_at.or(p.closed_at).is_some_and(within)),
                ),
                open: count(&mut pulls.iter().map(|p| p.state == "open")),
            },
        ),
    );
    let issues = store.gh_issues(None, None).await?;
    stages.insert(
        "issue.open".into(),
        (
            "derive",
            Counts {
                entered: count(&mut issues.iter().map(|i| within(i.created_at))),
                exited: count(&mut issues.iter().map(|i| i.closed_at.is_some_and(within))),
                open: count(&mut issues.iter().map(|i| i.state == "open")),
            },
        ),
    );
    let mut lists: BTreeMap<String, i64> = BTreeMap::new();
    for c in core.cards().await?.iter().filter(|c| !c.deleted) {
        *lists.entry(c.status.clone()).or_default() += 1;
    }
    // Card lists the ledger knows and the board holds now.
    for stage in episodes.keys().filter(|s| s.starts_with("card.")) {
        lists.entry(stage["card.".len()..].to_string()).or_default();
    }
    for (list, open) in lists {
        stages.insert(
            format!("card.{list}"),
            (
                "derive",
                Counts {
                    open: Some(open),
                    ..Default::default()
                },
            ),
        );
    }
    let stages = stages
        .into_iter()
        .map(|(stage, (capture, truth))| {
            // Card lists compare what is open only (their truth has no
            // entered / exited counts, so those are not compared).
            let ledger = ledger_counts(&stage);
            let (mut status, gap) = coverage_status(ledger, Some(truth));
            if last.is_none() {
                status = "not_captured";
            }
            CoverageRow {
                label: stage_label(&stage),
                stage,
                capture,
                ledger,
                truth: Some(truth),
                status,
                gap,
                last_pass_at: last,
            }
        })
        .collect();
    Ok(CoverageReport {
        as_of: now,
        days,
        capture_from: first,
        stages,
    })
}

/// Per-stage flow read from the stored ledger over `[since, now]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LedgerFlow {
    pub stage: String,
    pub open: i64,
    pub arrivals: i64,
    pub completions: i64,
    pub median_wait_ms: Option<i64>,
}

/// The ledger's flow per stage: open episodes, arrivals and completions since
/// `since`, and the median wait of the completions.
pub async fn ledger_flow(core: &Core, since: i64) -> Result<Vec<LedgerFlow>> {
    let rows = core
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["lifecycle".into()],
            limit: 10_000,
            ..Default::default()
        })
        .await?;
    let mut open: BTreeMap<(String, String), bool> = BTreeMap::new();
    let mut by: BTreeMap<String, (LedgerFlow, Vec<i64>)> = BTreeMap::new();
    for r in rows.iter().rev() {
        let p = &r.payload;
        let stage = p["stage"].as_str().unwrap_or("").to_string();
        let key = p["key"].as_str().unwrap_or("").to_string();
        let at = p["at"].as_i64().unwrap_or(0);
        let f = by.entry(stage.clone()).or_insert_with(|| {
            (
                LedgerFlow {
                    stage: stage.clone(),
                    ..Default::default()
                },
                Vec::new(),
            )
        });
        match p["type"].as_str() {
            Some("entered") => {
                open.insert((stage, key), true);
                if at >= since {
                    f.0.arrivals += 1;
                }
            }
            Some("exited") => {
                open.insert((stage, key), false);
                if at >= since {
                    f.0.completions += 1;
                    f.1.extend(p["waitMs"].as_i64());
                }
            }
            _ => {}
        }
    }
    for ((stage, _), is_open) in open {
        if is_open && let Some(f) = by.get_mut(&stage) {
            f.0.open += 1;
        }
    }
    Ok(by
        .into_values()
        .map(|(mut f, waits)| {
            f.median_wait_ms = median(waits);
            f
        })
        .collect())
}

// ---- weighted role table --------------------------------------------------------

/// One row of the weighted role table: evidence `kind` (`profile`, `label`,
/// `tag`, `word`, `weak`) naming `role` with `weight`. A role scores the sum,
/// over groups, of its heaviest matching rule.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RoleRule {
    pub kind: &'static str,
    pub value: &'static str,
    pub role: &'static str,
    pub weight: i64,
}

const fn rr(kind: &'static str, value: &'static str, role: &'static str, weight: i64) -> RoleRule {
    RoleRule {
        kind,
        value,
        role,
        weight,
    }
}

/// A dedicated worker's weight: it decides alone.
pub const ROLE_DEDICATED: i64 = 1000;
/// Below this no role but `sse` has evidence.
pub const ROLE_MIN_SCORE: i64 = 40;
/// A winner other than `sse` needs this lead over the runner-up.
pub const ROLE_MARGIN: i64 = 10;
/// On equal scores the more specific discipline wins.
pub const ROLE_TIE_ORDER: [&str; 7] = [
    "security", "qa", "devops", "reviewer", "writer", "planner", "sse",
];

/// The weighted role table (a port of the web role rules).
pub const ROLE_RULES: &[RoleRule] = &[
    rr("profile", "patcher", "security", ROLE_DEDICATED),
    rr("profile", "vuln-scout", "security", ROLE_DEDICATED),
    rr("profile", "triage", "security", ROLE_DEDICATED),
    rr("profile", "secret-triager", "security", ROLE_DEDICATED),
    rr("profile", "verifier", "security", 90),
    rr("profile", "test-fixer", "qa", ROLE_DEDICATED),
    rr("profile", "lint-fixer", "qa", ROLE_DEDICATED),
    rr("profile", "flaky-hunter", "qa", ROLE_DEDICATED),
    rr("profile", "ci-explainer", "devops", ROLE_DEDICATED),
    rr("profile", "workflow-fixer", "devops", ROLE_DEDICATED),
    rr("profile", "container-fixer", "devops", ROLE_DEDICATED),
    rr("profile", "release-steward", "devops", ROLE_DEDICATED),
    rr("profile", "reviewer", "reviewer", ROLE_DEDICATED),
    rr("profile", "pr-reviewer", "reviewer", ROLE_DEDICATED),
    rr("profile", "planner", "planner", ROLE_DEDICATED),
    rr("profile", "designer", "planner", ROLE_DEDICATED),
    rr("profile", "ticket-writer", "writer", ROLE_DEDICATED),
    rr("profile", "scout", "planner", 60),
    rr("profile", "repro", "planner", 60),
    rr("profile", "conflict-resolver", "sse", 70),
    rr("profile", "builder", "sse", 40),
    rr("label", "security", "security", 85),
    rr("label", "vuln", "security", 85),
    rr("label", "cve", "security", 85),
    rr("label", "secrets", "security", 85),
    rr("label", "malware", "security", 85),
    rr("label", "tests", "qa", 85),
    rr("label", "test", "qa", 85),
    rr("label", "lint", "qa", 85),
    rr("label", "flaky", "qa", 85),
    rr("label", "ci", "devops", 70),
    rr("label", "infra", "devops", 70),
    rr("label", "release", "devops", 70),
    rr("label", "docker", "devops", 70),
    rr("label", "docs", "writer", 70),
    rr("label", "changelog", "writer", 70),
    rr("label", "plan", "planner", 55),
    rr("label", "design", "planner", 55),
    rr("label", "pr-review", "reviewer", 50),
    rr("label", "merge-conflict", "sse", 60),
    rr("tag", "vuln", "security", 90),
    rr("tag", "security", "security", 90),
    rr("tag", "test", "qa", 90),
    rr("tag", "quality", "qa", 90),
    rr("tag", "ci", "devops", 90),
    rr("tag", "deploy", "devops", 90),
    rr("tag", "docs", "writer", 90),
    rr("tag", "plan", "planner", 90),
    rr("tag", "feature", "sse", 90),
    rr("tag", "fix", "sse", 90),
    rr("word", "cve", "security", 60),
    rr("word", "vulnerability", "security", 60),
    rr("word", "secret", "security", 60),
    rr("word", "sbom", "security", 60),
    rr("word", "test", "qa", 60),
    rr("word", "tests", "qa", 60),
    rr("word", "flaky", "qa", 60),
    rr("word", "lint", "qa", 60),
    rr("word", "ci", "devops", 60),
    rr("word", "pipeline", "devops", 60),
    rr("word", "terraform", "devops", 60),
    rr("word", "docker", "devops", 60),
    rr("word", "deploy", "devops", 60),
    rr("word", "docs", "writer", 60),
    rr("word", "readme", "writer", 60),
    rr("word", "changelog", "writer", 60),
    rr("word", "plan", "planner", 60),
    rr("word", "investigate", "planner", 60),
    rr("word", "triage", "planner", 60),
    rr("word", "implement", "sse", 60),
    rr("word", "feature", "sse", 60),
    rr("word", "refactor", "sse", 60),
    rr("word", "bug", "sse", 60),
    rr("weak", "regression", "qa", 15),
    rr("weak", "release", "devops", 15),
    rr("weak", "logs", "devops", 15),
    rr("weak", "review", "reviewer", 15),
    rr("weak", "guide", "writer", 15),
    rr("weak", "design", "planner", 15),
    rr("weak", "scope", "planner", 15),
];

/// The role a piece of work counts as, with its score per role and the
/// rules that decided it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RoleVerdict {
    pub role: String,
    pub score: BTreeMap<String, i64>,
    pub rules: Vec<String>,
    pub ambiguous: bool,
}

/// Classifies work by the weighted role table: profile (`belai:` prefix
/// dropped), card labels, `[tag]`s and words of the title.
pub fn classify_weighted(agent: &str, labels: &[String], title: &str) -> RoleVerdict {
    let profile = agent.trim().to_lowercase();
    let profile = profile.trim_start_matches("belai:");
    let lower = title.to_lowercase();
    let words: BTreeSet<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let tags: BTreeSet<String> = lower
        .split('[')
        .skip(1)
        .filter_map(|s| s.split_once(']').map(|(t, _)| t.trim().to_string()))
        .collect();
    let labels: BTreeSet<String> = labels.iter().map(|l| l.trim().to_lowercase()).collect();
    // Heaviest matching rule per (role, kind).
    let mut best: BTreeMap<(&str, &str), &RoleRule> = BTreeMap::new();
    for r in ROLE_RULES {
        let hit = match r.kind {
            "profile" => profile == r.value,
            "label" => labels.contains(r.value),
            "tag" => tags.contains(r.value),
            _ => words.contains(r.value),
        };
        if hit {
            let e = best.entry((r.role, r.kind)).or_insert(r);
            if r.weight > e.weight {
                *e = r;
            }
        }
    }
    let mut v = RoleVerdict::default();
    let mut rules_of: BTreeMap<&str, Vec<&RoleRule>> = BTreeMap::new();
    for ((role, _), r) in &best {
        *v.score.entry(role.to_string()).or_default() += r.weight;
        rules_of.entry(role).or_default().push(r);
    }
    let rank = |r: &str| ROLE_TIE_ORDER.iter().position(|x| *x == r).unwrap_or(99);
    let mut ranked: Vec<(&String, &i64)> = v.score.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(rank(a.0).cmp(&rank(b.0))));
    let pick = match ranked.as_slice() {
        [] => None,
        [(r, s)] => Some((r.as_str(), **s, 0)),
        [(r, s), (_, s2), ..] => Some((r.as_str(), **s, **s2)),
    };
    v.role = match pick {
        Some((r, s, _)) if s >= ROLE_DEDICATED => r.to_string(),
        Some((r, s, next)) if s >= ROLE_MIN_SCORE && (r == "sse" || s - next >= ROLE_MARGIN) => {
            r.to_string()
        }
        Some(_) => {
            v.ambiguous = true;
            "sse".into()
        }
        None => "sse".into(),
    };
    if let Some(rs) = rules_of.get(v.role.as_str()) {
        let mut rs = rs.clone();
        rs.sort_by_key(|r| std::cmp::Reverse(r.weight));
        v.rules = rs
            .iter()
            .map(|r| format!("{}:{}", r.kind, r.value))
            .collect();
    }
    v
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::board::CardDraft;
    use crate::estimates::{EstimateInputs, EstimateSettings, estimate};

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Core::open(dir.path(), None).await.unwrap(), dir)
    }

    #[test]
    fn today_and_month_to_date() {
        // 2026-11-22T10:00:00Z
        let now = 1_795_341_600_000;
        let t = Range::today(now);
        assert_eq!(t.from, kls_store::github::day_start(now));
        assert_eq!(t.days(), 1);
        let m = Range::month_to_date(now);
        // 2026-11-01T00:00:00Z
        assert_eq!(m.from, 1_793_491_200_000);
        assert_eq!(m.label, "mtd");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    /// Pins the estimate model: a fixed input priced with the default
    /// settings must match `testdata/estimate.golden.json` exactly
    /// (`KLS_UPDATE_GOLDEN=1` rewrites it).
    #[test]
    fn estimate_golden() {
        let mut inputs = EstimateInputs::default();
        inputs.agent_hours.insert("sse".into(), 3.5);
        inputs.agent_hours.insert("security".into(), 1.25);
        inputs.agent_hours.insert("qa".into(), 0.5);
        inputs.decisions.insert("plan".into(), 2);
        inputs.decisions.insert("merge".into(), 1);
        inputs.decisions.insert("toolcall".into(), 40);
        inputs.people_hours = Some(1.5);
        inputs.model_cost_usd = Some(12.40);
        inputs.sources = vec!["card:K-golden".into()];
        let e = estimate(&inputs, &EstimateSettings::default());
        let got = serde_json::to_string_pretty(&e).unwrap() + "\n";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/estimate.golden.json");
        if std::env::var("KLS_UPDATE_GOLDEN").is_ok() {
            std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata")).unwrap();
            std::fs::write(path, &got).unwrap();
        }
        let want = std::fs::read_to_string(path).expect("golden file");
        assert_eq!(got, want, "estimate drifted from {path}");
    }

    #[test]
    fn weighted_roles() {
        let v = classify_weighted("belai:patcher", &[], "Refactor login");
        assert_eq!(v.role, "security");
        assert_eq!(v.rules, vec!["profile:patcher".to_string()]);
        let v = classify_weighted("", &["tests".into()], "Fix flaky test");
        assert_eq!(v.role, "qa");
        assert!(!v.ambiguous);
        // Thin evidence stays with sse.
        let v = classify_weighted("", &[], "review the logs");
        assert_eq!(v.role, "sse");
        assert!(v.ambiguous);
        assert_eq!(classify_weighted("", &[], "").role, "sse");
    }

    #[tokio::test]
    async fn outcome_detail_links_card_sessions_and_people() {
        let (core, _d) = core().await;
        let card = core
            .create_card(
                CardDraft {
                    title: "Fix login".into(),
                    repo: "acme/api".into(),
                    pr_url: "https://github.com/acme/api/pull/7".into(),
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap();
        let s = core.store();
        s.upsert_outcome(&Outcome {
            outcome_key: "pr:acme/api#7".into(),
            kind: "pr".into(),
            title: "Fix login".into(),
            repo: "acme/api".into(),
            result: "merged".into(),
            opened_at: Some(1_000),
            resolved_at: Some(50_000),
            pr_ref: "acme/api#7".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        s.upsert_action(&HumanAction {
            source: "review".into(),
            ref_id: "r1".into(),
            action_kind: "review_approved".into(),
            actor_key: "gh:bo".into(),
            actor_login: "bo".into(),
            outcome_key: Some("pr:acme/api#7".into()),
            occurred_at: 20_000,
            raw_units_json: r#"{"lines":100}"#.into(),
            basis: "derived".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let d = outcome(&core, "pr:acme/api#7").await.unwrap().unwrap();
        assert_eq!(d.links.card_key, card.key);
        assert_eq!(d.people.len(), 1);
        assert_eq!(d.people[0].key, "gh:bo");
        let whats: Vec<&str> = d.timeline.iter().map(|t| t.what.as_str()).collect();
        assert_eq!(whats, vec!["opened", "review_approved", "resolved"]);
        assert!(outcome(&core, "pr:nope#1").await.unwrap().is_none());

        // The linking sweep stores the card on the outcome and audits it.
        let r = link_outcomes(&core, "owner").await.unwrap();
        assert_eq!(r.linked, 1);
        assert_eq!(
            s.outcome("pr:acme/api#7").await.unwrap().unwrap().card_id,
            card.id
        );
        let led = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["delivery".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(led.iter().any(|e| e.subject["action"] == "outcome.link"
            && e.actor == "owner"
            && e.payload["cardId"] == card.id.as_str()));
        // Idempotent.
        assert_eq!(link_outcomes(&core, "owner").await.unwrap().linked, 0);
        let r = rollup(&core, "sweep").await.unwrap();
        assert!(r.refused.is_none());
    }

    #[tokio::test]
    async fn lifecycle_ledger_is_stored_once_and_read_back() {
        let (core, _d) = core().await;
        let c = core
            .create_card(
                CardDraft {
                    title: "Ship".into(),
                    status: "todo".into(),
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap();
        core.move_card(&c.id, c.version, "doing", "t")
            .await
            .unwrap();
        let r = lifecycle_sweep(&core, "sweep").await.unwrap();
        assert_eq!((r.entered, r.exited), (2, 1), "{r:?}");
        // A second pass writes nothing new.
        let r = lifecycle_sweep(&core, "sweep").await.unwrap();
        assert_eq!((r.entered, r.exited), (0, 0));
        let flow = ledger_flow(&core, 0).await.unwrap();
        let todo = flow.iter().find(|f| f.stage == "card.todo").unwrap();
        assert_eq!((todo.arrivals, todo.completions, todo.open), (1, 1, 0));
        let doing = flow.iter().find(|f| f.stage == "card.doing").unwrap();
        assert_eq!(doing.open, 1);
        // Provenance: the sweep is the actor and the hash chain verifies.
        let led = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["lifecycle".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(led.len(), 3);
        assert!(led.iter().all(|e| e.actor == "lifecycle sweep"));
        assert!(core.audit_verify().await.unwrap().ok);
    }

    #[test]
    fn coverage_status_matches_the_web_tolerance() {
        let n = |e, x, o| Counts {
            entered: Some(e),
            exited: Some(x),
            open: Some(o),
        };
        assert_eq!(
            coverage_status(n(120, 118, 2), Some(n(120, 118, 2))),
            ("ok", 0)
        );
        assert_eq!(
            coverage_status(n(119, 118, 2), Some(n(120, 118, 2))),
            ("ok", 1)
        );
        assert_eq!(
            coverage_status(n(100, 98, 2), Some(n(120, 118, 2))),
            ("gap", 20)
        );
        // 1% of 500 is 5.
        assert_eq!(coverage_status(n(495, 0, 0), Some(n(500, 0, 0))), ("ok", 5));
        assert_eq!(
            coverage_status(n(0, 0, 0), Some(n(3, 1, 2))),
            ("not_captured", 3)
        );
        assert_eq!(coverage_status(n(0, 0, 0), None), ("ok", 0));
        let open_only = Counts {
            open: Some(4),
            ..Default::default()
        };
        assert_eq!(coverage_status(n(9, 9, 4), Some(open_only)), ("ok", 0));
    }

    #[tokio::test]
    async fn lifecycle_coverage_compares_the_ledger_with_the_sources() {
        let (core, _d) = core().await;
        let c = core
            .create_card(
                CardDraft {
                    title: "Ship".into(),
                    status: "todo".into(),
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap();
        let now = kls_store::now_ms();
        let pull = |n: i64, at: i64| kls_store::github::GhPull {
            repo: "acme/api".into(),
            number: n,
            state: "open".into(),
            created_at: at,
            ..Default::default()
        };
        core.store()
            .gh_upsert_pull(&pull(1, now - 3_600_000))
            .await
            .unwrap();
        // No pass yet: nothing is captured.
        let r = lifecycle_coverage(&core, 7).await.unwrap();
        assert!(r.capture_from.is_none());
        assert!(r.stages.iter().all(|s| s.status == "not_captured"), "{r:?}");
        // After a pass the ledger agrees with the sources.
        lifecycle_sweep(&core, "sweep").await.unwrap();
        let r = lifecycle_coverage(&core, 7).await.unwrap();
        assert!(r.capture_from.is_some());
        let pr = r.stages.iter().find(|s| s.stage == "pr.open").unwrap();
        assert_eq!(pr.label, "Pull requests open");
        assert_eq!(pr.ledger.open, Some(1));
        assert_eq!(pr.truth.unwrap().open, Some(1));
        let todo = r.stages.iter().find(|s| s.stage == "card.todo").unwrap();
        assert_eq!((todo.status, todo.ledger.open), ("ok", Some(1)));
        assert!(
            todo.truth.unwrap().entered.is_none(),
            "card lists compare open"
        );
        assert!(r.stages.iter().all(|s| s.status == "ok"), "{r:?}");
        // Rows the sweep never recorded show as a gap.
        for n in 2..6 {
            core.store()
                .gh_upsert_pull(&pull(n, now - 1_800_000))
                .await
                .unwrap();
        }
        let r = lifecycle_coverage(&core, 7).await.unwrap();
        let pr = r.stages.iter().find(|s| s.stage == "pr.open").unwrap();
        assert_eq!((pr.status, pr.gap), ("gap", 4), "{pr:?}");
        // A card moved after the last pass: one off is within tolerance.
        core.move_card(&c.id, c.version, "doing", "t")
            .await
            .unwrap();
        let r = lifecycle_coverage(&core, 7).await.unwrap();
        let todo = r.stages.iter().find(|s| s.stage == "card.todo").unwrap();
        assert_eq!((todo.status, todo.gap), ("ok", 1));
    }

    #[tokio::test]
    async fn sweeps_are_refused_under_the_kill_switch() {
        let (core, _d) = core().await;
        core.create_card(
            CardDraft {
                title: "x".into(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
        crate::control::pause(&core, "test").await.unwrap();
        for r in [
            lifecycle_sweep(&core, "sweep").await.unwrap(),
            link_outcomes(&core, "sweep").await.unwrap(),
            rollup(&core, "sweep").await.unwrap(),
        ] {
            assert!(r.refused.is_some(), "{r:?}");
            assert_eq!(r.entered + r.linked + r.rolled_up, 0);
        }
        let led = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["lifecycle".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(led.is_empty());
        // The refusal is evented (and audited by control::check).
        let ev = core
            .store()
            .events_with_prefix("factory.refused", None, 10)
            .await
            .unwrap();
        assert!(!ev.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kls_store::github::DecisionEnd;

    const H: i64 = 3_600_000;

    fn action(source: &str, kind: &str, who: &str, key: &str, at: i64, units: &str) -> HumanAction {
        HumanAction {
            source: source.into(),
            ref_id: format!("{source}:{who}:{at}"),
            action_kind: kind.into(),
            actor_key: format!("gh:{who}"),
            actor_login: who.into(),
            outcome_key: Some(key.into()),
            repo: Some("acme/api".into()),
            occurred_at: at,
            raw_units_json: units.into(),
            basis: "derived".into(),
            ..Default::default()
        }
    }

    async fn seeded() -> (Core, tempfile::TempDir, Range) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let s = core.store();
        let now = 100 * 86_400_000;
        let range = Range::last_days(7, now);
        let t = now - 2 * H;
        s.upsert_outcome(&Outcome {
            outcome_key: "pr:acme/api#1".into(),
            kind: "pr".into(),
            title: "=cmd|calc".into(),
            repo: "acme/api".into(),
            result: "merged".into(),
            opened_at: Some(t - 10 * H),
            resolved_at: Some(t),
            cycle_ms: Some(10 * H),
            first_review_ms: Some(H),
            reviewers: vec!["bo".into()],
            decisions_json: Some(r#"{"mode":2,"plan":1}"#.into()),
            ..Default::default()
        })
        .await
        .unwrap();
        s.upsert_outcome(&Outcome {
            outcome_key: "issue:acme/web#2".into(),
            kind: "issue".into(),
            title: "Bug, with comma".into(),
            repo: "acme/web".into(),
            result: "closed".into(),
            resolved_at: Some(t - H),
            cycle_ms: Some(2 * H),
            ..Default::default()
        })
        .await
        .unwrap();
        // Old outcome outside the range.
        s.upsert_outcome(&Outcome {
            outcome_key: "pr:acme/api#0".into(),
            kind: "pr".into(),
            result: "merged".into(),
            resolved_at: Some(now - 30 * 86_400_000),
            ..Default::default()
        })
        .await
        .unwrap();
        for a in [
            action(
                "admin",
                "pr_opened",
                "ana",
                "pr:acme/api#1",
                t - 10 * H,
                r#"{"started":true}"#,
            ),
            action("commit", "commit", "ana", "pr:acme/api#1", t - 9 * H, "{}"),
            action(
                "commit",
                "commit",
                "ana",
                "pr:acme/api#1",
                t - 9 * H + 60_000,
                "{}",
            ),
            action(
                "review",
                "approved",
                "bo",
                "pr:acme/api#1",
                t - 9 * H,
                r#"{"lines":250,"comments":2}"#,
            ),
            action(
                "checkpoint",
                "plan",
                "bo",
                "pr:acme/api#1",
                t - 3 * H,
                &format!(
                    r#"{{"openedAt":{},"decidedAt":{},"verdict":"approved"}}"#,
                    t - 3 * H - 600_000,
                    t - 3 * H
                ),
            ),
        ] {
            s.upsert_action(&a).await.unwrap();
        }
        let mut bot = action(
            "review",
            "commented",
            "dependabot[bot]",
            "pr:acme/api#1",
            t,
            "{}",
        );
        bot.is_bot = true;
        bot.bot_kind = "bot".into();
        s.upsert_action(&bot).await.unwrap();
        s.upsert_person(&Person {
            person_key: "gh:ana".into(),
            login: "ana".into(),
            display_name: "Ana".into(),
            emails: vec!["ana@acme.example".into()],
            is_internal: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
        s.upsert_person(&Person {
            person_key: "gh:bo".into(),
            login: "bo".into(),
            is_internal: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        s.upsert_person(&Person {
            person_key: "gh:dependabot[bot]".into(),
            login: "dependabot[bot]".into(),
            is_bot: true,
            ..Default::default()
        })
        .await
        .unwrap();
        for (sha, signed) in [("a1", Some(true)), ("a2", Some(false)), ("a3", None)] {
            s.upsert_commit(&DeliveryCommit {
                repo: "acme/api".into(),
                sha: sha.into(),
                author_login: "ana".into(),
                author_key: "gh:ana".into(),
                signed,
                committed_at: Some(t - 9 * H),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        s.record_decision(
            t,
            "mode",
            "clef",
            DecisionEnd::Proceeded,
            Some(0.95),
            Some(200),
        )
        .await
        .unwrap();
        s.record_decision(
            t,
            "mode",
            "clef",
            DecisionEnd::Escalated,
            Some(0.55),
            Some(400),
        )
        .await
        .unwrap();
        s.record_decision(t, "plan", "big", DecisionEnd::RuleHandled, Some(0.85), None)
            .await
            .unwrap();
        (core, dir, range)
    }

    #[tokio::test]
    async fn outcomes_report_and_filters() {
        let (core, _d, range) = seeded().await;
        let s = core.store();
        let r = outcomes(s, &range, "", None).await.unwrap();
        assert_eq!(r.total, 2);
        assert_eq!((r.merged_prs, r.closed_issues), (1, 1));
        assert_eq!(r.no_person_needed, 1);
        assert_eq!(r.median_cycle_ms, Some(6 * H));
        assert_eq!(r.by_day.len(), 7);
        assert_eq!(r.by_day.iter().map(|d| d.1).sum::<i64>(), 2);
        let pr = &r.rows[0];
        assert_eq!(pr.key, "pr:acme/api#1");
        assert_eq!(pr.people, 2, "ana and bo, not the bot");
        assert_eq!(pr.people_actions, 5);
        assert_eq!(pr.decisions, Some(3));
        assert_eq!(outcomes(s, &range, "WEB", None).await.unwrap().total, 1);
        assert_eq!(
            outcomes(s, &range, "", Some("gh:bo")).await.unwrap().total,
            1
        );
        assert_eq!(
            outcomes(s, &range, "", Some("gh:zed")).await.unwrap().total,
            0
        );
        let csv = outcomes_csv(&r.rows);
        let lines: Vec<&str> = csv.split("\r\n").collect();
        assert!(lines[0].starts_with("key,kind,title,repo,result"));
        assert!(
            lines[1].contains(",'=cmd|calc,"),
            "formula sanitised: {}",
            lines[1]
        );
        assert!(lines[2].contains("\"Bug, with comma\""));
        assert!(lines[1].contains(",1970-04-10T22:00:00Z,"));
    }

    #[tokio::test]
    async fn decisions_report() {
        let (core, _d, range) = seeded().await;
        let r = decisions(&core, &range).await.unwrap();
        assert_eq!(r.total, 3);
        assert_eq!((r.proceeded, r.rule_handled, r.escalated), (1, 1, 1));
        assert_eq!(r.days.len(), 7);
        assert_eq!(r.histogram[9] + r.histogram[5] + r.histogram[8], 3);
        assert_eq!(r.threshold, 0.8);
        assert_eq!(r.models[0].model, "clef");
        assert_eq!(r.models[0].mean_latency_ms, Some(300));
        assert_eq!(r.models[0].escalated_pct, 50.0);
        assert_eq!(r.kinds.len(), 2);
        assert_eq!(r.asked.total, 1);
        assert_eq!(r.asked.approved, 1);
        assert_eq!(r.asked.by_kind[0].median_wait_ms, Some(600_000));
        let csv = decisions_csv(&r);
        assert_eq!(csv.lines().count(), 8);
        core.store()
            .set_setting(settings::MIN_CONFIDENCE, "0.7")
            .await
            .unwrap();
        assert_eq!(decisions(&core, &range).await.unwrap().threshold, 0.7);
    }

    #[tokio::test]
    async fn people_report_time_rules() {
        let (core, _d, range) = seeded().await;
        let r = people(core.store(), &range).await.unwrap();
        assert_eq!(r.people, 2);
        assert_eq!(r.actions, 5);
        assert_eq!(r.bots.len(), 1);
        assert_eq!(r.bots[0].login, "dependabot[bot]");
        let bo = r.matrix.iter().find(|p| p.login == "bo").unwrap();
        // Review: 250 lines -> 10 min, 2 comments -> 3 min; checkpoint wait 10 min.
        assert_eq!(bo.time_ms, 13 * 60_000 + 10 * 60_000);
        let ana = r.matrix.iter().find(|p| p.login == "ana").unwrap();
        assert_eq!(ana.name, "Ana");
        // Two sittings start (30 s each), the second commit 60 s later.
        assert_eq!(ana.time_ms, 30_000 + 30_000 + 60_000);
        assert_eq!(r.waiting_on_person_ms, Some(600_000));
        assert!(people_csv(&r).starts_with("person_key,name,login,checkpoint"));
    }

    #[tokio::test]
    async fn contributors_and_detail() {
        let (core, _d, range) = seeded().await;
        let s = core.store();
        let r = contributors(s, &range, Scope::All, "").await.unwrap();
        assert_eq!((r.count_all, r.count_internal, r.count_external), (2, 1, 1));
        assert_eq!(r.outcomes_with_person, 1);
        assert_eq!(r.no_person_needed, 1);
        assert_eq!(r.decisions_answered, 1);
        let ana = r.items.iter().find(|c| c.login == "ana").unwrap();
        assert_eq!(ana.started, 1);
        assert_eq!(ana.signed_off, 2);
        assert_eq!(ana.signed_rate, Some(50.0));
        assert_eq!(ana.repos_top, vec!["acme/api".to_string()]);
        assert_eq!(
            contributors(s, &range, Scope::External, "")
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        assert_eq!(
            contributors(s, &range, Scope::All, "acme.example")
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        let review = r.steps_in.iter().find(|x| x.id == "pr_review").unwrap();
        assert_eq!((review.count, review.outcomes), (1, 1));
        assert_eq!(review.pct_of_outcomes, Some(100.0));
        assert!(contributors_csv(&r).lines().count() == 3);

        let d = contributor(s, "gh:ana", &range).await.unwrap().unwrap();
        assert_eq!(d.name, "Ana");
        assert_eq!(d.outcomes_touched, 1);
        assert_eq!(
            d.work[0].parts,
            vec!["started".to_string(), "signed".to_string()]
        );
        assert_eq!(d.work[0].others, vec!["bo".to_string()]);
        assert_eq!((d.signed_verified, d.signed_total), (1, 2));
        assert_eq!(d.identities[0].kind, "github");
        assert_eq!(d.parts.len(), 8);
        assert_eq!(d.days.len(), 7);
        assert!(
            contributor(s, "gh:dependabot[bot]", &range)
                .await
                .unwrap()
                .is_none()
        );
        assert!(contributor(s, "gh:nobody", &range).await.unwrap().is_none());
    }

    #[test]
    fn csv_cells_and_dates() {
        assert_eq!(csv_cell("@x", true), "'@x");
        assert_eq!(csv_cell("  -x", true), "'  -x");
        assert_eq!(csv_cell("-5", false), "-5");
        assert_eq!(csv_cell("a\"b", true), "\"a\"\"b\"");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_792_661_531_000), "2026-10-22T09:32:11Z");
        let r = Range::last_days(7, 10 * 86_400_000 + 5);
        assert_eq!(r.day_starts().len(), 7);
        assert_eq!(r.days(), 7);
        assert_eq!(median(vec![3, 1, 2]), Some(2));
        assert_eq!(median(vec![]), None);
    }
}
