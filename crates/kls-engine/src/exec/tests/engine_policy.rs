//! Engine policy (WP2): schedule blackouts and `held` runs, the own-identity
//! guard, budgets before every model call, `Dispatch.budget` / `tier`,
//! gates woken by pull request events, System One decision rows, the
//! unclaimed-card wait, `conflict_cards`, the run key, the `pr_head` base,
//! settings readers. Each new path is refused under the kill switch (and
//! the A2A blackout where agents talk to agents), with an audit entry.

use super::*;
use kls_core::control::keys::BLACKOUT;
use std::sync::atomic::{AtomicBool, Ordering};

fn cron_graph() -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Hourly","config":{"kind":"cron","cron":"0 * * * *","overlap":"queue"}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"tick"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

async fn all_runs(t: &T, wf: &str) -> Vec<Run> {
    t.core
        .store()
        .workflow_runs_in(
            wf,
            &[
                "held",
                "queued",
                "running",
                "waiting_approval",
                "succeeded",
                "failed",
                "cancelled",
            ],
            None,
        )
        .await
        .unwrap()
}

async fn due_now(t: &T, wf: &str) {
    let sch = t.core.store().schedules_for_workflow(wf).await.unwrap()[0].clone();
    t.core
        .store()
        .set_schedule_next(&sch.id, Some(now_ms() - 1000))
        .await
        .unwrap();
}

async fn set(t: &T, key: &str, v: Value) {
    let doc = t.core.settings_doc().await.unwrap();
    let mut ch = std::collections::BTreeMap::new();
    ch.insert(key.to_string(), v);
    t.core.settings_save(doc.revision, ch).await.unwrap();
}

#[tokio::test]
async fn cron_fire_in_a_blackout_is_held_survives_restart_and_is_audited() {
    let t = setup().await;
    let wf = published(&t.core, cron_graph()).await;
    t.core
        .store()
        .set_setting(BLACKOUT, "daily 00:00-24:00")
        .await
        .unwrap();
    due_now(&t, &wf).await;
    let mut rx = t.core.subscribe();
    t.engine.tick(now_ms()).await.unwrap();
    let runs = all_runs(&t, &wf).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "held");
    assert!(
        runs[0].reason.contains("schedule blackout"),
        "{}",
        runs[0].reason
    );
    let mut held_ev = false;
    while let Ok(e) = rx.try_recv() {
        held_ev |= e.kind == ops::RUN_HELD;
    }
    assert!(held_ev, "run.held is evented");
    assert!(
        crate::guard::audited(&t.core, "run")
            .await
            .iter()
            .any(|e| e.subject["action"] == "run.start" && e.msg.contains("held")),
        "the hold is audited"
    );
    // A restart (a new engine over the same store) keeps it held.
    let again = Engine::new(t.core.clone(), Integrations::default(), config());
    again.recover().await.unwrap();
    assert_eq!(run(&t, &runs[0].id).await.status, "held");
    // The kill switch keeps it held even when the window ends.
    t.core.store().set_setting(BLACKOUT, "").await.unwrap();
    t.core.set_factory_stopped(true, "drill").await.unwrap();
    assert!(ops::release_held(&t.core).await.unwrap().is_empty());
    assert_eq!(run(&t, &runs[0].id).await.status, "held");
    t.core.set_factory_stopped(false, "").await.unwrap();
    // The window ended: queued and run, the release audited.
    again.tick(now_ms()).await.unwrap();
    assert_eq!(run(&t, &runs[0].id).await.status, "succeeded");
    assert!(
        crate::guard::audited(&t.core, "run")
            .await
            .iter()
            .any(|e| e.subject["action"] == "run.release")
    );
}

#[tokio::test]
async fn event_trigger_in_a_blackout_is_held() {
    let t = setup().await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Any","config":{"kind":"event","event":"custom.thing","overlap":"queue"}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }));
    let wf = published(&t.core, g).await;
    t.core
        .store()
        .set_setting(BLACKOUT, "daily 00:00-24:00")
        .await
        .unwrap();
    let ev = t.core.emit("custom.thing", "x", &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let runs = all_runs(&t, &wf).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "held");
}

