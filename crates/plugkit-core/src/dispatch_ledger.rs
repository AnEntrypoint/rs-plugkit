#![cfg(target_arch = "wasm32")]

use crate::wasm_dispatch::{
    host_cas_write, host_fs_readdir, host_now_ms, host_read, host_remove_file_never_directory,
    host_stat, unpack_to_value_pub,
};
use serde_json::{json, Value};

const MAX_ENTRIES: usize = 5000;
pub const RETAIN_MS: u64 = 24 * 60 * 60 * 1000;
const SPOOL_TMP_MARKER: &str = ".tmp-";
const SPOOL_TMP_STALE_MS: u64 = 30 * 1000;

pub const RETAINED_ENTRIES: usize = MAX_ENTRIES;

pub fn ledger_path(cwd: &str) -> String {
    if cwd.is_empty() {
        ".gm/exec-spool/.dispatch-ledger.json".to_string()
    } else {
        format!(
            "{}/.gm/exec-spool/.dispatch-ledger.json",
            cwd.trim_end_matches('/').trim_end_matches('\\')
        )
    }
}

fn now_ms() -> u64 {
    unsafe { host_now_ms() }
}

fn spool_dir(cwd: &str) -> String {
    if cwd.is_empty() {
        ".gm/exec-spool".to_string()
    } else {
        format!(
            "{}/.gm/exec-spool",
            cwd.trim_end_matches('/').trim_end_matches('\\')
        )
    }
}

fn sweep_stale_spool_tmp(cwd: &str, now: u64) {
    let dir = spool_dir(cwd);
    let Value::Array(entries) =
        unpack_to_value_pub(unsafe { host_fs_readdir(dir.as_ptr(), dir.len() as u32) })
    else {
        return;
    };
    for name in entries.iter().filter_map(Value::as_str) {
        if !name.contains(SPOOL_TMP_MARKER) {
            continue;
        }
        let path = format!("{dir}/{name}");
        let stale = host_stat(&path)
            .and_then(|stat| stat.get("mtimeMs").and_then(Value::as_u64))
            .is_some_and(|mtime| mtime.saturating_add(SPOOL_TMP_STALE_MS) < now);
        if stale {
            host_remove_file_never_directory(&path);
        }
    }
}

const LEDGER_BODY_MAX_CHARS: usize = 8192;

fn bounded_request_body(body: &str) -> (String, bool) {
    let text: String = body.chars().take(LEDGER_BODY_MAX_CHARS).collect();
    let truncated = text.len() < body.len();
    (text, truncated)
}

pub fn record(
    cwd: &str,
    verb: &str,
    request_body: &str,
    fingerprint: &str,
    exit_code: i64,
    session_id: Option<&str>,
    output_bytes: Option<u64>,
) -> Result<String, String> {
    let path = ledger_path(cwd);
    sweep_stale_spool_tmp(cwd, now_ms());
    let (body_text, body_truncated) = bounded_request_body(request_body);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let existing = host_read(&path).unwrap_or_default();
        let parsed: Option<Vec<Value>> = if existing.trim().is_empty() {
            Some(vec![])
        } else {
            match serde_json::from_str::<Value>(&existing) {
                Ok(Value::Array(a)) => Some(a),
                _ => None,
            }
        };
        let ts = now_ms();
        let Some(mut list) = parsed else {
            return Err(ledger_unavailable(&path));
        };
        let seq = list.len() as u64;
        let dispatch_id = new_dispatch_id(verb, fingerprint, ts, seq);
        list.push(json!({
            "dispatch_id": dispatch_id,
            "verb": verb,
            "fingerprint": fingerprint,
            "ts": ts,
            "exit_code": exit_code,
            "output_bytes": output_bytes,
            "session_id": session_id,
            "cwd": cwd,
            "request_body": body_text,
            "request_body_truncated": body_truncated,
        }));
        list.retain(|e| {
            e.get("ts")
                .and_then(|v| v.as_u64())
                .is_some_and(|t| t.saturating_add(RETAIN_MS) >= ts)
        });
        if list.len() > MAX_ENTRIES {
            let drop = list.len() - MAX_ENTRIES;
            list.drain(0..drop);
        }
        let serialized = Value::Array(list).to_string();
        match host_cas_write(&path, &existing, &serialized) {
            CAS_SWAPPED => return Ok(dispatch_id),
            CAS_MISMATCH if attempt < CAS_MAX_ATTEMPTS => continue,
            _ => return Err(ledger_unavailable(&path)),
        }
    }
}

