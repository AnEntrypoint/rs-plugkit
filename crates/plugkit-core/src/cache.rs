#![cfg(target_arch = "wasm32")]
//! The generalized cache abstraction every other cache in this tree should
//! eventually route through, and that other plugins can reach over
//! `host_plugin_call`.
//!
//! Why this exists: this codebase keeps re-growing the same cache by hand, and
//! each hand-rolled copy re-learns the same failure modes the hard way. The
//! codeinsight file manifest just cost a fix for a permanent whole-tree digest
//! mismatch -- a cache whose stored key and its recomputed key were different
//! quantities (stored mtime vs. `fnv1a64(content)`), so every entry missed
//! forever while reporting itself healthy. The recall path just cost a
//! separate fix for the opposite defect: a dead embedder returned
//! `ok:true, hits:[]`, a "miss" the caller could not tell apart from a hard
//! error. Both are cache bugs, not domain bugs, and both are structurally
//! excluded by the contract below.
//!
//! Contract, in order of importance:
//!   1. A MISS AND AN ERROR ARE DIFFERENT VALUES. `get` returns
//!      `Result<Option<Entry>, CacheError>`: `Ok(None)` is a real, witnessed
//!      absence; `Err(_)` means the store could not answer and the caller must
//!      NOT treat it as "not cached" and overwrite good state. This is the
//!      single rule the recall bug violated, so it is the one the type system
//!      enforces here rather than a convention a future caller can forget.
//!   2. EXPIRY IS A MISS, NEVER STALE DATA. TTL is evaluated in SQL on the read
//!      path, so an expired row cannot be returned even if a sweep has not run
//!      yet. Eviction is a space optimization; correctness never depends on it.
//!   3. THE STORED HASH IS OF THE VALUE. `content_hash` is `fnv1a64` over the
//!      value bytes -- the same quantity on write and on any later re-check --
//!      because a hash of some *proxy* for the value (an mtime, a size, a path)
//!      is exactly the digest-mismatch bug that was just fixed upstream.
//!   4. CONCURRENCY-SAFE ACROSS PROJECTS. The plugin instance is process-wide
//!      and shared by concurrently-active projects, and the libsql plugin
//!      opens/closes per call with no retained connection, so nothing may be
//!      cached in a process-global here. Every operation is a single statement
//!      against the shared db, and `put` is an upsert -- concurrent writers of
//!      the same key race to a last-writer-wins outcome rather than to a
//!      duplicate row or a lost table.
//!   5. BUDGETS ARE CONFIG, NOT CONSTANTS. `CacheConfig` is threaded through
//!      every entry point so budgets/TTLs become vendorable later without
//!      touching call sites. `DEFAULTS` is the one place the defaults live.

use serde_json::{json, Value};

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params};

const TABLE: &str = "cache_entries";

/// Namespace budgets and TTL policy.
///
/// Deliberately a struct threaded through the API rather than a set of `const`
/// reads at the point of use: a later change makes these vendorable per
/// project, and a hardcoded constant read deep inside `put` would have to be
/// re-plumbed through every caller at that point. Passing config from the edge
/// now costs one parameter and makes that change a no-op at the call sites.
#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    /// Maximum live entries retained per namespace. Exceeding it evicts the
    /// least-recently-used entries down to the budget.
    pub max_entries_per_namespace: usize,
    /// Maximum total value bytes retained per namespace, enforced by the same
    /// LRU pass. A namespace can breach this with a single oversized value;
    /// `max_value_bytes` is what actually bounds one entry.
    pub max_bytes_per_namespace: usize,
    /// Largest single value accepted. A value over this is REJECTED at `put`
    /// with a visible error rather than silently truncated -- a truncated
    /// cached value is indistinguishable from a real one on read, which is the
    /// worst possible failure mode for a cache.
    pub max_value_bytes: usize,
    /// TTL applied when a `put` does not specify one. `None` means entries
    /// live until evicted by budget.
    pub default_ttl_ms: Option<i64>,
}

