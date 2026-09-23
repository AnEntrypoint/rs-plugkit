# AGENTS.md

`plugkit-core` is the wasm cdylib guest behind gm -- the FSM/PRD/mutables
orchestrator, spool-dispatch verb handlers, code search, and memory/recall
all live in this one crate, compiled to `plugkit.wasm`/`plugkit-slim.wasm`.
It is host-agnostic; `agentplug-runner` (repo `AnEntrypoint/agentplug`) is
the sole host that loads it. See `README.md` for the full architecture,
spool ABI, and build/cascade details -- this file covers crate-internal
conventions only.

## Project structure

```
crates/plugkit-core/src/
  lib.rs                 - crate root, wasm entry point
  wasm_dispatch/         - per-verb dispatch handlers (git_*, fs_*, prd-*, ci-status, ...)
  orchestrator/          - FSM phase graph, gates, transitions, served instruction prose
  code_index.rs          - codesearch/codeinsight indexing pipeline
  embed.rs               - BERT embedding (batched + per-item fallback)
  vecstore.rs / vecns.rs - vector storage, namespacing
  rssearch_vectors.rs    - vector search over libsql
  git_commit_vectors.rs  - git-history-aware search ranking
  memory_md.rs           - human-readable memory file I/O
  dataflow.rs / dataflow_exec.rs - data-driven plugin pipeline schema + executor
  dispatch_ledger.rs     - per-write audit tuple (id, hash, ts)
  gates.rs               - transition gate predicates
  config.rs / config_sync.rs / config_path.rs - three-tier config/prose resolution
  prose.rs               - per-key prose resolution chain
  gitignore.rs           - managed-gitignore block handling
  pkfs.rs                - project-scoped filesystem helpers
  filter.rs              - stdout -> compact-stdout transform
  validation.rs          - shared input validation
  legacy_reaper.rs        - stale-state cleanup
  mediator.rs            - cross-verb coordination
  poll_detect.rs          - polling-pattern detection/rejection
  shared_db.rs / libsql_wasm.rs - libsql-backed storage
  cache.rs / embed_marker.rs / evidence_receipt.rs / ragconfig.rs / browser_witness.rs
```

## Code style

Follows the same discipline as gm's own `AGENTS.md` (parent repo): no
source comments. A fact a name can carry goes in the name (this crate's long
descriptive identifiers are deliberate); a non-obvious rationale that is still
true goes in "Source invariants" below, 1-3 lines each, or in the gm recall
store. Kept in source: `#[...]` attributes, `// SAFETY:` on `unsafe` blocks,
license headers. Doc comments count as comments: rustdoc is never built or
published. No synthetic test files or test frameworks of any kind --
verification is a real build plus a live-witnessed dispatch through the
actual spool, never a mock. No graphical/decorative glyphs in source or docs.
No UTF-8 BOM.

## Development

```bash
cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim
cargo clippy -p rs-plugkit --release --target wasm32-wasip1 --features slim
cargo check -p rs-plugkit --offline
```

The first line is exactly what CI builds and publishes. The host `cargo check`
is the only build that compiles `#[cfg(not(target_arch = "wasm32"))]` code.

There is no standalone way to "run" this crate outside a wasm host --
verification means building, then dispatching real spool verbs against a
project with `agentplug-runner` loaded (see gm's own `AGENTS.md` for the
spool-dispatch ABI and boot procedure) and reading the actual response
JSON, never asserting behavior from source reading alone.

## Parser-shaped surfaces need adversarial input, not just hand-picked cases

`orchestrator/fsm.rs`'s `graph.json` parsing, the
`config.rs`/`config_sync.rs`/`prose.rs` three-tier resolution chain, and
every `wasm_dispatch` handler's spool-JSON body parsing are parser-shaped.
Externally-vendorable or caller-supplied input reaches this crate's own
code at each surface. This is the class of surface a coverage-guided
fuzzer earns its keep against, in a project that has one.

This crate has no fuzz harness. A standing fuzz target is a test-adjacent
artifact, and this project's no-test-file rule already excludes it. The
adversarial-input coverage a fuzzer would otherwise buy comes from a
live-witnessed batch instead, dispatched at DECIDE against the real verb
for each surface, never a workaround through an unrelated verb.

