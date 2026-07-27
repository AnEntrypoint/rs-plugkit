//! Git-backed materialization of repo-sourced config: the [`RepoFetcher`]
//! implementation `config.rs` declares as a seam and deliberately leaves empty.
//!
//! [`config::load_repo_tier`] calls `refresh` on the resolution path, which
//! means this code runs on config reads -- potentially many per session, across
//! every project sharing the process-wide plugin instance. Everything below is
//! shaped by that one fact: the common case must cost approximately nothing,
//! and no failure here may take the session down with it.
//!
//! # Why a remote probe instead of a fetch
//!
//! `git fetch` transfers objects. `git ls-remote` transfers one line per ref
//! and never writes to the object store, so it is the cheap way to answer the
//! only question a refresh actually asks: "did the remote sha move?" We fetch
//! ONLY when the answer is yes. On an unchanged remote -- overwhelmingly the
//! common case, since config repos change rarely and refresh is called often --
//! the cost is one ref advertisement, not a pack negotiation.
//!
//! The clone/fetch we do issue is `--depth 1`: config resolution reads a
//! worktree at one commit and never inspects history, so downloading it would
//! be pure waste.
//!
//! # Failure is never fatal, but never silent either
//!
//! A probe needs the network, and the network is not available in a plane, a
//! CI sandbox, or a coffee shop. A refresh that failed but has a usable prior
//! checkout returns `Ok` -- resolution proceeds against the last good copy --
//! and emits a `config_sync_degraded` event so the staleness is visible in the
//! watcher log rather than inferred later from confusing config behavior. Only
//! a failure with NO local copy at all returns `Err`, because at that point
//! there is genuinely nothing to resolve and `config.rs` must reject the tier
//! rather than fall through to a lower one (see its `load_repo_tier` docs).
//!
//! # Backoff
//!
//! Debounce alone does not protect a dead remote: a 15-minute debounce still
//! probes a permanently-unreachable host every 15 minutes forever, and each of
//! those probes blocks a config read for however long the host's git takes to
//! time out. Consecutive failures therefore extend the retry delay
//! exponentially ([`BACKOFF_BASE_MS`] doubling up to [`BACKOFF_MAX_MS`]), and
//! any success resets it. A transient outage costs one slow probe; a dead
//! remote decays to roughly hourly.
//!
//! # Concurrency
//!
//! The plugin instance is shared across concurrently-active projects, and
//! `config.rs` points every project's USER tier at one shared cache dir under
//! `$HOME` -- so two projects can genuinely refresh the same source at the same
//! moment. Two mechanisms cover it:
//!
//! - **State** is published by write-to-temp + atomic rename, so a concurrent
//!   reader sees either the old state or the new one, never a half-written
//!   file. (A plain overwrite can be observed torn, which would look like
//!   corrupt JSON and silently reset the debounce.)
//! - **Git work** is guarded by a lock directory whose creation is atomic
//!   (`mkdir` fails if it exists). A loser does not wait or fail; it proceeds
//!   with the existing checkout, which is exactly the degraded-but-usable path
//!   the offline case already takes. A lock older than [`LOCK_STALE_MS`] is
//!   broken, since a process killed mid-refresh would otherwise wedge the
//!   source permanently.
//!
//! In-process statics are deliberately NOT used for any of this: they would be
//! shared across projects that must not share debounce state, and would be
//! invisible to the separate host processes that can also run this code.

use serde_json::{json, Value};

use crate::config::{RepoFetcher, RepoSource};

/// Minimum spacing between remote probes for one source. A config repo that
/// changes more often than this is pathological; anything shorter turns a
/// burst of config reads into a burst of network round-trips.
const DEFAULT_DEBOUNCE_MS: u64 = 15 * 60 * 1000;

/// First retry delay after a failure, doubled per consecutive failure.
const BACKOFF_BASE_MS: u64 = 60 * 1000;

/// Ceiling for the exponential backoff. An hour keeps a dead remote from
/// costing anything measurable while still recovering on its own once the
/// network returns -- no session restart required.
const BACKOFF_MAX_MS: u64 = 60 * 60 * 1000;

/// Age past which a lock directory is presumed abandoned. Generous relative to
/// a shallow clone so a merely-slow refresh is never stolen from, but bounded
/// so a killed process cannot wedge a source forever.
const LOCK_STALE_MS: u64 = 10 * 60 * 1000;

