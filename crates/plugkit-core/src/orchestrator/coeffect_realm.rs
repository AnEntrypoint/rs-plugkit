#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealmTable {
    #[serde(default)]
    realm_table: BTreeMap<String, String>,
    #[serde(default)]
    by_realm: BTreeMap<String, BTreeMap<String, String>>,
}

impl RealmTable {
    pub fn new() -> RealmTable {
        RealmTable::default()
    }

    pub fn realm_of(&self, key: &str) -> String {
        self.realm_table.get(key).cloned().unwrap_or_default()
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        let realm = self.realm_of(key);
        self.by_realm.get(&realm).and_then(|table| table.get(key))
    }

    pub fn set(&mut self, key: &str, value: String) -> bool {
        let realm = self.realm_of(key);
        let table = self.by_realm.entry(realm).or_default();
        if table.contains_key(key) {
            return false;
        }
        table.insert(key.to_string(), value);
        true
    }

    pub fn isolate(&mut self, key: &str, realm: &str) {
        self.realm_table.insert(key.to_string(), realm.to_string());
    }

    pub fn realm_table(&self) -> &BTreeMap<String, String> {
        &self.realm_table
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeKind {
    ScalarOverwrite,
    SetUnion,
}

impl MergeKind {
    fn identity(self) -> String {
        String::new()
    }

    fn combine(self, base: &str, overlay: &str) -> String {
        match self {
            MergeKind::ScalarOverwrite => {
                if overlay.is_empty() {
                    base.to_string()
                } else {
                    overlay.to_string()
                }
            }
            MergeKind::SetUnion => {
                let mut items: Vec<&str> = base
                    .split(',')
                    .chain(overlay.split(','))
                    .filter(|s| !s.is_empty())
                    .collect();
                items.sort_unstable();
                items.dedup();
                items.join(",")
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterceptionContext {
    #[serde(default)]
    context_carried: BTreeMap<String, String>,
    #[serde(default)]
    merge_kind: BTreeMap<String, MergeKind>,
}

impl InterceptionContext {
    pub fn new() -> InterceptionContext {
        InterceptionContext::default()
    }

    pub fn declare_merge_kind(&mut self, key: &str, kind: MergeKind) {
        self.merge_kind.insert(key.to_string(), kind);
    }

    fn kind_of(&self, key: &str) -> MergeKind {
        self.merge_kind.get(key).copied().unwrap_or(MergeKind::ScalarOverwrite)
    }

    pub fn context_metadata(&self, key: &str) -> String {
        self.context_carried
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.kind_of(key).identity())
    }

    pub fn intercept(&mut self, key: &str, nu: &str) {
        let kind = self.kind_of(key);
        let merged = kind.combine(&self.context_metadata(key), nu);
        self.context_carried.insert(key.to_string(), merged);
    }

    pub fn resolve(&self, key: &str, component_declared: &str) -> String {
        let kind = self.kind_of(key);
        kind.combine(component_declared, &self.context_metadata(key))
    }
}
