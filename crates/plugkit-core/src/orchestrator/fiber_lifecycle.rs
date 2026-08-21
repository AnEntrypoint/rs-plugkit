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

/// Advances one component's persisted lifecycle by exactly one Cordis-style
/// transition (Section 4.3), given whether its target currently holds.
/// `Inactive -> Active` when the target becomes satisfied; `Active ->
/// Unloading` the instant it stops being satisfied (L-Leave); `Unloading
/// -> Inactive` on the following call (L-Unload). Returns whether the
/// component counts as providing THIS call, which is `Active` alone -- an
/// `Unloading` fiber's own withdrawal is in flight and must not itself be
/// read as still satisfying anyone's coeffect.
///
/// Takes `state_path` rather than a component name: the caller owns what a
/// name means and where its state lives (a discipline's directory, a
/// plugin's cache directory, or a future kind's own location); this
/// function owns only the transition table, so it never needs editing to
/// support a component kind that does not exist yet.
pub fn advance_fiber(state_path: &str, target_satisfied: bool) -> bool {
    let current = read_fiber_state(state_path);
    let next = match (current, target_satisfied) {
        (FiberLifecycle::Inactive, true) => FiberLifecycle::Active,
        (FiberLifecycle::Inactive, false) => FiberLifecycle::Inactive,
        (FiberLifecycle::Active, true) => FiberLifecycle::Active,
        (FiberLifecycle::Active, false) => FiberLifecycle::Unloading,
        (FiberLifecycle::Unloading, _) => FiberLifecycle::Inactive,
    };
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
