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

/// Queries the plugkit-bin GitHub Releases API for the latest published tag
/// (`vX.Y.Z` -> `X.Y.Z`), giving gm-runner parity with the JS wrapper's
/// self-heal (`bun x gm-plugkit@latest` forces a version check+reinstall
/// every boot). Prior to this, `ensure_wasm_installed` only ever checked
/// `existing.exists()` -- once a wasm was installed it was never compared
/// against a newer release, so a stale binary served forever. Best-effort:
/// network failure returns Ok(None) rather than erroring the whole spool
/// loop, since a failed freshness check must never block already-working
/// dispatch.
pub fn fetch_latest_plugkit_version() -> anyhow::Result<Option<String>> {
    let url = "https://api.github.com/repos/AnEntrypoint/plugkit-bin/releases/latest";
    let resp = ureq::get(url)
        .set("User-Agent", "gm-runner")
        .call()?;
    let body: serde_json::Value = serde_json::from_str(&resp.into_string()?)?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches('v').to_string());
    Ok(tag)
}

/// gm-runner registers `host_vec_embed` unconditionally in
/// `wasm_host.rs::register_env_imports` (real native candle inference via
/// `crate::embed::embed`, not a stub -- see `crates/gm-runner/src/embed.rs`),
/// so every gm-runner process is always a valid slim-wasm host: the slim
/// artifact's `cfg(feature = "slim")` build has no wasm-embedded safetensors
/// fallback and *requires* a host that answers `host_vec_embed` for real,
/// which gm-runner always does. Fetching the fat (embedded-weights) artifact
/// on a gm-runner host would only waste ~130MB of download+disk for a
/// fallback path that never triggers. Local install still lands at the fixed
/// `plugkit.wasm` filename (`wasm_path()` in main.rs) -- this only changes
/// which REMOTE artifact is fetched under that local name.
const REMOTE_ARTIFACT_NAME: &str = "plugkit-slim.wasm";

/// Downloads plugkit's wasm module for `version` from the plugkit-bin GitHub
/// Releases channel (same source gm-plugkit/bootstrap.js's
/// downloadFromGithubReleases uses), verified against the release's own
/// .sha256 sidecar. Always fetches the slim artifact (see
/// `REMOTE_ARTIFACT_NAME`) -- gm-runner always implements `host_vec_embed`
/// natively, so the fat artifact's embedded-safetensors fallback is never
/// needed on this host.
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
    let wasm_url = format!("{base}/{REMOTE_ARTIFACT_NAME}");
    let sha_url = format!("{base}/{REMOTE_ARTIFACT_NAME}.sha256");

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

/// The gm-runner-bin release asset name for the current host platform/arch,
/// mirroring bin/install.js's gmRunnerAssetName() exactly (that JS function's
/// own comment names this Rust function as the other place the mapping is
/// spelled). Returns None for a host combination CI does not build.
pub fn gm_runner_asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("gm-runner-windows-x64.exe"),
        ("windows", "aarch64") => Some("gm-runner-windows-arm64.exe"),
        ("macos", "x86_64") => Some("gm-runner-macos-x64"),
        ("macos", "aarch64") => Some("gm-runner-macos-arm64"),
        ("linux", "x86_64") => Some("gm-runner-linux-x64"),
        ("linux", "aarch64") => Some("gm-runner-linux-arm64"),
        _ => None,
    }
}

/// Queries gm-runner-bin's GitHub Releases API for the latest published tag,
/// same shape as fetch_latest_plugkit_version. Best-effort: network failure
/// returns Ok(None) so a flaky connection never blocks the spool loop.
pub fn fetch_latest_gm_runner_version() -> anyhow::Result<Option<String>> {
    let url = "https://api.github.com/repos/AnEntrypoint/gm-runner-bin/releases/latest";
    let resp = ureq::get(url)
        .set("User-Agent", "gm-runner")
        .call()?;
    let body: serde_json::Value = serde_json::from_str(&resp.into_string()?)?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches('v').to_string());
    Ok(tag)
}

/// Downloads gm-runner's own executable for `version` from the gm-runner-bin
/// GitHub Releases channel, verified against the release's own .sha256
/// sidecar (identical machinery to bootstrap_plugkit_wasm), and atomically
/// swaps it into place at `current_exe`. The running process keeps executing
/// from its already-mapped file descriptor / loaded pages on every platform
/// this targets (Windows allows overwrite-via-rename of a running exe's file
/// entry; Unix unlink-then-replace never touches the inode the running
/// process holds open) -- the NEW binary takes effect on the next process
/// start, not the current run. Caller is responsible for triggering that next
/// start (e.g. a clean exit for the supervisor/OS service manager to relaunch
/// from, mirroring the existing plugkit.wasm version-skew reload path).
pub fn bootstrap_gm_runner_self_update(version: &str, current_exe: &Path) -> anyhow::Result<PathBuf> {
    let asset = gm_runner_asset_name()
        .ok_or_else(|| anyhow::anyhow!("no gm-runner-bin release asset published for this host platform/arch"))?;

    let base = format!("https://github.com/AnEntrypoint/gm-runner-bin/releases/download/v{version}");
    let bin_url = format!("{base}/{asset}");
    let sha_url = format!("{base}/{asset}.sha256");

    let sha_resp = ureq::get(&sha_url).call()?;
    let sha_line = sha_resp.into_string()?;
    let expected_sha = sha_line
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty sha256 sidecar at {sha_url}"))?
        .to_string();

    let staged = install_dir().join(format!("{asset}.new"));
    download_and_verify(&bin_url, &staged, &expected_sha)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&staged)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged, perms)?;
    }

    // Rename-over-running-exe: on Unix this unlinks the old inode (the
    // running process keeps its already-open fd/mapped pages, unaffected)
    // and the new file takes over the path atomically. On Windows a running
    // exe's own file cannot be replaced while it holds an exclusive handle
    // on itself in the general case, but Windows does permit renaming an
    // open-for-execute file when opened with FILE_SHARE_DELETE (the default
    // sharing mode Rust's own std::fs::rename target-side handling assumes);
    // if this fails, the caller keeps the staged `.new` file for a follow-up
    // attempt on the current_exe.with_extension("new") path rather than
    // losing the verified download.
    fs::rename(&staged, current_exe)?;
    fs::write(install_dir().join("gm-runner.version"), version)?;
    Ok(current_exe.to_path_buf())
}
