//! `NodeGateway` on `Hub`: dispatches and jobs routed to nodes over Link.
//! Owner: M1 sessions workstream.
//!
//! * A dispatch is a `dispatches` row plus a replayable `ServerFrame::Dispatch`
//!   on the node's outbox. It turns "delivered" once the node acks that
//!   frame on Link and "started"/"failed" when the node reports through
//!   Jobs.Complete (job_id = dispatch id) or a session upserts naming it.
//! * A job is a `jobs` row plus a `ServerFrame::Job` to an explicit node or
//!   to the least busy online node meeting the job's needs; `job()` waits
//!   for its Jobs.Complete (announced as `job.updated`) up to the timeout.

use crate::server::Hub;
use anyhow::{Context, Result, bail};
use kls_core::sessions::{Dispatch, kinds as skinds};
use kls_core::{Core, DispatchSpec, JobNeeds, JobOutcome, NodeGateway, kinds as core_kinds};
use kls_proto::v1::{self as pb, server_frame};
use kls_store::{Node, now_ms};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// How often queued dispatches are re-checked against node acks while
/// some are outstanding.
const DELIVERY_SWEEP: Duration = Duration::from_millis(500);

/// Lifetime of the provider keys attached to a dispatch (it has no deadline).
const DISPATCH_GRANT_TTL_MS: i64 = 2 * 60 * 60 * 1000;

/// Installs the gateway on the core and starts the delivery sweeper.
pub fn install(hub: &Hub) {
    hub.core().set_gateway(Arc::new(hub.clone()));
    let core = hub.core().clone();
    tokio::spawn(delivery_sweeper(core));
}

/// Marks dispatches delivered once their outbox frame is acked. Sleeps on
/// the event bus while nothing is queued.
async fn delivery_sweeper(core: Core) {
    let mut bus = core.subscribe();
    loop {
        let queued = match core.store().count_queued_dispatches().await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("dispatch sweep: {e:#}");
                0
            }
        };
        if queued > 0 {
            if let Err(e) = core.sweep_delivered_dispatches().await {
                tracing::warn!("dispatch sweep: {e:#}");
            }
            tokio::time::sleep(DELIVERY_SWEEP).await;
            continue;
        }
        // Wait for a new dispatch (or a node coming back with old ones).
        loop {
            match bus.recv().await {
                Ok(ev)
                    if ev.kind == skinds::DISPATCH_UPDATED
                        || ev.kind == core_kinds::NODE_ONLINE =>
                {
                    break;
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => return,
            }
        }
    }
}

fn eligible(node: &Node, needs: &JobNeeds) -> bool {
    let caps = &node.host.capabilities;
    node.revoked_at.is_none()
        && (!needs.gh || caps.gh_authenticated)
        && needs
            .provider
            .as_deref()
            .filter(|p| !p.is_empty())
            .is_none_or(|p| caps.providers.iter().any(|q| q == p))
        && needs.forge.as_ref().is_none_or(|(cli, host)| {
            caps.forges
                .iter()
                .any(|f| f.signed_in && &f.cli == cli && f.host.eq_ignore_ascii_case(host))
        })
}

/// Whether the node runs jobs of `kind`: it lists the kind in
/// `Capabilities.job_kinds` (a node that lists none predates the list).
fn runs_kind(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|k| k == kind)
}

