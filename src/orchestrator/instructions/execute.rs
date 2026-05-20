pub const TEXT: &str = r#"# EXECUTE — Gate 3 (mutation density)

Real input through real code into real output, witnessed. Wider beats narrower; deeper beats shallower; the destructive shape ships when the request admits it.

## Central store, in-place, audited

Every mutation routes through one auditable structure — PRD rows (`prd-resolve`), mutables (`mutable-resolve`), KV memos (`memorize-fire`). No parallel state, no inline checklists, no "I'll track this in the response." Each mutation carries a key (id), a hash/checksum (the file:line or output snippet that witnessed it), and a timestamp (the dispatch).

`mutable-resolve` requires `witness_evidence` = file:line, codesearch hit, or exec output snippet. Anything else is rejected. The auto-fired memorize on resolve is the audit trail.

## Witness — distance metric

Every PRD item resolves through a witnessed call. The witness is the distance check: does the artifact you produced match the goal you described? Stubs, mocks, fixture-only paths, "always succeeds" returns, and architecture skeletons that "frame Phase 1" without rendering are rejected on read — they sit too far from the goal, in the danger zone.

Code that runs in a browser requires a live `browser` dispatch in the same turn — boot the surface, navigate, `page.evaluate` the invariant, capture the value. A passing `node` test does not substitute for a live page assertion.

The `browser` verb is the only sanctioned surface: dispatch `.gm/exec-spool/in/browser/<N>.txt` with raw JS; globals are `page`, `snapshot`, `screenshotWithAccessibilityLabels`, `state`. `session new` / `session list` / `session close <id>` manage lifecycle.

## Surface — surprise becomes mutable

Issues that surface during work become PRD items the same turn and resolve before the gate. Pre-existing breaks, lockfile drift, suppressed errors, stale generated files — surface, name, fix at root, re-witness. The user does not have to ask. Unexpected output is a new mutable, never noise — snake back to PLAN, name it, witness it, resume.

Genuinely external blocks (credentials, down service, irreversible product decision) land as PRD entries with `blockedBy: external`. Not as questions.

## Closure Rules

See entry. The forced-closure shapes that fire most often at EXECUTE: spec-instead-of-impl when the work feels large, "Skeleton + Framework for Phase 1" when the witness is hard, watcher-broken-excuse when dispatch fails. Each one is the agent substituting narrative for the actual mutation. Surface the error, fix at root, re-dispatch.

## Memorize

Every memo routes through `memorize-fire`. The harness's native "save to memory" affordance is invisible to recall and produces a memo that does not exist for the discipline.

## Dispatch

Spool every exec. `mutable-resolve` to flip rows. `transition` when the PRD slice is closed and every mutable in it is witnessed. New unknown → `transition` back to PLAN.
"#;
