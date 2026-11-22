//! Executor tests with a fake gateway. The engine is driven directly
//! (`advance`, `on_event`, `tick`) so every transition is deterministic.

use super::*;
use crate::ops::{self, NewWorkflow};
use kls_core::gateway::{JobOutcome, NodeGateway};
use serde_json::json;

#[derive(Default)]
struct Fake {
    online: Mutex<Vec<String>>,
    dispatched: Mutex<Vec<(String, DispatchSpec)>>,
    job: Mutex<Option<JobOutcome>>,
    fail_dispatch: Mutex<bool>,
}

#[async_trait::async_trait]
impl NodeGateway for Fake {
    async fn online(&self) -> Vec<String> {
        self.online.lock().unwrap().clone()
    }
    async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
        if *self.fail_dispatch.lock().unwrap() {
            anyhow::bail!("node refused");
        }
        let mut d = self.dispatched.lock().unwrap();
        d.push((node_id.to_string(), spec));
        Ok(format!("d{}", d.len()))
    }
    async fn job(
        &self,
        _node_id: Option<&str>,
        _kind: &str,
        _payload_json: &str,
        _needs: JobNeeds,
        _timeout_ms: i64,
    ) -> Result<JobOutcome> {
        Ok(self.job.lock().unwrap().clone().unwrap_or(JobOutcome {
            status: "ok".into(),
            output_json: "{}".into(),
            node_id: "node-a".into(),
            ..Default::default()
        }))
    }
}

struct T {
    core: Core,
    engine: Engine,
    gw: Arc<Fake>,
    _dir: tempfile::TempDir,
}

fn config() -> EngineConfig {
    EngineConfig {
        retry_backoff_ms: 0,
        no_host_retry_ms: 60_000,
        ..Default::default()
    }
}

async fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let gw = Arc::new(Fake::default());
    gw.online.lock().unwrap().push("node-a".into());
    core.set_gateway(gw.clone());
    let engine = Engine::new(core.clone(), Integrations::default(), config());
    T {
        core,
        engine,
        gw,
        _dir: dir,
    }
}

fn graph(v: Value) -> Graph {
    serde_json::from_value(v).unwrap()
}

