//! Join tokens: `kls1.<base64url(json)>`.
//!
//! A token tells a belai host everything it needs to join exactly once:
//! where to dial, which server key to pin, and a single-use secret. Only
//! the secret's SHA-256 is stored. The payload is JSON with short keys so
//! belai can decode it with the standard library.

use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "kls1.";
pub const DEFAULT_TTL_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Payload {
    /// Format version (1).
    pub v: u32,
    /// Server id and display name.
    pub sid: String,
    pub sn: String,
    /// Addresses to try, in order, as `host:port`.
    pub a: Vec<String>,
    /// SHA-256 of the server certificate's SubjectPublicKeyInfo (base64url).
    pub pin: String,
    /// Token id and secret (base64url).
    pub t: String,
    pub s: String,
    /// Expiry, Unix ms.
    pub e: i64,
}

impl Payload {
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("payload serializes");
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    pub fn decode(token: &str) -> Result<Self> {
        let Some(body) = token.trim().strip_prefix(PREFIX) else {
            bail!("not a Keleustes join token");
        };
        let p: Payload = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body)?)?;
        if p.v != 1 {
            bail!("unsupported join token version {}", p.v);
        }
        Ok(p)
    }
}

/// A freshly minted token: the text to hand to the node and what to store.
pub struct Minted {
    pub token: String,
    pub id: String,
    pub secret_sha256: String,
    pub expires_at: i64,
}

pub fn secret_hash(secret: &[u8]) -> String {
    hex::encode(Sha256::digest(secret))
}

pub fn mint(
    server_id: &str,
    server_name: &str,
    addrs: Vec<String>,
    spki_sha256: [u8; 32],
    expires_at: i64,
) -> Minted {
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    let id = uuid::Uuid::now_v7().to_string();
    let payload = Payload {
        v: 1,
        sid: server_id.into(),
        sn: server_name.into(),
        a: addrs,
        pin: URL_SAFE_NO_PAD.encode(spki_sha256),
        t: id.clone(),
        s: URL_SAFE_NO_PAD.encode(secret),
        e: expires_at,
    };
    Minted {
        token: payload.encode(),
        id,
        secret_sha256: secret_hash(&secret),
        expires_at,
    }
}

/// Addresses a node on the LAN can dial: configured ones if any, otherwise
/// every non-loopback interface address, then loopback last.
pub fn advertise_addrs(configured: Option<&str>, listen: &str) -> Vec<String> {
    if let Some(list) = configured {
        let v: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    let port = listen.rsplit(':').next().unwrap_or("7443");
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
    let bind_any = matches!(host, "0.0.0.0" | "[::]" | "::" | "");
    if !bind_any {
        return vec![listen.to_string()];
    }
    let mut out = Vec::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            let ip = i.ip();
            if ip.is_loopback() {
                continue;
            }
            match ip {
                std::net::IpAddr::V4(v4) => {
                    if v4.is_link_local() {
                        continue;
                    }
                    out.push(format!("{v4}:{port}"));
                }
                std::net::IpAddr::V6(v6) => {
                    // Skip link-local (fe80::/10): needs a zone to dial.
                    if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                        continue;
                    }
                    out.push(format!("[{v6}]:{port}"));
                }
            }
        }
    }
    out.push(format!("127.0.0.1:{port}"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let m = mint("sid", "Desk", vec!["10.0.0.2:7443".into()], [7u8; 32], 99);
        let p = Payload::decode(&m.token).unwrap();
        assert_eq!(p.t, m.id);
        assert_eq!(p.a, vec!["10.0.0.2:7443"]);
        let secret = URL_SAFE_NO_PAD.decode(&p.s).unwrap();
        assert_eq!(secret_hash(&secret), m.secret_sha256);
        assert!(Payload::decode("nope").is_err());
        assert!(Payload::decode("kls1.!!!").is_err());
    }

    #[test]
    fn addrs() {
        assert_eq!(
            advertise_addrs(Some("a:1, b:2"), "0.0.0.0:7443"),
            vec!["a:1", "b:2"]
        );
        assert_eq!(
            advertise_addrs(None, "192.168.1.5:9000"),
            vec!["192.168.1.5:9000"]
        );
        let any = advertise_addrs(None, "0.0.0.0:7443");
        assert_eq!(any.last().unwrap(), "127.0.0.1:7443");
    }
}
