#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_log, host_write};

pub const GATE_LONG_GAP_NO_INSTRUCTION_DEFAULT: &str = "long-gap-no-instruction: {gap_ms}ms since last `instruction` dispatch (threshold 300000ms). Idle mid-chain is a deviation. Dispatch `instruction` for recovery prose before any other verb.";

const GATE_REPEAT_STATE_PATH: &str = ".gm/exec-spool/.gate-deviation-repeats.json";

fn gate_repeat_key(operation: &str, event: &str) -> String {
    format!("{}::{}", operation, event)
}

fn record_gate_repeat(operation: &str, event: &str) -> u64 {
    let key = gate_repeat_key(operation, event);
    let mut state: Value = host_read(GATE_REPEAT_STATE_PATH)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let count = state.get(&key).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
    if let Some(obj) = state.as_object_mut() {
        obj.insert(key, json!(count));
    }
    let _ = host_write(GATE_REPEAT_STATE_PATH, &state.to_string());
    count
}

pub fn clear_gate_repeats(operation: &str) {
    let prefix = format!("{}::", operation);
    let mut state: Value = host_read(GATE_REPEAT_STATE_PATH)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = state.as_object_mut() {
        obj.retain(|k, _| !k.starts_with(&prefix));
        let _ = host_write(GATE_REPEAT_STATE_PATH, &state.to_string());
    }
}

pub struct GateVerdict {
    pub allowed: bool,
    pub reason: Option<String>,
    pub await_result: bool,
    pub pending_step_id: Option<String>,
    pub pending_step_full: Option<Value>,
    pub residuals: Vec<String>,
    pub next_dispatch: Option<String>,
}

