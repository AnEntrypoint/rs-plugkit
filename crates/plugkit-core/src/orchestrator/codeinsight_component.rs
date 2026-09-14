#![cfg(target_arch = "wasm32")]

use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle};
use super::gm_dir;

fn fiber_state_path(role: &str) -> std::path::PathBuf {
    gm_dir().join("codeinsight-fiber-state").join(format!("{role}.json"))
}

fn known_roles() -> Vec<String> {
    let cfg = crate::ragconfig::RagConfig::resolved().namespaces;
    vec![cfg.code.clone(), cfg.vec_namespace(&cfg.code), cfg.manifest_namespace()]
}

pub fn advance_all_roles() -> Vec<String> {
    let roles = known_roles();
    let mut active = Vec::new();
    for role in &roles {
        let path = fiber_state_path(role).to_string_lossy().to_string();
        if fiber_lifecycle::advance_fiber(&path, true) {
            active.push(role.clone());
        }
    }
    active
}

pub fn active_role_set() -> ActiveFiberSet {
    let mut set = ActiveFiberSet::new();
    for role in &known_roles() {
        let path = fiber_state_path(role).to_string_lossy().to_string();
        if fiber_lifecycle::read_fiber_state(&path) == FiberLifecycle::Active {
            let _ = set.insert(role, &[role.clone()]);
        }
    }
    set
}

pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let active = advance_all_roles();
    let set = active_role_set();
    let payload = serde_json::json!({
        "ok": true,
        "roles_checked": known_roles().len(),
        "active": active,
        "active_fiber_set_len": set.len(),
        "storage_backend_of_component_substance": "libsql (shared_db.rs) -- NOT filesystem, unlike disciplines/memory-namespaces/plugins",
    });
    (payload.to_string(), String::new(), 0)
}
