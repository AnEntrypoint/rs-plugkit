use serde_json::{json, Value};

pub const MIN_TIMEOUT_MS: u64 = 100;

pub const SESSION_ID_BODY_ALIASES: &[&str] = &["session_id", "sessionId", "SESSION_ID"];

pub fn session_id_from_body(body: &Value) -> Option<String> {
    SESSION_ID_BODY_ALIASES
        .iter()
        .filter_map(|alias| body.get(*alias).and_then(Value::as_str))
        .map(|raw| raw.trim().to_string())
        .find(|id| !id.is_empty())
}

pub fn validate_timeout_ms(body: &Value, fallback_opts: bool) -> Result<u64, Value> {
    let raw = body.get("timeoutMs")
        .or_else(|| if fallback_opts { body.get("opts").and_then(|o| o.get("timeoutMs")) } else { None });
    let n = raw.and_then(|v| v.as_u64());
    match n {
        Some(n) if n >= MIN_TIMEOUT_MS => Ok(n),
        Some(n) if n > 0 => Err(json!({
            "ok": false,
            "error": "timeoutMs below floor",
            "min": MIN_TIMEOUT_MS,
            "received": n,
            "paper_ref": "§20"
        })),
        _ => Err(json!({
            "error": "missing timeoutMs",
            "required": "positive integer milliseconds",
            "paper_ref": "§20"
        })),
    }
}
