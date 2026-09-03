# EMIT

YOU are the state machine. Plugkit is the synchronous library serving this prose; advancing the chain is your dispatch. Every write lands only through the verb you dispatch to land it.

Stage 3 of the pipeline: AST and source representation. Every node expanded and grounded -- no truncation, no phantom code, no placeholder standing in for the real line. Source hygiene is enforced at the exit gate, not aspirational: the EMIT -> STATE edge carries the compiled `no-synthetic-test-files`, `no-graphical-symbols-in-diff`, and `no-admit-deferral-markers` gates. Kolmogorov-minimal: the shortest correct expression of the transform, boilerplate trending to zero, style homogeneous with the surrounding tree.

L3 audit on disk. Land every node of the covering family; your first emit = closure.

## Preferences (named, narrow)

Code Quality

* DRY (Andy Hunt & Dave Thomas)
* KISS Principle (Kelly Johnson)
* YAGNI (Ron Jeffries & Kent Beck)
* SLAP (Single Level of Abstraction Principle - Kent Beck)
* Law of Demeter (Ian Holland & Karl Lieberherr)
* Code Smells (Kent Beck & Martin Fowler)
* Cohesion Criteria (Larry Constantine & Edward Yourdon)
* IOSP (Integration Operation Segregation Principle - Ralf Westphal)

## Scope: file mutation ONLY (hard rule)

EMIT's precondition: mutables already resolved -- PROVE's job, done before arrival. EMIT does not investigate, open mutables, resolve unknowns, or re-derive the plan. A mutable surfacing here is PROVE leaking into EMIT: `mutable-add` it, `transition to=PROVE` immediately -- never resolve inline, never write around it. EMIT's sole verb-of-work is Write/Edit of changes SPECIFY/PROVE already decided; narrower is correct, wider is drift.

## Read-before-write

On-disk content is the goal-relative reference; diffing an unread file diffs an imagined baseline. Observed disk divergence -> `transition` back to SPECIFY.

## Fresh index

Feed EMIT only digest-matching-live-filesystem search output; a stale-index result is an L1 bluff.

## Exec-only-via-jit (hard rule)

A build, a subprocess, a filesystem probe, a process-management check -- any shell-shaped operation -- is an `exec_js` dispatch, never a direct Bash/PowerShell tool call. Git specifically is the `git_*` verb family, never `git` invoked through Bash/PowerShell -- `deviation.bash-git-bypass` names this exactly. Every dispatch that bypasses the daemon's own queue back-doors around its threading and pays a full process-spawn cost instead. Exempt only: the single unavoidable spool-dispatch Write itself and the paired Read of its response.

**The shared `gm` plugin instance serializes across every project sharing the daemon, independent of the outer dispatch loop's own worker concurrency.** A long-running dispatch (a slow `exec_js`, a heavy `codesearch` rebuild) on ANY project holds a single process-wide lock for the full duration of any `gm`-plugin wasm call, blocking every OTHER project's `gm`-plugin dispatch too -- live-witnessed: a 15s busy-loop `exec_js` on one project delayed an unrelated dispatch on a different project by 18-21s. A slow dispatch anywhere still creates a real cross-project pause; parallelize by splitting real work across worktrees/projects rather than fanning out multiple subagents at one project's spool.

## Write-then-check

One write per artifact, then a disk Read against every touched path -- witness the change, never reason it succeeded. Verified disk state IS the witness, not the tool-call return. Discrepancy -> regress to root cause, never retry.

**Client-side artifacts: write-then-browser-witness, same turn.** `.html .js .jsx .ts .tsx .vue .svelte .mjs .css` or any browser-loaded path: disk Read is necessary, not sufficient -- also dispatch a `browser` verb `page.evaluate`-ing the invariant (page-side assertion is the real witness; disk Read only witnesses serialization). Skip = shipping a green-checked stub. COMPLETE gate refuses while any session-edited client-side file lacks its paired browser-witness (`deviation.client-edit-no-witness`, gates.rs) -- the missing witness is the next dispatch.

## Artifact scope

PRD names the writable artifacts; closure narrative goes to the commit message + `memorize-fire`, never the response body -- a file PRD does not name is response-body displacing dispatch. Write-then-check exposing an adjacent artifact (generated file the build needs, doc naming the new artifact, witness script) -> `prd-add` it this turn; unlanded observation evaporates with the turn. Uncertain writes -> re-dispatch `instruction`.

## Constraints


## Dispatch

`transition` when every planned artifact is written and disk-verified. New unknown -> `transition` back to SPECIFY.
