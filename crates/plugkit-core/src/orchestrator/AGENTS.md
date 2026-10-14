# AGENTS.md

Module-scoped rules for the orchestrator sources in this directory. The [root AGENTS.md](../../../../AGENTS.md) owns the ownership map and the delivery rules.

## Disciplines, fibers and calculus

- `capability_proxy::resolve` and `discipline_note::requires_satisfied` must match providers identically: both use `resolve_key_realm` (an empty or self-named realm maps to the discipline realm), require an Active provider and check `provides`.
- `discipline_note::active_policies` is the only caller of discipline `advance_fiber`, once per `instruction`. `all_known_discipline_dirs` includes disabled names with state files, so a disabled discipline advances to Inactive.
- `build_interception_context` folds enabled-file order with right-biased `MergeKind::combine`; for `ScalarOverwrite` the last nonempty declaration wins. `handle_check_removal` is the only writer of `enabled.txt` and CAS-writes against the snapshot it read.
- Guest `fiber_lifecycle::transition` is mirrored arm for arm by agentplug-host `registry.rs` `PluginFiberLifecycle`; the guest has only Inactive, Active and Unloading.
- `calculus::verify_calculus` and `formal/CordisCalculus/` change together; it stops silently at `max_states`, so `ok` covers only explored states. Base-registry unload's `retired || !satisfied` equals the paper's `target != omega`; ExtendedRegistry has no retirement flag.
- `component_loader_dispatch::parse_entries` silently drops entries that fail to deserialize; `isolate` is the tagged `{"kind": "none"|"local"|"global"}` shape; `claim_audit_clean` passes only on the exact body `clean`.

## PRD witness binding

- `prd-resolve` binds a witness three ways: `witness_dispatch_id`, a file inside the project root (`witness_output_path`, relative or absolute under the root, re-read and re-hashed), or inline text (`witness_output`, hashed by `verify_inline_witness_output`). The witness sandbox reads nothing outside the project root, so an absolute path outside it is refused with a sentence that names the inline way out.
- An inline binding stores `output_sha256`, `output_inline` and `output_bytes`, never the text, and no `output_path`: `stale_completed_rows` skips a binding with no `output_path`. The duplicate-witness check compares only open rows, because a resolved row moves to `.gm/prd-log.jsonl`.

## Dream-RSI replay

- Replay is frozen-world evidence, not execution or deployment authority: records stay bound to their completed dispatch and the owner session.
- Score each world as best attained quality minus `beta1 * summed cost` plus `beta2 * parallelism_bonus`; a policy scores the mean over its worlds, and a challenger wins only with a strict improvement over the incumbent on the same worlds. Betas are explicit finite nonnegative inputs.
- `dream-replay-round` is session-owned progressive reveal from the policy roots and the revealed frontier; it closes at `max_rounds` or `max_nodes`, and `max_online_rounds` bounds new distinct values.
- `dream-replay-cycle` is observation-only maintenance: it uses the canonical owner `session_id`, verifies ledger evidence through `automatic_replay`, defers when no new verified dispatch exists, and never refreshes phase clocks.
- `admit_dispatch` ranks and never refuses: `Admission::Allow` or `Admission::Advisory`, attached to a dispatch that ran (`dream_rsi_advisory`).
- A successful `instruction` clears the ranking by stamping `.gm/dream-rsi/<sid>/reorientation-ts` and the project-wide `.gm/dream-rsi/_any-session/reorientation-ts` (`PROJECT_WIDE_MARKER_SESSION`); the ranking lapses after `VETO_MAX_AGE_MS` (600000). `.gm/last-instruction-ts` is a third marker, but `gates` stamps it only for a prose-serving dispatch.

