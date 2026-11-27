//! Steps inside Keleustes: the CLI scan's result, rescan branch and write
//! autonomy, the kept state card and the OpenVEX output, each with the kill
//! switch and its ledger entry.

use super::*;

fn with_board(t: &T) -> Engine {
    Engine::new(
        t.core.clone(),
        Integrations {
            forge: None,
            board: Some(Arc::new(CoreBoard(t.core.clone()))),
        },
        config(),
    )
}

async fn start_on(t: &T, engine: &Engine, wf: &str) -> Run {
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
    engine.advance(&r.id).await.unwrap();
    r
}

/// The scan's card reaches done with `result` as its CLI result.
async fn finish_card(t: &T, engine: &Engine, card_id: &str, result: Value) {
    let c = t.core.card(card_id).await.unwrap().unwrap();
    let meta = json!({"belai":{"list":"done","agent":{"cliResult": result}}});
    let p = kls_core::board::CardPatch {
        status: Some("done".into()),
        meta_json: Some(meta.to_string()),
        ..Default::default()
    };
    t.core.update_card(&c.id, c.version, p, "n1").await.unwrap();
    let ev = t.core.emit("card.moved", &c.id, &json!({})).await.unwrap();
    engine.on_event(&ev).await.unwrap();
}

fn scan_card(t: &T, job: &str) -> impl std::future::Future<Output = kls_store::board::Card> {
    let core = t.core.clone();
    let label = format!("cli-job:{job}");
    async move {
        core.cards()
            .await
            .unwrap()
            .into_iter()
            .rfind(|c| c.labels.contains(&label) && c.status != "done")
            .unwrap()
    }
}

#[tokio::test]
async fn a_scan_reads_its_result_with_the_web_grammar() {
    let t = setup().await;
    let g = |fail_on: Value| {
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"s","type":"scan","title":"Verify","config":{"source":"cli","job":"attest-verify","failOn": fail_on}},
            ],
            "edges": [{"from":"t","to":"s"}]
        }))
    };
    // Exit 1 with nothing listed: one harness finding for the next node.
    let wf = published(&t.core, g(json!([]))).await;
    let r = start_on(&t, &t.engine, &wf).await;
    let c = scan_card(&t, "attest-verify").await;
    finish_card(
        &t,
        &t.engine,
        &c.id,
        json!({"job":"attest-verify","exitCode":1}),
    )
    .await;
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    let st = steps(&t, &r.id).await;
    let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(d["result"]["findings"][0]["id"], "attest-verify:exit-1");
    let f = crate::forge::outputs::findings(&st, i64::MAX).unwrap();
    assert_eq!(f.high, 1);

    // Another job's result is refused.
    let r = start_on(&t, &t.engine, &wf).await;
    let c = scan_card(&t, "attest-verify").await;
    finish_card(&t, &t.engine, &c.id, json!({"job":"sca","exitCode":0})).await;
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "s" && s.error == "cli_result_mismatch"),
        "{st:?}"
    );
}

#[tokio::test]
async fn fail_on_malware_stops_the_run() {
    let t = setup().await;
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"s","type":"scan","title":"Malware","config":{"source":"cli","job":"malscan","failOn":["malware"]}},
            ],
            "edges": [{"from":"t","to":"s"}]
        })),
    )
    .await;
    let r = start_on(&t, &t.engine, &wf).await;
    let c = scan_card(&t, "malscan").await;
    finish_card(
        &t,
        &t.engine,
        &c.id,
        json!({"job":"malscan","exitCode":1,"counts":{"critical":1},
               "findings":[{"id":"pkg@1","kind":"malscan","severity":"critical"}]}),
    )
    .await;
    let st = steps(&t, &r.id).await;
    assert!(
        st.iter()
            .any(|s| s.node_id == "s" && s.error == "scan_blocking: malware"),
        "{st:?}"
    );
}

