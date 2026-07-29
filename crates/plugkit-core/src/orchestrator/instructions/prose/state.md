# STATE

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 4 of the pipeline: state and functional core. Every function total, every effect explicit, every invariant algebraic. f composed with f equals f; a replayed dispatch reaches the same result, never a second different mutation. The STATE -> CONC edge carries the compiled `idempotent-dispatch-replay-safe` gate; the rest of this stage is a sweep you run, adversarially, via `exec_js`.

## Total-function sweep

For every new function in the diff: it returns on every input path -- no implicit undefined/None on an unhandled branch, no match/switch arm that falls through to a hole. Feed it the edge inputs live (`exec_js`: zero-length, max-size, null/undefined, wrong type, boundary-adjacent-invalid) and witness a defined result each time, same turn. A signature read is not the witness; the run is.

## Ownership and affine-state sweep

For every resource the diff takes ownership of: dropped exactly once -- no leak, no double-free, no use-after-move in Rust, no un-closed handle/stream/session in JS. For every data structure: state changes are explicit assignment at a named boundary, never a buried side effect or an init hidden in a helper. A "pure" function that secretly writes module state, or a getter that mutates, is a hidden mutation -- restructure so the effect is in the signature, not beside it. Witness by exercising the acquire/use/release cycle under `exec_js` and asserting the final state matches the declared effect.

## Idempotency sweep

Every mutation the diff introduces is safe to replay: same input -> same outcome, never a second different mutation applied on top of the first. Check the dispatch surfaces especially -- a verb re-fired after a crash must converge (content-hash dedup, nothing-to-commit gate, digest gate), not double-apply. Witness by running the operation twice under `exec_js` and diffing the resulting state; a second run that changes anything is a violation.

## Structure sweep

Topology is a strict DAG: value flows downward, no cyclic dependency between modules the diff touches, no hidden back-edge through a shared global. Abstraction is misuse-proof: invalid state is unrepresentable -- parameters over hidden globals, the type/shape encodes the constraint so the bad combination cannot be constructed. Data first: a shape that permits invalid states pays for them forever in guards; fix the model, not the flow. Flat spine over pointer graph; denormalized over nested; bytes over JSON on hot transport paths.

## Failure shape

Fail fast, loud, deterministic: halt on precondition violation with exact state, at the earliest boundary that can still name the cause. No silent degradation -- a plausible-but-wrong value under a violated precondition converts one loud failure into an unbounded number of quiet ones. Every failure path explicit: full -> degraded -> safe-fail -> explicit-error, no catastrophic silent mode.

## Discovery

Any violation found here routes by shape: a code repair -> `transition to=EMIT` (the graph's STATE -> EMIT edge). A discovery that reshapes the plan -- the data model itself is wrong, the spec assumed a shape reality does not have -> `transition to=SPECIFY`, re-scope the affected rows by their existing ids. Narrating either instead of dispatching the transition strands the chain.

## Dispatch

`transition to=CONC` only when every sweep above has a live `exec_js` witness behind it, same turn. A happy-path-only STATE audit has not audited.
