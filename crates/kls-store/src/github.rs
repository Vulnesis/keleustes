//! Store: M4 GitHub plane: repos, orgs, pulls, issues mirror, conflicts, delivery facts. Migration 0006.
//!
//! Write SQL once with `?` placeholders via `self.sql(..)`; keep to types
//! both dialects share (i64, f64, text, bytes; JSON as text, booleans as
//! 0/1 BIGINT). Add the same migration to migrations/sqlite and
//! migrations/postgres.

use super::*;

fn b(v: bool) -> i64 {
    v as i64
}

fn get_bool(r: &AnyRow, col: &str) -> Result<bool> {
    Ok(r.try_get::<i64, _>(col)? != 0)
}

fn get_opt_bool(r: &AnyRow, col: &str) -> Result<Option<bool>> {
    Ok(r.try_get::<Option<i64>, _>(col)?.map(|v| v != 0))
}

fn json_vec(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

/// Start of the UTC hour holding `ms`.
pub fn hour_bucket(ms: i64) -> i64 {
    ms - ms.rem_euclid(3_600_000)
}

/// Start of the UTC day holding `ms`.
pub fn day_start(ms: i64) -> i64 {
    ms - ms.rem_euclid(86_400_000)
}

// ---- types -----------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GhOrg {
    pub login: String,
    /// "Organization" or "User".
    pub kind: String,
    pub managed: bool,
    pub enabled: bool,
    pub repo_count: i64,
    pub avatar_url: String,
    pub description: String,
    pub synced_at: i64,
}

/// Per-repository defaults (factory.md 2.5 settings).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoSettings {
    /// "observe", "suggest", "draft_pr", "pr", "merge".
    pub autonomy: String,
    /// "off", "mirror", "all".
    pub kanban_mode: String,
    pub monthly_token_cap: Option<i64>,
    pub protected_branches: bool,
    pub tea_publish: bool,
    pub watched: bool,
    /// Default workflow keys this repository opted out of.
    pub defaults_off: Vec<String>,
    /// Merge method for the Merge action: "merge", "squash", "rebase".
    pub merge_method: String,
}

impl Default for RepoSettings {
    fn default() -> Self {
        Self {
            autonomy: "observe".into(),
            kanban_mode: "off".into(),
            monthly_token_cap: None,
            protected_branches: true,
            tea_publish: false,
            watched: false,
            defaults_off: Vec::new(),
            merge_method: "squash".into(),
        }
    }
}

