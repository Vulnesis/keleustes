//! P4 teleport tests over real mutual TLS: a teleport records the profile
//! and crews its session needs, asks the origin to back them up, hands
//! them to the target in the download header resolved against the library
//! and installs them there (belai-counterparts #17), every step through
//! `hosts::library_dispatch` (kill switch, A2A blackout, audit). A
//! third-party harness session is refused, audited (harnesses.md C4.7).
#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kls_core::Core;
use kls_node::{Hub, hub, pki, run, token};
use kls_proto::v1::library_client::LibraryClient;
use kls_proto::v1::node_client::NodeClient;
use kls_proto::v1::teleport_client::TeleportClient;
use kls_proto::v1::{self as pb};
use kls_store::audit::LedgerQuery;
use kls_store::sessions::Session;
use rcgen::{CertificateParams, KeyPair};
use sha2::{Digest, Sha256};
use std::time::Duration;
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
                capabilities: Some(pb::Capabilities {
                    remote_control: true,
                    dispatch_kinds: vec!["library_backup".into(), "library_install".into()],
                    ..Default::default()
                }),
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

async fn session(core: &Core, id: &str, node: &str, kind: &str, agent: &str) {
    core.store()
        .upsert_session(&Session {
            id: id.into(),
            node_id: node.into(),
            kind: kind.into(),
            agent: agent.into(),
            title: "t".into(),
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn ledger_has(core: &Core, needle: &str) -> bool {
    core.store()
        .ledger(&LedgerQuery {
            limit: 500,
            ..Default::default()
        })
        .await
        .unwrap()
        .iter()
        .any(|e| e.msg.contains(needle) || e.payload.to_string().contains(needle))
}

fn header(
    session_id: &str,
    to: &str,
    body: &[u8],
    needs: Vec<pb::LibraryRef>,
) -> pb::TeleportHeader {
    pb::TeleportHeader {
        session_id: session_id.into(),
        to_node: to.into(),
        size: body.len() as i64,
        sha256: sha(body),
        needs,
        ..Default::default()
    }
}

fn need(kind: &str, name: &str) -> pb::LibraryRef {
    pb::LibraryRef {
        kind: kind.into(),
        name: name.into(),
        id: "forged".into(),
        ..Default::default()
    }
}

#[test]
fn needs_are_cleaned_and_capped() {
    let header = vec![
        need("crew", "ops"),
        need("profile", "belai:triage-vulns"),
        need("skill", "s"),
        need("profile", "a b"),
        need("profile", "log"),
    ];
    let n = kls_node::teleport::clean_needs(&header, "log");
    let got: Vec<_> = n
        .iter()
        .map(|r| (r.kind.as_str(), r.name.as_str(), r.id.as_str()))
        .collect();
    assert_eq!(got, [("profile", "log", ""), ("crew", "ops", "")]);
    let many: Vec<_> = (0..40).map(|i| need("profile", &format!("p{i}"))).collect();
    assert_eq!(
        kls_node::teleport::clean_needs(&many, "").len(),
        kls_node::teleport::MAX_NEEDS
    );
}

#[tokio::test]
async fn needs_are_recorded_backed_up_resolved_and_installed() {
    let h = start().await;
    let a = join(&h, "a").await;
    let b = join(&h, "b").await;
    let core = h.core().clone();
    session(&core, "s1", &a.id, "rc", "log").await;
    kls_core::vault::save_library(&core, "", "agent", "log", "", r#"{"name":"log"}"#, "t")
        .await
        .unwrap();

    let body = b"bundle".to_vec();
    let r = upload(
        &a,
        parts(
            "",
            header("s1", &b.id, &body, vec![need("crew", "ops")]),
            &body,
            4,
        ),
    )
    .await
    .unwrap();
    let id = r.teleport_id;
    // Recorded with the teleport.
    let stored = kls_node::teleport::stored_needs(&core, &id).await;
    assert_eq!(stored.len(), 2);
    // Backed up from the origin: a library_backup dispatch, audited with the
    // teleport as its origin (rules 1 and 2).
    let ds = core.store().dispatches(Some(&a.id), 20).await.unwrap();
    assert!(ds.iter().any(|d| d.kind == "library_backup"), "{ds:?}");
    assert!(ledger_has(&core, &format!("teleport:{id}")).await);

    // The download header names the needs as the library holds them.
    let (hd, got) = download(&b, &id).await.unwrap();
    assert_eq!(got, body);
    let log = hd.needs.iter().find(|n| n.name == "log").expect("log need");
    assert_eq!(log.kind, "profile");
    assert!(!log.id.is_empty() && log.version >= 1 && log.sha256.len() == 64);
    let ops = hd.needs.iter().find(|n| n.name == "ops").expect("ops need");
    assert!(ops.id.is_empty(), "the library has no crew ops");
    // Installed on the target, once.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ds = core.store().dispatches(Some(&b.id), 20).await.unwrap();
        if ds.iter().any(|d| d.kind == "library_install") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no install dispatch"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn needs_dispatches_obey_the_kill_switch_and_the_a2a_blackout() {
    let h = start().await;
    let a = join(&h, "a").await;
    let b = join(&h, "b").await;
    let core = h.core().clone();
    session(&core, "s1", &a.id, "rc", "log").await;
    kls_core::vault::save_library(&core, "", "agent", "log", "", r#"{"name":"log"}"#, "t")
        .await
        .unwrap();

    // Under stop the backup is refused (and audited); the upload still lands.
    kls_core::control::stop(&core, "drill").await.unwrap();
    let body = b"bundle".to_vec();
    let id = upload(&a, parts("", header("s1", &b.id, &body, vec![]), &body, 4))
        .await
        .unwrap()
        .teleport_id;
    let ds = core.store().dispatches(Some(&a.id), 20).await.unwrap();
    assert!(!ds.iter().any(|d| d.kind == "library_backup"));
    assert!(ledger_has(&core, "library_backup").await);
    kls_core::control::resume(&core).await.unwrap();

    // Under the A2A blackout the profile install on the target is refused,
    // audited.
    kls_core::control::set_a2a_blackout(&core, true, "drill", "owner")
        .await
        .unwrap();
    download(&b, &id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ds = core.store().dispatches(Some(&b.id), 20).await.unwrap();
    assert!(!ds.iter().any(|d| d.kind == "library_install"), "{ds:?}");
    assert!(ledger_has(&core, "library_install").await);
}

fn library(n: &TestNode) -> LibraryClient<Channel> {
    LibraryClient::new(n.chan.clone())
}

async fn fetch_for_teleport(
    n: &TestNode,
    teleport_id: &str,
    kind: &str,
    name: &str,
) -> Result<pb::LibraryDoc, tonic::Status> {
    library(n)
        .fetch(pb::LibraryFetch {
            teleport_id: teleport_id.into(),
            r#ref: Some(pb::LibraryRef {
                kind: kind.into(),
                name: name.into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .map(|r| r.into_inner())
}

async fn download_fetching(n: &TestNode, id: &str) {
    let mut s = n
        .teleport()
        .download(pb::TeleportRequest {
            teleport_id: id.into(),
            fetch_needs: true,
        })
        .await
        .unwrap()
        .into_inner();
    while let Some(p) = s.message().await.unwrap() {
        if p.last {
            break;
        }
    }
}

#[tokio::test]
async fn the_target_fetches_its_needs_by_teleport_id_only() {
    let h = start().await;
    let a = join(&h, "a").await;
    let b = join(&h, "b").await;
    let c = join(&h, "c").await;
    let core = h.core().clone();
    session(&core, "s1", &a.id, "rc", "log").await;
    kls_core::vault::save_library(&core, "", "agent", "log", "", r#"{"name":"log"}"#, "t")
        .await
        .unwrap();
    kls_core::vault::save_library(&core, "", "agent", "other", "", r#"{"name":"other"}"#, "t")
        .await
        .unwrap();
    let body = b"bundle".to_vec();
    let id = upload(&a, parts("", header("s1", &b.id, &body, vec![]), &body, 4))
        .await
        .unwrap()
        .teleport_id;

    // Not yet: the teleport must be ready (it is) and addressed to the caller.
    let e = fetch_for_teleport(&c, &id, "profile", "log")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied, "{e:?}");

    // The target downloads, saying it fetches the needs itself: no dispatch.
    download_fetching(&b, &id).await;
    wait_status(&core, &id, "delivered").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ds = core.store().dispatches(Some(&b.id), 20).await.unwrap();
    assert!(!ds.iter().any(|d| d.kind == "library_install"), "{ds:?}");

    // The need is answered, audited with the teleport.
    let doc = fetch_for_teleport(&b, &id, "profile", "log").await.unwrap();
    assert!(String::from_utf8_lossy(&doc.doc).contains("log"));
    let r = doc.r#ref.unwrap();
    assert_eq!(r.name, "log");
    assert_eq!(r.sha256, sha(&doc.doc));
    assert!(ledger_has(&core, &id).await);
    // An item the teleport does not need, or of another kind, is refused.
    let e = fetch_for_teleport(&b, &id, "profile", "other")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied, "{e:?}");
    let e = fetch_for_teleport(&b, &id, "hook", "log")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied, "{e:?}");
    // A fetch naming neither a dispatch nor a teleport is refused.
    let e = fetch_for_teleport(&b, "", "profile", "log")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied, "{e:?}");

    // The kill switch and the A2A blackout refuse it.
    kls_core::control::stop(&core, "drill").await.unwrap();
    let e = fetch_for_teleport(&b, &id, "profile", "log")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition, "{e:?}");
    kls_core::control::resume(&core).await.unwrap();
    kls_core::control::set_a2a_blackout(&core, true, "drill", "owner")
        .await
        .unwrap();
    let e = fetch_for_teleport(&b, &id, "profile", "log")
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied, "{e:?}");
}

async fn reconcile(n: &TestNode, items: Vec<pb::LibraryRef>) -> Vec<pb::LibraryVerdict> {
    library(n)
        .reconcile(pb::LibraryInventory { items })
        .await
        .unwrap()
        .into_inner()
        .verdicts
}

fn held(kind: &str, name: &str, sha256: &str) -> pb::LibraryRef {
    pb::LibraryRef {
        kind: kind.into(),
        name: name.into(),
        sha256: sha256.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn reconcile_queues_the_automatic_sync_but_never_for_hooks() {
    let h = start().await;
    let a = join(&h, "a").await;
    let core = h.core().clone();
    let old = r#"{"name":"log"}"#;
    kls_core::vault::save_library(&core, "", "agent", "log", "", old, "t")
        .await
        .unwrap();
    kls_core::vault::save_library(
        &core,
        "",
        "agent",
        "log",
        "",
        r#"{"name":"log","prompt":"v2"}"#,
        "t",
    )
    .await
    .unwrap();
    let items = vec![
        held("profile", "fresh", &"a".repeat(64)),
        held("profile", "log", &sha(old.as_bytes())),
        held("hook", "pre", &"b".repeat(64)),
    ];
    let v = reconcile(&a, items.clone()).await;
    let by = |n: &str| {
        v.iter()
            .find(|x| x.r#ref.as_ref().unwrap().name == n)
            .unwrap()
    };
    assert_eq!(by("fresh").verdict, "push");
    assert_eq!(by("log").verdict, "install");
    assert_eq!(by("pre").verdict, "push");
    assert!(
        by("pre").dispatch_id.is_empty(),
        "a hook never moves by sync"
    );
    let ds = core.store().dispatches(Some(&a.id), 20).await.unwrap();
    let backup = ds
        .iter()
        .find(|d| d.kind == "library_backup")
        .expect("backup");
    let install = ds
        .iter()
        .find(|d| d.kind == "library_install")
        .expect("install");
    assert_eq!(by("fresh").dispatch_id, backup.id);
    assert_eq!(by("log").dispatch_id, install.id);
    let spec: serde_json::Value = serde_json::from_str(&install.spec_json).unwrap();
    assert_eq!(spec["overwrite"], true, "{spec}");
    assert!(!backup.spec_json.contains("\"pre\""));
    assert!(ledger_has(&core, "library.sync").await);

    // The same set again: the same dispatches, nothing new queued.
    let again = reconcile(&a, items.clone()).await;
    assert_eq!(again.len(), 3);
    let n = core
        .store()
        .dispatches(Some(&a.id), 20)
        .await
        .unwrap()
        .len();
    assert_eq!(n, ds.len());

    // Off: verdicts only.
    core.store()
        .set_setting(kls_core::belai::library::AUTO_SYNC, "off")
        .await
        .unwrap();
    let off = reconcile(&a, vec![held("profile", "other", &"c".repeat(64))]).await;
    assert_eq!(off[0].verdict, "push");
    assert!(off[0].dispatch_id.is_empty());
    core.store()
        .set_setting(kls_core::belai::library::AUTO_SYNC, "on")
        .await
        .unwrap();

    // Under the kill switch nothing is queued (refused, audited).
    kls_core::control::stop(&core, "drill").await.unwrap();
    let stopped = reconcile(&a, vec![held("profile", "third", &"d".repeat(64))]).await;
    assert!(stopped[0].dispatch_id.is_empty());
}

#[tokio::test]
async fn a_harness_session_is_refused_and_audited() {
    let h = start().await;
    let a = join(&h, "a").await;
    let b = join(&h, "b").await;
    let core = h.core().clone();
    session(&core, "hs", &a.id, "harness", "").await;
    let body = b"bundle".to_vec();
    let e = upload(&a, parts("", header("hs", &b.id, &body, vec![]), &body, 4))
        .await
        .unwrap_err();
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    assert!(e.message().contains("harness session cannot be teleported"));
    assert!(core.store().teleports(10).await.unwrap().is_empty());
    assert!(ledger_has(&core, "teleport of harness session hs refused").await);
    assert!(ledger_has(&core, &format!("node:{}", a.id)).await);
    // And from the console: refused before any job leaves.
    let e = core.teleport_request("hs", &a.id, &b.id).await.unwrap_err();
    assert!(
        e.to_string()
            .contains("harness session cannot be teleported")
    );
}
