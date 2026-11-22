//! Local certificate authority.
//!
//! Keleustes generates its own CA on first run. The CA signs the server
//! certificate (DNS SAN [`TLS_SERVER_NAME`], so nodes verify one fixed name
//! whatever address they dial) and one client certificate per joined node
//! (CN = node id, ClientAuth EKU). Nodes first pin the server SPKI from
//! their join token, then trust the CA certificate returned by Join.

use anyhow::{Context, Result, anyhow};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType, SerialNumber,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use time::{Duration, OffsetDateTime};

pub const TLS_SERVER_NAME: &str = "keleustes.node";

pub struct Pki {
    issuer: Issuer<'static, KeyPair>,
    ca_cert_pem: String,
    ca_cert_der: Vec<u8>,
    server_cert_pem: String,
    server_key_pem: String,
    server_spki_sha256: [u8; 32],
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)?;
        Ok(())
    }
}

fn random_serial() -> SerialNumber {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[0] = (b[0] & 0x3f) | 0x40; // positive, minimal DER INTEGER (no leading zero)
    SerialNumber::from_slice(&b)
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let p = ::pem::parse(pem).context("parse pem")?;
    Ok(p.contents().to_vec())
}

fn spki_sha256(cert_der: &[u8]) -> Result<[u8; 32]> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| anyhow!("parse certificate: {e}"))?;
    Ok(Sha256::digest(cert.tbs_certificate.subject_pki.raw).into())
}

impl Pki {
    /// Loads the CA and server certificate from `dir`, creating them on first run.
    pub fn load_or_create(dir: &Path, server_id: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let paths = Paths::new(dir);
        if !paths.ca_key.exists() {
            Self::create(&paths, server_id)?;
        }
        Self::load(&paths)
    }

    fn create(p: &Paths, server_id: &str) -> Result<()> {
        let now = OffsetDateTime::now_utc();
        let ca_key = KeyPair::generate()?;
        let mut ca = CertificateParams::default();
        let short: String = server_id.chars().take(8).collect();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, format!("Keleustes CA {short}"));
        dn.push(DnType::OrganizationName, "Keleustes");
        ca.distinguished_name = dn;
        ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca.not_before = now - Duration::days(1);
        ca.not_after = now + Duration::days(365 * 20);
        ca.serial_number = Some(random_serial());
        let ca_cert = ca.self_signed(&ca_key)?;
        let issuer = Issuer::new(ca, ca_key);

        let server_key = KeyPair::generate()?;
        let server_cert = server_params(now)?.signed_by(&server_key, &issuer)?;

        write_private(&p.ca_key, &issuer_key_pem(&issuer))?;
        std::fs::write(&p.ca_cert, ca_cert.pem())?;
        write_private(&p.server_key, &server_key.serialize_pem())?;
        std::fs::write(&p.server_cert, server_cert.pem())?;
        Ok(())
    }

    fn load(p: &Paths) -> Result<Self> {
        let ca_key = KeyPair::from_pem(&std::fs::read_to_string(&p.ca_key)?)?;
        let ca_cert_pem = std::fs::read_to_string(&p.ca_cert)?;
        let issuer = Issuer::from_ca_cert_pem(&ca_cert_pem, ca_key)?;
        let server_cert_pem = std::fs::read_to_string(&p.server_cert)?;
        let server_key_pem = std::fs::read_to_string(&p.server_key)?;
        let server_spki_sha256 = spki_sha256(&pem_to_der(&server_cert_pem)?)?;
        Ok(Self {
            issuer,
            ca_cert_der: pem_to_der(&ca_cert_pem)?,
            ca_cert_pem,
            server_cert_pem,
            server_key_pem,
            server_spki_sha256,
        })
    }

    pub fn ca_cert_pem(&self) -> &str {
        &self.ca_cert_pem
    }
    pub fn ca_cert_der(&self) -> &[u8] {
        &self.ca_cert_der
    }
    pub fn server_cert_pem(&self) -> &str {
        &self.server_cert_pem
    }
    pub fn server_key_pem(&self) -> &str {
        &self.server_key_pem
    }
    pub fn server_spki_sha256(&self) -> [u8; 32] {
        self.server_spki_sha256
    }

    /// Verifies a node's CSR and issues its client certificate. The CSR's
    /// subject and extensions are ignored; only its public key is used.
    /// Returns (certificate DER, serial hex).
    pub fn sign_node(&self, csr_der: &[u8], node_id: &str) -> Result<(Vec<u8>, String)> {
        let csr = CertificateSigningRequestParams::from_der(&csr_der.to_vec().into())
            .map_err(|e| anyhow!("invalid certificate signing request: {e}"))?;
        let now = OffsetDateTime::now_utc();
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, node_id);
        dn.push(DnType::OrganizationName, "Keleustes node");
        params.distinguished_name = dn;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.not_before = now - Duration::hours(1);
        params.not_after = now + Duration::days(365 * 5);
        let serial = random_serial();
        let serial_hex = hex::encode(serial.to_bytes());
        params.serial_number = Some(serial);
        let cert = params.signed_by(&csr.public_key, &self.issuer)?;
        Ok((cert.der().to_vec(), serial_hex))
    }
}

fn server_params(now: OffsetDateTime) -> Result<CertificateParams> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, TLS_SERVER_NAME);
    params.distinguished_name = dn;
    params.subject_alt_names = vec![SanType::DnsName(TLS_SERVER_NAME.try_into()?)];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(365 * 10);
    params.serial_number = Some(random_serial());
    Ok(params)
}

fn issuer_key_pem(issuer: &Issuer<'static, KeyPair>) -> String {
    issuer.key().serialize_pem()
}

struct Paths {
    ca_key: PathBuf,
    ca_cert: PathBuf,
    server_key: PathBuf,
    server_cert: PathBuf,
}

impl Paths {
    fn new(dir: &Path) -> Self {
        Self {
            ca_key: dir.join("ca.key.pem"),
            ca_cert: dir.join("ca.crt.pem"),
            server_key: dir.join("server.key.pem"),
            server_cert: dir.join("server.crt.pem"),
        }
    }
}

/// Reads the CN of a client certificate (the node id).
pub fn client_cert_identity(cert_der: &[u8]) -> Result<(String, String)> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| anyhow!("parse client certificate: {e}"))?;
    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|a| a.as_str().ok())
        .ok_or_else(|| anyhow!("client certificate has no common name"))?
        .to_string();
    let serial = hex::encode(cert.tbs_certificate.raw_serial());
    Ok((cn, serial))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_reload_and_sign() {
        let dir = tempfile::tempdir().unwrap();
        let a = Pki::load_or_create(dir.path(), "abcdef0123").unwrap();
        let b = Pki::load_or_create(dir.path(), "abcdef0123").unwrap();
        assert_eq!(
            a.server_spki_sha256(),
            b.server_spki_sha256(),
            "reload keeps identity"
        );

        let node_key = KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let mut req = CertificateParams::default();
        req.distinguished_name.push(DnType::CommonName, "ignored");
        let csr = req.serialize_request(&node_key).unwrap();
        let (der, serial) = b.sign_node(csr.der(), "node-1").unwrap();
        let (cn, s2) = client_cert_identity(&der).unwrap();
        assert_eq!(cn, "node-1");
        assert_eq!(serial, s2);

        assert!(b.sign_node(b"not a csr", "x").is_err());
    }
}