#[tokio::test]
async fn schedule_settings_fill_zone_and_catch_up() {
    let t = setup().await;
    set(&t, "schedules.missed", json!("skip")).await;
    set(&t, "schedules.tz", json!("Australia/Melbourne")).await;
    let wf = published(&t.core, cron_graph()).await;
    let sch = t.core.store().schedules_for_workflow(&wf).await.unwrap()[0].clone();
    assert!(
        sch.tz.is_empty(),
        "a trigger with no zone follows the setting"
    );
    // Missed fires with no catchUp of its own follow `schedules.missed`.
    let hour = 3_600_000;
    t.core
        .store()
        .set_schedule_next(&sch.id, Some((now_ms() / hour - 4) * hour))
        .await
        .unwrap();
    t.engine.tick(now_ms()).await.unwrap();
    assert!(
        all_runs(&t, &wf).await.is_empty(),
        "skip policy from settings"
    );
    assert_eq!(
        ops::schedule_zone("", "Australia/Melbourne"),
        "Australia/Melbourne"
    );
    assert_eq!(ops::schedule_zone("UTC", "Australia/Melbourne"), "UTC");
}

#[tokio::test]
async fn max_attempts_setting_stops_the_line() {
    let t = setup().await;
    set(&t, "autonomy.max_attempts", json!(1)).await;
    *t.gw.fail_dispatch.lock().unwrap() = true;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder","maxAttempts":3}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let r = run(&t, &r.id).await;
    assert_eq!(r.status, "failed");
    assert!(r.reason.starts_with("stopped_line"), "{}", r.reason);
}

fn webhook_wf() -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"PR","config":{"kind":"webhook","event":"pull_request","overlap":"queue"}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

async fn pull(t: &T, n: i64, extra: Value) -> Event {
    let mut d = json!({"repo":"acme/api","number":n,"base":"main","head":"feature/x","author":"ana",
        "draft":false,"fork":false,"labels":[],"labelsAdded":[]});
    for (k, v) in extra.as_object().unwrap() {
        d[k] = v.clone();
    }
    t.core
        .emit("github.pull.opened", &format!("acme/api#{n}"), &d)
        .await
        .unwrap()
}

#[tokio::test]
async fn factory_authored_webhook_events_never_trigger() {
    let t = setup().await;
    let wf = published(&t.core, webhook_wf()).await;
    t.core
        .store()
        .set_setting(FACTORY_IDENTITY, "sf-robot")
        .await
        .unwrap();
    for (n, extra) in [
        (1, json!({"fromFactory": true})),
        (
            2,
            json!({"body": "Fix <!-- keleustes:review run=x node=y -->"}),
        ),
        (3, json!({"author": "sf-robot"})),
        (4, json!({"author": "keleustes[bot]"})),
    ] {
        let ev = pull(&t, n, extra).await;
        t.engine.on_event(&ev).await.unwrap();
    }
    assert!(all_runs(&t, &wf).await.is_empty());
    let sch = t.core.store().schedules_for_workflow(&wf).await.unwrap()[0].clone();
    let fires = t.core.store().trigger_fires(&sch.id, 0).await.unwrap();
    assert_eq!(fires.len(), 4);
    assert!(fires.iter().all(|f| f.outcome == "skipped:own_identity"));
    assert_eq!(crate::guard::audited(&t.core, "trigger").await.len(), 4);
    // A person's event still triggers.
    let ev = pull(&t, 5, json!({})).await;
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(all_runs(&t, &wf).await.len(), 1);
}

async fn hard_budget(t: &T) {
    kls_core::vault::save_budget(
        &t.core,
        &kls_store::vault::Budget {
            name: "tiny".into(),
            applies_to: "all".into(),
            provider: "all".into(),
            period: "total".into(),
            limit_tokens: Some(100),
            action_at_limit: "hard_stop".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn start_job_over_budget_is_refused() {
    let t = setup().await;
    hard_budget(&t).await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"job","title":"Ask","config":{"job":"model"},"budget":{"tokens":1000}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "failed").await;
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "b" && s.error == "budget_exceeded")
    );
}

