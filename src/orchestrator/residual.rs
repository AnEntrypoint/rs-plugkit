use std::fs;
use super::gm_dir;

fn prd_empty_or_missing() -> bool {
    let prd = gm_dir().join("prd.yml");
    if !prd.exists() {
        return true;
    }
    match fs::read_to_string(&prd) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return true;
            }
            if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
                if let Some(seq) = yaml.as_sequence() {
                    return seq.is_empty();
                }
                if let Some(items) = yaml.get("items").and_then(|v| v.as_sequence()) {
                    return items.is_empty();
                }
            }
            true
        }
        Err(_) => true,
    }
}

fn browser_sessions_open() -> bool {
    let marker = gm_dir().join("browser-sessions.json");
    if !marker.exists() {
        return false;
    }
    match fs::read_to_string(&marker) {
        Ok(s) => !s.trim().is_empty() && s.trim() != "{}" && s.trim() != "[]",
        Err(_) => false,
    }
}

fn running_tasks_exist() -> bool {
    let status = gm_dir().join("exec-spool").join(".status.json");
    if !status.exists() {
        return false;
    }
    match fs::read_to_string(&status) {
        Ok(s) => s.contains("\"running\"") || s.contains("\"runningTasks\":["),
        Err(_) => false,
    }
}

pub fn handle_scan(_content: &str) -> (String, String, i32) {
    let marker = gm_dir().join("residual-check-fired");

    if !prd_empty_or_missing() {
        let payload = serde_json::json!({
            "scan": "skipped",
            "reason": "PRD still has items; complete or remove them before residual scan."
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

    let _ = fs::create_dir_all(gm_dir());
    let _ = fs::write(&marker, "fired");

    let message = "Residual scan: name reachable in-spirit work and add to PRD, OR explicitly state 'residual scan: none reachable in-spirit'.";
    let payload = serde_json::json!({
        "scan": "fired",
        "marker": marker.display().to_string(),
        "imperative": message,
    });
    (payload.to_string(), String::new(), 0)
}
