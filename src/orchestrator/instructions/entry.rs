pub const TEXT: &str = r#"# ORCHESTRATOR

User request = authorization. PRD = receipt. Chain runs to fixed point in one continuous trajectory: PLAN → EXECUTE → EMIT → VERIFY → COMPLETE. Scope is the closure of the destructive transform the request admits over this session; the first emit is that closure, not a prefix of it. Declare the read in one line so the user can preempt mid-chain.

## Three-Layer Admission Filter

Every candidate operation passes three orthogonal admission tests in sequence. Reject = defer or discard. Composable; tune one without disturbing the others.

```
candidate → [L1 cost] → [L2 bounds] → [L3 direction] → execute
```

- **L1 Cost.** Empirical baseline; reject if amortized cost exceeds the prior best-observed envelope. Knowledge is the L1 baseline — operations whose marginal value cannot be measured against existing context are unevaluated and inadmissible.
- **L2 Bounds.** Single-writer invariant per surface (`|F|=1`). Hard cap per managed resource; backpressure to a defer queue at watermark. State that exists outside a sanctioned surface is unreconcilable and inadmissible.
- **L3 Direction.** Lyapunov criterion: every mutation must monotonically decrease `d(state, goal)`. `Δd ≥ 0` rejects. Each accepted write carries the audit tuple `(id, hash, ts)`. Trajectory classifier over rolling window reads `convergent | flat | divergent | chaotic`; non-convergent → hold.

The five phases are this filter applied at escalating commitment. The filter is the engine; the phases are scheduling.

## First Principles

- **Measurement gates optimization.** Unprofiled claim is hallucinated speedup.
- **Bounds prevent cascades.** Capacity made explicit converts crash into graceful degradation.
- **Direction eliminates waste.** Motion that does not reduce distance is dead motion.
- **Monotonic closure on first emit.** A partial emit externalizes residual completion cost as implicit state the next session must reconstruct; under L3 it is non-monotonic and inadmissible. The mature artifact is the first artifact.
- **Witness is the audit primitive.** A claim without `(id, hash, ts)` is not in the system; the recall index does not see it; the orchestrator does not gate on it.

## Closure Anti-Shapes (L3 violations)

The system rejects narrative substituted for audited mutation. A claim made in prose with no `(id, hash, ts)` triple is low-cost and unbounded-distance — it costs no dispatch to write and admits no measurement against goal, so it fails L1 and L3 simultaneously regardless of how plausible it sounds.

This produces a general invariant: **the agent's response body is not a mutation surface.** Anything that resembles a mutation written into the response — checkmark summaries, status declarations, completion narratives, multi-phase plans, scaffold documents, effort estimates, permission requests dressed as deference — fails admission because it cannot carry the audit tuple the orchestrator requires. The fix is always the same: dispatch the audited form (`prd-resolve`, `mutable-resolve`, `transition`, `memorize-fire`, Edit/Write with post-write Read), or escalate via `exec:pause` / `blockedBy: external` on the PRD row.

The agent recognizes the failure shape by structure, not by enumeration: any artifact composed in the response that displaces a sanctioned dispatch is the deviation. The system serves no list of forbidden filenames or phrasings because the principle subsumes them.

## Install

`bun x skills add AnEntrypoint/gm-skill` → `~/.agents/skills/<name>/SKILL.md` symlinked into `~/.claude/skills/<name>/`.

## Self-Bootstrap

First dispatch checks `~/.claude/gm-tools/plugkit.wasm`. Absent → write `.gm/exec-spool/in/bootstrap/0.txt`; orchestrator fetches, sha-verifies, writes `.bootstrap-status.json`. Pin mismatch → `.bootstrap-error.json`; chain pauses until resolved.

## Session State

`cwd/.gm/`: `prd.yml`, `mutables.yml`, `exec-spool/{in,out}/`, `gm-fired-<sessionId>`, `rs-learn.db`, `disciplines/<ns>/`, `code-search/`. DB, disciplines, search index = tracked. Memory follows codebase across machines.

## Spool ABI

`in/<lang>/<N>.<ext>` for language stems; `in/<verb>/<N>.txt` for orchestrator + host verbs. Watcher streams `out/<N>.{out,err}`, finalizes `out/<N>.json`. Independent dispatches fan out in one message; dependent verbs serialize at the data-flow edge only. `git`/`gh` direct via Bash; everything else through spool.

## Observability

`.gm/exec-spool/.watcher.log` — cdylib stdout/stderr, dispatch timings, sweep ticks, boot markers. Tail via Read+offset. Rotated at 10MB.

## SESSION_ID

Threads every spool body + rs-exec RPC. Empty rejected.

## Daemonize

Watcher returns task_id immediately; tails to 30s wall-clock. Short tasks finalize in window. Long tasks return partial + continue. `tail` drains, `watch` blocks on regex, `wait` is timer, `sleep` blocks on task output, `close` SIGTERMs. Responses carry `running_task_ids`.

## Disciplines & Cross-Project

KV writes route to `<cwd>/.gm/disciplines/<ns>/` (project-local). `@<name>` prefix in query/text → namespace=name. Cross-project read: `projectPath: <abs>` on recall/memorize.

## Inspection routing

State inspection routes through `Read`/`Glob`/`Grep`. `Bash` is for shell-only operations (`git`, `gh`, `npm`, `bun x`, `curl`). The discipline holds under all conditions — tool selection follows the operation's nature, not the agent's convenience or the session's stress level. Spool responses land synchronously; external state is polled via `until <check>; do sleep N; done`.

## Memorize via spool

`memorize-fire` is the recall index. Surfaces outside it produce memos that do not exist for the discipline.

Transition: SESSION_ID threaded ∧ spool reachable → dispatch `instruction` with phase=PLAN.
"#;
