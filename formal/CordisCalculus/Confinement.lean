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

/-!
## Correspondence to `verbs.rs::confinement_violation` (Rust)

`crates/plugkit-core/src/wasm_dispatch/verbs.rs:646-677`, read verbatim
this session:

```rust
fn confinement_violation(body: &Value, namespace: &str) -> Option<String> {
    let claimed = body.get("discipline").and_then(|v| v.as_str())?;
    if claimed == namespace {
        return None;
    }
    let enabled = crate::orchestrator::discipline_note::enabled_names();
    if enabled.iter().any(|n| n == namespace) {
        Some(format!("confinement violation ..."))
    } else {
        None
    }
}
```

**Correspondence, branch by branch, for the `claimed = Some(c)` case:**

- Rust `claimed == namespace -> None` (admitted) corresponds exactly to
  `admits`'s left disjunct `a.actor == a.target` -- both admit
  unconditionally when the declared actor equals the touched namespace,
  before consulting the registry at all (`confinement_admits_self` above
  is this branch's theorem).
- Rust `enabled.iter().any(|n| n == namespace)` (is `namespace` a
  currently-enabled component) corresponds exactly to `r.contains
  a.target` -- both ask "is the target name a live member of the
  registry."
- Rust returns `Some(violation)` (rejects) exactly when `claimed !=
  namespace AND namespace is enabled`; `admits` returns `false` (rejects)
  exactly when `a.actor != a.target AND r.contains a.target` -- the same
  conjunction, De Morgan-dual to `admits`'s definition
  (`a.actor == a.target || !(r.contains a.target)`). `Registry.confinement`
  above is precisely this branch's soundness theorem: an admitted attempt
  against a registry-member target forces `actor = target`.

**Fidelity gap, not silently elided:** Rust's `claimed` is obtained via
`body.get("discipline").and_then(|v| v.as_str())?` -- the `?` early-returns
`None` (admitted, unconditionally) the moment the caller's JSON body omits
a `discipline` field, WITHOUT ever reaching the `enabled`/registry check.
`EffectAttempt.actor` in this file is a total `String`, carrying no
`Option`/absent case -- the Lean model above covers only the `claimed =
Some(c)` branch of the Rust function; the `claimed = None` branch (every
one of `confinement_violation`'s own doc comments, verbs.rs:646-661,
already names this "fully bypassable, offers no security boundary": a
caller wanting to violate confinement does so by omitting `discipline`
entirely) has no theorem here because it has no admission RULE to state --
it is a fixed `None` regardless of `r`/`a`, trivially "admitted" in a
sense this file's `EffectAttempt` cannot even express since it has no
absent-actor constructor. This is documented, not overlooked: extending
`EffectAttempt.actor` to `Option String` to model the omitted-discipline
case would produce a second theorem stating exactly the Rust comment's own
claim (`actor = none -> admits always = true`), which is definitionally
immediate and adds no new proof content beyond what the Rust author's own
comment already asserts in prose -- not undertaken here because it is not
load-bearing (the check's own author has already documented it is not a
real security boundary).

**Live witness (this session):** `Read` on
`wasm_dispatch/verbs.rs:646-677` (via a fresh Explore-agent dispatch)
returned the function body transcribed above verbatim, confirming the
branch structure this correspondence argument depends on.
-/

end Registry
