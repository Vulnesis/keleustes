//! The defaults catalogue (vdb-site `factory_defaults.go`): workflows seeded
//! per managed repository, gated by `defaults.enabled`, the repository's
//! `defaults_off` opt-outs and its autonomy, and upgraded with the catalogue
//! unless a person customised them.
//!
//! The core defaults (`pr-review`, `ci-fix` with its default-branch
//! `workflow_run` trigger, `test-sweep` and `lint-sweep` with push
//! triggers, `issue-to-pr` with an outsider hold, `approve-ready`,
//! `conflict-fix` as a rule, `repo-scout`) and the 34 CLI defaults
//! ([`crate::defaults_cli`]). Each one can be turned off everywhere with
//! `defaults.<key>.enabled`; an event a gated default would have taken
//! emits `trigger.skipped` with the reason (at most hourly per workflow).
//! The `sys-` slug prefix is reserved for them.

use crate::graph::Graph;
use crate::guard::{self, Origin};
use crate::ops::{self, NewWorkflow};
use anyhow::Result;
use kls_core::Core;
use serde::Serialize;
use serde_json::{Value, json};

/// One default workflow.
#[derive(Clone, Debug)]
pub struct DefaultSpec {
    pub key: &'static str,
    pub name: &'static str,
    pub rev: i64,
    /// The repository autonomy it needs to run.
    pub min_autonomy: &'static str,
    pub description: &'static str,
}

pub const CATALOGUE: [DefaultSpec; 8] = [
    DefaultSpec {
        key: "pr-review",
        name: "Default: pull request review",
        rev: 1,
        min_autonomy: "suggest",
        description: "Reviews every pull request that opens, is updated or leaves draft with belai:pr-reviewer and posts the verdict as a review. A fork pull request waits for you first. It never approves.",
    },
    DefaultSpec {
        key: "ci-fix",
        name: "Default: failing check fixes",
        rev: 2,
        min_autonomy: "draft_pr",
        description: "When a pull request's checks fail, or a workflow run fails on the default branch, belai:bug reproduces and fixes the failure and the fix opens as a draft pull request on top of the failing branch.",
    },
    DefaultSpec {
        key: "test-sweep",
        name: "Default: test sweep",
        rev: 2,
        min_autonomy: "draft_pr",
        description: "Once a week, and after pushes to the default branch (at most twice an hour), belai:delivery runs the tests and belai:test-fixer fixes what fails; the fix opens as a draft pull request.",
    },
    DefaultSpec {
        key: "lint-sweep",
        name: "Default: lint sweep",
        rev: 2,
        min_autonomy: "draft_pr",
        description: "Once a week, and after pushes to the default branch (at most twice an hour), belai:lint-fixer fixes lint drift with the linter's autofix; the fix opens as a draft pull request.",
    },
    DefaultSpec {
        key: "issue-to-pr",
        name: "Default: issue to pull request",
        rev: 2,
        min_autonomy: "draft_pr",
        description: "A new issue goes to belai:planner (an issue from outside the team waits for you first); you sign the plan off, then belai:builder implements it, belai:reviewer checks it and a draft pull request opens.",
    },
    DefaultSpec {
        key: "approve-ready",
        name: "Default: approve when ready",
        rev: 1,
        min_autonomy: "pr",
        description: "Approves a same-repository pull request once checks are green and nobody asked for changes. It never merges: a person merges.",
    },
    DefaultSpec {
        key: "conflict-fix",
        name: "Default: merge conflict fixes",
        rev: 1,
        min_autonomy: "draft_pr",
        description: "Files a merge-conflict card for each open pull request GitHub reports as conflicting; belai:conflict-resolver claims it. A fork's card waits for you. Runs as a rule (kls_engine::conflicts), not a workflow.",
    },
    DefaultSpec {
        key: "repo-scout",
        name: "Default: repo scout",
        rev: 2,
        min_autonomy: "observe",
        description: "Works out what a repository is built with and stores it as the repository's profile, which the CLI defaults read to decide whether they apply. Runs when a repository is added, after a manifest change, weekly and on sf:scout. It changes no code.",
    },
];

