#![cfg(target_arch = "wasm32")]

//! Reads TencentDB Agent Memory's real on-disk `vectors.db` format
//! (Node's built-in `node:sqlite` `DatabaseSync` + the `sqlite-vec`
//! extension) exactly as `MemoryCore/src/core/store/sqlite.ts` writes it.
//!
//! This is the ONLY place format compatibility with the original Tencent
//! project is enforced -- `tencentdb_memory.rs` (gm's own native backend)
//! never needs to match this schema. See
//! `docs/gm-memory-tencentdb-design.md` for the split rationale.
//!
//! rs-plugkit's wasm32-wasip1 target has no viable native path to a real
//! SQLite engine that understands `sqlite-vec`'s `vec0` virtual tables:
//! `rusqlite`/`libsqlite3-sys` have no documented/CI-proven wasm32-wasip1
//! C-compile path, the `sqlite-vec` crate is FFI bindings over rusqlite
//! (same blocker), and gm's own libsql-based vector store
//! (`libsql_wasm.rs`/`shared_db.rs`) uses libsql's own incompatible native
//! vector extension (`F32_BLOB`/`vector_top_k`), not `sqlite-vec`'s `vec0`
//! format -- an unrelated, mutually-incompatible on-disk representation.
//! So, matching the already-proven pattern in `memory_md.rs::rename_batch`
//! and the `lang` plugin runner (`wasm_dispatch/verbs.rs`), this shells a
//! generated Node script through `host_exec_js`: the script opens the file
//! with `node:sqlite`, runs the query, and writes JSON to stdout for this
//! module to parse.

use serde_json::{json, Value};

/// META-delimited scene-block markdown format, exactly as
/// `MemoryCore/src/core/scene/scene-format.ts::parseSceneBlock` writes and
/// reads it -- plain-text delimiters wrapping `key: value` lines, followed
/// by free-text content. Ported directly rather than shelled to Node since
/// this is plain-file parsing, not a SQLite query -- `host_exec_js` stays
/// reserved for the actual `node:sqlite` reads below.
const SCENE_META_START: &str = "-----META-START-----";
const SCENE_META_END: &str = "-----META-END-----";

fn parse_scene_block(raw: &str, filename: &str) -> Value {
    let start = raw.find(SCENE_META_START);
    let end = raw.find(SCENE_META_END);
    let (start, end) = match (start, end) {
        (Some(s), Some(e)) if e > s => (s, e),
        _ => {
            return json!({
                "filename": filename,
                "meta": {"created": "", "updated": "", "summary": "", "heat": 0},
                "content": raw.trim(),
            });
        }
    };
    let meta_block = &raw[start + SCENE_META_START.len()..end];
    let content = raw[end + SCENE_META_END.len()..].trim();
    let field = |name: &str| -> String {
        meta_block
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("{}:", name)))
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let heat: i64 = field("heat").parse().unwrap_or(0);
    json!({
        "filename": filename,
        "meta": {
            "created": field("created"),
            "updated": field("updated"),
            "summary": field("summary"),
            "heat": heat,
        },
        "content": content,
    })
}

/// Read L2 scene-block content directly from `scene_blocks/*.md` under a
/// TencentDB Agent Memory data directory -- these files, not any SQLite
/// table, are the authoritative content source. `.metadata/scene_index.json`
/// (not read here) is explicitly a rebuildable INDEX over these same files
/// (`scene-index.ts::syncSceneIndex` regenerates it by re-scanning
/// `scene_blocks/`), confirmed by direct read of the vendored source with no
/// SQLite/DatabaseSync reference anywhere in the scene module.
pub fn read_l2_scenes(data_dir: &str, limit: u64) -> Result<Value, String> {
    let dir = format!("{}/scene_blocks", data_dir.trim_end_matches('/'));
    let listing = crate::pkfs::readdir(&dir);
    let mut filenames: Vec<String> = listing
        .as_ref()
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    filenames.retain(|n| n.ends_with(".md"));
    filenames.sort();
    filenames.truncate(limit as usize);

    let mut out = Vec::with_capacity(filenames.len());
    for name in &filenames {
        let path = format!("{}/{}", dir, name);
        if let Some(content) = crate::pkfs::read_to_string(&path) {
            out.push(parse_scene_block(&content, name));
        }
    }
    Ok(Value::Array(out))
}

