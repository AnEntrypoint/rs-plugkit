# AGENTS.md

Module-scoped rules for the sources in this directory. The [root AGENTS.md](../../../AGENTS.md) owns the ownership map and the delivery rules.

## Indexing and search

### Code index (`code_index.rs`)

- In-instance caches key on the dispatch project (one wasm instance serves every project): `project_scoped_cache_key` folds `host_cwd` in when no explicit root is given. Per-root KV namespaces are salted by `root_ns_suffix`; the no-root namespace stays unsalted.
- `SKIP_FILE_SUFFIXES` (`.rlib`, `.rmeta`, `.pdb`) excludes build output outside directories named exactly `target`.
- `BUILD_OUTPUT_DIR_SEGMENTS` marks output only by convention: `is_skipped_dir_path` keeps such a segment under a `SOURCE_DIR_SEGMENTS` ancestor (`src/static` is source, `static` at a project root is output). Configured `extra_skip_dirs` stay absolute.
- Schema setup (`ensure_schema_at_cfg`, `rssearch_vectors`, `git_commit_vectors`) drops a mismatched-width table before `CREATE TABLE IF NOT EXISTS`; the `code_chunks` path index is created after `drop_if_dim_mismatch_cfg`.
- `parse_manifest` accepts `MIN_READABLE_MANIFEST_VERSION..=MANIFEST_VERSION`; a parse failure goes to `purge_stale_manifest_row`. New manifest fields stay optional.
- `FileManifest::digest_hash`, including the stat-only fast path, must record the per-file value `current_digest()` folds, and `current_digest_cfg` must apply the indexer's size cap. Per-file `(mtime_bits, size) -> hash` pairs persist to `.gm/exec-spool/.codeinsight-file-digests.json`.
- Over-budget passes store `<digest>:partial=N`, which never equals a fresh digest so the next dispatch resumes, and must still be written. A tree that never fits re-runs every dispatch until `IndexConfig::wall_budget_ms` or `MemorySyncBudgetConfig` is raised; `topup_allowed` admits a partial tree once per `PARTIAL_TOPUP_MIN_INTERVAL_MS` per project.
- `index_cfg_impl` visits the whole sorted listing each pass, starting at `.gm/exec-spool/.codeinsight-cursor` and wrapping; `max_files` bounds fresh extraction, never the listing.
- Per-file chunk caps bound fresh embeddings, not retained chunks: reusable embeddings are kept, at most `cap` new ones are embedded, the rest count into `skipped_no_embed`. Manifests older than `FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS` re-extract once.
- Charge the fresh-file allowance and one-file grace only once extraction shows a fresh embedding is needed; `pessimistic_ms_per_chunk` derives the allowance from worst-case wasm embedding cost.
- libsql: an unfiltered `COUNT(*)` or `COUNT(DISTINCT ...)` over an `F32_BLOB` table returns 0; count vector rows through a `GROUP BY` subquery. Call edges are one KV row per file (`cef-<crc32(path)>` in `<code_ns>-edges-by-file`).
- `WINDOWS_RESERVED_DEVICE_NAMES` are listed by `readdir` but Win32 refuses to stat or open them, so an entry that lists and does not stat is skipped, never fatal. The pre-filter is Win32-only (`host_paths_use_windows_syntax`).
- A walk that outlives its wall budget keeps what it listed and reports `complete: false` (`collect_files_within_wall_budget`).
- BM25 counts a chunk's term frequencies only for the query's terms; `BM25_TF_BODY_CAP_BYTES` (8192) caps a chunk's body.

### Literal scan (`scan_literal`, `scan_universe.rs`)

