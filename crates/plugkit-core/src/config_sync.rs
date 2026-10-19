use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::config::{Freshness, RepoFetcher, RepoSource};

const DEFAULT_DEBOUNCE_MS: u64 = 15 * 1000;

const BACKOFF_BASE_MS: u64 = 60 * 1000;

const BACKOFF_MAX_MS: u64 = 60 * 60 * 1000;

const LOCK_STALE_MS: u64 = 10 * 60 * 1000;

const SHARED_PROBE_POLL_MS: u64 = 250;

const SHARED_PROBE_WAIT_MS: u64 = 5 * 1000;

const UNMEASURED_REF_INTERVAL_MS: u64 = 60 * 1000;

const PROBE_BACKOFF_BASE_MS: u64 = 60 * 1000;

const PROBE_BACKOFF_MAX_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct SyncOutcome {
    pub sha: Option<String>,
    pub changed: bool,
    pub degraded_reason: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
struct SyncState {
    last_checked_ms: u64,
    last_sha: String,
    consecutive_failures: u32,
    degraded_reason: Option<String>,
    degraded_sticky: bool,
}

impl SyncState {
    fn parse(raw: &str) -> SyncState {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return SyncState::default(),
        };
        SyncState {
            last_checked_ms: v
                .get("last_checked_ms")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            last_sha: v
                .get("last_sha")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            consecutive_failures: v
                .get("consecutive_failures")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
            degraded_reason: v
                .get("degraded_reason")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            degraded_sticky: v
                .get("degraded_sticky")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        }
    }

    fn to_json(&self) -> String {
        json!({
            "last_checked_ms": self.last_checked_ms,
            "last_sha": self.last_sha,
            "consecutive_failures": self.consecutive_failures,
            "degraded_reason": self.degraded_reason,
            "degraded_sticky": self.degraded_sticky,
        })
        .to_string()
    }

    fn next_probe_delay_ms(&self, debounce_ms: u64) -> u64 {
        if self.consecutive_failures == 0 {
            return debounce_ms;
        }
        let exp = self.consecutive_failures.min(20);
        let backoff = BACKOFF_BASE_MS
            .saturating_mul(1u64 << exp)
            .min(BACKOFF_MAX_MS);
        backoff.max(debounce_ms)
    }
}

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
}

enum ProbeDecision {
    Due,
    Skip { sha: String },
    Backoff { reason: String },
}

