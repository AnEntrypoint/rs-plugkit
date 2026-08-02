#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::ragconfig::RagConfig;
use crate::shared_db::{shared_ensure_open, shared_exec, shared_exec_params, shared_query_params, SHARED_DB};
use crate::vecns::{self, QueryBudget, VecTableSpec};
use crate::wasm_dispatch::plugin_call;

const MIN_EMBEDS_PER_PASS: u32 = 8;
const DIFF_CHAR_CAP: usize = 4000;
const LOG_WINDOW: usize = 500;

/// See `rssearch_vectors::default_cfg` -- constructed per call, never cached,
/// because the plugin instance is shared across concurrently-active projects.
fn default_cfg() -> RagConfig {
    RagConfig::resolved()
}

fn shared_db_path() -> String {
    crate::code_index::project_db_path(None)
}

fn spec<'a>(path: &'a str, cfg: &'a RagConfig) -> VecTableSpec<'a> {
    VecTableSpec::from_names(path, &cfg.git_commits)
}

pub fn ensure_schema() -> Result<(), String> {
    ensure_schema_cfg(&default_cfg())
}

static SCHEMA_ENSURED: std::sync::Mutex<Option<std::collections::HashSet<(String, usize)>>> =
    std::sync::Mutex::new(None);

pub fn forget_ensured_schema() {
    if let Some(seen) = SCHEMA_ENSURED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        seen.clear();
    }
}

pub fn ensure_schema_cfg(cfg: &RagConfig) -> Result<(), String> {
    let path = shared_db_path();
    let memo_key = (path.clone(), cfg.dim());
    {
        let guard = SCHEMA_ENSURED.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|seen| seen.contains(&memo_key)) {
            return Ok(());
        }
    }
    shared_ensure_open(&path)?;
    // Mismatch guard BEFORE the CREATE -- see the identical ordering note in
    // rssearch_vectors::ensure_schema_cfg. `CREATE TABLE IF NOT EXISTS` will
    // not widen a surviving column.
    let _ = spec(&path, cfg).drop_if_dim_mismatch_cfg(&cfg.embed);
    shared_exec(&format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE, message TEXT, embedding F32_BLOB({}), updated_at INTEGER, deleted INTEGER NOT NULL DEFAULT 0)",
        cfg.git_commits.table, cfg.dim()
    ))?;
    spec(&path, cfg).ensure_index();
    // Table-scoped, not the shared/global marker -- same reasoning as the
    // sibling fix in rssearch_vectors::ensure_schema_cfg.
    crate::embed_marker::record_embed_generation_for_table(&cfg.git_commits.table);
    SCHEMA_ENSURED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(std::collections::HashSet::new)
        .insert(memo_key);
    Ok(())
}

fn read_watermark(cfg: &RagConfig) -> Option<String> {
    let path = shared_db_path();
    let sql = format!("SELECT hash FROM {} ORDER BY id DESC LIMIT 1", cfg.git_commits.table);
    let resp = plugin_call("libsql", "query", &json!({ "db": SHARED_DB, "path": path, "sql": sql, "params": [] }));
    if !resp.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
        return None;
    }
    let rows = resp.get("rows")?;
    rows.as_array()?.first()?.get("hash")?.as_str().map(|s| s.to_string())
}

