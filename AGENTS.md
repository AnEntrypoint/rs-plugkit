# AGENTS.md

`plugkit-core` is gm's host-agnostic WASM guest: orchestration, spool verbs, code search and memory live in `crates/plugkit-core/src/`. `agentplug-runner` is the supported host. Follow the parent [AGENTS.md](../AGENTS.md) and the gm skill; [README.md](README.md) owns architecture, verb inventory, spool ABI and release details.

## Work and verification

- Use GM MCP and the served workflow. Structural questions start with `callers`/`impact`, then `codesearch`. Parallel agents get distinct spool session IDs and disjoint writer ownership.
- Source carries no comments except attributes, unsafe `SAFETY` rationale and license headers. Keep only non-obvious current constraints here. UTF-8, no BOM, no decorative glyphs.
- No synthetic test code, fixtures, mocks or frameworks; a fuzz target driving the real spool-JSON and config-path parsers is permitted. Verify by a real build and a live dispatch through the owning verb and supported host, reading actual output; documentation-only changes need factual, scope and whitespace checks.
- Build `cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim`; lint `cargo clippy`; `cargo check -p rs-plugkit --offline` also compiles native-only branches. Parser changes need live malformed and boundary inputs (`fsm-validate` reaches `FsmGraph::validate`). Config and prose resolution has no validate-only verb: use a real configuration and the resolving verb.
- A verb ships as five places together: handler, `wasm_dispatch/verbs.rs` route, gates and transitions, the parent verb contract, the README inventory. Push scoped changes to `main` through GM git verbs; the signed release workflow owns version bumps.
- Keep this file below 30,000 bytes. Compact only after revalidating it against source, history and retained notes.

## Indexing and search

### Code index (`code_index.rs`)

- One wasm instance serves every project the daemon knows, so in-instance caches key on the dispatch project: `project_scoped_cache_key` folds `host_cwd` in when no explicit root is given. Per-root KV namespaces are salted by `root_ns_suffix`; the no-root namespace stays unsalted.
- `SKIP_FILE_SUFFIXES` (`.rlib`, `.rmeta`, `.pdb`) excludes build output outside directories named exactly `target`.
- Names in `BUILD_OUTPUT_DIR_SEGMENTS` mark output only by convention, so `is_skipped_dir_path` keeps such a segment under a `SOURCE_DIR_SEGMENTS` ancestor: `src/static` is source, `static` at a project root is output. Configured `extra_skip_dirs` stay absolute.
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
- BM25 counts a chunk's term frequencies only for the query's terms; keeping every term per chunk held ~5.3M entries (~300MB wasm heap) for at most `q_tokens.len()` of them. `BM25_TF_BODY_CAP_BYTES` (8192) caps a chunk's body.

### Literal scan (`scan_literal`, `scan_universe.rs`)

- `scan_literal` skips digest, index, embedding, vector and fusion work entirely (routing a workspace literal query through them measured 120-420 s). Its limits `LITERAL_SCAN_MAX_FILES` and `LITERAL_SCAN_MAX_FILE_BYTES` are independent of `IndexConfig::digest_max_files` and `max_file_bytes`.
- Any coverage-affecting bound, unreadable file or code-rule exclusion (`excluded_by_rule`, every rule except `gm_state_dir` and `agentplug_kv_cache`) clears `exhaustive`; `files_skipped_binary` never does, and the per-path listing rides only when a rule dropped code (`insert_excluded_by_rule`, `GM_STATE_EXCLUSION_RULES`). `finish_scan_reply` makes that call, not `scan_literal`.
- `host_read` returns `None` for both I/O failure and non-UTF-8; `host_stat` separates them (stat ok = binary skip, stat failed = unreadable gap). Request `file_cap + 1` entries from `list_scan_universe`.
- Unicode lowercasing can change byte width (U+0130); `LiteralMatcher::find_all` then falls back to a char-aligned scan and returns every match per line.
- A caller-named scope (`path`, `paths`, `root`) beats every exclusion rule: `named_scope` short-circuits `exclusion_rule` and skips `prune_own_state`; `path` is a literal path or glob, never a regex.
- An own-state exclusion names the `.gm/<child>` entry holding the pruned file (`reported_own_state_entry_under_root`). An ignored untracked own-state file never reaches `prune_own_state` (`--others --exclude-standard` omits it), so `report_unlisted_own_state` names those from disk; a `.gm/` file git lists is project content (`is_versioned_gm_state`).
- A gitlink (mode `160000` in `ls-files --stage`) splits the two listings: `--cached --recurse-submodules` reaches a submodule's tracked files while `--others` stops at the gitlink. `gitlink_dirs` names those directories and `nested_untracked` re-runs `--others` inside each, recursing to `NESTED_REPO_DEPTH_LIMIT`.
- `no_ignore` is `--others` without `--exclude-standard`, the only flag keeping ignored files out, so it returns tracked, untracked and ignored together; `.git` is not a worktree entry either way.
- `walk_every_file_in_scope: false` stops the walk once it holds `max_matches` rows; `true` covers every file in scope and lets `max_matches` size the reply.
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

