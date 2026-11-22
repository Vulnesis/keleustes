//! Engine parity: trigger conditions and safeguards, the kill switch on
//! every path, routing cards, the SystemOne decision node and autonomy
//! gating, each with its audit entry.

use super::*;

fn webhook_graph(conditions: Value, extra: Value) -> Graph {
    let mut cfg =
        json!({"kind":"webhook","event":"pull_request","overlap":"queue","conditions": conditions});
    for (k, v) in extra.as_object().unwrap() {
        cfg[k] = v.clone();
    }
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"PR","config": cfg},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

async fn pull_event(t: &T, number: i64, data: Value) -> Event {
    let mut d = json!({"repo":"acme/api","number":number,"base":"main","head":"feature/x",
        "author":"ana","draft":false,"fork":false,"labels":[],"labelsAdded":[]});
    for (k, v) in data.as_object().unwrap() {
        d[k] = v.clone();
    }
    t.core
        .emit("github.pull.opened", &format!("acme/api#{number}"), &d)
        .await
        .unwrap()
}

async fn wf_runs(t: &T, wf: &str) -> Vec<Run> {
    t.core
        .store()
        .workflow_runs_in(
            wf,
            &[
                "queued",
                "running",
                "waiting_approval",
                "succeeded",
                "failed",
            ],
            None,
        )
        .await
        .unwrap()
}

async fn fires(t: &T, wf: &str) -> Vec<String> {
    let sch = t.core.store().schedules_for_workflow(wf).await.unwrap();
    let mut out = Vec::new();
    for s in sch {
        for f in t.core.store().trigger_fires(&s.id, 0).await.unwrap() {
            out.push(f.outcome);
        }
    }
    out
}

#[tokio::test]
async fn webhook_conditions_and_safeguards() {
    let t = setup().await;
    let wf = published(
        &t.core,
        webhook_graph(
            json!({"baseBranches":["main"],"drafts":"exclude"}),
            json!({"debounceSeconds": 600}),
        ),
    )
    .await;

    let ev = pull_event(&t, 1, json!({})).await;
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(wf_runs(&t, &wf).await.len(), 1);
    let r = &wf_runs(&t, &wf).await[0];
    let e: Value = serde_json::from_str(&r.event_json).unwrap();
    assert_eq!(
        e["subject"], "acme/api#1",
        "provenance: the event rides on the run"
    );

    // Same subject inside the debounce window: coalesced.
    let ev = pull_event(&t, 1, json!({})).await;
    t.engine.on_event(&ev).await.unwrap();
    // A condition that does not match is not a fire.
    let ev = pull_event(&t, 2, json!({"base":"dev"})).await;
    t.engine.on_event(&ev).await.unwrap();
    let ev = pull_event(&t, 3, json!({"draft":true})).await;
    t.engine.on_event(&ev).await.unwrap();
    // The factory's own branch never retriggers it.
    let ev = pull_event(&t, 4, json!({"head":"belai/K-7f21ab"})).await;
    t.engine.on_event(&ev).await.unwrap();
    // A fork needs a workflow with a person in it.
    let ev = pull_event(&t, 5, json!({"fork":true})).await;
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(wf_runs(&t, &wf).await.len(), 1);
    let mut f = fires(&t, &wf).await;
    f.sort();
    assert_eq!(
        f,
        [
            "skipped:debounced",
            "skipped:fork_needs_human",
            "skipped:own_branch",
            "started"
        ]
    );
    // Every fire is audited.
    assert_eq!(crate::guard::audited(&t.core, "trigger").await.len(), 4);
    assert!(!crate::guard::audited(&t.core, "run").await.is_empty());
}

#[tokio::test]
async fn runs_per_hour_cap_and_fanout_priority() {
    let t = setup().await;
    let wf = published(
        &t.core,
        webhook_graph(json!({}), json!({"maxRunsPerHour": 1})),
    )
    .await;
    for n in 1..=3 {
        let ev = pull_event(&t, n, json!({})).await;
        t.engine.on_event(&ev).await.unwrap();
    }
    assert_eq!(wf_runs(&t, &wf).await.len(), 1);
    assert_eq!(
        fires(&t, &wf)
            .await
            .iter()
            .filter(|f| *f == "skipped:run_cap")
            .count(),
        2
    );
}

