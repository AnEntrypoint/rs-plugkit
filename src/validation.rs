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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exec_js_accepts_positive_integer() {
        let body = json!({"code": "1+1", "timeoutMs": 5000});
        let r = validate_timeout_ms(&body, true);
        assert_eq!(r.unwrap(), 5000);
    }

    #[test]
    fn exec_js_rejects_missing_timeout() {
        let body = json!({"code": "1+1"});
        let err = validate_timeout_ms(&body, true).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
        assert_eq!(err["paper_ref"], "§20");
        assert_eq!(err["required"], "positive integer milliseconds");
    }

    #[test]
    fn exec_js_rejects_zero() {
        let body = json!({"timeoutMs": 0});
        let err = validate_timeout_ms(&body, true).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
    }

    #[test]
    fn exec_js_rejects_float() {
        let body = json!({"timeoutMs": 1.5});
        let err = validate_timeout_ms(&body, true).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
    }

    #[test]
    fn exec_js_rejects_negative() {
        let body = json!({"timeoutMs": -1});
        let err = validate_timeout_ms(&body, true).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
    }

    #[test]
    fn exec_js_falls_back_to_opts_timeout() {
        let body = json!({"code":"x", "opts": {"timeoutMs": 2000}});
        let r = validate_timeout_ms(&body, true);
        assert_eq!(r.unwrap(), 2000);
    }

    #[test]
    fn shell_exec_accepts_positive_integer() {
        let body = json!({"timeoutMs": 30000});
        let r = validate_timeout_ms(&body, false);
        assert_eq!(r.unwrap(), 30000);
    }

    #[test]
    fn shell_exec_rejects_missing_timeout() {
        let body = json!({"code": "ls"});
        let err = validate_timeout_ms(&body, false).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
        assert_eq!(err["paper_ref"], "§20");
    }

    #[test]
    fn exec_js_rejects_timeout_below_floor() {
        let body = json!({"timeoutMs": 50});
        let err = validate_timeout_ms(&body, true).unwrap_err();
        assert_eq!(err["error"], "timeoutMs below floor");
        assert_eq!(err["min"], 100);
        assert_eq!(err["received"], 50);
        assert_eq!(err["ok"], false);
        assert_eq!(err["paper_ref"], "§20");
    }

    #[test]
    fn exec_js_accepts_timeout_at_floor() {
        let body = json!({"timeoutMs": 100});
        let r = validate_timeout_ms(&body, true);
        assert_eq!(r.unwrap(), 100);
    }

    #[test]
    fn shell_exec_rejects_timeout_below_floor() {
        let body = json!({"timeoutMs": 1});
        let err = validate_timeout_ms(&body, false).unwrap_err();
        assert_eq!(err["error"], "timeoutMs below floor");
        assert_eq!(err["min"], 100);
        assert_eq!(err["received"], 1);
    }

    #[test]
    fn shell_exec_ignores_opts_fallback() {
        let body = json!({"opts": {"timeoutMs": 1000}});
        let err = validate_timeout_ms(&body, false).unwrap_err();
        assert_eq!(err["error"], "missing timeoutMs");
    }
}
