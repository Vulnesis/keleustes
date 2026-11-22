//! WP5 rules 1 to 4: a save reaches its nodes through an audited
//! `library_install` with its origin, refused under the kill switch and the
//! A2A blackout; crews are checked in core; crew resume honours the
//! blackout; the avatar job is rate limited.

use super::*;
use crate::gateway::{JobOutcome, NodeGateway};
use crate::vault::CrewMember;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Gw {
    sent: Mutex<Vec<(String, DispatchSpec)>>,
    svg: String,
}

#[async_trait::async_trait]
impl NodeGateway for Gw {
    async fn online(&self) -> Vec<String> {
        vec!["n1".into()]
    }
    async fn dispatch(&self, node_id: &str, spec: DispatchSpec) -> Result<String> {
        let mut s = self.sent.lock().unwrap();
        s.push((node_id.into(), spec));
        Ok(format!("d{}", s.len()))
    }
    async fn job(
        &self,
        _node: Option<&str>,
        _kind: &str,
        payload: &str,
        _needs: JobNeeds,
        _timeout_ms: i64,
    ) -> Result<JobOutcome> {
        assert!(payload.contains("\"task\":\"avatar\""));
        Ok(JobOutcome {
            status: "ok".into(),
            output_json: json!({ "svg": self.svg }).to_string(),
            node_id: "n1".into(),
            ..Default::default()
        })
    }
}

