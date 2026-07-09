#[cfg(target_arch = "wasm32")]
pub mod wasm_dispatch;

#[cfg(target_arch = "wasm32")]
pub mod libsql_wasm;

#[cfg(target_arch = "wasm32")]
pub mod shared_db;

#[cfg(target_arch = "wasm32")]
pub mod code_index;

#[cfg(target_arch = "wasm32")]
pub mod embed;

#[cfg(target_arch = "wasm32")]
pub mod pipeline;

#[cfg(target_arch = "wasm32")]
pub mod gitignore;

#[cfg(target_arch = "wasm32")]
pub mod gates;

#[cfg(target_arch = "wasm32")]
pub mod browser_witness;

#[cfg(target_arch = "wasm32")]
pub mod poll_detect;

#[cfg(target_arch = "wasm32")]
pub mod rssearch_vectors;

#[cfg(target_arch = "wasm32")]
pub mod git_commit_vectors;

#[cfg(target_arch = "wasm32")]
pub mod rslearn_vectors;

#[cfg(target_arch = "wasm32")]
pub mod memory_md;

pub mod pkfs;
pub mod prose;
pub mod orchestrator;
pub mod filter;
pub mod validation;

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
pub unsafe extern "C" fn plugkit_free(ptr: *mut u8, len: usize) {
    let _ = Vec::from_raw_parts(ptr, len, len);
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
    use crate::wasm_dispatch::{host_read, host_write, host_exists};

    fn path_for(name: &str) -> String {
        format!(".gm/{}", name)
    }

    pub fn read_marker(name: &str) -> bool {
        let _ = host_exists;
        let s = host_read(&path_for(name)).unwrap_or_default();
        !s.is_empty()
    }

    pub fn write_marker(name: &str) {
        let _ = host_write(&path_for(name), "1");
    }

    pub fn clear_marker(name: &str) {
        let _ = host_write(&path_for(name), "");
    }

    pub fn read_file(name: &str) -> String {
        host_read(&path_for(name)).unwrap_or_default()
    }

    fn signal_platform_search_drift(tool_name: &str) {
        let ts = read_file("turn-state.json");
        let phase = serde_json::from_str::<Value>(&ts).ok()
            .and_then(|v| v.get("phase").and_then(|p| p.as_str()).map(|p| p.to_string()))
            .unwrap_or_default();
        if phase.is_empty() || phase == "COMPLETE" { return; }
        let evt = json!({
            "event": "deviation.platform-search-drift",
            "sub": "hook",
            "detail": format!("tool={} during in-flight chain (phase={}); codesearch/recall are the discovery surfaces, platform Grep/Glob is exploration outside the spool", tool_name, phase),
            "ts": crate::orchestrator::state::now_ms() as u64,
            "source": "rs-plugkit/hooks",
        });
        let line = format!("evt: {}", evt);
        unsafe { crate::wasm_dispatch::host_log(1, line.as_ptr(), line.len() as u32); }
    }

    pub fn pre_tool_use(input: &Value) -> Value {
        let tool_name = input.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        if tool_name == "Grep" || tool_name == "Glob" {
            signal_platform_search_drift(tool_name);
        }
        let needs_gm = read_marker("needs-gm");
        let gm_fired = read_marker("gm-fired-this-turn");

        let is_skill = tool_name == "Skill";
        let is_agent = tool_name == "Agent" || tool_name == "Task";
        let skill_name = input.get("tool_input").and_then(|v| v.get("skill")).and_then(|v| v.as_str()).unwrap_or("");
        let agent_type = input.get("tool_input").and_then(|v| v.get("subagent_type")).and_then(|v| v.as_str()).unwrap_or("");
        let is_gm_name = |n: &str| n == "gm-skill" || n == "gm:gm";
        let invokes_gm = (is_skill && is_gm_name(skill_name)) || (is_agent && is_gm_name(agent_type));

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
                    "permissionDecisionReason": "HARD CONSTRAINT: dispatch the gm-skill instruction verb before any other tool. Write .gm/exec-spool/in/instruction/<N>.txt with {\"prompt\":\"<request>\"} and read the response. This is the portable gate -- it works identically on every agent because it depends only on the spool, not on any tool-specific invocation form."
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
                let _ = host_write(&path_for("lastskill"), skill);
            }
        }
        json!({ "continue": true })
    }

    pub fn session_start(_input: &Value) -> Value {
        let policy = "=== gm response policy (always active) ===\n\nTerse. Technical substance stays. Fluff dies. Drop articles/filler/pleasantries/hedging. Fragments OK. Pattern: [thing] [action] [reason]. [next step].\n\nDispatch the gm-skill instruction verb (write .gm/exec-spool/in/instruction/<N>.txt, read the response) as the first action after every user message. A turn that ends without a tool call is a stop; never summarize until the instruction surface returns phase=COMPLETE. This gate is spool-only, identical on every agent.";
        clear_marker("gm-fired-this-turn");
        clear_marker("residual-check-fired");
        write_marker("needs-gm");
        let _ = crate::gitignore::ensure_managed_gitignore("");
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
        let _ = host_write(&path_for("turn-state.json"), "{}");
        json!({ "continue": true })
    }

    pub fn prompt_submit(input: &Value) -> Value {
        clear_marker("gm-fired-this-turn");
        clear_marker("residual-check-fired");
        write_marker("needs-gm");
        let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if !prompt.is_empty() {
            let _ = host_write(&path_for("last-prompt.txt"), prompt);
        }
        let mut ctx = String::from("Invoke the gm-skill first, then dispatch the instruction verb (write .gm/exec-spool/in/instruction/<N>.txt, read the response). The spool-dispatch gate enforces this.\n");
        if !prompt.is_empty() {
            ctx.push_str(&format!("\nUser prompt: {}\n", prompt.chars().take(280).collect::<String>()));
        }
        let next_step = host_read(&path_for("next-step.md")).unwrap_or_default();
        let next_step_trim = next_step.trim();
        if !next_step_trim.is_empty() {
            ctx.push_str(&format!("\n=== CURRENT NEXT STEP (from .gm/next-step.md) ===\n\n{}\n", next_step_trim));
        }
        let prd = host_read(&path_for("prd.yml")).unwrap_or_default();
        let open_count = prd.lines().filter(|l| {
            let t = l.trim_start().trim_start_matches("- ");
            match t.strip_prefix("status:") {
                Some(v) => crate::orchestrator::prd::status_is_open(v),
                None => false,
            }
        }).count();
        if open_count > 0 {
            ctx.push_str(&format!("\n{} open PRD item(s) -- finish what's already planned before adding new scope.\n", open_count));
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
        let policy = "=== RESPONSE POLICY -- ALWAYS ACTIVE (post-compact reinforcement) ===\n\nTerse. Drop filler. Pattern: [thing] [action] [reason]. [next step].\n\n=== POST-COMPACT FIRST RESPONSE -- HARD RULE ===\n\nThe very next response after this compaction invokes the gm-skill and dispatches the instruction verb as the FIRST action.";
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
                "reason": format!("Work items remain in .gm/prd.yml. Remove completed items as they finish. Delete the file when all items are done.\n\n{}\n\nNEXT ACTION: invoke the gm-skill and dispatch the instruction verb first.", prd_trim)
            });
        }
        let muts = read_file("mutables.yml");
        if muts.contains("status: unknown") {
            write_marker("needs-gm");
            return json!({
                "decision": "block",
                "reason": "Cannot stop while .gm/mutables.yml has unresolved mutables. Resolve each unknown by witness; update mutables.yml entries to status: witnessed.\n\nNEXT ACTION: invoke the gm-skill and dispatch the instruction verb first."
            });
        }
        if !read_marker("residual-check-fired") {
            write_marker("residual-check-fired");
            write_marker("needs-gm");
            return json!({
                "decision": "block",
                "reason": "Residual scan before stop. PRD is empty, but the user's ask may still have reachable in-spirit residuals not yet captured. Enumerate every residual that is (a) within the spirit of the original ask AND (b) reachable from this session.\n\nIf any reachable residual exists: dispatch prd-add to append PRD items, transition to PLAN, and execute through to COMPLETE.\nIf zero reachable in-spirit residuals exist: state that explicitly in one line and stop again.\n\nNEXT ACTION: invoke the gm-skill and dispatch the instruction verb first."
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
