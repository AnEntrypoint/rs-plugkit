use super::prd::{clamp_chars, one_line, request_flag, request_object, row_text, status_is_open};
use super::row_store::{self, Store};
use super::session_liveness::{ActivityProbe, SessionEvidence};
use super::state::{dispatch_session_id, now_ms};
use super::yaml_util::yaml_to_json;
use crate::pkfs;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const DEFAULT_OLDER_THAN_DAYS: u64 = 14;
const DEFAULT_IDLE_SECS: u64 = 3600;
const DEFAULT_ARCHIVE_LIMIT: usize = 100;
const LISTED_ROWS_MAX: usize = 200;
const LISTED_TITLE_CHARS: usize = 100;
const MS_PER_DAY: u64 = 86_400_000;
const REASON_EXTERNAL: &str = "external_stale";
const REASON_SESSION: &str = "session_gone";
const ARCHIVE_STATUS: &str = "archived";

pub const HELP: &str = "\
prd-gc {older_than_days, idle_secs, reasons, confirm, limit}.
  Lists the open PRD rows that are safe to retire and, with confirm:true, retires them. Review is the
  default: nothing is written without confirm:true.
  external_stale: the row is blockedBy external or out-of-reach and its row file was last written
                  more than older_than_days days ago (default 14). A PRD row carries no creation
                  time, so age is the time since the row file was last written; a checkout or an
                  edit resets it. The reply names this as age_basis.
  session_gone:   the row names a session_id other than the caller's, and that session shows no sign
                  of life: no live session lease among the leases the host passes in
                  (_lease_sessions) and no spool activity in the last idle_secs seconds (default
                  3600). Such a row can hold real open work: review the list before confirming.
  reasons: an array that narrows the set, for example [\"external_stale\"]; the default is both.
  A row of the caller's own session is never listed as session_gone. A row with a closed status is
  never listed.
  confirm:true moves at most limit rows (default 100, oldest first) into the append-only commit log
  .gm/prd-log.jsonl with status archived and a note that names the reasons, then removes the row
  file. The whole row stays in the log. A row that changed after the scan is skipped, never moved.
  reply: mode (review or archive), counts {rows_open, candidates, external_stale, session_gone,
         archived, skipped, left}, rows {id, title, status, reasons, session_id, blockedBy,
         age_days}, sessions (the evidence per session), archived, skipped, age_basis.
  help:true returns this text and writes no state.";

struct Policy {
    older_than_ms: u64,
    reasons: Vec<String>,
    caller: Option<String>,
}

impl Policy {
    fn allows(&self, reason: &str) -> bool {
        self.reasons.iter().any(|allowed| allowed == reason)
    }
}

struct Candidate {
    id: String,
    row: Value,
    reasons: Vec<&'static str>,
    age_ms: Option<u64>,
}

fn number_field(request: &Value, key: &str) -> Option<u64> {
    request.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
    })
}

fn requested_reasons(request: &Value) -> Result<Vec<String>, String> {
    let Some(listed) = request.get("reasons").and_then(Value::as_array) else {
        return Ok(vec![REASON_EXTERNAL.to_string(), REASON_SESSION.to_string()]);
    };
    let mut reasons = Vec::new();
    for item in listed {
        match item.as_str() {
            Some(REASON_EXTERNAL) => reasons.push(REASON_EXTERNAL.to_string()),
            Some(REASON_SESSION) => reasons.push(REASON_SESSION.to_string()),
            _ => {
                return Err(format!(
                    "prd-gc reasons accepts {REASON_EXTERNAL} and {REASON_SESSION}, got {item}"
                ))
            }
        }
    }
    Ok(reasons)
}

fn is_external_marker(value: &Value) -> bool {
    matches!(value.as_str(), Some("external") | Some("out-of-reach"))
}

fn blocked_externally(row: &Value) -> bool {
    match row.get("blockedBy") {
        Some(Value::Array(items)) => items.iter().any(is_external_marker),
        Some(other) => is_external_marker(other),
        None => false,
    }
}

