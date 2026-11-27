//! Host management rules 1 to 4 for every action kind: provenance stored,
//! audit chained, refused under the kill switch and the A2A blackout, and
//! refused from anything but the console.

use super::*;
use crate::gateway::{DispatchSpec, JobNeeds, JobOutcome, NodeGateway};
use std::sync::{Arc, Mutex};

struct FakeGateway {
    online: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl NodeGateway for FakeGateway {
    async fn online(&self) -> Vec<String> {
        self.online.lock().unwrap().clone()
    }
    async fn dispatch(&self, _node_id: &str, _spec: DispatchSpec) -> Result<String> {
        Ok("d1".into())
    }
    async fn job(
        &self,
        _node_id: Option<&str>,
        _kind: &str,
        _payload_json: &str,
        _needs: JobNeeds,
        _timeout_ms: i64,
    ) -> Result<JobOutcome> {
        anyhow::bail!("no jobs here")
    }
}

const NODE: &str = "n1";

struct T {
    core: Core,
    gw: Arc<FakeGateway>,
    _dir: tempfile::TempDir,
}

fn full_inventory(level: &str, shell: bool) -> Inventory {
    Inventory {
        at_ms: now_ms(),
        settings_sha256: "s1".into(),
        manage: HostManage {
            host_actions: KINDS.iter().map(|(k, _)| k.to_string()).collect(),
            manage_level: level.into(),
            manage_shell: shell,
        },
        products: vec![ProductStatus {
            product: "sensor".into(),
            state: "absent".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn setup(level: &str) -> T {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    core.store()
        .insert_node(NODE, "build-01", &Default::default(), "serial", "tok")
        .await
        .unwrap();
    let gw = Arc::new(FakeGateway {
        online: Mutex::new(vec![NODE.into()]),
    });
    core.set_gateway(gw.clone());
    report_inventory(&core, NODE, &full_inventory(level, true))
        .await
        .unwrap();
    T {
        core,
        gw,
        _dir: dir,
    }
}

/// Params that pass the Keleustes-side checks for each kind.
fn params_for(kind: &str) -> Vec<(&'static str, &'static str)> {
    match kind {
        "product_probe" | "product_install" | "product_upgrade" | "product_uninstall" => {
            vec![("product", "sensor")]
        }
        "product_configure" => vec![("product", "package_firewall"), ("on", "npm")],
        "product_auth" => vec![("product", "cli")],
        "config_set" => vec![("key", "agents.max_workers"), ("value", "2")],
        _ => vec![],
    }
}

fn req(kind: &str, origin: Origin) -> ActionRequest {
    let mut r = ActionRequest::new(NODE, kind, origin);
    for (k, v) in params_for(kind) {
        r = r.param(k, v);
    }
    r
}

async fn ledger(core: &Core) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&kls_store::audit::LedgerQuery::default())
        .await
        .unwrap()
}

/// Simulates the node finishing a dry run with `sha`.
async fn finish_dry(core: &Core, id: &str, sha: &str) {
    apply_update(
        core,
        NODE,
        Update {
            action_id: id.into(),
            seq: 1,
            state: "done".into(),
            preview_json: r#"{"argv":["x"]}"#.into(),
            preview_sha256: sha.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

/// A real (non-dry) action of `kind` that passes every check.
async fn confirmed(core: &Core, kind: &str) -> Result<HostActionRow> {
    let class = class_of(kind).unwrap();
    let mut r = req(kind, Origin::owner("products"));
    if matches!(class, Class::Provision | Class::Shell) {
        let dry = request_action(core, r.clone().dry()).await?;
        finish_dry(core, &dry.id, "p1").await;
        r.preview_sha256 = "p1".into();
        r.confirm_name = "build-01".into();
    }
    request_action(core, r).await
}

#[tokio::test]
async fn every_kind_queues_with_origin_and_audit() {
    let t = setup("provision").await;
    for (kind, _) in KINDS {
        let row = confirmed(&t.core, kind)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(row.state, "queued", "{kind}");
        let o = row.origin();
        assert_eq!(
            (o.actor.as_str(), o.surface.as_str()),
            ("owner", "fleet.host/products")
        );
        let stored = t.core.store().host_action(&row.id).await.unwrap().unwrap();
        assert_eq!(stored.origin(), o, "{kind}: provenance stored");
    }
    let entries = ledger(&t.core).await;
    for (kind, _) in KINDS {
        assert!(
            entries.iter().any(|e| e.kind == "host_action"
                && e.msg.contains(&format!("host.action.requested {kind} "))
                && e.msg.contains("owner@fleet.host/products")),
            "{kind}: audit entry with origin"
        );
    }
}

#[tokio::test]
async fn kill_switch_refuses_every_change_and_audits() {
    let t = setup("provision").await;
    control::stop(&t.core, "incident").await.unwrap();
    for (kind, class) in KINDS {
        let r = req(kind, Origin::owner("products"));
        let res = request_action(&t.core, r).await;
        if class == Class::Read {
            // O3: a person's click may still read, audited as stopped.
            let row = res.unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(row.state, "queued");
        } else {
            let e = res.unwrap_err();
            assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch", "{kind}");
        }
    }
    // An automatic read (a plan) is refused too.
    let mut auto = req("preflight", Origin::owner("x"));
    auto.origin.surface = "plan:p1".into();
    auto.origin.plan_id = "p1".into();
    let e = request_action(&t.core, auto).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");

    let entries = ledger(&t.core).await;
    for (kind, class) in KINDS {
        if class.mutating() {
            assert!(
                entries
                    .iter()
                    .any(|e| e.msg.contains(&format!("host.action.refused {kind} "))
                        && e.payload["reason"] == "kill_switch"),
                "{kind}: refusal audited"
            );
        } else {
            assert!(
                entries.iter().any(
                    |e| e.msg.contains(&format!("host.action.requested {kind} "))
                        && e.payload["control"] == "stopped"
                ),
                "{kind}: read audited with control stopped"
            );
        }
    }
    let evs = t.core.store().recent_events(500).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == kinds::ACTION_REFUSED));
    assert!(evs.iter().any(|e| e.kind == "factory.refused"));
}

#[tokio::test]
async fn pause_refuses_changes_too() {
    let t = setup("provision").await;
    control::pause(&t.core, "look").await.unwrap();
    let e = confirmed(&t.core, "config_set").await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");
}

#[tokio::test]
async fn queued_action_refused_at_delivery_after_stop() {
    let t = setup("provision").await;
    let row = confirmed(&t.core, "repo_trust").await.unwrap();
    let read = request_action(&t.core, req("preflight", Origin::owner("diagnostics")))
        .await
        .unwrap();
    control::stop(&t.core, "now").await.unwrap();
    assert!(!may_deliver(&t.core, &row).await.unwrap());
    let r = t.core.store().host_action(&row.id).await.unwrap().unwrap();
    assert_eq!(r.state, "refused");
    assert!(
        may_deliver(&t.core, &read).await.unwrap(),
        "clicked read leaves"
    );
    assert!(
        ledger(&t.core)
            .await
            .iter()
            .any(|e| e.payload["actionId"] == row.id && e.payload["outcome"] == "refused")
    );
}

#[tokio::test]
async fn a2a_blackout_freezes_a2a_and_sync_keys() {
    let t = setup("provision").await;
    t.core.set_a2a_blackout(true, "quiet").await.unwrap();
    for key in ["a2a.enabled", "sync.remote_prompts", "sync.hooks"] {
        let r = ActionRequest::new(NODE, "config_set", Origin::owner("config"))
            .param("key", key)
            .param("value", "off");
        let e = request_action(&t.core, r).await.unwrap_err();
        assert_eq!(refusal_of(&e).unwrap().code(), "a2a_blackout", "{key}");
    }
    let ok = ActionRequest::new(NODE, "config_set", Origin::owner("config"))
        .param("key", "agents.max_workers")
        .param("value", "2");
    request_action(&t.core, ok).await.unwrap();
    let entries = ledger(&t.core).await;
    assert!(
        entries
            .iter()
            .any(|e| e.payload["reason"] == "a2a_blackout")
    );
    let evs = t.core.store().recent_events(200).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == "a2a.withheld"));
}

#[tokio::test]
async fn only_the_console_manages_a_host() {
    let t = setup("provision").await;
    for actor in ["agent:belai:pr-reviewer", "node:n2", ""] {
        for (kind, _) in KINDS {
            let mut r = req(kind, Origin::owner("products"));
            r.origin.actor = actor.into();
            let e = request_action(&t.core, r).await.unwrap_err();
            assert_eq!(
                refusal_of(&e).unwrap().code(),
                "not_console",
                "{actor} {kind}"
            );
        }
    }
    let mut peer = req("preflight", Origin::owner("x"));
    peer.origin.surface = "fleet.request".into();
    let e = request_action(&t.core, peer).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_console");
    for (kind, _) in KINDS {
        assert!(is_host_kind(kind));
    }
    assert!(is_host_kind("host_anything") && !is_host_kind("session"));
}

#[tokio::test]
async fn unknown_kind_offline_not_offered_and_gate() {
    let t = setup("read").await;
    let e = request_action(&t.core, req("rm_rf", Origin::owner("x")))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "unknown_kind");

    let e = confirmed(&t.core, "config_set").await.unwrap_err();
    assert_eq!(
        refusal_of(&e).unwrap(),
        &Refusal::Gate {
            need: "config".into(),
            have: "read".into()
        }
    );
    request_action(&t.core, req("preflight", Origin::owner("diagnostics")))
        .await
        .unwrap();

    let mut inv = full_inventory("provision", false);
    inv.manage.host_actions.retain(|k| k != "log_tail");
    report_inventory(&t.core, NODE, &inv).await.unwrap();
    let e = request_action(&t.core, req("log_tail", Origin::owner("diagnostics")))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_offered");
    let e = confirmed(&t.core, "shell_exec").await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "shell_off");

