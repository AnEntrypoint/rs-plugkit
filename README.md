# rs-plugkit

The wasm cdylib guest behind gm (`plugkit-core`), built to `plugkit.wasm`
(fat, with baked-in bert embedding weights) and `plugkit-slim.wasm` (no
weights, used whenever a real embed answerer exists out-of-wasm). Both ship
from `AnEntrypoint/plugkit-bin` and are consumed exclusively by
`agentplug-runner` (repo `AnEntrypoint/agentplug`), the sole host that loads
this guest. There is no standalone `plugkit.exe` CLI anymore, no private
self-update path, and no direct-loader fallback — the retired `gm-runner`
native host and the retired JS wasm-host (`plugkit-wasm-wrapper.js`) both
routed through code paths this crate no longer ships. (The `rs-exec` crate
is also retired and archived; this crate has never depended on it.)

## Architecture

`plugkit-core` exposes a single wasm entry point that agentplug-runner calls
per spool dispatch. The guest routes shared capabilities (`host_plugin_call`,
`host_vec_embed`) to sibling wasm plugins (`bert`, `libsql`, `treesitter`)
that agentplug-runner loads alongside it; browser automation and background
task management are native in `agentplug-host`, not implemented in this
crate at all.

State lives on disk under a project's `.gm/` directory: `prd.yml`,
`mutables.yml`, `exec-spool/{in,out}/`, `rs-learn.db`, `disciplines/<ns>/`,
`code-search/`.

## Spool dispatch ABI

Callers write request JSON to `.gm/exec-spool/in/<verb>/<N>.txt` (or
`in/<lang>/<N>.<ext>` for language-execution stems); the watcher processes on
read and writes `out/<N>.json` (metadata) alongside `out/<N>.out`/`.err` for
process-execution verbs.

Orchestrator verbs: `instruction`, `transition`, `phase-status`,
`mutable-resolve`, `memorize-fire`, `residual-scan`, `auto-recall`.

Wasm-direct verbs: `fs_read`/`fs_write`/`fs_stat`/`fs_readdir`, `kv`/`kv_get`/
`kv_put`/`kv_delete`, `exec`/`exec_js`, `fetch`, `env_get`, `recall`,
`codesearch`, `memorize`/`memorize-prune`, `health`, `filter`, the full git
verb family (`git_status`, `git_log`, `git_diff`, `git_show`, `git_branch`,
`git_add`, `git_commit`, `git_finalize`, `git_push`, `git_checkout`,
`git_fetch`, `git_rm`, `git_revert`, `git_reset`), plus `ci-status` (real
GitHub Actions workflow-run query), `prd-add`/`prd-list`/`prd-resolve`/
`prd-status`, `mutable-add`/`mutable-list`, `discipline-note`, `fsm-vendor`,
`submodule-check`, `sql_open`/`sql_query`/`sql_exec`/`sql_list_dbs`/
`sql_smoke`, `task-spawn`/`task-list`/`task-output`/`task-stop`,
`background-convert`, `kill-port`, `similarity`, `claim-audit`.

`git_finalize` bundles add -> commit -> porcelain-gate -> push in one
dispatch, then runs `ci-status` inline against the pushed commit's SHA: on a
green result it writes `.gm/exec-spool/.ci-validated` (`{"head_sha": "..."}`)
automatically, so a caller does not need a separate CI-poll-then-marker-write
round trip. On a non-green or unresolvable result, the response's
`next_dispatch` field names `ci-status` so the caller can re-check once CI
finishes.

## FSM graph

The phase graph (SPECIFY -> PROVE -> EMIT -> STATE -> CONC -> SEC -> RES ->
DECIDE -> COMPLETE by default) is data, not hardcoded control flow — a
project's `.gm/instructions/fsm/graph.json` (written by `fsm-vendor`) can
define a different phase set or gate shape. The COMPLETE gate's
`ci-validated-fresh` check compares `.ci-validated`'s `head_sha` against the
current `git rev-parse HEAD`; a stale or missing marker denies the
transition and names `ci-status` as the next verb to dispatch.

## Prose resolution

Phase-specific behavioral prose (served by the `instruction` verb) resolves
through a three-tier chain per key: `.gm/instructions/<key>.md` (project
vendored override) -> a configured source repo synced into
`.gm/instructions-source-cache/` -> the compiled-in default under
`crates/plugkit-core/src/orchestrator/instructions/prose/*.md`
(`include_str!`'d at build). Editing the compiled default requires a push
to this repo and a cascade rebuild — it is never read live from this
checkout.

## Build

```bash
cargo build --release
```

Outputs `target/wasm32-wasip1/release/plugkit.wasm` (or `plugkit-slim.wasm`
via the slim build profile). Release artifacts for the wasm target are
produced by `.github/workflows/release.yml` on `git push` to `main`, and
published to `AnEntrypoint/plugkit-bin` as both npm packages
(`plugkit-wasm`) and GitHub Releases assets, sha256-verified alongside each
resolved release tag.

## Cascade

A push to `AnEntrypoint/{rs-codeinsight, rs-search, rs-plugkit}` triggers
`cascade.yml`, which lands here and builds the single `plugkit.wasm` +
`plugkit-slim.wasm` pair via `release.yml`. `agentplug` consumes that
release artifact as a separate downstream pipeline, decoupled at the
`plugkit-bin` release boundary — it is not itself a stage in this cascade.

## Observability

The watcher emits structured `evt:` lines to `.gm/exec-spool/.watcher.log`
(dispatch timings, code-index sweep progress, config resolution, boot
markers), rotated at 10MB. Runtime diagnostic files at `.gm/exec-spool/`
root (`.status.json`, `.turn-summary.json`) are plain JSON, readable
directly.
