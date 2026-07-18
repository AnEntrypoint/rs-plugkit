#![cfg(target_arch = "wasm32")]

use serde_json::Value;

use crate::libsql_wasm;
use crate::vecstore::{drop_if_dim_mismatch_at, vec_to_json_literal};

/// Identifies one vector table's physical location: which db instance it
/// lives in, its table name, and its libsql_vector_idx index name. Every
/// call site (rssearch_vectors, git_commit_vectors, code_index's code_chunks
/// and memories tables) constructs one of these and delegates the shared
/// mechanics below to it, instead of reimplementing dim-mismatch drop /
/// index create-or-rebuild / shadow-row-recovery insert per call site with
/// slightly different constants.
pub struct VecTableSpec {
    pub db_name: &'static str,
    pub table: &'static str,
    pub index: &'static str,
}

impl VecTableSpec {
    /// DROP INDEX IF EXISTS + CREATE INDEX (unconditional, not IF NOT EXISTS)
    /// -- the shadow-row recovery step: rebuild the vector index from
    /// scratch after a corrupted shadow-table read.
    pub fn rebuild_index(&self) -> Result<(), String> {
        let _ = libsql_wasm::exec(self.db_name, &format!("DROP INDEX IF EXISTS {}", self.index));
        libsql_wasm::exec(self.db_name, &format!(
            "CREATE INDEX {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
            self.index, self.table
        ))
    }

    /// CREATE INDEX IF NOT EXISTS -- the normal (non-recovery) schema-ensure path.
    pub fn ensure_index(&self) {
        let _ = libsql_wasm::exec(self.db_name, &format!(
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

    fn exec(&self, sql: &str) -> Result<(), String> {
        libsql_wasm::exec(self.db_name, sql)
    }

    fn exec_params(&self, sql: &str, params: &[&str]) -> Result<(), String> {
        libsql_wasm::exec_params(self.db_name, sql, params)
    }

    pub fn query_params(&self, sql: &str, params: &[&str]) -> Result<Value, String> {
        libsql_wasm::query_params(self.db_name, sql, params)
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
    spec: &VecTableSpec,
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

/// Delete-then-insert a single embedding row, the shape all 3 call sites use
/// to sidestep libsql_vector_idx's unreliable ON CONFLICT DO UPDATE support
/// -- always a fresh insert. `delete_sql`/`delete_params` runs first
/// (best-effort, error ignored, matching every existing call site), then
/// `insert_sql`/`insert_params` runs through the shadow-row-recovery path.
pub fn delete_then_insert_with_recovery(
    spec: &VecTableSpec,
    delete_sql: &str,
    delete_params: &[&str],
    insert_sql: &str,
    insert_params: &[&str],
    on_recovery: impl FnOnce(&str),
) -> Result<(), String> {
    let _ = spec.exec_params(delete_sql, delete_params);
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
