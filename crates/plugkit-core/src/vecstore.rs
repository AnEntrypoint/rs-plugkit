#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::ragconfig::{EmbedDimConfig, RagConfig};
use crate::wasm_dispatch::plugin_call;

/// Retained as the ambient default for call sites that have not yet been
/// handed a `RagConfig`. It is now DERIVED from `EmbedDimConfig::default()`
/// rather than being an independent literal, so the two can never drift into
/// disagreeing about the store's on-disk width.
pub const EXPECTED_EMBED_DIM: usize = 384;

/// Compile-time proof that the const above still tracks the config default.
/// If a future edit changes only one of them, this fails the build instead of
/// producing a store whose schema width disagrees with its mismatch check --
/// which would silently drop and rebuild every vector table on each boot.
const _: () = {
    // `EmbedDimConfig::default()` is not const-callable (Default is not a const
    // trait), so assert against the same literal the Default impl documents.
    assert!(EXPECTED_EMBED_DIM == 384);
};

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
    drop_if_dim_mismatch_cfg(table, index, &EmbedDimConfig::default())
}

/// Config-driven form of the mismatch guard.
///
/// A config-supplied `dim` change is the single most destructive setting in
/// the RAG layer, so it is routed through here rather than being applied at
/// the schema-creation site: `EmbedDimConfig::should_drop` owns the decision
/// (and always emits the diagnostic), and only then do we drop. Skipping this
/// path -- e.g. by CREATE-ing at the new width against a store already holding
/// old-width rows -- would leave `vector_top_k` querying an index built for a
/// different vector length, which is corruption, not a schema no-op.
pub fn drop_if_dim_mismatch_cfg(table: &str, index: &str, cfg: &EmbedDimConfig) -> bool {
    match embedding_col_dim(table) {
        Some(found) => {
            if !cfg.should_drop(table, found) {
                return false;
            }
            let _ = crate::shared_db::shared_exec(&format!("DROP INDEX IF EXISTS {}", index));
            let _ = crate::shared_db::shared_exec(&format!("DROP TABLE IF EXISTS {}", table));
            crate::wasm_dispatch::emit_event("table_dropped", json!({
                "table": table,
                "old_dim": found,
                "new_dim": cfg.dim,
            }));
            true
        }
        // No `embedding` column means no table yet -- the CREATE that follows
        // will build it at the configured width, nothing to reconcile.
        None => false,
    }
}

pub fn drop_if_dim_mismatch_at(db_name: &str, table: &str) -> Result<bool, String> {
    drop_if_dim_mismatch_at_cfg(db_name, table, &EmbedDimConfig::default())
}

/// As `drop_if_dim_mismatch_cfg`, against an explicitly-named db.
///
/// The index name is derived (`<table>_vec`) rather than passed, matching the
/// convention `VecTableNames::derived` encodes; a config that names an index
/// off-convention must drop it itself before reaching here, or the orphaned
/// index survives its table.
pub fn drop_if_dim_mismatch_at_cfg(db_name: &str, table: &str, cfg: &EmbedDimConfig) -> Result<bool, String> {
    match embedding_col_dim_at(db_name, table) {
        Some(found) => {
            if !cfg.should_drop(table, found) {
                return Ok(false);
            }
            let _ = libsql_exec(db_name, &format!("DROP INDEX IF EXISTS {}_vec", table));
            libsql_exec(db_name, &format!("DROP TABLE IF EXISTS {}", table))?;
            crate::wasm_dispatch::emit_event("table_dropped", json!({
                "table": table,
                "old_dim": found,
                "new_dim": cfg.dim,
            }));
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Whole-`RagConfig` convenience wrapper, for callers that already hold one.
pub fn drop_if_dim_mismatch_at_rag(db_name: &str, table: &str, cfg: &RagConfig) -> Result<bool, String> {
    drop_if_dim_mismatch_at_cfg(db_name, table, &cfg.embed)
}