    report_inventory(&t.core, NODE, &full_inventory("off", false))
        .await
        .unwrap();
    let e = request_action(&t.core, req("preflight", Origin::owner("d")))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "host_gate");

    t.gw.online.lock().unwrap().clear();
    let e = request_action(&t.core, req("preflight", Origin::owner("d")))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "offline");
    // Every refusal is a stored row.
    let rows = t.core.store().host_actions(NODE, 100).await.unwrap();
    assert!(rows.iter().filter(|r| r.state == "refused").count() >= 5);
}

#[tokio::test]
async fn provision_needs_typed_name_and_current_preview() {
    let t = setup("provision").await;
    let base = req("product_install", Origin::owner("products"));
    let e = request_action(&t.core, base.clone()).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "confirm");

    let mut typed = base.clone();
    typed.confirm_name = "build-01".into();
    typed.preview_sha256 = "nope".into();
    let e = request_action(&t.core, typed.clone()).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "preview_stale");

    let dry = request_action(&t.core, base.clone().dry()).await.unwrap();
    finish_dry(&t.core, &dry.id, "abc").await;
    // Different params than the preview: stale.
    let mut other = typed.clone();
    other.preview_sha256 = "abc".into();
    other.params.insert("product".into(), "cli".into());
    let e = request_action(&t.core, other).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "preview_stale");

    let row = confirm(&t.core, &dry.id, "build-01", Origin::owner("products"))
        .await
        .unwrap();
    assert_eq!(
        (row.state.as_str(), row.preview_sha256.as_str()),
        ("queued", "abc")
    );
    let e = confirm(&t.core, &dry.id, "wrong", Origin::owner("products"))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "confirm");
}

