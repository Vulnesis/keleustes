//! Chat-channel notifications: factory events posted to a Slack, Discord or
//! plain JSON webhook through the one HTTP client (`reqwest`). Owner: WP1.
//!
//! - The webhook address is a credential: it is sealed in the vault (a
//!   named blob, never granted to a node), shown only by host, and never
//!   written to an event, the ledger or a log line.
//! - Which events go out is `notify.chat_events` (a `notify.events` level);
//!   quiet hours (`notify.quiet`, `notify.muted`) hold all but escalations.
//! - Every post first checks the kill switch (action `notify`): while the
//!   factory is paused or stopped nothing is posted, and the refusal is
//!   audited. Every outcome (sent, failed, held, refused) is a ledger entry.

use super::config::{self, Effective};
use crate::Core;
use anyhow::{Result, anyhow, bail};
use kls_store::audit::LedgerDraft;
use kls_vault::SecretString;
use serde_json::{Value, json};

/// The vault blob holding the webhook address.
pub const WEBHOOK_BLOB: &str = "notify.chat_webhook";

pub mod kinds {
    /// The webhook was set or cleared: `host`, `by` (never the address).
    pub const CHAT_WEBHOOK: &str = "notify.chat_webhook";
}

/// What a post did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// Off, no webhook, or not a level the setting lets through.
    Skipped(&'static str),
    /// Inside quiet hours.
    Held,
    Failed(String),
}

impl Outcome {
    fn word(&self) -> String {
        match self {
            Outcome::Sent => "sent".into(),
            Outcome::Skipped(w) => format!("skipped: {w}"),
            Outcome::Held => "held: quiet hours".into(),
            Outcome::Failed(e) => format!("failed: {e}"),
        }
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// Checks a webhook address: https, or http to this machine (a local relay).
pub fn check_webhook(url: &str) -> Result<String> {
    let url = url.trim();
    let host = host_of(url).ok_or_else(|| anyhow!("write a https:// address"))?;
    let local =
        host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]");
    if !url.starts_with("https://") && !(url.starts_with("http://") && local) {
        bail!("a webhook address must be https://");
    }
    if url.len() > 2000 || url.chars().any(char::is_whitespace) {
        bail!("not a webhook address");
    }
    Ok(host)
}

async fn record(core: &Core, action: &str, outcome: &str, payload: Value) {
    let d = LedgerDraft {
        at: 0,
        kind: "notify".into(),
        actor: "keleustes".into(),
        actor_kind: "system".into(),
        msg: format!("{action} {outcome}"),
        subject: json!({ "action": action }),
        payload: json!({ "outcome": outcome, "data": payload }),
        origin: "factory".into(),
        node_id: String::new(),
        session_id: String::new(),
        ref_key: None,
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(action, "notify audit: {e:#}");
    }
}

/// Seals the webhook address in the vault ("" clears it). Audited by host.
pub async fn set_webhook(core: &Core, url: SecretString) -> Result<Option<String>> {
    let v = crate::vault::vault(core).await?;
    let raw = url.expose().trim().to_string();
    let host = if raw.is_empty() {
        v.put_blob(WEBHOOK_BLOB, b"").await?;
        None
    } else {
        let h = check_webhook(&raw)?;
        v.put_blob(WEBHOOK_BLOB, raw.as_bytes()).await?;
        Some(h)
    };
    core.emit(
        kinds::CHAT_WEBHOOK,
        "notify",
        &json!({ "host": host, "set": host.is_some(), "by": "owner" }),
    )
    .await?;
    Ok(host)
}

async fn webhook(core: &Core) -> Result<Option<SecretString>> {
    let v = crate::vault::vault(core).await?;
    let Some(b) = v.get_blob(WEBHOOK_BLOB).await? else {
        return Ok(None);
    };
    let s = String::from_utf8(b.to_vec()).unwrap_or_default();
    Ok((!s.trim().is_empty()).then(|| SecretString::new(s.trim())))
}

/// The host of the sealed webhook, for the Settings screen.
pub async fn webhook_host(core: &Core) -> Result<Option<String>> {
    Ok(webhook(core).await?.and_then(|u| host_of(u.expose())))
}

/// The body each kind of channel expects.
pub fn payload(kind: &str, event: &str, subject: &str, text: &str) -> Value {
    match kind {
        "slack" => json!({ "text": text }),
        "discord" => json!({ "content": text }),
        _ => json!({ "text": text, "event": event, "subject": subject, "source": "keleustes" }),
    }
}

/// One line for an event: kind, subject and its short summary.
pub fn line(ev: &kls_store::Event) -> String {
    let data: Value = serde_json::from_str(&ev.data_json).unwrap_or(json!({}));
    let mut parts = vec![format!("[keleustes] {}", ev.kind)];
    if !ev.subject.is_empty() {
        parts.push(ev.subject.chars().take(80).collect());
    }
    for k in ["reason", "title", "status", "action", "mode"] {
        if let Some(s) = data
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            parts.push(format!("{k}: {}", s.chars().take(120).collect::<String>()));
        }
    }
    parts.join(" | ")
}