impl Hub {
    /// Picks the node for a job of `kind`: `explicit` when given (it must
    /// exist, not be revoked, run `kind` and meet `needs`), else the online
    /// node that runs `kind` and meets `needs` with the fewest open jobs.
    pub async fn pick_job_node(
        &self,
        explicit: Option<&str>,
        kind: &str,
        needs: &JobNeeds,
    ) -> Result<String> {
        let store = self.core().store();
        if let Some(id) = explicit.filter(|s| !s.is_empty()) {
            let node = store.node(id).await?.context("unknown node")?;
            if node.revoked_at.is_some() {
                bail!("node {} is revoked", node.name);
            }
            if !runs_kind(&node.host.capabilities.job_kinds, kind) {
                bail!(
                    "node {} does not run {kind} jobs; update belai on the host",
                    node.name
                );
            }
            if !eligible(&node, needs) {
                bail!(
                    "node {} cannot run this job ({})",
                    node.name,
                    describe(needs)
                );
            }
            return Ok(node.id);
        }
        // Row 48: nodes whose plan window for the provider is nearly used
        // are skipped while another can run the job.
        let online = kls_core::belai::plan::route_filter(
            self.core(),
            self.online().await,
            needs.provider.as_deref().unwrap_or(""),
        )
        .await;
        let load = store.open_jobs_by_node().await?;
        let mut best: Option<(i64, String)> = None;
        for n in store.nodes().await? {
            if !online.contains(&n.id)
                || !runs_kind(&n.host.capabilities.job_kinds, kind)
                || !eligible(&n, needs)
            {
                continue;
            }
            let l = load
                .iter()
                .find(|(id, _)| *id == n.id)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            if best
                .as_ref()
                .is_none_or(|(bl, bid)| (l, &n.id) < (*bl, bid))
            {
                best = Some((l, n.id));
            }
        }
        best.map(|(_, id)| id)
            .with_context(|| format!("no online node can run this job ({})", describe(needs)))
    }
}

fn describe(needs: &JobNeeds) -> String {
    let mut parts = Vec::new();
    if needs.gh {
        parts.push("GitHub CLI signed in".to_string());
    }
    if let Some(p) = needs.provider.as_deref().filter(|p| !p.is_empty()) {
        parts.push(format!("provider {p}"));
    }
    if let Some((cli, host)) = &needs.forge {
        parts.push(format!("{cli} signed in to {host}"));
    }
    if parts.is_empty() {
        "any node".into()
    } else {
        parts.join(", ")
    }
}

#[tonic::async_trait]
impl NodeGateway for Hub {
    async fn online(&self) -> Vec<String> {
        Hub::online(self).await
    }

    async fn dispatch(&self, node_id: &str, mut spec: DispatchSpec) -> Result<String> {
        let core = self.core();
        let node = core.store().node(node_id).await?.context("unknown node")?;
        if node.revoked_at.is_some() {
            bail!("node {} is revoked", node.name);
        }
        let kind = if spec.kind.is_empty() {
            "session".to_string()
        } else {
            spec.kind.clone()
        };
        spec.kind = kind.clone();
        if let Err(why) = check_dispatch(core, &node, &spec).await {
            refused(core, node_id, &spec, &why).await;
            bail!("{why}");
        }
        if spec.trigger.is_none() {
            spec.trigger = default_trigger(&spec);
        }
        // Provider keys first: a vault failure leaves no dangling row. Only
        // work that starts something gets keys.
        // A harness session with its own login takes no keys at all.
        let own_login =
            !spec.harness.is_empty() && matches!(spec.harness_keys.as_str(), "" | "own_login");
        // Keys scoped to other nodes, and expired keys, stay behind.
        let starts = matches!(kind.as_str(), "session" | "worker" | "crew") && !own_login;
        let purpose = if spec.harness.is_empty() {
            "dispatch"
        } else {
            "harness"
        };
        let grant = if starts {
            kls_core::vault::grant_for_node(
                core,
                &spec.vault_providers,
                grant_ttl(&spec),
                purpose,
                Some(node_id),
            )
            .await?
        } else {
            None
        };
        // MCP credentials (WP-F): the bound env secrets, for the named
        // server keys only.
        let mcp = if starts && !spec.mcp.is_empty() {
            mcp_grant(core, &spec, node_id).await?
        } else {
            Vec::new()
        };
        let id = uuid::Uuid::now_v7().to_string();
        core.dispatch_queued(&Dispatch {
            id: id.clone(),
            node_id: node_id.to_string(),
            kind: kind.clone(),
            spec_json: serde_json::to_string(&spec)?,
            status: "queued".into(),
            card_id: spec.card_id.clone(),
            run_id: spec.run_id.clone(),
            ..Default::default()
        })
        .await?;
        let vault = match (grant.map(grant_pb), mcp.is_empty()) {
            (Some(mut g), false) => {
                g.mcp = mcp;
                Some(g)
            }
            (None, false) => Some(pb::VaultGrant {
                grant_id: uuid::Uuid::now_v7().to_string(),
                secrets: Vec::new(),
                expires_at_ms: now_ms() + grant_ttl(&spec),
                mcp,
            }),
            (g, true) => g,
        };
        let frame = server_frame::Body::Dispatch(dispatch_pb(&id, spec, vault));
        let seq = match self.send(node_id, frame).await {
            Ok(seq) => seq,
            Err(e) => {
                let _ = core
                    .store()
                    .resolve_dispatch(&id, "failed", "", &format!("not sent: {e:#}"))
                    .await;
                return Err(e);
            }
        };
        core.dispatch_sent(&id, seq).await?;
        Ok(id)
    }

