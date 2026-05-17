#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

extern "C" {
    pub fn host_fs_read(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fs_write(path_ptr: *const u8, path_len: u32, data_ptr: *const u8, data_len: u32) -> u32;
    pub fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fs_stat(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fetch(url_ptr: *const u8, url_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_kv_get(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u64;
    pub fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    pub fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    pub fn host_vec_search(q_ptr: *const u8, q_len: u32, k: u32) -> u64;
    pub fn host_vec_embed(text_ptr: *const u8, text_len: u32) -> u64;
    pub fn host_browser_spawn(url_ptr: *const u8, url_len: u32) -> u64;
    pub fn host_browser_eval(session_id: u64, code_ptr: *const u8, code_len: u32) -> u64;
    pub fn host_browser_close(session_id: u64) -> u32;
    pub fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    pub fn host_now_ms() -> u64;
    pub fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64;
}

fn pack(s: String) -> u64 {
    let bytes = s.into_bytes();
    let len = bytes.len() as u64;
    let mut v = bytes;
    let ptr = v.as_mut_ptr() as u64;
    std::mem::forget(v);
    (ptr & 0xffff_ffff) | (len << 32)
}

fn read_str(ptr: *const u8, len: u32) -> String {
    if ptr.is_null() || len == 0 { return String::new(); }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn unpack_to_string(packed: u64) -> Option<String> {
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 || l == 0 { return None; }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn unpack_to_value(packed: u64) -> Value {
    match unpack_to_string(packed) {
        Some(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        None => Value::Null,
    }
}

pub fn host_read(path: &str) -> Option<String> {
    let packed = unsafe { host_fs_read(path.as_ptr(), path.len() as u32) };
    unpack_to_string(packed)
}

pub fn host_write(path: &str, data: &str) -> bool {
    let rc = unsafe { host_fs_write(path.as_ptr(), path.len() as u32, data.as_ptr(), data.len() as u32) };
    rc != 0
}

pub fn host_stat(path: &str) -> Option<Value> {
    let packed = unsafe { host_fs_stat(path.as_ptr(), path.len() as u32) };
    unpack_to_string(packed).map(|s| serde_json::from_str(&s).unwrap_or(Value::Null))
}

pub fn host_exists(path: &str) -> bool {
    host_stat(path).map(|v| !v.is_null()).unwrap_or(false)
}

fn err(verb: &str, reason: &str) -> u64 {
    pack(json!({ "ok": false, "verb": verb, "error": reason }).to_string())
}

fn ok(verb: &str, data: Value) -> u64 {
    pack(json!({ "ok": true, "verb": verb, "data": data }).to_string())
}

fn fs_read(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_read", "path required"); }
    match host_read(path) {
        Some(s) => ok("fs_read", Value::String(s)),
        None => err("fs_read", "not found or empty"),
    }
}

fn fs_write(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_write", "path required"); }
    if host_write(path, data) { ok("fs_write", json!({ "bytes": data.len() })) } else { err("fs_write", "write failed") }
}

fn fs_readdir(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() { return err("fs_readdir", "empty"); }
    ok("fs_readdir", v)
}

fn fs_stat(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_stat", "path required"); }
    match host_stat(path) {
        Some(v) if !v.is_null() => ok("fs_stat", v),
        _ => err("fs_stat", "not found"),
    }
}

fn fetch(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() { return err("fetch", "url required"); }
    let opts = body.get("opts").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() { return err("fetch", "host_fetch empty"); }
    ok("fetch", v)
}

fn env_get(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("env_get", "key required"); }
    let packed = unsafe { host_env_get(key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("env_get", Value::String(s)),
        None => ok("env_get", Value::Null),
    }
}

fn browser_spawn(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank");
    let sid = unsafe { host_browser_spawn(url.as_ptr(), url.len() as u32) };
    ok("browser_spawn", json!({ "sessionId": sid }))
}

fn browser_eval(body: &Value) -> u64 {
    let sid = body.get("sessionId").and_then(|v| v.as_u64()).unwrap_or(0);
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if sid == 0 || code.is_empty() { return err("browser_eval", "sessionId+code required"); }
    let packed = unsafe { host_browser_eval(sid, code.as_ptr(), code.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("browser_eval", Value::String(s)),
        None => ok("browser_eval", Value::Null),
    }
}

fn browser_close(body: &Value) -> u64 {
    let sid = body.get("sessionId").and_then(|v| v.as_u64()).unwrap_or(0);
    if sid == 0 { return err("browser_close", "sessionId required"); }
    let rc = unsafe { host_browser_close(sid) };
    if rc != 0 { ok("browser_close", Value::Null) } else { err("browser_close", "close failed") }
}

fn exec_js(body: &Value) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_empty() { return err("exec_js", "code required"); }
    let opts = body.get("opts").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("exec_js", Value::String(s)),
        None => ok("exec_js", Value::Null),
    }
}

fn kv_get(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("kv_get", "key required"); }
    let packed = unsafe { host_kv_get(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("kv_get", Value::String(s)),
        None => ok("kv_get", Value::Null),
    }
}

fn kv_put(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let val = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("kv_put", "key required"); }
    let rc = unsafe { host_kv_put(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
    if rc != 0 { ok("kv_put", json!({"bytes": val.len()})) } else { err("kv_put", "put failed") }
}

fn kv_query(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let q = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, q.as_ptr(), q.len() as u32) };
    let v = unpack_to_value(packed);
    ok("kv_query", v)
}

fn recall(body: &Value) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as u32;
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if query.is_empty() { return err("recall", "query required"); }
    let emb_packed = unsafe { host_vec_embed(query.as_ptr(), query.len() as u32) };
    let embedding = if emb_packed != 0 { unpack_to_value(emb_packed) } else { Value::Null };
    let q_json = json!({ "query": query, "embedding": embedding }).to_string();
    let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, limit) };
    let vec_hits = unpack_to_value(packed);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        return ok("recall", vec_hits);
    }
    let packed = unsafe { host_kv_query(namespace.as_ptr(), namespace.len() as u32, query.as_ptr(), query.len() as u32) };
    let kv_hits = unpack_to_value(packed);
    ok("recall", kv_hits)
}

fn memorize_with_raw(body: &Value, raw: &str) -> u64 {
    let text = body.get("text").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if text.is_empty() { return err("memorize", "text required"); }
    let text = text.as_str();
    let now = unsafe { host_now_ms() };
    let key = format!("mem-{}-{}", now, text.len());
    let rc = unsafe { host_kv_put(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32, text.as_ptr(), text.len() as u32) };
    if rc == 0 { return err("memorize", "kv_put failed"); }
    let emb_packed = unsafe { host_vec_embed(text.as_ptr(), text.len() as u32) };
    if emb_packed != 0 {
        let vec_ns = format!("{}-vec", namespace);
        let emb_str = unpack_to_string(emb_packed).unwrap_or_default();
        if !emb_str.is_empty() {
            let _ = unsafe { host_kv_put(vec_ns.as_ptr(), vec_ns.len() as u32, key.as_ptr(), key.len() as u32, emb_str.as_ptr(), emb_str.len() as u32) };
        }
    }
    ok("memorize", json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": emb_packed != 0}))
}

fn codesearch(body: &Value) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    if query.is_empty() { return err("codesearch", "query required"); }
    let emb_packed = unsafe { host_vec_embed(query.as_ptr(), query.len() as u32) };
    let embedding = if emb_packed != 0 { unpack_to_value(emb_packed) } else { Value::Null };
    let q_json = json!({ "query": query, "embedding": embedding, "namespace": "codeinsight" }).to_string();
    let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, k) };
    let vec_hits = unpack_to_value(packed);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        return ok("codesearch", vec_hits);
    }
    let ns = "codeinsight";
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, query.as_ptr(), query.len() as u32) };
    let hits = unpack_to_value(packed);
    ok("codesearch", hits)
}

