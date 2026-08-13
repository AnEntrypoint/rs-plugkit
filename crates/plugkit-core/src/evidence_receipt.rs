#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_write, host_now_ms};

const RECEIPT_PATH: &str = ".gm/exec-spool/.evidence-receipt.json";
const LEDGER_PATH: &str = ".gm/exec-spool/.dispatch-ledger.json";

fn now_ms() -> u64 {
    unsafe { host_now_ms() }
}

fn completed_prd_ids() -> Vec<String> {
    let (body, _err, code) = crate::orchestrator::prd::handle_list("");
    if code != 0 { return vec![]; }
    let Ok(v) = serde_json::from_str::<Value>(&body) else { return vec![] };
    let Some(items) = v.get("items").and_then(|v| v.as_array()) else { return vec![] };
    items
        .iter()
        .filter(|it| {
            let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            !crate::orchestrator::prd::status_is_open(status)
        })
        .filter_map(|it| it.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect()
}

fn head_sha() -> String {
    crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn write() -> Value {
    let ledger_raw = host_read(LEDGER_PATH).unwrap_or_default();
    let ledger_hash = format!("{:016x}", crate::hash::fnv1a64(ledger_raw.as_bytes()));
    let dispatch_count = serde_json::from_str::<Value>(&ledger_raw)
        .ok()
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0);
    let last_dispatch_id = serde_json::from_str::<Value>(&ledger_raw)
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.last().cloned()))
        .and_then(|e| e.get("dispatch_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let receipt = json!({
        "completed_prd_ids": completed_prd_ids(),
        "dispatch_count": dispatch_count,
        "last_dispatch_id": last_dispatch_id,
        "ledger_hash": ledger_hash,
        "ledger_hash_algo": "fnv1a64",
        "head_sha": head_sha(),
        "ts": now_ms(),
    });
    host_write(RECEIPT_PATH, &receipt.to_string());
    receipt
}
