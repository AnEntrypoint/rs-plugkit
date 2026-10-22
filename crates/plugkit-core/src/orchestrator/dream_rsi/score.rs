use super::*;

pub struct Outcome {
    pub exit_code: i64,
    pub output_bytes: u64,
    pub(crate) gate_drift: bool,
    pub(crate) gate_id: Option<String>,
    pub(crate) predicate: Option<String>,
    pub(crate) first_error_line: Option<String>,
    pub(crate) failure: FailureClass,
}

pub(crate) fn outcome(result: &Value) -> Outcome {
    let decoded = result
        .get("data")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(|value| value.is_object());
    let data = decoded
        .as_ref()
        .or_else(|| result.get("data").filter(|value| value.is_object()))
        .unwrap_or(result);
    let flag = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_bool)
            .or_else(|| data.get(key).and_then(Value::as_bool))
    };
    let exit_code = data
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or(if flag("ok") == Some(true) { 0 } else { 1 });
    let failure = failure_class(result, exit_code);
    Outcome {
        exit_code,
        output_bytes: dispatch_output_bytes(result),
        gate_drift: failure.error_code().is_some_and(is_gate_drift_code),
        gate_id: reply_field(result, "gate_id"),
        predicate: reply_field(result, "predicate"),
        first_error_line: error_line(result, exit_code),
        failure,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn observe_recorded(
    outcome: Outcome,
    dispatch_id: &str,
    session_id: &str,
    verb: &str,
    fingerprint: &str,
    reply: &str,
    lean_node: Option<&str>,
    run_end: bool,
) {
    observe_dispatch(ObservedDispatch {
        session_id,
        dispatch_id,
        verb,
        fingerprint,
        exit_code: outcome.exit_code,
        output_bytes: Some(outcome.output_bytes),
        gate_drift: outcome.gate_drift,
        gate_id: outcome.gate_id.as_deref(),
        predicate: outcome.predicate.as_deref(),
        first_error_line: outcome.first_error_line.as_deref(),
        reply,
        lean_node,
        failure: outcome.failure,
        run_end,
    });
}

pub(crate) fn session_id() -> Result<String, String> {
    crate::orchestrator::state::dispatch_session_id()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "Dream-RSI requires a gm session ID".to_string())
}

pub(crate) const DISPATCH_PAYLOAD_KEYS: [&str; 9] = [
    "data", "result", "output", "text", "matches", "edges", "chunks", "rows", "stdout",
];

pub(crate) fn dispatch_output_bytes(result: &Value) -> u64 {
    pub(crate) fn payload_text_bytes(value: &Value) -> u64 {
        match value {
            Value::String(text) if text.trim().is_empty() => 0,
            Value::String(text) => text.len() as u64,
            Value::Array(items) => items.iter().map(payload_text_bytes).sum(),
            Value::Object(fields) => fields
                .iter()
                .filter(|(key, _)| DISPATCH_PAYLOAD_KEYS.contains(&key.as_str()))
                .map(|(_, child)| payload_text_bytes(child))
                .sum(),
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        }
    }
    payload_text_bytes(result)
}

pub(crate) fn outcome_score(exit_code: i64, no_match: bool, output_bytes: Option<u64>) -> f64 {
    match (exit_code, no_match, output_bytes) {
        (0, _, Some(0)) => 0.5,
        (0, _, _) => 1.0,
        (_, true, _) => 0.5,
        _ => 0.0,
    }
}

