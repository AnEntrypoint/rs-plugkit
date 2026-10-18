use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const CLAIM_LIVE_MS: u64 = 6 * 60 * 60 * 1000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn claim_file(cwd: Option<&str>) -> String {
    format!("{}/.gm/lane-claims.json", cwd.unwrap_or(".").trim_end_matches('/'))
}

fn load(cwd: Option<&str>) -> Vec<Value> {
    crate::pkfs::read_to_string(&claim_file(cwd))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("claims").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn save(cwd: Option<&str>, claims: &[Value]) -> bool {
    crate::pkfs::write(&claim_file(cwd), &json!({ "claims": claims }).to_string())
}

fn owner(claim: &Value) -> &str {
    claim.get("session_id").and_then(Value::as_str).unwrap_or("")
}

fn is_live(claim: &Value, now: u64) -> bool {
    let ts = claim.get("ts_ms").and_then(Value::as_u64).unwrap_or(0);
    now.saturating_sub(ts) <= CLAIM_LIVE_MS
}

fn claimed_paths(claim: &Value) -> Vec<String> {
    claim
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn normalize(path: &str) -> &str {
    let trimmed = path.trim().trim_start_matches("./").trim_end_matches('/');
    if trimmed == "." { "" } else { trimmed }
}

fn overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}

/// Live claims held by other sessions that overlap the publish scope. An empty
/// `paths` means a whole-index publish, which overlaps every foreign claim.
pub(super) fn lane_claim_conflicts(paths: &[String], session_id: &str, cwd: Option<&str>) -> Vec<Value> {
    if session_id.is_empty() {
        return Vec::new();
    }
    let now = now_ms();
    load(cwd)
        .iter()
        .filter(|claim| owner(claim) != session_id && is_live(claim, now))
        .filter_map(|claim| {
            let theirs = claimed_paths(claim);
            let overlap: Vec<String> = if paths.is_empty() {
                theirs.clone()
            } else {
                paths
                    .iter()
                    .filter(|p| theirs.iter().any(|t| overlaps(p, t)))
                    .cloned()
                    .collect()
            };
            (!overlap.is_empty()).then(|| json!({ "session_id": owner(claim), "paths": overlap }))
        })
        .collect()
}

pub(super) fn lane_claim_refusal(verb: &str, conflicts: &[Value]) -> Value {
    let summary: Vec<String> = conflicts
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .get("paths")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect())
                .unwrap_or_default();
            format!("{} held by {}", paths.join(", "), c.get("session_id").and_then(Value::as_str).unwrap_or("(unknown)"))
        })
        .collect();
    json!({
        "error": format!("{verb} refused: live lane claim(s) by other sessions overlap this publish scope: {}. Wait for that lane to publish, or pass paths that do not overlap", summary.join("; ")),
        "error_code": "lane_claim_conflict",
        "conflicts": conflicts,
    })
}

/// Replaces this session's claim with `paths`; other sessions' live claims are kept, stale ones dropped.
pub(super) fn lane_claim_record(paths: &[String], session_id: &str, cwd: Option<&str>) -> bool {
    if session_id.is_empty() || paths.is_empty() {
        return false;
    }
    let now = now_ms();
    let mut claims: Vec<Value> = load(cwd)
        .into_iter()
        .filter(|c| owner(c) != session_id && is_live(c, now))
        .collect();
    claims.push(json!({ "session_id": session_id, "paths": paths, "ts_ms": now }));
    save(cwd, &claims)
}

/// Drops this session's claim once its publish is underway; the lane owns those paths only until then.
pub(super) fn lane_claim_release(session_id: &str, cwd: Option<&str>) {
    if session_id.is_empty() {
        return;
    }
    let now = now_ms();
    let before = load(cwd);
    let after: Vec<Value> = before
        .iter()
        .filter(|c| owner(c) != session_id && is_live(c, now))
        .cloned()
        .collect();
    if after.len() != before.len() {
        save(cwd, &after);
    }
}
