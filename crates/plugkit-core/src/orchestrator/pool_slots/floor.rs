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

pub(super) fn floor_denial_text(verb: &str, live: u64, floor: u64, open_rows: u64, advertised: u64, shortfall: u64) -> String {
    let launch = if advertised == 0 {
        format!("No launch is advertised: launch shortfall={shortfall} gm-worker subagents from slots.candidates (node-first, then any open row) now.")
    } else if advertised < shortfall {
        format!(
            "Launch the {advertised} advertised gm-worker subagents from slots.launch now, then launch the other {} from slots.candidates (node-first, then any open row).",
            shortfall - advertised
        )
    } else {
        format!("Launch shortfall={shortfall} gm-worker subagents from slots.launch of a pool-observe reply now.")
    };
    format!(
        "{verb} refused: floor_gate_denied -- live={live} is under the floor of {floor} with open_rows={open_rows}; shortfall={shortfall}, advertised={advertised}. {launch} Then call pool-observe with body.live set to the ListAgents count and retry {verb}."
    )
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
    let text = floor_denial_text(verb, live, floor, open_rows, refill_needed, refill_needed);
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
