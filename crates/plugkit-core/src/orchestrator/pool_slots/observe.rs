use serde_json::{json, Value};

use super::super::pool_rank;
use super::super::transitions::prd_open_rows_with_recency;
use super::floor::{ceiling_from_refusal, floor_denial_text, launch_instruction, monitor_block};
use super::heartbeats::{count_of_record_live, write_count_of_record, HELD_ROWS_FILE};
use super::state::{read_ceiling, slot_parts, spawn_ceiling};
use super::traversal::{
    read_surface_state, record_scanned_surfaces, traversal_candidates, TRAVERSAL_SURFACE_SHOWN,
};
use super::{now_ms, pool_dir, LAUNCH_ID_PREFIX, REFILL_FLOOR, TRAVERSAL_LAUNCH_ID};
use crate::pkfs;

const TRAVERSAL_SUPPLY_FACTOR: u64 = 2;
const DEFAULT_LIST_LIMIT: usize = 12;

const POOL_RULES: [&str; 8] = [
    "Floor 10: while open_rows > 0 keep live at or above the floor, which is 10 or the recorded spawn ceiling when that is lower; the floor is a gate, not a launch count: refill is measured against the ceiling (rule 3).",
    "Pass the ListAgents count of running subagents as body.live on every call: it is the count of record, kept 5 minutes in .gm/pool/count-of-record.json. prd-resolve and transition use that record when they carry no live field; with no fresh record they do not deny and reply count_of_record: absent. slots.live_heartbeats is only the heartbeat cross-check.",
    "Spawn ceiling 20, or the N a spawn refusal recorded: while open_rows > 0 fill toward it. free_slots = ceiling - live; refill_needed = min(free_slots, launchable + traversal slot), where launchable = slots.candidates after the dirty_target and live_writer gates (node rows first, then any open row) and the traversal slot is 1 while traversal.needed; idle_slots = free_slots - refill_needed. shortfall = floor - live is what the floor needs; sufficient is true when the advertised launches cover the shortfall. Launch exactly the ids in slots.launch; unfilled_shortfall = shortfall - refill_needed is filled from slots.candidates in order.",
    "A live count under the floor while open_rows > 0 is a gate denial (error_code floor_gate_denied) on pool-observe and transition (never on prd-resolve: a closure adds no launch): launch the advertised gm-worker subagents from slots.launch, then launch unfilled_shortfall more from slots.candidates (node-first, then any open row), then retry. It is also a FAILURE: append `FAILURE: <UTC timestamp> live count fell to <live> with <open_rows> pending rows` to .gm/witness-log.md.",
    "Refill on every completion, in the same turn: launch one replacement per freed slot from launch (node-first candidates). Never launch a row that is in slots.live_rows.",
    "Pass body.held = the row id of every running worker on every call, including a worker whose heartbeat is not written yet. The newest held list is kept for 30 minutes; send held: []. slots.live_rows lists only ids that exist as a row id in the prd store, so a scan of the prd text finds every id it lists; a held or heartbeat id with no such row is listed in slots.live_rows_unmatched instead.",
    "A heartbeat refreshes at least every 5 minutes and counts as live for 10 minutes; one older than 5 minutes is listed in slots.aging_heartbeats, not dropped. On a spawn refusal, call pool-observe with body.refusal set to the refusal text.",
    "A candidate whose named target file has uncommitted changes in the worktree is removed from slots.candidates and from the launch list; slots.candidates_removed names it with filter dirty_target and its dirty_target path. Target names come only from the row subject, title, why, witness, acceptance, acceptance_criteria and text fields, and only tokens with a source or document extension: a token with a directory matches that path, and a bare file name matches any dirty file of that name outside .gm/; a shared document name (AGENTS.md, README.md, CHANGELOG.md) counts only when the row's surface field names it; a row naming no such token is not filtered. If git status cannot be read, every row with a named path is removed with filter git_status_unknown. A candidate whose repo-relative target file is also named by a live row, or by an earlier advertised row of the same reply, is removed with filter live_writer and writer_target names that path. A candidate that passes both gates but is not advertised in this reply is listed in slots.launch_filters with filter beyond_refill: it waits for a free slot. Every launch item, slots.launch_filters entry and slots.candidate_reasons value carries a reason string, and slots.candidate_reasons maps each displayed candidate to its reason. A candidate whose row id is a whole token of the subject of a non-witness commit among the last 500 commits reachable from HEAD is removed with filter already_fixed_on_head and that commit's sha, and slots.already_fixed_on_head lists those rows; git is the only input, and an unreadable git removes no row by this filter.",
];