/// The single place cache defaults live.
///
/// Sizes are chosen against what this store already holds: `gm.db` runs tens of
/// MB of code chunks and vectors, so a cache namespace is budgeted to stay a
/// small fraction of that rather than to compete with it. The 1 MiB per-value
/// ceiling is above any JSON verb payload this dispatch table produces and well
/// under the point where a single row would stall the shared db for a
/// concurrent project.
pub const DEFAULTS: CacheConfig = CacheConfig {
    max_entries_per_namespace: 512,
    max_bytes_per_namespace: 8 * 1024 * 1024,
    max_value_bytes: 1024 * 1024,
    // No default expiry: an entry that is still within budget is still valid.
    // A caller that needs freshness states its own TTL, which is honest --
    // inventing a global one here would silently expire callers that never
    // asked for it.
    default_ttl_ms: None,
};

/// Every way a cache operation can fail, kept distinct from a miss.
///
/// `Store` carries the underlying message verbatim instead of collapsing to a
/// bool, because the shared-db layer distinguishes recoverable corruption
/// ("malformed") from ordinary failure, and a caller that flattens the two
/// loses the ability to trigger recovery.
#[derive(Debug)]
pub enum CacheError {
    /// Namespace or key was empty. Both are part of the identity of an entry,
    /// so an empty one would alias unrelated writes onto the same row.
    InvalidKey(String),
    /// Value exceeded `max_value_bytes`. Rejected, never truncated.
    ValueTooLarge { bytes: usize, limit: usize },
    /// The libsql store itself failed (open, schema, statement).
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

    /// Stable machine-readable discriminant, so a caller over the verb boundary
    /// can branch on the failure class without parsing the message text.
    pub fn kind(&self) -> &'static str {
        match self {
            CacheError::InvalidKey(_) => "invalid_key",
            CacheError::ValueTooLarge { .. } => "value_too_large",
            CacheError::Store(_) => "store_failure",
        }
    }
}

/// A live cache entry as read back from the store.
#[derive(Debug, Clone)]
pub struct Entry {
    pub namespace: String,
    pub key: String,
    pub value: String,
    /// `fnv1a64` of `value`'s bytes, hex. Recomputable by the caller from the
    /// value alone -- see the module docs on why this is a hash of the content
    /// and never of a proxy for it.
    pub content_hash: String,
    pub created_at: i64,
    /// Absolute expiry instant, not a duration, so a read is a comparison
    /// against `now` and never depends on when the row was last touched.
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

/// FNV-1a over the value bytes.
///
/// Same function and same input domain as the digest the code index stores, so
/// a hash computed here and one computed there for identical bytes agree. That
/// property is the whole point: the fixed digest-mismatch bug happened because
/// two sides of a comparison hashed different quantities.
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

/// Numeric columns come back from the libsql plugin as either JSON numbers or
/// strings depending on the driver path, so every integer read goes through
/// this rather than a bare `as_i64` -- matching the same defensive read
/// `chunk_rows_by_path` already needs in code_index.
fn row_i64(row: &Value, col: &str) -> Option<i64> {
    row.get(col).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// Create the table if absent.
///
/// Called at the top of every public operation rather than once at boot,
/// because there is no retained connection and no process-wide "already
/// initialized" flag would be safe: the instance is shared across projects, so
/// a flag set while project A was active would wrongly suppress schema creation
/// for project B's separate database file. `CREATE TABLE IF NOT EXISTS` is
/// cheap and idempotent, which is the right cost to pay for that safety.
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
    // Read path filters on (namespace, expires_at) and the LRU pass orders by
    // last_used_at within a namespace; without this index both degrade to a
    // full scan once a namespace is at its entry budget.
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

/// Read one entry.
///
/// `Ok(None)` is a real miss (absent or expired). `Err` means the store could
/// not answer -- see contract rule 1: the caller must not treat that as a miss.
///
/// Expiry is evaluated in the WHERE clause, so an expired row is unreachable
/// through this function regardless of whether eviction has swept it yet.
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
    // Touch for LRU. A failed touch degrades eviction ordering but never
    // correctness, so it must not turn a successful read into an error --
    // returning Err here would make a healthy hit look like a store failure.
    let touch = format!("UPDATE {} SET last_used_at=?1 WHERE namespace=?2 AND key=?3", TABLE);
    let _ = shared_exec_params(&touch, &[&now_s, namespace, key]);
    Ok(Some(entry))
}

/// Insert or replace an entry, then enforce the namespace budget.
///
/// Returns the stored `content_hash` so a caller can record what it cached
/// without recomputing the hash itself.
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
    // A non-positive TTL would compute an expiry at or before `now`, storing a
    // row that can never be read. Rejecting it as invalid rather than storing
    // an instantly-dead entry keeps "put succeeded" meaning "a later get can
    // hit".
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
    // Upsert on the (namespace, key) primary key: two projects racing the same
    // key converge to last-writer-wins with one row, rather than to a duplicate
    // or a constraint failure. created_at is deliberately overwritten -- a
    // rewritten value is a new entry, and keeping the original timestamp would
    // make an age check lie about content that has since changed.
    //
    // NULLIF(?6,'') is written into the statement directly rather than patched
    // in afterwards: the params array binds every value as text, so a real NULL
    // for "no expiry" cannot be passed through it, and an empty string is the
    // stand-in. Expressing that inline keeps the placeholder's meaning visible
    // at the one place the statement is defined -- a later edit that adds
    // another `?6`-shaped placeholder cannot silently re-target it.
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

