#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

const SCENE_META_START_DELIMITER: &str = "-----META-START-----";
const SCENE_META_END_DELIMITER: &str = "-----META-END-----";

fn parse_scene_meta_and_content_block(raw: &str, filename: &str) -> Value {
    let start = raw.find(SCENE_META_START_DELIMITER);
    let end = raw.find(SCENE_META_END_DELIMITER);
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
    let meta_block = &raw[start + SCENE_META_START_DELIMITER.len()..end];
    let content = raw[end + SCENE_META_END_DELIMITER.len()..].trim();
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

pub fn read_l2_scene_block_files(data_dir: &str, limit: u64) -> Result<Value, String> {
    let scene_blocks_dir = format!("{}/scene_blocks", data_dir.trim_end_matches('/'));
    let listing = crate::pkfs::readdir(&scene_blocks_dir);
    let mut markdown_filenames: Vec<String> = listing
        .as_ref()
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    markdown_filenames.retain(|n| n.ends_with(".md"));
    markdown_filenames.sort();
    markdown_filenames.truncate(limit as usize);

    let mut parsed_scenes = Vec::with_capacity(markdown_filenames.len());
    for name in &markdown_filenames {
        let path = format!("{}/{}", scene_blocks_dir, name);
        if let Some(content) = crate::pkfs::read_to_string(&path) {
            parsed_scenes.push(parse_scene_meta_and_content_block(&content, name));
        }
    }
    Ok(Value::Array(parsed_scenes))
}

pub fn read_l3_persona_file(data_dir: &str) -> Value {
    let path = format!("{}/persona.md", data_dir.trim_end_matches('/'));
    match crate::pkfs::read_to_string(&path) {
        Some(content) => json!({"exists": true, "content": content}),
        None => json!({"exists": false, "content": ""}),
    }
}

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

pub fn read_l0_conversation_messages(vectors_db_path: &str, session_id: Option<&str>, limit: u64) -> Result<Value, String> {
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

pub fn read_skills(vectors_db_path: &str, head_only: bool, limit: u64) -> Result<Value, String> {
    let where_clause = if head_only { "WHERE is_head = 1" } else { "" };
    let sql = format!(
        "SELECT row_id, skill_id, version, is_head, user_id, owner_agent_id, team_id, task_id, name, description, content, content_hash, manifest_json, storage_dir, status, metadata_json, created_at_ms, updated_at_ms FROM skills {} ORDER BY updated_at_ms DESC LIMIT {}",
        where_clause, limit
    );
    query_rows(vectors_db_path, &sql)
}

pub fn read_skill_version_history(vectors_db_path: &str, skill_id: &str) -> Result<Value, String> {
    let sql = format!(
        "SELECT row_id, skill_id, version, is_head, owner_agent_id, team_id, manifest_json, storage_dir, status, created_at_ms, updated_at_ms FROM skills WHERE skill_id = '{}' ORDER BY version ASC",
        escape_sql_literal(skill_id)
    );
    query_rows(vectors_db_path, &sql)
}

pub fn read_skills_summary(vectors_db_path: &str, limit: u64) -> Result<Value, String> {
    let head_rows = read_skills(vectors_db_path, true, limit)?;
    let rows = head_rows.as_array().cloned().unwrap_or_default();

    let mut skill_count_by_owner: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut skill_count_by_status: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut skill_summaries = Vec::with_capacity(rows.len());

    for row in &rows {
        let owner = row.get("owner_agent_id").and_then(Value::as_str).unwrap_or("unknown").to_string();
        let status = row.get("status").and_then(Value::as_str).unwrap_or("unknown").to_string();
        let increment_count = |m: &mut serde_json::Map<String, Value>, k: &str| {
            let n = m.get(k).and_then(Value::as_i64).unwrap_or(0);
            m.insert(k.to_string(), json!(n + 1));
        };
        increment_count(&mut skill_count_by_owner, &owner);
        increment_count(&mut skill_count_by_status, &status);

        let manifest_raw = row.get("manifest_json").and_then(Value::as_str).unwrap_or("");
        let manifest: Value = serde_json::from_str(manifest_raw).unwrap_or(Value::Null);
        let manifest_resource_files = manifest
            .get("files")
            .or_else(|| manifest.get("resources"))
            .or_else(|| manifest.get("resource_files"))
            .cloned()
            .unwrap_or(json!([]));

        skill_summaries.push(json!({
            "skill_id": row.get("skill_id"),
            "version": row.get("version"),
            "owner_agent_id": owner,
            "team_id": row.get("team_id"),
            "status": status,
            "storage_dir": row.get("storage_dir"),
            "manifest_present": !manifest_raw.is_empty() && manifest != Value::Null,
            "resource_files": manifest_resource_files,
            "updated_at_ms": row.get("updated_at_ms"),
        }));
    }

    Ok(json!({
        "head_skill_count": rows.len(),
        "by_owner": skill_count_by_owner,
        "by_status": skill_count_by_status,
        "skills": skill_summaries,
    }))
}

pub fn read_embedding_provider_fingerprint(vectors_db_path: &str) -> Result<Value, String> {
    let rows = query_rows(
        vectors_db_path,
        "SELECT value FROM embedding_meta WHERE key = 'embedding_provider_info' LIMIT 1",
    )?;
    let embedding_provider_info_json = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|row| row.get("value"))
        .and_then(Value::as_str);
    match embedding_provider_info_json {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| format!("tencentdb_compat: embedding_meta value not valid JSON: {}", e)),
        None => Ok(Value::Null),
    }
}

pub fn probe(vectors_db_path: &str) -> Result<Value, String> {
    let content_bearing_table_names = [
        "l1_records", "l0_conversations", "skills", "entity_teams", "entity_users",
        "entity_agents", "entity_tasks", "entity_knowledge", "memory_audit",
    ];
    let table_name_array_literal = js_string_array_literal(&content_bearing_table_names);
    let code = format!(
        r#"const {{ DatabaseSync }} = require('node:sqlite');
try {{
  const db = new DatabaseSync({}, {{ readOnly: true }});
  const tables = {};
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
        js_string_literal(vectors_db_path),
        table_name_array_literal
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
    let response = exec_js_json(&code, 30_000)?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("rows").cloned().unwrap_or(json!([])))
    } else {
        Err(response.get("error").and_then(Value::as_str).unwrap_or("tencentdb_compat: query failed").to_string())
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

fn js_string_array_literal(values: &[&str]) -> String {
    let mut out = String::with_capacity(values.len() * 16 + 2);
    out.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&js_string_literal(v));
    }
    out.push(']');
    out
}

fn escape_sql_literal(s: &str) -> String {
    s.replace('\'', "''")
}
