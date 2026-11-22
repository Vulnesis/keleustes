//! belai counterparts over real TLS on loopback (docs/gaps/belai-counterparts.md).

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
                trusted_dirs: vec!["/work".into()],
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

fn upsert(id: &str) -> pb::SessionUpsert {
    pb::SessionUpsert {
        session_id: id.into(),
        title: "fix the build".into(),
        cwd: "/work/api".into(),
        kind: "rc".into(),
        state: "live".into(),
        model: "opus".into(),
        started_at_ms: 1,
        updated_at_ms: 1,
        ..Default::default()
    }
}

fn permission_ask() -> pb::PendingAsk {
    pb::PendingAsk {
        id: "ask1".into(),
        kind: "permission".into(),
        title: "permission: Bash rm -rf build".into(),
        tool: "Bash".into(),
        options: vec![
            pb::AskOption {
                id: "allow_once".into(),
                label: "Allow once".into(),
            },
            pb::AskOption {
                id: "deny".into(),
                label: "Deny".into(),
            },
        ],
        ..Default::default()
    }
}

/// WP-A: upsert -> pending_ask stored -> answer -> inbox item with decision;
/// usage lands in the Budgets rows; the end rollup replaces it.
#[tokio::test]
async fn structured_ask_round_trip_and_usage() {
    let h = start().await;
    let n = join(&h, pb::Capabilities::default()).await;
    let mut s = n.sessions();
    let mut up = upsert("s1");
    up.state = "waiting".into();
    up.pending_question_id = "ask1".into();
    up.pending_question = "permission: Bash rm -rf build".into();
    up.pending_ask = Some(permission_ask());
    up.accepts = Some(pb::SessionAccepts {
        prompts: true,
        answers: true,
        ..Default::default()
    });
    up.git = Some(pb::GitFacts {
        commit: "abc1234".into(),
        dirty: true,
        ahead: 1,
        ..Default::default()
    });
    up.usage = vec![pb::ModelUsage {
        provider: "openai".into(),
        model: "test".into(),
        input_tokens: 40,
        output_tokens: 2,
        cost_usd: 0.01,
        ..Default::default()
    }];
    s.upsert(up.clone()).await.unwrap();
    let core = h.core();
    let x = core.store().session_extras("s1").await.unwrap().unwrap();
    let ask = x.pending_ask.unwrap();
    assert_eq!((ask.id.as_str(), ask.options.len()), ("ask1", 2));
    assert_eq!(x.git.unwrap().commit, "abc1234");
    assert_eq!(
        core.store().usage_sum(&Default::default()).await.unwrap().0,
        42
    );

    // A decision the ask does not list is refused before it is queued.
    assert!(
        core.queue_ask_answer("s1", "ask1", "allow_always", "", "")
            .await
            .is_err()
    );
    core.queue_ask_answer("s1", "ask1", "allow_once", "", "")
        .await
        .unwrap();
    let mut inbox = s
        .inbox(pb::InboxRequest {
            session_id: "s1".into(),
            after: 0,
        })
        .await
        .unwrap()
        .into_inner();
    let item = next(&mut inbox).await;
    match item.body {
        Some(inbox_item::Body::Answer(a)) => {
            assert_eq!(a.question_id, "ask1");
            assert_eq!(a.decision, "allow_once");
        }
        other => panic!("{other:?}"),
    }

    // The ask closes; usage grows; the end's rollup is final.
    up.pending_ask = None;
    up.pending_question_id.clear();
    up.pending_question.clear();
    up.usage[0].input_tokens = 100;
    s.upsert(up).await.unwrap();
    assert!(
        core.store()
            .session_extras("s1")
            .await
            .unwrap()
            .unwrap()
            .pending_ask
            .is_none()
    );
    s.end(pb::SessionEnd {
        session_id: "s1".into(),
        ended_at_ms: 5,
        reason: "completed".into(),
        usage: vec![pb::ModelUsage {
            provider: "openai".into(),
            model: "test".into(),
            input_tokens: 120,
            output_tokens: 8,
            ..Default::default()
        }],
        turns: 2,
        tool_calls: 3,
        termination_verified: true,
        empty_result: false,
        adapter_error: true,
        timing: Some(pb::HarnessTiming {
            startup_ms: 150,
            wall_ms: 4000,
            ..Default::default()
        }),
        plan_sha256: String::new(),
    })
    .await
    .unwrap();
    assert_eq!(
        core.store().usage_sum(&Default::default()).await.unwrap().0,
        128
    );
    let x = core.store().session_extras("s1").await.unwrap().unwrap();
    assert_eq!((x.turns, x.tool_calls), (2, 3));
    let end = x.harness.and_then(|h| h.end).unwrap();
    assert!(end.termination_verified && end.adapter_error && !end.empty_result);
    assert_eq!((end.startup_ms, end.wall_ms), (150, 4000));
}

