## 2026-08-12 - scan_deps: fixed a real correctness bug in the changed-since signature

**The per-package change signature (added the prior day, see below) was silently wrong, not just imprecise.** It stat'd only each package's own top-level directory and used that entry's `mtime_ms`/`size` as the signature. Live-verified on a real filesystem (`mkdir`/`touch`/`sleep`/edit-in-place/`stat`) that a directory's own mtime updates ONLY when a direct child is added/removed/renamed -- editing an existing file's content, anywhere in the tree including nested subdirectories, never touches any ancestor directory's mtime. That means a package modified by overwriting an existing file (a patch, a postinstall rewrite, or exactly the attack this scanner exists to catch) was silently and permanently marked "unchanged" after its first scan and never rescanned again.

**Fixed: the signature is now MAX `mtime_ms` + SUM `size` across every non-noise file actually visited in a full recursive walk of the package**, not the top-level directory alone. To avoid doubling the walk cost this would otherwise add (one walk to compute the signature, a second separate walk via the old `collect_package_files` to gather files to scan), the signature computation and the noise-filtered candidate-file collection are now the same single recursive walk (`walk_package`) -- every file is `host_stat`'d exactly once per dispatch regardless of whether its package turns out changed or not, and a changed package's candidate list is already in hand with no re-walk. A noise-named subdirectory is pruned before recursion (contributing to neither the signature nor the candidate list) -- an accepted, narrow blind spot (a change confined entirely to a noise dir stays undetected) traded for not paying real walk cost on the often-enormous noise trees under `node_modules` (fixtures, coverage output).

## 2026-08-11 - scan_deps: changed-since-last-scan stamp + noise-dir ignore list

**Fixed a real efficiency gap in `scan_deps` (shipped earlier the same day, see below): every dispatch re-walked the entire `node_modules` tree with no memory of what a prior scan already covered.** On a real ~21k-file tree that meant every session touching dependencies paid the same near-100s-worst-case cost, unbounded by whether anything had actually changed. Now walks `node_modules` per-top-level-package (each package dir, or each scoped package under an `@scope/` dir, gets its own change signature), comparing each package's `mtime_ms` + `size` (never mtime alone -- a bare mtime match is not a safe cache key, see `code_index.rs`'s own digest-fast-path reasoning) against a stamp file (`.gm/scan-deps-stamp.json`) written after each scan. An unchanged package is skipped entirely on the full per-file walk on every subsequent dispatch -- it already passed and nothing in it moved. Pass `{"full": true}` to force a full re-walk ignoring the stamp for a genuinely exhaustive one-off sweep.

**Also added a noise-dir/noise-suffix ignore list** (`test`/`tests`/`__tests__`/`docs`/`examples`/`fixtures`/etc. directories, `.map`/`.d.ts`/`.md`/`.css` files) on top of `IndexConfig`'s code-search-oriented defaults -- none of these are ever a payload carrier for this attack class and were pure scan volume (live-measured: 5916 `.map` files alone in one real `node_modules`, 73 test/docs/example directories).

**Discovered and worked around a real footgun in `code_index::collect_files`'s force-include mechanism** while implementing the per-package walk: `IndexConfig::force_include_path_substrings_overriding_every_skip` does a plain substring match, so an initial attempt to force-include the literal string `"node_modules"` (to unlock walking past that dir's own default `SKIP_DIRS` exclusion) also matched -- and therefore force-included, silently bypassing every skip filter for -- every single descendant path under it, since a substring match on a path prefix matches every path beneath it. The per-package walk (starting each sub-walk already past the `node_modules` segment, using the DEFAULT non-force-included config) avoids this entirely.

## 2026-08-11 - new scan_deps verb: supply-chain scan for HiddenSpawn-class compromise

**New `scan_deps` verb (`crates/plugkit-core/src/scan_deps.rs`, `Capability::ProjectPath`).** Detects the obfuscated-dropper supply-chain pattern confirmed across 17+ separately-compromised repos in an August 2026 org-wide incident: a payload appended after a file's real end (usually one extremely long whitespace-padded line) resolving a C2 address, fetching, decoding, and `eval`/`spawn`-ing attacker code. Checks two structural properties that survive the exact IP/wallet/decode-cipher changing in the next variant -- (1) a file whose byte size is wildly disproportionate to its line count, (2) a dense run (4+) of `\uXXXX` escapes decoding to an identifier shape -- rather than hardcoding today's known literal values as the primary detector. Scans git-tracked source in full (reuses `code_index::collect_files`, now `pub(crate)`) plus a bounded `node_modules` walk (a per-file size cap and a global file-count budget keep this fast enough for a per-session dispatch; live-measured against a real ~21k-file tree, an unbounded scan exceeded 100s). Response is structured JSON (`ok`, `failCount`/`warnCount`/`blockedCount`, `failing`/`warnings`/`blocked` arrays, `nodeModulesTruncated`) -- a blocked file read is itself surfaced as a finding, never silently skipped. Escape-detection is a hand-rolled parser (no new `regex` dependency for this feature) verified against 6 live cases including a real false positive an earlier prototype caught (a looser "decodes to printable ASCII" check wrongly flagged a legitimate escaped CSS-selector-punctuation string).

