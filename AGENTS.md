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
cargo check -p plugkit-core
cargo build --release
```

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
2. Wire it into the verb-dispatch match in `wasm_dispatch/mod.rs` (or
   sibling entry point).
3. If it changes phase/gate behavior, update `orchestrator/gates.rs` and/or
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
- `ensure_schema_at_cfg`: the dim-mismatch drop must run before `CREATE TABLE
  IF NOT EXISTS`, which is a silent no-op against a surviving old-width table.
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
- The libsql plugin retains no connection: every `libsql_wasm` call opens the
  db by path, so per-file queries inside the index loop cost a full open each;
  batch them (`chunk_rows_by_path`).
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
  answer. Every bound hit clears `exhaustive`, except `files_skipped_binary`.
- `host_read` returns `None` for both IO failure and non-UTF-8 content;
  `scan_literal` tells them apart with `host_stat` (stat ok means binary skip,
  stat failed means an unreadable gap).
- Lowercasing can change byte length (`İ`): `LiteralMatcher::find_all` falls
  back to a char-aligned scan when lengths differ, and match text is taken with
  `get`, never a slice index.

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
