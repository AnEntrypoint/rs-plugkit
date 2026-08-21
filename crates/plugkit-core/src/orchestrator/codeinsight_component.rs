#![cfg(target_arch = "wasm32")]

use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle};
use super::gm_dir;

/// A FOURTH component kind, chosen adversarially to test
/// `fiber_lifecycle`'s generality claim rather than confirm it:
/// disciplines and memory namespaces both store their primary substance
/// as filesystem trees under `.gm/`; sibling wasm plugins (agentplug) are
/// binary modules cached on disk. This kind's primary substance --
/// `code_index.rs`'s codeinsight/manifest/vec namespaces -- lives in a
/// libsql database (`shared_db.rs`), a completely different storage
/// backend from every prior caller.
///
/// The test: does `fiber_lifecycle::read_fiber_state`/`advance_fiber`,
/// which take a `state_path: &str` and call `pkfs::read_to_string`/
/// `write` (filesystem-backed host imports), still work for a component
/// whose OWN substance is NOT filesystem-backed at all? Answer: yes,
/// unmodified -- a component's fiber-state is a small sidecar fact ("is
/// this namespace role currently active") entirely separate from where
/// the namespace's actual content lives. `fiber_lifecycle` never touches
/// the component's substance, only a state-path the caller supplies, so
/// the caller's storage backend is invisible to it by construction. This
/// is the same reason `PluginFiberLifecycle` in agentplug (a SEPARATE
/// repo, wasm-binary-backed) works: the fiber-state file is always a
/// small filesystem sidecar, regardless of what the component itself is
/// made of. Zero changes were needed in `fiber_lifecycle.rs` for this
/// kind either.
fn fiber_state_path(role: &str) -> std::path::PathBuf {
    gm_dir().join("codeinsight-fiber-state").join(format!("{role}.json"))
}

/// The three codeinsight namespace roles this project's config declares
/// (`ragconfig::NamespaceConfig`): the base code namespace, its vector
/// sidecar, and its manifest. Each role's provision is its own name;
/// none declares a `requires` today (a role does not depend on another
/// role activating), so every role's target is unconditionally
/// satisfied -- this is a deliberately minimal instantiation, proving fit
/// rather than adding a coeffect relationship nothing in this codebase
/// actually needs yet.
fn known_roles() -> Vec<String> {
    let cfg = crate::ragconfig::RagConfig::resolved().namespaces;
    vec![cfg.code.clone(), cfg.vec_namespace(&cfg.code), cfg.manifest_namespace()]
}

/// Advances every known role's fiber by one transition (always satisfied,
/// per `known_roles`'s note) and returns which reached `Active`.
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

/// Builds an `ActiveFiberSet` from the currently-`Active` roles, exercising
/// preservation-by-construction over the fourth kind exactly as the first
/// three do.
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

/// Verb entry point for `codeinsight-namespace-audit`: the fourth
/// component-kind instantiation, wired as a real dispatchable check
/// rather than left unreachable.
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
