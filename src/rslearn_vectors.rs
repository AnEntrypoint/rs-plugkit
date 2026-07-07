#![cfg(target_arch = "wasm32")]

use serde_json::Value;

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query, shared_query_params, SHARED_DB};

const TABLE: &str = "rslearn_vectors";
const INDEX: &str = "rslearn_vectors_vec";
const EXPECTED_EMBED_DIM: usize = 384;

fn embedding_col_dim() -> Option<usize> {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", TABLE);
    let rows = crate::libsql_wasm::query(SHARED_DB, &sql).ok()?;
    let arr = rows.as_array()?;
    let row = arr.first()?;
    let ty = row.get("type")?.as_str()?;
    let start = ty.find('(')? + 1;
    let end = ty.find(')')?;
    if end < start { return None; }
    ty[start..end].parse::<usize>().ok()
}

fn drop_if_dim_mismatch() -> bool {
    match embedding_col_dim() {
        Some(dim) if dim == EXPECTED_EMBED_DIM => false,
        Some(old_dim) => {
            let _ = shared_exec(&format!("DROP INDEX IF EXISTS {}", INDEX));
            let _ = shared_exec(&format!("DROP TABLE IF EXISTS {}", TABLE));
            crate::wasm_dispatch::emit_event("table_dropped", serde_json::json!({
                "table": TABLE,
                "old_dim": old_dim,
                "new_dim": EXPECTED_EMBED_DIM,
            }));
            true
        }
        None => false,
    }
}

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

fn has_deleted_column() -> bool {
    let sql = format!("SELECT name FROM pragma_table_info('{}') WHERE name = 'deleted'", TABLE);
    crate::libsql_wasm::query(SHARED_DB, &sql)
        .ok()
        .and_then(|rows| rows.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false)
}

pub fn ensure_schema() -> Result<(), String> {
    shared_ensure_open(&shared_db_path())?;
    let _ = drop_if_dim_mismatch();
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, edge_id TEXT NOT NULL, src TEXT, dst TEXT, relation TEXT, group_id TEXT, embedding F32_BLOB({}), created_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(edge_id))",
        TABLE, EXPECTED_EMBED_DIM
    ))?;
    if !has_deleted_column() {
        shared_exec(&format!(
            "ALTER TABLE {} ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            TABLE
        ))?;
    }
    let _ = shared_exec(&format!(
        "CREATE INDEX IF NOT EXISTS {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
        INDEX, TABLE
    ));
    Ok(())
}

fn vec_to_json_literal(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, f) in v.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&format!("{:.6}", f));
    }
    s.push(']');
    s
}

fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        out.push(x.as_f64()? as f32);
    }
    Some(out)
}

pub fn write(
    edge_id: &str,
    src: &str,
    dst: &str,
    relation: &str,
    group_id: &str,
    embedding: &Value,
    created_at_ms: i64,
) -> Result<(), String> {
    let vec = match json_to_f32_vec(embedding) {
        Some(v) if !v.is_empty() => v,
        _ => return Err("rslearn_vectors: empty or non-array embedding; refusing NULL-embedding row".to_string()),
    };
    if let Err(e) = ensure_schema() {
        return Err(format!("rslearn_vectors ensure_schema failed: {}", e));
    }
    let embedding_sql = format!("vector('{}')", vec_to_json_literal(&vec));
    let sql = format!(
        "INSERT INTO {}(edge_id, src, dst, relation, group_id, embedding, created_at, deleted) VALUES(?1,?2,?3,?4,?5,{},?6,0) \
         ON CONFLICT(edge_id) DO UPDATE SET src=excluded.src, dst=excluded.dst, relation=excluded.relation, \
         group_id=excluded.group_id, embedding=excluded.embedding, created_at=excluded.created_at, deleted=0",
        TABLE, embedding_sql
    );
    let created_s = created_at_ms.to_string();
    shared_exec_params(&sql, &[edge_id, src, dst, relation, group_id, &created_s])
}

pub fn mark_deleted(edge_id: &str) -> Result<(), String> {
    if let Err(e) = ensure_schema() {
        return Err(format!("rslearn_vectors ensure_schema failed: {}", e));
    }
    let sql = format!("UPDATE {} SET deleted=1 WHERE edge_id=?1", TABLE);
    shared_exec_params(&sql, &[edge_id])
}

pub fn row_count() -> Option<i64> {
    ensure_schema().ok()?;
    let sql = format!("SELECT COUNT(*) AS n FROM {}", TABLE);
    let rows = shared_query(&sql).ok()?;
    rows.as_array()?.first()?.get("n")?.as_i64()
}

pub fn search(query_embedding: &Value, limit: usize) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rslearn_vectors search: invalid query embedding".to_string())?;
    ensure_schema()?;
    let qlit = vec_to_json_literal(&qvec);
    let pool = limit.saturating_mul(5).max(20);
    let sql = format!(
        "SELECT r.edge_id, r.src, r.dst, r.relation, r.group_id, r.created_at, \
         vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0 ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, limit
    );
    shared_query_params(&sql, &[&qlit, &qlit])
}
