//! What the factory needs from a forge, in one shape. Mirrors vdb-site
//! `forge.Client`.
//!
//! Every method answers in GitHub's REST shapes (a GitLab merge request
//! comes back as a GitHub pull request JSON, iid as `number`), so [`sync`],
//! the poller, the engine and the UI keep one code path and the store keeps
//! the rows it already holds. Repositories are named by their store key;
//! each dialect turns the key into its own path.
//!
//! [`sync`]: crate::sync

use super::cap::{self, Support};
use super::kind::Dialect;
use crate::GhError;
use crate::client::{Client, Paged};
use crate::http::encode_path;
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde_json::{Value, json};

/// A pull request to open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PullIn {
    pub title: String,
    pub head: String,
    pub base: String,
    pub body: String,
    pub draft: bool,
}

/// A commit status (what the engine's `check_run` writes on every forge).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusIn {
    /// success, failure, error, pending.
    pub state: String,
    pub context: String,
    pub description: String,
    pub target_url: String,
}

/// Fields to change on an issue; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `open` or `closed`.
    pub state: Option<String>,
    /// The whole label set.
    pub labels: Option<Vec<String>>,
}

impl IssueEdit {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.body.is_none() && self.state.is_none() && self.labels.is_none()
    }

    /// GitHub's PATCH body.
    pub fn github_json(&self) -> Value {
        let mut b = json!({});
        if let Some(t) = &self.title {
            b["title"] = json!(t);
        }
        if let Some(t) = &self.body {
            b["body"] = json!(t);
        }
        if let Some(t) = &self.state {
            b["state"] = json!(t);
        }
        if let Some(l) = &self.labels {
            b["labels"] = json!(l);
        }
        b
    }
}

/// A review comment on one line of a pull request's diff (the new side).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct InlineComment {
    pub path: String,
    pub line: i64,
    pub body: String,
}

/// The inline comments as lines under a review body (forges without them).
pub fn fold_inline(body: &str, comments: &[InlineComment]) -> String {
    if comments.is_empty() {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    out.push_str("\n\n");
    for c in comments {
        out.push_str(&format!("- `{}:{}` {}\n", c.path, c.line, c.body));
    }
    out
}

/// Whether an issue JSON carries a label by name.
pub fn has_label(issue: &Value, label: &str) -> bool {
    issue["labels"].as_array().is_some_and(|a| {
        a.iter()
            .any(|l| l["name"].as_str().or(l.as_str()) == Some(label))
    })
}

/// [`GhError::Unsupported`] for `what` on this connection's forge.
pub fn unsupported(c: &Client, what: &str) -> anyhow::Error {
    anyhow!(GhError::Unsupported(format!(
        "{what} on {}",
        c.conn().kind.id()
    )))
}

/// GitHub's commit-status state for a check-run conclusion.
pub fn status_state(conclusion: &str) -> &'static str {
    match conclusion {
        "success" | "neutral" | "skipped" => "success",
        "pending" | "queued" | "in_progress" => "pending",
        "cancelled" | "timed_out" | "action_required" | "stale" => "error",
        _ => "failure",
    }
}

/// Is this a 404 (or 403: feature off / no access)?
pub fn absent(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<GhError>(),
        Some(GhError::Http {
            status: 403 | 404 | 410,
            ..
        })
    )
}

/// Commit statuses (`state`, `context`) as check runs.
pub fn statuses_as_checks(statuses: &[Value]) -> Vec<Value> {
    statuses
        .iter()
        .map(|s| {
            let st = s["state"].as_str().unwrap_or("");
            let (status, conclusion) = match st {
                "pending" | "running" | "created" | "waiting_for_resource" | "preparing" => {
                    ("in_progress", "")
                }
                "success" => ("completed", "success"),
                "skipped" => ("completed", "skipped"),
                "manual" => ("completed", "neutral"),
                "canceled" | "cancelled" => ("completed", "cancelled"),
                "warning" => ("completed", "neutral"),
                _ => ("completed", "failure"),
            };
            json!({
                "name": s.get("context").or_else(|| s.get("name")).cloned().unwrap_or(json!("status")),
                "status": status, "conclusion": conclusion,
            })
        })
        .collect()
}

