import CordisCalculus.Independence

























variable {Gamma : Type}



abbrev Member (Gamma : Type) := RevertibleEffect Gamma × List (Gamma → Gamma)



def MemberIndependent (m1 m2 : Member Gamma) : Prop :=
  Independent m1.1 m2.1 m1.2 m2.2

theorem MemberIndependent.symm {m1 m2 : Member Gamma} (h : MemberIndependent m1 m2) :
    MemberIndependent m2 m1 := Independent.symm h












theorem theorem20_peel_head
    (e : RevertibleEffect Gamma) (egens : List (Gamma → Gamma)) (hgen : e.fwd ∈ egens)
    (rest : List (Member Gamma))
    (hindep : ∀ m ∈ rest, MemberIndependent (e, egens) m)
    (hrestgen : ∀ m ∈ rest, m.1.fwd ∈ m.2) :
    ∀ gamma0 : Gamma,
      e.inv (rest.foldl (fun s m => m.1.fwd s) gamma0)
            (rest.foldl (fun s m => m.1.fwd s) (e.fwd gamma0))
        = rest.foldl (fun s m => m.1.fwd s) gamma0 := by
  induction rest with
  | nil =>
    intro gamma0
    exact e.left_inv gamma0
  | cons hd tl ih =>
    intro gamma0
    have hindep_hd : MemberIndependent (e, egens) hd := hindep hd List.mem_cons_self
    have hindep_tl : ∀ m ∈ tl, MemberIndependent (e, egens) m :=
      fun m hm => hindep m (List.mem_cons_of_mem hd hm)
    have hgen_hd : hd.1.fwd ∈ hd.2 := hrestgen hd List.mem_cons_self
    have hgen_tl : ∀ m ∈ tl, m.1.fwd ∈ m.2 := fun m hm => hrestgen m (List.mem_cons_of_mem hd hm)
    show e.inv (tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0))
          (tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd (e.fwd gamma0)))
        = tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0)
    have hstep : hd.1.fwd (e.fwd gamma0) = e.fwd (hd.1.fwd gamma0) := by
      have hcommuting : Commuting e.fwd hd.1.fwd :=
        hindep_hd.commute e.fwd (InMonoid.gen e.fwd hgen) hd.1.fwd (InMonoid.gen hd.1.fwd hgen_hd)
      exact congrFun hcommuting.symm gamma0
    rw [hstep]
    exact ih hindep_tl hgen_tl (hd.1.fwd gamma0)














def revertPermuted (order : List (Member Gamma)) (full gamma0 : Gamma) : Gamma :=
  match order with
  | [] => full
  | hd :: tl =>
      revertPermuted tl (hd.1.inv (tl.foldl (fun s m => m.1.fwd s) gamma0) full) gamma0


















theorem corollary21
    (order : List (Member Gamma)) (hpw : order.Pairwise MemberIndependent)
    (hgen : ∀ m ∈ order, m.1.fwd ∈ m.2) (gamma0 : Gamma) :
    revertPermuted order (order.foldl (fun s m => m.1.fwd s) gamma0) gamma0 = gamma0 := by
  induction order with
  | nil => rfl
  | cons hd tl ih =>
    have hpw_tl : tl.Pairwise MemberIndependent := hpw.tail
    have hgen_tl : ∀ m ∈ tl, m.1.fwd ∈ m.2 := fun m hm => hgen m (List.mem_cons_of_mem hd hm)
    have hindep_hd : ∀ m ∈ tl, MemberIndependent (hd.1, hd.2) m := by
      intro m hm
      rw [List.pairwise_cons] at hpw
      have := hpw.1 m hm
      simpa using this
    have hgen_hd : hd.1.fwd ∈ hd.2 := hgen hd List.mem_cons_self
    show revertPermuted tl
          (hd.1.inv (tl.foldl (fun s m => m.1.fwd s) gamma0)
            ((hd :: tl).foldl (fun s m => m.1.fwd s) gamma0)) gamma0
        = gamma0
    have hunfold : (hd :: tl).foldl (fun s m => m.1.fwd s) gamma0
        = tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0) := rfl
    rw [hunfold, theorem20_peel_head hd.1 hd.2 hgen_hd tl hindep_hd hgen_tl gamma0]
    exact ih hpw_tl hgen_tl