pub const AUTONOMY_LEVELS: [&str; 5] = ["observe", "suggest", "draft_pr", "pr", "merge"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhRepo {
    pub full_name: String,
    pub owner: String,
    pub name: String,
    pub visibility: String,
    pub default_branch: String,
    pub archived: bool,
    pub fork: bool,
    pub html_url: String,
    pub description: String,
    pub can_push: bool,
    pub managed: bool,
    pub mirror_pulls: bool,
    pub mirror_issues: bool,
    pub sync_labels: bool,
    pub settings: RepoSettings,
    pub revision: i64,
    pub pushed_at: Option<i64>,
    pub synced_at: i64,
}

/// Mirroring switches of one repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mirroring {
    pub pulls: bool,
    pub issues: bool,
    pub labels: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChecksSummary {
    pub total: i64,
    pub success: i64,
    pub failure: i64,
    pub pending: i64,
    pub skipped: i64,
    /// Names of failing checks (at most 10).
    pub failing: Vec<String>,
}

impl ChecksSummary {
    /// "success", "failure", "pending" or "none".
    pub fn state(&self) -> &'static str {
        if self.total == 0 {
            "none"
        } else if self.failure > 0 {
            "failure"
        } else if self.pending > 0 {
            "pending"
        } else {
            "success"
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhPull {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub author: String,
    pub head_ref: String,
    pub head_sha: String,
    pub head_repo: String,
    pub base_ref: String,
    pub base_sha: String,
    pub mergeable: String,
    pub mergeable_state: String,
    pub review_decision: String,
    pub requested_reviewers: Vec<String>,
    pub labels: Vec<String>,
    pub checks_state: String,
    pub checks: ChecksSummary,
    pub html_url: String,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub merged_at: Option<i64>,
    pub closed_at: Option<i64>,
    pub synced_at: i64,
    pub delivery_captured: bool,
}

impl GhPull {
    pub fn is_fork(&self) -> bool {
        !self.head_repo.is_empty() && !self.head_repo.eq_ignore_ascii_case(&self.repo)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhIssue {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub state_reason: String,
    pub author: String,
    pub author_association: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub body: String,
    pub html_url: String,
    pub comments: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    pub card_id: Option<String>,
    pub synced_at: i64,
}

/// What an upsert changed, so callers emit the right event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upsert {
    Inserted,
    Updated,
    Unchanged,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhLabel {
    pub repo: String,
    pub name: String,
    pub color: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EtagEntry {
    pub etag: String,
    pub link: String,
    pub body: String,
    pub fetched_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    pub route: String,
    pub resource: String,
    pub limit: i64,
    pub remaining: i64,
    pub used: i64,
    pub reset_at: i64,
    pub observed_at: i64,
    pub secondary_until: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CallCount {
    pub caller: String,
    pub calls: i64,
    pub not_modified: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub id: String,
    pub repo: String,
    pub number: i64,
    pub branch: String,
    pub commit_sha: String,
    pub parent_shas: Vec<String>,
    pub files: Vec<String>,
    pub signing: String,
    pub verified: Option<bool>,
    pub verification_reason: String,
    pub route: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub outcome_key: String,
    /// "pr", "issue", "card", "run", "session".
    pub kind: String,
    pub title: String,
    pub repo: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub author: String,
    pub opened_at: Option<i64>,
    pub started_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub result: String,
    pub role_key: String,
    pub active_ms: Option<i64>,
    pub wait_ms: Option<i64>,
    pub wall_ms: Option<i64>,
    pub cycle_ms: Option<i64>,
    pub first_review_ms: Option<i64>,
    pub cost_micro_usd: Option<i64>,
    /// `{"toolcall": n, ...}` when recorded.
    pub decisions_json: Option<String>,
    pub escalated: Option<i64>,
    pub reviewers: Vec<String>,
    pub commits: Option<i64>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub pr_ref: String,
    pub card_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HumanAction {
    /// "checkpoint", "commit", "comment", "review", "chat", "notification", "admin", "pixcall".
    pub source: String,
    pub ref_id: String,
    pub action_kind: String,
    pub actor_key: String,
    pub actor_login: String,
    pub is_bot: bool,
    pub bot_kind: String,
    pub outcome_key: Option<String>,
    pub repo: Option<String>,
    pub occurred_at: i64,
    pub raw_units_json: String,
    /// "measured" or "derived".
    pub basis: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Person {
    /// `gh:<login>`, `email:<address>`.
    pub person_key: String,
    pub login: String,
    pub display_name: String,
    pub avatar_url: String,
    pub emails: Vec<String>,
    pub is_internal: Option<bool>,
    pub association: String,
    pub is_bot: bool,
    pub bot_kind: String,
    pub first_seen_at: Option<i64>,
    pub last_seen_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeliveryCommit {
    pub repo: String,
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub author_login: String,
    pub author_key: String,
    pub is_bot: bool,
    pub trailers: Vec<String>,
    pub signed: Option<bool>,
    pub pr_number: Option<i64>,
    pub outcome_key: Option<String>,
    pub committed_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionDay {
    pub day_start: i64,
    pub kind: String,
    pub model: String,
    pub total: i64,
    pub proceeded: i64,
    pub rule_handled: i64,
    pub escalated: i64,
    pub conf_buckets: Vec<i64>,
    pub latency_sum_ms: i64,
    pub latency_n: i64,
}

/// How a decision ended, for [`Store::record_decision`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionEnd {
    Proceeded,
    RuleHandled,
    Escalated,
}

// ---- column lists and row mappers -----------------------------------------

const ORG_COLS: &str =
    "login, kind, managed, enabled, repo_count, avatar_url, description, synced_at";

fn org_from_row(r: &AnyRow) -> Result<GhOrg> {
    Ok(GhOrg {
        login: r.try_get("login")?,
        kind: r.try_get("kind")?,
        managed: get_bool(r, "managed")?,
        enabled: get_bool(r, "enabled")?,
        repo_count: r.try_get("repo_count")?,
        avatar_url: r.try_get("avatar_url")?,
        description: r.try_get("description")?,
        synced_at: r.try_get("synced_at")?,
    })
}

const REPO_COLS: &str = "full_name, owner, name, visibility, default_branch, archived, fork, \
     html_url, description, can_push, managed, mirror_pulls, mirror_issues, sync_labels, \
     settings_json, revision, pushed_at, synced_at";

fn repo_from_row(r: &AnyRow) -> Result<GhRepo> {
    let settings: String = r.try_get("settings_json")?;
    Ok(GhRepo {
        full_name: r.try_get("full_name")?,
        owner: r.try_get("owner")?,
        name: r.try_get("name")?,
        visibility: r.try_get("visibility")?,
        default_branch: r.try_get("default_branch")?,
        archived: get_bool(r, "archived")?,
        fork: get_bool(r, "fork")?,
        html_url: r.try_get("html_url")?,
        description: r.try_get("description")?,
        can_push: get_bool(r, "can_push")?,
        managed: get_bool(r, "managed")?,
        mirror_pulls: get_bool(r, "mirror_pulls")?,
        mirror_issues: get_bool(r, "mirror_issues")?,
        sync_labels: get_bool(r, "sync_labels")?,
        settings: serde_json::from_str(&settings).unwrap_or_default(),
        revision: r.try_get("revision")?,
        pushed_at: r.try_get("pushed_at")?,
        synced_at: r.try_get("synced_at")?,
    })
}

const PULL_COLS: &str = "repo, number, title, state, draft, merged, author, head_ref, head_sha, \
     head_repo, base_ref, base_sha, mergeable, mergeable_state, review_decision, \
     requested_reviewers_json, labels_json, checks_state, checks_json, html_url, additions, \
     deletions, changed_files, created_at, updated_at, merged_at, closed_at, synced_at, \
     delivery_captured";

fn pull_from_row(r: &AnyRow) -> Result<GhPull> {
    let rr: String = r.try_get("requested_reviewers_json")?;
    let labels: String = r.try_get("labels_json")?;
    let checks: String = r.try_get("checks_json")?;
    Ok(GhPull {
        repo: r.try_get("repo")?,
        number: r.try_get("number")?,
        title: r.try_get("title")?,
        state: r.try_get("state")?,
        draft: get_bool(r, "draft")?,
        merged: get_bool(r, "merged")?,
        author: r.try_get("author")?,
        head_ref: r.try_get("head_ref")?,
        head_sha: r.try_get("head_sha")?,
        head_repo: r.try_get("head_repo")?,
        base_ref: r.try_get("base_ref")?,
        base_sha: r.try_get("base_sha")?,
        mergeable: r.try_get("mergeable")?,
        mergeable_state: r.try_get("mergeable_state")?,
        review_decision: r.try_get("review_decision")?,
        requested_reviewers: json_vec(&rr),
        labels: json_vec(&labels),
        checks_state: r.try_get("checks_state")?,
        checks: serde_json::from_str(&checks).unwrap_or_default(),
        html_url: r.try_get("html_url")?,
        additions: r.try_get("additions")?,
        deletions: r.try_get("deletions")?,
        changed_files: r.try_get("changed_files")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        merged_at: r.try_get("merged_at")?,
        closed_at: r.try_get("closed_at")?,
        synced_at: r.try_get("synced_at")?,
        delivery_captured: get_bool(r, "delivery_captured")?,
    })
}

const ISSUE_COLS: &str = "repo, number, title, state, state_reason, author, author_association, \
     labels_json, assignees_json, body, html_url, comments, created_at, updated_at, closed_at, \
     card_id, synced_at";

fn issue_from_row(r: &AnyRow) -> Result<GhIssue> {
    let labels: String = r.try_get("labels_json")?;
    let assignees: String = r.try_get("assignees_json")?;
    Ok(GhIssue {
        repo: r.try_get("repo")?,
        number: r.try_get("number")?,
        title: r.try_get("title")?,
        state: r.try_get("state")?,
        state_reason: r.try_get("state_reason")?,
        author: r.try_get("author")?,
        author_association: r.try_get("author_association")?,
        labels: json_vec(&labels),
        assignees: json_vec(&assignees),
        body: r.try_get("body")?,
        html_url: r.try_get("html_url")?,
        comments: r.try_get("comments")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        closed_at: r.try_get("closed_at")?,
        card_id: r.try_get("card_id")?,
        synced_at: r.try_get("synced_at")?,
    })
}

const QUOTA_COLS: &str =
    "route, resource, rate_limit, remaining, used, reset_at, observed_at, secondary_until";

fn quota_from_row(r: &AnyRow) -> Result<Quota> {
    Ok(Quota {
        route: r.try_get("route")?,
        resource: r.try_get("resource")?,
        limit: r.try_get("rate_limit")?,
        remaining: r.try_get("remaining")?,
        used: r.try_get("used")?,
        reset_at: r.try_get("reset_at")?,
        observed_at: r.try_get("observed_at")?,
        secondary_until: r.try_get("secondary_until")?,
    })
}

const RESOLUTION_COLS: &str = "id, repo, number, branch, commit_sha, parent_shas_json, files_json, \
     signing, verified, verification_reason, route, created_at";

fn resolution_from_row(r: &AnyRow) -> Result<Resolution> {
    let parents: String = r.try_get("parent_shas_json")?;
    let files: String = r.try_get("files_json")?;
    Ok(Resolution {
        id: r.try_get("id")?,
        repo: r.try_get("repo")?,
        number: r.try_get("number")?,
        branch: r.try_get("branch")?,
        commit_sha: r.try_get("commit_sha")?,
        parent_shas: json_vec(&parents),
        files: json_vec(&files),
        signing: r.try_get("signing")?,
        verified: get_opt_bool(r, "verified")?,
        verification_reason: r.try_get("verification_reason")?,
        route: r.try_get("route")?,
        created_at: r.try_get("created_at")?,
    })
}

const OUTCOME_COLS: &str = "outcome_key, kind, title, repo, trigger_kind, trigger_ref, author, \
     opened_at, started_at, resolved_at, result, role_key, active_ms, wait_ms, wall_ms, cycle_ms, \
     first_review_ms, cost_micro_usd, decisions_json, escalated, reviewers_json, commits, \
     additions, deletions, pr_ref, card_id, created_at, updated_at";

fn outcome_from_row(r: &AnyRow) -> Result<Outcome> {
    let reviewers: String = r.try_get("reviewers_json")?;
    Ok(Outcome {
        outcome_key: r.try_get("outcome_key")?,
        kind: r.try_get("kind")?,
        title: r.try_get("title")?,
        repo: r.try_get("repo")?,
        trigger_kind: r.try_get("trigger_kind")?,
        trigger_ref: r.try_get("trigger_ref")?,
        author: r.try_get("author")?,
        opened_at: r.try_get("opened_at")?,
        started_at: r.try_get("started_at")?,
        resolved_at: r.try_get("resolved_at")?,
        result: r.try_get("result")?,
        role_key: r.try_get("role_key")?,
        active_ms: r.try_get("active_ms")?,
        wait_ms: r.try_get("wait_ms")?,
        wall_ms: r.try_get("wall_ms")?,
        cycle_ms: r.try_get("cycle_ms")?,
        first_review_ms: r.try_get("first_review_ms")?,
        cost_micro_usd: r.try_get("cost_micro_usd")?,
        decisions_json: r.try_get("decisions_json")?,
        escalated: r.try_get("escalated")?,
        reviewers: json_vec(&reviewers),
        commits: r.try_get("commits")?,
        additions: r.try_get("additions")?,
        deletions: r.try_get("deletions")?,
        pr_ref: r.try_get("pr_ref")?,
        card_id: r.try_get("card_id")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const ACTION_COLS: &str = "source, ref_id, action_kind, actor_key, actor_login, is_bot, bot_kind, \
     outcome_key, repo, occurred_at, raw_units_json, basis";

fn action_from_row(r: &AnyRow) -> Result<HumanAction> {
    Ok(HumanAction {
        source: r.try_get("source")?,
        ref_id: r.try_get("ref_id")?,
        action_kind: r.try_get("action_kind")?,
        actor_key: r.try_get("actor_key")?,
        actor_login: r.try_get("actor_login")?,
        is_bot: get_bool(r, "is_bot")?,
        bot_kind: r.try_get("bot_kind")?,
        outcome_key: r.try_get("outcome_key")?,
        repo: r.try_get("repo")?,
        occurred_at: r.try_get("occurred_at")?,
        raw_units_json: r.try_get("raw_units_json")?,
        basis: r.try_get("basis")?,
    })
}

const PERSON_COLS: &str = "person_key, login, display_name, avatar_url, emails_json, is_internal, \
     association, is_bot, bot_kind, first_seen_at, last_seen_at";

fn person_from_row(r: &AnyRow) -> Result<Person> {
    let emails: String = r.try_get("emails_json")?;
    Ok(Person {
        person_key: r.try_get("person_key")?,
        login: r.try_get("login")?,
        display_name: r.try_get("display_name")?,
        avatar_url: r.try_get("avatar_url")?,
        emails: json_vec(&emails),
        is_internal: get_opt_bool(r, "is_internal")?,
        association: r.try_get("association")?,
        is_bot: get_bool(r, "is_bot")?,
        bot_kind: r.try_get("bot_kind")?,
        first_seen_at: r.try_get("first_seen_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
    })
}

const COMMIT_COLS: &str = "repo, sha, author_name, author_email, author_login, author_key, \
     is_bot, trailers_json, signed, pr_number, outcome_key, committed_at";

fn commit_from_row(r: &AnyRow) -> Result<DeliveryCommit> {
    let trailers: String = r.try_get("trailers_json")?;
    Ok(DeliveryCommit {
        repo: r.try_get("repo")?,
        sha: r.try_get("sha")?,
        author_name: r.try_get("author_name")?,
        author_email: r.try_get("author_email")?,
        author_login: r.try_get("author_login")?,
        author_key: r.try_get("author_key")?,
        is_bot: get_bool(r, "is_bot")?,
        trailers: json_vec(&trailers),
        signed: get_opt_bool(r, "signed")?,
        pr_number: r.try_get("pr_number")?,
        outcome_key: r.try_get("outcome_key")?,
        committed_at: r.try_get("committed_at")?,
    })
}

const DECISION_COLS: &str = "day_start, kind, model, total, proceeded, rule_handled, escalated, \
     conf_buckets_json, latency_sum_ms, latency_n";

fn decision_from_row(r: &AnyRow) -> Result<DecisionDay> {
    let buckets: String = r.try_get("conf_buckets_json")?;
    Ok(DecisionDay {
        day_start: r.try_get("day_start")?,
        kind: r.try_get("kind")?,
        model: r.try_get("model")?,
        total: r.try_get("total")?,
        proceeded: r.try_get("proceeded")?,
        rule_handled: r.try_get("rule_handled")?,
        escalated: r.try_get("escalated")?,
        conf_buckets: serde_json::from_str(&buckets).unwrap_or_else(|_| vec![0; 10]),
        latency_sum_ms: r.try_get("latency_sum_ms")?,
        latency_n: r.try_get("latency_n")?,
    })
}

impl Store {
    // ---- orgs --------------------------------------------------------------

    /// Inserts or refreshes an org; `managed` and `enabled` are kept.
    pub async fn gh_upsert_org(&self, o: &GhOrg) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_orgs (login, kind, managed, enabled, repo_count, avatar_url, description, synced_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (login) DO UPDATE SET kind = excluded.kind, \
             repo_count = excluded.repo_count, avatar_url = excluded.avatar_url, \
             description = excluded.description, synced_at = excluded.synced_at",
        ))
        .bind(&o.login)
        .bind(&o.kind)
        .bind(b(o.managed))
        .bind(b(o.enabled))
        .bind(o.repo_count)
        .bind(&o.avatar_url)
        .bind(&o.description)
        .bind(o.synced_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn gh_orgs(&self) -> Result<Vec<GhOrg>> {
        let q = format!("SELECT {ORG_COLS} FROM gh_orgs ORDER BY login");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(org_from_row).collect()
    }

    pub async fn gh_org(&self, login: &str) -> Result<Option<GhOrg>> {
        let q = format!("SELECT {ORG_COLS} FROM gh_orgs WHERE login = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(login)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(org_from_row).transpose()
    }

    pub async fn gh_set_org_managed(&self, login: &str, managed: bool) -> Result<bool> {
        let r = sqlx::query(self.sql("UPDATE gh_orgs SET managed = ? WHERE login = ?"))
            .bind(b(managed))
            .bind(login)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn gh_set_org_enabled(&self, login: &str, enabled: bool) -> Result<bool> {
        let r = sqlx::query(self.sql("UPDATE gh_orgs SET enabled = ? WHERE login = ?"))
            .bind(b(enabled))
            .bind(login)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    // ---- repos -------------------------------------------------------------

    /// Inserts or refreshes the GitHub facts of a repository; the switches
    /// (managed, mirroring) and settings are kept on update.
    pub async fn gh_upsert_repo(&self, r: &GhRepo) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_repos (full_name, owner, name, visibility, default_branch, archived, fork, \
             html_url, description, can_push, managed, mirror_pulls, mirror_issues, sync_labels, \
             settings_json, revision, pushed_at, synced_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (full_name) DO UPDATE SET owner = excluded.owner, name = excluded.name, \
             visibility = excluded.visibility, default_branch = excluded.default_branch, \
             archived = excluded.archived, fork = excluded.fork, html_url = excluded.html_url, \
             description = excluded.description, can_push = excluded.can_push, \
             pushed_at = excluded.pushed_at, synced_at = excluded.synced_at",
        ))
        .bind(&r.full_name)
        .bind(&r.owner)
        .bind(&r.name)
        .bind(&r.visibility)
        .bind(&r.default_branch)
        .bind(b(r.archived))
        .bind(b(r.fork))
        .bind(&r.html_url)
        .bind(&r.description)
        .bind(b(r.can_push))
        .bind(b(r.managed))
        .bind(b(r.mirror_pulls))
        .bind(b(r.mirror_issues))
        .bind(b(r.sync_labels))
        .bind(serde_json::to_string(&r.settings)?)
        .bind(r.revision)
        .bind(r.pushed_at)
        .bind(r.synced_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn gh_repo(&self, full_name: &str) -> Result<Option<GhRepo>> {
        let q = format!("SELECT {REPO_COLS} FROM gh_repos WHERE full_name = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(full_name)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(repo_from_row).transpose()
    }

    /// Every repository, optionally of one owner, by name.
    pub async fn gh_repos(&self, owner: Option<&str>) -> Result<Vec<GhRepo>> {
        let rows = match owner {
            Some(o) => {
                let q =
                    format!("SELECT {REPO_COLS} FROM gh_repos WHERE owner = ? ORDER BY full_name");
                sqlx::query(self.sql(&q))
                    .bind(o)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let q = format!("SELECT {REPO_COLS} FROM gh_repos ORDER BY full_name");
                sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?
            }
        };
        rows.iter().map(repo_from_row).collect()
    }

    /// Managed repositories (the poller's scope).
    pub async fn gh_managed_repos(&self) -> Result<Vec<GhRepo>> {
        let q = format!(
            "SELECT {REPO_COLS} FROM gh_repos WHERE managed = 1 AND archived = 0 ORDER BY full_name"
        );
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(repo_from_row).collect()
    }

    pub async fn gh_set_repo_managed(&self, full_name: &str, managed: bool) -> Result<bool> {
        let r = sqlx::query(self.sql("UPDATE gh_repos SET managed = ? WHERE full_name = ?"))
            .bind(b(managed))
            .bind(full_name)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn gh_set_repo_mirroring(&self, full_name: &str, m: Mirroring) -> Result<bool> {
        let r = sqlx::query(self.sql(
            "UPDATE gh_repos SET mirror_pulls = ?, mirror_issues = ?, sync_labels = ? WHERE full_name = ?",
        ))
        .bind(b(m.pulls))
        .bind(b(m.issues))
        .bind(b(m.labels))
        .bind(full_name)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Writes settings if `revision` is the stored one; returns the new
    /// revision, or `None` on a revision conflict (or unknown repository).
    pub async fn gh_set_repo_settings(
        &self,
        full_name: &str,
        settings: &RepoSettings,
        revision: i64,
    ) -> Result<Option<i64>> {
        if !AUTONOMY_LEVELS.contains(&settings.autonomy.as_str()) {
            bail!("unknown autonomy {:?}", settings.autonomy);
        }
        let r = sqlx::query(self.sql(
            "UPDATE gh_repos SET settings_json = ?, revision = revision + 1 \
             WHERE full_name = ? AND revision = ?",
        ))
        .bind(serde_json::to_string(settings)?)
        .bind(full_name)
        .bind(revision)
        .execute(&self.pool)
        .await?;
        Ok((r.rows_affected() == 1).then_some(revision + 1))
    }

    // ---- pulls -------------------------------------------------------------

    /// Upserts a pull; keeps `delivery_captured`. Reports whether anything a
    /// screen shows changed (`synced_at` alone does not count).
    pub async fn gh_upsert_pull(&self, p: &GhPull) -> Result<Upsert> {
        let old = self.gh_pull(&p.repo, p.number).await?;
        if let Some(o) = &old {
            let mut cmp = p.clone();
            cmp.synced_at = o.synced_at;
            cmp.delivery_captured = o.delivery_captured;
            if &cmp == o {
                return Ok(Upsert::Unchanged);
            }
        }
        sqlx::query(self.sql(
            "INSERT INTO gh_pulls (repo, number, title, state, draft, merged, author, head_ref, head_sha, \
             head_repo, base_ref, base_sha, mergeable, mergeable_state, review_decision, \
             requested_reviewers_json, labels_json, checks_state, checks_json, html_url, additions, \
             deletions, changed_files, created_at, updated_at, merged_at, closed_at, synced_at, \
             delivery_captured) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (repo, number) DO UPDATE SET title = excluded.title, state = excluded.state, \
             draft = excluded.draft, merged = excluded.merged, author = excluded.author, \
             head_ref = excluded.head_ref, head_sha = excluded.head_sha, head_repo = excluded.head_repo, \
             base_ref = excluded.base_ref, base_sha = excluded.base_sha, mergeable = excluded.mergeable, \
             mergeable_state = excluded.mergeable_state, review_decision = excluded.review_decision, \
             requested_reviewers_json = excluded.requested_reviewers_json, \
             labels_json = excluded.labels_json, checks_state = excluded.checks_state, \
             checks_json = excluded.checks_json, html_url = excluded.html_url, \
             additions = excluded.additions, deletions = excluded.deletions, \
             changed_files = excluded.changed_files, created_at = excluded.created_at, \
             updated_at = excluded.updated_at, merged_at = excluded.merged_at, \
             closed_at = excluded.closed_at, synced_at = excluded.synced_at",
        ))
        .bind(&p.repo)
        .bind(p.number)
        .bind(&p.title)
        .bind(&p.state)
        .bind(b(p.draft))
        .bind(b(p.merged))
        .bind(&p.author)
        .bind(&p.head_ref)
        .bind(&p.head_sha)
        .bind(&p.head_repo)
        .bind(&p.base_ref)
        .bind(&p.base_sha)
        .bind(&p.mergeable)
        .bind(&p.mergeable_state)
        .bind(&p.review_decision)
        .bind(serde_json::to_string(&p.requested_reviewers)?)
        .bind(serde_json::to_string(&p.labels)?)
        .bind(&p.checks_state)
        .bind(serde_json::to_string(&p.checks)?)
        .bind(&p.html_url)
        .bind(p.additions)
        .bind(p.deletions)
        .bind(p.changed_files)
        .bind(p.created_at)
        .bind(p.updated_at)
        .bind(p.merged_at)
        .bind(p.closed_at)
        .bind(p.synced_at)
        .bind(b(p.delivery_captured))
        .execute(&self.pool)
        .await?;
        Ok(if old.is_some() {
            Upsert::Updated
        } else {
            Upsert::Inserted
        })
    }

    pub async fn gh_pull(&self, repo: &str, number: i64) -> Result<Option<GhPull>> {
        let q = format!("SELECT {PULL_COLS} FROM gh_pulls WHERE repo = ? AND number = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(repo)
            .bind(number)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(pull_from_row).transpose()
    }

    /// Pulls of one repository (or all) in one state (or all), newest first.
    pub async fn gh_pulls(&self, repo: Option<&str>, state: Option<&str>) -> Result<Vec<GhPull>> {
        let q = format!(
            "SELECT {PULL_COLS} FROM gh_pulls WHERE (? = '' OR repo = ?) AND (? = '' OR state = ?) \
             ORDER BY updated_at DESC"
        );
        let repo = repo.unwrap_or("");
        let state = state.unwrap_or("");
        let rows = sqlx::query(self.sql(&q))
            .bind(repo)
            .bind(repo)
            .bind(state)
            .bind(state)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(pull_from_row).collect()
    }

    /// Open pulls GitHub reports as conflicting.
    pub async fn gh_conflicting_pulls(&self) -> Result<Vec<GhPull>> {
        let q = format!(
            "SELECT {PULL_COLS} FROM gh_pulls WHERE state = 'open' AND mergeable = 'conflict' \
             ORDER BY updated_at DESC"
        );
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(pull_from_row).collect()
    }

    pub async fn gh_set_pull_draft(&self, repo: &str, number: i64, draft: bool) -> Result<()> {
        sqlx::query(self.sql("UPDATE gh_pulls SET draft = ? WHERE repo = ? AND number = ?"))
            .bind(b(draft))
            .bind(repo)
            .bind(number)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn gh_mark_pull_captured(&self, repo: &str, number: i64) -> Result<()> {
        sqlx::query(
            self.sql("UPDATE gh_pulls SET delivery_captured = 1 WHERE repo = ? AND number = ?"),
        )
        .bind(repo)
        .bind(number)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- issues ------------------------------------------------------------

    /// Upserts an issue; keeps `card_id`. Reports what changed.
    pub async fn gh_upsert_issue(&self, i: &GhIssue) -> Result<Upsert> {
        let old = self.gh_issue(&i.repo, i.number).await?;
        if let Some(o) = &old {
            let mut cmp = i.clone();
            cmp.synced_at = o.synced_at;
            cmp.card_id = o.card_id.clone();
            if &cmp == o {
                return Ok(Upsert::Unchanged);
            }
        }
        sqlx::query(self.sql(
            "INSERT INTO gh_issues (repo, number, title, state, state_reason, author, author_association, \
             labels_json, assignees_json, body, html_url, comments, created_at, updated_at, closed_at, \
             card_id, synced_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (repo, number) DO UPDATE SET title = excluded.title, state = excluded.state, \
             state_reason = excluded.state_reason, author = excluded.author, \
             author_association = excluded.author_association, labels_json = excluded.labels_json, \
             assignees_json = excluded.assignees_json, body = excluded.body, \
             html_url = excluded.html_url, comments = excluded.comments, \
             created_at = excluded.created_at, updated_at = excluded.updated_at, \
             closed_at = excluded.closed_at, synced_at = excluded.synced_at",
        ))
        .bind(&i.repo)
        .bind(i.number)
        .bind(&i.title)
        .bind(&i.state)
        .bind(&i.state_reason)
        .bind(&i.author)
        .bind(&i.author_association)
        .bind(serde_json::to_string(&i.labels)?)
        .bind(serde_json::to_string(&i.assignees)?)
        .bind(&i.body)
        .bind(&i.html_url)
        .bind(i.comments)
        .bind(i.created_at)
        .bind(i.updated_at)
        .bind(i.closed_at)
        .bind(&i.card_id)
        .bind(i.synced_at)
        .execute(&self.pool)
        .await?;
        Ok(if old.is_some() {
            Upsert::Updated
        } else {
            Upsert::Inserted
        })
    }

    pub async fn gh_issue(&self, repo: &str, number: i64) -> Result<Option<GhIssue>> {
        let q = format!("SELECT {ISSUE_COLS} FROM gh_issues WHERE repo = ? AND number = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(repo)
            .bind(number)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(issue_from_row).transpose()
    }

    pub async fn gh_issues(&self, repo: Option<&str>, state: Option<&str>) -> Result<Vec<GhIssue>> {
        let q = format!(
            "SELECT {ISSUE_COLS} FROM gh_issues WHERE (? = '' OR repo = ?) AND (? = '' OR state = ?) \
             ORDER BY updated_at DESC"
        );
        let repo = repo.unwrap_or("");
        let state = state.unwrap_or("");
        let rows = sqlx::query(self.sql(&q))
            .bind(repo)
            .bind(repo)
            .bind(state)
            .bind(state)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(issue_from_row).collect()
    }

    /// Newest `updated_at` mirrored for a repository (the `since` cursor).
    pub async fn gh_issues_cursor(&self, repo: &str) -> Result<Option<i64>> {
        let row =
            sqlx::query(self.sql("SELECT MAX(updated_at) AS m FROM gh_issues WHERE repo = ?"))
                .bind(repo)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.try_get::<Option<i64>, _>("m")?)
    }

    pub async fn gh_link_issue_card(&self, repo: &str, number: i64, card_id: &str) -> Result<()> {
        sqlx::query(self.sql("UPDATE gh_issues SET card_id = ? WHERE repo = ? AND number = ?"))
            .bind(card_id)
            .bind(repo)
            .bind(number)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- labels ------------------------------------------------------------

    /// Replaces the stored label set of a repository.
    pub async fn gh_set_labels(&self, repo: &str, labels: &[GhLabel]) -> Result<()> {
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM gh_labels WHERE repo = ?"))
            .bind(repo)
            .execute(&mut *tx)
            .await?;
        let now = now_ms();
        for l in labels {
            sqlx::query(self.sql(
                "INSERT INTO gh_labels (repo, name, color, description, synced_at) VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (repo, name) DO NOTHING",
            ))
            .bind(repo)
            .bind(&l.name)
            .bind(&l.color)
            .bind(&l.description)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn gh_labels(&self, repo: &str) -> Result<Vec<GhLabel>> {
        let rows = sqlx::query(self.sql(
            "SELECT repo, name, color, description FROM gh_labels WHERE repo = ? ORDER BY name",
        ))
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(GhLabel {
                    repo: r.try_get("repo")?,
                    name: r.try_get("name")?,
                    color: r.try_get("color")?,
                    description: r.try_get("description")?,
                })
            })
            .collect()
    }

    // ---- etag cache --------------------------------------------------------

    pub async fn gh_etag(&self, key: &str) -> Result<Option<EtagEntry>> {
        let row = sqlx::query(
            self.sql("SELECT etag, link, body, fetched_at FROM gh_etags WHERE cache_key = ?"),
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(EtagEntry {
                etag: r.try_get("etag")?,
                link: r.try_get("link")?,
                body: r.try_get("body")?,
                fetched_at: r.try_get("fetched_at")?,
            })
        })
        .transpose()
    }

    pub async fn gh_put_etag(&self, key: &str, etag: &str, link: &str, body: &str) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_etags (cache_key, etag, link, body, fetched_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (cache_key) DO UPDATE SET etag = excluded.etag, link = excluded.link, \
             body = excluded.body, fetched_at = excluded.fetched_at",
        ))
        .bind(key)
        .bind(etag)
        .bind(link)
        .bind(body)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drops cached entries whose key starts with `prefix` (after a write).
    pub async fn gh_drop_etags(&self, prefix: &str) -> Result<u64> {
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let r = sqlx::query(self.sql("DELETE FROM gh_etags WHERE cache_key LIKE ? ESCAPE '\\'"))
            .bind(pattern)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ---- rate budget -------------------------------------------------------

    /// Records a rate-limit observation; `secondary_until` is kept.
    pub async fn gh_put_quota(&self, q: &Quota) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_quota (route, resource, rate_limit, remaining, used, reset_at, observed_at, secondary_until) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (route, resource) DO UPDATE SET \
             rate_limit = excluded.rate_limit, remaining = excluded.remaining, used = excluded.used, \
             reset_at = excluded.reset_at, observed_at = excluded.observed_at",
        ))
        .bind(&q.route)
        .bind(&q.resource)
        .bind(q.limit)
        .bind(q.remaining)
        .bind(q.used)
        .bind(q.reset_at)
        .bind(q.observed_at)
        .bind(q.secondary_until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Refuses non-user calls until `until` (a secondary limit or a 429).
    pub async fn gh_set_secondary_until(
        &self,
        route: &str,
        resource: &str,
        until: i64,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_quota (route, resource, secondary_until, observed_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (route, resource) DO UPDATE SET secondary_until = excluded.secondary_until",
        ))
        .bind(route)
        .bind(resource)
        .bind(until)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn gh_quota(&self, route: &str, resource: &str) -> Result<Option<Quota>> {
        let q = format!("SELECT {QUOTA_COLS} FROM gh_quota WHERE route = ? AND resource = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(route)
            .bind(resource)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(quota_from_row).transpose()
    }

    pub async fn gh_quotas(&self) -> Result<Vec<Quota>> {
        let q = format!("SELECT {QUOTA_COLS} FROM gh_quota ORDER BY route, resource");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(quota_from_row).collect()
    }

    /// Counts one call (`not_modified` when GitHub answered 304).
    pub async fn gh_count_call(
        &self,
        route: &str,
        caller: &str,
        at: i64,
        not_modified: bool,
    ) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_calls (route, caller, hour_bucket, calls, not_modified) VALUES (?, ?, ?, 1, ?) \
             ON CONFLICT (route, caller, hour_bucket) DO UPDATE SET calls = gh_calls.calls + 1, \
             not_modified = gh_calls.not_modified + excluded.not_modified",
        ))
        .bind(route)
        .bind(caller)
        .bind(hour_bucket(at))
        .bind(b(not_modified))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Calls per caller in the UTC hour holding `at`.
    pub async fn gh_calls_in_hour(&self, route: &str, at: i64) -> Result<Vec<CallCount>> {
        let rows = sqlx::query(self.sql(
            "SELECT caller, calls, not_modified FROM gh_calls WHERE route = ? AND hour_bucket = ? ORDER BY caller",
        ))
        .bind(route)
        .bind(hour_bucket(at))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(CallCount {
                    caller: r.try_get("caller")?,
                    calls: r.try_get("calls")?,
                    not_modified: r.try_get("not_modified")?,
                })
            })
            .collect()
    }

    // ---- conflict resolutions ---------------------------------------------

    pub async fn gh_insert_resolution(&self, r: &Resolution) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO gh_resolutions (id, repo, number, branch, commit_sha, parent_shas_json, files_json, \
             signing, verified, verification_reason, route, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&r.id)
        .bind(&r.repo)
        .bind(r.number)
        .bind(&r.branch)
        .bind(&r.commit_sha)
        .bind(serde_json::to_string(&r.parent_shas)?)
        .bind(serde_json::to_string(&r.files)?)
        .bind(&r.signing)
        .bind(r.verified.map(b))
        .bind(&r.verification_reason)
        .bind(&r.route)
        .bind(r.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn gh_resolutions(&self, repo: &str, number: i64) -> Result<Vec<Resolution>> {
        let q = format!(
            "SELECT {RESOLUTION_COLS} FROM gh_resolutions WHERE repo = ? AND number = ? ORDER BY created_at DESC"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(repo)
            .bind(number)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(resolution_from_row).collect()
    }

    // ---- delivery facts ----------------------------------------------------

    /// Inserts or replaces an outcome (keeps `created_at`).
    pub async fn upsert_outcome(&self, o: &Outcome) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO delivery_outcomes (outcome_key, kind, title, repo, trigger_kind, trigger_ref, author, \
             opened_at, started_at, resolved_at, result, role_key, active_ms, wait_ms, wall_ms, cycle_ms, \
             first_review_ms, cost_micro_usd, decisions_json, escalated, reviewers_json, commits, \
             additions, deletions, pr_ref, card_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (outcome_key) DO UPDATE SET kind = excluded.kind, title = excluded.title, \
             repo = excluded.repo, trigger_kind = excluded.trigger_kind, trigger_ref = excluded.trigger_ref, \
             author = excluded.author, opened_at = excluded.opened_at, started_at = excluded.started_at, \
             resolved_at = excluded.resolved_at, result = excluded.result, role_key = excluded.role_key, \
             active_ms = excluded.active_ms, wait_ms = excluded.wait_ms, wall_ms = excluded.wall_ms, \
             cycle_ms = excluded.cycle_ms, first_review_ms = excluded.first_review_ms, \
             cost_micro_usd = excluded.cost_micro_usd, decisions_json = excluded.decisions_json, \
             escalated = excluded.escalated, reviewers_json = excluded.reviewers_json, \
             commits = excluded.commits, additions = excluded.additions, deletions = excluded.deletions, \
             pr_ref = excluded.pr_ref, card_id = excluded.card_id, updated_at = excluded.updated_at",
        ))
        .bind(&o.outcome_key)
        .bind(&o.kind)
        .bind(&o.title)
        .bind(&o.repo)
        .bind(&o.trigger_kind)
        .bind(&o.trigger_ref)
        .bind(&o.author)
        .bind(o.opened_at)
        .bind(o.started_at)
        .bind(o.resolved_at)
        .bind(&o.result)
        .bind(&o.role_key)
        .bind(o.active_ms)
        .bind(o.wait_ms)
        .bind(o.wall_ms)
        .bind(o.cycle_ms)
        .bind(o.first_review_ms)
        .bind(o.cost_micro_usd)
        .bind(&o.decisions_json)
        .bind(o.escalated)
        .bind(serde_json::to_string(&o.reviewers)?)
        .bind(o.commits)
        .bind(o.additions)
        .bind(o.deletions)
        .bind(&o.pr_ref)
        .bind(&o.card_id)
        .bind(if o.created_at == 0 { now } else { o.created_at })
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn outcome(&self, key: &str) -> Result<Option<Outcome>> {
        let q = format!("SELECT {OUTCOME_COLS} FROM delivery_outcomes WHERE outcome_key = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(outcome_from_row).transpose()
    }

    /// Outcomes resolved in `[from, to)`, newest first.
    pub async fn outcomes_between(&self, from: i64, to: i64) -> Result<Vec<Outcome>> {
        let q = format!(
            "SELECT {OUTCOME_COLS} FROM delivery_outcomes WHERE resolved_at >= ? AND resolved_at < ? \
             ORDER BY resolved_at DESC, outcome_key DESC"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(outcome_from_row).collect()
    }

    /// Inserts an action once (`(source, ref_id)` is its identity); a
    /// repeated capture changes nothing. Returns whether it was new.
    pub async fn upsert_action(&self, a: &HumanAction) -> Result<bool> {
        let r = sqlx::query(self.sql(
            "INSERT INTO delivery_actions (source, ref_id, action_kind, actor_key, actor_login, is_bot, bot_kind, \
             outcome_key, repo, occurred_at, raw_units_json, basis) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (source, ref_id) DO NOTHING",
        ))
        .bind(&a.source)
        .bind(&a.ref_id)
        .bind(&a.action_kind)
        .bind(&a.actor_key)
        .bind(&a.actor_login)
        .bind(b(a.is_bot))
        .bind(&a.bot_kind)
        .bind(&a.outcome_key)
        .bind(&a.repo)
        .bind(a.occurred_at)
        .bind(&a.raw_units_json)
        .bind(&a.basis)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Actions in `[from, to)`, oldest first.
    pub async fn actions_between(&self, from: i64, to: i64) -> Result<Vec<HumanAction>> {
        let q = format!(
            "SELECT {ACTION_COLS} FROM delivery_actions WHERE occurred_at >= ? AND occurred_at < ? \
             ORDER BY occurred_at, source, ref_id"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(action_from_row).collect()
    }

    /// Inserts a person or merges what is known: names fill in, emails
    /// union, seen times widen.
    pub async fn upsert_person(&self, p: &Person) -> Result<()> {
        let old = self.person(&p.person_key).await?;
        let merged = match old {
            None => p.clone(),
            Some(o) => {
                let mut emails = o.emails.clone();
                for e in &p.emails {
                    if !emails.contains(e) {
                        emails.push(e.clone());
                    }
                }
                let pick = |a: &str, b: &str| {
                    if a.is_empty() {
                        b.to_string()
                    } else {
                        a.to_string()
                    }
                };
                Person {
                    person_key: o.person_key.clone(),
                    login: pick(&p.login, &o.login),
                    display_name: pick(&p.display_name, &o.display_name),
                    avatar_url: pick(&p.avatar_url, &o.avatar_url),
                    emails,
                    is_internal: p.is_internal.or(o.is_internal),
                    association: pick(&p.association, &o.association),
                    is_bot: p.is_bot || o.is_bot,
                    bot_kind: pick(&p.bot_kind, &o.bot_kind),
                    first_seen_at: match (o.first_seen_at, p.first_seen_at) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    },
                    last_seen_at: match (o.last_seen_at, p.last_seen_at) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    },
                }
            }
        };
        sqlx::query(self.sql(
            "INSERT INTO delivery_people (person_key, login, display_name, avatar_url, emails_json, is_internal, \
             association, is_bot, bot_kind, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (person_key) DO UPDATE SET login = excluded.login, display_name = excluded.display_name, \
             avatar_url = excluded.avatar_url, emails_json = excluded.emails_json, \
             is_internal = excluded.is_internal, association = excluded.association, \
             is_bot = excluded.is_bot, bot_kind = excluded.bot_kind, \
             first_seen_at = excluded.first_seen_at, last_seen_at = excluded.last_seen_at",
        ))
        .bind(&merged.person_key)
        .bind(&merged.login)
        .bind(&merged.display_name)
        .bind(&merged.avatar_url)
        .bind(serde_json::to_string(&merged.emails)?)
        .bind(merged.is_internal.map(b))
        .bind(&merged.association)
        .bind(b(merged.is_bot))
        .bind(&merged.bot_kind)
        .bind(merged.first_seen_at)
        .bind(merged.last_seen_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn person(&self, key: &str) -> Result<Option<Person>> {
        let q = format!("SELECT {PERSON_COLS} FROM delivery_people WHERE person_key = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(person_from_row).transpose()
    }

    pub async fn people(&self) -> Result<Vec<Person>> {
        let q = format!("SELECT {PERSON_COLS} FROM delivery_people ORDER BY person_key");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(person_from_row).collect()
    }

    pub async fn upsert_commit(&self, c: &DeliveryCommit) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO delivery_commits (repo, sha, author_name, author_email, author_login, author_key, is_bot, \
             trailers_json, signed, pr_number, outcome_key, committed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (repo, sha) DO UPDATE SET signed = excluded.signed, pr_number = excluded.pr_number, \
             outcome_key = excluded.outcome_key",
        ))
        .bind(&c.repo)
        .bind(&c.sha)
        .bind(&c.author_name)
        .bind(&c.author_email)
        .bind(&c.author_login)
        .bind(&c.author_key)
        .bind(b(c.is_bot))
        .bind(serde_json::to_string(&c.trailers)?)
        .bind(c.signed.map(b))
        .bind(c.pr_number)
        .bind(&c.outcome_key)
        .bind(c.committed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Commits committed in `[from, to)`, oldest first.
    pub async fn commits_between(&self, from: i64, to: i64) -> Result<Vec<DeliveryCommit>> {
        let q = format!(
            "SELECT {COMMIT_COLS} FROM delivery_commits WHERE committed_at >= ? AND committed_at < ? \
             ORDER BY committed_at, sha"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(commit_from_row).collect()
    }

    /// Adds one decision to its day's rollup. `confidence` is 0..=1.
    pub async fn record_decision(
        &self,
        at: i64,
        kind: &str,
        model: &str,
        end: DecisionEnd,
        confidence: Option<f64>,
        latency_ms: Option<i64>,
    ) -> Result<()> {
        let day = day_start(at);
        let mut tx = self.begin_write().await?;
        let row = sqlx::query(self.sql(
            "SELECT conf_buckets_json FROM delivery_decision_days WHERE day_start = ? AND kind = ? AND model = ?",
        ))
        .bind(day)
        .bind(kind)
        .bind(model)
        .fetch_optional(&mut *tx)
        .await?;
        let mut buckets: Vec<i64> = row
            .map(|r| r.get::<String, _>("conf_buckets_json"))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| vec![0; 10]);
        buckets.resize(10, 0);
        if let Some(c) = confidence {
            let i = ((c.clamp(0.0, 1.0) * 10.0).floor() as usize).min(9);
            buckets[i] += 1;
        }
        let (p, rh, e) = match end {
            DecisionEnd::Proceeded => (1, 0, 0),
            DecisionEnd::RuleHandled => (0, 1, 0),
            DecisionEnd::Escalated => (0, 0, 1),
        };
        sqlx::query(self.sql(
            "INSERT INTO delivery_decision_days (day_start, kind, model, total, proceeded, rule_handled, escalated, \
             conf_buckets_json, latency_sum_ms, latency_n) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (day_start, kind, model) DO UPDATE SET total = delivery_decision_days.total + 1, \
             proceeded = delivery_decision_days.proceeded + excluded.proceeded, \
             rule_handled = delivery_decision_days.rule_handled + excluded.rule_handled, \
             escalated = delivery_decision_days.escalated + excluded.escalated, \
             conf_buckets_json = excluded.conf_buckets_json, \
             latency_sum_ms = delivery_decision_days.latency_sum_ms + excluded.latency_sum_ms, \
             latency_n = delivery_decision_days.latency_n + excluded.latency_n",
        ))
        .bind(day)
        .bind(kind)
        .bind(model)
        .bind(p)
        .bind(rh)
        .bind(e)
        .bind(serde_json::to_string(&buckets)?)
        .bind(latency_ms.unwrap_or(0))
        .bind(latency_ms.map(|_| 1i64).unwrap_or(0))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Decision rollups for days starting in `[from, to)`.
    pub async fn decision_days(&self, from: i64, to: i64) -> Result<Vec<DecisionDay>> {
        let q = format!(
            "SELECT {DECISION_COLS} FROM delivery_decision_days WHERE day_start >= ? AND day_start < ? \
             ORDER BY day_start, kind, model"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(day_start(from))
            .bind(to)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(decision_from_row).collect()
    }
}

// ---- forge connections (migration 0009) -----------------------------------

/// The id of the github.com connection the migration creates.
pub const GITHUB_COM: &str = "github.com";

/// One signed-in place Keleustes works on (`forge_connections`). No secret.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ForgeConnectionRow {
    pub id: String,
    /// github, ghes, gitlab, gitea, forgejo.
    pub kind: String,
    pub host: String,
    pub label: String,
    pub login: String,
    /// The tea login name ("" for gh and glab).
    pub cli_name: String,
    pub git_protocol: String,
    /// "auto", "local", "node:<id>".
    pub route_pref: String,
    pub enabled: bool,
    pub status_json: String,
    /// Who added it ("migration", "user", "cli").
    pub origin: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Where a repository key lives: its connection, the forge's own
/// `owner/name` ("" means the key itself) and GitLab's project id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoForge {
    pub connection: String,
    pub forge_full_name: String,
    pub forge_id: Option<i64>,
}

const CONN_COLS: &str = "id, kind, host, label, login, cli_name, git_protocol, route_pref, \
     enabled, status_json, origin, created_at, updated_at";

fn conn_from_row(r: &AnyRow) -> Result<ForgeConnectionRow> {
    Ok(ForgeConnectionRow {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        host: r.try_get("host")?,
        label: r.try_get("label")?,
        login: r.try_get("login")?,
        cli_name: r.try_get("cli_name")?,
        git_protocol: r.try_get("git_protocol")?,
        route_pref: r.try_get("route_pref")?,
        enabled: get_bool(r, "enabled")?,
        status_json: r.try_get("status_json")?,
        origin: r.try_get("origin")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

impl Store {
    /// Every connection, github.com first, then by host.
    pub async fn forge_connections(&self) -> Result<Vec<ForgeConnectionRow>> {
        let q = format!(
            "SELECT {CONN_COLS} FROM forge_connections \
             ORDER BY CASE WHEN id = 'github.com' THEN 0 ELSE 1 END, host, kind"
        );
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(conn_from_row).collect()
    }

    pub async fn forge_connection(&self, id: &str) -> Result<Option<ForgeConnectionRow>> {
        let q = format!("SELECT {CONN_COLS} FROM forge_connections WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(conn_from_row).transpose()
    }

    /// The connection of `kind` on `host`, if one exists.
    pub async fn forge_connection_at(
        &self,
        kind: &str,
        host: &str,
    ) -> Result<Option<ForgeConnectionRow>> {
        let q = format!("SELECT {CONN_COLS} FROM forge_connections WHERE kind = ? AND host = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(kind)
            .bind(host)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(conn_from_row).transpose()
    }

    /// Inserts or replaces a connection by id (`created_at` is kept).
    pub async fn upsert_forge_connection(&self, c: &ForgeConnectionRow) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO forge_connections (id, kind, host, label, login, cli_name, git_protocol, \
             route_pref, enabled, status_json, origin, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET \
             kind = excluded.kind, host = excluded.host, label = excluded.label, \
             login = excluded.login, cli_name = excluded.cli_name, \
             git_protocol = excluded.git_protocol, route_pref = excluded.route_pref, \
             enabled = excluded.enabled, status_json = excluded.status_json, \
             updated_at = excluded.updated_at",
        ))
        .bind(&c.id)
        .bind(&c.kind)
        .bind(&c.host)
        .bind(&c.label)
        .bind(&c.login)
        .bind(&c.cli_name)
        .bind(&c.git_protocol)
        .bind(&c.route_pref)
        .bind(b(c.enabled))
        .bind(&c.status_json)
        .bind(&c.origin)
        .bind(c.created_at)
        .bind(c.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the account last seen and the last status (JSON).
    pub async fn forge_set_status(&self, id: &str, login: &str, status_json: &str) -> Result<bool> {
        let r = sqlx::query(self.sql(
            "UPDATE forge_connections SET login = CASE WHEN ? = '' THEN login ELSE ? END, \
             status_json = ?, updated_at = ? WHERE id = ?",
        ))
        .bind(login)
        .bind(login)
        .bind(status_json)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Records what the server said about itself (ServerInfo JSON, migration 0260).
    pub async fn forge_set_server(&self, id: &str, server_json: &str) -> Result<bool> {
        let r = sqlx::query(
            self.sql("UPDATE forge_connections SET server_json = ?, updated_at = ? WHERE id = ?"),
        )
        .bind(server_json)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// The last server probe of a connection (`None` when there is no such connection).
    pub async fn forge_server(&self, id: &str) -> Result<Option<String>> {
        let row = sqlx::query(self.sql("SELECT server_json FROM forge_connections WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| r.try_get::<String, _>("server_json").map_err(Into::into))
            .transpose()
    }

    pub async fn forge_set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let r = sqlx::query(
            self.sql("UPDATE forge_connections SET enabled = ?, updated_at = ? WHERE id = ?"),
        )
        .bind(b(enabled))
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Removes a connection (never github.com) with its orgs and repos' rows.
    pub async fn delete_forge_connection(&self, id: &str) -> Result<bool> {
        if id == GITHUB_COM {
            anyhow::bail!("the github.com connection cannot be removed; disable it instead");
        }
        for t in ["gh_orgs", "gh_repos"] {
            let q = format!("DELETE FROM {t} WHERE connection = ?");
            sqlx::query(self.sql(&q))
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        let r = sqlx::query(self.sql("DELETE FROM forge_connections WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Says which connection a repository key belongs to.
    pub async fn gh_set_repo_forge(&self, key: &str, f: &RepoForge) -> Result<bool> {
        let r = sqlx::query(self.sql(
            "UPDATE gh_repos SET connection = ?, forge_full_name = ?, forge_id = ? WHERE full_name = ?",
        ))
        .bind(&f.connection)
        .bind(&f.forge_full_name)
        .bind(f.forge_id)
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// The connection of a stored repository key (`None` when not stored).
    pub async fn gh_repo_forge(&self, key: &str) -> Result<Option<RepoForge>> {
        let row =
            sqlx::query(self.sql(
                "SELECT connection, forge_full_name, forge_id FROM gh_repos WHERE full_name = ?",
            ))
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| {
            Ok(RepoForge {
                connection: r.try_get("connection")?,
                forge_full_name: r.try_get("forge_full_name")?,
                forge_id: r.try_get("forge_id")?,
            })
        })
        .transpose()
    }

    /// Repository keys of one connection.
    pub async fn gh_repo_keys_of(&self, connection: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            self.sql("SELECT full_name FROM gh_repos WHERE connection = ? ORDER BY full_name"),
        )
        .bind(connection)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok(r.try_get("full_name")?)).collect()
    }

    /// Says which connection an org key belongs to and its forge login.
    pub async fn gh_set_org_forge(
        &self,
        login: &str,
        connection: &str,
        forge_login: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            self.sql("UPDATE gh_orgs SET connection = ?, forge_login = ? WHERE login = ?"),
        )
        .bind(connection)
        .bind(forge_login)
        .bind(login)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// `(connection, forge_login)` of a stored org key.
    pub async fn gh_org_forge(&self, login: &str) -> Result<Option<(String, String)>> {
        let row =
            sqlx::query(self.sql("SELECT connection, forge_login FROM gh_orgs WHERE login = ?"))
                .bind(login)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| Ok((r.try_get("connection")?, r.try_get("forge_login")?)))
            .transpose()
    }

    /// Org keys of one connection.
    pub async fn gh_org_keys_of(&self, connection: &str) -> Result<Vec<String>> {
        let rows =
            sqlx::query(self.sql("SELECT login FROM gh_orgs WHERE connection = ? ORDER BY login"))
                .bind(connection)
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(|r| Ok(r.try_get("login")?)).collect()
    }

    /// Marks a route's budget as known (rate headers) or counted.
    pub async fn gh_set_budget_known(&self, route: &str, known: bool) -> Result<()> {
        sqlx::query(self.sql("UPDATE gh_quota SET budget_known = ? WHERE route = ?"))
            .bind(b(known))
            .bind(route)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records which connection a conflict resolution went to.
    pub async fn gh_set_resolution_connection(&self, id: &str, connection: &str) -> Result<()> {
        sqlx::query(self.sql("UPDATE gh_resolutions SET connection = ? WHERE id = ?"))
            .bind(connection)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLES: [&str; 15] = [
        "gh_orgs",
        "gh_repos",
        "gh_pulls",
        "gh_issues",
        "gh_labels",
        "gh_etags",
        "gh_quota",
        "gh_calls",
        "gh_resolutions",
        "delivery_outcomes",
        "delivery_actions",
        "delivery_people",
        "delivery_commits",
        "delivery_decision_days",
        "settings",
    ];

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut out = vec![(s, Some(dir))];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            let s = Store::open(&url).await.unwrap();
            for t in TABLES {
                sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                    .execute(&s.pool)
                    .await
                    .unwrap();
            }
            out.push((s, None));
        }
        out
    }

    fn repo(full: &str) -> GhRepo {
        let (o, n) = full.split_once('/').unwrap();
        GhRepo {
            full_name: full.into(),
            owner: o.into(),
            name: n.into(),
            visibility: "private".into(),
            default_branch: "main".into(),
            mirror_pulls: true,
            synced_at: 1,
            ..Default::default()
        }
    }

    fn pull(repo: &str, n: i64) -> GhPull {
        GhPull {
            repo: repo.into(),
            number: n,
            title: "Fix".into(),
            state: "open".into(),
            mergeable: "unknown".into(),
            mergeable_state: "unknown".into(),
            checks_state: "none".into(),
            head_repo: repo.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn orgs_keep_switches_on_refresh() {
        for (s, _d) in stores().await {
            let mut o = GhOrg {
                login: "acme".into(),
                kind: "Organization".into(),
                enabled: true,
                repo_count: 3,
                ..Default::default()
            };
            s.gh_upsert_org(&o).await.unwrap();
            assert!(s.gh_set_org_managed("acme", true).await.unwrap());
            assert!(s.gh_set_org_enabled("acme", false).await.unwrap());
            o.repo_count = 4;
            s.gh_upsert_org(&o).await.unwrap();
            let got = s.gh_org("acme").await.unwrap().unwrap();
            assert!(got.managed && !got.enabled);
            assert_eq!(got.repo_count, 4);
            assert_eq!(s.gh_orgs().await.unwrap().len(), 1);
            assert!(!s.gh_set_org_managed("nope", true).await.unwrap());
        }
    }

    #[tokio::test]
    async fn repos_settings_revision_and_mirroring() {
        for (s, _d) in stores().await {
            s.gh_upsert_repo(&repo("acme/api")).await.unwrap();
            s.gh_upsert_repo(&repo("acme/web")).await.unwrap();
            s.gh_upsert_repo(&repo("other/x")).await.unwrap();
            assert_eq!(s.gh_repos(Some("acme")).await.unwrap().len(), 2);
            assert_eq!(s.gh_repos(None).await.unwrap().len(), 3);
            assert!(s.gh_set_repo_managed("acme/api", true).await.unwrap());
            let m = Mirroring {
                pulls: true,
                issues: true,
                labels: false,
            };
            assert!(s.gh_set_repo_mirroring("acme/api", m).await.unwrap());
            let settings = RepoSettings {
                autonomy: "draft_pr".into(),
                monthly_token_cap: Some(2_000_000),
                ..Default::default()
            };
            assert_eq!(
                s.gh_set_repo_settings("acme/api", &settings, 0)
                    .await
                    .unwrap(),
                Some(1)
            );
            // Stale revision is refused.
            assert_eq!(
                s.gh_set_repo_settings("acme/api", &settings, 0)
                    .await
                    .unwrap(),
                None
            );
            let bad = RepoSettings {
                autonomy: "yolo".into(),
                ..Default::default()
            };
            assert!(s.gh_set_repo_settings("acme/api", &bad, 1).await.is_err());
            // A refresh from GitHub keeps switches and settings.
            let mut fresh = repo("acme/api");
            fresh.default_branch = "trunk".into();
            s.gh_upsert_repo(&fresh).await.unwrap();
            let got = s.gh_repo("acme/api").await.unwrap().unwrap();
            assert_eq!(got.default_branch, "trunk");
            assert!(got.managed && got.mirror_issues && !got.sync_labels);
            assert_eq!(got.settings.autonomy, "draft_pr");
            assert_eq!(got.settings.monthly_token_cap, Some(2_000_000));
            assert_eq!(got.revision, 1);
            let managed = s.gh_managed_repos().await.unwrap();
            assert_eq!(managed.len(), 1);
            assert_eq!(managed[0].full_name, "acme/api");
        }
    }

    #[tokio::test]
    async fn pulls_upsert_reports_changes() {
        for (s, _d) in stores().await {
            let mut p = pull("acme/api", 7);
            p.checks = ChecksSummary {
                total: 2,
                success: 1,
                failure: 1,
                failing: vec!["lint".into()],
                ..Default::default()
            };
            p.checks_state = p.checks.state().into();
            p.requested_reviewers = vec!["ana".into()];
            assert_eq!(s.gh_upsert_pull(&p).await.unwrap(), Upsert::Inserted);
            p.synced_at = 99;
            assert_eq!(s.gh_upsert_pull(&p).await.unwrap(), Upsert::Unchanged);
            s.gh_mark_pull_captured("acme/api", 7).await.unwrap();
            p.mergeable = "conflict".into();
            assert_eq!(s.gh_upsert_pull(&p).await.unwrap(), Upsert::Updated);
            let got = s.gh_pull("acme/api", 7).await.unwrap().unwrap();
            assert!(got.delivery_captured, "capture flag survives upsert");
            assert_eq!(got.checks.failing, vec!["lint".to_string()]);
            assert_eq!(got.checks_state, "failure");
            assert_eq!(got.requested_reviewers, vec!["ana".to_string()]);
            assert_eq!(s.gh_conflicting_pulls().await.unwrap().len(), 1);
            s.gh_set_pull_draft("acme/api", 7, true).await.unwrap();
            assert!(s.gh_pull("acme/api", 7).await.unwrap().unwrap().draft);
            let mut closed = pull("acme/web", 1);
            closed.state = "closed".into();
            s.gh_upsert_pull(&closed).await.unwrap();
            assert_eq!(s.gh_pulls(None, Some("open")).await.unwrap().len(), 1);
            assert_eq!(s.gh_pulls(Some("acme/web"), None).await.unwrap().len(), 1);
            assert_eq!(s.gh_pulls(None, None).await.unwrap().len(), 2);
            assert!(!got.is_fork());
        }
    }

    #[tokio::test]
    async fn issues_mirror_and_cursor() {
        for (s, _d) in stores().await {
            assert_eq!(s.gh_issues_cursor("acme/api").await.unwrap(), None);
            let mut i = GhIssue {
                repo: "acme/api".into(),
                number: 3,
                title: "Bug".into(),
                state: "open".into(),
                labels: vec!["bug".into()],
                updated_at: 100,
                ..Default::default()
            };
            assert_eq!(s.gh_upsert_issue(&i).await.unwrap(), Upsert::Inserted);
            s.gh_link_issue_card("acme/api", 3, "card-1").await.unwrap();
            i.synced_at = 5;
            assert_eq!(s.gh_upsert_issue(&i).await.unwrap(), Upsert::Unchanged);
            i.state = "closed".into();
            i.updated_at = 200;
            assert_eq!(s.gh_upsert_issue(&i).await.unwrap(), Upsert::Updated);
            let got = s.gh_issue("acme/api", 3).await.unwrap().unwrap();
            assert_eq!(got.card_id.as_deref(), Some("card-1"));
            assert_eq!(got.labels, vec!["bug".to_string()]);
            assert_eq!(s.gh_issues_cursor("acme/api").await.unwrap(), Some(200));
            assert_eq!(
                s.gh_issues(Some("acme/api"), Some("closed"))
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert!(s.gh_issues(None, Some("open")).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn labels_replace() {
        for (s, _d) in stores().await {
            let l = |n: &str| GhLabel {
                repo: "acme/api".into(),
                name: n.into(),
                color: "00e0c9".into(),
                description: String::new(),
            };
            s.gh_set_labels("acme/api", &[l("sf:fix"), l("bug")])
                .await
                .unwrap();
            s.gh_set_labels("acme/api", &[l("sf:fix")]).await.unwrap();
            let got = s.gh_labels("acme/api").await.unwrap();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].name, "sf:fix");
        }
    }

    #[tokio::test]
    async fn etag_cache_roundtrip_and_drop() {
        for (s, _d) in stores().await {
            assert!(s.gh_etag("local|/repos/a/b").await.unwrap().is_none());
            s.gh_put_etag("local|/repos/a/b", "W/\"1\"", "", "{}")
                .await
                .unwrap();
            s.gh_put_etag("local|/repos/a/b", "W/\"2\"", "<x>; rel=\"next\"", "[]")
                .await
                .unwrap();
            s.gh_put_etag("local|/repos/a/b/pulls", "W/\"3\"", "", "[]")
                .await
                .unwrap();
            s.gh_put_etag("local|/user", "W/\"4\"", "", "{}")
                .await
                .unwrap();
            let e = s.gh_etag("local|/repos/a/b").await.unwrap().unwrap();
            assert_eq!(e.etag, "W/\"2\"");
            assert_eq!(e.body, "[]");
            assert!(e.link.contains("next"));
            assert_eq!(s.gh_drop_etags("local|/repos/a/b").await.unwrap(), 2);
            assert!(s.gh_etag("local|/user").await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn quota_and_calls() {
        for (s, _d) in stores().await {
            let q = Quota {
                route: "local".into(),
                resource: "core".into(),
                limit: 5000,
                remaining: 4000,
                used: 1000,
                reset_at: 10,
                observed_at: 1,
                secondary_until: 0,
            };
            s.gh_put_quota(&q).await.unwrap();
            s.gh_set_secondary_until("local", "core", 777)
                .await
                .unwrap();
            let mut q2 = q.clone();
            q2.remaining = 3999;
            s.gh_put_quota(&q2).await.unwrap();
            let got = s.gh_quota("local", "core").await.unwrap().unwrap();
            assert_eq!(got.remaining, 3999);
            assert_eq!(got.secondary_until, 777, "kept across observations");
            s.gh_set_secondary_until("node:n1", "core", 5)
                .await
                .unwrap();
            assert_eq!(s.gh_quotas().await.unwrap().len(), 2);

            let at = 3_600_000 * 5 + 10;
            s.gh_count_call("local", "poller", at, false).await.unwrap();
            s.gh_count_call("local", "poller", at + 5, true)
                .await
                .unwrap();
            s.gh_count_call("local", "user", at, false).await.unwrap();
            s.gh_count_call("local", "poller", at + 3_600_000, false)
                .await
                .unwrap();
            let c = s.gh_calls_in_hour("local", at).await.unwrap();
            assert_eq!(c.len(), 2);
            assert_eq!(c[0].caller, "poller");
            assert_eq!((c[0].calls, c[0].not_modified), (2, 1));
            assert_eq!(c[1].calls, 1);
        }
    }

    #[tokio::test]
    async fn resolutions_roundtrip() {
        for (s, _d) in stores().await {
            let r = Resolution {
                id: "r1".into(),
                repo: "acme/api".into(),
                number: 4,
                branch: "feat".into(),
                commit_sha: "abc".into(),
                parent_shas: vec!["h".into(), "b".into()],
                files: vec!["src/a.rs".into()],
                signing: "github_signs".into(),
                verified: Some(true),
                verification_reason: "valid".into(),
                route: "local".into(),
                created_at: 5,
            };
            s.gh_insert_resolution(&r).await.unwrap();
            assert_eq!(s.gh_resolutions("acme/api", 4).await.unwrap(), vec![r]);
        }
    }

    #[tokio::test]
    async fn delivery_facts() {
        for (s, _d) in stores().await {
            let o = Outcome {
                outcome_key: "pr:acme/api#1".into(),
                kind: "pr".into(),
                title: "T".into(),
                repo: "acme/api".into(),
                resolved_at: Some(1000),
                result: "merged".into(),
                cycle_ms: Some(500),
                reviewers: vec!["ana".into()],
                ..Default::default()
            };
            s.upsert_outcome(&o).await.unwrap();
            let mut o2 = o.clone();
            o2.title = "T2".into();
            s.upsert_outcome(&o2).await.unwrap();
            let got = s.outcome("pr:acme/api#1").await.unwrap().unwrap();
            assert_eq!(got.title, "T2");
            assert_eq!(got.reviewers, vec!["ana".to_string()]);
            assert_eq!(s.outcomes_between(0, 2000).await.unwrap().len(), 1);
            assert!(s.outcomes_between(1001, 2000).await.unwrap().is_empty());

            let a = HumanAction {
                source: "review".into(),
                ref_id: "rev-1".into(),
                action_kind: "approved".into(),
                actor_key: "gh:ana".into(),
                actor_login: "ana".into(),
                outcome_key: Some(o.outcome_key.clone()),
                repo: Some("acme/api".into()),
                occurred_at: 900,
                raw_units_json: "{}".into(),
                basis: "derived".into(),
                ..Default::default()
            };
            assert!(s.upsert_action(&a).await.unwrap());
            assert!(!s.upsert_action(&a).await.unwrap(), "captured once");
            assert_eq!(s.actions_between(0, 1000).await.unwrap(), vec![a]);

            let p = Person {
                person_key: "gh:ana".into(),
                login: "ana".into(),
                emails: vec!["a@x".into()],
                first_seen_at: Some(50),
                last_seen_at: Some(60),
                ..Default::default()
            };
            s.upsert_person(&p).await.unwrap();
            s.upsert_person(&Person {
                person_key: "gh:ana".into(),
                display_name: "Ana".into(),
                emails: vec!["b@x".into(), "a@x".into()],
                is_internal: Some(true),
                first_seen_at: Some(10),
                last_seen_at: Some(30),
                ..Default::default()
            })
            .await
            .unwrap();
            let got = s.person("gh:ana").await.unwrap().unwrap();
            assert_eq!(got.login, "ana");
            assert_eq!(got.display_name, "Ana");
            assert_eq!(got.emails, vec!["a@x".to_string(), "b@x".to_string()]);
            assert_eq!(got.is_internal, Some(true));
            assert_eq!((got.first_seen_at, got.last_seen_at), (Some(10), Some(60)));
            assert_eq!(s.people().await.unwrap().len(), 1);

            let c = DeliveryCommit {
                repo: "acme/api".into(),
                sha: "s1".into(),
                author_login: "ana".into(),
                author_key: "gh:ana".into(),
                signed: Some(true),
                pr_number: Some(1),
                committed_at: Some(800),
                ..Default::default()
            };
            s.upsert_commit(&c).await.unwrap();
            s.upsert_commit(&c).await.unwrap();
            assert_eq!(s.commits_between(0, 1000).await.unwrap(), vec![c]);
        }
    }

    #[tokio::test]
    async fn decision_rollup() {
        for (s, _d) in stores().await {
            let day = 86_400_000 * 3;
            s.record_decision(
                day + 5,
                "mode",
                "m1",
                DecisionEnd::Proceeded,
                Some(0.85),
                Some(100),
            )
            .await
            .unwrap();
            s.record_decision(
                day + 6,
                "mode",
                "m1",
                DecisionEnd::Escalated,
                Some(1.0),
                None,
            )
            .await
            .unwrap();
            s.record_decision(
                day + 7,
                "plan",
                "m1",
                DecisionEnd::RuleHandled,
                None,
                Some(50),
            )
            .await
            .unwrap();
            let days = s.decision_days(day + 1, day + 86_400_000).await.unwrap();
            assert_eq!(days.len(), 2);
            let mode = &days[0];
            assert_eq!(mode.kind, "mode");
            assert_eq!((mode.total, mode.proceeded, mode.escalated), (2, 1, 1));
            assert_eq!(mode.conf_buckets[8], 1);
            assert_eq!(mode.conf_buckets[9], 1);
            assert_eq!((mode.latency_sum_ms, mode.latency_n), (100, 1));
            assert_eq!(days[1].rule_handled, 1);
        }
    }

    /// forges.md 8.1: a database stopped at 0008 (before every forge) with
    /// GitHub rows, then opened by this build (0009 onwards): every key and
    /// value stays, and the new columns say github.com.
    #[tokio::test]
    async fn upgrade_from_0008_keeps_every_github_row() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("old.db").display());
        sqlx::any::install_default_drivers();
        {
            let pool = AnyPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            crate::SQLITE_MIGRATIONS.run_to(8, &pool).await.unwrap();
            assert!(
                sqlx::query("SELECT id FROM forge_connections")
                    .fetch_all(&pool)
                    .await
                    .is_err(),
                "0008 has no forge_connections"
            );
            for q in [
                "INSERT INTO gh_orgs (login, kind, repo_count) VALUES ('acme', 'Organization', 3)",
                "INSERT INTO gh_repos (full_name, owner, name, can_push, managed, settings_json) \
                 VALUES ('acme/api', 'acme', 'api', 1, 1, '{\"protected_branches\":false}')",
                "INSERT INTO gh_pulls (repo, number, title, head_sha, mergeable) \
                 VALUES ('acme/api', 7, 'Fix', 'abc', 'conflict')",
                "INSERT INTO gh_quota (route, resource, rate_limit, remaining) \
                 VALUES ('local', 'core', 5000, 4999)",
                "INSERT INTO gh_resolutions (id, repo, number, branch, commit_sha, created_at) \
                 VALUES ('r1', 'acme/api', 7, 'feat', 'def', 1)",
            ] {
                sqlx::query(q).execute(&pool).await.unwrap();
            }
            pool.close().await;
        }
        let s = Store::open(&url).await.unwrap();
        let conns = s.forge_connections().await.unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(
            (conns[0].id.as_str(), conns[0].origin.as_str()),
            (GITHUB_COM, "migration")
        );
        assert_eq!(
            s.forge_server(GITHUB_COM).await.unwrap().as_deref(),
            Some("{}")
        );
        let org = s.gh_org("acme").await.unwrap().unwrap();
        assert_eq!(org.repo_count, 3);
        assert_eq!(
            s.gh_org_forge("acme").await.unwrap().map(|f| f.0),
            Some(GITHUB_COM.to_string())
        );
        let r = s.gh_repo("acme/api").await.unwrap().unwrap();
        assert!(r.can_push && r.managed && !r.settings.protected_branches);
        let f = s.gh_repo_forge("acme/api").await.unwrap().unwrap();
        assert_eq!(
            (
                f.connection.as_str(),
                f.forge_full_name.as_str(),
                f.forge_id
            ),
            (GITHUB_COM, "", None)
        );
        let p = s.gh_pull("acme/api", 7).await.unwrap().unwrap();
        assert_eq!(
            (p.title.as_str(), p.mergeable.as_str()),
            ("Fix", "conflict")
        );
        let q = s.gh_quota("local", "core").await.unwrap().unwrap();
        assert_eq!((q.limit, q.remaining), (5000, 4999));
        let row = sqlx::query("SELECT connection, signing FROM gh_resolutions WHERE id = 'r1'")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(row.try_get::<String, _>("connection").unwrap(), GITHUB_COM);
        assert_eq!(row.try_get::<String, _>("signing").unwrap(), "github_signs");
        // The server probe column takes a value on the upgraded database.
        assert!(s.forge_set_server(GITHUB_COM, "{\"x\":1}").await.unwrap());
        assert_eq!(
            s.forge_server(GITHUB_COM).await.unwrap().as_deref(),
            Some("{\"x\":1}")
        );
        assert_eq!(s.forge_server("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn forge_connections_and_keys() {
        for (s, _d) in stores().await {
            sqlx::query(AssertSqlSafe(
                "DELETE FROM forge_connections WHERE id <> 'github.com'".to_string(),
            ))
            .execute(&s.pool)
            .await
            .unwrap();
            // The migration made the github.com row.
            let all = s.forge_connections().await.unwrap();
            assert_eq!(all[0].id, GITHUB_COM);
            assert_eq!(all[0].origin, "migration");
            assert_eq!((all[0].kind.as_str(), all[0].enabled), ("github", true));
            let cb = ForgeConnectionRow {
                id: "c1".into(),
                kind: "forgejo".into(),
                host: "codeberg.org".into(),
                label: "Codeberg".into(),
                cli_name: "codeberg.org".into(),
                git_protocol: "https".into(),
                route_pref: "auto".into(),
                enabled: true,
                status_json: "{}".into(),
                origin: "user".into(),
                created_at: 5,
                updated_at: 5,
                ..Default::default()
            };
            s.upsert_forge_connection(&cb).await.unwrap();
            assert!(s.forge_set_status("c1", "ana", "{\"x\":1}").await.unwrap());
            assert!(s.forge_set_status("c1", "", "{}").await.unwrap());
            let got = s.forge_connection("c1").await.unwrap().unwrap();
            assert_eq!((got.login.as_str(), got.created_at), ("ana", 5));
            assert_eq!(
                s.forge_connection_at("forgejo", "codeberg.org")
                    .await
                    .unwrap()
                    .map(|c| c.id),
                Some("c1".into())
            );
            assert!(s.forge_set_enabled("c1", false).await.unwrap());
            assert!(!s.forge_connection("c1").await.unwrap().unwrap().enabled);
            // Old rows keep github.com; new rows say where they live.
            s.gh_upsert_repo(&repo("acme/api")).await.unwrap();
            assert_eq!(
                s.gh_repo_forge("acme/api")
                    .await
                    .unwrap()
                    .unwrap()
                    .connection,
                GITHUB_COM
            );
            let mut r = repo("codeberg.org/ana/app");
            r.owner = "codeberg.org/ana".into();
            r.name = "app".into();
            s.gh_upsert_repo(&r).await.unwrap();
            let f = RepoForge {
                connection: "c1".into(),
                forge_full_name: "ana/app".into(),
                forge_id: Some(7),
            };
            assert!(s.gh_set_repo_forge(&r.full_name, &f).await.unwrap());
            assert_eq!(s.gh_repo_forge(&r.full_name).await.unwrap(), Some(f));
            assert_eq!(
                s.gh_repo_keys_of("c1").await.unwrap(),
                vec!["codeberg.org/ana/app".to_string()]
            );
            s.gh_upsert_org(&GhOrg {
                login: "codeberg.org/ana".into(),
                kind: "User".into(),
                enabled: true,
                ..Default::default()
            })
            .await
            .unwrap();
            assert!(
                s.gh_set_org_forge("codeberg.org/ana", "c1", "ana")
                    .await
                    .unwrap()
            );
            assert_eq!(
                s.gh_org_forge("codeberg.org/ana").await.unwrap(),
                Some(("c1".into(), "ana".into()))
            );
            assert_eq!(s.gh_org_keys_of("c1").await.unwrap().len(), 1);
            assert!(s.delete_forge_connection(GITHUB_COM).await.is_err());
            assert!(s.delete_forge_connection("c1").await.unwrap());
            assert!(s.gh_repo("codeberg.org/ana/app").await.unwrap().is_none());
            assert!(s.gh_repo("acme/api").await.unwrap().is_some());
            s.gh_set_budget_known("local", false).await.unwrap();
            s.gh_set_resolution_connection("none", "c1").await.unwrap();
        }
    }

    #[test]
    fn buckets() {
        assert_eq!(hour_bucket(3_600_000 + 5), 3_600_000);
        assert_eq!(day_start(86_400_000 * 2 + 1), 86_400_000 * 2);
    }
}
