use super::gm_dir;
#[cfg(target_arch = "wasm32")]
use crate::wasm_dispatch::host_abi::{host_cas_write, host_random_fill, host_read};
#[cfg(target_arch = "wasm32")]
use crate::wasm_dispatch::host_now_ms;
use serde_json::{json, Value};
use std::collections::HashMap;

const LEDGER_FILE: &str = ".subagent-ledger.jsonl";
const END_STATUSES: &[&str] = &["done", "failed", "stopped"];
#[cfg(target_arch = "wasm32")]
const LEDGER_CAS_MAX_ATTEMPTS: u32 = 8;
#[cfg(target_arch = "wasm32")]
const LEDGER_BACKOFF_BASE_MS: u64 = 10;
#[cfg(target_arch = "wasm32")]
const LEDGER_BACKOFF_CAP_MS: u64 = 200;
#[cfg(target_arch = "wasm32")]
const LEDGER_JITTER_MAX_MS: u64 = 10;
#[cfg(target_arch = "wasm32")]
const LEDGER_RETENTION_MS: u64 = 60 * 60 * 1000;
#[cfg(target_arch = "wasm32")]
const CAS_WRITTEN: u32 = 1;
#[cfg(target_arch = "wasm32")]
const CAS_CONFLICT: u32 = 2;

fn reject(error: &str) -> (String, String, i32) {
    (json!({ "ok": false, "error": error }).to_string(), error.to_string(), 1)
}

fn accept(data: Value) -> (String, String, i32) {
    (data.to_string(), String::new(), 0)
}

fn body_str<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or("")
}

fn row_ts(row: &Value) -> u64 {
    row.get("ts").and_then(Value::as_u64).unwrap_or(0)
}

fn is_valid_subagent_id(parent: &str, subagent: &str) -> bool {
    subagent.starts_with(&format!("{parent}-")) && subagent.contains("-sub")
}

fn ledger_path(cwd: &str) -> String {
    if cwd.is_empty() {
        gm_dir()
            .join("exec-spool")
            .join(LEDGER_FILE)
            .to_string_lossy()
            .into_owned()
    } else {
        format!(
            "{}/.gm/exec-spool/{LEDGER_FILE}",
            cwd.trim_end_matches(['/', '\\'])
        )
    }
}

