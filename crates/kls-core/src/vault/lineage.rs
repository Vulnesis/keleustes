//! Key lifecycle beyond save: rotation grace and version lineage, custom
//! expiry, and auto-rotation where a management (provisioning) key lets the
//! factory mint keys itself. Owner: WP1 (governance).
//!
//! - **Grace.** A rotation keeps the old value sealed (under the master key,
//!   never plaintext) for `vault.rotation_grace_hours`, so a bad rotation can
//!   be rolled back; [`tick`] shreds it after. The lineage keeps every
//!   version's fingerprint, origin and dates.
//! - **Auto-rotation.** A key with `rotate_every_days > 0` whose provider has
//!   a [`Minter`] and a management key on file (provider slug
//!   `<provider>-management`, never granted to a node) is re-minted when due.
//!   The minted key's handle is kept so the retired one is revoked at the
//!   provider once its grace ends. Each step checks the kill switch
//!   (`provision`) and is audited.
//!
//! Values never appear in events, logs, errors or stored details.

use super::{SecretMeta, SecretString, kinds, vault};
use crate::Core;
use anyhow::{Result, anyhow, bail};
use kls_vault::{RotateOpts, SecretVersion};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Settings keys.
pub mod keys {
    /// Hours a rotated-out value stays sealed for a rollback (0: shred now).
    pub const GRACE_HOURS: &str = "vault.rotation_grace_hours";
}

pub const DEFAULT_GRACE_HOURS: i64 = 24;
const DAY_MS: i64 = 86_400_000;

/// The rotation grace in ms.
pub async fn grace_ms(core: &Core) -> i64 {
    core.store()
        .setting(keys::GRACE_HOURS)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_GRACE_HOURS)
        .clamp(0, 24 * 90)
        * 3_600_000
}

/// Rotates `id` with the configured grace. `origin` is the actor
/// ("owner", "auto-rotate"); `remote_id` the provider's handle of a minted key.
pub async fn rotate_with_grace(
    core: &Core,
    id: &str,
    value: SecretString,
    origin: &str,
    remote_id: &str,
) -> Result<SecretMeta> {
    let grace = grace_ms(core).await;
    let m = vault(core)
        .await?
        .rotate_with(
            id,
            value,
            &RotateOpts {
                grace_ms: grace,
                origin: origin.into(),
                remote_id: remote_id.into(),
            },
        )
        .await?;
    core.emit(
        kinds::VAULT_SECRET_SAVED,
        id,
        &json!({
            "id": id, "provider": m.provider, "env": m.env, "rotated": true,
            "fingerprint": m.fingerprint, "version": m.version, "graceMs": grace,
            "by": origin,
        }),
    )
    .await?;
    super::after_save(core, id).await;
    Ok(m)
}

/// Restores the newest rotated-out value still inside its grace.
pub async fn rollback(core: &Core, id: &str) -> Result<SecretMeta> {
    let v = vault(core).await?;
    let from = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    let m = v.rollback(id, "owner").await?;
    core.emit(
        kinds::VAULT_SECRET_ROLLBACK,
        id,
        &json!({
            "id": id, "provider": m.provider, "from": from.version, "version": m.version,
            "fingerprint": m.fingerprint, "by": "owner",
        }),
    )
    .await?;
    Ok(m)
}

/// A key's lineage, newest first.
pub async fn versions(core: &Core, id: &str) -> Result<Vec<SecretVersion>> {
    core.store().vault_versions(id).await
}

/// Whether a rollback is possible now (a sealed value inside its grace).
pub fn can_roll_back(versions: &[SecretVersion], now: i64) -> bool {
    versions
        .iter()
        .any(|v| v.sealed && v.grace_until.is_some_and(|g| g > now))
}

// ---- custom expiry ---------------------------------------------------------------

