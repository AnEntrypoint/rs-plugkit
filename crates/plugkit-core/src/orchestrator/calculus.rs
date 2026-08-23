#![cfg(target_arch = "wasm32")]

//! A direct, gm-independent implementation of the Cordis paper's Section
//! 4.2 base calculus (two-state `Registry`/`Fiber` model, five rules) AND
//! Section 4.3's extended ten-rule, four-state calculus
//! (`ExtendedRegistry`/`ExtendedFiber`, below the base model in this same
//! file). Every metatheory check elsewhere in this crate
//! (`discipline_note.rs`'s `discipline-audit`) runs over ONE gm-specific
//! instantiation of the paper's BASE model (disciplines, or
//! memory/codeinsight namespaces) at its CURRENT state alone -- gm's own
//! fiber kinds reduce cleanly to the two-state model since none of them
//! yet models multi-step iteration, asynchronous landing, or failure
//! outcomes (Section 4.3.2-4.3.4). `verify_calculus` below exhaustively
//! enumerates EVERY state reachable from an initial registry under every
//! legal rule application (bounded by a small fiber/capability alphabet),
//! checking the metatheory holds for the whole reachable state space
//! rather than for whatever state gm happens to be in when audited.

use std::collections::{BTreeSet, HashMap};

/// A fiber's lifecycle state (Definition 44, reduced as `fiber_lifecycle`
/// reduces it: no async load step in this model either, so `Reloading`
/// collapses into the transition itself rather than being a separate
/// persisted state -- L-Reload is atomic here, matching Section 4.2's
/// base calculus before Section 4.3 splits it into `Reloading`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifecycleState {
    Inactive,
    Active,
}

/// A component in the calculus (paper Definition 43: a component is the
/// triple (d, p, e); `e` -- the effect function -- has no computational
/// content in this abstract model beyond "installs `provides`", so it is
/// elided, leaving the (d, p) pair plus the lifecycle state a fiber
/// carries at runtime, Definition 44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fiber {
    pub requires: BTreeSet<String>,
    pub provides: BTreeSet<String>,
    pub state: LifecycleState,
    /// Retirement flag (Definition 44's `tau`): set by O-Retire, read by
    /// O-Remove's premise.
    pub retired: bool,
}

/// The registry (Definition 45): named fibers, `Registry` itself is the
/// full state `gamma` a rule transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub fibers: HashMap<String, Fiber>,
}

impl Registry {
    pub fn empty() -> Registry {
        Registry { fibers: HashMap::new() }
    }

    /// The coeffect context (Definition 45's `sigma_gamma`): the union of
    /// every `Active` fiber's `provides`, well-defined only because
    /// distinct fibers' provisions are disjoint (Definition 58 clause 2,
    /// checked by `well_formed` below) -- exactly `ActiveFiberSet`'s
    /// invariant in `fiber_lifecycle.rs`, re-derived here independently
    /// for the abstract calculus.
    pub fn coeffect_context(&self) -> BTreeSet<String> {
        let mut ctx = BTreeSet::new();
        for fiber in self.fibers.values() {
            if fiber.state == LifecycleState::Active {
                for cap in &fiber.provides {
                    ctx.insert(cap.clone());
                }
            }
        }
        ctx
    }

    /// The satisfaction predicate (Section 3.2.2, `sigma |= d`): every
    /// capability `name`'s fiber requires is in the current coeffect
    /// context. Definition 46 is the target view `target_n(gamma)` built
    /// on top of this predicate, not the predicate itself.
    pub fn satisfied(&self, name: &str) -> bool {
        let ctx = self.coeffect_context();
        match self.fibers.get(name) {
            Some(fiber) => fiber.requires.iter().all(|dep| ctx.contains(dep)),
            None => false,
        }
    }

