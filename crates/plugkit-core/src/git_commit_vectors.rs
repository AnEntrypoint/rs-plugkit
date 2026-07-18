#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params, SHARED_DB};
use crate::vecstore::{drop_if_dim_mismatch, vec_to_json_literal, EXPECTED_EMBED_DIM};

const TABLE: &str = "git_commit_vectors";
const INDEX: &str = "git_commit_vectors_vec";
const EMBED_BUDGET_MS: u64 = 2000;
const DIFF_CHAR_CAP: usize = 4000;
const LOG_WINDOW: usize = 500;

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

pub fn ensure_schema() -> Result<(), String> {
    shared_ensure_open(&shared_db_path())?;
    let _ = drop_if_dim_mismatch(TABLE, INDEX);
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE, message TEXT, embedding F32_BLOB({}), updated_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0)",
        TABLE, EXPECTED_EMBED_DIM
    ))?;
    let _ = shared_exec(&format!(
        "CREATE INDEX IF NOT EXISTS {} ON {}(libsql_vector_idx(embedding, 'metric=cosine'))",
        INDEX, TABLE
    ));
    Ok(())
}

fn read_watermark() -> Option<String> {
    let sql = format!("SELECT hash FROM {} ORDER BY id DESC LIMIT 1", TABLE);
    let rows = crate::libsql_wasm::query(SHARED_DB, &sql).ok()?;
    rows.as_array()?.first()?.get("hash")?.as_str().map(|s| s.to_string())
}

fn parse_log_entries(stdout: &str) -> Vec<(String, String)> {
    // Format: %x00%H%x00%s%x1e  (NUL-separated hash/subject, RS-separated commits)
    let mut out = Vec::new();
    for rec in stdout.split('\u{1e}') {
        let rec = rec.trim_matches(|c| c == '\u{0}' || c == '\n' || c == '\r');
        if rec.is_empty() { continue; }
        let mut parts = rec.splitn(2, '\u{0}');
        let hash = match parts.next() { Some(h) if h.len() == 40 => h.to_string(), _ => continue };
        let subject = parts.next().unwrap_or("").to_string();
        out.push((hash, subject));
    }
    out
}

fn commit_diff_text(hash: &str) -> String {
    let v = crate::wasm_dispatch::git_call_argv(
        &["show", "--no-color", "--stat=200", "-p", "--first-parent", hash],
        None,
    );
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if !ok || exit_code != 0 { return String::new(); }
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let filtered: String = stdout
        .lines()
        .filter(|l| !l.starts_with("Binary files "))
        .collect::<Vec<_>>()
        .join("\n");
    if filtered.len() > DIFF_CHAR_CAP {
        filtered.chars().take(DIFF_CHAR_CAP).collect()
    } else {
        filtered
    }
}

