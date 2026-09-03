import CordisCalculus.Basic

/-!
Preservation (paper Theorem 59, clause 2): applying any of the five base
rules to a well-formed registry produces a well-formed registry, proved
as an unbounded `theorem` -- for EVERY `Registry` of every length and
EVERY fiber name/capability list, not checked for finitely many
enumerated states the way `calculus-model-check` (in the Rust crate)
does. This is the artifact a bounded model check cannot be.
-/

namespace Registry

/-- `retire`/`remove`/`reload`/`unload` only ever change one entry's
`state`/`retired` field, or delete an entry outright -- none of them can
touch a `provides` list, so `wellFormed` (a predicate purely about
`provides` disjointness) survives every one of them trivially. This
lemma is proved once and reused by all four theorems below, rather than
by four separate case-by-case arguments -- the paper's own economy
(preservation is one property; the rules that cannot affect its subject
matter don't need bespoke reasoning per rule). -/
theorem map_preserves_wellFormed_of_provides_untouched
    (r : Registry) (f : String × Fiber → String × Fiber)
    (hname : ∀ p, (f p).1 = p.1)
    (hprov : ∀ p, (f p).2.provides = p.2.provides)
    (hwf : wellFormed r) :
    wellFormed (r.map f) := by
  unfold wellFormed at hwf ⊢
  rw [List.pairwise_map]
  apply hwf.imp
  intro p q hpq hne
  rw [hname p, hname q] at hne
  rw [hprov p, hprov q]
  exact hpq hne

theorem filter_preserves_wellFormed (r : Registry) (p : String × Fiber → Bool) (hwf : wellFormed r) :
    wellFormed (r.filter p) := by
  unfold wellFormed at hwf ⊢
  exact List.Pairwise.filter p hwf

/-- `filter l2.contains l1 = []` says every element of `l1` avoids `l2` --
the plain characterization of list disjointness this file needs, proved
once so `insert_preserves_wellFormed` states its core argument directly
instead of unwinding `List.filter`/`Bool.not`/`List.any` by hand at the
point of use. -/
theorem filter_contains_eq_nil_iff (l1 l2 : List String) :
    l1.filter l2.contains = [] ↔ ∀ x ∈ l1, x ∉ l2 := by
  rw [List.filter_eq_nil_iff]
  simp

/-- The state-update map every one of `retire`/`reload`/`unload` applies:
touches only the named entry's `Fiber`, and only via a field update that
never rewrites `provides` (that field is copied through unchanged by
Lean's `{ p.2 with ... }` structure-update syntax in every rule
definition). -/
def updateStateName (name : String) (upd : Fiber → Fiber) (p : String × Fiber) : String × Fiber :=
  if p.1 == name then (p.1, upd p.2) else p

theorem retire_preserves_wellFormed (r : Registry) (name : String) (r' : Registry)
    (h : r.retire name = some r') (hwf : wellFormed r) : wellFormed r' := by
  unfold retire at h
  split at h
  · injection h with h
    subst h
    apply map_preserves_wellFormed_of_provides_untouched r _ _ _ hwf
    · intro p; split <;> rfl
    · intro p; split <;> rfl
  · contradiction

theorem remove_preserves_wellFormed (r : Registry) (name : String) (r' : Registry)
    (h : r.remove name = some r') (hwf : wellFormed r) : wellFormed r' := by
  unfold remove at h
  split at h
  · split at h
    · injection h with h
      subst h
      exact filter_preserves_wellFormed r _ hwf
    · contradiction
  · contradiction

theorem reload_preserves_wellFormed (r : Registry) (name : String) (r' : Registry)
    (h : r.reload name = some r') (hwf : wellFormed r) : wellFormed r' := by
  unfold reload at h
  split at h
  · split at h
    · injection h with h
      subst h
      apply map_preserves_wellFormed_of_provides_untouched r _ _ _ hwf
      · intro p; split <;> rfl
      · intro p; split <;> rfl
    · contradiction
  · contradiction

theorem unload_preserves_wellFormed (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') (hwf : wellFormed r) : wellFormed r' := by
  unfold unload at h
  split at h
  · split at h
    · injection h with h
      subst h
      apply map_preserves_wellFormed_of_provides_untouched r _ _ _ hwf
      · intro p; split <;> rfl
      · intro p; split <;> rfl
    · contradiction
  · contradiction

/-- O-Insert is the one rule that CAN change `provides` (it adds a new
entry with a caller-chosen `provides`), so its preservation proof is the
substantive one: the rule's own second premise (the `if` branch that
returns `none` on any overlap) is EXACTLY the disjointness `wellFormed`
demands of the new entry against every existing one, so appending it
keeps the whole list pairwise-disjoint. -/
theorem insert_preserves_wellFormed (r : Registry) (name : String) (req prov : List String) (r' : Registry)
    (h : r.insert name req prov = some r') (hwf : wellFormed r) : wellFormed r' := by
  unfold insert at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · injection h with h
      subst h
      rename_i hcontains hoverlap
      unfold wellFormed at hwf ⊢
      rw [List.pairwise_append]
      refine ⟨hwf, List.pairwise_singleton _ _, ?_⟩
      intro p hp q hq _
      have hq' : q = (name, { requires := req, provides := prov, state := LifecycleState.inactive, retired := false }) :=
        List.mem_singleton.mp hq
      subst hq'
      -- `hoverlap` is the negation of "some existing fiber's provides
      -- meets `prov`" -- `List.any_eq_false` reads it as "every existing
      -- fiber's filtered-overlap is empty", the exact hypothesis
      -- O-Insert's guard checked before admitting this new entry. The
      -- goal asks the symmetric direction (does the new entry's
      -- `provides`, filtered against `p`'s, come up empty); both express
      -- the disjointness of the same two sets, so
      -- `filter_contains_eq_nil_iff`'s membership form closes it directly.
      rw [Bool.not_eq_true, List.any_eq_false] at hoverlap
      have hp_disjoint := (filter_contains_eq_nil_iff p.2.provides prov).mp (by
        have h1 := hoverlap p hp
        simpa using h1)
      rw [List.isEmpty_iff, filter_contains_eq_nil_iff]
      intro cap hcap hcap'
      exact hp_disjoint cap hcap hcap'

/-- Preservation (Theorem 59), stated once over ALL FIVE rules: whichever
one a caller applies to a well-formed registry, if it succeeds, the
result is well-formed. This is the single theorem
`calculus-model-check`'s bounded enumeration approximates for 75 states;
here it holds for every `Registry` of every length, every name, and
every capability list -- an unbounded, kernel-checked proof. -/
theorem preservation (r : Registry) (hwf : wellFormed r) :
    (∀ name req prov r', r.insert name req prov = some r' → wellFormed r') ∧
    (∀ name r', r.retire name = some r' → wellFormed r') ∧
    (∀ name r', r.remove name = some r' → wellFormed r') ∧
    (∀ name r', r.reload name = some r' → wellFormed r') ∧
    (∀ name r', r.unload name = some r' → wellFormed r') :=
  ⟨fun name req prov r' h => insert_preserves_wellFormed r name req prov r' h hwf,
   fun name r' h => retire_preserves_wellFormed r name r' h hwf,
   fun name r' h => remove_preserves_wellFormed r name r' h hwf,
   fun name r' h => reload_preserves_wellFormed r name r' h hwf,
   fun name r' h => unload_preserves_wellFormed r name r' h hwf⟩

end Registry
