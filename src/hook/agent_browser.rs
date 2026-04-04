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
    let mut children: Vec<std::process::Child> = sessions.iter().filter_map(|id| {
        let mut del_args = prefix.clone();
        del_args.extend(["session".into(), "delete".into(), id.clone()]);
        Command::new(&bin).args(&del_args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()
    }).collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        if children.is_empty() || std::time::Instant::now() > deadline { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for c in &mut children { let _ = c.kill(); }
    if !sessions.is_empty() {
        map.remove(claude_session_id);
        let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
    }
}

pub fn close_all_sessions() {
    let (bin, prefix) = find_pw();
    let mut args = prefix.clone();
    args.extend(["session".into(), "list".into()]);
    let mut list_child = match Command::new(&bin)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match list_child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() > deadline => { let _ = list_child.kill(); return; }
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    let text = match list_child.wait_with_output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return,
    };
    let ids: Vec<String> = text.lines().skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|id| id.parse::<u32>().is_ok())
        .map(|s| s.to_string())
        .collect();
    let mut children: Vec<std::process::Child> = ids.iter().filter_map(|id| {
        let mut del_args = prefix.clone();
        del_args.extend(["session".into(), "delete".into(), id.clone()]);
        Command::new(&bin).args(&del_args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()
    }).collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        if children.is_empty() || std::time::Instant::now() > deadline { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for c in &mut children { let _ = c.kill(); }
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
