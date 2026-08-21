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
`fiber_lifecycle.rs` and `calculus.rs` both reduce it: no async load step
in this model, so `Reloading` collapses into an atomic transition. -/
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

end Registry