#[tokio::test]
async fn kill_switch_refuses_triggers_runs_and_cron_with_audit() {
    let t = setup().await;
    let wf = published(&t.core, webhook_graph(json!({}), json!({}))).await;
    t.core.set_factory_stopped(true, "drill").await.unwrap();
    let mut rx = t.core.subscribe();
    let ev = pull_event(&t, 1, json!({})).await;
    t.engine.on_event(&ev).await.unwrap();
    assert!(wf_runs(&t, &wf).await.is_empty());
    assert_eq!(fires(&t, &wf).await, ["skipped:kill_switch:stop: drill"]);
    let mut refused = false;
    while let Ok(e) = rx.try_recv() {
        refused |= e.kind == kls_core::control::kinds::FACTORY_REFUSED;
    }
    assert!(refused);
    t.core.audit_sync().await.unwrap();
    assert!(
        !crate::guard::audited(&t.core, "kill_switch")
            .await
            .is_empty()
    );
    // A manual start is refused and audited too.
    let before = crate::guard::audited(&t.core, "kill_switch").await.len();
    let r = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.clone(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(r.is_err());
    t.core.audit_sync().await.unwrap();
    assert_eq!(
        crate::guard::audited(&t.core, "kill_switch").await.len(),
        before + 1
    );
}

#[tokio::test]
async fn crew_step_files_a_routing_card_then_dispatches() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"dispatch":"worker","agent":"belai:builder","brief":"Fix the login"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let cards = t.core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!(c.status, "backlog");
    assert_eq!(c.labels, ["build"]);
    assert!(c.body.contains("Fix the login") && c.body.contains(&r.id));
    let meta: Value = serde_json::from_str(&c.meta_json).unwrap();
    assert_eq!(meta["belai"]["list"], "backlog");
    assert_eq!(meta["keleustes"]["runId"], r.id);
    let d = t.gw.dispatched.lock().unwrap().clone();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].1.card_id, c.id,
        "the dispatch carries the routing card"
    );
    let st = steps(&t, &r.id).await;
    let details: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(details["routeCardId"], c.id);
    assert_eq!(crate::guard::audited(&t.core, "dispatch").await.len(), 1);
    assert!(
        crate::guard::audited(&t.core, "card")
            .await
            .iter()
            .any(|e| e.subject["action"] == "route.file")
    );
    // A session step files no card.
    let wf2 = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"S","config":{"dispatch":"session","brief":"Look around"}})),
    )
    .await;
    start(&t, &wf2).await;
    assert_eq!(t.core.cards().await.unwrap().len(), 1);
}

#[tokio::test]
async fn dispatch_waits_under_the_kill_switch() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    assert_eq!(t.gw.dispatched.lock().unwrap().len(), 1);
    // A second run's step is held, not dispatched, while stopped.
    t.core.set_factory_stopped(true, "").await.unwrap();
    let report = ops::stop_all_runs(&t.core, "drill").await.unwrap();
    assert_eq!(report.runs_cancelled, 1);
    assert_eq!(run(&t, &r.id).await.status, "cancelled");
    assert!(
        crate::guard::audited(&t.core, "run")
            .await
            .iter()
            .any(|e| e.subject["action"] == "run.cancel")
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
async fn decision_routes_by_confidence_and_audits() {
    let t = setup().await;
    *t.gw.job.lock().unwrap() = Some(systemone("no", 0.9));
    let wf = published(&t.core, decision_graph(json!({}))).await;
    let r = start(&t, &wf).await;
    wait_status(&t, &r.id, "succeeded").await;
    let st = steps(&t, &r.id).await;
    let d = st.iter().find(|s| s.node_id == "d").unwrap();
    assert_eq!(d.route, "no");
    assert!(
        st.iter()
            .any(|s| s.node_id == "n" && s.status == "succeeded")
    );
    assert!(!st.iter().any(|s| s.node_id == "y"));
    let a = crate::guard::audited(&t.core, "decision").await;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].actor, "systemone:m1");

    // Below the threshold a person decides, with the pick suggested.
    *t.gw.job.lock().unwrap() = Some(systemone("yes", 0.4));
    let r = start(&t, &wf).await;
    let r = wait_status(&t, &r.id, "waiting_approval").await;
    let ap = t
        .core
        .store()
        .approvals(Some("pending"), Some(&r.id), 5)
        .await
        .unwrap();
    assert_eq!(ap.len(), 1);
    ops::decide_approval(&t.core, &ap[0].id, true, "ok", None)
        .await
        .unwrap();
    t.engine.advance(&r.id).await.unwrap();
    wait_status(&t, &r.id, "succeeded").await;
    assert!(steps(&t, &r.id).await.iter().any(|s| s.node_id == "y"));

    // A failed job abstains: fails closed by default.
    *t.gw.job.lock().unwrap() = Some(JobOutcome {
        status: "failed".into(),
        ..Default::default()
    });
    let r = start(&t, &wf).await;
    wait_status(&t, &r.id, "failed").await;
}

