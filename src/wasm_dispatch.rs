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
    pub fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
    pub fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    pub fn host_vec_search(q_ptr: *const u8, q_len: u32, k: u32) -> u64;
    pub fn host_vec_embed(text_ptr: *const u8, text_len: u32, out_ptr: *mut f32, out_len: u32) -> i32;
    pub fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    pub fn host_now_ms() -> u64;
    pub fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64;
    pub fn host_browser_exec(body_ptr: *const u8, body_len: u32, cwd_ptr: *const u8, cwd_len: u32, session_id_ptr: *const u8, session_id_len: u32) -> u64;
    pub fn host_task_proc(action_ptr: *const u8, action_len: u32, params_ptr: *const u8, params_len: u32) -> u64;
    pub fn host_git(args_ptr: *const u8, args_len: u32, cwd_ptr: *const u8, cwd_len: u32) -> u64;
}

pub fn host_task(action: &str, params: &Value) -> Value {
    let params_s = params.to_string();
    let packed = unsafe { host_task_proc(action.as_ptr(), action.len() as u32, params_s.as_ptr(), params_s.len() as u32) };
    unpack_to_value(packed)
}

pub fn git_call(args: &str, cwd: Option<&str>) -> Value {
    let cwd_s = cwd.unwrap_or("");
    let packed = unsafe { host_git(args.as_ptr(), args.len() as u32, cwd_s.as_ptr(), cwd_s.len() as u32) };
    unpack_to_value(packed)
}

pub fn git_porcelain() -> String {
    let v = git_call("status --porcelain", None);
    v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string()
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

fn err_json(verb: &str, detail: Value) -> u64 {
    let mut obj = json!({ "ok": false, "verb": verb });
    if let Some(map) = detail.as_object() {
        for (k, v) in map {
            obj[k] = v.clone();
        }
    }
    pack(obj.to_string())
}

fn ok(verb: &str, data: Value) -> u64 {
    let hint = if verb == "instruction" { Value::Null } else { json!("instruction") };
    pack(json!({ "ok": true, "verb": verb, "data": data, "next_dispatch_hint": hint }).to_string())
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
    if v.is_null() { return err("inference", "host_fetch empty - inference must be served via the callback protocol (agent callback)"); }
    let status = v.get("status").and_then(|s| s.as_u64()).unwrap_or(0);
    if status < 200 || status >= 300 {
        let detail = v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
        return err("inference", &format!("inference endpoint returned {}: {}", status, detail));
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

fn lang(body: &Value) -> u64 {
    let project_dir = body.get("projectDir").and_then(|v| v.as_str()).unwrap_or("");
    let command = body.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if project_dir.is_empty() { return err("lang", "projectDir required"); }
    if command.is_empty() { return err("lang", "command required"); }
    let timeout_ms = body.get("timeoutMs").and_then(|v| v.as_u64()).unwrap_or(35000);
    let runner_js = format!(
        r#"(async () => {{
  const fs = require('fs');
  const path = require('path');
  const projectDir = {project_dir};
  const command = {command};
  const code = {code};
  const langDir = path.join(projectDir, 'lang');
  if (!fs.existsSync(langDir)) {{ process.stdout.write(JSON.stringify({{ok:false, error:'no-lang-dir', langDir}})); return; }}
  const files = fs.readdirSync(langDir).filter(f => f.endsWith('.js') && f !== 'loader.js');
  const plugins = files.reduce((acc, f) => {{
    try {{
      const p = require(path.join(langDir, f));
      if (p && typeof p.id === 'string' && p.exec && p.exec.match instanceof RegExp && typeof p.exec.run === 'function') acc.push(p);
    }} catch (_) {{}}
    return acc;
  }}, []);
  const plugin = plugins.find(p => p.exec.match.test(command));
  if (!plugin) {{ process.stdout.write(JSON.stringify({{ok:false, error:'no-plugin-matched', command, available: plugins.map(p => p.id)}})); return; }}
  const t0 = Date.now();
  try {{
    const out = await Promise.race([
      Promise.resolve(plugin.exec.run(code, projectDir)),
      new Promise((_, rej) => setTimeout(() => rej(new Error('plugin-timeout')), 30000))
    ]);
    process.stdout.write(JSON.stringify({{ok:true, plugin_id: plugin.id, output: String(out), ms: Date.now() - t0}}));
  }} catch (e) {{
    process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e), plugin_id: plugin.id, ms: Date.now() - t0}}));
  }}
}})().catch(e => {{ process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e)}})); }})"#,
        project_dir = serde_json::to_string(project_dir).unwrap_or_else(|_| "\"\"".to_string()),
        command = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".to_string()),
        code = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".to_string()),
    );
    let opts = json!({"timeoutMs": timeout_ms}).to_string();
    let packed = unsafe { host_exec_js(runner_js.as_ptr(), runner_js.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => {
            let envelope: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
            if envelope.is_null() {
                return err("lang", "host_exec_js returned non-JSON");
            }
            let stdout = envelope.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let exit_code = envelope.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let timed_out = envelope.get("timed_out").and_then(|v| v.as_bool()).unwrap_or(false);
            if timed_out { return err("lang", "host_exec_js timed out"); }
            if exit_code != 0 {
                let stderr = envelope.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
                return err_json("lang", json!({"error":"runner exit non-zero","exit_code":exit_code,"stderr":stderr,"stdout":stdout}));
            }
            let inner: Value = serde_json::from_str(stdout).unwrap_or_else(|_| Value::String(stdout.to_string()));
            ok("lang", inner)
        }
        None => err("lang", "host_exec_js returned empty"),
    }
}

