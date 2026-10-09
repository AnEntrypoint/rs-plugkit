The walk is complete once the PRD is empty and the worktree is clean. Hand off to gm-continue, which searches for remaining work.

## PRD resolution mantra

The graph traversal creates PRD rows as nodes find work. A node that finds an unknown adds a row with `prd-add`; an unknown is a row, never a guess. A row opened by one finding may open more rows, and the walk never stops while any row is open.

A row closes only with a witness: a dispatch id from its own live run. A row without a witness stays open. The walk continues until no row is open.

## Parallel PRD fan-out

Every open row is resolved by its own parallel worker, with its own session id.

1. Read the open rows with `prd-list` and `{"status":"pending"}`.
2. For each open row, dispatch one worker in the same tool-call block. The worker session id is `<parent-session>-pw-<row-id>`, and the worker body carries the row id. Every worker uses the gm skill and codeinsight (`callers`/`impact`) first.
3. A worker closes its row only with a witness dispatch id from its own live run. A row without a witness stays open.
4. Record the fan-out as a note on the parent row with `prd-add` and id `pw-fanout-<n>`, listing each row id, worker session id, resolution status and witness dispatch id.
5. A session with open rows and no running worker is a stall. Re-fan-out the open rows. A stall is not a stop.

Continuation: never stop while a row is open. Ask only for a world-scoped one-way door; every other unknown becomes a row.

Publishing: workers do not commit or push. The parent session publishes every change in one delivery: commit with the work's own paths, push the submodule, bump the parent pin to the pushed commit, then push the parent. Nothing stays local. A walk is complete only when the PRD is empty and the worktree is clean; until then, the next node is dispatched.

## Automatic decisions

When an instruction already settles a choice, apply it without asking. Commit identity: use the repository's configured identity; where none is set, use the identity the user approved for authorship (anentrypoint <admin@coas.co.za>) written as repository-local config, never global. Publishing: push every commit and bump every pin in the same delivery. Runner: a restart of agentplug is approved at any time; load a rebuilt plugin through the isolated recipe first, then restart. Ask only for a world-scoped one-way door.
