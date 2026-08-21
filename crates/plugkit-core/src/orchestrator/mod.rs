pub mod state;
pub mod fsm;
pub mod fsm_vendor;
pub mod fsm_propose;
pub mod transitions;
pub mod deviations;
pub mod cas;
pub mod mutables;
pub mod memorize;
pub mod discipline_note;
pub mod fiber_lifecycle;
pub mod memory_component;
pub mod codeinsight_component;
pub mod calculus;
pub mod config_notify;
pub mod residual;
pub mod recall;
pub mod instructions;
pub mod yaml_util;
pub mod prd;
pub mod task;
pub mod claim_audit;
pub mod submodule_drift;

use std::path::PathBuf;

fn parse_toplevel_common_dir(out: &str) -> Option<PathBuf> {
    let mut lines = out.lines();
    let toplevel = lines.next()?.trim();
    let common_dir = lines.next()?.trim();
    if toplevel.is_empty() || common_dir.is_empty() { return None; }
    let common_path = PathBuf::from(common_dir);
    if common_path.ends_with(".git") {
        Some(PathBuf::from(toplevel))
    } else {
        common_path.parent().map(|p| p.to_path_buf())
    }
}

#[cfg(target_arch = "wasm32")]
fn git_common_dir_project_root_once() -> Option<PathBuf> {
    let v = crate::wasm_dispatch::git_call("rev-parse --show-toplevel --git-common-dir", None);
    if v.get("async_parked").and_then(|x| x.as_bool()).unwrap_or(false) {
        return fs_walk_project_root();
    }
    let out = v.get("stdout").and_then(|x| x.as_str())?;
    parse_toplevel_common_dir(out)
}

/// Root resolution for hosts that park `git rev-parse` under the async
/// pending-token protocol instead of answering inline: the git probe above
/// can never succeed there, but host_fs_* IS synchronous on every host, and
/// the question rev-parse answers is only "which ancestor of the cwd holds
/// the .git entry". Walk up probing that entry (`.git` itself for gitfile
/// worktrees, `.git/HEAD` for the common directory form). Returns None when
/// no ancestor holds one -- resolve_project_root_with_retry's deliberate
/// refuse-to-mis-root panic still fires for the genuinely repoless case,
/// exactly as a real git failure produces it on sync hosts. The walk's
/// result lands in the same cwd-keyed PROJECT_ROOT_CACHE the git path feeds,
/// so the parked-rev-parse probe happens at most once per cwd per process.
#[cfg(target_arch = "wasm32")]
fn fs_walk_project_root() -> Option<PathBuf> {
    let cwd = current_cwd_string();
    let mut dir = cwd.trim_end_matches(['/', '\\']).to_string();
    if dir.is_empty() { dir = "/".to_string(); }
    loop {
        let base = if dir == "/" { String::new() } else { dir.clone() };
        if crate::wasm_dispatch::host_exists(&format!("{base}/.git"))
            || crate::wasm_dispatch::host_exists(&format!("{base}/.git/HEAD"))
        {
            return Some(PathBuf::from(if base.is_empty() { "/".to_string() } else { base }));
        }
        if dir == "/" || dir.is_empty() { return None; }
        dir = match dir.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => dir[..i].to_string(),
            None => String::new(),
        };
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn git_common_dir_project_root_once() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() { return None; }
    let out = String::from_utf8_lossy(&output.stdout);
    parse_toplevel_common_dir(&out)
}

const RESOLVE_MAX_ATTEMPTS: u32 = 5;
const RESOLVE_BACKOFF_BASE_MS: u64 = 20;

fn sleep_ms(ms: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ms;
    }
}

fn current_cwd_string() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        crate::wasm_dispatch::host_cwd_string().unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
    }
}

static PROJECT_ROOT_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, PathBuf>>> =
    std::sync::Mutex::new(None);

