//! Kiro credentials (web `VaultEntryDialog.vue:189-207`, vdb-site
//! docs/secrets-vault.md "Kiro").
//!
//! Kiro has two kinds of credential, filed under provider [`PROVIDER`]:
//!
//! - A **sign-in** ([`LOGIN_ENV`], `KIRO_LOGIN`): an AWS SSO-OIDC refresh
//!   token and its client registration, as JSON. belai's own `kiro`
//!   provider reads it from `KIRO_LOGIN` (belai
//!   `internal/provider/registry.go`, `internal/kiroauth`) and mints an
//!   access token per request. `belai login kiro` and
//!   `vulnetix ai-firewall kiro login` write it.
//! - An **API key** ([`API_KEY_ENV`], `KIRO_API_KEY`, `ksk_...`): what
//!   `kiro-cli` reads in headless mode (kiro.dev/docs/getting-started/
//!   authentication). The kiro-cli harness lists it in `grant_env`, so a
//!   grant hands it to that harness the way other harness keys go.
//!
//! A pasted or opened sign-in is checked ([`parse_login`]), re-encoded in
//! the shape belai's `kiroauth.ParseLogin` reads and sealed like any key.
//! GitHub and Google (social) Kiro sign-ins carry no client registration
//! and are refused. The test of a stored sign-in mints an access token at
//! `oidc.<region>.amazonaws.com`; a refresh token AWS rotates on the way is
//! sealed as the next version (actor `kiro-refresh`). Values never appear
//! in an event, a log line, an error or the ledger.

use super::{ProbeResult, ProbeState, SecretMeta, SecretString, kinds, vault};
use crate::Core;
use anyhow::{Result, anyhow, bail};
use kls_store::vault::Provider;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::time::Instant;

/// The provider slug Kiro credentials are filed under.
pub const PROVIDER: &str = "kiro";
/// belai's kiro provider reads the sign-in JSON from this variable.
pub const LOGIN_ENV: &str = "KIRO_LOGIN";
/// kiro-cli reads its API key from this variable (headless mode).
pub const API_KEY_ENV: &str = "KIRO_API_KEY";
/// Largest sign-in (an API key may be 16 KiB; a sign-in is JSON).
pub const MAX_LOGIN_BYTES: usize = 8 * 1024;
/// Where a sign-in comes from, for the hint.
pub const LOGIN_HINT: &str = "belai login kiro, or vulnetix ai-firewall kiro login";

/// What the form shows of a checked sign-in: never a token or secret.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoginSummary {
    pub region: String,
    /// The Kiro API region (empty: the sign-in region).
    pub api_region: String,
    pub start_url: String,
    /// An IAM Identity Center sign-in (a profile ARN or a start URL other
    /// than the AWS Builder ID one).
    pub identity_center: bool,
    /// When the client registration stops working (ms), if it says.
    pub registration_expires_at: Option<i64>,
}

/// Whether `provider` / `env` hold a Kiro sign-in.
pub fn is_login(provider: &str, env: &str) -> bool {
    provider == PROVIDER && env.trim() == LOGIN_ENV
}

/// An AWS region name (belai `kiroauth.ValidRegion`):
/// `^[a-z]{2}(-gov|-iso[a-z]?)?-[a-z]+-[0-9]{1,2}$`. It keeps a pasted
/// region from steering the pinned hosts anywhere else.
pub fn valid_region(r: &str) -> bool {
    let parts: Vec<&str> = r.split('-').collect();
    let lower = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase());
    let num = |s: &str| (1..=2).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    let partition = |s: &str| {
        s == "gov"
            || (s.starts_with("iso") && s.len() <= 4 && s.chars().all(|c| c.is_ascii_lowercase()))
    };
    match parts.as_slice() {
        [a, b, n] => a.len() == 2 && lower(a) && lower(b) && num(n),
        [a, p, b, n] => a.len() == 2 && lower(a) && partition(p) && lower(b) && num(n),
        _ => false,
    }
}

