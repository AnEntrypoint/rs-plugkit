use super::state::{Phase, set_phase_with_session, read_state};
use super::prd;
use super::recall;
use super::mutables;

pub fn next_skill(current: Phase) -> &'static str {
    match current {
        Phase::Plan => "gm-execute",
        Phase::Execute => "gm-emit",
        Phase::Emit => "gm-verify",
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
        #[cfg(target_arch = "wasm32")]
        {
            let porcelain = crate::wasm_dispatch::git_porcelain();
            let worktree_clean = porcelain.trim().is_empty();
            let (ahead, behind, branch) = {
                let branch = crate::wasm_dispatch::git_call("rev-parse --abbrev-ref HEAD", None)
                    .get("stdout").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let remote_cfg = crate::wasm_dispatch::git_call(&format!("config --get branch.{}.remote", branch), None)
                    .get("stdout").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let remote = if remote_cfg.is_empty() { "origin".to_string() } else { remote_cfg };
                // git_push/git_finalize do not refresh the local remote-tracking ref, so a
                // rev-list against the stale origin/<branch> reports a false ahead:N right after
                // a successful push and wedges this gate even though HEAD is on the remote. Fetch
                // first so remote-pushed reflects the true remote; tolerate offline failure.
                let _ = crate::wasm_dispatch::git_call(&format!("fetch {} {}", remote, branch), None);
                let counts = crate::wasm_dispatch::git_call(&format!("rev-list --left-right --count {}/{}...HEAD", remote, branch), None)
                    .get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parts: Vec<&str> = counts.split_whitespace().collect();
                let (b, a) = if parts.len() == 2 {
                    (parts[0].parse::<u64>().unwrap_or(0), parts[1].parse::<u64>().unwrap_or(0))
                } else { (0u64, 0u64) };
                (a, b, branch)
            };
            let remote_pushed = ahead == 0;
            let prd_empty = {
                let (b, _e, c) = prd::handle_list("");
                if c != 0 { false } else {
                    serde_json::from_str::<serde_json::Value>(&b).ok()
                        .and_then(|v| v.get("items").cloned())
                        .and_then(|v| v.as_array().cloned())
                        .map(|arr| arr.iter().all(|it| {
                            let s = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                            s == "done" || s == "complete" || s == "completed"
                        }))
                        .unwrap_or(true)
                }
            };
            let mutables_witnessed = mutables::pending_detailed().is_empty();
            if !(worktree_clean && remote_pushed && prd_empty && mutables_witnessed) {
                return (
                    String::new(),
                    format!(
                        "transition to COMPLETE rejected: four-observation gate not satisfied — worktree-clean={} remote-pushed={} (branch={} ahead={} behind={}) prd-empty={} mutables-witnessed={}. All four must hold simultaneously per VERIFY convergence.",
                        worktree_clean, remote_pushed, branch, ahead, behind, prd_empty, mutables_witnessed
                    ),
                    1,
                );
            }
        }
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
