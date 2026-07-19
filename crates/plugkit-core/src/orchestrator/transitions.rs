use super::state::{Phase, set_phase_with_session, read_state};
use super::prd;
use super::recall;
use super::mutables;

pub fn next_skill(current: Phase) -> &'static str {
    match current {
        Phase::Plan => "gm-execute",
        Phase::Execute => "gm-emit",
        Phase::Emit => "gm-verify",
        Phase::Verify => "gm-consolidate",
        Phase::Consolidate => "gm-complete",
        Phase::Complete => "update-docs",
    }
}

pub fn next_phase(current: Phase) -> Phase {
    match current {
        Phase::Plan => Phase::Execute,
        Phase::Execute => Phase::Emit,
        Phase::Emit => Phase::Verify,
        Phase::Verify => Phase::Consolidate,
        Phase::Consolidate => Phase::Complete,
        Phase::Complete => Phase::Complete,
    }
}

fn pending_mutables_rejection(target: &str) -> Option<(String, String, i32)> {
    let pending_muts = mutables::pending_detailed();
    if pending_muts.is_empty() { return None; }
    let ids: Vec<String> = pending_muts.iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    Some((
        String::new(),
        format!(
            "transition to {} rejected: {} mutables still pending -- resolve them with witness_evidence before transitioning. Pending: {}",
            target,
            pending_muts.len(),
            ids.join(", ")
        ),
        1,
    ))
}

fn pending_prd_rejection(target: &str) -> Option<(String, String, i32)> {
    let (body, _err, code) = prd::handle_list("");
    if code != 0 { return None; }
    let v = serde_json::from_str::<serde_json::Value>(&body).ok()?;
    let items = v.get("items").and_then(|v| v.as_array())?;
    // No blockedBy:external exemption: everything is fixable, so an apparent
    // external blocker is a row to build past, not a resting state that a
    // transition may skip over. An open row -- external annotation or not --
    // blocks the transition until it is genuinely resolved with a real fix.
    // (The former exemption, matched to the CONSOLIDATE gate's, was the escape
    // hatch that let "external" stand in for a completed row; both sites drop it
    // together.)
    let pending_prd: Vec<String> = items.iter()
        .filter(|it| {
            let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            prd::status_is_open(status)
        })
        .filter_map(|it| it.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();
    if pending_prd.is_empty() { return None; }
    Some((
        String::new(),
        format!(
            "transition to {} rejected: {} PRD items still pending -- execute or remove them before transitioning. Pending: {}",
            target,
            pending_prd.len(),
            pending_prd.join(", ")
        ),
        1,
    ))
}

pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id: Option<String> = None;
    let target = if trimmed.is_empty() {
        let cur = read_state();
        let cur_phase = cur.phase;
        next_phase(cur_phase)
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
            None => {
                let cur = read_state();
                let cur_phase = cur.phase;
                next_phase(cur_phase)
            }
        }
    } else {
        match Phase::parse(trimmed) {
            Some(p) => p,
            None => return (String::new(), format!("invalid phase: {}", trimmed), 1),
        }
    };

    if matches!(target, Phase::Consolidate) {
        if let Some(r) = pending_mutables_rejection("CONSOLIDATE") { return r; }
        if let Some(r) = pending_prd_rejection("CONSOLIDATE") { return r; }
        #[cfg(target_arch = "wasm32")]
        {
            let residual_marker = super::gm_dir().join("residual-check-fired");
            let residual_ps = residual_marker.to_string_lossy().to_string();
            if !crate::pkfs::exists(&residual_ps) {
                return (
                    String::new(),
                    "transition to CONSOLIDATE rejected: residual-scan not fired in this stop window -- dispatch `residual-scan` before CONSOLIDATE.".to_string(),
                    1,
                );
            }
        }
    }

    if matches!(target, Phase::Complete) {
        #[cfg(target_arch = "wasm32")]
        {
            let cur = read_state();
            if cur.phase != Phase::Consolidate {
                return (
                    String::new(),
                    format!(
                        "transition to COMPLETE rejected: current phase is {}, not CONSOLIDATE -- CONSOLIDATE is the mandatory git-consolidation + CI/CD-validation phase between VERIFY and COMPLETE; transition to CONSOLIDATE first.",
                        cur.phase.as_str()
                    ),
                    1,
                );
            }
        }
        if let Some(r) = pending_mutables_rejection("COMPLETE") { return r; }
        if let Some(r) = pending_prd_rejection("COMPLETE") { return r; }
    }

    let skill = next_skill(target);
    match set_phase_with_session(target, Some(skill.to_string()), session_id) {
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
