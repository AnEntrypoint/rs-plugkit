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
use std::env;

#[cfg(target_arch = "wasm32")]
fn git_common_dir_project_root() -> Option<PathBuf> {
    let v = crate::wasm_dispatch::git_call("rev-parse --show-toplevel --git-common-dir", None);
    let out = v.get("stdout").and_then(|x| x.as_str())?;
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

#[cfg(not(target_arch = "wasm32"))]
fn git_common_dir_project_root() -> Option<PathBuf> {
    None
}

pub fn gm_dir() -> PathBuf {
    if let Some(root) = git_common_dir_project_root() {
        return root.join(".gm");
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    env::var("CLAUDE_PROJECT_DIR")
        .ok()
        .map(|p| PathBuf::from(p).join(".gm"))
        .unwrap_or_else(|| PathBuf::from(home).join(".gm"))
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
