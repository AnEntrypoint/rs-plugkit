#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params, SHARED_DB};
use crate::vecns::{self, QueryBudget, RecencyParams, VecTableSpec};
use crate::vecstore::EXPECTED_EMBED_DIM;

const TABLE: &str = "rssearch_vectors";
const INDEX: &str = "rssearch_vectors_vec";
const HALF_LIFE_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;
const RECENCY_FLOOR: f64 = 0.4;

const RECENCY: RecencyParams = RecencyParams { half_life_ms: HALF_LIFE_MS, recency_floor: RECENCY_FLOOR };
const BUDGET: QueryBudget = QueryBudget { pool_multiplier: 5, pool_floor: 20 };

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

/// Resolved fresh every call (never a `const`) -- the plugin is stateless
/// and process-wide shared, so `db_name` must be THIS dispatch's real
/// absolute path (host_cwd_string() underneath), not a value baked in at
/// compile time.
fn spec(path: &str) -> VecTableSpec<'_> {
    VecTableSpec { db_name: path, table: TABLE, index: INDEX }
}

fn has_deleted_column(path: &str) -> bool {
    let sql = format!("SELECT name FROM pragma_table_info('{}') WHERE name = 'deleted'", TABLE);
    let resp = crate::wasm_dispatch::plugin_call("libsql", "query", &json!({ "db": SHARED_DB, "path": path, "sql": sql }));
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    resp.get("rows").and_then(|rows| rows.as_array().map(|a| !a.is_empty())).unwrap_or(false)
}

pub fn ensure_schema() -> Result<(), String> {
    let path = shared_db_path();
    shared_ensure_open(&path)?;
    let _ = spec(&path).drop_if_dim_mismatch();
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL, key TEXT NOT NULL, text TEXT, embedding F32_BLOB({}), updated_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(namespace, key))",
        TABLE, EXPECTED_EMBED_DIM
    ))?;
    if !has_deleted_column(&path) {
        shared_exec(&format!(
            "ALTER TABLE {} ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            TABLE
        ))?;
    }
    spec(&path).ensure_index();
    Ok(())
}

fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    vecns::json_to_f32_vec(v)
}