#[tokio::test]
async fn a_write_job_needs_draft_pr_and_a_rescan_reads_the_crew_branch() {
    let t = setup().await;
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"f","type":"scan","title":"Fix","config":{"source":"cli","job":"fix"}},
        ],
        "edges": [{"from":"t","to":"f"}]
    }));
    // Below draft_pr (the default is suggest): skipped, refused, audited.
    let wf = published(&t.core, g).await;
    let r = start_on(&t, &t.engine, &wf).await;
    assert_eq!(run(&t, &r.id).await.status, "succeeded");
    let st = steps(&t, &r.id).await;
    let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(d["skipped"], "autonomy");
    assert!(t.gw.dispatched.lock().unwrap().is_empty());
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.msg.contains("autonomy:suggest"))
    );
    t.core
        .store()
        .set_setting("autonomy.default", "draft_pr")
        .await
        .unwrap();
    let r = start_on(&t, &t.engine, &wf).await;
    assert_eq!(
        t.gw.dispatched.lock().unwrap()[0].1.agent,
        "belai:cli-runner-write"
    );
    ops::cancel_run(&t.core, &r.id, "test").await.unwrap();

    // A read-only scan after a crew scans the crew's branch.
    let g = graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
            {"id":"c","type":"crew","title":"Patch","config":{"agent":"belai:patcher"}},
            {"id":"s","type":"scan","title":"Rescan","config":{"source":"cli","job":"sast"}},
        ],
        "edges": [{"from":"t","to":"c"},{"from":"c","to":"s"}]
    }));
    let wf = published(&t.core, g).await;
    let r = start_on(&t, &t.engine, &wf).await;
    let st = steps(&t, &r.id).await;
    let crew = st.iter().find(|s| s.node_id == "c").unwrap();
    let d: Value = serde_json::from_str(&crew.details_json).unwrap();
    let card_id = d["routeCardId"].as_str().unwrap().to_string();
    let c = t.core.card(&card_id).await.unwrap().unwrap();
    let p = kls_core::board::CardPatch {
        status: Some("done".into()),
        branch: Some("keleustes/K-fix1".into()),
        ..Default::default()
    };
    t.core.update_card(&c.id, c.version, p, "n1").await.unwrap();
    let ev = t.core.emit("card.moved", &c.id, &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let sent = t.gw.dispatched.lock().unwrap().clone();
    let rescan = sent
        .iter()
        .find(|(_, s)| s.agent == "belai:cli-runner")
        .unwrap();
    assert_eq!(rescan.1.branch, "keleustes/K-fix1");
    let card = scan_card(&t, "sast").await;
    assert_eq!(card.branch, "keleustes/K-fix1");
}

#[tokio::test]
async fn tea_release_needs_the_repository_tea_switch() {
    let t = setup().await;
    let mut r = kls_store::github::GhRepo {
        full_name: "acme/api".into(),
        owner: "acme".into(),
        name: "api".into(),
        default_branch: "main".into(),
        ..Default::default()
    };
    r.settings.autonomy = "draft_pr".into();
    t.core.store().gh_upsert_repo(&r).await.unwrap();
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"p","type":"scan","title":"Publish to TEA","config":{"source":"cli","job":"tea-release"}},
            ],
            "edges": [{"from":"t","to":"p"}]
        })),
    )
    .await;
    let run1 = start_on(&t, &t.engine, &wf).await;
    let st = steps(&t, &run1.id).await;
    let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(d["skipped"], "tea_disabled");
    assert!(t.gw.dispatched.lock().unwrap().is_empty());
    let rev = t
        .core
        .store()
        .gh_repo("acme/api")
        .await
        .unwrap()
        .unwrap()
        .revision;
    r.settings.tea_publish = true;
    t.core
        .store()
        .gh_set_repo_settings("acme/api", &r.settings, rev)
        .await
        .unwrap();
    start_on(&t, &t.engine, &wf).await;
    assert_eq!(
        t.gw.dispatched.lock().unwrap()[0].1.agent,
        "belai:cli-runner-write"
    );
}

