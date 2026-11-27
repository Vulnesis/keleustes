//! Events a webhook would have sent, synthesised by the poller (there are
//! no webhooks on a desktop): new commits on the default branch (`push`),
//! finished CI runs (`workflow_run.completed`), published releases and new
//! security alerts. Each list keeps a cursor per connection and repository
//! (in `gh_etags`, key `cursor|<connection>|<kind>|<repo>`); the first read
//! only sets the cursor, so nothing old floods the triggers. A list the
//! forge does not offer (404, 403) is skipped quietly.

use crate::client::Client;
use crate::forge::Dialect;
use crate::forge::api::absent;
use anyhow::Result;
use kls_core::github::kinds;
use kls_store::github::GhRepo;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EventReport {
    pub pushes: usize,
    pub ci_runs: usize,
    pub releases: usize,
    pub alerts: usize,
    pub comments: usize,
    /// `registry_package.published`: new package versions.
    pub packages: usize,
    /// `github-posture` and `github-risk` signals.
    pub risks: usize,
    /// `deployment_review.requested|approved|rejected`.
    pub deploy_reviews: usize,
    /// `deployment_status.created`: a deployment changed state.
    pub deployments: usize,
}

impl EventReport {
    pub fn total(&self) -> usize {
        self.deploy_reviews
            + self.deployments
            + self.pushes
            + self.ci_runs
            + self.releases
            + self.alerts
            + self.comments
            + self.packages
            + self.risks
    }
}

fn cursor_key(c: &Client, kind: &str, repo: &str) -> String {
    format!("cursor|{}|{kind}|{repo}", c.conn().id)
}

pub(crate) async fn cursor(c: &Client, kind: &str, repo: &str) -> Result<Option<String>> {
    Ok(c.core()
        .store()
        .gh_etag(&cursor_key(c, kind, repo))
        .await?
        .map(|e| e.body))
}

pub(crate) async fn set_cursor(c: &Client, kind: &str, repo: &str, v: &str) -> Result<()> {
    c.core()
        .store()
        .gh_put_etag(&cursor_key(c, kind, repo), "cursor", "", v)
        .await
}

/// Skips a list the forge does not offer.
fn quiet<T: Default>(r: Result<T>) -> Result<T> {
    match r {
        Err(e) if absent(&e) => Ok(T::default()),
        other => other,
    }
}

fn id_of(v: &Value) -> i64 {
    v["id"]
        .as_i64()
        .or_else(|| v["number"].as_i64())
        .unwrap_or(0)
}

