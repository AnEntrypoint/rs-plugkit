#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params};

const TABLE: &str = "cache_entries";

#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    pub max_entries_per_namespace: usize,
    pub max_bytes_per_namespace: usize,
    pub max_value_bytes: usize,
    pub default_ttl_ms: Option<i64>,
}

pub const DEFAULTS: CacheConfig = CacheConfig {
    max_entries_per_namespace: 512,
    max_bytes_per_namespace: 8 * 1024 * 1024,
    max_value_bytes: 1024 * 1024,
    default_ttl_ms: None,
};

#[derive(Debug)]
pub enum CacheError {
    InvalidKey(String),
    ValueTooLarge { bytes: usize, limit: usize },
    Store(String),
}

impl CacheError {
    pub fn message(&self) -> String {
        match self {
            CacheError::InvalidKey(what) => format!("cache: {}", what),
            CacheError::ValueTooLarge { bytes, limit } => format!(
                "cache: value of {} bytes exceeds max_value_bytes={}; rejected rather than truncated",
                bytes, limit
            ),
            CacheError::Store(e) => format!("cache store failure: {}", e),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            CacheError::InvalidKey(_) => "invalid_key",
            CacheError::ValueTooLarge { .. } => "value_too_large",
            CacheError::Store(_) => "store_failure",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub namespace: String,
    pub key: String,
    pub value: String,
    pub content_hash: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        json!({
            "namespace": self.namespace,
            "key": self.key,
            "value": self.value,
            "content_hash": self.content_hash,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
        })
    }
}

pub fn content_hash(value: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in value.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

fn now_ms() -> i64 {
    unsafe { crate::wasm_dispatch::host_now_ms() as i64 }
}

fn store_err(e: String) -> CacheError {
    CacheError::Store(e)
}

fn row_i64(row: &Value, col: &str) -> Option<i64> {
    row.get(col).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

pub fn ensure_schema(_cfg: &CacheConfig) -> Result<(), CacheError> {
    let path = crate::code_index::project_db_path(None);
    shared_ensure_open(&path).map_err(store_err)?;
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (\
           namespace TEXT NOT NULL, \
           key TEXT NOT NULL, \
           value TEXT NOT NULL, \
           content_hash TEXT NOT NULL, \
           created_at INTEGER NOT NULL, \
           expires_at INTEGER, \
           last_used_at INTEGER NOT NULL, \
           bytes INTEGER NOT NULL, \
           PRIMARY KEY(namespace, key))",
        TABLE
    ))
    .map_err(store_err)?;
    let _ = shared_exec(&format!(
        "CREATE INDEX IF NOT EXISTS {t}_ns_lru ON {t}(namespace, last_used_at)",
        t = TABLE
    ));
    Ok(())
}

fn validate_identity(namespace: &str, key: &str) -> Result<(), CacheError> {
    if namespace.is_empty() {
        return Err(CacheError::InvalidKey("namespace required".to_string()));
    }
    if key.is_empty() {
        return Err(CacheError::InvalidKey("key required".to_string()));
    }
    Ok(())
}

pub fn get(cfg: &CacheConfig, namespace: &str, key: &str) -> Result<Option<Entry>, CacheError> {
    validate_identity(namespace, key)?;
    ensure_schema(cfg)?;
    let now = now_ms();
    let now_s = now.to_string();
    let sql = format!(
        "SELECT value, content_hash, created_at, expires_at FROM {} \
         WHERE namespace=?1 AND key=?2 AND (expires_at IS NULL OR expires_at > ?3)",
        TABLE
    );
    let rows = shared_query_params(&sql, &[namespace, key, &now_s]).map_err(store_err)?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r,
        None => return Ok(None),
    };
    let value = row
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CacheError::Store("row present but value column missing".to_string()))?
        .to_string();
    let entry = Entry {
        namespace: namespace.to_string(),
        key: key.to_string(),
        content_hash: row
            .get("content_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        created_at: row_i64(row, "created_at").unwrap_or(now),
        expires_at: row_i64(row, "expires_at"),
        value,
    };
    let touch = format!("UPDATE {} SET last_used_at=?1 WHERE namespace=?2 AND key=?3", TABLE);
    let _ = shared_exec_params(&touch, &[&now_s, namespace, key]);
    Ok(Some(entry))
}

