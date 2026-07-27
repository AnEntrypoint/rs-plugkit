#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::wasm_dispatch::{host_read, host_stat, unpack_to_value_pub};
use crate::vecstore::{drop_if_dim_mismatch_at_cfg as drop_if_dim_mismatch_cfg, vec_to_json_literal};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
    fn host_plugin_call(plugin_ptr: *const u8, plugin_len: u32, verb_ptr: *const u8, verb_len: u32, body_ptr: *const u8, body_len: u32) -> u64;
}

fn call_out_of_process_plugin(plugin: &str, verb: &str, body: &Value) -> Value {
    let body_s = body.to_string();
    let packed = unsafe {
        host_plugin_call(
            plugin.as_ptr(), plugin.len() as u32,
            verb.as_ptr(), verb.len() as u32,
            body_s.as_ptr(), body_s.len() as u32,
        )
    };
    unpack_to_value_pub(packed)
}

fn plugin_ok(resp: &Value) -> bool {
    resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

use crate::libsql_wasm;

fn fv_put(ns: &str, key: &str, val: &str) -> bool {
    let rc = unsafe { host_kv_put(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
    let succeeded = rc != 0;
    if !succeeded {
        crate::wasm_dispatch::emit_event("codeinsight_kv_put_failed", json!({
            "namespace": ns,
            "key": key,
        }));
    }
    succeeded
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
    clear_codeinsight_cfg(&crate::ragconfig::RagConfig::default())
}

pub fn clear_codeinsight_cfg(cfg: &crate::ragconfig::RagConfig) -> u32 {
    let code_ns = &cfg.namespaces.code;
    let vec_ns = cfg.namespaces.vec_namespace(code_ns);
    let mut cleared = 0u32;
    let data_rows = fv_query(code_ns, "");
    if let Some(arr) = data_rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete(code_ns, key);
                cleared += 1;
            }
        }
    }
    let vec_rows = fv_query(&vec_ns, "");
    if let Some(arr) = vec_rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete(&vec_ns, key);
            }
        }
    }
    cleared
}

pub fn clear_codeinsight_full() -> u32 {
    clear_codeinsight_full_cfg(&crate::ragconfig::RagConfig::default())
}

pub fn clear_codeinsight_full_cfg(cfg: &crate::ragconfig::RagConfig) -> u32 {
    let cleared = clear_codeinsight_cfg(cfg);
    let manifest_ns = cfg.namespaces.manifest_namespace();
    let rows = fv_query(&manifest_ns, "");
    if let Some(arr) = rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete(&manifest_ns, key);
            }
        }
    }
    let db_path = project_db_path(None);
    // Clears rows, not the table: the schema (and its ANN index) is correct at
    // the configured width here -- only a DIM change warrants a DROP, which
    // ensure_schema_at_cfg's guard owns.
    let _ = libsql_wasm::exec(&db_path, &format!("DELETE FROM {}", cfg.code_chunks.table));
    cleared
}

fn clear_codeinsight_if_dim_mismatch() -> bool {
    clear_codeinsight_if_dim_mismatch_cfg(&crate::ragconfig::RagConfig::default())
}

/// Flat-JSON sibling of the libsql-table dim guard.
///
/// The `<code>-vec` kv namespace stores embeddings as JSON arrays, so there is
/// no `F32_BLOB(n)` column type to inspect -- the width has to be read off an
/// actual stored entry. The DECISION is still `EmbedDimConfig::should_drop`,
/// so an operator who set `drop_on_mismatch=false` gets the same
/// diagnose-don't-destroy behaviour on both storage shapes rather than having
/// one of them quietly wipe the namespace anyway.
fn clear_codeinsight_if_dim_mismatch_cfg(cfg: &crate::ragconfig::RagConfig) -> bool {
    let vec_ns = cfg.namespaces.vec_namespace(&cfg.namespaces.code);
    let vec_rows = fv_query(&vec_ns, "");
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
        Some(d) => d,
        // Every entry failed to parse a width; nothing trustworthy to compare
        // against, so leave the namespace alone rather than clearing on a guess.
        None => return false,
    };
    if !cfg.embed.should_drop(&vec_ns, old_dim) {
        return false;
    }
    let cleared = clear_codeinsight_cfg(cfg);
    crate::wasm_dispatch::emit_event("codeinsight_namespace_cleared", serde_json::json!({
        "reason": "embed_dim_mismatch",
        "old_dim": old_dim,
        "new_dim": cfg.dim(),
        "keys_cleared": cleared,
    }));
    let msg = format!("code_index: {} namespace cleared on dim mismatch old={} new={} keys={}", cfg.namespaces.code, old_dim, cfg.dim(), cleared);
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    true
}

fn lang_for_ext(ext: &str) -> Option<&'static str> {
    let e = ext.to_lowercase();
    match e.as_str() {
        ".js" | ".mjs" | ".jsx" => Some("javascript"),
        ".ts" => Some("typescript"),
        ".tsx" => Some("tsx"),
        ".py" => Some("python"),
        ".rs" => Some("rust"),
        ".go" => Some("go"),
        ".c" | ".h" => Some("c"),
        ".cpp" | ".cc" | ".hpp" | ".hh" | ".cxx" => Some("cpp"),
        ".glsl" | ".vert" | ".frag" | ".comp" | ".geom" | ".tesc" | ".tese" | ".vsh" | ".fsh" | ".glslv" | ".glslf" => Some("c"),
        ".java" => Some("java"),
        ".json" => Some("json"),
        ".html" | ".htm" => Some("html"),
        ".css" => Some("css"),
        ".sh" | ".bash" => Some("bash"),
        ".md" | ".markdown" => Some("markdown"),
        ".ps1" | ".psm1" | ".psd1" => Some("powershell"),
        ".rb" => Some("ruby"),
        ".cs" => Some("csharp"),
        ".php" | ".phtml" => Some("php"),
        ".hs" | ".lhs" => Some("haskell"),
        ".jl" => Some("julia"),
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
    "section",
];

const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", ".hg", ".bzr", "CVS", ".gm",
    "node_modules", ".npm", ".yarn", ".pnp", ".next", ".nuxt", "dist", "out",
    "build", ".cache", ".parcel-cache", ".vite", ".turbo", ".nx", ".rush",
    ".lerna", ".pnpm-store", ".docusaurus", ".vuepress",
    "__pycache__", ".pytest_cache", ".mypy_cache", ".hypothesis", ".pyre",
    ".pytype", "env", "venv", "ENV", ".venv", ".tox", "htmlcov", "site-packages",
    "target",
    "vendor",
    ".gradle", ".mvn", "bin", "obj",
    ".bundle",
    "Pods", "DerivedData",
    ".terraform", ".serverless",
    ".docker",
    ".llamaindex", ".chroma", ".vectorstore", ".embeddings", ".langchain",
    "embeddings", "vector-db", "faiss-index", "chromadb",
    ".claude", ".wfgy", ".kilo", ".agents", ".code-search",
    ".plugkit-browser-profile-default", ".plugkit-agent-worktree",
    ".test-chrome-profile",
    ".vscode", ".idea", ".vs", ".sublime-text", ".cursor", ".windsurf",
    ".zed", ".helix",
    "coverage", ".nyc_output", "test-results", "playwright-report",
    ".plugkit-browser-profile",
    "_site", "public", "static", "site", "output", "builds", "artifacts",
    "compiled", "generated", "gen",
    "Carthage", "fastlane",
    "mlruns", "wandb", "weights",
    ".cargo", ".rustup", ".rbenv", ".rvm", ".nvm", ".pyenv", ".conda",
    ".m2", ".sbt", ".ivy2", ".gem",
];

