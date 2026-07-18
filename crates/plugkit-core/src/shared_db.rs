#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

pub const SHARED_DB: &str = "gm";

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_plugin_call(
        plugin_ptr: *const u8,
        plugin_len: u32,
        verb_ptr: *const u8,
        verb_len: u32,
        body_ptr: *const u8,
        body_len: u32,
    ) -> u64;
}

fn call_plugin(plugin: &str, verb: &str, body: Value) -> Value {
    let body_str = body.to_string();
    let packed = unsafe {
        host_plugin_call(
            plugin.as_ptr(),
            plugin.len() as u32,
            verb.as_ptr(),
            verb.len() as u32,
            body_str.as_ptr(),
            body_str.len() as u32,
        )
    };
    crate::wasm_dispatch::unpack_to_value_pub(packed)
}

fn plugin_ok(resp: &Value) -> Result<(), String> {
    if resp.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("plugin call failed")
            .to_string())
    }
}

fn plugin_rows(resp: Value) -> Result<Value, String> {
    if resp.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(resp.get("rows").cloned().unwrap_or(Value::Array(vec![])))
    } else {
        Err(resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("plugin call failed")
            .to_string())
    }
}

/// agentplug-libsql is now stateless and process-wide shared across every
/// concurrently active project -- open/close are accepted-but-inert no-ops
/// plugin-side (every exec/query call already does its own atomic
/// open-operate-close cycle), and the plugin keys EVERY call purely off the
/// JSON body's `path` field (defaulting to `:memory:`, silently throwaway,
/// if absent) -- it never looks at `db`. `SHARED_DB` ("gm") is kept only as
/// a label for logging/doc-comment continuity; it is never sufficient on
/// its own to identify a db anymore. shared_ensure_open/shared_exec/
/// shared_query/shared_exec_params/shared_query_params below all resolve
/// and forward the CURRENT dispatch's real absolute path
/// (crate::code_index::project_db_path(None), fresh via host_cwd_string()
/// every call) rather than relying on a remembered path from an earlier
/// open() call -- callers no longer need to pre-open at all, though
/// shared_ensure_open is kept for callers that still call it explicitly
/// (now itself just a resolve+no-op-forward).
fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

pub fn shared_ensure_open(path: &str) -> Result<(), String> {
    let resp = call_plugin("libsql", "open", json!({ "db": SHARED_DB, "path": path }));
    plugin_ok(&resp)
}

pub fn recreate_shared_db(path: &str) -> Result<(), String> {
    let _ = call_plugin("libsql", "close", json!({ "db": SHARED_DB, "path": path }));
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{}", path, suffix));
    }
    shared_ensure_open(path)
}

pub fn is_malformed(err: &str) -> bool {
    err.contains("malformed")
}

pub fn recover_malformed_shared_db() -> bool {
    let path = shared_db_path();
    crate::wasm_dispatch::emit_event("shared_db_recreated", serde_json::json!({
        "path": path,
        "reason": "database disk image is malformed; derived state dropped for full rebuild",
    }));
    if let Err(e) = recreate_shared_db(&path) {
        crate::wasm_dispatch::emit_event("shared_db_recreate_failed", serde_json::json!({ "path": path, "error": e }));
        return false;
    }
    crate::rssearch_vectors::ensure_schema().is_ok()
}

pub fn shared_exec(sql: &str) -> Result<(), String> {
    let path = shared_db_path();
    let resp = call_plugin("libsql", "exec", json!({ "db": SHARED_DB, "path": path, "sql": sql }));
    plugin_ok(&resp)
}

pub fn shared_query(sql: &str) -> Result<Value, String> {
    let path = shared_db_path();
    let resp = call_plugin("libsql", "query", json!({ "db": SHARED_DB, "path": path, "sql": sql }));
    plugin_rows(resp)
}

pub fn shared_exec_params(sql: &str, params: &[&str]) -> Result<(), String> {
    let path = shared_db_path();
    let resp = call_plugin(
        "libsql",
        "exec_params",
        json!({ "db": SHARED_DB, "path": path, "sql": sql, "params": params }),
    );
    plugin_ok(&resp)
}

pub fn shared_query_params(sql: &str, params: &[&str]) -> Result<Value, String> {
    let path = shared_db_path();
    let resp = call_plugin(
        "libsql",
        "query_params",
        json!({ "db": SHARED_DB, "path": path, "sql": sql, "params": params }),
    );
    plugin_rows(resp)
}
