# AGENTS.md

`plugkit-core` is gm's host-agnostic WASM guest: orchestration, spool verbs, code search and memory live in `crates/plugkit-core/src/`. `agentplug-runner` is the supported host. Follow the parent [AGENTS.md](../AGENTS.md) and the gm skill; [README.md](README.md) owns architecture, verb inventory, spool ABI and release details.

## Work and verification

- Use GM MCP and the served workflow. Structural questions start with `callers`/`impact`, then `codesearch`. Parallel agents get distinct spool session IDs and disjoint writer ownership.
- Source carries no comments except attributes, unsafe `SAFETY` rationale and license headers. Keep only non-obvious current constraints here. Text is UTF-8 without BOM and without decorative glyphs.
- No synthetic test code, fixtures, mocks or test frameworks. A fuzz target that drives the real entry point of the spool JSON and config-path parsers is permitted. Verify with a real build and a live dispatch through the owning verb and supported host, reading the actual output. Documentation-only changes need factual, scope and whitespace checks, not a rebuild.
- Build with `cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim`; lint with `cargo clippy`; `cargo check -p rs-plugkit --offline` also compiles native-only branches. Parser changes need live malformed and boundary inputs: `fsm-validate` reaches `FsmGraph::validate`, so exercise invalid structures, not legitimate FSM feedback cycles. Config and prose resolution has no validate-only verb; use a real configuration and the resolving verb. Malformed spool JSON must reach the real parser unrepaired.
- A verb needs its handler, its `wasm_dispatch/verbs.rs` route, gates and transitions, the parent verb contract and the README inventory together. Update both sides of a cross-repository ABI. Push scoped changes to `main` through GM git verbs. The signed release workflow owns version bumps; verify its result before claiming a release.
- Keep this file below 30,000 bytes. Compact only after revalidating it against source, history and retained notes.

## Ownership map

- `lib.rs`, `wasm_dispatch/`: guest entry point and per-verb handlers.
- `orchestrator/`, `gates.rs`: phase graph, obligations, discipline and fiber lifecycle, served prose.
- `code_index.rs`, `code_symbols.rs`, `git_commit_vectors.rs`, `embed.rs`, `vecstore.rs`, `vecns.rs`, `rssearch_vectors.rs`: indexing, symbols, history ranking, embeddings, vector namespaces.
- `memory_md.rs`, `cache.rs`, `shared_db.rs`, `libsql_wasm.rs`: durable memory, cache, database.
- `config.rs`, `config_sync.rs`, `config_path.rs`, `prose.rs`: configuration tiers and bounded source resolution.
- `dataflow*.rs`, `mediator.rs`, `dispatch_ledger.rs`, `pkfs.rs`, `validation.rs`, `gitignore.rs`, `legacy_reaper.rs`, `poll_detect.rs`: pipelines, routing, evidence, project paths, cleanup.

## Indexing and search

### Code index (`code_index.rs`)

