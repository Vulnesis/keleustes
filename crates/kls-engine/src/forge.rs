//! The engine's [`Forge`] on the GitHub plane (`kls-github`): `github`
//! output steps (open a pull request, comment, review, label, mark ready
//! and merge, check runs, file issues) and `forge` gates (checks, merge
//! conflicts, reviews), plus the GitHub event names the `webhook` and
//! `label` triggers match ([`webhook_events`], [`labels_added`]).
//!
//! [`crate::start`] installs it with the process's GitHub service; it
//! reaches GitHub through whatever route that service has (local `gh` or a
//! node), so no token is held here. Every write goes through
//! `kls_github::Client::request`, which checks the kill switch for
//! automation and audits the call with its origin (`factory:<workflow>`);
//! refusals made here (protected branch, readiness) are audited through
//! `forge::guard::refuse`. Each forge kind does what its dialect can and
//! says `unsupported` otherwise (`forge::api::require`).

mod outputs;

use crate::exec::{Forge, GateResult, StepContext, short_id};
use anyhow::{Context, Result, bail};
use kls_core::Core;
use kls_core::board::CardPatch;
use kls_github::forge::api::{IssueEdit, PullIn, StatusIn, status_state};
use kls_github::forge::{attribution, guard};
use kls_github::sync::{self, PullAction};
use kls_github::{Client, Service};
use kls_store::Event;
use kls_store::board::Card;
use kls_store::github::GhPull;
use serde_json::{Value, json};
use std::sync::Arc;

enum Source {
    Service(Arc<Service>),
    Client(Box<Client>),
}

/// GitHub for the engine.
pub struct GitHubForge {
    src: Source,
}

impl GitHubForge {
    /// On the process's GitHub service (its active route).
    pub fn new(service: Arc<Service>) -> Self {
        Self {
            src: Source::Service(service),
        }
    }

    /// On a fixed client (tests).
    pub fn with_client(c: Client) -> Self {
        Self {
            src: Source::Client(Box::new(c)),
        }
    }

    fn core(&self) -> Core {
        match &self.src {
            Source::Service(s) => s.core().clone(),
            Source::Client(c) => c.core().clone(),
        }
    }

    /// The client for the connection `repo` belongs to (github.com when empty).
    async fn client(&self, repo: &str) -> Result<Client> {
        match &self.src {
            Source::Service(s) if repo.trim().is_empty() => s.client().await,
            Source::Service(s) => s.client_for(repo.trim()).await,
            Source::Client(c) => Ok((**c).clone()),
        }
    }
}

/// What a step acts on.
#[derive(Debug, Default)]
struct Target {
    repo: String,
    number: Option<i64>,
    card: Option<Card>,
}

fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().trim_start_matches('#').parse().ok(),
        _ => None,
    }
    .filter(|n| *n > 0)
}

fn as_text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

/// `https://github.com/acme/api/pull/7` -> 7, and the GitLab
/// `…/-/merge_requests/N` and Gitea `…/pulls/N` forms.
fn pull_number_of(url: &str) -> Option<i64> {
    kls_github::forge::pull_ref_of(url).map(|r| r.number)
}

async fn target(core: &Core, ctx: &StepContext) -> Result<Target> {
    let cfg = &ctx.node.config;
    let event = &ctx.context["event"];
    // Event triggers wrap the bus event: {id, kind, subject, data}.
    let data = if event.get("data").is_some_and(Value::is_object) {
        &event["data"]
    } else {
        event
    };
    let inputs = &ctx.context["inputs"];
    let card = if ctx.run.card_id.is_empty() {
        None
    } else {
        core.card(&ctx.run.card_id).await?.filter(|c| !c.deleted)
    };
    let repo = [
        as_text(cfg.get("repo")),
        ctx.run.repo.as_str(),
        as_text(inputs.get("repo")),
        as_text(data.get("repo")),
        card.as_ref().map(|c| c.repo.as_str()).unwrap_or(""),
    ]
    .into_iter()
    .find(|s| !s.trim().is_empty())
    .unwrap_or("")
    .trim()
    .to_string();
    let from_steps = ctx.context["steps"].as_object().and_then(|m| {
        m.values()
            .filter(|s| as_text(s.get("repo")) == repo || as_text(s.get("repo")).is_empty())
            .find_map(|s| s.get("number").and_then(as_int))
    });
    let number = cfg
        .get("number")
        .and_then(as_int)
        .or_else(|| inputs.get("number").and_then(as_int))
        .or_else(|| data.get("number").and_then(as_int))
        .or(from_steps)
        .or_else(|| card.as_ref().and_then(|c| pull_number_of(&c.pr_url)));
    Ok(Target { repo, number, card })
}

fn need_repo(t: &Target) -> Result<&str> {
    if !kls_github::valid_key(&t.repo) {
        bail!("the run names no repository");
    }
    Ok(&t.repo)
}

fn need_number(t: &Target) -> Result<i64> {
    t.number
        .context("the run names no issue or pull request (set number, or start it from one)")
}

fn fill(tpl: &str, ctx: &StepContext, t: &Target) -> String {
    tpl.replace("{repo}", &t.repo)
        .replace("{run}", &short_id(&ctx.run.id))
        .replace("{node}", &ctx.node.title)
        .replace(
            "{number}",
            &t.number.map(|n| n.to_string()).unwrap_or_default(),
        )
        .replace(
            "{card}",
            t.card.as_ref().map(|c| c.key.as_str()).unwrap_or(""),
        )
}

