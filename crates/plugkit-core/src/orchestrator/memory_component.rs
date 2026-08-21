#![cfg(target_arch = "wasm32")]

use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle};
use super::gm_dir;
use crate::pkfs;

/// A memory namespace (`.gm/memories/` for `default`, `.gm/disciplines/<ns>/memories`
/// for any other) read as a Cordis component (paper Definition 43), the
/// THIRD component kind gm's `fiber_lifecycle` module now serves --
/// disciplines and sibling wasm plugins (agentplug's own
/// `PluginFiberLifecycle`) being the first two. Nothing was added to
/// `fiber_lifecycle.rs` itself to support this: it is called with exactly
/// the same two public functions (`read_fiber_state`/`advance_fiber`) and
/// the same `ActiveFiberSet`, each taking a path this module supplies, the
/// same way `discipline_note.rs` does. This is the concrete test of the
/// module's generality claim: a caller nobody anticipated when
/// `fiber_lifecycle.rs` was written needed zero changes to it.
///
/// A namespace's coeffect specification is a `depends_on` field in an
/// optional `.gm/memories-manifest/<ns>.json` (or the discipline-nested
/// equivalent): the other namespace names whose content this one assumes
/// is coherent (e.g. a derived/summary namespace assuming its source
/// namespace's memory files still exist). A namespace's provision is
/// always its own name -- one namespace, one capability, unlike a
/// discipline's `provides` which can name arbitrary capability strings;
/// this is a deliberately simpler reduction, since namespaces have no
/// analogue of a discipline's realm-scoped multi-capability provision
/// today.
fn manifest_path(namespace: &str) -> std::path::PathBuf {
    gm_dir().join("memories-manifest").join(format!("{namespace}.json"))
}

fn fiber_state_path(namespace: &str) -> std::path::PathBuf {
    gm_dir().join("memories-manifest").join(format!("{namespace}.fiber-state.json"))
}

fn declared_depends_on(namespace: &str) -> Vec<String> {
    let path = manifest_path(namespace).to_string_lossy().to_string();
    pkfs::read_to_string(&path)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("depends_on").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| arr.into_iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

fn namespace_exists(namespace: &str) -> bool {
    crate::memory_md::md_dir(namespace)
        .map(|dir| pkfs::exists(&dir))
        .unwrap_or(false)
}

/// Whether `namespace`'s coeffect specification is satisfied: every
/// namespace it `depends_on` must itself exist (have a memories
/// directory). This is the same reactive-coeffect shape
/// `discipline_note.rs::requires_satisfied` uses, reduced to the simpler
/// existence-only provision namespaces carry.
fn depends_satisfied(namespace: &str, known: &[String]) -> bool {
    declared_depends_on(namespace)
        .iter()
        .all(|dep| known.iter().any(|n| n == dep) && namespace_exists(dep))
}

/// Advances every known namespace's persisted fiber by exactly one
/// transition, using `fiber_lifecycle`'s kind-agnostic `advance_fiber`,
/// and returns the names that reached `Active` this call -- the same
/// pattern `discipline_note.rs::active_policies` uses, confirming the
/// shared module drives a second, independently-designed component kind
/// without modification.
pub fn advance_all_namespaces(known: &[String]) -> Vec<String> {
    let mut active = Vec::new();
    for ns in known {
        let target = depends_satisfied(ns, known);
        let path = fiber_state_path(ns).to_string_lossy().to_string();
        if fiber_lifecycle::advance_fiber(&path, target) {
            active.push(ns.clone());
        }
    }
    active
}

/// Builds an `ActiveFiberSet` from the currently-`Active` namespaces,
/// exercising the same preservation-by-construction guarantee
/// `discipline_note.rs::audit_preservation` uses -- two namespaces never
/// collide since each provides only its own name, but the type itself
/// (not a namespace-specific assumption) is what a caller relies on here.
pub fn active_namespace_set(known: &[String]) -> ActiveFiberSet {
    let mut set = ActiveFiberSet::new();
    for ns in known {
        let path = fiber_state_path(ns).to_string_lossy().to_string();
        if fiber_lifecycle::read_fiber_state(&path) == FiberLifecycle::Active {
            let _ = set.insert(ns, &[ns.clone()]);
        }
    }
    set
}

/// The namespaces known to this project: "default" plus any name with a
/// `.json` manifest under `.gm/memories-manifest/` (excluding the
/// `.fiber-state.json` files this module itself writes there).
fn known_namespaces() -> Vec<String> {
    let mut out = vec!["default".to_string()];
    let base = gm_dir().join("memories-manifest").to_string_lossy().to_string();
    if let Some(serde_json::Value::Array(entries)) = pkfs::readdir(&base) {
        for entry in entries {
            let name = entry.get("name").and_then(|n| n.as_str()).or_else(|| entry.as_str());
            if let Some(name) = name {
                if let Some(ns) = name.strip_suffix(".json") {
                    if !ns.ends_with(".fiber-state") && !out.iter().any(|n| n == ns) {
                        out.push(ns.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Verb entry point for `memory-namespace-audit`: advances every known
/// namespace's fiber by one transition and reports which are `Active`,
/// exercising the same fiber-lifecycle/`ActiveFiberSet` machinery
/// `discipline-audit` exercises for disciplines, over gm's third real
/// component family.
pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let known = known_namespaces();
    let active = advance_all_namespaces(&known);
    let set = active_namespace_set(&known);
    let payload = serde_json::json!({
        "ok": true,
        "namespaces_checked": known.len(),
        "active": active,
        "active_fiber_set_len": set.len(),
    });
    (payload.to_string(), String::new(), 0)
}
