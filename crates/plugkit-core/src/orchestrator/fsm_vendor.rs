#![cfg(target_arch = "wasm32")]

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

const SOURCE_SPEC_EXAMPLE: &str = r#"{
  "_comment": "Scaffolded INERT, with an .example suffix, because activating it changes where this project's PROSE comes from (phase text, gate-denial and residual-scan messages). Rename to .gm/instructions/source.json to apply it to this project -- that is the only path prose::resolve reads; there is no home-directory tier. A file that is present but unreachable degrades to the compiled default -- it never hard-fails. This spec does NOT cover the FSM graph itself (states/edges/gates/policy) or gate hooks -- see .gm/config.source.json.example and .gm/gm.config.json.example for that.",
  "_tiers": "Resolution order, highest first: (1) project-vendored .gm/instructions/<key>.md, (2) this spec's source repo cache, (3) compiled defaults.",
  "_shadowing": "A vendored tier-1 file WINS over whatever this repo supplies for the same key. fsm-vendor writes compiled defaults into .gm/instructions/, so running it here inertises this repo's prose for every key it wrote. Delete the local file to fall back to the repo.",
  "_debounce": "The source repo is re-synced at most once per plugin_update_poll_interval_secs (default 600s) per project, so a fresh push is not picked up immediately.",
  "repo": "https://github.com/AnEntrypoint/gm-config",
  "branch": "main",
  "path": ""
}
"#;

const CONFIG_SOURCE_SPEC_EXAMPLE: &str = r#"{
  "_comment": "Scaffolded INERT, with an .example suffix, because activating it changes where this project's FSM GRAPH, POLICY, and GATE HOOKS come from -- a much bigger authority grant than .gm/instructions/source.json's prose-only scope. Rename to .gm/config.source.json (this project only) or ~/.gm/config.source.json (every project this user runs, lower priority than the project file) to activate.",
  "_tiers": "Resolution order, highest first: (1) .gm/gm.config.json (a real config committed into this project), (2) .gm/config.source.json (this file, activated), (3) ~/.gm/config.source.json (user-wide), (4) compiled defaults. See .gm/gm.config.json.example for the file this spec's repo is expected to publish (fsm.graph names the relative path to a graph.json within that repo).",
  "_debounce": "Re-synced at most once per plugin_update_poll_interval_secs (default 600s) per project.",
  "_hooks": "WARNING: a graph pulled through this tier can carry gate hooks -- arbitrary JS executed on this machine at every gate evaluation. CONSEQUENCE: anyone who can push to this repo, or compromise it, gets code execution on every project pointing at it, with no local review step. This tier has the exact same authority as a project's own local git history. Only point this at a repo you trust with full code-execution authority over every machine that syncs it.",
  "repo": "https://github.com/AnEntrypoint/gm-config",
  "branch": "main",
  "path": ""
}
"#;

const GM_CONFIG_EXAMPLE: &str = r#"{
  "_comment": "Scaffolded INERT, with an .example suffix. This is the real config schema (config.rs), not a pointer -- rename to .gm/gm.config.json to apply it directly, or publish something shaped like this from the repo named in .gm/config.source.json.",
  "fsm": {
    "graph": ""
  }
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
// step) has to touch that file before the FSM lets DECIDE proceed.
const fs = require('fs');
return fs.existsSync('.gm/ship-approved');
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MergePolicy {
    PerKeyWholeFile,
    Wholesale,
    PerFieldParseGated,
    NoMergeStandalone,
    GeneratedNotRead,
}

