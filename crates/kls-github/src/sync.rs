//! Mirroring a forge into the store, and the pull request actions.
//!
//! Every read goes through the connection's dialect ([`ForgeApi`]), which
//! answers in GitHub's shapes; on GitHub every read is conditional (the
//! client sends the stored ETag), so a pass over a quiet repository costs
//! 304s, which do not count toward the factory share. Each change a screen
//! shows emits a `github.*` event (on every forge; data carries
//! `connection` off github.com).
//!
//! [`ForgeApi`]: crate::forge::api::ForgeApi

use crate::client::Client;
use crate::forge::guard;
use crate::parse;
use crate::{GhError, valid_key, valid_repo};
use anyhow::{Result, bail};
use kls_core::github::kinds;
use kls_store::github::{
    DeliveryCommit, GhIssue, GhOrg, GhPull, GhRepo, HumanAction, Outcome, Person, RepoForge, Upsert,
};
use kls_store::now_ms;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

// ---- orgs and repos --------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OrgRefresh {
    pub orgs: usize,
    pub repos: usize,
    pub added: Vec<String>,
}

/// The forge's own login of a stored org key.
async fn forge_login(c: &Client, org: &GhOrg) -> Result<String> {
    if c.conn().is_github_com() {
        return Ok(org.login.clone());
    }
    Ok(match c.core().store().gh_org_forge(&org.login).await? {
        Some((_, l)) if !l.is_empty() => l,
        _ => c.forge_name(&org.login).to_string(),
    })
}

/// Lists the signed-in account and its orgs (groups on GitLab); with
/// `repos`, also the repositories of every managed org.
pub async fn refresh_orgs(c: &Client, caller: &str, repos: bool) -> Result<OrgRefresh> {
    let store = c.core().store();
    let now = now_ms();
    let api = c.api();
    let me = api.viewer(c, caller).await?;
    let mut orgs = vec![parse::org(&me, "User", now)];
    for o in api.accounts(c, caller).await? {
        orgs.push(parse::org(&o, "Organization", now));
    }
    let mut out = OrgRefresh::default();
    for o in &orgs {
        if o.login.is_empty() {
            continue;
        }
        let mut row = o.clone();
        row.login = c.conn().org_key(&o.login);
        if let Some(prev) = store.gh_org(&row.login).await? {
            row.managed = prev.managed;
            row.enabled = prev.enabled;
            row.repo_count = prev.repo_count;
        }
        store.gh_upsert_org(&row).await?;
        if !c.conn().is_github_com() {
            store
                .gh_set_org_forge(&row.login, &c.conn().id, &o.login)
                .await?;
        }
        out.orgs += 1;
        if repos && row.managed {
            let r = refresh_org_repos(c, &row, caller).await?;
            out.repos += r.repos;
            out.added.extend(r.added);
        }
    }
    c.core()
        .emit(
            kinds::ORG_UPDATED,
            "",
            &json!({ "orgs": out.orgs, "repos": out.repos, "connection": c.conn().id }),
        )
        .await?;
    Ok(out)
}

/// Lists one org's (or the user's own) repositories into the store. A
/// repository new to an org listed before emits `github.repo.added`.
pub async fn refresh_org_repos(c: &Client, org: &GhOrg, caller: &str) -> Result<OrgRefresh> {
    let store = c.core().store();
    let login = forge_login(c, org).await?;
    let list = c
        .api()
        .account_repos(c, &login, org.kind == "User", caller)
        .await?;
    let now = now_ms();
    let mut out = OrgRefresh::default();
    let listed_before = org.synced_at > 0 && org.repo_count > 0;
    for v in &list.items {
        let mut r = parse::repo(v, now);
        let forge_full = r.full_name.clone();
        let ok = if c.conn().is_github_com() {
            valid_repo(&forge_full)
        } else {
            crate::forge::valid_forge_name(&forge_full)
        };
        if !ok {
            continue;
        }
        r.full_name = c.conn().repo_key(&forge_full);
        r.owner = c.conn().org_key(&r.owner);
        let added = store.gh_repo(&r.full_name).await?.is_none();
        if added {
            out.added.push(r.full_name.clone());
        }
        store.gh_upsert_repo(&r).await?;
        if !c.conn().is_github_com() {
            store
                .gh_set_repo_forge(
                    &r.full_name,
                    &RepoForge {
                        connection: c.conn().id.clone(),
                        forge_full_name: forge_full,
                        forge_id: v.get("id").and_then(Value::as_i64),
                    },
                )
                .await?;
        }
        if added && listed_before && org.managed {
            c.core()
                .emit(
                    kinds::REPO_ADDED,
                    &r.full_name,
                    &json!({ "repo": r.full_name, "org": org.login, "connection": c.conn().id }),
                )
                .await?;
        }
        out.repos += 1;
    }
    let mut row = org.clone();
    row.repo_count = out.repos as i64;
    row.synced_at = now;
    store.gh_upsert_org(&row).await?;
    if !list.not_modified {
        c.core()
            .emit(
                kinds::REPO_UPDATED,
                &org.login,
                &json!({ "org": org.login, "repos": out.repos, "added": out.added }),
            )
            .await?;
    }
    Ok(out)
}

/// Re-reads one repository's details.
pub async fn refresh_repo(c: &Client, full: &str, caller: &str) -> Result<GhRepo> {
    if !valid_key(full) {
        bail!("not a repository: {full:?}");
    }
    let (v, not_modified) = c.api().repo(c, full, caller).await?;
    let mut repo = parse::repo(&v, now_ms());
    repo.full_name = full.to_string();
    repo.owner = full.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();
    let store = c.core().store();
    store.gh_upsert_repo(&repo).await?;
    if !not_modified {
        c.core()
            .emit(kinds::REPO_UPDATED, full, &json!({ "repo": full }))
            .await?;
    }
    Ok(store.gh_repo(full).await?.unwrap_or(repo))
}

// ---- pulls -----------------------------------------------------------------

/// A pull read in full: detail, checks of the head and reviews.
#[derive(Clone, Debug)]
pub struct PullRead {
    pub pull: GhPull,
    pub reviews: Vec<Value>,
    pub association: String,
}

/// The head repository as a key (forks off github.com get the host too).
fn fix_head_repo(c: &Client, p: &mut GhPull) {
    if !c.conn().is_github_com() && !p.head_repo.is_empty() && !p.head_repo.starts_with("fork:") {
        p.head_repo = c.conn().repo_key(&p.head_repo);
    }
}

