import CordisCalculus.Basic

/-!
Confinement (paper Definition 48, Section 4.2): an effect function's
writes/reads during a fiber's activation must be bounded to that fiber's
own name -- it cannot mutate or read state belonging to a differently
named fiber. `Basic.lean`'s `Registry`/`Fiber` model carries no notion of
an effect TARGET at all (only `requires`/`provides` capability lists), so
Confinement has no home there; this file adds the minimal extra structure
needed to state and prove it, mirroring the real gap found in
`wasm_dispatch/verbs.rs`: `kv_put`/`kv_query`/`kv_get`/`memorize` took a
caller-supplied `namespace` string with no check against which component
was actually dispatching, so any enabled component could name any OTHER
enabled component's namespace and read or write its state. `verbs.rs`'s
`confinement_violation` is the Rust-side fix this file's `guard` mirrors
exactly: refuse only when the claimed identity differs from the target
AND the target names a member of the registry (a currently-enabled
component), leaving every unscoped call (no claimed identity) untouched.
-/

namespace Registry

/-- An effect application: a fiber named `actor` attempts to touch state
named `target`. This is the abstract shape of every `kv_put`/`kv_query`/
`kv_get`/`memorize` dispatch in `verbs.rs` -- `actor` is the caller's
declared `discipline` field (when present), `target` is the `namespace`
field the effect actually touches. -/
structure EffectAttempt where
  actor : String
  target : String
  deriving DecidableEq, Repr

/-- The guard `verbs.rs`'s `confinement_violation` implements: an attempt
is admitted unless it names a DIFFERENT actor and target, AND that target
is a member of the registry (an enabled, distinctly-named component whose
state the actor has no claim to). An attempt targeting its own name is
always admitted; an attempt targeting a name absent from the registry
(no component owns it) is not a confinement question and is also
admitted -- exactly the two `None`-returning branches of the Rust
function. -/
def admits (r : Registry) (a : EffectAttempt) : Bool :=
  a.actor == a.target || !(r.contains a.target)

/-- Confinement (Definition 48), stated as the guard's own soundness: an
admitted attempt whose target IS a member of the registry must have
`actor = target` -- the fiber touching state under name `target` can only
be the fiber named `target` itself. This is the unbounded form of what
`confinement_violation`'s test in `verbs.rs` checks per-dispatch: there,
one JSON body is checked against one registry snapshot; here, the
property holds for EVERY `Registry` and EVERY `EffectAttempt`, not one
witnessed call. -/
theorem confinement (r : Registry) (a : EffectAttempt) (hmem : r.contains a.target)
    (hadmit : admits r a = true) : a.actor = a.target := by
  unfold admits at hadmit
  rw [Bool.or_eq_true, Bool.not_eq_true'] at hadmit
  cases hadmit with
  | inl h => exact of_decide_eq_true h
  | inr h => rw [h] at hmem; contradiction

/-- The converse a guard must also satisfy to be doing real work, not a
vacuous refusal: an attempt whose actor genuinely equals its target is
NEVER rejected, for every registry -- confinement never blocks a
component from touching its own state. Matches `confinement_violation`'s
first check (`if claimed == namespace { return None }`), which returns
before the registry is even consulted. -/
theorem confinement_admits_self (r : Registry) (name : String) :
    admits r { actor := name, target := name } = true := by
  unfold admits
  simp

end Registry