/// Outcome of [`ensure_current`], for callers that want more than the
/// `Result<(), String>` the [`RepoFetcher`] trait can carry.
#[derive(Debug, Clone)]
pub struct SyncOutcome {
    /// Sha now live in the cache dir. `None` only when the checkout is
    /// unreadable, which for a usable cache should not happen.
    pub sha: Option<String>,
    /// Whether this call moved the worktree. False for a debounced call, an
    /// up-to-date probe, or a degraded fall back to the prior copy.
    pub changed: bool,
    /// True when the remote could not be reached (or git failed) and a prior
    /// local copy is being served instead. The config is usable but may be
    /// stale.
    pub degraded: bool,
    /// Human-readable reason, always populated -- "why did config not update"
    /// is unanswerable from a bare bool.
    pub detail: String,
}

/// Persisted per-source sync state. Kept beside the checkout rather than in KV
/// because it must survive alongside exactly the thing it describes: a cache
/// dir deleted by hand should not leave stale state claiming a sha is current.
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
            // A corrupt state file must not be fatal: the worst case of
            // treating it as absent is one extra probe, whereas propagating a
            // parse error would break config resolution over a cache artifact.
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

    /// Delay that must elapse before the next probe. Backoff replaces the
    /// debounce (rather than adding to it) once failures start, and is always
    /// at least the debounce so a failing source is never probed MORE often
    /// than a healthy one.
    fn next_probe_delay_ms(&self, debounce_ms: u64) -> u64 {
        if self.consecutive_failures == 0 {
            return debounce_ms;
        }
        // Saturating shift: a large failure count must not wrap the delay back
        // to zero and turn backoff into a hot loop. Cap the exponent well
        // below u64's bit width, then clamp.
        let exp = self.consecutive_failures.min(20);
        let backoff = BACKOFF_BASE_MS.saturating_mul(1u64 << exp).min(BACKOFF_MAX_MS);
        backoff.max(debounce_ms)
    }
}

fn now_ms() -> u64 {
    crate::orchestrator::state::now_ms() as u64
}

/// Stable identity for a source, used to name its state file.
///
/// Hashed rather than derived from the URL text because a repo URL contains
/// characters that are illegal or meaningful in a path (`:`, `/`, `@`), and
/// because two specs differing only in `ref` must not share state -- a probe
/// result for `main` says nothing about `v2`.
fn source_key(src: &RepoSource) -> String {
    let ident = format!("{}\u{0}{}", src.repo, src.reference.as_deref().unwrap_or(""));
    format!("{:016x}", crate::pipeline::fnv1a64(ident.as_bytes()))
}

fn state_path(src: &RepoSource) -> String {
    // Sibling of the cache dir, not inside it: a `git clean` or a re-clone that
    // wipes the checkout would otherwise take the debounce record with it and
    // reset the backoff a dead remote just earned.
    format!("{}.{}.sync.json", src.cache_dir, source_key(src))
}

fn lock_path(src: &RepoSource) -> String {
    format!("{}.{}.lock", src.cache_dir, source_key(src))
}

fn read_state(src: &RepoSource) -> SyncState {
    match crate::pkfs::read_to_string(&state_path(src)) {
        Some(raw) => SyncState::parse(&raw),
        None => SyncState::default(),
    }
}

/// Publish state by atomic rename.
///
/// Two projects can write this concurrently; rename makes each write
/// all-or-nothing, so the loser's state is simply overwritten by the winner
/// rather than interleaved into unparseable bytes.
fn write_state(src: &RepoSource, st: &SyncState) {
    let path = state_path(src);
    let tmp = format!("{}.tmp-{}", path, now_ms());
    if !crate::pkfs::write(&tmp, &st.to_json()) {
        return;
    }
    if !rename(&tmp, &path) {
        // Leaving a stray temp file behind would accumulate one per failed
        // publish, so drop it; losing the state update only costs an extra
        // probe next call.
        let _ = crate::wasm_dispatch::host_remove(&tmp);
    }
}

/// `fs.renameSync` via the host's JS escape hatch.
///
/// There is no rename in the host ABI (`host_fs_write` overwrites in place,
/// which is exactly the torn-write this must avoid), so this mirrors the
/// approach `memory_md.rs::rename_batch` already uses for the same reason.
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