pub fn put(
    cfg: &CacheConfig,
    namespace: &str,
    key: &str,
    value: &str,
    ttl_ms: Option<i64>,
) -> Result<String, CacheError> {
    validate_identity(namespace, key)?;
    let bytes = value.len();
    if bytes > cfg.max_value_bytes {
        return Err(CacheError::ValueTooLarge { bytes, limit: cfg.max_value_bytes });
    }
    ensure_schema(cfg)?;

    let now = now_ms();
    let hash = content_hash(value);
    let effective_ttl = ttl_ms.or(cfg.default_ttl_ms);
    if let Some(t) = effective_ttl {
        if t <= 0 {
            return Err(CacheError::InvalidKey(format!(
                "ttl_ms must be positive; got {} which would store an already-expired entry",
                t
            )));
        }
    }
    let expires_at = effective_ttl.map(|t| now.saturating_add(t));

    let now_s = now.to_string();
    let bytes_s = bytes.to_string();
    let expires_s = expires_at.map(|e| e.to_string());
    let expires_param: &str = expires_s.as_deref().unwrap_or("");
    let sql = format!(
        "INSERT INTO {t}(namespace, key, value, content_hash, created_at, expires_at, last_used_at, bytes) \
         VALUES(?1,?2,?3,?4,?5,NULLIF(?6,''),?5,?7) \
         ON CONFLICT(namespace, key) DO UPDATE SET \
           value=excluded.value, content_hash=excluded.content_hash, \
           created_at=excluded.created_at, expires_at=excluded.expires_at, \
           last_used_at=excluded.last_used_at, bytes=excluded.bytes",
        t = TABLE
    );
    shared_exec_params(
        &sql,
        &[namespace, key, value, &hash, &now_s, expires_param, &bytes_s],
    )
    .map_err(store_err)?;

    let _ = enforce_budget(cfg, namespace);
    Ok(hash)
}

pub fn invalidate(cfg: &CacheConfig, namespace: &str, key: &str) -> Result<bool, CacheError> {
    validate_identity(namespace, key)?;
    ensure_schema(cfg)?;
    let existed = get(cfg, namespace, key)?.is_some();
    let sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", TABLE);
    shared_exec_params(&sql, &[namespace, key]).map_err(store_err)?;
    Ok(existed)
}

pub fn invalidate_namespace(cfg: &CacheConfig, namespace: &str) -> Result<i64, CacheError> {
    if namespace.is_empty() {
        return Err(CacheError::InvalidKey("namespace required".to_string()));
    }
    ensure_schema(cfg)?;
    let live = count_live(namespace)?;
    let sql = format!("DELETE FROM {} WHERE namespace=?1", TABLE);
    shared_exec_params(&sql, &[namespace]).map_err(store_err)?;
    Ok(live)
}

fn count_live(namespace: &str) -> Result<i64, CacheError> {
    let now_s = now_ms().to_string();
    let sql = format!(
        "SELECT COUNT(*) AS n FROM {} WHERE namespace=?1 AND (expires_at IS NULL OR expires_at > ?2)",
        TABLE
    );
    let rows = shared_query_params(&sql, &[namespace, &now_s]).map_err(store_err)?;
    Ok(rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| row_i64(r, "n"))
        .unwrap_or(0))
}

pub fn stats(cfg: &CacheConfig, namespace: &str) -> Result<(i64, i64), CacheError> {
    if namespace.is_empty() {
        return Err(CacheError::InvalidKey("namespace required".to_string()));
    }
    ensure_schema(cfg)?;
    let now_s = now_ms().to_string();
    let sql = format!(
        "SELECT COUNT(*) AS n, COALESCE(SUM(bytes),0) AS b FROM {} \
         WHERE namespace=?1 AND (expires_at IS NULL OR expires_at > ?2)",
        TABLE
    );
    let rows = shared_query_params(&sql, &[namespace, &now_s]).map_err(store_err)?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r,
        None => return Ok((0, 0)),
    };
    Ok((row_i64(row, "n").unwrap_or(0), row_i64(row, "b").unwrap_or(0)))
}

pub fn enforce_budget(cfg: &CacheConfig, namespace: &str) -> Result<usize, CacheError> {
    let now_s = now_ms().to_string();
    let purge = format!(
        "DELETE FROM {} WHERE namespace=?1 AND expires_at IS NOT NULL AND expires_at <= ?2",
        TABLE
    );
    let _ = shared_exec_params(&purge, &[namespace, &now_s]);

    let sql = format!(
        "SELECT key, bytes FROM {} WHERE namespace=?1 ORDER BY last_used_at DESC",
        TABLE
    );
    let rows = shared_query_params(&sql, &[namespace]).map_err(store_err)?;
    let arr = match rows.as_array() {
        Some(a) => a,
        None => return Ok(0),
    };

    let mut kept_bytes: i64 = 0;
    let mut victims: Vec<String> = Vec::new();
    for (i, row) in arr.iter().enumerate() {
        let key = match row.get("key").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => continue,
        };
        let b = row_i64(row, "bytes").unwrap_or(0);
        let over_entries = i >= cfg.max_entries_per_namespace;
        let over_bytes = kept_bytes.saturating_add(b) > cfg.max_bytes_per_namespace as i64;
        if over_entries || over_bytes {
            victims.push(key.to_string());
        } else {
            kept_bytes = kept_bytes.saturating_add(b);
        }
    }
    if victims.is_empty() {
        return Ok(0);
    }
    let del = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", TABLE);
    let mut evicted = 0usize;
    for key in &victims {
        if shared_exec_params(&del, &[namespace, key]).is_ok() {
            evicted += 1;
        }
    }
    if evicted > 0 {
        crate::wasm_dispatch::emit_event(
            "cache_evicted",
            json!({
                "namespace": namespace,
                "evicted": evicted,
                "kept_bytes": kept_bytes,
                "max_entries": cfg.max_entries_per_namespace,
                "max_bytes": cfg.max_bytes_per_namespace,
            }),
        );
    }
    Ok(evicted)
}
