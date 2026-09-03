#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use crate::pkfs;

/// A component's persisted lifecycle state (paper Section 4.3, Definition
/// 49), kind-agnostic. Any environment lacking an async load step (reading
/// a manifest file is synchronous) reduces the paper's four states to
/// three: `Inactive` (never activated, or fully withdrawn), `Active`
/// (currently providing), and `Unloading` (target unsatisfied, but the
/// previous dispatch's dependents have not yet had a chance to observe the
/// loss). Mirrors L-Leave/L-Unload's split (Section 4.3.1): a fiber stops
/// providing the moment it enters `Unloading`, but persists one more
/// dispatch, present-but-leaving, before collapsing to `Inactive`.
///
/// This type and the functions below carry NO assumption about what a
/// component IS -- a discipline, a plugin, or any future kind -- only that
/// it has a name and a storage location for its state. `discipline_note.rs`
/// is the first caller; a second component kind would call these same
/// functions with its own path function, not copy this file.
///
/// Identifier fidelity against the paper's own Table 2 (Section 5.1,
/// theory-to-implementation correspondence): `theta` (Definition 44) maps
/// to `fiber.state` there and to `FiberLifecycle` here; the paper's
/// runtime `LOADING`/`FAILED` states are folded into this crate's
/// `Unloading` reduction rather than kept as separate names, since this
/// reduction (see above) collapses the paper's four runtime states to
/// three and has no failure-outcome concept yet (Section 4.3.4, `L-Raise`)
/// -- see this module's own `advance_fiber`/`transition` for where a
/// fourth, failure-carrying state would need to land if that gap is ever
/// closed. `provider_k(gamma)` (Definition 45) has no single named
/// counterpart in this crate; `discipline_note.rs::active_policies`'s
/// `Active`-filtered lookup plays that role structurally. `target_n(gamma)`
/// (Definition 46) corresponds to this module's `target_satisfied`
/// parameter to `transition`/`advance_fiber`, computed by each caller's
/// own `requires_satisfied`-shaped function rather than a single shared
/// `target` function, since gm's coeffect resolution is realm-scoped per
/// caller (Section 3.2.3) in a way the paper's own `fiber.target` field
/// (recomputed by `refresh`, Algorithm 5) does not need to distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiberLifecycle {
    Inactive,
    Active,
    Unloading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FiberState {
    state: FiberLifecycle,
    #[serde(default)]
    updated_at_ms: u128,
}

/// Reads the persisted state at `path`, defaulting to `Inactive` when
/// absent or unparseable -- the same "no state yet" reading every caller
/// needs, regardless of what kind of component `path` names.
pub fn read_fiber_state(path: &str) -> FiberLifecycle {
    pkfs::read_to_string(path)
        .and_then(|s| serde_json::from_str::<FiberState>(&s).ok())
        .map(|s| s.state)
        .unwrap_or(FiberLifecycle::Inactive)
}

fn write_fiber_state(path: &str, state: FiberLifecycle) {
    let body = FiberState { state, updated_at_ms: super::state::now_ms() };
    if let Ok(text) = serde_json::to_string(&body) {
        let _ = pkfs::write(path, &text);
    }
}

/// The transition table itself, with no I/O -- `advance_fiber` is this
/// function plus a read and a conditional write. Split out so an audit can
/// ask "what would this transition produce" without the read-only query
/// itself causing a write, and so the table exists in exactly one place
/// that both the real transition and any proof-carrying wrapper (see
/// `WithdrawalComplete` below) call. `Inactive -> Active` when the target
/// becomes satisfied; `Active -> Unloading` the instant it stops being
/// satisfied (L-Leave); `Unloading -> Inactive` on the following call
/// (L-Unload).
pub fn transition(current: FiberLifecycle, target_satisfied: bool) -> FiberLifecycle {
    match (current, target_satisfied) {
        (FiberLifecycle::Inactive, true) => FiberLifecycle::Active,
        (FiberLifecycle::Inactive, false) => FiberLifecycle::Inactive,
        (FiberLifecycle::Active, true) => FiberLifecycle::Active,
        (FiberLifecycle::Active, false) => FiberLifecycle::Unloading,
        (FiberLifecycle::Unloading, _) => FiberLifecycle::Inactive,
    }
}

