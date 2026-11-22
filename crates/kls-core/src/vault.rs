//! Domain: vault and workforce (M5).
//!
//! The vault holds model provider keys. Keleustes never calls a model: it
//! hands keys to belai nodes in a grant ([`grant_for`]) attached to a
//! dispatch or job, and nodes do all model work. Around it: budgets
//! ([`check_budget`]), routing ([`route`]), usage (read from `session.*`
//! events), agent profiles (drafted on a node with [`draft_agent`]), crews
//! and the library (prompts, skills, commands, processes, versioned).
//!
//! Secret values never appear in events, logs or errors.

use crate::{Core, JobNeeds};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub use kls_store::vault::{
    AgentProfile, Budget, Crew, CrewMember, GrantRecord, LibraryItem, LibraryVersion, Provider,
    RoutingRule, SecretMeta, UsageFilter, UsageRow, UsageTotal,
};
pub use kls_vault::{
    GrantSecret, KeyBackend, KeySource, SecretString, SecretVersion, Vault, VaultGrantData,
    is_management_provider,
};

pub mod lineage;
pub mod probe;
pub use probe::{ProbeResult, ProbeState};

/// What [`grant_for`] returns: one `VaultGrant` for a dispatch or job.
/// Convert with `secrets.iter().map(|s| (s.provider, s.env, s.value.expose()))`.
pub type GrantPayload = VaultGrantData;

/// Event kinds. Payloads carry ids and names, never secret values.
pub mod kinds {
    pub const VAULT_SECRET_SAVED: &str = "vault.secret_saved";
    pub const VAULT_SECRET_DELETED: &str = "vault.secret_deleted";
    pub const VAULT_GRANTED: &str = "vault.granted";
    pub const VAULT_GRANT_REVOKED: &str = "vault.grant_revoked";
    /// A grant was refused (kill switch).
    pub const VAULT_GRANT_REFUSED: &str = "vault.grant_refused";
    /// A key test started (indicator only).
    pub const VAULT_SECRET_TESTING: &str = "vault.secret_testing";
    /// A key test finished: `outcome`, `status`, `ms`, `reason`, `by`.
    pub const VAULT_SECRET_TESTED: &str = "vault.secret_tested";
    /// An integrity check ran: `ok`, `reason`.
    pub const VAULT_SECRET_VERIFIED: &str = "vault.secret_verified";
    /// A key's label changed (value untouched).
    pub const VAULT_SECRET_LABEL: &str = "vault.secret_label";
    /// Discovered models were added to the provider: `added`.
    pub const VAULT_MODELS_APPLIED: &str = "vault.models_applied";
    /// A rotation was rolled back inside its grace: `version`, `from`.
    pub const VAULT_SECRET_ROLLBACK: &str = "vault.secret_rollback";
    /// Retired values past their grace were shredded: `count`, `versions`.
    pub const VAULT_SECRET_SHREDDED: &str = "vault.secret_shredded";
    /// A key's auto-rotation period or expiry changed.
    pub const VAULT_SECRET_POLICY: &str = "vault.secret_policy";
    /// An auto-rotation or provider-side revoke failed: `reason`.
    pub const VAULT_ROTATE_FAILED: &str = "vault.rotate_failed";
    /// A budget crossed an alert threshold: `pct`, `budgetId`.
    pub const BUDGET_ALERT: &str = "budget.alert";
    /// The budget gate held back model work: `action`, `budgetId`, `origin`.
    pub const BUDGET_BLOCKED: &str = "budget.blocked";
    pub const BUDGET_SAVED: &str = "budget.saved";
    pub const BUDGET_DELETED: &str = "budget.deleted";
    pub const BUDGET_LIMIT: &str = "budget.limit";
    pub const BUDGET_PROVIDER: &str = "budget.provider";
    pub const BUDGET_ROUTING: &str = "budget.routing";
    pub const BUDGET_USAGE: &str = "budget.usage";
    pub const AGENT_SAVED: &str = "agent.saved";
    pub const AGENT_DELETED: &str = "agent.deleted";
    pub const AGENT_DRAFTED: &str = "agent.drafted";
    pub const CREW_SAVED: &str = "crew.saved";
    pub const CREW_DELETED: &str = "crew.deleted";
    pub const LIBRARY_SAVED: &str = "library.saved";
    pub const LIBRARY_DELETED: &str = "library.deleted";
    pub const LIBRARY_IMPORTED: &str = "library.imported";
    /// An agent was installed from a library version: `itemId`, `version`.
    pub const LIBRARY_INSTALLED: &str = "library.installed";
}

/// Settings keys.
pub mod settings {
    /// `<provider>/<model>` used when no rule or profile names one.
    pub const MODELS_DEFAULT: &str = "models.default";
    /// `<provider>/<model>` a `downgrade_to_fast` budget switches to.
    pub const MODELS_FAST: &str = "models.fast";
    /// `agent=provider/model` entries, comma separated: the model per agent.
    pub const MODELS_AGENTS: &str = "models.agents";
    /// Highest event id folded into usage rows.
    pub const USAGE_CURSOR: &str = "vault.usage_cursor";
}

/// Environment override: `file` keeps the master key in `<data>/vault.key`.
pub const BACKEND_ENV: &str = "KELEUSTES_VAULT_BACKEND";
pub const KEY_FILE: &str = "vault.key";

/// Library kind of a backed-up agent profile (not listed with the others).
pub const AGENT_KIND: &str = "agent";
/// Library kind of a backed-up crew (P0 #14; not listed with the others).
pub const CREW_KIND: &str = "crew";
/// The library item kinds (belai `libitem`): Markdown prompts, skills,
/// commands and processes; JSON hooks, MCP servers, repositories, token
/// budgets, provider sets and the Bash rewrite table.
pub const LIBRARY_KINDS: [&str; 10] = [
    "prompt", "skill", "command", "process", "hook", "mcp", "repo", "budget", "provider", "rewrite",
];
/// The JSON kinds, with the top-level keys belai's validator accepts
/// (`libitem` repo.go, budget.go, provider.go, rewrite.go). belai checks the
/// whole document again when it installs one.
pub const JSON_ITEM_KEYS: [(&str, &[&str]); 4] = [
    (
        "repo",
        &[
            "name",
            "url",
            "visibility",
            "auth",
            "installation_id",
            "refs",
            "dir",
            "depth",
            "submodules",
            "enabled",
        ],
    ),
    ("budget", &["name", "budgets", "cycle_seconds", "warn"]),
    ("provider", &["name", "providers", "firewall"]),
    ("rewrite", &["name", "enabled", "rules"]),
];
pub const BUDGET_ACTIONS: [&str; 5] = [
    "alert_only",
    "downgrade_to_fast",
    "escalate_human",
    "pause_run",
    "hard_stop",
];
pub const BUDGET_PERIODS: [&str; 4] = ["day", "week", "month", "total"];
/// `org` is the whole factory (single tenant): every spend, every provider,
/// whatever the budget's provider list says.
pub const BUDGET_SCOPES: [&str; 8] = [
    "all", "org", "provider", "model", "agent", "repo", "workflow", "node",
];

// ---- the vault instance ---------------------------------------------------------

type VaultCache = Mutex<Vec<(PathBuf, Arc<Vault>)>>;

fn cache() -> &'static VaultCache {
    static C: OnceLock<VaultCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn open_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn cached(core: &Core) -> Option<Arc<Vault>> {
    cache()
        .lock()
        .unwrap()
        .iter()
        .find(|(p, _)| p == core.data_dir())
        .map(|(_, v)| v.clone())
}

async fn default_backend(core: &Core) -> Result<KeyBackend> {
    let file = core.data_dir().join(KEY_FILE);
    if std::env::var(BACKEND_ENV).is_ok_and(|v| v == "file") {
        return Ok(KeyBackend::File(file));
    }
    Ok(KeyBackend::Keyring {
        account: core.server_id().await?,
        fallback_file: file,
    })
}

/// The vault for this core's data directory, opened once per process.
pub async fn vault(core: &Core) -> Result<Arc<Vault>> {
    if let Some(v) = cached(core) {
        return Ok(v);
    }
    let backend = default_backend(core).await?;
    install_vault(core, backend).await
}

/// Opens the vault with `backend` and keeps it for this core (tests and
/// headless hosts use a key file). A vault already open is returned as is.
pub async fn install_vault(core: &Core, backend: KeyBackend) -> Result<Arc<Vault>> {
    let _g = open_lock().lock().await;
    if let Some(v) = cached(core) {
        return Ok(v);
    }
    let v = Arc::new(Vault::open(core.store().clone(), backend).await?);
    cache()
        .lock()
        .unwrap()
        .push((core.data_dir().to_path_buf(), v.clone()));
    Ok(v)
}

#[derive(Clone, Debug, PartialEq)]
pub struct VaultStatus {
    pub source: KeySource,
    /// Set when the master key had to go in a file.
    pub warning: Option<String>,
}

pub async fn vault_status(core: &Core) -> Result<VaultStatus> {
    let v = vault(core).await?;
    Ok(VaultStatus {
        source: v.key_source(),
        warning: v.warning().map(str::to_string),
    })
}

// ---- secrets ------------------------------------------------------------------

/// Stores (or rotates) the key for `provider`. `env` defaults to the
/// provider catalogue's variable.
pub async fn put_secret(
    core: &Core,
    provider: &str,
    env: &str,
    label: &str,
    value: SecretString,
) -> Result<SecretMeta> {
    let env = if env.trim().is_empty() {
        core.store()
            .provider(provider)
            .await?
            .map(|p| p.env)
            .filter(|e| !e.is_empty())
            .ok_or_else(|| anyhow!("name the environment variable for {provider}"))?
    } else {
        env.trim().to_string()
    };
    let replaced = core
        .store()
        .vault_sealed_for(provider)
        .await?
        .iter()
        .any(|s| s.env == env);
    let m = vault(core).await?.put(provider, &env, label, value).await?;
    core.emit(
        kinds::VAULT_SECRET_SAVED,
        &m.id,
        &json!({
            "id": m.id, "provider": m.provider, "env": m.env, "label": m.label,
            "replaced": replaced, "fingerprint": m.fingerprint, "by": "owner",
        }),
    )
    .await?;
    after_save(core, &m.id).await;
    Ok(m)
}

/// Saved first, tested after: the test runs in the background and only
/// sets the indicator.
async fn after_save(core: &Core, id: &str) {
    if probe::auto_test(core).await {
        probe::spawn_probe(core, id, "keleustes");
    }
}

/// Rotates key `id`: the old value stays sealed for the rotation grace
/// (`vault.rotation_grace_hours`) so it can be rolled back, and the lineage
/// records both fingerprints.
pub async fn rotate_secret(core: &Core, id: &str, value: SecretString) -> Result<SecretMeta> {
    lineage::rotate_with_grace(core, id, value, "owner", "").await
}

/// The key already held for `provider` and `env`, if any (a save replaces it).
pub async fn existing_secret(core: &Core, provider: &str, env: &str) -> Result<Option<SecretMeta>> {
    Ok(core
        .store()
        .vault_secrets()
        .await?
        .into_iter()
        .find(|s| s.provider == provider && s.env == env.trim()))
}

/// Changes a key's label without touching its value.
pub async fn set_secret_label(core: &Core, id: &str, label: &str) -> Result<SecretMeta> {
    if label.chars().count() > 120 {
        bail!("a label is at most 120 characters");
    }
    let v = vault(core).await?;
    if !v.set_label(id, label).await? {
        bail!("no such key");
    }
    let m = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    core.emit(
        kinds::VAULT_SECRET_LABEL,
        id,
        &json!({"id": id, "provider": m.provider, "label": m.label, "by": "owner"}),
    )
    .await?;
    Ok(m)
}

/// Integrity check of one key (opens with the master key, fingerprint
/// matches); recorded on the key and audited. Returns the failure reason
/// ("" when it passed).
pub async fn verify_secret(core: &Core, id: &str) -> Result<String> {
    let err = vault(core)
        .await?
        .verify(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    core.emit(
        kinds::VAULT_SECRET_VERIFIED,
        id,
        &json!({"id": id, "ok": err.is_empty(), "reason": err, "by": "owner"}),
    )
    .await?;
    Ok(err)
}

/// The provider slug generic env-var secrets are filed under; a grant for
/// `["env"]` hands out every one the node may have.
pub const ENV_PROVIDER: &str = "env";

/// Stores a generic env-var secret (an MCP token, a registry token). The
/// name is upper case, not reserved (PATH, LD_*, BELAI_*…) and the value
/// at least 8 characters so output scrubbing works.
pub async fn put_env_secret(
    core: &Core,
    name: &str,
    label: &str,
    value: SecretString,
) -> Result<SecretMeta> {
    let name = name.trim().to_ascii_uppercase();
    if !kls_vault::valid_env(&name) {
        bail!("{name:?} is not an environment variable name");
    }
    if kls_vault::reserved_env(&name) {
        bail!("{name} is reserved");
    }
    if value.expose().trim().chars().count() < kls_vault::MIN_ENV_VALUE_LEN {
        bail!(
            "a value is at least {} characters",
            kls_vault::MIN_ENV_VALUE_LEN
        );
    }
    let m = put_secret(core, ENV_PROVIDER, &name, label, value).await?;
    core.store()
        .vault_set_scope(&m.id, m.expires_at, &m.nodes, "env")
        .await?;
    Ok(core.store().vault_secret_meta(&m.id).await?.unwrap_or(m))
}

/// Where and until when a key may be granted. `nodes` empty: every node.
pub async fn set_secret_scope(
    core: &Core,
    id: &str,
    expires_at: Option<i64>,
    nodes: &[String],
) -> Result<SecretMeta> {
    let m = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))?;
    let nodes: Vec<String> = nodes
        .iter()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    core.store()
        .vault_set_scope(id, expires_at, &nodes, &m.kind)
        .await?;
    core.emit(
        kinds::VAULT_SECRET_LABEL,
        id,
        &json!({"id": id, "provider": m.provider, "expiresAt": expires_at, "nodes": nodes,
                "scope": true, "by": "owner"}),
    )
    .await?;
    core.store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such key"))
}

/// A key's lifetime state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifetime {
    Serving,
    /// Expires within 14 days.
    Expiring,
    Expired,
}

pub const EXPIRING_MS: i64 = 14 * 86_400_000;

pub fn lifetime(m: &SecretMeta, now: i64) -> Lifetime {
    match m.expires_at {
        Some(e) if e <= now => Lifetime::Expired,
        Some(e) if e - now <= EXPIRING_MS => Lifetime::Expiring,
        _ => Lifetime::Serving,
    }
}

/// Every vault entry as YAML, values never included ("entries as code").
pub async fn entries_as_code(core: &Core) -> Result<String> {
    let mut out = String::from("# Keleustes vault entries. Values are never exported.\nentries:\n");
    for m in core.store().vault_secrets().await? {
        out.push_str(&format!(
            "  - provider: {}\n    env: {}\n    kind: {}\n    label: {:?}\n    fingerprint: {}\n",
            m.provider,
            m.env,
            if m.kind.is_empty() {
                "provider"
            } else {
                &m.kind
            },
            m.label,
            m.fingerprint
        ));
        if let Some(e) = m.expires_at {
            out.push_str(&format!("    expires_at: {e}\n"));
        }
        if !m.nodes.is_empty() {
            out.push_str(&format!("    nodes: [{}]\n", m.nodes.join(", ")));
        }
    }
    Ok(out)
}

