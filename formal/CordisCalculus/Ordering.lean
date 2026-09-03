import CordisCalculus.Basic

/-!
Ordering (paper Theorem 63): a fiber's withdrawal (`unload`) only ever
fires when its own target is genuinely lost -- retired, or its
`requires` is no longer satisfied. This is the guard condition
`unload`'s own `if` already encodes; the theorem states it as an
unbounded fact extractable from ANY successful `unload` call, over
every `Registry` and `name`, not merely observed to hold for the
particular calls a runtime happens to make.
-/

namespace Registry

/-- If `unload name` succeeds, the fiber named `name` (read from the
ORIGINAL registry, before the call) was retired or had lost
satisfaction -- `unload` never fires on a fiber whose target still
holds. This is Theorem 63's core content: a component is never
withdrawn while it would still be providing under its own declared
target. -/
theorem unload_only_on_lost_target (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') :
    ∃ fiber, r.find name = some fiber ∧ fiber.state = .active ∧ (fiber.retired ∨ ¬ r.satisfied name = true) := by
  unfold unload at h
  cases hfind : r.find name with
  | none => rw [hfind] at h; simp at h
  | some fiber =>
    rw [hfind] at h
    simp only at h
    by_cases hcond : fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)
    · have hactive : fiber.state = .active := by
        have h1 := (Bool.and_eq_true .. |>.mp hcond).1
        simpa using h1
      have hlost : fiber.retired ∨ ¬ r.satisfied name = true := by
        have h2 := (Bool.and_eq_true .. |>.mp hcond).2
        rcases Bool.or_eq_true .. |>.mp h2 with hc | hc
        · left; simpa using hc
        · right; simpa using hc
      refine ⟨fiber, ?_, hactive, hlost⟩
      rfl
    · exfalso
      rw [Bool.not_eq_true] at hcond
      rw [hcond] at h
      simp at h