async fn setup() -> (Core, Arc<Gw>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let host = kls_store::HostInfo {
        capabilities: kls_store::Capabilities {
            dispatch_kinds: [
                "worker",
                "pause",
                "resume",
                "stop",
                "crew_control",
                "library_install",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            inventory: vec![kls_store::InventoryRef {
                kind: "profile".into(),
                name: "patcher".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    core.store()
        .insert_node("n1", "build-01", &host, "serial", "tok")
        .await
        .unwrap();
    let gw = Arc::new(Gw {
        svg: r##"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="#123456"/></svg>"##.into(),
        ..Default::default()
    });
    core.set_gateway(gw.clone());
    (core, gw, dir)
}

fn agent(name: &str, meta: Value) -> AgentProfile {
    AgentProfile {
        name: name.into(),
        prompt: "fix things".into(),
        meta_json: meta.to_string(),
        ..Default::default()
    }
}

async fn ledger(core: &Core) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&Default::default()).await.unwrap()
}

fn origin() -> Origin {
    Origin::fleet("workforce")
}

#[tokio::test]
async fn saving_an_agent_installs_it_on_its_nodes() {
    let (core, gw, _d) = setup().await;
    let (a, p) = save_and_publish_agent(&core, &agent("patcher", json!({})), None, origin())
        .await
        .unwrap();
    // The node that holds it got a library_install naming the new version.
    assert_eq!(p.sent.len(), 1);
    assert!(p.sent[0].refused.is_empty(), "{:?}", p.sent);
    let sent = gw.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "n1");
    assert_eq!(sent[0].1.kind, "library_install");
    assert_eq!(sent[0].1.library[0].kind, "profile");
    assert_eq!(sent[0].1.library[0].name, a.name);
    assert_eq!(sent[0].1.library[0].version, p.version);
    // Provenance: the trigger names the surface; the ledger the origin.
    assert_eq!(
        sent[0].1.trigger.as_ref().unwrap().subject,
        "fleet/workforce"
    );
    let l = ledger(&core).await;
    assert!(l.iter().any(|e| e.kind == "host_library"
        && e.payload["outcome"] == "dispatched"
        && e.payload["origin"]["surface"] == "fleet/workforce"));
    assert!(l.iter().any(|e| e.kind == "workforce"
        && e.payload["itemId"] == p.item_id.as_str()
        && e.payload["origin"]["actor"] == "owner"));
    // The library holds it, so the node can fetch it.
    assert!(
        core.store()
            .library_item(&p.item_id)
            .await
            .unwrap()
            .is_some_and(|i| i.kind == AGENT_KIND)
    );
}

#[tokio::test]
async fn kill_switch_and_blackout_refuse_the_install() {
    let (core, gw, _d) = setup().await;
    control::stop(&core, "incident").await.unwrap();
    let (_, p) = save_and_publish_agent(&core, &agent("patcher", json!({})), None, origin())
        .await
        .unwrap();
    assert_eq!(p.sent[0].refused, "kill_switch");
    assert!(gw.sent.lock().unwrap().is_empty());
    let l = ledger(&core).await;
    assert!(l.iter().any(|e| e.kind == "host_library"
        && e.payload["outcome"] == "refused"
        && e.payload["reason"] == "kill_switch"));
    assert!(
        l.iter()
            .any(|e| e.kind == "workforce" && e.msg.contains("1 refused"))
    );

    control::resume(&core).await.unwrap();
    core.set_a2a_blackout(true, "quiet").await.unwrap();
    let a = core
        .store()
        .agent_profile_by_name("patcher")
        .await
        .unwrap()
        .unwrap();
    let p = publish_agent(&core, &a.id, None, origin()).await.unwrap();
    assert_eq!(p.sent[0].refused, "a2a_blackout");
    assert!(gw.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn crews_enter_the_library_with_their_members() {
    let (core, gw, _d) = setup().await;
    save_agent(&core, &agent("patcher", json!({})))
        .await
        .unwrap();
    let (c, p) = save_and_publish_crew(
        &core,
        &Crew {
            name: "fixers".into(),
            members: vec![CrewMember {
                agent: "patcher".into(),
                role: "fixer".into(),
                count: 2,
                model: String::new(),
            }],
            ..Default::default()
        },
        Some(&["n1".to_string()]),
        origin(),
    )
    .await
    .unwrap();
    let item = core
        .store()
        .library_item(&p.item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (item.kind.as_str(), item.name.as_str()),
        (CREW_KIND, c.name.as_str())
    );
    let sent = gw.sent.lock().unwrap().clone();
    let kinds: Vec<&str> = sent[0].1.library.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["crew", "profile"]);
}

#[tokio::test]
async fn an_invalid_crew_is_refused_in_core() {
    let (core, _gw, _d) = setup().await;
    let worker = |labels: &[&str], handoff: &[&str]| {
        json!({"mode": "worker", "kanban": {"lists": ["backlog"], "labels": labels,
               "handoff_labels": handoff}})
    };
    save_agent(&core, &agent("a1", worker(&["bug"], &[])))
        .await
        .unwrap();
    save_agent(&core, &agent("a2", worker(&["bug"], &[])))
        .await
        .unwrap();
    save_agent(&core, &agent("a3", worker(&["fix"], &["needs-review"])))
        .await
        .unwrap();
    let crew = |members: &[(&str, i64)]| Crew {
        name: "c".into(),
        members: members
            .iter()
            .map(|(a, n)| CrewMember {
                agent: a.to_string(),
                count: *n,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    for (bad, why) in [
        (crew(&[("a1", 1), ("a2", 1)]), "claim the same cards"),
        (crew(&[("a3", 1)]), "which no member claims"),
        (crew(&[("a1", 9)]), "replicas"),
        (crew(&[]), "at least one member"),
    ] {
        let e = save_crew(&core, &bad).await.unwrap_err();
        assert!(format!("{e:#}").contains(why), "{e:#}");
        assert!(matches!(
            e.downcast_ref::<crate::sessions::Refusal>(),
            Some(crate::sessions::Refusal::Invalid(_))
        ));
    }
    // A valid crew saves.
    save_crew(&core, &crew(&[("a1", 2)])).await.unwrap();
    assert!(core.store().crews().await.unwrap().len() == 1);
}

#[tokio::test]
async fn crew_resume_respects_the_a2a_blackout() {
    let (core, gw, _d) = setup().await;
    let all = |paused| Control::Crew {
        run: "r1".into(),
        crew: "fixers".into(),
        workers: vec!["w1".into(), "w2".into()],
        paused,
    };
    core.set_a2a_blackout(true, "quiet").await.unwrap();
    let e = worker_control(&core, "n1", all(false), origin())
        .await
        .unwrap_err();
    assert_eq!(hosts::refusal_of(&e).unwrap().code(), "a2a_blackout");
    assert!(gw.sent.lock().unwrap().is_empty());
    // Pausing holds work: it still goes out, one control per worker.
    let ids = worker_control(&core, "n1", all(true), origin())
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    let sent = gw.sent.lock().unwrap().clone();
    assert_eq!(sent[0].1.control.as_ref().unwrap().op, "worker_pause");
    let l = ledger(&core).await;
    assert!(l.iter().any(|e| e.kind == "workforce"
        && e.payload["outcome"] == "refused"
        && e.payload["reason"] == "a2a_blackout"));
    assert!(l.iter().any(|e| e.kind == "workforce"
        && e.payload["control"] == "crew_pause"
        && e.payload["origin"]["surface"] == "fleet/workforce"));
    let evs = core.store().recent_events(200).await.unwrap();
    assert!(
        evs.iter()
            .any(|e| e.kind == "a2a.withheld" || e.kind.contains("withheld"))
    );
}

/// #10: weight, scope and replicas edits go out as `crew_control` with
/// their fields, audited with the origin; out-of-range values, the kill
/// switch and the A2A blackout refuse, except edits that narrow work.
#[tokio::test]
async fn crew_scope_and_replica_edits() {
    let (core, gw, _d) = setup().await;
    let edit = |edit| Control::CrewEdit {
        run: "r1".into(),
        crew: "fixers".into(),
        edit,
    };
    worker_control(
        &core,
        "n1",
        edit(CrewEdit::Weight {
            dir: "/w/api".into(),
            weight: 7,
        }),
        origin(),
    )
    .await
    .unwrap();
    worker_control(
        &core,
        "n1",
        edit(CrewEdit::Replicas {
            profile: "patcher".into(),
            replicas: 3,
        }),
        origin(),
    )
    .await
    .unwrap();
    {
        let sent = gw.sent.lock().unwrap();
        let c = sent[0].1.control.clone().unwrap();
        assert_eq!(
            (c.op.as_str(), c.dir.as_str(), c.weight, c.run.as_str()),
            ("scope_weight", "/w/api", 7, "r1")
        );
        let c = sent[1].1.control.clone().unwrap();
        assert_eq!((c.op.as_str(), c.replicas), ("replicas", 3));
        assert_eq!(sent[0].1.trigger.as_ref().unwrap().source, "console");
    }
    // Out of range: refused in core.
    for bad in [
        CrewEdit::Weight {
            dir: "/w".into(),
            weight: 11,
        },
        CrewEdit::Replicas {
            profile: "patcher".into(),
            replicas: 9,
        },
        CrewEdit::AddDir {
            dir: "relative".into(),
            weight: 0,
        },
    ] {
        assert!(
            worker_control(&core, "n1", edit(bad), origin())
                .await
                .is_err()
        );
    }
    assert_eq!(gw.sent.lock().unwrap().len(), 2);
    // Kill switch: a weight change is refused, a pause of a directory is not.
    control::pause(&core, "drill").await.unwrap();
    let e = worker_control(
        &core,
        "n1",
        edit(CrewEdit::AddDir {
            dir: "/w/lib".into(),
            weight: 2,
        }),
        origin(),
    )
    .await
    .unwrap_err();
    assert_eq!(hosts::refusal_of(&e).unwrap().code(), "kill_switch");
    worker_control(
        &core,
        "n1",
        edit(CrewEdit::PauseDir {
            dir: "/w/api".into(),
            paused: true,
        }),
        origin(),
    )
    .await
    .unwrap();
    control::resume(&core).await.unwrap();
    // A2A blackout: a widening edit is refused.
    core.set_a2a_blackout(true, "quiet").await.unwrap();
    let e = worker_control(
        &core,
        "n1",
        edit(CrewEdit::Weight {
            dir: "/w/api".into(),
            weight: 3,
        }),
        origin(),
    )
    .await
    .unwrap_err();
    assert_eq!(hosts::refusal_of(&e).unwrap().code(), "a2a_blackout");
    assert_eq!(gw.sent.lock().unwrap().len(), 3);
    let l = ledger(&core).await;
    assert!(l.iter().any(|e| e.payload["control"] == "crew_scope_weight"
        && e.payload["edit"]["weight"] == 7
        && e.payload["origin"]["surface"] == "fleet/workforce"));
    assert!(
        l.iter()
            .any(|e| e.payload["control"] == "crew_scope_add"
                && e.payload["reason"] == "kill_switch")
    );
}

#[tokio::test]
async fn kill_switch_refuses_start_and_resume_but_not_pause() {
    let (core, gw, _d) = setup().await;
    control::pause(&core, "drill").await.unwrap();
    for c in [
        Control::Start {
            profile: "patcher".into(),
            cwd: "/w".into(),
        },
        Control::Resume {
            worker: "w1".into(),
        },
    ] {
        let e = worker_control(&core, "n1", c, origin()).await.unwrap_err();
        assert_eq!(hosts::refusal_of(&e).unwrap().code(), "kill_switch");
    }
    assert!(gw.sent.lock().unwrap().is_empty());
    worker_control(
        &core,
        "n1",
        Control::Pause {
            worker: "w1".into(),
        },
        origin(),
    )
    .await
    .unwrap();
    worker_control(
        &core,
        "n1",
        Control::Stop {
            session: "s1".into(),
        },
        origin(),
    )
    .await
    .unwrap();
    assert_eq!(gw.sent.lock().unwrap().len(), 2);
    let refused = ledger(&core)
        .await
        .into_iter()
        .filter(|e| e.kind == "workforce" && e.payload["reason"] == "kill_switch")
        .count();
    assert_eq!(refused, 2);
}

#[tokio::test]
async fn avatar_job_is_sanitised_rate_limited_and_stops_with_the_switch() {
    let (core, _gw, _d) = setup().await;
    let brief = AvatarBrief {
        display_name: "Patcher".into(),
        palette: vec!["#123456".into()],
        ..Default::default()
    };
    for _ in 0..AVATAR_PER_HOUR {
        let svg = draw_avatar(&core, "patcher", &brief, None, "owner", 1000)
            .await
            .unwrap();
        assert!(svg.starts_with("<svg"));
    }
    let e = draw_avatar(&core, "patcher", &brief, None, "owner", 1000)
        .await
        .unwrap_err();
    assert!(format!("{e}").contains("an hour"));
    assert_eq!(avatar_candidates(&core, "patcher").await.unwrap().len(), 1);
    assert!(
        ledger(&core)
            .await
            .iter()
            .any(|e| e.kind == "workforce" && e.msg.contains("avatar"))
    );
    control::stop(&core, "x").await.unwrap();
    assert!(
        draw_avatar(&core, "other", &brief, None, "owner", 1000)
            .await
            .unwrap_err()
            .downcast_ref::<control::Blocked>()
            .is_some()
    );
}

#[tokio::test]
async fn reconcile_answers_install_when_the_library_is_ahead() {
    let (core, _gw, _d) = setup().await;
    let a = save_agent(&core, &agent("patcher", json!({})))
        .await
        .unwrap();
    crate::vault::agent_to_library(&core, &a.id, "v1")
        .await
        .unwrap();
    let item = core
        .store()
        .library_item_by_name(AGENT_KIND, "patcher")
        .await
        .unwrap()
        .unwrap();
    let v1 = core
        .store()
        .library_latest(&item.id)
        .await
        .unwrap()
        .unwrap();
    let mut a2 = a.clone();
    a2.prompt = "fix more things".into();
    let a2 = save_agent(&core, &a2).await.unwrap();
    crate::vault::agent_to_library(&core, &a2.id, "v2")
        .await
        .unwrap();
    let held = |kind: &str, sha: String| LibraryRef {
        kind: kind.into(),
        name: "patcher".into(),
        sha256: sha,
        ..Default::default()
    };
    let sha = crate::belai::library::sha256_hex(v1.body.as_bytes());
    let v = core
        .library_reconcile(&[held("profile", sha.clone()), held("profile", "zz".into())])
        .await
        .unwrap();
    assert_eq!(v[0].1, "install");
    assert_eq!(v[1].1, "diverged");
    // Hooks and MCP servers never get "install". (Saved bodies are
    // validated, so these are real hook documents.)
    let hook = |cmd: &str| {
        format!(
            r#"{{"name":"patcher","hooks":{{"PreToolUse":[{{"matcher":"Bash","hooks":[{{"type":"command","command":"{cmd}"}}]}}]}}}}"#
        )
    };
    let (hid, _) = save_library(&core, "", "hook", "patcher", "", &hook("vulnetix one"), "")
        .await
        .unwrap();
    let one = core
        .store()
        .library_latest(&hid)
        .await
        .unwrap()
        .unwrap()
        .body;
    save_library(
        &core,
        &hid,
        "hook",
        "patcher",
        "",
        &hook("vulnetix two"),
        "",
    )
    .await
    .unwrap();
    let v = core
        .library_reconcile(&[held(
            "hook",
            crate::belai::library::sha256_hex(one.as_bytes()),
        )])
        .await
        .unwrap();
    assert_eq!(v[0].1, "diverged");
}
