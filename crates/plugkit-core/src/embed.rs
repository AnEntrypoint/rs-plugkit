#![cfg(target_arch = "wasm32")]

use std::sync::OnceLock;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct, PositionEmbeddingType};
use tokenizers::Tokenizer;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_vec_embed(text_ptr: *const u8, text_len: u32, out_ptr: *mut f32, out_len: u32) -> i32;
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
        Some(out)
    } else {
        None
    }
}

fn elog(msg: &str) {
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}

static MODEL_SAFETENSORS: &[u8] = include_bytes!("../../../weights/bge-small-en-v1.5.safetensors");
static TOKENIZER_JSON: &[u8] = include_bytes!("../../../weights/bge-tokenizer.json");

const EMBED_MODEL_NAME: &str = "BAAI/bge-small-en-v1.5";
const EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 512;

const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

const QUERY_CACHE_CAP: usize = 64;
const QUERY_CACHE_TTL_MS: i64 = 600_000;

fn custom_getrandom(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    #[link(wasm_import_module = "env")]
    extern "C" {
        fn host_random_fill(ptr: *mut u8, len: u32) -> u32;
    }
    let rc = unsafe { host_random_fill(buf.as_mut_ptr(), buf.len() as u32) };
    if rc == 0 {
        Err(getrandom::Error::UNSUPPORTED)
    } else {
        Ok(())
    }
}
getrandom::register_custom_getrandom!(custom_getrandom);

struct EmbedCtx {
    tokenizer: Option<Tokenizer>,
    model: Option<BertModel>,
    device: Device,
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

fn bge_small_config() -> Config {
    Config {
        vocab_size: 30522,
        hidden_size: 384,
        num_hidden_layers: 12,
        num_attention_heads: 12,
        intermediate_size: 1536,
        hidden_act: HiddenAct::Gelu,
        hidden_dropout_prob: 0.1,
        max_position_embeddings: 512,
        type_vocab_size: 2,
        initializer_range: 0.02,
        layer_norm_eps: 1e-12,
        pad_token_id: 0,
        position_embedding_type: PositionEmbeddingType::Absolute,
        use_cache: true,
        classifier_dropout: None,
        model_type: Some("bert".to_string()),
    }
}

fn init_ctx() -> Result<EmbedCtx, String> {
    if probe_host_embed() {
        crate::wasm_dispatch::emit_event("embed.host-delegated", serde_json::json!({
            "embed_dim": EMBED_DIM,
            "reason": "host_vec_embed probe returned EMBED_DIM; skipping wasm safetensors load",
        }));
        elog("embed::init_ctx host-delegated (probe ok); skipping safetensors+tokenizer load");
        return Ok(EmbedCtx {
            tokenizer: None,
            model: None,
            device: Device::Cpu,
            host_delegated: true,
        });
    }

    crate::wasm_dispatch::emit_event("embed.wasm-loading", serde_json::json!({
        "reason": "host_vec_embed probe failed; loading wasm-side bert model",
    }));

    let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
        .map_err(|e| format!("tokenizer load: {}", e))?;

    let device = Device::Cpu;

    let vb = VarBuilder::from_slice_safetensors(MODEL_SAFETENSORS, DType::F32, &device)
        .map_err(|e| format!("varbuilder safetensors: {}", e))?;

    let config = bge_small_config();
    let model = BertModel::load(vb, &config)
        .map_err(|e| format!("bert init: {}", e))?;

    crate::wasm_dispatch::emit_event("embed.model-loaded", serde_json::json!({
        "model": EMBED_MODEL_NAME,
        "embed_dim": EMBED_DIM,
        "num_hidden_layers": config.num_hidden_layers,
        "safetensors_bytes": MODEL_SAFETENSORS.len(),
        "tokenizer_bytes": TOKENIZER_JSON.len(),
    }));

    Ok(EmbedCtx {
        tokenizer: Some(tokenizer),
        model: Some(model),
        device,
        host_delegated: false,
    })
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
            crate::wasm_dispatch::emit_event("embed_init_ok", serde_json::json!({
                "host_delegated": c.host_delegated,
                "safetensors_bytes": if c.host_delegated { 0 } else { MODEL_SAFETENSORS.len() },
                "tokenizer_bytes": if c.host_delegated { 0 } else { TOKENIZER_JSON.len() },
            }));
            Ok(CTX.get_or_init(|| c))
        }
        Err(e) => {
            elog(&format!("embed::init_ctx FAILED (will retry next call): {}", e));
            crate::wasm_dispatch::emit_event("embed_init_fail", serde_json::json!({
                "error": e,
            }));
            Err("embed init failed")
        }
    }
}

fn l2_normalize(v: &mut [f32]) {
    let mut s = 0f32;
    for x in v.iter() { s += *x * *x; }
    let n = s.sqrt();
    if n > 0.0 {
        for x in v.iter_mut() { *x /= n; }
    }
}

macro_rules! step {
    ($label:expr, $expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                let err_s = format!("{}", e);
                elog(&format!("embed::embed_text step '{}' failed: {}", $label, err_s));
                crate::wasm_dispatch::emit_event("embed_fail", serde_json::json!({
                    "step": $label,
                    "error": err_s,
                }));
                return None;
            }
        }
    };
}

pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let cacheable = text.len() <= PLAIN_CACHE_MAX_TEXT;
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
    if let Some(v) = try_host_embed(text) {
        return Some(v);
    }
    let c = match ctx() {
        Ok(c) => c,
        Err(e) => {
            elog(&format!("embed::embed_text ctx() failed: {} (text_len={})", e, text.len()));
            return None;
        }
    };

    if c.host_delegated {
        elog("embed::embed_text host-delegated but host_vec_embed returned non-EMBED_DIM; no wasm fallback available");
        crate::wasm_dispatch::emit_event("embed_fail", serde_json::json!({
            "step": "host_delegated_no_fallback",
            "error": "host_vec_embed returned non-EMBED_DIM and wasm model was skipped at init",
        }));
        return None;
    }

    let tokenizer = match c.tokenizer.as_ref() {
        Some(t) => t,
        None => {
            elog("embed::embed_text tokenizer missing in non-host-delegated ctx");
            return None;
        }
    };
    let model = match c.model.as_ref() {
        Some(m) => m,
        None => {
            elog("embed::embed_text model missing in non-host-delegated ctx");
            return None;
        }
    };

    let enc = step!("tokenizer.encode", tokenizer.encode(text, true));
    let mut ids: Vec<u32> = enc.get_ids().to_vec();
    let mut mask: Vec<u32> = enc.get_attention_mask().to_vec();
    if ids.len() > MAX_TOKENS {
        ids.truncate(MAX_TOKENS);
        mask.truncate(MAX_TOKENS);
    }
    let seq_len = ids.len();
    if seq_len == 0 {
        elog(&format!("embed::embed_text empty tokenization (text_len={})", text.len()));
        return None;
    }

    let ids_t = step!("Tensor::from_vec(ids)", Tensor::from_vec(ids.clone(), (1, seq_len), &c.device));
    let mask_t = step!("Tensor::from_vec(mask)", Tensor::from_vec(mask.clone(), (1, seq_len), &c.device));
    let token_type_ids = step!("Tensor::zeros(token_type_ids)", Tensor::zeros((1, seq_len), DType::U32, &c.device));

    let hidden_raw = step!("model.forward", model.forward(&ids_t, &token_type_ids, Some(&mask_t)));
    drop(ids_t);
    drop(token_type_ids);
    let hidden = step!("hidden.to_dtype(F32)", hidden_raw.to_dtype(DType::F32));
    drop(hidden_raw);

    let mask_f = step!("mask.to_dtype(F32)", mask_t.to_dtype(DType::F32));
    drop(mask_t);
    let mask_e = step!("mask.unsqueeze(2)", mask_f.unsqueeze(2));
    let masked = step!("hidden.broadcast_mul(mask)", hidden.broadcast_mul(&mask_e));
    drop(hidden);
    drop(mask_e);
    let sum = step!("masked.sum(1)", masked.sum(1));
    drop(masked);
    let denom_s = step!("mask.sum(1)", mask_f.sum(1));
    drop(mask_f);
    let denom = step!("denom.unsqueeze(1)", denom_s.unsqueeze(1));
    drop(denom_s);
    let pooled = step!("sum.broadcast_div(denom)", sum.broadcast_div(&denom));
    drop(sum);
    drop(denom);

    let flat_t = step!("pooled.flatten_all", pooled.flatten_all());
    drop(pooled);
    let flat: Vec<f32> = step!("flat.to_vec1", flat_t.to_vec1());
    drop(flat_t);
    if flat.len() != EMBED_DIM {
        elog(&format!("embed::embed_text dim mismatch: got={} expected={}", flat.len(), EMBED_DIM));
        return None;
    }
    let mut out = flat;
    l2_normalize(&mut out);
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
    if trimmed.is_empty() { return None; }

    if let Some(cached) = query_cache_get(trimmed) {
        crate::wasm_dispatch::emit_event("embed.query_cache_hit", serde_json::json!({
            "query_len": trimmed.len(),
        }));
        return Some(vec_to_json(cached));
    }

    let prefixed = format!("{}{}", BGE_QUERY_PREFIX, trimmed);
    let v = embed_text(&prefixed)?;
    query_cache_put(trimmed, &v);
    Some(vec_to_json(v))
}

fn vec_to_json(v: Vec<f32>) -> serde_json::Value {
    serde_json::Value::Array(
        v.into_iter()
            .map(|f| serde_json::Number::from_f64(f as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
            .collect(),
    )
}

use std::sync::Mutex;

struct CacheEntry {
    key: String,
    embedding: Vec<f32>,
    ts_ms: i64,
}

static QUERY_CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());
static PLAIN_CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());

const PLAIN_CACHE_MAX_TEXT: usize = 4096;

fn now_ms() -> i64 {
    unsafe { crate::wasm_dispatch::host_now_ms() as i64 }
}

fn cache_get(cache: &Mutex<Vec<CacheEntry>>, key: &str) -> Option<Vec<f32>> {
    let mut guard = cache.lock().ok()?;
    let now = now_ms();
    guard.retain(|e| now - e.ts_ms < QUERY_CACHE_TTL_MS);
    let idx = guard.iter().position(|e| e.key == key)?;
    let entry = guard.remove(idx);
    let emb = entry.embedding.clone();
    guard.push(entry);
    Some(emb)
}

fn cache_put(cache: &Mutex<Vec<CacheEntry>>, key: &str, embedding: &[f32]) {
    let mut guard = match cache.lock() { Ok(g) => g, Err(_) => return };
    let now = now_ms();
    guard.retain(|e| now - e.ts_ms < QUERY_CACHE_TTL_MS && e.key != key);
    while guard.len() >= QUERY_CACHE_CAP { guard.remove(0); }
    guard.push(CacheEntry { key: key.to_string(), embedding: embedding.to_vec(), ts_ms: now });
}

fn query_cache_get(key: &str) -> Option<Vec<f32>> {
    cache_get(&QUERY_CACHE, key)
}

fn query_cache_put(key: &str, embedding: &[f32]) {
    cache_put(&QUERY_CACHE, key, embedding)
}
