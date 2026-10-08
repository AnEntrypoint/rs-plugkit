use super::*;

pub(super) const UNINDEXED_CANDIDATE_MAX: usize = 500;

/// BM25 and the vector channel both lose the phrase: BM25 scores each query term independently
/// and the embedder scores meaning, not wording. A verbatim match is therefore found by an
/// exhaustive scan of the tree, run here within its own wall budget so a ranked query still
/// answers in seconds. 6s covers a full scan of a tree the size of litebox-main (measured 2.1s).
pub(super) const DUAL_PHRASE_SCAN_BUDGET_MS: u64 = 6_000;
pub(super) const DUAL_PHRASE_SCAN_MAX_MATCHES: usize = 20;

/// Matches the phrase scan may collect from one file before it moves on to the next. A scan that
/// stops at its first `max_matches` hits answers with whatever the walk reached first, and the walk
/// is ordered by path: on litebox-main a query for `cross_process_fork_slot` -- 43 matching lines --
/// spent all 20 slots on `AGENTS.md`, `docs/*` and `litebox_shim_linux/src/lib.rs`, so
/// `litebox_shim_linux/src/syscalls/process.rs`, which holds 19 of those 43 lines, answered with
/// nothing at all and read to the caller as "this symbol does not exist". The phrase channel is the
/// one channel that answers "which files contain this string verbatim", so a match-dense file must
/// not be able to take another file's only slot.
pub(super) const DUAL_PHRASE_SCAN_PER_FILE_MATCHES: usize = 8;

/// Room to collect the per-file quota from each of the first `DUAL_PHRASE_SCAN_MAX_MATCHES`
/// matching files. Only a phrase common enough to fill the old cap before the walk ends pays for
/// the wider collection, and even then the scan early-exits at this cap and is wall-bounded by
/// `DUAL_PHRASE_SCAN_BUDGET_MS`; a full scan of a tree the size of litebox-main measures under a
/// second. A phrase rarer than the cap costs exactly what it cost before.
pub(super) const DUAL_PHRASE_SCAN_COLLECT_MATCHES: usize =
    DUAL_PHRASE_SCAN_MAX_MATCHES * DUAL_PHRASE_SCAN_PER_FILE_MATCHES;

pub(super) fn dual_phrase_hits(query: &str, root: Option<&str>, cfg: &crate::ragconfig::RagConfig, no_ignore: bool) -> (Vec<Value>, u64, bool) {
    if rs_search::tokenize::tokenize(query).len() < 2 { return (Vec::new(), 0, false); }
    let scan = crate::code_index::LiteralScan {
        pattern: query,
        root,
        paths: &[],
        regex: false,
        case_insensitive: true,
        whole_word: false,
        comments_only: false,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        max_matches: DUAL_PHRASE_SCAN_COLLECT_MATCHES,
        max_matches_per_file: Some(DUAL_PHRASE_SCAN_PER_FILE_MATCHES),
        max_files: crate::code_index::LITERAL_SCAN_MAX_FILES,
        context: 0,
        term_combination: Some("phrase"),
        budget_ms: Some(DUAL_PHRASE_SCAN_BUDGET_MS),
        refresh: false,
        no_ignore,
        output: crate::code_index::ScanOutput::Matches,
        list_limit: None,
        max_chars: usize::MAX,
        spill_name: String::new(),
        verbose: true,
    };
    let out = crate::code_index::scan_literal(&scan, cfg);
    if out.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return (Vec::new(), 0, false);
    }
    // `phrase_match_count` is the lines the phrase itself matched, so it stays honest when the
    // returned rows are capped: `match_count` is the length of the returned array and would report
    // a capped scan as complete. `exhaustive` is the scan's own verdict and is false when the
    // budget, a size ceiling or a match cap cut the walk short.
    let total = out
        .get("phrase_match_count")
        .or_else(|| out.get("match_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let exhaustive = out
        .get("exhaustive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let hits = out
        .get("matches")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    (
        fair_share_phrase_hits(hits, DUAL_PHRASE_SCAN_MAX_MATCHES),
        total,
        exhaustive,
    )
}

/// Take one hit from every path, then a second from every path that still has one, and so on until
/// the cap is filled. Walk order still decides which paths are seen first but no longer decides
/// which of them are represented: a file matching once is not crowded out by a file matching fifty
/// times. The selection is re-sorted by path then line so the answer reads grouped per file.
pub(super) fn fair_share_phrase_hits(hits: Vec<Value>, cap: usize) -> Vec<Value> {
    if hits.len() <= cap {
        return hits;
    }
    let keep = crate::code_index::fair_share_indices(&hits, cap, |h| {
        h.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    });
    let mut out: Vec<Value> = keep.into_iter().map(|i| hits[i].clone()).collect();
    out.sort_by(|a, b| {
        let (pa, pb) = (
            a.get("path").and_then(|x| x.as_str()).unwrap_or(""),
            b.get("path").and_then(|x| x.as_str()).unwrap_or(""),
        );
        let (la, lb) = (
            a.get("line").and_then(|x| x.as_u64()).unwrap_or(0),
            b.get("line").and_then(|x| x.as_u64()).unwrap_or(0),
        );
        pa.cmp(pb).then(la.cmp(&lb))
    });
    out
}

/// First dispatch on a tree with no digest at all. Bounded, not the full 110s index budget: a
/// search that answers in half a minute with a full index beats one that times out at 120s. Measured
/// on litebox-main (1834 files) a 30s pass indexes the whole tree, so every dispatch after it needs
/// no index pass at all.
pub(super) const COLD_INDEX_PASS_BUDGET_MS: u64 = 30_000;

pub(super) fn unindexed_on_disk_candidates(namespace: &str) -> (Vec<Value>, usize) {
    let indexed = crate::rssearch_vectors::live_keys(namespace);
    let mut paths: Vec<(String, String)> = crate::memory_md::on_disk_memo_paths(namespace)
        .into_iter()
        .filter(|(key, _)| !indexed.iter().any(|k| k == key))
        .collect();
    let total = paths.len();
    paths.truncate(UNINDEXED_CANDIDATE_MAX);
    let mut out = Vec::new();
    for (key, path) in paths {
        let Some(content) = host_read(&path) else {
            continue;
        };
        let text = crate::memory_md::parse(&content)
            .map(|doc| doc.text)
            .unwrap_or_else(|| content.trim().to_string());
        out.push(json!({
            "key": key,
            "namespace": namespace,
            "text": text,
            "indexed": false,
            "source": "on_disk_unindexed",
        }));
    }
    (out, total)
}

pub(super) const COMPACT_DUAL_CANDIDATE_MULTIPLIER: usize = 4;

pub(super) fn with_corpus_symbol(corpus: &crate::code_index::FusionCorpus, row: &Value) -> Value {
    let mut row = row.clone();
    if row.get("symbol").is_none() {
        let symbol = row
            .get("key")
            .and_then(|k| k.as_str())
            .and_then(|k| corpus.symbol_for_key(k));
        if let (Some(symbol), Some(obj)) = (symbol, row.as_object_mut()) {
            obj.insert("symbol".to_string(), symbol);
        }
    }
    row
}

pub(super) fn dual_channel_depth(body: &Value, k: u32) -> usize {
    if body
        .get("verbose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        k as usize
    } else {
        (k as usize).saturating_mul(COMPACT_DUAL_CANDIDATE_MULTIPLIER)
    }
}

pub(super) const LITERAL_FALLBACK_MIN_TERM_LEN: usize = 3;

pub(super) fn literal_fallback_scan(body: &Value, term: &str, k: usize) -> Option<Value> {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let named_paths: Vec<String> = body
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .map(|p| vec![p.to_string()])
        .unwrap_or_default();
    let path_refs: Vec<&str> = named_paths.iter().map(String::as_str).collect();
    let scan = crate::code_index::LiteralScan {
        no_ignore: scan_no_ignore_requested(body),
        pattern: term,
        root: body
            .get("root")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
            .filter(|p| !p.is_empty()),
        paths: &path_refs,
        regex: false,
        case_insensitive: true,
        whole_word: false,
        comments_only: false,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        max_matches: k.max(1),
        max_files: crate::code_index::LITERAL_SCAN_MAX_FILES,
        output: crate::code_index::ScanOutput::Matches,
        list_limit: None,
        max_chars: crate::code_index::DEFAULT_REPLY_MAX_CHARS,
        spill_name: format!(
            "codesearch-fallback-{}.txt",
            dispatch_task_id().unwrap_or_else(|| unsafe { host_now_ms() }.to_string())
        ),
        term_combination: Some("phrase"),
        budget_ms: None,
        max_matches_per_file: None,
        context: 0,
        refresh: false,
        verbose: false,
    };
    let out = crate::code_index::scan_literal(&scan, &cfg);
    if out.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        return None;
    }
    Some(out)
}

pub(super) fn note_map(map: &mut serde_json::Map<String, Value>, note: String) {
    map.insert("note".to_string(), json!(note));
}

pub(super) fn compact_dual_reply(body: &Value, query: &str, k: u32, raw: Value) -> Value {
    let root = body
        .get("root")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()));
    let opts = crate::codesearch_rank::RankOptions::from_body(body, k as usize, root);
    if opts.verbose {
        return raw;
    }
    let mut compact = crate::codesearch_rank::compact_dual(query, &raw, &opts);
    if compact.get("literal_matches").and_then(|v| v.as_u64()) != Some(0) {
        return compact;
    }
    let term = query.trim();
    if term.len() < LITERAL_FALLBACK_MIN_TERM_LEN || term.contains(char::is_whitespace) {
        return compact;
    }
    let Some(scan) = literal_fallback_scan(body, term, k as usize) else {
        if let Some(map) = compact.as_object_mut() {
            note_map(
                map,
                format!("the exhaustive literal check for \"{term}\" did not run -- the rows above are ranked nearest neighbours and are not verified to contain the term"),
            );
        }
        return compact;
    };
    let found = scan
        .get("match_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if found > 0 {
        let mut out = scan;
        if let Some(map) = out.as_object_mut() {
            map.insert("literal_scan_escalated".to_string(), json!(true));
            note_map(
                map,
                format!("dual retrieval returned no row containing \"{term}\" verbatim -- these are exhaustive literal matches"),
            );
        }
        return out;
    }
    if let Some(map) = compact.as_object_mut() {
        map.insert("literal_scan_matches".to_string(), json!(0));
        note_map(
            map,
            format!("no file in the scanned corpus contains \"{term}\" verbatim -- the rows above are ranked nearest neighbours, not occurrences"),
        );
    }
    compact
}

