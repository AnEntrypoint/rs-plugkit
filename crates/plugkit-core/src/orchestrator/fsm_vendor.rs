use serde_json::{json, Value};
use crate::pkfs;
use super::instructions::{compiled_default_for_prose_key, has_compiled_default_for_prose_key};
use super::residual::{
    RESIDUAL_PRD_OPEN_DEFAULT, RESIDUAL_BROWSER_OPEN_DEFAULT, RESIDUAL_TASKS_RUNNING_DEFAULT,
    RESIDUAL_DIRTY_TREE_DEFAULT, RESIDUAL_IMPERATIVE_DEFAULT,
};
use super::{fsm, transitions};

const GATE_DEFAULTS: &[(&str, &str)] = &[
    ("long-gap-no-instruction", crate::gates::GATE_LONG_GAP_NO_INSTRUCTION_DEFAULT),
];

const RESIDUAL_DEFAULTS: &[(&str, &str)] = &[
    ("prd-open", RESIDUAL_PRD_OPEN_DEFAULT),
    ("browser-open", RESIDUAL_BROWSER_OPEN_DEFAULT),
    ("tasks-running", RESIDUAL_TASKS_RUNNING_DEFAULT),
    ("dirty-tree", RESIDUAL_DIRTY_TREE_DEFAULT),
    ("imperative", RESIDUAL_IMPERATIVE_DEFAULT),
];

const BROWSER_CONFIG_EXAMPLE: &str = r#"{
  "cdp_poll_timeout_ms": 1000,
  "cdp_poll_interval_ms": 250,
  "chrome_ready_deadline_ms": 30000,
  "eval_timeout_grace_ms": 6000,
  "headless": false,
  "session_idle_timeout_ms": 1800000
}
"#;

