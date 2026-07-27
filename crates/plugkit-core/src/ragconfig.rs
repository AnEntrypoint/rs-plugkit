#![cfg(target_arch = "wasm32")]

//! Declarative description of the RAG/vector layer.
//!
//! Every knob here was a `const` scattered across `vecstore.rs`, `vecns.rs`,
//! `rssearch_vectors.rs`, `git_commit_vectors.rs`, `code_index.rs` and
//! `wasm_dispatch/verbs.rs`. Hardcoding them meant a second knowledgebase --
//! a different embedding model, a differently-tuned recency curve, a
//! domain-specific namespace set -- could only exist by forking the modules.
//! Hoisting them into one struct makes a knowledgebase a *value*, so a
//! resolution layer (`config.rs`, owned separately) can populate it from disk
//! without any of the consuming modules learning where config comes from.
//!
//! INVARIANT: every `Default` impl in this file reproduces the exact constant
//! it replaced. Defaulting must be a byte-for-byte no-op on live stores --
//! this landed alongside a live `.gm/gm.db` holding real `code_chunks` and
//! `rssearch_vectors` rows, and a drifted default would silently re-tune
//! retrieval (or, for `embed_dim`, trigger a real table drop) on the next
//! boot of every existing project.
//!
//! CONCURRENCY: the plugin instance is process-wide and shared across
//! concurrently-active projects, so nothing here is cached in a `static`.
//! Config is passed by reference into each call, constructed per-dispatch by
//! the caller. A process-global "current config" would let project A's
//! knowledgebase settings leak into project B's index pass.

use serde_json::json;

/// Physical location of one vector table + its libsql ANN index.
///
/// Kept separate from `VecTableSpec` (which additionally carries the resolved
/// `db_name` for a specific call) because table/index NAMES are configuration
/// while the db path is resolved per-project at call time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VecTableNames {
    pub table: String,
    pub index: String,
}

impl VecTableNames {
    pub fn new(table: &str, index: &str) -> Self {
        VecTableNames { table: table.to_string(), index: index.to_string() }
    }

    /// The convention every table in this codebase already follows: the ANN
    /// index is the table name with a `_vec` suffix. `drop_if_dim_mismatch_at`
    /// hardcodes exactly this derivation when it drops an index it was never
    /// told the name of, so a config that breaks the convention must supply
    /// both names explicitly rather than relying on this helper.
    pub fn derived(table: &str) -> Self {
        VecTableNames { table: table.to_string(), index: format!("{}_vec", table) }
    }
}

/// How a raw cosine distance becomes a ranked score.
///
/// `half_life_ms`/`recency_floor` are the exponential-decay recency multiplier
/// applied on top of cosine similarity; `cos_floor` drops hits below a
/// similarity bar BEFORE recency can rescue them (a stale-but-relevant hit is
/// worth keeping, an irrelevant-but-fresh one is not).
#[derive(Clone, Debug, PartialEq)]
pub struct ScoringConfig {
    pub half_life_ms: f64,
    pub recency_floor: f64,
    /// Minimum cosine similarity a hit must clear to be scored at all.
    /// 0.0 keeps every candidate the ANN index returned -- which is what both
    /// live `search_memory_hits` call sites in `verbs.rs` pass today.
    pub cos_floor: f64,
    /// Jaccard token overlap at or above which a lower-scored hit is dropped
    /// as a near-duplicate of one already kept.
    pub dedup_jaccard: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        ScoringConfig {
            // 30 days, as `rssearch_vectors::HALF_LIFE_MS` spelled it out.
            half_life_ms: 30.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            recency_floor: 0.4,
            cos_floor: 0.0,
            dedup_jaccard: 0.7,
        }
    }
}

/// How many rows to pull out of the ANN index relative to the caller's limit.
///
/// The ANN pool must overshoot the requested limit because recency reweighting
/// and dedup both happen AFTER retrieval: a hit that wins on final score can
/// sit outside the top-`limit` by raw cosine distance, so retrieving exactly
/// `limit` rows would make the reranker a no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryBudgetConfig {
    pub pool_multiplier: usize,
    pub pool_floor: usize,
    /// Default `limit` when a caller does not specify one (the `recall` verb's
    /// historical `unwrap_or(8)`).
    pub default_limit: usize,
    /// Default `k` for the code-search verbs (their historical
    /// `unwrap_or(10)`).
    pub default_k: usize,
}

impl Default for QueryBudgetConfig {
    fn default() -> Self {
        QueryBudgetConfig { pool_multiplier: 5, pool_floor: 20, default_limit: 8, default_k: 10 }
    }
}

