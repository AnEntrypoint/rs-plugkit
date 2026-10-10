use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};

use super::transitions::prd_open_rows_with_recency;
use crate::pkfs;

pub const HEARTBEAT_REFRESH_MS: u64 = 5 * 60 * 1000;
pub const HEARTBEAT_LIVE_MS: u64 = 10 * 60 * 1000;
const HEARTBEAT_REAP_MS: u64 = 60 * 60 * 1000;
const DEFAULT_SPAWN_CEILING: usize = 20;
const CEILING_KEYWORDS: [&str; 3] = ["maximum", "ceiling", "limit"];
const REFILL_FLOOR: u64 = 12;
const LAUNCH_ID_PREFIX: &str = "witness-gap-";
const LAUNCH_ID_EXCLUDED_SEGMENTS: [&str; 3] = ["-blocker-", "-finding-", "-defect-"];
const HELD_ROWS_FILE: &str = "held-rows.json";
const HELD_ROWS_TTL_MS: u64 = 30 * 60 * 1000;
const TRAVERSAL_SUPPLY_FACTOR: u64 = 2;
const TRAVERSAL_LAUNCH_ID: &str = "traversal-node-supply";

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
    value
        .split(',')
        .filter_map(non_empty)
        .filter(|name| !name.contains(char::is_whitespace))
        .collect()
}

fn json_heartbeat_identity(body: &str) -> Option<HeartbeatIdentity> {
    let record: Value = serde_json::from_str(body.trim_start_matches('\u{feff}').trim()).ok()?;
    let fields = record.as_object()?;
    let mut identity = HeartbeatIdentity::default();
    identity.session = ["session_id", "session", "sessionId"]
        .iter()
        .find_map(|key| fields.get(*key).and_then(Value::as_str).and_then(non_empty));
    for key in ["row", "row_id"] {
        match fields.get(key) {
            Some(Value::String(text)) => identity.rows.extend(row_names(text)),
            Some(Value::Array(items)) => identity
                .rows
                .extend(items.iter().filter_map(Value::as_str).flat_map(|text| row_names(text))),
            _ => {}
        }
    }
    Some(identity)
}

fn normalised_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn absorb_identity_field(identity: &mut HeartbeatIdentity, key: &str, value: &str) {
    match normalised_key(key).as_str() {
        "session" | "sessionid" => {
            if identity.session.is_none() {
                identity.session = non_empty(value);
            }
        }
        "row" => identity.rows.extend(row_names(value)),
        _ => {}
    }
}

fn heartbeat_identity(body: &str) -> HeartbeatIdentity {
    if let Some(identity) = json_heartbeat_identity(body) {
        return identity;
    }
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
    for line in &lines {
        if let Some((key, value)) = line.split_once(':') {
            absorb_identity_field(&mut identity, key, value);
        }
        for token in line.split_whitespace() {
            if let Some((key, value)) = token.split_once('=') {
                absorb_identity_field(&mut identity, key, value);
            }
        }
    }
    identity.session = identity.session.or(lead_token);
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
    aging: Vec<Value>,
}

fn read_heartbeats(dir: &str, now: u64) -> LiveHeartbeats {
    let mut live = LiveHeartbeats {
        count: 0,
        sessions: Vec::new(),
        rows: Vec::new(),
        words: BTreeSet::new(),
        reaped: Vec::new(),
        aging: Vec::new(),
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
        if age_ms > HEARTBEAT_REFRESH_MS {
            live.aging.push(json!({
                "file": name.clone(),
                "session": identity.session.clone(),
                "age_s": age_ms / 1000
            }));
        }
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

fn module_path_of(row: &Value) -> Option<String> {
    ["subject", "witness", "why", "title", "acceptance", "acceptance_criteria"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_str))
        .flat_map(str::split_whitespace)
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | ',' | ';' | '(' | ')' | '[' | ']'))
                .trim_end_matches('.')
        })
        .map(|token| token.split(':').next().unwrap_or_default())
        .find(|token| {
            token.contains('/')
                && !token.starts_with('/')
                && !token.contains("..")
                && MODULE_EXTENSIONS.iter().any(|extension| token.ends_with(*extension))
        })
        .map(str::to_string)
}

const MODULE_EXTENSIONS: [&str; 6] = [".js", ".mjs", ".cjs", ".ts", ".jsx", ".tsx"];

fn node_only_witness(row: &Value) -> bool {
    ["subject", "witness", "why", "title", "acceptance", "acceptance_criteria", "text"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_str))
        .all(|text| !text.to_ascii_lowercase().contains("cargo"))
}

fn witness_gap_admitted(row: &Value, project_root: &str) -> bool {
    let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
    let status = row.get("status").and_then(Value::as_str).unwrap_or("pending");
    id.starts_with(LAUNCH_ID_PREFIX)
        && !LAUNCH_ID_EXCLUDED_SEGMENTS.iter().any(|segment| id.contains(*segment))
        && status == "pending"
        && !super::pool_rank::has_blocker_notes(row)
        && node_only_witness(row)
        && node_only_module(row, project_root)
}

