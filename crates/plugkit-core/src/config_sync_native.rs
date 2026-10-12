use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{DEFAULT_REPO_CACHE_REL, DEFAULT_REPO_REFERENCE, DEFAULT_REPO_URL};

/// How long one dispatch waits for another process's in-flight probe of the same
/// (repo, reference) before it serves its own checkout instead.
const SHARED_PROBE_WAIT_MS: u64 = 15 * 1000;

const SHARED_PROBE_POLL_MS: u64 = 25;

/// Re-probe interval for a ref whose change cadence this machine has never
/// measured. The first probe that sees a new tip replaces it with the measured
/// interval, so the steady-state cadence is a fact about the ref, not a constant.
const UNMEASURED_REF_INTERVAL_MS: u64 = 60 * 60 * 1000;

const PROBE_BACKOFF_BASE_MS: u64 = 60 * 1000;

const PROBE_BACKOFF_MAX_MS: u64 = 60 * 60 * 1000;

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

/// The machine-wide answer to "what is the tip of `<repo> <reference>`". One record
/// per (repo, reference), shared by every project root on this machine, so N project
/// roots resolving in the same window cost at most one `git ls-remote`.
#[derive(Debug, Clone, Default)]
struct RemoteRecord {
    remote_sha: String,
    advertised_since_ms: u64,
    last_probe_ms: u64,
    change_interval_ms: u64,
    probe_count: u64,
    consecutive_failures: u32,
    degraded_reason: Option<String>,
}

impl RemoteRecord {
    fn newer(a: &RemoteRecord, b: &RemoteRecord) -> RemoteRecord {
        if b.last_probe_ms > a.last_probe_ms {
            b.clone()
        } else {
            a.clone()
        }
    }

    fn to_json(&self) -> String {
        json!({
            "remote_sha": self.remote_sha,
            "advertised_since_ms": self.advertised_since_ms,
            "last_probe_ms": self.last_probe_ms,
            "change_interval_ms": self.change_interval_ms,
            "probe_count": self.probe_count,
            "consecutive_failures": self.consecutive_failures,
            "degraded_reason": self.degraded_reason,
        })
        .to_string()
    }

    fn parse(raw: &str) -> RemoteRecord {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return RemoteRecord::default(),
        };
        RemoteRecord {
            remote_sha: v
                .get("remote_sha")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            advertised_since_ms: v
                .get("advertised_since_ms")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            last_probe_ms: v.get("last_probe_ms").and_then(|x| x.as_u64()).unwrap_or(0),
            change_interval_ms: v
                .get("change_interval_ms")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            probe_count: v.get("probe_count").and_then(|x| x.as_u64()).unwrap_or(0),
            consecutive_failures: v
                .get("consecutive_failures")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
            degraded_reason: v
                .get("degraded_reason")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        }
    }
}

/// Whether a remote probe is warranted. Derived from the pin being checked and the
/// last known remote sha, never from a fixed TTL:
///
/// * no tip is recorded for this (repo, reference) yet, or
/// * this project's checkout HEAD is not the recorded tip, or
/// * the recorded tip has stood longer than the interval at which this ref was last
///   observed to move.
///
/// A recorded probe failure replaces all three with a backoff counted in failures.
enum ProbeDecision {
    Due,
    Skip { sha: String },
    Backoff { reason: String },
}

fn probe_decision(rec: &RemoteRecord, have: Option<&str>, now: u64) -> ProbeDecision {
    if rec.remote_sha.is_empty() {
        return ProbeDecision::Due;
    }
    if rec.consecutive_failures > 0 {
        let backoff = PROBE_BACKOFF_BASE_MS
            .saturating_mul(1u64 << rec.consecutive_failures.min(20))
            .min(PROBE_BACKOFF_MAX_MS);
        return if now.saturating_sub(rec.last_probe_ms) >= backoff {
            ProbeDecision::Due
        } else {
            ProbeDecision::Backoff {
                reason: rec
                    .degraded_reason
                    .clone()
                    .unwrap_or_else(|| "the last remote probe failed".to_string()),
            }
        };
    }
    if have != Some(rec.remote_sha.as_str()) {
        return ProbeDecision::Due;
    }
    let interval = if rec.change_interval_ms > 0 {
        rec.change_interval_ms
    } else {
        UNMEASURED_REF_INTERVAL_MS
    };
    if now.saturating_sub(rec.advertised_since_ms) >= interval {
        ProbeDecision::Due
    } else {
        ProbeDecision::Skip {
            sha: rec.remote_sha.clone(),
        }
    }
}

