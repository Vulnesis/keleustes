//! Gitea, Forgejo and Codeberg through `tea api` (paths relative to
//! `/api/v1`). The REST shapes are close to GitHub's; the differences are
//! handled here: `limit` paging, `WIP:` drafts, label ids, review states,
//! commit statuses as checks, and the merge base found by walking history.

use super::api::{
    ForgeApi, InlineComment, IssueEdit, PullIn, StatusIn, absent, decode_contents,
    statuses_as_checks,
};
use super::gitlab::draft_title;
use super::kind::Dialect;
use crate::GhError;
use crate::client::{Client, Paged};
use crate::http::encode_path;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub struct GiteaApi;

/// Commits read at most on each side when looking for the merge base.
pub const MERGE_BASE_WALK: usize = 1000;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn r(c: &Client, key: &str) -> String {
    c.forge_name(key).to_string()
}

/// An organisation (`username`) as a GitHub org (`login`).
pub fn org_json(o: &Value) -> Value {
    json!({ "login": o.get("username").or_else(|| o.get("login")).cloned().unwrap_or(json!("")),
            "avatar_url": s(o, "avatar_url"), "description": s(o, "description") })
}

/// A Gitea repository with GitHub's `pushed_at`.
pub fn repo_json(v: &Value) -> Value {
    let mut v = v.clone();
    if v.get("pushed_at").is_none() {
        v["pushed_at"] = v.get("updated_at").cloned().unwrap_or(Value::Null);
    }
    v
}

/// A Gitea pull with GitHub's `draft` and `mergeable_state`.
pub fn pull_json(p: &Value) -> Value {
    let mut v = p.clone();
    let title = s(p, "title");
    let wip = ["WIP:", "[WIP]", "Draft:", "[Draft]"].iter().any(|x| {
        title
            .to_ascii_lowercase()
            .starts_with(&x.to_ascii_lowercase())
    });
    v["draft"] = json!(p["draft"].as_bool().unwrap_or(false) || wip);
    if let Some(m) = p.get("mergeable").and_then(Value::as_bool) {
        v["mergeable_state"] = json!(if m { "clean" } else { "dirty" });
    }
    if v["user"].get("login").is_none() {
        v["user"]["login"] = json!(s(&p["user"], "username"));
    }
    v
}

/// A pull list item. List items carry `mergeable` too; it is dropped so a
/// list read keeps the detail's view.
fn list_pull(x: &Value) -> Value {
    let mut v = pull_json(x);
    if let Some(o) = v.as_object_mut() {
        o.remove("mergeable");
        o.remove("mergeable_state");
    }
    v
}

/// Gitea review states as GitHub's (stale verdicts dropped).
pub fn reviews_json(list: &[Value]) -> Vec<Value> {
    list.iter()
        .filter(|r| {
            !r["stale"].as_bool().unwrap_or(false) && !r["dismissed"].as_bool().unwrap_or(false)
        })
        .map(|r| {
            let mut v = r.clone();
            v["state"] = json!(match s(r, "state") {
                "REQUEST_CHANGES" => "CHANGES_REQUESTED",
                "APPROVED" => "APPROVED",
                "COMMENT" => "COMMENTED",
                other => other,
            });
            v
        })
        .collect()
}