const SKIP_FILE_SUFFIXES: &[&str] = &[
    ".min.js", ".min.css", ".bundle.js", ".chunk.js", ".map",
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb",
    "bun.lock", "Cargo.lock", "composer.lock", "Gemfile.lock", "poetry.lock",
    "Pipfile.lock", "go.sum", "uv.lock",
    ".codeinsight", ".codeinsight.digest", ".perf-baseline.json",
    ".rs-exec.lock",
    ".glb", ".gltf", ".vrm", ".fbx", ".blend", ".blend1", ".usdz", ".hf",
    ".uasset", ".umap",
    ".wasm", ".exe", ".dll", ".dylib", ".so", ".o", ".obj", ".a", ".lib",
    ".pdb", ".class", ".jar", ".war", ".ear", ".apk", ".aab", ".ipa",
    ".hex", ".elf", ".uf2", ".dfu",
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".bmp", ".webp", ".tiff",
    ".pdf", ".mov", ".mp4", ".avi", ".flv", ".mkv", ".webm", ".mp3",
    ".m4a", ".wav", ".flac", ".ogg", ".woff", ".woff2", ".ttf", ".otf",
    ".eot", ".zip", ".tar", ".tar.gz", ".tgz", ".rar", ".7z", ".iso",
    ".bz2", ".xz", ".lz4", ".zst", ".cab", ".deb", ".rpm", ".dmg", ".msi",
    ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx",
    ".psd", ".ai", ".sketch", ".aep",
    ".pkl", ".pickle", ".h5", ".hdf5", ".parquet", ".npy", ".npz",
    ".safetensors", ".ckpt", ".pt", ".pth", ".onnx", ".gguf",
    "tokenizer.json", "vocab.json", "vocab.txt", "merges.txt",
    "-tokenizer.json", "-vocab.json",
    ".stackdump", ".dmp", ".core",
    ".key", ".pem", ".p12", ".pfx", ".p8", ".crt", ".cer", ".der",
    "credentials.json", "secrets.yaml", "secrets.yml",
    ".db", ".sqlite", ".sqlite3",
];

fn is_skipped_filename(name: &str) -> bool {
    SKIP_FILE_SUFFIXES.iter().any(|suf| name.ends_with(suf))
}

pub fn ensure_schema_at(path: &str) -> Result<(), String> {
    ensure_schema_at_cfg(path, &crate::ragconfig::RagConfig::default())
}

pub fn ensure_schema_at_cfg(path: &str, cfg: &crate::ragconfig::RagConfig) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    libsql_wasm::open(path)?;
    // Both CREATEs previously spelled the width as a literal 384 while the
    // guards above them compared against EXPECTED_EMBED_DIM -- two independent
    // sources of truth for the same number, one of which nothing would have
    // updated on a dim change. Both now read `cfg.dim()`, and the guard runs
    // first so a changed dim actually drops the old-width table (a
    // `CREATE TABLE IF NOT EXISTS` at the new width against a surviving table
    // is a silent no-op, leaving the store queryable only at the old width).
    let _ = drop_if_dim_mismatch_cfg(path, &cfg.code_chunks.table, &cfg.embed);
    let _ = drop_if_dim_mismatch_cfg(path, &cfg.memories.table, &cfg.embed);
    libsql_wasm::exec(path, &format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, body TEXT, embedding F32_BLOB({}))",
        cfg.code_chunks.table, cfg.dim()
    ))?;
    libsql_wasm::exec(path, &format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT, text TEXT, ts INTEGER, embedding F32_BLOB({}))",
        cfg.memories.table, cfg.dim()
    ))?;
    crate::vecns::VecTableSpec::from_names(path, &cfg.code_chunks).ensure_index();
    crate::vecns::VecTableSpec::from_names(path, &cfg.memories).ensure_index();
    Ok(())
}

fn project_db_filename(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("ext-{:x}.db", crc32(p)),
        _ => "gm.db".to_string(),
    }
}

pub(crate) fn project_db_path(project_path: Option<&str>) -> String {
    libsql_wasm::absolute_db_path(&project_db_filename(project_path))
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
    ensure_schema_at(&project_db_path(None))
}

fn ensure_schema_for(project_path: Option<&str>) -> Result<String, String> {
    let path = project_db_path(project_path);
    ensure_schema_at(&path)?;
    Ok(path)
}

fn list_dir(path: &str) -> Vec<String> {
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value_pub(packed);
    match v {
        Value::Array(arr) => arr.into_iter().filter_map(|x| {
            if let Some(s) = x.as_str() { return Some(s.to_string()); }
            x.get("name").or_else(|| x.get("path")).or_else(|| x.get("file"))
                .and_then(|n| n.as_str()).map(String::from)
        }).collect(),
        _ => Vec::new(),
    }
}

fn ignore_file_path(root: &str, filename: &str) -> String {
    if root.is_empty() || root == "/" || root == "." {
        filename.to_string()
    } else if root.ends_with('/') {
        format!("{}{}", root, filename)
    } else {
        format!("{}/{}", root, filename)
    }
}

fn load_repo_gitignore(root: &str) -> Option<ignore::gitignore::Gitignore> {
    let gitignore_content = host_read(&ignore_file_path(root, ".gitignore"));
    let custom_content = host_read(&ignore_file_path(root, ".codesearchignore"));
    if gitignore_content.is_none() && custom_content.is_none() { return None; }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for content in [gitignore_content, custom_content].into_iter().flatten() {
        for line in content.lines() {
            let _ = builder.add_line(None, line);
        }
    }
    builder.build().ok()
}

fn gitignore_excludes(gi: &Option<ignore::gitignore::Gitignore>, rel_path: &str, is_dir: bool) -> bool {
    match gi {
        Some(g) => g.matched(rel_path, is_dir).is_ignore(),
        None => false,
    }
}

fn is_hidden_segment(seg: &str) -> bool {
    seg.starts_with('.') && seg != "." && seg != ".."
}

fn collect_files(root: &str, max_files: usize) -> Vec<String> {
    let gi = load_repo_gitignore(root);
    let entries = list_dir(root);
    if entries.is_empty() { return Vec::new(); }
    let has_slashes = entries.iter().any(|e| e.contains('/'));
    if has_slashes {
        return entries.into_iter()
            .filter(|p| !p.split('/').any(is_hidden_segment))
            .filter(|p| !SKIP_DIRS.iter().any(|d| p.split('/').any(|seg| seg == *d)))
            .filter(|p| {
                let name = p.rsplit('/').next().unwrap_or(p.as_str());
                !is_skipped_filename(name)
            })
            .filter(|p| !gitignore_excludes(&gi, p, false))
            .take(max_files)
            .collect();
    }
    let mut files = Vec::new();
    walk_posix(root, max_files, &mut files, &gi);
    files
}

fn walk_posix(root: &str, max_files: usize, files: &mut Vec<String>, gi: &Option<ignore::gitignore::Gitignore>) {
    if files.len() >= max_files { return; }
    if SKIP_DIRS.iter().any(|d| root.ends_with(d) || root.contains(&format!("/{}/", d))) { return; }
    for entry in list_dir(root) {
        if files.len() >= max_files { return; }
        if is_hidden_segment(&entry) { continue; }
        if is_skipped_filename(&entry) { continue; }
        let next = if root.ends_with('/') { format!("{}{}", root, entry) } else { format!("{}/{}", root, entry) };
        let is_dir_entry = host_stat(&next)
            .and_then(|v| v.get("isDirectory").and_then(|b| b.as_bool()))
            .unwrap_or_else(|| !entry.contains('.'));
        if gitignore_excludes(gi, &next, is_dir_entry) { continue; }
        if !is_dir_entry {
            files.push(next);
        } else {
            walk_posix(&next, max_files, files, gi);
        }
    }
}

