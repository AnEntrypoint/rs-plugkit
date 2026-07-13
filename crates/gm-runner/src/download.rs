use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn install_dir() -> PathBuf {
    let base = directories::BaseDirs::new().expect("no home directory resolvable on this platform");
    base.home_dir().join(".gm-tools")
}

fn progress_file() -> PathBuf {
    install_dir().join(".download-progress.json")
}

/// Written before/during/after every download so a dispatching agent can
/// poll `~/.gm-tools/.download-progress.json` (same pattern as the spool
/// watcher's `.status.json` heartbeat) to know content isn't available yet
/// rather than a dispatch failing opaquely mid-download. Cleared (removed)
/// on completion -- absence of the file means no download in flight.
fn write_progress(artifact: &str, downloaded: u64, total: Option<u64>, done: bool) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if done {
        let _ = fs::remove_file(progress_file());
        return;
    }
    let body = serde_json::json!({
        "artifact": artifact,
        "downloaded_bytes": downloaded,
        "total_bytes": total,
        "pct": total.map(|t| if t > 0 { (downloaded as f64 / t as f64 * 100.0).round() } else { 0.0 }),
        "ts": now_ms,
    });
    let _ = fs::write(progress_file(), body.to_string());
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
/// Streams in chunks (not one read_to_end) so `.download-progress.json` gets
/// real byte-level updates during large transfers (the 133MB embed weights
/// take real wall-clock time; a dispatching agent polling progress sees
/// actual movement, not a single all-or-nothing jump).
pub fn download_and_verify(url: &str, dest: &Path, expected_sha256_hex: &str) -> anyhow::Result<()> {
    let artifact_name = dest.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let resp = ureq::get(url).call()?;
    let total: Option<u64> = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());

    let mut reader = resp.into_reader();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 65536];
    let mut downloaded: u64 = 0;
    write_progress(&artifact_name, 0, total, false);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        downloaded += n as u64;
        write_progress(&artifact_name, downloaded, total, false);
    }

    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(expected_sha256_hex) {
        write_progress(&artifact_name, downloaded, total, true);
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
    write_progress(&artifact_name, downloaded, total, true);
    Ok(())
}

/// Reads the current in-flight download's progress, if any. Callers (the
/// spool watcher, or an agent probing via a future dedicated verb) use this
/// to report "content not yet available" instead of guessing from a hung
/// dispatch.
pub fn current_progress() -> Option<serde_json::Value> {
    let content = fs::read_to_string(progress_file()).ok()?;
    serde_json::from_str(&content).ok()
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
