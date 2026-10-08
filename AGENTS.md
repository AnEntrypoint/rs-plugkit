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

- One gm wasm instance serves every project the daemon knows (the gm pool is
  process-wide), so any in-instance static cache must key on the dispatch's
  project: `project_scoped_cache_key` folds `host_cwd` in when no explicit root
  is given. A `""` key once served one project's BM25 corpus to another.

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
- `index_cfg_impl` walks the whole sorted listing every pass, starting at the
  path in `.gm/exec-spool/.codeinsight-cursor` (the first file the previous pass
  deferred) and wrapping. `max_files` bounds freshly extracted files per pass,
  never the listing: slicing the first N paths of the DFS listing meant a tree
  past N files (spoint: `packages/` after 500 `apps/`/`client/` paths) was never
  indexed, and a budget that always ran out in the same prefix starved the rest.
- The per-file chunk cap defers fresh embeds instead of truncating: every chunk
  whose embedding is reusable is kept, at most `cap` new ones are embedded, the
  rest count into the manifest's `skipped_no_embed` so the next pass re-extracts
  and continues. Truncating stored a 1-of-28-chunk file as complete forever.
  Embed failures also keep the pass partial, so they are retried next pass.
- The fresh-file allowance and the one-file floor grace are charged only after
  extraction shows the file needs a fresh embed; a reuse-only re-extraction (an
  older manifest, a touched mtime) costs no allowance, and the hash-match branch
  rewrites the manifest's mtime/size so the stat fast path hits next time.
  Otherwise N touched files took N top-up passes and never reached complete.
- `current_digest_cfg_at` folds a content hash per file, so reading the tree
  was unavoidable on every `ensure_current_insight` call and the in-instance
  `DIGEST_CACHE` (5 s TTL) is lost whenever the daemon evicts and reinstates
  the project -- which it does constantly across ~126 registry roots. Per-file
  `(mtime_bits, size) -> hash` now persists to
  `.gm/exec-spool/.codeinsight-file-digests.json`, so a repeat digest costs a
  walk plus stats and no file reads. The pair is the same identity the manifest
  stat fast path already trusts, and a miss only costs a re-read.
- `chunk_rows_by_path` is a full aggregate over `code_chunks` (measured 12 s);
  the pass's cleanup sweep reuses the map built before the walk instead of
  recomputing it, since every chunk written during the pass belongs to a path
  already in `files_set` and can never be an orphan.
- `ensure_schema_at_cfg` creates `code_chunks` with no index on `path`, so the
  `GROUP BY path` above scanned a 200 MB table and dragged every `body` and
  `F32_BLOB` through the reader; `_path` is now created beside the table and
  after `drop_if_dim_mismatch_cfg`, which drops the table and its indexes.
- A top-up on a tree whose stored digest is `:partial=` used to run on every
  `codesearch` dispatch, because `verbs.rs`' two auto-index sites tested
  `stale` where `ensure_current_insight` tests `stale && !prior_partial`. An
  over-budget digest never equals a fresh one, so the gate could not close and
  the tree re-walked forever. `topup_allowed` now admits a partial tree at most
  once per `PARTIAL_TOPUP_MIN_INTERVAL_MS` per project, so progress still
  resumes but one dispatch no longer costs one walk.
- Throughput bound, measured on spoint: bge-small in wasm embeds a 512-token
  chunk in ~5-7 s (opt-level z ~7 s, opt-level 3 + simd128 ~5 s), and the
  codesearch top-up budget is 4 s, so a cold 7000-chunk tree advances about one
  file per codesearch call; `codeinsight_index` (wall budget) is the bulk path.
  Manifests older than `FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS` could
  be silently truncated, so they are re-extracted once (embeddings reused).
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

### scan_universe.rs

- A scope the caller spelled out (`path`/`paths`/`root`) beats every exclusion
  rule: `named_scope` short-circuits `exclusion_rule` and skips
  `prune_own_state`. `.gm` is a gitignored dot-directory, so the default-walk
  rules hid the one tree an agent is most often sent to read -- a `grep` naming
  `.gm/mutables.yml` was refused outright. The rules stay in force for the
  unscoped universe, where they are what keeps a project-wide scan inside its
  budget, and a pruned `.gm` still reports as `gm_state_dir`.