/// Integrity check of every key; returns (passed, failed).
pub async fn verify_all(core: &Core) -> Result<(usize, usize)> {
    let (mut ok, mut bad) = (0, 0);
    for m in core.store().vault_secrets().await? {
        if verify_secret(core, &m.id).await?.is_empty() {
            ok += 1;
        } else {
            bad += 1;
        }
    }
    Ok((ok, bad))
}

/// One line of the vault activity log.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VaultActivity {
    pub at: i64,
    pub kind: String,
    /// "Added", "Replaced", "Rotated", "Label", "Removed", "Tested",
    /// "Checked", "Granted", "Revoked", "Denied".
    pub what: String,
    pub provider: String,
    pub subject: String,
    /// Outcome or detail ("accepted", "HTTP 401", "no key: openai").
    pub result: String,
    pub by: String,
    /// True for refusals and failures (the Denied filter).
    pub denied: bool,
    /// True for saves, rotations, label changes and deletions.
    pub change: bool,
}

/// The vault's activity, newest first, from `vault.*` events.
pub async fn vault_activity(core: &Core, limit: i64) -> Result<Vec<VaultActivity>> {
    let evs = core
        .store()
        .events_with_prefix("vault.", None, limit)
        .await?;
    Ok(evs.iter().filter_map(activity_of).collect())
}

pub fn activity_of(ev: &kls_store::Event) -> Option<VaultActivity> {
    let d: Value = serde_json::from_str(&ev.data_json).unwrap_or_default();
    let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let b = |k: &str| d.get(k).and_then(Value::as_bool).unwrap_or(false);
    let mut a = VaultActivity {
        at: ev.at,
        kind: ev.kind.clone(),
        provider: s("provider"),
        subject: ev.subject.clone(),
        by: match s("by") {
            x if x.is_empty() => "keleustes".into(),
            x => x,
        },
        ..Default::default()
    };
    match ev.kind.as_str() {
        kinds::VAULT_SECRET_SAVED => {
            a.what = if b("rotated") {
                "Rotated"
            } else if b("replaced") {
                "Replaced"
            } else {
                "Added"
            }
            .into();
            a.result = s("env");
            a.change = true;
        }
        kinds::VAULT_SECRET_LABEL => {
            a.what = if b("scope") { "Access" } else { "Label" }.into();
            a.result = s("label");
            a.change = true;
        }
        kinds::VAULT_SECRET_DELETED => {
            a.what = "Removed".into();
            a.change = true;
        }
        kinds::VAULT_SECRET_TESTED => {
            a.what = "Tested".into();
            let outcome = s("outcome");
            a.denied = outcome == "refused";
            a.result = match d.get("status").and_then(Value::as_i64) {
                Some(st) => format!("{outcome} · HTTP {st}"),
                None => outcome,
            };
        }
        kinds::VAULT_SECRET_VERIFIED => {
            a.what = "Checked".into();
            a.denied = !b("ok");
            a.result = if b("ok") {
                "verified".into()
            } else {
                s("reason")
            };
        }
        kinds::VAULT_GRANTED => {
            a.what = "Granted".into();
            let providers: Vec<String> = d
                .get("providers")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let missing: Vec<String> = d
                .get("missing")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            a.provider = providers.join(", ");
            a.denied = !missing.is_empty();
            a.result = if missing.is_empty() {
                s("purpose")
            } else {
                format!("no key: {}", missing.join(", "))
            };
        }
        kinds::VAULT_GRANT_REVOKED => a.what = "Revoked".into(),
        kinds::VAULT_GRANT_REFUSED => {
            a.what = "Denied".into();
            a.denied = true;
            a.result = s("reason");
        }
        _ => return None,
    }
    Some(a)
}

pub async fn delete_secret(core: &Core, id: &str) -> Result<bool> {
    let gone = vault(core).await?.delete(id).await?;
    if gone {
        core.emit(kinds::VAULT_SECRET_DELETED, id, &json!({"id": id}))
            .await?;
    }
    Ok(gone)
}

pub async fn list_secrets(core: &Core) -> Result<Vec<SecretMeta>> {
    core.store().vault_secrets().await
}

/// Keys for `providers`, valid for `ttl_ms`, to attach to a dispatch or job
/// as its `VaultGrant`. `None` when no provider is named or the vault holds
/// no key for any of them (the node may use its own configuration). The
/// grant is logged (providers, secret ids) and a `vault.granted` event is
/// emitted; values never leave this call except in the returned payload.
pub async fn grant_for(
    core: &Core,
    providers: &[String],
    ttl_ms: i64,
) -> Result<Option<GrantPayload>> {
    grant_for_purpose(core, providers, ttl_ms, "node").await
}

/// [`grant_for`] with a purpose recorded in the grant log ("dispatch",
/// "job", "draft"...).
pub async fn grant_for_purpose(
    core: &Core,
    providers: &[String],
    ttl_ms: i64,
    purpose: &str,
) -> Result<Option<GrantPayload>> {
    grant_for_node(core, providers, ttl_ms, purpose, None).await
}

/// [`grant_for_purpose`] for the node that will receive the keys: keys
/// scoped to other nodes, and expired keys, stay behind (`vault.grant_refused`
/// lists them, audited).
pub async fn grant_for_node(
    core: &Core,
    providers: &[String],
    ttl_ms: i64,
    purpose: &str,
    node: Option<&str>,
) -> Result<Option<GrantPayload>> {
    let wanted: Vec<String> = providers
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    let target = node.unwrap_or(purpose);
    // Kill switch first: no key leaves while the factory is paused or stopped.
    if let Err(e) = crate::control::check(core, "grant", target, purpose).await {
        core.emit(
            kinds::VAULT_GRANT_REFUSED,
            target,
            &json!({"providers": wanted, "purpose": purpose, "nodeId": node, "reason": e.to_string()}),
        )
        .await?;
        return Err(e);
    }
    let g = vault(core)
        .await?
        .grant_scoped(&wanted, ttl_ms, purpose, node)
        .await?;
    if !g.denied.is_empty() {
        core.emit(
            kinds::VAULT_GRANT_REFUSED,
            target,
            &json!({"providers": wanted, "purpose": purpose, "nodeId": node,
                    "reason": g.denied.join("; "), "denied": g.denied}),
        )
        .await?;
    }
    if g.secrets.is_empty() {
        return Ok(None);
    }
    core.emit(
        kinds::VAULT_GRANTED,
        &g.grant_id,
        &json!({
            "grantId": g.grant_id,
            "providers": wanted,
            "missing": g.missing,
            "denied": g.denied,
            "expiresAt": g.expires_at_ms,
            "purpose": purpose,
            "nodeId": node,
        }),
    )
    .await?;
    Ok(Some(g))
}

pub async fn revoke_grant(core: &Core, id: &str) -> Result<bool> {
    let done = vault(core).await?.revoke_grant(id).await?;
    if done {
        core.emit(kinds::VAULT_GRANT_REVOKED, id, &json!({"grantId": id}))
            .await?;
    }
    Ok(done)
}

// ---- providers and routing ----------------------------------------------------

pub async fn save_provider(core: &Core, p: &Provider) -> Result<()> {
    if !kls_vault::valid_provider(&p.id) {
        bail!("{:?} is not a provider slug", p.id);
    }
    if !p.env.is_empty() && !kls_vault::valid_env(&p.env) {
        bail!("{:?} is not an environment variable name", p.env);
    }
    core.store().upsert_provider(p).await?;
    core.emit(
        kinds::BUDGET_PROVIDER,
        &p.id,
        &json!({"id": p.id, "factory": p.factory}),
    )
    .await?;
    Ok(())
}

pub async fn delete_provider(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_provider(id).await?;
    if gone {
        core.emit(
            kinds::BUDGET_PROVIDER,
            id,
            &json!({"id": id, "deleted": true}),
        )
        .await?;
    }
    Ok(gone)
}

pub async fn save_rule(core: &Core, r: &RoutingRule) -> Result<RoutingRule> {
    if r.provider.trim().is_empty() {
        bail!("a rule names a provider");
    }
    let mut r = r.clone();
    if r.id.is_empty() {
        r.id = new_id();
    }
    core.store().upsert_routing_rule(&r).await?;
    core.emit(kinds::BUDGET_ROUTING, &r.id, &json!({"id": r.id}))
        .await?;
    Ok(r)
}

pub async fn delete_rule(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_routing_rule(id).await?;
    if gone {
        core.emit(
            kinds::BUDGET_ROUTING,
            id,
            &json!({"id": id, "deleted": true}),
        )
        .await?;
    }
    Ok(gone)
}

pub async fn set_default_model(core: &Core, key: &str, provider_model: &str) -> Result<()> {
    if key != settings::MODELS_DEFAULT && key != settings::MODELS_FAST {
        bail!("not a model setting: {key}");
    }
    core.store().set_setting(key, provider_model.trim()).await?;
    core.emit(kinds::BUDGET_ROUTING, key, &json!({"setting": key}))
        .await?;
    Ok(())
}

/// What a piece of work looks like to the router.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteQuery {
    /// "owner/name".
    pub repo: String,
    pub agent: String,
    /// The board card's labels.
    pub labels: Vec<String>,
    /// Route key: "code_smart", "code_fast", "planner_reviewer", "images"...
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub provider: String,
    pub model: String,
    /// "rule:<id>", "agent", "default", "catalogue".
    pub source: String,
}

fn glob(pattern: &str, s: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => s.starts_with(prefix),
        None => pattern.eq_ignore_ascii_case(s),
    }
}

fn split_pm(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(match s.split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (s.to_string(), String::new()),
    })
}

/// Picks `(provider, model)` for a card, repo or agent: the first enabled
/// rule (by priority) whose non-empty matchers all match and whose provider
/// factory work may use; then the agent profile's own choice; then the
/// `models.default` setting; then the first factory provider of the
/// catalogue. A blank model becomes the provider's default model.
pub fn route(
    q: &RouteQuery,
    rules: &[RoutingRule],
    agent: Option<&AgentProfile>,
    providers: &[Provider],
    default: Option<&str>,
) -> Option<Route> {
    let usable = |id: &str| {
        providers
            .iter()
            .find(|p| p.id == id)
            .is_none_or(|p| p.factory)
    };
    let fill = |provider: String, model: String, source: String| {
        let model = if model.is_empty() {
            providers
                .iter()
                .find(|p| p.id == provider)
                .map(|p| p.default_model.clone())
                .unwrap_or_default()
        } else {
            model
        };
        Route {
            provider,
            model,
            source,
        }
    };
    let mut sorted: Vec<&RoutingRule> = rules.iter().filter(|r| r.enabled).collect();
    sorted.sort_by_key(|r| r.priority);
    for r in sorted {
        let ok = (r.match_repo.is_empty() || glob(&r.match_repo, &q.repo))
            && (r.match_agent.is_empty() || glob(&r.match_agent, &q.agent))
            && (r.match_label.is_empty() || q.labels.iter().any(|l| l == &r.match_label))
            && (r.match_kind.is_empty() || r.match_kind == q.kind);
        if ok && usable(&r.provider) {
            return Some(fill(
                r.provider.clone(),
                r.model.clone(),
                format!("rule:{}", r.id),
            ));
        }
    }
    if let Some(a) = agent.filter(|a| !a.provider.is_empty() && usable(&a.provider)) {
        return Some(fill(a.provider.clone(), a.model.clone(), "agent".into()));
    }
    if let Some((p, m)) = default.and_then(split_pm).filter(|(p, _)| usable(p)) {
        return Some(fill(p, m, "default".into()));
    }
    providers
        .iter()
        .find(|p| p.factory)
        .map(|p| fill(p.id.clone(), String::new(), "catalogue".into()))
}

/// The `(provider, model)` `models.agents` names for `agent`
/// (`agent=provider/model, other=provider/model`).
pub fn agent_model(map: &str, agent: &str) -> Option<(String, String)> {
    map.split(',').find_map(|e| {
        let (a, pm) = e.split_once('=')?;
        if a.trim().eq_ignore_ascii_case(agent.trim()) {
            split_pm(pm)
        } else {
            None
        }
    })
}

/// [`route`] against the stored rules, profile and defaults. The catalogue
/// fallback only considers providers the vault holds a key for.
pub async fn route_for(core: &Core, q: &RouteQuery) -> Result<Option<Route>> {
    let s = core.store();
    let rules = s.routing_rules().await?;
    let agent = if q.agent.is_empty() {
        None
    } else {
        s.agent_profile_by_name(&q.agent).await?
    };
    let keyed: Vec<String> = s
        .vault_secrets()
        .await?
        .into_iter()
        .map(|m| m.provider)
        .collect();
    let providers = s.providers().await?;
    let default = s.setting(settings::MODELS_DEFAULT).await?;
    let r = route(q, &rules, agent.as_ref(), &providers, default.as_deref());
    // Settings > Models: a per-agent model beats the profile and the default
    // (routing rules still win).
    if !q.agent.is_empty()
        && !r.as_ref().is_some_and(|r| r.source.starts_with("rule:"))
        && let Some(map) = s.setting(settings::MODELS_AGENTS).await?
        && let Some((p, m)) = agent_model(&map, &q.agent)
    {
        return Ok(Some(Route {
            provider: p,
            model: m,
            source: "agent_map".into(),
        }));
    }
    if r.as_ref().is_some_and(|r| r.source == "catalogue") {
        let with_key: Vec<Provider> = providers
            .into_iter()
            .filter(|p| keyed.contains(&p.id))
            .collect();
        return Ok(route(&RouteQuery::default(), &[], None, &with_key, None));
    }
    Ok(r)
}

// ---- budgets ------------------------------------------------------------------

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Start (Unix ms, UTC) of the budget period containing `now_ms`: the day,
/// the ISO week (Monday), the month, or 0 for "total".
pub fn period_start(period: &str, now_ms: i64) -> i64 {
    const DAY: i64 = 86_400_000;
    let days = now_ms.div_euclid(DAY);
    match period {
        "day" => days * DAY,
        // 1970-01-01 was a Thursday (weekday 3 counting Monday as 0).
        "week" => (days - (days + 3).rem_euclid(7)) * DAY,
        "month" => {
            let (_, _, d) = civil(days);
            (days - (d - 1)) * DAY
        }
        _ => 0,
    }
}

/// A budget with what it has spent this period.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub budget: Budget,
    pub period_start: i64,
    pub spent_tokens: i64,
    pub spent_usd: f64,
    /// Highest of the token and USD percentages (0 without limits).
    pub pct: f64,
}