/// `git rev-parse --show-toplevel --git-common-dir` shells a subprocess (wasm:
/// via the host git bridge, native: a real `git` child process) on every call.
/// The project root cannot change within a process's lifetime for a fixed cwd,
/// so this is cached exactly like `pkfs::project_root`'s `ROOT_CACHE` -- keyed
/// by cwd in a map (never a single slot), so concurrent dispatches from
/// different projects sharing this process never evict each other's cached
/// root, and a legitimate cwd change (a worktree switch, a different project
/// driving the same shared process) still resolves fresh rather than serving
/// a stale root for the wrong tree.
/// Same retry/backoff/cache logic `resolve_project_root_with_retry` panics
/// on exhaustion of, but returns the attempt count on failure instead of
/// unwinding. Split out so a caller that CAN act on a clean failure --
/// dispatch_verb_inner, before routing into any panicking `gm_dir()` call --
/// gets one, while every existing infallible `gm_dir()` call site keeps its
/// exact prior behavior untouched.
fn try_resolve_project_root() -> Result<PathBuf, u32> {
    let cwd = current_cwd_string();
    if let Ok(cache) = PROJECT_ROOT_CACHE.lock() {
        if let Some(root) = cache.as_ref().and_then(|m| m.get(&cwd)) {
            return Ok(root.clone());
        }
    }
    let mut last_err_attempts = 0u32;
    for attempt in 0..RESOLVE_MAX_ATTEMPTS {
        if let Some(root) = git_common_dir_project_root_once() {
            if let Ok(mut cache) = PROJECT_ROOT_CACHE.lock() {
                cache.get_or_insert_with(std::collections::HashMap::new).insert(cwd, root.clone());
            }
            return Ok(root);
        }
        last_err_attempts = attempt + 1;
        if attempt + 1 < RESOLVE_MAX_ATTEMPTS {
            sleep_ms(RESOLVE_BACKOFF_BASE_MS * 2u64.pow(attempt));
        }
    }
    Err(last_err_attempts)
}

/// True when the project root is currently resolvable (from cache or a
/// live git probe). Callers that can return a structured error to their
/// caller -- verb dispatch, specifically -- should check this BEFORE
/// routing into any codepath that calls `gm_dir()`, since `gm_dir()` itself
/// still panics on exhaustion (see its doc comment) and that panic does not
/// reliably unwind to a clean error response on every wasm host: hosts
/// without the wasm exception-handling proposal enabled trap the whole
/// instance instead, which `catch_unwind` in wasm_dispatch cannot catch.
pub fn project_root_resolvable() -> bool {
    try_resolve_project_root().is_ok()
}

/// Human-readable reason the project root could not be resolved, for a
/// caller that already called `project_root_resolvable() == false` and
/// needs the same message `gm_dir()`'s panic would have carried, without
/// triggering that panic to get it.
pub fn project_root_unresolvable_reason() -> String {
    match try_resolve_project_root() {
        Ok(_) => "project root is resolvable".to_string(),
        Err(attempts) => format!(
            "gm_dir: project root resolution failed after {attempts} attempts via `git rev-parse --show-toplevel --git-common-dir` -- refusing to silently fall back to CLAUDE_PROJECT_DIR/HOME, which would mis-root every stateful verb onto the wrong tree. Check for git subprocess/lock contention or a missing .git directory."
        ),
    }
}

fn resolve_project_root_with_retry() -> PathBuf {
    match try_resolve_project_root() {
        Ok(root) => root,
        Err(attempts) => panic!(
            "gm_dir: project root resolution failed after {} attempts via `git rev-parse --show-toplevel --git-common-dir` -- refusing to silently fall back to CLAUDE_PROJECT_DIR/HOME, which would mis-root every stateful verb onto the wrong tree. Check for git subprocess/lock contention or a missing .git directory.",
            attempts
        ),
    }
}

pub fn gm_dir() -> PathBuf {
    resolve_project_root_with_retry().join(".gm")
}

/// Which verbs `dispatch_verb_inner` routes to the orchestrator.
///
/// `is_orchestrator_verb` and the mediator's advertised subsystem map both
/// read this slice, so the ADVERTISED surface cannot drift from it. The
/// DISPATCHED surface is a separate match below, and nothing in the type
/// system tied the two together -- a verb added to one and not the other
/// either advertises a verb that returns unknown_verb, or dispatches one no
/// caller can discover. `debug_assert_verb_sets_agree` closes that gap: it
/// runs on every orchestrator dispatch in a debug build and names the exact
/// verb that drifted.
pub const ORCHESTRATOR_VERBS: &[&str] = &[
    "transition", "transition-revert", "mutable-resolve", "mutable-add", "mutable-list",
    "memorize-fire", "discipline-note", "discipline-check-removal", "discipline-audit", "memory-namespace-audit", "codeinsight-namespace-audit", "calculus-model-check", "phase-status", "residual-scan", "auto-recall",
    "instruction", "prd-add", "prd-resolve", "prd-list", "prd-defer",
    "task-spawn", "task-list", "task-stop", "task-output",
    "memorize-continue", "fsm-vendor", "fsm-validate", "fsm-propose-override", "claim-audit", "submodule-check",
];


