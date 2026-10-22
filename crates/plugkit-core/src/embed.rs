#![cfg(target_arch = "wasm32")]

use std::sync::OnceLock;

#[cfg(not(feature = "wit-env"))]
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_vec_embed(text_ptr: *const u8, text_len: u32, out_ptr: *mut f32, out_len: u32) -> i32;
}

#[cfg(feature = "wit-env")]
use crate::wasm_dispatch::host_abi::{host_log, host_vec_embed};

static LAST_EMBED_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn record_embed_failure(reason: String) {
    elog(&format!("embed::failure {reason}"));
    if let Ok(mut slot) = LAST_EMBED_FAILURE.lock() {
        *slot = Some(reason);
    }
}

fn clear_embed_failure() {
    if let Ok(mut slot) = LAST_EMBED_FAILURE.lock() {
        *slot = None;
    }
}

pub fn last_embed_failure() -> Option<String> {
    LAST_EMBED_FAILURE.lock().ok().and_then(|slot| slot.clone())
}

fn try_host_embed(text: &str) -> Option<Vec<f32>> {
    let mut out = vec![0f32; EMBED_DIM];
    let rc = unsafe {
        host_vec_embed(
            text.as_ptr(),
            text.len() as u32,
            out.as_mut_ptr(),
            EMBED_DIM as u32,
        )
    };
    if rc == EMBED_DIM as i32 {
        l2_normalize(&mut out);
        return Some(out);
    }
    let reason = if rc < 0 {
        format!("host_vec_embed (bert sibling plugin, via the agentplug host) returned rc={rc} for a {}-byte input: the host-side dispatch itself failed -- bert not loaded for this project, its shared pool slot evicted, or its dispatch deadline exceeded. A retry may help once the bert pool is free", text.len())
    } else {
        format!("host_vec_embed returned rc={rc} but this build expects exactly {EMBED_DIM} dimensions: the host's embedder and this binary's compiled-in model disagree on width. A retry will NOT help -- the host and plugin builds have to match")
    };
    record_embed_failure(reason);
    None
}

fn try_sibling_plugin_embed(text: &str) -> Option<Vec<f32>> {
    let resp = crate::wasm_dispatch::plugin_call(
        "bert",
        "embed",
        &serde_json::json!({ "text": text, "kind": "passage" }),
    );
    let arr = match resp.get("embedding").and_then(|e| e.as_array()) {
        Some(a) => a.clone(),
        None => {
            let why = match &resp {
                serde_json::Value::Null => "host_plugin_call returned no bytes".to_string(),
                serde_json::Value::String(raw) => raw.clone(),
                v => v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("bert answered with no embedding field and no error field")
                    .to_string(),
            };
            record_embed_failure(format!(
                "bert sibling-plugin embed over host_plugin_call also failed for a {}-byte input: {why}. 'unknown_plugin' means bert is not installed for this project at all (check ~/.agentplug/daemon.log for a 'plugin bert failed to compile/install' line -- an install that cannot resolve a release tag leaves the plugin absent); 'plugin_not_loaded_yet'/'evicted' is transient and a retry helps",
                text.len()
            ));
            return None;
        }
    };
    let mut out: Vec<f32> = arr
        .iter()
        .filter_map(|x| x.as_f64())
        .map(|x| x as f32)
        .collect();
    if out.len() != EMBED_DIM {
        record_embed_failure(format!(
            "bert sibling-plugin embed returned {} dimensions but this build expects exactly {EMBED_DIM}: the bert plugin's model and this binary's compiled-in width disagree. A retry will NOT help",
            out.len()
        ));
        return None;
    }
    l2_normalize(&mut out);
    Some(out)
}

fn elog(msg: &str) {
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}

pub const EMBED_MODEL_NAME: &str = "BAAI/bge-small-en-v1.5";

pub const EMBED_QUERY_PREFIX_IDENTITY: &str = BGE_QUERY_PREFIX;

const EMBED_DIM: usize = 384;

const _: () = {
    assert!(
        EMBED_DIM == crate::vecstore::EXPECTED_EMBED_DIM,
        "embed.rs's compiled model width and the vector store's expected width have diverged; \
         a store created at one width cannot hold vectors of the other"
    );
};

