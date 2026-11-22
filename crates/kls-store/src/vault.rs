//! Store: M5 vault and workforce: vault secrets, budgets, providers, routing,
//! usage, agents, crews, library. Migration 0005.
//!
//! The store never sees a plaintext secret: `kls-vault` seals values before
//! they arrive here and opens them after they leave.

use super::*;

// ---- types -----------------------------------------------------------------

/// A sealed secret row. `nonce` and `ciphertext` are opaque to the store.
#[derive(Clone, PartialEq)]
pub struct SealedSecret {
    pub id: String,
    pub provider: String,
    pub env: String,
    pub label: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

impl std::fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedSecret")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("env", &self.env)
            .field("label", &self.label)
            .field(
                "ciphertext",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .finish_non_exhaustive()
    }
}

/// A secret's metadata: everything but the value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub id: String,
    pub provider: String,
    pub env: String,
    pub label: String,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub last_used_at: Option<i64>,
    /// Last key test: "" (untested), "testing", "accepted", "refused",
    /// "unreachable" or "skipped". An indicator only.
    #[serde(default)]
    pub test_state: String,
    /// HTTP status of the last test, when one came back.
    #[serde(default)]
    pub test_status: Option<i64>,
    #[serde(default)]
    pub test_ms: Option<i64>,
    /// Why a test was refused or unreachable (never holds the key).
    #[serde(default)]
    pub test_detail: String,
    #[serde(default)]
    pub tested_at: Option<i64>,
    /// First 16 hex chars of sha256(value), kept to check integrity.
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub verified_at: Option<i64>,
    /// Set when the last integrity check failed.
    #[serde(default)]
    pub verify_error: String,
    /// Past this a grant refuses the key (the value is kept). `None`: never.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// Node ids that may receive it; empty: every node.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// "provider" (a model key) or "env" (a generic variable).
    #[serde(default)]
    pub kind: String,
    /// The live version (1 at save, +1 per rotation).
    #[serde(default)]
    pub version: i64,
    /// Auto-rotate after this many days (0: never).
    #[serde(default)]
    pub rotate_every_days: i64,
    /// Model ids the last accepted test listed.
    #[serde(default)]
    pub models: Vec<String>,
    /// The provider's own handle of the live key (never the key).
    #[serde(default)]
    pub remote_id: String,
}

/// One value a key has held (never the value itself). `sealed` is true while
/// the previous value is still kept, sealed, inside its rotation grace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecretVersion {
    pub secret_id: String,
    pub version: i64,
    pub fingerprint: String,
    pub created_at: i64,
    pub retired_at: Option<i64>,
    pub grace_until: Option<i64>,
    pub sealed: bool,
    pub remote_id: String,
    pub origin: String,
}

/// A retired value still sealed inside its grace (for a rollback).
#[derive(Clone, PartialEq)]
pub struct SealedVersion {
    pub version: SecretVersion,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for SealedVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedVersion")
            .field("version", &self.version)
            .field(
                "ciphertext",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .finish_non_exhaustive()
    }
}

/// A key test result to record on a secret.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecretTest {
    pub state: String,
    pub status: Option<i64>,
    pub ms: Option<i64>,
    pub detail: String,
    pub at: i64,
}

/// One grant handed to a node (audit row; values are never stored).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GrantRecord {
    pub id: String,
    pub providers: Vec<String>,
    pub secret_ids: Vec<String>,
    pub missing: Vec<String>,
    pub purpose: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    /// Slug, e.g. "anthropic".
    pub id: String,
    pub name: String,
    /// Environment variable the provider's key goes in.
    pub env: String,
    pub default_model: String,
    pub fast_model: String,
    pub models: Vec<String>,
    /// Factory work may use this provider.
    pub factory: bool,
    /// Free endpoint that checks a key (a models list); "" = not testable.
    #[serde(default)]
    pub probe_url: String,
    /// How the key is sent to `probe_url`: "bearer", "x-api-key" or
    /// "x-goog-api-key".
    #[serde(default)]
    pub probe_auth: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub id: String,
    pub name: String,
    /// "all", "provider", "model", "agent", "repo".
    pub applies_to: String,
    /// What `applies_to` names ("" for all).
    pub subject: String,
    /// "all", a provider id or a comma list.
    pub provider: String,
    /// "day", "week", "month", "total".
    pub period: String,
    pub limit_tokens: Option<i64>,
    pub limit_usd: Option<f64>,
    /// Percentages that raise an alert.
    pub alert_at: Vec<i64>,
    /// "hard_stop", "pause_run", "escalate_human", "downgrade_to_fast", "alert_only".
    pub action_at_limit: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: String,
    pub priority: i64,
    pub match_repo: String,
    pub match_agent: String,
    pub match_label: String,
    pub match_kind: String,
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    pub id: String,
    pub at: i64,
    /// "session" or "job".
    pub source: String,
    pub session_id: String,
    pub node_id: String,
    pub provider: String,
    pub model: String,
    pub agent: String,
    pub repo: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