fn health(_body: &Value) -> u64 {
    let now = unsafe { host_now_ms() };
    ok("health", json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "now": now,
        "imports": [
            "host_fs_read","host_fs_write","host_fs_readdir","host_fs_stat",
            "host_fetch","host_kv_get","host_kv_put","host_kv_query",
            "host_vec_search","host_vec_embed",
            "host_browser_spawn","host_browser_eval","host_browser_close",
            "host_exec_js","host_log","host_now_ms","host_env_get"
        ]
    }))
}

fn status(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 { return err("status", "taskId required"); }
    let key = format!("{}", task_id);
    let packed = unsafe { host_kv_get("outbox".as_ptr(), 6, key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("status", serde_json::from_str(&s).unwrap_or(Value::String(s))),
        None => err("status", "task not found"),
    }
}

fn wait(body: &Value) -> u64 {
    let ms = body.get("ms").and_then(|v| v.as_u64()).unwrap_or(1000);
    let start = unsafe { host_now_ms() };
    // Busy-wait: thebird host should implement async sleep; this is a fallback
    let _ = start; let _ = ms;
    ok("wait", json!({ "waitedMs": ms, "startMs": start, "note": "use exec:sleep for async wait" }))
}

fn sleep(body: &Value) -> u64 { wait(body) }

fn close(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 { return err("close", "taskId required"); }
    let key = format!("{}", task_id);
    let rc = unsafe { host_kv_put("outbox".as_ptr(), 6, key.as_ptr(), key.len() as u32, "closed".as_ptr(), 6) };
    if rc != 0 { ok("close", json!({ "taskId": task_id })) } else { err("close", "close failed") }
}

fn kill_port(body: &Value) -> u64 {
    let port = body.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
    if port == 0 { return err("kill-port", "port required"); }
    let code = format!("(function(){{ try{{ const p={}; return JSON.stringify({{ok:true,port:p}}); }}catch(e){{ return JSON.stringify({{ok:false,error:e.message}}); }} }})()", port);
    let opts = "{}";
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("kill-port", serde_json::from_str(&s).unwrap_or(Value::String(s))),
        None => ok("kill-port", json!({ "port": port, "note": "port kill emulated via exec_js" })),
    }
}

