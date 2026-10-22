use super::*;

pub(crate) fn session_store_path(session: &str, file: &str) -> String {
    let dir = format!(".gm/dream-rsi/{session}");
    if file.is_empty() {
        dir
    } else {
        format!("{dir}/{file}")
    }
}

pub(crate) fn is_gate_drift_code(code: &str) -> bool {
    matches!(
        code,
        crate::wasm_dispatch::ERR_CODE_GATE_DENIED
            | crate::wasm_dispatch::ERR_CODE_UNKNOWN_VERB
            | crate::wasm_dispatch::ERR_CODE_RETIRED_VERB
    )
}

pub(crate) const FAILURE_CLASS_KEYS: [&str; 4] = ["error_code", "error_kind", "reason", "code"];

pub(crate) const FAILURE_CLASS_MAX_CHARS: usize = 64;

pub(crate) const MATCH_COUNT_KEYS: [&str; 3] = ["match_count", "total_matches", "returned"];

pub(crate) fn token_like(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= FAILURE_CLASS_MAX_CHARS
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

pub(crate) fn reply_layers(result: &Value) -> [Option<&Value>; 2] {
    [Some(result), result.get("data")]
}

pub(crate) fn reply_field(result: &Value, key: &str) -> Option<String> {
    reply_layers(result).iter().flatten().find_map(|layer| {
        layer.get(key).and_then(Value::as_str).map(str::trim).filter(|text| token_like(text)).map(str::to_owned)
    })
}

pub(crate) const ERROR_LINE_KEYS: [&str; 5] = ["stderr", "error", "message", "reason", "output"];

pub(crate) const ERROR_LINE_MAX_CHARS: usize = 240;

pub(crate) fn error_line(result: &Value, exit_code: i64) -> Option<String> {
    if exit_code == 0 {
        return None;
    }
    reply_layers(result).iter().flatten().find_map(|layer| {
        ERROR_LINE_KEYS.iter().find_map(|key| {
            layer
                .get(*key)
                .and_then(Value::as_str)
                .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
                .map(|line| line.chars().take(ERROR_LINE_MAX_CHARS).collect::<String>())
        })
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum FailureClass {
    Ok,
    Coded(String),
    NoMatch,
    Unclassified,
}

impl FailureClass {
    pub(crate) fn error_code(&self) -> Option<&str> {
        match self {
            Self::Coded(code) => Some(code),
            _ => None,
        }
    }

    pub(crate) fn no_match(&self) -> bool {
        matches!(self, Self::NoMatch)
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Coded(_) => "coded",
            Self::NoMatch => "no_match",
            Self::Unclassified => "unclassified",
        }
    }

    pub(crate) fn for_exit(self, exit_code: i64) -> Self {
        match (exit_code == 0, self) {
            (true, _) => Self::Ok,
            (false, Self::Ok) => Self::Unclassified,
            (false, other) => other,
        }
    }
}

pub(crate) fn failure_class(result: &Value, exit_code: i64) -> FailureClass {
    if exit_code == 0 {
        return FailureClass::Ok;
    }
    let layers = reply_layers(result);
    let error_code = layers.iter().flatten().find_map(|layer| {
        FAILURE_CLASS_KEYS.iter().find_map(|key| {
            layer.get(*key).and_then(Value::as_str).map(str::trim).filter(|text| token_like(text)).map(str::to_owned)
        })
    });
    if let Some(code) = error_code {
        return if code == "not_found" {
            FailureClass::NoMatch
        } else {
            FailureClass::Coded(code)
        };
    }
    let zero_matches = layers.iter().flatten().any(|layer| {
        MATCH_COUNT_KEYS.iter().any(|key| layer.get(*key).and_then(Value::as_u64) == Some(0))
    });
    if zero_matches {
        FailureClass::NoMatch
    } else {
        FailureClass::Coded("exit_nonzero".to_owned())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ObservationRecord {
    pub(crate) dispatch_id: Option<String>,
    pub(crate) verb: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) exit_code: Option<i64>,
    pub(crate) gate_drift: Option<bool>,
    pub(crate) gate_id: Option<String>,
    pub(crate) predicate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prd_open_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mutable_open_count: Option<u64>,
    pub(crate) quality: Option<f64>,
    pub(crate) ts: Option<i64>,
    pub(crate) reply_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lean_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) no_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) first_error_line: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) run_end: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) policy_id: Option<String>,
}

