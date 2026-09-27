

















































namespace Coeffect



abbrev Sigma := List (String × String)

namespace Sigma


def domMem (s : Sigma) (k : String) : Bool := s.any (fun p => p.1 == k)


def get (s : Sigma) (k : String) : Option String :=
  (s.find? (fun p => p.1 == k)).map Prod.snd




def set (s : Sigma) (k v : String) : Option Sigma :=
  if s.domMem k then none else some (s ++ [(k, v)])




def restrict (s : Sigma) (k : String) : Sigma :=
  s.filter (fun p => p.1 != k)






theorem find_none_of_domMem_false (s : Sigma) (k : String) (h : s.domMem k = false) :
    s.find? (fun p => p.1 == k) = none := by
  induction s with
  | nil => rfl
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    simp only [List.find?_cons, h.1]
    exact ih h.2




theorem find_append_singleton (s : Sigma) (k v : String) (h : s.domMem k = false) :
    (s ++ [(k, v)]).find? (fun p => p.1 == k) = some (k, v) := by
  induction s with
  | nil => simp
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    simp only [List.cons_append, List.find?_cons, h.1]
    exact ih h.2

theorem get_set_self (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.get k = some v := by
  refine ⟨s ++ [(k, v)], ?_, ?_⟩
  · unfold Sigma.set
    simp [h]
  · unfold Sigma.get
    rw [find_append_singleton s k v h]
    rfl





theorem restrict_append_singleton (s : Sigma) (k v : String) (h : s.domMem k = false) :
    (s ++ [(k, v)]).restrict k = s := by
  unfold Sigma.restrict
  induction s with
  | nil => simp
  | cons hd tl ih =>
    unfold Sigma.domMem at h
    simp only [List.any_cons, Bool.or_eq_false_iff] at h
    have hne : hd.1 != k := by
      simp only [bne_iff_ne]
      intro heq
      rw [heq] at h
      simp at h
    simp only [List.cons_append, List.filter_cons, hne, if_true]
    congr 1
    exact ih h.2




theorem restrict_set (s : Sigma) (k v : String) (h : s.domMem k = false) :
    ∃ s', s.set k v = some s' ∧ s'.restrict k = s := by
  refine ⟨s ++ [(k, v)], ?_, restrict_append_singleton s k v h⟩
  unfold Sigma.set
  simp [h]

end Sigma







structure SigmaIso where
  rho : Sigma
  sigma : Sigma
  deriving DecidableEq, Repr

namespace SigmaIso








def realmOf (t : SigmaIso) (k : String) : String :=
  ((t.rho.reverse.find? (fun p => p.1 == k)).map Prod.snd).getD k


def get (t : SigmaIso) (k : String) : Option String :=
  t.sigma.get (t.realmOf k)





def set (t : SigmaIso) (k v : String) : Option SigmaIso :=
  (t.sigma.set (t.realmOf k) v).map (fun sigma' => { t with sigma := sigma' })




def isolate (t : SigmaIso) (k r : String) : SigmaIso :=
  { t with rho := t.rho ++ [(k, r)] }






theorem isolate_reassigns (t : SigmaIso) (k r1 r2 : String) :
    ((t.isolate k r1).isolate k r2).realmOf k = r2 := by
  unfold SigmaIso.isolate SigmaIso.realmOf
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.find?_cons]
  simp





theorem isolate_preserves_sigma (t : SigmaIso) (k r : String) :
    (t.isolate k r).sigma = t.sigma := rfl




theorem realmOf_default (t : SigmaIso) (k : String) (h : Sigma.domMem t.rho k = false) :
    t.realmOf k = k := by
  have hrev : Sigma.domMem t.rho.reverse k = false := by
    unfold Sigma.domMem at h ⊢
    rw [List.any_reverse]
    exact h
  unfold SigmaIso.realmOf
  rw [Sigma.find_none_of_domMem_false t.rho.reverse k hrev]
  rfl






theorem isolate_distinct_keys_independent (t : SigmaIso) (k1 k2 r : String) (hne : k1 ≠ k2) :
    (t.isolate k1 r).realmOf k2 = t.realmOf k2 := by
  unfold SigmaIso.isolate SigmaIso.realmOf
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.find?_cons]
  have : (k1 == k2) = false := by
    simp only [beq_eq_false_iff_ne]
    exact hne
  simp [this]

end SigmaIso

end Coeffect
