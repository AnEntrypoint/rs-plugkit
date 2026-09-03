#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params};

const DEFAULT_DATA_DIR: &str = ".gm/tencentdb-memory";
const DEFAULT_DIM: usize = 768;
const DEFAULT_TABLE: &str = "tencentdb_memory_index";
const DEFAULT_INDEX: &str = "tencentdb_memory_index_vec";

#[derive(Clone, Debug)]
pub struct TencentBackendConfig {
    pub enabled: bool,
    pub data_dir: String,
    pub dim: usize,
    pub table: String,
    pub index: String,
}

impl Default for TencentBackendConfig {
    fn default() -> Self {
        TencentBackendConfig {
            enabled: false,
            data_dir: DEFAULT_DATA_DIR.to_string(),
            dim: DEFAULT_DIM,
            table: DEFAULT_TABLE.to_string(),
            index: DEFAULT_INDEX.to_string(),
        }
    }
}

pub fn resolved_config() -> TencentBackendConfig {
    let tiered = crate::config::resolve().config.value;
    let block = tiered.get("memory").and_then(|m| m.get("tencentdb_backend"));
    let mut cfg = TencentBackendConfig::default();
    if let Some(b) = block {
        if let Some(v) = b.get("enabled").and_then(Value::as_bool) {
            cfg.enabled = v;
        }
        if let Some(v) = b.get("data_dir").and_then(Value::as_str) {
            if !v.is_empty() {
                cfg.data_dir = v.to_string();
            }
        }
        if let Some(v) = b.get("vectors_db_dims").and_then(Value::as_u64) {
            if v > 0 {
                cfg.dim = v as usize;
            }
        }
    }
    cfg
}

pub fn routed_namespaces() -> Vec<String> {
    let tiered = crate::config::resolve().config.value;
    tiered
        .get("memory")
        .and_then(|m| m.get("tencentdb_backend"))
        .and_then(|b| b.get("namespaces"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn namespace_is_routed(namespace: &str) -> bool {
    let cfg = resolved_config();
    cfg.enabled && routed_namespaces().iter().any(|n| n == namespace)
}

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

fn ensure_schema(cfg: &TencentBackendConfig) -> Result<(), String> {
    let path = shared_db_path();
    shared_ensure_open(&path)?;
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, file_path TEXT NOT NULL, embedding F32_BLOB({}), updated_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(namespace, key))",
        cfg.table, cfg.dim
    ))?;
    let _ = shared_exec(&format!(
        "CREATE INDEX IF NOT EXISTS {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
        cfg.index, cfg.table
    ));
    Ok(())
}

fn content_key(namespace: &str, text: &str) -> String {
    let hash = crate::hash::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
    format!("tdai-{:016x}-{}", hash, text.len())
}

fn file_path_for_kind_grouped_memory(data_dir: &str, namespace: &str, kind: &str, key: &str) -> String {
    format!("{}/{}/{}/{}.md", data_dir.trim_end_matches('/'), namespace, kind, key)
}

pub fn write(namespace: &str, kind: &str, text: &str, embedding: &Value, now_ms: i64) -> Result<Value, String> {
    let cfg = resolved_config();
    write_cfg(namespace, kind, text, embedding, now_ms, &cfg)
}

