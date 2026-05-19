use super::state::{Phase, set_phase_with_session, read_state};
use super::prd;
use super::recall;
use super::mutables;

pub fn next_skill(current: Phase) -> &'static str {
    match current {
        Phase::Plan => "gm-execute",
        Phase::Execute => "gm-emit",
        Phase::Emit => "gm-complete",
        Phase::Verify => "gm-complete",
        Phase::Complete => "update-docs",
    }
}

pub fn next_phase(current: Phase) -> Phase {
    match current {
        Phase::Plan => Phase::Execute,
        Phase::Execute => Phase::Emit,
        Phase::Emit => Phase::Verify,
        Phase::Verify => Phase::Complete,
        Phase::Complete => Phase::Complete,
    }
}

pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id: Option<String> = None;
    let target = if trimmed.is_empty() {
        let cur = read_state();
        let cur_phase = Phase::parse(&cur.phase).unwrap_or(Phase::Plan);
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
                let cur_phase = Phase::parse(&cur.phase).unwrap_or(Phase::Plan);
                next_phase(cur_phase)
            }
        }
    } else {
        match Phase::parse(trimmed) {
            Some(p) => p,
            None => return (String::new(), format!("invalid phase: {}", trimmed), 1),
        }
    };

    if matches!(target, Phase::Complete) {
        let pending_muts = mutables::pending_detailed();
        if !pending_muts.is_empty() {
            let ids: Vec<String> = pending_muts.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect();
            return (
                String::new(),
                format!(
                    "transition to COMPLETE rejected: {} mutables still pending — resolve them with witness_evidence before transitioning. Pending: {}",
                    pending_muts.len(),
                    ids.join(", ")
                ),
                1,
            );
        }
        let (body, _err, code) = prd::handle_list("");
        if code == 0 {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(items) = v.get("items").and_then(|v| v.as_array()) {
                    let pending_prd: Vec<String> = items.iter()
                        .filter(|it| {
                            let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                            status != "done" && status != "complete" && status != "completed"
                        })
                        .filter_map(|it| it.get("id").and_then(|v| v.as_str()).map(String::from))
                        .collect();
                    if !pending_prd.is_empty() {
                        return (
                            String::new(),
                            format!(
                                "transition to COMPLETE rejected: {} PRD items still pending — execute or remove them before transitioning. Pending: {}",
                                pending_prd.len(),
                                pending_prd.join(", ")
                            ),
                            1,
                        );
                    }
                }
            }
        }
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
                                status != "done" && status != "complete" && status != "completed"
                            }).cloned()
                        })
                        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_default()
                } else { String::new() }
            };
            let combined = if query.is_empty() { s.phase.clone() } else { format!("{} {}", s.phase, query) };
            let hits = recall::recall_hits(&combined, 3);
            let payload = serde_json::json!({
                "phase": s.phase,
                "nextSkill": skill,
                "recall_hits": hits,
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("write state failed: {}", e), 1),
    }
}
