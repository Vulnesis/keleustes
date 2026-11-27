//! GitLab through `glab api` (paths relative to `/api/v4`), translated into
//! GitHub's shapes. Projects are addressed by their URL-encoded full path
//! (`projects/grp%2Fsub%2Fapp`), merge requests by iid (stored as `number`).

use super::api::{
    ForgeApi, InlineComment, IssueEdit, PullIn, StatusIn, absent, fold_inline, statuses_as_checks,
};
use super::kind::Dialect;
use crate::GhError;
use crate::client::{Client, Paged};
use crate::http::{encode_component, encode_path};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

pub struct GitLabApi;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `projects/<encoded path>` of a key.
pub fn project(c: &Client, key: &str) -> String {
    format!("projects/{}", encode_component(c.forge_name(key)))
}

fn user(v: &Value) -> Value {
    json!({ "login": s(v, "username"), "avatar_url": s(v, "avatar_url"), "id": v.get("id") })
}

/// A project as a GitHub repository.
pub fn repo_json(p: &Value) -> Value {
    let access = p["permissions"]["project_access"]["access_level"]
        .as_i64()
        .unwrap_or(0)
        .max(
            p["permissions"]["group_access"]["access_level"]
                .as_i64()
                .unwrap_or(0),
        );
    json!({
        "full_name": s(p, "path_with_namespace"),
        "id": p.get("id"),
        "visibility": s(p, "visibility"),
        "private": s(p, "visibility") != "public",
        "default_branch": s(p, "default_branch"),
        "archived": p["archived"].as_bool().unwrap_or(false),
        "fork": p.get("forked_from_project").is_some_and(|f| !f.is_null()),
        "html_url": s(p, "web_url"),
        "description": p.get("description").cloned().unwrap_or(json!("")),
        "permissions": { "push": access >= 30, "admin": access >= 40 },
        "pushed_at": p.get("last_activity_at"),
    })
}

/// A merge request as a GitHub pull. `detail` adds the mergeability.
pub fn pull_json(mr: &Value, full: &str, detail: bool) -> Value {
    let state = match s(mr, "state") {
        "opened" => "open",
        _ => "closed",
    };
    let same_project = mr["source_project_id"] == mr["target_project_id"];
    let labels: Vec<Value> = mr["labels"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|l| json!({ "name": l.as_str().or_else(|| l["name"].as_str()).unwrap_or("") }))
                .collect()
        })
        .unwrap_or_default();
    let mut v = json!({
        "number": mr.get("iid"),
        "title": s(mr, "title"),
        "state": state,
        "draft": mr["draft"].as_bool().or(mr["work_in_progress"].as_bool()).unwrap_or(false),
        "merged": s(mr, "state") == "merged",
        "user": user(&mr["author"]),
        "head": {
            "ref": s(mr, "source_branch"),
            "sha": s(mr, "sha"),
            "repo": { "full_name": if same_project { full.to_string() } else { format!("fork:{}", mr["source_project_id"]) } },
        },
        "base": { "ref": s(mr, "target_branch"), "sha": s(&mr["diff_refs"], "start_sha") },
        "requested_reviewers": mr["reviewers"].as_array().map(|a| a.iter().map(user).collect::<Vec<_>>()).unwrap_or_default(),
        "labels": labels,
        "html_url": s(mr, "web_url"),
        "created_at": mr.get("created_at"),
        "updated_at": mr.get("updated_at"),
        "merged_at": mr.get("merged_at"),
        "closed_at": mr.get("closed_at").filter(|x| !x.is_null()).or(mr.get("merged_at")),
    });
    if detail {
        let (m, st) = match s(mr, "detailed_merge_status") {
            "mergeable" => (json!(true), "clean"),
            "broken_status" | "conflict" => (json!(false), "dirty"),
            "checking" | "unchecked" | "preparing" | "approvals_syncing" | "" => {
                if mr["has_conflicts"].as_bool() == Some(true) {
                    (json!(false), "dirty")
                } else {
                    (Value::Null, "unknown")
                }
            }
            _ if mr["has_conflicts"].as_bool() == Some(true) => (json!(false), "dirty"),
            _ => (json!(true), "blocked"),
        };
        v["mergeable"] = m;
        v["mergeable_state"] = json!(st);
        if let Some(n) = mr["changes_count"]
            .as_str()
            .and_then(|c| c.trim_end_matches('+').parse::<i64>().ok())
        {
            v["changed_files"] = json!(n);
        }
    }
    v
}

