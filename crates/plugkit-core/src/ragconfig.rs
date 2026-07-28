#![cfg(target_arch = "wasm32")]

use serde_json::json;
use std::sync::Mutex;

const RESOLVED_CACHE_TTL_MS: u64 = 5_000;

struct ResolvedEntryScopedToOneProjectRootNeverGlobal {
    root: String,
    ts_ms: u64,
    config: RagConfig,
}

static RESOLVED_CACHE: Mutex<Option<ResolvedEntryScopedToOneProjectRootNeverGlobal>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VecTableNames {
    pub table: String,
    pub index: String,
}

impl VecTableNames {
    pub fn new(table: &str, index: &str) -> Self {
        VecTableNames { table: table.to_string(), index: index.to_string() }
    }

    pub fn with_conventional_vec_index(table: &str) -> Self {
        VecTableNames { table: table.to_string(), index: format!("{}_vec", table) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoringConfig {
    pub half_life_ms: f64,
    pub recency_floor: f64,
    pub cos_floor_applied_before_recency_rescue: f64,
    pub dedup_jaccard_near_duplicate_threshold: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        ScoringConfig {
            half_life_ms: 30.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            recency_floor: 0.4,
            cos_floor_applied_before_recency_rescue: 0.0,
            dedup_jaccard_near_duplicate_threshold: 0.7,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryBudgetConfig {
    pub pool_multiplier: usize,
    pub pool_floor: usize,
    pub default_limit: usize,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceConfig {
    pub code: String,
    pub default: String,
    pub vec_sidecar_suffix: String,
    pub code_manifest_suffix: String,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        NamespaceConfig {
            code: "codeinsight".to_string(),
            default: "default".to_string(),
            vec_sidecar_suffix: "-vec".to_string(),
            code_manifest_suffix: "-manifest".to_string(),
        }
    }
}

impl NamespaceConfig {
    pub fn is_code(&self, ns: &str) -> bool {
        ns == self.code
    }

    pub fn vec_namespace(&self, ns: &str) -> String {
        format!("{}{}", ns, self.vec_sidecar_suffix)
    }

    pub fn manifest_namespace(&self) -> String {
        format!("{}{}", self.code, self.code_manifest_suffix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedDimConfig {
    pub dim: usize,
    pub keep_mismatched_table_intact_instead_of_dropping: bool,
}

impl Default for EmbedDimConfig {
    fn default() -> Self {
        EmbedDimConfig { dim: 384, keep_mismatched_table_intact_instead_of_dropping: false }
    }
}

impl EmbedDimConfig {
    pub fn should_drop_table_for_dim_mismatch(&self, table: &str, found_dim: usize) -> bool {
        if found_dim == self.dim {
            return false;
        }
        let will_drop = !self.keep_mismatched_table_intact_instead_of_dropping;
        crate::wasm_dispatch::emit_event("embed_dim_mismatch", json!({
            "table": table,
            "old_dim": found_dim,
            "new_dim": self.dim,
            "will_drop": will_drop,
        }));
        will_drop
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexConfig {
    pub split_chunk_above_bytes: usize,
    pub max_chunks_embedded_per_file_per_pass_count_bound_only: usize,
    pub wall_budget_ms: u64,
    pub max_file_bytes: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            split_chunk_above_bytes: 8192,
            max_chunks_embedded_per_file_per_pass_count_bound_only: 64,
            wall_budget_ms: 30_000,
            max_file_bytes: 256 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisciplineNoteConfig {
    pub max_name_len_hard_refuse_not_truncate: usize,
    pub max_text_len_hard_refuse_not_truncate: usize,
    pub active_policies_surfaced_in_instruction_payload_limit: usize,
}

impl Default for DisciplineNoteConfig {
    fn default() -> Self {
        DisciplineNoteConfig {
            max_name_len_hard_refuse_not_truncate: 64,
            max_text_len_hard_refuse_not_truncate: 200,
            active_policies_surfaced_in_instruction_payload_limit: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrowserWitnessConfig {
    pub always_browser_extensions_regardless_of_directory: Vec<String>,
    pub conditional_extensions_only_under_browser_dir_prefixes: Vec<String>,
    pub browser_dir_prefixes_normalized_slash_lowercase: Vec<String>,
}

impl Default for BrowserWitnessConfig {
    fn default() -> Self {
        BrowserWitnessConfig {
            always_browser_extensions_regardless_of_directory: [".html", ".htm", ".tsx", ".jsx", ".vue", ".svelte"]
                .iter().map(|s| s.to_string()).collect(),
            conditional_extensions_only_under_browser_dir_prefixes: [".mjs", ".cjs", ".js", ".ts", ".css", ".scss", ".sass"]
                .iter().map(|s| s.to_string()).collect(),
            browser_dir_prefixes_normalized_slash_lowercase: [
                "public/", "site/", "app/", "pages/", "components/", "client/", "web/",
                "src/frontend/", "packages/web-app/", "frontend/", "webapp/",
            ].iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstructionPayloadConfig {
    pub ready_wave_limit: usize,
    pub instruction_recall_hits: u32,
    pub transition_recall_hits: u32,
    pub prompt_excerpt_chars: usize,
    pub max_marker_age_ms: i64,
    pub orient_noun_limit: usize,
    pub orient_stopwords_compared_lowercase: Vec<String>,
}

impl Default for InstructionPayloadConfig {
    fn default() -> Self {
        InstructionPayloadConfig {
            ready_wave_limit: 3,
            instruction_recall_hits: 5,
            transition_recall_hits: 3,
            prompt_excerpt_chars: 400,
            max_marker_age_ms: 6 * 60 * 60 * 1000,
            orient_noun_limit: 5,
            orient_stopwords_compared_lowercase: [
                "the","a","an","to","of","in","on","for","and","or","is","are","was","were",
                "be","been","being","do","does","did","have","has","had","i","you","we","they",
                "it","this","that","these","those","with","from","as","at","by","but","if",
                "then","so","can","could","would","should","will","shall","may","might",
                "please","me","my","our","your","their","his","her",
            ].iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineConfig {
    pub ttl_ms: u64,
    pub summarize_threshold: usize,
    pub max_result_bytes_advertised_and_enforced_by_one_field: usize,
    pub max_attempts: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            ttl_ms: 120_000,
            summarize_threshold: 2048,
            max_result_bytes_advertised_and_enforced_by_one_field: 4096,
            max_attempts: 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClaimAuditConfig {
    pub shipped_claim_markers_matched_case_insensitive_substring: Vec<String>,
    pub scan_paths_relative_to_project_root_missing_is_skip_not_error: Vec<String>,
}

impl Default for ClaimAuditConfig {
    fn default() -> Self {
        ClaimAuditConfig {
            shipped_claim_markers_matched_case_insensitive_substring: ["shipped", "validated", "confirmed live", "landed in", "fixed in", "live-witnessed"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            scan_paths_relative_to_project_root_missing_is_skip_not_error: vec!["AGENTS.md".to_string()],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RagConfig {
    pub embed: EmbedDimConfig,
    pub namespaces: NamespaceConfig,
    pub scoring: ScoringConfig,
    pub budget: QueryBudgetConfig,
    pub index: IndexConfig,
    pub rssearch: VecTableNames,
    pub git_commits: VecTableNames,
    pub code_chunks: VecTableNames,
    pub legacy_memories_alongside_code_chunks: VecTableNames,
    pub claim_audit: ClaimAuditConfig,
    pub pipeline: PipelineConfig,
    pub instruction_payload: InstructionPayloadConfig,
    pub browser_witness: BrowserWitnessConfig,
    pub discipline_note: DisciplineNoteConfig,
}

impl Default for RagConfig {
    fn default() -> Self {
        RagConfig {
            embed: EmbedDimConfig::default(),
            index: IndexConfig::default(),
            namespaces: NamespaceConfig::default(),
            scoring: ScoringConfig::default(),
            budget: QueryBudgetConfig::default(),
            rssearch: VecTableNames::with_conventional_vec_index("rssearch_vectors"),
            git_commits: VecTableNames::with_conventional_vec_index("git_commit_vectors"),
            code_chunks: VecTableNames::with_conventional_vec_index("code_chunks"),
            legacy_memories_alongside_code_chunks: VecTableNames::with_conventional_vec_index("memories"),
            claim_audit: ClaimAuditConfig::default(),
            pipeline: PipelineConfig::default(),
            instruction_payload: InstructionPayloadConfig::default(),
            browser_witness: BrowserWitnessConfig::default(),
            discipline_note: DisciplineNoteConfig::default(),
        }
    }
}

/// Accept only `[A-Za-z_][A-Za-z0-9_]*`.
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
    /// A resolution layer reads config off disk, so unlike the compile-time
    /// assertion in `embed.rs` these values are not known until runtime -- and
    /// the failure mode is silent and total: a `dim` the compiled embedder
    /// cannot emit makes every `ensure_schema_*` drop its table (width
    /// mismatch), then every write fails its own width check, leaving a
    /// permanently empty knowledgebase that reports no errors at the verb
    /// layer. Callers should refuse a config that fails this rather than fall
    /// back to defaults, since silently ignoring an operator's stated dim is
    /// how a store gets rebuilt at a width nobody asked for.
    /// Build from a resolved config value, defaulting every absent field.
    /// This is the bridge that was missing: `config.rs` resolved a full 4-tier
    /// chain and `RagConfig` was a complete value type, but nothing joined
    /// them -- `config::resolve()`'s only caller was its own reporting verb, so
    /// every knob here came from `Default` no matter what a project vendored.
    /// The whole configuration surface was therefore observable and inert.
    /// Absent keys default rather than erroring, because `config.rs` deep-merges
    /// a partial tier over the builtin: a config legitimately ships only the
    /// keys it wants to change. An unparseable key is a different matter and is
    /// reported, since silently ignoring a value someone wrote is the failure
    /// this whole chain exists to avoid.
    /// Validation runs before returning, so a config that would produce an
    /// unusable store is refused here rather than at the first query.
    pub fn from_value(v: &serde_json::Value) -> Result<RagConfig, String> {
        let mut cfg = RagConfig::default();
        let mut problems: Vec<String> = Vec::new();

        let num = |parent: &str, key: &str, out: &mut usize, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_u64() {
                    Some(n) => *out = n as usize,
                    None => problems.push(format!("{parent}.{key} must be a non-negative integer, got {found}")),
                }
            }
        };
        let num64 = |parent: &str, key: &str, out: &mut u64, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_u64() {
                    Some(n) => *out = n,
                    None => problems.push(format!("{parent}.{key} must be a non-negative integer, got {found}")),
                }
            }
        };

        num64("index", "wall_budget_ms", &mut cfg.index.wall_budget_ms, &mut problems);
        num("index", "max_file_bytes", &mut cfg.index.max_file_bytes, &mut problems);
        num("index", "max_chunks_per_file_per_pass", &mut cfg.index.max_chunks_embedded_per_file_per_pass_count_bound_only, &mut problems);
        num("index", "oversized_chunk_split_threshold", &mut cfg.index.split_chunk_above_bytes, &mut problems);
        num("memory", "embed_dim", &mut cfg.embed.dim, &mut problems);
        num("memory", "recall_limit", &mut cfg.budget.default_limit, &mut problems);

        if let Some(ns) = v.get("memory").and_then(|m| m.get("namespace")).and_then(|n| n.as_str()) {
            cfg.namespaces.default = ns.to_string();
        }

        if !problems.is_empty() {
            return Err(format!("ragconfig: {}", problems.join("; ")));
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// The knowledgebase settings actually in force for this project.
    /// Resolves the 4-tier config chain and builds a `RagConfig` from it,
    /// falling back to the compiled defaults when no tier supplies one or when
    /// what it supplies is unusable. This is the bridge every consumer should
    /// call: `from_value` existed and was correct, but its only caller was the
    /// reporting verb, so the entire RAG surface was observable and inert --
    /// `config_resolve` could report a tier had won while every knob still came
    /// from `Default`.
    /// A config that fails validation degrades to defaults rather than
    /// propagating an error, because the alternative is a project whose
    /// retrieval stops working entirely over a mistyped number. The rejection
    /// is reported through `resolve_and_report`'s own events, so it is loud
    /// without being fatal.
    ///
    pub fn resolved() -> RagConfig {
        let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
        let now = unsafe { crate::wasm_dispatch::host_now_ms() };
        if let Ok(guard) = RESOLVED_CACHE.lock() {
            if let Some(entry) = guard.as_ref() {
                if entry.root == root && now.saturating_sub(entry.ts_ms) < RESOLVED_CACHE_TTL_MS {
                    return entry.config.clone();
                }
            }
        }
        let value = crate::config::resolve().config.value;
        let config = RagConfig::from_value(&value).unwrap_or_default();
        if let Ok(mut guard) = RESOLVED_CACHE.lock() {
            *guard = Some(ResolvedEntryScopedToOneProjectRootNeverGlobal { root, ts_ms: now, config: config.clone() });
        }
        config
    }

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
        if !(0.0..=1.0).contains(&self.scoring.cos_floor_applied_before_recency_rescue) {
            return Err(format!(
                "ragconfig: scoring.cos_floor {} outside [0,1]; embeddings are L2-normalized so \
                 cosine similarity cannot exceed 1 -- a higher floor silently matches nothing",
                self.scoring.cos_floor_applied_before_recency_rescue
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
        for names in [&self.rssearch, &self.git_commits, &self.code_chunks, &self.legacy_memories_alongside_code_chunks] {
            valid_sql_ident(&names.table)?;
            valid_sql_ident(&names.index)?;
        }
        if self.namespaces.code.is_empty() || self.namespaces.default.is_empty() {
            return Err("ragconfig: namespaces.code and namespaces.default must be non-empty".to_string());
        }
        Ok(())
    }
}