fn exec_js(body: &Value, body_s: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body_s.to_string());
    if code.is_empty() { return err("exec_js", "code required (provide raw code as body or JSON {code: ...})"); }
    let timeout_ms = match crate::validation::validate_timeout_ms(body, true) {
        Ok(n) => n,
        Err(detail) => return err_json("exec_js", detail),
    };
    let mut opts_obj = body.get("opts").cloned().unwrap_or_else(|| json!({}));
    if let Some(map) = opts_obj.as_object_mut() {
        map.insert("timeoutMs".to_string(), json!(timeout_ms));
    } else {
        opts_obj = json!({"timeoutMs": timeout_ms});
    }
    let opts = opts_obj.to_string();
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts.as_ptr(), opts.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("exec_js", Value::String(s)),
        None => ok("exec_js", Value::Null),
    }
}

pub fn host_kv_read(namespace: &str, key: &str) -> Option<String> {
    if key.is_empty() { return None; }
    let packed = unsafe { host_kv_get(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
    unpack_to_string(packed)
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
    check_sigil_ignored(query, namespace);
    let derived_query = query.to_string();
    let embedding = crate::embed::embed_text_json_query(query).unwrap_or(Value::Null);
    let q_json = json!({ "query": query, "embedding": embedding, "namespace": namespace }).to_string();
    let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, limit) };
    let vec_hits = unpack_to_value(packed);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        let annotated = annotate_hits_with_score(vec_hits);
        let reranked = crate::orchestrator::recall::rerank_by_adapter(query, annotated);
        return ok("recall", json!({
            "mode": "vector_top_k",
            "namespace": namespace,
            "derived_query": derived_query,
            "hits": reranked,
        }));
    }
    let packed = unsafe { host_kv_query(namespace.as_ptr(), namespace.len() as u32, query.as_ptr(), query.len() as u32) };
    let kv_hits = unpack_to_value(packed);
    let annotated = annotate_hits_with_score(kv_hits);
    ok("recall", json!({
        "mode": "fallback_like",
        "namespace": namespace,
        "derived_query": derived_query,
        "hits": annotated,
    }))
}

