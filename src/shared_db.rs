#![cfg(target_arch = "wasm32")]

use serde_json::Value;

pub const SHARED_DB: &str = "gm";

pub fn shared_ensure_open(path: &str) -> Result<(), String> {
    crate::libsql_wasm::open(SHARED_DB, path)
}

pub fn recreate_shared_db(path: &str) -> Result<(), String> {
    let _ = crate::libsql_wasm::close(SHARED_DB);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{}", path, suffix));
    }
    shared_ensure_open(path)
}

pub fn is_malformed(err: &str) -> bool {
    err.contains("malformed")
}

pub fn recover_malformed_shared_db() -> bool {
    let path = crate::code_index::project_db_path(None);
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
    crate::libsql_wasm::exec(SHARED_DB, sql)
}

pub fn shared_query(sql: &str) -> Result<Value, String> {
    crate::libsql_wasm::query(SHARED_DB, sql)
}

pub fn shared_exec_params(sql: &str, params: &[&str]) -> Result<(), String> {
    crate::libsql_wasm::exec_params(SHARED_DB, sql, params)
}

pub fn shared_query_params(sql: &str, params: &[&str]) -> Result<Value, String> {
    crate::libsql_wasm::query_params(SHARED_DB, sql, params)
}
