//! `keleustes forge …`: connect a forge, sign its CLI in, list and check
//! connections. `keleustes github …` stays as the github.com alias.

use anyhow::{Result, bail};
use clap::Subcommand;
use kls_core::Core;
use kls_forge::forge::auth::{SignInMethod, auth_for};
use kls_forge::login::{WebLogin, WebState};
use kls_forge::{Connection, ForgeKind};

#[derive(Subcommand)]
pub enum ForgeCmd {
    /// Every connection: id, kind, host, login, enabled.
    List,
    /// Adds a connection (github, ghes, gitlab, gitea, forgejo, codeberg).
    Connect {
        #[arg(long)]
        kind: String,
        /// Defaults to the kind's public host (github.com, gitlab.com, gitea.com, codeberg.org).
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "")]
        label: String,
    },
    /// Signs the forge's CLI in: a one-time code (--device), the CLI's
    /// browser flow (--web), or a token from standard input (--with-token).
    Login {
        #[arg(long, default_value = "github")]
        kind: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, conflicts_with_all = ["web", "with_token"])]
        device: bool,
        #[arg(long, conflicts_with = "with_token")]
        web: bool,
        #[arg(long)]
        with_token: bool,
        /// gh only: keep the account and add the scopes its token lacks.
        #[arg(long)]
        refresh: bool,
    },
    /// One line per connection: host, login, route, missing scopes. Exit 0
    /// when every listed connection is signed in.
    Status {
        #[arg(long)]
        host: Option<String>,
    },
}

fn kind_host(kind: &str, host: Option<&str>) -> Result<(ForgeKind, String)> {
    let codeberg = kind.eq_ignore_ascii_case("codeberg");
    let Some(k) = ForgeKind::parse(kind) else {
        bail!("unknown forge kind {kind:?} (github, ghes, gitlab, gitea, forgejo, codeberg)");
    };
    let host = match host {
        Some(h) => h.to_string(),
        None if codeberg => "codeberg.org".into(),
        None => match k.default_host() {
            Some(h) => h.into(),
            None => bail!("--host is required for {}", k.label()),
        },
    };
    Ok((k, host))
}

pub async fn run(core: &Core, cmd: &ForgeCmd) -> Result<i32> {
    let svc = kls_forge::service(core);
    match cmd {
        ForgeCmd::List => {
            for c in svc.connections().await? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    c.id,
                    c.kind.id(),
                    c.host,
                    c.login,
                    if c.enabled { "enabled" } else { "disabled" }
                );
            }
            Ok(0)
        }
        ForgeCmd::Connect { kind, host, label } => {
            let (k, h) = kind_host(kind, host.as_deref())?;
            let c = svc.add_connection(k, &h, label, "cli").await?;
            println!("{}\t{}\t{}", c.id, c.kind.id(), c.host);
            Ok(0)
        }
        ForgeCmd::Status { host } => {
            let mut all = true;
            for c in svc.connections().await? {
                if host.as_deref().is_some_and(|h| h != c.host) {
                    continue;
                }
                let route = svc.recheck_conn(&c).await;
                let auth = svc.auth_state(&c).await;
                let missing = match &auth {
                    kls_forge::forge::auth::AuthState::SignedIn { missing, .. } => {
                        missing.join(",")
                    }
                    _ => String::new(),
                };
                all &= auth.signed_in() && c.enabled;
                println!(
                    "{}\t{}\t{}\t{}",
                    c.host,
                    if auth.signed_in() {
                        auth.login()
                    } else {
                        auth.why()
                    },
                    route.label(),
                    missing
                );
            }
            Ok(if all { 0 } else { 1 })
        }
        ForgeCmd::Login {
            kind,
            host,
            device,
            web,
            with_token,
            refresh,
        } => {
            let (k, h) = kind_host(kind, host.as_deref())?;
            let conn: Connection = svc.add_connection(k, &h, "", "cli").await?;
            if *with_token {
                let mut token = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
                return Ok(match svc.login_token_conn(&conn, &token, "cli").await {
                    Ok(r) => {
                        println!("signed in · {}", r.label());
                        0
                    }
                    Err(e) => {
                        eprintln!("{e:#}");
                        1
                    }
                });
            }
            let probe = svc.probe(&conn).await;
            if !probe.installed {
                eprintln!("{}", probe.error);
                for (os, c) in conn.cli().install() {
                    eprintln!("  {os}: {c}");
                }
                return Ok(1);
            }
            let methods = auth_for(conn.cli()).methods(&probe, &conn);
            let want = if *device {
                Some(SignInMethod::DeviceCode)
            } else if *web {
                Some(SignInMethod::BrowserLoopback)
            } else {
                None
            };
            let method = match want {
                // gh's browser sign-in is its device code.
                Some(SignInMethod::BrowserLoopback) if conn.cli() == kls_forge::CliKind::Gh => {
                    SignInMethod::DeviceCode
                }
                Some(m) if methods.contains(&m) => m,
                Some(m) => bail!(
                    "{} on {} does not offer {}; try --with-token",
                    conn.cli().name(),
                    conn.host,
                    m.label().to_lowercase()
                ),
                None => methods
                    .iter()
                    .copied()
                    .find(|m| *m != SignInMethod::Token)
                    .unwrap_or(SignInMethod::Token),
            };
            if method == SignInMethod::Token {
                bail!(
                    "{} on {} signs in with a token: --with-token < token.txt",
                    conn.cli().name(),
                    conn.host
                );
            }
            let w = if conn.is_github_com() {
                if *refresh {
                    WebLogin::refresh()
                } else {
                    WebLogin::default()
                }
            } else {
                WebLogin::for_conn(&conn, method, *refresh)
            };
            let w2 = w.clone();
            let shown = std::sync::Mutex::new(None::<String>);
            let route = svc
                .login_web_as(
                    &w,
                    move || {
                        let mut s = shown.lock().unwrap();
                        match w2.state() {
                            WebState::Code { code, url } if s.as_deref() != Some(&code) => {
                                eprintln!("One-time code: {code}\nEnter it at: {url}");
                                if crate::desktop::open_in_browser(&url) {
                                    eprintln!("(opened in your browser; waiting for approval)");
                                }
                                *s = Some(code);
                            }
                            WebState::Browser { url, .. }
                                if !url.is_empty() && s.as_deref() != Some(&url) =>
                            {
                                eprintln!("Approve in your browser: {url}");
                                *s = Some(url);
                            }
                            _ => {}
                        }
                    },
                    "cli",
                )
                .await;
            match w.state() {
                WebState::Done => {
                    println!("signed in · {}", route.label());
                    Ok(0)
                }
                WebState::Failed(e) => {
                    eprintln!("{e}");
                    Ok(1)
                }
                _ => Ok(1),
            }
        }
    }
}
