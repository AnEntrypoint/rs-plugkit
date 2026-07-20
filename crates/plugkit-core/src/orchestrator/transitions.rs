use super::fsm::{self, GateDef, HookMode};
use super::state::{Phase, set_phase_with_session, read_state};
use super::prd;
use super::recall;
use super::mutables;

/// Default forward skill for a phase, from the active graph's state node --
/// falls back to the phase name itself (lowercased-with-gm- prefix pattern)
/// only if the graph is somehow missing a node for an otherwise-legal
/// current phase, which should not happen for any graph produced by the
/// scaffold verb or the compiled-in default.
pub fn next_skill(current: &Phase) -> String {
    let g = fsm::graph();
    g.state(current.as_str())
        .and_then(|s| s.skill.clone())
        .unwrap_or_else(|| format!("gm-{}", current.as_str().to_ascii_lowercase()))
}

/// The phase reached by a bare `transition` (no explicit `to`) -- the
/// active graph's first-listed outbound edge from the current phase, or the
/// SAME phase if none exists (terminal state self-loop, matching the old
/// Phase::Complete => Phase::Complete behavior for any phase a custom graph
/// declares terminal by omitting its own outbound edge).
pub fn next_phase(current: &Phase) -> Phase {
    let g = fsm::graph();
    match g.default_edge_from(current.as_str()) {
        Some(e) => Phase::parse(&e.to).unwrap_or_else(|| current.clone()),
        None => current.clone(),
    }
}

/// Runs a graph-registered predicate by name. This is the compiled side of
/// GateDef.predicate -- the ONLY thing a project's .gm/instructions/fsm/
/// graph.json can do is choose WHICH of these fire on WHICH edge, in what
/// order; it cannot invent an entirely new compiled predicate (that needs a
/// hook script instead, see hook_result below). Adding a new predicate here
/// is still a Rust change, same as adding a new gate class always has been
/// -- what's now data-driven is the wiring, not the primitive set.
/// (name, one-line description) for every predicate predicate_result
/// recognizes -- kept as the single source fsm-vendor's reference file
/// generates from, so the vendored doc can never silently drift out of
/// sync with what actually exists (the alternative, a hand-written
/// duplicate list in the vendor-verb code, is exactly the kind of doc that
/// goes stale the next time a predicate is added here and forgotten there).
pub fn known_predicates() -> Vec<(&'static str, &'static str)> {
    vec![
        ("residual-scan-fired", "true once `residual-scan` has been dispatched in this stop window (the .gm/residual-check-fired marker exists)"),
        ("prd-all-closed", "true when .gm/prd.yml has zero rows with an open status (pending/in-progress, not completed)"),
        ("mutables-all-resolved", "true when .gm/mutables.yml has zero rows still in unknown/pending status"),
    ]
}

fn predicate_result(name: &str) -> bool {
    match name {
        "residual-scan-fired" => residual_scan_fired(),
        "prd-all-closed" => !prd_has_open_items(),
        "mutables-all-resolved" => mutables::pending_detailed().is_empty(),
        // An unrecognized predicate name fails CLOSED (denies), never open
        // -- a typo'd or stale predicate name in a hand-edited graph must
        // never silently skip a real check.
        _ => false,
    }
}

#[cfg(target_arch = "wasm32")]
fn residual_scan_fired() -> bool {
    let residual_marker = super::gm_dir().join("residual-check-fired");
    crate::pkfs::exists(&residual_marker.to_string_lossy().to_string())
}
#[cfg(not(target_arch = "wasm32"))]
fn residual_scan_fired() -> bool { true }

fn prd_has_open_items() -> bool {
    let (body, _err, code) = prd::handle_list("");
    if code != 0 { return false; }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else { return false };
    let Some(items) = v.get("items").and_then(|v| v.as_array()) else { return false };
    items.iter().any(|it| {
        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        prd::status_is_open(status)
    })
}

