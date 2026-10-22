#[cfg(target_arch = "wasm32")]
use serde_json::{json, Value};

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    super::dream_cycle::adapt(status(content))
}

#[cfg(target_arch = "wasm32")]
fn status(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| error.to_string())?;
    if !body.is_object() {
        return Err("dream-status requires a JSON object".into());
    }
    let owner = super::dream_cycle::canonical_owner(&body, "dream-status")?;
    if super::state::dispatch_session_id().as_deref() != Some(owner) {
        return Err("dream-status owner differs from dispatch session".into());
    }
    let cycle = super::dream_rsi::persisted_cycle_result(Some(owner));
    let status = if cycle.is_null() { "none" } else { "recorded" };
    Ok(json!({
        "ok": true, "kind": "dream-status", "owner_session_id": owner,
        "status": status,
        "cycle_result": cycle,
        "probe_strategy": super::dream_rsi::persisted_probe(owner),
    }))
}