## 2026-08-06 - opt-in TencentDB-Agent-Memory-compatible memory backend

**New `tencentdb_memory` module: file-pointer + lean-index storage for the memory verbs.** `memorize`/`recall`/`memorize-prune`'s explicit-key path now route to this backend when a namespace is opted in via `gm.config.json`'s `memory.tencentdb_backend` (disabled by default). Unlike the default `rssearch_vectors` table, the new `tencentdb_memory_index` table stores only a namespace/kind/key/file_path pointer plus the embedding -- never inline text. Embedding dimension is independently config-driven (default 768), since it cannot share a vector column with the compiled-in 384-dim `bge-small-en-v1.5` embedder the default path uses.

**New `tencentdb_compat` module: read-only interop with real TencentDB Agent Memory `vectors.db` files.** Confirmed via research that no viable Rust/wasm32-wasip1 path exists to a SQLite engine understanding `sqlite-vec`'s `vec0` virtual tables (`rusqlite`/`libsqlite3-sys` have no documented wasm32-wasip1 C-compile support; the `sqlite-vec` crate is FFI bindings over `rusqlite`, same blocker; gm's own libsql vector store uses an incompatible native format). Uses the already-proven `host_exec_js` Node-shelling pattern (same mechanism `memory_md.rs::rename_batch` already uses) to read `l1_records`/`l0_conversations`/`skills`/`embedding_meta` matching `MemoryCore/src/core/store/sqlite.ts`'s schema exactly. New `tencentdb-compat-probe` verb for row-count/embedding-fingerprint diagnostics.

**Confirmed the memory_md sync starvation self-heals correctly.** Live-witnessed against gm's own project: a partial digest (`:partial=652` on an 892-file corpus) converges incrementally across repeated `recall`/`memorize-prune {query}` dispatches (311 -> 312 rows after one forced sync), and plain `recall` (no forced sync) returns correct real results even mid-convergence via its existing keyword/vector fallback tiers. No code change needed -- the `:partial=N` mechanism (already fixed once, per this file's own `memory_md.rs` comment) is working as designed.

## 2026-05-12 - session_start: kill+respawn watcher when plugkit version changed

`hook/session_start.rs::start_exec_spool` now compares the running watcher's
recorded `plugkit_version` (from `.gm/exec-spool/.last-session-start.json`)
against `CARGO_PKG_VERSION` of the spawning binary. On mismatch the watcher
PID is killed via `rs_exec::kill::kill_tree` and a fresh watcher is spawned
under the new binary. Without this, a long-lived session keeps an old watcher
process executing old `spool.rs` code (notably the pending-vs-work-dir code-file
race fixed in rs-exec 6fe3294) even after the on-disk binary is upgraded by
`try_promote_pending`. New-session watcher version now always matches the
binary running the hook.

## 2026-05-12 - ci(build): workflow_dispatch entrypoint for cascade smoke

`.github/workflows/build.yml` now also fires on `workflow_dispatch` with optional `upstream_repo` and `upstream_sha` inputs so upstream repos (rs-exec) can dispatch a downstream build smoke on PR without invoking `release.yml` (which auto-bumps and publishes). Push-to-main behaviour unchanged.

## 2026-05-12 - rebuild trigger to ship rs-exec spool plugkit-discovery fix

Force a non-`[skip ci]` build so the cascade rebuilds plugkit with the rs-exec change in commit 6fe3294 ("spool: fix plugkit discovery + code-file race"). Previous head was `chore: auto-bump version to 0.1.347 [skip ci]`, leaving deployed plugkit at 0.1.345/0.1.346 — versions whose `which_plugkit` resolves only `CLAUDE_PLUGIN_ROOT/bin/plugkit` and PATH, both empty under the gm-cc versioned-cache layout (`/root/.claude/gm-tools/plugkit` runs the watcher but is not on PATH). Symptom: every utility verb (codesearch, recall, memorize, status, health, ...) dispatched via `.gm/exec-spool/in/<verb>/N` resolved with `{"error":"plugkit not found in PATH","exitCode":-1}`. The patched `which_plugkit` adds `PLUGKIT_BIN` env, a `current_exe`-with-"plugkit"-substring match, and a last-resort `current_exe` return so the watcher's own ELF is the dispatch target whenever it embeds rs-exec.

## 2026-05-02 - prompt_submit: parallelize search + codeinsight subprocess spawns

