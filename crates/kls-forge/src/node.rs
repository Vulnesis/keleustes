//! A forge CLI on a connected belai node.
//!
//! [`NodeGh`]: `gh` for github.com through a `github` job (payload
//! `{"args": [...], "stdin": "..."|null}`), for nodes that predate forge
//! logins. [`NodeCli`]: any CLI for any host through a `forge` job (payload
//! `{"cli", "host", "args", "stdin"}`, `JobNeeds.forge = (cli, host)`), for
//! nodes that advertise `Capabilities.forges`. Both answer
//! `{"stdout", "stderr", "exit"}` in `output_json`.

use crate::forge::CliKind;
use crate::{ForgeCli, GhOutput, Route};
use anyhow::{Result, bail};
use kls_core::{Core, JobNeeds, JobOutcome};

pub const JOB_KIND: &str = "github";
/// The job kind that runs gh, glab or tea for a host (forges.md WP-9).
pub const FORGE_JOB_KIND: &str = "forge";
pub const JOB_TIMEOUT_MS: i64 = 90_000;

fn output(out: JobOutcome, node: &str, kind: &str) -> Result<GhOutput> {
    if out.status != "ok" {
        bail!(
            "node {node}: {kind} job {}{}",
            out.status,
            if out.error.is_empty() {
                String::new()
            } else {
                format!(": {}", out.error)
            }
        );
    }
    serde_json::from_str(&out.output_json)
        .map_err(|e| anyhow::anyhow!("node {node}: bad {kind} job output: {e}"))
}

pub struct NodeGh {
    core: Core,
    node_id: String,
    node_name: String,
}

impl NodeGh {
    pub fn new(core: Core, node_id: &str, node_name: &str) -> Self {
        Self {
            core,
            node_id: node_id.into(),
            node_name: node_name.into(),
        }
    }
}

#[async_trait::async_trait]
impl ForgeCli for NodeGh {
    fn route(&self) -> Route {
        Route::Node {
            id: self.node_id.clone(),
            name: self.node_name.clone(),
        }
    }

    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        let payload = serde_json::json!({ "args": args, "stdin": stdin });
        let out = self
            .core
            .gateway()?
            .job(
                Some(&self.node_id),
                JOB_KIND,
                &payload.to_string(),
                JobNeeds {
                    gh: true,
                    ..Default::default()
                },
                JOB_TIMEOUT_MS,
            )
            .await?;
        output(out, &self.node_name, JOB_KIND)
    }
}

/// gh, glab or tea for one host on a node that advertises that login.
pub struct NodeCli {
    core: Core,
    node_id: String,
    node_name: String,
    cli: CliKind,
    host: String,
}

impl NodeCli {
    pub fn new(core: Core, node_id: &str, node_name: &str, cli: CliKind, host: &str) -> Self {
        Self {
            core,
            node_id: node_id.into(),
            node_name: node_name.into(),
            cli,
            host: host.into(),
        }
    }

    /// The `forge` job payload (belai forces `--hostname` / `-l` itself).
    pub fn payload(&self, args: &[String], stdin: Option<&str>) -> String {
        serde_json::json!({ "cli": self.cli.name(), "host": self.host, "args": args, "stdin": stdin })
            .to_string()
    }
}

#[async_trait::async_trait]
impl ForgeCli for NodeCli {
    fn route(&self) -> Route {
        Route::Node {
            id: self.node_id.clone(),
            name: self.node_name.clone(),
        }
    }

    fn cli(&self) -> CliKind {
        self.cli
    }

    async fn push_merge(&self, payload: &serde_json::Value) -> Result<Option<String>> {
        self.push(payload).await.map(Some)
    }

    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        let out = self
            .core
            .gateway()?
            .job(
                Some(&self.node_id),
                FORGE_JOB_KIND,
                &self.payload(args, stdin),
                JobNeeds {
                    gh: false,
                    provider: None,
                    forge: Some((self.cli.name().into(), self.host.clone())),
                },
                JOB_TIMEOUT_MS,
            )
            .await?;
        output(out, &self.node_name, FORGE_JOB_KIND)
    }
}

/// The job kind that builds a merge commit on the node and pushes it with
/// the node's CLI as git credential helper (forges.md 3.6).
pub const PUSH_JOB_KIND: &str = "forge_push";

impl NodeCli {
    /// Runs a `forge_push` job; the answer names the pushed commit.
    pub async fn push(&self, payload: &serde_json::Value) -> Result<String> {
        let mut p = payload.clone();
        p["cli"] = serde_json::json!(self.cli.name());
        p["host"] = serde_json::json!(self.host);
        let out = self
            .core
            .gateway()?
            .job(
                Some(&self.node_id),
                PUSH_JOB_KIND,
                &p.to_string(),
                JobNeeds {
                    gh: false,
                    provider: None,
                    forge: Some((self.cli.name().into(), self.host.clone())),
                },
                JOB_TIMEOUT_MS * 3,
            )
            .await?;
        if out.status != "ok" {
            bail!(
                "node {}: forge_push job {}{}",
                self.node_name,
                out.status,
                if out.error.is_empty() {
                    String::new()
                } else {
                    format!(": {}", out.error)
                }
            );
        }
        let v: serde_json::Value = serde_json::from_str(&out.output_json)
            .map_err(|e| anyhow::anyhow!("node {}: bad forge_push output: {e}", self.node_name))?;
        match v["commit"].as_str() {
            Some(sha) if !sha.is_empty() => Ok(sha.to_string()),
            _ => bail!(
                "node {}: {}",
                self.node_name,
                v["error"]
                    .as_str()
                    .unwrap_or("forge_push answered no commit")
            ),
        }
    }
}