/// Reads the four lists of one repository and emits what is new.
pub async fn poll_repo(c: &Client, r: &GhRepo, caller: &str) -> Result<EventReport> {
    let mut rep = EventReport::default();
    let api = c.api();
    let repo = r.full_name.as_str();
    let conn = c.conn().id.clone();

    // push: the default branch's head moved.
    let commits = quiet(api.branch_commits(c, repo, &r.default_branch, caller).await)?;
    if !commits.not_modified
        && let Some(head) = commits.items.first().and_then(|x| x["sha"].as_str())
    {
        let before = cursor(c, "push", repo).await?;
        if before.as_deref() != Some(head) {
            if let Some(before) = &before {
                let new: Vec<&Value> = commits
                    .items
                    .iter()
                    .take_while(|x| x["sha"].as_str() != Some(before))
                    .collect();
                let changed: Vec<String> = if c.conn().dialect() == Dialect::Gitea {
                    Vec::new()
                } else {
                    match api.compare(c, repo, before, head, caller).await {
                        Ok(v) => v["files"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|f| f["filename"].as_str().map(String::from))
                                    .take(300)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        Err(_) => Vec::new(),
                    }
                };
                let messages: Vec<&str> = new
                    .iter()
                    .filter_map(|x| x["commit"]["message"].as_str())
                    .map(|m| m.lines().next().unwrap_or(""))
                    .take(10)
                    .collect();
                c.core()
                    .emit(
                        kinds::PUSH,
                        repo,
                        &json!({ "repo": repo, "ref": format!("refs/heads/{}", r.default_branch),
                                 "branch": r.default_branch, "before": before, "after": head,
                                 "commits": new.len(), "messages": messages,
                                 "changedFiles": changed, "connection": conn }),
                    )
                    .await?;
                rep.pushes += 1;
            }
            set_cursor(c, "push", repo, head).await?;
        }
    }

    // workflow_run.completed: CI runs newer than the cursor.
    let runs = quiet(api.ci_runs(c, repo, caller).await)?;
    if let Some(max) = runs.iter().map(id_of).max() {
        let last = cursor(c, "ci", repo)
            .await?
            .and_then(|s| s.parse::<i64>().ok());
        if let Some(last) = last {
            let mut new: Vec<&Value> = runs.iter().filter(|x| id_of(x) > last).collect();
            new.sort_by_key(|x| id_of(x));
            for run in new {
                c.core()
                    .emit(
                        kinds::WORKFLOW_RUN,
                        &format!("{repo}@{}", id_of(run)),
                        &json!({ "repo": repo, "id": id_of(run), "name": run["name"],
                                 "conclusion": run["conclusion"], "headBranch": run["head_branch"],
                                 "headSha": run["head_sha"], "url": run["html_url"],
                                 "failed": matches!(run["conclusion"].as_str(), Some("failure" | "timed_out")),
                                 "connection": conn }),
                    )
                    .await?;
                rep.ci_runs += 1;
            }
        }
        if last != Some(max) {
            set_cursor(c, "ci", repo, &max.to_string()).await?;
        }
    }

    // release.published
    let rels = quiet(api.releases(c, repo, caller).await)?;
    if let Some(max) = rels.iter().map(id_of).max() {
        let last = cursor(c, "release", repo)
            .await?
            .and_then(|s| s.parse::<i64>().ok());
        if let Some(last) = last {
            for rel in rels.iter().filter(|x| id_of(x) > last) {
                c.core()
                    .emit(
                        kinds::RELEASE,
                        &format!("{repo}@{}", rel["tag_name"].as_str().unwrap_or("")),
                        &json!({ "repo": repo, "tag": rel["tag_name"], "name": rel["name"],
                                 "url": rel["html_url"], "connection": conn }),
                    )
                    .await?;
                rep.releases += 1;
            }
        }
        if last != Some(max) {
            set_cursor(c, "release", repo, &max.to_string()).await?;
        }
    }

    // *_alert.created
    let alerts = quiet(api.alerts(c, repo, caller).await)?;
    for kind in ["dependabot", "code_scanning", "secret_scanning"] {
        let of: Vec<&Value> = alerts
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, v)| v)
            .collect();
        let Some(max) = of.iter().map(|v| id_of(v)).max() else {
            continue;
        };
        let ck = format!("alert:{kind}");
        let last = cursor(c, &ck, repo)
            .await?
            .and_then(|s| s.parse::<i64>().ok());
        if let Some(last) = last {
            for a in of.iter().filter(|v| id_of(v) > last) {
                c.core()
                    .emit(
                        kinds::ALERT,
                        &format!("{repo}#{kind}-{}", id_of(a)),
                        &json!({ "repo": repo, "alert": kind, "number": id_of(a),
                                 "severity": a["severity"], "summary": a["summary"],
                                 "url": a["html_url"], "connection": conn }),
                    )
                    .await?;
                rep.alerts += 1;
            }
        }
        if last != Some(max) {
            set_cursor(c, &ck, repo, &max.to_string()).await?;
        }
    }

    // issue_comment.created: comments newer than the cursor (`/sf` commands).
    let last = cursor(c, "comments", repo)
        .await?
        .and_then(|s| s.parse::<i64>().ok());
    match last {
        None => set_cursor(c, "comments", repo, &crate::now_ms_str()).await?,
        Some(last) => {
            let list = quiet(api.comments_since(c, repo, last, caller).await)?;
            let mut max = last;
            let mut assoc: std::collections::BTreeMap<String, String> = Default::default();
            for cm in &list {
                let at = crate::parse::ts(&cm["created_at"]).unwrap_or(0);
                if at <= last {
                    continue;
                }
                max = max.max(at);
                let n = cm["number"].as_i64().unwrap_or(0);
                let author = cm["author"].as_str().unwrap_or("").to_string();
                let mut association = cm["author_association"].as_str().unwrap_or("").to_string();
                if association.is_empty() && !author.is_empty() {
                    if !assoc.contains_key(&author) {
                        let a = api
                            .permission(c, repo, &author, caller)
                            .await
                            .unwrap_or_default();
                        assoc.insert(author.clone(), a);
                    }
                    association = assoc[&author].clone();
                }
                let body: String = cm["body"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(4000)
                    .collect();
                let command = sf_command(&body);
                let is_pull = c.core().store().gh_pull(repo, n).await?.is_some();
                let from_factory = crate::sync::factory_authored(
                    c,
                    repo,
                    crate::sync::Authored::Comment,
                    cm["id"].as_i64().unwrap_or(0),
                    &author,
                    &body,
                )
                .await;
                c.core()
                    .emit(
                        kinds::ISSUE_COMMENT,
                        &format!("{repo}#{n}"),
                        &json!({ "repo": repo, "number": n, "id": cm["id"], "body": body,
                                 "author": author, "authorAssociation": association,
                                 "internal": crate::parse::is_internal(&association),
                                 "url": cm["html_url"], "isPull": is_pull, "command": command,
                                 "fromFactory": from_factory, "connection": conn }),
                    )
                    .await?;
                rep.comments += 1;
            }
            if max > last {
                set_cursor(c, "comments", repo, &max.to_string()).await?;
            }
        }
    }

    // registry_package.published, deployment_review.* and github-posture (GitHub only).
    if c.conn().dialect() == Dialect::GitHub {
        rep.packages += packages(c, r, caller).await?;
        rep.deploy_reviews += deployment_reviews(c, r, caller).await?;
        rep.deployments += deployment_statuses(c, r, caller).await?;
        if risk_on(c).await {
            rep.risks += posture(c, r, caller).await?;
        }
    }
    Ok(rep)
}

