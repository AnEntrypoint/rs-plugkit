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
    let bin = super::plugkit_bin();
    let _ = std::process::Command::new(&bin)
        .args([rs_exec::SUBCMD_SESSION_CLEANUP, &format!("--session={}", session_id)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
