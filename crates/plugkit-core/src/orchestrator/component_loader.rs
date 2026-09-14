#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use super::coeffect_realm::RealmTable;
use super::gm_dir;
use crate::pkfs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Isolate {
    None,
    Local,
    Global { realm: String },
}

impl Isolate {
    pub fn realm_for(&self, entry_id: &str) -> Option<String> {
        match self {
            Isolate::None => None,
            Isolate::Local => Some(format!("local:{entry_id}")),
            Isolate::Global { realm } => Some(realm.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEntry {
    pub id: String,
    pub url: String,
    #[serde(default = "default_isolate")]
    pub isolate: Isolate,
    #[serde(default)]
    pub intercept: BTreeMap<String, String>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub isolated_keys: Vec<String>,
}

fn default_isolate() -> Isolate {
    Isolate::None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOp {
    Rebuild,
    ReassignRealms,
    UpdateIntercept,
    ApplyConfig,
    ToggleDisabled,
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileDecision {
    pub id: String,
    pub op: ReconcileOp,
    pub changed_fields: Vec<String>,
}

pub fn diff_entries(previous: &[ComponentEntry], next: &[ComponentEntry]) -> Vec<ReconcileDecision> {
    let prev_by_id: BTreeMap<&str, &ComponentEntry> = previous.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut out = Vec::new();

    for entry in next {
        match prev_by_id.get(entry.id.as_str()) {
            None => out.push(ReconcileDecision {
                id: entry.id.clone(),
                op: ReconcileOp::Rebuild,
                changed_fields: vec!["<new>".to_string()],
            }),
            Some(prev) => {
                let mut changed = Vec::new();
                if prev.id != entry.id || prev.url != entry.url {
                    changed.push(if prev.id != entry.id { "id".to_string() } else { "url".to_string() });
                    out.push(ReconcileDecision { id: entry.id.clone(), op: ReconcileOp::Rebuild, changed_fields: changed });
                    continue;
                }
                if prev.isolate != entry.isolate {
                    changed.push("isolate".to_string());
                }
                if prev.intercept != entry.intercept {
                    changed.push("intercept".to_string());
                }
                if prev.config != entry.config {
                    changed.push("config".to_string());
                }
                if prev.disabled != entry.disabled {
                    changed.push("disabled".to_string());
                }
                if changed.is_empty() {
                    continue;
                }
                let op = if changed.contains(&"disabled".to_string()) {
                    ReconcileOp::ToggleDisabled
                } else if changed.contains(&"isolate".to_string()) {
                    ReconcileOp::ReassignRealms
                } else if changed.contains(&"config".to_string()) {
                    ReconcileOp::ApplyConfig
                } else {
                    ReconcileOp::UpdateIntercept
                };
                out.push(ReconcileDecision { id: entry.id.clone(), op, changed_fields: changed });
            }
        }
    }

    for entry in previous {
        if !next.iter().any(|e| e.id == entry.id) {
            out.push(ReconcileDecision {
                id: entry.id.clone(),
                op: ReconcileOp::ToggleDisabled,
                changed_fields: vec!["<removed>".to_string()],
            });
        }
    }

    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmKeyDiff {
    pub key: String,
    pub old_realm: String,
    pub new_realm: String,
    pub entry_tag: u64,
    pub provider_tag: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmReassignment {
    pub entry_id: String,
    pub key_diffs: Vec<RealmKeyDiff>,
    pub binding_moved: Vec<String>,
    pub affected_dependents: Vec<String>,
}

fn next_tag(state: &mut LoaderState) -> u64 {
    state.tag_counter += 1;
    state.tag_counter
}

pub fn patch_isolation(
    state: &mut LoaderState,
    entry: &ComponentEntry,
    new_isolate: &Isolate,
    all_entries: &[ComponentEntry],
) -> RealmReassignment {
    let rho = entry_realm_table(entry);
    let rho_prime = {
        let mut e = entry.clone();
        e.isolate = new_isolate.clone();
        entry_realm_table(&e)
    };

    let mut delta: Vec<String> = entry
        .isolated_keys
        .iter()
        .filter(|k| rho.realm_of(k) != rho_prime.realm_of(k))
        .cloned()
        .collect();
    delta.sort();
    delta.dedup();

    let mut key_diffs = Vec::new();
    let mut binding_moved = Vec::new();
    let mut affected_dependents: BTreeSet<String> = BTreeSet::new();

    for key in &delta {
        let old_realm = rho.realm_of(key);
        let new_realm = rho_prime.realm_of(key);
        let entry_tag = next_tag(state);
        state.entry_delta_tags.insert((entry.id.clone(), key.clone()), entry_tag);

        let provider_id = state.provider_of.get(&(key.clone(), old_realm.clone())).cloned();
        let provider_tag = provider_id
            .as_ref()
            .and_then(|pid| state.entry_delta_tags.get(&(pid.clone(), key.clone())).copied());

        let own_binding = provider_id.as_deref() == Some(entry.id.as_str())
            && !state.provider_of.contains_key(&(key.clone(), new_realm.clone()));
        if own_binding {
            state.provider_of.remove(&(key.clone(), old_realm.clone()));
            state.provider_of.insert((key.clone(), new_realm.clone()), entry.id.clone());
            binding_moved.push(key.clone());
        }

        for dep in all_entries {
            if dep.id == entry.id {
                continue;
            }
            let dep_realm = entry_realm_table(dep).realm_of(key);
            if dep_realm != old_realm && dep_realm != new_realm {
                continue;
            }
            let dep_tag = state.entry_delta_tags.get(&(dep.id.clone(), key.clone())).copied();
            let owned_old = dep_tag == Some(entry_tag);
            let owned_new = dep_tag == provider_tag && provider_tag.is_some();
            if owned_old != owned_new {
                affected_dependents.insert(dep.id.clone());
            }
        }

        key_diffs.push(RealmKeyDiff { key: key.clone(), old_realm, new_realm, entry_tag, provider_tag });
    }

    RealmReassignment {
        entry_id: entry.id.clone(),
        key_diffs,
        binding_moved,
        affected_dependents: affected_dependents.into_iter().collect(),
    }
}

fn entry_realm_table(entry: &ComponentEntry) -> RealmTable {
    let mut table = RealmTable::new();
    if let Some(realm) = entry.isolate.realm_for(&entry.id) {
        for key in &entry.isolated_keys {
            table.isolate(key, &realm);
        }
    }
    table
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoaderState {
    #[serde(default)]
    pub tag_counter: u64,
    #[serde(default)]
    pub entry_delta_tags: BTreeMap<(String, String), u64>,
    #[serde(default)]
    pub provider_of: BTreeMap<(String, String), String>,
}

fn state_path() -> std::path::PathBuf {
    gm_dir().join("component-loader").join("loader-state.json")
}

pub fn read_state() -> LoaderState {
    pkfs::read_to_string(&state_path().to_string_lossy())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_state(state: &LoaderState) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = pkfs::write(&state_path().to_string_lossy(), &text);
    }
}

pub type ImportGraph = BTreeMap<String, Vec<String>>;

fn get_imports<'a>(graph: &'a ImportGraph, url: &str) -> &'a [String] {
    graph.get(url).map(|v| v.as_slice()).unwrap_or(&[])
}

pub fn classify(stashed: &BTreeSet<String>, externals: &BTreeSet<String>, graph: &ImportGraph) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut accepted: BTreeSet<String> = stashed.clone();
    let mut declined: BTreeSet<String> = externals.clone();
    let mut pending: BTreeSet<String> = BTreeSet::new();

    for url in stashed {
        for imp in get_imports(graph, url) {
            if !accepted.contains(imp) && !declined.contains(imp) {
                pending.insert(imp.clone());
            }
        }
    }

    loop {
        let mut progress = false;
        let mut next_pending = pending.clone();
        for url in &pending {
            let imports = get_imports(graph, url);
            if imports.iter().any(|i| accepted.contains(i)) {
                accepted.insert(url.clone());
                next_pending.remove(url);
                progress = true;
            } else if !imports.is_empty() && imports.iter().all(|i| declined.contains(i)) {
                declined.insert(url.clone());
                next_pending.remove(url);
                progress = true;
            } else {
                for imp in imports {
                    if !accepted.contains(imp) && !declined.contains(imp) {
                        next_pending.insert(imp.clone());
                    }
                }
            }
        }
        pending = next_pending;
        if !progress {
            break;
        }
    }

    declined.extend(pending);

    (accepted, declined)
}

pub fn get_dependencies(root: &str, declined: &BTreeSet<String>, graph: &ImportGraph) -> BTreeSet<String> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(url) = stack.pop() {
        if deps.contains(&url) || declined.contains(&url) {
            continue;
        }
        deps.insert(url.clone());
        for child in get_imports(graph, &url) {
            if !deps.contains(child) {
                stack.push(child.clone());
            }
        }
    }
    deps
}

pub fn detect(entries: &[ComponentEntry], accepted: &BTreeSet<String>, declined: &BTreeSet<String>, graph: &ImportGraph) -> (Vec<String>, BTreeSet<String>) {
    let mut accepted = accepted.clone();
    let mut stale_entries = Vec::new();
    for entry in entries {
        let tree = get_dependencies(&entry.url, declined, graph);
        if tree.iter().any(|u| accepted.contains(u)) {
            accepted.extend(tree);
            stale_entries.push(entry.id.clone());
        }
    }
    (stale_entries, accepted)
}

pub type ModuleBackup = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReloadOutcome {
    Committed { reloaded: Vec<String> },
    RolledBack { reloaded_from_backup: Vec<String>, error: String },
}

pub trait FiberSwap {
    fn dispose(&mut self, entry_id: &str);
    fn instantiate(&mut self, entry_id: &str, url: &str, source: &str, config: &serde_json::Value) -> Result<String, String>;
}

pub fn reload(
    stale_entries: &[ComponentEntry],
    sources: &BTreeMap<String, String>,
    backup: &ModuleBackup,
    swap: &mut dyn FiberSwap,
) -> ReloadOutcome {
    let mut disposed: Vec<String> = Vec::new();
    let mut reloaded: Vec<String> = Vec::new();

    for entry in stale_entries {
        swap.dispose(&entry.id);
        disposed.push(entry.id.clone());
        let source = sources.get(&entry.url).cloned().unwrap_or_default();
        match swap.instantiate(&entry.id, &entry.url, &source, &entry.config) {
            Ok(_new_source) => {
                reloaded.push(entry.id.clone());
            }
            Err(error) => {
                let mut restored = Vec::new();
                for e in stale_entries {
                    swap.dispose(&e.id);
                    let backup_source = backup.get(&e.url).cloned().unwrap_or_default();
                    let _ = swap.instantiate(&e.id, &e.url, &backup_source, &e.config);
                    restored.push(e.id.clone());
                }
                return ReloadOutcome::RolledBack { reloaded_from_backup: restored, error };
            }
        }
    }

    ReloadOutcome::Committed { reloaded }
}

pub fn hmr_cycle(
    stashed: &BTreeSet<String>,
    externals: &BTreeSet<String>,
    entries: &[ComponentEntry],
    graph: &ImportGraph,
    current_sources: &BTreeMap<String, String>,
    next_sources: &BTreeMap<String, String>,
    swap: &mut dyn FiberSwap,
) -> (Vec<String>, BTreeSet<String>, ReloadOutcome) {
    let (accepted, declined) = classify(stashed, externals, graph);
    let (stale_ids, accepted) = detect(entries, &accepted, &declined, graph);

    let backup: ModuleBackup = accepted
        .iter()
        .filter_map(|url| current_sources.get(url).map(|s| (url.clone(), s.clone())))
        .collect();

    let stale_entries: Vec<ComponentEntry> = entries.iter().filter(|e| stale_ids.contains(&e.id)).cloned().collect();
    let outcome = reload(&stale_entries, next_sources, &backup, swap);

    (stale_ids, accepted, outcome)
}
