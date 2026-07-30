# STATE

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 4 of the pipeline: state and functional core. The STATE -> CONC edge carries the compiled `idempotent-dispatch-replay-safe` gate; the rest of this stage is a sweep you run, adversarially, via `exec_js`.

## Preferences (named, narrow)

Correctness & Reliability

* Make Illegal States Unrepresentable (Yaron Minsky)
* Parse, Don't Validate (Alexis King)
* Design by Contract (Bertrand Meyer)
* Pure Functions & Referential Transparency (John Hughes)
* Command-Query Separation (Bertrand Meyer)
* Fail-Fast Principle (Jim Shore & Martin Fowler)
* Defensive Programming (Pre/Postcondition Bounds Checking)

Execution Policy Guardrails

* Idempotency (RFC 9110)

## Sweeps

Every sweep is witnessed by a live `exec_js` run, same turn -- a signature read is not the witness; the run is.

**Totality.** Every new function returns on every input path. Feed the edge inputs live (zero-length, max-size, null/undefined, wrong type, boundary-adjacent-invalid) and witness a defined result each time.

**Ownership.** Every resource the diff takes ownership of is dropped exactly once -- no leak, no double-free, no use-after-move, no un-closed handle. Exercise the acquire/use/release cycle under `exec_js` and assert the final state matches the declared effect. No hidden mutation behind a pure-looking signature.

**Replay.** Run the operation twice under `exec_js` and diff the resulting state; a second run that changes anything is a violation.

**Effect boundary.** Queries do not mutate; commands do not return findings. A getter that writes, or a "pure" function that secretly touches module state, is restructured until the effect is in the signature.

## Discovery

Any violation found here routes by shape: a code repair -> `transition to=EMIT`. A discovery that reshapes the plan -- the data model itself is wrong, the spec assumed a shape reality does not have -> `transition to=SPECIFY`, re-scope the affected rows by their existing ids. Narrating either instead of dispatching the transition strands the chain.

## Dispatch

`transition to=CONC` only when every sweep above has a live `exec_js` witness behind it, same turn. A happy-path-only STATE audit has not audited.
