#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::shared_db::{shared_exec, SHARED_DB};

pub const EXPECTED_EMBED_DIM: usize = 384;

pub fn vec_to_json_literal(v: &[f32]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

pub fn embedding_col_dim(table: &str) -> Option<usize> {
    embedding_col_dim_at(SHARED_DB, table)
}

pub fn embedding_col_dim_at(db_name: &str, table: &str) -> Option<usize> {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", table);
    let rows = crate::libsql_wasm::query(db_name, &sql).ok()?;
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
            let _ = shared_exec(&format!("DROP INDEX IF EXISTS {}", index));
            let _ = shared_exec(&format!("DROP TABLE IF EXISTS {}", table));
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
            let _ = crate::libsql_wasm::exec(db_name, &format!("DROP INDEX IF EXISTS {}_vec", table));
            crate::libsql_wasm::exec(db_name, &format!("DROP TABLE IF EXISTS {}", table))?;
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
