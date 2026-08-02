#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::wasm_dispatch::{host_cwd_string, host_read, host_write};

const MARKER_REL: &str = ".gm/.embed-generation";

/// Per-table marker files, so each store's own dim-mismatch check answers
/// against ITS OWN last-recorded generation instead of a single process-wide
/// value. The single shared `.gm/.embed-generation` file was written ONLY by
/// code_index.rs::ensure_schema_at_cfg, but drop_if_dim_mismatch_at_cfg is
/// also called independently by rssearch_vectors.rs and git_commit_vectors.rs
/// for their OWN separate tables -- once code_index's pass wrote the shared
/// marker as "current", every OTHER store's later dim-mismatch check read
/// embed_generation_changed()==false regardless of whether THAT store's own
/// table had actually been re-checked or re-embedded against the new
/// generation, a real false-negative that could leave a stale-dimension table
/// silently un-dropped. Scoping the marker file per table closes that: each
/// store's own ensure_schema call only ever answers for the table it itself
/// just verified.
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

/// Table-scoped variant: answers for the table's OWN last-recorded
/// generation rather than the shared process-wide marker. See
/// `marker_rel_for_table`'s doc comment for why this exists.
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
