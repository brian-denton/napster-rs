//! TLS for the server's encrypted port.
//!
//! Operators can supply a certificate (`--tls-cert`/`--tls-key`, e.g. from
//! Let's Encrypt). Without one, a self-signed certificate is generated once
//! and kept in the data directory; napster-rs clients pin it on first
//! connect, so its fingerprint (printed at startup) is what users verify.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

pub struct TlsSetup {
    pub config: Arc<rustls::ServerConfig>,
    /// SHA-256 of the leaf certificate, `AB:CD:...`
    pub fingerprint: String,
}

pub fn fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn load_pem(cert: &Path, key: &Path) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(
        std::fs::File::open(cert).with_context(|| format!("opening {}", cert.display()))?,
    ))
    .collect::<Result<Vec<_>, _>>()
    .with_context(|| format!("parsing {}", cert.display()))?;
    if certs.is_empty() {
        bail!("{} contains no certificates", cert.display());
    }
    let key = rustls_pemfile::private_key(&mut BufReader::new(
        std::fs::File::open(key).with_context(|| format!("opening {}", key.display()))?,
    ))?
    .with_context(|| format!("{} contains no private key", key.display()))?;
    Ok((certs, key))
}

fn generate(hosts: Vec<String>) -> anyhow::Result<(String, String)> {
    let c = rcgen::generate_simple_self_signed(hosts)?;
    Ok((c.cert.pem(), c.key_pair.serialize_pem()))
}

/// Load the configured certificate, or create/reuse a self-signed one in
/// `data_dir` (in memory if there is none).
pub fn setup(
    cert: Option<&Path>,
    key: Option<&Path>,
    data_dir: Option<&Path>,
    public_host: Option<&str>,
) -> anyhow::Result<TlsSetup> {
    let (certs, key) = match (cert, key) {
        (Some(c), Some(k)) => load_pem(c, k)?,
        (Some(_), None) | (None, Some(_)) => bail!("--tls-cert and --tls-key must be given together"),
        (None, None) => {
            let mut hosts = vec!["localhost".to_owned()];
            hosts.extend(public_host.map(String::from));
            match data_dir {
                Some(dir) => {
                    let (c, k): (PathBuf, PathBuf) = (dir.join("tls-cert.pem"), dir.join("tls-key.pem"));
                    if !c.exists() || !k.exists() {
                        let (cp, kp) = generate(hosts)?;
                        std::fs::write(&c, cp)?;
                        std::fs::write(&k, kp)?;
                        restrict(&k);
                        tracing::info!("generated a self-signed TLS certificate in {}", dir.display());
                    }
                    load_pem(&c, &k)?
                }
                None => {
                    let dir = tempfile_dir()?;
                    let (cp, kp) = generate(hosts)?;
                    let (c, k) = (dir.join("c.pem"), dir.join("k.pem"));
                    std::fs::write(&c, cp)?;
                    std::fs::write(&k, kp)?;
                    let r = load_pem(&c, &k);
                    let _ = std::fs::remove_dir_all(&dir);
                    r?
                }
            }
        }
    };
    let fingerprint = fingerprint(&certs[0]);
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsSetup { config: Arc::new(config), fingerprint })
}

fn tempfile_dir() -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("napd-tls-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Make the private key readable by its owner only (Unix).
fn restrict(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let a = setup(None, None, Some(dir.path()), Some("example.org")).unwrap();
        let b = setup(None, None, Some(dir.path()), None).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 32 * 3 - 1);
        let mem = setup(None, None, None, None).unwrap();
        assert_ne!(mem.fingerprint, a.fingerprint);
    }
}
