#![cfg(target_arch = "wasm32")]

// Contract for rs-learn/rs-search: mirror the KvBackend pattern (SqlKv in
// wasm_dispatch.rs), not a wasm host-import. Define a trait shaped like this
// module's fns in your own crate, implement it on a struct here whose methods
// delegate to shared_exec/shared_query/shared_exec_params/shared_query_params,
// then inject that struct at your session constructor the same way
// rs_learn::LearnSession::new(SqlKv) already does. All tables share
// SHARED_DB ("gm") with rs-plugkit's own code_chunks/memories/pipeline_state --
// prefix every owned table (rslearn_*, rssearch_*) via CREATE TABLE IF NOT
// EXISTS to avoid name collisions; shared_ensure_open is idempotent (no-op if
// already open under this name).

use serde_json::Value;

pub const SHARED_DB: &str = "gm";

pub fn shared_ensure_open(path: &str) -> Result<(), String> {
    crate::libsql_wasm::open(SHARED_DB, path)
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
