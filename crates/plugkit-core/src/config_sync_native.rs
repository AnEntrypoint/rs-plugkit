use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{DEFAULT_REPO_CACHE_REL, DEFAULT_REPO_REFERENCE, DEFAULT_REPO_URL};

const DEBOUNCE_MS: u64 = 15 * 1000;

const LOCK_STALE_SECS: u64 = 10 * 60;

pub struct NativeSync {
    pub sha: Option<String>,
    pub changed: bool,
    pub degraded: bool,
    pub detail: String,
}

pub fn ensure_default_cache() -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let cache = cwd
        .join(DEFAULT_REPO_CACHE_REL)
        .to_string_lossy()
        .replace('\\', "/");
    ensure_current(DEFAULT_REPO_URL, DEFAULT_REPO_REFERENCE, &cache, DEBOUNCE_MS)?;
    Ok(cache)
}

pub fn read_cache_prose(cache: &str, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(format!("{cache}/prose/{key}.md")).ok()?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn ensure_current(
    repo: &str,
    reference: &str,
    cache: &str,
    debounce_ms: u64,
) -> Result<NativeSync, String> {
    if let Some(parent) = Path::new(cache).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let state_path = format!("{cache}.native.sync.json");
    let now = now_ms();
    let last_checked = read_last_checked(&state_path);
    let debounced = last_checked > 0 && last_checked <= now && now - last_checked < debounce_ms;
    if debounced {
        return Ok(NativeSync {
            sha: local_sha(cache),
            changed: false,
            degraded: false,
            detail: "debounced: checked recently".to_string(),
        });
    }

    let have = local_sha(cache);
    let lock_path = format!("{cache}.native.lock");
    if !acquire(&lock_path) {
        return match have {
            Some(sha) => Ok(NativeSync {
                sha: Some(sha),
                changed: false,
                degraded: false,
                detail: "another refresh in progress; serving current checkout".to_string(),
            }),
            None => Err(format!(
                "another process is cloning {repo} and no local checkout exists yet"
            )),
        };
    }
    let result = refresh(repo, reference, cache, have);
    let _ = std::fs::remove_dir(&lock_path);
    let _ = std::fs::write(&state_path, json!({ "last_checked_ms": now }).to_string());
    result
}

fn refresh(
    repo: &str,
    reference: &str,
    cache: &str,
    have: Option<String>,
) -> Result<NativeSync, String> {
    let remote = match remote_sha(repo, reference) {
        Ok(sha) => sha,
        Err(e) => return fallback(have, format!("remote probe failed ({e}); serving last good checkout")),
    };
    if have.as_deref() == Some(remote.as_str()) {
        return Ok(NativeSync {
            sha: have,
            changed: false,
            degraded: false,
            detail: "remote sha unchanged; no fetch needed".to_string(),
        });
    }
    match materialize(repo, reference, cache) {
        Ok(()) => Ok(NativeSync {
            sha: local_sha(cache),
            changed: true,
            degraded: false,
            detail: format!("updated to {remote}"),
        }),
        Err(e) => fallback(have, format!("update to {remote} failed ({e}); serving previous checkout")),
    }
}

fn fallback(have: Option<String>, detail: String) -> Result<NativeSync, String> {
    match have {
        Some(sha) => Ok(NativeSync {
            sha: Some(sha),
            changed: false,
            degraded: true,
            detail,
        }),
        None => Err(format!("{detail}; no local checkout to fall back to")),
    }
}

fn materialize(repo: &str, reference: &str, cache: &str) -> Result<(), String> {
    let staging = format!("{cache}.native.staging-{}", std::process::id());
    let retired = format!("{cache}.native.retired");
    wipe(&staging);
    wipe(&retired);
    if let Err(e) = git_out(
        &["clone", "--depth", "1", "--branch", reference, "--", repo, staging.as_str()],
        None,
    ) {
        wipe(&staging);
        return Err(e);
    }
    let had_live = Path::new(cache).exists();
    if had_live {
        if let Err(e) = std::fs::rename(cache, &retired) {
            wipe(&staging);
            return Err(format!("could not move {cache} aside: {e}"));
        }
    }
    if let Err(e) = std::fs::rename(&staging, cache) {
        if had_live {
            let _ = std::fs::rename(&retired, cache);
        }
        wipe(&staging);
        return Err(format!("could not move staged checkout into {cache}: {e}"));
    }
    wipe(&retired);
    Ok(())
}

fn acquire(lock_path: &str) -> bool {
    if std::fs::create_dir(lock_path).is_ok() {
        return true;
    }
    let stale = std::fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > Duration::from_secs(LOCK_STALE_SECS));
    stale && std::fs::remove_dir(lock_path).is_ok() && std::fs::create_dir(lock_path).is_ok()
}

fn wipe(path: &str) {
    let _ = std::fs::remove_dir_all(path);
}

fn read_last_checked(state_path: &str) -> u64 {
    std::fs::read_to_string(state_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("last_checked_ms").and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

fn local_sha(cache: &str) -> Option<String> {
    let out = git_out(&["rev-parse", "HEAD"], Some(cache)).ok()?;
    let sha = out.trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

fn remote_sha(repo: &str, reference: &str) -> Result<String, String> {
    let out = git_out(&["ls-remote", "--", repo, reference], None)?;
    out.split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("remote {repo} advertises no ref matching {reference}"))
}

fn git_out(args: &[&str], cwd: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(dir) = cwd {
        cmd.arg("-C").arg(dir);
    }
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or("?"),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
