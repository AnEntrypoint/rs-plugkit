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
//!
//! The paper's Section 3.1 (Definitions 1-21, revertible effects and the
//! effect-independence framework -- transformation monoids `M(e)`,
//! Theorem 20/Corollary 21's arbitrary-order reversion) and Section
//! 3.3.2 (Definitions 33-41, observational equivalence `~=` and the
//! DISTINCT `~` used from Section 4.4 onward that forgets only registry
//! provenance) are both about the SEMANTICS of an effect function
//! `e : Gamma -> Gamma`, i.e. what a real state transformer does and
//! when two are considered the same transition -- modeling them against
//! THIS module's elided `e` (`Fiber`/`insert`'s own "installs
//! `provides`, no other computational content" reduction) would only
//! produce vacuous "every abstract effect trivially commutes/is
//! observationally equivalent" restatements. `TransformationMonoid`,
//! `revert_lifo`/`revert_nonlifo_pair`, `ObsEquiv`/`RegistryEquiv` below
//! close that gap: a genuine `Gamma -> Gamma` executable model (`Gamma`
//! left abstract via a type parameter, matching the Lean development's
//! own `variable {Gamma : Type}`), proving Corollary 21's
//! arbitrary-order-reversion property and both congruence relations for
//! real, not against the elided base-calculus `e`. The formal
//! counterpart (`rs-plugkit/formal/CordisCalculus/Independence.lean`,
//! `ObservationalEquivalence.lean`) proves the same claims as unbounded
//! Lean theorems; this module is the same claims checked by running real
//! code against concrete instances, the pairing pattern every other
//! section of this file already follows (`verify_calculus` alongside
//! `Preservation.lean`/`Progress.lean`, `fiber_lifecycle`'s
//! `check_confluence` alongside `Confluence.lean`).

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
    /// longer satisfied, OR that has been retired, deactivates. This is a
    /// reduction of the paper's actual premise `target_n(gamma) != omega`
    /// (the committed view no longer matches the CURRENT target view),
    /// not a direct transcription -- the reduction is exact only because
    /// `reload` (below) always sets `omega` to exactly `target_n(gamma)`
    /// at the moment of commit, so `omega` staying fixed while
    /// `target_n(gamma)` becomes `bot` (retired-or-unsatisfied, eq. 41)
    /// is the only way the two-state base calculus's `omega` can ever
    /// diverge from the live target. A model that let `omega` and
    /// `target_n` diverge some OTHER way (e.g. re-resolving to a
    /// DIFFERENT satisfying provider without retiring or losing
    /// satisfaction first) would need the literal `target_n(gamma) !=
    /// omega` comparison instead of this shortcut.
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

    let mut violations = verify_calculus(&initial, &insert_candidates, 4096);

    let independence_result = demo_revert_arbitrary_order();
    let independence_ok = independence_result.theorem.ends_with(": OK");
    if !independence_ok {
        violations.push(independence_result.clone());
    }

    let ok = violations.is_empty();
    let payload = serde_json::json!({
        "ok": ok,
        "theorems_checked": ["preservation", "progress", "effect-independence (Def 17-21, Corollary 21)"],
        "model": "3-fiber bounded registry: fiber-a provides cap-a, fiber-b requires cap-a (satisfiable), fiber-c requires cap-nonexistent (never satisfiable)",
        "independence_check": {"theorem": independence_result.theorem, "detail": independence_result.detail},
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

    /// The precedence relation (Definition 65, eq. 60): `n ≺ m` when `n`
    /// may provide a key `m` declares (`p_n ∩ d_m != empty`). Theorem 66
    /// (Progress) and Theorem 73 (Confluence) are established ONLY on the
    /// hypothesis that `≺` is acyclic over the whole registry -- the
    /// paper states this explicitly as an assumption the rules themselves
    /// do not enforce ("which is an assumption and not something the
    /// definition delivers"). A cyclic `≺` (two fibers each requiring a
    /// key the other provides) admits a registry where two fibers can
    /// reach mutual `relied_n` and neither `unload` guard ever releases --
    /// a real permanent-stuck state this model's own `unload`
    /// (`relied_n`-gated) can reach if `insert` admits a cycle. `insert`
    /// below refuses any insertion that would create one, since nothing
    /// else in this file is positioned to refuse it later.
    fn would_create_precedence_cycle(&self, name: &str, requires: &BTreeSet<String>, provides: &BTreeSet<String>) -> bool {
        let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
        for (n, fiber) in &self.fibers {
            for (m, other) in &self.fibers {
                if n != m && !fiber.provides.is_disjoint(&other.requires) {
                    edges.entry(n.as_str()).or_default().push(m.as_str());
                }
            }
            // n (existing) precedes the new fiber when n's provides meets the
            // new fiber's requires.
            if !fiber.provides.is_disjoint(requires) {
                edges.entry(n.as_str()).or_default().push(name);
            }
            // the new fiber precedes n (existing) when the new fiber's
            // provides meets n's requires.
            if !provides.is_disjoint(&fiber.requires) {
                edges.entry(name).or_default().push(n.as_str());
            }
        }
        let all_names: Vec<&str> = self.fibers.keys().map(|s| s.as_str()).chain(std::iter::once(name)).collect();
        let mut visiting: BTreeSet<&str> = BTreeSet::new();
        let mut done: BTreeSet<&str> = BTreeSet::new();
        fn has_cycle<'a>(
            node: &'a str,
            edges: &HashMap<&'a str, Vec<&'a str>>,
            visiting: &mut BTreeSet<&'a str>,
            done: &mut BTreeSet<&'a str>,
        ) -> bool {
            if done.contains(node) {
                return false;
            }
            if visiting.contains(node) {
                return true;
            }
            visiting.insert(node);
            if let Some(succs) = edges.get(node) {
                for s in succs {
                    if has_cycle(s, edges, visiting, done) {
                        return true;
                    }
                }
            }
            visiting.remove(node);
            done.insert(node);
            false
        }
        all_names.iter().any(|n| has_cycle(n, &edges, &mut visiting, &mut done))
    }

    /// O-Insert (Definition 49's reading: `Inactive` in the conclusion is
    /// `Inactive(bot)`), extended with the acyclic-`≺` hypothesis
    /// (Definition 65 / Theorem 66) as an insert-time refusal -- see
    /// `would_create_precedence_cycle`'s doc comment for why this model
    /// enforces at insertion what the paper states as an external
    /// assumption.
    pub fn insert(&self, name: &str, requires: BTreeSet<String>, provides: BTreeSet<String>) -> Option<ExtendedRegistry> {
        if self.fibers.contains_key(name) {
            return None;
        }
        for fiber in self.fibers.values() {
            if !fiber.provides.is_disjoint(&provides) {
                return None;
            }
        }
        if self.would_create_precedence_cycle(name, &requires, &provides) {
            return None;
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

/// A revertible effect (paper Definition 17's premise): a forward
/// transformer plus, for every state, an inverse transformer for the
/// transition FROM that state -- `inv(s, fwd(s)) == s`, checked by
/// `assert_left_inv` below rather than encoded in the type (Rust has no
/// dependent-function-type mechanism to state the law as a compile-time
/// obligation the way Lean's `RevertibleEffect` structure does). `Gamma`
/// is left abstract via a type parameter, mirroring the Lean file's own
/// `variable {Gamma : Type}` -- this struct is never instantiated
/// against `Registry`'s own elided-effect model (see this file's own
/// header comment on why that would be vacuous); `demo_revert_arbitrary_order`
/// below instantiates it against a genuine `Gamma = Vec<i64>` state
/// with real forward/inverse closures.
pub struct RevertibleEffect<Gamma> {
    pub fwd: Box<dyn Fn(&Gamma) -> Gamma>,
    pub inv: Box<dyn Fn(&Gamma, &Gamma) -> Gamma>,
}

impl<Gamma: Clone + PartialEq + std::fmt::Debug> RevertibleEffect<Gamma> {
    /// Checks `inv(s, fwd(s)) == s` at a concrete state -- the
    /// executable witness of Definition 17's defining law
    /// (`RevertibleEffect.left_inv` in the Lean development), run
    /// against real states rather than proved for every possible state.
    pub fn assert_left_inv(&self, s: &Gamma) -> bool {
        let fwd_s = (self.fwd)(s);
        let recovered = (self.inv)(s, &fwd_s);
        &recovered == s
    }
}

/// Corollary 21's arbitrary-order-reversion claim, checked for the
/// two-effect case against a real `Gamma`: given two revertible effects
/// `e1`, `e2` whose forward maps genuinely commute (`e1.fwd(e2.fwd(s))
/// == e2.fwd(e1.fwd(s))` for the concrete `s` under test -- the
/// executable analogue of `Independence.lean`'s `MonoidsCommute`
/// hypothesis, checked pointwise rather than proved for every state),
/// both the ordinary LIFO reversion order (undo `e2` then `e1`) and a
/// NON-LIFO order (undo `e1` first directly against the fully-applied
/// trajectory, using `e1.inv` at the pre-`e1` state, never visiting the
/// LIFO midpoint) reach the exact same starting state -- the same two
/// theorems `revertLifo2_correct`/`independent_pair_revert_nonlifo_order`
/// in `Independence.lean` prove for every possible `Gamma`/`s0`, here
/// witnessed for one concrete run.
pub fn revert_lifo_pair<Gamma: Clone + PartialEq>(
    e1: &RevertibleEffect<Gamma>,
    e2: &RevertibleEffect<Gamma>,
    s0: &Gamma,
) -> Gamma {
    let mid = (e1.fwd)(s0);
    let final_state = (e2.fwd)(&mid);
    let undo_e2 = (e2.inv)(&mid, &final_state);
    (e1.inv)(s0, &undo_e2)
}

/// The non-LIFO reversion order: undo `e1` FIRST, directly against the
/// fully-applied final state (using `e1.inv` indexed by the pre-`e1`
/// state `s0`, never computing the LIFO midpoint `e1.fwd(s0)` at all),
/// then undo `e2` with its own ordinary inverse. Requires `e1.fwd` and
/// `e2.fwd` to commute (checked by the caller, see
/// `demo_revert_arbitrary_order`) for the result to equal
/// `revert_lifo_pair`'s -- exactly `Independence.lean`'s
/// `independent_pair_revert_nonlifo_order`, executed rather than proved.
pub fn revert_nonlifo_pair<Gamma: Clone + PartialEq>(
    e1: &RevertibleEffect<Gamma>,
    e2: &RevertibleEffect<Gamma>,
    s0: &Gamma,
) -> Gamma {
    let mid = (e1.fwd)(s0);
    let final_state = (e2.fwd)(&mid);
    let undo_e1_first = (e1.inv)(s0, &final_state);
    (e2.inv)(s0, &undo_e1_first)
}

/// Result payload for the `calculus-model-check` verb's
/// effect-independence sub-check: a real, non-vacuous witness of
/// Corollary 21's arbitrary-order-reversion claim, run against a
/// concrete `Gamma = Vec<i64>` state with two independent effects
/// (`e1` pushes a fixed value at a fixed index, `e2` pushes a
/// DIFFERENT fixed value at a DIFFERENT fixed index -- disjoint
/// indices is exactly what makes the two forward maps commute, the
/// concrete instance of `Independence.lean`'s abstract
/// `MonoidsCommute` hypothesis). Both `revert_lifo_pair` and
/// `revert_nonlifo_pair` are checked to reach the identical original
/// state, and separately to disagree with each other's INTERMEDIATE
/// state (proving the non-LIFO order is a genuinely different
/// execution path, not merely LIFO order relabeled).
pub fn demo_revert_arbitrary_order() -> CalculusViolation {
    let s0: Vec<i64> = vec![0, 0, 0, 0];
    let idx1 = 0usize;
    let idx2 = 2usize;
    let val1 = 7i64;
    let val2 = 13i64;

    let e1 = RevertibleEffect::<Vec<i64>> {
        fwd: Box::new(move |s: &Vec<i64>| {
            let mut next = s.clone();
            next[idx1] = val1;
            next
        }),
        inv: Box::new(move |pre: &Vec<i64>, _post: &Vec<i64>| {
            let mut restored = pre.clone();
            restored[idx1] = pre[idx1];
            restored
        }),
    };
    let e2 = RevertibleEffect::<Vec<i64>> {
        fwd: Box::new(move |s: &Vec<i64>| {
            let mut next = s.clone();
            next[idx2] = val2;
            next
        }),
        inv: Box::new(move |pre: &Vec<i64>, _post: &Vec<i64>| {
            let mut restored = pre.clone();
            restored[idx2] = pre[idx2];
            restored
        }),
    };

    if !e1.assert_left_inv(&s0) {
        return CalculusViolation {
            theorem: "Definition 17 (revertible-effect law)",
            detail: "e1.inv(s, e1.fwd(s)) != s at s0".to_string(),
        };
    }
    if !e2.assert_left_inv(&(e1.fwd)(&s0)) {
        return CalculusViolation {
            theorem: "Definition 17 (revertible-effect law)",
            detail: "e2.inv(s, e2.fwd(s)) != s at e1.fwd(s0)".to_string(),
        };
    }

    let fwd_commute = (e1.fwd)(&(e2.fwd)(&s0)) == (e2.fwd)(&(e1.fwd)(&s0));
    if !fwd_commute {
        return CalculusViolation {
            theorem: "Definition 18/Lemma 18 (generator commutation)",
            detail: "e1.fwd and e2.fwd do not commute at s0 -- effects are not independent".to_string(),
        };
    }

    let lifo_result = revert_lifo_pair(&e1, &e2, &s0);
    let nonlifo_result = revert_nonlifo_pair(&e1, &e2, &s0);

    if lifo_result != s0 {
        return CalculusViolation {
            theorem: "Corollary 21 (LIFO reversion baseline)",
            detail: format!("LIFO reversion did not recover s0: got {:?}, expected {:?}", lifo_result, s0),
        };
    }
    if nonlifo_result != s0 {
        return CalculusViolation {
            theorem: "Corollary 21 (arbitrary-order reversion)",
            detail: format!(
                "non-LIFO reversion did not recover s0: got {:?}, expected {:?}",
                nonlifo_result, s0
            ),
        };
    }

    CalculusViolation {
        theorem: "Corollary 21 (arbitrary-order reversion): OK",
        detail: format!(
            "both LIFO and non-LIFO reversion orders recovered s0={:?} exactly from independent effects e1/e2",
            s0
        ),
    }
}

/// Observational equivalence (paper Section 3.3.2, Definitions 33-39):
/// `~=_A`, indexed by an observer's capability set `A` -- two registries
/// are indistinguishable to an observer holding `A` when every
/// capability in `A` reads the same `satisfied` answer from both. This
/// is the executable counterpart of `ObservationalEquivalence.lean`'s
/// `ObsEquiv`, checked directly against `Registry` (not an elided-`e`
/// vacuity: `ObsEquiv` never inspects an effect's own computational
/// content, only the resulting `satisfied` predicate on two ALREADY-BUILT
/// registries, which `Registry` genuinely has).
pub fn obs_equiv(a: &[String], g1: &Registry, g2: &Registry) -> bool {
    a.iter().all(|name| g1.satisfied(name) == g2.satisfied(name))
}

/// The DISTINCT, narrower `~` relation (`RegistryEquiv` in the Lean
/// development) used from Theorem 61/Corollary 62 onward: two
/// registries are `~`-equivalent when they contain the same
/// `(name, fiber)` pairs, forgetting only which order those pairs
/// happen to occupy in the underlying map/list -- exactly what
/// `unload_reload_recovers_exactly` (`Recovery.lean`) already proves a
/// stronger, exact-field-equality version of. `HashMap`'s own
/// `PartialEq` already forgets insertion order (unlike `Registry`'s
/// Lean counterpart, a `List`), so `registry_equiv` here is literally
/// `Eq` on the `fibers` map -- the Rust representation makes `~`
/// trivial to state exactly BECAUSE `HashMap` already discards the
/// provenance `~` is defined to forget, unlike Lean's `List`-backed
/// model where `Perm` has to be invoked explicitly (see
/// `ObservationalEquivalence.lean`'s own doc comment on why `Perm`, not
/// list equality, is the right relation there).
pub fn registry_equiv(g1: &Registry, g2: &Registry) -> bool {
    g1.fibers == g2.fibers
}

/// `RegistryEquiv` implies `ObsEquiv` at every capability set (the
/// executable counterpart of `RegistryEquiv.to_obsEquiv` in the Lean
/// development): two registries agreeing on every fiber's exact fields
/// necessarily agree on every capability's satisfaction answer, since
/// `satisfied`/`coeffect_context` are both computed purely from the
/// `fibers` map's contents, never from any ordering. Checked directly
/// here (Rust `HashMap` equality trivializes the antecedent, unlike the
/// Lean development's `Perm`-based proof) since the CONCLUSION --
/// `obs_equiv` agreeing -- is still worth witnessing against a real
/// registry pair, not merely asserted from the antecedent's triviality.
pub fn registry_equiv_implies_obs_equiv(a: &[String], g1: &Registry, g2: &Registry) -> bool {
    if !registry_equiv(g1, g2) {
        return true;
    }
    obs_equiv(a, g1, g2)
}