fn probe_decision(rec: &RemoteRecord, now: u64, forced: bool) -> ProbeDecision {
    if forced {
        return ProbeDecision::Due;
    }
    if rec.remote_sha.is_empty() || rec.last_probe_ms > now {
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
    let interval = if rec.change_interval_ms > 0 {
        rec.change_interval_ms
    } else {
        UNMEASURED_REF_INTERVAL_MS
    };
    if now.saturating_sub(rec.last_probe_ms) >= interval {
        ProbeDecision::Due
    } else {
        ProbeDecision::Skip {
            sha: rec.remote_sha.clone(),
        }
    }
}

enum GlobalTip {
    Known(String),
    Held(String),
    Unavailable(String),
}

fn now_ms() -> u64 {
    crate::orchestrator::state::now_ms() as u64
}

static REMEMBERED_PROBE_MS: std::sync::Mutex<Option<HashMap<String, u64>>> = std::sync::Mutex::new(None);

fn remembered_probe_ms(src: &RepoSource) -> u64 {
    match REMEMBERED_PROBE_MS.lock() {
        Ok(guard) => guard.as_ref().and_then(|m| m.get(&source_key(src))).copied().unwrap_or(0),
        Err(_) => 0,
    }
}

fn remember_probe_ms(src: &RepoSource, now: u64) {
    if let Ok(mut guard) = REMEMBERED_PROBE_MS.lock() {
        guard.get_or_insert_with(HashMap::new).insert(source_key(src), now);
    }
}

fn source_key(src: &RepoSource) -> String {
    let ident = format!(
        "{}\u{0}{}",
        src.repo,
        src.reference.as_deref().unwrap_or("")
    );
    format!("{:016x}", crate::hash::fnv1a64(ident.as_bytes()))
}

fn cache_root(src: &RepoSource) -> String {
    crate::pkfs::anchor(&src.cache_dir)
}

fn state_path(src: &RepoSource) -> String {
    format!("{}.{}.sync.json", cache_root(src), source_key(src))
}

fn lock_path(src: &RepoSource) -> String {
    format!("{}.{}.lock", cache_root(src), source_key(src))
}

struct RemoteProbePaths {
    root: String,
    key: String,
}

impl RemoteProbePaths {
    fn record(&self) -> String {
        format!("{}/{}.json", self.root, self.key)
    }

    fn lock(&self) -> String {
        format!("{}/{}.lock", self.root, self.key)
    }
}

fn remote_probe_paths(src: &RepoSource) -> RemoteProbePaths {
    let project_gm_dir = Path::new(&src.cache_dir)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    let root = crate::config::global_probe_root(project_gm_dir.as_deref())
        .unwrap_or_else(|| project_gm_dir.unwrap_or_default());
    let ident = format!(
        "{}\u{0}{}",
        src.repo,
        src.reference.as_deref().unwrap_or("")
    );
    RemoteProbePaths {
        root,
        key: format!("{:016x}", crate::hash::fnv1a64(ident.as_bytes())),
    }
}

static IN_MEMORY_REMOTE_RECORDS: std::sync::Mutex<Option<HashMap<String, RemoteRecord>>> =
    std::sync::Mutex::new(None);

fn remembered_remote(key: &str) -> Option<RemoteRecord> {
    IN_MEMORY_REMOTE_RECORDS
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|map| map.get(key)).cloned())
}

fn remember_remote(key: &str, rec: &RemoteRecord) {
    if let Ok(mut guard) = IN_MEMORY_REMOTE_RECORDS.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        let merged = RemoteRecord::newer(&map.get(key).cloned().unwrap_or_default(), rec);
        map.insert(key.to_string(), merged);
    }
}

fn read_remote_record(paths: &RemoteProbePaths) -> RemoteRecord {
    let disk = crate::pkfs::read_to_string(&paths.record())
        .as_deref()
        .map(RemoteRecord::parse)
        .unwrap_or_default();
    match remembered_remote(&paths.key) {
        Some(remembered) => RemoteRecord::newer(&disk, &remembered),
        None => disk,
    }
}

fn write_remote_record(paths: &RemoteProbePaths, rec: &RemoteRecord) {
    remember_remote(&paths.key, rec);
    let path = paths.record();
    crate::dispatch_ledger::sweep_stale_tmp_beside(&path, now_ms());
    let tmp = format!("{path}.tmp-{}", now_ms());
    if !crate::pkfs::write(&tmp, &rec.to_json()) {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(&tmp);
        return;
    }
    if !rename(&tmp, &path) {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(&tmp);
    }
}

fn global_remote_tip(src: &RepoSource, now: u64) -> GlobalTip {
    global_remote_tip_with(src, now, false)
}

fn global_remote_tip_forced(src: &RepoSource, now: u64) -> GlobalTip {
    global_remote_tip_with(src, now, true)
}

