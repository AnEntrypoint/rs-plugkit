import CordisCalculus.Independence



























variable {V : Type}







abbrev CoeffectCtx (V : Type) := String → V









def KeyOp (k : String) (u : V → V) : CoeffectCtx V → CoeffectCtx V :=
  fun sigma k' => if k' == k then u (sigma k) else sigma k'




theorem keyOp_commuting_of_ne {k1 k2 : String} (hne : k1 ≠ k2) (u1 u2 : V → V) :
    Commuting (KeyOp k1 u1) (KeyOp k2 u2) := by
  have hk12 : (k1 == k2) = false := by simp only [beq_eq_false_iff_ne, ne_eq]; exact hne
  have hk21 : (k2 == k1) = false := by simp only [beq_eq_false_iff_ne, ne_eq]; exact Ne.symm hne
  funext sigma
  funext k'
  show (KeyOp k1 u1) ((KeyOp k2 u2) sigma) k' = (KeyOp k2 u2) ((KeyOp k1 u1) sigma) k'
  unfold KeyOp
  by_cases h1 : k' = k1
  · subst h1
    rw [if_pos (beq_self_eq_true k'), if_neg (by simpa using beq_eq_false_iff_ne.mpr hne),
        if_pos (beq_self_eq_true k'), hk12]
    simp
  · by_cases h2 : k' = k2
    · subst h2
      rw [if_neg (by simpa using beq_eq_false_iff_ne.mpr (Ne.symm hne)),
          if_pos (beq_self_eq_true k'), if_pos (beq_self_eq_true k'), hk21]
      simp
    · rw [if_neg (by simpa using beq_eq_false_iff_ne.mpr h1),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h2),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h2),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h1)]









theorem theorem40_commute
    {k1 k2 : String} (hne : k1 ≠ k2)
    (us1 us2 : List (V → V)) :
    ∀ f1, InMonoid (us1.map (KeyOp k1)) f1 →
    ∀ f2, InMonoid (us2.map (KeyOp k2)) f2 →
    Commuting f1 f2 := by
  apply commuting_of_generators_commuting
  intro g1 hg1 g2 hg2
  obtain ⟨u1, _, heq1⟩ := List.mem_map.mp hg1
  obtain ⟨u2, _, heq2⟩ := List.mem_map.mp hg2
  rw [← heq1, ← heq2]
  exact keyOp_commuting_of_ne hne u1 u2








theorem keyOp_reads_undisturbed_at_distinct_key
    {k1 k2 : String} (hne : k1 ≠ k2) (u2 : V → V) (sigma : CoeffectCtx V) :
    (KeyOp k2 u2 sigma) k1 = sigma k1 := by
  unfold KeyOp
  have h : (k1 == k2) = false := by
    simp only [beq_eq_false_iff_ne, ne_eq]
    exact hne
  simp only [h, Bool.false_eq_true, if_false]
















theorem theorem42_of_generator_commutation
    {Gamma : Type} (e1 e2 : RevertibleEffect Gamma) (gens1 gens2 : List (Gamma → Gamma))
    (hcomm : ∀ g1 ∈ gens1, ∀ g2 ∈ gens2, Commuting g1 g2)
    (hu12 : InverseUndisturbed gens1 e2) (hu21 : InverseUndisturbed gens2 e1) :
    Independent e1 e2 gens1 gens2 :=
  independent_of_generators e1 e2 gens1 gens2 hcomm hu12 hu21