fn quiet_now(eff: &Effective, now: i64) -> bool {
    if eff.notify_muted {
        return true;
    }
    let (d, m) = crate::control::local_slot(now, &eff.schedules_tz);
    crate::control::parse_windows(&eff.notify_quiet)
        .map(|w| crate::control::in_windows(&w, d, m))
        .unwrap_or(false)
}

fn escalation(kind: &str) -> bool {
    config::notify_allows("escalations_only", kind)
}

/// Posts event `ev` to the chat channel when the settings let it through.
/// Refused (and audited) under the kill switch.
pub async fn post_event(core: &Core, ev: &kls_store::Event) -> Result<Outcome> {
    let eff = Effective::load(core).await?;
    if eff.chat.is_empty() || eff.chat == "off" {
        return Ok(Outcome::Skipped("off"));
    }
    if !config::notify_allows(&eff.chat_events, &ev.kind) {
        return Ok(Outcome::Skipped("level"));
    }
    post(
        core,
        &eff,
        &ev.kind,
        &ev.subject,
        &line(ev),
        kls_store::now_ms(),
    )
    .await
}

/// Posts `text` about `event`/`subject`. The kill switch is checked first.
pub async fn post(
    core: &Core,
    eff: &Effective,
    event: &str,
    subject: &str,
    text: &str,
    now: i64,
) -> Result<Outcome> {
    let detail = json!({ "event": event, "subject": subject, "channel": eff.chat });
    if let Err(e) = crate::control::check(core, "notify", subject, &format!("chat:{event}")).await {
        record(
            core,
            "chat.post",
            "refused",
            json!({ "event": event, "subject": subject, "why": format!("{e:#}") }),
        )
        .await;
        return Err(e);
    }
    let Some(url) = webhook(core).await? else {
        return Ok(Outcome::Skipped("no webhook"));
    };
    let out = if quiet_now(eff, now) && !escalation(event) {
        Outcome::Held
    } else {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| anyhow!("http client: {e}"))?;
        let res = client
            .post(url.expose())
            .header("content-type", "application/json")
            .body(payload(&eff.chat, event, subject, text).to_string())
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => Outcome::Sent,
            Ok(r) => Outcome::Failed(format!("the channel answered {}", r.status().as_u16())),
            // without_url: the address never reaches the error text.
            Err(e) => Outcome::Failed(if e.is_timeout() {
                "no answer in 10 s".into()
            } else {
                e.without_url().to_string()
            }),
        }
    };
    record(core, "chat.post", &out.word(), detail).await;
    Ok(out)
}

/// Posts a test line now (the Settings "Send test" button).
pub async fn post_test(core: &Core) -> Result<Outcome> {
    let eff = Effective::load(core).await?;
    if eff.chat.is_empty() || eff.chat == "off" {
        bail!("choose a chat channel first");
    }
    post(
        core,
        &eff,
        "notify.test",
        "settings",
        "[keleustes] test notification",
        kls_store::now_ms(),
    )
    .await
}

