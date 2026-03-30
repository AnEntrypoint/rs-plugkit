use std::io::Read;

pub fn run() {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: serde_json::Value = serde_json::from_str(&stdin).unwrap_or_default();
    let session_id = data["session_id"].as_str().unwrap_or("");

    if !session_id.is_empty() {
        let bin = super::plugkit_bin();
        let _ = std::process::Command::new(&bin)
            .args(["session-cleanup", &format!("--session={}", session_id)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}
