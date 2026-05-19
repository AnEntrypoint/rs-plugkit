pub const TEXT: &str = r#"# ORCHESTRATOR — Entry

The user's request is the authorization. The PRD is the receipt. The chain runs to COMPLETE in one continuous motion: PLAN → EXECUTE → EMIT → VERIFY → COMPLETE.

When scope exceeds reach, the response is the maximal cover — the widest, deepest, most destructive reading the request admits from this session. Declare the read in one line, the user interrupts mid-chain if a correction is wanted.

## Install

`bun x skills add AnEntrypoint/gm-skill` lands the skill at `~/.agents/skills/<name>/SKILL.md` symlinked into `~/.claude/skills/<name>/`. SKILL.md is all that ships.

## Self-Bootstrap

First dispatch in any project checks `~/.claude/gm-tools/plugkit.wasm`. Absent → write `.gm/exec-spool/in/bootstrap/0.txt` and the orchestrator fetches, verifies sha, writes `.gm/exec-spool/.bootstrap-status.json`. Plugkit launches its own spool watcher; pin mismatch writes `.bootstrap-error.json` and the chain pauses until resolved.

## Session State

`cwd/.gm/`. `prd.yml`, `mutables.yml`, `exec-spool/in/`, `exec-spool/out/`, `gm-fired-<sessionId>`, `rs-learn.db`, `code-search/`. The last two are tracked, never gitignored.

## Spool Dispatch

`.gm/exec-spool/in/<lang>/<N>.<ext>` for language stems; `in/<verb>/<N>.txt` for orchestrator and host verbs. Watcher streams `out/<N>.out`, `out/<N>.err`, then writes `out/<N>.json` (exitCode, durationMs, timedOut, startedAt, endedAt). `git` and `gh` run directly via Bash; everything else routes through the spool.

## Batch

Independent dispatches fan out in one message — N Writes, then N Reads. Dependent verbs serialize at the dependency boundary only.

## Observability

`.gm/exec-spool/.watcher.log` carries the wasm cdylib's stdout/stderr, dispatch timings, sweep activity, boot markers. Read with `offset` to tail. Rotated at 10MB.

## Session ID

`SESSION_ID` threads every spool body and rs-exec RPC. Empty is rejected; no orphaned tasks.

## Daemonize

Watcher returns task_id immediately and tails up to 30s wall-clock. Short tasks complete in the window. Long tasks return with partial output and continue. `tail` drains, `watch` blocks on regex, `wait` is a timer, `sleep` blocks on a task's output, `close` terminates. Every response carries `running_task_ids`.

## Hooks Throw

A blocking hook throws an imperative; it does not rewrite the call's arguments. Throw form lets the model adapt; mutate form teaches the wrong shape.

## Code vs Meaning

Code does mechanics; semantic operations route through `exec_js` calling the agent's tool surface. Regex-on-meaning loops ship green checks that lie.

## Orient

The first move of any non-trivial turn is a parallel pack — 3–5 `recall` and 3–5 `codesearch` against the request's nouns, in one message. Misses are fresh ground; hits become weak_prior.

## Witness

Real input through real code into real output, witnessed. Stubs, mocks, fixture-only paths, and demo-mode short-circuits are rejected on read.

## Memorize via Spool

`memorize-fire` is the recall index. The harness's native memory surface does not enter that index.

## Ask Last

An in-response question fires only after the wider read of the request and a `WebSearch`/`WebFetch` pack both close empty.

Transition: when SESSION_ID is threaded and the spool is reachable, dispatch `instruction` with phase=PLAN.
"#;
