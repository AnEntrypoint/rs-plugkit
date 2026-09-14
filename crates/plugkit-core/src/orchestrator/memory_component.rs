#![cfg(target_arch = "wasm32")]

use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle};
use super::gm_dir;
use crate::pkfs;

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

fn depends_satisfied(namespace: &str, known: &[String]) -> bool {
    declared_depends_on(namespace)
        .iter()
        .all(|dep| known.iter().any(|n| n == dep) && namespace_exists(dep))
}

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
