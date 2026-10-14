//! Release signing for napster-rs auto-update.
//!
//! ```text
//! napster-sign keygen <secret-key-file>   write a new secret key, print the public key
//! napster-sign sign <file>...             write <file>.sig for each file
//!                                         (secret key from $NAPSTER_SIGNING_KEY)
//! napster-sign verify <public-key> <file> check <file>.sig
//! ```
//!
//! Keys and signatures are base64 Ed25519. Clients carry the public key
//! (`napster_core::update::RELEASE_PUBLIC_KEY`) and refuse any update
//! whose signature doesn't verify.

use std::path::Path;

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let out = args.get(1).context("usage: napster-sign keygen <secret-key-file>")?;
            if Path::new(out).exists() {
                bail!("{out} already exists; refusing to overwrite a signing key");
            }
            let mut seed = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut seed);
            let key = SigningKey::from_bytes(&seed);
            if let Some(dir) = Path::new(out).parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(out, B64.encode(seed))?;
            println!("secret key written to {out} (keep it private and backed up)");
            println!("public key: {}", B64.encode(key.verifying_key().to_bytes()));
        }
        Some("sign") => {
            let secret = std::env::var("NAPSTER_SIGNING_KEY").context("NAPSTER_SIGNING_KEY is not set")?;
            let seed: [u8; 32] = B64
                .decode(secret.trim())
                .context("NAPSTER_SIGNING_KEY is not base64")?
                .try_into()
                .map_err(|_| anyhow::anyhow!("NAPSTER_SIGNING_KEY must be 32 bytes"))?;
            let key = SigningKey::from_bytes(&seed);
            println!("signing with public key {}", B64.encode(key.verifying_key().to_bytes()));
            if args.len() < 2 {
                bail!("usage: napster-sign sign <file>...");
            }
            for file in &args[1..] {
                let data = std::fs::read(file).with_context(|| format!("reading {file}"))?;
                let sig = key.sign(&data);
                std::fs::write(format!("{file}.sig"), B64.encode(sig.to_bytes()))?;
                println!("signed {file}");
            }
        }
        Some("verify") => {
            let (Some(public), Some(file)) = (args.get(1), args.get(2)) else {
                bail!("usage: napster-sign verify <public-key> <file>");
            };
            let pk: [u8; 32] = B64.decode(public)?.try_into().map_err(|_| anyhow::anyhow!("bad public key"))?;
            let pk = VerifyingKey::from_bytes(&pk)?;
            let sig: [u8; 64] = B64
                .decode(std::fs::read_to_string(format!("{file}.sig"))?.trim())?
                .try_into()
                .map_err(|_| anyhow::anyhow!("bad signature"))?;
            pk.verify(&std::fs::read(file)?, &ed25519_dalek::Signature::from_bytes(&sig))
                .context("SIGNATURE DOES NOT MATCH")?;
            println!("{file}: signature OK");
        }
        _ => bail!("usage: napster-sign keygen <file> | sign <file>... | verify <public-key> <file>"),
    }
    Ok(())
}
