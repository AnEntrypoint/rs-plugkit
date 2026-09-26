import CordisCalculus.Basic



















namespace Registry






structure EffectAttempt where
  actor : String
  target : String
  deriving DecidableEq, Repr









def admits (r : Registry) (a : EffectAttempt) : Bool :=
  a.actor == a.target || !(r.contains a.target)









theorem confinement (r : Registry) (a : EffectAttempt) (hmem : r.contains a.target)
    (hadmit : admits r a = true) : a.actor = a.target := by
  unfold admits at hadmit
  rw [Bool.or_eq_true, Bool.not_eq_true'] at hadmit
  cases hadmit with
  | inl h => exact of_decide_eq_true h
  | inr h => rw [h] at hmem; contradiction







theorem confinement_admits_self (r : Registry) (name : String) :
    admits r { actor := name, target := name } = true := by
  unfold admits
  simp





































































end Registry
