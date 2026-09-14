#![cfg(target_arch = "wasm32")]

use super::discipline_note::{self, Component};
use super::fiber_lifecycle::FiberLifecycle;

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

pub fn resolve(accessor: &str, key: &str) -> Result<String, ResolveError> {
    if accessor == key {
        return Ok(accessor.to_string());
    }

    let accessor_component = Component::read(accessor);

    if !accessor_component.requires.iter().any(|d| d == key) {
        return Err(ResolveError::UndeclaredAccess {
            accessor: accessor.to_string(),
            key: key.to_string(),
        });
    }

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
        Some(p) => Ok(p.clone()),
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
