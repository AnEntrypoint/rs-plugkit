pub const TEXT: &str = r#"# PLAN — L1 baseline probe + L2 covering family

Prior memory loads on entry.

## ORIENT — L1 baseline

The first dispatch of any non-trivial turn is a parallel fan-out of `recall` and `codesearch` against the request's nouns. Hits constitute the empirical baseline; misses delimit fresh ground. Operations whose marginal cost is unmeasured against existing context are unevaluated; an agent that proceeds without orient commits to an envelope it has not observed.

`in/recall/{N}.txt` and `in/codesearch/{N}.txt` are the surfaces. Fan-out is parallel writes in a single message; reads in the next.

## Cover — L2 maximal closure

The PRD is the central store for plan-items (`|F|=1`). Its content is the closure of the destructive transform the request admits over this session, enumerated as a dependency DAG. Session is the unit; reach permits the next node; the next node is in-scope.

A plan-item that names a smaller-than-necessary slice while a larger reachable shape exists is non-monotonic — it ships a prefix and externalizes the remainder. Partition only along genuine dependency edges; partition along schedule ("Phase N") is L3 deferral, not planning. In-spirit reachable residuals expand the PRD silently; the read declares itself in one line.

The response body is not a planning surface. Inline TODO lists violate `|F|=1` and produce state the orchestrator cannot reconcile.

## Mutables

Unknowns enter `.gm/mutables.yml` at `status: unknown`. Transition to `witnessed` requires concrete witness (`file:line`, codesearch hit, exec output). Narrative resolution is rejected at the verb. Unwitnessed rows block the transition gate.

## Dispatch

`recall`, `codesearch`, `prd-add`, `mutable-add`, `mutable-resolve`, `transition`.
"#;
