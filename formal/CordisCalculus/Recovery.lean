import CordisCalculus.Basic

/-!
Recovery-exactness (paper Theorem 61): withdrawing a component
(`unload`) and later reinstating it (`reload`) once its target is
satisfied again recovers EXACTLY the fiber that existed before
withdrawal -- same `requires`, same `provides`, same `retired` flag,
differing only in `state`, which returns to `Active`. No information
about the component is lost or altered by a withdraw/reinstate round
trip; this is what makes `unload` a genuinely REVERTIBLE effect rather
than a destructive one.
-/

namespace Registry

/-- After a successful `unload`, the named fiber is unaffected in every
field except `state`, which is `Inactive`; this is the same content as
`unload_result_is_inactive` plus the field-preservation guarantee. -/
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

/-- Reloading a just-unloaded fiber, once its `requires` is satisfied
again in the withdrawn registry, restores it to `Active` with every
other field exactly as it was before the `unload` -- the recovery is
EXACT, not merely "some `Active` fiber reappeared with this name". -/
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
