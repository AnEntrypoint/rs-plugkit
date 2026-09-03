import CordisCalculus.Episode

/-!
Paper Theorem 61 (recovery exactness) and Theorem 64 (resolution
coherence), proved over the NON-ATOMIC state space of Definition 49 --
`Reloading(i, g, ω)` and `Unloading(g, ω, ζ)` present as real states an
episode passes through, rather than collapsed into the single atomic
`reload`/`unload` pair `Basic.lean` models.

`Recovery.lean` proves the atomic special case: `unload` then `reload`
restores a fiber's fields exactly. That statement is about ONE step in
each direction. Theorem 61 is the general claim: at ANY point inside an
episode, however many iterations have run and however many other fibers
have moved in between, applying `n`'s accumulator to the current state
yields exactly the state those other fibers' steps alone would have
produced from where the episode opened. The accumulator is built one
iteration at a time by L-Iter, L-Finish, and a landing L-Divert, and
Definition 51's witness condition is what makes each composition exact.
-/

universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}

/-- The accumulator a lifecycle state carries is what L-Unload will apply,
Table 1's `Ψ = g` row. Theorem 61's induction tracks this field across the
episode, so the invariant it maintains is stated about it directly. -/
abbrev Accum (Γ : Type u) := Γ → Γ

/-- Definition 51's witness condition, lifted to the exact obligation each
step of Theorem 61's induction discharges: the inverse a step contributes
undoes that step's own state map. Table 1 gives L-Iter, L-Finish, and a
landing L-Divert an `h` drawn from the iterator (whose witness condition
supplies this), and gives L-Leave, L-Raise, an aborting L-Divert, and an
O-Retire `h = id` with `Ψ = id` (for which it holds trivially). -/
def StepInverseWitnessed {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
    (_ : Step Γ View Error θ θ' Ψ) (h : Γ → Γ) : Prop :=
  ∀ γ, h (Ψ γ) = γ

/-- The step-level obligation Theorem 61's induction consumes at a step
acting on `n`: the post-state's accumulator is the pre-state's composed
with an inverse witnessing that step's own state map.

Table 1 supplies this row by row. At L-Iter, L-Finish, and a landing
L-Divert the accumulator becomes `g ∘ h` for the `h` the iteration yields,
and Definition 51's witness gives `h(Ψ(γ)) = γ`. At L-Leave, L-Raise, an
aborting L-Divert, and an O-Retire of `n`, Table 1 gives `Ψ = id` and
leaves the accumulator unchanged, which is the same statement with
`h = id`. Those are the two cases Theorem 61's proof splits on, and
Lemma 54(4) excludes L-Begin and L-Unload from the interior of an
episode while `installed` denies O-Insert and O-Remove. -/
structure SelfStepRecovers {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ}
    (s : Step Γ View Error θ θ' Ψ) : Type u where
  inverse : Γ → Γ
  witnessed : ∀ γ, inverse (Ψ γ) = γ
  composes : ∀ γ, θ'.accumulator γ = θ.accumulator (inverse γ)

/-- Table 1 discharges `SelfStepRecovers` for every rule an episode's
interior can contain, without any hypothesis beyond Definition 51's own
witness condition on the iterator the `Reloading` state carries.

The `Ψ = id` rows (L-Leave, L-Raise, an aborting L-Divert, O-Retire) are
immediate with `inverse = id`. The iteration rows (L-Iter, L-Finish, a
landing L-Divert) take `inverse` to be the `h` the iteration yields at the
state it runs at, which the `Witnessed` hypothesis supplies. -/
def selfStepRecovers_of_idStateMap
    {θ θ' : Lifecycle Γ View Error}
    (s : Step Γ View Error θ θ' (id : Γ → Γ))
    (haccum : θ'.accumulator = θ.accumulator) :
    SelfStepRecovers s :=
  { inverse := id
    witnessed := fun _ => rfl
    composes := fun γ => by rw [haccum]; rfl }

/-- Table 1 discharges the recovery obligation for EVERY rule the interior
of an episode can contain, with no hypothesis beyond the witness condition
each iteration row already carries as a premise.

Lemma 54(4) excludes L-Begin and L-Unload from an episode's interior, and
an installed `θ_n` denies O-Insert and O-Remove; the remaining eight rows
split into the iteration rows (L-Iter, L-Finish, a landing L-Divert),
where the composed inverse is the `h` the iteration yields and the row's
own `hwitness` premise is Definition 51's condition, and the `Ψ = id` rows
(O-Retire, L-Leave, L-Raise, an aborting L-Divert), where the accumulator
is carried through unchanged. This is what makes `recovery_exactness`
below a theorem about the calculus rather than about an assumption. -/
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

/-- Theorem 61 (recovery exactness), general non-atomic form.

Let an episode of `n` be given, let `g_b` be the accumulator `n` carries
where the episode opens and `g_u` the one it carries where the theorem is
read, and suppose:

  * every step acting on `n` inside the episode composes an inverse onto
    the accumulator that witnesses that step's own state map
    (`SelfStepRecovers`, which Table 1 discharges for every such rule);
  * every step acting on another fiber commutes with `n`'s accumulator
    (Definition 60's pairwise independence).

Then applying `n`'s accumulator at the state the episode has reached
yields exactly the state the other fibers' steps alone would have produced
from where the episode opened:

    g_u (γ^u)  =  (Ψ^{t_l} ∘ ⋯ ∘ Ψ^{t_1}) (g_b (γ^b))

which is equation (56), with `g_b = id` at an episode opened by an
L-Begin (Table 1's L-Begin row writing `Reloading(e_n, id, ω)`) reducing
the right-hand side to `(Ψ^{t_l} ∘ ⋯ ∘ Ψ^{t_1})(γ^b)` on the nose.

The induction is exactly the paper's: at a step acting on `n` the newly
composed inverse cancels that step's own state map, leaving the index set
unchanged; at a step acting on another fiber independence carries the
accumulator past it, appending `Ψ^u` to the composite. -/
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
