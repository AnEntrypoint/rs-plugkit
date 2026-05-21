#![cfg(target_arch = "wasm32")]

use std::sync::OnceLock;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct, PositionEmbeddingType};
use tokenizers::Tokenizer;

extern "C" {
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
}

fn elog(msg: &str) {
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}

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
    let r = CTX.get_or_init(|| {
        let res = init_ctx();
        if let Err(ref e) = res {
            elog(&format!("embed::init_ctx FAILED: {}", e));
        } else {
            elog(&format!(
                "embed::init_ctx OK (safetensors={}B tokenizer={}B)",
                MODEL_SAFETENSORS.len(),
                TOKENIZER_JSON.len()
            ));
        }
        res
    });
    match r {
        Ok(c) => Ok(c),
        Err(e) => {
            elog(&format!("embed::ctx returning cached init failure: {}", e));
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
                elog(&format!("embed::embed_text step '{}' failed: {}", $label, e));
                return None;
            }
        }
    };
}

pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let c = match ctx() {
        Ok(c) => c,
        Err(e) => {
            elog(&format!("embed::embed_text ctx() failed: {} (text_len={})", e, text.len()));
            return None;
        }
    };

    let enc = step!("tokenizer.encode", c.tokenizer.encode(text, true));
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

    let hidden_raw = step!("model.forward", c.model.forward(&ids_t, &token_type_ids, Some(&mask_t)));
    let hidden = step!("hidden.to_dtype(F32)", hidden_raw.to_dtype(DType::F32));

    let mask_f = step!("mask.to_dtype(F32)", mask_t.to_dtype(DType::F32));
    let mask_e = step!("mask.unsqueeze(2)", mask_f.unsqueeze(2));
    let masked = step!("hidden.broadcast_mul(mask)", hidden.broadcast_mul(&mask_e));
    let sum = step!("masked.sum(1)", masked.sum(1));
    let denom_s = step!("mask.sum(1)", mask_f.sum(1));
    let denom = step!("denom.unsqueeze(1)", denom_s.unsqueeze(1));
    let pooled = step!("sum.broadcast_div(denom)", sum.broadcast_div(&denom));

    let flat_t = step!("pooled.flatten_all", pooled.flatten_all());
    let flat: Vec<f32> = step!("flat.to_vec1", flat_t.to_vec1());
    if flat.len() != EMBED_DIM {
        elog(&format!("embed::embed_text dim mismatch: got={} expected={}", flat.len(), EMBED_DIM));
        return None;
    }
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
