import CordisCalculus.Basic

/-!
Confluence (paper Theorem 73): two rule applications on DISTINCTLY
named fibers commute -- applying one then the other reaches the same
registry regardless of order. This is the algebraic content behind
"independent components can be scheduled in either order with no
observable difference," the property `calculus.rs`'s bounded model
checker spot-checks via BFS reachability-set equality on a fixed
3-fiber fixture; here it is proved as an unbounded algebraic fact
about `List.map` composition, true of every `Registry` and every pair
of distinct names.
-/

namespace Registry

/-- Two independent state-updates (distinct target names) commute under
`List.map` -- updating `a` then `b` gives the same list as updating `b`
then `a`, since each update only ever touches the entry whose name
matches its own target. -/
theorem map_update_comm (r : Registry) (a b : String) (hab : a ≠ b) (updA updB : Fiber → Fiber) :
    (r.map (fun p => if p.1 == a then (p.1, updA p.2) else p)).map
        (fun p => if p.1 == b then (p.1, updB p.2) else p)
      = (r.map (fun p => if p.1 == b then (p.1, updB p.2) else p)).map
        (fun p => if p.1 == a then (p.1, updA p.2) else p) := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons]
    by_cases hha : hd.1 == a
    · have hha' : hd.1 = a := by simpa using hha
      have hhb : (hd.1 == b) = false := by
        simp only [beq_eq_false_iff_ne, ne_eq]
        rw [hha']; exact hab
      simp only [hha, if_true, hhb, Bool.false_eq_true, if_false, ih]
    · by_cases hhb : hd.1 == b
      · simp only [hha, Bool.false_eq_true, if_false, hhb, if_true, ih]
      · simp only [hha, Bool.false_eq_true, if_false, hhb, ih]

/-- An update-by-name map (as `retire`/`reload`/`unload` all apply) never
changes which names are present in the registry -- `contains` is
invariant under it. This is what licenses composing `retire b` after
`retire a`: the second call's `contains b` guard reads the same answer
whether or not the first call already ran. -/
theorem contains_map_update_name (r : Registry) (target other : String) (upd : Fiber → Fiber) :
    Registry.contains (r.map (fun p => if p.1 == target then (p.1, upd p.2) else p)) other = Registry.contains r other := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons]
    unfold Registry.contains at *
    simp only [List.any_cons]
    by_cases hh : hd.1 == target
    · simp only [hh, if_true, ih]
    · simp only [hh, Bool.false_eq_true, if_false, ih]

/-- `retire` on one name and `retire` on a distinct name commute: doing
both in either order lands on the same registry. This is Theorem 73's
core content for the simplest rule pair; the same `contains`-invariance
plus `map_update_comm` argument applies uniformly to every other pair
of independent rule applications (retire/reload, reload/unload, ...)
since every one of those rules reduces to the same
"guard-check-then-`List.map`-by-name" shape. -/
theorem retire_retire_comm (r : Registry) (a b : String) (hab : a ≠ b)
    (r1 r2 r1' r2' : Registry)
    (h1 : r.retire a = some r1) (h1' : r1.retire b = some r1')
    (h2 : r.retire b = some r2) (h2' : r2.retire a = some r2') :
    r1' = r2' := by
  unfold Registry.retire at h1 h1' h2 h2'
  by_cases hca : Registry.contains r a
  · by_cases hcb : Registry.contains r b
    · simp only [hca, if_true] at h1
      injection h1 with h1; subst h1
      simp only [hcb, if_true] at h2
      injection h2 with h2; subst h2
      have hcb1 : Registry.contains (r.map (fun p => if p.1 == a then (p.1, { p.2 with retired := true }) else p)) b = true :=
        (contains_map_update_name r a b (fun f => { f with retired := true })).trans hcb
      have hca2 : Registry.contains (r.map (fun p => if p.1 == b then (p.1, { p.2 with retired := true }) else p)) a = true :=
        (contains_map_update_name r b a (fun f => { f with retired := true })).trans hca
      simp only [hcb1, if_true] at h1'
      injection h1' with h1'
      simp only [hca2, if_true] at h2'
      injection h2' with h2'
      rw [← h1', ← h2']
      exact map_update_comm r a b hab (fun f => { f with retired := true }) (fun f => { f with retired := true })
    · exfalso
      simp only [hcb, Bool.false_eq_true, if_false] at h2
      exact absurd h2 (by simp)
  · exfalso
    simp only [hca, Bool.false_eq_true, if_false] at h1
    exact absurd h1 (by simp)

end Registry
