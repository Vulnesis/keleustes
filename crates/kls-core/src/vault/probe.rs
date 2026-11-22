//! Key tests: one free request (a models list) per provider that says whether
//! the provider accepts a key. A test is an indicator only: it never gates
//! or blocks saving, rotating or granting a key.
//!
//! Lifecycle of a stored key's test state (persisted on `vault_secrets`):
//! `untested` -> `testing` -> `accepted` | `refused` | `unreachable` |
//! `skipped`. A new value (save or rotate) resets it to `untested` and a
//! background test starts at once. A `testing` state older than
//! [`TESTING_STALE_MS`] (the app quit mid-test) reads as `untested`.
//!
//! The key is sent only in a request header to the provider's own endpoint.
//! It never appears in a URL, an event, a log line or a stored detail.

use super::{Provider, SecretMeta, SecretString, kinds, vault};
use crate::Core;
use anyhow::{Result, anyhow};
use kls_store::vault::SecretTest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

/// A test request gives up after this long and reads `unreachable`.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// A `testing` state older than this is a test that never finished.
pub const TESTING_STALE_MS: i64 = 30_000;
/// Off ("0", "false", "off") stops the automatic test after a save.
pub const AUTO_TEST_SETTING: &str = "vault.auto_test";
const DETAIL_MAX: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProbeState {
    #[default]
    Untested,
    Testing,
    Accepted,
    Refused,
    Unreachable,
    /// The provider has no test endpoint.
    Skipped,
}

impl ProbeState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeState::Untested => "",
            ProbeState::Testing => "testing",
            ProbeState::Accepted => "accepted",
            ProbeState::Refused => "refused",
            ProbeState::Unreachable => "unreachable",
            ProbeState::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "testing" => ProbeState::Testing,
            "accepted" => ProbeState::Accepted,
            "refused" => ProbeState::Refused,
            "unreachable" => ProbeState::Unreachable,
            "skipped" => ProbeState::Skipped,
            _ => ProbeState::Untested,
        }
    }
}

/// One test's outcome.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub state: ProbeState,
    /// HTTP status, when the provider answered.
    pub status: Option<i64>,
    pub ms: Option<i64>,
    /// Why it was refused or unreachable, in the provider's words (trimmed,
    /// never holding the key).
    pub detail: String,
    pub at: i64,
    /// Model ids an accepted test listed (model discovery). Never the key.
    #[serde(default)]
    pub models: Vec<String>,
}

/// Longest model list kept from a test.
pub const MODELS_MAX: usize = 500;

