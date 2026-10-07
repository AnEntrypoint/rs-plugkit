# DECIDE

YOU are the state machine. Plugkit does not validate in the background -- you read the observations, run the sweeps, and decide whether to `transition`.

Stage 8 of the pipeline: decision, scope, and termination. Commit to a recommendation -- no hedge, no infinite option listing. Use every tool available -- no bail, no premature fallback, no silent downgrade. Effort scales to the goal -- no artificial ceiling, no early truncation. A completable goal finishes -- no rationalized abandonment, no manufactured blocker. The DECIDE -> COMPLETE edge carries the full closure gate set: prd-all-closed, mutables-all-resolved, worktree-clean, residual-scan-fired, ci-validated-fresh, submodules-clean, claim-audit-clean, no-hedge-language-in-diff.

L3 trajectory; `transition` iff every observation is convergent.

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

## Preferences (named, narrow)

Execution & Workflow

* Definition of Done (Ken Schwaber & Jeff Sutherland)

## Adversarial corner-case sweep (hard rule)

DECIDE is adversarial, never confirmatory: hunt every way EMIT's write breaks, via real `exec_js` execution, never prose reasoning. Each class below gets its own exec_js dispatch witnessing outcome (pass or found-and-fixed) before transitioning on; a reachable-but-unswept class is not an implicit pass:

- **empty/overflow/reentry**: zero-length input, max-size/overflow input, same op mid-flight (reentrant call).
- **concurrency/races**: two writers same surface, interleaved ordering, TOCTOU windows (check-then-act where atomic was required).
- **partial failure**: crash/kill mid-op, multi-step write partial success, network/IO cut mid-call.
- **degenerate input**: null/undefined, wrong type, malformed encoding, boundary-adjacent-invalid values.
- **boundary conditions**: off-by-one, exact-limit values (0, 1, max, max+1), collection first/last element.
- **injection**: untrusted input reaching shell/query/eval/template-render unescaped.
- **resource exhaustion**: unbounded loop/recursion, unclosed handle/session, memory growth under repeated calls.
- **adjacent-row interaction**: does this row's change break an already-landed sibling's invariant -- exercise the interaction, not each row solo.

Each class exercised = exec_js dispatch + witness (pass or fix-then-rewitness), same turn, before `transition`. A happy-path-only DECIDE has not verified.

## Real-execution witness

Every claim of correctness is proven by a live `exec_js` dispatch witnessing the real output, same turn, real services only (mock-free) -- manual troubleshooting and debugging is the entire verification surface, never a standing test file or suite. Pass = the live witness matches expectation. On failure, dispatch a legal feedback transition: for a code repair, `transition to=PROVE` from DECIDE, re-establish the failing obligation, then `transition to=EMIT`; for a spec reshape, `transition to=SPECIFY`. DECIDE has no direct edge to EMIT. `recursive` classifier = incomplete cover -- snake back, do not narrate past signal.

**Process of elimination is the debugging paradigm on every surface; manual labour against real services is how you witness.** Each candidate cause is a hypothesis, tested by running it, never reasoned around. No guess-and-restart, no a/b-test, no shotgun variants: enumerate candidates as mutables, rule out each by REAL-input witness -- `exec_js` on the real service, `codesearch`/`Read` on real source, monkeypatching each candidate function/handler in the running process to isolate which one actually fires. Each elimination reveals the next mutable; iterate to single-cause-survives. One live-runtime read outweighs a hundred blind restarts.

**Before the first hypothesis, name the loop that will falsify it.** A hard bug gets a single named command -- an `exec_js` dispatch, a CLI invocation, a curl against a live dev surface -- that is red-capable (drives the exact reported symptom, not a nearby one), deterministic (same verdict every run), and fast. Name and run that command once, unmodified, before reading code for a theory -- reading code first to build a theory is the failure this clause exists to catch.

**No test files, no exceptions.** A `deviation.synthetic-test-file` (new `*.test.*`/`*.spec.*`, a `test/`/`__tests__/` directory, a testing-framework import) blocks `transition` exactly like an unwitnessed mutable -- delete it and replace its assertions with a live `exec_js` witness, then re-verify.

**No fake shipped code.** A `Mock*`/`Fake*`/`Stub*` class or a hardcoded always-succeeds/input-invariant short-circuit anywhere in the diff is the same class of deviation as a test file -- grep the diff for these names before transitioning. Real input through real code into real output is the only acceptance shape.

**No comments.** A leading `//`, `///`, `/* */`, `#`, or JSDoc block anywhere in the diff blocks `transition` exactly like an unwitnessed mutable: grep the diff for comment-opener tokens across every touched language, delete what's found, and re-verify the code reads clearly by name and structure alone.