/// A spend about to happen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub provider: String,
    pub model: String,
    pub agent: String,
    pub repo: String,
    pub tokens: i64,
    pub usd: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetHit {
    pub budget_id: String,
    pub name: String,
    /// Percentage after the planned spend.
    pub pct: f64,
    pub action: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BudgetCheck {
    Allow,
    Warn(BudgetHit),
    Block(BudgetHit),
}

fn provider_list(b: &Budget) -> Vec<String> {
    if b.provider.trim().is_empty() || b.provider.trim() == "all" {
        return Vec::new();
    }
    b.provider
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Where a spend happens beyond what [`Spend`] names: the workflow and the
/// node. Workflow and node budgets only apply when it is known.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendScope {
    pub workflow: String,
    pub node: String,
}

/// Whether budget `b` limits spend `s`.
pub fn budget_applies(b: &Budget, s: &Spend) -> bool {
    budget_applies_in(b, s, &SpendScope::default())
}

/// [`budget_applies`] with the workflow and node known.
pub fn budget_applies_in(b: &Budget, s: &Spend, scope: &SpendScope) -> bool {
    let subj = b.subject.trim();
    match b.applies_to.as_str() {
        "workflow" => return !subj.is_empty() && subj == scope.workflow,
        "node" => return !subj.is_empty() && subj == scope.node,
        // The whole factory: no provider list narrows it.
        "org" => return true,
        _ => {}
    }
    let providers = provider_list(b);
    if !providers.is_empty() && !providers.contains(&s.provider) {
        return false;
    }
    let subj = b.subject.trim();
    match b.applies_to.as_str() {
        "all" | "" => true,
        "provider" => subj.is_empty() || subj == s.provider,
        "model" => subj == s.model || subj == format!("{}/{}", s.provider, s.model),
        "agent" => subj == s.agent,
        "repo" => glob(subj, &s.repo),
        _ => false,
    }
}

fn usage_filter(b: &Budget, since: i64) -> UsageFilter {
    let mut f = UsageFilter {
        since,
        providers: provider_list(b),
        ..Default::default()
    };
    let subj = b.subject.trim().to_string();
    match b.applies_to.as_str() {
        "org" => f.providers.clear(),
        "provider" if !subj.is_empty() => f.providers = vec![subj],
        "model" => f.model = subj,
        "agent" => f.agent = subj,
        "repo" => f.repo = subj,
        _ => {}
    }
    f
}

fn pct_of(spent_tokens: i64, spent_usd: f64, b: &Budget) -> f64 {
    let t = b
        .limit_tokens
        .filter(|l| *l > 0)
        .map(|l| spent_tokens as f64 * 100.0 / l as f64)
        .unwrap_or(0.0);
    let u = b
        .limit_usd
        .filter(|l| *l > 0.0)
        .map(|l| spent_usd * 100.0 / l)
        .unwrap_or(0.0);
    t.max(u)
}

/// Evaluates a planned spend against every budget that limits it. The
/// tightest wins: any budget at its limit with a stopping action
/// (`hard_stop`, `pause_run`, `escalate_human`) blocks; a budget at its limit
/// with `alert_only` or `downgrade_to_fast`, or past its lowest alert
/// threshold, warns. Among equals the highest percentage is reported.
pub fn check_budget(statuses: &[BudgetStatus], spend: &Spend) -> BudgetCheck {
    check_budget_in(statuses, spend, &SpendScope::default())
}

/// [`check_budget`] with the workflow and node known.
pub fn check_budget_in(
    statuses: &[BudgetStatus],
    spend: &Spend,
    scope: &SpendScope,
) -> BudgetCheck {
    let mut block: Option<BudgetHit> = None;
    let mut warn: Option<BudgetHit> = None;
    for st in statuses
        .iter()
        .filter(|st| budget_applies_in(&st.budget, spend, scope))
    {
        let b = &st.budget;
        let pct = pct_of(
            st.spent_tokens + spend.tokens.max(0),
            st.spent_usd + spend.usd.max(0.0),
            b,
        );
        let hit = |reason: String| BudgetHit {
            budget_id: b.id.clone(),
            name: b.name.clone(),
            pct,
            action: b.action_at_limit.clone(),
            reason,
        };
        let keep = |slot: &mut Option<BudgetHit>, h: BudgetHit| {
            if slot.as_ref().is_none_or(|o| h.pct > o.pct) {
                *slot = Some(h);
            }
        };
        if pct > 100.0 || (pct >= 100.0 && (spend.tokens > 0 || spend.usd > 0.0)) {
            let stops = matches!(
                b.action_at_limit.as_str(),
                "hard_stop" | "pause_run" | "escalate_human"
            );
            let h = hit(format!(
                "{} is at {:.0}% of its {} limit",
                b.name, pct, b.period
            ));
            if stops {
                keep(&mut block, h);
            } else {
                keep(&mut warn, h);
            }
        } else if let Some(t) = b.alert_at.iter().copied().filter(|t| *t > 0).min()
            && pct >= t as f64
        {
            keep(
                &mut warn,
                hit(format!("{} passed {}% ({:.0}%)", b.name, t, pct)),
            );
        }
    }
    match (block, warn) {
        (Some(b), _) => BudgetCheck::Block(b),
        (None, Some(w)) => BudgetCheck::Warn(w),
        _ => BudgetCheck::Allow,
    }
}

/// Tokens and USD the runs of workflow `id` spent since `since` (their
/// sessions' totals plus run tokens no session carries).
async fn workflow_spend(core: &Core, id: &str, since: i64) -> Result<(i64, f64)> {
    if id.is_empty() {
        return Ok((0, 0.0));
    }
    let s = core.store();
    let runs = s
        .runs(&kls_store::engine::RunFilter {
            workflow_id: Some(id.to_string()),
            from: Some(since),
            limit: 5000,
            ..Default::default()
        })
        .await?;
    let (mut t, mut u) = (0i64, 0f64);
    for r in runs {
        let sessions = s
            .sessions(&kls_store::sessions::SessionQuery {
                run_id: Some(r.id.clone()),
                limit: 500,
                ..Default::default()
            })
            .await?;
        let st: i64 = sessions
            .iter()
            .map(|x| x.input_tokens + x.output_tokens)
            .sum();
        t += st.max(r.tokens);
        u += sessions.iter().map(|x| x.cost_usd.max(0.0)).sum::<f64>();
    }
    Ok((t, u))
}

/// The workflow a `run:<id>` origin belongs to ("" otherwise).
async fn workflow_of_origin(core: &Core, origin: &str) -> String {
    match origin.strip_prefix("run:") {
        Some(id) => core
            .store()
            .run(id)
            .await
            .ok()
            .flatten()
            .map(|r| r.workflow_id)
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Every budget with its spend this period (usage synced first).
pub async fn budget_statuses(core: &Core) -> Result<Vec<BudgetStatus>> {
    sync_usage(core).await?;
    let now = kls_store::now_ms();
    let mut out = Vec::new();
    for b in core.store().budgets().await? {
        let since = period_start(&b.period, now);
        let (t, u) = match b.applies_to.as_str() {
            "workflow" => workflow_spend(core, b.subject.trim(), since).await?,
            "node" => {
                let rows = core.store().usage_rows(since, i64::MAX).await?;
                rows.iter()
                    .filter(|r| r.node_id == b.subject.trim())
                    .fold((0, 0.0), |(t, u), r| {
                        (t + r.input_tokens + r.output_tokens, u + r.cost_usd)
                    })
            }
            _ => core.store().usage_sum(&usage_filter(&b, since)).await?,
        };
        out.push(BudgetStatus {
            pct: pct_of(t, u, &b),
            budget: b,
            period_start: since,
            spent_tokens: t,
            spent_usd: u,
        });
    }
    Ok(out)
}

/// [`check_budget`] against the stored budgets; a block emits `budget.limit`.
pub async fn check_budget_now(core: &Core, spend: &Spend) -> Result<BudgetCheck> {
    let st = budget_statuses(core).await?;
    let c = check_budget(&st, spend);
    if let BudgetCheck::Block(h) = &c {
        core.emit(kinds::BUDGET_LIMIT, &h.budget_id, h).await?;
    }
    Ok(c)
}

/// What the budget gate says about model work about to start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum BudgetGate {
    Allow,
    /// Go ahead on the fast model instead.
    Downgrade {
        provider: String,
        model: String,
        hit: BudgetHit,
    },
    /// Pause the run (`pause_run`).
    Pause(BudgetHit),
    /// Ask a person first (`escalate_human`).
    Escalate(BudgetHit),
    /// Fail the step, `budget_exceeded` (`hard_stop`).
    Block(BudgetHit),
}

impl BudgetGate {
    /// Whether the work may start now (possibly downgraded).
    pub fn proceeds(&self) -> bool {
        matches!(self, BudgetGate::Allow | BudgetGate::Downgrade { .. })
    }

    pub fn hit(&self) -> Option<&BudgetHit> {
        match self {
            BudgetGate::Allow => None,
            BudgetGate::Downgrade { hit, .. }
            | BudgetGate::Pause(hit)
            | BudgetGate::Escalate(hit)
            | BudgetGate::Block(hit) => Some(hit),
        }
    }
}

/// Settings read by the gate.
pub mod budget_settings {
    /// Action for a budget at its limit that names none (also the default
    /// action of a new budget).
    pub const ON_EXCEED: &str = "budget.on_exceed";
    /// Factory-wide alert thresholds, percent, added to each budget's own.
    pub const ALERTS: &str = "budget.alerts";
}

fn parse_alerts(s: &str) -> Vec<i64> {
    s.split([',', ' '])
        .filter_map(|x| x.trim().trim_end_matches('%').parse::<i64>().ok())
        .filter(|p| (1..=100).contains(p))
        .collect()
}

/// The default budget action (`budget.on_exceed`, default `pause_run`).
pub async fn default_budget_action(core: &Core) -> String {
    core.store()
        .setting(budget_settings::ON_EXCEED)
        .await
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| BUDGET_ACTIONS.contains(&s.as_str()))
        .unwrap_or_else(|| "pause_run".into())
}

/// **The budget gate.** Call before any model work (dispatch, job,
/// decision) with what it is planned to spend and its origin
/// (`run:<id>`, `agent:<name>`...). The tightest budget wins. Alert
/// thresholds crossed (each budget's own plus `budget.alerts`) are recorded
/// once per period as `budget.alert`; anything but Allow/Downgrade is a
/// `budget.blocked` event. Both are audited.
pub async fn budget_gate(core: &Core, spend: &Spend, origin: &str) -> Result<BudgetGate> {
    let scope = SpendScope {
        workflow: workflow_of_origin(core, origin).await,
        node: String::new(),
    };
    budget_gate_scoped(core, spend, &scope, origin).await
}

/// [`budget_gate`] with the workflow and node known, so workflow and node
/// budgets apply. (`budget_gate` works the workflow out from a `run:<id>`
/// origin.)
pub async fn budget_gate_scoped(
    core: &Core,
    spend: &Spend,
    scope: &SpendScope,
    origin: &str,
) -> Result<BudgetGate> {
    let statuses = budget_statuses(core).await?;
    let global = core
        .store()
        .setting(budget_settings::ALERTS)
        .await?
        .map(|s| parse_alerts(&s))
        .unwrap_or_else(|| vec![50, 80, 100]);
    let default_action = default_budget_action(core).await;
    let statuses: Vec<BudgetStatus> = statuses
        .into_iter()
        .map(|mut st| {
            if st.budget.action_at_limit.trim().is_empty() {
                st.budget.action_at_limit = default_action.clone();
            }
            st
        })
        .collect();
    for st in statuses
        .iter()
        .filter(|st| budget_applies_in(&st.budget, spend, scope))
    {
        let b = &st.budget;
        let pct = pct_of(
            st.spent_tokens + spend.tokens.max(0),
            st.spent_usd + spend.usd.max(0.0),
            b,
        );
        let mut thresholds: Vec<i64> = b
            .alert_at
            .iter()
            .copied()
            .chain(global.iter().copied())
            .collect();
        thresholds.sort_unstable();
        thresholds.dedup();
        for t in thresholds.into_iter().filter(|t| pct >= *t as f64) {
            if core
                .store()
                .budget_alert_once(&b.id, st.period_start, t)
                .await?
            {
                core.emit(
                    kinds::BUDGET_ALERT,
                    &b.id,
                    &json!({
                        "budgetId": b.id, "name": b.name, "pct": t, "at": pct.round(),
                        "period": b.period, "origin": origin,
                    }),
                )
                .await?;
            }
        }
    }
    let gate = match check_budget_in(&statuses, spend, scope) {
        BudgetCheck::Allow => BudgetGate::Allow,
        BudgetCheck::Warn(h) if h.pct >= 100.0 && h.action == "downgrade_to_fast" => {
            let fast = core.store().setting(settings::MODELS_FAST).await?;
            match fast.as_deref().and_then(split_pm) {
                Some((provider, model)) => BudgetGate::Downgrade {
                    provider,
                    model,
                    hit: h,
                },
                // No fast model to go to: the provider's own fast model.
                None => {
                    let model = core
                        .store()
                        .provider(&spend.provider)
                        .await?
                        .map(|p| p.fast_model)
                        .unwrap_or_default();
                    if model.is_empty() {
                        BudgetGate::Pause(h)
                    } else {
                        BudgetGate::Downgrade {
                            provider: spend.provider.clone(),
                            model,
                            hit: h,
                        }
                    }
                }
            }
        }
        BudgetCheck::Warn(_) => BudgetGate::Allow,
        BudgetCheck::Block(h) => match h.action.as_str() {
            "hard_stop" => BudgetGate::Block(h),
            "escalate_human" => BudgetGate::Escalate(h),
            _ => BudgetGate::Pause(h),
        },
    };
    if let Some(h) = gate.hit() {
        let verdict = match &gate {
            BudgetGate::Downgrade { .. } => "downgrade",
            BudgetGate::Pause(_) => "pause",
            BudgetGate::Escalate(_) => "escalate",
            _ => "block",
        };
        core.emit(
            if gate.proceeds() {
                kinds::BUDGET_LIMIT
            } else {
                kinds::BUDGET_BLOCKED
            },
            &h.budget_id,
            &json!({
                "budgetId": h.budget_id, "name": h.name, "pct": h.pct.round(),
                "action": h.action, "outcome": verdict, "reason": h.reason,
                "origin": origin, "workflowId": scope.workflow, "nodeId": scope.node,
                "provider": spend.provider, "model": spend.model,
                "agent": spend.agent, "repo": spend.repo, "tokens": spend.tokens,
            }),
        )
        .await?;
    }
    Ok(gate)
}

pub async fn save_budget(core: &Core, b: &Budget) -> Result<Budget> {
    let mut b = b.clone();
    if b.name.trim().is_empty() {
        bail!("a budget has a name");
    }
    if !BUDGET_SCOPES.contains(&b.applies_to.as_str()) {
        bail!("unknown budget scope {:?}", b.applies_to);
    }
    if !matches!(b.applies_to.as_str(), "all" | "org" | "provider") && b.subject.trim().is_empty() {
        bail!("say which {} the budget limits", b.applies_to);
    }
    if !BUDGET_PERIODS.contains(&b.period.as_str()) {
        bail!("unknown budget period {:?}", b.period);
    }
    if b.action_at_limit.trim().is_empty() {
        b.action_at_limit = default_budget_action(core).await;
    }
    if !BUDGET_ACTIONS.contains(&b.action_at_limit.as_str()) {
        bail!("unknown action {:?}", b.action_at_limit);
    }
    if b.limit_tokens.is_none_or(|l| l <= 0) && b.limit_usd.is_none_or(|l| l <= 0.0) {
        bail!("set a token or USD limit");
    }
    if b.alert_at.iter().any(|p| !(1..=100).contains(p)) {
        bail!("alert percentages are 1 to 100");
    }
    if b.provider.trim().is_empty() {
        b.provider = "all".into();
    }
    if b.id.is_empty() {
        b.id = new_id();
    }
    let rev =
        core.store().save_budget(&b).await?.ok_or_else(|| {
            anyhow!("the budget changed since you opened it; reload and try again")
        })?;
    b.revision = rev;
    core.emit(
        kinds::BUDGET_SAVED,
        &b.id,
        &json!({"id": b.id, "revision": rev}),
    )
    .await?;
    Ok(b)
}

pub async fn delete_budget(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_budget(id).await?;
    if gone {
        core.emit(kinds::BUDGET_DELETED, id, &json!({"id": id}))
            .await?;
    }
    Ok(gone)
}

// ---- usage --------------------------------------------------------------------

fn field<'a>(v: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let obj = v.get("session").filter(|s| s.is_object()).unwrap_or(v);
    names
        .iter()
        .find_map(|n| obj.get(*n).or_else(|| v.get(*n)))
        .filter(|x| !x.is_null())
}

