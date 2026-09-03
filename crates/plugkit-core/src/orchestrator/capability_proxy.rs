#![cfg(target_arch = "wasm32")]

use super::discipline_note::{self, Component};
use super::fiber_lifecycle::FiberLifecycle;

/// The paper's two named runtime errors (Section 5.1.4, Algorithm 6),
/// raised by `resolve` at the exact point of access rather than only at
/// discipline-activation time. `INACTIVE_ACCESS`: the accessing fiber (or
/// an ancestor in its coeffect chain) declares this capability but has not
/// committed it -- the provider is not currently `Active`. `UNDECLARED_ACCESS`:
/// the walk reached the root with no fiber ever declaring the capability at
/// all -- the accessor never asked for it in its own `requires`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    InactiveAccess { accessor: String, key: String, provider: String },
    UndeclaredAccess { accessor: String, key: String },
}

impl ResolveError {
    pub fn code(&self) -> &'static str {
        match self {
            ResolveError::InactiveAccess { .. } => "INACTIVE_ACCESS",
            ResolveError::UndeclaredAccess { .. } => "UNDECLARED_ACCESS",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ResolveError::InactiveAccess { accessor, key, provider } => format!(
                "INACTIVE_ACCESS: component '{accessor}' declares capability '{key}' but its provider '{provider}' is not Active (fiber.inject, not fiber.committed)"
            ),
            ResolveError::UndeclaredAccess { accessor, key } => format!(
                "UNDECLARED_ACCESS: component '{accessor}' accessed capability '{key}' without declaring it in requires.json"
            ),
        }
    }
}

/// Algorithm 6, `resolve(ctx, key)`, specialized to gm's discipline
/// components. The paper's fiber chain (`fiber.parent.fiber`, ending at
/// `root`) has no multi-level nesting in gm's flat discipline model --
/// every discipline resolves directly against the realm-scoped enabled
/// set, so the walk here has exactly the two rungs the paper's Line 4/5/6
/// distinguish: THIS fiber's own committed/inject view, then the root
/// (the walk terminates in one hop because there is no parent fiber to
/// ascend to). This still names the three distinct outcomes Algorithm 6
/// names, not a collapsed two-way check:
///
/// - Line 4 (`key in fiber.committed`): `accessor` itself declares `key`
///   in `requires`, AND some Active, requires-satisfied provider in the
///   same realm currently supplies it. Authorized; returns the provider.
/// - Line 5 (`key in fiber.inject`): `accessor` declares `key` in
///   `requires` (so the capability request -- the paper's "inject
///   declaration" -- exists), but no Active provider currently satisfies
///   it (withdrawn, never activated, wrong realm, or provider itself not
///   requires-satisfied). `INACTIVE_ACCESS`.
/// - Line 6 (`fiber = root`): `accessor` never declared `key` in its own
///   `requires` at all. `UNDECLARED_ACCESS`.
///
/// `accessor` is the caller-claimed discipline name (the `discipline`
/// field callers already pass to `kv_get`/`kv_put`/`kv_query`, per
/// `confinement_violation`'s own documented caveat: a caller that omits
/// `discipline` is not naming itself as an accessing fiber at all, so no
/// capability check applies to it -- this mirrors the paper's Algorithm 6
/// itself, which resolves a NAMED ctx's chain, not an anonymous access).
/// `key` is the capability name being accessed -- gm's KV `namespace`
/// argument, since that is the actual coeffect surface disciplines read
/// and write through (see `discipline_note.rs`'s own doc comment: "Disciplines
/// route KV writes to `<cwd>/.gm/disciplines/<ns>/`").
pub fn resolve(accessor: &str, key: &str) -> Result<String, ResolveError> {
    // A component accessing its OWN namespace is not going through the
    // coeffect chain at all (it is not consuming a dependency, it owns
    // this storage outright) -- Algorithm 6 mediates access to ANOTHER
    // fiber's committed view, never a fiber's own state.
    if accessor == key {
        return Ok(accessor.to_string());
    }

    let accessor_component = Component::read(accessor);

    // Line 6 reached with no declaration: accessor never named `key` in
    // its own requires.json at all.
    if !accessor_component.requires.iter().any(|d| d == key) {
        return Err(ResolveError::UndeclaredAccess {
            accessor: accessor.to_string(),
            key: key.to_string(),
        });
    }

    // accessor declares `key` (the inject exists) -- now resolve it
    // against the realm-scoped enabled set exactly as
    // `discipline_note::requires_satisfied` does, but naming the
    // resolved provider rather than returning a bare bool, since Line 4
    // requires returning `fiber.committed[key]` (the binding), not just
    // whether one exists. The realm compared is the per-key isolation
    // realm (Definition 28-29's `rho: K -> R`, `resolve_key_realm`), not
    // the accessor's bare discipline-level `realm` field -- a `key` the
    // accessor isolates via `requires.json`'s `isolation` map must match
    // the provider's OWN per-key realm for that same key, since a
    // provider can likewise isolate the capability it supplies under a
    // different realm than its own discipline-level default.
    let enabled = discipline_note::enabled_names();
    let realm_table = discipline_note::build_realm_table(&enabled);
    let dep_realm = discipline_note::resolve_key_realm(&realm_table, &accessor_component.realm, key);
    let provider = enabled
        .iter()
        .filter(|n| n.as_str() != accessor)
        .filter(|n| {
            let c = Component::read(n);
            discipline_note::resolve_key_realm(&realm_table, &c.realm, key) == dep_realm
        })
        .filter(|n| Component::read(n).lifecycle == FiberLifecycle::Active)
        .find(|n| Component::read(n).provides.iter().any(|cap| cap == key));

    match provider {
        // Line 4: committed -- an Active, same-realm provider actually
        // supplies `key`. Authorized.
        Some(p) => Ok(p.clone()),
        // Line 5: declared (inject exists via accessor's own requires)
        // but nothing committed -- no Active provider resolves it right
        // now. The provider name surfaced here is whichever known
        // discipline's `provides` names `key`, even if not currently
        // Active, so the error is actionable (names what to re-enable)
        // rather than merely "nothing provides this."
        None => {
            let named_provider = all_known_providers_of(key, &dep_realm, &realm_table)
                .into_iter()
                .next()
                .unwrap_or_else(|| key.to_string());
            Err(ResolveError::InactiveAccess {
                accessor: accessor.to_string(),
                key: key.to_string(),
                provider: named_provider,
            })
        }
    }
}

