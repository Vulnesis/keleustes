//! A conflict resolution committed over git (forges.md 3.6), for forges
//! whose API cannot make a two-parent commit (GitLab, Gitea, Forgejo).
//!
//! In a temporary bare repository: fetch the head and base branches, build
//! the tree (head's tree, the base branch's other changes, the resolved
//! files), `git commit-tree -p head -p base`, then push with
//! `--force-with-lease` on the head's old commit, so a branch that moved is
//! refused, never overwritten. The forge CLI is the credential helper:
//! Keleustes sees no token.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// A change the base branch made that the merge takes as is.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseTake {
    pub path: String,
    pub removed: bool,
    pub previous: Option<String>,
}

/// What to build and where to push it.
#[derive(Clone, Debug, Default)]
pub struct PushPlan {
    /// `https://host/owner/name.git`, or `file://…` in tests.
    pub url: String,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    /// Final text of each resolved file (`None` deletes it).
    pub files: BTreeMap<String, Option<String>>,
    /// Paths renamed away on either side (removed from the tree).
    pub renamed_from: Vec<String>,
    pub base_only: Vec<BaseTake>,
    pub message: String,
    /// `(host, helper)`: `credential.https://<host>.helper`.
    pub helper: Option<(String, String)>,
    /// Used when git has no `user.name` / `user.email`.
    pub author: (String, String),
    /// Use `author`'s email even when git has one (Settings > Signing email).
    pub author_forced: bool,
}

struct TempRepo(PathBuf);

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn git(
    dir: &Path,
    pre: &[String],
    args: &[&str],
    env: &[(&str, String)],
    stdin: Option<&str>,
) -> Result<(bool, String, String)> {
    let mut cmd = tokio::process::Command::new(
        std::env::var("KELEUSTES_GIT").unwrap_or_else(|_| "git".into()),
    );
    cmd.arg("-C")
        .arg(dir)
        .args(pre)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().context("run git")?;
    if let (Some(s), Some(mut p)) = (stdin, child.stdin.take()) {
        p.write_all(s.as_bytes()).await?;
        drop(p);
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        child.wait_with_output(),
    )
    .await
    .context("git timed out")??;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

async fn must(
    dir: &Path,
    pre: &[String],
    args: &[&str],
    env: &[(&str, String)],
    stdin: Option<&str>,
) -> Result<String> {
    let (ok, out, err) = git(dir, pre, args, env, stdin).await?;
    if !ok {
        bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            err.lines().last().unwrap_or("")
        );
    }
    Ok(out)
}

/// `(mode, blob)` of `path` in `tree_ish`, if it is there.
async fn entry(dir: &Path, tree_ish: &str, path: &str) -> Result<Option<(String, String)>> {
    let out = must(dir, &[], &["ls-tree", tree_ish, "--", path], &[], None).await?;
    Ok(out.lines().next().and_then(|l| {
        let (meta, _) = l.split_once('\t')?;
        let mut it = meta.split_whitespace();
        let mode = it.next()?.to_string();
        let _kind = it.next()?;
        Some((mode, it.next()?.to_string()))
    }))
}