- One gm wasm instance serves every project the daemon knows, so any in-instance static cache keys on the dispatch project: `project_scoped_cache_key` folds `host_cwd` in when no explicit root is given. A shared empty key once served one project's BM25 corpus to another.
- Per-root KV namespaces are salted by `root_ns_suffix`, since host KV rows are keyed by namespace string alone. The no-root namespace stays unsalted.
- `SKIP_FILE_SUFFIXES` (`.rlib`, `.rmeta`, `.pdb`) excludes build output outside directories named exactly `target`. Readable symbol names do not make such files source.
- Directory names in `BUILD_OUTPUT_DIR_SEGMENTS` mark build output only by convention, so `is_skipped_dir_path` keeps them when a `SOURCE_DIR_SEGMENTS` ancestor sits above the segment: `src/static` is source, `static` at a project root is output. Pruning `src/static` by name alone left every callers/outline answer for one project's `src/static` empty while literal scan found the same call at `src/static/GLBTransformer.js:64`; a false prune is silent, an indexed output directory is merely noise. Configured `extra_skip_dirs` stay absolute.
- Schema setup (`ensure_schema_at_cfg`, `rssearch_vectors`, `git_commit_vectors`) drops a mismatched-width table before `CREATE TABLE IF NOT EXISTS`, which would silently keep the old one. The `code_chunks` path index is created after `drop_if_dim_mismatch_cfg`, which also removes indexes.
- `parse_manifest` accepts `MIN_READABLE_MANIFEST_VERSION..=MANIFEST_VERSION`; a parse failure goes to `purge_stale_manifest_row`. A strict version check would wipe the cache on every bump, so new manifest fields must be optional.
- `FileManifest::digest_hash`, including the stat-only fast path, must record the per-file value `current_digest()` folds, and `current_digest_cfg` must apply the indexer's size cap; otherwise every dispatch re-indexes. Per-file `(mtime_bits, size) -> hash` pairs persist to `.gm/exec-spool/.codeinsight-file-digests.json`. A hash-match reuse refreshes manifest mtime and size.
- Over-budget passes store `<digest>:partial=N`, which never equals a fresh digest, so the next dispatch resumes. It must still be written; a missing digest forces a full re-index. A tree that never fits re-runs every dispatch until `IndexConfig::wall_budget_ms` or `MemorySyncBudgetConfig` is raised. `topup_allowed` admits a partial tree once per `PARTIAL_TOPUP_MIN_INTERVAL_MS` per project.
- `index_cfg_impl` visits the whole sorted listing each pass, starting at `.gm/exec-spool/.codeinsight-cursor` and wrapping. `max_files` bounds fresh extraction, never the listing: slicing a prefix permanently starves later paths.
- Per-file chunk caps bound fresh embeddings, not retained chunks. Reusable embeddings are kept, at most `cap` new ones are embedded, and the rest count into `skipped_no_embed` for the next pass. Truncating marked a partly embedded file complete forever. Embed failures keep the pass partial. Manifests older than `FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS` re-extract once with embeddings reused.
- Charge the fresh-file allowance and one-file grace only after extraction shows a fresh embedding is needed. `pessimistic_ms_per_chunk` derives the fresh-chunk allowance from measured worst-case wasm embedding cost; lowering it unmeasured can overrun the epoch deadline and poison the Store.
- libsql: an unfiltered `COUNT(*)` or `COUNT(DISTINCT ...)` over an `F32_BLOB` table returns 0. Count vector rows through a `GROUP BY` subquery.
- Call edges are one KV row per file (`cef-<crc32(path)>` in `<code_ns>-edges-by-file`), never per edge: per-edge rows made deletion a full-namespace scan.

### Literal scan (`scan_literal`, `scan_universe.rs`)

- `scan_literal` skips digest, index, embedding, vector and fusion work entirely; routing a workspace literal query through them measured 120-420 s.
- Its limits, `LITERAL_SCAN_MAX_FILES` and `LITERAL_SCAN_MAX_FILE_BYTES`, are independent of `IndexConfig::digest_max_files` and `max_file_bytes`; reusing the digest bounds undercounted a 2427-file tree.
- Any coverage-affecting bound, unreadable file or code rule exclusion (`excluded_by_rule`, every rule except `gm_state_dir` and `agentplug_kv_cache`) clears `exhaustive`; gm's own state is named in `excluded_by_rule_summary` and never does. `files_skipped_binary` never does: a non-text file cannot hold a text match. The reply layer (`finish_scan_reply`) makes that call, not `scan_literal`: the symbol and dual phrase routes read the walk verdict, so an exclusion they cannot recover does not trigger a second definitions scan.
- `host_read` returns `None` for both I/O failure and non-UTF-8. `host_stat` separates them: stat ok means binary skip; stat failed means an unreadable gap.
- Request `file_cap + 1` entries from `list_scan_universe` so an exactly full listing is distinguishable from a truncated one.
- Unicode lowercasing can change byte width (U+0130). `LiteralMatcher::find_all` then falls back to a char-aligned scan, reads match text with `get`, and returns every match per line.
- A caller-named scope (`path`, `paths`, `root`) beats every exclusion rule: `named_scope` short-circuits `exclusion_rule` and skips `prune_own_state`. The rules still govern the unscoped universe. `path` is always a literal path or glob, never a regex.

### Code symbols (`code_symbols.rs`)