fn extract_chunks(_path: &str, source: &str, lang_name: &str) -> Vec<(String, String, usize, usize, String)> {
    let resp = call_out_of_process_plugin("treesitter", "parse", &json!({ "lang": lang_name, "source": source }));
    if !plugin_ok(&resp) { return Vec::new(); }
    let nodes = match resp.get("nodes").and_then(|v| v.as_array()) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let src_bytes = source.as_bytes();
    let mut out = Vec::new();
    for node in nodes {
        let kind = match node.get("kind").and_then(|v| v.as_str()) { Some(k) => k, None => continue };
        if !CHUNK_NODE_TYPES.contains(&kind) { continue; }
        let start = node.get("start_byte").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let end = (node.get("end_byte").and_then(|v| v.as_u64()).unwrap_or(0) as usize).min(src_bytes.len());
        if end <= start { continue; }
        let body = String::from_utf8_lossy(&src_bytes[start..end]).into_owned();
        let line_start = node.get("start_row").and_then(|v| v.as_u64()).unwrap_or(0) as usize + 1;
        let line_end = node.get("end_row").and_then(|v| v.as_u64()).unwrap_or(0) as usize + 1;
        let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        out.push((kind.to_string(), name, line_start, line_end, body));
    }
    out
}

const OVERSIZED_CHUNK_SPLIT_THRESHOLD: usize = 8192;
const OVERSIZED_CHUNK_OVERLAP: usize = 800;

fn split_oversized_chunk(
    kind: &str,
    name: &str,
    line_start: usize,
    line_end: usize,
    body: &str,
) -> Vec<(String, String, usize, usize, String)> {
    if body.len() <= OVERSIZED_CHUNK_SPLIT_THRESHOLD {
        return vec![(kind.to_string(), name.to_string(), line_start, line_end, body.to_string())];
    }
    let total_lines = line_end.saturating_sub(line_start).max(1);
    let bytes_per_line = (body.len() as f64 / total_lines as f64).max(1.0);
    let stride = OVERSIZED_CHUNK_SPLIT_THRESHOLD - OVERSIZED_CHUNK_OVERLAP;
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut part = 0usize;
    while start < body.len() {
        let mut end = (start + OVERSIZED_CHUNK_SPLIT_THRESHOLD).min(body.len());
        while end > start && !body.is_char_boundary(end) { end -= 1; }
        let sub_body = &body[start..end];
        let sub_line_start = line_start + ((start as f64 / bytes_per_line) as usize);
        let sub_line_end = line_start + ((end as f64 / bytes_per_line) as usize);
        let sub_name = if part == 0 { name.to_string() } else { format!("{}#part{}", name, part + 1) };
        out.push((kind.to_string(), sub_name, sub_line_start, sub_line_end.max(sub_line_start), sub_body.to_string()));
        if end >= body.len() { break; }
        let mut next_start = end.saturating_sub(OVERSIZED_CHUNK_OVERLAP);
        while next_start > 0 && !body.is_char_boundary(next_start) { next_start -= 1; }
        start = next_start.max(start + stride.min(1));
        part += 1;
    }
    out
}

fn embed_text(text: &str) -> Option<Vec<f32>> {
    let resp = call_out_of_process_plugin("bert", "embed", &json!({ "text": text }));
    if !plugin_ok(&resp) { return None; }
    resp.get("embedding").and_then(json_to_f32_vec)
}

use crate::embed::BGE_QUERY_PREFIX;

fn embed_text_json_query(query_text: &str) -> Option<Value> {
    let trimmed = query_text.trim();
    if trimmed.is_empty() { return None; }
    let prefixed = format!("{}{}", BGE_QUERY_PREFIX, trimmed);
    let v = embed_text(&prefixed)?;
    Some(Value::Array(v.into_iter().map(|f| {
        serde_json::Number::from_f64(f as f64).map(Value::Number).unwrap_or(Value::Null)
    }).collect()))
}

fn json_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    if let Value::Array(arr) = v {
        let mut out = Vec::with_capacity(arr.len());
        for x in arr { if let Some(f) = x.as_f64() { out.push(f as f32); } }
        if !out.is_empty() { return Some(out); }
    }
    None
}

// The indexing pipeline below threads no config: it is a deep call chain
// (walk -> chunk -> embed -> persist) whose every level would need a
// `&RagConfig` parameter to reach these three namespace names. Rather than
// leave them as free-floating literals that a config change would silently
// desync from, they are resolved ONCE here from the same
// `NamespaceConfig::default()` the rest of the RAG layer defaults to.
// Threading real config down this chain is the remaining step; until then a
// non-default `namespaces.code` would only take effect on the query side, so
// these deliberately resolve from defaults rather than pretending otherwise.
fn code_ns_default() -> crate::ragconfig::NamespaceConfig {
    crate::ragconfig::NamespaceConfig::default()
}

fn manifest_ns() -> String {
    code_ns_default().manifest_namespace()
}

fn code_ns() -> String {
    code_ns_default().code
}

fn code_vec_ns() -> String {
    let ns = code_ns_default();
    ns.vec_namespace(&ns.code)
}

const MANIFEST_VERSION: u64 = 5;

#[derive(Clone)]
struct ChunkRecord {
    key: String,
    kind: String,
    name: String,
    ls: usize,
    le: usize,
    emb: Vec<f32>,
}

struct FileManifest {
    hash: u32,
    /// The value this file contributes to the WHOLE-TREE digest.
    ///
    /// Deliberately separate from `hash`: `hash` is crc32 (change detection
    /// within this module), while `current_digest()` folds an fnv1a64-derived
    /// u32 per file. Storing it here lets the stat-only fast path contribute
    /// the correct digest value WITHOUT re-reading the file -- previously that
    /// branch pushed the mtime instead, which `current_digest()` could never
    /// reproduce, so the stored digest structurally never matched and every
    /// dispatch re-indexed the entire tree.
    ///
    /// Optional because manifests written before this field existed still
    /// parse; a missing value simply forces that one file down the full path
    /// once, which repopulates it.
    digest_hash: Option<u32>,
    mtime_ms: f64,
    commit_overview: Option<String>,
    chunks: Vec<ChunkRecord>,
}

fn manifest_to_json(fp: &str, hash: u32, digest_hash: u32, mtime_ms: f64, commit_overview: &Option<String>, chunks: &[ChunkRecord]) -> String {
    let arr: Vec<Value> = chunks.iter().map(|c| json!({
        "key": c.key,
        "kind": c.kind,
        "name": c.name,
        "ls": c.ls,
        "le": c.le,
        "emb": c.emb,
    })).collect();
    json!({ "v": MANIFEST_VERSION, "path": fp, "hash": hash, "digest_hash": digest_hash, "mtime_ms": mtime_ms, "commit_overview": commit_overview, "chunks": arr }).to_string()
}

