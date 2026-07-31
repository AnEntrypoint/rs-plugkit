use serde_json::{json, Value};
use super::host_abi::{
    host_fs_readdir, host_fetch, host_kv_get, host_kv_put, host_kv_delete, host_kv_query,
    host_exec_js, host_now_ms, host_env_get, host_browser_exec,
    pack, read_str, unpack_to_string, unpack_to_value,
    git_call, git_call_argv, host_read, plugin_call as call_plugin,
};
use super::events::{emit_event, log_deviation_push, install_panic_hook};
use crate::orchestrator::yaml_util::base64_decode;

pub fn plugin_ok(resp: &Value) -> bool {
    resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub const PLUGIN_FAIL_UNKNOWN_PLUGIN: &str = "unknown_plugin";
pub const PLUGIN_FAIL_NOT_LOADED: &str = "plugin_not_loaded_yet";
pub const PLUGIN_FAIL_DEADLINE: &str = "deadline_exceeded";
pub const PLUGIN_FAIL_MALFORMED: &str = "malformed_response";
pub const PLUGIN_FAIL_HOST_EMPTY: &str = "host_returned_empty";
pub const PLUGIN_FAIL_PLUGIN_ERROR: &str = "plugin_error";

fn text_names_deadline_exceeded(low: &str) -> bool {
    (low.contains("deadline") && low.contains("exceed"))
        || (low.contains(" exceeded ") && low.contains("executing verb"))
}

fn text_names_unknown_plugin(low: &str) -> bool {
    low.contains("unknown plugin") || low.contains("not registered")
}

pub fn plugin_failure_code(resp: &Value) -> &'static str {
    if resp.is_null() { return PLUGIN_FAIL_HOST_EMPTY; }
    if let Value::String(raw) = resp {
        let low = raw.to_ascii_lowercase();
        if text_names_deadline_exceeded(&low) { return PLUGIN_FAIL_DEADLINE; }
        if text_names_unknown_plugin(&low) { return PLUGIN_FAIL_UNKNOWN_PLUGIN; }
        return PLUGIN_FAIL_MALFORMED;
    }
    let raw_err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let low = raw_err.to_ascii_lowercase();
    if low == PLUGIN_FAIL_UNKNOWN_PLUGIN || text_names_unknown_plugin(&low) {
        return PLUGIN_FAIL_UNKNOWN_PLUGIN;
    }
    if low == PLUGIN_FAIL_NOT_LOADED || low.contains("not loaded") { return PLUGIN_FAIL_NOT_LOADED; }
    if text_names_deadline_exceeded(&low) { return PLUGIN_FAIL_DEADLINE; }
    PLUGIN_FAIL_PLUGIN_ERROR
}

fn plugin_error_message_or_bare_string_fallback(resp: &Value, message_when_shape_unrecognized: &str) -> String {
    match resp {
        Value::String(raw) => raw.clone(),
        Value::Null => "host_plugin_call returned no bytes".to_string(),
        _ => resp.get("error").and_then(|v| v.as_str()).unwrap_or(message_when_shape_unrecognized).to_string(),
    }
}

pub fn plugin_error_detail(resp: &Value, message_when_shape_unrecognized: &str) -> Value {
    let code = plugin_failure_code(resp);
    let message = plugin_error_message_or_bare_string_fallback(resp, message_when_shape_unrecognized);
    let mut detail = json!({
        "error": message,
        "plugin_failure": code,
        "retryable": code == PLUGIN_FAIL_NOT_LOADED || code == PLUGIN_FAIL_DEADLINE,
    });
    if let Some(p) = resp.get("plugin").and_then(|v| v.as_str()) {
        detail["plugin"] = json!(p);
    }
    detail
}

fn plugin_error(resp: &Value, message_when_shape_unrecognized: &str) -> String {
    let code = plugin_failure_code(resp);
    let message = plugin_error_message_or_bare_string_fallback(resp, message_when_shape_unrecognized);
    format!("[{}] {}", code, message)
}

fn err_plugin(verb: &str, resp: &Value, fallback: &str) -> u64 {
    err_json(verb, plugin_error_detail(resp, fallback))
}

pub fn vec_search_local(embedding: &Value, namespace: &str, k: u32) -> Value {
    let (hits, _) = rssearch_vector_hits(embedding, namespace, k, false);
    hits
}

fn embed_query(query: &str) -> Value {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        emit_event("embed_query_failed", json!({ "reason": "empty query after trim" }));
        return Value::Null;
    }
    let resp = call_plugin("bert", "embed", &json!({ "text": query, "kind": "query" }));
    if !plugin_ok(&resp) {
        emit_event("embed_query_failed", json!({
            "reason": "bert plugin call failed",
            "error": plugin_error(&resp, "no error field on bert response"),
            "plugin_failure": plugin_failure_code(&resp),
            "query_len": query.len(),
        }));
        return Value::Null;
    }
    match resp.get("embedding") {
        Some(v) if !v.is_null() => v.clone(),
        _ => {
            emit_event("embed_query_failed", json!({
                "reason": "bert responded ok but carried no embedding field",
                "query_len": query.len(),
            }));
            Value::Null
        }
    }
}

fn embed_passage(text: &str) -> Option<Value> {
    let resp = call_plugin("bert", "embed", &json!({ "text": text, "kind": "passage" }));
    if !plugin_ok(&resp) {
        emit_event("embed_passage_failed", json!({
            "reason": "bert plugin call failed",
            "error": plugin_error(&resp, "no error field on bert response"),
            "plugin_failure": plugin_failure_code(&resp),
            "text_len": text.len(),
        }));
        return None;
    }
    match resp.get("embedding") {
        Some(v) if !v.is_null() => Some(v.clone()),
        _ => {
            emit_event("embed_passage_failed", json!({
                "reason": "bert responded ok but carried no embedding field",
                "plugin_failure": PLUGIN_FAIL_MALFORMED,
                "text_len": text.len(),
            }));
            None
        }
    }
}

pub const BROWSER_DEFAULT_TIMEOUT_MS: u64 = 120_000;

pub const ERR_CODE_FAILED: &str = "failed";
pub const ERR_CODE_RETIRED_VERB: &str = "retired_verb";
pub const ERR_CODE_UNSUPPORTED: &str = "unsupported_by_design";
pub const ERR_CODE_UNKNOWN_VERB: &str = "unknown_verb";
pub const ERR_CODE_INVALID_ARGS: &str = "invalid_args";
pub const ERR_CODE_PANIC: &str = "panic";
pub const ERR_CODE_GATE_DENIED: &str = "gate_denied";

fn next_dispatch_hint_for(verb: &str) -> Value {
    if verb == "instruction" { Value::Null } else { json!("instruction") }
}

fn err(verb: &str, reason: &str) -> u64 {
    err_coded(verb, ERR_CODE_FAILED, reason)
}

fn err_coded(verb: &str, code: &str, reason: &str) -> u64 {
    pack(json!({
        "ok": false,
        "verb": verb,
        "error": reason,
        "error_code": code,
        "next_dispatch_hint": next_dispatch_hint_for(verb),
    }).to_string())
}

fn err_json(verb: &str, detail: Value) -> u64 {
    let mut obj = json!({
        "ok": false,
        "verb": verb,
        "error_code": ERR_CODE_FAILED,
        "next_dispatch_hint": next_dispatch_hint_for(verb),
    });
    if let Some(map) = detail.as_object() {
        for (k, v) in map {
            obj[k] = v.clone();
        }
    }
    pack(obj.to_string())
}

fn ok(verb: &str, data: Value) -> u64 {
    pack(json!({ "ok": true, "verb": verb, "data": data, "next_dispatch_hint": next_dispatch_hint_for(verb) }).to_string())
}

fn path_within_project(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    !normalized.split('/').any(|seg| seg == "..")
        && !normalized.starts_with('/')
        && !normalized.contains(':')
}

fn fs_read(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_read", "path required"); }
    if !path_within_project(path) {
        return err("fs_read", "path must be relative and within the project");
    }
    match host_read(path) {
        Some(s) => ok("fs_read", Value::String(s)),
        None => err("fs_read", "not found or empty"),
    }
}

fn fs_write(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let data = body.get("content").and_then(|v| v.as_str())
        .or_else(|| body.get("data").and_then(|v| v.as_str()))
        .unwrap_or("");
    if path.is_empty() { return err("fs_write", "path required"); }
    if !path_within_project(path) {
        return err("fs_write", "path must be relative and within the project");
    }
    if super::host_abi::host_write(path, data) { ok("fs_write", json!({ "bytes": data.len() })) } else { err("fs_write", "write failed") }
}

fn fs_readdir(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if !path_within_project(path) {
        return err("fs_readdir", "path must be relative and within the project");
    }
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() { return err("fs_readdir", "empty"); }
    ok("fs_readdir", v)
}

fn fs_stat(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() { return err("fs_stat", "path required"); }
    if !path_within_project(path) {
        return err("fs_stat", "path must be relative and within the project");
    }
    match super::host_abi::host_stat(path) {
        Some(v) if !v.is_null() => ok("fs_stat", v),
        _ => err("fs_stat", "not found"),
    }
}

pub const FETCH_DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const FETCH_MAX_TIMEOUT_MS: u64 = 300_000;

fn fetch(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() { return err("fetch", "url required"); }
    if let Err(reason) = crate::config_path::validate_fetch_url(url) {
        return err_coded("fetch", ERR_CODE_INVALID_ARGS, &reason);
    }
    let has_explicit_timeout = body.get("timeoutMs").is_some()
        || body.get("opts").and_then(|o| o.get("timeoutMs")).is_some();
    let timeout_ms = if has_explicit_timeout {
        match crate::validation::validate_timeout_ms(body, true) {
            Ok(n) if n > FETCH_MAX_TIMEOUT_MS => {
                return err_json("fetch", json!({
                    "error": "timeoutMs above ceiling",
                    "max": FETCH_MAX_TIMEOUT_MS,
                    "received": n,
                }));
            }
            Ok(n) => n,
            Err(detail) => return err_json("fetch", detail),
        }
    } else {
        FETCH_DEFAULT_TIMEOUT_MS
    };
    let mut opts_obj = body.get("opts").cloned().unwrap_or_else(|| json!({}));
    if let Some(map) = opts_obj.as_object_mut() {
        map.insert("timeoutMs".to_string(), json!(timeout_ms));
    } else {
        opts_obj = json!({"timeoutMs": timeout_ms});
    }
    let opts = opts_obj.to_string();
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() { return err("fetch", "host_fetch empty"); }
    ok("fetch", v)
}

const ENV_GET_ALLOWED_EXACT: &[&str] = &["CLAUDE_PROJECT_DIR", "GITHUB_TOKEN", "GH_TOKEN"];
const ENV_GET_ALLOWED_PREFIXES: &[&str] = &["PLUGKIT_", "GM_"];