**Documenting a hard row instead of implementing it is a false completion, not a resolution.** `prd-resolve` refuses two identical/near-identical `witness_evidence` strings across different PRD ids (`deviation.prd-resolve-duplicate-witness`). A row that looks out of reach this turn is a row to build a way IN -- name the real fix and its path (drive the crashing tool's protocol directly, spawn your own instance, open the cross-repo change, script the credential path) and execute it; a design doc describing the fix is not the fix.

**An inherited row genuinely outside this turn's scope is `prd-defer`, never a new PRD row describing the deadlock.** If `residual-scan`/`transition` stays blocked by an OLD row from a prior session's `session_id` that this turn did not create and correctly cannot resolve (a distinct flaky test, a cross-repo change out of scope), dispatch `prd-defer {"id":"<row-id>","reason":"<why genuinely out of reach this session>"}` on that row -- see SPECIFY's "Inherited rows resume first" section. Writing a second PRD row that only describes being stuck on the first is drift, not progress: it adds an open item without resolving the one blocking the gate.

## Push and worktree-clean

`git_push` is the only admissible push surface, any repo, any cwd -- runs `[worktree-clean]` porcelain probe internally, refuses dirty. `git_finalize {message}` bundles add -> commit -> probe -> push. In a worktree another writer is also editing, commit only your files: `git_commit {message, paths:[...]}` / `git_finalize {message, paths:[...]}` stage and commit exactly those pathspecs, the porcelain probe checks only them, and the push goes by explicit ref when dirt remains outside them; `git_push {rev:"HEAD"}` is the sanctioned push of an already-made commit over someone else's dirt. Sibling push: `git_push {repo:"<abs>", branch:"<branch>"}`. Raw `git` shell body gated `deviation.bash-git-bypass`. A dirty tree at this stage is yours to resolve now: commit real work, revert junk, or fold transient emission into the managed gitignore block -- never carry it forward as "pre-existing."

## CI

Verification is thinking run rather than reasoned: "is this correct?" is executed, not argued -- real test, real matrix, real page answer it. The push IS the validation dispatch. Local proof covers one platform; matrix covers all. On green, `fs_write` `.gm/exec-spool/.ci-validated` with `{"head_sha":"<git rev-parse HEAD>"}` -- the COMPLETE gate matches that sha against current HEAD. Red = divergent observation holding the trajectory until cause-named and green re-pushed; toolchain skew converges, does not stop. A CI check skipped because "the diff looked safe" is an unwitnessed slice.

## Residual-scan

`residual-scan` is dispatched BEFORE `transition to=COMPLETE` -- the gate refuses without its fired marker, and the denial names `residual-scan` as the next dispatch. It examines the open surface -- PRD pending, running tasks, dirty tree, untracked artifacts -- non-empty = non-convergent -> expand PRD with the reachable in-spirit residual, re-execute. One-shot per stop window via marker.

Before accepting an empty scan, re-apply "every possible" to the closing PRD: every resolved row's skipped variant, every touched adjacent surface, every validation proving a row in practice not claim -- each hit is `prd-add` + re-execution. Clean scan on a short PRD for a long-horizon prompt is a false negative.

**Every `git status --porcelain` entry triaged this turn -- "pre-existing" is not a stop excuse.** Dirty worktree: commit (real work), managed-gitignore-block it (transient runtime emission), or revert (junk). `.gm/disciplines/` tracked; new memorize-fire `mem-*.json` committed.

## Decisive commitment

Re-read every new `.md`/`.txt`/comment-bearing file the diff touched: no hedge ('we should probably', 'for now', 'as a stopgap', 'out of scope for this'), no infinite option listing in place of a recommendation, no rationalized abandonment of a row that was actually completable. The `no-hedge-language-in-diff` gate catches the common phrases; this sweep catches the shape the phrase-list misses. Commitment: Committed(c) and Recommendation(c) for every c, or the decision is not made and the chain stays here.

## Trace to a human outcome

Before accepting the slice convergent, trace every shipped change to a human outcome -- capability gained, wait removed, failure no longer hit, a developer the interface stops fighting. Impact chain ending in technical elegance with no reachable human = aesthetics, revert candidate.

## Completion

Chain enters COMPLETE only when your `transition` returns COMPLETE phase; on-disk state moves only on `transition`. **Done is plugkit's pronouncement, not yours** -- gate-allowance is not done, only a dispatched `transition` returning COMPLETE is; a narrated walk with the gate open or the verb un-dispatched is fabrication. Not-COMPLETE means a next transition exists; idle/"waiting for the user" mid-chain are deviations (closure authorized at request time).

**No summary, no prose-only turn here.** A summary, recap, announced-but-undispatched next move, or any tool-less message IS a stop. Until this surface returns phase=COMPLETE after `transition`, every turn ends in a verb (`phase-status`, `residual-scan`, the push verbs, `instruction`, `transition`). Catching yourself composing a summary IS the drift signal -> dispatch `phase-status` instead.

## Feedback

DECIDE's findings flow back into specification -- the graph's DECIDE -> SPECIFY edge is the empirical fitness loop: tool diagnostics, strategy refinements, and every witnessed gap between spec and reality become `prd-add` rows at SPECIFY, never lessons held in prose. A chain that learned something and did not route it back has not finished deciding.

## Dispatch

`transition` to COMPLETE only when the closure gate set is fully true; the handler hard-rejects while any open mutable or PRD item remains. Any gate false: stay in DECIDE, dispatch the recovery verb the gate names (`git_finalize`, `residual-scan`, `claim-audit`, or the CI-watching verb), never retry the bare transition.
