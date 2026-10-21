#![cfg(target_arch = "wasm32")]

use crate::wasm_dispatch::{host_log, host_read, host_write};
use serde_json::{json, Value};

pub const GATE_LONG_GAP_NO_INSTRUCTION_DEFAULT: &str = "long-gap-no-instruction: {gap_ms}ms since last `instruction` dispatch (threshold {threshold_ms}ms). Idle mid-chain is a deviation. Dispatch `instruction` for recovery prose before any other verb.";

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
    pub gate_id: Option<String>,
    pub predicate: Option<String>,
}

impl GateVerdict {
    fn allow() -> Self {
        Self {
            allowed: true,
            reason: None,
            await_result: false,
            pending_step_id: None,
            pending_step_full: None,
            residuals: vec![],
            next_dispatch: None,
            gate_id: None,
            predicate: None,
        }
    }
    fn deny(reason: String) -> Self {
        Self {
            allowed: false,
            reason: Some(reason),
            await_result: false,
            pending_step_id: None,
            pending_step_full: None,
            residuals: vec![],
            next_dispatch: None,
            gate_id: None,
            predicate: None,
        }
    }
    fn with_next(mut self, next: &str) -> Self {
        self.next_dispatch = Some(next.to_string());
        self
    }
    fn gated(mut self, gate_id: &str, predicate: Option<&str>) -> Self {
        self.gate_id = Some(gate_id.to_string());
        self.predicate = predicate.map(str::to_string);
        self
    }
    pub fn to_denial_json(&self, verb: &str) -> Value {
        let next: &str = self
            .next_dispatch
            .as_deref()
            .unwrap_or(if self.await_result {
                "memorize-continue"
            } else {
                "instruction"
            });
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
            "error": reason_with_hint,
            "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
            "next_dispatch": next,
            "next_dispatch_hint": next,
            "gate_id": self.gate_id,
            "predicate": self.predicate,
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
    if s.is_empty() {
        return (String::new(), 0, 0);
    }
    let mut parts = s.splitn(3, '|');
    let verb = parts.next().unwrap_or("").to_string();
    let count = parts
        .next()
        .and_then(|c| c.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let ts = parts
        .next()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0);
    (verb, count, ts)
}

const LONGGAP_GATED_STATE_CHANGING_VERBS: &[&str] = &[
    "fs_write", "fs_edit", "kv_put", "cache_put", "cache_invalidate",
    "sql_open", "sql_close", "sql_exec", "sql_serialize", "sql_deserialize", "sql_smoke",
    "codeinsight_index",
    "config-sync-now", "discipline", "close",
    "git_add", "git_commit", "git_amend", "git_finalize", "git_push", "git_fetch", "git_pull", "git_rewrite_authors",
    "git_worktree", "git_checkout", "git_merge", "git_merge_abort", "git_branch_delete", "git_rm",
    "git_revert", "git_reset", "git_cherry_pick", "git_stash", "git_stash_pop", "git_stash_drop",
    "git_worktree_add", "git_worktree_remove", "git_worktree_prune",
];

const LONGGAP_EXEMPT_WORK_VERBS: &[&str] = &[
    "exec_js", "nodejs", "javascript", "node", "js", "typescript", "bash", "sh", "shell", "zsh",
    "python", "py", "powershell", "ps1", "go", "rust", "c", "cpp", "java", "deno",
    "git_add", "git_commit", "git_amend", "git_fetch", "git_push", "git_finalize",
];

fn long_gap_chain_belongs_to_this_session(policy: &crate::orchestrator::fsm::Policy) -> bool {
    if crate::orchestrator::state::dispatch_session_id()
        .is_some_and(|caller| caller.contains("-sub"))
    {
        return false;
    }
    let state = crate::orchestrator::state::read_state();
    if state.phase.as_str() == policy.terminal_phase {
        return false;
    }
    match (
        &state.session_id,
        crate::orchestrator::state::dispatch_session_id(),
    ) {
        (Some(owner), Some(caller)) => *owner == caller,
        _ => true,
    }
}

fn is_longgap_activity_exempt(verb: &str, policy: &crate::orchestrator::fsm::Policy) -> bool {
    policy.longgap_exempt_verbs.iter().any(|v| v == verb)
}

fn is_longgap_denial_exempt(verb: &str, policy: &crate::orchestrator::fsm::Policy) -> bool {
    if is_longgap_activity_exempt(verb, policy) {
        return true;
    }
    if LONGGAP_EXEMPT_WORK_VERBS.contains(&verb) {
        return true;
    }
    !LONGGAP_GATED_STATE_CHANGING_VERBS.contains(&verb)
        && !crate::orchestrator::is_orchestrator_verb(verb)
}

fn is_longgap_refresh(verb: &str, policy: &crate::orchestrator::fsm::Policy) -> bool {
    policy.longgap_refresh_verbs.iter().any(|v| v == verb)
}

pub fn dispatch_serves_no_phase_prose(verb: &str, body: &Value) -> bool {
    verb == "dream-replay-cycle"
        || (verb == "instruction"
            && crate::orchestrator::instructions::body_requests_investigate_readonly(body))
}

fn dispatch_submitted_at_ms() -> Option<u64> {
    let key = "AGENTPLUG_DISPATCH_SUBMITTED_AT_MS";
    let packed = unsafe { crate::wasm_dispatch::host_env_get(key.as_ptr(), key.len() as u32) };
    crate::wasm_dispatch::unpack_to_string_pub(packed)?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|ms| *ms > 0)
}

