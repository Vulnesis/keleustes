//! M1 end-to-end tests over real TLS on loopback: session mirroring, the
//! session inbox, dispatches and jobs.

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

fn entry(index: u64, text: &str) -> pb::Entry {
    pb::Entry {
        index,
        role: "assistant".into(),
        text: text.into(),
        at_ms: index as i64,
        meta_json: String::new(),
    }
}

async fn next<T>(s: &mut tonic::Streaming<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), s.message())
        .await
        .expect("stream timed out")
        .unwrap()
        .expect("stream ended")
}

/// Waits until `cond` holds for the stored dispatch.
async fn wait_dispatch(
    core: &Core,
    id: &str,
    cond: impl Fn(&kls_store::sessions::Dispatch) -> bool,
) -> kls_store::sessions::Dispatch {
    let mut bus = core.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let d = core.store().dispatch(id).await.unwrap().unwrap();
        if cond(&d) {
            return d;
        }
        tokio::time::timeout_at(deadline, bus.recv())
            .await
            .unwrap_or_else(|_| panic!("dispatch stayed {:?}", d.status))
            .ok();
    }
}

#[tokio::test]
async fn upsert_append_end_are_idempotent_and_owned() {
    let h = start().await;
    let a = join(&h, pb::Capabilities::default()).await;
    let b = join(&h, pb::Capabilities::default()).await;
    let mut sa = a.sessions();

    sa.upsert(upsert("s1")).await.unwrap();
    let r = sa
        .append(pb::SessionEntries {
            session_id: "s1".into(),
            entries: vec![entry(0, "a"), entry(1, "b")],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(r.next_index, 2);
    // A retry overlapping what is stored only adds the new entry.
    let r = sa
        .append(pb::SessionEntries {
            session_id: "s1".into(),
            entries: vec![entry(1, "B!"), entry(2, "c")],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(r.next_index, 3);
    let texts: Vec<String> = h
        .core()
        .store()
        .entries("s1", 0, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.text)
        .collect();
    assert_eq!(texts, ["a", "b", "c"]);

    // A second upsert with empty fields keeps the stored ones.
    sa.upsert(pb::SessionUpsert {
        session_id: "s1".into(),
        state: "waiting".into(),
        pending_question_id: "q1".into(),
        pending_question: "Run the migration?".into(),
        input_tokens: 1200,
        ..Default::default()
    })
    .await
    .unwrap();
    let s = h.core().store().session("s1").await.unwrap().unwrap();
    assert_eq!(s.node_id, a.id);
    assert_eq!(s.title, "fix the build");
    assert_eq!(s.state, "waiting");
    assert_eq!(s.pending_question, "Run the migration?");
    assert_eq!(s.input_tokens, 1200);
    assert_eq!(s.entry_count, 3);

    // Another node can neither take it over nor write to it.
    let mut sb = b.sessions();
    let err = sb.upsert(upsert("s1")).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = sb
        .append(pb::SessionEntries {
            session_id: "s1".into(),
            entries: vec![entry(3, "x")],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = sb
        .inbox(pb::InboxRequest {
            session_id: "s1".into(),
            after: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = sa.upsert(pb::SessionUpsert::default()).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // End twice is fine; then the inbox refuses new items.
    for _ in 0..2 {
        sa.end(pb::SessionEnd {
            session_id: "s1".into(),
            ended_at_ms: 99,
            reason: "completed".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let s = h.core().store().session("s1").await.unwrap().unwrap();
    assert_eq!((s.ended_at, s.end_reason.as_str()), (Some(99), "completed"));
    assert!(h.core().queue_prompt("s1", "more").await.is_err());
    let err = sa
        .end(pb::SessionEnd {
            session_id: "nope".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);

    // Audit lines are stored raw and idempotently.
    let line = |index| pb::AuditLine {
        index,
        at_ms: 1,
        kind: "tool".into(),
        json: "{}".into(),
    };
    for _ in 0..2 {
        sa.audit(pb::AuditBatch {
            session_id: "s1".into(),
            lines: vec![line(0), line(1)],
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let lines = h
        .core()
        .store()
        .audit_lines(&a.id, "s1", 0, 0)
        .await
        .unwrap();
    assert_eq!(lines.len(), 2);
}

#[tokio::test]
async fn inbox_replays_streams_live_and_keeps_until_acked() {
    let h = start().await;
    let a = join(&h, pb::Capabilities::default()).await;
    let mut sa = a.sessions();
    sa.upsert(upsert("s1")).await.unwrap();
    let core = h.core();

    // Queued before the session subscribes: replayed.
    let p = core.queue_prompt("s1", "add tests").await.unwrap();
    let mut inbox = sa
        .inbox(pb::InboxRequest {
            session_id: "s1".into(),
            after: 0,
        })
        .await
        .unwrap()
        .into_inner();
    let got = next(&mut inbox).await;
    assert_eq!((got.seq, got.id.as_str()), (1, p.id.as_str()));
    assert!(matches!(got.body, Some(inbox_item::Body::Prompt(ref x)) if x.text == "add tests"));

    // Queued while subscribed: pushed live.
    let q = core.queue_answer("s1", "q1", "yes").await.unwrap();
    let got = next(&mut inbox).await;
    assert_eq!(got.seq, 2);
    assert!(
        matches!(got.body, Some(inbox_item::Body::Answer(ref x)) if x.question_id == "q1" && x.text == "yes")
    );
    let stored = core.store().inbox_item(&q.id).await.unwrap().unwrap();
    assert!(stored.delivered_at.is_some());
    assert_eq!(stored.status, "delivered");

    // Ack the prompt only, then reconnect from scratch: the answer comes again.
    sa.ack(pb::InboxAck {
        session_id: "s1".into(),
        item_id: p.id.clone(),
        status: "done".into(),
        detail: String::new(),
    })
    .await
    .unwrap();
    drop(inbox);
    let mut inbox = sa
        .inbox(pb::InboxRequest {
            session_id: "s1".into(),
            after: 0,
        })
        .await
        .unwrap()
        .into_inner();
    let got = next(&mut inbox).await;
    assert_eq!(got.id, q.id);

    // Resuming after the applied seq skips it; new commands still arrive.
    drop(inbox);
    let mut inbox = sa
        .inbox(pb::InboxRequest {
            session_id: "s1".into(),
            after: 2,
        })
        .await
        .unwrap()
        .into_inner();
    core.queue_command("s1", "stop", &[]).await.unwrap();
    let got = next(&mut inbox).await;
    assert_eq!(got.seq, 3);
    assert!(matches!(got.body, Some(inbox_item::Body::Command(ref c)) if c.name == "stop"));

    sa.ack(pb::InboxAck {
        session_id: "s1".into(),
        item_id: q.id.clone(),
        status: "refused".into(),
        detail: "the turn moved on".into(),
    })
    .await
    .unwrap();
    let stored = core.store().inbox_item(&q.id).await.unwrap().unwrap();
    assert_eq!(stored.status, "refused");
    let err = sa
        .ack(pb::InboxAck {
            session_id: "s1".into(),
            item_id: "nope".into(),
            status: "done".into(),
            detail: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn dispatch_delivered_on_link_and_completed() {
    let h = start().await;
    let a = join(
        &h,
        pb::Capabilities {
            remote_control: true,
            ..Default::default()
        },
    )
    .await;
    let gw = h.core().gateway().unwrap();

    // Queued while the node is offline, replayed when it links.
    let spec = DispatchSpec {
        kind: "session".into(),
        cwd: "/work".into(),
        prompt: "fix the build".into(),
        model: "opus".into(),
        mode: "agent".into(),
        ..Default::default()
    };
    let did = gw.dispatch(&a.id, spec.clone()).await.unwrap();
    let d = h.core().store().dispatch(&did).await.unwrap().unwrap();
    assert_eq!(d.status, "queued");
    assert!(d.outbox_seq > 0);

    let (tx, mut down) = a.link().await;
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(ref got)) = f.body else {
        panic!("expected a dispatch, got {f:?}");
    };
    assert_eq!(got.id, did);
    assert_eq!(
        (got.cwd.as_str(), got.prompt.as_str()),
        ("/work", "fix the build")
    );
    assert_eq!(f.seq as i64, d.outbox_seq);

    // The node acks the frame on Link: delivered.
    tx.send(pb::NodeFrame {
        ack: f.seq,
        body: Some(node_frame::Body::Heartbeat(pb::Heartbeat { sent_at_ms: 1 })),
        ..Default::default()
    })
    .await
    .unwrap();
    wait_dispatch(h.core(), &did, |d| d.status == "delivered").await;

    // The outcome comes through Jobs.Complete with job_id = dispatch id.
    a.jobs()
        .complete(pb::JobResult {
            job_id: did.clone(),
            status: "ok".into(),
            session_id: "s-started".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = wait_dispatch(h.core(), &did, |d| d.status == "started").await;
    assert_eq!(d.session_id, "s-started");

    // A live dispatch the node refuses ends failed with the reason.
    let did2 = gw.dispatch(&a.id, spec).await.unwrap();
    loop {
        let f = next(&mut down).await;
        if matches!(f.body, Some(server_frame::Body::Dispatch(ref x)) if x.id == did2) {
            break;
        }
    }
    a.jobs()
        .complete(pb::JobResult {
            job_id: did2.clone(),
            status: "refused".into(),
            error: "directory is not trusted".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let d = h.core().store().dispatch(&did2).await.unwrap().unwrap();
    assert_eq!(d.status, "failed");
    assert!(d.error.contains("not trusted"));

    // Another node cannot report it; unknown ids are refused.
    let b = join(&h, pb::Capabilities::default()).await;
    let err = b
        .jobs()
        .complete(pb::JobResult {
            job_id: did.clone(),
            status: "failed".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    let err = a
        .jobs()
        .complete(pb::JobResult {
            job_id: "nope".into(),
            status: "ok".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
    assert!(gw.dispatch("nope", DispatchSpec::default()).await.is_err());
}

/// Reads the link until a Job frame arrives and returns it.
async fn next_job(down: &mut tonic::Streaming<pb::ServerFrame>) -> pb::Job {
    loop {
        if let Some(server_frame::Body::Job(j)) = next(down).await.body {
            return j;
        }
    }
}

#[tokio::test]
async fn jobs_route_by_capability_and_time_out() {
    let h = start().await;
    let gh = join(
        &h,
        pb::Capabilities {
            gh_authenticated: true,
            ..Default::default()
        },
    )
    .await;
    let model = join(
        &h,
        pb::Capabilities {
            providers: vec!["anthropic".into()],
            ..Default::default()
        },
    )
    .await;
    let (_gtx, mut gdown) = gh.link().await;
    let (_mtx, mut mdown) = model.link().await;
    let gw = h.core().gateway().unwrap();
    let mut online = gw.online().await;
    online.sort();
    let mut want = vec![gh.id.clone(), model.id.clone()];
    want.sort();
    assert_eq!(online, want);

    // A GitHub job goes to the node signed in to gh.
    let gw2 = gw.clone();
    let run = tokio::spawn(async move {
        gw2.job(
            None,
            "github",
            r#"{"args":["pr","list"]}"#,
            JobNeeds {
                gh: true,
                provider: None,
                ..Default::default()
            },
            10_000,
        )
        .await
    });
    let job = next_job(&mut gdown).await;
    assert_eq!(job.kind, "github");
    assert!(job.deadline_ms > 0);
    // Only the node it went to may answer.
    let err = model
        .jobs()
        .complete(pb::JobResult {
            job_id: job.id.clone(),
            status: "ok".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    gh.jobs()
        .complete(pb::JobResult {
            job_id: job.id.clone(),
            status: "ok".into(),
            output_json: r#"{"stdout":"[]"}"#.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let out = run.await.unwrap().unwrap();
    assert_eq!(out.status, "ok");
    assert_eq!(out.node_id, gh.id);
    assert_eq!(out.output_json, r#"{"stdout":"[]"}"#);

    // A model job goes to the node with the provider.
    let gw2 = gw.clone();
    let run = tokio::spawn(async move {
        gw2.job(
            None,
            "model",
            r#"{"task":"draft"}"#,
            JobNeeds {
                gh: false,
                provider: Some("anthropic".into()),
                ..Default::default()
            },
            10_000,
        )
        .await
    });
    let job = next_job(&mut mdown).await;
    model
        .jobs()
        .complete(pb::JobResult {
            job_id: job.id,
            status: "failed".into(),
            error: "quota".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let out = run.await.unwrap().unwrap();
    assert_eq!(
        (out.status.as_str(), out.error.as_str()),
        ("failed", "quota")
    );
    assert_eq!(out.node_id, model.id);

    // Nobody qualifies.
    let err = gw
        .job(
            None,
            "model",
            "{}",
            JobNeeds {
                gh: true,
                provider: Some("anthropic".into()),
                ..Default::default()
            },
            1_000,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no online node"), "{err}");
    // An explicit node must meet the needs too.
    assert!(
        gw.job(
            Some(&model.id),
            "github",
            "{}",
            JobNeeds {
                gh: true,
                provider: None,
                ..Default::default()
            },
            1_000
        )
        .await
        .is_err()
    );

    // No answer before the deadline: timeout, and a late answer is ignored.
    let out = gw
        .job(Some(&gh.id), "github", "{}", JobNeeds::default(), 300)
        .await
        .unwrap();
    assert_eq!(out.status, "timeout");
    let job = next_job(&mut gdown).await;
    gh.jobs()
        .complete(pb::JobResult {
            job_id: job.id.clone(),
            status: "ok".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let stored = h.core().store().job(&job.id).await.unwrap().unwrap();
    assert_eq!(stored.status, "timeout");
}

/// Vault keys attached to a dispatch reach the node, but never the outbox
/// table: the stored frame carries the grant without values, and a replay
/// fills them back in from memory.
#[tokio::test]
async fn vault_grant_values_never_reach_the_outbox() {
    let h = start().await;
    let key_dir = tempfile::tempdir().unwrap();
    kls_core::vault::install_vault(
        h.core(),
        kls_core::vault::KeyBackend::File(key_dir.path().join("vault.key")),
    )
    .await
    .unwrap();
    const SECRET: &str = "sk-test-NEVER-ON-DISK-7f3a";
    kls_core::vault::put_secret(
        h.core(),
        "anthropic",
        "ANTHROPIC_API_KEY",
        "test",
        SECRET.to_string().into(),
    )
    .await
    .unwrap();
    let a = join(
        &h,
        pb::Capabilities {
            remote_control: true,
            ..Default::default()
        },
    )
    .await;
    let did = h
        .core()
        .gateway()
        .unwrap()
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "session".into(),
                cwd: "/work".into(),
                prompt: "p".into(),
                vault_providers: vec!["anthropic".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Nothing in the outbox holds the value.
    let stored = h.core().store().outbox_after(&a.id, 0).await.unwrap();
    assert!(!stored.is_empty());
    for (_, bytes) in &stored {
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
            "vault value persisted in node_outbox"
        );
    }

    // The node, linking after the dispatch was queued, still gets the key.
    let (_tx, mut down) = a.link().await;
    let f = next(&mut down).await;
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("expected a dispatch, got {f:?}");
    };
    assert_eq!(d.id, did);
    let g = d.vault.expect("grant attached");
    assert_eq!(g.secrets.len(), 1);
    assert_eq!(g.secrets[0].env, "ANTHROPIC_API_KEY");
    assert_eq!(g.secrets[0].value, SECRET);
}

/// A second process on the same database (a `keleustes` CLI command next to
/// the running app) dispatches and revokes; the app's live link delivers
/// both within its outbox sweep.
#[tokio::test]
async fn other_process_dispatch_and_revoke_reach_the_live_link() {
    let h = start().await;
    let a = join(
        &h,
        pb::Capabilities {
            remote_control: true,
            ..Default::default()
        },
    )
    .await;
    let (_tx, mut down) = a.link().await;

    let cli_core = Core::open(h._dir.path(), None).await.unwrap();
    let cli_hub = hub(cli_core.clone()).await.unwrap();
    let did = cli_core
        .gateway()
        .unwrap()
        .dispatch(
            &a.id,
            DispatchSpec {
                kind: "session".into(),
                cwd: "/work".into(),
                prompt: "from the cli".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let f = tokio::time::timeout(Duration::from_secs(5), next(&mut down))
        .await
        .expect("swept within 5s");
    let Some(server_frame::Body::Dispatch(d)) = f.body else {
        panic!("expected a dispatch, got {f:?}");
    };
    assert_eq!(d.id, did);

    assert!(cli_hub.revoke(&a.id, "from the cli").await.unwrap());
    let f = tokio::time::timeout(Duration::from_secs(5), next(&mut down))
        .await
        .expect("revocation within 5s");
    assert!(matches!(f.body, Some(server_frame::Body::Revoked(_))));
}
