//! A pull request's conflicting files and committing their resolution.
//!
//! Loading reads the merge base from the compare API, then the three
//! versions (merge base, head = "ours", base branch = "theirs") of every
//! file both sides changed, and merges them with [`Merge3`]. Committing
//! makes a merge commit on the head branch through the git data API
//! (blobs, a tree on the head's tree that also takes the base branch's
//! other changes, a commit with parents head and base, a fast-forward of
//! the head ref). Commits made through the API are signed by GitHub: the
//! resolution records `signing: github_signs` and what GitHub's
//! `verification` said.

use crate::client::Client;
use crate::forge::guard;
use crate::forge::push::{BaseTake, PushPlan, push_merge};
use crate::forge::{Dialect, ForgeKind};
use crate::http::encode_path;
use crate::merge3::{Merge3, has_markers};
use crate::{GhError, valid_key};
use anyhow::{Result, anyhow, bail};
use kls_core::github::kinds;
use kls_store::github::Resolution;
use kls_store::now_ms;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictFile {
    pub path: String,
    /// The base branch renamed the file from here.
    pub previous: Option<String>,
    /// `None` when the file does not exist on that side.
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    /// Not text (or too large for the contents API): resolve by taking a side.
    pub binary: bool,
    pub merge: Merge3,
    /// The three-way merge left conflicts (or one side deleted what the other changed).
    pub conflicting: bool,
}

/// A change on the base branch since the merge base (taken as is).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BaseChange {
    pub path: String,
    /// "added", "modified", "removed", "renamed", ...
    pub status: String,
    pub previous: Option<String>,
    /// Blob of the file on the base branch.
    pub sha: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConflictSet {
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub head_repo: String,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    pub merge_base: String,
    /// Files both sides changed.
    pub files: Vec<ConflictFile>,
    /// Files only the base branch changed.
    pub base_only: Vec<BaseChange>,
    /// The forge of the connection it was loaded from.
    pub forge: ForgeKind,
}

impl ConflictSet {
    /// The resolution is committed through the forge's API, which signs it
    /// (GitHub and GHES); elsewhere it is a merge commit pushed unsigned.
    pub fn api_signed(&self) -> bool {
        self.forge.dialect() == Dialect::GitHub
    }

    pub fn is_fork(&self) -> bool {
        !self.head_repo.is_empty() && !self.head_repo.eq_ignore_ascii_case(&self.repo)
    }

    pub fn conflicting(&self) -> impl Iterator<Item = &ConflictFile> {
        self.files.iter().filter(|f| f.conflicting)
    }
}

fn file_names(cmp: &Value) -> Vec<Value> {
    cmp["files"].as_array().cloned().unwrap_or_default()
}

