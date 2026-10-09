use serde_json::{json, Value};

use super::transitions::prd_open_rows;
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

fn parse_row(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.trim().strip_prefix("row:"))
        .map(|row| row.trim().to_string())
        .filter(|row| !row.is_empty())
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
        live.count += 1;
        if let Some(row) = pkfs::read_to_string(&path).and_then(|body| parse_row(&body)) {
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

fn is_blocker_row(row: &Value) -> bool {
    row.get("id").and_then(Value::as_str).is_some_and(super::pool_rank::is_blocker_id)
}

pub fn slot_state(project_root: &str) -> Value {
    let dir = pool_dir(project_root);
    let live = read_heartbeats(&dir, now_ms());
    let ceiling = read_ceiling(&dir);
    let free = ceiling.map(|c| c.saturating_sub(live.count as u64));
    let (blockers, work): (Vec<Value>, Vec<Value>) =
        prd_open_rows().into_iter().partition(is_blocker_row);
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
