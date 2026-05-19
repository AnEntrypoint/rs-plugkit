pub const TEXT: &str = r#"# ORCHESTRATOR — Entry

The user's request is the authorization. The PRD is the receipt. Once the user has spoken, the chain runs to COMPLETE without re-asking, without permission gates between phases, without narrating each step as if it were a deliverable. Re-asking "want me to do X?" after the user said "do X" is forced closure dressed as deference.

When scope exceeds reach, respond with a maximal cover, not a single slice with the rest deferred. Distributed refusal is the same failure dressed as triage. Pick the wider read, declare the read in one line so the user can interrupt mid-chain, execute.

The chain is one continuous motion: PLAN → EXECUTE → EMIT → VERIFY → COMPLETE. No stop between phases. No approval gates. No summarizing-as-completion. The next phase fires the moment the current phase's transition is named. A phase that ends without invoking its successor has stalled the chain.

## Install Model

Skills are installed via `bun x skills add AnEntrypoint/gm-skill`. Each skill lands as `~/.agents/skills/<name>/SKILL.md` symlinked into `~/.claude/skills/<name>/`. SKILL.md is the only file that ships per skill. No `bin/`, no `lib/`, no `scripts/`, no `lang/` survives the install — anything those used to provide must route through the spool.

## Self-Bootstrap

First spool dispatch in any project checks for `~/.claude/gm-tools/plugkit.wasm`. Absent → write `.gm/exec-spool/in/bootstrap/0.txt` (empty body) and the orchestrator fetches the binary from `AnEntrypoint/plugkit-bin` releases, sha256-verifies against the pinned manifest, and writes `.gm/exec-spool/.bootstrap-status.json` before any other verb runs. Pin mismatch or fetch failure writes `.bootstrap-error.json` and the chain pauses until resolved. Plugkit launches its own spool watcher on first invocation — no external launcher script.

## Session State

Lives entirely in `cwd/.gm/`. `prd.yml` (current task plan), `mutables.yml` (unresolved unknowns gate writes and git), `exec-spool/in/` and `exec-spool/out/` (request/response triplets), `gm-fired-<sessionId>` (orchestrator gate marker), `rs-learn.db` and `code-search/` (tracked persistent memory and index — never gitignored).

## Spool Dispatch Surface

Every dispatch goes through the spool. Tool args are ephemeral; the spool inverts that — request lives on disk before the watcher reads it, watcher is detached from the agent process, output triplet (`.out`, `.err`, `.json`) is auditable after the fact.

Write to `.gm/exec-spool/in/<lang>/<N>.<ext>` (nodejs, python, bash, typescript, go, rust, c, cpp, java, deno) or `in/<verb>/<N>.txt` (codesearch, recall, memorize, wait, sleep, status, close, browser, runner, type, kill-port, forget, feedback, learn-status, learn-debug, learn-build, discipline, pause, health, bootstrap, instruction, transition, phase-status, prd-add, prd-resolve, prd-list, mutable-add, mutable-resolve, mutable-list, memorize-fire, residual-scan, auto-recall, task-spawn, task-list, task-stop, task-output). Watcher streams `out/<N>.out` and `out/<N>.err` line-by-line, then writes `out/<N>.json` (exitCode, durationMs, timedOut, startedAt, endedAt) at completion.

Only `git` and `gh` run directly via the Bash tool. Inline `node script.js`, `Bash(exec:<anything>)`, JSON-form dispatch — denied at the hook layer.

## Batch Dispatch

The watcher processes verbs sequentially internally, but the agent's bottleneck is round-trip latency, not the watcher. Write N inputs in one message via parallel Write tool calls, then read N outputs in one message via parallel Read calls. A 5-verb batch is one agent turn, not five. Serial round-trips for independent verbs are forced closure dressed as caution. Dependent verbs (transition after instruction, prd-resolve after the work it resolves) serialize at the dependency boundary only, not across independent dispatches.

## Observability — .watcher.log

The watcher writes its own stdout/stderr (plus the wasm cdylib's `println!`/`eprintln!`) to `.gm/exec-spool/.watcher.log`. Read it directly when a dispatch returned an error you don't understand, when a verb seems slow (the log shows `[dispatch] ← verb=X ms=N`), when sweep activity needs explaining, or when boot issues surface (`--- watcher boot ... ---` markers). Use Read with `offset` to tail. Rotated at 10MB to `.watcher.log.1`.

## Session-ID Threading

At entry, generate or detect SESSION_ID (`SESSION_ID` env or fresh uuid). Every rs-exec RPC body and every spool-written task body carries `sessionId: "<id>"`. Task-scoped cleanup (deleteTask, getTask, appendOutput, killSessionTasks) requires matching sessionId. Empty sessionId is hard-rejected — no orphaned tasks.

## Daemonize by Default

Watcher returns task_id immediately and tails the logfile up to 30s wall-clock before returning. Short tasks complete inside the window and look synchronous. Long tasks return the task_id with partial output and continue running. Never re-spawn a long task to check on it — that orphans the first.

Resumption grammar: `tail` drains additional output without blocking. `watch` blocks until regex matches or timeout. `wait` is a pure timer. `sleep` blocks on a specific task's output. `close` terminates. Every RPC response carries `running_task_ids` for the calling session so background work is never lost.

## Hooks Throw, Never Mutate

A hook blocking a tool call throws an error with an imperative instruction string. It does not rewrite the call's arguments into a self-failing form. Throw form is for "use a different tool" (the model adapts policy); mutate form reinforces the wrong mental model.

## Code vs Meaning

Code does mechanics; semantic operations route through `exec_js` calling the agent's tool surface. A keyword-list or regex-on-meaning-phrases loop deciding semantic questions ships a green check that lies.

## Orient Before Naming

Before naming any unknown, dispatch a parallel pack of 3-5 `exec:recall` + 3-5 `exec:codesearch` calls. Hits become weak_prior; misses confirm the unknown is fresh. Orient cost equals skip cost; skipping it duplicates discovery and risks disagree-with-prior-witness.

## Maximize Scope

When the request admits a deeper or broader shape reachable from this session, take that shape. "Is this too much work?" is the agent's question, not the user's; below the destructive-irreversible-and-not-covered bar, larger always beats smaller. Declare the read in one PRD line so the user can interrupt; execute.

## Nothing Fake

Stubs, mocks, placeholder returns, fixture-only paths, demo-mode short-circuits, "TODO: implement" bodies — forbidden in shipped code. Acceptance is real input through real code into real output, witnessed.

## Memorize is the Only Memory

`memorize-fire` is the only sanctioned destination for what-was-learned. The harness may expose an internal-memory surface ("writing memories", model-side memo, scratchpad-as-memory) — reaching for it is forced closure: the write looks like memory but never enters the rs-learn recall index, so the next session cannot witness it. A memo not dispatched through `memorize-fire` is a green check that lies. Multiple facts → multiple parallel `.gm/exec-spool/in/memorize-fire/<N>.txt` writes in one message.

## Ask Last, After Scope and Web

An in-conversation question that forms in the agent's planning is a signal to expand, not to ask. The wider read of the request is the first response. A `WebSearch` / `WebFetch` pack against the unknown is the second. Asking the user fires only when both close empty and the unknown is genuinely destructive-irreversible-and-not-covered. The model's pull toward "let me just check one thing with you" is the exact failure mode this rule guards against — most of those checks are scope-narrowing dressed as deference, and the answer the user would give matches what scope-expansion or one web-search would produce.

Transition: when SESSION_ID is threaded and the spool is reachable, dispatch the `instruction` verb again with phase=PLAN to advance to PLAN.
"#;