pub fn restamp_last_dispatch_to_completion(verb: &str) {
    let policy = crate::orchestrator::fsm::graph().policy.clone();
    if is_longgap_activity_exempt(verb, &policy) {
        return;
    }
    let _ = crate::wasm_dispatch::host_write(
        &crate::pkfs::anchor(".gm/last-dispatch-ts"),
        &now_ms().to_string(),
    );
}

fn long_gap_should_fire(
    last_instruction_ms: u64,
    prev_dispatch_ms: u64,
    now: u64,
    threshold: u64,
) -> bool {
    if last_instruction_ms == 0 {
        return false;
    }
    let idle_since_instruction = now.saturating_sub(last_instruction_ms) > threshold;
    let idle_since_any = prev_dispatch_ms == 0 || now.saturating_sub(prev_dispatch_ms) > threshold;
    idle_since_instruction && idle_since_any
}

fn log_deviation(event: &str, detail: &str) {
    let msg = format!("plugkit gate: {} {}", event, detail);
    unsafe {
        host_log(2, msg.as_ptr(), msg.len() as u32);
    }
    let registered = crate::orchestrator::deviations::kind_is_known(event);
    let severity = crate::orchestrator::deviations::effective_severity(event);
    let mut payload = json!({
        "event": format!("deviation.{}", event),
        "sub": "hook",
        "detail": detail,
        "kind": event,
        "severity": severity.as_str(),
        "ts": now_ms(),
        "source": "rs-plugkit/gates",
    });
    if !registered {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("unregistered_kind".to_string(), json!(true));
        }
    }
    let evt_line = format!("evt: {}", payload);
    unsafe {
        host_log(1, evt_line.as_ptr(), evt_line.len() as u32);
    }
}

fn turn_state_value() -> Result<Option<Value>, String> {
    let path = crate::pkfs::anchor(".gm/turn-state.json");
    let content = host_read(&path).unwrap_or_default();
    if content.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<Value>(&content)
        .map(Some)
        .map_err(|e| format!("turn-state-corrupt: {} is not valid JSON ({})", path, e))
}

fn parse_pending_step() -> Option<(String, u64)> {
    let v = turn_state_value().ok()??;
    let step_id = v
        .get("pending_step_id")
        .and_then(|s| s.as_str())?
        .to_string();
    if step_id.is_empty() {
        return None;
    }
    let deadline = v
        .get("pending_step_deadline_ms")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    if deadline > 0 && now_ms() > deadline {
        return None;
    }
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
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn current_phase_key() -> String {
    crate::orchestrator::state::read_state()
        .phase
        .as_str()
        .to_string()
}

fn is_transition_to_complete(verb: &str, body: &Value) -> bool {
    verb == "transition"
        && body
            .get("to")
            .and_then(|v| v.as_str())
            .map(|to| to.eq_ignore_ascii_case("complete") || to.eq_ignore_ascii_case("stop"))
            .unwrap_or(false)
}

fn fanout_slice_count(prose: &str) -> Option<u64> {
    let marker = "Fan-out:";
    let line = prose.lines().find(|l| l.contains(marker))?;
    let rest = &line[line.find(marker)? + marker.len()..];
    rest.split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
}

fn check_fanout_missed(to: &str, body: &Value) {
    let Some(parent) = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let prose = crate::prose::resolve(&to.to_ascii_lowercase(), "");
    if !fanout_slice_count(&prose).is_some_and(|n| n >= 2) {
        return;
    }
    let since_ms: u64 = host_read(&crate::pkfs::anchor(".gm/last-instruction-ts"))
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0);
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    match crate::dispatch_ledger::subagent_dispatches_since(&cwd, parent, since_ms) {
        Ok(0) => log_deviation(
            "fanout-missed",
            &format!("node={} parent_session={} since_ms={}", to, parent, since_ms),
        ),
        Ok(_) => {}
        Err(e) => log_deviation(
            "fanout-ledger-unreadable",
            &format!("node={} parent_session={} error={}", to, parent, e),
        ),
    }
}