fn env_get_allowed(key: &str) -> bool {
    ENV_GET_ALLOWED_EXACT.contains(&key)
        || ENV_GET_ALLOWED_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn env_get(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("env_get", "key required"); }
    if !env_get_allowed(key) {
        return err("env_get", "key not on env_get allowlist");
    }
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
  let timer = null;
  try {{
    const out = await Promise.race([
      Promise.resolve(plugin.exec.run(code, projectDir)),
      new Promise((_, rej) => {{ timer = setTimeout(() => rej(new Error('plugin-timeout')), {inner_timeout}); }})
    ]);
    process.stdout.write(JSON.stringify({{ok:true, plugin_id: plugin.id, output: String(out), ms: Date.now() - t0}}));
  }} catch (e) {{
    process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e), plugin_id: plugin.id, ms: Date.now() - t0}}));
  }} finally {{
    if (timer) clearTimeout(timer);
  }}
}})().catch(e => {{ process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e)}})); }})"#,
        project_dir = serde_json::to_string(project_dir).unwrap_or_else(|_| "\"\"".to_string()),
        command = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".to_string()),
        code = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".to_string()),
        inner_timeout = timeout_ms.saturating_sub(2000).max(1000),
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

const KV_PUT_ALLOWED_NAMESPACES: &[&str] = &["default", "session", "config", "cache", "user"];

