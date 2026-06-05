#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use tree_sitter::{Language, Parser};

use crate::libsql_wasm;
use crate::wasm_dispatch::{host_read, unpack_to_value_pub};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
}

fn fv_put(ns: &str, key: &str, val: &str) {
    let _ = unsafe { host_kv_put(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
}

fn fv_query(ns: &str, q: &str) -> Value {
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, q.as_ptr(), q.len() as u32) };
    unpack_to_value_pub(packed)
}

fn fv_delete(ns: &str, key: &str) {
    let _ = unsafe { host_kv_delete(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
}

fn entry_embed_dim(entry_value: &str) -> Option<usize> {
    let parsed: Value = serde_json::from_str(entry_value).ok()?;
    let arr = parsed.get("embedding").and_then(|e| e.as_array())?;
    Some(arr.len())
}

pub fn clear_codeinsight() -> u32 {
    let mut cleared = 0u32;
    let data_rows = fv_query("codeinsight", "");
    if let Some(arr) = data_rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete("codeinsight", key);
                cleared += 1;
            }
        }
    }
    let vec_rows = fv_query("codeinsight-vec", "");
    if let Some(arr) = vec_rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete("codeinsight-vec", key);
            }
        }
    }
    cleared
}

// Like clear_codeinsight() but also drops the per-file manifests, forcing the next index() to
// re-embed everything from scratch (no incremental reuse). Used by the explicit `rebuild:true`
// codesearch path where the caller wants a guaranteed clean rebuild.
pub fn clear_codeinsight_full() -> u32 {
    let cleared = clear_codeinsight();
    let rows = fv_query("codeinsight-manifest", "");
    if let Some(arr) = rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete("codeinsight-manifest", key);
            }
        }
    }
    cleared
}

