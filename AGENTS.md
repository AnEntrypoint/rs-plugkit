# AGENTS.md

`plugkit-core` is gm's host-agnostic WASM guest: orchestration, spool verbs, code search and memory live in `crates/plugkit-core/src/`. `agentplug-runner` is the supported host. Follow the parent [AGENTS.md](../AGENTS.md) and the gm skill; [README.md](README.md) owns architecture, verb inventory, spool ABI and release details.

## Work and verification

- Use GM MCP and the served workflow. Structural questions start with `callers`/`impact`, then `codesearch`. Parallel agents get distinct spool session IDs and disjoint writer ownership.
- Source carries no comments except attributes, unsafe `SAFETY` rationale and license headers. Keep only non-obvious constraints here. UTF-8, no BOM, no decorative glyphs.
- No synthetic test code, fixtures, mocks or frameworks; a fuzz target driving the real spool-JSON and config-path parsers is permitted. Verify by a real build and a live dispatch through the owning verb and supported host, reading actual output; documentation-only changes need factual, scope and whitespace checks.
- Build `cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim`; lint `cargo clippy`; `cargo check -p rs-plugkit --offline` also compiles native-only branches. Parser changes need live malformed and boundary inputs (`fsm-validate` reaches `FsmGraph::validate`). Config and prose have no validate-only verb: resolve a real configuration through its verb. Where the container lacks the `wasm32-wasip1` target and rustup is read-only (`rustup target add` fails) or `/config/.cargo` is root-owned, run the local check as `scripts/cargo-local.sh check -p rs-plugkit --offline` (the script uses a `/tmp` CARGO_HOME when the default is unwritable); the wasm build then runs in CI (`.github/workflows/wasm-plugin-release.yml`) and its artifact is the witness.
- A verb ships as five places together: handler, `wasm_dispatch/verbs.rs` route, gates and transitions, the parent verb contract, the README inventory. Push scoped changes to `main` through GM git verbs; the signed release workflow owns version bumps.
- Keep this file below 30,000 bytes; compact only after revalidating it against source, history and notes.

## Module rules

Module-scoped rules live beside their source: [crates/plugkit-core/src/AGENTS.md](crates/plugkit-core/src/AGENTS.md) covers indexing and search, cache and memory, and dependency scanning; [crates/plugkit-core/src/orchestrator/AGENTS.md](crates/plugkit-core/src/orchestrator/AGENTS.md) covers disciplines, fibers, calculus and Dream-RSI replay.

## Embedding, vectors and host boundaries

- No candle dependency: the in-wasm bert model was removed because candle-core pulls zip 1.x, which is fully yanked. Restore from git history at 68ae320be2 or earlier only after a resolvable candle release exists. The build always uses the host `host_vec_embed` path.
- A model-width change updates weights, `EMBED_DIM`, `vecstore::EXPECTED_EMBED_DIM` and `EmbedDimConfig::default().dim` together; `memory_md::flat_vec_embedding` checks `cfg.dim()`.
- BGE queries carry `BGE_QUERY_PREFIX` via `condition_query`, passages do not; embedding cache slots compare the stored full key, though the slot name is an `fnv1a64` hash.
- `host_vec_embed` and `try_sibling_plugin_embed` are distinct host routes to one BERT model. `build.rs` embeds `PLUGKIT_SOURCE_SHA`; fix `PLUGKIT_BUILD_SHA` for byte-for-byte A/B comparisons.
- `crawl_cdp` is a required host import, so guest and runner ship together; `crawl` takes a plain-text body verbatim and serves only `engine=cdp`, which is always headful; a `headless` body is refused.
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