fn concurrency_refusal_at_complete(body: &Value, open_rows: usize) -> Option<String> {
    let policy = crate::orchestrator::fsm::graph().policy.clone();
    let parent = body
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let now = unsafe { crate::wasm_dispatch::host_now_ms() };
    let running = match parent {
        None => 0,
        Some(p) => match crate::dispatch_ledger::subagent_running_count(
            &cwd,
            p,
            now,
            policy.concurrency_window_ms,
        ) {
            Ok(n) => n,
            Err(e) => {
                return Some(format!(
                    "concurrency-ledger-unreadable: the running subagent count cannot be read ({}); COMPLETE is refused until the dispatch ledger is repaired.",
                    e
                ))
            }
        },
    };
    let shortfall = open_rows.saturating_sub(running);
    if shortfall == 0 {
        return None;
    }
    Some(format!(
        "concurrency-shortfall: {} open slice(s) available, {} subagent(s) running, shortfall {}. Launch the available slices as subagents before COMPLETE; dispatch `instruction` for the slices to launch.",
        open_rows, running, shortfall
    ))
}

fn effective_severity_is_deny(kind: &str) -> bool {
    crate::orchestrator::deviations::effective_severity(kind)
        == crate::orchestrator::deviations::Severity::Deny
}

fn is_synthetic_test_path(rel: &str) -> bool {
    let norm = rel.replace('\\', "/").to_lowercase();
    let segments: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    let Some(file) = segments.last() else {
        return false;
    };
    if segments
        .iter()
        .rev()
        .skip(1)
        .any(|s| *s == "test" || *s == "tests" || *s == "__tests__" || *s == "spec")
    {
        return true;
    }
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
}

fn is_unsolicited_toplevel_doc(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    if norm.contains('/') {
        return false;
    }
    let lower_ext_is_doc =
        norm.to_lowercase().ends_with(".md") || norm.to_lowercase().ends_with(".txt");
    if !lower_ext_is_doc {
        return false;
    }
    !crate::orchestrator::fsm::graph()
        .policy
        .toplevel_doc_allowlist
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&norm))
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
                if depth > 0 {
                    j += 1;
                }
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
    let policy = crate::orchestrator::fsm::graph().policy.clone();
    if let Some(v) = await_gate(verb, &policy) {
        return v;
    }
    if let Some(v) = shell_git_gate(verb, body, &policy) {
        return v;
    }
    if let Some(v) = native_search_gate(verb, body, &policy) {
        return v;
    }
    policy_gate(verb, body, &policy)
}

fn await_gate(verb: &str, policy: &crate::orchestrator::fsm::Policy) -> Option<GateVerdict> {
    if let Err(detail) = turn_state_value() {
        if policy.await_allowed_verbs.iter().any(|v| v == verb) {
            return None;
        }
        log_deviation("turn-state-corrupt", &format!("verb={} {}", verb, detail));
        return Some(GateVerdict::deny(format!(
            "{}; restore the file from its .corrupt-<ts> backup or repair it before dispatching {}",
            detail, verb
        ))
        .gated("deviation.turn-state-corrupt", Some("turn-state-corrupt")));
    }
    if let Some(step_id) = read_pending_step() {
        if !policy.await_allowed_verbs.iter().any(|v| v == verb) {
            log_deviation(
                "await-result-violation",
                &format!("verb={} step={}", verb, step_id),
            );
            let mut v = GateVerdict::deny(format!(
                "pipeline suspended at step_id={}; only memorize-continue advances state. \
                 The full pending_step recovery payload is embedded in this response as `pending_step_full` \
                 (no need to re-dispatch `instruction` first). Compute the step inline using \
                 `pending_step_full.state.original_body` and the prompt_template, then dispatch \
                 memorize-continue with body {{token, step_id, result}}. No other verb is valid until \
                 this completes.",
                step_id
            ))
            .gated("deviation.await-result-violation", Some("await-result-violation"));
            v.await_result = true;
            v.pending_step_id = Some(step_id.clone());
            v.pending_step_full = read_pending_step_full();
            return Some(v);
        }
    }
    None
}

const COMMAND_WRAPPERS: &[&str] = &[
    "env", "command", "exec", "time", "nohup", "sudo", "xargs", "nice", "builtin", "if", "then",
    "else", "elif", "do", "while", "until", "timeout", "stdbuf", "ionice", "setsid", "doas",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "powershell", "pwsh", "cmd"];

const MAX_UNWRAP_DEPTH: usize = 8;

