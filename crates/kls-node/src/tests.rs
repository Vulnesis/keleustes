//! End-to-end node server tests over real TLS on loopback.

use super::*;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kls_proto::v1::node_client::NodeClient;
use kls_proto::v1::{self as pb, node_frame, server_frame};
use rcgen::{CertificateParams, KeyPair};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

struct Harness {
    hub: Hub,
    addr: String,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    // Pick a free port.
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
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Harness {
        hub,
        addr: listen,
        _dir: dir,
        _stop: stop,
    }
}

fn pem(tag: &str, der: &[u8]) -> String {
    ::pem::encode(&::pem::Pem::new(tag, der.to_vec()))
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

fn join_request(token: &str, key: &KeyPair) -> pb::JoinRequest {
    let p = token::Payload::decode(token).unwrap();
    let csr = CertificateParams::default().serialize_request(key).unwrap();
    pb::JoinRequest {
        token_id: p.t,
        secret: URL_SAFE_NO_PAD.decode(p.s).unwrap(),
        csr_der: csr.der().to_vec(),
        host: Some(pb::HostAdvert {
            hostname: "worker-1".into(),
            os: "linux".into(),
            capabilities: Some(pb::Capabilities {
                gh_authenticated: true,
                ..Default::default()
            }),
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn join_link_heartbeat_revoke() {
    let h = start().await;
    let ca = h.hub.pki_ca_pem();
    let minted = h.hub.mint_token("", token::DEFAULT_TTL_MS).await.unwrap();
    let payload = token::Payload::decode(&minted.token).unwrap();
    assert_eq!(payload.a, vec![h.addr.clone()]);

    let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let mut client = NodeClient::new(channel(&h.addr, &ca, None).await);
    let joined = client
        .join(join_request(&minted.token, &key))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(joined.tls_server_name, pki::TLS_SERVER_NAME);

    // Single use.
    let again = client
        .join(join_request(&minted.token, &key))
        .await
        .unwrap_err();
    assert_eq!(again.code(), tonic::Code::PermissionDenied);

    // Link without a client certificate is refused.
    let mut anon = NodeClient::new(channel(&h.addr, &ca, None).await);
    let (_t, r) = tokio::sync::mpsc::channel::<pb::NodeFrame>(1);
    let err = anon
        .link(tokio_stream::wrappers::ReceiverStream::new(r))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);

    // mTLS link.
    let id = Identity::from_pem(
        pem("CERTIFICATE", &joined.client_cert_der),
        key.serialize_pem(),
    );
    let mut client = NodeClient::new(channel(&h.addr, &ca, Some(id)).await);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(pb::NodeFrame {
        body: Some(node_frame::Body::Hello(pb::Hello {
            resume_after: 0,
            belai_version: "test".into(),
        })),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut down = client
        .link(tokio_stream::wrappers::ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    let welcome = down.message().await.unwrap().unwrap();
    assert!(
        matches!(welcome.body, Some(server_frame::Body::Welcome(ref w)) if w.node_id == joined.node_id)
    );

    tx.send(pb::NodeFrame {
        seq: 1,
        body: Some(node_frame::Body::Heartbeat(pb::Heartbeat { sent_at_ms: 1 })),
        ..Default::default()
    })
    .await
    .unwrap();
    let ping = down.message().await.unwrap().unwrap();
    assert!(matches!(ping.body, Some(server_frame::Body::Ping(_))));
    assert_eq!(ping.ack, 1);
    assert_eq!(h.hub.online().await, vec![joined.node_id.clone()]);

    let node = h
        .hub
        .core()
        .store()
        .node(&joined.node_id)
        .await
        .unwrap()
        .unwrap();
    assert!(node.host.capabilities.gh_authenticated);
    assert_eq!(node.name, "worker-1");

    // Replayable frame queued while online arrives with a seq.
    h.hub
        .send(
            &joined.node_id,
            server_frame::Body::Dispatch(pb::Dispatch {
                id: "d1".into(),
                kind: "session".into(),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let cmd = down.message().await.unwrap().unwrap();
    assert_eq!(cmd.seq, 1);

    // Revoke closes the link and refuses reconnects.
    assert!(h.hub.revoke(&joined.node_id, "test").await.unwrap());
    let revoked = down.message().await.unwrap().unwrap();
    assert!(matches!(revoked.body, Some(server_frame::Body::Revoked(_))));
    let id = Identity::from_pem(
        pem("CERTIFICATE", &joined.client_cert_der),
        key.serialize_pem(),
    );
    let mut client = NodeClient::new(channel(&h.addr, &ca, Some(id)).await);
    let (_t, r) = tokio::sync::mpsc::channel::<pb::NodeFrame>(1);
    let err = client
        .link(tokio_stream::wrappers::ReceiverStream::new(r))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

async fn open_link(
    addr: &str,
    ca: &str,
    cert_der: &[u8],
    key: &KeyPair,
) -> (
    tokio::sync::mpsc::Sender<pb::NodeFrame>,
    tonic::Streaming<pb::ServerFrame>,
) {
    let id = Identity::from_pem(pem("CERTIFICATE", cert_der), key.serialize_pem());
    let mut client = NodeClient::new(channel(addr, ca, Some(id)).await);
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(pb::NodeFrame {
        body: Some(node_frame::Body::Hello(pb::Hello::default())),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut down = client
        .link(tokio_stream::wrappers::ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    let w = down.message().await.unwrap().unwrap();
    assert!(matches!(w.body, Some(server_frame::Body::Welcome(_))));
    (tx, down)
}

/// A second link for the same node supersedes the first, which is closed;
/// the node stays online until the surviving link ends.
#[tokio::test]
async fn newer_link_supersedes_older() {
    let h = start().await;
    let ca = h.hub.pki_ca_pem();
    let minted = h.hub.mint_token("", token::DEFAULT_TTL_MS).await.unwrap();
    let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let mut client = NodeClient::new(channel(&h.addr, &ca, None).await);
    let joined = client
        .join(join_request(&minted.token, &key))
        .await
        .unwrap()
        .into_inner();

    let (_tx1, mut down1) = open_link(&h.addr, &ca, &joined.client_cert_der, &key).await;
    let (tx2, _down2) = open_link(&h.addr, &ca, &joined.client_cert_der, &key).await;

    let err = down1.message().await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Aborted);
    assert_eq!(err.message(), server::SUPERSEDED);
    assert_eq!(h.hub.online().await, vec![joined.node_id.clone()]);

    drop(tx2);
    for _ in 0..50 {
        if h.hub.online().await.is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("node still online after its only link closed");
}