fn all_known_providers_of(key: &str, dep_realm: &str, realm_table: &super::coeffect_realm::RealmTable) -> Vec<String> {
    discipline_note::all_known_discipline_dirs_pub()
        .into_iter()
        .filter(|n| {
            let c = Component::read(n);
            discipline_note::resolve_key_realm(realm_table, &c.realm, key) == dep_realm
                && c.provides.iter().any(|cap| cap == key)
        })
        .collect()
}

/// Verb entry point for `capability-resolve`: a live, dispatchable witness
/// of `resolve` against the current discipline set, so the proxy mediation
/// is testable the same way `discipline-audit` tests the fiber-lifecycle
/// metatheory -- a real Rust check, not prose describing one.
pub fn handle(content: &str) -> (String, String, i32) {
    let parsed: serde_json::Value = serde_json::from_str(content).unwrap_or(serde_json::Value::Null);
    let accessor = parsed.get("accessor").and_then(|v| v.as_str()).unwrap_or("");
    let key = parsed.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if accessor.is_empty() || key.is_empty() {
        return (
            String::new(),
            "capability-resolve refused: accessor and key required".to_string(),
            1,
        );
    }
    match resolve(accessor, key) {
        Ok(provider) => {
            let payload = serde_json::json!({
                "ok": true,
                "accessor": accessor,
                "key": key,
                "provider": provider,
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => {
            let payload = serde_json::json!({
                "ok": false,
                "error_code": e.code(),
                "accessor": accessor,
                "key": key,
                "detail": e.message(),
            });
            (payload.to_string(), String::new(), 1)
        }
    }
}