/// Every default: the core ones, then the 34 Vulnetix CLI defaults.
pub fn entries() -> Vec<DefaultSpec> {
    let mut out: Vec<DefaultSpec> = CATALOGUE.to_vec();
    out.extend(crate::defaults_cli::SPECS.iter().map(|s| DefaultSpec {
        key: s.key,
        name: s.name,
        rev: 1,
        min_autonomy: s.min_autonomy,
        description: s.description,
    }));
    out
}

pub fn spec(key: &str) -> Option<DefaultSpec> {
    entries().into_iter().find(|d| d.key == key)
}

fn at(i: usize) -> Value {
    json!({ "x": 40.0 + 220.0 * i as f64, "y": 80.0 })
}

fn webhook(
    id: &str,
    title: &str,
    event: &str,
    conditions: Value,
    debounce: i64,
    per_hour: i64,
) -> Value {
    json!({"id": id, "type": "trigger", "title": title, "config": {
        "kind": "webhook", "event": event, "overlap": "skip_if_running", "conditions": conditions,
        "debounceSeconds": debounce, "maxRunsPerHour": per_hour }, "position": {"x": 40.0, "y": 80.0}})
}

fn agent(id: &str, title: &str, agent: &str, attempts: i64, brief: &str, i: usize) -> Value {
    json!({"id": id, "type": "crew", "title": title, "config": {
        "dispatch": "worker", "agent": agent, "await": "session", "maxAttempts": attempts, "brief": brief },
        "position": at(i)})
}

fn crew(id: &str, title: &str, crew: &str, brief: &str, i: usize) -> Value {
    json!({"id": id, "type": "crew", "title": title, "config": {
        "dispatch": "crew", "crew": crew, "mode": "goal", "await": "session", "maxAttempts": 1, "brief": brief },
        "position": at(i)})
}

fn output(id: &str, title: &str, action: &str, extra: Value, i: usize) -> Value {
    let mut cfg = json!({ "action": action });
    for (k, v) in extra.as_object().into_iter().flatten() {
        cfg[k] = v.clone();
    }
    json!({"id": id, "type": "github", "title": title, "config": cfg, "position": at(i)})
}

fn human(id: &str, title: &str, i: usize) -> Value {
    json!({"id": id, "type": "human", "title": title, "config": {
        "approvers": ["owner"], "slaHours": 72, "onTimeout": "end_run" }, "position": at(i)})
}

/// A cron trigger with no zone or catch-up of its own: it follows
/// `schedules.tz` and `schedules.missed`.
fn cron(id: &str, title: &str, expr: &str) -> Value {
    json!({"id": id, "type": "trigger", "title": title, "config": {
        "kind": "cron", "cron": expr, "overlap": "skip_if_running" },
        "position": {"x": 40.0, "y": 200.0}})
}

/// A push to the default branch: one fire per 15 minutes, at most two an hour.
fn push_default(id: &str, title: &str) -> Value {
    let mut v = webhook(
        id,
        title,
        "push",
        json!({"headBranches": ["$default"], "origin": "same_repo"}),
        900,
        2,
    );
    v["position"] = json!({"x": 40.0, "y": 320.0});
    v
}

fn e(from: &str, to: &str, label: &str) -> Value {
    json!({ "from": from, "to": to, "label": label })
}

const PR_ACTIONS: [&str; 4] = ["opened", "synchronize", "ready_for_review", "reopened"];