/// Model ids in a provider's models list: `data[].id` (Anthropic, OpenAI,
/// Mistral, OpenRouter), `models[].name` without `models/` (Google). Ids
/// that hold `key` (a provider echoing it) or are not plain ids are dropped.
pub fn models_from(body: &str, key: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let list = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(|x| x.as_array());
    for m in list.into_iter().flatten() {
        let id = m
            .get("id")
            .or_else(|| m.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let id = id.strip_prefix("models/").unwrap_or(id);
        let plain = !id.is_empty()
            && id.len() <= 120
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.:/@".contains(c));
        if plain && (key.len() < 4 || !id.contains(key)) && !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
        if out.len() >= MODELS_MAX {
            break;
        }
    }
    out
}

/// A stored key's test state as the UI should show it (a stale `testing`
/// reads as untested).
pub fn effective(meta: &SecretMeta, now: i64) -> ProbeResult {
    let state = ProbeState::parse(&meta.test_state);
    let stale =
        state == ProbeState::Testing && meta.tested_at.is_none_or(|t| now - t > TESTING_STALE_MS);
    ProbeResult {
        state: if stale { ProbeState::Untested } else { state },
        status: meta.test_status,
        ms: meta.test_ms,
        detail: meta.test_detail.clone(),
        at: meta.tested_at.unwrap_or_default(),
        models: meta.models.clone(),
    }
}

/// The test endpoint and how the key is sent, for `p`: its own `probe_url`,
/// else the known endpoint for a catalogue provider. `None`: not testable.
pub fn target(p: &Provider) -> Option<(String, String)> {
    let auth = |a: &str| if a.is_empty() { "bearer" } else { a }.to_string();
    if !p.probe_url.trim().is_empty() {
        return Some((p.probe_url.trim().to_string(), auth(&p.probe_auth)));
    }
    let (url, a) = match p.id.as_str() {
        "anthropic" => ("https://api.anthropic.com/v1/models", "x-api-key"),
        "openai" => ("https://api.openai.com/v1/models", "bearer"),
        "google" => (
            "https://generativelanguage.googleapis.com/v1beta/models",
            "x-goog-api-key",
        ),
        "openrouter" => ("https://openrouter.ai/api/v1/auth/key", "bearer"),
        "mistral" => ("https://api.mistral.ai/v1/models", "bearer"),
        _ => return None,
    };
    Some((url.into(), a.into()))
}

/// Cuts `s` to a short single line and removes the key if a provider
/// echoed it.
fn clean(s: &str, value: &str) -> String {
    let mut s = s.replace(['\n', '\r', '\t'], " ");
    if value.len() >= 4 {
        s = s.replace(value, "•••");
    }
    let s = s.trim();
    let mut out: String = s.chars().take(DETAIL_MAX).collect();
    if s.chars().count() > DETAIL_MAX {
        out.push('…');
    }
    out
}

/// The provider's reason from an error body (`error.message`, `message`,
/// `error`), else the body itself.
fn reason(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let pick = v
        .pointer("/error/message")
        .or_else(|| v.get("message"))
        .or_else(|| v.get("error"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    pick.unwrap_or_else(|| body.to_string())
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("http client: {e}"))
}

/// Tests `value` against `p`'s free endpoint. Never fails: every problem is
/// an outcome. 2xx accepted; 4xx refused; 5xx, timeouts and network errors
/// unreachable; no endpoint skipped.
pub async fn probe_key(p: &Provider, value: &SecretString) -> ProbeResult {
    let at = kls_store::now_ms();
    let Some((url, auth)) = target(p) else {
        return ProbeResult {
            state: ProbeState::Skipped,
            detail: "no test address".into(),
            at,
            ..Default::default()
        };
    };
    let key = value.expose().trim();
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult {
                state: ProbeState::Unreachable,
                detail: e.to_string(),
                at,
                ..Default::default()
            };
        }
    };
    let mut req = client.get(&url);
    req = match auth.as_str() {
        "x-api-key" => req
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "x-goog-api-key" => req.header("x-goog-api-key", key),
        _ => req.bearer_auth(key),
    };
    let started = Instant::now();
    let res = req.send().await;
    let ms = Some(started.elapsed().as_millis() as i64);
    match res {
        Err(e) => {
            let why = if e.is_timeout() {
                format!("no answer in {} s", TIMEOUT.as_secs())
            } else if e.is_connect() {
                "could not connect".to_string()
            } else {
                // without_url: the error text never carries the address.
                e.without_url().to_string()
            };
            ProbeResult {
                state: ProbeState::Unreachable,
                ms,
                detail: clean(&why, key),
                at,
                ..Default::default()
            }
        }
        Ok(r) => {
            let status = r.status();
            let code = Some(status.as_u16() as i64);
            if status.is_success() {
                let body = r.text().await.unwrap_or_default();
                return ProbeResult {
                    state: ProbeState::Accepted,
                    status: code,
                    ms,
                    at,
                    models: models_from(&body, key),
                    ..Default::default()
                };
            }
            let body = r.text().await.unwrap_or_default();
            let detail = clean(&reason(&body), key);
            ProbeResult {
                state: if status.is_client_error() {
                    ProbeState::Refused
                } else {
                    ProbeState::Unreachable
                },
                status: code,
                ms,
                detail,
                at,
                models: Vec::new(),
            }
        }
    }
}

fn record(r: &ProbeResult) -> SecretTest {
    SecretTest {
        state: r.state.as_str().to_string(),
        status: r.status,
        ms: r.ms,
        detail: r.detail.clone(),
        at: r.at,
    }
}

