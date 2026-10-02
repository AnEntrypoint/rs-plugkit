pub mod state;
pub mod fsm;
pub mod fsm_vendor;
pub mod fsm_propose;
pub mod transitions;
pub mod predicate_registry;
pub mod deviations;
pub mod cas;
pub mod mutables;
pub mod memorize;
pub mod discipline_note;
pub mod fiber_lifecycle;
pub mod coeffect_realm;
pub mod capability_proxy;
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
pub mod dream_rsi;
pub mod component_loader;
pub mod component_loader_dispatch;
pub mod wait;

use std::path::PathBuf;

fn parse_toplevel(out: &str) -> Option<PathBuf> {
    let toplevel = out.lines().next()?.trim();
    if toplevel.is_empty() { return None; }
    Some(PathBuf::from(toplevel))
}

enum RootProbe {
    Root(PathBuf),
    NotARepo,
    Transient,
}

fn stderr_says_not_a_repo(stderr: &str) -> bool {
    let lowered = stderr.to_ascii_lowercase();
    lowered.contains("not a git repository") || lowered.contains("gitfile")
}

#[cfg(target_arch = "wasm32")]
fn git_project_root_once() -> RootProbe {
    let v = crate::wasm_dispatch::git_call("rev-parse --show-toplevel", None);
    if v.get("async_parked").and_then(|x| x.as_bool()).unwrap_or(false) {
        return match fs_walk_project_root() {
            Some(root) => RootProbe::Root(root),
            None => RootProbe::Transient,
        };
    }
    if let Some(root) = v.get("stdout").and_then(|x| x.as_str()).and_then(parse_toplevel) {
        return RootProbe::Root(root);
    }
    let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
    if stderr_says_not_a_repo(stderr) { RootProbe::NotARepo } else { RootProbe::Transient }
}