fn text(o: &Map<String, Value>, keys: &[&str]) -> Result<String, String> {
    for k in keys {
        match o.get(*k) {
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) => return Ok(s.trim().to_string()),
            Some(_) => return Err(format!("{} is not text", keys[0])),
        }
    }
    Ok(String::new())
}

/// An expiry written as Unix seconds (or ms), or RFC 3339 text; seconds.
fn expiry_secs(v: Option<&Value>) -> Result<i64, String> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => {
            let n = n.as_i64().ok_or("the registration expiry is not a time")?;
            Ok(if n > 100_000_000_000 { n / 1000 } else { n })
        }
        Some(Value::String(s)) if s.trim().is_empty() => Ok(0),
        Some(Value::String(s)) => s
            .trim()
            .parse::<jiff::Timestamp>()
            .map(|t| t.as_second())
            .map_err(|_| "the registration expiry is not a time".to_string()),
        Some(_) => Err("the registration expiry is not a time".into()),
    }
}

fn first<'a>(o: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| o.get(*k).filter(|v| !v.is_null()))
}

/// Checks a Kiro sign-in and re-encodes it in belai's `kiroauth.Login`
/// shape (snake case, expiry in Unix seconds, compact). Takes belai's own
/// form and the camel-case names of the AWS SSO cache. `now_ms` decides
/// whether the client registration has expired. Errors never quote a value.
pub fn parse_login(raw: &str, now_ms: i64) -> Result<(SecretString, LoginSummary), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("paste the Kiro sign-in".into());
    }
    if raw.len() > MAX_LOGIN_BYTES {
        return Err(format!("a Kiro sign-in is at most {MAX_LOGIN_BYTES} bytes"));
    }
    let v: Value =
        serde_json::from_str(raw).map_err(|_| format!("a Kiro sign-in is JSON ({LOGIN_HINT})"))?;
    let o = v
        .as_object()
        .ok_or_else(|| format!("a Kiro sign-in is a JSON object ({LOGIN_HINT})"))?;
    let social = text(o, &["provider", "authMethod", "auth_method"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(social.as_str(), "github" | "google" | "social") {
        return Err("GitHub and Google Kiro sign-ins cannot be used".into());
    }
    let refresh = text(o, &["refresh_token", "refreshToken"])?;
    let client_id = text(o, &["client_id", "clientId"])?;
    let client_secret = text(o, &["client_secret", "clientSecret"])?;
    if refresh.is_empty() || client_id.is_empty() || client_secret.is_empty() {
        return Err(
            "the sign-in needs refresh_token, client_id and client_secret; GitHub and Google Kiro sign-ins cannot be used"
                .into(),
        );
    }
    let mut region = text(o, &["region"])?;
    if region.is_empty() {
        region = "us-east-1".into();
    }
    if !valid_region(&region) {
        return Err("the sign-in's region is not an AWS region".into());
    }
    let api_region = text(o, &["api_region", "apiRegion"])?;
    if !api_region.is_empty() && !valid_region(&api_region) {
        return Err("the sign-in's api_region is not an AWS region".into());
    }
    let start_url = text(o, &["start_url", "startUrl"])?;
    if !start_url.is_empty()
        && (!start_url.starts_with("https://")
            || start_url.len() > 512
            || start_url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err("the sign-in's start_url is not an https address".into());
    }
    let profile_arn = text(o, &["profile_arn", "profileArn"])?;
    if !profile_arn.is_empty()
        && (!profile_arn.starts_with("arn:aws:codewhisperer:") || profile_arn.len() > 256)
    {
        return Err("the sign-in's profile_arn is not a CodeWhisperer profile".into());
    }
    let expires = expiry_secs(first(
        o,
        &[
            "client_secret_expires_at",
            "clientSecretExpiresAt",
            "registrationExpiresAt",
        ],
    ))?;
    if expires > 0 && expires * 1000 <= now_ms {
        return Err(format!(
            "the client registration expired; sign in again ({LOGIN_HINT})"
        ));
    }
    let mut out = Map::new();
    out.insert("refresh_token".into(), json!(refresh));
    out.insert("client_id".into(), json!(client_id));
    out.insert("client_secret".into(), json!(client_secret));
    if expires > 0 {
        out.insert("client_secret_expires_at".into(), json!(expires));
    }
    out.insert("region".into(), json!(region));
    for (k, v) in [
        ("start_url", &start_url),
        ("api_region", &api_region),
        ("profile_arn", &profile_arn),
    ] {
        if !v.is_empty() {
            out.insert(k.into(), json!(v));
        }
    }
    let encoded = SecretString::new(Value::Object(out).to_string());
    drop((refresh, client_secret));
    let summary = LoginSummary {
        identity_center: !profile_arn.is_empty()
            || (!start_url.is_empty() && start_url != "https://view.awsapps.com/start"),
        region,
        api_region,
        start_url,
        registration_expires_at: (expires > 0).then_some(expires * 1000),
    };
    Ok((encoded, summary))
}

/// The form's inline rule: "" when `raw` is a usable sign-in.
pub fn login_rule(raw: &str) -> String {
    parse_login(raw, kls_store::now_ms())
        .err()
        .unwrap_or_default()
}

/// Reads a sign-in from a file (the "Open" button and a dropped file):
/// at most [`MAX_LOGIN_BYTES`], text only. The bytes are moved into a
/// zeroizing buffer at once.
pub fn read_login_file(path: &std::path::Path) -> Result<SecretString> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| anyhow!("open {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take(MAX_LOGIN_BYTES as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > MAX_LOGIN_BYTES {
        bail!("a Kiro sign-in is at most {MAX_LOGIN_BYTES} bytes");
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(SecretString::new(s)),
        Err(_) => bail!("that file is not text"),
    }
}

