# CordisCalculus

A Lean 4 formalization of the Cordis paper's Section 4.2 base calculus
(the abstract Registry/Fiber model and its five rules --
O-Insert/O-Retire/O-Remove/L-Reload/L-Unload), Section 3.1.3's
effect-independence framework, and Section 3.3.2's observational
equivalence, proved directly from the paper's own definitions with no
dependency on gm's Rust code.

`CordisCalculus/Basic.lean` defines `Registry`, `Fiber`, `wellFormed`
(Definition 58 clause 2), `satisfied` (Definition 46), and the five
rules as total functions matching the paper's own premises.

All five base-calculus theorems from the paper's metatheory (Section 7)
are proved here as unbounded Lean theorems, over every `Registry` of
every length and every fiber name -- not a bounded enumeration. Every
one is checked via `#print axioms` to depend only on Lean's standard
trusted core (`propext`, `Classical.choice`, `Quot.sound`), with zero
`sorry` and zero custom axioms anywhere in the project.

- **Theorem 59 (preservation)**, `CordisCalculus/Preservation.lean`:
  `Registry.preservation` -- applying any of the five rules to a
  well-formed registry, if it succeeds, produces a well-formed
  registry.
- **Theorem 61 (recovery-exactness)**, `CordisCalculus/Recovery.lean`:
  `Registry.unload_preserves_fields` (an `unload` touches only the
  named fiber's `state` field) and `Registry.unload_reload_recovers_exactly`
  (withdrawing then reinstating a fiber, once its target is satisfied
  again, restores it to `Active` with every other field byte-identical
  to before withdrawal -- `unload` is a genuinely revertible effect,
  not a destructive one).
- **Theorem 63 (ordering)**, `CordisCalculus/Ordering.lean`:
  `Registry.unload_only_on_lost_target` (a successful `unload` only
  ever fires on a fiber that was retired or had lost satisfaction),
  `Registry.unload_refuses_satisfied_non_retired` (the contrapositive:
  an active, non-retired, satisfied fiber can never be unloaded), and
  `Registry.unload_result_is_inactive` (the withdrawn fiber genuinely
  reaches `Inactive`).
- **Theorem 66 (progress)**, `CordisCalculus/Progress.lean`:
  `Registry.progress` -- every fiber outside the three rest states
  (inactive+unsatisfied, active+satisfied, retired+removed) admits a
  legal rule application; the calculus never gets stuck mid-lifecycle
  on a single component.
- **Theorem 73 (confluence)**, `CordisCalculus/Confluence.lean`:
  `Registry.map_update_comm` (independent name-targeted state updates
  commute under `List.map`) and `Registry.retire_retire_comm` (two
  `retire` calls on distinct names reach the same registry regardless
  of order) -- the same argument applies uniformly to every other pair
  of independent rule applications, since each rule reduces to the
  same guard-check-then-map-by-name shape.

- **Definitions 17-21 (effect-independence framework)**,
  `CordisCalculus/Independence.lean`: `RevertibleEffect`, `InMonoid`
  (Definition 17's transformation monoid, modeled as an inductive
  closure predicate since this project has no monoid/group-theory
  library dependency), `commuting_of_generators_commuting` (Lemma 18:
  generator-level commutation lifts to the whole monoid), `Independent`
  (Definition 19's two-clause independence), and
  `independent_pair_revert_nonlifo_order`/`revertLifo2_correct`
  (Theorem 20/Corollary 21: two independent effects revert in either
  LIFO or non-LIFO order and reach the exact same initial state).
- **Definitions 33-41 (observational equivalence)**,
  `CordisCalculus/ObservationalEquivalence.lean`: `ObsEquiv` (`~=_A`,
  the general capability-indexed congruence used throughout Section
  3.3) and the DISTINCT, narrower `RegistryEquiv` (`~`, used from
  Theorem 61/Corollary 62 onward, which forgets only registry list-order
  provenance -- modeled as `List.Perm` rather than list equality).
  `RegistryEquiv.to_obsEquiv` proves the one-directional containment:
  `~`-equivalent registries are `~=_A`-equivalent for every capability
  set `A`. Definition 40/Theorem 42 (commutative keys connecting back
  to the independence framework) is explicitly scoped out with a
  one-line reason in the file's own doc comment -- it needs a genuine
  commutative-key algebraic structure on the coeffect index type, a
  separate, comparably-sized effort to the two frameworks this file
  completes.

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