    async fn job(
        &self,
        node_id: Option<&str>,
        kind: &str,
        payload_json: &str,
        needs: JobNeeds,
        timeout_ms: i64,
    ) -> Result<JobOutcome> {
        let core = self.core();
        let node = self.pick_job_node(node_id, kind, &needs).await?;
        let id = uuid::Uuid::now_v7().to_string();
        let timeout_ms = timeout_ms.max(1);
        let deadline = now_ms() + timeout_ms;
        let providers: Vec<String> = needs.provider.iter().cloned().collect();
        let grant = kls_core::vault::grant_for_node(
            core,
            &providers,
            timeout_ms + 60_000,
            &format!("job:{kind}"),
            Some(&node),
        )
        .await?;
        // Subscribe before sending so the result cannot slip past.
        let mut bus = core.subscribe();
        core.job_queued(&id, &node, kind, payload_json, deadline)
            .await?;
        if let Err(e) = self
            .send(
                &node,
                server_frame::Body::Job(pb::Job {
                    id: id.clone(),
                    kind: kind.to_string(),
                    payload_json: payload_json.to_string(),
                    vault: grant.map(grant_pb),
                    deadline_ms: deadline,
                }),
            )
            .await
        {
            let _ = core
                .store()
                .resolve_job(&id, "failed", "", &format!("not sent: {e:#}"))
                .await;
            return Err(e);
        }
        let sleep = tokio::time::sleep(Duration::from_millis(timeout_ms as u64));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                ev = bus.recv() => match ev {
                    Ok(ev) if ev.kind == skinds::JOB_UPDATED && ev.subject == id => {}
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => bail!("event bus closed"),
                },
                _ = &mut sleep => {
                    let job = match core.job_timed_out(&id).await? {
                        Some(j) => j,
                        None => core.store().job(&id).await?.context("job vanished")?,
                    };
                    return Ok(outcome(job));
                }
            }
            if let Some(j) = core.store().job(&id).await?
                && j.resolved_at.is_some()
            {
                return Ok(outcome(j));
            }
        }
    }
}

fn outcome(j: kls_store::sessions::Job) -> JobOutcome {
    JobOutcome {
        status: j.status,
        output_json: j.output_json,
        error: j.error,
        session_id: String::new(),
        node_id: j.node_id,
    }
}

/// The wire form of a vault grant. Values are copied, never logged.
fn grant_pb(g: kls_core::vault::GrantPayload) -> pb::VaultGrant {
    pb::VaultGrant {
        grant_id: g.grant_id,
        secrets: g
            .secrets
            .into_iter()
            .map(|s| pb::VaultSecret {
                provider: s.provider,
                env: s.env,
                value: s.value.expose().into(),
            })
            .collect(),
        expires_at_ms: g.expires_at_ms,
        mcp: Vec::new(),
    }
}

/// Event kind: a dispatch was refused before it was queued. Subject: node.
pub const DISPATCH_REFUSED: &str = "dispatch.refused";

/// Kinds that only stop or hold work: they still go out while the factory
/// is stopped.
fn winds_down(spec: &DispatchSpec) -> bool {
    match spec.kind.as_str() {
        "stop" | "pause" => true,
        "crew_control" => spec
            .control
            .as_ref()
            .is_some_and(|c| matches!(c.op.as_str(), "stop" | "worker_pause")),
        _ => false,
    }
}

