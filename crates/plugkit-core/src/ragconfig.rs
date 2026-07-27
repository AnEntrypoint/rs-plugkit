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

/// Code-index chunking and pass budgets.
///
/// Every value here was a bare literal in `code_index.rs`, which made the
/// indexing cost profile un-tunable per project -- and these are exactly the
/// knobs that matter when a corpus does not resemble the one they were chosen
/// against. A prose-heavy tree wants a different chunk split than a code tree;
/// a slow machine wants a different wall budget than a fast one.
///
/// Per this module's own invariant, the defaults reproduce the historical
/// literals EXACTLY, so adopting the config is a no-op until a value is
/// deliberately edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexConfig {
    /// Split a chunk whose body exceeds this many bytes (was 8192).
    pub oversized_chunk_split_threshold: usize,
    /// Ceiling on chunks embedded per file per pass (was 64). A COUNT bound
    /// only -- the real protection is `wall_budget_ms`, because per-chunk
    /// embed cost is highly non-uniform: a 49-chunk prose file took 39981ms
    /// while sitting comfortably under this cap.
    pub max_chunks_per_file_per_pass: usize,
    /// Wall-clock budget for one indexing pass (was 30000ms). A pass that
    /// exceeds it defers the remaining files rather than running to
    /// completion, so one large tree cannot starve the supervisor heartbeat.
    pub wall_budget_ms: u64,
    /// Skip any file larger than this (was 256 * 1024). A file that size is
    /// nearly always generated or vendored, and embedding it costs far more
    /// than it returns.
    pub max_file_bytes: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            oversized_chunk_split_threshold: 8192,
            max_chunks_per_file_per_pass: 64,
            wall_budget_ms: 30_000,
            max_file_bytes: 256 * 1024,
        }
    }
}

/// Size caps on discipline notes.
///
/// The two length caps HARD-REFUSE a note that exceeds them, so they are not
/// merely cosmetic: a project whose policy names are longer than 64 chars
/// cannot record them at all. `active_policies_limit` truncates the list fed
/// into every instruction payload, so it trades context budget against how
/// many standing policies the agent can actually see.
///
/// Defaults reproduce the previous literals exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct DisciplineNoteConfig {
    /// Longest accepted note name (was 64). Exceeding it is refused, not truncated.
    pub max_name_len: usize,
    /// Longest accepted note body (was 200). Exceeding it is refused, not truncated.
    pub max_text_len: usize,
    /// Active policies surfaced in the instruction payload (was 50).
    pub active_policies_limit: usize,
}

impl Default for DisciplineNoteConfig {
    fn default() -> Self {
        DisciplineNoteConfig { max_name_len: 64, max_text_len: 200, active_policies_limit: 50 }
    }
}

/// Which edited files count as "runs in a browser", and therefore owe a
/// browser witness before COMPLETE.
///
/// This one is a real guarantee hole rather than an inconvenience. The
/// browser-witness-coverage gate only demands a witness for files this
/// classifier claims, so a project whose client lives in a directory outside
/// the hardcoded prefix list (`ui/`, `renderer/`, `apps/web/`, ...) had every
/// one of its client edits classified as non-browser -- and the gate then
/// passed with zero coverage, reporting a guarantee it had never checked.
/// Silent false-negatives in a gate are worse than a loud failure.
///
/// Defaults reproduce the previous literals exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserWitnessConfig {
    /// Extensions that are browser-running wherever they live, because the
    /// file type itself implies a rendered surface.
    pub always_browser_extensions: Vec<String>,
    /// Extensions that are browser-running ONLY under one of
    /// `browser_dir_prefixes` -- a `.js` file can equally be server code.
    pub conditional_extensions: Vec<String>,
    /// Path prefixes (normalised to `/`, matched lowercase) that mark a
    /// conditional extension as client-side.
    pub browser_dir_prefixes: Vec<String>,
}

