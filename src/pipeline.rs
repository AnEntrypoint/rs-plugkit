#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::libsql_wasm;
use crate::wasm_dispatch::host_now_ms;

const GM_DB: &str = "gm";
const TTL_MS: u64 = 120_000;
const HMAC_KEY_DEFAULT: &str = "dev-only-not-secret-rotate-in-prod";
const SUMMARIZE_THRESHOLD: usize = 2048;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64;
}

pub fn ensure_pipeline_schema() -> Result<(), String> {
    libsql_wasm::exec(
        GM_DB,
        "CREATE TABLE IF NOT EXISTS pipeline_state (step_id TEXT PRIMARY KEY, state TEXT NOT NULL, deadline_ms INTEGER NOT NULL, created_ms INTEGER NOT NULL)",
    )
}

fn hmac_key() -> String {
    let k = "RS_LEARN_PIPELINE_HMAC_KEY";
    let packed = unsafe { host_env_get(k.as_ptr(), k.len() as u32) };
    let ptr = (packed & 0xFFFF_FFFF) as u32;
    let len = (packed >> 32) as u32;
    if ptr != 0 && len != 0 {
        let s = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
        if !s.is_empty() {
            return String::from_utf8_lossy(s).into_owned();
        }
    }
    HMAC_KEY_DEFAULT.to_string()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn keyed_hash(key: &str, data: &str) -> String {
    let inner = fnv1a64(format!("{}|{}", key, data).as_bytes());
    let outer = fnv1a64(format!("{}|{:016x}", key, inner).as_bytes());
    format!("{:016x}{:016x}", outer, inner)
}

fn mint_step_id() -> String {
    let now = unsafe { host_now_ms() } as u64;
    let r = fnv1a64(format!("{}|{}", now, now.wrapping_mul(2654435761)).as_bytes());
    format!("stp_{:016x}", now ^ r)
}

fn mint_flow_id() -> String {
    let now = unsafe { host_now_ms() } as u64;
    let r = fnv1a64(format!("flow|{}", now).as_bytes());
    format!("flw_{:016x}", r)
}

pub fn mint_token(step_id: &str, kv_key: &str, deadline_ms: u64) -> String {
    let payload = format!("{}|{}|{}", step_id, kv_key, deadline_ms);
    format!("tkn_{}.{}", step_id, keyed_hash(&hmac_key(), &payload))
}

pub fn verify_token(token: &str, step_id: &str, kv_key: &str, deadline_ms: u64) -> bool {
    let expected = mint_token(step_id, kv_key, deadline_ms);
    token.len() == expected.len() && constant_time_eq(token.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff: u8 = 0;
    for i in 0..a.len() { diff |= a[i] ^ b[i]; }
    diff == 0
}

fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

pub fn persist_state(step_id: &str, state: &Value, deadline_ms: u64, created_ms: u64) -> Result<(), String> {
    ensure_pipeline_schema()?;
    let state_s = state.to_string();
    let sql = format!(
        "INSERT OR REPLACE INTO pipeline_state(step_id, state, deadline_ms, created_ms) VALUES('{}','{}',{},{})",
        sql_quote(step_id), sql_quote(&state_s), deadline_ms, created_ms
    );
    libsql_wasm::exec(GM_DB, &sql)
}

pub fn load_state(step_id: &str) -> Option<Value> {
    let _ = ensure_pipeline_schema();
    let sql = format!(
        "SELECT state, deadline_ms FROM pipeline_state WHERE step_id='{}'",
        sql_quote(step_id)
    );
    let rows = libsql_wasm::query(GM_DB, &sql).ok()?;
    let arr = rows.as_array()?;
    let row = arr.first()?;
    let state_s = row.get("state").and_then(|v| v.as_str())?;
    serde_json::from_str::<Value>(state_s).ok()
}

pub fn delete_state(step_id: &str) {
    let sql = format!("DELETE FROM pipeline_state WHERE step_id='{}'", sql_quote(step_id));
    let _ = libsql_wasm::exec(GM_DB, &sql);
}

pub fn evict_expired() {
    let _ = ensure_pipeline_schema();
    let now = unsafe { host_now_ms() } as u64;
    let sql = format!("DELETE FROM pipeline_state WHERE deadline_ms < {}", now);
    let _ = libsql_wasm::exec(GM_DB, &sql);
}

pub fn needs_summarize(text: &str) -> bool {
    text.len() > SUMMARIZE_THRESHOLD
}

pub fn build_pending_step(text: &str, namespace: &str, project_path: Option<&str>) -> Value {
    let step_id = mint_step_id();
    let flow_id = mint_flow_id();
    let now = unsafe { host_now_ms() } as u64;
    let deadline_ms = now + TTL_MS;
    let kv_key = format!("rs-learn/pipeline/{}", step_id);
    let bounded_input: String = text.chars().take(8192).collect();
    let payload = json!({
        "input": bounded_input,
        "target_chars": 400,
        "preserve": ["entities", "numbers", "ids"]
    });
    let result_schema = json!({
        "type": "object",
        "required": ["summary"],
        "properties": { "summary": { "type": "string", "maxLength": 800 } }
    });
    let prompt_template = "Summarize the following text into <=400 chars, preserving entities and any numeric facts. Return JSON {\"summary\": string}. Input:\n{{input}}";

    let state = json!({
        "flow_id": flow_id,
        "verb": "memorize",
        "original_body": {
            "text": text,
            "namespace": namespace,
            "project_path": project_path,
        },
        "pipeline": [
            { "step": "summarize", "status": "pending", "id": step_id },
            { "step": "embed", "status": "queued" },
            { "step": "persist", "status": "queued" }
        ],
        "cursor": 0,
        "results_so_far": {},
        "created_ms": now,
        "deadline_ms": deadline_ms,
        "attempts_used": 0,
        "result_schema": result_schema,
        "kind": "summarize",
        "kv_key": kv_key
    });

    if let Err(e) = persist_state(&step_id, &state, deadline_ms, now) {
        return json!({ "ok": false, "error": format!("pipeline persist failed: {}", e) });
    }

    let token = mint_token(&step_id, &kv_key, deadline_ms);
    let _ = mark_turn_pending(&step_id, deadline_ms);

    json!({
        "ok": true,
        "pending_step": {
            "kind": "summarize",
            "id": step_id,
            "payload": payload,
            "prompt_template": prompt_template,
            "max_result_bytes": 4096,
            "result_schema": result_schema
        },
        "token": token,
        "state_kv_key": kv_key,
        "deadline_ms": deadline_ms,
        "attempts_remaining": 2
    })
}

fn mark_turn_pending(step_id: &str, deadline_ms: u64) -> Result<(), String> {
    let mut st = crate::orchestrator::state::read_state();
    st.pending_step_id = Some(step_id.to_string());
    st.pending_step_deadline_ms = Some(deadline_ms as u128);
    st.updated_at_ms = crate::orchestrator::state::now_ms();
    crate::orchestrator::state::write_state(&st).map_err(|e| format!("write_state failed: {}", e))
}

fn clear_turn_pending() {
    let mut st = crate::orchestrator::state::read_state();
    st.pending_step_id = None;
    st.pending_step_deadline_ms = None;
    st.updated_at_ms = crate::orchestrator::state::now_ms();
    let _ = crate::orchestrator::state::write_state(&st);
}

fn validate_result(result: &Value, schema: &Value, max_bytes: usize) -> Result<(), String> {
    let serialized = result.to_string();
    if serialized.len() > max_bytes {
        return Err(format!("result exceeds max_result_bytes ({} > {})", serialized.len(), max_bytes));
    }
    let obj = match result.as_object() {
        Some(o) => o,
        None => return Err("result must be an object".to_string()),
    };
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for k in required {
            if let Some(key) = k.as_str() {
                if !obj.contains_key(key) {
                    return Err(format!("missing required key: {}", key));
                }
            }
        }
    }
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for (k, prop_schema) in props {
            if let Some(val) = obj.get(k) {
                if prop_schema.get("type").and_then(|v| v.as_str()) == Some("string") {
                    let s = match val.as_str() {
                        Some(s) => s,
                        None => return Err(format!("key {} must be string", k)),
                    };
                    if let Some(max_len) = prop_schema.get("maxLength").and_then(|v| v.as_u64()) {
                        if s.chars().count() > max_len as usize {
                            return Err(format!("key {} exceeds maxLength {}", k, max_len));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn handle_continue(body: &Value) -> Value {
    evict_expired();

    let token = match body.get("token").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return json!({ "ok": false, "error": "missing token" }),
    };
    let step_id = match body.get("step_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return json!({ "ok": false, "error": "missing step_id" }),
    };
    let result = match body.get("result") {
        Some(r) => r,
        None => return json!({ "ok": false, "error": "missing result" }),
    };

    let state = match load_state(step_id) {
        Some(s) => s,
        None => {
            clear_turn_pending();
            let flow_id = body.get("flow_id").and_then(|v| v.as_str()).unwrap_or("");
            return json!({
                "ok": false,
                "error": "expired",
                "flow_id": flow_id,
                "hint": "redispatch original verb"
            });
        }
    };

    let kv_key = state.get("kv_key").and_then(|v| v.as_str()).unwrap_or("");
    let deadline_ms = state.get("deadline_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    if !verify_token(token, step_id, kv_key, deadline_ms) {
        return json!({ "ok": false, "error": "invalid_token" });
    }

    let now = unsafe { host_now_ms() } as u64;
    if now > deadline_ms {
        delete_state(step_id);
        clear_turn_pending();
        return json!({ "ok": false, "error": "expired", "hint": "redispatch original verb" });
    }

    let schema = state.get("result_schema").cloned().unwrap_or(json!({}));
    if let Err(e) = validate_result(result, &schema, 4096) {
        let attempts_used = state.get("attempts_used").and_then(|v| v.as_u64()).unwrap_or(0);
        let attempts_remaining = 2u64.saturating_sub(attempts_used + 1);
        if attempts_remaining == 0 {
            delete_state(step_id);
            clear_turn_pending();
            return json!({
                "ok": false,
                "error": "step_unresolvable",
                "kind": state.get("kind").cloned().unwrap_or(Value::Null),
                "step_id": step_id,
                "last_validation_error": e
            });
        }
        let mut new_state = state.clone();
        if let Some(obj) = new_state.as_object_mut() {
            obj.insert("attempts_used".to_string(), json!(attempts_used + 1));
        }
        let _ = persist_state(step_id, &new_state, deadline_ms, now);
        return json!({
            "ok": false,
            "error": "result_schema_violation",
            "detail": e,
            "pending_step": {
                "id": step_id,
                "kind": state.get("kind").cloned().unwrap_or(Value::Null),
            },
            "attempts_remaining": attempts_remaining
        });
    }

    let original = state.get("original_body").cloned().unwrap_or(Value::Null);
    let text = original.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let namespace = original.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let project_path = original.get("project_path").and_then(|v| v.as_str());
    let summary = result.get("summary").and_then(|v| v.as_str()).unwrap_or(text);

    let finalize = crate::code_index::memorize_at_finalize(summary, text, namespace, None, project_path);

    delete_state(step_id);
    clear_turn_pending();

    finalize
}
