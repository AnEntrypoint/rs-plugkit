import CordisCalculus.Basic










namespace Registry









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






theorem filter_contains_eq_nil_iff (l1 l2 : List String) :
    l1.filter l2.contains = [] ↔ ∀ x ∈ l1, x ∉ l2 := by
  rw [List.filter_eq_nil_iff]
  simp






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








      rw [Bool.not_eq_true, List.any_eq_false] at hoverlap
      have hp_disjoint := (filter_contains_eq_nil_iff p.2.provides prov).mp (by
        have h1 := hoverlap p hp
        simpa using h1)
      rw [List.isEmpty_iff, filter_contains_eq_nil_iff]
      intro cap hcap hcap'
      exact hp_disjoint cap hcap hcap'







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
