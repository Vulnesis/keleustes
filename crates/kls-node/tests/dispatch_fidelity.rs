//! P3 dispatch fidelity over real TLS on loopback: the New session
//! dialog's start fields and a stored MCP binding, sent through
//! `kls_core::sessions::start::dispatch`, reach the node's `Dispatch` frame
//! and its `VaultGrant.mcp`; no value reaches the database or the ledger.

#![allow(dead_code, unused_imports)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kls_core::{Core, DispatchSpec, JobNeeds};
use kls_node::{Hub, hub, pki, run, token};
use kls_proto::v1::jobs_client::JobsClient;
use kls_proto::v1::node_client::NodeClient;
use kls_proto::v1::sessions_client::SessionsClient;
use kls_proto::v1::{self as pb, inbox_item, node_frame, server_frame};
use rcgen::{CertificateParams, KeyPair};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

struct Harness {
    hub: Hub,
    addr: String,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Harness {
    fn core(&self) -> &Core {
        self.hub.core()
    }
}

async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    core.store()
        .set_setting(kls_core::settings::LISTEN_ADDR, &listen)
        .await
        .unwrap();
    let hub = hub(core).await.unwrap();
    let (stop, rx) = tokio::sync::oneshot::channel();
    let h2 = hub.clone();
    tokio::spawn(async move {
        run(h2, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(&listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Harness {
        hub,
        addr: listen,
        _dir: dir,
        _stop: stop,
    }
}

async fn channel(addr: &str, ca_pem: &str, identity: Option<Identity>) -> Channel {
    let mut tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .domain_name(pki::TLS_SERVER_NAME);
    if let Some(id) = identity {
        tls = tls.identity(id);
    }
    Channel::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap()
}

/// A joined node: its id and an mTLS channel.
struct TestNode {
    id: String,
    chan: Channel,
}

impl TestNode {
    fn sessions(&self) -> SessionsClient<Channel> {
        SessionsClient::new(self.chan.clone())
    }
    fn jobs(&self) -> JobsClient<Channel> {
        JobsClient::new(self.chan.clone())
    }

    /// Opens Link and reads the Welcome.
    async fn link(
        &self,
    ) -> (
        mpsc::Sender<pb::NodeFrame>,
        tonic::Streaming<pb::ServerFrame>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        tx.send(pb::NodeFrame {
            body: Some(node_frame::Body::Hello(pb::Hello::default())),
            ..Default::default()
        })
        .await
        .unwrap();
        let mut down = NodeClient::new(self.chan.clone())
            .link(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        let w = down.message().await.unwrap().unwrap();
        assert!(matches!(w.body, Some(server_frame::Body::Welcome(_))));
        (tx, down)
    }
}

async fn join(h: &Harness, caps: pb::Capabilities) -> TestNode {
    let ca = h.hub.pki_ca_pem();
    let minted = h.hub.mint_token("", token::DEFAULT_TTL_MS).await.unwrap();
    let p = token::Payload::decode(&minted.token).unwrap();
    let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let csr = CertificateParams::default()
        .serialize_request(&key)
        .unwrap();
    let joined = NodeClient::new(channel(&h.addr, &ca, None).await)
        .join(pb::JoinRequest {
            token_id: p.t,
            secret: URL_SAFE_NO_PAD.decode(p.s).unwrap(),
            csr_der: csr.der().to_vec(),
            host: Some(pb::HostAdvert {
                hostname: "worker".into(),
                capabilities: Some(caps),
                trusted_dirs: vec!["/work".into(), "/work/lib".into()],
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();
    let cert = ::pem::encode(&::pem::Pem::new("CERTIFICATE", joined.client_cert_der));
    let id = Identity::from_pem(cert, key.serialize_pem());
    TestNode {
        id: joined.node_id,
        chan: channel(&h.addr, &ca, Some(id)).await,
    }
}

async fn next<T>(s: &mut tonic::Streaming<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), s.message())
        .await
        .expect("stream timed out")
        .unwrap()
        .expect("stream ended")
}
fn rc_caps() -> pb::Capabilities {
    pb::Capabilities {
        remote_control: true,
        dispatch_kinds: [
            "session",
            "worker",
            "crew",
            "stop",
            "pause",
            "resume",
            "crew_control",
        ]
        .map(String::from)
        .to_vec(),
        ..Default::default()
    }
}

#[tokio::test]
async fn dialog_fields_and_mcp_binding_reach_the_frame() {
    use kls_core::gateway::McpBinding;
    use kls_core::sessions::start::{self, StartFields};
    let h = start().await;
    let key_dir = tempfile::tempdir().unwrap();
    kls_core::vault::install_vault(
        h.core(),
        kls_core::vault::KeyBackend::File(key_dir.path().join("vault.key")),
    )
    .await
    .unwrap();
    const MCP: &str = "ghp_p3_mcp_NEVER_ON_DISK_9";
    kls_core::vault::put_env_secret(h.core(), "GH_MCP_TOKEN", "", MCP.to_string().into())
        .await
        .unwrap();
    let origin = kls_core::hosts::Origin {
        actor: "owner".into(),
        surface: "sessions/new".into(),
        ..Default::default()
    };
    start::mcp_bind(
        h.core(),
        McpBinding {
            server: "github".into(),
            key: "token".into(),
            secret: "GH_MCP_TOKEN".into(),
        },
        &origin,
    )
    .await
    .unwrap();
    let bindings: Vec<McpBinding> = start::mcp_bindings(h.core())
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.binding)
        .collect();
    let n = join(&h, rc_caps()).await;
    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let fields = StartFields {
        dirs: vec!["/work/lib".into()],
        effort: "high".into(),
        ask: Some(true),
        mcp: bindings,
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/work".into(),
        prompt: "fix it".into(),
        ..Default::default()
    };
    start::dispatch(h.core(), &n.id, spec, &fields, &origin)
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.dirs, ["/work/lib"]);
    assert_eq!(d.effort, "high");
    assert_eq!(d.ask, Some(true));
    assert_eq!(d.trigger.as_ref().unwrap().source, "console");
    let g = d.vault.expect("a grant for the MCP binding");
    assert_eq!(g.mcp.len(), 1);
    assert_eq!(
        (
            g.mcp[0].server.as_str(),
            g.mcp[0].key.as_str(),
            g.mcp[0].value.as_str()
        ),
        ("github", "token", MCP)
    );

    // A staying worker: stay and its idle wait on the frame.
    let w = StartFields {
        stay: true,
        stay_idle_max_s: 600,
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "worker".into(),
        cwd: "/work".into(),
        agent: "patcher".into(),
        ..Default::default()
    };
    start::dispatch(h.core(), &n.id, spec, &w, &origin)
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert!(d.stay);
    assert_eq!(d.stay_idle_max_s, 600);

    // The value is in no stored row: outbox, dispatch spec, ledger, events.
    for (_, bytes) in h.core().store().outbox_after(&n.id, 0).await.unwrap() {
        assert!(!String::from_utf8_lossy(&bytes).contains(MCP));
    }
    for e in h.core().audit_entries(&Default::default()).await.unwrap() {
        assert!(!serde_json::to_string(&e).unwrap().contains(MCP));
    }
    for e in h.core().store().recent_events(500).await.unwrap() {
        assert!(!format!("{e:?}").contains(MCP));
    }
    let specs = h
        .core()
        .store()
        .dispatches(Some(&n.id), 10)
        .await
        .unwrap_or_default();
    for d in specs {
        assert!(!d.spec_json.contains(MCP));
    }

    // The kill switch refuses before anything is sent.
    kls_core::control::stop(h.core(), "test").await.unwrap();
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/work".into(),
        prompt: "again".into(),
        ..Default::default()
    };
    assert!(
        start::dispatch(h.core(), &n.id, spec, &fields, &origin)
            .await
            .is_err()
    );
}

/// P3 rows 45, 46, 51, 52, 55 over TLS: a node that lists no dispatch
/// features gets none of the new fields (refused and audited); one that
/// does gets resume, fork, answers, gates and a one-off worker on its
/// frames, and its upsert's checkpoints, relay draft and plan hash are
/// stored for the console.
#[tokio::test]
async fn p3_fields_cross_the_wire_behind_their_features() {
    use kls_core::sessions::resume::{self, Resume};
    use kls_core::sessions::start::{self, StartFields};
    let h = start().await;
    let origin = kls_core::hosts::Origin {
        actor: "owner".into(),
        surface: "sessions/resume".into(),
        ..Default::default()
    };
    // An older host: no features.
    let old = join(&h, rc_caps()).await;
    let answers = StartFields {
        answers_json: r#"[{"choose":0}]"#.into(),
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/work".into(),
        prompt: "go".into(),
        ..Default::default()
    };
    let e = start::dispatch(h.core(), &old.id, spec.clone(), &answers, &origin)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("answers dispatch field"), "{e}");

    let mut caps = rc_caps();
    caps.dispatch_features = [
        "resume",
        "fork",
        "answers",
        "gates",
        "adhoc",
        "plan_upload",
        "session_actions",
    ]
    .map(String::from)
    .to_vec();
    let n = join(&h, caps).await;
    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The node's session: checkpoints and a relay review, then a plan.
    let mut s = n.sessions();
    s.upsert(pb::SessionUpsert {
        session_id: "s1".into(),
        cwd: "/work".into(),
        provider: "openai".into(),
        model: "test".into(),
        kind: "rc".into(),
        state: "waiting".into(),
        accepts: Some(pb::SessionAccepts {
            answers: true,
            controls: true,
            controls_allowed: vec!["relay".into(), "rewind".into()],
            ..Default::default()
        }),
        pending_ask: Some(pb::PendingAsk {
            id: "r1".into(),
            kind: "relay_review".into(),
            title: "handoff".into(),
            options: ["approve", "edit", "drop"]
                .map(|o| pb::AskOption {
                    id: o.into(),
                    label: o.into(),
                })
                .to_vec(),
            relay_json: r#"{"text":"draft","direction":""}"#.into(),
            ..Default::default()
        }),
        checkpoints: vec![pb::Checkpoint {
            id: "cp1".into(),
            at_ms: 5,
            label: "write it".into(),
            entry_index: 3,
            entry_id: "e3".into(),
            files: 2,
        }],
        ..Default::default()
    })
    .await
    .unwrap();
    let x = h
        .core()
        .store()
        .session_extras("s1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(x.checkpoints[0].entry_id, "e3");
    assert!(x.pending_ask.unwrap().relay_json.contains("draft"));
    h.core()
        .queue_action("s1", "rewind", &["cp1".into()], "owner")
        .await
        .unwrap();
    let sha = "cd".repeat(32);
    s.end(pb::SessionEnd {
        session_id: "s1".into(),
        ended_at_ms: 9,
        reason: "completed".into(),
        plan_sha256: sha.clone(),
        ..Default::default()
    })
    .await
    .unwrap();
    let x = h
        .core()
        .store()
        .session_extras("s1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(x.plan_sha256, sha);

    // Fork, answers + gates, a one-off worker: on the frames.
    resume::resume(
        h.core(),
        Resume {
            session_id: "s1".into(),
            fork_entry: "e3".into(),
            ..Default::default()
        },
        &origin,
    )
    .await
    .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(
        (d.resume_session.as_str(), d.fork_entry.as_str()),
        ("s1", "e3")
    );
    assert_eq!(d.trigger.unwrap().subject, "fork:s1@e3");
    let mut fields = answers.clone();
    fields.gates.insert("lint_loop".into(), "on".into());
    start::dispatch(h.core(), &n.id, spec, &fields, &origin)
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.answers_json, r#"[{"choose":0}]"#);
    assert_eq!(d.options.get("lint_loop").map(String::as_str), Some("on"));
    let w = StartFields {
        adhoc: Some(kls_core::gateway::AdhocWorker {
            prompt: "fix flaky tests".into(),
            tools: vec!["Read".into()],
            read_only: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let spec = DispatchSpec {
        kind: "worker".into(),
        cwd: "/work".into(),
        ..Default::default()
    };
    start::dispatch(h.core(), &n.id, spec, &w, &origin)
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    let a = d.adhoc.expect("adhoc on the frame");
    assert_eq!((a.prompt.as_str(), a.read_only), ("fix flaky tests", true));
    assert!(d.agent.is_empty());
}