fn parse_rows(raw: &str) -> Vec<Value> {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn read_ledger(path: &str) -> String {
    host_read(path).unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_ledger(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

pub fn lifecycle_running_count(cwd: &str, parent: &str, now_ms: u64, window_ms: u64) -> usize {
    let since_ms = now_ms.saturating_sub(window_ms);
    let mut latest: HashMap<String, (String, u64)> = HashMap::new();
    for row in parse_rows(&read_ledger(&ledger_path(cwd))) {
        let ts = row_ts(&row);
        if body_str(&row, "parent") != parent || ts < since_ms || ts > now_ms {
            continue;
        }
        latest.insert(
            body_str(&row, "subagent").to_string(),
            (body_str(&row, "kind").to_string(), ts),
        );
    }
    latest
        .values()
        .filter(|(kind, _)| kind.as_str() == "start")
        .count()
}

#[cfg(target_arch = "wasm32")]
enum Failure {
    Rejected(String),
    Contention,
    WriteFailed,
}

#[cfg(target_arch = "wasm32")]
fn failure_response(failure: Failure) -> (String, String, i32) {
    match failure {
        Failure::Rejected(msg) => reject(&msg),
        Failure::Contention => {
            let body = json!({
                "ok": false,
                "error": "ledger_contention",
                "attempts": LEDGER_CAS_MAX_ATTEMPTS,
            });
            (body.to_string(), "ledger_contention".to_string(), 1)
        }
        Failure::WriteFailed => reject("ledger_write_failed"),
    }
}

#[cfg(target_arch = "wasm32")]
fn compact_lines(raw: &str, now_ms: u64) -> String {
    let mut latest: HashMap<String, (String, u64)> = HashMap::new();
    for row in parse_rows(raw) {
        latest.insert(
            body_str(&row, "subagent").to_string(),
            (body_str(&row, "kind").to_string(), row_ts(&row)),
        );
    }
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !row.is_object() {
            continue;
        }
        let expired = latest
            .get(body_str(&row, "subagent"))
            .is_some_and(|(kind, ts)| {
                kind.as_str() == "end" && ts.saturating_add(LEDGER_RETENTION_MS) < now_ms
            });
        if !expired {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
fn jitter_ms() -> u64 {
    let mut buf = [0u8; 4];
    unsafe { host_random_fill(buf.as_mut_ptr(), buf.len() as u32) };
    u64::from(u32::from_le_bytes(buf)) % (LEDGER_JITTER_MAX_MS + 1)
}

#[cfg(target_arch = "wasm32")]
fn backoff_ms(attempt: u32) -> u64 {
    let doubled = LEDGER_BACKOFF_BASE_MS << (attempt - 1).min(5);
    (doubled.min(LEDGER_BACKOFF_CAP_MS) + jitter_ms()).min(LEDGER_BACKOFF_CAP_MS)
}

#[cfg(target_arch = "wasm32")]
fn pause_ms(ms: u64) {
    let until = unsafe { host_now_ms() } + ms;
    while unsafe { host_now_ms() } < until {}
}

#[cfg(target_arch = "wasm32")]
fn append_event<F>(path: &str, now_ms: u64, plan: F) -> Result<bool, Failure>
where
    F: Fn(&str) -> Result<Option<Value>, String>,
{
    for attempt in 0..LEDGER_CAS_MAX_ATTEMPTS {
        if attempt > 0 {
            pause_ms(backoff_ms(attempt));
        }
        let old = read_ledger(path);
        let event = match plan(&old) {
            Err(msg) => return Err(Failure::Rejected(msg)),
            Ok(None) => return Ok(false),
            Ok(Some(event)) => event,
        };
        let mut next = compact_lines(&old, now_ms);
        next.push_str(&event.to_string());
        next.push('\n');
        match host_cas_write(path, &old, &next) {
            CAS_WRITTEN => return Ok(true),
            CAS_CONFLICT => continue,
            _ => return Err(Failure::WriteFailed),
        }
    }
    Err(Failure::Contention)
}

#[cfg(target_arch = "wasm32")]
fn run_start(cwd: &str, parent: &str, subagent: &str, slice_id: &str) -> (String, String, i32) {
    let now = unsafe { host_now_ms() };
    let event = json!({
        "kind": "start",
        "parent": parent,
        "subagent": subagent,
        "slice_id": slice_id,
        "status": Value::Null,
        "ts": now,
    });
    match append_event(&ledger_path(cwd), now, |_| Ok(Some(event.clone()))) {
        Ok(_) => {
            crate::dispatch_ledger::record(cwd, "subagent-start", slice_id, 0, Some(subagent), None);
            accept(json!({ "ok": true, "subagent_session_id": subagent, "ts": now }))
        }
        Err(failure) => failure_response(failure),
    }
}

#[cfg(target_arch = "wasm32")]
fn run_end(cwd: &str, parent: &str, subagent: &str, status: &str) -> (String, String, i32) {
    let now = unsafe { host_now_ms() };
    let plan = |raw: &str| -> Result<Option<Value>, String> {
        let rows = parse_rows(raw);
        let ended = rows
            .iter()
            .any(|r| body_str(r, "kind") == "end" && body_str(r, "subagent") == subagent);
        if ended {
            return Ok(None);
        }
        let open = rows.iter().any(|r| {
            body_str(r, "kind") == "start"
                && body_str(r, "subagent") == subagent
                && body_str(r, "parent") == parent
        });
        if !open {
            return Err("no open subagent".to_string());
        }
        Ok(Some(json!({
            "kind": "end",
            "parent": parent,
            "subagent": subagent,
            "status": status,
            "ts": now,
        })))
    };
    match append_event(&ledger_path(cwd), now, plan) {
        Ok(true) => accept(json!({ "ok": true, "subagent_session_id": subagent, "ts": now })),
        Ok(false) => accept(json!({
            "ok": true,
            "already_ended": true,
            "subagent_session_id": subagent,
            "ts": now,
        })),
        Err(failure) => failure_response(failure),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_start(_cwd: &str, _parent: &str, _subagent: &str, _slice_id: &str) -> (String, String, i32) {
    reject("subagent lifecycle requires the wasm32 host")
}

#[cfg(not(target_arch = "wasm32"))]
fn run_end(_cwd: &str, _parent: &str, _subagent: &str, _status: &str) -> (String, String, i32) {
    reject("subagent lifecycle requires the wasm32 host")
}

pub fn handle_start(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => return reject(&format!("invalid JSON: {e}")),
    };
    let parent = body_str(&body, "parent_session_id");
    let subagent = body_str(&body, "subagent_session_id");
    if parent.is_empty() {
        return reject("parent_session_id required");
    }
    if !is_valid_subagent_id(parent, subagent) {
        return reject("invalid subagent session id");
    }
    run_start(
        body_str(&body, "cwd"),
        parent,
        subagent,
        body_str(&body, "slice_id"),
    )
}

pub fn handle_end(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => return reject(&format!("invalid JSON: {e}")),
    };
    let parent = body_str(&body, "parent_session_id");
    let subagent = body_str(&body, "subagent_session_id");
    if parent.is_empty() {
        return reject("parent_session_id required");
    }
    if subagent.is_empty() {
        return reject("subagent_session_id required");
    }
    let status = body.get("status").and_then(Value::as_str).unwrap_or("done");
    if !END_STATUSES.contains(&status) {
        return reject("invalid status");
    }
    run_end(body_str(&body, "cwd"), parent, subagent, status)
}
