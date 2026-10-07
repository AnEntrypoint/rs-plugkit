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
    let replay = super::dream_rsi::automatic_replay(Some(owner));
    let latest = replay
        .get("replays")
        .and_then(Value::as_array)
        .and_then(|replays| replays.first())
        .and_then(|entry| entry.get("dispatch_id"))
        .and_then(Value::as_str);
    let reason = match latest {
        None => Some("no-verified-observations"),
        Some(latest) if Some(latest) == after => Some("no-new-verified-observations"),
        Some(_) => None,
    };
    if let Some(reason) = reason {
        return Ok(json!({
            "ok": true, "kind": "observations", "status": "deferred",
            "owner_session_id": owner, "cycle_id": cycle_id, "reason": reason
        }));
    }
    Ok(json!({
        "ok": true, "kind": "observations", "status": "replayed",
        "owner_session_id": owner, "cycle_id": cycle_id,
        "last_dispatch_id": latest, "replay": replay
    }))
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