async fn published(core: &Core, g: Graph) -> String {
    let w = ops::create_workflow(
        core,
        NewWorkflow {
            name: "Test flow".into(),
            graph: Some(g),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ops::publish(core, &w.id, 1, "first").await.unwrap();
    w.id
}

async fn start(t: &T, wf: &str) -> Run {
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.into(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    t.engine.advance(&r.id).await.unwrap();
    r
}

async fn run(t: &T, id: &str) -> Run {
    t.core.store().run(id).await.unwrap().unwrap()
}

async fn steps(t: &T, id: &str) -> Vec<RunStep> {
    t.core.store().run_steps(id).await.unwrap()
}

async fn wait_status(t: &T, id: &str, status: &str) -> Run {
    for _ in 0..200 {
        let r = run(t, id).await;
        if r.status == status {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("run {id} never reached {status}: {:?}", run(t, id).await);
}

fn simple(second: Value) -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            second,
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

#[tokio::test]
async fn manual_run_succeeds() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"notify","title":"Say","config":{"message":"{workflow} is {status}","severity":"info"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let r = run(&t, &r.id).await;
    assert_eq!(r.status, "succeeded");
    assert!(r.started_at.is_some() && r.finished_at.is_some());
    let st = steps(&t, &r.id).await;
    assert_eq!(st.len(), 2);
    assert!(st.iter().all(|s| s.status == "succeeded"));
    let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(d["message"], "Test flow is running");
    let kinds_seen: Vec<String> = t
        .core
        .store()
        .recent_events(100)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    for k in ["run.queued", "run.step", "run.notify", "run.finished"] {
        assert!(kinds_seen.iter().any(|x| x == k), "{k} missing");
    }
}

#[tokio::test]
async fn refuses_unpublished_and_disabled() {
    let t = setup().await;
    let w = ops::create_workflow(&t.core, NewWorkflow::default())
        .await
        .unwrap();
    let req = StartRun {
        workflow_id: w.id.clone(),
        ..Default::default()
    };
    assert!(ops::start_run(&t.core, req.clone()).await.is_err());
    ops::publish(&t.core, &w.id, 1, "").await.unwrap();
    ops::set_workflow_enabled(&t.core, &w.id, false)
        .await
        .unwrap();
    assert!(ops::start_run(&t.core, req.clone()).await.is_err());
    ops::set_workflow_enabled(&t.core, &w.id, true)
        .await
        .unwrap();
    // Idempotency.
    let keyed = StartRun {
        dedupe_key: Some("k".into()),
        ..req
    };
    let a = ops::start_run(&t.core, keyed.clone()).await.unwrap();
    let b = ops::start_run(&t.core, keyed).await.unwrap();
    assert!(matches!(a, StartOutcome::Started(_)));
    assert!(matches!(b, StartOutcome::Duplicate(ref r) if r.id == a.run().unwrap().id));
}

#[tokio::test]
async fn crew_dispatch_settles_on_session_end() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"dispatch":"worker","agent":"belai:builder","brief":"Do the thing"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    assert_eq!(st[1].status, "waiting");
    assert_eq!(st[1].dispatch_id.as_deref(), Some("d1"));
    let (node, spec) = t.gw.dispatched.lock().unwrap()[0].clone();
    assert_eq!(node, "node-a");
    assert_eq!(spec.agent, "belai:builder");
    assert_eq!(spec.run_id, r.id);
    assert!(spec.prompt.starts_with("Do the thing"));

    let ev = t
        .core
        .emit(
            "dispatch.updated",
            "d1",
            &json!({"nodeId":"node-a","status":"started","sessionId":"s1","error":""}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let st = steps(&t, &r.id).await;
    assert_eq!(st[1].status, "waiting");
    assert_eq!(st[1].session_id.as_deref(), Some("s1"));

    let ev = t
        .core
        .emit("session.ended", "s1", &json!({"reason":"done"}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}

#[tokio::test]
async fn cancel_stops_the_sessions_of_open_steps() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"dispatch":"worker","agent":"belai:builder","brief":"Do it"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    // The node starts the session for dispatch d1.
    t.core
        .session_upsert(
            "node-a",
            kls_store::sessions::Session {
                id: "s1".into(),
                state: "live".into(),
                run_id: r.id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ev = t
        .core
        .emit(
            "dispatch.updated",
            "d1",
            &json!({"nodeId":"node-a","status":"started","sessionId":"s1","error":""}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(steps(&t, &r.id).await[1].session_id.as_deref(), Some("s1"));

    ops::cancel_run(&t.core, &r.id, "enough").await.unwrap();
    let inbox = t.core.store().inbox_items("s1", 10).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].kind, "command");
    assert!(inbox[0].payload_json.contains("\"stop\""));
    assert_eq!(steps(&t, &r.id).await[1].status, "cancelled");
}

#[tokio::test]
async fn failed_dispatch_retries_then_stops_the_line() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"dispatch":"worker","agent":"belai:builder","maxAttempts":2}})),
    )
    .await;
    let r = start(&t, &wf).await;
    for d in ["d1", "d2"] {
        let ev = t
            .core
            .emit(
                "dispatch.updated",
                d,
                &json!({"status":"failed","error":"refused by the node"}),
            )
            .await
            .unwrap();
        t.engine.on_event(&ev).await.unwrap();
    }
    let r = run(&t, &r.id).await;
    assert_eq!(
        (r.status.as_str(), r.reason.as_str()),
        ("failed", "stopped_line:b")
    );
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 2);
    let st = steps(&t, &r.id).await;
    assert_eq!(st.iter().filter(|s| s.node_id == "b").count(), 2);
    assert!(
        st.iter()
            .filter(|s| s.node_id == "b")
            .all(|s| s.error == "dispatch_failed")
    );

    // Retry from the failed step creates attempt 3 and runs again.
    ops::retry_run(&t.core, &r.id, None).await.unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 3);
    assert_eq!(run(&t, &r.id).await.status, "running");
}

/// P3 (#7, #8, #26): a step's start fields reach the gateway and the audit
/// line; fields that do not fit the kind fail the step without a dispatch.
#[tokio::test]
async fn step_start_fields_reach_the_dispatch() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{
            "dispatch":"worker","agent":"belai:builder","stay":true,"stayIdleMaxS":900,
            "ask":false,"clone":{"owner":"acme","name":"api","branch":"main"},
            "mcp":[{"server":"github","key":"token","secret":"GH_MCP_TOKEN"}]}})),
    )
    .await;
    start(&t, &wf).await;
    let (_, spec) = t.gw.dispatched.lock().unwrap()[0].clone();
    assert!(spec.stay);
    assert_eq!(spec.stay_idle_max_s, 900);
    assert_eq!(spec.ask, Some(false));
    assert_eq!(spec.clone.unwrap().name, "api");
    assert_eq!(spec.mcp[0].secret, "GH_MCP_TOKEN");
    let sent = t
        .core
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .any(|e| e.payload.to_string().contains("github:token<-GH_MCP_TOKEN"));
    assert!(sent, "fields in the audit line");

    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Ask","config":{
            "dispatch":"session","brief":"Fix it","dirs":["/work/lib"],"effort":"high","gitSync":true,
            "workspace":"acme/api/fix"}})),
    )
    .await;
    start(&t, &wf).await;
    let (_, spec) = t.gw.dispatched.lock().unwrap()[1].clone();
    assert_eq!(spec.kind, "session");
    assert_eq!(spec.dirs, ["/work/lib"]);
    assert_eq!(spec.effort, "high");
    assert_eq!(spec.git_sync, Some(true));
    assert_eq!(spec.workspace, "acme/api/fix");

    // Stay on a session does not fit: refused, audited, nothing sent.
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Bad","config":{
            "dispatch":"session","brief":"Fix it","stay":true}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 2);
    let st = steps(&t, &r.id).await;
    assert!(st.iter().any(|s| s.error == "invalid_start_fields"));
}