static RECALL_SCORE_UNAVAILABLE_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SIGIL_IGNORED_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn annotate_hits_with_score(v: Value) -> Value {
    let arr = match v {
        Value::Array(a) => a,
        other => return other,
    };
    let mut out = Vec::with_capacity(arr.len());
    let mut any_missing = false;
    for hit in arr {
        match hit {
            Value::Object(mut map) => {
                if !map.contains_key("score") {
                    map.insert("score".to_string(), Value::Null);
                    any_missing = true;
                }
                out.push(Value::Object(map));
            }
            other => {
                any_missing = true;
                out.push(json!({ "value": other, "score": Value::Null }));
            }
        }
    }
    if any_missing && !RECALL_SCORE_UNAVAILABLE_FIRED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        emit_event("recall_score_unavailable", json!({
            "reason": "host_vec_search return shape elides per-hit score",
        }));
    }
    Value::Array(out)
}

fn check_sigil_ignored(text: &str, namespace: &str) {
    if namespace != "default" { return; }
    let sigil = extract_sigil(text);
    if let Some(s) = sigil {
        if !SIGIL_IGNORED_FIRED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            emit_event("discipline_sigil_ignored", json!({
                "sigil": s,
                "fallback_namespace": "default",
            }));
        }
    }
}

fn extract_sigil(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let first_tok = trimmed.split_whitespace().next()?;
    let rest = first_tok.strip_prefix('@')?;
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if name.is_empty() { return None; }
    Some(format!("@{}", name))
}

fn memorize_with_raw(body: &Value, raw: &str) -> u64 {
    let text = body.get("text").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if text.is_empty() { return err("memorize", "text required"); }
    let text = text.as_str();
    check_sigil_ignored(text, namespace);
    let now = unsafe { host_now_ms() };
    let counter = MEMORIZE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let key = format!("mem-{}-{}-{}", now, counter, text.len());
    // Embed FIRST and write text only on success — atomic by construction. The prior order (text
    // then embed) left a text-only orphan whenever embed_text_json returned None (cold/failed
    // model): the memory persisted with no vector, unreachable by vector recall (only kv-LIKE
    // fallback), polluting the store. A memory without an embedding is not properly recallable, so
    // refuse it rather than orphan it. Witnessed: 12 text-without-vec orphans in default ns from
    // early-May embed failures.
    let emb = match crate::embed::embed_text_json(text) {
        Some(e) => e,
        None => return err("memorize", "embed failed; refusing to write a text-only memory with no vector (un-vector-recallable orphan)"),
    };
    let rc = unsafe { host_kv_put(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32, text.as_ptr(), text.len() as u32) };
    if rc == 0 { return err("memorize", "kv_put failed"); }
    let vec_ns = format!("{}-vec", namespace);
    let emb_str = emb.to_string();
    let _ = unsafe { host_kv_put(vec_ns.as_ptr(), vec_ns.len() as u32, key.as_ptr(), key.len() as u32, emb_str.as_ptr(), emb_str.len() as u32) };
    ok("memorize", json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": true}))
}

fn memorize_prune(body: &Value) -> u64 {
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    // Mode 1: explicit key(s) — delete exactly those memories. Use when the agent KNOWS a specific
    // memory is wrong/superseded (e.g. a recall hit it just judged stale).
    let mut keys: Vec<String> = Vec::new();
    if let Some(k) = body.get("key").and_then(|v| v.as_str()) {
        if !k.is_empty() { keys.push(k.to_string()); }
    }
    if let Some(arr) = body.get("keys").and_then(|v| v.as_array()) {
        for v in arr { if let Some(s) = v.as_str() { keys.push(s.to_string()); } }
    }
    if !keys.is_empty() {
        let mut deleted = Vec::new();
        for key in &keys {
            let rc = unsafe { host_kv_delete(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
            if rc != 0 {
                deleted.push(key.clone());
                invalidate_memory_edge(key);
                emit_event("memory.pruned", json!({"key": key, "namespace": namespace, "mode": "explicit-key"}));
            }
        }
        return ok("memorize-prune", json!({"namespace": namespace, "deleted": deleted, "mode": "explicit-key"}));
    }
    // Mode 2: query + min_score — surface candidate stale memories for the agent to review. Pruning
    // bad memory matters more than retention, but a blind similarity-delete is itself a bad-memory
    // generator; so query mode is REVIEW-ONLY by default (returns candidates with keys), and only
    // deletes when confirm:true is passed alongside the explicit candidate keys. This keeps the
    // destructive step under the agent's judgment, never an automatic similarity heuristic.
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return err("memorize-prune", "provide `key`/`keys` to delete, or `query` to list prune candidates");
    }
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let embedding = crate::embed::embed_text_json_query(query).unwrap_or(Value::Null);
    let q_json = json!({ "query": query, "embedding": embedding, "namespace": namespace }).to_string();
    let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, k) };
    let hits = unpack_to_value(packed);
    ok("memorize-prune", json!({
        "namespace": namespace,
        "mode": "review",
        "candidates": hits,
        "note": "Review-only: re-dispatch memorize-prune with {keys:[...]} naming the stale ones to delete. Pruning is agent-judged, never auto-similarity-deleted.",
    }))
}

