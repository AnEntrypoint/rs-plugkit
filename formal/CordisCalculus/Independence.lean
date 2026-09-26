






























variable {Gamma : Type}






structure RevertibleEffect (Gamma : Type) where
  fwd : Gamma → Gamma
  inv : Gamma → Gamma → Gamma
  left_inv : ∀ s, inv s (fwd s) = s








def RevertibleEffect.generators (e : RevertibleEffect Gamma) (reachable : List Gamma) :
    List (Gamma → Gamma) :=
  e.fwd :: reachable.map e.inv









inductive InMonoid (gens : List (Gamma → Gamma)) : (Gamma → Gamma) → Prop where
  | gen : ∀ f, f ∈ gens → InMonoid gens f
  | id : InMonoid gens id
  | comp : ∀ f g, InMonoid gens f → InMonoid gens g → InMonoid gens (f ∘ g)




def Commuting (f g : Gamma → Gamma) : Prop := f ∘ g = g ∘ f

theorem Commuting.symm {f g : Gamma → Gamma} (h : Commuting f g) : Commuting g f := Eq.symm h

theorem commuting_id_left (f : Gamma → Gamma) : Commuting (id : Gamma → Gamma) f := rfl
theorem commuting_id_right (f : Gamma → Gamma) : Commuting f (id : Gamma → Gamma) := rfl

theorem commuting_comp_left {f1 f2 g : Gamma → Gamma}
    (h1 : Commuting f1 g) (h2 : Commuting f2 g) : Commuting (f1 ∘ f2) g := by
  funext s
  show f1 (f2 (g s)) = g (f1 (f2 s))
  have e2 : f2 (g s) = g (f2 s) := congrFun h2 s
  rw [e2]
  exact congrFun h1 (f2 s)

theorem commuting_comp_right {f g1 g2 : Gamma → Gamma}
    (h1 : Commuting f g1) (h2 : Commuting f g2) : Commuting f (g1 ∘ g2) :=
  (commuting_comp_left h1.symm h2.symm).symm












theorem commuting_of_generators_commuting
    {gens1 gens2 : List (Gamma → Gamma)}
    (hcomm : ∀ g1 ∈ gens1, ∀ g2 ∈ gens2, Commuting g1 g2) :
    ∀ f1, InMonoid gens1 f1 → ∀ f2, InMonoid gens2 f2 → Commuting f1 f2 := by
  intro f1 hf1
  induction hf1 with
  | gen g1 hg1 =>
    intro f2 hf2
    induction hf2 with
    | gen g2 hg2 => exact hcomm g1 hg1 g2 hg2
    | id => exact commuting_id_right g1
    | comp x y _ _ ihx ihy => exact commuting_comp_right ihx ihy
  | id =>
    intro f2 _
    exact commuting_id_left f2
  | comp x y _ _ ihx ihy =>
    intro f2 hf2
    exact commuting_comp_left (ihx f2 hf2) (ihy f2 hf2)




def MonoidsCommute (gens1 gens2 : List (Gamma → Gamma)) : Prop :=
  ∀ f1, InMonoid gens1 f1 → ∀ f2, InMonoid gens2 f2 → Commuting f1 f2




















def InverseUndisturbed (gens1 : List (Gamma → Gamma)) (e2 : RevertibleEffect Gamma) : Prop :=
  ∀ f1, InMonoid gens1 f1 → ∀ s t : Gamma, e2.inv (f1 s) (f1 t) = f1 (e2.inv s t)




structure Independent (e1 e2 : RevertibleEffect Gamma)
    (gens1 gens2 : List (Gamma → Gamma)) : Prop where
  commute : MonoidsCommute (Gamma := Gamma) gens1 gens2
  undisturbed12 : InverseUndisturbed gens1 e2
  undisturbed21 : InverseUndisturbed gens2 e1

theorem Independent.symm {e1 e2 : RevertibleEffect Gamma} {gens1 gens2 : List (Gamma → Gamma)}
    (h : Independent e1 e2 gens1 gens2) : Independent e2 e1 gens2 gens1 :=
  { commute := fun f2 hf2 f1 hf1 => (h.commute f1 hf1 f2 hf2).symm
    undisturbed12 := h.undisturbed21
    undisturbed21 := h.undisturbed12 }






