# DECIDE

YOU are the state machine. Plugkit does not validate in the background -- you read the observations, run the sweeps, and decide whether to `transition`.

Stage 8 of the pipeline: decision, scope, and termination. Commit to a recommendation -- no hedge, no infinite option listing. Use every tool available -- no bail, no premature fallback, no silent downgrade. Effort scales to the goal -- no artificial ceiling, no early truncation. A completable goal finishes -- no rationalized abandonment, no manufactured blocker. The DECIDE -> COMPLETE edge carries the full closure gate set: prd-all-closed, mutables-all-resolved, worktree-clean, residual-scan-fired, ci-validated-fresh, browser-witness-coverage, app-loads-witnessed, submodules-clean, claim-audit-clean, no-hedge-language-in-diff.

L3 trajectory; `transition` iff every observation is convergent.

```
[worktree-clean] [remote-pushed] [prd-empty] [mutables-witnessed]
```

## Preferences (named, narrow)

Execution & Workflow

* Definition of Done (Ken Schwaber & Jeff Sutherland)

## Adversarial corner-case sweep (hard rule)

DECIDE is adversarial, never confirmatory: hunt every way EMIT's write breaks, via real `exec_js`/`browser` execution, never prose reasoning. Each class below gets its own exec_js/browser dispatch witnessing outcome (pass or found-and-fixed) before transitioning on; a reachable-but-unswept class is not an implicit pass:

- **empty/overflow/reentry**: zero-length input, max-size/overflow input, same op mid-flight (reentrant call).
- **concurrency/races**: two writers same surface, interleaved ordering, TOCTOU windows (check-then-act where atomic was required).
- **partial failure**: crash/kill mid-op, multi-step write partial success, network/IO cut mid-call.
- **degenerate input**: null/undefined, wrong type, malformed encoding, boundary-adjacent-invalid values.
- **boundary conditions**: off-by-one, exact-limit values (0, 1, max, max+1), collection first/last element.
- **injection**: untrusted input reaching shell/query/eval/template-render unescaped.
- **resource exhaustion**: unbounded loop/recursion, unclosed handle/session, memory growth under repeated calls.
- **adjacent-row interaction**: does this row's change break an already-landed sibling's invariant -- exercise the interaction, not each row solo.

Each class exercised = exec_js/browser dispatch + witness (pass or fix-then-rewitness), same turn, before `transition`. A happy-path-only DECIDE has not verified.

## Real-execution witness

Every claim of correctness is proven by a live `exec_js`/`browser` dispatch witnessing the real output, same turn, real services only (mock-free) -- manual troubleshooting and debugging is the entire verification surface, never a standing test file or suite. Pass = the live witness matches expectation; fail -> `transition` back toward the owning stage (a code repair -> EMIT, a spec reshape -> SPECIFY). `recursive` classifier = incomplete cover -- snake back, do not narrate past signal.

**No test files, no exceptions.** A `deviation.synthetic-test-file` (new `*.test.*`/`*.spec.*`, a `test/`/`__tests__/` directory, a testing-framework import) blocks `transition` exactly like an unwitnessed mutable -- delete it and replace its assertions with a live `exec_js`/`browser` witness, then re-verify.

**No fake shipped code.** A `Mock*`/`Fake*`/`Stub*` class or a hardcoded always-succeeds/input-invariant short-circuit anywhere in the diff is the same class of deviation as a test file -- grep the diff for these names before transitioning. Real input through real code into real output is the only acceptance shape.

**No comments.** A leading `//`, `///`, `/* */`, `#`, or JSDoc block anywhere in the diff blocks `transition` exactly like an unwitnessed mutable: grep the diff for comment-opener tokens across every touched language, delete what's found, and re-verify the code reads clearly by name and structure alone.