/// Loads the files of `repo#number` that both sides changed, with their
/// three versions and the three-way merge.
pub async fn load(c: &Client, repo: &str, number: i64, caller: &str) -> Result<ConflictSet> {
    if !valid_key(repo) {
        bail!("not a repository: {repo:?}");
    }
    let api = c.api();
    let pr = api.pull(c, repo, number, caller).await?;
    let head_sha = pr["head"]["sha"].as_str().unwrap_or("").to_string();
    let base_sha = pr["base"]["sha"].as_str().unwrap_or("").to_string();
    if head_sha.is_empty() || base_sha.is_empty() {
        bail!("{repo}#{number} has no head or base commit");
    }
    let head_side = api.compare(c, repo, &base_sha, &head_sha, caller).await?;
    let merge_base = head_side["merge_base_commit"]["sha"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("the forge did not report a merge base"))?
        .to_string();
    let base_side = api.compare(c, repo, &merge_base, &base_sha, caller).await?;
    let ours: BTreeSet<String> = file_names(&head_side)
        .iter()
        .flat_map(|f| {
            let mut v = vec![f["filename"].as_str().unwrap_or("").to_string()];
            if let Some(p) = f["previous_filename"].as_str() {
                v.push(p.to_string());
            }
            v
        })
        .filter(|s| !s.is_empty())
        .collect();
    let mut files = Vec::new();
    let mut base_only = Vec::new();
    for f in file_names(&base_side) {
        let path = f["filename"].as_str().unwrap_or("").to_string();
        let previous = f["previous_filename"].as_str().map(String::from);
        let both = ours.contains(&path) || previous.as_ref().is_some_and(|p| ours.contains(p));
        if !both {
            base_only.push(BaseChange {
                path,
                status: f["status"].as_str().unwrap_or("modified").into(),
                previous,
                sha: f["sha"].as_str().unwrap_or("").into(),
            });
            continue;
        }
        let (base, b_bin) = api
            .file_at(
                c,
                repo,
                previous.as_deref().unwrap_or(&path),
                &merge_base,
                caller,
            )
            .await?;
        let (o, o_bin) = api.file_at(c, repo, &path, &head_sha, caller).await?;
        let (t, t_bin) = api.file_at(c, repo, &path, &base_sha, caller).await?;
        let binary = b_bin || o_bin || t_bin;
        let merge = Merge3::new(
            base.as_deref().unwrap_or(""),
            o.as_deref().unwrap_or(""),
            t.as_deref().unwrap_or(""),
        );
        let deleted_vs_changed =
            (o.is_none() && t.is_some() && t != base) || (t.is_none() && o.is_some() && o != base);
        let conflicting = binary || merge.has_conflicts() || deleted_vs_changed;
        files.push(ConflictFile {
            path,
            previous: previous.clone(),
            base,
            ours: o,
            theirs: t,
            binary,
            merge,
            conflicting,
        });
    }
    let mut head_repo = pr["head"]["repo"]["full_name"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if !c.conn().is_github_com() && !head_repo.is_empty() && !head_repo.starts_with("fork:") {
        head_repo = c.conn().repo_key(&head_repo);
    }
    Ok(ConflictSet {
        repo: repo.into(),
        number,
        title: pr["title"].as_str().unwrap_or("").into(),
        head_repo,
        head_ref: pr["head"]["ref"].as_str().unwrap_or("").into(),
        head_sha,
        base_ref: pr["base"]["ref"].as_str().unwrap_or("").into(),
        base_sha,
        merge_base,
        files,
        base_only,
        forge: c.conn().kind,
    })
}

/// Commits the resolution: `resolved` maps each file both sides changed to
/// its final text (`None` deletes it). Files not listed take the merge
/// result, which must then be free of conflicts. On GitHub through the git
/// data API (GitHub signs); elsewhere a merge commit built with git and
/// pushed with the forge CLI as credential helper (`unsigned_push`).
/// Refused for automation while the factory is stopped; always audited.
pub async fn commit(
    c: &Client,
    set: &ConflictSet,
    resolved: &BTreeMap<String, Option<String>>,
    message: &str,
    caller: &str,
) -> Result<Resolution> {
    let target = format!("{}#{}", set.repo, set.number);
    guard::check_kill(c.core(), caller, "conflict_commit", &target, &c.conn().id).await?;
    let protected = guard::protected_branch(c, &set.repo, &set.head_ref, caller).await;
    if !protected.is_empty() {
        guard::refuse(
            c.core(),
            caller,
            "conflict_commit",
            &target,
            &c.conn().id,
            "protected_branch",
            &protected,
        )
        .await;
        bail!("{} is protected: {protected}", set.head_ref);
    }
    let r = commit_inner(c, set, resolved, message, caller).await;
    guard::audit(
        c.core(),
        caller,
        "conflict_commit",
        &target,
        &c.conn().id,
        &guard::outcome(&r),
        json!({ "branch": set.head_ref, "commit": r.as_ref().map(|x| x.commit_sha.clone()).unwrap_or_default(),
                "signing": r.as_ref().map(|x| x.signing.clone()).unwrap_or_default() }),
    )
    .await;
    r
}

async fn commit_inner(
    c: &Client,
    set: &ConflictSet,
    resolved: &BTreeMap<String, Option<String>>,
    message: &str,
    caller: &str,
) -> Result<Resolution> {
    let repo = &set.repo;
    if set.is_fork() {
        bail!(
            "the head branch is in the fork {}; commit the resolution there",
            set.head_repo
        );
    }
    if set.head_ref.is_empty() {
        bail!("the pull request has no head branch");
    }
    let mut finals: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for f in &set.files {
        let text = match resolved.get(&f.path) {
            Some(t) => t.clone(),
            None if f.conflicting => bail!("{} is not resolved yet", f.path),
            None => Some(f.merge.render(&[])),
        };
        if let Some(t) = &text
            && has_markers(t)
        {
            bail!("{} still has conflict markers", f.path);
        }
        finals.insert(&f.path, text);
    }
    if c.conn().dialect() != Dialect::GitHub {
        return commit_by_push(c, set, &finals, message, caller).await;
    }
    let fr = c.forge_name(repo).to_string();
    // Modes from the base branch's tree (one call); 100644 when unknown.
    let tree = c
        .get_json(
            &format!("/repos/{fr}/git/trees/{}?recursive=1", set.base_sha),
            caller,
        )
        .await
        .unwrap_or(Value::Null);
    let modes: BTreeMap<&str, &str> = tree["tree"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| Some((e["path"].as_str()?, e["mode"].as_str()?)))
                .collect()
        })
        .unwrap_or_default();
    let mode = |p: &str| modes.get(p).copied().unwrap_or("100644").to_string();
    let head = c
        .get_json(&format!("/repos/{fr}/git/commits/{}", set.head_sha), caller)
        .await?;
    let head_tree = head["tree"]["sha"]
        .as_str()
        .ok_or_else(|| anyhow!("head commit has no tree"))?
        .to_string();
    let mut entries = Vec::new();
    for (path, text) in &finals {
        match text {
            None => {
                entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": null }))
            }
            Some(t) => {
                let blob = c
                    .request(
                        "POST",
                        &format!("/repos/{fr}/git/blobs"),
                        Some(&json!({ "content": t, "encoding": "utf-8" })),
                        caller,
                    )
                    .await?
                    .json()?;
                let sha = blob["sha"]
                    .as_str()
                    .ok_or_else(|| anyhow!("blob has no sha"))?;
                entries
                    .push(json!({ "path": path, "mode": mode(path), "type": "blob", "sha": sha }));
            }
        }
    }
    for f in &set.files {
        if let Some(prev) = &f.previous
            && prev != &f.path
        {
            entries.push(json!({ "path": prev, "mode": "100644", "type": "blob", "sha": null }));
        }
    }
    for b in &set.base_only {
        if let Some(prev) = &b.previous
            && prev != &b.path
        {
            entries.push(json!({ "path": prev, "mode": "100644", "type": "blob", "sha": null }));
        }
        if b.status == "removed" {
            entries.push(json!({ "path": b.path, "mode": "100644", "type": "blob", "sha": null }));
        } else if !b.sha.is_empty() {
            entries.push(
                json!({ "path": b.path, "mode": mode(&b.path), "type": "blob", "sha": b.sha }),
            );
        }
    }
    let new_tree = c
        .request(
            "POST",
            &format!("/repos/{fr}/git/trees"),
            Some(&json!({ "base_tree": head_tree, "tree": entries })),
            caller,
        )
        .await?
        .json()?;
    let tree_sha = new_tree["sha"]
        .as_str()
        .ok_or_else(|| anyhow!("tree has no sha"))?;
    let message = if message.trim().is_empty() {
        format!("Merge {} into {}", set.base_ref, set.head_ref)
    } else {
        message.trim().to_string()
    };
    let commit = c
        .request(
            "POST",
            &format!("/repos/{fr}/git/commits"),
            Some(&json!({ "message": message, "tree": tree_sha, "parents": [set.head_sha, set.base_sha] })),
            caller,
        )
        .await?
        .json()?;
    let commit_sha = commit["sha"]
        .as_str()
        .ok_or_else(|| anyhow!("commit has no sha"))?
        .to_string();
    c.request(
        "PATCH",
        &format!("/repos/{fr}/git/refs/heads/{}", encode_path(&set.head_ref)),
        Some(&json!({ "sha": commit_sha, "force": false })),
        caller,
    )
    .await
    .map_err(|e| match e.downcast_ref::<GhError>() {
        Some(GhError::Http { status: 422, .. }) => {
            anyhow!("the head branch moved since the conflicts were loaded; load them again")
        }
        _ => e,
    })?;
    let res = Resolution {
        id: uuid::Uuid::now_v7().to_string(),
        repo: repo.clone(),
        number: set.number,
        branch: set.head_ref.clone(),
        commit_sha: commit_sha.clone(),
        parent_shas: vec![set.head_sha.clone(), set.base_sha.clone()],
        files: finals.keys().map(|s| s.to_string()).collect(),
        signing: "github_signs".into(),
        verified: commit["verification"]["verified"].as_bool(),
        verification_reason: commit["verification"]["reason"]
            .as_str()
            .unwrap_or("")
            .into(),
        route: c.route_key().to_string(),
        created_at: now_ms(),
    };
    finish(c, res, caller).await
}