    /// Definition 58: a well-formed registry has disjoint provisions
    /// across every pair of distinct fibers (clause 2) -- the invariant
    /// `ActiveFiberSet` enforces by construction in the gm-specific
    /// modules; here it is checked directly against the whole registry
    /// (not only `Active` fibers), since O-Insert's premise (below)
    /// refuses to admit a colliding fiber at ALL, active or not.
    pub fn well_formed(&self) -> bool {
        let names: Vec<&String> = self.fibers.keys().collect();
        for (i, a) in names.iter().enumerate() {
            for b in names.iter().skip(i + 1) {
                let a_provides = &self.fibers[*a].provides;
                let b_provides = &self.fibers[*b].provides;
                if !a_provides.is_disjoint(b_provides) {
                    return false;
                }
            }
        }
        true
    }

    /// O-Insert (Section 4.2): admits a new fiber named `name` only if no
    /// existing fiber's `provides` collides with `provides` (the last
    /// premise of O-Insert) and `name` is fresh. Returns `None` on a
    /// refused insert, matching the paper's premise-gated rule rather
    /// than a silently-clamped one.
    pub fn insert(&self, name: &str, requires: BTreeSet<String>, provides: BTreeSet<String>) -> Option<Registry> {
        if self.fibers.contains_key(name) {
            return None;
        }
        for fiber in self.fibers.values() {
            if !fiber.provides.is_disjoint(&provides) {
                return None;
            }
        }
        let mut next = self.clone();
        next.fibers.insert(
            name.to_string(),
            Fiber { requires, provides, state: LifecycleState::Inactive, retired: false },
        );
        Some(next)
    }

