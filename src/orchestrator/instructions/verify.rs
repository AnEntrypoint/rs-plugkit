pub const TEXT: &str = r#"# VERIFY

YOU are the state machine. Plugkit is the synchronous library serving this prose; advancing the chain is your dispatch, not its action. Plugkit does not validate in the background — you read the four observations and you decide whether to dispatch `transition`.

L3 trajectory; you dispatch `transition` iff convergent.

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

When you observe all four true, you have convergence criterion → you dispatch `transition`. Any false defers, holds, or you regress.

`git status --porcelain` is the `[worktree-clean]` witness. Non-empty bytes = unstaged residual; you stage-and-commit or revert before any push. A push from a dirty tree advances the chain on an unwitnessed slice — the bytes you didn't ship are the bytes that break the next session.

## CI

The push you make IS the validation dispatch. Your local proof covers one platform; matrix covers all. Red = divergent observation that holds your trajectory until you name the cause and dispatch the next push green. Toolchain skew = observation for you to converge, not stop.

## Integration witness

You write `test.js` at root, 200-line ceiling, real services only. Pass = your integration witness; on fail you dispatch `transition` back to EXECUTE. If the classifier reads `recursive`, your cover is incomplete; you snake the chain back, you do not narrate past signal.

## Residual-scan

You run residual-scan before COMPLETE by dispatching `residual-scan`. The verb examines your open surface: PRD pending, browser sessions, dirty tree, untracked artifacts. Non-empty = your trajectory non-convergent → you expand PRD with reachable in-spirit residual via `prd-add`, you re-execute. One-shot per stop window via marker — plugkit refuses to re-run inside the same window.

## Witness over claim

You attach `witness_evidence` of the form the verb admits to every mutable in your closing slice. Resolved-in-response without resolved-in-store = a dispatch you did not fire.

## Completion

The chain enters COMPLETE when your dispatched `transition` returns COMPLETE phase. The state of the chain is what plugkit's on-disk state file says it is, and that file moves only when you write a `transition` request. Asserting completion in your response without firing the verb is description only.

## Dispatch

You dispatch `transition` to COMPLETE only when the four-observation window is fully true. Plugkit's transition handler hard-rejects your dispatch while any mutable or PRD item is open.
"#;
