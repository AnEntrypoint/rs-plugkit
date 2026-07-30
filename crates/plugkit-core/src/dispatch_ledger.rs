#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_write, host_now_ms};

const MAX_ENTRIES: usize = 500;

fn ledger_path(cwd: &str) -> String {
    if cwd.is_empty() {
        ".gm/exec-spool/.dispatch-ledger.json".to_string()
    } else {
        format!("{}/.gm/exec-spool/.dispatch-ledger.json", cwd.trim_end_matches('/').trim_end_matches('\\'))
    }
}

fn now_ms() -> u64 {
    unsafe { host_now_ms() }
}

pub fn record(cwd: &str, verb: &str, fingerprint: &str, exit_code: i64) -> String {
    let path = ledger_path(cwd);
    let existing = host_read(&path).unwrap_or_default();
    let mut list: Vec<Value> = if existing.trim().is_empty() {
        vec![]
    } else {
        serde_json::from_str::<Value>(&existing)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    };
    let ts = now_ms();
    let seq = list.len() as u64;
    let dispatch_id = format!("{}-{}-{:x}", ts, seq, {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        verb.hash(&mut h);
        fingerprint.hash(&mut h);
        ts.hash(&mut h);
        h.finish()
    });
    let entry = json!({
        "dispatch_id": dispatch_id,
        "verb": verb,
        "fingerprint": fingerprint,
        "ts": ts,
        "exit_code": exit_code,
    });
    list.push(entry);
    if list.len() > MAX_ENTRIES {
        let drop = list.len() - MAX_ENTRIES;
        list.drain(0..drop);
    }
    let serialized = Value::Array(list).to_string();
    host_write(&path, &serialized);
    dispatch_id
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
    list.into_iter().find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id))
}
