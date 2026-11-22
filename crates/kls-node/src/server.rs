//! gRPC node server: Join and the Link control stream.

use crate::pki::{self, Pki, TLS_SERVER_NAME};
use crate::token;
use anyhow::{Context, Result};
use kls_core::{Core, kinds, settings};
use kls_proto::v1::board_server::BoardServer;
use kls_proto::v1::jobs_server::JobsServer;
use kls_proto::v1::node_server::{Node as NodeRpc, NodeServer};
use kls_proto::v1::sessions_server::SessionsServer;
use kls_proto::v1::teleport_server::TeleportServer;
use kls_proto::v1::{self as pb, node_frame, server_frame};
use kls_store::{Capabilities, HostInfo, now_ms};
use prost::Message;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

pub const HEARTBEAT_SECONDS: u32 = 15;
/// A link with no inbound frame for this long is considered dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(HEARTBEAT_SECONDS as u64 * 3);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a link checks the outbox and its revocation for changes made
/// by another process.
const OUTBOX_SWEEP: Duration = Duration::from_secs(2);

/// Status message a superseded link is closed with; belai backs off on it.
pub const SUPERSEDED: &str = "superseded by a newer link for this node";

struct Conn {
    conn_id: u64,
    tx: mpsc::Sender<Result<pb::ServerFrame, Status>>,
    /// Closes this link's driver (supersede, revoke).
    kick: Option<tokio::sync::oneshot::Sender<Status>>,
    /// Highest outbox seq sent on this link (live or replayed).
    sent: Arc<std::sync::atomic::AtomicU64>,
}

/// Live node connections plus the durable outbox. Shared by the gRPC
/// service and the UI.
#[derive(Clone)]
pub struct Hub {
    core: Core,
    pki: Arc<Pki>,
    conns: Arc<Mutex<HashMap<String, Conn>>>,
    next_conn: Arc<std::sync::atomic::AtomicU64>,
    /// Live vault grants by id; their values are never persisted.
    grants: Arc<Mutex<HashMap<String, pb::VaultGrant>>>,
}

