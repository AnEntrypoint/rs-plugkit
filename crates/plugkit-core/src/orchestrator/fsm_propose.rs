#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::pkfs;
use crate::config_path::validate_prose_key;
use super::fsm;

const INSTRUCTIONS_BASE: &str = ".gm/instructions";
const GRAPH_PATH: &str = ".gm/instructions/fsm/graph.json";
const CONFIG_PATH: &str = ".gm/gm.config.json";

fn prose_target_path(key: &str) -> Result<String, String> {
    validate_prose_key(key)?;
    let path = format!("{INSTRUCTIONS_BASE}/{key}.md");
    if !crate::config_path::path_contained_within(INSTRUCTIONS_BASE, &path) {
        return Err(format!("prose key resolves to {path}, which escapes {INSTRUCTIONS_BASE}"));
    }
    Ok(path)
}

pub fn handle_propose(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content.trim()).unwrap_or(Value::Null);
    let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let proposed_text = body.get("proposed_text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let reason = body.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let witness = body.get("witness").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let user_confirmed = body.get("user_confirmed").and_then(|v| v.as_bool()).unwrap_or(false);

    if !matches!(kind.as_str(), "prose" | "graph" | "config") {
        return (
            json!({
                "ok": false,
                "error": "fsm-propose-override requires body.kind in {\"prose\",\"graph\",\"config\"}",
            }).to_string(),
            String::new(),
            1,
        );
    }
    if proposed_text.trim().is_empty() {
        return (
            json!({ "ok": false, "error": "fsm-propose-override requires non-empty body.proposed_text" }).to_string(),
            String::new(),
            1,
        );
    }
    if reason.trim().is_empty() || witness.trim().is_empty() {
        return (
            json!({
                "ok": false,
                "error": "fsm-propose-override requires body.reason and body.witness -- a self-authored override with no cited friction signal (gate-repeat count, deviation kind, file:line) is a guess, not a proposal",
            }).to_string(),
            String::new(),
            1,
        );
    }

    let target_path = match kind.as_str() {
        "prose" => {
            if key.trim().is_empty() {
                return (
                    json!({ "ok": false, "error": "kind=prose requires body.key naming the prose key (e.g. \"specify\")" }).to_string(),
                    String::new(),
                    1,
                );
            }
            match prose_target_path(&key) {
                Ok(p) => p,
                Err(e) => return (json!({ "ok": false, "error": e }).to_string(), String::new(), 1),
            }
        }
        "graph" => GRAPH_PATH.to_string(),
        "config" => CONFIG_PATH.to_string(),
        _ => unreachable!(),
    };

    let requires_authority = kind == "graph";
    let current_text = pkfs::read_to_string(&target_path);

    let mut validation_problems: Vec<String> = Vec::new();
    let mut validation_gates_weaker: Vec<Value> = Vec::new();
    if kind == "graph" {
        match serde_json::from_str::<fsm::Graph>(&proposed_text) {
            Ok(parsed) => {
                validation_problems = parsed.validate();
                validation_gates_weaker = fsm::gates_missing_vs_default(&parsed)
                    .into_iter()
                    .map(|(from, to, missing)| json!({ "from": from, "to": to, "missing_gates": missing }))
                    .collect();
            }
            Err(e) => validation_problems.push(format!("proposed graph does not parse as valid JSON graph: {e}")),
        }
    }
    if kind == "config" {
        if let Err(e) = serde_json::from_str::<Value>(&proposed_text) {
            validation_problems.push(format!("proposed config does not parse as valid JSON: {e}"));
        }
    }

    if !validation_problems.is_empty() {
        return (
            json!({
                "ok": false,
                "proposed": false,
                "error": "proposed override fails validation -- fix and re-propose, never trust an unparsed/inconsistent artifact",
                "validation_problems": validation_problems,
                "validation_gates_weaker": validation_gates_weaker,
            }).to_string(),
            String::new(),
            1,
        );
    }

    if !user_confirmed {
        let diff_summary = match &current_text {
            Some(cur) if cur.trim() == proposed_text.trim() => "proposed text is identical to the current file -- no-op".to_string(),
            Some(cur) => format!("current file is {} bytes, proposed is {} bytes", cur.len(), proposed_text.len()),
            None => format!("no file currently exists at {target_path} -- this would create it"),
        };
        let gate_name = body.get("gate").and_then(|v| v.as_str()).unwrap_or(key.as_str());
        let friction_query = format!("gate {gate_name} {reason}");
        let historical_friction_hits = super::recall::recall_hits(&friction_query, 5);
        return (
            json!({
                "ok": true,
                "proposed": true,
                "requires_confirmation": true,
                "requires_execution_authority": requires_authority,
                "kind": kind,
                "target_path": target_path,
                "reason": reason,
                "witness": witness,
                "diff_summary": diff_summary,
                "proposed_text": proposed_text,
                "validation_gates_weaker": validation_gates_weaker,
                "historical_friction_hits": historical_friction_hits,
                "historical_friction_note": "Prior cross-session recall hits for this gate/friction pattern -- extrapolate the proposal from repeated real friction, not only this session's single instance, before dispatching with user_confirmed:true.",
                "note": if requires_authority {
                    "kind=graph grants execution authority (vendored graphs may carry gate hooks). Dispatch AskUserQuestion, state which gates the proposed graph drops relative to the compiled default (see validation_gates_weaker), and only re-dispatch this verb with user_confirmed:true after the user answers yes."
                } else {
                    "No filesystem write has happened. Re-dispatch this verb with an identical body plus user_confirmed:true to apply it."
                },
            }).to_string(),
            String::new(),
            0,
        );
    }

    if requires_authority {
        let confirmation_witness = body.get("confirmation_witness").and_then(|v| v.as_str()).unwrap_or("");
        if confirmation_witness.trim().is_empty() {
            return (
                json!({
                    "ok": false,
                    "error": "kind=graph with user_confirmed:true also requires body.confirmation_witness citing the AskUserQuestion dispatch that obtained consent -- execution-authority writes are never applied on a bare confirmed flag with no witnessed consent trail",
                }).to_string(),
                String::new(),
                1,
            );
        }
    }

    let mut backed_up = false;
    if let Some(prev) = &current_text {
        if prev.trim() != proposed_text.trim() {
            backed_up = pkfs::write(&format!("{target_path}.bak"), prev);
        }
    }
    let write_ok = pkfs::write(&target_path, &proposed_text);

    let post_write_check: Value = if kind == "graph" && write_ok {
        let (graph, tier, source_path) = fsm::graph_detailed();
        json!({
            "active_tier": tier.as_str(),
            "active_source_path": source_path,
            "took_effect": tier == fsm::GraphTier::LocalOverride,
            "problems": graph.validate(),
        })
    } else if kind == "config" && write_ok {
        let resolution = crate::config::resolve_forced(".");
        json!({
            "active_tier": resolution.tier.as_str(),
            "took_effect": resolution.tier == crate::config::Tier::ProjectVendored,
        })
    } else {
        Value::Null
    };

    let payload = json!({
        "ok": write_ok,
        "proposed": false,
        "applied": write_ok,
        "kind": kind,
        "target_path": target_path,
        "backed_up_previous": backed_up,
        "reason": reason,
        "witness": witness,
        "post_write_check": post_write_check,
        "note": "Write took the same path prose::resolve/fsm::graph_detailed/config::resolve already read on their next call -- no new read-path code, effective on next dispatch.",
    });
    (payload.to_string(), String::new(), if write_ok { 0 } else { 1 })
}
