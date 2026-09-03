import CordisCalculus.Iterator

/-!
Paper Table 1 (Section 4.4): the ten rules of Section 4.3 read as writes
on the fiber they act on. Each row of the table fixes the lifecycle state
before the step, the lifecycle state after it, the state map `Ψ` the step
applies to the ambient context, and the control fields the step edits.

`Step` below is that table verbatim, one constructor per row, indexed by
the pre-state and post-state so that Lemma 54's case analyses become
`cases` on a `Step`. Every rule's own side premises from Section 4.3
(L-Begin's `target_n(γ) = ω ≠ ⊥`, L-Iter/L-Finish's `target_n(γ) = ω`,
L-Divert's `target_n(γ) ≠ ω`, L-Unload's `¬ relied_n(γ)`) are carried as
explicit hypothesis fields, so a `Step` value is a derivation of the rule,
not merely a pair of endpoints.
-/

universe u

namespace Cordis

variable {Γ : Type u} {View : Type u} {Error : Type u}

/-- Which of the ten rules a step applies. Definition 53's `step^t = r(n)`
records exactly this together with the name, and every case analysis in
Section 4.4 is a lookup keyed on it. -/
inductive Rule where
  | oInsert | oRetire | oRemove
  | lBegin | lIter | lFinish | lDivert | lRaise | lLeave | lUnload
  deriving DecidableEq, Repr

/-- Paper Table 1. A `Step Γ View Error` is one row of the table applied
at one fiber: the rule, its pre-state and post-state lifecycle values, and
the state map `Ψ` it applies to the ambient context.

The three orchestration rows (O-Insert, O-Retire, O-Remove) all have
`Ψ = id` and touch only registry-level control fields, so they carry no
lifecycle transition of their own beyond what the table's second and third
columns record.

`stateMap` is the field the metatheory reads as `Ψ^t`; `accumBefore` and
`accumAfter` are the accumulators the pre- and post-states carry, which
Lemma 54(3) and Theorem 61's induction both track. -/
inductive Step (Γ : Type u) (View : Type u) (Error : Type u) :
    Lifecycle Γ View Error → Lifecycle Γ View Error → (Γ → Γ) → Type u where
  /-- Table 1 row O-Insert: `undefined ↦ Inactive(⊥)`, `Ψ = id`. Modelled
  as the entry becoming `Inactive none`; the domain edit itself is a
  registry-level write no lifecycle field records. -/
  | oInsert :
      Step Γ View Error (.inactive none) (.inactive none) id
  /-- Table 1 row O-Retire: lifecycle state unchanged, `Ψ = id`, the
  retirement flag being the only control field edited. -/
  | oRetire (θ : Lifecycle Γ View Error) :
      Step Γ View Error θ θ id
  /-- Table 1 row O-Remove: premise `Inactive(−)`, `Ψ = id`. -/
  | oRemove (ζ : Option Error) :
      Step Γ View Error (.inactive ζ) (.inactive ζ) id
  /-- L-Begin (Section 4.3.2): `Inactive(⊥) ↦ Reloading(e_n, id, ω)`,
  `Ψ = id`, under the premise that the target view is defined and equals
  the view committed to. -/
  | lBegin (e : EffectIter Γ) (ω : View) :
      Step Γ View Error (.inactive none) (.reloading e id ω) id
  /-- L-Iter: `Reloading(i, g, ω) ↦ Reloading(i', g ∘ h, ω)` with
  `Ψ = pr1 ∘ i`, under `target_n(γ) = ω`. The continuation `i'` is the
  `Just` branch of the iterator, so this row is available only at `more`. -/
  | lIter (Ψ h : Γ → Γ) (next : EffectIter Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hcont : i.continuation = some next)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.reloading next (g ∘ h) ω)
        Ψ
  /-- L-Finish: `Reloading(i, g, ω) ↦ Active(g ∘ h, ω)` with
  `Ψ = pr1 ∘ i`, at the `Nothing` continuation. -/
  | lFinish (Ψ h : Γ → Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hcont : i.continuation = none)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.active (g ∘ h) ω)
        Ψ
  /-- L-Divert, aborting alternative: `Reloading(i, g, ω) ↦
  Unloading(g, ω, ⊥)` with `Ψ = id` and `h = id`. Available only where
  the target view has turned. -/
  | lDivertAbort (i : EffectIter Γ) (g : Γ → Γ) (ω : View) :
      Step Γ View Error (.reloading i g ω) (.unloading g ω none) id
  /-- L-Divert, landing alternative: `Reloading(i, g, ω) ↦
  Unloading(g ∘ h, ω, ⊥)` with `Ψ = pr1 ∘ i`. Section 4.3.3's inertia is
  the restriction that only this alternative is available once an
  iteration is in flight. -/
  | lDivertLand (Ψ h : Γ → Γ) (i : EffectIter Γ) (g : Γ → Γ) (ω : View)
      (hwitness : ∀ γ, h (Ψ γ) = γ) :
      Step Γ View Error
        (.reloading i g ω)
        (.unloading (g ∘ h) ω none)
        Ψ
  /-- L-Raise (Section 4.3.4): `Reloading(i, g, ω) ↦ Unloading(g, ω, ξ)`
  with `Ψ = id`, the accumulator built up to the failing iteration carried
  through unchanged. A raise has nothing to undo, so no inverse composes. -/
  | lRaise (i : EffectIter Γ) (g : Γ → Γ) (ω : View) (ξ : Error) :
      Step Γ View Error (.reloading i g ω) (.unloading g ω (some ξ)) id
  /-- L-Leave (Section 4.3.1): `Active(g, ω) ↦ Unloading(g, ω, ⊥)` with
  `Ψ = id`. Records the decision to deactivate without acting on it, which
  stops the fiber providing its coeffects while leaving every committed
  view intact. -/
  | lLeave (g : Γ → Γ) (ω : View) :
      Step Γ View Error (.active g ω) (.unloading g ω none) id
  /-- L-Unload (Section 4.3.1): `Unloading(g, ω, ζ) ↦ Inactive(ζ)` with
  `Ψ = g`. The only rule in the calculus that applies an accumulator, and
  the fact Theorem 59 and Corollary 62 both turn on. -/
  | lUnload (g : Γ → Γ) (ω : View) (ζ : Option Error) :
      Step Γ View Error (.unloading g ω ζ) (.inactive ζ) g

