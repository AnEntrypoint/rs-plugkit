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

/-- A key absent from `s` (per `domMem`) is never found by `find?`
searching for that same key -- the bridge every proof below needs
between the `Bool`-valued `domMem`/`any` and `find?`'s own search,
proved once by structural induction rather than re-derived at each use
site. -/
theorem find_none_of_domMem_false (s : Sigma) (k : String) (h : s.domMem k = false) :
    s.find? (fun p => p.1 == k) = none := by
  induction s with
  | nil => rfl
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    simp only [List.find?_cons, h.1]
    exact ih h.2

/-- `find?` over `s ++ [(k, v)]` where `s` does not itself contain `k`
locates the appended pair: search fails all the way through `s` (via
`find_none_of_domMem_false`), then matches the freshly appended entry. -/
theorem find_append_singleton (s : Sigma) (k v : String) (h : s.domMem k = false) :
    (s ++ [(k, v)]).find? (fun p => p.1 == k) = some (k, v) := by
  induction s with
  | nil => simp
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    simp only [List.cons_append, List.find?_cons, h.1]
    exact ih h.2

theorem get_set_self (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.get k = some v := by
  refine ⟨s ++ [(k, v)], ?_, ?_⟩
  · unfold Sigma.set
    simp [h]
  · unfold Sigma.get
    rw [find_append_singleton s k v h]
    rfl

/-- `restrict` over `s ++ [(k, v)]` where `s` does not contain `k`
drops exactly the appended entry, since `filter`'s predicate `p.1 != k`
keeps every entry of `s` (none of them equal `k`) and discards the
appended `(k, v)`. -/
theorem restrict_append_singleton (s : Sigma) (k v : String) (h : s.domMem k = false) :
    (s ++ [(k, v)]).restrict k = s := by
  unfold Sigma.restrict
  induction s with
  | nil => simp
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    have hne : hd.1 != k := by
      simp only [bne_iff_ne]
      intro heq
      rw [heq] at h
      simp at h
    simp only [List.cons_append, List.filter_cons, hne, if_true]
    congr 1
    exact ih h.2

/-- `restrict` after `set` recovers the original table exactly -- the
effect-function law Definition 23's `set` states its inverse must
satisfy (`sigma' . sigma' \ k` restores `sigma`). -/
theorem restrict_set (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.restrict k = s := by
  refine ⟨s ++ [(k, v)], ?_, restrict_append_singleton s k v h⟩
  unfold Sigma.set
  simp [h]

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

/-- `rho(k)`, reading the LAST-appended binding for `k` (the most
recent `isolate` call): `isolate` is a reassignment operation, unlike
`Sigma.set`'s once-only extension, so resolution must prefer the
newest entry. `List.find?` over the REVERSED list finds the most
recently appended entry first. A key outside `dom(rho)` resolves to
its own realm (Definition 28's own text), matching
`coeffect_realm.rs`'s `RealmTable::realm_of`. -/
def realmOf (t : SigmaIso) (k : String) : String :=
  ((t.rho.reverse.find? (fun p => p.1 == k)).map Prod.snd).getD k

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
"a key already isolated is reassigned rather than refused." -/
def isolate (t : SigmaIso) (k r : String) : SigmaIso :=
  { t with rho := t.rho ++ [(k, r)] }

/-- Reassigning an already-isolated key changes the realm it resolves
to on the NEXT `get`/`set`, witnessing "a key already isolated is
reassigned rather than refused" concretely: two successive `isolate`
calls on the same key leave the SECOND realm as the one `realmOf`
reports, never an error and never the first. -/
theorem isolate_reassigns (t : SigmaIso) (k r1 r2 : String) :
    ((t.isolate k r1).isolate k r2).realmOf k = r2 := by
  unfold SigmaIso.isolate SigmaIso.realmOf
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
theorem realmOf_default (t : SigmaIso) (k : String) (h : Sigma.domMem t.rho k = false) :
    t.realmOf k = k := by
  have hrev : Sigma.domMem t.rho.reverse k = false := by
    unfold Sigma.domMem at h ⊢
    rw [List.any_reverse]
    exact h
  unfold SigmaIso.realmOf
  rw [Sigma.find_none_of_domMem_false t.rho.reverse k hrev]
  rfl

/-- Two keys isolated into different realms via `isolate` resolve
independently: isolating `k1` never disturbs what a DIFFERENT key `k2`
already resolves to (a real multi-tenant scenario -- isolating a
capability for one component's realm must not perturb another
component's already-isolated realm for a different key). -/
theorem isolate_distinct_keys_independent (t : SigmaIso) (k1 k2 r : String) (hne : k1 ≠ k2) :
    (t.isolate k1 r).realmOf k2 = t.realmOf k2 := by
  unfold SigmaIso.isolate SigmaIso.realmOf
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.find?_cons]
  have : (k1 == k2) = false := by
    simp only [beq_eq_false_iff_ne]
    exact hne
  simp [this]

end SigmaIso

end Coeffect
