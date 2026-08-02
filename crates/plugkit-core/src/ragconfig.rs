#![cfg(target_arch = "wasm32")]

use serde_json::json;
use std::sync::Mutex;

const RESOLVED_CACHE_TTL_MS: u64 = 5_000;
const HOST_DISPATCH_CALL_DEADLINE_MS: u64 = 40_000;
const WALL_BUDGET_TAIL_MARGIN_MS: u64 = 10_000;

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
    pub bm25_k1_term_frequency_saturation: f64,
    pub bm25_b_document_length_normalization: f64,
    pub fusion_rrf_k: f64,
    pub fusion_identifier_boost: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        ScoringConfig {
            half_life_ms: 30.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            recency_floor: 0.4,
            cos_floor_applied_before_recency_rescue: 0.0,
            dedup_jaccard_near_duplicate_threshold: 0.7,
            bm25_k1_term_frequency_saturation: 1.2,
            bm25_b_document_length_normalization: 0.75,
            fusion_rrf_k: rs_search::fusion::RRF_K,
            fusion_identifier_boost: rs_search::fusion::IDENTIFIER_BOOST,
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
    /// Discipline namespaces fanned out alongside the base on recall.
    ///
    /// Union-ed with `.gm/disciplines/enabled.txt` rather than replacing it:
    /// that file is the only per-project namespace configuration that predates
    /// this config, and a project relying on it must keep working untouched.
    pub discipline_fanout: Vec<String>,
    /// Extra namespaces the agent-facing `kv_put` verb may write.
    ///
    /// ADDITIVE ONLY -- the compiled allowlist is always permitted, and this
    /// extends it. A replacing list would let a project silently revoke
    /// `default`/`session`/`config`/`cache`/`user` and break callers that have
    /// no idea a config file exists.
    pub kv_put_extra: Vec<String>,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        NamespaceConfig {
            discipline_fanout: Vec::new(),
            kv_put_extra: Vec::new(),
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
    pub pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound: u64,
    pub wall_budget_ms: u64,
    pub max_file_bytes: usize,
    pub extra_skip_dirs_appended_to_builtins_never_replacing: Vec<String>,
    pub extra_skip_file_suffixes_appended_to_builtins_never_replacing: Vec<String>,
    pub force_include_path_substrings_overriding_every_skip: Vec<String>,
    /// Ceiling on files enumerated when deciding which chunk rows to prune.
    /// A repo larger than this silently keeps rows for the files past the cap,
    /// so a monorepo needs to raise it rather than discover stale rows later.
    pub prune_enumeration_file_cap: usize,
    /// Ceiling on files walked to compute the corpus digest. Two repos that
    /// differ only past this cap produce the same digest, so an index can
    /// believe it is converged when it is not.
    pub digest_max_files: usize,
    /// Bounds the caller-supplied file limit for a prune pass. The floor stops
    /// a tiny limit from making a pass pointless; the ceiling stops one pass
    /// from walking an unbounded tree.
    pub prune_pass_file_limit_floor: usize,
    pub prune_pass_file_limit_ceiling: usize,
    pub likely_orphaned_symbol_scan_enabled: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            split_chunk_above_bytes: 8192,
            max_chunks_embedded_per_file_per_pass_count_bound_only: 64,
            // Real measurement (2026-07-30, .watcher.log code_index_slow_file_embed
            // events against the bert plugin's wasm-hosted BERT forward pass):
            // 501703ms/33 chunks, 138049ms/13, 126331ms/21, 97884ms/10, 21753ms/6
            // -- 3626ms to 15203ms per chunk, 15-30x the previous 800ms guess. The
            // old value let budget_chunks (code_index.rs's per-file cap derived
            // from remaining_ms / this constant) admit far more chunks than the
            // real wall-budget could ever finish, so a single slow file could blow
            // past both this module's own wall_budget_ms AND the wasmtime dispatch
            // epoch deadline before the next per-file check point could catch it --
            // the actual cause of the poisoned-Store crashes this constant's
            // mis-calibration produced. Rounded up from the worst observed
            // per-chunk cost, not the average, since this bound exists specifically
            // to keep a single file from starving the whole pass.
            pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound: 16_000,
            wall_budget_ms: HOST_DISPATCH_CALL_DEADLINE_MS - WALL_BUDGET_TAIL_MARGIN_MS,
            max_file_bytes: 256 * 1024,
            extra_skip_dirs_appended_to_builtins_never_replacing: Vec::new(),
            extra_skip_file_suffixes_appended_to_builtins_never_replacing: Vec::new(),
            force_include_path_substrings_overriding_every_skip: Vec::new(),
            prune_enumeration_file_cap: 20000,
            digest_max_files: 2000,
            prune_pass_file_limit_floor: 50,
            prune_pass_file_limit_ceiling: 2000,
            likely_orphaned_symbol_scan_enabled: false,
        }
    }
}

