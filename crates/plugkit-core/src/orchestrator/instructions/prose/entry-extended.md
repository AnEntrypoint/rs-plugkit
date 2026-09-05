# ORCHESTRATOR (extended reference)

On-demand tail of `entry` prose -- install/bootstrap/observability/disciplines/
memorize/constraints detail rarely needed mid-chain. Dispatch `instruction`
with `{"prompt":"entry-extended"}` or read this prose key directly when one of
these surfaces is actually in play.

## Code Invariants (every possible emission)

The named-principle canon lives distributed across the stage prose files (Correctness & Reliability + Idempotency at STATE, Performance at CONC, Architecture + Workflow + XY at SPECIFY, Code Quality at EMIT, Security at SEC, Definition of Done at DECIDE, Chain-of-Thought at PROVE); those names are the wide preferences with narrow selection text, and they govern every emission. What remains here is the gm-specific operational residue the canon does not cover:

- **Naming by scale:** <50 lines single-letter algebraic; 50-200 short descriptors; >200 full names; public APIs explicit.
- **Binary transport, append-only persistence:** varint fields; lexical cursors for sparse reads; append-only sequence for replay; chunked by lexical range, modify only the touched chunk.
- **Single focused task per session:** no drive-by refactors; pre-compute and inline.
- **Async boundary explicit:** sequential awaitable primitives; no implicit callback ordering; unified error channel, never swallow rejections.

## Token Discipline

English describing intent = liability when code encodes it; comments = liability when names+structure encode the same; duplication-that-must-sync = liability. Same economy for reasoning: a runnable thought held as silent prose = liability -- reason by executing, not narrating; hypothesis becomes dispatch, output is conclusion. Prose enacts the discipline structurally, never narrates scenarios. Closure anti-shape: a claim composed in prose displacing a dispatch (unrun thought standing in for witnessed one). Response body is not a mutation surface.

## Install

`bun x skills add AnEntrypoint/gm` (or the `install.sh`/`install.ps1` canonical installers) copies the skill directory into `~/.claude/skills/gm/` (and `~/.agents/skills/gm/`), installed as `/gm`. No npx, no marketplace, no npm registry.

## Bootstrap

First dispatch checks `~/.gm-tools/plugkit.wasm` (or `~/.claude/gm-tools/plugkit.wasm` on legacy installs). Absent -> write `.gm/exec-spool/in/bootstrap/<session_id>-0.txt`; plugkit fetches, sha-verifies, writes `.bootstrap-status.json`. On pin mismatch it writes `.bootstrap-error.json` and you pause the chain.

## Supervisor drift and version updates

A supervisor respawns the watcher under fresh code on `wrapper.drift`/`version.drift` or a stale `.status.json`. A dispatch landing in that window returns `wasm_aborted: true` -- retry the same dispatch. `update.available` means newer on-disk fixes -- continue, the supervisor picks them up.

## Observability

`.gm/exec-spool/.watcher.log` -- cdylib stdout/stderr, dispatch timings, sweep ticks, boot markers; tail via Read+offset; rotated 10MB.

## Daemonize

The watcher returns task_id immediately and tails to 30s wall-clock. Short finalizes in-window; long returns partial + continues -- read the partial and decide `tail`/`watch`/`wait`/`sleep`/`close`. Responses carry `running_task_ids` you track.

## Disciplines

Route KV writes to `<cwd>/.gm/disciplines/<ns>/`. `@<name>` prefix sets namespace=name; cross-project read passes `projectPath: <abs>`.

## Memorize

Write the recall index only via `memorize-fire`; surfaces outside it produce memos the index never sees. Prune bad memory on sight: a stale/superseded/wrong recall hit poisons every future recall, so `memorize-prune {key}` removes it (text + embedding); pruning bad memory matters more than preserving good. For an uncertain set, `memorize-prune {query}` returns review-only candidates to judge before removing by `{keys}` -- never a blind similarity-removal.

## Constraints

**Specification precedes implementation (pro-rata).** Treat every emission as if it were being checked by a sound, total, strongly-normalizing, predicative, parametric proof assistant with a verified TCB, and scale the rigour to what the surface actually bears: specify first as dependent types would state it -- pre/post-conditions, invariants, security labels, resource bounds, versioning -- validated once, then implement as a constructive inhabitant of that spec. Total functions, h-set data, closed proofs (cross-checked for critical claims), DAG value flow, confluent evaluation. At the boundary: versioned opaque invariant-enforcing types rather than raw primitives, one designated effect type, a total parser returning `Accepted A | Rejected R` and never an exception, observational equivalence, info-flow-labelled logs, constant-time handling for secrets. Concurrency via substructural types; distributed protocols verified; toolchain-to-execution verified or kernel-direct. The point is not to reach for a proof assistant on every row -- it is that synthesis IS correctness: a spec stated this way makes the implementation the only remaining degree of freedom, which is why the spec is written first and validated once rather than reverse-engineered from working code.

**Data first, then the code that moves it.** Choose the representation before the algorithm -- the layout of the state is the design, and code is what falls out of it. A shape that makes an invalid state unrepresentable removes the validation, the branch, and the class of bug at once; a shape that permits invalid states pays for them forever in guards that must each be remembered. Prefer the flat spine (arrays, indices, contiguous fields) over the pointer graph, and make the common access pattern the one the layout is optimized for.

**Optimize the worst case, not the average.** The average case is what a benchmark advertises; the worst case is what a user experiences and what an operator is paged for. A path with an unbounded tail (an unbudgeted loop over unbounded input, a synchronous burst that starves a scheduler, an allocation that grows with load) is a defect even when its measured mean is excellent -- bound it by time or by size, and make the bound explicit in the code rather than implicit in the input distribution that happened to hold during measurement.

**Fail fast, at the earliest boundary that can still name the cause.** Validate at entry, where the offending input is still in scope and the error message can be specific; a check moved downstream reports a symptom whose cause has already been lost. Silent degradation is worse than a crash: a component that returns a plausible-but-wrong value under a violated precondition converts one loud failure into an unbounded number of quiet ones. Never swallow an error to keep a path alive -- a fallback is admissible only when it is a real, named, correct behaviour for that condition, never as a way to avoid handling it.

**Names and structure carry meaning; comments do not.** A comment that says what the line does is duplication that must be kept in sync and will not be. When the urge to write one arrives, rename, extract, or restructure instead -- a name, a function boundary, or a small type IS the explanation, and a comment beside one is a second, driftable copy. This includes the paragraph-long rationale comment: explaining a WHY inline is the same violation at greater volume, not an exemption from it, and that explaining urge is the signal a name is doing too little.

Rationale genuinely worth keeping -- the constraint being honoured, the failure mode prevented, the measurement that motivated a non-obvious shape -- goes in the commit message, `AGENTS.md`, or the recall store, where it is durable and searchable, never beside the line it describes. EXECUTE states the enforcement form of this rule and VERIFY blocks a transition on any comment in the diff; this is the same rule, not a softer one.

**No standing test files, ever.** Verification is running the real code path and reading its real output through `exec_js`/`browser`, not a suite asserting against mocks. Never create `*.test.*`/`*.spec.*` files, `test/`/`__tests__/` directories, or pull in jest/mocha/vitest/pytest/unittest or any assertion/mocking framework. A mock standing in for real code is the same false-completion class as a hedged `prd-resolve`: it reports a pass that the real path never produced.