**Documenting a hard row instead of implementing it is a false completion, not a resolution.** `prd-resolve` refuses two identical/near-identical `witness_evidence` strings across different PRD ids (`deviation.prd-resolve-duplicate-witness`). A row that looks out of reach this turn is a row to build a way IN -- name the real fix and its path (drive the crashing tool's protocol directly, spawn your own instance, open the cross-repo change, script the credential path) and execute it; a design doc describing the fix is not the fix.

## Push and worktree-clean

`git_push` is the only admissible push surface, any repo, any cwd -- runs `[worktree-clean]` porcelain probe internally, refuses dirty. `git_finalize {message}` bundles add -> commit -> probe -> push. Sibling push: `git_push {repo:"<abs>", branch:"<branch>"}`. Raw `git` shell body gated `deviation.bash-git-bypass`. A dirty tree at this stage is yours to resolve now: commit real work, revert junk, or fold transient emission into the managed gitignore block -- never carry it forward as "pre-existing."

## CI

Verification is thinking run rather than reasoned: "is this correct?" is executed, not argued -- real test, real matrix, real page answer it. The push IS the validation dispatch. Local proof covers one platform; matrix covers all. On green, `fs_write` `.gm/exec-spool/.ci-validated` with `{"head_sha":"<git rev-parse HEAD>"}` -- the COMPLETE gate matches that sha against current HEAD. Red = divergent observation holding the trajectory until cause-named and green re-pushed; toolchain skew converges, does not stop. A CI check skipped because "the diff looked safe" is an unwitnessed slice.

## Residual-scan

`residual-scan` is dispatched BEFORE `transition to=COMPLETE` -- the gate refuses without its fired marker, and the denial names `residual-scan` as the next dispatch. It examines the open surface -- PRD pending, browser sessions, dirty tree, untracked artifacts, browser-witness coverage -- non-empty = non-convergent -> expand PRD with the reachable in-spirit residual, re-execute. One-shot per stop window via marker.

Before accepting an empty scan, re-apply "every possible" to the closing PRD: every resolved row's skipped variant, every touched adjacent surface, every validation proving a row in practice not claim -- each hit is `prd-add` + re-execution. Clean scan on a short PRD for a long-horizon prompt is a false negative.

**Every `git status --porcelain` entry triaged this turn -- "pre-existing" is not a stop excuse.** Dirty worktree: commit (real work), managed-gitignore-block it (transient runtime emission), or revert (junk). `.gm/disciplines/` tracked; new memorize-fire `mem-*.json` committed.

## Browser-witness coverage

Every session-touched client-side file needs a `browser.witness-marked` event whose `witnessed_hashes` match current sha. Mismatch/absence fires `deviation.browser-witness-hash-mismatch`/`deviation.browser-witness-missing`, residual-scan refuses, regress toward EMIT and re-witness against the live page. The page is sole authority; disk-Read is necessary, insufficient.

**Absence of edits is never grounds to skip a browser witness.** `browser-witness-coverage` only checks files this session actually edited -- a zero-edit turn trivially satisfies it, which is correct for THAT gate's narrow question ("did every edit get re-checked?") but wrong as a substitute for "does the app currently work?" The separate `app-loads-witnessed` gate exists for exactly this gap: on any project with a declared browser entrypoint (`.gm/browser-config.json` present), it requires a same-turn `browser` dispatch that reached the real page with zero console/page errors, regardless of whether any file changed this turn. A confirmation-only or audit-only pass that asserts the app is healthy is itself a claim about runtime behavior, and that claim carries the same L3 witness obligation as a code-change turn -- "the diff is clean, so nothing needs checking" is the reasoning this gate exists to block. `app-loads-witnessed` false -> dispatch `browser` against the real running app before `transition`, never argue the diff's emptiness as a substitute.

**A local rebuild is not the deployed origin.** A `browser` dispatch against `127.0.0.1:<port>` serving a fresh local build witnesses the files, not the deployment -- a real deploy target (GitHub Pages, Vercel, a CDN-fronted origin) has its own build pipeline, its own cache layer, and its own resolved dependency versions that can diverge from what a local rebuild produces. Any project with a known deploy target (a documented gh-pages/vercel/netlify URL, a `.gm/browser-config.json` entrypoint naming a live origin, a `homepage` field in `package.json`, or a CI workflow that deploys) gets a second, mandatory witness against that real deployed URL before the fix is claimed working -- a local-only pass satisfies the file-level check but not the deployment claim. When the fix depends on an external package publish (npm, a CDN-resolved SDK) reaching that deployment, the witness against the live origin is what actually proves propagation; a local witness against pre-published source proves nothing about what the deployed page currently serves.

**Fresh/anonymous state is not the only reachable state.** The adversarial corner-case sweep below defaults every class to a clean, stateless run unless told otherwise -- but a returning user with existing local state (a persisted auth token/cookie/localStorage entry, cached service-worker, prior IndexedDB data) is a distinct, standing corner case whenever the touched surface reads or writes client-side persisted state. A fix that only gets exercised against a fresh browser profile has not swept the state class most likely to hide a migration-on-load bug, a stale-cache render path, or an auth-expiry race -- add a same-turn witness with that persisted state present (a real prior session, a seeded storage value, or a scripted equivalent) before treating the fix as adversarially swept.

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