/// Environment reviews of waiting workflow runs (the `deployment_review`
/// webhook, polled): each `pending_deployments` entry of a `waiting` run is
/// emitted once as `github.deployment_review.requested`; when it is no longer
/// pending, the run's review history decides `.approved` or `.rejected` (a
/// run that ended without a review counts as rejected, with its conclusion
/// as the reason). Open reviews are kept in the `deploy` cursor. Returns how
/// many events were emitted.
async fn deployment_reviews(c: &Client, r: &GhRepo, caller: &str) -> Result<usize> {
    use std::collections::BTreeMap;
    let repo = r.full_name.as_str();
    let forge_repo = c.forge_name(repo);
    let conn = c.conn().id.clone();
    let mut open: BTreeMap<String, Value> = cursor(c, "deploy", repo)
        .await?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let before = open.clone();
    let runs = match c
        .get_json(
            &format!("/repos/{forge_repo}/actions/runs?status=waiting&per_page=20"),
            caller,
        )
        .await
    {
        Err(e) if absent(&e) => return Ok(0),
        other => other?,
    };
    let mut pending: BTreeMap<String, Value> = BTreeMap::new();
    for run in runs["workflow_runs"].as_array().into_iter().flatten() {
        let id = id_of(run);
        if id <= 0 {
            continue;
        }
        let list = match c
            .get_json(
                &format!("/repos/{forge_repo}/actions/runs/{id}/pending_deployments"),
                caller,
            )
            .await
        {
            Err(e) if absent(&e) => continue,
            other => other?,
        };
        let requester = run["triggering_actor"]["login"]
            .as_str()
            .or_else(|| run["actor"]["login"].as_str())
            .unwrap_or("");
        for d in list.as_array().into_iter().flatten() {
            let env = d["environment"]["name"].as_str().unwrap_or("");
            if env.is_empty() {
                continue;
            }
            pending.insert(
                format!("{repo}:{id}:{env}"),
                json!({ "repo": repo, "runId": id.to_string(), "environment": env,
                        "url": run["html_url"], "requester": requester,
                        "workflow": run["name"], "connection": conn }),
            );
        }
    }
    let mut n = 0;
    for (subject, data) in &pending {
        if open.contains_key(subject) {
            continue;
        }
        c.core()
            .emit(kinds::DEPLOY_REVIEW_REQUESTED, subject, data)
            .await?;
        open.insert(subject.clone(), data.clone());
        n += 1;
    }
    // Reviews no longer pending: the run's review history decides.
    let gone: Vec<String> = open
        .keys()
        .filter(|s| !pending.contains_key(*s))
        .cloned()
        .collect();
    let mut history: BTreeMap<String, (Value, Value)> = BTreeMap::new();
    for subject in gone {
        let mut data = open.remove(&subject).unwrap_or_default();
        let run_id = data["runId"].as_str().unwrap_or("").to_string();
        let env = data["environment"].as_str().unwrap_or("").to_string();
        if !history.contains_key(&run_id) {
            let approvals = c
                .get_json(
                    &format!("/repos/{forge_repo}/actions/runs/{run_id}/approvals"),
                    caller,
                )
                .await
                .unwrap_or(Value::Null);
            let run = c
                .get_json(
                    &format!("/repos/{forge_repo}/actions/runs/{run_id}"),
                    caller,
                )
                .await
                .unwrap_or(Value::Null);
            history.insert(run_id.clone(), (approvals, run));
        }
        let (approvals, run) = &history[&run_id];
        // Newest review naming this environment.
        let review = approvals.as_array().and_then(|a| {
            a.iter().rev().find(|x| {
                x["environments"]
                    .as_array()
                    .is_some_and(|e| e.iter().any(|e| e["name"].as_str() == Some(&env)))
            })
        });
        let kind = match review.and_then(|x| x["state"].as_str()) {
            Some("approved") => kinds::DEPLOY_REVIEW_APPROVED,
            Some(_) => kinds::DEPLOY_REVIEW_REJECTED,
            None => {
                let ended = run["status"].as_str() == Some("completed");
                let ok = run["conclusion"].as_str() == Some("success");
                if ended && !ok {
                    data["reason"] = json!(format!(
                        "run_{}",
                        run["conclusion"].as_str().unwrap_or("ended")
                    ));
                    kinds::DEPLOY_REVIEW_REJECTED
                } else {
                    data["reason"] = json!("proceeded");
                    kinds::DEPLOY_REVIEW_APPROVED
                }
            }
        };
        if let Some(x) = review {
            data["reviewer"] = x["user"]["login"].clone();
            data["comment"] = x["comment"].clone();
        }
        c.core().emit(kind, &subject, &data).await?;
        n += 1;
    }
    if open != before {
        set_cursor(c, "deploy", repo, &serde_json::to_string(&open)?).await?;
    }
    Ok(n)
}

