# AGENTS.md

`plugkit-core` is gm's host-agnostic WASM guest: orchestration, spool verbs, code search and memory live in `crates/plugkit-core/src/`. `agentplug-runner` is the supported host. Follow the parent [AGENTS.md](../AGENTS.md) and gm skill; [README.md](README.md) owns architecture, verb inventory, spool ABI and release details.

## Work and verification

- Use GM MCP and the served workflow. Structural questions start with `callers`/`impact`, then `codesearch`; read only located paths. Give parallel agents distinct spool session IDs and disjoint writer ownership.
- Source carries no comments except attributes, unsafe `SAFETY` rationale and license headers. Rustdoc is not published. Keep non-obvious current constraints here; make ordinary facts self-explanatory code. Keep text UTF-8 without a BOM and without decorative glyphs.
- No synthetic tests, fixtures, mocks, fuzz harnesses or test frameworks. Verify changed behavior with a real build and live dispatch through the owning verb and supported host; read the actual output. Documentation-only changes require factual, scope and whitespace verification, not an unrelated rebuild.
- Build the published guest with `cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim`; lint with the equivalent `cargo clippy`. `cargo check -p rs-plugkit --offline` additionally compiles native-only branches. The guest is not a standalone runtime.
- Parser changes require live malformed and boundary inputs. `fsm-validate` reaches `FsmGraph::validate` for graph validation; exercise invalid structures, not legitimate FSM feedback cycles. Config/prose resolution has no dedicated validate-only verb: use a real project configuration and the actual resolving verb, and distinguish that side-effect witness from a dedicated validator. Malformed spool JSON must reach the actual in-file/parser, not be repaired by a wrapper first.
- A verb adds its handler, `wasm_dispatch/verbs.rs` route, relevant gates/transitions, the parent verb contract and this README inventory together. Update both sides of a cross-repository ABI. Push scoped changes directly to `main` through GM git verbs; do not manufacture branches or PRs. The signed release/cascade workflow owns version bumps and publication; verify its result before claiming a release.
- Keep this file below 30,000 bytes. Revalidate the complete file, retained notes/memories and repository history when compacting. Absorb only current reusable facts, discard duplicate or superseded narration, and snapshot exact records before explicit-key pruning. Do not sweep another writer's files or runtime caches into delivery.

## Ownership map

- `lib.rs`, `wasm_dispatch/`: guest entry point and per-verb handlers.
- `orchestrator/`, `gates.rs`: phase graph, obligations, discipline/fiber lifecycle and served prose.
- `code_index.rs`, `code_symbols.rs`, `git_commit_vectors.rs`: retrieval, structural indexing and history ranking.
- `embed.rs`, `vecstore.rs`, `vecns.rs`, `rssearch_vectors.rs`: embeddings and vector namespaces.
- `memory_md.rs`, `cache.rs`, `shared_db.rs`, `libsql_wasm.rs`: durable memory, cache and database ownership.
- `config.rs`, `config_sync.rs`, `config_path.rs`, `prose.rs`: configuration tiers and bounded source resolution.
- `dataflow.rs`, `dataflow_exec.rs`, `mediator.rs`, `dispatch_ledger.rs`: pipeline execution, routing and evidence.
- `pkfs.rs`, `validation.rs`, `gitignore.rs`, `legacy_reaper.rs`, `poll_detect.rs`: project paths, boundaries and cleanup.

## Indexing and search

### code_index.rs

