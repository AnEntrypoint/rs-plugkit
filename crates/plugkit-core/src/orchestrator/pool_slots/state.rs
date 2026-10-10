use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::super::pool_rank;
use super::super::transitions::{prd_open_rows_with_recency, prd_row_ids};
use super::admission::{dirty_target_verdict, row_by_id, worktree_dirt, writer_targets_of};
use super::heartbeats::{count_of_record_live, declared_holds, held_rows, read_heartbeats};
use super::{now_ms, pool_dir, DEFAULT_SPAWN_CEILING, LAUNCH_ID_PREFIX, TRAVERSAL_LAUNCH_ID};
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
    let (live_rows, live_rows_unmatched) = reconcile_live_rows(held_rows(&live, &work), &work, &dir);
    let ranked = pool_rank::rank(&work, &blockers, &live_rows);
    let dirt = worktree_dirt();
    let mut claimed: HashSet<String> = work
        .iter()
        .filter(|(row, _)| row.get("id").and_then(Value::as_str).is_some_and(|id| live_rows.iter().any(|live| live == id)))
        .flat_map(|(row, _)| writer_targets_of(row))
        .collect();
    let budget = ceiling.unwrap_or(DEFAULT_SPAWN_CEILING as u64).saturating_sub(live_count);
    let ranked_ids: std::collections::BTreeSet<&str> = ranked["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let fixed = super::admission::fixed_on_head_shas(&ranked_ids);
    let mut candidates_removed: Vec<Value> = Vec::new();
    let mut already_fixed: Vec<Value> = Vec::new();
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
        if let Some(sha) = fixed.get(id) {
            already_fixed.push(json!({"id": id, "sha": sha}));
            candidates_removed.push(json!({"id": id, "filter": "already_fixed_on_head", "sha": sha}));
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
    let ranked_rows = ranked["candidates"].as_array().map_or(0, Vec::len);
    let scan = candidates_route(&ranked["supply"], &gate_tally(&candidates_removed), open_rows, ranked_rows, candidates.len());
    let acceptance = candidate_acceptance(&work, &candidates);
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
        "malformed_heartbeats": live.malformed,
        "live_rows": live_rows,
        "live_rows_unmatched": live_rows_unmatched,
        "live_sessions": live.sessions,
        "open_rows": open_rows,
        "witness_gap_open": witness_gap_open,
        "blocker_rows": blockers.len(),
        "candidates": candidate_values,
        "candidate_acceptance": acceptance,
        "candidates_route": scan,
        "launchable": candidates.len(),
        "node_launchable": node_launchable,
        "advertised": advertised.clone(),
        "candidates_removed": candidates_removed,
        "already_fixed_on_head": already_fixed,
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

const ACCEPTANCE_FIELDS: [&str; 3] = ["acceptance_criteria", "acceptance", "acceptance_text"];
const ACCEPTANCE_MAP_CAP: usize = 32;
const ACCEPTANCE_TEXT_CHARS: usize = 240;

const RANK_SKIP_FIELDS: [&str; 6] = [
    "design_decision_rows",
    "excluded_outcome_rows",
    "excluded_refuted_rows",
    "blocker_notes_rows",
    "live_skipped_rows",
    "rows_with_pending_blocker",
];

fn clipped_text(text: &str, limit: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut clipped: String = flat.chars().take(limit).collect();
    clipped.push_str(" ...");
    clipped
}

fn candidate_acceptance(work: &[(Value, usize)], candidates: &[String]) -> Value {
    let mut map = serde_json::Map::new();
    for id in candidates.iter().take(ACCEPTANCE_MAP_CAP) {
        let Some(row) = row_by_id(work, id) else { continue };
        let Some(text) = ACCEPTANCE_FIELDS
            .iter()
            .filter_map(|key| row.get(*key).and_then(Value::as_str))
            .find(|text| !text.trim().is_empty())
        else {
            continue;
        };
        map.insert(id.clone(), json!(clipped_text(text, ACCEPTANCE_TEXT_CHARS)));
    }
    Value::Object(map)
}

fn gate_tally(removed: &[Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in removed {
        if let Some(filter) = entry["filter"].as_str() {
            *counts.entry(filter.to_string()).or_insert(0usize) += 1;
        }
    }
    counts
}

fn candidates_route(
    supply: &Value,
    gates: &BTreeMap<String, usize>,
    open_rows: usize,
    ranked_rows: usize,
    survived: usize,
) -> Value {
    let mut skipped = serde_json::Map::new();
    for key in RANK_SKIP_FIELDS {
        skipped.insert((*key).to_string(), json!(supply[key].as_u64().unwrap_or(0)));
    }
    let mut removed = serde_json::Map::new();
    for (filter, count) in gates {
        removed.insert(filter.clone(), json!(*count));
    }
    let route = if survived > 0 {
        "launch: launch the ids in slots.launch in order, node-first; slots.candidate_acceptance carries each shown candidate's acceptance text".to_string()
    } else if open_rows == 0 {
        "none: no open row, so no row is scanned and no launch is advertised".to_string()
    } else {
        format!(
            "no-candidate: the pending-row scan ranked {ranked_rows} of {open_rows} open rows and every one was skipped by rank or removed by a gate, so slots.candidates is empty and no row can be launched; take the traversal route while traversal.needed is true (pool-brief {{\"role\":\"traversal\",\"row\":\"{traversal}\",\"session\":\"<your SESSION_ID>\"}}), else wait {{\"ms\":60000}} and re-dispatch pool-observe with body.live and body.held; skipped_by_rank and removed_by_gate below name the class that emptied the list",
            traversal = TRAVERSAL_LAUNCH_ID
        )
    };
    json!({
        "scanned_pending_rows": open_rows,
        "ranked_rows": ranked_rows,
        "skipped_by_rank": skipped,
        "removed_by_gate": removed,
        "survived": survived,
        "empty": survived == 0,
        "route": route,
    })
}

fn reconcile_live_rows(rows: Vec<String>, work: &[(Value, usize)], dir: &str) -> (Vec<String>, Vec<String>) {
    let mut rows = rows;
    rows.extend(declared_holds(dir, now_ms()));
    rows.sort();
    rows.dedup();
    let open_ids: BTreeSet<&str> = work
        .iter()
        .filter_map(|(row, _)| row.get("id").and_then(Value::as_str))
        .collect();
    if rows.iter().all(|id| open_ids.contains(id.as_str())) {
        return (rows, Vec::new());
    }
    let prd_ids = prd_row_ids();
    let mut rows_matching_an_open_or_prd_id = Vec::with_capacity(rows.len());
    let mut rows_matching_no_open_or_prd_id = Vec::new();
    for id in rows {
        if open_ids.contains(id.as_str()) || prd_ids.contains(&id) {
            rows_matching_an_open_or_prd_id.push(id);
        } else {
            rows_matching_no_open_or_prd_id.push(id);
        }
    }
    (rows_matching_an_open_or_prd_id, rows_matching_no_open_or_prd_id)
}

pub fn spawn_ceiling(slots: &Value) -> usize {
    slots["ceiling"].as_u64().map_or(DEFAULT_SPAWN_CEILING, |ceiling| ceiling as usize)
}