#[tokio::test]
async fn config_set_against_old_settings_is_refused() {
    let t = setup("config").await;
    let mut r = req("config_set", Origin::owner("config"));
    r.if_match = "old".into();
    let e = request_action(&t.core, r.clone()).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "settings_changed");
    r.if_match = "s1".into();
    request_action(&t.core, r).await.unwrap();
    let row = request_action(&t.core, req("config_set", Origin::owner("config")))
        .await
        .unwrap();
    assert_eq!(row.if_match, "s1", "if_match defaults to the inventory's");
}

#[tokio::test]
async fn updates_must_be_addressed_and_monotonic() {
    let t = setup("provision").await;
    let row = request_action(&t.core, req("preflight", Origin::owner("d")))
        .await
        .unwrap();
    let u = Update {
        action_id: row.id.clone(),
        seq: 1,
        state: "accepted".into(),
        ..Default::default()
    };
    let e = apply_update(&t.core, "n2", u.clone()).await.unwrap_err();
    assert_eq!(
        e.downcast_ref::<PatchRefusal>(),
        Some(&PatchRefusal::NotAddressed)
    );
    apply_update(&t.core, NODE, u.clone()).await.unwrap();
    let e = apply_update(&t.core, NODE, u).await.unwrap_err();
    assert!(matches!(
        e.downcast_ref::<PatchRefusal>(),
        Some(PatchRefusal::Stale { .. })
    ));
    let done = Update {
        action_id: row.id.clone(),
        seq: 2,
        state: "done".into(),
        result_json: r#"{"checks":[]}"#.into(),
        ..Default::default()
    };
    let r = apply_update(&t.core, NODE, done).await.unwrap();
    assert_eq!(r.state, "done");
    let entries = ledger(&t.core).await;
    assert!(
        entries
            .iter()
            .any(|e| e.actor_kind == "host" && e.msg.contains("host.action.accepted preflight"))
    );
    assert!(
        entries
            .iter()
            .any(|e| e.msg.contains("host.action.finished preflight"))
    );
}

#[tokio::test]
async fn host_side_refusal_is_audited_as_host() {
    let t = setup("provision").await;
    let row = confirmed(&t.core, "repo_trust").await.unwrap();
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: row.id.clone(),
            seq: 1,
            state: "refused".into(),
            error: "path outside workspace_dirs".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(ledger(&t.core).await.iter().any(|e| e.actor_kind == "host"
        && e.payload["outcome"] == "refused"
        && e.payload["actionId"] == row.id));
}