impl QueryBudgetConfig {
    pub fn pool(&self, limit: usize) -> usize {
        limit.saturating_mul(self.pool_multiplier).max(self.pool_floor)
    }
}

/// Namespace vocabulary of a knowledgebase.
///
/// `code` is the one namespace treated structurally differently everywhere:
/// it is fed by the tree-sitter code indexer rather than by markdown memory
/// files, so `rssearch_vector_hits` migrates it from flat JSON instead of
/// syncing it, and `memory_md`'s digest/sync passes skip it entirely. That
/// branch was written as a literal `ns == "codeinsight"` in four places; it is
/// a name, not a law, so it belongs in config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceConfig {
    /// Namespace holding code chunks (default `codeinsight`).
    pub code: String,
    /// Namespace used when a caller names none (default `default`).
    pub default: String,
    /// Suffix appended to a namespace to address its flat-JSON embedding
    /// sidecar (`<ns>-vec`).
    pub vec_suffix: String,
    /// Suffix for the code namespace's file manifest (`<code>-manifest`).
    pub manifest_suffix: String,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        NamespaceConfig {
            code: "codeinsight".to_string(),
            default: "default".to_string(),
            vec_suffix: "-vec".to_string(),
            manifest_suffix: "-manifest".to_string(),
        }
    }
}

impl NamespaceConfig {
    pub fn is_code(&self, ns: &str) -> bool {
        ns == self.code
    }

    pub fn vec_namespace(&self, ns: &str) -> String {
        format!("{}{}", ns, self.vec_suffix)
    }

    pub fn manifest_namespace(&self) -> String {
        format!("{}{}", self.code, self.manifest_suffix)
    }
}

/// The embedding model's output dimension, plus the policy for what happens
/// when a store on disk disagrees with it.
///
/// This is the one setting that can DESTROY data, so it is modelled
/// explicitly rather than as a bare `usize`. libsql's `F32_BLOB(n)` column
/// type is fixed at CREATE time and its `vector_top_k` index is built against
/// that width -- a store written at 384 cannot answer a 768-dim query, it
/// errors or (worse) returns garbage distances. There is no in-place migration
/// short of re-embedding every row, which the indexer does anyway on its next
/// pass, so the existing behaviour is: drop the mismatched table + index and
/// let it rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedDimConfig {
    pub dim: usize,
    /// When false, a dimension mismatch is REPORTED but the table is left
    /// intact. Retrieval against that table will fail until it is rebuilt --
    /// which is the correct trade for an operator who would rather diagnose a
    /// loud failure than have a store silently emptied under them.
    pub drop_on_mismatch: bool,
}

impl Default for EmbedDimConfig {
    fn default() -> Self {
        // 384 = BAAI/bge-small-en-v1.5's hidden size, the model `embed.rs`
        // embeds as safetensors AND the width the host's `host_vec_embed`
        // delegation probe is validated against. Changing this alone is not
        // enough: `embed.rs`'s EMBED_DIM and its bert Config must move with
        // it, or every embed call fails its own dim check and returns None.
        EmbedDimConfig { dim: 384, drop_on_mismatch: true }
    }
}

impl EmbedDimConfig {
    /// Single decision point for a dimension mismatch, so no call site can
    /// invent its own policy. Returns whether the caller should destroy the
    /// store; emits the diagnostic either way, because an operator who set
    /// `drop_on_mismatch=false` still needs to know their store is now
    /// unqueryable.
    pub fn should_drop(&self, table: &str, found_dim: usize) -> bool {
        if found_dim == self.dim {
            return false;
        }
        crate::wasm_dispatch::emit_event("embed_dim_mismatch", json!({
            "table": table,
            "old_dim": found_dim,
            "new_dim": self.dim,
            "will_drop": self.drop_on_mismatch,
        }));
        self.drop_on_mismatch
    }
}

/// A whole knowledgebase's retrieval settings.
///
/// Threaded by reference into the vector modules. Constructed with
/// `RagConfig::default()` at every call site today; a resolution layer will
/// later hand in a populated one without any of those modules changing shape.
#[derive(Clone, Debug, PartialEq)]
pub struct RagConfig {
    pub embed: EmbedDimConfig,
    pub namespaces: NamespaceConfig,
    pub scoring: ScoringConfig,
    pub budget: QueryBudgetConfig,
    /// The namespace-keyed memory/RAG table (default `rssearch_vectors`).
    pub rssearch: VecTableNames,
    /// Commit-message/diff vectors (default `git_commit_vectors`).
    pub git_commits: VecTableNames,
    /// Tree-sitter code chunks (default `code_chunks`).
    pub code_chunks: VecTableNames,
    /// Legacy flat memory table kept alongside `code_chunks` in the project db.
    pub memories: VecTableNames,
}

