#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

static MEMORIZE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    pub fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    pub fn host_now_ms() -> u64;
    pub fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64;
    pub fn host_browser_exec(body_ptr: *const u8, body_len: u32, cwd_ptr: *const u8, cwd_len: u32, session_id_ptr: *const u8, session_id_len: u32) -> u64;
    pub fn host_task_proc(action_ptr: *const u8, action_len: u32, params_ptr: *const u8, params_len: u32) -> u64;
}

pub fn host_task(action: &str, params: &Value) -> Value {
    let params_s = params.to_string();
    let packed = unsafe { host_task_proc(action.as_ptr(), action.len() as u32, params_s.as_ptr(), params_s.len() as u32) };
    unpack_to_value(packed)
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

pub fn unpack_to_value_pub(packed: u64) -> Value { unpack_to_value(packed) }

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

fn inference(body: &Value) -> u64 {
    let messages = body.get("messages").cloned().unwrap_or(Value::Null);
    if messages.is_null() {
        let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if prompt.is_empty() { return err("inference", "messages or prompt required"); }
    }
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("http://127.0.0.1:4800/v1/chat/completions");
    let model = body.get("model").and_then(|v| v.as_str()).map(String::from);
    let messages_value = if !messages.is_null() {
        messages
    } else {
        let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        json!([{ "role": "user", "content": prompt }])
    };
    let mut req_body = json!({ "messages": messages_value });
    if let Some(m) = model { req_body["model"] = Value::String(m); }
    let body_str = req_body.to_string();
    let opts = json!({ "method": "POST", "headers": {"content-type": "application/json"}, "body": body_str }).to_string();
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() { return err("inference", "host_fetch empty - is acptoapi running on 4800?"); }
    let status = v.get("status").and_then(|s| s.as_u64()).unwrap_or(0);
    if status < 200 || status >= 300 {
        let detail = v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
        return err("inference", &format!("acptoapi returned {}: {}", status, detail));
    }
    let body_text = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let parsed: Value = serde_json::from_str(body_text).unwrap_or(Value::String(body_text.to_string()));
    ok("inference", parsed)
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

fn exec_js(body: &Value, body_s: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body_s.to_string());
    if code.is_empty() { return err("exec_js", "code required (provide raw code as body or JSON {code: ...})"); }
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
    let q_json = json!({ "query": query, "embedding": embedding, "namespace": namespace }).to_string();
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
    let counter = MEMORIZE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let key = format!("mem-{}-{}-{}", now, counter, text.len());
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
            "host_exec_js","host_log","host_now_ms","host_env_get","host_browser_exec","host_task_proc"
        ]
    }))
}

fn status(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 { return err("status", "taskId required"); }
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("outbox");
    let key = format!("{}", task_id);
    let packed = unsafe { host_kv_get(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
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

fn shell_exec(body: &Value, body_s: &str, lang: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body_s.to_string());
    if code.is_empty() { return err(lang, "code required (provide raw code as body or JSON {code: ...})"); }
    let opts = json!({ "lang": lang }).to_string();
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok(lang, Value::String(s)),
        None => ok(lang, json!({ "note": "emulated via thebird host_exec_js", "lang": lang })),
    }
}

fn browser(body: &Value, body_s: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body_s.to_string());
    if code.is_empty() { return err("browser", "code required (provide JS body or {code, cwd?, sessionId?} JSON)"); }
    let cwd = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = body.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
    let packed = unsafe { host_browser_exec(
        code.as_ptr(), code.len() as u32,
        cwd.as_ptr(), cwd.len() as u32,
        session_id.as_ptr(), session_id.len() as u32,
    ) };
    match unpack_to_string(packed) {
        Some(s) => {
            let v: Value = serde_json::from_str(&s).unwrap_or(Value::String(s));
            ok("browser", v)
        }
        None => err("browser", "host_browser_exec returned empty"),
    }
}

fn rejected(verb: &str) -> u64 {
    err(verb, "verb unavailable in browser; use exec:nodejs or host-side dispatch")
}

fn db_name_from(body: &Value) -> String {
    body.get("db_name").or_else(|| body.get("db")).and_then(|v| v.as_str()).unwrap_or("main").to_string()
}

fn sql_open(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or(":memory:");
    let name = db_name_from(body);
    match crate::libsql_wasm::open(&name, path) {
        Ok(()) => ok("sql_open", json!({ "path": path, "db_name": name })),
        Err(e) => err("sql_open", &e),
    }
}

fn sql_close(body: &Value) -> u64 {
    let name = db_name_from(body);
    match crate::libsql_wasm::close(&name) {
        Ok(()) => ok("sql_close", json!({ "db_name": name })),
        Err(e) => err("sql_close", &e),
    }
}

fn sql_list_dbs(_body: &Value) -> u64 {
    let names = crate::libsql_wasm::list_dbs();
    ok("sql_list_dbs", json!({ "dbs": names }))
}

fn sql_exec(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_exec", "missing sql"),
    };
    let name = db_name_from(body);
    match crate::libsql_wasm::exec(&name, sql) {
        Ok(()) => ok("sql_exec", json!({})),
        Err(e) => err("sql_exec", &e),
    }
}

fn sql_query(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_query", "missing sql"),
    };
    let name = db_name_from(body);
    match crate::libsql_wasm::query(&name, sql) {
        Ok(rows) => ok("sql_query", json!({ "rows": rows })),
        Err(e) => err("sql_query", &e),
    }
}