- `path` is a literal path or glob and is never compiled as a regex; only
  `pattern` is. `grep`'s "the pattern was read as a regex because of its
  <reason>" suffix is appended only when `scan_literal` failed with
  `error_kind: "pattern"` -- attached to a scope error it read as "your path
  was treated as a regex alternation".

### code_symbols.rs

- Structural queries first run bounded refresh without embeddings and refuse incomplete graph evidence. `sync_files` also runs during indexing and stale-insight maintenance; schema 3 records a current-byte FNV64 `source_hash` so same-size/mtime edits invalidate reuse. Failed parses retry. Bump the schema when stored shape changes.
- Symbols, metrics and raw import specs live in `code_symbols`, `code_symbol_files` and `code_imports`, not vector chunks. Multi-row inserts stay within SQLite's 999-parameter bound.
- JSON has no structural symbol support: scoped queries reject it; source-only coverage excludes it explicitly and observed JSON clears prior cached symbols/imports/edges. Cleanup failures make coverage incomplete; line length alone must not defer supported bounded source.
- Resolved `index.max_file_bytes` has a 2 MiB minimum after typed configuration overlay; wall-time and indexed-chunk bounds still apply. The unnormalized default remains 256 KiB.
- Public `index.max_chunks_per_file_per_pass` bounds vector work only; `0` disables chunk embeddings while keeping bounded BM25 text and full text manifests. Text-only indexing still reports deferred vectors explicitly.
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

- `project_path_rejection` is the single path guard for `fs_read`/`fs_readdir`/
  `fs_stat`/`fs_write`; the three read verbs pass `caller_opted_outside_root(body)`
  and `fs_write` passes `false`. `READ_ONLY_OUTSIDE_ROOT_VERBS` is what makes the
  opt-in unreachable from a write even if a caller later routes `fs_write` through
  the same helper, so widening a read never silently widens a write.
- `allowOutsideRoot` is admitted only as a literal boolean `true`
  (`allow_outside_root`/`allowAbsolute` alias it) and only on the call that names
  the path -- nothing infers it from the path shape, so a call without it fails
  exactly as it did before. It widens WHICH root a read may address, never whether
  a read may climb out of one: `path_has_parent_traversal` rejects any `..`
  segment with or without the flag, and the refusal says so instead of naming the
  flag again. The refusal a caller sees without the flag names the flag, which is
  the whole discovery path for it.
- The guest guard is only half of this gate: `agentplug-host`'s
  `sandboxed_guest_path_with_extra_roots` independently refuses anything outside
  the project root, the user gm root, and the granted extra roots, so an admitted
  path still has to be granted before the host will serve it.
  `outside_root_read_granted` asks for that grant through the same
  `host_fs_allow_root` `scan_deps` already uses -- it adds no capability a caller
  could not already reach by dispatching `scan_deps` at that root. It tries the
  path itself, then each ancestor up to (not including) the drive root, because
  `host_fs_allow_root` grants directories only and `fs_read`/`fs_stat` are handed
  file paths. A marked ancestor grants its whole subtree, so a directory nested
  under one (`Temp\claude\<project>\<session>\scratchpad` under `Temp\.gm`) is
  served. A directory with no project marker on itself or any ancestor is still
  refused, and the refusal says which roots the host will serve
  rather than the "not found or empty" the host's silent `None` would otherwise
  surface. Coverage of arbitrary non-project directories needs the host side
  widened too, which is an `agentplug` rebuild and runner swap, not a gm change.
- `scan_deps` keeps its own absolute-root rule (`host_allow_root`, a directory the
  host grants) and is deliberately not part of this opt-in: it is a write-shaped
  scan that already had a grant mechanism, and folding it in would have widened a
  second surface for no caller need.
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
- `git_pathspec_scope` emits every exclude pathspec before the caller's own.
  git 2.46 on Windows silently stages nothing for `git add -- <untracked> <exclude>`
  (exit 0), so an exclude ordered last turns a scoped commit into a no-op
  staging plus an unscoped `git commit -m`, which is how a commit came to hold
  files nobody asked for. Excludes first is load-bearing, and
  `caller_pathspec_names` keeps an explicit `paths` from being withheld.