/// A GitLab issue as a GitHub issue.
pub fn issue_json(i: &Value) -> Value {
    json!({
        "number": i.get("iid"),
        "title": s(i, "title"),
        "state": if s(i, "state") == "opened" { "open" } else { "closed" },
        "state_reason": "",
        "user": user(&i["author"]),
        "author_association": "",
        "labels": i["labels"].as_array().map(|a| a.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>()).unwrap_or_default(),
        "assignees": i["assignees"].as_array().map(|a| a.iter().map(user).collect::<Vec<_>>()).unwrap_or_default(),
        "body": i.get("description").cloned().unwrap_or(json!("")),
        "html_url": s(i, "web_url"),
        "comments": i["user_notes_count"].as_i64().unwrap_or(0),
        "created_at": i.get("created_at"),
        "updated_at": i.get("updated_at"),
        "closed_at": i.get("closed_at"),
    })
}

fn map_paged(p: Paged, f: impl Fn(&Value) -> Value) -> Paged {
    Paged {
        items: p.items.iter().map(f).collect(),
        ..p
    }
}

/// Is `n` a merge request of this repository (as the mirror knows it)?
async fn is_mr(c: &Client, key: &str, n: i64) -> Result<bool> {
    Ok(c.core().store().gh_pull(key, n).await?.is_some())
}

fn mr_path(c: &Client, key: &str, n: i64) -> String {
    format!("{}/merge_requests/{n}", project(c, key))
}

/// `(base_sha, start_sha, head_sha)` of a merge request, when all three are
/// known and the head is the reviewed `commit` (lines of another head would
/// land in the wrong place).
fn diff_refs(mr: &Value, commit: &str) -> Option<(String, String, String)> {
    let r = &mr["diff_refs"];
    let (base, start, head) = (s(r, "base_sha"), s(r, "start_sha"), s(r, "head_sha"));
    if base.is_empty() || start.is_empty() || head.is_empty() {
        return None;
    }
    if !commit.is_empty() && commit != head {
        return None;
    }
    Some((base.into(), start.into(), head.into()))
}

#[async_trait::async_trait]
impl ForgeApi for GitLabApi {
    fn dialect(&self) -> Dialect {
        Dialect::GitLab
    }

    async fn viewer(&self, c: &Client, caller: &str) -> Result<Value> {
        Ok(user(&c.get_json("user", caller).await?))
    }

    async fn accounts(&self, c: &Client, caller: &str) -> Result<Vec<Value>> {
        let g = c
            .get_all("groups?min_access_level=30&per_page=100", caller)
            .await?;
        Ok(g.items
            .iter()
            .map(|g| {
                json!({ "login": s(g, "full_path"), "avatar_url": s(g, "avatar_url"),
                        "description": s(g, "description"), "id": g.get("id") })
            })
            .collect())
    }

    async fn account_repos(
        &self,
        c: &Client,
        forge_login: &str,
        user: bool,
        caller: &str,
    ) -> Result<Paged> {
        let path = if user {
            "projects?owned=true&per_page=100".to_string()
        } else {
            format!(
                "groups/{}/projects?include_subgroups=true&per_page=100",
                encode_component(forge_login)
            )
        };
        Ok(map_paged(c.get_all(&path, caller).await?, repo_json))
    }

    async fn repo(&self, c: &Client, key: &str, caller: &str) -> Result<(Value, bool)> {
        let r = c.get(&project(c, key), caller).await?;
        Ok((repo_json(&r.json()?), r.not_modified))
    }

    async fn open_pulls(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        let full = c.forge_name(key).to_string();
        let p = c
            .get_all(
                &format!(
                    "{}/merge_requests?state=opened&per_page=100",
                    project(c, key)
                ),
                caller,
            )
            .await?;
        Ok(map_paged(p, |m| pull_json(m, &full, false)))
    }

