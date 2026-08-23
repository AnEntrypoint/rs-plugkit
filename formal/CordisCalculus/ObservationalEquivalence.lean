import CordisCalculus.Basic

/-!
Observational equivalence (paper Section 3.3.2, Definitions 33-41).
This file models TWO DISTINCT congruence relations the paper uses, kept
carefully separate per this session's own instruction:

1. `ObsEquiv A g1 g2` (`~=_A`, Definitions 33-39): the general
   capability-indexed congruence used throughout Section 3.3 -- two
   registries are indistinguishable to an observer holding capability
   set `A` when every capability in `A` reads the same satisfaction
   answer from both registries. This is a FAMILY of relations, one per
   observer capability set, and is coarser as `A` shrinks (an observer
   who can query fewer capabilities distinguishes fewer registry pairs)
   -- Definitions 33-39's own monotonicity content, proved below as
   `ObsEquiv.mono`.

2. `RegistryEquiv g1 g2` (`~`, used from Theorem 61/Corollary 62
   onward in Section 4.4): a STRICTLY NARROWER relation that forgets
   ONLY registry bookkeeping/provenance -- two registries are `~`
   equivalent when they contain the same fibers under the same names
   with the same `requires`/`provides`/`state`/`retired` fields, in
   possibly different LIST ORDER (a `Registry` is a `List`, and list
   order is bookkeeping the paper's own state model does not consider
   observable -- Definition 45 defines the registry as a NAMED SET of
   components). `RegistryEquiv` is provably FINER than `ObsEquiv` at
   every capability set (`RegistryEquiv.to_obsEquiv` below): agreeing
   on every field of every fiber implies agreeing on every
   capability's satisfaction, but not conversely -- two registries can
   satisfy the exact same capabilities while differing in `requires`
   lists for fibers no observer's capability set queries, so `~=_A` is
   coarser than `~` in general. `unload_reload_recovers_exactly`
   (`Recovery.lean`) already proves the STRONGER exact-field-equality
   statement directly, so `RegistryEquiv` is not re-derived from it
   here; this file instead establishes `RegistryEquiv` as its own
   standalone relation with its own equivalence-relation proof, and
   connects it to `ObsEquiv` via the one-directional
   `to_obsEquiv` implication the paper's own Section 4.4 relies on
   (a `~`-equivalent recovered registry is also `~=_A`-equivalent for
   EVERY `A`, which is what licenses treating recovery as "as if
   nothing happened" from any observer's point of view, not merely
   under the specific bookkeeping-forgetting relation itself).
-/

namespace Registry