/// Follows the event stream for the life of the process, posting what the
/// settings let through.
pub fn start(core: &Core) -> tokio::task::JoinHandle<()> {
    let core = core.clone();
    tokio::spawn(async move {
        let mut bus = core.subscribe();
        loop {
            match bus.recv().await {
                Ok(ev) => {
                    // Its own audit trail is not news.
                    if ev.kind.starts_with("audit.") || ev.kind == kinds::CHAT_WEBHOOK {
                        continue;
                    }
                    if let Err(e) = post_event(&core, &ev).await
                        && !crate::control::is_blocked(&e)
                    {
                        tracing::debug!(kind = %ev.kind, "chat: {e:#}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::config::keys;
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn sink() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let s2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let mut buf = vec![0u8; 16384];
                let n = c.read(&mut buf).await.unwrap_or(0);
                s2.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let _ = c
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });
        (
            format!("http://{addr}/hooks/T000/B000/secret-token-xyz"),
            seen,
        )
    }

    async fn ledger(c: &Core, outcome: &str) -> usize {
        c.audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["notify".into()],
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap()
        .iter()
        .filter(|e| e.payload["outcome"] == outcome)
        .count()
    }

    fn ev(kind: &str) -> kls_store::Event {
        kls_store::Event {
            kind: kind.into(),
            subject: "run-1".into(),
            data_json: r#"{"reason":"budget"}"#.into(),
            id: 0,
            at: 0,
        }
    }

    #[tokio::test]
    async fn posts_holds_and_refuses_under_the_kill_switch() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        let (url, seen) = sink().await;
        assert!(check_webhook("ftp://x").is_err());
        assert!(check_webhook("http://example.com/x").is_err());
        assert_eq!(
            set_webhook(&c, SecretString::new(url.clone()))
                .await
                .unwrap()
                .as_deref(),
            Some(url.trim_start_matches("http://").split('/').next().unwrap())
        );
        // Off by default.
        assert_eq!(
            post_event(&c, &ev("approval.requested")).await.unwrap(),
            Outcome::Skipped("off")
        );
        c.store().set_setting(keys::CHAT, "slack").await.unwrap();
        // Level: escalations only by default.
        assert_eq!(
            post_event(&c, &ev("run.finished")).await.unwrap(),
            Outcome::Skipped("level")
        );
        assert_eq!(
            post_event(&c, &ev("approval.requested")).await.unwrap(),
            Outcome::Sent
        );
        let body = seen.lock().unwrap().last().unwrap().clone();
        assert!(body.contains("\"text\"") && body.contains("approval.requested"));
        // Quiet hours hold non-escalations.
        c.store()
            .set_setting(keys::CHAT_EVENTS, "everything")
            .await
            .unwrap();
        c.store()
            .set_setting(keys::NOTIFY_MUTED, "true")
            .await
            .unwrap();
        assert_eq!(
            post_event(&c, &ev("run.finished")).await.unwrap(),
            Outcome::Held
        );
        assert_eq!(
            post_event(&c, &ev("approval.requested")).await.unwrap(),
            Outcome::Sent
        );
        let sent = seen.lock().unwrap().len();
        // Paused: refused, audited, nothing posted.
        crate::control::pause(&c, "drill").await.unwrap();
        let e = post_event(&c, &ev("approval.requested")).await.unwrap_err();
        assert!(crate::control::is_blocked(&e));
        assert_eq!(seen.lock().unwrap().len(), sent);
        assert_eq!(ledger(&c, "sent").await, 2);
        assert_eq!(ledger(&c, "held: quiet hours").await, 1);
        assert_eq!(ledger(&c, "refused").await, 1);
        // The address never shows outside the vault.
        c.audit_sync().await.unwrap();
        for e in c.store().recent_events(500).await.unwrap() {
            assert!(!e.data_json.contains("secret-token-xyz"), "{}", e.kind);
        }
        for e in c
            .audit_entries(&kls_store::audit::LedgerQuery {
                limit: 1000,
                ..Default::default()
            })
            .await
            .unwrap()
        {
            assert!(!format!("{} {}", e.msg, e.payload).contains("secret-token-xyz"));
        }
        for (_, v) in c.store().all_settings().await.unwrap() {
            assert!(!v.contains("secret-token-xyz"));
        }
        assert!(webhook_host(&c).await.unwrap().is_some());
        set_webhook(&c, SecretString::new("")).await.unwrap();
        assert!(webhook_host(&c).await.unwrap().is_none());
    }

    #[test]
    fn payload_shapes() {
        assert_eq!(payload("discord", "e", "s", "t")["content"], "t");
        assert_eq!(payload("slack", "e", "s", "t")["text"], "t");
        assert_eq!(payload("generic", "e", "s", "t")["event"], "e");
    }
}
