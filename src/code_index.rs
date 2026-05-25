#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use tree_sitter::{Language, Parser};

use crate::libsql_wasm;
use crate::wasm_dispatch::{host_read, unpack_to_value_pub};

extern "C" {
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_now_ms() -> u64;
}

fn fv_put(ns: &str, key: &str, val: &str) {
    let _ = unsafe { host_kv_put(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
}

fn lang_for_ext(ext: &str) -> Option<(&'static str, Language)> {
    let e = ext.to_lowercase();
    match e.as_str() {
        ".js" | ".mjs" | ".jsx" => Some(("javascript", tree_sitter_javascript::LANGUAGE.into())),
        ".ts" => Some(("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())),
        ".tsx" => Some(("typescript", tree_sitter_typescript::LANGUAGE_TSX.into())),
        ".py" => Some(("python", tree_sitter_python::LANGUAGE.into())),
        ".rs" => Some(("rust", tree_sitter_rust::LANGUAGE.into())),
        ".go" => Some(("go", tree_sitter_go::LANGUAGE.into())),
        ".c" | ".h" => Some(("c", tree_sitter_c::LANGUAGE.into())),
        ".cpp" | ".cc" | ".hpp" | ".hh" | ".cxx" => Some(("cpp", tree_sitter_cpp::LANGUAGE.into())),
        ".java" => Some(("java", tree_sitter_java::LANGUAGE.into())),
        ".json" => Some(("json", tree_sitter_json::LANGUAGE.into())),
        ".html" | ".htm" => Some(("html", tree_sitter_html::LANGUAGE.into())),
        ".css" => Some(("css", tree_sitter_css::LANGUAGE.into())),
        ".sh" | ".bash" => Some(("bash", tree_sitter_bash::LANGUAGE.into())),
        ".md" | ".markdown" => Some(("markdown", tree_sitter_md::LANGUAGE.into())),
        _ => None,
    }
}

const CHUNK_NODE_TYPES: &[&str] = &[
    "function_declaration", "function_definition", "function_item",
    "method_declaration", "method_definition",
    "class_declaration", "class_definition",
    "impl_item", "struct_item", "enum_item", "trait_item",
    "arrow_function",
    "generator_function_declaration",
];

const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".cache",
    ".next", ".nuxt", ".turbo", "coverage", "vendor", ".plugkit-browser-profile",
];

const GM_DB: &str = "gm";

const EXPECTED_EMBED_DIM: usize = 384;

fn embedding_col_dim(db_name: &str, table: &str) -> Option<usize> {
    let sql = format!("SELECT type FROM pragma_table_info('{}') WHERE name = 'embedding'", table);
    let rows = libsql_wasm::query(db_name, &sql).ok()?;
    let arr = rows.as_array()?;
    let row = arr.first()?;
    let ty = row.get("type")?.as_str()?;
    let start = ty.find('(')? + 1;
    let end = ty.find(')')?;
    ty[start..end].parse::<usize>().ok()
}

fn drop_if_dim_mismatch(db_name: &str, table: &str) -> Result<bool, String> {
    match embedding_col_dim(db_name, table) {
        Some(dim) if dim == EXPECTED_EMBED_DIM => Ok(false),
        Some(old_dim) => {
            let _ = libsql_wasm::exec(db_name, &format!("DROP INDEX IF EXISTS {}_vec", table));
            libsql_wasm::exec(db_name, &format!("DROP TABLE IF EXISTS {}", table))?;
            crate::wasm_dispatch::emit_event("table_dropped", serde_json::json!({
                "table": table,
                "old_dim": old_dim,
                "new_dim": EXPECTED_EMBED_DIM,
            }));
            Ok(true)
        }
        None => Ok(false),
    }
}

pub fn ensure_schema_at(db_name: &str, path: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    libsql_wasm::open(db_name, path)?;
    let _ = drop_if_dim_mismatch(db_name, "code_chunks");
    let _ = drop_if_dim_mismatch(db_name, "memories");
    libsql_wasm::exec(db_name, "CREATE TABLE IF NOT EXISTS code_chunks (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, body TEXT, embedding F32_BLOB(384))")?;
    libsql_wasm::exec(db_name, "CREATE TABLE IF NOT EXISTS memories (id INTEGER PRIMARY KEY, namespace TEXT, text TEXT, ts INTEGER, embedding F32_BLOB(384))")?;
    let _ = libsql_wasm::exec(db_name, "CREATE INDEX IF NOT EXISTS code_chunks_vec ON code_chunks(libsql_vector_idx(embedding, 'metric=cosine'))");
    let _ = libsql_wasm::exec(db_name, "CREATE INDEX IF NOT EXISTS memories_vec ON memories(libsql_vector_idx(embedding, 'metric=cosine'))");
    Ok(())
}

fn project_db_path(project_path: Option<&str>) -> String {
    // libsql file-backed open requires WASI fs syscalls (fd_prestat_get etc.)
    // that the wasm host doesn't fully implement yet — using a file path
    // crashes the watcher with 'unimplemented WASI call'. Until the host
    // WASI shim is fleshed out, default to :memory: even when project_path
    // is requested. The KV store at <cwd>/.gm/disciplines/<ns>/ (in the
    // JS-side host_kv_* implementation) already provides project-local
    // persistent memory; libsql is only needed for vector-index ANN.
    let _ = project_path;
    ":memory:".to_string()
}

fn project_db_name(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("gm_ext_{:x}", crc32(p)),
        _ => GM_DB.to_string(),
    }
}

fn crc32(s: &str) -> u32 {
    let mut h: u32 = 0xffffffff;
    for b in s.bytes() {
        h ^= b as u32;
        for _ in 0..8 {
            h = if h & 1 != 0 { (h >> 1) ^ 0xedb88320 } else { h >> 1 };
        }
    }
    !h
}

pub fn ensure_schema() -> Result<(), String> {
    ensure_schema_at(GM_DB, &project_db_path(None))
}

fn ensure_schema_for(project_path: Option<&str>) -> Result<String, String> {
    let name = project_db_name(project_path);
    let path = project_db_path(project_path);
    ensure_schema_at(&name, &path)?;
    Ok(name)
}

fn list_dir(path: &str) -> Vec<String> {
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value_pub(packed);
    match v {
        Value::Array(arr) => arr.into_iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

// thebird (busybase) host returns full flat paths from fs_readdir, not posix-style
// single-level entries. Use the flat list directly; if any entry contains a slash, treat
// the host's output as already-recursive. Otherwise walk one level at a time.
fn collect_files(root: &str, max_files: usize) -> Vec<String> {
    let entries = list_dir(root);
    if entries.is_empty() { return Vec::new(); }
    let has_slashes = entries.iter().any(|e| e.contains('/'));
    if has_slashes {
        // Flat list mode: paths are already complete. Apply skip-dir filter inline.
        return entries.into_iter()
            .filter(|p| !SKIP_DIRS.iter().any(|d| p.split('/').any(|seg| seg == *d)))
            .take(max_files)
            .collect();
    }
    // POSIX mode: walk recursively.
    let mut files = Vec::new();
    walk_posix(root, max_files, &mut files);
    files
}

fn walk_posix(root: &str, max_files: usize, files: &mut Vec<String>) {
    if files.len() >= max_files { return; }
    if SKIP_DIRS.iter().any(|d| root.ends_with(d) || root.contains(&format!("/{}/", d))) { return; }
    for entry in list_dir(root) {
        if files.len() >= max_files { return; }
        let next = if root.ends_with('/') { format!("{}{}", root, entry) } else { format!("{}/{}", root, entry) };
        if entry.contains('.') {
            files.push(next);
        } else {
            walk_posix(&next, max_files, files);
        }
    }
}

fn extract_chunks(path: &str, source: &str, lang: Language) -> Vec<(String, String, usize, usize, String)> {
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() { return Vec::new(); }
    let tree = match parser.parse(source, None) { Some(t) => t, None => return Vec::new() };
    let mut out = Vec::new();
    let src_bytes = source.as_bytes();
    let mut cursor = tree.walk();
    let mut stack: Vec<tree_sitter::Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if CHUNK_NODE_TYPES.contains(&kind) {
            let start = node.start_byte();
            let end = node.end_byte().min(src_bytes.len());
            if end > start {
                let body = String::from_utf8_lossy(&src_bytes[start..end]).into_owned();
                let line_start = node.start_position().row + 1;
                let line_end = node.end_position().row + 1;
                let name = node
                    .child_by_field_name("name")
                    .map(|n| String::from_utf8_lossy(&src_bytes[n.start_byte()..n.end_byte().min(src_bytes.len())]).into_owned())
                    .unwrap_or_default();
                out.push((kind.to_string(), name, line_start, line_end, body));
                let _ = cursor;
                continue;
            }
        }
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) { stack.push(c); }
        }
    }
    out
}

fn embed_text(text: &str) -> Option<Vec<f32>> {
    crate::embed::embed_text(text)
}

fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    if let Value::Array(arr) = v {
        let mut out = Vec::with_capacity(arr.len());
        for x in arr { if let Some(f) = x.as_f64() { out.push(f as f32); } }
        if !out.is_empty() { return Some(out); }
    }
    None
}