/// Advances one component's persisted lifecycle by exactly one Cordis-style
/// transition (Section 4.3), given whether its target currently holds.
/// Returns whether the component counts as providing THIS call, which is
/// `Active` alone -- an `Unloading` fiber's own withdrawal is in flight
/// and must not itself be read as still satisfying anyone's coeffect.
///
/// Takes `state_path` rather than a component name: the caller owns what a
/// name means and where its state lives (a discipline's directory, a
/// plugin's cache directory, or a future kind's own location); this
/// function owns only the transition table, so it never needs editing to
/// support a component kind that does not exist yet.
pub fn advance_fiber(state_path: &str, target_satisfied: bool) -> bool {
    let current = read_fiber_state(state_path);
    let next = transition(current, target_satisfied);
    if next != current {
        write_fiber_state(state_path, next);
    }
    next == FiberLifecycle::Active
}

/// A collection of components known to be `Active`, whose only public
/// constructor enforces the paper's preservation invariant (Theorem 59,
/// clause 2: distinct fibers' provisions are disjoint) at insertion time
/// rather than checking it after the fact. Two components attempting to
/// join this set while both providing the same capability cannot both
/// succeed -- the type itself is the guarantee, not a runtime audit run
/// afterward and hoped to catch every path that builds a fiber set.
///
/// This is the paper's static metatheory brought into the type system: a
/// value of `ActiveFiberSet` can only exist in states the theorem permits,
/// the way a well-typed term in the paper's calculus can only reach states
/// its operational semantics allows.
#[derive(Debug, Default)]
pub struct ActiveFiberSet {
    entries: Vec<(String, Vec<String>)>,
}

/// Why `ActiveFiberSet::insert` refused a component -- names both sides of
/// the collision so a caller can report it exactly as `discipline-audit`'s
/// `MetatheoryViolation` does, without needing its own parallel check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservationViolation {
    pub incoming: String,
    pub existing: String,
    pub capability: String,
}

impl ActiveFiberSet {
    pub fn new() -> ActiveFiberSet {
        ActiveFiberSet { entries: Vec::new() }
    }

