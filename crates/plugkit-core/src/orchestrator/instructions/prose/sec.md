# SEC

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 6 of the pipeline: network, security, and authority. Zero trust at every boundary; every input parsed, never interpolated; every secret out of the tree. The SEC -> RES edge carries the compiled `no-secrets-in-diff` gate -- a high-confidence secret shape in the working diff refuses the transition outright.

## Secrets sweep

The gate catches the common accidental-commit shapes (API key literals, private-key headers, inline-password connection strings, bearer tokens). You sweep for what a shape-list cannot: secrets at lower entropy, secrets in config/prose/fixtures, secrets reachable via a committed path (a key file referenced, not inlined). Every secret routes through an env var or a secret store, never a tracked literal, never a tracked path to a literal. Witness: grep the full diff for the shapes yourself before transitioning, not just trust the gate.

## Injection sweep

Every untrusted input reaching a shell, query, eval, template render, or path join is parameterized or escaped -- never string-interpolated. Walk each input source in the diff (request bodies, CLI args, file contents, env vars, network responses) to its sink and witness the boundary: a total parser returning Accepted/Rejected, a parameterized query, a shellescape. `exec_js` the adversarial input live -- quote chars, separators, path traversal, encoding tricks -- and witness rejection or safe handling, never assume the escape works.

## Identity and authority sweep

Zero trust: every request authenticated, every action authorized at the boundary that performs it -- no ambient authority, no "the caller already checked", no trust-by-network-position. Boundary limits are explicit: what this component may do is declared at its edge, and anything outside that declaration fails closed. Failing CLOSED on the unrecognized is the standing rule -- a gate, a parser, an auth check that waves through what it does not positively recognize is a vulnerability wearing a convenience costume.

## Message and timing sweep

Messages exactly-once and idempotent: a replayed delivery converges (the same idempotency discipline STATE audits for dispatch surfaces, applied to every network edge the diff touches). Byzantine-tolerant at trust boundaries: a malformed, duplicated, or reordered message degrades to a named rejection, never to corrupted state. Constant-time for secrets: no secret-dependent control flow -- no early-exit comparison, no branch on a key byte, no timing sidechannel a remote observer could measure. Witness by exercising the replay/malformed cases under `exec_js` and diffing state after each.

## Discovery

A fix shaped like a boundary/ownership change routes `transition to=STATE` (the graph's SEC -> STATE feedback edge -- zero-trust and affine boundary enforcement flow back into the functional core). A code repair routes `transition to=EMIT`. A reshaping discovery routes `transition to=SPECIFY`. Dispatch, never narrate.

## Dispatch

`transition to=RES` only when every sweep above has a live witness behind it, same turn.