fn text(v: &Value, names: &[&str]) -> String {
    field(v, names)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn int(v: &Value, names: &[&str]) -> Option<i64> {
    field(v, names).and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
}

/// A usage row from a `session.*` event, when it carries token counts.
pub fn usage_from_session_event(ev: &kls_store::Event) -> Option<UsageRow> {
    let v: Value = serde_json::from_str(&ev.data_json).ok()?;
    let input = int(&v, &["input_tokens", "inputTokens"]);
    let output = int(&v, &["output_tokens", "outputTokens"]);
    let cost = field(&v, &["cost_usd", "costUsd"]).and_then(|x| x.as_f64());
    if input.is_none() && output.is_none() && cost.is_none() {
        return None;
    }
    let mut sid = text(&v, &["session_id", "sessionId"]);
    if sid.is_empty() {
        sid = text(&v, &["id"]);
    }
    if sid.is_empty() {
        sid = ev.subject.clone();
    }
    if sid.is_empty() {
        return None;
    }
    Some(UsageRow {
        id: format!("session:{sid}"),
        at: ev.at,
        source: "session".into(),
        session_id: sid,
        node_id: text(&v, &["node_id", "nodeId"]),
        provider: text(&v, &["provider"]),
        model: text(&v, &["model"]),
        agent: text(&v, &["agent"]),
        repo: text(&v, &["repo"]),
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
        cost_usd: cost.unwrap_or(0.0),
    })
}

/// Folds `session.*` events newer than the stored cursor into usage rows
/// (session totals are cumulative, so the latest event wins). Events without
/// token counts, or no session events at all, are fine. Returns rows written.
pub async fn sync_usage(core: &Core) -> Result<usize> {
    let s = core.store();
    let mut cursor: i64 = s
        .setting(settings::USAGE_CURSOR)
        .await?
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut written = 0;
    loop {
        let evs = s.events_after(cursor, 500).await?;
        let Some(last) = evs.last().map(|e| e.id) else {
            break;
        };
        for ev in evs.iter().filter(|e| e.kind.starts_with("session.")) {
            if let Some(u) = usage_from_session_event(ev) {
                s.upsert_usage(&u).await?;
                written += 1;
            }
        }
        cursor = last;
        s.set_setting(settings::USAGE_CURSOR, &cursor.to_string())
            .await?;
    }
    Ok(written)
}

/// Records spend reported for a job (for example a profile draft).
pub async fn record_usage(core: &Core, u: &UsageRow) -> Result<()> {
    let mut u = u.clone();
    if u.id.is_empty() {
        u.id = format!("job:{}", new_id());
    }
    if u.at == 0 {
        u.at = kls_store::now_ms();
    }
    core.store().upsert_usage(&u).await?;
    core.emit(kinds::BUDGET_USAGE, &u.id, &json!({"id": u.id}))
        .await?;
    Ok(())
}

/// One host's gauge on the Budgets screen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostGauge {
    pub node_id: String,
    pub name: String,
    pub tokens_7d: i64,
    pub cost_7d: f64,
    /// Share of all tokens in the window.
    pub pct: f64,
    pub by_provider: Vec<UsageTotal>,
}

/// Everything the Budgets screen shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BudgetsView {
    pub budgets: Vec<BudgetStatus>,
    pub providers: Vec<Provider>,
    pub secrets: Vec<SecretMeta>,
    pub rules: Vec<RoutingRule>,
    pub default_model: String,
    pub fast_model: String,
    pub by_provider: Vec<UsageTotal>,
    pub by_model: Vec<UsageTotal>,
    pub by_agent: Vec<UsageTotal>,
    pub by_repo: Vec<UsageTotal>,
    pub usage: Vec<UsageRow>,
    pub hosts: Vec<HostGauge>,
    /// Human-equivalent estimate of the range's sessions.
    pub estimate: crate::estimates::Estimate,
    /// Performance per agent over the range.
    pub performance: Vec<crate::estimates::PerfRow>,
    pub grants: Vec<GrantRecord>,
    pub vault_warning: Option<String>,
    pub vault_error: Option<String>,
    /// Tool calls over the range, per agent (the tool-call bars).
    pub tool_calls: Vec<ToolCallTotal>,
}

/// Tool calls the sessions of one agent made.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallTotal {
    pub key: String,
    pub calls: i64,
    pub sessions: i64,
}

/// Tool calls per agent of the sessions with usage since `since` (each
/// session's latest count, from its belai facts), busiest first.
pub async fn tool_calls_by_agent(core: &Core, since: i64) -> Result<Vec<ToolCallTotal>> {
    let s = core.store();
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<ToolCallTotal> = Vec::new();
    for r in s.usage_rows(since, 5000).await? {
        if r.session_id.is_empty() || seen.contains(&r.session_id) {
            continue;
        }
        seen.push(r.session_id.clone());
        let Some(x) = s.session_extras(&r.session_id).await? else {
            continue;
        };
        if x.tool_calls <= 0 {
            continue;
        }
        let key = if r.agent.is_empty() {
            "(none)".to_string()
        } else {
            r.agent.clone()
        };
        match out.iter_mut().find(|t| t.key == key) {
            Some(t) => {
                t.calls += x.tool_calls;
                t.sessions += 1;
            }
            None => out.push(ToolCallTotal {
                key,
                calls: x.tool_calls,
                sessions: 1,
            }),
        }
    }
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.key.cmp(&b.key)));
    Ok(out)
}

/// Loads the Budgets screen for the last `range_days` days of usage.
pub async fn budgets_view(core: &Core, range_days: i64) -> Result<BudgetsView> {
    let s = core.store();
    let since = kls_store::now_ms() - range_days.max(1) * 86_400_000;
    let (vault_warning, vault_error) = match vault_status(core).await {
        Ok(st) => (st.warning, None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let week = kls_store::now_ms() - 7 * 86_400_000;
    let nodes = s.nodes().await?;
    let per_node = s.usage_by(week, "node").await?;
    let total: i64 = per_node.iter().map(|u| u.tokens).sum();
    let week_rows = s.usage_rows(week, i64::MAX).await?;
    let mut hosts = Vec::new();
    for n in nodes.iter().filter(|n| n.revoked_at.is_none()) {
        let mine = per_node.iter().find(|u| u.key == n.id);
        let mut by_provider: Vec<UsageTotal> = Vec::new();
        for r in week_rows.iter().filter(|r| r.node_id == n.id) {
            let t = r.input_tokens + r.output_tokens;
            match by_provider.iter_mut().find(|u| u.key == r.provider) {
                Some(u) => {
                    u.tokens += t;
                    u.cost_usd += r.cost_usd;
                }
                None => by_provider.push(UsageTotal {
                    key: r.provider.clone(),
                    tokens: t,
                    cost_usd: r.cost_usd,
                }),
            }
        }
        let tokens = mine.map(|u| u.tokens).unwrap_or(0);
        hosts.push(HostGauge {
            node_id: n.id.clone(),
            name: n.name.clone(),
            tokens_7d: tokens,
            cost_7d: mine.map(|u| u.cost_usd).unwrap_or(0.0),
            pct: if total > 0 {
                tokens as f64 * 100.0 / total as f64
            } else {
                0.0
            },
            by_provider,
        });
    }
    Ok(BudgetsView {
        budgets: budget_statuses(core).await?,
        providers: s.providers().await?,
        secrets: s.vault_secrets().await?,
        rules: s.routing_rules().await?,
        default_model: s
            .setting(settings::MODELS_DEFAULT)
            .await?
            .unwrap_or_default(),
        fast_model: s.setting(settings::MODELS_FAST).await?.unwrap_or_default(),
        by_provider: s.usage_by(since, "provider").await?,
        by_model: s.usage_by(since, "model").await?,
        by_agent: s.usage_by(since, "agent").await?,
        by_repo: s.usage_by(since, "repo").await?,
        usage: s.usage_rows(since, 200).await?,
        hosts,
        estimate: crate::estimates::for_range(core, since).await?,
        performance: crate::estimates::performance(core, since, crate::estimates::GroupBy::Agent)
            .await?,
        grants: s.vault_grants(50).await?,
        vault_warning,
        vault_error,
        tool_calls: tool_calls_by_agent(core, since).await?,
    })
}

/// Everything the Vault screen shows. Values are never part of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VaultView {
    pub providers: Vec<Provider>,
    pub secrets: Vec<SecretMeta>,
    pub grants: Vec<GrantRecord>,
    pub activity: Vec<VaultActivity>,
    pub source: Option<KeySource>,
    pub vault_warning: Option<String>,
    pub vault_error: Option<String>,
}

pub async fn vault_view(core: &Core) -> Result<VaultView> {
    let s = core.store();
    let (source, vault_warning, vault_error) = match vault_status(core).await {
        Ok(st) => (Some(st.source), st.warning, None),
        Err(e) => (None, None, Some(format!("{e:#}"))),
    };
    Ok(VaultView {
        providers: s.providers().await?,
        secrets: s.vault_secrets().await?,
        grants: s.vault_grants(100).await?,
        activity: vault_activity(core, 300).await?,
        source,
        vault_warning,
        vault_error,
    })
}
// ---- agents -------------------------------------------------------------------

/// Saved agents and crews reaching nodes, crew checks, the avatar job and
/// worker controls (WP5).
pub mod workforce;
pub use workforce::{
    Published, Sent, crew_problems, crew_to_library, draw_avatar, publish_agent, publish_crew,
    save_and_publish_agent, save_and_publish_crew,
};

/// A name: lower-case letters, digits, `-`, `_`, `:`; 1..64 long.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | ':' | '.')
        })
}

/// Turns free text into a name ("Patch Bot!" -> "patch-bot").
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(64).collect()
}

/// Keeps an SVG avatar only when it looks like inert SVG markup (no script,
/// event handlers, external references or embedded HTML); else empty.
pub fn sanitize_svg(svg: &str) -> String {
    let s = svg.trim();
    let lower = s.to_ascii_lowercase();
    let ok = lower.starts_with("<svg")
        && (lower.ends_with("</svg>") || lower.ends_with("/>"))
        && s.len() <= 64 * 1024
        && ![
            "<script",
            "javascript:",
            "foreignobject",
            "<iframe",
            "href=\"http",
            "href='http",
        ]
        .iter()
        .any(|bad| lower.contains(bad))
        && !lower
            .split(|c: char| c.is_whitespace())
            .any(|tok| tok.starts_with("on") && tok.contains('='));
    if ok { s.to_string() } else { String::new() }
}

pub async fn save_agent(core: &Core, a: &AgentProfile) -> Result<AgentProfile> {
    let mut a = a.clone();
    a.name = a.name.trim().to_string();
    if !valid_name(&a.name) {
        bail!(
            "agent names are lower-case letters, digits and - _ : . (got {:?})",
            a.name
        );
    }
    if let Some(other) = core.store().agent_profile_by_name(&a.name).await?
        && other.id != a.id
    {
        bail!("an agent named {} already exists", a.name);
    }
    if !a.avatar_svg.is_empty() {
        let clean = sanitize_svg(&a.avatar_svg);
        if clean.is_empty() {
            bail!("the avatar is not plain SVG");
        }
        a.avatar_svg = clean;
    }
    if !a.meta_json.trim().is_empty() {
        serde_json::from_str::<Value>(&a.meta_json).context("routing extras are not JSON")?;
    }
    a.tools.retain(|t| !t.trim().is_empty());
    if a.id.is_empty() {
        a.id = new_id();
    }
    a.version = core.store().save_agent_profile(&a).await?;
    core.emit(
        kinds::AGENT_SAVED,
        &a.id,
        &json!({"id": a.id, "name": a.name, "version": a.version}),
    )
    .await?;
    Ok(a)
}

pub async fn delete_agent(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_agent_profile(id).await?;
    if gone {
        core.emit(kinds::AGENT_DELETED, id, &json!({"id": id}))
            .await?;
    }
    Ok(gone)
}

/// A profile drafted on a node. Every field is a suggestion the builder
/// keeps editable; nothing is saved until the person saves.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentDraft {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub tools: Vec<String>,
    pub avatar_svg: String,
    /// The node that drafted it.
    pub node_id: String,
}

