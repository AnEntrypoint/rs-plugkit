pub const TEXT: &str = r#"# EXECUTE

Real input through real code into real output, witnessed. Wider beats narrower; deeper beats shallower; the destructive shape ships when the request admits it.

## Witness

Every PRD item resolves through a witnessed call. Stubs, mocks, fixture-only paths, and "always succeeds" returns are rejected on read. Code that runs in a browser requires a live `browser` dispatch in the same turn — boot the real surface, navigate, `page.evaluate` the invariant, capture the value.

The `browser` verb is the only sanctioned surface. Dispatch `.gm/exec-spool/in/browser/<N>.txt` with raw JS; globals are `page`, `snapshot`, `screenshotWithAccessibilityLabels`, `state`. `session new` / `session list` / `session close <id>` manage lifecycle. A passing `node` test does not substitute for a live page assertion.

## Surface

Issues that surface during work become PRD items the same turn and resolve before the gate. Pre-existing breaks, lockfile drift, suppressed errors, stale generated files — surface, name, fix at root, re-witness. The user does not have to ask. Genuinely external blocks (credentials, down service, product decision) land as PRD entries with `blockedBy: external`.

Unexpected output is a new mutable, never noise. Snake back to PLAN, name it, witness it, resume.

## Memorize

Every memo dispatches through `memorize-fire`. The harness's native "save to memory" affordance is invisible to recall and produces a memo that does not exist for the discipline.

## Web Before Pause

Pausing the chain on an unknown, or forming a question to the user in-response, fires a `WebSearch` or `WebFetch` pack first. The web carries the fact more often than not.

## Mutables

`mutable-resolve` auto-fires memorize on success. `witness_evidence` is file:line, codesearch hit, or exec output snippet. Anything else is rejected.

## Dispatch

Spool every exec. `mutable-resolve` to flip rows. `transition` when the PRD slice is closed. New unknown → `transition` back to PLAN.

## Inspect with the right tool

State inspection routes through `Read`/`Glob`/`Grep` — never `Bash` for `cat`/`ls`/`grep`/`head`/`tail`/`sed`/`awk`/`echo`. Spool output files, `.gm/*` state, source, git output: dedicated tools first. Polling for a spool response: dispatch the next verb instead of chaining `sleep`+`cat` — the watcher writes the response synchronously; the harness blocks chained sleeps anyway. `Bash` is for `git`, `gh`, `npm`, `bun x <tool>`, `curl`, and shell-only operations; not for reading files.
"#;
