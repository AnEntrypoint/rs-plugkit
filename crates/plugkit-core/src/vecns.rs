#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::vecstore::{drop_if_dim_mismatch_at, vec_to_json_literal};
use crate::wasm_dispatch::unpack_to_value_pub;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_plugin_call(
        plugin_ptr: *const u8, plugin_len: u32,
        verb_ptr: *const u8, verb_len: u32,
        body_ptr: *const u8, body_len: u32,
    ) -> u64;
}

/// Dispatches one call through the host_plugin_call import and parses the
/// JSON response. Every libsql/bert/treesitter call in this file routes
/// through this instead of calling crate::libsql_wasm::*/crate::embed::*/
/// tree_sitter::* in-process.
fn call_plugin(plugin: &str, verb: &str, body: &Value) -> Value {
    let body_s = body.to_string();
    let packed = unsafe {
        host_plugin_call(
            plugin.as_ptr(), plugin.len() as u32,
            verb.as_ptr(), verb.len() as u32,
            body_s.as_ptr(), body_s.len() as u32,
        )
    };
    unpack_to_value_pub(packed)
}

fn plugin_ok_err(resp: &Value) -> Result<(), String> {
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("plugin call failed").to_string())
    }
}

fn libsql_exec(db_name: &str, sql: &str) -> Result<(), String> {
    let resp = call_plugin("libsql", "exec", &json!({ "db": db_name, "sql": sql }));
    plugin_ok_err(&resp)
}

fn libsql_exec_params(db_name: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    let resp = call_plugin("libsql", "exec_params", &json!({ "db": db_name, "sql": sql, "params": params }));
    plugin_ok_err(&resp)
}

fn libsql_query_params(db_name: &str, sql: &str, params: &[&str]) -> Result<Value, String> {
    let resp = call_plugin("libsql", "query_params", &json!({ "db": db_name, "sql": sql, "params": params }));
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        Ok(resp.get("rows").cloned().unwrap_or(Value::Array(vec![])))
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("plugin call failed").to_string())
    }
}

/// Identifies one vector table's physical location: which db instance it
/// lives in, its table name, and its libsql_vector_idx index name. Every
/// call site (rssearch_vectors, git_commit_vectors, code_index's code_chunks
/// and memories tables) constructs one of these and delegates the shared
/// mechanics below to it, instead of reimplementing dim-mismatch drop /
/// index create-or-rebuild / shadow-row-recovery insert per call site with
/// slightly different constants.
pub struct VecTableSpec<'a> {
    pub db_name: &'a str,
    pub table: &'a str,
    pub index: &'a str,
}

impl<'a> VecTableSpec<'a> {
    /// DROP INDEX IF EXISTS + CREATE INDEX (unconditional, not IF NOT EXISTS)
    /// -- the shadow-row recovery step: rebuild the vector index from
    /// scratch after a corrupted shadow-table read.
    pub fn rebuild_index(&self) -> Result<(), String> {
        let _ = libsql_exec(self.db_name, &format!("DROP INDEX IF EXISTS {}", self.index));
        libsql_exec(self.db_name, &format!(
            "CREATE INDEX {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
            self.index, self.table
        ))
    }

    /// CREATE INDEX IF NOT EXISTS -- the normal (non-recovery) schema-ensure path.
    pub fn ensure_index(&self) {
        let _ = libsql_exec(self.db_name, &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
            self.index, self.table
        ));
    }

    /// Drops+recreates the table (via vecstore::drop_if_dim_mismatch_at) if
    /// its existing `embedding` column dimension no longer matches
    /// EXPECTED_EMBED_DIM. Must be called before CREATE TABLE IF NOT EXISTS
    /// so a stale-dim table gets rebuilt rather than silently kept.
    pub fn drop_if_dim_mismatch(&self) -> bool {
        drop_if_dim_mismatch_at(self.db_name, self.table).unwrap_or(false)
    }

    pub fn exec(&self, sql: &str) -> Result<(), String> {
        libsql_exec(self.db_name, sql)
    }

    pub fn exec_params(&self, sql: &str, params: &[&str]) -> Result<(), String> {
        libsql_exec_params(self.db_name, sql, params)
    }

    pub fn query_params(&self, sql: &str, params: &[&str]) -> Result<Value, String> {
        libsql_query_params(self.db_name, sql, params)
    }
}

pub fn is_shadow_row_err(err: &str) -> bool {
    err.contains("shadow row")
}

