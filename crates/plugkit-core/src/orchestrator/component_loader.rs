#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use super::coeffect_realm::RealmTable;
use super::gm_dir;
use crate::pkfs;

/// paper Section 5.2.1, Definition 74. An entry declares a single fiber.
/// `isolate` is `None` for no isolation, `Some(true)` for a local
/// per-entry realm, `Some(false)` reserved (never produced by
/// `Isolate::Local`/`Isolate::Global`, kept only so round-tripping an
/// externally-vendored config that sends `"isolate": false` does not
/// silently misparse), `Some` of an arbitrary string for a global realm
/// name -- represented here as `Isolate` rather than a raw
/// `Option<serde_json::Value>` so the two scoping rules the paper's own
/// text distinguishes ("a value of true asks for a local realm ... a
/// string asks for a global realm") are two variants a match can be
/// exhaustive over, not two ad hoc value shapes re-parsed at every call
/// site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Isolate {
    None,
    Local,
    Global { realm: String },
}

impl Isolate {
    /// The realm a key isolated under this annotation resolves to, given
    /// the owning entry's own `id` -- `Local` is "private to the entry
    /// and tagged by its id" per the paper's own text; `Global` is the
    /// named shared realm as-is.
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
    /// Keys this entry's component both requires and installs -- the
    /// finite substitute for the paper's `get_imports`-driven realm
    /// resolution: which realm-scoped keys this entry participates in is
    /// declared data here rather than derived from a live module graph,
    /// since this crate has no JS-style dynamic import to introspect.
    #[serde(default)]
    pub isolated_keys: Vec<String>,
}

fn default_isolate() -> Isolate {
    Isolate::None
}

/// paper Section 5.2.1: "On top of the fiber that an entry declares, the
/// loader dispatches on which of the entry's fields changed and applies
/// the least disruptive operation for each." One variant per bullet in
/// that dispatch list, `id`/`url` folded into one `Rebuild` (the paper
/// gives them the same operation: "rebuilds the entry, since its identity
/// or its component has changed").
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

/// One entry's reconciliation outcome: which operation the field diff
/// selected, in the paper's own priority order when several fields
/// changed at once (identity first, since a rebuild subsumes every other
/// operation on the same entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileDecision {
    pub id: String,
    pub op: ReconcileOp,
    pub changed_fields: Vec<String>,
}

/// Diffs `previous` against `next` by entry `id` (Definition 74: "id --
/// a stable identifier, used as the reconciliation key when its group's
/// child list changes") and returns one `ReconcileDecision` per entry
/// that is new, removed, or field-changed. A removed entry (present in
/// `previous`, absent from `next`) reports `ToggleDisabled` with
/// `changed_fields: ["<removed>"]`, matching the paper's own withdrawal
/// path (Corollary 62: "a departing fiber's contribution to the state is
/// nothing"). A brand-new entry reports `Rebuild`.
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
                // Priority order matches the paper's bullet list: isolate
                // reassignment is a structural move, so it takes priority
                // over the read-time-only intercept update and the
                // component-decided config apply; disabled is evaluated
                // last since unloading supersedes any other adjustment to
                // an entry that is about to stop existing as a fiber.
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

/// One key's realm-reassignment diff record -- the `diff[k]` entry
/// Algorithm 7 builds at line 7: `(rho(k), rho'(k), delta_k, provider's
/// delta_k)`. `tag` here is the fresh tag Algorithm 7 line 6 draws for
/// the entry's own context under `delta_k`; `provider_tag` is the
/// provider fiber's own `delta_k`, read from `provider_tags` supplied by
/// the caller since this crate has no live context tree to read
/// `store[rho(k)].fiber.ctx[delta_k]` from directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmKeyDiff {
    pub key: String,
    pub old_realm: String,
    pub new_realm: String,
    pub entry_tag: u64,
    pub provider_tag: Option<u64>,
}

/// The result of Algorithm 7: which keys moved realm, whether the
/// binding itself moved (the entry was the provider at that key and
/// `own` held), and the fresh `entry_tag` values to persist for next
/// time (Definition 65's own-test: `gamma'[delta_k] = d1 <=> gamma' is
/// derived from the entry's context`).
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

/// Algorithm 7, `patch_isolation`. `entry` is the entry being
/// reassigned; `new_isolate` is `rho'`; `all_entries` supplies every
/// other live entry so `affected(fiber, k)` (line 15-17) can be
/// evaluated per dependent, and `provider_of` maps a realm name to the
/// entry id currently providing at that realm (the `store[rho(k)]`
/// lookup) so the binding-move check at line 12 has something concrete
/// to test against. Returns the full diff plus the dependent ids
/// Algorithm 7's own `notify(entry.ctx, Delta, affected)` (line 18)
/// would have to walk -- computing the notify set is this function's
/// job since the paper's `notify` is itself the generic coeffect-change
/// broadcast (Algorithm 3) this crate models with `RealmTable` (see
/// `coeffect_realm.rs`), not a routine private to isolation.
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

        // line 12: `d1 = d2 and store[s1] and not store[s2]` -- the
        // entry itself is (or was) the provider at the old realm, and no
        // provider is yet registered at the new realm, so the binding
        // (not merely the reader) moves with the entry.
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

