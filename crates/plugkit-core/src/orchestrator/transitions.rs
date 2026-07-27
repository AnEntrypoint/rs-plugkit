use super::fsm::{self, GateDef, HookMode};
use super::state::{Phase, set_phase_with_session, read_state};
use super::prd;
use super::recall;
use super::mutables;

pub fn next_skill(current: &Phase) -> String {
    let g = fsm::graph();
    g.state(current.as_str())
        .and_then(|s| s.skill.clone())
        .unwrap_or_else(|| format!("gm-{}", current.as_str().to_ascii_lowercase()))
}

pub fn next_phase(current: &Phase) -> Phase {
    let g = fsm::graph();
    match g.default_edge_from(current.as_str()) {
        Some(e) => Phase::parse(&e.to).unwrap_or_else(|| current.clone()),
        None => current.clone(),
    }
}

pub fn known_predicates() -> Vec<(&'static str, &'static str)> {
    predicate_table().iter().map(|(name, desc, _)| (*name, *desc)).collect()
}

/// The ONE table: name, human description, and evaluator declared together.
///
/// These used to be two independent lists -- `known_predicates()` for the
/// generated reference and a `match` in `predicate_result` for the behaviour --
/// and they drifted: the match dispatched 8 predicates while the list published
/// 6, so `fsm_vendor` generated a `predicates.md` that contradicted the default
/// `graph.json` it generated beside it, while asserting in its own text that it
/// could not drift. Declaring all three facets in one place makes that class of
/// bug unrepresentable: you cannot add an evaluator without also supplying the
/// name and description the reference is generated from.
///
/// The evaluator is a fn pointer rather than a closure so this stays a plain
/// const-shaped table with no allocation or lazy init.
type PredicateFn = fn() -> bool;

fn predicate_table() -> &'static [(&'static str, &'static str, PredicateFn)] {
    &[
        ("residual-scan-fired", "true once `residual-scan` has been dispatched in this stop window (the .gm/residual-check-fired marker is present AND non-empty -- it is invalidated by truncation)", residual_scan_fired as PredicateFn),
        ("prd-all-closed", "true when .gm/prd.yml has zero rows with an open status (pending/in-progress, not completed)", pred_prd_all_closed),
        ("mutables-all-resolved", "true when .gm/mutables.yml has zero rows still in unknown/pending status", pred_mutables_all_resolved),
        ("worktree-clean", "true when `git status --porcelain` is empty -- no uncommitted/unpushed delta", pred_worktree_clean),
        ("ci-validated-fresh", "true when .gm/exec-spool/.ci-validated exists and its head_sha matches the current `git rev-parse HEAD` -- a witnessed-green CI run for the exact pushed commit", ci_validation_fresh as PredicateFn),
        ("browser-witness-coverage", "true when every client-side file edited this session (per .gm/exec-spool/.turn-browser-edits.json) has a matching entry in .gm/exec-spool/.turn-browser-witnessed with the same content hash", pred_browser_witness_coverage),
        ("claim-audit-clean", "true when the claim audit finds no unwitnessed completion claims -- see orchestrator::claim_audit", pred_claim_audit_clean),
        ("submodules-clean", "true when no submodule has drifted from its recorded commit -- see orchestrator::submodule_drift", pred_submodules_clean),
    ]
}

fn pred_prd_all_closed() -> bool { !prd_has_open_items() }
fn pred_mutables_all_resolved() -> bool { mutables::pending_detailed().is_empty() }
fn pred_worktree_clean() -> bool { !worktree_dirty() }
fn pred_browser_witness_coverage() -> bool { check_browser_witness_coverage_for_cwd("").is_empty() }
fn pred_claim_audit_clean() -> bool { super::claim_audit::claim_audit_clean() }
fn pred_submodules_clean() -> bool { super::submodule_drift::submodules_clean() }

fn predicate_result(name: &str) -> bool {
    if let Some((_, _, f)) = predicate_table().iter().find(|(n, _, _)| *n == name) {
        return f();
    }
    match name {
        other => {
            crate::wasm_dispatch::emit_event("fsm_unknown_predicate", serde_json::json!({
                "predicate": other,
                "reason": "not in transitions::known_predicates(); this gate can never be satisfied. Fix the name in .gm/instructions/fsm/graph.json (see fsm/predicates.md for the valid set) or use a jit hook for a condition that has no compiled predicate.",
            }));
            false
        }
    }
}