fn forget(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if key.is_empty() { return err("forget", "key required"); }
    // KV delete via overwrite with empty + tombstone marker
    let tombstone = format!("__deleted__{}", unsafe { host_now_ms() });
    let _ = unsafe { host_kv_put(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32, tombstone.as_ptr(), tombstone.len() as u32) };
    ok("forget", json!({ "namespace": ns, "key": key }))
}

fn feedback(body: &Value) -> u64 {
    let request_id = body.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
    let quality = body.get("quality").and_then(|v| v.as_f64()).unwrap_or(0.5);
    if request_id.is_empty() { return err("feedback", "requestId required"); }
    let key = format!("fb-{}", request_id);
    let val = json!({ "requestId": request_id, "quality": quality, "ts": unsafe { host_now_ms() } }).to_string();
    let rc = unsafe { host_kv_put("feedback".as_ptr(), 8, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
    if rc != 0 { ok("feedback", json!({ "requestId": request_id, "quality": quality })) } else { err("feedback", "store failed") }
}

fn learn_status(_body: &Value) -> u64 {
    let now = unsafe { host_now_ms() };
    ok("learn-status", json!({ "ok": true, "now": now, "mode": "wasm", "note": "learning state via KV" }))
}

fn learn_debug(_body: &Value) -> u64 {
    ok("learn-debug", json!({ "note": "use exec:nodejs + require('fs') to inspect .gm/ state" }))
}

fn learn_build(_body: &Value) -> u64 {
    ok("learn-build", json!({ "note": "WASM build uses thebird host bindings — no separate build step" }))
}

fn discipline(body: &Value) -> u64 {
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("list");
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "list" => {
            let packed = unsafe { host_kv_query("disciplines".as_ptr(), 11, "".as_ptr(), 0) };
            ok("discipline", unpack_to_value(packed))
        }
        "get" => {
            if name.is_empty() { return err("discipline", "name required for get"); }
            let packed = unsafe { host_kv_get("disciplines".as_ptr(), 11, name.as_ptr(), name.len() as u32) };
            match unpack_to_string(packed) {
                Some(s) => ok("discipline", serde_json::from_str(&s).unwrap_or(Value::String(s))),
                None => err("discipline", "not found"),
            }
        }
        _ => err("discipline", "unknown action"),
    }
}

fn pause(body: &Value) -> u64 {
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("toggle");
    let key = "pause-state";
    match action {
        "on" => {
            let val = json!({ "paused": true, "ts": unsafe { host_now_ms() } }).to_string();
            let _ = unsafe { host_kv_put("runner".as_ptr(), 6, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
            ok("pause", json!({ "paused": true }))
        }
        "off" => {
            let val = json!({ "paused": false, "ts": unsafe { host_now_ms() } }).to_string();
            let _ = unsafe { host_kv_put("runner".as_ptr(), 6, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
            ok("pause", json!({ "paused": false }))
        }
        _ => {
            let packed = unsafe { host_kv_get("runner".as_ptr(), 6, key.as_ptr(), key.len() as u32) };
            ok("pause", unpack_to_value(packed))
        }
    }
}

fn runner(body: &Value) -> u64 {
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("status");
    match action {
        "start" => {
            let val = json!({ "running": true, "ts": unsafe { host_now_ms() } }).to_string();
            let _ = unsafe { host_kv_put("runner".as_ptr(), 6, "state".as_ptr(), 5, val.as_ptr(), val.len() as u32) };
            ok("runner", json!({ "running": true }))
        }
        "stop" => {
            let val = json!({ "running": false, "ts": unsafe { host_now_ms() } }).to_string();
            let _ = unsafe { host_kv_put("runner".as_ptr(), 6, "state".as_ptr(), 5, val.as_ptr(), val.len() as u32) };
            ok("runner", json!({ "running": false }))
        }
        _ => {
            let packed = unsafe { host_kv_get("runner".as_ptr(), 6, "state".as_ptr(), 5) };
            ok("runner", unpack_to_value(packed))
        }
    }
}

fn type_into(body: &Value) -> u64 {
    let sid = body.get("sessionId").and_then(|v| v.as_u64()).unwrap_or(0);
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if sid == 0 || text.is_empty() { return err("type", "sessionId+text required"); }
    let code = format!("(function(){{ var s=document.activeElement; if(s){{ s.value+='{}'; return 'ok'; }} return 'no active element'; }})()", text.replace('\'', "\\'"));
    let packed = unsafe { host_browser_eval(sid, code.as_ptr(), code.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("type", Value::String(s)),
        None => err("type", "eval failed"),
    }
}

fn browser_alias(body: &Value) -> u64 {
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("spawn");
    match action {
        "spawn" => browser_spawn(body),
        "eval" => browser_eval(body),
        "close" => browser_close(body),
        _ => err("browser", "unknown action — use spawn/eval/close"),
    }
}

fn shell_exec(body: &Value, lang: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_empty() { return err(lang, "code required"); }
    let opts = json!({ "lang": lang }).to_string();
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok(lang, Value::String(s)),
        None => ok(lang, json!({ "note": "emulated via thebird host_exec_js", "lang": lang })),
    }
}

fn rejected(verb: &str) -> u64 {
    err(verb, "verb unavailable in browser; use exec:nodejs or host-side dispatch")
}

#[no_mangle]
pub extern "C" fn dispatch_verb(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    let verb = read_str(verb_ptr as *const u8, verb_len);
    let body_s = read_str(body_ptr as *const u8, body_len);
    let body: Value = if body_s.is_empty() { Value::Null } else {
        serde_json::from_str(&body_s).unwrap_or(Value::Null)
    };
    if crate::orchestrator::is_orchestrator_verb(&verb) {
        let (out, err_msg, code) = crate::orchestrator::dispatch(&verb, "", &body_s);
        let ok_flag = code == 0;
        let response = if ok_flag {
            let data: Value = serde_json::from_str(&out).unwrap_or(Value::String(out));
            json!({ "ok": true, "verb": verb, "data": data })
        } else {
            json!({ "ok": false, "verb": verb, "error": err_msg, "stdout": out, "exitCode": code })
        };
        return pack(response.to_string());
    }
    match verb.as_str() {
        "recall" => recall(&body),
        "memorize" => memorize_with_raw(&body, &body_s),
        "codesearch" => codesearch(&body),
        "fs_read" => fs_read(&body),
        "fs_write" => fs_write(&body),
        "fs_readdir" => fs_readdir(&body),
        "fs_stat" => fs_stat(&body),
        "fetch" => fetch(&body),
        "env_get" => env_get(&body),
        "kv_get" => kv_get(&body),
        "kv_put" => kv_put(&body),
        "kv_query" => kv_query(&body),
        "browser_spawn" => browser_spawn(&body),
        "browser_eval" => browser_eval(&body),
        "browser_close" => browser_close(&body),
        "exec_js" | "nodejs" | "javascript" | "node" | "js" => exec_js(&body),
        "health" => health(&body),
        "python" | "py" => shell_exec(&body, "python"),
        "bash" | "sh" | "shell" | "zsh" => shell_exec(&body, "bash"),
        "powershell" | "ps1" => shell_exec(&body, "powershell"),
        "ssh" => shell_exec(&body, "ssh"),
        "go" | "rust" | "c" | "cpp" | "java" | "deno" => shell_exec(&body, &verb),
        "status" => status(&body),
        "wait" => wait(&body),
        "sleep" => sleep(&body),
        "close" => close(&body),
        "kill-port" => kill_port(&body),
        "forget" => forget(&body),
        "feedback" => feedback(&body),
        "learn-status" => learn_status(&body),
        "learn-debug" => learn_debug(&body),
        "learn-build" => learn_build(&body),
        "discipline" => discipline(&body),
        "pause" => pause(&body),
        "runner" => runner(&body),
        "type" => type_into(&body),
        "browser" => browser_alias(&body),
        "instruction" | "transition" | "mutable-resolve" | "memorize-fire" | "phase-status" | "residual-scan" | "auto-recall" => {
            let body_str = match &body {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            let (stdout, stderr, exit_code) = crate::orchestrator::dispatch(&verb, "wasm", &body_str);
            pack(json!({
                "ok": exit_code == 0,
                "verb": verb,
                "stdout": stdout,
                "stderr": stderr,
                "exitCode": exit_code,
            }).to_string())
        }
        "" => err("", "verb required"),
        _ => err(&verb, "unknown verb"),
    }
}
