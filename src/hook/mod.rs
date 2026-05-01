mod session_start;
mod session_end;
mod pre_tool_use;
mod post_tool_use;
mod prompt_submit;
mod pre_compact;
mod post_compact;
mod stop;
pub mod agent_browser;
pub mod rs_learn;

pub use session_start::run as session_start;
pub use session_end::run as session_end;
pub use pre_tool_use::run as pre_tool_use;
pub use post_tool_use::run as post_tool_use;
pub use prompt_submit::run as prompt_submit;
pub use pre_compact::run as pre_compact;
pub use post_compact::run as post_compact;
pub use stop::run_stop;
pub use stop::run_stop_git;

use std::{env, path::PathBuf, process::Command};

#[cfg(target_os = "windows")]
pub fn no_window_cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(program);
    cmd.creation_flags(0x08000000);
    cmd
}

#[cfg(not(target_os = "windows"))]
pub fn no_window_cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
}

pub fn tools_dir() -> PathBuf {
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".claude").join("gm-tools")
}

pub fn plugkit_bin() -> PathBuf {
    if let Ok(plugin_root) = env::var("CLAUDE_PLUGIN_ROOT") {
        let p = if cfg!(windows) {
            PathBuf::from(&plugin_root).join("bin").join("plugkit.exe")
        } else {
            PathBuf::from(&plugin_root).join("bin").join("plugkit")
        };
        if p.exists() { return p; }
    }
    let dir = tools_dir();
    if cfg!(windows) { dir.join("plugkit.exe") } else { dir.join("plugkit") }
}

pub fn find_playwriter() -> String {
    for candidate in &["playwriter", "playwriter.cmd"] {
        if no_window_cmd(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok()
        {
            return candidate.to_string();
        }
    }
    "playwriter".to_string()
}

pub fn project_dir() -> Option<String> {
    env::var("CLAUDE_PROJECT_DIR")
        .or_else(|_| env::var("GEMINI_PROJECT_DIR"))
        .or_else(|_| env::var("OC_PROJECT_DIR"))
        .or_else(|_| env::var("KILO_PROJECT_DIR"))
        .ok()
}

pub fn is_gemini() -> bool {
    env::var("GEMINI_PROJECT_DIR").is_ok()
}

pub fn is_opencode() -> bool {
    env::var("OC_PROJECT_DIR").is_ok()
}

pub fn is_kilo() -> bool {
    env::var("KILO_PROJECT_DIR").is_ok()
}

pub fn allow_with_noop(context: &str) -> serde_json::Value {
    use std::{fs, time::{SystemTime, UNIX_EPOCH}};
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let tmp = env::temp_dir().join(format!("plugkit-out-{}.txt", ts));
    let _ = fs::write(&tmp, context);
    let tmp_str = tmp.to_string_lossy();
    let tmp_unix = to_unix_path(&tmp_str);
    let cmd = format!("cat '{}' && rm -f '{}'", tmp_unix, tmp_unix);
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": cmd }
        }
    })
}

pub fn to_unix_path(p: &str) -> String {
    // Convert Windows path (C:\foo\bar) to Git Bash path (/c/foo/bar)
    if cfg!(windows) {
        if let Some(rest) = p.strip_prefix(|c: char| c.is_ascii_alphabetic()).and_then(|r| r.strip_prefix(':')) {
            let drive = p.chars().next().unwrap().to_ascii_lowercase();
            return format!("/{}{}", drive, rest.replace('\\', "/"));
        }
    }
    p.replace('\\', "/")
}

pub fn allow(additional_context: Option<&str>) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow"
        }
    });
    if let Some(ctx) = additional_context {
        obj["hookSpecificOutput"]["additionalContext"] = serde_json::Value::String(ctx.to_string());
    }
    obj
}

pub fn deny(reason: &str) -> serde_json::Value {
    if is_gemini() {
        serde_json::json!({ "decision": "deny", "reason": reason })
    } else {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        })
    }
}

pub fn load_prompt(key: &str) -> Option<String> {
    let plugin_root = env::var("CLAUDE_PLUGIN_ROOT").ok()?;
    let path = PathBuf::from(plugin_root).join("prompts").join(format!("{}.txt", key));
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

use std::sync::OnceLock;
use std::collections::HashMap;
use std::sync::Mutex;

fn reach_cache() -> &'static Mutex<HashMap<String, bool>> {
    static C: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse_owner_repo(url: &str) -> Option<(String, String)> {
    let u = url.trim().trim_end_matches(".git");
    let stripped = if let Some(rest) = u.strip_prefix("git@github.com:") { rest.to_string() }
        else if let Some(rest) = u.strip_prefix("https://github.com/") { rest.to_string() }
        else if let Some(rest) = u.strip_prefix("ssh://git@github.com/") { rest.to_string() }
        else { return None };
    let mut parts = stripped.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() { return None }
    Some((owner, repo))
}

pub fn user_can_push_to_remote(project_dir: &str) -> bool {
    if which::which("gh").is_err() { return false }
    {
        let cache = reach_cache().lock().ok();
        if let Some(map) = cache.as_deref() { if let Some(v) = map.get(project_dir) { return *v } }
    }
    let url = no_window_cmd("git").args(["remote", "get-url", "origin"]).current_dir(project_dir).output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let result = match url.as_deref().and_then(parse_owner_repo) {
        None => false,
        Some((owner, repo)) => {
            let api_path = format!("repos/{}/{}", owner, repo);
            let out = no_window_cmd("gh").args(["api", &api_path, "--jq", ".permissions.push"]).current_dir(project_dir).output().ok();
            out.filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
                .unwrap_or(false)
        }
    };
    if let Ok(mut m) = reach_cache().lock() { m.insert(project_dir.to_string(), result); }
    result
}

pub fn run_self(args: &[&str]) -> String {
    let started = std::time::Instant::now();
    let result = run_self_inner(args);
    rs_exec::obs::event("plugkit", "run_self", serde_json::json!({
        "subcmd": args.first().copied().unwrap_or(""),
        "argc": args.len(),
        "out_len": result.len(),
        "dur_ms": started.elapsed().as_millis() as u64
    }));
    result
}

fn run_self_inner(args: &[&str]) -> String {
    let bin = env::current_exe().unwrap_or_else(|_| plugkit_bin());
    let child = match no_window_cmd(&bin).args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let timeout = std::time::Duration::from_secs(16);
    let start = std::time::Instant::now();
    let mut child = child;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut o) = child.stdout.take() { let _ = std::io::Read::read_to_end(&mut o, &mut stdout); }
                if let Some(mut e) = child.stderr.take() { let _ = std::io::Read::read_to_end(&mut e, &mut stderr); }
                let _ = status;
                let so = String::from_utf8_lossy(&stdout).to_string();
                let se = String::from_utf8_lossy(&stderr).to_string();
                return format!("{}{}", so, se).trim().to_string();
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return "Error: exec timed out after 20s".to_string();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return String::new(),
        }
    }
}