fn message(ctx: &StepContext, t: &Target) -> String {
    let m = ctx.node.str("message").trim();
    if m.is_empty() {
        format!("{} ({})", ctx.node.title, short_id(&ctx.run.id))
    } else {
        fill(m, ctx, t)
    }
}

fn caller(ctx: &StepContext) -> String {
    format!("factory:{}", ctx.run.workflow_id)
}

fn pull_json(p: &GhPull) -> Value {
    json!({
        "repo": p.repo, "number": p.number, "url": p.html_url, "state": p.state,
        "draft": p.draft, "merged": p.merged, "mergeable": p.mergeable,
        "checks": p.checks_state, "review": p.review_decision, "headSha": p.head_sha,
    })
}

/// The run's steps (for scan results and earlier reviews).
async fn run_steps(core: &Core, ctx: &StepContext) -> Vec<kls_store::engine::RunStep> {
    core.store()
        .run_steps(&ctx.run.id)
        .await
        .unwrap_or_default()
}

/// The attribution a factory write carries (web factory_engine_attribution.go):
/// the card's session and the vulnerabilities the run's scans found.
fn attribution_of(t: &Target, found: Option<&outputs::Findings>) -> attribution::Context {
    let mut a = attribution::Context {
        bot: "vulnetix".into(),
        ..Default::default()
    };
    if let Some(card) = &t.card {
        a.session_id = kls_core::board::cards::history(card)
            .into_iter()
            .rev()
            .map(|h| h.session_id)
            .find(|s| !s.is_empty())
            .unwrap_or_default();
    }
    if let Some(f) = found {
        a.vuln_ids = f
            .items
            .iter()
            .filter(|i| i.kind == "vuln")
            .map(|i| i.id.clone())
            .collect();
    }
    a
}

fn signed(body: &str, a: &attribution::Context) -> String {
    attribution::append(body, a, true)
}

/// `belai/K-` + `K-1a2b3c` -> `belai/K-1a2b3c` (no doubled `K-`).
pub fn branch_name(prefix: &str, key: &str) -> String {
    let p = prefix.trim();
    match (p.strip_suffix("K-"), key.strip_prefix("K-")) {
        (Some(_), Some(rest)) => format!("{p}{rest}"),
        _ => format!("{p}{key}"),
    }
}