/// A custom expiry: "" (never), `YYYY-MM-DD` (the end of that UTC day),
/// `YYYY-MM-DD HH:MM` (UTC) or `<n>d` / `<n>h` from `now`. It must lie in
/// the future.
pub fn parse_expiry(s: &str, now: i64) -> Result<Option<i64>> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("never") {
        return Ok(None);
    }
    let rel = |unit: char, ms: i64| -> Option<i64> {
        s.strip_suffix(unit)?
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0 && *n <= 36_500 * DAY_MS / ms)
            .map(|n| now + n * ms)
    };
    let at = if let Some(t) = rel('d', DAY_MS).or_else(|| rel('h', 3_600_000)) {
        t
    } else if s.len() > 10
        && let Ok(dt) = s.replacen(' ', "T", 1).parse::<jiff::civil::DateTime>()
    {
        dt.to_zoned(jiff::tz::TimeZone::UTC)
            .map_err(|e| anyhow!("{s:?}: {e}"))?
            .timestamp()
            .as_millisecond()
    } else if s.len() == 10
        && let Ok(d) = s.parse::<jiff::civil::Date>()
    {
        d.at(23, 59, 59, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .map_err(|e| anyhow!("{s:?}: {e}"))?
            .timestamp()
            .as_millisecond()
    } else {
        bail!("{s:?}: write a date (2027-01-31), a date and time (2027-01-31 18:00) or 30d");
    };
    if at <= now {
        bail!("{s:?} is in the past");
    }
    Ok(Some(at))
}

/// Sets a key's expiry from [`parse_expiry`] text, keeping its node scope.
pub async fn set_expiry(core: &Core, id: &str, text: &str) -> Result<SecretMeta> {
    let at = parse_expiry(text, kls_store::now_ms())?;
    let m = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    super::set_secret_scope(core, id, at, &m.nodes).await
}

// ---- auto-rotation -----------------------------------------------------------------

/// The provider slug a provider's management key is filed under.
pub fn management_slug(provider: &str) -> String {
    format!("{provider}{}", kls_vault::MANAGEMENT_SUFFIX)
}

/// A key minted by a provider's management API.
pub struct Minted {
    pub value: SecretString,
    /// The provider's handle of the key (to revoke it later).
    pub remote_id: String,
}

/// A provider whose management key can mint and revoke API keys.
#[async_trait::async_trait]
pub trait Minter: Send + Sync {
    fn provider(&self) -> &str;
    async fn mint(&self, management: &SecretString, name: &str) -> Result<Minted>;
    async fn revoke(&self, management: &SecretString, remote_id: &str) -> Result<()>;
}

/// OpenRouter's provisioning API (`POST /api/v1/keys`, `DELETE
/// /api/v1/keys/{hash}`), authorised by a provisioning key.
pub struct OpenRouterMinter {
    pub base: String,
}

impl Default for OpenRouterMinter {
    fn default() -> Self {
        Self {
            base: "https://openrouter.ai/api/v1".into(),
        }
    }
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(super::probe::TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("keleustes/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("http client: {e}"))
}

#[async_trait::async_trait]
impl Minter for OpenRouterMinter {
    fn provider(&self) -> &str {
        "openrouter"
    }

    async fn mint(&self, management: &SecretString, name: &str) -> Result<Minted> {
        let r = http()?
            .post(format!("{}/keys", self.base))
            .bearer_auth(management.expose())
            .header("content-type", "application/json")
            .body(json!({ "name": name }).to_string())
            .send()
            .await
            .map_err(|e| anyhow!("mint: {}", e.without_url()))?;
        let status = r.status();
        let body: serde_json::Value =
            serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or_default();
        if !status.is_success() {
            bail!("mint: the provider answered {}", status.as_u16());
        }
        let key = body["key"].as_str().unwrap_or("").trim();
        if key.is_empty() {
            bail!("mint: the provider sent no key");
        }
        Ok(Minted {
            value: SecretString::new(key),
            remote_id: body["data"]["hash"].as_str().unwrap_or("").to_string(),
        })
    }

    async fn revoke(&self, management: &SecretString, remote_id: &str) -> Result<()> {
        let r = http()?
            .delete(format!("{}/keys/{remote_id}", self.base))
            .bearer_auth(management.expose())
            .send()
            .await
            .map_err(|e| anyhow!("revoke: {}", e.without_url()))?;
        if !r.status().is_success() {
            bail!("revoke: the provider answered {}", r.status().as_u16());
        }
        Ok(())
    }
}

/// The built-in minter for `provider`, if its management API can mint keys.
pub fn minter_for(provider: &str) -> Option<Box<dyn Minter>> {
    match provider {
        "openrouter" => Some(Box::new(OpenRouterMinter::default())),
        _ => None,
    }
}

/// Providers whose management key can mint keys (auto-rotation offered).
pub const MINTABLE: [&str; 1] = ["openrouter"];

/// The management key on file for `provider`, if any.
pub async fn management_key(core: &Core, provider: &str) -> Result<Option<SecretString>> {
    let slug = management_slug(provider);
    let Some(m) = core
        .store()
        .vault_secrets()
        .await?
        .into_iter()
        .find(|m| m.provider == slug)
    else {
        return Ok(None);
    };
    vault(core).await?.get(&m.id).await
}

/// Whether key `m` is due for auto-rotation at `now`.
pub fn rotation_due(m: &SecretMeta, now: i64) -> bool {
    m.rotate_every_days > 0
        && !kls_vault::is_management_provider(&m.provider)
        && now - m.rotated_at.unwrap_or(m.created_at) >= m.rotate_every_days * DAY_MS
}

/// Sets a key's auto-rotation period (0: off). Refuses a provider with no
/// minter: rotation there stays manual.
pub async fn set_rotation(core: &Core, id: &str, days: i64) -> Result<SecretMeta> {
    let m = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    if days > 0 && !MINTABLE.contains(&m.provider.as_str()) {
        bail!("{} keys cannot be minted here: rotate by hand", m.provider);
    }
    if !(0..=365).contains(&days) {
        bail!("rotate every 1 to 365 days, or 0 for never");
    }
    core.store().vault_set_rotation(id, days).await?;
    core.emit(
        kinds::VAULT_SECRET_POLICY,
        id,
        &json!({"id": id, "provider": m.provider, "rotateEveryDays": days, "by": "owner"}),
    )
    .await?;
    core.store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))
}