fn parse_manifest(val: &str) -> Option<(String, FileManifest)> {
    let parsed: Value = serde_json::from_str(val).ok()?;
    // Accept any manifest version we know how to read forward, rather than
    // rejecting everything that is not the current version.
    //
    // Rejecting on `v != MANIFEST_VERSION` looks conservative but is actively
    // destructive here: load_manifests routes a parse failure to
    // purge_stale_manifest_row, which fv_deletes the file's chunk keys AND its
    // manifest row. So bumping MANIFEST_VERSION did not merely invalidate the
    // cache -- it made every pass DELETE the entire cache and rebuild it from
    // zero, forever, because the rewritten rows are only ever written for files
    // that survive a pass. Live-witnessed: all 230 manifest rows on disk were
    // v4 while the code demanded v5.
    //
    // Every field added since v4 is optional-with-a-sane-default on read
    // (commit_overview: Option, digest_hash: Option), so an older row is
    // readable as-is and is silently upgraded the next time its file is
    // genuinely re-indexed. A row OLDER than the readable floor still returns
    // None and is purged, which is correct -- we cannot interpret it.
    const MIN_READABLE_MANIFEST_VERSION: u64 = 4;
    match parsed.get("v").and_then(|v| v.as_u64()) {
        Some(v) if v >= MIN_READABLE_MANIFEST_VERSION && v <= MANIFEST_VERSION => {}
        _ => return None,
    }
    let fp = parsed.get("path").and_then(|p| p.as_str())?.to_string();
    let hash = parsed.get("hash").and_then(|h| h.as_u64())? as u32;
    let digest_hash = parsed.get("digest_hash").and_then(|h| h.as_u64()).map(|h| h as u32);
    let mtime_ms = parsed.get("mtime_ms").and_then(|m| m.as_f64()).unwrap_or(0.0);
    let commit_overview = parsed.get("commit_overview").and_then(|v| v.as_str()).map(String::from);
    let arr = parsed.get("chunks").and_then(|c| c.as_array())?;
    let mut chunks = Vec::with_capacity(arr.len());
    for c in arr {
        let key = c.get("key").and_then(|x| x.as_str())?.to_string();
        let kind = c.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ls = c.get("ls").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let le = c.get("le").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let emb = json_to_f32_vec(c.get("emb")?)?;
        chunks.push(ChunkRecord { key, kind, name, ls, le, emb });
    }
    Some((fp, FileManifest { hash, digest_hash, mtime_ms, commit_overview, chunks }))
}

const SUBMODULE_DIRS: &[&str] = &[
    "rs-plugkit", "rs-codeinsight", "rs-search",
    "agentplug", "agentplug-bert", "agentplug-libsql", "agentplug-treesitter",
];

fn is_submodule_path(fp: &str) -> bool {
    let first_seg = fp.split('/').next().unwrap_or(fp);
    SUBMODULE_DIRS.contains(&first_seg)
}

fn compute_commit_overview(fp: &str) -> Option<String> {
    if is_submodule_path(fp) {
        return None;
    }
    let v = crate::wasm_dispatch::git_call_argv(
        &["log", "-1", "--format=%h\u{0}%s", "--shortstat", "--", fp],
        None,
    );
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if !ok || exit_code != 0 { return None; }
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let mut lines = stdout.lines();
    let header = lines.next()?.trim();
    if header.is_empty() { return None; }
    let mut parts = header.splitn(2, '\u{0}');
    let sha = parts.next()?.to_string();
    let subject = parts.next().unwrap_or("").trim().to_string();
    if sha.is_empty() { return None; }

    let mut files_touched: u32 = 1;
    let mut insertions: u32 = 0;
    let mut deletions: u32 = 0;
    for line in lines {
        let t = line.trim();
        if t.is_empty() { continue; }
        if let Some(n) = t.split(',').find_map(|seg| {
            let seg = seg.trim();
            seg.strip_suffix("file changed").or_else(|| seg.strip_suffix("files changed"))
                .map(|s| s.trim())
                .and_then(|s| s.parse::<u32>().ok())
        }) { files_touched = n; }
        if let Some(n) = t.split(',').find_map(|seg| {
            let seg = seg.trim();
            seg.strip_suffix("insertion(+)").or_else(|| seg.strip_suffix("insertions(+)"))
                .map(|s| s.trim())
                .and_then(|s| s.parse::<u32>().ok())
        }) { insertions = n; }
        if let Some(n) = t.split(',').find_map(|seg| {
            let seg = seg.trim();
            seg.strip_suffix("deletion(-)").or_else(|| seg.strip_suffix("deletions(-)"))
                .map(|s| s.trim())
                .and_then(|s| s.parse::<u32>().ok())
        }) { deletions = n; }
    }
    let subject = if subject.len() > 80 {
        let mut e = 77.min(subject.len());
        while e > 0 && !subject.is_char_boundary(e) { e -= 1; }
        format!("{}...", &subject[..e])
    } else {
        subject
    };
    Some(format!(
        "last changed {}: {} (+{}-{}, {} files)",
        sha, subject, insertions, deletions, files_touched
    ))
}

fn purge_stale_manifest_row(row_key: &str, val: &str) {
    if let Ok(parsed) = serde_json::from_str::<Value>(val) {
        if let Some(arr) = parsed.get("chunks").and_then(|c| c.as_array()) {
            for c in arr {
                if let Some(k) = c.get("key").and_then(|x| x.as_str()) {
                    fv_delete(&code_ns(), k);
                    fv_delete(&code_vec_ns(), k);
                }
            }
        }
    }
    fv_delete(&manifest_ns(), row_key);
}

fn load_manifests() -> std::collections::HashMap<String, FileManifest> {
    let mut out = std::collections::HashMap::new();
    let rows = fv_query(&manifest_ns(), "");
    if let Some(arr) = rows.as_array() {
        for row in arr {
            let val = match row.get("value").and_then(|v| v.as_str()) { Some(v) => v, None => continue };
            match parse_manifest(val) {
                Some((fp, m)) => { out.insert(fp, m); }
                None => {
                    if let Some(k) = row.get("key").and_then(|k| k.as_str()) {
                        purge_stale_manifest_row(k, val);
                    }
                }
            }
        }
    }
    out
}

fn slice_lines(content: &str, ls: usize, le: usize) -> String {
    if ls == 0 || le < ls { return String::new(); }
    content.lines().skip(ls - 1).take(le - ls + 1).collect::<Vec<_>>().join("\n")
}

/// Every path's chunk-row count in ONE query.
///
/// This replaces a per-file `SELECT COUNT(*) ... WHERE path=?1` that was called
/// from inside the indexing loop -- including on the stat-only "nothing
/// changed" fast path -- and the
/// shared libsql plugin opens, operates and closes the database on EVERY
/// exec_params call (no connection is retained across calls), so a warm pass
/// over an N-file tree paid N full open/close cycles purely to re-learn counts
/// that one GROUP BY answers. That is pure unnecessary waiting on the path
/// whose entire purpose is to be cheap.
///
/// Returns an empty map on failure, which makes every lookup read 0 and simply
/// routes files down the full path -- correct, just not fast, so a db hiccup
/// degrades to slow rather than to wrong.
fn chunk_rows_by_path(db_path: &str) -> std::collections::HashMap<String, usize> {
    let mut out = std::collections::HashMap::new();
    let rows = match libsql_wasm::query(db_path, "SELECT path, COUNT(*) AS c FROM code_chunks GROUP BY path") {
        Ok(r) => r,
        Err(_) => return out,
    };
    if let Some(arr) = rows.as_array() {
        for row in arr {
            let path = match row.get("path").and_then(|v| v.as_str()) { Some(p) => p, None => continue };
            let c = row
                .get("c")
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                .unwrap_or(0) as usize;
            out.insert(path.to_string(), c);
        }
    }
    out
}

const INSERT_CHUNK_SQL: &str = "INSERT INTO code_chunks(path, kind, name, line_start, line_end, body, embedding) VALUES(?1,?2,?3,?4,?5,?6,vector(?7))";

