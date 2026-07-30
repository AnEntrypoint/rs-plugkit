# CONSOLIDATE

YOU are the state machine. CONSOLIDATE sits between VERIFY and COMPLETE -- the closing phase where the witnessed slice becomes durable: git consolidation, then CI/CD validation.

L3 landing. Entry precondition (checked on `transition to=CONSOLIDATE`): mutables resolved, PRD work-rows done, residual-scan fired. Exit precondition (checked on `transition to=COMPLETE`): worktree-clean, remote-pushed, CI/CD validated.

## Git consolidation

Stage, commit, push -- via the git verbs, never a shell git. `git_finalize {message}` bundles add -> commit -> porcelain-gate -> push in one dispatch; prefer it. `git_push {repo, branch}` for a sibling repo. A dirty tree at this phase is yours to resolve now: commit real work, revert junk, or fold transient emission into the managed gitignore block -- never carry it forward as "pre-existing."

## CI/CD validation

The push IS part of the validation dispatch, but CONSOLIDATE also witnesses the pipeline going green, not just the push landing. Dispatch `ci-status {repo, sha}` with the repo and SHA just pushed. Poll it until `status` is `success` or `failure`, not still pending. On `success`, `fs_write` `.gm/exec-spool/.ci-validated` with `{"head_sha":"<git rev-parse HEAD>"}` -- the COMPLETE gate matches that sha against current HEAD; any other content, a stale sha, or a marker written before the final push reads as unvalidated. On `failure`, read the response's `failed_jobs` array and `prd-add` one row per distinct failure, naming the job and the cause from that structured data -- never a vague "CI failed" row. Fix, re-push, re-dispatch `ci-status`. A CI check skipped because "the diff looked safe" is an unwitnessed slice.

## Constraints


## Dispatch

`transition to=COMPLETE` only once worktree-clean + remote-pushed + `.ci-validated` fresh all hold. Any false: stay in CONSOLIDATE, dispatch the recovery verb the gate names (`git_finalize`, `residual-scan`, or the CI-watching verb), never retry the bare transition.
