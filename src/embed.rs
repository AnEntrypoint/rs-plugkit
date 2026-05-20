#![cfg(target_arch = "wasm32")]

use std::sync::OnceLock;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct, PositionEmbeddingType};
use tokenizers::Tokenizer;

static MODEL_SAFETENSORS: &[u8] = include_bytes!("../weights/minilm-l6-v2.safetensors");
static TOKENIZER_JSON: &[u8] = include_bytes!("../weights/tokenizer.json");

const EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 512;

fn custom_getrandom(buf: &mut [u8]) -> Result<(), getrandom::Error> {
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
    tokenizer: Tokenizer,
    model: BertModel,
    device: Device,
}

static CTX: OnceLock<Result<EmbedCtx, String>> = OnceLock::new();

fn minilm_config() -> Config {
    Config {
        vocab_size: 30522,
        hidden_size: 384,
        num_hidden_layers: 6,
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
    let tokenizer = Tokenizer::from_bytes(TOKENIZER_JSON)
        .map_err(|e| format!("tokenizer load: {}", e))?;

    let device = Device::Cpu;

    let vb = VarBuilder::from_slice_safetensors(MODEL_SAFETENSORS, DType::F16, &device)
        .map_err(|e| format!("varbuilder safetensors: {}", e))?;

    let config = minilm_config();
    let model = BertModel::load(vb, &config)
        .map_err(|e| format!("bert init: {}", e))?;

    Ok(EmbedCtx { tokenizer, model, device })
}

fn ctx() -> Result<&'static EmbedCtx, &'static str> {
    let r = CTX.get_or_init(init_ctx);
    match r {
        Ok(c) => Ok(c),
        Err(_) => Err("embed init failed"),
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

pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let c = ctx().ok()?;

    let enc = c.tokenizer.encode(text, true).ok()?;
    let mut ids: Vec<u32> = enc.get_ids().to_vec();
    let mut mask: Vec<u32> = enc.get_attention_mask().to_vec();
    if ids.len() > MAX_TOKENS {
        ids.truncate(MAX_TOKENS);
        mask.truncate(MAX_TOKENS);
    }
    let seq_len = ids.len();
    if seq_len == 0 { return None; }

    let ids_t = Tensor::from_vec(ids.clone(), (1, seq_len), &c.device).ok()?;
    let mask_t = Tensor::from_vec(mask.clone(), (1, seq_len), &c.device).ok()?;
    let token_type_ids = Tensor::zeros((1, seq_len), DType::U32, &c.device).ok()?;

    let hidden_raw = c.model.forward(&ids_t, &token_type_ids, Some(&mask_t)).ok()?;
    let hidden = hidden_raw.to_dtype(DType::F32).ok()?;

    let mask_f = mask_t.to_dtype(DType::F32).ok()?;
    let mask_e = mask_f.unsqueeze(2).ok()?;
    let masked = hidden.broadcast_mul(&mask_e).ok()?;
    let sum = masked.sum(1).ok()?;
    let denom = mask_f.sum(1).ok()?.unsqueeze(1).ok()?;
    let pooled = sum.broadcast_div(&denom).ok()?;

    let flat: Vec<f32> = pooled.flatten_all().ok()?.to_vec1().ok()?;
    if flat.len() != EMBED_DIM { return None; }
    let mut out = flat;
    l2_normalize(&mut out);
    Some(out)
}

pub fn embed_text_json(text: &str) -> Option<serde_json::Value> {
    let v = embed_text(text)?;
    Some(serde_json::Value::Array(
        v.into_iter()
            .map(|f| serde_json::Number::from_f64(f as f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
            .collect(),
    ))
}
