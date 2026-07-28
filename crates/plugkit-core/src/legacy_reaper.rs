#![cfg(target_arch = "wasm32")]
//! Self-healing removal of retired plugkit state artifacts.
//!
//! Why this exists: retiring a subsystem upstream does not clean up the state
//! it already wrote into every project that ever ran it. The `rs-learn` crate
//! was removed (memory now routes through memorize/recall/memorize-prune, md
//! corpus at `.gm/memories` indexed into `gm.db`), but every project that ran
//! the old build still carries a `.gm/rs-learn.db` nobody reads or writes --
//! live-witnessed at 32MB, frozen months earlier, sitting beside a live 28MB
//! `gm.db`. Deleting it by hand fixes exactly one repo; the tooling has to
//! reap its own retired artifacts so every project self-heals on next boot.
//!
//! Discipline, in order of importance:
//!   1. NEVER delete a live artifact. The reap list is an explicit allowlist of
//!      paths this codebase has genuinely retired -- never a pattern, never a
//!      heuristic like "old mtime" or "unknown file in .gm/".
//!   2. Idempotent. A per-project marker records the completed reap generation,
//!      so a boot after a successful reap does no filesystem work at all.
//!   3. Concurrency-safe. The plugin is a process-wide instance shared across
//!      concurrently-active projects, so two boots can race here. Deletion is
//!      last-writer-wins-benign (removing an already-removed file is a no-op),
//!      and the marker write is a plain overwrite of an idempotent value.
//!   4. Witnessed. Emits one event naming every artifact reclaimed and the
//!      bytes freed, so the cleanup shows up in the watcher log rather than
//!      silently mutating a user's project.

use serde_json::json;

use crate::wasm_dispatch::{host_cwd_string, host_read, host_remove_file_never_directory, host_stat, host_write};

/// Marker value recorded once a project's reap completes.
///
/// Deliberately DERIVED from the artifact list rather than hand-maintained: a
/// hand-bumped counter is a silent footgun, because forgetting the bump means
/// every project that already wrote a marker skips the new entry forever (the
/// reap is marker-gated, so it never looks at the list again). Keying the
/// marker to the list's own content makes "added an artifact" and "projects
/// re-run the reap" the same event, structurally -- there is nothing left to
/// forget.
///
/// A rename or reordering also changes the key, which merely costs one extra
/// no-op reap pass -- the safe direction to err in.
fn reap_key() -> String {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a
    for a in RETIRED_ARTIFACTS {
        for b in a.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:x}", h)
}

/// Paths, relative to the project root, that this codebase has RETIRED and no
/// longer reads or writes. Every entry needs a comment naming what removed it,
/// so a future reader can verify the claim rather than trust the list.
///
/// Hard invariant: `gm.db` (the live codeinsight + rssearch/rag store) and
/// `.gm/memories` (the md corpus that store is built from) are NEVER listed.
const RETIRED_ARTIFACTS: &[&str] = &[
    // The rs-learn crate is removed; the `learn` verb now returns a retirement
    // error (see wasm_dispatch::verbs) and nothing in the tree opens this
    // database. Its -wal/-shm siblings are listed explicitly rather than
    // glob-matched, so the allowlist stays literal.
    ".gm/rs-learn.db",
    ".gm/rs-learn.db-wal",
    ".gm/rs-learn.db-shm",
];

/// `<project_root>/<rel>`, or None when the host cannot resolve a cwd -- in
/// which case the reaper does nothing at all rather than guessing at a root
/// and deleting relative to the wrong directory.
fn project_path(rel: &str) -> Option<String> {
    let root = host_cwd_string()?;
    let root = root.trim_end_matches(['/', '\\']);
    if root.is_empty() {
        return None;
    }
    Some(format!("{}/{}", root, rel))
}

/// True when this project has already completed a reap for the CURRENT
/// artifact list. Any read failure (missing marker, unreadable) returns false
/// so the reap simply runs -- removing an already-absent file is harmless,
/// while skipping a needed reap is not.
fn already_reaped(marker: &str, key: &str) -> bool {
    match host_read(marker) {
        Some(s) => s.trim() == key,
        None => false,
    }
}

/// Best-effort byte size for reporting only. host_stat's shape varies by host,
/// so accept either `size` or `bytes`, and fall back to 0 rather than failing
/// the reap over a cosmetic number.
fn stat_size(path: &str) -> u64 {
    host_stat(path)
        .and_then(|v| {
            v.get("size")
                .or_else(|| v.get("bytes"))
                .and_then(|n| n.as_u64())
        })
        .unwrap_or(0)
}

/// Remove every retired artifact present in this project, exactly once per
/// generation. Safe to call on every boot; safe to call concurrently from two
/// projects sharing the process-wide plugin instance.
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
        // Size is read purely to report what was reclaimed; a file that
        // vanishes between the stat and the remove (a racing sibling boot) is
        // expected, and host_remove already reports miss rather than erroring.
        let size = stat_size(&abs);
        if host_remove_file_never_directory(&abs) {
            freed_bytes = freed_bytes.saturating_add(size);
            reclaimed.push(json!({ "path": rel, "bytes": size }));
        }
    }

    // Record completion even when nothing was found: a project that never ran
    // the retired subsystem should not re-scan on every boot.
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