/// The catalogue entry for Kiro, added the first time a Kiro credential is
/// saved (belai's kiro provider models, `internal/provider/registry.go`).
pub fn provider() -> Provider {
    Provider {
        id: PROVIDER.into(),
        name: "Kiro".into(),
        env: LOGIN_ENV.into(),
        default_model: "claude-sonnet-4.5".into(),
        fast_model: "claude-haiku-4.5".into(),
        models: [
            "auto",
            "claude-sonnet-4.5",
            "claude-sonnet-4",
            "claude-haiku-4.5",
            "claude-opus-4.5",
        ]
        .map(String::from)
        .to_vec(),
        factory: true,
        ..Default::default()
    }
}

async fn ensure_provider(core: &Core) -> Result<()> {
    if core.store().provider(PROVIDER).await?.is_none() {
        super::save_provider(core, &provider()).await?;
    }
    Ok(())
}

/// Seals a Kiro sign-in under `KIRO_LOGIN` (replacing the one held), after
/// [`parse_login`]. Audited like any key save, with `source` naming where
/// it came from ("paste" or "file"); the summary is returned for the form.
pub async fn put_login(
    core: &Core,
    label: &str,
    raw: SecretString,
    source: &str,
) -> Result<(SecretMeta, LoginSummary)> {
    let (value, summary) =
        parse_login(raw.expose(), kls_store::now_ms()).map_err(|e| anyhow!(e))?;
    drop(raw);
    ensure_provider(core).await?;
    let label = if label.trim().is_empty() {
        "kiro-signin"
    } else {
        label
    };
    let m = super::put_secret(core, PROVIDER, LOGIN_ENV, label, value).await?;
    record_import(core, &m, &summary, source).await;
    Ok((m, summary))
}