impl ObservationRecord {
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let record: Self = serde_json::from_value(value.clone()).ok()?;
        (record.exit_code.is_some() && record.is_consistent()).then_some(record)
    }

    pub(crate) fn is_consistent(&self) -> bool {
        let coded = self.error_code.is_some();
        let exit_ok = self.exit_code == Some(0);
        let outcome_ok = if exit_ok {
            !coded && !self.no_match
        } else {
            !(coded && self.no_match)
        };
        let expected_class = if coded {
            "coded"
        } else if self.no_match {
            "no_match"
        } else if exit_ok {
            "ok"
        } else {
            "unclassified"
        };
        outcome_ok && self.failure_class.as_deref().is_none_or(|name| name == expected_class)
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub(crate) fn is_gate_drift_failure(&self) -> bool {
        !self.succeeded() && self.gate_drift.unwrap_or(false)
    }
}

pub(crate) fn read_observation_values(raw: &str) -> (Vec<Value>, usize) {
    if raw.trim_start().starts_with('[') {
        return match serde_json::from_str::<Vec<Value>>(raw) {
            Ok(values) => (values, 0),
            Err(_) => (Vec::new(), 1),
        };
    }
    let mut rejected = 0;
    let values = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(value) => Some(value),
            Err(_) => {
                rejected += 1;
                None
            }
        })
        .collect();
    (values, rejected)
}

pub(crate) fn read_observations(raw: &str) -> (Vec<ObservationRecord>, usize) {
    let (values, parse_rejected) = read_observation_values(raw);
    let records: Vec<ObservationRecord> = values.iter().filter_map(ObservationRecord::from_value).collect();
    let shape_rejected = values.len() - records.len();
    (records, parse_rejected + shape_rejected)
}

pub(crate) fn reply_sha256_hex(reply: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(reply.as_bytes()))
}

pub struct ObservedDispatch<'a> {
    pub session_id: &'a str,
    pub dispatch_id: &'a str,
    pub verb: &'a str,
    pub fingerprint: &'a str,
    pub exit_code: i64,
    pub output_bytes: Option<u64>,
    pub gate_drift: bool,
    pub gate_id: Option<&'a str>,
    pub predicate: Option<&'a str>,
    pub first_error_line: Option<&'a str>,
    pub reply: &'a str,
    pub lean_node: Option<&'a str>,
    pub failure: FailureClass,
    pub run_end: bool,
}

pub(crate) const OBSERVATIONS_LIVE_CAP: usize = 256;

pub(crate) const OBSERVATIONS_ARCHIVE_MAX_CHUNKS: usize = 32;

pub(crate) const OBSERVATIONS_ARCHIVE_PREFIX: &str = "observations-archive-";

pub(crate) const OBSERVATIONS_LIVE_COUNT_FILE: &str = "observations.count";

