//! Scriptable session commands. They work against the data directory
//! beside a running app: frames queued here reach the node through the
//! app's outbox sweep, and the app's screens see the events through its
//! event tail.
//!
//! Human output (the default) is unchanged; `--json` prints the contract
//! (`crate::ax`): one envelope as the last stdout line.

use crate::ax::view::{self, Want};
use crate::ax::{self, AxError, Code, Ctx, Done, fail, q};
use anyhow::{Context, Result};
use clap::Subcommand;
use kls_core::{Core, DispatchSpec};
use kls_store::sessions::SessionQuery;
use serde_json::{Value, json};
use std::time::Duration;

// Clap parses one variant per run; boxing the dispatch flags buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum SessionsCmd {
    /// Sessions, newest first.
    List {
        #[arg(long)]
        node: Option<String>,
        #[arg(long)]
        open: bool,
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Start a session on a node (belai rc). Prints the dispatch id first.
    Dispatch {
        /// Node id or name.
        #[arg(long)]
        node: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        prompt: String,
        #[arg(long, default_value = "")]
        provider: String,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long, default_value = "")]
        agent: String,
        #[arg(long, default_value = "")]
        mode: String,
        /// Attach vault keys for these providers.
        #[arg(long = "vault")]
        vault: Vec<String>,
        /// Wait for the node to start (or refuse) it, up to this many seconds.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// A short human label for the session (shown in its provenance).
        #[arg(long, default_value = "")]
        label: String,
        /// Wait until: started, ask, done or ended (see `keleustes contract`).
        #[arg(long, value_enum)]
        wait_for: Option<Want>,
        /// One deadline over delivery, session start and the turn (500ms,
        /// 30s, 10m, 1h or seconds). Implies --wait-for done. Default 30m.
        #[arg(long)]
        timeout: Option<String>,
        /// Run a third-party harness the node advertises instead of belai.
        #[arg(long)]
        harness: Option<String>,
        /// A vault key id to hand to the harness (its key mode permitting).
        #[arg(long)]
        key: Option<String>,
        /// Resume this native harness session (a harness_session_id; --harness only).
        #[arg(long, default_value = "")]
        resume: String,
        /// Scripted answers for the questions the session asks (belai
        /// `-answers` JSON: [{"choose","note","skip","match"}]).
        #[arg(long)]
        answers_file: Option<std::path::PathBuf>,
        /// A harness gate, key=value (finish_review=on, lint_loop=on,
        /// checkpoints=on, code_mode=off, commit_mode=off|ask|auto).
        #[arg(long = "gate")]
        gate: Vec<String>,
        /// Plan first: the session runs in plan mode and returns its plan file.
        #[arg(long)]
        plan_first: bool,
    },
    /// Continue an ended belai session on its node in a new session, or fork
    /// it at an entry (the belai entry id). Prints the dispatch id first.
    Resume {
        session: String,
        /// Fork at this entry (Entry.meta_json "id").
        #[arg(long, default_value = "")]
        fork: String,
        #[arg(long, default_value = "")]
        prompt: String,
        /// Wait for the node to start (or refuse) it, up to this many seconds.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Carry out the plan file the session wrote (agent mode).
        #[arg(long)]
        execute_plan: bool,
    },
    /// Start a one-off worker made from a prompt (no stored profile).
    Worker {
        #[arg(long)]
        node: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        prompt: String,
        /// Tool names, comma separated (default: all).
        #[arg(long, default_value = "")]
        tools: String,
        /// LIST[:LABEL,LABEL].
        #[arg(long, default_value = "")]
        claim: String,
        #[arg(long, default_value = "")]
        to: String,
        /// none, draft_pr or agent.
        #[arg(long, default_value = "")]
        publish: String,
        #[arg(long)]
        read_only: bool,
    },
    /// Wait until a session (or dispatch) needs you or ends.
    Wait {
        /// Session id, or the dispatch id from the handle line.
        session: String,
        /// started, ask (an ask, a finished turn or the end), done or ended.
        #[arg(long = "for", value_enum, default_value = "ask")]
        want: Want,
        /// 500ms, 30s, 10m, 1h or seconds.
        #[arg(long, default_value = "10m")]
        timeout: String,
    },
    /// Install library items on a node, or back a node's items up
    /// (`kind:name`, e.g. `profile:patcher`, `skill:fmt`).
    Library {
        #[arg(long)]
        node: String,
        #[arg(long = "install")]
        install: Vec<String>,
        #[arg(long = "backup")]
        backup: Vec<String>,
        #[arg(long)]
        overwrite: bool,
    },
    /// The harnesses a node advertises.
    Harnesses { node: String },
    /// Print a session's transcript.
    Transcript {
        session: String,
        /// Keep printing new entries until the session ends.
        #[arg(long)]
        follow: bool,
        /// Only entries with an index above this one.
        #[arg(long)]
        since: Option<i64>,
    },
    /// Queue a prompt for a live session (resumes an idle one).
    Prompt { session: String, text: String },
    /// Move a session to another node (the origin uploads a bundle; run
    /// `belai -teleport <id>` on the target to continue it).
    Teleport {
        session: String,
        /// Target node id or name.
        #[arg(long)]
        to: String,
        /// Seconds to wait for the bundle to be uploaded.
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
    /// A session's state, open ask, what it accepts and its usage (JSON).
    Show { session: String },
    /// Answer the session's open ask: an option id, or --payload for clarify.
    Answer {
        session: String,
        ask: String,
        #[arg(default_value = "")]
        decision: String,
        #[arg(long, default_value = "")]
        payload: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Cross-host requests nodes made (newest first).
    Requests {
        #[arg(long, default_value = "")]
        state: String,
    },
    /// Approve a cross-host request that waits for approval.
    Approve { request: String },
    /// Decline a cross-host request that waits for approval.
    Decline { request: String },
    /// Queue a session control (stop, interrupt, mode …).
    Command {
        session: String,
        name: String,
        args: Vec<String>,
    },
    /// Show the harness key-mode policy (JSON), or set it with --set (audited).
    HarnessKeys {
        #[arg(long)]
        set: Option<String>,
    },
    /// Turn the A2A blackout on or off.
    A2aBlackout {
        /// "on" or "off".
        state: String,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Send an A2A message as the owner (keleustes:owner).
    A2aSend { to: String, text: String },
    /// Store a vault key read from standard input; prints its id.
    VaultKey {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        env: String,
        #[arg(long, default_value = "")]
        label: String,
    },
}

impl SessionsCmd {
    /// The envelope kind.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionsCmd::List { .. } => "sessions.list",
            SessionsCmd::Dispatch { .. } => "sessions.dispatch",
            SessionsCmd::Resume { .. } => "sessions.resume",
            SessionsCmd::Worker { .. } => "sessions.worker",
            SessionsCmd::Wait { .. } => "sessions.wait",
            SessionsCmd::Library { .. } => "sessions.library",
            SessionsCmd::Harnesses { .. } => "sessions.harnesses",
            SessionsCmd::Transcript { .. } => "sessions.transcript",
            SessionsCmd::Prompt { .. } => "sessions.prompt",
            SessionsCmd::Teleport { .. } => "sessions.teleport",
            SessionsCmd::Show { .. } => "sessions.show",
            SessionsCmd::Answer { .. } => "sessions.answer",
            SessionsCmd::Requests { .. } => "sessions.requests",
            SessionsCmd::Approve { .. } => "sessions.approve",
            SessionsCmd::Decline { .. } => "sessions.decline",
            SessionsCmd::Command { .. } => "sessions.command",
            SessionsCmd::HarnessKeys { .. } => "sessions.harness-keys",
            SessionsCmd::A2aBlackout { .. } => "sessions.a2a-blackout",
            SessionsCmd::A2aSend { .. } => "sessions.a2a-send",
            SessionsCmd::VaultKey { .. } => "sessions.vault-key",
        }
    }

    /// Whether it changes state (audited whatever the outcome).
    pub fn mutating(&self) -> bool {
        match self {
            SessionsCmd::List { .. }
            | SessionsCmd::Wait { .. }
            | SessionsCmd::Harnesses { .. }
            | SessionsCmd::Transcript { .. }
            | SessionsCmd::Show { .. }
            | SessionsCmd::Requests { .. } => false,
            SessionsCmd::HarnessKeys { set } => set.is_some(),
            _ => true,
        }
    }
}