/// Gitea's `status` field as `state`.
fn statuses(v: &Value) -> Vec<Value> {
    v["statuses"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let st = x
                        .get("status")
                        .or_else(|| x.get("state"))
                        .cloned()
                        .unwrap_or(json!(""));
                    json!({ "context": s(x, "context"), "state": st })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn label_ids(c: &Client, key: &str, names: &[String], caller: &str) -> Result<Vec<i64>> {
    let have = c
        .get_all(&format!("/repos/{}/labels?limit=50", r(c, key)), caller)
        .await?;
    let by_name: BTreeMap<String, i64> = have
        .items
        .iter()
        .filter_map(|l| Some((s(l, "name").to_lowercase(), l["id"].as_i64()?)))
        .collect();
    let mut ids = Vec::new();
    for n in names {
        match by_name.get(&n.to_lowercase()) {
            Some(id) => ids.push(*id),
            None => {
                let l = c
                    .request(
                        "POST",
                        &format!("/repos/{}/labels", r(c, key)),
                        Some(&json!({ "name": n, "color": format!("#{}", crate::labels::COLOR) })),
                        caller,
                    )
                    .await?
                    .json()?;
                ids.push(
                    l["id"]
                        .as_i64()
                        .ok_or_else(|| anyhow!("label {n} has no id"))?,
                );
            }
        }
    }
    Ok(ids)
}

/// Commits reachable from `sha`, newest first, up to [`MERGE_BASE_WALK`].
async fn history(c: &Client, key: &str, sha: &str, caller: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut page = 1;
    while out.len() < MERGE_BASE_WALK {
        let v = c
            .get_json(
                &format!(
                    "/repos/{}/commits?sha={sha}&limit=50&page={page}&stat=false&verification=false",
                    r(c, key)
                ),
                caller,
            )
            .await?;
        let a = v.as_array().cloned().unwrap_or_default();
        if a.is_empty() {
            break;
        }
        let n = a.len();
        out.extend(a);
        if n < 50 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

/// Every best common ancestor of `head` and `base`: the common commits that
/// are not an ancestor of another common commit, by the `parents` each
/// listed commit carries. More than one is a criss-cross merge, which the
/// walk refuses (as the website's `giteaClient.MergeBase` does). Commits
/// listed without parents fall back to [`merge_base_of`].
pub fn merge_bases(head: &[Value], base: &[Value]) -> Vec<String> {
    let parents_of = |c: &Value| -> Vec<String> {
        c["parents"]
            .as_array()
            .map(|a| a.iter().map(|p| s(p, "sha").to_string()).collect())
            .unwrap_or_default()
    };
    if head.iter().chain(base).all(|c| c.get("parents").is_none()) {
        return merge_base_of(head, base).into_iter().collect();
    }
    let in_head: BTreeSet<&str> = head.iter().map(|c| s(c, "sha")).collect();
    let common: Vec<&str> = base
        .iter()
        .map(|c| s(c, "sha"))
        .filter(|sha| !sha.is_empty() && in_head.contains(sha))
        .collect();
    let mut graph: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for c in head.iter().chain(base) {
        graph.entry(s(c, "sha")).or_insert_with(|| parents_of(c));
    }
    // Strict ancestors of any common commit.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut todo: Vec<String> = common
        .iter()
        .flat_map(|c| graph.get(c).cloned().unwrap_or_default())
        .collect();
    while let Some(n) = todo.pop() {
        if seen.insert(n.clone()) {
            todo.extend(graph.get(n.as_str()).cloned().unwrap_or_default());
        }
    }
    common
        .into_iter()
        .filter(|c| !seen.contains(*c))
        .map(str::to_string)
        .collect()
}

/// The newest commit of `base` that is also in `head`'s history.
pub fn merge_base_of(head: &[Value], base: &[Value]) -> Option<String> {
    let in_head: BTreeSet<&str> = head.iter().map(|c| s(c, "sha")).collect();
    base.iter()
        .map(|c| s(c, "sha"))
        .find(|sha| in_head.contains(sha))
        .map(str::to_string)
}

#[async_trait::async_trait]
impl ForgeApi for GiteaApi {
    fn dialect(&self) -> Dialect {
        Dialect::Gitea
    }

    async fn viewer(&self, c: &Client, caller: &str) -> Result<Value> {
        c.get_json("/user", caller).await
    }

    async fn accounts(&self, c: &Client, caller: &str) -> Result<Vec<Value>> {
        Ok(c.get_all("/user/orgs?limit=50", caller)
            .await?
            .items
            .iter()
            .map(org_json)
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
            "/user/repos?limit=50".to_string()
        } else {
            format!("/orgs/{forge_login}/repos?limit=50")
        };
        let mut p = c.get_all(&path, caller).await?;
        if user {
            // /user/repos lists every repository the account can see; keep its own.
            let me = c.get_json("/user", caller).await?;
            let login = s(&me, "login").to_string();
            p.items.retain(|v| {
                s(&v["owner"], "login") == login || s(&v["owner"], "username") == login
            });
        }
        p.items = p.items.iter().map(repo_json).collect();
        Ok(p)
    }

    async fn repo(&self, c: &Client, key: &str, caller: &str) -> Result<(Value, bool)> {
        let resp = c.get(&format!("/repos/{}", r(c, key)), caller).await?;
        Ok((repo_json(&resp.json()?), resp.not_modified))
    }

    async fn open_pulls(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        let mut p = c
            .get_all(
                &format!(
                    "/repos/{}/pulls?state=open&sort=recentupdate&limit=50",
                    r(c, key)
                ),
                caller,
            )
            .await?;
        p.items = p.items.iter().map(list_pull).collect();
        Ok(p)
    }

    /// Gitea has no `since` on pulls: newest first by `recentupdate`, paging
    /// stops at the first pull older than the cursor.
    async fn pulls_since(
        &self,
        c: &Client,
        key: &str,
        since: i64,
        caller: &str,
    ) -> Result<Option<Paged>> {
        let mut p = c
            .get_until(
                &format!(
                    "/repos/{}/pulls?state=all&sort=recentupdate&limit=50",
                    r(c, key)
                ),
                caller,
                |x| crate::parse::ts(&x["updated_at"]).is_some_and(|t| t < since),
            )
            .await?;
        p.items = p.items.iter().map(list_pull).collect();
        Ok(Some(p))
    }

    async fn pull(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Value> {
        let mut v = pull_json(
            &c.get_json(&format!("/repos/{}/pulls/{n}", r(c, key)), caller)
                .await?,
        );
        if v.get("mergeable").is_none() {
            v["mergeable"] = Value::Null;
        }
        Ok(v)
    }

    async fn checks(&self, c: &Client, key: &str, sha: &str, caller: &str) -> Result<Vec<Value>> {
        match c
            .get_json(
                &format!("/repos/{}/commits/{sha}/status", r(c, key)),
                caller,
            )
            .await
        {
            Ok(v) => Ok(statuses_as_checks(&statuses(&v))),
            Err(e) if absent(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn reviews(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<Value>> {
        let p = c
            .get_all(
                &format!("/repos/{}/pulls/{n}/reviews?limit=50", r(c, key)),
                caller,
            )
            .await?;
        Ok(reviews_json(&p.items))
    }

    async fn issues_since(
        &self,
        c: &Client,
        key: &str,
        since: Option<i64>,
        caller: &str,
    ) -> Result<Paged> {
        let mut path = format!("/repos/{}/issues?type=issues&state=all&limit=50", r(c, key));
        if let Some(since) = since {
            path.push_str(&format!("&since={}", crate::parse::iso(since)));
        }
        let mut p = c.get_all(&path, caller).await?;
        // Gitea lists newest first; the mirror wants oldest first.
        p.items.reverse();
        Ok(p)
    }

    async fn labels(&self, c: &Client, key: &str, caller: &str) -> Result<Paged> {
        let mut p = c
            .get_all(&format!("/repos/{}/labels?limit=50", r(c, key)), caller)
            .await?;
        for l in &mut p.items {
            let color = s(l, "color").trim_start_matches('#').to_string();
            l["color"] = json!(color);
        }
        Ok(p)
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
                &format!("/repos/{}/labels", r(c, key)),
                Some(&body),
                caller,
            )
            .await
        {
            Ok(resp) => {
                let mut l = resp.json()?;
                let col = s(&l, "color").trim_start_matches('#').to_string();
                l["color"] = json!(col);
                Ok(Some(l))
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
        let path = format!("/repos/{}/pulls/{n}", r(c, key));
        let p = c.get_json(&path, caller).await?;
        let title = draft_title(s(&p, "title"), draft, "WIP: ");
        c.request("PATCH", &path, Some(&json!({ "title": title })), caller)
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
            "POST",
            &format!("/repos/{}/pulls/{n}/update?style=merge", r(c, key)),
            None,
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
            &format!("/repos/{}/pulls/{n}/requested_reviewers", r(c, key)),
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
        let mut body = json!({ "Do": method });
        if !sha.is_empty() {
            body["head_commit_id"] = json!(sha);
        }
        c.request(
            "POST",
            &format!("/repos/{}/pulls/{n}/merge", r(c, key)),
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
            &format!("/repos/{}/pulls/{n}/commits?limit=50", r(c, key)),
            caller,
        )
        .await?
        .items)
    }

    async fn create_pull(&self, c: &Client, key: &str, p: &PullIn, caller: &str) -> Result<Value> {
        let open = self.open_pulls(c, key, caller).await?;
        if let Some(x) = open.items.iter().find(|x| s(&x["head"], "ref") == p.head) {
            return Ok(x.clone());
        }
        let title = draft_title(&p.title, p.draft, "WIP: ");
        let v = c
            .request(
                "POST",
                &format!("/repos/{}/pulls", r(c, key)),
                Some(&json!({ "title": title, "head": p.head, "base": p.base, "body": p.body })),
                caller,
            )
            .await?
            .json()?;
        Ok(pull_json(&v))
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
            &format!("/repos/{}/issues/{n}/comments", r(c, key)),
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
        let event = if event == "REQUEST_CHANGES" {
            "REQUEST_CHANGES"
        } else {
            "COMMENT"
        };
        c.request(
            "POST",
            &format!("/repos/{}/pulls/{n}/reviews", r(c, key)),
            Some(&json!({ "body": body, "event": event })),
            caller,
        )
        .await?
        .json()
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
        let event = if event == "REQUEST_CHANGES" {
            "REQUEST_CHANGES"
        } else {
            "COMMENT"
        };
        let mut b = json!({ "body": body, "event": event });
        if !commit.is_empty() {
            b["commit_id"] = json!(commit);
        }
        if !comments.is_empty() {
            b["comments"] = json!(
                comments
                    .iter()
                    .map(|x| json!({"path": x.path, "new_position": x.line, "body": x.body}))
                    .collect::<Vec<_>>()
            );
        }
        c.request(
            "POST",
            &format!("/repos/{}/pulls/{n}/reviews", r(c, key)),
            Some(&b),
            caller,
        )
        .await?
        .json()
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
        c.request(
            "POST",
            &format!("/repos/{}/pulls/{n}/reviews", r(c, key)),
            Some(&json!({ "body": body, "event": "APPROVED", "commit_id": sha })),
            caller,
        )
        .await?
        .json()
    }

    async fn update_issue(
        &self,
        c: &Client,
        key: &str,
        n: i64,
        e: &IssueEdit,
        caller: &str,
    ) -> Result<()> {
        let mut b = e.github_json();
        if let Some(m) = b.as_object_mut() {
            m.remove("labels");
        }
        if b.as_object().is_some_and(|m| !m.is_empty()) {
            c.request(
                "PATCH",
                &format!("/repos/{}/issues/{n}", r(c, key)),
                Some(&b),
                caller,
            )
            .await?;
        }
        if let Some(labels) = &e.labels {
            // Gitea replaces labels by id.
            let ids = label_ids(c, key, labels, caller).await?;
            c.request(
                "PUT",
                &format!("/repos/{}/issues/{n}/labels", r(c, key)),
                Some(&json!({ "labels": ids })),
                caller,
            )
            .await?;
        }
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
        let ids = label_ids(c, key, labels, caller).await?;
        c.request(
            "POST",
            &format!("/repos/{}/issues/{n}/labels", r(c, key)),
            Some(&json!({ "labels": ids })),
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
        let ids = label_ids(c, key, labels, caller).await?;
        c.request(
            "POST",
            &format!("/repos/{}/issues", r(c, key)),
            Some(&json!({ "title": title, "body": body, "labels": ids })),
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
        st: &StatusIn,
        caller: &str,
    ) -> Result<Value> {
        let mut body =
            json!({ "state": st.state, "context": st.context, "description": st.description });
        if !st.target_url.is_empty() {
            body["target_url"] = json!(st.target_url);
        }
        c.request(
            "POST",
            &format!("/repos/{}/statuses/{sha}", r(c, key)),
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
        let hh = history(c, key, head, caller).await?;
        let bh = history(c, key, base, caller).await?;
        let bases = merge_bases(&hh, &bh);
        if bases.len() > 1 {
            bail!(
                "{base} and {head} have {} merge bases (a criss-cross merge); resolve on the forge",
                bases.len()
            );
        }
        let Some(mb) = bases.into_iter().next() else {
            bail!(
                "no merge base within {MERGE_BASE_WALK} commits of {base} and {head}; resolve on the forge"
            );
        };
        // Files changed on `head` since the merge base: compare when the
        // server has it, else the files of the commits in between.
        let files = match c
            .get_json(
                &format!("/repos/{}/compare/{mb}...{head}", r(c, key)),
                caller,
            )
            .await
        {
            Ok(v) if v["files"].is_array() => v["files"].as_array().cloned().unwrap_or_default(),
            Ok(_) | Err(_) => {
                let mut seen = BTreeMap::new();
                for cm in hh.iter().take_while(|cm| s(cm, "sha") != mb) {
                    for f in cm["files"].as_array().cloned().unwrap_or_default() {
                        let name = s(&f, "filename").to_string();
                        if !name.is_empty() {
                            seen.entry(name.clone()).or_insert(json!({
                                "filename": name, "status": s(&f, "status"), "sha": ""
                            }));
                        }
                    }
                }
                seen.into_values().collect()
            }
        };
        Ok(json!({ "merge_base_commit": { "sha": mb }, "files": files }))
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
                    r(c, key),
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
        let resp = c
            .get(
                &format!(
                    "/repos/{}/commits?sha={}&limit=20&stat=false&verification=false",
                    r(c, key),
                    encode_path(branch)
                ),
                caller,
            )
            .await?;
        Ok(Paged {
            items: resp.json()?.as_array().cloned().unwrap_or_default(),
            not_modified: resp.not_modified,
            pages: 1,
        })
    }

    async fn ci_runs(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(
                &format!("/repos/{}/actions/tasks?limit=20", r(c, key)),
                caller,
            )
            .await?;
        Ok(v["workflow_runs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|t| !matches!(s(t, "status"), "running" | "waiting" | "blocked" | ""))
                    .map(|t| {
                        let conclusion = match s(t, "status") {
                            "success" => "success",
                            "skipped" => "skipped",
                            "cancelled" => "cancelled",
                            _ => "failure",
                        };
                        json!({ "id": t.get("id"), "name": s(t, "name"), "conclusion": conclusion,
                                "head_branch": s(t, "head_branch"), "head_sha": s(t, "head_sha"),
                                "html_url": s(t, "url") })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn releases(&self, c: &Client, key: &str, caller: &str) -> Result<Vec<Value>> {
        let v = c
            .get_json(&format!("/repos/{}/releases?limit=10", r(c, key)), caller)
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| !x["draft"].as_bool().unwrap_or(false))
                    .cloned()
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
        _is_pull: bool,
        caller: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let p = c
            .get_all(
                &format!("/repos/{}/issues/{n}/timeline?limit=50", r(c, key)),
                caller,
            )
            .await?;
        // Gitea's label timeline entry: body "1" adds, "" removes.
        Ok(super::api::label_adds(&p.items, |e| {
            (s(e, "type") == "label" && !s(e, "body").is_empty()).then(|| {
                (
                    s(&e["label"], "name").to_string(),
                    s(&e["user"], "login").to_string(),
                )
            })
        }))
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
                    "/repos/{}/issues/comments?limit=50&since={}",
                    r(c, key),
                    crate::parse::iso(since)
                ),
                caller,
            )
            .await?;
        Ok(p.items
            .iter()
            .map(|v| {
                json!({
                    "id": v["id"], "number": super::api::number_of(s(v, "issue_url")),
                    "body": v["body"], "author": s(&v["user"], "login"),
                    "author_association": "", "html_url": s(v, "html_url"),
                    "created_at": v["created_at"],
                })
            })
            .collect())
    }

    fn pull_path(&self, c: &Client, key: &str, n: i64) -> String {
        format!("/repos/{}/pulls/{n}", r(c, key))
    }

    async fn pull_files(&self, c: &Client, key: &str, n: i64, caller: &str) -> Result<Vec<String>> {
        let p = c
            .get_all(
                &format!("/repos/{}/pulls/{n}/files?limit=50", r(c, key)),
                caller,
            )
            .await?;
        Ok(super::api::file_names(&p.items, "filename"))
    }

    /// Collaborator permission; when the account may not read it (403), the
    /// org teams the login is in (`permissionFromTeams`).
    async fn permission(&self, c: &Client, key: &str, login: &str, caller: &str) -> Result<String> {
        if login.is_empty() {
            return Ok(String::new());
        }
        let rn = r(c, key);
        match c
            .get_json(
                &format!(
                    "/repos/{rn}/collaborators/{}/permission",
                    encode_path(login)
                ),
                caller,
            )
            .await
        {
            Ok(v) => Ok(gitea_association(s(&v, "permission")).into()),
            Err(e)
                if matches!(
                    e.downcast_ref::<GhError>(),
                    Some(GhError::Http { status: 404, .. })
                ) =>
            {
                Ok("NONE".into())
            }
            Err(e) if absent(&e) => {
                let owner = rn.split('/').next().unwrap_or("");
                match c
                    .get_json(
                        &format!("/orgs/{owner}/members/{}", encode_path(login)),
                        caller,
                    )
                    .await
                {
                    Ok(_) => Ok("MEMBER".into()),
                    Err(e) if absent(&e) => Ok(String::new()),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// GitHub's association word for a Gitea collaborator permission.
pub fn gitea_association(permission: &str) -> &'static str {
    match permission {
        "owner" => "OWNER",
        "admin" | "write" => "COLLABORATOR",
        "read" => "CONTRIBUTOR",
        _ => "NONE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let p = pull_json(&json!({"number": 2, "title": "WIP: x", "mergeable": false,
                                  "user": {"username": "ana"}, "head": {"ref": "f", "sha": "h"},
                                  "base": {"ref": "main", "sha": "b"}, "state": "open"}));
        let gp = crate::parse::pull("codeberg.org/ana/app", &p, None, 1);
        assert!(gp.draft);
        assert_eq!(
            (gp.mergeable.as_str(), gp.author.as_str()),
            ("conflict", "ana")
        );
        let rv = reviews_json(&[
            json!({"user": {"login": "bo"}, "state": "REQUEST_CHANGES"}),
            json!({"user": {"login": "cy"}, "state": "APPROVED", "stale": true}),
        ]);
        assert_eq!(rv.len(), 1);
        assert_eq!(
            crate::parse::review_decision(&rv, "ana"),
            "CHANGES_REQUESTED"
        );
        let st = statuses(&json!({"statuses": [{"context": "ci", "status": "failure"}]}));
        let ch = crate::parse::checks(&statuses_as_checks(&st));
        assert_eq!(ch.failure, 1);
        assert_eq!(org_json(&json!({"username": "acme"}))["login"], "acme");
    }

    #[test]
    fn merge_base_by_history() {
        let c = |s: &str| json!({"sha": s});
        let head = [c("h2"), c("h1"), c("m"), c("o")];
        let base = [c("b2"), c("b1"), c("m"), c("o")];
        assert_eq!(merge_base_of(&head, &base).as_deref(), Some("m"));
        assert_eq!(merge_base_of(&head, &[c("x")]), None);
        // Without parents the walk is the plain newest-common commit.
        assert_eq!(merge_bases(&head, &base), vec!["m".to_string()]);
    }

    #[test]
    fn criss_cross_merges_have_two_bases() {
        let c = |s: &str, p: &[&str]| json!({"sha": s, "parents": p.iter().map(|x| json!({"sha": x})).collect::<Vec<_>>()});
        // A linear fork point: one base.
        let head = [c("h", &["m"]), c("m", &["o"]), c("o", &[])];
        let base = [c("b", &["m"]), c("m", &["o"]), c("o", &[])];
        assert_eq!(merge_bases(&head, &base), vec!["m".to_string()]);
        // x and y each merged the other: both are best common ancestors.
        let head = [
            c("h", &["x1"]),
            c("x1", &["x", "y"]),
            c("x", &["o"]),
            c("y", &["o"]),
            c("o", &[]),
        ];
        let base = [
            c("b", &["y1"]),
            c("y1", &["y", "x"]),
            c("y", &["o"]),
            c("x", &["o"]),
            c("o", &[]),
        ];
        let mut got = merge_bases(&head, &base);
        got.sort();
        assert_eq!(got, vec!["x".to_string(), "y".to_string()]);
    }
}