/// Deployment states after which a deployment is not read again.
pub const DEPLOY_TERMINAL: [&str; 4] = ["success", "failure", "error", "inactive"];
/// Newest deployments read on each pass.
pub const DEPLOYMENTS_READ: usize = 10;

/// Deployment statuses (the `deployment_status` webhook, polled): the
/// newest [`DEPLOYMENTS_READ`] deployments (a conditional read), and for
/// each one not yet in a final state its newest status. A state that
/// changed is emitted once as `github.deployment.status`. The `deployment`
/// cursor keeps each deployment's last state; the first pass only fills it.
async fn deployment_statuses(c: &Client, r: &GhRepo, caller: &str) -> Result<usize> {
    use std::collections::BTreeMap;
    let repo = r.full_name.as_str();
    let forge_repo = c.forge_name(repo);
    let first = cursor(c, "deployment", repo).await?.is_none();
    let before: BTreeMap<String, String> = cursor(c, "deployment", repo)
        .await?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let list = match c
        .get_json(
            &format!("/repos/{forge_repo}/deployments?per_page={DEPLOYMENTS_READ}"),
            caller,
        )
        .await
    {
        Err(e) if absent(&e) => return Ok(0),
        other => other?,
    };
    let mut after = BTreeMap::new();
    let mut n = 0;
    for d in list.as_array().into_iter().flatten().take(DEPLOYMENTS_READ) {
        let id = id_of(d);
        if id <= 0 {
            continue;
        }
        let key = id.to_string();
        let seen = before.get(&key).cloned().unwrap_or_default();
        if DEPLOY_TERMINAL.contains(&seen.as_str()) {
            after.insert(key, seen);
            continue;
        }
        let statuses = match c
            .get_json(
                &format!("/repos/{forge_repo}/deployments/{id}/statuses?per_page=1"),
                caller,
            )
            .await
        {
            Err(e) if absent(&e) => continue,
            other => other?,
        };
        let st = statuses.as_array().and_then(|a| a.first());
        let state = st
            .and_then(|s| s["state"].as_str())
            .unwrap_or("")
            .to_string();
        after.insert(key, state.clone());
        if first || state.is_empty() || state == seen {
            continue;
        }
        let env = d["environment"].as_str().unwrap_or("");
        c.core()
            .emit(
                kinds::DEPLOYMENT_STATUS,
                &format!("{repo}:{id}"),
                &json!({ "repo": repo, "deploymentId": id, "environment": env,
                         "state": state, "ref": d["ref"], "sha": d["sha"],
                         "url": st.and_then(|s| s["log_url"].as_str().or(s["target_url"].as_str())),
                         "creator": d["creator"]["login"], "connection": c.conn().id }),
            )
            .await?;
        n += 1;
    }
    if first || before != after {
        set_cursor(c, "deployment", repo, &serde_json::to_string(&after)?).await?;
    }
    Ok(n)
}