/// Waits for the background test of key `id` to settle; its outcome
/// ("" when it never finished: the key reads untested).
async fn settle_key_test(core: &Core, id: &str) -> String {
    use kls_core::vault::probe::{self, ProbeState};
    let deadline = std::time::Instant::now() + probe::TIMEOUT * 2 + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(m)) = core.store().vault_secret_meta(id).await {
            match probe::effective(&m, kls_store::now_ms()).state {
                ProbeState::Untested | ProbeState::Testing => {}
                s => return s.as_str().to_string(),
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    String::new()
}

/// A node by id or name; revoked nodes do not count.
pub async fn node_id(core: &Core, id_or_name: &str) -> Result<String> {
    let nodes = core.store().nodes().await?;
    let hit: Vec<_> = nodes
        .iter()
        .filter(|n| n.revoked_at.is_none() && (n.id == id_or_name || n.name == id_or_name))
        .collect();
    match hit.as_slice() {
        [one] => Ok(one.id.clone()),
        [] if nodes
            .iter()
            .any(|n| n.id == id_or_name || n.name == id_or_name) =>
        {
            Err(fail(
                Code::NodeUnavailable,
                format!("node {id_or_name:?} is revoked"),
            ))
        }
        [] => Err(AxError::new(
            Code::NotFound,
            format!("no active node named {id_or_name:?}"),
        )
        .next(vec!["keleustes nodes list".into()])
        .into()),
        _ => Err(AxError::new(
            Code::Ambiguous,
            format!("{id_or_name:?} names several nodes; use the id"),
        )
        .next(vec!["keleustes nodes list".into()])
        .into()),
    }
}

fn timeout_of(s: &str) -> Result<Duration> {
    ax::parse_duration(s).map_err(|e| fail(Code::UsageError, e))
}

fn entry_json(session: &str, e: &kls_store::sessions::Entry) -> Value {
    json!({ "v": ax::V, "kind": "entry", "sessionId": session, "idx": e.idx, "role": e.role,
            "text": e.text, "at": e.at })
}

/// The owner at the command line, as provenance.
fn cli_origin() -> kls_core::hosts::Origin {
    kls_core::hosts::Origin {
        actor: "owner".into(),
        surface: "cli".into(),
        ..Default::default()
    }
}

/// A kill-switch check for an outbound frame the CLI is about to queue.
async fn gate(core: &Core, action: &str, target: &str) -> Result<()> {
    kls_core::control::check(core, action, target, "owner:cli").await
}

/// A session view as the envelope's `session`, with its next[].
async fn with_view(core: &Core, id: &str, data: Value) -> Result<Done> {
    let snap = view::snapshot(core, id).await?;
    Ok(Done::data(data).with_session(snap.view))
}

pub async fn run(core: &Core, hub: &kls_node::Hub, cmd: &SessionsCmd, ctx: &Ctx) -> Result<Done> {
    match cmd {
        SessionsCmd::List { node, open, limit } => {
            let node_id = match node {
                Some(n) => Some(node_id(core, n).await?),
                None => None,
            };
            let rows = core
                .store()
                .sessions(&SessionQuery {
                    node_id,
                    open_only: *open,
                    limit: *limit,
                    ..Default::default()
                })
                .await?;
            let mut out = Vec::new();
            for s in rows {
                ctx.out(format!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    s.id,
                    s.state,
                    s.node_id,
                    s.model,
                    s.cwd,
                    if s.title.is_empty() { "-" } else { &s.title }
                ));
                if ctx.json {
                    out.push(view::snapshot(core, &s.id).await?.view);
                }
            }
            Ok(Done::data(json!({ "sessions": out })))
        }
        SessionsCmd::Dispatch {
            node,
            cwd,
            prompt,
            provider,
            model,
            agent,
            mode,
            vault,
            wait,
            label,
            wait_for,
            timeout,
            harness,
            key,
            resume,
            answers_file,
            gate: gates,
            plan_first,
        } => {
            // Parse before anything is queued.
            let mut fields = kls_core::sessions::start::StartFields::default();
            if let Some(p) = answers_file {
                fields.answers_json = std::fs::read_to_string(p)
                    .map_err(|e| fail(Code::UsageError, format!("{}: {e}", p.display())))?
                    .trim()
                    .to_string();
            }
            for g in gates {
                let (k, v) = g
                    .split_once('=')
                    .ok_or_else(|| fail(Code::UsageError, format!("{g}: use key=value")))?;
                fields.gates.insert(k.trim().into(), v.trim().into());
            }
            let mode = &if *plan_first {
                "plan".to_string()
            } else {
                mode.clone()
            };
            let limit = match timeout {
                Some(t) => Some(timeout_of(t)?),
                None => None,
            };
            let node = node_id(core, node).await?;
            let n = core.store().node(&node).await?.context("node vanished")?;
            if harness.is_none() && !n.host.capabilities.takes_dispatch("session") {
                return Err(fail(
                    Code::NodeUnavailable,
                    format!(
                        "node {} does not take session dispatches (update belai on the host)",
                        n.name
                    ),
                ));
            }
            // The kill switch first: refused, evented and audited.
            gate(core, "dispatch", &node).await?;
            let mut warnings = Vec::new();
            let id = if let Some(h) = harness {
                if !label.is_empty() {
                    warnings.push("--label is not carried by harness dispatches".to_string());
                }
                let req = kls_core::harness::HarnessRequest {
                    node_id: node.clone(),
                    harness: h.clone(),
                    cwd: cwd.clone(),
                    prompt: prompt.clone(),
                    mode: mode.clone(),
                    model: model.clone(),
                    key_id: key.clone(),
                    resume: resume.clone(),
                    actor: "owner".into(),
                    ..Default::default()
                };
                let keys = kls_core::vault::list_secrets(core)
                    .await
                    .unwrap_or_default();
                core.dispatch_harness(&req, &n.host.harnesses, &keys, hub)
                    .await?
            } else {
                let spec = DispatchSpec {
                    kind: "session".into(),
                    cwd: cwd.clone(),
                    prompt: prompt.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    agent: agent.clone(),
                    mode: mode.clone(),
                    vault_providers: vault.clone(),
                    // Provenance: the owner at the console, with the label.
                    trigger: Some(kls_core::gateway::Trigger {
                        source: "console".into(),
                        subject: label.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                if fields.is_empty() {
                    core.gateway()?.dispatch(&node, spec).await?
                } else {
                    // Answers and gates are checked, and audited by name and
                    // hash, by the start path.
                    kls_core::sessions::start::dispatch(core, &node, spec, &fields, &cli_origin())
                        .await?
                }
            };
            // The handle: addressable before any agent work.
            ctx.out(&id);
            ctx.stream(json!({ "v": ax::V, "kind": "handle", "dispatchId": id, "nodeId": node, "label": label }));
            let data = json!({ "dispatchId": id, "nodeId": node, "label": label });
            let want = match (wait_for, limit, *wait) {
                (Some(w), _, _) => Some(*w),
                (None, Some(_), _) => Some(Want::Done),
                (None, None, 0) => None,
                (None, None, _) => Some(Want::Started),
            };
            let Some(want) = want else {
                let mut d = with_view(core, &id, data).await?;
                d.warnings = warnings;
                return Ok(d);
            };
            let budget = limit.unwrap_or(if *wait > 0 {
                Duration::from_secs(*wait)
            } else {
                Duration::from_secs(30 * 60)
            });
            let retry = format!(
                "keleustes sessions wait {id} --for {} --timeout {}s",
                want.name(),
                budget.as_secs().max(1)
            );
            let deadline = tokio::time::Instant::now() + budget;
            let snap = view::wait(core, &id, want, deadline, &retry).await?;
            if wait_for.is_none() && limit.is_none() {
                ctx.out(format!("session {}", snap.session_id));
            } else {
                ctx.out(format!("session {}\t{}", snap.session_id, snap.agent));
            }
            let mut d = Done::data(data).with_session(snap.view);
            d.warnings = warnings;
            Ok(d)
        }
        SessionsCmd::Resume {
            session,
            fork,
            prompt,
            wait,
            execute_plan,
        } => {
            let session = &resolve(core, session).await?;
            let id = if *execute_plan {
                if !fork.is_empty() || !prompt.is_empty() {
                    return Err(fail(
                        Code::UsageError,
                        "--execute-plan takes no --fork or --prompt",
                    ));
                }
                kls_core::sessions::resume::execute_plan(core, session, &cli_origin()).await?
            } else {
                kls_core::sessions::resume::resume(
                    core,
                    kls_core::sessions::resume::Resume {
                        session_id: session.clone(),
                        fork_entry: fork.clone(),
                        prompt: prompt.clone(),
                        ..Default::default()
                    },
                    &cli_origin(),
                )
                .await?
            };
            ctx.out(&id);
            ctx.stream(
                json!({ "v": ax::V, "kind": "handle", "dispatchId": id, "resumeOf": session }),
            );
            let data = json!({ "dispatchId": id, "resumeOf": session, "forkEntry": fork });
            if *wait == 0 {
                return with_view(core, &id, data).await;
            }
            let retry = format!("keleustes sessions wait {id} --for started --timeout {wait}s");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(*wait);
            let snap = view::wait(core, &id, Want::Started, deadline, &retry).await?;
            ctx.out(format!("session {}", snap.session_id));
            Ok(Done::data(data).with_session(snap.view))
        }
        SessionsCmd::Worker {
            node,
            cwd,
            prompt,
            tools,
            claim,
            to,
            publish,
            read_only,
        } => {
            let node = node_id(core, node).await?;
            let split = |s: &str| -> Vec<String> {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            };
            let (list, labels) = claim.split_once(':').unwrap_or((claim.as_str(), ""));
            let fields = kls_core::sessions::start::StartFields {
                adhoc: Some(kls_core::gateway::AdhocWorker {
                    prompt: prompt.clone(),
                    tools: split(tools),
                    claim_lists: split(list),
                    claim_labels: split(labels),
                    to: to.clone(),
                    publish: publish.clone(),
                    read_only: *read_only,
                }),
                ..Default::default()
            };
            let spec = DispatchSpec {
                kind: "worker".into(),
                cwd: cwd.clone(),
                ..Default::default()
            };
            let id = kls_core::sessions::start::dispatch(core, &node, spec, &fields, &cli_origin())
                .await?;
            ctx.out(&id);
            Ok(Done::data(json!({ "dispatchId": id, "nodeId": node })))
        }
        SessionsCmd::Wait {
            session,
            want,
            timeout,
        } => {
            let t = timeout_of(timeout)?;
            let retry = format!(
                "keleustes sessions wait {session} --for {} --timeout {}",
                want.name(),
                q(timeout)
            );
            let snap = view::wait(
                core,
                session,
                *want,
                tokio::time::Instant::now() + t,
                &retry,
            )
            .await?;
            ctx.out(format!(
                "{}\t{}\t{}\t{}",
                snap.session_id,
                snap.agent,
                snap.delivery,
                snap.view["verified"].as_str().unwrap_or("")
            ));
            Ok(
                Done::data(json!({ "for": want.name(), "satisfied": true }))
                    .with_session(snap.view),
            )
        }
        SessionsCmd::Transcript {
            session,
            follow,
            since,
        } => {
            let snap = view::snapshot(core, session).await?;
            let session = snap.session_id.clone();
            if session.is_empty() {
                return Err(
                    AxError::new(Code::InvalidState, "the dispatch has no session yet")
                        .session(snap.view.clone())
                        .next(view::next_for(&snap.view))
                        .into(),
                );
            }
            let mut from = since.map(|s| s + 1).unwrap_or(0).max(0);
            let mut count = 0usize;
            let mut last: Option<i64> = None;
            let print = |e: &kls_store::sessions::Entry| {
                ctx.out(format!(
                    "{}\t{}\t{}",
                    e.idx,
                    e.role,
                    e.text.replace('\n', "\n\t\t")
                ));
                ctx.stream(entry_json(&session, e));
            };
            loop {
                for e in core.store().entries(&session, from, 500).await? {
                    from = e.idx + 1;
                    count += 1;
                    last = Some(e.idx);
                    print(&e);
                }
                if !follow {
                    break;
                }
                let s = core.store().session(&session).await?;
                if s.is_none_or(|s| s.ended_at.is_some()) {
                    // One last read for entries that raced the end.
                    for e in core.store().entries(&session, from, 500).await? {
                        count += 1;
                        last = Some(e.idx);
                        print(&e);
                    }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let mut d =
                with_view(core, &session, json!({ "count": count, "lastIdx": last })).await?;
            if let Some(l) = last {
                d.next.insert(
                    0,
                    format!("keleustes sessions transcript {session} --since {l}"),
                );
                d.next.dedup();
            }
            Ok(d)
        }
        SessionsCmd::Teleport { session, to, wait } => {
            let s = core
                .store()
                .session(session)
                .await?
                .ok_or_else(|| fail(Code::NotFound, format!("no session {session}")))?;
            let to = node_id(core, to).await?;
            let t = core.teleport_request(session, &s.node_id, &to).await?;
            ctx.out(&t.id);
            // The upload job runs from this process: stay until it settles.
            let deadline = tokio::time::Instant::now() + Duration::from_secs((*wait).max(1));
            loop {
                let cur = core
                    .store()
                    .teleport(&t.id)
                    .await?
                    .context("teleport vanished")?;
                match cur.status.as_str() {
                    "ready" | "delivered" => {
                        eprintln!(
                            "ready: {} bytes; on the target run: belai -teleport {}",
                            cur.size, cur.id
                        );
                        let mut d = Done::data(
                            json!({ "teleportId": cur.id, "status": cur.status, "size": cur.size }),
                        );
                        d.next = vec![format!("belai -teleport {}", cur.id)];
                        return Ok(d);
                    }
                    "failed" | "expired" => {
                        return Err(fail(
                            Code::DispatchFailed,
                            format!("teleport {}: {}", cur.status, cur.error),
                        ));
                    }
                    _ => {}
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(fail(
                        Code::Timeout,
                        format!("teleport still {} after {wait}s", cur.status),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        SessionsCmd::Show { session } => {
            let snap = view::snapshot(core, session).await?;
            if snap.session_id.is_empty() {
                // A dispatch the node has not started yet.
                ctx.out(&snap.view);
                return Ok(Done::data(Value::Null).with_session(snap.view));
            }
            let session = &snap.session_id;
            let s = core
                .store()
                .session(session)
                .await?
                .ok_or_else(|| fail(Code::NotFound, format!("no session {session}")))?;
            let x = core
                .store()
                .session_extras(session)
                .await?
                .unwrap_or_default();
            // A harness session takes stop, interrupt and its own keys (the
            // node's advert), not belai's controls.
            let mut harness_keys: Vec<String> = Vec::new();
            if let Some(h) = x.harness.as_ref()
                && let Some(n) = core.store().node(&s.node_id).await?
                && let Some(a) = n.host.harnesses.iter().find(|a| a.id == h.harness)
            {
                harness_keys = a.allow_keys.clone();
            }
            let out = serde_json::json!({
                "id": s.id, "title": s.title, "state": s.state, "nodeId": s.node_id,
                "endReason": s.end_reason, "checkpoints": x.checkpoints,
                "planSha256": x.plan_sha256,
                "relayJson": x.pending_ask.as_ref().map(|a| a.relay_json.as_str()).unwrap_or(""),
                "inputTokens": s.input_tokens, "outputTokens": s.output_tokens, "costUsd": s.cost_usd,
                "pendingAsk": x.pending_ask, "accepts": x.accepts, "git": x.git, "usage": x.usage,
                "turns": x.turns, "toolCalls": x.tool_calls, "harness": x.harness,
                "keys": harness_keys,
                "delivery": snap.view["delivery"], "agent": snap.view["agent"],
                "verified": snap.view["verified"],
            });
            ctx.out(&out);
            Ok(Done::data(out).with_session(snap.view))
        }
        SessionsCmd::Answer {
            session,
            ask,
            decision,
            payload,
            note,
        } => {
            let session = &resolve(core, session).await?;
            let item = match core
                .queue_ask_answer(session, ask, decision, payload, note)
                .await
            {
                Ok(i) => i,
                Err(e) => {
                    let mut ax = ax::classify(&e);
                    // Under stop only a refusal goes through (kls-core's
                    // own refusal for that case).
                    if e.to_string().starts_with("the factory is stopped") {
                        ax.code = Code::RefusedStopped;
                    }
                    if let Ok(snap) = view::snapshot(core, session).await {
                        ax.next = view::next_for(&snap.view);
                        ax.session = Some(snap.view);
                    }
                    return Err(ax.into());
                }
            };
            ctx.out(&item.id);
            with_view(core, session, json!({ "itemId": item.id })).await
        }
        SessionsCmd::A2aBlackout { state, reason } => {
            let on = match state.as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(fail(Code::UsageError, "say on or off")),
            };
            core.set_a2a_blackout(on, reason).await?;
            Ok(Done::data(json!({ "a2aBlackout": on })))
        }
        SessionsCmd::A2aSend { to, text } => {
            let st = kls_core::control::state(core).await?;
            let s = match core.send_owner_a2a(to, text, "").await {
                Ok(s) => s,
                Err(e) if st.a2a_blackout || !st.mode.running() => {
                    return Err(fail(Code::RefusedA2a, e.to_string()));
                }
                Err(e) => return Err(e),
            };
            if !s.delivered {
                return Err(fail(Code::RefusedA2a, format!("refused: {}", s.refused)));
            }
            let id = s.message.map(|m| m.id).unwrap_or_default();
            ctx.out(&id);
            Ok(Done::data(json!({ "messageId": id })))
        }
        SessionsCmd::HarnessKeys { set } => {
            if let Some(j) = set {
                let p: kls_core::harness::KeyPolicy = serde_json::from_str(j)
                    .map_err(|e| fail(Code::UsageError, format!("--set: {e}")))?;
                core.set_harness_key_policy(p, "owner").await?;
            }
            let policy = serde_json::to_value(core.harness_key_policy().await?)?;
            ctx.out(&policy);
            Ok(Done::data(policy))
        }
        SessionsCmd::VaultKey {
            provider,
            env,
            label,
        } => {
            let mut v = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut v)?;
            let m = kls_core::vault::put_secret(
                core,
                provider,
                env,
                label,
                kls_core::vault::SecretString::new(v.trim_end().to_string()),
            )
            .await?;
            ctx.out(&m.id);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            // The key is saved. Its test runs on this process's runtime:
            // stay until it settles (the test's own timeout bounds it), or
            // it dies with the process and the key reads untested forever.
            let test = if kls_core::vault::probe::auto_test(core).await {
                settle_key_test(core, &m.id).await
            } else {
                String::new()
            };
            Ok(Done::data(json!({ "keyId": m.id, "test": test })))
        }
        SessionsCmd::Library {
            node,
            install,
            backup,
            overwrite,
        } => {
            let node = node_id(core, node).await?;
            let refs = |v: &Vec<String>| -> Result<Vec<kls_core::gateway::LibraryRef>> {
                v.iter()
                    .map(|s| {
                        let (kind, name) = s
                            .split_once(':')
                            .ok_or_else(|| fail(Code::UsageError, format!("{s}: use kind:name")))?;
                        Ok(kls_core::gateway::LibraryRef {
                            kind: kind.into(),
                            name: name.into(),
                            ..Default::default()
                        })
                    })
                    .collect()
            };
            let lists = [
                ("library_install", refs(install)?),
                ("library_backup", refs(backup)?),
            ];
            if lists.iter().any(|(_, l)| !l.is_empty()) {
                gate(core, "dispatch", &node).await?;
            }
            let mut out = serde_json::Map::new();
            for (kind, list) in lists {
                if list.is_empty() {
                    continue;
                }
                let id = core
                    .gateway()?
                    .dispatch(
                        &node,
                        DispatchSpec {
                            kind: kind.into(),
                            library: list,
                            overwrite: *overwrite,
                            ..Default::default()
                        },
                    )
                    .await?;
                ctx.out(format!("{kind}\t{id}"));
                out.insert(kind.into(), json!(id));
            }
            Ok(Done::data(Value::Object(out)))
        }
        SessionsCmd::Harnesses { node } => {
            let id = node_id(core, node).await?;
            let n = core.store().node(&id).await?.context("node vanished")?;
            let mut out = Vec::new();
            for h in n.host.harnesses {
                ctx.out(format!(
                    "{}\t{}\t{}\t{}\t{}",
                    h.id,
                    h.name,
                    h.version,
                    h.state,
                    h.modes.join(",")
                ));
                out.push(json!({ "id": h.id, "name": h.name, "version": h.version,
                                 "state": h.state, "modes": h.modes }));
            }
            Ok(Done::data(json!({ "harnesses": out })))
        }
        SessionsCmd::Requests { state } => {
            let mut out = Vec::new();
            for r in core.store().node_requests(state, 100).await? {
                ctx.out(format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    r.id, r.state, r.kind, r.origin_node, r.target_node, r.initiated_by, r.reason
                ));
                let next = if r.state == "awaiting_approval" {
                    vec![
                        format!("keleustes sessions approve {}", r.id),
                        format!("keleustes sessions decline {}", r.id),
                    ]
                } else {
                    vec![]
                };
                out.push(json!({ "id": r.id, "state": r.state, "kind": r.kind,
                                 "originNode": r.origin_node, "targetNode": r.target_node,
                                 "initiatedBy": r.initiated_by, "reason": r.reason,
                                 "dispatchId": r.dispatch_id, "next": next }));
            }
            Ok(Done::data(json!({ "requests": out })))
        }
        SessionsCmd::Approve { request } | SessionsCmd::Decline { request } => {
            let approve = matches!(cmd, SessionsCmd::Approve { .. });
            let t = core.fleet_decide(request, approve).await.map_err(|e| {
                // fleet_decide reports these two in prose; both are the
                // caller's view of the request being stale.
                let m = e.to_string();
                if m == "unknown request" {
                    fail(Code::NotFound, m)
                } else if m.starts_with("the request is ") {
                    fail(Code::InvalidState, m)
                } else {
                    e
                }
            })?;
            ctx.out(format!("{}\t{}\t{}", t.request_id, t.state, t.dispatch_id));
            if !t.reason.is_empty() {
                eprintln!("{}", t.reason);
            }
            if t.state == "refused" && approve {
                return Err(fail(Code::RefusedPolicy, format!("refused: {}", t.reason)));
            }
            let mut d = Done::data(json!({ "requestId": t.request_id, "state": t.state,
                                           "dispatchId": t.dispatch_id, "reason": t.reason }));
            if !t.dispatch_id.is_empty() {
                d.next = vec![format!(
                    "keleustes sessions wait {} --for ask --timeout 10m",
                    t.dispatch_id
                )];
            }
            Ok(d)
        }
        SessionsCmd::Prompt { session, text } => {
            let session = &resolve(core, session).await?;
            // Core checks the kill switch (and audits a refusal).
            let item = core.queue_prompt_from(session, text, "owner:cli").await?;
            ctx.out(&item.id);
            let mut d = with_view(core, session, json!({ "itemId": item.id })).await?;
            d.next = vec![format!(
                "keleustes sessions wait {session} --for ask --timeout 10m"
            )];
            Ok(d)
        }
        SessionsCmd::Command {
            session,
            name,
            args,
        } => {
            let session = &resolve(core, session).await?;
            // Core checks the kill switch for all but wind-down commands.
            let item = core
                .queue_command_from(session, name, args, "owner:cli")
                .await?;
            ctx.out(&item.id);
            with_view(core, session, json!({ "itemId": item.id })).await
        }
    }
}

/// A session id from a session or dispatch id.
async fn resolve(core: &Core, id: &str) -> Result<String> {
    let snap = view::snapshot(core, id).await?;
    if snap.session_id.is_empty() {
        return Err(
            AxError::new(Code::InvalidState, "the dispatch has no session yet")
                .next(view::next_for(&snap.view))
                .session(snap.view)
                .into(),
        );
    }
    Ok(snap.session_id)
}