/// Detail, checks of the head and reviews of one pull (conditional calls on GitHub).
pub async fn read_pull(c: &Client, repo: &str, number: i64, caller: &str) -> Result<PullRead> {
    let store = c.core().store();
    let api = c.api();
    let prev = store.gh_pull(repo, number).await?;
    let detail = api.pull(c, repo, number, caller).await?;
    let mut p = parse::pull(repo, &detail, prev.as_ref(), now_ms());
    fix_head_repo(c, &mut p);
    if !p.head_sha.is_empty() {
        let runs = api.checks(c, repo, &p.head_sha, caller).await?;
        p.checks = parse::checks(&runs);
        p.checks_state = p.checks.state().into();
    }
    let reviews = api.reviews(c, repo, number, caller).await?;
    p.review_decision = parse::review_decision(&reviews, &p.author);
    Ok(PullRead {
        pull: p,
        reviews,
        association: detail
            .get("author_association")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Reads one pull in full, stores it, emits its event, and captures the
/// delivery facts once it merged. Returns the stored pull.
pub async fn refresh_pull(c: &Client, repo: &str, number: i64, caller: &str) -> Result<GhPull> {
    let read = read_pull(c, repo, number, caller).await?;
    store_pull(c, &read, caller).await?;
    Ok(c.core()
        .store()
        .gh_pull(repo, number)
        .await?
        .unwrap_or(read.pull))
}

/// The paths an open pull changes, for path conditions: read once per head
/// commit (cached under `files|<connection>|<repo>#<n>`), `None` when unknown.
async fn pull_files(
    c: &Client,
    p: &GhPull,
    old: Option<&GhPull>,
    caller: &str,
) -> Option<Vec<String>> {
    if p.state != "open" || p.head_sha.is_empty() {
        return None;
    }
    let store = c.core().store();
    let key = format!("files|{}|{}#{}", c.conn().id, p.repo, p.number);
    if let Ok(Some(e)) = store.gh_etag(&key).await
        && e.etag == p.head_sha
    {
        return serde_json::from_str(&e.body).ok();
    }
    if old.is_some_and(|o| o.head_sha == p.head_sha) {
        return None;
    }
    let files = c
        .api()
        .pull_files(c, &p.repo, p.number, caller)
        .await
        .ok()?;
    let _ = store
        .gh_put_etag(&key, &p.head_sha, "", &serde_json::to_string(&files).ok()?)
        .await;
    Some(files)
}

async fn store_pull(c: &Client, read: &PullRead, caller: &str) -> Result<Upsert> {
    let store = c.core().store();
    let prev = store.gh_pull(&read.pull.repo, read.pull.number).await?;
    let p = &read.pull;
    let up = store.gh_upsert_pull(p).await?;
    if up != Upsert::Unchanged {
        let kind = if up == Upsert::Inserted && p.state == "open" {
            kinds::PULL_OPENED
        } else if p.merged && !prev.as_ref().is_some_and(|x| x.merged) {
            kinds::PULL_MERGED
        } else if p.state == "closed" && prev.as_ref().is_some_and(|x| x.state == "open") {
            kinds::PULL_CLOSED
        } else {
            kinds::PULL_UPDATED
        };
        // What changed, for the engine's webhook and label triggers.
        let old = prev.as_ref();
        let added: Vec<&String> = p
            .labels
            .iter()
            .filter(|l| !old.is_some_and(|o| o.labels.contains(l)))
            .collect();
        let removed: Vec<&String> = old
            .map(|o| o.labels.iter().filter(|l| !p.labels.contains(l)).collect())
            .unwrap_or_default();
        let was_pending = old.is_some_and(|o| o.checks_state == "pending");
        let files = pull_files(c, p, old, caller).await;
        let (labeled_by, labels_by_factory) =
            label_facts(c, &p.repo, p.number, true, &added, caller).await;
        let authored = factory_authored(c, &p.repo, Authored::Pull, p.number, &p.author, "").await;
        // Labels added: whether the factory added them (a person labelling
        // the factory's pull still counts); otherwise who authored it.
        let from_factory = labels_by_factory || (added.is_empty() && authored);
        c.core()
            .emit(
                kind,
                &format!("{}#{}", p.repo, p.number),
                &json!({ "repo": p.repo, "number": p.number, "title": p.title, "state": p.state,
                         "mergeable": p.mergeable, "checks": p.checks_state,
                         "review": p.review_decision, "draft": p.draft,
                         "factoryAuthored": authored, "headSha": p.head_sha,
                         "url": p.html_url, "author": p.author, "head": p.head_ref,
                         "labels": p.labels, "labelsAdded": added, "labelsRemoved": removed,
                         "labeledBy": labeled_by, "fromFactory": from_factory,
                         "files": files,
                         "base": p.base_ref, "fork": p.is_fork(),
                         "readyForReview": old.is_some_and(|o| o.draft) && !p.draft,
                         "convertedToDraft": old.is_some_and(|o| !o.draft) && p.draft,
                         "checksCompleted": was_pending
                             && matches!(p.checks_state.as_str(), "success" | "failure"),
                         "connection": c.conn().id }),
            )
            .await?;
        // An open pull that just turned conflicted: input to conflict-fix automation.
        if p.state == "open"
            && p.mergeable == "conflict"
            && !old.is_some_and(|o| o.mergeable == "conflict")
        {
            c.core()
                .emit(
                    kinds::CONFLICT_DETECTED,
                    &format!("{}#{}", p.repo, p.number),
                    &json!({ "repo": p.repo, "number": p.number, "url": p.html_url,
                             "head": p.head_ref, "headSha": p.head_sha, "base": p.base_ref,
                             "fork": p.is_fork(), "author": p.author, "connection": c.conn().id }),
                )
                .await?;
        }
    }
    reviews_submitted(c, read, prev.is_some()).await?;
    if p.merged && !prev.as_ref().is_some_and(|x| x.delivery_captured) {
        capture_pull(c, read, caller).await?;
    }
    Ok(up)
}

/// `pull_request_review.submitted`: reviews newer than the pull's cursor
/// (`cursor|<connection>|review|<repo>#<n>`). The first read of a pull only
/// sets the cursor, so old reviews do not fire triggers.
async fn reviews_submitted(c: &Client, read: &PullRead, seen_before: bool) -> Result<usize> {
    let p = &read.pull;
    let ck = format!("{}#{}", p.repo, p.number);
    let max = read.reviews.iter().filter_map(|r| r["id"].as_i64()).max();
    let Some(max) = max else {
        return Ok(0);
    };
    let last = crate::events::cursor(c, "review", &ck)
        .await?
        .and_then(|s| s.parse::<i64>().ok());
    let mut n = 0;
    if let Some(last) = last.filter(|_| seen_before) {
        let mut new: Vec<&Value> = read
            .reviews
            .iter()
            .filter(|r| r["id"].as_i64().is_some_and(|id| id > last))
            .filter(|r| r["state"].as_str().is_some_and(|s| s != "PENDING"))
            .collect();
        new.sort_by_key(|r| r["id"].as_i64());
        for r in new {
            let body: String = r["body"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect();
            let author = r["user"]["login"].as_str().unwrap_or("");
            c.core()
                .emit(
                    kinds::REVIEW_SUBMITTED,
                    &ck,
                    &json!({ "repo": p.repo, "number": p.number, "id": r["id"], "action": "submitted",
                             "state": r["state"].as_str().unwrap_or("").to_ascii_lowercase(),
                             "author": author,
                             "authorAssociation": r["author_association"],
                             "body": body, "url": r["html_url"],
                             "headSha": p.head_sha, "head": p.head_ref, "base": p.base_ref,
                             "fork": p.is_fork(), "prAuthor": p.author,
                             "fromFactory": body.contains("keleustes:review"),
                             "connection": c.conn().id }),
                )
                .await?;
            n += 1;
        }
    }
    if last != Some(max) {
        crate::events::set_cursor(c, "review", &ck, &max.to_string()).await?;
    }
    Ok(n)
}

/// Mirrors the open pulls of a repository. A pull whose list entry changed
/// (or whose mergeability is still unknown, or checks pending) is read in
/// full; a pull that left the open list is read once more to learn whether
/// it merged. Returns how many pulls changed.
pub async fn sync_pulls(c: &Client, repo: &str, caller: &str) -> Result<usize> {
    let store = c.core().store();
    let list = c.api().open_pulls(c, repo, caller).await?;
    let stored = store.gh_pulls(Some(repo), Some("open")).await?;
    let mut changed = 0;
    let mut seen = BTreeSet::new();
    for item in &list.items {
        let number = item.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        if number == 0 {
            continue;
        }
        seen.insert(number);
        let prev = stored.iter().find(|p| p.number == number);
        let mut p = parse::pull(repo, item, prev, now_ms());
        fix_head_repo(c, &mut p);
        let stale = match prev {
            None => true,
            Some(o) => {
                o.updated_at != p.updated_at
                    || o.head_sha != p.head_sha
                    || o.mergeable == "unknown"
                    || o.checks_state == "pending"
            }
        };
        if stale {
            let read = read_pull(c, repo, number, caller).await?;
            if store_pull(c, &read, caller).await? != Upsert::Unchanged {
                changed += 1;
            }
        }
    }
    for gone in stored.iter().filter(|p| !seen.contains(&p.number)) {
        match read_pull(c, repo, gone.number, caller).await {
            Ok(read) => {
                if store_pull(c, &read, caller).await? != Upsert::Unchanged {
                    changed += 1;
                }
            }
            // Deleted with its repository, or no longer visible: mark closed.
            Err(e)
                if matches!(
                    e.downcast_ref::<GhError>(),
                    Some(GhError::Http { status: 404, .. })
                ) =>
            {
                let mut p = gone.clone();
                p.state = "closed".into();
                store.gh_upsert_pull(&p).await?;
                changed += 1;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(changed)
}

// ---- pull request actions ---------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum PullAction {
    Ready,
    Draft,
    UpdateBranch,
    /// Logins, or `org/team-slug` for a team (1 to 15).
    RequestReviewers(Vec<String>),
    /// "merge", "squash" or "rebase".
    Merge(String),
}

impl PullAction {
    pub fn name(&self) -> &'static str {
        match self {
            PullAction::Ready => "ready",
            PullAction::Draft => "draft",
            PullAction::UpdateBranch => "update_branch",
            PullAction::RequestReviewers(_) => "request_reviewers",
            PullAction::Merge(_) => "merge",
        }
    }
}

/// Runs one action on one pull, then re-reads the pull.
pub async fn pull_action(
    c: &Client,
    repo: &str,
    number: i64,
    action: &PullAction,
    caller: &str,
) -> Result<GhPull> {
    if !valid_key(repo) || number <= 0 {
        bail!("bad pull request {repo}#{number}");
    }
    let store = c.core().store();
    let api = c.api();
    match action {
        PullAction::Ready => {
            api.set_draft(c, repo, number, false, caller).await?;
            store.gh_set_pull_draft(repo, number, false).await?;
        }
        PullAction::Draft => {
            crate::forge::api::require(c, "draft")?;
            api.set_draft(c, repo, number, true, caller).await?;
            store.gh_set_pull_draft(repo, number, true).await?;
        }
        PullAction::UpdateBranch => {
            let sha = store
                .gh_pull(repo, number)
                .await?
                .map(|p| p.head_sha)
                .unwrap_or_default();
            api.update_branch(c, repo, number, &sha, caller).await?;
        }
        PullAction::RequestReviewers(who) => {
            if who.is_empty() || who.len() > 15 {
                bail!("request 1 to 15 reviewers");
            }
            let (teams, users): (Vec<String>, Vec<String>) =
                who.iter().cloned().partition(|w| w.contains('/'));
            let teams: Vec<String> = teams
                .iter()
                .filter_map(|t| t.split_once('/').map(|x| x.1.to_string()))
                .collect();
            if !teams.is_empty() {
                crate::forge::api::require(c, "team_reviewers")?;
            }
            api.request_reviewers(c, repo, number, &users, &teams, caller)
                .await?;
        }
        PullAction::Merge(method) => {
            if !matches!(method.as_str(), "merge" | "squash" | "rebase") {
                bail!("merge method must be merge, squash or rebase");
            }
            // The factory never merges: only a person's click does, and only
            // where the repository's autonomy allows changes.
            if guard::automated(caller) {
                guard::audit(
                    c.core(),
                    caller,
                    "merge",
                    &format!("{repo}#{number}"),
                    &c.conn().id,
                    "refused",
                    json!({ "reason": "factory_never_merges" }),
                )
                .await;
                bail!("the factory never merges; a person merges from Repos");
            }
            if let Some(r) = store.gh_repo(repo).await?
                && r.settings.autonomy == "observe"
            {
                guard::audit(
                    c.core(),
                    caller,
                    "merge",
                    &format!("{repo}#{number}"),
                    &c.conn().id,
                    "refused",
                    json!({ "reason": "autonomy_observe" }),
                )
                .await;
                bail!("{repo} is set to observe; raise its autonomy to merge from Keleustes");
            }
            let sha = store
                .gh_pull(repo, number)
                .await?
                .map(|p| p.head_sha)
                .unwrap_or_default();
            api.merge(c, repo, number, method, &sha, caller).await?;
        }
    }
    c.invalidate(&api.pull_path(c, repo, number)).await?;
    c.core()
        .emit(
            kinds::PULL_ACTION,
            &format!("{repo}#{number}"),
            &json!({ "repo": repo, "number": number, "action": action.name(),
                     "by": guard::actor(caller).1, "caller": caller, "connection": c.conn().id }),
        )
        .await?;
    refresh_pull(c, repo, number, caller).await
}

/// One result of a bulk action.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BulkResult {
    pub repo: String,
    pub number: i64,
    pub ok: bool,
    pub error: String,
}

/// 1 to 50 distinct targets; each runs on its own and reports its result.
pub async fn bulk_pull_action(
    c: &Client,
    targets: &[(String, i64)],
    action: &PullAction,
    caller: &str,
) -> Result<Vec<BulkResult>> {
    if targets.is_empty() || targets.len() > 50 {
        bail!("choose 1 to 50 pull requests");
    }
    let uniq: BTreeSet<&(String, i64)> = targets.iter().collect();
    if uniq.len() != targets.len() {
        bail!("a pull request is listed twice");
    }
    let mut out = Vec::new();
    for (repo, number) in targets {
        let r = pull_action(c, repo, *number, action, caller).await;
        out.push(BulkResult {
            repo: repo.clone(),
            number: *number,
            ok: r.is_ok(),
            error: r.err().map(|e| format!("{e:#}")).unwrap_or_default(),
        });
    }
    Ok(out)
}

// ---- issues ----------------------------------------------------------------

/// Mirrors issues updated since the newest one stored (not pull requests).
/// Emits `github.issue.opened|updated|closed` with the facts a board card
/// needs. Returns how many changed.
pub async fn sync_issues(c: &Client, repo: &str, caller: &str) -> Result<usize> {
    let store = c.core().store();
    let since = store.gh_issues_cursor(repo).await?;
    let list = c.api().issues_since(c, repo, since, caller).await?;
    if list.not_modified {
        return Ok(0);
    }
    let now = now_ms();
    let cards = store.gh_repo(repo).await?.is_some_and(|r| r.mirror_issues)
        || c.core().issue_mirror_of(repo).await?.is_some();
    let mut changed = 0;
    for v in &list.items {
        if v.get("pull_request").is_some() {
            continue;
        }
        let i = parse::issue(repo, v, now);
        if i.number == 0 {
            continue;
        }
        let prev = store.gh_issue(repo, i.number).await?;
        let up = store.gh_upsert_issue(&i).await?;
        if up == Upsert::Unchanged {
            continue;
        }
        changed += 1;
        let was_open = prev.as_ref().map(|p| p.state == "open");
        let kind = match (up, i.state.as_str(), was_open) {
            (Upsert::Inserted, "open", _) => kinds::ISSUE_OPENED,
            (_, "closed", Some(true)) | (Upsert::Inserted, "closed", _) => kinds::ISSUE_CLOSED,
            _ => kinds::ISSUE_UPDATED,
        };
        let stored_link = prev.as_ref().and_then(|p| p.card_id.clone());
        let mut card_id = stored_link
            .clone()
            .filter(|id| !id.starts_with(kls_core::board::UNLINKED_PREFIX));
        if cards {
            let mut linked = i.clone();
            linked.card_id = stored_link;
            match issue_card(c.core(), &linked).await {
                Ok(id) => card_id = id.or(card_id),
                Err(e) => tracing::warn!(repo, number = i.number, "issue card: {e:#}"),
            }
        }
        let added: Vec<&String> = i
            .labels
            .iter()
            .filter(|l| !prev.as_ref().is_some_and(|p| p.labels.contains(l)))
            .collect();
        let removed: Vec<&String> = prev
            .as_ref()
            .map(|p| p.labels.iter().filter(|l| !i.labels.contains(l)).collect())
            .unwrap_or_default();
        let (labeled_by, labels_by_factory) =
            label_facts(c, repo, i.number, false, &added, caller).await;
        let authored =
            factory_authored(c, repo, Authored::Issue, i.number, &i.author, &i.body).await;
        let from_factory = labels_by_factory || (added.is_empty() && authored);
        c.core()
            .emit(
                kind,
                &format!("{repo}#{}", i.number),
                &json!({ "repo": repo, "number": i.number, "title": i.title, "state": i.state,
                         "stateReason": i.state_reason, "labels": i.labels,
                         "labelsAdded": added, "labelsRemoved": removed,
                         "labeledBy": labeled_by, "fromFactory": from_factory,
                         "factoryAuthored": authored,
                         "connection": c.conn().id,
                         "reopened": was_open == Some(false) && i.state == "open",
                         "assignees": i.assignees, "url": i.html_url,
                         "author": i.author, "authorAssociation": i.author_association,
                         "cardId": card_id }),
            )
            .await?;
        if i.state == "closed" && i.state_reason != "not_planned" {
            capture_issue(c, &i).await?;
        }
    }
    Ok(changed)
}

/// Actor on card activity written by the mirror.
pub const CARD_ACTOR: &str = "github";

fn card_title(i: &GhIssue) -> String {
    let t = format!("#{} {}", i.number, i.title.trim());
    t.chars().take(kls_core::board::TITLE_MAX).collect()
}

/// Whether a person unlinked this issue from its card
/// ([`kls_core::board::UNLINKED_PREFIX`]).
pub fn unlinked(i: &GhIssue) -> bool {
    i.card_id
        .as_deref()
        .is_some_and(|s| s.starts_with(kls_core::board::UNLINKED_PREFIX))
}

/// Keeps the board card of a mirrored issue (repositories with issue
/// mirroring on, or mirrored by a board project): an open issue without a
/// card gets one in the project's inbound list (triage by default), linked
/// through `gh_issues.card_id`; a linked card takes what changed on GitHub
/// since the last sync (title, state, body, labels: a closed issue moves the
/// card to done, a reopened one back to triage), but a field the board
/// changed too is left as a conflict, never overwritten
/// ([`kls_core::board::sync::plan`]). A card the owner deleted stays
/// deleted. Idempotent. Returns the card id.
pub async fn issue_card(core: &kls_core::Core, i: &GhIssue) -> Result<Option<String>> {
    use kls_core::board::{CardDraft, LABELS_MAX, sync as bsync};
    let row = core.issue_mirror_of(&i.repo).await?;
    // A person unlinked this issue from its card (`unlinked:<card id>`):
    // mirrored, no card wanted (no create, no follow).
    if unlinked(i) {
        return Ok(None);
    }
    let linked = match i.card_id.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => core.card(id).await?,
        None => None,
    };
    let Some(card) = linked else {
        let inbound = row.as_ref().map(|r| r.inbound.as_str()).unwrap_or("triage");
        if i.state != "open" || inbound == "off" {
            return Ok(None);
        }
        let mut labels: Vec<String> = i.labels.iter().take(LABELS_MAX).cloned().collect();
        if let Some(r) = &row
            && labels.len() < LABELS_MAX
        {
            labels.push(r.label());
        }
        let base: std::collections::BTreeMap<String, String> = bsync::github_values(i)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let meta = json!({
            "github": { "repo": i.repo, "number": i.number, "url": i.html_url,
                        "project": row.as_ref().map(|r| r.project.clone()) },
            "keleustes": { "syncBase": base },
        });
        let card = core
            .create_card(
                CardDraft {
                    title: card_title(i),
                    body: bsync::issue_body_on_card(i),
                    status: inbound.into(),
                    labels,
                    repo: i.repo.clone(),
                    meta_json: meta.to_string(),
                    ..Default::default()
                },
                CARD_ACTOR,
            )
            .await?;
        core.store()
            .gh_link_issue_card(&i.repo, i.number, &card.id)
            .await?;
        return Ok(Some(card.id));
    };
    if card.deleted {
        return Ok(Some(card.id));
    }
    let rules = row.map(|r| r.rules).unwrap_or_default();
    reconcile(core, card, i, &rules, None).await?;
    Ok(i.card_id.clone())
}

/// Opens an issue for a board card (the drawer's Open issue) and links the
/// card to it. Automation is refused under the kill switch; the create is
/// audited with the connection id (`issue.open`), the link by
/// [`kls_core::Core::link_card_issue`]. The card's sync base starts at the
/// title and state both sides now share. Returns `(number, url)`.
pub async fn open_issue(
    c: &Client,
    card: &kls_store::board::Card,
    body: &str,
    caller: &str,
) -> Result<(i64, String)> {
    use kls_core::board::sync as bsync;
    let core = c.core();
    let conn = c.conn().id.clone();
    let target = format!("{}:{}", card.repo, card.key);
    if !valid_repo(&card.repo) {
        bail!("the card names no repository");
    }
    guard::check_kill(core, caller, "issue.open", &target, &conn).await?;
    let title = card.title.trim().to_string();
    let r = c
        .api()
        .create_issue(c, &card.repo, &title, body, &card.labels, caller)
        .await;
    let v = match r {
        Ok(v) => v,
        Err(e) => {
            let out: String = format!("error: {e:#}").chars().take(200).collect();
            guard::audit(
                core,
                caller,
                "issue.open",
                &target,
                &conn,
                &out,
                json!({ "card": card.key }),
            )
            .await;
            return Err(e);
        }
    };
    let Some(n) = v["number"].as_i64() else {
        bail!("the forge returned no issue number");
    };
    let url = v["html_url"].as_str().unwrap_or("").to_string();
    note_authored(c, &card.repo, Authored::Issue, n, caller).await;
    guard::audit(
        core,
        caller,
        "issue.open",
        &format!("{}#{n}", card.repo),
        &conn,
        "ok",
        json!({ "card": card.key, "url": url }),
    )
    .await;
    let (_, actor) = guard::actor(caller);
    let linked = core
        .link_card_issue(&card.id, &card.repo, n, &url, &actor)
        .await?;
    // Both sides agree on the title (in board form, `#<n> <title>`, as on
    // mirrored cards) and the state now.
    let board_title: String = format!("#{n} {title}")
        .chars()
        .take(kls_core::board::TITLE_MAX)
        .collect();
    let mut base = bsync::base_of(&linked);
    base.insert("title".into(), board_title.clone());
    base.insert(
        "state".into(),
        if linked.status == "done" {
            "closed"
        } else {
            "open"
        }
        .into(),
    );
    let patch = kls_core::board::CardPatch {
        title: Some(board_title),
        meta_json: Some(bsync::with_base(&linked.meta_json, &base)),
        ..Default::default()
    };
    core.update_card(&linked.id, linked.version, patch, CARD_ACTOR)
        .await?;
    Ok((n, url))
}

/// What a sync pass did for the cards of one repository.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CardSync {
    /// Cards whose fields were written to their issue.
    pub pushed: usize,
    /// Cards that took fields from their issue.
    pub taken: usize,
    /// Cards with an open field conflict.
    pub conflicts: usize,
    /// Pushes that failed (a retry is scheduled).
    pub failed: usize,
    /// Pushes waiting for their retry time.
    pub waiting: usize,
    /// Pushes refused by the kill switch.
    pub refused: usize,
}

/// First retry after a failed push; doubles each attempt up to [`RETRY_MAX_MS`].
pub const RETRY_BASE_MS: i64 = 60_000;
pub const RETRY_MAX_MS: i64 = 3_600_000;

/// The wait before retry `attempt` (1-based).
pub fn retry_backoff(attempt: i64) -> i64 {
    let shift = (attempt - 1).clamp(0, 16) as u32;
    (RETRY_BASE_MS << shift).min(RETRY_MAX_MS)
}

/// The outbound half of a pass for a card, set by [`sync_cards`].
struct Outbound<'a> {
    c: &'a Client,
    caller: &'a str,
    /// Push the board's changes (off: an inbound-only mirror).
    outbound: bool,
    rep: &'a mut CardSync,
}

/// Plans one card against its issue, takes GitHub's changes, records
/// conflicts (escalating old ones), and with `out` pushes the board's
/// changes. One card write at the end.
async fn reconcile(
    core: &kls_core::Core,
    card: kls_store::board::Card,
    i: &GhIssue,
    rules: &std::collections::BTreeMap<String, String>,
    out: Option<(Outbound<'_>, i64)>,
) -> Result<()> {
    use kls_core::board::{VersionConflict, sync as bsync};
    let now = now_ms();
    let plan = bsync::plan(&card, i, rules);
    let mut base = plan.base.clone();
    let mut patch = bsync::take_patch(&card, &plan.take);
    for (f, v) in &plan.take {
        base.insert(f.clone(), v.clone());
    }
    // Conflicts: when each was first seen, escalated once when old.
    let (old_since, old_esc) = bsync::conflict_since(&card);
    let mut since = std::collections::BTreeMap::new();
    for f in &plan.conflicts {
        since.insert(
            f.field.clone(),
            old_since.get(&f.field).copied().unwrap_or(now),
        );
    }
    let mut escalated = old_esc && !since.is_empty();
    let mut retry = bsync::retry_of(&card);
    let target = format!("{}#{}", i.repo, i.number);
    if let Some((o, hours)) = out {
        let oldest = since.values().min().copied();
        if hours > 0
            && !escalated
            && let Some(t) = oldest
            && now - t >= hours * 3_600_000
        {
            escalated = true;
            let fields: Vec<&str> = plan.conflicts.iter().map(|f| f.field.as_str()).collect();
            core.emit(
                bsync::SYNC_ESCALATED,
                &target,
                &json!({ "repo": i.repo, "number": i.number, "cardId": card.id,
                         "key": card.key, "fields": fields, "since": t, "hours": hours }),
            )
            .await?;
            guard::audit(
                core,
                o.caller,
                "sync.escalate",
                &target,
                &o.c.conn().id,
                "escalated",
                json!({ "card": card.key, "fields": fields, "hours": hours }),
            )
            .await;
        }
        if !plan.conflicts.is_empty() {
            o.rep.conflicts += 1;
        }
        if !plan.take.is_empty() {
            o.rep.taken += 1;
        }
        if !plan.push.is_empty() && o.outbound {
            push(o, i, &card, &plan.push, &mut base, &mut retry, now).await?;
        }
    }
    let meta = bsync::with_conflicts(
        &bsync::with_retry(&bsync::with_base(&card.meta_json, &base), retry.as_ref()),
        &since,
        escalated,
    );
    let meta_same = {
        let a: Value = serde_json::from_str(&meta).unwrap_or(Value::Null);
        let b: Value = serde_json::from_str(&card.meta_json).unwrap_or(Value::Null);
        a == b
    };
    if !meta_same {
        patch.meta_json = Some(meta);
    }
    let empty = patch.title.is_none()
        && patch.body.is_none()
        && patch.status.is_none()
        && patch.labels.is_none()
        && patch.meta_json.is_none();
    if empty {
        return Ok(());
    }
    match core
        .update_card(&card.id, card.version, patch, CARD_ACTOR)
        .await
    {
        Ok(_) => Ok(()),
        // The card moved under us: the next pass plans again.
        Err(e) if e.downcast_ref::<VersionConflict>().is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Writes the board's changed fields to the issue; on success moves the base
/// and the stored issue, on failure schedules a retry.
async fn push(
    o: Outbound<'_>,
    i: &GhIssue,
    card: &kls_store::board::Card,
    fields: &[(String, String)],
    base: &mut std::collections::BTreeMap<String, String>,
    retry: &mut Option<kls_core::board::sync::Retry>,
    now: i64,
) -> Result<()> {
    use crate::forge::api::IssueEdit;
    use kls_core::board::sync as bsync;
    let Outbound { c, caller, rep, .. } = o;
    if let Some(r) = retry.as_ref() {
        if r.unsupported {
            return Ok(());
        }
        if r.retry_at > now {
            rep.waiting += 1;
            return Ok(());
        }
    }
    let target = format!("{}#{}", i.repo, i.number);
    let conn = c.conn().id.clone();
    // Kill switch first: refused, emitted and audited; no attempt is spent.
    if guard::check_kill(c.core(), caller, "issue.push", &target, &conn)
        .await
        .is_err()
    {
        rep.refused += 1;
        return Ok(());
    }
    let mut edit = IssueEdit::default();
    let mut stored = i.clone();
    for (f, v) in fields {
        match f.as_str() {
            "title" => {
                let t = bsync::title_to_issue(v, i.number);
                stored.title = t.clone();
                edit.title = Some(t);
            }
            "state" => {
                stored.state = v.clone();
                edit.state = Some(v.clone());
            }
            "body" => {
                stored.body = v.clone();
                edit.body = Some(v.clone());
            }
            "labels" => {
                let l: Vec<String> = v.lines().map(str::to_string).collect();
                stored.labels = l.clone();
                edit.labels = Some(l);
            }
            _ => {}
        }
    }
    let names: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).collect();
    let attempt = retry.as_ref().map(|r| r.attempt).unwrap_or(0) + 1;
    match c
        .api()
        .update_issue(c, &i.repo, i.number, &edit, caller)
        .await
    {
        Ok(()) => {
            for (f, v) in fields {
                base.insert(f.clone(), v.clone());
            }
            *retry = None;
            if stored.state == "closed" && stored.closed_at.is_none() {
                stored.closed_at = Some(now);
            }
            c.core().store().gh_upsert_issue(&stored).await?;
            c.core()
                .emit(
                    kinds::ISSUE_PUSHED,
                    &target,
                    &json!({ "repo": i.repo, "number": i.number, "cardId": card.id,
                             "key": card.key, "fields": names, "attempt": attempt,
                             "closed": edit.state.as_deref() == Some("closed"),
                             "connection": conn }),
                )
                .await?;
            rep.pushed += 1;
        }
        Err(e) if matches!(e.downcast_ref::<GhError>(), Some(GhError::Stopped(_))) => {
            rep.refused += 1;
        }
        Err(e) => {
            let unsupported = matches!(e.downcast_ref::<GhError>(), Some(GhError::Unsupported(_)));
            let retry_at = if unsupported {
                0
            } else {
                now + retry_backoff(attempt)
            };
            let error: String = format!("{e:#}").chars().take(200).collect();
            *retry = Some(bsync::Retry {
                attempt,
                retry_at,
                error: error.clone(),
                unsupported,
            });
            c.core()
                .emit(
                    kinds::ISSUE_PUSH_FAILED,
                    &target,
                    &json!({ "repo": i.repo, "number": i.number, "cardId": card.id,
                             "key": card.key, "fields": names, "attempt": attempt,
                             "retryAt": retry_at, "unsupported": unsupported,
                             "error": error, "connection": conn }),
                )
                .await?;
            guard::audit(
                c.core(),
                caller,
                "issue.push",
                &target,
                &conn,
                if unsupported { "unsupported" } else { "retry" },
                json!({ "card": card.key, "fields": names, "attempt": attempt,
                        "retryAt": retry_at, "error": error }),
            )
            .await;
            rep.failed += 1;
        }
    }
    Ok(())
}

/// The outbound sync of one repository's mirrored cards: each linked card
/// is planned against its stored issue (read by [`sync_issues`] first);
/// GitHub's changes are taken, the board's are written to the issue (a card
/// moved to done closes its issue), conflicts wait for the owner, and a
/// failed write is retried with backoff on a later pass. Under the kill
/// switch nothing is written (refused, emitted and audited).
pub async fn sync_cards(c: &Client, repo: &str, caller: &str) -> Result<CardSync> {
    let core = c.core();
    let row = core.issue_mirror_of(repo).await?;
    let outbound = row.as_ref().is_none_or(|r| r.outbound);
    let rules = row.as_ref().map(|r| r.rules.clone()).unwrap_or_default();
    let hours = row.as_ref().map(|r| r.escalate_hours).unwrap_or(0);
    let mut rep = CardSync::default();
    for i in core.store().gh_issues(Some(repo), None).await? {
        if unlinked(&i) {
            continue;
        }
        let Some(id) = i.card_id.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(card) = core.store().card(id).await? else {
            continue;
        };
        if card.deleted {
            continue;
        }
        let o = Outbound {
            c,
            caller,
            outbound,
            rep: &mut rep,
        };
        reconcile(core, card, &i, &rules, Some((o, hours))).await?;
    }
    Ok(rep)
}

/// What a backfill pass did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Backfill {
    /// Issues opened for existing cards this pass.
    pub opened: usize,
    /// Cards still to open (the next pass takes them).
    pub left: usize,
    /// The backfill finished this pass.
    pub done: bool,
}

/// The mirror row's "existing cards" choice (web `MirrorBackfill`): once a
/// project starts mirroring a repository with `recent` or `all`, its cards
/// that were already on the board open as issues, at most
/// [`bsync::BACKFILL_PER_PASS`] a pass, through [`open_issue`] (kill switch,
/// audit, link). Board-only `project:` labels stay on the card. Needs the
/// row's outbound switch; when no card is left the row is marked done
/// (audited) and later cards follow the normal mirror rules.
///
/// [`bsync::BACKFILL_PER_PASS`]: kls_core::board::sync::BACKFILL_PER_PASS
pub async fn backfill_cards(c: &Client, repo: &str, caller: &str) -> Result<Backfill> {
    use kls_core::board::sync as bsync;
    let core = c.core();
    let mut out = Backfill::default();
    let Some(row) = core.issue_mirror_of(repo).await? else {
        return Ok(out);
    };
    if !row.backfill_due() || !row.outbound {
        return Ok(out);
    }
    let cards = core.store().cards(false).await?;
    let todo = bsync::backfill_candidates(&row, &cards, now_ms());
    for card in todo.iter().take(bsync::BACKFILL_PER_PASS) {
        let mut card = (*card).clone();
        card.repo = row.repo.clone();
        card.labels
            .retain(|l| !l.starts_with(bsync::BOARD_ONLY_PREFIX));
        let body = {
            let mut b = card.body.trim().to_string();
            if !b.is_empty() {
                b.push_str("\n\n");
            }
            b.push_str(&format!("Board card {}", card.key));
            b
        };
        open_issue(c, &card, &body, caller).await?;
        out.opened += 1;
    }
    out.left = todo.len().saturating_sub(out.opened);
    if out.left == 0 {
        core.mark_mirror_backfilled(repo, out.opened, caller)
            .await?;
        out.done = true;
    }
    Ok(out)
}

/// Syncs one card with its issue now (the drawer's Sync now).
pub async fn sync_card(c: &Client, card_id: &str, caller: &str) -> Result<CardSync> {
    let core = c.core();
    let Some(card) = core.card(card_id).await? else {
        bail!("no such card");
    };
    let Some(i) = core
        .store()
        .gh_issues(Some(&card.repo), None)
        .await?
        .into_iter()
        .find(|i| i.card_id.as_deref() == Some(card.id.as_str()))
    else {
        bail!("{} is not linked to an issue", card.key);
    };
    let row = core.issue_mirror_of(&i.repo).await?;
    let rules = row.as_ref().map(|r| r.rules.clone()).unwrap_or_default();
    let hours = row.as_ref().map(|r| r.escalate_hours).unwrap_or(0);
    let mut rep = CardSync::default();
    let o = Outbound {
        c,
        caller,
        outbound: row.as_ref().is_none_or(|r| r.outbound),
        rep: &mut rep,
    };
    reconcile(core, card, &i, &rules, Some((o, hours))).await?;
    Ok(rep)
}

// ---- labels ----------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LabelSync {
    pub repo: String,
    pub created: Vec<String>,
    pub existing: usize,
    pub error: String,
}

/// Reads a repository's labels; with `create`, adds the missing `sf:`
/// labels (one call each; a label that exists is left as the team has it).
pub async fn sync_labels(c: &Client, repo: &str, create: bool, caller: &str) -> Result<LabelSync> {
    let store = c.core().store();
    let api = c.api();
    let list = api.labels(c, repo, caller).await?;
    let mut have: Vec<_> = list.items.iter().map(|v| parse::label(repo, v)).collect();
    let names: BTreeSet<String> = have.iter().map(|l| l.name.to_lowercase()).collect();
    let mut out = LabelSync {
        repo: repo.into(),
        ..Default::default()
    };
    if create {
        for (name, desc) in crate::labels::DEFAULT_LABELS {
            if names.contains(name) {
                out.existing += 1;
                continue;
            }
            match api
                .create_label(c, repo, name, crate::labels::COLOR, desc, caller)
                .await
            {
                Ok(Some(v)) => {
                    have.push(parse::label(repo, &v));
                    out.created.push(name.to_string());
                }
                Ok(None) => {
                    out.existing += 1;
                }
                Err(e) => {
                    out.error = format!("{e:#}");
                    break;
                }
            }
        }
        if !out.created.is_empty() {
            c.invalidate(&format!("/repos/{}/labels", c.forge_name(repo)))
                .await?;
        }
    }
    store.gh_set_labels(repo, &have).await?;
    if !out.created.is_empty() || !out.error.is_empty() || !list.not_modified {
        c.core().emit(kinds::LABELS_SYNCED, repo, &out).await?;
    }
    Ok(out)
}

// ---- delivery capture --------------------------------------------------------

fn person_from(user: &Value, association: &str, at: Option<i64>) -> Person {
    let login = user.get("login").and_then(|v| v.as_str()).unwrap_or("");
    let bot = parse::is_bot(user);
    Person {
        person_key: format!("gh:{login}"),
        login: login.into(),
        avatar_url: user
            .get("avatar_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into(),
        is_internal: if bot {
            None
        } else {
            parse::is_internal(association)
        },
        association: association.into(),
        is_bot: bot,
        bot_kind: if bot { "bot".into() } else { String::new() },
        first_seen_at: at,
        last_seen_at: at,
        ..Default::default()
    }
}

/// Lines of a commit message that name people: `Signed-off-by`, `Co-authored-by`.
pub fn trailers(message: &str) -> Vec<(String, String, String)> {
    message
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let kind = match k.trim().to_ascii_lowercase().as_str() {
                "signed-off-by" => "signoff",
                "co-authored-by" => "coauthor",
                _ => return None,
            };
            let v = v.trim();
            let (name, email) = match (v.find('<'), v.rfind('>')) {
                (Some(a), Some(b)) if a < b => (v[..a].trim(), v[a + 1..b].trim()),
                _ => (v, ""),
            };
            Some((kind.to_string(), name.to_string(), email.to_lowercase()))
        })
        .collect()
}