/// Read L3 persona content: a single `persona.md` file
/// (`StoragePaths.persona = "persona.md"` in
/// `MemoryCore/src/core/storage/types.ts`), not a SQLite row -- unlike L1/L2
/// there is exactly one persona per data directory, no query/filter surface.
pub fn read_l3_persona(data_dir: &str) -> Value {
    let path = format!("{}/persona.md", data_dir.trim_end_matches('/'));
    match crate::pkfs::read_to_string(&path) {
        Some(content) => json!({"exists": true, "content": content}),
        None => json!({"exists": false, "content": ""}),
    }
}

/// Read L1 records (structured/summarized memories) from a real
/// `vectors.db`. Returns the raw rows as JSON objects matching
/// `l1_records`'s column names exactly -- callers translate into whatever
/// shape they need (the migration script re-embeds and re-keys; nothing
/// here does).
pub fn read_l1_records(vectors_db_path: &str, team_id: Option<&str>, limit: u64) -> Result<Value, String> {
    let where_clause = match team_id {
        Some(t) => format!("WHERE team_id = '{}'", escape_sql_literal(t)),
        None => String::new(),
    };
    let sql = format!(
        "SELECT record_id, content, type, priority, scene_name, session_key, session_id, team_id, task_id, user_id, agent_id, version, timestamp_str, timestamp_start, timestamp_end, created_time, updated_time, metadata_json FROM l1_records {} ORDER BY updated_time DESC LIMIT {}",
        where_clause, limit
    );
    query_rows(vectors_db_path, &sql)
}

/// Read L0 records (raw per-message chat history).
pub fn read_l0_conversations(vectors_db_path: &str, session_id: Option<&str>, limit: u64) -> Result<Value, String> {
    let where_clause = match session_id {
        Some(s) => format!("WHERE session_id = '{}'", escape_sql_literal(s)),
        None => String::new(),
    };
    let sql = format!(
        "SELECT record_id, session_key, session_id, team_id, task_id, user_id, agent_id, role, message_text, recorded_at, timestamp FROM l0_conversations {} ORDER BY timestamp DESC LIMIT {}",
        where_clause, limit
    );
    query_rows(vectors_db_path, &sql)
}

/// Read Skills (versioned, HEAD only by default).
pub fn read_skills(vectors_db_path: &str, head_only: bool, limit: u64) -> Result<Value, String> {
    let where_clause = if head_only { "WHERE is_head = 1" } else { "" };
    let sql = format!(
        "SELECT row_id, skill_id, version, is_head, user_id, owner_agent_id, team_id, task_id, name, description, content, content_hash, manifest_json, storage_dir, status, metadata_json, created_at_ms, updated_at_ms FROM skills {} ORDER BY updated_at_ms DESC LIMIT {}",
        where_clause, limit
    );
    query_rows(vectors_db_path, &sql)
}

/// Reads the `embedding_meta` fingerprint (`{provider, model, dimensions}`)
/// so a caller can tell what embedding space the source vectors.db's `l1_vec`/
/// `l0_vec`/`skill_vec` tables actually hold -- required before treating any
/// embedding column as directly comparable to gm's own.
///
/// `embedding_meta` is a key-value table (`key TEXT PRIMARY KEY, value TEXT`),
/// not a row of named columns: `sqlite.ts::writeEmbeddingMeta` stores a single
/// row keyed `'embedding_provider_info'` whose `value` is the JSON-encoded
/// `{provider, model, dimensions}` object. Read that row and parse its value.
pub fn read_embedding_meta(vectors_db_path: &str) -> Result<Value, String> {
    let rows = query_rows(
        vectors_db_path,
        "SELECT value FROM embedding_meta WHERE key = 'embedding_provider_info' LIMIT 1",
    )?;
    let raw = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|row| row.get("value"))
        .and_then(Value::as_str);
    match raw {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| format!("tencentdb_compat: embedding_meta value not valid JSON: {}", e)),
        None => Ok(Value::Null),
    }
}

