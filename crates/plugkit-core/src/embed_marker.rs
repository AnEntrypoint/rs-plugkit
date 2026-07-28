#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::wasm_dispatch::{host_cwd_string, host_read, host_write};

const MARKER_REL: &str = ".gm/.embed-generation";

const COMPONENT_SEPARATOR: u8 = 0xff;

fn fnv1a_over(components: &[&str]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for c in components {
        for b in c.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= COMPONENT_SEPARATOR as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:x}", h)
}

pub fn embed_generation_key_for(model: &str, dim: usize, query_prefix: &str) -> String {
    fnv1a_over(&[model, &dim.to_string(), query_prefix])
}

pub fn embed_generation_key() -> String {
    embed_generation_key_for(
        crate::embed::EMBED_MODEL_NAME,
        crate::vecstore::EXPECTED_EMBED_DIM,
        crate::embed::EMBED_QUERY_PREFIX_IDENTITY,
    )
}

fn marker_path() -> Option<String> {
    let root = host_cwd_string()?;
    let root = root.trim_end_matches(['/', '\\']);
    if root.is_empty() {
        return None;
    }
    Some(format!("{}/{}", root, MARKER_REL))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedGenerationState {
    Matches,
    Absent,
    Changed,
}

pub fn embed_generation_state() -> EmbedGenerationState {
    let path = match marker_path() {
        Some(p) => p,
        None => return EmbedGenerationState::Absent,
    };
    match host_read(&path) {
        Some(s) => {
            let recorded = s.trim();
            if recorded.is_empty() {
                EmbedGenerationState::Absent
            } else if recorded == embed_generation_key() {
                EmbedGenerationState::Matches
            } else {
                EmbedGenerationState::Changed
            }
        }
        None => EmbedGenerationState::Absent,
    }
}

pub fn embed_generation_changed() -> bool {
    embed_generation_state() == EmbedGenerationState::Changed
}

pub fn record_embed_generation() -> bool {
    let path = match marker_path() {
        Some(p) => p,
        None => return false,
    };
    let key = embed_generation_key();
    let wrote = host_write(&path, &key);
    if wrote {
        crate::wasm_dispatch::emit_event(
            "embed_generation_recorded",
            json!({
                "key": key,
                "model": crate::embed::EMBED_MODEL_NAME,
                "dim": crate::vecstore::EXPECTED_EMBED_DIM,
            }),
        );
    }
    wrote
}
