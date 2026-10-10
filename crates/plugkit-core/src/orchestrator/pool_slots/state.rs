use serde_json::{json, Value};
use std::collections::HashSet;

use super::super::pool_rank;
use super::super::transitions::prd_open_rows_with_recency;
use super::admission::{dirty_target_verdict, row_by_id, worktree_dirt, writer_targets_of};
use super::heartbeats::{count_of_record_live, declared_holds, held_rows, read_heartbeats};
use super::{now_ms, pool_dir, DEFAULT_SPAWN_CEILING, LAUNCH_ID_PREFIX};
use crate::pkfs;

pub(super) fn read_ceiling(dir: &str) -> Option<u64> {
    let body = pkfs::read_to_string(&format!("{}/ceiling.json", dir))?;
    serde_json::from_str::<Value>(&body).ok()?.get("ceiling")?.as_u64()
}

pub fn slot_state(project_root: &str) -> Value {
    let observed = count_of_record_live(project_root, now_ms()).map(|live| (live, "count_of_record"));
    slot_parts(project_root, observed).0
}

pub(super) fn slot_parts(project_root: &str, observed: Option<(u64, &'static str)>) -> (Value, Vec<String>) {
    let dir = pool_dir(project_root);
    let live = read_heartbeats(&dir, now_ms());
    let heartbeat_live = live.count as u64;
    let live_count = observed.map_or(heartbeat_live, |(count, _)| count);
    let live_source = observed.map_or("heartbeats", |(_, source)| source);
    let ceiling = read_ceiling(&dir);
    let free = ceiling.map(|c| c.saturating_sub(live_count));
    let (blocker_entries, work): (Vec<(Value, usize)>, Vec<(Value, usize)>) = prd_open_rows_with_recency()
        .into_iter()
        .partition(|(row, _)| pool_rank::is_blocker_row(row));
    let blockers: Vec<Value> = blocker_entries.into_iter().map(|(row, _)| row).collect();
    let open_rows = work.len();
    let witness_gap_open = work
        .iter()
        .filter(|(row, _)| row.get("id").and_then(Value::as_str).is_some_and(|id| id.starts_with(LAUNCH_ID_PREFIX)))
        .count();
    let mut live_rows = held_rows(&live, &work);
    live_rows.extend(declared_holds(&dir, now_ms()));
    live_rows.sort();
    live_rows.dedup();
    let ranked = pool_rank::rank(&work, &blockers, &live_rows);
    let dirt = worktree_dirt();
    let mut claimed: HashSet<String> = work
        .iter()
        .filter(|(row, _)| row.get("id").and_then(Value::as_str).is_some_and(|id| live_rows.iter().any(|live| live == id)))
        .flat_map(|(row, _)| writer_targets_of(row))
        .collect();
    let budget = ceiling.unwrap_or(DEFAULT_SPAWN_CEILING as u64).saturating_sub(live_count);
    let mut candidates_removed: Vec<Value> = Vec::new();
    let mut candidates: Vec<String> = Vec::new();
    let mut advertised: Vec<String> = Vec::new();
    for id in ranked["candidates"].as_array().into_iter().flatten().filter_map(|value| value.as_str()) {
        let Some(row) = row_by_id(&work, id) else {
            continue;
        };
        if let Some((filter, field, file)) = dirty_target_verdict(row, &dirt) {
            let mut entry = json!({"id": id, "filter": filter});
            entry[field] = json!(file);
            candidates_removed.push(entry);
            continue;
        }
        let targets = writer_targets_of(row);
        if let Some(target) = targets.iter().find(|target| claimed.contains(target.as_str())) {
            candidates_removed.push(json!({"id": id, "filter": "live_writer", "writer_target": target}));
            continue;
        }
        if (advertised.len() as u64) < budget {
            claimed.extend(targets);
            advertised.push(id.to_string());
        }
        candidates.push(id.to_string());
    }
    let node_launchable = candidates
        .iter()
        .filter(|id| row_by_id(&work, id.as_str()).is_some_and(|row| pool_rank::arm(row).is_none()))
        .count();
    let candidate_values: Vec<Value> = candidates.iter().map(|id| json!(id)).collect();
    let launch_filters: Vec<Value> = candidates
        .iter()
        .filter(|id| !advertised.contains(*id))
        .map(|id| json!({"id": id, "filter": "beyond_refill"}))
        .collect();
    let action = match (open_rows, free) {
        (0, _) => "none",
        (_, Some(0)) => "hold",
        _ => "launch",
    };
    let slots = json!({
        "live": live_count,
        "live_source": live_source,
        "live_heartbeats": heartbeat_live,
        "aging_heartbeats": live.aging,
        "live_rows": live_rows,
        "live_sessions": live.sessions,
        "open_rows": open_rows,
        "witness_gap_open": witness_gap_open,
        "blocker_rows": blockers.len(),
        "candidates": candidate_values,
        "launchable": candidates.len(),
        "node_launchable": node_launchable,
        "advertised": advertised.clone(),
        "candidates_removed": candidates_removed,
        "launch_filters": launch_filters,
        "dirty_check": {"source": "git status --porcelain -uall", "unknown": dirt.unknown, "entries": dirt.entries.len()},
        "supply": ranked["supply"].clone(),
        "ceiling": ceiling,
        "free": free,
        "action": action,
        "reaped_heartbeats": live.reaped,
    });
    (slots, advertised)
}

pub fn spawn_ceiling(slots: &Value) -> usize {
    slots["ceiling"].as_u64().map_or(DEFAULT_SPAWN_CEILING, |ceiling| ceiling as usize)
}