fn global_remote_tip_with(src: &RepoSource, now: u64, forced: bool) -> GlobalTip {
    let paths = remote_probe_paths(src);
    let rec = read_remote_record(&paths);
    match probe_decision(&rec, now, forced) {
        ProbeDecision::Skip { sha } => return GlobalTip::Known(sha),
        ProbeDecision::Backoff { reason } => return GlobalTip::Unavailable(reason),
        ProbeDecision::Due => {}
    }
    let baseline = rec.last_probe_ms;
    match try_lock_at(&paths.lock(), &paths.root) {
        LockProbe::Acquired(lock) => {
            let under_lock = read_remote_record(&paths);
            let outcome = match probe_decision(&under_lock, now, forced) {
                ProbeDecision::Skip { sha } => GlobalTip::Known(sha),
                ProbeDecision::Backoff { reason } => GlobalTip::Unavailable(reason),
                ProbeDecision::Due => probe_and_record(src, &paths, &under_lock, now),
            };
            let _ = unlock(&lock);
            outcome
        }
        LockProbe::Held => {
            let mut waited_ms = 0;
            loop {
                if let Some(shared) = shared_probe_since(&paths, baseline) {
                    break GlobalTip::Known(shared.remote_sha);
                }
                if waited_ms >= SHARED_PROBE_WAIT_MS {
                    break GlobalTip::Held(format!(
                        "config probe lock {} stayed held by another process for {} ms; the guest gave up waiting for its result",
                        paths.lock(),
                        SHARED_PROBE_WAIT_MS
                    ));
                }
                crate::orchestrator::sleep_ms(SHARED_PROBE_POLL_MS);
                waited_ms += SHARED_PROBE_POLL_MS;
            }
        }
        LockProbe::Failed(reason) => GlobalTip::Unavailable(reason),
    }
}

fn shared_probe_since(paths: &RemoteProbePaths, baseline_ms: u64) -> Option<RemoteRecord> {
    let rec = read_remote_record(paths);
    if !rec.remote_sha.is_empty() && rec.last_probe_ms > baseline_ms {
        remember_remote(&paths.key, &rec);
        return Some(rec);
    }
    None
}

fn probe_and_record(
    src: &RepoSource,
    paths: &RemoteProbePaths,
    rec: &RemoteRecord,
    now: u64,
) -> GlobalTip {
    let mut next = rec.clone();
    next.last_probe_ms = now;
    next.probe_count = next.probe_count.saturating_add(1);
    match probe_remote_sha(src) {
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
            GlobalTip::Known(sha)
        }
        Err(e) => {
            next.consecutive_failures = next.consecutive_failures.saturating_add(1);
            next.degraded_reason = Some(e.clone());
            write_remote_record(paths, &next);
            GlobalTip::Unavailable(e)
        }
    }
}

fn read_state(src: &RepoSource) -> SyncState {
    match crate::pkfs::read_to_string(&state_path(src)) {
        Some(raw) => SyncState::parse(&raw),
        None => SyncState::default(),
    }
}

fn write_state(src: &RepoSource, st: &SyncState) {
    let path = state_path(src);
    crate::dispatch_ledger::sweep_stale_tmp_beside(&path, now_ms());
    let tmp = format!("{}.tmp-{}", path, now_ms());
    if !crate::pkfs::write(&tmp, &st.to_json()) {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(&tmp);
        return;
    }
    if !rename(&tmp, &path) {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(&tmp);
    }
}

fn rename(from: &str, to: &str) -> bool {
    let (Ok(f), Ok(t)) = (serde_json::to_string(from), serde_json::to_string(to)) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');try{{fs.renameSync({f},{t});process.stdout.write('ok');}}catch(e){{process.stdout.write('fail');}}"
    );
    exec_js_stdout(&code, 15000)
        .map(|s| s.contains("ok"))
        .unwrap_or(false)
}

