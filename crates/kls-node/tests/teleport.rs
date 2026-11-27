//! M6 teleport tests over real mutual TLS on loopback: a requested bundle
//! reaches its origin as a `teleport` job, uploads with SHA-256 checks and
//! a size cap, and downloads only to the node it is addressed to.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kls_core::Core;
use kls_core::audit::kinds;
use kls_node::{Hub, hub, pki, run, token};
use kls_proto::v1::jobs_client::JobsClient;
use kls_proto::v1::node_client::NodeClient;
use kls_proto::v1::teleport_client::TeleportClient;
use kls_proto::v1::{self as pb, node_frame, server_frame};
use rcgen::{CertificateParams, KeyPair};
use sha2::{Digest, Sha256};
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

struct TestNode {
    id: String,
    chan: Channel,
}

impl TestNode {
    fn teleport(&self) -> TeleportClient<Channel> {
        TeleportClient::new(self.chan.clone())
    }
}

async fn join(h: &Harness, name: &str) -> TestNode {
    let ca = h.hub.pki_ca_pem();
    let minted = h.hub.mint_token(name, token::DEFAULT_TTL_MS).await.unwrap();
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
                hostname: name.into(),
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

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// The parts of an upload: header first, then `chunk`-sized pieces.
fn parts(id: &str, header: pb::TeleportHeader, body: &[u8], chunk: usize) -> Vec<pb::TeleportPart> {
    let pieces: Vec<&[u8]> = if body.is_empty() {
        vec![&[]]
    } else {
        body.chunks(chunk).collect()
    };
    let n = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, c)| pb::TeleportPart {
            teleport_id: id.into(),
            header: (i == 0).then(|| header.clone()),
            chunk: c.to_vec(),
            last: i + 1 == n,
        })
        .collect()
}

async fn upload(
    n: &TestNode,
    parts: Vec<pb::TeleportPart>,
) -> Result<pb::TeleportReceipt, tonic::Status> {
    n.teleport()
        .upload(tokio_stream::iter(parts))
        .await
        .map(|r| r.into_inner())
}

async fn download(n: &TestNode, id: &str) -> Result<(pb::TeleportHeader, Vec<u8>), tonic::Status> {
    let mut s = n
        .teleport()
        .download(pb::TeleportRequest {
            teleport_id: id.into(),
            ..Default::default()
        })
        .await?
        .into_inner();
    let mut header = None;
    let mut body = Vec::new();
    while let Some(p) = s.message().await? {
        if let Some(h) = p.header {
            header = Some(h);
        }
        body.extend_from_slice(&p.chunk);
        if p.last {
            break;
        }
    }
    Ok((header.expect("header on the first part"), body))
}