- Structural queries run a bounded refresh without embeddings and refuse incomplete graph evidence. Schema 3 records a current-byte FNV64 `source_hash`, so same-size, same-mtime edits invalidate reuse. Bump the schema when stored shape changes.
- Symbols, metrics and raw import specs live in `code_symbols`, `code_symbol_files` and `code_imports`, not vector chunks. Multi-row inserts stay within SQLite's 999-parameter bound.
- JSON has no structural symbol support: scoped queries reject it, and observed JSON clears prior symbols, imports and edges. Cleanup failures make coverage incomplete; line length alone must not defer supported source.
- Resolved `index.max_file_bytes` has a 2 MiB minimum after typed overlay. `index.max_chunks_per_file_per_pass` bounds vector work only; `0` keeps BM25 text and manifests.
- Import specs resolve at query time; only resolved indexed targets form edges. Edges are unqualified callee names, not definition-bound: callers, callees, impact and tests reject scopes they cannot represent, and `impact` does not expand same-name ambiguity unless `through_ambiguous` is explicit.
- `find` escapes `%`, `_` and `\` with `ESCAPE '\'`; stripping them breaks snake_case queries.

### Search dispatch and retention (`ragconfig.rs`, `codesearch`)

- `digest_max_files` and `prune_enumeration_file_cap` bound visibility as well as work. Insufficient limits leave stale chunks while reporting convergence.
- Git-history sync has an elapsed hard ceiling independent of its embed floor. Cheap `--shortstat` chooses subject-only embedding above `git_commit_full_diff_max_changed_lines` or `_max_files`.
- `RetentionConfig` reclaims only already-tombstoned data; pruning live rows is an explicit agent decision.
- `codesearch_exhaustive` (literal and regex) routes before the root branch and every digest, index and embedding step. Root-scoped retrieval (`codesearch_at_root`) never uses cwd-bound corpus, fusion or dataflow state, which belongs to the current project's db and would mix roots.
- `CODESEARCH_MODES` (`dual`, `literal`, `regex`, `filename`), `CODESEARCH_LIMIT_FIELDS` and `codesearch_result_limit` reject unknown modes and ignored limits. Only an explicit limit bounds an exhaustive request. Filename mode applies its path, limit and output controls.
- `.gm/index-config.json` is an overlay outside the configuration tiers. A project-vendored tier replaces lower tiers wholesale; overlay lists append, never replace.

## Embedding, vectors and host boundaries