fn draft_from(v: &Value) -> AgentDraft {
    let p = v
        .get("profile")
        .or_else(|| v.get("draft"))
        .filter(|x| x.is_object())
        .unwrap_or(v);
    let s = |names: &[&str]| -> String {
        names
            .iter()
            .find_map(|n| p.get(*n).and_then(|x| x.as_str()))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let tools = match p.get("tools") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|t| {
                t.as_str()
                    .map(str::to_string)
                    .or_else(|| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
            })
            .collect(),
        Some(Value::String(s)) => s
            .split([',', '\n'])
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let display = s(&["display_name", "displayName", "title"]);
    let mut name = s(&["name", "slug"]);
    if !valid_name(&name) {
        name = slugify(if name.is_empty() { &display } else { &name });
    }
    AgentDraft {
        name,
        display_name: display,
        description: s(&["description", "summary"]),
        prompt: s(&["prompt", "system_prompt", "systemPrompt", "instructions"]),
        provider: s(&["provider"]),
        model: s(&["model"]),
        tools,
        avatar_svg: sanitize_svg(&s(&["avatar_svg", "avatarSvg", "avatar"])),
        node_id: String::new(),
    }
}

/// Drafts an agent profile from a brief. Keleustes cannot call a model, so
/// this runs a `model` job (`{"task":"draft","brief":..}`) on a node that
/// can reach `provider` (routed when `None`). The gateway attaches the vault
/// grant for `JobNeeds.provider` (see [`grant_for`]).
pub async fn draft_agent(
    core: &Core,
    brief: &str,
    provider: Option<&str>,
    timeout_ms: i64,
) -> Result<AgentDraft> {
    let brief = brief.trim();
    if brief.is_empty() {
        bail!("describe the agent first");
    }
    crate::control::check(core, "job", "agent.draft", "owner").await?;
    let (provider, model) = match provider.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            let m = core
                .store()
                .provider(p)
                .await?
                .map(|x| x.default_model)
                .unwrap_or_default();
            (p.to_string(), m)
        }
        None => {
            let q = RouteQuery {
                kind: "planner_reviewer".into(),
                ..Default::default()
            };
            let r = route_for(core, &q)
                .await?
                .ok_or_else(|| anyhow!("no provider to draft with; add a key under Budgets"))?;
            (r.provider, r.model)
        }
    };
    let gw = core.gateway()?;
    let payload = json!({
        "task": "draft",
        "brief": brief,
        "provider": provider,
        "model": model,
        "want": ["name", "display_name", "description", "prompt", "tools"],
    });
    let out = gw
        .job(
            None,
            "model",
            &payload.to_string(),
            JobNeeds {
                gh: false,
                provider: Some(provider.clone()),
                ..Default::default()
            },
            timeout_ms,
        )
        .await?;
    if out.status != "ok" {
        bail!(
            "the node could not draft the agent ({}): {}",
            out.status,
            if out.error.is_empty() {
                "no detail"
            } else {
                &out.error
            }
        );
    }
    let v: Value =
        serde_json::from_str(&out.output_json).context("the node's draft is not JSON")?;
    let mut d = draft_from(&v);
    if d.provider.is_empty() {
        d.provider = provider.clone();
    }
    if d.model.is_empty() {
        d.model = model;
    }
    d.node_id = out.node_id.clone();
    if let Some(u) = v.get("usage") {
        let n = |k: &[&str]| {
            k.iter()
                .find_map(|k| u.get(*k).and_then(|x| x.as_i64()))
                .unwrap_or(0)
        };
        record_usage(
            core,
            &UsageRow {
                source: "job".into(),
                node_id: out.node_id.clone(),
                provider: provider.clone(),
                model: d.model.clone(),
                input_tokens: n(&["input_tokens", "inputTokens"]),
                output_tokens: n(&["output_tokens", "outputTokens"]),
                cost_usd: u
                    .get("cost_usd")
                    .or_else(|| u.get("costUsd"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                ..Default::default()
            },
        )
        .await?;
    }
    core.emit(
        kinds::AGENT_DRAFTED,
        &d.name,
        &json!({"name": d.name, "provider": provider, "node": out.node_id}),
    )
    .await?;
    Ok(d)
}

// ---- crews --------------------------------------------------------------------

pub async fn save_crew(core: &Core, c: &Crew) -> Result<Crew> {
    let mut c = c.clone();
    c.name = c.name.trim().to_string();
    if !valid_name(&c.name) {
        bail!(
            "crew names are lower-case letters, digits and - _ : . (got {:?})",
            c.name
        );
    }
    if c.members.iter().any(|m| m.agent.trim().is_empty()) {
        bail!("every member names an agent");
    }
    if c.process.is_empty() {
        c.process = "sequential".into();
    }
    for m in &mut c.members {
        m.count = m.count.max(1);
    }
    // The website's validateCrew, enforced here so every caller (UI, CLI,
    // workflows) is held to it.
    let problems = crew_problems(core, &c).await?;
    if !problems.is_empty() {
        return Err(anyhow::Error::new(crate::sessions::Refusal::Invalid(
            problems.join("; "),
        )));
    }
    if c.id.is_empty() {
        c.id = new_id();
    }
    if core
        .store()
        .crews()
        .await?
        .iter()
        .any(|o| o.name == c.name && o.id != c.id)
    {
        bail!("a crew named {} already exists", c.name);
    }
    c.version = core.store().save_crew(&c).await?;
    core.emit(
        kinds::CREW_SAVED,
        &c.id,
        &json!({"id": c.id, "name": c.name, "version": c.version}),
    )
    .await?;
    Ok(c)
}

pub async fn delete_crew(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_crew(id).await?;
    if gone {
        core.emit(kinds::CREW_DELETED, id, &json!({"id": id}))
            .await?;
    }
    Ok(gone)
}

/// Live workers per node, read from `session.*` events (the latest state of
/// each worker session) and the node list. Tolerates no session events.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeWorkers {
    pub node_id: String,
    pub name: String,
    pub online: bool,
    pub providers: Vec<String>,
    pub live_workers: usize,
    /// Agents of the live workers, with counts.
    pub agents: Vec<(String, usize)>,
}

pub async fn workers_summary(core: &Core) -> Result<Vec<NodeWorkers>> {
    let online = match core.gateway() {
        Ok(g) => g.online().await,
        Err(_) => Vec::new(),
    };
    // session id -> (node, agent, live)
    let mut sessions: Vec<(String, String, String, bool)> = Vec::new();
    for ev in core.store().recent_events(2000).await? {
        if !ev.kind.starts_with("session.") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&ev.data_json) else {
            continue;
        };
        let mut sid = text(&v, &["session_id", "sessionId", "id"]);
        if sid.is_empty() {
            sid = ev.subject.clone();
        }
        let kind = text(&v, &["kind"]);
        let state = text(&v, &["state"]);
        let ended = ev.kind.ends_with("ended") || state == "ended";
        let node = text(&v, &["node_id", "nodeId"]);
        let agent = text(&v, &["agent"]);
        match sessions.iter_mut().find(|s| s.0 == sid) {
            Some(s) => {
                if !node.is_empty() {
                    s.1 = node;
                }
                if !agent.is_empty() {
                    s.2 = agent;
                }
                s.3 = !ended && (s.3 || kind == "worker");
            }
            None if kind == "worker" => sessions.push((sid, node, agent, !ended)),
            None => {}
        }
    }
    let mut out = Vec::new();
    for n in core.store().nodes().await? {
        if n.revoked_at.is_some() {
            continue;
        }
        let mut agents: Vec<(String, usize)> = Vec::new();
        let mut live = 0;
        for s in sessions.iter().filter(|s| s.3 && s.1 == n.id) {
            live += 1;
            let a = if s.2.is_empty() { "worker" } else { &s.2 };
            match agents.iter_mut().find(|x| x.0 == a) {
                Some(x) => x.1 += 1,
                None => agents.push((a.to_string(), 1)),
            }
        }
        out.push(NodeWorkers {
            online: online.contains(&n.id),
            node_id: n.id,
            name: n.name,
            providers: n.host.capabilities.providers,
            live_workers: live,
            agents,
        });
    }
    Ok(out)
}

// ---- library ------------------------------------------------------------------

/// Saves a new version of a library item (`id` empty creates one). Returns
/// the item id and the version number.
pub async fn save_library(
    core: &Core,
    id: &str,
    kind: &str,
    name: &str,
    description: &str,
    body: &str,
    note: &str,
) -> Result<(String, i64)> {
    if !LIBRARY_KINDS.contains(&kind) && kind != AGENT_KIND && kind != CREW_KIND {
        bail!(
            "library items are prompts, skills, commands, processes, hooks, MCP servers, \
             repositories, budgets, provider sets or rewrite tables"
        );
    }
    let name = name.trim();
    if !valid_name(name) {
        bail!("names are lower-case letters, digits and - _ : . (got {name:?})");
    }
    let mut id = id.to_string();
    if id.is_empty()
        && let Some(o) = core.store().library_item_by_name(kind, name).await?
    {
        id = o.id;
    }
    // The bundle files the item's latest version carries: the hook validator
    // checks them, and a new version keeps them.
    let carried = match kind {
        "hook" if !id.is_empty() => match core.store().library_latest(&id).await? {
            Some(prev) => {
                let mut out = Vec::new();
                for e in core.store().bundle_files(&id, prev.version).await? {
                    if let Some(data) = core.store().library_blob(&e.sha256).await? {
                        out.push((e.path, data));
                    }
                }
                out
            }
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    // belai's validators (WP0): a hand-made item is held to what a host
    // accepts. JSON kinds are stored in canonical form.
    let body = match validate_library_body(kind, name, body, &carried) {
        Ok(b) => b,
        Err(e) => {
            let data =
                json!({"kind": kind, "name": name, "reason": e.to_string(), "outcome": "invalid"});
            core.emit(crate::libsearch::kinds::LIBRARY_REFUSED, name, &data)
                .await?;
            core.audit_record(kls_store::audit::LedgerDraft {
                kind: "library".into(),
                actor: "owner".into(),
                actor_kind: "user".into(),
                msg: format!("{kind} {name} refused: {e}"),
                subject: json!({ "kind": kind, "name": name }),
                payload: data,
                ..Default::default()
            })
            .await?;
            return Err(anyhow::Error::new(crate::sessions::Refusal::Invalid(
                e.to_string(),
            )));
        }
    };
    let body = body.as_str();
    if let Some(o) = core.store().library_item_by_name(kind, name).await?
        && o.id != id
    {
        if !id.is_empty() {
            bail!("a {kind} named {name} already exists");
        }
        // Saving under an existing name adds a version to it.
        id = o.id;
    }
    if let Some(prev) = core.store().library_latest(&id).await?
        && prev.body == body
        && core
            .store()
            .library_item(&id)
            .await?
            .is_some_and(|i| i.name == name && i.description == description.trim())
    {
        // Nothing changed: no new version.
        return Ok((id, prev.version));
    }
    if id.is_empty() {
        id = new_id();
    }
    let v = core
        .store()
        .save_library_version(&id, kind, name, description.trim(), body, note.trim())
        .await?;
    if !carried.is_empty() {
        core.store().save_bundle_files(&id, v, &carried).await?;
    }
    core.emit(
        kinds::LIBRARY_SAVED,
        &id,
        &json!({"id": id, "kind": kind, "name": name, "version": v}),
    )
    .await?;
    Ok((id, v))
}

/// Backs an agent profile up to the library (kind `agent`): each call with
/// a changed profile is a new version. Returns (item id, version).
pub async fn agent_to_library(core: &Core, agent_id: &str, note: &str) -> Result<(String, i64)> {
    let a = core
        .store()
        .agent_profile(agent_id)
        .await?
        .ok_or_else(|| anyhow!("no such agent"))?;
    let body = serde_json::to_string_pretty(&json!({
        "name": a.name, "displayName": a.display_name, "description": a.description,
        "prompt": a.prompt, "provider": a.provider, "model": a.model, "tools": a.tools,
        "avatarSvg": a.avatar_svg, "meta": serde_json::from_str::<Value>(&a.meta_json).unwrap_or(json!({})),
        "agentVersion": a.version,
    }))?;
    save_library(core, "", AGENT_KIND, &a.name, &a.description, &body, note).await
}

/// Installs (or updates) the agent a library `agent` item holds, at
/// `version` (latest when `None`). The profile records where it came from
/// (`meta.installedFrom`), and `library.installed` is emitted (audited).
pub async fn install_agent_from_library(
    core: &Core,
    item_id: &str,
    version: Option<i64>,
) -> Result<AgentProfile> {
    let s = core.store();
    let item = s
        .library_item(item_id)
        .await?
        .ok_or_else(|| anyhow!("no such library item"))?;
    if item.kind != AGENT_KIND {
        bail!("{} is a {}, not an agent", item.name, item.kind);
    }
    let v = match version {
        Some(n) => s
            .library_versions(item_id)
            .await?
            .into_iter()
            .find(|v| v.version == n),
        None => s.library_latest(item_id).await?,
    }
    .ok_or_else(|| anyhow!("no such version"))?;
    let b: Value = serde_json::from_str(&v.body).context("the library agent is not JSON")?;
    let st = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut meta = b.get("meta").cloned().unwrap_or(json!({}));
    if !meta.is_object() {
        meta = json!({});
    }
    meta["installedFrom"] =
        json!({"itemId": item.id, "version": v.version, "at": kls_store::now_ms()});
    let existing = s.agent_profile_by_name(&st("name")).await?;
    let a = AgentProfile {
        id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_default(),
        version: existing.as_ref().map(|e| e.version).unwrap_or(0),
        name: st("name"),
        display_name: st("displayName"),
        description: st("description"),
        prompt: st("prompt"),
        provider: st("provider"),
        model: st("model"),
        tools: b
            .get("tools")
            .and_then(|t| serde_json::from_value(t.clone()).ok())
            .unwrap_or_default(),
        avatar_svg: st("avatarSvg"),
        meta_json: meta.to_string(),
        ..Default::default()
    };
    let saved = save_agent(core, &a).await?;
    core.emit(
        kinds::LIBRARY_INSTALLED,
        &item.id,
        &json!({"itemId": item.id, "version": v.version, "agent": saved.name, "agentId": saved.id, "by": "owner"}),
    )
    .await?;
    Ok(saved)
}

/// Library agents with whether each is installed and at which version.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryAgent {
    pub item: LibraryItem,
    pub installed: bool,
    /// The library version the installed profile came from, if it did.
    pub installed_version: Option<i64>,
}

pub async fn library_agents(core: &Core) -> Result<Vec<LibraryAgent>> {
    let s = core.store();
    let mut out = Vec::new();
    for item in s.library_items(AGENT_KIND).await? {
        let p = s.agent_profile_by_name(&item.name).await?;
        let from = p
            .as_ref()
            .and_then(|p| serde_json::from_str::<Value>(&p.meta_json).ok())
            .and_then(|m| m.pointer("/installedFrom/version").and_then(Value::as_i64));
        out.push(LibraryAgent {
            installed: p.is_some(),
            installed_version: from,
            item,
        });
    }
    Ok(out)
}

pub async fn delete_library(core: &Core, id: &str) -> Result<bool> {
    let gone = core.store().delete_library_item(id).await?;
    if gone {
        core.emit(kinds::LIBRARY_DELETED, id, &json!({"id": id}))
            .await?;
    }
    Ok(gone)
}

/// Holds a library item to belai's validators before it is saved (WP0),
/// returning the body to store. Hooks and MCP servers are validated whole
/// (`kls_libsearch` `validate::hook` / `validate::mcp`, a hook with the
/// bundle files it carries) and stored in belai's canonical JSON; the other
/// JSON kinds are canonical objects with only the keys belai accepts. A
/// skill, command or prompt written in belai's front-matter form is
/// validated as one; plain Markdown is a local prompt and kept as written.
/// The document's own name must be the item's.
pub fn validate_library_body(
    kind: &str,
    name: &str,
    body: &str,
    files: &[(String, Vec<u8>)],
) -> Result<String> {
    use kls_libsearch::convert::{canonical, validate};
    let same_name = |doc_name: &str| -> Result<()> {
        if doc_name != name {
            bail!("the document is named {doc_name:?}, not {name:?}");
        }
        Ok(())
    };
    match kind {
        "hook" | "mcp" => {
            let doc = canonical::json(body.as_bytes(), canonical::MAX_JSON_DEPTH)?;
            let doc_name = if kind == "hook" {
                let files: Vec<kls_libsearch::BundleFile> = files
                    .iter()
                    .map(|(p, d)| kls_libsearch::BundleFile {
                        path: p.clone(),
                        sha256: canonical::hash(d),
                        data: d.clone(),
                    })
                    .collect();
                validate::hook(&doc, &files)?.0
            } else {
                validate::mcp(&doc)?
            };
            same_name(&doc_name)?;
            Ok(String::from_utf8(doc)?)
        }
        k if JSON_ITEM_KEYS.iter().any(|(j, _)| *j == k) => {
            let keys = JSON_ITEM_KEYS
                .iter()
                .find(|(j, _)| *j == k)
                .map(|(_, ks)| *ks)
                .unwrap_or_default();
            let doc = canonical::json(body.as_bytes(), canonical::MAX_JSON_DEPTH)?;
            let v: Value = serde_json::from_slice(&doc)?;
            let m = v
                .as_object()
                .ok_or_else(|| anyhow!("the document must be a JSON object"))?;
            if let Some(bad) = m.keys().find(|key| !keys.contains(&key.as_str())) {
                bail!("{k}.{bad} is not a field belai accepts");
            }
            let doc_name = m
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{k}.name is required"))?;
            if k == "rewrite" && doc_name != "bash_rewrite" {
                bail!("the rewrite table is named \"bash_rewrite\"");
            }
            same_name(doc_name)?;
            Ok(String::from_utf8(doc)?)
        }
        "skill" | "command" | "prompt" if body.trim_start().starts_with("---") => {
            let doc = canonical::markdown(body)?;
            let (doc_name, _) = validate::library(kind, doc.as_bytes(), &[])?;
            same_name(&doc_name)?;
            Ok(doc)
        }
        _ => Ok(body.to_string()),
    }
}

/// The `source:` and `source_sha256:` an exported file carries, if any. An
/// import keeps them as the version's note only, never as provenance.
pub fn library_file_source(text: &str) -> Option<(String, String)> {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))?;
    let head = &rest[..rest.find("\n---")?];
    let (mut url, mut sha) = (String::new(), String::new());
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v: String = v
                .trim()
                .trim_matches('"')
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect();
            match k.trim() {
                "source" => url = v,
                "source_sha256" => sha = v,
                _ => {}
            }
        }
    }
    (!url.is_empty()).then_some((url, sha))
}