impl MergePolicy {
    fn id(self) -> &'static str {
        match self {
            MergePolicy::PerKeyWholeFile => "per-key-whole-file",
            MergePolicy::Wholesale => "wholesale-replace",
            MergePolicy::PerFieldParseGated => "per-field-parse-gated",
            MergePolicy::NoMergeStandalone => "no-merge-standalone",
            MergePolicy::GeneratedNotRead => "generated-not-read",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            MergePolicy::PerKeyWholeFile => "one file (the whole key)",
            MergePolicy::Wholesale => "the entire file",
            MergePolicy::PerFieldParseGated => "one JSON field, if the file parses",
            MergePolicy::NoMergeStandalone => "the whole file, loaded only when something names it",
            MergePolicy::GeneratedNotRead => "nothing -- this file is an output, not an input",
        }
    }

    fn rule(self) -> &'static str {
        match self {
            MergePolicy::PerKeyWholeFile => "prose::resolve serves this file INSTEAD OF the compiled default whenever it is present and not whitespace-only. There is no merge WITHIN a file: a partial override loses every paragraph it omits. Delete the file to fall back; blanking it also falls back, because read_clean treats whitespace-only as absent.",
            MergePolicy::Wholesale => "fsm::graph_detailed serves this file INSTEAD OF the compiled default graph entirely -- states, edges, gates and policy together. Nothing is merged in, so a state or gate added to a later build never reaches a project that vendored earlier; `fsm-validate` reports the delta as `staleness` and `weaker_than_default` rather than folding it in. A file that fails to parse or fails validation is rejected and the compiled default serves whole.",
            MergePolicy::PerFieldParseGated => "TWO policies, selected by whether the file parses. Parses -> every field is an Option, so an OMITTED field falls back to the compiled default individually (per-field merge). Fails to parse -> serde returns None and the ENTIRE file is discarded, so every field silently reverts to compiled defaults at once. The failure mode is silent: there is no diagnostic distinguishing 'no config' from 'malformed config'.",
            MergePolicy::NoMergeStandalone => "Loaded by path only when another artifact names it, and never merged with a default. There is no compiled fallback to merge against: a named-but-MISSING hook fails CLOSED (its gate denies forever) rather than degrading to a default, which is the opposite of every prose artifact's behaviour.",
            MergePolicy::GeneratedNotRead => "Not an override at all -- an OUTPUT of the vendor pass, regenerated from live code and read back by nothing. Editing it changes documentation only, configures nothing, and the edit is discarded by the next `fsm-vendor` with force. It is listed here because it sits in the same vendored tree as the real overrides and is otherwise easy to mistake for one.",
        }
    }
}

