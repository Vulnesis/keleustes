//! Each forge CLI's sign-in and status: probe, status parsing, the sign-in
//! methods it offers, argv for interactive and token sign-ins, the token
//! page with the scopes to tick, progress parsing and the git helper.
//!
//! Stateless; the credential always stays in the CLI's own store.

use super::kind::{CliKind, Connection, ForgeKind};
use crate::exec::Exec;
use crate::login::{ALL_SCOPES, device_code, device_url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Installed? Version and the optional flags its `--help` lists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CliProbe {
    pub cli: Option<CliKind>,
    pub installed: bool,
    pub version: String,
    /// `device`, `web`, `stdin`, `api`, `oauth`, `client_id`.
    pub flags: BTreeSet<String>,
    pub error: String,
    /// What the server said about itself (forges.md 2.3, 2.4); `None`
    /// before the server probe ran.
    #[serde(default)]
    pub server: Option<super::server::ServerInfo>,
}

impl CliProbe {
    pub fn has(&self, f: &str) -> bool {
        self.flags.contains(f)
    }
}

/// The CLI's sign-in to one host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AuthState {
    SignedIn {
        login: String,
        /// Scopes of the "all scopes" set the token lacks.
        missing: Vec<String>,
        /// Unix ms (GitLab PATs).
        expires_at: Option<i64>,
    },
    SignedOut(String),
    /// The CLI is not installed.
    Missing(String),
    /// Installed but lacks a command Keleustes needs (e.g. `tea api`).
    TooOld(String),
}

impl AuthState {
    pub fn signed_in(&self) -> bool {
        matches!(self, Self::SignedIn { .. })
    }

    pub fn login(&self) -> &str {
        match self {
            Self::SignedIn { login, .. } => login,
            _ => "",
        }
    }

    /// Why not signed in ("" when signed in).
    pub fn why(&self) -> &str {
        match self {
            Self::SignedIn { .. } => "",
            Self::SignedOut(w) | Self::Missing(w) | Self::TooOld(w) => w,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignInMethod {
    /// A one-time code entered on the forge's device page.
    DeviceCode,
    /// The CLI opens the browser and waits on a loopback callback.
    BrowserLoopback,
    /// A pasted personal access token.
    Token,
}

impl SignInMethod {
    pub fn label(self) -> &'static str {
        match self {
            SignInMethod::DeviceCode => "Device",
            SignInMethod::BrowserLoopback => "Browser",
            SignInMethod::Token => "Token",
        }
    }
}

/// How the token reaches the CLI. Never its argv (forges.md 2.6): another
/// local user can read a process's command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenInput {
    Stdin,
    /// Set in the child's environment only.
    Env(&'static str),
}

/// The variable tea's `login add --token` reads when the flag is absent.
pub const TEA_TOKEN_ENV: &str = "GITEA_SERVER_TOKEN";

/// Whether `tea login add --help` says the token can come from
/// [`TEA_TOKEN_ENV`] (urfave/cli prints it as `[$GITEA_SERVER_TOKEN]`).
pub fn tea_takes_token_env(help: &str) -> bool {
    help.contains(&format!("${TEA_TOKEN_ENV}"))
}

/// A process to start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Where a token is made and what to tick there.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenPage {
    pub url: String,
    pub scopes: Vec<&'static str>,
    /// The page arrives with the name and scopes already filled in.
    pub prefilled: bool,
    pub expiry_required: bool,
}

/// What one line of an interactive sign-in said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Code { code: String, url: String },
    OpenUrl(String),
    PressEnter,
    Done(String),
    Failed(String),
}

#[async_trait::async_trait]
pub trait CliAuth: Send + Sync {
    fn cli(&self) -> CliKind;
    /// Installed? Version and detected flags. `host` lets glab check for a
    /// configured OAuth client id.
    async fn probe(&self, exec: &dyn Exec, host: &str) -> CliProbe;
    /// Signed in to `conn.host`, as whom, with which scopes missing.
    async fn status(&self, exec: &dyn Exec, conn: &Connection) -> AuthState;
    /// Sign-in methods this CLI offers for this host, best first.
    fn methods(&self, probe: &CliProbe, conn: &Connection) -> Vec<SignInMethod>;
    /// argv and environment for an interactive method.
    fn interactive(&self, m: SignInMethod, conn: &Connection) -> Spawn;
    /// argv, environment and how the token is passed.
    fn token(&self, conn: &Connection) -> (Spawn, TokenInput);
    /// The token page and the scopes to tick there.
    fn token_page(&self, conn: &Connection) -> TokenPage;
    /// Reads one output line of an interactive sign-in.
    fn progress(&self, line: &str) -> Option<Progress>;
    /// Git credential helper for pushes.
    fn git_helper(&self, conn: &Connection) -> String;
}

