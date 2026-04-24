## 2026-04-24 - write needs-gm sentinel on stop-hook blocks

ccsniff audit found: stop hook feedback messages arrive as isMeta:true user messages, bypassing UserPromptSubmit hook. Model responds to git/CI block messages directly with Bash instead of Skill(gm).

Fix: run_stop() and run_stop_git() now write .gm/needs-gm before every block decision. Pre-tool-use hook then blocks non-gm tools even without prompt-submit firing.

Also added NEXT ACTION hint to all block reason strings.

# Changelog

## Unreleased

- fix: stop hook checks `.gm/prd.yml` (YAML) instead of legacy `.prd` (JSON)