fn row_session(row: &Value) -> Option<String> {
    row.get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn mtime_ms(path: &str) -> Option<u64> {
    let stat = pkfs::stat(path).filter(|found| !found.is_null())?;
    let mtime = stat.get("mtime_ms").or_else(|| stat.get("mtimeMs"))?.as_f64()?;
    Some(mtime as u64)
}

fn row_age_ms(store: &Store, id: &str, now: u64) -> Option<u64> {
    let written = mtime_ms(&row_store::item_path(store, id)).or_else(|| mtime_ms(&store.legacy))?;
    Some(now.saturating_sub(written))
}

fn latest_rows(blocks: &[serde_yaml::Value]) -> Vec<(String, Value)> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: BTreeMap<String, Value> = BTreeMap::new();
    for block in blocks {
        let Some(id) = row_store::block_id(block) else {
            continue;
        };
        if !latest.contains_key(&id) {
            order.push(id.clone());
        }
        latest.insert(id, yaml_to_json(block));
    }
    order
        .into_iter()
        .filter_map(|id| latest.remove(&id).map(|row| (id, row)))
        .collect()
}

struct Judge<'a> {
    policy: &'a Policy,
    probe: ActivityProbe,
    leases: Option<Vec<Value>>,
    idle_ms: u64,
    now: u64,
    evidence: BTreeMap<String, SessionEvidence>,
}

impl Judge<'_> {
    fn session_is_gone(&mut self, row: &Value) -> bool {
        let Some(session) = row_session(row) else {
            return false;
        };
        if self.policy.caller.as_deref() == Some(session.as_str()) {
            return false;
        }
        let (probe, leases, idle_ms, now) = (&self.probe, &self.leases, self.idle_ms, self.now);
        let evidence = self
            .evidence
            .entry(session.clone())
            .or_insert_with(|| probe.assess(&session, leases.as_deref(), idle_ms, now));
        !evidence.live
    }

    fn reasons_for(&mut self, store: &Store, id: &str, row: &Value) -> (Vec<&'static str>, Option<u64>) {
        let status = row.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !status_is_open(status) {
            return (Vec::new(), None);
        }
        let mut reasons = Vec::new();
        let mut age_ms = None;
        if self.policy.allows(REASON_EXTERNAL) && blocked_externally(row) {
            age_ms = row_age_ms(store, id, self.now);
            if age_ms.is_some_and(|age| age > self.policy.older_than_ms) {
                reasons.push(REASON_EXTERNAL);
            }
        }
        if self.policy.allows(REASON_SESSION) && self.session_is_gone(row) {
            reasons.push(REASON_SESSION);
        }
        (reasons, age_ms)
    }
}

fn listed_row(candidate: &Candidate) -> Value {
    json!({
        "id": candidate.id,
        "title": clamp_chars(&one_line(&row_text(&candidate.row, &["title", "subject"])), LISTED_TITLE_CHARS),
        "status": candidate.row.get("status").and_then(Value::as_str).unwrap_or("pending"),
        "reasons": candidate.reasons,
        "session_id": row_session(&candidate.row),
        "blockedBy": candidate.row.get("blockedBy"),
        "age_days": candidate.age_ms.map(|age| (age as f64 / MS_PER_DAY as f64 * 10.0).round() / 10.0),
    })
}

fn archive(store: &Store, candidates: &[Candidate], limit: usize) -> (Vec<Value>, Vec<Value>) {
    let mut archived = Vec::new();
    let mut skipped = Vec::new();
    for candidate in candidates.iter().take(limit) {
        let fresh = row_store::item_blocks(store, &candidate.id)
            .last()
            .map(yaml_to_json);
        match fresh {
            None => {
                skipped.push(json!({ "id": candidate.id, "why": "the row is already gone" }));
                continue;
            }
            Some(row) if row != candidate.row => {
                skipped.push(json!({ "id": candidate.id, "why": "the row changed after the scan" }));
                continue;
            }
            Some(_) => {}
        }
        let note = format!("prd-gc: {}", candidate.reasons.join("+"));
        match row_store::retire_to_log(store, &candidate.id, ARCHIVE_STATUS, Some(&note)) {
            Ok(entry) => archived.push(json!({
                "id": candidate.id,
                "reasons": candidate.reasons,
                "log_sha256": entry.get("sha256"),
            })),
            Err(why) => skipped.push(json!({ "id": candidate.id, "why": why })),
        }
    }
    (archived, skipped)
}

