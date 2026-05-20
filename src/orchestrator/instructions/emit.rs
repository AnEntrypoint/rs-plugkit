pub const TEXT: &str = r#"# EMIT — L3 audit on disk

Intent → artifact. The whole covering family lands. The first emit is the closure; partial emits are non-monotonic.

## Read-before-write — distance check on disk

The target file's on-disk content IS the goal-relative reference. A diff against an unread file diffs against an imagined baseline — the candidate mutation is unmeasured. Mismatch between assumed and actual disk state regresses to PLAN; the mutable that surfaces is the divergence itself.

## Fresh index

Search and structural outputs feed EMIT only when their digest matches the live filesystem. A stale index is a baseline observed against a state that no longer exists; admitting its output is L1 bluff. The fresh-index requirement holds even when the staleness is small.

## Write-then-verify — central store + checksum

One write per artifact, followed by a disk Read that asserts the change. Verified disk state IS the witness; the tool call's return code is not. Discrepancy regresses to root cause, not to retry.

## Artifact scope

The PRD names the artifacts the chain emits. The set of legitimate destinations for closure narrative is the commit message and `memorize-fire`. Any file created on disk that the PRD does not name is unsanctioned — its existence indicates the response body has displaced the dispatch surface. The principle subsumes any specific filename, extension, or location; the agent does not consult an exclusion list because the inclusion criterion (in-PRD) is the discipline.

## Dispatch

`transition` when every planned artifact is written and disk-verified. New unknown → `transition` back to PLAN.
"#;