    /// O-Retire (Section 4.2): sets the retirement flag. Unconditional on
    /// the fiber's own lifecycle state (a retired-but-still-Active fiber
    /// must first be deactivated by L-Unload before O-Remove admits it),
    /// matching the paper's O-Retire premise (`n in dom(F_gamma)` alone).
    pub fn retire(&self, name: &str) -> Option<Registry> {
        if !self.fibers.contains_key(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().retired = true;
        Some(next)
    }

    /// O-Remove (Section 4.2): removes a retired, `Inactive` fiber.
    pub fn remove(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if !fiber.retired || fiber.state != LifecycleState::Inactive {
            return None;
        }
        let mut next = self.clone();
        next.fibers.remove(name);
        Some(next)
    }

    /// L-Reload (Section 4.2): an `Inactive`, non-retired fiber whose
    /// target is satisfied activates. Atomic in this base-calculus model
    /// (no `Reloading` in-flight state; Section 4.3 is where that split
    /// lives, already modeled separately by `fiber_lifecycle`'s
    /// `Unloading` reduction).
    pub fn reload(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if fiber.state != LifecycleState::Inactive || fiber.retired || !self.satisfied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = LifecycleState::Active;
        Some(next)
    }

    /// L-Unload (Section 4.2): an `Active` fiber whose target is no
    /// longer satisfied, OR that has been retired, deactivates.
    pub fn unload(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if fiber.state != LifecycleState::Active {
            return None;
        }
        let target_lost = fiber.retired || !self.satisfied(name);
        if !target_lost {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = LifecycleState::Inactive;
        Some(next)
    }

    /// Every state one legal rule application can reach from `self`, over
    /// the given candidate names/capability sets for O-Insert (the only
    /// rule that needs an outside supply of new names/capabilities to
    /// enumerate, since the others act only on names already present).
    fn successors(&self, insert_candidates: &[(String, BTreeSet<String>, BTreeSet<String>)]) -> Vec<Registry> {
        let mut out = Vec::new();
        let names: Vec<String> = self.fibers.keys().cloned().collect();
        for name in &names {
            if let Some(r) = self.retire(name) {
                out.push(r);
            }
            if let Some(r) = self.remove(name) {
                out.push(r);
            }
            if let Some(r) = self.reload(name) {
                out.push(r);
            }
            if let Some(r) = self.unload(name) {
                out.push(r);
            }
        }
        for (name, requires, provides) in insert_candidates {
            if let Some(r) = self.insert(name, requires.clone(), provides.clone()) {
                out.push(r);
            }
        }
        out
    }
}

/// A metatheory violation found while exhaustively enumerating the
/// reachable state space from an initial registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculusViolation {
    pub theorem: &'static str,
    pub detail: String,
}

/// Exhaustively enumerates every state reachable from `initial` under the
/// five base-calculus rules (bounded by `insert_candidates`, the finite
/// set of names/capabilities O-Insert may draw from -- a real
/// implementation draws fresh names from an unbounded supply, Definition
/// 47, but a bounded model check needs a finite candidate pool to
/// terminate), and checks two theorems against EVERY state found, not
/// merely the initial or final one:
///
/// - **Preservation** (Theorem 59): every reachable state is well-formed
///   (disjoint provisions).
/// - **Progress** (Theorem 66, reduced): a well-formed non-quiescent state
///   (some fiber's actual lifecycle state disagrees with what its target
///   demands) always has at least one legal rule application available --
///   the state space never contains a state where the metatheory's own
///   "always a next move" guarantee would be false.
///
/// Confluence and recovery-exactness are checked by `fiber_lifecycle`'s
/// `check_confluence`/`verify_recovery_exactness` already (kind-agnostic,
/// reused rather than reimplemented here); ordering is a consequence of
/// `unload`'s own premise (`target_lost`), checked structurally by that
/// function's type, the same way `SafeToWithdraw` enforces it for gm's
/// instantiations.
pub fn verify_calculus(
    initial: &Registry,
    insert_candidates: &[(String, BTreeSet<String>, BTreeSet<String>)],
    max_states: usize,
) -> Vec<CalculusViolation> {
    let mut violations = Vec::new();
    let mut seen: Vec<Registry> = Vec::new();
    let mut frontier: Vec<Registry> = vec![initial.clone()];
    seen.push(initial.clone());

    while let Some(state) = frontier.pop() {
        if !state.well_formed() {
            violations.push(CalculusViolation {
                theorem: "preservation (Theorem 59)",
                detail: format!("state with colliding provisions reached: {:?}", state.fibers.keys().collect::<Vec<_>>()),
            });
        }

        let is_quiescent = state.fibers.iter().all(|(name, fiber)| {
            let target_active = !fiber.retired && state.satisfied(name);
            (fiber.state == LifecycleState::Active) == target_active
        });

        let successors = state.successors(insert_candidates);
        if !is_quiescent && successors.is_empty() {
            violations.push(CalculusViolation {
                theorem: "progress (Theorem 66)",
                detail: format!("non-quiescent state with no legal rule application: {:?}", state.fibers.keys().collect::<Vec<_>>()),
            });
        }

        for next in successors {
            if !seen.contains(&next) {
                if seen.len() >= max_states {
                    continue;
                }
                seen.push(next.clone());
                frontier.push(next);
            }
        }
    }

    violations
}

/// Verb entry point for `calculus-model-check`: builds a small, fixed
/// registry (three names -- a base provider, a dependent, and a fiber
/// whose `requires` can never be satisfied by anything in the candidate
/// pool, exercising the "never activates" case as well as the ordinary
/// provider/dependent case) and a bounded insert-candidate pool (letting
/// the search also explore adding/retiring/removing each of the three),
/// then exhaustively enumerates every reachable state and reports any
/// preservation/progress violation found across the WHOLE reachable
/// state space -- the direct, gm-independent verification the paper's
/// Section 4.4 metatheory describes, as opposed to a live check over
/// gm's own current discipline/plugin/namespace state alone.
pub fn handle_model_check(_content: &str) -> (String, String, i32) {
    let requires_a = BTreeSet::new();
    let mut provides_a = BTreeSet::new();
    provides_a.insert("cap-a".to_string());

    let mut requires_b = BTreeSet::new();
    requires_b.insert("cap-a".to_string());
    let provides_b = BTreeSet::new();

    let mut requires_c = BTreeSet::new();
    requires_c.insert("cap-nonexistent".to_string());
    let provides_c = BTreeSet::new();

    let initial = Registry::empty();
    let insert_candidates = vec![
        ("fiber-a".to_string(), requires_a, provides_a),
        ("fiber-b".to_string(), requires_b, provides_b),
        ("fiber-c".to_string(), requires_c, provides_c),
    ];

    let violations = verify_calculus(&initial, &insert_candidates, 4096);
    let ok = violations.is_empty();
    let payload = serde_json::json!({
        "ok": ok,
        "theorems_checked": ["preservation", "progress"],
        "model": "3-fiber bounded registry: fiber-a provides cap-a, fiber-b requires cap-a (satisfiable), fiber-c requires cap-nonexistent (never satisfiable)",
        "violations": violations.iter().map(|v| serde_json::json!({"theorem": v.theorem, "detail": v.detail})).collect::<Vec<_>>(),
    });
    (payload.to_string(), String::new(), if ok { 0 } else { 1 })
}

/// Section 4.3's extended lifecycle (Definition 49, eq. 43): the base
/// two-state `Inactive|Active` is replaced by four states, splitting both
/// activation and deactivation into a state the fiber occupies while the
/// transition is under way. `outcome` is the paper's `zeta : {bot} u Xi`
/// (eq. 43/44): `None` is `bot` (no error), `Some(err)` is a raised error
/// from the failure layer (Section 4.3.4). This module reduces the
/// paper's effect iterator (Definition 51, `i : Effect_Gamma^iter*`) to a
/// caller-supplied `remaining_iterations: u32` counter -- the calculus's
/// own metatheory (Lemma 54, Table 1) treats the iterator only through
/// its Maybe(next)/Left(error) outcome shape at each step, never through
/// what an iteration computes, so a counter models every rule's guard
/// faithfully (zero remaining = L-Finish next, nonzero = L-Iter next)
/// without needing the iterator's own computational content, which -- like
/// the base calculus's effect functions -- has none in this abstract model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtendedLifecycle {
    /// Definition 49's `Inactive(zeta)`: `outcome` is `bot` after O-Insert
    /// or a successful withdrawal, `Some(err)` after L-Raise.
    Inactive { outcome: Option<&'static str> },
    /// `Reloading(i, g, omega)`: `remaining_iterations` stands for `i`,
    /// `committed` for `omega`. No `g` is tracked explicitly -- this
    /// model's `Fiber::provides`/`requires` play the paper's `g`/`omega`
    /// role structurally (see `ExtendedFiber` below), matching how the
    /// base-calculus `calculus.rs` above elides `e`'s computational
    /// content.
    Reloading { remaining_iterations: u32, committed: BTreeSet<String> },
    /// `Active(g, omega)`.
    Active { committed: BTreeSet<String> },
    /// `Unloading(g, omega, zeta)`: `outcome` is the `zeta` this
    /// deactivation is headed for (`None` = ordinary L-Leave-initiated
    /// withdrawal, `Some(err)` = L-Raise-initiated).
    Unloading { committed: BTreeSet<String>, outcome: Option<&'static str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedFiber {
    pub requires: BTreeSet<String>,
    pub provides: BTreeSet<String>,
    pub state: ExtendedLifecycle,
}

/// The extended registry (Definition 45, read at the wider state space of
/// Definition 49). `provider_k(gamma)` (Definition 45) and `target_n(gamma)`
/// /`quiet(gamma)` (Definition 46, eq. 45's wider reading) are re-derived
/// here rather than shared with the base `Registry` -- the coeffect
/// context union (eq. 45's second clause) is now restricted to `Active`
/// fibers alone, explicitly excluding `Reloading`/`Unloading`, which the
/// base calculus's two-state model has no way to distinguish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedRegistry {
    pub fibers: HashMap<String, ExtendedFiber>,
}

impl ExtendedRegistry {
    pub fn empty() -> ExtendedRegistry {
        ExtendedRegistry { fibers: HashMap::new() }
    }