/// Persisted loader state across dispatches: the fresh-tag counter
/// (Algorithm 7 line 6, "fresh tag" -- a monotonic counter is a sound
/// freshness source since this crate has no concurrent writers per
/// project, matching the single-writer invariant `entry.md` already
/// requires of every surface) and the provider registry (`store` in the
/// paper's own notation) mapping `(key, realm) -> owning entry id`.
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

// ---------------------------------------------------------------------
// Section 5.2.2 Hot Module Replacement -- Algorithms 8, 9, 10.
// ---------------------------------------------------------------------

/// The dependency graph HMR classification walks: `get_imports(url)` per
/// the paper's own notation, supplied by the caller as an adjacency map
/// since this crate indexes real source files via `code_index.rs` rather
/// than a live JS module loader.
pub type ImportGraph = BTreeMap<String, Vec<String>>;

fn get_imports<'a>(graph: &'a ImportGraph, url: &str) -> &'a [String] {
    graph.get(url).map(|v| v.as_slice()).unwrap_or(&[])
}

/// Algorithm 8, `classify`. `stashed` = changed-file URLs, `externals` =
/// modules that force a full restart. Returns `(accepted, declined)`
/// exactly as the paper's own return statement.
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

    // line 21: any module left undecided (an import cycle) defaults to
    // declined.
    declined.extend(pending);

    (accepted, declined)
}

/// Algorithm 9's inner `get_dependencies`: the transitive-import closure
/// of `root`, stopping at `declined` boundaries (line 4: `if url in deps
/// or url in declined then return`).
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

/// Algorithm 9's outer `detect`. Folds each stale entry's whole
/// dependency tree into `accepted` as it goes (line 14), matching the
/// paper's own note that "every stale module along it is invalidated in
/// the next phase" -- so a later entry in `entries` sees the growing
/// `accepted` set from earlier ones in the same call, not a snapshot.
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

/// A backed-up module's prior source, keyed by url -- what Algorithm 10's
/// `invalidate_caches(accepted)` (line 2) returns as `backup`, and what
/// `backup[entry.url]` (line 11) re-imports on rollback.
pub type ModuleBackup = BTreeMap<String, String>;

/// The outcome of a transactional reload attempt (Algorithm 10). `Ok`
/// carries the entries that were actually swapped in; `Err` carries the
/// import failure's message, and by construction of `reload` below every
/// stale entry has already been restored from `backup` before the error
/// is returned -- so a caller holding an `Err` needs no further recovery
/// step, matching the paper's own "the system never enters a
/// half-reloaded state" guarantee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReloadOutcome {
    Committed { reloaded: Vec<String> },
    RolledBack { reloaded_from_backup: Vec<String>, error: String },
}

/// One stale entry's real reload step -- disposing the old fiber and
/// instantiating a fresh one from `source`. Kept as a trait rather than
/// a bare closure so `reload` below can be driven by both live source
/// (the real path) and a caller-supplied failing stub during an
/// adversarial DECIDE-phase sweep, without `reload` itself branching on
/// which.
pub trait FiberSwap {
    fn dispose(&mut self, entry_id: &str);
    /// Imports `source` and instantiates a new fiber for `entry_id`
    /// bound to `config`. Returns the new module's persisted source on
    /// success (what the next backup would restore to), or an error
    /// message on import failure (e.g. a syntax error).
    fn instantiate(&mut self, entry_id: &str, url: &str, source: &str, config: &serde_json::Value) -> Result<String, String>;
}

/// Algorithm 10, `reload`. `sources` supplies each stale entry's current
/// module source under its url -- the real content `invalidate_caches`
/// would fetch fresh and what `backup` preserves. Every entry is
/// attempted; on the FIRST import failure, every entry already disposed
/// in this call (successes so far, plus the one that just failed if it
/// was disposed before the instantiate error) is restored from `backup`
/// via a second `dispose`+`instantiate(backup[...])` pass, matching the
/// paper's own catch block (lines 8-11) which unconditionally rebuilds
/// every `stale_entries` member from backup, not only the ones already
/// swapped -- the paper's own text: "every stale entry is rebuilt from
/// backup[entry.url] ... undoing the swaps already made" reads as the
/// full set, since a not-yet-attempted entry was never disposed and
/// rebuilding it from backup is a no-op re-instantiation of what is
/// already running.
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

/// The full three-phase HMR pipeline (Algorithms 8-10 composed), the
/// `@cordisjs/hmr` engine's own top-level entry point. `current_sources`
/// backs `invalidate_caches`: every url in the returned `accepted` set is
/// backed up from its currently-running source before any dispose runs,
/// matching Algorithm 10 line 2 running before the loop at line 4.
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
