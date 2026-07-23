#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::wasm_dispatch::plugin_call;

pub const EXPECTED_EMBED_DIM: usize = 384;

fn libsql_query(db_name: &str, sql: &str) -> Result<Value, String> {
    let resp = plugin_call("libsql", "query", &json!({ "path": db_name, "sql": sql }));
    if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(resp.get("rows").cloned().unwrap_or(Value::Array(vec![])))
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("libsql query failed").to_string())
    }
}

fn libsql_exec(db_name: &str, sql: &str) -> Result<(), String> {
    let resp = plugin_call("libsql", "exec", &json!({ "path": db_name, "sql": sql }));
    if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("libsql exec failed").to_string())
    }
}

pub fn vec_to_json_literal(v: &[f32]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

pub fn embedding_col_dim(table: &str) -> Option<usize> {
    embedding_col_dim_at(crate::shared_db::SHARED_DB, table)
}

pub fn embedding_col_dim_at(db_name: &str, table: &str) -> Option<usize> {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", table);
    let rows = libsql_query(db_name, &sql).ok()?;
    let arr = rows.as_array()?;
    let row = arr.first()?;
    let ty = row.get("type")?.as_str()?;
    let start = ty.find('(')? + 1;
    let end = ty.find(')')?;
    if end < start { return None; }
    ty[start..end].parse::<usize>().ok()
}

pub fn drop_if_dim_mismatch(table: &str, index: &str) -> bool {
    match embedding_col_dim(table) {
        Some(dim) if dim == EXPECTED_EMBED_DIM => false,
        Some(old_dim) => {
            let _ = crate::shared_db::shared_exec(&format!("DROP INDEX IF EXISTS {}", index));
            let _ = crate::shared_db::shared_exec(&format!("DROP TABLE IF EXISTS {}", table));
            crate::wasm_dispatch::emit_event("table_dropped", json!({
                "table": table,
                "old_dim": old_dim,
                "new_dim": EXPECTED_EMBED_DIM,
            }));
            true
        }
        None => false,
    }
}

pub fn drop_if_dim_mismatch_at(db_name: &str, table: &str) -> Result<bool, String> {
    match embedding_col_dim_at(db_name, table) {
        Some(dim) if dim == EXPECTED_EMBED_DIM => Ok(false),
        Some(old_dim) => {
            let _ = libsql_exec(db_name, &format!("DROP INDEX IF EXISTS {}_vec", table));
            libsql_exec(db_name, &format!("DROP TABLE IF EXISTS {}", table))?;
            crate::wasm_dispatch::emit_event("table_dropped", json!({
                "table": table,
                "old_dim": old_dim,
                "new_dim": EXPECTED_EMBED_DIM,
            }));
            Ok(true)
        }
        None => Ok(false),
    }
}