#[tokio::test]
async fn gateway_error_fails_the_attempt() {
    let t = setup().await;
    *t.gw.fail_dispatch.lock().unwrap() = true;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let r = run(&t, &r.id).await;
    assert_eq!(
        (r.status.as_str(), r.reason.as_str()),
        ("failed", "dispatch_error:b")
    );
}

#[tokio::test]
async fn no_node_waits_then_unblocks() {
    let t = setup().await;
    t.gw.online.lock().unwrap().clear();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder","await":"none"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    assert_eq!(
        (st[1].status.as_str(), st[1].error.as_str()),
        ("waiting", "deferred:no_host")
    );
    let cps = ops::checkpoints(&t.core).await.unwrap();
    assert_eq!(cps.len(), 1);
    assert_eq!(cps[0].kind, ops::CheckpointKind::NoNode);

    t.gw.online.lock().unwrap().push("node-b".into());
    ops::unblock_step(&t.core, &st[1].id, true, "")
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    assert_eq!(t.gw.dispatched.lock().unwrap()[0].0, "node-b");
}

fn approval_graph() -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"h","type":"human","title":"Approve it","config":{"approvers":["owner"],"slaHours":1}},
            {"id":"n","type":"notify","title":"Done","config":{"message":"ok"}}
        ],
        "edges": [{"from":"t","to":"h"},{"from":"h","to":"n"}]
    }))
}