fn vec_to_json_literal(v: &[f32]) -> String {
    let mut s = String::from("[");
    for (i, f) in v.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str(&format!("{:.6}", f));
    }
    s.push(']');
    s
}

fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

pub fn index(root: &str, max_files: usize) -> Value {
    if let Err(e) = ensure_schema() { return json!({ "ok": false, "error": e }); }
    let r = if root.is_empty() { "/" } else { root };
    let limit = max_files.max(50).min(2000);
    let files = collect_files(r, limit);
    let mut indexed = 0;
    let mut chunked = 0;
    let mut embedded = 0;
    let mut skipped_no_embed = 0u32;
    let mut langs = std::collections::BTreeMap::<String, u32>::new();
    for fp in &files {
        let dot = fp.rfind('.');
        let ext = match dot { Some(i) => &fp[i..], None => "" };
        let (lang_name, lang) = match lang_for_ext(ext) { Some(x) => x, None => continue };
        let read_path = if fp.starts_with('/') { fp.clone() } else { format!("/{}", fp) };
        let content = match host_read(&read_path) { Some(c) => c, None => continue };
        if content.len() > 256 * 1024 { continue; }
        indexed += 1;
        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
        let chunks = extract_chunks(fp, &content, lang);
        for (kind, name, ls, le, body) in chunks {
            let emb_blob = embed_text(&format!("{} {}", name, &body[..body.len().min(512)]));
            let v = match emb_blob {
                Some(v) => v,
                None => {
                    skipped_no_embed += 1;
                    let msg = format!("code_index: embed failed for {}:{} ({}); skipping chunk to avoid NULL-embedding row", fp, ls, name);
                    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
                    continue;
                }
            };
            chunked += 1;
            embedded += 1;
            let embedding_sql = format!("vector('{}')", vec_to_json_literal(&v));
            let sql = format!(
                "INSERT INTO code_chunks(path, kind, name, line_start, line_end, body, embedding) VALUES('{}','{}','{}',{},{},'{}',{})",
                sql_quote(fp), sql_quote(&kind), sql_quote(&name), ls, le, sql_quote(&body[..body.len().min(8192)]), embedding_sql
            );
            let _ = libsql_wasm::exec(GM_DB, &sql);
            // Also write the chunk to the file-vec store the `codesearch` verb actually reads
            // (host_vec_search over namespace `codeinsight` / `codeinsight-vec`). The libsql insert
            // above feeds the `_libsql` codesearch variant; this dual-write feeds the primary
            // file-vec codesearch, mirroring how memorize_with_raw writes text + `-vec` embedding.
            let key = format!("ci-{}-{}-{}", unsafe { host_now_ms() }, ls, chunked);
            let text = format!("{}:{}:{} {}\n{}", fp, ls, le, name, &body[..body.len().min(8192)]);
            fv_put("codeinsight", &key, &text);
            let emb_json = serde_json::json!({ "embedding": v }).to_string();
            fv_put("codeinsight-vec", &key, &emb_json);
        }
    }
    json!({
        "ok": true,
        "files_scanned": files.len(),
        "files_indexed": indexed,
        "chunks": chunked,
        "embedded": embedded,
        "skipped_no_embed": skipped_no_embed,
        "by_language": langs,
    })
}

