#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::orchestrator::yaml_util::{base64_decode, base64_encode};
use crate::wasm_dispatch::{host_cwd_string, plugin_call};

/// What a libsql failure actually was, recovered from the plugin's message.
///
/// The plugin flattens a real `sqlite3_extended_errcode` into its error string
/// ("exec rc=11 ext=11 msg=..."), so the codes are present but unusable by a
/// consumer without re-parsing. Two consumers make DESTRUCTIVE decisions on
/// that string -- shared_db deletes the database on "malformed", vecns drops
/// an index on "shadow row" -- and a bare `contains` there matches any message
/// that merely quotes the phrase, including one describing a different db.
///
/// Classifying on the numeric code first, and only falling back to text when
/// no code is present, keeps those decisions on ground the plugin actually
/// asserted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibsqlErrorKind {
    Corrupt,
    Busy,
    NotFound,
    ShadowRow,
    Other,
}

const SQLITE_BUSY: i64 = 5;
const SQLITE_LOCKED: i64 = 6;
const SQLITE_CORRUPT: i64 = 11;
const SQLITE_NOTADB: i64 = 26;

fn parse_code(err: &str, field: &str) -> Option<i64> {
    let at = err.find(field)? + field.len();
    let rest = &err[at..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub fn classify_error(err: &str) -> LibsqlErrorKind {
    let code = parse_code(err, "ext=").or_else(|| parse_code(err, "rc="));
    if let Some(c) = code {
        let primary = c & 0xff;
        match primary {
            SQLITE_CORRUPT | SQLITE_NOTADB => return LibsqlErrorKind::Corrupt,
            SQLITE_BUSY | SQLITE_LOCKED => return LibsqlErrorKind::Busy,
            _ => {}
        }
        // The plugin stated a code and it was not corruption. Text matching
        // past this point would let a message that merely QUOTES "database
        // disk image is malformed" -- a log line, an error about a different
        // file -- trigger deletion of this database. The code is the
        // authority; only ShadowRow is text-only, since no SQLite code
        // expresses it.
        return if err.to_ascii_lowercase().contains("shadow row") {
            LibsqlErrorKind::ShadowRow
        } else {
            LibsqlErrorKind::Other
        };
    }
    let e = err.to_ascii_lowercase();
    if e.contains("shadow row") {
        LibsqlErrorKind::ShadowRow
    } else if e.contains("database disk image is malformed")
        || e.contains("sqlite_corrupt")
        || e.contains("file is not a database")
    {
        LibsqlErrorKind::Corrupt
    } else if e.contains("no such table") || e.contains("not found") {
        LibsqlErrorKind::NotFound
    } else {
        LibsqlErrorKind::Other
    }
}

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

pub fn absolute_db_path(filename: &str) -> String {
    if filename.is_empty() || filename == ":memory:" || filename.starts_with('/') || (filename.len() > 1 && filename.as_bytes()[1] == b':') {
        return filename.to_string();
    }
    match host_cwd_string() {
        Some(cwd) if !cwd.is_empty() => {
            let cwd = cwd.trim_end_matches(['/', '\\']);
            format!("{}/{}/{}", cwd, crate::ragconfig::RagConfig::resolved().db_path.state_root_dir, filename)
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