fn truncate_body(body: &str) -> &str {
    let mut e = body.len().min(8192);
    while e > 0 && !body.is_char_boundary(e) { e -= 1; }
    &body[..e]
}

fn truncate_for_embed(body: &str) -> &str {
    let mut e = body.len().min(1200);
    while e > 0 && !body.is_char_boundary(e) { e -= 1; }
    &body[..e]
}

fn write_chunk(libsql_ok: bool, db_path: &str, fp: &str, c: &ChunkRecord, body: &str) {
    if libsql_ok {
        let embedding_lit = vec_to_json_literal(&c.emb);
        let ls = c.ls.to_string();
        let le = c.le.to_string();
        let body_trunc = truncate_body(body);
        let params: [&str; 7] = [fp, &c.kind, &c.name, &ls, &le, body_trunc, &embedding_lit];
        let _ = libsql_wasm::exec_params(db_path, INSERT_CHUNK_SQL, &params);
    }
    let emb_json = serde_json::json!({ "embedding": c.emb }).to_string();
    fv_put(&code_ns(), &c.key, &emb_json);
}

fn delete_chunk_keys(chunks: &[ChunkRecord]) {
    for c in chunks {
        fv_delete(&code_ns(), &c.key);
        fv_delete(&code_vec_ns(), &c.key);
    }
}

pub fn index(root: &str, max_files: usize) -> Value {
    index_cfg(root, max_files, &crate::ragconfig::RagConfig::default())
}