#[tokio::test]
async fn sensor_handoff_settles_only_when_probe_confirms() {
    let t = setup("provision").await;
    let row = confirmed(&t.core, "product_install").await.unwrap();
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: row.id.clone(),
            seq: 1,
            state: "needs_human".into(),
            handoff_command: "sudo vulnetix-sensor enroll --from-cli-login".into(),
            handoff_reason: "needs_root".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let n = needs(&t.core).await.unwrap();
    assert!(
        n.iter()
            .any(|x| x.kind == "handoff" && x.command.starts_with("sudo "))
    );
    // HM-14: the needs-you queue carries it, with the command and the link.
    let q = t.core.needs_you().await.unwrap();
    let c = q
        .iter()
        .find(|c| c.key == format!("host:{}", row.id))
        .expect("hand-off in needs you");
    assert_eq!(c.kind, crate::needs::CheckpointKind::HostHandoff);
    assert!(c.reason.starts_with("sudo vulnetix-sensor enroll"));
    assert_eq!(c.url, format!("keleustes://fleet/{NODE}/overview"));

    // Done: Keleustes re-probes; the hand-off settles on the probe.
    let check = handoff_done(&t.core, &row.id, Origin::owner("overview"))
        .await
        .unwrap();
    assert_eq!(check.kind, "product_probe");
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: check.id.clone(),
            seq: 1,
            state: "done".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut inv = full_inventory("provision", true);
    inv.products[0].state = "active".into();
    report_inventory(&t.core, NODE, &inv).await.unwrap();
    let r = t.core.store().host_action(&row.id).await.unwrap().unwrap();
    assert_eq!(r.state, "done");
    assert!(
        needs(&t.core)
            .await
            .unwrap()
            .iter()
            .all(|x| x.action_id != row.id)
    );
}

#[tokio::test]
async fn inventory_event_only_on_change() {
    let t = setup("read").await;
    let inv = full_inventory("read", true);
    let mut inv2 = inv.clone();
    inv2.at_ms = 0;
    report_inventory(&t.core, NODE, &inv2).await.unwrap();
    let count =
        |evs: &[kls_store::Event]| evs.iter().filter(|e| e.kind == kinds::INVENTORY).count();
    let before = count(&t.core.store().recent_events(500).await.unwrap());
    assert!(!report_inventory(&t.core, NODE, &inv2).await.unwrap());
    assert_eq!(
        count(&t.core.store().recent_events(500).await.unwrap()),
        before
    );
}