#[tokio::test]
async fn approval_approve_and_reject() {
    let t = setup().await;
    let wf = published(&t.core, approval_graph()).await;
    let r = start(&t, &wf).await;
    assert_eq!(run(&t, &r.id).await.status, "waiting_approval");
    let cps = ops::checkpoints(&t.core).await.unwrap();
    assert_eq!(cps[0].kind, ops::CheckpointKind::Approval);
    let a = cps[0].approval.clone().unwrap();
    assert!(a.sla_at.is_some());
    // A stale revision is refused.
    assert!(
        ops::decide_approval(&t.core, &a.id, true, "", Some(99))
            .await
            .is_err()
    );
    ops::decide_approval(&t.core, &a.id, true, "looks good", Some(1))
        .await
        .unwrap();
    assert!(
        ops::decide_approval(&t.core, &a.id, true, "", None)
            .await
            .is_err()
    );
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");

    let r2 = start(&t, &wf).await;
    let a2 = t
        .core
        .store()
        .approvals(Some("pending"), Some(&r2.id), 1)
        .await
        .unwrap()[0]
        .clone();
    ops::decide_approval(&t.core, &a2.id, false, "no", None)
        .await
        .unwrap();
    t.engine.advance(&r2.id).await.unwrap();
    let r2 = run(&t, &r2.id).await;
    assert_eq!(
        (r2.status.as_str(), r2.reason.as_str()),
        ("cancelled", "declined:h")
    );
}

#[tokio::test]
async fn approval_sla_renotifies_or_expires() {
    let t = setup().await;
    let wf = published(&t.core, approval_graph()).await;
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    let h = st.iter().find(|s| s.node_id == "h").unwrap();
    // Force the timer due.
    t.core
        .store()
        .update_step(
            &h.id,
            &[],
            &StepUpdate {
                wake_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    t.engine.tick(now_ms()).await.unwrap();
    let h2 = t.core.store().step(&h.id).await.unwrap().unwrap();
    assert_eq!(h2.status, "waiting");
    assert!(h2.wake_at.unwrap() > now_ms());

    // onTimeout end_run expires it.
    let mut g = approval_graph();
    g.node_mut("h")
        .unwrap()
        .config
        .insert("onTimeout".into(), json!("end_run"));
    let wf2 = published(&t.core, g).await;
    let r = start(&t, &wf2).await;
    let h = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "h")
        .unwrap();
    t.core
        .store()
        .update_step(
            &h.id,
            &[],
            &StepUpdate {
                wake_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    t.engine.tick(now_ms()).await.unwrap();
    let r = run(&t, &r.id).await;
    assert_eq!(
        (r.status.as_str(), r.reason.as_str()),
        ("failed", "expired:h")
    );
    assert_eq!(
        t.core
            .store()
            .approvals(None, Some(&r.id), 5)
            .await
            .unwrap()[0]
            .state,
        "expired"
    );
}

#[tokio::test]
async fn pause_resume_cancel() {
    let t = setup().await;
    let wf = published(&t.core, approval_graph()).await;
    let r = start(&t, &wf).await;
    let p = ops::pause_run(&t.core, &r.id).await.unwrap();
    assert_eq!(
        (p.status.as_str(), p.reason.as_str()),
        ("paused", "paused_from:waiting_approval")
    );
    assert!(ops::pause_run(&t.core, &r.id).await.is_err());
    assert_eq!(
        ops::checkpoints(&t.core)
            .await
            .unwrap()
            .iter()
            .filter(|c| c.kind == ops::CheckpointKind::Paused)
            .count(),
        1
    );
    // A paused run does not move.
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "paused");
    let back = ops::resume_run(&t.core, &r.id).await.unwrap();
    assert_eq!(back.status, "waiting_approval");
    assert!(ops::resume_run(&t.core, &r.id).await.is_err());
    let c = ops::cancel_run(&t.core, &r.id, "").await.unwrap();
    assert_eq!(c.status, "cancelled");
    assert!(ops::cancel_run(&t.core, &r.id, "").await.is_err());
    assert!(
        t.core
            .store()
            .approvals(Some("pending"), Some(&r.id), 5)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        steps(&t, &r.id)
            .await
            .iter()
            .all(|s| !walker::is_open(&s.status))
    );
    // Retry from cancelled re-opens the human step.
    ops::retry_run(&t.core, &r.id, None).await.unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "waiting_approval");
}

#[tokio::test]
async fn job_output_drives_a_condition() {
    let t = setup().await;
    *t.gw.job.lock().unwrap() = Some(JobOutcome {
        status: "ok".into(),
        output_json: r#"{"failed": 3}"#.into(),
        node_id: "node-a".into(),
        ..Default::default()
    });
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"j","type":"job","title":"Tests","config":{"job":"tests.run"}},
            {"id":"c","type":"condition","title":"Failing?","config":{
                "rules":[{"path":"steps.j.output.failed","op":"gt","value":0,"outcome":"red"}],
                "default":"green","outcomes":[{"value":"red"},{"value":"green","end":true}]}},
            {"id":"n","type":"notify","title":"Alarm","config":{"message":"red"}}
        ],
        "edges": [{"from":"t","to":"j"},{"from":"j","to":"c"},{"from":"c","to":"n","outcome":"red"}]
    }));
    let wf = published(&t.core, g).await;
    let r = start(&t, &wf).await;
    wait_status(&t, &r.id, "succeeded").await;
    let st = steps(&t, &r.id).await;
    let c = st.iter().find(|s| s.node_id == "c").unwrap();
    assert_eq!(c.route, "red");
    assert!(
        st.iter()
            .any(|s| s.node_id == "n" && s.status == "succeeded")
    );

    // A failing job fails the run.
    *t.gw.job.lock().unwrap() = Some(JobOutcome {
        status: "timeout".into(),
        ..Default::default()
    });
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "failed").await;
    assert_eq!(r.reason, "job_timeout:j");
}

