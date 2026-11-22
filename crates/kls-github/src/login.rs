//! Signing a forge CLI in: through the browser (gh device code, glab
//! device code or browser, tea OAuth) or with a pasted personal access
//! token. github.com through `gh` is the default and unchanged.
//!
//! Keleustes still holds no forge token: every flow hands the credential to
//! the CLI, which stores it in its own keyring entry or config.

use crate::forge::auth::{GhAuth, Progress, SignInMethod, TokenInput, auth_for};
use crate::forge::{CliKind, Connection};
use anyhow::{Context, Result, bail};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const HOST: &str = "github.com";

/// Every classic OAuth scope. Parent scopes (`repo`, `admin:org`, `user`, …)
/// include their children.
pub const ALL_SCOPES: &[&str] = &[
    "repo",
    "workflow",
    "write:packages",
    "delete:packages",
    "admin:org",
    "admin:public_key",
    "admin:repo_hook",
    "admin:org_hook",
    "gist",
    "notifications",
    "user",
    "delete_repo",
    "write:discussion",
    "admin:enterprise",
    "audit_log",
    "codespace",
    "copilot",
    "project",
    "admin:gpg_key",
    "admin:ssh_signing_key",
];

/// Where device codes are entered.
pub const DEVICE_URL: &str = "https://github.com/login/device";

/// Device codes expire after 15 minutes.
const WEB_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The "new classic token" page with the description and every scope ticked.
pub fn token_url() -> String {
    format!(
        "https://github.com/settings/tokens/new?description=Keleustes&scopes={}",
        ALL_SCOPES.join(",")
    )
}

/// `XXXX-XXXX` from a line `gh` printed.
pub fn device_code(line: &str) -> Option<String> {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|w| {
            let b = w.as_bytes();
            b.len() == 9
                && b[4] == b'-'
                && b.iter()
                    .enumerate()
                    .all(|(i, c)| i == 4 || c.is_ascii_uppercase() || c.is_ascii_digit())
        })
        .map(str::to_string)
}

/// The first `https://` URL in a line.
pub fn device_url(line: &str) -> Option<String> {
    let i = line.find("https://")?;
    let url: String = line[i..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    Some(url.trim_end_matches(['.', ',', ')']).to_string())
}

/// Progress of a browser sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebState {
    Starting,
    /// Enter `code` at `url`; the CLI is waiting for approval.
    Code {
        code: String,
        url: String,
    },
    /// The CLI opened (or asks to open) `url` and waits on a loopback
    /// callback until `until` (Unix ms; tea gives up after 60 s).
    Browser {
        url: String,
        until: i64,
    },
    Done,
    Failed(String),
    Cancelled,
}

impl WebState {
    pub fn finished(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled)
    }
}

/// A running browser sign-in, shared with whoever shows it.
#[derive(Clone)]
pub struct WebLogin {
    state: Arc<Mutex<WebState>>,
    cancel: Arc<tokio::sync::Notify>,
    refresh: bool,
    conn: Connection,
    method: SignInMethod,
}

impl Default for WebLogin {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(WebState::Starting)),
            cancel: Arc::default(),
            refresh: false,
            conn: Connection::github_com(),
            method: SignInMethod::DeviceCode,
        }
    }
}

/// How long the CLI may wait for the person.
fn timeout_for(cli: CliKind, method: SignInMethod) -> Duration {
    match (cli, method) {
        // tea's own authTimeout is 60 s; leave it room to say so.
        (CliKind::Tea, _) => Duration::from_secs(75),
        (CliKind::Glab, SignInMethod::BrowserLoopback) => Duration::from_secs(5 * 60),
        _ => WEB_TIMEOUT,
    }
}

impl WebLogin {
    /// `gh auth refresh`: adds the missing scopes to the signed-in account.
    pub fn refresh() -> Self {
        Self {
            refresh: true,
            ..Self::default()
        }
    }

    /// An interactive sign-in of `conn`'s CLI with `method` (`refresh`:
    /// add scopes to the gh account instead).
    pub fn for_conn(conn: &Connection, method: SignInMethod, refresh: bool) -> Self {
        Self {
            refresh: refresh && conn.cli() == CliKind::Gh,
            conn: conn.clone(),
            method,
            ..Self::default()
        }
    }