#[tokio::test]
async fn decision_over_budget_is_refused() {
    let t = setup().await;
    hard_budget(&t).await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"d","type":"decision","title":"Decide","budget":{"tokens":1000},
             "config":{"question":"Ship?","outcomes":[{"value":"yes"},{"value":"no"}]}},
            {"id":"y","type":"notify","title":"Yes","config":{"message":"y"}}
        ],
        "edges": [{"from":"t","to":"d"},{"from":"d","to":"y","outcome":"yes"},{"from":"d","to":"y","outcome":"no"}]
    }));
    let wf = published(&t.core, g).await;
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "failed").await;
    assert!(
        steps(&t, &r.id)
            .await
            .iter()
            .any(|s| s.node_id == "d" && s.error == "budget_exceeded")
    );
}

#[tokio::test]
async fn escalate_budget_files_an_approval() {
    let t = setup().await;
    kls_core::vault::save_budget(
        &t.core,
        &kls_store::vault::Budget {
            name: "ask".into(),
            applies_to: "all".into(),
            provider: "all".into(),
            period: "total".into(),
            limit_tokens: Some(100),
            action_at_limit: "escalate_human".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"},"budget":{"tokens":1000}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "waiting_approval").await;
    let ap = t
        .core
        .store()
        .approvals(Some("pending"), Some(&r.id), 5)
        .await
        .unwrap();
    assert_eq!((ap.len(), ap[0].kind.as_str()), (1, "budget"));
    assert!(
        ap[0].title.starts_with("Budget ask at ") && ap[0].title.ends_with(": let Build go ahead?"),
        "{}",
        ap[0].title
    );
    assert!(t.gw.dispatched.lock().unwrap().is_empty());
    // Approved: the step goes past the gate and dispatches.
    ops::decide_approval(&t.core, &ap[0].id, true, "go", None)
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dispatch_carries_budget_tier_and_member_models() {
    let t = setup().await;
    set(&t, "agents.max_cost_usd_per_card", json!(7)).await;
    set(
        &t,
        "models.agents",
        json!("belai:planner=anthropic/claude-x"),
    )
    .await;
    let mut g = simple(json!({"id":"b","type":"crew","title":"Build",
        "config":{"dispatch":"crew","crew":"belai:feature","memberModels":{"belai:builder":"openai/gpt-y"}},
        "budget":{"tokens":5000}}));
    g.limits.max_passes = Some(4);
    let wf = published(&t.core, g).await;
    let r = start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap();
    assert_eq!(d.len(), 1, "run {}", r.id);
    let spec = &d[0].1;
    let b = spec.budget.as_ref().expect("budget on the dispatch");
    assert_eq!(b.max_cost_usd, 7.0);
    assert_eq!(b.max_passes, 4, "the workflow limit wins");
    assert_eq!(b.max_tokens, 5000);
    assert_eq!(spec.tier, "", "no downgrade");
    assert_eq!(
        spec.member_models.get("belai:builder").map(String::as_str),
        Some("openai/gpt-y")
    );
}

#[tokio::test]
async fn crew_member_model_is_the_per_member_fallback() {
    let t = setup().await;
    set(&t, "models.agents", json!("builder=acme/by-agent")).await;
    let member = |agent: &str, model: &str| kls_store::vault::CrewMember {
        agent: agent.into(),
        role: "r".into(),
        count: 1,
        model: model.into(),
    };
    t.core
        .store()
        .save_crew(&kls_store::vault::Crew {
            id: "c-sec".into(),
            name: "sec".into(),
            process: "sequential".into(),
            members: vec![
                member("patcher", "acme/member"),
                member("planner", "acme/member-2"),
                member("builder", ""),
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    let g = simple(json!({"id":"b","type":"crew","title":"Fix",
        "config":{"dispatch":"crew","crew":"sec","memberModels":{"planner":"acme/step"}}}));
    let wf = published(&t.core, g).await;
    let r = start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap();
    assert_eq!(d.len(), 1, "run {}", r.id);
    let mm = &d[0].1.member_models;
    assert_eq!(
        mm.get("planner").map(String::as_str),
        Some("acme/step"),
        "the step wins"
    );
    assert_eq!(
        mm.get("patcher").map(String::as_str),
        Some("acme/member"),
        "then the member"
    );
    assert_eq!(
        mm.get("builder").map(String::as_str),
        Some("acme/by-agent"),
        "then models.agents"
    );
}

#[tokio::test]
async fn downgrade_sets_the_fast_tier() {
    let t = setup().await;
    set(&t, "models.fast", json!("anthropic/fast-1")).await;
    kls_core::vault::save_budget(
        &t.core,
        &kls_store::vault::Budget {
            name: "soft".into(),
            applies_to: "all".into(),
            provider: "all".into(),
            period: "total".into(),
            limit_tokens: Some(100),
            action_at_limit: "downgrade_to_fast".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build",
            "config":{"agent":"belai:builder","model":"anthropic/big-1"},"budget":{"tokens":1000}})),
    )
    .await;
    start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].1.tier.as_str(), d[0].1.model.as_str()),
        ("fast", "fast-1")
    );
}

#[tokio::test]
async fn crew_dispatch_waits_under_the_a2a_blackout() {
    let t = setup().await;
    kls_core::control::set_a2a_blackout(&t.core, true, "drill", "owner")
        .await
        .unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"dispatch":"crew","crew":"belai:feature"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert!(t.gw.dispatched.lock().unwrap().is_empty());
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "b" && s.error == "deferred:a2a")
    );
    t.core.audit_sync().await.unwrap();
    assert!(!crate::guard::audited(&t.core, "a2a").await.is_empty());
    // A worker step is not agent-to-agent: it still dispatches.
    let wf2 = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    start(&t, &wf2).await;
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn job_and_decision_wait_under_the_kill_switch() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"job","title":"Ask","config":{"job":"model"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    wait_status(&t, &r.id, "succeeded").await;
    // A maintenance window holds jobs like the switch does.
    t.core
        .store()
        .set_setting(kls_core::control::keys::MAINTENANCE, "daily 00:00-24:00")
        .await
        .unwrap();
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "b" && s.error == "deferred:kill_switch")
    );
}