const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

pub fn condition_query(query_text: &str) -> String {
    format!("{}{}", BGE_QUERY_PREFIX, query_text)
}

fn custom_getrandom(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    let rc = unsafe {
        crate::wasm_dispatch::host_abi::host_random_fill(buf.as_mut_ptr(), buf.len() as u32)
    };
    if rc == 0 {
        Err(getrandom::Error::UNSUPPORTED)
    } else {
        Ok(())
    }
}
getrandom::register_custom_getrandom!(custom_getrandom);

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(dest: *mut u8, len: usize) -> Result<(), getrandom::Error> {
    custom_getrandom(unsafe { core::slice::from_raw_parts_mut(dest, len) })
}

struct EmbedCtx {
    host_delegated: bool,
}

static CTX: OnceLock<EmbedCtx> = OnceLock::new();

fn probe_host_embed() -> bool {
    let probe_text = "init-probe";
    let mut out = vec![0f32; EMBED_DIM];
    let rc = unsafe {
        host_vec_embed(
            probe_text.as_ptr(),
            probe_text.len() as u32,
            out.as_mut_ptr(),
            EMBED_DIM as u32,
        )
    };
    rc == EMBED_DIM as i32
}

fn init_ctx() -> Result<EmbedCtx, String> {
    if probe_host_embed() {
        crate::wasm_dispatch::emit_event(
            "embed.host-delegated",
            serde_json::json!({
                "embed_dim": EMBED_DIM,
                "reason": "host_vec_embed probe returned EMBED_DIM; skipping wasm safetensors load",
            }),
        );
        elog("embed::init_ctx host-delegated (probe ok); skipping safetensors+tokenizer load");
        return Ok(EmbedCtx {
            host_delegated: true,
        });
    }

    crate::wasm_dispatch::emit_event(
        "embed.slim-build-no-fallback",
        serde_json::json!({
            "reason": "this is a slim build (feature=slim): no wasm-embedded safetensors fallback exists, and the host_vec_embed probe just failed -- embedding is genuinely unavailable this session, not a bug",
        }),
    );
    Err("slim build has no wasm-side embedding fallback -- host_vec_embed must be implemented by the host (e.g. agentplug-runner's native candle path)".to_string())
}

fn ctx() -> Result<&'static EmbedCtx, &'static str> {
    if let Some(c) = CTX.get() {
        return Ok(c);
    }
    let res = init_ctx();
    match res {
        Ok(c) => {
            elog(&format!(
                "embed::init_ctx OK (host_delegated={})",
                c.host_delegated
            ));
            let (safetensors_bytes, tokenizer_bytes) = (0, 0);
            crate::wasm_dispatch::emit_event(
                "embed_init_ok",
                serde_json::json!({
                    "host_delegated": c.host_delegated,
                    "safetensors_bytes": safetensors_bytes,
                    "tokenizer_bytes": tokenizer_bytes,
                }),
            );
            Ok(CTX.get_or_init(|| c))
        }
        Err(e) => {
            elog(&format!(
                "embed::init_ctx FAILED (will retry next call): {}",
                e
            ));
            crate::wasm_dispatch::emit_event(
                "embed_init_fail",
                serde_json::json!({
                    "error": e,
                }),
            );
            Err("embed init failed")
        }
    }
}

fn l2_normalize(v: &mut [f32]) {
    let mut s = 0f32;
    for x in v.iter() {
        s += *x * *x;
    }
    let n = s.sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let cacheable = text.len() <= plain_cache_max_text();
    if cacheable {
        if let Some(v) = cache_get(&PLAIN_CACHE, text) {
            return Some(v);
        }
    }
    let v = embed_text_uncached(text)?;
    if cacheable {
        cache_put(&PLAIN_CACHE, text, &v);
    }
    Some(v)
}

