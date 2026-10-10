use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::transitions::prd_open_rows_with_recency;
use crate::pkfs;

pub const HEARTBEAT_LIVE_MS: u64 = 5 * 60 * 1000;
const HEARTBEAT_REAP_MS: u64 = 60 * 60 * 1000;
const DEFAULT_SPAWN_CEILING: usize = 20;
const CEILING_KEYWORDS: [&str; 3] = ["maximum", "ceiling", "limit"];
const REFILL_FLOOR: u64 = 12;
const LAUNCH_ID_PREFIX: &str = "witness-gap-";
const LAUNCH_ID_EXCLUDED_SEGMENTS: [&str; 3] = ["-blocker-", "-finding-", "-defect-"];

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

#[derive(Default)]
struct HeartbeatIdentity {
    session: Option<String>,
    rows: Vec<String>,
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn row_names(value: &str) -> Vec<String> {
    value.split(',').filter_map(non_empty).collect()
}

fn is_positional_row(line: &str) -> bool {
    let bytes = line.as_bytes();
    let dated = bytes.len() >= 10
        && bytes[..10]
            .iter()
            .enumerate()
            .all(|(index, byte)| if index == 4 || index == 7 { *byte == b'-' } else { byte.is_ascii_digit() });
    !line.contains(char::is_whitespace) && !line.contains([':', '=']) && !dated
}

fn heartbeat_identity(body: &str) -> HeartbeatIdentity {
    let mut identity = HeartbeatIdentity::default();
    let lines: Vec<&str> = body
        .lines()
        .map(|raw| raw.trim().trim_start_matches('\u{feff}').trim())
        .filter(|line| !line.is_empty())
        .collect();
    let lead_token = lines
        .first()
        .and_then(|line| line.split_whitespace().next())
        .filter(|token| !token.contains([':', '=']))
        .and_then(non_empty);
    let bare_format = lead_token.is_some();
    for line in &lines {
        if let Some(name) = line.strip_prefix("session:").and_then(non_empty) {
            identity.session = identity.session.or(Some(name));
        }
        if let Some(value) = line.strip_prefix("row:") {
            identity.rows.extend(row_names(value));
        }
        for token in line.split_whitespace() {
            if let Some(value) = token.strip_prefix("session=") {
                identity.session = identity.session.or_else(|| non_empty(value));
            } else if let Some(value) = token.strip_prefix("row=") {
                identity.rows.extend(row_names(value));
            }
        }
    }
    let words: Vec<&str> = lines.iter().copied().flat_map(|line| line.split_whitespace()).collect();
    for pair in words.windows(2) {
        if pair[0] == "row" && is_positional_row(pair[1]) {
            identity.rows.extend(row_names(pair[1]));
        }
    }
    identity.session = identity.session.or(lead_token);
    if identity.rows.is_empty() && bare_format {
        if let Some(second) = lines.get(1).copied().filter(|line| is_positional_row(line)) {
            identity.rows = row_names(second);
        }
    }
    identity
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
    sessions: Vec<String>,
    rows: Vec<String>,
    words: BTreeSet<String>,
    reaped: Vec<String>,
}

fn read_heartbeats(dir: &str, now: u64) -> LiveHeartbeats {
    let mut live = LiveHeartbeats {
        count: 0,
        sessions: Vec::new(),
        rows: Vec::new(),
        words: BTreeSet::new(),
        reaped: Vec::new(),
    };
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
        let body = pkfs::read_to_string(&path).unwrap_or_default();
        let identity = heartbeat_identity(&body);
        live.words.extend(
            body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .filter(|word| !word.is_empty())
                .map(str::to_string),
        );
        live.count += 1;
        if let Some(session) = identity.session {
            live.sessions.push(session);
        }
        live.rows.extend(identity.rows);
    }
    live.sessions.sort();
    live.rows.sort();
    live.rows.dedup();
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

fn is_launch_row(row: &Value) -> bool {
    let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
    let status = row.get("status").and_then(Value::as_str).unwrap_or("pending");
    id.starts_with(LAUNCH_ID_PREFIX)
        && !LAUNCH_ID_EXCLUDED_SEGMENTS.iter().any(|segment| id.contains(*segment))
        && status == "pending"
        && !super::pool_rank::has_blocker_notes(row)
        && super::pool_rank::arm(row).is_none()
}

fn held_rows(live: &LiveHeartbeats, work: &[(Value, usize)]) -> Vec<String> {
    let mut rows = live.rows.clone();
    rows.extend(
        work.iter()
            .filter_map(|(row, _)| row.get("id").and_then(Value::as_str))
            .filter(|id| live.words.contains(*id))
            .map(str::to_string),
    );
    rows.sort();
    rows.dedup();
    rows
}

pub fn slot_state(project_root: &str) -> Value {
    slot_parts(project_root).0
}

fn slot_parts(project_root: &str) -> (Value, Vec<String>) {
    let dir = pool_dir(project_root);
    let live = read_heartbeats(&dir, now_ms());
    let ceiling = read_ceiling(&dir);
    let free = ceiling.map(|c| c.saturating_sub(live.count as u64));
    let (blocker_entries, work): (Vec<(Value, usize)>, Vec<(Value, usize)>) = prd_open_rows_with_recency()
        .into_iter()
        .partition(|(row, _)| super::pool_rank::is_blocker_row(row));
    let blockers: Vec<Value> = blocker_entries.into_iter().map(|(row, _)| row).collect();
    let open_rows = work.len();
    let live_rows = held_rows(&live, &work);
    let ranked = super::pool_rank::rank(&work, &blockers, &live_rows);
    let candidates = ranked["candidates"].clone();
    let launchable: BTreeSet<&str> = work
        .iter()
        .filter(|(row, _)| is_launch_row(row))
        .filter_map(|(row, _)| row.get("id").and_then(Value::as_str))
        .collect();
    let node_candidates: Vec<String> = ranked["node_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().filter(|id| launchable.contains(id)).map(str::to_string))
        .collect();
    let action = match (open_rows, free) {
        (0, _) => "none",
        (_, Some(0)) => "hold",
        _ => "launch",
    };
    let slots = json!({
        "live": live.count,
        "live_rows": live_rows,
        "live_sessions": live.sessions,
        "open_rows": open_rows,
        "blocker_rows": blockers.len(),
        "candidates": candidates,
        "supply": ranked["supply"].clone(),
        "ceiling": ceiling,
        "free": free,
        "action": action,
        "reaped_heartbeats": live.reaped,
    });
    (slots, node_candidates)
}

pub fn spawn_ceiling(slots: &Value) -> usize {
    slots["ceiling"].as_u64().map_or(DEFAULT_SPAWN_CEILING, |ceiling| ceiling as usize)
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
    let (slots, node_candidates) = slot_parts(".");
    let live = slots["live"].as_u64().unwrap_or(0);
    let ceiling = spawn_ceiling(&slots) as u64;
    let refill_needed = REFILL_FLOOR.saturating_sub(live);
    let launch_cap = refill_needed.min(ceiling.saturating_sub(live)) as usize;
    let launch: Vec<Value> = node_candidates
        .into_iter()
        .take(launch_cap)
        .map(|id| json!({"id": id}))
        .collect();
    let out = json!({
        "ok": true,
        "verb": "pool-observe",
        "refusal_ceiling": refused_ceiling,
        "floor": REFILL_FLOOR,
        "ceiling": ceiling,
        "refill_needed": refill_needed,
        "launch": launch,
        "slots": slots,
    });
    (out.to_string(), String::new(), 0)
}

const MONITOR_ALARM_ACTION: &str = "refill from slots.candidates until a spawn refusal names the ceiling; when candidates run out, dispatch a traversal hop to log node-only PRDs; the loop is wait {\"ms\":60000}, then instruction, then launch the free slots";
const WORKER_BRIEF_PATH: &str = "C:/dev/spoint/.gm/config-source-cache-default/prose/worker.md";

pub fn monitor_block(slots: &Value) -> Value {
    let live = slots["live"].as_u64().unwrap_or(0);
    let open_work = slots["open_rows"].as_u64().unwrap_or(0);
    let shortfall = if open_work > 0 { REFILL_FLOOR.saturating_sub(live) } else { 0 };
    json!({
        "live": live,
        "floor": REFILL_FLOOR,
        "open_work": open_work,
        "shortfall": shortfall,
        "alarm": shortfall > 0,
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
        let missing: Vec<&str> = ["row", "session", "role"]
            .into_iter()
            .filter(|&key| field(key).is_none())
            .collect();
        return (
            String::new(),
            format!(
                "pool-brief: body must be JSON with non-empty string fields row (a PRD row id), session (the subagent's SESSION_ID; the key is spelled session, not session_id) and role (one of: resolver, traversal). Missing or empty: {}",
                missing.join(", ")
            ),
            1,
        );
    };
    if role != "resolver" && role != "traversal" {
        return (String::new(), format!("pool-brief: role must be resolver or traversal, got {}", role), 1);
    }
    let Some(template) = pkfs::read_to_string(WORKER_BRIEF_PATH) else {
        return (String::new(), format!("pool-brief: worker brief missing at {}", WORKER_BRIEF_PATH), 1);
    };
    let brief = template
        .replace("\r\n", "\n")
        .replace("{row}", &row)
        .replace("{session}", &session)
        .replace("{role}", &role);
    (json!({"ok": true, "verb": "pool-brief", "brief": brief}).to_string(), String::new(), 0)
}
