//! Pending-notification store for config-source changes.
//!
//! A config source (agent-instruction tier, discipline policy, FSM vendor doc, ...) can
//! change at ANY moment -- typically between two instruction dispatches, i.e. while no
//! agent is inside a verb call at all. A change emitted only as a log line at the instant
//! it happens is therefore invisible to every running agent: nothing is listening. So the
//! change is PERSISTED here at record time and drained onto the next instruction response
//! body, the same surface `update_available` / `discipline_policies` already use.
//!
//! Delivery-once is per SESSION, not global. Several agents run concurrently against the
//! same project (the plugin instance itself is process-wide and shared), so a global
//! "delivered" flag would let whichever agent dispatched first swallow the notification
//! for all the others. Each record instead carries a `delivered_to` roster of session ids
//! it has already been handed to; a session is notified exactly once and never again.
//!
//! No process-wide caching lives in this module for the same reason: every path is derived
//! from `gm_dir()` per call, which re-resolves the project root, so two projects sharing
//! one plugin instance never read or write each other's store.

use serde_json::{json, Value};

use super::gm_dir;
use crate::pkfs;

/// Cap on retained records. A config source that flaps (a sync loop rewriting the same
/// file) must not grow this file without bound; the newest records are the ones an agent
/// still needs to act on, so eviction drops the oldest.
const MAX_RECORDS: usize = 32;

/// Cap on the per-record change summary. The whole point of this field is that an agent
/// learns WHAT changed, but a wholesale diff of a large config file would crowd out the
/// rest of the instruction body, so the summary is truncated to a readable roster.
const MAX_SUMMARY_ITEMS: usize = 24;

/// Records older than this are dropped on the next drain even if some session never
/// collected them. A session that has been gone for a day is not coming back for its
/// notification, and without this the `delivered_to` roster pins records forever.
const MAX_RECORD_AGE_MS: u128 = 24 * 60 * 60 * 1000;

/// Cap on the retained `delivered_to` roster per record. Sessions are unbounded over time;
/// without a cap a long-lived record accumulates one entry per dispatching agent forever.
/// Overflow evicts the oldest session id, which can at worst re-notify a very old session
/// -- strictly better than unbounded growth, and unreachable in practice because a record
/// that many sessions have seen is already past `MAX_RECORD_AGE_MS`.
const MAX_DELIVERED_TO: usize = 64;

fn store_path() -> String {
    gm_dir()
        .join("exec-spool")
        .join(".config-changes.json")
        .to_string_lossy()
        .to_string()
}

fn read_records() -> Vec<Value> {
    let path = store_path();
    if !pkfs::exists(&path) {
        return Vec::new();
    }
    let Some(raw) = pkfs::read_to_string(&path) else {
        return Vec::new();
    };
    // A torn or hand-mangled store must not take down the instruction dispatch: config
    // notification is an advisory surface, so an unparseable file degrades to "no pending
    // changes" rather than propagating an error into every agent's response body.
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn write_records(records: &[Value]) -> bool {
    pkfs::write(&store_path(), &Value::Array(records.to_vec()).to_string())
}

fn short_sha(sha: &str) -> String {
    // Config shas are compared for equality, never resolved, so a full 40-char hex pair per
    // record is pure payload weight. 12 chars is the git-conventional collision-safe prefix.
    let trimmed = sha.trim();
    trimmed.chars().take(12).collect()
}

/// Record that a config source changed. Called by the config-sync module once it has
/// resolved a real before/after pair -- this module never inspects or syncs anything itself.
///
/// `changed` is the concrete roster of what moved (config keys, file paths) so an agent can
/// tell "the discipline policy gained a rule" from "an unrelated doc was reformatted"
/// without re-reading the source. An empty roster is accepted but recorded honestly as
/// such rather than being dressed up as a summary.
///
/// Returns the recorded change id, or `None` if the store could not be written (in which
/// case the caller's change simply goes unannounced -- it is not retried here, because a
/// failed write means the spool dir is unavailable and a retry would fail identically).
pub fn record_change(tier: &str, old_sha: &str, new_sha: &str, changed: &[String]) -> Option<String> {
    // A no-op rewrite (same content hashed to the same sha) is not a change an agent needs
    // to hear about; recording it would burn the delivery-once budget on nothing.
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

    // The id folds tier+shas+timestamp so two different sources changing in the same
    // millisecond stay distinct records, and so a re-record of the identical transition
    // is recognisable as such by any consumer keying off it.
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
    // Oldest-first eviction: a record an agent has not collected yet is worth less than a
    // fresher one describing the same source's current state.
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

/// Drain the changes this session has not been shown yet, marking them delivered.
///
/// Called once per instruction dispatch. Marking happens here rather than on a later
/// acknowledgement verb because there is no such verb and no guarantee an agent would
/// dispatch it -- at-most-once delivery (a change lost if the response is dropped in
/// flight) is the correct trade against re-notifying the same session on every dispatch
/// forever, which is what the caller explicitly must not do.
///
/// `session_id` is `None` for a dispatch that never carried one. Those share the
/// `"(no-session)"` bucket, which still gets delivery-once semantics -- the alternative,
/// treating every session-less dispatch as a fresh recipient, reintroduces exactly the
/// re-notify-forever loop this function exists to prevent.
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
        // A record with an unreadable/absent ts is kept rather than dropped: losing a real
        // notification is worse than retaining one stale entry, and the MAX_RECORDS cap
        // still bounds the file.
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

        // The roster is bookkeeping the agent does not need; strip it from what is surfaced
        // so the response body carries only the change itself.
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

    // Persist the marks even when nothing new was found, because the age sweep above may
    // have pruned records. A failed write is deliberately not fatal: the caller still gets
    // the notifications, and the worst case is one repeat delivery on the next dispatch --
    // strictly better than withholding a real config change because the spool is unwritable.
    if dirty {
        let _ = write_records(&records);
    }

    Value::Array(undelivered)
}
