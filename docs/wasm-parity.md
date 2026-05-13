# rs-plugkit wasm parity design

Status: design — implementation deferred to a multi-PR follow-on.

## Current state (v0.1.360)

`src/lib.rs` lines 56-230 implement a wasm-only `wasm_hooks` module that exposes 10 hook entrypoints (`hook_pre_tool_use`, `hook_post_tool_use`, `hook_session_start`, `hook_session_end`, `hook_user_prompt_submit`, `hook_prompt_submit`, `hook_pre_compact`, `hook_post_compact`, `hook_stop`, `hook_stop_git`). Each is a standalone reimplementation that does marker-file gating against `$CLAUDE_PROJECT_DIR/.gm/` plus prompt-injection. The native dispatcher under `src/hook/*.rs` is gated `cfg(not(target_arch = "wasm32"))` and is excluded from wasm.

Resulting `plugkit.wasm` is ~142KB. Compared to native plugkit, the wasm path is missing:

- rs-exec spool dispatcher (codesearch, recall, memorize, browser, nodejs, python, bash, ssh, runner, type, kill-port, forget, feedback, learn-status, learn-debug, learn-build, discipline, pause, health, wait, sleep, status, close)
- rs-learn libsql DB + retrieval (auto-recall on prompt-submit)
- rs-search index (codesearch verb backend)
- rs-codeinsight digest injection at session-start (this one is the closest to wasm-ready — pure tree-sitter, no fs spawn)
- Background daemonization
- Process spawning
- Disk-spool watcher

## Target host-import surface

plugsdk in browser mode must provide the following imports to plugkit.wasm. Names are illustrative; the actual ABI is defined by the wasm function signatures in `src/wasm_dispatch.rs` (to be added).

| Import | Wasm signature | Browser impl | Native equivalent |
|---|---|---|---|
| `host_fs_read` | `(path_ptr,len) -> packed_result` | OPFS read OR IndexedDB keyval | `std::fs::read` |
| `host_fs_write` | `(path_ptr,len,data_ptr,len) -> ok` | OPFS write OR IndexedDB | `std::fs::write` |
| `host_fs_readdir` | `(path_ptr,len) -> packed_result(json)` | OPFS / virtual fs | `std::fs::read_dir` |
| `host_fs_stat` | `(path_ptr,len) -> packed_result(json)` | OPFS metadata | `std::fs::metadata` |
| `host_fetch` | `(url_ptr,len,opts_ptr,len) -> packed_result(json)` | window.fetch | `reqwest` |
| `host_kv_get` | `(ns_ptr,len,key_ptr,len) -> packed_result` | IndexedDB | libsql blob lookup |
| `host_kv_put` | `(ns_ptr,len,key_ptr,len,val_ptr,len) -> ok` | IndexedDB | libsql insert |
| `host_kv_query` | `(ns_ptr,len,query_ptr,len) -> packed_result(json)` | IndexedDB cursor | libsql FTS |
| `host_vec_search` | `(query_ptr,len,k) -> packed_result(json)` | wa-sqlite + bge-small via transformers.js | libsql vector |
| `host_browser_spawn` | `(url_ptr,len) -> session_id` | iframe / window.open | playwright spawn |
| `host_browser_eval` | `(session_id,code_ptr,len) -> packed_result` | postMessage to iframe | playwright eval |
| `host_browser_close` | `(session_id) -> ok` | DOM remove | playwright close |
| `host_exec_js` | `(code_ptr,len,opts_ptr,len) -> packed_result` | Function() eval in worker | node child_process |
| `host_log` | `(level,msg_ptr,len) -> ok` | console + JSONL via host shim | stderr |
| `host_now_ms` | `() -> i64` | Date.now() | SystemTime |
| `host_env_get` | `(key_ptr,len) -> packed_result` | env-shim from query string / localStorage | std::env::var |

