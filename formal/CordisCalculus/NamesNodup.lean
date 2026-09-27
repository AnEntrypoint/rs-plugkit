import CordisCalculus.ObservationalEquivalence
















namespace Registry













theorem map_fst_fixed (r : Registry) (f : String × Fiber → String × Fiber)
    (hfix : ∀ p, (f p).1 = p.1) :
    (r.map f).map Prod.fst = r.map Prod.fst := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons, hfix hd, ih]



theorem retire_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.retire name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold retire at h
  split at h
  · injection h with h
    subst h
    unfold namesNodup
    rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
    exact hnodup
  · contradiction


theorem reload_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.reload name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold reload at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
      exact hnodup
    · contradiction
  · contradiction


theorem unload_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold unload at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
      exact hnodup
    · contradiction
  · contradiction






theorem remove_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.remove name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold remove at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      exact List.Nodup.sublist (List.Sublist.map Prod.fst List.filter_sublist) hnodup
    · contradiction
  · contradiction








theorem insert_preserves_namesNodup (r : Registry) (name : String) (req prov : List String) (r' : Registry)
    (h : r.insert name req prov = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold insert at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · injection h with h
      subst h
      rename_i hcontains _
      unfold namesNodup
      rw [List.map_append]
      simp only [List.map_cons, List.map_nil]
      rw [List.nodup_append]
      have hsingle : List.Nodup [name] := by unfold List.Nodup; simp
      refine ⟨hnodup, hsingle, ?_⟩
      intro x hx b hb
      have hbname : b = name := List.mem_singleton.mp hb
      subst hbname
      intro hxname
      subst hxname
      rw [Bool.not_eq_true, Bool.eq_false_iff] at hcontains
      apply hcontains
      unfold contains
      rw [List.any_eq_true]
      obtain ⟨p, hpmem, hpeq⟩ := List.mem_map.mp hx
      exact ⟨p, hpmem, by simp [hpeq]⟩










theorem namesNodup_preservation (r : Registry) (hnodup : r.namesNodup) :
    (∀ name req prov r', r.insert name req prov = some r' → r'.namesNodup) ∧
    (∀ name r', r.retire name = some r' → r'.namesNodup) ∧
    (∀ name r', r.remove name = some r' → r'.namesNodup) ∧
    (∀ name r', r.reload name = some r' → r'.namesNodup) ∧
    (∀ name r', r.unload name = some r' → r'.namesNodup) :=
  ⟨fun name req prov r' h => insert_preserves_namesNodup r name req prov r' h hnodup,
   fun name r' h => retire_preserves_namesNodup r name r' h hnodup,
   fun name r' h => remove_preserves_namesNodup r name r' h hnodup,
   fun name r' h => reload_preserves_namesNodup r name r' h hnodup,
   fun name r' h => unload_preserves_namesNodup r name r' h hnodup⟩







theorem empty_namesNodup : (empty : Registry).namesNodup := List.nodup_nil

end Registry