enum GlobalTip {
    Known { sha: String, probed: bool },
    Held,
    Failed(String),
    Unavailable(String),
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
    let synced = ensure_current(
        DEFAULT_REPO_URL,
        DEFAULT_REPO_REFERENCE,
        &cache,
        SHARED_PROBE_WAIT_MS,
    )?;
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
    let have = local_sha(cache);

    let (remote, probed) =
        match global_remote_tip(repo, reference, cache, have.as_deref(), now, debounce_ms) {
            GlobalTip::Known { sha, probed } => (sha, probed),
            GlobalTip::Held => {
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
                };
            }
            GlobalTip::Failed(reason) => {
                return match have {
                    Some(sha) => Ok(NativeSync {
                        sha: Some(sha),
                        changed: false,
                        degraded_reason: Some(reason.clone()),
                        degraded_sticky: prior.degraded_sticky,
                        detail: format!("{reason}; serving current checkout"),
                    }),
                    None => Err(format!("{reason}; no local checkout to fall back to")),
                };
            }
            GlobalTip::Unavailable(reason) => {
                return fallback(
                    have,
                    format!("remote probe failed ({reason}); serving last good checkout"),
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
            detail: if probed {
                "remote sha unchanged; no fetch needed".to_string()
            } else {
                format!("remote sha {remote} is already recorded machine-wide; no probe needed")
            },
        });
    }

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
            };
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
            };
        }
    }
    let result = refresh(repo, reference, cache, have, &remote);
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
    remote: &str,
) -> Result<NativeSync, String> {
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

/// The single-flight, machine-global remote probe. Every project root on this
/// machine asks the same question, so at most one of them runs `git ls-remote`
/// per window and the rest adopt the recorded answer.
fn global_remote_tip(
    repo: &str,
    reference: &str,
    cache: &str,
    have: Option<&str>,
    now: u64,
    wait_budget_ms: u64,
) -> GlobalTip {
    let paths = probe_paths(repo, reference, cache);
    let rec = read_remote_record(&paths);
    match probe_decision(&rec, have, now) {
        ProbeDecision::Skip { sha } => return GlobalTip::Known { sha, probed: false },
        ProbeDecision::Backoff { reason } => return GlobalTip::Unavailable(reason),
        ProbeDecision::Due => {}
    }
    if let Err(e) = std::fs::create_dir_all(&paths.root) {
        return GlobalTip::Unavailable(format!("could not create {}: {e}", paths.root));
    }
    let baseline = rec.last_probe_ms;
    match acquire(&paths.lock()) {
        LockOutcome::Acquired => {
            let under_lock = read_remote_record(&paths);
            let outcome = match probe_decision(&under_lock, have, now) {
                ProbeDecision::Skip { sha } => GlobalTip::Known { sha, probed: false },
                ProbeDecision::Backoff { reason } => GlobalTip::Unavailable(reason),
                ProbeDecision::Due => probe_and_record(repo, reference, &paths, &under_lock, now),
            };
            let _ = std::fs::remove_dir(&paths.lock());
            outcome
        }
        LockOutcome::Held => match wait_for_shared_probe(&paths, baseline, wait_budget_ms) {
            Some(shared) => GlobalTip::Known {
                sha: shared.remote_sha.clone(),
                probed: false,
            },
            None => GlobalTip::Held,
        },
        LockOutcome::Failed(reason) => GlobalTip::Failed(reason),
    }
}

fn probe_and_record(
    repo: &str,
    reference: &str,
    paths: &ProbePaths,
    rec: &RemoteRecord,
    now: u64,
) -> GlobalTip {
    let mut next = rec.clone();
    next.last_probe_ms = now;
    next.probe_count = next.probe_count.saturating_add(1);
    match remote_sha(repo, reference) {
        Ok(sha) => {
            if next.remote_sha.as_str() != sha.as_str() {
                if !next.remote_sha.is_empty() && now > next.advertised_since_ms {
                    next.change_interval_ms = now.saturating_sub(next.advertised_since_ms);
                }
                next.remote_sha = sha.clone();
                next.advertised_since_ms = now;
            }
            next.consecutive_failures = 0;
            next.degraded_reason = None;
            write_remote_record(paths, &next);
            GlobalTip::Known { sha, probed: true }
        }
        Err(e) => {
            next.consecutive_failures = next.consecutive_failures.saturating_add(1);
            next.degraded_reason = Some(e.clone());
            write_remote_record(paths, &next);
            GlobalTip::Unavailable(e)
        }
    }
}

struct ProbePaths {
    root: String,
    key: String,
}

impl ProbePaths {
    fn record(&self) -> String {
        format!("{}/{}.json", self.root, self.key)
    }

    fn lock(&self) -> String {
        format!("{}/{}.lock", self.root, self.key)
    }
}

fn probe_paths(repo: &str, reference: &str, cache: &str) -> ProbePaths {
    let project_gm_dir = Path::new(cache)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    let root = crate::config::global_probe_root(project_gm_dir.as_deref())
        .unwrap_or_else(|| project_gm_dir.unwrap_or_default());
    let ident = format!("{repo}\u{0}{reference}");
    ProbePaths {
        root,
        key: format!("{:016x}", crate::hash::fnv1a64(ident.as_bytes())),
    }
}

/// The in-memory twin of the on-disk record: the write can fail under load, and
/// without it every dispatch would re-run `git ls-remote` with no backoff.
static REMEMBERED_REMOTE: Mutex<Option<HashMap<String, RemoteRecord>>> = Mutex::new(None);

fn remembered_remote(key: &str) -> Option<RemoteRecord> {
    REMEMBERED_REMOTE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|map| map.get(key)).cloned())
}

