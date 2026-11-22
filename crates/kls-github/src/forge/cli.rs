//! A forge CLI on this host for one connection.

use super::kind::{CliKind, Connection};
use crate::exec::Exec;
use crate::{ForgeCli, GhOutput, Route};
use anyhow::Result;
use std::sync::Arc;

/// `gh`, `glab` or `tea` on the Keleustes host.
pub struct LocalCli {
    exec: Arc<dyn Exec>,
    cli: CliKind,
    program: String,
    conn_id: String,
}

impl LocalCli {
    pub fn new(exec: Arc<dyn Exec>, conn: &Connection) -> Self {
        Self {
            exec,
            cli: conn.cli(),
            program: conn.cli().program(),
            conn_id: conn.id.clone(),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn connection(&self) -> &str {
        &self.conn_id
    }
}

#[async_trait::async_trait]
impl ForgeCli for LocalCli {
    fn route(&self) -> Route {
        Route::Local
    }

    fn cli(&self) -> CliKind {
        self.cli
    }

    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        self.exec.exec(&self.program, args, stdin).await
    }
}