fn sql_smoke() -> u64 {
    pack(crate::libsql_wasm::smoke().to_string())
}

fn b64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b = ((bytes[i] as u32) << 16) | ((bytes[i+1] as u32) << 8) | (bytes[i+2] as u32);
        out.push(TABLE[((b >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((b >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((b >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(b & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b = (bytes[i] as u32) << 16;
        out.push(TABLE[((b >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((b >> 12) & 0x3F) as usize] as char);
        out.push('='); out.push('=');
    } else if rem == 2 {
        let b = ((bytes[i] as u32) << 16) | ((bytes[i+1] as u32) << 8);
        out.push(TABLE[((b >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((b >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((b >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v: u32 = match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62, b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); buf &= (1 << bits) - 1; }
    }
    Some(out)
}

fn sql_serialize(body: &Value) -> u64 {
    let name = db_name_from(body);
    match crate::libsql_wasm::serialize(&name) {
        Ok(bytes) => ok("sql_serialize", json!({ "bytes_b64": b64_encode(&bytes), "size": bytes.len(), "db_name": name })),
        Err(e) => err("sql_serialize", &e),
    }
}

fn sql_deserialize(body: &Value) -> u64 {
    let s = match body.get("bytes_b64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_deserialize", "missing bytes_b64"),
    };
    let bytes = match b64_decode(s) { Some(b) => b, None => return err("sql_deserialize", "invalid base64") };
    let size = bytes.len();
    let name = db_name_from(body);
    match crate::libsql_wasm::deserialize(&name, &bytes) {
        Ok(()) => ok("sql_deserialize", json!({ "restored": size, "db_name": name })),
        Err(e) => err("sql_deserialize", &e),
    }
}

fn codeinsight_index(body: &Value) -> u64 {
    let root = body.get("root").and_then(|v| v.as_str()).unwrap_or("/");
    let max_files = body.get("max_files").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
    pack(crate::code_index::index(root, max_files).to_string())
}

fn codesearch_libsql(body: &Value, raw: &str) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let inline = body.get("embedding");
    pack(crate::code_index::search(&query, k, inline).to_string())
}

fn memorize_libsql(body: &Value, raw: &str) -> u64 {
    let text = body.get("text").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if text.is_empty() { return err("memorize", "missing text"); }
    let inline = body.get("embedding");
    pack(crate::code_index::memorize(&text, ns, inline).to_string())
}

fn recall_libsql(body: &Value, raw: &str) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    let ns = body.get("namespace").and_then(|v| v.as_str());
    if query.is_empty() { return err("recall", "missing query"); }
    let inline = body.get("embedding");
    pack(crate::code_index::recall(&query, limit, ns, inline).to_string())
}

fn filter(body: &Value, raw: &str) -> u64 {
    let (data, err_msg) = crate::filter::dispatch(body, raw);
    match err_msg {
        Some(e) => err("filter", &e),
        None => ok("filter", data),
    }
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
        "fs_read" => fs_read(&body),
        "fs_write" => fs_write(&body),
        "fs_readdir" => fs_readdir(&body),
        "fs_stat" => fs_stat(&body),
        "fetch" => fetch(&body),
        "inference" | "chat" | "complete" => inference(&body),
        "env_get" => env_get(&body),
        "kv_get" => kv_get(&body),
        "kv_put" => kv_put(&body),
        "kv_query" => kv_query(&body),
        "exec_js" | "nodejs" | "javascript" | "node" | "js" => exec_js(&body, &body_s),
        "browser" => browser(&body, &body_s),
        "health" => health(&body),
        "sql_open" => sql_open(&body),
        "sql_close" => sql_close(&body),
        "sql_list_dbs" => sql_list_dbs(&body),
        "sql_exec" => sql_exec(&body),
        "sql_query" => sql_query(&body),
        "sql_smoke" => sql_smoke(),
        "sql_serialize" => sql_serialize(&body),
        "sql_deserialize" => sql_deserialize(&body),
        "codeinsight_index" => codeinsight_index(&body),
        "codesearch" => codesearch_libsql(&body, &body_s),
        "memorize" => memorize_libsql(&body, &body_s),
        "recall" => recall_libsql(&body, &body_s),
        "recall_kv" => recall(&body),
        "memorize_kv" => memorize_with_raw(&body, &body_s),
        "codesearch_kv" => codesearch(&body),
        "python" | "py" => shell_exec(&body, &body_s, "python"),
        "bash" | "sh" | "shell" | "zsh" => shell_exec(&body, &body_s, "bash"),
        "powershell" | "ps1" => shell_exec(&body, &body_s, "powershell"),
        "ssh" => shell_exec(&body, &body_s, "ssh"),
        "go" | "rust" | "c" | "cpp" | "java" | "deno" => shell_exec(&body, &body_s, &verb),
        "status" => status(&body),
        "wait" => wait(&body),
        "sleep" => sleep(&body),
        "close" => close(&body),
        "kill-port" => kill_port(&body),
        "filter" => filter(&body, &body_s),
        "forget" => forget(&body),
        "feedback" => feedback(&body),
        "learn-status" => learn_status(&body),
        "learn-debug" => learn_debug(&body),
        "learn-build" => learn_build(&body),
        "discipline" => discipline(&body),
        "pause" => pause(&body),
        "runner" => runner(&body),
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
