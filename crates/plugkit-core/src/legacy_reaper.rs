#![cfg(target_arch = "wasm32")]

use serde_json::json;

use crate::wasm_dispatch::{host_cwd_string, host_read, host_remove_file_never_directory, host_stat, host_write};

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A_64_PRIME: u64 = 0x100000001b3;
const ARTIFACT_SEPARATOR_BYTE: u64 = 0xff;

fn reap_key() -> String {
    let mut h: u64 = FNV1A_64_OFFSET_BASIS;
    for a in RETIRED_ARTIFACTS {
        for b in a.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(FNV1A_64_PRIME);
        }
        h ^= ARTIFACT_SEPARATOR_BYTE;
        h = h.wrapping_mul(FNV1A_64_PRIME);
    }
    format!("{:x}", h)
}

const RETIRED_ARTIFACTS: &[&str] = &[
    ".gm/rs-learn.db",
    ".gm/rs-learn.db-wal",
    ".gm/rs-learn.db-shm",
    ".gm/rslearn-counter.json",
];

fn project_path(rel: &str) -> Option<String> {
    let root = host_cwd_string()?;
    let root = root.trim_end_matches(['/', '\\']);
    if root.is_empty() {
        return None;
    }
    Some(format!("{}/{}", root, rel))
}

fn already_reaped(marker: &str, key: &str) -> bool {
    match host_read(marker) {
        Some(s) => s.trim() == key,
        None => false,
    }
}

fn stat_size(path: &str) -> u64 {
    host_stat(path)
        .and_then(|v| {
            v.get("size")
                .or_else(|| v.get("bytes"))
                .and_then(|n| n.as_u64())
        })
        .unwrap_or(0)
}

pub fn reap_retired_artifacts() {
    let marker = match project_path(".gm/.legacy-reaped") {
        Some(p) => p,
        None => return,
    };
    let key = reap_key();
    if already_reaped(&marker, &key) {
        return;
    }

    let mut reclaimed: Vec<serde_json::Value> = Vec::new();
    let mut freed_bytes: u64 = 0;

    for rel in RETIRED_ARTIFACTS {
        let abs = match project_path(rel) {
            Some(p) => p,
            None => continue,
        };
        let size = stat_size(&abs);
        if host_remove_file_never_directory(&abs) {
            freed_bytes = freed_bytes.saturating_add(size);
            reclaimed.push(json!({ "path": rel, "bytes": size }));
        }
    }

    let _ = host_write(&marker, &key);

    if !reclaimed.is_empty() {
        crate::wasm_dispatch::emit_event(
            "legacy_artifacts_reaped",
            json!({
                "reap_key": key,
                "reclaimed": reclaimed,
                "freed_bytes": freed_bytes,
            }),
        );
    }
}