/// Seals a kiro-cli API key under `KIRO_API_KEY`.
pub async fn put_api_key(core: &Core, label: &str, value: SecretString) -> Result<SecretMeta> {
    let v = value.expose().trim();
    if v.is_empty() || v.starts_with('{') {
        bail!("a Kiro API key is one line (ksk_…); a sign-in goes under Sign-in");
    }
    ensure_provider(core).await?;
    let label = if label.trim().is_empty() {
        "kiro-cli"
    } else {
        label
    };
    super::put_secret(core, PROVIDER, API_KEY_ENV, label, value).await
}

/// The provenance of an imported sign-in, on the hash-chained ledger: who,
/// from where, which region and fingerprint. Never a token.
async fn record_import(core: &Core, m: &SecretMeta, s: &LoginSummary, source: &str) {
    let d = kls_store::audit::LedgerDraft {
        at: 0,
        kind: "vault".into(),
        actor: "owner".into(),
        actor_kind: "user".into(),
        msg: format!("kiro sign-in imported ({source})"),
        subject: json!({ "secretId": m.id, "provider": PROVIDER, "env": LOGIN_ENV }),
        payload: json!({
            "action": "kiro_signin_imported", "source": source, "region": s.region,
            "apiRegion": s.api_region, "identityCenter": s.identity_center,
            "fingerprint": m.fingerprint, "version": m.version,
        }),
        origin: format!("vault:{source}"),
        node_id: String::new(),
        session_id: String::new(),
        ref_key: Some(format!("kiro-import:{}:{}", m.id, m.version)),
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(secret = %m.id, "kiro import audit: {e:#}");
    }
}

/// The SSO-OIDC endpoint for `region`.
pub fn oidc_base(region: &str) -> String {
    format!("https://oidc.{region}.amazonaws.com")
}

/// Mints an access token from a stored sign-in at `base` (the SSO-OIDC
/// endpoint; a test passes its own). Accepted on 2xx; refused on 4xx.
/// Returns the rotated sign-in when AWS sent a new refresh token.
pub async fn mint(value: &SecretString, base: &str) -> (ProbeResult, Option<SecretString>) {
    let at = kls_store::now_ms();
    let fail = |state, detail: &str| ProbeResult {
        state,
        detail: detail.into(),
        at,
        ..Default::default()
    };
    let Ok(v) = serde_json::from_str::<Value>(value.expose()) else {
        return (
            fail(ProbeState::Refused, "the stored sign-in is not JSON"),
            None,
        );
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let client = match reqwest::Client::builder()
        .timeout(super::probe::TIMEOUT)
        .connect_timeout(super::probe::TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                fail(ProbeState::Unreachable, &format!("http client: {e}")),
                None,
            );
        }
    };
    let body = json!({
        "clientId": s("client_id"), "clientSecret": s("client_secret"),
        "grantType": "refresh_token", "refreshToken": s("refresh_token"),
    });
    let started = Instant::now();
    let res = client
        .post(format!("{}/token", base.trim_end_matches('/')))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await;
    drop(body);
    let ms = Some(started.elapsed().as_millis() as i64);
    let r = match res {
        Err(e) => {
            let why = if e.is_timeout() {
                "no answer in time".to_string()
            } else if e.is_connect() {
                "could not connect".to_string()
            } else {
                e.without_url().to_string()
            };
            return (
                ProbeResult {
                    ms,
                    ..fail(ProbeState::Unreachable, &why)
                },
                None,
            );
        }
        Ok(r) => r,
    };
    let status = r.status();
    let code = Some(status.as_u16() as i64);
    let text = r.text().await.unwrap_or_default();
    let answer: Value = serde_json::from_str(&text).unwrap_or_default();
    drop(text);
    if !status.is_success() {
        // The OIDC error code only (`invalid_grant`...), never the body.
        let why = answer
            .get("error")
            .and_then(Value::as_str)
            .filter(|e| e.len() <= 64 && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .map(|e| format!("{e}; sign in again"))
            .unwrap_or_else(|| "refused".into());
        let state = if status.is_client_error() {
            ProbeState::Refused
        } else {
            ProbeState::Unreachable
        };
        return (
            ProbeResult {
                status: code,
                ms,
                ..fail(state, &why)
            },
            None,
        );
    }
    let ok = answer
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|t| !t.is_empty());
    if !ok {
        return (
            ProbeResult {
                status: code,
                ms,
                ..fail(ProbeState::Refused, "no access token came back")
            },
            None,
        );
    }
    let rotated = answer
        .get("refreshToken")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && *t != s("refresh_token"))
        .map(|t| {
            let mut next = v.clone();
            next["refresh_token"] = json!(t);
            SecretString::new(next.to_string())
        });
    (
        ProbeResult {
            state: ProbeState::Accepted,
            status: code,
            ms,
            at,
            ..Default::default()
        },
        rotated,
    )
}

