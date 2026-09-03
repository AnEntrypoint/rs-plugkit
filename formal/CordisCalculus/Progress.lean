import CordisCalculus.Basic

/-!
Progress (paper Theorem 66): a fiber that is not already at rest --
not yet reconciled with its own target and retirement status -- always
has a legal rule application available. "At rest" for a single fiber
means: `Inactive` and not retired and NOT satisfied (nothing to do
until its `requires` is met), or `Active` and satisfied and not
retired (correctly running, nothing pending), or retired and removed
already (gone, nothing left to do). Every OTHER combination of
state/retired/satisfied is exactly the guard one of `reload`/`unload`/
`remove` checks, so a fiber outside those three rest states always
admits a move -- the calculus never gets stuck on a single component
mid-lifecycle.
-/

namespace Registry

/-- A fiber that is `Inactive`, not retired, and whose target IS
satisfied is not at rest: `reload` fires on it. -/
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

/-- A fiber that is `Active` and either retired or has lost satisfaction
is not at rest: `unload` fires on it. -/
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

/-- A fiber that is retired and already `Inactive` is not at rest while
it still occupies a registry slot: `remove` fires on it, clearing it
out entirely. -/
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

/-- Progress (Theorem 66), assembled: every fiber present in the
registry that is not in one of the three rest states (`Inactive` +
not-retired + unsatisfied; `Active` + not-retired + satisfied; already
removed) admits a legal rule application. Stated as a case split over
every reachable combination of `state`/`retired`/`satisfied`, covering
the full space -- there is no combination left over that gets stuck. -/
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
