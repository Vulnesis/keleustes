//! `keleustes`: the desktop Software Factory.
//!
//! With no subcommand it opens the window and serves the node port.
//! `--headless` serves without a window. `token new` and `nodes ...` work
//! against the same data directory while the app is running.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kls_core::Core;
use std::path::PathBuf;

mod ax;
mod cli;
mod control;
mod desktop;
mod forge_cmd;
mod host_cmd;
mod tray;

#[derive(Parser)]
#[command(name = "keleustes", version, about = "The desktop Software Factory")]
struct Cli {
    /// Data directory (database, CA, artifacts).
    #[arg(long, env = "KELEUSTES_DATA", global = true)]
    data: Option<PathBuf>,
    /// Use this database instead of <data>/keleustes.db (sqlite:… or postgres://…).
    #[arg(long, env = "KELEUSTES_DATABASE_URL", global = true)]
    database_url: Option<String>,
    /// Serve nodes without opening a window.
    #[arg(long)]
    headless: bool,
    /// Print the contract: one JSON envelope as the last stdout line
    /// (see `keleustes contract`).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
// Parsed once per process; the sessions subcommand carries many flags.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Join tokens.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Joined nodes.
    Nodes {
        #[command(subcommand)]
        cmd: NodesCmd,
    },
    /// Show or change a setting (for example node.listen_addr).
    Config { key: String, value: Option<String> },
    /// Sessions on nodes: list, dispatch, transcript, prompt.
    Sessions {
        #[command(subcommand)]
        cmd: cli::SessionsCmd,
    },
    /// Open a keleustes:// link (or an old web path) in the running window,
    /// starting it if none runs.
    Open { url: Option<String> },
    /// The local gh CLI's sign-in to github.com (alias of `forge … --kind github`).
    Github {
        #[command(subcommand)]
        cmd: GithubCmd,
    },
    /// Forge connections (GitHub, GitHub Enterprise, GitLab, Gitea, Forgejo,
    /// Codeberg): connect, sign the CLI in, list, status.
    Forge {
        #[command(subcommand)]
        cmd: forge_cmd::ForgeCmd,
    },
    /// Host management: inventory, actions, kill switch, audit verify.
    Host {
        #[command(subcommand)]
        cmd: host_cmd::HostCmd,
    },
    /// The machine-readable CLI contract: verbs, exit codes, error codes and
    /// JSON Schemas. Trust it over the docs.
    Contract {
        /// Only this schema.
        #[arg(long, value_parser = ax::contract::SCHEMAS)]
        schema: Option<String>,
        /// The operator skill (SKILL.md) for agents that drive Keleustes.
        #[arg(long, conflicts_with = "schema")]
        skill: bool,
    },
    /// Usage friction in sessions, dispatches and the audit ledger, sorted
    /// into a fixed taxonomy (read-only but for --append-findings).
    AnalyzeUsage {
        /// How far back (30m, 12h, 7d).
        #[arg(long, default_value = "7d")]
        since: String,
        /// Only findings that are interface defects.
        #[arg(long)]
        ax_only: bool,
        /// Append new findings to this JSONL fix queue (deduplicated by findingId).
        #[arg(long)]
        append_findings: Option<PathBuf>,
        /// Only this session.
        #[arg(long)]
        session: Option<String>,
        /// Asks open longer than this count as unanswered.
        #[arg(long, default_value = "15m")]
        ask_after: String,
        /// Dispatches unbound longer than this count as stuck.
        #[arg(long, default_value = "10m")]
        stuck_after: String,
    },
}

#[derive(Subcommand)]
enum GithubCmd {
    /// Sign the local gh in with every scope: a one-time code for the
    /// browser, or --with-token to read a classic token from standard input.
    Login {
        #[arg(long, conflicts_with = "refresh")]
        with_token: bool,
        /// Keep the signed-in account and add the scopes its token lacks.
        #[arg(long)]
        refresh: bool,
    },
    /// The route to GitHub and whether the local gh is signed in.
    Status,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Mint a single-use join token and print it.
    New {
        #[arg(long, default_value = "")]
        label: String,
        /// Minutes until it expires (default: the node.token_ttl_minutes setting, else 15).
        #[arg(long)]
        ttl_minutes: Option<i64>,
    },
}

