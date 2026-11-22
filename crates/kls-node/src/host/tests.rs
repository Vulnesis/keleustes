//! A fake belai host over real mTLS on loopback: it reports an inventory,
//! takes actions from `Host.Actions` and answers with `Host.Progress`.

use super::*;
use crate::pki::{self, Pki};
use kls_core::Core;
use kls_core::gateway::{DispatchSpec, JobNeeds, JobOutcome, NodeGateway};
use kls_core::hosts::{ActionRequest, Origin};
use kls_proto::v1::host_client::HostClient;
use rcgen::{CertificateParams, KeyPair};
use std::sync::Arc;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity, Server, ServerTlsConfig};

struct Online(Vec<String>);

#[tonic::async_trait]
impl NodeGateway for Online {
    async fn online(&self) -> Vec<String> {
        self.0.clone()
    }
    async fn dispatch(&self, _n: &str, _s: DispatchSpec) -> anyhow::Result<String> {
        Ok("d".into())
    }
    async fn job(
        &self,
        _n: Option<&str>,
        _k: &str,
        _p: &str,
        _needs: JobNeeds,
        _t: i64,
    ) -> anyhow::Result<JobOutcome> {
        anyhow::bail!("none")
    }
}

struct H {
    core: Core,
    addr: String,
    ca: String,
    pki: Arc<Pki>,
    _dir: tempfile::TempDir,
    _stop: tokio::sync::oneshot::Sender<()>,
}

async fn start() -> H {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let pki = Arc::new(
        Pki::load_or_create(&dir.path().join("ca"), &core.server_id().await.unwrap()).unwrap(),
    );
    let hub = Hub::new(core.clone(), pki.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            pki.server_cert_pem(),
            pki.server_key_pem(),
        ))
        .client_ca_root(Certificate::from_pem(pki.ca_cert_pem()))
        .client_auth_optional(true);
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .unwrap()
            .add_service(service(hub))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                async {
                    let _ = rx.await;
                },
            )
            .await
            .unwrap();
    });
    H {
        ca: pki.ca_cert_pem().to_string(),
        core,
        addr,
        pki,
        _dir: dir,
        _stop: stop,
    }
}

/// Registers a node and returns a client holding its certificate.
async fn node(h: &H, id: &str, name: &str) -> HostClient<Channel> {
    let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let csr = CertificateParams::default()
        .serialize_request(&key)
        .unwrap();
    let (der, serial) = h.pki.sign_node(csr.der(), id).unwrap();
    h.core
        .store()
        .insert_node(id, name, &Default::default(), &serial, "tok")
        .await
        .unwrap();
    let cert = ::pem::encode(&::pem::Pem::new("CERTIFICATE", der));
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&h.ca))
        .domain_name(pki::TLS_SERVER_NAME)
        .identity(Identity::from_pem(cert, key.serialize_pem()));
    let ch = Channel::from_shared(format!("https://{}", h.addr))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    HostClient::new(ch)
}

fn inventory() -> pb::HostInventory {
    pb::HostInventory {
        at_ms: 1,
        belai: Some(pb::BelaiFacts {
            version: "0.42.1".into(),
            ..Default::default()
        }),
        manage: Some(pb::HostManage {
            host_actions: vec![
                "preflight".into(),
                "repo_trust".into(),
                "product_probe".into(),
                "no_such_kind".into(),
            ],
            manage_level: "provision".into(),
            manage_shell: false,
        }),
        repos: vec![pb::HostRepo {
            path: "~/src/api".into(),
            owner: "acme".into(),
            name: "api".into(),
            ..Default::default()
        }],
        settings_sha256: "s1".into(),
        ..Default::default()
    }
}

async fn next_action(s: &mut tonic::Streaming<pb::HostAction>) -> Option<pb::HostAction> {
    tokio::time::timeout(Duration::from_secs(3), s.message())
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten()
}

#[tokio::test]
async fn report_stores_inventory_and_drops_unknown_kinds() {
    let h = start().await;
    let mut c = node(&h, "n1", "build-01").await;
    c.report(inventory()).await.unwrap();
    c.report(inventory()).await.unwrap();
    let (_, inv) = hosts::inventory(&h.core, "n1").await.unwrap().unwrap();
    assert_eq!(inv.belai.version, "0.42.1");
    assert_eq!(
        inv.repos[0].link().as_deref(),
        Some("keleustes://repos/acme/api")
    );
    assert!(!inv.manage.host_actions.iter().any(|k| k == "no_such_kind"));
    let evs = h.core.store().recent_events(100).await.unwrap();
    assert_eq!(evs.iter().filter(|e| e.kind == kinds::INVENTORY).count(), 1);
}