impl Default for RagConfig {
    fn default() -> Self {
        RagConfig {
            embed: EmbedDimConfig::default(),
            namespaces: NamespaceConfig::default(),
            scoring: ScoringConfig::default(),
            budget: QueryBudgetConfig::default(),
            rssearch: VecTableNames::derived("rssearch_vectors"),
            git_commits: VecTableNames::derived("git_commit_vectors"),
            code_chunks: VecTableNames::derived("code_chunks"),
            memories: VecTableNames::derived("memories"),
        }
    }
}

/// Accept only `[A-Za-z_][A-Za-z0-9_]*`.
///
/// Deliberately stricter than SQLite's own identifier rules (no quoting, no
/// dots, no unicode): these names reach SQL through `format!`, never a bind
/// parameter, so the whitelist is the entire defence. Rejecting a legal-but-
/// exotic name is a far better failure than accepting one carrying a quote.
fn valid_sql_ident(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "ragconfig: {:?} is not a valid SQL identifier -- table/index names are interpolated \
             directly into SQL, so only [A-Za-z_][A-Za-z0-9_]* is accepted",
            name
        ))
    }
}

impl RagConfig {
    /// Convenience for the many call sites that only need the dimension.
    pub fn dim(&self) -> usize {
        self.embed.dim
    }

    /// Reject a config whose settings would destroy or permanently break a
    /// store, BEFORE any schema call acts on it.
    ///
    /// A resolution layer reads config off disk, so unlike the compile-time
    /// assertion in `embed.rs` these values are not known until runtime -- and
    /// the failure mode is silent and total: a `dim` the compiled embedder
    /// cannot emit makes every `ensure_schema_*` drop its table (width
    /// mismatch), then every write fails its own width check, leaving a
    /// permanently empty knowledgebase that reports no errors at the verb
    /// layer. Callers should refuse a config that fails this rather than fall
    /// back to defaults, since silently ignoring an operator's stated dim is
    /// how a store gets rebuilt at a width nobody asked for.
    pub fn validate(&self) -> Result<(), String> {
        if self.embed.dim == 0 {
            return Err("ragconfig: embed.dim must be non-zero".to_string());
        }
        if self.embed.dim != crate::vecstore::EXPECTED_EMBED_DIM {
            return Err(format!(
                "ragconfig: embed.dim {} does not match this binary's compiled embedder width {} -- \
                 every table would be dropped as mismatched and then never repopulate, because the \
                 embedder cannot produce vectors of the configured width. Swap the model weights \
                 (embed.rs) together with this setting, or leave it at the default.",
                self.embed.dim, crate::vecstore::EXPECTED_EMBED_DIM
            ));
        }
        // A cosine floor above 1.0 rejects every possible hit (cos is bounded
        // by 1 for the normalized vectors embed.rs produces), which reads as
        // "the knowledgebase is empty" at the verb layer.
        if !(0.0..=1.0).contains(&self.scoring.cos_floor) {
            return Err(format!(
                "ragconfig: scoring.cos_floor {} outside [0,1]; embeddings are L2-normalized so \
                 cosine similarity cannot exceed 1 -- a higher floor silently matches nothing",
                self.scoring.cos_floor
            ));
        }
        if !(0.0..=1.0).contains(&self.scoring.recency_floor) {
            return Err(format!(
                "ragconfig: scoring.recency_floor {} outside [0,1]; it is the multiplier an \
                 infinitely-old hit decays to, so a value above 1 would boost stale hits over fresh ones",
                self.scoring.recency_floor
            ));
        }
        if self.scoring.half_life_ms <= 0.0 {
            return Err("ragconfig: scoring.half_life_ms must be positive (it divides an age)".to_string());
        }
        if self.budget.pool_multiplier == 0 {
            return Err("ragconfig: budget.pool_multiplier must be non-zero; a zero pool retrieves nothing".to_string());
        }
        // Table/index names are string-interpolated into SQL (libsql has no
        // bind parameter for an identifier), which was safe while they were
        // `const &str` literals but is an injection surface the moment they
        // come from a config file. Constrain them to bare identifiers.
        for names in [&self.rssearch, &self.git_commits, &self.code_chunks, &self.memories] {
            valid_sql_ident(&names.table)?;
            valid_sql_ident(&names.index)?;
        }
        if self.namespaces.code.is_empty() || self.namespaces.default.is_empty() {
            return Err("ragconfig: namespaces.code and namespaces.default must be non-empty".to_string());
        }
        Ok(())
    }
}