/// Writes the outcome, the actions (opened, reviews, commits, sign-offs),
/// the people and the commits of a merged pull, once.
pub async fn capture_pull(c: &Client, read: &PullRead, caller: &str) -> Result<()> {
    let store = c.core().store();
    let p = &read.pull;
    let key = format!("pr:{}#{}", p.repo.to_lowercase(), p.number);
    let repo_l = p.repo.to_lowercase();
    let commits = c.api().pull_commits(c, &p.repo, p.number, caller).await?;
    // Off GitHub the payloads carry no association: ask the forge once per person.
    let mut assoc_of: std::collections::BTreeMap<String, String> = Default::default();
    let association = if read.association.is_empty() {
        let a = c
            .api()
            .permission(c, &p.repo, &p.author, caller)
            .await
            .unwrap_or_default();
        assoc_of.insert(p.author.clone(), a.clone());
        a
    } else {
        read.association.clone()
    };
    let author_user = json!({ "login": p.author });
    store
        .upsert_person(&person_from(&author_user, &association, Some(p.created_at)))
        .await?;
    let author_bot = p.author.ends_with("[bot]");
    store
        .upsert_action(&HumanAction {
            source: "admin".into(),
            ref_id: format!("pr_opened:{key}"),
            action_kind: "pr_opened".into(),
            actor_key: format!("gh:{}", p.author),
            actor_login: p.author.clone(),
            is_bot: author_bot,
            bot_kind: if author_bot {
                "bot".into()
            } else {
                String::new()
            },
            outcome_key: Some(key.clone()),
            repo: Some(repo_l.clone()),
            occurred_at: p.created_at,
            raw_units_json: json!({ "started": true }).to_string(),
            basis: "derived".into(),
        })
        .await?;
    let mut reviewers = BTreeSet::new();
    let mut first_review: Option<i64> = None;
    let lines = p.additions + p.deletions;
    for r in &read.reviews {
        let user = &r["user"];
        let login = user.get("login").and_then(|v| v.as_str()).unwrap_or("");
        if login.is_empty() || login == p.author {
            continue;
        }
        let at = parse::ts(&r["submitted_at"]).unwrap_or(p.updated_at);
        let bot = parse::is_bot(user);
        let mut assoc = r
            .get("author_association")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if assoc.is_empty() && !c.conn().is_github_com() {
            if !assoc_of.contains_key(login) {
                let a = c
                    .api()
                    .permission(c, &p.repo, login, caller)
                    .await
                    .unwrap_or_default();
                assoc_of.insert(login.to_string(), a);
            }
            assoc = assoc_of[login].clone();
        }
        let assoc = assoc.as_str();
        store
            .upsert_person(&person_from(user, assoc, Some(at)))
            .await?;
        if !bot {
            reviewers.insert(login.to_string());
            first_review = Some(first_review.map_or(at, |f: i64| f.min(at)));
        }
        let comments = r
            .get("body")
            .and_then(|b| b.as_str())
            .map(|b| (!b.trim().is_empty()) as i64)
            .unwrap_or(0);
        store
            .upsert_action(&HumanAction {
                source: "review".into(),
                ref_id: format!(
                    "review:{}",
                    r.get("id").and_then(|v| v.as_i64()).unwrap_or(0)
                ),
                action_kind: r
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase(),
                actor_key: format!("gh:{login}"),
                actor_login: login.into(),
                is_bot: bot,
                bot_kind: if bot { "bot".into() } else { String::new() },
                outcome_key: Some(key.clone()),
                repo: Some(repo_l.clone()),
                occurred_at: at,
                raw_units_json: json!({ "lines": lines, "comments": comments }).to_string(),
                basis: "derived".into(),
            })
            .await?;
    }
    for cm in &commits {
        let sha = cm.get("sha").and_then(|v| v.as_str()).unwrap_or("");
        if sha.is_empty() {
            continue;
        }
        let commit = &cm["commit"];
        let user = &cm["author"];
        let login = user.get("login").and_then(|v| v.as_str()).unwrap_or("");
        let email = commit["author"]["email"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        let name = commit["author"]["name"].as_str().unwrap_or("").to_string();
        let at = parse::ts(&commit["author"]["date"])
            .or_else(|| parse::ts(&commit["committer"]["date"]));
        let bot = parse::is_bot(user) || email.ends_with("[bot]@users.noreply.github.com");
        let author_key = if !login.is_empty() {
            format!("gh:{login}")
        } else if !email.is_empty() {
            format!("email:{email}")
        } else {
            String::new()
        };
        let msg = commit["message"].as_str().unwrap_or("");
        let tr = trailers(msg);
        if !author_key.is_empty() {
            let mut person = person_from(user, "", at);
            person.person_key = author_key.clone();
            person.display_name = name.clone();
            if !email.is_empty() {
                person.emails = vec![email.clone()];
            }
            person.is_bot = bot;
            store.upsert_person(&person).await?;
            store
                .upsert_action(&HumanAction {
                    source: "commit".into(),
                    ref_id: format!("commit:{repo_l}@{sha}"),
                    action_kind: "commit".into(),
                    actor_key: author_key.clone(),
                    actor_login: login.into(),
                    is_bot: bot,
                    bot_kind: if bot { "bot".into() } else { String::new() },
                    outcome_key: Some(key.clone()),
                    repo: Some(repo_l.clone()),
                    occurred_at: at.unwrap_or(p.updated_at),
                    raw_units_json: "{}".into(),
                    basis: "derived".into(),
                })
                .await?;
        }
        for (kind, tname, temail) in &tr {
            if temail.is_empty() || *temail == email {
                continue;
            }
            let k = format!("email:{temail}");
            store
                .upsert_person(&Person {
                    person_key: k.clone(),
                    display_name: tname.clone(),
                    emails: vec![temail.clone()],
                    first_seen_at: at,
                    last_seen_at: at,
                    ..Default::default()
                })
                .await?;
            store
                .upsert_action(&HumanAction {
                    source: "commit".into(),
                    ref_id: format!("{kind}:{repo_l}@{sha}:{temail}"),
                    action_kind: kind.clone(),
                    actor_key: k,
                    outcome_key: Some(key.clone()),
                    repo: Some(repo_l.clone()),
                    occurred_at: at.unwrap_or(p.updated_at),
                    raw_units_json: "{}".into(),
                    basis: "derived".into(),
                    ..Default::default()
                })
                .await?;
        }
        store
            .upsert_commit(&DeliveryCommit {
                repo: repo_l.clone(),
                sha: sha.into(),
                author_name: name,
                author_email: email,
                author_login: login.into(),
                author_key,
                is_bot: bot,
                trailers: tr
                    .iter()
                    .map(|(k, n, e)| format!("{k}: {n} <{e}>"))
                    .collect(),
                signed: commit["verification"]["verified"].as_bool(),
                pr_number: Some(p.number),
                outcome_key: Some(key.clone()),
                committed_at: at,
            })
            .await?;
    }
    let resolved = p.merged_at.or(p.closed_at);
    store
        .upsert_outcome(&Outcome {
            outcome_key: key.clone(),
            kind: "pr".into(),
            title: p.title.clone(),
            repo: repo_l.clone(),
            author: p.author.clone(),
            opened_at: Some(p.created_at),
            resolved_at: resolved,
            result: "merged".into(),
            cycle_ms: resolved.map(|r| (r - p.created_at).max(0)),
            first_review_ms: first_review.map(|f| (f - p.created_at).max(0)),
            reviewers: reviewers.into_iter().collect(),
            commits: Some(commits.len() as i64),
            additions: Some(p.additions),
            deletions: Some(p.deletions),
            pr_ref: format!("{}#{}", repo_l, p.number),
            ..Default::default()
        })
        .await?;
    store.gh_mark_pull_captured(&p.repo, p.number).await?;
    c.core()
        .emit(
            kinds::DELIVERY_CAPTURED,
            &key,
            &json!({ "key": key, "commits": commits.len() }),
        )
        .await?;
    Ok(())
}

/// Writes the outcome of a completed issue and its opener's action.
pub async fn capture_issue(c: &Client, i: &GhIssue) -> Result<()> {
    let store = c.core().store();
    let repo_l = i.repo.to_lowercase();
    let key = format!("issue:{repo_l}#{}", i.number);
    let bot = i.author.ends_with("[bot]");
    store
        .upsert_person(&person_from(
            &json!({ "login": i.author }),
            &i.author_association,
            Some(i.created_at),
        ))
        .await?;
    store
        .upsert_action(&HumanAction {
            source: "admin".into(),
            ref_id: format!("issue_opened:{key}"),
            action_kind: "issue_opened".into(),
            actor_key: format!("gh:{}", i.author),
            actor_login: i.author.clone(),
            is_bot: bot,
            bot_kind: if bot { "bot".into() } else { String::new() },
            outcome_key: Some(key.clone()),
            repo: Some(repo_l.clone()),
            occurred_at: i.created_at,
            raw_units_json: json!({ "started": true }).to_string(),
            basis: "derived".into(),
        })
        .await?;
    store
        .upsert_outcome(&Outcome {
            outcome_key: key.clone(),
            kind: "issue".into(),
            title: i.title.clone(),
            repo: repo_l,
            author: i.author.clone(),
            opened_at: Some(i.created_at),
            resolved_at: i.closed_at,
            result: "closed".into(),
            cycle_ms: i.closed_at.map(|cl| (cl - i.created_at).max(0)),
            card_id: i.card_id.clone().unwrap_or_default(),
            ..Default::default()
        })
        .await?;
    c.core()
        .emit(kinds::DELIVERY_CAPTURED, &key, &json!({ "key": key }))
        .await?;
    Ok(())
}

// ---- authorship provenance -----------------------------------------------------

/// Logins the factory acts as on every forge (beyond Settings `forge.identity`).
pub const FACTORY_LOGINS: [&str; 2] = ["keleustes[bot]", "belai[bot]"];
/// The marker the factory leaves in what it writes (`<!-- keleustes:... -->`).
pub const FACTORY_MARKER: &str = "<!-- keleustes:";

/// What the factory authored on a forge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authored {
    Pull,
    Issue,
    Comment,
}