fn kv_put(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let val = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() { return err("kv_put", "key required"); }
    if !KV_PUT_ALLOWED_NAMESPACES.contains(&ns) {
        return err("kv_put", "namespace not permitted; allowed: default, session, config, cache, user");
    }
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

fn discipline_fanout_namespaces(base: &str) -> Vec<String> {
    let mut out = vec![base.to_string()];
    if let Some(content) = host_read(".gm/disciplines/enabled.txt") {
        for line in content.lines() {
            let name = line.trim();
            if !name.is_empty() && !name.starts_with('#') && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub const EMBED_UNAVAILABLE: &str = "query embedding unavailable -- the bert embedder failed, so no vector search was attempted";

pub fn query_embedding_unusable(query_embedding: &Value) -> bool {
    match query_embedding {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        _ => true,
    }
}

fn rssearch_vector_hits(query_embedding: &Value, namespace: &str, limit: u32, do_sync: bool) -> (Value, Option<Vec<String>>) {
    if query_embedding_unusable(query_embedding) {
        emit_event("rssearch_vector_hits_skipped", json!({
            "namespace": namespace,
            "reason": EMBED_UNAVAILABLE,
        }));
        return (json!({ "error": EMBED_UNAVAILABLE }), None);
    }
    let namespaces = discipline_fanout_namespaces(namespace);
    let now_ms = unsafe { host_now_ms() } as i64;
    let cfg = crate::ragconfig::RagConfig::resolved();
    let mut memory_namespaces: Vec<String> = Vec::new();
    for ns in &namespaces {
        if cfg.namespaces.is_code(ns) {
            if let Err(e) = crate::rssearch_vectors::migrate_namespace_from_flat_json_cfg(ns, now_ms, &cfg) {
                emit_event("rssearch_vectors_migration_failed", json!({ "namespace": ns, "error": e }));
            }
        } else {
            if do_sync {
                let _ = crate::memory_md::export_flat_json(ns, now_ms);
            }
            memory_namespaces.push(ns.clone());
        }
    }
    let converged = if memory_namespaces.is_empty() {
        false
    } else if do_sync {
        let sync = crate::memory_md::sync_index(&memory_namespaces, now_ms);
        sync.get("converged").and_then(|v| v.as_bool()).unwrap_or(false)
    } else {
        crate::memory_md::has_stored_digest(&memory_namespaces)
    };
    let hits = match crate::rssearch_vectors::search_with_recency_cfg(query_embedding, &namespaces, limit as usize, now_ms, &cfg) {
        Ok(hits) => hits,
        Err(e) => {
            emit_event("rssearch_vector_hits_failed", json!({
                "namespace": namespace, "error": e,
                "reason": "search_with_recency failed even after malformed-db recovery attempt",
            }));
            json!({ "error": e })
        }
    };
    (hits, if converged { Some(memory_namespaces) } else { None })
}

pub fn memory_recall_backend(query_embedding: &Value, namespace: &str, limit: u32) -> Option<Value> {
    if query_embedding_unusable(query_embedding) {
        return None;
    }
    let (_, mem_ns) = rssearch_vector_hits(query_embedding, namespace, limit, true);
    let mem_ns = mem_ns?;
    let now_ms = unsafe { host_now_ms() } as i64;
    let cfg = crate::ragconfig::RagConfig::resolved();
    crate::rssearch_vectors::search_memory_hits_cfg(query_embedding, &mem_ns, limit as usize, now_ms, &cfg)
        .ok()
        .filter(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false))
}

fn recall(body: &Value) -> u64 {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(cfg.budget.default_limit as u64) as u32;
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or(&cfg.namespaces.default);
    if query.is_empty() { return err("recall", "query required"); }
    check_sigil_ignored(query, namespace);
    let derived_query = query.to_string();
    let embedding = embed_query(query);
    let (vector_hits, mem_ns) = rssearch_vector_hits(&embedding, namespace, limit, false);
    if let Some(mem_ns) = &mem_ns {
        let now_ms = unsafe { host_now_ms() } as i64;
        if let Ok(md_hits) = crate::rssearch_vectors::search_memory_hits_cfg(&embedding, mem_ns, limit as usize, now_ms, &cfg) {
            if md_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                return ok("recall", json!({
                    "mode": "vector_top_k",
                    "namespace": namespace,
                    "derived_query": derived_query,
                    "hits": md_hits,
                    "vector_hits": vector_hits,
                }));
            }
        }
    }
    let vec_hits = vec_search_local(&embedding, namespace, limit);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        let annotated = annotate_hits_with_score(vec_hits);
        return ok("recall", json!({
            "mode": "vector_top_k",
            "namespace": namespace,
            "derived_query": derived_query,
            "hits": annotated,
            "vector_hits": vector_hits,
        }));
    }
    let packed = unsafe { host_kv_query(namespace.as_ptr(), namespace.len() as u32, query.as_ptr(), query.len() as u32) };
    let kv_hits = unpack_to_value(packed);
    let annotated = annotate_hits_with_score(kv_hits);

    let embed_failed = query_embedding_unusable(&embedding);
    let vec_err = vector_hits
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    let degraded = embed_failed || vec_err.is_some();
    let empty = annotated.as_array().map(|a| a.is_empty()).unwrap_or(true);

    if degraded {
        emit_event("recall_degraded", json!({
            "namespace": namespace,
            "embed_failed": embed_failed,
            "vec_err": vec_err,
            "kv_hits": annotated.as_array().map(|a| a.len()).unwrap_or(0),
        }));
    }

    if degraded && empty {
        return err(
            "recall",
            &format!(
                "semantic retrieval unavailable (embedder failed{}) and the keyword fallback matched nothing -- this is NOT an empty-knowledgebase result",
                vec_err.map(|e| format!(": {}", e)).unwrap_or_default()
            ),
        );
    }

    ok("recall", json!({
        "mode": "fallback_like",
        "degraded": degraded,
        "namespace": namespace,
        "derived_query": derived_query,
        "hits": annotated,
        "vector_hits": vector_hits,
    }))
}

const DIAGNOSTIC_EVENT_COOLDOWN_MS: i64 = 5 * 60 * 1000;
static RECALL_SCORE_UNAVAILABLE_LAST_FIRED_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static SIGIL_IGNORED_LAST_FIRED_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn diagnostic_event_should_fire(last_fired: &std::sync::atomic::AtomicI64) -> bool {
    let now = unsafe { host_now_ms() } as i64;
    let prev = last_fired.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(prev) < DIAGNOSTIC_EVENT_COOLDOWN_MS {
        return false;
    }
    last_fired.compare_exchange(prev, now, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed).is_ok()
}

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
    if any_missing && diagnostic_event_should_fire(&RECALL_SCORE_UNAVAILABLE_LAST_FIRED_MS) {
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
        if diagnostic_event_should_fire(&SIGIL_IGNORED_LAST_FIRED_MS) {
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
    let content_hash = crate::pipeline::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
    let key = format!("mem-{:016x}-{}", content_hash, text.len());
    let flat_dedup = super::host_abi::host_kv_read(namespace, &key)
        .map(|existing| existing == text)
        .unwrap_or(false);
    if flat_dedup || crate::memory_md::memory_text_matches(namespace, &key, text) {
        let md_path = memory_md_write_path(namespace, &key, text);
        return ok("memorize", json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": true, "deduped": true, "md_file": md_path}));
    }
    let emb = match embed_passage(text) {
        Some(e) => e,
        None => return err("memorize", "embed failed; refusing to write a text-only memory with no vector (un-vector-recallable orphan)"),
    };
    let md_path = memory_md_write_path(namespace, &key, text);
    if md_path.is_none() {
        return err("memorize", "memory md write failed; the md corpus is the durable store, refusing an unbacked memory");
    }
    let now_ms = unsafe { host_now_ms() } as i64;
    if let Err(e) = crate::rssearch_vectors::write(namespace, &key, text, &emb, now_ms) {
        emit_event("rssearch_vectors_write_failed", json!({
            "key": key,
            "namespace": namespace,
            "error": e,
        }));
    }
    ok("memorize", json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": true, "md_file": md_path}))
}

fn memory_md_write_path(namespace: &str, key: &str, text: &str) -> Option<String> {
    let now_ms = unsafe { host_now_ms() } as i64;
    match crate::memory_md::write_memory(namespace, key, text, now_ms) {
        crate::memory_md::WriteOutcome::Created(p)
        | crate::memory_md::WriteOutcome::Updated(p)
        | crate::memory_md::WriteOutcome::Deduped(p) => Some(p),
        crate::memory_md::WriteOutcome::Invalid(reason) => {
            emit_event("memory_md_write_invalid", json!({
                "key": key, "namespace": namespace, "reason": reason,
            }));
            None
        }
        crate::memory_md::WriteOutcome::Failed(_) => None,
    }
}

fn memorize_prune(body: &Value) -> u64 {
    let namespace = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    let mut keys: Vec<String> = Vec::new();
    if let Some(k) = body.get("key").and_then(|v| v.as_str()) {
        if !k.is_empty() { keys.push(k.to_string()); }
    }
    if let Some(arr) = body.get("keys").and_then(|v| v.as_array()) {
        for v in arr { if let Some(s) = v.as_str() { keys.push(s.to_string()); } }
    }
    if !keys.is_empty() {
        let vec_ns = format!("{}-vec", namespace);
        let mut deleted = Vec::new();
        let mut not_found = Vec::new();
        for key in &keys {
            let flat_rc = unsafe { host_kv_delete(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
            let _ = unsafe { host_kv_delete(vec_ns.as_ptr(), vec_ns.len() as u32, key.as_ptr(), key.len() as u32) };
            let md_deleted = crate::memory_md::delete_memory(namespace, key);
            let idx_marked = crate::rssearch_vectors::mark_deleted(namespace, key).is_ok();
            if flat_rc != 0 || md_deleted {
                deleted.push(key.clone());
                emit_event("memory.pruned", json!({"key": key, "namespace": namespace, "mode": "explicit-key", "md_deleted": md_deleted, "index_marked": idx_marked}));
            } else {
                not_found.push(key.clone());
                emit_event("memory.prune-miss", json!({"key": key, "namespace": namespace, "mode": "explicit-key"}));
            }
        }
        let mut resp = json!({"namespace": namespace, "deleted": deleted, "mode": "explicit-key"});
        if !not_found.is_empty() {
            resp["not_found"] = json!(not_found);
            resp["note"] = json!("Keys in not_found did not exist in this namespace -- nothing was pruned for them. The key is likely under a different namespace (pass {namespace:<the recall hit's namespace>}) or the key string did not match exactly. Re-run memorize-prune {query} to get live candidates with their exact keys + namespaces.");
        }
        return ok("memorize-prune", resp);
    }
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return err("memorize-prune", "provide `key`/`keys` to delete, or `query` to list prune candidates");
    }
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let embedding = embed_query(query);
    if query_embedding_unusable(&embedding) {
        emit_event("memorize_prune_degraded", json!({
            "namespace": namespace,
            "reason": EMBED_UNAVAILABLE,
        }));
        return err(
            "memorize-prune",
            "semantic retrieval unavailable (the embedder failed) so no prune candidates could be ranked -- this is NOT an empty-namespace result, and deleting on the strength of it would prune memories that were never searched",
        );
    }
    let (vector_candidates, _) = rssearch_vector_hits(&embedding, namespace, k, true);
    let candidates = vec_search_local(&embedding, namespace, k);
    ok("memorize-prune", json!({
        "namespace": namespace,
        "mode": "review",
        "candidates": candidates,
        "vector_candidates": vector_candidates,
        "note": "Review-only: re-dispatch memorize-prune with {keys:[...]} naming the stale ones to delete. Pruning is agent-judged, never auto-similarity-deleted. candidates falls back to the libsql rssearch_vectors result when host_vec_search is unimplemented (both native runtimes stub it).",
    }))
}

fn codesearch(body: &Value) -> u64 {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(cfg.budget.default_k as u64) as u32;
    if query.is_empty() { return err("codesearch", "query required"); }
    if body.get("rebuild").and_then(|v| v.as_bool()).unwrap_or(false)
        && !body.get("auto_indexed").and_then(|v| v.as_bool()).unwrap_or(false) {
        let cleared = crate::code_index::clear_codeinsight_full_cfg(&cfg);
        emit_event("codeinsight_rebuild", json!({ "reason": "explicit-rebuild", "keys_cleared": cleared }));
        let _ = crate::code_index::index(".", 500);
        let mut retry = body.clone();
        if let Some(obj) = retry.as_object_mut() {
            obj.insert("auto_indexed".to_string(), Value::Bool(true));
            obj.insert("rebuild".to_string(), Value::Bool(false));
        }
        return codesearch(&retry);
    }
    let already_indexed = body.get("auto_indexed").and_then(|v| v.as_bool()).unwrap_or(false);
    if !already_indexed {
        let stored = crate::code_index::stored_digest();
        let current = crate::code_index::current_digest();
        let stale = match &stored { Some(s) => s != &current, None => true };
        if stale {
            let reason = if stored.is_none() { "digest-absent" } else { "digest-mismatch" };
            emit_event("codeinsight_rebuild", json!({ "reason": reason, "stored_then_current": current }));
            let _ = crate::code_index::index(".", 500);
            let mut retry = body.clone();
            if let Some(obj) = retry.as_object_mut() {
                obj.insert("auto_indexed".to_string(), Value::Bool(true));
            }
            return codesearch(&retry);
        }
    }
    let cand_k = cfg.budget.pool(k as usize).max(50) as u32;
    let embedding = embed_query(query);
    let code_ns = cfg.namespaces.code.as_str();
    let (vector_hits, _) = rssearch_vector_hits(&embedding, code_ns, k, false);
    let vec_hits = vec_search_local(&embedding, code_ns, cand_k);
    let vec_ids: Vec<String> = vec_hits.as_array().map(|a| {
        a.iter().filter_map(|h| h.get("key").and_then(|x| x.as_str()).map(String::from)).collect()
    }).unwrap_or_default();
    let mut corpus = crate::code_index::FusionCorpus::load();
    let vec_ids: Vec<String> = if vec_ids.is_empty() {
        let vres = crate::code_index::search(query, cand_k as usize, Some(&embedding));
        vres.get("rows")
            .and_then(|r| r.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        let path = r.get("path").and_then(|v| v.as_str())?;
                        let ls = r.get("line_start").and_then(|v| v.as_u64())? as usize;
                        corpus.key_for_path_line(path, ls)
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec_ids
    };
    let bm25_ids = corpus.bm25_rank_cfg(query, cand_k as usize, &cfg.scoring);
    let commits = crate::code_index::git_commit_rank(query, 10);
    let build_hit = |corpus: &mut crate::code_index::FusionCorpus, key: &str, score: Option<f64>, fallback_text: Option<&str>| -> Value {
        let text = corpus.text_for_key(key)
            .or_else(|| fallback_text.map(String::from))
            .unwrap_or_default();
        let mut obj = serde_json::Map::new();
        obj.insert("key".to_string(), json!(key));
        obj.insert("text".to_string(), json!(text));
        if let Some(s) = score { obj.insert("score".to_string(), json!(s)); }
        if let Some(sym) = corpus.symbol_for_key(key) { obj.insert("symbol".to_string(), sym); }
        if let Some(ov) = corpus.overview_for_key(key) { obj.insert("overview".to_string(), json!(ov)); }
        Value::Object(obj)
    };
    let vector_top10: Vec<Value> = vector_hits.as_array()
        .filter(|a| !a.is_empty())
        .map(|a| a.iter().take(10).cloned().collect())
        .or_else(|| vec_hits.as_array().filter(|a| !a.is_empty()).map(|a| a.iter().take(10).cloned().collect()))
        .unwrap_or_else(|| vec_ids.iter().take(10).map(|key| build_hit(&mut corpus, key, None, None)).collect());
    let bm25_top10: Vec<Value> = bm25_ids.iter().take(10).map(|key| build_hit(&mut corpus, key, None, None)).collect();
    if !vec_ids.is_empty() || !bm25_ids.is_empty() {
        let lists = vec![vec_ids, bm25_ids];
        let weights = [1.0, cfg.scoring.fusion_identifier_boost];
        let fused = rs_search::fusion::fuse_n_cfg(&lists, &weights, query, cfg.scoring.fusion_rrf_k);
        let hits: Vec<Value> = fused.into_iter().take(k as usize).map(|(key, score)| {
            let fallback = vec_hits.as_array().and_then(|a| {
                a.iter().find(|h| h.get("key").and_then(|x| x.as_str()) == Some(key.as_str()))
                    .and_then(|h| h.get("text").and_then(|t| t.as_str()))
            });
            build_hit(&mut corpus, &key, Some(score), fallback)
        }).collect();
        let vector_hits_response = vector_top10.clone();
        return ok("codesearch", json!({
            "mode": "fusion", "hits": hits, "commits": commits, "vector_hits": vector_hits_response,
            "vector_top10": vector_top10, "bm25_top10": bm25_top10,
        }));
    }
    let ns = cfg.namespaces.code.as_str();
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, query.as_ptr(), query.len() as u32) };
    let hits = unpack_to_value(packed);
    let kv_empty = hits.is_null() || hits.as_array().map(|a| a.is_empty()).unwrap_or(true);
    if kv_empty && !body.get("auto_indexed").and_then(|v| v.as_bool()).unwrap_or(false) {
        let _ = crate::code_index::index(".", 500);
        let mut retry = body.clone();
        if let Some(obj) = retry.as_object_mut() {
            obj.insert("auto_indexed".to_string(), Value::Bool(true));
        }
        return codesearch(&retry);
    }
    let vec_unavailable = vector_top10.is_empty();
    let kv_empty_now = hits.is_null() || hits.as_array().map(|a| a.is_empty()).unwrap_or(true);

    if vec_unavailable {
        emit_event("codesearch_degraded", json!({
            "namespace": cfg.namespaces.code,
            "kv_hits": hits.as_array().map(|a| a.len()).unwrap_or(0),
        }));
    }

    if vec_unavailable && kv_empty_now {
        return err(
            "codesearch",
            "semantic retrieval unavailable (no vector hits and the embedder produced nothing) and the keyword fallback matched nothing -- this is NOT an empty-index result",
        );
    }

    ok("codesearch", json!({
        "mode": "fallback_kv", "degraded": vec_unavailable,
        "hits": hits, "commits": commits, "vector_hits": vector_top10.clone(),
        "vector_top10": vector_top10, "bm25_top10": bm25_top10,
    }))
}

const LIFECYCLE_STALE_WARN_MS: u64 = 30 * 60 * 1000;

fn lifecycle_liveness() -> Value {
    let log = match crate::wasm_dispatch::host_read(".gm/exec-spool/.watcher.log") {
        Some(l) => l,
        None => return json!({ "last_event_age_ms": null, "note": "no .watcher.log yet -- expected on a brand-new project with no dispatches fired" }),
    };
    let last_evt_line = log.lines().rev().find_map(|l| l.strip_prefix("evt: "));
    let Some(line) = last_evt_line else {
        return json!({ "last_event_age_ms": null, "note": "watcher.log exists but contains no evt: lines -- the lifecycle-event pipe may be dead (see the 2026-07-22 gm-log outage incident this check targets)" });
    };
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return json!({ "last_event_age_ms": null, "note": "last evt: line in watcher.log failed to parse as JSON" });
    };
    let Some(ts) = v.get("ts").and_then(|t| t.as_u64()) else {
        return json!({ "last_event_age_ms": null, "note": "last evt: line has no ts field" });
    };
    let now = unsafe { host_now_ms() };
    let age_ms = now.saturating_sub(ts);
    json!({
        "last_event_age_ms": age_ms,
        "last_event": v.get("event").cloned().unwrap_or(Value::Null),
        "stale": age_ms > LIFECYCLE_STALE_WARN_MS,
    })
}

fn health(_body: &Value) -> u64 {
    let now = unsafe { host_now_ms() };
    let subsystems: Vec<Value> = crate::mediator::all_verbs_by_subsystem()
        .into_iter()
        .map(|(sub, verbs)| json!({ "subsystem": sub.as_str(), "verbs": verbs }))
        .collect();
    let aliases: Vec<Value> = crate::mediator::alias_table()
        .into_iter()
        .map(|(alias, canonical, lang_preserved)| json!({
            "alias": alias,
            "canonical": canonical,
            "lang_preserved": lang_preserved,
        }))
        .collect();
    fn project_gm_json_pinned_version() -> Value {
        match crate::wasm_dispatch::host_read("gm.json")
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get("plugkitVersion").and_then(|p| p.as_str().map(String::from)))
        {
            Some(tag) => Value::String(tag),
            None => Value::Null,
        }
    }

    ok("health", json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "crate_version": env!("CARGO_PKG_VERSION"),
        "source_sha": env!("PLUGKIT_SOURCE_SHA"),
        "loaded_module_is_compiled_version_above_not_project_pin": true,
        "project_gm_json_pinned_version": project_gm_json_pinned_version(),
        "now": now,
        "imports": super::host_abi::HOST_IMPORTS,
        "subsystems": subsystems,
        "verb_aliases": aliases,
        "error_codes": [
            ERR_CODE_FAILED, ERR_CODE_RETIRED_VERB, ERR_CODE_UNSUPPORTED,
            ERR_CODE_UNKNOWN_VERB, ERR_CODE_INVALID_ARGS, ERR_CODE_PANIC, ERR_CODE_GATE_DENIED,
        ],
        "plugin_failure_codes": [
            PLUGIN_FAIL_UNKNOWN_PLUGIN, PLUGIN_FAIL_NOT_LOADED, PLUGIN_FAIL_DEADLINE,
            PLUGIN_FAIL_MALFORMED, PLUGIN_FAIL_HOST_EMPTY, PLUGIN_FAIL_PLUGIN_ERROR,
        ],
        "lifecycle_liveness": lifecycle_liveness()
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

fn close(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 { return err("close", "taskId required"); }
    let key = format!("{}", task_id);
    let rc = unsafe { host_kv_put("outbox".as_ptr(), 6, key.as_ptr(), key.len() as u32, "closed".as_ptr(), 6) };
    if rc != 0 { ok("close", json!({ "taskId": task_id })) } else { err("close", "close failed") }
}

fn forget(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("default");
    if key.is_empty() { return err("forget", "key required"); }
    let rc = unsafe { host_kv_delete(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
    if rc == 0 { ok("forget", json!({ "namespace": ns, "key": key })) } else { err("forget", "delete failed") }
}

const ROUTER_MODELS: &[&str] = &["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-7"];

const ROUTE_BUCKET_CAPS: &[u64] = &[1000, 4000, 16000, 64000];

fn bucket_for_tokens(n: u64) -> u8 {
    for (i, &cap) in ROUTE_BUCKET_CAPS.iter().enumerate() {
        if n <= cap {
            return i as u8;
        }
    }
    4
}

pub fn route_hint(prompt: &str, estimated_tokens: u64) -> Value {
    if prompt.trim().is_empty() { return Value::Null; }
    serde_json::json!({
        "model": ROUTER_MODELS[0],
        "context_bucket": bucket_for_tokens(estimated_tokens),
        "temperature": 0.7f32,
        "top_p": 0.9f32,
        "confidence": 0.5f32,
        "algo": "rule",
        "exploration": false,
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

fn browser_page_health_or_none_for_session_management_body(v: &Value) -> Option<bool> {
    let debug = v.get("debug")?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let page_errors_empty = debug.get("pageErrors").and_then(|a| a.as_array()).map(|a| a.is_empty()).unwrap_or(true);
    let console_clean = debug.get("console").and_then(|a| a.as_array())
        .map(|entries| !entries.iter().any(|e| {
            e.get("level").and_then(|l| l.as_str()).map(|l| l.eq_ignore_ascii_case("error")).unwrap_or(false)
        }))
        .unwrap_or(true);
    Some(ok && page_errors_empty && console_clean)
}

fn record_app_loads_witness_from_response(cwd: &str, v: &Value) {
    let Some(healthy) = browser_page_health_or_none_for_session_management_body(v) else { return };
    let detail = if healthy {
        "browser dispatch reached the page with ok:true, zero pageErrors, zero console error-level lines".to_string()
    } else {
        let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
        let page_err_count = v.get("debug").and_then(|d| d.get("pageErrors")).and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0);
        format!("browser dispatch unhealthy: ok={ok}, pageErrors={page_err_count}")
    };
    crate::browser_witness::record_app_loads_witness_unconditional_on_edits(cwd, healthy, &detail);
}

fn browser(body: &Value, body_s: &str) -> u64 {
    let code = body.get("code").and_then(|v| v.as_str()).map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body_s.to_string());
    if code.is_empty() { return err("browser", "code required (provide JS body or {code, cwd?, sessionId?} JSON)"); }
    let cwd = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let explicit_sid = body.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let session_id = if !explicit_sid.is_empty() {
        explicit_sid
    } else if let Some(dispatch_sid) = super::events::current_dispatch_session_id().filter(|s| !s.trim().is_empty()) {
        dispatch_sid
    } else {
        host_read(".gm/exec-spool/.session-current").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_default()
    };
    let timeout_ms = match body.get("timeoutMs") {
        None | Some(Value::Null) => BROWSER_DEFAULT_TIMEOUT_MS,
        Some(raw) => match raw.as_u64() {
            Some(n) if n >= crate::validation::MIN_TIMEOUT_MS => n,
            Some(n) => return err_json("browser", json!({
                "error": "timeoutMs below floor",
                "error_code": ERR_CODE_INVALID_ARGS,
                "min": crate::validation::MIN_TIMEOUT_MS,
                "received": n,
            })),
            None => return err_json("browser", json!({
                "error": "timeoutMs must be a positive integer number of milliseconds -- a string, float or negative value is rejected rather than silently falling back to the default budget",
                "error_code": ERR_CODE_INVALID_ARGS,
                "min": crate::validation::MIN_TIMEOUT_MS,
                "received": raw.clone(),
            })),
        },
    };
    let envelope = json!({ "body": code, "timeoutMs": timeout_ms }).to_string();
    let packed = unsafe { host_browser_exec(
        envelope.as_ptr(), envelope.len() as u32,
        cwd.as_ptr(), cwd.len() as u32,
        session_id.as_ptr(), session_id.len() as u32,
    ) };
    match unpack_to_string(packed) {
        Some(s) => {
            let v: Value = serde_json::from_str(&s).unwrap_or(Value::String(s));
            let transport_ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false)
                && !v.get("timed_out").and_then(|b| b.as_bool()).unwrap_or(false)
                && v.get("exit_code").and_then(|n| n.as_i64()).map(|c| c == 0).unwrap_or(true);
            let mut v = v;
            if transport_ok {
                let witnessed = crate::browser_witness::witness_all_pending_edits_by_rehashing_current_content(&cwd);
                if witnessed > 0 {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("witness_marked".to_string(), json!(witnessed));
                    }
                }
            } else if let Some(obj) = v.as_object_mut() {
                obj.insert("witness_skipped_transport_failure".to_string(), json!(true));
            }
            record_app_loads_witness_from_response(cwd, &v);
            ok("browser", v)
        }
        None => err_json("browser", json!({
            "error": "host_browser_exec returned empty -- the browser host produced no bytes at all, which is the daemon-flake symptom, NOT a script that returned undefined",
            "error_code": ERR_CODE_FAILED,
            "timeout_ms": timeout_ms,
            "session_id": session_id,
            "retryable": true,
        })),
    }
}

fn db_display_label_not_identity(body: &Value) -> String {
    body.get("db_name").or_else(|| body.get("db")).and_then(|v| v.as_str()).unwrap_or("main").to_string()
}

fn db_identity_path(body: &Value) -> String {
    match body.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => crate::code_index::project_db_path(None),
    }
}

fn sql_open(body: &Value) -> u64 {
    let path = db_identity_path(body);
    let name = db_display_label_not_identity(body);
    let resp = call_plugin("libsql", "open", &json!({ "db": name, "path": path }));
    if plugin_ok(&resp) {
        ok("sql_open", json!({ "path": path, "db_name": name }))
    } else {
        err_plugin("sql_open", &resp, "open failed")
    }
}

fn sql_close(body: &Value) -> u64 {
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "close", &json!({ "db": name, "path": path }));
    if plugin_ok(&resp) {
        ok("sql_close", json!({ "db_name": name }))
    } else {
        err_plugin("sql_close", &resp, "close failed")
    }
}

fn sql_list_dbs(_body: &Value) -> u64 {
    let resp = call_plugin("libsql", "list_dbs", &json!({}));
    let names = resp.get("dbs").cloned().unwrap_or_else(|| json!([]));
    ok("sql_list_dbs", json!({ "dbs": names }))
}

fn sql_exec(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_exec", "missing sql"),
    };
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "exec", &json!({ "db": name, "path": path, "sql": sql }));
    if plugin_ok(&resp) {
        ok("sql_exec", json!({}))
    } else {
        err_plugin("sql_exec", &resp, "exec failed")
    }
}

fn sql_query(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_query", "missing sql"),
    };
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "query", &json!({ "db": name, "path": path, "sql": sql }));
    if plugin_ok(&resp) {
        let rows = resp.get("rows").cloned().unwrap_or_else(|| json!([]));
        ok("sql_query", json!({ "rows": rows }))
    } else {
        err_plugin("sql_query", &resp, "query failed")
    }
}