    /// eq. 45's `sigma_gamma`, restricted to `Active` per eq. 45's note
    /// under Definition 49: "a fiber whose transition is under way in
    /// either direction reads its coeffects through the omega it holds
    /// and provides none of its own."
    pub fn coeffect_context(&self) -> BTreeSet<String> {
        let mut ctx = BTreeSet::new();
        for fiber in self.fibers.values() {
            if let ExtendedLifecycle::Active { .. } = fiber.state {
                for cap in &fiber.provides {
                    ctx.insert(cap.clone());
                }
            }
        }
        ctx
    }

    pub fn satisfied(&self, name: &str) -> bool {
        let ctx = self.coeffect_context();
        match self.fibers.get(name) {
            Some(fiber) => fiber.requires.iter().all(|dep| ctx.contains(dep)),
            None => false,
        }
    }

    /// `installed_n(gamma)` (Definition 49, eq. 44): any state but
    /// `Inactive`.
    pub fn installed(&self, name: &str) -> bool {
        match self.fibers.get(name) {
            Some(fiber) => !matches!(fiber.state, ExtendedLifecycle::Inactive { .. }),
            None => false,
        }
    }

    /// `target_n(gamma)` (Definition 46), represented as the resolved
    /// dependency set when defined (`requires` all satisfied and not
    /// retired-equivalent) or `None` for `bot`. This model has no
    /// separate retirement flag on `ExtendedFiber` (retirement is
    /// modeled by driving `requires` to an unsatisfiable set via the
    /// caller, matching how `O-Retire` in the base calculus only ever
    /// takes effect through the target view collapsing to `bot`) --
    /// `target_defined` is the boolean form every rule guard below reads.
    fn target_defined(&self, name: &str) -> bool {
        self.fibers.contains_key(name) && self.satisfied(name)
    }

