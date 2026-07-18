#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

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

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(B64_CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { B64_CHARS[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64_CHARS[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = s.bytes().filter(|&c| c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3 + 3);
    for chunk in clean.chunks(4) {
        let mut n: u32 = 0;
        let mut bits = 0u32;
        for &c in chunk {
            let v = val(c).ok_or_else(|| "invalid base64 char".to_string())?;
            n = (n << 6) | v;
            bits += 6;
        }
        n <<= 24u32.saturating_sub(bits);
        let nbytes = (bits / 8) as usize;
        let b = n.to_be_bytes();
        out.extend_from_slice(&b[..nbytes]);
    }
    Ok(out)
}