/// A session of a node that is gone past the grace (or revoked) is lost.
#[tokio::test]
async fn sessions_of_vanished_nodes_are_lost() {
    let h = start().await;
    let n = join(&h, pb::Capabilities::default()).await;
    n.sessions().upsert(upsert("s1")).await.unwrap();
    let core = h.core();
    let now = kls_store::now_ms();
    // Within the grace: kept.
    assert_eq!(kls_node::lost::sweep(&h.hub, now, now).await.unwrap(), 0);
    // Past it, with no link: lost.
    let later = now + 11 * 60_000;
    assert_eq!(kls_node::lost::sweep(&h.hub, now, later).await.unwrap(), 1);
    let s = core.store().session("s1").await.unwrap().unwrap();
    assert_eq!(s.end_reason, kls_node::lost::LOST);
    // An online node keeps its sessions; a revoked one loses them at once.
    n.sessions().upsert(upsert("s2")).await.unwrap();
    let (_tx, _down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(kls_node::lost::sweep(&h.hub, now, later).await.unwrap(), 0);
    h.hub.revoke(&n.id, "test").await.unwrap();
    assert_eq!(kls_node::lost::sweep(&h.hub, now, now).await.unwrap(), 1);
    let s = core.store().session("s2").await.unwrap().unwrap();
    assert_eq!(s.end_reason, kls_node::lost::REVOKED);
}

/// WP-D: a node that lists `crew_control` gets one, replayed after a
/// reconnect with its trigger; an older node is refused before queueing;
/// the kill switch refuses new work (audited) but still lets a stop out.
#[tokio::test]
async fn dispatch_kinds_capabilities_and_kill_switch() {
    use kls_core::gateway::CrewControl;
    let h = start().await;
    let old = join(&h, pb::Capabilities::default()).await;
    let new = join(
        &h,
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
        },
    )
    .await;
    let gw = h.core().gateway().unwrap();
    let ctl = DispatchSpec {
        kind: "crew_control".into(),
        control: Some(CrewControl {
            run: "run1".into(),
            op: "replicas".into(),
            profile: "belai:patcher".into(),
            replicas: 2,
            ..Default::default()
        }),
        ..Default::default()
    };
    let e = gw.dispatch(&old.id, ctl.clone()).await.unwrap_err();
    assert!(e.to_string().contains("update Belai"), "{e}");
    assert!(
        h.core()
            .store()
            .dispatches(None, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let id = gw.dispatch(&new.id, ctl.clone()).await.unwrap();
    let (tx, mut down) = new.link().await;
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.id, id);
    assert_eq!(d.control.as_ref().unwrap().replicas, 2);
    assert_eq!(d.trigger.as_ref().unwrap().source, "console");
    // Not acked: a reconnect replays it.
    drop(tx);
    drop(down);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_tx, mut down) = new.link().await;
    let f = next(&mut down).await;
    assert!(matches!(f.body, Some(server_frame::Body::Dispatch(ref d)) if d.id == id));

    // Kill switch: new work is refused and audited; a stop still goes out.
    h.core().set_factory_stopped(true, "test").await.unwrap();
    let start = DispatchSpec {
        kind: "session".into(),
        cwd: "/work".into(),
        prompt: "go".into(),
        ..Default::default()
    };
    assert!(gw.dispatch(&new.id, start).await.is_err());
    gw.dispatch(
        &new.id,
        DispatchSpec {
            kind: "stop".into(),
            session_id: "s9".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let refusals = h
        .core()
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "dispatch" && e.msg.contains("refused"))
        .count();
    assert_eq!(refusals, 2, "the old node's and the kill switch's");
}

fn fleet(n: &TestNode) -> kls_proto::v1::fleet_client::FleetClient<Channel> {
    kls_proto::v1::fleet_client::FleetClient::new(n.chan.clone())
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

fn ask(target: &str, cwd: &str, by: &str) -> pb::DispatchRequest {
    pb::DispatchRequest {
        target_node: target.into(),
        spec: Some(pb::Dispatch {
            kind: "session".into(),
            cwd: cwd.into(),
            prompt: "run the tests".into(),
            ..Default::default()
        }),
        origin_session: "sa".into(),
        initiated_by: by.into(),
    }
}

/// WP-I: host A asks Keleustes for work on host B. Off by default; "ask"
/// parks it until the owner approves; an agent's request is never
/// automatic; a directory B does not offer is refused before queueing; the
/// kill switch refuses; Watch follows the request to started; every step
/// is audited with A as origin; B sees a node trigger naming A.
#[tokio::test]
async fn cross_host_dispatch_policy_and_watch() {
    use kls_core::belai::fleet::{Action, Policy, Rule};
    let h = start().await;
    let a = join(&h, rc_caps()).await;
    let b = join(&h, rc_caps()).await;
    let core = h.core();
    let (_btx, mut bdown) = b.link().await;
    let (_atx, _adown) = a.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Off by default.
    let t = fleet(&a)
        .request(ask(&b.id, "/work", "user"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "refused");
    assert!(fleet(&a).nodes(pb::NodesRequest {}).await.is_err());

    let mut p = Policy {
        enabled: true,
        ..Default::default()
    };
    core.set_fleet_policy(&p).await.unwrap();
    let nodes = fleet(&a)
        .nodes(pb::NodesRequest {})
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(nodes.len(), 1, "A sees B only");
    assert!(
        nodes[0].trusted_dirs.is_empty(),
        "directories are not shared by default"
    );

    // A directory B does not offer: refused before queueing.
    let t = fleet(&a)
        .request(ask(&b.id, "/elsewhere", "user"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "refused");
    assert!(t.reason.contains("does not offer"), "{}", t.reason);

    // Default rule: ask. The owner approves; B gets a node trigger.
    let t = fleet(&a)
        .request(ask("", "/work", "user"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (t.state.as_str(), t.target_node.as_str()),
        ("awaiting_approval", b.id.as_str())
    );
    let mut watch = fleet(&a)
        .watch(pb::WatchRequest {
            request_ids: vec![t.request_id.clone()],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(next(&mut watch).await.state, "awaiting_approval");
    let approved = core.fleet_decide(&t.request_id, true).await.unwrap();
    assert_eq!(approved.state, "queued");
    let f = next(&mut bdown).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    let tr = d.trigger.unwrap();
    assert_eq!(
        (
            tr.source.as_str(),
            tr.origin_node.as_str(),
            tr.origin_session.as_str()
        ),
        ("node", a.id.as_str(), "sa")
    );
    assert!(d.vault.is_none(), "a node's request carries no vault grant");
    // B starts it: Watch reaches started.
    b.jobs()
        .complete(pb::JobResult {
            job_id: d.id.clone(),
            status: "ok".into(),
            session_id: "sb".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        let s = next(&mut watch).await;
        if s.state == "started" {
            assert_eq!(s.session_id, "sb");
            break;
        }
    }

    // An agent's request is never automatic, even under an "auto" rule.
    p.rules = vec![Rule {
        action: Action::Auto,
        ..Default::default()
    }];
    core.set_fleet_policy(&p).await.unwrap();
    let t = fleet(&a)
        .request(ask(&b.id, "/work", "agent"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "awaiting_approval");
    let t = fleet(&a)
        .request(ask(&b.id, "/work", "user"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "queued");
    assert!(!t.dispatch_id.is_empty());

    // Kill switch.
    core.set_factory_stopped(true, "test").await.unwrap();
    let t = fleet(&a)
        .request(ask(&b.id, "/work", "user"))
        .await
        .unwrap_or_else(|e| panic!("kill-switch request: {e:?}"))
        .into_inner();
    assert_eq!(t.state, "refused");
    assert!(t.reason.contains("stopped"), "{}", t.reason);

    let audited = core
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "node_request" && e.origin == format!("host:{}", a.id))
        .count();
    assert!(audited >= 7, "audited {audited}");
}

/// #41 rule 4: the A2A blackout refuses an agent's Fleet.Request (audited
/// `a2a.withheld`), holds one an agent parked before the blackout even when
/// the owner approves it, and leaves a person's request alone.
#[tokio::test]
async fn cross_host_dispatch_a2a_blackout() {
    use kls_core::belai::fleet::Policy;
    let h = start().await;
    let a = join(&h, rc_caps()).await;
    let b = join(&h, rc_caps()).await;
    let core = h.core();
    let (_btx, _bdown) = b.link().await;
    let (_atx, _adown) = a.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    core.set_fleet_policy(&Policy {
        enabled: true,
        ..Default::default()
    })
    .await
    .unwrap();
    // Parked before the blackout.
    let parked = fleet(&a)
        .request(ask(&b.id, "", "agent"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(parked.state, "awaiting_approval");

    core.set_a2a_blackout(true, "test").await.unwrap();
    let t = fleet(&a)
        .request(ask(&b.id, "", "agent"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "refused");
    assert!(t.reason.contains("A2A"), "{}", t.reason);
    let decided = core.fleet_decide(&parked.request_id, true).await.unwrap();
    assert_eq!(decided.state, "refused");
    assert!(decided.reason.contains("A2A"), "{}", decided.reason);
    // A person on host A is not agent-to-agent traffic.
    let t = fleet(&a)
        .request(ask(&b.id, "", "user"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.state, "awaiting_approval");

    assert!(core.audit_verify().await.unwrap().ok);
    let withheld = core
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "a2a_withheld")
        .count();
    assert_eq!(withheld, 2, "one per refusal");
}

/// WP-I prompts: a session that does not take remote prompts is refused,
/// and the A2A blackout withholds a peer prompt.
#[tokio::test]
async fn peer_prompt_gates() {
    use kls_core::belai::fleet::Policy;
    let h = start().await;
    let a = join(&h, rc_caps()).await;
    let b = join(&h, rc_caps()).await;
    let core = h.core();
    core.set_fleet_policy(&Policy {
        enabled: true,
        prompts: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut up = upsert("sb");
    up.accepts = Some(pb::SessionAccepts::default());
    b.sessions().upsert(up.clone()).await.unwrap();
    let pp = |by: &str| pb::PeerPrompt {
        target_session: format!("{}:sb", b.id),
        text: "look at this".into(),
        origin_session: "sa".into(),
        initiated_by: by.into(),
    };
    let t = fleet(&a).prompt(pp("user")).await.unwrap().into_inner();
    assert_eq!(t.state, "refused");
    assert!(t.reason.contains("remote prompts"), "{}", t.reason);
    up.accepts = Some(pb::SessionAccepts {
        prompts: true,
        ..Default::default()
    });
    b.sessions().upsert(up).await.unwrap();
    let t = fleet(&a).prompt(pp("user")).await.unwrap().into_inner();
    assert_eq!(t.state, "queued");
    let t = fleet(&a).prompt(pp("agent")).await.unwrap().into_inner();
    assert_eq!(t.state, "awaiting_approval");
    core.set_a2a_blackout(true, "test").await.unwrap();
    let t = fleet(&a).prompt(pp("user")).await.unwrap().into_inner();
    assert_eq!(t.state, "refused");
    assert!(t.reason.contains("A2A"), "{}", t.reason);
}

/// WP-J: a request with an unknown kind is refused; the owner's decision
/// reaches the node as a replayable frame, replayed after a reconnect.
#[tokio::test]
async fn attention_requests_and_decisions() {
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let mut board = kls_proto::v1::board_client::BoardClient::new(n.chan.clone());
    let r = board
        .attention(pb::AttentionRequest {
            id: "req-0000000001".into(),
            kind: "vulnetix_login".into(),
            reason: "reauth_needed".into(),
            subject: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(r.state, "refused");
    let r = board
        .attention(pb::AttentionRequest {
            id: "req-0000000002".into(),
            kind: "gh_login".into(),
            reason: "reauth_needed".into(),
            subject: "github.com".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(r.state, "open");
    assert_eq!(
        h.core().store().attention("open", 0).await.unwrap().len(),
        1
    );
    h.core()
        .attention_decide(&n.id, "req-0000000002", true)
        .await
        .unwrap();
    let decided = |f: &pb::ServerFrame| matches!(&f.body, Some(server_frame::Body::AttentionDecided(d)) if d.id == "req-0000000002" && d.state == "approved");
    // The decision is queued durably on the node's outbox (by a background
    // task, retried through SQLite's busy errors) before the link opens.
    for _ in 0..500 {
        if !h
            .core()
            .store()
            .outbox_after(&n.id, 0)
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (tx, mut down) = n.link().await;
    let f = next(&mut down).await;
    assert!(decided(&f), "{f:?}");
    drop(tx);
    drop(down);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_tx, mut down) = n.link().await;
    let f = next(&mut down).await;
    assert!(decided(&f), "not replayed: {f:?}");
}

/// WP-C: a host audit stream (no session) is stored under its stream id,
/// deduplicated by index, and its head says where to resume.
#[tokio::test]
async fn host_audit_stream() {
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let mut s = n.sessions();
    let head = |s: &mut SessionsClient<Channel>| {
        let mut s = s.clone();
        async move {
            s.audit_head(pb::AuditStreamRequest {
                stream_id: "abc-1".into(),
            })
            .await
            .unwrap()
            .into_inner()
            .last_index
        }
    };
    assert_eq!(head(&mut s).await, -1);
    let line = |i| pb::AuditLine {
        index: i,
        at_ms: 1,
        kind: "host.dispatch".into(),
        json: "{}".into(),
    };
    for _ in 0..2 {
        s.audit(pb::AuditBatch {
            session_id: String::new(),
            stream_id: "abc-1".into(),
            lines: vec![line(0), line(1)],
        })
        .await
        .unwrap();
    }
    assert_eq!(head(&mut s).await, 1);
    // Neither a session nor a stream: refused.
    assert!(
        s.audit(pb::AuditBatch {
            lines: vec![line(0)],
            ..Default::default()
        })
        .await
        .is_err()
    );
    assert!(
        s.audit(pb::AuditBatch {
            stream_id: "../x".into(),
            lines: vec![line(0)],
            ..Default::default()
        })
        .await
        .is_err()
    );
}

/// Harness sessions: the fields reach the node on the ordinary dispatch,
/// with no vault grant under own login; A2APeers is empty under the A2A
/// blackout.
#[tokio::test]
async fn harness_dispatch_and_peers() {
    use kls_core::harness::{HarnessDispatcher, HarnessFields};
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let (_tx, mut down) = n.link().await;
    let id = h
        .hub
        .dispatch_harness(
            &n.id,
            DispatchSpec {
                cwd: "/work".into(),
                prompt: "list the crates".into(),
                vault_providers: vec!["anthropic".into()],
                ..Default::default()
            },
            HarnessFields {
                harness: "claude-code".into(),
                harness_mode: "plan".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.id, id);
    assert_eq!(
        (
            d.harness.as_str(),
            d.harness_mode.as_str(),
            d.harness_keys.as_str()
        ),
        ("claude-code", "plan", "own_login")
    );
    assert!(d.vault.is_none());
    assert!(d.origin_json.contains("owner"));

    n.sessions().upsert(upsert("s1")).await.unwrap();
    let mut board = kls_proto::v1::board_client::BoardClient::new(n.chan.clone());
    h.core().set_a2a_blackout(true, "test").await.unwrap();
    let peers = board
        .a2a_peers(pb::A2aPeersRequest::default())
        .await
        .unwrap()
        .into_inner()
        .peers;
    assert!(peers.is_empty());
}

/// WP-G: a host's schedules sync both ways over Board, and a factory
/// schedule fires a worker on an online node that has the profile and the
/// directory, with a schedule trigger; the kill switch refuses it.
#[tokio::test]
async fn schedules_sync_and_factory_schedule_dispatch() {
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let core = h.core();
    // The node advertises a worker profile the way belai does: as a route
    // (HostAdvert.agent_routes), beside a session agent (join's advert has
    // /work only).
    let mut host = core.store().node(&n.id).await.unwrap().unwrap().host;
    host.agents = vec!["belai:debug".into()];
    host.capabilities.agent_routes = vec![kls_store::AgentRoute {
        name: "belai:scout".into(),
        mode: "worker".into(),
        lists: vec!["backlog".into()],
        labels: vec!["scout".into()],
        read_only: true,
    }];
    core.store().update_node_host(&n.id, &host).await.unwrap();
    let mut board = kls_proto::v1::board_client::BoardClient::new(n.chan.clone());
    let pushed = board
        .schedule_push(pb::ScheduleBatch {
            items: vec![pb::ScheduleItem {
                id: "sch-1".into(),
                profile: "belai:scout".into(),
                cron: "@daily".into(),
                cwd: "/work".into(),
                enabled: true,
                version: 0,
                ..Default::default()
            }],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pushed.accepted.len(), 1);
    // The owner pauses it; the node pulls the change.
    let mut s = core
        .store()
        .host_schedule(&n.id, "sch-1")
        .await
        .unwrap()
        .unwrap();
    s.enabled = false;
    core.save_host_schedule(s).await.unwrap();
    let page = board
        .schedule_pull(pb::SchedulePullRequest {
            since_ms: 0,
            limit: 0,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.items.len(), 1);
    assert!(!page.items[0].enabled && page.items[0].version == 2);

    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        core.dispatch_scheduled(None, "belai:nope", "/work", "nightly")
            .await
            .is_err()
    );
    let (node, id) = core
        .dispatch_scheduled(None, "belai:scout", "/work", "nightly")
        .await
        .unwrap();
    assert_eq!(node, n.id);
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!((d.id.as_str(), d.kind.as_str()), (id.as_str(), "worker"));
    let t = d.trigger.unwrap();
    assert_eq!(
        (t.source.as_str(), t.workflow_name.as_str()),
        ("schedule", "nightly")
    );
    core.set_factory_stopped(true, "test").await.unwrap();
    assert!(
        core.dispatch_scheduled(None, "belai:scout", "/work", "nightly")
            .await
            .is_err()
    );
}

/// WP-E: a document is answered only for a library_install dispatch to the
/// calling node that names it; a backup only for a backup dispatch; the
/// automatic sync never says "install"; the kill switch refuses an install.
#[tokio::test]
async fn library_fetch_backup_reconcile() {
    use kls_core::gateway::LibraryRef;
    let mut caps = rc_caps();
    caps.dispatch_kinds.push("library_install".into());
    caps.dispatch_kinds.push("library_backup".into());
    let h = start().await;
    let a = join(&h, caps.clone()).await;
    let b = join(&h, caps).await;
    let core = h.core();
    let (item, v) = kls_core::vault::save_library(
        core,
        "",
        "skill",
        "fmt",
        "",
        "---\nname: fmt\ndescription: Formats code\n---\nformat it\n",
        "",
    )
    .await
    .unwrap();
    let gw = core.gateway().unwrap();
    let r = LibraryRef {
        kind: "skill".into(),
        id: item.clone(),
        name: "fmt".into(),
        version: v,
        ..Default::default()
    };
    let did = gw
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_install".into(),
                library: vec![r.clone()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let fetch = |n: &TestNode, dispatch: &str| {
        let mut c = kls_proto::v1::library_client::LibraryClient::new(n.chan.clone());
        let req = pb::LibraryFetch {
            dispatch_id: dispatch.into(),
            teleport_id: String::new(),
            r#ref: Some(pb::LibraryRef {
                kind: "skill".into(),
                id: item.clone(),
                name: "fmt".into(),
                version: 0,
                sha256: String::new(),
            }),
        };
        async move { c.fetch(req).await }
    };
    let doc = fetch(&a, &did).await.unwrap().into_inner();
    assert!(String::from_utf8_lossy(&doc.doc).contains("format it"));
    assert_eq!(doc.r#ref.unwrap().sha256.len(), 64);
    // Another node, or no dispatch, gets nothing.
    assert_eq!(
        fetch(&b, &did).await.unwrap_err().code(),
        tonic::Code::PermissionDenied
    );
    assert!(fetch(&a, "").await.is_err());

    // A backup needs a backup dispatch naming it.
    let mut lib = kls_proto::v1::library_client::LibraryClient::new(a.chan.clone());
    let backup = |dispatch: String| pb::LibraryDoc {
        dispatch_id: dispatch,
        r#ref: Some(pb::LibraryRef {
            kind: "prompt".into(),
            name: "review".into(),
            ..Default::default()
        }),
        doc: b"review this".to_vec(),
        files: Vec::new(),
    };
    assert!(lib.backup(backup(did.clone())).await.is_err());
    let bid = gw
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_backup".into(),
                library: vec![LibraryRef {
                    kind: "prompt".into(),
                    name: "review".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let got = lib.backup(backup(bid)).await.unwrap().into_inner();
    assert_eq!(got.stored.unwrap().version, 1);

    // Reconcile: current, push, and never install for a hook.
    let inv = pb::LibraryInventory {
        items: vec![
            pb::LibraryRef {
                kind: "prompt".into(),
                name: "review".into(),
                sha256: kls_core::belai::library::sha256_hex(b"review this"),
                ..Default::default()
            },
            pb::LibraryRef {
                kind: "hook".into(),
                name: "guard".into(),
                ..Default::default()
            },
        ],
    };
    let v = lib.reconcile(inv).await.unwrap().into_inner().verdicts;
    assert_eq!(v[0].verdict, "current");
    assert_eq!(v[1].verdict, "push");
    assert!(v.iter().all(|x| x.verdict != "install"));

    core.set_factory_stopped(true, "test").await.unwrap();
    assert!(
        gw.dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_install".into(),
                library: vec![r],
                ..Default::default()
            },
        )
        .await
        .is_err()
    );
    let audited = core
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "library")
        .count();
    assert!(audited >= 2, "{audited}");
}

/// WP-F: a worker dispatch carries its provider key and the MCP credential
/// its binding names, scoped to the node; the outbox never holds a value; the
/// grant lives no longer than the worker may wait for work.
#[tokio::test]
async fn worker_and_mcp_secrets_in_the_grant() {
    use kls_core::gateway::McpBinding;
    let h = start().await;
    let key_dir = tempfile::tempdir().unwrap();
    kls_core::vault::install_vault(
        h.core(),
        kls_core::vault::KeyBackend::File(key_dir.path().join("vault.key")),
    )
    .await
    .unwrap();
    const KEY: &str = "sk-worker-NEVER-ON-DISK-1";
    const MCP: &str = "ghp_mcp_NEVER_ON_DISK_2";
    kls_core::vault::put_secret(
        h.core(),
        "anthropic",
        "ANTHROPIC_API_KEY",
        "t",
        KEY.to_string().into(),
    )
    .await
    .unwrap();
    kls_core::vault::put_env_secret(h.core(), "GH_MCP_TOKEN", "", MCP.to_string().into())
        .await
        .unwrap();
    let n = join(&h, rc_caps()).await;
    let (_tx, mut down) = n.link().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.core()
        .gateway()
        .unwrap()
        .dispatch(
            &n.id,
            DispatchSpec {
                kind: "worker".into(),
                cwd: "/work".into(),
                agent: "belai:patcher".into(),
                stay: true,
                stay_idle_max_s: 600,
                vault_providers: vec!["anthropic".into()],
                mcp: vec![
                    McpBinding {
                        server: "github".into(),
                        key: "token".into(),
                        secret: "GH_MCP_TOKEN".into(),
                    },
                    McpBinding {
                        server: "other".into(),
                        key: "token".into(),
                        secret: "NOT_IN_VAULT".into(),
                    },
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    let g = d.vault.unwrap();
    assert!(g.secrets.iter().any(|s| s.value == KEY));
    assert_eq!(g.mcp.len(), 1);
    assert_eq!(
        (g.mcp[0].server.as_str(), g.mcp[0].value.as_str()),
        ("github", MCP)
    );
    // Capped at the worker's idle wait.
    assert!(g.expires_at_ms - kls_store::now_ms() <= 600_000);
    // The database never holds a value.
    for (_, bytes) in h.core().store().outbox_after(&n.id, 0).await.unwrap() {
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            !s.contains(KEY) && !s.contains(MCP),
            "a value reached the outbox"
        );
    }
}

/// WP-H: a node files a forge request (no bundle); the coordinator's steer
/// reaches the worker's node as a dispatch with every field; the kill
/// switch refuses a steer.
#[tokio::test]
async fn forge_request_and_steer() {
    let mut caps = rc_caps();
    caps.dispatch_kinds.push("steer".into());
    let h = start().await;
    let n = join(&h, caps).await;
    let (_tx, mut down) = n.link().await;
    let mut forge = kls_proto::v1::forge_client::ForgeClient::new(n.chan.clone());
    let req = pb::ForgeRequest {
        request_id: "0123456789abcdef".into(),
        item_id: "card-7".into(),
        worker_id: "w-1".into(),
        kind: "pull_request".into(),
        repo_owner: "acme".into(),
        repo_name: "api".into(),
        branch: "belai/K-7/a1".into(),
        head_sha: "b".repeat(40),
        title: "fix it".into(),
        ..Default::default()
    };
    let part = |r: pb::ForgeRequest, chunk: Vec<u8>| pb::ForgePart {
        request: Some(r),
        chunk,
        last: true,
    };
    let filed = forge
        .file(tokio_stream::iter(vec![part(req.clone(), vec![])]))
        .await
        .unwrap()
        .into_inner();
    assert!(!filed.duplicate);
    // A bundle is not taken.
    assert!(
        forge
            .file(tokio_stream::iter(vec![part(
                req.clone(),
                b"PACK".to_vec()
            )]))
            .await
            .is_err()
    );
    let core = h.core();
    let id = core
        .forge_steer(
            &n.id,
            "0123456789abcdef",
            "took_over",
            12,
            "https://github.com/acme/api/pull/12",
            0,
            "",
        )
        .await
        .unwrap();
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("{f:?}")
    };
    assert_eq!(d.id, id);
    assert_eq!(
        (
            d.kind.as_str(),
            d.worker_id.as_str(),
            d.card_id.as_str(),
            d.steer_event.as_str(),
            d.steer_pr
        ),
        ("steer", "w-1", "card-7", "took_over", 12)
    );
    core.set_factory_stopped(true, "test").await.unwrap();
    assert!(
        core.forge_steer(&n.id, "0123456789abcdef", "failed", 0, "", 0, "auth")
            .await
            .is_err()
    );
}

/// WP-K: a scan's report is taken only for that node's scan dispatch; an
/// import names the scanned item and its hash, and a copy whose hash differs
/// from the scan's is refused.
#[tokio::test]
async fn library_scan_and_import() {
    let mut caps = rc_caps();
    caps.dispatch_kinds
        .extend(["library_scan", "library_import"].map(String::from));
    let h = start().await;
    let a = join(&h, caps.clone()).await;
    let b = join(&h, caps).await;
    let core = h.core();
    let gw = core.gateway().unwrap();
    let scan = gw
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_scan".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sha = "a".repeat(64);
    let report = |dispatch: &str| pb::LibraryScanReport {
        dispatch_id: dispatch.into(),
        found: vec![pb::LibraryRef {
            kind: "prompt".into(),
            name: "review".into(),
            sha256: sha.clone(),
            ..Default::default()
        }],
        paths: vec!["/home/u/.claude/commands/review.md".into()],
        harnesses: vec!["claude-code".into()],
        verdicts: vec!["ok".into()],
        partial: false,
    };
    let lib = |n: &TestNode| kls_proto::v1::library_client::LibraryClient::new(n.chan.clone());
    assert!(lib(&b).scan_report(report(&scan)).await.is_err());
    lib(&a).scan_report(report(&scan)).await.unwrap();
    let import = core.library_import(&scan, 0, "").await.unwrap();
    let d = core.store().dispatch(&import).await.unwrap().unwrap();
    assert_eq!(d.kind, "library_import");
    let upload = |hash: &str| pb::LibraryDoc {
        dispatch_id: import.clone(),
        r#ref: Some(pb::LibraryRef {
            kind: "prompt".into(),
            name: "review".into(),
            sha256: hash.into(),
            ..Default::default()
        }),
        doc: b"review it".to_vec(),
        files: Vec::new(),
    };
    assert!(lib(&a).backup(upload(&"b".repeat(64))).await.is_err());
    let got = lib(&a).backup(upload(&sha)).await.unwrap().into_inner();
    assert_eq!(got.stored.unwrap().name, "review");
}

/// A frame another process queued (a `keleustes` CLI command writes the
/// outbox; this process's sweep streams it) is not skipped when this
/// process pushes a later frame live: both reach the node, in seq order, and
/// the earlier one is not marked delivered by the ack of the later one.
#[tokio::test]
async fn frame_queued_by_another_process_is_not_skipped_by_a_live_push() {
    use prost::Message;
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let (_tx, mut down) = n.link().await;
    // The CLI's frame: in the outbox only, not on the live stream.
    let cli = pb::ServerFrame {
        seq: 0,
        ack: 0,
        body: Some(server_frame::Body::Dispatch(pb::Dispatch {
            id: "from-cli".into(),
            kind: "library_install".into(),
            ..Default::default()
        })),
    };
    let cli_seq = h
        .core()
        .store()
        .outbox_push(&n.id, &cli.encode_to_vec())
        .await
        .unwrap();
    // This process pushes the next frame at once (a control broadcast).
    let live_seq = h
        .hub
        .send(
            &n.id,
            server_frame::Body::Control(pb::FactoryControl::default()),
        )
        .await
        .unwrap();
    assert_eq!(live_seq, cli_seq + 1);
    let first = next(&mut down).await;
    assert_eq!(first.seq, cli_seq as u64, "the CLI's frame comes first");
    assert!(matches!(&first.body, Some(server_frame::Body::Dispatch(d)) if d.id == "from-cli"));
    let second = next(&mut down).await;
    assert_eq!(second.seq, live_seq as u64);
    assert!(matches!(second.body, Some(server_frame::Body::Control(_))));
}

/// Only the process serving nodes pushes FactoryControl: a CLI process on
/// the same data directory (which opens a hub but serves nothing) queues no
/// control frame, so it can never append a state that a later change made
/// stale. The server sends the state when it learns of a change.
#[tokio::test]
async fn only_the_serving_process_broadcasts_control() {
    let h = start().await;
    let n = join(&h, rc_caps()).await;
    let cli = Core::open(h.core().data_dir(), None).await.unwrap();
    let _cli_hub = hub(cli.clone()).await.unwrap();
    let queued =
        |c: Core, id: String| async move { c.store().outbox_after(&id, 0).await.unwrap().len() };
    let before = queued(h.core().clone(), n.id.clone()).await;
    cli.set_factory_stopped(true, "drill").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        queued(h.core().clone(), n.id.clone()).await,
        before,
        "the CLI process queued a control frame"
    );
    h.core().set_factory_stopped(false, "").await.unwrap();
    let mut after = before;
    for _ in 0..50 {
        after = queued(h.core().clone(), n.id.clone()).await;
        if after > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(after, before + 1, "the server sends the state");
}

/// P5 (#21, #22, #38): a crew a host backs up is stored as a library crew
/// and fetched for another host's install; the host's crew hash reads
/// "current". A hook with bundle files is fetched only after the owner's
/// second confirmation of exactly those files, with each file and its hash;
/// a file changed after the confirmation is refused. Knowledge and plugin
/// advert entries are "listed". Under the kill switch the fetch, the backup
/// and the confirmation are refused. Each step is audited.
#[tokio::test]
async fn crew_round_trip_and_hook_files_confirmation() {
    use kls_core::gateway::LibraryRef;
    let mut caps = rc_caps();
    caps.dispatch_kinds.push("library_install".into());
    caps.dispatch_kinds.push("library_backup".into());
    let h = start().await;
    let a = join(&h, caps.clone()).await;
    let b = join(&h, caps).await;
    let core = h.core();
    let gw = core.gateway().unwrap();
    let crew_ref = || LibraryRef {
        kind: "crew".into(),
        name: "builders".into(),
        ..Default::default()
    };
    let pb_ref = |kind: &str, name: &str| pb::LibraryRef {
        kind: kind.into(),
        name: name.into(),
        ..Default::default()
    };

    // Host A backs its crew up.
    let canonical = "{\n  \"id\": \"0b0e6f1e-1c2d-4e5f-8a9b-0c1d2e3f4a5b\",\n  \"name\": \"builders\",\n  \"description\": \"\",\n  \"members\": [\n    {\n      \"profile\": \"crew-builder\",\n      \"replicas\": 2\n    }\n  ]\n}\n";
    let crew_doc = serde_json::to_vec(&serde_json::json!({
        "name": "builders", "description": "", "process": "parallel",
        "members": [{"agent": "crew-builder", "role": "", "count": 2}],
        "meta": {"belaiCrew": canonical},
    }))
    .unwrap();
    let bid = gw
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_backup".into(),
                library: vec![crew_ref()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut lib_a = kls_proto::v1::library_client::LibraryClient::new(a.chan.clone());
    let stored = lib_a
        .backup(pb::LibraryDoc {
            dispatch_id: bid,
            r#ref: Some(pb_ref("crew", "builders")),
            doc: crew_doc.clone(),
            files: Vec::new(),
        })
        .await
        .unwrap()
        .into_inner()
        .stored
        .unwrap();
    assert_eq!(stored.version, 1);
    // Item 6: the crew also lands in the Crews table.
    let crews = core.store().crews().await.unwrap();
    assert!(
        crews
            .iter()
            .any(|c| c.name == "builders" && c.members[0].count == 2),
        "{crews:?}"
    );
    // The advert's crew hash (of the canonical form) reads current.
    let v = lib_a
        .reconcile(pb::LibraryInventory {
            items: vec![
                pb::LibraryRef {
                    sha256: kls_core::belai::library::sha256_hex(canonical.as_bytes()),
                    ..pb_ref("crew", "builders")
                },
                pb_ref("knowledge", "demo"),
                pb_ref("plugin", "fmt"),
            ],
        })
        .await
        .unwrap()
        .into_inner()
        .verdicts;
    assert_eq!(v[0].verdict, "current");
    assert_eq!(
        (v[1].verdict.as_str(), v[2].verdict.as_str()),
        ("listed", "listed")
    );

    // Host B installs it: the fetch answers the same document.
    let iid = gw
        .dispatch(
            &b.id,
            DispatchSpec {
                kind: "library_install".into(),
                library: vec![crew_ref()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut lib_b = kls_proto::v1::library_client::LibraryClient::new(b.chan.clone());
    let fetch = |lib: &mut kls_proto::v1::library_client::LibraryClient<Channel>,
                 dispatch: &str,
                 r: pb::LibraryRef| {
        let req = pb::LibraryFetch {
            dispatch_id: dispatch.into(),
            teleport_id: String::new(),
            r#ref: Some(r),
        };
        let mut lib = lib.clone();
        async move { lib.fetch(req).await }
    };
    let got = fetch(&mut lib_b, &iid, pb_ref("crew", "builders"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.doc, crew_doc);

    // A hook with a script: saved through the validator, files attached.
    let hook = r#"{"name":"guard","description":"Guards","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"guard.sh"}]}]}}"#;
    let (hid, hv) = kls_core::vault::save_library(core, "", "hook", "guard", "", hook, "")
        .await
        .unwrap();
    let script = b"#!/bin/sh\necho '{}'\n".to_vec();
    core.store()
        .save_bundle_files(&hid, hv, &[("guard.sh".into(), script.clone())])
        .await
        .unwrap();
    let hook_dispatch = || async {
        gw.dispatch(
            &b.id,
            DispatchSpec {
                kind: "library_install".into(),
                library: vec![LibraryRef {
                    kind: "hook".into(),
                    name: "guard".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap()
    };
    // Without the second confirmation: refused.
    let d1 = hook_dispatch().await;
    let e = fetch(&mut lib_b, &d1, pb_ref("hook", "guard"))
        .await
        .unwrap_err();
    assert!(e.message().contains("second confirmation"), "{e:?}");
    // Confirmed for host A only: still refused for B.
    core.confirm_bundle_files(&a.id, &hid, 0, "console:library")
        .await
        .unwrap();
    assert!(
        fetch(&mut lib_b, &d1, pb_ref("hook", "guard"))
            .await
            .is_err()
    );
    // Confirmed for B: the files travel with their hashes.
    let listed = core
        .confirm_bundle_files(&b.id, &hid, 0, "console:library")
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let got = fetch(&mut lib_b, &d1, pb_ref("hook", "guard"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.files.len(), 1);
    assert_eq!(got.files[0].path, "guard.sh");
    assert_eq!(got.files[0].data, script);
    assert_eq!(
        got.files[0].sha256,
        kls_core::belai::library::sha256_hex(&script)
    );
    // A file changed after the confirmation: refused again.
    core.store()
        .save_bundle_files(
            &hid,
            hv,
            &[("guard.sh".into(), b"#!/bin/sh\nrm -rf /\n".to_vec())],
        )
        .await
        .unwrap();
    assert!(
        fetch(&mut lib_b, &d1, pb_ref("hook", "guard"))
            .await
            .is_err()
    );

    // The kill switch refuses the fetch, the backup and the confirmation.
    core.confirm_bundle_files(&b.id, &hid, 0, "console:library")
        .await
        .unwrap();
    let d2 = hook_dispatch().await;
    let bid2 = gw
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "library_backup".into(),
                library: vec![crew_ref()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    core.set_factory_stopped(true, "test").await.unwrap();
    let e = fetch(&mut lib_b, &d2, pb_ref("hook", "guard"))
        .await
        .unwrap_err();
    assert!(e.message().contains("refused"), "{e:?}");
    assert!(
        core.confirm_bundle_files(&b.id, &hid, 0, "console:library")
            .await
            .is_err()
    );
    assert!(
        core.library_backup(&a.id, &bid2, &crew_ref(), &crew_doc)
            .await
            .is_err_and(|e| kls_core::control::is_blocked(&e))
    );
    core.set_factory_stopped(false, "").await.unwrap();
    let msgs: Vec<String> = core
        .audit_entries(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "library")
        .map(|e| e.msg)
        .collect();
    assert!(
        msgs.iter().any(|m| m.contains("crew builders backed up")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("files were not confirmed")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("confirmed for install")),
        "{msgs:?}"
    );
}
