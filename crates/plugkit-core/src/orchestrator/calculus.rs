#![cfg(target_arch = "wasm32")]

//! A direct, gm-independent implementation of the Cordis paper's Section
//! 4.2 base calculus: an abstract `Registry` of named `Fiber`s, each
//! carrying a coeffect specification (`requires`) and provision
//! (`provides`), advanced by the five base rules (O-Insert, O-Retire,
//! O-Remove, L-Reload, L-Unload). Every metatheory check elsewhere in
//! this crate (`discipline_note.rs`'s `discipline-audit`) runs over ONE
//! gm-specific instantiation of the paper's model (disciplines, or
//! memory/codeinsight namespaces) at its CURRENT state alone. This module
//! is the calculus itself, with no discipline/plugin/namespace concept
//! anywhere in it, and `verify_calculus` below exhaustively enumerates
//! EVERY state reachable from an initial registry under every legal rule
//! application (bounded by a small fiber/capability alphabet), checking
//! the metatheory holds for the whole reachable state space rather than
//! for whatever state gm happens to be in when audited.

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

    /// The satisfaction predicate (Definition 46: `sigma |= d`): every
    /// capability `name`'s fiber requires is in the current coeffect
    /// context.
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