All return values are `u64` packed as `(ptr & 0xffffffff) | (len << 32)` per the existing `pack_result` convention in `src/lib.rs:39-47`. Allocation/free via existing `plugkit_alloc` / `plugkit_free`.

`host_exec_js` is intentionally a Function() eval — the browser cannot run node/python/bash sandboxed without a separate worker pool. Brownfield runtime in plugsdk wraps it with capability checks. Languages other than js inside the browser path become unsupported and fall back to a "language unavailable in browser" error returned through the spool.

## Verb migration

Each verb dispatches through a single wasm function `dispatch_verb(verb_ptr, body_ptr) -> packed_result`. Internal verb table:

| Verb | Browser impl |
|---|---|
| codesearch | host_kv_query against codeinsight digest stored in IndexedDB |
| recall | host_vec_search against rs-learn embeddings in IndexedDB |
| memorize | host_kv_put + queued host_vec_index |
| browser | host_browser_spawn + page.evaluate via host_browser_eval |
| nodejs | host_exec_js — js only, others reject |
| python | reject with "use exec:nodejs in browser parity v1" |
| bash | reject with same |
| ssh | reject — no network reach from browser sandbox |
| runner | start/stop a Worker via host_exec_js |
| type | host_browser_eval typing simulation |
| kill-port | reject |
| wait/sleep | setTimeout via host_now_ms loop |
| status | enumerate host_kv_query("tasks") |
| close | host_kv_put status=closed |
| forget | host_kv_query + host_kv_put tombstones |
| feedback | host_kv_put with channel=feedback |
| learn-* | host_vec_search debug paths |
| discipline | enable bit in IndexedDB |
| pause | host_kv_put status=paused |
| health | enumerate imports + return version |

## Leaf-crate work

| Crate | Wasm feature today | Real wasm impl needed |
|---|---|---|
| rs-exec | `wasm = []` empty flag | Reimplement spool dispatcher loop reading from host_kv_query("inbox") instead of fs::watch |
| rs-learn | `wasm = []` empty flag | Replace libsql with host_kv_* + host_vec_search adapters; gate dashmap/moka/rayon under not-wasm |
| rs-search | `wasm = []` empty flag, vector default gated | Replace candle/tokenizers with host_vec_search; embedding done host-side via transformers.js |
| rs-codeinsight | `wasm = []` empty flag, no cfg gates | Already wasm-friendly. tree-sitter parsers compile to wasm. Need to verify with cargo check. |

## Defensive CI

Add to each leaf's release.yml after the native build matrix:

```yaml
  wasm-check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-wasip1
      - uses: Swatinem/rust-cache@v2
      - run: cargo check --target wasm32-wasip1 --no-default-features --features wasm --lib
```

This fails-fast when a leaf change breaks the wasm feature, before triggering the rs-plugkit cascade build.

## plugsdk browser entry

Add `plugsdk/src/browser.ts` exporting:

```ts
import { WASI } from "@bjorn3/browser_wasi_shim";

export async function loadPlugkit(opts: { url: string; storage: "opfs" | "idb" }) {
  const wasi = new WASI([], [], []);
  const imports = makeHostImports(opts.storage);
  const { instance } = await WebAssembly.instantiateStreaming(fetch(opts.url), {
    wasi_snapshot_preview1: wasi.wasiImport,
    env: imports,
  });
  wasi.start(instance as any);
  return wrapHookCalls(instance);
}
```

`makeHostImports` provides the 16 imports listed above backed by OPFS+IndexedDB. `wrapHookCalls` returns `{ preToolUse, postToolUse, sessionStart, ... }` matching plugsdk's existing hook API. Replace `await import('node:wasi')` branch in current plugsdk with conditional import based on `typeof window`.

## Freddie wiring

Freddie's agent loop calls plugsdk hooks at:

