use serde::{Deserialize, Serialize};
use super::gm_dir;
use crate::pkfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Plan,
    Execute,
    Emit,
    Verify,
    Complete,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Plan => "PLAN",
            Phase::Execute => "EXECUTE",
            Phase::Emit => "EMIT",
            Phase::Verify => "VERIFY",
            Phase::Complete => "COMPLETE",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
        match s.trim().to_ascii_uppercase().as_str() {
            "PLAN" => Some(Phase::Plan),
            "EXECUTE" => Some(Phase::Execute),
            "EMIT" => Some(Phase::Emit),
            "VERIFY" => Some(Phase::Verify),
            "COMPLETE" => Some(Phase::Complete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnState {
    pub phase: String,
    pub session_id: Option<String>,
    pub last_skill: Option<String>,
    pub updated_at_ms: u128,
}

impl Default for TurnState {
    fn default() -> Self {
        TurnState {
            phase: "PLAN".to_string(),
            session_id: None,
            last_skill: None,
            updated_at_ms: now_ms(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u128 {
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
        Some(s) => serde_json::from_str(&s).unwrap_or_default(),
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

pub fn set_phase(phase: Phase, last_skill: Option<String>) -> Result<TurnState, std::io::Error> {
    let mut s = read_state();
    s.phase = phase.as_str().to_string();
    if last_skill.is_some() {
        s.last_skill = last_skill;
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
