# rs-plugkit

The wasm cdylib guest behind gm (`plugkit-core`), built to `plugkit.wasm`
(fat, with baked-in bert embedding weights) and `plugkit-slim.wasm` (no
weights, used whenever a real embed answerer exists out-of-wasm). Both ship
from `AnEntrypoint/plugkit-bin` and are consumed exclusively by
`agentplug-runner` (repo `AnEntrypoint/agentplug`), the sole host that loads
this guest. There is no standalone `plugkit.exe` CLI anymore, no private
self-update path, and no direct-loader fallback — the retired `gm-runner`
native host and the retired JS wasm-host (`plugkit-wasm-wrapper.js`) both
routed through code paths this crate no longer ships. (The `rs-exec` crate
is also retired and archived; this crate has never depended on it.)

## Architecture

`plugkit-core` exposes a single wasm entry point that agentplug-runner calls
per spool dispatch. The guest routes shared capabilities (`host_plugin_call`,
`host_vec_embed`) to sibling wasm plugins (`bert`, `libsql`, `treesitter`)
that agentplug-runner loads alongside it; background task management is
native in `agentplug-host`, not implemented in this crate at all.

State lives on disk under a project's `.gm/` directory: `prd.yml`,
`mutables.yml`, `exec-spool/{in,out}/`, `gm.db`, `disciplines/<ns>/`,
`code-search/`.

## Spool dispatch ABI

`dream-replay-cycle` accepts canonical `session_id`, optional opaque `cycle_id` and
`after_dispatch_id`. The owner is a nonempty ASCII path component of at most 256
bytes containing letters, digits, dots, underscores or hyphens, but not `.` or `..`.
Supplied optional IDs must be nonempty strings, bounded to
128 and 512 bytes respectively; malformed input fails. It summarizes up to eight
recent recorded observations using
same-owner completed dispatch-ledger evidence; it does not execute tools, evaluate
sealed worlds, register policies or deploy strategies. A successful new summary
returns `kind: "observations"`, `status: "replayed"`, `owner_session_id`, the echoed
`cycle_id`, `last_dispatch_id` and `replay`. Missing verified evidence or an unchanged
verified dispatch returns `status: "deferred"` with a reason and no acknowledgment
ID. Ordinary pending-pipeline gates remain authoritative. This maintenance verb
does not refresh agent phase-prose timestamps.

Callers write request JSON to `.gm/exec-spool/in/<verb>/<N>.txt` (or
`in/<lang>/<N>.<ext>` for language-execution stems); the watcher processes on
read and writes `out/<N>.json` (metadata) alongside `out/<N>.out`/`.err` for
process-execution verbs.

`codesearch` is the canonical code-search verb. `code_search` and `search` are accepted
compatibility alias with the same request body and response behavior.

Orchestrator verbs: `instruction`, `transition`, `transition-revert`,
`discipline-check-removal`, `discipline-audit`, `memory-namespace-audit`,
`codeinsight-namespace-audit`, `calculus-model-check`, `phase-status`,
`mutable-resolve`, `memorize-fire`, `residual-scan`, `auto-recall`,
`component-loader-reconcile`, `component-loader-hmr`.

