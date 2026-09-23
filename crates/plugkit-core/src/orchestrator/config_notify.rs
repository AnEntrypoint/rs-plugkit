use serde_json::{json, Value};

use crate::pkfs;

const MAX_RECORDS: usize = 32;
const MAX_SUMMARY_ITEMS: usize = 24;
const MAX_RECORD_AGE_MS: u128 = 24 * 60 * 60 * 1000;
const MAX_DELIVERED_TO: usize = 64;

const STORE_PATH: &str = ".gm/exec-spool/.config-changes.json";

fn read_records() -> Vec<Value> {
    if !pkfs::exists(STORE_PATH) {
        return Vec::new();
    }
    let Some(raw) = pkfs::read_to_string(STORE_PATH) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn write_records(records: &[Value]) -> bool {
    pkfs::write(STORE_PATH, &Value::Array(records.to_vec()).to_string())
}

fn short_sha(sha: &str) -> String {
    let trimmed = sha.trim();
    trimmed.chars().take(12).collect()
}

pub fn record_change(tier: &str, old_sha: &str, new_sha: &str, changed: &[String]) -> Option<String> {
    if old_sha.trim() == new_sha.trim() {
        return None;
    }

    let now = super::state::now_ms();
    let mut summary: Vec<Value> = changed
        .iter()
        .take(MAX_SUMMARY_ITEMS)
        .map(|s| Value::String(s.clone()))
        .collect();
    let truncated = changed.len() > MAX_SUMMARY_ITEMS;
    if truncated {
        summary.push(Value::String(format!(
            "... and {} more",
            changed.len() - MAX_SUMMARY_ITEMS
        )));
    }

    let id = format!("cfg-{}-{}-{}", tier, short_sha(new_sha), now);

    let record = json!({
        "id": id,
        "tier": tier,
        "old_sha": short_sha(old_sha),
        "new_sha": short_sha(new_sha),
        "changed": summary,
        "changed_count": changed.len(),
        "changed_truncated": truncated,
        "ts": now as u64,
        "delivered_to": Value::Array(Vec::new()),
    });

    let mut records = read_records();
    records.push(record);
    if records.len() > MAX_RECORDS {
        let overflow = records.len() - MAX_RECORDS;
        records.drain(0..overflow);
    }

    if !write_records(&records) {
        return None;
    }

    #[cfg(target_arch = "wasm32")]
    crate::wasm_dispatch::emit_event(
        "config_changed",
        json!({
            "id": id,
            "tier": tier,
            "old_sha": short_sha(old_sha),
            "new_sha": short_sha(new_sha),
            "changed": Value::Array(summary),
            "changed_count": changed.len(),
        }),
    );

    Some(id)
}

pub fn drain_for_session(session_id: Option<&str>) -> Value {
    let key = session_id
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("(no-session)");

    let mut records = read_records();
    if records.is_empty() {
        return Value::Array(Vec::new());
    }

    let now = super::state::now_ms();
    let before_len = records.len();
    records.retain(|r| {
        let ts = r.get("ts").and_then(|v| v.as_u64()).unwrap_or(0) as u128;
        ts == 0 || now.saturating_sub(ts) <= MAX_RECORD_AGE_MS
    });
    let mut dirty = records.len() != before_len;

    let mut undelivered: Vec<Value> = Vec::new();
    for record in records.iter_mut() {
        let already = record
            .get("delivered_to")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().any(|s| s.as_str() == Some(key)))
            .unwrap_or(false);
        if already {
            continue;
        }

        let mut out = record.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.remove("delivered_to");
        }
        undelivered.push(out);

        if let Some(roster) = record
            .get_mut("delivered_to")
            .and_then(|v| v.as_array_mut())
        {
            roster.push(Value::String(key.to_string()));
            if roster.len() > MAX_DELIVERED_TO {
                let overflow = roster.len() - MAX_DELIVERED_TO;
                roster.drain(0..overflow);
            }
        } else {
            record["delivered_to"] = json!([key]);
        }
        dirty = true;
    }

    if dirty {
        let _ = write_records(&records);
    }

    Value::Array(undelivered)
}
