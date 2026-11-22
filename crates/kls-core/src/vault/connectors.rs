//! Connector secrets (web `VaultConnectorsTab.vue`, vdb-site
//! docs/secrets-vault.md "Connector secrets"): the credentials Keleustes
//! itself uses to reach a tool (a Slack bot token, a chat or signed webhook
//! address, a Jira token, an AWS or Wiz secret, a registry login).
//!
//! - Filed under `connector-<kind>` (`kls_vault::CONNECTOR_PREFIX`) with
//!   entry kind `connector`. The value is a JSON object of strings, one
//!   member per field of the kind ([`KINDS`]), at most 8 KiB.
//! - **Never granted**: `kls_vault::Vault::grant_scoped` refuses the
//!   provider, so no node, harness or session receives one.
//! - **Opened only by Keleustes** through [`open`], which obeys the kill
//!   switch and records each opening on the ledger, merged to one entry per
//!   secret and surface per hour.
//! - Saved, replaced and revoked with provenance on the ledger (who, from
//!   which screen, fingerprint and version), never a value.

use super::{SecretMeta, SecretString, vault};
use crate::Core;
use anyhow::{Result, anyhow, bail};
use kls_store::audit::{LedgerDraft, LedgerQuery};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The entry kind of a connector secret.
pub const KIND: &str = "connector";
/// Largest connector value.
pub const MAX_BYTES: usize = 8 * 1024;
/// Most live connector secrets.
pub const MAX_LIVE: usize = 100;

/// One connector kind: its id, label, required and optional fields, and
/// which fields hold an https address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectorKind {
    pub id: &'static str,
    pub label: &'static str,
    pub fields: &'static [&'static str],
    pub optional: &'static [&'static str],
}

/// The kinds the web vault holds (docs/secrets-vault.md table).
pub const KINDS: [ConnectorKind; 10] = [
    ConnectorKind {
        id: "slack",
        label: "Slack",
        fields: &["botToken"],
        optional: &[],
    },
    ConnectorKind {
        id: "discord",
        label: "Discord",
        fields: &["url"],
        optional: &[],
    },
    ConnectorKind {
        id: "google_chat",
        label: "Google Chat",
        fields: &["url"],
        optional: &[],
    },
    ConnectorKind {
        id: "teams",
        label: "Microsoft Teams",
        fields: &["url"],
        optional: &[],
    },
    ConnectorKind {
        id: "webhook",
        label: "Signed webhook",
        fields: &["url", "signingSecret"],
        optional: &["previousSecret"],
    },
    ConnectorKind {
        id: "jira",
        label: "Jira",
        fields: &["email", "apiToken"],
        optional: &[],
    },
    ConnectorKind {
        id: "aws_iam_user",
        label: "AWS access key",
        fields: &["secretAccessKey"],
        optional: &[],
    },
    ConnectorKind {
        id: "aws_sso",
        label: "AWS Identity Center",
        fields: &["clientSecret"],
        optional: &[],
    },
    ConnectorKind {
        id: "wiz",
        label: "Wiz",
        fields: &["clientSecret"],
        optional: &[],
    },
    ConnectorKind {
        id: "registry",
        label: "Container registry",
        fields: &["username", "password"],
        optional: &[],
    },
];

/// The kind `id` names.
pub fn kind(id: &str) -> Option<&'static ConnectorKind> {
    KINDS.iter().find(|k| k.id == id)
}

/// The provider slug a kind is filed under.
pub fn provider_of(kind: &str) -> String {
    format!("{}{kind}", kls_vault::CONNECTOR_PREFIX)
}

/// The kind of a connector provider slug.
pub fn kind_of(provider: &str) -> Option<&'static ConnectorKind> {
    provider
        .strip_prefix(kls_vault::CONNECTOR_PREFIX)
        .and_then(kind)
}

/// The slot name a connector is filed under (`env`): its name, upper case,
/// `[A-Z0-9_]`.
pub fn slot(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("C_{s}")
    } else {
        s
    }
}

