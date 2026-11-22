//! P3 rules 1 to 3 for start fields and MCP bindings: the fields reach the
//! gateway, the trigger names the console, every outcome is audited, the
//! kill switch refuses, and an MCP value never reaches the store or the log.

use super::*;
use crate::gateway::{JobNeeds, JobOutcome, NodeGateway};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Gw {
    sent: Mutex<Vec<(String, DispatchSpec)>>,
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
        _: Option<&str>,
        _: &str,
        _: &str,
        _: JobNeeds,
        _: i64,
    ) -> Result<JobOutcome> {
        anyhow::bail!("no jobs")
    }
}

async fn setup() -> (Core, Arc<Gw>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let gw = Arc::new(Gw::default());
    core.set_gateway(gw.clone());
    (core, gw, dir)
}

fn origin() -> Origin {
    Origin {
        actor: "owner".into(),
        surface: "sessions/new".into(),
        ..Default::default()
    }
}

async fn ledger(core: &Core, kind: &str) -> Vec<kls_store::audit::LedgerEntry> {
    core.audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[test]
fn config_parses_and_validates() {
    let cfg = json!({
        "dirs": ["/work/a", "/work/b"], "workspace": "acme/api/fix", "effort": "High",
        "gitSync": false, "ask": "on", "stay": true, "stayIdleMaxS": 900,
        "clone": "acme/api@main",
        "mcp": [{"server": "github", "key": "token", "secret": "GH_MCP_TOKEN"}],
    });
    let f = StartFields::from_config(cfg.as_object().unwrap()).unwrap();
    assert_eq!(f.dirs, ["/work/a", "/work/b"]);
    assert_eq!(f.effort, "high");
    assert_eq!(f.git_sync, Some(false));
    assert_eq!(f.ask, Some(true));
    assert_eq!(f.clone.as_ref().unwrap().branch, "main");
    assert_eq!(f.mcp[0].secret, "GH_MCP_TOKEN");
    // Session fields and worker fields do not mix.
    assert!(f.validate("session").is_err());
    assert!(f.validate("worker").is_err());
    let session = StartFields {
        stay: false,
        stay_idle_max_s: 0,
        clone: None,
        ..f.clone()
    };
    session.validate("session").unwrap();
    let worker = StartFields {
        dirs: vec![],
        workspace: String::new(),
        git_sync: None,
        ..f
    };
    worker.validate("worker").unwrap();
    assert!(worker.validate("stop").is_err());
    for bad in [
        json!({"dirs": ["relative"]}),
        json!({"dirs": ["/a/../etc"]}),
        json!({"workspace": "../x"}),
        json!({"effort": "turbo"}),
    ] {
        let f = StartFields::from_config(bad.as_object().unwrap()).unwrap();
        assert!(f.validate("session").is_err(), "{bad}");
    }
    for bad in [json!({"fill": true}), json!({"stayIdleMaxS": 100})] {
        let f = StartFields::from_config(bad.as_object().unwrap()).unwrap();
        assert!(f.validate("worker").is_err(), "{bad}");
    }
    assert!(StartFields::from_config(json!({"dirs": 3}).as_object().unwrap()).is_err());
    assert!(
        check_binding(&McpBinding {
            server: "gh".into(),
            key: "token".into(),
            secret: "lower".into()
        })
        .is_err()
    );
}

#[tokio::test]
async fn dispatch_carries_fields_with_provenance_and_audit() {
    let (core, gw, _d) = setup().await;
    let fields = StartFields {
        dirs: vec!["/work/lib".into()],
        effort: "high".into(),
        ask: Some(false),
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/work/app".into(),
        prompt: "go".into(),
        ..Default::default()
    };
    let id = dispatch(&core, "n1", spec, &fields, &origin())
        .await
        .unwrap();
    let (node, sent) = gw.sent.lock().unwrap()[0].clone();
    assert_eq!(node, "n1");
    assert_eq!(sent.dirs, ["/work/lib"]);
    assert_eq!(sent.effort, "high");
    assert_eq!(sent.ask, Some(false));
    assert_eq!(sent.trigger.unwrap().source, "console");
    let l = ledger(&core, "dispatch").await;
    assert!(
        l.iter()
            .any(|e| e.payload["origin"]["surface"] == "sessions/new")
    );
    assert!(
        l.iter()
            .any(|e| e.msg.contains(&id) && e.payload["fields"]["effort"] == "high")
    );

    // A worker that stays.
    let w = StartFields {
        stay: true,
        stay_idle_max_s: 600,
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "worker".into(),
        agent: "patcher".into(),
        cwd: "/work/app".into(),
        ..Default::default()
    };
    dispatch(&core, "n1", spec, &w, &origin()).await.unwrap();
    let sent = gw.sent.lock().unwrap()[1].1.clone();
    assert!(sent.stay);
    assert_eq!(sent.stay_idle_max_s, 600);

    // A field that does not fit the kind is refused and audited.
    let bad = DispatchSpec {
        kind: "session".into(),
        ..Default::default()
    };
    assert!(dispatch(&core, "n1", bad, &w, &origin()).await.is_err());
    assert_eq!(gw.sent.lock().unwrap().len(), 2);
    assert!(
        ledger(&core, "dispatch")
            .await
            .iter()
            .any(|e| e.payload["outcome"] == "refused")
    );
}

#[tokio::test]
async fn kill_switch_refuses_a_start_dispatch() {
    let (core, gw, _d) = setup().await;
    for stop in [true, false] {
        if stop {
            control::stop(&core, "test").await.unwrap();
        } else {
            control::pause(&core, "test").await.unwrap();
        }
        let spec = DispatchSpec {
            kind: "session".into(),
            cwd: "/w".into(),
            ..Default::default()
        };
        let fields = StartFields {
            effort: "low".into(),
            ..Default::default()
        };
        let e = dispatch(&core, "n1", spec, &fields, &origin())
            .await
            .unwrap_err();
        assert!(format!("{e}").contains("refused"), "{e}");
        control::resume(&core).await.unwrap();
    }
    assert!(gw.sent.lock().unwrap().is_empty());
    let refused = ledger(&core, "dispatch")
        .await
        .into_iter()
        .filter(|e| e.payload["outcome"] == "refused")
        .count();
    assert_eq!(refused, 2);
}

#[tokio::test]
async fn mcp_binding_stores_names_only_and_obeys_the_switch() {
    let (core, gw, d) = setup().await;
    crate::vault::install_vault(
        &core,
        crate::vault::KeyBackend::File(d.path().join("vault.key")),
    )
    .await
    .unwrap();
    const VALUE: &str = "ghp_mcp_NEVER_STORED_123";
    crate::vault::put_env_secret(&core, "GH_MCP_TOKEN", "", VALUE.to_string().into())
        .await
        .unwrap();
    let b = McpBinding {
        server: "github".into(),
        key: "token".into(),
        secret: "gh_mcp_token".into(),
    };
    // Unknown secret: refused.
    assert!(
        mcp_bind(
            &core,
            McpBinding {
                secret: "NOPE_TOKEN".into(),
                ..b.clone()
            },
            &origin()
        )
        .await
        .is_err()
    );
    // Kill switch: refused and audited.
    control::stop(&core, "test").await.unwrap();
    assert!(mcp_bind(&core, b.clone(), &origin()).await.is_err());
    control::resume(&core).await.unwrap();

    let s = mcp_bind(&core, b.clone(), &origin()).await.unwrap();
    assert_eq!(s.binding.secret, "GH_MCP_TOKEN");
    assert_eq!(
        (s.by.as_str(), s.surface.as_str()),
        ("owner", "sessions/new")
    );
    let list = mcp_bindings(&core).await.unwrap();
    assert_eq!(list.len(), 1);

    // The dispatch carries the binding by name.
    let fields = StartFields {
        mcp: list.iter().map(|x| x.binding.clone()).collect(),
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/w".into(),
        ..Default::default()
    };
    dispatch(&core, "n1", spec, &fields, &origin())
        .await
        .unwrap();
    let sent = gw.sent.lock().unwrap()[0].1.clone();
    assert_eq!(sent.mcp[0].secret, "GH_MCP_TOKEN");
    assert_eq!(sent.mcp[0].server, "github");

    // Unbinding winds down: allowed under the switch.
    control::stop(&core, "test").await.unwrap();
    assert!(
        mcp_unbind(&core, "github", "token", &origin())
            .await
            .unwrap()
    );
    control::resume(&core).await.unwrap();
    assert!(mcp_bindings(&core).await.unwrap().is_empty());

    // Names in the ledger, never the value; the stored setting holds none.
    let vault = ledger(&core, "vault").await;
    assert!(vault.iter().any(|e| e.payload["outcome"] == "refused"));
    assert!(vault.iter().any(|e| e.payload["op"] == "unbind"));
    for e in core.audit_entries(&Default::default()).await.unwrap() {
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains(VALUE), "a value reached the ledger");
    }
    for e in core.store().recent_events(500).await.unwrap() {
        assert!(
            !format!("{e:?}").contains(VALUE),
            "a value reached an event"
        );
    }
    let raw = core
        .store()
        .setting(MCP_KEY)
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(!raw.contains(VALUE));
}