`component-loader-reconcile`/`component-loader-hmr`
(`orchestrator/component_loader.rs`, `orchestrator/component_loader_dispatch.rs`)
implement the Cordis paper's Section 5.2 Component Loader on top of the same
kind-agnostic `fiber_lifecycle`/`coeffect_realm` machinery `discipline_note.rs`
already uses -- `ComponentEntry` is Definition 74's entry record (`id`, `url`,
`isolate`, `intercept`, `config`, `disabled`), and `component-loader-reconcile`
(`{previous: [entry...], next: [entry...]}`) diffs two entry lists by `id` and
dispatches the least-disruptive operation per Section 5.2.1's own bullet list
(`id`/`url` change -> rebuild; `isolate` change -> Algorithm 7's
`patch_isolation` realm reassignment; `config` change -> apply; `disabled`
toggle -> unload/reload), reporting each entry's `ReconcileOp` plus, for a
realm reassignment, the per-key diff (`old_realm`, `new_realm`, fresh
`entry_tag`) and the notified-dependent set Algorithm 7's own `notify` line
computes. `component-loader-hmr` (`{stashed, externals, entries, graph,
current_sources, next_sources, fail_urls?}`) runs the full three-phase HMR
engine: Algorithm 8 `classify` (accepted/declined fixed point over the
caller-supplied import graph, an unresolved import cycle defaulting to
declined per the algorithm's own line 21), Algorithm 9 `detect` (walks each
entry's dependency tree via `get_dependencies`, respecting the declined
boundary, folding a stale entry's tree back into `accepted` as it goes), and
Algorithm 10 `reload` (invalidates+backs-up the accepted modules, disposes and
reinstantiates each stale entry, and on ANY import failure unconditionally
rebuilds every stale entry from `backup` before returning the error -- the
paper's own transactional guarantee that the system never observes a
half-reloaded state). `fail_urls` lets a caller deliberately simulate an
import failure to witness the rollback path in a live dispatch, matching this
crate's own no-test-file convention (see `AGENTS.md`'s "Parser-shaped
surfaces need adversarial input" section) for a surface with no standing test
harness.

`transition-revert` pops the most recent entry off `TurnState.phase_history`
(a LIFO accumulator every `transition` call appends to) and restores the
phase it recorded, one step at a time -- the accumulator/inverse pairing a
Cordis-style revertible effect gives a state transformation, applied to gm's
own phase graph so a feedback-edge re-entry (e.g. DECIDE->SPECIFY) can be
undone precisely rather than only via the separately-tracked
mutables.yml/prd.yml side state.

`discipline-check-removal` ({"discipline": name}) reports whether disabling
a discipline right now would break a dependent: `orchestrator/discipline_note.rs`'s
`removal_dependents` names every other enabled, requires-satisfied
discipline whose `requires` still resolves to the target's `provides`. This
is the withdrawal-ordering guard from the Cordis paper's spatial-composability
theorem (Section 4.3.1) -- a provider's removal should wait on its dependents
deactivating first; here it surfaces the dependents so the caller (a human
editing `enabled.txt`, or a future automated deactivation path) can defer
removal rather than silently breaking a satisfied requirement. `Component`
(same file) reads a discipline as the paper's (d, p, e) triple -- requires,
provides, and whether its policy.md carries a non-empty effect -- so the
requires/provides/policy files are read through one canonical struct instead
of three independently-read files that happen to correspond.

Each discipline also carries a persisted `.gm/disciplines/<name>/fiber-state.json`
(`FiberLifecycle`: `Inactive`/`Active`/`Unloading`), advanced by exactly one
transition per `active_policies()` call via `advance_fiber`. This is the
paper's fiber lifecycle (Section 4.3, Definition 49) reduced to gm's
substrate, which has no async load step: `Inactive -> Active` on first
satisfaction, `Active -> Unloading` the instant satisfaction is lost
(L-Leave -- the fiber stops providing but is still named as present-but-
leaving), `Unloading -> Inactive` on the following dispatch (L-Unload --
withdrawal completes). `removal_dependents` only counts a discipline whose
own fiber has reached `Active` as a real dependent, so the withdrawal
guard is grounded in actually-observed activation state rather than a
one-shot recompute of whether requires happens to be satisfiable.

`discipline-audit` runs the paper's core metatheory (Section 4.4) as live
checks against the current discipline set rather than leaving them as
static proof: preservation (Theorem 59 -- no two `Active` disciplines
share a `provides` capability), recovery exactness (Theorem 61 -- an
`Unloading` fiber reaches and stays at `Inactive`), ordering (Theorem 63
-- `removal_dependents` is empty for any non-`Active` fiber), and progress
(Theorem 66 -- `advance_fiber`'s transition table is total, checked
structurally by the compiler). It also runs `dangling_requires`, the
paper's access-control discipline (Section 6.3, adapted from
`UNDECLARED_ACCESS`) applied to `requires.json`: a `requires` entry naming
a capability no known discipline (enabled or not) ever `provides` is
flagged as dangling, distinguishing a typo/stale reference from a live
dependency merely waiting on its provider being enabled.

