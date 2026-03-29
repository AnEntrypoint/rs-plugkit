mod session_start;
mod pre_tool_use;
mod prompt_submit;
mod stop;
pub mod agent_browser;

pub use session_start::run as session_start;
pub use pre_tool_use::run as pre_tool_use;
pub use prompt_submit::run as prompt_submit;
pub use stop::run_stop;
pub use stop::run_stop_git;

use std::{env, path::PathBuf};

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
    let tmp = env::temp_dir().join(format!("gm-out-{}.txt", ts));
    let _ = fs::write(&tmp, context);
    let tmp_str = tmp.to_string_lossy();
    let tmp_unix = tmp_str.replace('\\', "/");
    let cmd = format!("cat '{}' && rm -f '{}'", tmp_unix, tmp_unix);
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": cmd }
        }
    })
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

pub fn run_self(args: &[&str]) -> String {
    let bin = env::current_exe().unwrap_or_else(|_| plugkit_bin());
    match std::process::Command::new(&bin).args(args).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            format!("{}{}", stdout, stderr).trim().to_string()
        }
        Err(_) => String::new(),
    }
}