pub fn search(query: &str, k: usize, inline_embedding: Option<&Value>) -> Value {
    if let Err(e) = ensure_schema() { return json!({ "ok": false, "error": e }); }
    let qvec = match inline_embedding.and_then(json_to_f32_vec).or_else(|| embed_text(query)) {
        Some(v) => v,
        None => {
            let like = format!("%{}%", sql_quote(query));
            let sql = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM code_chunks WHERE body LIKE '{}' OR name LIKE '{}' LIMIT {}", like, like, k);
            return match libsql_wasm::query(GM_DB, &sql) {
                Ok(rows) => json!({ "ok": true, "mode": "fallback_like", "rows": rows }),
                Err(e) => json!({ "ok": false, "mode": "fallback_like", "error": e }),
            };
        }
    };
    let qlit = vec_to_json_literal(&qvec);
    let pool = k.saturating_mul(5).max(20);
    let sql = format!(
        "SELECT c.path, c.kind, c.name, c.line_start, c.line_end, substr(c.body,1,400) AS snippet, vector_distance_cos(c.embedding, vector('{}')) AS distance FROM vector_top_k('code_chunks_vec', vector('{}'), {}) AS v JOIN code_chunks AS c ON c.rowid = v.id ORDER BY distance ASC LIMIT {}",
        qlit, qlit, pool, k
    );
    match libsql_wasm::query(GM_DB, &sql) {
        Ok(rows) => json!({ "ok": true, "mode": "vector_top_k", "rows": rows }),
        Err(e) => {
            let like = format!("%{}%", sql_quote(query));
            let sql2 = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM code_chunks WHERE body LIKE '{}' OR name LIKE '{}' LIMIT {}", like, like, k);
            match libsql_wasm::query(GM_DB, &sql2) {
                Ok(rows) => json!({ "ok": true, "mode": "fallback_like_after_vec_err", "vec_err": e, "rows": rows }),
                Err(e2) => json!({ "ok": false, "vec_err": e, "fallback_err": e2 }),
            }
        }
    }
}