pub fn auth_for(cli: CliKind) -> &'static dyn CliAuth {
    match cli {
        CliKind::Gh => &GhAuth,
        CliKind::Glab => &GlabAuth,
        CliKind::Tea => &TeaAuth,
    }
}

fn spawn(cli: CliKind, args: Vec<String>) -> Spawn {
    Spawn {
        program: cli.program(),
        args,
        env: cli
            .env()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn first_line(o: &crate::GhOutput, skip: &str, fallback: &str) -> String {
    o.stderr
        .lines()
        .chain(o.stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != skip)
        .unwrap_or(fallback)
        .trim_start_matches(['X', '!', ' ', '✗'])
        .to_string()
}

async fn run(exec: &dyn Exec, cli: CliKind, args: &[&str]) -> anyhow::Result<crate::GhOutput> {
    exec.exec(&cli.program(), &crate::args(args.iter().copied()), None)
        .await
}

async fn version(exec: &dyn Exec, cli: CliKind) -> CliProbe {
    let mut p = CliProbe {
        cli: Some(cli),
        ..Default::default()
    };
    match run(exec, cli, &["--version"]).await {
        Ok(o) => {
            p.installed = true;
            p.version = o
                .stdout
                .lines()
                .chain(o.stderr.lines())
                .find_map(|l| {
                    l.split_whitespace()
                        .find(|w| {
                            w.trim_start_matches('v')
                                .starts_with(|c: char| c.is_ascii_digit())
                        })
                        .map(|w| w.trim_start_matches('v').trim_end_matches(',').to_string())
                })
                .unwrap_or_default();
        }
        Err(e) => p.error = format!("{} is not installed on this host ({e:#})", cli.name()),
    }
    p
}

// ---- gh ----------------------------------------------------------------------

/// Classic scopes for GHES: github.com's set without `copilot` and `codespace`.
pub fn scopes(kind: ForgeKind) -> Vec<&'static str> {
    match kind {
        ForgeKind::Github => ALL_SCOPES.to_vec(),
        ForgeKind::Ghes => ALL_SCOPES
            .iter()
            .copied()
            .filter(|s| !matches!(*s, "copilot" | "codespace"))
            .collect(),
        ForgeKind::Gitlab => GITLAB_SCOPES.to_vec(),
        ForgeKind::Gitea | ForgeKind::Forgejo => GITEA_SCOPES.to_vec(),
    }
}

/// GitLab PAT scopes Keleustes asks for (`api` alone covers every call).
pub const GITLAB_SCOPES: &[&str] = &[
    "api",
    "read_api",
    "read_user",
    "read_repository",
    "write_repository",
    "read_registry",
    "write_registry",
    "create_runner",
    "manage_runner",
    "k8s_proxy",
    "ai_features",
    "self_rotate",
];

/// Gitea / Forgejo token scopes (`write:*` include `read:*`).
pub const GITEA_SCOPES: &[&str] = &[
    "write:activitypub",
    "write:issue",
    "write:misc",
    "write:notification",
    "write:organization",
    "write:package",
    "write:repository",
    "write:user",
];

pub struct GhAuth;

/// `ana` from "Logged in to github.com account ana (keyring)".
pub fn gh_account(status: &str) -> Option<String> {
    let rest = status.split("account ").nth(1)?;
    rest.split_whitespace().next().map(str::to_string)
}

/// Scopes of `want` absent from gh's "Token scopes:" line (none for a
/// fine-grained token, which reports none).
pub fn gh_missing_scopes(status: &str, want: &[&str]) -> Vec<String> {
    let Some(line) = status
        .lines()
        .find_map(|l| l.trim().strip_prefix("- Token scopes:"))
    else {
        return Vec::new();
    };
    let have: Vec<&str> = line
        .split(',')
        .map(|s| s.trim().trim_matches('\''))
        .filter(|s| !s.is_empty())
        .collect();
    if have.is_empty() || have == ["none"] {
        return Vec::new();
    }
    want.iter()
        .filter(|s| !have.contains(s))
        .map(|s| s.to_string())
        .collect()
}

impl GhAuth {
    /// `gh auth status` of the active account on `host` (`--active` needs
    /// gh 2.40; older gh checks every account). Ok with the output, or why not.
    pub async fn raw_status(
        exec: &dyn Exec,
        program: &str,
        host: &str,
    ) -> Result<crate::GhOutput, String> {
        let mut out = exec
            .exec(
                program,
                &crate::args(["auth", "status", "--hostname", host, "--active"]),
                None,
            )
            .await;
        if let Ok(o) = &out
            && o.exit != 0
            && o.stderr.contains("unknown flag")
        {
            out = exec
                .exec(
                    program,
                    &crate::args(["auth", "status", "--hostname", host]),
                    None,
                )
                .await;
        }
        match out {
            Ok(o) if o.exit == 0 => Ok(o),
            Ok(o) => Err(format!(
                "local gh is not signed in ({})",
                first_line(&o, host, "not signed in")
            )),
            Err(e) => Err(format!("{} ({e:#})", crate::local::MISSING)),
        }
    }

    /// The `auth login` argv for `host`, keeping the git protocol.
    pub fn login_args(conn: &Connection) -> Vec<String> {
        crate::args([
            "auth",
            "login",
            "--hostname",
            &conn.host,
            "--git-protocol",
            &conn.git_protocol,
            "--skip-ssh-key",
            "--scopes",
        ])
        .into_iter()
        .chain([scopes(conn.kind).join(",")])
        .collect()
    }

    /// `auth refresh` adding every scope to the active account of `host`.
    pub fn refresh_args(conn: &Connection) -> Vec<String> {
        crate::args(["auth", "refresh", "--hostname", &conn.host, "--scopes"])
            .into_iter()
            .chain([scopes(conn.kind).join(",")])
            .collect()
    }
}

#[async_trait::async_trait]
impl CliAuth for GhAuth {
    fn cli(&self) -> CliKind {
        CliKind::Gh
    }

    async fn probe(&self, exec: &dyn Exec, _host: &str) -> CliProbe {
        let mut p = version(exec, CliKind::Gh).await;
        if p.installed {
            for f in ["device", "stdin", "api"] {
                p.flags.insert(f.into());
            }
        }
        p
    }

    async fn status(&self, exec: &dyn Exec, conn: &Connection) -> AuthState {
        match Self::raw_status(exec, &CliKind::Gh.program(), &conn.host).await {
            Ok(o) => {
                let text = format!("{}\n{}", o.stdout, o.stderr);
                AuthState::SignedIn {
                    login: gh_account(&text).unwrap_or_default(),
                    missing: gh_missing_scopes(&text, &scopes(conn.kind)),
                    expires_at: None,
                }
            }
            Err(why) if why.starts_with(crate::local::MISSING) => AuthState::Missing(why),
            Err(why) => AuthState::SignedOut(why),
        }
    }

    fn methods(&self, _probe: &CliProbe, _conn: &Connection) -> Vec<SignInMethod> {
        vec![SignInMethod::DeviceCode, SignInMethod::Token]
    }

    fn interactive(&self, _m: SignInMethod, conn: &Connection) -> Spawn {
        let mut a = Self::login_args(conn);
        a.push("--web".into());
        let mut s = spawn(CliKind::Gh, a);
        // gh opens nothing itself; the window opens the device page.
        s.env.retain(|(k, _)| k != "GH_PROMPT_DISABLED");
        s.env.push(("GH_BROWSER".into(), "true".into()));
        s.env.push(("BROWSER".into(), "true".into()));
        s
    }

    fn token(&self, conn: &Connection) -> (Spawn, TokenInput) {
        let mut a = Self::login_args(conn);
        a.push("--with-token".into());
        (spawn(CliKind::Gh, a), TokenInput::Stdin)
    }

    fn token_page(&self, conn: &Connection) -> TokenPage {
        TokenPage {
            url: format!(
                "https://{}/settings/tokens/new?description=Keleustes&scopes={}",
                conn.host,
                scopes(conn.kind).join(",")
            ),
            scopes: scopes(conn.kind),
            prefilled: true,
            expiry_required: false,
        }
    }

    fn progress(&self, line: &str) -> Option<Progress> {
        if let Some(c) = device_code(line) {
            return Some(Progress::Code {
                code: c,
                url: device_url(line).unwrap_or_default(),
            });
        }
        if line.contains("Press Enter") {
            return Some(Progress::PressEnter);
        }
        device_url(line).map(Progress::OpenUrl)
    }

    fn git_helper(&self, _conn: &Connection) -> String {
        format!("!{} auth git-credential", CliKind::Gh.program())
    }
}

// ---- glab --------------------------------------------------------------------

pub struct GlabAuth;

/// A GitLab-style one-time code: 4 to 12 upper-case letters, digits and dashes
/// after the word "code", with at least one letter or digit group.
pub fn generic_code(line: &str) -> Option<String> {
    if let Some(c) = device_code(line) {
        return Some(c);
    }
    let lower = line.to_ascii_lowercase();
    let i = lower.find("code")?;
    line[i + 4..]
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|w| {
            (4..=12).contains(&w.len())
                && w.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
                && w.chars().any(|c| c.is_ascii_alphanumeric())
        })
        .map(str::to_string)
}