/// The package types read for `registry_package` (the `cli-image` default
/// fires on a container image).
pub const PACKAGE_TYPES: [&str; 2] = ["container", "npm"];

async fn risk_on(c: &Client) -> bool {
    c.core()
        .store()
        .setting(kls_core::github::settings::RISK_POLL)
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v.trim() != "off")
}

/// New versions of the packages a repository publishes: the owner's
/// package list (org, else user; conditional reads) filtered to the
/// repository, with a cursor of each package's `updated_at`. A changed
/// package reads its newest version once. Absent lists are skipped quietly.
async fn packages(c: &Client, r: &GhRepo, caller: &str) -> Result<usize> {
    let repo = r.full_name.as_str();
    let forge_repo = c.forge_name(repo);
    let owner = forge_repo.split('/').next().unwrap_or("");
    if owner.is_empty() {
        return Ok(0);
    }
    let mut n = 0;
    for kind in PACKAGE_TYPES {
        let mut base = format!("/orgs/{owner}/packages");
        let path = |b: &str| format!("{b}?package_type={kind}&per_page=100");
        let list = match c.get_all(&path(&base), caller).await {
            Err(e) if absent(&e) => {
                base = format!("/users/{owner}/packages");
                quiet(c.get_all(&path(&base), caller).await)?
            }
            other => other?,
        };
        let mine: Vec<&Value> = list
            .items
            .iter()
            .filter(|p| p["repository"]["full_name"].as_str() == Some(forge_repo))
            .collect();
        let ck = format!("package:{kind}");
        let before: std::collections::BTreeMap<String, String> = cursor(c, &ck, repo)
            .await?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let first = cursor(c, &ck, repo).await?.is_none();
        let mut after = std::collections::BTreeMap::new();
        for p in &mine {
            let name = p["name"].as_str().unwrap_or("").to_string();
            let updated = p["updated_at"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            after.insert(name.clone(), updated.clone());
            if first || before.get(&name) == Some(&updated) {
                continue;
            }
            let versions = c
                .get_json(
                    &format!(
                        "{base}/{kind}/{}/versions?per_page=1",
                        crate::http::encode_component(&name)
                    ),
                    caller,
                )
                .await
                .unwrap_or(Value::Null);
            let v = versions.as_array().and_then(|a| a.first()).cloned();
            let version = v
                .as_ref()
                .and_then(|v| {
                    v["metadata"]["container"]["tags"]
                        .as_array()
                        .and_then(|t| t.first())
                        .and_then(Value::as_str)
                        .or_else(|| v["name"].as_str())
                })
                .unwrap_or("")
                .to_string();
            c.core()
                .emit(
                    kinds::PACKAGE_PUBLISHED,
                    &format!("{repo}@{name}"),
                    &json!({ "repo": repo, "action": "published", "package": name,
                             "ecosystem": kind, "version": version,
                             "digest": v.as_ref().and_then(|v| v["name"].as_str()),
                             "url": v.as_ref().and_then(|v| v["html_url"].as_str())
                                 .or(p["html_url"].as_str()),
                             "connection": c.conn().id }),
                )
                .await?;
            n += 1;
        }
        if first || before != after {
            set_cursor(c, &ck, repo, &serde_json::to_string(&after)?).await?;
        }
    }
    Ok(n)
}

/// A repository's security posture, as the poller last saw it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct Posture {
    pub visibility: String,
    pub secret_scanning: String,
    pub push_protection: String,
    /// The default branch is protected (None: unknown).
    pub protected: Option<bool>,
}

