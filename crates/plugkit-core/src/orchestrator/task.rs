use super::gm_dir;
use crate::pkfs;
use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1000;
const HARD_FLOOR_MS: u64 = 1000;
const STUCK_SPOOL_AGE_MS: u64 = 90_000;

fn override_max_timeout_ms() -> Option<u64> {
    let p = gm_dir()
        .join("exec-spool")
        .join(".task-timeout-override.json");
    let ps = p.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return None;
    }
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

fn err_resp(_verb: &str, msg: &str) -> (String, String, i32) {
    (String::new(), msg.to_string(), 1)
}

fn ok_resp(_verb: &str, data: Value) -> (String, String, i32) {
    (data.to_string(), String::new(), 0)
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

#[cfg(target_arch = "wasm32")]
fn ensure_current_insight() -> Value {
    crate::code_index::ensure_current_insight()
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_current_insight() -> Value {
    json!({ "ok": false, "ready": false, "error": "code insight unavailable on native target" })
}

pub fn handle_spawn(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => return err_resp("task-spawn", &format!("invalid JSON: {}", e)),
    };
    let lang = body.get("lang").and_then(|v| v.as_str()).unwrap_or("");
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("body").and_then(|v| v.as_str()))
        .unwrap_or("");
    if lang.is_empty() {
        return err_resp("task-spawn", "lang required");
    }
    if code.is_empty() {
        return err_resp("task-spawn", "code required");
    }
    let codeinsight_start = ensure_current_insight();
    if !codeinsight_start
        .get("ready")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return err_resp("task-spawn", &format!(
            "code insight is required before starting a job but did not reach a current complete index: {}",
            codeinsight_start
        ));
    }
    let requested = body.get("timeoutMs").and_then(|v| v.as_u64());
    let timeout = clamp_timeout(requested);
    let was_clamped = requested.map(|r| r != timeout).unwrap_or(false);
    let mut params = json!({
        "lang": lang,
        "code": code,
        "timeoutMs": timeout,
    });
    if let Some(cwd) = body.get("cwd") {
        params["cwd"] = cwd.clone();
    }
    let result = host_task("spawn", &params);
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let msg = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("spawn failed")
            .to_string();
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

pub fn handle_list(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str(content) {
        Ok(Value::Object(body)) => Value::Object(body),
        Ok(_) => return err_resp("task-list", "body must be an object"),
        Err(error) => return err_resp("task-list", &format!("invalid JSON: {error}")),
    };
    if body.as_object().is_some_and(|body| {
        body.keys().any(|key| {
            !matches!(
                key.as_str(),
                "prepare_handoff" | "SESSION_ID" | "session_id" | "sessionId"
            )
        })
    }) {
        return err_resp("task-list", "unknown body field");
    }
    if body
        .get("prepare_handoff")
        .is_some_and(|value| !value.is_boolean())
    {
        return err_resp("task-list", "prepare_handoff must be a boolean");
    }
    let result = host_task("list", &body);
    let tasks = result
        .get("tasks")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let mut data = json!({ "tasks": tasks });
    if let Some(status) = result.get("handoff_status") {
        data["handoff_status"] = status.clone();
    }
    ok_resp("task-list", data)
}

pub fn handle_stop(content: &str) -> (String, String, i32) {
    let id = if let Ok(v) = serde_json::from_str::<Value>(content) {
        v.get("id")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("task_id").and_then(|x| x.as_str()))
            .map(|s| s.to_string())
            .or_else(|| v.as_str().map(|s| s.to_string()))
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
        let msg = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("stop failed")
            .to_string();
        err_resp("task-stop", &msg)
    }
}

pub fn handle_output(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("task_id").and_then(|v| v.as_str()))
        .or_else(|| body.as_str())
        .unwrap_or("");
    if id.is_empty() {
        return err_resp("task-output", "task id required");
    }
    let max_bytes = body
        .get("max_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(65536);
    let result = host_task("output", &json!({ "id": id, "max_bytes": max_bytes }));
    ok_resp("task-output", result)
}

const WAIT_FOR_TASK_POLL_MS: u64 = 2_000;
const WAIT_FOR_TASK_DEFAULT_MS: u64 = 100_000;
const WAIT_FOR_TASK_MAX_MS: u64 = 100_000;

