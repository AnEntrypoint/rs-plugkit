import CordisCalculus.ObservationalEquivalence

/-!
Name-uniqueness preservation (paper Section 4.2, an invariant implicit
in Definition 45 stating the registry as a NAMED SET of components --
distinct names, not a list that could repeat one). `ObservationalEquivalence.lean`
introduced `namesNodup` and used it as an explicit hypothesis on
`wellFormed_unique_name`/`RegistryEquiv.to_obsEquiv`, noting in its own
header that proving `namesNodup` PRESERVED by the five base-calculus
rules was out of scope for that Section-3.3.2-focused session. This file
closes that gap: `insert` is the only rule that can introduce a new
name, and its own `r.contains name` guard is exactly the freshness
check that keeps `namesNodup` invariant; the other four rules only
`map`/`filter` the existing list by name, which cannot introduce a
duplicate that was not already there.
-/

namespace Registry

/-- `List.map` by ANY per-entry function `f` that leaves the `.1`
component fixed (the shape `retire`/`reload`/`unload` all apply, each
via a `{ p.2 with ... }` structure update that only ever touches
`Fiber` fields, never `p.1`) never changes the projected-to-fst list --
the same content `find_map_update`/`contains_map_update_name`
(`Basic.lean`, `Confluence.lean`) already establish, restated here at
the `map Prod.fst` level `namesNodup` is stated over, and stated over an
arbitrary per-entry `f` (rather than the specific "update-by-name"
shape) so it applies via plain `exact`/`simp` regardless of the exact
struct-update term Lean elaborates `retire`/`reload`/`unload`'s bodies
into, which higher-order `rw`/`▸` unification cannot always match
syntactically. -/
theorem map_fst_fixed (r : Registry) (f : String × Fiber → String × Fiber)
    (hfix : ∀ p, (f p).1 = p.1) :
    (r.map f).map Prod.fst = r.map Prod.fst := by
  induction r with
  | nil => rfl
  | cons hd tl ih =>
    simp only [List.map_cons, hfix hd, ih]

/-- `retire` preserves `namesNodup`: it only relabels via `map_fst_fixed`
above, which leaves the name list, and hence its `Nodup`-ness, untouched. -/
theorem retire_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.retire name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold retire at h
  split at h
  · injection h with h
    subst h
    unfold namesNodup
    rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
    exact hnodup
  · contradiction

/-- `reload` preserves `namesNodup`, same argument as `retire`. -/
theorem reload_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.reload name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold reload at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
      exact hnodup
    · contradiction
  · contradiction

/-- `unload` preserves `namesNodup`, same argument as `retire`. -/
theorem unload_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold unload at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      rw [map_fst_fixed r _ (by intro p; split <;> rfl)]
      exact hnodup
    · contradiction
  · contradiction

/-- `remove` preserves `namesNodup`: `List.filter`ing the registry down to
a sublist yields, after `map Prod.fst`, a sublist of the original name
list (`List.Sublist.map` composed with `List.filter_sublist`) -- `Nodup`
is closed under `Sublist` (`List.Nodup.sublist`), so removing entries can
only shrink a duplicate-free list into another duplicate-free one. -/
theorem remove_preserves_namesNodup (r : Registry) (name : String) (r' : Registry)
    (h : r.remove name = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold remove at h
  split at h
  · split at h
    · injection h with h
      subst h
      unfold namesNodup
      exact List.Nodup.sublist (List.Sublist.map Prod.fst List.filter_sublist) hnodup
    · contradiction
  · contradiction

/-- O-Insert (Section 4.2): the ONLY rule that can introduce a new name,
and its own `r.contains name` guard -- refusing the insert when `name`
is already present -- is exactly the freshness condition `namesNodup`
needs. `contains` and `find?`/membership-in-`map Prod.fst` agree (both
ask "does some entry's name equal `name`"), so a refused-`contains`
premise transfers directly to "name is not a member of the projected
list," which is precisely `List.Nodup.cons`'s own hypothesis. -/
theorem insert_preserves_namesNodup (r : Registry) (name : String) (req prov : List String) (r' : Registry)
    (h : r.insert name req prov = some r') (hnodup : r.namesNodup) : r'.namesNodup := by
  unfold insert at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · injection h with h
      subst h
      rename_i hcontains _
      unfold namesNodup
      rw [List.map_append]
      simp only [List.map_cons, List.map_nil]
      rw [List.nodup_append]
      have hsingle : List.Nodup [name] := by unfold List.Nodup; simp
      refine ⟨hnodup, hsingle, ?_⟩
      intro x hx b hb
      have hbname : b = name := List.mem_singleton.mp hb
      subst hbname
      intro hxname
      subst hxname
      rw [Bool.not_eq_true, Bool.eq_false_iff] at hcontains
      apply hcontains
      unfold contains
      rw [List.any_eq_true]
      obtain ⟨p, hpmem, hpeq⟩ := List.mem_map.mp hx
      exact ⟨p, hpmem, by simp [hpeq]⟩

/-- Name-uniqueness preservation (the Section-4.2 companion to
`Preservation.lean`'s `preservation` theorem, stated once over all five
rules the same way): whichever rule a caller applies to a
name-duplicate-free registry, if it succeeds, the result is still
name-duplicate-free. Closes the gap `ObservationalEquivalence.lean`
named as out of scope -- `wellFormed_unique_name`/`RegistryEquiv.to_obsEquiv`'s
`namesNodup` hypothesis is now derivable by induction on the sequence of
rule applications that built any registry reachable from `empty`,
rather than needing to be assumed at every call site. -/
theorem namesNodup_preservation (r : Registry) (hnodup : r.namesNodup) :
    (∀ name req prov r', r.insert name req prov = some r' → r'.namesNodup) ∧
    (∀ name r', r.retire name = some r' → r'.namesNodup) ∧
    (∀ name r', r.remove name = some r' → r'.namesNodup) ∧
    (∀ name r', r.reload name = some r' → r'.namesNodup) ∧
    (∀ name r', r.unload name = some r' → r'.namesNodup) :=
  ⟨fun name req prov r' h => insert_preserves_namesNodup r name req prov r' h hnodup,
   fun name r' h => retire_preserves_namesNodup r name r' h hnodup,
   fun name r' h => remove_preserves_namesNodup r name r' h hnodup,
   fun name r' h => reload_preserves_namesNodup r name r' h hnodup,
   fun name r' h => unload_preserves_namesNodup r name r' h hnodup⟩

/-- `empty` is trivially name-duplicate-free -- the base case every
reachable registry's `namesNodup` induction (built up via
`namesNodup_preservation` above, one rule application at a time)
ultimately rests on, since `empty` is the calculus's sole starting
state (`Basic.lean`'s `Registry.empty`, `calculus.rs`'s
`Registry::empty()`, `handle_model_check`'s own initial state). -/
theorem empty_namesNodup : (empty : Registry).namesNodup := List.nodup_nil

end Registry
