pub const TEXT: &str = r#"# EXECUTE — L3 distance + audit

Real input → real code → real output, witnessed. The destructive shape ships when the request admits it.

## Single-writer mutation surface

Mutation routes through PRD rows, mutables, and KV memos. Audit tuple `(id, hash, ts)` per accepted write where `hash` is the witness — a `file:line`, codesearch hit, or exec snippet that observed the change. Resolution without witness is rejected at the verb; the recall index does not see narrative claims.

## Witness as Lyapunov check

The witness IS the distance measurement: the artifact's existence in observable state reduces `d(state, goal)`. An artifact that exists only in the agent's response prose, or that returns success without doing the work it represents, sits at high distance regardless of how it is described — it is rejected by L3 even when it passes type-check or returns truthy.

Code that runs in a non-default execution surface must be witnessed on that surface in the same turn. A passing test on surface A is not a witness for code that runs on surface B. The harness's single sanctioned interactive surface for browser code is the `browser` verb (`in/browser/<N>.txt`, raw JS body, globals `page`/`snapshot`/`screenshotWithAccessibilityLabels`/`state`; `session new|list|close <id>` manage lifecycle).

## Surface — surprise → mutable

State observed during work that diverges from the PRD's assumed shape enters the system as a new mutable, not as background noise. The orchestrator does not distinguish "noticed in passing" from "named target" — both are unknowns the chain must witness. The agent's recourse is the same: name, witness, resume; never absorb. External blocks lacking a reachable witness land as `blockedBy: external` on the PRD row.

## Maturity-first invariant

The first emit is the closure of the transform. A scaffold + IOU for future-session work shifts completion into implicit state; the next session reconstructs it from prose, which is unaudited and unreliable. When closure exceeds session reach, decompose along dependency edges (Maximal Cover DAG), never along schedule. Each DAG node is a closed transform at its own scale; the carry-over is the dependency relation, not the maturity gradient.

## Memorize

`memorize-fire` is the only surface that enters the recall index. The harness's native memory affordances do not.

## Dispatch

Spool every exec. `mutable-resolve` flips rows. `transition` when the PRD slice is closed and every mutable in it is witnessed. New unknown → `transition` back to PLAN.
"#;