fn codesearch(body: &Value) -> u64 {
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    if query.is_empty() { return err("codesearch", "query required"); }
    let embedding = crate::embed::embed_text_json_query(query).unwrap_or(Value::Null);
    let q_json = json!({ "query": query, "embedding": embedding, "namespace": "codeinsight" }).to_string();
    let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, k) };
    let vec_hits = unpack_to_value(packed);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        return ok("codesearch", vec_hits);
    }
    let ns = "codeinsight";
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, query.as_ptr(), query.len() as u32) };
    let hits = unpack_to_value(packed);
    let kv_empty = hits.is_null() || hits.as_array().map(|a| a.is_empty()).unwrap_or(true);
    // Sync-before-emit (paper §27): an empty codeinsight index means it was never built for this
    // project. code_index::index now dual-writes the file-vec store this verb reads, so an autobuild
    // then retry returns real hits. The auto_indexed guard prevents a rebuild loop when the tree
    // genuinely has no indexable chunks.
    if kv_empty && !body.get("auto_indexed").and_then(|v| v.as_bool()).unwrap_or(false) {
        let _ = crate::code_index::index(".", 500);
        let mut retry = body.clone();
        if let Some(obj) = retry.as_object_mut() {
            obj.insert("auto_indexed".to_string(), Value::Bool(true));
        }
        return codesearch(&retry);
    }
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
            "host_vec_search",
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

fn kv_ns_count(n: &str) -> usize {
    let packed = unsafe { host_kv_query(n.as_ptr(), n.len() as u32, "".as_ptr(), 0) };
    match unpack_to_value(packed) {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => 0,
    }
}

fn learn_status(body: &Value) -> u64 {
    let now = unsafe { host_now_ms() };
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let vec_ns = format!("{}-vec", ns);
    let text_rows = kv_ns_count(ns);
    let vec_rows = kv_ns_count(&vec_ns);
    ok("learn-status", json!({
        "ok": true,
        "now": now,
        "mode": "wasm",
        "namespace": ns,
        "text_rows": text_rows,
        "vec_rows": vec_rows,
        "balanced": text_rows == vec_rows,
    }))
}

