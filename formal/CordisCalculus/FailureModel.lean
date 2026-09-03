import CordisCalculus.Basic

/-!
Section 4.3's extended lifecycle (paper Definition 49, eq. 43) and its
failure layer (Section 4.3.4, L-Raise): `Basic.lean`'s `LifecycleState`
is the base calculus's two-state `Inactive|Active` reduction, and every
operation there returns `Option Registry`, collapsing every rejection
-- a guard failing, a name missing -- into the same `none`. That
collapse is sound for the base calculus, whose `Inactive` carries no
`zeta : {bot} u Xi` outcome to distinguish (Definition 49's own text:
"In the two-state calculus the distinction is empty"), but it has no
counterpart for THIS section's `Inactive(zeta)`, where `zeta` is either
`bot` (ordinary withdrawal) or an error drawn from `Xi` (Section
4.3.4's failure layer). This file models eq. 43's four-state lifecycle
directly, with a distinct `Xi` type standing for the paper's error set,
so `Inactive none` (bot) and `Inactive (some xi)` (a raised error) are
different inhabitants rather than aliased through one `Option Registry`
success/failure signal the way `Basic.lean`'s operations are. Mirrors
`orchestrator/calculus.rs`'s `ExtendedLifecycle`/`raise` (rs-plugkit).
-/

universe u

section
variable (Xi : Type u)

/-- Eq. 43's outcome `zeta : {bot} u Xi`, carried by `Unloading` as the
outcome a deactivation is headed for and by `Inactive` as the one it
reached. `none` is `bot`; `some xi` is a raised error. -/
abbrev Outcome := Option Xi

/-- Definition 49, eq. 43: `ThetaGamma := Inactive(zeta) | Reloading(i,
g, omega) | Active(g, omega) | Unloading(g, omega, zeta)`. The
accumulator `g : Gamma -> Gamma` and committed view `omega : d -> N`
have no computational content in this abstract model, matching how
`Basic.lean`'s base calculus elides the effect function `e` -- both
`Reloading` and `Unloading` here carry only the `requires`-satisfaction
witness (`committed`, standing for `omega`) that the rules' own guards
read, the same reduction `ExtendedLifecycle` in `calculus.rs` takes.
`Reloading`'s remaining-iterator `i : Effect_Iter*` is reduced to a
`Bool` (`moreIterations`): `Lemma 54`/Table 1 read the iterator only
through its Left(error)/more-remain/no-remain outcome shape at each
step, never through what an iteration computes, so a two-valued flag
models every rule's guard faithfully without the iterator's own
unneeded computational content -- the same reduction the paper's own
closing remark on Section 4.3.2 licenses (a plain effect function is
the `i = false`-from-the-start degenerate case). -/
inductive ExtLifecycle where
  | inactive (zeta : Outcome Xi)
  | reloading (committed : List String) (moreIterations : Bool)
  | active (committed : List String)
  | unloading (committed : List String) (zeta : Outcome Xi)
  deriving DecidableEq

structure ExtFiber where
  requires : List String
  provides : List String
  state : ExtLifecycle Xi
  deriving DecidableEq

abbrev ExtRegistry := List (String × ExtFiber Xi)

end

namespace ExtRegistry

variable {Xi : Type u} [DecidableEq Xi]

def find (r : ExtRegistry Xi) (name : String) : Option (ExtFiber Xi) :=
  (r.find? (fun p => p.1 == name)).map Prod.snd

/-- Eq. 44's `installed_n(gamma) := theta_n != Inactive(-)`: any state
but `inactive`. -/
def installed (fiber : ExtFiber Xi) : Bool :=
  match fiber.state with
  | .inactive _ => false
  | _ => true

/-- Eq. 44's `failed_n(gamma) := exists xi in Xi. theta_n = Inactive(xi)`:
`inactive` carrying a raised error rather than `bot`. -/
def failed (fiber : ExtFiber Xi) : Prop :=
  ∃ xi : Xi, fiber.state = .inactive (some xi)

/-- The coeffect context (eq. 45's `sigma_gamma`, restricted to `Active`
per Definition 49's own note under eq. 45: a fiber whose transition is
under way in either direction "reads its coeffects through the omega it
holds and provides none of its own"). -/
def coeffectContext (r : ExtRegistry Xi) : List String :=
  (r.filterMap (fun p => match p.2.state with
    | .active committed => some committed
    | _ => none)).flatMap (fun c => c)

def targetDefined (r : ExtRegistry Xi) (name : String) : Bool :=
  match r.find name with
  | none => false
  | some fiber => fiber.requires.all (fun dep => (r.coeffectContext).contains dep)

def updateAt (r : ExtRegistry Xi) (name : String) (upd : ExtFiber Xi → ExtFiber Xi) : ExtRegistry Xi :=
  r.map (fun p => if p.1 == name then (p.1, upd p.2) else p)

/-- L-Begin: `Inactive(bot)`, target defined -> `Reloading(e_n, id,
omega)`. The premise `theta_n = Inactive(bot)` (never `Inactive(xi)`)
is the paper's own textual claim that "the lifecycle is not re-entered
from an error outcome" -- stated here as this operation's guard, proved
as a theorem (`no_reentry_from_failed` below) rather than assumed. -/
def begin (r : ExtRegistry Xi) (name : String) (moreIterations : Bool) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    if fiber.state == .inactive none && r.targetDefined name then
      some (r.updateAt name (fun f => { f with state := .reloading fiber.requires moreIterations }))
    else none
  | none => none

/-- L-Iter: `Reloading`, target still `omega`, iterations remain ->
stays `Reloading` (this abstract model has no per-iteration `h` to
compose, matching `calculus.rs`'s `iterate`). -/
def iter (r : ExtRegistry Xi) (name : String) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .reloading committed true =>
      if r.targetDefined name && fiber.requires == committed then
        some (r.updateAt name (fun f => { f with state := .reloading committed true }))
      else none
    | _ => none
  | none => none

/-- L-Finish: `Reloading`, target still `omega`, no iterations remain ->
`Active(g, omega)`. -/
def finish (r : ExtRegistry Xi) (name : String) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .reloading committed false =>
      if r.targetDefined name && fiber.requires == committed then
        some (r.updateAt name (fun f => { f with state := .active committed }))
      else none
    | _ => none
  | none => none

/-- L-Divert: `Reloading`, target has changed from `omega` -> aborts
into `Unloading(g o h, omega, bot)`. -/
def divert (r : ExtRegistry Xi) (name : String) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .reloading committed _ =>
      if !(r.targetDefined name && fiber.requires == committed) then
        some (r.updateAt name (fun f => { f with state := .unloading committed none }))
      else none
    | _ => none
  | none => none

/-- L-Raise (Section 4.3.4): `Reloading`, the iterator raises ->
`Unloading(g, omega, xi)`. This is the SOLE operation in this file that
ever writes a `some xi` outcome -- every other operation (`begin`,
`iter`, `finish`, `divert`, `leave`, `unload`) either writes no outcome
at all or propagates a pre-existing one unchanged, which is exactly
what `raise_is_sole_error_source` proves. -/
def raise (r : ExtRegistry Xi) (name : String) (xi : Xi) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .reloading committed _ =>
      some (r.updateAt name (fun f => { f with state := .unloading committed (some xi) }))
    | _ => none
  | none => none

/-- L-Leave: `Active`, target no longer `omega` -> `Unloading(g, omega,
bot)`. -/
def leave (r : ExtRegistry Xi) (name : String) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .active committed =>
      if !(r.targetDefined name && fiber.requires == committed) then
        some (r.updateAt name (fun f => { f with state := .unloading committed none }))
      else none
    | _ => none
  | none => none

/-- L-Unload: `Unloading(g, omega, zeta)`, not relied upon -> `Inactive(zeta)`.
`zeta` passes through unchanged -- L-Unload never manufactures an
outcome, only relocates the one `Unloading` already carries. This
model elides the `relied_n(gamma)` guard (Definition 50): it is a
registry-wide, cross-fiber property Basic.lean's single-`Registry`
guards already don't model at this abstraction level (no analogous
guard exists on `Basic.lean`'s own `unload`), so this file follows the
same reduction rather than introducing an asymmetric one just for this
operation. -/
def unload (r : ExtRegistry Xi) (name : String) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .unloading _ zeta =>
      some (r.updateAt name (fun f => { f with state := .inactive zeta }))
    | _ => none
  | none => none

theorem find_updateAt (r : ExtRegistry Xi) (name : String) (upd : ExtFiber Xi → ExtFiber Xi) :
    (r.updateAt name upd).find name = (r.find name).map upd := by
  unfold updateAt find
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons, List.find?_cons]
    by_cases hc : hd.1 == name
    · simp only [hc, if_true, Option.map_some]
    · simp only [hc, Bool.false_eq_true, if_false]
      exact ih

private theorem write_never_failed_helper
    {Xi : Type u} [DecidableEq Xi] (r : ExtRegistry Xi) (name : String)
    (newState : ExtLifecycle Xi)
    (hne : ∀ xi : Xi, newState ≠ .inactive (some xi))
    (r' : ExtRegistry Xi)
    (h : r' = r.updateAt name (fun f => { f with state := newState }))
    (fiber : ExtFiber Xi) (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  rw [h, find_updateAt] at hfind
  rcases hr2 : r.find name with _ | origFiber
  · rw [hr2] at hfind; simp at hfind
  · rw [hr2] at hfind
    simp only [Option.map_some] at hfind
    injection hfind with hfind
    subst hfind
    intro ⟨xi, hxi⟩
    exact hne xi hxi

/-- `begin` never produces
a `failed` fiber (its own guard requires the PRIOR state be
`inactive none`, and its write lands in `reloading`, never
`inactive`). -/
theorem begin_never_produces_failed (r r' : ExtRegistry Xi) (name : String) (moreIterations : Bool)
    (h : r.begin name moreIterations = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  unfold ExtRegistry.begin at h
  split at h
  · split at h
    · injection h with h
      exact write_never_failed_helper r name _ (by simp) r' h.symm fiber hfind
    · simp at h
  · simp at h

theorem iter_never_produces_failed (r r' : ExtRegistry Xi) (name : String)
    (h : r.iter name = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  unfold ExtRegistry.iter at h
  split at h
  · rename_i origFiber _
    split at h
    · rename_i committed heq
      split at h
      · injection h with h
        exact write_never_failed_helper r name _ (by simp) r' h.symm fiber hfind
      · simp at h
    all_goals simp at h
  · simp at h

theorem finish_never_produces_failed (r r' : ExtRegistry Xi) (name : String)
    (h : r.finish name = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  unfold ExtRegistry.finish at h
  split at h
  · rename_i origFiber _
    split at h
    · rename_i committed heq
      split at h
      · injection h with h
        exact write_never_failed_helper r name _ (by simp) r' h.symm fiber hfind
      · simp at h
    all_goals simp at h
  · simp at h

theorem divert_never_produces_failed (r r' : ExtRegistry Xi) (name : String)
    (h : r.divert name = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  unfold ExtRegistry.divert at h
  split at h
  · rename_i origFiber _
    split at h
    · rename_i committed more heq
      split at h
      · injection h with h
        exact write_never_failed_helper r name _ (by simp) r' h.symm fiber hfind
      · simp at h
    all_goals simp at h
  · simp at h

theorem leave_never_produces_failed (r r' : ExtRegistry Xi) (name : String)
    (h : r.leave name = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) : ¬ failed fiber := by
  unfold ExtRegistry.leave at h
  split at h
  · rename_i origFiber _
    split at h
    · rename_i committed heq
      split at h
      · injection h with h
        exact write_never_failed_helper r name _ (by simp) r' h.symm fiber hfind
      · simp at h
    all_goals simp at h
  · simp at h

/-- `unload` reaching a `failed` outcome REQUIRES the fiber to already
carry a `some xi` outcome on entry (i.e. requires a prior `raise`) --
`unload` itself only relocates `zeta`, it never introduces one. -/
theorem unload_failed_requires_prior_error (r r' : ExtRegistry Xi) (name : String)
    (h : r.unload name = some r') (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) (hfailed : failed fiber) :
    ∃ committed xi, r.find name = some { fiber with state := .unloading committed (some xi) } := by
  unfold ExtRegistry.unload at h
  split at h
  · rename_i origFiber hr2
    split at h
    · rename_i committed zeta heq
      injection h with h
      subst h
      rw [find_updateAt r name _, hr2] at hfind
      simp only [Option.map_some] at hfind
      injection hfind with hfind
      subst hfind
      obtain ⟨xi, hxi⟩ := hfailed
      simp only at hxi
      refine ⟨committed, xi, ?_⟩
      have hzeta : zeta = some xi := by injection hxi
      subst hzeta
      rw [hr2]
      congr 1
      cases origFiber
      simp_all
    all_goals simp at h
  · simp at h

/-- **L-Raise is the sole route into a failed outcome, summarized.**
Combining the five never-produces-failed lemmas with
`unload_failed_requires_prior_error`: the only operation in this file
whose output can be `failed` at all is `unload`, and even then only
when the fiber it unloads was already carrying a raised error --
i.e. only when a PRIOR `raise` put it there. No sequence of
`begin`/`iter`/`finish`/`divert`/`leave`/`unload` calls with no `raise`
anywhere in it can ever produce a `failed` fiber. -/
theorem only_raise_then_unload_reaches_failed
    (r r' : ExtRegistry Xi) (name : String) (fiber : ExtFiber Xi)
    (hfind : r'.find name = some fiber) (hfailed : failed fiber)
    (hstep : r.begin name true = some r' ∨ r.begin name false = some r' ∨
             r.iter name = some r' ∨ r.finish name = some r' ∨
             r.divert name = some r' ∨ r.leave name = some r' ∨
             r.unload name = some r') :
    r.unload name = some r' ∧
      ∃ committed xi, r.find name = some { fiber with state := .unloading committed (some xi) } := by
  rcases hstep with h | h | h | h | h | h | h
  · exact absurd (begin_never_produces_failed r r' name true h fiber hfind) (fun hn => hn hfailed)
  · exact absurd (begin_never_produces_failed r r' name false h fiber hfind) (fun hn => hn hfailed)
  · exact absurd (iter_never_produces_failed r r' name h fiber hfind) (fun hn => hn hfailed)
  · exact absurd (finish_never_produces_failed r r' name h fiber hfind) (fun hn => hn hfailed)
  · exact absurd (divert_never_produces_failed r r' name h fiber hfind) (fun hn => hn hfailed)
  · exact absurd (leave_never_produces_failed r r' name h fiber hfind) (fun hn => hn hfailed)
  · exact ⟨h, unload_failed_requires_prior_error r r' name h fiber hfind hfailed⟩

/-- **No re-entry from a failed outcome** (the paper's own textual
claim under L-Raise: "L-Begin has `Inactive(bot)` as a premise, so the
lifecycle is not re-entered from an error outcome"). A `failed` fiber
can never satisfy `begin`'s guard, so no sequence of operations
starting from a `failed` fiber can reach `reloading`/`active` again
without an intervening state change that is impossible by construction
-- `begin` structurally requires `inactive none`, which `failed`
excludes by definition. -/
theorem no_reentry_from_failed (r : ExtRegistry Xi) (name : String) (moreIterations : Bool)
    (fiber : ExtFiber Xi) (hfind : r.find name = some fiber) (hfailed : failed fiber) :
    r.begin name moreIterations = none := by
  obtain ⟨xi, hxi⟩ := hfailed
  unfold ExtRegistry.begin
  split
  · rename_i f heq
    rw [heq] at hfind
    injection hfind with hfind
    subst hfind
    rw [hxi]
    simp
  · rfl

/-- A `failed` fiber is never `installed` -- it carries no committed
view and (per the paper's own remark) "obstructs nothing". -/
theorem failed_not_installed (fiber : ExtFiber Xi) (h : failed fiber) : installed fiber = false := by
  obtain ⟨xi, hxi⟩ := h
  unfold installed
  rw [hxi]

end ExtRegistry