/// The graph of a default for `repo`.
pub fn graph(key: &str, repo: &str) -> Option<Graph> {
    let (nodes, edges, budget): (Vec<Value>, Vec<Value>, i64) = match key {
        "pr-review" => (
            vec![
                webhook(
                    "n1",
                    "Pull request from this repository",
                    "pull_request",
                    json!({"actions": PR_ACTIONS, "drafts": "exclude", "origin": "same_repo"}),
                    60,
                    60,
                ),
                webhook(
                    "n2",
                    "Pull request from a fork",
                    "pull_request",
                    json!({"actions": PR_ACTIONS, "drafts": "exclude", "origin": "fork"}),
                    60,
                    60,
                ),
                human("n3", "Approve reviewing fork code", 1),
                agent(
                    "n4",
                    "Review the pull request",
                    "belai:pr-reviewer",
                    2,
                    "Review the pull request named in the Trigger event line of this card. Write a verdict note.",
                    2,
                ),
                output("n5", "Post the review", "pr_review", json!({}), 3),
            ],
            vec![
                e("n1", "n4", "pull request"),
                e("n2", "n3", "fork pull request"),
                e("n3", "n4", "approved"),
                e("n4", "n5", "verdict note"),
            ],
            150_000,
        ),
        "ci-fix" => (
            vec![
                webhook(
                    "n1",
                    "Checks finished on a pull request",
                    "check_suite.completed",
                    json!({"origin": "same_repo"}),
                    60,
                    12,
                ),
                json!({"id": "n2", "type": "condition", "title": "Did they fail?", "config": {
                    "rules": [
                        {"path": "event.data.checks", "op": "eq", "value": "failure", "outcome": "failed"},
                        {"path": "event.data.failed", "op": "eq", "value": true, "outcome": "failed"},
                        {"path": "event.data.conclusion", "op": "eq", "value": "failure", "outcome": "failed"}
                    ],
                    "default": "green", "outcomes": [{"value": "failed"}, {"value": "green", "end": true}] },
                    "position": at(1)}),
                crew(
                    "n3",
                    "Reproduce, fix and review",
                    "belai:bug",
                    "The pull request's checks failed. Reproduce the failure and fix it with the smallest change.",
                    2,
                ),
                output(
                    "n4",
                    "Draft pull request",
                    "draft_pr",
                    json!({"prState": "draft", "base": "pr_head", "ifNoBranch": "skip"}),
                    3,
                ),
                {
                    let mut v = webhook(
                        "n5",
                        "Workflow run failed on the default branch",
                        "workflow_run.completed",
                        json!({"headBranches": ["$default"], "origin": "same_repo"}),
                        300,
                        6,
                    );
                    v["position"] = json!({"x": 40.0, "y": 320.0});
                    v
                },
            ],
            vec![
                e("n5", "n2", "workflow run"),
                e("n1", "n2", "checks"),
                json!({"from": "n2", "to": "n3", "outcome": "failed"}),
                e("n3", "n4", "fix branch"),
            ],
            400_000,
        ),
        "test-sweep" => (
            vec![
                cron("n1", "Weekly test sweep", "0 5 * * 1"),
                crew(
                    "n2",
                    "Sweep the tests",
                    "belai:delivery",
                    "Run the repository's test suites and list the failing ones.",
                    1,
                ),
                agent(
                    "n3",
                    "Fix the failing tests",
                    "belai:test-fixer",
                    2,
                    "Fix the failing tests with the smallest change; never weaken a test.",
                    2,
                ),
                output(
                    "n4",
                    "Draft pull request",
                    "draft_pr",
                    json!({"prState": "draft", "ifNoBranch": "skip"}),
                    3,
                ),
                push_default("n5", "Push to the default branch"),
            ],
            vec![
                e("n1", "n2", "repo"),
                e("n5", "n2", "push"),
                e("n2", "n3", "failing suites"),
                e("n3", "n4", "fix branch"),
            ],
            300_000,
        ),
        "lint-sweep" => (
            vec![
                cron("n1", "Weekly lint sweep", "0 6 * * 2"),
                agent(
                    "n2",
                    "Fix lint drift",
                    "belai:lint-fixer",
                    2,
                    "Run the linter from the rule files the repository checked in and fix drift with its autofix. Never edit a lint rule. If the linter is already clean, say so and stop.",
                    1,
                ),
                output(
                    "n3",
                    "Draft pull request",
                    "draft_pr",
                    json!({"prState": "draft", "ifNoBranch": "skip"}),
                    2,
                ),
                push_default("n4", "Push to the default branch"),
            ],
            vec![
                e("n1", "n2", "repo"),
                e("n4", "n2", "push"),
                e("n2", "n3", "fix branch"),
            ],
            200_000,
        ),
        "issue-to-pr" => (
            vec![
                webhook(
                    "n1",
                    "Issue opened by the team",
                    "issues.opened",
                    json!({"senders": "team"}),
                    0,
                    30,
                ),
                {
                    let mut v = webhook(
                        "n7",
                        "Issue opened by someone outside the team",
                        "issues.opened",
                        json!({"senders": "outsider"}),
                        0,
                        30,
                    );
                    v["position"] = json!({"x": 40.0, "y": 320.0});
                    v
                },
                human("n8", "Approve an outsider's issue", 1),
                agent(
                    "n2",
                    "Plan the work",
                    "belai:planner",
                    2,
                    "Plan the work for the issue named in the Trigger event line of this card and write plan.md. Do not change any code in this step.",
                    1,
                ),
                human("n3", "Plan approval", 2),
                agent(
                    "n4",
                    "Implement",
                    "belai:builder",
                    3,
                    "Implement the approved plan. Keep the change small, add or update tests, and run the test suite before you finish.",
                    3,
                ),
                agent(
                    "n5",
                    "Review against the issue",
                    "belai:reviewer",
                    2,
                    "Review the builder's branch against the issue and the plan.",
                    4,
                ),
                output(
                    "n6",
                    "Draft pull request",
                    "draft_pr",
                    json!({"prState": "draft"}),
                    5,
                ),
            ],
            vec![
                e("n1", "n2", "issue"),
                e("n7", "n8", "outsider's issue"),
                e("n8", "n2", "approved"),
                e("n2", "n3", "plan.md"),
                e("n3", "n4", "approved"),
                e("n4", "n5", "branch"),
                e("n5", "n6", "review"),
            ],
            600_000,
        ),
        "approve-ready" => (
            vec![
                webhook(
                    "n1",
                    "Pull request opened or updated",
                    "pull_request",
                    json!({"actions": PR_ACTIONS, "drafts": "exclude", "origin": "same_repo"}),
                    0,
                    120,
                ),
                webhook(
                    "n2",
                    "Checks finished",
                    "check_suite.completed",
                    json!({"origin": "same_repo"}),
                    0,
                    120,
                ),
                output("n3", "Approve when ready", "approve_ready", json!({}), 1),
            ],
            vec![e("n1", "n3", "pull request"), e("n2", "n3", "checks")],
            0,
        ),
        "repo-scout" => (
            vec![
                webhook(
                    "n1",
                    "Repository added to the factory",
                    "installation_repositories.added",
                    json!({"origin": "same_repo"}),
                    0,
                    30,
                ),
                webhook(
                    "n2",
                    "Push that changes a manifest",
                    "push",
                    json!({"headBranches": ["$default"], "paths": crate::defaults_cli::PROFILE_PATHS, "origin": "same_repo"}),
                    900,
                    2,
                ),
                json!({"id": "n3", "type": "trigger", "title": "Label sf:scout", "config": {
                    "kind": "label", "label": "sf:scout", "maxRunsPerHour": 10, "overlap": "queue" }, "position": {"x": 40.0, "y": 320.0}}),
                cron("n4", "Weekly scout", "0 4 * * 0"),
                json!({"id": "n5", "type": "scan", "title": "Scout the repository", "config": {"source": "cli", "job": "scout"}, "position": at(1)}),
                json!({"id": "n6", "type": "profile", "title": "Store the profile", "config": {"output": "repo_profile"}, "position": at(2)}),
            ],
            vec![
                e("n1", "n5", "repository added"),
                e("n2", "n5", "manifest changed"),
                e("n3", "n5", "scout requested"),
                e("n4", "n5", "repos in scope"),
                e("n5", "n6", "signals"),
            ],
            0,
        ),
        k => return crate::defaults_cli::spec(k).map(|s| crate::defaults_cli::build(s, repo)),
    };
    let mut g: Graph = serde_json::from_value(json!({
        "nodes": nodes, "edges": edges, "scope": { "repos": [repo] },
    }))
    .ok()?;
    if budget > 0 {
        g.limits.run_budget_tokens = Some(budget);
    }
    Some(g)
}

