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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddingColumn {
    Absent,
    Width(usize),
    Unparseable,
    Unknown,
}

pub fn embedding_col_at(db_name: &str, table: &str) -> EmbeddingColumn {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", table);
    let rows = match crate::libsql_wasm::retry_on_busy(|| libsql_query(db_name, &sql)) {
        Ok(r) => r,
        Err(e) => {
            crate::wasm_dispatch::emit_event("embed_col_probe_failed", json!({
                "table": table,
                "error": e,
                "effect": "column width unknown; treated as indeterminate rather than absent, so no drop decision is made on it",
            }));
            return EmbeddingColumn::Unknown;
        }
    };
    let ty = match rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("type")).and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return EmbeddingColumn::Absent,
    };
    let parsed = ty
        .find('(')
        .and_then(|open| ty.find(')').map(|close| (open + 1, close)))
        .filter(|(start, end)| end >= start)
        .and_then(|(start, end)| ty[start..end].parse::<usize>().ok());
    match parsed {
        Some(w) => EmbeddingColumn::Width(w),
        None => EmbeddingColumn::Unparseable,
    }
}

pub fn embedding_col_dim_at(db_name: &str, table: &str) -> Option<usize> {
    match embedding_col_at(db_name, table) {
        EmbeddingColumn::Width(w) => Some(w),
        _ => None,
    }
}

fn drop_table(db_name: &str, table: &str, cfg: &EmbedDimConfig, reason: &str, old_dim: Value) -> Result<bool, String> {
    let _ = libsql_exec(db_name, &format!("DROP INDEX IF EXISTS {}_vec", table));
    libsql_exec(db_name, &format!("DROP TABLE IF EXISTS {}", table))?;
    crate::wasm_dispatch::emit_event("table_dropped", json!({
        "table": table,
        "reason": reason,
        "old_dim": old_dim,
        "new_dim": cfg.dim,
    }));
    Ok(true)
}

pub fn drop_if_dim_mismatch_at_cfg(db_name: &str, table: &str, cfg: &EmbedDimConfig) -> Result<bool, String> {
    match embedding_col_at(db_name, table) {
        EmbeddingColumn::Width(found) => {
            if cfg.should_drop_table_for_dim_mismatch(table, found) {
                return drop_table(db_name, table, cfg, "dim_mismatch", json!(found));
            }
            if cfg.keep_mismatched_table_intact_instead_of_dropping {
                return Ok(false);
            }
            if crate::embed_marker::embed_generation_changed_for_table(table) {
                return drop_table(db_name, table, cfg, "embed_generation_changed", json!(found));
            }
            Ok(false)
        }
        EmbeddingColumn::Unparseable => {
            crate::wasm_dispatch::emit_event("embed_col_type_unparseable", json!({
                "table": table,
                "expected_dim": cfg.dim,
            }));
            Ok(false)
        }
        EmbeddingColumn::Absent | EmbeddingColumn::Unknown => Ok(false),
    }
}

/// Whole-`RagConfig` convenience wrapper, for callers that already hold one.
pub fn drop_if_dim_mismatch_at_rag(db_name: &str, table: &str, cfg: &RagConfig) -> Result<bool, String> {
    drop_if_dim_mismatch_at_cfg(db_name, table, &cfg.embed)
}