struct FlipForge {
    pass: AtomicBool,
    asked: Mutex<usize>,
}

#[async_trait::async_trait]
impl Forge for FlipForge {
    async fn action(&self, _ctx: &StepContext, action: &str) -> Result<Value> {
        Ok(json!({ "action": action }))
    }
    async fn gate(&self, _ctx: &StepContext) -> Result<GateResult> {
        *self.asked.lock().unwrap() += 1;
        Ok(if self.pass.load(Ordering::SeqCst) {
            GateResult::Pass(json!({}))
        } else {
            GateResult::Pending
        })
    }
}

#[tokio::test]
async fn forge_gate_wakes_on_a_pull_update() {
    let t = setup().await;
    let forge = Arc::new(FlipForge {
        pass: AtomicBool::new(false),
        asked: Mutex::new(0),
    });
    let engine = Engine::new(
        t.core.clone(),
        Integrations {
            forge: Some(forge.clone()),
            board: None,
        },
        config(),
    );
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"gate","title":"Checks","config":{"check":"forge","onFail":"end_run"}})),
    )
    .await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            event: json!({"data": {"number": 7}}),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    engine.advance(&r.id).await.unwrap();
    assert_eq!(*forge.asked.lock().unwrap(), 1);
    let st = steps(&t, &r.id).await;
    assert!(st.iter().any(|s| s.error == "deferred:checks"));
    // Another pull's update does not wake it.
    forge.pass.store(true, Ordering::SeqCst);
    let other = t
        .core
        .emit(
            "github.pull.updated",
            "acme/api#8",
            &json!({"repo":"acme/api","number":8}),
        )
        .await
        .unwrap();
    engine.on_event(&other).await.unwrap();
    assert_eq!(*forge.asked.lock().unwrap(), 1);
    // Its own pull's update does, long before the 5-minute recheck.
    let ev = t
        .core
        .emit(
            "github.pull.updated",
            "acme/api#7",
            &json!({"repo":"acme/api","number":7}),
        )
        .await
        .unwrap();
    engine.on_event(&ev).await.unwrap();
    assert_eq!(*forge.asked.lock().unwrap(), 2);
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}

