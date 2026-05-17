pub mod state;
pub mod transitions;
pub mod mutables;
pub mod memorize;

use std::path::PathBuf;
use std::env;

pub fn gm_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    env::var("CLAUDE_PROJECT_DIR")
        .ok()
        .map(|p| PathBuf::from(p).join(".gm"))
        .unwrap_or_else(|| PathBuf::from(home).join(".gm"))
}

pub fn is_orchestrator_verb(verb: &str) -> bool {
    matches!(verb, "transition" | "mutable-resolve" | "memorize-fire" | "phase-status")
}

pub fn dispatch(verb: &str, _file_id: &str, content: &str) -> (String, String, i32) {
    match verb {
        "transition" => transitions::handle(content),
        "mutable-resolve" => mutables::handle_resolve(content),
        "memorize-fire" => memorize::handle_fire(content),
        "phase-status" => state::handle_status(),
        _ => (format!("Unknown orchestrator verb: {}", verb), String::new(), 1),
    }
}