thread_local! {
    static DISPATCH_STARTED_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CALLER_BUDGET_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub(super) fn set_caller_budget(timeout_ms: Option<u64>) {
    DISPATCH_STARTED_MS.with(|c| c.set(unsafe { host_now_ms() }));
    CALLER_BUDGET_MS.with(|c| c.set(timeout_ms.unwrap_or(0)));
}

pub(super) fn caller_remaining_ms() -> Option<u64> {
    let budget = CALLER_BUDGET_MS.with(|c| c.get());
    if budget == 0 { return None; }
    let elapsed = unsafe { host_now_ms() }.saturating_sub(DISPATCH_STARTED_MS.with(|c| c.get()));
    Some(budget.saturating_sub(elapsed))
}

pub(super) const DUAL_SEARCH_RESERVE_MS: u64 = 30_000;

pub(super) const VECTOR_CHANNEL_RESERVE_MS: u64 = 15_000;

pub(super) const VECTOR_CHANNEL_COLD_RESERVE_MS: u64 = 60_000;

pub(super) fn index_progress_of(result: &Value) -> Value {
    json!({
        "files_indexed": result.get("files_indexed").and_then(|v| v.as_u64()).unwrap_or(0),
        "files_deferred": result.get("deferred_files").and_then(|v| v.as_u64()).unwrap_or(0),
        "chunks": result.get("chunks").and_then(|v| v.as_u64()).unwrap_or(0),
        "complete": result.get("complete").and_then(|v| v.as_bool()).unwrap_or(false),
        "pass_ms": result.get("pass_ms").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

pub(super) fn unindexed_channel(root: &str) -> Value {
    json!({ "independent": true, "indexed_root": root, "ran": false })
}

pub(super) fn cold_index_pending_reply(root: &str, retry: &Value) -> Value {
    json!({
        "mode": "dual",
        "root": root,
        "partial": true,
        "partial_reason": "this project's index was stale, and the pass that rebuilds it used the budget this dispatch had left; re-dispatch the same query to search what is indexed now and to continue indexing",
        "index_progress": retry.get("index_progress").cloned().unwrap_or_else(|| json!({})),
        "stage_ms": retry.get("stage_ms").cloned().unwrap_or_else(|| json!({})),
        "vector_hits": [],
        "bm25_hits": [],
        "phrase_hits": [],
        "commits": [],
        "channels": {
            "vector": unindexed_channel(root),
            "bm25": unindexed_channel(root),
            "phrase": unindexed_channel(root),
            "commits": unindexed_channel(root),
        },
        "degraded": true,
    })
}

pub(super) const PATH_SCOPE_FIELDS: &[&str] = &["paths", "path", "path_glob", "glob", "include"];

pub(super) struct PathScope {
    patterns: Vec<String>,
    globs: Vec<crate::path_glob::PathGlob>,
}

impl PathScope {
    fn from_body(body: &Value) -> Result<Option<PathScope>, String> {
        let mut patterns: Vec<String> = Vec::new();
        for field in PATH_SCOPE_FIELDS {
            let Some(value) = body.get(*field) else {
                continue;
            };
            match value {
                Value::String(text) => push_scope_pattern(&mut patterns, text, field)?,
                Value::Array(entries) => {
                    if entries.is_empty() {
                        return Err(format!(
                            "{field} is an empty array -- a blank scope is dropped before the scan runs and silently searches everything; omit the field to search unscoped"
                        ));
                    }
                    for entry in entries {
                        match entry.as_str() {
                            Some(text) => push_scope_pattern(&mut patterns, text, field)?,
                            None => {
                                return Err(format!(
                                "{field} entries must all be non-empty path strings; got {entry}"
                            ))
                            }
                        }
                    }
                }
                Value::Null => continue,
                other => {
                    return Err(format!(
                        "{field} must be a string or an array of strings; got {other}"
                    ))
                }
            }
        }
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut globs = Vec::with_capacity(patterns.len());
        for pattern in &patterns {
            match crate::path_glob::PathGlob::parse(pattern) {
                Ok(glob) => globs.push(glob),
                Err(e) => return Err(e),
            }
        }
        Ok(Some(PathScope { patterns, globs }))
    }

    fn admits(&self, root: &str, path: &str) -> bool {
        self.globs.iter().any(|glob| glob.admits(root, None, path))
    }

    fn applied(&self) -> String {
        if self.patterns.len() == 1 {
            return self.patterns[0].clone();
        }
        self.patterns.join(" ")
    }
}

pub(super) fn push_scope_pattern(out: &mut Vec<String>, raw: &str, field: &str) -> Result<(), String> {
    let normalized = raw.trim().replace('\\', "/");
    let normalized = normalized
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string();
    if normalized.is_empty() {
        return Err(format!(
            "{field} carries an empty path -- a blank scope is dropped before the scan runs and silently searches everything; omit the field to search unscoped"
        ));
    }
    if normalized.starts_with('!') {
        return Err(format!(
            "{field} carries the negated pattern \"{normalized}\" -- there is no exclude filter in this build; scope positively and send one dispatch per subtree"
        ));
    }
    if crate::path_glob::looks_like_glob(&normalized) {
        if !out.contains(&normalized) {
            out.push(normalized)
        }
        return Ok(());
    }
    let below = format!("{normalized}/**");
    if !out.contains(&normalized) {
        out.push(normalized)
    }
    if !out.contains(&below) {
        out.push(below)
    }
    Ok(())
}

pub(super) const SCOPED_CANDIDATE_MULTIPLIER: usize = 8;

pub(super) const SCOPED_CANDIDATE_FLOOR: usize = 50;

pub(super) fn scoped_candidate_k(k: u32, scope: Option<&PathScope>) -> usize {
    match scope {
        Some(_) => (k as usize)
            .saturating_mul(SCOPED_CANDIDATE_MULTIPLIER)
            .max(SCOPED_CANDIDATE_FLOOR),
        None => k as usize,
    }
}

pub(super) fn hit_scope_path(hit: &Value) -> Option<String> {
    hit.get("path")
        .and_then(|v| v.as_str())
        .or_else(|| {
            hit.get("symbol")
                .and_then(|s| s.get("path"))
                .and_then(|v| v.as_str())
        })
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
}

pub(super) fn retain_hits_in_scope(hits: &mut Vec<Value>, root: &str, scope: Option<&PathScope>) {
    let Some(scope) = scope else { return };
    hits.retain(|hit| match hit_scope_path(hit) {
        Some(path) => scope.admits(root, &path),
        None => false,
    });
}

pub(super) fn apply_scope_echo(reply: &mut Value, scope: &PathScope, channels: &[&Vec<Value>]) {
    let mut files: Vec<String> = Vec::new();
    for channel in channels {
        for hit in *channel {
            if let Some(path) = hit_scope_path(hit) {
                if !files.contains(&path) {
                    files.push(path)
                }
            }
        }
    }
    let Value::Object(map) = reply else { return };
    map.insert("path_glob".to_string(), json!(scope.applied()));
    map.insert("files_matching_glob".to_string(), json!(files.len()));
    if files.is_empty() {
        map.insert("glob_matched_no_files".to_string(), json!(true));
    }
}

pub(super) fn codesearch_at_root(
    body: &Value,
    root: &str,
    query: &str,
    k: u32,
    cfg: &crate::ragconfig::RagConfig,
) -> u64 {
    if !crate::wasm_dispatch::host_allow_root(root) {
        return err(
            "codesearch",
            &format!(
                "root '{root}' is not a real, existing directory the host will grant access to"
            ),
        );
    }
    let scope = match PathScope::from_body(body) {
        Ok(scope) => scope,
        Err(e) => return err("codesearch", &e),
    };
    let already_indexed = body
        .get("auto_indexed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let prior_stage_ms = body.get("stage_ms").cloned().unwrap_or_else(|| json!({}));
    if !already_indexed {
        let digest_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        let stored = crate::code_index::stored_digest_at(Some(root));
        let current = crate::code_index::current_digest_at(root);
        let stale = match &stored { Some(s) => s != &current, None => true };
        if stale && crate::code_index::topup_allowed(&stored, Some(root)) {
            let cold_start = stored.is_none();
            let reason = if cold_start {
                "digest-absent"
            } else {
                "digest-mismatch"
            };
            emit_event(
                "codeinsight_rebuild",
                json!({ "reason": reason, "root": root, "stored_then_current": current }),
            );
            let index_started = unsafe { crate::wasm_dispatch::host_now_ms() };
            let cold_budget_ms = match caller_remaining_ms() {
                Some(remaining) => COLD_INDEX_PASS_BUDGET_MS.min(remaining.saturating_sub(DUAL_SEARCH_RESERVE_MS)),
                None => COLD_INDEX_PASS_BUDGET_MS,
            };
            let index_result = if cold_start {
                crate::code_index::index_at_topup(root, cfg.index.prune_pass_file_limit_ceiling, root, cold_budget_ms)
            } else {
                crate::code_index::index_at_topup(root, cfg.index.prune_pass_file_limit_ceiling, root, cfg.index.incremental_topup_wall_budget_ms)
            };
            let index_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(index_started);
            let mut retry = body.clone();
            if let Some(obj) = retry.as_object_mut() {
                obj.insert("auto_indexed".to_string(), Value::Bool(true));
                obj.insert("index_progress".to_string(), index_progress_of(&index_result));
                obj.insert("stage_ms".to_string(), json!({
                    "digest": index_started.saturating_sub(digest_started),
                    "index_pass": index_ms,
                    "total": unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms),
                }));
            }
            if caller_remaining_ms().map(|left| left < DUAL_SEARCH_RESERVE_MS).unwrap_or(false) {
                emit_event("codesearch_index_pending", json!({
                    "root": root,
                    "index_progress": retry.get("index_progress").cloned().unwrap_or_else(|| json!({})),
                }));
                return ok("codesearch", cold_index_pending_reply(root, &retry));
            }
            return codesearch_at_root(&retry, root, query, k, cfg);
        }
    }
    let stage = |from: u64| unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(from);
    let mut at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let shown_k = dual_channel_depth(body, k);
    let scan_k = scoped_candidate_k(shown_k as u32, scope.as_ref());
    let mut corpus = crate::code_index::FusionCorpus::load_at(Some(root));
    let corpus_ms = stage(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let bm25_ranked = corpus.bm25_rank_cfg(query, scan_k, &cfg.scoring);
    let bm25_ms = stage(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut bm25_hits: Vec<Value> = bm25_ranked
        .into_iter()
        .map(|(key, score)| {
            let text = corpus.text_for_key(&key).unwrap_or_default();
            let mut hit = serde_json::Map::new();
            hit.insert("key".to_string(), json!(key));
            hit.insert("text".to_string(), json!(text));
            hit.insert("score".to_string(), json!(score));
            if let Some(symbol) =
                corpus.symbol_for_key(hit.get("key").and_then(|v| v.as_str()).unwrap_or(""))
            {
                hit.insert("symbol".to_string(), symbol);
            }
            Value::Object(hit)
        })
        .collect();
    let commits = crate::code_index::git_commit_rank_at(root, query, 10);
    let commits_ms = stage(at); at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let (mut phrase_hits, mut phrase_total, phrase_exhaustive) =
        dual_phrase_hits(query, Some(root), cfg, scan_no_ignore_requested(body));
    let vector_reserve_ms = if prior_stage_ms.get("index_pass").is_some() {
        VECTOR_CHANNEL_COLD_RESERVE_MS
    } else {
        VECTOR_CHANNEL_RESERVE_MS
    };
    let vector_channel_ran = caller_remaining_ms().map(|left| left >= vector_reserve_ms).unwrap_or(true);
    let (mut vector_hits, embed_ms, vector_ms, degraded) = if vector_channel_ran {
        let embedding = embed_query(query);
        let embed_ms = stage(at); at = unsafe { crate::wasm_dispatch::host_now_ms() };
        let vres = crate::code_index::search_at(query, scan_k, Some(&embedding), Some(root));
        let rows = vres.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
        let vector_ms = stage(at);
        (rows, embed_ms, vector_ms, vres.get("degraded").cloned().unwrap_or(json!(false)))
    } else {
        (Vec::new(), 0u64, 0u64, json!(true))
    };
    retain_hits_in_scope(&mut vector_hits, root, scope.as_ref());
    retain_hits_in_scope(&mut bm25_hits, root, scope.as_ref());
    vector_hits.truncate(shown_k);
    bm25_hits.truncate(shown_k);
    if scope.is_some() {
        retain_hits_in_scope(&mut phrase_hits, root, scope.as_ref());
        phrase_total = phrase_hits.len() as u64;
    }
    let phrase_ms = stage(at);
    let stage_ms = json!({
        "embed_query": embed_ms,
        "vector_search": vector_ms,
        "corpus_load": corpus_ms,
        "bm25_rank": bm25_ms,
        "commits": commits_ms,
        "phrase_scan": phrase_ms,
        "total": unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms),
    });
    let mut raw = json!({
        "mode": "dual",
        "root": root,
        "vector_hits": vector_hits,
        "bm25_hits": bm25_hits,
        "phrase_hits": phrase_hits,
        "phrase_hits_total": phrase_total,
        "phrase_hits_truncated": !phrase_exhaustive || phrase_total > phrase_hits.len() as u64,
        "commits": commits,
        "channels": {
            "vector": if vector_channel_ran {
                json!({ "independent": true, "indexed_root": root, "ran": true })
            } else {
                json!({
                    "independent": true,
                    "indexed_root": root,
                    "ran": false,
                    "skipped": "the embedding channel could not finish inside this dispatch's remaining budget, so bm25, phrase and commits answered instead; re-dispatch for the vector channel",
                })
            },
            "bm25": { "independent": true, "indexed_root": root },
            "phrase": {
                "independent": true,
                "budget_ms": DUAL_PHRASE_SCAN_BUDGET_MS,
                "root": root,
                "per_file_matches": DUAL_PHRASE_SCAN_PER_FILE_MATCHES,
                "exhaustive": phrase_exhaustive && phrase_total <= phrase_hits.len() as u64,
            },
            "commits": { "independent": true, "indexed_root": root },
        },
        "stage_ms": merge_stage_ms(prior_stage_ms, stage_ms),
        "degraded": degraded,
    });
    if let Some(scope) = scope.as_ref() {
        apply_scope_echo(&mut raw, scope, &[&vector_hits, &bm25_hits]);
    }
    ok("codesearch", compact_dual_reply(body, query, k, raw))
}

/// An index pass runs in the dispatch that discovered the stale digest and then recurses, so the
/// stages measured before and after it are summed into one report.
pub(super) fn merge_stage_ms(prior: Value, now: Value) -> Value {
    let mut out = match prior {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    if let Value::Object(m) = now {
        for (k, v) in m {
            let sum = out.get(&k).and_then(|p| p.as_u64()).unwrap_or(0) + v.as_u64().unwrap_or(0);
            out.insert(k, json!(sum));
        }
    }
    Value::Object(out)
}

pub(super) const REGEX_SCAN_DEFAULT_BUDGET_MS: u64 = 20_000;

pub(super) const CODESEARCH_MODES: &[&str] = &["dual", "literal", "regex", "filename"];

pub(super) const CODESEARCH_EXHAUSTIVE_FIELDS: &[&str] = &[
    "query",
    "mode",
    "path",
    "glob",
    "path_glob",
    "exclude",
    "exclude_glob",
    "exclude_globs",
    "case_insensitive",
    "whole_word",
    "comments_only",
    "no_ignore",
    "include_ignored",
    "exhaustive",
    "k",
    "max_results",
    "maxResults",
    "limit",
    "head_limit",
    "max_matches",
    "max_files",
    "output",
    "max_chars",
    "verbose",
    "docs",
    "timeout_ms",
    "refresh",
    "no_cache",
    "force_disk",
    "root",
    "projectPath",
    "cwd",
    "combine",
    "term_combination",
    "verbatim",
];

pub(super) const CODESEARCH_LIMIT_FIELDS: &[&str] = &["k", "max_results", "maxResults", "limit", "head_limit"];

pub(super) fn scan_result_limit(body: &Value, fields: &[&str], default: u32) -> Result<(u32, bool), String> {
    let mut seen: Vec<(&str, u64)> = Vec::new();
    for field in fields {
        if let Some(v) = body.get(*field) {
            match v.as_u64() {
                Some(n) if n > 0 => seen.push((field, n)),
                _ => {
                    return Err(format!(
                        "body field \"{field}\" must be a positive integer result limit, got {v}"
                    ))
                }
            }
        }
    }
    match seen.first() {
        None => Ok((default, false)),
        Some((_, first)) => {
            if let Some((other, other_n)) = seen.iter().find(|(_, n)| n != first) {
                let (winner, _) = seen[0];
                return Err(format!(
                    "conflicting result limits in one body: \"{winner}\"={first} and \"{other}\"={other_n} -- pass one, not both"
                ));
            }
            Ok((*first as u32, true))
        }
    }
}

pub(super) fn codesearch_result_limit(
    body: &Value,
    cfg: &crate::ragconfig::RagConfig,
) -> Result<(u32, bool), String> {
    scan_result_limit(body, CODESEARCH_LIMIT_FIELDS, cfg.budget.default_k as u32)
}

/// The directory a scan searches. `root` and its alias `projectPath` win; `cwd` is the spelling an
/// MCP client already has in hand for the same thing, so it is read rather than silently ignored.
pub(super) fn scan_root(body: &Value) -> Option<&str> {
    body.get("root")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
        .or_else(|| body.get("cwd").and_then(|v| v.as_str()))
        .filter(|p| !p.is_empty())
}

pub(super) fn scan_target_paths(body: &Value) -> Result<Vec<String>, String> {
    let mut paths: Vec<String> = Vec::new();
    for field in ["path", "paths", "files"] {
        match body.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => paths.push(s.clone()),
            Some(Value::Array(items)) => for item in items {
                match item.as_str() {
                    Some(s) => paths.push(s.to_string()),
                    None => return Err(format!("{field} entries must all be path strings, got {item}")),
                }
            },
            Some(other) => return Err(format!("{field} must be a path string or an array of path strings, got {other}")),
        }
    }
    paths.retain(|p| !p.trim().is_empty());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    Ok(paths)
}

pub(super) fn resolve_scan_target(body: &Value) -> Result<(Option<&str>, Vec<String>), String> {
    let mut root = scan_root(body);
    let mut paths = scan_target_paths(body)?;
    if let Some(candidate) = root {
        if candidate == "." || candidate == "./" {
            root = None;
        } else if !crate::wasm_dispatch::host_allow_root(candidate) {
            let scope = candidate.strip_prefix("./").unwrap_or(candidate);
            let valid_relative_scope = !scope.starts_with('/')
                && scope
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..");
            if valid_relative_scope {
                if paths.is_empty() {
                    paths.push(scope.to_owned());
                } else {
                    for path in &mut paths { *path = format!("{scope}/{path}"); }
                }
                root = None;
            } else {
                return Err(format!("root '{candidate}' is not a real, existing directory the host will grant access to"));
            }
        }
    }
    Ok((root, paths))
}

pub(super) fn scan_path_refs(paths: &[String]) -> Vec<&str> {
    paths.iter().map(String::as_str).collect()
}

pub(super) fn glob_patterns_from(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(Some(s.clone())
            .filter(|g| !g.is_empty())
            .into_iter()
            .collect()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item.as_str() {
                Some(s) => Ok(s.to_string()),
                None => Err("array entries must all be glob strings".to_string()),
            })
            .map(|r| r.map(|s| s.trim().to_string()))
            .filter(|r| !matches!(r, Ok(s) if s.is_empty()))
            .collect(),
        Some(_) => Err("must be a glob string or an array of glob strings".to_string()),
    }
}

pub(super) fn dispatch_task_id() -> Option<String> {
    let key = "AGENTPLUG_DISPATCH_TASK";
    let packed = unsafe { host_env_get(key.as_ptr(), key.len() as u32) };
    unpack_to_string(packed)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub(super) fn codesearch_exhaustive(
    body: &Value,
    query: &str,
    regex: bool,
    cfg: &crate::ragconfig::RagConfig,
    explicit_limit: Option<u32>,
) -> u64 {
    let filenames = body.get("mode").and_then(|v| v.as_str()) == Some("filename");
    if filenames {
        for field in ["whole_word", "comments_only"] {
            if body.get(field).and_then(|value| value.as_bool()) == Some(true) {
                return err(
                    "codesearch",
                    &format!("{field} applies to source contents, not filename matching"),
                );
            }
        }
    }
    let (root, paths) = match resolve_scan_target(body) {
        Ok(target) => target,
        Err(e) => return err("codesearch", &e),
    };
    let path_refs = scan_path_refs(&paths);
    let max_matches = match body.get("max_matches") {
        Some(value) => match value.as_u64() {
            Some(limit) if limit > 0 => limit as usize,
            _ => return err("codesearch", "max_matches must be a positive integer"),
        },
        None => explicit_limit
            .map(|limit| limit as usize)
            .unwrap_or(usize::MAX),
    };
    let explicit_combine = body
        .get("combine")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("term_combination").and_then(|v| v.as_str()))
        .filter(|c| !c.is_empty());
    // `verbatim` is the caller-facing spelling of "match the query as one string": it is the
    // default for a multi-word query, so it is only named here to keep the intent in the body.
    let combine = match explicit_combine {
        Some(c) => Some(c),
        None if body
            .get("verbatim")
            .and_then(|v| v.as_bool())
            .unwrap_or(false) =>
        {
            Some("phrase")
        }
        None => None,
    };
    if let Some(c) = combine {
        if !["or", "and", "phrase"].contains(&c) {
            return err("codesearch", &format!(
                "combine \"{c}\" is not a term combination -- valid values are \"phrase\" (default: a multi-word query is matched verbatim as one string, spaces included), \"and\" (a line must carry every term) and \"or\" (a multi-word query is split into terms and a line needs any of them, ranked by how many it carries -- lines carrying fewer terms rank strictly below lines carrying all of them)"
            ));
        }
    }
    let output = match body.get("output") {
        None | Some(Value::Null) => crate::code_index::ScanOutput::Matches,
        Some(raw) => match raw.as_str().and_then(crate::code_index::ScanOutput::parse) {
            Some(mode) => mode,
            None => {
                return err(
                    "codesearch",
                    &format!(
                        "output {raw} is not an output mode -- valid modes are {}",
                        crate::code_index::SCAN_OUTPUT_NAMES
                    ),
                )
            }
        },
    };
    let max_chars = match body.get("max_chars") {
        None | Some(Value::Null) => crate::code_index::DEFAULT_REPLY_MAX_CHARS,
        Some(value) => match value.as_u64() {
            Some(n) if n > 0 => (n as usize).min(crate::code_index::MAX_REPLY_MAX_CHARS),
            _ => {
                return err(
                    "codesearch",
                    "max_chars must be a positive integer number of characters",
                )
            }
        },
    };
    let list_output = matches!(
        output,
        crate::code_index::ScanOutput::Files | crate::code_index::ScanOutput::Count
    );
    let (max_matches, list_limit) = if list_output {
        (
            body.get("max_matches")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(usize::MAX),
            explicit_limit.map(|n| n as usize),
        )
    } else {
        (max_matches, None)
    };
    let mut include_globs: Vec<String> = Vec::new();
    let mut exclude_globs: Vec<String> = Vec::new();
    for key in ["path_glob", "glob"] {
        match glob_patterns_from(body.get(key)) {
            Ok(patterns) => {
                for pattern in patterns {
                    match pattern.strip_prefix('!') {
                        Some(negated) => exclude_globs.push(negated.to_string()),
                        None => include_globs.push(pattern),
                    }
                }
            }
            Err(e) => return err("codesearch", &format!("{key} {e}")),
        }
    }
    for key in ["exclude", "exclude_glob", "exclude_globs"] {
        match glob_patterns_from(body.get(key)) {
            Ok(patterns) => exclude_globs.extend(
                patterns
                    .into_iter()
                    .map(|p| p.strip_prefix('!').map(str::to_string).unwrap_or(p)),
            ),
            Err(e) => return err("codesearch", &format!("{key} {e}")),
        }
    }
    if body.get("docs").is_some_and(|v| !v.is_boolean()) {
        return err("codesearch", "docs must be a boolean -- literal/regex scans include docs by default; docs=false excludes *.md/*.mdx/*.rst/*.adoc and docs/ trees");
    }
    if body.get("docs").and_then(|v| v.as_bool()) == Some(false) {
        exclude_globs.extend(
            crate::codesearch_rank::DOC_EXCLUDE_GLOBS
                .iter()
                .map(|g| g.to_string()),
        );
    }
    let budget_ms = match body.get("timeout_ms") {
        None | Some(Value::Null) => regex.then_some(REGEX_SCAN_DEFAULT_BUDGET_MS),
        Some(value) => match value.as_u64() {
            Some(n) if n > 0 => Some(n.min(cfg.index.wall_budget_ms)),
            _ => {
                return err(
                    "codesearch",
                    "timeout_ms must be a positive integer number of milliseconds",
                )
            }
        },
    };
    let spill_name = format!(
        "codesearch-{}.txt",
        dispatch_task_id().unwrap_or_else(|| unsafe { host_now_ms() }.to_string())
    );
    let scan = crate::code_index::LiteralScan {
        pattern: query,
        root,
        paths: &path_refs,
        regex,
        case_insensitive: body
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(filenames),
        whole_word: body
            .get("whole_word")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        comments_only: body
            .get("comments_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        include_globs,
        exclude_globs,
        max_matches,
        max_files: body
            .get("max_files")
            .and_then(|v| v.as_u64())
            .unwrap_or(crate::code_index::LITERAL_SCAN_MAX_FILES as u64)
            as usize,
        term_combination: combine,
        budget_ms,
        max_matches_per_file: None,
        context: 0,
        refresh: scan_refresh_requested(body),
        no_ignore: scan_no_ignore_requested(body),
        output,
        list_limit,
        max_chars,
        spill_name,
        verbose: body
            .get("verbose")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    let scanned = if filenames {
        crate::code_index::scan_filenames(&scan, cfg)
    } else {
        crate::code_index::scan_literal(&scan, cfg)
    };
    if scanned.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        return err(
            "codesearch",
            scanned
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("exhaustive scan failed"),
        );
    }
    let mut out = match scanned {
        Value::Object(map) => map,
        other => return ok("codesearch", other),
    };
    let partial = finish_scan_reply(&mut out, max_matches.min(u32::MAX as usize) as u32);
    answer_scan("codesearch", out, partial)
}

pub(super) const GREP_OUTPUT_MODES: &[&str] = &["content", "files_with_matches", "count"];

pub(super) const GREP_MODES: &[&str] = &["pattern", "comments"];

pub(super) const GREP_EXCLUDE_FIELDS: &[&str] = &["exclude", "exclude_glob", "exclude_globs"];

pub(super) const GREP_ACCEPTED_FIELDS: &[&str] = &[
    "pattern", "query", "mode", "comments", "help",
    "path", "paths", "files",
    "glob", "include", "path_glob", "exclude", "exclude_glob", "exclude_globs",
    "output_mode", "outputMode", "detail", "columns",
    "regex", "fixed_strings", "fixedStrings", "case_insensitive", "ignore_case", "whole_word",
    "context", "max_files",
    "no_ignore", "include_ignored",
    "max_results", "maxResults", "limit", "max_matches", "k",
    "refresh", "no_cache", "force_disk", "file_source",
];

pub(super) const GREP_HELP: &str = "\
grep (alias \"rg\") is an exhaustive literal/regex scan of the tree. Every reply is bounded; read
\"exhaustive\" in the response to know whether it saw everything.

  {\"pattern\":\"<text or regex>\"}                    required (unless mode:\"comments\"); \"query\" is accepted too
  {\"path\":\"<dir or file>\"}                         narrow the scan to one subtree or one file
  {\"paths\":[\"src/a.rs\",\"src/b/\"]}                  narrow it to exactly those files and directories;
                                                     \"files\" is an alias. Paths resolve under the project
                                                     root, or under \"root\"/\"projectPath\" when one is given.
                                                     A field that is not one of the accepted names is
                                                     refused, never ignored.
  {\"glob\":\"**/*.rs\"}                               narrow by path glob; \"include\"/\"path_glob\" are aliases;
                                                     a \"!\"-prefixed entry excludes instead: [\"**/*.rs\",\"!vendor/**\"]
  {\"exclude\":[\"vendor/**\",\"test/hardware/**\"]}   drop paths from the scan: one glob or a list of globs, matched
                                                     like \"glob\", so name a whole tree as \"vendor/**\". This is how a
                                                     sweep skips vendored, generated or other-owned trees -- never
                                                     hand-write an alternation for it. \"exclude_glob\"/\"exclude_globs\"
                                                     are aliases. Exclusions never affect \"exhaustive\".
  {\"output_mode\":\"content\"}                        \"content\" (default): one hit per line; \"files_with_matches\"; \"count\"
  {\"detail\":true}                                   \"content\" answers \"output\" (one \"path:line: text\" string per hit)
                                                     plus \"counts\" ({path,count} per file) -- never both \"output\" and
                                                     \"matches\". \"detail\":true returns structured \"matches\" objects
                                                     instead of \"output\"; add \"columns\":true to keep each hit's column.
                                                     \"occurrence_count\" is omitted whenever it is 1.
  {\"regex\":true}                                    force regex on/off; unset, the pattern is auto-read
  {\"fixed_strings\":true}                            match the pattern literally, never as a regex
  {\"case_insensitive\":true}                         \"ignore_case\" is an alias
  {\"whole_word\":true}
  {\"context\":2}                                     include N lines before and after each hit, in any mode including
                                                     mode comments; context lines print as path-N- text, hit
                                                     lines as path:line: text, gaps between windows as --
  {\"max_results\":200}                               hit cap (default 200); aliases: maxResults, limit, max_matches, k
  {\"max_files\":50000}                               file cap (default 50000)
  {\"refresh\":true}                                  re-read from disk: walk instead of `git ls-files --cached`,
                                                     and bypass the mtime-keyed content cache, so uncommitted
                                                     edits and untracked files are visible. \"file_source\":\"disk\"
                                                     and \"no_cache\":true are aliases.
  {\"no_ignore\":true}                                 include files .gitignore would hide: build output, vendored
                                                     trees and scratch scripts the project never committed are
                                                     listed and scanned like any other file. \"include_ignored\"
                                                     is an alias. .git is never listed either way.
  {\"mode\":\"pattern\"}                               scan for \"pattern\" (the default; naming it explicitly is
                                                     always accepted)
  {\"mode\":\"comments\"}                              find comment spans instead of a pattern (no \"pattern\" needed)

mode:\"comments\" -- one pass for every comment in the tree, column-1 and inline alike:
  languages: C/C++/Java/Go/Rust/JS/TS/C#/PHP/Kotlin/Swift/Zig/Scala and Faust .dsp use // and /* */;
             shell, YAML, TOML/INI/conf, Python, Ruby, Perl, PowerShell, Julia, Elixir, Dockerfile and
             Makefile use #. String literals are respected, so \"http://x\" and '#' are not comments.
  returns:   \"comments\":  [{path,line,column,kind:\"line\"|\"block\",syntax,inline,text}]
             \"directives\": [{...same shape...}]  -- keepers, not prose to delete: #!/bin/sh shebangs,
                            # syntax=docker/dockerfile:1 and # shellcheck disable=... land here, never in
                            \"comments\". \"inline\":true marks a comment with code before it on the same line.
             plus \"comment_count\", \"directive_count\", \"files\", \"output\" (path:line:column: text),
             \"file_source\", \"file_source_detail\" and \"exhaustive\".

Every scan reports \"file_source\" and \"file_source_detail\": \"git\" means `git ls-files --cached`
(tracked files only), \"walk\" a filesystem walk and \"file\" a single file read straight from disk.
When \"exhaustive\" is false the reply also carries \"partial\": true and a \"partial_reason\" naming the
bound that fired, plus \"exhaustive_note\" on how to reach full coverage: scope with \"path\"/\"paths\"/\"glob\" and
repeat per subtree, or raise \"max_results\".";

pub(super) const CODESEARCH_HELP: &str = "\
codesearch (aliases \"code_search\", \"search\") is the canonical search verb.
  {\"query\":\"<text>\"}                 required
  {\"mode\":\"dual\"}                    \"dual\" (default): ranked BM25+vector retrieval;
                                       \"literal\"/\"regex\": exhaustive, every match with path:line, no ranking;
                                       \"filename\": matches paths only
  {\"k\":10}                            result cap for \"dual\"; aliases: max_results, maxResults, limit
  {\"max_matches\":1000}                hit cap for the exhaustive modes
  {\"max_files\":50000}                 file cap
  {\"path\":\"<dir or file>\"}           narrow the scan; may be absolute when it is inside the search root
  {\"root\":\"<project dir>\"}           search another project; \"path\" is then relative to it
  {\"projectPath\":\"<project dir>\"}    alias of \"root\"
  {\"cwd\":\"<project dir>\"}            alias of \"root\", lowest precedence of the three
  {\"path_glob\":\"**/*.rs\"}            narrow by glob; \"glob\" is an alias
  {\"combine\":\"phrase\"}               \"phrase\" (default for a multi-word query), \"and\" (every term on one line),
                                       \"or\" (ranked union of any term)
  {\"case_insensitive\":true, \"whole_word\":true}
  {\"refresh\":true}                    re-read from disk for the exhaustive modes: walk instead of
                                       `git ls-files --cached` and bypass the content cache
  {\"no_ignore\":true}                  include files .gitignore would hide, in every mode: build output,
                                       vendored trees and scratch scripts the project never committed
                                       are listed and scanned like any other file. \"include_ignored\"
                                       is an alias. .git is never listed either way.";

pub(super) const FS_READ_HELP: &str = "\
fs_read returns a file's contents.
  {\"path\":\"<relative path>\"}     required, relative and within the project
  {\"offset\":0, \"limit\":200}      read a line range; both clamp to the file's real line count, so an
                                  offset past the end returns \"\" with \"returned_lines\":0 instead of failing.
                                  Omit both for the whole file (the pre-existing behaviour).
  {\"max_bytes\":65536}            cap the returned chunk; \"truncated_at_bytes\" reports whether it fired.
  {\"allowOutsideRoot\":true}      opt in to an absolute path outside the project root; required per call.
                                  \"allow_outside_root\" is an alias. A path holding a \"..\" segment is still
                                  refused, and the host sandbox still serves only paths under the user gm root
                                  or a directory carrying a project marker (.git, .gm, package.json,
                                  Cargo.toml, go.mod, pyproject.toml).
Paged replies add \"total_lines\", \"offset\", \"returned_lines\" and \"has_more_lines\".";

pub(super) const FS_WRITE_HELP: &str = "\
fs_write writes a file inside the project.
  {\"path\":\"<relative path>\"}     required, relative and within the project
  {\"content\":\"<text>\"}           required; \"data\" is an alias
Returns {\"bytes\": <written>}. A write outside the root is always refused, allowOutsideRoot included: that
flag widens the read verbs only.";

pub(super) const FS_READDIR_HELP: &str = "\
fs_readdir lists one directory inside the project. {\"path\":\"<relative dir>\"} (default \".\").
  {\"allowOutsideRoot\":true}      opt in to an absolute path outside the project root; required per call.
                                  \"allow_outside_root\" is an alias. A path holding a \"..\" segment is still
                                  refused, and the host sandbox still serves only paths under the user gm root
                                  or a directory carrying a project marker (.git, .gm, package.json,
                                  Cargo.toml, go.mod, pyproject.toml).";

pub(super) const FS_STAT_HELP: &str = "\
fs_stat stats one path inside the project. {\"path\":\"<relative path>\"}, required.
  {\"allowOutsideRoot\":true}      opt in to an absolute path outside the project root; required per call.
                                  \"allow_outside_root\" is an alias. A path holding a \"..\" segment is still
                                  refused, and the host sandbox still serves only paths under the user gm root
                                  or a directory carrying a project marker (.git, .gm, package.json,
                                  Cargo.toml, go.mod, pyproject.toml).";

pub(super) fn help_requested(body: &Value) -> bool {
    match body.get("help") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let s = s.trim().to_lowercase();
            !s.is_empty() && s != "false" && s != "0" && s != "no" && s != "off"
        }
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) > 0,
        _ => false,
    }
}

pub(super) fn verb_help_doc(verb: &str) -> Option<&'static str> {
    match verb {
        "grep" | "rg" => Some(GREP_HELP),
        "codesearch" | "code_search" | "search" => Some(CODESEARCH_HELP),
        "fs_read" => Some(FS_READ_HELP),
        "fs_write" => Some(FS_WRITE_HELP),
        "fs_readdir" => Some(FS_READDIR_HELP),
        "fs_stat" => Some(FS_STAT_HELP),
        "git_worktree" => Some("git_worktree {action: list} returns worktrees; {action: add, path, ref?: HEAD, detach?: true} creates a linked checkout; detach false requires an existing local branch name; {action: remove, path} removes a clean unlocked checkout without force. Unknown fields are refused per action. Repository selectors and session fields are accepted."),
        _ => None,
    }
}

pub(super) fn scan_refresh_requested(body: &Value) -> bool {
    if body
        .get("refresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if body
        .get("no_cache")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if body
        .get("force_disk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    body.get("file_source")
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("disk") || s.eq_ignore_ascii_case("walk"))
        .unwrap_or(false)
}

/// "no_ignore" is how ripgrep spells it; "include_ignored" says the same thing in the caller's own
/// words. Both are opt-in, so neither changes a scan that does not name one.
pub(super) fn scan_no_ignore_requested(body: &Value) -> bool {
    body.get("no_ignore").and_then(|v| v.as_bool()).unwrap_or(false)
        || body.get("include_ignored").and_then(|v| v.as_bool()).unwrap_or(false)
        || body.get("exhaustive").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub(super) fn scan_scope_hint(scan_cap: u32) -> String {
    format!(
        "to reach every match, scope the scan and repeat per subtree: pass \"path\":\"<dir-or-file>\" or \"paths\":[\"<one>\",\"<two>\"] and/or \"glob\":\"**/*.rs\" -- a scoped scan's cap is per call, so union the per-scope results; or raise the cap with \"max_results\": <n> (this call used {scan_cap}); \"output_mode\":\"files_with_matches\" or \"count\" covers far more files per call than \"content\""
    )
}

/// Counters that describe the scan to a human sitting in front of it and nothing else: the cache is
/// internal, the phase split is profiling, and the listed/unreadable counts are already folded into
/// `partial_reason` on the scans where they left the answer incomplete. `excluded_by_rule_summary`
/// stays -- it names which rule hid which subtree, which is the one a caller can act on.
pub(super) const SCAN_TELEMETRY_DROPPED: &[&str] = &[
    "scan_cache",
    "phase_ms",
    "files_listed",
    "files_unreadable",
    "files_with_nul_scanned",
    "excluded_by_rule",
    "excluded_by_rule_count",
];

/// A bounded scan still answers, so it stays `ok` -- and `ok: true` beside an empty `matches` reads
/// as "there is nothing there" when it means "this did not look at everything". The bound that fired
/// rides at the top of the envelope, where a caller meets it before the answer.
pub(super) fn scan_partial_reason(out: &serde_json::Map<String, Value>) -> Option<String> {
    if out.get("exhaustive").and_then(|v| v.as_bool()) != Some(false) {
        return None;
    }
    let num = |key: &str| out.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let mut bounds: Vec<String> = Vec::new();
    if out.get("budget_exhausted").and_then(|v| v.as_bool()) == Some(true) {
        bounds.push(format!(
            "the {} ms wall budget ran out after {} of {} listed files",
            num("budget_ms"),
            num("files_scanned"),
            num("files_listed")
        ));
    }
    if out.get("matches_truncated").and_then(|v| v.as_bool()) == Some(true) {
        bounds.push(format!(
            "the hit cap cut at {} matches",
            num("matches_truncated_at")
        ));
    }
    if out.get("files_truncated").and_then(|v| v.as_bool()) == Some(true) {
        bounds.push(format!(
            "the file cap cut at {} files",
            num("files_truncated_at")
        ));
    }
    let unreadable = num("files_unreadable");
    if unreadable > 0 {
        bounds.push(format!("{unreadable} listed files could not be read"));
    }
    let too_large = num("files_skipped_too_large_count");
    if too_large > 0 {
        bounds.push(format!(
            "{too_large} files were skipped as over the size ceiling"
        ));
    }
    let untyped = num("files_skipped_untyped_oversize_count");
    if untyped > 0 {
        bounds.push(format!(
            "{untyped} oversize files with no extension were skipped before reading"
        ));
    }
    let reason = if bounds.is_empty() {
        "the scan did not cover the whole scope".to_string()
    } else {
        bounds.join("; ")
    };
    Some(format!(
        "{reason} -- the matches below are NOT every match in the scope"
    ))
}

pub(super) fn finish_scan_reply(out: &mut serde_json::Map<String, Value>, scan_cap: u32) -> Option<String> {
    let partial = scan_partial_reason(out);
    for key in SCAN_TELEMETRY_DROPPED {
        out.remove(*key);
    }
    if out.contains_key("hint") {
        out.remove("query_note");
    }
    if partial.is_some() {
        out.insert(
            "exhaustive_note".to_string(),
            json!(scan_scope_hint(scan_cap)),
        );
    }
    out.remove("scope_hint");
    partial
}

pub(super) const GREP_LIMIT_FIELDS: &[&str] = &["max_results", "maxResults", "limit", "max_matches", "k"];

pub(super) const GREP_DEFAULT_MAX_MATCHES: u32 = 200;

pub(super) const GREP_NON_CONTENT_SCAN_CAP: u32 = 50_000;

pub(super) const GREP_REGEX_CLASS_ESCAPES: &[u8] = &[b'b', b'B', b'd', b'D', b'w', b'W', b's', b'S'];

/// A grep pattern is read as a regex when it carries a construct nobody means literally: an
/// alternation bar, a class escape such as `\d`, a `[a-z]`-shaped range, or an anchor pinned to
/// an edge. Anything else stays a substring, so a plain call keeps matching the text it names.
/// A doubled `||` is left literal -- in source that is the logical-or operator, not a union with
/// an empty branch. Callers who want the decision made for them pass `"regex": true` or
/// `"regex": false` (or `"fixed_strings": true`) and skip this entirely.
pub(super) fn grep_regex_trigger(pattern: &str) -> Option<&'static str> {
    let bytes = pattern.as_bytes();
    let mut i = 0usize;
    let mut alternation = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if bytes
                    .get(i + 1)
                    .is_some_and(|c| GREP_REGEX_CLASS_ESCAPES.contains(c))
                {
                    return Some("class escape");
                }
                i += 2;
            }
            b'|' => {
                let doubled = bytes.get(i + 1) == Some(&b'|') || (i > 0 && bytes[i - 1] == b'|');
                if !doubled {
                    alternation = true;
                }
                i += 1;
            }
            b'[' => {
                let close = match bytes[i..].iter().position(|c| *c == b']') {
                    Some(offset) => i + offset,
                    None => {
                        i += 1;
                        continue;
                    }
                };
                let inner = &bytes[i + 1..close];
                if inner.len() > 1 && inner.contains(&b'-') {
                    return Some("character class range");
                }
                i = close + 1;
            }
            _ => i += 1,
        }
    }
    if alternation {
        return Some("alternation");
    }
    let trimmed = pattern.trim();
    if trimmed.starts_with('^') || trimmed.ends_with('$') {
        Some("anchor")
    } else {
        None
    }
}

pub(super) fn grep_hit_path(hit: &Value) -> String {
    hit.get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn grep_hit_line(hit: &Value) -> String {
    hit.get("line")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .to_string()
}

pub(super) fn grep_hit_text(hit: &Value) -> String {
    hit.get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn grep_output_line(hit: &Value, output_mode: &str) -> String {
    let path = grep_hit_path(hit);
    match output_mode {
        "files_with_matches" => path,
        "count" => format!(
            "{path}:{}",
            hit.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        _ => format!("{path}:{}: {}", grep_hit_line(hit), grep_hit_text(hit)),
    }
}

pub(super) fn grep_counted_files(matches: &[Value]) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for hit in matches {
        let path = grep_hit_path(hit);
        if !counts.contains_key(&path) {
            order.push(path.clone());
        }
        let seen = counts.entry(path).or_insert(0);
        *seen += 1;
    }
    order
        .into_iter()
        .map(|path| json!({ "path": path, "count": counts.get(&path).copied().unwrap_or(0) }))
        .collect()
}

/// A `!`-prefixed entry in the include glob excludes instead, and every `exclude*` field is
/// exclusion-only. Both feed the same `exclude_globs` the literal walker already honours, so an
/// omitted exclude leaves the scan exactly as it was.
pub(super) fn grep_route_globs(body: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let mut include_globs: Vec<String> = Vec::new();
    let mut exclude_globs: Vec<String> = Vec::new();
    let include_field = ["glob", "include", "path_glob"]
        .into_iter()
        .find(|field| body.get(*field).is_some());
    if let Some(field) = include_field {
        for pattern in glob_patterns_from(body.get(field)).map_err(|e| format!("{field} {e}"))? {
            match pattern.strip_prefix('!') {
                Some(negated) => exclude_globs.push(negated.to_string()),
                None => include_globs.push(pattern),
            }
        }
    }
    for field in GREP_EXCLUDE_FIELDS {
        for pattern in glob_patterns_from(body.get(field)).map_err(|e| format!("{field} {e}"))? {
            let pattern = pattern
                .strip_prefix('!')
                .map(str::to_string)
                .unwrap_or(pattern);
            exclude_globs.push(pattern);
        }
    }
    Ok((include_globs, exclude_globs))
}

pub(super) fn grep_shaped_matches(matches: Vec<Value>, want_columns: bool) -> Vec<Value> {
    matches
        .into_iter()
        .map(|hit| {
            let Value::Object(mut map) = hit else {
                return hit;
            };
            if !want_columns {
                map.remove("column");
            }
            if map.get("occurrence_count").and_then(|v| v.as_u64()) == Some(1) {
                map.remove("occurrence_count");
            }
            Value::Object(map)
        })
        .collect()
}

pub(super) fn grep(body: &Value) -> u64 {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let mode = body
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let comments_flag = body
        .get("comments")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let want_comments = mode == "comments" || comments_flag;
    if let Some(refusal) = refuse_unknown_fields("grep", body, GREP_ACCEPTED_FIELDS) { return refusal; }
    if !mode.is_empty() && !GREP_MODES.contains(&mode.as_str()) && !comments_flag {
        return err(
            "grep",
            &format!(
                "mode \"{mode}\" is not a grep mode -- valid modes are {}. \
             \"pattern\" (the default) scans for a pattern; \"comments\" finds comment spans and \
             needs no \"pattern\". Pass {{\"help\": true}} for the full parameter list.",
                GREP_MODES
                    .iter()
                    .map(|m| format!("\"{m}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
    if want_comments {
        return grep_comments(&body, &cfg);
    }
    let pattern = body
        .get("pattern")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("query").and_then(|v| v.as_str()))
        .unwrap_or("");
    if pattern.is_empty() {
        return err("grep", "pattern required -- pass {\"pattern\":\"<text>\"}, optionally narrowed by \"path\" (one file or directory under the project root) or \"paths\"/\"files\" (a list of them) and \"glob\"; pass {\"mode\":\"comments\"} to find comment spans instead, or {\"help\": true} for the full parameter list");
    }
    let output_mode = body
        .get("output_mode")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("outputMode").and_then(|v| v.as_str()))
        .unwrap_or("content");
    if !GREP_OUTPUT_MODES.contains(&output_mode) {
        return err(
            "grep",
            &format!(
                "output_mode \"{output_mode}\" is not a grep output mode -- valid modes are {}. \
             \"content\" returns every hit as path:line: text, \"files_with_matches\" returns each \
             matching path once, \"count\" returns path:count.",
                GREP_OUTPUT_MODES
                    .iter()
                    .map(|m| format!("\"{m}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
    let (root, paths) = match resolve_scan_target(body) {
        Ok(target) => target,
        Err(e) => return err("grep", &e),
    };
    let path_refs = scan_path_refs(&paths);
    let (max_matches, limit_was_explicit) = match scan_result_limit(body, GREP_LIMIT_FIELDS, GREP_DEFAULT_MAX_MATCHES) {
        Ok(limit) => limit,
        Err(e) => return err("grep", &e),
    };
    let scan_cap = match (limit_was_explicit, output_mode) {
        (true, _) | (false, "content") => max_matches,
        _ => GREP_NON_CONTENT_SCAN_CAP,
    };
    let fixed_strings = body
        .get("fixed_strings")
        .and_then(|v| v.as_bool())
        .or_else(|| body.get("fixedStrings").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let regex_asked = body.get("regex").and_then(|v| v.as_bool());
    let auto_trigger = match (regex_asked, fixed_strings) {
        (None, false) => grep_regex_trigger(pattern),
        _ => None,
    };
    let use_regex = match regex_asked {
        Some(want) => want && !fixed_strings,
        None => auto_trigger.is_some(),
    };
    let context = match body.get("context").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => 0,
    };
    let (include_globs, exclude_globs) = match grep_route_globs(body) {
        Ok(globs) => globs,
        Err(e) => return err("grep", &e),
    };
    let scan = crate::code_index::LiteralScan {
        pattern,
        root,
        paths: &path_refs,
        regex: use_regex,
        case_insensitive: body
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .or_else(|| body.get("ignore_case").and_then(|v| v.as_bool()))
            .unwrap_or(false),
        whole_word: body
            .get("whole_word")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        comments_only: false,
        include_globs,
        exclude_globs,
        max_matches: scan_cap as usize,
        max_files: body
            .get("max_files")
            .and_then(|v| v.as_u64())
            .unwrap_or(crate::code_index::LITERAL_SCAN_MAX_FILES as u64)
            as usize,
        context,
        term_combination: Some("phrase"),
        budget_ms: None,
        max_matches_per_file: None,
        refresh: scan_refresh_requested(body),
        no_ignore: scan_no_ignore_requested(body),
        output: crate::code_index::ScanOutput::Matches,
        list_limit: None,
        max_chars: usize::MAX,
        spill_name: String::new(),
        verbose: false,
    };
    let scanned = crate::code_index::scan_literal(&scan, &cfg);
    if scanned.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        let base = scanned.get("error").and_then(|e| e.as_str()).unwrap_or("grep scan failed").to_string();
        // "error_kind":"pattern" is the matcher's own rejection. Anything else -- a scope that does
        // not exist, a path outside the root -- says nothing about how the pattern was read, and
        // appending the regex-detected note to it reads as "your path was compiled as a regex".
        let pattern_rejected = scanned.get("error_kind").and_then(|k| k.as_str()) == Some("pattern");
        return match auto_trigger.filter(|_| pattern_rejected) {
            Some(reason) => err("grep", &format!(
                "{base} -- the pattern was read as a regex because of its {reason}; \
                 pass \"regex\": false (or \"fixed_strings\": true) to search for it literally"
                ),
            ),
            None => err("grep", &base),
        };
    }
    let mut out = match scanned {
        Value::Object(map) => map,
        other => return ok("grep", other),
    };
    let matches: Vec<Value> = out
        .get("matches")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let counts = grep_counted_files(&matches);
    out.insert("output_mode".to_string(), json!(output_mode));
    out.insert("regex".to_string(), json!(use_regex));
    if let Some(reason) = auto_trigger {
        out.insert("regex_detected".to_string(), json!(reason));
    }
    out.insert("file_count".to_string(), json!(counts.len()));
    out.insert("max_matches".to_string(), json!(scan_cap));
    out.insert("context".to_string(), json!(context));
    match output_mode {
        "files_with_matches" => {
            out.remove("matches");
            out.insert("counts".to_string(), Value::Array(counts.clone()));
            out.insert(
                "output".to_string(),
                Value::Array(counts.iter().map(|c| json!(grep_hit_path(c))).collect()),
            );
        }
        "count" => {
            out.remove("matches");
            out.insert("counts".to_string(), Value::Array(counts.clone()));
            out.insert(
                "output".to_string(),
                Value::Array(
                    counts
                        .iter()
                        .map(|c| json!(grep_output_line(c, "count")))
                        .collect(),
                ),
            );
        }
        _ => {
            out.insert("counts".to_string(), Value::Array(counts));
            if body
                .get("detail")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                let want_columns = body
                    .get("columns")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                out.insert(
                    "matches".to_string(),
                    Value::Array(grep_shaped_matches(matches, want_columns)),
                );
            } else {
                out.remove("matches");
                out.insert("output".to_string(), Value::Array(
                    crate::code_index::grep_content_lines(&matches, context, false).into_iter().map(|line| json!(line)).collect(),
                ));
            }
        }
    }
    let partial = finish_scan_reply(&mut out, scan_cap);
    answer_scan("grep", out, partial)
}

pub(super) fn answer_scan(verb: &str, out: serde_json::Map<String, Value>, partial: Option<String>) -> u64 {
    match partial {
        Some(reason) => ok_partial(verb, Value::Object(out), &reason),
        None => ok(verb, Value::Object(out)),
    }
}

pub(super) fn grep_comments(body: &Value, cfg: &crate::ragconfig::RagConfig) -> u64 {
    let (root, paths) = match resolve_scan_target(body) {
        Ok(target) => target,
        Err(e) => return err("grep", &e),
    };
    let path_refs = scan_path_refs(&paths);
    let (max_matches, _) = match scan_result_limit(body, GREP_LIMIT_FIELDS, GREP_DEFAULT_MAX_MATCHES) {
        Ok(limit) => limit,
        Err(e) => return err("grep", &e),
    };
    let (_, exclude_globs) = match grep_route_globs(body) {
        Ok(globs) => globs,
        Err(e) => return err("grep", &e),
    };
    let context = match body.get("context").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => 0,
    };
    let scan = crate::code_index::CommentScan {
        root,
        paths: &path_refs,
        path_glob: body.get("glob").and_then(|v| v.as_str())
            .or_else(|| body.get("include").and_then(|v| v.as_str()))
            .or_else(|| body.get("path_glob").and_then(|v| v.as_str()))
            .filter(|g| !g.is_empty()),
        exclude_globs,
        max_matches: max_matches as usize,
        max_files: body.get("max_files").and_then(|v| v.as_u64())
            .unwrap_or(crate::code_index::LITERAL_SCAN_MAX_FILES as u64) as usize,
        context,
        refresh: scan_refresh_requested(body),
        no_ignore: scan_no_ignore_requested(body),
    };
    let scanned = crate::code_index::scan_comments(&scan, cfg);
    if scanned.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        let base = scanned
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("comment scan failed")
            .to_string();
        return err("grep", &base);
    }
    let mut out = match scanned {
        Value::Object(map) => map,
        other => return ok("grep", other),
    };
    out.insert("verb_mode".to_string(), json!("comments"));
    let partial = finish_scan_reply(&mut out, max_matches);
    answer_scan("grep", out, partial)
}

pub(super) const CODESEARCH_QUERY_SHAPE: &str = "query required -- pass {\"query\":\"<the text to search for>\"}: a plain STRING of text, never an object, array or path list; \"dual\" (the default) embeds it and ranks BM25+vector hits, \"literal\"/\"regex\" match it verbatim, \"filename\" matches it as a path substring or glob. Optional {\"mode\":\"dual\"|\"literal\"|\"regex\"|\"filename\"}, {\"k\":10} result cap for \"dual\", {\"max_matches\":1000} for the exhaustive modes, {\"path\":\"<dir or file>\"}, {\"path_glob\":\"**/*.rs\"}. There is no query-less listing mode, so a body without query is always a caller mistake";

pub(super) struct IdentifierScan<'a> {
    pattern: &'a str,
    regex: bool,
    whole_word: bool,
}

pub(super) fn identifier_scan_lines(scan: &IdentifierScan, body: &Value, root: Option<&str>, include_globs: &[String], exclude_globs: &[String], cfg: &crate::ragconfig::RagConfig) -> Result<(Vec<String>, bool), String> {
    let named = scan_target_paths(body)?;
    let path_refs = scan_path_refs(&named);
    let request = crate::code_index::LiteralScan {
        pattern: scan.pattern,
        root,
        paths: &path_refs,
        regex: scan.regex,
        case_insensitive: false,
        whole_word: scan.whole_word,
        comments_only: false,
        include_globs: include_globs.to_vec(),
        exclude_globs: exclude_globs.to_vec(),
        max_matches: crate::codesearch_rank::IDENTIFIER_SCAN_MAX_MATCHES,
        max_files: crate::code_index::LITERAL_SCAN_MAX_FILES,
        term_combination: None,
        budget_ms: None,
        max_matches_per_file: None,
        context: 0,
        refresh: scan_refresh_requested(body),
        no_ignore: false,
        output: crate::code_index::ScanOutput::Compact,
        list_limit: None,
        max_chars: usize::MAX,
        spill_name: String::new(),
        verbose: false,
    };
    let out = crate::code_index::scan_literal(&request, cfg);
    if out.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        return Err(out
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("identifier scan failed")
            .to_string());
    }
    let lines = out
        .get("matches")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok((
        lines,
        out.get("exhaustive")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
    ))
}

pub(super) fn identifier_route_globs(
    body: &Value,
    include_docs: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut include_globs: Vec<String> = Vec::new();
    let mut exclude_globs: Vec<String> = if include_docs {
        Vec::new()
    } else {
        crate::codesearch_rank::DOC_EXCLUDE_GLOBS
            .iter()
            .map(|g| g.to_string())
            .collect()
    };
    for key in ["path_glob", "glob"] {
        for pattern in glob_patterns_from(body.get(key)).map_err(|e| format!("{key} {e}"))? {
            match pattern.strip_prefix('!') {
                Some(negated) => exclude_globs.push(negated.to_string()),
                None => include_globs.push(pattern),
            }
        }
    }
    for key in ["exclude", "exclude_glob", "exclude_globs"] {
        let patterns = glob_patterns_from(body.get(key)).map_err(|e| format!("{key} {e}"))?;
        exclude_globs.extend(
            patterns
                .into_iter()
                .map(|p| p.strip_prefix('!').map(str::to_string).unwrap_or(p)),
        );
    }
    Ok((include_globs, exclude_globs))
}

pub(super) fn codesearch_identifier(
    body: &Value,
    query: &str,
    root: Option<&str>,
    cfg: &crate::ragconfig::RagConfig,
    opts: &crate::codesearch_rank::RankOptions,
) -> Option<Result<Value, String>> {
    if let Some(r) = root {
        if r != "." && !crate::wasm_dispatch::host_allow_root(r) {
            return None;
        }
    }
    let scan_root = root.filter(|r| *r != "." && *r != "./");
    let (include_globs, exclude_globs) = match identifier_route_globs(body, opts.include_docs) {
        Ok(globs) => globs,
        Err(e) => return Some(Err(e)),
    };
    let substring_pattern = format!("(?i){}", regex::escape(query));
    let passes = [
        (
            IdentifierScan {
                pattern: query,
                regex: false,
                whole_word: true,
            },
            false,
        ),
        (
            IdentifierScan {
                pattern: &substring_pattern,
                regex: true,
                whole_word: false,
            },
            true,
        ),
    ];
    for (scan, substring) in &passes {
        let (lines, complete) =
            match identifier_scan_lines(scan, body, scan_root, &include_globs, &exclude_globs, cfg)
            {
                Ok(found) => found,
                Err(e) => return Some(Err(e)),
            };
        if lines.is_empty() {
            continue;
        }
        let (definition_lines, definitions_complete) = if complete {
            (Vec::new(), true)
        } else {
            let pattern = crate::codesearch_rank::any_definition_pattern(query, *substring);
            let definitions_scan = IdentifierScan {
                pattern: &pattern,
                regex: true,
                whole_word: false,
            };
            match identifier_scan_lines(
                &definitions_scan,
                body,
                scan_root,
                &include_globs,
                &exclude_globs,
                cfg,
            ) {
                Ok(found) => found,
                Err(e) => return Some(Err(e)),
            }
        };
        let scan_lines = crate::codesearch_rank::ScanLines {
            lines: &lines,
            definition_lines: &definition_lines,
            complete,
            definitions_complete,
        };
        if let Some(report) =
            crate::codesearch_rank::identifier_report(query, &scan_lines, opts, *substring)
        {
            return Some(Ok(report));
        }
    }
    None
}

pub(super) fn codesearch(body: &Value) -> u64 {
    let packed = codesearch_dispatch(body);
    let mut v = unpack_to_value(packed);
    if !v.is_object() {
        return packed;
    }
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        if let Some(obj) = v.as_object_mut() {
            obj.entry("modes".to_string()).or_insert_with(|| json!({
                "literal": "exhaustive path:line substring search -- pass mode:\"literal\" when you can name the symbol or string exactly",
                "regex": "the same, with a regex query",
                "filename": "path-only match",
                "dual": "ranked BM25+vector retrieval (the default)"
            }));
        }
    }
    pack(v.to_string())
}

pub(super) fn codesearch_comments_requested(body: &Value) -> bool {
    body.get("mode").and_then(|v| v.as_str()) == Some("comments")
        || body.get("comments").and_then(|v| v.as_bool()).unwrap_or(false)
        || body.get("comments_only").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub(super) fn codesearch_dispatch(body: &Value) -> u64 {
    let cfg = crate::ragconfig::RagConfig::resolved();
    if codesearch_comments_requested(body) {
        return grep_comments(body, &cfg);
    }
    let Some(raw_query) = body.get("query") else {
        return err_retry_same_verb("codesearch", CODESEARCH_QUERY_SHAPE);
    };
    let Some(query) = raw_query.as_str() else {
        let shown: String = raw_query.to_string().chars().take(80).collect();
        return err_retry_same_verb(
            "codesearch",
            &format!(
                "{}; got non-string JSON under \"query\": {}",
                CODESEARCH_QUERY_SHAPE, shown
            ),
        );
    };
    if query.is_empty() {
        return err_retry_same_verb("codesearch", CODESEARCH_QUERY_SHAPE);
    }
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("dual");
    if !CODESEARCH_MODES.contains(&mode) {
        return err("codesearch", &format!(
            "mode \"{}\" is not a codesearch mode -- valid modes are {}. \
             \"literal\"/\"regex\" are exhaustive: every match with path:line, no ranking, no top-k. \
             \"dual\" is ranked BM25+vector retrieval. \"filename\" matches paths only. \
             An unrecognised mode is refused here rather than served as \"dual\", \
             which is what used to happen.",
            mode,
            CODESEARCH_MODES.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", "),
        ));
    }
    let (k, limit_was_explicit) = match codesearch_result_limit(body, &cfg) {
        Ok(v) => v,
        Err(e) => return err("codesearch", &e),
    };
    if mode == "literal" || mode == "regex" || mode == "filename" {
        if let Some(refusal) =
            refuse_unknown_fields("codesearch", body, CODESEARCH_EXHAUSTIVE_FIELDS)
        {
            return refusal;
        }
        let explicit = if limit_was_explicit { Some(k) } else { None };
        return codesearch_exhaustive(body, query, mode == "regex", &cfg, explicit);
    }
    let root = scan_root(body);
    let rank_opts = crate::codesearch_rank::RankOptions::from_body(body, k as usize, root);
    let identifier = query.trim();
    let flag = |name: &str| body.get(name).and_then(|v| v.as_bool()).unwrap_or(false);
    let dataflow_override_active = crate::dataflow::document_detailed().1
        != crate::dataflow::DataflowTier::CompiledDefault
        && crate::dataflow::pipeline_for("codesearch").is_some();
    let identifier_route = mode == "dual"
        && !rank_opts.verbose
        && !flag("rebuild")
        && !flag("auto_indexed")
        && !dataflow_override_active
        && crate::codesearch_rank::is_identifier(identifier);
    if identifier_route {
        match codesearch_identifier(body, identifier, root, &cfg, &rank_opts) {
            Some(Ok(report)) => return ok("codesearch", report),
            Some(Err(e)) => return err("codesearch", &e),
            None => {}
        }
    }
    if let Some(root) = root {
        return codesearch_at_root(body, root, query, k, &cfg);
    }
    let (_dataflow_doc, dataflow_tier, dataflow_path) = crate::dataflow::document_detailed();
    if dataflow_tier != crate::dataflow::DataflowTier::CompiledDefault {
        if let Some(pipeline) = crate::dataflow::pipeline_for("codesearch") {
            emit_event(
                "dataflow_pipeline_override_used",
                json!({
                    "entry_point": "codesearch", "tier": dataflow_tier.as_str(), "path": dataflow_path,
                }),
            );
            let cand_k = cfg.budget.pool(k as usize).max(50) as u32;
            let mut request = body.clone();
            if let Some(obj) = request.as_object_mut() {
                obj.insert("code_namespace".to_string(), json!(cfg.namespaces.code));
                obj.insert("k".to_string(), json!(k));
                obj.insert("cand_k".to_string(), json!(cand_k));
            }
            let out = crate::dataflow_exec::run(&pipeline, request);
            return ok("codesearch", out);
        }
    }
    if body
        .get("rebuild")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && !body
            .get("auto_indexed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        let cleared = crate::code_index::clear_codeinsight_full_cfg(&cfg);
        emit_event(
            "codeinsight_rebuild",
            json!({ "reason": "explicit-rebuild", "keys_cleared": cleared }),
        );
        let _ = crate::code_index::index_topup(
            ".",
            cfg.index.prune_pass_file_limit_ceiling,
            COLD_INDEX_PASS_BUDGET_MS,
        );
        let mut retry = body.clone();
        if let Some(obj) = retry.as_object_mut() {
            obj.insert("auto_indexed".to_string(), Value::Bool(true));
            obj.insert("rebuild".to_string(), Value::Bool(false));
        }
        return codesearch_dispatch(&retry);
    }
    let already_indexed = body
        .get("auto_indexed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !already_indexed {
        let stored = crate::code_index::stored_digest();
        let current = crate::code_index::current_digest();
        let stale = match &stored { Some(s) => s != &current, None => true };
        if stale && crate::code_index::topup_allowed(&stored, None) {
            let cold_start = stored.is_none();
            let reason = if cold_start {
                "digest-absent"
            } else {
                "digest-mismatch"
            };
            emit_event(
                "codeinsight_rebuild",
                json!({ "reason": reason, "stored_then_current": current }),
            );
            if cold_start {
                let _ = crate::code_index::index_topup(
                    ".",
                    cfg.index.prune_pass_file_limit_ceiling,
                    COLD_INDEX_PASS_BUDGET_MS,
                );
            } else {
                let _ = crate::code_index::index_topup(
                    ".",
                    cfg.index.prune_pass_file_limit_ceiling,
                    cfg.index.incremental_topup_wall_budget_ms,
                );
            }
            let mut retry = body.clone();
            if let Some(obj) = retry.as_object_mut() {
                obj.insert("auto_indexed".to_string(), Value::Bool(true));
            }
            return codesearch_dispatch(&retry);
        }
    }
    let cand_k = cfg.budget.pool(k as usize).max(50) as u32;
    let embedding = embed_query(query);
    let code_ns = cfg.namespaces.code.as_str();
    let vec_hits = vec_search_local(&embedding, code_ns, cand_k);
    let vec_ids: Vec<String> = vec_hits
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| h.get("key").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut corpus = crate::code_index::FusionCorpus::load();
    let vec_ids: Vec<String> = if vec_ids.is_empty() {
        let vres = crate::code_index::search(query, cand_k as usize, Some(&embedding));
        vres.get("rows")
            .and_then(|r| r.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        let path = r.get("path").and_then(|v| v.as_str())?;
                        let ls = r.get("line_start").and_then(|v| v.as_u64())? as usize;
                        corpus.key_for_path_line(path, ls)
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec_ids
    };
    let bm25_ranked = corpus.bm25_rank_cfg(query, cand_k as usize, &cfg.scoring);
    let bm25_ids: Vec<String> = bm25_ranked.iter().map(|(key, _)| key.clone()).collect();
    let commit_ranked = crate::code_index::git_commit_rank(query, 10);
    let commits: Vec<Value> = commit_ranked
        .iter()
        .map(|(hash, message, score)| json!({ "hash": hash, "message": message, "score": score }))
        .collect();
    let (phrase_hits, phrase_total, phrase_exhaustive) = dual_phrase_hits(query, None, &cfg, scan_no_ignore_requested(body));
    let build_hit = |corpus: &mut crate::code_index::FusionCorpus, key: &str, score: Option<f64>, fallback_text: Option<&str>| -> Value {
        let text = corpus.text_for_key(key)
            .or_else(|| fallback_text.map(String::from))
            .unwrap_or_default();
        let mut obj = serde_json::Map::new();
        obj.insert("key".to_string(), json!(key));
        obj.insert("text".to_string(), json!(text));
        if let Some(s) = score {
            obj.insert("score".to_string(), json!(s));
        }
        if let Some(sym) = corpus.symbol_for_key(key) {
            obj.insert("symbol".to_string(), sym);
        }
        if let Some(ov) = corpus.overview_for_key(key) {
            obj.insert("overview".to_string(), json!(ov));
        }
        Value::Object(obj)
    };
    let shown_k = dual_channel_depth(body, k);
    let vector_ranked: Vec<Value> = vec_hits
        .as_array()
        .filter(|a| !a.is_empty())
        .map(|a| {
            a.iter()
                .take(shown_k)
                .map(|row| with_corpus_symbol(&corpus, row))
                .collect()
        })
        .unwrap_or_else(|| {
            vec_ids
                .iter()
                .take(shown_k)
                .map(|key| build_hit(&mut corpus, key, None, None))
                .collect()
        });
    let bm25_ranked_response: Vec<Value> = bm25_ranked
        .iter()
        .take(shown_k)
        .map(|(key, score)| build_hit(&mut corpus, key, Some(*score), None))
        .collect();
    if !vec_ids.is_empty() || !bm25_ids.is_empty() || !phrase_hits.is_empty() {
        return ok(
            "codesearch",
            compact_dual_reply(
                body,
                query,
                k,
                json!({
                    "mode": "dual",
                    "vector_hits": vector_ranked,
                    "bm25_hits": bm25_ranked_response,
                    "phrase_hits": phrase_hits,
                    "phrase_hits_total": phrase_total,
                    "phrase_hits_truncated": !phrase_exhaustive || phrase_total > phrase_hits.len() as u64,
                    "commits": commits,
                }),
            ),
        );
    }
    let ns = cfg.namespaces.code.as_str();
    let packed = unsafe {
        host_kv_query(
            ns.as_ptr(),
            ns.len() as u32,
            query.as_ptr(),
            query.len() as u32,
        )
    };
    let hits = unpack_to_value(packed);
    let kv_empty = hits.is_null() || hits.as_array().map(|a| a.is_empty()).unwrap_or(true);
    if kv_empty
        && !body
            .get("auto_indexed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        let _ = crate::code_index::index_topup(
            ".",
            cfg.index.prune_pass_file_limit_ceiling,
            COLD_INDEX_PASS_BUDGET_MS,
        );
        let mut retry = body.clone();
        if let Some(obj) = retry.as_object_mut() {
            obj.insert("auto_indexed".to_string(), Value::Bool(true));
        }
        return codesearch_dispatch(&retry);
    }
    let vec_unavailable = vector_ranked.is_empty();
    let kv_empty_now = hits.is_null() || hits.as_array().map(|a| a.is_empty()).unwrap_or(true);

    if vec_unavailable {
        emit_event(
            "codesearch_degraded",
            json!({
                "namespace": cfg.namespaces.code,
                "kv_hits": hits.as_array().map(|a| a.len()).unwrap_or(0),
            }),
        );
    }

    if vec_unavailable && kv_empty_now && phrase_hits.is_empty() {
        return err(
            "codesearch",
            "semantic retrieval unavailable (no vector hits and the embedder produced nothing) and the keyword fallback matched nothing -- this is NOT an empty-index result",
        );
    }

    ok(
        "codesearch",
        compact_dual_reply(
            body,
            query,
            k,
            json!({
                "mode": "fallback_kv", "degraded": vec_unavailable,
                "hits": hits, "commits": commits, "vector_hits": vector_ranked, "bm25_hits": bm25_ranked_response,
                "phrase_hits": phrase_hits,
                "phrase_hits_total": phrase_total,
                "phrase_hits_truncated": !phrase_exhaustive || phrase_total > phrase_hits.len() as u64,
            }),
        ),
    )
}