#[tokio::test]
async fn system_one_decision_row_is_written_and_capped() {
    let t = setup().await;
    *t.gw.job.lock().unwrap() = Some(systemone("no", 0.9));
    let wf = published(&t.core, decision_graph(json!({}))).await;
    let r = start(&t, &wf).await;
    wait_status(&t, &r.id, "succeeded").await;
    let rows = t
        .core
        .store()
        .engine_decisions(Some(&r.id), 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].outcome.as_str(),
            rows[0].why.as_str(),
            rows[0].model.as_str()
        ),
        ("no", "chosen", "m1")
    );
    assert!((rows[0].confidence - 0.9).abs() < 1e-9);
    // The day's stats feed the Decisions report.
    let day = kls_store::github::day_start(now_ms());
    assert_eq!(
        t.core
            .store()
            .decision_days(day, day + 86_400_000)
            .await
            .unwrap()
            .iter()
            .map(|d| d.total)
            .sum::<i64>(),
        1
    );
    // Past the daily cap a person decides, and the refusal is audited.
    t.core
        .store()
        .set_setting(crate::exec::policy::DECISION_CAP, "1")
        .await
        .unwrap();
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "waiting_approval").await;
    let rows = t
        .core
        .store()
        .engine_decisions(Some(&r.id), 10)
        .await
        .unwrap();
    assert_eq!(rows[0].why, "daily_cap");
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.msg.contains("daily_cap"))
    );
}

#[tokio::test]
async fn unclaimed_card_waits_then_fails_at_the_timeout() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let b = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "b")
        .unwrap();
    let wake = |at: i64, details: Option<String>| StepUpdate {
        wake_at: Some(Some(at)),
        details_json: details,
        ..Default::default()
    };
    t.core
        .store()
        .update_step(&b.id, &["waiting"], &wake(1, None))
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    let b2 = t.core.store().step(&b.id).await.unwrap().unwrap();
    assert_eq!(
        (b2.status.as_str(), b2.error.as_str()),
        ("waiting", "waiting:unclaimed")
    );
    assert!(
        crate::guard::audited(&t.core, "card")
            .await
            .iter()
            .any(|e| e.subject["action"] == "route.unclaimed")
    );
    // Past the step's timeout it fails.
    let mut d: Value = serde_json::from_str(&b2.details_json).unwrap();
    d["timeoutAt"] = json!(2);
    t.core
        .store()
        .update_step(&b.id, &["waiting"], &wake(1, Some(d.to_string())))
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert!(
        steps(&t, &r.id)
            .await
            .iter()
            .any(|s| s.node_id == "b" && s.error == "card_unclaimed")
    );
}

async fn mirror_pull(t: &T, n: i64, mergeable: &str) {
    t.core
        .store()
        .gh_upsert_pull(&kls_store::github::GhPull {
            repo: "acme/api".into(),
            number: n,
            title: format!("PR {n}"),
            state: "open".into(),
            head_ref: format!("feature/{n}"),
            head_sha: "abc".into(),
            head_repo: "acme/api".into(),
            base_ref: "main".into(),
            mergeable: mergeable.into(),
            html_url: format!("https://github.com/acme/api/pull/{n}"),
            ..Default::default()
        })
        .await
        .unwrap();
}