fn stateful_cwd_root_once() -> Option<PathBuf> {
    let cwd = current_cwd_string();
    let root = cwd.trim_end_matches(['/', '\\']);
    if root.is_empty() { return None; }
    #[cfg(target_arch = "wasm32")]
    let has_gm_state = crate::wasm_dispatch::host_stat(&format!("{root}/.gm"))
        .and_then(|stat| stat.get("isDirectory").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    #[cfg(not(target_arch = "wasm32"))]
    let has_gm_state = std::fs::metadata(PathBuf::from(root).join(".gm"))
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    has_gm_state.then(|| PathBuf::from(root))
}

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
fn git_project_root_once() -> RootProbe {
    let output = match std::process::Command::new("git").args(["rev-parse", "--show-toplevel"]).output() {
        Ok(o) => o,
        Err(_) => return RootProbe::Transient,
    };
    if output.status.success() {
        return match parse_toplevel(&String::from_utf8_lossy(&output.stdout)) {
            Some(root) => RootProbe::Root(root),
            None => RootProbe::Transient,
        };
    }
    if stderr_says_not_a_repo(&String::from_utf8_lossy(&output.stderr)) { RootProbe::NotARepo } else { RootProbe::Transient }
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

static NONREPO_FALLBACK_CWDS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

fn cwd_is_nonrepo_fallback(cwd: &str) -> bool {
    NONREPO_FALLBACK_CWDS.lock().ok()
        .and_then(|set| set.as_ref().map(|s| s.contains(cwd)))
        .unwrap_or(false)
}

fn set_nonrepo_fallback(cwd: &str, on: bool) {
    if let Ok(mut set) = NONREPO_FALLBACK_CWDS.lock() {
        let set = set.get_or_insert_with(std::collections::HashSet::new);
        if on { set.insert(cwd.to_string()); } else { set.remove(cwd); }
    }
}

fn cwd_has_dot_git(cwd: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::wasm_dispatch::host_exists(&format!("{cwd}/.git"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::path::Path::new(cwd).join(".git").exists()
    }
}

fn nonrepo_fallback_root(cwd: &str) -> Option<PathBuf> {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') { return None; }
    Some(PathBuf::from(trimmed))
}

pub fn disclose_nonrepo_root(out: String) -> String {
    let cwd = current_cwd_string();
    if !cwd_is_nonrepo_fallback(&cwd) { return out; }
    let Ok(root) = try_resolve_project_root() else { return out };
    let note = format!(
        "cwd is not inside a git repository; using cwd itself as the project root ({}/.gm holds this dispatch's state). Pass git_root_override to pin a different root.",
        root.to_string_lossy().trim_end_matches(['/', '\\'])
    );
    match serde_json::from_str::<serde_json::Value>(&out) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert("project_root_disclosure".to_string(), serde_json::Value::String(note));
            serde_json::Value::Object(map).to_string()
        }
        _ => out,
    }
}

fn try_resolve_project_root() -> Result<PathBuf, u32> {
    let cwd = current_cwd_string();
    if let Ok(cache) = PROJECT_ROOT_CACHE.lock() {
        if let Some(root) = cache.as_ref().and_then(|m| m.get(&cwd)) {
            if !cwd_is_nonrepo_fallback(&cwd) || !cwd_has_dot_git(&cwd) {
                return Ok(root.clone());
            }
        }
    }
    set_nonrepo_fallback(&cwd, false);
    let mut last_err_attempts = 0u32;
    for attempt in 0..RESOLVE_MAX_ATTEMPTS {
        if let Some(root) = git_project_root_once().or_else(stateful_cwd_root_once) {
            if let Ok(mut cache) = PROJECT_ROOT_CACHE.lock() {
                cache.get_or_insert_with(std::collections::HashMap::new).insert(cwd, root.clone());
        match git_project_root_once() {
            RootProbe::Root(root) => {
                if let Ok(mut cache) = PROJECT_ROOT_CACHE.lock() {
                    cache.get_or_insert_with(std::collections::HashMap::new).insert(cwd, root.clone());
                }
                return Ok(root);
            }
            RootProbe::NotARepo => {
                let Some(root) = nonrepo_fallback_root(&cwd) else { return Err(attempt + 1) };
                if let Ok(mut cache) = PROJECT_ROOT_CACHE.lock() {
                    cache.get_or_insert_with(std::collections::HashMap::new).insert(cwd.clone(), root.clone());
                }
                set_nonrepo_fallback(&cwd, true);
                return Ok(root);
            }
            RootProbe::Transient => {}
        }
        last_err_attempts = attempt + 1;
        if attempt + 1 < RESOLVE_MAX_ATTEMPTS {
            sleep_ms(RESOLVE_BACKOFF_BASE_MS * 2u64.pow(attempt));
        }
    }
    Err(last_err_attempts)
}

pub fn project_root_resolvable() -> bool {
    try_resolve_project_root().is_ok()
}

const PROJECT_ROOT_UNRESOLVABLE_WORKAROUND: &str = "If cwd is intentionally a non-repo or multi-repo directory (e.g. a cross-repo audit root with no .git of its own), either dispatch with cwd set to one of the actual git repos underneath it, or pass `git_root_override: \"<path>\"` in this dispatch's body to pin the project root explicitly and skip git resolution entirely for this cwd.";

pub fn project_root_unresolvable_reason() -> String {
    match try_resolve_project_root() {
        Ok(_) => "project root is resolvable".to_string(),
        Err(attempts) => format!(
            "gm_dir: project root resolution failed after {attempts} attempts via `git rev-parse --show-toplevel` -- refusing to silently fall back to CLAUDE_PROJECT_DIR/HOME, which would mis-root every stateful verb onto the wrong tree. Check for git subprocess/lock contention or a missing .git directory. {PROJECT_ROOT_UNRESOLVABLE_WORKAROUND}"
        ),
    }
}

pub fn seed_project_root_override(root_str: &str) {
    let trimmed = root_str.trim();
    if trimmed.is_empty() { return; }
    let cwd = current_cwd_string();
    set_nonrepo_fallback(&cwd, false);
    if let Ok(mut cache) = PROJECT_ROOT_CACHE.lock() {
        cache.get_or_insert_with(std::collections::HashMap::new)
            .insert(cwd, PathBuf::from(trimmed));
    }
}

fn resolve_project_root_with_retry() -> PathBuf {
    match try_resolve_project_root() {
        Ok(root) => root,
        Err(attempts) => panic!(
            "gm_dir: project root resolution failed after {} attempts via `git rev-parse --show-toplevel` -- refusing to silently fall back to CLAUDE_PROJECT_DIR/HOME, which would mis-root every stateful verb onto the wrong tree. Check for git subprocess/lock contention or a missing .git directory. {}",
            attempts, PROJECT_ROOT_UNRESOLVABLE_WORKAROUND
        ),
    }
}

pub fn gm_dir() -> PathBuf {
    resolve_project_root_with_retry().join(".gm")
}

macro_rules! orchestrator_dispatch_table {
    ( $content:ident, $( $verb:literal => $handler:expr ),+ $(,)? ) => {
        pub const ORCHESTRATOR_VERBS: &[&str] = &[ $( $verb ),+ ];

        const DISPATCH_ARM_VERBS: &[&str] = &[ $( $verb ),+ ];

        #[cfg(not(target_arch = "wasm32"))]
        pub fn dispatch(verb: &str, _file_id: &str, _content: &str) -> (String, String, i32) {
            (format!("{{\"ok\":false,\"error\":\"orchestrator verb '{}' requires wasm32\"}}", verb), String::new(), 1)
        }

        #[cfg(target_arch = "wasm32")]
        pub fn dispatch(verb: &str, _file_id: &str, $content: &str) -> (String, String, i32) {
            assert_verb_sets_agree();
            let (out, err_msg, code) = match verb {
                $( $verb => $handler, )+
                _ => (format!("Unknown orchestrator verb: {}", verb), String::new(), 1),
            };
            (if code == 0 { disclose_nonrepo_root(out) } else { out }, err_msg, code)
        }
    };
}

fn assert_verb_sets_agree() {
    for v in ORCHESTRATOR_VERBS {
        assert!(
            verb_has_dispatch_arm(v),
            "ORCHESTRATOR_VERBS advertises {v} but dispatch() has no arm for it"
        );
    }
    for v in DISPATCH_ARM_VERBS {
        assert!(
            ORCHESTRATOR_VERBS.contains(v),
            "dispatch() has an arm for {v} but ORCHESTRATOR_VERBS does not advertise it -- this verb is unreachable in production"
        );
    }
}

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

fn verb_has_dispatch_arm(verb: &str) -> bool {
    DISPATCH_ARM_VERBS.contains(&verb)
}

orchestrator_dispatch_table! {
    content,
    "transition" => transitions::handle(content),
    "transition-revert" => transitions::handle_revert(content),
    "mutable-resolve" => mutables::handle_resolve(content),
    "mutable-add" => mutables::handle_add(content),
    "mutable-list" => mutables::handle_list(content),
    "mutable-defer" => mutables::handle_defer(content),
    "dream-policy-register" => dream_rsi::handle_policy_register(content),
    "dream-evaluator-receipt" => dream_rsi::handle_evaluator_receipt(content),
    "dream-discovery-record" => dream_rsi::handle_discovery_record(content),
    "dream-world-seal" => dream_rsi::handle_seal(content),
    "dream-replay-round" => dream_rsi::handle_replay_round(content),
    "dream-replay" => dream_rsi::handle(content),
    "memorize-fire" => memorize::handle_fire(content),
    "memorize-backfill" => memorize::handle_backfill(content),
    "discipline-note" => discipline_note::handle(content),
    "discipline-check-removal" => discipline_note::handle_check_removal(content),
    "discipline-audit" => discipline_note::handle_audit(content),
    "capability-resolve" => capability_proxy::handle(content),
    "memory-namespace-audit" => memory_component::handle_audit(content),
    "codeinsight-namespace-audit" => codeinsight_component::handle_audit(content),
    "calculus-model-check" => calculus::handle_model_check(content),
    "phase-status" => state::handle_status(),
    "wait" => wait::handle(content),
    "sleep" => wait::handle(content),
    "residual-scan" => residual::handle_scan(content),
    "claim-audit" => claim_audit::handle_audit(content),
    "submodule-check" => submodule_drift::handle_check(content),
    "component-loader-reconcile" => component_loader_dispatch::handle_reconcile(content),
    "component-loader-hmr" => component_loader_dispatch::handle_hmr(content),
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
    "predicates-md" => transitions::handle_predicates_md(content),
    "fsm-propose-override" => fsm_propose::handle_propose(content),
}