/// The file form of a library item: Markdown with a small front matter.
pub fn library_file(item: &LibraryItem, body: &str) -> String {
    library_file_with_source(item, body, None)
}

/// [`library_file`] with the item's origin (`source:` and `source_sha256:`)
/// for an imported item. A re-import keeps them as a note only, never as
/// trusted provenance.
pub fn library_file_with_source(
    item: &LibraryItem,
    body: &str,
    source: Option<(&str, &str)>,
) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], " ");
    let mut head = format!(
        "---\nkind: {}\nname: {}\ndescription: {}\nversion: {}\n",
        item.kind,
        item.name,
        one_line(&item.description),
        item.latest_version,
    );
    if let Some((url, sha)) = source
        && !url.is_empty()
    {
        head.push_str(&format!("source: {}\n", one_line(url)));
        if !sha.is_empty() {
            head.push_str(&format!("source_sha256: {}\n", one_line(sha)));
        }
    }
    format!("{head}---\n{body}")
}

/// The origin URL and upstream hash of an imported library item.
pub async fn library_source(core: &Core, item: &LibraryItem) -> Option<(String, String)> {
    let p = crate::libsearch::origin(core, &item.kind, &item.id)
        .await
        .ok()
        .flatten()?;
    let url = serde_json::from_str::<Vec<String>>(&p.source_urls_json)
        .ok()
        .and_then(|v| v.into_iter().next())
        .unwrap_or_else(|| p.source.clone());
    Some((url, p.upstream_sha256))
}

/// Parses [`library_file`] output: `(kind, name, description, body)`. Kind
/// and name may be missing (taken from the path by the importer).
pub fn parse_library_file(text: &str) -> (String, String, String, String) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = t
        .strip_prefix("---\n")
        .or_else(|| t.strip_prefix("---\r\n"))
    else {
        return (String::new(), String::new(), String::new(), t.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (String::new(), String::new(), String::new(), t.to_string());
    };
    let (head, tail) = rest.split_at(end);
    let body = tail
        .strip_prefix("\n---")
        .unwrap_or(tail)
        .trim_start_matches('\r')
        .strip_prefix('\n')
        .unwrap_or_default();
    let (mut kind, mut name, mut desc) = (String::new(), String::new(), String::new());
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "kind" => kind = v,
                "name" => name = v,
                "description" => desc = v,
                _ => {}
            }
        }
    }
    (kind, name, desc, body.to_string())
}

/// Writes one item's latest version to `path`.
pub async fn export_library_item(core: &Core, id: &str, path: &Path) -> Result<()> {
    let item = core
        .store()
        .library_item(id)
        .await?
        .ok_or_else(|| anyhow!("no such library item"))?;
    let v = core.store().library_latest(id).await?.unwrap_or_default();
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let src = library_source(core, &item).await;
    let file = library_file_with_source(
        &item,
        &v.body,
        src.as_ref().map(|(u, s)| (u.as_str(), s.as_str())),
    );
    tokio::fs::write(path, file).await?;
    Ok(())
}

/// Writes every item to `<dir>/<kind>/<name>.md`. Returns how many.
pub async fn export_library(core: &Core, dir: &Path) -> Result<usize> {
    let items = core.store().library_items("").await?;
    for it in &items {
        let p = dir.join(&it.kind).join(format!("{}.md", it.name));
        export_library_item(core, &it.id, &p).await?;
    }
    Ok(items.len())
}

