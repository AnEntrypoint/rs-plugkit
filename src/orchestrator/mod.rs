pub mod state;
pub mod transitions;
pub mod cas;
pub mod mutables;
pub mod memorize;
pub mod residual;
pub mod recall;
pub mod instructions;
pub mod yaml_util;
pub mod prd;
pub mod task;

use std::path::PathBuf;
use std::sync::OnceLock;

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

static PROJECT_ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn gm_dir() -> PathBuf {
    PROJECT_ROOT
        .get_or_init(resolve_project_root_with_retry)
        .join(".gm")
}

pub fn is_orchestrator_verb(verb: &str) -> bool {
    matches!(
        verb,
        "transition" | "mutable-resolve" | "mutable-add" | "mutable-list"
            | "memorize-fire" | "phase-status" | "residual-scan" | "auto-recall"
            | "instruction" | "prd-add" | "prd-resolve" | "prd-list"
            | "task-spawn" | "task-list" | "task-stop" | "task-output"
            | "memorize-continue"
    )
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

pub fn dispatch(verb: &str, _file_id: &str, content: &str) -> (String, String, i32) {
    match verb {
        "transition" => transitions::handle(content),
        "mutable-resolve" => mutables::handle_resolve(content),
        "mutable-add" => mutables::handle_add(content),
        "mutable-list" => mutables::handle_list(content),
        "memorize-fire" => memorize::handle_fire(content),
        "phase-status" => state::handle_status(),
        "residual-scan" => residual::handle_scan(content),
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
        _ => (format!("Unknown orchestrator verb: {}", verb), String::new(), 1),
    }
}