`orchestrator/fiber_lifecycle.rs` factors `FiberLifecycle`/`advance_fiber`/
`read_fiber_state` out of `discipline_note.rs` into a kind-agnostic module:
any component family with a name and a state-file path can call these same
two functions rather than reimplementing the transition table, so
disciplines are the first caller and not a special case the module was
written around. The same file's `ActiveFiberSet` brings preservation
(Theorem 59) into the type system rather than checking it afterward: its
only public constructor refuses a colliding insert outright, so a value of
`ActiveFiberSet` can only ever hold a preservation-satisfying set --
`audit_preservation` reports exactly the inserts that were refused, rather
than running a separate scan alongside a set that could already be wrong.

A discipline's `requires.json` also accepts a `realm` field (paper Section
3.2.3, Definition 28-29's coeffect isolation): two disciplines in
different realms providing the same bare capability name do not satisfy
each other's `requires` and do not collide for preservation purposes --
real multi-tenant isolation at the discipline-name grain, without a
security sandbox. `requires_satisfied`/`removal_dependents`/
`audit_preservation` all resolve within the consumer's own realm; a
`requires.json` with no `realm` field resolves in the default (empty
string) realm, matching every discipline written before isolation existed.

`requires_satisfied` checks a candidate provider's own fiber state
(`Active`, not merely enabled) before counting its `provides` as
satisfying a dependency -- the other half of Theorem 63's ordering
guarantee: a provider mid-withdrawal (`Unloading`) must not be read as
still available, even though it stays nameable by `removal_dependents`
for one more dispatch. `active_policies` computes every discipline's
`target_satisfied` in one snapshot pass BEFORE advancing any fiber, then
advances all of them in a second pass -- a single combined loop would let
an earlier discipline's own `advance_fiber` call mutate its
`fiber-state.json` mid-iteration, so a later discipline's
`requires_satisfied` check could see a provider as already `Unloading`
even though it was `Active` when the dispatch began, incorrectly forcing
an unrelated dependent into withdrawal it should not have entered for
another full dispatch. Live-verified via a standalone rustc trace modeling
both the single-pass (buggy, forces the dependent to `Unloading`
prematurely) and two-phase (correct, dependent stays `Active`) orderings.

Recovery exactness (Theorem 61) and ordering (Theorem 63) are also
brought into the type system, alongside preservation's `ActiveFiberSet`:
`verify_recovery_exactness` checks the pure `transition` table exhaustively
over both reachable targets without mutating anything, used by
`discipline-audit`. `fiber_lifecycle::SafeToWithdraw::check`
is the only way to obtain proof a component has no dependents, taking the
caller's own coeffect-resolved dependent set as a parameter (kept
kind-agnostic by NOT recomputing it) -- any future code path that
actually deletes a component's storage can require a `SafeToWithdraw` as
its own parameter type, making an out-of-order deletion a compile error
for that path rather than a runtime check a caller could skip.
`discipline-audit`'s ordering check now obtains this same proof object
rather than a parallel hand-rolled equivalent.

`orchestrator/memory_component.rs` is a THIRD component kind
instantiated using `fiber_lifecycle`'s existing public API
(`read_fiber_state`/`advance_fiber`/`ActiveFiberSet`) with zero new
infrastructure added to `fiber_lifecycle.rs` -- gm's memory-namespace
subsystem (`.gm/memories/`, `memory_md.rs`) read as a Cordis component,
independently of disciplines and sibling wasm plugins. A namespace's
coeffect specification is an optional `.gm/memories-manifest/<ns>.json`'s
`depends_on` array (other namespaces whose content it assumes exists);
its provision is always its own name. `memory-namespace-audit` advances
every known namespace's fiber and reports which reached `Active`,
exercising the same machinery `discipline-audit` exercises for
disciplines, proving the module serves a caller nobody anticipated when
it was written.

