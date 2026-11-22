//! P6 wiring over real TLS on loopback: a budget push (#33) reaches the
//! node as a `budget_update` frame; plan limits (row 48) from HostStatus and
//! a session upsert land per node; a knowledge search (row 49) and an
//! AGENTS.md draft (row 54) go out as jobs and come back through
//! Jobs.Complete.

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

fn p6_caps() -> pb::Capabilities {
    pb::Capabilities {
        remote_control: true,
        dispatch_kinds: ["session", "worker", "budget_update", "knowledge_reindex"]
            .map(String::from)
            .to_vec(),
        job_kinds: ["knowledge_search", "init_draft"]
            .map(String::from)
            .to_vec(),
        ..Default::default()
    }
}

fn plan(window: &str, used: f64) -> pb::PlanLimit {
    pb::PlanLimit {
        provider: "anthropic".into(),
        window: window.into(),
        used_fraction: used,
        reset_at_ms: 4_102_444_800_000,
        source: "exact".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_budget_push_reaches_the_node_and_the_kill_switch_holds_it() {
    use kls_core::belai::budget::{PushRequest, push};
    use kls_core::gateway::{BudgetPush, WorkerBudget};
    let h = start().await;
    let n = join(&h, p6_caps()).await;
    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let req = PushRequest {
        push: BudgetPush {
            action: "limits".into(),
            budget_id: "b1".into(),
            budget_name: "Monthly".into(),
            reason: "saved".into(),
            ..Default::default()
        },
        budget: Some(WorkerBudget {
            max_cost_usd: 2.5,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = push(h.core(), req.clone(), "budget:b1").await.unwrap();
    assert_eq!(r.sent.len(), 1, "{r:?}");
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.kind, "budget_update");
    let p = d.budget_push.expect("the push on the frame");
    assert_eq!((p.action.as_str(), p.budget_id.as_str()), ("limits", "b1"));
    assert_eq!(d.budget.unwrap().max_cost_usd, 2.5);
    assert_eq!(d.trigger.unwrap().subject, "budget:b1");
    // A downgrade with no tier is refused before anything is sent.
    let bad = PushRequest {
        push: BudgetPush {
            action: "downgrade".into(),
            ..Default::default()
        },
        model: "m".into(),
        ..Default::default()
    };
    assert!(push(h.core(), bad, "owner").await.is_err());
    kls_core::control::stop(h.core(), "drill").await.unwrap();
    let r = push(h.core(), req, "budget:b1").await.unwrap();
    assert!(r.refused.is_some());
    // The stop itself goes out as a control frame; no dispatch follows.
    while let Ok(Ok(Some(f))) =
        tokio::time::timeout(Duration::from_millis(300), down.message()).await
    {
        assert!(
            !matches!(f.body, Some(server_frame::Body::Dispatch(_))),
            "a dispatch went out under the kill switch: {f:?}"
        );
    }
}

#[tokio::test]
async fn plan_limits_from_status_and_sessions_land_per_node() {
    let h = start().await;
    let n = join(&h, p6_caps()).await;
    let (tx, _down) = n.link().await;
    tx.send(pb::NodeFrame {
        body: Some(node_frame::Body::Status(pb::HostStatus {
            at_ms: 1,
            plan_limits: vec![plan("5h", 0.95)],
            ..Default::default()
        })),
        ..Default::default()
    })
    .await
    .unwrap();
    n.sessions()
        .upsert(pb::SessionUpsert {
            session_id: "s-plan".into(),
            kind: "rc".into(),
            state: "live".into(),
            plan_limits: vec![plan("7d", 0.4)],
            ..Default::default()
        })
        .await
        .unwrap();
    let mut got = Vec::new();
    for _ in 0..50 {
        got = h.core().node_plan_limits(&n.id).await.unwrap();
        if got.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let windows: Vec<&str> = got.iter().map(|l| l.window.as_str()).collect();
    assert_eq!(windows, ["5h", "7d"]);
    assert_eq!(got[1].session_id, "s-plan");
    // The 5h window is nearly used: a job for the provider has no other node,
    // so this one is kept (work waits on the provider, not on Keleustes).
    let picked = h
        .hub
        .pick_job_node(
            None,
            "knowledge_search",
            &JobNeeds {
                provider: Some("anthropic".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(picked.is_err() || picked.unwrap() == n.id);
}

#[tokio::test]
async fn knowledge_search_and_init_draft_go_out_as_jobs() {
    let h = start().await;
    let n = join(&h, p6_caps()).await;
    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let core = h.core().clone();
    let search = tokio::spawn(async move {
        kls_core::belai::knowledge::search(&core, "deploy", "", "", 5, "owner").await
    });
    let f = next(&mut down).await;
    let Some(server_frame::Body::Job(j)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(j.kind, "knowledge_search");
    assert!(j.payload_json.contains("\"query\":\"deploy\""));
    n.jobs()
        .complete(pb::JobResult {
            job_id: j.id,
            status: "ok".into(),
            output_json: r#"{"hits":[{"address":"kb://x/a.md#1","index":"project","score":0.7,"snippet":"run just deploy"}]}"#.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let r = search.await.unwrap().unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].node_id, n.id);

    let core = h.core().clone();
    let nid = n.id.clone();
    let draft = tokio::spawn(async move {
        kls_core::belai::initdraft::request(&core, &nid, "/work", "", "owner").await
    });
    let f = next(&mut down).await;
    let Some(server_frame::Body::Job(j)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(j.kind, "init_draft");
    let md = "# Agents\n\nRun `just check`.\n";
    let sha = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(md.as_bytes()))
    };
    n.jobs()
        .complete(pb::JobResult {
            job_id: j.id,
            status: "ok".into(),
            output_json: serde_json::json!({ "path": "AGENTS.md", "markdown": md, "sha256": sha })
                .to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = draft.await.unwrap().unwrap();
    assert_eq!((d.status.as_str(), d.cwd.as_str()), ("pending", "/work"));
    assert_eq!(d.sha256, sha);
}
