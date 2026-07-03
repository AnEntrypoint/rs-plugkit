use serde_json::{json, Value};
use crate::pkfs;
use super::gm_dir;

const DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const HARD_FLOOR_MS: u64 = 1000;
const STUCK_SPOOL_AGE_MS: u64 = 90_000;

fn override_max_timeout_ms() -> Option<u64> {
    let p = gm_dir().join("exec-spool").join(".task-timeout-override.json");
    let ps = p.to_string_lossy().to_string();
    if !pkfs::exists(&ps) { return None; }
    let content = pkfs::read_to_string(&ps)?;
    let v: Value = serde_json::from_str(&content).ok()?;
    v.get("maxTimeoutMs").and_then(|x| x.as_u64())
}

fn resolved_cap() -> u64 {
    override_max_timeout_ms().unwrap_or(DEFAULT_TIMEOUT_MS)
}

fn clamp_timeout(requested: Option<u64>) -> u64 {
    let cap = resolved_cap();
    let want = requested.unwrap_or(DEFAULT_TIMEOUT_MS);
    let bounded = want.max(HARD_FLOOR_MS).min(cap);
    bounded
}

fn err_resp(verb: &str, msg: &str) -> (String, String, i32) {
    (json!({ "ok": false, "verb": verb, "error": msg }).to_string(), String::new(), 1)
}

fn ok_resp(verb: &str, data: Value) -> (String, String, i32) {
    (json!({ "ok": true, "verb": verb, "data": data }).to_string(), String::new(), 0)
}

#[cfg(target_arch = "wasm32")]
fn host_task(action: &str, params: &Value) -> Value {
    crate::wasm_dispatch::host_task(action, params)
}

#[cfg(not(target_arch = "wasm32"))]
fn host_task(_action: &str, _params: &Value) -> Value {
    json!({ "ok": false, "error": "task exec unavailable on native target" })
}

#[cfg(target_arch = "wasm32")]
fn host_now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

#[cfg(not(target_arch = "wasm32"))]
fn host_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn handle_spawn(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => return err_resp("task-spawn", &format!("invalid JSON: {}", e)),
    };
    let lang = body.get("lang").and_then(|v| v.as_str()).unwrap_or("");
    let code = body.get("code").and_then(|v| v.as_str())
        .or_else(|| body.get("body").and_then(|v| v.as_str()))
        .unwrap_or("");
    if lang.is_empty() { return err_resp("task-spawn", "lang required"); }
    if code.is_empty() { return err_resp("task-spawn", "code required"); }
    let requested = body.get("timeoutMs").and_then(|v| v.as_u64());
    let timeout = clamp_timeout(requested);
    let was_clamped = requested.map(|r| r != timeout).unwrap_or(false);
    let params = json!({
        "lang": lang,
        "code": code,
        "timeoutMs": timeout,
    });
    let result = host_task("spawn", &params);
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let msg = result.get("error").and_then(|v| v.as_str()).unwrap_or("spawn failed").to_string();
        return err_resp("task-spawn", &msg);
    }
    let mut data = result;
    if was_clamped {
        if let Value::Object(ref mut m) = data {
            m.insert("timeout_clamped".to_string(), json!(true));
            m.insert("timeout_requested_ms".to_string(), json!(requested));
            m.insert("timeout_applied_ms".to_string(), json!(timeout));
            m.insert("cap_ms".to_string(), json!(resolved_cap()));
        }
    }
    ok_resp("task-spawn", data)
}

pub fn handle_list(_content: &str) -> (String, String, i32) {
    let result = host_task("list", &json!({}));
    let tasks = result.get("tasks").cloned().unwrap_or(Value::Array(Vec::new()));
    ok_resp("task-list", json!({ "tasks": tasks }))
}

pub fn handle_stop(content: &str) -> (String, String, i32) {
    let id = if let Ok(v) = serde_json::from_str::<Value>(content) {
        v.get("id").and_then(|x| x.as_str())
            .or_else(|| v.get("task_id").and_then(|x| x.as_str()))
            .map(|s| s.to_string())
            .or_else(|| if let Some(s) = v.as_str() { Some(s.to_string()) } else { None })
    } else {
        Some(content.trim().to_string())
    };
    let id = match id {
        Some(s) if !s.is_empty() => s,
        _ => return err_resp("task-stop", "task id required"),
    };
    let result = host_task("stop", &json!({ "id": id }));
    if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        ok_resp("task-stop", result)
    } else {
        let msg = result.get("error").and_then(|v| v.as_str()).unwrap_or("stop failed").to_string();
        err_resp("task-stop", &msg)
    }
}