async fn wait_status(core: &Core, id: &str, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let t = core.store().teleport(id).await.unwrap().unwrap();
        if t.status == want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "teleport stuck at {}",
            t.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn requested_teleport_round_trip() {
    let h = start().await;
    let from = join(&h, "origin").await;
    let to = join(&h, "target").await;
    let other = join(&h, "other").await;

    // The origin holds its link, so it receives the teleport job.
    let (link_tx, link_rx) = mpsc::channel(8);
    link_tx
        .send(pb::NodeFrame {
            body: Some(node_frame::Body::Hello(pb::Hello::default())),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut down = NodeClient::new(from.chan.clone())
        .link(ReceiverStream::new(link_rx))
        .await
        .unwrap()
        .into_inner();
    let w = down.message().await.unwrap().unwrap();
    assert!(matches!(w.body, Some(server_frame::Body::Welcome(_))));

    let mut bus = h.core().subscribe();
    let t = h
        .core()
        .teleport_request("sess-1", &from.id, &to.id)
        .await
        .unwrap();
    let job = loop {
        let f = tokio::time::timeout(Duration::from_secs(5), down.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(server_frame::Body::Job(j)) = f.body {
            break j;
        }
    };
    assert_eq!(job.kind, "teleport");
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).unwrap();
    assert_eq!(payload["teleportId"], t.id.as_str());
    assert_eq!(payload["toNode"], to.id.as_str());

    // 1 MiB + change, sent in odd-sized chunks.
    let body: Vec<u8> = (0..(1 << 20) + 4321).map(|i| (i % 251) as u8).collect();
    let header = pb::TeleportHeader {
        session_id: "sess-1".into(),
        from_node: from.id.clone(),
        to_node: to.id.clone(),
        title: "fix the build".into(),
        size: body.len() as i64,
        sha256: sha(&body),
        ..Default::default()
    };

    // Only the origin may upload it.
    let e = upload(&other, parts(&t.id, header.clone(), &body, 70_000))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied);
    // A wrong hash is refused and the teleport stays open for a retry.
    let mut bad = header.clone();
    bad.sha256 = sha(b"something else");
    let e = upload(&from, parts(&t.id, bad, &body, 70_000))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::DataLoss);
    // More bytes than the header said: capped.
    let mut short = header.clone();
    short.size = 1000;
    let e = upload(&from, parts(&t.id, short, &body, 70_000))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::ResourceExhausted);
    // A stream that never sends `last`.
    let mut cut = parts(&t.id, header.clone(), &body, 70_000);
    cut.pop();
    let e = upload(&from, cut).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    // Too large for any teleport.
    let mut huge = header.clone();
    huge.size = kls_core::audit::TELEPORT_MAX_BYTES as i64 + 1;
    let e = upload(&from, parts(&t.id, huge, b"x", 10))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        h.core()
            .store()
            .teleport(&t.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "requested"
    );

    // Not downloadable before the bundle is in.
    let e = download(&to, &t.id).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);

    let receipt = upload(&from, parts(&t.id, header.clone(), &body, 70_000))
        .await
        .unwrap();
    assert_eq!(receipt.teleport_id, t.id);
    assert_eq!(receipt.size, body.len() as i64);
    assert!(h.core().blobs().exists(&sha(&body)).await);
    // Uploading twice is refused.
    let e = upload(&from, parts(&t.id, header.clone(), &body, 70_000))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);
    // The origin reports its job done.
    JobsClient::new(from.chan.clone())
        .complete(pb::JobResult {
            job_id: job.id.clone(),
            status: "ok".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    // Only the addressed node may download it.
    for n in [&from, &other] {
        let e = download(n, &t.id).await.unwrap_err();
        assert_eq!(e.code(), tonic::Code::PermissionDenied);
    }
    let (hd, got) = download(&to, &t.id).await.unwrap();
    assert_eq!(got, body);
    assert_eq!(hd.sha256, sha(&body));
    assert_eq!(hd.title, "fix the build");
    assert_eq!(hd.from_node, from.id);
    wait_status(h.core(), &t.id, "delivered").await;
    // A retry after an interrupted download still works.
    assert_eq!(download(&to, &t.id).await.unwrap().1, body);

    let e = download(&to, "no-such-teleport").await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::NotFound);

    // Events: requested, uploaded, downloaded.
    let mut seen = Vec::new();
    while let Ok(ev) = bus.try_recv() {
        if ev.kind.starts_with("teleport.") && ev.subject == t.id {
            seen.push(ev.kind);
        }
    }
    for k in [
        kinds::TELEPORT_REQUESTED,
        kinds::TELEPORT_UPLOADED,
        kinds::TELEPORT_DOWNLOADED,
    ] {
        assert!(seen.iter().any(|s| s == k), "{k} missing from {seen:?}");
    }

    // Expired bundles are gone.
    h.core()
        .teleport_sweep_at(kls_store::now_ms() + 365 * 86_400_000)
        .await
        .unwrap();
    let e = download(&to, &t.id).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);
    assert!(!h.core().blobs().exists(&sha(&body)).await);

    // The ledger recorded the teleport.
    h.core().audit_sync().await.unwrap();
    let entries = h
        .core()
        .audit_entries(&kls_core::audit::LedgerQuery {
            kinds: vec!["teleport".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(entries.len() >= 3, "{entries:?}");
}

#[tokio::test]
async fn node_started_teleport() {
    let h = start().await;
    let from = join(&h, "origin").await;
    let to = join(&h, "target").await;
    let body = b"a small bundle".to_vec();
    let header = pb::TeleportHeader {
        session_id: "s9".into(),
        to_node: to.id.clone(),
        size: body.len() as i64,
        sha256: sha(&body),
        ..Default::default()
    };
    // No header on the first part.
    let mut no_header = parts("", header.clone(), &body, 4);
    no_header[0].header = None;
    let e = upload(&from, no_header).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    // Claiming to be another node.
    let mut spoof = header.clone();
    spoof.from_node = to.id.clone();
    let e = upload(&from, parts("", spoof, &body, 4)).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied);

    let r = upload(&from, parts("", header, &body, 4)).await.unwrap();
    assert!(!r.teleport_id.is_empty());
    let t = h
        .core()
        .store()
        .teleport(&r.teleport_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (t.from_node.as_str(), t.to_node.as_str()),
        (from.id.as_str(), to.id.as_str())
    );
    assert_eq!(t.status, "ready");
    assert_eq!(download(&to, &t.id).await.unwrap().1, body);

    // A revoked target cannot download.
    h.hub.revoke(&to.id, "test").await.unwrap();
    let e = download(&to, &t.id).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied);
}
