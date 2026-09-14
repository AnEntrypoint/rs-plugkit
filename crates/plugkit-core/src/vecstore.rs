#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::ragconfig::{EmbedDimConfig, RagConfig};
use crate::wasm_dispatch::plugin_call;

pub const EXPECTED_EMBED_DIM: usize = 384;

const _: () = {
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

pub fn drop_if_dim_mismatch_at_rag(db_name: &str, table: &str, cfg: &RagConfig) -> Result<bool, String> {
    drop_if_dim_mismatch_at_cfg(db_name, table, &cfg.embed)
}