/// The OIDC base a test points the mint at (tests only; empty in a build).
static TEST_BASE: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();

fn base_for(region: &str) -> String {
    let t = TEST_BASE
        .get()
        .map(|m| m.lock().unwrap().clone())
        .unwrap_or_default();
    if t.is_empty() { oidc_base(region) } else { t }
}

#[cfg(test)]
pub(crate) fn set_test_base(base: &str) {
    *TEST_BASE.get_or_init(Default::default).lock().unwrap() = base.to_string();
}

/// The test of a stored sign-in (`probe::probe_stored` routes here). The
/// mint obeys the kill switch: while the factory is paused or stopped no
/// request leaves (it could rotate the credential) and the test reads
/// skipped. A rotated refresh token is sealed as the next version (actor
/// `kiro-refresh`, audited) without starting another test.
pub async fn test_stored(core: &Core, meta: &SecretMeta, value: &SecretString) -> ProbeResult {
    if let Err(e) = crate::control::check(core, "vault_test", &meta.id, "vault:kiro").await {
        return ProbeResult {
            state: ProbeState::Skipped,
            detail: if crate::control::is_blocked(&e) {
                "factory stopped; not tested".into()
            } else {
                format!("{e:#}")
            },
            at: kls_store::now_ms(),
            ..Default::default()
        };
    }
    let region = serde_json::from_str::<Value>(value.expose())
        .ok()
        .and_then(|v| v.get("region").and_then(Value::as_str).map(str::to_string))
        .filter(|r| valid_region(r))
        .unwrap_or_else(|| "us-east-1".into());
    let (r, rotated) = mint(value, &base_for(&region)).await;
    if let Some(next) = rotated
        && let Err(e) = keep_rotated(core, &meta.id, next).await
    {
        tracing::warn!(secret = %meta.id, "kiro refresh: {e:#}");
    }
    r
}

async fn keep_rotated(core: &Core, id: &str, next: SecretString) -> Result<()> {
    let m = vault(core)
        .await?
        .rotate_with(
            id,
            next,
            &kls_vault::RotateOpts {
                grace_ms: 0,
                origin: "kiro-refresh".into(),
                remote_id: String::new(),
            },
        )
        .await?;
    core.emit(
        kinds::VAULT_SECRET_SAVED,
        id,
        &json!({
            "id": id, "provider": m.provider, "env": m.env, "rotated": true,
            "fingerprint": m.fingerprint, "version": m.version, "by": "kiro-refresh",
        }),
    )
    .await?;
    Ok(())
}

/// The in-dialog test of a typed sign-in: checked here, never sent (a mint
/// may rotate the refresh token, and the typed copy would then be stale).
/// The stored copy is tested at AWS after the save.
pub fn check_typed(raw: &str) -> ProbeResult {
    let at = kls_store::now_ms();
    match parse_login(raw, at) {
        Ok(_) => ProbeResult {
            state: ProbeState::Skipped,
            detail: "well formed; tested at AWS after save".into(),
            at,
            ..Default::default()
        },
        Err(why) => ProbeResult {
            state: ProbeState::Refused,
            detail: why,
            at,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests;