    pub fn is_refresh(&self) -> bool {
        self.refresh
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn method(&self) -> SignInMethod {
        self.method
    }

    pub fn state(&self) -> WebState {
        self.state.lock().unwrap().clone()
    }

    /// Stops the CLI (the process is killed).
    pub fn cancel(&self) {
        self.cancel.notify_one();
    }

    fn set(&self, s: WebState, on_change: &(dyn Fn() + Send + Sync)) {
        *self.state.lock().unwrap() = s;
        on_change();
    }

    /// Runs the CLI's sign-in to completion, updating the state as it
    /// prints the code or the URL and finishes. `program` is the CLI
    /// (`gh` for github.com). `on_change` runs after each update.
    pub async fn run(&self, program: &str, on_change: impl Fn() + Send + Sync) {
        let r = if self.conn.cli() == CliKind::Gh {
            self.drive_gh(program, &on_change).await
        } else {
            self.drive_other(program, &on_change).await
        };
        let end = match r {
            Ok(s) => s,
            Err(e) => WebState::Failed(format!("{e:#}")),
        };
        self.set(end, &on_change);
    }

    async fn drive_gh(
        &self,
        program: &str,
        on_change: &(dyn Fn() + Send + Sync),
    ) -> Result<WebState> {
        let host = self.conn.host.clone();
        let mut conn = self.conn.clone();
        let args = if self.refresh {
            GhAuth::refresh_args(&conn)
        } else {
            conn.git_protocol = git_protocol(program, &host).await;
            let mut a = GhAuth::login_args(&conn);
            a.push("--web".into());
            a
        };
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            // gh opens nothing itself; the window opens the device page.
            .env("GH_BROWSER", "true")
            .env("BROWSER", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("run {program}"))?;
        let mut stdin = child.stdin.take();
        let mut out = BufReader::new(child.stdout.take().context("gh stdout")?).lines();
        let mut err = BufReader::new(child.stderr.take().context("gh stderr")?).lines();
        let mut log = Vec::new();
        let mut code: Option<String> = None;
        let mut url = format!("https://{host}/login/device");
        let (mut out_open, mut err_open) = (true, true);
        let deadline = tokio::time::Instant::now() + WEB_TIMEOUT;
        while out_open || err_open {
            let line = tokio::select! {
                _ = self.cancel.notified() => {
                    let _ = child.kill().await;
                    return Ok(WebState::Cancelled);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = child.kill().await;
                    bail!("the one-time code expired; start again");
                }
                l = out.next_line(), if out_open => match l? { Some(l) => l, None => { out_open = false; continue } },
                l = err.next_line(), if err_open => match l? { Some(l) => l, None => { err_open = false; continue } },
            };
            if code.is_none() {
                if let Some(u) = device_url(&line) {
                    url = u;
                }
                if let Some(c) = device_code(&line) {
                    code = Some(c.clone());
                    self.set(
                        WebState::Code {
                            code: c,
                            url: url.clone(),
                        },
                        on_change,
                    );
                }
            } else if let Some(u) = device_url(&line)
                && let Some(c) = &code
            {
                url = u;
                self.set(
                    WebState::Code {
                        code: c.clone(),
                        url: url.clone(),
                    },
                    on_change,
                );
            }
            // Older gh waits for Enter before polling.
            if line.contains("Press Enter")
                && let Some(mut s) = stdin.take()
            {
                let _ = s.write_all(b"\n").await;
            }
            if !line.trim().is_empty() {
                log.push(line);
            }
        }
        let status = tokio::select! {
            _ = self.cancel.notified() => {
                let _ = child.kill().await;
                return Ok(WebState::Cancelled);
            }
            s = child.wait() => s?,
        };
        if status.success() {
            Ok(WebState::Done)
        } else {
            bail!("{}", last_words(&log, "gh auth login failed"))
        }
    }

    /// glab (device code or browser) and tea (OAuth in the browser).
    async fn drive_other(
        &self,
        program: &str,
        on_change: &(dyn Fn() + Send + Sync),
    ) -> Result<WebState> {
        let cli = self.conn.cli();
        let auth = auth_for(cli);
        let mut conn = self.conn.clone();
        if keeps_git_protocol(cli) {
            conn.git_protocol = git_protocol(program, &conn.host).await;
        }
        let spec = auth.interactive(self.method, &conn);
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&spec.args);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("run {program}"))?;
        let mut stdin = child.stdin.take();
        let mut out = BufReader::new(child.stdout.take().context("stdout")?).lines();
        let mut err = BufReader::new(child.stderr.take().context("stderr")?).lines();
        let wait = timeout_for(cli, self.method);
        let until = kls_store::now_ms() + wait.as_millis() as i64;
        let deadline = tokio::time::Instant::now() + wait;
        let (mut out_open, mut err_open) = (true, true);
        let mut log = Vec::new();
        let mut code: Option<String> = None;
        let mut failed: Option<String> = None;
        if self.method == SignInMethod::BrowserLoopback {
            self.set(
                WebState::Browser {
                    url: String::new(),
                    until,
                },
                on_change,
            );
        }
        while out_open || err_open {
            let line = tokio::select! {
                _ = self.cancel.notified() => {
                    let _ = child.kill().await;
                    return Ok(WebState::Cancelled);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = child.kill().await;
                    bail!("timed out waiting for approval in the browser; retry");
                }
                l = out.next_line(), if out_open => match l? { Some(l) => l, None => { out_open = false; continue } },
                l = err.next_line(), if err_open => match l? { Some(l) => l, None => { err_open = false; continue } },
            };
            match auth.progress(&line) {
                Some(Progress::Code { code: c, url }) => {
                    let url = if url.is_empty() {
                        format!("https://{}/oauth/device", self.conn.host)
                    } else {
                        url
                    };
                    code = Some(c.clone());
                    self.set(WebState::Code { code: c, url }, on_change);
                }
                Some(Progress::OpenUrl(u)) => match &code {
                    Some(c) => self.set(
                        WebState::Code {
                            code: c.clone(),
                            url: u,
                        },
                        on_change,
                    ),
                    None => self.set(WebState::Browser { url: u, until }, on_change),
                },
                Some(Progress::PressEnter) => {
                    if let Some(mut s) = stdin.take() {
                        let _ = s.write_all(b"\n").await;
                    }
                }
                Some(Progress::Failed(why)) => failed = Some(why),
                Some(Progress::Done(_)) | None => {}
            }
            if !line.trim().is_empty() {
                log.push(line);
            }
        }
        let status = tokio::select! {
            _ = self.cancel.notified() => {
                let _ = child.kill().await;
                return Ok(WebState::Cancelled);
            }
            s = child.wait() => s?,
        };
        if let Some(why) = failed {
            bail!("{why}");
        }
        if status.success() {
            Ok(WebState::Done)
        } else {
            bail!(
                "{}",
                last_words(&log, &format!("{} sign-in failed", cli.name()))
            )
        }
    }
}