/// Same as [`index`], with the knowledgebase config supplied explicitly --
/// matching the `_cfg` convention every other config-aware entry point in this
/// module already follows.
pub fn index_cfg(root: &str, max_files: usize, cfg: &crate::ragconfig::RagConfig) -> Value {
    let db_path = project_db_path(None);
    let libsql_err = ensure_schema_at(&db_path).err().map(|e| e.to_string());
    let libsql_ok = libsql_err.is_none();
    if let Some(e) = &libsql_err {
        let msg = format!("code_index: libsql unavailable at {} -- {} (digest will not persist and chunk reads return empty)", db_path, e);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let kvvec_cleared = clear_codeinsight_if_dim_mismatch();
    if kvvec_cleared {
        let rows = fv_query(&manifest_ns(), "");
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|k| k.as_str()) { fv_delete(&manifest_ns(), k); }
            }
        }
    }
    let prior = load_manifests();
    // Hoisted out of the per-file loop: one GROUP BY instead of one full
    // open/query/close per file (see chunk_rows_by_path).
    let chunk_counts = if libsql_ok {
        chunk_rows_by_path(&db_path)
    } else {
        std::collections::HashMap::new()
    };
    let chunk_rows = |fp: &str| -> usize { chunk_counts.get(fp).copied().unwrap_or(0) };
    let r = if root.is_empty() { "/" } else { root };
    let limit = max_files.max(50).min(2000);
    let files = collect_files(r, limit);
    const PRUNE_ENUMERATION_CAP: usize = 20000;
    let full_files = if limit >= PRUNE_ENUMERATION_CAP { files.clone() } else { collect_files(r, PRUNE_ENUMERATION_CAP) };
    {
        let msg = format!("code_index: indexing root={} files={} libsql_ok={} manifests={}", r, files.len(), libsql_ok, prior.len());
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let index_wall_budget_ms: u64 = cfg.index.wall_budget_ms;
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut indexed = 0;
    let mut chunked = 0;
    let mut embedded = 0;
    let mut reused = 0;
    let mut reused_files = 0;
    let mut skipped_no_embed = 0u32;
    let mut deferred_files = 0u32;
    let mut langs = std::collections::BTreeMap::<String, u32>::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut digest_entries: Vec<(String, u32)> = Vec::with_capacity(files.len());

    for raw_fp in &files {
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > index_wall_budget_ms {
            deferred_files += 1;
            continue;
        }
        let canon = raw_fp.trim_start_matches("./").trim_start_matches('/').to_string();
        let fp = &canon;
        let dot = fp.rfind('.');
        let ext = match dot { Some(i) => &fp[i..], None => "" };
        let lang_name = match lang_for_ext(ext) { Some(x) => x, None => continue };

        if let Some(m) = prior.get(fp) {
            if let Some(stat) = crate::wasm_dispatch::host_stat(fp)
                .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            {
                let stat_mtime = stat.get("mtime_ms").and_then(|v| v.as_f64());
                // Only take the stat-only fast path when the manifest can supply
                // this file's digest contribution; without it we cannot produce a
                // digest that current_digest() will reproduce, and skipping the
                // read would poison the whole-tree digest (see digest_hash docs).
                if let (Some(mtime), Some(dh)) = (stat_mtime, m.digest_hash) {
                    if mtime == m.mtime_ms && libsql_ok && chunk_rows(fp) == m.chunks.len() {
                        seen.insert(fp.clone());
                        indexed += 1;
                        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
                        chunked += m.chunks.len() as i32;
                        reused += m.chunks.len() as i32;
                        reused_files += 1;
                        // The digest MUST be the same content-derived value on
                        // every branch. current_digest() (what this is compared
                        // against next dispatch) folds fnv1a64(content), so
                        // pushing mtime here made the stored digest structurally
                        // unable to ever match -- every dispatch saw
                        // "digest-mismatch" and re-indexed the whole tree, which
                        // is exactly the cost this fast path exists to avoid.
                        digest_entries.push((fp.clone(), dh));
                        continue;
                    }
                }
            }
        }

        let content = match host_read(fp)
            .or_else(|| host_read(raw_fp))
            .or_else(|| host_read(&format!("/{}", fp)))
        { Some(c) => c, None => continue };
        if content.len() > cfg.index.max_file_bytes { continue; }
        let file_mtime = crate::wasm_dispatch::host_stat(fp)
            .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            .and_then(|s| s.get("mtime_ms").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        seen.insert(fp.clone());
        indexed += 1;
        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
        let file_hash = crc32(&content);
        let path_hash = crc32(fp);
        // Computed once and BOTH pushed into this pass's digest and persisted in
        // the manifest, so a later stat-only fast path can contribute the exact
        // same value without re-reading the file. Must stay identical to
        // current_digest()'s own per-file hash or the digest never matches.
        let file_digest_hash = crate::pipeline::fnv1a64(content.as_bytes()) as u32;
        digest_entries.push((fp.clone(), file_digest_hash));

        if let Some(m) = prior.get(fp) {
            if m.hash == file_hash {
                if libsql_ok && chunk_rows(fp) == m.chunks.len() {
                    chunked += m.chunks.len() as i32;
                    reused += m.chunks.len() as i32;
                    reused_files += 1;
                    continue;
                }
                if libsql_ok {
                    let _ = libsql_wasm::exec_params(&db_path, "DELETE FROM code_chunks WHERE path=?1", &[fp]);
                }
                for c in &m.chunks {
                    let body = slice_lines(&content, c.ls, c.le);
                    write_chunk(libsql_ok, &db_path, fp, c, &body);
                    chunked += 1;
                    reused += 1;
                }
                reused_files += 1;
                continue;
            }
            if libsql_ok {
                let _ = libsql_wasm::exec_params(&db_path, "DELETE FROM code_chunks WHERE path=?1", &[fp]);
            }
            delete_chunk_keys(&m.chunks);
        } else if libsql_ok {
            let _ = libsql_wasm::exec_params(&db_path, "DELETE FROM code_chunks WHERE path=?1", &[fp]);
        }

        let mut chunks = extract_chunks(fp, &content, lang_name);
        if chunks.is_empty() && lang_name == "markdown" && !content.trim().is_empty() {
            let whole = content.chars().take(4000).collect::<String>();
            let line_end = content.lines().count().max(1);
            chunks.push(("document".to_string(), String::new(), 1, line_end, whole));
        }
        if chunks.iter().any(|(_, _, _, _, body)| body.len() > OVERSIZED_CHUNK_SPLIT_THRESHOLD) {
            chunks = chunks
                .into_iter()
                .flat_map(|(kind, name, ls, le, body)| split_oversized_chunk(&kind, &name, ls, le, &body))
                .collect();
        }

        const MAX_CHUNKS_PER_FILE_PER_PASS: usize = 64;
        // The count cap alone does NOT bound wall-clock cost, and that gap is
        // what actually stalls the index. Live-witnessed: a 153KB AGENTS.md
        // produced 49 chunks -- comfortably UNDER the 64 cap, so it was never
        // truncated -- yet its single embed_texts_batch call took 39981ms,
        // blowing both index_wall_budget_ms (30s by default, and the outer check at the
        // top of this loop only fires BETWEEN files) and the supervisor's own
        // 30s heartbeat-stale limit, which then killed the watcher mid-pass
        // and left the index frozen at deferred_files=499 with the embedder
        // crashed. Per-chunk cost is highly non-uniform (a chunk of prose
        // embeds far slower than a short function body), so a count cap can
        // never stand in for a time bound.
        //
        // Bound the batch by the budget actually remaining for this pass:
        // scale the per-file chunk allowance down as the pass approaches
        // index_wall_budget_ms. A file arriving with little budget left
        // embeds only a small prefix now and finishes on a later pass, which
        // still converges (the file is marked seen and its manifest written,
        // exactly as the count-cap path already does -- see the livelock
        // rationale above for why deferring the file ENTIRELY is wrong).
        let elapsed_now = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        let remaining_ms = index_wall_budget_ms.saturating_sub(elapsed_now);
        // Derived from the same live measurement: 39981ms / 49 chunks is
        // ~816ms per prose chunk on unaccelerated wasm32 BERT. Budget against
        // a deliberately pessimistic per-chunk estimate so the bound holds on
        // the slow (prose) case rather than the fast (short-code) case.
        const PESSIMISTIC_MS_PER_CHUNK: u64 = 800;
        let budget_chunks = (remaining_ms / PESSIMISTIC_MS_PER_CHUNK).max(1) as usize;
        let cap = MAX_CHUNKS_PER_FILE_PER_PASS.min(budget_chunks);
        let oversized = chunks.len() > cap;
        if oversized {
            let full = chunks.len();
            chunks.truncate(cap);
            let msg = format!(
                "code_index: capping {} chunks={} -> {} (count_cap={} budget_chunks={} remaining_ms={}; file still indexed and marked seen)",
                fp, full, cap, MAX_CHUNKS_PER_FILE_PER_PASS, budget_chunks, remaining_ms
            );
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_chunk_cap", json!({
                "path": fp,
                "chunks_total": full,
                "chunks_indexed": cap,
                "count_cap": MAX_CHUNKS_PER_FILE_PER_PASS,
                "budget_chunks": budget_chunks,
                "remaining_ms": remaining_ms,
            }));
        }

        let embed_inputs: Vec<String> = chunks.iter()
            .map(|(_, name, _, _, body)| format!("{} {}", name, truncate_for_embed(body)))
            .collect();
        let embed_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        let embed_results = embed_texts_batch(&embed_inputs);
        let embed_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(embed_started);
        if embed_ms > 3000 {
            let msg = format!("code_index: SLOW embed_texts_batch fp={} chunks={} embed_ms={}", fp, embed_inputs.len(), embed_ms);
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_slow_file_embed", json!({
                "path": fp,
                "chunks": embed_inputs.len(),
                "embed_ms": embed_ms,
            }));
        }

        let mut records: Vec<ChunkRecord> = Vec::new();
        for (idx, ((kind, name, ls, le, body), emb_opt)) in chunks.into_iter().zip(embed_results.into_iter()).enumerate() {
            let v = match emb_opt {
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
            let key = format!("ci-{:x}-{:x}-{}", path_hash, file_hash, idx);
            let rec = ChunkRecord { key, kind, name, ls, le, emb: v };
            write_chunk(libsql_ok, &db_path, fp, &rec, &body);
            records.push(rec);
        }
        let commit_overview = compute_commit_overview(fp);
        fv_put(&manifest_ns(), fp, &manifest_to_json(fp, file_hash, file_digest_hash, file_mtime, &commit_overview, &records));
    }

    let files_set: std::collections::HashSet<&str> = full_files.iter().map(|s| s.trim_start_matches("./").trim_start_matches('/')).collect();
    let mut removed_files = 0;
    for (fp, m) in &prior {
        if !seen.contains(fp) && !files_set.contains(fp.as_str()) {
            delete_chunk_keys(&m.chunks);
            fv_delete(&manifest_ns(), fp);
            removed_files += 1;
        }
    }
    // A partial pass MUST still persist what it converged, or the digest is
    // never written at all on any tree big enough to exceed the wall budget --
    // and a missing digest is treated as stale, so the very next dispatch
    // re-indexes everything, which guarantees the next pass is also partial.
    // That is a self-sustaining loop: live-witnessed as a permanently absent
    // .codeinsight-digest alongside 230 manifest rows, with only 10 distinct
    // paths ever reaching code_chunks.
    //
    // The digest is only a CHANGE DETECTOR, so a partial digest is still sound:
    // it is computed from the files this pass actually accounted for, and the
    // deferred ones simply keep their prior entries absent, which reads as
    // "changed" next pass -- exactly the resume behaviour wanted. Marking it
    // partial keeps the distinction visible rather than pretending convergence.
    if deferred_files == 0 {
        let digest = digest_from_entries(digest_entries);
        store_digest(&digest);
        let msg = format!("code_index: done files_indexed={} chunks={} embedded={} reused={} reused_files={} removed_files={} skipped_no_embed={} digest={}", indexed, chunked, embedded, reused, reused_files, removed_files, skipped_no_embed, digest);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    } else {
        // Persist the converged subset (see the rationale above). Tagged
        // ":partial=N" so it can never be mistaken for a complete-tree digest:
        // current_digest() always produces the untagged full-tree form, so a
        // partial digest still compares as "changed" next pass and the resume
        // continues -- but the file now EXISTS, which stops the
        // never-stored/always-reindex loop that starved this cache entirely.
        let partial_digest = format!("{}:partial={}", digest_from_entries(digest_entries), deferred_files);
        store_digest(&partial_digest);
        let msg = format!("code_index: partial pass (wall budget) files_indexed={} deferred_files={} embedded={} reused={} removed_files={} -- partial digest stored, next call resumes", indexed, deferred_files, embedded, reused, removed_files);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
        crate::wasm_dispatch::emit_event("codeinsight_index_partial", json!({
            "files_indexed": indexed,
            "deferred_files": deferred_files,
            "embedded": embedded,
        }));
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
        "deferred_files": deferred_files,
        "kvvec_cleared_dim_mismatch": kvvec_cleared,
        "by_language": langs,
    })
}

fn embed_text_batch_fallback(inputs: &[String]) -> Vec<Option<Vec<f32>>> {
    inputs.iter().map(|t| embed_text(t)).collect()
}

fn embed_texts_batch(inputs: &[String]) -> Vec<Option<Vec<f32>>> {
    if inputs.is_empty() { return Vec::new(); }
    let resp = call_out_of_process_plugin("bert", "embed_batch", &json!({ "texts": inputs }));
    if !plugin_ok(&resp) { return embed_text_batch_fallback(inputs); }
    match resp.get("embeddings").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == inputs.len() => {
            arr.iter().map(|e| if e.is_null() { None } else { json_to_f32_vec(e) }).collect()
        }
        _ => embed_text_batch_fallback(inputs),
    }
}