pub fn handle_observe(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let observed_live = body.get("live").and_then(Value::as_u64);
    let refusal = body
        .get("refusal")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dir = pool_dir(".");
    let record_now = now_ms();
    if let Some(live) = observed_live {
        if !write_count_of_record(&dir, live, record_now) {
            return (String::new(), "pool-observe: could not write .gm/pool/count-of-record.json".to_string(), 1);
        }
    }
    let observed = observed_live
        .map(|live| (live, "listagents"))
        .or_else(|| count_of_record_live(".", record_now).map(|live| (live, "count_of_record")));
    let refused_ceiling = refusal.as_deref().and_then(ceiling_from_refusal);
    let record_path = format!("{}/ceiling.json", dir);
    if observed_live.is_some() || refusal.is_some() {
        let previous: Value = pkfs::read_to_string(&record_path)
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        let record = json!({
            "ceiling": refused_ceiling.or_else(|| read_ceiling(&dir)),
            "observed_live": observed_live.or_else(|| previous["observed_live"].as_u64()),
            "refusal_text": refusal.or_else(|| previous["refusal_text"].as_str().map(str::to_string)),
            "ts": now_ms(),
        });
        if !pkfs::write(&record_path, &record.to_string()) {
            return (String::new(), "pool-observe: could not write .gm/pool/ceiling.json".to_string(), 1);
        }
    }
    if let Some(held) = body.get("held").and_then(Value::as_array) {
        let rows: Vec<String> = held
            .iter()
            .filter_map(|row| row.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
            .collect();
        let record = json!({"rows": rows, "ts": now_ms()});
        if !pkfs::write(&format!("{}/{}", dir, HELD_ROWS_FILE), &record.to_string()) {
            return (String::new(), "pool-observe: could not write .gm/pool/held-rows.json".to_string(), 1);
        }
    }
    let (mut slots, advertised) = slot_parts(".", observed);
    let limit = body
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map_or(DEFAULT_LIST_LIMIT, |n| n as usize);
    cap_candidates(&mut slots, limit);
    for key in ["live_rows", "live_rows_unmatched", "live_sessions", "candidates_removed", "launch_filters", "already_fixed_on_head"] {
        cap_list(&mut slots, key, limit);
    }
    let held = slots["live_rows_total"].as_u64().unwrap_or(0);
    let live = slots["live"].as_u64().unwrap_or(0);
    let ceiling = spawn_ceiling(&slots) as u64;
    let floor = REFILL_FLOOR.min(ceiling);
    let open_rows = slots["open_rows"].as_u64().unwrap_or(0);
    let shortfall = if open_rows > 0 { floor.saturating_sub(live) } else { 0 };
    let free_slots = ceiling.saturating_sub(live);
    let now = now_ms();
    let state_dir = pool_dir(".");
    let scanned: Vec<String> = body
        .get("scanned_surfaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if !scanned.is_empty() {
        if let Err(reason) = record_scanned_surfaces(&state_dir, &scanned, now) {
            return (String::new(), format!("pool-observe: {reason}"), 1);
        }
    }
    let (surface_state, state_error) = match read_surface_state(&state_dir) {
        Ok(state) => (state, None),
        Err(reason) => (std::collections::BTreeMap::new(), Some(reason)),
    };
    let unscanned = if state_error.is_some() {
        Vec::new()
    } else {
        traversal_candidates(".", &surface_state, now)
    };
    let launchable = slots["launchable"].as_u64().unwrap_or(0);
    let node_launchable = slots["node_launchable"].as_u64().unwrap_or(0);
    let traversal_threshold = TRAVERSAL_SUPPLY_FACTOR * REFILL_FLOOR;
    let traversal_needed = (node_launchable < traversal_threshold || launchable < shortfall) && !unscanned.is_empty();
    let refill_needed = if open_rows > 0 {
        free_slots.min(launchable + u64::from(traversal_needed))
    } else {
        0
    };
    let idle_slots = free_slots.saturating_sub(refill_needed);
    let unfilled_shortfall = shortfall.saturating_sub(refill_needed);
    let next_surface = unscanned.first().cloned();
    attach_reasons(&mut slots, &advertised, refill_needed, free_slots);
    let mut launch: Vec<Value> = advertised
        .iter()
        .take(refill_needed as usize)
        .enumerate()
        .map(|(index, id)| json!({"id": id, "role": "resolver", "reason": advertised_reason(index + 1, refill_needed)}))
        .collect();
    if traversal_needed && (launch.len() as u64) < refill_needed {
        launch.push(json!({"id": TRAVERSAL_LAUNCH_ID, "role": "traversal", "surface": next_surface, "reason": "traversal: fills a free slot after every advertised row, scanning the next unscanned surface"}));
    }
    let launch_ids: Vec<String> = launch
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    let sufficient = open_rows == 0 || shortfall == 0 || (launch_ids.len() as u64) >= shortfall;
    let traversal = json!({
        "needed": traversal_needed,
        "state_error": state_error,
        "node_witness_candidates": node_launchable,
        "threshold": traversal_threshold,
        "unscanned_surfaces": unscanned.iter().take(TRAVERSAL_SURFACE_SHOWN).collect::<Vec<&String>>(),
        "unscanned_total": unscanned.len(),
        "rule": "traversal is needed only while node witness candidates are below 2 x floor, or the advertised launches do not cover the shortfall, and candidate surfaces remain; a candidate is unscanned, unleased, and has a module (up to 3 levels deep, .js/.mjs/.cjs/.ts/.jsx/.tsx) that no scripts/ file names it by content (the module file stem as a whole word in the text of a scripts/ file); pool-observe advertises the first candidate in launch and leases nothing; a traversal brief for an open row leases the surface that row names when that surface is unscanned, and is refused otherwise; a traversal brief for traversal-node-supply leases the first candidate when it is issued; a surface named in scanned_surfaces is leased for 6 hours",
    });
    let mut monitor = monitor_block(&slots);
    if monitor["alarm"] == json!(false) {
        if let Some(fields) = monitor.as_object_mut() {
            fields.remove("alarm_action");
        }
    }
    let count_of_record = observed.map_or(json!("absent"), |(count, _)| json!(count));
    slots["launch"] = json!(launch.clone());
    let mut out = json!({
        "ok": true,
        "verb": "pool-observe",
        "refusal_ceiling": refused_ceiling,
        "floor": floor,
        "ceiling": ceiling,
        "held": held,
        "count_of_record": count_of_record,
        "refill_needed": refill_needed,
        "launchable": launchable,
        "free_slots": free_slots,
        "idle_slots": idle_slots,
        "launch": launch,
        "traversal": traversal,
        "monitor": monitor,
        "slots": slots,
    });
    out["shortfall"] = json!(shortfall);
    out["unfilled_shortfall"] = json!(unfilled_shortfall);
    out["sufficient"] = json!(sufficient);
    if body.get("rules").and_then(Value::as_bool) == Some(true) {
        out["rules"] = json!(POOL_RULES);
    } else {
        out["rules_hint"] = json!("pass body.rules=true for the pool rules");
    }
    if body.get("blocker_notes").and_then(Value::as_bool) == Some(true) {
        out["blocker_notes"] = blocker_notes_block(limit);
    }
    if observed.is_some() && open_rows > 0 && live < floor {
        let launch_text = launch_instruction(&launch_ids, shortfall);
        let mut text = floor_denial_text("pool-observe", live, floor, open_rows, refill_needed, shortfall, &launch_text);
        if refill_needed == 0 {
            text.push_str(&format!(" Advertised launchable={launchable}, free_slots={free_slots}, traversal_needed={traversal_needed}."));
        }
        out["ok"] = json!(false);
        out["error_code"] = json!("floor_gate_denied");
        out["error"] = json!(text.as_str());
        return (out.to_string(), text, 1);
    }
    (out.to_string(), String::new(), 0)
}

fn blocker_notes_block(limit: usize) -> Value {
    let rows: Vec<Value> = prd_open_rows_with_recency()
        .into_iter()
        .map(|(row, _)| row)
        .filter(|row| pool_rank::has_blocker_notes(row))
        .map(|row| {
            json!({
                "id": row.get("id").cloned().unwrap_or(Value::Null),
                "status": row.get("status").cloned().unwrap_or(Value::Null),
                "blocker_notes": row.get("blocker_notes").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let total = rows.len();
    json!({"total": total, "rows": rows.into_iter().take(limit).collect::<Vec<Value>>()})
}

fn is_witness_gap_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| id.starts_with(LAUNCH_ID_PREFIX))
}

fn cap_candidates(slots: &mut Value, limit: usize) {
    let all: Vec<Value> = slots["candidates"].as_array().cloned().unwrap_or_default();
    let total = all.len();
    let open = slots["open_rows"].as_u64().unwrap_or(0) as usize;
    let gap_open = slots["witness_gap_open"].as_u64().unwrap_or(0) as usize;
    let gap_total = all.iter().filter(|value| is_witness_gap_id(value)).count();
    let gap_quota = if open == 0 || gap_open == 0 {
        0
    } else {
        ((limit * gap_open + open / 2) / open).max(1).min(gap_total)
    };
    let other_quota = limit.saturating_sub(gap_quota);
    let mut chosen = vec![false; total];
    let (mut gap_taken, mut other_taken) = (0usize, 0usize);
    for (index, value) in all.iter().enumerate() {
        if is_witness_gap_id(value) {
            if gap_taken < gap_quota {
                chosen[index] = true;
                gap_taken += 1;
            }
        } else if other_taken < other_quota {
            chosen[index] = true;
            other_taken += 1;
        }
    }
    let want = limit.min(total);
    let mut shown = chosen.iter().filter(|flag| **flag).count();
    for flag in chosen.iter_mut() {
        if shown >= want {
            break;
        }
        if !*flag {
            *flag = true;
            shown += 1;
        }
    }
    let kept: Vec<Value> = all
        .into_iter()
        .zip(chosen)
        .filter(|(_, flag)| *flag)
        .map(|(value, _)| value)
        .collect();
    let gap_shown = kept.iter().filter(|value| is_witness_gap_id(value)).count();
    if let Some(fields) = slots.as_object_mut() {
        fields.insert("candidates".to_string(), Value::Array(kept));
        fields.insert("candidates_total".to_string(), json!(total));
        fields.insert("candidates_witness_gap_total".to_string(), json!(gap_total));
        fields.insert("candidates_witness_gap_shown".to_string(), json!(gap_shown));
    }
}

fn cap_list(slots: &mut Value, key: &str, limit: usize) {
    let total = slots[key].as_array().map_or(0, Vec::len);
    if let Some(items) = slots[key].as_array_mut() {
        items.truncate(limit);
    }
    if let Some(fields) = slots.as_object_mut() {
        fields.insert(format!("{key}_total"), json!(total));
    }
}

fn advertised_reason(position: usize, refill_needed: u64) -> String {
    format!("advertised: launch slot {position} of {refill_needed}")
}

fn beyond_refill_reason(free_slots: u64) -> String {
    format!("beyond_refill: {free_slots} free slot(s) all go to higher-ranked rows; waits for a free slot")
}

fn attach_reasons(slots: &mut Value, advertised: &[String], refill_needed: u64, free_slots: u64) {
    let beyond = beyond_refill_reason(free_slots);
    let mut reasons = serde_json::Map::new();
    for id in slots["candidates"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let reason = match advertised.iter().position(|launched| launched.as_str() == id) {
            Some(index) => advertised_reason(index + 1, refill_needed),
            None => beyond.clone(),
        };
        reasons.insert(id.to_string(), json!(reason));
    }
    if let Some(items) = slots["launch_filters"].as_array_mut() {
        for item in items.iter_mut() {
            item["reason"] = json!(beyond);
        }
    }
    slots["candidate_reasons"] = Value::Object(reasons);
}