impl Hub {
    pub fn new(core: Core, pki: Arc<Pki>) -> Self {
        Self {
            core,
            pki,
            conns: Arc::default(),
            next_conn: Arc::default(),
            grants: Arc::default(),
        }
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    /// Ids of nodes with an open Link stream.
    pub async fn online(&self) -> Vec<String> {
        self.conns.lock().await.keys().cloned().collect()
    }

    /// Mints a join token for a new node.
    pub async fn mint_token(&self, label: &str, ttl_ms: i64) -> Result<token::Minted> {
        self.mint_token_by(label, ttl_ms, "owner").await
    }

    /// [`Self::mint_token`] recording the actor `by` (`owner:tray`).
    pub async fn mint_token_by(&self, label: &str, ttl_ms: i64, by: &str) -> Result<token::Minted> {
        let s = self.core.store();
        let listen = self.core.listen_addr().await?;
        let configured = s.setting(settings::ADVERTISE_ADDRS).await?;
        let addrs = token::advertise_addrs(configured.as_deref(), &listen);
        let minted = token::mint(
            &self.core.server_id().await?,
            &self.core.server_name().await?,
            addrs,
            self.pki.server_spki_sha256(),
            now_ms() + ttl_ms,
        );
        s.insert_join_token(&minted.id, &minted.secret_sha256, label, minted.expires_at)
            .await?;
        self.core
            .emit(
                kinds::TOKEN_MINTED,
                &minted.id,
                &serde_json::json!({ "label": label, "expiresAt": minted.expires_at, "by": by }),
            )
            .await?;
        Ok(minted)
    }

    /// Queues a replayable frame for a node and pushes it if the node is online.
    pub async fn send(&self, node_id: &str, body: server_frame::Body) -> Result<i64> {
        let mut frame = pb::ServerFrame {
            seq: 0,
            ack: 0,
            body: Some(body),
        };
        // Provider keys never reach the database: the outbox keeps the
        // grant's id, providers and env names only, and the values live in
        // memory until the grant expires (see `hydrate`).
        let mut stored = frame.clone();
        if let Some(g) = stored.body.as_mut().and_then(vault_of) {
            if !g.grant_id.is_empty() {
                self.grants
                    .lock()
                    .await
                    .insert(g.grant_id.clone(), g.clone());
            }
            for s in &mut g.secrets {
                s.value.clear();
            }
            // MCP credentials likewise (WP-F).
            for m in &mut g.mcp {
                m.value.clear();
            }
        }
        let seq = self
            .core
            .store()
            .outbox_push(node_id, &stored.encode_to_vec())
            .await?;
        // Live only when every earlier frame is already on the stream: a
        // lower seq another process queued (a `keleustes` CLI command) waits
        // for the outbox sweep, and moving `sent` past it would skip it
        // there while the node's ack of this one marked it delivered. Out of
        // turn, this frame goes with the sweep too, in order.
        if let Some(c) = self.conns.lock().await.get(node_id)
            && c.sent.load(std::sync::atomic::Ordering::Relaxed) + 1 == seq as u64
        {
            let node_seq = self.node_seq(node_id).await;
            frame.seq = seq as u64;
            frame.ack = node_seq;
            if c.tx.try_send(Ok(frame)).is_ok() {
                c.sent
                    .fetch_max(seq as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(seq)
    }

    /// Restores the secret values of a frame read back from the outbox:
    /// from the in-memory grant while it lives, else (after a restart) by
    /// re-granting the same providers from the vault; an expired grant is
    /// dropped so the node falls back to its own configuration.
    pub(crate) async fn hydrate(&self, frame: &mut pb::ServerFrame) {
        let Some(slot) = frame.body.as_mut().and_then(vault_slot) else {
            return;
        };
        let Some(stored) = slot.take() else { return };
        let now = now_ms();
        let mut grants = self.grants.lock().await;
        grants.retain(|_, g| g.expires_at_ms == 0 || g.expires_at_ms > now);
        if let Some(g) = grants.get(&stored.grant_id) {
            *slot = Some(g.clone());
            return;
        }
        drop(grants);
        if stored.expires_at_ms != 0 && stored.expires_at_ms <= now {
            return;
        }
        let mut providers: Vec<String> =
            stored.secrets.iter().map(|s| s.provider.clone()).collect();
        providers.dedup();
        let ttl = if stored.expires_at_ms == 0 {
            2 * 60 * 60 * 1000
        } else {
            stored.expires_at_ms - now
        };
        match kls_core::vault::grant_for_purpose(&self.core, &providers, ttl, "replay").await {
            Ok(Some(g)) => {
                let pbg = pb::VaultGrant {
                    grant_id: g.grant_id.clone(),
                    secrets: g
                        .secrets
                        .iter()
                        .map(|s| pb::VaultSecret {
                            provider: s.provider.clone(),
                            env: s.env.clone(),
                            value: s.value.expose().into(),
                        })
                        .collect(),
                    expires_at_ms: g.expires_at_ms,
                    // MCP credentials are not re-granted after a restart: the
                    // worker runs on the host's own MCP keys.
                    mcp: Vec::new(),
                };
                self.grants
                    .lock()
                    .await
                    .insert(pbg.grant_id.clone(), pbg.clone());
                *slot = Some(pbg);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("re-granting vault keys for a replayed frame: {e:#}"),
        }
    }

    async fn node_seq(&self, node_id: &str) -> u64 {
        self.core
            .store()
            .node(node_id)
            .await
            .ok()
            .flatten()
            .map(|n| n.node_seq as u64)
            .unwrap_or(0)
    }

    /// Revokes a node: its certificate is refused from now on and an open
    /// link is told and closed.
    pub async fn revoke(&self, node_id: &str, reason: &str) -> Result<bool> {
        let changed = self.core.store().revoke_node(node_id, reason).await?;
        if changed {
            self.core
                .emit(
                    kinds::NODE_REVOKED,
                    node_id,
                    &serde_json::json!({ "reason": reason }),
                )
                .await?;
        }
        if let Some(c) = self.conns.lock().await.remove(node_id) {
            let _ =
                c.tx.send(Ok(pb::ServerFrame {
                    seq: 0,
                    ack: 0,
                    body: Some(server_frame::Body::Revoked(pb::Revoked {
                        reason: reason.into(),
                    })),
                }))
                .await;
            if let Some(k) = c.kick {
                let _ = k.send(Status::permission_denied("node revoked"));
            }
        }
        Ok(changed)
    }

    /// Serves the node port until `shutdown` resolves.
    pub async fn serve(
        self,
        addr: std::net::SocketAddr,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                self.pki.server_cert_pem(),
                self.pki.server_key_pem(),
            ))
            .client_ca_root(Certificate::from_pem(self.pki.ca_cert_pem()))
            // Join runs before the node has a certificate; Link checks it.
            .client_auth_optional(true);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("listen on {addr} (another Keleustes running?)"))?;
        tracing::info!(%addr, "node server listening");
        crate::lost::start(&self);
        crate::attention::start(&self);
        Server::builder()
            .tls_config(tls)
            .context("tls config")?
            .http2_keepalive_interval(Some(Duration::from_secs(20)))
            .http2_keepalive_timeout(Some(Duration::from_secs(20)))
            .add_service(NodeServer::new(Service { hub: self.clone() }))
            .add_service(SessionsServer::new(crate::sessions::Svc {
                hub: self.clone(),
            }))
            .add_service(BoardServer::new(crate::board::Svc { hub: self.clone() }))
            .add_service(JobsServer::new(crate::jobs::Svc { hub: self.clone() }))
            .add_service(kls_proto::v1::fleet_server::FleetServer::new(
                crate::fleet::Svc { hub: self.clone() },
            ))
            .add_service(crate::host::service(self.clone()))
            .add_service(kls_proto::v1::forge_server::ForgeServer::new(
                crate::forge::Svc { hub: self.clone() },
            ))
            .add_service(kls_proto::v1::library_server::LibraryServer::new(
                crate::library::Svc { hub: self.clone() },
            ))
            .add_service(kls_proto::v1::screen_server::ScreenServer::new(
                crate::screen::Svc { hub: self.clone() },
            ))
            .add_service(TeleportServer::new(crate::teleport::Svc { hub: self }))
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(listener),
                shutdown,
            )
            .await
            .context("node server")
    }
}

/// Binds the configured listen address and serves.
pub async fn run(
    hub: Hub,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listen = hub.core().listen_addr().await?;
    let addr: std::net::SocketAddr = listen
        .parse()
        .with_context(|| format!("listen address {listen:?}"))?;
    // Only the serving process pushes FactoryControl. It sees every change
    // (its own and, through the event tail, a CLI's) and sends the state as
    // it is then; a short-lived CLI process broadcasting too could append a
    // state read before a later change, leaving the node on a stale one.
    crate::board::start_control_broadcast(hub.clone());
    hub.serve(addr, shutdown).await
}

/// Authenticates a node RPC by its client certificate: known node, not
/// revoked, current certificate. Returns the node id.
pub async fn authenticate<T>(hub: &Hub, req: &Request<T>) -> Result<String, Status> {
    let certs = req
        .peer_certs()
        .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
    let (node_id, serial) = pki::client_cert_identity(leaf)
        .map_err(|_| Status::unauthenticated("bad client certificate"))?;
    let node = hub
        .core
        .store()
        .node(&node_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| Status::unauthenticated("unknown node"))?;
    if node.revoked_at.is_some() {
        return Err(Status::permission_denied("node revoked"));
    }
    if node.cert_serial != serial {
        return Err(Status::unauthenticated("certificate superseded"));
    }
    Ok(node_id)
}

struct Service {
    hub: Hub,
}

fn host_info(h: &pb::HostAdvert) -> HostInfo {
    let caps = h.capabilities.clone().unwrap_or_default();
    HostInfo {
        hostname: h.hostname.clone(),
        os: h.os.clone(),
        arch: h.arch.clone(),
        belai_version: h.belai_version.clone(),
        host_id: h.host_id.clone(),
        capabilities: Capabilities {
            gh_authenticated: caps.gh_authenticated,
            terminal: caps.terminal,
            remote_control: caps.remote_control,
            providers: caps.providers,
            max_sessions: caps.max_sessions,
            dispatch_kinds: caps.dispatch_kinds,
            job_kinds: caps.job_kinds,
            can_clone: caps.can_clone,
            models: caps
                .models
                .into_iter()
                .map(|m| kls_store::ProviderModels {
                    provider: m.provider,
                    models: m.models,
                })
                .collect(),
            forges: caps
                .forges
                .iter()
                .map(|f| kls_store::ForgeLogin {
                    cli: f.cli.clone(),
                    host: f.host.to_ascii_lowercase(),
                    kind: f.kind.clone(),
                    login: f.login.clone(),
                    signed_in: f.signed_in,
                })
                .collect(),
            harness_sessions: caps.harness_sessions,
            dispatch_features: caps.dispatch_features.clone(),
            inventory: h
                .inventory
                .iter()
                .take(2000)
                .map(|r| kls_store::InventoryRef {
                    kind: r.kind.clone(),
                    id: r.id.clone(),
                    name: r.name.clone(),
                    version: r.version,
                    sha256: r.sha256.clone(),
                })
                .collect(),
            agent_routes: h
                .agent_routes
                .iter()
                .map(|r| kls_store::AgentRoute {
                    name: r.name.clone(),
                    mode: r.mode.clone(),
                    lists: r.lists.clone(),
                    labels: r.labels.clone(),
                    read_only: r.read_only,
                })
                .collect(),
        },
        trusted_dirs: h.trusted_dirs.clone(),
        agents: h.agents.clone(),
        crews: h.crews.clone(),
        harnesses: h
            .harnesses
            .iter()
            .take(64)
            .map(|x| kls_store::HarnessInfo {
                id: x.id.clone(),
                name: x.name.clone(),
                version: x.version.clone(),
                state: x.state.clone(),
                detail: x.detail.clone(),
                resume: x.resume,
                a2a: x.a2a,
                logged_in: x.logged_in,
                grant_env: x.grant_env.clone(),
                modes: x.modes.clone(),
                transcript: x.transcript.clone(),
                allow_keys: x.allow_keys.clone(),
                path: x.path.clone(),
                version_hash: x.version_hash.clone(),
                probed_at_ms: x.probed_at_ms,
                conf: x.conf.clone(),
                kill: x.kill,
                asks: x.asks,
                named: x.named,
                continuity: x.continuity.iter().take(8).cloned().collect(),
                caveats: x
                    .caveats
                    .iter()
                    .take(16)
                    .map(|c| kls_store::HarnessCaveat {
                        code: c.code.chars().take(40).collect(),
                        note: c
                            .note
                            .chars()
                            .filter(|ch| !ch.is_control())
                            .take(200)
                            .collect(),
                    })
                    .collect(),
                startup_p50_ms: x.startup_p50_ms.max(0),
                startup_p95_ms: x.startup_p95_ms.max(0),
                startup_samples: i64::from(x.startup_samples),
                startup_measured_at_ms: x.startup_measured_at_ms.max(0),
            })
            .collect(),
    }
}

pub(crate) fn internal(e: anyhow::Error) -> Status {
    tracing::error!("node server: {e:#}");
    Status::internal("internal error")
}

type LinkStream = Pin<Box<dyn Stream<Item = Result<pb::ServerFrame, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl NodeRpc for Service {
    async fn join(
        &self,
        req: Request<pb::JoinRequest>,
    ) -> Result<Response<pb::JoinResponse>, Status> {
        let req = req.into_inner();
        let refused = || Status::permission_denied("join token is unknown, used or expired");
        // The raw 32-byte secret (the token carries it base64url-encoded).
        if req.secret.len() != 32 {
            return Err(refused());
        }
        let secret = req.secret;
        let node_id = uuid::Uuid::now_v7().to_string();
        // Check the CSR before spending the token.
        let (cert_der, serial) = self
            .hub
            .pki
            .sign_node(&req.csr_der, &node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let store = self.hub.core.store();
        let tok = store
            .redeem_join_token(&req.token_id, &token::secret_hash(&secret), &node_id)
            .await
            .map_err(|_| refused())?;
        let host = req.host.as_ref().map(host_info).unwrap_or_default();
        let name = if !tok.label.is_empty() {
            tok.label.clone()
        } else if !host.hostname.is_empty() {
            host.hostname.clone()
        } else {
            format!("node {}", &node_id[..8])
        };
        store
            .insert_node(&node_id, &name, &host, &serial, &tok.id)
            .await
            .map_err(internal)?;
        self.hub
            .core
            .emit(
                kinds::NODE_JOINED,
                &node_id,
                &serde_json::json!({ "name": name, "hostname": host.hostname }),
            )
            .await
            .map_err(internal)?;
        tracing::info!(%node_id, %name, "node joined");
        Ok(Response::new(pb::JoinResponse {
            node_id,
            server_id: self.hub.core.server_id().await.map_err(internal)?,
            server_name: self.hub.core.server_name().await.map_err(internal)?,
            client_cert_der: cert_der,
            ca_cert_der: self.hub.pki.ca_cert_der().to_vec(),
            tls_server_name: TLS_SERVER_NAME.into(),
        }))
    }

    type LinkStream = LinkStream;

    async fn link(
        &self,
        req: Request<Streaming<pb::NodeFrame>>,
    ) -> Result<Response<Self::LinkStream>, Status> {
        let node_id = authenticate(&self.hub, &req).await?;
        let (tx, rx) = mpsc::channel(256);
        let hub = self.hub.clone();
        let inbound = req.into_inner();
        tokio::spawn(async move {
            if let Err(e) = drive_link(hub, node_id.clone(), inbound, tx).await {
                tracing::warn!(%node_id, "link ended: {e:#}");
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn drive_link(
    hub: Hub,
    node_id: String,
    mut inbound: Streaming<pb::NodeFrame>,
    tx: mpsc::Sender<Result<pb::ServerFrame, Status>>,
) -> Result<()> {
    let store = hub.core.store().clone();

    // First frame must be Hello.
    let first = tokio::time::timeout(HELLO_TIMEOUT, inbound.message())
        .await
        .context("hello timeout")??
        .context("stream closed before hello")?;
    let Some(node_frame::Body::Hello(hello)) = first.body else {
        anyhow::bail!("first frame was not hello");
    };

    let node = store.node(&node_id).await?.context("node vanished")?;
    let mut node_seq = node.node_seq as u64;

    // The node has applied everything up to resume_after.
    if hello.resume_after > 0 {
        store
            .outbox_ack(&node_id, hello.resume_after as i64)
            .await?;
    }
    tx.send(Ok(pb::ServerFrame {
        seq: 0,
        ack: node_seq,
        body: Some(server_frame::Body::Welcome(pb::Welcome {
            node_id: node_id.clone(),
            server_name: hub.core.server_name().await?,
            resume_after: node_seq,
            heartbeat_seconds: HEARTBEAT_SECONDS,
            control: Some(crate::board::control_to_pb(
                &hub.core.factory_control().await?,
            )),
        })),
    }))
    .await?;

    let conn_id = hub
        .next_conn
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let sent = Arc::new(std::sync::atomic::AtomicU64::new(hello.resume_after));
    let (kick_tx, mut kick_rx) = tokio::sync::oneshot::channel::<Status>();
    {
        // Register before replay so frames queued meanwhile are pushed live;
        // the node drops any duplicate by seq. One live link per node: a
        // newer link supersedes an older one (a half-open connection, or a
        // second process holding the same node certificate), which is told
        // and closed so it can never linger unregistered.
        let mut conns = hub.conns.lock().await;
        if let Some(old) = conns.insert(
            node_id.clone(),
            Conn {
                conn_id,
                tx: tx.clone(),
                kick: Some(kick_tx),
                sent: sent.clone(),
            },
        ) && let Some(k) = old.kick
        {
            let _ = k.send(Status::aborted(SUPERSEDED));
        }
    }
    for (seq, bytes) in store
        .outbox_after(&node_id, hello.resume_after as i64)
        .await?
    {
        let mut f = pb::ServerFrame::decode(bytes.as_slice())?;
        hub.hydrate(&mut f).await;
        f.seq = seq as u64;
        f.ack = node_seq;
        tx.send(Ok(f)).await?;
        sent.fetch_max(seq as u64, std::sync::atomic::Ordering::Relaxed);
    }
    store.touch_node(&node_id).await?;
    hub.core
        .emit(
            kinds::NODE_ONLINE,
            &node_id,
            &serde_json::json!({ "belaiVersion": hello.belai_version }),
        )
        .await?;

    // Frames another process queued (a `keleustes` CLI command, a second
    // app on the same database) and revocations it recorded reach the link
    // through this sweep; frames this process queues are pushed at once.
    let mut sweep = tokio::time::interval(OUTBOX_SWEEP);
    let mut last_rx = tokio::time::Instant::now();
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result: Result<()> = async {
        loop {
            let msg = tokio::select! {
                _ = sweep.tick() => {
                    if store.node(&node_id).await?.is_none_or(|n| n.revoked_at.is_some()) {
                        let _ = tx
                            .send(Ok(pb::ServerFrame {
                                seq: 0,
                                ack: node_seq,
                                body: Some(server_frame::Body::Revoked(pb::Revoked {
                                    reason: "node revoked".into(),
                                })),
                            }))
                            .await;
                        let _ = tx.send(Err(Status::permission_denied("node revoked"))).await;
                        return Ok(());
                    }
                    let after = sent.load(std::sync::atomic::Ordering::Relaxed);
                    for (seq, bytes) in store.outbox_after(&node_id, after as i64).await? {
                        let mut f = pb::ServerFrame::decode(bytes.as_slice())?;
                        hub.hydrate(&mut f).await;
                        f.seq = seq as u64;
                        f.ack = node_seq;
                        tx.send(Ok(f)).await?;
                        sent.fetch_max(seq as u64, std::sync::atomic::Ordering::Relaxed);
                    }
                    continue;
                }
                kicked = &mut kick_rx => {
                    if let Ok(status) = kicked {
                        let _ = tx.send(Err(status)).await;
                    }
                    return Ok(());
                }
                r = tokio::time::timeout_at(last_rx + IDLE_TIMEOUT, inbound.message()) => match r {
                    Err(_) => anyhow::bail!("idle timeout"),
                    Ok(r) => r?,
                },
            };
            let Some(frame) = msg else { return Ok(()) };
            last_rx = tokio::time::Instant::now();
            if frame.ack > 0 {
                store.outbox_ack(&node_id, frame.ack as i64).await?;
            }
            if frame.seq > 0 && frame.seq <= node_seq {
                continue; // replayed duplicate
            }
            let reply_ping = matches!(frame.body, Some(node_frame::Body::Heartbeat(_)));
            apply(&hub, &node_id, frame.body).await?;
            if frame.seq > 0 {
                node_seq = frame.seq;
                store.set_node_seq(&node_id, node_seq as i64).await?;
            }
            if reply_ping {
                tx.send(Ok(pb::ServerFrame {
                    seq: 0,
                    ack: node_seq,
                    body: Some(server_frame::Body::Ping(pb::Ping {
                        sent_at_ms: now_ms(),
                    })),
                }))
                .await?;
            }
        }
    }
    .await;

    let mut conns = hub.conns.lock().await;
    if conns.get(&node_id).map(|c| c.conn_id) == Some(conn_id) {
        conns.remove(&node_id);
        drop(conns);
        let _ = store.touch_node(&node_id).await;
        let _ = hub
            .core
            .emit(kinds::NODE_OFFLINE, &node_id, &serde_json::json!({}))
            .await;
    }
    result
}

async fn apply(hub: &Hub, node_id: &str, body: Option<node_frame::Body>) -> Result<()> {
    let store = hub.core.store();
    match body {
        None | Some(node_frame::Body::Hello(_)) => {}
        Some(node_frame::Body::Heartbeat(_)) => store.touch_node(node_id).await?,
        Some(node_frame::Body::Attention(a)) => {
            crate::attention::file(hub, node_id, a).await;
        }
        Some(node_frame::Body::Status(s)) => {
            hub.core()
                .node_status_apply(node_id, crate::status::of(s))
                .await?;
        }
        Some(node_frame::Body::Host(h)) => {
            let info = host_info(&h);
            store.update_node_host(node_id, &info).await?;
            hub.core
                .emit(
                    kinds::NODE_UPDATED,
                    node_id,
                    &serde_json::json!({ "hostname": info.hostname }),
                )
                .await?;
        }
    }
    Ok(())
}

impl Hub {
    /// The CA certificate (PEM) nodes trust after joining.
    pub fn pki_ca_pem(&self) -> String {
        self.pki.ca_cert_pem().to_string()
    }
}

/// The vault grant a replayable frame carries, if any.
fn vault_of(body: &mut server_frame::Body) -> Option<&mut pb::VaultGrant> {
    vault_slot(body).and_then(|s| s.as_mut())
}

fn vault_slot(body: &mut server_frame::Body) -> Option<&mut Option<pb::VaultGrant>> {
    match body {
        server_frame::Body::Dispatch(d) => Some(&mut d.vault),
        server_frame::Body::Job(j) => Some(&mut j.vault),
        _ => None,
    }
}
