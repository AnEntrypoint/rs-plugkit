use serde_json::{json, Value};

use super::super::pool_rank;
use super::super::transitions::prd_open_rows_with_recency;
use super::heartbeats::count_of_record_live;
use super::state::{read_ceiling, spawn_ceiling};
use super::{now_ms, pool_dir, DEFAULT_SPAWN_CEILING, REFILL_FLOOR};

const CEILING_KEYWORDS: [&str; 3] = ["maximum", "ceiling", "limit"];

pub(super) fn ceiling_from_refusal(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let after_keyword = CEILING_KEYWORDS
        .iter()
        .filter_map(|keyword| lower.find(*keyword).map(|at| at + keyword.len()))
        .min()?;
    lower[after_keyword..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

pub(super) fn floor_denial_text(verb: &str, live: u64, floor: u64, open_rows: u64, advertised: u64, shortfall: u64, launch: &str) -> String {
    format!(
        "{verb} refused: floor_gate_denied -- live={live} is under the floor of {floor} with open_rows={open_rows}; shortfall={shortfall}, advertised={advertised}. {launch} Then call pool-observe with body.live set to the ListAgents count and retry {verb}."
    )
}

pub(super) fn launch_instruction(advertised: &[String], shortfall: u64, route: &str) -> String {
    let count = advertised.len() as u64;
    if count == 0 {
        return format!(
            "No launch is advertised for shortfall={shortfall}: the pending-row scan left no admissible row in slots.candidates; route: {route}. slots.candidates_route carries the scan: skipped_by_rank names how many open rows are not launchable work (design_decision, outcome, refuted, blocker_notes, pending_blocker, live) and removed_by_gate names how many passed rank but lost a gate (dirty_target, git_status_unknown, already_fixed_on_head, live_writer); a gate removal while open work exists is a supply defect to report to gm."
        );
    }
    let names = advertised.join(", ");
    if count >= shortfall {
        format!("Launch exactly the {count} advertised ids from slots.launch now ({names}): they cover shortfall={shortfall}, so launching them reaches the floor.")
    } else {
        format!(
            "Launch exactly the {count} advertised ids from slots.launch now ({names}): they cover {count} of shortfall={shortfall}; no further admissible row exists in slots.candidates, which is a supply defect to report to gm."
        )
    }
}

fn open_work_rows(exclude_row: Option<&str>) -> u64 {
    prd_open_rows_with_recency()
        .into_iter()
        .filter(|(row, _)| !pool_rank::is_blocker_row(row))
        .filter(|(row, _)| exclude_row.map_or(true, |id| row.get("id").and_then(Value::as_str) != Some(id)))
        .count() as u64
}

pub fn floor_gate(verb: &str, body_live: Option<u64>, exclude_row: Option<&str>) -> Result<Value, Value> {
    let Some(live) = body_live.or_else(|| count_of_record_live(".", now_ms())) else {
        return Ok(json!("absent"));
    };
    let ceiling = read_ceiling(&pool_dir(".")).unwrap_or(DEFAULT_SPAWN_CEILING as u64);
    let floor = REFILL_FLOOR.min(ceiling);
    let open_rows = open_work_rows(exclude_row);
    if verb == "prd-resolve" || open_rows == 0 || live >= floor {
        return Ok(json!(live));
    }
    let refill_needed = floor - live;
    let launch = format!("Launch shortfall={refill_needed} gm-worker subagents from slots.launch of a pool-observe reply now.");
    let text = floor_denial_text(verb, live, floor, open_rows, refill_needed, refill_needed, &launch);
    Err(json!({
        "ok": false,
        "verb": verb,
        "error_code": "floor_gate_denied",
        "error": text,
        "live": live,
        "floor": floor,
        "ceiling": ceiling,
        "open_rows": open_rows,
        "refill_needed": refill_needed,
        "count_of_record": live,
    }))
}

const MONITOR_ALARM_ACTION: &str = "refill from launch (node-first candidates) in the same turn, one replacement per freed slot; when candidates run out, traversal is launched; loop: wait {\"ms\":60000}, then pool-observe with body.live (ListAgents count) and body.held, then launch";

pub fn monitor_block(slots: &Value) -> Value {
    let live = slots["live"].as_u64().unwrap_or(0);
    let open_work = slots["open_rows"].as_u64().unwrap_or(0);
    let floor = REFILL_FLOOR.min(spawn_ceiling(slots) as u64);
    let shortfall = if open_work > 0 { floor.saturating_sub(live) } else { 0 };
    json!({
        "live": live,
        "floor": floor,
        "open_work": open_work,
        "shortfall": shortfall,
        "alarm": shortfall > 0,
        "alarm_action": MONITOR_ALARM_ACTION,
    })
}