#[tokio::test]
async fn manual_gate_fails_back_to_crew() {
    let t = setup().await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"a","type":"crew","title":"Build","config":{"agent":"belai:builder","await":"none"}},
            {"id":"g","type":"gate","title":"Check","config":{"check":"manual","onFail":"return_to_crew"}}
        ],
        "edges": [{"from":"t","to":"a"},{"from":"a","to":"g"},{"from":"g","to":"a","outcome":"fail","loop":true}]
    }));
    let wf = published(&t.core, g).await;
    let r = start(&t, &wf).await;
    let cps = ops::checkpoints(&t.core).await.unwrap();
    assert_eq!(cps[0].kind, ops::CheckpointKind::Gate);
    let step_id = cps[0].step_id.clone().unwrap();
    ops::unblock_step(&t.core, &step_id, false, "tests fail")
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(
        t.gw.dispatched.lock().unwrap().len(),
        2,
        "the crew ran again"
    );
    // Pass the second gate attempt.
    let errs = ops::bulk_decide(
        &t.core,
        &ops::checkpoints(&t.core)
            .await
            .unwrap()
            .iter()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>(),
        true,
        "",
    )
    .await;
    assert!(errs.is_empty(), "{errs:?}");
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}

#[tokio::test]
async fn missing_integrations_fail_clearly() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"github","title":"PR","config":{"action":"draft_pr"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert_eq!(run(&t, &r.id).await.reason, "integration_missing:b");
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"card","title":"Move","config":{"action":"move","status":"done"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert_eq!(run(&t, &r.id).await.reason, "no_card:b");
}

#[tokio::test]
async fn wait_for_event() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"wait","title":"Until review","config":{"event":"card.moved","match":{"data.detail.to":"review"}}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert_eq!(steps(&t, &r.id).await[1].status, "waiting");
    let other = t
        .core
        .emit(
            "card.moved",
            "c1",
            &json!({"detail":{"from":"todo","to":"doing"}}),
        )
        .await
        .unwrap();
    t.engine.on_event(&other).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "running");
    let hit = t
        .core
        .emit(
            "card.moved",
            "c1",
            &json!({"detail":{"from":"doing","to":"review"}}),
        )
        .await
        .unwrap();
    t.engine.on_event(&hit).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}