fn wrapper_arg_flags(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "env" => &["-u", "--unset", "-C", "--chdir"],
        "sudo" => &["-u", "-g", "-h", "-p", "-r", "-t", "-U", "-C", "-D", "-T", "-R"],
        "doas" => &["-u", "-C"],
        "timeout" => &["-s", "--signal", "-k", "--kill-after"],
        "stdbuf" => &["-i", "-o", "-e"],
        "ionice" => &["-c", "-n", "-p", "-P", "-u"],
        "nice" => &["-n"],
        "time" => &["-f", "-o"],
        "exec" => &["-a"],
        "xargs" => &["-I", "-L", "-n", "-P", "-d", "-E", "-s", "-a"],
        _ => &[],
    }
}

fn wrapper_positionals(wrapper: &str) -> usize {
    usize::from(wrapper == "timeout")
}

fn is_wrapper_prefix(token: &str) -> bool {
    token.is_empty()
        || token.chars().all(|c| c.is_ascii_digit())
        || token.split_once('=').is_some_and(|(key, _)| {
            !key.is_empty()
                && !key.starts_with(|c: char| c.is_ascii_digit())
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn redirect_body(token: &str) -> &str {
    token.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&')
}

fn is_redirect(token: &str) -> bool {
    redirect_body(token).starts_with(['<', '>'])
}

fn is_bare_redirect(token: &str) -> bool {
    is_redirect(token) && redirect_body(token).trim_start_matches(['<', '>']).is_empty()
}

fn shell_name(token: &str) -> Option<String> {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token).to_ascii_lowercase();
    let name = base.strip_suffix(".exe").unwrap_or(&base);
    SHELLS.contains(&name).then(|| name.to_string())
}

fn shell_command_flag(shell: &str, flag: &str) -> bool {
    let f = flag.to_ascii_lowercase();
    match shell {
        "cmd" => f == "/c" || f == "/k",
        "powershell" | "pwsh" => f.len() > 1 && "-command".starts_with(f.as_str()),
        _ => {
            f == "-c"
                || (f.len() > 2
                    && !f.starts_with("--")
                    && f.starts_with('-')
                    && f[1..].chars().all(|c| c.is_ascii_alphabetic())
                    && f.contains('c'))
        }
    }
}

fn rest_of(tokens: &[&str], from: usize) -> String {
    tokens.get(from..).unwrap_or_default().join(" ")
}

const INTERPRETER_PREFIXES: &[&str] = &[
    "node", "deno", "bun", "python", "perl", "ruby", "php", "lua",
];

const INLINE_SCRIPT_FLAGS: &[&str] = &["-c", "-e", "-E", "-p", "-r", "--eval", "--print"];

fn is_interpreter(token: &str) -> bool {
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token).to_ascii_lowercase();
    let name = base.strip_suffix(".exe").unwrap_or(&base);
    INTERPRETER_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

fn is_inline_script_arg(before: &str) -> bool {
    let mut words = before.split_whitespace().rev();
    let flag_is_inline = words.next().is_some_and(|flag| INLINE_SCRIPT_FLAGS.contains(&flag));
    flag_is_inline && words.find(|word| !word.starts_with('-')).is_some_and(is_interpreter)
}

fn split_inline_scripts(text: &str) -> (String, Vec<(&str, char)>) {
    let mut code = String::with_capacity(text.len());
    let mut scripts = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find(['\'', '"']) {
        let quote = char::from(rest.as_bytes()[open]);
        let Some(len) = rest[open + 1..].find(quote) else {
            break;
        };
        let span_end = open + len + 2;
        let before = &rest[..open];
        code.push_str(before);
        if is_inline_script_arg(before) {
            scripts.push((&rest[open + 1..open + 1 + len], quote));
            code.push(' ');
        } else {
            code.push_str(&rest[open..span_end]);
        }
        rest = &rest[span_end..];
    }
    code.push_str(rest);
    (code, scripts)
}

fn contains_git_word(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    lower.match_indices("git").any(|(i, _)| {
        let before = i.checked_sub(1).is_some_and(|p| is_word(bytes[p]));
        let after = bytes.get(i + 3).is_some_and(|&b| is_word(b));
        !before && !after
    })
}

fn substitutions_invoke_git(
    text: &str,
    is_git_token: &dyn Fn(&str) -> bool,
    depth: usize,
) -> bool {
    extract_substitution_bodies(text)
        .into_iter()
        .any(|inner| text_invokes_git(&inner, is_git_token, depth + 1))
}

fn is_drive_path(token: &str) -> bool {
    let b = token.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let msys = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/';
    drive || msys
}

fn closing_quote(chars: &[char], open: usize) -> Option<usize> {
    let quote = chars[open];
    let mut j = open + 1;
    while j < chars.len() {
        if quote == '"' && chars[j] == '\\' && chars.get(j + 1) == Some(&'"') {
            j += 2;
            continue;
        }
        if chars[j] == quote {
            return Some(j);
        }
        j += 1;
    }
    None
}

fn push_word(segments: &mut Vec<Vec<String>>, word: &mut String, in_word: &mut bool) {
    if *in_word {
        if let Some(segment) = segments.last_mut() {
            segment.push(std::mem::take(word));
        }
    }
    word.clear();
    *in_word = false;
}

fn split_words(text: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut in_word = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            if let Some(end) = closing_quote(&chars, i) {
                let mut j = i + 1;
                while j < end {
                    if c == '"' && chars[j] == '\\' && chars.get(j + 1) == Some(&'"') {
                        j += 1;
                    }
                    word.push(chars[j]);
                    j += 1;
                }
                in_word = true;
                i = end + 1;
                continue;
            }
        }
        match c {
            ' ' | '\t' | '\r' => push_word(&mut segments, &mut word, &mut in_word),
            '\n' | ';' | '|' | '&' => {
                push_word(&mut segments, &mut word, &mut in_word);
                segments.push(Vec::new());
            }
            _ => {
                word.push(c);
                in_word = true;
            }
        }
        i += 1;
    }
    push_word(&mut segments, &mut word, &mut in_word);
    segments
}

