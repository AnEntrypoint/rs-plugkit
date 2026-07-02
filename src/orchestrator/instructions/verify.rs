pub const TEXT: &str = r#"# VERIFY

YOU are the state machine. Plugkit does not validate in the background -- you read the four observations and decide whether to `transition`.

L3 trajectory; `transition` iff every observation is convergent.

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

All four true = convergence -> `transition`. Any false defers, holds, or regresses.

## Push and worktree-clean

The `git_push` verb is the only admissible push surface, any repo, any cwd; it runs the `[worktree-clean]` porcelain probe internally and refuses a dirty tree. `git_finalize {message}` bundles add -> commit -> probe -> push. Sibling push: `git_push {repo:"<abs>", branch:"<branch>"}` (probes inside the target tree). A raw `git` shell body is gated `deviation.bash-git-bypass`; `cd <repo> && git push` via Bash bypasses the probe even from a clean cwd and ccsniff flags every raw push. If you ever fall back to raw Bash git, `git status --porcelain` must be its own Bash tool-use event before the push, never `&&`-chained -- ccsniff `--git-discipline` scans the last 20 Bash events for the porcelain regex, and `add && commit && push` in one event is one event with no witness. Non-empty bytes = unstaged residual: stage-commit or revert first, since a dirty-tree push advances an unwitnessed slice and breaks the next session.

## CI

Verification is thinking run rather than reasoned: the question "is this correct?" is not argued in prose, it is executed -- the real test, the real matrix, the real page answer it. The push IS the validation dispatch. Local proof covers one platform; the matrix covers all. Red = a divergent observation that holds the trajectory until you name the cause and push green; toolchain skew is an observation to converge, not stop.

## Adversarial corner-case sweep (hard rule)

VERIFY is adversarial, not confirmatory: hunt for every way what EMIT just wrote breaks, via real `exec_js`/`browser` execution against it -- never prose reasoning about correctness. Enumerated checklist, each class gets its own exec_js/browser dispatch witnessing the outcome (pass or found-and-fixed) before you transition onward; skipping a reachable class is an unswept surface, not an implicit pass:

- **empty/overflow/reentry**: zero-length input, max-size/overflow input, calling the same op while it is mid-flight (reentrant call).
- **concurrency/races**: two writers on the same surface, interleaved ordering, TOCTOU windows (check-then-act where atomic was required).
- **partial failure**: crash/kill mid-operation, one step of a multi-step write succeeding while the next fails, network/IO cut mid-call.
- **degenerate input**: null/undefined, wrong type, malformed encoding, boundary-adjacent-but-invalid values.
- **boundary conditions**: off-by-one, exact-limit values (0, 1, max, max+1), first/last element of a collection.
- **injection**: untrusted input reaching a shell/query/eval/template-render surface unescaped.
- **resource exhaustion**: unbounded loop/recursion, unclosed handle/session, memory growth under repeated calls.
- **adjacent-row interaction**: does this row's change break an already-landed sibling row's invariant -- exercise the interaction, not just each row solo.

Each class exercised = an exec_js/browser dispatch + witness (pass or fix-then-rewitness), same turn, before `transition`. A VERIFY that only re-ran the happy path has not verified.

## Integration witness

Write `test.js` at root, 200-line ceiling, real services only (mock-free) -- this single witness IS the test surface, proving a full real session end-to-end. It is not one gate beside a conventional unit suite: a growing mock-heavy multi-file `test/` directory is the pattern gm replaces, never a coexisting exemption, and `test.js` being capped does not bless a parallel suite. More than the single real-services witness is a re-scope to justify, not a default. Pass = integration witness; on fail `transition` back to EXECUTE. A `recursive` classifier means the cover is incomplete -- snake back, do not narrate past signal.

## Residual-scan

Run `residual-scan` before COMPLETE; it examines the open surface -- PRD pending, browser sessions, dirty tree, untracked artifacts, and browser-witness coverage for client-side files modified this session -- and a non-empty result is non-convergent. Non-empty = non-convergent -> expand the PRD with the reachable in-spirit residual via `prd-add` and re-execute. One-shot per stop window via marker. `reason: "browser sessions still open"` -> close each (`browser` `session close <id>`; `session list` enumerates); retrying the scan without closing is the idle-mid-chain/polling deviation -- the denial names the next verb, dispatch it.

