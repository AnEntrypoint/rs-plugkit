use serde_json::{json, Value};

pub const MIN_TIMEOUT_MS: u64 = 100;

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
