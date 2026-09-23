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
- `browser` and `cdp` share `host_browser_exec`; the engine travels in the opts
  JSON (`"engine"`), never inside the code body, so the host picks
  lightpanda/steel/chrome without re-escaping caller JS.

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

- `call` merges the `{abi, plugin, verb, body}` envelope over the body's own
  top-level fields, envelope keys last. Pre-envelope `libsql`/`bert` read
  top-level fields, so sending only the nested envelope breaks every call.
- `parse_response`: a null or empty-object reply (the host's empty pointer
  pair) is `PluginNotFound`; an `ok:true` reply without `data` returns the whole
  object minus `ok`/`abi`, which keeps legacy `rows`/`embedding` results.
- `AbiErrorKind` wire strings (`plugin-not-found`, `verb-not-supported`,
  `plugin-error`, `timeout`) are frozen. Without an explicit `kind`,
  `classify_failure` sniffs the "unknown verb"/"verb not supported" texts
  `verbs.rs` emits.

### scan_deps.rs

- `find_suspicious_escapes` requires an identifier shape, not printable ASCII
  (escaped CSS punctuation would fail the scan); `count_hex_obfuscator_idents`
  covers the escape-free `_0x` variant. A size ratio alone only warns.
- `walk_package` signs a package by max `mtime_ms` plus summed size (a dir
  mtime misses in-place edits) and walks node_modules per package with
  `list_dir`: `IndexConfig::is_force_included` is a substring match and would
  force-include every descendant.
- `scan_one_file`: a file over `MAX_SCAN_BYTES` warns without a content scan.
  `host_read` `None` with stat size > 0 is a blocked read (AV), which fails the
  scan; size 0 is an empty file.

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
- `handle_check_removal` is the crate's only writer of `enabled.txt` (CAS
  against a re-read taken just before the write).
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

### orchestrator/dream_rsi.rs

- The replay objective follows the Dream-RSI paper's Eq. 1: per-world
  `replay_score = quality - beta1*cost + beta2*parallelism_bonus`, where
  `quality` is the MAX node score observed in the revealed trajectory (not a
  sum -- matches the paper's best-attained-quality term), `cost` is the
  summed per-node cost (every node's cost is fixed at 1 by
  `evaluator_receipt`, so this already equals the paper's revealed-node
  count), and `parallelism_bonus` is observed-node-count divided by the
  count of distinct `round` values among those nodes (average attempts per
  decision round).
- `beta1`/`beta2` are required, non-negative, finite fields on every
  `dream-replay` call (`nonneg_f64_field`) -- never given a default, since
  they are the paper's fixed per-experiment hyperparameters and this crate's
  own admission-filter prose rejects unmeasured constants.
- `round` is caller-declared per discovery (`dream-discovery-record`'s
  optional `round`), carried through `seal()` into each sealed node.
  Siblings sharing a `round` represent one decision-round batch. Omitted
  `round` defaults to the node's sequential position at seal/parse time,
  giving one round per node (`parallelism_bonus` = 1.0, i.e. no bonus) --
  the honest default when a caller has not declared batching. This keeps
  every world sealed before this field existed parseable.
- A policy's evaluation score is the MEAN `replay_score` across all
  supplied worlds (the paper's R-bar), not a sum across worlds -- adding a
  world must not mechanically change which policy wins on its own. The
  strictly-higher-than-baseline no-regression selection rule is unchanged.
- `seal()` builds each sealed node's `children` from the discoveries whose
  `parent_id` points to it (real tree topology, including branching --
  more than one discovery may share a parent), never from `discovery_ids`
  array order. `dream-replay`'s one-shot BFS stays a cheap non-interactive
  approximation; `dream-replay-round` is the paper-faithful path (Section
  3.2/Algorithm 1): a session-scoped, stateful, round-by-round replay
  where the CALLER (the agent's own inference, not this crate) picks each
  round's batch from the current eligible set -- a policy's declared
  `roots` (always eligible, can reopen a new branch at any round) union
  the leaves of the revealed subtree (a revealed node with no revealed
  child yet). `max_rounds` (the paper's N2) and a policy's `max_nodes`
  both bound a replay; either cap reached closes it and folds that
  round's reveal into the closing tally, never truncates it. State lives
  at `.gm/dream-rsi/<session>/replay-rounds.json`, one record per
  `replay_id`, closed replays reject further calls.
- `register_policy` takes an optional `max_online_rounds` (the paper's
  N1), stored on the policy record with an empty `online_rounds` tally.
  When set, `record_discovery` for that policy requires an explicit
  `round` and rejects a NEW round value once `online_rounds.len()` would
  exceed the cap -- reusing an already-used round (a parallel sibling in
  the same batch) never counts twice. A policy without
  `max_online_rounds` is unbounded, unchanged from before this field
  existed. This is what actually forces the paper's alternate-and-improve
  structure: past the cap, the caller must seal and dream-replay (or
  dream-replay-round) before a fresh deployed policy can keep recording
  online discoveries.
- `dream-replay-round` never exposes the full frozen world to the caller,
  only each round's newly revealed nodes and the current eligible set --
  by construction, a policy revised from this feedback cannot be shaped
  around exact node ids, scores, or targets it was never shown, matching
  the paper's warning against overfitting policy logic to one frozen
  trace.

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