theorem independent_of_generators
    (e1 e2 : RevertibleEffect Gamma) (gens1 gens2 : List (Gamma → Gamma))
    (hcomm : ∀ g1 ∈ gens1, ∀ g2 ∈ gens2, Commuting g1 g2)
    (hu12 : InverseUndisturbed gens1 e2) (hu21 : InverseUndisturbed gens2 e1) :
    Independent e1 e2 gens1 gens2 :=
  { commute := commuting_of_generators_commuting hcomm
    undisturbed12 := hu12
    undisturbed21 := hu21 }
















theorem independent_pair_revert_nonlifo_order
    (e1 e2 : RevertibleEffect Gamma) (gens1 gens2 : List (Gamma → Gamma))
    (h : Independent e1 e2 gens1 gens2)
    (hg2 : e2.fwd ∈ gens2) (s0 : Gamma) :






    e1.inv (e2.fwd s0) (e2.fwd (e1.fwd s0)) = e2.fwd s0 ∧



    e2.inv s0 (e2.fwd s0) = s0 := by
  refine ⟨?_, e2.left_inv s0⟩
  have hund : e1.inv (e2.fwd s0) (e2.fwd (e1.fwd s0)) = e2.fwd (e1.inv s0 (e1.fwd s0)) :=
    h.undisturbed21 e2.fwd (InMonoid.gen e2.fwd hg2) s0 (e1.fwd s0)
  rw [e1.left_inv] at hund
  exact hund








theorem revertLifo2_correct (e1 e2 : RevertibleEffect Gamma) (s0 : Gamma) :
    e1.inv s0 (e2.inv (e1.fwd s0) (e2.fwd (e1.fwd s0))) = s0 := by
  rw [e2.left_inv]
  exact e1.left_inv s0




def applyAll (effects : List (RevertibleEffect Gamma)) (s0 : Gamma) : Gamma :=
  effects.foldl (fun s e => e.fwd s) s0



















def revertHeadFirst (effects : List (RevertibleEffect Gamma)) (preStates : List Gamma)
    (finalState : Gamma) : Gamma :=
  match effects, preStates with
  | [], _ => finalState
  | _, [] => finalState
  | e :: erest, s :: srest => e.inv s (revertHeadFirst erest srest finalState)














theorem revertHeadFirst_pair (e1 e2 : RevertibleEffect Gamma) (s0 : Gamma) :
    revertHeadFirst [e1, e2] [s0, e1.fwd s0] (applyAll [e1, e2] s0) = s0 := by
  show e1.inv s0 (e2.inv (e1.fwd s0) (revertHeadFirst [] [] (applyAll [e1, e2] s0))) = s0
  show e1.inv s0 (e2.inv (e1.fwd s0) (applyAll [e1, e2] s0)) = s0
  have hfinal : applyAll [e1, e2] s0 = e2.fwd (e1.fwd s0) := by
    simp [applyAll, List.foldl]
  rw [hfinal, e2.left_inv]
  exact e1.left_inv s0












structure PairwiseIndependentPair (Gamma : Type) where
  e1 : RevertibleEffect Gamma
  e2 : RevertibleEffect Gamma
  gens1 : List (Gamma → Gamma)
  gens2 : List (Gamma → Gamma)
  indep : Independent e1 e2 gens1 gens2
  gen1_has_fwd : e1.fwd ∈ gens1
  gen2_has_fwd : e2.fwd ∈ gens2





theorem PairwiseIndependentPair.revert_both_orders
    (p : PairwiseIndependentPair Gamma) (s0 : Gamma) :
    (p.e1.inv s0 (p.e2.inv (p.e1.fwd s0) (p.e2.fwd (p.e1.fwd s0))) = s0) ∧
    (p.e1.inv (p.e2.fwd s0) (p.e2.fwd (p.e1.fwd s0)) = p.e2.fwd s0 ∧
     p.e2.inv s0 (p.e2.fwd s0) = s0) :=
  ⟨revertLifo2_correct p.e1 p.e2 s0,
   independent_pair_revert_nonlifo_order p.e1 p.e2 p.gens1 p.gens2 p.indep p.gen2_has_fwd s0⟩