/// The catalogue key of a seeded workflow for `repo`.
pub fn system_key(key: &str, repo: &str) -> String {
    format!("sys:{key}:{}", repo.to_lowercase())
}

/// What a seeding pass did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedReport {
    pub created: usize,
    pub upgraded: usize,
    pub enabled: usize,
    pub disabled: usize,
    pub skipped_customised: usize,
}

/// The settings key that turns one default off for every repository
/// (`defaults.<key>.enabled`, default on).
pub fn enabled_key(key: &str) -> String {
    format!("defaults.{key}.enabled")
}

/// The Settings section id of the per-default switches.
pub const SETTINGS_SECTION: &str = "defaults";

/// Registers one `defaults.<key>.enabled` switch (Bool, default on) per
/// default with the settings document ([`kls_core::audit::config::register_extra`]),
/// so Settings shows and saves them (audited like every setting). Called by
/// [`crate::start_with`]; idempotent.
pub fn register_settings() -> bool {
    use kls_core::audit::config::{Field, Kind, register_extra};
    let fields: Vec<Field> = entries()
        .into_iter()
        .map(|d| {
            let key: &'static str = Box::leak(enabled_key(d.key).into_boxed_str());
            let label = d.name.strip_prefix("Default: ").unwrap_or(d.name);
            Field::new(key, label, Kind::Bool, "true").help(d.description)
        })
        .collect();
    register_extra(
        SETTINGS_SECTION,
        "Defaults",
        "Built-in workflows, on or off for every repository",
        fields,
    )
}

