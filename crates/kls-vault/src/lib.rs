//! The Keleustes vault: model provider keys sealed at rest.
//!
//! Each value is sealed with XChaCha20-Poly1305 under a 256-bit master key.
//! The master key lives in the OS keyring (service `keleustes`, account = the
//! server id). Without a usable keyring it falls back to a `0600` key file in
//! the data directory and [`Vault::warning`] says so.
//!
//! Keleustes never calls a model itself: it hands keys to belai nodes in a
//! grant ([`Vault::grant`]) attached to a dispatch or job. Every grant is an
//! audit row naming providers and secret ids, never values. Plaintext is held
//! in [`SecretString`], which zeroizes on drop and never prints.

use anyhow::{Context, Result, anyhow, bail};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use kls_store::Store;
use kls_store::vault::SealedSecret;
pub use kls_store::vault::{GrantRecord, SecretMeta, SecretVersion};

/// How a rotation keeps the value it replaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RotateOpts {
    /// Keep the old value sealed this long for a rollback (0: shred now).
    pub grace_ms: i64,
    /// Who rotated: "owner", "auto-rotate", "rollback:v3".
    pub origin: String,
    /// The provider's handle of the new key (a minted key), if any.
    pub remote_id: String,
}
use rand::RngCore;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

/// Keyring service name.
pub const KEYRING_SERVICE: &str = "keleustes";
/// Settings key holding a sealed known value that proves the master key.
pub const KEY_CHECK_SETTING: &str = "vault.key_check";
/// Longest value accepted (bytes).
pub const MAX_VALUE_LEN: usize = 16 * 1024;
/// Grant lifetime bounds.
pub const MIN_GRANT_TTL_MS: i64 = 1_000;
pub const MAX_GRANT_TTL_MS: i64 = 24 * 3_600_000;

const KEY_CHECK_PLAIN: &[u8] = b"keleustes-vault-key-check-v1";
const KEY_CHECK_AAD: &[u8] = b"kls-vault:v1:key-check";

// ---- SecretString -----------------------------------------------------------

/// A secret value. Zeroized on drop; `Debug` never shows it and there is no
/// `Display`. Read it with [`SecretString::expose`] only where it must leave.
#[derive(Clone, Default)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

// ---- grants -------------------------------------------------------------------

/// One secret in a grant.
#[derive(Clone)]
pub struct GrantSecret {
    pub provider: String,
    /// Environment variable the provider reads, e.g. ANTHROPIC_API_KEY.
    pub env: String,
    pub value: SecretString,
}

impl std::fmt::Debug for GrantSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantSecret")
            .field("provider", &self.provider)
            .field("env", &self.env)
            .field("value", &self.value)
            .finish()
    }
}

/// Keys for one dispatch or job; maps 1:1 onto the protocol's `VaultGrant`.
#[derive(Clone, Debug, Default)]
pub struct VaultGrantData {
    pub grant_id: String,
    pub secrets: Vec<GrantSecret>,
    pub expires_at_ms: i64,
    /// Providers asked for that the vault holds no key for.
    pub missing: Vec<String>,
    /// Keys held but refused: "<provider> <ENV>: expired" or "…: not for this node".
    pub denied: Vec<String>,
}

// ---- master key ---------------------------------------------------------------

struct MasterKey(Zeroizing<[u8; 32]>);

impl MasterKey {
    fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        rand::rng().fill_bytes(&mut *k);
        Self(k)
    }

    fn from_hex(s: &str) -> Result<Self> {
        let mut bytes = Zeroizing::new(hex::decode(s.trim()).context("master key is not hex")?);
        if bytes.len() != 32 {
            bail!("master key has the wrong length");
        }
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&bytes);
        bytes.zeroize();
        Ok(Self(k))
    }

    fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex::encode(*self.0))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&*self.0).into())
    }
}

/// Where the master key is kept.
#[derive(Clone, Debug)]
pub enum KeyBackend {
    /// The OS keyring (service [`KEYRING_SERVICE`], this account), falling
    /// back to `fallback_file` when no keyring is usable.
    Keyring {
        account: String,
        fallback_file: PathBuf,
    },
    /// A `0600` key file only (tests, hosts without a keyring).
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    File,
}

fn keyring_get(account: &str) -> Result<Option<MasterKey>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account)?;
    match entry.get_password() {
        Ok(s) => {
            let s = Zeroizing::new(s);
            Ok(Some(MasterKey::from_hex(&s)?))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn keyring_set(account: &str, key: &MasterKey) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account)?;
    entry.set_password(&key.to_hex())?;
    // Read it back through a fresh entry: some backends accept a write they
    // cannot keep (a locked or session-only store).
    match keyring_get(account)? {
        Some(k) if *k.0 == *key.0 => Ok(()),
        _ => bail!("the keyring did not keep the key"),
    }
}

fn read_key_file(path: &Path) -> Result<MasterKey> {
    let s = Zeroizing::new(std::fs::read_to_string(path).context("read vault key file")?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    MasterKey::from_hex(&s)
}

fn write_key_file(path: &Path, key: &MasterKey) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).context("create vault key file")?;
    f.write_all(key.to_hex().as_bytes())?;
    f.sync_all()?;
    Ok(())
}