- One guest instance serves multiple projects. Static corpus/digest/embedding caches include the dispatch project; `project_scoped_cache_key` incorporates `host_cwd` when no root is explicit. Per-root KV namespaces also include `root_ns_suffix`; the no-root namespace is unsalted.
- `SKIP_FILE_SUFFIXES` removes readable build artifacts such as `.rlib`, `.rmeta` and `.pdb` outside a directory named `target`; never infer that readable symbol names make an artifact source.
- In `ensure_schema_at_cfg`, `rssearch_vectors` and `git_commit_vectors`, drop mismatched-width tables before `CREATE TABLE IF NOT EXISTS`, which does not replace an existing table.
- `parse_manifest` accepts `MIN_READABLE_MANIFEST_VERSION..=MANIFEST_VERSION`; new fields are optional. Rejecting every old version invokes `purge_stale_manifest_row` and destroys incremental reuse.
- `FileManifest::digest_hash`, including stat-only branches, records the same per-file value used by `current_digest`; `current_digest_cfg` uses the indexer's size cap. Hash-match reuse refreshes manifest mtime/size.
- Deferred passes in code and memory indexing write `<digest>:partial=N`. Missing digests force repeated full work; partial digests never signify convergence. Raise actual configured budgets when the tree cannot converge.
- `index_cfg_impl` visits the whole sorted listing, starts at `.gm/exec-spool/.codeinsight-cursor`, and wraps. `max_files` bounds fresh extraction, not the listed universe; a bounded prefix permanently starves later paths.
- Per-file chunk caps bound fresh embeddings, not retained chunks: keep reusable embeddings, record deferred chunks in `skipped_no_embed`, and retry embed failures. Older manifests predating `FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS` re-extract once with embedding reuse.
- Charge fresh-file allowance and the one-file grace only after extraction proves a fresh embedding is needed. Reuse-only extraction consumes neither. `codeinsight_index` is the bulk indexing path; embedding work must stay within the host deadline rather than promise a fixed throughput.
- Count vector-table rows through `GROUP BY` subqueries: unfiltered `COUNT(*)`/`COUNT(DISTINCT ...)` over the libsql `F32_BLOB` table can return zero. Structural tables are ordinary tables and do not need this workaround.
- Store call edges once per file, not per edge: deletion must not scan an entire edge namespace for every changed file. `index_with_dead_code` stays opt-in because ordinary overview runs during every `instruction`.
- `scan_literal` skips digest/index/embedding/vector/fusion work. Its file-count and byte limits are independent of embedding/digest limits, which must not silently shrink an exhaustive text answer.
- Every coverage-affecting bound or unreadable file clears `exhaustive`; a binary skip does not. `host_read` returning `None` covers both I/O failure and invalid UTF-8: distinguish them with `host_stat`.
- Request `file_cap + 1` entries from `list_scan_universe` to distinguish an exactly full listing from a truncated one. Scoped gitignored/non-git trees use their declared scan universe, not an unrelated indexed project.
- Unicode lowercasing can change byte width. `LiteralMatcher::find_all` uses character-aligned matching when lengths differ, safe `get` slices, and every occurrence per line rather than one representative call site.

### code_symbols.rs

- Structural queries first run bounded refresh without embeddings and refuse incomplete graph evidence. `sync_files` also runs during indexing and stale-insight maintenance; schema 3 records a current-byte FNV64 `source_hash` so same-size/mtime edits invalidate reuse. Failed parses retry. Bump the schema when stored shape changes.
- Symbols, metrics and raw import specs live in `code_symbols`, `code_symbol_files` and `code_imports`, not vector chunks. Multi-row inserts stay within SQLite's 999-parameter bound.
- Call edges use one `cef-<crc32(path)>` KV row in `<code_ns>-edges-by-file`; first sync purges the retired per-edge namespace. `callee_name_for_call` records the final callee-expression leaf, selecting the widest node starting at the call.
- Raw import specs resolve at query time against indexed paths; only resolved indexed targets form edges. Changes in one file must not invalidate another file's stored import spelling.
- `find` escapes `%`, `_` and `\` with SQL `ESCAPE '\'`; stripping them breaks snake-case queries. `cx` counts decisions within a function's byte range, nested closures included; Boolean operators count only for Python `boolean_operator`.
- Edges are unqualified callee names, not definition-bound relationships. Callers/callees/impact/tests reject path/file/glob/line scopes they cannot represent; callers/impact disclose resolution and same-name ambiguity. `impact` does not transitively expand ambiguous names unless `through_ambiguous` is explicit. The README owns the public query contract.

### ragconfig.rs and search dispatch