fn apply_cache_budget_overrides_from(cfg: &mut crate::cache::CacheConfig, source: &Value) {
    if let Some(n) = source.get("max_entries_per_namespace").and_then(|v| v.as_u64()) {
        cfg.max_entries_per_namespace = n as usize;
    }
    if let Some(n) = source.get("max_bytes_per_namespace").and_then(|v| v.as_u64()) {
        cfg.max_bytes_per_namespace = n as usize;
    }
    if let Some(n) = source.get("max_value_bytes").and_then(|v| v.as_u64()) {
        cfg.max_value_bytes = n as usize;
    }
    if let Some(n) = source.get("default_ttl_ms").and_then(|v| v.as_i64()) {
        cfg.default_ttl_ms = Some(n);
    }
}

fn cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body: &Value) -> crate::cache::CacheConfig {
    let mut cfg = crate::cache::DEFAULTS;
    let resolved = crate::config::resolve().config.value;
    if let Some(vendored_cache_section) = resolved.get("cache") {
        apply_cache_budget_overrides_from(&mut cfg, vendored_cache_section);
    }
    apply_cache_budget_overrides_from(&mut cfg, body);
    cfg
}

fn cache_err_with_machine_readable_kind(verb: &str, e: crate::cache::CacheError) -> u64 {
    err_json(verb, json!({ "error": e.message(), "error_kind": e.kind() }))
}

fn cache_get(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::get(&cfg, ns, key) {
        Ok(Some(entry)) => ok("cache_get", json!({ "hit": true, "entry": entry.to_json() })),
        Ok(None) => ok("cache_get", json!({ "hit": false, "namespace": ns, "key": key })),
        Err(e) => cache_err_with_machine_readable_kind("cache_get", e),
    }
}

fn cache_put(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = match body.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => return err("cache_put", "value required"),
    };
    let ttl = body.get("ttl_ms").and_then(|v| v.as_i64());
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::put(&cfg, ns, key, &value, ttl) {
        Ok(hash) => ok("cache_put", json!({ "content_hash": hash, "bytes": value.len() })),
        Err(e) => cache_err_with_machine_readable_kind("cache_put", e),
    }
}

fn cache_invalidate(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match body.get("key").and_then(|v| v.as_str()).filter(|k| !k.is_empty()) {
        Some(key) => match crate::cache::invalidate(&cfg, ns, key) {
            Ok(existed) => ok("cache_invalidate", json!({ "removed": existed })),
            Err(e) => cache_err_with_machine_readable_kind("cache_invalidate", e),
        },
        None => match crate::cache::invalidate_namespace(&cfg, ns) {
            Ok(n) => ok("cache_invalidate", json!({ "removed": n, "namespace": ns })),
            Err(e) => cache_err_with_machine_readable_kind("cache_invalidate", e),
        },
    }
}

