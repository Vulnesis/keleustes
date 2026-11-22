//! Signed audit exports.
//!
//! An export is the JSONL of the ledger entries (oldest first) and the chain
//! head line, then one `signature` line: Ed25519 over the SHA-256 of every
//! byte before it. The signing key is made on first use and kept sealed in
//! the vault (`Vault::put_blob`); the public key is in the signature line,
//! and [`Core::audit_public_key`] returns it for out-of-band pinning.
//! [`verify_export`] checks a file without the vault.

use crate::Core;
use anyhow::{Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use kls_store::audit::{LedgerQuery, canonical_json};
use kls_store::now_ms;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const BLOB: &str = "audit_signing_key";

async fn signing_key(core: &Core) -> Result<SigningKey> {
    let v = crate::vault::vault(core).await?;
    if let Some(seed) = v.get_blob(BLOB).await? {
        let arr: [u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("the audit signing key is corrupt"))?;
        return Ok(SigningKey::from_bytes(&arr));
    }
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut seed);
    v.put_blob(BLOB, &seed).await?;
    let k = SigningKey::from_bytes(&seed);
    seed.iter_mut().for_each(|b| *b = 0);
    Ok(k)
}

/// What a verified export says.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedExport {
    pub entries: i64,
    pub public_key: String,
    pub signed_at: i64,
}

/// Checks the signature line of an export against the bytes before it.
pub fn verify_export(text: &str) -> Result<VerifiedExport> {
    let body_end = text
        .trim_end_matches('\n')
        .rfind('\n')
        .map(|i| i + 1)
        .ok_or_else(|| anyhow!("no signature line"))?;
    let (body, last) = text.split_at(body_end);
    let sig: Value = serde_json::from_str(last.trim())?;
    if sig["type"] != "signature" {
        bail!("the last line is not a signature");
    }
    let digest = Sha256::digest(body.as_bytes());
    if sig["sha256"].as_str() != Some(hex::encode(digest).as_str()) {
        bail!("the export was changed after it was signed");
    }
    let pk: [u8; 32] = hex::decode(sig["publicKey"].as_str().unwrap_or(""))?
        .try_into()
        .map_err(|_| anyhow!("bad public key"))?;
    let s: [u8; 64] = hex::decode(sig["signature"].as_str().unwrap_or(""))?
        .try_into()
        .map_err(|_| anyhow!("bad signature"))?;
    VerifyingKey::from_bytes(&pk)?
        .verify(&digest, &Signature::from_bytes(&s))
        .map_err(|_| anyhow!("the signature does not match"))?;
    Ok(VerifiedExport {
        entries: sig["entries"].as_i64().unwrap_or(0),
        public_key: hex::encode(pk),
        signed_at: sig["signedAt"].as_i64().unwrap_or(0),
    })
}

impl Core {
    /// The public half of the audit signing key, hex.
    pub async fn audit_public_key(&self) -> Result<String> {
        Ok(hex::encode(
            signing_key(self).await?.verifying_key().to_bytes(),
        ))
    }

    /// The export ([`Core::audit_export_jsonl`]) with a signature line.
    pub async fn audit_export_signed(&self, q: &LedgerQuery) -> Result<(Vec<u8>, i64)> {
        let mut buf: Vec<u8> = Vec::new();
        let n = self.audit_export_jsonl(q, &mut buf).await?;
        let key = signing_key(self).await?;
        let digest = Sha256::digest(&buf);
        let sig = key.sign(&digest);
        let line = json!({
            "type": "signature", "alg": "ed25519", "sha256": hex::encode(digest),
            "publicKey": hex::encode(key.verifying_key().to_bytes()),
            "signature": hex::encode(sig.to_bytes()), "entries": n, "signedAt": now_ms(),
            "server": self.server_id().await?, "fromAt": q.from_at, "toAt": q.to_at,
        });
        buf.extend_from_slice(canonical_json(&line).as_bytes());
        buf.push(b'\n');
        Ok((buf, n))
    }

    /// Writes a signed export to `path` (a file the person chose) and emits
    /// `audit.exported` (audited). Returns the entries written.
    pub async fn audit_export_to(&self, q: &LedgerQuery, path: &std::path::Path) -> Result<i64> {
        let (buf, n) = self.audit_export_signed(q).await?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        tokio::fs::write(path, &buf).await?;
        self.emit(
            super::kinds::AUDIT_EXPORTED,
            &path.display().to_string(),
            &json!({"entries": n, "fromAt": q.from_at, "sha256": hex::encode(Sha256::digest(&buf)), "by": "owner"}),
        )
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn export_is_signed_and_tamper_evident() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), None).await.unwrap();
        crate::vault::install_vault(
            &c,
            crate::vault::KeyBackend::File(dir.path().join("vault.key")),
        )
        .await
        .unwrap();
        c.emit("vault.secret_saved", "k", &json!({"provider": "x"}))
            .await
            .unwrap();
        c.audit_sync().await.unwrap();
        let (buf, n) = c
            .audit_export_signed(&LedgerQuery::default())
            .await
            .unwrap();
        assert!(n >= 1);
        let text = String::from_utf8(buf).unwrap();
        let v = verify_export(&text).unwrap();
        assert_eq!(v.entries, n);
        assert_eq!(
            v.public_key,
            c.audit_public_key().await.unwrap(),
            "the key is kept"
        );
        let bad = text.replacen("vault.secret_saved", "vault.secret_sav3d", 1);
        assert!(verify_export(&bad).is_err());
        let path = dir.path().join("out/a.jsonl");
        c.audit_export_to(&LedgerQuery::default(), &path)
            .await
            .unwrap();
        assert!(verify_export(&std::fs::read_to_string(&path).unwrap()).is_ok());
    }
}
