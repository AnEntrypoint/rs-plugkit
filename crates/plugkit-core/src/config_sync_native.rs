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
    pub degraded_reason: Option<String>,
    pub degraded_sticky: bool,
    pub detail: String,
}

struct CheckState {
    last_checked_ms: u64,
    degraded_reason: Option<String>,
    degraded_sticky: bool,
}

impl CheckState {
    fn to_json(&self) -> String {
        json!({
            "last_checked_ms": self.last_checked_ms,
            "degraded_reason": self.degraded_reason,
            "degraded_sticky": self.degraded_sticky,
        })
        .to_string()
    }
}

enum Materialized {
    Atomic,
    InPlace { reason: String },
}

enum LockOutcome {
    Acquired,
    Held,
    Failed(String),
}

pub fn ensure_default_cache() -> Result<(String, Option<String>), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let root = crate::config::normalize_project_root(&cwd.to_string_lossy().replace('\\', "/"));
    let cache = Path::new(&root)
        .join(DEFAULT_REPO_CACHE_REL)
        .to_string_lossy()
        .replace('\\', "/");
    let synced = ensure_current(DEFAULT_REPO_URL, DEFAULT_REPO_REFERENCE, &cache, DEBOUNCE_MS)?;
    Ok((cache, synced.degraded_reason))
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
    let prior = read_state(&state_path);
    let debounced = prior.last_checked_ms > 0
        && prior.last_checked_ms <= now
        && now - prior.last_checked_ms < debounce_ms;
    if debounced {
        return Ok(NativeSync {
            sha: local_sha(cache),
            changed: false,
            degraded_reason: prior.degraded_reason.clone(),
            degraded_sticky: prior.degraded_sticky,
            detail: "debounced: checked recently".to_string(),
        });
    }

    let have = local_sha(cache);
    let lock_path = format!("{cache}.native.lock");
    match acquire(&lock_path) {
        LockOutcome::Acquired => {}
        LockOutcome::Held => {
            return match have {
                Some(sha) => Ok(NativeSync {
                    sha: Some(sha),
                    changed: false,
                    degraded_reason: prior.degraded_reason.clone(),
                    degraded_sticky: prior.degraded_sticky,
                    detail: "another refresh in progress; serving current checkout".to_string(),
                }),
                None => Err(format!(
                    "another process is cloning {repo} and no local checkout exists yet"
                )),
            }
        }
        LockOutcome::Failed(reason) => {
            return match have {
                Some(sha) => Ok(NativeSync {
                    sha: Some(sha),
                    changed: false,
                    degraded_reason: Some(reason.clone()),
                    degraded_sticky: prior.degraded_sticky,
                    detail: format!("{reason}; serving current checkout"),
                }),
                None => Err(format!("{reason}; no local checkout to fall back to")),
            }
        }
    }
    let result = refresh(repo, reference, cache, have, &prior);
    let _ = std::fs::remove_dir(&lock_path);
    let persisted = match &result {
        Ok(synced) => CheckState {
            last_checked_ms: now,
            degraded_reason: synced.degraded_reason.clone(),
            degraded_sticky: synced.degraded_sticky,
        },
        Err(e) => CheckState {
            last_checked_ms: now,
            degraded_reason: Some(e.clone()),
            degraded_sticky: true,
        },
    };
    let _ = std::fs::write(&state_path, persisted.to_json());
    result
}

fn refresh(
    repo: &str,
    reference: &str,
    cache: &str,
    have: Option<String>,
    prior: &CheckState,
) -> Result<NativeSync, String> {
    let remote = match remote_sha(repo, reference) {
        Ok(sha) => sha,
        Err(e) => {
            return fallback(
                have,
                format!("remote probe failed ({e}); serving last good checkout"),
                false,
            )
        }
    };
    if have.as_deref() == Some(remote.as_str()) {
        let sticky = prior.degraded_sticky;
        return Ok(NativeSync {
            sha: have,
            changed: false,
            degraded_reason: if sticky {
                prior.degraded_reason.clone()
            } else {
                None
            },
            degraded_sticky: sticky,
            detail: "remote sha unchanged; no fetch needed".to_string(),
        });
    }
    match materialize(repo, reference, cache) {
        Ok(Materialized::Atomic) => Ok(NativeSync {
            sha: local_sha(cache),
            changed: true,
            degraded_reason: None,
            degraded_sticky: false,
            detail: format!("updated to {remote}"),
        }),
        Ok(Materialized::InPlace { reason }) => Ok(NativeSync {
            sha: local_sha(cache),
            changed: true,
            degraded_reason: Some(reason.clone()),
            degraded_sticky: true,
            detail: reason,
        }),
        Err(e) => fallback(
            have,
            format!("update to {remote} failed ({e}); serving previous checkout"),
            true,
        ),
    }
}

fn fallback(have: Option<String>, detail: String, sticky: bool) -> Result<NativeSync, String> {
    match have {
        Some(sha) => Ok(NativeSync {
            sha: Some(sha),
            changed: false,
            degraded_reason: Some(detail.clone()),
            degraded_sticky: sticky,
            detail,
        }),
        None => Err(format!("{detail}; no local checkout to fall back to")),
    }
}

fn materialize(repo: &str, reference: &str, cache: &str) -> Result<Materialized, String> {
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
            return update_in_place(reference, cache)
                .map(|()| Materialized::InPlace {
                    reason: format!(
                        "could not move {cache} aside ({e}); checkout updated in place, so readers can briefly see a mix of old and new files"
                    ),
                })
                .map_err(|in_place| {
                    format!("could not move {cache} aside ({e}), and the in-place update also failed: {in_place}")
                });
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
    Ok(Materialized::Atomic)
}

fn update_in_place(reference: &str, cache: &str) -> Result<(), String> {
    git_out(
        &["-c", "gc.auto=0", "fetch", "--depth", "1", "origin", reference],
        Some(cache),
    )?;
    git_out(
        &["-c", "gc.auto=0", "checkout", "--force", "FETCH_HEAD"],
        Some(cache),
    )?;
    Ok(())
}

fn acquire(lock_path: &str) -> LockOutcome {
    match std::fs::create_dir(lock_path) {
        Ok(()) => LockOutcome::Acquired,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = std::fs::metadata(lock_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(LOCK_STALE_SECS));
            if !stale {
                return LockOutcome::Held;
            }
            match std::fs::remove_dir(lock_path).and_then(|()| std::fs::create_dir(lock_path)) {
                Ok(()) => LockOutcome::Acquired,
                Err(e) => LockOutcome::Failed(format!(
                    "could not reclaim the stale refresh lock {lock_path} ({e})"
                )),
            }
        }
        Err(e) => LockOutcome::Failed(format!(
            "could not take the refresh lock {lock_path} ({e})"
        )),
    }
}

fn wipe(path: &str) {
    let _ = std::fs::remove_dir_all(path);
}

fn read_state(state_path: &str) -> CheckState {
    let raw: Option<Value> = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    CheckState {
        last_checked_ms: raw
            .as_ref()
            .and_then(|v| v.get("last_checked_ms"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        degraded_reason: raw
            .as_ref()
            .and_then(|v| v.get("degraded_reason"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
        degraded_sticky: raw
            .as_ref()
            .and_then(|v| v.get("degraded_sticky"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
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
