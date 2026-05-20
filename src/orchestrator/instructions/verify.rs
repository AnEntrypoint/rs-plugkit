pub const TEXT: &str = r#"# VERIFY — L3 trajectory; emit transition iff convergent

COMPLETE is earned. The four-observation window:

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

All-four-true is the convergence criterion; state Open → emit transition. Any false defers, holds, or regresses.

## CI is the build

Local proof covers one platform; the build matrix covers all. The push IS the validation dispatch. A red run is a divergent observation that holds the trajectory at VERIFY until the cause is named and the next push lands green. Toolchain skew is an observation to converge, not a stop condition.

## Integration witness

`test.js` at project root, 200-line ceiling, real services only. Its passage is the integration witness; its failure regresses to EXECUTE. The classifier reading `recursive` means the cover was incomplete; the chain snakes back, it does not narrate done past the signal.

## Residual-scan as the gate

Before COMPLETE, `residual-scan` examines the chain's open surface (PRD pending count, browser sessions, dirty tree, untracked artifacts). A non-empty result is the trajectory window reading non-convergent — the agent's recourse is to expand the PRD with the reachable in-spirit residual and re-execute, not to declare done. The marker is one-shot per stop window; the scan is the gate, not a formality.

## Witness over claim

Every mutable in the closing PRD slice must carry `witness_evidence` of the concrete form the verb admits (`file:line`, codesearch hit, exec snippet). The orchestrator hard-rejects narrative resolution at the dispatch boundary; a row that reads as resolved in the response but is not resolved in the store is the unfired dispatch the agent must complete.

## Completion is a verb output, not a response composition

The chain enters COMPLETE when `transition` returns the COMPLETE phase. The state of the chain is what the orchestrator says it is. A response that asserts completion without the verb's emission has only described an outcome — the closure trajectory is in the dispatched state, not the prose.

## Dispatch

`transition` to COMPLETE only when the four-observation window is fully true. The orchestrator hard-rejects the transition while any mutable or PRD item is open.
"#;