impl Default for BrowserWitnessConfig {
    fn default() -> Self {
        BrowserWitnessConfig {
            always_browser_extensions: [".html", ".htm", ".tsx", ".jsx", ".vue", ".svelte"]
                .iter().map(|s| s.to_string()).collect(),
            conditional_extensions: [".mjs", ".cjs", ".js", ".ts", ".css", ".scss", ".sass"]
                .iter().map(|s| s.to_string()).collect(),
            browser_dir_prefixes: [
                "public/", "site/", "app/", "pages/", "components/", "client/", "web/",
                "src/frontend/", "packages/web-app/", "frontend/", "webapp/",
            ].iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// How much, and what, the instruction payload puts in front of the agent
/// every dispatch.
///
/// These shape what the agent actually SEES, which makes them the most
/// consequential numbers in the orchestrator and the least defensible as
/// literals. The stopword list in particular is English-only: a project
/// working in another language gets orient-noun extraction that filters
/// nothing, silently degrading recall quality rather than failing.
///
/// Defaults reproduce the previous literals exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct InstructionPayloadConfig {
    /// Ready-wave PRD rows surfaced per dispatch (was 3).
    pub ready_wave_limit: usize,
    /// Recall hits fetched for the instruction payload (was 5).
    /// `u32` to match `recall_hits`' own signature -- no cast at the call site.
    pub instruction_recall_hits: u32,
    /// Recall hits fetched on a transition (was 3).
    pub transition_recall_hits: u32,
    /// Prompt echo truncation, in chars (was 400).
    pub prompt_excerpt_chars: usize,
    /// Age past which a turn marker is treated as stale (was 6h).
    pub max_marker_age_ms: i64,
    /// Orient-noun keywords kept from a prompt (was 5).
    pub orient_noun_limit: usize,
    /// Words filtered out of orient-noun extraction. Lowercased on both
    /// sides at compare time, so casing here does not matter.
    pub orient_stopwords: Vec<String>,
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
            orient_stopwords: [
                "the","a","an","to","of","in","on","for","and","or","is","are","was","were",
                "be","been","being","do","does","did","have","has","had","i","you","we","they",
                "it","this","that","these","those","with","from","as","at","by","but","if",
                "then","so","can","could","would","should","will","shall","may","might",
                "please","me","my","our","your","their","his","her",
            ].iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Step-pipeline lifetime, size, and retry budgets.
///
/// These were four literals in pipeline.rs, two of them written twice --
/// `max_result_bytes` at the point it is ADVERTISED to the caller and again
/// at the point it is ENFORCED, and the attempt budget likewise. Two
/// independent literals meant to be one value is a latent divergence bug:
/// change one and the pipeline promises a limit it does not apply, or
/// applies one it never announced.
///
/// Defaults reproduce the previous literals exactly.
#[derive(Clone, Debug, PartialEq)]
pub struct PipelineConfig {
    /// How long a pipeline state row stays valid (was 120_000ms).
    pub ttl_ms: u64,
    /// Result payloads above this are summarised rather than returned whole
    /// (was 2048 bytes).
    pub summarize_threshold: usize,
    /// Hard ceiling on a single step's serialized result (was 4096 bytes).
    /// Advertised to the caller AND enforced on validation -- one field now,
    /// so the two cannot disagree.
    pub max_result_bytes: usize,
    /// Total attempts a step gets before it is failed out (was 2).
    pub max_attempts: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            ttl_ms: 120_000,
            summarize_threshold: 2048,
            max_result_bytes: 4096,
            max_attempts: 2,
        }
    }
}

/// What the claim audit looks for, and where.
///
/// Both halves were hardcoded: an English marker vocabulary and `AGENTS.md`
/// as the only scanned file. Neither generalises -- another project keeps its
/// running log under a different filename, and a project whose notes are
/// written in another language (or simply with a different house vocabulary,
/// e.g. "deployed"/"verified") gets a silently useless audit rather than an
/// obviously broken one.
///
/// Defaults reproduce the previous literals exactly, so an unset config is a
/// guaranteed no-op.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimAuditConfig {
    /// Phrases that mark a line as ASSERTING something shipped, and therefore
    /// as owing a commit hash. Matched case-insensitively as substrings.
    pub markers: Vec<String>,
    /// Files scanned for unbacked claims, relative to the project root.
    /// A path that does not exist is skipped, not an error -- a project
    /// without one of these files simply has nothing to audit there.
    pub scan_paths: Vec<String>,
}

impl Default for ClaimAuditConfig {
    fn default() -> Self {
        ClaimAuditConfig {
            markers: ["shipped", "validated", "confirmed live", "landed in", "fixed in", "live-witnessed"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            scan_paths: vec!["AGENTS.md".to_string()],
        }
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
    /// Chunking and pass budgets for the code indexer.
    pub index: IndexConfig,
    /// The namespace-keyed memory/RAG table (default `rssearch_vectors`).
    pub rssearch: VecTableNames,
    /// Commit-message/diff vectors (default `git_commit_vectors`).
    pub git_commits: VecTableNames,
    /// Tree-sitter code chunks (default `code_chunks`).
    pub code_chunks: VecTableNames,
    /// Legacy flat memory table kept alongside `code_chunks` in the project db.
    pub memories: VecTableNames,
    /// Claim-audit marker vocabulary and scanned files.
    pub claim_audit: ClaimAuditConfig,
    /// Step-pipeline lifetime, size, and retry budgets.
    pub pipeline: PipelineConfig,
    /// What the instruction payload puts in front of the agent each dispatch.
    pub instruction_payload: InstructionPayloadConfig,
    /// Which edited files owe a browser witness.
    pub browser_witness: BrowserWitnessConfig,
    /// Size caps on discipline notes.
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
            rssearch: VecTableNames::derived("rssearch_vectors"),
            git_commits: VecTableNames::derived("git_commit_vectors"),
            code_chunks: VecTableNames::derived("code_chunks"),
            memories: VecTableNames::derived("memories"),
            claim_audit: ClaimAuditConfig::default(),
            pipeline: PipelineConfig::default(),
            instruction_payload: InstructionPayloadConfig::default(),
            browser_witness: BrowserWitnessConfig::default(),
            discipline_note: DisciplineNoteConfig::default(),
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
    /// Build from a resolved config value, defaulting every absent field.
    ///
    /// This is the bridge that was missing: `config.rs` resolved a full 4-tier
    /// chain and `RagConfig` was a complete value type, but nothing joined
    /// them -- `config::resolve()`'s only caller was its own reporting verb, so
    /// every knob here came from `Default` no matter what a project vendored.
    /// The whole configuration surface was therefore observable and inert.
    ///
    /// Absent keys default rather than erroring, because `config.rs` deep-merges
    /// a partial tier over the builtin: a config legitimately ships only the
    /// keys it wants to change. An unparseable key is a different matter and is
    /// reported, since silently ignoring a value someone wrote is the failure
    /// this whole chain exists to avoid.
    ///
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
        num("index", "max_chunks_per_file_per_pass", &mut cfg.index.max_chunks_per_file_per_pass, &mut problems);
        num("index", "oversized_chunk_split_threshold", &mut cfg.index.oversized_chunk_split_threshold, &mut problems);
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
