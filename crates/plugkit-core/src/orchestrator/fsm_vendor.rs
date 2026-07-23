use serde_json::{json, Value};
use crate::pkfs;
use super::instructions::compiled_default_for_prose_key;
use super::{fsm, transitions};

const EXAMPLE_HOOK: &str = r#"// Example FSM jit-hook (per fsm-framework-jit-hook-concreting). A hook
// is a plain exec_js script the orchestrator runs automatically at a
// gate's evaluation. It is wrapped in an async function body before
// running (the same wrapping every exec_js dispatch gets, per
// agentplug-host's build_command), so the gate result comes from an
// EXPLICIT `return`, never a bare trailing expression statement --
// `foo();` on its own last line is a statement whose value is discarded,
// not an implicit return, exactly like a normal JS function body. `true`
// passes the gate, anything else (false, a thrown error, a non-boolean
// return, a missing `return` at all, a missing/unreadable file) fails it
// CLOSED (denies), never open. Wire it into gates.json via a GateDef's
// `hook` field (a path relative to this hooks/ dir) and `hook_mode`
// ("hook-only" to replace the compiled predicate entirely, "both" to
// require both the compiled predicate AND this hook to pass, or the
// default "predicate-only" to ignore this file).
//
// This example: a made-up project-specific condition -- deny until a
// file named .gm/ship-approved exists, so a human (or an earlier CI
// step) has to touch that file before the FSM lets CONSOLIDATE proceed.
const fs = require('fs');
return fs.existsSync('.gm/ship-approved');
"#;

fn write_if_absent_or_forced(path: &str, content: &str, force: bool) -> (bool, &'static str) {
    if !force && pkfs::exists(path) {
        return (false, "skipped-existing");
    }
    let ok = pkfs::write(path, content);
    (ok, if ok { "written" } else { "write-failed" })
}

pub fn handle_vendor(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content.trim()).unwrap_or(Value::Null);
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut results: Vec<Value> = Vec::new();

    let graph = fsm::graph();
    let mut prose_keys: Vec<String> = graph.states.iter().map(|s| s.prose_key.clone()).collect();
    prose_keys.push("entry".to_string());
    prose_keys.push("browser".to_string());
    prose_keys.sort();
    prose_keys.dedup();
    for key in &prose_keys {
        let path = format!(".gm/instructions/{}.md", key);
        let text = compiled_default_for_prose_key(key);
        let (ok, status) = write_if_absent_or_forced(&path, text, force);
        results.push(json!({ "path": path, "ok": ok, "status": status }));
    }

    let graph_path = ".gm/instructions/fsm/graph.json";
    let (ok, status) = write_if_absent_or_forced(graph_path, &fsm::default_graph_json_pretty(), force);
    results.push(json!({ "path": graph_path, "ok": ok, "status": status }));

    let predicates_ref = {
        let mut lines = vec![
            "# Compiled FSM gate predicates".to_string(),
            String::new(),
            "Reference for `gates.predicate` in .gm/instructions/fsm/graph.json's `gates` array -- generated from the SAME registry transitions.rs's predicate_result() dispatches on, so this can never silently drift out of sync with what actually exists. A predicate name here is the ONLY thing a graph's gates array can reference directly; a genuinely new condition needs a jit hook instead (see hooks/example.js) or a Rust change to add a new compiled predicate.".to_string(),
            String::new(),
        ];
        for (name, desc) in transitions::known_predicates() {
            lines.push(format!("- `{}` -- {}", name, desc));
        }
        lines.join("\n")
    };
    let predicates_path = ".gm/instructions/fsm/predicates.md";
    let (ok, status) = write_if_absent_or_forced(predicates_path, &predicates_ref, force);
    results.push(json!({ "path": predicates_path, "ok": ok, "status": status }));

    let hook_path = ".gm/instructions/hooks/example.js";
    let (ok, status) = write_if_absent_or_forced(hook_path, EXAMPLE_HOOK, force);
    results.push(json!({ "path": hook_path, "ok": ok, "status": status }));

    let payload = json!({
        "ok": true,
        "vendored": results,
        "note": "instruction/transition now serve from these files wherever present (per-key fallback to the compiled default for any prose file, wholesale-replace for the graph). Edit .gm/instructions/fsm/graph.json to add a custom phase, rewire an edge, or change which gates guard which transition -- no rebuild needed. Re-run this verb with {\"force\":true} to reset any of these back to the compiled defaults.",
    });
    (payload.to_string(), String::new(), 0)
}
