import CordisCalculus.Transition






















universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}





inductive EpisodeStep (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → Type u where


  | self {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
      (s : Step Γ View Error θ θ' Ψ)
      (interiorBefore : θ.installed) (interiorAfter : θ'.installed) :
      EpisodeStep Γ View Error θ θ'




  | other {θ : Lifecycle Γ View Error} (Ψ : Γ → Γ) : EpisodeStep Γ View Error θ θ

namespace EpisodeStep



def stateMap {θ θ' : Lifecycle Γ View Error} :
    EpisodeStep Γ View Error θ θ' → (Γ → Γ)
  | .self (Ψ := Ψ) _ _ _ => Ψ
  | .other Ψ => Ψ



def actsOnOther {θ θ' : Lifecycle Γ View Error} :
    EpisodeStep Γ View Error θ θ' → Bool
  | .self _ _ _ => false
  | .other _ => true

end EpisodeStep




inductive Episode (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → Type u where
  | nil {θ : Lifecycle Γ View Error} : Episode Γ View Error θ θ
  | cons {θ θ' θ'' : Lifecycle Γ View Error}
      (s : EpisodeStep Γ View Error θ θ') (rest : Episode Γ View Error θ' θ'') :
      Episode Γ View Error θ θ''

namespace Episode




def otherComposite {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → (Γ → Γ)
  | .nil => id
  | .cons s rest =>
    if s.actsOnOther then rest.otherComposite ∘ s.stateMap
    else rest.otherComposite



def reachedState {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → (Γ → Γ)
  | .nil => id
  | .cons s rest => rest.reachedState ∘ s.stateMap

end Episode






def IndependentOf (g : Γ → Γ) (Ψ : Γ → Γ) : Prop :=
  ∀ γ, g (Ψ γ) = Ψ (g γ)




def Episode.OtherStepsIndependent {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → Prop
  | .nil => True
  | .cons s rest =>
    (s.actsOnOther = true →
      ∀ g : Γ → Γ, IndependentOf g s.stateMap) ∧ rest.OtherStepsIndependent

end Cordis