fn remember_remote(key: &str, rec: &RemoteRecord) {
    if let Ok(mut guard) = REMEMBERED_REMOTE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        let merged = RemoteRecord::newer(&map.get(key).cloned().unwrap_or_default(), rec);
        map.insert(key.to_string(), merged);
    }
}

fn read_remote_record(paths: &ProbePaths) -> RemoteRecord {
    let disk = std::fs::read_to_string(paths.record())
        .ok()
        .as_deref()
        .map(RemoteRecord::parse)
        .unwrap_or_default();
    match remembered_remote(&paths.key) {
        Some(remembered) => RemoteRecord::newer(&disk, &remembered),
        None => disk,
    }
}

fn write_remote_record(paths: &ProbePaths, rec: &RemoteRecord) {
    remember_remote(&paths.key, rec);
    let path = paths.record();
    let tmp = format!("{path}.tmp-{}", std::process::id());
    if std::fs::write(&tmp, rec.to_json()).is_err() {
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn wait_for_shared_probe(
    paths: &ProbePaths,
    baseline_ms: u64,
    budget_ms: u64,
) -> Option<RemoteRecord> {
    let deadline = now_ms().saturating_add(budget_ms);
    loop {
        let rec = read_remote_record(paths);
        if !rec.remote_sha.is_empty() && rec.last_probe_ms > baseline_ms {
            remember_remote(&paths.key, &rec);
            return Some(rec);
        }
        if now_ms() >= deadline {
            return None;
        }
        sleep(Duration::from_millis(SHARED_PROBE_POLL_MS));
    }
}

fn materialize(repo: &str, reference: &str, cache: &str) -> Result<Materialized, String> {
    let staging = format!("{cache}.native.staging-{}", std::process::id());
    let retired = format!("{cache}.native.retired");
    wipe(&staging);
    wipe(&retired);
    if let Err(e) = git_out(
        &[
            "clone",
            "--depth",
            "1",
            "--branch",
            reference,
            "--",
            repo,
            staging.as_str(),
        ],
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