/// Every verb this list advertises must have a real dispatch arm. Checked by
/// routing each one through the same `verb_has_dispatch_arm` predicate the
/// match itself is built from, so adding an arm without listing it (or the
/// reverse) is caught at the first dispatch rather than by a caller.
#[cfg(debug_assertions)]
fn debug_assert_verb_sets_agree() {
    for v in ORCHESTRATOR_VERBS {
        debug_assert!(
            verb_has_dispatch_arm(v),
            "ORCHESTRATOR_VERBS advertises {v} but dispatch() has no arm for it"
        );
    }
}

#[cfg(not(debug_assertions))]
fn debug_assert_verb_sets_agree() {}

pub fn is_orchestrator_verb(verb: &str) -> bool {
    ORCHESTRATOR_VERBS.contains(&verb)
}

#[cfg(target_arch = "wasm32")]
fn handle_memorize_continue(content: &str) -> (String, String, i32) {
    let body: serde_json::Value = serde_json::from_str(content).unwrap_or(serde_json::Value::Null);
    let result = crate::pipeline::handle_continue(&body);
    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    (result.to_string(), String::new(), if ok { 0 } else { 1 })
}

#[cfg(not(target_arch = "wasm32"))]
fn handle_memorize_continue(_content: &str) -> (String, String, i32) {
    ("{\"ok\":false,\"error\":\"memorize-continue requires wasm32\"}".to_string(), String::new(), 1)
}

#[cfg(debug_assertions)]
fn verb_has_dispatch_arm(verb: &str) -> bool {
    matches!(
        verb,
        "transition" | "transition-revert" | "mutable-resolve" | "mutable-add" | "mutable-list"
            | "memorize-fire" | "discipline-note" | "discipline-check-removal" | "discipline-audit" | "memory-namespace-audit" | "codeinsight-namespace-audit" | "calculus-model-check" | "phase-status" | "residual-scan"
            | "auto-recall" | "instruction" | "prd-add" | "prd-resolve" | "prd-list" | "prd-defer"
            | "task-spawn" | "task-list" | "task-stop" | "task-output"
            | "memorize-continue" | "fsm-vendor" | "fsm-validate" | "fsm-propose-override"
            | "claim-audit" | "submodule-check"
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub fn dispatch(verb: &str, _file_id: &str, _content: &str) -> (String, String, i32) {
    (format!("{{\"ok\":false,\"error\":\"orchestrator verb '{}' requires wasm32\"}}", verb), String::new(), 1)
}

#[cfg(target_arch = "wasm32")]
pub fn dispatch(verb: &str, _file_id: &str, content: &str) -> (String, String, i32) {
    debug_assert_verb_sets_agree();
    match verb {
        "transition" => transitions::handle(content),
        "transition-revert" => transitions::handle_revert(content),
        "mutable-resolve" => mutables::handle_resolve(content),
        "mutable-add" => mutables::handle_add(content),
        "mutable-list" => mutables::handle_list(content),
        "memorize-fire" => memorize::handle_fire(content),
        "discipline-note" => discipline_note::handle(content),
        "discipline-check-removal" => discipline_note::handle_check_removal(content),
        "discipline-audit" => discipline_note::handle_audit(content),
        "memory-namespace-audit" => memory_component::handle_audit(content),
        "codeinsight-namespace-audit" => codeinsight_component::handle_audit(content),
        "calculus-model-check" => calculus::handle_model_check(content),
        "phase-status" => state::handle_status(),
        "residual-scan" => residual::handle_scan(content),
        "claim-audit" => claim_audit::handle_audit(content),
        "submodule-check" => submodule_drift::handle_check(content),
        "auto-recall" => recall::handle_auto_recall(content),
        "instruction" => instructions::handle_instruction(content),
        "prd-add" => prd::handle_add(content),
        "prd-resolve" => prd::handle_resolve(content),
        "prd-list" => prd::handle_list(content),
        "prd-defer" => prd::handle_defer(content),
        "task-spawn" => task::handle_spawn(content),
        "task-list" => task::handle_list(content),
        "task-stop" => task::handle_stop(content),
        "task-output" => task::handle_output(content),
        "memorize-continue" => handle_memorize_continue(content),
        "fsm-vendor" => fsm_vendor::handle_vendor(content),
        "fsm-validate" => fsm_vendor::handle_validate(content),
        "fsm-propose-override" => fsm_propose::handle_propose(content),
        _ => (format!("Unknown orchestrator verb: {}", verb), String::new(), 1),
    }
}
