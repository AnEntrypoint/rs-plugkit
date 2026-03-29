use std::process::{Command, Stdio};
use super::tools_dir;

pub fn close_all_sessions() {
    let bin = find_pw_bin();
    let out = Command::new(&bin)
        .args(["session", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines().skip(1) {
            if let Some(id) = line.split_whitespace().next() {
                if id.parse::<u32>().is_ok() {
                    let _ = Command::new(&bin).args(["session", "reset", id]).output();
                }
            }
        }
    }
}

fn find_pw_bin() -> String {
    let dir = tools_dir();
    let ext = if cfg!(windows) { ".cmd" } else { "" };
    let local = dir.join("node_modules").join(".bin").join(format!("playwriter{}", ext));
    if local.exists() { return local.to_string_lossy().to_string(); }
    "playwriter".to_string()
}
