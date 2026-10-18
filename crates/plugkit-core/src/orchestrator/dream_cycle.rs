#[cfg(target_arch = "wasm32")]
use serde_json::{json, Value};

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    let result = evaluate(content);
    match result {
        Ok(value) => (value.to_string(), String::new(), 0),
        Err(error) => (
            json!({"ok": false, "error": error}).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| error.to_string())?;
    if !body.is_object() {
        return Err("dream-replay-cycle requires a JSON object".into());
    }
    let owner = body
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|owner| {
            !owner.is_empty()
                && owner.len() <= 256
                && !matches!(*owner, "." | "..")
                && owner
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        .ok_or("dream-replay-cycle requires a safe canonical session_id")?;
    let active_owner = super::state::dispatch_session_id();
    if active_owner.as_deref() != Some(owner) {
        return Err("dream-replay-cycle owner differs from dispatch session".into());
    }
    let cycle_id = optional_id(&body, "cycle_id", 128)?;
    let after = optional_id(&body, "after_dispatch_id", 512)?;
    let beta1 = optional_beta(&body, "beta1", super::dream_rsi::DEFAULT_CYCLE_BETA1)?;
    let beta2 = optional_beta(&body, "beta2", super::dream_rsi::DEFAULT_CYCLE_BETA2)?;
    let stored = stored_cursor(owner);
    let daemon = owner_cursor(owner, ".cycle-state.json");
    let round = match super::dream_rsi::history_round(owner, after.or(stored.as_deref()), beta1, beta2) {
        Ok(round) => round,
        Err(error) if error.starts_with(super::dream_rsi::POSTCONDITION_FAILED) => {
            return Ok(refused(owner, cycle_id, error));
        }
        Err(error) => return Err(error),
    };
    let latest = round.latest_dispatch_id.as_deref();
    let reason = match latest {
        None => Some("no-verified-observations"),
        Some(latest)
            if Some(latest) == after
                || Some(latest) == stored.as_deref()
                || Some(latest) == daemon.as_deref() =>
        {
            Some("no-new-verified-observations")
        }
        Some(_) if round.summary.pointer("/history/worlds").and_then(Value::as_u64) == Some(0) => {
            Some("no-replayable-worlds")
        }
        Some(_) => None,
    };
    if let Some(reason) = reason {
        return Ok(json!({
            "ok": true, "kind": "observations", "status": "deferred",
            "owner_session_id": owner, "cycle_id": cycle_id, "reason": reason,
            "history": round.summary.get("history"),
        }));
    }
    let lost_cursor = round.lost_cursor;
    let mut fields = round.summary;
    fields["partial_window"] = json!(lost_cursor.is_some());
    fields["lost_cursor"] = json!(lost_cursor);
    fields["cycle_id"] = json!(cycle_id);
    fields["last_dispatch_id"] = json!(latest);
    if let Some(violation) = contract_violation(&fields) {
        let mut reply = refused(owner, cycle_id, violation);
        reply["dampened_probe"] = json!(dampen_session_probe(owner));
        return Ok(reply);
    }
    match super::dream_rsi::persist_replay_result(
        super::dream_rsi::ReplayFile::Cycle,
        super::dream_rsi::ReplaySource("dream-replay-cycle"),
        fields.as_object().cloned().unwrap_or_default(),
    ) {
        Ok(replay_result_path) => Ok(json!({
            "ok": true, "kind": "observations", "status": "replayed",
            "owner_session_id": owner, "cycle_id": cycle_id,
            "last_dispatch_id": latest, "cycle": fields,
            "replay": {"replays": [{"dispatch_id": latest}]},
            "replay_result_path": replay_result_path
        })),
        Err(error) => Ok(json!({
            "ok": false, "error_code": "persist_failed", "kind": "observations",
            "status": "persist_failed", "owner_session_id": owner, "cycle_id": cycle_id,
            "last_dispatch_id": latest, "error": error
        })),
    }
}

#[cfg(target_arch = "wasm32")]
fn refused(owner: &str, cycle_id: Option<&str>, error: String) -> Value {
    json!({
        "ok": false, "error_code": "postcondition_failed", "kind": "observations",
        "status": "refused", "owner_session_id": owner, "cycle_id": cycle_id, "error": error
    })
}

#[cfg(target_arch = "wasm32")]
fn dampen_session_probe(owner: &str) -> Option<String> {
    let path = format!(".gm/dream-rsi/{owner}/{}.json", super::dream_rsi::REPLAY_RESULT_FILE);
    crate::pkfs::remove_file(&path).then_some(path)
}

#[cfg(target_arch = "wasm32")]
fn contract_violation(fields: &Value) -> Option<String> {
    let incumbent = fields.get("incumbent_score").and_then(Value::as_f64)?;
    let selected = fields.get("selected_score").and_then(Value::as_f64)?;
    if selected < incumbent {
        return Some(format!("selected_score {selected} below incumbent_score {incumbent}"));
    }
    if fields.get("improved").and_then(Value::as_bool) == Some(true) && selected <= incumbent {
        return Some(format!("improved without gain: selected_score {selected} <= incumbent_score {incumbent}"));
    }
    None
}

#[cfg(target_arch = "wasm32")]
fn stored_cursor(owner: &str) -> Option<String> {
    owner_cursor(owner, &format!("{}.json", super::dream_rsi::CYCLE_RESULT_FILE))
}

#[cfg(target_arch = "wasm32")]
fn owner_cursor(owner: &str, file: &str) -> Option<String> {
    let path = format!(".gm/dream-rsi/{owner}/{file}");
    let text = crate::pkfs::read_to_string(&path)?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("last_dispatch_id")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(target_arch = "wasm32")]
fn optional_beta(body: &Value, field: &str, default: f64) -> Result<f64, String> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(raw) => raw
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| format!("dream-replay-cycle {field} must be a non-negative finite number")),
    }
}

#[cfg(target_arch = "wasm32")]
fn optional_id<'a>(body: &'a Value, field: &str, limit: usize) -> Result<Option<&'a str>, String> {
    body.get(field)
        .map(|value| {
            value.as_str().filter(|id| !id.is_empty() && id.len() <= limit)
                .ok_or_else(|| format!("dream-replay-cycle {field} requires a nonempty string of at most {limit} bytes"))
        })
        .transpose()
}
