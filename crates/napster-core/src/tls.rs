//! TLS to the server, with trust-on-first-use pinning.
//!
//! A certificate that chains to a public CA (e.g. Let's Encrypt) and
//! matches the host name is accepted as usual. Otherwise, as with
//! self-signed certificates that `napd` generates, the certificate's
//! SHA-256 fingerprint is remembered on first connect (like SSH's
//! known_hosts) and any later change is refused as a possible
//! impersonation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub fn fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// `host:port` -> pinned certificate fingerprint.
#[derive(Debug, Default)]
pub struct KnownServers {
    path: PathBuf,
    pub pins: BTreeMap<String, String>,
}

impl KnownServers {
    pub fn default_path() -> PathBuf {
        crate::ClientConfig::default_path().with_file_name("known_servers.json")
    }

    pub fn load(path: &Path) -> Self {
        let pins = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path: path.to_owned(), pins }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.pins)?)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Default)]
struct Seen {
    fingerprint: Option<String>,
    ca_valid: bool,
    mismatch: bool,
}

#[derive(Debug)]
struct PinningVerifier {
    webpki: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    pinned: Option<String>,
    seen: Arc<Mutex<Seen>>,
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        let ca_valid = self.webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now).is_ok();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.fingerprint = Some(fp.clone());
        seen.ca_valid = ca_valid;
        if ca_valid {
            return Ok(ServerCertVerified::assertion());
        }
        match &self.pinned {
            Some(p) if *p != fp => {
                seen.mismatch = true;
                Err(rustls::Error::General("server certificate changed".into()))
            }
            // pinned and matching, or first contact (trust on first use)
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub struct Connected {
    pub stream: TlsStream<TcpStream>,
    /// Set when this was the first connection and the certificate was
    /// pinned just now.
    pub newly_pinned: Option<String>,
}

/// Run the TLS handshake over `tcp` to `host:port`, applying and updating
/// the pins in `known`.
pub async fn connect(tcp: TcpStream, host: &str, port: u16, known: &mut KnownServers) -> anyhow::Result<Connected> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build()?;
    let key = format!("{host}:{port}");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let verifier = PinningVerifier { webpki, provider: provider.clone(), pinned: known.pins.get(&key).cloned(), seen: seen.clone() };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|_| anyhow!("invalid server name {host}"))?;
    let result = TlsConnector::from(Arc::new(config)).connect(name, tcp).await;
    let seen = std::mem::take(&mut *seen.lock().unwrap_or_else(|e| e.into_inner()));
    let stream = match result {
        Ok(s) => s,
        Err(e) if seen.mismatch => {
            return Err(anyhow!(
                "SECURITY WARNING: {key}'s certificate has changed since you last connected \
                 (expected {}, got {}). Someone may be impersonating the server. If its operator \
                 really replaced the certificate, remove \"{key}\" from {} and reconnect. ({e})",
                known.pins.get(&key).map(String::as_str).unwrap_or("?"),
                seen.fingerprint.as_deref().unwrap_or("?"),
                known.path().display()
            ))
        }
        Err(e) => return Err(e).context("TLS handshake with the server failed"),
    };
    let mut newly_pinned = None;
    if !seen.ca_valid && !known.pins.contains_key(&key) {
        if let Some(fp) = seen.fingerprint {
            known.pins.insert(key, fp.clone());
            known.save()?;
            newly_pinned = Some(fp);
        }
    }
    Ok(Connected { stream, newly_pinned })
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprint_format() {
        let fp = super::fingerprint(b"abc");
        assert_eq!(fp.len(), 95);
        assert!(fp.starts_with("BA:78:16:BF"));
    }
}