- prompt-submit: before forwarding user message to LLM
- pre-tool-use: when LLM emits a tool-call, before allowing the call
- post-tool-use: after tool returns
- session-start: on chat open
- stop: before turn termination
- pre-compact / post-compact: around compaction events

Each hook's `additionalContext` / `systemMessage` is injected into freddie's running context window. Denials returned by pre-tool-use surface as tool-blocked errors in the chat transcript.

## Storage model

IndexedDB schema (single database `plugkit-wasm`):

- store `kv`: `(ns: string, key: string) -> value: ArrayBuffer`
- store `vec`: `(id: string) -> { embedding: Float32Array, payload: any }`
- store `tasks`: `(taskId: string) -> { verb, body, status, createdAt, completedAt }`
- store `keys`: `(provider: string) -> { value: string, addedAt: number }` (the nim-key UI source of truth)

OPFS used for large blobs (rs-search index, codeinsight digest) above 1MB.

## nim integration

`nim` is a local-only key proxy at C:/dev/nim — `.env` file holding 12 provider keys (NVIDIA/GROQ/CEREBRAS/GOOGLE/MISTRAL/CLOUDFLARE/OPENROUTER/SAMBANOVA/CODESTRAL/ZAI/QWEN/OPENCODE_ZEN), loaded by start.bat into acptoapi on `localhost:4900` with master key `AGENTAPI_API_KEY`. It is NOT a github repo and is unreachable from `https://anentrypoint.github.io/thebird` over the public web.

Implication: thebird's settings UI is the primary key authority for the github.io deployment. The UI optionally accepts a `NIM_URL` field — when filled (e.g. `http://localhost:4900` during local dev) the UI fetches the master proxy and uses provider-routed calls; when empty, the UI requires per-provider keys be typed directly. Either path writes to the IndexedDB `keys` store; freddie reads from there.

## thebird UI surface

`C:/dev/thebird/dist/app-shell.mjs` is the live AppShell. Tabs today: chat, term, preview (TAB_META). Adding a fourth `config` tab with a key-management view is one file change in app-shell.mjs plus a small `app/config.js` module. The view reads/writes `localStorage.agent_keys` (and mirrors to the IndexedDB store once plugsdk wasm dispatch lands). `window.__debug.config` exposes the API.

## Out of scope for v1

- `ssh:` verb (no browser network reach)
- `python:` / `bash:` verbs (sandbox)
- multi-tab session isolation (single-tab assumption)
- COOP/COEP for SharedArrayBuffer — host page must opt in via headers; github.io does not. Falls back to non-shared atomics where possible.

## Acceptance for v1 ship

- plugkit.wasm exposes `dispatch_verb` and all 10 hook entrypoints.
- plugsdk browser loader instantiates wasm + provides all 16 imports.
- freddie chat round-trip witnessed: prompt-submit fires, response context shows recall injection, hook log visible in `window.__debug.gm.lastHook`.
- todolist app generated via freddie chat renders in preview iframe, persists to IndexedDB, survives reload.

## Estimated work breakdown

| Item | Engineer-days |
|---|---|
| rs-codeinsight wasm-check defensive CI | 0.5 |
| rs-search wasm dispatch shim (drop candle, route to host_vec_search) | 3 |
| rs-learn wasm dispatch shim (drop libsql, route to host_kv_*) | 5 |
| rs-exec spool wasm dispatch shim | 4 |
| rs-plugkit wasm_dispatch.rs + verb table | 5 |
| plugsdk browser loader + 16 host imports impl | 8 |
| plugsdk OPFS+IDB storage layer | 3 |
| freddie hook wiring | 2 |
| thebird nim-key settings UI | 2 |
| browser e2e validation | 2 |
| Total | ~34 engineer-days |

Total fits 6-8 calendar weeks for a single engineer assuming CI cycles allowing one merged PR per leaf per day.

## Reachable in this session vs deferred

Status as of 2026-05-13 (rs-plugkit v0.1.364 / plugkit.wasm 296010B):

