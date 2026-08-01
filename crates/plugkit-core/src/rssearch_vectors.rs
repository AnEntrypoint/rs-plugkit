#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;

use crate::ragconfig::RagConfig;
use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params, SHARED_DB};
use crate::vecns::{self, QueryBudget, RecencyParams, VecTableSpec};

/// Ambient config for the existing non-`_cfg` entry points.
///
/// Every public function here has a `_cfg` twin taking `&RagConfig`; the
/// original name keeps its signature and forwards through this so the ~10
/// call sites in `verbs.rs`/`memory_md.rs` need not change until a resolution
/// layer actually has something to hand them. Constructed per call rather than
/// cached in a `static`, because the plugin instance is process-wide and
/// shared across concurrently-active projects -- a cached config would let one
/// project's knowledgebase settings answer another project's query.
fn default_cfg() -> RagConfig {
    RagConfig::resolved()
}

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

fn spec<'a>(path: &'a str, cfg: &'a RagConfig) -> VecTableSpec<'a> {
    VecTableSpec::from_names(path, &cfg.rssearch)
}

fn has_deleted_column(path: &str, cfg: &RagConfig) -> bool {
    let sql = format!("SELECT name FROM pragma_table_info('{}') WHERE name = 'deleted'", cfg.rssearch.table);
    let resp = crate::wasm_dispatch::plugin_call("libsql", "query", &json!({ "db": SHARED_DB, "path": path, "sql": sql }));
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    resp.get("rows").and_then(|rows| rows.as_array().map(|a| !a.is_empty())).unwrap_or(false)
}

pub fn ensure_schema() -> Result<(), String> {
    ensure_schema_cfg(&default_cfg())
}

/// `ensure_schema_cfg` is called by EVERY read as well as every write, and it
/// issues four separate libsql round trips (dim-mismatch probe, CREATE TABLE,
/// pragma_table_info, CREATE INDEX over a vector index). The plugin opens the
/// database fresh per call, so on a large store that fixed cost dominates the
/// search it is supposed to be preparing for. The schema cannot change under a
/// running process except through this function, so remembering the
/// (path, dim) pairs already ensured makes the repeat calls free while still
/// re-running in full whenever either changes.
static SCHEMA_ENSURED: Mutex<Option<HashSet<(String, usize)>>> = Mutex::new(None);

/// Anything that destroys the tables out from under the memo must call this,
/// or the next `ensure_schema_cfg` returns Ok against a database that no
/// longer has the schema in it.
pub fn forget_ensured_schema() {
    if let Some(seen) = SCHEMA_ENSURED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        seen.clear();
    }
}

pub fn ensure_schema_cfg(cfg: &RagConfig) -> Result<(), String> {
    let path = shared_db_path();
    let memo_key = (path.clone(), cfg.dim());
    {
        let guard = SCHEMA_ENSURED.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|seen| seen.contains(&memo_key)) {
            return Ok(());
        }
    }
    shared_ensure_open(&path)?;
    // ORDER IS LOAD-BEARING: the mismatch guard runs before the CREATE, so a
    // config-driven `embed.dim` change destroys the old-width table first.
    // Reversing this makes the CREATE a no-op against the surviving table and
    // leaves the store answering queries at the previous vector width.
    let _ = spec(&path, cfg).drop_if_dim_mismatch_cfg(&cfg.embed);
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL, key TEXT NOT NULL, text TEXT, embedding F32_BLOB({}), updated_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(namespace, key))",
        cfg.rssearch.table, cfg.dim()
    ))?;
    if !has_deleted_column(&path, cfg) {
        shared_exec(&format!(
            "ALTER TABLE {} ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            cfg.rssearch.table
        ))?;
    }
    spec(&path, cfg).ensure_index();
    SCHEMA_ENSURED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(memo_key);
    Ok(())
}

fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    vecns::json_to_f32_vec(v)
}

pub fn write(namespace: &str, key: &str, text: &str, embedding: &Value, now_ms: i64) -> Result<(), String> {
    write_cfg(namespace, key, text, embedding, now_ms, &default_cfg())
}

