import CordisCalculus.Basic











































namespace Registry







def observedSatisfaction (r : Registry) (A : List String) : List Bool :=
  A.map (fun name => r.satisfied name)





def ObsEquiv (A : List String) (g1 g2 : Registry) : Prop :=
  ∀ name ∈ A, g1.satisfied name = g2.satisfied name

theorem ObsEquiv.refl (A : List String) (g : Registry) : ObsEquiv A g g := fun _ _ => rfl

theorem ObsEquiv.symm {A : List String} {g1 g2 : Registry} (h : ObsEquiv A g1 g2) :
    ObsEquiv A g2 g1 := fun name hname => (h name hname).symm

theorem ObsEquiv.trans {A : List String} {g1 g2 g3 : Registry}
    (h12 : ObsEquiv A g1 g2) (h23 : ObsEquiv A g2 g3) : ObsEquiv A g1 g3 :=
  fun name hname => (h12 name hname).trans (h23 name hname)








theorem ObsEquiv.mono {A A' : List String} (hsub : ∀ name ∈ A', name ∈ A)
    {g1 g2 : Registry} (h : ObsEquiv A g1 g2) : ObsEquiv A' g1 g2 :=
  fun name hname => h name (hsub name hname)






theorem ObsEquiv.trivial_at_empty (g1 g2 : Registry) : ObsEquiv [] g1 g2 :=
  fun _ hname => absurd hname (List.not_mem_nil)


















def RegistryEquiv (g1 g2 : Registry) : Prop := g1.Perm g2

theorem RegistryEquiv.refl (g : Registry) : RegistryEquiv g g := List.Perm.refl g

theorem RegistryEquiv.symm {g1 g2 : Registry} (h : RegistryEquiv g1 g2) : RegistryEquiv g2 g1 :=
  List.Perm.symm h

theorem RegistryEquiv.trans {g1 g2 g3 : Registry}
    (h12 : RegistryEquiv g1 g2) (h23 : RegistryEquiv g2 g3) : RegistryEquiv g1 g3 :=
  List.Perm.trans h12 h23







































def namesNodup (g : Registry) : Prop := (g.map Prod.fst).Nodup

theorem wellFormed_unique_name {g : Registry} (hnodup0 : g.namesNodup) {p q : String × Fiber}
    (hp : p ∈ g) (hq : q ∈ g) (hpq : p.1 = q.1) : p = q := by
  unfold Registry.namesNodup at hnodup0
  induction g generalizing p q with
  | nil => cases hp
  | cons hd tl ih =>
    rw [List.map_cons, List.nodup_cons] at hnodup0
    cases hp with
    | head => cases hq with
      | head => rfl
      | tail _ hq' =>
        exfalso
        apply hnodup0.1
        rw [hpq]
        exact List.mem_map_of_mem hq'
    | tail _ hp' => cases hq with
      | head =>
        exfalso
        apply hnodup0.1
        rw [← hpq]
        exact List.mem_map_of_mem hp'
      | tail _ hq' => exact ih hnodup0.2 hp' hq' hpq















theorem find_perm_invariant {g1 g2 : Registry} (hperm : g1.Perm g2)
    (hnd1 : g1.namesNodup) (name : String) : g1.find name = g2.find name := by
  have hnd2 : g2.namesNodup := by
    unfold Registry.namesNodup at *
    exact hnd1.perm (hperm.map Prod.fst)
  unfold Registry.find
  cases hfind1 : g1.find? (fun p => p.1 == name) with
  | none =>
    cases hfind2 : g2.find? (fun p => p.1 == name) with
    | none => rfl
    | some fiber2 =>
      exfalso
      have hmem2 : fiber2 ∈ g2 := List.mem_of_find?_eq_some hfind2
      have hpred2 : fiber2.1 == name := (List.find?_eq_some_iff_append.mp hfind2).1
      have hmem1 : fiber2 ∈ g1 := (List.Perm.mem_iff hperm.symm).mp hmem2
      have hnone : ∀ x ∈ g1, ¬ (x.1 == name) = true := List.find?_eq_none.mp hfind1
      exact absurd hpred2 (hnone fiber2 hmem1)
  | some fiber1 =>
    have hmem1 : fiber1 ∈ g1 := List.mem_of_find?_eq_some hfind1
    have hpred1 : fiber1.1 == name := (List.find?_eq_some_iff_append.mp hfind1).1
    have hmem2 : fiber1 ∈ g2 := (List.Perm.mem_iff hperm).mp hmem1
    cases hfind2 : g2.find? (fun p => p.1 == name) with
    | none =>
      exfalso
      have hnone : ∀ x ∈ g2, ¬ (x.1 == name) = true := List.find?_eq_none.mp hfind2
      exact absurd hpred1 (hnone fiber1 hmem2)
    | some fiber2 =>
      have hmem2' : fiber2 ∈ g2 := List.mem_of_find?_eq_some hfind2
      have hpred2 : fiber2.1 == name := (List.find?_eq_some_iff_append.mp hfind2).1
      have hpq : fiber1.1 = fiber2.1 := (beq_iff_eq.mp hpred1).trans (beq_iff_eq.mp hpred2).symm
      have := wellFormed_unique_name hnd2 hmem2 hmem2' hpq
      rw [this]













theorem coeffectContext_mem_perm_invariant {g1 g2 : Registry} (h : RegistryEquiv g1 g2) (dep : String) :
    g1.coeffectContext.contains dep = g2.coeffectContext.contains dep := by
  unfold Registry.coeffectContext
  have hfp : (g1.filter (fun p => p.2.state == LifecycleState.active)).Perm
      (g2.filter (fun p => p.2.state == LifecycleState.active)) := h.filter _
  have hflat : ((g1.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)).Perm
      ((g2.filter (fun p => p.2.state == LifecycleState.active)).flatMap (fun p => p.2.provides)) :=
    hfp.flatMap_right _
  simp only [List.contains_eq_mem, decide_eq_decide]
  exact hflat.mem_iff

theorem RegistryEquiv.to_obsEquiv {g1 g2 : Registry} (h : RegistryEquiv g1 g2)
    (hnd1 : g1.namesNodup) (A : List String) :
    ObsEquiv A g1 g2 := by
  intro name _
  unfold Registry.satisfied
  rw [find_perm_invariant h hnd1 name]
  cases hfind2 : g2.find name with
  | none => rfl
  | some fiber =>
    exact List.all_congr rfl (fun dep => coeffectContext_mem_perm_invariant h dep)













def commutativeKeysConnectionClosedByCommutativeKeysLean : Unit := ()

end Registry