    // Budget enforcement is a separate, best-effort pass: a failure here leaves
    // a namespace temporarily over budget, which is a space problem, not a
    // correctness one, and must not fail a write that already landed.
    let _ = enforce_budget(cfg, namespace);
    Ok(hash)
}

/// Remove one entry. `Ok(true)` when a live entry existed, `Ok(false)` when
/// there was nothing to remove -- the distinction a caller needs to tell
/// "invalidated something" from "already gone", and separate again from `Err`.
pub fn invalidate(cfg: &CacheConfig, namespace: &str, key: &str) -> Result<bool, CacheError> {
    validate_identity(namespace, key)?;
    ensure_schema(cfg)?;
    // Existence is checked before the delete rather than inferred from it,
    // because the exec path reports no row count. An expired-but-unswept row
    // reports false (it was already logically absent) and is still deleted.
    let existed = get(cfg, namespace, key)?.is_some();
    let sql = format!("DELETE FROM {} WHERE namespace=?1 AND key=?2", TABLE);
    shared_exec_params(&sql, &[namespace, key]).map_err(store_err)?;
    Ok(existed)
}

/// Drop every entry in a namespace. Returns the count that was live
/// beforehand, so the caller can see whether the call did anything.
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

/// Count of non-expired entries in a namespace.
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

/// Live entry count and total live bytes for a namespace, for `cache_stats`.
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

/// Bound a namespace to its configured entry and byte budgets.
///
/// Expired rows are dropped first -- they are already unreachable through
/// `get`, so reclaiming them may satisfy the budget without evicting anything
/// live. Whatever remains over budget is evicted least-recently-used first.
///
/// "LRU-ish" is the honest description: `last_used_at` is updated on read
/// best-effort, and a concurrent writer from another project can interleave
/// between the SELECT that picks victims and the DELETE that removes them. The
/// consequence of losing that race is evicting a slightly-wrong entry, which
/// costs one later miss -- never a correctness violation, because a miss is
/// always a legal answer for a cache.
pub fn enforce_budget(cfg: &CacheConfig, namespace: &str) -> Result<usize, CacheError> {
    let now_s = now_ms().to_string();
    let purge = format!(
        "DELETE FROM {} WHERE namespace=?1 AND expires_at IS NOT NULL AND expires_at <= ?2",
        TABLE
    );
    let _ = shared_exec_params(&purge, &[namespace, &now_s]);

    // Victims are selected newest-first and then walked from the end, so the
    // running byte total reflects what the namespace would retain -- everything
    // past the point where either budget is breached is evicted.
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