impl IndexConfig {
    pub fn is_force_included(&self, path: &str) -> bool {
        self.force_include_path_substrings_overriding_every_skip
            .iter()
            .any(|needle| !needle.is_empty() && path.contains(needle.as_str()))
    }

    pub fn skips_dir_segment(&self, seg: &str, builtins: &[&str]) -> bool {
        builtins.iter().any(|d| seg == *d)
            || self.extra_skip_dirs_appended_to_builtins_never_replacing.iter().any(|d| seg == d.as_str())
    }

    pub fn skips_filename(&self, name: &str, builtins: &[&str]) -> bool {
        builtins.iter().any(|suf| name.ends_with(suf))
            || self.extra_skip_file_suffixes_appended_to_builtins_never_replacing
                .iter()
                .any(|suf| !suf.is_empty() && name.ends_with(suf.as_str()))
    }
}

/// Where the store lives. The filename and the state-root directory were
/// hardcoded in two DIFFERENT modules (code_index for the filename, libsql_wasm
/// for the .gm segment), so relocating a store meant editing both and knowing
/// they existed. A project that wants its gm state somewhere other than .gm --
/// or two stores side by side -- has no way to say so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbPathConfig {
    pub state_root_dir: String,
    pub db_filename: String,
}

impl Default for DbPathConfig {
    fn default() -> Self {
        DbPathConfig { state_root_dir: ".gm".to_string(), db_filename: "gm.db".to_string() }
    }
}

/// The two bookkeeping tables backing markdown-memory sync. They sit in the
/// same database as the configurable vector tables but were hardcoded, so a
/// project that renamed its vector tables to coexist with another store still
/// collided on these two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryMdTableNames {
    pub meta: String,
    pub files: String,
}

impl Default for MemoryMdTableNames {
    fn default() -> Self {
        MemoryMdTableNames {
            meta: "memories_md_meta".to_string(),
            files: "memories_md_files".to_string(),
        }
    }
}

/// Wall budgets for the two background passes that embed in bulk: migrating a
/// namespace off the legacy flat-JSON store, and vectorizing git commits.
/// Both stop early when the budget runs out and resume on a later call, so a
/// budget too small for a large repo means the pass never finishes -- and,
/// like the memory sync, never signals that it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkEmbedBudgetConfig {
    pub flat_json_migration_budget_ms: u64,
    pub git_commit_embed_budget_ms: u64,
}

impl Default for BulkEmbedBudgetConfig {
    fn default() -> Self {
        BulkEmbedBudgetConfig {
            flat_json_migration_budget_ms: 2000,
            git_commit_embed_budget_ms: 30000,
        }
    }
}

/// Bounds on the in-process embedding caches. Capacity and TTL decide how
/// much repeat work is avoided between Store evictions; the cacheability
/// threshold decides which passages are worth remembering at all. A large
/// corpus and a small cap interact badly -- entries evict before they are
/// reused -- and that tradeoff is workload-specific, so it belongs in config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedCacheConfig {
    pub query_cache_capacity: usize,
    pub query_cache_ttl_ms: i64,
    pub plain_cache_max_text_bytes: usize,
}

impl Default for EmbedCacheConfig {
    fn default() -> Self {
        EmbedCacheConfig {
            query_cache_capacity: 64,
            query_cache_ttl_ms: 600_000,
            plain_cache_max_text_bytes: 4096,
        }
    }
}