#[derive(Subcommand)]
enum NodesCmd {
    List,
    Revoke {
        id: String,
        #[arg(long, default_value = "revoked from the command line")]
        reason: String,
    },
}

/// `keleustes github …`. Exit code 0 when signed in.
async fn github_cmd(core: &Core, cmd: &GithubCmd) -> Result<i32> {
    use kls_forge::login::{self, WebLogin, WebState};
    let svc = kls_forge::service(core);
    match cmd {
        GithubCmd::Status => {
            let route = svc.recheck_route().await;
            println!("route\t{}", route.label());
            let local = svc.local_auth().await;
            match &local {
                kls_forge::LocalAuth::SignedIn { login, missing } => {
                    println!("local gh\tsigned in as {login}");
                    if !missing.is_empty() {
                        println!(
                            "missing scopes\t{} (keleustes github login --refresh)",
                            missing.join(",")
                        );
                    }
                }
                kls_forge::LocalAuth::SignedOut(w) | kls_forge::LocalAuth::Missing(w) => {
                    println!("local gh\t{w}")
                }
            }
            return Ok(if local.signed_in() { 0 } else { 1 });
        }
        GithubCmd::Login {
            with_token: true, ..
        } => {
            let mut token = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut token)?;
            if let Err(e) = login::login_with_token(&svc.local_program(), &token).await {
                eprintln!("{e:#}");
                return Ok(1);
            }
        }
        GithubCmd::Login {
            with_token: false,
            refresh,
        } => {
            let web = if *refresh {
                WebLogin::refresh()
            } else {
                WebLogin::default()
            };
            let w2 = web.clone();
            let shown = std::sync::Mutex::new(None::<String>);
            web.run(&svc.local_program(), move || {
                if let WebState::Code { code, url } = w2.state() {
                    let mut s = shown.lock().unwrap();
                    if s.as_deref() != Some(&code) {
                        eprintln!("One-time code: {code}\nEnter it at: {url}");
                        if desktop::open_in_browser(&url) {
                            eprintln!("(opened in your browser; waiting for approval)");
                        }
                        *s = Some(code);
                    }
                }
            })
            .await;
            match web.state() {
                WebState::Done => {}
                WebState::Failed(e) => {
                    eprintln!("{e}");
                    return Ok(1);
                }
                _ => return Ok(1),
            }
        }
    }
    let route = svc.recheck_route().await;
    // A running window reloads its GitHub screens on this event.
    core.emit(
        "github.login",
        "local",
        &serde_json::json!({ "route": route.label() }),
    )
    .await?;
    kls_forge::forge::guard::audit(
        core,
        "cli",
        "sign_in",
        "github.com",
        "github.com",
        "ok",
        serde_json::json!({ "cli": "gh", "route": route.label() }),
    )
    .await;
    println!("signed in · {}", route.label());
    Ok(0)
}

/// The envelope kind of a verb and whether it changes state.
fn kind_of(cmd: &Cmd) -> (String, bool) {
    // A verb owned elsewhere: its subcommand name from the command line.
    let sub = |top: &str| {
        let mut args = std::env::args().skip(1);
        let mut found = false;
        let mut skip = false;
        for a in args.by_ref() {
            if skip {
                skip = false;
                continue;
            }
            if a == "--data" || a == "--database-url" {
                skip = true;
                continue;
            }
            if a.starts_with('-') {
                continue;
            }
            if found {
                return format!("{top}.{a}");
            }
            found = a == top;
        }
        top.to_string()
    };
    match cmd {
        Cmd::Token { .. } => ("token.new".into(), true),
        Cmd::Nodes {
            cmd: NodesCmd::List,
        } => ("nodes.list".into(), false),
        Cmd::Nodes {
            cmd: NodesCmd::Revoke { .. },
        } => ("nodes.revoke".into(), true),
        Cmd::Config { value, .. } => ("config".into(), value.is_some()),
        Cmd::Sessions { cmd } => (cmd.kind().into(), cmd.mutating()),
        Cmd::Open { .. } => ("open".into(), false),
        Cmd::Github {
            cmd: GithubCmd::Status,
        } => ("github.status".into(), false),
        Cmd::Github { .. } => ("github.login".into(), true),
        Cmd::Forge { .. } => (sub("forge"), false),
        Cmd::Host { .. } => (sub("host"), false),
        Cmd::Contract { .. } => ("contract".into(), false),
        Cmd::AnalyzeUsage { .. } => ("analyze-usage".into(), false),
    }
}

