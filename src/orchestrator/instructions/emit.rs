pub const TEXT: &str = r#"# EMIT — Gate 3 audit on disk

Intent becomes artifact. The whole covering family lands, not a representative subset.

## Read Before Write — distance check

Read the target path first. A diff against an unread file is a diff against a stale model — the candidate mutation is being measured against an imagined baseline, not the real one. Mismatch → snake to PLAN. This is the disk-side distance check: the file as it is now is the goal-relative reference; your write is the mutation; the post-write Read is the audit.

## Fresh Index

Codeinsight and search outputs feed EMIT only from a freshly-completed index. Digest match against the live filesystem or no result. Emitting from an unverified index is bluffing the cost-gate — the agent reads stale output as ground truth and acts on a state that no longer exists.

## Write Then Verify — central store + checksum

One Edit or Write per artifact. After the write, Read the file from disk and assert the change is present. The verified disk state is the witness, not the green tool call. Discrepancy → fix at root, re-emit, re-verify.

Artifacts the PRD names. Closing narrative goes in the commit message and the next `memorize-fire`. Nowhere else.

## Closure Rules

See entry. The shape that fires hardest at EMIT: unsolicited docs. The literal regex of forbidden filenames at project root, `docs/`, or anywhere: `(?i)(SESSION|IMPLEMENTATION|TERRAIN|<COMPONENT>)-(SPEC|SUMMARY|PLAN|ROADMAP|STATUS|NOTES|COMPLETE)\.(md|txt)$` and variants like `START-HERE.md`, `COMPLETED.md`, `SUMMARY.txt`. The commit message + `memorize-fire` are the only sanctioned destinations for closure narrative.

"✅ Work Accomplished / 📋 Key Findings / 📁 Deliverables / Session Complete" prose composed in lieu of writing the artifact is forced closure. Effort estimates as stop-grounds ("Phase 1: 4-6h, Phase 2: 8-12h") are the same shape — the chain is COMPLETE when plugkit says it is, never when the agent narrates it.

## Dispatch

`transition` when every planned artifact is written and disk-verified. New unknown → `transition` back to PLAN.
"#;