pub fn write(namespace: &str, key: &str, text: &str, embedding: &Value, now_ms: i64) -> Result<(), String> {
    let vec = match json_to_f32_vec(embedding) {
        Some(v) if !v.is_empty() => v,
        _ => return Err("rssearch_vectors: empty or non-array embedding; refusing NULL-embedding row".to_string()),
    };
    if let Err(e) = ensure_schema() {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let delete_sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", TABLE);
    let embedding_sql = format!("vector('{}')", vecns::qlit(&vec));
    let sql = format!(
        "INSERT INTO {}(namespace, key, text, embedding, updated_at, deleted) VALUES(?1,?2,?3,{},?4,0)",
        TABLE, embedding_sql
    );
    let now_s = now_ms.to_string();
    let path = shared_db_path();
    vecns::delete_then_insert_with_recovery(
        &spec(&path),
        |s| s.exec_params(&delete_sql, &[namespace, key]),
        &sql, &[namespace, key, text, &now_s],
        |e| {
            crate::wasm_dispatch::emit_event("rssearch_vectors_shadow_row_recovery", json!({
                "namespace": namespace, "key": key, "error": e,
            }));
        },
    )
}

pub fn mark_deleted(namespace: &str, key: &str) -> Result<(), String> {
    if let Err(e) = ensure_schema() {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let sql = format!("UPDATE {} SET deleted=1 WHERE namespace=?1 AND key=?2", TABLE);
    shared_exec_params(&sql, &[namespace, key])
}

pub fn undelete(namespace: &str, key: &str, updated_at_ms: i64) -> Result<(), String> {
    if let Err(e) = ensure_schema() {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let upd = updated_at_ms.to_string();
    let sql = format!("UPDATE {} SET deleted=0, updated_at=?1 WHERE namespace=?2 AND key=?3", TABLE);
    shared_exec_params(&sql, &[&upd, namespace, key])
}

pub fn row_count(namespace: &str) -> Option<i64> {
    ensure_schema().ok()?;
    let sql = format!("SELECT COUNT(*) AS n FROM {} WHERE namespace=?1", TABLE);
    let rows = shared_query_params(&sql, &[namespace]).ok()?;
    rows.as_array()?.first()?.get("n")?.as_i64()
}

fn recover_and_retry<F>(op: F) -> Result<Value, String>
where
    F: Fn() -> Result<Value, String>,
{
    match op() {
        Err(e) if crate::shared_db::is_malformed(&e) => {
            if crate::shared_db::recover_malformed_shared_db() {
                op()
            } else {
                Err(e)
            }
        }
        other => other,
    }
}

pub fn search(query_embedding: &Value, namespaces: &[String], limit: usize) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search: invalid query embedding".to_string())?;
    ensure_schema()?;
    let qlit = vecns::qlit(&qvec);
    let pool = BUDGET.pool(limit);
    let ns_placeholders: Vec<String> = (0..namespaces.len()).map(|i| format!("?{}", i + 3)).collect();
    let ns_filter = if namespaces.is_empty() {
        String::new()
    } else {
        format!(" AND r.namespace IN ({})", ns_placeholders.join(","))
    };
    let sql = format!(
        "SELECT r.namespace, r.key, r.text, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0{} ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, ns_filter, limit
    );
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    recover_and_retry(|| shared_query_params(&sql, &params))
}

pub fn search_with_recency(query_embedding: &Value, namespaces: &[String], limit: usize, now_ms: i64) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search_with_recency: invalid query embedding".to_string())?;
    ensure_schema()?;
    let qlit = vecns::qlit(&qvec);
    let pool = BUDGET.pool(limit);
    let ns_placeholders: Vec<String> = (0..namespaces.len()).map(|i| format!("?{}", i + 3)).collect();
    let ns_filter = if namespaces.is_empty() {
        String::new()
    } else {
        format!(" AND r.namespace IN ({})", ns_placeholders.join(","))
    };
    let sql = format!(
        "SELECT r.namespace, r.key, r.text, r.updated_at, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0{} ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, ns_filter, pool
    );
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    let rows = recover_and_retry(|| shared_query_params(&sql, &params))?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut scored: Vec<(f64, Value)> = Vec::with_capacity(arr.len());
    for row in arr {
        let distance = row.get("distance").and_then(|d| d.as_f64()).unwrap_or(2.0);
        let cos = 1.0 - distance;
        let updated_at = row.get("updated_at").and_then(|u| u.as_i64()).unwrap_or(now_ms);
        let (recency, score) = vecns::recency_score(cos, updated_at, now_ms, &RECENCY);
        let mut obj = row.as_object().cloned().unwrap_or_default();
        obj.insert("cos".to_string(), json!(cos));
        obj.insert("recency".to_string(), json!(recency));
        obj.insert("score".to_string(), json!(score));
        scored.push((score, Value::Object(obj)));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let out: Vec<Value> = scored.into_iter().take(limit).map(|(_, v)| v).collect();
    Ok(Value::Array(out))
}

fn jaccard_overlap(a: &str, b: &str) -> f64 {
    let tokenize = |s: &str| -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_string())
            .collect()
    };
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    inter / (ta.len() as f64 + tb.len() as f64 - inter)
}

pub fn search_memory_hits(query_embedding: &Value, namespaces: &[String], limit: usize, now_ms: i64, cos_floor: f64) -> Result<Value, String> {
    const DEDUP_JACCARD: f64 = 0.7;
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search_memory_hits: invalid query embedding".to_string())?;
    ensure_schema()?;
    let qlit = vecns::qlit(&qvec);
    let pool = BUDGET.pool(limit);
    let ns_placeholders: Vec<String> = (0..namespaces.len()).map(|i| format!("?{}", i + 3)).collect();
    let ns_filter = if namespaces.is_empty() {
        String::new()
    } else {
        format!(" AND r.namespace IN ({})", ns_placeholders.join(","))
    };
    let sql = format!(
        "SELECT r.namespace, r.key, r.text, r.updated_at, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0{} ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, ns_filter, pool
    );
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    let rows = recover_and_retry(|| shared_query_params(&sql, &params))?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut scored: Vec<(f64, Value)> = Vec::with_capacity(arr.len());
    for row in arr {
        let distance = row.get("distance").and_then(|d| d.as_f64()).unwrap_or(2.0);
        let cos = 1.0 - distance;
        if cos < cos_floor {
            continue;
        }
        let updated_at = row.get("updated_at").and_then(|u| u.as_i64()).unwrap_or(now_ms);
        let (recency, score) = vecns::recency_score(cos, updated_at, now_ms, &RECENCY);
        let hit = json!({
            "key": row.get("key").cloned().unwrap_or(Value::Null),
            "namespace": row.get("namespace").cloned().unwrap_or(Value::Null),
            "text": row.get("text").cloned().unwrap_or(Value::Null),
            "cos": cos,
            "recency": recency,
            "score": score,
        });
        scored.push((score, hit));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<Value> = Vec::new();
    for (_, hit) in scored {
        let text = hit.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let dup = out.iter().any(|kept| {
            jaccard_overlap(text, kept.get("text").and_then(|t| t.as_str()).unwrap_or("")) >= DEDUP_JACCARD
        });
        if !dup {
            out.push(hit);
        }
        if out.len() >= limit {
            break;
        }
    }
    Ok(Value::Array(out))
}

fn extract_embedding_value(v: &Value) -> Option<Value> {
    if v.is_array() { return Some(v.clone()); }
    if let Some(arr) = v.get("embedding") {
        if arr.is_array() { return Some(arr.clone()); }
    }
    if let Some(emb) = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()).and_then(|e| e.get("embedding")) {
        if emb.is_array() { return Some(emb.clone()); }
    }
    None
}

