# AGENTS.md

`plugkit-core` is gm's host-agnostic WASM guest: orchestration, spool verbs, code search and memory live in `crates/plugkit-core/src/`. `agentplug-runner` is the supported host. Follow the parent [AGENTS.md](../AGENTS.md) and the gm skill; [README.md](README.md) owns architecture, verb inventory, spool ABI and release details.

## Work and verification

- Use GM MCP and the served workflow. Structural questions start with `callers`/`impact`, then `codesearch`. Parallel agents get distinct spool session IDs and disjoint writer ownership.
- Source carries no comments except attributes, unsafe `SAFETY` rationale and license headers. Keep only non-obvious constraints here. UTF-8, no BOM, no decorative glyphs.
- No synthetic test code, fixtures, mocks or frameworks; a fuzz target driving the real spool-JSON and config-path parsers is permitted. Verify by a real build and a live dispatch through the owning verb and supported host, reading actual output; documentation-only changes need factual, scope and whitespace checks.
- Build `cargo build -p rs-plugkit --release --target wasm32-wasip1 --features slim`; lint `cargo clippy`; `cargo check -p rs-plugkit --offline` also compiles native-only branches. Parser changes need live malformed and boundary inputs (`fsm-validate` reaches `FsmGraph::validate`). Config and prose have no validate-only verb: resolve a real configuration through its verb.
- A verb ships as five places together: handler, `wasm_dispatch/verbs.rs` route, gates and transitions, the parent verb contract, the README inventory. Push scoped changes to `main` through GM git verbs; the signed release workflow owns version bumps.
- Keep this file below 30,000 bytes; compact only after revalidating it against source, history and notes.

## Module rules

Module-scoped rules live beside their source: [crates/plugkit-core/src/AGENTS.md](crates/plugkit-core/src/AGENTS.md) covers indexing and search, cache and memory, and dependency scanning; [crates/plugkit-core/src/orchestrator/AGENTS.md](crates/plugkit-core/src/orchestrator/AGENTS.md) covers disciplines, fibers, calculus and Dream-RSI replay.

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
- `git_pathspec_scope` emits every exclude before the caller's pathspecs: on Windows git 2.46 stages nothing for `git add -- <untracked> <exclude>` (exit 0), and an exclude ordered last turns a scoped commit into an unscoped one.
- Every staging path (`git_add`, `git_commit`, `git_finalize`, porcelain probes, `git_push`'s dirty gate) appends `GIT_PROTECTED_PATHSPECS` (`:(top,exclude).gm`, `:(top,exclude).agentplug*`); receipts list them under `excluded`.
- `git_commit` deduplicates by cwd, pre-commit HEAD, message, paths and `amend` within `GIT_COMMIT_DEDUP_TTL_MS` (180000), replaying the real SHA; a replay counts only while HEAD still equals the recorded `sha_full`. `amend:true` refuses `pushed_commit_refused` and `amend_requires_head`, and during an active merge refuses explicit paths and `add_all`.
- `wasm_dispatch/dangling_refs.rs` validates before staging, so a refusal leaves the index unchanged: a dangling target is an existing, untracked, non-ignored file outside the commit's path set (generated output is ignored), and any existing unreadable scoped source blocks a clean scan unless waived.
- `git_status` preserves observed paths, diagnostics, head SHA and branch; failed or skipped status cannot establish clean. `truncated` only shortens the sample lists: `counts`, `by_directory` and `dirty` stay complete, and the default mode's full porcelain listing is in `spill_file`. `git_finalize {paths}` scopes porcelain checks to those paths and pushes by explicit new-HEAD ref.
- `git_push {rev}` never rebases a dirty checkout. A remote-moved rejection names the recovery: `git_pull`, then `git_push {rev:"<sha>"}`; `rev:"HEAD"` is symbolic. A remote 5xx is transient, retried `GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS` times.
- `git_push` publishes a lineage: with no `rev` the source ref defaults to `HEAD` and `exec_git_push_in` sends `git push origin <source_ref>:<branch>`, so every commit on local `main` the remote lacks ships, including another lane's. A source ref that does not resolve to a commit (empty, `HEAD`, `@`, the branch name, a `refs/` or `origin/` ref) is refused while the delta is nonempty, and the reply lists `unnamed_commits` and the `rev` to pass. It is a naming gate, not an ancestry filter: `rev` names the tip and cannot exclude an ancestor. `allow_foreign_commits:true` publishes the tip after the caller read the delta. `git_finalize` always inserts the sha it publishes; `git_commit` never publishes, the push gate is the only publishing surface.
- A nonzero `git_pull` with no conflicts re-fetches and compares HEAD with the tracking ref before trusting the failure. Missing committer identity must name local `user.name`/`user.email`; tooling never configures a global account silently. Git verbs resolve the actual dispatch project and fail loudly outside a repository; `git_log` parses `--pretty=format` on `\u{1f}`.
- Message verbs screen through `wasm_dispatch/attribution.rs`: `git_commit`/`git_amend`, `git_finalize` (commit and PRD-note flush), `git_merge` and `git_reword` load the policy once (`.gm/attribution.json` in the project git toplevel, else `~/.gm/attribution.json`; no file is off), refuse in `refuse` mode before any staging, and strip again after PRD notes are bundled. A bad file refuses (`attribution_policy_invalid`) and never reads as off; `author` is the commit identity when the body names none and the committer of a reword; receipts list `attribution_stripped`.
- Never spell a remote-tracking ref `origin/<branch>`: git resolves a bare `origin/main` to `refs/heads/origin/main` first, so `remote_tracking_ref` is `refs/remotes/origin/<branch>`. A caller-typed revision goes through `disambiguated_revision` (`ref_candidates` is one `for-each-ref` over heads, tags and remotes) and the receipt says `ambiguous_ref`/`ambiguous_refs` when a bare name matched several refs.
- `git_log` records end with U+001E and fields split on U+001F; it asks git for `limit + 1` commits to answer `truncated`, and trailers come from `%(trailers:only=true,unfold=true)` (git 2.22+; an older git answers `unsupported`), never from a regex over the body.
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

- `orchestrator/lean_graph.json` and `orchestrator/instructions/prose/*.md` are vendored copies of `gm-config/fsm/graph.json` and `gm-config/prose/*.md`; `fsm::default_graph()` parses the lean graph and `instructions/lean_prose.rs` maps each key to its file.
- Re-sync: copy both sources from the gm-config commit named by `DEFAULT_REPO_PINNED_SHA`, regenerate `lean_prose.rs` from every `prose/*.md` except `entry` and `entry-extended`, delete prose files gm-config no longer has, keep the pin on that commit.
- A state with `"entry": true` is a graph root and skips the unreachable-state check; a configured graph that fails validation makes `instruction` and `transition` return the error. A gate with `"advisory": true` never blocks; its message appears in the `advisory` array of the `transition` reply.

## Other contracts

- `dataflow::default_document` is not executed for CompiledDefault: configured pipelines run declared steps then fuse nodes, with no topological scheduler, and `plugin == "gm"` calls internal functions, not WASM self-dispatch.
- `legacy_reaper::RETIRED_ARTIFACTS` is an exact allowlist, never a glob and never a memory or database deletion; its hash invalidates the reap marker.
- `mediator::SELF_LANG_VERBS` share dispatch code with their base verbs but are not aliases; each keeps its own language for `shell_exec`.
- `submodule_head_sha` skips uninitialized submodule directories without their own `.git`.