/-- Definitions 33-36's observer-relevant predicate: an observer
holding capability set `A` can query `satisfied name` for exactly the
names IN `A` (capabilities the observer itself has access to querying,
per Section 3.3.1's coeffect-scoped observation model) -- this is the
one observation `ObsEquiv` is built from, matching Definition 46's own
`sigma |= d` predicate as the atomic fact an observer reads. -/
def observedSatisfaction (r : Registry) (A : List String) : List Bool :=
  A.map (fun name => r.satisfied name)

/-- Definitions 33-39: `~=_A`, observational equivalence relative to
capability set `A` -- two registries are indistinguishable to an
observer with capability set `A` exactly when every capability `A`
names reads the same satisfaction answer from both. -/
def ObsEquiv (A : List String) (g1 g2 : Registry) : Prop :=
  ∀ name ∈ A, g1.satisfied name = g2.satisfied name

theorem ObsEquiv.refl (A : List String) (g : Registry) : ObsEquiv A g g := fun _ _ => rfl

theorem ObsEquiv.symm {A : List String} {g1 g2 : Registry} (h : ObsEquiv A g1 g2) :
    ObsEquiv A g2 g1 := fun name hname => (h name hname).symm

theorem ObsEquiv.trans {A : List String} {g1 g2 g3 : Registry}
    (h12 : ObsEquiv A g1 g2) (h23 : ObsEquiv A g2 g3) : ObsEquiv A g1 g3 :=
  fun name hname => (h12 name hname).trans (h23 name hname)

/-- Definitions 33-39's monotonicity content: an observer with a
SMALLER capability set distinguishes FEWER registry pairs -- `ObsEquiv`
at a sublist `A'` of `A` is implied by `ObsEquiv` at `A` (a coarser
relation, since it only has to agree on fewer capabilities). This is
the formal content behind "a more restricted observer sees the two
registries as MORE alike, never less" -- an observer capability set
strictly narrows what can be distinguished, it never grows it. -/
theorem ObsEquiv.mono {A A' : List String} (hsub : ∀ name ∈ A', name ∈ A)
    {g1 g2 : Registry} (h : ObsEquiv A g1 g2) : ObsEquiv A' g1 g2 :=
  fun name hname => h name (hsub name hname)

/-- The empty capability set is the coarsest observer: every registry
pair is trivially `~=_[]`-equivalent, since there is nothing to
observe. This is `ObsEquiv.mono`'s degenerate boundary case, named
separately since it is the base case a shrinking-capability-set
induction bottoms out at. -/
theorem ObsEquiv.trivial_at_empty (g1 g2 : Registry) : ObsEquiv [] g1 g2 :=
  fun _ hname => absurd hname (List.not_mem_nil)

/-- `RegistryEquiv`, the narrower `~` relation used from Theorem
61/Corollary 62 onward: two registries are `~`-equivalent when the
underlying `(name, fiber)` list of one is a PERMUTATION of the other
-- exactly the same entries, in possibly different order. `List.Perm`
is chosen (rather than a `find`-pointwise formulation) because it is
the relation that GENUINELY forgets only list-order provenance: a
`Registry` is `List (String x Fiber)` (`Basic.lean`), and `Perm` is
Lean's own standard "same multiset of elements" congruence over
`List`, with every `filter`/`flatMap`/`map` operation `satisfied`/
`coeffectContext` are built from already PROVEN permutation-invariant
in Lean's core library -- unlike a `find`-pointwise formulation (an
earlier draft of this file), which does not by itself constrain
`filter`/`flatMap` without an additional no-duplicate-names
side-condition. `Registry.wellFormed` (Definition 58's own
distinct-names clause, `List.Pairwise` on name inequality) is exactly
that side-condition for `find`-pointwise agreement to coincide with
`Perm`; `Perm` alone is what this file needs and proves against. -/
def RegistryEquiv (g1 g2 : Registry) : Prop := g1.Perm g2

theorem RegistryEquiv.refl (g : Registry) : RegistryEquiv g g := List.Perm.refl g

theorem RegistryEquiv.symm {g1 g2 : Registry} (h : RegistryEquiv g1 g2) : RegistryEquiv g2 g1 :=
  List.Perm.symm h

theorem RegistryEquiv.trans {g1 g2 g3 : Registry}
    (h12 : RegistryEquiv g1 g2) (h23 : RegistryEquiv g2 g3) : RegistryEquiv g1 g3 :=
  List.Perm.trans h12 h23

/-- `RegistryEquiv` implies `ObsEquiv` at every capability set (proved
below as `RegistryEquiv.to_obsEquiv`): two registries agreeing on every
fiber's exact fields (differing only in list-order provenance)
necessarily agree on every capability's satisfaction answer too, since
`satisfied`/`coeffectContext` are both computed purely from the
`(name, fiber)` pairs a registry contains, never from their list
position. This is the one-directional containment `~` (finer) implies
`~=_A` (coarser) for every `A`, the connection Section 4.4 leans on
when treating a `~`-recovered registry as observationally transparent
to every possible observer, not merely equivalent under the specific
field-level relation. The converse fails in general: two registries
can satisfy the same capabilities while differing in the `requires`
list of a fiber no observer's capability set actually queries.

Every name a `namesNodup` registry contains (defined just below) looks
up uniquely.