#[derive(Default)]
struct RecForge {
    actions: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Forge for RecForge {
    async fn action(&self, _ctx: &StepContext, action: &str) -> Result<Value> {
        self.actions.lock().unwrap().push(action.into());
        Ok(json!({"ok": true}))
    }
    async fn gate(&self, _ctx: &StepContext) -> Result<GateResult> {
        Ok(GateResult::Pass(json!({})))
    }
}

#[tokio::test]
async fn autonomy_gates_github_outputs() {
    let t = setup().await;
    let forge = Arc::new(RecForge::default());
    let engine = Engine::new(
        t.core.clone(),
        Integrations {
            forge: Some(forge.clone()),
            board: None,
        },
        config(),
    );
    let g = |action: &str| {
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"b","type":"github","title":"Out","config":{"action": action}}
            ],
            "edges": [{"from":"t","to":"b"}]
        }))
    };
    for (action, called) in [
        ("issue_comment", true),
        ("draft_pr", false),
        ("approve_ready", false),
    ] {
        let wf = published(&t.core, g(action)).await;
        let out = ops::start_run(
            &t.core,
            StartRun {
                workflow_id: wf,
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
        engine.advance(&r.id).await.unwrap();
        assert_eq!(run(&t, &r.id).await.status, "succeeded", "{action}");
        assert_eq!(
            forge.actions.lock().unwrap().contains(&action.to_string()),
            called,
            "{action}"
        );
        if !called {
            let st = steps(&t, &r.id).await;
            let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
            assert_eq!(d["skipped"], "autonomy");
        }
    }
    // Default autonomy is `suggest` (a comment passes); the refusals are audited.
    assert_eq!(crate::guard::audited(&t.core, "refused").await.len(), 2);
    assert_eq!(crate::guard::audited(&t.core, "github").await.len(), 1);
}

#[tokio::test]
async fn sf_stop_label_cancels_open_runs_on_the_subject() {
    let t = setup().await;
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"PR","config":{"kind":"webhook","event":"pull_request","overlap":"queue"}},
                {"id":"h","type":"human","title":"Approve","config":{"approvers":["owner"]}}
            ],
            "edges": [{"from":"t","to":"h"}]
        })),
    )
    .await;
    let ev = pull_event(&t, 9, json!({})).await;
    t.engine.on_event(&ev).await.unwrap();
    let r = wf_runs(&t, &wf).await;
    assert_eq!(r.len(), 1);
    let stop = t
        .core
        .emit(
            "github.pull.updated",
            "acme/api#9",
            &json!({"repo":"acme/api","number":9,"labels":["sf:stop"],"labelsAdded":["sf:stop"]}),
        )
        .await
        .unwrap();
    t.engine.on_event(&stop).await.unwrap();
    assert_eq!(run(&t, &r[0].id).await.status, "cancelled");
}

#[tokio::test]
async fn scan_settles_on_its_card_and_profile_is_stored() {
    let t = setup().await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"s","type":"scan","title":"Scout","config":{"source":"cli","job":"scout"}},
            {"id":"p","type":"profile","title":"Store","config":{}}
        ],
        "edges": [{"from":"t","to":"s"},{"from":"s","to":"p"}]
    }));
    let wf = published(&t.core, g).await;
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf,
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
    let cards = t.core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    let c = cards[0].clone();
    assert_eq!(c.labels, ["cli-job:scout"]);
    assert_eq!(
        t.gw.dispatched.lock().unwrap()[0].1.agent,
        "belai:cli-runner"
    );
    // The runner finishes: result on the card, card to done.
    let meta = json!({"belai":{"list":"done","agent":{"cliResult":{"signals":["Dockerfile","crypto"],"packageManagers":["npm"]}}}});
    let p = kls_core::board::CardPatch {
        status: Some("done".into()),
        meta_json: Some(meta.to_string()),
        ..Default::default()
    };
    t.core.update_card(&c.id, c.version, p, "n1").await.unwrap();
    let ev = t.core.emit("card.moved", &c.id, &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    let prof = repo_profile(&t.core, "acme/api").await.unwrap();
    assert_eq!(prof.signals, ["crypto", "dockerfile"]);
    assert!(
        crate::guard::audited(&t.core, "repo")
            .await
            .iter()
            .any(|e| e.subject["action"] == "repo.profile")
    );
}

