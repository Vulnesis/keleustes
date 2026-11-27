//! A repository's profile from its file names (builtin-agents 2.2, web
//! `factory_repo_profile.go`). Keleustes keeps no file mirror, so the paths
//! come from one tree listing of the default branch through the budgeted,
//! cached client (`GET /repos/{repo}` for the branch and language, then
//! `GET /repos/{repo}/git/trees/{branch}?recursive=1`; both conditional, so
//! an unchanged repo costs two 304s). The profile is pure
//! (`kls_core::repo_profile`); the scout's deep signals ride through.
//!
//! The poller refreshes a managed repository's profile when it has none or
//! when the repo was pushed since the last refresh, at most once per
//! [`REFRESH_EVERY_MS`]. Each refresh is audited (`forge` ledger entry,
//! action `repo.profile`) and emits `repo.profiled`; the client holds it
//! under the kill switch like every automated call.

use crate::Client;
use crate::forge::guard;
use anyhow::{Result, bail};
use kls_core::repo_profile::{self, RepoProfile};
use kls_store::now_ms;
use serde_json::{Value, json};

/// The event a refresh emits: `{repo, signals, files, truncated, origin}`.
pub const PROFILED: &str = "repo.profiled";
/// A repository's profile is read again at most this often.
pub const REFRESH_EVERY_MS: i64 = 3_600_000;
/// Paths read from one listing (GitHub cuts a recursive tree at 100 000).
pub const MAX_PATHS: usize = 100_000;

/// The stored profile of `repo`, as stored.
pub async fn stored(c: &Client, repo: &str) -> Option<RepoProfile> {
    let raw = c
        .core()
        .store()
        .setting(&RepoProfile::key(repo))
        .await
        .ok()
        .flatten()?;
    serde_json::from_str(&raw).ok()
}

/// The blob paths of a tree listing.
pub fn tree_paths(tree: &Value) -> Vec<String> {
    tree.get("tree")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("blob"))
        .filter_map(|e| e.get("path").and_then(Value::as_str))
        .take(MAX_PATHS)
        .map(str::to_string)
        .collect()
}

/// Whether the poller should read `r`'s paths again at `now`.
pub fn due(stored: Option<&RepoProfile>, pushed_at: Option<i64>, now: i64) -> bool {
    match stored {
        None => true,
        Some(p) if p.refreshed_at == 0 => true,
        Some(p) => {
            now - p.refreshed_at >= REFRESH_EVERY_MS
                && pushed_at.is_some_and(|at| at > p.refreshed_at)
        }
    }
}

/// Reads `repo`'s default-branch tree, works the profile out, stores it
/// over what the scout found, audits and emits [`PROFILED`].
pub async fn refresh(c: &Client, repo: &str, caller: &str) -> Result<RepoProfile> {
    if !crate::valid_repo(repo) {
        bail!("not a repository name: {repo:?}");
    }
    let core = c.core();
    // Reads are not held by the client; an automated refresh writes, so it is.
    guard::check_kill(core, caller, "repo.profile", repo, &c.conn().id).await?;
    let meta = c.get_json(&format!("/repos/{repo}"), caller).await?;
    let branch = meta
        .get("default_branch")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty() && !b.contains(".."))
        .unwrap_or("HEAD")
        .to_string();
    let language = meta
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tree = c
        .get_json(
            &format!("/repos/{repo}/git/trees/{branch}?recursive=1"),
            caller,
        )
        .await?;
    let paths = tree_paths(&tree);
    let mut fresh = repo_profile::from_files(&paths, &language);
    fresh.truncated =
        tree.get("truncated").and_then(Value::as_bool) == Some(true) || paths.len() >= MAX_PATHS;
    let before = stored(c, repo).await;
    let p = RepoProfile::after_refresh(before.as_ref(), fresh, now_ms());
    core.store()
        .set_setting(&RepoProfile::key(repo), &serde_json::to_string(&p)?)
        .await?;
    let changed = before.as_ref().is_none_or(|b| {
        (
            &b.signals,
            &b.package_managers,
            &b.languages,
            &b.test_runners,
            &b.ci,
            &b.artifact_types,
        ) != (
            &p.signals,
            &p.package_managers,
            &p.languages,
            &p.test_runners,
            &p.ci,
            &p.artifact_types,
        )
    });
    guard::audit(
        core,
        caller,
        "repo.profile",
        repo,
        &c.conn().id,
        if changed { "refreshed" } else { "unchanged" },
        json!({ "basis": { "files": p.files, "truncated": p.truncated, "branch": branch,
                           "sources": ["tree"] },
                "signals": p.signals, "packageManagers": p.package_managers,
                "testRunners": p.test_runners, "ci": p.ci,
                "artifactTypes": p.artifact_types, "languages": p.languages }),
    )
    .await;
    if changed {
        core.emit(
            PROFILED,
            repo,
            &json!({ "repo": repo, "signals": p.signals, "files": p.files,
                     "truncated": p.truncated, "origin": caller }),
        )
        .await?;
    }
    Ok(p)
}

/// The poller's step: refreshes `repo` when [`due`] (github.com only: the
/// tree listing is the GitHub API). `Ok(false)` when nothing was read.
pub async fn refresh_if_due(
    c: &Client,
    repo: &kls_store::github::GhRepo,
    caller: &str,
) -> Result<bool> {
    if !c.conn().is_github_com() {
        return Ok(false);
    }
    let have = stored(c, &repo.full_name).await;
    if !due(have.as_ref(), repo.pushed_at, now_ms()) {
        return Ok(false);
    }
    match refresh(c, &repo.full_name, caller).await {
        Ok(_) => Ok(true),
        // An empty repo has no tree; a gone one is the mirror's to notice.
        Err(e) if crate::forge::api::absent(&e) => Ok(false),
        Err(e) => Err(e),
    }
}