/// Whether `defaults.<key>.enabled` leaves the default on (unset is on;
/// `false`, `0`, `off` turn it off).
pub async fn key_enabled(core: &Core, key: &str) -> bool {
    match core.store().setting(&enabled_key(key)).await {
        Ok(Some(v)) => !matches!(
            v.trim().trim_matches('"').to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        ),
        Ok(None) => true,
        // Fail closed: an unreadable setting keeps it off.
        Err(_) => false,
    }
}

/// Why a default does not run on `repo`, or `None` when it may.
pub fn gate(
    d: &DefaultSpec,
    defaults_on: bool,
    key_on: bool,
    defaults_off: &[String],
    autonomy: &str,
    archived: bool,
) -> Option<&'static str> {
    if !defaults_on {
        return Some("defaults_off");
    }
    if !key_on {
        return Some("default_disabled");
    }
    if archived {
        return Some("archived");
    }
    if defaults_off.iter().any(|k| k == d.key) {
        return Some("repo_opted_out");
    }
    if guard::autonomy_rank(autonomy) < guard::autonomy_rank(d.min_autonomy) {
        return Some("autonomy_too_low");
    }
    None
}

/// Seeds and upgrades the defaults on every managed repository, and turns
/// each on or off by its gate. Idempotent. A provision action: refused
/// under the kill switch.
pub async fn seed(core: &Core) -> Result<SeedReport> {
    if guard::halted(core).await.is_some() {
        guard::refuse_switch(core, "provision", json!({})).await?;
        anyhow::bail!("the factory is stopped");
    }
    let s = core.store();
    let defaults_on = kls_core::audit::config::Effective::load(core)
        .await
        .map(|e| e.defaults_enabled)
        .unwrap_or(false);
    let mut out = SeedReport::default();
    for repo in s.gh_managed_repos().await? {
        let autonomy = guard::effective_autonomy(core, &repo.full_name).await;
        for d in &entries() {
            let Some(g) = graph(d.key, &repo.full_name) else {
                continue;
            };
            let key = system_key(d.key, &repo.full_name);
            let origin =
                Origin::engine(json!({ "catalogue": d.key, "rev": d.rev, "repo": repo.full_name }));
            let w = match s.workflow_by_system_key(&key).await? {
                Some(w) => {
                    if w.system_rev < d.rev {
                        if w.customised {
                            out.skipped_customised += 1;
                        } else {
                            let doc = ops::workflow_doc(core, &w.id)
                                .await?
                                .ok_or_else(|| anyhow::anyhow!("workflow vanished"))?;
                            let (version, revision, _) = doc.editable();
                            ops::save_draft(core, &w.id, revision, &g).await?;
                            ops::publish(core, &w.id, version, &format!("catalogue rev {}", d.rev))
                                .await?;
                            s.set_workflow_system(&w.id, "system", &key, d.rev, false)
                                .await?;
                            guard::audit(
                                core,
                                "workflow",
                                "defaults.upgrade",
                                json!({ "workflowId": w.id, "key": key }),
                                &origin,
                                "upgraded",
                                json!({}),
                            )
                            .await?;
                            out.upgraded += 1;
                        }
                    }
                    s.workflow(&w.id).await?.unwrap_or(w)
                }
                None => {
                    let w = ops::create_workflow(
                        core,
                        NewWorkflow {
                            name: format!("{} · {}", d.name, repo.full_name),
                            slug: Some(format!(
                                "sys-{}-{}",
                                d.key,
                                repo.full_name.replace('/', "-").to_lowercase()
                            )),
                            description: d.description.into(),
                            template_id: format!("default:{}", d.key),
                            graph: Some(g),
                        },
                    )
                    .await?;
                    ops::publish(core, &w.id, 1, &format!("catalogue rev {}", d.rev)).await?;
                    s.set_workflow_system(&w.id, "system", &key, d.rev, false)
                        .await?;
                    guard::audit(
                        core,
                        "workflow",
                        "defaults.seed",
                        json!({ "workflowId": w.id, "key": key }),
                        &origin,
                        "seeded",
                        json!({}),
                    )
                    .await?;
                    out.created += 1;
                    s.workflow(&w.id).await?.unwrap_or(w)
                }
            };
            let on = gate(
                d,
                defaults_on,
                key_enabled(core, d.key).await,
                &repo.settings.defaults_off,
                &autonomy,
                repo.archived,
            )
            .is_none();
            if w.enabled != on {
                ops::set_workflow_enabled(core, &w.id, on).await?;
            }
            if on {
                out.enabled += 1;
            } else {
                out.disabled += 1;
            }
        }
    }
    Ok(out)
}