/// Why a dispatch may not be queued for `node`, if it may not.
async fn check_dispatch(core: &Core, node: &Node, spec: &DispatchSpec) -> Result<(), String> {
    let kind = spec.kind.as_str();
    if !kls_core::gateway::DISPATCH_KINDS.contains(&kind) {
        return Err(format!("unknown dispatch kind {kind:?}"));
    }
    if !node.host.capabilities.takes_dispatch(kind) {
        return Err(format!(
            "node {} does not take {kind} dispatches (update Belai on the host)",
            node.name
        ));
    }
    if let Some(f) = kls_core::sessions::start::features_needed(spec)
        .into_iter()
        .find(|f| !node.host.capabilities.takes_feature(f))
    {
        return Err(format!(
            "node {} does not take the {f} dispatch field (update Belai on the host)",
            node.name
        ));
    }
    if !winds_down(spec) {
        let origin = match &spec.trigger {
            Some(t) if t.source == "node" => format!("node:{}", t.origin_node),
            Some(t) if !t.run_id.is_empty() => format!("run:{}", t.run_id),
            _ if !spec.run_id.is_empty() => format!("run:{}", spec.run_id),
            _ => "owner".to_string(),
        };
        kls_core::control::check(core, "dispatch", &node.id, &origin)
            .await
            .map_err(|e| e.to_string())?;
    }
    match kind {
        "pause" | "resume" if spec.worker_id.trim().is_empty() => {
            return Err(format!("a {kind} dispatch names a worker"));
        }
        "stop" if spec.session_id.trim().is_empty() => {
            return Err("a stop dispatch names a session".into());
        }
        "crew_control" if spec.control.as_ref().is_none_or(|c| c.op.is_empty()) => {
            return Err("a crew_control dispatch carries a control".into());
        }
        "project_prefs"
            if spec.cwd.is_empty()
                || (spec.prefs_set.is_empty() && spec.prefs_unset.is_empty()) =>
        {
            return Err("a project_prefs dispatch names a directory and keys".into());
        }
        "steer" if spec.worker_id.is_empty() || spec.steer_request.is_empty() => {
            return Err("a steer dispatch names the worker and the request".into());
        }
        "library_install" | "library_backup"
            if spec.library.is_empty() || spec.library.len() > 64 =>
        {
            return Err(format!("a {kind} dispatch names 1 to 64 library items"));
        }
        "budget_update"
            if spec.budget_push.as_ref().is_none_or(|p| {
                !kls_core::gateway::BUDGET_PUSH_ACTIONS.contains(&p.action.as_str())
                    || (p.action == "downgrade" && p.tier.is_empty())
                    || p.session_ids.len() > 256
            }) =>
        {
            return Err("a budget_update dispatch carries a known push".into());
        }
        "knowledge_reindex"
            if !matches!(
                spec.options.get("scope").map(String::as_str),
                Some("profile" | "project")
            ) || (spec.options.get("key").is_none_or(|k| k.is_empty())
                && spec.options.get("root").is_none_or(|r| r.is_empty())) =>
        {
            return Err("a knowledge_reindex dispatch names a scope and a key or root".into());
        }
        _ => {}
    }
    if !spec.dirs.is_empty() {
        let offered = &node.host.trusted_dirs;
        if let Some(d) = spec.dirs.iter().find(|d| !offered.contains(d)) {
            return Err(format!("node {} does not offer {d}", node.name));
        }
    }
    if spec.clone.is_some() && !node.host.capabilities.can_clone {
        return Err(format!("node {} cannot clone", node.name));
    }
    Ok(())
}