- `git_commit {patch}` or `{hunks}` (`git_commit_hunk_scoped`) commits through a temporary index built from HEAD and never touches the work tree; `git_add` refuses `hunks`. An unscoped commit carries the session's earlier `git_add` paths recorded under kv namespace `git_add_staged`.
- Scoped `git_commit` and `git_finalize` stage and commit only the caller's pathspecs, including the concurrent-write amend; a scope that stages nothing is refused, and an empty scoped add is never followed by an unscoped commit. Blanket staging needs `add_all: true`.
- `git_pathspec_scope` emits every exclude before the caller's pathspecs: on Windows git 2.46 stages nothing for `git add -- <untracked> <exclude>` (exit 0), and an exclude ordered last turns a scoped commit into an unscoped one.
- Every staging path (`git_add`, `git_commit`, `git_finalize`, porcelain probes, `git_push`'s dirty gate) appends `GIT_PROTECTED_PATHSPECS` (`:(top,exclude).gm`, `:(top,exclude).agentplug*`); receipts list them under `excluded`.
- `git_commit` deduplicates by cwd, pre-commit HEAD, message, paths and `amend` within `GIT_COMMIT_DEDUP_TTL_MS` (180000), replaying the real SHA; a replay counts only while HEAD still equals the recorded `sha_full`. `amend:true` refuses `pushed_commit_refused` and `amend_requires_head`, and during an active merge refuses explicit paths and `add_all`.
- `wasm_dispatch/dangling_refs.rs` validates before staging, so a refusal leaves the index unchanged: a dangling target is an existing, untracked, non-ignored file outside the commit's path set (generated output is ignored), and any existing unreadable scoped source blocks a clean scan unless waived.
- `git_status` preserves observed paths, diagnostics, head SHA and branch; failed or skipped status cannot establish clean. `truncated` only shortens the sample lists: `counts`, `by_directory` and `dirty` stay complete, and the default mode's full porcelain listing is in `spill_file`. `git_finalize {paths}` scopes porcelain checks to those paths and pushes by explicit new-HEAD ref.
- `git_push {rev}` never rebases a dirty checkout. A remote-moved rejection names the recovery: `git_pull`, then `git_push {rev:"<sha>"}`; `rev:"HEAD"` is symbolic. A remote 5xx is transient, retried `GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS` times.
- Git read fields: `git_log` takes `max_count` (aliases `n`, `limit`, `count`); `git_fetch` takes the refspec as `ref`, `rev` or `refspec` (`remote` defaults to `origin`); `git_show` pages with `offset` and reports `total_bytes` and `next_offset` (`null` on the last page). These three refuse unknown fields.
- `git_push` publishes a lineage: with no `rev` the source ref defaults to `HEAD` and `exec_git_push_in` sends `git push origin <source_ref>:<branch>`, so every commit on local `main` the remote lacks ships, including another lane's. A source ref that does not resolve to a commit (empty, `HEAD`, `@`, the branch name, a `refs/` or `origin/` ref) is refused while the delta is nonempty, and the reply lists `unnamed_commits` and the `rev` to pass. It is a naming gate, not an ancestry filter: `rev` names the tip and cannot exclude an ancestor. `allow_foreign_commits:true` publishes the tip after the caller read the delta. `git_finalize` always inserts the sha it publishes; `git_commit` never publishes, the push gate is the only publishing surface.
- A nonzero `git_pull` with no conflicts re-fetches and compares HEAD with the tracking ref before trusting the failure. Commit identity is passed per invocation as `-c user.*`, `-c author.*` and `-c committer.*` (`identity_config_argv`, fed by `commit_identity`) on commit, pull, merge, cherry-pick, revert and reland: body `author_name`/`author_email`, else gm config `git.author_name`/`git.author_email`, else `DEFAULT_COMMIT_AUTHOR_*` (name `lanmower`, email `lanmower@users.noreply.github.com`, the same address the CI release commits use, so the personal address stays out of history). Repository config is never read or written for it. Git verbs resolve the actual dispatch project and fail loudly outside a repository; `git_log` parses `--pretty=format` on `\u{1f}`.
- Spawning `git` with a command line past ~32 KiB fails on Windows with os error 206, reported as `git_status_incomplete`: exclude pathspecs stop at `GIT_PATHSPEC_EXCLUDE_ARGV_BUDGET_CHARS` (8000).
- `remote_moved` is only a label (any explicit-ref push rejection that is not an auth failure or transient 5xx); acting on it needs ancestry: `remote_moved_is_same_branch_advance` requires the remote tip to resolve, differ from the pushed sha, not be an ancestor of it, and share a merge base.

## Configuration, prose and notifications

### config.rs, config_sync.rs, prose.rs

- `update_checkout_in_place` is the fallback when the live checkout cannot be renamed aside; it rewrites files one at a time.
- `resolve_with`: a `ProjectVendored` win still runs lower tiers' `load_repo_tier` for its side effect, so `config_notify::record_change` keeps upstream drift visible.
- `RESOLVE_CACHE` is keyed by `normalize_project_root(host_cwd_string())` with a 2 s TTL; `normalize_project_root` cuts a root inside `<project>/.gm/config-source-cache*` back to `<project>`. Pass a dispatch's resolved FSM graph through `read_state_with_graph` and `set_phase_with_session_with_graph`.
- `RepoSource.repo` is a `config_path::RepoUrl`, parsed once in `parse_source_entry`; `config_sync` never revalidates it. Config repositories come only through approved remote transports: HTTP(S) with nonempty authority.
- Prose keys and source paths are untrusted relative paths: accept only safe components, since an unsafe prose key is terminal.
- The remote-tip probe is single-flight and machine-global in both tiers: `config_sync_native::ensure_current` and `config_sync::ensure_current` (wasm) share one record under `$HOME/.gm/config-source-remote-probe` (`global_probe_root`), keyed by (repo, reference). A probe runs when no tip is recorded, when `now - last_probe_ms` reaches the ref's interval (`change_interval_ms`, else `UNMEASURED_REF_INTERVAL_MS`), or when a recorded failure's backoff has elapsed. `last_probe_ms` advances on every probe, so a stable tip is re-probed at most once per interval; `advertised_since_ms` moves only when the sha changes. Falsified by more than one `git ls-remote` per window across N project roots (dispatch log). A **forced** refresh (`GitRepoFetcher::forced`, the `config-sync-now` verb) is the one caller the record may not answer: `probe_decision` returns `Due` for it and `ensure_current` reads the checkout's sha back afterwards, so an unexpired record can no longer report a stale tip as current. Wasm keeps sync state and locks beside the checkout; its `try_lock` is a non-recursive `mkdirSync`.
- The host ABI has no rename: `config_sync::rename` and `memory_md::rename_batch` run `fs.renameSync` through `host_exec_js`.
- `prose::read_from_config_repo` reads `config::resolve().cache_dir`, never a hardcoded directory; `read_clean` treats whitespace-only content as absent. `tier3_user_wide_repo`: a missing `.gm/instructions/source.json` falls through to defaults, an explicitly empty file means not configured.

### orchestrator/config_notify.rs

- A change is persisted when recorded and drained onto the next `instruction` response (`update_available`, `discipline_policies`); paths resolve through project-scoped `pkfs` per call.
- Delivery is once per session, not per process (agents share one plugin instance); each record keeps a `delivered_to` roster, and a missing session buckets under `"(no-session)"`.
- `MAX_RECORDS` (32), `MAX_SUMMARY_ITEMS` (24), `MAX_RECORD_AGE_MS` (24 h) and `MAX_DELIVERED_TO` (64) bound history and rosters, evicting oldest first. A torn store reads as "no pending changes". `record_change` skips no-op SHA changes; delivery is marked during the drain, since no acknowledgement verb exists.

### Instruction serving (`orchestrator/instructions/mod.rs`, `orchestrator/state.rs`)

- `instructions::handle` suppresses prose only when the caller asserts the hash it holds; `.last-instruction-hash-<sid>.json` records what is sent, not what arrives.
- Non-read-only replies carry `reply_hash` (`.last-instruction-reply-<sid>.json`): a caller asserting the current instruction hash and a matching `known_reply_hash` gets a delta, with equal fields in `unchanged_since_last_reply`, dropped ones in `removed_since_last_reply` and `FIELDS_ALWAYS_RESTATED_IN_A_DELTA_REPLY` inline. Key only on the caller's assertion, never on the server's last write; `{"full":true}` forces the whole envelope, and no `session_id` means no delta.
- `investigate_readonly` instructions serve no phase prose and stamp neither `last-instruction-ts` nor `last-dispatch-ts` (`gates::dispatch_serves_no_phase_prose`).
- Inline PRD and mutable rows are bounded by `instruction_payload.mutables_pending_rows_inlined_limit` and `prd_items_rows_inlined_limit`; counts stay exact and a `*_truncated` block names the file and serving verb.
- `has_compiled_default_for_prose_key` matches exactly the keys `compiled_default_for_prose_key` matches, plus `entry`; unknown keys fall through to ENTRY prose.
- `residual::handle_scan` writes `residual-check-fired` as `<session_id>:<fired_at_ms>`; existence never passes the gate, `yaml_util::invalidate_residual_marker(reason)` is the only clearing path, and a `prd-add` answering `already_identical` must not invalidate.
- `automatic_supply_chain_scan()` runs `scan_deps` on every `instruction`, debounced by `.gm/exec-spool/.last-scan-deps-ts` at `SUPPLY_CHAIN_SCAN_DEBOUNCE_MS` (300000) through `pkfs::write_if_changed`; legacy `.gm/.last-scan-deps-*` files are read-only fallbacks.

### Mutables and PRD

- `mutable-add` requires a nonempty caller `id` and a payload field beyond the envelope; `status` counts as payload, so `{id, status}` reopens a row. `mutables::handle_add` upserts by `id` and collapses duplicates, keeping a resolved row over an unresolved one; `handle_list` stays un-deduplicated. `prd-list` defaults to brief rows; internal evidence and gates use `handle_list_full`.

## Lean graph and compiled prose

- `orchestrator/lean_graph.json` and `orchestrator/instructions/prose/*.md` are vendored copies of `gm-config/fsm/graph.json` and `gm-config/prose/*.md`; `fsm::default_graph()` parses the lean graph and `crates/plugkit-core/src/orchestrator/instructions/lean_prose.rs` maps each key to its file.
- Re-sync: copy both sources from the gm-config commit named by `DEFAULT_REPO_PINNED_SHA`, regenerate `lean_prose.rs` from every `prose/*.md` except `entry` and `entry-extended`, delete prose files gm-config no longer has, keep the pin on that commit.
- A state with `"entry": true` is a graph root and skips the unreachable-state check; a configured graph that fails validation makes `instruction` and `transition` return the error. A gate with `"advisory": true` never blocks; its message appears in the `advisory` array of the `transition` reply.

## Code search and symbols (crates/plugkit-core/src)

### code_index.rs, codesearch_rank.rs

- `ScanOutput::parse` accepts `content` as the Compact spelling of grep's `output_mode`.
- `UNCOVERED_SCAN_BUDGET_MS` (8 s): the file universe is git or a gitignore-honouring walk, so gitignored scopes are never opened and a zero-match reply there means not searched.
- `scan_uncovered_files` lists the scope with ignore rules off, drops files the index covered, and line-scans the rest.
- `corpus_blind_spot` holds when git tracks no file, git reports untracked files, or the listing is incomplete; it forces `listed_files_all_read_within_scope` false.
- `files_scanned_indexed` counts what the index read, `files_scanned_uncovered` what the disk pass added; `files_scanned` appears in both reply shapes, so an unopened scope never reads as a zero-match.
- `file_source` and `file_source_detail` appear on every reply, naming the corpus that answered.
- The compact codesearch_rank shape carries `path_glob`, `files_matching_glob` and `glob_matched_no_files`, so a scoped reply differs from an unscoped one.

### code_symbols.rs

- `package_of`: a Go package or Rust module is the basename of its defining file's directory.
- `file_reaches_package`: a file reaches a package from one of its directories or from an import whose last segment (split on `/`, `\`, `:`) equals the package name.
- Call edges carry bare names; a qualified `pkg.Name` pins its package before any edge is attributed.
- `Resolved.package` and `Resolved.dirs` are set when a qualified name pins the symbol; `Resolved.merges_definitions` is set when resolution fell back to the bare name.
- `callers` and `callees` filter only depth-1 neighbours to the qualified package; deeper hops are attributed in the reply.
- `SELF_HEAL_SYNC_WALL_BUDGET_MS` (45 s) bounds the extra incremental passes one dispatch spends beyond the top-up pass; `SELF_HEAL_SYNC_PASS_CEILING_MS` (20 s) caps one pass; `SELF_HEAL_SYNC_MAX_PASSES` (6) bounds the loop.
- Two passes with an equal `SELF_HEAL_PROGRESS_COUNTERS` tuple (12 counters) moved the index nowhere, which stops the loop.
- `drive_sync_to_completion` returns the last pass's report; `self_heal_passes` counts the extra passes.
- `outline` is exempt: it answers an incomplete index with `partial` true and decides the focus file in one pass.
- Other actions drive the index to completion and answer, with the reason, when the budget runs out.
- A busy store refuses the verb: another sync pass cannot fix that contention.

### scan_universe.rs

- `git_nested_worktrees` and `git_nested_untracked_failed` name the submodule checkouts that widen the corpus and whether their untracked listing failed; a failure is a blind spot.
- `nested_worktree_dirs` finds nested checkouts by listing, not `git ls-files --stage`, since git's parent listing names a submodule's tracked files only; it stops at the first `.git`, at `NESTED_WORKTREE_SCAN_DEPTH` (2) and at `MAX_NESTED_WORKTREES` (64).
- Nested discovery takes both `gitlink_dirs` and `nested_worktree_dirs`, since either can answer; untracked files inside each nested worktree join the corpus, so a new file is searchable without refresh.
- `nested_untracked_failed` is set when discovery hits `MAX_NESTED_WORKTREES` or a nested untracked listing is incomplete; a failed gitlink listing alone is not a blind spot.
- `scope_path_exists` tests a named scope under the root; `search.rs` answers a scope that resolves to nothing as an error, never an empty scan, because an unopened scope reads as a real zero.
- `git_join` answers on the union of the walk and git's listing when git's tracked set is empty, so an untracked tree is scanned rather than reported as covered and empty.

### crates/plugkit-core/examples/prd_yaml_guard.rs

- A native harness for the YAML write guard, since `pkfs` is wasm32-only; each guard is a pure text function driven with `std::fs` on a real file.

## Remote-tip probe and config sync (config.rs, config_sync.rs, config_sync_native.rs)

- Without a home directory `global_probe_root` falls back to the project's `.gm` directory, keeping per-project granularity.
- `SHARED_PROBE_WAIT_MS` (15 s): a dispatch waits for another process's in-flight probe of the same (repo, reference) before serving its checkout.
- `UNMEASURED_REF_INTERVAL_MS` (60 s) re-probes a ref with no measured cadence; the first probe that sees a new tip stores the measured `change_interval_ms`.
- A failure backoff caps at `PROBE_BACKOFF_MAX_MS` (1 h).
- A project whose checkout is behind a freshly recorded tip never probes; it materializes to that tip.
- `IN_MEMORY_REMOTE_RECORDS` is the in-memory twin of the disk record; without it a failed disk write re-runs `git ls-remote` on every dispatch.
- `global_remote_tip_with` adopts the lock holder's recorded answer and never sleeps; a forced caller that cannot hold the lock never learns the tip and never reports success.
- `ensure_current` skips the debounce for a forced refresh and when the clock is not sane (last probe zero or in the future).
- A recorded Skip sha equal to the checkout returns without probe or fetch; a different sha falls through to materialize.

## Row stores and PRD (orchestrator/row_store.rs, cas.rs, mutables.rs, prd.rs)

- The folder is the index: `.gm/prd/<id>.yml` and `.gm/mutables/<id>.yml`, one file per row; readdir is the enumeration, and a lane writing row A cannot clobber row B.
- `item_stem` folds characters outside `[A-Za-z0-9_.-]` to `-`, appends the id's `fnv1a64` when the stem changes (so `a/b` and `a-b` differ), and caps at `ITEM_STEM_MAX_CHARS` (80); the id stays verbatim in the body.
- Legacy `.gm/prd.yml` and `.gm/mutables.yml` are absorbed on the first write (`maybe_absorb`): rows merge into existing item files, and a row already in the commit log is never re-split.
- After absorption a marker (`prd.absorbed.json`, `mutables.absorbed.json`) suppresses the legacy file; the marker is written only once every group reached the folder, and the legacy file stays because projects track it.
- `parse_blocks` accepts a hand-edited single mapping or a sequence.
- `ReadSet` carries each item file's raw text, so `write_blocks` compare-and-swaps per item; removal is compare-and-swapped too, and a rewrite by another lane makes the operation retry.
- `legacy_stat`: the host answers `Value::Null` for a missing file, meaning absent, never size 0.
- `row_store::cas_retry_write_verified` is the store-backed twin of `cas::cas_retry_write_verified`; the contract is the same, written back per row.
- `cas::cas_retry_write_verified` counts a write done only when the re-read bytes still satisfy verify; a vanished row is a lost update, never success.
- `append_log` hash-chains entries: each carries `prev` (sha256 of the previous non-empty line) and `sha256` over the entry without that field, so truncation or edit is visible.
- `log_row` returns the newest entry for an id as a listable row marked resolved and source, so an id stays answerable after its file is cleared.
- Resolution moves material: `retire_to_log` appends to `.gm/prd-log.jsonl` or `.gm/mutables-log.jsonl` and removes the item file, so the folder holds open work only.
- `is_prd_state_pathspec` covers legacy files, row folders and logs; with one file per row no git verb needs a whole-file foreign-row filter.
- `canonical_persisted_witness_path`: `referenced_path_token` trims a leading `.`, so `.gm/witness-out/...` and `gm/witness-out/...` name one file.
- `prd_repair.rs`: other row-writing verbs refuse an unparseable PRD file, so one unquoted scalar locks the store; `repair_prd_store` re-quotes the scalar, backs the original up, and writes only if the copy re-parses and round-trips.
- `PIN_HISTORY_SEARCH_DEPTH` (50) bounds the pin-history walk, which also stops at the first commit that pinned a different value.

## Submodule pins and YAML repair (orchestrator/submodule_drift.rs, yaml_util.rs)

- `staged_gitlink_sha` is the pin the next commit lands, even before HEAD carries it.
- `commit_present`: an object the submodule clone never fetched makes `merge-base --is-ancestor` fail; a failed check is not proof that a pin moved backwards.
- `pin_direction_known` claims a direction only when both ends are objects this clone holds; a local-only phantom pin over a published tip is a forward correction.
- `pin_regressions` reports backwards pins in the index (what the next commit writes) and in this repo's history.
- `YamlFault`: line and column are 1-based, `substring` is the exact fault text, `token` the breaking token such as `": "` inside an unquoted prose value.
- `parse_fault` reads the position from the serde_yaml message and `Error::location()`, trying both so a fault is never dropped.
- `requote_fault` changes only the scalar owning the fault and returns None outside a `key: value` entry, so callers refuse rather than guess.
- `serialize_checked`: a document that does not round-trip is never written.
- `repair_text`: `max_passes` bounds the loop; prd-repair defaults it to 16.

## Verb handlers (wasm_dispatch/verbs.rs, verbs/git.rs, verbs/search.rs, verbs/web.rs)

- `VERB_OWNED_RESERVED_BODY_KEYS`: verbs own `_plan` (the async git replay id `git_async_entry` reads back) and `_lease_sessions`; other `_` keys are transport bookkeeping.
- `strip_reserved_body_keys` removes `_agentplug_lane_wait_ms`, which gm-mcp stamps on every JSON body, so strict validators refuse only misspelled caller fields.
- `APPEND_ONLY_PATH_SUFFIXES` and `APPEND_ONLY_PATH_PATTERNS` guard `/.gm/witness-log.md`, `/.gm/lean-walk/new-nodes.jsonl` and `/.gm/pool/ledger-<wave>.txt`: each is a shared append log, and a concurrent whole-file write took the witness log from 4675 lines to 3 (measured 2026-10-10). Every whole-file write to these paths is refused; only `fs_write {append:true}` lands.
- `prd.yml`, `mutables.yml` and the row folders are absent from the guard, since their verbs reconcile per row and must rewrite a row; the commit logs need no guard because a single writer appends one line under a CAS.
- `fs_read_directory` answers a bounded listing (`FS_READ_DIRECTORY_DEFAULT_LIMIT` 100, `FS_READ_DIRECTORY_MAX_LIMIT` 500) because `host_read` returns nothing for a directory.
- `FS_READ_DIRECTORY_ENTRY_CAP_KEYS` (`max_entries`, `entries_limit`, `entry_limit`): `limit` is a `fs_read` line-range alias.
- Appends are `fs_write {path, content, append:true}` (no `fs_append` verb is registered). The write runs `fs.appendFileSync` through `host_exec_js` with a 15000 ms timeout, since the host ABI has no append import; concurrent appenders cannot lose an update.
- `config_sync_now_force_immediate_refresh` answers `forced_refresh_unconfirmed` with `degraded_reason` when `r.degraded` is set, so a stale graph is never reported as synced.
- `CODEINSIGHT_SELECTOR_KEYS` reads `action`, `mode`, `verb` in that order, the first nonempty string winning; no selector returns the index overview.
- `agentplug_runtime_rule_applies`: the `.agentplug` rule covers an empty pathspec or a spec starting `.agentplug` after stripping `./`; the `agentplug` gitlink is not runtime state.
- `literal_scopes` are the patterns `looks_like_glob` rejects; a scan points at them directly, while a glob only filters a wider listing.
- `hoist_glob_paths` moves a glob written under `path`, `paths` or `files` to the `glob` field; left under `path`, the scan refused outright.
- `finish_scan_reply`: a rule exclusion costs coverage only while the excluded paths stay unread; once `uncovered_scan` reports complete, `exhaustive` keeps its result.
- The `dual` route builds `PathScope::from_body` and resolves it with `scope_paths_resolve` under `.`; without that, ranked channels answer a scoped query with project-wide hits.
- `verbs/tree_staleness.rs` stamps `stale_tree` on codesearch and codeinsight replies when the scoped repo (a submodule when `path` lies in one) is behind `@{u}` or its `FETCH_HEAD` is over 24 h old; it reads local refs only (one `rev-parse`, plus `rev-list` when the shas differ), never fetches, so behind counts are as of the last fetch.
- `fetch`: an empty converted text is a failed retrieval (`ok` false), never an empty answer.
- `page_or_clamp`: with no page the whole-document read answers, clipped tail included; with a page the bounded read reports page fields.
- Percent-encoding keeps search qualifiers literal (`:`, `>`, `<`, `=`, `/`, `@`, `*`, `!`, parentheses, commas), since github reads `user:`, `repo:owner/name` and `stars:>=100`.
- The wikipedia search uses `action=query` with `list=search`, since opensearch returns only titles; a caller-supplied MediaWiki url keeps its own action.

## Other contracts

- `dataflow::default_document` is not executed for CompiledDefault: configured pipelines run declared steps then fuse nodes, with no topological scheduler, and `plugin == "gm"` calls internal functions, not WASM self-dispatch.
- `legacy_reaper::RETIRED_ARTIFACTS` is an exact allowlist, never a glob and never a memory or database deletion; its hash invalidates the reap marker.
- `mediator::SELF_LANG_VERBS` share dispatch code with their base verbs but are not aliases; each keeps its own language for `shell_exec`.
- `submodule_head_sha` skips uninitialized submodule directories without their own `.git`.