/// The posture in a repository JSON and its default branch JSON.
pub fn posture_of(repo: &Value, branch: Option<&Value>) -> Posture {
    let sa = &repo["security_and_analysis"];
    Posture {
        visibility: repo["visibility"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                repo["private"]
                    .as_bool()
                    .map(|p| if p { "private" } else { "public" }.to_string())
            })
            .unwrap_or_default(),
        secret_scanning: sa["secret_scanning"]["status"]
            .as_str()
            .unwrap_or("")
            .into(),
        push_protection: sa["secret_scanning_push_protection"]["status"]
            .as_str()
            .unwrap_or("")
            .into(),
        protected: branch.and_then(|b| b["protected"].as_bool()),
    }
}

/// `github-posture` rules: what got weaker between two snapshots
/// (rule, severity, summary).
pub fn posture_rules(
    before: &Posture,
    after: &Posture,
) -> Vec<(&'static str, &'static str, String)> {
    let mut out = Vec::new();
    if before.visibility != "public"
        && !before.visibility.is_empty()
        && after.visibility == "public"
    {
        out.push((
            "repo-made-public",
            "high",
            format!("visibility {} -> public", before.visibility),
        ));
    }
    if before.secret_scanning == "enabled" && after.secret_scanning == "disabled" {
        out.push((
            "secret-scanning-off",
            "high",
            "secret scanning disabled".into(),
        ));
    }
    if before.push_protection == "enabled" && after.push_protection == "disabled" {
        out.push((
            "push-protection-off",
            "medium",
            "secret push protection disabled".into(),
        ));
    }
    if before.protected == Some(true) && after.protected == Some(false) {
        out.push((
            "branch-protection-removed",
            "high",
            "default branch protection removed".into(),
        ));
    }
    out
}

/// Reads the repository and its default branch (conditional), compares the
/// posture with the last snapshot and emits a `github.risk` for each rule
/// that fires. The first read only stores the snapshot.
async fn posture(c: &Client, r: &GhRepo, caller: &str) -> Result<usize> {
    let repo = r.full_name.as_str();
    let fr = c.forge_name(repo);
    let Ok((rv, _)) = quiet_pair(c.api().repo(c, repo, caller).await)? else {
        return Ok(0);
    };
    let branch = if r.default_branch.is_empty() {
        None
    } else {
        match c
            .get_json(
                &format!(
                    "/repos/{fr}/branches/{}",
                    crate::http::encode_component(&r.default_branch)
                ),
                caller,
            )
            .await
        {
            Ok(v) => Some(v),
            Err(e) if absent(&e) => None,
            Err(e) => return Err(e),
        }
    };
    let after = posture_of(&rv, branch.as_ref());
    let before: Option<Posture> = cursor(c, "posture", repo)
        .await?
        .and_then(|s| serde_json::from_str(&s).ok());
    let mut n = 0;
    if let Some(before) = &before {
        for (rule, severity, summary) in posture_rules(before, &after) {
            c.core()
                .emit(
                    kinds::RISK,
                    &format!("{repo}#{rule}"),
                    &json!({ "source": "github-posture", "repo": repo, "rule": rule,
                             "severity": severity, "summary": summary,
                             "before": before, "after": after, "connection": c.conn().id }),
                )
                .await?;
            n += 1;
        }
    }
    if before.as_ref() != Some(&after) {
        set_cursor(c, "posture", repo, &serde_json::to_string(&after)?).await?;
    }
    Ok(n)
}

fn quiet_pair(r: Result<(Value, bool)>) -> Result<Result<(Value, bool)>> {
    match r {
        Err(e) if absent(&e) => Ok(Err(e)),
        Err(e) => Err(e),
        Ok(v) => Ok(Ok(v)),
    }
}