pub(crate) fn dir_entry_names(dir: &str) -> Vec<String> {
    let Some(Value::Array(entries)) = crate::pkfs::readdir(dir) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("name").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

pub(crate) fn archive_names(session_id: &str) -> Vec<String> {
    let mut names: Vec<(i64, String)> = dir_entry_names(&session_store_path(session_id, ""))
        .into_iter()
        .filter_map(|name| {
            let ts = name
                .strip_prefix(OBSERVATIONS_ARCHIVE_PREFIX)?
                .strip_suffix(".json")?
                .parse::<i64>()
                .ok()?;
            Some((ts, name))
        })
        .collect();
    names.sort_by_key(|(ts, _)| *ts);
    names.into_iter().map(|(_, name)| name).collect()
}

pub(crate) fn archive_observations(session_id: &str, ts: i64, raw: &str) -> Option<String> {
    let path = session_store_path(session_id, &format!("{OBSERVATIONS_ARCHIVE_PREFIX}{ts}.json"));
    if crate::pkfs::exists(&path) || !crate::pkfs::write(&path, raw) {
        return None;
    }
    let names = archive_names(session_id);
    for stale in names.iter().take(names.len().saturating_sub(OBSERVATIONS_ARCHIVE_MAX_CHUNKS)) {
        crate::pkfs::remove_file(&session_store_path(session_id, stale));
    }
    Some(path)
}

pub(crate) fn live_count_path(session_id: &str) -> String {
    session_store_path(session_id, OBSERVATIONS_LIVE_COUNT_FILE)
}

pub(crate) fn read_live_count(session_id: &str) -> Option<usize> {
    crate::pkfs::read_to_string(&live_count_path(session_id))?.trim().parse().ok()
}

pub(crate) fn write_live_count(session_id: &str, count: usize) {
    crate::pkfs::write(&live_count_path(session_id), &format!("{count}\n"));
}

pub(crate) fn bump_live_count(session_id: &str) {
    const LIVE_COUNT_CAS_ATTEMPTS: usize = 64;
    let path = live_count_path(session_id);
    for _ in 0..LIVE_COUNT_CAS_ATTEMPTS {
        let Some(raw) = crate::pkfs::read_to_string(&path) else {
            return;
        };
        let Ok(count) = raw.trim().parse::<usize>() else {
            return;
        };
        if matches!(
            crate::pkfs::cas_write(&path, &raw, &format!("{}\n", count + 1)),
            crate::pkfs::CasWriteOutcome::Swapped
        ) {
            return;
        }
    }
}

pub(crate) fn recover_observations_tail(path: &str) -> Option<String> {
    let Some(raw) = crate::pkfs::read_to_string(path) else {
        return Some(String::new());
    };
    if raw.is_empty() || raw.ends_with('\n') {
        return Some(raw);
    }
    let keep = raw.rfind('\n').map_or(0, |i| i + 1);
    let tail = &raw[keep..];
    let tail_parses = serde_json::from_str::<Value>(tail.trim()).is_ok();
    if !tail_parses && !crate::pkfs::append(&format!("{path}.torn"), &format!("{tail}\n")) {
        return None;
    }
    let body = if tail_parses {
        format!("{raw}\n")
    } else {
        raw[..keep].to_owned()
    };
    match crate::pkfs::cas_write(path, &raw, &body) {
        crate::pkfs::CasWriteOutcome::Swapped => Some(body),
        _ => None,
    }
}

pub(crate) fn rotate_observations_if_full(session_id: &str, path: &str, ts: i64) {
    if read_live_count(session_id).is_some_and(|count| count < OBSERVATIONS_LIVE_CAP) {
        return;
    }
    let Some(raw) = crate::pkfs::read_to_string(path) else {
        return;
    };
    if raw.trim().is_empty() {
        write_live_count(session_id, 0);
        return;
    }
    let legacy_array = raw.trim_start().starts_with('[');
    let live_lines = raw.lines().filter(|line| !line.trim().is_empty()).count();
    if !legacy_array && live_lines < OBSERVATIONS_LIVE_CAP {
        write_live_count(session_id, live_lines);
        return;
    }
    let Some(archive) = archive_observations(session_id, ts, &raw) else {
        return;
    };
    let swapped = matches!(
        crate::pkfs::cas_write(path, &raw, ""),
        crate::pkfs::CasWriteOutcome::Swapped
    );
    if !swapped {
        crate::pkfs::remove_file(&archive);
        write_live_count(session_id, OBSERVATIONS_LIVE_CAP);
        return;
    }
    write_live_count(session_id, 0);
}

pub(crate) fn session_history(session_id: &str) -> (Vec<ObservationRecord>, usize) {
    let mut records: Vec<ObservationRecord> = Vec::new();
    let mut rejected = 0;
    let mut absorb = |raw: &str| {
        let (found, dropped) = read_observations(raw);
        records.extend(found);
        rejected += dropped;
    };
    for name in archive_names(session_id) {
        if let Some(raw) = crate::pkfs::read_to_string(&session_store_path(session_id, &name)) {
            absorb(&raw);
        }
    }
    if let Some(raw) = recover_observations_tail(&session_store_path(session_id, OBSERVATIONS_FILE)) {
        absorb(&raw);
    }
    (records, rejected)
}

use super::super::NON_DECISION_VERBS;

pub(crate) fn observe_dispatch(observed: ObservedDispatch<'_>) {
    let ObservedDispatch {
        session_id,
        dispatch_id,
        verb,
        fingerprint,
        exit_code,
        output_bytes,
        gate_drift,
        gate_id,
        predicate,
        first_error_line,
        reply,
        lean_node,
        failure,
        run_end,
    } = observed;
    if verb.starts_with("dream-") || NON_DECISION_VERBS.contains(&verb) {
        return;
    }
    let failure = failure.for_exit(exit_code);
    let path = session_store_path(session_id, OBSERVATIONS_FILE);
    let ts = super::super::state::now_ms() as i64;
    rotate_observations_if_full(session_id, &path, ts);
    if recover_observations_tail(&path).is_none() {
        record_observation_drop(session_id, dispatch_id, "observations_tail_unrecoverable");
        return;
    }
    let (prd_open_count, mutable_open_count) = super::super::instructions::open_row_counts();
    let record = ObservationRecord {
        dispatch_id: Some(dispatch_id.to_string()),
        verb: Some(verb.to_string()),
        fingerprint: Some(fingerprint.to_string()),
        exit_code: Some(exit_code),
        gate_drift: Some(gate_drift),
        gate_id: gate_id.map(str::to_owned),
        predicate: predicate.map(str::to_owned),
        first_error_line: first_error_line.map(str::to_owned),
        prd_open_count,
        mutable_open_count: Some(mutable_open_count),
        quality: Some(outcome_score(exit_code, failure.no_match(), output_bytes)),
        ts: Some(ts),
        reply_sha256: Some(reply_sha256_hex(reply)),
        lean_node: lean_node.map(str::to_owned),
        error_code: failure.error_code().map(str::to_owned),
        no_match: failure.no_match(),
        failure_class: Some(failure.name().to_owned()),
        run_end,
        policy_id: Some(incumbent_policy(session_id).id),
        ..Default::default()
    };
    let Ok(line) = serde_json::to_string(&record) else {
        record_observation_drop(session_id, dispatch_id, "record_serialize_failed");
        return;
    };
    if crate::pkfs::append(&path, &format!("{line}\n")) {
        bump_live_count(session_id);
        super::super::dream_cycle::replay_if_due(session_id);
    } else {
        record_observation_drop(session_id, dispatch_id, "append_failed");
    }
}

pub(crate) const OBSERVATION_DROPS_NS: &str = "dream_observation_drops";
pub(crate) const OBSERVATION_DROPS_RECENT: usize = 20;

pub(crate) fn observation_drops_record(session: &str) -> Value {
    crate::wasm_dispatch::host_kv_read(OBSERVATION_DROPS_NS, session)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({ "count": 0, "recent": [] }))
}

pub(crate) fn observation_drops(session: &str) -> Value {
    observation_drops_record(session)
}

pub(crate) fn record_observation_drop(session_id: &str, dispatch_id: &str, reason: &str) {
    let previous = observation_drops_record(session_id);
    let count = previous.get("count").and_then(Value::as_u64).unwrap_or(0) + 1;
    let mut recent = previous.get("recent").and_then(Value::as_array).cloned().unwrap_or_default();
    recent.push(json!({ "dispatch_id": dispatch_id, "reason": reason, "ts": super::super::state::now_ms() as i64 }));
    let excess = recent.len().saturating_sub(OBSERVATION_DROPS_RECENT);
    recent.drain(..excess);
    let value = json!({ "count": count, "recent": recent }).to_string();
    unsafe {
        crate::wasm_dispatch::host_kv_put(
            OBSERVATION_DROPS_NS.as_ptr(),
            OBSERVATION_DROPS_NS.len() as u32,
            session_id.as_ptr(),
            session_id.len() as u32,
            value.as_ptr(),
            value.len() as u32,
        );
    }
}