fn parse_log_entries(stdout: &str) -> Vec<(String, String)> {
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

pub fn sync_incremental() -> Result<Value, String> {
    sync_incremental_cfg(&default_cfg())
}

pub fn sync_incremental_cfg(cfg: &RagConfig) -> Result<Value, String> {
    // Clock starts BEFORE ensure_schema_cfg and the git log preamble, not
    // after -- both were previously unbudgeted, and while ensure_schema_cfg
    // is memoized (SCHEMA_ENSURED) so every call after the first is cheap, a
    // large repo's `git log -n 500` shortstat walk and any genuinely first-run
    // schema creation are real costs that should count against this pass's
    // own budget rather than being invisible to every downstream elapsed-time
    // check in this function.
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    ensure_schema_cfg(cfg)?;
    let db_path = shared_db_path();
    let log = crate::wasm_dispatch::git_call(
        &format!("log --format=%x00%H%x00%s%x1e -n {}", LOG_WINDOW),
        None,
    );
    let ok = log.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let exit_code = log.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if !ok || exit_code != 0 {
        return Ok(json!({ "synced": false, "reason": "git-log-unavailable" }));
    }
    let stdout = log.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let entries = parse_log_entries(stdout);
    if entries.is_empty() {
        return Ok(json!({ "synced": true, "embedded": 0, "reason": "empty-history" }));
    }

    let live_hashes: std::collections::HashSet<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
    if let Ok(rows) = shared_query_params(&format!("SELECT hash FROM {} WHERE deleted=0", cfg.git_commits.table), &[]) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(h) = row.get("hash").and_then(|v| v.as_str()) {
                    if !live_hashes.contains(h) {
                        let _ = shared_exec_params(
                            &format!("UPDATE {} SET deleted=1 WHERE hash=?1", cfg.git_commits.table),
                            &[h],
                        );
                        crate::wasm_dispatch::emit_event("git_commit_vector_reconciled_deleted", json!({ "hash": h }));
                    }
                }
            }
        }
    }

    let watermark = read_watermark(cfg);
    let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(rows) = shared_query_params(&format!("SELECT hash FROM {}", cfg.git_commits.table), &[]) {
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
    /// A run this long means the embedder is down, not that one commit is odd.
    /// Small on purpose: the whole cost of being wrong is one extra pass, while
    /// the cost of continuing through a dead embedder is a wedged daemon.
    const MAX_CONSECUTIVE_EMBED_FAILURES: u32 = 5;
    let mut consecutive_embed_failures: u32 = 0;
    let embed_budget_ms = crate::ragconfig::RagConfig::resolved().bulk_embed.git_commit_embed_budget_ms;
    for (hash, subject) in &entries {
        if present.contains(hash) { continue; }
        if Some(hash.as_str()) == watermark.as_deref() { continue; }
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > embed_budget_ms && embedded >= MIN_EMBEDS_PER_PASS {
            deferred += 1;
            continue;
        }
        let diff = commit_diff_text(hash);
        let text = if diff.is_empty() {
            subject.clone()
        } else {
            format!("{}\n\n{}", subject, diff)
        };
        // Routed through the versioned plugin ABI rather than a raw
        // plugin_call, so the distinct failure modes stay distinguishable.
        // The previous shape collapsed every one of them into an empty vec:
        // the bert plugin not being registered at all, the plugin crashing
        // mid-embed, and a genuine empty result were literally the same value,
        // so a dead embedder was silently counted as "skipped" forever. That
        // is the same silent-degradation class that made `recall` return
        // ok:true with zero hits while the embedder was crashed.
        let vec: Vec<f32> = match crate::plugin_abi::call("bert", "embed", &json!({ "text": text })) {
            Ok(data) => {
                // Reset on success so only a genuine RUN trips the breaker --
                // scattered failures across a long history are normal and must
                // not accumulate into a false trip.
                consecutive_embed_failures = 0;
                data.get("embedding")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect::<Vec<f32>>())
                    .unwrap_or_default()
            }
            Err(e) => {
                // N failures IN A ROW is a different condition from one failure:
                // one bad commit is skippable, but a run of them means the
                // embedder itself is down and every remaining call will trap the
                // same way. Continuing burns real time and memory per commit --
                // live-witnessed as 167 failures ~90ms apart during a
                // many-commit session, with the shared daemon reaching 1.3GB RSS
                // and its heartbeat frozen (no busy_until), un-revivable by
                // re-registration. Stop the pass instead; the next one retries
                // from the same cursor, so nothing is lost.
                consecutive_embed_failures += 1;
                crate::wasm_dispatch::emit_event("git_commit_embed_failed", json!({
                    "kind": e.kind.as_str(),
                    "detail": e.message,
                    "consecutive": consecutive_embed_failures,
                }));
                if consecutive_embed_failures >= MAX_CONSECUTIVE_EMBED_FAILURES {
                    crate::wasm_dispatch::emit_event("git_commit_embed_circuit_open", json!({
                        "consecutive": consecutive_embed_failures,
                        "reason": "the embedder failed on every one of the last N commits, so it is down rather than tripping on one input; abandoning this sync pass. The next pass resumes from the same cursor.",
                    }));
                    break;
                }
                Vec::new()
            }
        };
        if vec.is_empty() {
            skipped += 1;
            continue;
        }
        // The bert plugin is a separate wasm module with its own model, so its
        // output width is not guaranteed to track this store's configured dim.
        // Skipping here keeps a half-migrated embedder from spraying INSERTs
        // that libsql rejects one at a time with an opaque blob-size error.
        if vec.len() != cfg.dim() {
            skipped += 1;
            crate::wasm_dispatch::emit_event("git_commit_vector_dim_mismatch", json!({
                "hash": hash,
                "embed_dim": vec.len(),
                "configured_dim": cfg.dim(),
            }));
            continue;
        }
        let embedding_sql = format!("vector('{}')", vecns::qlit(&vec));
        let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
        let delete_sql = format!("DELETE FROM {} WHERE hash=?1", cfg.git_commits.table);
        let _ = spec(&db_path, cfg).exec_params(&delete_sql, &[hash]);
        let sql = format!(
            "INSERT INTO {}(hash, message, embedding, updated_at, deleted) VALUES(?1,?2,{},?3,0)",
            cfg.git_commits.table, embedding_sql
        );
        let now_s = now_ms.to_string();
        match spec(&db_path, cfg).exec_params(&sql, &[hash, subject, &now_s]) {
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

pub fn search(query_embedding: &Value, limit: usize) -> Result<Vec<(String, String, f64)>, String> {
    search_cfg(query_embedding, limit, &default_cfg())
}

pub fn search_cfg(query_embedding: &Value, limit: usize, cfg: &RagConfig) -> Result<Vec<(String, String, f64)>, String> {
    let qvec = query_embedding.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect::<Vec<f32>>())
        .ok_or_else(|| "git_commit_vectors search: invalid query embedding".to_string())?;
    if qvec.is_empty() {
        return Err("git_commit_vectors search: empty query embedding".to_string());
    }
    ensure_schema_cfg(cfg)?;
    let budget = QueryBudget::from_config(&cfg.budget);
    let qlit = vecns::qlit(&qvec);
    let pool = budget.pool(limit);
    // Unlike the rssearch paths, this one truncates to `limit` in SQL: commit
    // hits are returned in raw cosine order with no recency reweight or dedup
    // pass, so nothing downstream can promote a row out of the ANN tail.
    let sql = format!(
        "SELECT r.hash, r.message, vector_distance_cos(r.embedding, vector(?1)) AS distance \
         FROM vector_top_k('{}', vector(?2), {}) AS v JOIN {} AS r ON r.rowid = v.id \
         WHERE r.deleted=0 ORDER BY distance ASC LIMIT {}",
        cfg.git_commits.index, pool, cfg.git_commits.table, limit
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