- `scan_literal` skips digest, index, embedding, vector and fusion work entirely. Its limits `LITERAL_SCAN_MAX_FILES` and `LITERAL_SCAN_MAX_FILE_BYTES` are independent of `IndexConfig::digest_max_files` and `max_file_bytes`.
- Any coverage-affecting bound, unreadable file or code-rule exclusion (`excluded_by_rule`, every rule except `gm_state_dir` and `agentplug_kv_cache`) clears `exhaustive`; `files_skipped_binary` never does. The per-path listing rides only when a rule dropped code (`insert_excluded_by_rule`, `GM_STATE_EXCLUSION_RULES`). `finish_scan_reply` makes that call, not `scan_literal`.
- `host_read` returns `None` for both I/O failure and non-UTF-8; `host_stat` separates them (stat ok = binary skip, stat failed = unreadable gap). Request `file_cap + 1` entries from `list_scan_universe`.
- Unicode lowercasing can change byte width (U+0130); `LiteralMatcher::find_all` then falls back to a char-aligned scan and returns every match per line.
- A caller-named scope (`path`, `paths`, `root`) beats every exclusion rule: `named_scope` short-circuits `exclusion_rule` and skips `prune_own_state`; `path` is a literal path or glob, never a regex.
- An own-state exclusion names the `.gm/<child>` entry holding the pruned file (`reported_own_state_entry_under_root`). An ignored untracked own-state file never reaches `prune_own_state` (`--others --exclude-standard` omits it), so `report_unlisted_own_state` names those from disk; a `.gm/` file git lists is project content (`is_versioned_gm_state`).
- A gitlink (mode `160000` in `ls-files --stage`) splits the two listings: `--cached --recurse-submodules` reaches a submodule's tracked files while `--others` stops at the gitlink. `gitlink_dirs` names those directories and `nested_untracked` re-runs `--others` inside each, recursing to `NESTED_REPO_DEPTH_LIMIT`.
- `no_ignore` is `--others` without `--exclude-standard`, so it returns tracked, untracked and ignored files together; `.git` is not a worktree entry either way.
- `walk_every_file_in_scope: false` stops once `max_matches` rows are held; `true` covers every file in scope.
- Exhaustive rows are shared across matching files (`EXHAUSTIVE_SCAN_MIN_ROWS_PER_FILE`): `hit_cap` bounds what is held, `max_matches` stays the reply size.
- A caller-named `max_matches_per_file` is a bounded sample and stops reading the file; the implied quota only stops collecting. A reply cut to `max_matches` rows is still `exhaustive`, with `matches_truncated` and `lines_with_matches` reporting the cut.
- A zero-match reply carries the coverage fields unasked (`matched_nothing_needs_coverage_fields`).
- Multi-term split: a regex carrying a metacharacter stays one matcher, so `yama|ptrace_scope` is not chopped into fragments; pieces shorter than `MIN_QUERY_TERM_CHARS` are reported only when the split happened. Split-form: comma-separated quoted arguments spell one path (as in `join(ROOT, 'apps', 'world', '_fixtures', 'e2e-ci-arena.js')`), and the last segment also matches with an extension; past `SPLIT_FORM_HITS_INLINED_PER_REPLY` the reply sets `split_form_truncated` and clears `exhaustive`.

### Code symbols (`code_symbols.rs`)

- Structural queries run a bounded refresh without embeddings and refuse incomplete graph evidence. Schema 3 records a current-byte FNV64 `source_hash`, so same-size, same-mtime edits invalidate reuse.
- Symbols, metrics and raw import specs live in `code_symbols`, `code_symbol_files` and `code_imports`, not vector chunks; multi-row inserts stay within SQLite's 999-parameter bound.
- JSON has no structural symbol support: scoped queries reject it, and observed JSON clears prior symbols, imports and edges. Cleanup failures make coverage incomplete.
- Resolved `index.max_file_bytes` has a 2 MiB minimum after typed overlay; `index.max_chunks_per_file_per_pass` bounds vector work only, `0` keeps BM25 text and manifests.
- Import specs resolve at query time; only resolved indexed targets form edges. Edges are unqualified callee names: callers, callees, impact and tests reject scopes they cannot represent, and `impact` does not expand same-name ambiguity unless `through_ambiguous` is explicit.

### Search dispatch and retention (`ragconfig.rs`, `codesearch`)

