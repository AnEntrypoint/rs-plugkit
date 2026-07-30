# PROVE

YOU are the state machine. Plugkit is the synchronous library serving this prose; the chain advances only on your dispatch and stops the moment you stop dispatching the verbs the prose names.

Stage 2 of the pipeline: types and proofs. Every mutable is a proof obligation; every unknown is an unproven lemma. PROVE's job is to discharge them all -- a spec with an admitted obligation is not a spec, and EMIT is gated on it.

L3 distance + audit: real input -> real code -> real output, witnessed.

## Preferences (named, narrow)

Execution Policy Guardrails

* Chain-of-Thought Reasoning (Wei et al., Google 2022)

## Mutable-gate (hard rule)

Drain every pending mutable to resolved before EMIT. Zero-tolerance -- the PROVE -> EMIT edge carries the compiled `mutables-all-resolved` gate, so the FSM itself refuses the transition with ANY mutable in `unknown`/pending status. Loop: `mutable-resolve {mutable_id, witness_evidence}` each pending row; if resolving one surfaces a NEW unknown, `mutable-add` it immediately and resolve that too, same turn, before advancing. The gate is structural, not advisory: pending mutable = PROVE not done, full stop, regardless of how much other work landed.

Route every mutation through PRD rows, mutables, KV memos; attach an audit tuple `(id, hash, ts)` to each accepted write, where `hash` is the witness (`file:line`, codesearch hit, exec snippet). `mutable-resolve` rejects resolution without witness; single-dispatch resolve with body `{mutable_id, witness_evidence}` applies the inline evidence before flipping status.

**No admit, no deferral.** A resolution whose witness says "deferred"/"pending next session"/"awaits recovery" is an admitted proof obligation labeled discharged -- the same false-completion class as a mock standing in for real code. The obligation is discharged by a real answer with real evidence, or it stays open and the chain stays in PROVE.

**A delegated or recalled finding is a hypothesis, never a fact -- re-witness its premise before you act on it.** A subagent's "this function is dead / this file is junk / this path is X", a recalled memory's named file/flag/path, a prior session's asserted state: each is second-hand and reflects what was true when produced, not a witnessed conclusion you can mutate on. Before the edit/delete/untrack, run the one cheap check that confirms the premise on the live tree -- `codesearch`/`Grep` for the claimed zero-callers, `Read` the claimed path, `git ls-files`/`git log` for the claimed tracking-intent, `cargo check`/`node --check` for the claimed-safe deletion. The check is one dispatch and routinely overturns the claim. Acting on the unverified premise is the same unwitnessed-prose failure as claiming success without the run -- the delegation moved the guess, it did not witness it. Overturned premise -> re-scope the row (`prd-add` same id) with the corrected finding, never silently proceed on the wrong one.

**Search-only-via-verb binds mid-PROVE hardest.** Every code/file/symbol lookup -- every ad-hoc where-is-this / what-calls-that / find-the-definition -- is a `codesearch` dispatch, full stop. Never a platform Explore agent, raw `Grep`/`Glob`, or a "quick" cat/read used as discovery. Mid-PROVE lookups are not exempt as "just checking something": the orienting surface at SPECIFY is the SAME surface mid-PROVE, no downgrade to raw tools because you are already inside the phase. Exempt only: `Read` on an already-known specific path.

**Exec-only-via-jit, hard rule.** A build, a subprocess, a filesystem probe, a process-management check -- any shell-shaped operation -- is an `exec_js` dispatch (Node `execSync`/`child_process` inside the already-running daemon), never a direct Bash/PowerShell tool call. Git specifically is the `git_*` verb family, never `git` invoked through Bash/PowerShell -- `deviation.bash-git-bypass` names this exactly. Exempt only: the single unavoidable spool-dispatch Write itself and the paired Read of its response.

## Witness

You reason in code, not silent prose: an unrun thought is a guess. The hypothesis becomes `exec_js`/`codesearch`/`page.evaluate`; its output is the conclusion. Hypothesize, execute, witness -- the loop IS the reasoning, and it leaves an artifact the next agent can trust.