- `pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound` derives fresh-chunk allowance from measured worst-case WASM embedding cost. Lowering it without measurement can overrun the epoch deadline and poison the Store.
- `digest_max_files` and `prune_enumeration_file_cap` bound visibility as well as work; insufficient limits can leave stale chunks while reporting convergence. Large monorepos need adequate values.
- Git-history sync has an elapsed hard ceiling independent of its minimum-embed floor. Cheap `--shortstat` chooses subject-only embedding above `git_commit_full_diff_max_changed_lines` or `_max_files`; the file limit covers binary-heavy zero-line diffs.
- `.gm/index-config.json` is an overlay outside the configuration tiers: a project-vendored tier replaces lower tiers wholesale and would otherwise hide prose/FSM/messages. Overlay lists append, never replace.
- `RetentionConfig` reclaims only already-tombstoned data; live-row pruning remains an explicit agent decision.
- `codesearch_exhaustive` routes before root-sensitive retrieval and all embedding/index steps. Root-scoped dual retrieval never uses cwd-bound corpus/fusion/dataflow state; scope filtering applies to vector and BM25 hits with adequate over-fetching and an echoed effective scope.
- `CODESEARCH_MODES`, `CODESEARCH_LIMIT_FIELDS` and `codesearch_result_limit` reject unknown modes and conflicting/ignored limits. Only an explicitly supplied limit bounds an exhaustive request. Filename mode must apply its accepted path, limit and output controls, not silently search the whole project.

## Embedding, vectors and host boundaries

### Cargo.toml and embed.rs

