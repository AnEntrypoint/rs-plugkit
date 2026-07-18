#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;

use crate::wasm_dispatch::plugin_call;

static OPEN_DBS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

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

pub fn open(name: &str, path: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "open", &json!({ "db": name, "path": path }));
    plugin_ok_err(&resp)?;
    let mut guard = OPEN_DBS.lock().map_err(|e| e.to_string())?;
    guard.get_or_insert_with(HashSet::new).insert(name.to_string());
    Ok(())
}

pub fn close(name: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "close", &json!({ "db": name }));
    plugin_ok_err(&resp)?;
    if let Ok(mut guard) = OPEN_DBS.lock() {
        if let Some(set) = guard.as_mut() {
            set.remove(name);
        }
    }
    Ok(())
}

pub fn list_dbs() -> Vec<String> {
    let guard = OPEN_DBS.lock().ok();
    guard.as_ref().and_then(|g| g.as_ref()).map(|s| s.iter().cloned().collect()).unwrap_or_default()
}

pub fn exec(name: &str, sql: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "exec", &json!({ "db": name, "sql": sql }));
    plugin_ok_err(&resp)
}

pub fn query(name: &str, sql: &str) -> Result<Value, String> {
    let resp = plugin_call("libsql", "query", &json!({ "db": name, "sql": sql }));
    plugin_ok_rows(&resp)
}

pub fn exec_params(name: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    let resp = plugin_call("libsql", "exec_params", &json!({ "db": name, "sql": sql, "params": params }));
    plugin_ok_err(&resp)
}

pub fn query_params(name: &str, sql: &str, params: &[&str]) -> Result<Value, String> {
    let resp = plugin_call("libsql", "query_params", &json!({ "db": name, "sql": sql, "params": params }));
    plugin_ok_rows(&resp)
}

/// begin/commit/rollback wrap the default autocommit-per-statement mode into
/// a single fsync-backed transaction for callers issuing many INSERT/UPDATE
/// statements in a loop (e.g. code_index::index()) -- one commit for the
/// whole batch instead of one per row. code_index::index() calls these
/// directly (not via a closure-wrapping helper) since its loop body has many
/// early `continue`s that don't map cleanly onto a single wrapped closure.
pub fn begin(name: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "begin", &json!({ "db": name }));
    plugin_ok_err(&resp)
}
pub fn commit(name: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "commit", &json!({ "db": name }));
    plugin_ok_err(&resp)
}
pub fn rollback(name: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "rollback", &json!({ "db": name }));
    plugin_ok_err(&resp)
}

pub struct PreparedStmt {
    db: String,
    handle: String,
}

impl Drop for PreparedStmt {
    fn drop(&mut self) {
        let _ = plugin_call("libsql", "finalize", &json!({ "db": self.db, "handle": self.handle }));
    }
}

/// Prepares `sql` once against `name`'s open connection; reuse via
/// `execute_bound` for every row in a batch to avoid re-parsing/re-planning
/// the same INSERT statement on every call (exec_params does prepare+finalize
/// per invocation, which is fine for one-off writes but wasteful in a loop).
/// The plugin call is cross-process, so the prepared statement is tracked
/// plugin-side by an opaque `handle` id returned from `prepare`, not a raw
/// `*mut sqlite3_stmt` pointer as it was when this ran in-process.
pub fn prepare(name: &str, sql: &str) -> Result<PreparedStmt, String> {
    let resp = plugin_call("libsql", "prepare", &json!({ "db": name, "sql": sql }));
    plugin_ok_err(&resp)?;
    let handle = resp.get("handle").and_then(|v| v.as_str())
        .ok_or_else(|| "prepare: missing handle in plugin response".to_string())?
        .to_string();
    Ok(PreparedStmt { db: name.to_string(), handle })
}

impl PreparedStmt {
    pub fn execute_bound(&self, params: &[&str]) -> Result<(), String> {
        let resp = plugin_call("libsql", "execute_bound", &json!({ "db": self.db, "handle": self.handle, "params": params }));
        plugin_ok_err(&resp)
    }
}

pub fn serialize(name: &str) -> Result<Vec<u8>, String> {
    let resp = plugin_call("libsql", "serialize", &json!({ "db": name }));
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("serialize failed").to_string());
    }
    let b64 = resp.get("data").and_then(|v| v.as_str())
        .ok_or_else(|| "serialize: missing data in plugin response".to_string())?;
    base64_decode(b64)
}

pub fn deserialize(name: &str, bytes: &[u8]) -> Result<(), String> {
    let b64 = base64_encode(bytes);
    let resp = plugin_call("libsql", "deserialize", &json!({ "db": name, "data": b64 }));
    plugin_ok_err(&resp)
}

pub fn smoke() -> Value {
    let mut log: Vec<Value> = Vec::new();
    let n = "smoke";
    log.push(json!({ "step": "open", "result": open(n, ":memory:").err() }));
    log.push(json!({ "step": "create_table", "result": exec(n, "CREATE TABLE memos (id INTEGER PRIMARY KEY, text TEXT, emb F32_BLOB(4))").err() }));
    log.push(json!({ "step": "insert", "result": exec(n, "INSERT INTO memos(text, emb) VALUES ('hello', vector('[0.1,0.2,0.3,0.4]'))").err() }));
    log.push(json!({ "step": "create_index", "result": exec(n, "CREATE INDEX memos_idx ON memos(libsql_vector_idx(emb, 'metric=cosine'))").err() }));
    log.push(json!({ "step": "vector_top_k", "rows": query(n, "SELECT id, text, vector_distance_cos(emb, vector('[0.1,0.2,0.3,0.4]')) AS d FROM vector_top_k('memos_idx', vector('[0.1,0.2,0.3,0.4]'), 5) JOIN memos ON memos.rowid = id").ok() }));
    let _ = close(n);
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
