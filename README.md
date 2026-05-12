# rs-plugkit

Unified CLI binary that bundles `rs-exec` (background-task spool + RPC runner),
`rs-search` (BM25 code retrieval), and `rs-codeinsight` (tree-sitter project
analyzer) behind one entry point. Installed at `~/.claude/gm-tools/plugkit.exe`
and self-updates on every invocation via `self_update.rs`.

## Subcommands

- `plugkit exec <code> [--lang nodejs|python|bash|rust|...]` — spool-backed code execution
- `plugkit search <query> [--root <path>]` — BM25 code search
- `plugkit codeinsight <path> [--json] [--cache] [--read-cache]` — codeinsight analyzer
- `plugkit hook <event>` — invoke a Claude Code hook (session-start, prompt-submit, etc.)
- `plugkit runner {start|stop|status}` — manage the persistent RPC runner
- `plugkit spool [--once]` — spool watcher daemon
- `plugkit kill-port <port>` — terminate the process holding a port

## codeinsight cache contract

`codeinsight` writes a **paired** cache to the analyzed root:

- `.codeinsight` — the analysis text/JSON (whatever the analyzer printed)
- `.codeinsight.digest` — an md5 digest of `(sorted [rel_path | mtime_secs], GIT_HEAD, dirty_count)`
  for every file `rs_codeinsight::collect_files` walked

The two files are written together by `--cache` and validated together by
`--read-cache`. Consumers MUST go through `plugkit codeinsight ... --read-cache`
to read the cache — never read `.codeinsight` directly, because there is no
way to tell from the file alone whether the cached content matches the live
tree. Direct readers ship stale context.

### Flags

| flag | behavior |
|---|---|
| (none) | run fresh analysis, print, do not write cache |
| `--cache` | run fresh analysis, print, write `.codeinsight` + `.codeinsight.digest` |
| `--read-cache` | if digest matches: serve cache verbatim; if digest missing or stale: run fresh analysis, write both files atomically, then print fresh output |

`--read-cache` always returns a result. There is no "cache miss exit code" —
miss is silently converted to a fresh run + cache write, matching the
standalone `codeinsight` binary's behavior. A stderr line
`[codeinsight cache <no_digest|digest_mismatch>; running fresh analyze]` signals
the rebuild path.

### Freshness digest covers

- file additions (new entry appears in `collect_files`)
- file deletions (missing entry shifts the hash)
- file renames (old rel_path gone, new rel_path present)
- in-place edits (mtime change)
- branch / commit switches (`GIT_HEAD` component)
- staged/working-tree mutations (`DIRTY` count component)

The digest is computed in parallel via `rayon` over the same file list the
analyzer consumes, so `--read-cache` never walks the tree twice on a miss.

### Used by

The plugkit hooks rely on this contract:

- `session-start` hook calls `codeinsight <dir> --read-cache` with a 1500ms
  budget, then spawns a detached `codeinsight <dir> --cache` in the background
  to prewarm the next session.
- `prompt-submit` hook calls `codeinsight <dir> --read-cache` inside a parallel
  thread group alongside `search` and `recall`, joined after recall finishes.
- `pre-compact` hook calls `codeinsight <dir>` (fresh) for full-detail context
  injection before compaction.

## exec spool

Code execution and utility verbs both flow through `~/.claude/<project>/exec-spool/`
(or `.gm/exec-spool/` when running inside a gm session). Languages live under
`in/<lang>/` (nodejs, python, bash, typescript, go, rust, c, cpp, java, deno);
verbs live under `in/<verb>/` (codesearch, recall, memorize, wait, sleep,
status, close, browser, runner, type, kill-port, forget, feedback,
learn-status, learn-debug, learn-build, discipline, pause, health).

Each task writes `out/<N>.out`, `out/<N>.err`, and `out/<N>.json` (metadata
sidecar with exitCode, durationMs, timedOut, startedAt, endedAt).

## Self-update

On every invocation `self_update.rs` reads `~/.claude/gm-tools/plugkit.sha256`,
compares against the running binary's embedded sha256, and spawns a detached
updater child if newer. The hot path never blocks. A lockfile guards
concurrent invocations.

To pin a version: write the desired version to
`~/.claude/gm-tools/plugkit.version`. To pause updates entirely: set
`PLUGKIT_NO_UPDATE=1` in the environment.

## Build

```bash
cargo build --release
```

Outputs `target/release/plugkit.exe` (Windows) or `target/release/plugkit`.
Release binaries for win32-x64, linux-x64, linux-arm64, darwin-x64, and
darwin-arm64 are produced by the Release workflow on `git push` to `main`.

## Observability

All subcommands emit structured JSONL to
`~/.claude/gm-log/<YYYY-MM-DD>/<subsystem>.jsonl`. Subsystems include `exec`,
`hook`, `plugkit`, `rs_codeinsight`, `rs_search`, `bootstrap`, `plugkit_wrapper`.

The `rs_codeinsight` subsystem logs `analyze.start` and `analyze.end` events;
on `analyze.end` the `source` field is one of `fresh`, `cache`, or
`fresh_after_miss` (with a `reason` of `no_digest` or `digest_mismatch`).