fn file_key(path: &Path) -> Result<MasterKey> {
    if path.exists() {
        return read_key_file(path);
    }
    let k = MasterKey::generate();
    write_key_file(path, &k)?;
    Ok(k)
}

fn load_key(backend: &KeyBackend) -> Result<(MasterKey, KeySource, Option<String>)> {
    match backend {
        KeyBackend::File(p) => Ok((file_key(p)?, KeySource::File, None)),
        KeyBackend::Keyring {
            account,
            fallback_file,
        } => {
            let file_warning = |why: String| {
                format!(
                    "No usable OS keyring ({why}); the vault master key is in {} (mode 0600).",
                    fallback_file.display()
                )
            };
            match keyring_get(account) {
                Ok(Some(k)) => Ok((k, KeySource::Keyring, None)),
                Ok(None) if fallback_file.exists() => {
                    // An earlier run had no keyring: keep using its key, or
                    // every sealed secret would become unreadable.
                    let k = read_key_file(fallback_file)?;
                    Ok((
                        k,
                        KeySource::File,
                        Some(file_warning(
                            "an earlier run stored the key in a file".into(),
                        )),
                    ))
                }
                Ok(None) => {
                    let k = MasterKey::generate();
                    match keyring_set(account, &k) {
                        Ok(()) => Ok((k, KeySource::Keyring, None)),
                        Err(e) => {
                            tracing::warn!("vault: keyring unusable, using key file: {e:#}");
                            write_key_file(fallback_file, &k)?;
                            Ok((k, KeySource::File, Some(file_warning(format!("{e:#}")))))
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("vault: keyring unavailable, using key file: {e:#}");
                    let k = file_key(fallback_file)?;
                    Ok((k, KeySource::File, Some(file_warning(format!("{e:#}")))))
                }
            }
        }
    }
}

// ---- validation ---------------------------------------------------------------

/// A provider slug: lower-case letters, digits, `.`, `_`, `-`; 1..64 long.
pub fn valid_provider(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// An environment variable name: `[A-Z_][A-Z0-9_]*`, at most 64 long.
pub fn valid_env(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Names a generic env-var secret may not take: they would change how the
/// node or its shell runs.
pub fn reserved_env(name: &str) -> bool {
    const EXACT: [&str; 10] = [
        "PATH",
        "HOME",
        "SHELL",
        "USER",
        "PWD",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "IFS",
        "TERM",
        "SSH_AUTH_SOCK",
    ];
    const PREFIX: [&str; 5] = ["DYLD_", "BELAI_", "VULNETIX_", "KELEUSTES_", "LD_"];
    EXACT.contains(&name) || PREFIX.iter().any(|p| name.starts_with(p))
}

/// The shortest value an env-var secret may have, so output scrubbing
/// cannot match ordinary text.
pub const MIN_ENV_VALUE_LEN: usize = 8;

fn check_value(v: &str) -> Result<()> {
    if v.is_empty() {
        bail!("the key is empty");
    }
    if v.len() > MAX_VALUE_LEN {
        bail!("the key is longer than {MAX_VALUE_LEN} bytes");
    }
    if v.chars().any(|c| c.is_control()) {
        bail!("the key holds a control character");
    }
    Ok(())
}

/// Suffix of the provider slug a management (provisioning) key is filed
/// under: `openrouter-management` holds the key that mints `openrouter` keys.
pub const MANAGEMENT_SUFFIX: &str = "-management";

/// Whether `provider` holds management keys (never granted to a node).
pub fn is_management_provider(provider: &str) -> bool {
    provider.ends_with(MANAGEMENT_SUFFIX)
}

/// First 16 hex chars of sha256(value): identifies a value without
/// revealing it.
pub fn fingerprint(v: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(v.as_bytes());
    hex::encode(&d[..8])
}

/// Callers pass exactly 24 bytes (checked before every use).
fn nonce_of(b: &[u8]) -> XNonce {
    let mut n = XNonce::default();
    n.copy_from_slice(b);
    n
}

fn aad(id: &str, provider: &str, env: &str) -> Vec<u8> {
    format!("kls-vault:v1:{id}:{provider}:{env}").into_bytes()
}

// ---- Vault --------------------------------------------------------------------

pub struct Vault {
    store: Store,
    key: MasterKey,
    source: KeySource,
    warning: Option<String>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("source", &self.source)
            .field("warning", &self.warning)
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// Loads (or creates) the master key and checks it opens this store's
    /// secrets.
    pub async fn open(store: Store, backend: KeyBackend) -> Result<Self> {
        let (key, source, warning) = tokio::task::spawn_blocking(move || load_key(&backend))
            .await
            .map_err(|e| anyhow!("vault key task: {e}"))??;
        let v = Self {
            store,
            key,
            source,
            warning,
        };
        v.check_key().await?;
        Ok(v)
    }

    async fn check_key(&self) -> Result<()> {
        match self.store.setting(KEY_CHECK_SETTING).await? {
            Some(hexed) => {
                let raw = hex::decode(hexed.trim()).context("vault key check is corrupt")?;
                if raw.len() < 24 {
                    bail!("vault key check is corrupt");
                }
                let (n, ct) = raw.split_at(24);
                let ok = self
                    .key
                    .cipher()
                    .decrypt(
                        &nonce_of(n),
                        Payload {
                            msg: ct,
                            aad: KEY_CHECK_AAD,
                        },
                    )
                    .map(|p| p == KEY_CHECK_PLAIN)
                    .unwrap_or(false);
                if !ok {
                    bail!(
                        "the vault master key does not match this database's sealed secrets \
                         (was the keyring entry or key file replaced?)"
                    );
                }
            }
            None => {
                let (n, ct) = self.seal_raw(KEY_CHECK_PLAIN, KEY_CHECK_AAD)?;
                let mut raw = n;
                raw.extend_from_slice(&ct);
                self.store
                    .set_setting(KEY_CHECK_SETTING, &hex::encode(raw))
                    .await?;
            }
        }
        Ok(())
    }

    /// Where the master key came from.
    pub fn key_source(&self) -> KeySource {
        self.source
    }

    /// Set when the key is in a file because no keyring was usable.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    fn seal_raw(&self, plain: &[u8], aad: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut n = [0u8; 24];
        rand::rng().fill_bytes(&mut n);
        let ct = self
            .key
            .cipher()
            .encrypt(&nonce_of(&n), Payload { msg: plain, aad })
            .map_err(|_| anyhow!("seal failed"))?;
        Ok((n.to_vec(), ct))
    }

    fn open_sealed(&self, s: &SealedSecret) -> Result<SecretString> {
        if s.nonce.len() != 24 {
            bail!("sealed secret {} is corrupt", s.id);
        }
        let plain = self
            .key
            .cipher()
            .decrypt(
                &nonce_of(&s.nonce),
                Payload {
                    msg: &s.ciphertext,
                    aad: &aad(&s.id, &s.provider, &s.env),
                },
            )
            .map_err(|_| anyhow!("secret {} does not open with this master key", s.id))?;
        match String::from_utf8(plain) {
            Ok(s) => Ok(SecretString::new(s)),
            Err(e) => {
                let mut b = e.into_bytes();
                b.zeroize();
                bail!("secret {} is not text", s.id)
            }
        }
    }

    /// Stores a key for `provider` under `env`, replacing (rotating) the one
    /// already held for that pair. Returns its metadata.
    pub async fn put(
        &self,
        provider: &str,
        env: &str,
        label: &str,
        value: SecretString,
    ) -> Result<SecretMeta> {
        if !valid_provider(provider) {
            bail!("{provider:?} is not a provider slug");
        }
        if !valid_env(env) {
            bail!("{env:?} is not an environment variable name");
        }
        let trimmed = SecretString::new(value.expose().trim());
        drop(value);
        check_value(trimmed.expose())?;
        // A pair already held: a rotation (lineage kept, no grace).
        if let Some(old) = self
            .store
            .vault_sealed_for(provider)
            .await?
            .into_iter()
            .find(|s| s.env == env)
        {
            self.store.vault_set_label(&old.id, label.trim()).await?;
            return self
                .rotate_with(&old.id, trimmed, &RotateOpts::default())
                .await;
        }
        let id = self
            .store
            .vault_sealed_for(provider)
            .await?
            .into_iter()
            .find(|s| s.env == env)
            .map(|s| s.id)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let (nonce, ciphertext) =
            self.seal_raw(trimmed.expose().as_bytes(), &aad(&id, provider, env))?;
        let kept = self
            .store
            .vault_put_sealed(&SealedSecret {
                id: id.clone(),
                provider: provider.into(),
                env: env.into(),
                label: label.trim().into(),
                nonce,
                ciphertext,
                created_at: 0,
                rotated_at: None,
                last_used_at: None,
            })
            .await?;
        if kept != id {
            // Lost a race with another writer for the same pair: our row was
            // sealed for an id that is not the one kept. Reseal under it.
            let (n, ct) = self.seal_raw(trimmed.expose().as_bytes(), &aad(&kept, provider, env))?;
            self.store.vault_reseal(&kept, &n, &ct).await?;
        }
        let fp = fingerprint(trimmed.expose());
        self.store.vault_set_fingerprint(&kept, &fp).await?;
        self.store
            .vault_put_version(
                &SecretVersion {
                    secret_id: kept.clone(),
                    version: 1,
                    fingerprint: fp,
                    created_at: kls_store::now_ms(),
                    origin: "owner".into(),
                    ..Default::default()
                },
                None,
            )
            .await?;
        tracing::info!(secret = %kept, provider, env, "vault: key stored");
        self.store
            .vault_secret_meta(&kept)
            .await?
            .ok_or_else(|| anyhow!("secret vanished after save"))
    }

    /// Replaces the value of secret `id` (no grace: the old value is not kept).
    pub async fn rotate(&self, id: &str, value: SecretString) -> Result<SecretMeta> {
        self.rotate_with(id, value, &RotateOpts::default()).await
    }

    /// Replaces the value of secret `id` and records the lineage: the old
    /// version is retired with its fingerprint and, with a grace, stays
    /// sealed (never plaintext) until `now + grace_ms` so [`Vault::rollback`]
    /// can restore it. The new version is `old + 1`.
    pub async fn rotate_with(
        &self,
        id: &str,
        value: SecretString,
        opts: &RotateOpts,
    ) -> Result<SecretMeta> {
        let meta = self
            .store
            .vault_secret_meta(id)
            .await?
            .ok_or_else(|| anyhow!("no such secret"))?;
        let old = self
            .store
            .vault_sealed(id)
            .await?
            .ok_or_else(|| anyhow!("no such secret"))?;
        let trimmed = SecretString::new(value.expose().trim());
        drop(value);
        check_value(trimmed.expose())?;
        let now = kls_store::now_ms();
        let old_v = meta.version.max(1);
        let known = self.store.vault_versions(id).await?;
        let prior = known.iter().find(|v| v.version == old_v);
        let grace = opts.grace_ms.max(0);
        self.store
            .vault_put_version(
                &SecretVersion {
                    secret_id: id.into(),
                    version: old_v,
                    fingerprint: meta.fingerprint.clone(),
                    created_at: prior
                        .map(|p| p.created_at)
                        .unwrap_or(meta.rotated_at.unwrap_or(meta.created_at)),
                    retired_at: Some(now),
                    grace_until: (grace > 0).then_some(now + grace),
                    sealed: grace > 0,
                    remote_id: meta.remote_id.clone(),
                    origin: prior.map(|p| p.origin.clone()).unwrap_or_default(),
                },
                (grace > 0).then_some((old.nonce.as_slice(), old.ciphertext.as_slice())),
            )
            .await?;
        let (n, ct) = self.seal_raw(
            trimmed.expose().as_bytes(),
            &aad(id, &meta.provider, &meta.env),
        )?;
        self.store.vault_reseal(id, &n, &ct).await?;
        let fp = fingerprint(trimmed.expose());
        self.store.vault_set_fingerprint(id, &fp).await?;
        let version = known
            .iter()
            .map(|v| v.version)
            .max()
            .unwrap_or(0)
            .max(old_v)
            + 1;
        self.store
            .vault_set_version(id, version, &opts.remote_id)
            .await?;
        self.store
            .vault_put_version(
                &SecretVersion {
                    secret_id: id.into(),
                    version,
                    fingerprint: fp,
                    created_at: now,
                    remote_id: opts.remote_id.clone(),
                    origin: if opts.origin.is_empty() {
                        "owner".into()
                    } else {
                        opts.origin.clone()
                    },
                    ..Default::default()
                },
                None,
            )
            .await?;
        tracing::info!(secret = %id, provider = %meta.provider, version, "vault: key rotated");
        self.store
            .vault_secret_meta(id)
            .await?
            .ok_or_else(|| anyhow!("secret vanished after rotate"))
    }

    /// Restores the newest retired value still inside its grace as a new
    /// version (the lineage shows the rollback). Errors when none is left.
    pub async fn rollback(&self, id: &str, origin: &str) -> Result<SecretMeta> {
        let now = kls_store::now_ms();
        let meta = self
            .store
            .vault_secret_meta(id)
            .await?
            .ok_or_else(|| anyhow!("no such secret"))?;
        let Some(v) = self
            .store
            .vault_versions(id)
            .await?
            .into_iter()
            .find(|v| v.sealed && v.grace_until.is_some_and(|g| g > now))
        else {
            bail!("no earlier value is inside its rotation grace");
        };
        let sv = self
            .store
            .vault_sealed_version(id, v.version)
            .await?
            .ok_or_else(|| anyhow!("the earlier value was shredded"))?;
        let value = self.open_sealed(&SealedSecret {
            id: id.into(),
            provider: meta.provider.clone(),
            env: meta.env.clone(),
            label: String::new(),
            nonce: sv.nonce,
            ciphertext: sv.ciphertext,
            created_at: 0,
            rotated_at: None,
            last_used_at: None,
        })?;
        self.rotate_with(
            id,
            value,
            &RotateOpts {
                grace_ms: v.grace_until.unwrap_or(now) - now,
                origin: format!("rollback:v{} by {origin}", v.version),
                remote_id: v.remote_id.clone(),
            },
        )
        .await
    }

    /// Opens retired version `version` while it is inside its grace (for a
    /// provider-side revoke of a minted key, never for a grant).
    pub async fn retired_value(&self, id: &str, version: i64) -> Result<Option<SecretString>> {
        let Some(meta) = self.store.vault_secret_meta(id).await? else {
            return Ok(None);
        };
        let Some(sv) = self.store.vault_sealed_version(id, version).await? else {
            return Ok(None);
        };
        self.open_sealed(&SealedSecret {
            id: id.into(),
            provider: meta.provider,
            env: meta.env,
            label: String::new(),
            nonce: sv.nonce,
            ciphertext: sv.ciphertext,
            created_at: 0,
            rotated_at: None,
            last_used_at: None,
        })
        .map(Some)
    }

    /// Shreds retired values whose grace ended (lineage and fingerprints
    /// stay). Returns the versions shredded.
    pub async fn sweep_grace(&self, now: i64) -> Result<Vec<SecretVersion>> {
        let out = self.store.vault_shred_versions(now).await?;
        if !out.is_empty() {
            tracing::info!(count = out.len(), "vault: retired values shredded");
        }
        Ok(out)
    }

    /// A key's lineage, newest first (fingerprints, never values).
    pub async fn versions(&self, id: &str) -> Result<Vec<SecretVersion>> {
        self.store.vault_versions(id).await
    }

    /// Seals a named internal secret (a signing key) into the settings
    /// table under the master key. Never granted to nodes.
    pub async fn put_blob(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let aad = format!("kls-vault:v1:blob:{name}");
        let (n, ct) = self.seal_raw(bytes, aad.as_bytes())?;
        let mut raw = n;
        raw.extend_from_slice(&ct);
        self.store
            .set_setting(&format!("vault.blob.{name}"), &hex::encode(raw))
            .await
    }

    /// Opens a named internal secret sealed with [`Vault::put_blob`].
    pub async fn get_blob(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let Some(hexed) = self.store.setting(&format!("vault.blob.{name}")).await? else {
            return Ok(None);
        };
        let raw = hex::decode(hexed.trim()).context("sealed blob is corrupt")?;
        if raw.len() < 24 {
            bail!("sealed blob is corrupt");
        }
        let (n, ct) = raw.split_at(24);
        let aad = format!("kls-vault:v1:blob:{name}");
        let plain = self
            .key
            .cipher()
            .decrypt(
                &nonce_of(n),
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("sealed blob {name} does not open with this master key"))?;
        Ok(Some(Zeroizing::new(plain)))
    }

    pub async fn set_label(&self, id: &str, label: &str) -> Result<bool> {
        self.store.vault_set_label(id, label.trim()).await
    }

    /// Integrity check of secret `id`: the sealed value opens with the
    /// master key and matches the fingerprint kept when it was saved (a key
    /// saved before fingerprints existed gets one now). The result is
    /// recorded on the row; `Ok(None)` when the secret is unknown, else the
    /// failure reason ("" when it passed).
    pub async fn verify(&self, id: &str) -> Result<Option<String>> {
        let Some(s) = self.store.vault_sealed(id).await? else {
            return Ok(None);
        };
        let meta = self.store.vault_secret_meta(id).await?.unwrap_or_default();
        let error = match self.open_sealed(&s) {
            Err(e) => format!("{e:#}"),
            Ok(v) => {
                let fp = fingerprint(v.expose());
                if meta.fingerprint.is_empty() {
                    self.store.vault_set_fingerprint(id, &fp).await?;
                    // Keep the last test: the value did not change.
                    if !meta.test_state.is_empty() {
                        self.store
                            .vault_set_test(
                                id,
                                &kls_store::vault::SecretTest {
                                    state: meta.test_state.clone(),
                                    status: meta.test_status,
                                    ms: meta.test_ms,
                                    detail: meta.test_detail.clone(),
                                    at: meta.tested_at.unwrap_or_default(),
                                },
                            )
                            .await?;
                    }
                    String::new()
                } else if fp != meta.fingerprint {
                    "the value does not match its fingerprint".into()
                } else {
                    String::new()
                }
            }
        };
        self.store
            .vault_set_verified(id, kls_store::now_ms(), &error)
            .await?;
        Ok(Some(error))
    }

    /// The plaintext of secret `id`.
    pub async fn get(&self, id: &str) -> Result<Option<SecretString>> {
        match self.store.vault_sealed(id).await? {
            Some(s) => Ok(Some(self.open_sealed(&s)?)),
            None => Ok(None),
        }
    }

    /// Metadata only, never values.
    pub async fn list(&self) -> Result<Vec<SecretMeta>> {
        self.store.vault_secrets().await
    }

    pub async fn delete(&self, id: &str) -> Result<bool> {
        let gone = self.store.vault_delete_secret(id).await?;
        if gone {
            tracing::info!(secret = %id, "vault: key deleted");
        }
        Ok(gone)
    }

    /// Whether the vault holds any key for `provider`.
    pub async fn has_provider(&self, provider: &str) -> Result<bool> {
        Ok(!self.store.vault_sealed_for(provider).await?.is_empty())
    }

    /// Opens every key held for `providers` into a grant that lives `ttl_ms`
    /// (clamped to 1 s .. 24 h). Providers without a key are listed in
    /// `missing`. A grant that carries at least one secret is recorded in the
    /// grant log (providers and secret ids, never values); an empty one is not
    /// recorded and has an empty `grant_id`.
    pub async fn grant(
        &self,
        providers: &[String],
        ttl_ms: i64,
        purpose: &str,
    ) -> Result<VaultGrantData> {
        self.grant_scoped(providers, ttl_ms, purpose, None).await
    }

    /// [`Vault::grant`] for one node: keys past their expiry, and keys
    /// scoped to other nodes, are left out and listed in `denied`.
    pub async fn grant_scoped(
        &self,
        providers: &[String],
        ttl_ms: i64,
        purpose: &str,
        node: Option<&str>,
    ) -> Result<VaultGrantData> {
        let now = kls_store::now_ms();
        let mut wanted: Vec<String> = Vec::new();
        for p in providers {
            let p = p.trim();
            if !valid_provider(p) {
                bail!("{p:?} is not a provider slug");
            }
            if !wanted.iter().any(|w| w == p) {
                wanted.push(p.to_string());
            }
        }
        let ttl = ttl_ms.clamp(MIN_GRANT_TTL_MS, MAX_GRANT_TTL_MS);
        let mut out = VaultGrantData::default();
        let mut ids = Vec::new();
        for p in &wanted {
            // Management keys mint and revoke keys here; they never leave.
            if is_management_provider(p) {
                out.denied
                    .push(format!("{p}: management keys are never granted"));
                continue;
            }
            let rows = self.store.vault_sealed_for(p).await?;
            if rows.is_empty() {
                out.missing.push(p.clone());
            }
            for s in rows {
                let meta = self
                    .store
                    .vault_secret_meta(&s.id)
                    .await?
                    .unwrap_or_default();
                if meta.expires_at.is_some_and(|e| e <= now) {
                    out.denied
                        .push(format!("{} {}: expired", s.provider, s.env));
                    continue;
                }
                if let Some(n) = node
                    && !meta.nodes.is_empty()
                    && !meta.nodes.iter().any(|x| x == n)
                {
                    out.denied
                        .push(format!("{} {}: not for this node", s.provider, s.env));
                    continue;
                }
                out.secrets.push(GrantSecret {
                    provider: s.provider.clone(),
                    env: s.env.clone(),
                    value: self.open_sealed(&s)?,
                });
                ids.push(s.id);
            }
        }
        if out.secrets.is_empty() {
            return Ok(out);
        }
        let now = kls_store::now_ms();
        out.grant_id = uuid::Uuid::now_v7().to_string();
        out.expires_at_ms = now + ttl;
        self.store
            .vault_insert_grant(&GrantRecord {
                id: out.grant_id.clone(),
                providers: wanted.clone(),
                secret_ids: ids.clone(),
                missing: out.missing.clone(),
                purpose: purpose.into(),
                created_at: now,
                expires_at: out.expires_at_ms,
                revoked_at: None,
            })
            .await?;
        self.store.vault_touch_secrets(&ids).await?;
        tracing::info!(grant = %out.grant_id, providers = ?wanted, purpose, "vault: grant issued");
        Ok(out)
    }

    /// Marks a grant revoked. Returns false when unknown or already revoked.
    pub async fn revoke_grant(&self, id: &str) -> Result<bool> {
        let done = self.store.vault_revoke_grant(id).await?;
        if done {
            tracing::info!(grant = %id, "vault: grant revoked");
        }
        Ok(done)
    }

    /// A grant is live when recorded, not revoked and not expired.
    pub async fn grant_live(&self, id: &str) -> Result<bool> {
        Ok(self
            .store
            .vault_grant(id)
            .await?
            .is_some_and(|g| g.revoked_at.is_none() && g.expires_at > kls_store::now_ms()))
    }

    /// The grant log, newest first.
    pub async fn grants(&self, limit: i64) -> Result<Vec<GrantRecord>> {
        self.store.vault_grants(limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-ant-test-0123456789-supersecret";

    async fn vault() -> (Vault, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let v = Vault::open(store, KeyBackend::File(dir.path().join("vault.key")))
            .await
            .unwrap();
        (v, dir)
    }

    #[tokio::test]
    async fn put_get_list_rotate_delete() {
        let (v, _d) = vault().await;
        assert_eq!(v.key_source(), KeySource::File);
        assert!(v.warning().is_none());
        let m = v
            .put(
                "anthropic",
                "ANTHROPIC_API_KEY",
                "prod",
                SecretString::new(format!("  {KEY}\n")),
            )
            .await
            .unwrap();
        assert_eq!(v.get(&m.id).await.unwrap().unwrap().expose(), KEY);
        // Same pair again: rotation keeps the id.
        let m2 = v
            .put(
                "anthropic",
                "ANTHROPIC_API_KEY",
                "prod",
                SecretString::new("k2"),
            )
            .await
            .unwrap();
        assert_eq!(m2.id, m.id);
        assert!(m2.rotated_at.is_some());
        assert_eq!(v.get(&m.id).await.unwrap().unwrap().expose(), "k2");
        v.rotate(&m.id, SecretString::new("k3")).await.unwrap();
        assert_eq!(v.get(&m.id).await.unwrap().unwrap().expose(), "k3");
        assert!(v.set_label(&m.id, "main").await.unwrap());

        let list = v.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "main");
        assert!(!format!("{list:?}").contains("k3"));
        assert!(v.has_provider("anthropic").await.unwrap());

        // The ciphertext is not the plaintext.
        let sealed = v.store().vault_sealed(&m.id).await.unwrap().unwrap();
        assert!(!sealed.ciphertext.windows(2).any(|w| w == b"k3"));

        assert!(v.delete(&m.id).await.unwrap());
        assert!(v.get(&m.id).await.unwrap().is_none());
    }

    /// Every byte the vault persisted (database, WAL, key file) lacks `plain`.
    fn on_disk_lacks(dir: &Path, plain: &str) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_file() {
                let b = std::fs::read(e.path()).unwrap();
                assert!(
                    !b.windows(plain.len()).any(|w| w == plain.as_bytes()),
                    "{} holds a plaintext value",
                    e.path().display()
                );
            }
        }
    }

    #[tokio::test]
    async fn rotation_grace_lineage_rollback_and_shred() {
        let (v, d) = vault().await;
        let k1 = "sk-or-v1-first-value-0123456789";
        let k2 = "sk-or-v1-second-value-9876543210";
        let m = v
            .put(
                "openrouter",
                "OPENROUTER_API_KEY",
                "",
                SecretString::new(k1),
            )
            .await
            .unwrap();
        assert_eq!(m.version, 1);
        let fp1 = m.fingerprint.clone();
        let opts = RotateOpts {
            grace_ms: 3_600_000,
            origin: "owner".into(),
            remote_id: "h-2".into(),
        };
        let m2 = v
            .rotate_with(&m.id, SecretString::new(k2), &opts)
            .await
            .unwrap();
        assert_eq!((m2.version, m2.remote_id.as_str()), (2, "h-2"));
        let lineage = v.versions(&m.id).await.unwrap();
        assert_eq!(lineage.len(), 2);
        assert_eq!(lineage[1].fingerprint, fp1);
        assert!(lineage[1].sealed && lineage[1].grace_until.is_some());
        assert_eq!(lineage[0].fingerprint, fingerprint(k2));
        // Neither value is on disk in the clear, nor in the lineage's Debug.
        on_disk_lacks(d.path(), k1);
        on_disk_lacks(d.path(), k2);
        assert!(!format!("{lineage:?}").contains(k1));
        // The retired value opens only inside its grace.
        assert_eq!(
            v.retired_value(&m.id, 1).await.unwrap().unwrap().expose(),
            k1
        );
        // Rollback restores it as version 3.
        let m3 = v.rollback(&m.id, "owner").await.unwrap();
        assert_eq!((m3.version, m3.fingerprint.as_str()), (3, fp1.as_str()));
        assert_eq!(v.get(&m.id).await.unwrap().unwrap().expose(), k1);
        assert!(
            v.versions(&m.id).await.unwrap()[0]
                .origin
                .starts_with("rollback:v1")
        );
        // Past every grace the sweep shreds the sealed copies; the lineage stays.
        let shred = v
            .sweep_grace(kls_store::now_ms() + 4_000_000)
            .await
            .unwrap();
        assert_eq!(shred.len(), 2, "v1 and v2 were both in grace");
        assert!(v.versions(&m.id).await.unwrap().iter().all(|x| !x.sealed));
        assert!(v.rollback(&m.id, "owner").await.is_err());
        assert!(v.retired_value(&m.id, 1).await.unwrap().is_none());
        // A put of the same pair is a rotation with lineage, no grace.
        let m4 = v
            .put(
                "openrouter",
                "OPENROUTER_API_KEY",
                "",
                SecretString::new(k2),
            )
            .await
            .unwrap();
        assert_eq!(m4.version, 4);
        assert!(!v.versions(&m.id).await.unwrap()[1].sealed);
    }

    #[tokio::test]
    async fn grants_skip_expired_and_other_nodes_keys() {
        let (v, _d) = vault().await;
        let a = v
            .put("openai", "OPENAI_API_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        let b = v
            .put("anthropic", "ANTHROPIC_API_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        v.store()
            .vault_set_scope(&a.id, Some(1), &[], "provider")
            .await
            .unwrap();
        v.store()
            .vault_set_scope(&b.id, None, &["n1".into()], "provider")
            .await
            .unwrap();
        let want = ["openai".to_string(), "anthropic".to_string()];
        let g = v
            .grant_scoped(&want, 60_000, "t", Some("n2"))
            .await
            .unwrap();
        assert!(g.secrets.is_empty());
        assert_eq!(g.denied.len(), 2);
        assert!(g.denied.iter().any(|d| d.contains("expired")));
        let g = v
            .grant_scoped(&want, 60_000, "t", Some("n1"))
            .await
            .unwrap();
        assert_eq!(g.secrets.len(), 1);
        assert_eq!(g.secrets[0].provider, "anthropic");
        assert!(
            reserved_env("PATH")
                && reserved_env("BELAI_TOKEN")
                && !reserved_env("GITHUB_MCP_TOKEN")
        );
    }

    #[tokio::test]
    async fn verify_checks_fingerprint() {
        let (v, _d) = vault().await;
        let m = v
            .put("openai", "OPENAI_API_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        assert_eq!(m.fingerprint, fingerprint(KEY));
        assert!(!m.fingerprint.contains(KEY));
        assert_eq!(v.verify(&m.id).await.unwrap().as_deref(), Some(""));
        let after = v.store().vault_secret_meta(&m.id).await.unwrap().unwrap();
        assert!(after.verified_at.is_some());
        // A tampered fingerprint fails the check and is recorded.
        v.store()
            .vault_set_fingerprint(&m.id, "0000")
            .await
            .unwrap();
        let err = v.verify(&m.id).await.unwrap().unwrap();
        assert!(err.contains("fingerprint"));
        let after = v.store().vault_secret_meta(&m.id).await.unwrap().unwrap();
        assert_eq!(after.verify_error, err);
        // A value that does not open fails too.
        v.store()
            .vault_reseal(&m.id, &[0; 24], b"junk")
            .await
            .unwrap();
        assert!(v.verify(&m.id).await.unwrap().unwrap().contains("open"));
        assert_eq!(v.verify("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn validation() {
        let (v, _d) = vault().await;
        let s = || SecretString::new("x");
        assert!(v.put("Bad Slug", "A", "", s()).await.is_err());
        assert!(v.put("ok", "lower", "", s()).await.is_err());
        assert!(v.put("ok", "1ABC", "", s()).await.is_err());
        assert!(
            v.put("ok", "OK", "", SecretString::new("   "))
                .await
                .is_err()
        );
        assert!(
            v.put("ok", "OK", "", SecretString::new("a\u{7}b"))
                .await
                .is_err()
        );
        assert!(
            v.put(
                "ok",
                "OK",
                "",
                SecretString::new("x".repeat(MAX_VALUE_LEN + 1))
            )
            .await
            .is_err()
        );
        assert!(v.grant(&["NOPE!".into()], 1000, "t").await.is_err());
    }

    #[tokio::test]
    async fn grants_and_audit() {
        let (v, _d) = vault().await;
        v.put("anthropic", "ANTHROPIC_API_KEY", "", SecretString::new(KEY))
            .await
            .unwrap();
        let g = v
            .grant(
                &["anthropic".into(), "openai".into(), "anthropic".into()],
                60_000,
                "job",
            )
            .await
            .unwrap();
        assert!(!g.grant_id.is_empty());
        assert_eq!(g.secrets.len(), 1);
        assert_eq!(g.secrets[0].env, "ANTHROPIC_API_KEY");
        assert_eq!(g.secrets[0].value.expose(), KEY);
        assert_eq!(g.missing, vec!["openai".to_string()]);
        assert!(g.expires_at_ms > kls_store::now_ms());

        // No value in Debug output, anywhere.
        let dbg = format!("{g:?} {:?} {v:?}", g.secrets[0]);
        assert!(!dbg.contains(KEY), "{dbg}");
        assert!(dbg.contains("<redacted>"));

        // The audit row names providers and ids only.
        let log = v.grants(10).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].providers, vec!["anthropic", "openai"]);
        assert_eq!(log[0].purpose, "job");
        assert!(!format!("{log:?}").contains(KEY));
        assert!(v.list().await.unwrap()[0].last_used_at.is_some());

        assert!(v.grant_live(&g.grant_id).await.unwrap());
        assert!(v.revoke_grant(&g.grant_id).await.unwrap());
        assert!(!v.revoke_grant(&g.grant_id).await.unwrap());
        assert!(!v.grant_live(&g.grant_id).await.unwrap());

        // Nothing held: an empty, unrecorded grant.
        let none = v.grant(&["openai".into()], 1, "job").await.unwrap();
        assert!(none.grant_id.is_empty() && none.secrets.is_empty());
        assert_eq!(v.grants(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn key_file_persists_and_wrong_key_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let kf = dir.path().join("vault.key");
        let id = {
            let v = Vault::open(
                Store::open_sqlite(&db).await.unwrap(),
                KeyBackend::File(kf.clone()),
            )
            .await
            .unwrap();
            v.put("openai", "OPENAI_API_KEY", "", SecretString::new("sk-1"))
                .await
                .unwrap()
                .id
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&kf).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let v = Vault::open(
            Store::open_sqlite(&db).await.unwrap(),
            KeyBackend::File(kf.clone()),
        )
        .await
        .unwrap();
        assert_eq!(v.get(&id).await.unwrap().unwrap().expose(), "sk-1");
        drop(v);

        // A different key must not silently open (or overwrite) the vault.
        let other = dir.path().join("other.key");
        let err = Vault::open(
            Store::open_sqlite(&db).await.unwrap(),
            KeyBackend::File(other),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("does not match"));
    }

    #[tokio::test]
    async fn keyring_fallback_uses_file_and_warns() {
        // The mock keyring never persists between entries, which is exactly a
        // keyring that cannot keep the key. Never touches the real keyring.
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
        let kf = dir.path().join("vault.key");
        let v = Vault::open(
            store,
            KeyBackend::Keyring {
                account: "test-server".into(),
                fallback_file: kf.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(v.key_source(), KeySource::File);
        assert!(v.warning().unwrap().contains("vault.key"));
        assert!(kf.exists());
        let key_text = std::fs::read_to_string(&kf).unwrap();
        assert!(!v.warning().unwrap().contains(key_text.trim()));
    }

    #[test]
    fn secret_string_debug() {
        let s = SecretString::new(KEY);
        assert_eq!(format!("{s:?}"), "SecretString(<redacted>)");
        assert_eq!(s.len(), KEY.len());
    }
}
