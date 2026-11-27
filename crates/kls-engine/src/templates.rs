//! Built-in templates, shipped as data (`templates.json`): preview, use
//! (copy into a new draft workflow at version 1) and dismiss.

use crate::graph::Graph;
use crate::ops::{self, EngineError, NewWorkflow};
use anyhow::Result;
use kls_core::Core;
use kls_core::engine::kinds;
use kls_store::engine::Workflow;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::OnceLock;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TemplateTrigger {
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub crews: Vec<String>,
    /// Pluggable integrations the template needs (`github`).
    #[serde(default)]
    pub integrations: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub category: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub flow: Vec<String>,
    pub trigger: TemplateTrigger,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default)]
    pub requires: Requires,
    pub graph: Graph,
}

impl Template {
    pub fn tokens_per_run(&self) -> (i64, i64) {
        crate::dryrun::estimate(&self.graph)
    }
}

pub const CATEGORIES: [&str; 5] = ["Delivery", "Testing", "Hygiene", "Operations", "Security"];

pub fn templates() -> &'static [Template] {
    static T: OnceLock<Vec<Template>> = OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str(include_str!("templates.json")).expect("templates.json is valid")
    })
}

pub fn template(id: &str) -> Option<&'static Template> {
    templates().iter().find(|t| t.id == id)
}

/// Every template with whether it was dismissed.
pub async fn template_list(core: &Core) -> Result<Vec<(Template, bool)>> {
    let dismissed = core.store().dismissed_templates().await?;
    Ok(templates()
        .iter()
        .map(|t| (t.clone(), dismissed.contains(&t.id)))
        .collect())
}

pub async fn set_template_dismissed(core: &Core, id: &str, dismissed: bool) -> Result<()> {
    if template(id).is_none() {
        return Err(EngineError::NotFound("template").into());
    }
    if dismissed {
        core.store().dismiss_template(id).await?;
    } else {
        core.store().undismiss_template(id).await?;
    }
    core.emit(
        kinds::TEMPLATE_DISMISSED,
        id,
        &json!({ "dismissed": dismissed }),
    )
    .await?;
    Ok(())
}

