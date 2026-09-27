import CordisCalculus.Basic




















universe u

section
variable (Xi : Type u)




abbrev Outcome := Option Xi

















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



def installed (fiber : ExtFiber Xi) : Bool :=
  match fiber.state with
  | .inactive _ => false
  | _ => true



def failed (fiber : ExtFiber Xi) : Prop :=
  ∃ xi : Xi, fiber.state = .inactive (some xi)





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






def begin (r : ExtRegistry Xi) (name : String) (moreIterations : Bool) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    if fiber.state == .inactive none && r.targetDefined name then
      some (r.updateAt name (fun f => { f with state := .reloading fiber.requires moreIterations }))
    else none
  | none => none




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







def raise (r : ExtRegistry Xi) (name : String) (xi : Xi) : Option (ExtRegistry Xi) :=
  match r.find name with
  | some fiber =>
    match fiber.state with
    | .reloading committed _ =>
      some (r.updateAt name (fun f => { f with state := .unloading committed (some xi) }))
    | _ => none
  | none => none



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



theorem failed_not_installed (fiber : ExtFiber Xi) (h : failed fiber) : installed fiber = false := by
  obtain ⟨xi, hxi⟩ := h
  unfold installed
  rw [hxi]

end ExtRegistry
