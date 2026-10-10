use serde_json::{json, Value};
use std::collections::HashSet;

use super::super::pool_rank;
use super::super::transitions::prd_open_rows_with_recency;
use super::admission::{
    dirty_target_verdict, launch_filter_of, node_only_module, row_by_id, witness_gap_admitted,
    witness_gap_refusal, worktree_dirt, WorktreeDirt,
};
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
    let admitted: HashSet<String> = work
        .iter()
        .filter(|(row, _)| witness_gap_admitted(row, project_root))
        .filter_map(|(row, _)| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let ranked = pool_rank::rank(&work, &blockers, &live_rows, &admitted);
    let dirt = worktree_dirt();
    let mut candidates_removed: Vec<Value> = Vec::new();
    let candidates: Vec<Value> = ranked["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|value| {
            let Some(id) = value.as_str() else {
                return true;
            };
            match row_by_id(&work, id).and_then(|row| dirty_target_verdict(row, &dirt)) {
                Some((filter, field, file)) => {
                    let mut entry = json!({"id": id, "filter": filter});
                    entry[field] = json!(file);
                    candidates_removed.push(entry);
                    false
                }
                None => true,
            }
        })
        .cloned()
        .collect();
    let node_candidates: Vec<String> = ranked["node_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str())
        .filter(|id| {
            row_by_id(&work, id).is_some_and(|row| {
                admitted.contains(*id)
                    && pool_rank::node_arm(row, true).is_none()
                    && node_only_module(row, project_root)
                    && dirty_target_verdict(row, &dirt).is_none()
            })
        })
        .map(str::to_string)
        .collect();
    let launch_filters: Vec<Value> = candidates
        .iter()
        .filter_map(|value| value.as_str())
        .filter(|id| !node_candidates.iter().any(|node| node.as_str() == *id))
        .map(|id| json!({"id": id, "filter": launch_filter_of(&work, id, &admitted, project_root)}))
        .collect();
    let action = match (open_rows, free) {
        (0, _) => "none",
        (_, Some(0)) => "hold",
        _ => "launch",
    };
    let witness_gap = witness_gap_verdicts(&work, &blockers, &live_rows, &dirt, project_root);
    let slots = json!({
        "live": live_count,
        "live_source": live_source,
        "live_heartbeats": heartbeat_live,
        "aging_heartbeats": live.aging,
        "live_rows": live_rows,
        "live_sessions": live.sessions,
        "open_rows": open_rows,
        "witness_gap_open": witness_gap_open,
        "witness_gap": witness_gap,
        "blocker_rows": blockers.len(),
        "candidates": candidates,
        "launchable": node_candidates.len(),
        "candidates_removed": candidates_removed,
        "launch_filters": launch_filters,
        "dirty_check": {"source": "git status --porcelain -uall", "unknown": dirt.unknown, "entries": dirt.entries.len()},
        "supply": ranked["supply"].clone(),
        "ceiling": ceiling,
        "free": free,
        "action": action,
        "reaped_heartbeats": live.reaped,
    });
    (slots, node_candidates)
}

const WITNESS_GAP_ROWS_SHOWN: usize = 60;

fn witness_gap_verdicts(
    work: &[(Value, usize)],
    blockers: &[Value],
    live_rows: &[String],
    dirt: &WorktreeDirt,
    project_root: &str,
) -> Value {
    let pending_blocked: HashSet<&str> = blockers
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .filter_map(pool_rank::blocked_row_of)
        .collect();
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut launchable = 0u64;
    let mut rows: Vec<Value> = Vec::new();
    for (row, _) in work {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !id.starts_with(LAUNCH_ID_PREFIX) {
            continue;
        }
        let verdict = if let Some(reason) = witness_gap_refusal(row, project_root) {
            reason
        } else if pool_rank::is_outcome_row(row) || pool_rank::is_refuted_row(row) {
            "excluded_outcome_or_refuted".to_string()
        } else if live_rows.iter().any(|live| live == id) {
            "held_by_live_heartbeat".to_string()
        } else if pending_blocked.contains(id) {
            "pending_blocker".to_string()
        } else if let Some((_, _, target)) = dirty_target_verdict(row, dirt) {
            format!("dirty_target:{target}")
        } else {
            launchable += 1;
            "launchable".to_string()
        };
        let bucket = verdict.split(':').next().unwrap_or_default().to_string();
        *counts.entry(bucket).or_insert(0) += 1;
        rows.push(json!({"id": id, "reason": verdict}));
    }
    let open = rows.len();
    rows.truncate(WITNESS_GAP_ROWS_SHOWN);
    json!({"open": open, "launchable": launchable, "counts": counts, "rows": rows})
}

pub fn spawn_ceiling(slots: &Value) -> usize {
    slots["ceiling"].as_u64().map_or(DEFAULT_SPAWN_CEILING, |ceiling| ceiling as usize)
}