#[tokio::test]
async fn ensure_keeps_one_open_card_per_ref_and_holds_under_the_switch() {
    let t = setup().await;
    let engine = with_board(&t);
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"k","type":"card","title":"Malware response card","config":{"action":"ensure","ref":"malware-response"}},
            ],
            "edges": [{"from":"t","to":"k"}]
        })),
    )
    .await;
    let r1 = start_on(&t, &engine, &wf).await;
    assert_eq!(run(&t, &r1.id).await.status, "succeeded");
    let cards = t.core.cards().await.unwrap();
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!(
        (c.status.as_str(), c.repo.as_str()),
        ("backlog", "acme/api")
    );
    assert!(c.labels.contains(&"factory".to_string()));
    assert!(c.title.ends_with(": malware-response"));
    // A second run finds the same open card.
    let r2 = start_on(&t, &engine, &wf).await;
    let st = steps(&t, &r2.id).await;
    let d: Value = serde_json::from_str(&st[1].details_json).unwrap();
    assert_eq!(
        (d["cardId"].as_str(), d["created"].as_bool()),
        (Some(c.id.as_str()), Some(false))
    );
    assert_eq!(t.core.cards().await.unwrap().len(), 1);
    let led = crate::guard::audited(&t.core, "card").await;
    let state: Vec<_> = led
        .iter()
        .filter(|e| e.subject["action"] == "state.card")
        .collect();
    assert_eq!(state.len(), 2, "{state:?}");
    let created = state
        .iter()
        .find(|e| e.payload["outcome"] == "created")
        .unwrap();
    assert_eq!(created.payload["origin"]["runId"], r1.id);
    assert!(state.iter().any(|e| e.payload["outcome"] == "found"));

    // A run queued before the kill switch writes no card until it lifts.
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.clone(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/other".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r3 = out.run().unwrap().clone();
    t.core.set_factory_stopped(true, "drill").await.unwrap();
    engine.advance(&r3.id).await.unwrap();
    assert_eq!(t.core.cards().await.unwrap().len(), 1);
    assert_ne!(run(&t, &r3.id).await.status, "succeeded");
    t.core.set_factory_stopped(false, "").await.unwrap();
    engine.advance(&r3.id).await.unwrap();
    assert_eq!(run(&t, &r3.id).await.status, "succeeded");
    // Another repository keeps its own card.
    assert_eq!(t.core.cards().await.unwrap().len(), 2);
}

#[tokio::test]
async fn vex_output_hands_its_document_to_vex_apply() {
    let t = setup().await;
    t.core
        .store()
        .set_setting("autonomy.default", "draft_pr")
        .await
        .unwrap();
    let wf = published(
        &t.core,
        graph(json!({
            "nodes": [
                {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
                {"id":"s","type":"scan","title":"Triage","config":{"source":"cli","job":"triage"}},
                {"id":"v","type":"github","title":"Write the OpenVEX document","config":{"action":"vex"}},
                {"id":"a","type":"scan","title":"Upload","config":{"source":"cli","job":"vex-apply","notEvaluable":"report"}},
            ],
            "edges": [{"from":"t","to":"s"},{"from":"s","to":"v"},{"from":"v","to":"a"}]
        })),
    )
    .await;
    let r = start_on(&t, &t.engine, &wf).await;
    let c = scan_card(&t, "triage").await;
    finish_card(
        &t,
        &t.engine,
        &c.id,
        json!({"job":"triage","exitCode":0,"counts":{"high":1},
               "findings":[{"id":"CVE-2026-1","kind":"vuln","severity":"high"},
                           {"id":"gh-token","kind":"secret","severity":"high"}]}),
    )
    .await;
    let st = steps(&t, &r.id).await;
    let v = st.iter().find(|s| s.node_id == "v").unwrap();
    assert_eq!(v.status, "succeeded");
    let d: Value = serde_json::from_str(&v.details_json).unwrap();
    assert_eq!(d["statements"], 1);
    assert_eq!(
        d["vex"]["statements"][0]["vulnerability"]["name"],
        "CVE-2026-1"
    );
    assert_eq!(d["vex"]["statements"][0]["status"], "affected");
    let apply = scan_card(&t, "vex-apply").await;
    let meta: Value = serde_json::from_str(&apply.meta_json).unwrap();
    assert_eq!(
        meta["keleustes"]["vex"]["@context"],
        "https://openvex.dev/ns/v0.2.0"
    );
    assert!(
        crate::guard::audited(&t.core, "vex")
            .await
            .iter()
            .any(|e| e.subject["action"] == "vex.written")
    );
}
