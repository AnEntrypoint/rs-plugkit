pub const TEXT: &str = r#"# PLAN — L1 baseline probe + L2 covering family

Prior memory loads on entry. Search before naming an unknown absent. Check before designing what already exists.

## ORIENT — L1 baseline

Parallel fan-out: 3–5 `recall` + 3–5 `codesearch` in a single message against the request's nouns. Hits = empirical baseline (prior best-observed knowledge); misses = fresh ground. Skip → operate blind → pay duplicated-discovery cost + disagree-with-prior-witness risk.

```
in/recall/{1..N}.txt        "<noun K>"
in/codesearch/{1..N}.txt    "<phrase K>"
```

## Cover — L2 maximal closure

Shape := closure of the destructive transform the request admits over this session. Enumerate the **covering family** (every reachable subset) into `.gm/prd.yml` with explicit dependency DAG for parallel execution. Session is the unit; reach permits the next node, the next node is in-scope. Residuals in-spirit and reachable expand the PRD silently — declare the read in one line; the chain continues.

`.gm/prd.yml` is the central store. `|F|=1`: every plan-item lands here, no parallel TODO lists in the response body, no inline checklists. PRD items proposing unrequested `.md`/`.txt` rejected on add.

**Maturity-first invariant**: a PRD row that ships a smaller-than-necessary slice while a larger reachable shape exists is distributed refusal — re-emit as the closure, not the scaffold. "Phase 1 of N" partitioning of single-session work is L3 deferral, not planning.

## Mutables

`.gm/mutables.yml` co-equal with PRD. Unknowns enter at `status: unknown`. Transition to `witnessed` only on concrete witness: `file:line`, codesearch hit, exec output snippet. Orchestrator hard-rejects narrative resolution and hard-rejects COMPLETE while any row is unwitnessed or any PRD item is unresolved.

## Closure Anti-Shapes

See entry. At PLAN exit boundary, the tempting failure is "scope-it-first instead of build-it" — same shape as `<COMPONENT>-SPEC.md` instead of the implementation.

## Dispatch

`recall`, `codesearch`, `prd-add`, `mutable-add`, `mutable-resolve`, `transition`. Pack opens → PRD writes → mutables file → transition fires.
"#;