impl GateVerdict {
    fn allow() -> Self {
        Self { allowed: true, reason: None, await_result: false, pending_step_id: None, pending_step_full: None, residuals: vec![], next_dispatch: None }
    }
    fn deny(reason: String) -> Self {
        Self { allowed: false, reason: Some(reason), await_result: false, pending_step_id: None, pending_step_full: None, residuals: vec![], next_dispatch: None }
    }
    fn with_next(mut self, next: &str) -> Self {
        self.next_dispatch = Some(next.to_string());
        self
    }
    pub fn to_denial_json(&self, verb: &str) -> Value {
        let next: &str = self.next_dispatch.as_deref().unwrap_or(if self.await_result { "memorize-continue" } else { "instruction" });
        let reason_with_hint = format!(
            "{} - dispatch `{}` for recovery; do not improvise around this denial.",
            self.reason.clone().unwrap_or_default(),
            next,
        );
        let mut obj = json!({
            "ok": false,
            "verb": verb,
            "gate_denied": true,
            "reason": reason_with_hint,
            "next_dispatch": next,
        });
        if self.await_result {
            obj["await_result"] = json!(true);
            if let Some(s) = &self.pending_step_id {
                obj["pending_step_id"] = json!(s);
            }
            if let Some(full) = &self.pending_step_full {
                obj["pending_step_full"] = full.clone();
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

fn parse_retry_state_v2(s: &str) -> (String, u32, u64) {
    let s = s.trim();
    if s.is_empty() { return (String::new(), 0, 0); }
    let mut parts = s.splitn(3, '|');
    let verb = parts.next().unwrap_or("").to_string();
    let count = parts.next().and_then(|c| c.trim().parse::<u32>().ok()).unwrap_or(0);
    let ts = parts.next().and_then(|t| t.trim().parse::<u64>().ok()).unwrap_or(0);
    (verb, count, ts)
}

fn is_longgap_exempt(verb: &str) -> bool {
    crate::orchestrator::fsm::graph().policy.longgap_exempt_verbs.iter().any(|v| v == verb)
}

fn long_gap_should_fire(last_instruction_ms: u64, prev_dispatch_ms: u64, now: u64, threshold: u64) -> bool {
    if last_instruction_ms == 0 { return false; }
    let idle_since_instruction = now.saturating_sub(last_instruction_ms) > threshold;
    let idle_since_any = prev_dispatch_ms == 0 || now.saturating_sub(prev_dispatch_ms) > threshold;
    idle_since_instruction && idle_since_any
}

fn log_deviation(event: &str, detail: &str) {
    let msg = format!("plugkit gate: {} {}", event, detail);
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
    let evt_payload = json!({
        "event": format!("deviation.{}", event),
        "sub": "hook",
        "detail": detail,
        "ts": now_ms(),
        "source": "rs-plugkit/gates",
    });
    let evt_line = format!("evt: {}", evt_payload);
    unsafe { host_log(1, evt_line.as_ptr(), evt_line.len() as u32); }
}

fn parse_pending_step() -> Option<(String, u64)> {
    let content = host_read(".gm/turn-state.json").unwrap_or_default();
    if content.is_empty() { return None; }
    let v: Value = serde_json::from_str(&content).ok()?;
    let step_id = v.get("pending_step_id").and_then(|s| s.as_str())?.to_string();
    if step_id.is_empty() { return None; }
    let deadline = v.get("pending_step_deadline_ms").and_then(|n| n.as_u64()).unwrap_or(0);
    if deadline > 0 && now_ms() > deadline { return None; }
    Some((step_id, deadline))
}

fn read_pending_step() -> Option<String> {
    parse_pending_step().map(|(step_id, _)| step_id)
}

fn read_pending_step_full() -> Option<Value> {
    let (step_id, deadline) = parse_pending_step()?;
    let kv_namespace = "rs-learn/pipeline";
    let state_raw = crate::wasm_dispatch::host_kv_read(kv_namespace, &step_id).unwrap_or_default();
    let state: Value = serde_json::from_str(&state_raw).unwrap_or(Value::Null);
    Some(json!({
        "step_id": step_id,
        "deadline_ms": deadline,
        "kv_namespace": kv_namespace,
        "kv_key": step_id,
        "state": state,
    }))
}

fn body_path_field(body: &Value) -> Option<String> {
    for k in &["file_path", "filePath", "path"] {
        if let Some(s) = body.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() { return Some(s.to_string()); }
        }
    }
    None
}

fn current_phase_key() -> String {
    crate::orchestrator::state::read_state().phase.as_str().to_string()
}

fn classify_operation(verb: &str, body: &Value) -> &'static str {
    if verb == "transition" {
        if let Some(to) = body.get("to").and_then(|v| v.as_str()) {
            if to.eq_ignore_ascii_case("complete") || to.eq_ignore_ascii_case("stop") {
                return "complete";
            }
            if to.eq_ignore_ascii_case("consolidate") {
                return "consolidate";
            }
        }
    }
    if verb == "fs_write" { return "write"; }
    "verb"
}

fn is_unsolicited_toplevel_doc(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    if norm.contains('/') { return false; }
    let lower_ext_is_doc = norm.to_lowercase().ends_with(".md") || norm.to_lowercase().ends_with(".txt");
    if !lower_ext_is_doc { return false; }
    !crate::orchestrator::fsm::graph().policy.toplevel_doc_allowlist.iter().any(|a| a.eq_ignore_ascii_case(&norm))
}

fn extract_substitution_bodies(cmd: &str) -> Vec<String> {
    let bytes: Vec<char> = cmd.chars().collect();
    let mut bodies: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == '$' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
            let mut depth = 1i32;
            let mut j = i + 2;
            let start = j;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth > 0 { j += 1; }
            }
            bodies.push(bytes[start..j.min(bytes.len())].iter().collect());
            i = if j < bytes.len() { j + 1 } else { bytes.len() };
            continue;
        }
        if bytes[i] == '`' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != '`' {
                j += 1;
            }
            bodies.push(bytes[(i + 1)..j.min(bytes.len())].iter().collect());
            i = if j < bytes.len() { j + 1 } else { bytes.len() };
            continue;
        }
        i += 1;
    }
    bodies
}