namespace Step

/-- The rule a step applies, Definition 53's `r` in `step^t = r(n)`. -/
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

/-- Lemma 54(3): `Ψ^t = g_n^t` only where `step^t = L-Unload(n)`; no other
step applies the accumulator to the state. Every other row of Table 1 has
`Ψ` equal to `id` or to an iterator's own `pr1 ∘ i`, neither of which is
the accumulator the pre-state carries. -/
theorem stateMap_eq_accumulator_iff_lUnload
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : s.rule = Rule.lUnload) : Ψ = θ.accumulator := by
  cases s <;> simp_all [rule, Lifecycle.accumulator]

/-- Lemma 54(4), the installed half: a fiber becomes installed only by an
L-Begin. Table 1's second column shows `Reloading`, `Active`, and
`Unloading` are reached from an `Inactive` pre-state by L-Begin alone. -/
theorem lBegin_of_not_installed_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : ¬ θ.installed) (h' : θ'.installed) : s.rule = Rule.lBegin := by
  cases s <;> simp_all [rule, Lifecycle.installed]

/-- Lemma 54(4), the uninstalled half: a fiber ceases to be installed only
by an L-Unload. Table 1 offers no other row whose pre-state is installed
and whose post-state is `Inactive`. -/
theorem lUnload_of_installed_not_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : θ.installed) (h' : ¬ θ'.installed) : s.rule = Rule.lUnload := by
  cases s <;> simp_all [rule, Lifecycle.installed]

/-- Lemma 54(2), the committed-view half: `ω_n` comes into existence only
at an L-Begin and ceases only at an L-Unload, so it is constant across an
episode. Every intermediate row of Table 1 carries `ω` through unchanged. -/
theorem committedView_const_of_installed
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : θ.installed) (h' : θ'.installed) :
    θ'.committedView = θ.committedView := by
  cases s <;> simp_all [Lifecycle.installed, Lifecycle.committedView]

/-- Definition 49's second reading, as a property of the rules: only
L-Finish makes a fiber start providing its coeffects, so a fiber whose
transition is under way in either direction provides none of its own. -/
theorem lFinish_of_not_providing_providing
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : ¬ θ.providing) (h' : θ'.providing) : s.rule = Rule.lFinish := by
  cases s <;> simp_all [rule, Lifecycle.providing]

/-- Section 4.3.4: L-Begin has `Inactive(⊥)` as its premise, so the
lifecycle is never re-entered from an error outcome -- a failed fiber is
withheld rather than retried against an unchanged environment. -/
theorem not_failed_of_lBegin
    {θ θ' : Lifecycle Γ View Error} {Ψ : Γ → Γ} (s : Step Γ View Error θ θ' Ψ)
    (h : s.rule = Rule.lBegin) : ¬ θ.failed := by
  cases s <;> simp_all [rule, Lifecycle.failed]

/-- Section 4.3.4: a failed fiber is `Inactive`, so it carries no
committed view and cannot make `relied` hold -- it obstructs nothing. -/
theorem committedView_none_of_failed {θ : Lifecycle Γ View Error}
    (h : θ.failed) : θ.committedView = none := by
  cases θ with
  | inactive ζ => rfl
  | _ => simp_all [Lifecycle.failed]

end Step

end Cordis