    /// `relied_n(gamma)` (Definition 50, eq. 46): some OTHER installed
    /// fiber's committed view resolves a key to `name`. This is the guard
    /// L-Unload adds beyond the base calculus's L-Unload -- withdrawal
    /// waits for every dependent's committed view to stop naming this
    /// fiber, not merely for the target view to change.
    pub fn relied(&self, name: &str) -> bool {
        self.fibers.iter().any(|(other_name, other)| {
            if other_name == name {
                return false;
            }
            if !self.installed(other_name) {
                return false;
            }
            let committed = match &other.state {
                ExtendedLifecycle::Reloading { committed, .. } => committed,
                ExtendedLifecycle::Active { committed } => committed,
                ExtendedLifecycle::Unloading { committed, .. } => committed,
                ExtendedLifecycle::Inactive { .. } => return false,
            };
            committed.contains(name)
        })
    }

    /// O-Insert (Definition 49's reading: `Inactive` in the conclusion is
    /// `Inactive(bot)`).
    pub fn insert(&self, name: &str, requires: BTreeSet<String>, provides: BTreeSet<String>) -> Option<ExtendedRegistry> {
        if self.fibers.contains_key(name) {
            return None;
        }
        for fiber in self.fibers.values() {
            if !fiber.provides.is_disjoint(&provides) {
                return None;
            }
        }
        let mut next = self.clone();
        next.fibers.insert(
            name.to_string(),
            ExtendedFiber { requires, provides, state: ExtendedLifecycle::Inactive { outcome: None } },
        );
        Some(next)
    }