#[tokio::test]
async fn unclaimed_routing_card_waits_past_the_claim_ttl() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    let b = st.iter().find(|s| s.node_id == "b").unwrap().clone();
    assert_eq!(b.status, "waiting");
    // The claim TTL passes with nobody claiming the card.
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
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "b" && s.status == "waiting" && s.error == "waiting:unclaimed")
    );
}

#[tokio::test]
async fn handoff_following_waits_for_child_cards() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Repro","config":{"agent":"belai:repro"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let card = t.core.cards().await.unwrap().remove(0);
    // The worker hands a build card on before its session ends.
    let child = t
        .core
        .create_card(
            kls_core::board::CardDraft {
                title: "Build".into(),
                status: "backlog".into(),
                labels: vec!["build".into()],
                meta_json: json!({"belai":{"agent":{"parent": card.id}}}).to_string(),
                ..Default::default()
            },
            "n1",
        )
        .await
        .unwrap();
    let ev = t
        .core
        .emit("dispatch.updated", "d1", &json!({"status":"done"}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(
        run(&t, &r.id).await.status,
        "running",
        "waits for the handed-off card"
    );
    let c = t
        .core
        .move_card(&child.id, child.version, "done", "n1")
        .await
        .unwrap();
    let ev = t.core.emit("card.moved", &c.id, &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
}

/// A worker that claims the routing card never stamps a session on the step:
/// the card reaching done settles it (web factory §5.6); blocked fails it.
#[tokio::test]
async fn routing_card_done_or_blocked_settles_the_worker_step() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Explain","config":{"agent":"belai:ci-explainer"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let card = t.core.cards().await.unwrap().remove(0);
    // An unrelated card moving changes nothing.
    let other = t
        .core
        .create_card(
            kls_core::board::CardDraft {
                title: "Other".into(),
                status: "done".into(),
                ..Default::default()
            },
            "n1",
        )
        .await
        .unwrap();
    let ev = t
        .core
        .emit("card.moved", &other.id, &json!({}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "running");
    let c = t
        .core
        .move_card(&card.id, card.version, "done", "n1")
        .await
        .unwrap();
    let ev = t.core.emit("card.moved", &c.id, &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    let b = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "b")
        .unwrap();
    let d: Value = serde_json::from_str(&b.details_json).unwrap();
    assert_eq!(d["cardStatus"], "done");

    // Blocked fails the step.
    let r = start(&t, &wf).await;
    let card = t
        .core
        .cards()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.meta_json.contains(&r.id))
        .unwrap();
    let mut labels = card.labels.clone();
    labels.push("blocked".into());
    let c = t
        .core
        .update_card(
            &card.id,
            card.version,
            kls_core::board::CardPatch {
                labels: Some(labels),
                ..Default::default()
            },
            "n1",
        )
        .await
        .unwrap();
    let ev = t
        .core
        .emit("card.updated", &c.id, &json!({}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let b = steps(&t, &r.id)
        .await
        .into_iter()
        .find(|s| s.node_id == "b")
        .unwrap();
    assert_eq!(
        (b.status.as_str(), b.error.as_str()),
        ("failed", "card_blocked")
    );
}

#[tokio::test]
async fn sf_comment_and_label_trust() {
    let t = setup().await;
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Fix","config":{"kind":"label","label":"sf:fix-tests","overlap":"queue"}},
                {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
            ],
            "edges": [{"from":"t","to":"b"}]
        })),
    )
    .await;
    let send = |n: i64, data: Value| {
        let core = t.core.clone();
        async move {
            let mut d = json!({"repo":"acme/api","number":n});
            for (k, v) in data.as_object().unwrap() {
                d[k] = v.clone();
            }
            core.emit("github.issue.updated", &format!("acme/api#{n}"), &d)
                .await
                .unwrap()
        }
    };
    let ev = send(
        1,
        json!({"comment":"/sf fix-tests please","commentAssociation":"MEMBER"}),
    )
    .await;
    t.engine.on_event(&ev).await.unwrap();
    let ev = send(
        2,
        json!({"comment":"/sf fix-tests","commentAssociation":"NONE"}),
    )
    .await;
    t.engine.on_event(&ev).await.unwrap();
    let ev = send(
        3,
        json!({"labelsAdded":["sf:fix-tests"],"fromFactory":true}),
    )
    .await;
    t.engine.on_event(&ev).await.unwrap();
    let ev = send(4, json!({"labelsAdded":["sf:fix-tests"],"labeledBy":"stranger","labeledByAssociation":"NONE"})).await;
    t.engine.on_event(&ev).await.unwrap();
    assert_eq!(wf_runs(&t, &wf).await.len(), 1);
    let mut f = fires(&t, &wf).await;
    f.sort();
    assert_eq!(
        f,
        [
            "skipped:comment_untrusted_sender",
            "skipped:from_factory",
            "skipped:label_untrusted_sender",
            "started"
        ]
    );
}