fn clear_codeinsight_if_dim_mismatch() -> bool {
    let vec_rows = fv_query("codeinsight-vec", "");
    let rows = match vec_rows.as_array() {
        Some(r) if !r.is_empty() => r,
        _ => return false,
    };
    let mut existing_dim: Option<usize> = None;
    for row in rows {
        if let Some(val) = row.get("value").and_then(|v| v.as_str()) {
            if let Some(d) = entry_embed_dim(val) {
                existing_dim = Some(d);
                break;
            }
        }
    }
    let old_dim = match existing_dim {
        Some(d) if d != EXPECTED_EMBED_DIM => d,
        _ => return false,
    };
    let cleared = clear_codeinsight();
    crate::wasm_dispatch::emit_event("codeinsight_namespace_cleared", serde_json::json!({
        "reason": "embed_dim_mismatch",
        "old_dim": old_dim,
        "new_dim": EXPECTED_EMBED_DIM,
        "keys_cleared": cleared,
    }));
    let msg = format!("code_index: codeinsight namespace cleared on dim mismatch old={} new={} keys={}", old_dim, EXPECTED_EMBED_DIM, cleared);
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    true
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
    ".gm",
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
        // The host returns either bare strings OR rich entry objects {name, is_dir, is_file}
        // (the current gm-plugkit wrapper emits objects). Accept both: a bare string is the name;
        // an object's name is under name/path/file. Previously only bare strings were handled, so
        // an object-returning host yielded an empty list and code_index found files=0, leaving
        // codesearch's autobuild index empty.
        Value::Array(arr) => arr.into_iter().filter_map(|x| {
            if let Some(s) = x.as_str() { return Some(s.to_string()); }
            x.get("name").or_else(|| x.get("path")).or_else(|| x.get("file"))
                .and_then(|n| n.as_str()).map(String::from)
        }).collect(),
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

const MANIFEST_NS: &str = "codeinsight-manifest";

// A persisted per-file index manifest: the file's content hash plus the full set
// of chunk records (text + embedding) that were written for it. Storing the
// embeddings here lets a re-index reuse them for unchanged files without
// recomputing the 12-layer BERT forward — eliminating the full-rebuild freeze on
// a digest mismatch where almost no file content actually changed.
//
// `key` is the deterministic file-vec store key (`ci-{filehash}-{chunkidx}`),
// stable across runs so the codeinsight / codeinsight-vec namespaces end in the
// same shape they'd have after a full rebuild.
#[derive(Clone)]
struct ChunkRecord {
    key: String,
    kind: String,
    name: String,
    ls: usize,
    le: usize,
    body: String,
    text: String,
    emb: Vec<f32>,
}

struct FileManifest {
    hash: u32,
    chunks: Vec<ChunkRecord>,
}

fn manifest_to_json(hash: u32, chunks: &[ChunkRecord]) -> String {
    let arr: Vec<Value> = chunks.iter().map(|c| json!({
        "key": c.key,
        "kind": c.kind,
        "name": c.name,
        "ls": c.ls,
        "le": c.le,
        "body": c.body,
        "text": c.text,
        "emb": c.emb,
    })).collect();
    json!({ "hash": hash, "chunks": arr }).to_string()
}

fn parse_manifest(val: &str) -> Option<FileManifest> {
    let parsed: Value = serde_json::from_str(val).ok()?;
    let hash = parsed.get("hash").and_then(|h| h.as_u64())? as u32;
    let arr = parsed.get("chunks").and_then(|c| c.as_array())?;
    let mut chunks = Vec::with_capacity(arr.len());
    for c in arr {
        let key = c.get("key").and_then(|x| x.as_str())?.to_string();
        let kind = c.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ls = c.get("ls").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let le = c.get("le").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let body = c.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let text = c.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let emb = json_to_f32_vec(c.get("emb")?)?;
        chunks.push(ChunkRecord { key, kind, name, ls, le, body, text, emb });
    }
    Some(FileManifest { hash, chunks })
}

// Load every persisted per-file manifest into a path->manifest map. Keyed by the
// file path so an incremental re-index can look up "did this file's content
// change?" in O(1) and reuse the stored chunk embeddings when it didn't.
fn load_manifests() -> std::collections::HashMap<String, FileManifest> {
    let mut out = std::collections::HashMap::new();
    let rows = fv_query(MANIFEST_NS, "");
    if let Some(arr) = rows.as_array() {
        for row in arr {
            let key = match row.get("key").and_then(|k| k.as_str()) { Some(k) => k, None => continue };
            let val = match row.get("value").and_then(|v| v.as_str()) { Some(v) => v, None => continue };
            if let Some(m) = parse_manifest(val) {
                out.insert(key.to_string(), m);
            }
        }
    }
    out
}

// Write one chunk record into the served file-vec stores (the namespaces
// `codesearch`/`recall` read), plus the best-effort libsql vector table. Used for
// both freshly-embedded chunks and reused (unchanged-file) chunks so the live
// index ends in identical shape regardless of which path produced the record.
fn write_chunk(libsql_ok: bool, fp: &str, c: &ChunkRecord) {
    if libsql_ok {
        let embedding_sql = format!("vector('{}')", vec_to_json_literal(&c.emb));
        let sql = format!(
            "INSERT INTO code_chunks(path, kind, name, line_start, line_end, body, embedding) VALUES('{}','{}','{}',{},{},'{}',{})",
            sql_quote(fp), sql_quote(&c.kind), sql_quote(&c.name), c.ls, c.le, sql_quote(&c.body[..c.body.len().min(8192)]), embedding_sql
        );
        let _ = libsql_wasm::exec(GM_DB, &sql);
    }
    fv_put("codeinsight", &c.key, &c.text);
    let emb_json = serde_json::json!({ "embedding": c.emb }).to_string();
    fv_put("codeinsight-vec", &c.key, &emb_json);
}

fn delete_chunk_keys(chunks: &[ChunkRecord]) {
    for c in chunks {
        fv_delete("codeinsight", &c.key);
        fv_delete("codeinsight-vec", &c.key);
    }
}

pub fn index(root: &str, max_files: usize) -> Value {
    // libsql schema init is best-effort: the file-vec store (host_kv_put) is the live codesearch
    // backend and must populate even when libsql/wasi schema init fails. Previously a failed
    // ensure_schema() returned early before any file-vec write, so codesearch's autobuild produced
    // an empty index silently. Decouple: record the libsql availability, keep going regardless.
    let libsql_ok = ensure_schema().is_ok();
    // A dimension mismatch in the persisted vectors means EVERY stored embedding is unusable, so the
    // incremental reuse path must not run — wipe both the served namespaces and the manifests so the
    // rebuild below re-embeds from scratch at the correct dim.
    let kvvec_cleared = clear_codeinsight_if_dim_mismatch();
    if kvvec_cleared {
        let rows = fv_query(MANIFEST_NS, "");
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|k| k.as_str()) { fv_delete(MANIFEST_NS, k); }
            }
        }
    }
    // INCREMENTAL re-index (was: a full clear_codeinsight() + re-embed of every chunk on ANY digest
    // mismatch, which froze the watcher for 5+ minutes recomputing 2500-5000 BERT forwards even when
    // a single new commit changed almost no file content). Reuse persisted embeddings for files whose
    // content hash is unchanged; only re-embed new/changed files; drop chunks for files that vanished.
    let prior = load_manifests();
    let r = if root.is_empty() { "/" } else { root };
    let limit = max_files.max(50).min(2000);
    let files = collect_files(r, limit);
    {
        let msg = format!("code_index: indexing root={} files={} libsql_ok={} manifests={}", r, files.len(), libsql_ok, prior.len());
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let mut indexed = 0;
    let mut chunked = 0;
    let mut embedded = 0;
    let mut reused = 0;
    let mut reused_files = 0;
    let mut skipped_no_embed = 0u32;
    let mut langs = std::collections::BTreeMap::<String, u32>::new();
    // Track which prior-manifest paths we still saw this run; anything left over is a deleted file
    // whose stored chunks + manifest must be removed so the index matches a full rebuild's shape.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for fp in &files {
        let dot = fp.rfind('.');
        let ext = match dot { Some(i) => &fp[i..], None => "" };
        let (lang_name, lang) = match lang_for_ext(ext) { Some(x) => x, None => continue };
        // host_fs_read does fs.readFileSync(path) relative to the watcher cwd. collect_files
        // yields cwd-relative paths (e.g. ./src/lib.rs or src/lib.rs). Prepending '/' made them
        // filesystem-absolute (/./src/lib.rs -> /src/lib.rs) which does not exist, so every read
        // failed and the loop indexed zero chunks despite files=101. Try the path as-is first
        // (relative, the common case), then a '/'-prefixed absolute as fallback for hosts that
        // require it.
        let rel = fp.trim_start_matches("./").to_string();
        let content = match host_read(&rel)
            .or_else(|| host_read(fp))
            .or_else(|| host_read(&format!("/{}", rel)))
        { Some(c) => c, None => continue };
        if content.len() > 256 * 1024 { continue; }
        seen.insert(fp.clone());
        indexed += 1;
        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
        let file_hash = crc32(&content);

        // FAST PATH: file content unchanged since last index -> reuse its persisted chunk embeddings.
        // No embed_text() call, so no BERT forward, so no freeze. Re-emit the served KV entries and
        // the (per-process, in-memory) libsql rows so the index ends in full-rebuild shape.
        if let Some(m) = prior.get(fp) {
            if m.hash == file_hash {
                for c in &m.chunks {
                    write_chunk(libsql_ok, fp, c);
                    chunked += 1;
                    reused += 1;
                }
                reused_files += 1;
                continue;
            }
            // Content changed: drop the stale chunk keys before re-embedding to avoid leftovers.
            delete_chunk_keys(&m.chunks);
        }

        // SLOW PATH: new or changed file -> re-embed its chunks.
        let chunks = extract_chunks(fp, &content, lang);
        let mut records: Vec<ChunkRecord> = Vec::new();
        for (idx, (kind, name, ls, le, body)) in chunks.into_iter().enumerate() {
            // Truncate body to <=512 bytes at a char boundary: a hard byte slice
            // panics when a multibyte char (e.g. an emoji) straddles byte 512,
            // which aborts the whole index rebuild and wedges the watcher.
            let body_head = {
                let mut e = body.len().min(512);
                while e > 0 && !body.is_char_boundary(e) { e -= 1; }
                &body[..e]
            };
            let emb_blob = embed_text(&format!("{} {}", name, body_head));
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
            // Deterministic per-file key (was `ci-{ms}-{ls}-{n}` with a timestamp, which made keys
            // non-reusable across runs). `ci-{filehash}-{chunkidx}` is stable so the manifest's
            // recorded keys still address the same rows on the next incremental pass.
            let key = format!("ci-{:x}-{}", file_hash, idx);
            let text = format!("{}:{}:{} {}\n{}", fp, ls, le, name, &body[..body.len().min(8192)]);
            let rec = ChunkRecord { key, kind, name, ls, le, body, text, emb: v };
            write_chunk(libsql_ok, fp, &rec);
            records.push(rec);
        }
        // Persist the per-file manifest (hash + full chunk records) so the next index can reuse it.
        fv_put(MANIFEST_NS, fp, &manifest_to_json(file_hash, &records));
    }

    // Reap files that disappeared since the last index: remove their served chunks + manifest.
    let mut removed_files = 0;
    for (fp, m) in &prior {
        if !seen.contains(fp) {
            delete_chunk_keys(&m.chunks);
            fv_delete(MANIFEST_NS, fp);
            removed_files += 1;
        }
    }
    // Stamp the tree digest the index was built against so codesearch can detect staleness
    // (sync-before-emit) and rebuild only on mismatch rather than serving an index from a stale tree.
    let digest = current_digest();
    store_digest(&digest);
    {
        let msg = format!("code_index: done files_indexed={} chunks={} embedded={} reused={} reused_files={} removed_files={} skipped_no_embed={} digest={}", indexed, chunked, embedded, reused, reused_files, removed_files, skipped_no_embed, digest);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    json!({
        "ok": true,
        "files_scanned": files.len(),
        "files_indexed": indexed,
        "chunks": chunked,
        "embedded": embedded,
        "reused": reused,
        "reused_files": reused_files,
        "removed_files": removed_files,
        "skipped_no_embed": skipped_no_embed,
        "kvvec_cleared_dim_mismatch": kvvec_cleared,
        "by_language": langs,
    })
}

