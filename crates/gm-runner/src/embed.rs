use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct, PositionEmbeddingType};
use tokenizers::Tokenizer;

const EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 512;

const MODEL_URL: &str = "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/model.safetensors";
const MODEL_SHA: &str = "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad";
const TOK_URL: &str = "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/tokenizer.json";
const TOK_SHA: &str = "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66";

struct EmbedCtx {
    tokenizer: Tokenizer,
    model: BertModel,
    device: Device,
}

static CTX: OnceLock<Result<EmbedCtx, String>> = OnceLock::new();

fn weights_dir() -> PathBuf {
    crate::download::install_dir().join("weights")
}

/// Downloads (if absent/mismatched) and loads the bge-small-en-v1.5 BERT
/// weights natively -- same model/source/sha as rs-plugkit's wasm-embedded
/// fallback (crates/plugkit-core/src/embed.rs), but served over
/// host_vec_embed instead of baked into the wasm binary via include_bytes!,
/// so the ~133MB safetensors never need to ship inside plugkit.wasm on the
/// native runner path (that's the bulk of the current ~150MB monolith).
fn ensure_ctx() -> &'static Result<EmbedCtx, String> {
    CTX.get_or_init(|| {
        let dir = weights_dir();
        let model_path = dir.join("bge-small-en-v1.5.safetensors");
        let tok_path = dir.join("bge-tokenizer.json");

        download_verified(MODEL_URL, &model_path, MODEL_SHA)?;
        download_verified(TOK_URL, &tok_path, TOK_SHA)?;

        let tokenizer = Tokenizer::from_file(&tok_path).map_err(|e| format!("tokenizer load: {e}"))?;
        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_path.clone()], DType::F32, &device)
                .map_err(|e| format!("varbuilder safetensors: {e}"))?
        };
        let config = bge_small_config();
        let model = BertModel::load(vb, &config).map_err(|e| format!("bert init: {e}"))?;

        Ok(EmbedCtx { tokenizer, model, device })
    })
}

fn download_verified(url: &str, dest: &Path, expected_sha: &str) -> Result<(), String> {
    if dest.exists() {
        if let Ok(actual) = crate::download::sha256_of_file(dest) {
            if actual.eq_ignore_ascii_case(expected_sha) {
                return Ok(());
            }
        }
    }
    crate::download::download_and_verify(url, dest, expected_sha).map_err(|e| e.to_string())
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

/// Returns L2-normalized embedding of `text`, or an error string on the
/// first call that triggers a failed download/load (subsequent calls reuse
/// the cached OnceLock result, matching embed.rs's lazy-init-once shape).
pub fn embed(text: &str) -> Result<Vec<f32>, String> {
    let ctx = ensure_ctx().as_ref().map_err(|e| e.clone())?;

    let encoding = ctx
        .tokenizer
        .encode(text, true)
        .map_err(|e| format!("tokenize: {e}"))?;
    let mut ids = encoding.get_ids().to_vec();
    ids.truncate(MAX_TOKENS);
    let mut type_ids = encoding.get_type_ids().to_vec();
    type_ids.truncate(MAX_TOKENS);

    let token_ids = Tensor::new(ids.as_slice(), &ctx.device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| format!("token tensor: {e}"))?;
    let token_type_ids = Tensor::new(type_ids.as_slice(), &ctx.device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| format!("type tensor: {e}"))?;

    let output = ctx
        .model
        .forward(&token_ids, &token_type_ids, None)
        .map_err(|e| format!("bert forward: {e}"))?;

    // Mean-pool over the sequence dimension (dim 1), matching the standard
    // sentence-embedding pooling strategy for bge-small.
    let (_batch, seq_len, _hidden) = output.dims3().map_err(|e| format!("dims: {e}"))?;
    let pooled = (output.sum(1).map_err(|e| format!("sum: {e}"))? / seq_len as f64)
        .map_err(|e| format!("mean: {e}"))?;
    let mut values: Vec<f32> = pooled
        .squeeze(0)
        .map_err(|e| format!("squeeze: {e}"))?
        .to_vec1()
        .map_err(|e| format!("to_vec1: {e}"))?;

    l2_normalize(&mut values);
    Ok(values)
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

pub const DIM: usize = EMBED_DIM;