async fn failed(core: &Core, id: &str, provider: &str, step: &str, e: &anyhow::Error) {
    let _ = core
        .emit(
            kinds::VAULT_ROTATE_FAILED,
            id,
            &json!({"id": id, "provider": provider, "step": step, "reason": format!("{e:#}"),
                    "by": "auto-rotate"}),
        )
        .await;
}

/// Mints a new key for `id` with `minter` and rotates it in (grace kept).
/// Refused under the kill switch (audited `factory.refused`).
pub async fn auto_rotate(
    core: &Core,
    minter: &dyn Minter,
    id: &str,
    origin: &str,
) -> Result<SecretMeta> {
    crate::control::check(core, "provision", id, &format!("vault:{origin}")).await?;
    let m = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    let Some(mgmt) = management_key(core, &m.provider).await? else {
        let e = anyhow!(
            "no management key on file ({})",
            management_slug(&m.provider)
        );
        failed(core, id, &m.provider, "mint", &e).await;
        return Err(e);
    };
    let name = format!("keleustes {} v{}", m.env, m.version + 1);
    let minted = match minter.mint(&mgmt, &name).await {
        Ok(x) => x,
        Err(e) => {
            failed(core, id, &m.provider, "mint", &e).await;
            return Err(e);
        }
    };
    rotate_with_grace(core, id, minted.value, origin, &minted.remote_id).await
}

/// What one [`tick`] did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickReport {
    pub shredded: usize,
    pub revoked: usize,
    pub rotated: usize,
    pub refused: usize,
    pub errors: Vec<String>,
}