/// Stores the resolution, emits `github.conflict.resolved` and reads the
/// pull again (the forge recomputes its mergeability).
async fn finish(c: &Client, res: Resolution, caller: &str) -> Result<Resolution> {
    let store = c.core().store();
    store.gh_insert_resolution(&res).await?;
    store
        .gh_set_resolution_connection(&res.id, &c.conn().id)
        .await?;
    c.invalidate(&c.api().pull_path(c, &res.repo, res.number))
        .await?;
    c.core()
        .emit(
            kinds::CONFLICT_RESOLVED,
            &format!("{}#{}", res.repo, res.number),
            &json!({ "repo": res.repo, "number": res.number, "commit": res.commit_sha,
                     "files": res.files, "signing": res.signing, "verified": res.verified,
                     "by": guard::actor(caller).1, "caller": caller, "connection": c.conn().id }),
        )
        .await?;
    let _ = crate::sync::refresh_pull(c, &res.repo, res.number, caller).await;
    Ok(res)
}

/// Off GitHub: build the merge commit with git and push it (forges.md 3.6).
/// Needs the local CLI (it is the credential helper).
async fn commit_by_push(
    c: &Client,
    set: &ConflictSet,
    finals: &BTreeMap<&str, Option<String>>,
    message: &str,
    caller: &str,
) -> Result<Resolution> {
    let auth = crate::forge::auth::auth_for(c.conn().cli());
    // Settings > Signing: the address merge commits are authored with.
    let signing_email = kls_core::audit::config::Effective::load(c.core())
        .await
        .ok()
        .map(|e| e.signing_email)
        .filter(|e| !e.trim().is_empty());
    let author_forced = signing_email.is_some();
    let mut renamed_from = Vec::new();
    for f in &set.files {
        if let Some(prev) = &f.previous
            && prev != &f.path
        {
            renamed_from.push(prev.clone());
        }
    }
    let plan = PushPlan {
        url: c.conn().clone_url(c.forge_name(&set.repo)),
        head_ref: set.head_ref.clone(),
        head_sha: set.head_sha.clone(),
        base_ref: set.base_ref.clone(),
        base_sha: set.base_sha.clone(),
        files: finals
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        renamed_from,
        base_only: set
            .base_only
            .iter()
            .map(|b| BaseTake {
                path: b.path.clone(),
                removed: b.status == "removed",
                previous: b.previous.clone(),
            })
            .collect(),
        message: if message.trim().is_empty() {
            format!("Merge {} into {}", set.base_ref, set.head_ref)
        } else {
            message.trim().to_string()
        },
        helper: Some((c.conn().host.clone(), auth.git_helper(c.conn()))),
        author_forced,
        author: (
            if c.conn().login.is_empty() {
                "Keleustes".into()
            } else {
                c.conn().login.clone()
            },
            signing_email.unwrap_or_else(|| {
                format!(
                    "{}@users.noreply.{}",
                    if c.conn().login.is_empty() {
                        "keleustes"
                    } else {
                        &c.conn().login
                    },
                    c.conn().host
                )
            }),
        ),
    };
    let sha = if *c.route() == crate::Route::Local {
        push_merge(&plan).await?
    } else {
        // On a node: the node builds the same commit and pushes it with its
        // own CLI as credential helper (forge_push job).
        match c.push_on_route(&push_payload(c, &plan, &set.repo)).await? {
            Some(sha) => sha,
            None => bail!(
                "conflict commits on {} need the local {} or a node that runs forge_push",
                c.conn().host,
                c.conn().cli().name()
            ),
        }
    };
    let res = Resolution {
        id: uuid::Uuid::now_v7().to_string(),
        repo: set.repo.clone(),
        number: set.number,
        branch: set.head_ref.clone(),
        commit_sha: sha,
        parent_shas: vec![set.head_sha.clone(), set.base_sha.clone()],
        files: finals.keys().map(|s| s.to_string()).collect(),
        signing: "unsigned_push".into(),
        verified: None,
        verification_reason: String::new(),
        route: c.route_key().to_string(),
        created_at: now_ms(),
    };
    finish(c, res, caller).await
}