fn embed_text_uncached(text: &str) -> Option<Vec<f32>> {
    clear_embed_failure();
    if let Some(v) = try_host_embed(text) {
        return Some(v);
    }
    let host_failure =
        last_embed_failure().unwrap_or_else(|| "host embedder unavailable".to_string());
    if let Some(v) = try_sibling_plugin_embed(text) {
        elog("embed::embed_text host_vec_embed was unusable; served by the bert sibling plugin over host_plugin_call instead");
        crate::wasm_dispatch::emit_event(
            "embed.sibling_plugin_recovered",
            serde_json::json!({
                "host_vec_embed_failure": host_failure,
                "text_len": text.len(),
            }),
        );
        clear_embed_failure();
        return Some(v);
    }
    let sibling_failure =
        last_embed_failure().unwrap_or_else(|| "bert sibling plugin unavailable".to_string());
    let host_failure = format!("{host_failure}; then {sibling_failure}");
    let c = match ctx() {
        Ok(c) => c,
        Err(e) => {
            record_embed_failure(format!(
                "embedding context init failed ({e}) after the host embedder was unusable ({host_failure}); text_len={}. A retry may help if the failure was transient",
                text.len()
            ));
            return None;
        }
    };

    if c.host_delegated {
        record_embed_failure(format!(
            "host-delegated embedding failed ({host_failure}) and no in-wasm fallback is available (slim build), so nothing can produce a vector for this text"
        ));
        crate::wasm_dispatch::emit_event(
            "embed_fail",
            serde_json::json!({
                "step": "host_delegated_no_fallback",
                "error": host_failure,
            }),
        );
        return None;
    }

    record_embed_failure("this build has no in-wasm embedding model compiled in: host_vec_embed must answer. A retry will not help until the host embedder recovers".to_string());
    None
}

pub fn embed_texts_batch(texts: &[String]) -> Option<Vec<Option<Vec<f32>>>> {
    if texts.is_empty() {
        return Some(Vec::new());
    }
    if texts.len() == 1 {
        return Some(vec![embed_text(&texts[0])]);
    }

    let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
    let mut uncached_idx: Vec<usize> = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        let cacheable = t.len() <= plain_cache_max_text();
        if cacheable {
            if let Some(v) = cache_get(&PLAIN_CACHE, t) {
                out[i] = Some(v);
                continue;
            }
        }
        uncached_idx.push(i);
    }
    if uncached_idx.is_empty() {
        return Some(out);
    }

    let mut still_uncached: Vec<usize> = Vec::new();
    for &i in &uncached_idx {
        let embedded = try_host_embed(&texts[i]).or_else(|| try_sibling_plugin_embed(&texts[i]));
        if let Some(v) = embedded {
            let cacheable = texts[i].len() <= plain_cache_max_text();
            if cacheable {
                cache_put(&PLAIN_CACHE, &texts[i], &v);
            }
            out[i] = Some(v);
        } else {
            still_uncached.push(i);
        }
    }
    if still_uncached.is_empty() {
        return Some(out);
    }

    if ctx().is_err() {
        for &i in &still_uncached {
            out[i] = embed_text(&texts[i]);
        }
    }
    Some(out)
}

pub fn embed_text_json(text: &str) -> Option<serde_json::Value> {
    embed_text_json_passage(text)
}

pub fn embed_text_json_passage(text: &str) -> Option<serde_json::Value> {
    let v = embed_text(text)?;
    Some(vec_to_json(v))
}

pub fn embed_text_json_query(query_text: &str) -> Option<serde_json::Value> {
    let trimmed = query_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(cached) = query_cache_get(trimmed) {
        crate::wasm_dispatch::emit_event(
            "embed.query_cache_hit",
            serde_json::json!({
                "query_len": trimmed.len(),
            }),
        );
        return Some(vec_to_json(cached));
    }

    if let Some(spilled) = query_spill_get(trimmed) {
        query_cache_put(trimmed, &spilled);
        crate::wasm_dispatch::emit_event(
            "embed.query_spill_hit",
            serde_json::json!({
                "query_len": trimmed.len(),
            }),
        );
        return Some(vec_to_json(spilled));
    }

    let v = embed_text(&condition_query(trimmed))?;
    query_cache_put(trimmed, &v);
    query_spill_put(trimmed, &v);
    Some(vec_to_json(v))
}

const QUERY_SPILL_NAMESPACE: &str = "embed-query-spill";

fn query_spill_key(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("q{:016x}-{}", h, text.len())
}