#[async_trait::async_trait]
pub trait ForgeApi: Send + Sync {
    fn dialect(&self) -> Dialect;

    /// The signed-in account: `{login, avatar_url, id}`.
    async fn viewer(&self, c: &Client, caller: &str) -> Result<Value>;
    /// Orgs or groups the account can write to: `[{login, avatar_url, description}]`.
    async fn accounts(&self, c: &Client, caller: &str) -> Result<Vec<Value>>;
    /// Repositories of an account (its forge login); `user` for the account itself.
    async fn account_repos(
        &self,
        c: &Client,
        forge_login: &str,
        user: bool,
        caller: &str,
    ) -> Result<Paged>;
    /// One repository and whether it was unchanged (304).
    async fn repo(&self, c: &Client, key: &str, caller: &str) -> Result<(Value, bool)>;
    async fn open_pulls(&self, c: &Client, key: &str, caller: &str) -> Result<Paged>;
    /// Pulls in any state updated at or after `since` (Unix ms), for a
    /// cursored poll (forges.md 4, "Polling"). `None`: this forge lists its
    /// open pulls in full every poll (GitHub, where ETag 304s make an idle
    /// poll free).
    async fn pulls_since(
        &self,
        _c: &Client,
        _key: &str,
        _since: i64,
        _caller: &str,
    ) -> Result<Option<Paged>> {
        Ok(None)
    }
    /// A pull in detail (has `mergeable`).
    async fn pull(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Value>;
    /// Check runs and statuses of a commit, as check runs.
    async fn checks(&self, c: &Client, key: &str, sha: &str, caller: &str) -> Result<Vec<Value>>;
    async fn reviews(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<Value>>;
    async fn issues_since(
        &self,
        c: &Client,
        key: &str,
        since: Option<i64>,
        caller: &str,
    ) -> Result<Paged>;
    async fn labels(&self, c: &Client, key: &str, caller: &str) -> Result<Paged>;
    /// `None` when the label already exists.
    async fn create_label(
        &self,
        c: &Client,
        key: &str,
        name: &str,
        color: &str,
        desc: &str,
        caller: &str,
    ) -> Result<Option<Value>>;
    async fn set_draft(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        draft: bool,
        caller: &str,
    ) -> Result<()>;
    async fn update_branch(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        expected_sha: &str,
        caller: &str,
    ) -> Result<()>;
    async fn request_reviewers(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        users: &[String],
        teams: &[String],
        caller: &str,
    ) -> Result<()>;
    async fn merge(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        method: &str,
        sha: &str,
        caller: &str,
    ) -> Result<()>;
    /// `[{sha, commit: {message, author: {name, email, date}}, author: {login}}]`.
    async fn pull_commits(&self, c: &Client, key: &str, n: i64, caller: &str)
    -> Result<Vec<Value>>;
    /// `{number, html_url}`.
    async fn create_pull(&self, c: &Client, key: &str, p: &PullIn, caller: &str) -> Result<Value>;
    /// `{id, html_url}`.
    async fn comment(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        body: &str,
        caller: &str,
    ) -> Result<Value>;
    /// `event`: COMMENT or REQUEST_CHANGES (the factory never approves). `{id}`.
    async fn review(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        event: &str,
        body: &str,
        caller: &str,
    ) -> Result<Value>;
    async fn add_labels(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        labels: &[String],
        caller: &str,
    ) -> Result<()>;
    /// `{number, html_url}`.
    async fn create_issue(
        &self,
        c: &Client,
        key: &str,
        title: &str,
        body: &str,
        labels: &[String],
        caller: &str,
    ) -> Result<Value>;
    async fn set_status(
        &self,
        c: &Client,
        key: &str,
        sha: &str,
        s: &StatusIn,
        caller: &str,
    ) -> Result<Value>;
    /// `{merge_base_commit: {sha}, files: [{filename, previous_filename, status, sha}]}`.
    async fn compare(
        &self,
        c: &Client,
        key: &str,
        base: &str,
        head: &str,
        caller: &str,
    ) -> Result<Value>;
    /// A file's text at a commit; `(None, false)` when absent, `(None, true)` when binary.
    async fn file_at(
        &self,
        c: &Client,
        key: &str,
        path: &str,
        sha: &str,
        caller: &str,
    ) -> Result<(Option<String>, bool)>;
    /// The newest commits of a branch: `[{sha, commit: {message}}]`.
    async fn branch_commits(
        &self,
        c: &Client,
        key: &str,
        branch: &str,
        caller: &str,
    ) -> Result<Paged>;
    /// Finished CI runs, newest first:
    /// `[{id, name, conclusion, head_branch, head_sha, html_url}]`.
    async fn ci_runs(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>>;
    /// Published releases, newest first: `[{id, tag_name, name, html_url}]`.
    async fn releases(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>>;
    /// Open security alerts: `(kind, {number, html_url, severity, summary})`.
    async fn alerts(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<(String, Value)>>;
    /// The API path a pull's cached reads live under (dropped after a write).
    fn pull_path(&self, c: &Client, key: &str, n: i64) -> String;

    /// Paths a pull changes (at most 300).
    async fn pull_files(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<String>>;

    /// GitHub's `author_association` for `login` on a forge whose payloads
    /// carry none (`MEMBER`, `COLLABORATOR`, `CONTRIBUTOR`, `NONE`; "" unknown).
    async fn permission(
        &self,
        _c: &Client,
        _key: &str,
        _login: &str,
        _caller: &str,
    ) -> Result<String> {
        Ok(String::new())
    }

    /// Who added each label to issue or pull `n` (latest add wins): label -> login.
    async fn label_actors(
        &self,
        _c: &Client,
        _key: &str,
        _n: i64,
        _is_pull: bool,
        _caller: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        Ok(Default::default())
    }

    /// Issue and pull comments created after `since` (Unix ms), oldest first:
    /// `[{id, number, body, author, author_association, html_url, created_at}]`.
    async fn comments_since(
        &self,
        _c: &Client,
        _key: &str,
        _since: i64,
        _caller: &str,
    ) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }

    /// Changes an issue's title, body, state (`open` / `closed`) or labels
    /// (the whole set). A forge without the write says so ([`GhError::Unsupported`]).
    async fn update_issue(
        &self,
        c: &Client,
        _key: &str,
        _n: i64,
        _e: &IssueEdit,
        _caller: &str,
    ) -> Result<()> {
        Err(unsupported(c, "issue updates"))
    }

    /// Issues (not pull requests) carrying `label`, newest first, at most 500.
    async fn issues_labelled(
        &self,
        c: &Client,
        key: &str,
        label: &str,
        caller: &str,
    ) -> Result<Vec<Value>> {
        let p = self.issues_since(c, key, None, caller).await?;
        let mut out: Vec<Value> = p
            .items
            .into_iter()
            .filter(|i| i.get("pull_request").is_none_or(Value::is_null))
            .filter(|i| has_label(i, label))
            .collect();
        out.reverse();
        out.truncate(500);
        Ok(out)
    }

    /// A review with comments on diff lines. A forge without inline review
    /// comments gets them folded into the review body (one line each).
    #[allow(clippy::too_many_arguments)]
    async fn review_inline(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        event: &str,
        body: &str,
        comments: &[InlineComment],
        _commit: &str,
        caller: &str,
    ) -> Result<Value> {
        self.review(c, key, n, event, &fold_inline(body, comments), caller)
            .await
    }

    /// An approving review on head `sha` (only `approve_ready` posts one,
    /// after its readiness rules). `{id}`.
    async fn approve(
        &self,
        c: &Client,
        _key: &str,
        _n: i64,
        _body: &str,
        _sha: &str,
        _caller: &str,
    ) -> Result<Value> {
        Err(unsupported(c, "approving reviews"))
    }

    /// Paths a pull changes with each file's unified patch when the forge
    /// sends one (`None`: lines unknown), at most 300.
    async fn pull_patches(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        caller: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        Ok(self
            .pull_files(c, key, n, caller)
            .await?
            .into_iter()
            .map(|p| (p, None))
            .collect())
    }

    /// Whether the forge protects `branch` (`None` when it cannot say).
    /// GitHub and Gitea share the path and the `protected` field.
    async fn branch_protected(
        &self,
        c: &Client,
        key: &str,
        branch: &str,
        caller: &str,
    ) -> Result<Option<bool>> {
        let path = format!(
            "/repos/{}/branches/{}",
            c.forge_name(key),
            crate::http::encode_path(branch)
        );
        match c.get_json(&path, caller).await {
            Ok(v) => Ok(v.get("protected").and_then(Value::as_bool)),
            Err(e) if absent(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// What this forge can do (the capability table, forges.md 5).
    fn supports(&self, c: &Client, cap_id: &str) -> Support {
        cap::support(c.conn().kind, cap_id)
    }
}

pub fn for_dialect(d: Dialect) -> &'static dyn ForgeApi {
    match d {
        Dialect::GitHub => &GitHubApi,
        Dialect::GitLab => &super::gitlab::GitLabApi,
        Dialect::Gitea => &super::gitea::GiteaApi,
    }
}

/// Decodes a contents-API answer (`encoding: base64`).
pub fn decode_contents(v: &Value) -> Result<(Option<String>, bool)> {
    let enc = v["encoding"].as_str().unwrap_or("");
    let raw = v["content"].as_str().unwrap_or("");
    if enc != "base64" {
        // "none": larger than 1 MB; the contents API does not send it.
        return Ok((None, true));
    }
    let clean: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(clean)
        .context("file content is not base64")?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) if !s.contains('\0') => (Some(s), false),
        _ => (None, true),
    })
}

/// GitHub and GitHub Enterprise (today's paths; the key is `owner/name`
/// on github.com and `host/owner/name` on GHES).
pub struct GitHubApi;

#[async_trait::async_trait]
impl ForgeApi for GitHubApi {
    fn dialect(&self) -> Dialect {
        Dialect::GitHub
    }

    async fn viewer(&self, c: &Client, caller: &str) -> Result<Value> {
        c.get_json("/user", caller).await
    }

    async fn accounts(&self, c: &Client, caller: &str) -> Result<Vec<Value>> {
        Ok(c.get_all("/user/orgs?per_page=100", caller).await?.items)
    }

    async fn account_repos(
        &self,
        c: &Client,
        forge_login: &str,
        user: bool,
        caller: &str,
    ) -> Result<Paged> {
        let path = if user {
            "/user/repos?affiliation=owner&per_page=100".to_string()
        } else {
            format!("/orgs/{forge_login}/repos?type=all&per_page=100")
        };
        c.get_all(&path, caller).await
    }

    async fn repo(&self, c: &Client, key: &str, caller: &str) -> Result<(Value, bool)> {
        let r = c
            .get(&format!("/repos/{}", c.forge_name(key)), caller)
            .await?;
        Ok((r.json()?, r.not_modified))
    }

    async fn open_pulls(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        c.get_all(
            &format!("/repos/{}/pulls?state=open&per_page=100", c.forge_name(key)),
            caller,
        )
        .await
    }

    async fn pull(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Value> {
        c.get_json(&format!("/repos/{}/pulls/{n}", c.forge_name(key)), caller)
            .await
    }

    async fn checks(&self, c: &Client, key: &str, sha: &str, caller: &str) -> Result<Vec<Value>> {
        let r = c.forge_name(key);
        let mut runs = c
            .get_all(
                &format!("/repos/{r}/commits/{sha}/check-runs?per_page=100"),
                caller,
            )
            .await?
            .items;
        // Commit statuses too (the factory's own `check_run` writes one).
        match c
            .get_json(&format!("/repos/{r}/commits/{sha}/status"), caller)
            .await
        {
            Ok(v) => runs.extend(statuses_as_checks(
                v["statuses"].as_array().map(Vec::as_slice).unwrap_or(&[]),
            )),
            Err(e) if absent(&e) => {}
            Err(e) => return Err(e),
        }
        Ok(runs)
    }

    async fn reviews(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<Value>> {
        Ok(c.get_all(
            &format!(
                "/repos/{}/pulls/{n}/reviews?per_page=100",
                c.forge_name(key)
            ),
            caller,
        )
        .await?
        .items)
    }

    async fn issues_since(
        &self,
        c: &Client,
        key: &str,
        since: Option<i64>,
        caller: &str,
    ) -> Result<Paged> {
        let mut path = format!(
            "/repos/{}/issues?state=all&sort=updated&direction=asc&per_page=100",
            c.forge_name(key)
        );
        if let Some(since) = since {
            path.push_str(&format!("&since={}", crate::parse::iso(since)));
        }
        c.get_all(&path, caller).await
    }

    async fn labels(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        c.get_all(
            &format!("/repos/{}/labels?per_page=100", c.forge_name(key)),
            caller,
        )
        .await
    }

    async fn create_label(
        &self,
        c: &Client,
        key: &str,
        name: &str,
        color: &str,
        desc: &str,
        caller: &str,
    ) -> Result<Option<Value>> {
        let body = json!({ "name": name, "color": color, "description": desc });
        match c
            .request(
                "POST",
                &format!("/repos/{}/labels", c.forge_name(key)),
                Some(&body),
                caller,
            )
            .await
        {
            Ok(r) => Ok(Some(r.json()?)),
            Err(e)
                if matches!(
                    e.downcast_ref::<GhError>(),
                    Some(GhError::Http { status: 422, .. })
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn set_draft(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        draft: bool,
        caller: &str,
    ) -> Result<()> {
        // `gh pr ready` (the REST API cannot convert to draft). On GHES the
        // repository is named with its host.
        let repo = if c.conn().is_github_com() {
            key.to_string()
        } else {
            format!("{}/{}", c.conn().host, c.forge_name(key))
        };
        let n = n.to_string();
        let mut a = crate::args(["pr", "ready", &n]);
        if draft {
            a.push("--undo".into());
        }
        a.extend(crate::args(["-R", &repo]));
        c.cli(a, caller).await?;
        Ok(())
    }

    async fn update_branch(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        expected_sha: &str,
        caller: &str,
    ) -> Result<()> {
        let mut body = json!({});
        if !expected_sha.is_empty() {
            body["expected_head_sha"] = json!(expected_sha);
        }
        c.request(
            "PUT",
            &format!("/repos/{}/pulls/{n}/update-branch", c.forge_name(key)),
            Some(&body),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn request_reviewers(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        users: &[String],
        teams: &[String],
        caller: &str,
    ) -> Result<()> {
        c.request(
            "POST",
            &format!("/repos/{}/pulls/{n}/requested_reviewers", c.forge_name(key)),
            Some(&json!({ "reviewers": users, "team_reviewers": teams })),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn merge(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        method: &str,
        sha: &str,
        caller: &str,
    ) -> Result<()> {
        let mut body = json!({ "merge_method": method });
        if !sha.is_empty() {
            body["sha"] = json!(sha);
        }
        c.request(
            "PUT",
            &format!("/repos/{}/pulls/{n}/merge", c.forge_name(key)),
            Some(&body),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn pull_commits(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        caller: &str,
    ) -> Result<Vec<Value>> {
        Ok(c.get_all(
            &format!(
                "/repos/{}/pulls/{n}/commits?per_page=100",
                c.forge_name(key)
            ),
            caller,
        )
        .await?
        .items)
    }

    async fn create_pull(&self, c: &Client, key: &str, p: &PullIn, caller: &str) -> Result<Value> {
        let r = c.forge_name(key);
        let v = c
            .request(
                "POST",
                &format!("/repos/{r}/pulls"),
                Some(&json!({
                    "title": p.title, "head": p.head, "base": p.base,
                    "body": p.body, "draft": p.draft,
                })),
                caller,
            )
            .await?
            .json()?;
        c.invalidate(&format!("/repos/{r}/pulls")).await?;
        Ok(v)
    }

    async fn comment(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        body: &str,
        caller: &str,
    ) -> Result<Value> {
        c.request(
            "POST",
            &format!("/repos/{}/issues/{n}/comments", c.forge_name(key)),
            Some(&json!({ "body": body })),
            caller,
        )
        .await?
        .json()
    }

    async fn review(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        event: &str,
        body: &str,
        caller: &str,
    ) -> Result<Value> {
        let r = c.forge_name(key);
        let v = c
            .request(
                "POST",
                &format!("/repos/{r}/pulls/{n}/reviews"),
                Some(&json!({ "body": body, "event": event })),
                caller,
            )
            .await?
            .json()?;
        c.invalidate(&format!("/repos/{r}/pulls/{n}")).await?;
        Ok(v)
    }

    async fn add_labels(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        labels: &[String],
        caller: &str,
    ) -> Result<()> {
        c.request(
            "POST",
            &format!("/repos/{}/issues/{n}/labels", c.forge_name(key)),
            Some(&json!({ "labels": labels })),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn create_issue(
        &self,
        c: &Client,
        key: &str,
        title: &str,
        body: &str,
        labels: &[String],
        caller: &str,
    ) -> Result<Value> {
        c.request(
            "POST",
            &format!("/repos/{}/issues", c.forge_name(key)),
            Some(&json!({ "title": title, "body": body, "labels": labels })),
            caller,
        )
        .await?
        .json()
    }

    async fn set_status(
        &self,
        c: &Client,
        key: &str,
        sha: &str,
        s: &StatusIn,
        caller: &str,
    ) -> Result<Value> {
        let mut body = json!({ "state": s.state, "context": s.context, "description": trim140(&s.description) });
        if !s.target_url.is_empty() {
            body["target_url"] = json!(s.target_url);
        }
        c.request(
            "POST",
            &format!("/repos/{}/statuses/{sha}", c.forge_name(key)),
            Some(&body),
            caller,
        )
        .await?
        .json()
    }

    async fn compare(
        &self,
        c: &Client,
        key: &str,
        base: &str,
        head: &str,
        caller: &str,
    ) -> Result<Value> {
        c.get_json(
            &format!("/repos/{}/compare/{base}...{head}", c.forge_name(key)),
            caller,
        )
        .await
    }

    async fn file_at(
        &self,
        c: &Client,
        key: &str,
        path: &str,
        sha: &str,
        caller: &str,
    ) -> Result<(Option<String>, bool)> {
        match c
            .get_json(
                &format!(
                    "/repos/{}/contents/{}?ref={sha}",
                    c.forge_name(key),
                    encode_path(path)
                ),
                caller,
            )
            .await
        {
            Ok(v) => decode_contents(&v),
            Err(e)
                if matches!(
                    e.downcast_ref::<GhError>(),
                    Some(GhError::Http { status: 404, .. })
                ) =>
            {
                Ok((None, false))
            }
            Err(e) => Err(e),
        }
    }

    async fn branch_commits(
        &self,
        c: &Client,
        key: &str,
        branch: &str,
        caller: &str,
    ) -> Result<Paged> {
        let r = c
            .get(
                &format!(
                    "/repos/{}/commits?sha={}&per_page=20",
                    c.forge_name(key),
                    encode_path(branch)
                ),
                caller,
            )
            .await?;
        Ok(Paged {
            not_modified: r.not_modified,
            items: r.json()?.as_array().cloned().unwrap_or_default(),
            pages: 1,
        })
    }

    async fn ci_runs(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(
                &format!(
                    "/repos/{}/actions/runs?status=completed&per_page=20",
                    c.forge_name(key)
                ),
                caller,
            )
            .await?;
        Ok(v["workflow_runs"].as_array().cloned().unwrap_or_default())
    }

    async fn releases(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(
                &format!("/repos/{}/releases?per_page=10", c.forge_name(key)),
                caller,
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| !r["draft"].as_bool().unwrap_or(false))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn alerts(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<(String, Value)>> {
        let r = c.forge_name(key);
        let mut out = Vec::new();
        for (kind, path) in [
            (
                "dependabot",
                format!("/repos/{r}/dependabot/alerts?state=open&per_page=50"),
            ),
            (
                "code_scanning",
                format!("/repos/{r}/code-scanning/alerts?state=open&per_page=50"),
            ),
            (
                "secret_scanning",
                format!("/repos/{r}/secret-scanning/alerts?state=open&per_page=50"),
            ),
        ] {
            match c.get_json(&path, caller).await {
                Ok(v) => {
                    for a in v.as_array().cloned().unwrap_or_default() {
                        let severity = a["security_advisory"]["severity"]
                            .as_str()
                            .or_else(|| a["rule"]["security_severity_level"].as_str())
                            .or_else(|| a["rule"]["severity"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let summary = a["security_advisory"]["summary"]
                            .as_str()
                            .or_else(|| a["rule"]["description"].as_str())
                            .or_else(|| a["secret_type_display_name"].as_str())
                            .unwrap_or("")
                            .to_string();
                        out.push((
                            kind.to_string(),
                            json!({ "number": a["number"], "html_url": a["html_url"],
                                    "severity": severity, "summary": summary }),
                        ));
                    }
                }
                // Off for this repository, or the token cannot see alerts.
                Err(e) if absent(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn pull_path(&self, c: &Client, key: &str, n: i64) -> String {
        format!("/repos/{}/pulls/{n}", c.forge_name(key))
    }

    async fn pull_files(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<String>> {
        let p = c
            .get_all(
                &format!("/repos/{}/pulls/{n}/files?per_page=100", c.forge_name(key)),
                caller,
            )
            .await?;
        Ok(file_names(&p.items, "filename"))
    }

    async fn pull_patches(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        caller: &str,
    ) -> Result<Vec<(String, Option<String>)>> {
        let p = c
            .get_all(
                &format!("/repos/{}/pulls/{n}/files?per_page=100", c.forge_name(key)),
                caller,
            )
            .await?;
        Ok(p.items
            .iter()
            .filter_map(|f| {
                Some((
                    f["filename"].as_str()?.to_string(),
                    f["patch"].as_str().map(String::from),
                ))
            })
            .take(300)
            .collect())
    }

    async fn label_actors(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        _is_pull: bool,
        caller: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let p = c
            .get_all(
                &format!(
                    "/repos/{}/issues/{n}/events?per_page=100",
                    c.forge_name(key)
                ),
                caller,
            )
            .await?;
        Ok(label_adds(&p.items, |e| {
            (e["event"] == "labeled").then(|| {
                (
                    e["label"]["name"].as_str().unwrap_or("").to_string(),
                    e["actor"]["login"].as_str().unwrap_or("").to_string(),
                )
            })
        }))
    }

    async fn update_issue(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        e: &IssueEdit,
        caller: &str,
    ) -> Result<()> {
        let path = format!("/repos/{}/issues/{n}", c.forge_name(key));
        c.request("PATCH", &path, Some(&e.github_json()), caller)
            .await?;
        c.invalidate(&path).await?;
        Ok(())
    }

    async fn issues_labelled(
        &self,
        c: &Client,
        key: &str,
        label: &str,
        caller: &str,
    ) -> Result<Vec<Value>> {
        let p = c
            .get_all(
                &format!(
                    "/repos/{}/issues?labels={}&state=all&sort=created&direction=desc&per_page=100",
                    c.forge_name(key),
                    crate::http::encode_component(label)
                ),
                caller,
            )
            .await?;
        Ok(p.items
            .into_iter()
            .filter(|i| i.get("pull_request").is_none_or(Value::is_null))
            .take(500)
            .collect())
    }

    async fn review_inline(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        event: &str,
        body: &str,
        comments: &[InlineComment],
        commit: &str,
        caller: &str,
    ) -> Result<Value> {
        let r = c.forge_name(key);
        let mut b = json!({ "body": body, "event": event });
        if !commit.is_empty() {
            b["commit_id"] = json!(commit);
        }
        if !comments.is_empty() {
            b["comments"] = json!(
                comments
                    .iter()
                    .map(
                        |x| json!({"path": x.path, "line": x.line, "side": "RIGHT", "body": x.body})
                    )
                    .collect::<Vec<_>>()
            );
        }
        let v = c
            .request(
                "POST",
                &format!("/repos/{r}/pulls/{n}/reviews"),
                Some(&b),
                caller,
            )
            .await?
            .json()?;
        c.invalidate(&format!("/repos/{r}/pulls/{n}")).await?;
        Ok(v)
    }

    async fn approve(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        body: &str,
        sha: &str,
        caller: &str,
    ) -> Result<Value> {
        let r = c.forge_name(key);
        let v = c
            .request(
                "POST",
                &format!("/repos/{r}/pulls/{n}/reviews"),
                Some(&json!({ "body": body, "event": "APPROVE", "commit_id": sha })),
                caller,
            )
            .await?
            .json()?;
        c.invalidate(&format!("/repos/{r}/pulls/{n}")).await?;
        Ok(v)
    }

    async fn comments_since(
        &self,
        c: &Client,
        key: &str,
        since: i64,
        caller: &str,
    ) -> Result<Vec<Value>> {
        let p = c
            .get_all(
                &format!(
                    "/repos/{}/issues/comments?sort=created&direction=asc&per_page=100&since={}",
                    c.forge_name(key),
                    crate::parse::iso(since)
                ),
                caller,
            )
            .await?;
        Ok(p.items
            .iter()
            .map(|v| {
                json!({
                    "id": v["id"], "number": number_of(v["issue_url"].as_str().unwrap_or("")),
                    "body": v["body"], "author": v["user"]["login"],
                    "author_association": v["author_association"],
                    "html_url": v["html_url"], "created_at": v["created_at"],
                })
            })
            .collect())
    }
}

fn trim140(s: &str) -> String {
    s.chars().take(140).collect()
}

/// `Err` naming the capability when the forge cannot do it.
pub fn require(c: &Client, cap_id: &str) -> Result<()> {
    match c.api().supports(c, cap_id) {
        Support::Unavailable(note) => Err(anyhow!(GhError::Unsupported(format!(
            "{cap_id} on {}: {note}",
            c.conn().kind.id()
        )))),
        _ => Ok(()),
    }
}

/// The `field` of each item (file paths), at most 300.
pub fn file_names(items: &[Value], field: &str) -> Vec<String> {
    items
        .iter()
        .filter_map(|f| f[field].as_str().map(String::from))
        .take(300)
        .collect()
}

/// Label -> who added it last, from a forge's issue event list.
pub fn label_adds(
    events: &[Value],
    add: impl Fn(&Value) -> Option<(String, String)>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for e in events {
        if let Some((label, who)) = add(e)
            && !label.is_empty()
        {
            out.insert(label, who);
        }
    }
    out
}

/// `…/issues/12` -> 12.
pub fn number_of(url: &str) -> i64 {
    url.rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
