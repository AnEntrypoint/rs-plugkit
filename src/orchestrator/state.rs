use serde::{Deserialize, Serialize};
use super::gm_dir;
use crate::pkfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[serde(rename = "PLAN")]
    Plan,
    #[serde(rename = "EXECUTE")]
    Execute,
    #[serde(rename = "EMIT")]
    Emit,
    #[serde(rename = "VERIFY")]
    Verify,
    #[serde(rename = "CONSOLIDATE")]
    Consolidate,
    #[serde(rename = "COMPLETE")]
    Complete,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Plan => "PLAN",
            Phase::Execute => "EXECUTE",
            Phase::Emit => "EMIT",
            Phase::Verify => "VERIFY",
            Phase::Consolidate => "CONSOLIDATE",
            Phase::Complete => "COMPLETE",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
        match s.trim().to_ascii_uppercase().as_str() {
            "PLAN" => Some(Phase::Plan),
            "EXECUTE" => Some(Phase::Execute),
            "EMIT" => Some(Phase::Emit),
            "VERIFY" => Some(Phase::Verify),
            "CONSOLIDATE" => Some(Phase::Consolidate),
            "COMPLETE" => Some(Phase::Complete),
            _ => None,
        }
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
            phase: Phase::Plan,
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
                eprintln!("turn-state.json parse failed ({}): backed up to {}", e, backup_path);
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

pub fn set_phase(phase: Phase, last_skill: Option<String>) -> Result<TurnState, std::io::Error> {
    set_phase_with_session(phase, last_skill, None)
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