pub fn handle(content: &str) -> (String, String, i32) {
    let request = request_object(content);
    let reasons = match requested_reasons(&request) {
        Ok(reasons) => reasons,
        Err(message) => return (String::new(), message, 1),
    };
    let older_than_days = number_field(&request, "older_than_days").unwrap_or(DEFAULT_OLDER_THAN_DAYS);
    let idle_secs = number_field(&request, "idle_secs").unwrap_or(DEFAULT_IDLE_SECS);
    let limit = number_field(&request, "limit")
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(DEFAULT_ARCHIVE_LIMIT);
    let confirm = request_flag(&request, &["confirm"]).unwrap_or(false);
    let policy = Policy {
        older_than_ms: older_than_days.saturating_mul(MS_PER_DAY),
        reasons,
        caller: dispatch_session_id(),
    };
    let store = Store::prd();
    let read = match row_store::read_blocks(&store) {
        Ok(read) => read,
        Err((message, code)) => return (String::new(), message, code),
    };
    let now = now_ms() as u64;
    let mut judge = Judge {
        policy: &policy,
        probe: ActivityProbe::load(),
        leases: request.get("_lease_sessions").and_then(Value::as_array).cloned(),
        idle_ms: idle_secs.saturating_mul(1000),
        now,
        evidence: BTreeMap::new(),
    };
    let mut rows_open = 0usize;
    let mut candidates: Vec<Candidate> = Vec::new();
    for (id, row) in latest_rows(&read.blocks) {
        let status = row.get("status").and_then(Value::as_str).unwrap_or("pending");
        if !status_is_open(status) {
            continue;
        }
        rows_open += 1;
        let (reasons, age_ms) = judge.reasons_for(&store, &id, &row);
        if !reasons.is_empty() {
            candidates.push(Candidate { id, row, reasons, age_ms });
        }
    }
    candidates.sort_by(|a, b| b.age_ms.unwrap_or(0).cmp(&a.age_ms.unwrap_or(0)).then(a.id.cmp(&b.id)));
    let external = candidates.iter().filter(|c| c.reasons.contains(&REASON_EXTERNAL)).count();
    let session = candidates.iter().filter(|c| c.reasons.contains(&REASON_SESSION)).count();
    let (archived, skipped) = if confirm {
        row_store::maybe_absorb(&store);
        archive(&store, &candidates, limit)
    } else {
        (Vec::new(), Vec::new())
    };
    let left = candidates.len().saturating_sub(archived.len());
    let listed: Vec<Value> = candidates.iter().take(LISTED_ROWS_MAX).map(listed_row).collect();
    let sessions: Vec<Value> = judge
        .evidence
        .iter()
        .map(|(id, found)| {
            let mut entry = found.to_json(now);
            entry["session_id"] = json!(id);
            entry
        })
        .collect();
    let reply = json!({
        "ok": true,
        "mode": if confirm { "archive" } else { "review" },
        "older_than_days": older_than_days,
        "idle_secs": idle_secs,
        "reasons_considered": policy.reasons,
        "age_basis": "row file mtime: the time since the row file was last written (the mtime of the legacy .gm/prd.yml for a row not yet split into .gm/prd/), because a PRD row carries no creation time",
        "counts": {
            "rows_open": rows_open,
            "candidates": candidates.len(),
            "external_stale": external,
            "session_gone": session,
            "archived": archived.len(),
            "skipped": skipped.len(),
            "left": left,
        },
        "rows": listed,
        "rows_truncated": candidates.len() > LISTED_ROWS_MAX,
        "sessions": sessions,
        "archived": archived,
        "skipped": skipped,
        "archive_log": ".gm/prd-log.jsonl",
    });
    (reply.to_string(), String::new(), 0)
}
