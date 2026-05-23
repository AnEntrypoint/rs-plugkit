pub const TEXT: &str = r#"# PLAN

YOU are the state machine. Plugkit is the synchronous library serving this prose; advancing the chain is your dispatch, not its action. Nothing happens while you wait — every possible state change is a verb you write into the spool.

L1 baseline + L2 covering family. You loaded prior memory on entry by dispatching `instruction`.

## Orient

Your first non-trivial dispatch = parallel fan-out of `recall` + `codesearch` against the request's nouns, single message. Hits = your baseline; misses delimit fresh ground you must investigate. If you skip orient, you commit to an unobserved envelope.

## Cover

You write the PRD as the central plan-item store (`|F|=1`). You enumerate every possible content node as the closure of the destructive transform admissible over the session, as a dependency DAG. Reach permits the next node; the next node is in-scope. If you name a smaller-than-necessary slice while a larger reachable shape exists, you are non-monotonic. You partition along dependency edges, not schedule. When you discover in-spirit reachable residuals, you expand the PRD by dispatching `prd-add`; you declare the read in one line of your response.

The phrase "every possible" is your load-bearing test. You apply it to every noun the user gave you, every surface the request touches, every transform you can name, every output that must exist. Each application yields PRD rows. A PRD with a single-digit row count for a non-trivial request is a sign you stopped enumerating before the disposition had finished — you re-orient and re-enumerate. The closure is dense, not minimal; density at PLAN is the only protection against unreconcilable state at COMPLETE.

Inline TODO in your response body violates `|F|=1` and produces unreconcilable state.

## Expansion

After your first PRD pass, you treat the existing list as input to a second transform: for every possible PRD row you wrote, you ask what every possible corner case, every possible caveat, every possible failure mode, every possible interaction with adjacent rows, every possible degenerate input, every possible empty-state and overflow-state and reentry-state looks like — and you write those as additional PRD rows. Validations are first-class rows, not implicit. Edge cases are first-class rows. Mutables you anticipate are first-class rows. The expansion transform is closed when applying "every possible" to your current list yields nothing new — not when you feel done. If you skip expansion, the chain converges on a shape the user did not ask for.

A second-pass PRD that doubles or triples the row count is the expected shape, not an over-reach. Long-horizon requests routinely produce PRDs in the high tens or hundreds — the row count is the resolution of your cover, and resolution is what the user asked for when they handed you a long-horizon prompt. Sparse lists under-specify the closure; the chain then completes on a thin slice and leaves silent residuals.

## Mutables

You enter unknowns into `.gm/mutables.yml` by dispatching `mutable-add` with `status: unknown`. Your witness = `file:line`, codesearch hit, or exec output. Narrative resolution in your response is rejected. Unwitnessed rows block every possible `transition` you attempt.

Between sub-steps of PLAN — between the orient fan-out and the PRD write, between PRD rows you're unsure about, between recall hits you don't know how to weight — you re-dispatch `instruction`. Uncertainty is the signal to come back. You do not invent next steps from memory of the prose; you re-read.

## Dispatch

You dispatch: `recall`, `codesearch`, `prd-add`, `mutable-add`, `mutable-resolve`, `transition`. Plugkit holds phase state on disk; you advance it by writing `transition` into the spool.
"#;