const DIGEST_MAX_FILES: usize = 2000;
const DIGEST_PATH: &str = ".gm/exec-spool/.codeinsight-digest";

fn digest_from_entries(mut entries: Vec<(String, u32)>) -> String {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup_by(|a, b| a.0 == b.0);
    let mut acc = String::with_capacity(entries.len() * 32);
    for (path, hash) in &entries {
        acc.push_str(path);
        acc.push('|');
        acc.push_str(&format!("{:08x}", hash));
        acc.push('\n');
    }
    format!("v3:{:016x}:files={}", crate::pipeline::fnv1a64(acc.as_bytes()), entries.len())
}

pub fn current_digest() -> String {
    current_digest_cfg(&crate::ragconfig::RagConfig::default())
}

/// Same as [`current_digest`] with explicit config. The file-size cap MUST match
/// the indexer own, or the digest counts files the index skips and the two can
/// never agree -- the same class of mismatch that made the stored digest
/// permanently unequal to the computed one.
pub fn current_digest_cfg(cfg: &crate::ragconfig::RagConfig) -> String {
    let files = collect_files(".", DIGEST_MAX_FILES);
    let mut entries: Vec<(String, u32)> = Vec::new();
    for raw_fp in &files {
        let canon = raw_fp.trim_start_matches("./").trim_start_matches('/').to_string();
        let ext = match canon.rfind('.') { Some(i) => &canon[i..], None => "" };
        if lang_for_ext(ext).is_none() { continue; }
        let stat = match crate::wasm_dispatch::host_stat(&canon)
            .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
        { Some(s) => s, None => continue };
        if stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0) > cfg.index.max_file_bytes as u64 { continue; }
        let content = match host_read(&canon)
            .or_else(|| host_read(raw_fp))
        { Some(c) => c, None => continue };
        let content_hash = crate::pipeline::fnv1a64(content.as_bytes()) as u32;
        entries.push((canon, content_hash));
    }
    digest_from_entries(entries)
}

pub fn stored_digest() -> Option<String> {
    crate::wasm_dispatch::host_read(DIGEST_PATH)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn store_digest(digest: &str) {
    let _ = crate::wasm_dispatch::host_write(DIGEST_PATH, digest);
    fv_delete(&code_ns(), "__digest__");
}

pub fn overview() -> Value {
    if stored_digest().is_none() {
        return Value::Null;
    }
    let db_path = project_db_path(None);
    let file_count = libsql_wasm::query_params(&db_path, "SELECT COUNT(DISTINCT path) AS c FROM code_chunks", &[])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()))
        .and_then(|row| row.get("c").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let symbol_count = libsql_wasm::query_params(&db_path, "SELECT COUNT(*) AS c FROM code_chunks", &[])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()))
        .and_then(|row| row.get("c").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let by_kind = libsql_wasm::query_params(
        &db_path,
        "SELECT kind, COUNT(*) AS c FROM code_chunks GROUP BY kind ORDER BY c DESC LIMIT 10",
        &[],
    )
    .unwrap_or(Value::Array(Vec::new()));
    let largest_files = libsql_wasm::query_params(
        &db_path,
        "SELECT path, COUNT(*) AS c FROM code_chunks GROUP BY path ORDER BY c DESC LIMIT 10",
        &[],
    )
    .unwrap_or(Value::Array(Vec::new()));
    json!({
        "file_count": file_count,
        "symbol_count": symbol_count,
        "by_kind": by_kind,
        "largest_files": largest_files,
        "digest": stored_digest(),
        "likely_orphaned": likely_orphaned_symbols(&db_path, 20),
    })
}

fn likely_orphaned_symbols(db_path: &str, limit: usize) -> Value {
    let candidates = libsql_wasm::query_params(
        db_path,
        "SELECT id, path, kind, name, line_start FROM code_chunks \
         WHERE kind IN ('function_item','function_declaration','method_definition') \
         AND name != '' AND LENGTH(name) > 3 LIMIT 2000",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    let mut orphaned = Vec::new();
    for c in &candidates {
        if orphaned.len() >= limit { break; }
        let Some(name) = c.get("name").and_then(|v| v.as_str()) else { continue };
        let Some(id) = c.get("id").and_then(|v| v.as_u64()) else { continue };
        let id_s = id.to_string();
        let pat_s = format!("%{}%", name);
        let count = libsql_wasm::query_params(
            db_path,
            "SELECT COUNT(*) AS c FROM code_chunks WHERE id != ?1 AND body LIKE ?2",
            &[id_s.as_str(), pat_s.as_str()],
        )
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()))
        .and_then(|row| row.get("c").and_then(|v| v.as_u64()))
        .unwrap_or(1);
        if count == 0 {
            orphaned.push(json!({
                "path": c.get("path"),
                "name": name,
                "line": c.get("line_start"),
            }));
        }
    }
    Value::Array(orphaned)
}

pub struct ChunkMeta {
    pub key: String,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub ls: usize,
    pub le: usize,
}

pub struct FusionCorpus {
    metas: Vec<ChunkMeta>,
    file_cache: std::collections::HashMap<String, Option<String>>,
    overview_by_path: std::collections::HashMap<String, String>,
}

impl FusionCorpus {
    pub fn load() -> Self {
        let mut metas = Vec::new();
        let mut overview_by_path = std::collections::HashMap::new();
        for (fp, m) in load_manifests() {
            if let Some(ov) = &m.commit_overview {
                overview_by_path.insert(fp.clone(), ov.clone());
            }
            for c in &m.chunks {
                metas.push(ChunkMeta {
                    key: c.key.clone(),
                    path: fp.clone(),
                    kind: c.kind.clone(),
                    name: c.name.clone(),
                    ls: c.ls,
                    le: c.le,
                });
            }
        }
        FusionCorpus { metas, file_cache: std::collections::HashMap::new(), overview_by_path }
    }

    pub fn overview_for_key(&self, key: &str) -> Option<String> {
        let m = self.metas.iter().find(|m| m.key == key)?;
        self.overview_by_path.get(&m.path).cloned()
    }

    pub fn symbol_for_key(&self, key: &str) -> Option<Value> {
        let m = self.metas.iter().find(|m| m.key == key)?;
        Some(json!({
            "path": m.path,
            "kind": m.kind,
            "name": m.name,
            "line_start": m.ls,
            "line_end": m.le,
        }))
    }

    fn file_content(&mut self, path: &str) -> Option<String> {
        if let Some(cached) = self.file_cache.get(path) { return cached.clone(); }
        let content = host_read(path).or_else(|| host_read(&format!("/{}", path)));
        self.file_cache.insert(path.to_string(), content.clone());
        content
    }

    pub fn key_for_path_line(&self, path: &str, ls: usize) -> Option<String> {
        let norm = path.trim_start_matches("./").trim_start_matches('/');
        self.metas.iter()
            .find(|m| {
                let mp = m.path.trim_start_matches("./").trim_start_matches('/');
                mp == norm && m.ls == ls
            })
            .map(|m| m.key.clone())
    }