#[async_trait::async_trait]
impl CliAuth for GlabAuth {
    fn cli(&self) -> CliKind {
        CliKind::Glab
    }

    async fn probe(&self, exec: &dyn Exec, host: &str) -> CliProbe {
        let mut p = version(exec, CliKind::Glab).await;
        if !p.installed {
            return p;
        }
        p.flags.insert("api".into());
        if let Ok(o) = run(exec, CliKind::Glab, &["auth", "login", "--help"]).await {
            let help = format!("{}{}", o.stdout, o.stderr);
            for f in ["device", "web", "stdin"] {
                if help.contains(&format!("--{f}")) {
                    p.flags.insert(f.into());
                }
            }
        }
        if !host.is_empty()
            && let Ok(o) = run(
                exec,
                CliKind::Glab,
                &["config", "get", "client_id", "--host", host],
            )
            .await
            && o.exit == 0
            && !o.stdout.trim().is_empty()
        {
            p.flags.insert("client_id".into());
        }
        p
    }

    async fn status(&self, exec: &dyn Exec, conn: &Connection) -> AuthState {
        let st = match run(
            exec,
            CliKind::Glab,
            &["auth", "status", "--hostname", &conn.host],
        )
        .await
        {
            Err(e) => {
                return AuthState::Missing(format!("glab is not installed on this host ({e:#})"));
            }
            Ok(o) => o,
        };
        if st.exit != 0 {
            return AuthState::SignedOut(format!(
                "local glab is not signed in to {} ({})",
                conn.host,
                first_line(&st, &conn.host, "not signed in")
            ));
        }
        let user = match run(
            exec,
            CliKind::Glab,
            &["api", "--hostname", &conn.host, "user"],
        )
        .await
        {
            Ok(o) if o.exit == 0 => serde_json::from_str::<Value>(&o.stdout).unwrap_or_default(),
            Ok(o) => {
                return AuthState::SignedOut(format!(
                    "glab cannot read {} ({})",
                    conn.host,
                    first_line(&o, "", "rejected")
                ));
            }
            Err(e) => return AuthState::SignedOut(format!("{e:#}")),
        };
        let login = user["username"].as_str().unwrap_or("").to_string();
        // A PAT reports its scopes and expiry; glab's OAuth token answers 401/404.
        let (missing, expires_at) = match run(
            exec,
            CliKind::Glab,
            &[
                "api",
                "--hostname",
                &conn.host,
                "personal_access_tokens/self",
            ],
        )
        .await
        {
            Ok(o) if o.exit == 0 => {
                let v: Value = serde_json::from_str(&o.stdout).unwrap_or_default();
                gitlab_token_facts(&v)
            }
            _ => (Vec::new(), None),
        };
        AuthState::SignedIn {
            login,
            missing,
            expires_at,
        }
    }