/// The verbs this file owns, under the contract (`ax`).
async fn run_verb(cmd: &Cmd, core: &Core, hub: &kls_node::Hub, ctx: &ax::Ctx) -> Result<ax::Done> {
    use serde_json::json;
    match cmd {
        Cmd::Token {
            cmd: TokenCmd::New { label, ttl_minutes },
        } => {
            let ttl_minutes = match ttl_minutes {
                Some(m) => *m,
                None => core
                    .store()
                    .setting("node.token_ttl_minutes")
                    .await?
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(15),
            };
            let t = hub.mint_token(label, ttl_minutes * 60_000).await?;
            ctx.out(&t.token);
            eprintln!(
                "Single use, expires in {ttl_minutes} min. On the host run: /connect <token> in belai, or belai connect <token>"
            );
            let mut d = ax::Done::data(json!({ "token": t.token, "ttlMinutes": ttl_minutes }));
            d.next = vec![format!("belai connect {}", t.token)];
            Ok(d)
        }
        Cmd::Nodes {
            cmd: NodesCmd::List,
        } => {
            let mut out = Vec::new();
            for n in core.store().nodes().await? {
                let state = if n.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                ctx.out(format!(
                    "{}\t{}\t{}\t{}\t{}",
                    n.id, n.name, n.host.hostname, n.host.belai_version, state
                ));
                out.push(
                    json!({ "id": n.id, "name": n.name, "hostname": n.host.hostname,
                                 "belaiVersion": n.host.belai_version, "state": state,
                                 "lastSeenAt": n.last_seen_at }),
                );
            }
            Ok(ax::Done::data(json!({ "nodes": out })))
        }
        Cmd::Nodes {
            cmd: NodesCmd::Revoke { id, reason },
        } => {
            let changed = hub.revoke(id, reason).await?;
            ctx.out(if changed {
                "revoked"
            } else {
                "already revoked or unknown"
            });
            Ok(ax::Done::data(json!({ "nodeId": id, "revoked": changed })))
        }
        Cmd::Config { key, value } => match value {
            Some(v) => {
                core.store().set_setting(key, v).await?;
                // A running app reloads settings on this event.
                core.emit("settings.saved", key, &json!({ "keys": [key] }))
                    .await?;
                Ok(ax::Done::data(json!({ "key": key, "value": v })))
            }
            None => {
                let v = core.store().setting(key).await?.unwrap_or_default();
                ctx.out(&v);
                Ok(ax::Done::data(json!({ "key": key, "value": v })))
            }
        },
        Cmd::Sessions { cmd } => cli::run(core, hub, cmd, ctx).await,
        Cmd::AnalyzeUsage {
            since,
            ax_only,
            append_findings,
            session,
            ask_after,
            stuck_after,
        } => {
            let ms = |s: &str| -> Result<i64> {
                ax::parse_duration(s)
                    .map(|d| d.as_millis() as i64)
                    .map_err(|e| ax::fail(ax::Code::UsageError, e))
            };
            let o = ax::analyze::Options {
                since_ms: ms(since)?,
                ax_only: *ax_only,
                session: session.clone(),
                ask_after_ms: ms(ask_after)?,
                stuck_after_ms: ms(stuck_after)?,
            };
            let findings = ax::analyze::analyze(core, &o, kls_store::now_ms()).await?;
            for f in &findings {
                ctx.out(format!(
                    "{}\t{}\t{}\t{}",
                    f["kind"].as_str().unwrap_or(""),
                    f["findingId"].as_str().unwrap_or(""),
                    f["detail"].as_str().unwrap_or(""),
                    f["pointer"].as_str().unwrap_or("")
                ));
            }
            let appended = match append_findings {
                Some(p) => Some(ax::analyze::append_findings(p, &findings)?),
                None => None,
            };
            Ok(ax::Done::data(json!({
                "findings": findings,
                "counts": ax::analyze::counts(&findings),
                "appended": appended,
            })))
        }
        Cmd::Github { .. }
        | Cmd::Forge { .. }
        | Cmd::Host { .. }
        | Cmd::Open { .. }
        | Cmd::Contract { .. } => Err(anyhow::anyhow!("handled in main")),
    }
}

