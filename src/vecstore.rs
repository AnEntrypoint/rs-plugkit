#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::shared_db::{shared_exec, SHARED_DB};

pub const EXPECTED_EMBED_DIM: usize = 384;

pub fn vec_to_json_literal(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, f) in v.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&format!("{:.6}", f));
    }
    s.push(']');
    s
}

pub fn embedding_col_dim(table: &str) -> Option<usize> {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", table);
    let rows = crate::libsql_wasm::query(SHARED_DB, &sql).ok()?;
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