#[cfg(target_arch = "wasm32")]
/// ONE global marker, deliberately -- not per-edge, and not per-phase.
///
/// Two edges gate on this today (VERIFY -> CONSOLIDATE and CONSOLIDATE ->
/// COMPLETE), so a single scan satisfies both. That looks like a hole worth
/// scoping per-edge, and is not: this is a STOP-WINDOW marker. Residual-scan
/// asks "is there loose work left in this window" -- a question whose answer
/// cannot differ between two edges of one continuous VERIFY -> CONSOLIDATE ->
/// COMPLETE walk inside the same window. Scoping it per-edge would force a
/// redundant second scan of state nothing has touched since the first, which
/// reads as diligence but is pure ceremony.
///
/// CAVEAT, and it is the real weakness here: the window boundary is enforced
/// ENTIRELY by `clear_marker("residual-check-fired")` in lib.rs's
/// session_start/session_end/prompt_submit hooks. Where those hooks do not
/// run, nothing ever clears this file, and the predicate then reports a scan
/// from an arbitrarily old session as if it had just fired -- observed live
/// with a marker ten days stale (its `gm-fired-this-turn` sibling, cleared by
/// the same hooks, was months stale). So the freshness guarantee is exactly
/// as good as hook delivery, and fails OPEN when that is absent. A mtime-vs-
/// session-start comparison here would degrade gracefully instead; not done
/// in this pass because the marker carries no timestamp to compare against
/// and adding one is a wire-format change, not a comment fix.
fn residual_scan_fired() -> bool {
    let residual_marker = super::gm_dir().join("residual-check-fired");
    crate::pkfs::read_to_string(&residual_marker.to_string_lossy().to_string())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
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

#[cfg(target_arch = "wasm32")]
fn worktree_dirty() -> bool {
    !crate::wasm_dispatch::git_porcelain().trim().is_empty()
}
#[cfg(not(target_arch = "wasm32"))]
fn worktree_dirty() -> bool { false }

#[cfg(target_arch = "wasm32")]
fn ci_validation_fresh() -> bool {
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.ci-validated").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() { return false; }
    let current_head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if current_head.is_empty() { return false; }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            let marker_sha = v.get("head_sha").and_then(|s| s.as_str()).unwrap_or("");
            !marker_sha.is_empty() && marker_sha == current_head
        }
        Err(_) => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn ci_validation_fresh() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn check_browser_witness_coverage_for_cwd(cwd: &str) -> Vec<String> {
    let edits_path = if cwd.is_empty() {
        ".gm/exec-spool/.turn-browser-edits.json".to_string()
    } else {
        format!("{}/.gm/exec-spool/.turn-browser-edits.json", cwd.trim_end_matches('/').trim_end_matches('\\'))
    };
    let edits_raw = crate::pkfs::read_to_string(&edits_path).unwrap_or_default();
    if edits_raw.trim().is_empty() { return vec![]; }
    let edits: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(&edits_raw) {
        Ok(serde_json::Value::Array(arr)) => arr,
        _ => return vec![],
    };
    if edits.is_empty() { return vec![]; }
    let witness_path = if cwd.is_empty() {
        ".gm/exec-spool/.turn-browser-witnessed".to_string()
    } else {
        format!("{}/.gm/exec-spool/.turn-browser-witnessed", cwd.trim_end_matches('/').trim_end_matches('\\'))
    };
    let witness_raw = crate::pkfs::read_to_string(&witness_path).unwrap_or_default();
    let witnessed_hashes: serde_json::Map<String, serde_json::Value> = if witness_raw.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<serde_json::Value>(&witness_raw).ok()
            .and_then(|v| v.get("witnessed_hashes").cloned())
            .and_then(|v| if let serde_json::Value::Object(m) = v { Some(m) } else { None })
            .unwrap_or_default()
    };
    let mut unwitnessed: Vec<String> = vec![];
    for entry in edits.iter() {
        let file = match entry.get("file").and_then(|v| v.as_str()) {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        if !crate::browser_witness::is_browser_running_file(file) { continue; }
        let edit_hash = entry.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        if edit_hash.is_empty() {
            unwitnessed.push(format!("{file} (edit recorded with no readable content hash)"));
            continue;
        }
        let witness_hash = witnessed_hashes.get(file).and_then(|v| v.as_str()).unwrap_or("");
        if witness_hash != edit_hash {
            unwitnessed.push(file.to_string());
        }
    }
    unwitnessed
}
#[cfg(not(target_arch = "wasm32"))]
fn check_browser_witness_coverage_for_cwd(_cwd: &str) -> Vec<String> { vec![] }

/// Why a hook did not pass.
///
/// Every one of these previously collapsed to a bare `false`, so an operator
/// staring at a denial could not tell a MISSING hook file from a hook that ran
/// and legitimately said no -- one is a broken config, the other is the system
/// working. They demand opposite responses and looked identical.
pub enum HookOutcome {
    Passed,
    /// The hook file does not exist at `.gm/instructions/hooks/<path>`.
    Missing,
    /// exec_js itself failed (threw, timed out, unreadable result).
    ExecFailed,
    /// The hook ran fine and returned something other than `true`.
    ReturnedFalse,
    /// Hooks are not available on this build (native).
    Unsupported,
}

impl HookOutcome {
    pub fn passed(&self) -> bool {
        matches!(self, HookOutcome::Passed)
    }

    /// Operator-facing explanation, appended to the gate's own message.
    pub fn reason(&self, hook_path: &str) -> Option<String> {
        match self {
            HookOutcome::Passed => None,
            HookOutcome::Missing => Some(format!(
                "hook `{hook_path}` is MISSING at .gm/instructions/hooks/{hook_path} -- gates fail CLOSED, so a hook that is not there denies forever. Create it, or clear the gate's `hook` field."
            )),
            HookOutcome::ExecFailed => Some(format!(
                "hook `{hook_path}` FAILED TO RUN (threw, timed out, or returned an unreadable result) -- this is a broken hook, not a legitimate denial."
            )),
            HookOutcome::ReturnedFalse => Some(format!(
                "hook `{hook_path}` ran and returned something other than `true` -- this is the hook denying on purpose. Note a hook body needs an explicit `return`; a bare trailing expression is discarded and reads as a denial."
            )),
            HookOutcome::Unsupported => Some(format!(
                "hook `{hook_path}` cannot run on this build (no exec_js host) -- gates fail CLOSED."
            )),
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn hook_outcome(hook_path: &str) -> HookOutcome {
    let full = format!(".gm/instructions/hooks/{}", hook_path);
    let Some(script) = crate::pkfs::read_to_string(&full) else { return HookOutcome::Missing };
    let opts = serde_json::json!({ "timeoutMs": fsm::graph().policy.hook_timeout_ms }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            script.as_ptr(), script.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return HookOutcome::ExecFailed;
    }
    if v.get("result").and_then(|r| r.as_bool()).unwrap_or(false) {
        HookOutcome::Passed
    } else {
        HookOutcome::ReturnedFalse
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn hook_outcome(_hook_path: &str) -> HookOutcome { HookOutcome::Unsupported }

fn hook_result(hook_path: &str) -> bool {
    hook_outcome(hook_path).passed()
}

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

fn gate_rejection(graph: &fsm::Graph, from: &str, to: &str) -> Option<(String, String, i32)> {
    let Some(edge) = graph.edge_between(from, to) else {
        return Some((
            String::new(),
            format!(
                "transition rejected: no edge from `{}` to `{}` in the active FSM graph -- there is no legal direct path between these phases.",
                from, to
            ),
            1,
        ));
    };
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else { continue };
        if !evaluate_gate(g) {
            let detail = hook_failure_detail(g);
            let message = match detail {
                Some(d) => format!("{} -- {}", g.message, d),
                None => g.message.clone(),
            };
            return Some((String::new(), message, 1));
        }
    }
    None
}

/// The hook-specific reason a gate denied, when a hook is what denied it.
///
/// `None` when the gate has no hook, when the hook is not consulted in this
/// mode, or when the hook passed and a compiled predicate is what failed --
/// in that last case the predicate is the real cause and a hook note would
/// misdirect.
fn hook_failure_detail(g: &GateDef) -> Option<String> {
    if matches!(g.hook_mode, HookMode::PredicateOnly) {
        return None;
    }
    let hook_path = g.hook.as_deref()?;
    hook_outcome(hook_path).reason(hook_path)
}

pub fn gate_residuals(from: &str, to: &str) -> (Vec<String>, Option<String>) {
    let graph = fsm::graph();
    let Some(edge) = graph.edge_between(from, to) else {
        return (
            vec![format!("no edge from `{from}` to `{to}` in the active FSM graph -- no legal direct path between these phases")],
            Some("instruction".to_string()),
        );
    };
    let mut residuals = Vec::new();
    let mut next_dispatch: Option<String> = None;
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else { continue };
        if !evaluate_gate(g) {
            residuals.push(match hook_failure_detail(g) {
                Some(d) => format!("{} -- {}", g.message, d),
                None => g.message.clone(),
            });
            if next_dispatch.is_none() {
                next_dispatch = Some(match gate_name.as_str() {
                    "residual-scan-fired" => "residual-scan",
                    "prd-all-closed" => "prd-resolve",
                    "mutables-all-resolved" => "mutable-resolve",
                    "worktree-clean" => "git_finalize",
                    "ci-validated-fresh" => "exec_js",
                    "browser-witness-coverage" => "browser",
                    "claim-audit-clean" => "claim-audit",
                    "submodules-clean" => "git_add",
                    _ => "instruction",
                }.to_string());
            }
        }
    }
    (residuals, next_dispatch)
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
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event("phase.transitioned", serde_json::json!({ "from": cur_phase.as_str(), "phase": s.phase.as_str() }));
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
            let hits = recall::recall_hits(&combined, crate::ragconfig::InstructionPayloadConfig::default().transition_recall_hits);
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
