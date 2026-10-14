//! Self-update from GitHub Releases.
//!
//! The release workflow signs every package with an Ed25519 key that only
//! GitHub Actions holds (`NAPSTER_SIGNING_KEY`) and uploads `<package>.sig`
//! next to it. The client:
//!
//! 1. asks the GitHub API for the latest release,
//! 2. downloads this platform's package and its `.sig` over HTTPS,
//! 3. refuses to go further unless the signature verifies against
//!    [`RELEASE_PUBLIC_KEY`],
//! 4. unpacks next to the running program and swaps itself in (the whole
//!    `Napster.app` on macOS),
//! 5. leaves restarting to the user.
//!
//! Unlike the original client's auto-upgrade (message 5, which downloaded
//! and ran an `upgrade.exe` from wherever the server pointed), nothing is
//! installed without the user's say-so and a valid signature. Message 5
//! is still ignored.
//!
//! Everything here blocks; call it from a blocking thread.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

/// Public half of the release signing key.
pub const RELEASE_PUBLIC_KEY: &str = "aiFkY0TOpiOanimBRxp9THtriZPc5x7pZ3RsROOptfo=";

/// `owner/name` of the repository whose releases we follow. Can be
/// overridden at build time with `NAPSTER_UPDATE_REPO`.
pub const REPO: &str = match option_env!("NAPSTER_UPDATE_REPO") {
    Some(r) => r,
    None => "brian-denton/napster-rs",
};

const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// e.g. "0.4.0"
    pub version: String,
    pub notes: String,
    pub page_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sig_url: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "v1.2.3" / "1.2.3" -> (1, 2, 3)
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().unwrap_or(Some(0))?, it.next().unwrap_or(Some(0))?))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Which release package this build installs from.
pub fn platform_asset() -> Option<&'static str> {
    if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        Some("napster-rs-windows-x64.zip")
    } else if cfg!(target_os = "macos") {
        Some("napster-rs-macos-universal.zip")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("napster-rs-linux-x64.tar.gz")
    } else {
        None
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(&format!("napster-rs/{}", current_version()))
        .build()
}

/// Is there a newer signed release for this platform?
pub fn check() -> anyhow::Result<Option<UpdateInfo>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let release: Release = match agent().get(&url).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => r.into_json()?,
        // no releases, or the repository isn't public
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(anyhow!("checking for updates: {e}")),
    };
    choose(release)
}

fn choose(release: Release) -> anyhow::Result<Option<UpdateInfo>> {
    if !is_newer(&release.tag_name, current_version()) {
        return Ok(None);
    }
    let Some(name) = platform_asset() else { return Ok(None) };
    let find = |n: &str| release.assets.iter().find(|a| a.name == n).map(|a| a.browser_download_url.clone());
    let (Some(asset_url), Some(sig_url)) = (find(name), find(&format!("{name}.sig"))) else {
        // unsigned or missing package: never offer it
        tracing::info!("release {} has no signed package for this platform", release.tag_name);
        return Ok(None);
    };
    Ok(Some(UpdateInfo {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        notes: release.body.unwrap_or_default(),
        page_url: release.html_url,
        asset_name: name.to_owned(),
        asset_url,
        sig_url,
    }))
}

/// Verify a base64 Ed25519 signature over `data`.
pub fn verify(data: &[u8], sig_b64: &str, public_key_b64: &str) -> anyhow::Result<()> {
    let pk: [u8; 32] = B64
        .decode(public_key_b64.trim())?
        .try_into()
        .map_err(|_| anyhow!("bad public key length"))?;
    let sig: [u8; 64] =
        B64.decode(sig_b64.trim())?.try_into().map_err(|_| anyhow!("bad signature length"))?;
    VerifyingKey::from_bytes(&pk)?
        .verify(data, &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("the update's signature is not valid; refusing to install it"))
}

fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = agent().get(url).call().map_err(|e| anyhow!("downloading {url}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().take(MAX_DOWNLOAD + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_DOWNLOAD {
        bail!("update package is unexpectedly large");
    }
    Ok(buf)
}

