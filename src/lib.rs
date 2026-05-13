#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::background_tasks;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::daemon;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::rpc_client;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::runner;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::runtime;

pub use rs_codeinsight::{analyze, collect_files, matches_ignore_pattern, AnalyzeOptions, AnalysisOutput};

pub use rs_search::{bm25, context, run_search, scanner};
#[cfg(not(target_arch = "wasm32"))]
pub use rs_search::mcp as search_mcp;

#[cfg(target_arch = "wasm32")]
pub mod wasm_dispatch;

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_free(ptr: *mut u8, len: usize) {
    unsafe { let _ = Vec::from_raw_parts(ptr, len, len); }
}

#[cfg(target_arch = "wasm32")]
fn pack_result(s: String) -> u64 {
    let bytes = s.into_bytes();
    let len = bytes.len() as u64;
    let mut v = bytes;
    let ptr = v.as_mut_ptr() as u64;
    std::mem::forget(v);
    (ptr & 0xffff_ffff) | (len << 32)
}

#[cfg(target_arch = "wasm32")]
fn read_input(ptr: *const u8, len: usize) -> serde_json::Value {
    if ptr.is_null() || len == 0 { return serde_json::Value::Null; }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    serde_json::from_slice(bytes).unwrap_or(serde_json::Value::Null)
}

#[cfg(target_arch = "wasm32")]
mod wasm_hooks {
    use serde_json::{json, Value};
    use std::path::PathBuf;

    pub fn project_dir() -> Option<PathBuf> {
        std::env::var("CLAUDE_PROJECT_DIR").ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
    }

    pub fn gm_dir() -> Option<PathBuf> {
        project_dir().map(|p| p.join(".gm"))
    }

    pub fn ensure_gm_dir() -> Option<PathBuf> {
        let d = gm_dir()?;
        let _ = std::fs::create_dir_all(&d);
        Some(d)
    }

    pub fn read_marker(name: &str) -> bool {
        gm_dir().map(|d| d.join(name).exists()).unwrap_or(false)
    }

    pub fn write_marker(name: &str) {
        if let Some(d) = ensure_gm_dir() { let _ = std::fs::write(d.join(name), "1"); }
    }

    pub fn clear_marker(name: &str) {
        if let Some(d) = gm_dir() { let _ = std::fs::remove_file(d.join(name)); }
    }

    pub fn read_file(name: &str) -> String {
        gm_dir()
            .and_then(|d| std::fs::read_to_string(d.join(name)).ok())
            .unwrap_or_default()
    }

    pub fn pre_tool_use(input: &Value) -> Value {
        let tool_name = input.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let needs_gm = read_marker("needs-gm");
        let gm_fired = read_marker("gm-fired-this-turn");

        let is_skill = tool_name == "Skill";
        let is_agent = tool_name == "Agent" || tool_name == "Task";
        let skill_name = input.get("tool_input").and_then(|v| v.get("skill")).and_then(|v| v.as_str()).unwrap_or("");
        let agent_type = input.get("tool_input").and_then(|v| v.get("subagent_type")).and_then(|v| v.as_str()).unwrap_or("");
        let invokes_gm = (is_skill && skill_name == "gm:gm") || (is_agent && agent_type == "gm:gm");

        if invokes_gm {
            write_marker("gm-fired-this-turn");
            return json!({
                "continue": true,
                "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow" }
            });
        }

