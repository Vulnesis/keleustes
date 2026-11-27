//! The GitHub read node (`read`, the web palette's "GitHub reads"): what
//! the run's pull request, repository or issues look like, for the next
//! steps' conditions. `source` picks where from: `mirror` (the local
//! mirror, 0 GitHub calls), `webhook` (what the triggering event carried,
//! 0 calls) or `api` (a live refresh of the pull request, 1 call). A live
//! read that fails reads the mirror instead when `onQuotaLow` is
//! `use_cached` (the default); the fallback is recorded on the step, as an
//! event and in the ledger, never silent.

use crate::exec::{Forge, StepContext};
use crate::graph::{read_source, read_what};
use crate::guard::{self, Origin};
use anyhow::{Result, bail};
use kls_core::Core;
use serde_json::{Value, json};
use std::sync::Arc;

/// Emitted when a live read fell back to the mirror.
pub const READ_FALLBACK: &str = "read.source_fallback";

/// Most issues a mirror read lists.
const ISSUES_MAX: usize = 20;

/// The event data a webhook read returns: the bus event's `data`, else the
/// event itself (a manual start has none).
pub fn webhook_data(event: &Value) -> Option<Value> {
    let d = if event.get("data").is_some_and(Value::is_object) {
        &event["data"]
    } else {
        event
    };
    d.as_object().filter(|o| !o.is_empty()).map(|_| d.clone())
}

/// The mirror's answer for `what` on `repo` (and pull request `number`).
pub async fn mirror(core: &Core, what: &str, repo: &str, number: Option<i64>) -> Result<Value> {
    let s = core.store();
    if repo.is_empty() {
        bail!("the run names no repository");
    }
    match what {
        "repo" => {
            let r = s
                .gh_repo(repo)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{repo} is not in the mirror"))?;
            Ok(json!({
                "repo": r.full_name, "defaultBranch": r.default_branch, "visibility": r.visibility,
                "archived": r.archived, "fork": r.fork, "url": r.html_url, "pushedAt": r.pushed_at,
                "syncedAt": r.synced_at,
            }))
        }
        "issues" => {
            let open = s.gh_issues(Some(repo), Some("open")).await?;
            let items: Vec<Value> = open
                .iter()
                .take(ISSUES_MAX)
                .map(|i| json!({ "number": i.number, "title": i.title, "labels": i.labels }))
                .collect();
            Ok(json!({ "repo": repo, "open": open.len(), "items": items }))
        }
        _ => {
            let n = number.ok_or_else(|| anyhow::anyhow!("the run names no pull request"))?;
            let p = s
                .gh_pull(repo, n)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{repo}#{n} is not in the mirror"))?;
            Ok(crate::forge::pull_json(&p))
        }
    }
}

/// Runs a read step: the step's details on success, `Err((code, details))`
/// when it fails.
pub async fn run(
    core: &Core,
    forge: Option<Arc<dyn Forge>>,
    sc: &StepContext,
) -> std::result::Result<Value, (String, Value)> {
    let source = read_source(&sc.node).to_string();
    let what = read_what(&sc.node).to_string();
    let fail = |code: &str, why: String| {
        Err((
            code.to_string(),
            json!({ "source": source, "what": what, "why": why }),
        ))
    };
    if source == "webhook" {
        return match webhook_data(&sc.context["event"]) {
            Some(data) => Ok(json!({
                "source": "webhook", "what": what, "data": data, "githubCalls": 0,
            })),
            None => fail(
                "read_no_event",
                "the run was not started by an event".into(),
            ),
        };
    }
    let (repo, number) = crate::forge::pull_of(core, sc).await.unwrap_or_default();
    if source == "api" {
        let live = match &forge {
            Some(f) => f.action(sc, "read").await,
            None => Err(anyhow::anyhow!("GitHub is not connected")),
        };
        match live {
            Ok(data) => {
                return Ok(json!({
                    "source": "api", "what": what, "repo": repo, "number": number,
                    "data": data, "githubCalls": 1,
                }));
            }
            Err(e) if sc.node.str("onQuotaLow") == "fail" => {
                return fail("read_live_failed", format!("{e:#}"));
            }
            Err(e) => {
                let note = format!("{e:#}");
                let data = match mirror(core, &what, &repo, number).await {
                    Ok(d) => d,
                    Err(m) => return fail("read_live_failed", format!("{note}; {m:#}")),
                };
                let ev = json!({
                    "runId": sc.run.id, "stepId": sc.step.id, "workflowId": sc.run.workflow_id,
                    "nodeId": sc.node.id, "requested": "api", "used": "mirror", "reason": note,
                });
                let _ = core.emit(READ_FALLBACK, &sc.run.id, &ev).await;
                let _ = guard::audit(
                    core,
                    "read",
                    "source_fallback",
                    json!({ "repo": repo, "number": number }),
                    &Origin::engine(ev.clone()),
                    "mirror",
                    json!({ "nodeId": sc.node.id }),
                )
                .await;
                return Ok(json!({
                    "source": "mirror", "requestedSource": "api", "stale": true, "note": note,
                    "what": what, "repo": repo, "number": number, "data": data, "githubCalls": 0,
                }));
            }
        }
    }
    match mirror(core, &what, &repo, number).await {
        Ok(data) => Ok(json!({
            "source": "mirror", "what": what, "repo": repo, "number": number,
            "data": data, "githubCalls": 0,
        })),
        Err(e) => fail("read_not_mirrored", format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_data_unwraps_the_bus_event() {
        let ev = json!({"id": 1, "kind": "github.pull.opened", "data": {"repo": "acme/api", "number": 4}});
        assert_eq!(webhook_data(&ev).unwrap()["number"], 4);
        assert_eq!(
            webhook_data(&json!({"repo": "acme/api"})).unwrap()["repo"],
            "acme/api"
        );
        assert!(webhook_data(&json!({})).is_none());
        assert!(webhook_data(&Value::Null).is_none());
    }
}