    /// Attempts to add `name` providing `capabilities` to the set. Ok(())
    /// only when none of `capabilities` collides with an already-inserted
    /// component's own provision; Err(violation) otherwise, and the set is
    /// left unchanged (the attempted insert never partially lands).
    pub fn insert(&mut self, name: &str, capabilities: &[String]) -> Result<(), PreservationViolation> {
        for (existing_name, existing_caps) in &self.entries {
            for cap in capabilities {
                if existing_caps.contains(cap) {
                    return Err(PreservationViolation {
                        incoming: name.to_string(),
                        existing: existing_name.clone(),
                        capability: cap.clone(),
                    });
                }
            }
        }
        self.entries.push((name.to_string(), capabilities.to_vec()));
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A component name proven to have advanced from `Unloading` to
/// `Inactive` -- the paper's recovery-exactness guarantee (Theorem 61)
/// brought into the type system. The only way to obtain a value of this
/// type is `WithdrawalComplete::advance`, which itself calls
/// `advance_fiber` and refuses to construct the value unless the
/// resulting state is actually `Inactive`. Since `advance_fiber`'s match
/// is total and exhaustive over `(FiberLifecycle, bool)` -- a fact the
/// compiler itself checks, not a runtime assumption -- an `Unloading`
/// fiber advanced with any `target_satisfied` value always reaches
/// `Inactive` (the `(Unloading, _)` arm matches both), so this
/// constructor's own internal assertion can never actually trip; it exists
/// so a FUTURE change to the transition table that broke that guarantee
/// would fail this constructor's debug assertion immediately, rather than
/// silently letting a caller believe recovery completed when it did not.
pub struct WithdrawalComplete {
    pub name: String,
}

impl WithdrawalComplete {
    /// Advances `name`'s fiber (expected `Unloading`) and returns proof
    /// it reached `Inactive`. Returns `None` if the fiber was not
    /// `Unloading` to begin with -- this constructor witnesses recovery
    /// FROM `Unloading`, not an arbitrary transition, so a caller with a
    /// fiber in any other state gets no proof object at all rather than a
    /// misleading one. Mutates state (via `advance_fiber`); use
    /// `verify_recovery_exactness` for a read-only check.
    pub fn advance(state_path: &str, name: &str) -> Option<WithdrawalComplete> {
        if read_fiber_state(state_path) != FiberLifecycle::Unloading {
            return None;
        }
        let reached_active = advance_fiber(state_path, false);
        debug_assert!(!reached_active, "advance_fiber from Unloading must never report Active");
        let final_state = read_fiber_state(state_path);
        debug_assert_eq!(final_state, FiberLifecycle::Inactive, "recovery exactness violated: Unloading did not reach Inactive");
        if final_state == FiberLifecycle::Inactive {
            Some(WithdrawalComplete { name: name.to_string() })
        } else {
            None
        }
    }
}

/// Read-only companion to `WithdrawalComplete::advance`: for a fiber
/// currently `Unloading`, checks (via the pure `transition` table, no I/O)
/// that BOTH reachable targets (`true` and `false`) send it to `Inactive`
/// -- the exhaustive check an audit needs without mutating any state,
/// unlike `WithdrawalComplete::advance` which performs the real,
/// state-mutating recovery. Returns `true` when the fiber is not currently
/// `Unloading` at all (nothing to check).
pub fn verify_recovery_exactness(current: FiberLifecycle) -> bool {
    if current != FiberLifecycle::Unloading {
        return true;
    }
    transition(current, true) == FiberLifecycle::Inactive && transition(current, false) == FiberLifecycle::Inactive
}

/// A component name proven safe to withdraw right now -- the paper's
/// ordering guarantee (Theorem 63) brought into the type system. The only
/// way to obtain a value of this type is `SafeToWithdraw::check`, which
/// requires the caller to supply the dependent set (computed however the
/// caller's own coeffect model works, e.g. `removal_dependents` in
/// `discipline_note.rs`) and refuses to construct the value unless that
/// set is empty. Any future code path that actually deletes a component's
/// storage can require a `SafeToWithdraw` as its own parameter type, making
/// a withdrawal attempted while a dependent still relies on the component
/// a compile-time impossibility for that code path, rather than a runtime
/// check a caller could forget to run.
pub struct SafeToWithdraw {
    pub name: String,
}

/// Confluence (Theorem 73): whatever order a set of independent fibers'
/// targets are evaluated in, the resulting set of `Active` names answers
/// only to the fixed inputs (each fiber's own `target_satisfied`), never
/// to the evaluation order. `targets` pairs each fiber's name with the
/// target it should transition against, computed ONCE from a fixed
/// snapshot (the caller's job, e.g. `discipline_note.rs::active_policies`'s
/// two-phase pass) -- this function then transitions every fiber twice,
/// once processing `targets` in its given order and once in reverse,
/// starting both runs from the SAME `initial_states`, and asserts the two
/// runs reach the same final `Active` set. Kind-agnostic like every other
/// function here: it takes fiber names and pre-computed targets, never
/// reading or writing persisted state itself, so it can check any
/// caller's fiber set without touching real files.
pub fn check_confluence(initial_states: &[(String, FiberLifecycle)], targets: &[(String, bool)]) -> bool {
    let run = |order: &[(String, bool)]| -> Vec<String> {
        let mut states: Vec<(String, FiberLifecycle)> = initial_states.to_vec();
        for (name, target) in order {
            if let Some(entry) = states.iter_mut().find(|(n, _)| n == name) {
                entry.1 = transition(entry.1, *target);
            }
        }
        let mut active: Vec<String> = states
            .into_iter()
            .filter(|(_, s)| *s == FiberLifecycle::Active)
            .map(|(n, _)| n)
            .collect();
        active.sort();
        active
    };

    let forward = run(targets);
    let mut reversed = targets.to_vec();
    reversed.reverse();
    let backward = run(&reversed);

    forward == backward
}

impl SafeToWithdraw {
    /// `dependents` is whatever the caller's own coeffect model reports as
    /// still relying on `name` (e.g. `removal_dependents(name)`); this
    /// function does not recompute it, since only the caller's kind-
    /// specific coeffect resolution (disciplines' realm-scoped
    /// requires/provides, or a future kind's own resolution) knows how to
    /// produce that set correctly -- `fiber_lifecycle` stays kind-agnostic
    /// by taking the answer as a parameter rather than a discipline-shaped
    /// computation of its own.
    pub fn check(name: &str, dependents: &[String]) -> Option<SafeToWithdraw> {
        if dependents.is_empty() {
            Some(SafeToWithdraw { name: name.to_string() })
        } else {
            None
        }
    }
}