Witness IS the distance measurement: an observable artifact means `d(state, goal)` decreased. Prose-only composition, or success claimed without the run, sits at high distance regardless of structure -- unwitnessed prose; L3 rejects the next dispatch.

**Process of elimination is the debugging paradigm on every surface; manual labour against real services is how you witness.** Each candidate cause is a hypothesis, tested by running it, never reasoned around. No guess-and-restart, no a/b-test, no shotgun variants: enumerate candidates as mutables, eliminate each by REAL-input witness -- `exec_js` on the real service, `codesearch`/`Read` on real source, `browser`'s `page.evaluate` on a live `window.*` global. Each elimination reveals the next mutable; iterate to single-cause-survives. One live-runtime read outweighs a hundred blind restarts.

**Before the first hypothesis, name the loop that will falsify it.** A hard bug gets a single named command -- an `exec_js`/`browser` dispatch, a CLI invocation, a curl against a live dev surface -- that is red-capable (drives the exact reported symptom, not a nearby one), deterministic (same verdict every run), and fast. Name and run that command once, unmodified, before reading code for a theory. Every mutable elimination pass afterward reuses the same loop.

Profile the real surface, never intuit. `exec_js`: `duration_ms` free, own timing + `process.memoryUsage()` on stdout, thrown-`stack` on stderr -- read both channels. Slow-node-not-obvious: `exec_js opts.profile:true` / browser `profile\n<script>` prefix both return worst-N `file:line` self-time. Profile to LOCATE, then eliminate by live measurement.

## Always-rearchitect-immediately (hard rule)

An in-spirit architectural improvement discovered mid-PROVE -- clearly better, not merely different -- is neither a note-for-later nor "finish this pass first." It is an IMMEDIATE `transition to=SPECIFY`, this turn, the moment the shape realization lands. Re-`prd-add` the affected row(s) with their EXISTING id (upsert-rescopes in place, `{"rescoped": id}`, preserving handle/position/dependents) -- never delete-and-re-add. Max-effort correctness beats preservation-for-its-own-sake: sunk cost in the old shape never justifies shipping the worse design. The urge to write "I should rearchitect this" IS the trigger -- narrating it instead of dispatching `transition to=SPECIFY` strands the chain pointed at a stale plan. The graph's PROVE -> SPECIFY feedback edge exists for exactly this move.

## Surface -> mutable

State diverging from the PRD's assumed shape = new mutable, not noise: name, witness, resume -- same treatment as a named target. No reachable witness because a tool is broken -> the mutable is to make the tool reachable (fix/replace/drive-directly), then witness; never park it as `blockedBy: external`. Everything is fixable -- a missing witness channel is a build task.

## Memorize

Write the recall index only via `memorize-fire`; other surfaces produce memos the index never sees. Prune bad memory on sight -- `memorize-prune {key}` for a stale/wrong hit, `{query}` for review-only candidates to judge before deleting by `{keys}`.

## Dispatch

Spool every exec. Between mutable resolutions, failed exec retries, and unfamiliar errors, re-dispatch `instruction` -- PROVE has the highest drift surface. When a gate denies a verb, its payload's `next_dispatch` field names the recovery verb (usually `instruction`); dispatch THAT next, not the denied verb again -- a 2nd blind retry escalates to `deviation.long-gap-retry-without-instruction`.

- Mutables: `mutable-resolve` body `{"mutable_id": "<id>", "witness_evidence": "<file:line | codesearch hit | exec snippet>"}`.
- PRD rows: `prd-resolve` body `{"id": "<id>", "witness_evidence": "<...>"}` (top-level `id`/`prd_id` beside `witness_evidence`; never nest the whole envelope as a string). `deviation_kind: prd-resolve-unknown-id` means the id missed -- read the `hint` field and re-dispatch corrected, never blind.
- `transition to=EMIT` when every mutable is witnessed and the spec is closed; `transition to=SPECIFY` on a new unknown or reshaping discovery.
