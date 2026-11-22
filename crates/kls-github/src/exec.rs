//! Spawning a program. Abstracted so route detection can be tested without
//! a forge CLI binary.

use crate::GhOutput;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[async_trait::async_trait]
pub trait Exec: Send + Sync {
    async fn exec(&self, program: &str, args: &[String], stdin: Option<&str>) -> Result<GhOutput>;
}

/// Runs a real process with a timeout. Every forge CLI gets a
/// non-interactive environment (no prompts, colour, pager or update check).
pub struct ProcessExec {
    pub timeout: Duration,
}

impl Default for ProcessExec {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

#[async_trait::async_trait]
impl Exec for ProcessExec {
    async fn exec(&self, program: &str, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args);
        for cli in [
            crate::CliKind::Gh,
            crate::CliKind::Glab,
            crate::CliKind::Tea,
        ] {
            for (k, v) in cli.env() {
                cmd.env(k, v);
            }
        }
        cmd.stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("run {program}"))?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input.as_bytes()).await?;
            drop(pipe);
        }
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .with_context(|| format!("{program} timed out after {:?}", self.timeout))??;
        Ok(GhOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit: out.status.code().unwrap_or(-1),
        })
    }
}
