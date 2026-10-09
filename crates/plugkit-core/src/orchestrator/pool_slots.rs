use serde_json::{json, Value};

use super::transitions::prd_open_rows_with_recency;
use crate::pkfs;

pub const HEARTBEAT_LIVE_MS: u64 = 10 * 60 * 1000;
const HEARTBEAT_REAP_MS: u64 = 60 * 60 * 1000;
const CEILING_KEYWORDS: [&str; 3] = ["maximum", "ceiling", "limit"];

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn pool_dir(project_root: &str) -> String {
    format!("{}/.gm/pool", project_root)
}

fn heartbeat_field(body: &str, key: &str) -> Option<String> {
    body.lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
        .find_map(|line| line.strip_prefix(key))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_arch = "wasm32")]
fn remove_file(path: &str) -> bool {
    crate::wasm_dispatch::host_abi::host_remove_file_never_directory(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn remove_file(_path: &str) -> bool {
    false
}

struct LiveHeartbeats {
    count: usize,
    rows: Vec<String>,
    reaped: Vec<String>,
}

fn read_heartbeats(dir: &str, now: u64) -> LiveHeartbeats {
    let mut live = LiveHeartbeats { count: 0, rows: Vec::new(), reaped: Vec::new() };
    let Some(Value::Array(entries)) = pkfs::readdir(dir) else {
        return live;
    };
    for entry in entries {
        let name = match entry.as_str() {
            Some(bare) => bare.to_string(),
            None => match entry.get("name").and_then(Value::as_str) {
                Some(obj_name) => obj_name.to_string(),
                None => continue,
            },
        };
        let is_file = entry
            .get("is_file")
            .or_else(|| entry.get("isFile"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !is_file || !name.ends_with(".live") {
            continue;
        }
        let path = format!("{}/{}", dir, name);
        let mtime = pkfs::stat(&path).and_then(|s| {
            s.get("mtime_ms")
                .or_else(|| s.get("mtimeMs"))
                .and_then(Value::as_f64)
        });
        let Some(mtime_ms) = mtime.map(|m| m as u64) else {
            continue;
        };
        let age_ms = now.saturating_sub(mtime_ms);
        if age_ms > HEARTBEAT_REAP_MS {
            if remove_file(&path) {
                live.reaped.push(name.to_string());
            }
            continue;
        }
        if age_ms > HEARTBEAT_LIVE_MS {
            continue;
        }
        let Some(body) = pkfs::read_to_string(&path) else {
            continue;
        };
        if heartbeat_field(&body, "session:").is_none() {
            continue;
        }
        live.count += 1;
        if let Some(row) = heartbeat_field(&body, "row:") {
            live.rows.push(row);
        }
    }
    live.rows.sort();
    live
}

fn read_ceiling(dir: &str) -> Option<u64> {
    let body = pkfs::read_to_string(&format!("{}/ceiling.json", dir))?;
    serde_json::from_str::<Value>(&body).ok()?.get("ceiling")?.as_u64()
}

fn ceiling_from_refusal(text: &str) -> Option<u64> {
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

pub fn slot_state(project_root: &str) -> Value {
    let dir = pool_dir(project_root);
    let live = read_heartbeats(&dir, now_ms());
    let ceiling = read_ceiling(&dir);
    let free = ceiling.map(|c| c.saturating_sub(live.count as u64));
    let (blocker_entries, work): (Vec<(Value, usize)>, Vec<(Value, usize)>) = prd_open_rows_with_recency()
        .into_iter()
        .partition(|(row, _)| super::pool_rank::is_blocker_row(row));
    let blockers: Vec<Value> = blocker_entries.into_iter().map(|(row, _)| row).collect();
    let open_rows = work.len();
    let ranked = super::pool_rank::rank(&work, &blockers, &live.rows);
    let candidates = ranked["candidates"].clone();
    let action = match (open_rows, free) {
        (0, _) => "none",
        (_, Some(0)) => "hold",
        _ => "launch",
    };
    json!({
        "live": live.count,
        "live_rows": live.rows,
        "open_rows": open_rows,
        "blocker_rows": blockers.len(),
        "candidates": candidates,
        "supply": ranked["supply"].clone(),
        "ceiling": ceiling,
        "free": free,
        "action": action,
        "reaped_heartbeats": live.reaped,
    })
}

pub fn slots_prose(slots: &Value) -> String {
    match slots["action"].as_str() {
        Some("none") => "No open PRD rows: launch no workers.".to_string(),
        Some("hold") => "All slots are full: wait for a completion and relaunch its replacement in the same turn.".to_string(),
        _ => match slots["free"].as_u64() {
            Some(free) => {
                let launch = free.min(slots["candidates"].as_array().map_or(0, |c| c.len() as u64));
                format!("Free slots known: launch min(free, candidates.length) = {} gm-worker subagents now.", launch)
            }
            None => "Ceiling unknown: launch until a spawn refusal, then call pool-observe with that refusal text.".to_string(),
        },
    }
}

pub fn handle_observe(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let observed_live = body.get("live").and_then(Value::as_u64);
    let refusal = body
        .get("refusal")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dir = pool_dir(".");
    let refused_ceiling = refusal.as_deref().and_then(ceiling_from_refusal);
    let ceiling = refused_ceiling.or_else(|| read_ceiling(&dir));
    let record = json!({
        "ceiling": ceiling,
        "observed_live": observed_live,
        "refusal_text": refusal,
        "ts": now_ms(),
    });
    if !pkfs::write(&format!("{}/ceiling.json", dir), &record.to_string()) {
        return (String::new(), "pool-observe: could not write .gm/pool/ceiling.json".to_string(), 1);
    }
    let out = json!({
        "ok": true,
        "verb": "pool-observe",
        "refusal_ceiling": refused_ceiling,
        "slots": slot_state("."),
    });
    (out.to_string(), String::new(), 0)
}

const MONITOR_FLOOR: u64 = 10;
const MONITOR_ALARM_ACTION: &str = "refill from slots.candidates until a spawn refusal names the ceiling; when candidates run out, dispatch a traversal hop to log node-only PRDs; the loop is wait {\"ms\":60000}, then instruction, then launch the free slots";
const WORKER_BRIEF_PATH: &str = "C:/dev/spoint/.gm/config-source-cache-default/prose/worker.md";

pub fn monitor_block(slots: &Value) -> Value {
    let live = slots["live"].as_u64().unwrap_or(0);
    let open_work = slots["open_rows"].as_u64().unwrap_or(0);
    json!({
        "live": live,
        "floor": MONITOR_FLOOR,
        "open_work": open_work,
        "alarm": live < MONITOR_FLOOR && open_work > 0,
        "alarm_action": MONITOR_ALARM_ACTION,
    })
}

pub fn handle_brief(content: &str) -> (String, String, i32) {
    let Ok(body) = serde_json::from_str::<Value>(content) else {
        return (String::new(), "pool-brief: body must be JSON {row, session, role}".to_string(), 1);
    };
    let field = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (Some(row), Some(session), Some(role)) = (field("row"), field("session"), field("role")) else {
        return (String::new(), "pool-brief: body requires non-empty row, session and role".to_string(), 1);
    };
    if role != "resolver" && role != "traversal" {
        return (String::new(), format!("pool-brief: role must be resolver or traversal, got {}", role), 1);
    }
    let Some(template) = pkfs::read_to_string(WORKER_BRIEF_PATH) else {
        return (String::new(), format!("pool-brief: worker brief missing at {}", WORKER_BRIEF_PATH), 1);
    };
    let brief = template
        .replace("{row}", &row)
        .replace("{session}", &session)
        .replace("{role}", &role);
    (json!({"ok": true, "verb": "pool-brief", "brief": brief}).to_string(), String::new(), 0)
}