/// The factory's own login on this forge ("" when the forge will not say).
async fn own_login(c: &Client, who: &str) -> String {
    c.api()
        .viewer(c, who)
        .await
        .ok()
        .and_then(|v| v["login"].as_str().map(str::to_lowercase))
        .unwrap_or_default()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split([',', ' '])
            .map(|s| s.trim().trim_start_matches('@').to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The head commit the run was started for (`headSha` on the event or the
/// inputs), "" when it does not say.
fn run_sha(ctx: &StepContext) -> String {
    let event = &ctx.context["event"];
    let data = if event.get("data").is_some_and(Value::is_object) {
        &event["data"]
    } else {
        event
    };
    [
        as_text(ctx.node.config.get("sha")),
        as_text(ctx.context["inputs"].get("headSha")),
        as_text(data.get("headSha")),
    ]
    .into_iter()
    .find(|s| !s.trim().is_empty())
    .unwrap_or("")
    .trim()
    .to_lowercase()
}

impl GitHubForge {
    /// The base a draft pull request targets: `default` (or unset), a branch
    /// name, or `pr_head`: the head branch of the pull request the run is
    /// about, so a fix stacks onto it. `pr_head` falls back to the default
    /// branch when the run has no pull request, it is closed, or its head is
    /// in a fork.
    async fn pr_base(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<String> {
        let core = c.core();
        let repo = need_repo(t)?;
        let default = core
            .store()
            .gh_repo(repo)
            .await?
            .map(|r| r.default_branch)
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "main".into());
        Ok(match ctx.node.str("base").trim() {
            "" | "default" => default,
            "pr_head" => {
                let Some(n) = t.number else {
                    return Ok(default);
                };
                let p = match core.store().gh_pull(repo, n).await? {
                    Some(p) if !p.head_ref.is_empty() => p,
                    _ => match sync::refresh_pull(c, repo, n, &caller(ctx)).await {
                        Ok(p) => p,
                        Err(_) => return Ok(default),
                    },
                };
                if p.state != "open" || p.is_fork() || p.head_ref.is_empty() {
                    default
                } else {
                    p.head_ref
                }
            }
            b => b.to_string(),
        })
    }

    async fn draft_pr(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        let core = c.core();
        let repo = need_repo(t)?;
        let who = caller(ctx);
        let cfg = &ctx.node.config;
        let eff = kls_core::audit::config::Effective::load(core).await.ok();
        let prefix = eff
            .as_ref()
            .map(|e| e.branch_prefix.clone())
            .unwrap_or_else(|| "belai/K-".into());
        let mut named = false;
        let mut branch = [
            as_text(cfg.get("branch")),
            t.card.as_ref().map(|c| c.branch.as_str()).unwrap_or(""),
            as_text(ctx.context["inputs"].get("branch")),
        ]
        .into_iter()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
        if branch.is_empty()
            && let Some(card) = &t.card
        {
            // The worker's branch is named by `forge.branch_prefix` and the card.
            branch = branch_name(&prefix, &card.key);
            named = true;
        }
        if branch.is_empty() {
            if ctx.node.str("ifNoBranch") == "skip" {
                return Ok(json!({ "skipped": "no branch" }));
            }
            bail!("the run has no branch to open a pull request from");
        }
        let why = guard::protected_branch(c, repo, &branch, &who).await;
        if !why.is_empty() {
            guard::refuse(
                core,
                &who,
                "draft_pr",
                repo,
                &c.conn().id,
                "protected_branch",
                &why,
            )
            .await;
            bail!("refused: {why}; the factory opens pull requests only from its own branches");
        }
        // Idempotent: an open pull request from this branch is the result.
        let existing = core
            .store()
            .gh_pulls(Some(repo), Some("open"))
            .await?
            .into_iter()
            .find(|p| p.head_ref == branch);
        let mut opened = false;
        let pull = match existing {
            Some(p) => p,
            None => {
                let base = self.pr_base(c, ctx, t).await?;
                let title = match (ctx.node.str("title"), &t.card) {
                    ("", Some(card)) => card.title.clone(),
                    ("", None) => format!("{} ({})", ctx.node.title, short_id(&ctx.run.id)),
                    (s, _) => fill(s, ctx, t),
                };
                let mut body = t.card.as_ref().map(|c| c.body.clone()).unwrap_or_default();
                if let Some(card) = &t.card {
                    body.push_str(&format!("\n\nCard: {}", card.key));
                }
                body.push_str(&format!("\nRun: {}", short_id(&ctx.run.id)));
                let steps = run_steps(core, ctx).await;
                let found = outputs::findings(&steps, ctx.step.seq);
                let body = signed(body.trim(), &attribution_of(t, found.as_ref()));
                // The node's state, else Settings `forge.pr_state`.
                let draft = match ctx.node.str("prState") {
                    "ready" => false,
                    "draft" => true,
                    _ => !eff.as_ref().is_some_and(|e| e.pr_ready),
                };
                let created = c
                    .api()
                    .create_pull(
                        c,
                        repo,
                        &PullIn {
                            title,
                            head: branch.clone(),
                            base,
                            body,
                            draft,
                        },
                        &who,
                    )
                    .await;
                let created = match created {
                    Ok(v) => v,
                    // A named branch nobody pushed: the same as no branch.
                    Err(e) if named && ctx.node.str("ifNoBranch") == "skip" => {
                        tracing::debug!(branch, "draft_pr: {e:#}");
                        return Ok(json!({ "skipped": "no branch", "branch": branch }));
                    }
                    Err(e) => return Err(e),
                };
                let number = created
                    .get("number")
                    .and_then(Value::as_i64)
                    .context("the forge did not say which pull request it opened")?;
                opened = true;
                // Before the read that emits `github.pull.opened`.
                sync::note_authored(c, repo, sync::Authored::Pull, number, &who).await;
                sync::refresh_pull(c, repo, number, &who).await?
            }
        };
        if let Some(card) = &t.card
            && card.pr_url != pull.html_url
            && !pull.html_url.is_empty()
        {
            let p = CardPatch {
                pr_url: Some(pull.html_url.clone()),
                ..Default::default()
            };
            if let Err(e) = core.update_card(&card.id, card.version, p, "engine").await {
                tracing::warn!(card = %card.key, "pr link on card: {e:#}");
            }
        }
        let mut out = pull_json(&pull);
        out["opened"] = json!(opened);
        out["base"] = json!(pull.base_ref);
        if opened {
            out["reviewers"] = self.request_reviewers(c, ctx, repo, &pull).await;
        }
        Ok(out)
    }

    /// The node's `reviewers`: `codeowners` (the forge requests code owners
    /// itself), logins, or `org/team`. A forge that cannot request them
    /// reports it; the pull request stays open either way.
    async fn request_reviewers(
        &self,
        c: &Client,
        ctx: &StepContext,
        repo: &str,
        p: &GhPull,
    ) -> Value {
        let who: Vec<String> = strings(ctx.node.config.get("reviewers"))
            .into_iter()
            .filter(|w| !w.eq_ignore_ascii_case("codeowners") && !w.eq_ignore_ascii_case(&p.author))
            .take(15)
            .collect();
        let codeowners = strings(ctx.node.config.get("reviewers"))
            .iter()
            .any(|w| w.eq_ignore_ascii_case("codeowners"));
        if who.is_empty() {
            return json!({ "requested": [], "codeowners": codeowners });
        }
        match sync::pull_action(
            c,
            repo,
            p.number,
            &PullAction::RequestReviewers(who.clone()),
            &caller(ctx),
        )
        .await
        {
            Ok(_) => json!({ "requested": who, "codeowners": codeowners }),
            Err(e) => {
                json!({ "requested": [], "codeowners": codeowners, "unsupported": format!("{e:#}") })
            }
        }
    }

    /// Approves a pull request that passes the readiness rules (web
    /// factory_engine_approve.go): open, same repository, the head the run
    /// was for, a factory review of this head with the approve verdict, no
    /// change requests, every check green, mergeable, and no sensitive path
    /// (built in plus `approve.sensitive_paths`). A draft is marked ready
    /// first. The factory never merges (factory.md 13).
    async fn approve_ready(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        let core = c.core();
        let repo = need_repo(t)?;
        let n = need_number(t)?;
        let who = caller(ctx);
        let target = format!("{repo}#{n}");
        let mut p = sync::refresh_pull(c, repo, n, &who).await?;
        let mut ready = false;
        if p.draft && p.state == "open" && !p.is_fork() {
            p = sync::pull_action(c, repo, n, &PullAction::Ready, &who).await?;
            ready = true;
        }
        let own = own_login(c, &who).await;
        let reviews: Vec<outputs::ReviewRow> = c
            .api()
            .reviews(c, repo, n, &who)
            .await
            .unwrap_or_default()
            .iter()
            .map(outputs::ReviewRow::from_json)
            .collect();
        let steps = run_steps(core, ctx).await;
        let run_verdict = steps
            .iter()
            .filter(|s| s.status == "succeeded")
            .filter_map(|s| serde_json::from_str::<Value>(&s.details_json).ok())
            .filter(|d| d.get("reviewId").is_some())
            .map(|d| {
                (
                    as_text(d.get("sha")).to_string(),
                    as_text(d.get("verdict")).to_string(),
                )
            })
            .rfind(|(s, v)| !s.is_empty() && !v.is_empty());
        let judged = outputs::judge(&outputs::ReadyIn {
            state: p.state.clone(),
            fork: p.is_fork(),
            draft: p.draft,
            author: p.author.clone(),
            head: p.head_sha.clone(),
            run_sha: run_sha(ctx),
            reviews,
            own,
            run_verdict,
            checks_total: p.checks.total,
            checks_pending: p.checks.pending,
            checks_failed: p.checks.failure,
            mergeable: p.mergeable.clone(),
        });
        if judged.done {
            return Ok(
                json!({ "pull": pull_json(&p), "ready": ready, "approved": true,
                              "already": true, "merged": false }),
            );
        }
        if !judged.ready {
            let why = format!("not approved: {}", judged.reason);
            guard::refuse(
                core,
                &who,
                "approve_ready",
                &target,
                &c.conn().id,
                &judged.reason,
                &why,
            )
            .await;
            let mut out = outputs::not_ready_json(&judged);
            out["pull"] = pull_json(&p);
            out["ready"] = json!(ready);
            return Ok(out);
        }
        let files = c.api().pull_files(c, repo, n, &who).await?;
        let extra = kls_core::audit::config::Effective::load(core)
            .await
            .map(|e| e.sensitive_paths)
            .unwrap_or_default();
        let hit = if files.len() >= 300 {
            Some(("too_many_files".to_string(), String::new()))
        } else {
            files
                .iter()
                .find_map(|f| outputs::sensitive(f, &extra).map(|g| (g, f.clone())))
        };
        if let Some((pattern, path)) = hit {
            let why = format!("not approved: {path} matches {pattern} (approve.sensitive_paths)");
            guard::refuse(
                core,
                &who,
                "approve_ready",
                &target,
                &c.conn().id,
                "sensitive_path",
                &why,
            )
            .await;
            return Ok(
                json!({ "pull": pull_json(&p), "ready": ready, "approved": false,
                              "reason": format!("sensitive_path:{pattern}"), "path": path,
                              "terminal": true, "merged": false }),
            );
        }
        kls_github::forge::api::require(c, "reviews")?;
        let head = p.head_sha.to_lowercase();
        let body = format!(
            "Approved by the Software Factory: checks green, reviewed, no sensitive paths.\n\nA person decides whether to merge.\n\n<!-- keleustes:approve run={} node={} sha={head} -->",
            short_id(&ctx.run.id),
            ctx.node.id
        );
        let body = signed(&body, &attribution_of(t, None));
        let v = c.api().approve(c, repo, n, &body, &head, &who).await?;
        let p = sync::refresh_pull(c, repo, n, &who).await.unwrap_or(p);
        Ok(
            json!({ "pull": pull_json(&p), "ready": ready, "approved": true, "reviewId": v.get("id"),
                   "sha": head, "merged": false }),
        )
    }

    /// `pr_review`: the reviewer's verdict note on the run's card
    /// (`verdict:` and `finding: FILE:LINE: text`), posted once per run,
    /// node and head as COMMENT or REQUEST_CHANGES (never APPROVE), with
    /// findings on the diff lines they concern. Without a verdict note the
    /// node's message is posted as before.
    async fn pr_review(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        let repo = need_repo(t)?;
        let n = need_number(t)?;
        let who = caller(ctx);
        let api = c.api();
        let notes: Vec<String> = t
            .card
            .as_ref()
            .map(|card| {
                kls_core::board::cards::history(card)
                    .into_iter()
                    .map(|h| h.note)
                    .filter(|s| !s.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let Some(verdict) = outputs::parse_verdict(&notes) else {
            // Never APPROVE from a workflow: a comment, or a request for changes.
            let event = match ctx.node.str("event").to_ascii_uppercase().as_str() {
                "REQUEST_CHANGES" => "REQUEST_CHANGES",
                _ => "COMMENT",
            };
            let body = format!("{}\n\n{}", message(ctx, t), review_marker(ctx));
            let body = signed(&body, &attribution_of(t, None));
            let v = api.review(c, repo, n, event, &body, &who).await?;
            return Ok(
                json!({ "repo": repo, "number": n, "reviewId": v.get("id"), "event": event }),
            );
        };
        let p = sync::refresh_pull(c, repo, n, &who).await?;
        let sha = p.head_sha.to_lowercase();
        if sha.is_empty() {
            bail!("{repo}#{n} has no head commit");
        }
        let run = short_id(&ctx.run.id);
        let key = format!("run={run} node={} sha={sha} ", ctx.node.id);
        let posted = api
            .reviews(c, repo, n, &who)
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| as_text(r.get("body")).contains(&key));
        if posted {
            return Ok(
                json!({ "skipped": "already_reviewed", "repo": repo, "number": n, "sha": sha,
                              "verdict": verdict.verdict }),
            );
        }
        let mut diff = std::collections::BTreeMap::new();
        if verdict.findings.iter().any(|f| f.line > 0) {
            for (path, patch) in api.pull_patches(c, repo, n, &who).await.unwrap_or_default() {
                if let Some(patch) = patch {
                    diff.insert(path, outputs::patch_lines(&patch));
                }
            }
        }
        let marker = outputs::review_marker(&run, &ctx.node.id, &sha, &verdict.verdict);
        let mut out = outputs::build_review(&verdict, &diff, &marker);
        out.body = signed(&out.body, &attribution_of(t, None));
        let v = api
            .review_inline(c, repo, n, &out.event, &out.body, &out.comments, &sha, &who)
            .await?;
        Ok(
            json!({ "repo": repo, "number": n, "reviewId": v.get("id"), "event": out.event,
                   "verdict": verdict.verdict, "sha": sha, "inline": out.comments.len(),
                   "findings": verdict.findings.len() }),
        )
    }

    /// `check_run`: a commit status on the pull request's head. The
    /// conclusion comes from the run's scans (exit 1 or a finding at or above
    /// `minSeverity` fails, exit 3 is neutral), else from `conclusion`
    /// (default success). `when: on_findings` posts only when it fails.
    async fn check_run(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        // A commit status on every forge: GitHub lets only Apps make check
        // runs, so a user token would get 403.
        let repo = need_repo(t)?;
        let n = need_number(t)?;
        let who = caller(ctx);
        let api = c.api();
        let steps = run_steps(c.core(), ctx).await;
        let found = outputs::findings(&steps, ctx.step.seq);
        let min = match ctx.node.str("minSeverity") {
            "" => "high",
            s => s,
        };
        let (conclusion, headline, hits) = match &found {
            Some(f) => {
                let (c, h, hits) = outputs::check_conclusion(f, min);
                (c.to_string(), h, hits)
            }
            None => {
                let c = match ctx.node.str("conclusion") {
                    "" => "success",
                    s => s,
                };
                (c.to_string(), message(ctx, t), 0)
            }
        };
        if ctx.node.str("when") == "on_findings" && conclusion != "failure" {
            return Ok(json!({ "skipped": "no_findings", "repo": repo, "number": n,
                              "conclusion": conclusion, "findings": hits }));
        }
        let p = sync::refresh_pull(c, repo, n, &who).await?;
        if p.head_sha.is_empty() {
            bail!("{repo}#{n} has no head commit");
        }
        let name = match ctx.node.str("name") {
            "" => match &found {
                Some(f) if !f.job.is_empty() => format!("Vulnetix {}", f.job),
                _ => ctx.node.title.clone(),
            },
            s => s.to_string(),
        };
        let v = api
            .set_status(
                c,
                repo,
                &p.head_sha,
                &StatusIn {
                    state: status_state(&conclusion).into(),
                    context: name.clone(),
                    description: headline.clone(),
                    target_url: String::new(),
                },
                &who,
            )
            .await?;
        c.invalidate(&api.pull_path(c, repo, n)).await?;
        Ok(
            json!({ "repo": repo, "number": n, "statusId": v.get("id"), "conclusion": conclusion,
                   "state": status_state(&conclusion), "name": name, "findings": hits,
                   "exitCode": found.as_ref().map(|f| f.exit_code), "headline": headline }),
        )
    }

    /// `file_issues`: the run's scan findings as issues (web
    /// factory_engine_node_issues.go). Severity floor `minSeverity` (default
    /// high), `group` run (one issue) or id (one per finding id), cap
    /// `maxIssues`, `kinds` / `skipKinds`, `from: profile` for the package
    /// firewall advisory. Each issue carries a marker, so a rescan updates
    /// the same issue (reopening it when its findings changed) instead of
    /// opening another; only issues the factory's own account opened are
    /// rewritten.
    async fn file_issues(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        let core = c.core();
        let repo = need_repo(t)?;
        let who = caller(ctx);
        let api = c.api();
        let cfg = &ctx.node.config;
        let label = match ctx.node.str("label") {
            "" => "sf:profile",
            l => l,
        };
        let Some((_, desc)) = kls_github::labels::DEFAULT_LABELS
            .iter()
            .find(|(n, _)| *n == label)
        else {
            bail!("the label must be one of the sf: labels the factory keeps (got {label:?})");
        };
        let wf = ctx.run.workflow_id.as_str();
        let run = short_id(&ctx.run.id);
        let steps = run_steps(core, ctx).await;
        let found = outputs::findings(&steps, ctx.step.seq);
        let attr = attribution_of(t, found.as_ref());
        let mut issues: Vec<(String, String, String)> = Vec::new(); // marker, title, body
        let mut doc = json!({ "repo": repo, "label": label });
        if ctx.node.str("from") == "profile" {
            let pms = crate::exec::repo_profile(core, repo)
                .await
                .map(|p| p.package_managers)
                .unwrap_or_default();
            let cmds = outputs::firewall_commands(&pms);
            if cmds.is_empty() {
                return Ok(json!({ "skipped": "no_package_manager", "repo": repo }));
            }
            let title = match ctx.node.str("title") {
                "" => "Vulnetix package firewall".to_string(),
                s => fill(s, ctx, t),
            };
            issues.push((
                outputs::issue_marker(wf, "all"),
                title,
                outputs::firewall_body(wf, &run, &cmds),
            ));
            doc["commands"] = json!(cmds.len());
        } else {
            let Some(f) = found.as_ref() else {
                bail!("no scan result before this step: file_issues files what a scan found");
            };
            let min = match ctx.node.str("minSeverity") {
                "" => "high",
                s => s,
            };
            let group = match ctx.node.str("group") {
                "id" => "id",
                _ => "run",
            };
            let (groups, total, omitted) = outputs::select(
                &f.items,
                min,
                group,
                &strings(cfg.get("kinds")),
                &strings(cfg.get("skipKinds")),
                outputs::max_issues(&Value::Object(cfg.clone())),
            );
            doc["findings"] = json!(total);
            doc["groups"] = json!(groups.len());
            doc["omitted"] = json!(omitted);
            if groups.is_empty() {
                doc["skipped"] = json!("no_findings");
                return Ok(doc);
            }
            let prefix = match ctx.node.str("title") {
                "" => format!("Vulnetix: {}", f.job),
                s => fill(s, ctx, t),
            };
            for g in &groups {
                let title = if g.key == "all" {
                    prefix.clone()
                } else {
                    format!("{prefix}: {}", g.key)
                };
                issues.push((
                    outputs::issue_marker(wf, &g.key),
                    title.chars().take(120).collect(),
                    outputs::issue_body(wf, &run, min, g, f),
                ));
            }
        }
        api.create_label(c, repo, label, kls_github::labels::COLOR, desc, &who)
            .await?;
        let own = own_login(c, &who).await;
        if own.is_empty() {
            bail!(
                "the forge did not say which account is signed in, so the factory's own issues cannot be told apart"
            );
        }
        let existing = api.issues_labelled(c, repo, label, &who).await?;
        let mut out = Vec::new();
        for (marker, title, body) in issues {
            let body = signed(&body, &attr);
            let hit = existing.iter().find(|i| {
                as_text(i["user"].get("login")).eq_ignore_ascii_case(&own)
                    && as_text(i.get("body")).contains(&marker)
            });
            if let Some(i) = hit {
                let number = i["number"].as_i64().unwrap_or(0);
                let mut updated = false;
                if as_text(i.get("body")) != body {
                    // Changed findings reopen a closed issue; an unchanged one
                    // stays closed (a person's close is respected).
                    let edit = IssueEdit {
                        body: Some(body),
                        state: (as_text(i.get("state")) == "closed").then(|| "open".to_string()),
                        ..Default::default()
                    };
                    api.update_issue(c, repo, number, &edit, &who).await?;
                    updated = true;
                }
                out.push(
                    json!({ "number": number, "created": false, "updated": updated,
                                 "url": i.get("html_url"), "marker": marker }),
                );
                continue;
            }
            let v = api
                .create_issue(c, repo, &title, &body, &[label.to_string()], &who)
                .await?;
            let number = v.get("number").and_then(Value::as_i64).unwrap_or(0);
            if number > 0 {
                sync::note_labels_added(c, repo, number, &[label.to_string()], &who).await;
                sync::note_authored(c, repo, sync::Authored::Issue, number, &who).await;
            }
            out.push(json!({ "number": number, "created": true, "updated": false,
                             "url": v.get("html_url"), "marker": marker }));
        }
        doc["issues"] = json!(out);
        if let Some(first) = out.first() {
            doc["number"] = first["number"].clone();
            doc["url"] = first["url"].clone();
        }
        Ok(doc)
    }

    /// `repo_profile` on the forge (web factory_engine_node_profile.go): the
    /// twelve CLI labels exist, and one "Software Factory profile" issue
    /// says what the scout found, kept current by its marker.
    async fn repo_profile(&self, c: &Client, ctx: &StepContext, t: &Target) -> Result<Value> {
        let core = c.core();
        let repo = need_repo(t)?;
        let who = caller(ctx);
        let api = c.api();
        let Some(p) = crate::exec::repo_profile(core, repo).await else {
            return Ok(json!({ "skipped": "no_profile", "repo": repo }));
        };
        let mut created = 0;
        for name in PROFILE_LABELS {
            let desc = kls_github::labels::DEFAULT_LABELS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| *d)
                .unwrap_or("Software Factory");
            if api
                .create_label(c, repo, name, kls_github::labels::COLOR, desc, &who)
                .await?
                .is_some()
            {
                created += 1;
            }
        }
        let row = |label: &str, v: &[String]| {
            if v.is_empty() {
                String::new()
            } else {
                format!("{label}: `{}`\n", v.join("`, `"))
            }
        };
        let mut body = String::from(
            "This issue is kept current by the Software Factory repo scout. Edits are overwritten by the next scout.\n\n",
        );
        if p.scouted_at > 0
            && let Ok(ts) = jiff::Timestamp::from_millisecond(p.scouted_at)
        {
            body.push_str(&format!("Scouted: {} (UTC)\n", ts.strftime("%Y-%m-%d")));
        }
        body.push_str(&row("Signals", &p.signals));
        body.push_str(&row("Package managers", &p.package_managers));
        body.push_str(&row("Languages", &p.languages));
        body.push_str("\nAdd the `sf:scout` label to this issue to scout again.\n\n");
        body.push_str(PROFILE_MARKER);
        body.push('\n');
        let body = signed(&body, &attribution_of(t, None));
        let own = own_login(c, &who).await;
        let existing = if own.is_empty() {
            Vec::new()
        } else {
            api.issues_labelled(c, repo, "sf:profile", &who).await?
        };
        let hit = existing.iter().find(|i| {
            as_text(i["user"].get("login")).eq_ignore_ascii_case(&own)
                && as_text(i.get("body")).contains(PROFILE_MARKER)
        });
        let (number, issue_created) = match hit {
            Some(i) => {
                let number = i["number"].as_i64().unwrap_or(0);
                if as_text(i.get("body")) != body {
                    let edit = IssueEdit {
                        body: Some(body),
                        ..Default::default()
                    };
                    api.update_issue(c, repo, number, &edit, &who).await?;
                }
                (number, false)
            }
            None if own.is_empty() => bail!("the forge did not say which account is signed in"),
            None => {
                let v = api
                    .create_issue(
                        c,
                        repo,
                        "Software Factory profile",
                        &body,
                        &["sf:profile".to_string()],
                        &who,
                    )
                    .await?;
                let n = v.get("number").and_then(Value::as_i64).unwrap_or(0);
                sync::note_authored(c, repo, sync::Authored::Issue, n, &who).await;
                (n, true)
            }
        };
        Ok(
            json!({ "repo": repo, "number": number, "created": issue_created, "labelsCreated": created,
                   "signals": p.signals }),
        )
    }
}

/// The labels the CLI defaults hand work on with (web factoryProfileLabels).
pub const PROFILE_LABELS: [&str; 12] = [
    "sf:scout",
    "sf:profile",
    "sf:pqc",
    "sf:ai",
    "sf:secret",
    "sf:malware",
    "sf:no-fix",
    "sf:fix-deps",
    "sf:fix-code",
    "sf:fix-iac",
    "sf:fix-container",
    "sf:fix-workflow",
];

const PROFILE_MARKER: &str = "<!-- keleustes:profile -->";

/// The capability an output needs (forges.md 5).
fn capability(action: &str) -> &'static str {
    match action {
        "pr_review" => "reviews",
        "add_label" | "file_issues" | "repo_profile" => "issues",
        "check_run" => "checks",
        "approve_ready" => "draft",
        _ => "pull_requests",
    }
}

/// Marks a review the factory posted (one per run and node).
fn review_marker(ctx: &StepContext) -> String {
    format!(
        "<!-- keleustes:review run={} node={} -->",
        short_id(&ctx.run.id),
        ctx.node.id
    )
}

#[async_trait::async_trait]
impl Forge for GitHubForge {
    async fn action(&self, ctx: &StepContext, action: &str) -> Result<Value> {
        let t = target(&self.core(), ctx).await?;
        let c = self.client(&t.repo).await?;
        let who = caller(ctx);
        if action != "merge" {
            kls_github::forge::api::require(&c, capability(action))?;
        }
        let api = c.api();
        match action {
            "draft_pr" => self.draft_pr(&c, ctx, &t).await,
            "approve_ready" => self.approve_ready(&c, ctx, &t).await,
            "pr_review" => self.pr_review(&c, ctx, &t).await,
            "check_run" => self.check_run(&c, ctx, &t).await,
            "file_issues" => self.file_issues(&c, ctx, &t).await,
            "repo_profile" => self.repo_profile(&c, ctx, &t).await,
            "merge" => {
                bail!(
                    "the factory never merges; a person merges from Repos (use approve_ready to mark it ready)"
                )
            }
            "issue_comment" => {
                let repo = need_repo(&t)?;
                let n = need_number(&t)?;
                let body = signed(&message(ctx, &t), &attribution_of(&t, None));
                let v = api.comment(&c, repo, n, &body, &who).await?;
                let id = v.get("id").and_then(Value::as_i64).unwrap_or(0);
                sync::note_authored(&c, repo, sync::Authored::Comment, id, &who).await;
                Ok(
                    json!({ "repo": repo, "number": n, "commentId": v.get("id"), "url": v.get("html_url") }),
                )
            }
            "add_label" => {
                let repo = need_repo(&t)?;
                let n = need_number(&t)?;
                let label = ctx.node.str("label").trim();
                if label.is_empty() {
                    bail!("name the label to add");
                }
                api.add_labels(&c, repo, n, &[label.to_string()], &who)
                    .await?;
                sync::note_labels_added(&c, repo, n, &[label.to_string()], &who).await;
                Ok(json!({ "repo": repo, "number": n, "label": label }))
            }
            other => bail!("unknown GitHub action {other:?}"),
        }
    }

    async fn gate(&self, ctx: &StepContext) -> Result<GateResult> {
        let t = target(&self.core(), ctx).await?;
        let c = self.client(&t.repo).await?;
        let (Ok(repo), Some(n)) = (need_repo(&t), t.number) else {
            return Ok(GateResult::Fail(
                "the run names no pull request to check".into(),
            ));
        };
        let p = sync::refresh_pull(&c, repo, n, &caller(ctx)).await?;
        let facts = pull_json(&p);
        if p.merged {
            return Ok(GateResult::Pass(facts));
        }
        if p.state != "open" {
            return Ok(GateResult::Fail(format!("{repo}#{n} is closed")));
        }
        if p.mergeable == "conflict" {
            return Ok(GateResult::Fail(format!("{repo}#{n} has merge conflicts")));
        }
        match p.checks_state.as_str() {
            "failure" => {
                return Ok(GateResult::Fail(format!(
                    "{} of {} checks failed on {repo}#{n}",
                    p.checks.failure, p.checks.total
                )));
            }
            "pending" => return Ok(GateResult::Pending),
            _ => {}
        }
        if ctx.node.bool("requireReview") {
            match p.review_decision.as_str() {
                "APPROVED" => {}
                "CHANGES_REQUESTED" => {
                    return Ok(GateResult::Fail(format!(
                        "a reviewer asked for changes on {repo}#{n}"
                    )));
                }
                _ => return Ok(GateResult::Pending),
            }
        }
        Ok(GateResult::Pass(facts))
    }
}

// ---- triggers ----------------------------------------------------------------

/// The GitHub webhook names (`pull_request.opened`, `issues.closed`, ...)
/// a mirrored `github.*` event stands for. Empty for other events.
pub fn webhook_events(ev: &Event, data: &Value) -> Vec<String> {
    use kls_core::github::kinds as k;
    let b = |key: &str| data.get(key).and_then(Value::as_bool).unwrap_or(false);
    let mut out: Vec<&str> = match ev.kind.as_str() {
        k::PULL_OPENED => vec!["pull_request.opened"],
        k::PULL_CLOSED => vec!["pull_request.closed"],
        k::PULL_MERGED => vec!["pull_request.closed", "pull_request.merged"],
        k::PULL_UPDATED => {
            let mut v = vec!["pull_request.synchronize"];
            if b("readyForReview") {
                v.push("pull_request.ready_for_review");
            }
            if b("convertedToDraft") {
                v.push("pull_request.converted_to_draft");
            }
            if b("checksCompleted") {
                v.push("check_suite.completed");
                v.push("check_run.completed");
            }
            v
        }
        k::PULL_ACTION if data.get("action").and_then(Value::as_str) == Some("ready") => {
            vec!["pull_request.ready_for_review"]
        }
        k::ISSUE_COMMENT => vec!["issue_comment.created"],
        k::PUSH => vec!["push"],
        k::WORKFLOW_RUN => vec!["workflow_run.completed", "check_suite.completed"],
        k::RELEASE => vec!["release.published"],
        k::REPO_ADDED => vec!["installation_repositories.added"],
        k::ALERT => match data.get("alert").and_then(Value::as_str) {
            Some("dependabot") => vec!["dependabot_alert.created"],
            Some("code_scanning") => vec!["code_scanning_alert.created"],
            Some("secret_scanning") => vec!["secret_scanning_alert.created"],
            _ => vec![],
        },
        k::REVIEW_SUBMITTED => vec!["pull_request_review.submitted"],
        k::PACKAGE_PUBLISHED => vec!["registry_package.published"],
        k::RISK => match data.get("source").and_then(Value::as_str) {
            Some("github-posture") => vec!["github-posture"],
            _ => vec!["github-risk"],
        },
        k::ISSUE_OPENED => vec!["issues.opened"],
        k::ISSUE_CLOSED => vec!["issues.closed"],
        k::ISSUE_UPDATED => {
            if data.get("state").and_then(Value::as_str) == Some("open") && b("reopened") {
                vec!["issues.reopened"]
            } else {
                vec!["issues.edited"]
            }
        }
        _ => vec![],
    };
    if !labels_added(data).is_empty() {
        out.push(if ev.kind.starts_with("github.pull.") {
            "pull_request.labeled"
        } else {
            "issues.labeled"
        });
    }
    out.into_iter().map(str::to_string).collect()
}

/// Labels a `github.*` event says were added (`labelsAdded`; a newly
/// opened issue or pull request counts all its labels).
pub fn labels_added(data: &Value) -> Vec<String> {
    let list = |k: &str| -> Vec<String> {
        data.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    list("labelsAdded")
}

/// Whether a `webhook` trigger's `event` (`pull_request.opened`, or a base
/// such as `pull_request`) matches one of `names`.
pub fn webhook_matches(want: &str, names: &[String]) -> bool {
    let want = want.trim();
    !want.is_empty()
        && names.iter().any(|n| {
            n == want || n.split_once('.').map(|(base, _)| base) == Some(want) || want == "*"
        })
}

#[cfg(test)]
mod tests;