prompt_submit.rs witnessed at 7007ms end-to-end in gm-log 5-min window. search and codeinsight subprocess spawns now run in parallel std::thread::spawn handles, joined after recall finishes. Output ordering preserved (search → recall → codeinsight). Expected reduction in hook latency for sessions where both contribute is roughly the cost of the shorter spawn.

## 2026-05-02 - hook obs: pre-tool autonomy field + dedupe prompt-submit fallback

pre_tool_use obs events now include `autonomous` (prd.yml exists) and `stage` (early|dispatch) so ccsniff gm-audit can distinguish legitimate autonomous-mode resumption from MISS first-action violations. prompt_submit fallback string for missing $CLAUDE_PLUGIN_ROOT/prompts/prompt-submit.txt shrunk from 5KB duplicate of canonical text to a 325-char pointer that fails loud, removing drift risk between rs-plugkit hardcoded copy and gm-starter canonical.

(Earlier commit added a prompt-submit.start event; reverted same day after witnessing the dispatcher-level wrapper already emits prompt-submit phase=start/end with dur_ms. The added event was duplicate observability fighting the no-parallel-surfaces rule.)

## 2026-05-02 - global needs-gm sentinel + fix ensure_tools_current + bootstrap stale partial cleanup

Global sentinel: prompt_submit and session_start now write ~/.claude/gm-tools/needs-gm in addition to the project-local .gm/needs-gm. pre_tool_use checks both, so non-gm projects (no AGENTS.md/.gm/) and non-gm-project sessions are now enforced just as strictly as gm projects. Sentinel cleared on gm:gm Skill invocation or autonomous mode.

ensure_tools_current: was copying from $CLAUDE_PLUGIN_ROOT/bin/ (JS wrappers only — no binaries). Now reads version from plugkit.version, resolves bootstrap cache dir (LOCALAPPDATA/plugkit/bin/v<ver>/ on Windows), copies platform-named binaries (plugkit-win32-x64.exe → plugkit.exe etc.) from there.

bootstrap.js stale partial: pruneOldVersions now detects stale locks (age > 5min or dead PID) and forces pruning of stale-locked dirs instead of skipping them. Also clears stale .partial files inside the current version dir before download to unblock stuck download retries.

## 2026-05-02 - session-start writes needs-gm to cover continuation-message bypass

session_start hook now writes .gm/needs-gm for every gm project (AGENTS.md or .gm/ present) at session start, unless prd.yml exists with content (autonomous mode). This closes the isMeta:true bypass where stop-hook feedback and short continuation messages skip UserPromptSubmit, leaving needs-gm unwritten and pre_tool_use unable to enforce gm:gm invocation first.

## 2026-05-02 - obs: trajectory_ingest event + prompt-submit-detail with project_dir/sess

spawn_trajectory_ingest now emits trajectory_ingest (pre-spawn) and trajectory_ingest_done (post-ingest) obs events to rs_learn.jsonl. prompt_submit now emits prompt-submit-detail to hook.jsonl with project_dir, sess, autonomous, and prompt_len fields — enabling correlation between hook fires and ccsniff session audits.

## 2026-04-24 - write needs-gm sentinel on stop-hook blocks

ccsniff audit found: stop hook feedback messages arrive as isMeta:true user messages, bypassing UserPromptSubmit hook. Model responds to git/CI block messages directly with Bash instead of Skill(gm).

Fix: run_stop() and run_stop_git() now write .gm/needs-gm before every block decision. Pre-tool-use hook then blocks non-gm tools even without prompt-submit firing.

Also added NEXT ACTION hint to all block reason strings.

# Changelog

## Unreleased

- fix: stop hook does not push-pressure agents on out-of-reach remotes. New `user_can_push_to_remote(project_dir)` helper in `hook/mod.rs` runs `gh api repos/<owner>/<repo> --jq .permissions.push` and caches the answer per project_dir. `run_stop_git()` now skips both the unpushed-commits check and the CI watch when the remote returns `permissions.push==false` (or no remote / non-github / gh missing). Uncommitted *tracked* changes still block (they're a local concern). On a clean tree against an out-of-reach remote, the stop hook approves with reason `remote is out of user reach (no push permission); local commits accepted, no push attempted, no CI watch`. This prevents agents from being prodded to push thoth/hermes/upstream forks where the user lacks write access.
- fix: session-end hook preserves browser + background tasks across session handoff. Previously closed on every SessionEnd regardless of reason — including `/compact`, `resume`, and background-agent handoffs — which killed the Chrome process tree that tests and agents were driving. Now only fires cleanup when `reason` is one of `clear | logout | prompt_input_exit`.
- fix: stop hook checks `.gm/prd.yml` (YAML) instead of legacy `.prd` (JSON)