fn exec_js_stdout(code: &str, timeout_ms: u32) -> Option<String> {
    let opts = json!({ "timeoutMs": timeout_ms }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed)?;
    let parsed: Value = serde_json::from_str(&out).ok()?;
    parsed
        .get("stdout")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

struct RefreshLock {
    path: String,
    token: String,
}

enum LockProbe {
    Acquired(RefreshLock),
    Held,
    Failed(String),
}

fn try_lock(src: &RepoSource) -> LockProbe {
    let mkdir_dir = Path::new(&cache_root(src))
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    try_lock_at(&lock_path(src), &mkdir_dir)
}

fn try_lock_at(lock_path: &str, mkdir_dir: &str) -> LockProbe {
    let path = lock_path.to_string();
    let Ok(p) = serde_json::to_string(&path) else {
        return LockProbe::Failed("lock path is not representable".to_string());
    };
    let Ok(parent_p) = serde_json::to_string(mkdir_dir) else {
        return LockProbe::Failed("cache root is not representable".to_string());
    };
    let code = format!(
        "const fs=require('fs');const p={p};const staleMs={LOCK_STALE_MS};\
             const parentDir={parent_p};\
             try{{fs.mkdirSync(parentDir,{{recursive:true}});}}catch(e0){{}}\
             process.stdout.write((function(){{\
             function acquire(){{\
               const token=process.pid+'-'+Date.now()+'-'+Math.random().toString(36).slice(2);\
               try{{fs.mkdirSync(p);}}catch(e){{return e.code==='EEXIST'?'busy':'failed:mkdir:'+e.code;}}\
               try{{fs.writeFileSync(p+'/owner',token,{{flag:'wx'}});fs.writeFileSync(p+'/owner-pid',String(process.ppid),{{flag:'wx'}});return 'acquired:'+token;}}\
               catch(e){{try{{fs.rmSync(p,{{recursive:true,force:true}});}}catch(e2){{}}return 'failed:owner:'+e.code;}}\
             }}\
             function ownerAlive(){{\
               let raw=null;try{{raw=fs.readFileSync(p+'/owner-pid','utf8');}}catch(e){{return null;}}\
               const pid=parseInt(raw,10);\
               if(!(pid>0)){{return null;}}\
               try{{process.kill(pid,0);return true;}}catch(e){{return e.code==='EPERM';}}\
             }}\
             let first=acquire();if(first!=='busy'){{return first;}}\
             let st=null;try{{st=fs.statSync(p);}}catch(e2){{return e2.code==='ENOENT'?'busy':'failed:stat:'+e2.code;}}\
             let observed=null;try{{observed=fs.readFileSync(p+'/owner','utf8');}}catch(e5){{observed=null;}}\
             if(ownerAlive()!==false&&Date.now()-st.mtimeMs<=staleMs){{return 'busy';}}\
             const aside=p+'.stale-'+process.pid+'-'+Date.now();\
             try{{fs.renameSync(p,aside);}}catch(e3){{return 'failed:stale-rename:'+e3.code;}}\
             let moved=null;try{{moved=fs.readFileSync(aside+'/owner','utf8');}}catch(e6){{moved=null;}}\
             if(moved!==observed){{try{{fs.renameSync(aside,p);}}catch(e7){{}}return 'busy';}}\
             try{{fs.rmSync(aside,{{recursive:true,force:true}});}}catch(e4){{}}\
             return acquire();\
             }})());"
    );
    let Some(reply) = exec_js_stdout(&code, 15000) else {
        return LockProbe::Failed("the host exec returned no stdout".to_string());
    };
    let text = reply.trim();
    if text == "busy" {
        return LockProbe::Held;
    }
    if let Some(token) = text.strip_prefix("acquired:").filter(|t| !t.is_empty()) {
        return LockProbe::Acquired(RefreshLock {
            path,
            token: token.to_string(),
        });
    }
    match text.strip_prefix("failed:") {
        Some(kind) => LockProbe::Failed(kind.to_string()),
        None => LockProbe::Failed("unexpected lock probe reply".to_string()),
    }
}

fn owns_lock(lock: &RefreshLock) -> bool {
    let (Ok(p), Ok(token)) = (
        serde_json::to_string(&lock.path),
        serde_json::to_string(&lock.token),
    ) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');try{{process.stdout.write(fs.readFileSync({p}+'/owner','utf8')==={token}?'owned':'lost');}}catch(e){{process.stdout.write('lost');}}"
    );
    exec_js_stdout(&code, 15000)
        .map(|s| s.trim() == "owned")
        .unwrap_or(false)
}

fn unlock(lock: &RefreshLock) -> bool {
    let (Ok(p), Ok(token)) = (
        serde_json::to_string(&lock.path),
        serde_json::to_string(&lock.token),
    ) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');try{{if(fs.readFileSync({p}+'/owner','utf8')!=={token}){{process.stdout.write('lost');}}else{{fs.rmSync({p},{{recursive:true,force:true}});process.stdout.write('released');}}}}catch(e){{process.stdout.write('lost');}}"
    );
    exec_js_stdout(&code, 15000)
        .map(|s| s.trim() == "released")
        .unwrap_or(false)
}

