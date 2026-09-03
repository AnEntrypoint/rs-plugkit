import CordisCalculus.RecoveryGeneral

/-!
Paper Theorem 64 (resolution coherence), in its general non-atomic form.

An episode of `n` opens at an L-Begin with committed view `ω`. The theorem
asserts three things about the interval that follows:

  * `Reloading` occupies an INITIAL interval `[b, r]` of the episode and is
    never re-entered inside it, L-Begin being the one rule leading into
    that lifecycle state and its own premise `θ_n = Inactive(⊥)` putting
    any second application outside the episode;
  * every iteration the transition runs -- every L-Iter and L-Finish --
    runs against that one resolution `ω`, since Table 1 gives both rules
    the premise `target_n(γ) = ω'` and Lemma 54(2) gives `ω' = ω`;
  * where the fiber leaves that interval, EXACTLY ONE of two things
    happened: an L-Finish landing in `Active(-, ω)`, or an L-Divert/L-Raise
    landing in `Unloading(-, ω, -)`, from which Lemma 54(4) makes an
    L-Unload the only exit and Corollary 62 supplies the recovery equation.

The dichotomy is the substance: it is what makes the inertia of Section
4.3.3 safe. An iteration already in flight when the target view turns
lands regardless, and that landing installs an effect computed against a
resolution that no longer holds -- so the guarantee cannot be about every
step. What the rules deliver instead is that the fiber then leaves through
`Unloading`, where its accumulator withdraws the very iteration that
landed. `resolution_coherence_exit` below is that disjunction, proved by
case analysis on Table 1's rows rather than assumed.
-/

universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}

/-- The committed view a `Reloading` state carries, as an explicit field
the exit dichotomy quantifies over. -/
def Lifecycle.reloadingView : Lifecycle Γ View Error → Option View
  | .reloading _ _ ω => some ω
  | _ => none

/-- Theorem 64, first claim: `Reloading` is entered only by L-Begin, so it
occupies an initial interval of the episode and is not re-entered. Table 1
offers exactly one row whose post-state is `Reloading` from a non-`Reloading`
pre-state, and its premise is `Inactive(⊥)`. -/
theorem reloading_entered_only_by_lBegin
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (hbefore : ¬ θ.inFlight) (hafter : θ'.inFlight) :
    s.rule = Rule.lBegin ∧ θ = Lifecycle.inactive none := by
  cases s <;> simp_all [Step.rule, Lifecycle.inFlight]

/-- Theorem 64's second claim: an L-Iter or an L-Finish runs against the
committed view the `Reloading` state already carries, which Lemma 54(2)
holds constant across the episode. Both rows of Table 1 preserve `ω`, so
no iteration of the transition can run against a resolution other than the
one the episode opened with. -/
theorem iteration_runs_against_committed_view
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (hrule : s.rule = Rule.lIter ∨ s.rule = Rule.lFinish) :
    θ'.committedView = θ.committedView := by
  cases s <;> simp_all [Step.rule, Lifecycle.committedView]

/-- Theorem 64's dichotomy. A step leaving `Reloading` is one of exactly
three Table 1 rows, and they land in exactly two places:

  1. `step^r = L-Finish(n)` with `θ^{r+1} = Active(-, ω)`; or
  2. `step^r ∈ {L-Divert(n), L-Raise(n)}` with `θ^{r+1} = Unloading(-, ω, -)`.

In both cases the committed view is preserved, so the `ω` the post-state
carries is the `ω` the episode opened with. `Unloading` is the only other
destination, which is what routes every early exit -- a diverted
transition and a failed one alike -- through the single rule that applies
an accumulator. -/
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

/-- The second branch of the dichotomy continues: `Unloading` admits
exactly one exit, the L-Unload whose `Ψ` is the accumulator itself. This
is Lemma 54(4) at the `Unloading` state, and it is what lets Corollary 62
read Theorem 61's equation at the index the episode closes. -/
theorem unloading_exits_only_by_lUnload
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    {g : Γ → Γ} {ω : View} {ζ : Option Error} (hθ : θ = .unloading g ω ζ)
    (hout : ¬ θ'.installed) :
    s.rule = Rule.lUnload ∧ Ψ = g ∧ θ' = .inactive ζ := by
  cases s <;> simp_all [Step.rule, Lifecycle.installed]

/-- Corollary 62 (terminal recovery), the closing form of Theorem 61 the
second branch of Theorem 64's dichotomy points at.

Where the episode closes, `step^u` is the L-Unload of `n` whose `Ψ` is
`g_n^u` by Lemma 54(3), so the state the episode leaves behind is exactly
the state the other fibers' own steps would have produced from where it
opened. Neither the statement nor the equivalence mentions the outcome
`ζ`, which by Table 1 is the one field in which the states L-Divert and
L-Raise lead to differ -- so a diverted transition and a failed one leave
the same trace behind. -/
theorem terminal_recovery
    {θb : Lifecycle Γ View Error} {g : Γ → Γ} {ω : View} {ζ : Option Error}
    (ep : Episode Γ View Error θb (.unloading g ω ζ))
    (hindep : ep.OtherStepsIndependent) (γ : Γ) :
    g (ep.reachedState γ) = ep.otherComposite (θb.accumulator γ) :=
  recovery_exactness ep hindep γ

/-- Corollary 62's own last sentence, as a theorem about the rules rather
than about one trace: an aborting L-Divert and an L-Raise applied at the
same `Reloading` state produce `Unloading` states that agree in EVERY
field except the outcome. Both carry the same accumulator `g` and the same
committed view `ω`, so both reach the same state under `terminal_recovery`
-- a failed transition strands exactly as little as a diverted one, and
the outcome is the only trace the failure leaves.

This is proved by reading the two Table 1 rows against each other, so it
holds for every `g`, `ω`, and error, not for one witnessed pair. -/
theorem divert_and_raise_agree_except_outcome
    {i : EffectIter Γ} {g : Γ → Γ} {ω : View} {ξ : Error}
    (_sDivert : Step Γ View Error (.reloading i g ω) (.unloading g ω none) id)
    (_sRaise : Step Γ View Error (.reloading i g ω) (.unloading g ω (some ξ)) id) :
    (Lifecycle.unloading g ω none : Lifecycle Γ View Error).accumulator
        = (Lifecycle.unloading g ω (some ξ) : Lifecycle Γ View Error).accumulator
      ∧ (Lifecycle.unloading g ω none : Lifecycle Γ View Error).committedView
        = (Lifecycle.unloading g ω (some ξ) : Lifecycle Γ View Error).committedView :=
  ⟨rfl, rfl⟩

/-- Section 4.3.4's substantive consequence, quantified over every episode
shape: because an aborting L-Divert and an L-Raise leave the same
accumulator and the same committed view behind, the state each one's
eventual L-Unload reaches is the same. A failing transition therefore
leaves its fiber's effects recovered rather than stranded, exactly as a
diverted one does. -/
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
