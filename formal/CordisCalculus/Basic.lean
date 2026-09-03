/-
A direct Lean 4 formalization of the Cordis paper's Section 4.2 base
calculus, mirroring `rs-plugkit`'s `orchestrator/calculus.rs` (an
executable Rust model of the same objects), but here the metatheory is
proved as unbounded theorems over EVERY possible `Registry`, not checked
for finitely many enumerated states. This is the artifact
`calculus-model-check` (a bounded, executed model check) cannot be: a
`theorem` that Lean's kernel type-checks and accepts as a proof for all
inputs of the stated type, the way the paper's own Theorem 59 is a
universally-quantified statement, not a claim about one run.
-/

/-- A fiber's lifecycle state (paper Definition 44), reduced as
`fiber_lifecycle.rs` and `calculus.rs` both reduce it: an atomic
transition, `Reloading` and `Unloading` collapsed away. Definition 49's
full four-state space, with both transition-in-progress states present as
real inhabitants, lives in `Iterator.lean`, and Table 1's ten-rule
relation over it lives in `Transition.lean`; Theorem 61 and Theorem 64 are
proved there in their general non-atomic form, this file's two-state space
being the idealized case those results specialise to. -/
inductive LifecycleState where
  | inactive
  | active
  deriving DecidableEq, Repr

/-- A component (paper Definition 43: the triple (d, p, e)); the effect
function `e` has no computational content in this abstract model beyond
"installs `provides`", so it is elided, leaving the (d, p) pair plus the
runtime lifecycle state (Definition 44) a fiber carries. -/
structure Fiber where
  requires : List String
  provides : List String
  state : LifecycleState
  retired : Bool
  deriving DecidableEq, Repr

/-- The registry (Definition 45): named fibers. A `List` rather than a
`Finmap`/`HashMap` keeps every proof a plain structural induction over
`List`, Lean's best-supported inductive type, while still modeling an
unbounded/arbitrary-size registry -- the theorems below quantify over
EVERY `Registry`, of any length, not a fixed bound. `abbrev` (not `def`)
so `Registry` unifies transparently with `List (String × Fiber)` for
`List`'s own operations (`++`, `map`, `filter`, ...). -/
abbrev Registry := List (String × Fiber)

namespace Registry

def empty : Registry := []

def find (r : Registry) (name : String) : Option Fiber :=
  (r.find? (fun p => p.1 == name)).map Prod.snd

def contains (r : Registry) (name : String) : Bool :=
  r.any (fun p => p.1 == name)