/// Records a refusal: an event and an audit entry, with the provenance the
/// spec carried.
async fn refused(core: &Core, node_id: &str, spec: &DispatchSpec, why: &str) {
    let origin = spec
        .trigger
        .as_ref()
        .map(|t| t.source.clone())
        .unwrap_or_default();
    let _ = core
        .emit(
            DISPATCH_REFUSED,
            node_id,
            &serde_json::json!({ "kind": spec.kind, "reason": why, "source": origin }),
        )
        .await;
    let draft = kls_store::audit::LedgerDraft {
        kind: "dispatch".into(),
        actor: if origin.is_empty() {
            "owner".into()
        } else {
            origin.clone()
        },
        actor_kind: if origin == "node" { "host" } else { "user" }.into(),
        msg: format!("{} dispatch refused: {why}", spec.kind),
        subject: serde_json::json!({ "node": node_id, "kind": spec.kind }),
        payload: serde_json::json!({ "outcome": "refused", "reason": why, "trigger": spec.trigger }),
        origin: match &spec.trigger {
            Some(t) if t.source == "node" => format!("host:{}", t.origin_node),
            _ => "factory".into(),
        },
        node_id: node_id.into(),
        ..Default::default()
    };
    // A refusal is always audited: retry a write that raced another.
    for i in 1..=5u64 {
        match core.audit_record(draft.clone()).await {
            Ok(_) => break,
            Err(e) if i == 5 => tracing::warn!("audit of a refused dispatch: {e:#}"),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(40 * i)).await,
        }
    }
}

/// The provenance of a dispatch that named none.
fn default_trigger(spec: &DispatchSpec) -> Option<kls_core::gateway::Trigger> {
    use kls_core::gateway::Trigger;
    if !spec.run_id.is_empty() {
        Some(Trigger {
            source: "workflow".into(),
            run_id: spec.run_id.clone(),
            card_id: spec.card_id.clone(),
            ..Default::default()
        })
    } else if !spec.card_id.is_empty() {
        Some(Trigger {
            source: "card".into(),
            card_id: spec.card_id.clone(),
            ..Default::default()
        })
    } else {
        Some(Trigger {
            source: "console".into(),
            ..Default::default()
        })
    }
}

/// A long-lived worker's grant lasts no longer than it may wait for work.
fn grant_ttl(spec: &DispatchSpec) -> i64 {
    if spec.stay && spec.stay_idle_max_s > 0 {
        (spec.stay_idle_max_s as i64 * 1000).min(24 * 60 * 60 * 1000)
    } else {
        DISPATCH_GRANT_TTL_MS
    }
}