For `graph.json`: dispatch `fsm-validate` (`orchestrator/mod.rs` ->
`fsm_vendor::handle_validate`, which calls `FsmGraph::validate()`
directly) against a batch of malformed inputs -- an empty graph, a cyclic
edge set, a graph missing its `gates` array. `fsm-validate` already exists
and already routes to the real Rust validator; do not reach for `exec_js`
as a workaround for a surface with its own sanctioned verb.

For the config resolution chain (`config.rs`/`config_sync.rs`/`prose.rs`):
no dedicated validate-only verb exists yet. Witnessing this surface today
means constructing a real project-local `gm.config.json` with a deeply
nested override chain and dispatching an ordinary config-reading verb
(`instruction`, which resolves config on every call) against it, reading
the live response for a crash or a silently wrong resolution -- add the
missing dedicated verb as its own PRD row before treating this half of the
sweep as covered by more than an ordinary-verb side effect.

For `wasm_dispatch`'s own spool-body parsing: `dispatch_verb_inner` parses
the body via `serde_json::from_str(&body_s).unwrap_or(Value::Null)` before
any verb handler runs, so a verb dispatch can only witness this surface if
the malformed body reaches the spool in the first place -- write a
malformed `.txt` file directly to `.gm/exec-spool/in/<verb>/<N>.txt` (a
`Write`-tool action, not an `exec_js` dispatch) and read the resulting
`out/<N>.json` for a clean-reject versus a silent-wrong-parse.

This is `decide.md`'s existing "degenerate input"/"boundary
conditions"/"empty/overflow/reentry" sweep classes, named here explicitly
against `orchestrator/fsm.rs`, `config.rs`, `config_sync.rs`, `prose.rs`,
and `wasm_dispatch`'s body-parsing entry point. A session touching one of
these files treats the sweep as covering this crate's own internals, not
only the target project's code the crate was dispatched against.

## Adding a verb

1. Add the handler in the relevant `wasm_dispatch/` module.
2. Wire it into the verb-dispatch match in `wasm_dispatch/verbs.rs`
   (`dispatch_verb_inner`).
3. If it changes phase/gate behavior, update `gates.rs` and/or
   `orchestrator/transitions.rs`.
4. Document the verb in gm's own `AGENTS.md` (Spool dispatch ABI section)
   and this crate's `README.md` verb enumeration -- a verb only gm's
   `AGENTS.md` or only this `README.md` know about is a documentation gap,
   not a completed change.
5. Push to `main`; the cascade (`cascade.yml` -> `release.yml`) builds and
   publishes the new `plugkit.wasm`/`plugkit-slim.wasm`, no manual version
   bump.

## Testing

No test files, no test frameworks, ever -- this repo is fully bound by the
gm-family no-test-framework rule (see gm's own `AGENTS.md`, Coding Style
section). A change is verified by a real build plus a live spool dispatch
witnessing the actual behavior, read via `Read`/`exec_js`, never a
`*.test.rs`/mock/fixture standing in for that witness.

## Pull requests