    fn methods(&self, probe: &CliProbe, conn: &Connection) -> Vec<SignInMethod> {
        let mut m = Vec::new();
        // `--device` needs GitLab 17.9: hidden when the server says it is
        // older; offered on the CLI's word when the version is unknown.
        let server_ok = probe
            .server
            .as_ref()
            .and_then(|s| s.device_flow())
            .unwrap_or(true);
        if probe.has("device") && server_ok {
            m.push(SignInMethod::DeviceCode);
        }
        if probe.has("web") && (conn.host == "gitlab.com" || probe.has("client_id")) {
            m.push(SignInMethod::BrowserLoopback);
        }
        m.push(SignInMethod::Token);
        m
    }

    fn interactive(&self, m: SignInMethod, conn: &Connection) -> Spawn {
        let flag = if m == SignInMethod::DeviceCode {
            "--device"
        } else {
            "--web"
        };
        spawn(
            CliKind::Glab,
            crate::args([
                "auth",
                "login",
                "--hostname",
                &conn.host,
                flag,
                "--git-protocol",
                &conn.git_protocol,
            ]),
        )
    }

    fn token(&self, conn: &Connection) -> (Spawn, TokenInput) {
        (
            spawn(
                CliKind::Glab,
                crate::args([
                    "auth",
                    "login",
                    "--hostname",
                    &conn.host,
                    "--git-protocol",
                    &conn.git_protocol,
                    "--stdin",
                ]),
            ),
            TokenInput::Stdin,
        )
    }

    fn token_page(&self, conn: &Connection) -> TokenPage {
        TokenPage {
            url: format!(
                "https://{}/-/user_settings/personal_access_tokens?name=Keleustes&scopes={}",
                conn.host,
                GITLAB_SCOPES.join(",")
            ),
            scopes: GITLAB_SCOPES.to_vec(),
            prefilled: true,
            expiry_required: true,
        }
    }

    fn progress(&self, line: &str) -> Option<Progress> {
        let l = line.to_ascii_lowercase();
        if l.contains("logged in as") {
            let who = line
                .split_whitespace()
                .skip_while(|w| !w.eq_ignore_ascii_case("as"))
                .nth(1)
                .unwrap_or("")
                .trim_matches(['.', ',']);
            return Some(Progress::Done(who.to_string()));
        }
        if l.starts_with("error") || l.contains("failed") {
            return Some(Progress::Failed(line.trim().to_string()));
        }
        if let Some(c) = generic_code(line) {
            return Some(Progress::Code {
                code: c,
                url: device_url(line).unwrap_or_default(),
            });
        }
        device_url(line).map(Progress::OpenUrl)
    }

