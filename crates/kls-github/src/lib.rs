//! Keleustes forge plane (GitHub, GitHub Enterprise, GitLab, Gitea, Forgejo).
//!
//! Keleustes holds no forge token. Every call runs the forge's own CLI
//! (`gh`, `glab`, `tea`): on this host when the CLI is signed in to the
//! connection's host ([`LocalCli`](forge::cli::LocalCli), [`LocalGh`]), otherwise
//! on a connected belai node that reports a login ([`NodeGh`], a `github`
//! job through the [`NodeGateway`](kls_core::NodeGateway); github.com only
//! until the node protocol carries forge logins). [`Selector`] picks the
//! route per [`Connection`](forge::Connection) and says which one is active.
//!
//! On top of the [`ForgeCli`] backend, [`Client`] speaks the REST API
//! through `<cli> api -i` with ETags, counts calls and keeps the rate budget
//! ([`budget`]); [`forge::api::ForgeApi`] translates GitLab and Gitea into
//! the GitHub shapes the store holds; [`sync`] mirrors orgs, repos, pulls,
//! checks, reviews, issues and labels; [`poller`] runs sync on a timer
//! within the budget (there are no webhooks) and synthesises push, CI,
//! release and alert events ([`events`]); [`conflicts`] loads the three
//! versions of a pull request's conflicting files and commits a resolution
//! (GitHub's git data API, or a merge commit pushed over git elsewhere).
//!
//! Every write is audited ([`forge::guard`]); factory writes and polls are
//! refused while the factory is stopped.

pub mod budget;
pub mod client;
pub mod conflicts;
pub mod delivery;
pub mod events;
pub mod exec;
pub mod fake;
pub mod forge;
pub mod http;
pub mod kanban;
pub mod labels;
pub mod local;
pub mod login;
pub mod merge3;
pub mod node;
pub mod parse;
pub mod poller;
pub mod profile;
pub mod select;
pub mod service;
pub mod sync;

pub use budget::{Budget, CallClass, Refusal};
pub use client::{Client, Paged, Response};
pub use forge::{CliKind, Connection, Dialect, ForgeKind, valid_key};
pub use local::{LocalAuth, LocalGh};
pub use node::NodeGh;
pub use select::{RouteStatus, Selector};
pub use service::{Service, service, start};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// What one CLI invocation printed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GhOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

/// [`GhOutput`] under its forge-neutral name.
pub type CliOutput = GhOutput;
/// Which way GitHub is reached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    Local,
    Node { id: String, name: String },
    Unavailable { why: String },
}

impl Route {
    /// "via local gh", "via node X", "unavailable: why".
    pub fn label(&self) -> String {
        self.label_for(CliKind::Gh)
    }

    /// "via local glab", … for another CLI.
    pub fn label_for(&self, cli: CliKind) -> String {
        match self {
            Route::Local => format!("via local {}", cli.name()),
            Route::Node { name, id } => {
                format!("via node {}", if name.is_empty() { id } else { name })
            }
            Route::Unavailable { why } => format!("unavailable: {why}"),
        }
    }

    /// Key the budget and the ETag cache are kept under.
    pub fn key(&self) -> String {
        match self {
            Route::Local => "local".into(),
            Route::Node { id, .. } => format!("node:{id}"),
            Route::Unavailable { .. } => "none".into(),
        }
    }

    pub fn available(&self) -> bool {
        !matches!(self, Route::Unavailable { .. })
    }
}

/// Errors callers may want to tell apart (downcast from `anyhow::Error`).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GhError {
    #[error("GitHub is unavailable: {0}")]
    Unavailable(String),
    #[error("GitHub rate limit: wait until {until}")]
    RateLimited { until: i64 },
    #[error("the factory's share of the GitHub budget is used up this hour")]
    ShareExhausted,
    #[error("GitHub answered {status}: {message}")]
    Http { status: u16, message: String },
    /// The factory kill switch is on: automation may not act.
    #[error("the factory is stopped: {0}")]
    Stopped(String),
    /// The forge cannot do this (`forge_capability_unavailable`).
    #[error("forge_capability_unavailable: {0}")]
    Unsupported(String),
}

/// A way to run a forge CLI for one connection: here or on a node.
/// [`LocalGh`], [`forge::cli::LocalCli`], [`NodeGh`] and the fake implement it.
#[async_trait::async_trait]
pub trait ForgeCli: Send + Sync {
    fn route(&self) -> Route;
    /// Which CLI this runs (`gh` unless said otherwise).
    fn cli(&self) -> CliKind {
        CliKind::Gh
    }
    /// Runs `<cli> <args>` with optional standard input.
    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput>;
    /// Builds and pushes a merge commit where the CLI runs (a `forge_push`
    /// job on a node; forges.md 3.6). `None`: this backend cannot (local CLIs
    /// push with git here instead).
    async fn push_merge(&self, _payload: &serde_json::Value) -> Result<Option<String>> {
        Ok(None)
    }
}

/// The old name of [`ForgeCli`].
pub use ForgeCli as GitHub;

/// `["a", "b"]` as owned args.
pub fn args<I, S>(it: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    it.into_iter().map(Into::into).collect()
}

/// Validates `owner/name`.
pub fn valid_repo(full: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && s != "."
            && s != ".."
    };
    matches!(full.split_once('/'), Some((o, n)) if ok(o) && ok(n))
}

/// Now (Unix ms) as text, for cursors.
pub(crate) fn now_ms_str() -> String {
    kls_store::now_ms().to_string()
}

#[cfg(test)]
mod tests;
