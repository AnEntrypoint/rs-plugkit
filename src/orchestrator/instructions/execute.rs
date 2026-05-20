pub const TEXT: &str = r#"# EXECUTE — L3 distance + audit

Real input → real code → real output, witnessed. Wider beats narrower; deeper beats shallower; the destructive shape ships when the request admits it.

## Single-writer mutation surface

Every mutation routes through one auditable structure: PRD rows (`prd-resolve`), mutables (`mutable-resolve`), KV memos (`memorize-fire`). No parallel state, no inline response checklists, no "I'll track this in chat." Audit tuple per accepted write: `(id, hash, ts)` where `hash` is the file:line or output snippet that witnessed it.

`mutable-resolve` requires `witness_evidence` ∈ {file:line, codesearch hit, exec snippet}. Else rejected. Resolve auto-fires memorize; the witness persists into the recall index.

## Witness as Lyapunov check

Each PRD item resolves through a witnessed call. The witness IS the distance measurement: does the produced artifact reduce `d(state, goal)`? Stubs, mocks, fixture-only paths, "always succeeds" returns, and "skeleton frameworks for Phase N" all sit at high distance — L3 rejects, *not* in-progress work.

Browser-running code requires a live `browser` dispatch in the same turn: boot the surface, navigate, `page.evaluate` the invariant, capture the return. A passing `node` test is not a witness for browser code.

`browser` verb is the sole sanctioned surface. `in/browser/<N>.txt` carries raw JS; globals `page`, `snapshot`, `screenshotWithAccessibilityLabels`, `state`. `session new` / `session list` / `session close <id>` manage lifecycle.

## Surface — surprise → mutable

Issues surfaced during work become PRD items the same turn and resolve before the gate. Pre-existing breaks, lockfile drift, suppressed errors, stale generated files — surface, name, fix at root, re-witness. Unexpected output is a new mutable, never noise. Snake back to PLAN, name, witness, resume.

Genuine external blocks → PRD `blockedBy: external`. Not questions.

## Maturity-first invariant

Ship the closure of the transform, not the prefix. "I'll do Phase 1 now, Phase 2 next session" externalizes completion cost — non-monotonic, L3-rejected. If the closure exceeds session reach, that's a Maximal Cover decomposition (PRD-DAG enumeration), not a scaffold + IOU. The first artifact emitted IS the mature artifact.

## Closure Anti-Shapes

See entry. At EXECUTE: spec-instead-of-impl when work feels large; "Architecture Skeleton" when witness is hard; watcher-broken-excuse when dispatch fails. Each substitutes narrative for the audited mutation. Surface, fix at root, re-dispatch.

## Memorize

Every memo through `memorize-fire`. Native memory surfaces are invisible to recall.

## Dispatch

Spool every exec. `mutable-resolve` flips rows. `transition` when the PRD slice is closed and every mutable in it is witnessed. New unknown → `transition` back to PLAN.
"#;
