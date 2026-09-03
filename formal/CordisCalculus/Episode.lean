import CordisCalculus.Transition

/-!
Paper Definition 53 and Section 4.4.2: an episode of a fiber `n` is a
maximal interval of indices throughout which `installed_n` holds. It opens
at an L-Begin and, where it closes, closes at an L-Unload (Lemma 54(4)).

Theorem 61 is an induction over such an interval, so the interval is what
this file makes into a Lean object. An `Episode` is the list of steps the
fiber `n` and the other fibers take between the L-Begin that opens it and
the index the theorem is read at, each step carrying its own `Ψ` and
either acting on `n` (a lifecycle transition of `n` itself) or acting on
some other fiber (a `Ψ` drawn from that fiber's own effect monoid, which
the independence hypothesis lets Theorem 61 commute past `n`'s
accumulator).

The paper's `≈` compares two states on everything but the control fields.
Since a `Step` here carries the control fields in its own indices and the
ambient context only through `Ψ`, `≈` on this model is equality on `Γ`,
and the theorems below are stated as honest equalities rather than
weakened to a relation with no content.
-/

universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}

/-- One entry of an episode: either a step `n` itself takes, or a step
some other fiber takes whose state map is `Ψ`. Definition 53's
`step^t = r(n)` is the first case and `step^t = r(m)` with `m ≠ n` the
second; Theorem 61's index set `t_1 < ⋯ < t_l` enumerates the second. -/
inductive EpisodeStep (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → Type u where
  /-- A step acting on `n`. Table 1 supplies both the lifecycle transition
  and the `Ψ` it applies. -/
  | self {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
      (s : Step Γ View Error θ θ' Ψ)
      (interiorBefore : θ.installed) (interiorAfter : θ'.installed) :
      EpisodeStep Γ View Error θ θ'
  /-- A step acting on some `m ≠ n`. `n`'s lifecycle state is untouched
  (Lemma 54, `θ_n` being written only where the step acts on `n`), and the
  ambient context moves by that fiber's own `Ψ`, which the independence
  hypothesis commutes past `n`'s accumulator. -/
  | other {θ : Lifecycle Γ View Error} (Ψ : Γ → Γ) : EpisodeStep Γ View Error θ θ

namespace EpisodeStep

/-- The state map an episode step applies to the ambient context,
Definition 53's `Ψ^t`. -/
def stateMap {θ θ' : Lifecycle Γ View Error} :
    EpisodeStep Γ View Error θ θ' → (Γ → Γ)
  | .self (Ψ := Ψ) _ _ _ => Ψ
  | .other Ψ => Ψ

/-- Whether the step acts on a fiber other than `n`; the index set
`t_1 < ⋯ < t_l` of Theorem 61 collects exactly these. -/
def actsOnOther {θ θ' : Lifecycle Γ View Error} :
    EpisodeStep Γ View Error θ θ' → Bool
  | .self _ _ _ => false
  | .other _ => true

end EpisodeStep

/-- An episode of `n`, from the state its opening L-Begin left it in to
the state it is read at. Definition 53's maximal interval `[b, u]`, as a
chain of `EpisodeStep`s. -/
inductive Episode (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → Type u where
  | nil {θ : Lifecycle Γ View Error} : Episode Γ View Error θ θ
  | cons {θ θ' θ'' : Lifecycle Γ View Error}
      (s : EpisodeStep Γ View Error θ θ') (rest : Episode Γ View Error θ' θ'') :
      Episode Γ View Error θ θ''

namespace Episode

/-- `Ψ^{t_l} ∘ ⋯ ∘ Ψ^{t_1}` restricted to the steps acting on fibers other
than `n`: the right-hand side of Theorem 61's equation (56), the state
those same steps would have produced from `γ^b`. -/
def otherComposite {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → (Γ → Γ)
  | .nil => id
  | .cons s rest =>
    if s.actsOnOther then rest.otherComposite ∘ s.stateMap
    else rest.otherComposite

/-- The state the whole episode actually reaches from `γ^b`, every step
included: `γ^u = (Ψ^{u-1} ∘ ⋯ ∘ Ψ^b)(γ^b)`, the left side's argument. -/
def reachedState {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → (Γ → Γ)
  | .nil => id
  | .cons s rest => rest.reachedState ∘ s.stateMap

end Episode

/-- The independence hypothesis of Definition 60, in the exact form
Theorem 61's proof consumes it: an accumulator `n` has built commutes with
the state map of any step another fiber takes. Definition 60's first
condition, `∀ f ∈ M(i), g ∈ M(j). f ∘ g ≃ g ∘ f`, read at the accumulator
`n` holds and the `Ψ` the other fiber contributes. -/
def IndependentOf (g : Γ → Γ) (Ψ : Γ → Γ) : Prop :=
  ∀ γ, g (Ψ γ) = Ψ (g γ)

/-- Every step another fiber takes during the episode is independent of
every accumulator `n` may hold. Definition 60's pairwise independence,
restricted to the pairs Theorem 61 actually reads. -/
def Episode.OtherStepsIndependent {θ θ' : Lifecycle Γ View Error} :
    Episode Γ View Error θ θ' → Prop
  | .nil => True
  | .cons s rest =>
    (s.actsOnOther = true →
      ∀ g : Γ → Γ, IndependentOf g s.stateMap) ∧ rest.OtherStepsIndependent

end Cordis