/// Incrementally embed new commits since the stored watermark, bounded by
/// EMBED_BUDGET_MS wall-clock so a large backlog defers rather than blocking
/// the calling dispatch -- mirrors code_index.rs's partial-pass file-indexing
/// pattern. Also reconciles rows whose hash no longer appears in `git log`
/// (history rewrite: rebase/squash/force-push) by marking them deleted.
pub fn sync_incremental() -> Result<Value, String> {
    ensure_schema()?;
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let log = crate::wasm_dispatch::git_call(
        &format!("log --format=%x00%H%x00%s%x1e -n {}", LOG_WINDOW),
        None,
    );
    let ok = log.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let exit_code = log.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if !ok || exit_code != 0 {
        // Not a git repo, or git unavailable this cwd -- not an error condition
        // for the caller, just nothing to index.
        return Ok(json!({ "synced": false, "reason": "git-log-unavailable" }));
    }
    let stdout = log.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let entries = parse_log_entries(stdout);
    if entries.is_empty() {
        return Ok(json!({ "synced": true, "embedded": 0, "reason": "empty-history" }));
    }

    // Reconcile: mark rows deleted whose hash is absent from the current log window's hash set.
    // Bounded to the same window we just fetched -- a rewrite older than LOG_WINDOW commits back
    // is not reconciled by this pass (acceptable: those hashes are also unlikely to be re-queried).
    let live_hashes: std::collections::HashSet<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
    if let Ok(rows) = shared_query_params(&format!("SELECT hash FROM {} WHERE deleted=0", TABLE), &[]) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(h) = row.get("hash").and_then(|v| v.as_str()) {
                    if !live_hashes.contains(h) {
                        let _ = shared_exec_params(
                            &format!("UPDATE {} SET deleted=1 WHERE hash=?1", TABLE),
                            &[h],
                        );
                        crate::wasm_dispatch::emit_event("git_commit_vector_reconciled_deleted", json!({ "hash": h }));
                    }
                }
            }
        }
    }

    let watermark = read_watermark();
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = shared_query_params(&format!("SELECT hash FROM {}", TABLE), &[]) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(h) = row.get("hash").and_then(|v| v.as_str()) {
                    present.insert(h.to_string());
                }
            }
        }
    }

    let mut embedded = 0u32;
    let mut deferred = 0u32;
    let mut skipped = 0u32;
    for (hash, subject) in &entries {
        if present.contains(hash) { continue; }
        if Some(hash.as_str()) == watermark.as_deref() { continue; }
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > EMBED_BUDGET_MS {
            deferred += 1;
            continue;
        }
        let diff = commit_diff_text(hash);
        let text = if diff.is_empty() {
            subject.clone()
        } else {
            format!("{}\n\n{}", subject, diff)
        };
        let vec = match crate::embed::embed_text(&text) {
            Some(v) if !v.is_empty() => v,
            _ => { skipped += 1; continue; }
        };
        let embedding_sql = format!("vector('{}')", vec_to_json_literal(&vec));
        let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
        // Same shadow-index UPDATE hazard as rssearch_vectors::write (libsql's
        // libsql_vector_idx does not reliably support ON CONFLICT DO UPDATE for
        // a row with an existing vector-index entry) -- delete first so this is
        // always a fresh insert. The `present` check above makes this a no-op
        // in the common case, but a concurrent writer racing between the
        // present-check and this insert would otherwise hit the same failure.
        let delete_sql = format!("DELETE FROM {} WHERE hash=?1", TABLE);
        let _ = shared_exec_params(&delete_sql, &[hash]);
        let sql = format!(
            "INSERT INTO {}(hash, message, embedding, updated_at, deleted) VALUES(?1,?2,{},?3,0)",
            TABLE, embedding_sql
        );
        let now_s = now_ms.to_string();
        match shared_exec_params(&sql, &[hash, subject, &now_s]) {
            Ok(()) => embedded += 1,
            Err(_) => skipped += 1,
        }
    }
    crate::wasm_dispatch::emit_event("git_commit_vectors_synced", json!({
        "embedded": embedded,
        "deferred": deferred,
        "skipped": skipped,
        "window": entries.len(),
    }));
    Ok(json!({ "synced": true, "embedded": embedded, "deferred": deferred, "skipped": skipped }))
}

/// Top-k commit hashes ranked by cosine similarity of (subject + capped diff)
/// embedding against the query embedding. Callers must have a fresh
/// query embedding already (crate::embed::embed_text_json_query).
pub fn search(query_embedding: &Value, limit: usize) -> Result<Vec<(String, String, f64)>, String> {
    let qvec = query_embedding.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect::<Vec<f32>>())
        .ok_or_else(|| "git_commit_vectors search: invalid query embedding".to_string())?;
    if qvec.is_empty() {
        return Err("git_commit_vectors search: empty query embedding".to_string());
    }
    ensure_schema()?;
    let qlit = vec_to_json_literal(&qvec);
    let pool = limit.saturating_mul(5).max(20);
    let sql = format!(
        "SELECT r.hash, r.message, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0 ORDER BY distance ASC LIMIT {}",
        INDEX, pool, TABLE, limit
    );
    let rows = shared_query_params(&sql, &[&qlit, &qlit])?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(arr.len());
    for row in arr {
        let hash = row.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let message = row.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let distance = row.get("distance").and_then(|v| v.as_f64()).unwrap_or(2.0);
        let cos = 1.0 - distance;
        if hash.is_empty() { continue; }
        out.push((hash, message, cos));
    }
    Ok(out)
}