#[cfg(target_arch = "wasm32")]
fn host_sleep_ms(ms: u64) -> bool {
    let code = format!("await new Promise(resolve => setTimeout(resolve, {ms}));");
    let opts = json!({ "timeoutMs": ms.saturating_add(5_000) }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let host = crate::wasm_dispatch::unpack_to_value_pub(packed);
    host.get("exit_code").and_then(Value::as_i64) == Some(0)
        && host.get("timed_out").and_then(Value::as_bool) != Some(true)
}

#[cfg(not(target_arch = "wasm32"))]
fn host_sleep_ms(ms: u64) -> bool {
    std::thread::sleep(std::time::Duration::from_millis(ms));
    true
}

pub fn handle_wait_for_task(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("task_id").and_then(|v| v.as_str()))
        .or_else(|| body.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return err_resp("wait-for-task", "task id required");
    }
    let requested = body
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(WAIT_FOR_TASK_DEFAULT_MS);
    if requested == 0 || requested > WAIT_FOR_TASK_MAX_MS {
        return err_resp(
            "wait-for-task",
            &format!("timeout_ms must be an integer 1..={WAIT_FOR_TASK_MAX_MS}"),
        );
    }
    let started = host_now_ms();
    let deadline = started.saturating_add(requested);
    loop {
        let listed = host_task("list", &json!({}));
        if listed.get("ok").and_then(Value::as_bool) == Some(false) {
            let msg = listed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("task list failed")
                .to_string();
            return err_resp("wait-for-task", &msg);
        }
        let task = listed
            .get("tasks")
            .and_then(Value::as_array)
            .and_then(|tasks| {
                tasks
                    .iter()
                    .find(|t| t.get("id").and_then(Value::as_str) == Some(id.as_str()))
            })
            .cloned();
        let Some(task) = task else {
            return err_resp("wait-for-task", &format!("task {id} not found"));
        };
        let now = host_now_ms();
        let waited_ms = now.saturating_sub(started);
        if task.get("status").and_then(Value::as_str) != Some("running") {
            let output = host_task("output", &json!({ "id": id, "max_bytes": 65536 }));
            return ok_resp(
                "wait-for-task",
                json!({
                    "completed": true,
                    "timed_out": false,
                    "task": task,
                    "output": output,
                    "waited_ms": waited_ms,
                }),
            );
        }
        if now >= deadline {
            return ok_resp(
                "wait-for-task",
                json!({
                    "completed": false,
                    "timed_out": true,
                    "status": "running",
                    "task": task,
                    "timeout_ms": requested,
                    "waited_ms": waited_ms,
                    "note": "the task is still running when timeout_ms passed; dispatch wait-for-task with the same id again to keep waiting",
                }),
            );
        }
        if !host_sleep_ms((deadline - now).min(WAIT_FOR_TASK_POLL_MS)) {
            return err_resp("wait-for-task", "the poll sleep host call failed");
        }
    }
}

pub fn live_running_tasks() -> Value {
    let result = host_task("list", &json!({}));
    let tasks = result
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let now_ms = host_now_ms();
    let mut running: Vec<Value> = Vec::new();
    for t in tasks {
        if t.get("status").and_then(|v| v.as_str()) == Some("running") {
            let pid = t.get("pid").and_then(|v| v.as_u64());
            let id = t.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let lang = t
                .get("lang")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let started = t
                .get("started_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(now_ms);
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

pub fn stuck_spool() -> Value {
    let in_dir = gm_dir().join("exec-spool").join("in");
    let out_dir = gm_dir().join("exec-spool").join("out");
    let in_ps = in_dir.to_string_lossy().to_string();
    let out_ps = out_dir.to_string_lossy().to_string();
    let now_ms = host_now_ms();
    let mut stuck: Vec<Value> = Vec::new();
    let verbs = match pkfs::readdir(&in_ps) {
        Some(v) => v,
        None => return Value::Array(Vec::new()),
    };
    let verb_list = verbs.as_array().cloned().unwrap_or_default();
    for verb_entry in verb_list {
        let is_dir = verb_entry
            .get("is_dir")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }
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
            if !f.get("is_file").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
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
            if age < STUCK_SPOOL_AGE_MS {
                continue;
            }
            let base = fname.trim_end_matches(".txt").to_string();
            let out_name = format!("{}-{}.json", verb_name, base);
            let out_path = format!("{}/{}", out_ps, out_name);
            if pkfs::exists(&out_path) {
                continue;
            }
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