fn learn_debug(_body: &Value) -> u64 {
    let packed = unsafe { host_kv_query("disciplines".as_ptr(), 11, "".as_ptr(), 0) };
    let names: Vec<String> = match unpack_to_value(packed) {
        Value::Array(a) => a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        Value::Object(o) => o.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let mut disciplines = Vec::new();
    for ns in std::iter::once("default".to_string()).chain(names.into_iter().filter(|n| n != "default")) {
        let text_rows = kv_ns_count(&ns);
        let vec_rows = kv_ns_count(&format!("{}-vec", ns));
        if text_rows == 0 && vec_rows == 0 { continue; }
        disciplines.push(json!({
            "namespace": ns,
            "text_rows": text_rows,
            "vec_rows": vec_rows,
            "balanced": text_rows == vec_rows,
            "orphans": (text_rows as i64 - vec_rows as i64).abs(),
        }));
    }
    ok("learn-debug", json!({
        "ok": true,
        "mode": "wasm",
        "disciplines": disciplines,
    }))
}

fn learn_build(_body: &Value) -> u64 {
    ok("learn-build", json!({ "note": "WASM build uses thebird host bindings — no separate build step" }))
}

pub struct PlugkitKv;

impl rs_learn::KvBackend for PlugkitKv {
    fn get(&self, namespace: &str, key: &str) -> Option<Vec<u8>> {
        let packed = unsafe { host_kv_get(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
        unpack_to_string(packed).map(|s| s.into_bytes())
    }
    fn put(&mut self, namespace: &str, key: &str, val: &[u8]) {
        let _ = unsafe { host_kv_put(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
    }
    fn list_prefix(&self, namespace: &str, prefix: &str) -> Vec<String> {
        let packed = unsafe { host_kv_query(namespace.as_ptr(), namespace.len() as u32, prefix.as_ptr(), prefix.len() as u32) };
        match unpack_to_value(packed) {
            Value::Array(a) => a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            Value::Object(o) => o.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }
}

fn learn(body: &Value, raw: &str) -> u64 {
    let _ = body;
    let mut session = rs_learn::LearnSession::new(PlugkitKv);
    let resp = rs_learn::dispatch_json(&mut session, raw.as_bytes());
    match String::from_utf8(resp) {
        Ok(s) => pack(s),
        Err(_) => err("learn", "rs_learn dispatch returned non-utf8"),
    }
}

const ROUTER_MODELS: &[&str] = &["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-7"];

fn learn_dispatch_value(req: &Value) -> Value {
    let mut session = rs_learn::LearnSession::new(PlugkitKv);
    let raw = req.to_string();
    let resp = rs_learn::dispatch_json(&mut session, raw.as_bytes());
    String::from_utf8(resp).ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}

fn invalidate_memory_edge(key: &str) {
    let now = unsafe { host_now_ms() };
    let req = serde_json::json!({
        "verb": "invalidate_edge",
        "body": { "edge_id": key, "invalid_at": now, "expired_at": now }
    });
    let _ = learn_dispatch_value(&req);
}

pub fn route_hint(prompt: &str, estimated_tokens: u64) -> Value {
    if prompt.trim().is_empty() { return Value::Null; }
    let embedding = match crate::embed::embed_text(prompt) {
        Some(e) => e,
        None => return Value::Null,
    };
    let route_req = serde_json::json!({
        "verb": "route",
        "body": { "embedding": embedding, "task_type": "code", "estimated_tokens": estimated_tokens }
    });
    let resp = learn_dispatch_value(&route_req);
    let routed_ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if routed_ok {
        if let Some(d) = resp.get("data") { if !d.is_null() { return d.clone(); } }
    }
    let targets: Vec<Value> = ROUTER_MODELS.iter().map(|m| Value::String((*m).into())).collect();
    let init_req = serde_json::json!({
        "verb": "init_router",
        "body": { "in_dim": 384, "targets": targets }
    });
    let _ = learn_dispatch_value(&init_req);
    let resp2 = learn_dispatch_value(&route_req);
    if resp2.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(d) = resp2.get("data") { if !d.is_null() { return d.clone(); } }
    }
    let cfg = rs_learn::RouterConfig::new(384, ROUTER_MODELS.iter().map(|m| (*m).to_string()).collect());
    let mut router = rs_learn::Router::new(cfg);
    let mut ctx = rs_learn::RouteCtx::default();
    ctx.task_type = Some("code".into());
    ctx.estimated_tokens = estimated_tokens;
    let r = router.route(&embedding, &ctx);
    serde_json::json!({
        "model": r.model,
        "context_bucket": r.context_bucket,
        "temperature": r.temperature,
        "top_p": r.top_p,
        "confidence": r.confidence,
        "algo": r.algo,
        "exploration": r.exploration,
    })
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
    let timeout_ms = match crate::validation::validate_timeout_ms(body, false) {
        Ok(n) => n,
        Err(detail) => return err_json(lang, detail),
    };
    let opts = json!({ "lang": lang, "timeoutMs": timeout_ms }).to_string();
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
    let project_path = body.get("projectPath").and_then(|v| v.as_str());
    pack(crate::code_index::memorize_at(&text, ns, inline, project_path).to_string())
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
    let project_path = body.get("projectPath").and_then(|v| v.as_str());
    pack(crate::code_index::recall_at(&query, limit, ns, inline, project_path).to_string())
}

fn exec_git(args: &str) -> String {
    let v = git_call(args, None);
    v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn log_deviation_push(event: &str, detail: &str) {
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

fn git_status(_body: &Value) -> u64 {
    let porcelain = git_porcelain();
    let mut modified: Vec<String> = vec![];
    let mut untracked: Vec<String> = vec![];
    let mut deleted: Vec<String> = vec![];
    let mut staged: Vec<String> = vec![];
    for line in porcelain.lines() {
        if line.len() < 3 { continue; }
        let xy = &line[..2];
        let path = line[3..].trim().to_string();
        let x = xy.chars().nth(0).unwrap_or(' ');
        let y = xy.chars().nth(1).unwrap_or(' ');
        if xy == "??" { untracked.push(path); continue; }
        if x != ' ' && x != '?' { staged.push(path.clone()); }
        if y == 'M' || x == 'M' { modified.push(path.clone()); }
        if y == 'D' || x == 'D' { deleted.push(path.clone()); }
    }
    let dirty = !porcelain.trim().is_empty();
    ok("git_status", json!({
        "dirty": dirty,
        "modified": modified,
        "untracked": untracked,
        "deleted": deleted,
        "staged": staged,
    }))
}

fn branch_status(_body: &Value) -> u64 {
    let branch = exec_git("rev-parse --abbrev-ref HEAD").trim().to_string();
    if branch.is_empty() {
        return err("branch_status", "unable to determine branch");
    }
    let remote = exec_git(&format!("config --get branch.{}.remote", branch)).trim().to_string();
    let remote = if remote.is_empty() { "origin".to_string() } else { remote };
    let counts = exec_git(&format!("rev-list --left-right --count {}/{}...HEAD", remote, branch));
    let counts = counts.trim();
    let mut behind: u64 = 0;
    let mut ahead: u64 = 0;
    let parts: Vec<&str> = counts.split_whitespace().collect();
    if parts.len() == 2 {
        behind = parts[0].parse().unwrap_or(0);
        ahead = parts[1].parse().unwrap_or(0);
    }
    ok("branch_status", json!({
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "remote": remote,
    }))
}

fn git_push(body: &Value) -> u64 {
    let repo = body.get("repo").and_then(|v| v.as_str()).map(String::from);
    let branch = body.get("branch").and_then(|v| v.as_str()).map(String::from)
        .unwrap_or_else(|| exec_git_in(repo.as_deref(), "rev-parse --abbrev-ref HEAD").trim().to_string());
    if branch.is_empty() {
        return err("git_push", "unable to determine branch");
    }
    let porcelain = git_porcelain_in(repo.as_deref());
    if !porcelain.trim().is_empty() {
        log_deviation_push("push-dirty", &branch);
        let porcelain_preview: String = porcelain.lines().take(8).collect::<Vec<_>>().join("\n");
        let more = if porcelain.lines().count() > 8 { format!("\n... +{} more", porcelain.lines().count() - 8) } else { String::new() };
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": branch,
            "porcelain": porcelain_preview.clone() + &more,
            "reason": format!(
                "worktree dirty in {} — commit or revert before pushing branch {}; an unpushed delta over a dirty tree is an unwitnessed slice. Porcelain:\n{}{}",
                repo.as_deref().unwrap_or("cwd"), branch, porcelain_preview, more
            ),
            "next_dispatch": "instruction",
            "next_action_hint": "Read porcelain field, decide stage-and-commit OR revert, dispatch git_status to confirm clean, then re-dispatch git_push. Do NOT retry git_push with the same dirty tree — the gate will deny again.",
        }).to_string());
    }
    let mut push_out = exec_git_push_in(repo.as_deref(), &branch);
    let mut attempts = 0u32;
    let mut rebased = false;
    while push_rejected(&push_out) && attempts < 3 {
        attempts += 1;
        let rebase_out = exec_git_in(repo.as_deref(), &format!("pull --rebase origin {}", branch));
        if rebase_failed(&rebase_out) || !git_porcelain_in(repo.as_deref()).trim().is_empty() {
            let _ = exec_git_in(repo.as_deref(), "rebase --abort");
            log_deviation_push("push-rebase-conflict", &branch);
            return pack(json!({
                "ok": false,
                "verb": "git_push",
                "gate_denied": true,
                "repo": repo,
                "branch": branch,
                "reason": format!(
                    "push rejected (remote moved); pull --rebase origin {} conflicted and was aborted — worktree could not be cleanly replayed onto origin. Resolve manually. Rebase output:\n{}",
                    branch, rebase_out
                ),
                "next_dispatch": "instruction",
            }).to_string());
        }
        rebased = true;
        push_out = exec_git_push_in(repo.as_deref(), &branch);
    }
    if push_rejected(&push_out) {
        log_deviation_push("push-remote-outpaces", &branch);
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": branch,
            "reason": format!(
                "push to {} rejected after {} rebase-retries — remote is moving faster than the push can land. Re-dispatch git_push after the remote settles. Last output:\n{}",
                branch, attempts, push_out
            ),
            "next_dispatch": "instruction",
        }).to_string());
    }
    ok("git_push", json!({
        "branch": branch,
        "repo": repo,
        "output": push_out,
        "rebased": rebased,
        "rebase_retries": attempts,
    }))
}

fn push_rejected(out: &str) -> bool {
    let l = out.to_lowercase();
    l.contains("rejected") || l.contains("non-fast-forward") || l.contains("fetch first")
        || l.contains("updates were rejected")
}

fn rebase_failed(out: &str) -> bool {
    let l = out.to_lowercase();
    l.contains("conflict") || l.contains("could not apply") || l.contains("error:")
        || l.contains("needs merge") || l.contains("automatic merge failed")
}

fn exec_git_in(repo: Option<&str>, args: &str) -> String {
    let v = git_call(args, repo);
    v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn git_porcelain_in(repo: Option<&str>) -> String {
    exec_git_in(repo, "status --porcelain")
}

fn exec_git_push_in(repo: Option<&str>, branch: &str) -> String {
    let v = git_call(&format!("push origin {}", branch), repo);
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
    format!("{}{}", stdout, stderr)
}

fn filter(body: &Value, raw: &str) -> u64 {
    let (data, err_msg) = crate::filter::dispatch(body, raw);
    match err_msg {
        Some(e) => err("filter", &e),
        None => ok("filter", data),
    }
}

static PANIC_HOOK_INIT: std::sync::Once = std::sync::Once::new();

fn install_panic_hook() {
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

#[no_mangle]
pub extern "C" fn dispatch_verb(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    install_panic_hook();
    let verb = read_str(verb_ptr as *const u8, verb_len);
    let body_s = read_str(body_ptr as *const u8, body_len);
    let body: Value = if body_s.is_empty() { Value::Null } else {
        serde_json::from_str(&body_s).unwrap_or(Value::Null)
    };
    let gate = crate::gates::check_dispatch(&verb, &body);
    if !gate.allowed {
        return pack(gate.to_denial_json(&verb).to_string());
    }
    let cwd_for_witness = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    crate::browser_witness::record_from_body(cwd_for_witness, &body);
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
        "lang" => lang(&body),
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
        "codesearch" => codesearch(&body),
        "memorize" => memorize_with_raw(&body, &body_s),
        "memorize-prune" | "memorize_prune" => memorize_prune(&body),
        "recall" => recall(&body),
        "recall_kv" => recall(&body),
        "memorize_kv" => memorize_with_raw(&body, &body_s),
        "codesearch_kv" => codesearch(&body),
        "recall_libsql" => recall_libsql(&body, &body_s),
        "memorize_libsql" => memorize_libsql(&body, &body_s),
        "codesearch_libsql" => codesearch_libsql(&body, &body_s),
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
        "git_status" => git_status(&body),
        "branch_status" => branch_status(&body),
        "git_push" => git_push(&body),
        "forget" => forget(&body),
        "feedback" => feedback(&body),
        "learn" => learn(&body, &body_s),
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