pub fn write_cfg(namespace: &str, key: &str, text: &str, embedding: &Value, now_ms: i64, cfg: &RagConfig) -> Result<(), String> {
    let vec = match json_to_f32_vec(embedding) {
        Some(v) if !v.is_empty() => v,
        _ => return Err("rssearch_vectors: empty or non-array embedding; refusing NULL-embedding row".to_string()),
    };
    // A row whose width disagrees with the column would be rejected by libsql
    // at INSERT with an opaque error; reject it here instead, naming both
    // widths, so a half-migrated embedder is diagnosable rather than just
    // "insert failed".
    if vec.len() != cfg.dim() {
        return Err(format!(
            "rssearch_vectors: embedding dim {} does not match configured dim {}; refusing to write a row the F32_BLOB column cannot hold",
            vec.len(), cfg.dim()
        ));
    }
    if let Err(e) = ensure_schema_cfg(cfg) {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let delete_sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", cfg.rssearch.table);
    let embedding_sql = format!("vector('{}')", vecns::qlit(&vec));
    let sql = format!(
        "INSERT INTO {}(namespace, key, text, embedding, updated_at, deleted) VALUES(?1,?2,?3,{},?4,0)",
        cfg.rssearch.table, embedding_sql
    );
    let now_s = now_ms.to_string();
    let path = shared_db_path();
    vecns::delete_then_insert_with_recovery(
        &spec(&path, cfg),
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
    mark_deleted_cfg(namespace, key, &default_cfg())
}

pub fn mark_deleted_cfg(namespace: &str, key: &str, cfg: &RagConfig) -> Result<(), String> {
    if let Err(e) = ensure_schema_cfg(cfg) {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let sql = format!("UPDATE {} SET deleted=1 WHERE namespace=?1 AND key=?2", cfg.rssearch.table);
    shared_exec_params(&sql, &[namespace, key])
}

pub fn undelete(namespace: &str, key: &str, updated_at_ms: i64) -> Result<(), String> {
    undelete_cfg(namespace, key, updated_at_ms, &default_cfg())
}

pub fn undelete_cfg(namespace: &str, key: &str, updated_at_ms: i64, cfg: &RagConfig) -> Result<(), String> {
    if let Err(e) = ensure_schema_cfg(cfg) {
        return Err(format!("rssearch_vectors ensure_schema failed: {}", e));
    }
    let upd = updated_at_ms.to_string();
    let sql = format!("UPDATE {} SET deleted=0, updated_at=?1 WHERE namespace=?2 AND key=?3", cfg.rssearch.table);
    shared_exec_params(&sql, &[&upd, namespace, key])
}

pub fn row_count(namespace: &str) -> Option<i64> {
    row_count_cfg(namespace, &default_cfg())
}

/// Counts live rows only, and never with an UNFILTERED scan.
///
/// An unfiltered aggregate over a libsql F32_BLOB vector table answers 0 even
/// when the table is full. Measured on this repo's store: `SELECT COUNT(*)
/// FROM rssearch_vectors` returns 0, and so does the subquery form without a
/// WHERE -- but the same aggregate WITH any predicate returns 755, which
/// reconciles exactly against 428 live plus 327 tombstoned rows. It is the
/// missing predicate, not the aggregate, that produces the false empty.
///
/// This matters more than a wrong number: a caller reading 0 would conclude
/// the knowledgebase is empty and could reasonably drop the table or re-index
/// from scratch. The `deleted=0` filter both avoids the hazard and answers the
/// question a caller actually means -- how many rows are live, not how many
/// tombstones are still on disk.
pub fn row_count_cfg(namespace: &str, cfg: &RagConfig) -> Option<i64> {
    ensure_schema_cfg(cfg).ok()?;
    let sql = format!(
        "SELECT count(1) AS n FROM (SELECT key FROM {} WHERE namespace=?1 AND deleted=0)",
        cfg.rssearch.table
    );
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

/// Build the ANN-retrieval SQL shared by both search entry points.
///
/// The `pool` (not `limit`) is used for BOTH `vector_top_k`'s k and the outer
/// LIMIT: recency reweighting and dedup happen after retrieval, so a hit that
/// wins on final score can sit outside the top-`limit` by raw cosine. Cutting
/// to `limit` here would make the reranker structurally unable to change the
/// result set.
fn ann_query_sql(namespaces: &[String], pool: usize, cfg: &RagConfig) -> String {
    // Namespace placeholders start at ?3 because ?1/?2 are both the query
    // vector literal (once for the distance projection, once for the index
    // probe).
    let ns_placeholders: Vec<String> = (0..namespaces.len()).map(|i| format!("?{}", i + 3)).collect();
    let ns_filter = if namespaces.is_empty() {
        String::new()
    } else {
        format!(" AND r.namespace IN ({})", ns_placeholders.join(","))
    };
    format!(
        "SELECT r.namespace, r.key, r.text, r.updated_at, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0{} ORDER BY distance ASC LIMIT {}",
        cfg.rssearch.index, pool, cfg.rssearch.table, ns_filter, pool
    )
}

pub fn search_with_recency(query_embedding: &Value, namespaces: &[String], limit: usize, now_ms: i64) -> Result<Value, String> {
    search_with_recency_cfg(query_embedding, namespaces, limit, now_ms, &default_cfg())
}

pub fn search_with_recency_cfg(query_embedding: &Value, namespaces: &[String], limit: usize, now_ms: i64, cfg: &RagConfig) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search_with_recency: invalid query embedding".to_string())?;
    ensure_schema_cfg(cfg)?;
    let recency_params = RecencyParams::from_scoring(&cfg.scoring);
    let budget = QueryBudget::from_config(&cfg.budget);
    let qlit = vecns::qlit(&qvec);
    let pool = budget.pool(limit);
    let sql = ann_query_sql(namespaces, pool, cfg);
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    let rows = recover_and_retry(|| shared_query_params(&sql, &params))?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut scored: Vec<(f64, Value)> = Vec::with_capacity(arr.len());
    for row in arr {
        let distance = row.get("distance").and_then(|d| d.as_f64()).unwrap_or(2.0);
        let cos = 1.0 - distance;
        if cos < cfg.scoring.cos_floor_applied_before_recency_rescue {
            continue;
        }
        let updated_at = row.get("updated_at").and_then(|u| u.as_i64()).unwrap_or(now_ms);
        let (recency, score) = vecns::recency_score(cos, updated_at, now_ms, &recency_params);
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

pub fn search_memory_hits_cfg(query_embedding: &Value, namespaces: &[String], limit: usize, now_ms: i64, cfg: &RagConfig) -> Result<Value, String> {
    let qvec = json_to_f32_vec(query_embedding)
        .ok_or_else(|| "rssearch_vectors search_memory_hits: invalid query embedding".to_string())?;
    ensure_schema_cfg(cfg)?;
    let recency_params = RecencyParams::from_scoring(&cfg.scoring);
    let budget = QueryBudget::from_config(&cfg.budget);
    let qlit = vecns::qlit(&qvec);
    let pool = budget.pool(limit);
    let sql = ann_query_sql(namespaces, pool, cfg);
    let mut params: Vec<&str> = vec![&qlit, &qlit];
    for n in namespaces { params.push(n.as_str()); }
    let rows = recover_and_retry(|| shared_query_params(&sql, &params))?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut scored: Vec<(f64, Value)> = Vec::with_capacity(arr.len());
    for row in arr {
        let distance = row.get("distance").and_then(|d| d.as_f64()).unwrap_or(2.0);
        let cos = 1.0 - distance;
        if cos < cfg.scoring.cos_floor_applied_before_recency_rescue {
            continue;
        }
        let updated_at = row.get("updated_at").and_then(|u| u.as_i64()).unwrap_or(now_ms);
        let (recency, score) = vecns::recency_score(cos, updated_at, now_ms, &recency_params);
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
            jaccard_overlap(text, kept.get("text").and_then(|t| t.as_str()).unwrap_or("")) >= cfg.scoring.dedup_jaccard_near_duplicate_threshold
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

/// How many distinct row-write failures the migration reports individually
/// before falling back to the aggregate count. Bounded so one systematically
/// broken namespace cannot flood the event stream, and independent of the
/// unrelated skip counters so a malformed row early in the loop cannot
/// suppress the reporting of genuine write failures after it.
const MIGRATE_REPORTED_FAILURES: u32 = 5;

pub fn migrate_namespace_from_flat_json(namespace: &str, now_ms: i64) -> Result<Value, String> {
    migrate_namespace_from_flat_json_cfg(namespace, now_ms, &default_cfg())
}

pub fn migrate_namespace_from_flat_json_cfg(namespace: &str, now_ms: i64, cfg: &RagConfig) -> Result<Value, String> {
    if namespace.is_empty() {
        return Err("migrate_namespace_from_flat_json: namespace required".to_string());
    }
    ensure_schema_cfg(cfg)?;
    let vec_ns = cfg.namespaces.vec_namespace(namespace);
    let vec_entries = host_kv_query_raw(&vec_ns, "");
    let entries = match vec_entries.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Ok(json!({ "migrated": false, "reason": "no-flat-json-entries", "namespace": namespace })),
    };
    let flat_total = entries.iter().filter(|e| e.get("key").and_then(|k| k.as_str()).map(|k| k != "__digest__").unwrap_or(false)).count() as i64;
    let existing = row_count_cfg(namespace, cfg).unwrap_or(0);
    if existing >= flat_total {
        return Ok(json!({ "migrated": false, "reason": "already-populated", "existing_rows": existing }));
    }
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = shared_query_params(
        &format!("SELECT key FROM {} WHERE namespace=?1", cfg.rssearch.table),
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
    // Only the code namespace has a tree-sitter corpus to recover chunk text
    // from; every other namespace's text lives in the flat kv store.
    let is_code_ns = cfg.namespaces.is_code(namespace);
    let mut corpus = if is_code_ns { Some(crate::code_index::FusionCorpus::load()) } else { None };
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut migrated = 0u32;
    let mut skipped = 0u32;
    let mut write_failures = 0u32;
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
        match write_cfg(namespace, key, &text, &embedding, now_ms, cfg) {
            Ok(()) => migrated += 1,
            Err(e) => {
                if write_failures < MIGRATE_REPORTED_FAILURES {
                    crate::wasm_dispatch::emit_event("rssearch_vectors_migrate_row_failed", json!({
                        "namespace": namespace,
                        "key": key,
                        "error": e,
                    }));
                }
                write_failures += 1;
                skipped += 1;
            }
        }
    }
    crate::wasm_dispatch::emit_event("rssearch_vectors_migrated", json!({
        "namespace": namespace,
        "migrated_count": migrated,
        "skipped_count": skipped,
        "write_failure_count": write_failures,
        "deferred_count": deferred,
    }));
    Ok(json!({ "migrated": true, "namespace": namespace, "migrated_count": migrated, "skipped_count": skipped, "deferred_count": deferred }))
}