/// The wire form of a dispatch.
pub(crate) fn dispatch_pb(
    id: &str,
    s: DispatchSpec,
    vault: Option<pb::VaultGrant>,
) -> pb::Dispatch {
    pb::Dispatch {
        budget_push: s.budget_push.map(|p| pb::BudgetPush {
            action: p.action,
            tier: p.tier,
            budget_id: p.budget_id,
            budget_name: p.budget_name,
            reason: p.reason,
            session_ids: p.session_ids,
        }),
        id: id.to_string(),
        kind: s.kind,
        cwd: s.cwd,
        repo: s.repo,
        branch: s.branch,
        prompt: s.prompt,
        provider: s.provider,
        model: s.model,
        agent: s.agent,
        crew: s.crew,
        card_id: s.card_id,
        run_id: s.run_id,
        mode: s.mode,
        vault,
        deadline_ms: 0,
        dirs: s.dirs,
        workspace: s.workspace,
        effort: s.effort,
        git_sync: s.git_sync,
        ask: s.ask,
        trigger: s.trigger.map(|t| pb::Trigger {
            source: t.source,
            run_id: t.run_id,
            workflow_id: t.workflow_id,
            workflow_name: t.workflow_name,
            card_id: t.card_id,
            subject: t.subject,
            origin_node: t.origin_node,
            origin_session: t.origin_session,
            origin_name: t.origin_name,
        }),
        budget: s.budget.map(|b| pb::WorkerBudget {
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
        fill: s.fill,
        stay: s.stay,
        stay_idle_max_s: s.stay_idle_max_s,
        member_models: s.member_models.into_iter().collect(),
        clone: s.clone.map(|c| pb::CloneSpec {
            owner: c.owner,
            name: c.name,
            branch: c.branch,
        }),
        worker_id: s.worker_id,
        session_id: s.session_id,
        control: s.control.map(|c| pb::CrewControl {
            run: c.run,
            op: c.op,
            worker: c.worker,
            dir: c.dir,
            profile: c.profile,
            weight: c.weight,
            paused: c.paused,
            replicas: c.replicas,
        }),
        tier: s.tier,
        harness: s.harness,
        harness_mode: s.harness_mode,
        harness_resume: s.harness_resume,
        harness_keys: s.harness_keys,
        origin_json: s.origin_json,
        prefs_set: s.prefs_set.into_iter().collect(),
        prefs_unset: s.prefs_unset,
        options: s.options.into_iter().collect(),
        library: s
            .library
            .into_iter()
            .map(|r| pb::LibraryRef {
                kind: r.kind,
                id: r.id,
                name: r.name,
                version: r.version,
                sha256: r.sha256,
            })
            .collect(),
        overwrite: s.overwrite,
        steer_request: s.steer_request,
        steer_event: s.steer_event,
        steer_pr_url: s.steer_pr_url,
        steer_until_ms: s.steer_until_ms,
        steer_reason: s.steer_reason,
        steer_pr: s.steer_pr,
        item_kind: s.item_kind,
        scan_dispatch: s.scan_dispatch,
        path: s.path,
        sha256: s.sha256,
        target: s.target,
        resume_session: s.resume_session,
        fork_entry: s.fork_entry,
        answers_json: s.answers_json,
        adhoc: s.adhoc.map(|a| pb::AdhocWorker {
            prompt: a.prompt,
            tools: a.tools,
            claim_lists: a.claim_lists,
            claim_labels: a.claim_labels,
            to: a.to,
            publish: a.publish,
            read_only: a.read_only,
        }),
    }
}

/// Harness dispatches (harnesses.md): the fields on the ordinary dispatch.
#[tonic::async_trait]
impl kls_core::harness::HarnessDispatcher for Hub {
    async fn dispatch_harness(
        &self,
        node_id: &str,
        mut spec: DispatchSpec,
        fields: kls_core::harness::HarnessFields,
    ) -> Result<String> {
        if spec.kind.is_empty() {
            spec.kind = "session".into();
        }
        if spec.kind != "session" {
            bail!("a harness runs as a session");
        }
        if spec.origin_json.is_empty() {
            spec.origin_json = serde_json::json!({
                "actor": "owner", "runId": spec.run_id, "cardId": spec.card_id,
                "keyId": fields.key_id,
            })
            .to_string();
        }
        spec.harness = fields.harness;
        spec.harness_mode = fields.harness_mode;
        spec.harness_resume = fields.harness_resume;
        spec.harness_keys = if fields.harness_keys.is_empty() {
            "own_login".into()
        } else {
            fields.harness_keys
        };
        NodeGateway::dispatch(self, node_id, spec).await
    }
}

/// The MCP credentials a dispatch binds (WP-F): each binding names an
/// env-var secret in the vault and the (server, key) on the host it feeds.
/// Only the bound secrets leave, scoped to the node like any key.
async fn mcp_grant(core: &Core, spec: &DispatchSpec, node_id: &str) -> Result<Vec<pb::McpSecret>> {
    let g = kls_core::vault::grant_for_node(
        core,
        &[kls_core::vault::ENV_PROVIDER.to_string()],
        grant_ttl(spec),
        "mcp",
        Some(node_id),
    )
    .await?;
    let Some(g) = g else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for b in spec.mcp.iter().take(32) {
        if let Some(s) = g
            .secrets
            .iter()
            .find(|s| s.env.eq_ignore_ascii_case(&b.secret))
        {
            out.push(pb::McpSecret {
                server: b.server.clone(),
                key: b.key.clone(),
                value: s.value.expose().into(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod job_kind_tests {
    use super::*;

    fn node(kinds: &[&str]) -> Vec<String> {
        kinds.iter().map(|k| k.to_string()).collect()
    }

    /// A node gets only the job kinds it lists (an older belai lists its
    /// forge logins but not `forge`), and one that lists none predates the list.
    #[test]
    fn jobs_go_only_to_nodes_that_run_the_kind() {
        let old = node(&["github", "model", "teleport", "forge_push"]);
        assert!(runs_kind(&old, "github"));
        assert!(!runs_kind(&old, "forge"));
        assert!(runs_kind(&node(&["github", "forge"]), "forge"));
        assert!(runs_kind(&node(&[]), "forge"));
    }
}