fn conflict_wf() -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"b","type":"github","title":"Conflict cards","config":{"action":"conflict_cards"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

async fn start_on_repo(t: &T, wf: &str) -> Run {
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
    t.engine.advance(&r.id).await.unwrap();
    r
}

#[tokio::test]
async fn conflict_cards_output_files_cards_and_rechecks_unknown() {
    let t = setup().await;
    set(&t, "autonomy.default", json!("draft_pr")).await;
    mirror_pull(&t, 1, "conflict").await;
    mirror_pull(&t, 2, "unknown").await;
    mirror_pull(&t, 3, "clean").await;
    let wf = published(&t.core, conflict_wf()).await;
    let r = start_on_repo(&t, &wf).await;
    let b = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "b")
        .unwrap();
    assert_eq!(
        (b.status.as_str(), b.error.as_str()),
        ("waiting", "deferred:recheck")
    );
    let cards = t.core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    assert!(cards[0].title.contains("acme/api#1"));
    // GitHub worked #2 out: the recheck files its card and succeeds.
    mirror_pull(&t, 2, "conflict").await;
    t.core
        .store()
        .update_step(
            &b.id,
            &["waiting"],
            &StepUpdate {
                wake_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    assert_eq!(t.core.cards().await.unwrap().len(), 2);
    assert_eq!(crate::conflicts::RECHECK_MINUTES, [0, 1, 3, 10]);

    // Under the kill switch the step waits, refused and audited.
    t.core.set_factory_stopped(true, "drill").await.unwrap();
    let st = RunStep {
        id: new_id(),
        run_id: r.id.clone(),
        node_id: "b".into(),
        status: "running".into(),
        ..Default::default()
    };
    t.core.store().insert_step(&st).await.unwrap();
    t.engine
        .conflict_cards(&r, &conflict_wf().nodes[1], &st, json!({}))
        .await
        .unwrap();
    let st = t.core.store().step(&st.id).await.unwrap().unwrap();
    assert_eq!(st.error, "deferred:kill_switch");
    t.core.audit_sync().await.unwrap();
    assert!(
        !crate::guard::audited(&t.core, "kill_switch")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn pr_head_base_resolves_to_the_runs_branch() {
    let r = Run {
        event_json: json!({"data": {"head": "feature/x"}}).to_string(),
        ..Default::default()
    };
    assert_eq!(pr_head_of(&r), "feature/x");
    let r = Run {
        event_json: json!({"data": {"headBranch": "main"}}).to_string(),
        ..Default::default()
    };
    assert_eq!(pr_head_of(&r), "main");
    assert_eq!(pr_head_of(&Run::default()), "");
}

#[derive(Default)]
struct BaseForge {
    bases: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Forge for BaseForge {
    async fn action(&self, ctx: &StepContext, _action: &str) -> Result<Value> {
        self.bases
            .lock()
            .unwrap()
            .push(ctx.node.str("base").to_string());
        Ok(json!({}))
    }
    async fn gate(&self, _ctx: &StepContext) -> Result<GateResult> {
        Ok(GateResult::Pass(json!({})))
    }
}

#[tokio::test]
async fn draft_pr_on_pr_head_gets_the_branch_not_the_word() {
    let t = setup().await;
    set(&t, "autonomy.default", json!("draft_pr")).await;
    let forge = Arc::new(BaseForge::default());
    let engine = Engine::new(
        t.core.clone(),
        Integrations {
            forge: Some(forge.clone()),
            board: None,
        },
        config(),
    );
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"github","title":"PR","config":{"action":"draft_pr","base":"pr_head"}})),
    )
    .await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            event: json!({"data": {"head": "feature/failing"}}),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    engine.advance(&out.run().unwrap().id).await.unwrap();
    assert_eq!(*forge.bases.lock().unwrap(), ["feature/failing"]);
}

#[tokio::test]
async fn run_key_dedupes_a_default_per_subject_and_commit() {
    let f = crate::triggers::Facts {
        subject: "acme/api#7".into(),
        head_sha: "abc".into(),
        ..Default::default()
    };
    assert_eq!(
        crate::triggers::run_key("sys:pr-review:acme/api", &f).as_deref(),
        Some("sys:pr-review:acme/api:acme/api#7:abc")
    );
    assert!(crate::triggers::run_key("", &f).is_none());
}

#[tokio::test]
async fn sys_slugs_are_reserved_for_defaults() {
    let t = setup().await;
    let w = ops::create_workflow(
        &t.core,
        NewWorkflow {
            name: "sys pr review".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(w.slug.starts_with("wf-sys-"), "{}", w.slug);
    assert!(
        ops::create_workflow(
            &t.core,
            NewWorkflow {
                name: "x".into(),
                slug: Some("sys-mine".into()),
                ..Default::default()
            },
        )
        .await
        .is_err()
    );
    let d = ops::create_workflow(
        &t.core,
        NewWorkflow {
            name: "x".into(),
            slug: Some("sys-ci-fix-acme-api".into()),
            template_id: "default:ci-fix".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(d.slug, "sys-ci-fix-acme-api");
}

#[tokio::test]
async fn prerequisites_block_a_provider_no_node_offers() {
    let t = setup().await;
    let g = simple(
        json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder","model":"mistral/m"}}),
    );
    // No node says what it holds: not judged.
    assert!(ops::prerequisites(&t.core, &g).await.unwrap().is_empty());
    let mut host = kls_store::HostInfo {
        hostname: "a".into(),
        ..Default::default()
    };
    host.capabilities.providers = vec!["anthropic".into()];
    t.core
        .store()
        .insert_node("node-a", "a", &host, "serial", "tok")
        .await
        .unwrap();
    let missing = ops::prerequisites(&t.core, &g).await.unwrap();
    assert_eq!(missing.len(), 1, "{missing:?}");
    let wf = published(&t.core, g).await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    match out {
        StartOutcome::Skipped(why) => assert!(why.starts_with("prerequisite_missing"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.msg.contains("prerequisite_missing"))
    );
}

fn decision_graph(cfg: Value) -> Graph {
    let mut c = json!({"question":"Is it a security fix?","outcomes":[{"value":"yes"},{"value":"no"}],"threshold":0.7});
    for (k, v) in cfg.as_object().unwrap() {
        c[k] = v.clone();
    }
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"d","type":"decision","title":"Decide","config": c},
            {"id":"y","type":"notify","title":"Yes","config":{"message":"y"}},
            {"id":"n","type":"notify","title":"No","config":{"message":"n"}}
        ],
        "edges": [
            {"from":"t","to":"d"},
            {"from":"d","to":"y","outcome":"yes"},
            {"from":"d","to":"n","outcome":"no"}
        ]
    }))
}

fn systemone(choice: &str, p: f64) -> JobOutcome {
    JobOutcome {
        status: "ok".into(),
        output_json: json!({"answers":{"decide":{"type":"choice","choice":choice,"probabilities":{choice:p}}},
            "calibrated":false,"provider":"anthropic","model":"m1"})
        .to_string(),
        node_id: "node-a".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_step_with_no_model_takes_the_per_agent_model_map() {
    let t = setup().await;
    set(
        &t,
        "models.agents",
        json!("belai:builder=anthropic/claude-z"),
    )
    .await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap();
    assert_eq!(
        (d[0].1.provider.as_str(), d[0].1.model.as_str()),
        ("anthropic", "claude-z")
    );
}

async fn register(t: &T, f: impl FnOnce(&mut kls_store::HostInfo)) {
    let mut host = kls_store::HostInfo {
        hostname: "a".into(),
        ..Default::default()
    };
    f(&mut host);
    t.core
        .store()
        .insert_node("node-a", "a", &host, "serial", "tok")
        .await
        .unwrap();
}

#[tokio::test]
async fn the_fast_tier_comes_from_a_provider_the_node_offers() {
    let t = setup().await;
    register(&t, |h| {
        h.capabilities.providers = vec!["openai".into()];
        h.capabilities.models = vec![kls_store::ProviderModels {
            provider: "openai".into(),
            models: vec!["gpt-fast".into()],
        }];
    })
    .await;
    set(&t, "models.fast", json!("anthropic/fast-1")).await;
    kls_core::vault::save_budget(
        &t.core,
        &kls_store::vault::Budget {
            name: "soft".into(),
            applies_to: "all".into(),
            provider: "all".into(),
            period: "total".into(),
            limit_tokens: Some(100),
            action_at_limit: "downgrade_to_fast".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build",
            "config":{"agent":"belai:builder","model":"openai/big"},"budget":{"tokens":1000}})),
    )
    .await;
    start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(
        (d[0].1.tier.as_str(), d[0].1.provider.as_str()),
        ("fast", "openai")
    );
}

#[tokio::test]
async fn no_host_for_repo_is_refused() {
    let t = setup().await;
    register(&t, |h| {
        h.agents = vec!["belai:builder".into()];
        h.trusted_dirs = vec!["/src/other".into()];
    })
    .await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start_on_repo(&t, &wf).await;
    assert!(t.gw.dispatched.lock().unwrap().is_empty());
    assert!(
        steps(&t, &r.id)
            .await
            .iter()
            .any(|s| s.error == crate::route::NO_HOST_FOR_REPO)
    );
    assert_eq!(run(&t, &r.id).await.status, "failed");
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.msg.contains(crate::route::NO_HOST_FOR_REPO))
    );
}

