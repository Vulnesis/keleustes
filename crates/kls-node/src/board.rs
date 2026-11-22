//! Board service: kanban, repository memory, A2A, factory control state.
//! Owner: M2 coordination workstream.
//!
//! Also [`start_control_broadcast`]: pushes `FactoryControl` on every
//! node's Link whenever the core emits `factory.control`.

use crate::server::{Hub, authenticate, internal};
use kls_core::board::{self as domain, PushOutcome, VersionConflict};
use kls_proto::v1::{self as pb, board_server::Board, server_frame};
use kls_store::board::{A2aMessage, Card, FactoryControl, MemoryNote};
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

pub struct Svc {
    pub hub: Hub,
}

type A2aStream = Pin<Box<dyn Stream<Item = Result<pb::A2aEnvelope, Status>> + Send + 'static>>;

const PULL_DEFAULT: u32 = 200;
const PULL_MAX: u32 = 1000;
const MEMORY_PAGE: i64 = 1000;

pub fn item_to_pb(c: &Card) -> pb::KanbanItem {
    pb::KanbanItem {
        id: c.id.clone(),
        key: c.key.clone(),
        title: c.title.clone(),
        body: c.body.clone(),
        status: c.status.clone(),
        priority: c.priority,
        labels: c.labels.clone(),
        assignee: c.assignee.clone(),
        repo: c.repo.clone(),
        branch: c.branch.clone(),
        pr_url: c.pr_url.clone(),
        created_at_ms: c.created_at,
        updated_at_ms: c.updated_at,
        version: c.version,
        deleted: c.deleted,
        meta_json: c.meta_json.clone(),
    }
}

pub fn item_from_pb(i: pb::KanbanItem) -> Card {
    Card {
        id: i.id,
        key: i.key,
        title: i.title,
        body: i.body,
        status: i.status,
        priority: i.priority,
        labels: i.labels,
        assignee: i.assignee,
        repo: i.repo,
        branch: i.branch,
        pr_url: i.pr_url,
        meta_json: i.meta_json,
        created_at: i.created_at_ms,
        updated_at: i.updated_at_ms,
        version: i.version,
        deleted: i.deleted,
        changed_at: 0,
    }
}

pub fn note_to_pb(n: &MemoryNote) -> pb::MemoryNote {
    pb::MemoryNote {
        id: n.id.clone(),
        scope: n.scope.clone(),
        text: n.text.clone(),
        tags: n.tags.clone(),
        author: n.author.clone(),
        created_at_ms: n.created_at,
        updated_at_ms: n.updated_at,
        deleted: n.deleted,
        host: n.host.clone(),
        a2a_ref: n.a2a_ref.clone(),
    }
}

pub fn envelope_to_pb(m: &A2aMessage) -> pb::A2aEnvelope {
    pb::A2aEnvelope {
        id: m.id.clone(),
        from: m.from.clone(),
        to: m.to.clone(),
        thread_id: m.thread_id.clone(),
        text: m.text.clone(),
        at_ms: m.at,
        seq: m.seq as u64,
        meta_json: m.meta_json.clone(),
    }
}

/// The protocol's control state (for `Board.State`, the Link push, and the
/// `Welcome.control` field).
pub fn control_to_pb(c: &FactoryControl) -> pb::FactoryControl {
    pb::FactoryControl {
        stopped: c.stopped,
        a2a_blackout: c.a2a_blackout,
        reason: domain::control_reason(c),
    }
}

/// Agent ids are `<nodeId>:<sessionId>`; a node speaks only for its own.
fn own_agent(node_id: &str, agent: &str) -> bool {
    agent
        .strip_prefix(node_id)
        .and_then(|r| r.strip_prefix(':'))
        .is_some_and(|s| !s.is_empty())
}

fn bad_request(e: anyhow::Error) -> Status {
    Status::invalid_argument(format!("{e:#}"))
}

/// Pushes `FactoryControl` to every joined, unrevoked node whenever the core
/// emits `factory.control`. Frames are replayable, so a node that is offline
/// gets the latest state when it reconnects. Call once per hub
/// (`kls_node::hub()` does).
pub fn start_control_broadcast(hub: Hub) {
    let mut rx = hub.core().subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) if ev.kind == domain::kinds::FACTORY_CONTROL => {}
                Ok(_) => continue,
                // Missed events: push the current state anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            }
            if let Err(e) = broadcast_control(&hub).await {
                tracing::warn!("factory control broadcast: {e:#}");
            }
        }
    });
}

