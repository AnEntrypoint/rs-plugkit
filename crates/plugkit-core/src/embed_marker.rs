#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::wasm_dispatch::{host_cwd_string, host_read, host_write};

const MARKER_REL: &str = ".gm/.embed-generation";

fn marker_rel_for_table(table: &str) -> String {
    format!("{}.{}", MARKER_REL, table)
}

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

fn marker_path_rel(rel: &str) -> Option<String> {
    let root = host_cwd_string()?;
    let root = root.trim_end_matches(['/', '\\']);
    if root.is_empty() {
        return None;
    }
    Some(format!("{}/{}", root, rel))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedGenerationState {
    Matches,
    Absent,
    Changed,
}

fn embed_generation_state_at(rel: &str) -> EmbedGenerationState {
    let path = match marker_path_rel(rel) {
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

pub fn embed_generation_state() -> EmbedGenerationState {
    embed_generation_state_at(MARKER_REL)
}

pub fn embed_generation_changed() -> bool {
    embed_generation_state() == EmbedGenerationState::Changed
}

pub fn embed_generation_changed_for_table(table: &str) -> bool {
    embed_generation_state_at(&marker_rel_for_table(table)) == EmbedGenerationState::Changed
}

pub fn record_embed_generation() -> bool {
    record_embed_generation_at(MARKER_REL)
}

pub fn record_embed_generation_for_table(table: &str) -> bool {
    record_embed_generation_at(&marker_rel_for_table(table))
}

fn record_embed_generation_at(rel: &str) -> bool {
    let path = match marker_path_rel(rel) {
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
                "scope": rel,
            }),
        );
    }
    wrote
}
