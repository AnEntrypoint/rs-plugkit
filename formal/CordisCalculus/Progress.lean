import CordisCalculus.Basic















namespace Registry



theorem progress_reload (r : Registry) (name : String) (fiber : Fiber)
    (hfind : r.find name = some fiber)
    (hstate : fiber.state = .inactive) (hnotretired : ¬ fiber.retired)
    (hsat : r.satisfied name = true) :
    ∃ r', r.reload name = some r' := by
  unfold reload
  rw [hfind]
  simp only
  have hcond : (fiber.state == LifecycleState.inactive && !fiber.retired && r.satisfied name) = true := by
    simp only [Bool.and_eq_true, beq_iff_eq]
    exact ⟨⟨hstate, by simpa using hnotretired⟩, hsat⟩
  rw [hcond]
  simp only [if_true]
  exact ⟨_, rfl⟩



theorem progress_unload (r : Registry) (name : String) (fiber : Fiber)
    (hfind : r.find name = some fiber)
    (hstate : fiber.state = .active) (hlost : fiber.retired ∨ ¬ r.satisfied name = true) :
    ∃ r', r.unload name = some r' := by
  unfold unload
  rw [hfind]
  simp only
  have hcond : (fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)) = true := by
    simp only [Bool.and_eq_true, beq_iff_eq, Bool.or_eq_true]
    refine ⟨hstate, ?_⟩
    rcases hlost with hc | hc
    · left; simpa using hc
    · right; simpa using hc
  rw [hcond]
  simp only [if_true]
  exact ⟨_, rfl⟩




theorem progress_remove (r : Registry) (name : String) (fiber : Fiber)
    (hfind : r.find name = some fiber)
    (hstate : fiber.state = .inactive) (hretired : fiber.retired) :
    ∃ r', r.remove name = some r' := by
  unfold remove
  rw [hfind]
  simp only
  have hcond : (fiber.retired && fiber.state == LifecycleState.inactive) = true := by
    simp only [Bool.and_eq_true, beq_iff_eq]
    exact ⟨hretired, hstate⟩
  rw [hcond]
  simp only [if_true]
  exact ⟨_, rfl⟩







theorem progress (r : Registry) (name : String) (fiber : Fiber) (hfind : r.find name = some fiber) :
    (fiber.state = .inactive ∧ ¬ fiber.retired ∧ ¬ r.satisfied name = true) ∨
    (fiber.state = .active ∧ ¬ fiber.retired ∧ r.satisfied name = true) ∨
    (∃ r', r.reload name = some r') ∨
    (∃ r', r.unload name = some r') ∨
    (∃ r', r.remove name = some r') := by
  cases hstate : fiber.state with
  | inactive =>
    by_cases hret : fiber.retired
    · right; right; right; right
      exact progress_remove r name fiber hfind hstate hret
    · by_cases hsat : r.satisfied name = true
      · right; right; left
        exact progress_reload r name fiber hfind hstate hret hsat
      · left; exact ⟨rfl, hret, hsat⟩
  | active =>
    by_cases hret : fiber.retired
    · right; right; right; left
      exact progress_unload r name fiber hfind hstate (Or.inl hret)
    · by_cases hsat : r.satisfied name = true
      · right; left; exact ⟨rfl, hret, hsat⟩
      · right; right; right; left
        exact progress_unload r name fiber hfind hstate (Or.inr hsat)

end Registry