/// Tests stored key `id` and records the result on it (`vault.secret_tested`,
/// audited). `by` is the actor ("owner" for a person, "keleustes" for the
/// automatic test after a save).
pub async fn probe_stored(core: &Core, id: &str, by: &str) -> Result<ProbeResult> {
    let s = core.store();
    let meta = s
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    let provider = s.provider(&meta.provider).await?.unwrap_or(Provider {
        id: meta.provider.clone(),
        ..Default::default()
    });
    s.vault_set_test(
        id,
        &SecretTest {
            state: ProbeState::Testing.as_str().into(),
            at: kls_store::now_ms(),
            ..Default::default()
        },
    )
    .await?;
    core.emit(
        kinds::VAULT_SECRET_TESTING,
        id,
        &json!({"id": id, "provider": meta.provider}),
    )
    .await?;
    let r = match vault(core).await?.get(id).await {
        Ok(Some(value)) => probe_key(&provider, &value).await,
        Ok(None) => return Err(anyhow!("no such key")),
        Err(e) => ProbeResult {
            state: ProbeState::Unreachable,
            detail: format!("{e:#}"),
            at: kls_store::now_ms(),
            ..Default::default()
        },
    };
    s.vault_set_test(id, &record(&r)).await?;
    if r.state == ProbeState::Accepted && !r.models.is_empty() {
        s.vault_set_models(id, &r.models).await?;
    }
    core.emit(
        kinds::VAULT_SECRET_TESTED,
        id,
        &json!({
            "id": id, "provider": meta.provider, "env": meta.env,
            "outcome": r.state.as_str(), "status": r.status, "ms": r.ms,
            "reason": r.detail, "by": by, "models": r.models.len(),
        }),
    )
    .await?;
    Ok(r)
}