fn collect_files(path: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if path.is_dir() {
        if depth > 4 {
            return Ok(());
        }
        for e in std::fs::read_dir(path)? {
            collect_files(&e?.path(), out, depth + 1)?;
        }
    } else if path
        .extension()
        .is_some_and(|e| e == "md" || e == "txt" || e == "markdown")
    {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// Imports a file or a directory of them (as [`export_library`] writes).
/// A file without front matter takes its kind from its folder (`prompt` or
/// `prompts`...) and its name from the file name. Returns how many imported.
pub async fn import_library(core: &Core, path: &Path) -> Result<usize> {
    let mut files = Vec::new();
    let p = path.to_path_buf();
    let files = tokio::task::spawn_blocking(move || {
        collect_files(&p, &mut files, 0)?;
        anyhow::Ok(files)
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    let mut n = 0;
    for f in files {
        let meta = tokio::fs::metadata(&f).await?;
        if meta.len() > 1024 * 1024 {
            bail!("{} is larger than 1 MiB", f.display());
        }
        let text = tokio::fs::read_to_string(&f)
            .await
            .with_context(|| format!("read {}", f.display()))?;
        let (mut kind, mut name, desc, body) = parse_library_file(&text);
        if kind.is_empty() {
            kind = f
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|d| d.to_str())
                .map(|d| d.trim_end_matches('s').to_string())
                .unwrap_or_default();
        }
        if !LIBRARY_KINDS.contains(&kind.as_str()) {
            kind = "prompt".into();
        }
        if name.is_empty() {
            name = slugify(f.file_stem().and_then(|s| s.to_str()).unwrap_or_default());
        }
        // An exported item's origin rides along as the version's note.
        let note = match library_file_source(&text) {
            Some((url, sha)) if sha.is_empty() => format!("imported; source: {url}"),
            Some((url, sha)) => format!("imported; source: {url} (sha256 {sha})"),
            None => "imported".to_string(),
        };
        save_library(core, "", &kind, &name, &desc, &body, &note).await?;
        n += 1;
    }
    core.emit(
        kinds::LIBRARY_IMPORTED,
        "",
        &json!({"count": n, "from": path.display().to_string()}),
    )
    .await?;
    Ok(n)
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DispatchSpec, JobOutcome, NodeGateway};

    const KEY: &str = "sk-ant-core-test-secret-value-42";

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        install_vault(&c, KeyBackend::File(dir.path().join(KEY_FILE)))
            .await
            .unwrap();
        (c, dir)
    }

    #[derive(Default)]
    struct FakeGateway {
        jobs: Mutex<Vec<(String, String, JobNeeds)>>,
        reply: String,
        status: String,
    }

    #[async_trait::async_trait]
    impl NodeGateway for FakeGateway {
        async fn online(&self) -> Vec<String> {
            vec!["n1".into()]
        }
        async fn dispatch(&self, _node_id: &str, _spec: DispatchSpec) -> Result<String> {
            Ok("d1".into())
        }
        async fn job(
            &self,
            _node_id: Option<&str>,
            kind: &str,
            payload_json: &str,
            needs: JobNeeds,
            _timeout_ms: i64,
        ) -> Result<JobOutcome> {
            self.jobs
                .lock()
                .unwrap()
                .push((kind.into(), payload_json.into(), needs));
            Ok(JobOutcome {
                status: self.status.clone(),
                output_json: self.reply.clone(),
                error: "boom".into(),
                node_id: "n1".into(),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn grant_for_attaches_keys_and_logs_without_values() {
        let (c, _d) = core().await;
        assert!(grant_for(&c, &[], 1000).await.unwrap().is_none());
        assert!(
            grant_for(&c, &["anthropic".into()], 1000)
                .await
                .unwrap()
                .is_none()
        );
        let m = put_secret(&c, "anthropic", "", "prod", SecretString::new(KEY))
            .await
            .unwrap();
        assert_eq!(m.env, "ANTHROPIC_API_KEY");
        let g = grant_for(&c, &["anthropic".into(), "openai".into()], 60_000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(g.secrets[0].value.expose(), KEY);
        assert_eq!(g.missing, vec!["openai".to_string()]);
        assert!(!format!("{g:?}").contains(KEY));
        // No event, setting or grant row holds the value.
        for ev in c.store().recent_events(100).await.unwrap() {
            assert!(!ev.data_json.contains(KEY), "{}", ev.kind);
        }
        let log = c.store().vault_grants(10).await.unwrap();
        assert_eq!(log.len(), 1);
        assert!(revoke_grant(&c, &g.grant_id).await.unwrap());
        rotate_secret(&c, &m.id, SecretString::new("k2"))
            .await
            .unwrap();
        assert!(delete_secret(&c, &m.id).await.unwrap());
        assert!(list_secrets(&c).await.unwrap().is_empty());
        assert!(vault_status(&c).await.unwrap().warning.is_none());
    }

    fn status(b: Budget, tokens: i64, usd: f64) -> BudgetStatus {
        BudgetStatus {
            pct: pct_of(tokens, usd, &b),
            budget: b,
            period_start: 0,
            spent_tokens: tokens,
            spent_usd: usd,
        }
    }

    fn budget(id: &str, applies: &str, subject: &str, limit: i64, action: &str) -> Budget {
        Budget {
            id: id.into(),
            name: id.into(),
            applies_to: applies.into(),
            subject: subject.into(),
            provider: "all".into(),
            period: "month".into(),
            limit_tokens: Some(limit),
            alert_at: vec![80, 100],
            action_at_limit: action.into(),
            ..Default::default()
        }
    }

    #[test]
    fn budget_verdicts() {
        let spend = Spend {
            provider: "anthropic".into(),
            model: "m".into(),
            agent: "patcher".into(),
            repo: "acme/api".into(),
            tokens: 100,
            usd: 0.0,
        };
        assert_eq!(check_budget(&[], &spend), BudgetCheck::Allow);
        let low = status(budget("low", "all", "", 10_000, "hard_stop"), 100, 0.0);
        assert_eq!(
            check_budget(std::slice::from_ref(&low), &spend),
            BudgetCheck::Allow
        );
        let warn = status(budget("w", "agent", "patcher", 1000, "hard_stop"), 750, 0.0);
        assert!(matches!(
            check_budget(&[low.clone(), warn.clone()], &spend),
            BudgetCheck::Warn(h) if h.budget_id == "w"
        ));
        let full = status(budget("f", "repo", "acme/*", 1000, "hard_stop"), 950, 0.0);
        assert!(matches!(
            check_budget(&[low.clone(), warn.clone(), full], &spend),
            BudgetCheck::Block(h) if h.budget_id == "f"
        ));
        // At the limit with a non-stopping action only warns.
        let soft = status(budget("s", "all", "", 1000, "downgrade_to_fast"), 1000, 0.0);
        assert!(matches!(
            check_budget(&[soft], &spend),
            BudgetCheck::Warn(h) if h.action == "downgrade_to_fast"
        ));
        // Not applicable: other agent, other provider filter.
        let other = status(budget("o", "agent", "builder", 10, "hard_stop"), 10, 0.0);
        let mut pf = budget("p", "all", "", 10, "hard_stop");
        pf.provider = "openai, mistral".into();
        let pf = status(pf, 10, 0.0);
        assert_eq!(check_budget(&[other, pf], &spend), BudgetCheck::Allow);
        // USD limits count too.
        let mut usd = budget("u", "provider", "anthropic", 0, "pause_run");
        usd.limit_tokens = None;
        usd.limit_usd = Some(1.0);
        let usd = status(usd, 0, 0.99);
        let pricey = Spend {
            usd: 0.5,
            ..spend.clone()
        };
        assert!(matches!(
            check_budget(&[usd], &pricey),
            BudgetCheck::Block(_)
        ));
    }

    #[test]
    fn periods() {
        // 2026-11-21T15:30:00Z is a Saturday.
        let now = 1_795_275_000_000;
        assert_eq!(period_start("day", now), 1_795_219_200_000); // 2026-11-21
        assert_eq!(period_start("week", now), 1_794_787_200_000); // Mon 2026-11-16
        assert_eq!(period_start("month", now), 1_793_491_200_000); // 2026-11-01
        assert_eq!(period_start("total", now), 0);
        assert_eq!(civil(0), (1970, 1, 1));
    }

    #[tokio::test]
    async fn budget_gate_stops_work_and_alerts_once() {
        let (c, _d) = core().await;
        let mk = |name: &str, action: &str, limit: i64| Budget {
            name: name.into(),
            applies_to: "provider".into(),
            subject: "anthropic".into(),
            period: "month".into(),
            limit_tokens: Some(limit),
            alert_at: vec![80],
            action_at_limit: action.into(),
            ..Default::default()
        };
        let spend = |t: i64| Spend {
            provider: "anthropic".into(),
            model: "m".into(),
            tokens: t,
            ..Default::default()
        };
        // No budgets: allowed.
        assert_eq!(
            budget_gate(&c, &spend(10), "run:r1").await.unwrap(),
            BudgetGate::Allow
        );
        let hard = save_budget(&c, &mk("hard", "hard_stop", 1000))
            .await
            .unwrap();
        record_usage(
            &c,
            &UsageRow {
                id: "u1".into(),
                at: kls_store::now_ms(),
                provider: "anthropic".into(),
                input_tokens: 900,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // 90%: allowed, the 80% alert (and the global 50/80) recorded once.
        assert!(
            budget_gate(&c, &spend(0), "run:r1")
                .await
                .unwrap()
                .proceeds()
        );
        assert!(
            budget_gate(&c, &spend(0), "run:r1")
                .await
                .unwrap()
                .proceeds()
        );
        let alerts = |c: &Core| {
            let c = c.clone();
            async move {
                c.store()
                    .recent_events(200)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|e| e.kind == kinds::BUDGET_ALERT)
                    .count()
            }
        };
        assert_eq!(alerts(&c).await, 2, "50 and 80, once each");
        // Planned spend over the limit: hard stop blocks the dispatch.
        let g = budget_gate(&c, &spend(200), "run:r1").await.unwrap();
        assert!(matches!(g, BudgetGate::Block(_)));
        assert!(!g.proceeds());
        // Pause and escalate map from their actions; tightest wins.
        delete_budget(&c, &hard.id).await.unwrap();
        let p = save_budget(&c, &mk("pause", "pause_run", 1000))
            .await
            .unwrap();
        assert!(matches!(
            budget_gate(&c, &spend(200), "x").await.unwrap(),
            BudgetGate::Pause(_)
        ));
        delete_budget(&c, &p.id).await.unwrap();
        let e = save_budget(&c, &mk("ask", "escalate_human", 1000))
            .await
            .unwrap();
        assert!(matches!(
            budget_gate(&c, &spend(200), "x").await.unwrap(),
            BudgetGate::Escalate(_)
        ));
        delete_budget(&c, &e.id).await.unwrap();
        // Downgrade goes to models.fast.
        c.store()
            .set_setting(settings::MODELS_FAST, "anthropic/haiku")
            .await
            .unwrap();
        save_budget(&c, &mk("fast", "downgrade_to_fast", 1000))
            .await
            .unwrap();
        match budget_gate(&c, &spend(200), "x").await.unwrap() {
            BudgetGate::Downgrade {
                provider, model, ..
            } => {
                assert_eq!((provider.as_str(), model.as_str()), ("anthropic", "haiku"))
            }
            g => panic!("{g:?}"),
        }
        // A budget saved without an action takes budget.on_exceed.
        c.store()
            .set_setting(budget_settings::ON_EXCEED, "hard_stop")
            .await
            .unwrap();
        let d = save_budget(&c, &mk("default", "", 5000)).await.unwrap();
        assert_eq!(d.action_at_limit, "hard_stop");
        // Refusals are audited.
        c.audit_sync().await.unwrap();
        let ledger = c
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(ledger.iter().any(|e| e.msg.contains("budget.blocked")));
        assert!(ledger.iter().any(|e| e.msg.contains("budget.alert")));
    }

    #[tokio::test]
    async fn vault_label_activity_verify_and_kill_switch_grant() {
        let (c, _d) = core().await;
        c.store()
            .set_setting(probe::AUTO_TEST_SETTING, "off")
            .await
            .unwrap();
        let m = put_secret(&c, "openai", "", "main", SecretString::new(KEY))
            .await
            .unwrap();
        assert!(
            existing_secret(&c, "openai", "OPENAI_API_KEY")
                .await
                .unwrap()
                .is_some()
        );
        let m2 = set_secret_label(&c, &m.id, "prod").await.unwrap();
        assert_eq!(m2.label, "prod");
        assert_eq!(verify_secret(&c, &m.id).await.unwrap(), "");
        assert_eq!(verify_all(&c).await.unwrap(), (1, 0));
        put_secret(&c, "openai", "", "", SecretString::new("sk-other-0000000"))
            .await
            .unwrap();
        let act = vault_activity(&c, 50).await.unwrap();
        let whats: Vec<&str> = act.iter().map(|a| a.what.as_str()).collect();
        assert_eq!(whats[0], "Replaced");
        assert!(whats.contains(&"Added") && whats.contains(&"Label") && whats.contains(&"Checked"));
        assert!(act.iter().all(|a| !format!("{a:?}").contains(KEY)));
        // Kill switch: no key leaves; refusal audited.
        crate::control::pause(&c, "test").await.unwrap();
        assert!(grant_for(&c, &["openai".into()], 60_000).await.is_err());
        let act = vault_activity(&c, 50).await.unwrap();
        assert!(act.iter().any(|a| a.what == "Denied" && a.denied));
        crate::control::resume(&c).await.unwrap();
        assert!(
            grant_for(&c, &["openai".into()], 60_000)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn org_budget_covers_the_factory_and_tool_call_bars() {
        let (c, _d) = core().await;
        let b = save_budget(
            &c,
            &Budget {
                name: "Factory month".into(),
                applies_to: "org".into(),
                // A provider list does not narrow an org budget.
                provider: "anthropic".into(),
                period: "month".into(),
                limit_tokens: Some(1000),
                alert_at: vec![50, 80, 100],
                action_at_limit: "hard_stop".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        for (sid, prov, agent, n, tools) in [
            ("s1", "anthropic", "builder", 300, 7),
            ("s2", "openai", "reviewer", 400, 3),
            ("s3", "openai", "builder", 100, 2),
        ] {
            c.emit(
                "session.updated",
                sid,
                &json!({"session_id": sid, "provider": prov, "agent": agent, "input_tokens": n}),
            )
            .await
            .unwrap();
            c.store()
                .save_session_extras(&kls_store::belai::SessionExtras {
                    session_id: sid.into(),
                    tool_calls: tools,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let st = budget_statuses(&c).await.unwrap();
        assert_eq!(st[0].spent_tokens, 800, "every provider counts");
        for p in ["openai", "mistral"] {
            let s = Spend {
                provider: p.into(),
                tokens: 300,
                ..Default::default()
            };
            assert!(budget_applies(&b, &s));
            assert!(matches!(
                check_budget_now(&c, &s).await.unwrap(),
                BudgetCheck::Block(_)
            ));
        }
        let view = budgets_view(&c, 7).await.unwrap();
        assert_eq!(
            view.tool_calls,
            vec![
                ToolCallTotal {
                    key: "builder".into(),
                    calls: 9,
                    sessions: 2
                },
                ToolCallTotal {
                    key: "reviewer".into(),
                    calls: 3,
                    sessions: 1
                },
            ]
        );
        // The save and the block are audited budget entries.
        c.audit_sync().await.unwrap();
        let led = c
            .audit_entries(&kls_store::audit::LedgerQuery {
                kinds: vec!["budget".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(led.iter().any(|e| e.msg.contains("budget.saved")));
        assert!(led.iter().any(|e| e.msg.contains("budget.limit")));
    }

    #[tokio::test]
    async fn budgets_and_usage_from_session_events() {
        let (c, _d) = core().await;
        // No session events at all: fine.
        assert_eq!(sync_usage(&c).await.unwrap(), 0);
        let b = save_budget(
            &c,
            &Budget {
                name: "Anthropic month".into(),
                applies_to: "provider".into(),
                subject: "anthropic".into(),
                period: "month".into(),
                limit_tokens: Some(1000),
                alert_at: vec![50],
                action_at_limit: "hard_stop".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(b.revision, 1);
        assert!(
            save_budget(
                &c,
                &Budget {
                    revision: 0,
                    ..b.clone()
                }
            )
            .await
            .is_err()
        );
        assert!(
            save_budget(
                &c,
                &Budget {
                    name: "".into(),
                    ..b.clone()
                }
            )
            .await
            .is_err()
        );

        c.emit(
            "session.updated",
            "s1",
            &json!({"session_id": "s1", "provider": "anthropic", "model": "m", "node_id": "n1",
                    "input_tokens": 300, "output_tokens": 100, "cost_usd": 0.2}),
        )
        .await
        .unwrap();
        // Cumulative: a later upsert replaces the earlier totals.
        c.emit(
            "session.updated",
            "s1",
            &json!({"session": {"sessionId": "s1", "provider": "anthropic", "inputTokens": 500, "outputTokens": 200}}),
        )
        .await
        .unwrap();
        c.emit("session.updated", "s2", &json!({"title": "no usage"}))
            .await
            .unwrap();
        let st = budget_statuses(&c).await.unwrap();
        assert_eq!(st[0].spent_tokens, 700);
        assert!((st[0].pct - 70.0).abs() < 1e-9);
        let spend = Spend {
            provider: "anthropic".into(),
            tokens: 400,
            ..Default::default()
        };
        assert!(matches!(
            check_budget_now(&c, &spend).await.unwrap(),
            BudgetCheck::Block(_)
        ));
        let view = budgets_view(&c, 7).await.unwrap();
        assert_eq!(view.by_provider[0].tokens, 700);
        assert!(view.providers.iter().any(|p| p.id == "anthropic"));
        assert!(delete_budget(&c, &b.id).await.unwrap());
    }

    #[test]
    fn routing() {
        let providers = vec![
            Provider {
                id: "anthropic".into(),
                default_model: "sonnet".into(),
                factory: true,
                ..Default::default()
            },
            Provider {
                id: "openai".into(),
                default_model: "gpt".into(),
                factory: false,
                ..Default::default()
            },
        ];
        let rules = vec![
            RoutingRule {
                id: "off".into(),
                priority: 1,
                provider: "anthropic".into(),
                model: "never".into(),
                enabled: false,
                ..Default::default()
            },
            RoutingRule {
                id: "blocked".into(),
                priority: 2,
                provider: "openai".into(),
                ..Default::default()
            },
            RoutingRule {
                id: "sec".into(),
                priority: 5,
                match_repo: "acme/*".into(),
                match_label: "security".into(),
                provider: "anthropic".into(),
                model: "opus".into(),
                enabled: true,
                ..Default::default()
            },
        ];
        let mut q = RouteQuery {
            repo: "acme/api".into(),
            labels: vec!["security".into()],
            ..Default::default()
        };
        let r = route(&q, &rules, None, &providers, None).unwrap();
        assert_eq!(
            (r.provider.as_str(), r.model.as_str()),
            ("anthropic", "opus")
        );
        assert_eq!(r.source, "rule:sec");
        q.labels.clear();
        let agent = AgentProfile {
            provider: "anthropic".into(),
            ..Default::default()
        };
        let r = route(&q, &rules, Some(&agent), &providers, None).unwrap();
        assert_eq!((r.model.as_str(), r.source.as_str()), ("sonnet", "agent"));
        let r = route(&q, &rules, None, &providers, Some("mistral/large")).unwrap();
        assert_eq!(
            (r.provider.as_str(), r.source.as_str()),
            ("mistral", "default")
        );
        // A default naming a provider factory work may not use is skipped.
        let r = route(&q, &rules, None, &providers, Some("openai/gpt")).unwrap();
        assert_eq!(r.source, "catalogue");
        assert_eq!(r.provider, "anthropic");
        assert!(route(&q, &[], None, &[], None).is_none());
    }

    #[tokio::test]
    async fn agents_back_up_to_and_install_from_the_library() {
        let (c, _d) = core().await;
        let a = save_agent(
            &c,
            &AgentProfile {
                name: "builder".into(),
                prompt: "build things".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (item, v1) = agent_to_library(&c, &a.id, "first").await.unwrap();
        assert_eq!(v1, 1);
        assert_eq!(
            agent_to_library(&c, &a.id, "same").await.unwrap().1,
            1,
            "unchanged: no version"
        );
        delete_agent(&c, &a.id).await.unwrap();
        let listed = library_agents(&c).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].installed);
        let back = install_agent_from_library(&c, &item, None).await.unwrap();
        assert_eq!(
            (back.name.as_str(), back.prompt.as_str()),
            ("builder", "build things")
        );
        assert!(back.meta_json.contains("installedFrom"));
        let listed = library_agents(&c).await.unwrap();
        assert!(listed[0].installed && listed[0].installed_version == Some(1));
        c.audit_sync().await.unwrap();
        let led = c
            .audit_entries(&kls_store::audit::LedgerQuery::default())
            .await
            .unwrap();
        assert!(led.iter().any(|e| e.msg.contains("library.installed")));
    }

    #[tokio::test]
    async fn workflow_and_node_budgets_apply_in_scope() {
        let (c, _d) = core().await;
        let mk = |scope: &str, subject: &str| Budget {
            name: format!("{scope} cap"),
            applies_to: scope.into(),
            subject: subject.into(),
            period: "month".into(),
            limit_tokens: Some(100),
            action_at_limit: "hard_stop".into(),
            ..Default::default()
        };
        save_budget(&c, &mk("node", "n1")).await.unwrap();
        save_budget(&c, &mk("workflow", "wf1")).await.unwrap();
        assert!(save_budget(&c, &mk("node", "")).await.is_err());
        let spend = Spend {
            provider: "anthropic".into(),
            tokens: 500,
            ..Default::default()
        };
        // Unscoped: neither applies.
        assert!(budget_gate(&c, &spend, "owner").await.unwrap().proceeds());
        let on = |w: &str, n: &str| SpendScope {
            workflow: w.into(),
            node: n.into(),
        };
        assert!(matches!(
            budget_gate_scoped(&c, &spend, &on("", "n1"), "x")
                .await
                .unwrap(),
            BudgetGate::Block(_)
        ));
        assert!(matches!(
            budget_gate_scoped(&c, &spend, &on("wf1", "n2"), "x")
                .await
                .unwrap(),
            BudgetGate::Block(_)
        ));
        assert!(
            budget_gate_scoped(&c, &spend, &on("wf2", "n2"), "x")
                .await
                .unwrap()
                .proceeds()
        );
    }

    #[tokio::test]
    async fn env_secrets_scope_expiry_and_node_grants() {
        let (c, _d) = core().await;
        c.store()
            .set_setting(probe::AUTO_TEST_SETTING, "off")
            .await
            .unwrap();
        assert!(
            put_env_secret(&c, "PATH", "", SecretString::new("abcdefgh123"))
                .await
                .is_err()
        );
        assert!(
            put_env_secret(&c, "GH_MCP_TOKEN", "", SecretString::new("short"))
                .await
                .is_err()
        );
        let e = put_env_secret(&c, "gh_mcp_token", "", SecretString::new("ghp_0123456789"))
            .await
            .unwrap();
        assert_eq!((e.env.as_str(), e.kind.as_str()), ("GH_MCP_TOKEN", "env"));
        let s = set_secret_scope(&c, &e.id, None, &["n1".into()])
            .await
            .unwrap();
        assert_eq!(s.nodes, vec!["n1".to_string()]);
        let env = [ENV_PROVIDER.to_string()];
        assert!(
            grant_for_node(&c, &env, 60_000, "dispatch", Some("n2"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            grant_for_node(&c, &env, 60_000, "dispatch", Some("n1"))
                .await
                .unwrap()
                .is_some()
        );
        let now = kls_store::now_ms();
        let s = set_secret_scope(&c, &e.id, Some(now - 1), &[])
            .await
            .unwrap();
        assert_eq!(lifetime(&s, now), Lifetime::Expired);
        assert_eq!(
            lifetime(
                &SecretMeta {
                    expires_at: Some(now + 86_400_000),
                    ..s.clone()
                },
                now
            ),
            Lifetime::Expiring
        );
        assert!(grant_for(&c, &env, 60_000).await.unwrap().is_none());
        let act = vault_activity(&c, 50).await.unwrap();
        assert!(
            act.iter()
                .any(|a| a.what == "Denied" && a.result.contains("expired"))
        );
        assert!(act.iter().any(|a| a.what == "Access"));
        let code = entries_as_code(&c).await.unwrap();
        assert!(code.contains("GH_MCP_TOKEN") && !code.contains("ghp_0123456789"));
    }

    #[tokio::test]
    async fn models_agents_setting_is_read_by_routing() {
        let (c, _d) = core().await;
        assert_eq!(
            agent_model("builder=openai/gpt-5, qa=anthropic/haiku", "QA"),
            Some(("anthropic".into(), "haiku".into()))
        );
        assert_eq!(agent_model("", "qa"), None);
        c.store()
            .set_setting(settings::MODELS_AGENTS, "builder=mistral/large")
            .await
            .unwrap();
        let r = route_for(
            &c,
            &RouteQuery {
                agent: "builder".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            (r.provider.as_str(), r.source.as_str()),
            ("mistral", "agent_map")
        );
        // Another agent falls through to the catalogue.
        let r = route_for(
            &c,
            &RouteQuery {
                agent: "other".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(r.is_none_or(|r| r.source != "agent_map"));
    }

    #[tokio::test]
    async fn route_for_prefers_keyed_providers() {
        let (c, _d) = core().await;
        put_secret(&c, "openai", "", "", SecretString::new("sk-x"))
            .await
            .unwrap();
        let r = route_for(&c, &RouteQuery::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.provider, "openai");
        set_default_model(&c, settings::MODELS_DEFAULT, "anthropic/opus")
            .await
            .unwrap();
        let r = route_for(&c, &RouteQuery::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.provider.as_str(), r.model.as_str()),
            ("anthropic", "opus")
        );
        let rule = save_rule(
            &c,
            &RoutingRule {
                match_agent: "patcher".into(),
                provider: "mistral".into(),
                enabled: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let q = RouteQuery {
            agent: "patcher".into(),
            ..Default::default()
        };
        assert_eq!(
            route_for(&c, &q).await.unwrap().unwrap().provider,
            "mistral"
        );
        assert!(delete_rule(&c, &rule.id).await.unwrap());
        let mut p = c.store().provider("mistral").await.unwrap().unwrap();
        p.factory = false;
        save_provider(&c, &p).await.unwrap();
        assert!(
            save_provider(
                &c,
                &Provider {
                    id: "BAD".into(),
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn draft_agent_runs_a_model_job() {
        let (c, _d) = core().await;
        put_secret(&c, "anthropic", "", "", SecretString::new(KEY))
            .await
            .unwrap();
        // No gateway yet.
        assert!(draft_agent(&c, "fix vulns", None, 1000).await.is_err());
        let gw = Arc::new(FakeGateway {
            status: "ok".into(),
            reply: json!({"profile": {
                "display_name": "Patch Bot",
                "description": "Patches vulnerable dependencies",
                "prompt": "You patch.",
                "tools": ["Bash", {"name": "Edit"}],
                "avatar_svg": "<svg xmlns='http://www.w3.org/2000/svg'><circle r='4'/></svg>",
            }, "usage": {"input_tokens": 10, "output_tokens": 20}})
            .to_string(),
            ..Default::default()
        });
        c.set_gateway(gw.clone());
        assert!(draft_agent(&c, "  ", None, 1000).await.is_err());
        let d = draft_agent(&c, "fix vulns", Some("anthropic"), 5000)
            .await
            .unwrap();
        assert_eq!(d.name, "patch-bot");
        assert_eq!(d.tools, vec!["Bash", "Edit"]);
        assert_eq!(d.provider, "anthropic");
        assert!(d.avatar_svg.starts_with("<svg"));
        assert_eq!(d.node_id, "n1");
        {
            let jobs = gw.jobs.lock().unwrap();
            let (kind, payload, needs) = &jobs[0];
            assert_eq!(kind, "model");
            let p: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(p["task"], "draft");
            assert_eq!(p["brief"], "fix vulns");
            assert_eq!(needs.provider.as_deref(), Some("anthropic"));
            assert!(!payload.contains(KEY));
        }
        // Routed provider when none is given.
        let d2 = draft_agent(&c, "review", None, 5000).await.unwrap();
        assert_eq!(d2.provider, "anthropic");
        assert_eq!(
            c.store()
                .usage_sum(&UsageFilter::default())
                .await
                .unwrap()
                .0,
            60
        );

        // Everything stays editable: save an edited draft.
        let a = save_agent(
            &c,
            &AgentProfile {
                name: d.name.clone(),
                description: "edited".into(),
                prompt: d.prompt,
                tools: d.tools,
                avatar_svg: d.avatar_svg,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(a.version, 1);
        assert!(
            save_agent(
                &c,
                &AgentProfile {
                    id: String::new(),
                    ..a.clone()
                }
            )
            .await
            .is_err()
        );
        assert!(
            save_agent(
                &c,
                &AgentProfile {
                    avatar_svg: "<svg onload=x()></svg>".into(),
                    ..a.clone()
                }
            )
            .await
            .is_err()
        );
        assert_eq!(save_agent(&c, &a).await.unwrap().version, 2);
        assert!(delete_agent(&c, &a.id).await.unwrap());
    }

    #[tokio::test]
    async fn draft_failure_is_an_error() {
        let (c, _d) = core().await;
        c.set_gateway(Arc::new(FakeGateway {
            status: "refused".into(),
            ..Default::default()
        }));
        let e = draft_agent(&c, "x", Some("anthropic"), 1000)
            .await
            .unwrap_err();
        assert!(format!("{e}").contains("boom"));
    }

    #[tokio::test]
    async fn crews_and_workers() {
        let (c, _d) = core().await;
        let crew = save_crew(
            &c,
            &Crew {
                name: "security".into(),
                members: vec![CrewMember {
                    agent: "patcher".into(),
                    role: "fixer".into(),
                    count: 0,
                    model: String::new(),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(crew.members[0].count, 1);
        assert_eq!(crew.process, "sequential");
        assert!(
            save_crew(
                &c,
                &Crew {
                    id: String::new(),
                    ..crew.clone()
                }
            )
            .await
            .is_err()
        );
        assert!(delete_crew(&c, &crew.id).await.unwrap());

        c.store()
            .insert_node("n1", "box", &Default::default(), "serial", "t")
            .await
            .unwrap();
        c.emit("session.updated", "w1", &json!({"session_id": "w1", "node_id": "n1", "kind": "worker", "agent": "patcher", "state": "live"}))
            .await
            .unwrap();
        c.emit(
            "session.updated",
            "w2",
            &json!({"session_id": "w2", "node_id": "n1", "kind": "worker", "state": "live"}),
        )
        .await
        .unwrap();
        c.emit("session.ended", "w2", &json!({"session_id": "w2"}))
            .await
            .unwrap();
        let w = workers_summary(&c).await.unwrap();
        assert_eq!(w[0].live_workers, 1);
        assert_eq!(w[0].agents, vec![("patcher".to_string(), 1)]);
        assert!(!w[0].online);
    }

    #[tokio::test]
    async fn library_versions_and_files() {
        let (c, d) = core().await;
        let (id, v) = save_library(&c, "", "prompt", "review", "Reviews", "Look closely.", "")
            .await
            .unwrap();
        assert_eq!(v, 1);
        // Unchanged save: no new version.
        assert_eq!(
            save_library(&c, &id, "prompt", "review", "Reviews", "Look closely.", "")
                .await
                .unwrap()
                .1,
            1
        );
        assert_eq!(
            save_library(&c, &id, "prompt", "review", "Reviews", "Look harder.", "")
                .await
                .unwrap()
                .1,
            2
        );
        // Same name, no id: a version of the same item.
        assert_eq!(
            save_library(&c, "", "prompt", "review", "Reviews", "Again.", "")
                .await
                .unwrap(),
            (id.clone(), 3)
        );
        assert!(
            save_library(&c, "", "widget", "x", "", "", "")
                .await
                .is_err()
        );

        let out = d.path().join("export");
        assert_eq!(export_library(&c, &out).await.unwrap(), 1);
        let file = out.join("prompt").join("review.md");
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            parse_library_file(&text),
            (
                "prompt".into(),
                "review".into(),
                "Reviews".into(),
                "Again.".into()
            )
        );

        // Import into a fresh core, plus a bare file whose kind is its folder.
        let (c2, d2) = core().await;
        let skills = d2.path().join("in").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("Triage Bugs.md"), "Sort them.").unwrap();
        std::fs::copy(&file, d2.path().join("in").join("review.md")).unwrap();
        assert_eq!(import_library(&c2, &d2.path().join("in")).await.unwrap(), 2);
        let s = c2
            .store()
            .library_item_by_name("skill", "triage-bugs")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            c2.store()
                .library_latest(&s.id)
                .await
                .unwrap()
                .unwrap()
                .body,
            "Sort them."
        );
        assert!(
            c2.store()
                .library_item_by_name("prompt", "review")
                .await
                .unwrap()
                .is_some()
        );
        assert!(delete_library(&c2, &s.id).await.unwrap());
    }

    #[test]
    fn helpers() {
        assert_eq!(slugify("Patch Bot!"), "patch-bot");
        assert!(valid_name("belai:security"));
        assert!(!valid_name("Bad Name"));
        assert_eq!(sanitize_svg("<script>alert(1)</script>"), "");
        assert_eq!(sanitize_svg("<svg><script/></svg>"), "");
        assert_eq!(sanitize_svg("<svg/>"), "<svg/>");
        assert_eq!(parse_library_file("plain").3, "plain");
    }
}

#[cfg(test)]
mod library_source_tests {
    use super::*;

    #[test]
    fn export_carries_source_keys_and_parse_ignores_them() {
        let item = LibraryItem {
            kind: "skill".into(),
            name: "pdf".into(),
            latest_version: 2,
            ..Default::default()
        };
        let f = library_file_with_source(&item, "body", Some(("https://x/y@abc", "deadbeef")));
        assert!(f.contains("source: https://x/y@abc\nsource_sha256: deadbeef\n"));
        let (kind, name, _, body) = parse_library_file(&f);
        assert_eq!(
            (kind.as_str(), name.as_str(), body.as_str()),
            ("skill", "pdf", "body")
        );
        assert!(!library_file(&item, "b").contains("source:"));
    }
}

#[cfg(test)]
mod library_validation_tests {
    use super::*;

    async fn core() -> (Core, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        (c, dir)
    }

    /// WP0: a hand-saved MCP item is held to belai's validator; the refusal
    /// carries the validator's error and is audited; nothing is stored.
    #[tokio::test]
    async fn an_invalid_mcp_item_is_refused_with_the_validators_error() {
        let (core, _d) = core().await;
        let bad = r#"{"name":"gh","transport":"http","url":"http://example.com/mcp"}"#;
        let e = save_library(&core, "", "mcp", "gh", "", bad, "")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("https"), "{e}");
        assert!(e.downcast_ref::<crate::sessions::Refusal>().is_some());
        assert!(
            core.store()
                .library_item_by_name("mcp", "gh")
                .await
                .unwrap()
                .is_none()
        );
        let ledger = core.audit_entries(&Default::default()).await.unwrap();
        assert!(
            ledger
                .iter()
                .any(|l| l.kind == "library" && l.msg.contains("mcp gh refused")),
            "{ledger:?}"
        );
        // Unknown keys, a wrong name: refused. A good one is stored canonical.
        assert!(
            save_library(&core, "", "mcp", "gh", "", r#"{"name":"gh","bogus":1}"#, "")
                .await
                .is_err()
        );
        let good = r#"{ "name": "gh", "transport": "http", "url": "https://example.com/mcp" }"#;
        assert!(
            save_library(&core, "", "mcp", "other", "", good, "")
                .await
                .is_err()
        );
        let (id, _) = save_library(&core, "", "mcp", "gh", "", good, "")
            .await
            .unwrap();
        let body = core
            .store()
            .library_latest(&id)
            .await
            .unwrap()
            .unwrap()
            .body;
        assert!(body.ends_with('\n') && !body.contains(": "), "{body}");
        // A hook whose JSON does not parse.
        assert!(
            save_library(&core, "", "hook", "guard", "", "one", "")
                .await
                .is_err()
        );
    }

    /// #22: the repo, budget, provider and rewrite kinds are library kinds,
    /// shape-checked against belai's top-level keys.
    #[tokio::test]
    async fn new_item_kinds_are_saved_and_shape_checked() {
        let (core, _d) = core().await;
        for k in ["repo", "budget", "provider", "rewrite"] {
            assert!(LIBRARY_KINDS.contains(&k));
        }
        save_library(
            &core,
            "",
            "repo",
            "web",
            "",
            r#"{"name":"web","url":"https://github.com/o/web"}"#,
            "",
        )
        .await
        .unwrap();
        assert!(
            save_library(
                &core,
                "",
                "repo",
                "web2",
                "",
                r#"{"name":"web2","token":"x"}"#,
                ""
            )
            .await
            .is_err()
        );
        save_library(
            &core,
            "",
            "rewrite",
            "bash_rewrite",
            "",
            r#"{"name":"bash_rewrite","rules":[]}"#,
            "",
        )
        .await
        .unwrap();
        assert!(
            save_library(&core, "", "rewrite", "mine", "", r#"{"name":"mine"}"#, "")
                .await
                .is_err()
        );
        save_library(
            &core,
            "",
            "budget",
            "team",
            "",
            r#"{"name":"team","budgets":[]}"#,
            "",
        )
        .await
        .unwrap();
        save_library(
            &core,
            "",
            "provider",
            "set",
            "",
            r#"{"name":"set","providers":{}}"#,
            "",
        )
        .await
        .unwrap();
    }

    /// An exported item's `source:` rides back in as the version note on a
    /// re-import, never as provenance.
    #[tokio::test]
    async fn reimport_keeps_source_as_a_note() {
        let (core, d) = core().await;
        let item = LibraryItem {
            kind: "prompt".into(),
            name: "pdf".into(),
            latest_version: 1,
            ..Default::default()
        };
        let f = library_file_with_source(&item, "Read the PDF.", Some(("https://x/y@abc", "beef")));
        assert_eq!(
            library_file_source(&f),
            Some(("https://x/y@abc".into(), "beef".into()))
        );
        assert_eq!(library_file_source("plain"), None);
        let p = d.path().join("pdf.md");
        std::fs::write(&p, f).unwrap();
        assert_eq!(import_library(&core, &p).await.unwrap(), 1);
        let it = core
            .store()
            .library_item_by_name("prompt", "pdf")
            .await
            .unwrap()
            .unwrap();
        let v = core.store().library_latest(&it.id).await.unwrap().unwrap();
        assert_eq!(v.note, "imported; source: https://x/y@abc (sha256 beef)");
        assert_eq!(v.body, "Read the PDF.");
        assert!(
            crate::libsearch::origin(&core, "prompt", &it.id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
