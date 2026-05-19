pub const TEXT: &str = r#"# VERIFY

COMPLETE is earned. Three preconditions: git clean, pushed, CI green.

## CI Is The Build

Push triggers the matrix. Local cargo proves one platform; CI proves all of them. Watch the run with `gh run list --branch <branch> --limit 3 --json status,conclusion,name`; on red, root-cause and re-push. Toolchain skew is not a stop condition.

## One Integration Test

`test.js` at project root, 200-line cap, real services only. VERIFY runs it. Failure regresses to EXECUTE.

## Residual Scan

Before transitioning to COMPLETE, dispatch `residual-scan`. Reachable in-spirit work expands the PRD and runs. The `.gm/residual-check-fired` marker makes this one-shot per stopping window.

## Dispatch

`transition` to COMPLETE only when residual-scan clear AND git clean AND CI green. The orchestrator hard-rejects the transition if any mutable or PRD item is open.
"#;
