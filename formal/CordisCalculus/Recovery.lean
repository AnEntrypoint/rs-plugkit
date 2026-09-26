import CordisCalculus.Basic












namespace Registry




theorem unload_preserves_fields (r : Registry) (name : String) (r' : Registry) (fiber : Fiber)
    (hfind : r.find name = some fiber) (h : r.unload name = some r') :
    r'.find name = some { fiber with state := .inactive } := by
  unfold unload at h
  rw [hfind] at h
  simp only at h
  by_cases hcond : fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)
  · simp only [hcond, if_true] at h
    injection h with h
    subst h
    rw [find_map_update r name (fun f => { f with state := .inactive }), hfind]
    rfl
  · rw [Bool.not_eq_true] at hcond
    rw [hcond] at h
    simp at h





theorem unload_reload_recovers_exactly (r : Registry) (name : String) (r' r'' : Registry) (fiber : Fiber)
    (hfind : r.find name = some fiber) (hnotretired : ¬ fiber.retired)
    (hunload : r.unload name = some r') (hsat' : r'.satisfied name = true)
    (hreload : r'.reload name = some r'') :
    r''.find name = some { fiber with state := .active } := by
  have hfind' : r'.find name = some { fiber with state := .inactive } :=
    unload_preserves_fields r name r' fiber hfind hunload
  unfold reload at hreload
  rw [hfind'] at hreload
  simp only at hreload
  have hcond : (({ fiber with state := .inactive } : Fiber).state == LifecycleState.inactive
      && !({ fiber with state := .inactive } : Fiber).retired && r'.satisfied name) = true := by
    simp only [Bool.and_eq_true, beq_iff_eq]
    refine ⟨⟨by decide, ?_⟩, hsat'⟩
    simpa using hnotretired
  rw [hcond] at hreload
  simp only [if_true] at hreload
  injection hreload with hreload
  subst hreload
  rw [find_map_update r' name (fun f => { f with state := .active }), hfind']
  rfl

end Registry