/// Runs a project's jit-hook script for a gate, per fsm-framework-jit-hook-
/// concreting: exec_js's own host_exec_js with the hook file's contents as
/// the script body. The hook's final expression value is coerced to bool --
/// anything that isn't exactly JSON `true` (a thrown error, a non-boolean
/// return, a missing/unreadable file) is treated as FALSE (gate denies),
/// matching predicate_result's fail-closed default for the same reason: an
/// ambiguous or broken custom condition must never silently pass a gate it
/// was configured to guard.
#[cfg(target_arch = "wasm32")]
fn hook_result(hook_path: &str) -> bool {
    let full = format!(".gm/instructions/hooks/{}", hook_path);
    let Some(script) = crate::pkfs::read_to_string(&full) else { return false };
    let opts = serde_json::json!({ "timeoutMs": 15000 }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            script.as_ptr(), script.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false)
        && v.get("result").and_then(|r| r.as_bool()).unwrap_or(false)
}
#[cfg(not(target_arch = "wasm32"))]
fn hook_result(_hook_path: &str) -> bool { false }

fn evaluate_gate(g: &GateDef) -> bool {
    match g.hook_mode {
        HookMode::PredicateOnly => g.predicate.as_deref().map(predicate_result).unwrap_or(true),
        HookMode::HookOnly => g.hook.as_deref().map(hook_result).unwrap_or(false),
        HookMode::Both => {
            let pred_ok = g.predicate.as_deref().map(predicate_result).unwrap_or(true);
            let hook_ok = g.hook.as_deref().map(hook_result).unwrap_or(false);
            pred_ok && hook_ok
        }
    }
}

/// Evaluates every gate named on the edge being taken, in list order,
/// returning the first failure's message (matching the old hardcoded
/// ordering: residual-scan-fired before prd-all-closed before mutables-all-
/// resolved for VERIFY->CONSOLIDATE). None = every gate on this edge
/// passed, or the edge has none.
fn gate_rejection(graph: &fsm::Graph, from: &str, to: &str) -> Option<(String, String, i32)> {
    let edge = graph.edge_between(from, to)?;
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else { continue };
        if !evaluate_gate(g) {
            return Some((String::new(), g.message.clone(), 1));
        }
    }
    None
}

pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id: Option<String> = None;
    let cur = read_state();
    let cur_phase = cur.phase.clone();
    let target = if trimmed.is_empty() {
        next_phase(&cur_phase)
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
            session_id = Some(sid.to_string());
        }
        let to_str = v.get("to").and_then(|s| s.as_str())
            .or_else(|| v.get("phase").and_then(|s| s.as_str()))
            .or_else(|| v.as_str());
        match to_str {
            Some(s) => match Phase::parse(s) {
                Some(p) => p,
                None => return (String::new(), format!("invalid phase: {}", s), 1),
            },
            None => next_phase(&cur_phase),
        }
    } else {
        match Phase::parse(trimmed) {
            Some(p) => p,
            None => return (String::new(), format!("invalid phase: {}", trimmed), 1),
        }
    };

    let graph = fsm::graph();

    // A target phase absent from the active graph entirely is always
    // illegal, regardless of gates -- this is the dynamic-phase-set
    // equivalent of the old Phase::parse's Option::None for an unrecognized
    // enum variant, now checked against the LIVE graph instead of a
    // compile-time-fixed list.
    if !graph.has_state(target.as_str()) {
        return (
            String::new(),
            format!(
                "transition rejected: `{}` is not a state in the active FSM graph (states: {}). A custom graph must declare every phase it uses -- see .gm/instructions/fsm/graph.json.",
                target.as_str(),
                graph.states.iter().map(|s| s.key.as_str()).collect::<Vec<_>>().join(", ")
            ),
            1,
        );
    }

    if let Some(r) = gate_rejection(&graph, cur_phase.as_str(), target.as_str()) {
        return r;
    }

    let skill = next_skill(&target);
    match set_phase_with_session(target.clone(), Some(skill.clone()), session_id) {
        Ok(s) => {
            let query = {
                let (body, _err, code) = prd::handle_list("");
                if code == 0 {
                    serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("items").cloned())
                        .and_then(|v| v.as_array().cloned())
                        .and_then(|arr| {
                            arr.iter().find(|it| {
                                let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                                prd::status_is_open(status)
                            }).cloned()
                        })
                        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_default()
                } else { String::new() }
            };
            let combined = if query.is_empty() { s.phase.as_str().to_string() } else { format!("{} {}", s.phase.as_str(), query) };
            let hits = recall::recall_hits(&combined, 3);
            let payload = serde_json::json!({
                "phase": s.phase.as_str(),
                "nextSkill": skill,
                "recall_hits": hits,
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("write state failed: {}", e), 1),
    }
}