fn placeholders_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == '{' {
            let mut j = i + 1;
            let mut name = String::new();
            while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j] == '_') {
                name.push(bytes[j]);
                j += 1;
            }
            if j < bytes.len() && bytes[j] == '}' && !name.is_empty() {
                let tok = format!("{{{}}}", name);
                if !found.contains(&tok) {
                    found.push(tok);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    found
}

struct PolicyRow {
    path: String,
    class: &'static str,
    policy: MergePolicy,
    reader: &'static str,
    placeholders: Vec<String>,
    derived: &'static str,
    note: Option<String>,
}

fn merge_policy_rows() -> Vec<PolicyRow> {
    let mut rows: Vec<PolicyRow> = Vec::new();

    let graph = fsm::graph();
    let mut prose_keys: Vec<String> = graph.states.iter().map(|s| s.prose_key.clone()).collect();
    prose_keys.push("entry".to_string());
    prose_keys.push("browser".to_string());
    prose_keys.sort();
    prose_keys.dedup();
    for key in &prose_keys {
        let compiled = has_compiled_default_for_prose_key(key);
        rows.push(PolicyRow {
            path: format!(".gm/instructions/{}.md", key),
            class: "phase prose",
            policy: MergePolicy::PerKeyWholeFile,
            reader: "crate::prose::resolve via instructions::get_instruction",
            placeholders: Vec::new(),
            derived: "iterated from fsm::graph().states[].prose_key plus the two non-state keys the vendor pass appends",
            note: if compiled {
                None
            } else {
                Some(format!(
                    "`{key}` has NO compiled default, so its fallback is ENTRY prose, not prose for this phase. Deleting this file does not restore a correct default -- it serves the wrong one."
                ))
            },
        });
    }
    if let Some(entry_row) = rows.iter_mut().find(|r| r.path == ".gm/instructions/entry.md") {
        entry_row.class = "phase prose (also prepended to every other phase)";
        entry_row.reader = "crate::prose::resolve, served for ENTRY and concatenated AHEAD of every non-entry phase's prose";
        entry_row.note = Some("Overriding this replaces the orchestrator preamble served with EVERY phase, not just ENTRY.".to_string());
    }

    rows.push(PolicyRow {
        path: ".gm/instructions/fsm/graph.json".to_string(),
        class: "FSM graph",
        policy: MergePolicy::Wholesale,
        reader: "fsm::graph_detailed",
        placeholders: Vec::new(),
        derived: "the single GRAPH_OVERRIDE_PATH fsm.rs reads",
        note: None,
    });

    for generated in [
        (".gm/instructions/fsm/predicates.md", "transitions::known_predicates()"),
        (".gm/instructions/fsm/deviations.md", "deviations::DEVIATION_TABLE"),
        (".gm/instructions/fsm/invariants.md", "the live fsm::graph().policy"),
        (".gm/instructions/fsm/configurable.md", "the live graph plus KNOWN_POLICY_KEYS and the predicate/deviation registries"),
        (".gm/instructions/fsm/merge-policy.md", "merge_policy_rows(), which walks the same registries the vendor pass writes from"),
    ] {
        rows.push(PolicyRow {
            path: generated.0.to_string(),
            class: "generated reference",
            policy: MergePolicy::GeneratedNotRead,
            reader: "nothing -- written for a human, never read back by this build",
            placeholders: Vec::new(),
            derived: "written unconditionally by the vendor pass",
            note: Some(format!(
                "Regenerated from {} on every `fsm-vendor` with force. Editing it changes documentation only; it configures nothing, and the edit is discarded on the next forced vendor.",
                generated.1
            )),
        });
    }

    rows.push(PolicyRow {
        path: ".gm/instructions/hooks/example.js".to_string(),
        class: "gate hook",
        policy: MergePolicy::NoMergeStandalone,
        reader: "fsm::evaluate_gate, only when a GateDef names it in `hook`",
        placeholders: Vec::new(),
        derived: "the single hook path the vendor pass scaffolds",
        note: Some("Hooks execute ONLY from the project-vendored graph tier; a hook named by a graph arriving from a config repo is refused and its gate falls back to predicate-only.".to_string()),
    });

    for (key, default_text) in GATE_DEFAULTS {
        rows.push(PolicyRow {
            path: format!(".gm/instructions/gates/{}.md", key),
            class: "gate denial message",
            policy: MergePolicy::PerKeyWholeFile,
            reader: "crate::prose::resolve_and_mark from gates.rs",
            placeholders: placeholders_in(default_text),
            derived: "iterated from fsm_vendor::GATE_DEFAULTS, the same table the vendor pass writes from",
            note: None,
        });
    }

    for (key, default_text) in RESIDUAL_DEFAULTS {
        rows.push(PolicyRow {
            path: format!(".gm/instructions/residual/{}.md", key),
            class: "residual-scan message",
            policy: MergePolicy::PerKeyWholeFile,
            reader: "crate::prose::resolve_and_mark from residual.rs",
            placeholders: placeholders_in(default_text),
            derived: "iterated from fsm_vendor::RESIDUAL_DEFAULTS, the same table the vendor pass writes from",
            note: None,
        });
    }

    rows.push(PolicyRow {
        path: ".gm/browser-config.json".to_string(),
        class: "host JSON config",
        policy: MergePolicy::PerFieldParseGated,
        reader: "agentplug-host BrowserConfig::load -- a DIFFERENT crate, not this build",
        placeholders: Vec::new(),
        derived: "NOT derived from code: no reader for this path exists in plugkit, so the field list and per-field defaults below are transcribed from agentplug-host and can drift without this build noticing",
        note: Some("The scaffolded example sets `headless: false`, matching the host's own compiled fallback `headless.unwrap_or(false)`. Removing the field reproduces the vendored example's behaviour.".to_string()),
    });

    rows.push(PolicyRow {
        path: ".gm/daemon-project-config.json".to_string(),
        class: "host JSON config",
        policy: MergePolicy::PerFieldParseGated,
        reader: "agentplug-host ProjectDaemonConfig::load -- a DIFFERENT crate, not this build",
        placeholders: Vec::new(),
        derived: "NOT derived from code: no reader for this path exists in plugkit, so this row is transcribed from agentplug-host and can drift without this build noticing",
        note: Some("`gm_concurrency_limit` falls back to UNLIMITED when omitted or when the value is 0, not to the example's 4.".to_string()),
    });

    rows
}

fn merge_policy_doc() -> String {
    let rows = merge_policy_rows();
    let mut lines = vec![
        "# Vendored artifact merge policy".to_string(),
        String::new(),
        "Every artifact `fsm-vendor` writes, and what happens to the compiled default when that artifact is present. Generated by `fsm-vendor` by walking the SAME registries the vendor pass writes from (`fsm::graph().states[].prose_key`, `GATE_DEFAULTS`, `RESIDUAL_DEFAULTS`), so the artifact list here cannot drift from the artifact list actually written.".to_string(),
        String::new(),
        "There is no single merge rule and there cannot be one. These artifacts are consumed by different mechanisms with genuinely different units of override -- a whole file, a whole graph, one JSON field, or nothing at all -- so any single global rule would be wrong for most of this table.".to_string(),
        String::new(),
        "## Policies".to_string(),
        String::new(),
    ];
    for p in [
        MergePolicy::PerKeyWholeFile,
        MergePolicy::Wholesale,
        MergePolicy::PerFieldParseGated,
        MergePolicy::NoMergeStandalone,
        MergePolicy::GeneratedNotRead,
    ] {
        lines.push(format!("### `{}`", p.id()));
        lines.push(String::new());
        lines.push(format!("Unit of override: {}.", p.unit()));
        lines.push(String::new());
        lines.push(p.rule().to_string());
        lines.push(String::new());
    }

    lines.push("## Placeholders".to_string());
    lines.push(String::new());
    lines.push("Some overridable messages have their `{token}` substrings substituted by the caller AFTER prose::resolve returns. Substitution is a blind `str::replace`, so an override that omits a token simply never receives that value -- the message renders without it and nothing reports the loss. Tokens are detected here by scanning each compiled default, so this column tracks the real defaults rather than a hand-kept list.".to_string());
    lines.push(String::new());

    lines.push("## Artifacts".to_string());
    lines.push(String::new());
    lines.push("| path | class | policy | placeholders | reader |".to_string());
    lines.push("| --- | --- | --- | --- | --- |".to_string());
    for r in &rows {
        let ph = if r.placeholders.is_empty() {
            "--".to_string()
        } else {
            r.placeholders.iter().map(|p| format!("`{}`", p)).collect::<Vec<_>>().join(" ")
        };
        lines.push(format!(
            "| `{}` | {} | `{}` | {} | {} |",
            r.path, r.class, r.policy.id(), ph, r.reader
        ));
    }
    lines.push(String::new());

    let notes: Vec<&PolicyRow> = rows.iter().filter(|r| r.note.is_some()).collect();
    if !notes.is_empty() {
        lines.push("## Per-artifact caveats".to_string());
        lines.push(String::new());
        for r in notes {
            lines.push(format!("- `{}` -- {}", r.path, r.note.as_deref().unwrap_or("")));
        }
        lines.push(String::new());
    }

    lines.push("## Derivation provenance".to_string());
    lines.push(String::new());
    lines.push("Where each row's policy came from. A row marked NOT derived is transcribed from another crate and is the drift risk in this table.".to_string());
    lines.push(String::new());
    for r in &rows {
        lines.push(format!("- `{}` -- {}", r.path, r.derived));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn write_if_absent_or_forced(path: &str, content: &str, force: bool) -> (bool, &'static str) {
    let existed = pkfs::exists(path);
    if !force && existed {
        return (false, "skipped-existing");
    }
    // force:true overwrites operator-authored config. Back the old content up
    // first: a vendor pass regenerates from live code, so without this a hand-
    // tuned graph or gate set is gone with no undo and no copy anywhere.
    // Identical content is not backed up -- a no-op rewrite should not bury the
    // one real backup under a stack of copies of itself.
    let mut backed_up = false;
    if force && existed {
        if let Some(prev) = pkfs::read_to_string(path) {
            if prev.as_str() != content {
                backed_up = pkfs::write(&format!("{path}.bak"), &prev);
            }
        }
    }
    let ok = pkfs::write(path, content);
    let status = if !ok {
        "write-failed"
    } else if backed_up {
        "written-previous-backed-up"
    } else {
        "written"
    };
    (ok, status)
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
        let self_labeled_placeholder_text;
        let text = if has_compiled_default_for_prose_key(key) {
            compiled_default_for_prose_key(key)
        } else {
            self_labeled_placeholder_text = format!(
                "# {key}\n\nTODO: write the instruction prose for the `{key}` phase.\n\n\
                 This file was scaffolded as a PLACEHOLDER because `{key}` is declared as a\n\
                 state's `prose_key` in .gm/instructions/fsm/graph.json but has no compiled\n\
                 default in this build. Until it is written, that phase serves ENTRY prose.\n"
            );
            self_labeled_placeholder_text.as_str()
        };
        let shadowed = crate::prose::config_repo_text(key).filter(|repo_text| repo_text.trim() != text.trim());
        let (ok, status) = write_if_absent_or_forced(&path, text, force);
        let mut row = json!({ "path": path, "ok": ok, "status": status });
        if let (Some(repo_text), true) = (shadowed, ok) {
            row["shadows_config_repo"] = json!(true);
            row["shadowed_note"] = json!(format!(
                "this project's config repo supplies a DIFFERENT `{key}` ({} bytes vs the {} just written). \
                 The vendored file wins, so the project now runs the compiled default while believing it follows \
                 the config repo. Delete this file to fall back to the repo, or keep it as a deliberate override.",
                repo_text.len(),
                text.len()
            ));
        }
        results.push(row);
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
    let config_map_ref = {
        let g = fsm::graph();
        let mut lines = vec![
            "# What is configurable, and what needs Rust".to_string(),
            String::new(),
            "Which parts of the FSM a project can change by vendoring, and which require a code change. Generated by `fsm-vendor` from the live graph and registries, so the counts and names below describe THIS build rather than a remembered one.".to_string(),
            String::new(),
            "## Configurable by vendoring".to_string(),
            String::new(),
            format!("- **States** ({}): `states[]` in fsm/graph.json. Each carries `key`, `prose_key` and an optional `skill`. A state whose `prose_key` has no compiled default serves ENTRY prose until its .md is written, and `fsm-vendor` scaffolds an obvious placeholder rather than a plausible-looking wrong file.", g.states.len()),
            format!("- **Edges** ({}): `edges[]`. Each names `from`, `to`, and the `gates` that must pass. Removing a gate from an edge is how a project deliberately weakens it; `fsm-validate` reports every edge weaker than the compiled default rather than merging the gate back.", g.edges.len()),
            format!("- **Gate wiring** ({} defined): `gates[]`. A gate binds a `name` and `message` to either a compiled `predicate`, a jit `hook`, or both via `hook_mode`.", g.gates.len()),
            format!("- **Policy** ({} keys): the `policy` object. Full list in this build: {}.", crate::orchestrator::fsm::Graph::KNOWN_POLICY_KEYS.len(), crate::orchestrator::fsm::Graph::KNOWN_POLICY_KEYS.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")),
            format!("- **Deviation severity** ({} kinds): `policy.deviation_severity`, keyed by kind, valued `deny` or `log`. See fsm/deviations.md.", crate::orchestrator::deviations::known_deviations().len()),
            "- **Instruction prose**: every `.gm/instructions/<prose_key>.md`, plus the gate and residual message files. Resolved per key, so overriding one leaves the rest on the compiled default.".to_string(),
            "- **Jit hooks**: `.gm/instructions/hooks/*.js`, referenced by a gate's `hook`. This is the escape hatch for a condition with no compiled predicate -- it runs real JS at gate evaluation and fails CLOSED.".to_string(),
            "- **Browser and daemon knobs**: browser-config.json and daemon-project-config.json, per field.".to_string(),
            String::new(),
            "## Requires a Rust change".to_string(),
            String::new(),
            format!("- **The predicate set** ({} compiled): a graph's `gates[].predicate` can only name one of these. A name outside the set emits `fsm_unknown_predicate` and denies that gate permanently, so a genuinely new CONDITION needs either a jit hook or a new compiled predicate. Full list in fsm/predicates.md.", transitions::known_predicates().len()),
            "- **The deviation kind registry**: a project can re-weight a kind's severity, but the set of kinds a build can emit is compiled. A kind not in the table is emitted by no code path.".to_string(),
            "- **Hook lifecycle**: hooks fire at gate evaluation only. On-enter, on-exit and on-deviation points do not exist and are not reachable by configuration -- see fsm/invariants.md for why that boundary is deliberate.".to_string(),
            "- **Step progression shape**: single-slot by specification, not by omission. See fsm/invariants.md.".to_string(),
            "- **Verb dispatch and the spool ABI**: which verbs exist, what they accept, and the in/out file protocol.".to_string(),
            "- **Table names and embedding width**: `from_value` parses no key that reaches them, and a mismatched `memory.embed_dim` is rejected before it can reach a live store. This is deliberate: those two would drop or orphan real data.".to_string(),
            String::new(),
            "## The boundary worth remembering".to_string(),
            String::new(),
            "Configuration changes WHICH conditions are checked, in what order, and what a denial says. It does not change WHAT a condition can inspect -- that is the predicate set, and a jit hook is the sanctioned way past it without a build.".to_string(),
            String::new(),
        ];
        lines.push(String::new());
        lines.join("\n")
    };
    let config_map_path = ".gm/instructions/fsm/configurable.md";
    let (ok, status) = write_if_absent_or_forced(config_map_path, &config_map_ref, force);
    results.push(json!({ "path": config_map_path, "ok": ok, "status": status }));

    let invariants_path = ".gm/instructions/fsm/invariants.md";
    let (ok, status) = write_if_absent_or_forced(invariants_path, &invariants_ref, force);
    results.push(json!({ "path": invariants_path, "ok": ok, "status": status }));

    let merge_policy_path = ".gm/instructions/fsm/merge-policy.md";
    let (ok, status) = write_if_absent_or_forced(merge_policy_path, &merge_policy_doc(), force);
    results.push(json!({ "path": merge_policy_path, "ok": ok, "status": status }));

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

    let source_spec_path = ".gm/instructions/source.json.example";
    let (ok, status) = write_if_absent_or_forced(source_spec_path, SOURCE_SPEC_EXAMPLE, force);
    results.push(json!({
        "path": source_spec_path,
        "ok": ok,
        "status": status,
        "inert": true,
        "activate": "rename to .gm/instructions/source.json -- covers PROSE only (phase text, gate-denial, residual-scan messages), project-root only, no home-directory tier",
    }));

    let config_source_spec_path = ".gm/config.source.json.example";
    let (ok, status) = write_if_absent_or_forced(config_source_spec_path, CONFIG_SOURCE_SPEC_EXAMPLE, force);
    results.push(json!({
        "path": config_source_spec_path,
        "ok": ok,
        "status": status,
        "inert": true,
        "activate": "rename to .gm/config.source.json (this project) or ~/.gm/config.source.json (every project this user runs) -- covers the FSM GRAPH, POLICY, and GATE HOOKS, a much broader authority grant than the prose-only source.json above",
    }));

    let gm_config_path = ".gm/gm.config.json.example";
    let (ok, status) = write_if_absent_or_forced(gm_config_path, GM_CONFIG_EXAMPLE, force);
    results.push(json!({
        "path": gm_config_path,
        "ok": ok,
        "status": status,
        "inert": true,
        "activate": "rename to .gm/gm.config.json for a real config committed into this project, or publish a file shaped like this from the repo named in .gm/config.source.json -- fsm.graph names the relative path to a graph.json within that repo",
    }));

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
        "merge_policy": merge_policy_rows().iter().map(|r| json!({
            "path": r.path,
            "class": r.class,
            "policy": r.policy.id(),
            "unit": r.policy.unit(),
            "reader": r.reader,
            "placeholders": r.placeholders,
            "derived_from": r.derived,
            "caveat": r.note,
        })).collect::<Vec<Value>>(),
        "merge_policy_note": "Per-artifact precedence semantics, derived by walking the same registries the vendor pass writes from. There is deliberately no single global merge rule: `per-key-whole-file` prose falls back a FILE at a time with no merge inside it, the graph is `wholesale-replace`, and the two host JSON configs are `per-field-parse-gated` -- per-field while the file PARSES, whole-file-discarded the moment it does not. The same table is written to .gm/instructions/fsm/merge-policy.md. Rows whose `derived_from` begins NOT derived are transcribed from agentplug-host, which this build cannot see, and are the drift risk.",
        "note": "instruction/transition now serve from these files wherever present (per-key fallback to the compiled default for any prose file, wholesale-replace for the graph). gates/<key>.md and residual/<key>.md override the matching gate-denial/residual-scan message text via the same prose::resolve chain, and some of those defaults carry {token} placeholders the caller substitutes AFTER resolution -- an override that drops a token silently renders without that value (see merge_policy[].placeholders). browser-config.json and daemon-project-config.json are example defaults for the fields BrowserConfig/ProjectDaemonConfig actually read -- removing a field falls back to that field's compiled default INDIVIDUALLY, but a file that fails to parse is discarded WHOLE and silently, reverting every field at once with no diagnostic. Note the example's `headless: false` matches the host's own fallback, so deleting that field preserves headful behaviour rather than inverting it. The machine-wide ~/.agentplug/daemon-config.json is out of this per-project verb's reach (gm.wasm's fs sandbox is rooted at the project cwd); agentplug-runner itself scaffolds that file with the same example-defaults shape on first daemon boot if absent. Edit .gm/instructions/fsm/graph.json to add a custom phase, rewire an edge, or change which gates guard which transition -- no rebuild needed. Re-run this verb with {\"force\":true} to reset any of these back to the compiled defaults.",
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
