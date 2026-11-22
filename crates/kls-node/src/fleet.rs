//! Fleet service (WP-I): node-originated coordination. A node lists the
//! nodes it may address, asks for work on one of them, watches its
//! requests and prompts sessions elsewhere. The decisions live in
//! `kls_core::belai::fleet` (policy, approvals, kill switch, audit).

use crate::server::{Hub, authenticate, internal};
use kls_core::belai::fleet::{Ask, Status, Ticket, kinds};
use kls_core::gateway::{CloneSpec, CrewControl, DispatchSpec, WorkerBudget};
use kls_core::sessions::kinds as skinds;
use kls_proto::v1::{self as pb, fleet_server::Fleet};
use std::collections::HashMap;
use std::pin::Pin;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status as GrpcStatus};

pub struct Svc {
    pub hub: Hub,
}

/// Most requests one Watch follows.
const MAX_WATCH: usize = 64;

fn ticket_pb(t: Ticket) -> pb::DispatchTicket {
    pb::DispatchTicket {
        request_id: t.request_id,
        state: t.state,
        reason: t.reason,
        dispatch_id: t.dispatch_id,
        target_node: t.target_node,
    }
}

fn status_pb(s: Status) -> pb::DispatchStatus {
    pb::DispatchStatus {
        request_id: s.request_id,
        dispatch_id: s.dispatch_id,
        state: s.state,
        session_id: s.session_id,
        reason: s.reason,
        target_node: s.target_node,
    }
}

/// The console's spec from a node's `Dispatch`. Ids, vault grants and
/// deadlines a node sends are ignored: Keleustes sets its own.
pub(crate) fn spec_of(d: pb::Dispatch) -> DispatchSpec {
    DispatchSpec {
        kind: d.kind,
        cwd: d.cwd,
        repo: d.repo,
        branch: d.branch,
        prompt: d.prompt,
        provider: d.provider,
        model: d.model,
        agent: d.agent,
        crew: d.crew,
        mode: d.mode,
        dirs: d.dirs,
        workspace: d.workspace,
        effort: d.effort,
        git_sync: d.git_sync,
        ask: d.ask,
        budget: d.budget.map(|b| WorkerBudget {
            max_cost_usd: b.max_cost_usd,
            finalize_at: b.finalize_at,
            max_continuations: b.max_continuations,
            max_attempts: b.max_attempts,
            max_wall: b.max_wall,
            max_passes: b.max_passes,
            max_tokens: b.max_tokens,
            max_iterations: b.max_iterations,
            price_input_per_mtok: b.price_input_per_mtok,
            price_output_per_mtok: b.price_output_per_mtok,
        }),
        fill: d.fill,
        stay: d.stay,
        stay_idle_max_s: d.stay_idle_max_s,
        member_models: d.member_models.into_iter().collect(),
        clone: d.clone.map(|c| CloneSpec {
            owner: c.owner,
            name: c.name,
            branch: c.branch,
        }),
        control: d.control.map(|c| CrewControl {
            run: c.run,
            op: c.op,
            worker: c.worker,
            dir: c.dir,
            profile: c.profile,
            weight: c.weight,
            paused: c.paused,
            replicas: c.replicas,
        }),
        tier: d.tier,
        ..Default::default()
    }
}

fn refused(e: anyhow::Error) -> GrpcStatus {
    GrpcStatus::failed_precondition(e.to_string())
}

type WatchStream = Pin<Box<dyn Stream<Item = Result<pb::DispatchStatus, GrpcStatus>> + Send>>;

#[tonic::async_trait]
impl Fleet for Svc {
    async fn nodes(
        &self,
        req: Request<pb::NodesRequest>,
    ) -> Result<Response<pb::NodeList>, GrpcStatus> {
        let node = authenticate(&self.hub, &req).await?;
        let online = self.hub.online().await;
        let nodes = self
            .hub
            .core()
            .fleet_nodes(&node, &online)
            .await
            .map_err(|e| GrpcStatus::permission_denied(e.to_string()))?;
        Ok(Response::new(pb::NodeList {
            nodes: nodes
                .into_iter()
                .map(|n| pb::NodeSummary {
                    node_id: n.node_id,
                    name: n.name,
                    online: n.online,
                    remote_control: n.remote_control,
                    trusted_dirs: n.trusted_dirs,
                    agents: n.agents,
                    crews: n.crews,
                    providers: n.providers,
                    running_sessions: n.running_sessions,
                    max_sessions: n.max_sessions,
                    agent_ids: n.agent_ids,
                })
                .collect(),
        }))
    }

    async fn request(
        &self,
        req: Request<pb::DispatchRequest>,
    ) -> Result<Response<pb::DispatchTicket>, GrpcStatus> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let spec = r
            .spec
            .ok_or_else(|| GrpcStatus::invalid_argument("spec is required"))?;
        let host_kind = kls_core::hosts::is_host_kind(&spec.kind);
        let t = self
            .hub
            .core()
            .fleet_request(
                &node,
                Ask {
                    target: r.target_node,
                    spec: spec_of(spec),
                    origin_session: r.origin_session,
                    initiated_by: r.initiated_by,
                },
            )
            .await
            .map_err(internal)?;
        if host_kind {
            // Filed (and audited) as refused; no node manages another host.
            return Err(GrpcStatus::permission_denied(t.reason));
        }
        Ok(Response::new(ticket_pb(t)))
    }

    type WatchStream = WatchStream;

    async fn watch(
        &self,
        req: Request<pb::WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, GrpcStatus> {
        let node = authenticate(&self.hub, &req).await?;
        let mut ids = req.into_inner().request_ids;
        ids.truncate(MAX_WATCH);
        let core = self.hub.core().clone();
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut bus = core.subscribe();
            let mut last: HashMap<String, Status> = HashMap::new();
            loop {
                let now = match core.fleet_status(&node, &ids).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(refused(e))).await;
                        return;
                    }
                };
                for s in now {
                    if last.get(&s.request_id) != Some(&s) {
                        last.insert(s.request_id.clone(), s.clone());
                        if tx.send(Ok(status_pb(s))).await.is_err() {
                            return;
                        }
                    }
                }
                loop {
                    tokio::select! {
                        _ = tx.closed() => return,
                        ev = bus.recv() => match ev {
                            Ok(ev) if ev.kind == kinds::FLEET_REQUEST
                                || ev.kind == skinds::DISPATCH_UPDATED
                                || ev.kind == skinds::SESSION_ENDED => break,
                            Ok(_) => continue,
                            Err(RecvError::Lagged(_)) => break,
                            Err(RecvError::Closed) => return,
                        },
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn prompt(
        &self,
        req: Request<pb::PeerPrompt>,
    ) -> Result<Response<pb::DispatchTicket>, GrpcStatus> {
        let node = authenticate(&self.hub, &req).await?;
        let r = req.into_inner();
        let t = self
            .hub
            .core()
            .fleet_prompt(
                &node,
                &r.target_session,
                &r.text,
                &r.origin_session,
                &r.initiated_by,
            )
            .await
            .map_err(internal)?;
        Ok(Response::new(ticket_pb(t)))
    }
}