/// `github-risk` rules over org audit-log actions: (rule, severity).
pub fn audit_rule(action: &str) -> Option<(&'static str, &'static str)> {
    Some(match action {
        "org.disable_two_factor_requirement" => ("two-factor-off", "critical"),
        "repo.access" => ("repo-visibility-changed", "high"),
        "protected_branch.destroy" => ("branch-protection-removed", "high"),
        "protected_branch.policy_override" => ("branch-protection-bypassed", "high"),
        "repository_secret_scanning.disable" | "secret_scanning.disable" => {
            ("secret-scanning-off", "high")
        }
        "repository_secret_scanning_push_protection.disable" => ("push-protection-off", "medium"),
        "repo.destroy" => ("repo-deleted", "high"),
        "repo.transfer" | "repo.transfer_outgoing" => ("repo-transferred", "high"),
        "org.update_member" => ("member-role-changed", "medium"),
        "org.add_member" => ("member-added", "low"),
        "hook.create" => ("webhook-added", "medium"),
        "integration_installation.create" => ("app-installed", "medium"),
        "org.update_default_repository_permission" => ("default-permission-changed", "medium"),
        "org_credential_authorization.grant" => ("credential-authorized", "low"),
        _ => return None,
    })
}

/// Org audit log read at most this often.
pub const AUDIT_LOG_MS: i64 = 60 * 60_000;

/// `github-risk`: the org's audit log (GitHub Enterprise Cloud; elsewhere
/// absent and skipped quietly) at most hourly, entries newer than the
/// cursor matched against [`audit_rule`]. The first read only sets the cursor.
pub async fn poll_org_risk(c: &Client, org: &str, caller: &str) -> Result<usize> {
    if c.conn().dialect() != Dialect::GitHub || !risk_on(c).await {
        return Ok(0);
    }
    let now = kls_store::now_ms();
    let checked = cursor(c, "risk:checked", org)
        .await?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if now - checked < AUDIT_LOG_MS {
        return Ok(0);
    }
    set_cursor(c, "risk:checked", org, &now.to_string()).await?;
    let login = org.rsplit('/').next().unwrap_or(org);
    let list = quiet(
        c.get_all(
            &format!("/orgs/{login}/audit-log?per_page=100&order=desc"),
            caller,
        )
        .await,
    )?;
    let at = |e: &Value| {
        e["@timestamp"]
            .as_i64()
            .or_else(|| e["created_at"].as_i64())
    };
    let Some(max) = list.items.iter().filter_map(at).max() else {
        return Ok(0);
    };
    let last = cursor(c, "risk", org)
        .await?
        .and_then(|s| s.parse::<i64>().ok());
    let mut n = 0;
    if let Some(last) = last {
        let mut new: Vec<&Value> = list
            .items
            .iter()
            .filter(|e| at(e).is_some_and(|t| t > last))
            .collect();
        new.sort_by_key(|e| at(e));
        for e in new {
            let action = e["action"].as_str().unwrap_or("");
            let Some((rule, severity)) = audit_rule(action) else {
                continue;
            };
            c.core()
                .emit(
                    kinds::RISK,
                    &format!("{org}#{rule}"),
                    &json!({ "source": "github-risk", "org": org, "rule": rule,
                             "severity": severity, "action": action,
                             "actor": e["actor"], "repo": e["repo"], "user": e["user"],
                             "at": at(e), "summary": format!("{action} by {}", e["actor"].as_str().unwrap_or("?")),
                             "connection": c.conn().id }),
                )
                .await?;
            n += 1;
        }
    }
    if last != Some(max) {
        set_cursor(c, "risk", org, &max.to_string()).await?;
    }
    Ok(n)
}

/// `/sf <name> [args]` on a comment's first command line: `Some("name")`.
pub fn sf_command(body: &str) -> Option<String> {
    body.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("/sf ")?;
        let name = rest.split_whitespace().next()?;
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
            .then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn sf_commands() {
        assert_eq!(
            super::sf_command("please\n/sf fix now").as_deref(),
            Some("fix")
        );
        assert_eq!(super::sf_command("/sf sf:review"), Some("sf:review".into()));
        assert_eq!(super::sf_command("/sfx"), None);
        assert_eq!(super::sf_command("/sf $(rm)"), None);
    }
}
