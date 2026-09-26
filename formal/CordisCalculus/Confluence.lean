import CordisCalculus.Basic













namespace Registry





theorem map_update_comm (r : Registry) (a b : String) (hab : a ≠ b) (updA updB : Fiber → Fiber) :
    (r.map (fun p => if p.1 == a then (p.1, updA p.2) else p)).map
        (fun p => if p.1 == b then (p.1, updB p.2) else p)
      = (r.map (fun p => if p.1 == b then (p.1, updB p.2) else p)).map
        (fun p => if p.1 == a then (p.1, updA p.2) else p) := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons]
    by_cases hha : hd.1 == a
    · have hha' : hd.1 = a := by simpa using hha
      have hhb : (hd.1 == b) = false := by
        simp only [beq_eq_false_iff_ne, ne_eq]
        rw [hha']; exact hab
      simp only [hha, if_true, hhb, Bool.false_eq_true, if_false, ih]
    · by_cases hhb : hd.1 == b
      · simp only [hha, Bool.false_eq_true, if_false, hhb, if_true, ih]
      · simp only [hha, Bool.false_eq_true, if_false, hhb, ih]






theorem contains_map_update_name (r : Registry) (target other : String) (upd : Fiber → Fiber) :
    Registry.contains (r.map (fun p => if p.1 == target then (p.1, upd p.2) else p)) other = Registry.contains r other := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons]
    unfold Registry.contains at *
    simp only [List.any_cons]
    by_cases hh : hd.1 == target
    · simp only [hh, if_true, ih]
    · simp only [hh, Bool.false_eq_true, if_false, ih]








theorem retire_retire_comm (r : Registry) (a b : String) (hab : a ≠ b)
    (r1 r2 r1' r2' : Registry)
    (h1 : r.retire a = some r1) (h1' : r1.retire b = some r1')
    (h2 : r.retire b = some r2) (h2' : r2.retire a = some r2') :
    r1' = r2' := by
  unfold Registry.retire at h1 h1' h2 h2'
  by_cases hca : Registry.contains r a
  · by_cases hcb : Registry.contains r b
    · simp only [hca, if_true] at h1
      injection h1 with h1; subst h1
      simp only [hcb, if_true] at h2
      injection h2 with h2; subst h2
      have hcb1 : Registry.contains (r.map (fun p => if p.1 == a then (p.1, { p.2 with retired := true }) else p)) b = true :=
        (contains_map_update_name r a b (fun f => { f with retired := true })).trans hcb
      have hca2 : Registry.contains (r.map (fun p => if p.1 == b then (p.1, { p.2 with retired := true }) else p)) a = true :=
        (contains_map_update_name r b a (fun f => { f with retired := true })).trans hca
      simp only [hcb1, if_true] at h1'
      injection h1' with h1'
      simp only [hca2, if_true] at h2'
      injection h2' with h2'
      rw [← h1', ← h2']
      exact map_update_comm r a b hab (fun f => { f with retired := true }) (fun f => { f with retired := true })
    · exfalso
      simp only [hcb, Bool.false_eq_true, if_false] at h2
      exact absurd h2 (by simp)
  · exfalso
    simp only [hca, Bool.false_eq_true, if_false] at h1
    exact absurd h1 (by simp)

end Registry