pub fn write_cfg(
    namespace: &str,
    kind: &str,
    text: &str,
    embedding: &Value,
    now_ms: i64,
    cfg: &TencentBackendConfig,
) -> Result<Value, String> {
    let vec = crate::vecns::json_to_f32_vec(embedding)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "tencentdb_memory: empty or non-array embedding; refusing NULL-embedding row".to_string())?;
    if vec.len() != cfg.dim {
        return Err(format!(
            "tencentdb_memory: embedding dim {} does not match configured dim {} (memory.tencentdb_backend.vectors_db_dims); refusing to write a row the index column cannot hold",
            vec.len(),
            cfg.dim
        ));
    }
    let key = content_key(namespace, text);
    let rel_path = file_path_for_kind_grouped_memory(&cfg.data_dir, namespace, kind, &key);

    if !crate::pkfs::write(&rel_path, text) {
        return Err(format!("tencentdb_memory: failed to write content file {}", rel_path));
    }

    ensure_schema(cfg)?;
    let embedding_sql = format!("vector('{}')", crate::vecns::qlit(&vec));
    let delete_sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", cfg.table);
    let insert_sql = format!(
        "INSERT INTO {}(namespace, kind, key, file_path, embedding, updated_at, deleted) VALUES(?1,?2,?3,?4,{},?5,0)",
        cfg.table, embedding_sql
    );
    let path = shared_db_path();
    let spec = crate::vecns::VecTableSpec { db_name: &path, table: &cfg.table, index: &cfg.index };
    let now_s = now_ms.to_string();
    let insert_params = [namespace, kind, key.as_str(), rel_path.as_str(), now_s.as_str()];
    crate::vecns::delete_then_insert_with_recovery(
        &spec,
        |s| s.exec_params(&delete_sql, &[namespace, key.as_str()]),
        &insert_sql,
        &insert_params,
        |e| {
            crate::wasm_dispatch::emit_event(
                "tencentdb_memory_shadow_row_recovery",
                json!({"namespace": namespace, "key": key, "error": e}),
            );
        },
    )?;
    Ok(json!({"key": key, "namespace": namespace, "kind": kind, "file_path": rel_path}))
}

pub fn recall(query_embedding: &Value, namespace: &str, limit: usize) -> Result<Value, String> {
    let cfg = resolved_config();
    recall_cfg(query_embedding, namespace, limit, &cfg)
}

pub fn recall_cfg(query_embedding: &Value, namespace: &str, limit: usize, cfg: &TencentBackendConfig) -> Result<Value, String> {
    let qvec = crate::vecns::json_to_f32_vec(query_embedding)
        .ok_or_else(|| "tencentdb_memory recall: invalid query embedding".to_string())?;
    if qvec.len() != cfg.dim {
        return Err(format!(
            "tencentdb_memory recall: query embedding dim {} does not match configured dim {}",
            qvec.len(),
            cfg.dim
        ));
    }
    ensure_schema(cfg)?;
    let qlit = crate::vecns::qlit(&qvec);
    let pool = (limit * 4).max(20);
    let sql = format!(
        "SELECT key, kind, file_path, updated_at, vector_distance_cos(embedding, vector(?1)) AS distance FROM {} WHERE deleted=0 AND namespace=?2 AND rowid IN (SELECT id FROM vector_top_k('{}', vector(?1), {})) ORDER BY distance ASC",
        cfg.table, cfg.index, pool
    );
    let rows = shared_query_params(&sql, &[&qlit, namespace])?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut hits = Vec::with_capacity(arr.len().min(limit));
    for row in arr.into_iter().take(limit) {
        let file_path = row.get("file_path").and_then(Value::as_str).unwrap_or_default().to_string();
        let text = crate::pkfs::read_to_string(&file_path);
        let distance = row.get("distance").and_then(Value::as_f64).unwrap_or(2.0);
        let mut obj = row.as_object().cloned().unwrap_or_default();
        obj.insert("cos".to_string(), json!(1.0 - distance));
        obj.insert("text".to_string(), json!(text));
        hits.push(Value::Object(obj));
    }
    Ok(json!({"namespace": namespace, "hits": hits, "mode": "tencentdb_backend_vector_top_k"}))
}

pub fn delete_index_first_then_file(namespace: &str, key: &str) -> Result<bool, String> {
    let cfg = resolved_config();
    delete_cfg(namespace, key, &cfg)
}

pub fn delete_cfg(namespace: &str, key: &str, cfg: &TencentBackendConfig) -> Result<bool, String> {
    ensure_schema(cfg)?;
    let select_sql = format!("SELECT file_path FROM {} WHERE namespace=?1 AND key=?2 AND deleted=0", cfg.table);
    let file_path = shared_query_params(&select_sql, &[namespace, key])?
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("file_path"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let existed = file_path.is_some();
    if existed {
        let mark_sql = format!("UPDATE {} SET deleted=1 WHERE namespace=?1 AND key=?2", cfg.table);
        shared_exec_params(&mark_sql, &[namespace, key])?;
    }
    if let Some(p) = file_path {
        let _ = crate::pkfs::exists(&p) && crate::wasm_dispatch::host_remove_file_never_directory(&p);
    }
    Ok(existed)
}
