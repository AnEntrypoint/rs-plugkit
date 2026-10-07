use serde_json::{json, Value};

const MAX_WAIT_MS: u64 = 60_000;

fn reject(error: &str, received: Option<&Value>) -> (String, String, i32) {
    let mut body = json!({
        "ok": false,
        "error": error,
        "error_code": "invalid_args",
        "max_ms": MAX_WAIT_MS,
    });
    if let Some(received) = received {
        body["received"] = received.clone();
    }
    (body.to_string(), String::new(), 1)
}

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let received = body.get("ms").or_else(|| body.get("durationMs"));
    let ms = match received.and_then(Value::as_u64) {
        Some(ms) if (1..=MAX_WAIT_MS).contains(&ms) => ms,
        Some(_) => return reject("wait ms must be between 1 and max_ms", received),
        None => {
            return reject(
                "wait requires ms as a positive integer duration in milliseconds",
                received,
            )
        }
    };
    let code = format!("await new Promise(resolve => setTimeout(resolve, {ms}));");
    let opts = json!({ "timeoutMs": ms.saturating_add(1_000) }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let host = match crate::wasm_dispatch::unpack_to_string_pub(packed) {
        Some(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        None => {
            return (
                json!({
                    "ok": false,
                    "error": "wait host returned empty",
                    "error_code": "failed",
                    "requested_ms": ms,
                    "retryable": true,
                })
                .to_string(),
                String::new(),
                1,
            )
        }
    };
    if host
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return (
            json!({
                "ok": false,
                "error": "wait host timed out before the requested duration elapsed",
                "error_code": "failed",
                "requested_ms": ms,
                "host": host,
            })
            .to_string(),
            String::new(),
            1,
        );
    }
    if host
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|code| code != 0)
        .unwrap_or(false)
    {
        return (
            json!({
                "ok": false,
                "error": "wait host returned a non-zero exit code",
                "error_code": "failed",
                "requested_ms": ms,
                "host": host,
            })
            .to_string(),
            String::new(),
            1,
        );
    }
    (
        json!({ "completed": true, "waited_ms": ms }).to_string(),
        String::new(),
        0,
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let received = body.get("ms").or_else(|| body.get("durationMs"));
    let ms = match received.and_then(Value::as_u64) {
        Some(ms) if (1..=MAX_WAIT_MS).contains(&ms) => ms,
        Some(_) => return reject("wait ms must be between 1 and max_ms", received),
        None => {
            return reject(
                "wait requires ms as a positive integer duration in milliseconds",
                received,
            )
        }
    };
    std::thread::sleep(std::time::Duration::from_millis(ms));
    (
        json!({ "completed": true, "waited_ms": ms }).to_string(),
        String::new(),
        0,
    )
}