- A search that could not see every file still answers `ok: true`: completeness rides in `partial`/`partial_reason`.
- `digest_max_files` and `prune_enumeration_file_cap` bound visibility as well as work. Git-history sync has an elapsed hard ceiling independent of its embed floor, and cheap `--shortstat` chooses subject-only embedding above `git_commit_full_diff_max_changed_lines` or `_max_files`. `RetentionConfig` reclaims only already-tombstoned data.
- `codesearch_exhaustive` (literal and regex) routes before the root branch and every digest, index and embedding step. Root-scoped retrieval (`codesearch_at_root`) never uses cwd-bound corpus, fusion or dataflow state.
- `CODESEARCH_MODES` (`dual`, `literal`, `regex`, `filename`), `CODESEARCH_LIMIT_FIELDS` and `codesearch_result_limit` reject unknown modes and ignored limits; only an explicit limit bounds an exhaustive request.
- `.gm/index-config.json` is an overlay outside the configuration tiers; a project-vendored tier replaces lower tiers wholesale, overlay lists append.
- Ranked `dual` loses a verbatim phrase, so an exhaustive phrase scan runs inside `DUAL_PHRASE_SCAN_BUDGET_MS` (6 s). Its reply reports `phrase_match_count`, the lines the phrase matched, never the length of the returned rows; hits are fair-shared by `DUAL_PHRASE_SCAN_PER_FILE_MATCHES` (8) and `DUAL_PHRASE_SCAN_COLLECT_MATCHES`.
- A first dispatch on a tree with no digest runs one index pass inside `COLD_INDEX_PASS_BUDGET_MS` (30 s) and recurses; `stage_ms` sums across recursion levels.
- `scan_root` reads `root`, then `projectPath`, then `cwd`. The host refuses a subdirectory of a marked project as a scan root, so `marked_ancestor_for_root` rewrites it onto the granted ancestor and `scope_under_marked_ancestor` carries the remainder as `path`. A nested scan's `scan_cache`, `phase_ms`, `files_listed`, `files_unreadable` and `files_with_nul_scanned` are dropped before the outer reply (`SCAN_TELEMETRY_DROPPED`); `excluded_by_rule*` stays.
- A grep pattern is read as a regex only when it carries a construct nobody means literally: an alternation bar, a `\d`-shaped class escape, a `[a-z]` range or an edge anchor. A doubled `||` stays literal; `"regex"`/`"fixed_strings"` skip the detection, and its note rides only on `error_kind:"pattern"`. `split_form_not_searched` names the gap on `regex` and ranked `dual`.

## Cache and memory

- `cache::get` distinguishes store failure from a miss and never overwrites on an unanswered lookup; expiry filters in the SELECT before the sweep, and the libsql error is preserved for corruption classification.
- `shared_db::shared_exec_params` binds every parameter as text and writes return no row count; SQL NULL travels as `''`, unwrapped by `NULLIF(?6,'')`, and `invalidate` checks existence through `get` first.
- Embedding generation markers are per table (`embed_marker::marker_rel_for_table`). `vector_table()` and `rssearch_vectors` must resolve configurable table names identically; the code namespace belongs to the tree-sitter indexer, so the sync pass skips it.
- `keyword_scan` (`KEYWORD_SCAN_MAX_FILES` = 2000, sorted by name) needs neither embedder nor libsql; the flat-KV variant no-ops without a libsql slot.
- A merely deferred sync stores its digest tagged `:partial=N`, or `has_stored_digest` stays false forever; never store a digest when `failed>0` or `rekeyed>0`. Orphan pruning requires full convergence. Explicit-key `memorize-prune` marks the index before removing the file; never read failed semantic retrieval as an empty namespace.

## Dependency scanning (`scan_deps.rs`)

- `find_suspicious_escapes` requires at least four Unicode escapes decoding to an identifier; `count_hex_obfuscator_idents` covers escape-free `_0x` obfuscation. A size ratio alone warns, never fails.
- Package signatures combine maximum mtime and summed bytes (directory mtime misses in-place writes); `walk_package` follows the real dependency tree with containment and visited guards.
- A failed read after a positive-size stat is a blocked read, not empty content; failed or blocked packages are never stamped clean, truncated scans are never complete, and `full:true` clears the stamp before walking. Signatures live in `.gm/exec-spool/.scan-deps-stamp.json`; legacy `.gm/scan-deps-stamp.json` is read-only.