- `slim` omits compiled-in safetensors and requires host `host_vec_embed`; the supported runner supplies it. Keep candle dependencies at the validated 0.8 WASM-compatible line unless the real target build proves a replacement.
- A model-width change updates weights, `EMBED_DIM`, `bge_small_config().hidden_size`, `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together.
- BGE queries carry `BGE_QUERY_PREFIX`; passages do not. Every query embedding uses `condition_query`. Project-scoped embedding cache slots compare the stored full key even though their slot name is an `fnv1a64` hash.
- `host_vec_embed` and `try_sibling_plugin_embed` are distinct host routes to the same BERT model. The sibling-plugin route preserves load/model diagnostics; it is not redundant merely because the model is shared.
- `build.rs` embeds `PLUGKIT_SOURCE_SHA`. Fix `PLUGKIT_BUILD_SHA` for byte-for-byte A/B comparisons; deleting source lines changes panic locations even when tokens are otherwise identical.

### rssearch_vectors.rs, libsql_wasm.rs and host_abi.rs

- `ann_query_sql` uses `pool`, not final `limit`, for both `vector_top_k` and outer SQL LIMIT; recency rescoring and dedup require candidate headroom.
- `SCHEMA_ENSURED`/`MIGRATION_COMPLETE` are process-lifetime memos. Destructive table recovery calls `forget_ensured_schema`/`forget_migration_complete`.
- Parsed SQLite `ext=`/`rc=` codes are authoritative in `classify_error` and suppress text fallback; only `ShadowRow` is text-only. Corruption recovery deletes the shared database, so quoted words such as "malformed" must not trigger it.
- `retry_on_busy` cannot sleep in the guest; each retry re-enters libsql's 8-second busy timeout. Total retries must fit the host dispatch deadline.
- Synchronous `git_call` turns `{pending,token}` into failure, never a clean porcelain result. Only `git_step`/`git_poll` use `git_call_async`.
- A dubious-ownership retry adds only the git-reported repository's scoped `safe.directory`, once, and only when that repository contains the requested cwd. Do not globally trust arbitrary paths.

### plugin_abi.rs

- `call` merges `{abi,plugin,verb,body}` over the body's top-level fields, envelope last. Retain top-level fields for older sibling plugins that read them directly.
- `parse_response` classifies null/empty-object replies as `PluginNotFound`; successful replies without `data` return the remaining object excluding `ok`/`abi`.
- `AbiErrorKind` wire strings are frozen: `plugin-not-found`, `verb-not-supported`, `plugin-error`, `timeout`. Missing explicit kind uses the actual host error spelling, not an invented classification.
- `KNOWN_PLUGINS` is only a diagnostic hint. The host capability allowlist is authoritative and includes plugins not listed here; do not turn the hint into access control.

## Git and filesystem delivery

### wasm_dispatch/verbs.rs

- Self-declared `discipline` controls `confinement_violation`/`capability_access_violation`; the spool carries no unforgeable caller identity. These catch accidental misuse, not hostile impersonation.
- `browser`/`cdp` share `host_browser_exec`. Engine selection travels in opts JSON, never embedded in caller JS.
- Scoped `git_commit`/`git_finalize` stage and commit only caller pathspecs, including concurrent-write amend. Refuse paths that stage nothing; never follow an empty scoped add with an unscoped commit. Default commit uses already-staged content; blanket staging requires explicit `add_all` (unscoped finalize owns its documented default).
- `git_pathspec_scope` emits excludes before inclusions; reversing them can make Windows git silently stage nothing. Preserve explicit caller paths. Always exclude `.agentplug*`; distinguish ignored/untracked generated `.gm` state from explicitly requested tracked state. Receipts disclose withheld runtime dirt rather than silently widening delivery.
- `git_commit` deduplicates a logical request by cwd, pre-commit HEAD, message and paths within `GIT_COMMIT_DEDUP_TTL_MS`; replay the real SHA rather than execute another commit.
- `git_finalize {paths}` scopes porcelain probes and pushes its new explicit ref, so unrelated writer dirt does not block it. `git_push {rev}` never rebases a dirty shared checkout; remote movement returns the recovery `git_pull` then `git_push {rev:"HEAD"}`.
- After a non-conflict pull failure, re-fetch and compare HEAD with the tracking ref before trusting timeout/hook/credential failure: the fast-forward may already have landed. Missing merge committer identity must name local `user.name`/`user.email` requirements; authentication is not commit identity, and tooling must not configure a global account silently.
- Git verbs resolve the actual dispatch project and fail loudly outside a repository. `git_log` parses its formatted fields on `\u{1f}`, not spaces; subjects may contain spaces.

### wasm_dispatch/dangling_refs.rs

- Validate before staging, so refusal leaves the index unchanged. Scan only already-staged files plus the proposed path/add-all scope, not sibling dirt.
- A dangling target is an existing, untracked, non-ignored file outside this commit's path set. An untracked target included in the same commit is valid; `git check-ignore` excludes ignored generated output.

## Configuration, prose and notifications

### config.rs, config_sync.rs and prose.rs

- A `ProjectVendored` win still refreshes lower repository tiers for `config_notify::record_change`; an override must not hide upstream drift.
- `RESOLVE_CACHE` keys the per-call project cwd and has a short TTL; one process serves multiple projects. Resolve a dispatch's FSM graph once and pass it through state/transition operations rather than compare resolutions from different tiers.
- `ensure_current` revalidates public `RepoSource` URLs at the git boundary. Disk sync state/locks are shared beside the checkout, not process-local; `try_lock` uses atomic non-recursive mkdir after creating parents.
- The host ABI lacks rename. `config_sync` and `memory_md::rename_batch` use `fs.renameSync` through `host_exec_js`; in-place writes expose torn files.
- `read_from_config_repo` reads the resolved cache directory, never a guessed global directory. Unsafe prose keys are terminal because later tiers also embed those keys in paths. `read_clean` treats whitespace-only content as absent and strips BOM/normalizes CRLF.
- `tier3_user_wide_repo` falls through. Missing `.gm/instructions/source.json` reaches configured defaults; an explicitly empty file means not configured and skips that source. Do not conflate absent, empty, broken and unreachable outcomes.

### orchestrator/config_notify.rs

- Persist changes when recorded and drain onto the next instruction; no active dispatch may exist at change time. Delivery is once per session, with absent session IDs bucketed separately, not once per process.
- Paths resolve through project-scoped `pkfs` per call. Never cache another project's pending notifications globally.
- `MAX_RECORDS`, `MAX_SUMMARY_ITEMS`, `MAX_RECORD_AGE_MS` and `MAX_DELIVERED_TO` bound history and rosters, evicting oldest first. Advisory torn reads degrade to no pending changes; failed drain writes can repeat delivery but must not fail the work dispatch.
- Skip no-op SHA changes; change IDs distinguish tier/SHA/time. Do not retry identical failed writes. Preserve unreadable timestamps rather than assume expiration; marking delivery happens during drain because no acknowledgement verb exists.

## Cache and memory

### cache.rs and embed_marker.rs

- `cache::get` distinguishes store failure from cache miss; never overwrite on an unanswered lookup. Expiry is filtered by SELECT before sweeping. Every public operation ensures schema for its project; no process-global initialized flag.
- LRU touch and budget enforcement are best-effort. Preserve the underlying libsql error for corruption classification. Integer columns may be integer, float or string; `row_i64` and chunk-row readers accept all three representations.
- Shared parameter binding is text and writes return no row count. `NULLIF(?6,'')` represents SQL NULL; invalidation checks existence through `get` first.
- Embedding generation markers are per table: one store's dimension check must not mask another table's stale width.

### memory_md.rs

- `vector_table` and `rssearch_vectors` must resolve configurable table names identically. Code namespace indexing belongs to tree-sitter, not Markdown memory digests.
- `flat_vec_embedding` checks `cfg.dim()`, not a fixed literal. `keyword_scan` is a deterministic bounded file fallback requiring neither embedder nor libsql; the flat-KV fallback still requires libsql.
- A purely deferred sync stores a partial digest, but failed/rekeyed passes do not. Orphan pruning requires full convergence: an incomplete manifest must not tombstone live memory.
- Explicit-key `memorize-prune` marks the index before removing the file to prevent resurrection, and can remove real on-disk memories lacking vector rows. Never interpret failed semantic retrieval as an empty namespace.

## Disciplines, fibers and executable calculus

- `capability_proxy::resolve` and `discipline_note::requires_satisfied` use identical `resolve_key_realm`, Active-provider and `provides` rules. An unmapped `RealmTable` key returns empty; `resolve_key_realm` maps empty or self-named keys to the discipline realm.
- `active_policies` alone advances discipline fibers, once per instruction; there is no push notification. Include disabled disciplines with stored state so they can become Inactive.
- `build_interception_context` folds enabled-file order with right-biased `MergeKind::combine`; the last nonempty scalar declaration wins. `handle_check_removal` alone writes `enabled.txt`, using one snapshot and CAS.
- Guest `fiber_lifecycle::transition` and host `PluginFiberLifecycle` match arm for arm. The executable guest reduction has only Inactive/Active/Unloading, not separate loading or failure states; a new failure-bearing state changes both sides. Codeinsight roles advance through their role-state files with unconditional satisfied targets because they declare no requirements.
- `calculus::verify_calculus` and `formal/CordisCalculus/` change together. Verification silently stops at `max_states`; success covers only explored states, not the unexplored graph.
- Base-registry unload's `retired || !satisfied` matches undefined target only because reload commits the current target. ExtendedRegistry has no retirement flag: unsatisfied requirements remove its target. Coeffects include only Active fibers; remaining reload iterations encode the pending effect/finish, and registry equivalence is unordered map equality.
- `component_loader_dispatch::parse_entries` drops deserialization failures. `isolate` is tagged `{kind:"none"|"local"|"global"}`, not a Boolean/string paper notation. Only exact marker `clean` passes `claim_audit_clean`.

## Orchestration replies and evidence

- `state::read_state_with_graph`/`set_phase_with_session_with_graph` use one resolved graph. Residual checks execute fixed order and stop on first failure; `residual-check-fired` carries `<session_id>:<fired_at_ms>`, not mere file existence.
- `has_compiled_default_for_prose_key` matches exactly `compiled_default_for_prose_key` plus `entry`; unknown keys otherwise receive ENTRY prose. `write_turn_summary` uses the already-drained config count rather than draining unseen notifications again.
- Suppress instruction prose only when the caller asserts a hash it actually received. Stored last-sent hashes are not delivery acknowledgements.
- Delta replies require the asserted current instruction hash and matching `known_reply_hash`. Equal fields are named as unchanged, removed fields are named separately, mandatory live fields stay inline, and `full_reply_at` names the full payload. No session means no delta; `full:true` forces full content. Never infer a client's knowledge from the last server write.
- `investigate_readonly` serves no phase prose and cannot refresh phase-prose timestamps. Inline PRD/mutable row budgets bound payload arrays; exact counts and explicit full-list paths/verbs remain authoritative.
- `mutables::handle_add` upserts IDs, collapses duplicates and prefers an already resolved row. `handle_list` remains the full-fidelity file view; deduplication must not reopen witnessed obligations.
- `automatic_supply_chain_scan` runs on instruction, debounced by `.gm/.last-scan-deps-ts`/`SUPPLY_CHAIN_SCAN_DEBOUNCE_MS`. Structural scans supplement, never replace, exact IOC searches.

## Dependency scanning

- `find_suspicious_escapes` requires at least four Unicode escapes decoding to an identifier, not arbitrary printable CSS punctuation. `count_hex_obfuscator_idents` covers escape-free `_0x` obfuscation; size ratio alone warns, never fails.
- Package signatures combine maximum mtime and summed bytes; directory mtime misses in-place writes. `walk_package` uses the actual dependency tree and containment/visited guards. `IndexConfig::is_force_included` is substring-based, including descendants.
- Oversized files warn without a full scan. A failed read after positive-size stat is a blocked read, not empty content; failed or blocked packages must not become stamped clean.
- `scan_node_modules` preserves earlier signatures for unchanged or budget-deferred packages. `full:true` clears the prior stamp before walking. Never silently report truncated scans as complete.

## Dream-RSI replay

- Replay is frozen-world evidence, not execution or deployment authority. Observations, evaluator receipts, policies and discoveries remain bound to their actual completed dispatch and owner session; normal authorization/phase paths govern deployment.
- Score each world as best attained quality minus `beta1 * summed cost` plus `beta2 * parallelism_bonus`; policy score is the mean over supplied worlds. Challenger selection requires a strict improvement over the incumbent on the same worlds. Betas are explicit finite nonnegative inputs, never arbitrary defaults.
- One-shot replay's bonus is revealed node count divided by distinct recorded rounds. Omitted discovery rounds receive sequential positions for older records. Sealing constructs children from `parent_id`, not array adjacency.
- `dream-replay-round` is session-owned progressive reveal: batches come from policy roots plus the current revealed frontier. Do not expose the full frozen world or treat unrevealed values as observed. Stateful round replay counts actual rounds, closes at `max_rounds` or `max_nodes`, includes the closing reveal in its tally, and rejects further calls after closure.
- `max_online_rounds` requires explicit discovery rounds and bounds new distinct values, not repeated values in an existing batch. Policies without the cap remain unbounded. Reaching a cap requires sealing/replay before further online rollout.

## Other contracts

- `dataflow::default_document` is not executed for CompiledDefault; configured pipelines execute declared steps then fuse nodes, not a topological scheduler. `plugin == "gm"` calls internal functions, not WASM self-dispatch.
- `legacy_reaper::RETIRED_ARTIFACTS` is an exact allowlist, never a glob or memory/database deletion. Its hash invalidates the reap marker when the allowlist changes.
- `mediator::SELF_LANG_VERBS` share dispatch code but are not aliases; retain their distinct language passed to `shell_exec`.
- `submodule_head_sha` skips uninitialized submodule directories without their own `.git`; running git there would return the parent's HEAD successfully.
- Browser witnesses are the flat `{file:hash}` map written by `browser_witness::record_witness`; transition readers also tolerate the nested `witnessed_hashes` wrapper and compare actual source hashes.
