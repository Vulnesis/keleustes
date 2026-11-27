//! P2 parity: the run blocked when no model is open to the factory, extra
//! triggers (`also`), and the GitHub read node.

use super::*;
use crate::validate::{self, Mode};

fn crew(model: &str) -> Graph {
    simple(
        json!({"id":"b","type":"crew","title":"Build","config":{"agent":"belai:builder","model":model}}),
    )
}

#[test]
fn model_block_reads_the_vault_catalogue() {
    let open = vec!["anthropic".to_string()];
    let known = vec!["anthropic".to_string(), "openai".to_string()];
    assert_eq!(
        ops::model_block(&crew("openai/gpt-5"), &open, &known).as_deref(),
        Some("openai is not open to the factory")
    );
    assert!(ops::model_block(&crew("anthropic/x"), &open, &known).is_none());
    // A provider the vault does not list is left to the node check.
    assert!(ops::model_block(&crew("acme/m1"), &open, &known).is_none());
    assert_eq!(
        ops::model_block(&crew(""), &[], &known).as_deref(),
        Some("no provider is open to the factory")
    );
    // No model step, or an empty catalogue: nothing to judge.
    let notify = simple(json!({"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}));
    assert!(ops::model_block(&notify, &[], &known).is_none());
    assert!(ops::model_block(&crew(""), &[], &[]).is_none());
}

#[tokio::test]
async fn a_run_is_blocked_when_its_model_is_not_open() {
    let t = setup().await;
    let wf = published(&t.core, crew("openai/gpt-5")).await;
    let mut p = t
        .core
        .store()
        .provider("openai")
        .await
        .unwrap()
        .expect("seeded provider");
    p.factory = false;
    t.core.store().upsert_provider(&p).await.unwrap();
    let out = ops::start_run(
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
    .await
    .unwrap();
    match out {
        StartOutcome::Skipped(why) => {
            assert_eq!(why, "no_model: openai is not open to the factory")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        crate::guard::audited(&t.core, "refused")
            .await
            .iter()
            .any(|e| e.msg.contains("no_model"))
    );
    assert!(
        t.core
            .store()
            .workflow_runs_in(&wf, &["queued", "running"], None)
            .await
            .unwrap()
            .is_empty()
    );
    // Opened again, it starts.
    p.factory = true;
    t.core.store().upsert_provider(&p).await.unwrap();
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
    assert!(matches!(out, StartOutcome::Started(_)), "{out:?}");
}

fn also_graph(also: Value) -> Graph {
    graph(json!({
        "nodes": [
            {"id":"t","type":"trigger","title":"Go","config":{"kind":"event","event":"custom.first","overlap":"queue","also": also}},
            {"id":"b","type":"notify","title":"Say","config":{"message":"hi"}}
        ],
        "edges": [{"from":"t","to":"b"}]
    }))
}

#[test]
fn extra_triggers_validate_and_become_rows() {
    let ok = also_graph(json!([{"kind":"event","event":"custom.second"},{"kind":"manual"}]));
    assert!(!validate::has_errors(&validate::validate(
        &ok,
        Mode::Publish
    )));
    let specs = ops::trigger_specs(&ok, 0);
    let ids: Vec<&str> = specs.iter().map(|s| s.node_id.as_str()).collect();
    assert_eq!(ids, ["t", "t#1"], "a manual extra trigger has no row");
    let cfg: Value = serde_json::from_str(&specs[1].config_json).unwrap();
    assert_eq!(cfg["event"], "custom.second");
    assert_eq!(crate::graph::trigger_of("t#1"), "t");
    assert_eq!(
        crate::walker::start_node(&ok, "t#1").map(|n| n.id.as_str()),
        Some("t")
    );
    let codes = |g: &Graph, mode| -> Vec<String> {
        validate::validate(g, mode)
            .into_iter()
            .filter(|i| i.level == validate::Level::Error)
            .map(|i| format!("{} {}", i.code, i.path))
            .collect()
    };
    let bad = also_graph(json!("label"));
    assert!(
        codes(&bad, Mode::Save)
            .iter()
            .any(|c| c.starts_with("invalid_type"))
    );
    let bad =
        also_graph(json!([{"kind":"cron","cron":"0 3 * * *"}, 7, {"kind":"label","also":[]}]));
    let c = codes(&bad, Mode::Publish);
    assert!(c.iter().any(|c| c.ends_with("also[0].kind")), "{c:?}");
    assert!(c.iter().any(|c| c.ends_with("also[1]")), "{c:?}");
    assert!(c.iter().any(|c| c.ends_with("also[2].also")), "{c:?}");
    assert!(c.iter().any(|c| c.ends_with("also[2].label")), "{c:?}");
    // A draft may leave the detail blank; publish may not.
    let draft = also_graph(json!([{"kind":"webhook"}]));
    assert!(codes(&draft, Mode::Save).is_empty());
    assert!(!codes(&draft, Mode::Publish).is_empty());
}

#[tokio::test]
async fn an_extra_trigger_starts_the_run_and_can_be_removed() {
    let t = setup().await;
    let wf = published(
        &t.core,
        also_graph(json!([{"kind":"event","event":"custom.second"}])),
    )
    .await;
    let rows = t.core.store().schedules_for_workflow(&wf).await.unwrap();
    assert_eq!(rows.len(), 2);
    let ev = t.core.emit("custom.second", "x", &json!({})).await.unwrap();
    t.engine.on_event(&ev).await.unwrap();
    let runs = t
        .core
        .store()
        .workflow_runs_in(&wf, &["queued", "running", "succeeded"], None)
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].trigger_node_id, "t",
        "the run starts at the trigger"
    );
    assert_eq!(runs[0].status, "succeeded");
    // Removing the extra trigger's row drops the `also` entry only.
    let extra = rows.iter().find(|r| r.node_id == "t#1").unwrap();
    let p = ops::delete_schedule(&t.core, &extra.id).await.unwrap();
    assert_eq!(p.schedules.len(), 1);
    assert_eq!(p.schedules[0].node_id, "t");
}

fn read_graph(cfg: Value) -> Graph {
    simple(json!({"id":"b","type":"read","title":"Read","config": cfg}))
}

async fn mirror_pull(t: &T) {
    t.core
        .store()
        .gh_upsert_pull(&kls_store::github::GhPull {
            repo: "acme/api".into(),
            number: 7,
            title: "Fix".into(),
            state: "open".into(),
            mergeable: "clean".into(),
            checks_state: "success".into(),
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn start_on(t: &T, wf: &str, inputs: Value) -> Run {
    let out = ops::start_run(
        &t.core,
        StartRun {
            workflow_id: wf.into(),
            trigger: TriggerInfo {
                kind: "manual".into(),
                ..Default::default()
            },
            repo: "acme/api".into(),
            inputs,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = out.run().unwrap().clone();
    t.engine.advance(&r.id).await.unwrap();
    run(t, &r.id).await
}

fn read_details(st: &[RunStep]) -> Value {
    let s = st.iter().find(|s| s.node_id == "b").unwrap();
    serde_json::from_str(&s.details_json).unwrap()
}

#[tokio::test]
async fn the_read_node_reads_the_mirror_and_falls_back_on_a_live_failure() {
    let t = setup().await;
    mirror_pull(&t).await;
    let g = read_graph(json!({"source":"mirror","what":"pull"}));
    assert!(!validate::has_errors(&validate::validate(
        &g,
        Mode::Publish
    )));
    let wf = published(&t.core, g).await;
    let r = start_on(&t, &wf, json!({"number": 7})).await;
    assert_eq!(r.status, "succeeded");
    let d = read_details(&steps(&t, &r.id).await);
    assert_eq!(
        (d["source"].as_str(), d["githubCalls"].as_i64()),
        (Some("mirror"), Some(0))
    );
    assert_eq!(d["data"]["mergeable"], "clean");

    // Live with no GitHub connected: the mirror, marked stale, evented and audited.
    let wf = published(&t.core, read_graph(json!({"source":"api","what":"pull"}))).await;
    let r = start_on(&t, &wf, json!({"number": 7})).await;
    assert_eq!(r.status, "succeeded");
    let d = read_details(&steps(&t, &r.id).await);
    assert_eq!(d["requestedSource"], "api");
    assert_eq!(d["stale"], true);
    assert_eq!(d["data"]["number"], 7);
    let events = t.core.store().recent_events(200).await.unwrap();
    assert!(events.iter().any(|e| e.kind == crate::read::READ_FALLBACK));
    assert!(
        crate::guard::audited(&t.core, "read")
            .await
            .iter()
            .any(|e| e.msg.contains("source_fallback"))
    );

    // `onQuotaLow: fail` fails instead; a pull the mirror lacks fails too.
    let wf = published(
        &t.core,
        read_graph(json!({"source":"api","what":"pull","onQuotaLow":"fail"})),
    )
    .await;
    let r = start_on(&t, &wf, json!({"number": 7})).await;
    assert_eq!(r.status, "failed");
    let st = steps(&t, &r.id).await;
    assert_eq!(
        st.iter().find(|s| s.node_id == "b").unwrap().error,
        "read_live_failed"
    );
    let wf = published(&t.core, read_graph(json!({"what":"pull"}))).await;
    let r = start_on(&t, &wf, json!({"number": 99})).await;
    let st = steps(&t, &r.id).await;
    assert_eq!(
        st.iter().find(|s| s.node_id == "b").unwrap().error,
        "read_not_mirrored"
    );

    // Issues from the mirror; a webhook read with no event fails.
    let wf = published(&t.core, read_graph(json!({"what":"issues"}))).await;
    let r = start_on(&t, &wf, Value::Null).await;
    assert_eq!(read_details(&steps(&t, &r.id).await)["data"]["open"], 0);
    let wf = published(&t.core, read_graph(json!({"source":"webhook"}))).await;
    let r = start_on(&t, &wf, Value::Null).await;
    let st = steps(&t, &r.id).await;
    assert_eq!(
        st.iter().find(|s| s.node_id == "b").unwrap().error,
        "read_no_event"
    );

    // A live read of anything but the pull request is refused on publish.
    let bad = read_graph(json!({"source":"api","what":"issues"}));
    assert!(validate::has_errors(&validate::validate(
        &bad,
        Mode::Publish
    )));
}
