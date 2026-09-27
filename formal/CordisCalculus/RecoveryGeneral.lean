import CordisCalculus.Episode



















universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}




abbrev Accum (Γ : Type u) := Γ → Γ







def StepInverseWitnessed {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
    (_ : Step Γ View Error θ θ' Ψ) (h : Γ → Γ) : Prop :=
  ∀ γ, h (Ψ γ) = γ













structure SelfStepRecovers {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
    (s : Step Γ View Error θ θ' Ψ) : Type u where
  inverse : Γ → Γ
  witnessed : ∀ γ, inverse (Ψ γ) = γ
  composes : ∀ γ, θ'.accumulator γ = θ.accumulator (inverse γ)









def selfStepRecovers_of_idStateMap
    {θ θ' : Lifecycle Γ View Error}
    (s : Step Γ View Error θ θ' (id : Γ → Γ))
    (haccum : θ'.accumulator = θ.accumulator) :
    SelfStepRecovers s :=
  { inverse := id
    witnessed := fun _ => rfl
    composes := fun γ => by rw [haccum]; rfl }













def selfStepRecovers {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
    (s : Step Γ View Error θ θ' Ψ)
    (hinterior : θ.installed) (hinterior' : θ'.installed) :
    SelfStepRecovers s := by
  cases s with
  | oInsert => exact absurd hinterior (by simp [Lifecycle.installed])
  | oRemove ζ => exact absurd hinterior (by simp [Lifecycle.installed])
  | oRetire θ => exact ⟨id, fun _ => rfl, fun γ => rfl⟩
  | lBegin e ω => exact absurd hinterior (by simp [Lifecycle.installed])
  | lUnload g ω ζ => exact absurd hinterior' (by simp [Lifecycle.installed])
  | lIter Ψ h next i g ω hcont hwitness =>
      exact ⟨h, hwitness, fun γ => rfl⟩
  | lFinish Ψ h i g ω hcont hwitness =>
      exact ⟨h, hwitness, fun γ => rfl⟩
  | lDivertLand Ψ h i g ω hwitness =>
      exact ⟨h, hwitness, fun γ => rfl⟩
  | lDivertAbort i g ω => exact ⟨id, fun _ => rfl, fun γ => rfl⟩
  | lRaise i g ω ξ => exact ⟨id, fun _ => rfl, fun γ => rfl⟩
  | lLeave g ω => exact ⟨id, fun _ => rfl, fun γ => rfl⟩



























theorem recovery_exactness
    {θb θu : Lifecycle Γ View Error}
    (ep : Episode Γ View Error θb θu)
    (hindep : ep.OtherStepsIndependent) (γ : Γ) :
    θu.accumulator (ep.reachedState γ) = ep.otherComposite (θb.accumulator γ) := by
  induction ep generalizing γ with
  | nil => rfl
  | @cons α β ωend s rest ih =>
    cases s with
    | @self _ Ψ st hin hin' =>
      have hrec := selfStepRecovers st hin hin'
      have hhead : β.accumulator (Ψ γ) = α.accumulator γ := by
        rw [hrec.composes (Ψ γ), hrec.witnessed γ]
      have := ih hindep.2 (Ψ γ)
      simpa [Episode.reachedState, Episode.otherComposite,
        EpisodeStep.stateMap, EpisodeStep.actsOnOther, Function.comp,
        hhead] using this
    | other Ψ =>
      have := ih hindep.2 (Ψ γ)
      have hcomm : α.accumulator (Ψ γ) = Ψ (α.accumulator γ) :=
        hindep.1 rfl α.accumulator γ
      simpa [Episode.reachedState, Episode.otherComposite,
        EpisodeStep.stateMap, EpisodeStep.actsOnOther, Function.comp,
        hcomm] using this

end Cordis