#[tokio::test]
async fn report_records_the_handshake_cert_expiry() {
    let h = start().await;
    let mut c = node(&h, "n1", "build-01").await;
    c.report(inventory()).await.unwrap();
    let (_, inv) = hosts::inventory(&h.core, "n1").await.unwrap().unwrap();
    let now = kls_store::now_ms();
    assert!(inv.link.received_at_ms > 0 && inv.link.received_at_ms <= now);
    // pki signs node certificates for five years.
    let left = inv
        .cert_left_ms(now)
        .expect("cert expiry from the handshake");
    let year = 365 * 86_400_000_i64;
    assert!(left > 4 * year && left <= 5 * year + 86_400_000, "{left}");
    assert_eq!(cert_not_after_ms(b"not a certificate"), 0);
}

#[tokio::test]
async fn actions_replay_until_answered_and_are_addressed() {
    let h = start().await;
    h.core.set_gateway(Arc::new(Online(vec!["n1".into()])));
    let mut c = node(&h, "n1", "build-01").await;
    let mut other = node(&h, "n2", "other").await;
    c.report(inventory()).await.unwrap();
    let row = hosts::request_action(
        &h.core,
        ActionRequest::new("n1", "preflight", Origin::owner("diagnostics")),
    )
    .await
    .unwrap();

    // Delivered, dropped unanswered, delivered again on reconnect.
    let mut s = c
        .actions(pb::HostActionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let a = next_action(&mut s).await.unwrap();
    assert_eq!(
        (a.id.as_str(), a.kind.as_str(), a.class.as_str()),
        (row.id.as_str(), "preflight", "read")
    );
    assert_eq!(a.origin.unwrap().surface, "fleet.host/diagnostics");
    drop(s);
    let mut s = c
        .actions(pb::HostActionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(next_action(&mut s).await.unwrap().id, row.id);

    // Another node cannot answer it.
    let upd = pb::HostActionUpdate {
        action_id: row.id.clone(),
        seq: 1,
        state: "accepted".into(),
        ..Default::default()
    };
    let e = other.progress(upd.clone()).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::PermissionDenied);
    c.progress(upd.clone()).await.unwrap();
    let e = c.progress(upd).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::FailedPrecondition, "seq must grow");
    drop(s);

    // Answered: not sent again.
    let mut s = c
        .actions(pb::HostActionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(next_action(&mut s).await.is_none());

    // A live action arrives on the open stream.
    let live = hosts::request_action(
        &h.core,
        ActionRequest::new("n1", "product_probe", Origin::owner("products"))
            .param("product", "cli"),
    )
    .await
    .unwrap();
    assert_eq!(next_action(&mut s).await.unwrap().id, live.id);
    c.progress(pb::HostActionUpdate {
        action_id: live.id.clone(),
        seq: 1,
        state: "done".into(),
        result_json: "{}".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let r = h.core.store().host_action(&live.id).await.unwrap().unwrap();
    assert_eq!(r.state, "done");
}

#[tokio::test]
async fn stopped_factory_holds_changes_back_and_audits() {
    let h = start().await;
    h.core.set_gateway(Arc::new(Online(vec!["n1".into()])));
    let mut c = node(&h, "n1", "build-01").await;
    c.report(inventory()).await.unwrap();
    let change = hosts::request_action(
        &h.core,
        ActionRequest::new("n1", "repo_trust", Origin::owner("repos")).param("path", "~/src/api"),
    )
    .await
    .unwrap();
    kls_core::control::stop(&h.core, "incident").await.unwrap();
    let mut s = c
        .actions(pb::HostActionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(
        next_action(&mut s).await.is_none(),
        "no change leaves while stopped"
    );
    let r = h
        .core
        .store()
        .host_action(&change.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.state, "refused");
    let ledger = h
        .core
        .audit_entries(&kls_store::audit::LedgerQuery::default())
        .await
        .unwrap();
    assert!(
        ledger
            .iter()
            .any(|e| e.payload["actionId"] == change.id && e.payload["outcome"] == "refused")
    );
}

#[tokio::test]
async fn unauthenticated_calls_are_refused() {
    let h = start().await;
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(&h.ca))
        .domain_name(pki::TLS_SERVER_NAME);
    let ch = Channel::from_shared(format!("https://{}", h.addr))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut anon = HostClient::new(ch);
    let e = anon.report(inventory()).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);
}