fn query_spill_get(text: &str) -> Option<Vec<f32>> {
    let raw = crate::wasm_dispatch::host_kv_read(QUERY_SPILL_NAMESPACE, &query_spill_key(text))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let arr = parsed.get("v")?.as_array()?;
    if parsed.get("t").and_then(|t| t.as_str()) != Some(text) {
        return None;
    }
    let v: Vec<f32> = arr
        .iter()
        .filter_map(|x| x.as_f64().map(|f| f as f32))
        .collect();
    if v.len() != arr.len() || v.is_empty() {
        return None;
    }
    Some(v)
}

fn query_spill_put(text: &str, v: &[f32]) {
    let payload = serde_json::json!({ "t": text, "v": v });
    let val = payload.to_string();
    let key = query_spill_key(text);
    let ns = QUERY_SPILL_NAMESPACE;
    unsafe {
        crate::wasm_dispatch::host_kv_put(
            ns.as_ptr(),
            ns.len() as u32,
            key.as_ptr(),
            key.len() as u32,
            val.as_ptr(),
            val.len() as u32,
        );
    }
}

fn vec_to_json(v: Vec<f32>) -> serde_json::Value {
    serde_json::Value::Array(
        v.into_iter()
            .map(|f| {
                serde_json::Number::from_f64(f as f64)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            })
            .collect(),
    )
}

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::sync::Mutex;

struct CacheEntry {
    key: String,
    embedding: Vec<f32>,
    ts_ms: i64,
}

struct EmbedCache {
    entries: BTreeMap<u64, CacheEntry>,
    order: VecDeque<u64>,
}

impl EmbedCache {
    const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }
}

static QUERY_CACHE: Mutex<EmbedCache> = Mutex::new(EmbedCache::new());
static PLAIN_CACHE: Mutex<EmbedCache> = Mutex::new(EmbedCache::new());

fn plain_cache_max_text() -> usize {
    crate::ragconfig::RagConfig::resolved()
        .embed_cache
        .plain_cache_max_text_bytes
}

fn query_cache_capacity() -> usize {
    crate::ragconfig::RagConfig::resolved()
        .embed_cache
        .query_cache_capacity
}

fn query_cache_ttl_ms() -> i64 {
    crate::ragconfig::RagConfig::resolved()
        .embed_cache
        .query_cache_ttl_ms
}

fn now_ms() -> i64 {
    unsafe { crate::wasm_dispatch::host_now_ms() as i64 }
}

fn scoped_key(key: &str) -> String {
    let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    format!("{root}\u{1f}{key}")
}

fn cache_slot(key: &str) -> u64 {
    crate::hash::fnv1a64(key.as_bytes())
}

fn cache_get(cache: &Mutex<EmbedCache>, key: &str) -> Option<Vec<f32>> {
    let key = scoped_key(key);
    let slot = cache_slot(&key);
    let mut guard = cache.lock().ok()?;
    let now = now_ms();
    let hit = match guard.entries.get(&slot) {
        Some(e) if e.key == key && now - e.ts_ms < query_cache_ttl_ms() => {
            Some(e.embedding.clone())
        }
        Some(_) => None,
        None => return None,
    };
    if hit.is_none() {
        guard.entries.remove(&slot);
        guard.order.retain(|s| *s != slot);
    }
    hit
}

fn cache_put(cache: &Mutex<EmbedCache>, key: &str, embedding: &[f32]) {
    let key = scoped_key(key);
    let slot = cache_slot(&key);
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let now = now_ms();
    let entry = CacheEntry {
        key,
        embedding: embedding.to_vec(),
        ts_ms: now,
    };
    if guard.entries.insert(slot, entry).is_none() {
        guard.order.push_back(slot);
    }
    while guard.order.len() > query_cache_capacity() {
        if let Some(evicted) = guard.order.pop_front() {
            guard.entries.remove(&evicted);
        }
    }
}

fn query_cache_get(key: &str) -> Option<Vec<f32>> {
    cache_get(&QUERY_CACHE, key)
}

fn query_cache_put(key: &str, embedding: &[f32]) {
    cache_put(&QUERY_CACHE, key, embedding)
}