fn host_kv_query_raw(namespace: &str, query: &str) -> Value {
    let packed = unsafe {
        crate::wasm_dispatch::host_kv_query(
            namespace.as_ptr(), namespace.len() as u32,
            query.as_ptr(), query.len() as u32,
        )
    };
    crate::wasm_dispatch::unpack_to_value_pub(packed)
}

const MIGRATE_BUDGET_MS: u64 = 2000;

pub fn migrate_namespace_from_flat_json(namespace: &str, now_ms: i64) -> Result<Value, String> {
    if namespace.is_empty() {
        return Err("migrate_namespace_from_flat_json: namespace required".to_string());
    }
    ensure_schema()?;
    let vec_ns = format!("{}-vec", namespace);
    let vec_entries = host_kv_query_raw(&vec_ns, "");
    let entries = match vec_entries.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Ok(json!({ "migrated": false, "reason": "no-flat-json-entries", "namespace": namespace })),
    };
    let flat_total = entries.iter().filter(|e| e.get("key").and_then(|k| k.as_str()).map(|k| k != "__digest__").unwrap_or(false)).count() as i64;
    let existing = row_count(namespace).unwrap_or(0);
    if existing >= flat_total {
        return Ok(json!({ "migrated": false, "reason": "already-populated", "existing_rows": existing }));
    }
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = shared_query_params(
        &format!("SELECT key FROM {} WHERE namespace=?1", TABLE),
        &[namespace],
    ) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|v| v.as_str()) {
                    present.insert(k.to_string());
                }
            }
        }
    }
    let text_entries = host_kv_query_raw(namespace, "");
    let mut text_by_key: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(arr) = text_entries.as_array() {
        for e in arr {
            if let (Some(k), Some(v)) = (e.get("key").and_then(|x| x.as_str()), e.get("value").and_then(|x| x.as_str())) {
                text_by_key.insert(k.to_string(), v.to_string());
            }
        }
    }
    let is_codeinsight = namespace == "codeinsight";
    let mut corpus = if is_codeinsight { Some(crate::code_index::FusionCorpus::load()) } else { None };
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut migrated = 0u32;
    let mut skipped = 0u32;
    let mut deferred = 0u32;
    for entry in &entries {
        let key = match entry.get("key").and_then(|k| k.as_str()) { Some(k) => k, None => { skipped += 1; continue; } };
        if key == "__digest__" { continue; }
        if present.contains(key) { continue; }
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > MIGRATE_BUDGET_MS {
            deferred += 1;
            continue;
        }
        let raw_value = match entry.get("value").and_then(|v| v.as_str()) { Some(v) => v, None => { skipped += 1; continue; } };
        let parsed: Value = match serde_json::from_str(raw_value) { Ok(v) => v, Err(_) => { skipped += 1; continue; } };
        let embedding = match extract_embedding_value(&parsed) { Some(e) => e, None => { skipped += 1; continue; } };
        let text = text_by_key.get(key).cloned()
            .or_else(|| corpus.as_mut().and_then(|c| c.text_for_key(key)))
            .unwrap_or_default();
        match write(namespace, key, &text, &embedding, now_ms) {
            Ok(()) => migrated += 1,
            Err(e) => {
                if skipped == 0 {
                    crate::wasm_dispatch::emit_event("rssearch_vectors_migrate_row_failed", json!({
                        "namespace": namespace,
                        "key": key,
                        "error": e,
                    }));
                }
                skipped += 1;
            }
        }
    }
    crate::wasm_dispatch::emit_event("rssearch_vectors_migrated", json!({
        "namespace": namespace,
        "migrated_count": migrated,
        "skipped_count": skipped,
        "deferred_count": deferred,
    }));
    Ok(json!({ "migrated": true, "namespace": namespace, "migrated_count": migrated, "skipped_count": skipped, "deferred_count": deferred }))
}