        if needs_gm && !gm_fired {
            return json!({
                "continue": true,
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "HARD CONSTRAINT: invoke gm before any other tool. Either Skill(skill=\"gm:gm\") OR Agent(subagent_type=\"gm:gm\") satisfies the gate. Subagent form is preferred — it isolates the orchestration loop in its own context. Must be the first action after every user message."
                }
            });
        }

        json!({ "continue": true, "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow" } })
    }

    pub fn post_tool_use(input: &Value) -> Value {
        let tool_name = input.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        if tool_name == "Skill" {
            let skill = input.get("tool_input").and_then(|v| v.get("skill")).and_then(|v| v.as_str()).unwrap_or("");
            if !skill.is_empty() {
                if let Some(d) = ensure_gm_dir() { let _ = std::fs::write(d.join("lastskill"), skill); }
            }
        }
        json!({ "continue": true })
    }

    pub fn session_start(_input: &Value) -> Value {
        let policy = "=== gm response policy (always active) ===\n\nTerse. Technical substance stays. Fluff dies. Drop articles/filler/pleasantries/hedging. Fragments OK. Pattern: [thing] [action] [reason]. [next step].\n\nInvoke Skill(\"gm:gm\") as the first tool after every user message. The pre-tool-use gate enforces this.";
        clear_marker("gm-fired-this-turn");
        clear_marker("residual-check-fired");
        write_marker("needs-gm");
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": policy
            }
        })
    }

    pub fn session_end(_input: &Value) -> Value {
        clear_marker("gm-fired-this-turn");
        clear_marker("residual-check-fired");
        clear_marker("needs-gm");
        if let Some(d) = gm_dir() { let _ = std::fs::write(d.join("turn-state.json"), "{}"); }
        json!({ "continue": true })
    }

    pub fn prompt_submit(input: &Value) -> Value {
        clear_marker("gm-fired-this-turn");
        clear_marker("residual-check-fired");
        write_marker("needs-gm");
        let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let mut ctx = String::from("Invoke Skill(gm:gm) first. The gate enforces this.\n");
        if !prompt.is_empty() {
            ctx.push_str(&format!("\nUser prompt: {}\n", prompt.chars().take(280).collect::<String>()));
        }
        json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": ctx
            }
        })
    }

    pub fn pre_compact(_input: &Value) -> Value {
        write_marker("needs-gm");
        let policy = "=== RESPONSE POLICY — ALWAYS ACTIVE (post-compact reinforcement) ===\n\nTerse. Drop filler. Pattern: [thing] [action] [reason]. [next step].\n\n=== POST-COMPACT FIRST RESPONSE — HARD RULE ===\n\nThe very next response after this compaction MUST call Skill(\"gm:gm\") as the FIRST tool invocation.";
        json!({ "systemMessage": policy })
    }

    pub fn post_compact(_input: &Value) -> Value {
        let last = read_file("lastskill");
        let last = last.trim();
        if last.is_empty() {
            return json!({ "continue": true });
        }
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PostCompact",
                "additionalContext": format!("Last active skill before compaction: `{0}`. Invoke the Skill tool with skill: \"{0}\" to resume it.", last)
            }
        })
    }

    pub fn stop(_input: &Value) -> Value {
        let prd = read_file("prd.yml");
        let prd_trim = prd.trim();
        if !prd_trim.is_empty() {
            write_marker("needs-gm");
            return json!({
                "decision": "block",
                "reason": format!("Work items remain in .gm/prd.yml. Remove completed items as they finish. Delete the file when all items are done.\n\n{}\n\nNEXT ACTION: invoke Skill(gm) first.", prd_trim)
            });
        }
        let muts = read_file("mutables.yml");
        if muts.contains("status: unknown") {
            write_marker("needs-gm");
            return json!({
                "decision": "block",
                "reason": "Cannot stop while .gm/mutables.yml has unresolved mutables. Resolve each unknown by witness; update mutables.yml entries to status: witnessed.\n\nNEXT ACTION: invoke Skill(gm) first."
            });
        }
        if !read_marker("residual-check-fired") {
            write_marker("residual-check-fired");
            write_marker("needs-gm");
            return json!({
                "decision": "block",
                "reason": "Residual scan before stop. PRD is empty, but the user's ask may still have reachable in-spirit residuals not yet captured. Enumerate every residual that is (a) within the spirit of the original ask AND (b) reachable from this session.\n\nIf any reachable residual exists: re-enter Skill(gm:planning), append PRD items, execute through to COMPLETE.\nIf zero reachable in-spirit residuals exist: state that explicitly in one line and stop again.\n\nNEXT ACTION: invoke Skill(gm) first."
            });
        }
        json!({ "decision": "approve" })
    }

    pub fn stop_git(_input: &Value) -> Value {
        json!({ "decision": "approve" })
    }
}

#[cfg(target_arch = "wasm32")]
macro_rules! wasm_hook {
    ($fn_name:ident, $impl:expr) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(ptr: *const u8, len: usize) -> u64 {
            let input = read_input(ptr, len);
            let out = $impl(&input);
            pack_result(out.to_string())
        }
    };
}

#[cfg(target_arch = "wasm32")] wasm_hook!(hook_pre_tool_use,       wasm_hooks::pre_tool_use);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_post_tool_use,      wasm_hooks::post_tool_use);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_session_start,      wasm_hooks::session_start);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_session_end,        wasm_hooks::session_end);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_user_prompt_submit, wasm_hooks::prompt_submit);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_prompt_submit,      wasm_hooks::prompt_submit);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_pre_compact,        wasm_hooks::pre_compact);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_post_compact,       wasm_hooks::post_compact);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_stop,               wasm_hooks::stop);
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_stop_git,           wasm_hooks::stop_git);