Reachable and landed:

- `wasm_hooks` (`src/lib.rs`) routes every marker / `.gm/*.yml` read+write through `wasm_dispatch::host_{read,write,exists}`, replacing the prior `std::fs::*` calls that silently no-op under the freddie-host WASI stub. PRD gate, mutables gate, needs-gm marker, residual-scan, lastskill — all real in browser.
- `wasm_dispatch.rs` extern block carries all 16 host imports (`host_fs_{read,write,readdir,stat}`, `host_fetch`, `host_env_get`, `host_kv_{get,put,query}`, `host_vec_search`, `host_browser_{spawn,eval,close}`, `host_log`, `host_now_ms`, `host_exec_js`). `dispatch_verb` table maps 20+ verbs onto them.
- `recall` / `memorize` / `codesearch` route entirely through host imports — no `rs_learn::Learn::new()` or `rs_search::Searcher` pull-in needed on the wasm path.
- `freddie-host.js::makeGmEnvImports` is a real impl backed by `instance.fs` (per-instance IndexedDB-snapshot fs), a `plugkit-wasm` IndexedDB `kv` store seeded into an in-memory map at boot for sync access, `window.fetch` via the queued-tasktoken+outbox pattern, a hidden iframe pool for `host_browser_*`, and `Function()` eval for `host_exec_js`. `host_env_get` reads `localStorage.agent_keys` then falls back to `fs.getConfig().env`.
- `window.__debug.gm` exposes `dispatch(verb, body)` plus shortcuts `recall`, `memorize`, `codesearch`, `fs_{read,write,stat,readdir}`, `env_get`, `browser_{spawn,eval,close}`, `kv.{map,db}`, `lastHook`, `trajectory`, `logs`. The `gm` freddie tool accepts `{verb, body}` for LLM-driven dispatch alongside the original `{hook, payload}` path.
- Browser e2e witnessed live at `https://anentrypoint.github.io/thebird/app/`: real recall hits scored by host_vec_search, real codesearch arrays, hook_stop returning `{decision:"block",...}` with real residual-scan reason text (proving wasm_hooks marker reads land), hook_user_prompt_submit returning hookSpecificOutput.additionalContext, hook_pre_tool_use returning permissionDecision based on needs-gm marker.

Deferred to follow-on PRs (genuinely out of single-session reach):

- Full libsql-in-wasm port for `rs-learn`. Current wasm path uses linear-scan `host_kv_query` and dot-product `host_vec_search` over a JS-side bucket. Adequate for hundreds-of-facts scale; libsql-vector at thousands-of-facts is the production target.
- Candle-in-wasm vector embeddings. Today embeddings are supplied externally (transformers.js or a remote endpoint); `host_vec_search` accepts a precomputed `embedding` field. Wiring transformers.js into the freddie boot path is its own 2-day item.
- Asyncify rewrite of `plugsdk/src/browser.js::syncify`. The plugsdk browser loader still throws on async host imports. thebird sidesteps this by wiring the wasm directly in `freddie-host.js::loadGmThebirdPlugin`; the plugsdk surface itself remains the documented future refactor.
- COOP/COEP headers for SharedArrayBuffer on github.io. github.io does not let users set response headers, so the wasm parity ships on non-shared atomics only. A custom-domain Cloudflare Worker is the planned escape hatch.
- Browser-side python/bash verbs. The dispatch table rejects them with "verb unavailable in browser; use exec:nodejs or host-side dispatch". Pyodide for python is a candidate (~10MB cost); bash sandboxing in-browser is unsolved upstream.
- rs-exec spool watcher in wasm. Today the verbs that target the spool watcher (status, wait, sleep, close, kill-port, forget, feedback, learn-*, discipline, pause, runner, type, browser) all reject in browser. A wasm-side spool would re-implement the inbox/outbox loop over `host_kv_query("inbox")` — order of 4 engineer-days per the table above.
