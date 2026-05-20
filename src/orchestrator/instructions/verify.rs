pub const TEXT: &str = r#"# VERIFY — L3 trajectory; emit transition iff convergent

COMPLETE is earned. Trajectory classifier reads the chain's 4-observation window:

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

All-four-true is the convergence criterion — state Open → emit transition to COMPLETE. Any false → defer, hold, or regress.

## CI is the build

Push triggers the matrix. Local cargo proves one platform; CI proves all. Watch with `gh run list --branch <branch> --limit 3 --json status,conclusion,name`; on red, root-cause and re-push. Toolchain skew is a divergent observation that holds trajectory, not a stop condition.

## One integration test

`test.js` at root, 200-line cap, real services only. VERIFY runs it. Failure regresses to EXECUTE — classifier reads `recursive`, chain snakes back. Declaring done past `recursive` violates L3.

## Residual scan — trajectory window

Before transitioning to COMPLETE, dispatch `residual-scan`. Reads PRD pending count, open browser sessions, dirty tree, untracked docs; either fires the four-check pass or returns a residual list. Reachable in-spirit residuals expand the PRD and run — `recursive` reading meant cover was incomplete. `.gm/residual-check-fired` marker enforces one-shot per stop window.

## Mutable witness — not checkmarks

`mutable-resolve` requires `witness_evidence` ∈ {file:line, codesearch hit, exec snippet}. Prose ending in `✓` is rejected on add. "Verified all mutables ✓ ✓ ✓" without dispatched resolves is self-declared completion, never a witness. Each ✓ in a final-message summary IS an unfired `mutable-resolve` — fire it before claiming done.

## Self-declared complete is the failure

"Session Complete", "Work Accomplished", "Status: deployable" written into the response without `transition` to COMPLETE having dispatched IS the close-by-narrative the orchestrator hard-rejects. Broken watcher → fix the watcher; not a license to narrate done.

## Closure Anti-Shapes

See entry. At VERIFY: committed work + named out-of-spirit residuals must equal the witnessable closure. Gap = cover not maximal → re-enter PLAN.

## Dispatch

`transition` to COMPLETE only when all four observations are true. Orchestrator hard-rejects transition if any mutable or PRD item is open.
"#;