fn porcelain_path(line: &str) -> &str {
    let rest = if line.len() > 3 { &line[3..] } else { line.trim_start() };
    match rest.rfind(" -> ") {
        Some(i) => &rest[i + 4..],
        None => rest,
    }
}

fn skipped_path(path: &str) -> bool {
    let p = path.trim_matches('"');
    p.split('/').any(|seg| SKIP_DIRS.iter().any(|d| seg == *d))
}

fn indexable_porcelain(porcelain: &str) -> String {
    porcelain
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !skipped_path(porcelain_path(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn current_digest() -> String {
    let head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let porcelain = crate::wasm_dispatch::git_porcelain();
    let dirty = crc32(&indexable_porcelain(&porcelain));
    format!("{}:{}", head, dirty)
}

pub fn stored_digest() -> Option<String> {
    crate::wasm_dispatch::host_kv_read("codeinsight", "__digest__")
}

pub fn store_digest(digest: &str) {
    fv_put("codeinsight", "__digest__", digest);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_path_extracts_path() {
        assert_eq!(porcelain_path(" M src/lib.rs"), "src/lib.rs");
        assert_eq!(porcelain_path("?? new.txt"), "new.txt");
        assert_eq!(porcelain_path("R  old.rs -> new.rs"), "new.rs");
    }

    #[test]
    fn skipped_path_matches_gm_and_skip_dirs() {
        assert!(skipped_path(".gm/disciplines/default/mem-1.json"));
        assert!(skipped_path(".gm/exec-spool/.status.json"));
        assert!(skipped_path("node_modules/x/index.js"));
        assert!(skipped_path("target/debug/foo"));
    }

    #[test]
    fn skipped_path_keeps_real_source() {
        assert!(!skipped_path("src/code_index.rs"));
        assert!(!skipped_path("lib/skill-bootstrap.js"));
        assert!(!skipped_path("docs/paper.html"));
        assert!(!skipped_path("gm.json"));
    }

    #[test]
    fn indexable_porcelain_drops_gm_keeps_source() {
        let raw = " M src/lib.rs\n?? .gm/disciplines/default/mem-1.json\n M .gm/prd.yml\n M lib/x.js";
        let filtered = indexable_porcelain(raw);
        assert!(filtered.contains("src/lib.rs"));
        assert!(filtered.contains("lib/x.js"));
        assert!(!filtered.contains(".gm/"));
    }

    #[test]
    fn indexable_porcelain_source_edit_still_changes_hash() {
        let before = indexable_porcelain("?? .gm/exec-spool/1.txt");
        let after = indexable_porcelain("?? .gm/exec-spool/1.txt\n M src/lib.rs");
        assert_ne!(crc32(&before), crc32(&after));
    }

    #[test]
    fn indexable_porcelain_gm_only_churn_is_stable() {
        let a = indexable_porcelain(" M .gm/prd.yml\n?? .gm/disciplines/default/mem-1.json");
        let b = indexable_porcelain(" M .gm/prd.yml\n?? .gm/disciplines/default/mem-2.json\n M .gm/exec-spool/.status.json");
        assert_eq!(crc32(&a), crc32(&b));
    }

    #[test]
    fn indexable_porcelain_empty_is_consistent() {
        assert_eq!(crc32(&indexable_porcelain("")), crc32(&indexable_porcelain("")));
        assert_eq!(indexable_porcelain(""), "");
    }
}