fn variable_name(token: &str) -> Option<&str> {
    let name = token.strip_prefix('$')?.trim_start_matches('{').trim_end_matches('}');
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
}

fn assignments(segments: &[Vec<String>]) -> Vec<(String, String)> {
    segments
        .iter()
        .flatten()
        .filter_map(|word| {
            let (key, value) = word.split_once('=')?;
            let valid = !key.is_empty()
                && !key.starts_with(|c: char| c.is_ascii_digit())
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            valid.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

fn text_invokes_git(text: &str, is_git_token: &dyn Fn(&str) -> bool, depth: usize) -> bool {
    if depth > MAX_UNWRAP_DEPTH {
        return true;
    }
    let (code, scripts) = split_inline_scripts(text);
    let quoted_substitution = scripts.iter().any(|&(script, quote)| {
        quote == '"' && substitutions_invoke_git(script, is_git_token, depth)
    });
    let script_names_git = scripts.iter().any(|&(script, _)| contains_git_word(script));
    let segments = split_words(&code);
    let vars = assignments(&segments);
    quoted_substitution
        || script_names_git
        || segments
            .iter()
            .any(|segment| segment_invokes_git(segment, &vars, is_git_token, depth))
        || substitutions_invoke_git(&code, is_git_token, depth)
}

fn segment_invokes_git(
    segment: &[String],
    vars: &[(String, String)],
    is_git_token: &dyn Fn(&str) -> bool,
    depth: usize,
) -> bool {
    let tokens: Vec<&str> = segment
        .iter()
        .map(|t| t.trim_matches(|c: char| matches!(c, '(' | '{' | '!' | '"' | '\'')))
        .collect();
    let mut wrapper: &str = "";
    let mut positionals = 0usize;
    let mut i = 0usize;
    while i < tokens.len() {
        let t = tokens[i];
        i += 1;
        if is_redirect(t) {
            if is_bare_redirect(t) {
                i += 1;
            }
            continue;
        }
        if positionals > 0 && !t.starts_with('-') {
            positionals -= 1;
            continue;
        }
        if t == "eval" {
            return text_invokes_git(&rest_of(&tokens, i), is_git_token, depth + 1);
        }
        if let Some(shell) = shell_name(t) {
            return tokens
                .get(i..)
                .unwrap_or_default()
                .iter()
                .position(|flag| shell_command_flag(&shell, flag))
                .is_some_and(|k| {
                    text_invokes_git(&rest_of(&tokens, i + k + 1), is_git_token, depth + 1)
                });
        }
        if COMMAND_WRAPPERS.contains(&t) {
            wrapper = t;
            positionals = wrapper_positionals(t);
            continue;
        }
        if t.starts_with('-') {
            if wrapper == "env" && matches!(t, "-S" | "--split-string") {
                return text_invokes_git(&rest_of(&tokens, i), is_git_token, depth + 1);
            }
            if wrapper_arg_flags(wrapper).contains(&t) {
                i += 1;
            }
            continue;
        }
        if is_wrapper_prefix(t) {
            continue;
        }
        if t.starts_with("$(") || t.starts_with('`') {
            return true;
        }
        if let Some(name) = variable_name(t) {
            return vars.iter().any(|(key, value)| {
                key.as_str() == name && text_invokes_git(value, is_git_token, depth + 1)
            });
        }
        if is_drive_path(t)
            && (i - 1..tokens.len()).any(|j| is_git_token(tokens[i - 1..=j].join(" ").as_str()))
        {
            return true;
        }
        return is_git_token(t);
    }
    false
}

fn shell_git_gate(
    verb: &str,
    body: &Value,
    policy: &crate::orchestrator::fsm::Policy,
) -> Option<GateVerdict> {
    let lang_is_shell = verb == "lang"
        && body
            .get("lang")
            .and_then(|v| v.as_str())
            .is_some_and(|lang| policy.shell_verbs.iter().any(|v| v == lang));
    let shell_verb = policy.shell_verbs.iter().any(|v| v == verb) || lang_is_shell;
    if policy.deny_shell_git && shell_verb {
        let cmd = body
            .get("command")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("code").and_then(|v| v.as_str()))
            .or_else(|| body.get("script").and_then(|v| v.as_str()))
            .unwrap_or("");
        let is_git_token = |first: &str| {
            let base = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
            let stem = [".exe", ".cmd", ".bat"]
                .iter()
                .find_map(|ext| base.strip_suffix(*ext))
                .unwrap_or(base.as_str());
            stem == "git"
        };
        if text_invokes_git(cmd, &is_git_token, 0) {
            log_deviation(
                "bash-git-bypass",
                &format!(
                    "verb={} cmd={}",
                    verb,
                    cmd.chars().take(80).collect::<String>()
                ),
            );
            return Some(GateVerdict::deny(format!(
                "bash-git-bypass:a `{}` verb invoking `git` is denied - git is a spool surface, not a shell command. Use the git verb for the intent; the substitution table in the Spool ABI section of the instruction reply maps each one. git_finalize {{message}} bundles add, commit and push in one dispatch. Command was: `{}`",
                verb, cmd.chars().take(120).collect::<String>()
            ))
            .with_next("git_finalize")
            .gated("deviation.bash-git-bypass", Some("bash-git-bypass")));
        }
    }
    None
}

fn native_search_gate(
    verb: &str,
    body: &Value,
    policy: &crate::orchestrator::fsm::Policy,
) -> Option<GateVerdict> {
    if !policy.deny_native_search || !policy.shell_verbs.iter().any(|v| v == verb) {
        return None;
    }
    let cmd = body
        .get("command")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("code").and_then(|v| v.as_str()))
        .or_else(|| body.get("script").and_then(|v| v.as_str()))
        .unwrap_or("");
    let is_native_search = |first: &str| {
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
        matches!(base, "find" | "find.exe" | "grep" | "grep.exe" | "rg" | "rg.exe")
    };
    let native_in_segments = |text: &str| {
        text.split([';', '\n', '|', '&'])
            .map(|s| s.trim_start())
            .any(|s| is_native_search(s.split_whitespace().next().unwrap_or("")))
    };
    let native_in_subshell = extract_substitution_bodies(cmd)
        .into_iter()
        .any(|inner| native_in_segments(&inner));
    if native_in_segments(cmd) || native_in_subshell {
        log_deviation(
            "native-search-bypass",
            &format!(
                "verb={} cmd={}",
                verb,
                cmd.chars().take(80).collect::<String>()
            ),
        );
        return Some(GateVerdict::deny(format!(
            "native-search-bypass:a `{}` verb invoking `find`, `grep` or `rg` is denied - codesearch is the code search surface. Use the codesearch verb instead with body {{\"query\":\"<term>\",\"mode\":\"literal\",\"path\":\"<dir>\"}} (mode \"regex\" for patterns; add \"case_insensitive\":true or \"whole_word\":true as needed; \"path\" or \"glob\" narrows scope). Command was: `{}`",
            verb,
            cmd.chars().take(120).collect::<String>()
        ))
        .with_next("codesearch")
        .gated("deviation.native-search-bypass", Some("native-search-bypass")));
    }
    None
}

fn failing_transition_gate(
    from: &str,
    to: &str,
    residuals: &[String],
) -> (Option<String>, Option<String>) {
    let graph = crate::orchestrator::fsm::graph();
    let Some(edge) = graph.transition_edge(from, to) else {
        return (None, None);
    };
    let first = residuals.first().map(String::as_str).unwrap_or_default();
    for name in &edge.gates {
        let Some(gate) = graph.gate(name) else {
            continue;
        };
        let detailed = format!("{} -- ", gate.message);
        let matched = first == gate.message.as_str() || first.starts_with(detailed.as_str());
        if !gate.advisory && matched {
            return (Some(gate.name.clone()), gate.predicate.clone());
        }
    }
    (None, None)
}

fn policy_gate(
    verb: &str,
    body: &Value,
    policy: &crate::orchestrator::fsm::Policy,
) -> GateVerdict {
    let claim_ms = now_ms();
    let submitted_at_ms = dispatch_submitted_at_ms()
        .map(|ms| ms.min(claim_ms))
        .unwrap_or(claim_ms);

    let serves_no_phase_prose = dispatch_serves_no_phase_prose(verb, body);
    let prev_dispatch_ms: u64 =
        if !is_longgap_activity_exempt(verb, &policy) && !serves_no_phase_prose {
            let p = host_read(&crate::pkfs::anchor(".gm/last-dispatch-ts"))
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0);
            let _ = crate::wasm_dispatch::host_write(
                &crate::pkfs::anchor(".gm/last-dispatch-ts"),
                &claim_ms.to_string(),
            );
            p
        } else {
            0
        };

    if is_longgap_refresh(verb, &policy) && !serves_no_phase_prose {
        let _ = crate::wasm_dispatch::host_write(
            &crate::pkfs::anchor(".gm/last-instruction-ts"),
            &claim_ms.to_string(),
        );
        let _ =
            crate::wasm_dispatch::host_write(&crate::pkfs::anchor(".gm/long-gap-retry-state"), "");
    } else if !is_longgap_denial_exempt(verb, &policy) && !serves_no_phase_prose {
        let last = host_read(&crate::pkfs::anchor(".gm/last-instruction-ts")).unwrap_or_default();
        let last_ms: u64 = last.trim().parse().unwrap_or(0);
        let longgap_threshold_ms = policy.longgap_threshold_ms;
        if long_gap_should_fire(
            last_ms,
            prev_dispatch_ms,
            submitted_at_ms,
            longgap_threshold_ms,
        ) && long_gap_chain_belongs_to_this_session(&policy)
        {
            let gap_ms = submitted_at_ms.saturating_sub(last_ms);
            let retry_state =
                host_read(&crate::pkfs::anchor(".gm/long-gap-retry-state")).unwrap_or_default();
            let (last_verb, count, last_denial_ts) = parse_retry_state_v2(&retry_state);
            let since_last_denial = submitted_at_ms.saturating_sub(last_denial_ts);
            let same_burst =
                last_denial_ts > 0 && since_last_denial <= policy.long_gap_same_burst_ms;
            let new_count =
                if last_verb == verb && since_last_denial > policy.long_gap_same_burst_ms {
                    count + 1
                } else if last_verb == verb {
                    count
                } else {
                    1u32
                };
            let _ = crate::wasm_dispatch::host_write(
                &crate::pkfs::anchor(".gm/long-gap-retry-state"),
                &format!("{}|{}|{}", verb, new_count, claim_ms),
            );
            if new_count >= policy.long_gap_retry_escalate_after {
                if !same_burst {
                    log_deviation(
                        "long-gap-retry-without-instruction",
                        &format!(
                            "verb={} consecutive_retries={} gap_ms={}",
                            verb, new_count, gap_ms
                        ),
                    );
                }
                return GateVerdict::deny(format!(
                    "long-gap-retry-without-instruction: verb=`{}` denied {}x in a row by long-gap-no-instruction gate, yet the agent retried instead of dispatching `instruction`. The gate's `next_dispatch` field names the recovery verb - when it says `instruction`, the next verb IS `instruction`, not the same verb again. Dispatch `instruction` now; the chain cannot recover by re-attempting the denied verb.",
                    verb, new_count
                ))
                .gated(
                    "deviation.long-gap-retry-without-instruction",
                    Some("long-gap-retry-without-instruction"),
                );
            }
            if !same_burst {
                log_deviation(
                    "long-gap-no-instruction",
                    &format!("verb={} gap_ms={}", verb, gap_ms),
                );
            }
            return GateVerdict::deny(crate::prose::fill_placeholders(
                "gates/long-gap-no-instruction",
                &crate::prose::resolve_and_mark(
                    "gates/long-gap-no-instruction",
                    GATE_LONG_GAP_NO_INSTRUCTION_DEFAULT,
                ),
                &[
                    ("gap_ms", gap_ms.to_string()),
                    ("threshold_ms", longgap_threshold_ms.to_string()),
                ],
            ))
            .gated("deviation.long-gap-no-instruction", Some("long-gap-no-instruction"));
        }
    }

    let is_complete_transition = is_transition_to_complete(verb, body);

    let requested_to_phase = if verb == "transition" {
        body.get("to").and_then(|v| v.as_str()).map(|s| {
            if s.eq_ignore_ascii_case("stop") {
                policy.terminal_phase.clone()
            } else {
                s.to_ascii_uppercase()
            }
        })
    } else {
        None
    };

    if is_complete_transition {
        if let Some(to) = requested_to_phase.as_deref() {
            check_fanout_missed(to, body);
        }
    }

    if let Some(to) = requested_to_phase {
        let from = current_phase_key();
        let to = to.as_str();
        let (residuals, next_recovery) =
            crate::orchestrator::transitions::gate_residuals(&from, to);
        if !residuals.is_empty() {
            let gate_key = to.to_ascii_lowercase();
            log_deviation(
                "gate-deny",
                &format!("{}-gate residuals={}", gate_key, residuals.len()),
            );
            let repeat_count = record_gate_repeat(&gate_key, "gate-deny");
            let label = if to == policy.terminal_phase {
                "stop-gate".to_string()
            } else {
                format!("{}-gate", to.to_ascii_lowercase())
            };
            let mut reason = format!("{} residuals: {}", label, residuals.join("; "));
            if repeat_count >= policy.gate_repeat_escalate_threshold {
                log_deviation(
                    "stuck-loop-escalation",
                    &format!("gate={} repeat_count={}", gate_key, repeat_count),
                );
                log_deviation(
                    "self-reconfig-candidate",
                    &format!("gate={} repeat_count={}", gate_key, repeat_count),
                );
                reason = format!(
                    "{} -- STUCK LOOP DETECTED: this exact gate denial has now fired {} times in a row with no successful transition between attempts. Retrying the bare transition again will repeat the same denial. Stop retrying: (1) `prd-add` a row describing the concrete stuck state (which residual, what you tried, why it did not clear), (2) invoke the wfgy-method skill's BBCR bounded-retry-then-surface discipline to recover instead of blind-retrying, (3) only then re-attempt the transition.",
                    reason, repeat_count
                );
            }
            let (failing_gate, failing_predicate) =
                failing_transition_gate(&from, to, &residuals);
            let mut v = GateVerdict::deny(reason).gated(
                failing_gate.as_deref().unwrap_or(label.as_str()),
                failing_predicate.as_deref(),
            );
            v.residuals = residuals;
            v.next_dispatch = next_recovery;
            return v;
        }
        clear_gate_repeats(&to.to_ascii_lowercase());
    }

    if verb == "fs_write" {
        if let Some(p) = body_path_field(body) {
            if is_unsolicited_toplevel_doc(&p) {
                log_deviation("unsolicited-doc-created", &p);
                if effective_severity_is_deny("unsolicited-doc-created") {
                    return GateVerdict::deny(format!(
                        "unsolicited-doc-created: `{}` is a top-level doc outside policy.toplevel_doc_allowlist, and this project's policy.deviation_severity promotes this kind to deny. A report/summary/findings file is not the deliverable -- return the finding in the response, or write it where the work lives.",
                        p
                    ))
                    .gated("deviation.unsolicited-doc-created", Some("unsolicited-doc-created"));
                }
            }
        }
        if let Some(p) = body_path_field(body) {
            if is_synthetic_test_path(&p) {
                log_deviation("synthetic-test-file", &p);
                if effective_severity_is_deny("synthetic-test-file") {
                    return GateVerdict::deny(format!(
                        "synthetic-test-file: `{}` is a standing test file, and this project's policy.deviation_severity promotes this kind to deny. Doctrine is a live exec_js witness run THIS turn, not a test case deferred to a later run.",
                        p
                    ))
                    .gated("deviation.synthetic-test-file", Some("no-synthetic-test-files"));
                }
            }
        }
    }

    if is_complete_transition {
        let (body_s, _err, code) = crate::orchestrator::prd::handle_list_full();
        let mut anti_shape: Vec<String> = Vec::new();
        let mut open_rows = 0usize;
        if code == 0 {
            if let Ok(v) = serde_json::from_str::<Value>(&body_s) {
                if let Some(items) = v.get("items").and_then(|v| v.as_array()) {
                    for it in items {
                        let status = it
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("pending");
                        if crate::orchestrator::prd::status_is_open(status) {
                            open_rows += 1;
                            continue;
                        }
                        let witness = it
                            .get("witness_evidence")
                            .or_else(|| it.get("witness"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if witness.trim().is_empty() {
                            let id = it.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            log_deviation(
                                "prd-anti-shape",
                                &format!(
                                    "id={} status={} no witness_evidence on closing transition",
                                    id, status
                                ),
                            );
                            anti_shape.push(id.to_string());
                        }
                    }
                }
            }
        }
        if open_rows > 0 {
            if let Some(reason) = concurrency_refusal_at_complete(body, open_rows) {
                return GateVerdict::deny(reason)
                    .with_next("instruction")
                    .gated("concurrency-shortfall", Some("concurrency-shortfall"));
            }
        }
        if !anti_shape.is_empty() && effective_severity_is_deny("prd-anti-shape") {
            return GateVerdict::deny(format!(
                "prd-anti-shape: {} row(s) are marked closed with empty witness_evidence ({}), and this project's policy.deviation_severity promotes this kind to deny. Re-resolve each with its own distinct witness_evidence before closing the chain.",
                anti_shape.len(),
                anti_shape.join(", ")
            ))
            .with_next("prd-resolve")
            .gated("deviation.prd-anti-shape", Some("prd-anti-shape"));
        }
    }

    GateVerdict::allow()
}