/// Acquire the per-source lock, breaking it if abandoned.
///
/// `mkdir` (non-recursive) is the primitive: it fails if the directory exists,
/// which makes creation an atomic test-and-set across processes. `fs.writeFile`
/// would not work here -- it succeeds unconditionally and would hand the lock
/// to every caller at once.
fn try_lock(src: &RepoSource) -> bool {
    let path = lock_path(src);
    let Ok(p) = serde_json::to_string(&path) else {
        return false;
    };
    let code = format!(
        "const fs=require('fs');const p={p};const staleMs={LOCK_STALE_MS};\
         try{{fs.mkdirSync(p);process.stdout.write('acquired');}}catch(e){{\
         try{{const st=fs.statSync(p);\
         if(Date.now()-st.mtimeMs>staleMs){{fs.rmSync(p,{{recursive:true,force:true}});\
         fs.mkdirSync(p);process.stdout.write('acquired');}}\
         else{{process.stdout.write('busy');}}}}catch(e2){{process.stdout.write('busy');}}}}"
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
    // `ok` absent defaults true and `exit_code` absent defaults 0, matching
    // how code_index.rs and verbs.rs read this same envelope -- a host that
    // omits the fields on success must not be read as a failure.
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

/// Whether the cache dir holds a git checkout we can serve.
///
/// Checked via `rev-parse` rather than a bare directory-exists test: a dir left
/// half-written by an interrupted clone exists but has no HEAD, and serving it
/// as "the last good copy" would surface an empty config as if it were real.
fn local_sha(src: &RepoSource) -> Option<String> {
    let out = git(&["rev-parse", "HEAD"], Some(&src.cache_dir)).ok()?;
    let s = out.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Ask the remote for one ref's sha without transferring objects.
///
/// The ref defaults to `HEAD` (the remote's default branch) when the spec
/// names none, which matches `RepoSource::reference`'s documented meaning.
fn probe_remote_sha(src: &RepoSource) -> Result<String, String> {
    let reference = src.reference.as_deref().unwrap_or("HEAD");
    let out = git(&["ls-remote", "--", &src.repo, reference], None)?;
    // Output is "<sha>\t<refname>" per line. A ref matching nothing yields
    // empty output with exit 0, so emptiness is a real error, not a success.
    if let Some(sha) = out.split_whitespace().next() {
        if !sha.is_empty() {
            return Ok(sha.to_string());
        }
    }
    // A sha passed as `ref` is not advertised by ls-remote (only refs are), so
    // an exact-40-hex spec legitimately probes empty. Treat it as its own
    // answer rather than an error, since the sha IS the target.
    if is_sha_like(reference) {
        return Ok(reference.to_string());
    }
    Err(format!("remote {} advertises no ref matching {}", src.repo, reference))
}

fn is_sha_like(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Shallow-clone a source that has no usable local checkout.
fn clone_shallow(src: &RepoSource) -> Result<(), String> {
    // A previous attempt may have left a partial directory that would make
    // `git clone` refuse ("destination path already exists"); clear it first so
    // recovery does not need manual intervention.
    if let Ok(p) = serde_json::to_string(&src.cache_dir) {
        let code = format!(
            "const fs=require('fs');try{{fs.rmSync({p},{{recursive:true,force:true}});}}catch(e){{}}process.stdout.write('done');"
        );
        let _ = exec_js_stdout(&code, 30000);
    }
    let mut argv: Vec<&str> = vec!["clone", "--depth", "1"];
    // `--branch` accepts a branch or tag but NOT a raw sha, so a sha-pinned
    // spec clones the default branch and is moved onto its commit below.
    let reference = src.reference.as_deref().unwrap_or("");
    let use_branch = !reference.is_empty() && !is_sha_like(reference);
    if use_branch {
        argv.push("--branch");
        argv.push(reference);
    }
    argv.push("--");
    argv.push(&src.repo);
    argv.push(&src.cache_dir);
    git(&argv, None)?;
    if !reference.is_empty() && !use_branch {
        fetch_and_checkout_sha(src, reference)?;
    }
    Ok(())
}

/// Fetch one commit shallowly and move the worktree onto it.
fn fetch_and_checkout_sha(src: &RepoSource, sha: &str) -> Result<(), String> {
    let cwd = Some(src.cache_dir.as_str());
    git(&["fetch", "--depth", "1", "origin", sha], cwd)?;
    // FETCH_HEAD rather than the sha: fetching a sha into a shallow repo does
    // not always create a local ref for it, but FETCH_HEAD always names what
    // was just fetched.
    git(&["checkout", "--force", "FETCH_HEAD"], cwd)?;
    Ok(())
}

/// Update an existing checkout to `target_sha`, staying shallow.
fn fetch_to(src: &RepoSource, target_sha: &str) -> Result<(), String> {
    let cwd = Some(src.cache_dir.as_str());
    let reference = src.reference.as_deref().unwrap_or("");
    if !reference.is_empty() && !is_sha_like(reference) {
        // Named ref: fetch it by name so the shallow history stays anchored to
        // the branch the spec asked for.
        git(&["fetch", "--depth", "1", "origin", reference], cwd)?;
        git(&["checkout", "--force", "FETCH_HEAD"], cwd)?;
        return Ok(());
    }
    fetch_and_checkout_sha(src, target_sha)
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

/// Ensure `src.cache_dir` holds an up-to-date checkout, reporting the live sha
/// and whether this call changed it.
///
/// This is the function the config resolver calls. It is safe to call on every
/// resolution: the debounce and backoff mean the overwhelming majority of calls
/// do no network work at all.
///
/// Returns `Err` ONLY when there is no usable local copy AND the remote could
/// not supply one -- the single case where `config.rs` must reject the tier
/// instead of resolving against a stale-but-real config.
pub fn ensure_current(src: &RepoSource, debounce_ms: u64) -> Result<SyncOutcome, String> {
    let mut st = read_state(src);
    let now = now_ms();
    let have_local = local_sha(src);

    // Debounce: a recent check plus a usable checkout means there is nothing
    // worth spending a round-trip on. Skipped entirely when the cache is cold,
    // since no debounce interval justifies serving a config that does not exist.
    if have_local.is_some() {
        let elapsed = now.saturating_sub(st.last_checked_ms);
        let required = st.next_probe_delay_ms(debounce_ms);
        // `last_checked_ms` in the future means the clock moved backwards
        // (NTP correction, a VM restore); treat it as due rather than
        // trusting a timestamp that would suppress probes for a long time.
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

    // Only one refresh per source at a time. A loser serves the existing
    // checkout -- the holder is already doing the work, so waiting would only
    // add latency to a config read.
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

/// The refresh body, run under the lock. Split out so every early return still
/// releases the lock and persists state in [`ensure_current`].
fn refresh_locked(
    src: &RepoSource,
    st: &mut SyncState,
    now: u64,
    have_local: Option<String>,
) -> Result<SyncOutcome, String> {
    // Re-read the checkout now that the lock is held: another process may have
    // completed a clone between our first look and acquiring the lock, in which
    // case there is nothing left to do.
    let have_local = have_local.or_else(|| local_sha(src));

    let remote = match probe_remote_sha(src) {
        Ok(sha) => sha,
        Err(e) => {
            st.last_checked_ms = now;
            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
            return match have_local {
                // Offline with a usable copy: proceed, loudly.
                Some(sha) => Ok(degraded(
                    Some(sha),
                    format!("remote probe failed ({e}); serving last good checkout"),
                    src,
                )),
                // Offline with nothing cached: the tier genuinely cannot
                // resolve, so report it rather than pretend.
                None => Err(format!("{e}; no local checkout to fall back to")),
            };
        }
    };

    st.last_checked_ms = now;

    // The whole point of the probe: an unchanged remote costs nothing further.
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

    let outcome = match have_local {
        Some(_) => fetch_to(src, &remote),
        None => clone_shallow(src),
    };

    if let Err(e) = outcome {
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        // Re-read rather than reusing the pre-fetch value: a failed fetch may
        // still have moved HEAD, and the sha we report must be the one on disk.
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

    // The ONLY place both shas exist at once, and therefore the only place a
    // change notification can be produced. config_notify::record_change had no
    // caller at all before this: changes were recorded nowhere, so the drain on
    // the instruction payload could only ever return empty and a running agent
    // could never learn its configuration had moved.
    crate::orchestrator::config_notify::record_change(
        &src.tier_label,
        have_local.as_deref().unwrap_or(""),
        live.as_deref().unwrap_or(&remote),
        &changed_config_paths(src),
    );

    Ok(SyncOutcome {
        sha: live,
        changed: true,
        degraded: false,
        detail: format!("updated to {remote}"),
    })
}

/// Config-relevant files this source supplies, for the change roster.
///
/// Deliberately the SPEC's own view (which config file this source resolves to)
/// rather than a full `git diff --name-only`: the roster exists to tell an agent
/// WHICH config moved, and a diff of an entire config repo would bury that in
/// unrelated files. A key-level old->new diff is a separate, finer row.
fn changed_config_paths(src: &RepoSource) -> Vec<String> {
    vec![src.config_path()]
}

/// The [`RepoFetcher`] `config.rs` resolves through.
///
/// Holds the debounce interval so a caller can tighten it (a config-editing
/// workflow may want near-immediate pickup) without touching resolution.
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
        // Discards the outcome detail because the trait's contract is only
        // "config_path() is readable if the repo has it". The degraded case
        // already emitted its own event, so nothing is lost here; callers
        // wanting sha/changed call `ensure_current` directly.
        ensure_current(src, self.debounce_ms).map(|_| ())
    }
}