fn git(argv: &[&str], cwd: Option<&str>) -> Result<String, String> {
    let v = crate::wasm_dispatch::git_call_argv(argv, cwd);
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let stdout = v
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if !ok || code != 0 {
        let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        let msg = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(format!(
            "git {} failed: {}",
            argv.first().copied().unwrap_or("?"),
            msg
        ));
    }
    Ok(stdout)
}

fn local_sha(src: &RepoSource) -> Option<String> {
    let out = git(&["rev-parse", "HEAD"], Some(&cache_root(src))).ok()?;
    let s = out.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn probe_remote_sha(src: &RepoSource) -> Result<String, String> {
    let reference = src.reference.as_deref().unwrap_or("HEAD");
    let out = git(&["ls-remote", "--", &src.repo, reference], None)?;
    if let Some(sha) = out.split_whitespace().next() {
        if !sha.is_empty() {
            return Ok(sha.to_string());
        }
    }
    if is_sha_like(reference) {
        return Ok(reference.to_string());
    }
    Err(format!(
        "remote {} advertises no ref matching {}",
        src.repo, reference
    ))
}

fn is_sha_like(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn staging_dir(src: &RepoSource, lock: &RefreshLock) -> String {
    format!(
        "{}.{}.staging-{}",
        cache_root(src),
        source_key(src),
        lock.token
    )
}

fn legacy_staging_dir(src: &RepoSource) -> String {
    format!("{}.{}.staging", cache_root(src), source_key(src))
}

fn retired_dir(src: &RepoSource) -> String {
    format!("{}.{}.retired", cache_root(src), source_key(src))
}

fn remove_tree(path: &str) -> bool {
    let Ok(p) = serde_json::to_string(path) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');try{{fs.rmSync({p},{{recursive:true,force:true}});process.stdout.write('ok');}}catch(e){{process.stdout.write('fail');}}"
    );
    exec_js_stdout(&code, 30000)
        .map(|s| s.contains("ok"))
        .unwrap_or(false)
}

fn recover_stranded(src: &RepoSource) {
    let retired = retired_dir(src);
    if crate::pkfs::exists(&retired) {
        if crate::pkfs::exists(&src.cache_dir) {
            remove_tree(&retired);
        } else {
            rename(&retired, &src.cache_dir);
        }
    }
    let staging = legacy_staging_dir(src);
    if crate::pkfs::exists(&staging) {
        remove_tree(&staging);
    }
}

enum Publish {
    Atomic,
    InPlace { reason: String },
}

fn publish_staged(src: &RepoSource, staging: &str, lock: &RefreshLock) -> Result<Publish, String> {
    if !owns_lock(lock) {
        return Err("config refresh lock ownership was lost before publish".to_string());
    }
    let retired = retired_dir(src);
    remove_tree(&retired);

    let had_live = crate::pkfs::exists(&src.cache_dir);
    if had_live && !rename(&src.cache_dir, &retired) {
        remove_tree(&staging);
        return update_checkout_in_place(src)
            .map(|()| Publish::InPlace {
                reason: format!(
                    "could not move {} aside to publish atomically; checkout updated in place, so readers can briefly see a mix of old and new files",
                    src.cache_dir
                ),
            })
            .map_err(|e| {
                format!(
                    "could not move {} aside to publish a new checkout, and the in-place update also failed: {e}",
                    src.cache_dir
                )
            });
    }
    if !rename(&staging, &src.cache_dir) {
        if had_live {
            rename(&retired, &src.cache_dir);
        }
        remove_tree(&staging);
        return Err(format!(
            "could not move staged checkout into {}",
            src.cache_dir
        ));
    }
    remove_tree(&retired);
    Ok(Publish::Atomic)
}

fn update_checkout_in_place(src: &RepoSource) -> Result<(), String> {
    let cache = src.cache_dir.as_str();
    let reference = src
        .reference
        .as_deref()
        .filter(|r| !r.is_empty())
        .unwrap_or("HEAD");
    git(
        &["-c", "gc.auto=0", "fetch", "--depth", "1", "origin", reference],
        Some(cache),
    )?;
    git(
        &["-c", "gc.auto=0", "checkout", "--force", "FETCH_HEAD"],
        Some(cache),
    )?;
    Ok(())
}

fn materialize(src: &RepoSource, reference: &str, lock: &RefreshLock) -> Result<Publish, String> {
    let staging = staging_dir(src, lock);
    remove_tree(&staging);

    let mut argv: Vec<&str> = vec!["clone", "--depth", "1"];
    let use_branch = !reference.is_empty() && !is_sha_like(reference);
    if use_branch {
        argv.push("--branch");
        argv.push(reference);
    }
    argv.push("--");
    argv.push(&src.repo);
    argv.push(&staging);
    if let Err(e) = git(&argv, None) {
        remove_tree(&staging);
        return Err(e);
    }

    if !reference.is_empty() && !use_branch {
        let cwd = Some(staging.as_str());
        let staged = git(&["fetch", "--depth", "1", "origin", reference], cwd)
            .and_then(|_| git(&["checkout", "--force", "FETCH_HEAD"], cwd));
        if let Err(e) = staged {
            remove_tree(&staging);
            return Err(e);
        }
    }

    publish_staged(src, &staging, lock)
}

fn fetch_to(src: &RepoSource, target_sha: &str, lock: &RefreshLock) -> Result<Publish, String> {
    let reference = src.reference.as_deref().unwrap_or("");
    if !reference.is_empty() && !is_sha_like(reference) {
        return materialize(src, reference, lock);
    }
    materialize(src, target_sha, lock)
}

fn emit_degraded(sha: Option<String>, detail: &str, src: &RepoSource) {
    crate::wasm_dispatch::emit_event(
        "config_sync_degraded",
        json!({
            "repo": src.repo.as_str(),
            "ref": src.reference.as_deref().unwrap_or("HEAD"),
            "cache_dir": src.cache_dir,
            "sha": sha,
            "detail": detail,
        }),
    );
}

fn degraded(sha: Option<String>, detail: String, src: &RepoSource) -> SyncOutcome {
    emit_degraded(sha.clone(), &detail, src);
    SyncOutcome {
        sha,
        changed: false,
        degraded_reason: Some(detail.clone()),
        detail,
    }
}

pub fn ensure_current(src: &RepoSource, debounce_ms: u64, forced: bool) -> Result<SyncOutcome, String> {
    let mut st = read_state(src);
    let now = now_ms();
    let have_local = local_sha(src);

    let last_probe = st.last_checked_ms.max(remembered_probe_ms(src));
    let clock_sane = last_probe > 0 && last_probe <= now;
    if !forced && clock_sane {
        let elapsed = now.saturating_sub(last_probe);
        let required = st.next_probe_delay_ms(debounce_ms);
        if elapsed < required {
            return Ok(SyncOutcome {
                sha: have_local,
                changed: false,
                degraded_reason: st.degraded_reason.clone(),
                detail: format!(
                    "debounced: checked {}ms ago, next probe in {}ms",
                    elapsed,
                    required.saturating_sub(elapsed)
                ),
            });
        }
    }

    let probe_paths = remote_probe_paths(src);
    if let ProbeDecision::Skip { sha } =
        probe_decision(&read_remote_record(&probe_paths), now, forced)
    {
        if have_local.as_deref() == Some(sha.as_str()) {
            return Ok(SyncOutcome {
                sha: have_local,
                changed: false,
                degraded_reason: st.degraded_reason.clone(),
                detail: format!("remote sha {sha} is already recorded machine-wide; no probe needed"),
            });
        }
    }

    let lock = match try_lock(src) {
        LockProbe::Acquired(lock) => lock,
        LockProbe::Held => {
            let detail = "another refresh in progress; serving current checkout".to_string();
            return match have_local {
                Some(sha) => {
                    if forced {
                        let reason = format!(
                            "a forced refresh of {} could not run: {detail}, so the tip it now advertises is unknown",
                            src.repo
                        );
                        emit_degraded(Some(sha), &reason, src);
                        Err(reason)
                    } else {
                        Ok(SyncOutcome {
                            sha: Some(sha),
                            changed: false,
                            degraded_reason: st.degraded_reason.clone(),
                            detail,
                        })
                    }
                }
                None => Err(format!(
                    "another process is cloning {} and no local checkout exists yet",
                    src.repo
                )),
            };
        }
        LockProbe::Failed(reason) => {
            let detail = format!("config refresh lock probe failed ({reason})");
            return match have_local {
                Some(sha) => Ok(degraded(
                    Some(sha),
                    format!("{detail}; serving current checkout"),
                    src,
                )),
                None => {
                    emit_degraded(None, &detail, src);
                    Err(format!("{detail}; no local checkout to fall back to"))
                }
            };
        }
    };

    let result = refresh_locked(src, &mut st, now, have_local.clone(), &lock, forced);
    if unlock(&lock) {
        write_state(src, &st);
    }
    if forced {
        if let Ok(outcome) = &result {
            match (local_sha(src), outcome.sha.clone()) {
                (Some(live), Some(reported)) if live == reported => {}
                (live, reported) => {
                    let reason = format!(
                        "a forced refresh of {} reported {} but left the checkout at {}",
                        src.repo,
                        reported.as_deref().unwrap_or("no sha"),
                        live.as_deref().unwrap_or("no checkout")
                    );
                    emit_degraded(live, &reason, src);
                    return Err(reason);
                }
            }
        }
    }
    result
}

fn refresh_locked(
    src: &RepoSource,
    st: &mut SyncState,
    now: u64,
    have_local: Option<String>,
    lock: &RefreshLock,
    forced: bool,
) -> Result<SyncOutcome, String> {
    if !owns_lock(lock) {
        return Err("config refresh lock ownership was lost before refresh".to_string());
    }
    recover_stranded(src);
    st.last_checked_ms = now;
    remember_probe_ms(src, now);
    let have_local = local_sha(src).or(have_local);

    let remote = match if forced {
        global_remote_tip_forced(src, now)
    } else {
        global_remote_tip(src, now)
    } {
        GlobalTip::Known(sha) => sha,
        GlobalTip::Held(waited) => {
            let detail = format!("another refresh in progress ({waited}); serving current checkout");
            return match have_local {
                Some(sha) => {
                    if forced {
                        let reason = format!(
                            "a forced refresh of {} could not run: {detail}, so the tip it now advertises is unknown",
                            src.repo
                        );
                        emit_degraded(Some(sha), &reason, src);
                        return Err(reason);
                    }
                    Ok(SyncOutcome {
                        sha: Some(sha),
                        changed: false,
                        degraded_reason: st.degraded_reason.clone(),
                        detail,
                    })
                }
                None => Err(format!(
                    "another process is cloning {} and no local checkout exists yet",
                    src.repo
                )),
            };
        }
        GlobalTip::Unavailable(reason) => {
            st.last_checked_ms = now;
            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
            let detail = format!("remote probe failed ({reason})");
            return match have_local {
                Some(sha) => {
                    let detail = format!("{detail}; serving last good checkout");
                    st.degraded_reason = Some(detail.clone());
                    st.degraded_sticky = false;
                    Ok(degraded(Some(sha), detail, src))
                }
                None => {
                    let detail = format!("{reason}; no local checkout to fall back to");
                    st.degraded_reason = Some(detail.clone());
                    st.degraded_sticky = false;
                    emit_degraded(None, &detail, src);
                    Err(detail)
                }
            };
        }
    };

    st.last_checked_ms = now;

    if have_local.as_deref() == Some(remote.as_str()) {
        st.consecutive_failures = 0;
        st.last_sha = remote.clone();
        if !st.degraded_sticky {
            st.degraded_reason = None;
        }
        return Ok(SyncOutcome {
            sha: Some(remote),
            changed: false,
            degraded_reason: st.degraded_reason.clone(),
            detail: "remote sha unchanged; no fetch needed".to_string(),
        });
    }

    let pre_fetch_config_text = crate::pkfs::read_to_string(&src.config_path());

    let publish = match fetch_to(src, &remote, lock) {
        Ok(publish) => publish,
        Err(e) => {
            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
            let attempt = format!("update to {remote} failed ({e})");
            return match local_sha(src) {
                Some(sha) => {
                    let detail = format!("{attempt}; serving checkout at {sha}");
                    st.degraded_reason = Some(detail.clone());
                    st.degraded_sticky = true;
                    Ok(degraded(Some(sha), detail, src))
                }
                None => {
                    st.degraded_reason = Some(attempt.clone());
                    st.degraded_sticky = true;
                    emit_degraded(None, &attempt, src);
                    Err(format!("could not materialize {}: {e}", src.repo))
                }
            };
        }
    };

    st.consecutive_failures = 0;
    let live = local_sha(src);
    st.last_sha = live.clone().unwrap_or_else(|| remote.clone());
    let degraded_reason = match publish {
        Publish::Atomic => {
            st.degraded_reason = None;
            st.degraded_sticky = false;
            None
        }
        Publish::InPlace { reason } => {
            st.degraded_reason = Some(reason.clone());
            st.degraded_sticky = true;
            Some(reason)
        }
    };

    crate::orchestrator::config_notify::record_change(
        &src.tier_label,
        have_local.as_deref().unwrap_or(""),
        live.as_deref().unwrap_or(&remote),
        &changed_config_paths(src, pre_fetch_config_text.as_deref()),
    );

    if let Some(reason) = degraded_reason.as_deref() {
        emit_degraded(live.clone(), reason, src);
    }
    let detail = match &degraded_reason {
        Some(reason) => format!("updated to {remote}; {reason}"),
        None => format!("updated to {remote}"),
    };
    Ok(SyncOutcome {
        sha: live,
        changed: true,
        degraded_reason,
        detail,
    })
}

fn changed_config_paths(src: &RepoSource, pre_fetch_text: Option<&str>) -> Vec<String> {
    let path = src.config_path();
    let Some(pre_text) = pre_fetch_text else {
        return vec![path];
    };
    let Some(post_text) = crate::pkfs::read_to_string(&path) else {
        return vec![path];
    };
    let (Ok(serde_json::Value::Object(pre)), Ok(serde_json::Value::Object(post))) = (
        serde_json::from_str::<serde_json::Value>(&pre_text),
        serde_json::from_str::<serde_json::Value>(&post_text),
    ) else {
        return vec![path];
    };
    let mut keys: Vec<String> = Vec::new();
    for (k, post_v) in post.iter() {
        match pre.get(k) {
            Some(pre_v) if pre_v == post_v => {}
            Some(_) => keys.push(format!("{k} (changed)")),
            None => keys.push(format!("{k} (added)")),
        }
    }
    for k in pre.keys() {
        if !post.contains_key(k) {
            keys.push(format!("{k} (removed)"));
        }
    }
    if keys.is_empty() {
        vec![path]
    } else {
        keys
    }
}

pub struct GitRepoFetcher {
    pub debounce_ms: u64,
    pub forced: bool,
}

impl GitRepoFetcher {
    pub fn new() -> GitRepoFetcher {
        GitRepoFetcher {
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            forced: false,
        }
    }

    pub fn with_debounce_ms(debounce_ms: u64) -> GitRepoFetcher {
        GitRepoFetcher {
            debounce_ms,
            forced: false,
        }
    }

    pub fn forced() -> GitRepoFetcher {
        GitRepoFetcher {
            debounce_ms: 0,
            forced: true,
        }
    }
}

impl Default for GitRepoFetcher {
    fn default() -> Self {
        GitRepoFetcher::new()
    }
}

impl RepoFetcher for GitRepoFetcher {
    fn refresh(&self, src: &RepoSource) -> Result<Freshness, String> {
        let outcome = ensure_current(src, self.debounce_ms, self.forced)?;
        Ok(match outcome.degraded_reason {
            Some(reason) => Freshness::Degraded { reason },
            None => Freshness::Current,
        })
    }
}
