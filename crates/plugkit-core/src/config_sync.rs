use serde_json::{json, Value};

use crate::config::{RepoFetcher, RepoSource};

const DEFAULT_DEBOUNCE_MS: u64 = 15 * 60 * 1000;

const BACKOFF_BASE_MS: u64 = 60 * 1000;

const BACKOFF_MAX_MS: u64 = 60 * 60 * 1000;

const LOCK_STALE_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct SyncOutcome {
    pub sha: Option<String>,
    pub changed: bool,
    pub degraded: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
struct SyncState {
    last_checked_ms: u64,
    last_sha: String,
    consecutive_failures: u32,
}

impl SyncState {
    fn parse(raw: &str) -> SyncState {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return SyncState::default(),
        };
        SyncState {
            last_checked_ms: v.get("last_checked_ms").and_then(|x| x.as_u64()).unwrap_or(0),
            last_sha: v.get("last_sha").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            consecutive_failures: v
                .get("consecutive_failures")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
        }
    }

    fn to_json(&self) -> String {
        json!({
            "last_checked_ms": self.last_checked_ms,
            "last_sha": self.last_sha,
            "consecutive_failures": self.consecutive_failures,
        })
        .to_string()
    }

    fn next_probe_delay_ms(&self, debounce_ms: u64) -> u64 {
        if self.consecutive_failures == 0 {
            return debounce_ms;
        }
        let exp = self.consecutive_failures.min(20);
        let backoff = BACKOFF_BASE_MS.saturating_mul(1u64 << exp).min(BACKOFF_MAX_MS);
        backoff.max(debounce_ms)
    }
}

fn now_ms() -> u64 {
    crate::orchestrator::state::now_ms() as u64
}

