use super::project_dir;
use std::io::Read;

pub fn run() {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: serde_json::Value = serde_json::from_str(&stdin).unwrap_or_default();
    let session_id = data["session_id"].as_str().unwrap_or("");
    let reason = data["reason"].as_str().unwrap_or("");

    if session_id.is_empty() {
        return;
    }

    let is_real_exit = matches!(reason, "clear" | "logout" | "prompt_input_exit");
    if !is_real_exit {
        eprintln!(
            "[session-end] reason={:?} — keeping browser + tasks alive across session handoff.",
            reason
        );
        return;
    }

    eprintln!("[session-end] reason={:?} — full cleanup.", reason);
    super::agent_browser::close_sessions_for(session_id);
    rs_exec::runtime::kill_session_browser(session_id);

    let killed = match rs_exec::rpc_client::rpc_call_sync(
        rs_exec_port().unwrap_or(0),
        "killSessionTasks",
        serde_json::json!({ "sessionId": session_id }),
        5000,
    ) {
        Ok(v) => v.get("killed").and_then(|n| n.as_u64()).unwrap_or(0),
        Err(_) => 0,
    };
    if killed > 0 { eprintln!("[session-end] killed {} background tasks", killed); }

    if let Some(dir) = project_dir() {
        let gm = std::path::Path::new(&dir).join(".gm");
        let _ = std::fs::write(gm.join("turn-state.json"), "{}");
        let _ = std::fs::remove_file(gm.join("no-memorize-this-turn"));
    }

    let bin = super::plugkit_bin();
    let _ = super::no_window_cmd(&bin)
        .args([rs_exec::SUBCMD_SESSION_CLEANUP, &format!("--session={}", session_id)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn rs_exec_port() -> Option<u16> {
    let pf = std::env::var("RS_EXEC_PORT_FILE").map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("glootie-runner.port"));
    std::fs::read_to_string(pf).ok()?.trim().parse().ok()
}