pub fn check_dispatch(verb: &str, body: &Value) -> GateVerdict {
    if let Some(step_id) = read_pending_step() {
        if !crate::orchestrator::fsm::graph().policy.await_allowed_verbs.iter().any(|v| v == verb) {
            log_deviation("await-result-violation", &format!("verb={} step={}", verb, step_id));
            let mut v = GateVerdict::deny(format!(
                "pipeline suspended at step_id={}; only memorize-continue advances state. \
                 The full pending_step recovery payload is embedded in this response as `pending_step_full` \
                 (no need to re-dispatch `instruction` first). Compute the step inline using \
                 `pending_step_full.state.pipeline[cursor].payload` and the prompt_template, then dispatch \
                 memorize-continue with body {{token, step_id, result}}. No other verb is valid until \
                 this completes.",
                step_id
            ));
            v.await_result = true;
            v.pending_step_id = Some(step_id.clone());
            v.pending_step_full = read_pending_step_full();
            return v;
        }
    }

    if matches!(verb, "bash" | "sh" | "shell" | "zsh" | "powershell" | "ps1") {
        let cmd = body.get("command").and_then(|v| v.as_str())
            .or_else(|| body.get("code").and_then(|v| v.as_str()))
            .or_else(|| body.get("script").and_then(|v| v.as_str()))
            .unwrap_or("");
        let is_git_token = |first: &str| {
            first == "git" || first == "git.exe"
                || first.ends_with("/git") || first.ends_with("\\git")
                || first.ends_with("/git.exe") || first.ends_with("\\git.exe")
        };
        let git_dominant = cmd
            .split(|c| c == ';' || c == '\n' || c == '|' || c == '&')
            .map(|s| s.trim_start())
            .any(|s| {
                let first = s.split_whitespace().next().unwrap_or("");
                is_git_token(first)
            });
        let git_in_subshell = extract_substitution_bodies(cmd).into_iter().any(|inner| {
            inner
                .split(|c| c == ';' || c == '\n' || c == '|' || c == '&')
                .map(|s| s.trim_start())
                .any(|s| {
                    let first = s.split_whitespace().next().unwrap_or("");
                    is_git_token(first)
                })
        });
        if git_dominant || git_in_subshell {
            log_deviation("bash-git-bypass", &format!("verb={} cmd={}", verb, cmd.chars().take(80).collect::<String>()));
            return GateVerdict::deny(format!(
                "bash-git-bypass: a `{}` verb invoking `git` is denied - git is a first-class spool surface, not a shell command. Use the git verb instead: \
                 git_status (porcelain), git_log, git_diff, git_show, git_branch (inspect); git_add, git_commit, git_finalize (stage/commit/push in one), git_push (push w/ rebase-retry); git_checkout, git_fetch, git_rm, git_revert, git_reset (mutate). \
                 git_finalize {{message}} bundles add->commit->porcelain-gate->push in ONE dispatch. The shell git bypasses the porcelain gate, the witness ledger, and is non-portable. Command was: `{}`",
                verb, cmd.chars().take(120).collect::<String>()
            )).with_next("git_finalize");
        }
    }

    let prev_dispatch_ms: u64 = if !is_longgap_exempt(verb) {
        let p = host_read(".gm/last-dispatch-ts").unwrap_or_default().trim().parse().unwrap_or(0);
        let _ = crate::wasm_dispatch::host_write(".gm/last-dispatch-ts", &now_ms().to_string());
        p
    } else { 0 };

    if verb == "instruction" || verb == "transition" || verb == "phase-status"
        || verb == "prd-add" || verb == "prd-resolve" || verb == "prd-list"
        || verb == "mutable-add" || verb == "mutable-resolve" || verb == "mutable-list" {
        let now = now_ms();
        let _ = crate::wasm_dispatch::host_write(".gm/last-instruction-ts", &now.to_string());
        let _ = crate::wasm_dispatch::host_write(".gm/long-gap-retry-state", "");
    } else if !is_longgap_exempt(verb) {
        let last = host_read(".gm/last-instruction-ts").unwrap_or_default();
        let last_ms: u64 = last.trim().parse().unwrap_or(0);
        let now = now_ms();
        let longgap_threshold_ms = crate::orchestrator::fsm::graph().policy.longgap_threshold_ms;
        if long_gap_should_fire(last_ms, prev_dispatch_ms, now, longgap_threshold_ms) {
            let gap_ms = now - last_ms;
            let retry_state = host_read(".gm/long-gap-retry-state").unwrap_or_default();
            let (last_verb, count, last_denial_ts) = parse_retry_state_v2(&retry_state);
            let since_last_denial = now.saturating_sub(last_denial_ts);
            let same_burst = last_denial_ts > 0 && since_last_denial <= 5_000;
            let new_count = if last_verb == verb && since_last_denial > 5_000 { count + 1 } else if last_verb == verb { count } else { 1u32 };
            let _ = crate::wasm_dispatch::host_write(".gm/long-gap-retry-state", &format!("{}|{}|{}", verb, new_count, now));
            if new_count >= 2 {
                if !same_burst {
                    log_deviation("long-gap-retry-without-instruction", &format!("verb={} consecutive_retries={} gap_ms={}", verb, new_count, gap_ms));
                }
                return GateVerdict::deny(format!(
                    "long-gap-retry-without-instruction: verb=`{}` denied {}x in a row by long-gap-no-instruction gate, yet the agent retried instead of dispatching `instruction`. The gate's `next_dispatch` field names the recovery verb - when it says `instruction`, the next verb IS `instruction`, not the same verb again. Dispatch `instruction` now; the chain cannot recover by re-attempting the denied verb.",
                    verb, new_count
                ));
            }
            if !same_burst {
                log_deviation("long-gap-no-instruction", &format!("verb={} gap_ms={}", verb, gap_ms));
            }
            return GateVerdict::deny(
                crate::prose::resolve_and_mark(
                    "gates/long-gap-no-instruction",
                    GATE_LONG_GAP_NO_INSTRUCTION_DEFAULT,
                )
                .replace("{gap_ms}", &gap_ms.to_string()),
            );
        }
    }

    let operation = classify_operation(verb, body);

    if operation == "consolidate" || operation == "complete" {
        let from = current_phase_key();
        let to = if operation == "consolidate" { "CONSOLIDATE" } else { "COMPLETE" };
        let (residuals, next_recovery) = crate::orchestrator::transitions::gate_residuals(&from, to);
        if !residuals.is_empty() {
            log_deviation("gate-deny", &format!("{}-gate residuals={}", operation, residuals.len()));
            let repeat_count = record_gate_repeat(operation, "gate-deny");
            let label = if operation == "consolidate" { "consolidate-gate" } else { "stop-gate" };
            let mut reason = format!("{} residuals: {}", label, residuals.join("; "));
            if repeat_count >= crate::orchestrator::fsm::graph().policy.gate_repeat_escalate_threshold {
                log_deviation("stuck-loop-escalation", &format!("operation={} repeat_count={}", operation, repeat_count));
                reason = format!(
                    "{} -- STUCK LOOP DETECTED: this exact gate denial has now fired {} times in a row with no successful transition between attempts. Retrying the bare transition again will repeat the same denial. Stop retrying: (1) `prd-add` a row describing the concrete stuck state (which residual, what you tried, why it did not clear), (2) invoke the wfgy-method skill's BBCR bounded-retry-then-surface discipline to recover instead of blind-retrying, (3) only then re-attempt the transition.",
                    reason, repeat_count
                );
            }
            let mut v = GateVerdict::deny(reason);
            v.residuals = residuals;
            v.next_dispatch = next_recovery;
            return v;
        }
        clear_gate_repeats(operation);
    }

    if verb == "fs_write" {
        if let Some(p) = body_path_field(body) {
            if is_unsolicited_toplevel_doc(&p) {
                log_deviation("unsolicited-doc-created", &p);
            }
        }
    }

    if operation == "complete" {
        let (body_s, _err, code) = crate::orchestrator::prd::handle_list("");
        if code == 0 {
            if let Ok(v) = serde_json::from_str::<Value>(&body_s) {
                if let Some(items) = v.get("items").and_then(|v| v.as_array()) {
                    for it in items {
                        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                        if crate::orchestrator::prd::status_is_open(status) { continue; }
                        let witness = it.get("witness_evidence").or_else(|| it.get("witness")).and_then(|v| v.as_str()).unwrap_or("");
                        if witness.trim().is_empty() {
                            let id = it.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            log_deviation("prd-anti-shape", &format!("id={} status={} no witness_evidence on closing transition", id, status));
                        }
                    }
                }
            }
        }
    }

    GateVerdict::allow()
}
