use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use super::gm_dir;
use crate::pkfs;

#[cfg(target_arch = "wasm32")]
fn dispatch_session_id() -> Option<String> {
    crate::wasm_dispatch::current_dispatch_session_id()
}

#[cfg(not(target_arch = "wasm32"))]
fn dispatch_session_id() -> Option<String> {
    None
}

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
            phase: initial_phase(),
            session_id: None,
            last_skill: None,
            updated_at_ms: now_ms(),
            pending_step_id: None,
            pending_step_deadline_ms: None,
        }
    }
}

fn initial_phase() -> Phase {
    Phase::parse(&super::fsm::graph().policy.initial_phase).unwrap_or_else(Phase::plan)
}

/// Migrates a persisted `TurnState` whose `phase` may not be valid in the
/// caller's already-resolved `g` to the graph's own initial phase. Takes `g`
/// rather than resolving `fsm::graph()` itself: each resolution is a real
/// network fetch through the config-repo tiers, and two independent
/// resolutions within one dispatch can observe two DIFFERENT graphs if the
/// tier flips mid-dispatch (a debounce window elapsing between calls, a
/// config-repo cache directory being concurrently refreshed by another
/// process). That TOCTOU let a persisted phase pass migration against
/// graph-instance-A's states, then fail the transition edge-check moments
/// later against graph-instance-B's edges -- observed live 2026-07-30
/// (thebird project): `transition{to:EXECUTE}` denied "no edge from PLAN to
/// EXECUTE" repeatedly even though `PLAN` had just passed state validation in
/// the same dispatch. Every caller in this module now resolves the graph
/// exactly once per dispatch and threads that single value through every
/// helper that needs it (see transitions::handle), so within one dispatch
/// every check agrees on which graph is active.
fn migrate_to_graph(mut s: TurnState, g: &super::fsm::Graph) -> TurnState {
    if !g.has_state(s.phase.as_str()) {
        s.phase = Phase::parse(&g.policy.initial_phase).unwrap_or_else(Phase::plan);
    }
    s
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
    let g = super::fsm::graph();
    read_state_with_graph(&g)
}

/// `read_state` split so a caller resolving the graph once for a whole
/// dispatch (see transitions::handle) can pass it in, avoiding a second
/// independent `fsm::graph()` network resolution that could disagree with
/// the first (see migrate_to_graph's doc comment).
pub fn read_state_with_graph(g: &super::fsm::Graph) -> TurnState {
    let p = state_path();
    let ps = p.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return default_state_for_graph(g);
    }
    match pkfs::read_to_string(&ps) {
        Some(s) => match serde_json::from_str(&s) {
            Ok(v) => migrate_to_graph(v, g),
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
                default_state_for_graph(g)
            }
        },
        None => default_state_for_graph(g),
    }
}

fn default_state_for_graph(g: &super::fsm::Graph) -> TurnState {
    TurnState {
        phase: Phase::parse(&g.policy.initial_phase).unwrap_or_else(Phase::plan),
        session_id: None,
        last_skill: None,
        updated_at_ms: now_ms(),
        pending_step_id: None,
        pending_step_deadline_ms: None,
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
    let g = super::fsm::graph();
    set_phase_with_session_with_graph(phase, last_skill, session_id, &g)
}

/// `set_phase_with_session` split so a caller resolving the graph once for a
/// whole dispatch (see transitions::handle) can pass it in, avoiding a
/// second independent `fsm::graph()` network resolution (see
/// migrate_to_graph's doc comment). `phase` here is the already-validated
/// target the caller computed against that SAME graph, so this function does
/// not re-validate it -- only the underlying `read_state_with_graph` call
/// uses `g`, to migrate whatever was on disk consistently with the rest of
/// this dispatch.
pub fn set_phase_with_session_with_graph(phase: Phase, last_skill: Option<String>, session_id: Option<String>, g: &super::fsm::Graph) -> Result<TurnState, std::io::Error> {
    let mut s = read_state_with_graph(g);
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
    let requesting_session_id = dispatch_session_id();
    let session_mismatch = match (&requesting_session_id, &s.session_id) {
        (Some(incoming), Some(prior)) => incoming != prior,
        _ => false,
    };
    let mut payload = match serde_json::to_value(&s) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("serialize error: {}", e), 1),
    };
    if let Value::Object(ref mut m) = payload {
        m.insert("session_owner_before_this_dispatch".to_string(), json!(s.session_id));
        m.insert("session_id".to_string(), json!(requesting_session_id));
        m.insert("session_mismatch".to_string(), json!(session_mismatch));
    }
    (payload.to_string(), String::new(), 0)
}
