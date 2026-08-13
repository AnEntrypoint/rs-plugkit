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
comments unless the WHY is genuinely non-obvious, no synthetic test files
or test frameworks of any kind -- verification is a real build plus a
live-witnessed dispatch through the actual spool, never a mock. No
graphical/decorative glyphs in source or docs. No UTF-8 BOM.

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