    /// L-Begin: `Inactive(bot)`, target defined -> `Reloading(e_n, id, omega)`.
    /// `remaining_iterations` seeds from the caller-supplied iteration
    /// count (a plain effect function per Section 4.3.2's closing
    /// paragraph is the degenerate `remaining_iterations = 0` case: "the
    /// first iteration already yields Nothing").
    pub fn begin(&self, name: &str, remaining_iterations: u32) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        if !matches!(fiber.state, ExtendedLifecycle::Inactive { outcome: None }) {
            return None;
        }
        if !self.target_defined(name) {
            return None;
        }
        let omega = fiber.requires.clone();
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Reloading { remaining_iterations, committed: omega };
        Some(next)
    }

    /// L-Iter: `Reloading`, target still equals `omega`, iterations
    /// remain -> stays `Reloading` with one fewer remaining and the same
    /// `omega` (this model has no per-iteration `g`/`h` composition to
    /// witness -- see `ExtendedLifecycle`'s doc comment).
    pub fn iterate(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let (remaining, committed) = match &fiber.state {
            ExtendedLifecycle::Reloading { remaining_iterations, committed } if *remaining_iterations > 0 => {
                (*remaining_iterations, committed.clone())
            }
            _ => return None,
        };
        if self.target_defined(name) && self.fibers[name].requires != committed {
            return None;
        }
        if !self.target_defined(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Reloading { remaining_iterations: remaining - 1, committed };
        Some(next)
    }

    /// L-Finish: `Reloading`, target still `omega`, no iterations remain
    /// -> `Active(g, omega)`.
    pub fn finish(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { remaining_iterations: 0, committed } => committed.clone(),
            _ => return None,
        };
        if !self.target_defined(name) || self.fibers[name].requires != committed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = ExtendedLifecycle::Active { committed };
        Some(next)
    }

    /// L-Divert: `Reloading`, target has CHANGED from `omega` -> aborts
    /// into `Unloading(g o h, omega, bot)`, whichever alternative (abort
    /// mid-iteration vs land one more first) this model collapses into
    /// the single available transition, since it tracks no per-iteration
    /// `h` to compose.
    pub fn divert(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { committed, .. } => committed.clone(),
            _ => return None,
        };
        let target_changed = !self.target_defined(name) || self.fibers[name].requires != committed;
        if !target_changed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: None };
        Some(next)
    }

    /// L-Raise (Section 4.3.4): `Reloading`, the iterator raises ->
    /// `Unloading(g, omega, xi)`. `error` is the paper's `xi in Xi`.
    pub fn raise(&self, name: &str, error: &'static str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { committed, .. } => committed.clone(),
            _ => return None,
        };
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: Some(error) };
        Some(next)
    }

    /// L-Leave: `Active`, target no longer equals `omega` -> `Unloading(g, omega, bot)`.
    pub fn leave(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Active { committed } => committed.clone(),
            _ => return None,
        };
        let target_changed = !self.target_defined(name) || self.fibers[name].requires != committed;
        if !target_changed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: None };
        Some(next)
    }

    /// L-Unload: `Unloading`, NOT relied upon -> `Inactive(zeta)`. This is
    /// the rule Definition 50's guard names: withdrawal waits for every
    /// dependent's committed view to stop naming this fiber (`relied`
    /// above), unlike the base calculus's `L-Unload` which has no such
    /// wait because the base calculus has nowhere for a dependent to be
    /// mid-teardown.
    pub fn unload(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let outcome = match &fiber.state {
            ExtendedLifecycle::Unloading { outcome, .. } => *outcome,
            _ => return None,
        };
        if self.relied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = ExtendedLifecycle::Inactive { outcome };
        Some(next)
    }

    /// O-Retire has no separate representation in this model beyond
    /// removing the fiber's future eligibility to `begin` -- see
    /// `target_defined`'s doc comment. `remove` mirrors the base
    /// calculus's O-Remove: an `Inactive`, non-relied fiber (no committed
    /// view left naming it) may be dropped.
    pub fn remove(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        if !matches!(fiber.state, ExtendedLifecycle::Inactive { .. }) {
            return None;
        }
        if self.relied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.remove(name);
        Some(next)
    }
}