    /// `updated_after` is inclusive, so the newest pull of the last poll comes
    /// back once more (unchanged, so not read again).
    async fn pulls_since(
        &self,
        c: &Client,
        key: &str,
        since: i64,
        caller: &str,
    ) -> Result<Option<Paged>> {
        let full = c.forge_name(key).to_string();
        let p = c
            .get_all(
                &format!(
                    "{}/merge_requests?state=all&updated_after={}&order_by=updated_at&sort=desc&per_page=100",
                    project(c, key),
                    crate::parse::iso(since)
                ),
                caller,
            )
            .await?;
        Ok(Some(map_paged(p, |m| pull_json(m, &full, false))))
    }

    async fn pull(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Value> {
        let full = c.forge_name(key).to_string();
        let mr = c.get_json(&mr_path(c, key, n), caller).await?;
        Ok(pull_json(&mr, &full, true))
    }

    async fn checks(&self, c: &Client, key: &str, sha: &str, caller: &str) -> Result<Vec<Value>> {
        let st = c
            .get_all(
                &format!(
                    "{}/repository/commits/{sha}/statuses?per_page=100",
                    project(c, key)
                ),
                caller,
            )
            .await?;
        Ok(statuses_as_checks(&st.items))
    }

    async fn reviews(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<Value>> {
        let v = match c
            .get_json(&format!("{}/approvals", mr_path(c, key, n)), caller)
            .await
        {
            Ok(v) => v,
            Err(e) if absent(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(v["approved_by"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        json!({ "id": x["user"]["id"], "user": user(&x["user"]), "state": "APPROVED",
                                "submitted_at": v.get("updated_at"), "body": "", "author_association": "" })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn issues_since(
        &self,
        c: &Client,
        key: &str,
        since: Option<i64>,
        caller: &str,
    ) -> Result<Paged> {
        let mut path = format!(
            "{}/issues?scope=all&order_by=updated_at&sort=asc&per_page=100",
            project(c, key)
        );
        if let Some(since) = since {
            path.push_str(&format!("&updated_after={}", crate::parse::iso(since)));
        }
        Ok(map_paged(c.get_all(&path, caller).await?, issue_json))
    }

    async fn labels(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        let p = c
            .get_all(&format!("{}/labels?per_page=100", project(c, key)), caller)
            .await?;
        Ok(map_paged(p, |l| {
            json!({ "name": s(l, "name"), "color": s(l, "color").trim_start_matches('#'),
                    "description": l.get("description").cloned().unwrap_or(json!("")) })
        }))
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
        let body = json!({ "name": name, "color": format!("#{color}"), "description": desc });
        match c
            .request(
                "POST",
                &format!("{}/labels", project(c, key)),
                Some(&body),
                caller,
            )
            .await
        {
            Ok(r) => {
                let l = r.json()?;
                Ok(Some(
                    json!({ "name": s(&l, "name"), "color": s(&l, "color").trim_start_matches('#'), "description": s(&l, "description") }),
                ))
            }
            Err(e)
                if matches!(
                    e.downcast_ref::<GhError>(),
                    Some(GhError::Http {
                        status: 409 | 422,
                        ..
                    })
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
        let mr = c.get_json(&mr_path(c, key, n), caller).await?;
        let title = draft_title(s(&mr, "title"), draft, "Draft: ");
        c.request(
            "PUT",
            &mr_path(c, key, n),
            Some(&json!({ "title": title })),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn update_branch(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        _expected_sha: &str,
        caller: &str,
    ) -> Result<()> {
        c.request(
            "PUT",
            &format!("{}/rebase", mr_path(c, key, n)),
            Some(&json!({})),
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
        if !teams.is_empty() {
            return Err(GhError::Unsupported(
                "team_reviewers on gitlab: GitLab requests users only".into(),
            )
            .into());
        }
        let mut ids = Vec::new();
        for u in users {
            let found = c
                .get_json(&format!("users?username={}", encode_path(u)), caller)
                .await?;
            let id = found
                .as_array()
                .and_then(|a| a.first())
                .and_then(|x| x["id"].as_i64())
                .ok_or_else(|| anyhow!("no GitLab user {u}"))?;
            ids.push(id);
        }
        c.request(
            "PUT",
            &mr_path(c, key, n),
            Some(&json!({ "reviewer_ids": ids })),
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
        if method == "rebase" {
            bail!("GitLab merges or squashes; rebase the branch first (Update branch)");
        }
        let mut body = json!({ "squash": method == "squash" });
        if !sha.is_empty() {
            body["sha"] = json!(sha);
        }
        c.request(
            "PUT",
            &format!("{}/merge", mr_path(c, key, n)),
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
        let p = c
            .get_all(
                &format!("{}/commits?per_page=100", mr_path(c, key, n)),
                caller,
            )
            .await?;
        Ok(p.items
            .iter()
            .map(|cm| {
                json!({
                    "sha": s(cm, "id"),
                    "commit": { "message": s(cm, "message"),
                                "author": { "name": s(cm, "author_name"), "email": s(cm, "author_email"), "date": cm.get("authored_date") },
                                "committer": { "date": cm.get("committed_date") } },
                    "author": { "login": "" },
                })
            })
            .collect())
    }

    async fn create_pull(&self, c: &Client, key: &str, p: &PullIn, caller: &str) -> Result<Value> {
        let full = c.forge_name(key).to_string();
        // One open merge request per source branch: reuse it.
        let existing = c
            .get_json(
                &format!(
                    "{}/merge_requests?state=opened&source_branch={}",
                    project(c, key),
                    encode_path(&p.head)
                ),
                caller,
            )
            .await?;
        if let Some(mr) = existing.as_array().and_then(|a| a.first()) {
            return Ok(pull_json(mr, &full, false));
        }
        let title = draft_title(&p.title, p.draft, "Draft: ");
        let mr = c
            .request(
                "POST",
                &format!("{}/merge_requests", project(c, key)),
                Some(&json!({ "source_branch": p.head, "target_branch": p.base,
                              "title": title, "description": p.body })),
                caller,
            )
            .await?
            .json()?;
        Ok(pull_json(&mr, &full, false))
    }

    async fn comment(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        body: &str,
        caller: &str,
    ) -> Result<Value> {
        let path = if is_mr(c, key, n).await? {
            format!("{}/notes", mr_path(c, key, n))
        } else {
            format!("{}/issues/{n}/notes", project(c, key))
        };
        let v = c
            .request("POST", &path, Some(&json!({ "body": body })), caller)
            .await?
            .json()?;
        Ok(json!({ "id": v.get("id"), "html_url": "" }))
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
        let mr = mr_path(c, key, n);
        let note = c
            .request(
                "POST",
                &format!("{mr}/notes"),
                Some(&json!({ "body": body })),
                caller,
            )
            .await?
            .json()?;
        if event == "REQUEST_CHANGES" {
            // Withdraw an earlier approval by this account (none is fine).
            if let Err(e) = c
                .request("POST", &format!("{mr}/unapprove"), Some(&json!({})), caller)
                .await
                && !absent(&e)
            {
                return Err(e);
            }
        }
        Ok(json!({ "id": note.get("id") }))
    }

    /// Each inline comment is a diff discussion on the new line, positioned
    /// with the merge request's `diff_refs`. Without the refs, or when GitLab
    /// refuses a position (a line outside the diff), the comment is folded
    /// into the review note instead.
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
        let mr = mr_path(c, key, n);
        let refs = if comments.is_empty() {
            None
        } else {
            match c.get_json(&mr, caller).await {
                Ok(v) => diff_refs(&v, commit),
                Err(e) if absent(&e) => None,
                Err(e) => return Err(e),
            }
        };
        let mut folded = Vec::new();
        let mut posted = Vec::new();
        for x in comments {
            let Some((base, start, head)) = &refs else {
                folded.push(x.clone());
                continue;
            };
            let b = json!({
                "body": x.body,
                "position": {
                    "position_type": "text", "base_sha": base, "start_sha": start,
                    "head_sha": head, "new_path": x.path, "old_path": x.path, "new_line": x.line,
                },
            });
            match c
                .request("POST", &format!("{mr}/discussions"), Some(&b), caller)
                .await
            {
                Ok(r) => posted.push(r.json()?.get("id").cloned().unwrap_or(Value::Null)),
                Err(e)
                    if matches!(
                        e.downcast_ref::<GhError>(),
                        Some(GhError::Http {
                            status: 400 | 422,
                            ..
                        })
                    ) =>
                {
                    folded.push(x.clone())
                }
                Err(e) => return Err(e),
            }
        }
        let mut v = self
            .review(c, key, n, event, &fold_inline(body, &folded), caller)
            .await?;
        v["discussions"] = json!(posted);
        v["folded"] = json!(folded.len());
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
        let mr = mr_path(c, key, n);
        let note = c
            .request(
                "POST",
                &format!("{mr}/notes"),
                Some(&json!({ "body": body })),
                caller,
            )
            .await?
            .json()?;
        // `sha` makes GitLab refuse when the head moved since the readiness check.
        c.request(
            "POST",
            &format!("{mr}/approve"),
            Some(&json!({ "sha": sha })),
            caller,
        )
        .await?;
        Ok(json!({ "id": note.get("id") }))
    }

    async fn branch_protected(
        &self,
        c: &Client,
        key: &str,
        branch: &str,
        caller: &str,
    ) -> Result<Option<bool>> {
        let path = format!(
            "{}/repository/branches/{}",
            project(c, key),
            encode_component(branch)
        );
        match c.get_json(&path, caller).await {
            Ok(v) => Ok(v.get("protected").and_then(Value::as_bool)),
            Err(e) if absent(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn update_issue(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        e: &IssueEdit,
        caller: &str,
    ) -> Result<()> {
        let mut b = json!({});
        if let Some(t) = &e.title {
            b["title"] = json!(t);
        }
        if let Some(t) = &e.body {
            b["description"] = json!(t);
        }
        match e.state.as_deref() {
            Some("closed") => b["state_event"] = json!("close"),
            Some("open") => b["state_event"] = json!("reopen"),
            _ => {}
        }
        if let Some(l) = &e.labels {
            b["labels"] = json!(l.join(","));
        }
        c.request(
            "PUT",
            &format!("{}/issues/{n}", project(c, key)),
            Some(&b),
            caller,
        )
        .await?;
        Ok(())
    }

    async fn add_labels(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        labels: &[String],
        caller: &str,
    ) -> Result<()> {
        let path = if is_mr(c, key, n).await? {
            mr_path(c, key, n)
        } else {
            format!("{}/issues/{n}", project(c, key))
        };
        c.request(
            "PUT",
            &path,
            Some(&json!({ "add_labels": labels.join(",") })),
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
        let v = c
            .request(
                "POST",
                &format!("{}/issues", project(c, key)),
                Some(&json!({ "title": title, "description": body, "labels": labels.join(",") })),
                caller,
            )
            .await?
            .json()?;
        Ok(json!({ "number": v.get("iid"), "html_url": s(&v, "web_url") }))
    }

    async fn set_status(
        &self,
        c: &Client,
        key: &str,
        sha: &str,
        st: &StatusIn,
        caller: &str,
    ) -> Result<Value> {
        let state = match st.state.as_str() {
            "success" => "success",
            "pending" => "running",
            "error" => "canceled",
            _ => "failed",
        };
        let mut body = json!({ "state": state, "name": st.context,
                               "description": st.description.chars().take(255).collect::<String>() });
        if !st.target_url.is_empty() {
            body["target_url"] = json!(st.target_url);
        }
        c.request(
            "POST",
            &format!("{}/statuses/{sha}", project(c, key)),
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
        let p = project(c, key);
        let mb = c
            .get_json(
                &format!("{p}/repository/merge_base?refs[]={base}&refs[]={head}"),
                caller,
            )
            .await?;
        let merge_base = s(&mb, "id").to_string();
        let cmp = c
            .get_json(
                &format!("{p}/repository/compare?from={base}&to={head}&straight=false"),
                caller,
            )
            .await?;
        let files: Vec<Value> = cmp["diffs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|d| {
                        let status = if d["new_file"].as_bool() == Some(true) {
                            "added"
                        } else if d["deleted_file"].as_bool() == Some(true) {
                            "removed"
                        } else if d["renamed_file"].as_bool() == Some(true) {
                            "renamed"
                        } else {
                            "modified"
                        };
                        let mut f =
                            json!({ "filename": s(d, "new_path"), "status": status, "sha": "" });
                        if status == "renamed" {
                            f["previous_filename"] = json!(s(d, "old_path"));
                        }
                        f
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "merge_base_commit": { "sha": merge_base }, "files": files }))
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
            .get(
                &format!(
                    "{}/repository/files/{}/raw?ref={sha}",
                    project(c, key),
                    encode_component(path)
                ),
                caller,
            )
            .await
        {
            Ok(r) if r.body.contains('\0') || r.body.contains('\u{FFFD}') => Ok((None, true)),
            Ok(r) => Ok((Some(r.body), false)),
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
                    "{}/repository/commits?ref_name={}&per_page=20",
                    project(c, key),
                    encode_path(branch)
                ),
                caller,
            )
            .await?;
        let items = r
            .json()?
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|cm| json!({ "sha": s(cm, "id"), "commit": { "message": s(cm, "message") } }))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Paged {
            items,
            not_modified: r.not_modified,
            pages: 1,
        })
    }

    async fn ci_runs(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(
                &format!("{}/pipelines?scope=finished&per_page=20", project(c, key)),
                caller,
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        let conclusion = match s(p, "status") {
                            "success" => "success",
                            "canceled" => "cancelled",
                            "skipped" => "skipped",
                            _ => "failure",
                        };
                        json!({ "id": p.get("id"), "name": format!("pipeline {}", p["id"]),
                                "conclusion": conclusion, "head_branch": s(p, "ref"),
                                "head_sha": s(p, "sha"), "html_url": s(p, "web_url") })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn releases(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(&format!("{}/releases?per_page=10", project(c, key)), caller)
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        // GitLab releases have no numeric id: the release time orders them.
                        let id = crate::parse::ts(&r["released_at"]).unwrap_or(0);
                        json!({ "id": id, "tag_name": s(r, "tag_name"), "name": s(r, "name"),
                                "html_url": s(&r["_links"], "self") })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn alerts(&self, _c: &Client, _key: &str, _caller: &str) -> Result<Vec<(String, Value)>> {
        Ok(Vec::new())
    }

    async fn label_actors(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        is_pull: bool,
        caller: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let path = if is_pull {
            format!("{}/resource_label_events?per_page=100", mr_path(c, key, n))
        } else {
            format!(
                "{}/issues/{n}/resource_label_events?per_page=100",
                project(c, key)
            )
        };
        let p = c.get_all(&path, caller).await?;
        Ok(super::api::label_adds(&p.items, |e| {
            (s(e, "action") == "add").then(|| {
                (
                    s(&e["label"], "name").to_string(),
                    s(&e["user"], "username").to_string(),
                )
            })
        }))
    }

    /// From the project's events (`action=commented`): one call per pass.
    async fn comments_since(
        &self,
        c: &Client,
        key: &str,
        since: i64,
        caller: &str,
    ) -> Result<Vec<Value>> {
        // `after` takes a date; filter the exact time here.
        let day = crate::parse::iso(since - 86_400_000);
        let p = c
            .get_all(
                &format!(
                    "{}/events?action=commented&sort=asc&per_page=100&after={}",
                    project(c, key),
                    &day[..10]
                ),
                caller,
            )
            .await?;
        Ok(p.items
            .iter()
            .filter(|e| crate::parse::ts(&e["created_at"]).is_some_and(|t| t > since))
            .filter(|e| matches!(s(&e["note"], "noteable_type"), "Issue" | "MergeRequest"))
            .map(|e| {
                json!({
                    "id": e["note"]["id"], "number": e["note"]["noteable_iid"],
                    "body": e["note"]["body"], "author": s(&e["author"], "username"),
                    "author_association": "", "html_url": "", "created_at": e["created_at"],
                })
            })
            .collect())
    }

    fn pull_path(&self, c: &Client, key: &str, n: i64) -> String {
        mr_path(c, key, n)
    }

    async fn pull_files(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<String>> {
        let p = c
            .get_all(
                &format!("{}/diffs?per_page=100", mr_path(c, key, n)),
                caller,
            )
            .await?;
        Ok(super::api::file_names(&p.items, "new_path"))
    }

    /// Project membership: Developer (30) and up is a member, Reporter or
    /// Guest a contributor, none `NONE`.
    async fn permission(&self, c: &Client, key: &str, login: &str, caller: &str) -> Result<String> {
        if login.is_empty() {
            return Ok(String::new());
        }
        let v = match c
            .get_json(
                &format!(
                    "{}/members/all?query={}",
                    project(c, key),
                    encode_path(login)
                ),
                caller,
            )
            .await
        {
            Ok(v) => v,
            Err(e) if absent(&e) => return Ok(String::new()),
            Err(e) => return Err(e),
        };
        Ok(gitlab_association(&v, login).into())
    }
}

/// `title` with or without the draft prefix.
pub fn draft_title(title: &str, draft: bool, prefix: &str) -> String {
    let bare = ["Draft: ", "Draft:", "WIP: ", "WIP:", "[Draft] ", "[WIP] "]
        .iter()
        .find_map(|p| title.strip_prefix(p))
        .unwrap_or(title)
        .trim_start();
    if draft {
        format!("{prefix}{bare}")
    } else {
        bare.to_string()
    }
}

/// GitHub's association word for a login in a GitLab members list.
pub fn gitlab_association(members: &Value, login: &str) -> &'static str {
    let level = members
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|m| s(m, "username").eq_ignore_ascii_case(login))
        })
        .and_then(|m| m["access_level"].as_i64());
    match level {
        Some(l) if l >= 50 => "OWNER",
        Some(l) if l >= 30 => "MEMBER",
        Some(_) => "CONTRIBUTOR",
        None => "NONE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_request_becomes_a_pull() {
        let mr = json!({
            "iid": 7, "title": "Draft: Fix", "state": "opened", "draft": true,
            "author": {"username": "ana"}, "source_branch": "feat", "target_branch": "main",
            "sha": "h1", "diff_refs": {"start_sha": "b1"}, "source_project_id": 3, "target_project_id": 3,
            "labels": ["sf:fix"], "reviewers": [{"username": "bo"}], "web_url": "u",
            "detailed_merge_status": "broken_status", "changes_count": "4",
            "created_at": "2026-10-20T00:00:00Z", "updated_at": "2026-10-20T00:00:00Z"
        });
        let v = pull_json(&mr, "grp/app", true);
        let p = crate::parse::pull("grp/app", &v, None, 1);
        assert_eq!((p.number, p.state.as_str(), p.draft), (7, "open", true));
        assert_eq!((p.head_sha.as_str(), p.base_sha.as_str()), ("h1", "b1"));
        assert_eq!(p.mergeable, "conflict");
        assert_eq!(p.labels, vec!["sf:fix".to_string()]);
        assert_eq!(p.requested_reviewers, vec!["bo".to_string()]);
        assert_eq!(p.changed_files, 4);
        assert!(!p.is_fork());
        let mut merged = mr.clone();
        merged["state"] = json!("merged");
        merged["merged_at"] = json!("2026-10-21T00:00:00Z");
        let p = crate::parse::pull("k", &pull_json(&merged, "grp/app", true), None, 1);
        assert!(p.merged && p.state == "closed");
    }

    #[test]
    fn issues_projects_and_titles() {
        let i = issue_json(
            &json!({"iid": 3, "state": "closed", "title": "T", "labels": ["a"],
                                   "author": {"username": "ana"}, "user_notes_count": 2}),
        );
        let gi = crate::parse::issue("k", &i, 1);
        assert_eq!(
            (gi.number, gi.state.as_str(), gi.comments),
            (3, "closed", 2)
        );
        let r = repo_json(
            &json!({"path_with_namespace": "grp/sub/app", "visibility": "internal",
                                  "default_branch": "trunk", "permissions": {"project_access": {"access_level": 30}}}),
        );
        let gr = crate::parse::repo(&r, 1);
        assert_eq!(gr.full_name, "grp/sub/app");
        assert!(gr.can_push && gr.visibility == "internal");
        assert_eq!(draft_title("Draft: x", false, "Draft: "), "x");
        assert_eq!(draft_title("x", true, "WIP: "), "WIP: x");
    }
}