const DAEMON_PROJECT_CONFIG_EXAMPLE: &str = r#"{
  "gm_concurrency_limit": 4
}
"#;

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
        // A key with no compiled default hits compiled_default_for_prose_key's `_` arm
        // and would be scaffolded as a file full of ENTRY prose under (say) triage.md --
        // which reads as real content and is far worse than an obviously-empty stub,
        // because nothing about the resulting file signals that it needs writing.
        let placeholder;
        let text = if has_compiled_default_for_prose_key(key) {
            compiled_default_for_prose_key(key)
        } else {
            placeholder = format!(
                "# {key}\n\nTODO: write the instruction prose for the `{key}` phase.\n\n\
                 This file was scaffolded as a PLACEHOLDER because `{key}` is declared as a\n\
                 state's `prose_key` in .gm/instructions/fsm/graph.json but has no compiled\n\
                 default in this build. Until it is written, that phase serves ENTRY prose.\n"
            );
            placeholder.as_str()
        };
        let (ok, status) = write_if_absent_or_forced(&path, text, force);
        results.push(json!({ "path": path, "ok": ok, "status": status }));
    }

    let pre_vendor_graph_raw = fsm::vendored_graph_raw();

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

    let deviations_ref = {
        let mut lines = vec![
            "# Compiled deviation kinds".to_string(),
            String::new(),
            "Every deviation this build can emit, generated from the SAME table (`orchestrator::deviations::DEVIATION_TABLE`) the emitters themselves reference, so this cannot silently drift out of sync with what actually fires. A kind absent from this list is not emitted by any compiled code path -- if the served doctrine names one, the doctrine is describing an enforcement that does not exist.".to_string(),
            String::new(),
            "`severity` is the DEFAULT. Override it per project by adding a `deviation_severity` map to the `policy` object in .gm/instructions/fsm/graph.json, keyed by the kind name and valued `\"deny\"` or `\"log\"`:".to_string(),
            String::new(),
            "```json".to_string(),
            "{ \"policy\": { \"deviation_severity\": { \"unsolicited-doc-created\": \"deny\", \"synthetic-test-file\": \"deny\" } } }".to_string(),
            "```".to_string(),
            String::new(),
            "`deny` means the emitter refuses the dispatch (a gate denial, or a non-zero rc). `log` means it records the event and lets the dispatch proceed. The map is empty by default, so an unconfigured project gets exactly the severities listed below. A key naming an unknown kind, or a value that is neither `deny` nor `log`, falls back to the default and is reported by `fsm-validate` rather than silently configuring nothing.".to_string(),
            String::new(),
        ];
        for (name, desc, sev) in crate::orchestrator::deviations::known_deviations() {
            lines.push(format!("- `{}` (default `{}`) -- {}", name, sev.as_str(), desc));
        }
        lines.join("\n")
    };
    let deviations_path = ".gm/instructions/fsm/deviations.md";
    let (ok, status) = write_if_absent_or_forced(deviations_path, &deviations_ref, force);
    results.push(json!({ "path": deviations_path, "ok": ok, "status": status }));

    let invariants_ref = {
        let policy = crate::orchestrator::fsm::graph().policy;
        let mut lines = vec![
            "# Frozen FSM invariants".to_string(),
            String::new(),
            "Decisions that constrain how this FSM may be extended. Each was taken deliberately; the alternative is recorded so a future change can weigh it rather than rediscover it. Generated by `fsm-vendor` from the live policy, so the verb lists below are what this build actually enforces.".to_string(),
            String::new(),
            "## Step progression is single-slot, by specification".to_string(),
            String::new(),
            "`TurnState` carries exactly one `pending_step_id` and one deadline, not a map. This is the spec, not a limitation awaiting generalisation.".to_string(),
            String::new(),
            "The single-slot property is what makes the await-result denial sound. While a step is pending, `check_dispatch` denies every verb outside the await allowlist and tells the caller that no other verb is valid until that step completes. With a slot map the denial loses its meaning: `read_pending_step` would have to answer *which* step blocks *which* verb, and the response payload -- which embeds `pending_step_full` so the caller can resume without re-dispatching `instruction` -- would have to choose one step to describe or return several and let the caller pick. Both turn an unambiguous refusal into a negotiation.".to_string(),
            String::new(),
            "So a 'steps within a phase' model must not be added by widening this field. If concurrent steps are ever genuinely needed, the await-result denial has to be redesigned first, with an explicit answer for what a caller is permitted to do while any subset of steps is outstanding.".to_string(),
            String::new(),
            format!("Verbs currently permitted while a step is pending: {}.", if policy.await_allowed_verbs.is_empty() { "(none)".to_string() } else { policy.await_allowed_verbs.iter().map(|v| format!("`{}`", v)).collect::<Vec<_>>().join(", ") }),
            String::new(),
            "## Hooks fire at gate evaluation only".to_string(),
            String::new(),
            "A hook is reachable exclusively through `GateDef.hook`, evaluated inside `evaluate_gate`. There are no on-enter, on-exit, or on-deviation hook points, and that boundary is deliberate.".to_string(),
            String::new(),
            "Gate hooks are predicates: they answer a question and the answer is either true or false. A failing one denies a transition, which is a safe and already-modelled outcome. Lifecycle hooks are a different thing wearing the same name -- they run for their side effects, so they need their own failure policy (does a failed on-enter hook block the state, roll it back, or log and continue?), their own re-entrancy story, and their own answer for what happens when a hook fires during deviation emission and itself deviates.".to_string(),
            String::new(),
            "Adding lifecycle hooks is therefore a separate design with its own failure semantics, not an extension of the gate hook mechanism. Until that design exists, a workflow needing code to run at a state boundary should express it as a gate predicate on the transition into or out of that state.".to_string(),
        ];
        lines.push(String::new());
        lines.join("\n")
    };
    let invariants_path = ".gm/instructions/fsm/invariants.md";
    let (ok, status) = write_if_absent_or_forced(invariants_path, &invariants_ref, force);
    results.push(json!({ "path": invariants_path, "ok": ok, "status": status }));

    let hook_path = ".gm/instructions/hooks/example.js";
    let (ok, status) = write_if_absent_or_forced(hook_path, EXAMPLE_HOOK, force);
    results.push(json!({ "path": hook_path, "ok": ok, "status": status }));

    for (key, default_text) in GATE_DEFAULTS {
        let path = format!(".gm/instructions/gates/{}.md", key);
        let (ok, status) = write_if_absent_or_forced(&path, default_text, force);
        results.push(json!({ "path": path, "ok": ok, "status": status }));
    }

    for (key, default_text) in RESIDUAL_DEFAULTS {
        let path = format!(".gm/instructions/residual/{}.md", key);
        let (ok, status) = write_if_absent_or_forced(&path, default_text, force);
        results.push(json!({ "path": path, "ok": ok, "status": status }));
    }

    let browser_config_path = ".gm/browser-config.json";
    let (ok, status) = write_if_absent_or_forced(browser_config_path, BROWSER_CONFIG_EXAMPLE, force);
    results.push(json!({ "path": browser_config_path, "ok": ok, "status": status }));

    let daemon_project_config_path = ".gm/daemon-project-config.json";
    let (ok, status) = write_if_absent_or_forced(daemon_project_config_path, DAEMON_PROJECT_CONFIG_EXAMPLE, force);
    results.push(json!({ "path": daemon_project_config_path, "ok": ok, "status": status }));

    let validation = fsm::graph().validate();

    let staleness = pre_vendor_graph_raw.as_deref().and_then(|raw| {
        let parsed = serde_json::from_str::<fsm::Graph>(raw).ok()?;
        let report = fsm::staleness_report(&parsed, Some(raw));
        report.has_findings().then(|| {
            let mut v = serde_json::to_value(&report).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("findings".to_string(), json!(report.lines()));
                obj.insert("refreshed_by_this_dispatch".to_string(), json!(force));
                obj.insert("action".to_string(), json!(if force {
                    "the graph WAS just overwritten with this build's current default, so the items above describe what the previous file was missing and what has now been restored. Re-apply any deliberate customisation from git history."
                } else {
                    "graph.json already existed and was LEFT AS IS -- a vendored graph replaces the compiled default wholesale, with no merge, so every item above is a guarantee this project is currently running without. Re-run `fsm-vendor` with {\"force\":true} to reset it to this build's default (discarding local customisation, recoverable from git history), or hand-add just the named items and bump `schema_version`."
                }));
            }
            v
        })
    });

    let payload = json!({
        "ok": true,
        "vendored": results,
        "validation": validation,
        "schema_version": fsm::GRAPH_SCHEMA_VERSION,
        "staleness": staleness,
        "staleness_note": "Null means the vendored graph.json is level with this build. Otherwise it names exactly which states, edges, gates, guarded-edge gates, policy keys and predicates the vendored file lacks relative to the compiled default -- reported, never merged, because silently folding them in would change a hand-written FSM's meaning under its author, and hard-failing would discard every unrelated customisation over a version integer.",
        "note": "instruction/transition now serve from these files wherever present (per-key fallback to the compiled default for any prose file, wholesale-replace for the graph). gates/<key>.md and residual/<key>.md override the matching gate-denial/residual-scan message text via the same prose::resolve chain. browser-config.json and daemon-project-config.json are example defaults matching every field BrowserConfig/ProjectDaemonConfig actually reads -- edit values, remove fields to fall back to compiled defaults. The machine-wide ~/.agentplug/daemon-config.json is out of this per-project verb's reach (gm.wasm's fs sandbox is rooted at the project cwd); agentplug-runner itself scaffolds that file with the same example-defaults shape on first daemon boot if absent. Edit .gm/instructions/fsm/graph.json to add a custom phase, rewire an edge, or change which gates guard which transition -- no rebuild needed. Re-run this verb with {\"force\":true} to reset any of these back to the compiled defaults.",
    });
    (payload.to_string(), String::new(), 0)
}

