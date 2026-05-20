pub const TEXT: &str = r#"# VERIFY

COMPLETE is earned. Three preconditions: git clean, pushed, CI green.

## CI Is The Build

Push triggers the matrix. Local cargo proves one platform; CI proves all of them. Watch the run with `gh run list --branch <branch> --limit 3 --json status,conclusion,name`; on red, root-cause and re-push. Toolchain skew is not a stop condition.

## One Integration Test

`test.js` at project root, 200-line cap, real services only. VERIFY runs it. Failure regresses to EXECUTE.

## Residual Scan

Before transitioning to COMPLETE, dispatch `residual-scan`. Reachable in-spirit work expands the PRD and runs. The `.gm/residual-check-fired` marker makes this one-shot per stopping window.

## Mutable Witness, Not Checkmarks

`mutable-resolve` requires `witness_evidence` = file:line, codesearch hit, or exec output snippet. A line of prose ending in `✓` is rejected on add. "Verified all mutables: quadtree streaming ✓, shaders compile ✓" without dispatched resolves is a self-declared completion, not a witness. Same for "Tested via browser ✓" without a `browser` dispatch in the same turn. Each ✓ in a final-message summary is an unfired `mutable-resolve` — go fire it before claiming done.

## Self-Declared Complete Is The Failure

"Session Complete", "Work Accomplished", "Status: Fully functional, stable, deployable" written into the response without `transition` to COMPLETE having actually run is the exact close-by-narrative the orchestrator hard-rejects. The chain is COMPLETE when plugkit says it is, not when the agent does. If the watcher is broken and `transition` will not dispatch, that is the work to fix — not a license to declare done in prose.

## Dispatch

`transition` to COMPLETE only when residual-scan clear AND git clean AND CI green. The orchestrator hard-rejects the transition if any mutable or PRD item is open.
"#;
