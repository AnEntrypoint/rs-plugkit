# RES

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 7 of the pipeline: resilience and the error model. Every exception handled or explicitly propagated, never a panic; degradation graceful and bounded; the failure boundary named before load finds it. The RES -> DECIDE edge carries the compiled `no-unchecked-panics-in-diff` gate -- a bare unwrap/expect/panic!/unhandled throw outside a test path refuses the transition.

## Exception-model sweep

The gate catches the visible shapes. You sweep for what a line-scanner cannot: a `.unwrap_or` returning a plausible-but-wrong default under a violated precondition (silent degradation, worse than a crash), an error swallowed to keep a path alive (a catch that logs and continues with corrupted state), a fallback that is not a real, named, correct behaviour for that condition. Every raised error lands in exactly one of two places: handled at a boundary that can still name the cause, or propagated with its context intact. Fail fast, loud, deterministic -- halt on precondition violation with exact state.

## Partial-failure sweep

Crash/kill mid-op: what state does the diff leave behind? Multi-step writes are atomic or recoverable -- staging-then-rename, append-only with replay, idempotent re-entry -- never a torn state a restart cannot reconcile. Network/IO cut mid-call: the retry converges (idempotent re-dispatch) instead of double-applying. Witness each by exercising the interruption under `exec_js` -- kill between steps, cut the connection mid-write -- and asserting the system reaches a named, correct end state on recovery.

## Degradation sweep

Service quality is p99-bounded graceful degradation: under overload the system sheds, queues, or degrades to a NAMED reduced behaviour -- never an unbounded tail (an unbudgeted loop over unbounded input, a synchronous burst that starves a scheduler, an allocation that grows with load). Every wait has a timeout, every retry a cap, every queue a bound; the bound is explicit in the code, not implicit in the input distribution that happened to hold during measurement. Optimize the worst case: the average is what a benchmark advertises, the worst case is what a user experiences.

## Crucible sweep

Stress the boundary before production does: max-load, degenerate input, resource exhaustion (unbounded loop/recursion, unclosed handle, memory growth under repeated calls) -- each exercised live via `exec_js`, pass or found-and-fixed, same turn. A boundary you have not driven to failure is a boundary you are guessing at.

## Discovery

A panic-elimination repair routes `transition to=EMIT` (the graph's RES -> EMIT feedback edge -- failure-boundary findings backprop into the source). A reshaping discovery routes `transition to=SPECIFY`. Dispatch, never narrate.

## Dispatch

`transition to=DECIDE` only when every sweep above has a live witness behind it, same turn.
