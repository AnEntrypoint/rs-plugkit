# SEC

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 6 of the pipeline: network, security, and authority. The SEC -> RES edge carries the compiled `no-secrets-in-diff` gate -- a high-confidence secret shape in the working diff refuses the transition outright.

## Preferences (named, narrow)

Security & Robustness

* OWASP Top 10 (OWASP Foundation)
* STRIDE Threat Model (Loren Kohnfelder & Praerit Garg)
* Postel's Law (Jon Postel)
* Principle of Least Privilege (Jerome Saltzer & Michael Schroeder)

## Sweeps

Every sweep is witnessed live, same turn -- never assume the escape works.

**Secrets.** The gate catches the common accidental-commit shapes. You sweep for what a shape-list cannot: secrets at lower entropy, secrets in config/prose/fixtures, secrets reachable via a committed path. Every secret routes through an env var or a secret store, never a tracked literal, never a tracked path to a literal. Grep the full diff for the shapes yourself before transitioning, not just trust the gate.

**Injection.** Every untrusted input reaching a shell, query, eval, template render, or path join is parameterized or escaped -- never string-interpolated. Walk each input source in the diff (request bodies, CLI args, file contents, env vars, network responses) to its sink and witness the boundary. `exec_js` the adversarial input live -- quote chars, separators, path traversal, encoding tricks -- and witness rejection or safe handling.

**Identity and authority.** Every request authenticated, every action authorized at the boundary that performs it -- no ambient authority, no "the caller already checked", no trust-by-network-position. Failing CLOSED on the unrecognized is the standing rule -- a gate, a parser, an auth check that waves through what it does not positively recognize is a vulnerability wearing a convenience costume.

**Message and timing.** Messages exactly-once and idempotent at every network edge the diff touches: a replayed delivery converges instead of double-applying. A malformed, duplicated, or reordered message degrades to a named rejection, never to corrupted state. Constant-time for secrets: no secret-dependent control flow, no early-exit comparison, no timing sidechannel. Witness by exercising the replay/malformed cases under `exec_js` and diffing state after each.

## Discovery

A fix shaped like a boundary/ownership change routes `transition to=STATE` (the graph's SEC -> STATE feedback edge -- zero-trust and affine boundary enforcement flow back into the functional core). A code repair routes `transition to=EMIT`. A reshaping discovery routes `transition to=SPECIFY`. Dispatch, never narrate.

## Dispatch

`transition to=RES` only when every sweep above has a live witness behind it, same turn.
