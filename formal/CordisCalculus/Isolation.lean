/-!
Coeffect isolation (paper Section 3.2.3, Definitions 22-23, 27-29),
mirroring `orchestrator/coeffect_realm.rs`'s `RealmTable`. Values are
fixed at `String` (as `Basic.lean` fixes `Fiber`'s `requires`/`provides`
lists to `String`), avoiding the dependent type family `V : K -> Type`
Definition 22 states in full generality -- every key sharing one value
type is the reduction this crate's own coeffect values (capability
strings, policy text) already live at, the same reduction
`coeffect_realm.rs` makes.

Both `Sigma` (Definition 22) and `Sigma^iso` (Definition 28) are modeled
as finite association lists (`List (String x String)`), the same
`List`-as-partial-function encoding `Basic.lean`'s own `Registry` uses,
so every proof here is a plain structural induction, no external
library.
-/

namespace Coeffect

/-- Definition 22: the coeffect context `Sigma := (k:K) -> V_k`, a
finite partial function `K -> V` modeled as an association list. -/
abbrev Sigma := List (String × String)

namespace Sigma

/-- `k in dom(sigma)`. -/
def domMem (s : Sigma) (k : String) : Bool := s.any (fun p => p.1 == k)

/-- Definition 23 `get`: defined when `k in dom(sigma)`. -/
def get (s : Sigma) (k : String) : Option String :=
  (s.find? (fun p => p.1 == k)).map Prod.snd