fn config_resolve_report_winning_tier_and_any_rejected_tier(_body: &Value) -> u64 {
    let r = crate::config::resolve();
    let mut payload = r.to_json();

    if let Some(obj) = payload.as_object_mut() {
        obj.insert("unknown_keys".to_string(), json!(r.config.unknown_top_level_keys()));
        match crate::ragconfig::RagConfig::from_value(&r.config.value) {
            Ok(rag) => {
                obj.insert("rag_effective".to_string(), json!({
                    "embed_dim": rag.embed.dim,
                    "default_namespace": rag.namespaces.default,
                    "recall_default_limit": rag.budget.default_limit,
                    "recall_pool_multiplier": rag.budget.pool_multiplier,
                    "recall_pool_floor": rag.budget.pool_floor,
                    "recall_default_k": rag.budget.default_k,
                    "index_wall_budget_ms": rag.index.wall_budget_ms,
                    "index_max_file_bytes": rag.index.max_file_bytes,
                    "index_max_chunks_per_file_per_pass": rag.index.max_chunks_embedded_per_file_per_pass_count_bound_only,
                    "index_pessimistic_ms_per_chunk": rag.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound,
                    "index_extra_skip_dirs": rag.index.extra_skip_dirs_appended_to_builtins_never_replacing,
                    "index_extra_skip_file_suffixes": rag.index.extra_skip_file_suffixes_appended_to_builtins_never_replacing,
                    "index_force_include_path_substrings": rag.index.force_include_path_substrings_overriding_every_skip,
                    "scoring_bm25_k1": rag.scoring.bm25_k1_term_frequency_saturation,
                    "scoring_bm25_b": rag.scoring.bm25_b_document_length_normalization,
                    "scoring_recency_floor": rag.scoring.recency_floor,
                    "scoring_cos_floor": rag.scoring.cos_floor_applied_before_recency_rescue,
                    "scoring_dedup_jaccard_threshold": rag.scoring.dedup_jaccard_near_duplicate_threshold,
                    "scoring_half_life_ms": rag.scoring.half_life_ms,
                    "scoring_fusion_rrf_k": rag.scoring.fusion_rrf_k,
                    "scoring_fusion_identifier_boost": rag.scoring.fusion_identifier_boost,
                    "pipeline_ttl_ms": rag.pipeline.ttl_ms,
                    "pipeline_summarize_threshold": rag.pipeline.summarize_threshold,
                    "pipeline_max_result_bytes": rag.pipeline.max_result_bytes_advertised_and_enforced_by_one_field,
                    "pipeline_max_attempts": rag.pipeline.max_attempts,
                }));
            }
            Err(reason) => {
                obj.insert("rag_effective".to_string(), Value::Null);
                obj.insert("rag_rejected".to_string(), Value::String(reason));
            }
        }
    }
    ok("config_resolve", payload)
}

fn cache_stats(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::stats(&cfg, ns) {
        Ok((entries, bytes)) => ok("cache_stats", json!({
            "namespace": ns,
            "entries": entries,
            "bytes": bytes,
            "max_entries_per_namespace": cfg.max_entries_per_namespace,
            "max_bytes_per_namespace": cfg.max_bytes_per_namespace,
            "max_value_bytes": cfg.max_value_bytes,
            "default_ttl_ms": cfg.default_ttl_ms,
        })),
        Err(e) => cache_err_with_machine_readable_kind("cache_stats", e),
    }
}

fn sql_smoke() -> u64 {
    let owned_path = crate::libsql_wasm::absolute_db_path(".sql-smoke.db");
    let path = owned_path.as_str();
    let mut log: Vec<Value> = Vec::new();
    let _ = call_plugin("libsql", "exec", &json!({ "path": path, "sql": "DROP TABLE IF EXISTS memos" }));
    let open_resp = call_plugin("libsql", "open", &json!({ "path": path }));
    log.push(json!({ "step": "open", "result": if plugin_ok(&open_resp) { Value::Null } else { Value::String(plugin_error(&open_resp, "open failed")) } }));
    let create_resp = call_plugin("libsql", "exec", &json!({ "path": path, "sql": "CREATE TABLE memos (id INTEGER PRIMARY KEY, text TEXT, emb F32_BLOB(4))" }));
    log.push(json!({ "step": "create_table", "result": if plugin_ok(&create_resp) { Value::Null } else { Value::String(plugin_error(&create_resp, "exec failed")) } }));
    let insert_resp = call_plugin("libsql", "exec", &json!({ "path": path, "sql": "INSERT INTO memos(text, emb) VALUES ('hello', vector('[0.1,0.2,0.3,0.4]'))" }));
    log.push(json!({ "step": "insert", "result": if plugin_ok(&insert_resp) { Value::Null } else { Value::String(plugin_error(&insert_resp, "exec failed")) } }));
    let index_resp = call_plugin("libsql", "exec", &json!({ "path": path, "sql": "CREATE INDEX memos_idx ON memos(libsql_vector_idx(emb, 'metric=cosine'))" }));
    log.push(json!({ "step": "create_index", "result": if plugin_ok(&index_resp) { Value::Null } else { Value::String(plugin_error(&index_resp, "exec failed")) } }));
    let query_resp = call_plugin("libsql", "query", &json!({ "path": path, "sql": "SELECT id, text, vector_distance_cos(emb, vector('[0.1,0.2,0.3,0.4]')) AS d FROM vector_top_k('memos_idx', vector('[0.1,0.2,0.3,0.4]'), 5) JOIN memos ON memos.rowid = id" }));
    log.push(json!({ "step": "vector_top_k", "rows": resp_rows_or_null(&query_resp) }));
    let _ = call_plugin("libsql", "exec", &json!({ "path": path, "sql": "DROP TABLE IF EXISTS memos" }));
    let _ = call_plugin("libsql", "close", &json!({ "path": path }));
    let version_resp = call_plugin("libsql", "version", &json!({}));
    let libsql_version = version_resp.get("version").cloned().unwrap_or(Value::Null);
    let failures: Vec<Value> = log.iter()
        .filter(|s| s.get("result").map(|r| !r.is_null()).unwrap_or(false))
        .cloned()
        .collect();
    let all_ok = failures.is_empty();
    pack(json!({ "ok": all_ok, "smoke": log, "failures": failures, "libsql_version": libsql_version }).to_string())
}

fn resp_rows_or_null(resp: &Value) -> Value {
    if plugin_ok(resp) { resp.get("rows").cloned().unwrap_or(Value::Null) } else { Value::Null }
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64_decode(s.trim()).ok()
}

fn sql_serialize(body: &Value) -> u64 {
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "serialize", &json!({ "db": name, "path": path }));
    if !plugin_ok(&resp) {
        return err_plugin("sql_serialize", &resp, "serialize failed");
    }
    let bytes_b64 = match resp.get("bytes_b64").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return err("sql_serialize", "plugin response missing bytes_b64"),
    };
    let size = resp.get("size").and_then(|v| v.as_u64())
        .unwrap_or_else(|| b64_decode(&bytes_b64).map(|b| b.len() as u64).unwrap_or(0));
    ok("sql_serialize", json!({ "bytes_b64": bytes_b64, "size": size, "db_name": name }))
}

fn sql_deserialize(body: &Value) -> u64 {
    let s = match body.get("bytes_b64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_deserialize", "missing bytes_b64"),
    };
    let bytes = match b64_decode(s) { Some(b) => b, None => return err("sql_deserialize", "invalid base64") };
    let size = bytes.len();
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "deserialize", &json!({ "db": name, "path": path, "bytes_b64": s }));
    if plugin_ok(&resp) {
        ok("sql_deserialize", json!({ "restored": size, "db_name": name }))
    } else {
        err_plugin("sql_deserialize", &resp, "deserialize failed")
    }
}

fn codeinsight_index(body: &Value) -> u64 {
    let root = body.get("root").and_then(|v| v.as_str()).unwrap_or(".");
    let max_files = body.get("max_files").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
    pack(crate::code_index::index(root, max_files).to_string())
}

fn body_cwd(body: &Value) -> Option<&str> {
    body.get("cwd").and_then(|v| v.as_str())
        .or_else(|| body.get("repo").and_then(|v| v.as_str()))
}

fn run_git_checked(argv: &[&str], cwd: Option<&str>, verb: &str, fallback: &str) -> Result<Value, u64> {
    let r = git_call_argv(argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        return Err(err(verb, r.get("stderr").and_then(|x| x.as_str()).unwrap_or(fallback)));
    }
    Ok(r)
}

fn git_status(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let porcelain = git_porcelain_in(cwd);
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

fn branch_status(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let branch = exec_git_in(cwd, "rev-parse --abbrev-ref HEAD").trim().to_string();
    if branch.is_empty() {
        return err("branch_status", "unable to determine branch");
    }
    let remote = exec_git_in(cwd, &format!("config --get branch.{}.remote", branch)).trim().to_string();
    let remote = if remote.is_empty() { "origin".to_string() } else { remote };
    if !body.get("no_fetch").and_then(|v| v.as_bool()).unwrap_or(false) {
        let _ = exec_git_in(cwd, &format!("fetch {} {}", remote, branch));
    }
    let counts = exec_git_in(cwd, &format!("rev-list --left-right --count {}/{}...HEAD", remote, branch));
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

fn resolve_ref(cwd: Option<&str>, refspec: &str) -> Option<String> {
    let sha = exec_git_in(cwd, &format!("rev-parse {}", refspec)).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

fn verify_push_landed(cwd: Option<&str>, branch: &str, local_head: &str, remote_before: Option<&str>) -> Result<(String, bool), String> {
    let _ = git_call_argv(&["fetch", "origin", branch], cwd);
    let remote_after = resolve_ref(cwd, &format!("origin/{}", branch));
    let remote_after = match remote_after {
        Some(s) => s,
        None => return Err(format!("could not resolve origin/{} after push -- remote-tracking ref missing", branch)),
    };
    if remote_after != local_head {
        return Err(format!(
            "push claimed success but origin/{} ({}) does not match local HEAD ({}) after fetch -- remote did not actually advance to this commit",
            branch, remote_after, local_head
        ));
    }
    let already_current = remote_before.map(|b| b == local_head).unwrap_or(false);
    Ok((remote_after, already_current))
}

fn git_push(body: &Value) -> u64 {
    let repo = body_cwd(body).map(String::from);
    let explicit_branch = body.get("branch").and_then(|v| v.as_str()).map(String::from);
    let current_branch = exec_git_in(repo.as_deref(), "rev-parse --abbrev-ref HEAD").trim().to_string();
    let branch = explicit_branch.clone().unwrap_or_else(|| {
        if current_branch == "HEAD" { "main".to_string() } else { current_branch.clone() }
    });
    if branch.is_empty() {
        return err("git_push", "unable to determine branch");
    }
    if explicit_branch.is_none() && current_branch != "main" && current_branch != "HEAD" {
        log_deviation_push("push-non-main-branch", &current_branch);
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": current_branch,
            "reason": format!(
                "current checkout is on branch '{}', not 'main' -- project rule is direct-push to main always, never a branch. This is likely a worktree checked out on a non-main ref. Pass explicit {{\"branch\":\"main\"}} to push to main from this worktree (git_push pushes HEAD to that ref), or {{\"branch\":\"{}\"}} if a non-main push is genuinely intended.",
                current_branch, current_branch
            ),
            "next_dispatch": "instruction",
        }).to_string());
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
                "worktree dirty in {} -- commit or revert before pushing branch {}; an unpushed delta over a dirty tree is an unwitnessed slice. Porcelain:\n{}{}",
                repo.as_deref().unwrap_or("cwd"), branch, porcelain_preview, more
            ),
            "next_dispatch": "instruction",
            "next_action_hint": "Read porcelain field, decide stage-and-commit OR revert, dispatch git_status to confirm clean, then re-dispatch git_push. Do NOT retry git_push with the same dirty tree -- the gate will deny again.",
        }).to_string());
    }
    let local_head_before = exec_git_in(repo.as_deref(), "rev-parse HEAD").trim().to_string();
    if local_head_before.is_empty() {
        return err("git_push", "could not resolve local HEAD before push -- refusing to proceed without a known starting point");
    }
    let _ = git_call_argv(&["fetch", "origin", &branch], repo.as_deref());
    let remote_before = resolve_ref(repo.as_deref(), &format!("origin/{}", branch));
    let (mut push_out, mut push_succeeded) = exec_git_push_in(repo.as_deref(), &branch);
    let mut attempts = 0u32;
    let mut rebased = false;
    while !push_succeeded && attempts < 3 {
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
                    "push rejected (remote moved); pull --rebase origin {} conflicted and was aborted -- worktree could not be cleanly replayed onto origin. Resolve manually. Rebase output:\n{}",
                    branch, rebase_out
                ),
                "next_dispatch": "instruction",
            }).to_string());
        }
        rebased = true;
        let (out, ok_now) = exec_git_push_in(repo.as_deref(), &branch);
        push_out = out;
        push_succeeded = ok_now;
    }
    if !push_succeeded {
        log_deviation_push("push-remote-outpaces", &branch);
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": branch,
            "reason": format!(
                "push to {} failed (non-zero exit_code) after {} rebase-retries -- remote is moving faster than the push can land, or a real error occurred. Re-dispatch git_push after the remote settles. Last output:\n{}",
                branch, attempts, push_out
            ),
            "next_dispatch": "instruction",
        }).to_string());
    }
    let local_head_after = exec_git_in(repo.as_deref(), "rev-parse HEAD").trim().to_string();
    match verify_push_landed(repo.as_deref(), &branch, &local_head_after, remote_before.as_deref()) {
        Err(reason) => {
            log_deviation_push("push-claimed-success-unverified", &branch);
            pack(json!({
                "ok": false,
                "verb": "git_push",
                "verification_failed": true,
                "repo": repo,
                "branch": branch,
                "local_head": local_head_after,
                "remote_before": remote_before,
                "subprocess_output": push_out,
                "reason": reason,
                "next_dispatch": "instruction",
            }).to_string())
        }
        Ok((remote_sha, already_current)) => ok("git_push", json!({
            "branch": branch,
            "repo": repo,
            "output": push_out,
            "rebased": rebased,
            "rebase_retries": attempts,
            "remote_advanced": !already_current,
            "already_current": already_current,
            "remote_sha": remote_sha,
            "local_head": local_head_after,
        })),
    }
}

