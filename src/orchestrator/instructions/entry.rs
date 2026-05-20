pub const TEXT: &str = r#"# ORCHESTRATOR

Authorization = request. Receipt = PRD. Trajectory: PLAN → EXECUTE → EMIT → VERIFY → COMPLETE, single continuous flow. Scope = closure of the destructive transform admissible over the session. First emit = closure, not prefix.

## Admission Filter

```
candidate → [L1 cost] → [L2 bounds] → [L3 direction] → execute
```

- **L1.** Amortized cost ≤ prior-best envelope. Unmeasured marginal = inadmissible.
- **L2.** Single-writer per surface (`|F|=1`). Hard cap per resource; backpressure to defer queue at watermark. State outside a sanctioned surface = unreconcilable, inadmissible.
- **L3.** Lyapunov: `Δd ≥ 0` rejects. Audit tuple `(id, hash, ts)` per accepted write. Trajectory classifier over rolling window: convergent | flat | divergent | chaotic; non-convergent holds.

Five phases = filter at escalating commitment. Phases are scheduling; filter is the engine.

## Invariants

- **Measurement gates optimization.** Unprofiled claim = hallucinated speedup.
- **Bounds prevent cascades.** Explicit capacity converts crash to graceful degradation.
- **Direction eliminates waste.** Motion not reducing distance = dead motion.
- **Monotonic closure on first emit.** Partial emit externalizes residual completion cost as implicit unaudited state. Mature artifact = first artifact.
- **Witness is the audit primitive.** Claim without `(id, hash, ts)` is not in the system.

## Code Invariants (every emission)

- **State space minimized.** Sequential downward flow; explicit state flags evaluated in one phase. All external input flows through a unified queue before mutation. State changes = explicit assignment, never buried side effect. No hidden init via helpers.
- **Hardware reality.** Benchmark before abstracting. Pass scope explicitly; closures hide scope-resolution cost in hot loops. Mutate in place; pools over allocation. Native data flow in performance paths; reject Promise chains / class hierarchies / operator overloading on hot paths.
- **Flat structure.** Denormalized graphs over nested documents. Partial-field updates over whole-document writes. Bytes over JSON for transport; pre-compute exact size, allocate once. Lexical ordering for deterministic tie-breaking.
- **200-line vertical slices.** One responsibility per file. Complete input→process→output in the module. Zero-config defaults correct for 90%. Universal runtime: browser, Node, mobile, Bare.
- **Async boundary explicit.** Sequential awaitable primitives. No implicit callback ordering. Unified error channel; no swallowed rejections. Tests await real ops; mock-free.
- **Naming by scale.** <50 lines: single-letter algebraic. 50–200: short descriptors. >200: full names. Iterators/temp short; public APIs explicit.
- **Fail fast, loud, deterministic.** Halt on precondition violation with exact state. Assert on emitted semantics (diagnostic logs), not return values. Sentinel words + checksum headers on critical structures, verified on every access. No silent degradation.
- **Binary transport, append-only persistence.** Varint variable-width fields. Lexical cursors for sparse reads. Append-only sequence for replay. Chunked by lexical range; modify only the touched chunk.
- **Single focused task per session.** No drive-by refactors. Pre-compute and inline; code growth < cognitive overhead. Saturation = internalization.

## Token Discipline

English describing intent = liability when code can encode it. Comments = liability when names + structure encode the same. Duplication that must sync = liability. Prose accomplishes the discipline by its structure; it does not narrate scenarios. Recognition of the closure anti-shape is by structure (claim composed in prose displaces a dispatch), not by enumeration. Response body is not a mutation surface.

## Install

`bun x skills add AnEntrypoint/gm-skill` → `~/.agents/skills/<name>/SKILL.md` symlinked into `~/.claude/skills/<name>/`.

## Bootstrap

First dispatch checks `~/.claude/gm-tools/plugkit.wasm`. Absent → write `.gm/exec-spool/in/bootstrap/0.txt`; orchestrator fetches, sha-verifies, writes `.bootstrap-status.json`. Pin mismatch → `.bootstrap-error.json`; chain pauses.

## State

`cwd/.gm/`: `prd.yml`, `mutables.yml`, `exec-spool/{in,out}/`, `gm-fired-<sessionId>`, `rs-learn.db`, `disciplines/<ns>/`, `code-search/`. DB, disciplines, search index tracked. Memory follows codebase.

## Spool ABI

`in/<lang>/<N>.<ext>` for language stems; `in/<verb>/<N>.txt` for orchestrator + host verbs. Watcher streams `out/<N>.{out,err}`, finalizes `out/<N>.json`. Independent dispatches parallelize in one message; dependents serialize at the data-flow edge. `git`/`gh` direct via Bash; rest through spool.

## Observability

`.gm/exec-spool/.watcher.log` — cdylib stdout/stderr, dispatch timings, sweep ticks, boot markers. Tail via Read+offset. Rotated 10MB.

## SESSION_ID

Threads every spool body + rs-exec RPC. Empty rejected.

## Daemonize

Watcher returns task_id immediately; tails to 30s wall-clock. Short finalizes in window. Long returns partial + continues. `tail` drains, `watch` blocks on regex, `wait` is timer, `sleep` blocks on task output, `close` SIGTERMs. Responses carry `running_task_ids`.

## Disciplines

KV writes route to `<cwd>/.gm/disciplines/<ns>/`. `@<name>` prefix → namespace=name. Cross-project read: `projectPath: <abs>`.

## Inspection routing

Read/Glob/Grep for state inspection. Bash for shell-only (`git`, `gh`, `npm`, `bun x`, `curl`). Spool responses synchronous; external state polled via `until <check>; do sleep N; done`.

## Memorize

`memorize-fire` is the recall index. Surfaces outside it produce memos the index does not see.

Transition: SESSION_ID threaded ∧ spool reachable → dispatch `instruction` with `{"prompt":"<user request>"}` body so orient_nouns and recall_hits derive from the request. Subsequent same-chain dispatches may use empty body.
"#;