- A scoped `git_commit`/`git_finalize` passes `-- <paths>` to `git commit`
  (and to the absorb-concurrent-write `--amend`) and refuses when the paths
  stage nothing: the unscoped commit that used to follow an empty stage is
  what made a narrowed request widen to the whole index.
- `git_commit`/`git_finalize` default to committing exactly what is already
  staged; blanket `git add -A` needs `add_all: true` (or, for `git_finalize`
  with no `paths`, stays the default there); `paths`/`files` stages only those
  pathspecs, so a shared writer's unrelated dirty files are never swept in.
  `git_commit`'s dedup cache (keyed on cwd+pre-commit-HEAD+message+paths,
  TTL'd via `GIT_COMMIT_DEDUP_TTL_MS`) replays the one real sha instead of
  re-running `add`/`commit` when a caller or host re-dispatches one logical
  commit request twice.
- Every staging path (`git_add`, `git_commit`, `git_finalize`, the porcelain
  probes and `git_push`'s dirty gate) appends `GIT_PROTECTED_PATHSPECS`
  (`:(top,exclude).gm`, `:(top,exclude).agentplug*`) after the caller's
  pathspecs, so the project's own runtime state (the KV cache and the rest of
  `.gm/`) is never staged, committed or counted as dirt whatever `paths` or
  `.gitignore` say; receipts list them under `excluded`.
- `git_finalize` given `paths` scopes its porcelain checks to those paths
  (`git_porcelain_scoped`) and pushes by explicit ref (its own new HEAD)
  instead of the unscoped push path, so another writer's pre-existing dirt
  elsewhere never blocks the push.
- `git_push`'s explicit-`rev` path never rebases a dirty checkout by design;
  when the remote has since moved past that ref (e.g. a CI autobump), its
  rejection names the exact recovery (`git_pull` then `git_push
  {rev:"HEAD"}`) instead of leaving the caller to rediscover it.
- A remote-side 5xx (`Internal Server Error` / `Service Unavailable` / a GitHub
  `Request ID`) is classified as a transient remote rejection, not movement:
  `git_push` retries it itself `GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS` times
  with `GIT_PUSH_TRANSIENT_REMOTE_BACKOFF_MS` growth between attempts and only
  then fails, reporting every `Request ID` seen. Telling that caller to
  `git_pull` sends it in circles -- the local ref did not diverge.
- `git_pull` on a nonzero exit with no conflicts re-fetches and compares HEAD
  against the remote-tracking ref before trusting the failure: a slow
  post-merge hook/auto-gc/credential prompt can make the host report a
  timeout-kill after the fast-forward itself already landed.
- `git_log` parses `--pretty=format` on `\u{1f}` (never plain-text split, since
  subjects can hold spaces) for `sha`/`sha_full`/`author {name,email,date}`.
### Cargo.toml and embed.rs