fn git_add(body: &Value) -> u64 {
    let repo = body.get("repo").and_then(|v| v.as_str());
    let cwd = body.get("cwd").and_then(|v| v.as_str()).or(repo);
    let paths: Vec<String> = body.get("paths")
        .or_else(|| body.get("files"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut argv: Vec<&str> = vec!["add"];
    if paths.is_empty() {
        argv.push("-A");
    } else {
        for p in &paths { argv.push(p.as_str()); }
    }
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        return err("git_add", r.get("stderr").and_then(|x| x.as_str()).unwrap_or("git add failed"));
    }
    ok("git_add", json!({ "staged": if paths.is_empty() { vec!["-A".to_string()] } else { paths } }))
}

fn bundle_prd_commit_comments(cwd: Option<&str>, message: &str) -> String {
    let notes = crate::orchestrator::prd::drain_pending_commit_comments(cwd);
    if notes.is_empty() {
        return message.to_string();
    }
    let mut out = message.to_string();
    out.push_str("\n\nResolved PRD rows:\n");
    for (id, comment) in &notes {
        out.push_str(&format!("- {}: {}\n", id, comment));
    }
    out
}

fn git_commit(body: &Value) -> u64 {
    let repo = body.get("repo").and_then(|v| v.as_str());
    let cwd = body.get("cwd").and_then(|v| v.as_str()).or(repo);
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").trim();
    if message.is_empty() {
        return err("git_commit", "message required");
    }
    let allow_empty = body.get("allow_empty").and_then(|v| v.as_bool()).unwrap_or(false);
    if git_porcelain_in(cwd).trim().is_empty() && !allow_empty {
        return ok("git_commit", json!({ "nothing_to_commit": true }));
    }
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let _ = git_call_argv(&["add", "-A"], cwd);
    let bundled_message = bundle_prd_commit_comments(cwd, message);
    let mut argv: Vec<&str> = vec!["commit", "-m", bundled_message.as_str()];
    if allow_empty { argv.push("--allow-empty"); }
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        let serr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        let sout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
        if sout.contains("nothing to commit") || serr.contains("nothing to commit") {
            return ok("git_commit", json!({ "nothing_to_commit": true }));
        }
        return err("git_commit", if serr.is_empty() { sout } else { serr });
    }
    let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    if head_after.is_empty() || head_after == head_before {
        return err("git_commit", "commit reported success (exit 0) but HEAD did not move -- refusing to claim committed:true without a real new sha");
    }
    let sha = head_after[..head_after.len().min(10)].to_string();
    let summary = message.lines().next().unwrap_or("").to_string();
    ok("git_commit", json!({ "committed": true, "sha": sha, "summary": summary }))
}

fn git_finalize(body: &Value) -> u64 {
    let repo = body_cwd(body).map(String::from);
    let cwd = repo.clone();
    let cwd_ref = cwd.as_deref();
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let mut steps: Vec<Value> = vec![];
    let mut committed = false;
    let mut sha = String::new();
    let mut summary = String::new();
    let head_before_any_commit = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();

    let dirty = !git_porcelain_in(cwd_ref).trim().is_empty();
    if dirty {
        if message.is_empty() {
            return err("git_finalize", "worktree dirty but no commit message provided -- pass {message}");
        }
        let _ = git_call_argv(&["add", "-A"], cwd_ref);
        let bundled_message = bundle_prd_commit_comments(cwd_ref, message.as_str());
        let cr = git_call_argv(&["commit", "-m", bundled_message.as_str()], cwd_ref);
        let ccode = cr.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        if ccode != 0 {
            let serr = cr.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            let sout = cr.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
            if !(sout.contains("nothing to commit") || serr.contains("nothing to commit")) {
                return err("git_finalize", &format!("commit failed: {}", if serr.is_empty() { sout } else { serr }));
            }
        } else {
            let head_after = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
            if head_after.is_empty() || head_after == head_before_any_commit {
                return err("git_finalize", "commit reported success (exit 0) but HEAD did not move -- refusing to claim committed:true without a real new sha");
            }
            committed = true;
            sha = head_after[..head_after.len().min(10)].to_string();
            summary = message.lines().next().unwrap_or("").to_string();
            emit_event("git.commit", json!({ "sha": sha, "summary": summary, "repo": repo }));
            steps.push(json!({ "step": "commit", "sha": sha, "summary": summary }));
        }
    } else {
        let pending_notes = crate::orchestrator::prd::peek_pending_commit_comments(cwd_ref);
        if !pending_notes.is_empty() {
            let flush_message = if message.is_empty() { "chore: flush resolved PRD notes".to_string() } else { message.clone() };
            let bundled_message = bundle_prd_commit_comments(cwd_ref, flush_message.as_str());
            let _ = git_call_argv(&["add", "-A"], cwd_ref);
            let cr = git_call_argv(&["commit", "--allow-empty", "-m", bundled_message.as_str()], cwd_ref);
            let ccode = cr.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
            let head_after = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
            if ccode == 0 && !head_after.is_empty() && head_after != head_before_any_commit {
                committed = true;
                sha = head_after[..head_after.len().min(10)].to_string();
                summary = bundled_message.lines().next().unwrap_or("").to_string();
                steps.push(json!({ "step": "commit", "sha": sha, "summary": summary, "flushed_pending_prd_notes": true }));
            }
        }
    }

    if !committed {
        let ahead_result = git_call("rev-list --count @{u}..HEAD", cwd_ref);
        let ahead_code = ahead_result.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        let ahead_stderr = ahead_result.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        let no_upstream = ahead_code != 0 && (ahead_stderr.contains("no upstream") || ahead_stderr.contains("unknown revision") || ahead_stderr.contains("@{u}"));
        let ahead_n: u64 = ahead_result.get("stdout").and_then(|x| x.as_str()).unwrap_or("0").trim().parse().unwrap_or(0);
        if !dirty && !no_upstream && ahead_n == 0 {
            return ok("git_finalize", json!({
                "nothing_to_commit": true,
                "committed": false,
                "pushed": false,
                "steps": [{"step": "commit", "nothing_to_commit": true}],
            }));
        }
        sha = head_before_any_commit[..head_before_any_commit.len().min(10)].to_string();
        summary = exec_git_in(cwd_ref, "log -1 --pretty=%s").trim().to_string();
        steps.push(json!({
            "step": "commit",
            "nothing_new_to_commit": true,
            "already_ahead_of_upstream": true,
            "sha": sha,
            "summary": summary,
            "no_upstream": no_upstream,
        }));
    }

    let leftover = git_porcelain_in(cwd_ref);
    if !leftover.trim().is_empty() {
        return err("git_finalize", &format!("worktree still dirty after commit (untriaged residual) -- refusing push. Porcelain:\n{}", leftover.lines().take(8).collect::<Vec<_>>().join("\n")));
    }

    let push_resp_packed = git_push(body);
    let push_resp = unpack_to_value(push_resp_packed);
    let pushed = push_resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !pushed {
        return pack(json!({
            "ok": false,
            "verb": "git_finalize",
            "committed": committed,
            "pushed": false,
            "sha": sha,
            "steps": steps,
            "push_result": push_resp,
            "reason": "commit landed (or nothing to commit) but push was refused -- read push_result.reason",
            "next_dispatch": "instruction",
        }).to_string());
    }
    emit_event("git.push", json!({ "repo": repo, "sha": sha }));
    let push_data = push_resp.get("data");
    let branch = push_data.and_then(|d| d.get("branch")).and_then(|b| b.as_str()).unwrap_or("").to_string();
    let remote_advanced = push_data.and_then(|d| d.get("remote_advanced")).and_then(|b| b.as_bool()).unwrap_or(false);
    let already_current = push_data.and_then(|d| d.get("already_current")).and_then(|b| b.as_bool()).unwrap_or(false);
    let remote_sha = push_data.and_then(|d| d.get("remote_sha")).and_then(|s| s.as_str()).unwrap_or("").to_string();
    steps.push(json!({ "step": "push", "branch": branch, "remote_advanced": remote_advanced, "already_current": already_current }));
    ok("git_finalize", json!({
        "committed": committed,
        "pushed": true,
        "sha": sha,
        "summary": summary,
        "branch": branch,
        "remote_advanced": remote_advanced,
        "already_current": already_current,
        "remote_sha": remote_sha,
        "steps": steps,
    }))
}