    fn git_helper(&self, _conn: &Connection) -> String {
        format!("!{} auth git-credential", CliKind::Glab.program())
    }
}

/// Missing scopes (only `api` matters) and expiry from `personal_access_tokens/self`.
pub fn gitlab_token_facts(v: &Value) -> (Vec<String>, Option<i64>) {
    let have: Vec<&str> = v["scopes"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing = if have.contains(&"api") {
        Vec::new()
    } else {
        vec!["api".to_string()]
    };
    let exp = v["expires_at"].as_str().and_then(|d| {
        format!("{d}T00:00:00Z")
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_millisecond())
    });
    (missing, exp)
}

// ---- tea ---------------------------------------------------------------------

pub struct TeaAuth;

/// One tea login (`tea login list -o json`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TeaLogin {
    pub name: String,
    pub url: String,
    pub user: String,
}

fn field<'a>(v: &'a Value, k: &str) -> &'a str {
    v.as_object()
        .and_then(|o| {
            o.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(k))
                .and_then(|(_, x)| x.as_str())
        })
        .unwrap_or("")
}

/// The logins tea lists (field names in any case).
pub fn tea_logins(json: &str) -> Vec<TeaLogin> {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|e| TeaLogin {
                    name: field(e, "name").into(),
                    url: field(e, "url").into(),
                    user: field(e, "user").into(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tea login whose URL is on `host`.
pub fn tea_login_for<'a>(logins: &'a [TeaLogin], host: &str) -> Option<&'a TeaLogin> {
    logins
        .iter()
        .find(|l| super::kind::normalize_host(&l.url).as_deref() == Some(host))
}

impl TeaAuth {
    /// The tea login name for a connection: an existing login for that URL
    /// under any name, else the connection's own name.
    pub async fn login_name(exec: &dyn Exec, conn: &Connection) -> String {
        if let Ok(o) = run(exec, CliKind::Tea, &["login", "list", "-o", "json"]).await
            && let Some(l) = tea_login_for(&tea_logins(&o.stdout), &conn.host)
        {
            return l.name.clone();
        }
        if conn.cli_name.is_empty() {
            conn.host.clone()
        } else {
            conn.cli_name.clone()
        }
    }
}

#[async_trait::async_trait]
impl CliAuth for TeaAuth {
    fn cli(&self) -> CliKind {
        CliKind::Tea
    }

    async fn probe(&self, exec: &dyn Exec, _host: &str) -> CliProbe {
        let mut p = version(exec, CliKind::Tea).await;
        if !p.installed {
            return p;
        }
        if let Ok(o) = run(exec, CliKind::Tea, &["--help"]).await {
            let help = format!("{}{}", o.stdout, o.stderr);
            if help.lines().any(|l| l.trim_start().starts_with("api")) {
                p.flags.insert("api".into());
            }
        }
        if let Ok(o) = run(exec, CliKind::Tea, &["login", "add", "--help"]).await {
            let help = format!("{}{}", o.stdout, o.stderr);
            if help.contains("--oauth") || help.contains("-o, --oauth") {
                p.flags.insert("oauth".into());
            }
            if tea_takes_token_env(&help) {
                p.flags.insert("token_env".into());
            }
        }
        p
    }

    async fn status(&self, exec: &dyn Exec, conn: &Connection) -> AuthState {
        let list = match run(exec, CliKind::Tea, &["login", "list", "-o", "json"]).await {
            Err(e) => {
                return AuthState::Missing(format!("tea is not installed on this host ({e:#})"));
            }
            Ok(o) => o,
        };
        let logins = tea_logins(&list.stdout);
        let Some(l) = tea_login_for(&logins, &conn.host) else {
            return AuthState::SignedOut(format!("tea has no login for {}", conn.host));
        };
        match run(exec, CliKind::Tea, &["api", "-l", &l.name, "/user"]).await {
            Ok(o) if o.exit == 0 => {
                let v: Value = serde_json::from_str(&o.stdout).unwrap_or_default();
                let login = v["login"].as_str().unwrap_or(&l.user).to_string();
                AuthState::SignedIn {
                    login,
                    missing: Vec::new(),
                    expires_at: None,
                }
            }
            Ok(o)
                if format!("{}{}", o.stderr, o.stdout).contains("No help topic")
                    || format!("{}{}", o.stderr, o.stdout).contains("Command not found")
                    || format!("{}{}", o.stderr, o.stdout).contains("unknown command") =>
            {
                AuthState::TooOld("this tea has no `api` command; update tea".into())
            }
            Ok(o) => AuthState::SignedOut(format!(
                "tea login {} does not work ({})",
                l.name,
                first_line(&o, "", "rejected")
            )),
            Err(e) => AuthState::SignedOut(format!("{e:#}")),
        }
    }

    fn methods(&self, probe: &CliProbe, _conn: &Connection) -> Vec<SignInMethod> {
        let mut m = Vec::new();
        if probe.has("oauth") {
            m.push(SignInMethod::BrowserLoopback);
        }
        m.push(SignInMethod::Token);
        m
    }

    fn interactive(&self, _m: SignInMethod, conn: &Connection) -> Spawn {
        let name = if conn.cli_name.is_empty() {
            conn.host.clone()
        } else {
            conn.cli_name.clone()
        };
        spawn(
            CliKind::Tea,
            crate::args([
                "login",
                "add",
                "--name",
                &name,
                "--url",
                &conn.web_base(),
                "--oauth",
            ]),
        )
    }

    fn token(&self, conn: &Connection) -> (Spawn, TokenInput) {
        let name = if conn.cli_name.is_empty() {
            conn.host.clone()
        } else {
            conn.cli_name.clone()
        };
        (
            spawn(
                CliKind::Tea,
                crate::args(["login", "add", "--name", &name, "--url", &conn.web_base()]),
            ),
            TokenInput::Env(TEA_TOKEN_ENV),
        )
    }

    fn token_page(&self, conn: &Connection) -> TokenPage {
        TokenPage {
            url: format!("https://{}/user/settings/applications", conn.host),
            scopes: GITEA_SCOPES.to_vec(),
            prefilled: false,
            expiry_required: false,
        }
    }

    fn progress(&self, line: &str) -> Option<Progress> {
        let t = line.trim();
        if t.contains("Redirect URL not registered") {
            return Some(Progress::Failed(
                "Redirect URL not registered: the instance disabled tea's sign-in app; use a token"
                    .into(),
            ));
        }
        if let Some(rest) = t.strip_prefix("Login as ")
            && t.contains("successful")
        {
            return Some(Progress::Done(
                rest.split_whitespace().next().unwrap_or("").to_string(),
            ));
        }
        if t.starts_with("Error") {
            return Some(Progress::Failed(
                t.trim_start_matches("Error: ").to_string(),
            ));
        }
        // A plain-http server (a test container) prints an http:// authorize URL.
        device_url(t)
            .or_else(|| {
                t.starts_with("http://")
                    .then(|| t.split_whitespace().next().unwrap_or("").to_string())
                    .filter(|u| u.contains("/login/oauth/authorize"))
            })
            .map(Progress::OpenUrl)
    }

    /// git appends the verb: `tea login helper get` (tea 0.16.0's help:
    /// "store, erase  No-op", "get  Return the stored token for a URL").
    fn git_helper(&self, _conn: &Connection) -> String {
        format!("!{} login helper", CliKind::Tea.program())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GhOutput;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Answers argv prefixes (joined by spaces) with canned output; records calls.
    #[derive(Default)]
    struct Canned {
        replies: HashMap<String, GhOutput>,
        missing: bool,
        calls: Mutex<Vec<String>>,
    }

    impl Canned {
        fn on(mut self, prefix: &str, stdout: &str, exit: i32) -> Self {
            self.replies.insert(
                prefix.into(),
                GhOutput {
                    stdout: stdout.into(),
                    stderr: String::new(),
                    exit,
                },
            );
            self
        }
    }

    #[async_trait::async_trait]
    impl Exec for Canned {
        async fn exec(
            &self,
            _p: &str,
            args: &[String],
            _s: Option<&str>,
        ) -> anyhow::Result<GhOutput> {
            if self.missing {
                anyhow::bail!("No such file or directory (os error 2)");
            }
            let line = args.join(" ");
            self.calls.lock().unwrap().push(line.clone());
            let best = self
                .replies
                .iter()
                .filter(|(k, _)| line.starts_with(k.as_str()))
                .max_by_key(|(k, _)| k.len())
                .map(|(_, v)| v.clone());
            Ok(best.unwrap_or(GhOutput {
                exit: 1,
                stderr: "unknown".into(),
                ..Default::default()
            }))
        }
    }

    fn conn(kind: ForgeKind, host: &str) -> Connection {
        Connection::new(kind, host, "test").unwrap()
    }

    #[test]
    fn gh_status_text_and_scopes_per_kind() {
        const STATUS: &str = "ghe.acme.dev\n  ✓ Logged in to ghe.acme.dev account ana (keyring)\n  - Token scopes: 'repo', 'workflow'\n";
        assert_eq!(gh_account(STATUS).as_deref(), Some("ana"));
        let m = gh_missing_scopes(STATUS, &scopes(ForgeKind::Ghes));
        assert!(!m.contains(&"copilot".to_string()));
        assert_eq!(m.len(), ALL_SCOPES.len() - 4);
        let c = conn(ForgeKind::Ghes, "ghe.acme.dev");
        let page = GhAuth.token_page(&c);
        assert!(
            page.url
                .starts_with("https://ghe.acme.dev/settings/tokens/new")
        );
        assert!(!page.url.contains("copilot"));
        let s = GhAuth.interactive(SignInMethod::DeviceCode, &c);
        assert!(
            s.args
                .windows(2)
                .any(|w| w == ["--hostname", "ghe.acme.dev"])
        );
        assert!(s.args.contains(&"--web".to_string()));
        assert_eq!(GhAuth.token(&c).1, TokenInput::Stdin);
    }

    #[tokio::test]
    async fn glab_probe_status_and_methods() {
        let ex = Canned::default()
            .on("--version", "glab 1.62.0 (2026-01-01)\n", 0)
            .on(
                "auth login --help",
                "  --device   Use device flow\n  --web  browser\n  --stdin  token\n",
                0,
            )
            .on("auth status", "gitlab.acme.dev\n  ✓ Logged in\n", 0)
            .on(
                "api --hostname gitlab.acme.dev user",
                r#"{"username":"ana"}"#,
                0,
            )
            .on(
                "api --hostname gitlab.acme.dev personal_access_tokens/self",
                r#"{"scopes":["read_api"],"expires_at":"2027-11-20"}"#,
                0,
            );
        let p = GlabAuth.probe(&ex, "gitlab.acme.dev").await;
        assert!(p.installed && p.has("device") && p.has("web") && !p.has("client_id"));
        assert_eq!(p.version, "1.62.0");
        let c = conn(ForgeKind::Gitlab, "gitlab.acme.dev");
        // Self-managed without a client id: no browser method.
        assert_eq!(
            GlabAuth.methods(&p, &c),
            vec![SignInMethod::DeviceCode, SignInMethod::Token]
        );
        // The server probe decides device flow: GitLab 17.9 or later only.
        let with = |v: &str| CliProbe {
            server: Some(crate::forge::server::parse(
                ForgeKind::Gitlab,
                &serde_json::json!({ "version": v }),
            )),
            ..p.clone()
        };
        assert_eq!(
            GlabAuth.methods(&with("17.8.3-ee"), &c),
            vec![SignInMethod::Token]
        );
        assert_eq!(
            GlabAuth.methods(&with("17.9.0"), &c),
            vec![SignInMethod::DeviceCode, SignInMethod::Token]
        );
        let unknown = CliProbe {
            server: Some(crate::forge::server::from_http(
                ForgeKind::Gitlab,
                401,
                &Value::Null,
            )),
            ..p.clone()
        };
        assert_eq!(
            GlabAuth.methods(&unknown, &c),
            vec![SignInMethod::DeviceCode, SignInMethod::Token]
        );
        let com = conn(ForgeKind::Gitlab, "gitlab.com");
        assert!(
            GlabAuth
                .methods(&p, &com)
                .contains(&SignInMethod::BrowserLoopback)
        );
        match GlabAuth.status(&ex, &c).await {
            AuthState::SignedIn {
                login,
                missing,
                expires_at,
            } => {
                assert_eq!(login, "ana");
                assert_eq!(missing, vec!["api".to_string()]);
                assert!(expires_at.unwrap() > 0);
            }
            s => panic!("{s:?}"),
        }
        let out = Canned::default().on("auth status", "", 1);
        assert!(matches!(
            GlabAuth.status(&out, &c).await,
            AuthState::SignedOut(_)
        ));
        let gone = Canned {
            missing: true,
            ..Default::default()
        };
        assert!(matches!(
            GlabAuth.status(&gone, &c).await,
            AuthState::Missing(_)
        ));
        let (s, input) = GlabAuth.token(&c);
        assert_eq!(input, TokenInput::Stdin);
        assert!(s.args.contains(&"--stdin".to_string()));
        assert!(GlabAuth.token_page(&c).prefilled);
    }

    #[test]
    fn glab_progress_reads_code_url_and_done() {
        assert_eq!(
            GlabAuth.progress("Your one-time code: 7QX9-PL2M"),
            Some(Progress::Code {
                code: "7QX9-PL2M".into(),
                url: String::new()
            })
        );
        assert_eq!(
            GlabAuth.progress("Open https://gitlab.acme.dev/oauth/device to continue."),
            Some(Progress::OpenUrl(
                "https://gitlab.acme.dev/oauth/device".into()
            ))
        );
        assert_eq!(
            GlabAuth.progress("✓ Logged in as ana"),
            Some(Progress::Done("ana".into()))
        );
    }

    /// Recorded from glab 1.130.0, `glab auth login --hostname gitlab.com
    /// --device` (2026-11-28; the code expired unused). The code has no dash.
    #[test]
    fn glab_device_flow_recorded_wording() {
        let out = "- Signing into gitlab.com\n\nFirst copy your one-time code: 4OWWB4CH\n\
                   Then open this URL on any device to authorize: https://gitlab.com/oauth/device\n\n\
                   Waiting for authorization...\n";
        let got: Vec<Progress> = out.lines().filter_map(|l| GlabAuth.progress(l)).collect();
        assert_eq!(
            got,
            vec![
                Progress::Code {
                    code: "4OWWB4CH".into(),
                    url: String::new()
                },
                Progress::OpenUrl("https://gitlab.com/oauth/device".into()),
            ]
        );
    }

    #[tokio::test]
    async fn tea_status_from_login_list_and_api() {
        let list = r#"[{"Name":"cb","URL":"https://codeberg.org","User":"ana"}]"#;
        let ex = Canned::default()
            .on("--version", "Version: 0.10.1\n", 0)
            .on("--help", "COMMANDS:\n   api   Make an API request\n", 0)
            .on(
                "login add --help",
                "   --oauth, -o  Use interactive OAuth\n",
                0,
            )
            .on("login list -o json", list, 0)
            .on("api -l cb /user", r#"{"login":"ana"}"#, 0);
        let p = TeaAuth.probe(&ex, "codeberg.org").await;
        assert!(p.has("api") && p.has("oauth"));
        let c = conn(ForgeKind::Forgejo, "codeberg.org");
        assert_eq!(TeaAuth.status(&ex, &c).await.login(), "ana");
        assert_eq!(TeaAuth::login_name(&ex, &c).await, "cb");
        assert_eq!(
            TeaAuth.methods(&p, &c),
            vec![SignInMethod::BrowserLoopback, SignInMethod::Token]
        );
        let other = conn(ForgeKind::Gitea, "gitea.com");
        assert!(matches!(
            TeaAuth.status(&ex, &other).await,
            AuthState::SignedOut(_)
        ));
        let page = TeaAuth.token_page(&c);
        assert!(!page.prefilled && page.scopes.contains(&"write:repository"));
        assert_eq!(TeaAuth.token(&c).1, TokenInput::Env(TEA_TOKEN_ENV));
        assert!(tea_takes_token_env(
            "   --token value, -t value  Access token [$GITEA_SERVER_TOKEN]"
        ));
        assert!(!tea_takes_token_env(
            "   --token value, -t value  Access token"
        ));
    }

    #[test]
    fn tea_progress() {
        assert_eq!(
            TeaAuth.progress("https://codeberg.org/login/oauth/authorize?client_id=x"),
            Some(Progress::OpenUrl(
                "https://codeberg.org/login/oauth/authorize?client_id=x".into()
            ))
        );
        assert_eq!(
            TeaAuth.progress(
                "Login as ana on https://codeberg.org successful. Added this login as codeberg.org"
            ),
            Some(Progress::Done("ana".into()))
        );
        assert!(matches!(
            TeaAuth.progress("Error: Redirect URL not registered in Gitea"),
            Some(Progress::Failed(_))
        ));
        assert_eq!(
            TeaAuth.git_helper(&Connection::github_com()),
            format!("!{} login helper", CliKind::Tea.program())
        );
    }

    /// Recorded 2026-11-28: tea 0.16.0 `login add --oauth` against a stock
    /// Forgejo 13.0.5 (plain http), with tea's pre-registered client id,
    /// approved headlessly; then the 60 s timeout when the redirect URL is
    /// refused (Forgejo answers 400 in the browser; tea never hears why).
    #[test]
    fn tea_oauth_recorded_wording() {
        let lines = [
            ("Starting local server on 127.0.0.1:43207...", None),
            ("Opening browser for authorization...", None),
            (
                "Please authorize the application by visiting this URL in your browser:",
                None,
            ),
            (
                "http://localhost:3300/login/oauth/authorize?client_id=d57cb8c4-630c-4168-8324-ec79935e18d4&code_challenge=c&code_challenge_method=S256&redirect_uri=http%3A%2F%2F127.0.0.1%3A43207&response_type=code&scope=admin+user&state=s",
                Some(Progress::OpenUrl("http://localhost:3300/login/oauth/authorize?client_id=d57cb8c4-630c-4168-8324-ec79935e18d4&code_challenge=c&code_challenge_method=S256&redirect_uri=http%3A%2F%2F127.0.0.1%3A43207&response_type=code&scope=admin+user&state=s".into())),
            ),
            (
                "Failed to open browser:  exec: \"xdg-open\": executable file not found in $PATH",
                None,
            ),
            (
                "Login as e2e on http://localhost:3300 successful. Added this login as oauth-e2e",
                Some(Progress::Done("e2e".into())),
            ),
            (
                "Error: authorization failed: authentication timed out after 1m0s",
                Some(Progress::Failed(
                    "authorization failed: authentication timed out after 1m0s".into(),
                )),
            ),
        ];
        for (line, want) in lines {
            assert_eq!(TeaAuth.progress(line), want, "{line}");
        }
        // A plain http link that is not the authorize page is not an Open button.
        assert_eq!(TeaAuth.progress("http://localhost:3300/"), None);
    }
}
