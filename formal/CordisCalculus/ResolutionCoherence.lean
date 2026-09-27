import CordisCalculus.RecoveryGeneral





























universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}



def Lifecycle.reloadingView : Lifecycle Γ View Error → Option View
  | .reloading _ _ ω => some ω
  | _ => none





theorem reloading_entered_only_by_lBegin
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (hbefore : ¬ θ.inFlight) (hafter : θ'.inFlight) :
    s.rule = Rule.lBegin ∧ θ = Lifecycle.inactive none := by
  cases s <;> simp_all [Step.rule, Lifecycle.inFlight]






theorem iteration_runs_against_committed_view
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (hrule : s.rule = Rule.lIter ∨ s.rule = Rule.lFinish) :
    θ'.committedView = θ.committedView := by
  cases s <;> simp_all [Step.rule, Lifecycle.committedView]












theorem resolution_coherence_exit
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (hin : θ.inFlight) (hout : ¬ θ'.inFlight) (hinstalled : θ'.installed) :
    (s.rule = Rule.lFinish ∧ ∃ g ω, θ' = .active g ω ∧ θ.committedView = some ω)
      ∨ ((s.rule = Rule.lDivert ∨ s.rule = Rule.lRaise)
          ∧ ∃ g ω ζ, θ' = .unloading g ω ζ ∧ θ.committedView = some ω) := by
  cases s with
  | lFinish Ψ h i g ω hcont hwitness =>
      exact Or.inl ⟨rfl, g ∘ h, ω, rfl, rfl⟩
  | lDivertAbort i g ω =>
      exact Or.inr ⟨Or.inl rfl, g, ω, none, rfl, rfl⟩
  | lDivertLand Ψ h i g ω hwitness =>
      exact Or.inr ⟨Or.inl rfl, g ∘ h, ω, none, rfl, rfl⟩
  | lRaise i g ω ξ =>
      exact Or.inr ⟨Or.inr rfl, g, ω, some ξ, rfl, rfl⟩
  | oRetire θ => exact absurd hin hout
  | _ => simp_all [Lifecycle.inFlight, Lifecycle.installed]





theorem unloading_exits_only_by_lUnload
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    {g : Γ → Γ} {ω : View} {ζ : Option Error} (hθ : θ = .unloading g ω ζ)
    (hout : ¬ θ'.installed) :
    s.rule = Rule.lUnload ∧ Ψ = g ∧ θ' = .inactive ζ := by
  cases s <;> simp_all [Step.rule, Lifecycle.installed]











theorem terminal_recovery
    {θb : Lifecycle Γ View Error} {g : Γ → Γ} {ω : View} {ζ : Option Error}
    (ep : Episode Γ View Error θb (.unloading g ω ζ))
    (hindep : ep.OtherStepsIndependent) (γ : Γ) :
    g (ep.reachedState γ) = ep.otherComposite (θb.accumulator γ) :=
  recovery_exactness ep hindep γ











theorem divert_and_raise_agree_except_outcome
    {i : EffectIter Γ} {g : Γ → Γ} {ω : View} {ξ : Error}
    (_sDivert : Step Γ View Error (.reloading i g ω) (.unloading g ω none) id)
    (_sRaise : Step Γ View Error (.reloading i g ω) (.unloading g ω (some ξ)) id) :
    (Lifecycle.unloading g ω none : Lifecycle Γ View Error).accumulator
        = (Lifecycle.unloading g ω (some ξ) : Lifecycle Γ View Error).accumulator
      ∧ (Lifecycle.unloading g ω none : Lifecycle Γ View Error).committedView
        = (Lifecycle.unloading g ω (some ξ) : Lifecycle Γ View Error).committedView :=
  ⟨rfl, rfl⟩







theorem failure_leaves_nothing_stranded
    {θb : Lifecycle Γ View Error}
    {g : Γ → Γ} {ω : View} {ξ : Error}
    (epRaise : Episode Γ View Error θb (.unloading g ω (some ξ)))
    (epDivert : Episode Γ View Error θb (.unloading g ω none))
    (hindepR : epRaise.OtherStepsIndependent)
    (hindepD : epDivert.OtherStepsIndependent)
    (γ : Γ)
    (hreach : epRaise.reachedState γ = epDivert.reachedState γ) :
    epRaise.otherComposite (θb.accumulator γ)
      = epDivert.otherComposite (θb.accumulator γ) := by
  rw [← terminal_recovery epRaise hindepR γ, ← terminal_recovery epDivert hindepD γ,
      hreach]

end Cordis