/// Download, verify and install `info`. Returns what to launch to run
/// the new version (an executable, or a `.app` bundle on macOS).
pub fn install(info: &UpdateInfo, mut progress: impl FnMut(&str)) -> anyhow::Result<PathBuf> {
    progress(&format!("Downloading {}...", info.asset_name));
    let data = download(&info.asset_url)?;
    let sig = String::from_utf8(download(&info.sig_url)?).context("signature isn't text")?;
    progress("Verifying signature...");
    verify(&data, &sig, RELEASE_PUBLIC_KEY)?;
    progress("Installing...");
    install_package(&data, &info.asset_name, &this_exe()?)
}

/// Install a package already on disk (`<package>.sig` must sit next to
/// it), e.g. for computers without internet access.
pub fn install_local(package: &Path) -> anyhow::Result<PathBuf> {
    let data = std::fs::read(package).with_context(|| format!("reading {}", package.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", package.display()));
    let sig = std::fs::read_to_string(&sig_path).with_context(|| format!("reading {}", sig_path.display()))?;
    verify(&data, &sig, RELEASE_PUBLIC_KEY)?;
    let name = package.file_name().and_then(|n| n.to_str()).context("bad package name")?;
    install_package(&data, name, &this_exe()?)
}

/// The running program, with symlinks resolved (except on Windows, where
/// that turns paths into the `\\?\C:\...` verbatim form).
fn this_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if cfg!(windows) {
        Ok(exe)
    } else {
        exe.canonicalize()
    }
}

/// Unpack a verified package and swap it in for `exe` (and its siblings).
pub fn install_package(data: &[u8], asset_name: &str, exe: &Path) -> anyhow::Result<PathBuf> {
    let bundle = app_bundle(exe);
    // unpack beside what we replace so the final renames stay on one volume
    let base = bundle.as_deref().unwrap_or(exe).parent().context("program has no parent directory")?;
    let staging = base.join(".napster-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = (|| {
        unpack(data, asset_name, &staging)?;
        let root = staging.join("napster-rs");
        let root = if root.is_dir() { root } else { staging.clone() };
        match &bundle {
            Some(app) => replace_bundle(&root, app),
            None => replace_binaries(&root, exe),
        }
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn unpack(data: &[u8], name: &str, dest: &Path) -> anyhow::Result<()> {
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            // enclosed_name rejects absolute paths and `..`
            let Some(rel) = entry.enclosed_name() else { bail!("unsafe path in update package") };
            let out = dest.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777))?;
            }
        }
    } else if name.ends_with(".tar.gz") {
        // tar's unpack refuses paths that escape `dest`
        tar::Archive::new(flate2::read::GzDecoder::new(data)).unpack(dest)?;
    } else {
        bail!("unknown package format: {name}");
    }
    Ok(())
}

/// `/Applications/Napster.app` if `exe` lives inside a macOS app bundle.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && p.join("Contents").is_dir())
        .map(Path::to_path_buf)
}