/// Model discovery: adds the models a key's last accepted test listed to
/// its provider (the "apply models after add" offer). Audited as a
/// provider change. Returns the ids that were new.
pub async fn apply_discovered_models(core: &Core, secret_id: &str) -> Result<Vec<String>> {
    let s = core.store();
    let meta = s
        .vault_secret_meta(secret_id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    let mut p = s.provider(&meta.provider).await?.unwrap_or(Provider {
        id: meta.provider.clone(),
        name: meta.provider.clone(),
        env: meta.env.clone(),
        ..Default::default()
    });
    let new: Vec<String> = meta
        .models
        .iter()
        .filter(|m| !p.models.contains(m))
        .cloned()
        .collect();
    if new.is_empty() {
        return Ok(new);
    }
    p.models.extend(new.iter().cloned());
    if p.default_model.is_empty() {
        p.default_model = p.models[0].clone();
    }
    super::save_provider(core, &p).await?;
    core.emit(
        kinds::VAULT_MODELS_APPLIED,
        secret_id,
        &json!({"id": secret_id, "provider": p.id, "added": new, "by": "owner"}),
    )
    .await?;
    Ok(new)
}

/// Whether a save starts a background test (setting `vault.auto_test`,
/// default on).
pub async fn auto_test(core: &Core) -> bool {
    !matches!(
        core.store()
            .setting(AUTO_TEST_SETTING)
            .await
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("0" | "false" | "off")
    )
}

/// Starts a test of stored key `id` in the background; returns at once.
pub fn spawn_probe(core: &Core, id: &str, by: &str) -> tokio::task::JoinHandle<()> {
    let (core, id, by) = (core.clone(), id.to_string(), by.to_string());
    tokio::spawn(async move {
        if let Err(e) = probe_stored(&core, &id, &by).await {
            tracing::debug!(secret = %id, "vault: key test: {e:#}");
        }
    })
}

/// Tests every stored key, one after another. Returns how many ran.
pub async fn test_all(core: &Core) -> Result<usize> {
    let mut n = 0;
    for m in core.store().vault_secrets().await? {
        probe_stored(core, &m.id, "owner").await?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A local HTTP server answering every request with `status` and
    /// `body`; it records each request head so tests can check headers.
    async fn fake(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let s2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = c.read(&mut buf).await.unwrap_or(0);
                s2.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}/v1/models"), seen)
    }

    fn provider(url: &str, auth: &str) -> Provider {
        Provider {
            id: "custom".into(),
            probe_url: url.into(),
            probe_auth: auth.into(),
            ..Default::default()
        }
    }

    const KEY: &str = "sk-probe-test-key-123456";

    #[tokio::test]
    async fn accepted_on_2xx_with_the_right_header() {
        let (url, seen) = fake(200, r#"{"data":[]}"#).await;
        let r = probe_key(&provider(&url, "x-api-key"), &SecretString::new(KEY)).await;
        assert_eq!(r.state, ProbeState::Accepted);
        assert_eq!(r.status, Some(200));
        assert!(r.ms.is_some());
        let head = seen.lock().unwrap()[0].to_lowercase();
        assert!(head.contains(&format!("x-api-key: {}", KEY.to_lowercase())));
        assert!(head.contains("anthropic-version"));
        let (url, seen) = fake(200, "{}").await;
        probe_key(&provider(&url, "bearer"), &SecretString::new(KEY)).await;
        assert!(
            seen.lock().unwrap()[0].contains(&format!("Bearer {KEY}"))
                || seen.lock().unwrap()[0]
                    .to_lowercase()
                    .contains(&format!("bearer {}", KEY.to_lowercase()))
        );
    }

    #[test]
    fn model_ids_from_list_bodies() {
        let oa = r#"{"data":[{"id":"gpt-5"},{"id":"gpt-5-mini"},{"id":"gpt-5"}]}"#;
        assert_eq!(models_from(oa, KEY), vec!["gpt-5", "gpt-5-mini"]);
        let g = r#"{"models":[{"name":"models/gemini-2.5-pro"}]}"#;
        assert_eq!(models_from(g, KEY), vec!["gemini-2.5-pro"]);
        // An echoed key or junk is never kept.
        let bad = format!(r#"{{"data":[{{"id":"{KEY}"}},{{"id":"a b"}},{{"id":"ok-1"}}]}}"#);
        assert_eq!(models_from(&bad, KEY), vec!["ok-1"]);
        assert!(models_from("not json", KEY).is_empty());
    }

    #[tokio::test]
    async fn discovered_models_are_stored_applied_and_never_leak_the_key() {
        let logs = super::super::lineage::tests::Logs::default();
        let _g = logs.install();
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        c.store()
            .set_setting(AUTO_TEST_SETTING, "off")
            .await
            .unwrap();
        let (url, _) = fake(200, r#"{"data":[{"id":"model-a"},{"id":"model-b"}]}"#).await;
        super::super::save_provider(&c, &provider(&url, "bearer"))
            .await
            .unwrap();
        let m =
            super::super::put_secret(&c, "custom", "CUSTOM_API_KEY", "", SecretString::new(KEY))
                .await
                .unwrap();
        let r = probe_stored(&c, &m.id, "owner").await.unwrap();
        assert_eq!(r.state, ProbeState::Accepted);
        assert_eq!(r.models, vec!["model-a", "model-b"]);
        let meta = c.store().vault_secret_meta(&m.id).await.unwrap().unwrap();
        assert_eq!(meta.models, r.models);
        assert_eq!(effective(&meta, kls_store::now_ms()).models.len(), 2);
        let added = apply_discovered_models(&c, &m.id).await.unwrap();
        assert_eq!(added.len(), 2);
        let p = c.store().provider("custom").await.unwrap().unwrap();
        assert_eq!(p.models, vec!["model-a", "model-b"]);
        assert_eq!(p.default_model, "model-a");
        // Applying again adds nothing.
        assert!(apply_discovered_models(&c, &m.id).await.unwrap().is_empty());
        c.audit_sync().await.unwrap();
        assert!(
            c.audit_entries(&kls_store::audit::LedgerQuery::default())
                .await
                .unwrap()
                .iter()
                .any(|e| e.msg.contains("vault.models_applied"))
        );
        super::super::lineage::tests::assert_no_plaintext(&c, dir.path(), &logs, KEY).await;
    }

    #[tokio::test]
    async fn refused_on_4xx_with_reason_and_no_key() {
        let (url, _) = fake(
            401,
            r#"{"error":{"message":"invalid x-api-key sk-probe-test-key-123456"}}"#,
        )
        .await;
        let r = probe_key(&provider(&url, "bearer"), &SecretString::new(KEY)).await;
        assert_eq!(r.state, ProbeState::Refused);
        assert_eq!(r.status, Some(401));
        assert!(r.detail.contains("invalid x-api-key"));
        assert!(!r.detail.contains(KEY), "the echoed key is scrubbed");
    }

    #[tokio::test]
    async fn unreachable_on_5xx_and_network_errors() {
        let (url, _) = fake(503, "down").await;
        let r = probe_key(&provider(&url, "bearer"), &SecretString::new(KEY)).await;
        assert_eq!(r.state, ProbeState::Unreachable);
        assert_eq!(r.status, Some(503));
        // Nothing listens on this port.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let r = probe_key(
            &provider(&format!("http://127.0.0.1:{port}/m"), "bearer"),
            &SecretString::new(KEY),
        )
        .await;
        assert_eq!(r.state, ProbeState::Unreachable);
        assert_eq!(r.status, None);
        assert!(!r.detail.contains(KEY));
    }

    #[tokio::test]
    async fn skipped_without_an_address() {
        let r = probe_key(&provider("", ""), &SecretString::new(KEY)).await;
        assert_eq!(r.state, ProbeState::Skipped);
        assert!(
            target(&Provider {
                id: "anthropic".into(),
                ..Default::default()
            })
            .is_some()
        );
    }

    #[test]
    fn stale_testing_reads_untested() {
        let m = SecretMeta {
            test_state: "testing".into(),
            tested_at: Some(1_000),
            ..Default::default()
        };
        assert_eq!(effective(&m, 2_000).state, ProbeState::Testing);
        assert_eq!(
            effective(&m, 1_000 + TESTING_STALE_MS + 1).state,
            ProbeState::Untested
        );
        for s in [
            "",
            "testing",
            "accepted",
            "refused",
            "unreachable",
            "skipped",
        ] {
            assert_eq!(ProbeState::parse(s).as_str(), s);
        }
    }

    #[tokio::test]
    async fn stored_test_records_state_events_and_audit() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        super::super::install_vault(
            &core,
            super::super::KeyBackend::File(dir.path().join("vault.key")),
        )
        .await
        .unwrap();
        core.store()
            .set_setting(AUTO_TEST_SETTING, "off")
            .await
            .unwrap();
        let (url, _) = fake(403, r#"{"message":"forbidden"}"#).await;
        super::super::save_provider(&core, &provider(&url, "bearer"))
            .await
            .unwrap();
        let m = super::super::put_secret(&core, "custom", "CUSTOM_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        // Saved at once and untested: the test never gates the save.
        assert_eq!(m.test_state, "");
        let r = probe_stored(&core, &m.id, "owner").await.unwrap();
        assert_eq!(r.state, ProbeState::Refused);
        let after = core
            .store()
            .vault_secret_meta(&m.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.test_state, "refused");
        assert_eq!(after.test_status, Some(403));
        assert!(after.tested_at.is_some());
        let evs = core.store().recent_events(50).await.unwrap();
        let tested = evs
            .iter()
            .find(|e| e.kind == kinds::VAULT_SECRET_TESTED)
            .unwrap();
        assert!(!tested.data_json.contains(KEY));
        assert!(evs.iter().any(|e| e.kind == kinds::VAULT_SECRET_TESTING));
        core.audit_sync().await.unwrap();
        let ledger = core
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(ledger.iter().any(|e| e.msg.contains("vault.secret_tested")));
        assert!(ledger.iter().all(|e| !e.msg.contains(KEY)));
    }

    #[tokio::test]
    async fn save_starts_a_background_test() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), None).await.unwrap();
        super::super::install_vault(
            &core,
            super::super::KeyBackend::File(dir.path().join("vault.key")),
        )
        .await
        .unwrap();
        let (url, _) = fake(200, "{}").await;
        super::super::save_provider(&core, &provider(&url, "bearer"))
            .await
            .unwrap();
        let mut rx = core.subscribe();
        let m = super::super::put_secret(&core, "custom", "CUSTOM_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let ev = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("a test ran")
                .unwrap();
            if ev.kind == kinds::VAULT_SECRET_TESTED {
                break;
            }
        }
        let after = core
            .store()
            .vault_secret_meta(&m.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.test_state, "accepted");
    }
}