- `slim` omits compiled-in safetensors and requires host `host_vec_embed`, which the supported runner supplies. Keep candle on the validated 0.8 WASM-compatible line unless the target build proves a replacement.
- A model-width change updates weights, `EMBED_DIM`, `bge_small_config().hidden_size`, `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together. `memory_md::flat_vec_embedding` checks `cfg.dim()`, not a literal.
- BGE queries carry `BGE_QUERY_PREFIX` via `condition_query`; passages do not. Embedding cache slots compare the stored full key, though the slot name is an `fnv1a64` hash.
- `host_vec_embed` and `try_sibling_plugin_embed` are distinct host routes to one BERT model; the sibling route keeps load and model diagnostics.
- `build.rs` embeds `PLUGKIT_SOURCE_SHA`. Fix `PLUGKIT_BUILD_SHA` for byte-for-byte A/B comparisons, since deleting source lines changes panic locations.
- `crawl_cdp` is a required host import: a runner without it cannot instantiate this guest, so guest and runner ship together. `crawl` takes a plain-text body received verbatim. `engine=lightpanda` reaches its sibling through `plugin_call_text`, which passes raw bytes.
- `project_path_rejection` guards `fs_read`, `fs_readdir`, `fs_stat` and `fs_write`. Read verbs pass `caller_opted_outside_root(body)`; `fs_write` passes `false`, and `READ_ONLY_OUTSIDE_ROOT_VERBS` keeps the opt-in unreachable from writes.
- `allowOutsideRoot` is honored only as literal `true` (aliases `allow_outside_root`, `allowAbsolute`) on the call naming the path. It widens which root a read may address, never whether it may climb out: `path_has_parent_traversal` rejects any `..` segment.
- The guest guard is half the gate. `agentplug-host`'s `sandboxed_guest_path_with_extra_roots` independently refuses paths outside the project root, user gm root and granted extra roots. `outside_root_read_granted` requests the grant via `host_fs_allow_root` for the path and each ancestor below the drive root; a marked ancestor grants its subtree. Other non-project coverage needs an `agentplug` rebuild and runner swap.
- `host_abi::git_call` turns an async `{pending, token}` envelope into `ok:false` with `async_parked:true`; only `git_step` and `git_poll` call `git_call_async`. A parked status is never read as clean.
- `porcelain_from` and `porcelain_or_dirty` never turn a git failure into a fake dirty entry; consumers would read it as work and hard-block finalize. A failure returns partial stdout plus `partial`, `failed` and `skipped_paths`. On Windows, MAX_PATH makes git warn and drop entries while exiting 0; those go to `skipped_paths`, so a truncated listing is never clean.
- `git_call_async` retries once with `-c safe.directory=<repo>` on "dubious ownership", only when that repository contains the cwd. Never trust arbitrary paths globally.
- `plugin_abi::call` merges `{abi,plugin,verb,body}` over the body's top-level fields, envelope last. Null or empty-object replies classify as `PluginNotFound`. `AbiErrorKind` wire strings are frozen: `plugin-not-found`, `verb-not-supported`, `plugin-error`, `timeout`.- `libsql_wasm::classify_error`: a parsed `ext=`/`rc=` code is authoritative and suppresses text matching; only `ShadowRow` is text-only. `Corrupt` deletes the shared database, so a quoted "malformed" must never trigger it. `retry_on_busy` cannot sleep in the guest, so each retry re-enters libsql's 8 s busy timeout and `BUSY_RETRY_ATTEMPTS` times 8 s must stay under the host deadline.
- `ann_query_sql`: `pool`, not the final `limit`, is both `vector_top_k`'s k and the outer LIMIT; recency rescoring and dedup need that headroom.
- `SCHEMA_ENSURED` and `MIGRATION_COMPLETE` are process-lifetime memos. Code that destroys tables calls `forget_ensured_schema` or `forget_migration_complete`.

## Git and filesystem delivery

### Staging and commit (`wasm_dispatch/verbs.rs`, `verbs/git.rs`)

- Scoped `git_commit` and `git_finalize` stage and commit only the caller's pathspecs, including the concurrent-write amend. A scope that stages nothing is refused; an empty scoped add is never followed by an unscoped commit. Blanket staging needs `add_all: true`.
- `git_pathspec_scope` emits every exclude before the caller's pathspecs. git 2.46 on Windows silently stages nothing for `git add -- <untracked> <exclude>` (exit 0), so an exclude ordered last turns a scoped commit into an unscoped one.- Every staging path (`git_add`, `git_commit`, `git_finalize`, porcelain probes, `git_push`'s dirty gate) appends `GIT_PROTECTED_PATHSPECS` (`:(top,exclude).gm`, `:(top,exclude).agentplug*`), so runtime state is never staged, committed or counted as dirt. Receipts list them under `excluded`.
- `git_commit` deduplicates by cwd, pre-commit HEAD, message, paths and `amend` within `GIT_COMMIT_DEDUP_TTL_MS` (180000), replaying the real SHA. A replay counts only while HEAD still equals the recorded `sha_full`; an object dropped by `git_reset_head` still exists.
- `git_commit {amend:true}` rewrites the current commit and refuses `pushed_commit_refused` and `amend_requires_head`. During an active merge, `git_commit` refuses explicit paths and `add_all`, since the merge consumes the complete prepared index.
- `wasm_dispatch/dangling_refs.rs` validates before staging, so a refusal leaves the index unchanged. A dangling target is an existing, untracked, non-ignored file outside the commit's path set; `git check-ignore` excludes generated output. Any existing unreadable scoped source blocks a clean scan unless the caller waives it.
- `git_status` preserves observed paths, diagnostics, head SHA and branch. Failed, parked or skipped status cannot establish clean, so commit and finalize refuse before mutation. `truncated` in either reply mode only shortens the sample lists: `counts`, `by_directory` and `dirty` stay complete, and the default mode's full porcelain listing is in `spill_file`.
- `git_finalize {paths}` scopes porcelain checks to those paths and pushes by explicit new-HEAD ref, so another writer's dirt never blocks it.
- `git_push {rev}` never rebases a dirty checkout. A rejection because the remote moved names the recovery: `git_pull`, then `git_push {rev:"HEAD"}`. A remote 5xx (`Internal Server Error`, `Service Unavailable`, `Request ID`) is transient: retried `GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS` times, then reported. The local ref did not diverge, so it must never send the caller to pull.
- `git_push` publishes a lineage, and that is intended, not a leak: with no `rev` the source ref defaults to `HEAD` (`verbs/git.rs` 1323) and `exec_git_push_in` sends `git push origin <source_ref>:<branch>` (`verbs/git.rs` 6689), so every commit on local `main` the remote lacks ships with the push, including another lane's commit. Passing `rev` names the tip only; it cannot exclude an ancestor, since a fast-forward of `main` to any ref carries that ref's whole history. Do not add a "refuse foreign commits" gate: on shared linear main every lane pushes `HEAD` and such a gate stalls them all. `git_commit` never publishes; the push gate is the only publishing surface.
- A nonzero `git_pull` with no conflicts re-fetches and compares HEAD with the tracking ref before trusting the failure, since a slow hook can time out after the fast-forward landed. Missing committer identity must name local `user.name`/`user.email`; tooling never configures a global account silently.
- Git verbs resolve the actual dispatch project and fail loudly outside a repository. `git_log` parses `--pretty=format` on `\u{1f}`, since subjects contain spaces.

## Configuration, prose and notifications

### config.rs, config_sync.rs, prose.rs

- `resolve_with`: a `ProjectVendored` win still runs lower repository tiers' `load_repo_tier` for its side effect, so `config_notify::record_change` keeps upstream drift visible.
- `RESOLVE_CACHE` is keyed by the per-call `host_cwd_string()` with a 2 s TTL. Pass a dispatch's resolved FSM graph through `read_state_with_graph` and `set_phase_with_session_with_graph`; separate resolutions can see different tiers.
- `RepoSource.repo` is a `config_path::RepoUrl`, parsed once in `parse_source_entry`; `config_sync` never revalidates it. Config repositories come only through approved remote transports; fetch is HTTP(S) with a nonempty authority.
- Prose keys and source paths are untrusted relative paths: accept only safe components. An unsafe prose key is terminal, since later tiers embed it in paths.
- `ensure_current` debounces on the last probe time whether or not a checkout exists, recording it in memory as well as on disk; a failed `.sync.json` write under load would otherwise re-run `git ls-remote` on every dispatch.
- Sync state and locks live on disk beside the checkout, never in statics, since the `$HOME` user-tier cache is shared by every project. `try_lock` is a non-recursive `mkdirSync` after creating parents.
- The host ABI has no rename. `config_sync::rename` and `memory_md::rename_batch` run `fs.renameSync` through `host_exec_js`, since `host_fs_write` overwrites in place and readers can see a torn file.
- `prose::read_from_config_repo` reads `config::resolve().cache_dir`, never a hardcoded directory. `read_clean` treats whitespace-only content as absent and strips BOM and normalizes CRLF.
- `tier3_user_wide_repo`: a missing `.gm/instructions/source.json` falls through to defaults; an explicitly empty file means not configured.

### orchestrator/config_notify.rs

- A change is persisted when recorded and drained onto the next `instruction` response (`update_available`, `discipline_policies`). Paths resolve through project-scoped `pkfs` per call; never cache another project's pending notices globally.
- Delivery is once per session, not per process, since agents share one plugin instance. Each record keeps a `delivered_to` roster; a missing session buckets under `"(no-session)"`.
- `MAX_RECORDS` (32), `MAX_SUMMARY_ITEMS` (24), `MAX_RECORD_AGE_MS` (24 h) and `MAX_DELIVERED_TO` (64) bound history and rosters, evicting oldest first.
- A torn store reads as "no pending changes" (advisory surface). A failed drain write is not fatal. `record_change` skips no-op SHA changes and never retries an identical failed write. Delivery is marked during the drain, since no acknowledgement verb exists.

### Instruction serving (`orchestrator/instructions/mod.rs`, `orchestrator/state.rs`)

- `instructions::handle` suppresses prose only when the caller asserts the hash it holds. `.last-instruction-hash-<sid>.json` records what was sent, not what arrived.
- Non-read-only replies carry `reply_hash`, stored in `.last-instruction-reply-<sid>.json`. A caller asserting the current instruction hash and a matching `known_reply_hash` gets a delta: equal fields move to `unchanged_since_last_reply`, dropped ones to `removed_since_last_reply`, and `FIELDS_ALWAYS_RESTATED_IN_A_DELTA_REPLY` stay inline. Key only on the caller's assertion, never on the server's last write, so a fork, lost response or retry never elides state the caller did not receive. `{"full":true}` forces the whole envelope; no `session_id` means no delta.
- `investigate_readonly` instructions serve no phase prose and stamp neither `last-instruction-ts` nor `last-dispatch-ts` (`gates::dispatch_serves_no_phase_prose`). Otherwise a read-only call could satisfy the long-gap gate without the recovery prose it exists for.
- Inline PRD and mutable rows are bounded by `instruction_payload.mutables_pending_rows_inlined_limit` and `prd_items_rows_inlined_limit`. Counts (`mutables_pending_count`, `epistemic_gap`, `prd_open_count`) stay exact; a `*_truncated` block names the file and the serving verb.
- `has_compiled_default_for_prose_key` matches exactly the keys `compiled_default_for_prose_key` matches, plus `entry`; unknown keys fall through to ENTRY prose.
- `write_turn_summary` takes `config_changed_count` from `handle`, which already drained `config_notify`; draining again would mark notices delivered to a session that never saw them.
- `residual::handle_scan` writes `residual-check-fired` as `<session_id>:<fired_at_ms>`; mere existence never passes the gate. `yaml_util::invalidate_residual_marker(reason)` is the only clearing path and rewrites `invalidated:<reason>`; `yaml_util::read_residual_marker` is the one parser. A `prd-add` answering `already_identical` must not invalidate.
- `automatic_supply_chain_scan()` runs `scan_deps` on every `instruction` (`supply_chain_scan`), debounced by `.gm/exec-spool/.last-scan-deps-ts` at `SUPPLY_CHAIN_SCAN_DEBOUNCE_MS` (300000). The cache sits under `exec-spool/`, because a host project tracking `.gm/` was dirtied by each tick. The stamp is rewritten on every scan; the result goes through `pkfs::write_if_changed`. Legacy `.gm/.last-scan-deps-*` files are read-only fallbacks.

### Mutables and PRD

- `mutable-add` requires a nonempty caller `id` and a payload field beyond the envelope, and writes nothing otherwise. `status` counts as payload, so `{id, status}` reopens a row.
- `mutables::handle_add` upserts by `id` and collapses duplicates, keeping a resolved row over an unresolved one so a witnessed obligation is never reopened. `handle_list` stays un-deduplicated. `prd-list` defaults to brief rows; internal evidence and gates use `handle_list_full`.

## Cache and memory

### cache.rs, embed_marker.rs, shared_db.rs

- `cache::get` distinguishes store failure from a miss; never overwrite on an unanswered lookup. Expiry filters in the SELECT before the sweep. Preserve the libsql error for corruption classification; integer columns may come back as integer, float or string.
- `shared_db::shared_exec_params` binds every parameter as text and writes return no row count. SQL NULL travels as `''`, unwrapped by `NULLIF(?6,'')`; `invalidate` checks existence through `get` first.
- Embedding generation markers are per table (`embed_marker::marker_rel_for_table`); a shared marker let one table's record mask another's stale width.

### memory_md.rs

- `vector_table()` and `rssearch_vectors` must resolve configurable table names identically. The code namespace belongs to the tree-sitter indexer, so the sync pass skips it.
- `keyword_scan` (`KEYWORD_SCAN_MAX_FILES` = 2000, sorted by name) needs neither embedder nor libsql, so it survives an embedder or libsql outage. The flat-KV keyword scan is libsql-backed and no-ops without a libsql slot.
- A merely deferred sync stores its digest tagged `:partial=N`, or `has_stored_digest` stays false forever. Never store a digest when `failed>0` or `rekeyed>0`. Orphan pruning requires full convergence, since pruning an incomplete view deletes live entries.
- Explicit-key `memorize-prune` marks the index before removing the file, to prevent resurrection. Never read failed semantic retrieval as an empty namespace.

## Disciplines, fibers and calculus

- `capability_proxy::resolve` and `discipline_note::requires_satisfied` must match providers identically: both use `resolve_key_realm` (an empty or self-named realm maps to the discipline realm), require an Active provider, and check `provides`.
- `discipline_note::active_policies` is the only caller of discipline `advance_fiber`, once per `instruction`; there is no push notify. `all_known_discipline_dirs` includes disabled names with state files, so a disabled discipline advances to Inactive.
- `build_interception_context` folds enabled-file order with right-biased `MergeKind::combine`; for `ScalarOverwrite` the last nonempty declaration wins. `handle_check_removal` is the only writer of `enabled.txt` and CAS-writes against the snapshot it read.
- Guest `fiber_lifecycle::transition` is mirrored arm for arm by agentplug-host `registry.rs` `PluginFiberLifecycle`. The guest has only Inactive, Active and Unloading; a new failure-bearing state changes both sides.
- `calculus::verify_calculus` and `formal/CordisCalculus/` change together. It stops silently at `max_states`, so `ok` covers only explored states.
- Base-registry unload's `retired || !satisfied` equals the paper's `target != omega` only because base reload commits the current target. ExtendedRegistry has no retirement flag; unsatisfied requirements remove its target.
- `component_loader_dispatch::parse_entries` silently drops entries that fail to deserialize. `isolate` is the tagged `{"kind": "none"|"local"|"global"}` shape. `claim_audit_clean` passes only on the exact body `clean`.

## Dependency scanning (`scan_deps.rs`)

- `find_suspicious_escapes` requires at least four Unicode escapes decoding to an identifier; `count_hex_obfuscator_idents` covers escape-free `_0x` obfuscation. A size ratio alone warns and never fails.
- Package signatures combine maximum mtime and summed bytes, since directory mtime misses in-place writes. `walk_package` follows the real dependency tree with containment and visited guards.
- A failed read after a positive-size stat is a blocked read, not empty content; failed or blocked packages are never stamped clean. Truncated scans are never reported complete, and `full:true` clears the stamp before walking.
- Signatures live in `.gm/exec-spool/.scan-deps-stamp.json`; the legacy `.gm/scan-deps-stamp.json` is read-only.

## Dream-RSI replay

- Replay is frozen-world evidence, not execution or deployment authority. Records stay bound to their completed dispatch and owner session; normal authorization and phase paths govern deployment.
- Score each world as best attained quality minus `beta1 * summed cost` plus `beta2 * parallelism_bonus`; a policy scores the mean over its worlds. A challenger wins only with a strict improvement over the incumbent on the same worlds. Betas are explicit finite nonnegative inputs, never defaults.
- `dream-replay-round` is session-owned progressive reveal from the policy roots and the revealed frontier. Never expose the full frozen world or treat unrevealed values as observed. It closes at `max_rounds` or `max_nodes`, counting the closing reveal. `max_online_rounds` bounds new distinct values; reaching the cap requires sealing and replay before more rollout.
- `dream-replay-cycle` is observation-only maintenance, separate from policy replay. It uses the canonical owner `session_id`, verifies ledger evidence through `automatic_replay`, and defers when no new verified dispatch exists. It never refreshes phase clocks or records its own maintenance as an observation.
- `admit_dispatch` ranks and never refuses: it returns `Admission::Allow` or `Admission::Advisory`, and the advisory attaches to a dispatch that ran (`dream_rsi_advisory`). Refusing was a livelock, since a refused dispatch is never observed. Safety refusals run earlier, in `gates::check_dispatch`.
- A successful `instruction` clears the ranking by stamping `.gm/dream-rsi/<sid>/reorientation-ts` and the project-wide `.gm/dream-rsi/_any-session/reorientation-ts` (`PROJECT_WIDE_MARKER_SESSION`). The MCP `gm_instruction` tool dispatches under a server-local id, so a per-session marker alone could not clear it. The ranking lapses after `VETO_MAX_AGE_MS` (600000).

## Lean graph and compiled prose

- `orchestrator/lean_graph.json` and `orchestrator/instructions/prose/*.md` are vendored copies of `gm-config/fsm/graph.json` and `gm-config/prose/*.md`. `fsm::default_graph()` parses the lean graph; `instructions/lean_prose.rs` maps each lean prose key to its file. `entry` and `entry-extended` remain separate compiled prose.
- Re-sync: copy both sources from the gm-config commit named by `DEFAULT_REPO_PINNED_SHA`, regenerate `lean_prose.rs` from every `prose/*.md` except `entry` and `entry-extended`, delete prose files gm-config no longer has, and keep the pin on that commit.
- A state with `"entry": true` is a graph root and skips the unreachable-state check. A configured graph that fails validation makes `instruction` and `transition` return the error; neither serves the compiled default instead.
- A gate with `"advisory": true` never blocks; its message appears in the `advisory` array of the `transition` reply.

## Other contracts

- `dataflow::default_document` is not executed for CompiledDefault. Configured pipelines run declared steps then fuse nodes; there is no topological scheduler. `plugin == "gm"` calls internal functions, not WASM self-dispatch.
- `legacy_reaper::RETIRED_ARTIFACTS` is an exact allowlist, never a glob and never a memory or database deletion. Its hash invalidates the reap marker when the list changes.
- `mediator::SELF_LANG_VERBS` share dispatch code with their base verbs but are not aliases; each keeps its own language for `shell_exec`.
- `submodule_head_sha` skips uninitialized submodule directories without their own `.git`; running git there would return the parent's HEAD.