#[tokio::test]
async fn schedules_add_retime_delete_publish_new_versions() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"notify","title":"Say","config":{"message":"hi"}})),
    )
    .await;
    let p = ops::add_cron_schedule(&t.core, &wf, "0 9 * * 1-5", "UTC")
        .await
        .unwrap();
    assert_eq!(p.version, 2);
    let sch = p
        .schedules
        .iter()
        .find(|s| s.kind == "cron")
        .unwrap()
        .clone();
    assert!(
        ops::retime_schedule(&t.core, &sch.id, "never at all", "UTC")
            .await
            .is_err()
    );
    let p = ops::retime_schedule(&t.core, &sch.id, "30 8 * * *", "Europe/Paris")
        .await
        .unwrap();
    let s2 = p.schedules.iter().find(|s| s.kind == "cron").unwrap();
    assert_eq!(
        (s2.cron.as_str(), s2.tz.as_str(), p.version),
        ("30 8 * * *", "Europe/Paris", 3)
    );
    let p = ops::delete_schedule(&t.core, &s2.id).await.unwrap();
    assert!(p.schedules.iter().all(|s| s.kind != "cron"));
    assert_eq!(
        crate::guard::audited(&t.core, "workflow")
            .await
            .iter()
            .filter(|e| e.subject["action"] == "schedule.edit")
            .count(),
        3
    );
}

#[tokio::test]
async fn workflow_from_an_event() {
    let t = setup().await;
    let ev = pull_event(&t, 3, json!({})).await;
    let w = ops::create_from_event(&t.core, &ev).await.unwrap();
    let doc = ops::workflow_doc(&t.core, &w.id).await.unwrap().unwrap();
    let (_, _, g) = doc.editable();
    let tr = g.triggers().next().unwrap();
    assert_eq!(
        (tr.str("kind"), tr.str("event")),
        ("webhook", "pull_request.opened")
    );
    assert_eq!(g.scope.repos, ["acme/api"]);
    ops::publish(&t.core, &w.id, 1, "").await.unwrap();
}

#[tokio::test]
async fn forge_comment_event_and_labeled_by_map() {
    let t = setup().await;
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Fix","config":{"kind":"label","label":"sf:fix-tests","overlap":"queue"}},
                {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
            ],
            "edges": [{"from":"t","to":"b"}]
        })),
    )
    .await;
    // The forge's github.issue.comment shape: body, authorAssociation, command.
    let ev = t
        .core
        .emit(
            "github.issue.comment",
            "acme/api#5",
            &json!({"repo":"acme/api","number":5,
            "body":"/sf fix-tests","authorAssociation":"OWNER","command":"fix-tests"}),
        )
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    // A bot added the label (labeledBy map): refused unless allowBots.
    let ev = t
        .core
        .emit("github.issue.updated", "acme/api#6", &json!({"repo":"acme/api","number":6,
            "labelsAdded":["sf:fix-tests"],"labeledBy":{"sf:fix-tests":"renovate[bot]"},"fromFactory":false}))
        .await
        .unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let mut f = fires(&t, &wf).await;
    f.sort();
    assert_eq!(f, ["skipped:label_bot_sender", "started"]);
}

