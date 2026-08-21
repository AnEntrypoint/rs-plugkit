# CordisCalculus

A Lean 4 formalization of the Cordis paper's Section 4.2 base calculus
(the abstract Registry/Fiber model and its five rules --
O-Insert/O-Retire/O-Remove/L-Reload/L-Unload), proved directly from the
paper's own definitions with no dependency on gm's Rust code.

`CordisCalculus/Basic.lean` defines `Registry`, `Fiber`, `wellFormed`
(Definition 58 clause 2), `satisfied` (Definition 46), and the five
rules as total functions matching the paper's own premises.

`CordisCalculus/Preservation.lean` proves `Registry.preservation`
(Theorem 59, clause 2): applying any of the five rules to a well-formed
registry, if it succeeds, produces a well-formed registry -- an
unbounded theorem over every `Registry` of every length, not a bounded
enumeration. `#print axioms Registry.preservation` reports only Lean's
standard trusted core (`propext`, `Classical.choice`, `Quot.sound`), no
`sorry` and no custom axiom.

This complements, rather than replaces, `../crates/plugkit-core/src/orchestrator/calculus.rs`
(an executable Rust model of the same objects, whose `calculus-model-check`
verb exhaustively enumerates every state reachable in a small bounded
fixture) and `discipline_note.rs`'s `discipline-audit` (live runtime
checks over gm's own current discipline/plugin/namespace state). Three
different strengths of evidence for the same theorems: a live check
over the current state, a bounded exhaustive model check, and an
unbounded machine-checked proof.

## Building

```sh
lake build
./.lake/build/bin/cordiscalculus
```

Requires Lean 4 (`elan`, https://leanprover-community.github.io/get_started.html);
CI (`.github/workflows/lean-formal-proof.yml` at the repo root) builds
this on every push touching `formal/`.