fn replace_bundle(root: &Path, app: &Path) -> anyhow::Result<PathBuf> {
    let new_app = root.join("Napster.app");
    if !new_app.is_dir() {
        bail!("update package has no Napster.app");
    }
    let old = app.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(app, &old).context("moving the old Napster.app aside")?;
    if let Err(e) = std::fs::rename(&new_app, app) {
        let _ = std::fs::rename(&old, app);
        return Err(e).context("moving the new Napster.app into place");
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(app.to_path_buf())
}

const PROGRAMS: [&str; 3] = ["napster", "napster-cli", "napd"];

fn replace_binaries(root: &Path, exe: &Path) -> anyhow::Result<PathBuf> {
    let name = exe.file_name().context("program has no file name")?;
    let new_exe = root.join(name);
    if !new_exe.is_file() {
        bail!("update package doesn't contain {}", name.to_string_lossy());
    }
    // works while running, including on Windows
    self_replace::self_replace(&new_exe).context("replacing the running program")?;
    // also refresh the other programs if they live alongside this one
    let dir = exe.parent().context("program has no parent directory")?;
    for prog in PROGRAMS {
        let file = format!("{prog}{}", std::env::consts::EXE_SUFFIX);
        let (src, dst) = (root.join(&file), dir.join(&file));
        if dst == exe || !src.is_file() || !dst.is_file() {
            continue;
        }
        let tmp = dir.join(format!("{file}.new"));
        let res = std::fs::copy(&src, &tmp).and_then(|_| std::fs::rename(&tmp, &dst));
        if let Err(e) = res {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!("couldn't update {} ({e}); is it running?", dst.display());
        }
    }
    Ok(exe.to_path_buf())
}

/// Start the freshly installed version (the caller should then exit).
pub fn relaunch(target: &Path) -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if target.extension().is_some_and(|e| e == "app") {
        std::process::Command::new("open").arg("-n").arg(target).arg("--args").args(&args).spawn()?;
    } else {
        std::process::Command::new(target).args(&args).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.10.2"), Some((0, 10, 2)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
        assert!(is_newer("v0.4.0", "0.3.9"));
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("garbage", "0.3.0"));
    }

    #[test]
    fn signatures() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk = B64.encode(key.verifying_key().to_bytes());
        let data = b"package bytes";
        let sig = B64.encode(key.sign(data).to_bytes());
        verify(data, &sig, &pk).unwrap();
        assert!(verify(b"tampered", &sig, &pk).is_err());
        let other = B64.encode(SigningKey::from_bytes(&[8u8; 32]).verifying_key().to_bytes());
        assert!(verify(data, &sig, &other).is_err());
        assert!(verify(data, "not base64!", &pk).is_err());
    }

    #[test]
    fn public_key_is_valid() {
        let pk: [u8; 32] = B64.decode(RELEASE_PUBLIC_KEY).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&pk).unwrap();
    }

    #[test]
    fn only_signed_newer_releases_are_offered() {
        let Some(name) = platform_asset() else { return };
        let mk = |tag: &str, with_sig: bool| {
            let mut assets = vec![Asset { name: name.into(), browser_download_url: "https://x/pkg".into() }];
            if with_sig {
                assets.push(Asset { name: format!("{name}.sig"), browser_download_url: "https://x/sig".into() });
            }
            Release { tag_name: tag.into(), body: None, html_url: "https://x".into(), assets }
        };
        assert!(choose(mk("v999.0.0", true)).unwrap().is_some());
        assert!(choose(mk("v999.0.0", false)).unwrap().is_none());
        assert!(choose(mk("v0.0.1", true)).unwrap().is_none());
    }

    fn zip_package(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn unpack_rejects_escaping_paths() {
        let dir = tempfile::tempdir().unwrap();
        let evil = zip_package(&[("../evil.txt", b"x")]);
        assert!(unpack(&evil, "p.zip", dir.path()).is_err());
        assert!(!dir.path().parent().unwrap().join("evil.txt").exists());
    }

    #[test]
    fn package_without_this_program_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let suffix = std::env::consts::EXE_SUFFIX;
        std::fs::write(dir.path().join(format!("napd{suffix}")), b"old napd").unwrap();
        let pkg = zip_package(&[
            (&format!("napster-rs/napd{suffix}"), b"new napd"),
            (&format!("napster-rs/napster-cli{suffix}"), b"new cli"),
        ]);
        let staging = dir.path().join("staging");
        unpack(&pkg, "p.zip", &staging).unwrap();
        // the package has no "napster" program, so nothing may be touched
        let fake_exe = dir.path().join(format!("napster{suffix}"));
        let err = replace_binaries(&staging.join("napster-rs"), &fake_exe).unwrap_err();
        assert!(err.to_string().contains("doesn't contain"));
        assert_eq!(std::fs::read(dir.path().join(format!("napd{suffix}"))).unwrap(), b"old napd");
    }
}
