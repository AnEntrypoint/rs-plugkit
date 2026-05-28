#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_exists, host_log};

pub const TOPLEVEL_DOC_ALLOWLIST: &[&str] = &[
    "AGENTS.md", "CLAUDE.md", "README.md", "SKILLS.md", "CHANGELOG.md", "LICENSE", "LICENSE.md",
];

const AWAIT_ALLOWED_VERBS: &[&str] = &["memorize-continue", "instruction", "phase-status", "health"];

pub struct GateVerdict {
    pub allowed: bool,
    pub reason: Option<String>,
    pub await_result: bool,
    pub pending_step_id: Option<String>,
    pub pending_step_full: Option<Value>,
    pub residuals: Vec<String>,
}

impl GateVerdict {
    fn allow() -> Self {
        Self { allowed: true, reason: None, await_result: false, pending_step_id: None, pending_step_full: None, residuals: vec![] }
    }
    fn deny(reason: String) -> Self {
        Self { allowed: false, reason: Some(reason), await_result: false, pending_step_id: None, pending_step_full: None, residuals: vec![] }
    }
    pub fn to_denial_json(&self, verb: &str) -> Value {
        let next = if self.await_result { "memorize-continue" } else { "instruction" };
        let reason_with_hint = format!(
            "{} — dispatch `{}` for recovery; do not improvise around this denial.",
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

fn parse_retry_state(s: &str) -> (String, u32) {
    let s = s.trim();
    if s.is_empty() { return (String::new(), 0); }
    let mut parts = s.splitn(2, '|');
    let verb = parts.next().unwrap_or("").to_string();
    let count = parts.next().and_then(|c| c.trim().parse::<u32>().ok()).unwrap_or(0);
    (verb, count)
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

// Verbs exempt from the long-gap gate and its dispatch-clock bookkeeping. health and
// auto-recall are internal/orientation pulses; wait and sleep are the agent deliberately
// pausing to poll a long-running task (wasm_dispatch.rs wait/sleep) — firing
// long-gap-no-instruction on a deliberate wait is contradictory. Checked in BOTH the
// prev-dispatch-stamp guard and the gap-check branch, so they must come from one source.
const LONGGAP_EXEMPT: &[&str] = &["health", "auto-recall", "wait", "sleep"];
fn is_longgap_exempt(verb: &str) -> bool { LONGGAP_EXEMPT.contains(&verb) }

// The long-gap gate fires only on genuine IDLE-mid-chain: both >threshold since the last
// instruction AND >threshold since the previous dispatch of any verb. prev_dispatch_ms==0
// means no prior work dispatch in this window (boot/overnight idle), which counts as idle.
// Active back-to-back work verbs keep prev_dispatch_ms fresh, so the gate stays quiet.
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

fn read_pending_step_full() -> Option<Value> {
    let content = host_read(".gm/turn-state.json").unwrap_or_default();
    if content.is_empty() { return None; }
    let v: Value = serde_json::from_str(&content).ok()?;
    let step_id = v.get("pending_step_id").and_then(|s| s.as_str())?.to_string();
    if step_id.is_empty() { return None; }
    let deadline = v.get("pending_step_deadline_ms").and_then(|n| n.as_u64()).unwrap_or(0);
    if deadline > 0 && now_ms() > deadline { return None; }
    let kv_key = format!("rs-learn/pipeline/{}", step_id);
    let state_raw = crate::wasm_dispatch::host_kv_read("rs-learn/pipeline", &step_id).unwrap_or_default();
    let state: Value = serde_json::from_str(&state_raw).unwrap_or(Value::Null);
    Some(json!({
        "step_id": step_id,
        "deadline_ms": deadline,
        "kv_key": kv_key,
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

fn worktree_dirty() -> bool {
    !crate::wasm_dispatch::git_porcelain().trim().is_empty()
}

fn check_browser_witness_coverage() -> Vec<String> {
    let edits_raw = host_read(".gm/exec-spool/.turn-browser-edits.json").unwrap_or_default();
    if edits_raw.trim().is_empty() { return vec![]; }
    let edits: Vec<Value> = match serde_json::from_str::<Value>(&edits_raw) {
        Ok(Value::Array(arr)) => arr,
        _ => return vec![],
    };
    if edits.is_empty() { return vec![]; }
    let witness_raw = host_read(".gm/exec-spool/.turn-browser-witnessed").unwrap_or_default();
    let witnessed_hashes: serde_json::Map<String, Value> = if witness_raw.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<Value>(&witness_raw).ok()
            .and_then(|v| v.get("witnessed_hashes").cloned())
            .and_then(|v| if let Value::Object(m) = v { Some(m) } else { None })
            .unwrap_or_default()
    };
    let mut unwitnessed: Vec<String> = vec![];
    for entry in edits.iter() {
        let file = match entry.get("file").and_then(|v| v.as_str()) {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        let edit_hash = entry.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        if edit_hash.is_empty() { continue; }
        let witness_hash = witnessed_hashes.get(file).and_then(|v| v.as_str()).unwrap_or("");
        if witness_hash != edit_hash {
            unwitnessed.push(file.to_string());
        }
    }
    unwitnessed
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
        let first = cmd.trim_start().split_whitespace().next().unwrap_or("");
        let git_dominant = first == "git"
            || first.ends_with("/git") || first.ends_with("\\git")
            || cmd.trim_start().starts_with("git ");
        if git_dominant {
            log_deviation("bash-git-bypass", &format!("verb={} cmd={}", verb, cmd.chars().take(80).collect::<String>()));
            return GateVerdict::deny(format!(
                "bash-git-bypass: a `{}` verb invoking `git` is denied — git is a first-class spool surface, not a shell command. Use the git verb instead: \
                 git_status (porcelain), git_log, git_diff, git_show, git_branch (inspect); git_add, git_commit, git_finalize (stage/commit/push in one), git_push (push w/ rebase-retry); git_checkout, git_fetch, git_rm, git_revert, git_reset (mutate). \
                 git_finalize {{message}} bundles add→commit→porcelain-gate→push in ONE dispatch. The shell git bypasses the porcelain gate, the witness ledger, and is non-portable. Command was: `{}`",
                verb, cmd.chars().take(120).collect::<String>()
            ));
        }
    }

    // Read the PRIOR last-ANY-dispatch ts BEFORE overwriting it: the long-gap gate guards
    // against IDLE-mid-chain, not active-work-without-re-reading-prose. A rapid succession of
    // work verbs (browser/exec_js/codesearch/...) is active work, so the gap since the PREVIOUS
    // dispatch — not since the last instruction alone — is what distinguishes idle from work.
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
        // The clock fires only on genuine IDLE: >300s since the last instruction AND
        // >300s since the PREVIOUS dispatch of any verb. Active back-to-back work verbs keep
        // the previous-dispatch ts fresh, so a browser-heavy debugging stretch never trips the
        // gate; a true overnight/boot idle (no dispatches at all, prev==0) still does.
        if long_gap_should_fire(last_ms, prev_dispatch_ms, now, 300_000) {
            let gap_ms = now - last_ms;
            let retry_state = host_read(".gm/long-gap-retry-state").unwrap_or_default();
            let (last_verb, count, last_denial_ts) = parse_retry_state_v2(&retry_state);
            let since_last_denial = now.saturating_sub(last_denial_ts);
            // A burst of dispatches within 5s while the gate is tripped is ONE logical gate-trip,
            // not N — whether they are the same verb (a retry loop) or different verbs (a parallel
            // orient fan-out of recall+codesearch). Dedup is TIME-based, not verb-based: any prior
            // long-gap denial recorded within the last 5s makes this dispatch part of the same burst,
            // so a 4-verb parallel fan-out logs one long-gap-no-instruction event, not four. Verb-keyed
            // dedup raced under parallel dispatch (non-atomic read-modify-write of the retry-state file
            // let each verb see no prior verb and log), so the window is keyed only on last_denial_ts.
            // The escalation count still tracks genuine SAME-verb retries past the window (the
            // ignored-next_dispatch failure mode). The deny still fires for every dispatch (correct);
            // only the audit noise dedups.
            let same_burst = last_denial_ts > 0 && since_last_denial <= 5_000;
            let new_count = if last_verb == verb && since_last_denial > 5_000 { count + 1 } else if last_verb == verb { count } else { 1u32 };
            let _ = crate::wasm_dispatch::host_write(".gm/long-gap-retry-state", &format!("{}|{}|{}", verb, new_count, now));
            if new_count >= 2 {
                if !same_burst {
                    log_deviation("long-gap-retry-without-instruction", &format!("verb={} consecutive_retries={} gap_ms={}", verb, new_count, gap_ms));
                }
                return GateVerdict::deny(format!(
                    "long-gap-retry-without-instruction: verb=`{}` denied {}× in a row by long-gap-no-instruction gate, yet the agent retried instead of dispatching `instruction`. The gate's `next_dispatch` field names the recovery verb — when it says `instruction`, the next verb IS `instruction`, not the same verb again. Dispatch `instruction` now; the chain cannot recover by re-attempting the denied verb.",
                    verb, new_count
                ));
            }
            if !same_burst {
                log_deviation("long-gap-no-instruction", &format!("verb={} gap_ms={}", verb, gap_ms));
            }
            return GateVerdict::deny(format!(
                "long-gap-no-instruction: {}ms since last `instruction` dispatch (threshold 300000ms). Idle mid-chain is a deviation. Dispatch `instruction` for recovery prose before any other verb.",
                gap_ms
            ));
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
        if worktree_dirty() {
            residuals.push("worktree dirty — commit or revert before declaring done; an unpushed delta is an unwitnessed slice".into());
            log_deviation("push-dirty", "COMPLETE attempted with dirty worktree");
        }
        if !host_exists(".gm/residual-check-fired") {
            residuals.push("residual-scan not fired in this stop window — dispatch residual-scan before COMPLETE".into());
            log_deviation("complete-without-residual-scan", "");
        }
        let bw_check = check_browser_witness_coverage();
        if !bw_check.is_empty() {
            residuals.push(format!("client-edit-no-witness: {} client-side file(s) edited without browser-witness in this session — dispatch `browser` verb to page.evaluate the invariant each edit establishes, then re-attempt COMPLETE. Files: {}", bw_check.len(), bw_check.join(", ")));
            log_deviation("client-edit-no-witness", &format!("files={}", bw_check.join(",")));
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

    let is_closing_to_complete = matches!(classify_operation(verb, body), "complete")
        || (verb == "transition" && body.get("to").and_then(|v| v.as_str()) == Some("COMPLETE"));
    if is_closing_to_complete {
        let (body_s, _err, code) = crate::orchestrator::prd::handle_list("");
        if code == 0 {
            if let Ok(v) = serde_json::from_str::<Value>(&body_s) {
                if let Some(items) = v.get("items").and_then(|v| v.as_array()) {
                    for it in items {
                        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                        let is_open = status == "pending" || status == "in_progress";
                        if !is_open { continue; }
                        let witness = it.get("witness_evidence").and_then(|v| v.as_str()).unwrap_or("");
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

#[cfg(test)]
mod long_gap_tests {
    use super::{long_gap_should_fire, is_longgap_exempt};
    const T: u64 = 300_000;

    #[test]
    fn exempt_set_covers_internal_and_pause_verbs() {
        for v in ["health", "auto-recall", "wait", "sleep"] {
            assert!(is_longgap_exempt(v), "{} must be long-gap-exempt", v);
        }
        for v in ["browser", "codesearch", "exec_js", "instruction", "git_push"] {
            assert!(!is_longgap_exempt(v), "{} must NOT be long-gap-exempt", v);
        }
    }

    #[test]
    fn fires_on_genuine_idle() {
        // >threshold since instruction AND >threshold since any dispatch (or none).
        let now = 1_000_000;
        assert!(long_gap_should_fire(now - T - 1, now - T - 1, now, T));
        assert!(long_gap_should_fire(now - T - 1, 0, now, T)); // boot/overnight: no prior dispatch
    }

    #[test]
    fn quiet_during_active_work() {
        // Stale instruction (>threshold) but a recent work dispatch keeps the chain alive.
        let now = 1_000_000;
        assert!(!long_gap_should_fire(now - T - 1, now - 1_000, now, T));
        assert!(!long_gap_should_fire(now - T - 50_000, now - 200_000, now, T));
    }

    #[test]
    fn quiet_when_instruction_recent() {
        let now = 1_000_000;
        assert!(!long_gap_should_fire(now - 1_000, 0, now, T));
    }

    #[test]
    fn quiet_when_no_instruction_yet() {
        let now = 1_000_000;
        assert!(!long_gap_should_fire(0, 0, now, T));
    }

    #[test]
    fn exempt_set_covers_pause_and_internal_verbs() {
        use super::is_longgap_exempt;
        // Internal pulses and deliberate-pause poll verbs never trip long-gap.
        for v in ["health", "auto-recall", "wait", "sleep"] {
            assert!(is_longgap_exempt(v), "{} must be long-gap-exempt", v);
        }
        // Real work/orchestrator verbs are NOT exempt.
        for v in ["browser", "exec_js", "codesearch", "instruction", "prd-add", "git_push"] {
            assert!(!is_longgap_exempt(v), "{} must NOT be exempt", v);
        }
    }
}