/// Why the default `key` is gated off on `repo` now (the same gate
/// [`seed`] applies), or `None` when it may run.
pub async fn gate_reason(core: &Core, key: &str, repo: &str) -> Option<&'static str> {
    let d = spec(key)?;
    let defaults_on = kls_core::audit::config::Effective::load(core)
        .await
        .map(|e| e.defaults_enabled)
        .unwrap_or(false);
    let gh = core.store().gh_repo(repo).await.ok().flatten();
    let off = gh
        .as_ref()
        .map(|r| r.settings.defaults_off.clone())
        .unwrap_or_default();
    let archived = gh.as_ref().is_some_and(|r| r.archived);
    let autonomy = guard::effective_autonomy(core, repo).await;
    gate(
        &d,
        defaults_on,
        key_enabled(core, key).await,
        &off,
        &autonomy,
        archived,
    )
}

/// The catalogue key and repository of a seeded workflow's `system_key`.
pub fn parse_system_key(system_key: &str) -> Option<(&str, &str)> {
    let rest = system_key.strip_prefix("sys:")?;
    rest.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{Mode, validate};

    #[test]
    fn every_default_validates() {
        for d in entries().iter().filter(|d| d.key != "conflict-fix") {
            let g = graph(d.key, "acme/api").unwrap();
            let issues = validate(&g, Mode::Publish);
            assert!(issues.is_empty(), "{}: {issues:?}", d.key);
            assert_eq!(g.scope.repos, ["acme/api"]);
            for n in &g.nodes {
                for k in ["agent", "crew"] {
                    let v = n.str(k);
                    if !v.is_empty() {
                        assert!(
                            crate::graph::workers().contains(&v)
                                || crate::graph::crews().contains(&v),
                            "{v}"
                        );
                    }
                }
            }
        }
        assert!(graph("nope", "a/b").is_none());
        assert_eq!(
            system_key("pr-review", "Acme/API"),
            "sys:pr-review:acme/api"
        );
    }

    #[test]
    fn gate_reasons() {
        let d = &spec("ci-fix").unwrap();
        assert_eq!(
            gate(d, false, true, &[], "draft_pr", false),
            Some("defaults_off")
        );
        assert_eq!(
            gate(d, true, false, &[], "draft_pr", false),
            Some("default_disabled")
        );
        assert_eq!(gate(d, true, true, &[], "draft_pr", true), Some("archived"));
        assert_eq!(
            gate(d, true, true, &["ci-fix".into()], "draft_pr", false),
            Some("repo_opted_out")
        );
        assert_eq!(
            gate(d, true, true, &[], "suggest", false),
            Some("autonomy_too_low")
        );
        assert_eq!(gate(d, true, true, &[], "draft_pr", false), None);
        assert_eq!(
            parse_system_key("sys:ci-fix:acme/api"),
            Some(("ci-fix", "acme/api"))
        );
    }

    #[tokio::test]
    async fn per_key_switch_reads_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        assert!(key_enabled(&core, "ci-fix").await);
        core.store()
            .set_setting(&enabled_key("ci-fix"), "false")
            .await
            .unwrap();
        assert!(!key_enabled(&core, "ci-fix").await);
        assert!(key_enabled(&core, "lint-sweep").await);
    }

    #[tokio::test]
    async fn per_default_switches_are_settings_saved_and_audited() {
        use kls_core::audit::config;
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        register_settings();
        // Idempotent.
        assert!(!register_settings());
        for d in entries() {
            let f = config::field(&enabled_key(d.key)).expect("registered");
            assert_eq!(f.kind, config::Kind::Bool);
            assert_eq!(config::section_of(f.key).unwrap().id, SETTINGS_SECTION);
        }
        // The built-in Defaults section keeps its own fields first.
        let s = config::section_of("defaults.enabled").unwrap();
        assert_eq!(s.fields[0].key, "defaults.enabled");
        let doc = core.settings_doc().await.unwrap();
        assert_eq!(doc.values["defaults.ci-fix.enabled"], json!(true));
        let mut ch = std::collections::BTreeMap::new();
        ch.insert("defaults.ci-fix.enabled".to_string(), json!(false));
        core.settings_save(doc.revision, ch).await.unwrap();
        assert!(!key_enabled(&core, "ci-fix").await);
        assert!(key_enabled(&core, "lint-sweep").await);
        let led = core
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["setting".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            led.iter()
                .any(|e| e.subject["setting"] == "defaults.ci-fix.enabled")
        );
        assert!(
            core.settings_yaml()
                .await
                .unwrap()
                .contains("defaults.ci-fix.enabled")
        );
    }

    #[test]
    fn defaults_carry_push_workflow_run_and_outsider_triggers() {
        let kinds = |key: &str| -> Vec<String> {
            graph(key, "acme/api")
                .unwrap()
                .triggers()
                .map(|t| format!("{}:{}", t.trigger_kind(), t.str("event")))
                .collect()
        };
        for k in ["test-sweep", "lint-sweep"] {
            assert!(kinds(k).contains(&"webhook:push".to_string()), "{k}");
        }
        assert!(kinds("ci-fix").contains(&"webhook:workflow_run.completed".to_string()));
        let g = graph("issue-to-pr", "acme/api").unwrap();
        let senders: Vec<&str> = g
            .triggers()
            .filter_map(|t| t.config.get("conditions")?.get("senders")?.as_str())
            .collect();
        assert_eq!(senders, ["team", "outsider"]);
        // The outsider's trigger reaches the planner only through a person.
        assert!(g.outgoing("n7").all(|e| e.to == "n8"));
        assert_eq!(g.node("n8").unwrap().kind, "human");
        // Cron defaults follow `schedules.tz` / `schedules.missed`.
        let t = graph("test-sweep", "acme/api").unwrap();
        let c = t.triggers().find(|t| t.trigger_kind() == "cron").unwrap();
        assert!(c.str("tz").is_empty() && c.str("catchUp").is_empty());
    }

    #[tokio::test]
    async fn seed_refused_under_the_kill_switch() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        assert_eq!(
            seed(&core).await.unwrap(),
            SeedReport::default(),
            "no managed repos"
        );
        core.set_factory_stopped(true, "").await.unwrap();
        assert!(seed(&core).await.is_err());
        core.audit_sync().await.unwrap();
        assert!(!crate::guard::audited(&core, "kill_switch").await.is_empty());
    }
}