/// Checks `fields` for `kind` and encodes them as the stored JSON object.
/// Errors name the field, never its value.
pub fn encode(kind_id: &str, fields: &[(String, SecretString)]) -> Result<SecretString, String> {
    let k = kind(kind_id).ok_or_else(|| format!("{kind_id:?} is not a connector kind"))?;
    let mut o = Map::new();
    for (name, v) in fields {
        let v = v.expose().trim();
        if !k.fields.contains(&name.as_str()) && !k.optional.contains(&name.as_str()) {
            return Err(format!("{} has no field {name}", k.label));
        }
        if v.is_empty() {
            continue;
        }
        if v.chars().any(char::is_control) {
            return Err(format!("{name} holds a control character"));
        }
        if name == "url"
            && (!v.starts_with("https://")
                || v.len() > 2048
                || v.chars().any(char::is_whitespace)
                || v["https://".len()..]
                    .split('/')
                    .next()
                    .is_none_or(str::is_empty))
        {
            return Err("url is an https address".into());
        }
        if name == "email" && (!v.contains('@') || v.chars().any(char::is_whitespace)) {
            return Err("email is an address".into());
        }
        o.insert(name.clone(), json!(v));
    }
    if let Some(missing) = k.fields.iter().find(|f| !o.contains_key(**f)) {
        return Err(format!("fill in {missing}"));
    }
    let s = Value::Object(o).to_string();
    if s.len() > MAX_BYTES {
        return Err(format!("a connector secret is at most {MAX_BYTES} bytes"));
    }
    Ok(SecretString::new(s))
}

/// One connector secret as the vault page lists it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connector {
    pub meta: SecretMeta,
    pub kind: String,
    pub kind_label: String,
    /// The connector's name (its slot).
    pub name: String,
    /// Surfaces that opened it (from the ledger), most recent first.
    pub used_by: Vec<String>,
}

async fn record(core: &Core, m: &SecretMeta, action: &str, origin: &str, extra: Value) {
    let ref_key = extra
        .get("refKey")
        .and_then(Value::as_str)
        .map(str::to_string);
    let d = LedgerDraft {
        at: 0,
        kind: "vault".into(),
        actor: if action == "connector_opened" {
            "keleustes".into()
        } else {
            "owner".into()
        },
        actor_kind: if action == "connector_opened" {
            "system".into()
        } else {
            "user".into()
        },
        msg: format!("{} {}", action.replace('_', " "), m.provider),
        subject: json!({ "secretId": m.id, "provider": m.provider, "env": m.env }),
        payload: json!({
            "action": action, "fingerprint": m.fingerprint, "version": m.version,
            "data": extra,
        }),
        origin: origin.into(),
        node_id: String::new(),
        session_id: String::new(),
        ref_key,
    };
    if let Err(e) = core.audit_record(d).await {
        tracing::warn!(secret = %m.id, action, "connector audit: {e:#}");
    }
}

