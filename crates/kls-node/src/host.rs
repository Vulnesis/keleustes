//! Host service (docs/gaps/host-management.md §6.6): inventory reports, the
//! node's action stream, and action progress. The domain rules live in
//! `kls_core::hosts`; this file converts the wire and authenticates the
//! node by its certificate.

use crate::server::{Hub, authenticate, internal};
use kls_core::hosts::{self, kinds};
use kls_proto::v1::host_server::{Host, HostServer};
use kls_proto::v1::{self as pb};
use kls_store::host::{HostActionRow, PatchRefusal};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// How often the action stream re-reads the store for actions another
/// process queued.
const SWEEP: Duration = Duration::from_secs(2);
/// Largest inventory accepted.
const MAX_REPOS: usize = 2000;
const MAX_LIST: usize = 500;

pub struct Svc {
    pub hub: Hub,
}

/// The service, for `Hub::serve` (docs/gaps/host-proto-request.md R1).
pub fn service(hub: Hub) -> HostServer<Svc> {
    HostServer::new(Svc { hub })
}

type ActionStream = Pin<Box<dyn Stream<Item = Result<pb::HostAction, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Host for Svc {
    async fn report(&self, req: Request<pb::HostInventory>) -> Result<Response<pb::Ok>, Status> {
        let node_id = authenticate(&self.hub, &req).await?;
        let not_after = req
            .peer_certs()
            .and_then(|c| c.first().map(|l| cert_not_after_ms(l)))
            .unwrap_or(0);
        report_for_link(&self.hub, &node_id, req.into_inner(), not_after).await?;
        Ok(Response::new(pb::Ok {}))
    }

    type ActionsStream = ActionStream;

    async fn actions(
        &self,
        req: Request<pb::HostActionsRequest>,
    ) -> Result<Response<Self::ActionsStream>, Status> {
        let node_id = authenticate(&self.hub, &req).await?;
        let (tx, rx) = mpsc::channel(64);
        let hub = self.hub.clone();
        tokio::spawn(async move {
            if let Err(e) = drive_actions(hub, node_id.clone(), tx).await {
                tracing::debug!(%node_id, "host action stream ended: {e:#}");
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn progress(
        &self,
        req: Request<pb::HostActionUpdate>,
    ) -> Result<Response<pb::Ok>, Status> {
        let node_id = authenticate(&self.hub, &req).await?;
        progress_for(&self.hub, &node_id, req.into_inner()).await?;
        Ok(Response::new(pb::Ok {}))
    }
}

/// `Host.Report` for an authenticated node.
pub async fn report_for(hub: &Hub, node_id: &str, inv: pb::HostInventory) -> Result<(), Status> {
    report_for_link(hub, node_id, inv, 0).await
}

/// The node certificate's expiry in Unix ms (0 when it cannot be read).
pub fn cert_not_after_ms(der: &[u8]) -> i64 {
    x509_parser::parse_x509_certificate(der)
        .map(|(_, c)| c.validity().not_after.timestamp() * 1000)
        .unwrap_or(0)
}

/// `Host.Report` with the certificate expiry read from the handshake (HM-13).
pub async fn report_for_link(
    hub: &Hub,
    node_id: &str,
    inv: pb::HostInventory,
    cert_not_after_ms: i64,
) -> Result<(), Status> {
    if inv.repos.len() > MAX_REPOS {
        return Err(Status::invalid_argument("too many repos in one inventory"));
    }
    let inv = inventory_of(inv);
    hosts::report_inventory_link(hub.core(), node_id, &inv, cert_not_after_ms)
        .await
        .map_err(internal)?;
    Ok(())
}

/// `Host.Progress` for an authenticated node.
pub async fn progress_for(hub: &Hub, node_id: &str, u: pb::HostActionUpdate) -> Result<(), Status> {
    let upd = hosts::Update {
        action_id: u.action_id,
        seq: u.seq,
        state: u.state,
        step: u.step,
        pct: u.pct,
        lines: u.lines,
        preview_json: u.preview_json,
        preview_sha256: u.preview_sha256,
        result_json: u.result_json,
        error: u.error,
        handoff_command: u.handoff_command,
        handoff_reason: u.handoff_reason,
    };
    match hosts::apply_update(hub.core(), node_id, upd).await {
        Ok(_) => Ok(()),
        Err(e) => Err(match e.downcast_ref::<PatchRefusal>() {
            Some(r @ PatchRefusal::NotFound) => Status::not_found(r.to_string()),
            Some(r @ PatchRefusal::NotAddressed) => Status::permission_denied(r.to_string()),
            Some(r @ PatchRefusal::Stale { .. }) => Status::failed_precondition(r.to_string()),
            Some(r @ PatchRefusal::Settled) => Status::failed_precondition(r.to_string()),
            None => internal(e),
        }),
    }
}

/// Sends every unanswered action, then new ones as they are queued. Each
/// send re-checks the kill switch (rule 3); a `product_auth` action gets a
/// fresh vault grant that is never stored.
async fn drive_actions(
    hub: Hub,
    node_id: String,
    tx: mpsc::Sender<Result<pb::HostAction, Status>>,
) -> anyhow::Result<()> {
    let core = hub.core().clone();
    let mut rx = core.subscribe();
    let mut sent = std::collections::HashSet::<String>::new();
    let mut sweep = tokio::time::interval(SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        for row in core.store().host_actions_unanswered(&node_id).await? {
            if sent.contains(&row.id) {
                continue;
            }
            if !hosts::may_deliver(&core, &row).await? {
                continue;
            }
            let frame = action_frame(&hub, &row).await;
            tx.send(Ok(frame)).await?;
            core.store()
                .mark_host_action_delivered(&row.id, kls_store::now_ms())
                .await?;
            sent.insert(row.id);
        }
        tokio::select! {
            _ = tx.closed() => return Ok(()),
            _ = sweep.tick() => {}
            ev = rx.recv() => match ev {
                Ok(ev) if ev.kind == kinds::ACTION_QUEUED && ev.subject == node_id => {}
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return Ok(()),
            },
        }
    }
}

async fn action_frame(hub: &Hub, row: &HostActionRow) -> pb::HostAction {
    use kls_core::hosts::HostActionRowExt;
    let o = row.origin();
    let vault = match hosts::grant_for(hub.core(), row).await {
        Ok(Some(g)) => Some(pb::VaultGrant {
            grant_id: g.grant_id.clone(),
            // Only what the CLI's non-interactive sign-in reads: an ApiKey
            // pair or a SigV4 pair.
            secrets: g
                .secrets
                .iter()
                .filter(|s| {
                    matches!(
                        s.env.as_str(),
                        "VULNETIX_API_KEY" | "VULNETIX_ORG_ID" | "VVD_ORG" | "VVD_SECRET"
                    )
                })
                .map(|s| pb::VaultSecret {
                    provider: s.provider.clone(),
                    env: s.env.clone(),
                    value: s.value.expose().into(),
                })
                .collect(),
            expires_at_ms: g.expires_at_ms,
            mcp: Vec::new(),
        }),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("vault grant for a host action: {e:#}");
            None
        }
    };
    pb::HostAction {
        id: row.id.clone(),
        kind: row.kind.clone(),
        params: row.params().into_iter().collect(),
        dry_run: row.dry_run,
        preview_sha256: row.preview_sha256.clone(),
        if_match: row.if_match.clone(),
        vault,
        origin: Some(pb::HostOrigin {
            actor: o.actor,
            surface: o.surface,
            run_id: o.run_id,
            workflow_id: o.workflow_id,
            plan_id: o.plan_id,
            reason: o.reason,
        }),
        deadline_ms: 0,
        plan_run_id: row.plan_run_id.clone(),
        step: row.step.max(0) as u32,
        class: row.class.clone(),
    }
}

// ---- wire -> domain ---------------------------------------------------------------

fn cap(s: String, n: usize) -> String {
    if s.len() <= n {
        s
    } else {
        s.chars().take(n).collect()
    }
}

fn caps(v: Vec<String>) -> Vec<String> {
    v.into_iter().take(MAX_LIST).map(|s| cap(s, 256)).collect()
}

fn check_of(c: pb::Check) -> hosts::Check {
    hosts::Check {
        id: cap(c.id, 64),
        status: cap(c.status, 16),
        hint: cap(c.hint, 160),
    }
}

fn toggle_of(t: pb::Toggle) -> hosts::Toggle {
    hosts::Toggle {
        id: cap(t.id, 128),
        label: cap(t.label, 128),
        scope: cap(t.scope, 16),
        on: t.on,
        path: cap(t.path, 512),
        state: cap(t.state, 32),
    }
}

pub fn inventory_of(i: pb::HostInventory) -> hosts::Inventory {
    let b = i.belai.unwrap_or_default();
    let l = i.limits.unwrap_or_default();
    let g = i.gates.unwrap_or_default();
    let d = i.disk.unwrap_or_default();
    let sb = i.sandbox.unwrap_or_default();
    let m = i.manage.unwrap_or_default();
    hosts::Inventory {
        at_ms: i.at_ms,
        belai: hosts::BelaiFacts {
            version: cap(b.version, 64),
            install_method: cap(b.install_method, 32),
            latest: cap(b.latest, 64),
            update_available: b.update_available,
            root: cap(b.root, 512),
        },
        products: i
            .products
            .into_iter()
            .take(16)
            .map(|p| hosts::ProductStatus {
                product: cap(p.product, 32),
                state: cap(p.state, 32),
                version: cap(p.version, 64),
                latest: cap(p.latest, 64),
                path: cap(p.path, 512),
                install_method: cap(p.install_method, 32),
                authenticated: p.authenticated,
                org_id: cap(p.org_id, 128),
                auth_source: cap(p.auth_source, 32),
                running: p.running,
                enabled: p.enabled,
                mode: cap(p.mode, 32),
                parts: p.parts.into_iter().take(MAX_LIST).map(toggle_of).collect(),
                checks: p.checks.into_iter().take(MAX_LIST).map(check_of).collect(),
                probed_at_ms: p.probed_at_ms,
                degraded: caps(p.degraded),
                units: caps(p.units),
                entitlement: cap(p.entitlement, 32),
            })
            .collect(),
        repos: i
            .repos
            .into_iter()
            .map(|r| {
                let det = r.detections.unwrap_or_default();
                hosts::HostRepo {
                    path: cap(r.path, 512),
                    remote: cap(r.remote, 512),
                    forge_host: cap(r.forge_host, 256),
                    owner: cap(r.owner, 128),
                    name: cap(r.name, 128),
                    branch: cap(r.branch, 256),
                    git: r.git.map(|g| hosts::GitFacts {
                        commit: cap(g.commit, 64),
                        dirty: g.dirty,
                        ahead: g.ahead,
                        behind: g.behind,
                        detached: g.detached,
                    }),
                    trusted: r.trusted,
                    registered: r.registered,
                    last_used_ms: r.last_used_ms,
                    detections: hosts::RepoDetections {
                        build: caps(det.build),
                        lint: caps(det.lint),
                        test: caps(det.test),
                        languages: caps(det.languages),
                        scans: caps(det.scans),
                        dep_watch: det.dep_watch,
                        in_sweep: det.in_sweep,
                        last_scan_ms: det.last_scan_ms,
                    },
                    agents: r.agents.into_iter().take(MAX_LIST).map(toggle_of).collect(),
                    prefs: r
                        .prefs
                        .into_iter()
                        .take(MAX_LIST)
                        .map(|(k, v)| (cap(k, 128), cap(v, 512)))
                        .collect(),
                }
            })
            .collect(),
        tools: i
            .tools
            .into_iter()
            .take(MAX_LIST)
            .map(|t| hosts::ToolFact {
                name: cap(t.name, 64),
                version: cap(t.version, 64),
                ok: t.ok,
                note: cap(t.note, 160),
            })
            .collect(),
        preflight: i
            .preflight
            .into_iter()
            .take(MAX_LIST)
            .map(check_of)
            .collect(),
        limits: hosts::HostLimits {
            max_sessions: l.max_sessions,
            max_workers: l.max_workers,
            reserved_workers: l.reserved_workers,
            max_jobs: l.max_jobs,
            idle_s: l.idle_s,
            sessions: l.sessions,
            workers: l.workers,
            jobs: l.jobs,
        },
        gates: hosts::RcGates {
            web_controls: g.web_controls,
            web_shell: g.web_shell,
            web_project_settings: g.web_project_settings,
            allow_guardrails_off: g.allow_guardrails_off,
            no_clone: g.no_clone,
            allow_private_cidr: g.allow_private_cidr,
        },
        disk: hosts::DiskUsage {
            sessions: d.sessions,
            worktrees: d.worktrees,
            logs: d.logs,
            free: d.free,
        },
        settings_sha256: cap(i.settings_sha256, 64),
        plugins: caps(i.plugins),
        mcp: i
            .mcp
            .into_iter()
            .take(MAX_LIST)
            .map(|x| hosts::McpServerFact {
                name: cap(x.name, 128),
                transport: cap(x.transport, 32),
                builtin: x.builtin,
                keys_bound: caps(x.keys_bound),
            })
            .collect(),
        providers: i
            .providers
            .into_iter()
            .take(MAX_LIST)
            .map(|p| hosts::ProviderFact {
                provider: cap(p.provider, 64),
                key_present: p.key_present,
                source: cap(p.source, 32),
            })
            .collect(),
        sandbox: hosts::SandboxFact {
            mode: cap(sb.mode, 32),
            network: cap(sb.network, 32),
            backend: cap(sb.backend, 32),
        },
        manage: hosts::HostManage {
            host_actions: caps(m.host_actions)
                .into_iter()
                .filter(|k| hosts::class_of(k).is_some())
                .collect(),
            manage_level: hosts::Level::parse(&m.manage_level).as_str().into(),
            manage_shell: m.manage_shell,
        },
        schedules: i
            .schedules
            .into_iter()
            .take(MAX_LIST)
            .map(|s| hosts::HostSchedule {
                id: cap(s.id, 128),
                target: cap(s.target, 128),
                cron: cap(s.cron, 64),
                tz: cap(s.tz, 64),
                dir: cap(s.dir, 512),
                enabled: s.enabled,
                last_run_ms: s.last_run_ms,
                last_status: cap(s.last_status, 16),
            })
            .collect(),
        builtin_agents: caps(i.builtin_agents),
        agents_enabled: i.agents_enabled,
        worktrees: i
            .worktrees
            .into_iter()
            .take(MAX_LIST)
            .map(|w| hosts::WorktreeFact {
                path: cap(w.path, 512),
                repo: cap(w.repo, 256),
                branch: cap(w.branch, 256),
                bytes: w.bytes,
                stale: w.stale,
            })
            .collect(),
        // Keleustes stamps the link facts itself (report_for_link).
        link: hosts::LinkFacts::default(),
    }
}

#[cfg(test)]
mod tests;
