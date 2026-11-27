//! The `gh` CLI on the Keleustes host, signed in to github.com.
//! Other CLIs and hosts: [`crate::forge::cli::LocalCli`].

use crate::exec::{Exec, ProcessExec};
use crate::forge::auth::{GhAuth, gh_account, gh_missing_scopes};
use crate::{ForgeCli, GhOutput, Route};
use anyhow::Result;
use std::sync::Arc;

pub(crate) const MISSING: &str = "gh is not installed on this host";

/// The local `gh` as the sign-in button needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalAuth {
    /// The active github.com account works. `missing` are the scopes of
    /// [`ALL_SCOPES`](crate::login::ALL_SCOPES) its token lacks (empty for
    /// a fine-grained token, which reports none).
    SignedIn {
        login: String,
        missing: Vec<String>,
    },
    SignedOut(String),
    Missing(String),
}

impl LocalAuth {
    pub fn signed_in(&self) -> bool {
        matches!(self, Self::SignedIn { .. })
    }
}

pub struct LocalGh {
    exec: Arc<dyn Exec>,
    program: String,
}

impl Default for LocalGh {
    fn default() -> Self {
        Self::new(Arc::new(ProcessExec::default()))
    }
}

impl LocalGh {
    pub fn new(exec: Arc<dyn Exec>) -> Self {
        Self {
            exec,
            program: crate::CliKind::Gh.program(),
        }
    }

    /// The program run (`gh`, or `$KELEUSTES_GH`).
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The process runner (shared with the other local CLIs).
    pub fn exec(&self) -> Arc<dyn Exec> {
        self.exec.clone()
    }

    /// Whether `gh` is installed and its active github.com account works.
    pub async fn auth(&self) -> LocalAuth {
        match self.status().await {
            Ok(o) => {
                let text = format!("{}\n{}", o.stdout, o.stderr);
                LocalAuth::SignedIn {
                    login: gh_account(&text).unwrap_or_default(),
                    missing: gh_missing_scopes(&text, crate::login::ALL_SCOPES),
                }
            }
            Err(why) if why.starts_with(MISSING) => LocalAuth::Missing(why),
            Err(why) => LocalAuth::SignedOut(why),
        }
    }

    /// `Ok(())` when `gh auth status` succeeds for github.com, else why not.
    pub async fn auth_status(&self) -> Result<(), String> {
        self.status().await.map(|_| ())
    }

    async fn status(&self) -> Result<GhOutput, String> {
        GhAuth::raw_status(self.exec.as_ref(), &self.program, crate::login::HOST).await
    }
}

#[async_trait::async_trait]
impl ForgeCli for LocalGh {
    fn route(&self) -> Route {
        Route::Local
    }

    async fn run(&self, args: &[String], stdin: Option<&str>) -> Result<GhOutput> {
        self.exec.exec(&self.program, args, stdin).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "github.com\n  ✓ Logged in to github.com account ana (keyring)\n  - Active account: true\n  - Git operations protocol: ssh\n  - Token: gho_****\n  - Token scopes: 'admin:gpg_key', 'repo', 'workflow'\n";

    #[test]
    fn reads_account_and_missing_scopes() {
        let all = crate::login::ALL_SCOPES;
        assert_eq!(gh_account(STATUS).as_deref(), Some("ana"));
        let m = gh_missing_scopes(STATUS, all);
        assert!(m.contains(&"audit_log".to_string()));
        assert!(!m.contains(&"repo".to_string()));
        assert_eq!(m.len(), all.len() - 3);
        assert!(gh_missing_scopes("  - Token: github_pat_x\n", all).is_empty());
    }
}