fn git_log(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let count = body.get("limit").and_then(|v| v.as_u64())
        .or_else(|| body.get("count").and_then(|v| v.as_u64()))
        .unwrap_or(10);
    let nflag = format!("-{}", count);
    let range = body.get("range").and_then(|v| v.as_str())
        .or_else(|| body.get("ref").and_then(|v| v.as_str()))
        .unwrap_or("").trim();
    let mut argv: Vec<&str> = vec!["log", &nflag, "--oneline", "--no-color"];
    if !range.is_empty() { argv.push(range); }
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        let stderr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        return err_json("git_log", json!({
            "error": stderr,
            "range": range,
            "hint": "git rejected the range; check both endpoints exist locally (a remote-tracking ref may need git_fetch first)"
        }));
    }
    let out = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let commits: Vec<Value> = out.lines().filter(|l| !l.is_empty()).map(|l| {
        let mut it = l.splitn(2, ' ');
        let sha = it.next().unwrap_or("").to_string();
        let subject = it.next().unwrap_or("").to_string();
        json!({ "sha": sha, "subject": subject })
    }).collect();
    ok("git_log", json!({ "commits": commits }))
}

fn git_diff(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let staged = body.get("staged").and_then(|v| v.as_bool()).unwrap_or(false);
    let path = body.get("path").and_then(|v| v.as_str());
    let range = body.get("range").and_then(|v| v.as_str())
        .or_else(|| body.get("ref").and_then(|v| v.as_str()))
        .unwrap_or("").trim();
    let stat = body.get("stat").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut argv: Vec<&str> = vec!["diff", "--no-color"];
    if staged { argv.push("--staged"); }
    if stat { argv.push("--stat"); }
    if !range.is_empty() { argv.push(range); }
    if let Some(p) = path { argv.push("--"); argv.push(p); }
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        let stderr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        return err_json("git_diff", json!({
            "error": stderr,
            "range": range,
            "hint": "git rejected the range; an empty diff must never be inferred from a rejected argument -- check both endpoints exist locally"
        }));
    }
    let mut diff = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let truncated = diff.len() > 60000;
    if truncated { diff.truncate(60000); }
    ok("git_diff", json!({ "diff": diff, "truncated": truncated, "range": range }))
}

fn git_show(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");
    let stat = body.get("stat").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut argv: Vec<&str> = vec!["show", "--no-color"];
    if stat { argv.push("--stat"); }
    argv.push(refspec);
    let r = git_call_argv(&argv, cwd);
    let mut out = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if out.len() > 60000 { out.truncate(60000); }
    ok("git_show", json!({ "output": out }))
}

fn git_fetch(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let remote = body.get("remote").and_then(|v| v.as_str()).unwrap_or("origin");
    let r = git_call_argv(&["fetch", remote], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 { return err("git_fetch", &out); }
    ok("git_fetch", json!({ "remote": remote, "output": out }))
}

fn ci_status_resolve_repo(body: &Value, cwd: Option<&str>) -> Result<String, u64> {
    if let Some(explicit) = body.get("repo").and_then(|v| v.as_str()) {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(explicit.to_string());
        }
    }
    let url = exec_git_in(cwd, "config --get remote.origin.url").trim().to_string();
    if url.is_empty() {
        return Err(err("ci-status", "repo required (pass {repo:\"owner/name\"} or run inside a checkout with an origin remote)"));
    }
    let trimmed = url.trim_end_matches(".git");
    let owner_name = trimmed
        .rsplit_once('/')
        .and_then(|(rest, name)| rest.rsplit_once(['/', ':']).map(|(_, owner)| format!("{}/{}", owner, name)));
    match owner_name {
        Some(s) if s.contains('/') => Ok(s),
        _ => Err(err("ci-status", &format!("could not parse owner/name from origin remote url: {}", url))),
    }
}

fn ci_status_resolve_sha(body: &Value, cwd: Option<&str>) -> Result<String, u64> {
    let requested = body.get("sha").and_then(|v| v.as_str())
        .or_else(|| body.get("ref").and_then(|v| v.as_str()))
        .unwrap_or("latest").trim();
    if requested.is_empty() || requested.eq_ignore_ascii_case("latest") {
        let sha = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
        if sha.is_empty() {
            return Err(err("ci-status", "sha not provided and unable to resolve local HEAD"));
        }
        return Ok(sha);
    }
    Ok(requested.to_string())
}

fn ci_status_token() -> Option<String> {
    if let Some(s) = unpack_to_string(unsafe { host_env_get(b"GITHUB_TOKEN".as_ptr(), 12) }) {
        if !s.is_empty() { return Some(s); }
    }
    if let Some(s) = unpack_to_string(unsafe { host_env_get(b"GH_TOKEN".as_ptr(), 8) }) {
        if !s.is_empty() { return Some(s); }
    }
    None
}

fn ci_status_conclusion_to_status(conclusion: &str, gh_status: &str) -> &'static str {
    if gh_status != "completed" {
        return "pending";
    }
    match conclusion {
        "success" => "success",
        "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure" => "failure",
        _ => "unknown",
    }
}

