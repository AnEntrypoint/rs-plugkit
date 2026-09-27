import CordisCalculus.Basic











namespace Registry







theorem unload_only_on_lost_target (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') :
    ∃ fiber, r.find name = some fiber ∧ fiber.state = .active ∧ (fiber.retired ∨ ¬ r.satisfied name = true) := by
  unfold unload at h
  cases hfind : r.find name with
  | none => rw [hfind] at h; simp at h
  | some fiber =>
    rw [hfind] at h
    simp only at h
    by_cases hcond : fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)
    · have hactive : fiber.state = .active := by
        have h1 := (Bool.and_eq_true .. |>.mp hcond).1
        simpa using h1
      have hlost : fiber.retired ∨ ¬ r.satisfied name = true := by
        have h2 := (Bool.and_eq_true .. |>.mp hcond).2
        rcases Bool.or_eq_true .. |>.mp h2 with hc | hc
        · left; simpa using hc
        · right; simpa using hc
      refine ⟨fiber, ?_, hactive, hlost⟩
      rfl
    · exfalso
      rw [Bool.not_eq_true] at hcond
      rw [hcond] at h
      simp at h








theorem unload_refuses_satisfied_non_retired (r : Registry) (name : String) (fiber : Fiber)
    (hfind : r.find name = some fiber) (hactive : fiber.state = .active)
    (hnotretired : ¬ fiber.retired) (hsat : r.satisfied name = true) :
    r.unload name = none := by
  unfold unload
  rw [hfind]
  simp only
  have hguard : (fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)) = false := by
    rw [hactive]
    simp only [beq_self_eq_true, Bool.true_and]
    rw [Bool.eq_false_iff]
    intro hc
    rw [Bool.or_eq_true] at hc
    rcases hc with hc | hc
    · exact hnotretired hc
    · rw [Bool.not_eq_true'] at hc
      rw [hc] at hsat
      exact absurd hsat (by decide)
  rw [hguard]
  rfl




theorem unload_result_is_inactive (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') :
    ∃ fiber, r'.find name = some fiber ∧ fiber.state = .inactive := by
  unfold unload at h
  cases hfind : r.find name with
  | none => rw [hfind] at h; simp at h
  | some fiber =>
    rw [hfind] at h
    simp only at h
    by_cases hcond : fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)
    · simp only [hcond, if_true] at h
      injection h with h
      subst h
      refine ⟨{ fiber with state := .inactive }, ?_, rfl⟩
      rw [find_map_update r name (fun f => { f with state := .inactive }), hfind]
      rfl
    · rw [Bool.not_eq_true] at hcond
      rw [hcond] at h
      simp at h




























































































































































end Registry
