#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_exists, host_log};

pub const TOPLEVEL_DOC_ALLOWLIST: &[&str] = &[
    "AGENTS.md", "CLAUDE.md", "README.md", "SKILLS.md", "CHANGELOG.md", "LICENSE", "LICENSE.md",
];

pub const DEFER_MARKERS: &[&str] = &[
    "next pass", "next session", "next turn",
    "defer to later", "deferred to later", "deferred for later",
    "future pass", "future session", "future turn",
    "address it next", "address this next", "leave for next",
    "documented for next", "documented for future",
    "below criticality", "skip for now", "punt for now",
    "do later", "fix later", "later pass",
];

const AWAIT_ALLOWED_VERBS: &[&str] = &["memorize-continue", "instruction", "phase-status", "health"];

pub struct GateVerdict {
    pub allowed: bool,
    pub reason: Option<String>,
    pub await_result: bool,
    pub pending_step_id: Option<String>,
    pub residuals: Vec<String>,
}

impl GateVerdict {
    fn allow() -> Self {
        Self { allowed: true, reason: None, await_result: false, pending_step_id: None, residuals: vec![] }
    }
    fn deny(reason: String) -> Self {
        Self { allowed: false, reason: Some(reason), await_result: false, pending_step_id: None, residuals: vec![] }
    }
    pub fn to_denial_json(&self, verb: &str) -> Value {
        let mut obj = json!({
            "ok": false,
            "verb": verb,
            "gate_denied": true,
            "reason": self.reason.clone().unwrap_or_default(),
        });
        if self.await_result {
            obj["await_result"] = json!(true);
            if let Some(s) = &self.pending_step_id {
                obj["pending_step_id"] = json!(s);
            }
        }
        if !self.residuals.is_empty() {
            obj["residuals"] = json!(self.residuals);
        }
        obj
    }
}

fn now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

fn log_deviation(event: &str, detail: &str) {
    let msg = format!("plugkit gate: {} {}", event, detail);
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
}

fn read_pending_step() -> Option<String> {
    let content = host_read(".gm/turn-state.json").unwrap_or_default();
    if content.is_empty() { return None; }
    let v: Value = serde_json::from_str(&content).ok()?;
    let step_id = v.get("pending_step_id").and_then(|s| s.as_str())?.to_string();
    if step_id.is_empty() { return None; }
    let deadline = v.get("pending_step_deadline_ms").and_then(|n| n.as_u64()).unwrap_or(0);
    if deadline > 0 && now_ms() > deadline { return None; }
    Some(step_id)
}

fn body_path_field(body: &Value) -> Option<String> {
    for k in &["file_path", "filePath", "path"] {
        if let Some(s) = body.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() { return Some(s.to_string()); }
        }
    }
    None
}

fn classify_operation(verb: &str, body: &Value) -> &'static str {
    if verb == "transition" {
        if let Some(to) = body.get("to").and_then(|v| v.as_str()) {
            if to.eq_ignore_ascii_case("complete") || to.eq_ignore_ascii_case("stop") {
                return "complete";
            }
        }
    }
    if verb == "fs_write" { return "write"; }
    "verb"
}

fn prd_has_open_items() -> bool {
    let content = host_read(".gm/prd.yml").unwrap_or_default();
    content.contains("status: pending") || content.contains("status: in_progress")
}

fn mutables_unresolved() -> bool {
    let content = host_read(".gm/mutables.yml").unwrap_or_default();
    content.contains("status: unknown")
}

fn defer_marker_in(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    for m in DEFER_MARKERS {
        if lower.contains(m) { return Some(*m); }
    }
    None
}

fn is_unsolicited_toplevel_doc(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    if norm.contains('/') { return false; }
    let lower_ext_is_doc = norm.to_lowercase().ends_with(".md") || norm.to_lowercase().ends_with(".txt");
    if !lower_ext_is_doc { return false; }
    !TOPLEVEL_DOC_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(&norm))
}

pub fn check_dispatch(verb: &str, body: &Value) -> GateVerdict {
    if let Some(step_id) = read_pending_step() {
        if !AWAIT_ALLOWED_VERBS.contains(&verb) {
            log_deviation("await-result-violation", &format!("verb={} step={}", verb, step_id));
            let mut v = GateVerdict::deny(format!(
                "pipeline suspended at step_id={}; only memorize-continue advances state. \
                 Read the AWAIT-RESULT instruction (dispatch `instruction`), compute the step inline \
                 using its prompt_template, then dispatch memorize-continue with the result. \
                 No other verb is valid until this completes.",
                step_id
            ));
            v.await_result = true;
            v.pending_step_id = Some(step_id);
            return v;
        }
    }

    let operation = classify_operation(verb, body);

    if operation == "complete" {
        let mut residuals: Vec<String> = vec![];
        if host_exists(".gm/prd.yml") && prd_has_open_items() {
            residuals.push("PRD has open items — resolve or name-and-stop before declaring done".into());
        }
        if host_exists(".gm/mutables.yml") && mutables_unresolved() {
            residuals.push("unresolved mutables present — resolve with witness_evidence before declaring done".into());
        }
        if !residuals.is_empty() {
            log_deviation("gate-deny", &format!("stop-gate residuals={}", residuals.len()));
            let mut v = GateVerdict::deny(format!("stop-gate residuals: {}", residuals.join("; ")));
            v.residuals = residuals;
            return v;
        }
    }

    if verb == "fs_write" {
        if let Some(p) = body_path_field(body) {
            if is_unsolicited_toplevel_doc(&p) {
                log_deviation("unsolicited-doc-created", &p);
            }
        }
    }

    let prd_content = host_read(".gm/prd.yml").unwrap_or_default();
    if !prd_content.is_empty() {
        for line in prd_content.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("subject:") {
                if let Some(marker) = defer_marker_in(rest) {
                    log_deviation("prd-defer-marker", marker);
                    break;
                }
            }
            if let Some(rest) = line.trim_start().strip_prefix("description:") {
                if let Some(marker) = defer_marker_in(rest) {
                    log_deviation("prd-defer-marker", marker);
                    break;
                }
            }
        }
    }

    GateVerdict::allow()
}
