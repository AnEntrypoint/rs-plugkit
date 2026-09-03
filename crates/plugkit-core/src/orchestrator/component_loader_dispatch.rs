#![cfg(target_arch = "wasm32")]

use std::collections::{BTreeMap, BTreeSet};
use super::component_loader::{
    self, ComponentEntry, FiberSwap, Isolate, LoaderState,
};
use serde_json::Value;

fn parse_entries(v: &Value) -> Vec<ComponentEntry> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(|e| serde_json::from_value::<ComponentEntry>(e.clone()).ok()).collect())
        .unwrap_or_default()
}

/// `component-loader-reconcile`: dispatch body `{previous: [entry...],
/// next: [entry...]}` (Definition 74 entries). Runs `diff_entries`
/// (the paper's per-field dispatch, Section 5.2.1) and, for every entry
/// whose decision is `ReassignRealms`, additionally runs Algorithm 7
/// (`patch_isolation`) against the persisted `LoaderState`. Returns the
/// full decision list plus each realm reassignment's key diffs and
/// notified dependents.
pub fn handle_reconcile(content: &str) -> (String, String, i32) {
    let parsed: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let previous = parse_entries(&parsed.get("previous").cloned().unwrap_or(Value::Array(vec![])));
    let next = parse_entries(&parsed.get("next").cloned().unwrap_or(Value::Array(vec![])));

    if next.is_empty() && parsed.get("next").is_none() {
        return (
            serde_json::json!({"ok": false, "error": "component-loader-reconcile requires next: [entry...]"}).to_string(),
            String::new(),
            1,
        );
    }

    let decisions = component_loader::diff_entries(&previous, &next);
    let mut state = component_loader::read_state();

    let mut reassignments = Vec::new();
    for decision in &decisions {
        if decision.op != component_loader::ReconcileOp::ReassignRealms {
            continue;
        }
        let Some(entry) = next.iter().find(|e| e.id == decision.id) else { continue };
        let Some(prev_entry) = previous.iter().find(|e| e.id == decision.id) else { continue };
        let reassignment = component_loader::patch_isolation(&mut state, prev_entry, &entry.isolate, &next);
        reassignments.push(reassignment);
    }

    component_loader::write_state(&state);

    let payload = serde_json::json!({
        "ok": true,
        "decisions": decisions,
        "realm_reassignments": reassignments,
    });
    (payload.to_string(), String::new(), 0)
}

struct RecordingSwap {
    disposed: Vec<String>,
    instantiated: Vec<(String, String)>,
    fail_urls: BTreeSet<String>,
}

impl FiberSwap for RecordingSwap {
    fn dispose(&mut self, entry_id: &str) {
        self.disposed.push(entry_id.to_string());
    }

    fn instantiate(&mut self, entry_id: &str, url: &str, source: &str, _config: &Value) -> Result<String, String> {
        if self.fail_urls.contains(url) {
            return Err(format!("simulated import failure for {url}"));
        }
        self.instantiated.push((entry_id.to_string(), url.to_string()));
        Ok(source.to_string())
    }
}

/// `component-loader-hmr`: dispatch body `{stashed: [url...], externals:
/// [url...], entries: [entry...], graph: {url: [import_url...]},
/// current_sources: {url: source}, next_sources: {url: source},
/// fail_urls: [url...]}`. `fail_urls` (optional) names modules whose
/// `instantiate` should simulate an import failure -- the transactional
/// rollback path (Algorithm 10 lines 7-11), exercised deliberately
/// rather than left unreachable in a live dispatch. Runs the full
/// Algorithms 8-10 pipeline (`hmr_cycle`) and reports which entries were
/// classified stale, the final accepted set, and the reload outcome.
pub fn handle_hmr(content: &str) -> (String, String, i32) {
    let parsed: Value = serde_json::from_str(content).unwrap_or(Value::Null);

    let stashed: BTreeSet<String> = parsed
        .get("stashed")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let externals: BTreeSet<String> = parsed
        .get("externals")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if stashed.is_empty() {
        return (
            serde_json::json!({"ok": false, "error": "component-loader-hmr requires a non-empty stashed: [url...]"}).to_string(),
            String::new(),
            1,
        );
    }

    let entries = parse_entries(&parsed.get("entries").cloned().unwrap_or(Value::Array(vec![])));

    let graph: component_loader::ImportGraph = parsed
        .get("graph")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    let imports = v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    (k.clone(), imports)
                })
                .collect()
        })
        .unwrap_or_default();

    let current_sources: BTreeMap<String, String> = parsed
        .get("current_sources")
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    let next_sources: BTreeMap<String, String> = parsed
        .get("next_sources")
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_else(|| current_sources.clone());

    let fail_urls: BTreeSet<String> = parsed
        .get("fail_urls")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let mut swap = RecordingSwap { disposed: Vec::new(), instantiated: Vec::new(), fail_urls };

    let (stale_ids, accepted, outcome) =
        component_loader::hmr_cycle(&stashed, &externals, &entries, &graph, &current_sources, &next_sources, &mut swap);

    let payload = serde_json::json!({
        "ok": true,
        "stale_entries": stale_ids,
        "accepted": accepted,
        "outcome": outcome,
        "disposed": swap.disposed,
        "instantiated": swap.instantiated,
    });
    (payload.to_string(), String::new(), 0)
}