fn node_only_module(row: &Value, project_root: &str) -> bool {
    module_path_of(row).is_some_and(|module| {
        pkfs::read_to_string(&format!("{}/{}", project_root, module))
            .is_some_and(|source| !super::pool_rank::references_browser_or_gpu_global(&source))
    })
}

fn declared_holds(dir: &str, now: u64) -> Vec<String> {
    let Some(text) = pkfs::read_to_string(&format!("{}/{}", dir, HELD_ROWS_FILE)) else {
        return Vec::new();
    };
    let Ok(record) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let fresh = record
        .get("ts")
        .and_then(Value::as_u64)
        .is_some_and(|ts| now.saturating_sub(ts) <= HELD_ROWS_TTL_MS);
    if !fresh {
        return Vec::new();
    }
    record
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.as_str().map(str::to_string))
        .collect()
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
    slot_parts(project_root, None).0
}

fn slot_parts(project_root: &str, observed_live: Option<u64>) -> (Value, Vec<String>) {
    let dir = pool_dir(project_root);
    let live = read_heartbeats(&dir, now_ms());
    let heartbeat_live = live.count as u64;
    let live_count = observed_live.unwrap_or(heartbeat_live);
    let live_source = if observed_live.is_some() { "listagents" } else { "heartbeats" };
    let ceiling = read_ceiling(&dir);
    let free = ceiling.map(|c| c.saturating_sub(live_count));
    let (blocker_entries, work): (Vec<(Value, usize)>, Vec<(Value, usize)>) = prd_open_rows_with_recency()
        .into_iter()
        .partition(|(row, _)| super::pool_rank::is_blocker_row(row));
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
    let ranked = super::pool_rank::rank(&work, &blockers, &live_rows, &admitted);
    let candidates = ranked["candidates"].clone();
    let node_candidates: Vec<String> = ranked["node_candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str())
        .filter(|id| {
            work.iter()
                .find(|(row, _)| row.get("id").and_then(Value::as_str) == Some(*id))
                .is_some_and(|(row, _)| {
                    admitted.contains(*id)
                        && super::pool_rank::node_arm(row, true).is_none()
                        && node_only_module(row, project_root)
                })
        })
        .map(str::to_string)
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
    let (mut slots, node_candidates) = slot_parts(".", observed_live);
    let limit = body
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map_or(DEFAULT_LIST_LIMIT, |n| n as usize);
    cap_candidates(&mut slots, limit);
    for key in ["live_rows", "live_sessions"] {
        cap_list(&mut slots, key, limit);
    }
    let held = slots["live_rows_total"].as_u64().unwrap_or(0);
    let live = slots["live"].as_u64().unwrap_or(0);
    let ceiling = spawn_ceiling(&slots) as u64;
    let open_rows = slots["open_rows"].as_u64().unwrap_or(0);
    let refill_needed = if open_rows > 0 { REFILL_FLOOR.saturating_sub(live) } else { 0 };
    let launch_cap = refill_needed.min(ceiling.saturating_sub(live)) as usize;
    let now = now_ms();
    let state_dir = pool_dir(".");
    let mut surface_state = read_surface_state(&state_dir);
    if let Some(names) = body.get("scanned_surfaces").and_then(Value::as_array) {
        for name in names.iter().filter_map(Value::as_str) {
            surface_state.insert(name.to_string(), now);
        }
    }
    let unscanned = unscanned_surfaces(&list_surfaces("."), &surface_state, now);
    let node_supply = node_candidates.len() as u64;
    let traversal_threshold = TRAVERSAL_SUPPLY_FACTOR * REFILL_FLOOR;
    let traversal_needed = node_supply < traversal_threshold && !unscanned.is_empty();
    let next_surface = unscanned.first().cloned();
    let mut launch: Vec<Value> = node_candidates
        .into_iter()
        .take(launch_cap)
        .map(|id| json!({"id": id, "role": "resolver"}))
        .collect();
    if traversal_needed && launch.len() < launch_cap {
        if let Some(surface) = &next_surface {
            surface_state.insert(surface.clone(), now);
        }
        launch.push(json!({"id": TRAVERSAL_LAUNCH_ID, "role": "traversal", "surface": next_surface}));
    }
    write_surface_state(&state_dir, &surface_state);
    let traversal = json!({
        "needed": traversal_needed,
        "node_witness_candidates": node_supply,
        "threshold": traversal_threshold,
        "unscanned_surfaces": unscanned.iter().take(TRAVERSAL_SURFACE_SHOWN).collect::<Vec<&String>>(),
        "unscanned_total": unscanned.len(),
        "rule": "traversal is needed only while node witness candidates are below 2 x floor and unscanned surfaces remain; a launched traversal hop is assigned the first unscanned surface, and a surface stays scanned for 6 hours",
    });
    let mut monitor = monitor_block(&slots);
    if monitor["alarm"] == json!(false) {
        if let Some(fields) = monitor.as_object_mut() {
            fields.remove("alarm_action");
        }
    }
    let mut out = json!({
        "ok": true,
        "verb": "pool-observe",
        "refusal_ceiling": refused_ceiling,
        "floor": REFILL_FLOOR,
        "ceiling": ceiling,
        "held": held,
        "refill_needed": refill_needed,
        "launch": launch,
        "traversal": traversal,
        "monitor": monitor,
        "slots": slots,
    });
    if body.get("rules").and_then(Value::as_bool) == Some(true) {
        out["rules"] = json!(POOL_RULES);
    } else {
        out["rules_hint"] = json!("pass body.rules=true for the pool rules");
    }
    if body.get("blocker_notes").and_then(Value::as_bool) == Some(true) {
        out["blocker_notes"] = blocker_notes_block(limit);
    }
    (out.to_string(), String::new(), 0)
}

const TRAVERSAL_SURFACE_ROOTS: [&str; 3] = ["src", "apps", "client"];
const TRAVERSAL_SURFACE_TTL_MS: u64 = 6 * 60 * 60 * 1000;
const TRAVERSAL_SURFACE_SHOWN: usize = 8;
const TRAVERSAL_SURFACES_FILE: &str = "traversal-surfaces.json";

fn list_surfaces(project_root: &str) -> Vec<String> {
    let mut surfaces = Vec::new();
    for root in TRAVERSAL_SURFACE_ROOTS {
        let Some(Value::Array(entries)) = pkfs::readdir(&format!("{}/{}", project_root, root)) else {
            continue;
        };
        for entry in entries {
            let name = match entry.as_str() {
                Some(bare) => bare.to_string(),
                None => match entry.get("name").and_then(Value::as_str) {
                    Some(obj_name) => obj_name.to_string(),
                    None => continue,
                },
            };
            let is_dir = match entry.get("is_file").or_else(|| entry.get("isFile")).and_then(Value::as_bool) {
                Some(is_file) => !is_file,
                None => !name.contains('.'),
            };
            if is_dir {
                surfaces.push(format!("{}/{}", root, name));
            }
        }
    }
    surfaces.sort();
    surfaces
}

fn read_surface_state(dir: &str) -> std::collections::BTreeMap<String, u64> {
    let mut state = std::collections::BTreeMap::new();
    let Some(text) = pkfs::read_to_string(&format!("{}/{}", dir, TRAVERSAL_SURFACES_FILE)) else {
        return state;
    };
    let Ok(record) = serde_json::from_str::<Value>(&text) else {
        return state;
    };
    if let Some(scanned) = record.get("scanned").and_then(Value::as_object) {
        for (name, ts) in scanned {
            if let Some(ts) = ts.as_u64() {
                state.insert(name.clone(), ts);
            }
        }
    }
    state
}

fn write_surface_state(dir: &str, state: &std::collections::BTreeMap<String, u64>) -> bool {
    pkfs::write(&format!("{}/{}", dir, TRAVERSAL_SURFACES_FILE), &json!({"scanned": state}).to_string())
}

fn unscanned_surfaces(surfaces: &[String], state: &std::collections::BTreeMap<String, u64>, now: u64) -> Vec<String> {
    surfaces
        .iter()
        .filter(|surface| match state.get(*surface) {
            Some(ts) => now.saturating_sub(*ts) > TRAVERSAL_SURFACE_TTL_MS,
            None => true,
        })
        .cloned()
        .collect()
}

const DEFAULT_LIST_LIMIT: usize = 12;

fn blocker_notes_block(limit: usize) -> Value {
    let rows: Vec<Value> = prd_open_rows_with_recency()
        .into_iter()
        .map(|(row, _)| row)
        .filter(|row| super::pool_rank::has_blocker_notes(row))
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

const MONITOR_ALARM_ACTION: &str = "refill from launch (node-first candidates) in the same turn, one replacement per freed slot; when candidates run out, traversal is launched; loop: wait {\"ms\":60000}, then pool-observe with body.live (ListAgents count) and body.held, then launch";

const POOL_RULES: [&str; 6] = [
    "Floor 12: while open_rows > 0 keep live at or above 12; refill_needed = 12 - live.",
    "Pass the ListAgents count of running subagents as body.live on every call: that count is the count of record. slots.live_heartbeats is only the heartbeat cross-check.",
    "A live count under 12 while open_rows > 0 is a FAILURE: append `FAILURE: <UTC timestamp> live count fell to <live> with <open_rows> pending rows` to .gm/witness-log.md, then refill in the same turn.",
    "Refill on every completion, in the same turn: launch one replacement per freed slot from launch (node-first candidates). Never launch a row that is in slots.live_rows.",
    "Pass body.held = the row id of every running worker on every call, including a worker whose heartbeat is not written yet. The newest held list is kept for 30 minutes; send held: [] to clear it.",
    "A heartbeat refreshes at least every 5 minutes and counts as live for 10 minutes; one older than 5 minutes is listed in slots.aging_heartbeats, not dropped. On a spawn refusal, call pool-observe with body.refusal set to the refusal text.",
];
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