pub fn memorize(text: &str, namespace: &str, inline_embedding: Option<&Value>) -> Value {
    memorize_at(text, namespace, inline_embedding, None)
}

pub fn memorize_at(text: &str, namespace: &str, inline_embedding: Option<&Value>, project_path: Option<&str>) -> Value {
    if inline_embedding.is_none() && crate::pipeline::needs_summarize(text) {
        if let Err(e) = ensure_schema_for(project_path) {
            return json!({ "ok": false, "error": e });
        }
        return crate::pipeline::build_pending_step(text, namespace, project_path);
    }
    memorize_at_finalize(text, text, namespace, inline_embedding, project_path)
}

pub fn memorize_at_finalize(embed_source: &str, stored_text: &str, namespace: &str, inline_embedding: Option<&Value>, project_path: Option<&str>) -> Value {
    let db_name = match ensure_schema_for(project_path) {
        Ok(n) => n,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let emb = inline_embedding.and_then(json_to_f32_vec).or_else(|| embed_text(embed_source));
    let v = match emb {
        Some(v) => v,
        None => {
            let msg = format!("memorize_at: embed_text failed for namespace={}; refusing to insert row with NULL embedding", namespace);
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            return json!({ "ok": false, "error": msg });
        }
    };
    let embedding_sql = format!("vector('{}')", vec_to_json_literal(&v));
    let sql = format!(
        "INSERT INTO memories(namespace, text, ts, embedding) VALUES('{}','{}',{},{})",
        sql_quote(namespace), sql_quote(stored_text), unsafe { crate::wasm_dispatch::host_now_ms() }, embedding_sql
    );
    match libsql_wasm::exec(&db_name, &sql) {
        Ok(()) => json!({ "ok": true, "memorized": true, "embedded": true, "inline": inline_embedding.is_some(), "project_path": project_path }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

pub fn recall(query: &str, limit: usize, namespace: Option<&str>, inline_embedding: Option<&Value>) -> Value {
    recall_at(query, limit, namespace, inline_embedding, None)
}

pub fn recall_at(query: &str, limit: usize, namespace: Option<&str>, inline_embedding: Option<&Value>, project_path: Option<&str>) -> Value {
    let db_name = match ensure_schema_for(project_path) {
        Ok(n) => n,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let qvec = match inline_embedding.and_then(json_to_f32_vec).or_else(|| embed_text(query)) {
        Some(v) => v,
        None => {
            let like = format!("%{}%", sql_quote(query));
            let ns_filter = match namespace { Some(n) => format!(" AND namespace='{}'", sql_quote(n)), None => String::new() };
            let sql = format!("SELECT id, namespace, text, ts FROM memories WHERE text LIKE '{}'{} ORDER BY ts DESC LIMIT {}", like, ns_filter, limit);
            return match libsql_wasm::query(&db_name, &sql) {
                Ok(rows) => json!({ "ok": true, "mode": "fallback_like", "rows": rows, "project_path": project_path }),
                Err(e) => json!({ "ok": false, "error": e }),
            };
        }
    };
    let qlit = vec_to_json_literal(&qvec);
    let sql = match namespace {
        Some(n) => format!(
            "SELECT id, namespace, text, ts, vector_distance_cos(embedding, vector('{}')) AS distance FROM memories WHERE namespace='{}' AND embedding IS NOT NULL ORDER BY distance ASC LIMIT {}",
            qlit, sql_quote(n), limit
        ),
        None => format!(
            "SELECT m.id, m.namespace, m.text, m.ts, vector_distance_cos(m.embedding, vector('{}')) AS distance FROM vector_top_k('memories_vec', vector('{}'), {}) AS v JOIN memories AS m ON m.rowid = v.id ORDER BY distance ASC LIMIT {}",
            qlit, qlit, limit.saturating_mul(5).max(20), limit
        ),
    };
    match libsql_wasm::query(&db_name, &sql) {
        Ok(rows) => json!({ "ok": true, "mode": "vector_top_k", "rows": rows, "project_path": project_path }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}