/-- The dual, and the other half of Theorem 63: `unload` never fires on
a fiber whose target is still satisfied AND that is not retired -- an
Active, non-retired, satisfied fiber is never a legal `unload` target.
Stated as the contrapositive, this is the direct withdrawal-ordering
guarantee: a component providing something a dependent still relies on
(hence its own target remains met, hence it is neither retired nor
unsatisfied) cannot be the subject of a successful `unload`. -/
theorem unload_refuses_satisfied_non_retired (r : Registry) (name : String) (fiber : Fiber)
    (hfind : r.find name = some fiber) (hactive : fiber.state = .active)
    (hnotretired : ¬ fiber.retired) (hsat : r.satisfied name = true) :
    r.unload name = none := by
  unfold unload
  rw [hfind]
  simp only
  have hguard : (fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)) = false := by
    rw [hactive]
    simp only [beq_self_eq_true, Bool.true_and]
    rw [Bool.eq_false_iff]
    intro hc
    rw [Bool.or_eq_true] at hc
    rcases hc with hc | hc
    · exact hnotretired hc
    · rw [Bool.not_eq_true'] at hc
      rw [hc] at hsat
      exact absurd hsat (by decide)
  rw [hguard]
  rfl

/-- After a successful `unload`, the fiber genuinely reaches `Inactive`
in the resulting registry -- the state a caller reads back is exactly
what the transition claims, not merely "some `Option` was returned". -/
theorem unload_result_is_inactive (r : Registry) (name : String) (r' : Registry)
    (h : r.unload name = some r') :
    ∃ fiber, r'.find name = some fiber ∧ fiber.state = .inactive := by
  unfold unload at h
  cases hfind : r.find name with
  | none => rw [hfind] at h; simp at h
  | some fiber =>
    rw [hfind] at h
    simp only at h
    by_cases hcond : fiber.state == LifecycleState.active && (fiber.retired || !r.satisfied name)
    · simp only [hcond, if_true] at h
      injection h with h
      subst h
      refine ⟨{ fiber with state := .inactive }, ?_, rfl⟩
      rw [find_map_update r name (fun f => { f with state := .inactive }), hfind]
      rfl
    · rw [Bool.not_eq_true] at hcond
      rw [hcond] at h
      simp at h

/-!
## Correspondence to the Rust withdrawal-ordering guard

Two separate Rust implementations both claim a relationship to Theorem
63; both were read verbatim this session and are addressed in turn.

### 1. `discipline_note.rs::removal_dependents` (the live runtime path)

`crates/plugkit-core/src/orchestrator/discipline_note.rs:309-344`:

```rust
pub fn removal_dependents(discipline: &str) -> Vec<String> {
    let names = enabled_names();
    if !names.iter().any(|n| n == discipline) { return Vec::new(); }
    let provided = declared_provides(discipline);
    let realm = declared_realm(discipline);
    names.iter()
        .filter(|n| n.as_str() != discipline)
        .filter(|n| declared_realm(n) == realm)
        .filter(|n| read_fiber_state(n) == FiberLifecycle::Active)
        .filter(|n| requires_satisfied(n, &names))
        .filter(|n| declared_requires(n).iter().any(|dep| provided.iter().any(|cap| cap == dep)))
        .cloned().collect()
}
```

This was policy code invoked ON DEMAND by the `discipline-check-removal`
verb (`handle_check_removal`, discipline_note.rs:460-...) BEFORE a
withdrawal was attempted -- it never fired an actual removal itself, it
only reported `safe_to_remove: dependents.is_empty()` for a caller (human
or agent) to act on, an advisory pre-flight a caller could skip.

**Closed (PRD row `cordis-withdrawal-guard-enforce-not-just-advise`):**
`handle_check_removal` now accepts `{"discipline", "remove": true}` as a
second mode that performs the actual withdrawal -- rewriting
`enabled.txt` with `discipline` dropped -- gated by
`fiber_lifecycle::SafeToWithdraw::check(&discipline, &dependents)`.
`SafeToWithdraw::check` (fiber_lifecycle.rs:294-300) returns `None` when
`dependents` is non-empty; `handle_check_removal`'s `remove:true` branch
pattern-matches that `None` and returns a hard refusal (`ok:false`, exit
code 1, no write to `enabled.txt`) rather than proceeding, structurally
the same `Option`-returning refusal shape as `ExtendedRegistry::unload`
returning `None` when `self.relied(name)` holds (part 2 below). This
verb is now the sanctioned live-runtime removal surface: the one code
path that actually withdraws a discipline cannot construct the
`enabled.txt` write without a `SafeToWithdraw` witness, closing the gap
this section originally found. `enabled.txt` remains an ordinary tracked
file a human can still hand-edit outside this verb -- the enforcement is
at the sanctioned removal surface, the same boundary `git_finalize` draws
around `git push` without disabling raw `git` everywhere.

**Structural correspondence** (what `removal_dependents` computes, once
consulted): a name `n` is a "dependent" iff `n != discipline`, `n` shares
`discipline`'s realm, `n` is currently `Active`, `n`'s OWN requires are
satisfied, and `n`'s `declared_requires` intersects `discipline`'s
`declared_provides`. This is exactly `relied_n(gamma)` (Definition 50) as
`calculus.rs::ExtendedRegistry::relied` computes it (see part 2) with one
addition (realm-scoping, `declared_realm`) that has no counterpart in
either this file's base `Registry` or `calculus.rs`'s `ExtendedRegistry`
-- realms are a gm-specific multi-tenancy refinement layered over the
paper's single global registry, out of scope for both Lean files, and
named here so the gap is not silently assumed away.

### 2. `calculus.rs::ExtendedRegistry::unload`/`relied` (the actual enforced guard)

`crates/plugkit-core/src/orchestrator/calculus.rs:487-503,711-723`, read
verbatim this session:

```rust
pub fn relied(&self, name: &str) -> bool {
    self.fibers.iter().any(|(other_name, other)| {
        if other_name == name { return false; }
        if !self.installed(other_name) { return false; }
        let committed = match &other.state {
            ExtendedLifecycle::Reloading { committed, .. } => committed,
            ExtendedLifecycle::Active { committed } => committed,
            ExtendedLifecycle::Unloading { committed, .. } => committed,
            ExtendedLifecycle::Inactive { .. } => return false,
        };
        committed.contains(name)
    })
}

pub fn unload(&self, name: &str) -> Option<ExtendedRegistry> {
    let fiber = self.fibers.get(name)?;
    let outcome = match &fiber.state {
        ExtendedLifecycle::Unloading { outcome, .. } => *outcome,
        _ => return None,
    };
    if self.relied(name) { return None; }
    let mut next = self.clone();
    next.fibers.get_mut(name).unwrap().state = ExtendedLifecycle::Inactive { outcome };
    Some(next)
}
```

This is Theorem 63's exact enforcement shape THIS file's own two theorems
state abstractly: `unload` (here, the two-state base-calculus version)
succeeds only when the target is genuinely lost
(`unload_only_on_lost_target`) and never fires on a satisfied, non-retired
fiber (`unload_refuses_satisfied_non_retired`). `ExtendedRegistry::unload`
adds exactly one further precondition beyond this file's base-calculus
`unload` -- `!self.relied(name)` -- which is `relied_n(gamma)` (Definition
50) guarding the SAME transition, not a separate advisory check. This is
the genuine Theorem 63 correspondence: `relied` decides "is any other
installed fiber's committed view still naming `name`," structurally
identical in shape to `removal_dependents`'s filter chain (part 1) but
enforced INSIDE the transition guard rather than reported ahead of one.

**The gap between parts 1 and 2, as originally found by this
correspondence pass:** `removal_dependents` (the live discipline-removal
path gm actually executes) and `ExtendedRegistry::relied`/`unload` (the
one Rust implementation with a Lean-provable-shaped guard baked directly
into the transition) were two independent, unconnected implementations of
the same Definition 50 idea -- `removal_dependents` was never called by
`ExtendedRegistry::unload` and vice versa. `ExtendedRegistry` itself
still carries the doc-commented caveat (calculus.rs:420-426) that it
exists for `verify_calculus`'s exhaustive model-check proof obligations,
not as gm's live runtime path -- that separation is unchanged and
correct; `ExtendedRegistry` remains the model-checked reference
implementation, `discipline_note.rs` remains the live runtime path. What
changed is that the live runtime path's own removal verb now enforces
the SAME Definition-50-shaped precondition inline (`SafeToWithdraw::check`
gating the `enabled.txt` write in `remove:true` mode), rather than only
reporting it. The two implementations are still textually independent
(no shared function), but both now REFUSE their respective withdrawal
transition -- `Option::None` from `unload`, `(ok:false, exit 1)` from
`handle_check_removal`'s `remove:true` branch -- under the identical
condition (a same-realm, Active, requires-satisfied dependent whose
declared requires intersects the withdrawing discipline's declared
provides). Theorem 63's real guarantee now holds in the live runtime path
by construction for any caller going through the sanctioned verb, not
merely by caller discipline to consult a report first.

**Live witness (original session):** `Read`/Explore-agent dispatch on
`discipline_note.rs:309-370` and `calculus.rs:420-503,705-723` returned
the function bodies transcribed above verbatim.

**Live witness (enforcement close, `cordis-withdraw-guard-1787470754-24447`):**
`exec_js` constructed a real two-discipline scenario --
`.gm/disciplines/enabled.txt` listing a provider discipline and a
dependent discipline whose `requires.json` names a capability the
provider's `requires.json` `provides`, with the dependent's
`fiber-state.json` at `Active` -- then dispatched
`discipline-check-removal` with `{"discipline":"<provider>","remove":true}`
against the real spool. The response returned `ok:false`, exit code 1,
`removed` absent from the payload, and `enabled.txt` unchanged on disk --
confirming the hard refusal fires. Removing the dependent discipline from
`enabled.txt` (breaking the reliance) and re-dispatching the identical
`remove:true` call against the provider then returned `ok:true,
removed:true`, and `enabled.txt` on disk no longer listed the provider --
confirming the enforced path performs the real withdrawal once
`SafeToWithdraw::check` accepts.
-/

end Registry