/// Builds the merge commit and pushes it. Returns the new commit's sha.
pub async fn push_merge(plan: &PushPlan) -> Result<String> {
    if plan.head_ref.is_empty() || plan.base_ref.is_empty() {
        bail!("the pull request has no head or base branch");
    }
    let dir = std::env::temp_dir().join(format!("kls-merge-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir)?;
    let _guard = TempRepo(dir.clone());
    must(&dir, &[], &["init", "--bare", "-q"], &[], None).await?;
    let mut cred = Vec::new();
    if let Some((host, helper)) = &plan.helper {
        cred.extend([
            "-c".to_string(),
            "credential.helper=".to_string(),
            "-c".to_string(),
            format!(
                "credential.{}://{host}.helper={helper}",
                if plan.url.starts_with("http://") {
                    "http"
                } else {
                    "https"
                }
            ),
        ]);
    }
    let head_spec = format!("+refs/heads/{}:refs/kls/head", plan.head_ref);
    let base_spec = format!("+refs/heads/{}:refs/kls/base", plan.base_ref);
    must(
        &dir,
        &cred,
        &[
            "fetch",
            "--no-tags",
            "-q",
            &plan.url,
            &head_spec,
            &base_spec,
        ],
        &[],
        None,
    )
    .await?;
    let head_now = must(&dir, &[], &["rev-parse", "refs/kls/head"], &[], None).await?;
    if head_now != plan.head_sha {
        bail!("the head branch moved since the conflicts were loaded; load them again");
    }
    let base_obj = format!("{}^{{commit}}", plan.base_sha);
    if !git(&dir, &[], &["cat-file", "-e", &base_obj], &[], None)
        .await?
        .0
    {
        bail!(
            "the base commit {} is gone from {}; load the conflicts again",
            plan.base_sha,
            plan.base_ref
        );
    }
    let index = dir.join("kls-index");
    let ienv = [("GIT_INDEX_FILE", index.to_string_lossy().into_owned())];
    must(&dir, &[], &["read-tree", &plan.head_sha], &ienv, None).await?;
    for p in &plan.renamed_from {
        must(
            &dir,
            &[],
            &["update-index", "--force-remove", "--", p],
            &ienv,
            None,
        )
        .await?;
    }
    for b in &plan.base_only {
        if let Some(prev) = &b.previous
            && prev != &b.path
        {
            must(
                &dir,
                &[],
                &["update-index", "--force-remove", "--", prev],
                &ienv,
                None,
            )
            .await?;
        }
        if b.removed {
            must(
                &dir,
                &[],
                &["update-index", "--force-remove", "--", &b.path],
                &ienv,
                None,
            )
            .await?;
        } else if let Some((mode, blob)) = entry(&dir, &plan.base_sha, &b.path).await? {
            let info = format!("{mode},{blob},{}", b.path);
            must(
                &dir,
                &[],
                &["update-index", "--add", "--cacheinfo", &info],
                &ienv,
                None,
            )
            .await?;
        }
    }
    for (path, text) in &plan.files {
        match text {
            None => {
                must(
                    &dir,
                    &[],
                    &["update-index", "--force-remove", "--", path],
                    &ienv,
                    None,
                )
                .await?;
            }
            Some(t) => {
                let blob = must(&dir, &[], &["hash-object", "-w", "--stdin"], &[], Some(t)).await?;
                let mode = match entry(&dir, &plan.head_sha, path).await? {
                    Some((m, _)) => m,
                    None => entry(&dir, &plan.base_sha, path)
                        .await?
                        .map(|e| e.0)
                        .unwrap_or_else(|| "100644".into()),
                };
                let info = format!("{mode},{blob},{path}");
                must(
                    &dir,
                    &[],
                    &["update-index", "--add", "--cacheinfo", &info],
                    &ienv,
                    None,
                )
                .await?;
            }
        }
    }
    let tree = must(&dir, &[], &["write-tree"], &ienv, None).await?;
    let mut who: Vec<(&str, String)> = Vec::new();
    let has = |k: &'static str| {
        let dir = dir.clone();
        async move {
            git(&dir, &[], &["config", k], &[], None)
                .await
                .map(|r| r.0 && !r.1.is_empty())
                .unwrap_or(false)
        }
    };
    if !has("user.name").await {
        who.push(("GIT_AUTHOR_NAME", plan.author.0.clone()));
        who.push(("GIT_COMMITTER_NAME", plan.author.0.clone()));
    }
    if plan.author_forced || !has("user.email").await {
        who.push(("GIT_AUTHOR_EMAIL", plan.author.1.clone()));
        who.push(("GIT_COMMITTER_EMAIL", plan.author.1.clone()));
    }
    let commit = must(
        &dir,
        &[],
        &[
            "commit-tree",
            &tree,
            "-p",
            &plan.head_sha,
            "-p",
            &plan.base_sha,
            "-m",
            &plan.message,
        ],
        &who,
        None,
    )
    .await?;
    let dst = format!("{commit}:refs/heads/{}", plan.head_ref);
    let lease = format!(
        "--force-with-lease=refs/heads/{}:{}",
        plan.head_ref, plan.head_sha
    );
    let (ok, _, err) = git(
        &dir,
        &cred,
        &["push", "-q", &plan.url, &dst, &lease],
        &[],
        None,
    )
    .await?;
    if !ok {
        if err.contains("stale info")
            || err.contains("rejected")
            || err.contains("non-fast-forward")
        {
            bail!("the head branch moved since the conflicts were loaded; load them again");
        }
        bail!("git push: {}", err.lines().last().unwrap_or("failed"));
    }
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sh(dir: &Path, args: &[&str]) -> String {
        let env = [
            ("GIT_AUTHOR_NAME", "t".to_string()),
            ("GIT_AUTHOR_EMAIL", "t@x".to_string()),
            ("GIT_COMMITTER_NAME", "t".to_string()),
            ("GIT_COMMITTER_EMAIL", "t@x".to_string()),
        ];
        must(dir, &[], args, &env, None).await.unwrap()
    }

    /// A "remote" with main and feat conflicting on app.txt; main also adds base.txt.
    async fn remote() -> (tempfile::TempDir, String, String, String) {
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("w");
        std::fs::create_dir_all(&w).unwrap();
        sh(&w, &["init", "-q", "-b", "main"]).await;
        std::fs::write(w.join("app.txt"), "a\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "init"]).await;
        sh(&w, &["checkout", "-q", "-b", "feat"]).await;
        std::fs::write(w.join("app.txt"), "feat\n").unwrap();
        sh(&w, &["commit", "-q", "-am", "feat"]).await;
        let head = sh(&w, &["rev-parse", "HEAD"]).await;
        sh(&w, &["checkout", "-q", "main"]).await;
        std::fs::write(w.join("app.txt"), "main\n").unwrap();
        std::fs::write(w.join("base.txt"), "new\n").unwrap();
        sh(&w, &["add", "."]).await;
        sh(&w, &["commit", "-q", "-m", "main"]).await;
        let base = sh(&w, &["rev-parse", "HEAD"]).await;
        let bare = d.path().join("r.git");
        sh(
            d.path(),
            &[
                "clone",
                "-q",
                "--bare",
                w.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        )
        .await;
        let url = format!("file://{}", bare.display());
        (d, url, head, base)
    }

    fn plan(url: &str, head: &str, base: &str) -> PushPlan {
        PushPlan {
            url: url.into(),
            head_ref: "feat".into(),
            head_sha: head.into(),
            base_ref: "main".into(),
            base_sha: base.into(),
            files: [("app.txt".to_string(), Some("both\n".to_string()))].into(),
            base_only: vec![BaseTake {
                path: "base.txt".into(),
                removed: false,
                previous: None,
            }],
            message: "Merge main into feat".into(),
            author: ("Keleustes".into(), "keleustes@localhost".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn builds_a_two_parent_merge_and_refuses_a_moved_branch() {
        let (d, url, head, base) = remote().await;
        let sha = push_merge(&plan(&url, &head, &base)).await.unwrap();
        let bare = d.path().join("r.git");
        assert_eq!(sh(&bare, &["rev-parse", "refs/heads/feat"]).await, sha);
        let parents = sh(&bare, &["rev-list", "--parents", "-n", "1", &sha]).await;
        assert_eq!(parents.split_whitespace().count(), 3, "{parents}");
        assert_eq!(
            sh(&bare, &["show", &format!("{sha}:app.txt")]).await,
            "both"
        );
        assert_eq!(
            sh(&bare, &["show", &format!("{sha}:base.txt")]).await,
            "new"
        );
        // The plan still names the old head: the branch moved, nothing is pushed.
        let e = push_merge(&plan(&url, &head, &base)).await.unwrap_err();
        assert!(format!("{e}").contains("moved"), "{e}");
        assert_eq!(sh(&bare, &["rev-parse", "refs/heads/feat"]).await, sha);
    }
}
