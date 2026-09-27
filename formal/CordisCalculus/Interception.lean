










namespace Coeffect




inductive MergeKind where
  | scalarOverwrite
  | setUnion
  deriving DecidableEq, Repr

namespace MergeKind


def identity : MergeKind → String := fun _ => ""





def normalize (xs : List String) : List String :=
  (xs.mergeSort (· ≤ ·)).eraseDups





def combine : MergeKind → String → String → String
  | scalarOverwrite, l, r => if r = "" then l else r
  | setUnion, l, r =>
    let items := (l.splitOn ",").filter (· ≠ "") ++ (r.splitOn ",").filter (· ≠ "")
    String.intercalate "," (normalize items)






theorem scalarOverwrite_left_id (r : String) :
    MergeKind.scalarOverwrite.combine "" r = r := by
  unfold combine
  by_cases h : r = ""
  · simp [h]
  · simp [h]





theorem scalarOverwrite_right_id (l : String) :
    MergeKind.scalarOverwrite.combine l "" = l := by
  unfold combine
  simp





theorem scalarOverwrite_assoc (l r s : String) :
    MergeKind.scalarOverwrite.combine (MergeKind.scalarOverwrite.combine l r) s
      = MergeKind.scalarOverwrite.combine l (MergeKind.scalarOverwrite.combine r s) := by
  unfold combine
  by_cases hs : s = ""
  · by_cases hr : r = "" <;> simp [hs, hr]
  · simp [hs]









theorem setUnion_token_right_id (xs : List String) (hnorm : normalize xs = xs) :
    normalize (xs ++ ([] : List String)) = xs := by
  rw [List.append_nil]
  exact hnorm

end MergeKind






structure Iota where
  contextCarried : List (String × String)
  mergeKind : List (String × MergeKind)
  deriving Repr

namespace Iota

def kindOf (t : Iota) (k : String) : MergeKind :=
  ((t.mergeKind.find? (fun p => p.1 == k)).map Prod.snd).getD MergeKind.scalarOverwrite






def contextMetadata (t : Iota) (k : String) : String :=
  ((t.contextCarried.reverse.find? (fun p => p.1 == k)).map Prod.snd).getD (t.kindOf k).identity





def intercept (t : Iota) (k nu : String) : Iota :=
  let merged := (t.kindOf k).combine (t.contextMetadata k) nu
  { t with contextCarried := t.contextCarried ++ [(k, merged)] }






theorem intercept_preserves_mergeKind (t : Iota) (k nu : String) :
    (t.intercept k nu).mergeKind = t.mergeKind := rfl






def resolve (t : Iota) (k componentDeclared : String) : String :=
  (t.kindOf k).combine componentDeclared (t.contextMetadata k)







theorem resolve_right_biased_scalar (t : Iota) (k componentDeclared ctxVal : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = ctxVal) (hne : ctxVal ≠ "") :
    t.resolve k componentDeclared = ctxVal := by
  unfold resolve
  rw [hkind, hctx]
  unfold MergeKind.combine
  simp [hne]






theorem resolve_falls_back_to_component_scalar (t : Iota) (k componentDeclared : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = "") :
    t.resolve k componentDeclared = componentDeclared := by
  unfold resolve
  rw [hkind, hctx]
  exact MergeKind.scalarOverwrite_right_id componentDeclared






theorem intercept_fresh_scalar (t : Iota) (k nu : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = "") :
    (t.intercept k nu).contextMetadata k = nu := by
  have hmerged : (t.kindOf k).combine (t.contextMetadata k) nu = nu := by
    rw [hkind, hctx]
    exact MergeKind.scalarOverwrite_left_id nu
  have hkindEq : (t.intercept k nu).kindOf k = t.kindOf k := rfl
  unfold Iota.contextMetadata
  rw [hkindEq]
  unfold Iota.intercept
  rw [hmerged]
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.nil_append, List.find?_cons]
  simp

end Iota

end Coeffect