/// Insert-with-shadow-row-recovery, generalized from rssearch_vectors::write
/// (commit 2c0e96d). A "vector index(insert): failed to insert shadow row"
/// error was live-observed on brand-new keys that had never been written
/// before -- ruling out an ON-CONFLICT-DO-UPDATE race against an existing
/// row. The shadow table backing libsql_vector_idx is a real on-disk
/// structure that a watcher crash storm (abrupt process kills mid-write) can
/// leave corrupted -- corruption that persists across restarts since the
/// shadow table lives on disk, not in memory, and nothing ever drops+
/// recreates the index once CREATE INDEX IF NOT EXISTS has run once.
/// Recover by dropping and recreating the vector index and retrying the
/// insert exactly once; if the retry still fails, surface the (now
/// index-rebuilt) error rather than looping. `on_recovery` is invoked with
/// the original error text before the rebuild so callers can emit their own
/// event name/fields (each of the 3 call sites emits a differently-named
/// event today; this preserves that instead of forcing one shared name).
pub fn exec_with_shadow_row_recovery(
    spec: &VecTableSpec<'_>,
    sql: &str,
    params: &[&str],
    on_recovery: impl FnOnce(&str),
) -> Result<(), String> {
    match spec.exec_params(sql, params) {
        Ok(()) => Ok(()),
        Err(e) if is_shadow_row_err(&e) => {
            on_recovery(&e);
            spec.rebuild_index()?;
            spec.exec_params(sql, params)
        }
        Err(e) => Err(e),
    }
}

/// Runs a caller-supplied delete step (each call site's own error-handling:
/// rssearch_vectors::write propagates the delete error with `?`,
/// git_commit_vectors::sync_incremental swallows it with `let _ =` since its
/// prior `present` check already makes the delete a no-op in the common
/// case -- callers pass that choice in as `delete`), then inserts through
/// the shadow-row-recovery path. This is the delete-then-insert shape all 3
/// call sites use to sidestep libsql_vector_idx's unreliable ON CONFLICT DO
/// UPDATE support -- always a fresh insert.
pub fn delete_then_insert_with_recovery(
    spec: &VecTableSpec<'_>,
    delete: impl FnOnce(&VecTableSpec<'_>) -> Result<(), String>,
    insert_sql: &str,
    insert_params: &[&str],
    on_recovery: impl FnOnce(&str),
) -> Result<(), String> {
    delete(spec)?;
    exec_with_shadow_row_recovery(spec, insert_sql, insert_params, on_recovery)
}

/// Recency-decay parameters: `half_life_ms` controls how fast a row's
/// contribution to `score` decays with age; `recency_floor` is the
/// asymptotic minimum multiplier a row can never decay below (a very old
/// but highly-relevant hit is still findable, never zeroed out).
pub struct RecencyParams {
    pub half_life_ms: f64,
    pub recency_floor: f64,
}

/// Exponential recency decay + combined score, factored out of
/// rssearch_vectors::search_with_recency / search_memory_hits so both share
/// one implementation of the exact same formula:
/// recency = floor + (1-floor) * exp(-age_ms / half_life_ms); score = cos * recency.
pub fn recency_score(cos: f64, updated_at_ms: i64, now_ms: i64, p: &RecencyParams) -> (f64, f64) {
    let age_ms = (now_ms - updated_at_ms).max(0) as f64;
    let recency = p.recency_floor + (1.0 - p.recency_floor) * (-age_ms / p.half_life_ms).exp();
    (recency, cos * recency)
}

/// Query-budget: the `vector_top_k` candidate-pool size pulled before
/// re-ranking/filtering down to `limit` -- every call site uses
/// `limit.saturating_mul(multiplier).max(floor)`, just with different
/// literal constants (5/20 today, everywhere) -- parameterized so a future
/// call site can tune without touching this shared code.
pub struct QueryBudget {
    pub pool_multiplier: usize,
    pub pool_floor: usize,
}

impl QueryBudget {
    pub fn pool(&self, limit: usize) -> usize {
        limit.saturating_mul(self.pool_multiplier).max(self.pool_floor)
    }
}

impl Default for QueryBudget {
    fn default() -> Self {
        QueryBudget { pool_multiplier: 5, pool_floor: 20 }
    }
}

pub fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        out.push(x.as_f64()? as f32);
    }
    Some(out)
}

pub fn qlit(vec: &[f32]) -> String {
    vec_to_json_literal(vec)
}
