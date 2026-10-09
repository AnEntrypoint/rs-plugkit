#[cfg(target_arch = "wasm32")]
use super::gm_dir;
#[cfg(target_arch = "wasm32")]
use crate::pkfs;

pub const RESIDUAL_PRD_OPEN_DEFAULT: &str =
    "PRD still has items; complete or remove them before residual scan.";
pub const RESIDUAL_TASKS_RUNNING_DEFAULT: &str = "background tasks still running -- wait for completion or kill them via the host_exec_js interface before retrying residual-scan";
pub const RESIDUAL_DIRTY_TREE_DEFAULT: &str = "worktree dirty -- modified={modified} untracked={untracked} -- commit or revert before residual scan; a push from a dirty tree orphans the unstaged delta";
pub const RESIDUAL_IMPERATIVE_DEFAULT: &str = "Residual scan. Worktree clean, remote pushed, PRD empty, mutables witnessed -- the four checks. Anything reachable and in-spirit expands the PRD and runs. Out-of-reach is credentials, down service, product decision.";

#[cfg(target_arch = "wasm32")]
fn porcelain_output() -> String {
    crate::wasm_dispatch::git_porcelain()
}


#[cfg(target_arch = "wasm32")]
fn count_modified_untracked(porcelain: &str) -> (usize, usize) {
    let mut modified = 0usize;
    let mut untracked = 0usize;
    for line in porcelain.lines() {
        if line.len() < 2 {
            continue;
        }
        if line.starts_with("??") {
            untracked += 1;
        } else {
            modified += 1;
        }
    }
    (modified, untracked)
}

#[cfg(target_arch = "wasm32")]
fn status_is_open(s: Option<&str>) -> bool {
    match s {
        Some(v) => super::prd::status_is_open(v),
        None => true,
    }
}

#[cfg(target_arch = "wasm32")]
fn prd_empty_or_missing() -> bool {
    let prd = gm_dir().join("prd.yml");
    let ps = prd.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return true;
    }
    match pkfs::read_to_string(&ps) {
        Some(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return true;
            }
            if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
                let items_opt = yaml
                    .as_sequence()
                    .or_else(|| yaml.get("items").and_then(|v| v.as_sequence()));
                if let Some(items) = items_opt {
                    if items.is_empty() {
                        return true;
                    }
                    let any_open = items.iter().any(|item| {
                        let status = item.get("status").and_then(|v| v.as_str());
                        let blocked_external = item
                            .get("blockedBy")
                            .and_then(|v| v.as_sequence())
                            .map(|seq| seq.iter().any(|x| x.as_str() == Some("external")))
                            .unwrap_or(false);
                        status_is_open(status) && !blocked_external
                    });
                    return !any_open;
                }
            }
            true
        }
        None => true,
    }
}

#[cfg(target_arch = "wasm32")]
fn running_tasks_exist() -> bool {
    super::task::any_running()
}

#[cfg(target_arch = "wasm32")]
fn deviation_scan_result(
    payload: serde_json::Value,
    severity: super::deviations::Severity,
    reason: &str,
) -> (String, String, i32) {
    match severity {
        super::deviations::Severity::Deny => (payload.to_string(), reason.to_string(), 1),
        super::deviations::Severity::Log => (payload.to_string(), String::new(), 0),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle_scan(_content: &str) -> (String, String, i32) {
    (
        "{\"ok\":false,\"error\":\"residual-scan requires wasm32\"}".to_string(),
        String::new(),
        1,
    )
}

#[cfg(target_arch = "wasm32")]
pub fn handle_scan(_content: &str) -> (String, String, i32) {
    let marker = gm_dir().join("residual-check-fired");

    let enabled = crate::orchestrator::fsm::graph()
        .policy
        .residual_checks
        .clone();
    let on = |k: &str| enabled.iter().any(|c| c == k);

    if on("prd-open") && !prd_empty_or_missing() {
        let reason = crate::prose::resolve_and_mark("residual/prd-open", RESIDUAL_PRD_OPEN_DEFAULT);
        let severity = super::deviations::effective_severity("residual-premature");
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": reason.clone(),
            "deviation_kind": "residual-premature",
            "deviation_severity": severity.as_str(),
            "next_dispatch": "prd-list",
            "next_dispatch_hint": "prd-list"
        });
        return deviation_scan_result(payload, severity, &reason);
    }

    if on("tasks-running") && running_tasks_exist() {
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": crate::prose::resolve_and_mark("residual/tasks-running", RESIDUAL_TASKS_RUNNING_DEFAULT),
            "next_dispatch": "phase-status",
            "next_dispatch_hint": "phase-status"
        });
        return (payload.to_string(), String::new(), 0);
    }

    let porcelain = porcelain_output();
    if on("dirty-tree") && !porcelain.trim().is_empty() {
        let (modified, untracked) = count_modified_untracked(&porcelain);
        let reason = crate::prose::fill_placeholders(
            "residual/dirty-tree",
            &crate::prose::resolve_and_mark("residual/dirty-tree", RESIDUAL_DIRTY_TREE_DEFAULT),
            &[
                ("modified", modified.to_string()),
                ("untracked", untracked.to_string()),
            ],
        );
        let severity = super::deviations::effective_severity("residual-dirty-tree");
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": reason.clone(),
            "deviation_kind": "residual-dirty-tree",
            "deviation_severity": severity.as_str(),
            "modified": modified,
            "untracked": untracked
        });
        return deviation_scan_result(payload, severity, &reason);
    }

    let marker_s = marker.to_string_lossy().to_string();
    let fired_sid = super::state::read_state().session_id.unwrap_or_default();
    let fired_at_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let _ = pkfs::write(&marker_s, &format!("{}:{}", fired_sid, fired_at_ms));

    let message =
        crate::prose::resolve_and_mark("residual/imperative", RESIDUAL_IMPERATIVE_DEFAULT);
    let mut payload = serde_json::json!({
        "scan": "fired",
        "marker": marker.display().to_string(),
        "imperative": message,
        "checks": ["worktree-clean", "remote-pushed", "prd-empty", "mutables-witnessed"],
    });
    if let Some(finding) = liqology_stale_memory_finding() {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("liqology_stale_memory".to_string(), finding);
        }
    }
    (payload.to_string(), String::new(), 0)
}

#[cfg(target_arch = "wasm32")]
fn liqology_stale_memory_finding() -> Option<serde_json::Value> {
    let resp =
        crate::wasm_dispatch::plugin_call("liqology", "prune_report", &serde_json::json!({}));
    if !crate::wasm_dispatch::plugin_ok(&resp) {
        return None;
    }
    let would_evict = resp.get("would_evict_ids").and_then(|v| v.as_array())?;
    let retained = resp
        .get("retained_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if would_evict.is_empty() || retained == 0 {
        return None;
    }
    let evict_count = would_evict.len() as u64;
    if evict_count * 3 < retained {
        return None;
    }
    Some(serde_json::json!({
        "would_evict_count": evict_count,
        "retained_count": retained,
        "note": "a third or more of liqology's tracked interaction history would be pruned under its current policy -- dispatch host_plugin_call(liqology, prune_report/tune_policy) to review",
    }))
}

