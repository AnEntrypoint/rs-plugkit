use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn install_dir() -> PathBuf {
    let base = directories::BaseDirs::new().expect("no home directory resolvable on this platform");
    base.home_dir().join(".gm-tools")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `url` into `dest`, verifying against `expected_sha256_hex` before
/// the atomic rename lands. A checksum mismatch leaves `dest` untouched and
/// returns an explicit error -- never a silently-accepted corrupt artifact.
pub fn download_and_verify(url: &str, dest: &Path, expected_sha256_hex: &str) -> anyhow::Result<()> {
    let resp = ureq::get(url).call()?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;

    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(expected_sha256_hex) {
        anyhow::bail!(
            "sha256 mismatch downloading {url}: expected {expected_sha256_hex}, got {actual}"
        );
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dest)?;
    Ok(())
}

pub fn sha256_of_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    Ok(sha256_hex(&bytes))
}

/// Downloads plugkit.wasm for `version` from the plugkit-bin GitHub Releases
/// channel (same source gm-plugkit/bootstrap.js's downloadFromGithubReleases
/// uses), verified against the release's own .sha256 sidecar.
pub fn bootstrap_plugkit_wasm(version: &str) -> anyhow::Result<PathBuf> {
    let dest = install_dir().join("plugkit.wasm");
    if dest.exists() {
        // Already-installed content is trusted only if its version file
        // matches; a mismatch means a stale install and forces re-fetch.
        let version_file = install_dir().join("plugkit.version");
        if let Ok(installed) = fs::read_to_string(&version_file) {
            if installed.trim() == version {
                return Ok(dest);
            }
        }
    }

    let base = format!("https://github.com/AnEntrypoint/plugkit-bin/releases/download/v{version}");
    let wasm_url = format!("{base}/plugkit.wasm");
    let sha_url = format!("{base}/plugkit.wasm.sha256");

    let sha_resp = ureq::get(&sha_url).call()?;
    let sha_line = sha_resp.into_string()?;
    let expected_sha = sha_line
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty sha256 sidecar at {sha_url}"))?
        .to_string();

    download_and_verify(&wasm_url, &dest, &expected_sha)?;
    fs::write(install_dir().join("plugkit.version"), version)?;
    Ok(dest)
}
