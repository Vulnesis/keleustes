//! The GitHub forge over the fake `gh` (no network), driven through the
//! engine: output steps, forge gates, and webhook and label triggers fired
//! from the mirror's `github.*` events.

use super::*;
use crate::exec::{CoreBoard, Engine, EngineConfig, Integrations};
use crate::graph::Graph;
use crate::ops::{self, NewWorkflow, StartRun, TriggerInfo};
use kls_core::board::CardDraft;
use kls_forge::fake::FakeGh;
use kls_store::engine::{Run, RunStep};
use kls_store::github::GhRepo;

struct T {
    core: Core,
    engine: Engine,
    gh: Arc<FakeGh>,
    _dir: tempfile::TempDir,
}

async fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let gh = FakeGh::new();
    let forge = GitHubForge::with_client(Client::new(gh.clone(), core.clone()));
    let engine = Engine::new(
        core.clone(),
        Integrations {
            forge: Some(Arc::new(forge)),
            board: Some(Arc::new(CoreBoard(core.clone()))),
        },
        EngineConfig {
            retry_backoff_ms: 0,
            ..Default::default()
        },
    );
    core.store()
        .gh_upsert_repo(&GhRepo {
            full_name: "acme/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            default_branch: "main".into(),
            settings: kls_store::github::RepoSettings {
                autonomy: "pr".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    core.store()
        .set_setting("autonomy.max", "pr")
        .await
        .unwrap();
    T {
        core,
        engine,
        gh,
        _dir: dir,
    }
}

fn pull(n: i64, draft: bool, checks: &[&str]) -> (Value, Value) {
    let detail = json!({
        "number": n, "title": format!("PR {n}"), "state": "open", "draft": draft,
        "mergeable": true, "mergeable_state": "clean",
        "user": {"login": "ana"}, "html_url": format!("https://github.com/acme/api/pull/{n}"),
        "head": {"ref": "feat", "sha": "h1", "repo": {"full_name": "acme/api"}},
        "base": {"ref": "main", "sha": "b1"},
        "requested_reviewers": [], "labels": [],
        "created_at": "2026-10-20T00:00:00Z", "updated_at": "2026-10-20T00:00:00Z"
    });
    let runs: Vec<Value> = checks
        .iter()
        .enumerate()
        .map(|(i, c)| match *c {
            "pending" => {
                json!({"name": format!("c{i}"), "status": "in_progress", "conclusion": null})
            }
            c => json!({"name": format!("c{i}"), "status": "completed", "conclusion": c}),
        })
        .collect();
    (
        detail,
        json!({"total_count": runs.len(), "check_runs": runs}),
    )
}

fn register(gh: &FakeGh, n: i64, draft: bool, checks: &[&str]) {
    let (detail, runs) = pull(n, draft, checks);
    gh.on("GET", &format!("/repos/acme/api/pulls/{n}"), 200, detail);
    gh.on(
        "GET",
        "/repos/acme/api/commits/h1/check-runs?per_page=100",
        200,
        runs,
    );
    gh.on(
        "GET",
        &format!("/repos/acme/api/pulls/{n}/reviews?per_page=100"),
        200,
        json!([]),
    );
}

fn flow(trigger: Value, step: Value) -> Graph {
    serde_json::from_value(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config": trigger},
            step,
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
    .unwrap()
}

async fn published(core: &Core, g: Graph) -> String {
    let w = ops::create_workflow(
        core,
        NewWorkflow {
            name: "Forge flow".into(),
            graph: Some(g),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ops::publish(core, &w.id, 1, "first").await.unwrap();
    w.id
}

async fn start(t: &T, wf: &str, card: &str, inputs: Value) -> Run {
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.into(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            card_id: card.into(),
            inputs,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    t.engine.advance(&r.id).await.unwrap();
    t.core.store().run(&r.id).await.unwrap().unwrap()
}

async fn steps(t: &T, id: &str) -> Vec<RunStep> {
    t.core.store().run_steps(id).await.unwrap()
}

#[test]
fn webhook_names_of_mirror_events() {
    let ev = |kind: &str| Event {
        id: 1,
        at: 0,
        kind: kind.into(),
        subject: "acme/api#1".into(),
        data_json: "{}".into(),
    };
    let names = webhook_events(
        &ev("github.pull.updated"),
        &json!({"readyForReview": true, "labelsAdded": ["x"]}),
    );
    assert!(names.contains(&"pull_request.ready_for_review".to_string()));
    assert!(names.contains(&"pull_request.labeled".to_string()));
    assert!(webhook_matches("pull_request", &names));
    assert!(!webhook_matches("issues", &names));
    let names = webhook_events(&ev("github.issue.opened"), &json!({}));
    assert_eq!(names, vec!["issues.opened".to_string()]);
    assert!(webhook_events(&ev("card.moved"), &json!({})).is_empty());
    assert_eq!(pull_number_of("https://github.com/a/b/pull/12"), Some(12));
    assert_eq!(
        pull_number_of("https://gitlab.example.com/g/sub/p/-/merge_requests/31"),
        Some(31)
    );
    assert_eq!(pull_number_of("https://codeberg.org/o/n/pulls/5"), Some(5));
    assert_eq!(pull_number_of("https://gitlab.com/g/p/-/issues/5"), None);
}

#[tokio::test]
async fn draft_pr_opens_once_and_links_the_card() {
    let t = setup().await;
    let card = t
        .core
        .create_card(
            CardDraft {
                title: "Fix login".into(),
                repo: "acme/api".into(),
                branch: "feat".into(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls",
        201,
        json!({"number": 7, "html_url": "https://github.com/acme/api/pull/7"}),
    );
    register(&t.gh, 7, true, &["success"]);
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Draft PR","config":{"action":"draft_pr","prState":"draft"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, &card.id, Value::Null).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    let posts: Vec<_> =
        t.gh.calls()
            .into_iter()
            .filter(|c| c.method == "POST" && c.path == "/repos/acme/api/pulls")
            .collect();
    assert_eq!(posts.len(), 1);
    let body: Value = serde_json::from_str(posts[0].stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["head"], "feat");
    assert_eq!(body["base"], "main");
    assert_eq!(body["draft"], true);
    assert_eq!(body["title"], "Fix login");
    let details: Value = serde_json::from_str(&steps(&t, &r.id).await[1].details_json).unwrap();
    assert_eq!(details["number"], 7);
    let card = t.core.card(&card.id).await.unwrap().unwrap();
    assert_eq!(card.pr_url, "https://github.com/acme/api/pull/7");

    // A second run finds the open pull request and opens none.
    let r = start(&t, &wf, &card.id, Value::Null).await;
    assert_eq!(r.status, "succeeded");
    assert_eq!(t.gh.count("POST", "/repos/acme/api/pulls"), 1);
}

#[tokio::test]
async fn comment_and_label_target_the_runs_pull() {
    let t = setup().await;
    t.gh.on(
        "POST",
        "/repos/acme/api/issues/9/comments",
        201,
        json!({"id": 1, "html_url": "u"}),
    );
    t.gh.on("POST", "/repos/acme/api/issues/9/labels", 200, json!([]));
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Say","config":{"action":"issue_comment","message":"Looked at {repo}#{number}"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, "", json!({"number": 9})).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    let call =
        t.gh.calls()
            .into_iter()
            .find(|c| c.path == "/repos/acme/api/issues/9/comments")
            .unwrap();
    assert!(call.stdin.unwrap().contains("Looked at acme/api#9"));

    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Tag","config":{"action":"add_label","label":"sf:triaged"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, "", json!({"number": 9})).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    assert_eq!(t.gh.count("POST", "/repos/acme/api/issues/9/labels"), 1);

    // No number: the step fails and says why.
    let r = start(&t, &wf, "", Value::Null).await;
    assert_eq!(r.status, "failed");
}

#[tokio::test]
async fn forge_gate_follows_checks() {
    let t = setup().await;
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"gate","title":"Checks","config":{"check":"forge"}}),
        ),
    )
    .await;
    register(&t.gh, 5, false, &["success", "pending"]);
    let r = start(&t, &wf, "", json!({"number": 5})).await;
    let st = steps(&t, &r.id).await;
    assert_eq!(st[1].status, "waiting");
    assert_eq!(st[1].error, "deferred:checks");

    register(&t.gh, 6, false, &["success", "failure"]);
    let r = start(&t, &wf, "", json!({"number": 6})).await;
    assert_eq!(r.status, "failed");
    assert!(
        steps(&t, &r.id).await[1]
            .details_json
            .contains("checks failed")
    );

    register(&t.gh, 8, false, &["success"]);
    let r = start(&t, &wf, "", json!({"number": 8})).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
}

#[tokio::test]
async fn approve_ready_marks_ready_and_never_merges() {
    let t = setup().await;
    register(&t.gh, 4, true, &["success"]);
    t.gh.on_cli(
        &["pr", "ready", "4"],
        kls_forge::GhOutput {
            exit: 0,
            ..Default::default()
        },
    );
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Ship","config":{"action":"approve_ready"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, "", json!({"number": 4})).await;
    // Marked ready, then it waits: no factory review of this head yet.
    assert_eq!(r.status, "running", "{r:?}");
    let st = steps(&t, &r.id).await;
    assert!(
        st[1].error.starts_with(crate::exec::READINESS_WAIT),
        "{:?}",
        st[1]
    );
    assert!(t.gh.calls().iter().any(|c| c.args.starts_with(&[
        "pr".to_string(),
        "ready".to_string(),
        "4".to_string()
    ])));
    // The factory never merges (factory.md 13).
    assert!(t.gh.calls().iter().all(|c| !c.path.ends_with("/merge")));
}

#[tokio::test]
async fn reviews_never_approve_and_check_run_writes_a_status() {
    let t = setup().await;
    register(&t.gh, 6, false, &["success"]);
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls/6/reviews",
        200,
        json!({"id": 1}),
    );
    t.gh.on("POST", "/repos/acme/api/statuses/h1", 201, json!({"id": 2}));
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Review","config":{"action":"pr_review","event":"APPROVE"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, "", json!({"number": 6})).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    let rv =
        t.gh.calls()
            .into_iter()
            .find(|c| c.path.ends_with("/reviews") && c.method == "POST")
            .unwrap();
    let body: Value = serde_json::from_str(rv.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(body["event"], "COMMENT");
    assert!(body["body"].as_str().unwrap().contains("keleustes:review"));
    let wf = published(
        &t.core,
        flow(
            json!({"kind":"manual"}),
            json!({"id":"b","type":"github","title":"Scan","config":{"action":"check_run","conclusion":"failure"}}),
        ),
    )
    .await;
    let r = start(&t, &wf, "", json!({"number": 6})).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    let st =
        t.gh.calls()
            .into_iter()
            .find(|c| c.path == "/repos/acme/api/statuses/h1")
            .unwrap();
    let body: Value = serde_json::from_str(st.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(
        (body["state"].as_str(), body["context"].as_str()),
        (Some("failure"), Some("Scan"))
    );
    assert!(
        t.gh.calls()
            .iter()
            .all(|c| !c.path.ends_with("/check-runs") || c.method == "GET")
    );
}

#[test]
fn webhook_names_of_polled_events() {
    let ev = |kind: &str| Event {
        id: 1,
        at: 0,
        kind: kind.into(),
        subject: "acme/api".into(),
        data_json: "{}".into(),
    };
    use kls_core::github::kinds as k;
    assert_eq!(
        webhook_events(&ev(k::PUSH), &json!({})),
        vec!["push".to_string()]
    );
    assert!(
        webhook_events(&ev(k::WORKFLOW_RUN), &json!({}))
            .contains(&"workflow_run.completed".to_string())
    );
    assert_eq!(
        webhook_events(&ev(k::RELEASE), &json!({})),
        vec!["release.published".to_string()]
    );
    assert_eq!(
        webhook_events(&ev(k::ALERT), &json!({"alert": "secret_scanning"})),
        vec!["secret_scanning_alert.created".to_string()]
    );
    assert_eq!(
        webhook_events(&ev(k::REPO_ADDED), &json!({})),
        vec!["installation_repositories.added".to_string()]
    );
    // cli-image triggers on `registry_package`; the review and risk sources too.
    let names = webhook_events(&ev(k::PACKAGE_PUBLISHED), &json!({}));
    assert!(webhook_matches("registry_package", &names));
    assert!(webhook_matches(
        "pull_request_review.submitted",
        &webhook_events(&ev(k::REVIEW_SUBMITTED), &json!({"state": "approved"}))
    ));
    assert_eq!(
        webhook_events(&ev(k::RISK), &json!({"source": "github-posture"})),
        vec!["github-posture".to_string()]
    );
    assert_eq!(
        webhook_events(&ev(k::RISK), &json!({})),
        vec!["github-risk".to_string()]
    );
    assert!(webhook_matches(
        "deployment_status.created",
        &webhook_events(&ev(k::DEPLOYMENT_STATUS), &json!({"state": "failure"}))
    ));
}

#[tokio::test]
async fn webhook_and_label_triggers_fire_from_mirror_events() {
    let t = setup().await;
    let say = json!({"id":"b","type":"notify","title":"Say","config":{"message":"hi"}});
    let on_open = published(
        &t.core,
        flow(
            json!({"kind":"webhook","event":"pull_request.opened"}),
            say.clone(),
        ),
    )
    .await;
    let on_label = published(
        &t.core,
        flow(json!({"kind":"label","label":"belai:fix"}), say),
    )
    .await;
    let runs = |wf: String| {
        let core = t.core.clone();
        async move {
            core.store()
                .workflow_runs_in(&wf, &["queued", "running", "succeeded"], None)
                .await
                .unwrap()
        }
    };

    let ev = t
        .core
        .emit(
            "github.pull.opened",
            "acme/api#3",
            &json!({"repo":"acme/api","number":3,"labelsAdded":[]}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let r = runs(on_open.clone()).await;
    assert_eq!(r.len(), 1, "deduped by event");
    assert_eq!(r[0].trigger_kind, "webhook");
    assert_eq!(r[0].repo, "acme/api");
    assert!(runs(on_label.clone()).await.is_empty());

    let ev = t
        .core
        .emit(
            "github.issue.updated",
            "acme/api#5",
            &json!({"repo":"acme/api","number":5,"state":"open","labelsAdded":["belai:fix"],"cardId":"card-5"}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let r = runs(on_label).await;
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].card_id, "card-5");
    assert_eq!(r[0].trigger_kind, "label");
    assert_eq!(runs(on_open).await.len(), 1);
}

// ---- outputs: file_issues, check_run, pr_review, approve_ready, draft_pr ----

use kls_store::audit::LedgerQuery;

/// A step context for node `b` of a fresh run, after an optional scan step
/// that left `scan` as its CLI result.
async fn ctx_for(
    t: &T,
    node: Value,
    card: &str,
    inputs: Value,
    scan: Option<Value>,
) -> StepContext {
    let wf = published(&t.core, flow(json!({"kind":"manual"}), node.clone())).await;
    ctx_in(t, &wf, node, card, inputs, scan).await
}

/// As [`ctx_for`], on an existing workflow (a rescan of the same one).
async fn ctx_in(
    t: &T,
    wf: &str,
    node: Value,
    card: &str,
    inputs: Value,
    scan: Option<Value>,
) -> StepContext {
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.into(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            card_id: card.into(),
            inputs,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let run = out.run().unwrap().clone();
    let s = t.core.store();
    if let Some(res) = scan {
        s.insert_step(&RunStep {
            id: uuid::Uuid::now_v7().to_string(),
            run_id: run.id.clone(),
            node_id: "s".into(),
            node_type: "scan".into(),
            status: "succeeded".into(),
            details_json: json!({ "result": res }).to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    s.insert_step(&RunStep {
        id: uuid::Uuid::now_v7().to_string(),
        run_id: run.id.clone(),
        node_id: "b".into(),
        node_type: "github".into(),
        status: "running".into(),
        details_json: "{}".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let steps = s.run_steps(&run.id).await.unwrap();
    let step = steps.iter().rfind(|x| x.node_id == "b").unwrap().clone();
    StepContext {
        context: crate::exec::run_context(&run, &steps),
        run,
        node: serde_json::from_value(node).unwrap(),
        step,
    }
}

fn forge_of(t: &T) -> GitHubForge {
    GitHubForge::with_client(Client::new(t.gh.clone(), t.core.clone()))
}

fn posted(gh: &FakeGh, method: &str, path: &str) -> Vec<Value> {
    gh.calls()
        .into_iter()
        .filter(|c| c.method == method && c.path == path)
        .map(|c| serde_json::from_str(c.stdin.as_deref().unwrap_or("null")).unwrap_or(Value::Null))
        .collect()
}

async fn forge_audit(core: &Core) -> Vec<(String, String, String)> {
    core.audit_entries(&LedgerQuery {
        kinds: vec!["forge".into()],
        ..Default::default()
    })
    .await
    .unwrap()
    .into_iter()
    .map(|e| {
        (
            e.subject["action"].as_str().unwrap_or("").to_string(),
            e.actor.clone(),
            e.payload["outcome"].as_str().unwrap_or("").to_string(),
        )
    })
    .collect()
}

fn scan_result(findings: Value, exit: i64) -> Value {
    json!({"job": "sca", "exitCode": exit, "counts": {"high": 2, "low": 1}, "findings": findings})
}

const ISSUES: &str = "/repos/acme/api/issues?labels=sf%3Afix-deps&state=all&sort=created&direction=desc&per_page=100";

#[tokio::test]
async fn file_issues_files_findings_and_a_rescan_updates_the_same_issue() {
    let t = setup().await;
    let f = forge_of(&t);
    t.gh.on("GET", "/user", 200, json!({"login": "kel"}));
    t.gh.on(
        "POST",
        "/repos/acme/api/labels",
        201,
        json!({"name": "sf:fix-deps"}),
    );
    t.gh.on("GET", ISSUES, 200, json!([]));
    t.gh.on(
        "POST",
        "/repos/acme/api/issues",
        201,
        json!({"number": 11, "html_url": "https://github.com/acme/api/issues/11"}),
    );
    let node = json!({"id":"b","type":"github","title":"File","config":{
        "action":"file_issues","label":"sf:fix-deps","title":"Vulnetix dependency findings","minSeverity":"high"}});
    let first = scan_result(
        json!([{"id":"CVE-2024-0001","kind":"vuln","path":"package.json","severity":"high","ref":"lodash@4"},
               {"id":"LOW-1","kind":"vuln","path":"a","severity":"low","ref":""}]),
        1,
    );
    let ctx = ctx_for(&t, node.clone(), "", Value::Null, Some(first)).await;
    let wf = ctx.run.workflow_id.clone();
    let out = f.action(&ctx, "file_issues").await.unwrap();
    assert_eq!(out["findings"], 1, "{out}");
    assert_eq!(out["issues"][0]["created"], true);
    let made = posted(&t.gh, "POST", "/repos/acme/api/issues");
    assert_eq!(made.len(), 1);
    let body = made[0]["body"].as_str().unwrap().to_string();
    assert!(
        body.contains("`CVE-2024-0001`") && !body.contains("LOW-1"),
        "{body}"
    );
    assert!(body.contains("<!-- keleustes:findings "));
    assert!(body.contains("Vulnerability: https://www.vulnes.is/vdb/CVE-2024-0001"));
    assert_eq!(made[0]["labels"], json!(["sf:fix-deps"]));
    assert_eq!(made[0]["title"], "Vulnetix dependency findings");

    // The rescan finds its own issue by the marker (a stranger's copy of the
    // marker is left alone), reopens it and rewrites the findings.
    t.gh.on(
        "GET",
        ISSUES,
        200,
        json!([
            {"number": 3, "user": {"login": "mallory"}, "body": body, "state": "open", "labels": [{"name":"sf:fix-deps"}]},
            {"number": 11, "user": {"login": "kel"}, "body": body, "state": "closed", "labels": [{"name":"sf:fix-deps"}]},
        ]),
    );
    t.gh.on(
        "PATCH",
        "/repos/acme/api/issues/11",
        200,
        json!({"number": 11}),
    );
    let second = scan_result(
        json!([{"id":"CVE-2024-0009","kind":"vuln","path":"package.json","severity":"critical","ref":"x@1"}]),
        1,
    );
    let ctx = ctx_in(&t, &wf, node.clone(), "", Value::Null, Some(second)).await;
    let out = f.action(&ctx, "file_issues").await.unwrap();
    assert_eq!(out["issues"][0]["number"], 11, "{out}");
    assert_eq!(out["issues"][0]["created"], false);
    assert_eq!(posted(&t.gh, "POST", "/repos/acme/api/issues").len(), 1);
    assert!(posted(&t.gh, "PATCH", "/repos/acme/api/issues/3").is_empty());
    let edits = posted(&t.gh, "PATCH", "/repos/acme/api/issues/11");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["state"], "open");
    assert!(edits[0]["body"].as_str().unwrap().contains("CVE-2024-0009"));

    // Nothing at or above the floor: no write at all.
    t.gh.clear_calls();
    let clean = scan_result(
        json!([{"id":"L","kind":"vuln","path":"a","severity":"low","ref":""}]),
        0,
    );
    let ctx = ctx_in(&t, &wf, node, "", Value::Null, Some(clean)).await;
    let out = f.action(&ctx, "file_issues").await.unwrap();
    assert_eq!(out["skipped"], "no_findings");
    assert!(t.gh.calls().iter().all(|c| c.method == "GET"));

    // Every write is audited with its origin (the workflow).
    let audit = forge_audit(&t.core).await;
    assert!(
        audit
            .iter()
            .any(|(a, actor, o)| a == "POST /repos/acme/api/issues"
                && actor.starts_with("workflow ")
                && o == "ok")
    );
    assert!(
        audit
            .iter()
            .any(|(a, _, o)| a == "PATCH /repos/acme/api/issues/11" && o == "ok")
    );
}

#[tokio::test]
async fn file_issues_group_by_id_caps_and_is_refused_when_stopped() {
    let t = setup().await;
    let f = forge_of(&t);
    t.gh.on("GET", "/user", 200, json!({"login": "kel"}));
    t.gh.on(
        "POST",
        "/repos/acme/api/labels",
        422,
        json!({"message": "exists"}),
    );
    let list = "/repos/acme/api/issues?labels=sf%3Afix-code&state=all&sort=created&direction=desc&per_page=100";
    t.gh.on("GET", list, 200, json!([]));
    t.gh.on("POST", "/repos/acme/api/issues", 201, json!({"number": 20}));
    let node = json!({"id":"b","type":"github","title":"File","config":{
        "action":"file_issues","label":"sf:fix-code","title":"Vulnetix code finding","minSeverity":"high","group":"id","maxIssues":2}});
    let res = scan_result(
        json!([{"id":"rule/a","kind":"sast","path":"a.rs","severity":"high","ref":""},
               {"id":"rule/b","kind":"sast","path":"b.rs","severity":"critical","ref":""},
               {"id":"rule/c","kind":"sast","path":"c.rs","severity":"high","ref":""}]),
        1,
    );
    let ctx = ctx_for(&t, node.clone(), "", Value::Null, Some(res.clone())).await;
    let out = f.action(&ctx, "file_issues").await.unwrap();
    assert_eq!(
        (out["groups"].as_i64(), out["omitted"].as_i64()),
        (Some(2), Some(1)),
        "{out}"
    );
    let titles: Vec<String> = posted(&t.gh, "POST", "/repos/acme/api/issues")
        .iter()
        .map(|b| b["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Vulnetix code finding: rule-b",
            "Vulnetix code finding: rule-a"
        ]
    );

    // Under the kill switch the write is refused and the refusal audited.
    t.gh.clear_calls();
    let ctx = ctx_for(&t, node, "", Value::Null, Some(res)).await;
    kls_core::control::stop(&t.core, "test").await.unwrap();
    assert!(f.action(&ctx, "file_issues").await.is_err());
    assert!(t.gh.calls().iter().all(|c| c.method == "GET"));
    assert!(
        forge_audit(&t.core)
            .await
            .iter()
            .any(|(a, _, o)| a.starts_with("POST /repos/acme/api/") && o == "refused")
    );
}

#[tokio::test]
async fn check_run_conclusion_follows_findings_exit_code_and_when() {
    let t = setup().await;
    let f = forge_of(&t);
    register(&t.gh, 6, false, &["success"]);
    t.gh.on("POST", "/repos/acme/api/statuses/h1", 201, json!({"id": 2}));
    let node = |when: &str| {
        json!({"id":"b","type":"github","title":"Check","config":{
            "action":"check_run","name":"Vulnetix jail","minSeverity":"high","when": when}})
    };
    let state = |gh: &FakeGh| {
        posted(gh, "POST", "/repos/acme/api/statuses/h1")
            .last()
            .map(|b| {
                (
                    b["state"].as_str().unwrap_or("").to_string(),
                    b["context"].as_str().unwrap_or("").to_string(),
                )
            })
    };
    // Exit 1 fails even with no finding at the floor.
    let ctx = ctx_for(
        &t,
        node("always"),
        "",
        json!({"number": 6}),
        Some(scan_result(json!([]), 1)),
    )
    .await;
    let out = f.action(&ctx, "check_run").await.unwrap();
    assert_eq!(out["conclusion"], "failure");
    assert_eq!(
        state(&t.gh),
        Some(("failure".into(), "Vulnetix jail".into()))
    );
    // Exit 3: the CLI could not evaluate: neutral (a success status).
    let ctx = ctx_for(
        &t,
        node("always"),
        "",
        json!({"number": 6}),
        Some(scan_result(json!([]), 3)),
    )
    .await;
    assert_eq!(
        f.action(&ctx, "check_run").await.unwrap()["conclusion"],
        "neutral"
    );
    assert_eq!(state(&t.gh).unwrap().0, "success");
    // A finding at the floor fails.
    let hit = json!([{"id":"x","kind":"jail","path":"","severity":"high","ref":""}]);
    let ctx = ctx_for(
        &t,
        node("always"),
        "",
        json!({"number": 6}),
        Some(scan_result(hit, 0)),
    )
    .await;
    let out = f.action(&ctx, "check_run").await.unwrap();
    assert_eq!(
        (out["conclusion"].as_str(), out["findings"].as_i64()),
        (Some("failure"), Some(1))
    );
    // when: on_findings posts nothing for a clean scan.
    t.gh.clear_calls();
    let ctx = ctx_for(
        &t,
        node("on_findings"),
        "",
        json!({"number": 6}),
        Some(scan_result(json!([]), 0)),
    )
    .await;
    let out = f.action(&ctx, "check_run").await.unwrap();
    assert_eq!(out["skipped"], "no_findings");
    assert!(posted(&t.gh, "POST", "/repos/acme/api/statuses/h1").is_empty());
}

#[tokio::test]
async fn pr_review_posts_the_verdict_note_with_inline_comments_once() {
    let t = setup().await;
    let f = forge_of(&t);
    register(&t.gh, 6, false, &["success"]);
    t.gh.on(
        "GET",
        "/repos/acme/api/pulls/6/files?per_page=100",
        200,
        json!([{"filename": "src/db.rs", "patch": "@@ -10,2 +10,3 @@\n a\n+b\n c"}]),
    );
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls/6/reviews",
        200,
        json!({"id": 5}),
    );
    let meta = json!({"belai": {"history": [
        {"to": "doing", "at": 1, "note": "claimed"},
        {"to": "review", "at": 2, "note": "verdict: request_changes\nfinding: src/db.rs:11: unchecked input\nfinding: src/db.rs:90: off the diff"}
    ]}});
    let card = t
        .core
        .create_card(
            CardDraft {
                title: "Review #6".into(),
                repo: "acme/api".into(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
    let node = json!({"id":"b","type":"github","title":"Review","config":{"action":"pr_review"}});
    let ctx = ctx_for(&t, node.clone(), &card.id, json!({"number": 6}), None).await;
    let out = f.action(&ctx, "pr_review").await.unwrap();
    assert_eq!(
        (out["verdict"].as_str(), out["inline"].as_i64()),
        (Some("request_changes"), Some(1)),
        "{out}"
    );
    let rv = posted(&t.gh, "POST", "/repos/acme/api/pulls/6/reviews");
    assert_eq!(rv.len(), 1);
    assert_eq!(rv[0]["event"], "REQUEST_CHANGES");
    assert_eq!(rv[0]["commit_id"], "h1");
    assert_eq!(
        rv[0]["comments"],
        json!([{"path":"src/db.rs","line":11,"side":"RIGHT","body":"unchecked input"}])
    );
    let body = rv[0]["body"].as_str().unwrap();
    assert!(body.contains("`src/db.rs:90`: off the diff"));
    assert!(body.contains("sha=h1 verdict=request_changes"));

    // The same run and node on the same head posts no second review.
    let again = json!([{"id": 5, "state": "CHANGES_REQUESTED", "body": body, "commit_id": "h1", "user": {"login": "kel"}}]);
    t.gh.on(
        "GET",
        "/repos/acme/api/pulls/6/reviews?per_page=100",
        200,
        again,
    );
    let ctx2 = StepContext {
        step: ctx.step.clone(),
        ..ctx
    };
    let out = f.action(&ctx2, "pr_review").await.unwrap();
    assert_eq!(out["skipped"], "already_reviewed");
    assert_eq!(
        posted(&t.gh, "POST", "/repos/acme/api/pulls/6/reviews").len(),
        1
    );
}

#[tokio::test]
async fn pr_review_posts_the_review_jobs_findings_over_a_verdict_note() {
    use kls_core::belai::review;
    let t = setup().await;
    let f = forge_of(&t);
    register(&t.gh, 6, false, &["success"]);
    t.gh.on(
        "GET",
        "/repos/acme/api/pulls/6/files?per_page=100",
        200,
        json!([{"filename": "src/db.rs", "patch": "@@ -10,2 +10,3 @@\n a\n+b\n c"}]),
    );
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls/6/reviews",
        200,
        json!({"id": 9}),
    );
    // A model's verdict note is on the card too; the review job wins.
    let meta = json!({"belai": {"history": [
        {"to": "review", "at": 2, "note": "verdict: approve"}
    ]}});
    let card = t
        .core
        .create_card(
            CardDraft {
                title: "Review #6".into(),
                repo: "acme/api".into(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
    let node = json!({"id":"b","type":"github","title":"Review","config":{"action":"pr_review"}});
    let ctx = ctx_for(&t, node, &card.id, json!({"number": 6}), None).await;
    let host = json!({"verdict": "request_changes", "target": "pr", "pr": 6, "files": 1, "findings": [
        {"path": "src/db.rs", "line": 11, "severity": "major", "criterion": "security",
         "body": "unchecked input reaches the query @team", "fix": "bind it"},
        {"path": "src/db.rs", "line": 90, "severity": "minor", "criterion": "docs", "body": "off the diff"}
    ]})
    .to_string();
    // A review of another pull request in the same run is not this one's.
    let other = review::Request {
        repo: "acme/api".into(),
        target: "pr".into(),
        reference: "5".into(),
        run_id: ctx.run.id.clone(),
        ..Default::default()
    };
    review::record(
        &t.core,
        &other,
        "n1",
        &json!({"verdict": "approve", "target": "pr", "pr": 5, "findings": []}).to_string(),
        "t",
    )
    .await
    .unwrap();
    let req = review::Request {
        reference: "6".into(),
        ..other
    };
    let kept = review::record(&t.core, &req, "n1", &host, "t")
        .await
        .unwrap();
    let out = f.action(&ctx, "pr_review").await.unwrap();
    assert_eq!(
        (
            out["verdict"].as_str(),
            out["source"].as_str(),
            out["inline"].as_i64()
        ),
        (Some("request_changes"), Some("review"), Some(1)),
        "{out}"
    );
    assert_eq!(out["review"]["id"], kept.id);
    let rv = posted(&t.gh, "POST", "/repos/acme/api/pulls/6/reviews");
    assert_eq!(rv.len(), 1);
    assert_eq!(rv[0]["event"], "REQUEST_CHANGES");
    let inline = rv[0]["comments"][0]["body"].as_str().unwrap();
    assert!(
        inline.starts_with("major security: unchecked input"),
        "{inline}"
    );
    assert!(
        inline.contains("@\u{200b}team") && inline.ends_with("Fix: bind it"),
        "{inline}"
    );
    let body = rv[0]["body"].as_str().unwrap();
    assert!(
        body.contains("`src/db.rs:90`: minor docs: off the diff"),
        "{body}"
    );
}

fn approvable(gh: &FakeGh, checks: &[&str], files: &[&str]) {
    register(gh, 4, false, checks);
    gh.on("GET", "/user", 200, json!({"login": "kel"}));
    gh.on(
        "GET",
        "/repos/acme/api/pulls/4/reviews?per_page=100",
        200,
        json!([{"id": 1, "state": "COMMENTED", "commit_id": "h1", "user": {"login": "kel"},
                "body": outputs::review_marker("run_1", "n9", "h1", "approve")}]),
    );
    let list: Vec<Value> = files.iter().map(|f| json!({"filename": f})).collect();
    gh.on(
        "GET",
        "/repos/acme/api/pulls/4/files?per_page=100",
        200,
        json!(list),
    );
    gh.on(
        "POST",
        "/repos/acme/api/pulls/4/reviews",
        200,
        json!({"id": 77}),
    );
}

#[tokio::test]
async fn approve_ready_approves_only_a_ready_head() {
    let t = setup().await;
    let f = forge_of(&t);
    let node =
        json!({"id":"b","type":"github","title":"Approve","config":{"action":"approve_ready"}});
    let approvals = |gh: &FakeGh| {
        posted(gh, "POST", "/repos/acme/api/pulls/4/reviews")
            .into_iter()
            .filter(|b| b["event"] == "APPROVE")
            .count()
    };
    let refusals = |core: Core| async move {
        core.audit_entries(&LedgerQuery {
            kinds: vec!["forge".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.payload["outcome"] == "refused")
        .map(|e| {
            e.payload["detail"]["reason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect::<Vec<_>>()
    };

    // A stale head: the run was for h0, the pull is now at h1.
    approvable(&t.gh, &["success"], &["src/lib.rs"]);
    let ctx = ctx_for(
        &t,
        node.clone(),
        "",
        json!({"number": 4, "headSha": "h0"}),
        None,
    )
    .await;
    let out = f.action(&ctx, "approve_ready").await.unwrap();
    assert_eq!(
        (out["approved"].as_bool(), out["reason"].as_str()),
        (Some(false), Some("stale_sha")),
        "{out}"
    );
    assert_eq!(approvals(&t.gh), 0);

    // A red check.
    approvable(&t.gh, &["success", "failure"], &["src/lib.rs"]);
    let ctx = ctx_for(&t, node.clone(), "", json!({"number": 4}), None).await;
    let out = f.action(&ctx, "approve_ready").await.unwrap();
    assert_eq!(out["reason"], "checks_failed", "{out}");
    assert_eq!(approvals(&t.gh), 0);

    // A sensitive path, built in and from approve.sensitive_paths.
    approvable(
        &t.gh,
        &["success"],
        &["src/lib.rs", ".github/workflows/ci.yml"],
    );
    let ctx = ctx_for(&t, node.clone(), "", json!({"number": 4}), None).await;
    let out = f.action(&ctx, "approve_ready").await.unwrap();
    assert_eq!(
        out["reason"], "sensitive_path:.github/workflows/**",
        "{out}"
    );
    t.core
        .store()
        .set_setting("approve.sensitive_paths", "docs/**")
        .await
        .unwrap();
    approvable(&t.gh, &["success"], &["docs/a.md"]);
    let ctx = ctx_for(&t, node.clone(), "", json!({"number": 4}), None).await;
    let out = f.action(&ctx, "approve_ready").await.unwrap();
    assert_eq!(out["reason"], "sensitive_path:docs/**", "{out}");
    assert_eq!(approvals(&t.gh), 0);
    let why = refusals(t.core.clone()).await;
    for r in ["stale_sha", "checks_failed", "sensitive_path"] {
        assert!(why.iter().any(|w| w == r), "{r} not audited: {why:?}");
    }

    // Ready: one APPROVE on the head it judged, and never a merge.
    approvable(&t.gh, &["success"], &["src/lib.rs"]);
    let ctx = ctx_for(
        &t,
        node.clone(),
        "",
        json!({"number": 4, "headSha": "h1"}),
        None,
    )
    .await;
    let out = f.action(&ctx, "approve_ready").await.unwrap();
    assert_eq!(out["approved"], true, "{out}");
    let ap: Vec<Value> = posted(&t.gh, "POST", "/repos/acme/api/pulls/4/reviews")
        .into_iter()
        .filter(|b| b["event"] == "APPROVE")
        .collect();
    assert_eq!(ap.len(), 1);
    assert_eq!(ap[0]["commit_id"], "h1");
    assert!(t.gh.calls().iter().all(|c| !c.path.ends_with("/merge")));
}

#[tokio::test]
async fn draft_pr_stacks_on_pr_head_requests_reviewers_and_honours_settings() {
    let t = setup().await;
    let f = forge_of(&t);
    // The pull request the run is about: #5, head `feat` in this repository.
    register(&t.gh, 5, false, &["success"]);
    let card = t
        .core
        .create_card(
            CardDraft {
                title: "Fix CI".into(),
                repo: "acme/api".into(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls",
        201,
        json!({"number": 7, "html_url": "https://github.com/acme/api/pull/7"}),
    );
    register(&t.gh, 7, false, &["success"]);
    t.gh.on(
        "POST",
        "/repos/acme/api/pulls/7/requested_reviewers",
        201,
        json!({}),
    );
    t.core
        .store()
        .set_setting("forge.pr_state", "ready")
        .await
        .unwrap();
    let node = json!({"id":"b","type":"github","title":"Draft PR","config":{
        "action":"draft_pr","base":"pr_head","reviewers":"codeowners, bob, acme/sec"}});
    let ctx = ctx_for(&t, node, &card.id, json!({"number": 5}), None).await;
    let out = f.action(&ctx, "draft_pr").await.unwrap();
    let pr = posted(&t.gh, "POST", "/repos/acme/api/pulls");
    assert_eq!(pr.len(), 1, "{out}");
    assert_eq!(
        pr[0]["base"], "feat",
        "pr_head is the pull request's head branch"
    );
    // No branch on the card: named by forge.branch_prefix and the card key.
    assert_eq!(pr[0]["head"], branch_name("belai/K-", &card.key));
    assert!(pr[0]["head"].as_str().unwrap().starts_with("belai/K-"));
    assert_eq!(pr[0]["draft"], false, "forge.pr_state = ready");
    let rq = posted(&t.gh, "POST", "/repos/acme/api/pulls/7/requested_reviewers");
    assert_eq!(
        rq,
        vec![json!({"reviewers": ["bob"], "team_reviewers": ["sec"]})]
    );

    // A protected head branch is refused and audited.
    let node = json!({"id":"b","type":"github","title":"Draft PR","config":{"action":"draft_pr","branch":"main"}});
    let ctx = ctx_for(&t, node, "", Value::Null, None).await;
    let err = f.action(&ctx, "draft_pr").await.unwrap_err();
    assert!(format!("{err:#}").contains("protected"), "{err:#}");
    assert!(
        forge_audit(&t.core)
            .await
            .iter()
            .any(|(a, _, o)| a == "draft_pr" && o == "refused")
    );
    assert_eq!(posted(&t.gh, "POST", "/repos/acme/api/pulls").len(), 1);
}

#[tokio::test]
async fn pr_head_falls_back_to_the_default_branch_for_a_fork() {
    let t = setup().await;
    let f = forge_of(&t);
    let (mut detail, runs) = pull(5, false, &["success"]);
    detail["head"]["repo"]["full_name"] = json!("eve/api");
    t.gh.on("GET", "/repos/acme/api/pulls/5", 200, detail);
    t.gh.on(
        "GET",
        "/repos/acme/api/commits/h1/check-runs?per_page=100",
        200,
        runs,
    );
    t.gh.on(
        "GET",
        "/repos/acme/api/pulls/5/reviews?per_page=100",
        200,
        json!([]),
    );
    t.gh.on("POST", "/repos/acme/api/pulls", 201, json!({"number": 7}));
    register(&t.gh, 7, true, &["success"]);
    let node = json!({"id":"b","type":"github","title":"Draft PR","config":{
        "action":"draft_pr","base":"pr_head","branch":"belai/K-abc"}});
    let ctx = ctx_for(&t, node, "", json!({"number": 5}), None).await;
    f.action(&ctx, "draft_pr").await.unwrap();
    assert_eq!(
        posted(&t.gh, "POST", "/repos/acme/api/pulls")[0]["base"],
        "main"
    );
}

/// Live: the `pr_review` post step against real GitHub through the local
/// `gh`, on a pull request named by `KLS_LIVE_PR=owner/repo#N` (whose diff has
/// `docs/review-probe.md`). Posts one COMMENT review with one inline finding,
/// then proves the second call is a no-op. `cargo test -p kls-engine --
/// --ignored live_pr_review`.
#[tokio::test]
#[ignore = "needs gh signed in and KLS_LIVE_PR"]
async fn live_pr_review_posts_once_through_real_gh() {
    let Ok(target) = std::env::var("KLS_LIVE_PR") else {
        return;
    };
    let (repo, n) = target.split_once('#').expect("owner/repo#N");
    let n: i64 = n.parse().unwrap();
    let (owner, name) = repo.split_once('/').unwrap();
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let gh: Arc<dyn kls_forge::ForgeCli> = Arc::new(kls_forge::local::LocalGh::default());
    let f = GitHubForge::with_client(Client::new(gh, core.clone()));
    core.store()
        .gh_upsert_repo(&GhRepo {
            full_name: repo.into(),
            owner: owner.into(),
            name: name.into(),
            default_branch: "main".into(),
            settings: kls_store::github::RepoSettings {
                autonomy: "pr".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    core.store()
        .set_setting("autonomy.max", "pr")
        .await
        .unwrap();
    let meta = json!({"belai": {"history": [
        {"to": "doing", "at": 1, "note": "claimed"},
        {"to": "review", "at": 2, "note": "verdict: approve\nfinding: docs/review-probe.md:3: probe finding on the diff\nfinding: README.md:1: probe finding off the diff"}
    ]}});
    let card = core
        .create_card(
            CardDraft {
                title: format!("Review #{n}"),
                repo: repo.into(),
                meta_json: meta.to_string(),
                ..Default::default()
            },
            "t",
        )
        .await
        .unwrap();
    let node = json!({"id":"b","type":"github","title":"Review","config":{"action":"pr_review"}});
    let wf = published(&core, flow(json!({"kind":"manual"}), node.clone())).await;
    let run = ops::start_run(
        &core,
        StartRun {
            workflow_id: wf,
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: repo.into(),
            card_id: card.id.clone(),
            inputs: json!({"number": n}),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .run()
    .unwrap()
    .clone();
    core.store()
        .insert_step(&RunStep {
            id: uuid::Uuid::now_v7().to_string(),
            run_id: run.id.clone(),
            node_id: "b".into(),
            node_type: "github".into(),
            status: "running".into(),
            details_json: "{}".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let steps = core.store().run_steps(&run.id).await.unwrap();
    let ctx = StepContext {
        context: crate::exec::run_context(&run, &steps),
        run: run.clone(),
        node: serde_json::from_value(node).unwrap(),
        step: steps.iter().rfind(|x| x.node_id == "b").unwrap().clone(),
    };
    let out = f.action(&ctx, "pr_review").await.unwrap();
    eprintln!("first: {out}");
    assert_eq!(out["event"], "COMMENT", "{out}");
    assert_eq!(out["inline"], 1, "{out}");
    assert!(out["reviewId"].is_number(), "{out}");
    let ctx2 = StepContext {
        step: ctx.step.clone(),
        ..ctx
    };
    let again = f.action(&ctx2, "pr_review").await.unwrap();
    eprintln!("second: {again}");
    assert_eq!(again["skipped"], "already_reviewed", "{again}");
}

/// A scout run (scan, then `profile`) with `signals` as the scan's result,
/// advanced once.
async fn scout(t: &T, wf: &str, signals: &[&str]) -> Run {
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.into(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    t.core
        .store()
        .insert_step(&RunStep {
            id: uuid::Uuid::now_v7().to_string(),
            run_id: r.id.clone(),
            node_id: "s".into(),
            node_type: "scan".into(),
            status: "succeeded".into(),
            details_json: json!({ "result": { "signals": signals } }).to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    t.core.store().run(&r.id).await.unwrap().unwrap()
}

fn profile_details(st: &[RunStep]) -> Value {
    let p = st
        .iter()
        .find(|s| s.node_id == "b")
        .unwrap_or_else(|| panic!("{st:#?}"));
    serde_json::from_str(&p.details_json).unwrap()
}

/// The repo-scout shape: a CLI scout scan, then the `profile` output.
fn scout_flow() -> Graph {
    serde_json::from_value(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"s","type":"scan","title":"Scout","config":{"source":"cli","job":"scout"}},
            {"id":"b","type":"profile","title":"Store","config":{"output":"repo_profile"}},
        ],
        "edges": [{"from":"t","to":"s"},{"from":"s","to":"b"}]
    }))
    .unwrap()
}

const PROFILE_ISSUES: &str =
    "/repos/acme/api/issues?labels=sf%3Aprofile&state=all&sort=created&direction=desc&per_page=100";

#[tokio::test]
async fn profile_step_keeps_the_labels_and_one_profile_issue() {
    let t = setup().await;
    t.gh.on("GET", "/user", 200, json!({"login": "kel"}));
    t.gh.on(
        "POST",
        "/repos/acme/api/labels",
        201,
        json!({"name": "sf:scout"}),
    );
    t.gh.on("GET", PROFILE_ISSUES, 200, json!([]));
    t.gh.on(
        "POST",
        "/repos/acme/api/issues",
        201,
        json!({"number": 5, "html_url": "https://github.com/acme/api/issues/5"}),
    );
    let wf = published(&t.core, scout_flow()).await;
    let r = scout(&t, &wf, &["Dockerfile"]).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    // The twelve CLI labels, then the one marked profile issue.
    assert_eq!(
        posted(&t.gh, "POST", "/repos/acme/api/labels").len(),
        PROFILE_LABELS.len()
    );
    let made = posted(&t.gh, "POST", "/repos/acme/api/issues");
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["title"], "Software Factory profile");
    assert_eq!(made[0]["labels"], json!(["sf:profile"]));
    let body = made[0]["body"].as_str().unwrap().to_string();
    assert!(body.contains("Signals: `dockerfile`"), "{body}");
    assert!(body.contains(PROFILE_MARKER));
    let d = profile_details(&steps(&t, &r.id).await);
    assert_eq!(d["profileIssue"]["number"], 5, "{d}");
    assert_eq!(d["profileIssue"]["created"], true);
    assert_eq!(d["profile"]["signals"], json!(["dockerfile"]));

    // A rescan with new signals edits the same issue (a stranger's copy of
    // the marker is left alone) and opens none.
    t.gh.on(
        "GET",
        PROFILE_ISSUES,
        200,
        json!([
            {"number": 3, "user": {"login": "mallory"}, "body": body, "state": "open", "labels": [{"name":"sf:profile"}]},
            {"number": 5, "user": {"login": "kel"}, "body": body, "state": "open", "labels": [{"name":"sf:profile"}]},
        ]),
    );
    t.gh.on(
        "PATCH",
        "/repos/acme/api/issues/5",
        200,
        json!({"number": 5}),
    );
    let r = scout(&t, &wf, &["Dockerfile", "terraform"]).await;
    assert_eq!(r.status, "succeeded");
    assert_eq!(posted(&t.gh, "POST", "/repos/acme/api/issues").len(), 1);
    assert!(posted(&t.gh, "PATCH", "/repos/acme/api/issues/3").is_empty());
    let edits = posted(&t.gh, "PATCH", "/repos/acme/api/issues/5");
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0]["body"]
            .as_str()
            .unwrap()
            .contains("`dockerfile`, `terraform`")
    );
    let d = profile_details(&steps(&t, &r.id).await);
    assert_eq!(d["profileIssue"]["created"], false, "{d}");
    // The write is audited on both ledgers: the forge call and the output.
    assert!(
        forge_audit(&t.core)
            .await
            .iter()
            .any(|(a, _, o)| a == "POST /repos/acme/api/issues" && o == "ok")
    );
    assert!(
        crate::guard::audited(&t.core, "github")
            .await
            .iter()
            .any(|e| e.subject["action"] == "repo_profile" && e.payload["outcome"] == "done")
    );
}

#[tokio::test]
async fn profile_step_below_suggest_stores_the_profile_and_writes_nothing() {
    let t = setup().await;
    t.core
        .store()
        .set_setting("autonomy.max", "observe")
        .await
        .unwrap();
    let wf = published(&t.core, scout_flow()).await;
    let r = scout(&t, &wf, &["Dockerfile"]).await;
    assert_eq!(r.status, "succeeded", "{r:?}");
    assert!(t.gh.calls().iter().all(|c| c.method == "GET"));
    let d = profile_details(&steps(&t, &r.id).await);
    assert_eq!(d["profileIssue"]["skipped"], "autonomy", "{d}");
    assert_eq!(d["profile"]["signals"], json!(["dockerfile"]));
    assert!(
        crate::exec::repo_profile(&t.core, "acme/api")
            .await
            .is_some()
    );
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.subject["action"] == "github"
                && e.subject["target"]["action"] == "repo_profile")
    );
}
