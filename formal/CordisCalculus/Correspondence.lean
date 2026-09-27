import CordisCalculus.FailureModel
import CordisCalculus.Transition





















namespace Cordis

variable {Xi : Type}






def Lifecycle.toExt : Lifecycle (List String) (List String) Xi → ExtLifecycle Xi
  | .inactive ζ => .inactive ζ
  | .reloading i _ ω => .reloading ω i.continuation.isSome
  | .active _ ω => .active ω
  | .unloading _ ω ζ => .unloading ω ζ




theorem toExt_installed_iff (θ : Lifecycle (List String) (List String) Xi) :
    θ.installed ↔ (match θ.toExt with | .inactive _ => False | _ => True) := by
  cases θ <;> simp [Lifecycle.installed, Lifecycle.toExt]


theorem toExt_failed_iff (θ : Lifecycle (List String) (List String) Xi) :
    θ.failed ↔ ∃ xi : Xi, θ.toExt = .inactive (some xi) := by
  cases θ with
  | inactive ζ =>
      cases ζ <;> simp [Lifecycle.failed, Lifecycle.toExt]
  | reloading i g ω => simp [Lifecycle.failed, Lifecycle.toExt]
  | active g ω => simp [Lifecycle.failed, Lifecycle.toExt]
  | unloading g ω ζ => simp [Lifecycle.failed, Lifecycle.toExt]






theorem step_lBegin_toExt {θ θ' : Lifecycle (List String) (List String) Xi}
    (s : Step (List String) (List String) Xi θ θ' (id : List String → List String))
    (h : s.rule = Rule.lBegin) :
    θ.toExt = .inactive none ∧
      ∃ (i : EffectIter (List String)) (ω : List String),
        θ'.toExt = .reloading ω i.continuation.isSome := by
  cases s <;> simp_all [Step.rule, Lifecycle.toExt]
  case lBegin e ω => exact ⟨e, rfl⟩








theorem step_toExt_matches_rule
    {θ θ' : Lifecycle (List String) (List String) Xi}
    (s : Step (List String) (List String) Xi θ θ' (id : List String → List String)) :
    (s.rule = Rule.lRaise →
      ∃ (committed : List String) (moreIterations : Bool) (ξ : Xi),
        θ.toExt = .reloading committed moreIterations ∧
        θ'.toExt = .unloading committed (some ξ)) ∧
    (s.rule = Rule.lLeave →
      ∃ committed : List String,
        θ.toExt = .active committed ∧
        θ'.toExt = .unloading committed none) ∧
    (s.rule = Rule.lUnload →
      ∃ (committed : List String) (ζ : Option Xi),
        θ.toExt = .unloading committed ζ ∧
        θ'.toExt = .inactive ζ) := by
  refine ⟨?_, ?_, ?_⟩ <;> intro h <;> cases s <;> simp_all [Step.rule, Lifecycle.toExt]







theorem toExt_raise_sole_error_source
    {θ θ' : Lifecycle (List String) (List String) Xi}
    (s : Step (List String) (List String) Xi θ θ' (id : List String → List String))
    (h : s.rule ≠ Rule.lRaise) :
    ∀ committed : List String, ∀ ξ : Xi,
      θ'.toExt = .unloading committed (some ξ) →
      θ.toExt = .unloading committed (some ξ) := by
  intro committed ξ hpost
  cases s <;> simp_all [Step.rule, Lifecycle.toExt]







theorem toExt_no_reentry_from_failed
    {θ θ' : Lifecycle (List String) (List String) Xi} {Ψ : List String → List String}
    (s : Step (List String) (List String) Xi θ θ' Ψ)
    (h : s.rule = Rule.lBegin) :
    ¬ ∃ xi : Xi, θ.toExt = .inactive (some xi) := by
  have hnf := Step.not_failed_of_lBegin s h
  intro ⟨xi, hxi⟩
  exact hnf ((toExt_failed_iff θ).mpr ⟨xi, hxi⟩)

end Cordis