/// On-demand referential-integrity check of the CURRENTLY-LOADED graph.
///
/// `Graph::validate()` already runs at load time, but its findings only reach
/// an emitted event, and only on the path where an override was parsed -- so a
/// project running the compiled default graph, or anyone wanting to check a
/// graph edit BEFORE relying on it, had no way to ask. This verb is that ask.
///
/// It reports rather than mutates: a graph that fails validation at load already
/// falls back to the built-in default (see fsm.rs's `fsm_graph_override_invalid`),
/// so the useful thing here is naming exactly which problems would trigger that,
/// including the fail-OPEN case where an edge names a gate that does not exist
/// and is therefore silently unguarded.
pub fn handle_validate(_content: &str) -> (String, String, i32) {
    let (graph, tier, source_path) = fsm::graph_detailed();
    let problems = graph.validate();
    let ok = problems.is_empty();

    // Not a "problem" -- the graph is internally consistent -- but a real
    // guarantee difference against the built-in default, which is exactly what
    // someone running a validation pass wants to know about.
    let weaker: Vec<Value> = fsm::gates_missing_vs_default(&graph)
        .into_iter()
        .map(|(from, to, missing)| json!({ "from": from, "to": to, "missing_gates": missing }))
        .collect();

    let staleness = {
        let raw = fsm::vendored_graph_raw();
        let report = fsm::staleness_report(&graph, raw.as_deref());
        let mut v = serde_json::to_value(&report).unwrap_or(Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("findings".to_string(), json!(report.lines()));
        }
        v
    };

    let payload = json!({
        "ok": ok,
        "tier": tier.as_str(),
        "source_path": source_path,
        "tier_note": "Which tier supplied the ACTIVE graph: `local_override` = .gm/instructions/fsm/graph.json, `source_repo` = the config repo's cached fsm.graph, `compiled_default` = built in. Gate hooks execute ONLY from local_override; a hook arriving from source_repo is refused and its gate falls back to predicate-only (or to the always-false `remote-hook-refused` predicate if the hook was its only condition).",
        "rejection": fsm::graph_rejection(),
        "problems": problems,
        "weaker_than_default": weaker,
        "schema_version": graph.schema_version,
        "current_schema_version": fsm::GRAPH_SCHEMA_VERSION,
        "min_plugkit_version": graph.min_plugkit_version,
        "running_plugkit_version": env!("CARGO_PKG_VERSION"),
        "min_plugkit_version_unmet": graph.min_plugkit_version_unmet(),
        "min_plugkit_version_note": "A graph may declare the oldest plugkit build it was authored against. Without it, a graph referencing a predicate added in a later build hits the unknown-predicate path and denies that gate forever, which reads as a legitimately-failing workflow rather than as a version mismatch. Declared as a dotted numeric version; a value that cannot be parsed is reported as ignored rather than silently enforced. Advisory -- nothing is rejected on this alone.",
        "policy_default_drift": fsm::Graph::policy_default_drift(),
        "policy_default_drift_note": "Self-check that the three descriptions of the policy defaults still agree: the Default impl, the per-field serde defaults, and the hand-maintained KNOWN_POLICY_KEYS list. They are independent sources for the same values, so any of them can drift alone -- and a baseline graph.json embedding serialized policy values makes a fourth. A non-empty list means a project omitting a policy field gets a different value than one vendoring the baseline, which is a silent behaviour change riding along with a supposedly no-op extraction. Empty is the expected state.",
        "staleness": staleness,
        "staleness_note": "How far the ACTIVE graph is behind this build's compiled default, itemised. `stale` compares schema_version integers; the missing_* lists are DERIVED by diffing the live default, so a hand-edited file carrying a current version number is still caught. Advisory only -- nothing here rejects or rewrites the graph.",
        "weaker_than_default_note": "Edges the ACTIVE graph guards with fewer gates than the built-in default. A vendored graph.json REPLACES the default wholesale (there is no merge), so a project that vendored before a gate existed never receives it and its edges stay permanently weaker with nothing saying so. Reported, never merged: silently adding gates would change a hand-written FSM's meaning, and a gate may have been dropped deliberately. Empty means the active graph is at least as strict as the default everywhere they overlap.",
        "states": graph.states.len(),
        "edges": graph.edges.len(),
        "gates": graph.gates.len(),
        "deviation_kinds": crate::orchestrator::deviations::known_deviations()
            .into_iter()
            .map(|(name, desc, default_sev)| json!({
                "kind": name,
                "description": desc,
                "default_severity": default_sev.as_str(),
                "effective_severity": crate::orchestrator::deviations::effective_severity_with(
                    name, &graph.policy.deviation_severity
                ).as_str(),
            }))
            .collect::<Vec<Value>>(),
        "deviation_kinds_note": "Every deviation kind this build can emit, with its registry default and the severity actually in force after policy.deviation_severity. A kind whose effective differs from its default has been overridden by this project. See fsm/deviations.md (generated from the same table).",
        "deviation_severity_warnings": graph.deviation_severity_warnings(),
        "deviation_severity_warnings_note": "policy.deviation_severity entries this build cannot honour -- an unknown kind name, or a value that is neither \"deny\" nor \"log\". NON-FATAL: each falls back to the registry default and the rest of the graph, including its other overrides, serves normally. Reported rather than rejected because discarding a whole graph over one mistyped severity key would silently drop every unrelated customisation in it.",
        "note": if ok {
            "graph passes referential-integrity validation: every edge's endpoints and gates resolve, every gate has a predicate or hook, every state is reachable and has a path to the terminal phase, and every declared prose_key resolves to real prose."
        } else {
            "graph has referential-integrity problems. An edge naming a gate that does not exist is the most dangerous of these: the gate is SKIPPED, so the edge appears guarded while being unguarded (fails OPEN). If these problems are in a vendored .gm/instructions/fsm/graph.json, that file is being REJECTED at load and the built-in default graph is serving instead."
        },
    });
    (payload.to_string(), String::new(), 0)
}
