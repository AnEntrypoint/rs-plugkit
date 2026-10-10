use serde_json::{json, Value};
use std::collections::BTreeSet;

use super::pool_dir;
use crate::pkfs;

pub const HEARTBEAT_REFRESH_MS: u64 = 5 * 60 * 1000;
pub const HEARTBEAT_LIVE_MS: u64 = 10 * 60 * 1000;
const HEARTBEAT_REAP_MS: u64 = 60 * 60 * 1000;
const COUNT_OF_RECORD_FILE: &str = "count-of-record.json";
const COUNT_OF_RECORD_TTL_MS: u64 = 5 * 60 * 1000;
pub(super) const HELD_ROWS_FILE: &str = "held-rows.json";
const HELD_ROWS_TTL_MS: u64 = 30 * 60 * 1000;

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

fn required_key(key: &str) -> Option<&'static str> {
    match normalised_key(key).as_str() {
        "session" | "sessionid" => Some("session"),
        "row" => Some("row"),
        "start" => Some("start"),
        _ => None,
    }
}

pub(super) const HEARTBEAT_REQUIRED_LINES: [&str; 3] = ["session", "row", "start"];

fn heartbeat_missing_lines(body: &str) -> Vec<&'static str> {
    let mut present: BTreeSet<&'static str> = BTreeSet::new();
    if let Ok(record) = serde_json::from_str::<Value>(body.trim_start_matches('\u{feff}').trim()) {
        if let Some(fields) = record.as_object() {
            if ["session_id", "session", "sessionId"]
                .iter()
                .any(|key| fields.get(*key).and_then(Value::as_str).map_or(false, |v| !v.trim().is_empty()))
            {
                present.insert("session");
            }
            if ["row", "row_id"].iter().any(|key| fields.get(*key).map_or(false, |value| !value.is_null())) {
                present.insert("row");
            }
            if fields.get("start").and_then(Value::as_str).map_or(false, |v| !v.trim().is_empty()) {
                present.insert("start");
            }
        }
    }
    for line in body.lines() {
        let line = line.trim().trim_start_matches('\u{feff}').trim();
        let mut keys: Vec<(&str, &str)> = Vec::new();
        if let Some((key, value)) = line.split_once(':') {
            keys.push((key, value));
        }
        for token in line.split_whitespace() {
            if let Some((key, value)) = token.split_once('=') {
                keys.push((key, value));
            }
        }
        for (key, value) in keys {
            if value.trim().is_empty() {
                continue;
            }
            if let Some(required) = required_key(key) {
                present.insert(required);
            }
        }
    }
    HEARTBEAT_REQUIRED_LINES.iter().filter(|line| !present.contains(*line)).copied().collect()
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

pub(super) struct LiveHeartbeats {
    pub(super) count: usize,
    pub(super) sessions: Vec<String>,
    pub(super) rows: Vec<String>,
    pub(super) words: BTreeSet<String>,
    pub(super) reaped: Vec<String>,
    pub(super) aging: Vec<Value>,
    pub(super) malformed: Vec<Value>,
}

pub(super) fn read_heartbeats(dir: &str, now: u64) -> LiveHeartbeats {
    let mut live = LiveHeartbeats {
        count: 0,
        sessions: Vec::new(),
        rows: Vec::new(),
        words: BTreeSet::new(),
        reaped: Vec::new(),
        aging: Vec::new(),
        malformed: Vec::new(),
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
        let missing = heartbeat_missing_lines(&body);
        if !missing.is_empty() {
            live.malformed.push(json!({
                "file": name.clone(),
                "session": identity.session.clone(),
                "missing": missing,
                "fix": "the heartbeat is exactly three lines: session: <session> / row: <row id> / start: <ISO-8601 UTC>"
            }));
        }
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

pub(super) fn count_of_record_live(project_root: &str, now: u64) -> Option<u64> {
    let body = pkfs::read_to_string(&format!("{}/{}", pool_dir(project_root), COUNT_OF_RECORD_FILE))?;
    let record: Value = serde_json::from_str(&body).ok()?;
    let ts = record.get("ts").and_then(Value::as_u64)?;
    if now.saturating_sub(ts) > COUNT_OF_RECORD_TTL_MS {
        return None;
    }
    record.get("live").and_then(Value::as_u64)
}

pub(super) fn write_count_of_record(dir: &str, live: u64, now: u64) -> bool {
    let record = json!({"live": live, "ts": now, "source": "pool-observe body.live"});
    pkfs::write(&format!("{}/{}", dir, COUNT_OF_RECORD_FILE), &record.to_string())
}

pub(super) fn declared_holds(dir: &str, now: u64) -> Vec<String> {
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

pub(super) fn held_rows(live: &LiveHeartbeats, work: &[(Value, usize)]) -> Vec<String> {
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