impl Authored {
    fn tag(self) -> &'static str {
        match self {
            Authored::Pull => "pull",
            Authored::Issue => "issue",
            Authored::Comment => "comment",
        }
    }
}

fn authored_key(c: &Client, repo: &str, what: Authored, id: i64) -> String {
    format!("authored|{}|{repo}|{}|{id}", c.conn().id, what.tag())
}

/// Records that the factory (`caller`, automation only) authored a pull
/// request, issue (by number) or comment (by id), so the mirror's events
/// about it say `fromFactory` (the own-identity guard).
pub async fn note_authored(c: &Client, repo: &str, what: Authored, id: i64, caller: &str) {
    if id <= 0 || !guard::automated(caller) {
        return;
    }
    let _ = c
        .core()
        .store()
        .gh_put_etag(
            &authored_key(c, repo, what, id),
            caller,
            "",
            &now_ms().to_string(),
        )
        .await;
}

/// The logins the factory acts as: [`FACTORY_LOGINS`] and Settings
/// `forge.identity` (comma-separated).
pub async fn factory_logins(core: &kls_core::Core) -> Vec<String> {
    let mut out: Vec<String> = FACTORY_LOGINS.iter().map(|s| s.to_string()).collect();
    if let Ok(Some(v)) = core.store().setting("forge.identity").await {
        out.extend(
            v.trim_matches('"')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    out
}

/// Whether the factory authored this pull, issue or comment: it was noted
/// by [`note_authored`], the body carries [`FACTORY_MARKER`], or the author
/// is one of [`factory_logins`].
pub async fn factory_authored(
    c: &Client,
    repo: &str,
    what: Authored,
    id: i64,
    author: &str,
    body: &str,
) -> bool {
    if body.contains(FACTORY_MARKER) {
        return true;
    }
    let a = author.trim();
    if !a.is_empty()
        && factory_logins(c.core())
            .await
            .iter()
            .any(|l| l.eq_ignore_ascii_case(a))
    {
        return true;
    }
    id > 0
        && matches!(
            c.core()
                .store()
                .gh_etag(&authored_key(c, repo, what, id))
                .await,
            Ok(Some(_))
        )
}

// ---- label provenance ----------------------------------------------------------

/// How long a factory label mark counts (the poll that sees it is minutes away).
pub const LABEL_MARK_MS: i64 = 6 * 3_600_000;

fn label_mark_key(c: &Client, repo: &str, n: i64, label: &str) -> String {
    format!(
        "labeled|{}|{repo}#{n}|{}",
        c.conn().id,
        label.to_lowercase()
    )
}

/// Records that the factory (`caller`) added `labels` to `repo#n`, so the
/// mirror's next event says `fromFactory` (the trigger loop guard).
pub async fn note_labels_added(c: &Client, repo: &str, n: i64, labels: &[String], caller: &str) {
    for l in labels {
        let _ = c
            .core()
            .store()
            .gh_put_etag(
                &label_mark_key(c, repo, n, l),
                caller,
                "",
                &now_ms().to_string(),
            )
            .await;
    }
}

/// `labeledBy` (label -> login, from the forge's issue events) and
/// `fromFactory` (every added label was added by the factory) for an event.
pub async fn label_facts(
    c: &Client,
    repo: &str,
    n: i64,
    is_pull: bool,
    added: &[&String],
    caller: &str,
) -> (Value, bool) {
    if added.is_empty() {
        return (json!({}), false);
    }
    let actors = c
        .api()
        .label_actors(c, repo, n, is_pull, caller)
        .await
        .unwrap_or_default();
    let by: serde_json::Map<String, Value> = added
        .iter()
        .filter_map(|l| actors.get(l.as_str()).map(|w| (l.to_string(), json!(w))))
        .collect();
    let now = now_ms();
    let store = c.core().store();
    let mut from_factory = true;
    for l in added {
        let marked = match store.gh_etag(&label_mark_key(c, repo, n, l)).await {
            Ok(Some(e)) => e
                .body
                .parse::<i64>()
                .is_ok_and(|at| now - at < LABEL_MARK_MS && e.etag != "user"),
            _ => false,
        };
        from_factory &= marked;
    }
    (Value::Object(by), from_factory)
}