- `slim` omits compiled-in safetensors and requires host `host_vec_embed`; the supported runner supplies it. Keep candle dependencies at the validated 0.8 WASM-compatible line unless the real target build proves a replacement.
- A model-width change updates weights, `EMBED_DIM`, `bge_small_config().hidden_size`, `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together.
- BGE queries carry `BGE_QUERY_PREFIX`; passages do not. Every query embedding uses `condition_query`. Project-scoped embedding cache slots compare the stored full key even though their slot name is an `fnv1a64` hash.
- `host_vec_embed` and `try_sibling_plugin_embed` are distinct host routes to the same BERT model. The sibling-plugin route preserves load/model diagnostics; it is not redundant merely because the model is shared.
- `build.rs` embeds `PLUGKIT_SOURCE_SHA`. Fix `PLUGKIT_BUILD_SHA` for byte-for-byte A/B comparisons; deleting source lines changes panic locations even when tokens are otherwise identical.
- `crawl_cdp` is a required host import: a runner that does not export it cannot instantiate this guest, so the guest and the runner ship together. `crawl` takes a plain-text body, so it is listed in `verb_body_must_be_json`'s non-JSON set and receives the body verbatim; `engine=lightpanda` reaches the `lightpanda` sibling through `plugin_call_text`, which passes the raw bytes rather than a JSON-encoded string.

### rssearch_vectors.rs, libsql_wasm.rs and host_abi.rs

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
  `ok:false` + `async_parked:true`; only `git_step`/`git_poll` call
  `git_call_async`. `porcelain_or_dirty` keeps its synthetic `?? git-status-failed`
  line for that parked case only, because it carries no status at all and reading
  it as clean would push over an unexamined worktree.
- `host_abi::porcelain_from` / `porcelain_or_dirty`: never launder a git failure
  into a fake dirty entry. Every consumer reads non-empty porcelain as "the
  worktree has work in it", so the old `?? git-status-failed` line made
  `worktree_dirty()` true and hard-blocked `git_finalize` with "worktree still
  dirty after commit (untriaged residual)". A real failure now returns the stdout
  git did produce plus `partial`/`failed`/`skipped_paths`, and `git_status`
  reports those as non-fatal fields. On Windows a path over MAX_PATH makes git
  warn `could not open directory '<p>': Filename too long` and silently drop
  entries while exiting 0 -- those are parsed into `skipped_paths`, so a
  truncated listing is never mistaken for a clean tree.
- `host_abi::git_call_async` retries once with `-c safe.directory=<repo>` when
  git refuses with "dubious ownership", and only when `<repo>` (the path git
  itself names) contains the git cwd. Windows worktrees created by an elevated
  process have a `.git` owned by BUILTIN/Administrators, so every git call --
  including `gm_dir`'s `rev-parse --show-toplevel` -- failed and `gm_dir`
  panicked (`wasm unreachable`) on every stateful verb, codesearch included.
- `host_read` recognizes the native `host_fs_read` packed empty-success marker `1` before pointer decoding. Only an actual successful empty read yields `Some("")`; `0` remains a read failure.
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
- Scoped `git_commit`/`git_finalize` stage and commit only caller pathspecs, including concurrent-write amend. Refuse paths that stage nothing; never follow an empty scoped add with an unscoped commit. Default commit uses already-staged content; blanket staging requires explicit `add_all` (unscoped finalize owns its documented default).
- `git_pathspec_scope` emits excludes before inclusions; reversing them can make Windows git silently stage nothing. Preserve explicit caller paths. Always exclude `.agentplug*`; distinguish ignored/untracked generated `.gm` state from explicitly requested tracked state. Receipts disclose withheld runtime dirt rather than silently widening delivery.
- `git_commit` deduplicates a logical request by cwd, pre-commit HEAD, message and paths within `GIT_COMMIT_DEDUP_TTL_MS`; replay the real SHA rather than execute another commit.
- `git_finalize {paths}` scopes porcelain probes and pushes its new explicit ref, so unrelated writer dirt does not block it. `git_push {rev}` never rebases a dirty shared checkout; remote movement returns the recovery `git_pull` then `git_push {rev:"HEAD"}`, while a remote 5xx is retried in-verb and reported with its `Request ID`s.
- After a non-conflict pull failure, re-fetch and compare HEAD with the tracking ref before trusting timeout/hook/credential failure: the fast-forward may already have landed. Missing merge committer identity must name local `user.name`/`user.email` requirements; authentication is not commit identity, and tooling must not configure a global account silently.
- Git verbs resolve the actual dispatch project and fail loudly outside a repository. `git_log` parses its formatted fields on `\u{1f}`, not spaces; subjects may contain spaces.

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
- `yaml_util::invalidate_residual_marker(reason)` is the ONLY thing that clears
  that marker, and it rewrites it to `invalidated:<reason>` rather than to empty
  -- the tombstone keeps the gate false while letting the denial name which
  verb (prd-add / mutable-add / prd-defer / mutable-defer) stale-dated the scan.
  `yaml_util::read_residual_marker` is the one parser; `residual_scan_fired`,
  `instructions::residual_check_fired_recently` and
  `transitions::predicate_detail("residual-scan-fired")` all read through it.
  A `prd-add` reporting `already_identical` must not invalidate: no PRD row
  changed, so the scan is not stale.
- `instructions::has_compiled_default_for_prose_key` must list exactly the keys
  `compiled_default_for_prose_key` matches, plus `entry`; unknown keys fall
  through to ENTRY prose.
- `instructions::write_turn_summary` takes `config_changed_count` from
  `handle`, which already drained `config_notify`; draining again marks records
  delivered to a session that never saw them.
- `instructions::handle` suppresses prose only when the caller asserts the hash
  it holds; `.last-instruction-hash-<sid>.json` records what was sent, not what
  arrived.
- Every non-read-only reply carries `reply_hash` (fnv of the full payload),
  stored in `.last-instruction-reply-<sid>.json`. A caller that asserts both the
  current instruction hash and `known_reply_hash` equal to that stored hash gets
  a delta: fields equal to the stored reply are elided and listed in
  `unchanged_since_last_reply`, dropped ones in `removed_since_last_reply`,
  `FIELDS_ALWAYS_RESTATED_IN_A_DELTA_REPLY` stay inline, `full_reply_at` names
  the file. Keying on the caller's assertion, never on what the server last
  wrote, is what keeps a fork sharing the sid, a lost response or a retry from
  eliding live state the caller never received; gm-mcp asserts the hash of the
  last reply it actually delivered. `{"full":true}` forces the whole envelope;
  a request with no `session_id` never gets a delta. This keeps the long-gap
  re-check cheap (an unchanged 12 KB reply measured ~1.4-2.5 KB).
- `gates::dispatch_serves_no_phase_prose`: an `instruction` in
  `investigate_readonly` mode serves no phase prose, so it neither refreshes
  `last-instruction-ts` nor stamps `last-dispatch-ts`; otherwise a 2 KB
  read-only call satisfied the long-gap gate mid-chain without delivering the
  recovery prose the gate exists for.
- `instructions::handle` inlines only `instruction_payload.mutables_pending_rows_inlined_limit`
  / `prd_items_rows_inlined_limit` rows; the counts (`mutables_pending_count`,
  `epistemic_gap`, `prd_open_count`) stay exact and a `*_truncated` block names
  the on-disk file and the `mutable-list`/`prd-list` verb that still serve the
  whole list. Unbounded arrays are what blew one first-turn response to 375 KB
  raw / 146 KB MCP-cleaned against a 146-row mutables file.
- `mutables::handle_add` refuses a body with no non-empty `id`, and refuses one
  whose only remaining keys are the dispatch envelope, writing nothing either
  way. Before this it generated `mut-<ms>` for a missing `id`, so a
  `mutable-add` dispatched with no body at all -- which the MCP layer turns into
  `{"session_id":...}`, never an empty string -- created an unaddressable
  placeholder row that read as real pending state. `status` counts as payload,
  so `{id, status}` still reopens an existing row.
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
- `has_converged_digest`/the sync pass both skip the code namespace: it is fed by
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
  by `.gm/exec-spool/.last-scan-deps-ts` at `SUPPLY_CHAIN_SCAN_DEBOUNCE_MS`
  (300000ms, matching this project's existing `sync.debounce_ms` convention
  rather than an invented number). The cache deliberately sits under
  `exec-spool/` and not bare in `.gm/`: a host project that tracks `.gm/` in
  git was left permanently dirty by every debounce tick, which blocked its
  own DECIDE -> COMPLETE stop-gate until a junk commit absorbed the churn.
  The debounce stamp must be rewritten on every scan, so no
  write-only-when-content-changed rule can hold it still; only the path can.
  The result JSON goes through `pkfs::write_if_changed` for the same reason.
  `.gm/.last-scan-deps-ts` and `.gm/.last-scan-deps-result.json` are still
  read as a fallback so an upgraded wasm reuses the previous scan instead of
  paying for a full rescan; they are never rewritten. Measured cost on this repo: 531ms for one full
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
- `admit_dispatch` RANKS, IT NEVER REFUSES. It returns `Admission::Allow` or
  `Admission::Advisory`, and the advisory is attached to a dispatch that ran
  (`dream_rsi_advisory`). It used to return `Err` for
  `codesearch|fetch|exec_js` once any gate-drift failure
  (`gate_denied`/`unknown_verb`/`retired_verb`) armed
  `replay-recorded-successes-first` for the session and that verb had no
  success newer than its own last such failure -- and a refused dispatch was
  never recorded as an observation, so the verb could never produce the
  success that cleared the refusal: a livelock, not a preference. The safety
  property the refusal was standing in for is already enforced one line
  earlier, because `gates::check_dispatch` runs first and denies on its own
  (long-gap, gate-repeat escalation), so the ranking layer only ever saw
  dispatches the gates had admitted.
- A successful `instruction` clears the ranking by stamping
  `.gm/dream-rsi/<sid>/reorientation-ts` AND the project-wide
  `.gm/dream-rsi/_any-session/reorientation-ts`
  (`PROJECT_WIDE_MARKER_SESSION`). The project-wide one is what makes the
  remedy reachable: the MCP `gm_instruction` tool dispatches under a
  server-local session id (`mcp-instruction-<pid>-<ts>`, gm-mcp
  `src/index.js`), while a `codesearch` body carries the caller's own
  session id, so a per-session marker alone could never clear a veto armed
  under the other id. `.gm/last-instruction-ts` is NOT a clearing path for
  the ranking either -- `gates::dispatch_serves_no_phase_prose` deliberately
  withholds it from `instruction` in `investigate_readonly` mode (a 2 KB
  read-only reply would otherwise satisfy the long-gap gate without
  delivering the prose that gate exists for); it is still read, because a
  non-readonly `instruction` does stamp it. The ranking also lapses
  `VETO_MAX_AGE_MS` (600 s) after the failure so a stale one cannot strand
  the verb for a whole session. `veto_reason`/`newest_marker` hold the
  decision with no host behind them so it can be tested: `cargo test -p
  rs-plugkit --lib` runs those four tests natively (`wasm_dispatch` is
  wasm-only, so a native test covers everything except the host reads and
  the verbs themselves).
- An active merge consumes the complete prepared index. `git_commit` refuses explicit paths and `add_all` before staging. Neither ordinary unscoped commits nor merge commits add commit pathspecs.
- `git_status` preserves observed paths, partial/skipped-read diagnostics, head SHA and branch. Failed, parked, truncated or skipped status cannot establish clean: commit/finalize refuse before mutation and string-only guards carry a non-porcelain failure marker.

### wasm_dispatch/dangling_refs.rs

- Validate before staging, so refusal leaves the index unchanged. Scan only already-staged files plus the proposed path/add-all scope, not sibling dirt.
- A dangling target is an existing, untracked, non-ignored file outside this commit's path set. An untracked target included in the same commit is valid; `git check-ignore` excludes ignored generated output.

- Every existing unreadable scoped source prevents a clean dangling-reference scan unless the caller explicitly waives the check. Empty readable files remain readable.

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

- `mutable-add` requires a nonempty caller-supplied ID and at least one payload field beyond the dispatch envelope. It never invents a placeholder row; `status` is a payload field.
- `prd-list` accepts ID/status filters and defaults to brief rows. Internal evidence, gates and instruction summaries use `handle_list_full`, not that abbreviated public view.
- `automatic_supply_chain_scan` runs on instruction, debounced by `.gm/exec-spool/.last-scan-deps-ts`/`SUPPLY_CHAIN_SCAN_DEBOUNCE_MS`. The cached result lives beside that stamp. Legacy root `.gm` paths are read-only fallbacks. Structural scans supplement, never replace, exact IOC searches.

## Dependency scanning

- `find_suspicious_escapes` requires at least four Unicode escapes decoding to an identifier, not arbitrary printable CSS punctuation. `count_hex_obfuscator_idents` covers escape-free `_0x` obfuscation; size ratio alone warns, never fails.
- Package signatures combine maximum mtime and summed bytes; directory mtime misses in-place writes. `walk_package` uses the actual dependency tree and containment/visited guards. `IndexConfig::is_force_included` is substring-based, including descendants.
- Oversized files warn without a full scan. A failed read after positive-size stat is a blocked read, not empty content; failed or blocked packages must not become stamped clean.
- `scan_node_modules` preserves earlier signatures for unchanged or budget-deferred packages. `full:true` clears the prior stamp before walking. Never silently report truncated scans as complete.

- Package signatures use `.gm/exec-spool/.scan-deps-stamp.json`; the legacy `.gm/scan-deps-stamp.json` is a read-only fallback. `full:true` clears both. Deterministic serialization and `write_if_changed` preserve identical cached bytes.

## Dream-RSI replay

- `dream-replay-cycle` is observation-only maintenance, separate from frozen-world
  policy replay. It uses canonical actual-owner `session_id`, verifies recorded ledger
  evidence through `automatic_replay`, and defers without an acknowledgment when no new
  verified dispatch exists. It preserves pending-pipeline gates and never refreshes
  agent phase clocks or records its own maintenance as a training observation.

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
