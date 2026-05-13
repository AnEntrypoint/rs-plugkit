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
fn hook_allow_response(event: &str, input: &serde_json::Value) -> String {
    serde_json::json!({
        "continue": true,
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": format!("[plugkit.wasm v{}] {} hook seen for {}",
                env!("CARGO_PKG_VERSION"),
                event,
                input.get("tool_name").and_then(|v| v.as_str())
                    .or_else(|| input.get("source").and_then(|v| v.as_str()))
                    .unwrap_or("unknown"))
        }
    }).to_string()
}

#[cfg(target_arch = "wasm32")]
macro_rules! wasm_hook {
    ($fn_name:ident, $event:expr) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(ptr: *const u8, len: usize) -> u64 {
            let input = read_input(ptr, len);
            pack_result(hook_allow_response($event, &input))
        }
    };
}

#[cfg(target_arch = "wasm32")] wasm_hook!(hook_pre_tool_use,       "PreToolUse");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_post_tool_use,      "PostToolUse");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_session_start,      "SessionStart");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_session_end,        "SessionEnd");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_user_prompt_submit, "UserPromptSubmit");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_prompt_submit,      "UserPromptSubmit");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_pre_compact,        "PreCompact");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_post_compact,       "PostCompact");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_stop,               "Stop");
#[cfg(target_arch = "wasm32")] wasm_hook!(hook_stop_git,           "Stop");
