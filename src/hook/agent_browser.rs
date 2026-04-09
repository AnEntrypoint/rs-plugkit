fn browser_session_map_file() -> std::path::PathBuf {
    std::env::temp_dir().join("plugkit-browser-sessions.json")
}

pub fn get_open_session_ids(claude_session_id: &str) -> Vec<String> {
    let path = browser_session_map_file();
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s) {
            if let Some(serde_json::Value::Array(arr)) = map.get(claude_session_id) {
                return arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            }
        }
    }
    vec![]
}

pub fn close_sessions_for(claude_session_id: &str) {
    let path = browser_session_map_file();
    let session_ids = get_open_session_ids(claude_session_id);
    if !session_ids.is_empty() {
        let pw = super::find_playwriter();
        for sid in &session_ids {
            let _ = std::process::Command::new(&pw)
                .args(["session", "delete", sid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
        }
        eprintln!("[browser] Deleted {} playwriter session(s) for claude session {}", session_ids.len(), claude_session_id);
    }
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s) {
            if map.remove(claude_session_id).is_some() {
                let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
            }
        }
    }
}

fn kill_relay_server() {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    for (_pid, proc) in sys.processes() {
        let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
        let is_relay = cmd.iter().any(|a: &String| a.contains("playwriter") && (a.contains("bin.js") || a.ends_with("playwriter")))
            && cmd.iter().any(|a: &String| a == "server" || a.contains("cdp-relay") || a.contains("relay"));
        let is_node_playwriter = cmd.iter().any(|a: &String| a.contains("playwriter"))
            && cmd.first().map(|s: &String| s.contains("node")).unwrap_or(false);
        if is_relay || is_node_playwriter {
            proc.kill();
        }
    }
}

pub fn close_all_sessions() {
    kill_relay_server();
    std::fs::remove_file(browser_session_map_file()).ok();
}

