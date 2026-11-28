//! One file proposed as a pull request (belai-counterparts row 54: an
//! accepted AGENTS.md draft): a new branch from the default branch, one
//! commit that adds or replaces the file, and a pull request back. Every
//! write goes through `Client::request`, so the kill switch, the write cap
//! and the forge audit apply to each call; [`accept_init_draft`] also
//! records the person's decision on the draft (`kls_core::belai::initdraft`).

use super::api::PullIn;
use super::kind::Dialect;
use crate::client::Client;
use crate::http::{encode_component, encode_path};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};

/// What to propose.
#[derive(Clone, Debug, Default)]
pub struct Proposal {
    pub path: String,
    pub content: String,
    /// The branch to create (`keleustes/...`).
    pub branch: String,
    pub title: String,
    pub body: String,
    pub message: String,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

/// Whether `branch` is a safe new branch name for a proposal.
pub fn valid_branch(b: &str) -> bool {
    b.starts_with("keleustes/")
        && b.len() <= 100
        && !b.contains("..")
        && !b.ends_with('/')
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
}

/// Opens the pull request; returns `{number, html_url}`.
pub async fn propose_file(c: &Client, key: &str, p: &Proposal, caller: &str) -> Result<Value> {
    if !valid_branch(&p.branch) {
        bail!("not a proposal branch: {}", p.branch);
    }
    if p.path.is_empty() || p.path.starts_with('/') || p.path.contains("..") {
        bail!("not a repository path: {}", p.path);
    }
    let api = c.api();
    let (repo, _) = api.repo(c, key, caller).await?;
    let base = match s(&repo, "default_branch") {
        "" => "main".to_string(),
        b => b.to_string(),
    };
    let r = c.forge_name(key).to_string();
    match api.dialect() {
        Dialect::GitHub => {
            let head = c
                .get_json(
                    &format!("/repos/{r}/git/ref/heads/{}", encode_path(&base)),
                    caller,
                )
                .await?;
            let sha = head["object"]["sha"]
                .as_str()
                .ok_or_else(|| anyhow!("the default branch has no commit"))?
                .to_string();
            let existing = c
                .get_json(
                    &format!(
                        "/repos/{r}/contents/{}?ref={}",
                        encode_path(&p.path),
                        encode_component(&base)
                    ),
                    caller,
                )
                .await
                .ok()
                .map(|v| s(&v, "sha").to_string())
                .filter(|x| !x.is_empty());
            c.request(
                "POST",
                &format!("/repos/{r}/git/refs"),
                Some(&json!({ "ref": format!("refs/heads/{}", p.branch), "sha": sha })),
                caller,
            )
            .await?;
            let mut body =
                json!({ "message": p.message, "content": b64(&p.content), "branch": p.branch });
            if let Some(x) = existing {
                body["sha"] = json!(x);
            }
            c.request(
                "PUT",
                &format!("/repos/{r}/contents/{}", encode_path(&p.path)),
                Some(&body),
                caller,
            )
            .await?;
        }
        Dialect::GitLab => {
            let project = super::gitlab::project(c, key);
            let exists = c
                .get_json(
                    &format!(
                        "{project}/repository/files/{}?ref={}",
                        encode_component(&p.path),
                        encode_component(&base)
                    ),
                    caller,
                )
                .await
                .is_ok();
            c.request(
                "POST",
                &format!("{project}/repository/commits"),
                Some(&json!({
                    "branch": p.branch, "start_branch": base, "commit_message": p.message,
                    "actions": [{ "action": if exists { "update" } else { "create" },
                                  "file_path": p.path, "content": p.content }],
                })),
                caller,
            )
            .await?;
        }
        Dialect::Gitea => {
            let existing = c
                .get_json(
                    &format!(
                        "/repos/{r}/contents/{}?ref={}",
                        encode_path(&p.path),
                        encode_component(&base)
                    ),
                    caller,
                )
                .await
                .ok()
                .map(|v| s(&v, "sha").to_string())
                .filter(|x| !x.is_empty());
            let mut body = json!({
                "message": p.message, "content": b64(&p.content),
                "branch": base, "new_branch": p.branch,
            });
            let method = match existing {
                Some(x) => {
                    body["sha"] = json!(x);
                    "PUT"
                }
                None => "POST",
            };
            c.request(
                method,
                &format!("/repos/{r}/contents/{}", encode_path(&p.path)),
                Some(&body),
                caller,
            )
            .await?;
        }
    }
    let pr = api
        .create_pull(
            c,
            key,
            &PullIn {
                title: p.title.clone(),
                head: p.branch.clone(),
                base,
                body: p.body.clone(),
                draft: false,
            },
            caller,
        )
        .await?;
    crate::sync::note_authored(
        c,
        key,
        crate::sync::Authored::Pull,
        pr.get("number").and_then(Value::as_i64).unwrap_or(0),
        caller,
    )
    .await;
    Ok(pr)
}

/// Accepts a pending AGENTS.md draft: opens its pull request (with the
/// person's edits, when they made any) and records the decision. Refused
/// under the kill switch before any write.
pub async fn accept_init_draft(
    svc: &crate::service::Service,
    core: &kls_core::Core,
    id: &str,
    edited: Option<&str>,
    by: &str,
) -> Result<kls_core::belai::initdraft::Draft> {
    use kls_core::belai::initdraft;
    let d = initdraft::get(core, id)
        .await
        .ok_or_else(|| anyhow!("no such draft"))?;
    if d.status != "pending" {
        bail!("that draft was already {}", d.status);
    }
    if !super::valid_key(&d.repo) {
        bail!("the draft names no repository; draft it for a repository to open a pull request");
    }
    kls_core::control::check(core, "forge", &d.repo, by).await?;
    let content = initdraft::clean_markdown(edited.unwrap_or(&d.markdown));
    if content.trim().is_empty() || content.len() > initdraft::MAX_BYTES {
        bail!("the draft is empty or too large");
    }
    let c = svc.client_for(&d.repo).await?;
    let short: String =
        d.id.chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    let pr = propose_file(
        &c,
        &d.repo,
        &Proposal {
            path: d.path.clone(),
            content: content.clone(),
            branch: format!("keleustes/agents-md-{short}"),
            title: format!("Add {}", d.path),
            body: format!(
                "Drafted on {} from the repository's detected facts and reviewed in Keleustes by {by}.\n\n<!-- keleustes:init_draft {} -->",
                d.node_name, d.id
            ),
            message: format!("Add {} (drafted on {}, reviewed by {by})", d.path, d.node_name),
        },
        by,
    )
    .await?;
    let url = s(&pr, "html_url").to_string();
    initdraft::decide(
        core,
        id,
        "accepted",
        &url,
        edited.map(|_| content.as_str()),
        by,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeGh;
    use kls_core::Core;

    #[tokio::test]
    async fn a_file_goes_out_as_a_branch_commit_and_a_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        let gh = FakeGh::new();
        gh.on(
            "GET",
            "/repos/acme/app",
            200,
            json!({ "full_name": "acme/app", "default_branch": "trunk" }),
        );
        gh.on(
            "GET",
            "/repos/acme/app/git/ref/heads/trunk",
            200,
            json!({ "object": { "sha": "abc123" } }),
        );
        gh.on(
            "GET",
            "/repos/acme/app/contents/AGENTS.md?ref=trunk",
            404,
            json!({}),
        );
        gh.on("POST", "/repos/acme/app/git/refs", 201, json!({}));
        gh.on("PUT", "/repos/acme/app/contents/AGENTS.md", 201, json!({}));
        gh.on(
            "POST",
            "/repos/acme/app/pulls",
            201,
            json!({ "number": 7, "html_url": "https://github.com/acme/app/pull/7" }),
        );
        let c = Client::new(gh.clone(), core.clone());
        let p = Proposal {
            path: "AGENTS.md".into(),
            content: "# Agents\n".into(),
            branch: "keleustes/agents-md-1".into(),
            title: "Add AGENTS.md".into(),
            body: "b".into(),
            message: "m".into(),
        };
        let pr = propose_file(&c, "acme/app", &p, "user").await.unwrap();
        assert_eq!(pr["number"], 7);
        assert_eq!(gh.count("POST", "/repos/acme/app/git/refs"), 1);
        assert_eq!(gh.count("PUT", "/repos/acme/app/contents/AGENTS.md"), 1);
        // Under the kill switch nothing is written.
        core.set_factory_stopped(true, "drill").await.unwrap();
        gh.clear_calls();
        assert!(propose_file(&c, "acme/app", &p, "factory:x").await.is_err());
        assert_eq!(gh.count("POST", "/repos/acme/app/git/refs"), 0);
    }

    #[test]
    fn proposal_branches_are_namespaced() {
        assert!(valid_branch("keleustes/agents-md-0a1b2c3d"));
        assert!(!valid_branch("main"));
        assert!(!valid_branch("keleustes/../main"));
        assert!(!valid_branch("keleustes/a b"));
    }
}