fn ci_status(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let repo = match ci_status_resolve_repo(body, cwd) { Ok(r) => r, Err(e) => return e };
    let sha = match ci_status_resolve_sha(body, cwd) { Ok(s) => s, Err(e) => return e };
    let url = format!("https://api.github.com/repos/{}/actions/runs?head_sha={}&per_page=20", repo, sha);
    if let Err(reason) = crate::config_path::validate_fetch_url(&url) {
        return err_coded("ci-status", ERR_CODE_INVALID_ARGS, &reason);
    }
    let token = body.get("token").and_then(|v| v.as_str()).map(String::from).or_else(ci_status_token);
    let mut headers = json!({
        "Accept": "application/vnd.github+json",
        "User-Agent": "plugkit-ci-status",
    });
    if let Some(t) = &token {
        headers["Authorization"] = json!(format!("Bearer {}", t));
    }
    let opts = json!({ "timeoutMs": FETCH_DEFAULT_TIMEOUT_MS, "headers": headers }).to_string();
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let resp = unpack_to_value(packed);
    if resp.is_null() {
        return err("ci-status", "host_fetch empty");
    }
    let body_text = resp.get("body").and_then(|v| v.as_str())
        .or_else(|| resp.get("text").and_then(|v| v.as_str()))
        .unwrap_or("");
    let status_code = resp.get("status").and_then(|v| v.as_i64())
        .or_else(|| resp.get("statusCode").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    if status_code != 200 {
        return err_json("ci-status", json!({
            "error": format!("GitHub Actions API returned HTTP {}", status_code),
            "repo": repo,
            "sha": sha,
            "response": body_text,
        }));
    }
    let parsed: Value = serde_json::from_str(body_text).unwrap_or(Value::Null);
    let runs = parsed.get("workflow_runs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if runs.is_empty() {
        return ok("ci-status", json!({
            "status": "unknown",
            "repo": repo,
            "sha": sha,
            "failed_jobs": [],
            "run_url": Value::Null,
            "reason": "no workflow runs found for this sha yet",
        }));
    }
    let mut overall = "success";
    let mut failed_jobs: Vec<Value> = vec![];
    let mut run_url: Option<String> = None;
    let mut any_pending = false;
    let mut any_failure = false;
    for run in &runs {
        let gh_status = run.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let conclusion = run.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        let per_run_status = ci_status_conclusion_to_status(conclusion, gh_status);
        let html_url = run.get("html_url").and_then(|v| v.as_str()).map(String::from);
        if run_url.is_none() { run_url = html_url.clone(); }
        if per_run_status == "pending" { any_pending = true; }
        if per_run_status == "failure" {
            any_failure = true;
            failed_jobs.push(json!({
                "name": run.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "conclusion": conclusion,
                "run_url": html_url,
            }));
        }
    }
    if any_failure {
        overall = "failure";
    } else if any_pending {
        overall = "pending";
    }
    ok("ci-status", json!({
        "status": overall,
        "repo": repo,
        "sha": sha,
        "failed_jobs": failed_jobs,
        "run_url": run_url,
        "run_count": runs.len(),
    }))
}

fn git_branch(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let current = exec_git_in(cwd, "rev-parse --abbrev-ref HEAD").trim().to_string();
    let listing = exec_git_in(cwd, "branch --no-color");
    let branches: Vec<String> = listing.lines()
        .map(|l| l.trim_start_matches('*').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    ok("git_branch", json!({ "current": current, "branches": branches }))
}

fn git_checkout(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body.get("ref").and_then(|v| v.as_str()).unwrap_or("").trim();
    if refspec.is_empty() { return err("git_checkout", "ref required"); }
    let create = body.get("create").and_then(|v| v.as_bool()).unwrap_or(false);
    let argv: Vec<&str> = if create { vec!["checkout", "-b", refspec] } else { vec!["checkout", refspec] };
    if let Err(e) = run_git_checked(&argv, cwd, "git_checkout", "checkout failed") { return e; }
    ok("git_checkout", json!({ "checked_out": refspec, "created": create }))
}

fn git_merge(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body.get("ref").and_then(|v| v.as_str()).unwrap_or("").trim();
    if refspec.is_empty() { return err("git_merge", "ref required"); }
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let ff_only = body.get("ff_only").and_then(|v| v.as_bool()).unwrap_or(false);
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").trim();
    let mut argv: Vec<&str> = vec!["merge", "--no-edit"];
    if ff_only { argv.push("--ff-only"); }
    if !message.is_empty() { argv.push("-m"); argv.push(message); }
    argv.push(refspec);
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 {
        let conflicts: Vec<String> = exec_git_in(cwd, "diff --name-only --diff-filter=U")
            .lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        return err_json("git_merge", json!({
            "error": out,
            "conflicted": !conflicts.is_empty(),
            "conflicts": conflicts,
            "head": head_before,
            "hint": "resolve each conflicted path, git_add them, then git_commit; or git_merge_abort to restore the pre-merge HEAD"
        }));
    }
    let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    ok("git_merge", json!({
        "merged": refspec,
        "head_before": head_before,
        "head_after": head_after,
        "already_up_to_date": head_before == head_after,
        "fast_forward": out.contains("Fast-forward"),
        "output": out
    }))
}

fn git_merge_abort(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    if let Err(e) = run_git_checked(&["merge", "--abort"], cwd, "git_merge_abort", "merge abort failed") { return e; }
    ok("git_merge_abort", json!({ "aborted": true, "head": exec_git_in(cwd, "rev-parse HEAD").trim() }))
}

fn git_branch_delete(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let name = body.get("branch").and_then(|v| v.as_str()).unwrap_or("").trim();
    if name.is_empty() { return err("git_branch_delete", "branch required"); }
    let remote = body.get("remote").and_then(|v| v.as_bool()).unwrap_or(false);
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    if remote {
        let remote_name = body.get("remote_name").and_then(|v| v.as_str()).unwrap_or("origin");
        let r = git_call_argv(&["push", remote_name, "--delete", name], cwd);
        let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        let out = format!("{}{}",
            r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
            r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
        if code != 0 { return err("git_branch_delete", &out); }
        return ok("git_branch_delete", json!({ "deleted": name, "scope": "remote", "remote": remote_name, "output": out }));
    }
    let flag = if force { "-D" } else { "-d" };
    let r = git_call_argv(&["branch", flag, name], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 {
        return err_json("git_branch_delete", json!({
            "error": out,
            "unmerged_guard": !force && out.contains("not fully merged"),
            "hint": "git refused because the branch holds commits reachable from nowhere else; merge it first, or pass force true only if that work is genuinely disposable"
        }));
    }
    ok("git_branch_delete", json!({ "deleted": name, "scope": "local", "forced": force, "output": out }))
}

fn git_rm(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let paths: Vec<String> = body.get("paths").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if paths.is_empty() { return err("git_rm", "paths required"); }
    let cached = body.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut argv: Vec<&str> = vec!["rm"];
    if cached { argv.push("--cached"); }
    argv.push("-r");
    for p in &paths { argv.push(p.as_str()); }
    if let Err(e) = run_git_checked(&argv, cwd, "git_rm", "git rm failed") { return e; }
    ok("git_rm", json!({ "removed": paths, "cached": cached }))
}

fn git_revert(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    if let Some(arr) = body.get("paths").and_then(|v| v.as_array()) {
        let paths: Vec<String> = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        if paths.is_empty() { return err("git_revert", "paths empty"); }
        let mut argv: Vec<&str> = vec!["checkout", "--"];
        for p in &paths { argv.push(p.as_str()); }
        if let Err(e) = run_git_checked(&argv, cwd, "git_revert", "discard failed") { return e; }
        return ok("git_revert", json!({ "discarded": paths }));
    }
    if let Some(refspec) = body.get("ref").and_then(|v| v.as_str()) {
        if let Err(e) = run_git_checked(&["revert", "--no-edit", refspec], cwd, "git_revert", "revert failed") { return e; }
        return ok("git_revert", json!({ "reverted": refspec }));
    }
    err("git_revert", "pass {paths:[...]} to discard working changes or {ref} to revert a commit")
}

fn git_reset(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("mixed");
    let mode_flag = match mode {
        "soft" => "--soft",
        "hard" => "--hard",
        _ => "--mixed",
    };
    if let Err(e) = run_git_checked(&["reset", mode_flag, refspec], cwd, "git_reset", "reset failed") { return e; }
    ok("git_reset", json!({ "reset_to": refspec, "mode": mode }))
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
    super::host_abi::porcelain_or_dirty(git_call("status --porcelain", repo))
}

fn exec_git_push_in(repo: Option<&str>, branch: &str) -> (String, bool) {
    let v = git_call(&format!("push origin HEAD:{}", branch), repo);
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
    let exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1);
    (format!("{}{}", stdout, stderr), exit_code == 0)
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
    install_panic_hook();
    let dispatched_verb = read_str(verb_ptr as *const u8, verb_len);
    #[cfg(target_arch = "wasm32")]
    let panic_start_ms = unsafe { host_now_ms() };
    let result = std::panic::catch_unwind(|| {
        dispatch_verb_inner(verb_ptr, verb_len, body_ptr, body_len)
    });
    match result {
        Ok(packed) => packed,
        Err(payload) => {
            let msg = payload.downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic during dispatch".to_string());
            let attributed = if dispatched_verb.is_empty() { "dispatch_verb" } else { dispatched_verb.as_str() };
            #[cfg(target_arch = "wasm32")]
            {
                let ms = unsafe { host_now_ms() }.saturating_sub(panic_start_ms);
                emit_event("dispatch.end", serde_json::json!({
                    "verb": attributed,
                    "ms": ms,
                    "panicked": true,
                    "error_code": ERR_CODE_PANIC,
                }));
            }
            err_json(attributed, json!({
                "error": msg,
                "error_code": ERR_CODE_PANIC,
                "panicked": true,
                "boundary": "dispatch_verb catch_unwind",
            }))
        }
    }
}

fn request_fingerprint(verb: &str, body_s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    verb.hash(&mut hasher);
    body_s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn verb_body_must_be_json(verb: &str) -> bool {
    !matches!(
        verb,
        "browser"
            | "exec_js" | "nodejs" | "javascript" | "node" | "js"
            | "python" | "py"
            | "bash" | "sh" | "shell" | "zsh"
            | "powershell" | "ps1"
    )
}

fn stamp_request_identity(mut value: Value, fingerprint: &str, body_parse_failed: bool, dispatch_id: Option<&str>) -> u64 {
    if let Some(obj) = value.as_object_mut() {
        obj.insert("request_fingerprint".to_string(), json!(fingerprint));
        if body_parse_failed {
            obj.insert("body_parse_error".to_string(), json!(true));
        }
        if let Some(id) = dispatch_id {
            obj.insert("dispatch_id".to_string(), json!(id));
        }
    }
    pack(value.to_string())
}

fn dispatch_verb_inner(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    let verb = read_str(verb_ptr as *const u8, verb_len);
    let body_s = read_str(body_ptr as *const u8, body_len);
    let fingerprint = request_fingerprint(&verb, &body_s);
    let body_parse_failed = verb_body_must_be_json(&verb)
        && !body_s.is_empty()
        && serde_json::from_str::<Value>(&body_s).is_err();
    let body: Value = if body_s.is_empty() { Value::Null } else {
        serde_json::from_str(&body_s).unwrap_or(Value::Null)
    };
    let dispatch_session_id = body.get("sessionId").and_then(|v| v.as_str())
        .or_else(|| body.get("session_id").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    super::events::set_dispatch_session_id(dispatch_session_id);
    let result_packed = dispatch_gated_verb(&verb, &body, &body_s);
    super::events::set_dispatch_session_id(None);
    let result_value = super::host_abi::unpack_to_value(result_packed);
    #[cfg(target_arch = "wasm32")]
    let dispatch_id = {
        let cwd = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let exit_code = if result_value.get("ok").and_then(|v| v.as_bool()).unwrap_or(true) { 0 } else { 1 };
        Some(crate::dispatch_ledger::record(cwd, &verb, &fingerprint, exit_code))
    };
    #[cfg(not(target_arch = "wasm32"))]
    let dispatch_id: Option<String> = None;
    stamp_request_identity(result_value, &fingerprint, body_parse_failed, dispatch_id.as_deref())
}

fn dispatch_gated_verb(verb: &str, body: &Value, body_s: &str) -> u64 {
    #[cfg(target_arch = "wasm32")]
    let dispatch_start_ms = unsafe { host_now_ms() };
    let gate = crate::gates::check_dispatch(verb, body);
    if !gate.allowed {
        return pack(gate.to_denial_json(verb).to_string());
    }
    let cwd_for_witness = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    crate::browser_witness::record_from_body(cwd_for_witness, body);
    if crate::orchestrator::is_orchestrator_verb(verb) {
        let (out, err_msg, code) = crate::orchestrator::dispatch(verb, "", body_s);
        #[cfg(target_arch = "wasm32")]
        {
            let ms = unsafe { host_now_ms() }.saturating_sub(dispatch_start_ms);
            emit_event("dispatch.end", serde_json::json!({ "verb": verb, "ms": ms }));
        }
        if code == 0 {
            let data: Value = serde_json::from_str(&out).unwrap_or(Value::String(out));
            return ok(verb, data);
        }
        return err_json(verb, json!({ "error": err_msg, "stdout": out, "exitCode": code }));
    }
    let body = body.clone();
    let body_s = body_s.to_string();
    let result = match verb {
        "fs_read" => fs_read(&body),
        "fs_write" => fs_write(&body),
        "fs_readdir" => fs_readdir(&body),
        "fs_stat" => fs_stat(&body),
        "fetch" => fetch(&body),
        "env_get" => env_get(&body),
        "kv_get" => kv_get(&body),
        "kv_put" => kv_put(&body),
        "kv_query" => kv_query(&body),
        "exec_js" | "nodejs" | "javascript" | "node" | "js" => exec_js(&body, &body_s),
        "lang" => lang(&body),
        "browser" => browser(&body, &body_s),
        "health" => health(&body),
        "config_resolve" => config_resolve_report_winning_tier_and_any_rejected_tier(&body),
        "sql_open" => sql_open(&body),
        "sql_close" => sql_close(&body),
        "sql_list_dbs" => sql_list_dbs(&body),
        "sql_exec" => sql_exec(&body),
        "sql_query" => sql_query(&body),
        "sql_smoke" => sql_smoke(),
        "sql_serialize" => sql_serialize(&body),
        "sql_deserialize" => sql_deserialize(&body),
        "cache_get" => cache_get(&body),
        "cache_put" => cache_put(&body),
        "cache_invalidate" => cache_invalidate(&body),
        "cache_stats" => cache_stats(&body),
        "codeinsight_index" => codeinsight_index(&body),
        "codesearch" => codesearch(&body),
        "memorize" => memorize_with_raw(&body, &body_s),
        "memorize-prune" | "memorize_prune" => memorize_prune(&body),
        "recall" => recall(&body),
        "python" | "py" => shell_exec(&body, &body_s, "python"),
        "bash" | "sh" | "shell" | "zsh" => shell_exec(&body, &body_s, "bash"),
        "powershell" | "ps1" => shell_exec(&body, &body_s, "powershell"),
        "ssh" => shell_exec(&body, &body_s, "ssh"),
        "go" | "rust" | "c" | "cpp" | "java" | "deno" => shell_exec(&body, &body_s, &verb),
        "status" => status(&body),
        "wait" | "sleep" => err_coded(&verb, ERR_CODE_UNSUPPORTED, "verb not supported: wasm has no real timer/async-sleep primitive here; use exec:sleep (bash `sleep N`, JS setTimeout via exec_js, or PowerShell Start-Sleep) for an actual wait"),
        "close" => close(&body),
        "filter" => filter(&body, &body_s),
        "git_status" => git_status(&body),
        "branch_status" => branch_status(&body),
        "git_push" => git_push(&body),
        "git_add" => git_add(&body),
        "git_commit" => git_commit(&body),
        "git_finalize" => git_finalize(&body),
        "git_log" => git_log(&body),
        "git_diff" => git_diff(&body),
        "git_show" => git_show(&body),
        "git_fetch" => git_fetch(&body),
        "ci-status" | "ci_status" => ci_status(&body),
        "git_branch" => git_branch(&body),
        "git_checkout" => git_checkout(&body),
        "git_merge" => git_merge(&body),
        "git_merge_abort" => git_merge_abort(&body),
        "git_branch_delete" => git_branch_delete(&body),
        "git_rm" => git_rm(&body),
        "git_revert" => git_revert(&body),
        "git_reset" => git_reset(&body),
        "forget" => forget(&body),
        "learn" => err_coded("learn", ERR_CODE_RETIRED_VERB, "verb retired: the rs-learn crate is removed; memory routes through memorize/recall/memorize-prune (md corpus at .gm/memories + gm.db index)"),
        "discipline" => discipline(&body),
        "" => err_coded("", ERR_CODE_INVALID_ARGS, "verb required"),
        _ => err_coded(&verb, ERR_CODE_UNKNOWN_VERB, "unknown verb"),
    };
    #[cfg(target_arch = "wasm32")]
    {
        let ms = unsafe { host_now_ms() }.saturating_sub(dispatch_start_ms);
        emit_event("dispatch.end", serde_json::json!({ "verb": verb, "ms": ms }));
    }
    result
}