    pub fn text_for_key(&mut self, key: &str) -> Option<String> {
        let i = self.metas.iter().position(|m| m.key == key)?;
        let (path, name, ls, le) = {
            let m = &self.metas[i];
            (m.path.clone(), m.name.clone(), m.ls, m.le)
        };
        let content = self.file_content(&path)?;
        let body = slice_lines(&content, ls, le);
        let body_trunc = {
            let mut e = body.len().min(8192);
            while e > 0 && !body.is_char_boundary(e) { e -= 1; }
            body[..e].to_string()
        };
        Some(format!("{}:{}:{} {}\n{}", path, ls, le, name, body_trunc))
    }

    pub fn bm25_rank(&mut self, query: &str, k: usize) -> Vec<String> {
        const K1: f64 = 1.2;
        const B: f64 = 0.75;
        let q_tokens = rs_search::tokenize::tokenize(query);
        if q_tokens.is_empty() || self.metas.is_empty() { return Vec::new(); }
        let mut doc_tfs: Vec<(usize, std::collections::HashMap<String, u32>, f64)> = Vec::new();
        for i in 0..self.metas.len() {
            let (path, name, ls, le) = {
                let m = &self.metas[i];
                (m.path.clone(), m.name.clone(), m.ls, m.le)
            };
            let content = match self.file_content(&path) { Some(c) => c, None => continue };
            let body = slice_lines(&content, ls, le);
            let tf = term_freqs(&format!("{} {} {}", path, name, body));
            let dl: u32 = tf.values().sum();
            doc_tfs.push((i, tf, dl as f64));
        }
        if doc_tfs.is_empty() { return Vec::new(); }
        let n = doc_tfs.len() as f64;
        let avgdl = doc_tfs.iter().map(|(_, _, dl)| dl).sum::<f64>() / n;
        let avgdl = if avgdl > 0.0 { avgdl } else { 1.0 };
        let mut df: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for t in &q_tokens {
            let c = doc_tfs.iter().filter(|(_, tf, _)| tf.contains_key(t)).count() as u32;
            df.insert(t.as_str(), c);
        }
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (i, tf, dl) in &doc_tfs {
            let mut score = 0.0;
            for t in &q_tokens {
                let f = *tf.get(t).unwrap_or(&0) as f64;
                if f == 0.0 { continue; }
                let d = *df.get(t.as_str()).unwrap_or(&0) as f64;
                let idf = (1.0 + (n - d + 0.5) / (d + 0.5)).ln();
                score += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl));
            }
            if score > 0.0 { scored.push((*i, score)); }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(i, _)| self.metas[i].key.clone()).collect()
    }
}

fn term_freqs(text: &str) -> std::collections::HashMap<String, u32> {
    let mut out: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for word in text.split(|c: char| c.is_whitespace() || "(){}[]<>,;:\"'`=+*&|!?/\\#".contains(c)) {
        if word.is_empty() { continue; }
        let mut set = std::collections::HashSet::new();
        rs_search::tokenize::add_word_tokens(word, &mut set);
        for t in set { *out.entry(t).or_insert(0) += 1; }
    }
    out
}

fn git_commit_rank_fallback(query: &str, k: usize) -> Vec<String> {
    let q_tokens = rs_search::tokenize::tokenize(query);
    if q_tokens.is_empty() { return Vec::new(); }
    let log = crate::wasm_dispatch::git_call("log --format=%H --name-only -n 100 --no-decorate", None);
    let stdout = log.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let mut commits: Vec<(String, f64)> = Vec::new();
    let mut cur_hash: Option<String> = None;
    let mut cur_score = 0.0f64;
    let flush = |commits: &mut Vec<(String, f64)>, hash: Option<String>, score: f64| {
        if let Some(h) = hash {
            if score > 0.0 { commits.push((h, score)); }
        }
    };
    for line in stdout.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }
        if t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            flush(&mut commits, cur_hash.take(), cur_score);
            cur_hash = Some(t.to_string());
            cur_score = 0.0;
        } else if cur_hash.is_some() {
            let ftoks = rs_search::tokenize::tokenize(t);
            cur_score += q_tokens.iter().filter(|q| ftoks.contains(q)).count() as f64;
        }
    }
    flush(&mut commits, cur_hash.take(), cur_score);
    commits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    commits.into_iter().take(k).map(|(h, _)| h).collect()
}

pub fn git_commit_rank(query: &str, k: usize) -> Vec<String> {
    let _ = crate::git_commit_vectors::sync_incremental();
    let embedding = embed_text_json_query(query);
    if let Some(emb) = embedding {
        if let Ok(hits) = crate::git_commit_vectors::search(&emb, k) {
            if !hits.is_empty() {
                return hits.into_iter().map(|(hash, _, _)| hash).collect();
            }
        }
    }
    git_commit_rank_fallback(query, k)
}

pub fn search(query: &str, k: usize, inline_embedding: Option<&Value>) -> Value {
    if let Err(e) = ensure_schema() { return json!({ "ok": false, "error": e }); }
    let db_path = project_db_path(None);
    let qvec = match inline_embedding.and_then(json_to_f32_vec).or_else(|| embed_text(query)) {
        Some(v) => v,
        None => {
            let like = format!("%{}%", query);
            let sql = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM code_chunks WHERE body LIKE ?1 OR name LIKE ?1 LIMIT {}", k);
            return match libsql_wasm::query_params(&db_path, &sql, &[&like]) {
                Ok(rows) => json!({ "ok": true, "mode": "fallback_like", "rows": rows }),
                Err(e) => json!({ "ok": false, "mode": "fallback_like", "error": e }),
            };
        }
    };
    let qlit = vec_to_json_literal(&qvec);
    let pool = crate::vecns::QueryBudget::default().pool(k);
    let sql = format!(
        "SELECT c.path, c.kind, c.name, c.line_start, c.line_end, substr(c.body,1,400) AS snippet, vector_distance_cos(c.embedding, vector(?1)) AS distance FROM vector_top_k('code_chunks_vec', vector(?2), {}) AS v JOIN code_chunks AS c ON c.rowid = v.id ORDER BY distance ASC LIMIT {}",
        pool, k
    );
    match libsql_wasm::query_params(&db_path, &sql, &[&qlit, &qlit]) {
        Ok(rows) => json!({ "ok": true, "mode": "vector_top_k", "rows": rows }),
        Err(e) if crate::shared_db::is_malformed(&e) && crate::shared_db::recover_malformed_shared_db() => {
            let _ = ensure_schema();
            match libsql_wasm::query_params(&db_path, &sql, &[&qlit, &qlit]) {
                Ok(rows) => json!({ "ok": true, "mode": "vector_top_k_after_recover", "recovered_from": e, "rows": rows }),
                Err(e2) => json!({ "ok": false, "mode": "recovered_but_still_failing", "vec_err": e, "retry_err": e2 }),
            }
        }
        Err(e) => {
            let like = format!("%{}%", query);
            let sql2 = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM code_chunks WHERE body LIKE ?1 OR name LIKE ?1 LIMIT {}", k);
            match libsql_wasm::query_params(&db_path, &sql2, &[&like]) {
                Ok(rows) => json!({ "ok": true, "mode": "fallback_like_after_vec_err", "vec_err": e, "rows": rows }),
                Err(e2) => json!({ "ok": false, "vec_err": e, "fallback_err": e2 }),
            }
        }
    }
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
    let ts = unsafe { crate::wasm_dispatch::host_now_ms() }.to_string();
    let sql = format!(
        "INSERT INTO memories(namespace, text, ts, embedding) VALUES(?1,?2,?3,{})",
        embedding_sql
    );
    match libsql_wasm::exec_params(&db_name, &sql, &[namespace, stored_text, &ts]) {
        Ok(()) => json!({ "ok": true, "memorized": true, "embedded": true, "inline": inline_embedding.is_some(), "project_path": project_path }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}
