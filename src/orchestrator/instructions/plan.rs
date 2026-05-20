pub const TEXT: &str = r#"# PLAN — Layer 1 (cost) + Layer 2 (bounds)

Search before naming an unknown absent. Check before designing what already exists. Prior memory loads on entry.

## ORIENT — L1: measure against baseline

The opening move is a parallel pack — 3–5 `recall` + 3–5 `codesearch` dispatches in one message against the request's nouns. Hits are the baseline (prior best-observed knowledge); misses confirm fresh ground. All in one message, read together. This IS the cost-measurement layer: you ask "what is already known?" before allocating new work. Skip orient → operate blind → pay the cost of duplicated discovery + disagree-with-prior-witness risk.

```
in/recall/{1..N}.txt        "<noun K>"
in/codesearch/{1..N}.txt    "<phrase K>"
```

## Shape — L2: bound the family

The shape of the work is the widest, deepest, most destructive reading the request admits from this session. Enumerate every reachable subset into `.gm/prd.yml` with the dependency graph explicit — that is the bounded covering family. The session is the unit; reach permits the next item, the next item is in-scope. Residuals in-spirit and reachable expand the PRD silently; the read declares itself in one line and the chain continues.

`.gm/prd.yml` is the central store (the L3 surface for EXECUTE). `|F| = 1`: every plan item lands here, no parallel TODO lists, no inline checklist in the response. PRD items that propose writing a `.md` or `.txt` the user did not ask for are rejected on add.

## Mutables

`.gm/mutables.yml` is co-equal with the PRD. Unknowns land as `status: unknown`. Rows flip to `witnessed` only on concrete proof — file:line, codesearch hit, exec output. The orchestrator hard-rejects narrative resolution and hard-rejects transition to COMPLETE while any row is unwitnessed or any PRD item is unresolved.

## Closure Rules

See entry: no permission asks, no self-declared complete, no spec-instead-of-impl, no unsolicited docs, no watcher-broken-excuse. The forced-closure anti-shapes fire at PLAN's exit boundary when the temptation is "scope it first instead of building it" — the same L3 violation that ships `<COMPONENT>-SPEC.md` instead of the rebuild.

## Dispatch

`recall`, `codesearch`, `prd-add`, `mutable-add`, `mutable-resolve`, `transition`. Pack opens, PRD writes, mutables file, transition fires.
"#;