/// The `forge_push` job payload (docs/gaps/forges-proto-request.md 5): the
/// node forces the URL from `host` + `repo` and the credential helper from
/// `cli`; no token or URL travels.
pub fn push_payload(c: &Client, plan: &PushPlan, key: &str) -> Value {
    json!({
        "cli": c.conn().cli().name(),
        "host": c.conn().host,
        "repo": c.forge_name(key),
        "head_ref": plan.head_ref, "head_sha": plan.head_sha,
        "base_ref": plan.base_ref, "base_sha": plan.base_sha,
        "files": plan.files,
        "renamed_from": plan.renamed_from,
        "base_only": plan.base_only.iter().map(|b| json!({
            "path": b.path, "removed": b.removed, "previous": b.previous,
        })).collect::<Vec<_>>(),
        "message": plan.message,
        "author_name": plan.author.0,
        "author_email": plan.author.1,
        "author_forced": plan.author_forced,
    })
}

// ---- lockfiles, carry-over, description, hand-off ---------------------------

/// Lockfiles and how each is regenerated. A lockfile is never hand-merged:
/// one side is kept whole and the commit description says to regenerate it.
pub const LOCKFILES: &[(&str, &str)] = &[
    ("Cargo.lock", "cargo generate-lockfile"),
    ("package-lock.json", "npm install"),
    ("npm-shrinkwrap.json", "npm install"),
    ("yarn.lock", "yarn install"),
    ("pnpm-lock.yaml", "pnpm install"),
    ("bun.lockb", "bun install"),
    ("bun.lock", "bun install"),
    ("go.sum", "go mod tidy"),
    ("go.work.sum", "go work sync"),
    ("poetry.lock", "poetry lock"),
    ("uv.lock", "uv lock"),
    ("Pipfile.lock", "pipenv lock"),
    ("pdm.lock", "pdm lock"),
    ("Gemfile.lock", "bundle install"),
    ("composer.lock", "composer update --lock"),
    ("mix.lock", "mix deps.get"),
    ("pubspec.lock", "dart pub get"),
    ("Podfile.lock", "pod install"),
    ("Package.resolved", "swift package resolve"),
    ("packages.lock.json", "dotnet restore --force-evaluate"),
    ("gradle.lockfile", "gradle dependencies --write-locks"),
    ("flake.lock", "nix flake lock"),
    ("deno.lock", "deno cache --reload"),
    ("conan.lock", "conan lock create"),
];