/// The vault's periodic work: shred values past their grace (revoking a
/// minted one at its provider) and auto-rotate keys that are due. `minter`
/// picks the minter per provider ([`minter_for`] in the app).
pub async fn tick(
    core: &Core,
    now: i64,
    minter: &(dyn Fn(&str) -> Option<Box<dyn Minter>> + Sync),
) -> Result<TickReport> {
    let mut r = TickReport::default();
    let v = vault(core).await?;
    let shredded = v.sweep_grace(now).await?;
    if !shredded.is_empty() {
        r.shredded = shredded.len();
        core.emit(
            kinds::VAULT_SECRET_SHREDDED,
            "vault",
            &json!({
                "count": shredded.len(), "by": "keleustes",
                "versions": shredded.iter().map(|x| json!({"id": x.secret_id, "version": x.version, "fingerprint": x.fingerprint})).collect::<Vec<_>>(),
            }),
        )
        .await?;
    }
    let metas = core.store().vault_secrets().await?;
    for sv in shredded.iter().filter(|x| !x.remote_id.is_empty()) {
        let Some(m) = metas.iter().find(|m| m.id == sv.secret_id) else {
            continue;
        };
        let Some(mt) = minter(&m.provider) else {
            continue;
        };
        if crate::control::check(core, "provision", &m.id, "vault:revoke")
            .await
            .is_err()
        {
            r.refused += 1;
            continue;
        }
        let res = match management_key(core, &m.provider).await? {
            Some(k) => mt.revoke(&k, &sv.remote_id).await,
            None => Err(anyhow!("no management key on file")),
        };
        match res {
            Ok(()) => {
                r.revoked += 1;
                core.emit(
                    kinds::VAULT_SECRET_SHREDDED,
                    &m.id,
                    &json!({"id": m.id, "provider": m.provider, "version": sv.version,
                            "revoked": true, "by": "keleustes"}),
                )
                .await?;
            }
            Err(e) => {
                failed(core, &m.id, &m.provider, "revoke", &e).await;
                r.errors.push(format!("{} v{}: {e:#}", m.id, sv.version));
            }
        }
    }
    for m in metas.iter().filter(|m| rotation_due(m, now)) {
        let Some(mt) = minter(&m.provider) else {
            continue;
        };
        match auto_rotate(core, mt.as_ref(), &m.id, "auto-rotate").await {
            Ok(_) => r.rotated += 1,
            Err(e) if crate::control::is_blocked(&e) => r.refused += 1,
            Err(e) => r.errors.push(format!("{}: {e:#}", m.id)),
        }
    }
    Ok(r)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Captures every log line written while it is the default subscriber.
    #[derive(Clone, Default)]
    pub(crate) struct Logs(pub Arc<Mutex<Vec<String>>>);

    struct Visit<'a>(&'a mut String);
    impl tracing::field::Visit for Visit<'_> {
        fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
            self.0.push_str(&format!(" {}={v:?}", f.name()));
        }
    }

    impl tracing::Subscriber for Logs {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, a: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            let mut s = String::new();
            a.record(&mut Visit(&mut s));
            self.0.lock().unwrap().push(s);
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, v: &tracing::span::Record<'_>) {
            let mut s = String::new();
            v.record(&mut Visit(&mut s));
            self.0.lock().unwrap().push(s);
        }
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, e: &tracing::Event<'_>) {
            let mut s = String::new();
            e.record(&mut Visit(&mut s));
            self.0.lock().unwrap().push(s);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    impl Logs {
        pub(crate) fn install(&self) -> tracing::subscriber::DefaultGuard {
            tracing::subscriber::set_default(self.clone())
        }
        pub(crate) fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    /// `plain` is nowhere: not in an event, a ledger entry, a setting, a log
    /// line or any byte of the data directory.
    pub(crate) async fn assert_no_plaintext(
        core: &Core,
        dir: &std::path::Path,
        logs: &Logs,
        plain: &str,
    ) {
        assert!(plain.len() >= 8);
        for e in core.store().recent_events(5000).await.unwrap() {
            assert!(!e.data_json.contains(plain), "event {} holds it", e.kind);
            assert!(!e.subject.contains(plain));
        }
        core.audit_sync().await.unwrap();
        for e in core
            .audit_entries(&kls_store::audit::LedgerQuery {
                limit: 10_000,
                ..Default::default()
            })
            .await
            .unwrap()
        {
            let all = format!("{} {} {}", e.msg, e.subject, e.payload);
            assert!(!all.contains(plain), "ledger {} holds it", e.kind);
        }
        for (k, v) in core.store().all_settings().await.unwrap() {
            assert!(!v.contains(plain), "setting {k} holds it");
        }
        for l in logs.lines() {
            assert!(!l.contains(plain), "a log line holds it: {l}");
        }
        fn walk(p: &std::path::Path, plain: &[u8]) {
            for e in std::fs::read_dir(p).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, plain);
                } else if let Ok(b) = std::fs::read(&path) {
                    assert!(
                        !b.windows(plain.len()).any(|w| w == plain),
                        "{} holds it",
                        path.display()
                    );
                }
            }
        }
        walk(dir, plain.as_bytes());
    }

    async fn core() -> (
        Core,
        tempfile::TempDir,
        Logs,
        tracing::subscriber::DefaultGuard,
    ) {
        let logs = Logs::default();
        let g = logs.install();
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        c.store()
            .set_setting(super::super::probe::AUTO_TEST_SETTING, "off")
            .await
            .unwrap();
        (c, dir, logs, g)
    }

    const K1: &str = "sk-or-v1-aaaaaaaaaaaaaaaaaaaa-first";
    const K2: &str = "sk-or-v1-bbbbbbbbbbbbbbbbbbbb-second";
    const MGMT: &str = "sk-or-prov-cccccccccccccccc-mgmt";

    #[tokio::test]
    async fn rotation_grace_lineage_and_rollback_never_leak() {
        let (c, d, logs, _g) = core().await;
        let m = super::super::put_secret(
            &c,
            "openrouter",
            "OPENROUTER_API_KEY",
            "",
            SecretString::new(K1),
        )
        .await
        .unwrap();
        let m2 = super::super::rotate_secret(&c, &m.id, SecretString::new(K2))
            .await
            .unwrap();
        assert_eq!(m2.version, 2);
        let vs = versions(&c, &m.id).await.unwrap();
        assert_eq!(vs.len(), 2);
        assert!(can_roll_back(&vs, kls_store::now_ms()));
        assert_eq!(vs[1].fingerprint, m.fingerprint);
        let back = rollback(&c, &m.id).await.unwrap();
        assert_eq!(back.fingerprint, m.fingerprint);
        assert_eq!(
            vault(&c)
                .await
                .unwrap()
                .get(&m.id)
                .await
                .unwrap()
                .unwrap()
                .expose(),
            K1
        );
        // The grace ends: the sealed copies are shredded, the lineage stays.
        let r = tick(&c, kls_store::now_ms() + 30 * 3_600_000, &|_| None)
            .await
            .unwrap();
        assert_eq!(r.shredded, 2);
        assert!(!can_roll_back(
            &versions(&c, &m.id).await.unwrap(),
            kls_store::now_ms()
        ));
        assert!(rollback(&c, &m.id).await.is_err());
        let evs = c.store().recent_events(100).await.unwrap();
        for k in [kinds::VAULT_SECRET_ROLLBACK, kinds::VAULT_SECRET_SHREDDED] {
            assert!(evs.iter().any(|e| e.kind == k), "{k}");
        }
        c.audit_sync().await.unwrap();
        let ledger = c
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["vault".into()],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            ledger
                .iter()
                .any(|e| e.msg.contains("vault.secret_rollback"))
        );
        assert!(
            ledger
                .iter()
                .any(|e| e.msg.contains("vault.secret_shredded"))
        );
        for p in [K1, K2] {
            assert_no_plaintext(&c, d.path(), &logs, p).await;
        }
    }

    #[test]
    fn expiry_text() {
        let now = 1_800_000_000_000; // 2027-01-15
        assert_eq!(parse_expiry("", now).unwrap(), None);
        assert_eq!(parse_expiry("never", now).unwrap(), None);
        assert_eq!(parse_expiry("30d", now).unwrap(), Some(now + 30 * DAY_MS));
        assert_eq!(
            parse_expiry("12h", now).unwrap(),
            Some(now + 12 * 3_600_000)
        );
        let d = parse_expiry("2027-02-01", now).unwrap().unwrap();
        assert_eq!(d, 1_801_526_399_000);
        assert!(parse_expiry("2027-02-01 18:30", now).unwrap().unwrap() < d);
        assert!(parse_expiry("2020-01-01", now).is_err());
        assert!(parse_expiry("soon", now).is_err());
        assert!(parse_expiry("0d", now).is_err());
    }

    #[tokio::test]
    async fn custom_expiry_is_kept_and_never_leaks() {
        let (c, d, logs, _g) = core().await;
        let m = super::super::put_secret(
            &c,
            "openrouter",
            "OPENROUTER_API_KEY",
            "",
            SecretString::new(K1),
        )
        .await
        .unwrap();
        let s = set_expiry(&c, &m.id, "2999-12-31").await.unwrap();
        assert_eq!(parse_expiry("2999-12-31", 0).unwrap(), s.expires_at);
        assert!(set_expiry(&c, &m.id, "yesterday").await.is_err());
        let s = set_expiry(&c, &m.id, "").await.unwrap();
        assert_eq!(s.expires_at, None);
        assert_no_plaintext(&c, d.path(), &logs, K1).await;
    }

    struct FakeMinter {
        minted: Mutex<Vec<String>>,
        revoked: Mutex<Vec<String>>,
        mgmt_seen: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Minter for FakeMinter {
        fn provider(&self) -> &str {
            "openrouter"
        }
        async fn mint(&self, management: &SecretString, _name: &str) -> Result<Minted> {
            self.mgmt_seen
                .lock()
                .unwrap()
                .push(management.expose().into());
            let mut m = self.minted.lock().unwrap();
            let n = m.len();
            let v = format!("sk-or-v1-minted-{n:04}-dddddddddddd");
            m.push(v.clone());
            Ok(Minted {
                value: SecretString::new(v),
                remote_id: format!("hash-{n}"),
            })
        }
        async fn revoke(&self, _m: &SecretString, remote_id: &str) -> Result<()> {
            self.revoked.lock().unwrap().push(remote_id.into());
            Ok(())
        }
    }

    #[tokio::test]
    async fn auto_rotation_mints_revokes_obeys_the_kill_switch_and_never_leaks() {
        let (c, d, logs, _g) = core().await;
        let m = super::super::put_secret(
            &c,
            "openrouter",
            "OPENROUTER_API_KEY",
            "",
            SecretString::new(K1),
        )
        .await
        .unwrap();
        // Only mintable providers rotate on their own.
        let a = super::super::put_secret(
            &c,
            "anthropic",
            "ANTHROPIC_API_KEY",
            "",
            SecretString::new(K2),
        )
        .await
        .unwrap();
        assert!(set_rotation(&c, &a.id, 30).await.is_err());
        set_rotation(&c, &m.id, 30).await.unwrap();
        let fake = Arc::new(FakeMinter {
            minted: Mutex::new(Vec::new()),
            revoked: Mutex::new(Vec::new()),
            mgmt_seen: Mutex::new(Vec::new()),
        });
        let f2 = fake.clone();
        struct Shared(Arc<FakeMinter>);
        #[async_trait::async_trait]
        impl Minter for Shared {
            fn provider(&self) -> &str {
                "openrouter"
            }
            async fn mint(&self, k: &SecretString, n: &str) -> Result<Minted> {
                self.0.mint(k, n).await
            }
            async fn revoke(&self, k: &SecretString, r: &str) -> Result<()> {
                self.0.revoke(k, r).await
            }
        }
        let pick = move |p: &str| -> Option<Box<dyn Minter>> {
            (p == "openrouter").then(|| Box::new(Shared(f2.clone())) as Box<dyn Minter>)
        };
        let later = kls_store::now_ms() + 31 * DAY_MS;
        // No management key yet: it fails, audited, nothing rotates.
        let r = tick(&c, later, &pick).await.unwrap();
        assert_eq!((r.rotated, r.errors.len()), (0, 1));
        // The management key is filed apart and never granted to a node.
        super::super::put_secret(
            &c,
            &management_slug("openrouter"),
            "OPENROUTER_PROVISIONING_KEY",
            "",
            SecretString::new(MGMT),
        )
        .await
        .unwrap();
        let g = vault(&c)
            .await
            .unwrap()
            .grant_scoped(&[management_slug("openrouter")], 60_000, "test", None)
            .await
            .unwrap();
        assert!(g.secrets.is_empty() && !g.denied.is_empty());
        // Paused: refused and audited, the key untouched.
        crate::control::pause(&c, "drill").await.unwrap();
        let r = tick(&c, later, &pick).await.unwrap();
        assert_eq!((r.rotated, r.refused), (0, 1));
        assert!(fake.minted.lock().unwrap().is_empty());
        let refused = c
            .store()
            .recent_events(200)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.kind == crate::control::kinds::FACTORY_REFUSED
                    && e.data_json.contains("vault:auto-rotate")
            })
            .count();
        assert_eq!(refused, 1);
        crate::control::resume(&c).await.unwrap();
        // Running: minted and rotated in, the old value in grace.
        let r = tick(&c, later, &pick).await.unwrap();
        assert_eq!(r.rotated, 1, "{r:?}");
        assert_eq!(
            fake.mgmt_seen.lock().unwrap().as_slice(),
            [MGMT.to_string()]
        );
        let now_m = c.store().vault_secret_meta(&m.id).await.unwrap().unwrap();
        assert_eq!((now_m.version, now_m.remote_id.as_str()), (2, "hash-0"));
        // Not due again right after.
        assert!(!rotation_due(&now_m, kls_store::now_ms()));
        // A second rotation retires the minted key; past its grace the
        // provider is told to revoke it.
        auto_rotate(&c, &Shared(fake.clone()), &m.id, "owner")
            .await
            .unwrap();
        let r = tick(&c, kls_store::now_ms() + 30 * 3_600_000, &pick)
            .await
            .unwrap();
        assert!(r.shredded >= 1 && r.revoked == 1, "{r:?}");
        assert_eq!(
            fake.revoked.lock().unwrap().as_slice(),
            ["hash-0".to_string()]
        );
        let minted = fake.minted.lock().unwrap().clone();
        for p in [K1, K2, MGMT].iter().map(|s| s.to_string()).chain(minted) {
            assert_no_plaintext(&c, d.path(), &logs, &p).await;
        }
    }
}
