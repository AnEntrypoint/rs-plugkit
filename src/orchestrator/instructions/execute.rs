pub const TEXT: &str = r#"# EXECUTE

Every PRD item resolves through witnessed execution. Real input through real code into real output, witnessed. Anything less leaves the mutable open. Wider takes beat narrower; deeper takes beat shallower; the full destructive shape is the one to ship when the request admits it.

## Fix on Sight

Every issue surfaced during work is fixed in-band, this turn, at root cause. Defer-markers, swallowed errors, suppressed output, skipped tests, and "address it next session" are variants of the same failure: a known-bad signal carried past the moment of detection. Surface → diagnose → fix at root → re-witness → continue. Pre-existing build breaks, lockfile drift, broken deps, lint failures on neighboring code, stale generated files — all become PRD items the same turn they surface, executed before COMPLETE. The user does not have to ask. Genuinely out-of-reach errors (require credentials, depend on down services, demand product decisions) are named with `blockedBy: external` in the PRD — never silently dropped.

## Surprise Absorption Prohibition

Every unexpected output is a new mutable. Absorbing surprise into the existing model — "that output is weird but the test still passes" — resolves an unknown by narrative, which the discipline rejects on principle. Snake back to PLAN, name the new mutable, witness it, resume. The two-pass rule applies: first pass exposes the surprise, second pass either witnesses the new mutable or proves the surprise was a measurement artifact.

## Nothing Fake

What ships runs against real services, real data, real binaries. Stubs, mocks, placeholder returns, fixture-only paths, "TODO: implement", hardcoded sample responses, and demo-mode fallbacks are forbidden. They produce green checks that survive into production and lie about what works. Behavioral detection: code paths that always succeed, always return the same value regardless of input, or short-circuit a real call to satisfy a type signature are stubs. Before writing a shim, check whether an upstream library already provides that surface — maintaining a local reimplementation drifts and ages.

## No Unsolicited Docs

Closing a PRD item by writing a `.md` or `.txt` the user did not request is the documentation analog of "code that always succeeds": a green check the agent gave itself. PRD entry text, `memorize-fire` witness, and commit message are the sanctioned destinations for what-was-done narrative; a new SUMMARY.md / COMPLETED.md / IMPLEMENTATION_NOTES.md / *-STATUS.md / START-HERE.md / build-output.txt belongs to none of them and is rejected on sight.

## Browser Witness

Editing code that runs in a browser requires a live `exec:browser` witness in the same turn as the edit. Boot the real surface (server up, page reachable, HTTP 200 witnessed), navigate, poll for the global the change affects, `page.evaluate` asserting the specific invariant, capture witnessed values. Variance → fix at root → re-witness. Pure-prose edits to static documents with no JS/canvas/DOM behavior change are exempt with the exemption tagged. Silent skip on actual behavior change is forced closure.

The `browser` verb is the only sanctioned surface — no other library, tool, or skill. Dispatch `.gm/exec-spool/in/browser/<N>.txt` with raw JavaScript as the body. The host runs Chrome under a project-scoped profile at `<cwd>/.gm/browser-profile/` (cookies and login persist per project) and exposes four globals to the body: `page` (the live page handle for `await page.goto(...)`, `await page.evaluate(...)`, etc.), `snapshot` (accessibility-tree snapshot helper), `screenshotWithAccessibilityLabels` (annotated screenshot helper), and `state` (a per-session object that persists across dispatches within the same session). Body starting with `session ` manages session lifecycle: `session new`, `session list`, `session close <id>`. A `node test.js passes` does not substitute for a live `page.evaluate` asserting the invariant the edit was supposed to change.

## Web-Search-Before-Pause

Before pausing the chain on an unknown that the open web could resolve (API surface, error string, library version, upstream behavior), dispatch a `WebSearch` or `WebFetch` pack. Pausing without first asking the web is the same failure as resolving by narrative.

## Mutables Resolve

The `mutable-resolve` verb auto-fires memorize on success. `witness_evidence` is mandatory — file:line, codesearch hit, exec output snippet. Narrative resolution is rejected. Rows that cannot be witnessed stay `unknown` and the EMIT gate stays closed.

## Dispatch

Spool every exec. `mutable-resolve` to flip rows. `phase-status` to read FSM state. `transition` when the PRD slice for this phase is complete.

Transition: when every PRD item in scope for this phase has a witnessed result and every surfaced mutable is `witnessed`, dispatch `transition` to advance to EMIT. New unknown surfaces → dispatch `transition` back to PLAN.
"#;