/// Budgets bounding a memory_md sync pass. These were compiled-in constants,
/// which made a slow store unfixable from config: a sync that cannot finish
/// inside its budget records a `:partial` digest, and a `:partial` digest can
/// never equal a freshly computed one, so the pass re-runs forever and never
/// converges. That exact loop caused this session's recall stall. Raising the
/// budget is the operator-side fix, so it has to be reachable.
/// Thresholds for reclaiming space from ALREADY-tombstoned rows.
///
/// Reclaim-only by construction: nothing here ever tombstones a live row. The
/// prune surface is agent-judged and never auto-similarity-deleted, so deciding
/// a memory is unwanted stays a human/agent call; this only decides when the
/// space behind rows already judged unwanted gets returned.
///
/// `auto_vacuum_enabled` defaults false so an unconfigured store behaves
/// exactly as before. The report is available regardless, which is the intended
/// order of adoption: look at what a policy would reclaim before enabling one.
#[derive(Clone, Debug, PartialEq)]
pub struct RetentionConfig {
    pub auto_vacuum_enabled: bool,
    pub tombstone_ratio_threshold: f64,
    pub tombstone_count_threshold: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        RetentionConfig {
            auto_vacuum_enabled: false,
            tombstone_ratio_threshold: 0.25,
            tombstone_count_threshold: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemorySyncBudgetConfig {
    pub embed_budget_ms: u64,
    pub total_budget_ms: u64,
    pub rekey_rows_deadline_ms: u64,
    pub shadow_abort_threshold: u32,
    pub rekey_batch_max: usize,
    pub rename_batch_chunk: usize,
}

impl Default for MemorySyncBudgetConfig {
    fn default() -> Self {
        MemorySyncBudgetConfig {
            embed_budget_ms: 1500,
            total_budget_ms: 2000,
            rekey_rows_deadline_ms: 1800,
            shadow_abort_threshold: 5,
            rekey_batch_max: 25,
            rename_batch_chunk: 60,
        }
    }
}

impl MemorySyncBudgetConfig {
    pub fn rekey_rows_deadline_within_total(&self) -> u64 {
        self.rekey_rows_deadline_ms.min(self.total_budget_ms)
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
    /// The summarize step's whole policy. The threshold decided WHETHER to
    /// summarize while the prompt, the size targets and the result schema --
    /// what the summary actually is -- stayed compiled in, so a project could
    /// change when summarization happens but not what it produces.
    pub summarize_prompt_template: String,
    pub summarize_target_chars: usize,
    pub summarize_max_summary_chars: usize,
    pub summarize_input_char_cap: usize,
    pub summarize_preserve: Vec<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            ttl_ms: 120_000,
            summarize_threshold: 2048,
            max_result_bytes_advertised_and_enforced_by_one_field: 4096,
            summarize_prompt_template: "Summarize the following text into <={{target_chars}} chars, preserving entities and any numeric facts. Return JSON {\"summary\": string}. Input:\n{{input}}".to_string(),
            summarize_target_chars: 400,
            summarize_max_summary_chars: 800,
            summarize_input_char_cap: 8192,
            summarize_preserve: vec!["entities".to_string(), "numbers".to_string(), "ids".to_string()],
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
    pub memory_sync: MemorySyncBudgetConfig,
    pub embed_cache: EmbedCacheConfig,
    pub bulk_embed: BulkEmbedBudgetConfig,
    pub memory_md_tables: MemoryMdTableNames,
    pub db_path: DbPathConfig,
    pub retention: RetentionConfig,
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
            retention: RetentionConfig::default(),
            memory_sync: MemorySyncBudgetConfig::default(),
            embed_cache: EmbedCacheConfig::default(),
            bulk_embed: BulkEmbedBudgetConfig::default(),
            memory_md_tables: MemoryMdTableNames::default(),
            db_path: DbPathConfig::default(),
        }
    }
}

fn reject_unless_ascii_alnum_underscore_sql_ident(name: &str) -> Result<(), String> {
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
    pub fn dim(&self) -> usize {
        self.embed.dim
    }

    pub fn from_value(v: &serde_json::Value) -> Result<RagConfig, String> {
        let mut cfg = RagConfig::default();
        let mut problems: Vec<String> = Vec::new();

        let overwrite_present_usize_or_record_problem = |parent: &str, key: &str, out: &mut usize, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_u64() {
                    Some(n) => *out = n as usize,
                    None => problems.push(format!("{parent}.{key} must be a non-negative integer, got {found}")),
                }
            }
        };
        let overwrite_present_u64_or_record_problem = |parent: &str, key: &str, out: &mut u64, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_u64() {
                    Some(n) => *out = n,
                    None => problems.push(format!("{parent}.{key} must be a non-negative integer, got {found}")),
                }
            }
        };

        let overwrite_present_f64_or_record_problem = |parent: &str, key: &str, out: &mut f64, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_f64() {
                    Some(n) => *out = n,
                    None => problems.push(format!("{parent}.{key} must be a number, got {found}")),
                }
            }
        };
        let overwrite_present_bool_or_record_problem = |parent: &str, key: &str, out: &mut bool, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_bool() {
                    Some(b) => *out = b,
                    None => problems.push(format!("{parent}.{key} must be a boolean, got {found}")),
                }
            }
        };
        let overwrite_present_i64_or_record_problem = |parent: &str, key: &str, out: &mut i64, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_i64() {
                    Some(n) => *out = n,
                    None => problems.push(format!("{parent}.{key} must be an integer, got {found}")),
                }
            }
        };
        let overwrite_present_string_or_record_problem = |parent: &str, key: &str, out: &mut String, problems: &mut Vec<String>| {
            if let Some(found) = v.get(parent).and_then(|p| p.get(key)) {
                match found.as_str() {
                    Some(s) => *out = s.to_string(),
                    None => problems.push(format!("{parent}.{key} must be a string, got {found}")),
                }
            }
        };
        let append_present_strings_or_record_problem = |parent: &str, key: &str, out: &mut Vec<String>, problems: &mut Vec<String>| {
            let found = match v.get(parent).and_then(|p| p.get(key)) {
                Some(f) => f,
                None => return,
            };
            match found.as_array() {
                Some(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(s) => out.push(s.to_string()),
                            None => problems.push(format!("{parent}.{key} entries must be strings, got {item}")),
                        }
                    }
                }
                None => problems.push(format!("{parent}.{key} must be an array of strings, got {found}")),
            }
        };

        overwrite_present_u64_or_record_problem("index", "wall_budget_ms", &mut cfg.index.wall_budget_ms, &mut problems);
        overwrite_present_usize_or_record_problem("index", "max_file_bytes", &mut cfg.index.max_file_bytes, &mut problems);
        overwrite_present_usize_or_record_problem("index", "max_chunks_per_file_per_pass", &mut cfg.index.max_chunks_embedded_per_file_per_pass_count_bound_only, &mut problems);
        overwrite_present_u64_or_record_problem("index", "pessimistic_ms_per_chunk", &mut cfg.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound, &mut problems);
        overwrite_present_usize_or_record_problem("index", "oversized_chunk_split_threshold", &mut cfg.index.split_chunk_above_bytes, &mut problems);
        append_present_strings_or_record_problem("index", "extra_skip_dirs", &mut cfg.index.extra_skip_dirs_appended_to_builtins_never_replacing, &mut problems);
        append_present_strings_or_record_problem("index", "extra_skip_file_suffixes", &mut cfg.index.extra_skip_file_suffixes_appended_to_builtins_never_replacing, &mut problems);
        append_present_strings_or_record_problem("index", "force_include_path_substrings", &mut cfg.index.force_include_path_substrings_overriding_every_skip, &mut problems);
        overwrite_present_usize_or_record_problem("index", "prune_enumeration_file_cap", &mut cfg.index.prune_enumeration_file_cap, &mut problems);
        overwrite_present_usize_or_record_problem("index", "digest_max_files", &mut cfg.index.digest_max_files, &mut problems);
        overwrite_present_usize_or_record_problem("index", "prune_pass_file_limit_floor", &mut cfg.index.prune_pass_file_limit_floor, &mut problems);
        overwrite_present_usize_or_record_problem("index", "prune_pass_file_limit_ceiling", &mut cfg.index.prune_pass_file_limit_ceiling, &mut problems);
        overwrite_present_bool_or_record_problem("index", "likely_orphaned_symbol_scan_enabled", &mut cfg.index.likely_orphaned_symbol_scan_enabled, &mut problems);
        overwrite_present_bool_or_record_problem("retention", "auto_vacuum_enabled", &mut cfg.retention.auto_vacuum_enabled, &mut problems);
        overwrite_present_f64_or_record_problem("retention", "tombstone_ratio_threshold", &mut cfg.retention.tombstone_ratio_threshold, &mut problems);
        overwrite_present_u64_or_record_problem("retention", "tombstone_count_threshold", &mut cfg.retention.tombstone_count_threshold, &mut problems);
        overwrite_present_u64_or_record_problem("memory_sync", "embed_budget_ms", &mut cfg.memory_sync.embed_budget_ms, &mut problems);
        overwrite_present_u64_or_record_problem("memory_sync", "total_budget_ms", &mut cfg.memory_sync.total_budget_ms, &mut problems);
        overwrite_present_u64_or_record_problem("memory_sync", "rekey_rows_deadline_ms", &mut cfg.memory_sync.rekey_rows_deadline_ms, &mut problems);
        overwrite_present_usize_or_record_problem("memory_sync", "rekey_batch_max", &mut cfg.memory_sync.rekey_batch_max, &mut problems);
        overwrite_present_usize_or_record_problem("memory_sync", "rename_batch_chunk", &mut cfg.memory_sync.rename_batch_chunk, &mut problems);
        overwrite_present_usize_or_record_problem("embed_cache", "query_cache_capacity", &mut cfg.embed_cache.query_cache_capacity, &mut problems);
        overwrite_present_usize_or_record_problem("embed_cache", "plain_cache_max_text_bytes", &mut cfg.embed_cache.plain_cache_max_text_bytes, &mut problems);
        overwrite_present_u64_or_record_problem("bulk_embed", "flat_json_migration_budget_ms", &mut cfg.bulk_embed.flat_json_migration_budget_ms, &mut problems);
        overwrite_present_u64_or_record_problem("bulk_embed", "git_commit_embed_budget_ms", &mut cfg.bulk_embed.git_commit_embed_budget_ms, &mut problems);
        overwrite_present_string_or_record_problem("memory_md_tables", "meta", &mut cfg.memory_md_tables.meta, &mut problems);
        overwrite_present_string_or_record_problem("memory_md_tables", "files", &mut cfg.memory_md_tables.files, &mut problems);
        overwrite_present_string_or_record_problem("db_path", "state_root_dir", &mut cfg.db_path.state_root_dir, &mut problems);
        overwrite_present_string_or_record_problem("db_path", "db_filename", &mut cfg.db_path.db_filename, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "bm25_k1", &mut cfg.scoring.bm25_k1_term_frequency_saturation, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "bm25_b", &mut cfg.scoring.bm25_b_document_length_normalization, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "fusion_rrf_k", &mut cfg.scoring.fusion_rrf_k, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "fusion_identifier_boost", &mut cfg.scoring.fusion_identifier_boost, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "recency_floor", &mut cfg.scoring.recency_floor, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "cos_floor", &mut cfg.scoring.cos_floor_applied_before_recency_rescue, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "dedup_jaccard_threshold", &mut cfg.scoring.dedup_jaccard_near_duplicate_threshold, &mut problems);
        overwrite_present_f64_or_record_problem("scoring", "half_life_ms", &mut cfg.scoring.half_life_ms, &mut problems);
        overwrite_present_usize_or_record_problem("memory", "embed_dim", &mut cfg.embed.dim, &mut problems);
        overwrite_present_usize_or_record_problem("memory", "recall_limit", &mut cfg.budget.default_limit, &mut problems);
        overwrite_present_usize_or_record_problem("memory", "pool_multiplier", &mut cfg.budget.pool_multiplier, &mut problems);
        overwrite_present_usize_or_record_problem("memory", "pool_floor", &mut cfg.budget.pool_floor, &mut problems);
        overwrite_present_usize_or_record_problem("memory", "default_k", &mut cfg.budget.default_k, &mut problems);
        overwrite_present_u64_or_record_problem("pipeline", "ttl_ms", &mut cfg.pipeline.ttl_ms, &mut problems);
        overwrite_present_usize_or_record_problem("pipeline", "summarize_threshold", &mut cfg.pipeline.summarize_threshold, &mut problems);
        overwrite_present_usize_or_record_problem("pipeline", "max_result_bytes", &mut cfg.pipeline.max_result_bytes_advertised_and_enforced_by_one_field, &mut problems);
        overwrite_present_string_or_record_problem("pipeline", "summarize_prompt_template", &mut cfg.pipeline.summarize_prompt_template, &mut problems);
        overwrite_present_usize_or_record_problem("pipeline", "summarize_target_chars", &mut cfg.pipeline.summarize_target_chars, &mut problems);
        overwrite_present_usize_or_record_problem("pipeline", "summarize_max_summary_chars", &mut cfg.pipeline.summarize_max_summary_chars, &mut problems);
        overwrite_present_usize_or_record_problem("pipeline", "summarize_input_char_cap", &mut cfg.pipeline.summarize_input_char_cap, &mut problems);
        overwrite_present_u64_or_record_problem("pipeline", "max_attempts", &mut cfg.pipeline.max_attempts, &mut problems);

        overwrite_present_usize_or_record_problem("instruction_payload", "ready_wave_limit", &mut cfg.instruction_payload.ready_wave_limit, &mut problems);
        if let Some(found) = v.get("instruction_payload").and_then(|p| p.get("instruction_recall_hits")) {
            match found.as_u64() {
                Some(n) => cfg.instruction_payload.instruction_recall_hits = n as u32,
                None => problems.push(format!("instruction_payload.instruction_recall_hits must be a non-negative integer, got {found}")),
            }
        }
        if let Some(found) = v.get("instruction_payload").and_then(|p| p.get("transition_recall_hits")) {
            match found.as_u64() {
                Some(n) => cfg.instruction_payload.transition_recall_hits = n as u32,
                None => problems.push(format!("instruction_payload.transition_recall_hits must be a non-negative integer, got {found}")),
            }
        }
        overwrite_present_usize_or_record_problem("instruction_payload", "prompt_excerpt_chars", &mut cfg.instruction_payload.prompt_excerpt_chars, &mut problems);
        overwrite_present_i64_or_record_problem("instruction_payload", "max_marker_age_ms", &mut cfg.instruction_payload.max_marker_age_ms, &mut problems);
        overwrite_present_usize_or_record_problem("instruction_payload", "orient_noun_limit", &mut cfg.instruction_payload.orient_noun_limit, &mut problems);
        append_present_strings_or_record_problem("instruction_payload", "extra_orient_stopwords", &mut cfg.instruction_payload.orient_stopwords_compared_lowercase, &mut problems);

        append_present_strings_or_record_problem("browser_witness", "extra_always_browser_extensions", &mut cfg.browser_witness.always_browser_extensions_regardless_of_directory, &mut problems);
        append_present_strings_or_record_problem("browser_witness", "extra_conditional_extensions", &mut cfg.browser_witness.conditional_extensions_only_under_browser_dir_prefixes, &mut problems);
        append_present_strings_or_record_problem("browser_witness", "extra_dir_prefixes", &mut cfg.browser_witness.browser_dir_prefixes_normalized_slash_lowercase, &mut problems);

        overwrite_present_usize_or_record_problem("discipline_note", "max_name_len", &mut cfg.discipline_note.max_name_len_hard_refuse_not_truncate, &mut problems);
        overwrite_present_usize_or_record_problem("discipline_note", "max_text_len", &mut cfg.discipline_note.max_text_len_hard_refuse_not_truncate, &mut problems);
        overwrite_present_usize_or_record_problem("discipline_note", "active_policies_instruction_limit", &mut cfg.discipline_note.active_policies_surfaced_in_instruction_payload_limit, &mut problems);

        append_present_strings_or_record_problem("claim_audit", "extra_shipped_claim_markers", &mut cfg.claim_audit.shipped_claim_markers_matched_case_insensitive_substring, &mut problems);
        append_present_strings_or_record_problem("claim_audit", "extra_scan_paths", &mut cfg.claim_audit.scan_paths_relative_to_project_root_missing_is_skip_not_error, &mut problems);

        for (parent, names) in [
            ("rssearch", &mut cfg.rssearch),
            ("git_commits", &mut cfg.git_commits),
            ("code_chunks", &mut cfg.code_chunks),
        ] {
            overwrite_present_string_or_record_problem(parent, "table", &mut names.table, &mut problems);
            overwrite_present_string_or_record_problem(parent, "index", &mut names.index, &mut problems);
        }

        if let Some(ns) = v.get("memory").and_then(|m| m.get("namespace")).and_then(|n| n.as_str()) {
            cfg.namespaces.default = ns.to_string();
        }
        if let Some(d) = v.get("memory").and_then(|m| m.get("discipline_namespaces")).and_then(|x| x.as_array()) {
            cfg.namespaces.discipline_fanout = d
                .iter()
                .filter_map(|e| e.as_str())
                .filter(|e| !e.trim().is_empty())
                .map(|e| e.trim().to_string())
                .collect();
        }
        if let Some(extra) = v.get("memory").and_then(|m| m.get("kv_put_extra_namespaces")).and_then(|x| x.as_array()) {
            cfg.namespaces.kv_put_extra = extra
                .iter()
                .filter_map(|e| e.as_str())
                .filter(|e| !e.trim().is_empty())
                .map(|e| e.trim().to_string())
                .collect();
        }

        if !problems.is_empty() {
            return Err(format!("ragconfig: {}", problems.join("; ")));
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn resolved() -> RagConfig {
        let project_root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
        let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
        if let Ok(cache) = RESOLVED_CACHE.lock() {
            if let Some(fresh_entry) = cache.as_ref() {
                if fresh_entry.root == project_root && now_ms.saturating_sub(fresh_entry.ts_ms) < RESOLVED_CACHE_TTL_MS {
                    return fresh_entry.config.clone();
                }
            }
        }
        let tiered_config_value = crate::config::resolve().config.value;
        // A validation failure discards EVERY setting in the file, not just the
        // offending one. Silently swallowing it meant an operator saw their
        // whole config ignored with no indication which value caused it --
        // every explanatory message validate() composes was written into a
        // Result that was then dropped.
        let resolved_config_or_defaults_on_validation_failure = match RagConfig::from_value(&tiered_config_value) {
            Ok(cfg) => cfg,
            Err(reason) => {
                crate::wasm_dispatch::emit_event("ragconfig_validation_failed", serde_json::json!({
                    "reason": reason,
                    "effect": "the ENTIRE resolved config is being ignored and compiled defaults are in use -- not just the offending key",
                }));
                RagConfig::default()
            }
        };
        if let Ok(mut cache) = RESOLVED_CACHE.lock() {
            *cache = Some(ResolvedEntryScopedToOneProjectRootNeverGlobal {
                root: project_root,
                ts_ms: now_ms,
                config: resolved_config_or_defaults_on_validation_failure.clone(),
            });
        }
        resolved_config_or_defaults_on_validation_failure
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
        if self.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound == 0 {
            return Err("ragconfig: index.pessimistic_ms_per_chunk must be non-zero; it divides the remaining wall budget to derive a per-file chunk allowance".to_string());
        }
        if self.index.max_chunks_embedded_per_file_per_pass_count_bound_only == 0 {
            return Err("ragconfig: index.max_chunks_per_file_per_pass must be non-zero; a zero cap truncates every file to no chunks and the index never populates".to_string());
        }
        if !(self.scoring.bm25_k1_term_frequency_saturation >= 0.0) {
            return Err(format!(
                "ragconfig: scoring.bm25_k1 {} must be finite and non-negative; it is the term-frequency saturation parameter and a negative value inverts the ranking",
                self.scoring.bm25_k1_term_frequency_saturation
            ));
        }
        if !(self.scoring.fusion_rrf_k > 0.0) {
            return Err(format!(
                "ragconfig: scoring.fusion_rrf_k {} must be positive; it is a denominator added to each rank position and a non-positive value can divide by zero or invert ranking",
                self.scoring.fusion_rrf_k
            ));
        }
        if !(self.scoring.fusion_identifier_boost >= 0.0) {
            return Err(format!(
                "ragconfig: scoring.fusion_identifier_boost {} must be non-negative; it multiplies the BM25 list's RRF contribution for identifier-shaped queries and a negative value would penalize BM25 matches instead of boosting them",
                self.scoring.fusion_identifier_boost
            ));
        }
        if !(0.0..=1.0).contains(&self.scoring.bm25_b_document_length_normalization) {
            return Err(format!(
                "ragconfig: scoring.bm25_b {} outside [0,1]; it interpolates between no length normalization (0) and full (1), and outside that range the denominator can go negative and flip score signs",
                self.scoring.bm25_b_document_length_normalization
            ));
        }
        if self.budget.pool_multiplier == 0 {
            return Err("ragconfig: budget.pool_multiplier must be non-zero; a zero pool retrieves nothing".to_string());
        }
        for names in [&self.rssearch, &self.git_commits, &self.code_chunks, &self.legacy_memories_alongside_code_chunks] {
            reject_unless_ascii_alnum_underscore_sql_ident(&names.table)?;
            reject_unless_ascii_alnum_underscore_sql_ident(&names.index)?;
        }
        if self.namespaces.code.is_empty() || self.namespaces.default.is_empty() {
            return Err("ragconfig: namespaces.code and namespaces.default must be non-empty".to_string());
        }
        Ok(())
    }
}
