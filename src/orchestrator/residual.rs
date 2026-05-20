use super::gm_dir;
use crate::pkfs;

fn status_is_open(s: Option<&str>) -> bool {
    match s {
        Some(v) => matches!(v, "pending" | "in_progress" | "unknown" | "blocked"),
        None => true,
    }
}

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
                let items_opt = yaml.as_sequence()
                    .or_else(|| yaml.get("items").and_then(|v| v.as_sequence()));
                if let Some(items) = items_opt {
                    if items.is_empty() {
                        return true;
                    }
                    let any_open = items.iter().any(|item| {
                        let status = item.get("status").and_then(|v| v.as_str());
                        let blocked_external = item.get("blockedBy")
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

fn browser_sessions_open() -> bool {
    let candidates = [
        gm_dir().join("exec-spool").join("browser-sessions.json"),
        gm_dir().join("browser-sessions.json"),
    ];
    for marker in &candidates {
        let ps = marker.to_string_lossy().to_string();
        if !pkfs::exists(&ps) { continue; }
        if let Some(s) = pkfs::read_to_string(&ps) {
            let t = s.trim();
            if !t.is_empty() && t != "{}" && t != "[]" { return true; }
        }
    }
    false
}

fn running_tasks_exist() -> bool {
    super::task::any_running()
}

pub fn handle_scan(_content: &str) -> (String, String, i32) {
    let marker = gm_dir().join("residual-check-fired");

    if !prd_empty_or_missing() {
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": "PRD still has items; complete or remove them before residual scan.",
            "deviation_kind": "residual-premature"
        });
        return (payload.to_string(), String::new(), 0);
    }

    if browser_sessions_open() {
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": "browser sessions still open"
        });
        return (payload.to_string(), String::new(), 0);
    }

    if running_tasks_exist() {
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": "background tasks still running"
        });
        return (payload.to_string(), String::new(), 0);
    }

    let marker_s = marker.to_string_lossy().to_string();
    let _ = pkfs::write(&marker_s, "fired");

    let message = "Residual scan. Worktree clean, remote pushed, PRD empty, mutables witnessed — the four checks. Anything reachable and in-spirit expands the PRD and runs. Out-of-reach is credentials, down service, product decision.";
    let payload = serde_json::json!({
        "scan": "fired",
        "marker": marker.display().to_string(),
        "imperative": message,
        "checks": ["worktree-clean", "remote-pushed", "prd-empty", "mutables-witnessed"],
    });
    (payload.to_string(), String::new(), 0)
}
