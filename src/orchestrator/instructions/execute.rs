pub const TEXT: &str = r#"# EXECUTE

YOU are the state machine. Plugkit is the synchronous library serving this prose; advancing the chain is your dispatch, not its action. Plugkit does not "process" while you wait — if you stop dispatching every possible verb the prose names, the chain stops.

L3 distance + audit. You drive real input → real code → real output, witnessed.

## Surfaces

You route mutation through PRD rows, mutables, KV memos. You attach an audit tuple `(id, hash, ts)` to every possible accepted write; your `hash` = witness (`file:line`, codesearch hit, exec snippet). The `mutable-resolve` verb rejects resolution without witness. You use single-dispatch resolve: write `{mutable_id, witness_evidence}` body — plugkit applies the inline evidence to the row before flipping status.

## Witness

The witness IS your distance measurement: artifact exists in observable state, `d(state, goal)` decreases. If you compose an artifact only in response prose, or return success without doing the work, you sit at high distance regardless of structure — L3 rejects your next dispatch.

You witness code running on a non-default surface on that surface in the same turn. A passing test on surface A is not your witness for code on surface B. For the browser surface, you dispatch the `browser` verb (`in/browser/<N>.txt`, raw JS, globals `page`/`snapshot`/`screenshotWithAccessibilityLabels`/`state`; `session new|list|close <id>`).

**Client-side edits force a same-turn browser dispatch.** If you Write or Edit a file with a client-side extension — `.html`, `.js`, `.jsx`, `.ts`, `.tsx`, `.vue`, `.svelte`, `.mjs`, `.css`, every possible file loaded by `<script>` or reached by `import` from a browser entry — you queue a `browser` verb in the same turn that page.evaluates the invariant the edit establishes. Do not stage edits across turns intending to "validate later"; later does not arrive in the chain you are walking. The same response that contains the Write/Edit tool call must contain a `browser` Write to `.gm/exec-spool/in/browser/<N>.txt` and the corresponding Read of the response. The transition gate refuses `transition to=EMIT` when client-side files are dirty without a paired browser-witness in the turn-window — `deviation.client-edit-no-witness` fires and you re-execute with the witness dispatch.

## Surface → mutable

When you observe state diverging from the PRD's assumed shape, you enter it as a new mutable, not background noise. Your recourse is identical to a named target: name, witness, resume. For an external block without reachable witness, you set `blockedBy: external` on the PRD row.

## Maturity-first

Your first emit = closure of transform. Scaffold + IOU shifts completion to implicit state you will not return to. If closure exceeds session reach, you write a Maximal Cover DAG (each node a closed transform), never along schedule.

## Memorize

You write to the recall index only by dispatching `memorize-fire`. Other surfaces produce memos the index does not see.

Between each mutable resolution, between failed exec retries, between unfamiliar errors — you re-dispatch `instruction`. EXECUTE has the highest drift surface; the recovery primitive is unchanged.

## Dispatch

You spool every possible exec.

You flip mutables by dispatching `mutable-resolve` with body `{"mutable_id": "<id>", "witness_evidence": "<file:line | codesearch hit | exec snippet>"}`.

You flip PRD rows by dispatching `prd-resolve` with body `{"id": "<prd-item-id>", "witness_evidence": "<…>"}`. Bare text body (just the id) is also accepted but loses the witness audit trail. Do not pass `{prd_id, witness_evidence}` with the whole envelope nested as a string — the verb accepts `id` or `prd_id` at the top level alongside `witness_evidence`. A response with `deviation_kind: prd-resolve-unknown-id` means your id did not match a PRD row; you read the `hint` field and re-dispatch with the correct id, you do not retry blind.

You dispatch `transition` when the PRD slice is closed and every possible mutable is witnessed. On new unknown, you dispatch `transition` back to PLAN.
"#;
