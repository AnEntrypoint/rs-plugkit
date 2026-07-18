use serde_json::{json, Value};
use super::host_abi::{host_log, host_now_ms, host_read};

pub(crate) fn log_deviation_push(event: &str, detail: &str) {
    let msg = format!("plugkit gate: {} {}", event, detail);
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
    let evt_payload = json!({
        "event": format!("deviation.{}", event),
        "sub": "hook",
        "detail": detail,
        "ts": unsafe { host_now_ms() },
        "source": "rs-plugkit/git_push",
    });
    let evt_line = format!("evt: {}", evt_payload);
    unsafe { host_log(1, evt_line.as_ptr(), evt_line.len() as u32); }
}

pub(crate) fn emit_event(event: &str, fields: Value) {
    let mut obj = serde_json::Map::new();
    obj.insert("event".to_string(), Value::String(event.to_string()));
    let sess = host_read(".gm/exec-spool/.session-current").unwrap_or_default();
    let sess_trim = sess.trim();
    if !sess_trim.is_empty() {
        obj.insert("sess".to_string(), Value::String(sess_trim.to_string()));
    }
    let ts = unsafe { host_now_ms() };
    obj.insert("ts".to_string(), Value::Number(serde_json::Number::from(ts)));
    if let Value::Object(map) = fields {
        for (k, v) in map { obj.insert(k, v); }
    }
    let payload = Value::Object(obj).to_string();
    let msg = format!("evt: {}", payload);
    unsafe { host_log(1, msg.as_ptr(), msg.len() as u32); }
}

static PANIC_HOOK_INIT: std::sync::Once = std::sync::Once::new();

pub(crate) fn install_panic_hook() {
    PANIC_HOOK_INIT.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(non-string panic payload)".to_string());
            let loc = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "(no location)".to_string());
            let s = format!("WASM PANIC at {}: {}", loc, msg);
            let bytes = s.as_bytes();
            unsafe { host_log(3, bytes.as_ptr(), bytes.len() as u32); }
            emit_event("wasm_panic", serde_json::json!({
                "location": loc,
                "message": msg,
            }));
        }));
    });
}