/// Signs `gh` in to github.com with a personal access token (read from
/// standard input by `gh`).
pub async fn login_with_token(program: &str, token: &str) -> Result<()> {
    login_with_token_for(&Connection::github_com(), program, token).await
}

fn check_token(token: &str) -> Result<&str> {
    let token = token.trim();
    if token.is_empty() {
        bail!("paste a token first");
    }
    if token.chars().any(char::is_whitespace) {
        bail!("the token has spaces in it; copy it again");
    }
    Ok(token)
}

/// Refuses a tea whose `login add` cannot read the token from
/// `GITEA_SERVER_TOKEN`: the only other way is its argv, which other local
/// users can read.
async fn tea_token_env_check(program: &str) -> Result<()> {
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(program)
            .args(["login", "add", "--help"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("tea --help timed out")?
    .with_context(|| format!("run {program}"))?;
    let help =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if !crate::forge::auth::tea_takes_token_env(&help) {
        bail!(
            "this tea reads a token only from its command line; update tea or use the browser sign-in"
        );
    }
    Ok(())
}

/// Signs the connection's CLI in with a token. The token goes to the CLI
/// (stdin for gh and glab, `GITEA_SERVER_TOKEN` for tea; never the argv) and
/// is never kept, logged or put in an error.
pub async fn login_with_token_for(conn: &Connection, program: &str, token: &str) -> Result<()> {
    let token = check_token(token)?;
    let auth = auth_for(conn.cli());
    let mut conn = conn.clone();
    if keeps_git_protocol(conn.cli()) {
        conn.git_protocol = git_protocol(program, &conn.host).await;
    }
    let (spec, input) = auth.token(&conn);
    if conn.cli() == CliKind::Tea {
        tea_token_env_check(program).await?;
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&spec.args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    match input {
        TokenInput::Env(var) => {
            cmd.env(var, token);
        }
        TokenInput::Stdin => {}
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run {program}"))?;
    if let Some(mut s) = child.stdin.take() {
        if input == TokenInput::Stdin {
            s.write_all(format!("{token}\n").as_bytes()).await?;
        }
        drop(s);
    }
    let out = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .with_context(|| format!("{} sign-in timed out", conn.cli().name()))??;
    if out.status.success() {
        return Ok(());
    }
    let text = (String::from_utf8_lossy(&out.stderr).into_owned()
        + &String::from_utf8_lossy(&out.stdout))
        .replace(token, "[token]");
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    bail!(
        "{}",
        last_words(&lines, &format!("{} rejected the token", conn.cli().name()))
    )
}

/// `auth login` for github.com, keeping the git protocol `gh` already uses.
#[cfg(test)]
fn login_args(protocol: &str) -> Vec<String> {
    let mut c = Connection::github_com();
    c.git_protocol = protocol.into();
    GhAuth::login_args(&c)
}

/// CLIs whose `auth login` takes `--git-protocol` and that answer
/// `config get git_protocol --host <host>`: a sign-in keeps the protocol the
/// person already uses instead of resetting it.
fn keeps_git_protocol(cli: CliKind) -> bool {
    matches!(cli, CliKind::Gh | CliKind::Glab)
}

/// The CLI's git protocol for `host` (`https` when unset); gh and glab.
async fn git_protocol(program: &str, host: &str) -> String {
    let out = tokio::process::Command::new(program)
        .args(["config", "get", "git_protocol", "--host", host])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if p == "ssh" { p } else { "https".into() }
        }
        _ => "https".into(),
    }
}

fn last_words(lines: &[String], fallback: &str) -> String {
    lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches(['!', 'X', ' ']).to_string())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_code_from_both_gh_wordings() {
        assert_eq!(
            device_code("! One-time code (F2FB-231A) copied to clipboard").as_deref(),
            Some("F2FB-231A")
        );
        assert_eq!(
            device_code("! First copy your one-time code: 0A1B-C2D3").as_deref(),
            Some("0A1B-C2D3")
        );
        assert_eq!(device_code("Press Enter to open github.com"), None);
        assert_eq!(device_code("git-protocol https-only"), None);
    }

    #[test]
    fn parses_device_url() {
        assert_eq!(
            device_url(
                "Open this URL to continue in your web browser: https://github.com/login/device"
            )
            .as_deref(),
            Some(DEVICE_URL)
        );
        assert_eq!(
            device_url("Press Enter to open https://github.com/login/device in your browser...")
                .as_deref(),
            Some(DEVICE_URL)
        );
        assert_eq!(device_url("no link"), None);
    }

    #[test]
    fn token_page_ticks_every_scope() {
        let u = token_url();
        for s in ALL_SCOPES {
            assert!(u.contains(s), "{s}");
        }
        let args = login_args("https");
        assert_eq!(args.last().unwrap(), &ALL_SCOPES.join(","));
        assert!(args.contains(&"--skip-ssh-key".to_string()));
    }

    /// A fake `gh` that prints a device code, then waits for approval.
    #[cfg(unix)]
    fn fake_gh(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("gh");
        std::fs::write(
            &p,
            format!("#!/bin/sh\nif [ \"$1\" = config ]; then echo ssh; exit 0; fi\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn web_login_reports_code_then_done() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            "echo '! One-time code (ABCD-1234) copied to clipboard' >&2\n\
             echo 'Open this URL to continue in your web browser: https://github.com/login/device' >&2\n\
             sleep 0.3\necho '✓ Logged in as someone' >&2",
        );
        let login = WebLogin::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let l2 = login.clone();
        login
            .run(&gh, move || s2.lock().unwrap().push(l2.state()))
            .await;
        assert_eq!(login.state(), WebState::Done);
        assert!(seen.lock().unwrap().contains(&WebState::Code {
            code: "ABCD-1234".into(),
            url: DEVICE_URL.into()
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn web_login_cancel_kills_gh() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "echo 'code: WXYZ-9876' >&2\nsleep 30");
        let login = WebLogin::default();
        let l2 = login.clone();
        let t = tokio::spawn(async move { l2.run(&gh, || {}).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        login.cancel();
        tokio::time::timeout(Duration::from_secs(5), t)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(login.state(), WebState::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_login_passes_token_on_stdin_and_reports_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("stdin");
        let gh = fake_gh(
            dir.path(),
            &format!(
                "cat > {}\nif grep -q good {}; then exit 0; fi\necho 'error validating token: HTTP 401' >&2\nexit 1",
                seen.display(),
                seen.display()
            ),
        );
        login_with_token(&gh, "  good_token \n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), "good_token\n");
        let e = login_with_token(&gh, "bad").await.unwrap_err();
        assert!(format!("{e}").contains("401"), "{e}");
        assert!(login_with_token(&gh, "a b").await.is_err());
        assert!(login_with_token(&gh, " ").await.is_err());
    }

    /// A fake CLI script (any name) that runs `body`.
    #[cfg(unix)]
    fn fake_cli(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn conn(kind: crate::ForgeKind, host: &str) -> Connection {
        Connection::new(kind, host, "test").unwrap()
    }

    /// tea gives up on its OAuth callback after 60 s: Keleustes waits longer
    /// (so tea's own words reach the person) and reports tea's timeout as a
    /// failure, never as a sign-in.
    #[cfg(unix)]
    #[tokio::test]
    async fn tea_oauth_60s_timeout_fails_with_teas_words() {
        assert!(timeout_for(CliKind::Tea, SignInMethod::BrowserLoopback) > Duration::from_secs(60));
        let dir = tempfile::tempdir().unwrap();
        let tea = fake_cli(
            dir.path(),
            "tea-timeout",
            "echo 'Please authorize the application by visiting this URL in your browser:'\necho 'https://codeberg.org/login/oauth/authorize?client_id=d57cb8c4'\necho 'Error: timed out waiting for the authorization callback' >&2\nexit 1",
        );
        let cb = conn(crate::ForgeKind::Forgejo, "codeberg.org");
        let login = WebLogin::for_conn(&cb, SignInMethod::BrowserLoopback, false);
        login.run(&tea, || {}).await;
        match login.state() {
            WebState::Failed(why) => assert!(why.contains("timed out"), "{why}"),
            s => panic!("{s:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn glab_device_reports_code_and_tea_oauth_reports_url() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        let glab = fake_cli(
            dir.path(),
            "glab",
            &format!(
                "echo \"$@\" > {}\necho 'Your one-time code: 7QX9-PL2M'\necho 'Open https://gitlab.acme.dev/oauth/device to continue'\nsleep 0.3\necho 'Logged in as ana'",
                args.display()
            ),
        );
        let c = conn(crate::ForgeKind::Gitlab, "gitlab.acme.dev");
        let login = WebLogin::for_conn(&c, SignInMethod::DeviceCode, false);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (s2, l2) = (seen.clone(), login.clone());
        login
            .run(&glab, move || s2.lock().unwrap().push(l2.state()))
            .await;
        assert_eq!(login.state(), WebState::Done);
        assert!(seen.lock().unwrap().contains(&WebState::Code {
            code: "7QX9-PL2M".into(),
            url: "https://gitlab.acme.dev/oauth/device".into()
        }));
        let argv = std::fs::read_to_string(&args).unwrap();
        assert!(
            argv.contains("--hostname gitlab.acme.dev --device --git-protocol https"),
            "{argv}"
        );

        let tea = fake_cli(
            dir.path(),
            "tea",
            "echo 'Please authorize the application by visiting this URL in your browser:'\necho 'https://codeberg.org/login/oauth/authorize?client_id=d57cb8c4'\nsleep 0.2\necho 'Login as ana on https://codeberg.org successful. Added this login as codeberg.org'",
        );
        let cb = conn(crate::ForgeKind::Forgejo, "codeberg.org");
        let login = WebLogin::for_conn(&cb, SignInMethod::BrowserLoopback, false);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (s2, l2) = (seen.clone(), login.clone());
        login
            .run(&tea, move || s2.lock().unwrap().push(l2.state()))
            .await;
        assert_eq!(login.state(), WebState::Done);
        assert!(seen.lock().unwrap().iter().any(|s| matches!(s,
            WebState::Browser { url, until } if url.starts_with("https://codeberg.org/login/oauth") && *until > 0)));

        let refused = fake_cli(
            dir.path(),
            "tea-refused",
            "echo 'Opening browser for authorization...'\necho 'Error: Redirect URL not registered in Gitea'\nexit 1",
        );
        let login = WebLogin::for_conn(&cb, SignInMethod::BrowserLoopback, false);
        login.run(&refused, || {}).await;
        match login.state() {
            WebState::Failed(why) => assert!(why.contains("use a token"), "{why}"),
            s => panic!("{s:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_reaches_glab_on_stdin_and_tea_errors_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("stdin");
        let argv = dir.path().join("argv");
        let glab = fake_cli(
            dir.path(),
            "glab",
            &format!(
                "echo \"$@\" > {}\ncat > {}\nexit 0",
                argv.display(),
                seen.display()
            ),
        );
        let c = conn(crate::ForgeKind::Gitlab, "gitlab.com");
        login_with_token_for(&c, &glab, "glpat-secret")
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), "glpat-secret\n");
        let a = std::fs::read_to_string(&argv).unwrap();
        assert!(!a.contains("glpat-secret") && a.contains("--stdin"), "{a}");

        // tea: the token rides in GITEA_SERVER_TOKEN, never the argv.
        let tea_argv = dir.path().join("tea-argv");
        let tea_env = dir.path().join("tea-env");
        let tea = fake_cli(
            dir.path(),
            "tea",
            &format!(
                "if [ \"$3\" = --help ]; then echo '   --token value, -t value  Access token [$GITEA_SERVER_TOKEN]'; exit 0; fi\n\
                 echo \"$@\" > {}\necho \"$GITEA_SERVER_TOKEN\" > {}\n\
                 if [ \"$GITEA_SERVER_TOKEN\" = tok_good ]; then exit 0; fi\n\
                 echo \"Error: token $GITEA_SERVER_TOKEN is invalid (401)\" >&2\nexit 1",
                tea_argv.display(),
                tea_env.display()
            ),
        );
        let cb = conn(crate::ForgeKind::Forgejo, "codeberg.org");
        login_with_token_for(&cb, &tea, "tok_good").await.unwrap();
        let a = std::fs::read_to_string(&tea_argv).unwrap();
        assert!(
            !a.contains("tok_good") && !a.contains("--token") && a.contains("login add"),
            "{a}"
        );
        assert_eq!(std::fs::read_to_string(&tea_env).unwrap(), "tok_good\n");
        let e = login_with_token_for(&cb, &tea, "tok_123")
            .await
            .unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("401") && !msg.contains("tok_123"), "{msg}");

        // An old tea with no env support is refused before the token goes anywhere.
        let old_argv = dir.path().join("old-argv");
        let old = fake_cli(
            dir.path(),
            "tea-old",
            &format!(
                "if [ \"$3\" = --help ]; then echo '   --token value  Access token'; exit 0; fi\necho \"$@\" > {}\nexit 0",
                old_argv.display()
            ),
        );
        let e = login_with_token_for(&cb, &old, "tok_good")
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("update tea"), "{e:#}");
        assert!(!old_argv.exists());
        assert!(
            login_with_token_for(&cb, "/nonexistent/tea", "x")
                .await
                .is_err()
        );
    }
}
