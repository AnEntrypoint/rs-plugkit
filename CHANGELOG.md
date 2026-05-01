## 2026-05-02 - session-start writes needs-gm to cover continuation-message bypass

session_start hook now writes .gm/needs-gm for every gm project (AGENTS.md or .gm/ present) at session start, unless prd.yml exists with content (autonomous mode). This closes the isMeta:true bypass where stop-hook feedback and short continuation messages skip UserPromptSubmit, leaving needs-gm unwritten and pre_tool_use unable to enforce gm:gm invocation first.

## 2026-05-02 - obs: trajectory_ingest event + prompt-submit-detail with project_dir/sess

spawn_trajectory_ingest now emits trajectory_ingest (pre-spawn) and trajectory_ingest_done (post-ingest) obs events to rs_learn.jsonl. prompt_submit now emits prompt-submit-detail to hook.jsonl with project_dir, sess, autonomous, and prompt_len fields — enabling correlation between hook fires and ccsniff session audits.

## 2026-04-24 - write needs-gm sentinel on stop-hook blocks

ccsniff audit found: stop hook feedback messages arrive as isMeta:true user messages, bypassing UserPromptSubmit hook. Model responds to git/CI block messages directly with Bash instead of Skill(gm).

Fix: run_stop() and run_stop_git() now write .gm/needs-gm before every block decision. Pre-tool-use hook then blocks non-gm tools even without prompt-submit firing.

Also added NEXT ACTION hint to all block reason strings.

# Changelog

## Unreleased

- fix: stop hook does not push-pressure agents on out-of-reach remotes. New `user_can_push_to_remote(project_dir)` helper in `hook/mod.rs` runs `gh api repos/<owner>/<repo> --jq .permissions.push` and caches the answer per project_dir. `run_stop_git()` now skips both the unpushed-commits check and the CI watch when the remote returns `permissions.push==false` (or no remote / non-github / gh missing). Uncommitted *tracked* changes still block (they're a local concern). On a clean tree against an out-of-reach remote, the stop hook approves with reason `remote is out of user reach (no push permission); local commits accepted, no push attempted, no CI watch`. This prevents agents from being prodded to push thoth/hermes/upstream forks where the user lacks write access.
- fix: session-end hook preserves browser + background tasks across session handoff. Previously closed on every SessionEnd regardless of reason — including `/compact`, `resume`, and background-agent handoffs — which killed the Chrome process tree that tests and agents were driving. Now only fires cleanup when `reason` is one of `clear | logout | prompt_input_exit`.
- fix: stop hook checks `.gm/prd.yml` (YAML) instead of legacy `.prd` (JSON)