`orchestrator/codeinsight_component.rs` is a FOURTH component kind,
chosen adversarially rather than to confirm the pattern already worked:
its substance (`code_index.rs`'s codeinsight/manifest/vec namespaces)
lives in a libsql database (`shared_db.rs`), not a filesystem tree like
disciplines/memory-namespaces or a wasm binary like sibling plugins. It
still fits `fiber_lifecycle`'s existing public API with zero changes to
that file (confirmed by `git diff --stat` showing no modification to
`fiber_lifecycle.rs` across this instantiation) -- a component's
fiber-state is always a small filesystem sidecar fact entirely separate
from where its substance actually lives, which is why the storage
backend of a component's own data is invisible to `fiber_lifecycle` by
construction. `codeinsight-namespace-audit` is the dispatchable entry
point.

`fiber_lifecycle::check_confluence` brings Theorem 73 (confluence) into a
live check rather than unproven prose: given a fixed set of initial fiber
states and pre-computed targets, it runs the same transitions once in
the given order and once reversed, from the same starting states, and
asserts the two runs reach the same `Active` set. `discipline-audit`'s
`audit_confluence` calls this over the live discipline set every audit.
Confluence holds structurally here because each fiber's transition
depends only on its own current state and its own target, never on
another fiber's -- live-verified via a standalone rustc trace including
a case with a repeated target on one fiber, confirming the check is a
real (non-vacuous) test rather than one that always trivially passes.

`orchestrator/calculus.rs` is a direct, gm-independent implementation of
the paper's Section 4.2 base calculus: an abstract `Registry` of named
`Fiber`s (no discipline/plugin/namespace concept anywhere in this file),
advanced by the five base rules (O-Insert/O-Retire/O-Remove/L-Reload/
L-Unload) as literal functions matching the paper's own premises. Every
other metatheory check in this crate runs over ONE gm-specific
instantiation at its CURRENT state; `calculus-model-check` instead
exhaustively enumerates EVERY state reachable from an empty registry
under a small bounded 3-fiber/2-capability configuration (a satisfiable
provider/dependent pair plus a fiber whose `requires` can never be
satisfied) and checks preservation and progress hold for the WHOLE
reachable state space, not sampled states. Live-executed as a standalone
native binary during development: 75 distinct reachable states were
enumerated and preservation/progress held for every one; a negative test
confirmed `well_formed`'s disjointness check actually detects a
constructed collision rather than the model being unable to reach a bad
state in the first place (which would make the positive result
vacuous). This is the direct model-check the paper's Section 4.4
metatheory describes, bounded rather than a full unbounded proof (which
would need a proof assistant, not a Rust crate), but real and executed
rather than asserted in a doc comment.

Wasm-direct verbs: `fs_read`/`fs_write`/`fs_stat`/`fs_readdir` (the three read
verbs accept `{"allowOutsideRoot": true}` to address an absolute path outside
the project root -- per call, never inferred, `".."` still refused; `fs_write`
ignores it and stays project-only), `scan_deps`
(supply-chain scan for the HiddenSpawn-class obfuscated-dropper pattern:
size/line-ratio disproportion + dense `\uXXXX`-escape-run detection across
git-tracked source and a bounded `node_modules` walk), `kv`/`kv_get`/
`kv_put`/`kv_delete`, `exec`/`exec_js`, `fetch`, `env_get`, `recall`,
`codesearch`, `codeinsight` (symbol, call, import, complexity and duplicate queries; `callers`/`callees`/`impact` are aliases), `memorize`/`memorize-prune`, `health`, `filter`, the full git
verb family (`git_status`, `git_log`, `git_diff`, `git_show`, `git_branch`, `git_remote`, `git_add`,
`git_commit`, `git_amend`, `git_finalize`, `git_push`, `git_checkout`, `git_merge`, `git_cherry_pick`, `git_fetch`,
`git_pull`, `git_stash`, `git_stash_pop`, `git_stash_drop`, `git_stash_list`, `git_init`,
`git_worktree`, `git_worktree_add`, `git_worktree_list`, `git_worktree_remove`, `git_worktree_prune`,
`git_rm`, `git_revert`, `git_reset`, `git_poll`), plus `ci-status` (real
GitHub Actions workflow-run query), `prd-add`/`prd-list`/`prd-resolve`/
`prd-status`, `mutable-add`/`mutable-list`, `discipline-note`, `fsm-vendor`,
`fsm-validate`, `fsm-propose-override`, `submodule-check`, `sql_open`/`sql_query`/`sql_exec`/`sql_list_dbs`/
`sql_smoke`, `task-spawn`/`task-list`/`task-output`/`task-stop`,
`background-convert`, `kill-port`, `similarity`, `claim-audit`.

`crawl` takes a plain-text body (not JSON). Its first line is `engine=cdp` (the default when the
line is absent) or `engine=lightpanda`; the rest is the crawl request (URLs and steps). `cdp` is
HEADFUL: the host drives a visible Chrome over CDP through its `crawl_cdp` entry point. `lightpanda`
is HEADLESS: the guest calls the `lightpanda` sibling plugin's `crawl` verb with the same body, and
the host keeps that lightpanda process warm. Both engines return the host's reply object unchanged,
with `ok`, `engine`, `headless`, `stdout`, `stderr`, `exit_code` and `duration_ms`. Any other engine
answers `ok: false` with `error_code: unknown_engine`, naming `cdp` and `lightpanda`.

```
engine=lightpanda
https://example.com/
```

The standalone `cdp` verb is removed; `crawl engine=cdp` replaces it. `browser` and `serp` are
removed too and answer `unknown_verb`.

When a shell verb returns a `task_id`, use `task-output` with JSON body
`{"id":"task-…"}` to retrieve it. Use `task-stop` with the same body to stop
it. These are GM verb dispatches, not shell commands.

`task-list {}` reports status without persisting results. The explicit
`{"prepare_handoff":true}` diagnostic runs the host's handoff preparation and
returns `handoff_status`, but transfers no ownership. It may write completed
result snapshots. Active execution, children or output drains defer preparation;
invalid or full storage refuses it without closing execution admission.
Preserved results last 30 minutes after child exit and retain at most 64 KiB
per stream. `task-output` reports omitted-byte counts, including a tighter
`max_bytes` request; `task-stop` removes the retained result. These guarantees
require a host with durable task-result support.

`codeinsight` reads current source and refreshes its structural index before answering,
without generating embeddings. Content hashes invalidate cached symbols even when file
size and modification time are unchanged. Successful replies include `codeinsight_index`
coverage for index-eligible files. Failed listings or stats, capped or budget-limited work,
unreadable or oversized files, failed parses, and failed stores reject the query instead of
presenting cached graph evidence as current. Use `action: "sync"`
for a longer refresh before retrying a budget-limited query.

An explicit `root` or `projectPath` requests access through the host before structural
reads. Existing readable directories keep their current access. External roots must
be existing project directories recognized by the host; a linked checkout's `.git`
file counts as a project marker. This applies to `codeinsight_index` too.
Relative roots anchor their database path at the dispatch project. Database opens
create missing parent directories only beneath a host-readable ancestor.

Readable empty files and supported files with long lines remain eligible for structural indexing.
The host file-read ABI returns `0` on failure, reserved packed value `1` for successful empty UTF-8
reads without allocation, and a pointer/length for nonempty reads. Empty-read success requires
both a supporting host and guest; older components continue reporting that coverage incomplete.

Call edges use unqualified names, not resolved definition bindings. `callers`, `callees`,
`impact` and `tests` reject path, file, glob and line scope fields. Use path-scoped
`outline`/`find` and `codesearch` to distinguish same-named definitions and exact call
sites. Caller and impact replies label this resolution model and definition ambiguity;
`through_ambiguous` remains an explicit impact traversal opt-in.

Close a completed PRD row with `prd-resolve` and JSON body
`{"id":"<prd-item-id>","witness_evidence":"<live evidence>"}` plus a witness
binding: either `witness_dispatch_id` (the `dispatch_id` of a gm dispatch in this
project's ledger, which keeps the 500 most recent dispatches), or all of
`witness_exit_code` (must be 0), `witness_output_sha256` (sha256 of the output
file), `witness_output_path` (that file, relative to the project root) and
`witness_ts` (RFC 3339). A binding is re-hashed against its file and refused on
any mismatch, and a row closed by binding records `witness_binding`. A dispatch id
that appears only inside `witness_evidence` is named in the reply as
`witness_dispatch_id_in_evidence` with `witness_dispatch_id_field`; it verifies only
when passed as `witness_dispatch_id`. An optional `commit_comment` attaches a
one-line resolution note to the next GM commit. `prd-list` takes `limit`, folds
repeated blocks of an id to the last block, and replies with `total` (matches) and
`store_total` (stored blocks).

`git_finalize` bundles add -> commit -> porcelain-gate -> push in one
dispatch, then runs `ci-status` inline against the pushed commit's SHA: on a
green result it writes `.gm/exec-spool/.ci-validated` (`{"head_sha": "..."}`)
automatically, so a caller does not need a separate CI-poll-then-marker-write
round trip. On a non-green or unresolvable result, the response's
`next_dispatch` field names `ci-status` so the caller can re-check once CI
finishes.

`git_worktree` accepts one action per dispatch:

- `{"action":"list"}` returns structured `worktrees` from Git's NUL-delimited porcelain records.
- `{"action":"add","path":"/absolute/checkout","ref":"HEAD","detach":true}` creates a linked checkout. `ref` defaults to `HEAD`; `detach` defaults to `true`. `detach:false` requires an explicit existing branch.
- `{"action":"remove","path":"/absolute/checkout"}` removes a clean, unlocked linked checkout. The verb provides no force option; Git rejects dirty or locked worktrees.

Unknown fields are refused for each action. Repository selectors and session fields retain the git-family contract. Relative paths resolve against the selected repository. Add and remove return the requested path; list returns Git's recorded paths.

When enabling `extensions.worktreeConfig`, move a common `core.worktree` value into the primary repository's `config.worktree` before adding per-worktree configuration. Move `core.bare` there too if it is true. Keep the primary worktree value's path semantics unchanged. A common `core.worktree` under this extension applies to linked worktrees and can make `git rev-parse --show-toplevel` report an administrative gitdir. Verify that command in both primary and linked checkouts before dispatching further GM verbs. Per-worktree author configuration then uses `git config --worktree user.name` and `user.email`.

`git_commit` consumes the whole index in a merge.
The verb refuses `paths` or `files` or `add_all`.
These requests do not stage files.
Review the staged index before the commit.
It does not add a pathspec to the commit.
Unstaged and untracked files are not in the commit.

`git_add`, `git_commit`, `git_finalize`, `git_diff`, `git_stash` and
`git_checkout` take an optional `paths` (alias `files`) pathspec list
(`git_checkout {paths, ref?}` restores just those files in the working tree from `ref`, default the index; it refuses an empty list, a leading `-` or `:`, `..`, an absolute path outside the repo, and anything under `.gm/` or `.agentplug*` except `.gm/.last-scan-deps-ts`, `.gm/.last-scan-deps-result.json`, and `.agentplug-kv/codeinsight-edges/`, and answers `{restored, source, output}`). With it, `git_commit` and
`git_finalize` stage and commit exactly those pathspecs (`git commit -- <paths>`,
so entries another writer staged stay staged and uncommitted), the
post-commit porcelain gate considers only those paths, and when dirt remains
outside them `git_finalize` pushes by explicit ref (`rev: "HEAD"`) instead of
refusing. `git_push {rev: "HEAD"}` publishes an existing commit over a dirty
worktree without touching it; it never rebases, so a remote that moved is
reported rather than reconciled. `paths` together with `rev` on
`git_finalize` is refused, as is an empty or non-string `paths`. Resolved-PRD
`commit_comment` notes are bundled only by a commit whose scope covers
`.gm/prd.yml` (no `paths`, or `paths` naming it or `.gm`); a narrower scoped
    commit leaves them pending so the row removal and its note land together.

    `git_cherry_pick {rev, cwd?|repo?|root?|projectPath?}` first requires a clean
    worktree, applies one existing commit without merging unrelated history, and
    returns both before and after HEAD values. A conflict lists its paths and is
    automatically aborted, so the target is not left in a cherry-pick state.

An explicit `paths` outranks the protected-path exclusion list: an entry the
caller named is never withheld, and a request whose every pathspec resolves
under `.agentplug*` is refused rather than silently widening. A scoped
`git_commit`/`git_finalize` whose paths stage nothing fails with
`error_code: invalid_args` and `requested_paths` instead of reporting
`nothing_to_commit`, so a pathspec that no-ops cannot fall through to
committing the rest of the index.

`git_commit {amend:true}` and the `git_amend` alias rewrite HEAD (`git commit
--amend`) with the same body fields; they refuse `pushed_commit_refused` when
HEAD is reachable from a `refs/remotes/` ref and `amend requires an existing HEAD
commit` on an unborn branch. A `paths` value that is not an array of strings is
refused, never silently narrowed. `nothing_to_commit` answers echo
`requested_paths`.

`git_commit` and `git_finalize` refuse before staging anything when the commit
would reference a file that exists on disk, is untracked, is not gitignored
and is not part of this commit (`error_code: dangling_reference`, one
`dangling_references` entry per offender naming `from`, `line`, `specifier`
and `target`, plus a `fixes` list). Specifiers are resolved the way the
runtime would: relative first, then bare against the nearest `package.json`
`exports` map (string, conditional object and `"./*"` wildcard) and npm
workspaces under `packages/*`. `allow_dangling: ["<target>"]` waives named
targets and `allow_dangling: true` waives all; either way the waived targets
come back in `dangling_waived`. Scanning skips `node_modules/`, `dist/`,
`vendor/`, `build/`, `coverage/`, `.git/`, `.gm/` and `.agentplug-kv/`, files
above 512 KiB, non-JS/TS/JSON extensions, and stops after 400 files.

An explicit `paths` outranks the protected-path exclusion list: an entry the
caller named is never withheld, and a request whose every pathspec resolves
under `.agentplug*` is refused rather than silently widening. A scoped
`git_commit`/`git_finalize` whose paths stage nothing fails with
`error_code: invalid_args` and `requested_paths` instead of reporting
`nothing_to_commit`, so a pathspec that no-ops cannot fall through to
committing the rest of the index.

`git_commit` and `git_finalize` refuse before staging anything when the commit
would reference a file that exists on disk, is untracked, is not gitignored
and is not part of this commit (`error_code: dangling_reference`, one
`dangling_references` entry per offender naming `from`, `line`, `specifier`
and `target`, plus a `fixes` list). Specifiers are resolved the way the
runtime would: relative first, then bare against the nearest `package.json`
`exports` map (string, conditional object and `"./*"` wildcard) and npm
workspaces under `packages/*`. `allow_dangling: ["<target>"]` waives named
targets and `allow_dangling: true` waives all; either way the waived targets
come back in `dangling_waived`. Scanning skips `node_modules/`, `dist/`,
`vendor/`, `build/`, `coverage/`, `.git/`, `.gm/` and `.agentplug-kv/`, files
above 512 KiB, non-JS/TS/JSON extensions, and stops after 400 files.

Async git hosts use a pending-token protocol shaped like `host_fetch`'s. A
host that cannot block the wasm call (a browser driving isomorphic-git on the
wasm's own thread) answers `host_git` with `{"pending": true, "token"}` and
parks the terminal `{"stdout", "stderr", "exit_code"}` JSON string in kv ns
`outbox` under that token. The async-aware git verbs (`git_status`, `git_add`,
`git_commit`, `git_log`, `git_diff`) return a pending envelope carrying the
token. The caller dispatches `git_poll {token}` until a non-pending envelope
comes back; that envelope is the verb's terminal result. `git_poll` consumes
the outbox entry, records the finished step in a kv-persisted plan (ns
`git_async`), and re-enters the parked verb, so compound verbs like
`git_commit` resume across dispatches with no caller-side state. Sync hosts
return terminal results inline from `host_git`, persist no plan, and see no
behavior change.

## FSM graph

The phase graph (SPECIFY -> PROVE -> EMIT -> STATE -> CONC -> SEC -> RES ->
DECIDE -> COMPLETE by default) is data, not hardcoded control flow — a
project's `.gm/instructions/fsm/graph.json` (written by `fsm-vendor`) can
define a different phase set or gate shape. The COMPLETE gate's
`ci-validated-fresh` check compares `.ci-validated`'s `head_sha` against the
current `git rev-parse HEAD`; a stale or missing marker denies the
transition and names `ci-status` as the next verb to dispatch.

## Prose resolution

Phase-specific behavioral prose (served by the `instruction` verb) resolves
through a three-tier chain per key: `.gm/instructions/<key>.md` (project
vendored override) -> a configured source repo synced into
`.gm/instructions-source-cache/` -> the compiled-in default under
`crates/plugkit-core/src/orchestrator/instructions/prose/*.md`
(`include_str!`'d at build). Editing the compiled default requires a push
to this repo and a cascade rebuild — it is never read live from this
checkout.

## Build

```bash
cargo build --release
```

Outputs `target/wasm32-wasip1/release/rs_plugkit.wasm`. The release workflow
publishes that artifact as `plugkit-slim.wasm`. Release artifacts for the wasm target are
produced by `.github/workflows/release.yml` on `git push` to `main`, and
published to `AnEntrypoint/plugkit-bin` as GitHub Releases assets,
sha256-verified alongside each resolved release tag. `agentplug-runner`
polls and installs from these GitHub Releases directly
(`agentplug-runner/src/download.rs`); there is no npm-publish step in this
pipeline, so the `plugkit-wasm` npm package is not kept current by CI and
should not be relied on as a live distribution channel.

### Isolated source verification

Keep each writer's checkout separate. Dependency compilation can reuse a target cache for the same Rust toolchain, target, profile, features and dependency lockfile. Cargo serializes builds in one target directory; avoid concurrent builds against that directory when their callers need to collect artifacts.

Hold a single-writer lease across the build and artifact copy. Select the cache with `CARGO_TARGET_DIR`, build from the isolated checkout, then copy the resulting WASM or native library to a session-owned artifact directory before releasing the lease. Each diagnostic process reads that immutable copy. A separate target directory per writer can instead start from a filesystem snapshot or reflink of a compatible dependency cache; it must retain its own final artifacts.

Record the checkout commit and uncommitted diff, Cargo lockfile digest, build command, Rust toolchain version and artifact SHA-256. Confirm source hashes are unchanged across the build and live diagnostic. Reuse dependencies only; rebuild changed source crates. A diagnostic artifact does not replace the installed runner or establish release provenance. Publish through the repository workflow and verify its artifact separately.

## Cascade

A push to `AnEntrypoint/{rs-codeinsight, rs-search, rs-plugkit}` triggers
`cascade.yml`, which lands here and builds the single `plugkit.wasm` +
`plugkit-slim.wasm` pair via `release.yml`. `agentplug` consumes that
release artifact as a separate downstream pipeline, decoupled at the
`plugkit-bin` release boundary — it is not itself a stage in this cascade.

## Observability

The watcher emits structured `evt:` lines to `.gm/exec-spool/.watcher.log`
(dispatch timings, code-index sweep progress, config resolution, boot
markers), rotated at 10MB. Runtime diagnostic files at `.gm/exec-spool/`
root (`.status.json`, `.turn-summary.json`) are plain JSON, readable
directly.