There are no branches or PRs in this workflow -- every change pushes
straight to `main` (see gm's own `AGENTS.md`, direct-push-to-main rule).
A branch or open PR found in this repo is a deviation to consolidate onto
`main` or remove, not a review step to wait on.

## Source invariants

Non-obvious facts the code cannot carry, relocated from source comments.
Each is still true of the current code; fix or delete an entry when that
changes.

### code_index.rs

- `SKIP_FILE_SUFFIXES`: `.rlib`/`.rmeta`/`.pdb` are the only exclusion for
  build output in dirs not named exactly `target` (e.g. `target-foo/`); they
  hold readable symbol names that pollute literal scans.
- `ensure_schema_at_cfg` (and `rssearch_vectors`/`git_commit_vectors` schema
  setup): the dim-mismatch drop must run before `CREATE TABLE IF NOT EXISTS`,
  which is a silent no-op against a surviving old-width table.
- `parse_manifest`: accepts every version in
  `MIN_READABLE_MANIFEST_VERSION..=MANIFEST_VERSION`. A parse failure routes to
  `purge_stale_manifest_row`, so a strict version check turns a version bump
  into a full cache wipe on every pass; new manifest fields must be optional.
- `FileManifest::digest_hash`: every branch, the stat-only fast path included,
  must record the same per-file value `current_digest()` folds, and
  `current_digest_cfg` must apply the indexer's file-size cap; otherwise the
  stored digest never matches and every dispatch re-indexes.
- Over-budget passes (`code_index.rs`, `memory_md.rs`) store
  `<digest>:partial=N`: it never equals a fresh digest, so the next dispatch
  resumes. It must still be written; a missing digest forces a full re-index
  that is itself partial. A tree that never fits re-runs every dispatch until
  `IndexConfig::wall_budget_ms` / `MemorySyncBudgetConfig` is raised.
- `root_ns_suffix`: host KV rows are keyed by namespace string alone, not by
  libsql db path, so every per-root db also salts its KV namespaces; the
  no-root namespace stays unsalted.
- libsql: an unfiltered `COUNT(*)` or `COUNT(DISTINCT ...)` over an `F32_BLOB`
  vector table returns 0; `overview` counts through a `GROUP BY` subquery.
- Call edges are one KV row per file, never per edge: per-edge rows made
  deletion a full-namespace scan per indexed file.
- `index_with_dead_code`: the likely-orphaned-symbol scan is off by default
  because `index_cfg` also backs `codeinsight_overview`, which runs on every
  `instruction` dispatch.
- `scan_literal`: `LITERAL_SCAN_MAX_FILES`/`LITERAL_SCAN_MAX_FILE_BYTES` are
  independent of `IndexConfig::digest_max_files`/`max_file_bytes` (digest and
  embedding cost bounds); reusing those drops real source from an "every match"
  answer -- measured on a real workspace, the digest cap (2000) undercounted a
  2427-file tree, and the 256KB embedding cap excluded genuine 300KB+ source
  files. Every bound hit clears `exhaustive`, except `files_skipped_binary`,
  which never clears it: a non-text file cannot hold a text match, so skipping
  one is not a gap.
- `scan_literal` skips the `dual`-mode retrieval machinery entirely (digest
  diff, `index()`, query embedding, vector search, fusion) -- routing a
  workspace-wide literal query through that machinery instead measured
  120-420s; skipping it is what keeps an exact-match answer fast.
- `list_scan_universe` is asked for `file_cap + 1` entries so hitting the cap
  is distinguishable from a tree that is exactly cap-sized, which is what makes
  `files_truncated` accurate at the boundary.
- `host_read` returns `None` for both IO failure and non-UTF-8 content;
  `scan_literal` tells them apart with `host_stat` (stat ok means binary skip,
  stat failed means an unreadable gap).
- Lowercasing can change byte length (U+0130): `LiteralMatcher::find_all` falls
  back to a char-aligned scan when lengths differ, match text is taken with
  `get` (never a slice index, for the same reason), and it returns every match
  per line rather than the first, since two matches on one line are two real
  call sites for a call-graph trace.

### ragconfig.rs

- `IndexConfig::pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound`
  = 16000: the worst measured wasm BERT cost is 3.6-15.2 s per chunk. It divides
  the remaining wall budget into a per-file chunk allowance; a lower value lets
  one slow file overrun the wasmtime epoch deadline and poison the Store.
- `IndexConfig::digest_max_files`/`prune_enumeration_file_cap`: files past the
  cap are silently ignored (stale chunk rows survive, the digest can report
  converged), so large monorepos must raise them.
- `BulkEmbedBudgetConfig::git_commit_sync_hard_ceiling_ms` is an elapsed-time
  stop independent of the `git_commit_min_embeds_per_pass` floor: `git show -p`
  cost scales with diff size (measured ~43 s/commit on a binary-heavy repo).
  Commits over `git_commit_full_diff_max_changed_lines`/`_max_files` (read from
  cheap `--shortstat`) embed their subject only; the file cap catches binary
  diffs that count ~0 lines.
- `.gm/index-config.json` (`apply_project_local_index_overlay`) sits outside
  the config tiers because `config::resolve_with` returns a project-vendored
  tier whole and drops lower tiers: a project tier holding one index tweak would
  lose the config-source prose, fsm and messages. Its lists only append.
- `RetentionConfig` only reclaims space behind already-tombstoned rows and
  never tombstones a live row; pruning stays an agent decision.

### wasm_dispatch/verbs.rs

- `confinement_violation`/`capability_access_violation` key off the caller's
  self-declared `discipline` field. The spool ABI carries no unforgeable caller
  identity, so omitting `discipline` bypasses both: they catch accidental
  cross-namespace access, they are not a security boundary.
- `codesearch_at_root` skips the cwd-bound fusion/BM25/dataflow machinery on
  purpose; it is tied to the current project's db and would mix roots.
- `codesearch_exhaustive` (literal/regex) is dispatched before the root branch
  and every digest/index/embedding step, none of which it reads; routed later,
  a literal query on a large workspace took minutes.
- `CODESEARCH_MODES`/`CODESEARCH_LIMIT_FIELDS`/`codesearch_result_limit` exist
  because an unrecognized `mode` and an unread `max_results` both used to be
  silently dropped (falling through to `mode: "dual"` / the default `k`) with
  nothing telling the caller its instruction was ignored; the limit function's
  bool return lets `codesearch_exhaustive` bound results only when the caller
  actually stated a limit.
- `browser` and `cdp` share `host_browser_exec`; the engine travels in the opts
  JSON (`"engine"`), never inside the code body, so the host picks
  lightpanda/steel/chrome without re-escaping caller JS.
- `git_commit`/`git_finalize` default to committing exactly what is already
  staged; blanket `git add -A` needs `add_all: true` (or, for `git_finalize`
  with no `paths`, stays the default there); `paths`/`files` stages only those
  pathspecs, so a shared writer's unrelated dirty files are never swept in.
  `git_commit`'s dedup cache (keyed on cwd+pre-commit-HEAD+message+paths,
  TTL'd via `GIT_COMMIT_DEDUP_TTL_MS`) replays the one real sha instead of
  re-running `add`/`commit` when a caller or host re-dispatches one logical
  commit request twice.
- `git_finalize` given `paths` scopes its porcelain checks to those paths
  (`git_porcelain_scoped`) and pushes by explicit ref (its own new HEAD)
  instead of the unscoped push path, so another writer's pre-existing dirt
  elsewhere never blocks the push.
- `git_push`'s explicit-`rev` path never rebases a dirty checkout by design;
  when the remote has since moved past that ref (e.g. a CI autobump), its
  rejection names the exact recovery (`git_pull` then `git_push
  {rev:"HEAD"}`) instead of leaving the caller to rediscover it.
- `git_pull` on a nonzero exit with no conflicts re-fetches and compares HEAD
  against the remote-tracking ref before trusting the failure: a slow
  post-merge hook/auto-gc/credential prompt can make the host report a
  timeout-kill after the fast-forward itself already landed.
- `git_log` parses `--pretty=format` on `\u{1f}` (never plain-text split, since
  subjects can hold spaces) for `sha`/`sha_full`/`author {name,email,date}`.

### Cargo.toml, embed.rs

- `slim` leaves out embed.rs's compiled-in safetensors model (the wasm-side
  embedding fallback). It is valid only under a host that implements
  `host_vec_embed` (agentplug-runner does); CI publishes the slim build.