`wellFormed` (Definition 58's own `Pairwise` over
`p.1 ≠ q.1 → disjoint-provides`) does not, by itself, name-Pairwise
distinguish two SAME-named entries -- its relation is vacuously true
whenever `p.1 = q.1` (the implication's premise fails). This project's
own registries never legally reach a duplicate-named state in the
first place (`insert`'s own `r.contains name` guard, `Basic.lean`,
refuses to admit one), so `wellFormed` alone is not strong enough to
derive `Nodup` on names for an ARBITRARY list; `NamesNodup.lean`
supplies the genuine `Registry.namesNodup` invariant -- separate from
`wellFormed`, tracking that `insert` is the only introduction rule and
it always guards freshness -- threaded through every constructor
(`insert`/`retire`/`remove`/`reload`/`unload`) as its own preserved
property (`Registry.namesNodup_preservation`), plus the `empty`
base case (`Registry.empty_namesNodup`) every reachable registry's own
`namesNodup` derives from. `find_perm_invariant`/`RegistryEquiv.to_obsEquiv`
below still take `namesNodup` as an explicit hypothesis rather than
re-deriving it inline (the well-formedness-hypothesis discipline this
session's own instruction calls for) -- `NamesNodup.lean`'s own
preservation theorem is what a caller starting from `Registry.empty`
uses to discharge that hypothesis for any state reached by real rule
applications. -/
def namesNodup (g : Registry) : Prop := (g.map Prod.fst).Nodup

theorem wellFormed_unique_name {g : Registry} (hnodup0 : g.namesNodup) {p q : String × Fiber}
    (hp : p ∈ g) (hq : q ∈ g) (hpq : p.1 = q.1) : p = q := by
  unfold Registry.namesNodup at hnodup0
  induction g generalizing p q with
  | nil => cases hp
  | cons hd tl ih =>
    rw [List.map_cons, List.nodup_cons] at hnodup0
    cases hp with
    | head => cases hq with
      | head => rfl
      | tail _ hq' =>
        exfalso
        apply hnodup0.1
        rw [hpq]
        exact List.mem_map_of_mem hq'
    | tail _ hp' => cases hq with
      | head =>
        exfalso
        apply hnodup0.1
        rw [← hpq]
        exact List.mem_map_of_mem hp'
      | tail _ hq' => exact ih hnodup0.2 hp' hq' hpq

/-- `find` under `Perm` may reorder which of several entries a
duplicate-name registry would report first, but `namesNodup` (a
well-formedness hypothesis this session states EXPLICITLY -- see
`namesNodup`'s own doc comment above for why `wellFormed` alone does
not suffice) rules out duplicates, so a `namesNodup` witness makes
`find` genuinely permutation-invariant -- the fact `to_obsEquiv` below
needs to relate `satisfied` (built from `find`) across a `Perm`.
Proved via membership: a successful `g1.find name = some fiber` gives
`fiber ∈ g1` with `fiber.1 = name` (`List.mem_of_find?_eq_some` plus
the `find?` predicate itself); `Perm` carries that same membership
over to `g2` (`List.Perm.mem_iff`); any OTHER name-matching entry `g2`
might independently find is forced equal to `fiber` by
`wellFormed_unique_name`, since both are `name`-matching members of
the same (permutation-equal, hence same-elements) list. -/
theorem find_perm_invariant {g1 g2 : Registry} (hperm : g1.Perm g2)
    (hnd1 : g1.namesNodup) (name : String) : g1.find name = g2.find name := by
  have hnd2 : g2.namesNodup := by
    unfold Registry.namesNodup at *
    exact hnd1.perm (hperm.map Prod.fst)
  unfold Registry.find
  cases hfind1 : g1.find? (fun p => p.1 == name) with
  | none =>
    cases hfind2 : g2.find? (fun p => p.1 == name) with
    | none => rfl
    | some fiber2 =>
      exfalso
      have hmem2 : fiber2 ∈ g2 := List.mem_of_find?_eq_some hfind2
      have hpred2 : fiber2.1 == name := (List.find?_eq_some_iff_append.mp hfind2).1
      have hmem1 : fiber2 ∈ g1 := (List.Perm.mem_iff hperm.symm).mp hmem2
      have hnone : ∀ x ∈ g1, ¬ (x.1 == name) = true := List.find?_eq_none.mp hfind1
      exact absurd hpred2 (hnone fiber2 hmem1)
  | some fiber1 =>
    have hmem1 : fiber1 ∈ g1 := List.mem_of_find?_eq_some hfind1
    have hpred1 : fiber1.1 == name := (List.find?_eq_some_iff_append.mp hfind1).1
    have hmem2 : fiber1 ∈ g2 := (List.Perm.mem_iff hperm).mp hmem1
    cases hfind2 : g2.find? (fun p => p.1 == name) with
    | none =>
      exfalso
      have hnone : ∀ x ∈ g2, ¬ (x.1 == name) = true := List.find?_eq_none.mp hfind2
      exact absurd hpred1 (hnone fiber1 hmem2)
    | some fiber2 =>
      have hmem2' : fiber2 ∈ g2 := List.mem_of_find?_eq_some hfind2
      have hpred2 : fiber2.1 == name := (List.find?_eq_some_iff_append.mp hfind2).1
      have hpq : fiber1.1 = fiber2.1 := (beq_iff_eq.mp hpred1).trans (beq_iff_eq.mp hpred2).symm
      have := wellFormed_unique_name hnd2 hmem2 hmem2' hpq
      rw [this]

/-- `coeffectContext`'s membership is permutation-invariant: `dep`
belongs to `g1.coeffectContext` iff it belongs to `g2.coeffectContext`,
for `Perm`-related registries -- proved via `Perm.filter` (the
`Active`-fiber sublist carries over as a `Perm` too) then
`Perm.flatMap` (flattening a `Perm`'d list of lists over the same `f`
yields a `Perm`'d result), and finally `Perm.mem_iff` to read off pure
membership from the resulting `Perm`. `satisfied` only ever tests
`.contains dep` on the coeffect context (a membership question), so
this membership-level fact is exactly what `to_obsEquiv` needs -- the
FULL list equality `hcoeffect` an earlier draft attempted is stronger
than necessary and not actually true in general (a `Perm`'d flatMap
result need not be `Eq`, only `Perm`). -/
theorem coeffectContext_mem_perm_invariant {g1 g2 : Registry} (h : RegistryEquiv g1 g2) (dep : String) :
    g1.coeffectContext.contains dep = g2.coeffectContext.contains dep := by
  unfold Registry.coeffectContext
  have hfp : (g1.filter (fun p => p.2.state == LifecycleState.active)).Perm
      (g2.filter (fun p => p.2.state == LifecycleState.active)) := h.filter _
  have hflat : ((g1.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)).Perm
      ((g2.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)) :=
    hfp.flatMap_right _
  simp only [List.contains_eq_mem, decide_eq_decide]
  exact hflat.mem_iff

theorem RegistryEquiv.to_obsEquiv {g1 g2 : Registry} (h : RegistryEquiv g1 g2)
    (hnd1 : g1.namesNodup) (A : List String) :
    ObsEquiv A g1 g2 := by
  intro name _
  unfold Registry.satisfied
  rw [find_perm_invariant h hnd1 name]
  cases hfind2 : g2.find name with
  | none => rfl
  | some fiber =>
    exact List.all_congr rfl (fun dep => coeffectContext_mem_perm_invariant h dep)
/-- Definition 39/Theorem 40 (commutative keys) closed by
`CommutativeKeys.lean`: `KeyOp` models a coeffect operation's generator
shape directly (reads/writes exactly one `String` key), and
`theorem40_commute`/`keyOp_reads_undisturbed_at_distinct_key` prove
Theorem 40 (operations at distinct keys are independent)
unconditionally, over `Independence.lean`'s own `Commuting`/`Independent`
vocabulary. Theorem 42 (coeffect-mediated effect functions built from
commutative-key operations are independent) is stated there as
`theorem42_of_generator_commutation`, taking generator-level commutation
as an explicit premise -- exactly how the paper's own proof reduces
Theorem 42 before it ever touches Definition 41's inductive coeffect-
mediated effect-function family, which this codebase's abstract `Gamma`-
level model has no counterpart for. -/
def commutativeKeysConnectionClosedByCommutativeKeysLean : Unit := ()

end Registry