/// Copies the template into a new draft workflow. Nothing runs until it is
/// published. `name` defaults to the template's; `repos` sets the scope;
/// `run_budget` overrides the run budget.
pub async fn use_template(
    core: &Core,
    id: &str,
    name: Option<&str>,
    repos: Vec<String>,
    run_budget: Option<i64>,
) -> Result<Workflow> {
    let t = template(id).ok_or(EngineError::NotFound("template"))?;
    let mut g = t.graph.clone();
    if !repos.is_empty() {
        g.scope.repos = repos.into_iter().map(|r| r.trim().to_lowercase()).collect();
    }
    if let Some(b) = run_budget {
        g.limits.run_budget_tokens = Some(b);
    }
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&t.name);
    ops::create_workflow(
        core,
        NewWorkflow {
            name: name.to_string(),
            slug: None,
            description: t.description.clone(),
            template_id: t.id.clone(),
            graph: Some(g),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{crews, is_background_agent, workers};
    use crate::validate::{Mode, validate};

    #[test]
    fn every_template_is_clean() {
        assert!(templates().len() >= 8);
        let mut ids = std::collections::HashSet::new();
        for t in templates() {
            assert!(ids.insert(t.id.clone()), "duplicate {}", t.id);
            let issues = validate(&t.graph, Mode::Publish);
            assert!(issues.is_empty(), "{}: {issues:?}", t.id);
            assert!(CATEGORIES.contains(&t.category.as_str()), "{}", t.id);
            for n in &t.graph.nodes {
                for key in ["agent", "crew"] {
                    let v = n.str(key);
                    if !v.is_empty() {
                        assert!(!is_background_agent(v), "{} names {v}", t.id);
                        assert!(
                            workers().contains(&v) || crews().contains(&v),
                            "{} names {v}, not a palette worker or crew",
                            t.id
                        );
                    }
                }
                assert!(n.position.is_some(), "{} {} has no position", t.id, n.id);
            }
            assert!(
                t.graph
                    .triggers()
                    .any(|n| n.trigger_kind() == t.trigger.kind)
            );
        }
    }

    /// Every GitHub event the poller mirrors, as the webhook names
    /// `forge::webhook_events` gives them (kls-forge `poller.rs`/`events.rs`:
    /// pulls, issues, comments, push, workflow runs, releases, alerts, repos,
    /// packages, deployments, reviews and the risk sources).
    const EMITTED_WEBHOOKS: [&str; 29] = [
        "pull_request.opened",
        "pull_request.closed",
        "pull_request.merged",
        "pull_request.synchronize",
        "pull_request.ready_for_review",
        "pull_request.converted_to_draft",
        "pull_request.labeled",
        "pull_request_review.submitted",
        "registry_package.published",
        "deployment_status.created",
        "github-risk",
        "github-posture",
        "check_suite.completed",
        "check_run.completed",
        "issue_comment.created",
        "push",
        "workflow_run.completed",
        "release.published",
        "installation_repositories.added",
        "dependabot_alert.created",
        "code_scanning_alert.created",
        "secret_scanning_alert.created",
        "issues.opened",
        "issues.closed",
        "issues.reopened",
        "issues.edited",
        "issues.labeled",
        // base names a trigger may use
        "pull_request",
        "issues",
    ];

    #[test]
    fn every_trigger_event_is_one_the_poller_emits() {
        for t in templates() {
            for n in t.graph.triggers() {
                match n.trigger_kind() {
                    "webhook" => {
                        let ev = n.str("event");
                        assert!(
                            EMITTED_WEBHOOKS.contains(&ev),
                            "{} triggers on {ev}, which the poller never emits",
                            t.id
                        );
                    }
                    "label" => assert!(!n.str("label").is_empty(), "{}", t.id),
                    "manual" | "cron" | "kanban" => {}
                    // Factory events (session.ended, ...) come from the core.
                    "event" => assert!(!n.str("event").is_empty(), "{}", t.id),
                    other => panic!("{} has trigger kind {other}", t.id),
                }
            }
        }
    }

    #[test]
    fn every_template_dry_runs() {
        for t in templates() {
            let d = crate::dryrun::dry_run(&t.graph, &serde_json::Value::Null);
            assert!(d.valid, "{}: {:?}", t.id, d.issues);
            assert!(!d.plan.is_empty(), "{}", t.id);
        }
    }

    /// The web's 25 event templates (factory_templates_events.go), less the
    /// three whose package, AI and egress firewall sources Keleustes has no
    /// counterpart for.
    const EVENT_TEMPLATES: [&str; 22] = [
        "failure-explainer",
        "flaky-test-quarantine",
        "default-branch-broke",
        "build-artifacts-to-tea",
        "sbom-to-tea",
        "low-quality-pr-comment",
        "dependabot-strategy-check",
        "dependency-scope-fix",
        "actions-workflow-hardening",
        "issue-to-ticket",
        "secret-alert-triage",
        "dependabot-alert-triage",
        "code-scanning-triage",
        "release-steward",
        "repo-onboarding",
        "label-fix-tests",
        "label-repro",
        "label-review",
        "github-risk-investigation",
        "settings-fix-ticket",
        "deploy-failed-card",
        "container-published-to-tea",
    ];

    #[test]
    fn the_web_catalogue_is_ported() {
        let ported = [
            "nightly-blackbox",
            "feature-from-issue",
            "malware-response",
            "secret-leak-response",
            "ci-failure-fixer",
            "dependabot-triage",
            "ui-polish-pass",
            "security-patch-train",
            "release-provenance",
        ];
        for id in ported.iter().chain(EVENT_TEMPLATES.iter()) {
            assert!(template(id).is_some(), "missing template {id}");
        }
        // The package, AI and egress firewalls are Vulnetix SaaS sources
        // with no Keleustes counterpart, so these stay out.
        for id in [
            "new-package-review",
            "ai-capability-review",
            "egress-decision",
        ] {
            assert!(
                template(id).is_none(),
                "{id} needs a source Keleustes lacks"
            );
        }
    }

    #[test]
    fn event_templates_end_in_the_web_outputs() {
        let last = |id: &str| -> (String, String) {
            let g = &template(id).unwrap().graph;
            let n = g
                .nodes
                .iter()
                .find(|n| g.outgoing(&n.id).next().is_none() && n.kind != "condition")
                .unwrap();
            let what = match n.kind.as_str() {
                "github" => n.str("action").to_string(),
                "card" => format!("{}:{}", n.str("action"), n.str("ref")),
                "scan" => n.str("job").to_string(),
                k => k.to_string(),
            };
            (n.kind.clone(), what)
        };
        // The web's kanban_card state is a kept card.
        for (id, r) in [
            ("flaky-test-quarantine", "flaky-tests"),
            ("github-risk-investigation", "github-investigate"),
            ("settings-fix-ticket", "settings-fix"),
            ("deploy-failed-card", "deploy-failures"),
        ] {
            assert_eq!(last(id), ("card".into(), format!("ensure:{r}")), "{id}");
        }
        // The web's tea_publish is the CLI's tea-release job (a write job).
        assert!(crate::catalogue::cli_write_job("tea-release"));
        for id in [
            "build-artifacts-to-tea",
            "sbom-to-tea",
            "container-published-to-tea",
        ] {
            assert_eq!(last(id), ("scan".into(), "tea-release".into()), "{id}");
        }
        let event = |id: &str| {
            let t = template(id).unwrap();
            t.graph.triggers().next().unwrap().str("event").to_string()
        };
        assert_eq!(event("github-risk-investigation"), "github-risk");
        assert_eq!(event("settings-fix-ticket"), "github-posture");
        assert_eq!(event("deploy-failed-card"), "deployment_status.created");
        assert_eq!(
            event("container-published-to-tea"),
            "registry_package.published"
        );
    }
}
