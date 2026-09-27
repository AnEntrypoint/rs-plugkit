import CordisCalculus.Basic

























universe u

namespace Cordis








inductive EffectIter (Γ : Type u) : Type u where
  | done (run : Γ → Γ × (Γ → Γ))
  | more (run : Γ → Γ × (Γ → Γ)) (next : EffectIter Γ)

namespace EffectIter

variable {Γ : Type u}


def stateMap : EffectIter Γ → Γ → Γ
  | .done run, γ => (run γ).1
  | .more run _, γ => (run γ).1


def inverse : EffectIter Γ → Γ → (Γ → Γ)
  | .done run, γ => (run γ).2
  | .more run _, γ => (run γ).2


def continuation : EffectIter Γ → Option (EffectIter Γ)
  | .done _ => none
  | .more _ next => some next


def length : EffectIter Γ → Nat
  | .done _ => 1
  | .more _ next => next.length + 1

theorem length_pos (i : EffectIter Γ) : 0 < i.length := by
  cases i <;> simp [length]





def Witnessed : EffectIter Γ → Prop
  | .done run => ∀ γ, (run γ).2 ((run γ).1) = γ
  | .more run next => (∀ γ, (run γ).2 ((run γ).1) = γ) ∧ next.Witnessed

theorem Witnessed.head {i : EffectIter Γ} (h : i.Witnessed) (γ : Γ) :
    i.inverse γ (i.stateMap γ) = γ := by
  cases i with
  | done run => exact h γ
  | more run next => exact h.1 γ

theorem Witnessed.tail {run : Γ → Γ × (Γ → Γ)} {next : EffectIter Γ}
    (h : (EffectIter.more run next).Witnessed) : next.Witnessed := h.2

end EffectIter









inductive Lifecycle (Γ : Type u) (View : Type u) (Error : Type u) : Type u where
  | inactive (outcome : Option Error)
  | reloading (remaining : EffectIter Γ) (accum : Γ → Γ) (view : View)
  | active (accum : Γ → Γ) (view : View)
  | unloading (accum : Γ → Γ) (view : View) (outcome : Option Error)

namespace Lifecycle

variable {Γ : Type u} {View : Type u} {Error : Type u}



def installed : Lifecycle Γ View Error → Prop
  | .inactive _ => False
  | _ => True



def failed : Lifecycle Γ View Error → Prop
  | .inactive (some _) => True
  | _ => False




def accumulator : Lifecycle Γ View Error → (Γ → Γ)
  | .inactive _ => id
  | .reloading _ g _ => g
  | .active g _ => g
  | .unloading g _ _ => g




def committedView : Lifecycle Γ View Error → Option View
  | .inactive _ => none
  | .reloading _ _ ω => some ω
  | .active _ ω => some ω
  | .unloading _ ω _ => some ω

theorem committedView_isSome_of_installed {θ : Lifecycle Γ View Error}
    (h : θ.installed) : (θ.committedView).isSome := by
  cases θ <;> simp_all [installed, committedView]





def providing : Lifecycle Γ View Error → Prop
  | .active _ _ => True
  | _ => False

theorem installed_of_providing {θ : Lifecycle Γ View Error}
    (h : θ.providing) : θ.installed := by
  cases θ <;> simp_all [providing, installed]


def inFlight : Lifecycle Γ View Error → Prop
  | .reloading _ _ _ => True
  | _ => False

end Lifecycle

end Cordis