#[tokio::test]
async fn kanban_and_event_triggers() {
    let t = setup().await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Card ready","config":{"kind":"kanban","toList":"todo","overlap":"queue"}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }));
    let wf = published(&t.core, g).await;
    let ev = t
        .core
        .emit(
            "card.moved",
            "card-9",
            &json!({"detail":{"from":"backlog","to":"todo"}}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    // Same event again: deduped.
    t.engine.on_event(&ev).await.unwrap();
    let runs = t
        .core
        .store()
        .workflow_runs_in(&wf, &["queued", "running", "succeeded"], None)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].card_id, "card-9");
    assert_eq!(runs[0].trigger_kind, "kanban");
    let miss = t
        .core
        .emit(
            "card.moved",
            "card-9",
            &json!({"detail":{"from":"todo","to":"done"}}),
        )
        .await
        .unwrap();
    t.engine.on_event(&miss).await.unwrap();
    assert_eq!(
        t.core
            .store()
            .workflow_runs_in(&wf, &["queued", "running", "succeeded"], None)
            .await
            .unwrap()
            .len(),
        1
    );

    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Ended","config":{"kind":"event","event":"session.*","match":{"data.reason":"done"}}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }));
    let wf2 = published(&t.core, g).await;
    let ev = t
        .core
        .emit("session.ended", "s5", &json!({"reason":"done"}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let ev = t
        .core
        .emit("session.ended", "s6", &json!({"reason":"killed"}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let runs = t
        .core
        .store()
        .workflow_runs_in(&wf2, &["queued", "running", "succeeded"], None)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
}

fn cron_graph(policy: &str) -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Every hour","config":{"kind":"cron","cron":"0 * * * *","tz":"UTC","overlap":"queue","catchUp":policy}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"tick"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

async fn cron_runs(t: &T, wf: &str) -> Vec<Run> {
    t.core
        .store()
        .workflow_runs_in(wf, &["queued", "running", "succeeded"], None)
        .await
        .unwrap()
}

#[tokio::test]
async fn cron_fires_once_per_due_time() {
    let t = setup().await;
    let wf = published(&t.core, cron_graph("once")).await;
    let sch = t.core.store().schedules_for_workflow(&wf).await.unwrap()[0].clone();
    let now = now_ms();
    assert!(sch.next_run_at.unwrap() > now);
    // Due a moment ago: a normal fire.
    t.core
        .store()
        .set_schedule_next(&sch.id, Some(now - 1000))
        .await
        .unwrap();
    t.engine.tick(now).await.unwrap();
    t.engine.tick(now).await.unwrap();
    let runs = cron_runs(&t, &wf).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger_kind, "cron");
    assert_eq!(runs[0].status, "succeeded");
    let sch = t.core.store().schedule(&sch.id).await.unwrap().unwrap();
    assert!(sch.next_run_at.unwrap() > now);
    assert_eq!(sch.last_run_id.as_deref(), Some(runs[0].id.as_str()));

    // Disabled schedules do not fire.
    ops::set_schedule_enabled(&t.core, &sch.id, false)
        .await
        .unwrap();
    t.engine.tick(now + 10 * 3_600_000).await.unwrap();
    assert_eq!(cron_runs(&t, &wf).await.len(), 1);
}

#[tokio::test]
async fn cron_catch_up_policies() {
    for (policy, want) in [("skip", 0usize), ("once", 1), ("all", 5)] {
        let t = setup().await;
        let wf = published(&t.core, cron_graph(policy)).await;
        let sch = t.core.store().schedules_for_workflow(&wf).await.unwrap()[0].clone();
        let now = now_ms();
        let hour = 3_600_000;
        // The app was closed for about five hourly fires.
        let missed_from = (now / hour - 4) * hour;
        t.core
            .store()
            .set_schedule_next(&sch.id, Some(missed_from))
            .await
            .unwrap();
        t.engine.tick(now).await.unwrap();
        assert_eq!(cron_runs(&t, &wf).await.len(), want, "policy {policy}");
        let sch = t.core.store().schedule(&sch.id).await.unwrap().unwrap();
        assert!(sch.next_run_at.unwrap() > now);
    }
}

#[tokio::test]
async fn kill_switch_holds_work() {
    let t = setup().await;
    let wf = published(&t.core, cron_graph("once")).await;
    let sch = t.core.store().schedules_for_workflow(&wf).await.unwrap()[0].clone();
    let now = now_ms();
    t.core
        .store()
        .set_schedule_next(&sch.id, Some(now - 1000))
        .await
        .unwrap();
    t.core.set_factory_stopped(true, "test").await.unwrap();
    t.engine.tick(now).await.unwrap();
    assert!(cron_runs(&t, &wf).await.is_empty(), "held while stopped");
    assert!(
        ops::start_run(
            &t.core,
            StartRun {
                workflow_id: wf.clone(),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    t.core.set_factory_stopped(false, "").await.unwrap();
    let last = t
        .core
        .store()
        .recent_events(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    t.engine.on_event(&last).await.unwrap();
    assert_eq!(cron_runs(&t, &wf).await.len(), 1, "fired once it lifted");
}

#[tokio::test]
async fn concurrency_limit_queues_runs() {
    let t = setup().await;
    let mut g = approval_graph();
    g.limits.concurrency = Some(1);
    let wf = published(&t.core, g).await;
    let a = start(&t, &wf).await;
    let b = start(&t, &wf).await;
    assert_eq!(run(&t, &a.id).await.status, "waiting_approval");
    assert_eq!(run(&t, &b.id).await.status, "queued");
    ops::cancel_run(&t.core, &a.id, "").await.unwrap();
    // The executor wakes queued runs when a slot frees (here: by hand).
    t.engine.advance(&b.id).await.unwrap();
    assert_eq!(run(&t, &b.id).await.status, "waiting_approval");
}

#[tokio::test]
async fn engine_loop_runs_end_to_end() {
    let t = setup().await;
    let handle = start_with(t.core.clone(), Integrations::default(), config())
        .await
        .unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"notify","title":"Say","config":{"message":"hi"}})),
    )
    .await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    wait_status(&t, &out.run().unwrap().id, "succeeded").await;
    handle.stop().await;
}

#[test]
fn conditions_and_matching() {
    let ctx = json!({"a": {"b": [1, {"c": "x"}]}, "n": "5", "list": ["p", "q"]});
    assert_eq!(lookup(&ctx, "a.b.1.c"), Some(&json!("x")));
    assert!(lookup(&ctx, "a.z").is_none());
    assert!(rule_matches("eq", lookup(&ctx, "n"), &json!(5)));
    assert!(rule_matches("gt", lookup(&ctx, "n"), &json!(4)));
    assert!(!rule_matches("lt", lookup(&ctx, "n"), &json!(4)));
    assert!(rule_matches("contains", lookup(&ctx, "list"), &json!("q")));
    assert!(rule_matches(
        "in",
        lookup(&ctx, "a.b.1.c"),
        &json!(["x", "y"])
    ));
    assert!(rule_matches("missing", lookup(&ctx, "nope"), &Value::Null));
    assert!(rule_matches("ne", lookup(&ctx, "nope"), &json!(1)));
    let ev = Event {
        id: 1,
        at: 0,
        kind: "session.ended".into(),
        subject: "s1".into(),
        data_json: r#"{"reason":"done"}"#.into(),
    };
    assert!(event_matches("session.*", None, &ev));
    assert!(event_matches("*", None, &ev));
    assert!(!event_matches("card.*", None, &ev));
    assert!(event_matches(
        "session.ended",
        Some(&json!({"data.reason":"done","subject":"s1"})),
        &ev
    ));
    assert!(!event_matches(
        "session.ended",
        Some(&json!({"data.reason":"x"})),
        &ev
    ));
    assert_eq!(backoff(30_000, 2), 30_000);
    assert_eq!(backoff(30_000, 3), 60_000);
    assert_eq!(backoff(30_000, 20), 600_000);
}

mod engine_policy;
mod parity;
mod provision;