#[tokio::test]
async fn plan_runs_steps_and_stops_on_kill_switch() {
    let t = setup("provision").await;
    let steps = vec![
        PlanStep {
            kind: "repo_trust".into(),
            params: [("path".to_string(), "~/src/api".to_string())].into(),
            label: "Trust".into(),
        },
        PlanStep {
            kind: "config_set".into(),
            params: [
                ("key".to_string(), "agents.max_workers".to_string()),
                ("value".to_string(), "4".to_string()),
            ]
            .into(),
            label: "Workers".into(),
        },
    ];
    let p = save_plan(&t.core, "", "Baseline", &steps).await.unwrap();
    let previews = preview_plan(&t.core, &p.id, NODE, Origin::owner("provision"))
        .await
        .unwrap();
    assert!(
        previews
            .iter()
            .all(|r| r.dry_run && r.origin().plan_id == p.id)
    );
    for (i, r) in previews.iter().enumerate() {
        finish_dry(&t.core, &r.id, &format!("h{i}")).await;
    }
    let ids: Vec<String> = previews.iter().map(|r| r.id.clone()).collect();
    let run = run_plan(
        &t.core,
        &p.id,
        NODE,
        "build-01",
        &ids,
        Origin::owner("provision"),
    )
    .await
    .unwrap();

    // Step 1 is queued; the node finishes it, then the switch flips.
    let step1 = wait_for_step(&t.core, &run, 1).await;
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: step1.id.clone(),
            seq: 1,
            state: "done".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    // Step 2 is in flight (queued, unanswered) when the switch flips: the
    // plan must notice without waiting for the step timeout.
    let step2 = wait_for_step(&t.core, &run, 2).await;
    assert_eq!(step2.state, "queued");
    control::stop(&t.core, "incident").await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let r = loop {
        let r = t.core.store().host_plan_run(&run).await.unwrap().unwrap();
        if r.state != "running" {
            break r;
        }
        assert!(tokio::time::Instant::now() < deadline, "plan did not stop");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!((r.state.as_str(), r.step), ("refused", 2));
    let s2 = t
        .core
        .store()
        .host_action(&step2.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s2.state, "cancelled", "the in-flight step never leaves");
    let acts = t.core.store().host_actions_of_run(&run).await.unwrap();
    assert!(
        acts.iter().all(|a| a.state != "queued"),
        "no step left queued"
    );
    assert!(
        ledger(&t.core)
            .await
            .iter()
            .any(|e| e.kind == "host_plan" && e.msg.contains("host.plan.finished"))
    );
}

async fn wait_for_step(core: &Core, run: &str, step: i64) -> HostActionRow {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let acts = core.store().host_actions_of_run(run).await.unwrap();
        if let Some(a) = acts.into_iter().find(|a| a.step == step && !a.dry_run) {
            return a;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "step {step} never queued"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn plan_run_refused_while_stopped_and_without_previews() {
    let t = setup("provision").await;
    let p = save_plan(&t.core, "", "P", &baseline_steps())
        .await
        .unwrap();
    let e = run_plan(
        &t.core,
        &p.id,
        NODE,
        "build-01",
        &[],
        Origin::owner("provision"),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "preview_stale");
    control::stop(&t.core, "x").await.unwrap();
    let e = run_plan(
        &t.core,
        &p.id,
        NODE,
        "build-01",
        &[],
        Origin::owner("provision"),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");
    assert!(
        save_plan(
            &t.core,
            "",
            "Bad",
            &[PlanStep {
                kind: "nope".into(),
                ..Default::default()
            }]
        )
        .await
        .is_err()
    );
}

#[test]
fn repo_links_only_for_clean_owner_and_name() {
    assert_eq!(
        repo_link("acme", "api").as_deref(),
        Some("keleustes://repos/acme/api")
    );
    assert_eq!(repo_link("", "api"), None);
    assert_eq!(repo_link("acme", "a/b"), None);
    assert_eq!(repo_link("ac me", "api"), None);
}

#[test]
fn reasons_are_capped_and_clean() {
    let o = clean_origin(Origin {
        reason: format!("a\u{7}{}", "x".repeat(500)),
        ..Default::default()
    });
    assert_eq!(o.reason.chars().count(), MAX_REASON);
    assert!(!o.reason.contains('\u{7}'));
}

#[tokio::test]
async fn restart_on_the_wrong_version_is_flagged_once() {
    let t = setup("provision").await;
    let base = ActionRequest::new(NODE, "rc_restart", Origin::owner("provision"))
        .param("force_after_s", "300")
        .param("expect_version", "v0.44.0");
    let dry = request_action(&t.core, base.clone().dry()).await.unwrap();
    finish_dry(&t.core, &dry.id, "r1").await;
    let row = confirm(&t.core, &dry.id, "build-01", Origin::owner("provision"))
        .await
        .unwrap();
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: row.id.clone(),
            seq: 1,
            state: "done".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let count = |evs: &[kls_store::Event]| {
        evs.iter()
            .filter(|e| e.kind == kinds::VERSION_MISMATCH)
            .count()
    };
    let mut inv = full_inventory("provision", true);
    inv.belai.version = "0.44.0".into();
    report_inventory(&t.core, NODE, &inv).await.unwrap();
    assert_eq!(count(&t.core.store().recent_events(500).await.unwrap()), 0);

    inv.belai.version = "0.43.0".into();
    report_inventory(&t.core, NODE, &inv).await.unwrap();
    inv.at_ms += 1;
    report_inventory(&t.core, NODE, &inv).await.unwrap();
    assert_eq!(count(&t.core.store().recent_events(500).await.unwrap()), 1);
    assert!(
        ledger(&t.core)
            .await
            .iter()
            .any(|e| e.payload["actionId"] == row.id
                && e.payload["outcome"] == "version_mismatch"
                && e.payload["running"] == "0.43.0")
    );
}

// ---- HM-10 schedules, HM-11 library ------------------------------------------------

const RC: &str = "rc1";

/// A node that takes worker and library dispatches, with a profile and a
/// directory, online on the fake gateway.
async fn rc_node(t: &T) {
    let host = kls_store::HostInfo {
        hostname: "rc1".into(),
        capabilities: kls_store::Capabilities {
            remote_control: true,
            dispatch_kinds: vec![
                "worker".into(),
                "library_install".into(),
                "library_backup".into(),
                "library_scan".into(),
                "library_import".into(),
            ],
            inventory: vec![kls_store::InventoryRef {
                kind: "skill".into(),
                name: "release-notes".into(),
                sha256: "abc".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        agents: vec!["belai:scout".into()],
        trusted_dirs: vec!["/work".into()],
        ..Default::default()
    };
    t.core
        .store()
        .insert_node(RC, "rc1", &host, "serial", "tok")
        .await
        .unwrap();
    t.gw.online.lock().unwrap().push(RC.into());
}

fn sched() -> ScheduleRow {
    ScheduleRow {
        node_id: RC.into(),
        profile: "belai:scout".into(),
        cron: "0 3 * * *".into(),
        cwd: "/work".into(),
        enabled: true,
        ..Default::default()
    }
}

fn skill() -> crate::gateway::LibraryRef {
    crate::gateway::LibraryRef {
        kind: "skill".into(),
        name: "release-notes".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn schedules_save_retime_delete_with_origin_and_audit() {
    let t = setup("provision").await;
    rc_node(&t).await;
    let s = save_schedule(&t.core, sched(), Origin::owner("schedules"))
        .await
        .unwrap();
    let mut re = s.clone();
    re.cron = "0 4 * * *".into();
    let re = save_schedule(&t.core, re, Origin::owner("schedules"))
        .await
        .unwrap();
    assert_eq!((re.cron.as_str(), re.version), ("0 4 * * *", 2));
    let mut del = re.clone();
    del.deleted = true;
    save_schedule(&t.core, del, Origin::owner("schedules"))
        .await
        .unwrap();
    let v = view(&t.core, RC).await.unwrap();
    assert!(v.schedules.is_empty(), "deleted schedules are hidden");
    let entries = ledger(&t.core).await;
    for ev in ["host.schedule.saved", "host.schedule.deleted"] {
        assert!(
            entries.iter().any(|e| e.kind == "host_schedule"
                && e.msg.contains(ev)
                && e.msg.contains("owner@fleet.host/schedules")),
            "{ev} audited with origin"
        );
    }
    // The WP-G checks still apply: a directory the node does not offer.
    let mut bad = sched();
    bad.cwd = "/etc".into();
    assert!(
        save_schedule(&t.core, bad, Origin::owner("schedules"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn schedule_changes_and_run_now_refused_under_stop_and_from_agents() {
    let t = setup("provision").await;
    rc_node(&t).await;
    let s = save_schedule(&t.core, sched(), Origin::owner("schedules"))
        .await
        .unwrap();
    let mut agent = Origin::owner("schedules");
    agent.actor = "agent:belai:scout".into();
    let e = save_schedule(&t.core, sched(), agent.clone())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_console");
    let e = run_schedule_now(&t.core, &s, agent).await.unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_console");

    let id = run_schedule_now(&t.core, &s, Origin::owner("schedules"))
        .await
        .unwrap();
    assert!(!id.is_empty());

    control::stop(&t.core, "incident").await.unwrap();
    let mut off = s.clone();
    off.enabled = false;
    let e = save_schedule(&t.core, off, Origin::owner("schedules"))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");
    let e = run_schedule_now(&t.core, &s, Origin::owner("schedules"))
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");
    let entries = ledger(&t.core).await;
    assert!(entries.iter().any(|e| e.kind == "host_schedule"
        && e.payload["outcome"] == "refused"
        && e.payload["reason"] == "kill_switch"));
    let evs = t.core.store().recent_events(500).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == kinds::CHANGE_REFUSED));
    assert!(evs.iter().any(|e| e.kind == "factory.refused"));
}

#[tokio::test]
async fn library_dispatch_rules() {
    let t = setup("provision").await;
    rc_node(&t).await;
    let o = || Origin::owner("library");
    // Install and scan go out, audited with their origin.
    library_dispatch(&t.core, RC, "library_install", vec![skill()], false, o())
        .await
        .unwrap();
    library_dispatch(&t.core, RC, "library_scan", vec![], false, o())
        .await
        .unwrap();
    // Hooks and MCP servers run code: a second, confirmed click.
    let hook = crate::gateway::LibraryRef {
        kind: "hook".into(),
        name: "pre-commit".into(),
        ..Default::default()
    };
    let e = library_dispatch(
        &t.core,
        RC,
        "library_install",
        vec![hook.clone()],
        false,
        o(),
    )
    .await
    .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "confirm");
    library_dispatch(&t.core, RC, "library_install", vec![hook], true, o())
        .await
        .unwrap();
    // Unknown kinds, a node that does not take the kind, agents.
    let e = library_dispatch(&t.core, RC, "library_wipe", vec![], false, o())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "unknown_kind");
    let e = library_dispatch(&t.core, NODE, "library_install", vec![skill()], false, o())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_offered");
    let mut agent = o();
    agent.actor = "node:n2".into();
    let e = library_dispatch(&t.core, RC, "library_install", vec![skill()], false, agent)
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "not_console");

    // A2A blackout: no agent, crew or MCP install; a skill still goes.
    t.core.set_a2a_blackout(true, "quiet").await.unwrap();
    let profile = crate::gateway::LibraryRef {
        kind: "profile".into(),
        name: "belai:scout".into(),
        ..Default::default()
    };
    let e = library_dispatch(&t.core, RC, "library_install", vec![profile], false, o())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "a2a_blackout");
    library_dispatch(&t.core, RC, "library_install", vec![skill()], false, o())
        .await
        .unwrap();
    t.core.set_a2a_blackout(false, "").await.unwrap();

    // Kill switch: nothing goes, and the refusal is audited.
    control::stop(&t.core, "incident").await.unwrap();
    for kind in LIBRARY_DISPATCHES {
        let e = library_dispatch(&t.core, RC, kind, vec![skill()], true, o())
            .await
            .unwrap_err();
        assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch", "{kind}");
    }
    let e = library_import(&t.core, RC, "scan-1", 0, o())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&e).unwrap().code(), "kill_switch");

    let entries = ledger(&t.core).await;
    assert!(entries.iter().any(|e| e.kind == "host_library"
        && e.payload["outcome"] == "dispatched"
        && e.msg.contains("owner@fleet.host/library")));
    for code in ["confirm", "a2a_blackout", "kill_switch", "not_console"] {
        assert!(
            entries
                .iter()
                .any(|e| e.kind == "host_library" && e.payload["reason"] == code),
            "{code} refusal audited"
        );
    }
    let evs = t.core.store().recent_events(500).await.unwrap();
    assert!(evs.iter().any(|e| e.kind == "a2a.withheld"));
}

#[tokio::test]
async fn library_view_reconciles_what_the_host_holds() {
    let t = setup("provision").await;
    rc_node(&t).await;
    let v = view(&t.core, RC).await.unwrap();
    assert_eq!(v.library.held.len(), 1);
    assert_eq!(v.library.held[0].verdict, "push", "the library lacks it");
}

#[test]
fn fleet_rows_count_as_the_console() {
    let o = Origin::fleet("library");
    assert!(o.is_console() && o.is_click());
    let mut a = o.clone();
    a.actor = "agent:x".into();
    assert!(!a.is_console());
}

// ---- P4: link facts, scrubbed progress, rename and revoke, run steps ---------------

#[test]
fn progress_lines_lose_escapes_controls_and_secrets() {
    let raw =
        "\u{1b}[31mred\u{1b}[0m \u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7} \u{202e}rtl\u{7}\r ok";
    assert_eq!(scrub_line(raw), "red link rtl ok");
    let s = scrub_line("token=abc123 ghp_0123456789abcdefghij --api_key=zz plain=1 sk-short");
    assert_eq!(s, "[redacted] [redacted] [redacted] plain=1 sk-short");
}

#[tokio::test]
async fn stored_lines_are_scrubbed() {
    let t = setup("provision").await;
    let row = confirmed(&t.core, "preflight").await.unwrap();
    apply_update(
        &t.core,
        NODE,
        Update {
            action_id: row.id.clone(),
            seq: 1,
            state: "running".into(),
            pct: 40,
            lines: vec!["\u{1b}[1mstep\u{1b}[0m password=hunter22".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lines = lines(&t.core, &row.id).await.unwrap();
    assert_eq!(lines[0].line, "step [redacted]");
}

#[tokio::test]
async fn link_facts_are_stamped_and_not_a_change() {
    let t = setup("provision").await;
    let mut inv = full_inventory("provision", true);
    inv.at_ms = now_ms() + 2_000;
    // A node cannot set its own link facts.
    inv.link.cert_not_after_ms = 1;
    report_inventory_link(&t.core, NODE, &inv, now_ms() + 86_400_000 * 20)
        .await
        .unwrap();
    let (_, got) = inventory(&t.core, NODE).await.unwrap().unwrap();
    assert!(got.link.received_at_ms > 0);
    let left = got.cert_left_ms(now_ms()).unwrap();
    assert!(left > 86_400_000 * 19 && left <= 86_400_000 * 20, "{left}");
    let skew = got.clock_skew_ms().unwrap();
    assert!((1_000..=2_100).contains(&skew), "{skew}");
    // The same inventory again, with new link facts, is not a change.
    assert!(
        !report_inventory_link(&t.core, NODE, &inv, now_ms() + 1)
            .await
            .unwrap()
    );
    let mut inv0 = inv.clone();
    inv0.at_ms = 0;
    assert_eq!(
        Inventory {
            at_ms: 0,
            ..Default::default()
        }
        .clock_skew_ms(),
        None
    );
    assert!(inv0.cert_left_ms(now_ms()).is_some_and(|l| l < 0));
}

#[test]
fn rtt_is_the_fastest_settled_round_trip() {
    let row = |state: &str, d: Option<i64>, f: Option<i64>| HostActionRow {
        state: state.into(),
        delivered_at: d,
        finished_at: f,
        ..Default::default()
    };
    assert_eq!(link_rtt_ms(&[]), None);
    let acts = [
        row("done", Some(100), Some(180)),
        row("failed", Some(100), Some(130)),
        row("running", Some(100), None),
        row("done", None, Some(5)),
        row("done", Some(200), Some(150)),
    ];
    assert_eq!(link_rtt_ms(&acts), Some(30));
}

#[tokio::test]
async fn rename_is_audited_with_origin_and_refused_under_stop() {
    let t = setup("provision").await;
    let mut o = Origin::owner("overview");
    o.reason = "naming scheme".into();
    rename_node(&t.core, NODE, "  build-02 ", o.clone())
        .await
        .unwrap();
    assert_eq!(
        t.core.store().node(NODE).await.unwrap().unwrap().name,
        "build-02"
    );
    let e = ledger(&t.core).await;
    let hit = e
        .iter()
        .find(|e| e.msg.contains("host.node.renamed") && e.payload["outcome"] == "renamed")
        .expect("rename audited");
    assert_eq!(hit.payload["from"], "build-01");
    assert_eq!(hit.payload["to"], "build-02");
    assert_eq!(hit.payload["origin"]["reason"], "naming scheme");
    assert_eq!(hit.payload["origin"]["surface"], "fleet.host/overview");

    // Bad names and non-console origins are refused and audited.
    let err = rename_node(&t.core, NODE, "  ", o.clone())
        .await
        .unwrap_err();
    assert_eq!(refusal_of(&err).unwrap().code(), "invalid");
    let mut agent = o.clone();
    agent.actor = "agent:x".into();
    let err = rename_node(&t.core, NODE, "x", agent).await.unwrap_err();
    assert_eq!(refusal_of(&err).unwrap().code(), "not_console");

    // Rule 3: refused (and audited) under the kill switch.
    control::stop(&t.core, "incident").await.unwrap();
    let err = rename_node(&t.core, NODE, "build-03", o).await.unwrap_err();
    assert_eq!(refusal_of(&err).unwrap().code(), "kill_switch");
    assert_eq!(
        t.core.store().node(NODE).await.unwrap().unwrap().name,
        "build-02"
    );
    let e = ledger(&t.core).await;
    assert!(e.iter().any(|e| e.msg.contains("host.node.renamed")
        && e.payload["outcome"] == "refused"
        && e.payload["reason"] == "kill_switch"));
}

#[tokio::test]
async fn revoke_is_audited_with_origin_even_when_stopped() {
    let t = setup("provision").await;
    control::stop(&t.core, "incident").await.unwrap();
    let mut o = Origin::owner("overview");
    o.reason = "lost laptop".into();
    record_revoke(&t.core, NODE, true, o).await.unwrap();
    let e = ledger(&t.core).await;
    let hit = e
        .iter()
        .find(|e| e.msg.contains("host.node.revoked"))
        .expect("revoke audited");
    assert_eq!(hit.payload["outcome"], "revoked");
    assert_eq!(hit.payload["origin"]["reason"], "lost laptop");
}

#[test]
fn run_actions_are_the_runs_real_steps_in_order() {
    let a = |id: &str, run: &str, step: i64, dry: bool| HostActionRow {
        id: id.into(),
        plan_run_id: run.into(),
        step,
        dry_run: dry,
        ..Default::default()
    };
    let v = HostView {
        actions: vec![
            a("s2", "r1", 2, false),
            a("p1", "r1", 1, true),
            a("s1", "r1", 1, false),
            a("o1", "r2", 1, false),
        ],
        ..Default::default()
    };
    let ids: Vec<String> = v.run_actions("r1").into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["s1", "s2"]);
}

#[test]
fn teleport_surfaces_count_as_the_console() {
    let o = Origin {
        actor: "owner".into(),
        surface: "teleport:t1".into(),
        ..Default::default()
    };
    assert!(o.is_console() && !o.is_click());
}

#[test]
fn telemetry_values_are_checked_before_they_leave() {
    let ok = |k: &str, v: &str| telemetry_value(k, v).is_ok();
    assert!(ok(
        "telemetry.otlp_endpoint",
        "https://otel.example.com:4318"
    ));
    assert!(ok("telemetry.otlp_endpoint", "http://localhost:4318/otlp"));
    assert!(ok("telemetry.otlp_endpoint", ""));
    assert!(!ok("telemetry.otlp_endpoint", "ftp://x"));
    assert!(!ok("telemetry.otlp_endpoint", "https://u:p@x.example"));
    assert!(!ok("telemetry.otlp_endpoint", "https://x.example/?k=v"));
    assert!(!ok("telemetry.otlp_endpoint", "https://x.example/#f"));
    assert!(ok("telemetry.traces", "false"));
    assert!(!ok("telemetry.metrics", "off"));
    assert!(ok("telemetry.headers.x-api-key", "env:OTEL_API_KEY"));
    assert!(ok("telemetry.headers.x-api-key", ""));
    // A secret value never goes through Keleustes.
    assert!(!ok("telemetry.headers.x-api-key", "sk-live-123"));
    assert!(!ok("telemetry.headers.bad name", "env:A"));
    assert!(!ok("telemetry.sampler", "x"));
    // Other keys are the host's to check.
    assert!(ok("model", "anything"));
    let sync = Origin {
        actor: "owner".into(),
        surface: "library.sync".into(),
        ..Default::default()
    };
    assert!(sync.is_console() && !sync.is_click());
}