/// The command that regenerates a lockfile at `path`; `None` for other files.
pub fn regenerate_command(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Whether `path` is a lockfile (resolved by keeping one side, then regenerated).
pub fn is_lockfile(path: &str) -> bool {
    regenerate_command(path).is_some()
}

impl ConflictFile {
    pub fn lockfile(&self) -> bool {
        is_lockfile(&self.path)
    }
}

/// How one file was resolved (for the commit description).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum How {
    /// The three-way merge was clean.
    #[default]
    Merged,
    /// Conflict by conflict.
    Hunks,
    /// The head's whole file.
    Ours,
    /// The base branch's whole file.
    Theirs,
    /// Edited by hand.
    Edited,
}

impl How {
    pub fn word(self) -> &'static str {
        match self {
            How::Merged => "merged cleanly",
            How::Hunks => "resolved conflict by conflict",
            How::Ours => "kept ours",
            How::Theirs => "kept theirs",
            How::Edited => "edited by hand",
        }
    }
}

/// The commit message: a subject line, then the files both sides changed and
/// how each was resolved, the lockfiles to regenerate and the base-only count.
pub fn describe(set: &ConflictSet, how: &BTreeMap<String, How>) -> String {
    let mut out = format!("Merge {} into {}\n", set.base_ref, set.head_ref);
    let mut lines = Vec::new();
    let mut regen = Vec::new();
    for f in &set.files {
        let h = how.get(&f.path).copied().unwrap_or_default();
        let conflicts = f.merge.conflicts();
        let mut line = format!("- {}: {}", f.path, h.word());
        if conflicts > 0 && h == How::Hunks {
            line.push_str(&format!(
                " ({conflicts} conflict{})",
                if conflicts == 1 { "" } else { "s" }
            ));
        }
        if let Some(cmd) = regenerate_command(&f.path) {
            line.push_str(" (lockfile)");
            regen.push(format!("- {}: run `{cmd}`", f.path));
        }
        lines.push(line);
    }
    if !lines.is_empty() {
        out.push_str(&format!(
            "\nResolved {} file{} both sides changed:\n",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        ));
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    if !regen.is_empty() {
        out.push_str("\nRegenerate these lockfiles before merging:\n");
        out.push_str(&regen.join("\n"));
        out.push('\n');
    }
    if !set.base_only.is_empty() {
        out.push_str(&format!(
            "\nTakes {} file{} only {} changed.\n",
            set.base_only.len(),
            if set.base_only.len() == 1 { "" } else { "s" },
            set.base_ref
        ));
    }
    out.push_str(&format!(
        "\nHead {} · base {} · merge base {}\n",
        short(&set.head_sha),
        short(&set.base_sha),
        short(&set.merge_base)
    ));
    out
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// After the head moved (`head_moved`): the files of `new` whose three
/// versions are the same as in `old`, so a resolution made on `old` still
/// holds and is kept. A different pull request keeps nothing.
pub fn unchanged_files(old: &ConflictSet, new: &ConflictSet) -> BTreeSet<String> {
    if old.repo != new.repo || old.number != new.number {
        return BTreeSet::new();
    }
    new.files
        .iter()
        .filter(|n| {
            old.files.iter().any(|o| {
                o.path == n.path
                    && o.base == n.base
                    && o.ours == n.ours
                    && o.theirs == n.theirs
                    && o.binary == n.binary
            })
        })
        .map(|f| f.path.clone())
        .collect()
}

/// Whether an error from [`commit`] means the head branch moved.
pub fn head_moved(e: &str) -> bool {
    e.contains("head branch moved")
}

/// The agent a pull request's conflicts are handed to.
pub const RESOLVER: &str = "belai:conflict-resolver";
/// The card label the resolver claims (`kls-engine` conflict-fix rule).
pub const RESOLVER_LABEL: &str = "merge-conflict";
const FORK_HOLD: &str = "checkpoint:fork_code";

/// The card filed for [`RESOLVER`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolverCard {
    pub card_id: String,
    pub key: String,
    /// An open card for the pull request already existed (nothing filed).
    pub existing: bool,
}

/// Hands the conflicts of `set` to `belai:conflict-resolver`: files one
/// `merge-conflict` card per pull request (an open one is returned again),
/// in the card shape the engine's conflict-fix rule uses. A fork's card is
/// held for a person. Refused under the kill switch; always audited with
/// the caller as the actor.
pub async fn file_resolver_card(
    core: &kls_core::Core,
    set: &ConflictSet,
    url: &str,
    caller: &str,
) -> Result<ResolverCard> {
    let target = format!("{}#{}", set.repo, set.number);
    let (_, actor) = guard::actor(caller);
    if let Err(e) = kls_core::control::check(core, "dispatch", &target, &actor).await {
        guard::audit(
            core,
            caller,
            "conflict_resolver",
            &target,
            "",
            "refused",
            json!({ "reason": "kill_switch", "why": format!("{e:#}"), "agent": RESOLVER }),
        )
        .await;
        return Err(e);
    }
    let r = file_resolver_inner(core, set, url, caller).await;
    guard::audit(
        core,
        caller,
        "conflict_resolver",
        &target,
        "",
        &guard::outcome(&r),
        json!({ "agent": RESOLVER,
                "card": r.as_ref().map(|c| c.key.clone()).unwrap_or_default(),
                "existing": r.as_ref().map(|c| c.existing).unwrap_or(false) }),
    )
    .await;
    r
}

async fn file_resolver_inner(
    core: &kls_core::Core,
    set: &ConflictSet,
    url: &str,
    caller: &str,
) -> Result<ResolverCard> {
    let target = format!("{}#{}", set.repo, set.number);
    if let Some(c) = core.cards().await?.into_iter().find(|c| {
        !c.deleted
            && c.status != "done"
            && c.labels.iter().any(|l| l == RESOLVER_LABEL)
            && ((!url.is_empty() && c.pr_url == url) || c.title.contains(&target))
    }) {
        return Ok(ResolverCard {
            card_id: c.id,
            key: c.key,
            existing: true,
        });
    }
    let fork = set.is_fork();
    let mut labels = vec![RESOLVER_LABEL.to_string()];
    if fork {
        labels.push(FORK_HOLD.to_string());
    }
    let (_, actor) = guard::actor(caller);
    let meta = json!({
        "belai": { "list": "backlog", "agent": { "labels": labels, "branch": set.head_ref, "pr": url } },
        "keleustes": { "origin": "conflict", "agent": RESOLVER, "by": actor, "caller": caller },
    });
    let card = core
        .create_card(
            kls_core::board::CardDraft {
                title: format!("Resolve merge conflicts in {target}"),
                body: resolver_body(set, url),
                status: "backlog".into(),
                labels,
                repo: set.repo.clone(),
                branch: set.head_ref.clone(),
                pr_url: url.to_string(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            &actor,
        )
        .await?;
    Ok(ResolverCard {
        card_id: card.id,
        key: card.key,
        existing: false,
    })
}

/// The card body: the fenced block the resolver reads, and the files.
pub fn resolver_body(set: &ConflictSet, url: &str) -> String {
    let mut b = format!(
        "Resolve the merge conflicts of this pull request so it merges cleanly into its base. \
         Keep both sides' intent; the result must build, lint and pass the tests.\n\n```conflict\n\
         repo: {}\npull: {}\nurl: {}\nhead: {}\nhead_sha: {}\nbase: {}\nfork: {}\n```\n",
        set.repo,
        set.number,
        url,
        set.head_ref,
        set.head_sha,
        set.base_ref,
        set.is_fork(),
    );
    let files: Vec<&ConflictFile> = set.conflicting().collect();
    if !files.is_empty() {
        b.push_str("\nConflicting files:\n");
        for f in files {
            match regenerate_command(&f.path) {
                Some(cmd) => b.push_str(&format!("- {} (lockfile: `{cmd}`)\n", f.path)),
                None => b.push_str(&format!("- {}\n", f.path)),
            }
        }
    }
    b
}

#[cfg(test)]
#[path = "conflicts_tests.rs"]
mod tests;