async fn broadcast_control(hub: &Hub) -> anyhow::Result<()> {
    let c = control_to_pb(&hub.core().factory_control().await?);
    for n in hub.core().store().nodes().await? {
        if n.revoked_at.is_none() {
            hub.send(&n.id, server_frame::Body::Control(c.clone()))
                .await?;
        }
    }
    Ok(())
}

#[tonic::async_trait]
impl Board for Svc {
    async fn kanban_pull(
        &self,
        req: Request<pb::KanbanPullRequest>,
    ) -> Result<Response<pb::KanbanPage>, Status> {
        let _node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let limit = match r.limit {
            0 => PULL_DEFAULT,
            n => n.min(PULL_MAX),
        };
        let (cards, more) = self
            .hub
            .core()
            .store()
            .cards_changed_since(r.since_ms, limit as i64)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::KanbanPage {
            next_since_ms: cards.last().map(|c| c.changed_at).unwrap_or(r.since_ms),
            items: cards.iter().map(item_to_pb).collect(),
            more,
        }))
    }

    async fn kanban_push(
        &self,
        req: Request<pb::KanbanBatch>,
    ) -> Result<Response<pb::KanbanPushResult>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let items = req.into_inner().items;
        if items.len() > 500 {
            return Err(Status::invalid_argument("at most 500 items per push"));
        }
        let mut out = pb::KanbanPushResult::default();
        for i in items {
            match self.hub.core().push_card(item_from_pb(i), &node).await {
                Ok(PushOutcome::Accepted(c)) => out.accepted.push(item_to_pb(&c)),
                Ok(PushOutcome::Conflict(c)) => out.conflicts.push(item_to_pb(&c)),
                Err(e) => match e.downcast_ref::<VersionConflict>() {
                    Some(v) => out.conflicts.push(item_to_pb(&v.current)),
                    None => return Err(bad_request(e)),
                },
            }
        }
        Ok(Response::new(out))
    }

    async fn memory_pull(
        &self,
        req: Request<pb::MemoryPullRequest>,
    ) -> Result<Response<pb::MemoryPage>, Status> {
        let _node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let (notes, _more) = self
            .hub
            .core()
            .store()
            .notes_changed_since(&r.scope, r.since_ms, MEMORY_PAGE)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::MemoryPage {
            next_since_ms: notes.last().map(|n| n.changed_at).unwrap_or(r.since_ms),
            notes: notes.iter().map(note_to_pb).collect(),
        }))
    }

    async fn memory_push(
        &self,
        req: Request<pb::MemoryNote>,
    ) -> Result<Response<pb::MemoryNote>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let n = req.into_inner();
        let note = MemoryNote {
            id: n.id,
            scope: n.scope,
            text: n.text,
            tags: n.tags,
            author: if n.author.is_empty() {
                node.clone()
            } else {
                n.author
            },
            created_at: n.created_at_ms,
            updated_at: n.updated_at_ms,
            deleted: n.deleted,
            changed_at: 0,
            // The source host comes from the certificate, never the note.
            host: node,
            a2a_ref: n.a2a_ref.chars().take(200).collect(),
        };
        let stored = self.hub.core().push_note(note).await.map_err(bad_request)?;
        Ok(Response::new(note_to_pb(&stored)))
    }

    async fn a2a_send(
        &self,
        req: Request<pb::A2aEnvelope>,
    ) -> Result<Response<pb::A2aSendResult>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let e = req.into_inner();
        if !own_agent(&node, &e.from) {
            return Err(Status::permission_denied(
                "from must be an agent on this node (<nodeId>:<sessionId>)",
            ));
        }
        let sent = self
            .hub
            .core()
            .send_a2a(A2aMessage {
                seq: 0,
                id: e.id.clone(),
                from: e.from,
                to: e.to,
                thread_id: e.thread_id,
                text: e.text,
                meta_json: e.meta_json,
                at: e.at_ms,
            })
            .await
            .map_err(bad_request)?;
        Ok(Response::new(pb::A2aSendResult {
            id: sent.message.map(|m| m.id).unwrap_or(e.id),
            delivered: sent.delivered,
            refused: sent.refused,
        }))
    }

    type A2AReceiveStream = A2aStream;

    async fn a2a_receive(
        &self,
        req: Request<pb::A2aReceiveRequest>,
    ) -> Result<Response<Self::A2AReceiveStream>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        if r.to.is_empty() {
            return Err(Status::invalid_argument("name at least one agent in to"));
        }
        if let Some(bad) = r.to.iter().find(|a| !own_agent(&node, a)) {
            return Err(Status::permission_denied(format!(
                "{bad} is not an agent on this node"
            )));
        }
        let core = self.hub.core().clone();
        // Subscribe before the replay so nothing sent meanwhile is missed.
        let mut bus = core.subscribe();
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut after = r.after as i64;
            loop {
                // Drain everything after the cursor.
                loop {
                    let (rows, cursor) = match core.a2a_inbox(&r.to, after).await {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = tx.send(Err(internal(e))).await;
                            return;
                        }
                    };
                    for m in &rows {
                        if tx.send(Ok(envelope_to_pb(m))).await.is_err() {
                            return;
                        }
                    }
                    if cursor == after {
                        break;
                    }
                    after = cursor;
                }
                // Wait for a new envelope or a lifted blackout.
                loop {
                    tokio::select! {
                        _ = tx.closed() => return,
                        ev = bus.recv() => match ev {
                            Ok(ev) if ev.kind == domain::kinds::A2A_SENT
                                || ev.kind == domain::kinds::FACTORY_CONTROL => break,
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                            Err(_) => return,
                        },
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn state(
        &self,
        req: Request<pb::StateRequest>,
    ) -> Result<Response<pb::FactoryControl>, Status> {
        let _node = authenticate(&self.hub, &req).await?;
        let c = self.hub.core().factory_control().await.map_err(internal)?;
        Ok(Response::new(control_to_pb(&c)))
    }

    /// WP-G: the node's schedules changed after a cursor.
    async fn schedule_pull(
        &self,
        req: Request<pb::SchedulePullRequest>,
    ) -> Result<Response<pb::SchedulePage>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let (items, next, more) = self
            .hub
            .core()
            .schedule_pull(&node, r.since_ms, r.limit as i64)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::SchedulePage {
            items: items.iter().map(crate::schedules::to_pb).collect(),
            next_since_ms: next,
            more,
        }))
    }

    /// WP-G: the node's schedule edits and run records.
    async fn schedule_push(
        &self,
        req: Request<pb::ScheduleBatch>,
    ) -> Result<Response<pb::SchedulePushResult>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        let items = req
            .into_inner()
            .items
            .into_iter()
            .map(|s| crate::schedules::of_pb(&node, s))
            .collect();
        let (acc, conf) = self
            .hub
            .core()
            .schedule_push(&node, items)
            .await
            .map_err(internal)?;
        Ok(Response::new(pb::SchedulePushResult {
            accepted: acc.iter().map(crate::schedules::to_pb).collect(),
            conflicts: conf.iter().map(crate::schedules::to_pb).collect(),
        }))
    }

    /// Live sessions any node may message (harnesses.md). Empty under the
    /// A2A blackout, which no path bypasses.
    async fn a2a_peers(
        &self,
        req: Request<pb::A2aPeersRequest>,
    ) -> Result<Response<pb::A2aPeerList>, Status> {
        let _node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let core = self.hub.core();
        if kls_core::control::state(core)
            .await
            .map_err(internal)?
            .a2a_blackout
        {
            return Ok(Response::new(pb::A2aPeerList { peers: Vec::new() }));
        }
        let limit = if r.limit == 0 { 50 } else { r.limit.min(500) } as i64;
        let peers = core.a2a_peers(&r.repo, limit).await.map_err(internal)?;
        Ok(Response::new(pb::A2aPeerList {
            peers: peers
                .into_iter()
                .map(|p| pb::A2aPeer {
                    agent_id: p.agent_id,
                    node_name: p.node_name,
                    title: p.title,
                    repo: p.repo,
                    branch: p.branch,
                    harness: p.harness,
                    agent: p.agent,
                    updated_at_ms: p.updated_at,
                })
                .collect(),
        }))
    }

    /// WP-J: a host asks the owner for something (crate::attention).
    async fn attention(
        &self,
        req: Request<pb::AttentionRequest>,
    ) -> Result<Response<pb::AttentionReceipt>, Status> {
        let node = authenticate(&self.hub, &req).await?;
        Ok(Response::new(
            crate::attention::file(&self.hub, &node, req.into_inner()).await,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pki, token};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use kls_core::Core;
    use kls_proto::v1::board_client::BoardClient;
    use kls_proto::v1::node_client::NodeClient;
    use kls_proto::v1::{node_frame, server_frame};
    use rcgen::{CertificateParams, KeyPair};
    use std::time::Duration;
    use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

    struct Harness {
        hub: Hub,
        addr: String,
        node_id: String,
        identity: (Vec<u8>, KeyPair),
        _dir: tempfile::TempDir,
        _stop: tokio::sync::oneshot::Sender<()>,
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

    /// A server on loopback with one joined node.
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
        let hub = crate::hub(core).await.unwrap();
        let (stop, rx) = tokio::sync::oneshot::channel();
        let h2 = hub.clone();
        tokio::spawn(async move {
            crate::run(h2, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let minted = hub.mint_token("", token::DEFAULT_TTL_MS).await.unwrap();
        let p = token::Payload::decode(&minted.token).unwrap();
        let key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap();
        let mut client = NodeClient::new(channel(&listen, &hub.pki_ca_pem(), None).await);
        let joined = client
            .join(pb::JoinRequest {
                token_id: p.t,
                secret: URL_SAFE_NO_PAD.decode(p.s).unwrap(),
                csr_der: csr.der().to_vec(),
                host: Some(pb::HostAdvert {
                    hostname: "worker-1".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap()
            .into_inner();
        Harness {
            hub,
            addr: listen,
            node_id: joined.node_id,
            identity: (joined.client_cert_der, key),
            _dir: dir,
            _stop: stop,
        }
    }

    impl Harness {
        async fn mtls(&self) -> Channel {
            let id = Identity::from_pem(
                pem("CERTIFICATE", &self.identity.0),
                self.identity.1.serialize_pem(),
            );
            channel(&self.addr, &self.hub.pki_ca_pem(), Some(id)).await
        }
        async fn board(&self) -> BoardClient<Channel> {
            BoardClient::new(self.mtls().await)
        }
        fn agent(&self, s: &str) -> String {
            format!("{}:{s}", self.node_id)
        }
    }

    #[tokio::test]
    async fn kanban_pull_push_conflict() {
        let h = start().await;
        let core = h.hub.core().clone();
        let a = core
            .create_card(
                domain::CardDraft {
                    title: "A".into(),
                    status: "todo".into(),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        let b = core
            .create_card(
                domain::CardDraft {
                    title: "B".into(),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        let mut board = h.board().await;

        // Unauthenticated callers are refused.
        let mut anon = BoardClient::new(channel(&h.addr, &h.hub.pki_ca_pem(), None).await);
        let err = anon
            .kanban_pull(pb::KanbanPullRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);

        // Paged pull.
        let p1 = board
            .kanban_pull(pb::KanbanPullRequest {
                since_ms: 0,
                limit: 1,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(p1.items.len(), 1);
        assert!(p1.more);
        assert_eq!(p1.items[0].id, a.id);
        let p2 = board
            .kanban_pull(pb::KanbanPullRequest {
                since_ms: p1.next_since_ms,
                limit: 10,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            p2.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            [b.id.as_str()]
        );
        assert!(!p2.more);

        // Push an edit based on version 1, then the same stale edit again.
        let mut edit = p1.items[0].clone();
        edit.status = "doing".into();
        let r = board
            .kanban_push(pb::KanbanBatch {
                items: vec![edit.clone()],
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(r.accepted.len(), 1);
        assert_eq!(r.accepted[0].version, 2);
        let r = board
            .kanban_push(pb::KanbanBatch { items: vec![edit] })
            .await
            .unwrap()
            .into_inner();
        assert!(r.accepted.is_empty());
        assert_eq!(r.conflicts[0].version, 2);
        assert_eq!(r.conflicts[0].status, "doing");

        // Only the moved card follows the cursor.
        let p3 = board
            .kanban_pull(pb::KanbanPullRequest {
                since_ms: p2.next_since_ms,
                limit: 0,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(p3.items.len(), 1);
        assert_eq!(p3.items[0].id, a.id);
        let log = core.card_activity(&a.id).await.unwrap();
        assert_eq!(log[0].actor, h.node_id);

        // A bad item is refused.
        let err = board
            .kanban_push(pb::KanbanBatch {
                items: vec![pb::KanbanItem {
                    title: "x".into(),
                    status: "bogus".into(),
                    ..Default::default()
                }],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn memory_push_pull() {
        let h = start().await;
        let mut board = h.board().await;
        let n = board
            .memory_push(pb::MemoryNote {
                id: "n1".into(),
                scope: "acme/api".into(),
                text: "tests run with just check".into(),
                tags: vec!["build".into()],
                created_at_ms: 100,
                updated_at_ms: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(n.author, h.node_id);
        // An older copy loses.
        let n2 = board
            .memory_push(pb::MemoryNote {
                id: "n1".into(),
                scope: "acme/api".into(),
                text: "stale".into(),
                created_at_ms: 100,
                updated_at_ms: 50,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(n2.text, "tests run with just check");
        let too_long = board
            .memory_push(pb::MemoryNote {
                scope: "acme/api".into(),
                text: "x".repeat(domain::NOTE_TEXT_MAX + 1),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(too_long.code(), tonic::Code::InvalidArgument);

        h.hub
            .core()
            .add_note("global", "be kind", vec![], "owner")
            .await
            .unwrap();
        let all = board
            .memory_pull(pb::MemoryPullRequest {
                scope: String::new(),
                since_ms: 0,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(all.notes.len(), 2);
        let scoped = board
            .memory_pull(pb::MemoryPullRequest {
                scope: "acme/api".into(),
                since_ms: 0,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(scoped.notes.len(), 1);
        // Deletion shows up after the cursor.
        h.hub.core().delete_note("n1").await.unwrap();
        let after = board
            .memory_pull(pb::MemoryPullRequest {
                scope: String::new(),
                since_ms: all.next_since_ms,
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(after.notes.len(), 1);
        assert!(after.notes[0].deleted);
    }

    #[tokio::test]
    async fn a2a_replay_live_and_blackout() {
        let h = start().await;
        let mut board = h.board().await;
        let (s1, s2) = (h.agent("s1"), h.agent("s2"));

        // Sent before anyone listens: replayed.
        let first = board
            .a2a_send(pb::A2aEnvelope {
                from: s1.clone(),
                to: s2.clone(),
                text: "are you editing src/a.go?".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        assert!(first.delivered);

        // Spoofed sender.
        let err = board
            .a2a_send(pb::A2aEnvelope {
                from: "someone-else:s9".into(),
                text: "hi".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        let err = board
            .a2a_receive(pb::A2aReceiveRequest {
                to: vec!["other:s1".into()],
                after: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);

        let mut inbox = board
            .a2a_receive(pb::A2aReceiveRequest {
                to: vec![s2.clone()],
                after: 0,
            })
            .await
            .unwrap()
            .into_inner();
        let got = tokio::time::timeout(Duration::from_secs(5), inbox.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(got.id, first.id);
        assert_eq!(got.thread_id, first.id);

        // Live.
        let mut sender = h.board().await;
        let reply = sender
            .a2a_send(pb::A2aEnvelope {
                from: s1.clone(),
                to: s2.clone(),
                thread_id: first.id.clone(),
                text: "second".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        let got = tokio::time::timeout(Duration::from_secs(5), inbox.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(got.id, reply.id);
        assert_eq!(got.thread_id, first.id);
        assert!(got.seq > 0);

        // Blackout refuses sends.
        h.hub.core().set_a2a_blackout(true, "test").await.unwrap();
        let refused = sender
            .a2a_send(pb::A2aEnvelope {
                from: s1.clone(),
                to: s2.clone(),
                text: "third".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();
        assert!(!refused.delivered);
        assert_eq!(refused.refused, "a2a blackout");
        let st = sender
            .state(pb::StateRequest {})
            .await
            .unwrap()
            .into_inner();
        assert!(st.a2a_blackout && !st.stopped);
        assert_eq!(st.reason, "test");
        assert!(
            tokio::time::timeout(Duration::from_millis(300), inbox.message())
                .await
                .is_err(),
            "nothing is relayed during the blackout"
        );
    }

    #[tokio::test]
    async fn control_is_pushed_on_link() {
        let h = start().await;
        let mut link = NodeClient::new(h.mtls().await);
        let (tx, rx) = mpsc::channel(8);
        tx.send(pb::NodeFrame {
            body: Some(node_frame::Body::Hello(pb::Hello::default())),
            ..Default::default()
        })
        .await
        .unwrap();
        let mut down = link
            .link(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        let w = down.message().await.unwrap().unwrap();
        assert!(matches!(w.body, Some(server_frame::Body::Welcome(_))));

        h.hub
            .core()
            .set_factory_stopped(true, "freeze")
            .await
            .unwrap();
        let f = tokio::time::timeout(Duration::from_secs(5), down.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match f.body {
            Some(server_frame::Body::Control(c)) => {
                assert!(c.stopped);
                assert_eq!(c.reason, "freeze");
                assert!(f.seq > 0, "replayable");
            }
            other => panic!("expected control, got {other:?}"),
        }
        drop(tx);
    }
}
