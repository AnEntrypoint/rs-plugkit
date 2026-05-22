pub const TEXT: &str = r#"# UPDATE-DOCS

YOU are the state machine. Plugkit is the synchronous library serving this prose; advancing the chain is your dispatch, not its action. Docs do not update themselves — you dispatch every edit and every push.

Your docs reflect the current state of the system, not its history. You write every rule in AGENTS.md as a present-tense statement about what must or must-not be the case in code now. Past-tense framing, `(FIXED)` markers, dated audit entries, and "we used to X, now we Y" phrasing belong in `git log` and `CHANGELOG.md` — you never put them in AGENTS.md.

## AGENTS.md and CLAUDE.md

You route edits to AGENTS.md and CLAUDE.md through the `memorize-fire` verb only — you never inline-edit. You dispatch by writing the fact body to `.gm/exec-spool/in/memorize-fire/<N>.txt` (raw text, or JSON `{text, namespace?}`). Plugkit's classifier rejects changelog-shaped facts from AGENTS.md ingestion (rs-learn still accepts them). For multiple facts you write multiple parallel spool requests in one message.

## README.md

You refresh README to reflect the surface a new reader actually encounters. You remove stale install steps, version pins, and features that no longer exist. You add what you added this session if it changes the public surface.

## docs/index.html

You regenerate or hand-edit to reflect the same surface. Site builds run from `site/`; the deployed `/` route renders from `site/content/pages/home.yaml` via flatspace. You route landing edits through `site/theme.mjs` (Hero) and the YAML — never `site/index.html` directly. `docs/styles.css` is generated from `site/input.css`; you append to the source, not the output.

## CHANGELOG.md

You write one entry per commit you landed this session. The commit message line plus a one-sentence "why" — no recipe, no narration. CHANGELOG carries the history that AGENTS.md refuses.

## Commit and Push

You stage doc updates only — you never bundle them with code changes from earlier phases (you committed those at their own time). One commit, present-tense imperative subject. Before you push, `git status --porcelain` reads empty; non-empty = uncommitted residual that the push would orphan, and you commit or revert it first. Then you push to main. Your push triggers the docs pipeline if the repo has one. A doc commit stages only paths matching AGENTS.md, CLAUDE.md, README.md, SKILLS.md, CHANGELOG.md, LICENSE*, docs/**, or site/** — any non-doc path in a doc commit is a sign you bundled phases and you split it back out before staging.

## COMPLETE

This is the terminal phase. After your push lands, you dispatch `transition` to COMPLETE. Plugkit then records the chain as concluded.

**Once `phase=COMPLETE` and `prd_pending_count=0`, the chain is closed.** You do not re-dispatch `instruction` to "check" status — there is nothing to check; the response will be the same UPDATE-DOCS prose you are reading now. You do not dispatch any other verb either. The session ends. If the user gives you a new request, plugkit will reset the phase to PLAN on the first instruction dispatch with a fresh prompt body.

Re-dispatching instruction on a COMPLETE chain with no new prompt is a deviation: it burns cycles, accumulates `turn.start`/`turn.end` pairs with `dispatches:1`, and signals that the agent is treating instruction as a polling primitive. The recovery is to stop dispatching; the user reactivates the chain.

## Dispatch

You dispatch `phase-status` to confirm the chain state, then `transition` to COMPLETE if you have not already. After COMPLETE lands, you stop.

Transition: when you have committed and pushed docs → you dispatch `transition` to advance to COMPLETE. Chain done.
"#;