#[tokio::test]
async fn evidence_pack_and_facts_and_stagger() {
    let t = setup().await;
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder"}})),
    )
    .await;
    let r = start(&t, &wf).await;
    let d = ops::run_detail(&t.core, &r.id).await.unwrap().unwrap();
    let f = d.facts();
    assert_eq!(f.agents, ["belai:builder"]);
    assert_eq!(f.nodes, ["node-a"]);
    assert_eq!(f.cards.len(), 1);
    let path = ops::write_evidence(&t.core, &r.id).await.unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["kind"], "keleustes.evidence/v1");
    assert_eq!(v["cards"].as_array().unwrap().len(), 1);
    assert!(
        crate::guard::audited(&t.core, "run")
            .await
            .iter()
            .any(|e| e.subject["action"] == "evidence.export")
    );
    // Stagger: stable per repo, inside the window.
    let a = stagger_offset("acme/api", 3_600_000);
    assert_eq!(a, stagger_offset("ACME/api", 3_600_000));
    assert!((0..3_600_000).contains(&a));
    assert_eq!(stagger_offset("x", 0), 0);
    // Dry-run call ranges.
    let g = graph(
        json!({"nodes":[{"id":"t","type":"trigger","config":{"kind":"manual"}},
        {"id":"p","type":"github","config":{"action":"draft_pr"}}],"edges":[{"from":"t","to":"p"}]}),
    );
    let dr = crate::dry_run(&g, &Value::Null);
    assert_eq!((dr.calls.min, dr.calls.max), (2, 4));
}

#[tokio::test]
async fn crew_settings_ride_on_the_dispatch() {
    let t = setup().await;
    let doc = t.core.settings_doc().await.unwrap();
    let mut ch = std::collections::BTreeMap::new();
    ch.insert("belai.tests.post_end".to_string(), json!(false));
    ch.insert("crews.lease".to_string(), json!(45));
    t.core.settings_save(doc.revision, ch).await.unwrap();
    let wf = published(
        &t.core,
        simple(json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder","showInternalWork":true}})),
    )
    .await;
    start(&t, &wf).await;
    let d = t.gw.dispatched.lock().unwrap().clone();
    let o = &d[0].1.options;
    assert_eq!(o.get("tests_post_end").map(String::as_str), Some("off"));
    assert_eq!(o.get("crew_lease_ms").map(String::as_str), Some("2700000"));
    assert_eq!(
        o.get("show_internal_work").map(String::as_str),
        Some("true"),
        "the step overrides"
    );
    assert_eq!(o.get("guardrails").map(String::as_str), Some("on"));
}

/// The options a dispatch carries with the default settings are values
/// belai's `rc.ApplyFactoryOptions` takes (it refuses the whole dispatch
/// otherwise): guardrails "on", tests_post_end and tests_on_fail from its
/// sessionctl lists, auto_commit_per_task only ever "false", booleans.
#[tokio::test]
async fn default_crew_options_are_values_belai_takes() {
    let t = setup().await;
    let eff = kls_core::audit::config::Effective::load(&t.core)
        .await
        .unwrap();
    let n: Node = serde_json::from_value(
        json!({"id":"b","type":"crew","config":{"agent":"belai:builder","testsPostEnd":true,"testsOnFail":"report"}}),
    )
    .unwrap();
    for node in [simple_node(), n] {
        let o = crew_options(Some(&eff), &node);
        for (k, v) in &o {
            let ok = match k.as_str() {
                "guardrails" => v == "on",
                "tests_post_end" => ["off", "goal", "goal_plan", "session"].contains(&v.as_str()),
                "tests_on_fail" => ["off", "diagnose", "fix"].contains(&v.as_str()),
                "auto_commit_per_task" => v == "false",
                "show_internal_work" | "a2a_show" => v == "true" || v == "false",
                "crew_lease_ms" => v.parse::<i64>().is_ok_and(|ms| ms > 0),
                _ => false,
            };
            assert!(ok, "belai refuses {k}={v}");
        }
    }
}

fn simple_node() -> Node {
    serde_json::from_value(json!({"id":"b","type":"crew","config":{"agent":"belai:builder"}}))
        .unwrap()
}

#[tokio::test]
async fn provision_step_checks_switch_plan_and_confirmation() {
    let t = setup().await;
    // Publishing needs the plan, the node and the typed host name.
    let bad = graph(
        json!({"nodes":[{"id":"t","type":"trigger","config":{"kind":"manual"}},
        {"id":"p","type":"provision","config":{"planId":"pl1","node":"node-a"}}],"edges":[{"from":"t","to":"p"}]}),
    );
    assert!(!crate::validate(&bad, crate::Mode::Publish).is_empty());
    let g = graph(
        json!({"nodes":[{"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
        {"id":"p","type":"provision","title":"Provision","config":{"planId":"nope","node":"node-a","confirmName":"node-a"}}],
        "edges":[{"from":"t","to":"p"}]}),
    );
    let wf = published(&t.core, g).await;
    // An unknown plan is refused by the hosts API and fails the step.
    let r = start(&t, &wf).await;
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "p" && s.error == "provision_refused"),
        "{st:?}"
    );
}
