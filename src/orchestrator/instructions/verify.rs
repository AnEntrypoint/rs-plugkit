pub const TEXT: &str = r#"# VERIFY — Gate 4 (trajectory + emit-only-if-converging)

COMPLETE is earned. The trajectory classifier reads the chain's last N observations and emits the Bridge only if the system is convergent. The four observations:

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

All four true → state Open → emit transition to COMPLETE. Any false → defer or hold or regress.

## CI Is The Build

Push triggers the matrix. Local cargo proves one platform; CI proves all of them. Watch with `gh run list --branch <branch> --limit 3 --json status,conclusion,name`; on red, root-cause and re-push. Toolchain skew is not a stop condition — it's a divergent observation that holds the trajectory until resolved.

## One Integration Test

`test.js` at project root, 200-line cap, real services only. VERIFY runs it. Failure regresses to EXECUTE — that is the trajectory classifier reading `recursive` and snaking the chain back, not declaring done in spite of the signal.

## Residual Scan — trajectory window

Before transitioning to COMPLETE, dispatch `residual-scan`. The scan reads the last window of the chain (PRD pending count, open browser sessions, dirty tree, untracked docs) and either fires the four-check pass or returns a residual list. Reachable in-spirit residuals expand the PRD and run — the trajectory was reading `recursive` because the cover was incomplete. The `.gm/residual-check-fired` marker makes the scan one-shot per stopping window.

## Mutable Witness, Not Checkmarks

`mutable-resolve` requires `witness_evidence` = file:line, codesearch hit, or exec output snippet. A line of prose ending in `✓` is rejected on add. "Verified all mutables: quadtree streaming ✓, shaders compile ✓" without dispatched resolves is a self-declared completion, not a witness. Each ✓ in a final-message summary is an unfired `mutable-resolve` — go fire it before claiming done.

## Self-Declared Complete Is The Failure

"Session Complete", "Work Accomplished", "Status: deployable" written into the response without `transition` to COMPLETE actually running is the exact close-by-narrative the orchestrator hard-rejects. If the watcher is broken and `transition` will not dispatch, that is the work to fix — surface the bootstrap error, reboot, dispatch. Not a license to declare done in prose.

## Closure Rules

See entry. At VERIFY, the residual must equal the witnessable closure minus the named out-of-spirit residuals. Gap means cover is not yet maximal — re-enter PLAN.

## Dispatch

`transition` to COMPLETE only when all four observations are true. The orchestrator hard-rejects the transition if any mutable or PRD item is open.
"#;
