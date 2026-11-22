//! `keleustes host …`: host management from the command line, for scripts
//! and the belai host e2e (`just e2e-host`). Actions go through the same
//! `kls_core::hosts::request_action` as the Host view, as the owner, with
//! the surface `cli`; the running app's action stream delivers them.
//! Output is JSON with the proto field names (protojson reads them).

use anyhow::{Result, bail};
use clap::Subcommand;
use kls_core::Core;
use kls_core::hosts::{self, ActionRequest, Origin};
use serde_json::json;
use std::time::Duration;

#[derive(Subcommand)]
pub enum HostCmd {
    /// The last inventory a node reported.
    Inventory { node: String },
    /// Request a host action and wait for it to settle.
    Act {
        node: String,
        kind: String,
        /// key=value (repeatable).
        #[arg(long = "param")]
        params: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "")]
        if_match: String,
        /// The confirmed preview (provision and shell kinds).
        #[arg(long, default_value = "")]
        preview: String,
        /// The host name, typed to confirm (provision and shell kinds).
        #[arg(long, default_value = "")]
        confirm: String,
        #[arg(long, default_value_t = 60)]
        wait: u64,
    },
    /// Stop the factory (kill switch).
    Stop {
        #[arg(long, default_value = "stopped from the command line")]
        reason: String,
    },
    /// Lift the kill switch.
    Resume,
    /// Verify the audit ledger; exit 1 when a chain is broken.
    Verify,
}

/// Resolves a node id or name.
async fn node_id(core: &Core, s: &str) -> Result<String> {
    let nodes = core.store().nodes().await?;
    match nodes.iter().find(|n| n.id == s || n.name == s) {
        Some(n) => Ok(n.id.clone()),
        None => bail!("unknown node {s:?}"),
    }
}

fn origin() -> Origin {
    // The CLI is the owner at the console; reads under a stopped factory
    // count as an explicit click (decision O3).
    Origin::owner("cli")
}

pub async fn run(core: &Core, cmd: &HostCmd) -> Result<i32> {
    match cmd {
        HostCmd::Inventory { node } => {
            let id = node_id(core, node).await?;
            match hosts::inventory(core, &id).await? {
                Some((_, inv)) => {
                    println!("{}", serde_json::to_string_pretty(&inv)?);
                    Ok(0)
                }
                None => {
                    eprintln!("no inventory yet");
                    Ok(1)
                }
            }
        }
        HostCmd::Act {
            node,
            kind,
            params,
            dry_run,
            if_match,
            preview,
            confirm,
            wait,
        } => {
            let id = node_id(core, node).await?;
            let mut req = ActionRequest::new(&id, kind, origin());
            for kv in params {
                let Some((k, v)) = kv.split_once('=') else {
                    bail!("--param wants key=value, got {kv:?}");
                };
                req = req.param(k, v);
            }
            req.dry_run = *dry_run;
            req.if_match = if_match.clone();
            req.preview_sha256 = preview.clone();
            req.confirm_name = confirm.clone();
            let row = match hosts::request_action(core, req).await {
                Ok(r) => r,
                Err(e) => {
                    let code = hosts::refusal_of(&e).map(|r| r.code()).unwrap_or("error");
                    println!(
                        "{}",
                        json!({ "action_id": "", "state": "refused", "error": e.to_string(), "code": code })
                    );
                    return Ok(2);
                }
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(*wait);
            loop {
                let r = core.store().host_action(&row.id).await?.unwrap_or_default();
                if r.settled()
                    || r.state == "needs_human"
                    || tokio::time::Instant::now() >= deadline
                {
                    let h: hosts::Handoff =
                        serde_json::from_str(&r.handoff_json).unwrap_or_default();
                    println!(
                        "{}",
                        json!({
                            "action_id": r.id, "seq": r.last_seq, "state": r.state,
                            "step": r.step_label, "pct": r.pct,
                            "preview_json": r.preview_json, "preview_sha256": r.preview_sha256,
                            "result_json": r.result_json, "error": r.error,
                            "handoff_command": h.command, "handoff_reason": h.reason,
                        })
                    );
                    return Ok(if r.state == "done" || r.state == "needs_human" {
                        0
                    } else {
                        1
                    });
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        HostCmd::Stop { reason } => {
            kls_core::control::stop(core, reason).await?;
            Ok(0)
        }
        HostCmd::Resume => {
            kls_core::control::resume(core).await?;
            Ok(0)
        }
        HostCmd::Verify => {
            core.audit_sync().await?;
            let r = core.audit_verify().await?;
            println!("{}", json!({ "ok": r.ok, "chains": r.chains.len() }));
            Ok(if r.ok { 0 } else { 1 })
        }
    }
}