fn source_key(src: &RepoSource) -> String {
    let ident = format!("{}\u{0}{}", src.repo, src.reference.as_deref().unwrap_or(""));
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

fn read_state(src: &RepoSource) -> SyncState {
    match crate::pkfs::read_to_string(&state_path(src)) {
        Some(raw) => SyncState::parse(&raw),
        None => SyncState::default(),
    }
}

fn write_state(src: &RepoSource, st: &SyncState) {
    let path = state_path(src);
    let tmp = format!("{}.tmp-{}", path, now_ms());
    if !crate::pkfs::write(&tmp, &st.to_json()) {
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
    exec_js_stdout(&code, 15000).map(|s| s.contains("ok")).unwrap_or(false)
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
    parsed.get("stdout").and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn try_lock(src: &RepoSource) -> bool {
    let path = lock_path(src);
    let Ok(p) = serde_json::to_string(&path) else {
        return false;
    };
    let Ok(parent_p) = serde_json::to_string(&cache_root(src)) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');const p={p};const staleMs={LOCK_STALE_MS};\
         const parentDir=require('path').dirname({parent_p});\
         try{{fs.mkdirSync(parentDir,{{recursive:true}});}}catch(e0){{}}\
         process.stdout.write((function(){{\
         try{{fs.mkdirSync(p);return 'acquired';}}catch(e){{}}\
         let st=null;try{{st=fs.statSync(p);}}catch(e2){{return 'busy';}}\
         if(Date.now()-st.mtimeMs<=staleMs){{return 'busy';}}\
         const aside=p+'.stale-'+process.pid+'-'+Date.now();\
         try{{fs.renameSync(p,aside);}}catch(e3){{return 'busy';}}\
         try{{fs.rmSync(aside,{{recursive:true,force:true}});}}catch(e4){{}}\
         try{{fs.mkdirSync(p);return 'acquired';}}catch(e5){{return 'busy';}}\
         }})());"
    );
    exec_js_stdout(&code, 15000).map(|s| s.contains("acquired")).unwrap_or(false)
}

fn unlock(src: &RepoSource) {
    let path = lock_path(src);
    let Ok(p) = serde_json::to_string(&path) else {
        return;
    };
    let code = format!(
        "const fs=require('fs');try{{fs.rmSync({p},{{recursive:true,force:true}});}}catch(e){{}}process.stdout.write('done');"
    );
    let _ = exec_js_stdout(&code, 15000);
}

fn git(argv: &[&str], cwd: Option<&str>) -> Result<String, String> {
    let v = crate::wasm_dispatch::git_call_argv(argv, cwd);
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if !ok || code != 0 {
        let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        let msg = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        return Err(format!("git {} failed: {}", argv.first().copied().unwrap_or("?"), msg));
    }
    Ok(stdout)
}

fn local_sha(src: &RepoSource) -> Option<String> {
    let out = git(&["rev-parse", "HEAD"], Some(&cache_root(src))).ok()?;
    let s = out.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
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
    Err(format!("remote {} advertises no ref matching {}", src.repo, reference))
}

fn is_sha_like(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn staging_dir(src: &RepoSource) -> String {
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
    exec_js_stdout(&code, 30000).map(|s| s.contains("ok")).unwrap_or(false)
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
    let staging = staging_dir(src);
    if crate::pkfs::exists(&staging) {
        remove_tree(&staging);
    }
}

fn publish_staged(src: &RepoSource) -> Result<(), String> {
    let staging = staging_dir(src);
    let retired = retired_dir(src);
    remove_tree(&retired);

    let had_live = crate::pkfs::exists(&src.cache_dir);
    if had_live && !rename(&src.cache_dir, &retired) {
        remove_tree(&staging);
        return Err(format!("could not move {} aside to publish a new checkout", src.cache_dir));
    }
    if !rename(&staging, &src.cache_dir) {
        if had_live {
            rename(&retired, &src.cache_dir);
        }
        remove_tree(&staging);
        return Err(format!("could not move staged checkout into {}", src.cache_dir));
    }
    remove_tree(&retired);
    Ok(())
}

fn materialize(src: &RepoSource, reference: &str) -> Result<(), String> {
    let staging = staging_dir(src);
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

    publish_staged(src)
}

fn fetch_to(src: &RepoSource, target_sha: &str) -> Result<(), String> {
    let reference = src.reference.as_deref().unwrap_or("");
    if !reference.is_empty() && !is_sha_like(reference) {
        return materialize(src, reference);
    }
    materialize(src, target_sha)
}

fn degraded(sha: Option<String>, detail: String, src: &RepoSource) -> SyncOutcome {
    crate::wasm_dispatch::emit_event(
        "config_sync_degraded",
        json!({
            "repo": src.repo,
            "ref": src.reference.as_deref().unwrap_or("HEAD"),
            "cache_dir": src.cache_dir,
            "sha": sha,
            "detail": detail,
        }),
    );
    SyncOutcome { sha, changed: false, degraded: true, detail }
}

pub fn ensure_current(src: &RepoSource, debounce_ms: u64) -> Result<SyncOutcome, String> {
    crate::config_path::validate_repo_url(&src.repo)?;

    let mut st = read_state(src);
    let now = now_ms();
    let have_local = local_sha(src);

    if have_local.is_some() {
        let elapsed = now.saturating_sub(st.last_checked_ms);
        let required = st.next_probe_delay_ms(debounce_ms);
        let clock_sane = st.last_checked_ms <= now;
        if clock_sane && elapsed < required {
            return Ok(SyncOutcome {
                sha: have_local,
                changed: false,
                degraded: false,
                detail: format!(
                    "debounced: checked {}ms ago, next probe in {}ms",
                    elapsed,
                    required.saturating_sub(elapsed)
                ),
            });
        }
    }

    if !try_lock(src) {
        return match have_local {
            Some(sha) => Ok(SyncOutcome {
                sha: Some(sha),
                changed: false,
                degraded: false,
                detail: "another refresh in progress; serving current checkout".to_string(),
            }),
            None => Err(format!(
                "another process is cloning {} and no local checkout exists yet",
                src.repo
            )),
        };
    }

    let result = refresh_locked(src, &mut st, now, have_local.clone());
    unlock(src);
    write_state(src, &st);
    result
}

fn refresh_locked(
    src: &RepoSource,
    st: &mut SyncState,
    now: u64,
    have_local: Option<String>,
) -> Result<SyncOutcome, String> {
    recover_stranded(src);
    let have_local = local_sha(src).or(have_local);

    let remote = match probe_remote_sha(src) {
        Ok(sha) => sha,
        Err(e) => {
            st.last_checked_ms = now;
            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
            return match have_local {
                Some(sha) => Ok(degraded(
                    Some(sha),
                    format!("remote probe failed ({e}); serving last good checkout"),
                    src,
                )),
                None => Err(format!("{e}; no local checkout to fall back to")),
            };
        }
    };

    st.last_checked_ms = now;

    if have_local.as_deref() == Some(remote.as_str()) {
        st.consecutive_failures = 0;
        st.last_sha = remote.clone();
        return Ok(SyncOutcome {
            sha: Some(remote),
            changed: false,
            degraded: false,
            detail: "remote sha unchanged; no fetch needed".to_string(),
        });
    }

    let pre_fetch_config_text = crate::pkfs::read_to_string(&src.config_path());

    let outcome = fetch_to(src, &remote);

    if let Err(e) = outcome {
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        return match local_sha(src) {
            Some(sha) => Ok(degraded(
                Some(sha),
                format!("update to {remote} failed ({e}); serving previous checkout"),
                src,
            )),
            None => Err(format!("could not materialize {}: {e}", src.repo)),
        };
    }

    st.consecutive_failures = 0;
    let live = local_sha(src);
    st.last_sha = live.clone().unwrap_or_else(|| remote.clone());

    crate::orchestrator::config_notify::record_change(
        &src.tier_label,
        have_local.as_deref().unwrap_or(""),
        live.as_deref().unwrap_or(&remote),
        &changed_config_paths(src, pre_fetch_config_text.as_deref()),
    );

    Ok(SyncOutcome {
        sha: live,
        changed: true,
        degraded: false,
        detail: format!("updated to {remote}"),
    })
}

fn changed_config_paths(src: &RepoSource, pre_fetch_text: Option<&str>) -> Vec<String> {
    let path = src.config_path();
    let Some(pre_text) = pre_fetch_text else { return vec![path] };
    let Some(post_text) = crate::pkfs::read_to_string(&path) else { return vec![path] };
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
    if keys.is_empty() { vec![path] } else { keys }
}

pub struct GitRepoFetcher {
    pub debounce_ms: u64,
}

impl GitRepoFetcher {
    pub fn new() -> GitRepoFetcher {
        GitRepoFetcher { debounce_ms: DEFAULT_DEBOUNCE_MS }
    }

    pub fn with_debounce_ms(debounce_ms: u64) -> GitRepoFetcher {
        GitRepoFetcher { debounce_ms }
    }
}

impl Default for GitRepoFetcher {
    fn default() -> Self {
        GitRepoFetcher::new()
    }
}

impl RepoFetcher for GitRepoFetcher {
    fn refresh(&self, src: &RepoSource) -> Result<(), String> {
        ensure_current(src, self.debounce_ms).map(|_| ())
    }
}
