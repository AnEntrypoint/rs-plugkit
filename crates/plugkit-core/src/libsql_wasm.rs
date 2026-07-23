#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::orchestrator::yaml_util::{base64_decode, base64_encode};
use crate::wasm_dispatch::{host_cwd_string, plugin_call};

/// agentplug-libsql is now stateless and shared as ONE process-wide instance
/// across every concurrently active project -- every call opens the db
/// fresh, does its one operation, and closes it before returning (see that
/// plugin's src/db.rs `handle()` doc comment). `open`/`close`/`begin`/
/// `commit`/`rollback`/`finalize` are accepted-but-inert no-ops on the
/// plugin side now; there is no persistent connection to remember, so a
/// bare `name` (formerly a lookup key into a name->connection map) means
/// nothing anymore. Every call below takes a real, absolute `path` and
/// forwards it in the JSON body's `path` field -- the plugin defaults to
/// `:memory:` (silently throwaway) when `path` is absent, so never omit it.
fn plugin_ok_err(resp: &Value) -> Result<(), String> {
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("libsql plugin call failed").to_string())
    }
}

fn plugin_ok_rows(resp: &Value) -> Result<Value, String> {
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        Ok(resp.get("rows").cloned().unwrap_or(Value::Array(Vec::new())))
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("libsql plugin call failed").to_string())
    }
}

/// Resolves `<host_cwd>/.gm/<filename>` fresh on every call -- host_cwd is
/// asked of the host per-dispatch (never cached wasm-side, see
/// host_cwd_string's own doc comment), since the same wasm instance may be
/// serving a different project's dispatch on the very next call. Returns
/// `filename` unchanged if host_cwd is unavailable (e.g. a loader that
/// hasn't wired the import yet, or `:memory:`/an already-absolute path
/// passed straight through) rather than fabricating a bad path -- callers
/// needing a guaranteed-absolute path should check the host_cwd_string()
/// Option themselves if that distinction matters.
pub fn absolute_db_path(filename: &str) -> String {
    if filename.is_empty() || filename == ":memory:" || filename.starts_with('/') || (filename.len() > 1 && filename.as_bytes()[1] == b':') {
        return filename.to_string();
    }
    match host_cwd_string() {
        Some(cwd) if !cwd.is_empty() => {
            let cwd = cwd.trim_end_matches(['/', '\\']);
            format!("{}/.gm/{}", cwd, filename)
        }
        _ => filename.to_string(),
    }
}

pub fn open(path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "open", &json!({ "path": path }));
    plugin_ok_err(&resp)
}

pub fn close(path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "close", &json!({ "path": path }));
    plugin_ok_err(&resp)
}

pub fn exec(path: &str, sql: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "exec", &json!({ "path": path, "sql": sql }));
    plugin_ok_err(&resp)
}

pub fn query(path: &str, sql: &str) -> Result<Value, String> {
    let resp = plugin_call("libsql", "query", &json!({ "path": path, "sql": sql }));
    plugin_ok_rows(&resp)
}

pub fn exec_params(path: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    let resp = plugin_call("libsql", "exec_params", &json!({ "path": path, "sql": sql, "params": params }));
    plugin_ok_err(&resp)
}

pub fn query_params(path: &str, sql: &str, params: &[&str]) -> Result<Value, String> {
    let resp = plugin_call("libsql", "query_params", &json!({ "path": path, "sql": sql, "params": params }));
    plugin_ok_rows(&resp)
}

/// begin/commit/rollback are accepted-but-inert on the now-stateless plugin
/// side (every exec/query call is already its own atomic open-operate-close
/// cycle) -- kept as no-op-forwarding calls rather than removed outright so
/// existing call sites (code_index::index()'s batch loop) don't need every
/// transaction-boundary call site ripped out in the same change; each row
/// in a batch now commits independently regardless of whether begin/commit
/// wrap it.
pub fn begin(path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "begin", &json!({ "path": path }));
    plugin_ok_err(&resp)
}
pub fn commit(path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "commit", &json!({ "path": path }));
    plugin_ok_err(&resp)
}
pub fn rollback(path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "rollback", &json!({ "path": path }));
    plugin_ok_err(&resp)
}

/// Prepare/execute_bound/finalize as a cross-call handle sequence no longer
/// exists plugin-side (a prepared statement handle is inherently
/// incompatible with per-call open-operate-close statelessness) -- the
/// plugin collapsed it into a single `prepare_execute` verb that prepares,
/// binds, steps, and finalizes atomically within one call, same shape as
/// exec_params. This one-shot wrapper replaces the old two-step
/// prepare()->PreparedStmt::execute_bound() API; callers doing a
/// prepare-once/execute-many bulk-insert loop (code_index::index()) now
/// call this once per row instead of once per loop, paying one
/// open+prepare+bind+step+finalize per row rather than amortizing prepare
/// across the loop -- a real, deliberate cost accepted in exchange for zero
/// persistent state (see agentplug-libsql's src/db.rs handle() doc comment).
pub fn prepare_execute(path: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    let resp = plugin_call("libsql", "prepare_execute", &json!({ "path": path, "sql": sql, "params": params }));
    plugin_ok_err(&resp)
}

pub fn serialize(path: &str) -> Result<Vec<u8>, String> {
    let resp = plugin_call("libsql", "serialize", &json!({ "path": path }));
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("serialize failed").to_string());
    }
    let b64 = resp.get("data").and_then(|v| v.as_str())
        .ok_or_else(|| "serialize: missing data in plugin response".to_string())?;
    base64_decode(b64)
}

pub fn deserialize(path: &str, bytes: &[u8]) -> Result<(), String> {
    let b64 = base64_encode(bytes);
    let resp = plugin_call("libsql", "deserialize", &json!({ "path": path, "data": b64 }));
    plugin_ok_err(&resp)
}

pub fn smoke() -> Value {
    let mut log: Vec<Value> = Vec::new();
    let p = ":memory:";
    log.push(json!({ "step": "open", "result": open(p).err() }));
    log.push(json!({ "step": "create_table", "result": exec(p, "CREATE TABLE memos (id INTEGER PRIMARY KEY, text TEXT, emb F32_BLOB(4))").err() }));
    log.push(json!({ "step": "insert", "result": exec(p, "INSERT INTO memos(text, emb) VALUES ('hello', vector('[0.1,0.2,0.3,0.4]'))").err() }));
    log.push(json!({ "step": "create_index", "result": exec(p, "CREATE INDEX memos_idx ON memos(libsql_vector_idx(emb, 'metric=cosine'))").err() }));
    log.push(json!({ "step": "vector_top_k", "rows": query(p, "SELECT id, text, vector_distance_cos(emb, vector('[0.1,0.2,0.3,0.4]')) AS d FROM vector_top_k('memos_idx', vector('[0.1,0.2,0.3,0.4]'), 5) JOIN memos ON memos.rowid = id").ok() }));
    let _ = close(p);
    json!({ "ok": true, "smoke": log, "libsql_version": "delegated" })
}

