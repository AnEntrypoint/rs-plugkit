#![cfg(target_arch = "wasm32")]

use serde_json::Value;

use crate::shared_db::{shared_exec, shared_exec_params, shared_query_params, SHARED_DB};

const TABLE: &str = "rssearch_vectors";
const INDEX: &str = "rssearch_vectors_vec";
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

pub fn ensure_schema() -> Result<(), String> {
    let _ = drop_if_dim_mismatch();
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL, key TEXT NOT NULL, text TEXT, embedding F32_BLOB({}), updated_at INTEGER, UNIQUE(namespace, key))",
        TABLE, EXPECTED_EMBED_DIM
    ))?;
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

pub fn write(namespace: &str, key: &str, text: &str, embedding: &Value, now_ms: i64) -> Result<(), String> {
    let vec = match json_to_f32_vec(embedding) {
        Some(v) if !v.is_empty() => v,
        _ => return Err("rssearch_vectors: empty or non-array embedding; refusing NULL-embedding row".to_string()),
    };
    if let Err(e) = ensure_schema() {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let embedding_sql = format!("vector('{}')", vec_to_json_literal(&vec));
    let sql = format!(
        "INSERT INTO {}(namespace, key, text, embedding, updated_at) VALUES(?1,?2,?3,{},?4) ON CONFLICT(namespace, key) DO UPDATE SET text=excluded.text, embedding=excluded.embedding, updated_at=excluded.updated_at",
        TABLE, embedding_sql
    );
    let now_s = now_ms.to_string();
    shared_exec_params(&sql, &[namespace, key, text, &now_s])
}

pub fn delete(namespace: &str, key: &str) -> Result<(), String> {
    if let Err(e) = ensure_schema() {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", TABLE);
    shared_exec_params(&sql, &[namespace, key])
}

pub fn row_count(namespace: &str) -> Option<i64> {
    ensure_schema().ok()?;
    let sql = format!("SELECT COUNT(*) AS n FROM {} WHERE namespace=?1", TABLE);
    let rows = shared_query_params(&sql, &[namespace]).ok()?;
    rows.as_array()?.first()?.get("n")?.as_i64()
}

pub fn search(query_embedding: &Value, namespaces: &[String], limit: usize) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search: invalid query embedding".to_string())?;
    ensure_schema()?;
    let qlit = vec_to_json_literal(&qvec);
    let pool = limit.saturating_mul(5).max(20);
    let ns_placeholders: Vec<String> = (0..namespaces.len()).map(|i| format!("?{}", i + 3)).collect();
    let ns_filter = if namespaces.is_empty() {
        String::new()
    } else {
        format!(" AND r.namespace IN ({})", ns_placeholders.join(","))
    };
    let sql = format!(
        "SELECT r.namespace, r.key, r.text, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE 1=1{} ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, ns_filter, limit
    );
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    shared_query_params(&sql, &params)
}