fn data_dir(cli: &Cli) -> Result<PathBuf> {
    if let Some(d) = &cli.data {
        return Ok(d.clone());
    }
    let dirs = directories::ProjectDirs::from("is", "vulnes", "keleustes")
        .context("no home directory to keep Keleustes data in; pass --data")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            let json = std::env::args().any(|a| a == "--json");
            if !json || matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                e.exit();
            }
            // The usage error as an envelope (exit 2), like every other verb.
            let msg = e.render().to_string();
            let msg = msg.trim().trim_start_matches("error: ");
            println!("{}", ax::usage_envelope("usage", msg));
            std::process::exit(ax::Code::UsageError.exit());
        }
    };
    let rt = tokio::runtime::Runtime::new()?;
    let prefix = match &cli.data {
        Some(d) => format!("keleustes --data {}", ax::q(&d.to_string_lossy())),
        None => "keleustes".to_string(),
    };
    let verb = cli
        .cmd
        .as_ref()
        .filter(|c| !matches!(c, Cmd::Open { .. }))
        .map(|c| {
            let (kind, mutating) = kind_of(c);
            ax::Ctx {
                json: cli.json,
                kind,
                prefix: prefix.clone(),
                mutating,
            }
        });
    // The contract needs no data directory.
    if let (Some(Cmd::Contract { schema, skill }), Some(ctx)) = (&cli.cmd, &verb) {
        let data = if *skill {
            ctx.out(ax::contract::SKILL.trim_end());
            serde_json::json!({ "skill": ax::contract::SKILL })
        } else {
            let v = match schema {
                Some(s) => ax::contract::schema(s).context("unknown schema")?,
                None => ax::contract::contract(&<Cli as clap::CommandFactory>::command()),
            };
            ctx.out(serde_json::to_string_pretty(&v)?);
            v
        };
        let code = rt.block_on(ax::finish(ctx, None, Ok(ax::Done::data(data))));
        std::process::exit(code);
    }
    // Under --json, a failure to open the data directory is an envelope too.
    let data = match (data_dir(&cli), &verb) {
        (Ok(d), _) => d,
        (Err(e), Some(ctx)) if ctx.json => {
            std::process::exit(rt.block_on(ax::finish(ctx, None, Err(e))))
        }
        (Err(e), _) => return Err(e),
    };
    // Single instance (M6): a window for this data directory takes the link.
    let open_url = match &cli.cmd {
        Some(Cmd::Open { url }) => Some(url.clone().unwrap_or_default()),
        _ => None,
    };
    if (open_url.is_some() || (cli.cmd.is_none() && !cli.headless))
        && desktop::hand_off(&data, open_url.as_deref().unwrap_or(""))
    {
        return Ok(());
    }
    let opened = rt.block_on(async {
        let core = Core::open(&data, cli.database_url.as_deref()).await?;
        let hub = kls_node::hub(core.clone()).await?;
        anyhow::Ok((core, hub))
    });
    let (core, hub) = match (opened, &verb) {
        (Ok(x), _) => x,
        (Err(e), Some(ctx)) if ctx.json => {
            std::process::exit(rt.block_on(ax::finish(ctx, None, Err(e))))
        }
        (Err(e), _) => return Err(e),
    };

    if let (Some(cmd), Some(ctx)) = (&cli.cmd, &verb) {
        // Verbs owned elsewhere keep their own output and exit code; under
        // --json their outcome is wrapped in an envelope.
        let foreign = match cmd {
            Cmd::Github { cmd } => Some(rt.block_on(github_cmd(&core, cmd))),
            Cmd::Forge { cmd } => Some(rt.block_on(forge_cmd::run(&core, cmd))),
            Cmd::Host { cmd } => Some(rt.block_on(host_cmd::run(&core, cmd))),
            _ => None,
        };
        let code = match foreign {
            Some(r) if !ctx.json => r?,
            Some(r) => {
                let res = match r {
                    Ok(0) => Ok(ax::Done::data(serde_json::json!({ "exit": 0 }))),
                    Ok(n) => Err(ax::fail(
                        ax::Code::Failed,
                        format!("exited {n}; its output above says why"),
                    )),
                    Err(e) => Err(e),
                };
                rt.block_on(ax::finish(ctx, Some(&core), res))
            }
            None => rt.block_on(async {
                let res = run_verb(cmd, &core, &hub, ctx).await;
                ax::finish(ctx, Some(&core), res).await
            }),
        };
        std::process::exit(code);
    }

    // Fail before any window opens when the node port is taken (most often
    // another Keleustes on this data directory or port).
    let listen = rt.block_on(core.listen_addr())?;
    if let Err(e) = std::net::TcpListener::bind(&listen) {
        anyhow::bail!(
            "cannot listen for nodes on {listen}: {e} (another Keleustes running? change it with: keleustes config node.listen_addr HOST:PORT)"
        );
    }
    // Pick up what `keleustes` commands beside this process write.
    {
        let _g = rt.enter();
        core.start_event_tail(std::time::Duration::from_millis(750));
    }
    // GitHub plane: route detection and the ETag poller (M4).
    rt.block_on(async { kls_forge::start(&core) });

    // Serve the node port in the background for the life of the process.
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = rt.spawn(kls_node::run(hub.clone(), async {
        let _ = stop_rx.await;
    }));
    // Workflow scheduler and executor (M3), for the life of the process.
    let _engine = rt.block_on(kls_engine::start(core.clone()))?;
    // Audit ledger fold and teleport retention (M6).
    let _audit = {
        let _g = rt.enter();
        kls_core::audit::start(&core)
    };
    // Governance: kill-switch stop cancels open runs; lost sessions,
    // retention and estimate records.
    kls_core::control::register_stopper(&core, std::sync::Arc::new(control::EngineStopper));
    let _sweeps = {
        let _g = rt.enter();
        kls_core::sweeps::start(&core)
    };

    if cli.headless {
        tracing::info!(data = %data.display(), "running headless; Ctrl-C to stop");
        rt.block_on(async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                r = server => { r??; }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        return Ok(());
    }

    // Deep links from `keleustes open` and notification clicks (M6).
    let (links_tx, mut links_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    rt.block_on(async {
        if let Err(e) = desktop::listen(&data, links_tx.clone()) {
            tracing::warn!("single-instance socket: {e:#}");
        }
        desktop::start_notifications(core.clone(), links_tx.clone());
    });
    if let Some(url) = open_url.filter(|u| !u.is_empty()) {
        let _ = links_tx.send(url);
    }
    let handle = rt.handle().clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Keleustes")
            // Wayland takes the window icon from keleustes.desktop by app id.
            .with_app_id("keleustes")
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([760.0, 480.0])
            .with_icon(std::sync::Arc::new(kls_ui::brand::window_icon())),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Keleustes",
        options,
        Box::new(move |cc| {
            let model = kls_ui::Model::new(hub, handle.clone(), cc.egui_ctx.clone());
            let (m, ctx) = (model.clone(), cc.egui_ctx.clone());
            handle.spawn(async move {
                while let Some(url) = links_rx.recv().await {
                    if let Some((screen, focus)) = desktop::parse_link(&url) {
                        m.go(screen, focus);
                    } else if !url.is_empty() {
                        tracing::warn!(%url, "unknown link");
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
            });
            let (tray_tx, tray_rx) = std::sync::mpsc::channel();
            let (state_tx, state_rx) = std::sync::mpsc::channel();
            let wake_ctx = cc.egui_ctx.clone();
            let tray = tray::start(tray_tx, state_rx, move || wake_ctx.request_repaint());
            if tray.is_none() {
                tracing::info!("no system tray; closing the window quits");
            } else {
                let _g = handle.enter();
                let wake_ctx = cc.egui_ctx.clone();
                tray::feed(model.hub.clone(), state_tx, move || {
                    wake_ctx.request_repaint()
                });
            }
            Ok(Box::new(tray::WithTray {
                app: kls_ui::App::new(model.clone()),
                model,
                actions: tray_rx,
                active: tray.is_some(),
                quitting: false,
                tray,
            }))
        }),
    );
    desktop::stop_listening(&data);
    let _ = stop_tx.send(());
    result.map_err(|e| anyhow::anyhow!("window: {e}"))
}
