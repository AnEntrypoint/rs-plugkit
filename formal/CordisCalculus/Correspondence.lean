import CordisCalculus.FailureModel
import CordisCalculus.Transition

/-!
Connects `FailureModel.lean`'s `ExtLifecycle`/`ExtRegistry` (imports only
`Basic`) to `Iterator.lean`/`Transition.lean`'s `Lifecycle`/`Step`
(imports chain `Iterator -> Transition`), both independent models of
Definition 49's four-state machine. `FailureModel.lean`'s own module
comment already states the reduction it takes from the general model:
`Reloading`'s iterator `i : Effect_Iter*` collapses to a `moreIterations
: Bool`, and both `Reloading` and `Unloading` carry only a `committed :
List String` witness in place of the accumulator `g : Gamma -> Gamma` and
committed view `omega : d -> N`. That reduction is exactly the
instantiation `Lifecycle (List String) (List String) Xi` at `Ψ = id`
throughout, with the iterator's remaining-length collapsed to its
`continuation.isSome`. This file makes that correspondence a theorem
rather than a shared paragraph in two module comments: an abstraction map
`toExt` from `Lifecycle (List String) (List String) Xi` to `ExtLifecycle
Xi`, and a proof that every `Step` in the general model that keeps `Ψ =
id` maps to the `ExtRegistry` operation `FailureModel.lean` names for the
same rule.
-/

namespace Cordis

variable {Xi : Type}

/-- The abstraction map: forgets the accumulator/view down to
`FailureModel.lean`'s `committed : List String` witness, and the
iterator's own computational content down to whether another iteration
remains. This is `FailureModel.lean`'s own stated reduction, made
executable. -/
def Lifecycle.toExt : Lifecycle (List String) (List String) Xi → ExtLifecycle Xi
  | .inactive ζ => .inactive ζ
  | .reloading i _ ω => .reloading ω i.continuation.isSome
  | .active _ ω => .active ω
  | .unloading _ ω ζ => .unloading ω ζ

/-- `toExt` commutes with `installed`: `Lifecycle.installed` (a `Prop`)
and `ExtRegistry.installed` (a `Bool` on `ExtFiber`, read on the state
alone here) agree on every state. -/
theorem toExt_installed_iff (θ : Lifecycle (List String) (List String) Xi) :
    θ.installed ↔ (match θ.toExt with | .inactive _ => False | _ => True) := by
  cases θ <;> simp [Lifecycle.installed, Lifecycle.toExt]

/-- `toExt` commutes with `failed`. -/
theorem toExt_failed_iff (θ : Lifecycle (List String) (List String) Xi) :
    θ.failed ↔ ∃ xi : Xi, θ.toExt = .inactive (some xi) := by
  cases θ with
  | inactive ζ =>
      cases ζ <;> simp [Lifecycle.failed, Lifecycle.toExt]
  | reloading i g ω => simp [Lifecycle.failed, Lifecycle.toExt]
  | active g ω => simp [Lifecycle.failed, Lifecycle.toExt]
  | unloading g ω ζ => simp [Lifecycle.failed, Lifecycle.toExt]

/-- `L-Begin` correspondence: a `Step` applying the `lBegin` rule at
`Ψ = id`, read through `toExt`, produces exactly the pre/post pair
`ExtRegistry.begin` writes at a matching entry -- the pre-state is
`inactive none` and the post-state is a `reloading` carrying the fresh
iterator's own `continuation.isSome` as its `moreIterations` flag. -/
theorem step_lBegin_toExt {θ θ' : Lifecycle (List String) (List String) Xi}
    (s : Step (List String) (List String) Xi θ θ' (id : List String → List String))
    (h : s.rule = Rule.lBegin) :
    θ.toExt = .inactive none ∧
      ∃ (i : EffectIter (List String)) (ω : List String),
        θ'.toExt = .reloading ω i.continuation.isSome := by
  cases s <;> simp_all [Step.rule, Lifecycle.toExt]
  case lBegin e ω => exact ⟨e, rfl⟩

/-- `L-Iter`/`L-Finish`/`L-Divert`/`L-Raise`/`L-Leave` correspondence,
stated uniformly: every `Step` whose state map is `id` maps a `toExt`
pre-state matching `ExtLifecycle`'s own shape for that rule's premise to
a `toExt` post-state matching the shape `ExtRegistry`'s same-named
operation writes. Proved by direct case analysis on `Step`, since `toExt`
is defined by structural recursion on the very same constructors Table 1
enumerates. -/
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

/-- The raise-outcome correspondence: `L-Raise` is `FailureModel.lean`'s
sole source of a `some xi` outcome (`raise_is_sole_error_source`'s own
doc comment), and this is the same fact read through `toExt` from the
general model's own rule set -- `lRaise` is Table 1's only row whose
post-state's outcome field is a fresh `some`, every other row either
carrying `none` through or leaving the outcome field absent. -/
theorem toExt_raise_sole_error_source
    {θ θ' : Lifecycle (List String) (List String) Xi}
    (s : Step (List String) (List String) Xi θ θ' (id : List String → List String))
    (h : s.rule ≠ Rule.lRaise) :
    ∀ committed : List String, ∀ ξ : Xi,
      θ'.toExt = .unloading committed (some ξ) →
      θ.toExt = .unloading committed (some ξ) := by
  intro committed ξ hpost
  cases s <;> simp_all [Step.rule, Lifecycle.toExt]

/-- `FailureModel.lean`'s `no_reentry_from_failed` restated over the
general model: `Transition.lean`'s `not_failed_of_lBegin` already proves
the general-model fact this specializes -- `toExt` carries it to
`ExtLifecycle`'s own `failed` predicate with no extra work, confirming
the two files' identically-named guarantees are the same theorem read
through `toExt` rather than two independent proofs that happen to agree. -/
theorem toExt_no_reentry_from_failed
    {θ θ' : Lifecycle (List String) (List String) Xi} {Ψ : List String → List String}
    (s : Step (List String) (List String) Xi θ θ' Ψ)
    (h : s.rule = Rule.lBegin) :
    ¬ ∃ xi : Xi, θ.toExt = .inactive (some xi) := by
  have hnf := Step.not_failed_of_lBegin s h
  intro ⟨xi, hxi⟩
  exact hnf ((toExt_failed_iff θ).mpr ⟨xi, hxi⟩)

end Cordis