/// Row counts across every content-bearing table, used by the migration
/// script's dry-run summary and by a caller deciding whether a path is
/// really a TencentDB vectors.db before attempting a full read.
pub fn probe(vectors_db_path: &str) -> Result<Value, String> {
    let code = format!(
        r#"const {{ DatabaseSync }} = require('node:sqlite');
try {{
  const db = new DatabaseSync({}, {{ readOnly: true }});
  const tables = ['l1_records','l0_conversations','skills','entity_teams','entity_users','entity_agents','entity_tasks','entity_knowledge','memory_audit'];
  const counts = {{}};
  for (const t of tables) {{
    try {{ counts[t] = db.prepare('SELECT COUNT(*) AS n FROM ' + t).get().n; }}
    catch (e) {{ counts[t] = null; }}
  }}
  db.close();
  process.stdout.write(JSON.stringify({{ ok: true, counts }}));
}} catch (e) {{
  process.stdout.write(JSON.stringify({{ ok: false, error: String(e && e.message || e) }}));
}}"#,
        js_string_literal(vectors_db_path)
    );
    exec_js_json(&code, 30_000)
}

fn query_rows(vectors_db_path: &str, sql: &str) -> Result<Value, String> {
    let code = format!(
        r#"const {{ DatabaseSync }} = require('node:sqlite');
try {{
  const db = new DatabaseSync({}, {{ readOnly: true }});
  const rows = db.prepare({}).all();
  db.close();
  process.stdout.write(JSON.stringify({{ ok: true, rows }}));
}} catch (e) {{
  process.stdout.write(JSON.stringify({{ ok: false, error: String(e && e.message || e) }}));
}}"#,
        js_string_literal(vectors_db_path),
        js_string_literal(sql)
    );
    let resp = exec_js_json(&code, 30_000)?;
    if resp.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(resp.get("rows").cloned().unwrap_or(json!([])))
    } else {
        Err(resp.get("error").and_then(Value::as_str).unwrap_or("tencentdb_compat: query failed").to_string())
    }
}

fn exec_js_json(code: &str, timeout_ms: u64) -> Result<Value, String> {
    let opts = format!("{{\"timeoutMs\":{}}}", timeout_ms);
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(), code.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
    let envelope: Value = serde_json::from_str(&out).map_err(|e| format!("tencentdb_compat: host_exec_js envelope not valid JSON: {}", e))?;
    let stdout = envelope.get("stdout").and_then(Value::as_str).unwrap_or_default();
    if stdout.is_empty() {
        let stderr = envelope.get("stderr").and_then(Value::as_str).unwrap_or_default();
        return Err(format!("tencentdb_compat: empty stdout from node subprocess (stderr: {})", stderr));
    }
    serde_json::from_str(stdout).map_err(|e| format!("tencentdb_compat: node script stdout not valid JSON: {} (stdout: {})", e, stdout))
}

/// Builds a JS double-quoted string literal from an arbitrary Rust &str,
/// escaping backslash/quote/control characters -- values here come from
/// filesystem paths and SQL text under our own control, not untrusted user
/// input, but literal construction still has to be correct or a path
/// containing a quote silently breaks the generated script.
fn js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Single-quoted SQL literal escaping (doubling embedded quotes) for the
/// small set of caller-supplied filter values (team_id/session_id) that get
/// interpolated directly into a generated SQL string rather than bound as a
/// parameter -- `node:sqlite`'s `DatabaseSync.prepare` does support bound
/// params, but the filter clause here is built before the statement text is
/// known, so this module escapes at string-build time instead.
fn escape_sql_literal(s: &str) -> String {
    s.replace('\'', "''")
}
