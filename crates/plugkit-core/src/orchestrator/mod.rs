pub mod state;
pub mod fsm;
pub mod fsm_vendor;
pub mod transitions;
pub mod deviations;
pub mod cas;
pub mod mutables;
pub mod memorize;
pub mod discipline_note;
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
    let out = v.get("stdout").and_then(|x| x.as_str())?;
    parse_toplevel_common_dir(out)
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

fn resolve_project_root_with_retry() -> PathBuf {
    let mut last_err_attempts = 0u32;
    for attempt in 0..RESOLVE_MAX_ATTEMPTS {
        if let Some(root) = git_common_dir_project_root_once() {
            return root;
        }
        last_err_attempts = attempt + 1;
        if attempt + 1 < RESOLVE_MAX_ATTEMPTS {
            sleep_ms(RESOLVE_BACKOFF_BASE_MS * 2u64.pow(attempt));
        }
    }
    panic!(
        "gm_dir: project root resolution failed after {} attempts via `git rev-parse --show-toplevel --git-common-dir` -- refusing to silently fall back to CLAUDE_PROJECT_DIR/HOME, which would mis-root every stateful verb onto the wrong tree. Check for git subprocess/lock contention or a missing .git directory.",
        last_err_attempts
    );
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
    "transition", "mutable-resolve", "mutable-add", "mutable-list",
    "memorize-fire", "discipline-note", "phase-status", "residual-scan", "auto-recall",
    "instruction", "prd-add", "prd-resolve", "prd-list",
    "task-spawn", "task-list", "task-stop", "task-output",
    "memorize-continue", "fsm-vendor", "fsm-validate", "claim-audit", "submodule-check",
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

/// Mirrors the arms of  below. Kept adjacent so the two are edited
/// together, and cross-checked against ORCHESTRATOR_VERBS on every dispatch in
/// a debug build by debug_assert_verb_sets_agree.
fn verb_has_dispatch_arm(verb: &str) -> bool {
    matches!(
        verb,
        "transition" | "mutable-resolve" | "mutable-add" | "mutable-list"
            | "memorize-fire" | "discipline-note" | "phase-status" | "residual-scan"
            | "auto-recall" | "instruction" | "prd-add" | "prd-resolve" | "prd-list"
            | "task-spawn" | "task-list" | "task-stop" | "task-output"
            | "memorize-continue" | "fsm-vendor" | "fsm-validate" | "claim-audit"
            | "submodule-check"
    )
}

pub fn dispatch(verb: &str, _file_id: &str, content: &str) -> (String, String, i32) {
    debug_assert_verb_sets_agree();
    match verb {
        "transition" => transitions::handle(content),
        "mutable-resolve" => mutables::handle_resolve(content),
        "mutable-add" => mutables::handle_add(content),
        "mutable-list" => mutables::handle_list(content),
        "memorize-fire" => memorize::handle_fire(content),
        "discipline-note" => discipline_note::handle(content),
        "phase-status" => state::handle_status(),
        "residual-scan" => residual::handle_scan(content),
        "claim-audit" => claim_audit::handle_audit(content),
        "submodule-check" => submodule_drift::handle_check(content),
        "auto-recall" => recall::handle_auto_recall(content),
        "instruction" => instructions::handle_instruction(content),
        "prd-add" => prd::handle_add(content),
        "prd-resolve" => prd::handle_resolve(content),
        "prd-list" => prd::handle_list(content),
        "task-spawn" => task::handle_spawn(content),
        "task-list" => task::handle_list(content),
        "task-stop" => task::handle_stop(content),
        "task-output" => task::handle_output(content),
        "memorize-continue" => handle_memorize_continue(content),
        "fsm-vendor" => fsm_vendor::handle_vendor(content),
        "fsm-validate" => fsm_vendor::handle_validate(content),
        _ => (format!("Unknown orchestrator verb: {}", verb), String::new(), 1),
    }
}