/// Which usage rows a sum covers; empty fields match everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageFilter {
    pub since: i64,
    pub providers: Vec<String>,
    pub model: String,
    pub agent: String,
    pub repo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotal {
    pub key: String,
    pub tokens: i64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub tools: Vec<String>,
    /// SVG text.
    pub avatar_svg: String,
    /// Routing extras (claims, labels, on_success...) as JSON text.
    pub meta_json: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewMember {
    pub agent: String,
    pub role: String,
    #[serde(default = "one")]
    pub count: i64,
    /// `provider/model` this member runs on (empty: the step's `memberModels`,
    /// else Settings `models.agents`, else the profile). Stored in
    /// `crews.members_json`, so no column is needed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

fn one() -> i64 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Crew {
    pub id: String,
    pub name: String,
    pub description: String,
    /// "sequential", "parallel", "hierarchical".
    pub process: String,
    pub members: Vec<CrewMember>,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    /// "prompt", "skill", "command", "process".
    pub kind: String,
    pub name: String,
    pub description: String,
    pub latest_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryVersion {
    pub item_id: String,
    pub version: i64,
    pub body: String,
    pub note: String,
    pub created_at: i64,
}

// ---- row mapping -------------------------------------------------------------

fn json_vec<T: serde::de::DeserializeOwned>(s: &str) -> Vec<T> {
    serde_json::from_str(s).unwrap_or_default()
}

const SECRET_META_COLS: &str = "id, provider, env, label, created_at, rotated_at, last_used_at, \
     test_state, test_status, test_ms, test_detail, tested_at, fingerprint, verified_at, verify_error, \
     expires_at, nodes_json, kind, version, rotate_every_days, models_json, remote_id";

const VERSION_COLS: &str = "secret_id, version, fingerprint, created_at, retired_at, grace_until, \
     remote_id, origin, nonce, ciphertext";

fn secret_version(r: &AnyRow) -> Result<SecretVersion> {
    let ct: Option<Vec<u8>> = r.try_get("ciphertext")?;
    Ok(SecretVersion {
        secret_id: r.try_get("secret_id")?,
        version: r.try_get("version")?,
        fingerprint: r.try_get("fingerprint")?,
        created_at: r.try_get("created_at")?,
        retired_at: r.try_get("retired_at")?,
        grace_until: r.try_get("grace_until")?,
        sealed: ct.is_some_and(|c| !c.is_empty()),
        remote_id: r.try_get("remote_id")?,
        origin: r.try_get("origin")?,
    })
}

fn secret_meta(r: &AnyRow) -> Result<SecretMeta> {
    Ok(SecretMeta {
        id: r.try_get("id")?,
        provider: r.try_get("provider")?,
        env: r.try_get("env")?,
        label: r.try_get("label")?,
        created_at: r.try_get("created_at")?,
        rotated_at: r.try_get("rotated_at")?,
        last_used_at: r.try_get("last_used_at")?,
        test_state: r.try_get("test_state")?,
        test_status: r.try_get("test_status")?,
        test_ms: r.try_get("test_ms")?,
        test_detail: r.try_get("test_detail")?,
        tested_at: r.try_get("tested_at")?,
        fingerprint: r.try_get("fingerprint")?,
        verified_at: r.try_get("verified_at")?,
        verify_error: r.try_get("verify_error")?,
        expires_at: r.try_get("expires_at")?,
        nodes: json_vec(&r.try_get::<String, _>("nodes_json")?),
        kind: r.try_get("kind")?,
        version: r.try_get("version")?,
        rotate_every_days: r.try_get("rotate_every_days")?,
        models: json_vec(&r.try_get::<String, _>("models_json")?),
        remote_id: r.try_get("remote_id")?,
    })
}

fn sealed(r: &AnyRow) -> Result<SealedSecret> {
    Ok(SealedSecret {
        id: r.try_get("id")?,
        provider: r.try_get("provider")?,
        env: r.try_get("env")?,
        label: r.try_get("label")?,
        nonce: r.try_get("nonce")?,
        ciphertext: r.try_get("ciphertext")?,
        created_at: r.try_get("created_at")?,
        rotated_at: r.try_get("rotated_at")?,
        last_used_at: r.try_get("last_used_at")?,
    })
}

const GRANT_COLS: &str = "id, providers_json, secret_ids_json, missing_json, purpose, created_at, \
     expires_at, revoked_at";

fn grant(r: &AnyRow) -> Result<GrantRecord> {
    Ok(GrantRecord {
        id: r.try_get("id")?,
        providers: json_vec(&r.try_get::<String, _>("providers_json")?),
        secret_ids: json_vec(&r.try_get::<String, _>("secret_ids_json")?),
        missing: json_vec(&r.try_get::<String, _>("missing_json")?),
        purpose: r.try_get("purpose")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

const PROVIDER_COLS: &str = "id, name, env, default_model, fast_model, models_json, factory, \
     probe_url, probe_auth, created_at, updated_at";

fn provider(r: &AnyRow) -> Result<Provider> {
    Ok(Provider {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        env: r.try_get("env")?,
        default_model: r.try_get("default_model")?,
        fast_model: r.try_get("fast_model")?,
        models: json_vec(&r.try_get::<String, _>("models_json")?),
        factory: r.try_get::<i64, _>("factory")? != 0,
        probe_url: r.try_get("probe_url")?,
        probe_auth: r.try_get("probe_auth")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const BUDGET_COLS: &str = "id, name, applies_to, subject, provider, period, limit_tokens, limit_usd, \
     alert_at_json, action_at_limit, revision, created_at, updated_at";

fn budget(r: &AnyRow) -> Result<Budget> {
    Ok(Budget {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        applies_to: r.try_get("applies_to")?,
        subject: r.try_get("subject")?,
        provider: r.try_get("provider")?,
        period: r.try_get("period")?,
        limit_tokens: r.try_get("limit_tokens")?,
        limit_usd: r.try_get("limit_usd")?,
        alert_at: json_vec(&r.try_get::<String, _>("alert_at_json")?),
        action_at_limit: r.try_get("action_at_limit")?,
        revision: r.try_get("revision")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const RULE_COLS: &str = "id, priority, match_repo, match_agent, match_label, match_kind, provider, \
     model, enabled, created_at, updated_at";

fn rule(r: &AnyRow) -> Result<RoutingRule> {
    Ok(RoutingRule {
        id: r.try_get("id")?,
        priority: r.try_get("priority")?,
        match_repo: r.try_get("match_repo")?,
        match_agent: r.try_get("match_agent")?,
        match_label: r.try_get("match_label")?,
        match_kind: r.try_get("match_kind")?,
        provider: r.try_get("provider")?,
        model: r.try_get("model")?,
        enabled: r.try_get::<i64, _>("enabled")? != 0,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const USAGE_COLS: &str = "id, at, source, session_id, node_id, provider, model, agent, repo, \
     input_tokens, output_tokens, cost_usd";

fn usage(r: &AnyRow) -> Result<UsageRow> {
    Ok(UsageRow {
        id: r.try_get("id")?,
        at: r.try_get("at")?,
        source: r.try_get("source")?,
        session_id: r.try_get("session_id")?,
        node_id: r.try_get("node_id")?,
        provider: r.try_get("provider")?,
        model: r.try_get("model")?,
        agent: r.try_get("agent")?,
        repo: r.try_get("repo")?,
        input_tokens: r.try_get("input_tokens")?,
        output_tokens: r.try_get("output_tokens")?,
        cost_usd: r.try_get("cost_usd")?,
    })
}

const AGENT_COLS: &str = "id, name, display_name, description, prompt, provider, model, tools_json, \
     avatar_svg, meta_json, version, created_at, updated_at";

fn agent(r: &AnyRow) -> Result<AgentProfile> {
    Ok(AgentProfile {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        display_name: r.try_get("display_name")?,
        description: r.try_get("description")?,
        prompt: r.try_get("prompt")?,
        provider: r.try_get("provider")?,
        model: r.try_get("model")?,
        tools: json_vec(&r.try_get::<String, _>("tools_json")?),
        avatar_svg: r.try_get("avatar_svg")?,
        meta_json: r.try_get("meta_json")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const CREW_COLS: &str =
    "id, name, description, process, members_json, version, created_at, updated_at";

fn crew(r: &AnyRow) -> Result<Crew> {
    Ok(Crew {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        process: r.try_get("process")?,
        members: json_vec(&r.try_get::<String, _>("members_json")?),
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const ITEM_COLS: &str = "id, kind, name, description, latest_version, created_at, updated_at";

fn item(r: &AnyRow) -> Result<LibraryItem> {
    Ok(LibraryItem {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        latest_version: r.try_get("latest_version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn version(r: &AnyRow) -> Result<LibraryVersion> {
    Ok(LibraryVersion {
        item_id: r.try_get("item_id")?,
        version: r.try_get("version")?,
        body: r.try_get("body")?,
        note: r.try_get("note")?,
        created_at: r.try_get("created_at")?,
    })
}

impl Store {
    // ---- vault secrets -----------------------------------------------------

    /// Inserts a sealed secret, or replaces the value of the one with the same
    /// provider and env (a rotation, which keeps its id). Returns the id kept.
    pub async fn vault_put_sealed(&self, s: &SealedSecret) -> Result<String> {
        let now = now_ms();
        let row = sqlx::query(self.sql(
            "INSERT INTO vault_secrets (id, provider, env, label, nonce, ciphertext, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (provider, env) DO UPDATE SET label = excluded.label, nonce = excluded.nonce, \
             ciphertext = excluded.ciphertext, rotated_at = ? RETURNING id",
        ))
        .bind(&s.id)
        .bind(&s.provider)
        .bind(&s.env)
        .bind(&s.label)
        .bind(s.nonce.clone())
        .bind(s.ciphertext.clone())
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    /// Replaces the sealed value of an existing secret.
    pub async fn vault_reseal(&self, id: &str, nonce: &[u8], ciphertext: &[u8]) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE vault_secrets SET nonce = ?, ciphertext = ?, rotated_at = ? WHERE id = ?",
        ))
        .bind(nonce.to_vec())
        .bind(ciphertext.to_vec())
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn vault_set_label(&self, id: &str, label: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("UPDATE vault_secrets SET label = ? WHERE id = ?"))
            .bind(label)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn vault_sealed(&self, id: &str) -> Result<Option<SealedSecret>> {
        let row = sqlx::query(self.sql(
            "SELECT id, provider, env, label, nonce, ciphertext, created_at, rotated_at, last_used_at \
             FROM vault_secrets WHERE id = ?",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(sealed).transpose()
    }

    /// Sealed secrets for `provider`, oldest first.
    pub async fn vault_sealed_for(&self, provider: &str) -> Result<Vec<SealedSecret>> {
        let rows = sqlx::query(self.sql(
            "SELECT id, provider, env, label, nonce, ciphertext, created_at, rotated_at, last_used_at \
             FROM vault_secrets WHERE provider = ? ORDER BY created_at, id",
        ))
        .bind(provider)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sealed).collect()
    }

    pub async fn vault_secrets(&self) -> Result<Vec<SecretMeta>> {
        let q = format!("SELECT {SECRET_META_COLS} FROM vault_secrets ORDER BY provider, env");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(secret_meta).collect()
    }

    pub async fn vault_secret_meta(&self, id: &str) -> Result<Option<SecretMeta>> {
        let q = format!("SELECT {SECRET_META_COLS} FROM vault_secrets WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(secret_meta).transpose()
    }

    pub async fn vault_delete_secret(&self, id: &str) -> Result<bool> {
        sqlx::query(self.sql("DELETE FROM vault_secret_versions WHERE secret_id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        let res = sqlx::query(self.sql("DELETE FROM vault_secrets WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Records a key test on secret `id` (state "testing" marks one under way).
    pub async fn vault_set_test(&self, id: &str, t: &SecretTest) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE vault_secrets SET test_state = ?, test_status = ?, test_ms = ?, test_detail = ?, \
             tested_at = ? WHERE id = ?",
        ))
        .bind(&t.state)
        .bind(t.status)
        .bind(t.ms)
        .bind(&t.detail)
        .bind(t.at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Stores the fingerprint of the value just sealed and clears the last
    /// test (the value changed).
    pub async fn vault_set_fingerprint(&self, id: &str, fingerprint: &str) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "UPDATE vault_secrets SET fingerprint = ?, verified_at = ?, verify_error = '', \
             test_state = '', test_status = NULL, test_ms = NULL, test_detail = '', tested_at = NULL \
             WHERE id = ?",
        ))
        .bind(fingerprint)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Sets where and until when a key may be granted, and its kind.
    pub async fn vault_set_scope(
        &self,
        id: &str,
        expires_at: Option<i64>,
        nodes: &[String],
        kind: &str,
    ) -> Result<bool> {
        let res =
            sqlx::query(self.sql(
                "UPDATE vault_secrets SET expires_at = ?, nodes_json = ?, kind = ? WHERE id = ?",
            ))
            .bind(expires_at)
            .bind(serde_json::to_string(nodes)?)
            .bind(if kind.is_empty() { "provider" } else { kind })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Records an integrity check: `error` is "" when it passed.
    pub async fn vault_set_verified(&self, id: &str, at: i64, error: &str) -> Result<bool> {
        let res = sqlx::query(
            self.sql("UPDATE vault_secrets SET verified_at = ?, verify_error = ? WHERE id = ?"),
        )
        .bind(at)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn vault_touch_secrets(&self, ids: &[String]) -> Result<()> {
        let now = now_ms();
        for id in ids {
            sqlx::query(self.sql("UPDATE vault_secrets SET last_used_at = ? WHERE id = ?"))
                .bind(now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    // ---- versions, models, rotation ----------------------------------------

    /// Records one version of a key. `sealed` keeps a retired value (sealed
    /// by `kls-vault`, opaque here) until its grace ends.
    pub async fn vault_put_version(
        &self,
        v: &SecretVersion,
        sealed: Option<(&[u8], &[u8])>,
    ) -> Result<()> {
        let (n, ct) = match sealed {
            Some((n, c)) => (Some(n.to_vec()), Some(c.to_vec())),
            None => (None, None),
        };
        sqlx::query(self.sql(
            "INSERT INTO vault_secret_versions (secret_id, version, fingerprint, created_at, retired_at, \
             grace_until, nonce, ciphertext, remote_id, origin) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (secret_id, version) DO UPDATE SET fingerprint = excluded.fingerprint, \
             retired_at = excluded.retired_at, grace_until = excluded.grace_until, nonce = excluded.nonce, \
             ciphertext = excluded.ciphertext, remote_id = excluded.remote_id, origin = excluded.origin",
        ))
        .bind(&v.secret_id)
        .bind(v.version)
        .bind(&v.fingerprint)
        .bind(v.created_at)
        .bind(v.retired_at)
        .bind(v.grace_until)
        .bind(n)
        .bind(ct)
        .bind(&v.remote_id)
        .bind(&v.origin)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A key's versions, newest first.
    pub async fn vault_versions(&self, id: &str) -> Result<Vec<SecretVersion>> {
        let q = format!(
            "SELECT {VERSION_COLS} FROM vault_secret_versions WHERE secret_id = ? ORDER BY version DESC"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(secret_version).collect()
    }

    /// A retired version still sealed inside its grace.
    pub async fn vault_sealed_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<SealedVersion>> {
        let q = format!(
            "SELECT {VERSION_COLS} FROM vault_secret_versions WHERE secret_id = ? AND version = ? \
             AND ciphertext IS NOT NULL"
        );
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .bind(version)
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else { return Ok(None) };
        Ok(Some(SealedVersion {
            version: secret_version(&r)?,
            nonce: r
                .try_get::<Option<Vec<u8>>, _>("nonce")?
                .unwrap_or_default(),
            ciphertext: r
                .try_get::<Option<Vec<u8>>, _>("ciphertext")?
                .unwrap_or_default(),
        }))
    }

    /// Shreds retired values whose grace ended by `now` (the lineage row and
    /// fingerprint stay). Returns the versions shredded.
    pub async fn vault_shred_versions(&self, now: i64) -> Result<Vec<SecretVersion>> {
        let q = format!(
            "SELECT {VERSION_COLS} FROM vault_secret_versions WHERE ciphertext IS NOT NULL \
             AND grace_until IS NOT NULL AND grace_until <= ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        let due: Vec<SecretVersion> = rows.iter().map(secret_version).collect::<Result<_>>()?;
        for v in &due {
            sqlx::query(self.sql(
                "UPDATE vault_secret_versions SET nonce = NULL, ciphertext = NULL \
                 WHERE secret_id = ? AND version = ?",
            ))
            .bind(&v.secret_id)
            .bind(v.version)
            .execute(&self.pool)
            .await?;
        }
        Ok(due)
    }

    /// Sets the live version number (and the provider handle) of a key.
    pub async fn vault_set_version(&self, id: &str, version: i64, remote_id: &str) -> Result<bool> {
        let res = sqlx::query(
            self.sql("UPDATE vault_secrets SET version = ?, remote_id = ? WHERE id = ?"),
        )
        .bind(version)
        .bind(remote_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Model ids a key test listed.
    pub async fn vault_set_models(&self, id: &str, models: &[String]) -> Result<bool> {
        let res = sqlx::query(self.sql("UPDATE vault_secrets SET models_json = ? WHERE id = ?"))
            .bind(serde_json::to_string(models)?)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Auto-rotation period in days (0: never).
    pub async fn vault_set_rotation(&self, id: &str, days: i64) -> Result<bool> {
        let res =
            sqlx::query(self.sql("UPDATE vault_secrets SET rotate_every_days = ? WHERE id = ?"))
                .bind(days.max(0))
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- grants ------------------------------------------------------------

    pub async fn vault_insert_grant(&self, g: &GrantRecord) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO vault_grants (id, providers_json, secret_ids_json, missing_json, purpose, \
             created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind(&g.id)
        .bind(serde_json::to_string(&g.providers)?)
        .bind(serde_json::to_string(&g.secret_ids)?)
        .bind(serde_json::to_string(&g.missing)?)
        .bind(&g.purpose)
        .bind(g.created_at)
        .bind(g.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn vault_revoke_grant(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(
            self.sql("UPDATE vault_grants SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL"),
        )
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn vault_grant(&self, id: &str) -> Result<Option<GrantRecord>> {
        let q = format!("SELECT {GRANT_COLS} FROM vault_grants WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(grant).transpose()
    }

    /// Newest first.
    pub async fn vault_grants(&self, limit: i64) -> Result<Vec<GrantRecord>> {
        let q = format!(
            "SELECT {GRANT_COLS} FROM vault_grants ORDER BY created_at DESC, id DESC LIMIT ?"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(grant).collect()
    }

    // ---- providers ---------------------------------------------------------

    pub async fn providers(&self) -> Result<Vec<Provider>> {
        let q = format!("SELECT {PROVIDER_COLS} FROM vault_providers ORDER BY name");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(provider).collect()
    }

    pub async fn provider(&self, id: &str) -> Result<Option<Provider>> {
        let q = format!("SELECT {PROVIDER_COLS} FROM vault_providers WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(provider).transpose()
    }

    pub async fn upsert_provider(&self, p: &Provider) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO vault_providers (id, name, env, default_model, fast_model, models_json, factory, \
             probe_url, probe_auth, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, env = excluded.env, \
             default_model = excluded.default_model, fast_model = excluded.fast_model, \
             models_json = excluded.models_json, factory = excluded.factory, \
             probe_url = excluded.probe_url, probe_auth = excluded.probe_auth, \
             updated_at = excluded.updated_at",
        ))
        .bind(&p.id)
        .bind(&p.name)
        .bind(&p.env)
        .bind(&p.default_model)
        .bind(&p.fast_model)
        .bind(serde_json::to_string(&p.models)?)
        .bind(p.factory as i64)
        .bind(&p.probe_url)
        .bind(if p.probe_auth.is_empty() {
            "bearer"
        } else {
            p.probe_auth.as_str()
        })
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_provider(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM vault_providers WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- budget alerts -----------------------------------------------------

    /// Records that `budget_id` crossed `pct` in the period starting at
    /// `period_start`. False when it was already recorded (alert once).
    pub async fn budget_alert_once(
        &self,
        budget_id: &str,
        period_start: i64,
        pct: i64,
    ) -> Result<bool> {
        let res = sqlx::query(self.sql(
            "INSERT INTO budget_alerts (budget_id, period_start, pct, at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (budget_id, period_start, pct) DO NOTHING",
        ))
        .bind(budget_id)
        .bind(period_start)
        .bind(pct)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- kill switch mode --------------------------------------------------

    /// The kill switch mode: "" (running), "pause" or "stop".
    pub async fn factory_mode(&self) -> Result<String> {
        let r = sqlx::query(self.sql("SELECT stopped, mode FROM factory_control WHERE id = 1"))
            .fetch_optional(&self.pool)
            .await?;
        Ok(match r {
            Some(r) if r.try_get::<i64, _>("stopped")? != 0 => {
                let m: String = r.try_get("mode")?;
                if m.is_empty() { "stop".into() } else { m }
            }
            _ => String::new(),
        })
    }

    /// Sets the kill switch mode ("" when it lifts).
    pub async fn set_factory_mode(&self, mode: &str) -> Result<()> {
        sqlx::query(self.sql("UPDATE factory_control SET mode = ? WHERE id = 1"))
            .bind(mode)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- budgets -----------------------------------------------------------

    pub async fn budgets(&self) -> Result<Vec<Budget>> {
        let q = format!("SELECT {BUDGET_COLS} FROM budgets ORDER BY name, id");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(budget).collect()
    }

    pub async fn budget(&self, id: &str) -> Result<Option<Budget>> {
        let q = format!("SELECT {BUDGET_COLS} FROM budgets WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(budget).transpose()
    }

    /// Inserts `b` (revision 1) or replaces it when `b.revision` matches the
    /// stored one. Returns the new revision, or `None` on a stale revision.
    pub async fn save_budget(&self, b: &Budget) -> Result<Option<i64>> {
        let now = now_ms();
        let alert = serde_json::to_string(&b.alert_at)?;
        if self.budget(&b.id).await?.is_none() {
            sqlx::query(self.sql(
                "INSERT INTO budgets (id, name, applies_to, subject, provider, period, limit_tokens, \
                 limit_usd, alert_at_json, action_at_limit, revision, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
            ))
            .bind(&b.id)
            .bind(&b.name)
            .bind(&b.applies_to)
            .bind(&b.subject)
            .bind(&b.provider)
            .bind(&b.period)
            .bind(b.limit_tokens)
            .bind(b.limit_usd)
            .bind(alert)
            .bind(&b.action_at_limit)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
            return Ok(Some(1));
        }
        let res = sqlx::query(self.sql(
            "UPDATE budgets SET name = ?, applies_to = ?, subject = ?, provider = ?, period = ?, \
             limit_tokens = ?, limit_usd = ?, alert_at_json = ?, action_at_limit = ?, \
             revision = revision + 1, updated_at = ? WHERE id = ? AND revision = ?",
        ))
        .bind(&b.name)
        .bind(&b.applies_to)
        .bind(&b.subject)
        .bind(&b.provider)
        .bind(&b.period)
        .bind(b.limit_tokens)
        .bind(b.limit_usd)
        .bind(alert)
        .bind(&b.action_at_limit)
        .bind(now)
        .bind(&b.id)
        .bind(b.revision)
        .execute(&self.pool)
        .await?;
        Ok((res.rows_affected() == 1).then_some(b.revision + 1))
    }

    pub async fn delete_budget(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM budgets WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- routing rules -----------------------------------------------------

    /// By priority (lowest first).
    pub async fn routing_rules(&self) -> Result<Vec<RoutingRule>> {
        let q = format!("SELECT {RULE_COLS} FROM routing_rules ORDER BY priority, created_at, id");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(rule).collect()
    }

    pub async fn upsert_routing_rule(&self, r: &RoutingRule) -> Result<()> {
        let now = now_ms();
        sqlx::query(self.sql(
            "INSERT INTO routing_rules (id, priority, match_repo, match_agent, match_label, match_kind, \
             provider, model, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET priority = excluded.priority, match_repo = excluded.match_repo, \
             match_agent = excluded.match_agent, match_label = excluded.match_label, \
             match_kind = excluded.match_kind, provider = excluded.provider, model = excluded.model, \
             enabled = excluded.enabled, updated_at = excluded.updated_at",
        ))
        .bind(&r.id)
        .bind(r.priority)
        .bind(&r.match_repo)
        .bind(&r.match_agent)
        .bind(&r.match_label)
        .bind(&r.match_kind)
        .bind(&r.provider)
        .bind(&r.model)
        .bind(r.enabled as i64)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_routing_rule(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM routing_rules WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- usage -------------------------------------------------------------

    /// Inserts or replaces a usage row by id (session rows carry cumulative
    /// totals, so a later upsert replaces the earlier one).
    pub async fn upsert_usage(&self, u: &UsageRow) -> Result<()> {
        sqlx::query(self.sql(
            "INSERT INTO usage_rows (id, at, source, session_id, node_id, provider, model, agent, repo, \
             input_tokens, output_tokens, cost_usd) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET at = excluded.at, node_id = excluded.node_id, \
             provider = excluded.provider, model = excluded.model, agent = excluded.agent, \
             repo = excluded.repo, input_tokens = excluded.input_tokens, \
             output_tokens = excluded.output_tokens, cost_usd = excluded.cost_usd",
        ))
        .bind(&u.id)
        .bind(u.at)
        .bind(&u.source)
        .bind(&u.session_id)
        .bind(&u.node_id)
        .bind(&u.provider)
        .bind(&u.model)
        .bind(&u.agent)
        .bind(&u.repo)
        .bind(u.input_tokens)
        .bind(u.output_tokens)
        .bind(u.cost_usd)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first.
    pub async fn usage_rows(&self, since: i64, limit: i64) -> Result<Vec<UsageRow>> {
        let q =
            format!("SELECT {USAGE_COLS} FROM usage_rows WHERE at >= ? ORDER BY at DESC LIMIT ?");
        let rows = sqlx::query(self.sql(&q))
            .bind(since)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(usage).collect()
    }

    /// Tokens (input + output) and cost of the rows `f` covers.
    pub async fn usage_sum(&self, f: &UsageFilter) -> Result<(i64, f64)> {
        // Filters are applied here rather than in SQL so the statement stays
        // constant; usage tables are small (one row per session or job).
        let rows = self.usage_rows(f.since, i64::MAX).await?;
        let mut tokens = 0i64;
        let mut usd = 0f64;
        for r in rows.iter().filter(|r| {
            (f.providers.is_empty() || f.providers.iter().any(|p| p == &r.provider))
                && (f.model.is_empty()
                    || f.model == r.model
                    || f.model == format!("{}/{}", r.provider, r.model))
                && (f.agent.is_empty() || f.agent == r.agent)
                && (f.repo.is_empty() || f.repo == r.repo)
        }) {
            tokens += r.input_tokens + r.output_tokens;
            usd += r.cost_usd;
        }
        Ok((tokens, usd))
    }

    /// Totals grouped by `by`: "provider", "model", "agent", "repo", "node".
    pub async fn usage_by(&self, since: i64, by: &str) -> Result<Vec<UsageTotal>> {
        let rows = self.usage_rows(since, i64::MAX).await?;
        let mut out: Vec<UsageTotal> = Vec::new();
        for r in rows {
            let key = match by {
                "provider" => r.provider.clone(),
                "model" if r.provider.is_empty() => r.model.clone(),
                "model" => format!("{}/{}", r.provider, r.model),
                "agent" => r.agent.clone(),
                "repo" => r.repo.clone(),
                "node" => r.node_id.clone(),
                _ => bail!("unknown usage grouping {by}"),
            };
            let t = r.input_tokens + r.output_tokens;
            match out.iter_mut().find(|u| u.key == key) {
                Some(u) => {
                    u.tokens += t;
                    u.cost_usd += r.cost_usd;
                }
                None => out.push(UsageTotal {
                    key,
                    tokens: t,
                    cost_usd: r.cost_usd,
                }),
            }
        }
        out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.key.cmp(&b.key)));
        Ok(out)
    }

    // ---- agent profiles ----------------------------------------------------

    pub async fn agent_profiles(&self) -> Result<Vec<AgentProfile>> {
        let q = format!("SELECT {AGENT_COLS} FROM agent_profiles ORDER BY name");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(agent).collect()
    }

    pub async fn agent_profile(&self, id: &str) -> Result<Option<AgentProfile>> {
        let q = format!("SELECT {AGENT_COLS} FROM agent_profiles WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(agent).transpose()
    }

    pub async fn agent_profile_by_name(&self, name: &str) -> Result<Option<AgentProfile>> {
        let q = format!("SELECT {AGENT_COLS} FROM agent_profiles WHERE name = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(agent).transpose()
    }

    /// Inserts or updates by id; an update bumps the version. Returns it.
    pub async fn save_agent_profile(&self, a: &AgentProfile) -> Result<i64> {
        let now = now_ms();
        let row = sqlx::query(self.sql(
            "INSERT INTO agent_profiles (id, name, display_name, description, prompt, provider, model, \
             tools_json, avatar_svg, meta_json, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, display_name = excluded.display_name, \
             description = excluded.description, prompt = excluded.prompt, provider = excluded.provider, \
             model = excluded.model, tools_json = excluded.tools_json, avatar_svg = excluded.avatar_svg, \
             meta_json = excluded.meta_json, version = agent_profiles.version + 1, \
             updated_at = excluded.updated_at RETURNING version",
        ))
        .bind(&a.id)
        .bind(&a.name)
        .bind(&a.display_name)
        .bind(&a.description)
        .bind(&a.prompt)
        .bind(&a.provider)
        .bind(&a.model)
        .bind(serde_json::to_string(&a.tools)?)
        .bind(&a.avatar_svg)
        .bind(if a.meta_json.is_empty() {
            "{}"
        } else {
            &a.meta_json
        })
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("version")?)
    }

    pub async fn delete_agent_profile(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM agent_profiles WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- crews -------------------------------------------------------------

    pub async fn crews(&self) -> Result<Vec<Crew>> {
        let q = format!("SELECT {CREW_COLS} FROM crews ORDER BY name");
        let rows = sqlx::query(self.sql(&q)).fetch_all(&self.pool).await?;
        rows.iter().map(crew).collect()
    }

    pub async fn crew(&self, id: &str) -> Result<Option<Crew>> {
        let q = format!("SELECT {CREW_COLS} FROM crews WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(crew).transpose()
    }

    /// Inserts or updates by id; an update bumps the version. Returns it.
    pub async fn save_crew(&self, c: &Crew) -> Result<i64> {
        let now = now_ms();
        let row = sqlx::query(self.sql(
            "INSERT INTO crews (id, name, description, process, members_json, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, description = excluded.description, \
             process = excluded.process, members_json = excluded.members_json, \
             version = crews.version + 1, updated_at = excluded.updated_at RETURNING version",
        ))
        .bind(&c.id)
        .bind(&c.name)
        .bind(&c.description)
        .bind(&c.process)
        .bind(serde_json::to_string(&c.members)?)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("version")?)
    }

    pub async fn delete_crew(&self, id: &str) -> Result<bool> {
        let res = sqlx::query(self.sql("DELETE FROM crews WHERE id = ?"))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // ---- library -----------------------------------------------------------

    /// All items, or those of one kind when `kind` is non-empty.
    pub async fn library_items(&self, kind: &str) -> Result<Vec<LibraryItem>> {
        let q = format!(
            "SELECT {ITEM_COLS} FROM library_items WHERE (? = '' OR kind = ?) ORDER BY kind, name"
        );
        let rows = sqlx::query(self.sql(&q))
            .bind(kind)
            .bind(kind)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(item).collect()
    }

    pub async fn library_item(&self, id: &str) -> Result<Option<LibraryItem>> {
        let q = format!("SELECT {ITEM_COLS} FROM library_items WHERE id = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(item).transpose()
    }

    pub async fn library_item_by_name(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Option<LibraryItem>> {
        let q = format!("SELECT {ITEM_COLS} FROM library_items WHERE kind = ? AND name = ?");
        let row = sqlx::query(self.sql(&q))
            .bind(kind)
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(item).transpose()
    }

    /// Saves a new version of the item (created when `id` is unknown) and
    /// returns its number. Name, kind and description follow the save.
    pub async fn save_library_version(
        &self,
        id: &str,
        kind: &str,
        name: &str,
        description: &str,
        body: &str,
        note: &str,
    ) -> Result<i64> {
        let now = now_ms();
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql(
            "INSERT INTO library_items (id, kind, name, description, latest_version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 0, ?, ?) ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, \
             name = excluded.name, description = excluded.description, updated_at = excluded.updated_at",
        ))
        .bind(id)
        .bind(kind)
        .bind(name)
        .bind(description)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(self.sql(
            "UPDATE library_items SET latest_version = latest_version + 1 WHERE id = ? RETURNING latest_version",
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let v: i64 = row.try_get("latest_version")?;
        sqlx::query(self.sql(
            "INSERT INTO library_versions (item_id, version, body, note, created_at) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(id)
        .bind(v)
        .bind(body)
        .bind(note)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(v)
    }

    /// Newest first.
    pub async fn library_versions(&self, item_id: &str) -> Result<Vec<LibraryVersion>> {
        let rows = sqlx::query(self.sql(
            "SELECT item_id, version, body, note, created_at FROM library_versions \
             WHERE item_id = ? ORDER BY version DESC",
        ))
        .bind(item_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(version).collect()
    }

    pub async fn library_latest(&self, item_id: &str) -> Result<Option<LibraryVersion>> {
        Ok(self.library_versions(item_id).await?.into_iter().next())
    }

    pub async fn delete_library_item(&self, id: &str) -> Result<bool> {
        let mut tx = self.begin_write().await?;
        sqlx::query(self.sql("DELETE FROM library_versions WHERE item_id = ?"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query(self.sql("DELETE FROM library_items WHERE id = ?"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let mut out = vec![(s, Some(dir))];
        if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
            let s = Store::open(&url).await.unwrap();
            for t in [
                "vault_secret_versions",
                "vault_secrets",
                "vault_grants",
                "budgets",
                "routing_rules",
                "usage_rows",
                "agent_profiles",
                "crews",
                "library_versions",
                "library_items",
            ] {
                sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                    .execute(&s.pool)
                    .await
                    .unwrap();
            }
            out.push((s, None));
        }
        out
    }

    fn sealed_row(id: &str, provider: &str, env: &str, ct: &[u8]) -> SealedSecret {
        SealedSecret {
            id: id.into(),
            provider: provider.into(),
            env: env.into(),
            label: "prod".into(),
            nonce: vec![7; 24],
            ciphertext: ct.to_vec(),
            created_at: 0,
            rotated_at: None,
            last_used_at: None,
        }
    }

    #[tokio::test]
    async fn secrets_rotate_list_delete() {
        for (s, _d) in stores().await {
            let id = s
                .vault_put_sealed(&sealed_row("s1", "anthropic", "ANTHROPIC_API_KEY", b"ct1"))
                .await
                .unwrap();
            assert_eq!(id, "s1");
            // Same provider and env: a rotation that keeps the first id.
            let id2 = s
                .vault_put_sealed(&sealed_row("s2", "anthropic", "ANTHROPIC_API_KEY", b"ct2"))
                .await
                .unwrap();
            assert_eq!(id2, "s1");
            let row = s.vault_sealed("s1").await.unwrap().unwrap();
            assert_eq!(row.ciphertext, b"ct2");
            assert!(row.rotated_at.is_some());
            assert!(!format!("{row:?}").contains("ct2"));

            assert!(s.vault_reseal("s1", &[1; 24], b"ct3").await.unwrap());
            assert!(s.vault_set_label("s1", "main").await.unwrap());
            assert_eq!(s.vault_sealed_for("anthropic").await.unwrap().len(), 1);
            s.vault_touch_secrets(&["s1".into()]).await.unwrap();
            let list = s.vault_secrets().await.unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].label, "main");
            assert!(list[0].last_used_at.is_some());
            assert!(s.vault_secret_meta("s1").await.unwrap().is_some());
            assert!(s.vault_delete_secret("s1").await.unwrap());
            assert!(s.vault_secrets().await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn versions_grace_shred_models_rotation() {
        for (s, _d) in stores().await {
            s.vault_put_sealed(&sealed_row(
                "v1",
                "openrouter",
                "OPENROUTER_API_KEY",
                b"live",
            ))
            .await
            .unwrap();
            let m = s.vault_secret_meta("v1").await.unwrap().unwrap();
            assert_eq!((m.version, m.rotate_every_days), (1, 0));
            assert!(m.models.is_empty() && m.remote_id.is_empty());
            let retired = SecretVersion {
                secret_id: "v1".into(),
                version: 1,
                fingerprint: "aaaa".into(),
                created_at: 10,
                retired_at: Some(20),
                grace_until: Some(100),
                origin: "owner".into(),
                ..Default::default()
            };
            s.vault_put_version(&retired, Some((&[7; 24], b"old")))
                .await
                .unwrap();
            s.vault_put_version(
                &SecretVersion {
                    secret_id: "v1".into(),
                    version: 2,
                    fingerprint: "bbbb".into(),
                    created_at: 20,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
            assert!(s.vault_set_version("v1", 2, "h-2").await.unwrap());
            let vs = s.vault_versions("v1").await.unwrap();
            assert_eq!(vs.iter().map(|v| v.version).collect::<Vec<_>>(), vec![2, 1]);
            assert!(vs[1].sealed && !vs[0].sealed);
            let sv = s.vault_sealed_version("v1", 1).await.unwrap().unwrap();
            assert_eq!(sv.ciphertext, b"old");
            assert!(!format!("{sv:?}").contains("old\""));
            // Not yet due; then shredded, the lineage kept.
            assert!(s.vault_shred_versions(50).await.unwrap().is_empty());
            assert_eq!(s.vault_shred_versions(100).await.unwrap().len(), 1);
            assert!(s.vault_sealed_version("v1", 1).await.unwrap().is_none());
            assert_eq!(s.vault_versions("v1").await.unwrap()[1].fingerprint, "aaaa");
            assert!(
                s.vault_set_models("v1", &["m-a".into(), "m-b".into()])
                    .await
                    .unwrap()
            );
            assert!(s.vault_set_rotation("v1", 30).await.unwrap());
            let m = s.vault_secret_meta("v1").await.unwrap().unwrap();
            assert_eq!((m.version, m.remote_id.as_str()), (2, "h-2"));
            assert_eq!((m.models.len(), m.rotate_every_days), (2, 30));
            assert!(s.vault_delete_secret("v1").await.unwrap());
            assert!(s.vault_versions("v1").await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn secret_test_fingerprint_alerts_mode() {
        for (s, _d) in stores().await {
            s.vault_put_sealed(&sealed_row("t1", "openai", "OPENAI_API_KEY", b"ct"))
                .await
                .unwrap();
            let m = s.vault_secret_meta("t1").await.unwrap().unwrap();
            assert_eq!(m.test_state, "");
            let t = SecretTest {
                state: "refused".into(),
                status: Some(401),
                ms: Some(12),
                detail: "invalid key".into(),
                at: 5,
            };
            assert!(s.vault_set_test("t1", &t).await.unwrap());
            let m = s.vault_secret_meta("t1").await.unwrap().unwrap();
            assert_eq!(
                (m.test_state.as_str(), m.test_status, m.tested_at),
                ("refused", Some(401), Some(5))
            );
            assert!(s.vault_set_fingerprint("t1", "abcd").await.unwrap());
            let m = s.vault_secret_meta("t1").await.unwrap().unwrap();
            assert_eq!(m.fingerprint, "abcd");
            assert_eq!(m.test_state, "", "a new value clears the last test");
            assert!(
                s.vault_set_scope("t1", Some(77), &["n1".into()], "env")
                    .await
                    .unwrap()
            );
            let m = s.vault_secret_meta("t1").await.unwrap().unwrap();
            assert_eq!(
                (m.expires_at, m.nodes.clone(), m.kind.as_str()),
                (Some(77), vec!["n1".to_string()], "env")
            );
            assert!(s.vault_set_verified("t1", 9, "bad").await.unwrap());
            let m = s.vault_secret_meta("t1").await.unwrap().unwrap();
            assert_eq!((m.verified_at, m.verify_error.as_str()), (Some(9), "bad"));
            s.vault_delete_secret("t1").await.unwrap();

            let p = s.provider("anthropic").await.unwrap().unwrap();
            assert_eq!(p.probe_auth, "x-api-key");
            assert!(p.probe_url.ends_with("/v1/models"));

            let b1 = format!("b-{}", now_ms());
            assert!(s.budget_alert_once(&b1, 100, 80).await.unwrap());
            assert!(!s.budget_alert_once(&b1, 100, 80).await.unwrap());
            assert!(s.budget_alert_once(&b1, 200, 80).await.unwrap());

            assert_eq!(s.factory_mode().await.unwrap(), "");
            s.set_factory_stopped(true, "x").await.unwrap();
            assert_eq!(s.factory_mode().await.unwrap(), "stop");
            s.set_factory_mode("pause").await.unwrap();
            assert_eq!(s.factory_mode().await.unwrap(), "pause");
            s.set_factory_stopped(false, "").await.unwrap();
            s.set_factory_mode("").await.unwrap();
            assert_eq!(s.factory_mode().await.unwrap(), "");
        }
    }

    #[tokio::test]
    async fn grants_log() {
        for (s, _d) in stores().await {
            let g = GrantRecord {
                id: "g1".into(),
                providers: vec!["anthropic".into()],
                secret_ids: vec!["s1".into()],
                missing: vec!["openai".into()],
                purpose: "job".into(),
                created_at: 1,
                expires_at: 2,
                revoked_at: None,
            };
            s.vault_insert_grant(&g).await.unwrap();
            assert_eq!(s.vault_grant("g1").await.unwrap().unwrap(), g);
            assert!(s.vault_revoke_grant("g1").await.unwrap());
            assert!(!s.vault_revoke_grant("g1").await.unwrap());
            let all = s.vault_grants(10).await.unwrap();
            assert!(all[0].revoked_at.is_some());
        }
    }

    #[tokio::test]
    async fn providers_catalogue() {
        for (s, _d) in stores().await {
            let all = s.providers().await.unwrap();
            assert!(
                all.iter()
                    .any(|p| p.id == "anthropic" && p.env == "ANTHROPIC_API_KEY")
            );
            let mut p = s.provider("openai").await.unwrap().unwrap();
            p.factory = false;
            p.default_model = "gpt-x".into();
            s.upsert_provider(&p).await.unwrap();
            let p2 = s.provider("openai").await.unwrap().unwrap();
            assert!(!p2.factory);
            assert_eq!(p2.default_model, "gpt-x");
            s.upsert_provider(&Provider {
                id: "local".into(),
                name: "Local".into(),
                ..Default::default()
            })
            .await
            .unwrap();
            assert!(s.delete_provider("local").await.unwrap());
        }
    }

    #[tokio::test]
    async fn budgets_revisions() {
        for (s, _d) in stores().await {
            let mut b = Budget {
                id: "b1".into(),
                name: "Month".into(),
                applies_to: "all".into(),
                provider: "all".into(),
                period: "month".into(),
                limit_tokens: Some(1000),
                alert_at: vec![80, 100],
                action_at_limit: "hard_stop".into(),
                ..Default::default()
            };
            assert_eq!(s.save_budget(&b).await.unwrap(), Some(1));
            b.revision = 1;
            b.limit_usd = Some(5.0);
            assert_eq!(s.save_budget(&b).await.unwrap(), Some(2));
            // Stale revision refused.
            assert_eq!(s.save_budget(&b).await.unwrap(), None);
            let got = s.budget("b1").await.unwrap().unwrap();
            assert_eq!(got.limit_usd, Some(5.0));
            assert_eq!(got.alert_at, vec![80, 100]);
            assert_eq!(s.budgets().await.unwrap().len(), 1);
            assert!(s.delete_budget("b1").await.unwrap());
        }
    }

    #[tokio::test]
    async fn routing_rules_order() {
        for (s, _d) in stores().await {
            for (id, prio) in [("r2", 20), ("r1", 10)] {
                s.upsert_routing_rule(&RoutingRule {
                    id: id.into(),
                    priority: prio,
                    provider: "anthropic".into(),
                    enabled: true,
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            let rules = s.routing_rules().await.unwrap();
            assert_eq!(rules[0].id, "r1");
            assert!(rules[0].enabled);
            assert!(s.delete_routing_rule("r1").await.unwrap());
            assert_eq!(s.routing_rules().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn usage_sums() {
        for (s, _d) in stores().await {
            let mut u = UsageRow {
                id: "session:a".into(),
                at: 100,
                source: "session".into(),
                session_id: "a".into(),
                node_id: "n1".into(),
                provider: "anthropic".into(),
                model: "m".into(),
                agent: "patcher".into(),
                repo: "r".into(),
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: 0.5,
            };
            s.upsert_usage(&u).await.unwrap();
            // Cumulative: replaces.
            u.input_tokens = 20;
            s.upsert_usage(&u).await.unwrap();
            s.upsert_usage(&UsageRow {
                id: "job:1".into(),
                at: 200,
                source: "job".into(),
                provider: "openai".into(),
                input_tokens: 100,
                ..Default::default()
            })
            .await
            .unwrap();
            let all = s.usage_sum(&UsageFilter::default()).await.unwrap();
            assert_eq!(all.0, 125);
            let anth = s
                .usage_sum(&UsageFilter {
                    providers: vec!["anthropic".into()],
                    model: "anthropic/m".into(),
                    agent: "patcher".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(anth, (25, 0.5));
            let late = s
                .usage_sum(&UsageFilter {
                    since: 150,
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(late.0, 100);
            let by = s.usage_by(0, "provider").await.unwrap();
            assert_eq!(by[0].key, "openai");
            assert_eq!(s.usage_by(0, "node").await.unwrap().len(), 2);
            assert!(s.usage_by(0, "nope").await.is_err());
            assert_eq!(s.usage_rows(0, 10).await.unwrap()[0].id, "job:1");
        }
    }

    #[tokio::test]
    async fn agents_and_crews() {
        for (s, _d) in stores().await {
            let mut a = AgentProfile {
                id: "a1".into(),
                name: "patcher".into(),
                prompt: "fix".into(),
                tools: vec!["Bash".into()],
                avatar_svg: "<svg/>".into(),
                ..Default::default()
            };
            assert_eq!(s.save_agent_profile(&a).await.unwrap(), 1);
            a.description = "patches".into();
            assert_eq!(s.save_agent_profile(&a).await.unwrap(), 2);
            let got = s.agent_profile_by_name("patcher").await.unwrap().unwrap();
            assert_eq!(got.tools, vec!["Bash".to_string()]);
            assert_eq!(got.meta_json, "{}");
            assert_eq!(s.agent_profiles().await.unwrap().len(), 1);
            assert!(s.agent_profile("a1").await.unwrap().is_some());

            let c = Crew {
                id: "c1".into(),
                name: "security".into(),
                process: "sequential".into(),
                members: vec![CrewMember {
                    agent: "patcher".into(),
                    role: "fixer".into(),
                    count: 2,
                    model: "acme/fast-1".into(),
                }],
                ..Default::default()
            };
            assert_eq!(s.save_crew(&c).await.unwrap(), 1);
            assert_eq!(s.save_crew(&c).await.unwrap(), 2);
            assert_eq!(s.crew("c1").await.unwrap().unwrap().members[0].count, 2);
            assert_eq!(
                s.crew("c1").await.unwrap().unwrap().members[0].model,
                "acme/fast-1"
            );
            // Members saved before the field read as no model.
            let old: Vec<CrewMember> =
                serde_json::from_str(r#"[{"agent":"a","role":"r","count":1}]"#).unwrap();
            assert_eq!(old[0].model, "");
            assert_eq!(s.crews().await.unwrap().len(), 1);
            assert!(s.delete_crew("c1").await.unwrap());
            assert!(s.delete_agent_profile("a1").await.unwrap());
        }
    }

    #[tokio::test]
    async fn library_versions() {
        for (s, _d) in stores().await {
            assert_eq!(
                s.save_library_version("i1", "prompt", "review", "", "v1 body", "")
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                s.save_library_version("i1", "prompt", "review", "d", "v2 body", "tweak")
                    .await
                    .unwrap(),
                2
            );
            s.save_library_version("i2", "skill", "triage", "", "x", "")
                .await
                .unwrap();
            assert_eq!(s.library_items("").await.unwrap().len(), 2);
            assert_eq!(s.library_items("skill").await.unwrap().len(), 1);
            let it = s
                .library_item_by_name("prompt", "review")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(it.latest_version, 2);
            assert_eq!(it.description, "d");
            assert!(s.library_item("i1").await.unwrap().is_some());
            let latest = s.library_latest("i1").await.unwrap().unwrap();
            assert_eq!(latest.body, "v2 body");
            assert_eq!(s.library_versions("i1").await.unwrap().len(), 2);
            assert!(s.delete_library_item("i1").await.unwrap());
            assert!(s.library_versions("i1").await.unwrap().is_empty());
        }
    }
}