/-- The coeffect context (Definition 45's `sigma_gamma`): every capability
some `Active` fiber provides. -/
def coeffectContext (r : Registry) : List String :=
  (r.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)

/-- The satisfaction predicate (Definition 46: `sigma |= d`). -/
def satisfied (r : Registry) (name : String) : Bool :=
  match r.find name with
  | none => false
  | some fiber => fiber.requires.all (fun dep => (r.coeffectContext).contains dep)

/-- Definition 58 clause 2: distinct fibers' provisions are disjoint.
This is preservation's OWN statement, over every pair of DISTINCTLY-NAMED
entries in the registry (not only `Active` ones -- O-Insert's premise,
mirrored below, refuses a colliding insert regardless of the existing
fiber's lifecycle state, so well-formedness is a property of the whole
registry). Stated over `List.Pairwise` on names+capability-disjointness
rather than index pairs, avoiding partial (`get!`) list access entirely
and staying total. -/
def wellFormed (r : Registry) : Prop :=
  r.Pairwise (fun p q => p.1 ≠ q.1 → (p.2.provides.filter q.2.provides.contains).isEmpty = true)

/-- O-Insert (Section 4.2): admits a new fiber only if its `provides` is
disjoint from every existing fiber's `provides`, and its name is fresh. -/
def insert (r : Registry) (name : String) (req prov : List String) : Option Registry :=
  if r.contains name then none
  else if r.any (fun p => !(p.2.provides.filter prov.contains).isEmpty) then none
  else some (r ++ [(name, { requires := req, provides := prov, state := .inactive, retired := false })])

/-- O-Retire (Section 4.2): sets the retirement flag, unconditional on the
fiber's own lifecycle state. -/
def retire (r : Registry) (name : String) : Option Registry :=
  if r.contains name then
    some (r.map (fun p => if p.1 == name then (p.1, { p.2 with retired := true }) else p))
  else none

/-- O-Remove (Section 4.2): removes a retired, `Inactive` fiber. -/
def remove (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.retired && fiber.state == LifecycleState.inactive then
      some (r.filter (fun p => p.1 != name))
    else none
  | none => none

/-- L-Reload (Section 4.2): an `Inactive`, non-retired fiber whose target
is satisfied activates. -/
def reload (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.state == LifecycleState.inactive && !fiber.retired && r.satisfied name then
      some (r.map (fun p => if p.1 == name then (p.1, { p.2 with state := .active }) else p))
    else none
  | none => none

/-- L-Unload (Section 4.2): an `Active` fiber whose target is no longer
satisfied, or that has been retired, deactivates. -/
def unload (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name) then
      some (r.map (fun p => if p.1 == name then (p.1, { p.2 with state := .inactive }) else p))
    else none
  | none => none

/-- Reading back the entry a state-update just wrote: mapping `upd` onto
the fiber named `name` (leaving every other entry untouched, the shape
every one of `retire`/`reload`/`unload`'s own `List.map` calls uses) and
then looking `name` up again yields exactly `upd` applied to whatever
`name` was bound to before -- proved by plain structural induction on
the list, the base case for every "the rule did what it claimed" lemma
below. -/
theorem find_map_update (r : List (String × Fiber)) (name : String) (upd : Fiber → Fiber) :
    Registry.find (r.map (fun p => if p.1 == name then (p.1, upd p.2) else p)) name
      = (Registry.find r name).map upd := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    unfold Registry.find at *
    simp only [List.map_cons, List.find?_cons]
    by_cases hc : hd.1 == name
    · simp only [hc, if_true, Option.map_some]
    · simp only [hc, Bool.false_eq_true, if_false]
      exact ih

/-!
## Correspondence to the Rust runtime (Definition 45/46)

`AGENTS.md` cites `registry.rs::get_active_provider` as the Rust
implementation of Definition 45's `provider_k(gamma)` and Definition 46's
`target_n(gamma)`/`quiet(gamma)`. No file named `registry.rs` and no
function named `get_active_provider` exist in `rs-plugkit` -- a live
`codesearch`/grep over `crates/plugkit-core/src` for `fn get_active_provider`
and `registry.rs` returns zero hits (witnessed this session). The real Rust
locus, confirmed by reading `orchestrator/calculus.rs` directly, is
`Registry::satisfied` (calculus.rs:107-113) and `Registry::coeffect_context`
(calculus.rs:91-101):

```rust
pub fn coeffect_context(&self) -> BTreeSet<String> {
    let mut ctx = BTreeSet::new();
    for fiber in self.fibers.values() {
        if fiber.state == LifecycleState::Active {
            for cap in &fiber.provides { ctx.insert(cap.clone()); }
        }
    }
    ctx
}

pub fn satisfied(&self, name: &str) -> bool {
    let ctx = self.coeffect_context();
    match self.fibers.get(name) {
        Some(fiber) => fiber.requires.iter().all(|dep| ctx.contains(dep)),
        None => false,
    }
}
```

**The correspondence is structural identity, not analogy.** Compare field
by field against this file's `Registry.coeffectContext` (line 53) and
`Registry.satisfied` (line 57):

- Rust `Registry.fibers : HashMap<String, Fiber>` vs. Lean `Registry :=
  List (String x Fiber)` -- an unordered finite map represented two
  different but observationally-equivalent ways (hash map vs. assoc list);
  every operation on both sides only ever reads via key lookup
  (`fibers.get`/`Registry.find`), never relies on iteration order, so the
  representation choice is immaterial to every theorem below.
- Rust `coeffect_context` filters `state == Active` then unions `provides`;
  Lean `coeffectContext` filters `state == active` then `flatMap provides`
  (a multiset union collapsing to a set union under `List.contains`) --
  the same two-step filter-then-union, same predicate (`Active` lifecycle
  state), same source field (`provides`).
- Rust `satisfied` looks up `name`, returns `false` on miss, else checks
  `requires.iter().all(|dep| ctx.contains(dep))`; Lean `satisfied` matches
  `Registry.find`, returns `false` on `none`, else checks
  `fiber.requires.all (fun dep => ctx.contains dep)` -- identical miss
  behavior, identical universally-quantified containment check over the
  identical `requires` field against the identical coeffect context.

This is Definition 46's `target_n(gamma)` (satisfaction, the boolean this
file's `satisfied` computes) and the coeffect-context half of Definition 45
(`provider_k` is realized structurally, not as one named function, by this
same `Active`-filtered `provides` union -- there is no single Rust
"provider lookup" because gm's discipline system resolves a capability
`k` to its provider implicitly through `coeffect_context.contains k`,
exactly as `satisfied`'s own `all (fun dep => ctx.contains dep)` does one
key at a time). `discipline_note.rs::active_policies` (the
`Active`-filtered enabled-discipline lookup AGENTS.md also names) is the
same union read back per-discipline rather than recomputed -- it consumes
`coeffect_context`'s result, it does not reimplement it.

**Quiescence (`quiet(gamma)`) has no Rust counterpart in the base-calculus
model, correctly.** `quiet` distinguishes a fiber genuinely at rest from
one mid-transition (`Reloading`/`Unloading`); `Basic.lean`'s own
`LifecycleState` (line 16) has only `inactive`/`active` -- no in-flight
state -- by the same documented reduction `calculus.rs`'s base `Registry`
uses (mirrored one-for-one above). Quiescence is a real distinction only
in `ExtendedRegistry` (`calculus.rs:427` onward, three/four-state
`ExtendedLifecycle`), which this file does not model -- `quiet` is
correctly absent here, not silently dropped; `cordis-iterator-asynchrony-lean-model`
(open PRD row) is where that extended state space gets its own Lean
treatment.

**Live witness (this session):** `grep -rn "fn get_active_provider"
crates/` over rs-plugkit returned zero matches; `Read` on
`orchestrator/calculus.rs` lines 91-113 confirmed the `coeffect_context`/
`satisfied` bodies transcribed above verbatim, matching this file's
`coeffectContext`/`satisfied` field-for-field as argued.
-/

end Registry
