use std::process::{Command, Stdio};

fn browser_session_map_file() -> std::path::PathBuf {
    std::env::temp_dir().join("plugkit-browser-sessions.json")
}

pub fn close_sessions_for(claude_session_id: &str) {
    let (bin, prefix) = find_pw();
    let path = browser_session_map_file();
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let sessions: Vec<String> = map.get(claude_session_id)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    for id in &sessions {
        let mut del_args = prefix.clone();
        del_args.extend(["session".into(), "delete".into(), id.clone()]);
        let _ = Command::new(&bin).args(&del_args).stdout(Stdio::null()).stderr(Stdio::null()).output();
    }
    if !sessions.is_empty() {
        map.remove(claude_session_id);
        let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
    }
}

pub fn close_all_sessions() {
    let (bin, prefix) = find_pw();
    let mut args = prefix.clone();
    args.extend(["session".into(), "list".into()]);
    let out = Command::new(&bin)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines().skip(1) {
            if let Some(id) = line.split_whitespace().next() {
                if id.parse::<u32>().is_ok() {
                    let mut del_args = prefix.clone();
                    del_args.extend(["session".into(), "delete".into(), id.to_string()]);
                    let _ = Command::new(&bin).args(&del_args).stdout(Stdio::null()).stderr(Stdio::null()).output();
                }
            }
        }
    }
}

fn find_pw() -> (String, Vec<String>) {
    let npm_dir = if cfg!(windows) {
        std::env::var("APPDATA").map(|d| std::path::PathBuf::from(d).join("npm")).ok()
    } else {
        None
    };
    if let Some(ref dir) = npm_dir {
        let bin_js = dir.join("node_modules").join("playwriter").join("bin.js");
        if bin_js.exists() {
            return ("node".to_string(), vec![bin_js.to_string_lossy().to_string()]);
        }
    }
    ("playwriter".to_string(), vec![])
}