#[tokio::test]
async fn a_gated_default_notes_trigger_skipped_hourly() {
    let t = setup().await;
    let g = crate::defaults::graph("pr-review", "acme/api").unwrap();
    let w = ops::create_workflow(
        &t.core,
        NewWorkflow {
            name: "pr review".into(),
            template_id: "default:pr-review".into(),
            graph: Some(g),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ops::publish(&t.core, &w.id, 1, "seed").await.unwrap();
    t.core
        .store()
        .set_workflow_system(&w.id, "system", "sys:pr-review:acme/api", 1, false)
        .await
        .unwrap();
    ops::set_workflow_enabled(&t.core, &w.id, false)
        .await
        .unwrap();
    t.core
        .store()
        .set_setting(&crate::defaults::enabled_key("pr-review"), "false")
        .await
        .unwrap();
    let mut rx = t.core.subscribe();
    for n in [1, 2] {
        let ev = pull(&t, n, json!({})).await;
        t.engine.on_event(&ev).await.unwrap();
    }
    let mut skipped = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if e.kind == ops::TRIGGER_SKIPPED {
            skipped.push(e);
        }
    }
    assert_eq!(skipped.len(), 1, "once an hour per workflow");
    let d: Value = serde_json::from_str(&skipped[0].data_json).unwrap();
    assert_eq!(d["reason"], "default_disabled");
    assert!(
        crate::guard::audited(&t.core, "trigger")
            .await
            .iter()
            .any(|e| e.subject["action"] == "trigger.skipped")
    );
}

struct ReadyForge {
    ready: AtomicBool,
    calls: Mutex<usize>,
}

#[async_trait::async_trait]
impl Forge for ReadyForge {
    async fn action(&self, _ctx: &StepContext, _action: &str) -> Result<Value> {
        *self.calls.lock().unwrap() += 1;
        Ok(if self.ready.load(Ordering::SeqCst) {
            json!({"approved": true})
        } else {
            json!({"approved": false, "reason": "checks_pending", "terminal": false})
        })
    }
    async fn gate(&self, _ctx: &StepContext) -> Result<GateResult> {
        Ok(GateResult::Pass(json!({})))
    }
}

#[tokio::test]
async fn approve_ready_waits_for_readiness_and_wakes_on_a_review() {
    let t = setup().await;
    set(&t, "autonomy.max", json!("auto_merge_low_risk")).await;
    set(&t, "autonomy.default", json!("auto_merge_low_risk")).await;
    let forge = Arc::new(ReadyForge {
        ready: AtomicBool::new(false),
        calls: Mutex::new(0),
    });
    let engine = Engine::new(
        t.core.clone(),
        Integrations {
            forge: Some(forge.clone()),
            board: None,
        },
        config(),
    );
    let wf = published(
        &t.core,
        simple(
            json!({"id":"b","type":"github","title":"Approve","config":{"action":"approve_ready"}}),
        ),
    )
    .await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            event: json!({"data": {"number": 9}}),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    engine.advance(&r.id).await.unwrap();
    let b = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "b")
        .unwrap();
    assert_eq!(
        (b.status.as_str(), b.error.as_str()),
        ("waiting", "deferred:readiness:checks_pending")
    );
    forge.ready.store(true, Ordering::SeqCst);
    let ev = t
        .core
        .emit(
            "github.review.submitted",
            "acme/api#9",
            &json!({"repo":"acme/api","number":9}),
        )
        .await
        .unwrap();
    engine.on_event(&ev).await.unwrap();
    assert_eq!(*forge.calls.lock().unwrap(), 2);
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}
