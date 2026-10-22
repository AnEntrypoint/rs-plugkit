use super::*;

pub(crate) fn active_strategy(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    let read = |owner: &str| -> Option<Value> {
        crate::pkfs::read_to_string(&session_store_path(owner, &format!("{ACTIVE_STRATEGY_FILE}.json")))
            .and_then(|raw| serde_json::from_str(&raw).ok())
    };
    read(session_id).or_else(|| read(PROJECT_WIDE_SESSION)).unwrap_or(Value::Null)
}

pub const PROJECT_WIDE_SESSION: &str = "_any-session";

pub const PROBE_STRATEGY_FILE: &str = "probe-strategy.json";

pub(crate) fn update_probe(owner: &str, fields: &Value, scores: Option<&ReplayScores>) {
    let Some(scores) = scores else { return };
    let path = session_store_path(owner, PROBE_STRATEGY_FILE);
    let baseline = scores.baseline;
    if crate::pkfs::exists(&path) {
        let probe = crate::pkfs::read_to_string(&path).and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let probe_score = probe
            .as_ref()
            .and_then(|probe| probe.get("policy_id")?.as_str())
            .and_then(|policy| {
                scores
                    .ranked
                    .iter()
                    .find(|(policy_id, _)| policy_id == policy)
                    .and_then(|(_, score)| finite_score(*score))
            });
        let dampen = matches!((probe_score, baseline), (Some(score), Some(base)) if score < base);
        if dampen {
            crate::pkfs::remove_file(&path);
        }
        return;
    }
    let improved = fields.get("improved").and_then(Value::as_bool).unwrap_or(false);
    let never_worse = fields.get("never_worse").and_then(Value::as_bool).unwrap_or(false);
    let observations = fields
        .get("history")
        .and_then(|history| history.get("observations"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if improved && never_worse && observations >= INSUFFICIENT_RECORD_THRESHOLD {
        let probe = json!({
            "kind": "probe-strategy",
            "scope": "session",
            "owner_session_id": owner,
            "policy_id": scores.selected_policy_id,
            "baseline_score": baseline,
            "selected_score": scores.selected,
            "n_observations": observations,
            "ts": super::super::state::now_ms() as i64,
        });
        crate::pkfs::write(&path, &probe.to_string());
    }
}

pub(crate) fn persisted_cycle_result(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    crate::pkfs::read_to_string(&session_store_path(session_id, &format!("{CYCLE_RESULT_FILE}.json")))
    .and_then(|raw| serde_json::from_str(&raw).ok())
    .unwrap_or(Value::Null)
}

pub(crate) fn promote_strategy(owner: &str, fields: &Value) -> Value {
    let improved = fields.get("improved").and_then(Value::as_bool).unwrap_or(false);
    let never_worse = fields.get("never_worse").and_then(Value::as_bool).unwrap_or(false);
    let observations = fields
        .get("history")
        .and_then(|history| history.get("observations"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let policy_id = fields.get("selected_policy_id").and_then(Value::as_str).map(str::to_owned);
    let path = session_store_path(owner, &format!("{ACTIVE_STRATEGY_FILE}.json"));
    let refusal = if !(improved && never_worse) {
        Some("replay is not a strict win that is never worse; active strategy unchanged")
    } else if observations < INSUFFICIENT_RECORD_THRESHOLD {
        Some("fewer observations than the promotion threshold; active strategy unchanged")
    } else if policy_id.is_none() {
        Some("replay selected no policy; active strategy unchanged")
    } else {
        None
    };
    if let Some(reason) = refusal {
        return json!({ "promoted": false, "reason": reason, "path": path });
    }
    let policy_id = policy_id.unwrap_or_default();
    let record = json!({
        "kind": "session-strategy",
        "scope": "session",
        "owner_session_id": owner,
        "selected_policy_id": policy_id,
        "selected_score": fields.get("selected_score").cloned().unwrap_or(Value::Null),
        "baseline_score": fields.get("baseline_score").cloned().unwrap_or(Value::Null),
        "n_observations": observations,
        "ts": super::super::state::now_ms() as i64,
    });
    let written = crate::pkfs::write(&path, &record.to_string());
    json!({
        "promoted": written,
        "reason": if written { "strict win that is never worse; selected policy written" } else { "write failed" },
        "policy_id": policy_id,
        "path": path,
    })
}

pub(crate) fn persisted_probe(session_id: &str) -> Value {
    crate::pkfs::read_to_string(&session_store_path(session_id, PROBE_STRATEGY_FILE))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

pub const ACTIVE_STRATEGY_FILE: &str = "active-strategy";

pub const CYCLE_RESULT_FILE: &str = "cycle-result";
pub const INDEX_PATH: &str = ".gm/dream-rsi/index.json";

pub const INDEX_SCHEMA_VERSION: u32 = 1;

pub(crate) fn write_index() {
    let index = json!({
        "schema_version": INDEX_SCHEMA_VERSION,
        "files": {
            "observations": OBSERVATIONS_FILE,
            "active_strategy": format!("{ACTIVE_STRATEGY_FILE}.json"),
            "cycle_result": format!("{CYCLE_RESULT_FILE}.json"),
        },
        "record_kinds": {
            "cycle_result": "cycle-result",
        },
        "fields": ["kind", "source", "owner_session_id", "ts"],
    });
    let _ = crate::pkfs::write(INDEX_PATH, &index.to_string());
}

pub enum ReplayFile {
    Cycle,
}

impl ReplayFile {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            ReplayFile::Cycle => CYCLE_RESULT_FILE,
        }
    }
}

pub struct ReplaySource(pub &'static str);

pub(crate) fn persist_replay_result(
    owner: &str,
    file: ReplayFile,
    source: ReplaySource,
    fields: serde_json::Map<String, Value>,
) -> Result<String, String> {
    let name = file.name();
    let path = session_store_path(owner, &format!("{name}.json"));
    let kind = match file {
        ReplayFile::Cycle => "cycle-result",
    };
    let mut record = fields;
    record.insert("kind".into(), json!(kind));
    record.insert("source".into(), json!(source.0));
    record.insert("owner_session_id".into(), json!(owner));
    record.insert("ts".into(), json!(super::super::state::now_ms() as i64));
    if crate::pkfs::write(&path, &Value::Object(record).to_string()) {
        write_index();
        Ok(path)
    } else {
        Err(format!("could not write replay result to {path}"))
    }
}