/// Every live connector secret, with what opened each.
pub async fn list(core: &Core) -> Result<Vec<Connector>> {
    let opened = core
        .audit_entries(&LedgerQuery {
            kinds: vec!["vault".into()],
            text: Some("connector opened".into()),
            limit: 1000,
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    for m in core.store().vault_secrets().await? {
        let Some(k) = kind_of(&m.provider) else {
            continue;
        };
        let mut used_by: Vec<String> = Vec::new();
        for e in &opened {
            if e.subject["secretId"] == m.id.as_str()
                && let Some(s) = e.payload["data"]["surface"].as_str()
                && !used_by.iter().any(|u| u == s)
            {
                used_by.push(s.to_string());
            }
        }
        out.push(Connector {
            kind: k.id.into(),
            kind_label: k.label.into(),
            name: m.env.clone(),
            used_by,
            meta: m,
        });
    }
    Ok(out)
}

/// Seals a connector secret (a save of the same kind and name replaces
/// it). `surface` is the screen it was added from (provenance).
pub async fn put(
    core: &Core,
    kind_id: &str,
    name: &str,
    label: &str,
    fields: &[(String, SecretString)],
    surface: &str,
) -> Result<SecretMeta> {
    let value = encode(kind_id, fields).map_err(|e| anyhow!(e))?;
    let env = slot(if name.trim().is_empty() {
        kind_id
    } else {
        name
    });
    if !kls_vault::valid_env(&env) {
        bail!("name the connector with letters and digits");
    }
    let provider = provider_of(kind_id);
    let held = core.store().vault_secrets().await?;
    let live = held
        .iter()
        .filter(|s| kind_of(&s.provider).is_some())
        .count();
    let replaces = held.iter().any(|s| s.provider == provider && s.env == env);
    if !replaces && live >= MAX_LIVE {
        bail!("at most {MAX_LIVE} connector secrets");
    }
    let m = super::put_secret(core, &provider, &env, label, value).await?;
    core.store()
        .vault_set_scope(&m.id, m.expires_at, &[], KIND)
        .await?;
    let m = core.store().vault_secret_meta(&m.id).await?.unwrap_or(m);
    record(
        core,
        &m,
        "connector_saved",
        &format!("vault:{surface}"),
        json!({ "kind": kind_id, "replaced": replaces, "surface": surface }),
    )
    .await;
    Ok(m)
}

/// Replaces a connector's value (a new version; the old one is kept for
/// the rotation grace like any key).
pub async fn replace(
    core: &Core,
    id: &str,
    fields: &[(String, SecretString)],
) -> Result<SecretMeta> {
    let meta = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such connector"))?;
    let k = kind_of(&meta.provider).ok_or_else(|| anyhow!("not a connector secret"))?;
    let value = encode(k.id, fields).map_err(|e| anyhow!(e))?;
    let m = super::lineage::rotate_with_grace(core, id, value, "owner", "").await?;
    record(
        core,
        &m,
        "connector_replaced",
        "vault:connectors",
        json!({ "kind": k.id }),
    )
    .await;
    Ok(m)
}

/// Revokes (deletes) a connector secret; whatever used it stops working.
pub async fn revoke(core: &Core, id: &str) -> Result<bool> {
    let Some(meta) = core.store().vault_secret_meta(id).await? else {
        return Ok(false);
    };
    if kind_of(&meta.provider).is_none() {
        bail!("not a connector secret");
    }
    let gone = super::delete_secret(core, id).await?;
    if gone {
        record(
            core,
            &meta,
            "connector_revoked",
            "vault:connectors",
            json!({}),
        )
        .await;
    }
    Ok(gone)
}

/// Opens a connector secret for Keleustes' own use by `surface`
/// ("notify", "registry", ...) on behalf of `subject`. Refused while the
/// factory is paused or stopped (the refusal is audited by the kill
/// switch). Each opening is on the ledger, one entry per secret and
/// surface per hour. Returns the JSON object of the fields.
pub async fn open(core: &Core, id: &str, surface: &str, subject: &str) -> Result<SecretString> {
    let meta = core
        .store()
        .vault_secret_meta(id)
        .await?
        .ok_or_else(|| anyhow!("no such connector"))?;
    if kind_of(&meta.provider).is_none() {
        bail!("not a connector secret");
    }
    crate::control::check(core, "connector", id, &format!("{surface}:{subject}")).await?;
    if meta.expires_at.is_some_and(|e| e <= kls_store::now_ms()) {
        bail!("that connector secret expired");
    }
    let v = vault(core)
        .await?
        .get(id)
        .await?
        .ok_or_else(|| anyhow!("no such connector"))?;
    let hour = kls_store::now_ms() / 3_600_000;
    record(
        core,
        &meta,
        "connector_opened",
        &format!("vault:{surface}"),
        json!({ "surface": surface, "subject": subject,
                "refKey": format!("connector-open:{id}:{surface}:{hour}") }),
    )
    .await;
    Ok(v)
}

/// One field of an opened connector value.
pub fn field(value: &SecretString, name: &str) -> Option<SecretString> {
    serde_json::from_str::<Value>(value.expose())
        .ok()?
        .get(name)?
        .as_str()
        .map(SecretString::new)
}