pub fn handle_output(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let id = body.get("id").and_then(|v| v.as_str())
        .or_else(|| body.get("task_id").and_then(|v| v.as_str()))
        .or_else(|| body.as_str())
        .unwrap_or("");
    if id.is_empty() { return err_resp("task-output", "task id required"); }
    let max_bytes = body.get("max_bytes").and_then(|v| v.as_u64()).unwrap_or(65536);
    let result = host_task("output", &json!({ "id": id, "max_bytes": max_bytes }));
    ok_resp("task-output", result)
}

pub fn live_running_tasks() -> Value {
    let result = host_task("list", &json!({}));
    let tasks = result.get("tasks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let now_ms = host_now_ms() as u64;
    let mut running: Vec<Value> = Vec::new();
    for t in tasks {
        if t.get("status").and_then(|v| v.as_str()) == Some("running") {
            let pid = t.get("pid").and_then(|v| v.as_u64());
            let id = t.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let lang = t.get("lang").and_then(|v| v.as_str()).map(|s| s.to_string());
            let started = t.get("started_ms").and_then(|v| v.as_u64()).unwrap_or(now_ms);
            let deadline = t.get("deadline_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            running.push(json!({
                "id": id,
                "pid": pid,
                "lang": lang,
                "age_ms": now_ms.saturating_sub(started),
                "deadline_ms": deadline,
                "remaining_ms": deadline.saturating_sub(now_ms),
            }));
        }
    }
    Value::Array(running)
}

pub fn any_running() -> bool {
    if let Value::Array(arr) = live_running_tasks() {
        return !arr.is_empty();
    }
    false
}

pub fn open_browser_sessions() -> Value {
    let p = gm_dir().join("exec-spool").join("browser-sessions.json");
    let ps = p.to_string_lossy().to_string();
    if !pkfs::exists(&ps) { return Value::Array(Vec::new()); }
    let content = match pkfs::read_to_string(&ps) {
        Some(s) => s,
        None => return Value::Array(Vec::new()),
    };
    let v: Value = match serde_json::from_str(&content) {
        Ok(x) => x,
        Err(_) => return Value::Array(Vec::new()),
    };
    let obj = match v.as_object() {
        Some(m) => m,
        None => return Value::Array(Vec::new()),
    };
    let mut out: Vec<Value> = Vec::new();
    let ports_p = gm_dir().join("exec-spool").join("browser-ports.json");
    let ports_s = ports_p.to_string_lossy().to_string();
    let ports: Value = pkfs::read_to_string(&ports_s)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let current_sid = super::state::read_state().session_id;
    for (sid, sessions) in obj {
        if let Some(cur) = &current_sid {
            if sid != cur { continue; }
        }
        let port_info = ports.get(sid).cloned().unwrap_or(Value::Null);
        out.push(json!({
            "session_id": sid,
            "browser_sessions": sessions,
            "port": port_info.get("port"),
            "chrome_pid": port_info.get("pid"),
        }));
    }
    Value::Array(out)
}

pub fn stuck_spool() -> Value {
    let in_dir = gm_dir().join("exec-spool").join("in");
    let out_dir = gm_dir().join("exec-spool").join("out");
    let in_ps = in_dir.to_string_lossy().to_string();
    let out_ps = out_dir.to_string_lossy().to_string();
    let now_ms = host_now_ms() as u64;
    let mut stuck: Vec<Value> = Vec::new();
    let verbs = match pkfs::readdir(&in_ps) {
        Some(v) => v,
        None => return Value::Array(Vec::new()),
    };
    let verb_list = verbs.as_array().cloned().unwrap_or_default();
    for verb_entry in verb_list {
        let is_dir = verb_entry.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
        if !is_dir { continue; }
        let verb_name = match verb_entry.get("name").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let verb_dir = format!("{}/{}", in_ps, verb_name);
        let files = match pkfs::readdir(&verb_dir) {
            Some(v) => v.as_array().cloned().unwrap_or_default(),
            None => continue,
        };
        for f in files {
            if !f.get("is_file").and_then(|v| v.as_bool()).unwrap_or(false) { continue; }
            let fname = match f.get("name").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let full = format!("{}/{}", verb_dir, fname);
            let stat = match pkfs::stat(&full) {
                Some(v) => v,
                None => continue,
            };
            let mtime = stat.get("mtime_ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64;
            let age = now_ms.saturating_sub(mtime);
            if age < STUCK_SPOOL_AGE_MS { continue; }
            let base = fname.trim_end_matches(".txt").to_string();
            let out_name = format!("{}-{}.json", verb_name, base);
            let out_path = format!("{}/{}", out_ps, out_name);
            if pkfs::exists(&out_path) { continue; }
            stuck.push(json!({
                "verb": verb_name,
                "task_base": base,
                "age_ms": age,
                "path": full,
            }));
        }
    }
    Value::Array(stuck)
}
