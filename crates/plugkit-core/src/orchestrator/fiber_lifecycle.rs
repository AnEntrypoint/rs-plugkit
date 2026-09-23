#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use crate::pkfs;

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

pub fn transition(current: FiberLifecycle, target_satisfied: bool) -> FiberLifecycle {
    match (current, target_satisfied) {
        (FiberLifecycle::Inactive, true) => FiberLifecycle::Active,
        (FiberLifecycle::Inactive, false) => FiberLifecycle::Inactive,
        (FiberLifecycle::Active, true) => FiberLifecycle::Active,
        (FiberLifecycle::Active, false) => FiberLifecycle::Unloading,
        (FiberLifecycle::Unloading, _) => FiberLifecycle::Inactive,
    }
}

pub fn advance_fiber(state_path: &str, target_satisfied: bool) -> bool {
    let current = read_fiber_state(state_path);
    let next = transition(current, target_satisfied);
    if next != current {
        write_fiber_state(state_path, next);
    }
    next == FiberLifecycle::Active
}

#[derive(Debug, Default)]
pub struct ActiveFiberSet {
    entries: Vec<(String, Vec<String>)>,
}

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

pub fn verify_recovery_exactness(current: FiberLifecycle) -> bool {
    if current != FiberLifecycle::Unloading {
        return true;
    }
    transition(current, true) == FiberLifecycle::Inactive && transition(current, false) == FiberLifecycle::Inactive
}

pub struct SafeToWithdraw {
    pub name: String,
}

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
    pub fn check(name: &str, dependents: &[String]) -> Option<SafeToWithdraw> {
        if dependents.is_empty() {
            Some(SafeToWithdraw { name: name.to_string() })
        } else {
            None
        }
    }
}