fn ledger_unavailable(path: &str) -> String {
    format!(
        "dispatch ledger {path} could not be read or written as a JSON array; dispatch not recorded and the file left unchanged"
    )
}

fn new_dispatch_id(verb: &str, fingerprint: &str, ts: u64, seq: u64) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    verb.hash(&mut h);
    fingerprint.hash(&mut h);
    ts.hash(&mut h);
    format!("{}-{}-{:x}", ts, seq, h.finish())
}

const CAS_SWAPPED: u32 = 1;
const CAS_MISMATCH: u32 = 2;
const CAS_MAX_ATTEMPTS: u32 = 16;

pub fn attach_reply_sha256(cwd: &str, dispatch_id: &str, reply_sha256: &str) {
    let path = ledger_path(cwd);
    for _ in 0..CAS_MAX_ATTEMPTS {
        let existing = host_read(&path).unwrap_or_default();
        let Ok(Value::Array(mut list)) = serde_json::from_str::<Value>(&existing) else {
            return;
        };
        let Some(entry) = list
            .iter_mut()
            .find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id))
        else {
            return;
        };
        entry["reply_sha256"] = json!(reply_sha256);
        if host_cas_write(&path, &existing, &Value::Array(list).to_string()) == CAS_SWAPPED {
            return;
        }
    }
}

pub fn subagent_running_count(
    cwd: &str,
    parent_session: &str,
    now_ms: u64,
    window_ms: u64,
) -> usize {
    let prefix = format!("{}-", parent_session);
    let since_ms = now_ms.saturating_sub(window_ms);
    let raw = host_read(&ledger_path(cwd)).unwrap_or_default();
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|e| {
            e.get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with(&prefix) && s.contains("-sub"))
                && e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0) >= since_ms
        })
        .count()
}

pub fn subagent_dispatches_since(cwd: &str, parent_session: &str, since_ms: u64) -> usize {
    let prefix = format!("{}-", parent_session);
    let raw = host_read(&ledger_path(cwd)).unwrap_or_default();
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|e| {
            e.get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with(&prefix))
                && e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0) >= since_ms
        })
        .count()
}

pub fn lookup(cwd: &str, dispatch_id: &str) -> Option<Value> {
    let path = ledger_path(cwd);
    let raw = host_read(&path).unwrap_or_default();
    if raw.trim().is_empty() {
        return None;
    }
    let list = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => return None,
    };
    list.into_iter()
        .find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id))
}

pub fn submodule_holding(cwd: &str, dispatch_id: &str) -> Option<String> {
    let root = cwd.trim_end_matches('/').trim_end_matches('\\');
    let gitmodules = host_read(&format!("{}/.gitmodules", root)).unwrap_or_default();
    gitmodules
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("path")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|path| format!("{}/{}", root, path.trim()))
        })
        .find(|sub_cwd| lookup(sub_cwd, dispatch_id).is_some())
        .map(|sub_cwd| ledger_path(&sub_cwd))
}

pub fn window(cwd: &str) -> Option<(usize, u64)> {
    let raw = host_read(&ledger_path(cwd))?;
    let list = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => a,
        _ => return None,
    };
    let oldest = list
        .iter()
        .filter_map(|e| e.get("ts").and_then(|v| v.as_u64()))
        .min()?;
    Some((list.len(), oldest))
}