## Embedding, vectors and host boundaries

- `slim` omits compiled-in safetensors and requires host `host_vec_embed`; keep candle on the validated 0.8 WASM-compatible line unless the target build proves a replacement.
- A model-width change updates weights, `EMBED_DIM`, `bge_small_config().hidden_size`, `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together; `memory_md::flat_vec_embedding` checks `cfg.dim()`.
- BGE queries carry `BGE_QUERY_PREFIX` via `condition_query`, passages do not; embedding cache slots compare the stored full key, though the slot name is an `fnv1a64` hash.
- `host_vec_embed` and `try_sibling_plugin_embed` are distinct host routes to one BERT model. `build.rs` embeds `PLUGKIT_SOURCE_SHA`; fix `PLUGKIT_BUILD_SHA` for byte-for-byte A/B comparisons.
- `crawl_cdp` is a required host import, so guest and runner ship together; `crawl` takes a plain-text body verbatim, `engine=lightpanda` reaches its sibling through `plugin_call_text`.
- `project_path_rejection` guards `fs_read`, `fs_readdir`, `fs_stat` and `fs_write`: read verbs pass `caller_opted_outside_root(body)`, `fs_write` passes `false`, and `READ_ONLY_OUTSIDE_ROOT_VERBS` keeps the opt-in unreachable from writes.
- `allowOutsideRoot` is honored only as literal `true` (aliases `allow_outside_root`, `allowAbsolute`) on the call naming the path; it widens which root a read may address, never whether it may climb out, since `path_has_parent_traversal` rejects any `..` segment.
- The guest guard is half the gate: `agentplug-host`'s `sandboxed_guest_path_with_extra_roots` independently refuses paths outside the project root, user gm root and granted extra roots. `outside_root_read_granted` requests the grant via `host_fs_allow_root` for the path and each ancestor below the drive root; a marked ancestor grants its subtree, other coverage needs an `agentplug` rebuild.
- `host_abi::git_call` turns an async `{pending, token}` envelope into `ok:false` with `async_parked:true`; only `git_step` and `git_poll` call `git_call_async`.
- `porcelain_from` and `porcelain_or_dirty` never turn a git failure into a fake dirty entry; a failure returns partial stdout plus `partial`, `failed` and `skipped_paths`. On Windows MAX_PATH makes git drop entries while exiting 0; those go to `skipped_paths`. `git_call_async` retries once with `-c safe.directory=<repo>` on "dubious ownership", only when that repository contains the cwd.
- `plugin_abi::call` merges `{abi,plugin,verb,body}` over the body's top-level fields, envelope last; null or empty-object replies classify as `PluginNotFound`. `AbiErrorKind` wire strings are frozen: `plugin-not-found`, `verb-not-supported`, `plugin-error`, `timeout`.
- `libsql_wasm::classify_error`: a parsed `ext=`/`rc=` code is authoritative and suppresses text matching; only `ShadowRow` is text-only. `Corrupt` deletes the shared database, so a quoted "malformed" must never trigger it. `retry_on_busy` cannot sleep in the guest, so `BUSY_RETRY_ATTEMPTS` times libsql's 8 s busy timeout must stay under the host deadline.
- `ann_query_sql`: `pool`, not the final `limit`, is both `vector_top_k`'s k and the outer LIMIT. `SCHEMA_ENSURED` and `MIGRATION_COMPLETE` are process-lifetime memos; code that destroys tables calls `forget_ensured_schema` or `forget_migration_complete`.

## Git and filesystem delivery

### Staging and commit (`wasm_dispatch/verbs.rs`, `verbs/git.rs`)

- Scoped `git_commit` and `git_finalize` stage and commit only the caller's pathspecs, including the concurrent-write amend; a scope that stages nothing is refused, and an empty scoped add is never followed by an unscoped commit. Blanket staging needs `add_all: true`.
- `git_pathspec_scope` emits every exclude before the caller's pathspecs: git 2.46 on Windows silently stages nothing for `git add -- <untracked> <exclude>` (exit 0), so an exclude ordered last turns a scoped commit into an unscoped one.
- Every staging path (`git_add`, `git_commit`, `git_finalize`, porcelain probes, `git_push`'s dirty gate) appends `GIT_PROTECTED_PATHSPECS` (`:(top,exclude).gm`, `:(top,exclude).agentplug*`); receipts list them under `excluded`.
- `git_commit` deduplicates by cwd, pre-commit HEAD, message, paths and `amend` within `GIT_COMMIT_DEDUP_TTL_MS` (180000), replaying the real SHA; a replay counts only while HEAD still equals the recorded `sha_full`. `amend:true` refuses `pushed_commit_refused` and `amend_requires_head`, and during an active merge refuses explicit paths and `add_all`.
- `wasm_dispatch/dangling_refs.rs` validates before staging, so a refusal leaves the index unchanged: a dangling target is an existing, untracked, non-ignored file outside the commit's path set (generated output is ignored), and any existing unreadable scoped source blocks a clean scan unless waived.
- `git_status` preserves observed paths, diagnostics, head SHA and branch; failed, parked or skipped status cannot establish clean. `truncated` only shortens the sample lists: `counts`, `by_directory` and `dirty` stay complete, and the default mode's full porcelain listing is in `spill_file`. `git_finalize {paths}` scopes porcelain checks to those paths and pushes by explicit new-HEAD ref.
- `git_push {rev}` never rebases a dirty checkout. A remote-moved rejection names the recovery: `git_pull`, then `git_push {rev:"<sha>"}`; `rev:"HEAD"` is symbolic. A remote 5xx is transient, retried `GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS` times.
- `git_push` publishes a lineage: with no `rev` the source ref defaults to `HEAD` and `exec_git_push_in` sends `git push origin <source_ref>:<branch>`, so every commit on local `main` the remote lacks ships, including another lane's. A source ref that does not resolve to a commit (empty, `HEAD`, `@`, the branch name, a `refs/` or `origin/` ref) is refused while the delta is nonempty, and the reply lists `unnamed_commits` and the `rev` to pass. It is a naming gate, not an ancestry filter: `rev` names the tip and cannot exclude an ancestor. `allow_foreign_commits:true` publishes the tip after the caller read the delta. `git_finalize` always inserts the sha it publishes; `git_commit` never publishes, the push gate is the only publishing surface.
- A nonzero `git_pull` with no conflicts re-fetches and compares HEAD with the tracking ref before trusting the failure. Missing committer identity must name local `user.name`/`user.email`; tooling never configures a global account silently. Git verbs resolve the actual dispatch project and fail loudly outside a repository; `git_log` parses `--pretty=format` on `\u{1f}`.
- Spawning `git` with a command line past ~32 KiB fails on Windows with os error 206, reported as `git_status_incomplete`: exclude pathspecs stop at `GIT_PATHSPEC_EXCLUDE_ARGV_BUDGET_CHARS` (8000).
- `remote_moved` is only a label (any explicit-ref push rejection that is neither a GitHub auth failure nor a transient 5xx), so acting on it needs ancestry: `remote_moved_is_same_branch_advance` requires the remote tip to resolve, differ from the pushed sha, not be an ancestor of it, and share a merge base.

## Configuration, prose and notifications

### config.rs, config_sync.rs, prose.rs

- `update_checkout_in_place` is the fallback when the live checkout cannot be renamed aside; it rewrites files one at a time.
- `resolve_with`: a `ProjectVendored` win still runs lower tiers' `load_repo_tier` for its side effect, so `config_notify::record_change` keeps upstream drift visible.
- `RESOLVE_CACHE` is keyed by `normalize_project_root(host_cwd_string())` with a 2 s TTL; `normalize_project_root` cuts a root inside `<project>/.gm/config-source-cache*` back to `<project>`. Pass a dispatch's resolved FSM graph through `read_state_with_graph` and `set_phase_with_session_with_graph`.
- `RepoSource.repo` is a `config_path::RepoUrl`, parsed once in `parse_source_entry`; `config_sync` never revalidates it. Config repositories come only through approved remote transports: HTTP(S) with nonempty authority.
- Prose keys and source paths are untrusted relative paths: accept only safe components, since an unsafe prose key is terminal.
- `ensure_current` debounces on the last probe time whether or not a checkout exists. Sync state and locks live on disk beside the checkout, never in statics, since the `$HOME` user-tier cache is shared by every project; `try_lock` is a non-recursive `mkdirSync`.
- The host ABI has no rename: `config_sync::rename` and `memory_md::rename_batch` run `fs.renameSync` through `host_exec_js`.
- `prose::read_from_config_repo` reads `config::resolve().cache_dir`, never a hardcoded directory; `read_clean` treats whitespace-only content as absent. `tier3_user_wide_repo`: a missing `.gm/instructions/source.json` falls through to defaults, an explicitly empty file means not configured.

### orchestrator/config_notify.rs

- A change is persisted when recorded and drained onto the next `instruction` response (`update_available`, `discipline_policies`); paths resolve through project-scoped `pkfs` per call.
- Delivery is once per session, not per process, since agents share one plugin instance; each record keeps a `delivered_to` roster, and a missing session buckets under `"(no-session)"`.
- `MAX_RECORDS` (32), `MAX_SUMMARY_ITEMS` (24), `MAX_RECORD_AGE_MS` (24 h) and `MAX_DELIVERED_TO` (64) bound history and rosters, evicting oldest first. A torn store reads as "no pending changes". `record_change` skips no-op SHA changes; delivery is marked during the drain, since no acknowledgement verb exists.

### Instruction serving (`orchestrator/instructions/mod.rs`, `orchestrator/state.rs`)

- `instructions::handle` suppresses prose only when the caller asserts the hash it holds; `.last-instruction-hash-<sid>.json` records what was sent, not what arrived.
- Non-read-only replies carry `reply_hash` (`.last-instruction-reply-<sid>.json`): a caller asserting the current instruction hash and a matching `known_reply_hash` gets a delta, with equal fields in `unchanged_since_last_reply`, dropped ones in `removed_since_last_reply` and `FIELDS_ALWAYS_RESTATED_IN_A_DELTA_REPLY` inline. Key only on the caller's assertion, never on the server's last write; `{"full":true}` forces the whole envelope, and no `session_id` means no delta.
- `investigate_readonly` instructions serve no phase prose and stamp neither `last-instruction-ts` nor `last-dispatch-ts` (`gates::dispatch_serves_no_phase_prose`).
- Inline PRD and mutable rows are bounded by `instruction_payload.mutables_pending_rows_inlined_limit` and `prd_items_rows_inlined_limit`; counts stay exact and a `*_truncated` block names the file and serving verb.
- `has_compiled_default_for_prose_key` matches exactly the keys `compiled_default_for_prose_key` matches, plus `entry`; unknown keys fall through to ENTRY prose.
- `residual::handle_scan` writes `residual-check-fired` as `<session_id>:<fired_at_ms>`; existence never passes the gate, `yaml_util::invalidate_residual_marker(reason)` is the only clearing path, and a `prd-add` answering `already_identical` must not invalidate.
- `automatic_supply_chain_scan()` runs `scan_deps` on every `instruction`, debounced by `.gm/exec-spool/.last-scan-deps-ts` at `SUPPLY_CHAIN_SCAN_DEBOUNCE_MS` (300000) through `pkfs::write_if_changed`; legacy `.gm/.last-scan-deps-*` files are read-only fallbacks.

### Mutables and PRD

- `mutable-add` requires a nonempty caller `id` and a payload field beyond the envelope; `status` counts as payload, so `{id, status}` reopens a row. `mutables::handle_add` upserts by `id` and collapses duplicates, keeping a resolved row over an unresolved one; `handle_list` stays un-deduplicated. `prd-list` defaults to brief rows; internal evidence and gates use `handle_list_full`.

## Cache and memory

- `cache::get` distinguishes store failure from a miss and never overwrites on an unanswered lookup; expiry filters in the SELECT before the sweep, and the libsql error is preserved for corruption classification.
- `shared_db::shared_exec_params` binds every parameter as text and writes return no row count; SQL NULL travels as `''`, unwrapped by `NULLIF(?6,'')`, and `invalidate` checks existence through `get` first.
- Embedding generation markers are per table (`embed_marker::marker_rel_for_table`). `vector_table()` and `rssearch_vectors` must resolve configurable table names identically; the code namespace belongs to the tree-sitter indexer, so the sync pass skips it.
- `keyword_scan` (`KEYWORD_SCAN_MAX_FILES` = 2000, sorted by name) needs neither embedder nor libsql; the flat-KV keyword scan is libsql-backed and no-ops without a libsql slot.
- A merely deferred sync stores its digest tagged `:partial=N`, or `has_stored_digest` stays false forever; never store a digest when `failed>0` or `rekeyed>0`. Orphan pruning requires full convergence. Explicit-key `memorize-prune` marks the index before removing the file; never read failed semantic retrieval as an empty namespace.

## Disciplines, fibers and calculus

- `capability_proxy::resolve` and `discipline_note::requires_satisfied` must match providers identically: both use `resolve_key_realm` (an empty or self-named realm maps to the discipline realm), require an Active provider and check `provides`.
- `discipline_note::active_policies` is the only caller of discipline `advance_fiber`, once per `instruction`. `all_known_discipline_dirs` includes disabled names with state files, so a disabled discipline advances to Inactive.
- `build_interception_context` folds enabled-file order with right-biased `MergeKind::combine`; for `ScalarOverwrite` the last nonempty declaration wins. `handle_check_removal` is the only writer of `enabled.txt` and CAS-writes against the snapshot it read.
- Guest `fiber_lifecycle::transition` is mirrored arm for arm by agentplug-host `registry.rs` `PluginFiberLifecycle`; the guest has only Inactive, Active and Unloading.
- `calculus::verify_calculus` and `formal/CordisCalculus/` change together; it stops silently at `max_states`, so `ok` covers only explored states. Base-registry unload's `retired || !satisfied` equals the paper's `target != omega`; ExtendedRegistry has no retirement flag.
- `component_loader_dispatch::parse_entries` silently drops entries that fail to deserialize; `isolate` is the tagged `{"kind": "none"|"local"|"global"}` shape; `claim_audit_clean` passes only on the exact body `clean`.

## Dependency scanning (`scan_deps.rs`)

- `find_suspicious_escapes` requires at least four Unicode escapes decoding to an identifier; `count_hex_obfuscator_idents` covers escape-free `_0x` obfuscation. A size ratio alone warns, never fails.
- Package signatures combine maximum mtime and summed bytes, since directory mtime misses in-place writes; `walk_package` follows the real dependency tree with containment and visited guards.
- A failed read after a positive-size stat is a blocked read, not empty content; failed or blocked packages are never stamped clean, truncated scans are never complete, and `full:true` clears the stamp before walking. Signatures live in `.gm/exec-spool/.scan-deps-stamp.json`; legacy `.gm/scan-deps-stamp.json` is read-only.

## Dream-RSI replay

- Replay is frozen-world evidence, not execution or deployment authority: records stay bound to their completed dispatch and the owner session
- Score each world as best attained quality minus `beta1 * summed cost` plus `beta2 * parallelism_bonus`; a policy scores the mean over its worlds, and a challenger wins only with a strict improvement over the incumbent on the same worlds. Betas are explicit finite nonnegative inputs.
- `dream-replay-round` is session-owned progressive reveal from the policy roots and the revealed frontier; it closes at `max_rounds` or `max_nodes`, and `max_online_rounds` bounds new distinct values.
- `dream-replay-cycle` is observation-only maintenance: it uses the canonical owner `session_id`, verifies ledger evidence through `automatic_replay`, defers when no new verified dispatch exists, and never refreshes phase clocks.
- `admit_dispatch` ranks and never refuses: `Admission::Allow` or `Admission::Advisory`, attached to a dispatch that ran (`dream_rsi_advisory`).
- A successful `instruction` clears the ranking by stamping `.gm/dream-rsi/<sid>/reorientation-ts` and the project-wide `.gm/dream-rsi/_any-session/reorientation-ts` (`PROJECT_WIDE_MARKER_SESSION`); the ranking lapses after `VETO_MAX_AGE_MS` (600000). `.gm/last-instruction-ts` is a third marker, but `gates` stamps it only for a prose-serving dispatch.

## Lean graph and compiled prose

- `orchestrator/lean_graph.json` and `orchestrator/instructions/prose/*.md` are vendored copies of `gm-config/fsm/graph.json` and `gm-config/prose/*.md`; `fsm::default_graph()` parses the lean graph and `instructions/lean_prose.rs` maps each key to its file.
- Re-sync: copy both sources from the gm-config commit named by `DEFAULT_REPO_PINNED_SHA`, regenerate `lean_prose.rs` from every `prose/*.md` except `entry` and `entry-extended`, delete prose files gm-config no longer has, keep the pin on that commit.
- A state with `"entry": true` is a graph root and skips the unreachable-state check; a configured graph that fails validation makes `instruction` and `transition` return the error. A gate with `"advisory": true` never blocks; its message appears in the `advisory` array of the `transition` reply.

## Other contracts

- `dataflow::default_document` is not executed for CompiledDefault: configured pipelines run declared steps then fuse nodes, with no topological scheduler, and `plugin == "gm"` calls internal functions, not WASM self-dispatch.
- `legacy_reaper::RETIRED_ARTIFACTS` is an exact allowlist, never a glob and never a memory or database deletion; its hash invalidates the reap marker when the list changes.
- `mediator::SELF_LANG_VERBS` share dispatch code with their base verbs but are not aliases; each keeps its own language for `shell_exec`.
- `submodule_head_sha` skips uninitialized submodule directories without their own `.git`.
