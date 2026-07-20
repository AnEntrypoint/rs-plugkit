use serde::{Deserialize, Serialize};
use super::gm_dir;
use crate::pkfs;

/// A phase name is a dynamic identifier, not a fixed Rust enum -- a project's
/// .gm/instructions/fsm/states.yml graph (see orchestrator::fsm) can define
/// phases beyond the six built-in ones (PLAN/EXECUTE/EMIT/VERIFY/CONSOLIDATE/
/// COMPLETE), e.g. inserting a custom REVIEW phase between EMIT and VERIFY,
/// with no Rust rebuild. Always stored/compared uppercase (parse() enforces
/// this at every construction site) so "plan"/"Plan"/"PLAN" from a caller's
/// JSON body are all the same phase. The six built-ins stay as associated
/// fn constructors (Phase::plan(), not a match arm) purely so existing call
/// sites read the same as before migrating off the old enum variants -- they
/// produce ordinary Phase values, not a distinguished case the type system
/// tracks specially. Whether a given phase name is actually LEGAL (has a
/// registered node in the active graph) is the fsm module's job, checked at
/// transition time against the loaded graph, never encoded in this type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Phase(String);

impl Phase {
    pub fn plan() -> Phase { Phase("PLAN".to_string()) }
    pub fn execute() -> Phase { Phase("EXECUTE".to_string()) }
    pub fn emit() -> Phase { Phase("EMIT".to_string()) }
    pub fn verify() -> Phase { Phase("VERIFY".to_string()) }
    pub fn consolidate() -> Phase { Phase("CONSOLIDATE".to_string()) }
    pub fn complete() -> Phase { Phase("COMPLETE".to_string()) }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Always succeeds (unlike the old enum's Option-returning parse) --
    /// any non-empty string is a syntactically valid phase name now; a
    /// custom-graph project can name a phase anything. Empty/whitespace-only
    /// input still returns None since it can never be a meaningful phase
    /// identifier. Whether the resulting name has a corresponding node in
    /// the ACTIVE graph is a separate, later check (fsm::graph().has_state).
    pub fn parse(s: &str) -> Option<Phase> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Phase(trimmed.to_ascii_uppercase()))
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnState {
    pub phase: Phase,
    pub session_id: Option<String>,
    pub last_skill: Option<String>,
    pub updated_at_ms: u128,
    #[serde(default)]
    pub pending_step_id: Option<String>,
    #[serde(default)]
    pub pending_step_deadline_ms: Option<u128>,
}

impl Default for TurnState {
    fn default() -> Self {
        TurnState {
            phase: Phase::plan(),
            session_id: None,
            last_skill: None,
            updated_at_ms: now_ms(),
            pending_step_id: None,
            pending_step_deadline_ms: None,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u128 {
    unsafe { crate::wasm_dispatch::host_now_ms() as u128 }
}

pub fn state_path() -> std::path::PathBuf {
    gm_dir().join("turn-state.json")
}

pub fn read_state() -> TurnState {
    let p = state_path();
    let ps = p.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return TurnState::default();
    }
    match pkfs::read_to_string(&ps) {
        Some(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                let now = now_ms();
                let backup_path = format!("{}.corrupted-{}", ps, now);
                let _ = pkfs::write(&backup_path, &s);
                let detail = format!("turn-state.json parse failed ({}): backed up to {}", e, backup_path);
                eprintln!("{}", detail);
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("turn-state-corrupted", serde_json::json!({
                    "error": e.to_string(),
                    "backupPath": backup_path,
                }));
                TurnState::default()
            }
        },
        None => TurnState::default(),
    }
}

pub fn write_state(state: &TurnState) -> Result<(), std::io::Error> {
    let p = state_path();
    let ps = p.to_string_lossy().to_string();
    let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    if pkfs::write(&ps, &json) {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "pkfs write failed"))
    }
}

pub fn set_phase_with_session(phase: Phase, last_skill: Option<String>, session_id: Option<String>) -> Result<TurnState, std::io::Error> {
    let mut s = read_state();
    s.phase = phase;
    if last_skill.is_some() {
        s.last_skill = last_skill;
    }
    if session_id.is_some() {
        s.session_id = session_id;
    }
    s.updated_at_ms = now_ms();
    write_state(&s)?;
    Ok(s)
}

pub fn handle_status() -> (String, String, i32) {
    let s = read_state();
    match serde_json::to_string_pretty(&s) {
        Ok(out) => (out, String::new(), 0),
        Err(e) => (String::new(), format!("serialize error: {}", e), 1),
    }
}
