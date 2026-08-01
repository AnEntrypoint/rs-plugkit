#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::ragconfig::{EmbedDimConfig, QueryBudgetConfig, ScoringConfig, VecTableNames};
use crate::vecstore::{drop_if_dim_mismatch_at_cfg, vec_to_json_literal};
use crate::wasm_dispatch::unpack_to_value_pub;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_plugin_call(
        plugin_ptr: *const u8, plugin_len: u32,
        verb_ptr: *const u8, verb_len: u32,
        body_ptr: *const u8, body_len: u32,
    ) -> u64;
}

fn call_libsql_plugin(plugin: &str, verb: &str, body: &Value) -> Value {
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
    let resp = call_libsql_plugin("libsql", "exec", &json!({ "path": db_name, "sql": sql }));
    plugin_ok_err(&resp)
}

fn libsql_exec_params(db_name: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    let resp = call_libsql_plugin("libsql", "exec_params", &json!({ "path": db_name, "sql": sql, "params": params }));
    plugin_ok_err(&resp)
}

fn libsql_query_params(db_name: &str, sql: &str, params: &[&str]) -> Result<Value, String> {
    let resp = call_libsql_plugin("libsql", "query_params", &json!({ "path": db_name, "sql": sql, "params": params }));
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        Ok(resp.get("rows").cloned().unwrap_or(Value::Array(vec![])))
    } else {
        Err(resp.get("error").and_then(|v| v.as_str()).unwrap_or("plugin call failed").to_string())
    }
}

pub struct VecTableSpec<'a> {
    pub db_name: &'a str,
    pub table: &'a str,
    pub index: &'a str,
}

impl<'a> VecTableSpec<'a> {
    /// Build a spec from configured names plus a call-time db path.
    ///
    /// The lifetime is tied to `names`, not to the spec, so the caller must
    /// keep the `VecTableNames` alive for the duration -- config is owned by
    /// the caller and passed down, never stashed in a process-global (the
    /// plugin instance is shared across concurrently-active projects).
    pub fn from_names(db_name: &'a str, names: &'a VecTableNames) -> VecTableSpec<'a> {
        VecTableSpec { db_name, table: &names.table, index: &names.index }
    }

    pub fn rebuild_index(&self) -> Result<(), String> {
        let _ = libsql_exec(self.db_name, &format!("DROP INDEX IF EXISTS {}", self.index));
        libsql_exec(self.db_name, &format!(
            "CREATE INDEX {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
            self.index, self.table
        ))
    }

    pub fn ensure_index(&self) {
        let _ = libsql_exec(self.db_name, &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
            self.index, self.table
        ));
    }

    /// Config-driven mismatch guard. Every schema-ensuring path must call this
    /// BEFORE its CREATE TABLE, because
    /// `CREATE TABLE IF NOT EXISTS` is a silent no-op against a surviving
    /// old-width table -- the width in the CREATE would be ignored and the
    /// store would keep answering queries with the wrong vector length.
    pub fn drop_if_dim_mismatch_cfg(&self, cfg: &EmbedDimConfig) -> bool {
        drop_if_dim_mismatch_at_cfg(self.db_name, self.table, cfg).unwrap_or(false)
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

/// Gates dropping and rebuilding a vector index.
pub fn is_shadow_row_err(err: &str) -> bool {
    crate::libsql_wasm::classify_error(err) == crate::libsql_wasm::LibsqlErrorKind::ShadowRow
}

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

pub struct RecencyParams {
    pub half_life_ms: f64,
    pub recency_floor: f64,
}

impl RecencyParams {
    /// Project a `ScoringConfig` onto the recency-only view `recency_score`
    /// needs. Kept as a projection rather than replacing `RecencyParams`
    /// outright so scoring stays a pure function of two numbers -- the cosine
    /// floor and dedup threshold are *filtering* decisions made by the caller
    /// before/after scoring, not inputs to the decay curve.
    pub fn from_scoring(cfg: &ScoringConfig) -> RecencyParams {
        RecencyParams { half_life_ms: cfg.half_life_ms, recency_floor: cfg.recency_floor }
    }
}

pub fn recency_score(cos: f64, updated_at_ms: i64, now_ms: i64, p: &RecencyParams) -> (f64, f64) {
    let age_ms = (now_ms - updated_at_ms).max(0) as f64;
    let recency = p.recency_floor + (1.0 - p.recency_floor) * (-age_ms / p.half_life_ms).exp();
    (recency, cos * recency)
}

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

impl QueryBudget {
    /// Project a `QueryBudgetConfig` onto the pool-sizing view. The default
    /// limit/k fields stay behind in config because they belong to the verb
    /// layer (what a caller asked for), not to ANN retrieval sizing.
    pub fn from_config(cfg: &QueryBudgetConfig) -> QueryBudget {
        QueryBudget { pool_multiplier: cfg.pool_multiplier, pool_floor: cfg.pool_floor }
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
