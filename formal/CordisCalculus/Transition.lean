import CordisCalculus.Iterator
















universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}




inductive Rule where
  | oInsert | oRetire | oRemove
  | lBegin | lIter | lFinish | lDivert | lRaise | lLeave | lUnload
  deriving DecidableEq, Repr













inductive Step (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → (Γ → Γ) → Type u where



  | oInsert :
      Step Γ View Error (.inactive none) (.inactive none) id


  | oRetire (θ : Lifecycle Γ View Error) :
      Step Γ View Error θ θ id

  | oRemove (ζ : Option Error) :
      Step Γ View Error (.inactive ζ) (.inactive ζ) id



  | lBegin (e : EffectIter Γ) (ω : View) :
      Step Γ View Error (.inactive none) (.reloading e id ω) id



  | lIter (Ψ h : Γ → Γ) (next : EffectIter Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hcont : i.continuation = some next)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.reloading next (g ∘ h) ω)
        Ψ


  | lFinish (Ψ h : Γ → Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hcont : i.continuation = none)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.active (g ∘ h) ω)
        Ψ



  | lDivertAbort (i : EffectIter Γ) (g : Γ → Γ) (ω : View) :
      Step Γ View Error (.reloading i g ω) (.unloading g ω none) id




  | lDivertLand (Ψ h : Γ → Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.unloading (g ∘ h) ω none)
        Ψ



  | lRaise (i : EffectIter Γ) (g : Γ → Γ) (ω : View) (ξ : Error) :
      Step Γ View Error (.reloading i g ω) (.unloading g ω (some ξ)) id




  | lLeave (g : Γ → Γ) (ω : View) :
      Step Γ View Error (.active g ω) (.unloading g ω none) id



  | lUnload (g : Γ → Γ) (ω : View) (ζ : Option Error) :
      Step Γ View Error (.unloading g ω ζ) (.inactive ζ) g

namespace Step


def rule {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} :
    Step Γ View Error θ θ' Ψ → Rule
  | .oInsert => .oInsert
  | .oRetire _ => .oRetire
  | .oRemove _ => .oRemove
  | .lBegin _ _ => .lBegin
  | .lIter _ _ _ _ _ _ _ _ => .lIter
  | .lFinish _ _ _ _ _ _ _ => .lFinish
  | .lDivertAbort _ _ _ => .lDivert
  | .lDivertLand _ _ _ _ _ _ => .lDivert
  | .lRaise _ _ _ _ => .lRaise
  | .lLeave _ _ => .lLeave
  | .lUnload _ _ _ => .lUnload





theorem stateMap_eq_accumulator_iff_lUnload
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : s.rule = Rule.lUnload) : Ψ = θ.accumulator := by
  cases s <;> simp_all [rule, Lifecycle.accumulator]




theorem lBegin_of_not_installed_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : ¬ θ.installed) (h' : θ'.installed) : s.rule = Rule.lBegin := by
  cases s <;> simp_all [rule, Lifecycle.installed]




theorem lUnload_of_installed_not_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : θ.installed) (h' : ¬ θ'.installed) : s.rule = Rule.lUnload := by
  cases s <;> simp_all [rule, Lifecycle.installed]




theorem committedView_const_of_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : θ.installed) (h' : θ'.installed) :
    θ'.committedView = θ.committedView := by
  cases s <;> simp_all [Lifecycle.installed, Lifecycle.committedView]




theorem lFinish_of_not_providing_providing
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : ¬ θ.providing) (h' : θ'.providing) : s.rule = Rule.lFinish := by
  cases s <;> simp_all [rule, Lifecycle.providing]




theorem not_failed_of_lBegin
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : s.rule = Rule.lBegin) : ¬ θ.failed := by
  cases s <;> simp_all [rule, Lifecycle.failed]



theorem committedView_none_of_failed {θ : Lifecycle Γ View Error}
    (h : θ.failed) : θ.committedView = none := by
  cases θ with
  | inactive ζ => rfl
  | _ => simp_all [Lifecycle.failed]

end Step

end Cordis
