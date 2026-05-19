pub const TEXT: &str = r#"# PLAN

Every turn begins with prior memory already loaded by auto-recall. PLAN adds targeted reconnaissance on top of that injection. Before any unknown is named as absent, search for it. Before an abstraction is designed, check the codebase for one that already exists.

## ORIENT

The first action of PLAN is a parallel pack: 3–5 `exec:recall` calls and 3–5 `exec:codesearch` calls against the request's nouns, dispatched in one message. Hits become weak_prior — still witnessed before adoption. Misses confirm the unknown is fresh. The pack is free relative to the duplicated discovery and disagree-with-prior-witness risk it prevents. Serial probing of nouns one-at-a-time is the failure mode this discipline guards against.

Spool the pack as the opening move:

```
.gm/exec-spool/in/recall/1.txt   "<noun phrase 1>"
.gm/exec-spool/in/recall/2.txt   "<noun phrase 2>"
.gm/exec-spool/in/recall/3.txt   "<noun phrase 3>"
.gm/exec-spool/in/codesearch/1.txt   "<two-word phrase 1>"
.gm/exec-spool/in/codesearch/2.txt   "<two-word phrase 2>"
.gm/exec-spool/in/codesearch/3.txt   "<two-word phrase 3>"
```

All in one message. Read `out/*.json` together.

## Maximal Cover

The default shape of the work is the widest, deepest, most destructive reading the request admits from this session. Scope-exceeds-reach is a planning condition, not a stopping condition. The covering family is the plan. Enumerate every bounded subset of the request witnessable from this session; write the family into `.gm/prd.yml` with the dependency graph explicit. Residuals within the spirit of the ask AND reachable from this session are self-authorized — expand the PRD and declare the read in one line ("treating X as in-spirit because Y"). Only out-of-spirit or unreachable residuals are name-and-stop.

When two viable shapes exist — the narrower and the broader — the narrower carries the burden of proof. "Maybe this is too much" is the agent's question, not the user's, and its answer is no whenever the broader shape matches the spirit of the ask. Build cost, CI duration, cross-repo reach, and breaking-change weight do not shrink the cover; they are inputs to executing it, not arguments against it.

## Mutables File

`.gm/mutables.yml` is co-equal with `.gm/prd.yml`. Every unknown surfaced lands as a row with `status: unknown`. The hook layer hard-blocks Write, Edit, `git commit`, `git push`, and stop while any row remains unknown. Rows flip to `witnessed` only when `witness_evidence` carries concrete proof — file:line, codesearch hit, exec output snippet. Narrative resolution is rejected on read. PLAN exits only at ε = 0 on the final pass.

## PRD Mandatory

Writing `.gm/prd.yml` is non-negotiable for every task whose scope exceeds a literal single-file single-line edit. Skipping the PRD costs the same as writing it (the work is enumerated mentally either way) and loses durable trace, resumability, and the cover-maximality check.

## No Unsolicited Docs

A PRD item to "write a summary doc", "add an IMPLEMENTATION.md", "ship a START-HERE.md", or "drop a *-STATUS.md" is rejected as a planning artifact unless the user explicitly asked for that file. Closure narrative belongs in the PRD entry's own description, in `memorize-fire` evidence, and in the commit message — not in a new file at project root or under `docs/`. Planning to write a doc the user did not request is the same failure as planning to mock the test.

## Dispatch

`phase-status` to read FSM state. `transition` to advance. `mutable-resolve` to mark witnessed (auto-fires memorize). Plus the usual `recall`, `codesearch`, `memorize`, `health`, language stems.

Transition: when the PRD is written, ORIENT pack results are read, and every surfaced unknown is either `witnessed` or filed in `.gm/mutables.yml`, dispatch `transition` to advance to EXECUTE.
"#;