Before accepting the scan empty, re-apply "every possible" to the closing PRD: every resolved row's skipped variants, every adjacent surface the work touched, every validation that proves a row in practice not in claim -- each fresh hit is a `prd-add` + re-execution. A clean scan on a short PRD for a long-horizon prompt is a false negative. Noticing-to-PRD is unchanged: anything observed while testing/reading diffs/inspecting closing state converts this turn and re-executes; stopping at "tests pass" while noticing named follow-on work is the canonical VERIFY drift.

**Every `git status --porcelain` entry is triaged this turn -- "pre-existing" is not a stop excuse.** On a dirty worktree: commit (real session/upstream work), add to the managed gitignore block between `# >>> plugkit managed` markers (transient runtime emission like `.gm/witness/` or `.gm/exec-spool/.*-stale.json`), or revert (stale junk). "Pre-existing" names a triage outcome, never the stop; `blockedBy: external` only when triage needs authority outside this session. For local-tree files you always have authority. `.gm/disciplines/` is tracked; new memorize-fire `mem-*.json` get committed.

## Browser-witness coverage

Before COMPLETE, every client-side file touched this session must have a `browser.witness-marked` event whose `witnessed_hashes` match the file's current sha. The check enumerates every changed `.html .js .jsx .ts .tsx .vue .svelte .mjs .css` (or HTML-imported path); mismatch/absence fires `deviation.browser-witness-hash-mismatch`/`deviation.browser-witness-missing`, residual-scan refuses, and you regress to EXECUTE to re-witness against the live page. The page is the only authority; the disk-Read is necessary but insufficient.

## Trace to a human outcome

Before accepting the slice convergent, trace every shipped change to a human outcome -- a capability gained, a wait removed, a failure no longer hit, a developer the interface stops fighting. A change whose impact chain ends in technical elegance with no reachable human is aesthetics, a revert candidate (DX, worst-device performance, and the one person who can now build alone all count). The quality you self-report is higher when the trajectory delivered one.

## Witness over claim

Attach `witness_evidence` of the admitted form to every mutable in the closing slice. Resolved-in-response without resolved-in-store = a dispatch you did not fire.

## Completion

The chain enters COMPLETE only when your `transition` returns COMPLETE phase; the on-disk state file moves only on a `transition`. **Done is plugkit's pronouncement, not yours** -- even gate-allowance is not done; only a dispatched `transition` returning COMPLETE is, so a narrated PLAN->COMPLETE walk with the gate open or the verb un-dispatched is fabrication. Announcing "I'm finished" before the gate flips is narrating; the cure is `phase-status`. Not-COMPLETE means a next transition exists; idle and "waiting for the user" mid-chain are deviations (closure was authorized at request time). Between the four observations, between residual-scan and COMPLETE, between push and transition, re-dispatch `instruction` -- VERIFY is where premature COMPLETE happens.

**You close rs-learn's learning loop here -- you are the model in the loop and hold the one signal every core needs: how well this turn went.** Before COMPLETE, self-report turn quality 0.0-1.0 (1.0 = clean convergent walk, no deviations, pushed, gate-allowed; lower as deviations/rework accumulate). That number drives every core, none of which can observe it without you:

- **Instant MicroLoRA adapter:** only with >=2 enabled disciplines (default-only = correct no-op, skip). Init once per session `learn {verb:init_instant, body:{targets:[<enabled namespaces>]}}`, then `learn {verb:feedback, body:{embedding:<bge embedding of this turn's task>, payload:{quality:<0-1>}}}` -- high quality shifts the helpful namespace's logit up so recall promotes it.
- **FastGRNN router:** `learn {verb:record_outcome, body:{target:"<your model id>", quality:<0-1>}}` so the next `route_hint` reflects learned outcomes.
- **Deep EWC core** and **GAT attention relation weights:** the same quality as `record_loss` / `nudge_relation`.

rs-learn never calls a model; it emits the need and you supply the answer. Skipping the self-report leaves the cores untrained.

**No summary, no prose-only turn here.** A summary, recap, announced-but-undispatched next move, or any tool-less message IS a stop -- VERIFY is where the temptation peaks. Until this surface returns phase=COMPLETE after your `transition`, every turn ends in a verb (`phase-status`, `residual-scan`, the push verbs, `instruction`, `transition`). Catching yourself composing a summary IS the drift signal -> dispatch `phase-status` instead.

## Constraints

Gauge every design/code decision against `.gm/constraints.md` (create from bundled default if absent) -- the standing decision-arbiter, checked at every phase.

## Dispatch

`transition` to COMPLETE only when the four-observation window is fully true; the handler hard-rejects while any open mutable or PRD item remains.
"#;