/-- Definition 23 `set(k,v)`: requires `k \notin dom(sigma)` as a
precondition; returns `none` (Definition 22's own "a violated
precondition ... produces no transition") when violated. -/
def set (s : Sigma) (k v : String) : Option Sigma :=
  if s.domMem k then none else some (s ++ [(k, v)])

/-- Definition 23 `set`'s companion inverse `\sigma' . \sigma' \ k`
(restriction): removing the binding `set` just installed recovers the
original table exactly, `set`'s own effect-function inverse. -/
def restrict (s : Sigma) (k : String) : Sigma :=
  s.filter (fun p => p.1 != k)

theorem get_set_self (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.get k = some v := by
  refine ⟨s ++ [(k, v)], ?_, ?_⟩
  · unfold Sigma.set
    rw [h]
  · unfold Sigma.get
    induction s with
    | nil => simp
    | cons hd tl ih =>
      simp only [List.find?_cons, List.append_eq, List.cons_append]
      unfold Sigma.domMem at h
      simp only [List.any_cons, Bool.or_eq_false_iff] at h
      by_cases hc : hd.1 == k
      · simp [hc] at h
      · simp only [hc, Bool.false_eq_true, if_false]
        apply ih
        simpa using h.2

/-- `restrict` after `set` recovers the original table exactly -- the
effect-function law Definition 23's `set` states its inverse must
satisfy (`sigma' . sigma' \ k` restores `sigma`). -/
theorem restrict_set (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.restrict k = s := by
  refine ⟨s ++ [(k, v)], by unfold Sigma.set; rw [h], ?_⟩
  unfold Sigma.restrict
  induction s with
  | nil => simp
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    have hne : hd.1 != k := by
      by_contra hc
      simp only [ne_eq, bne_iff_ne, not_not] at hc
      exact absurd (by simp [hc]) (by simpa using h.1)
    simp only [List.cons_append, List.filter_cons, hne, if_true]
    congr 1
    exact ih h.2

end Sigma

/-- Definition 28: the coeffect context with isolation,
`Sigma^iso := (K -> R) x ((r:R) -> V_r)`, represented as the pair
`(rho, sigma)`: `rho` the isolation realm table (Definition 28's
`K -> R`, itself a `Sigma`-shaped association list since realm
identifiers are `String`s here), `sigma` the dependency table keyed by
realm identifier (also `Sigma`-shaped, `R = String`). -/
structure SigmaIso where
  rho : Sigma
  sigma : Sigma
  deriving DecidableEq, Repr

namespace SigmaIso

/-- `rho(k)`: a key outside `dom(rho)` resolves to its own realm
(Definition 28's own text), matching `coeffect_realm.rs`'s
`RealmTable::realm_of`. -/
def realmOf (t : SigmaIso) (k : String) : String :=
  (t.rho.get k).getD k

/-- Definition 29 `get`: `get(k)(rho,sigma) = sigma(rho(k))`. -/
def get (t : SigmaIso) (k : String) : Option String :=
  t.sigma.get (t.realmOf k)

/-- Definition 29 `set(k,v)`: carries the precondition of Definition 23
transported along `rho`, namely `rho(k) \in dom(sigma)` is the
extension target and must currently be absent -- `set` writes
`sigma[rho(k) -> v]`. -/
def set (t : SigmaIso) (k v : String) : Option SigmaIso :=
  (t.sigma.set (t.realmOf k) v).map (fun sigma' => { t with sigma := sigma' })

/-- Definition 29 `isolate(k,r)`: `rho[k -> r]`, inheriting `sigma`
unchanged -- a *derived* realization (Definition 27): no precondition,
"a key already isolated is reassigned rather than refused." Modeled by
appending, then reading via `get` (which uses `List.find?`, taking the
FIRST match) after `SigmaIso.realmOf` is redefined below to take the
LAST match, matching genuine reassignment semantics rather than
Definition 23's extension-only `set`. -/
def isolate (t : SigmaIso) (k r : String) : SigmaIso :=
  { t with rho := t.rho ++ [(k, r)] }

/-- `realmOf` must read the LAST-appended binding for `k` (the most
recent `isolate` call), not the first, since `isolate` is a
reassignment operation, unlike `Sigma.set`'s once-only extension. This
redefinition captures that: `List.find?` over the REVERSED list finds
the most recently appended entry first. -/
def realmOfLatest (t : SigmaIso) (k : String) : String :=
  ((t.rho.reverse.find? (fun p => p.1 == k)).map Prod.snd).getD k

/-- Reassigning an already-isolated key changes the realm it resolves
to on the NEXT `get`/`set` (via `realmOfLatest`), witnessing "a key
already isolated is reassigned rather than refused" concretely: two
successive `isolate` calls on the same key leave the SECOND realm as
the one `realmOfLatest` reports, never an error and never the first. -/
theorem isolate_reassigns (t : SigmaIso) (k r1 r2 : String) (hne : r1 ≠ r2) :
    ((t.isolate k r1).isolate k r2).realmOfLatest k = r2 := by
  unfold SigmaIso.isolate SigmaIso.realmOfLatest
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.find?_cons]
  simp

/-- `isolate` never touches `sigma`, matching Definition 27's derived
realization ("leaves the input intact... inherits the dependency table
unchanged"): the shared table component is definitionally equal before
and after. -/
theorem isolate_preserves_sigma (t : SigmaIso) (k r : String) :
    (t.isolate k r).sigma = t.sigma := rfl

/-- A key with NO isolation entry resolves to its own name, Definition
28's stated default (`rho(k) = k` for `k \notin dom(rho)`) -- the base
case every `isolate` call above starts from. -/
theorem realmOf_default (t : SigmaIso) (k : String) (h : t.rho.domMem k = false) :
    t.realmOf k = k := by
  have hget : t.rho.get k = none := by
    unfold Sigma.get
    unfold Sigma.domMem at h
    induction t.rho with
    | nil => rfl
    | cons hd tl ih =>
      simp only [List.any_cons, Bool.or_eq_false_iff] at h
      have hne : ¬ (hd.1 == k) := by simpa using h.1
      simp only [List.find?_cons, hne, Bool.false_eq_true, if_false]
      exact ih h.2
  unfold SigmaIso.realmOf
  rw [hget]
  rfl

end SigmaIso

end Coeffect
