



















inductive LifecycleState where
  | inactive
  | active
  deriving DecidableEq, Repr





structure Fiber where
  requires : List String
  provides : List String
  state : LifecycleState
  retired : Bool
  deriving DecidableEq, Repr








abbrev Registry := List (String × Fiber)

namespace Registry

def empty : Registry := []

def find (r : Registry) (name : String) : Option Fiber :=
  (r.find? (fun p => p.1 == name)).map Prod.snd

def contains (r : Registry) (name : String) : Bool :=
  r.any (fun p => p.1 == name)



def coeffectContext (r : Registry) : List String :=
  (r.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)


def satisfied (r : Registry) (name : String) : Bool :=
  match r.find name with
  | none => false
  | some fiber => fiber.requires.all (fun dep => (r.coeffectContext).contains dep)









def wellFormed (r : Registry) : Prop :=
  r.Pairwise (fun p q => p.1 ≠ q.1 → (p.2.provides.filter q.2.provides.contains).isEmpty = true)



def insert (r : Registry) (name : String) (req prov : List String) : Option Registry :=
  if r.contains name then none
  else if r.any (fun p => !(p.2.provides.filter prov.contains).isEmpty) then none
  else some (r ++ [(name, { requires := req, provides := prov, state := .inactive, retired := false })])



def retire (r : Registry) (name : String) : Option Registry :=
  if r.contains name then
    some (r.map (fun p => if p.1 == name then (p.1, { p.2 with retired := true }) else p))
  else none


def remove (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.retired && fiber.state == LifecycleState.inactive then
      some (r.filter (fun p => p.1 != name))
    else none
  | none => none



def reload (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.state == LifecycleState.inactive && !fiber.retired && r.satisfied name then
      some (r.map (fun p => if p.1 == name then (p.1, { p.2 with state := .active }) else p))
    else none
  | none => none



def unload (r : Registry) (name : String) : Option Registry :=
  match r.find name with
  | some fiber =>
    if fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name) then
      some (r.map (fun p => if p.1 == name then (p.1, { p.2 with state := .inactive }) else p))
    else none
  | none => none








theorem find_map_update (r : List (String × Fiber)) (name : String) (upd : Fiber → Fiber) :
    Registry.find (r.map (fun p => if p.1 == name then (p.1, upd p.2) else p)) name
      = (Registry.find r name).map upd := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    unfold Registry.find at *
    simp only [List.map_cons, List.find?_cons]
    by_cases hc : hd.1 == name
    · simp only [hc, if_true, Option.map_some]
    · simp only [hc, Bool.false_eq_true, if_false]
      exact ih























































































end Registry
