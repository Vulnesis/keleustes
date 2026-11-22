//! The `provision` step end to end against the host-management API
//! (HM-9b): a node with an inventory, a plan, the host previewing each step,
//! the plan running, and the step settling. The host's side is played by
//! `hosts::apply_update`, as `Host.Progress` would.

use super::*;
use kls_core::hosts::{self, HostManage, Inventory, PlanStep, Update};

const NODE: &str = "node-a";

/// The node joined, named as the workflow confirms it, offering every kind
/// at the provision gate.
async fn host(t: &T) {
    t.core
        .store()
        .insert_node(NODE, NODE, &Default::default(), "serial", "tok")
        .await
        .unwrap();
    let inv = Inventory {
        at_ms: now_ms(),
        manage: HostManage {
            host_actions: hosts::KINDS.iter().map(|(k, _)| k.to_string()).collect(),
            manage_level: "provision".into(),
            manage_shell: false,
        },
        ..Default::default()
    };
    hosts::report_inventory(&t.core, NODE, &inv).await.unwrap();
}

async fn plan(t: &T) -> String {
    let step = |kind: &str, params: &[(&str, &str)]| PlanStep {
        kind: kind.into(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        label: kind.into(),
    };
    hosts::save_plan(
        &t.core,
        "",
        "Baseline",
        &[
            step("repo_trust", &[("path", "~/src/api")]),
            step(
                "product_configure",
                &[("product", "package_firewall"), ("on", "npm")],
            ),
        ],
    )
    .await
    .unwrap()
    .id
}

async fn provision_flow(t: &T, plan_id: &str) -> (String, Run) {
    let g = graph(json!({"nodes":[
        {"id":"t","type":"trigger","title":"Go","config":{"kind":"manual"}},
        {"id":"p","type":"provision","title":"Provision",
         "config":{"planId":plan_id,"node":NODE,"confirmName":NODE}}],
        "edges":[{"from":"t","to":"p"}]}));
    let wf = published(&t.core, g).await;
    let r = start(t, &wf).await;
    (wf, r)
}

/// The host answers an action (seq 1, then 2 for a real one).
async fn host_says(t: &T, id: &str, seq: u32, state: &str, sha: &str) {
    hosts::apply_update(
        &t.core,
        NODE,
        Update {
            action_id: id.into(),
            seq,
            state: state.into(),
            preview_json: if sha.is_empty() {
                String::new()
            } else {
                r#"{"argv":["x"]}"#.into()
            },
            preview_sha256: sha.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

async fn previews(t: &T) -> Vec<kls_store::host::HostActionRow> {
    let mut v: Vec<_> = t
        .core
        .store()
        .host_actions(NODE, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.dry_run && a.state == "queued")
        .collect();
    v.sort_by_key(|a| a.step);
    v
}

async fn provision_step(t: &T, run_id: &str) -> RunStep {
    steps(t, run_id)
        .await
        .into_iter()
        .find(|s| s.node_id == "p")
        .expect("provision step")
}

/// Fires the provision step's timer now (it polls every
/// PROVISION_POLL_MS).
async fn wake(t: &T, run_id: &str) {
    let st = provision_step(t, run_id).await;
    t.core
        .store()
        .update_step(
            &st.id,
            &[],
            &StepUpdate {
                wake_at: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    t.engine.tick(now_ms()).await.unwrap();
}

#[tokio::test]
async fn previews_each_step_runs_the_plan_and_succeeds() {
    let t = setup().await;
    host(&t).await;
    let plan_id = plan(&t).await;
    let (wf, r) = provision_flow(&t, &plan_id).await;

    // One host-side dry run per plan step, from the workflow.
    let pre = previews(&t).await;
    assert_eq!(pre.len(), 2);
    for p in &pre {
        let o = hosts::HostActionRowExt::origin(p);
        assert_eq!(o.surface, format!("workflow:{wf}"));
        assert_eq!(o.run_id, r.id);
    }
    assert_eq!(provision_step(&t, &r.id).await.status, "waiting");
    for (i, p) in pre.iter().enumerate() {
        host_says(&t, &p.id, 1, "done", &format!("sha{i}")).await;
    }

    // Previews in: the plan runs with the host name captured at publish.
    wake(&t, &r.id).await;
    let runs = t.core.store().host_plan_runs(NODE, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    let plan_run = runs[0].id.clone();
    // The host finishes each real step as the plan queues it.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let pr = t
            .core
            .store()
            .host_plan_run(&plan_run)
            .await
            .unwrap()
            .unwrap();
        if pr.state != "running" {
            assert_eq!(pr.state, "done", "{}", pr.error);
            break;
        }
        for a in t.core.store().host_actions_of_run(&plan_run).await.unwrap() {
            if !a.dry_run && a.state == "queued" {
                assert!(
                    !a.preview_sha256.is_empty(),
                    "real step carries its preview"
                );
                host_says(&t, &a.id, 1, "done", "").await;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "plan never finished"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    wake(&t, &r.id).await;
    wait_status(&t, &r.id, "succeeded").await;
    let ledger = t
        .core
        .audit_entries(&kls_store::audit::LedgerQuery::default())
        .await
        .unwrap();
    assert!(
        ledger
            .iter()
            .any(|e| e.kind == "host_plan" && e.msg.contains("host.plan.finished"))
    );
    assert!(
        ledger
            .iter()
            .any(|e| e.kind == "host_action" && e.msg.contains(&format!("workflow:{wf}")))
    );
}

#[tokio::test]
async fn a_failed_preview_fails_the_step_and_runs_nothing() {
    let t = setup().await;
    host(&t).await;
    let plan_id = plan(&t).await;
    let (_wf, r) = provision_flow(&t, &plan_id).await;
    let pre = previews(&t).await;
    host_says(&t, &pre[0].id, 1, "done", "sha0").await;
    host_says(&t, &pre[1].id, 1, "failed", "").await;
    wake(&t, &r.id).await;
    let st = provision_step(&t, &r.id).await;
    assert_eq!(st.error, "provision_preview_failed", "{st:?}");
    assert!(
        t.core
            .store()
            .host_plan_runs(NODE, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn under_stop_the_plan_never_starts_and_the_refusal_is_audited() {
    let t = setup().await;
    host(&t).await;
    let plan_id = plan(&t).await;
    let (_wf, r) = provision_flow(&t, &plan_id).await;
    let pre = previews(&t).await;
    for (i, p) in pre.iter().enumerate() {
        host_says(&t, &p.id, 1, "done", &format!("sha{i}")).await;
    }
    kls_core::control::stop(&t.core, "incident").await.unwrap();
    wake(&t, &r.id).await;
    assert!(
        t.core
            .store()
            .host_plan_runs(NODE, 10)
            .await
            .unwrap()
            .is_empty(),
        "no plan run while stopped"
    );
    assert!(
        !host_queued_real(&t).await,
        "no real host action queued while stopped"
    );
    // The engine holds every run while stopped; the stop itself is audited.
    t.core.audit_sync().await.unwrap();
    let ledger = t
        .core
        .audit_entries(&kls_store::audit::LedgerQuery::default())
        .await
        .unwrap();
    assert!(ledger.iter().any(|e| e.msg.contains("factory.mode")));
    // The hosts API refuses a plan run asked for directly, and audits it.
    let ids: Vec<String> = pre.iter().map(|p| p.id.clone()).collect();
    let e = hosts::run_plan(
        &t.core,
        &plan_id,
        NODE,
        NODE,
        &ids,
        hosts::Origin::owner("provision"),
    )
    .await
    .unwrap_err();
    assert_eq!(hosts::refusal_of(&e).unwrap().code(), "kill_switch");
    let evs = t.core.store().recent_events(500).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == "factory.refused"));
    let st = provision_step(&t, &r.id).await;
    assert_ne!(st.status, "succeeded", "{st:?}");

    // Held, not dropped: once lifted, the step runs the plan.
    kls_core::control::resume(&t.core).await.unwrap();
    wake(&t, &r.id).await;
    let runs = t.core.store().host_plan_runs(NODE, 10).await.unwrap();
    assert!(
        runs.iter().any(|x| x.state == "running"),
        "plan starts after resume: {runs:?}"
    );
}

async fn host_queued_real(t: &T) -> bool {
    t.core
        .store()
        .host_actions(NODE, 100)
        .await
        .unwrap()
        .iter()
        .any(|a| !a.dry_run && a.state == "queued")
}
