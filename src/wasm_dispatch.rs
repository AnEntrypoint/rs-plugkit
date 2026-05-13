#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

extern "C" {
    pub fn host_fs_read(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fs_write(path_ptr: *const u8, path_len: u32, data_ptr: *const u8, data_len: u32) -> u32;
    pub fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fs_stat(path_ptr: *const u8, path_len: u32) -> u64;
    pub fn host_fetch(url_ptr: *const u8, url_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_browser_spawn(url_ptr: *const u8, url_len: u32) -> u64;
    pub fn host_browser_eval(session_id: u64, code_ptr: *const u8, code_len: u32) -> u64;
    pub fn host_browser_close(session_id: u64) -> u32;
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

fn err(verb: &str, reason: &str) -> u64 {
    pack(json!({ "ok": false, "verb": verb, "error": reason }).to_string())
}

fn ok(verb: &str, data: Value) -> u64 {
    pack(json!({ "ok": true, "verb": verb, "data": data }).to_string())
}

fn fs_read(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_read", "path required"); }
    let packed = unsafe { host_fs_read(path.as_ptr(), path.len() as u32) };
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 || l == 0 { return err("fs_read", "host_fs_read returned empty"); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let s = String::from_utf8_lossy(&bytes).into_owned();
    ok("fs_read", Value::String(s))
}

fn fs_write(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_write", "path required"); }
    let rc = unsafe { host_fs_write(path.as_ptr(), path.len() as u32, data.as_ptr(), data.len() as u32) };
    if rc == 0 { ok("fs_write", json!({ "bytes": data.len() })) } else { err("fs_write", &format!("rc={}", rc)) }
}

fn fs_readdir(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 { return err("fs_readdir", "empty"); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    ok("fs_readdir", v)
}

fn fs_stat(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_stat", "path required"); }
    let packed = unsafe { host_fs_stat(path.as_ptr(), path.len() as u32) };
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 { return err("fs_stat", "not found"); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    ok("fs_stat", v)
}

fn fetch(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() { return err("fetch", "url required"); }
    let opts = body.get("opts").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 { return err("fetch", "host_fetch empty"); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    ok("fetch", v)
}

fn env_get(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("env_get", "key required"); }
    let packed = unsafe { host_env_get(key.as_ptr(), key.len() as u32) };
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 { return ok("env_get", Value::Null); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let s = String::from_utf8_lossy(&bytes).into_owned();
    ok("env_get", Value::String(s))
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
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 { return ok("browser_eval", Value::Null); }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    let s = String::from_utf8_lossy(&bytes).into_owned();
    ok("browser_eval", Value::String(s))
}

fn browser_close(body: &Value) -> u64 {
    let sid = body.get("sessionId").and_then(|v| v.as_u64()).unwrap_or(0);
    if sid == 0 { return err("browser_close", "sessionId required"); }
    let rc = unsafe { host_browser_close(sid) };
    if rc == 0 { ok("browser_close", Value::Null) } else { err("browser_close", &format!("rc={}", rc)) }
}

fn recall(body: &Value) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    if query.is_empty() { return err("recall", "query required"); }
    let learn = rs_learn::Learn::new();
    match learn.recall(query, limit) {
        Ok(hits) => ok("recall", serde_json::to_value(&hits).unwrap_or(Value::Null)),
        Err(e) => err("recall", &format!("{:?}", e)),
    }
}

fn memorize(body: &Value) -> u64 {
    let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if text.is_empty() { return err("memorize", "text required"); }
    let learn = rs_learn::Learn::new();
    match learn.memorize(text, namespace) {
        Ok(()) => ok("memorize", json!({ "namespace": namespace })),
        Err(e) => err("memorize", &format!("{:?}", e)),
    }
}

fn codesearch(body: &Value) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    if query.is_empty() { return err("codesearch", "query required"); }
    let root = body.get("root").and_then(|v| v.as_str()).unwrap_or(".");
    let searcher = rs_search::Searcher::new(root);
    let hits = searcher.search(query, k);
    let json_hits: Vec<Value> = hits.into_iter().map(|h| json!({
        "id": h.id, "score": h.score, "snippet": h.snippet
    })).collect();
    ok("codesearch", Value::Array(json_hits))
}

fn spool_dispatch(verb: &str, body: &Value) -> u64 {
    let task_json = json!({
        "verb": verb,
        "taskId": body.get("taskId").cloned().unwrap_or(json!(0)),
        "lang": body.get("lang").cloned().unwrap_or(json!("nodejs")),
        "code": body.get("code").cloned().unwrap_or(json!("")),
        "cwd": body.get("cwd").cloned().unwrap_or(json!("")),
        "timeoutMs": body.get("timeoutMs").cloned().unwrap_or(json!(300000)),
    }).to_string();
    let rc = rs_exec::wasm_spool::execute(&task_json);
    if rc == 0 { ok(verb, json!({ "dispatched": true })) } else { err(verb, &format!("spool rc={}", rc)) }
}

fn rejected(verb: &str) -> u64 {
    err(verb, "language unavailable in browser; use exec:nodejs")
}

#[no_mangle]
pub extern "C" fn dispatch_verb(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    let verb = read_str(verb_ptr as *const u8, verb_len);
    let body_s = read_str(body_ptr as *const u8, body_len);
    let body: Value = if body_s.is_empty() { Value::Null } else {
        serde_json::from_str(&body_s).unwrap_or(Value::Null)
    };
    match verb.as_str() {
        "recall" => recall(&body),
        "memorize" => memorize(&body),
        "codesearch" => codesearch(&body),
        "fs_read" => fs_read(&body),
        "fs_write" => fs_write(&body),
        "fs_readdir" => fs_readdir(&body),
        "fs_stat" => fs_stat(&body),
        "fetch" => fetch(&body),
        "env_get" => env_get(&body),
        "browser_spawn" => browser_spawn(&body),
        "browser_eval" => browser_eval(&body),
        "browser_close" => browser_close(&body),
        "python" | "bash" | "ssh" | "powershell" | "ps1" | "sh" | "zsh" => rejected(&verb),
        "nodejs" | "javascript" | "node" | "js"
        | "status" | "wait" | "sleep" | "close" | "kill-port" | "forget"
        | "feedback" | "learn-status" | "learn-debug" | "learn-build"
        | "discipline" | "pause" | "health" | "runner" | "type" | "browser"
            => spool_dispatch(&verb, &body),
        "" => err("", "verb required"),
        _ => err(&verb, "unknown verb"),
    }
}