- candle-* stay at 0.8: candle-core 0.11.0 does not compile for wasm32-wasip1
  with `default-features = false` (undefined `CurrentCpuF16`/`BF16` aliases).
- `EMBED_DIM` is the compiled model's width, not a setting: a model swap moves
  the weights, `EMBED_DIM`, `bge_small_config().hidden_size`,
  `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together.
- `condition_query`: BGE is asymmetric; queries carry `BGE_QUERY_PREFIX`,
  passages never do, and every query-side embed goes through it.
- `scoped_key`: one plugin instance serves concurrent projects, so embedding
  cache keys carry the project cwd; a hit compares the stored full key, since
  the slot is only an `fnv1a64` hash.

### rssearch_vectors.rs, libsql_wasm.rs, host_abi.rs

- `ann_query_sql`: `pool`, not `limit`, is both `vector_top_k`'s k and the
  outer LIMIT; recency rescoring and dedup run after retrieval and need it.
- `SCHEMA_ENSURED`/`MIGRATION_COMPLETE` are process-lifetime memos: code that
  destroys the tables calls `forget_ensured_schema`/`forget_migration_complete`
  (as `shared_db::recover_malformed_shared_db` does).
- `libsql_wasm::classify_error`: a parsed `ext=`/`rc=` code is authoritative
  and suppresses text matching; only `ShadowRow` is text-only. `Corrupt`
  deletes the shared db, so a message quoting "malformed" must never trigger it.
- `retry_on_busy`: the guest has no sleep import, so each retry re-enters the
  libsql plugin's 8 s busy timeout; `BUSY_RETRY_ATTEMPTS` x 8 s must stay under
  the host's per-dispatch deadline.
- `host_abi::git_call` turns an async `{pending, token}` envelope into
  `ok:false` (`porcelain_or_dirty` would read a shapeless value as a clean
  tree); only `git_step`/`git_poll` call `git_call_async`.

### plugin_abi.rs

- Drained to gm recall (`mem-ae3514f8ed9a27b4-980`, query "plugin_abi call
  envelope parse_response AbiErrorKind"): the envelope-over-body merge order
  in `call`, `parse_response`'s null/empty-object/no-`data` handling, and the
  frozen `AbiErrorKind` wire strings with their text-sniffing fallback.

### scan_deps.rs

- Drained to gm recall (`mem-73c705bb66f60897-863`, query "scan_deps
  find_suspicious_escapes walk_package is_force_included"):
  `find_suspicious_escapes`/`count_hex_obfuscator_idents` escape-shape rules,
  `walk_package`'s mtime+size package signature, and `scan_one_file`'s
  blocked-read-vs-empty-file distinction.

### config.rs, config_sync.rs, prose.rs

- `resolve_with`: a `ProjectVendored` win still runs the lower tiers'
  `load_repo_tier`/`load_implicit_default_repo_tier` for their side effect:
  that refresh fires `config_notify::record_change`, so upstream drift behind an
  override stays visible.
- `RESOLVE_CACHE` is keyed by the per-call `host_cwd_string()` (one plugin
  instance serves several projects); its 2 s TTL folds the ~10 `resolve()`
  calls of one dispatch into one git-backed resolution.
- `config_sync::ensure_current` re-runs `validate_repo_url` because
  `RepoSource` is a pub struct; the check sits where the string reaches `git`.
- `config_sync` keeps sync state and locks on disk beside the checkout, never
  in statics: the user tier's cache under `$HOME` is shared by every project and
  host process. `try_lock` is a non-recursive `mkdirSync` (atomic
  test-and-set) after creating the parent recursively.
- The host ABI has no rename: `config_sync::rename` and `memory_md`
  `rename_batch` run `fs.renameSync` through `host_exec_js`, since
  `host_fs_write` overwrites in place and readers can see a torn file.
- `prose::read_from_config_repo` reads `config::resolve().cache_dir`, never a
  hardcoded dir: tiers write to different caches.

### orchestrator/config_notify.rs

- A config-source change is PERSISTED at record time and drained onto the next
  `instruction` response body (the `update_available`/`discipline_policies`
  surface), because a change between two dispatches with no agent inside a
  verb call would otherwise be visible to nothing.
- Delivery-once is per session, not global: several agents share one
  process-wide plugin instance, so a global "delivered" flag would let
  whichever agent dispatches first swallow the notification for the rest.
  Each record keeps a `delivered_to` session-id roster instead; a
  `session_id` of `None` buckets under `"(no-session)"`, which still gets
  delivery-once semantics rather than re-notifying forever.
- No process-wide cache: `pkfs` anchors `STORE_PATH` against the dispatching
  cwd's project root per call, so two projects sharing one plugin instance
  never cross-read.
- `MAX_RECORDS`(32)/`MAX_SUMMARY_ITEMS`(24)/`MAX_RECORD_AGE_MS`(24h)/
  `MAX_DELIVERED_TO`(64) bound the store against a flapping config source and
  an unbounded delivery roster; each evicts oldest-first, and a record already
  past `MAX_RECORD_AGE_MS` cannot practically reach `MAX_DELIVERED_TO`.
- `read_records` degrades a torn/unparseable store to "no pending changes"
  rather than failing the dispatch -- this is an advisory surface. A failed
  `write_records` in `drain_for_session` is likewise not fatal: the caller
  still gets this dispatch's notifications, and the worst case is one repeat
  delivery next time.
- `record_change` skips a no-op resha (same sha before and after) and never
  retries a failed write (the spool dir being unwritable would fail
  identically); its id folds tier+shas+timestamp so two sources changing in
  the same millisecond stay distinct. `drain_for_session` keeps a record with
  an unreadable/absent `ts` rather than dropping it, and marks delivery on the
  drain itself since no separate acknowledgement verb exists.

### cache.rs, embed_marker.rs

- `cache.rs`: `shared_exec_params` binds every param as text and `shared_exec*`
  return no row count, so a SQL NULL travels as `''` unwrapped by
  `NULLIF(?6,'')`, and `invalidate` learns existence from a `get` first.
- `embed_marker::marker_rel_for_table`: dim-mismatch checks read a per-table
  marker; the shared `.gm/.embed-generation` marker let one store's record mask
  another table's stale embedding width.

### orchestrator (disciplines, fibers, calculus)

- `capability_proxy::resolve` and `discipline_note::requires_satisfied` must
  match providers identically: both go through `resolve_key_realm` (an unmapped
  key's `""` realm maps to the discipline realm), require an `Active` provider,
  and check its `provides`. Divergence splits KV access from activation.
- `discipline_note::active_policies` is the only caller of discipline
  `advance_fiber`, once per `instruction` dispatch; there is no push `notify`,
  so background fiber mutation would need one.
- `all_known_discipline_dirs` includes disabled names that still have state
  files, so a just-disabled discipline advances to `Inactive` instead of
  staying `Active`.
- `build_interception_context` folds every enabled discipline in `enabled.txt`
  order; `MergeKind::combine` is right-biased, so for `ScalarOverwrite` the last
  non-empty declaration wins.
- `handle_check_removal` is the crate's only writer of `enabled.txt`; it reads
  it exactly once and CAS-writes against that same snapshot, so a change
  concurrent with its safety evaluation is never silently overwritten.
- `fiber_lifecycle::transition` is mirrored arm for arm by agentplug-host
  `registry.rs` `PluginFiberLifecycle`; change both repos together.
- `calculus::verify_calculus` is the runnable counterpart of the Lean proofs in
  `formal/CordisCalculus/`; a model change needs both. It stops silently at
  `max_states`, so `ok` covers only the explored states.
- `calculus::Registry::unload`: `retired || !satisfied` equals the paper's
  `target != omega` only because base-model `reload` commits exactly the
  current target.
- `component_loader_dispatch::parse_entries` silently drops entries that fail
  to deserialize; `isolate` is the tagged `{"kind": "none"|"local"|"global"}`
  shape, not the paper's `true`/string form.
- `claim_audit_clean` passes only on the exact marker body `clean`; empty,
  truncated or unknown bodies fail closed.

### orchestrator (state, residual, instructions)

- `state.rs` `read_state_with_graph`/`set_phase_with_session_with_graph`:
  resolve `fsm::graph()` once per dispatch and pass it through; two resolutions
  can see different tiers and fail each other's edge check.
- `residual::handle_scan` writes `residual-check-fired` as
  `<session_id>:<fired_at_ms>`, which `transitions.rs` `residual_scan_fired`
  parses; mere existence must never pass the gate. Checks run in fixed order and
  the first failure ends the scan.
- `instructions::has_compiled_default_for_prose_key` must list exactly the keys
  `compiled_default_for_prose_key` matches, plus `entry`; unknown keys fall
  through to ENTRY prose.
- `instructions::write_turn_summary` takes `config_changed_count` from
  `handle`, which already drained `config_notify`; draining again marks records
  delivered to a session that never saw them.
- `instructions::handle` suppresses prose only when the caller asserts the hash
  it holds; `.last-instruction-hash-<sid>.json` records what was sent, not what
  arrived.
- `instructions::handle` inlines only `instruction_payload.mutables_pending_rows_inlined_limit`
  / `prd_items_rows_inlined_limit` rows; the counts (`mutables_pending_count`,
  `epistemic_gap`, `prd_open_count`) stay exact and a `*_truncated` block names
  the on-disk file and the `mutable-list`/`prd-list` verb that still serve the
  whole list. Unbounded arrays are what blew one first-turn response to 375 KB
  raw / 146 KB MCP-cleaned against a 146-row mutables file.
- `mutables::handle_add` upserts on `id` and collapses pre-existing duplicate
  ids, keeping a resolved row over an unresolved one so a witnessed obligation
  is never reopened by the collapse. Before this it pushed blindly, so one id
  could occupy several byte-identical rows and inflate both the payload and
  `epistemic_gap`. `handle_list` stays un-deduped: it is the full-fidelity view
  of the file.

### memory_md.rs

- `vector_table()` re-resolves the rssearch table name from config with its
  own hand-written SQL rather than reusing `crate::rssearch_vectors`'s
  resolution; both must resolve it the same way or a configured rename leaves
  this module querying a table that no longer exists.
- `has_stored_digest`/the sync pass both skip the code namespace: it is fed by
  the tree-sitter indexer, not by markdown memory files, so it has no corpus
  digest to sync here.
- `flat_vec_embedding` rejects a stale-width embedding by comparing against
  `cfg.dim()` rather than a bare literal, so a configured embedding-dimension
  change does not leave it silently rejecting every valid embedding at the
  `F32_BLOB` column.
- `KEYWORD_SCAN_MAX_FILES` (2000) bounds `keyword_scan`, a read path with no
  index behind it (files sorted by name for a deterministic bound); a corpus
  past this size has a working vector store in every healthy configuration,
  and this rung exists only for the unhealthy one. It is the last rung in
  `recall`'s fallback chain that needs neither an embedder nor libsql (unlike
  the flat-kv keyword scan, which is libsql-backed through `host_kv` and
  silently no-ops without a libsql slot) -- the plain-file md corpus is what
  survives an embedder/libsql outage, so a memo stored during one is still
  reachable.
- A merely-deferred sync pass (wall budget hit, nothing failed, nothing
  rekeyed) still stores its digest, tagged `:partial=N` so it is never
  mistaken for converged; without this, `has_stored_digest` stays false
  forever on any corpus large enough to defer (live-witnessed:
  `memories_md_meta` at 0 rows against 168 real `memories_md_files` entries,
  `memory_md_sync_partial` recurring every boot). Never stored when
  `failed>0`/`rekeyed>0`, and the orphan-prune stays gated on full
  convergence, since pruning decides what to `mark_deleted` by diffing the
  manifest -- acting on an incomplete view would delete live entries.

### orchestrator/instructions/mod.rs -- automatic supply-chain scan

- `automatic_supply_chain_scan()` runs `scan_deps` on every `instruction`
  dispatch, surfaced as the response's `supply_chain_scan` field, debounced
  by `.gm/.last-scan-deps-ts` at `SUPPLY_CHAIN_SCAN_DEBOUNCE_MS` (300000ms,
  matching this project's existing `sync.debounce_ms` convention rather
  than an invented number). Measured cost on this repo: 531ms for one full
  scan (17 tracked files plus `node_modules`'s already-incremental,
  changed-since-stamp pass) -- cheap enough to run passively without being
  asked, which is the point: a HiddenSpawn-class payload smuggled into a
  legitimate-looking commit (real incident, 2026-09, gm-mcp and gm-config)
  is exactly the class of thing nobody remembers to check for by hand. This
  does not replace `codesearch`-based literal-signature hunts for a known
  specific IOC once one is found -- `scan_deps`'s structural heuristics
  (size-ratio disproportion, dense `\uXXXX` escape runs) catch the general
  shape, not every possible disguise.

### orchestrator/dream_rsi.rs

- Drained to gm recall (`mem-4beb69b539b50f83-3404`, query "dream_rsi
  replay_score beta1 beta2 parallelism_bonus"): the Dream-RSI paper's Eq. 1
  replay-objective formula, `beta1`/`beta2` field rules, `round` semantics,
  the mean-score policy-evaluation rule, `seal()` tree topology, the
  `dream-replay-round` session-scoped windowed-replay protocol, and
  `max_online_rounds` admission-cap semantics.

### Other modules

- `dataflow::default_document` is never executed: `verbs.rs` runs
  `dataflow_exec::run` for recall/codesearch only when the tier is not
  `CompiledDefault`. `dataflow_exec::run` executes steps in declaration order,
  then fuse nodes (no topological scheduler); `plugin == "gm"` steps call
  internal functions directly, never a wasm self-call through the host.
- `legacy_reaper::RETIRED_ARTIFACTS` is a literal allowlist, never a glob and
  never `gm.db`/`.gm/memories`; `reap_key` hashes it into `.gm/.legacy-reaped`,
  so adding an entry re-runs the reap in every project.
- `mediator::SELF_LANG_VERBS` (`go`, `rust`, `cpp`, ...) share one dispatch
  arm but are not aliases: each reaches `shell_exec` as its own lang, so they
  stay out of `VERB_ALIASES`.
- `submodule_drift::submodule_head_sha`: `git rev-parse HEAD` inside an
  uninitialized submodule dir answers with the parent repo's HEAD (exit 0), so
  a dir without its own `.git` is skipped, never compared.
- `.gm/exec-spool/.turn-browser-witnessed` is a flat `{file: hash}` map written
  only by `browser_witness::record_witness`; `transitions.rs` reads the flat
  shape and tolerates a nested `witnessed_hashes` wrapper.
