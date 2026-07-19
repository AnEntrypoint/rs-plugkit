#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::wasm_dispatch::{host_read, unpack_to_value_pub};
use crate::vecstore::{drop_if_dim_mismatch_at as drop_if_dim_mismatch, vec_to_json_literal, EXPECTED_EMBED_DIM};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
    fn host_plugin_call(plugin_ptr: *const u8, plugin_len: u32, verb_ptr: *const u8, verb_len: u32, body_ptr: *const u8, body_len: u32) -> u64;
}

/// Dispatches one call to an out-of-process plugin (libsql/bert/treesitter)
/// via the host_plugin_call import, returning the raw JSON response --
/// {"ok":true,...} or {"ok":false,"error":"..."}. Every libsql_wasm::*,
/// crate::embed::*, and tree_sitter::* call this file used to make
/// in-process now routes through here instead; callers below unwrap this
/// into the same Result<T, String>/Option<T> shapes the old direct calls
/// returned, so nothing outside this file needs to change.
fn call_plugin(plugin: &str, verb: &str, body: &Value) -> Value {
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

// This file used to define its OWN private `mod libsql_wasm { ... }` here,
// a second, independent, drifted-stale copy of crate::libsql_wasm (the real
// crate-level module in libsql_wasm.rs) -- it shadowed the crate module
// since this file never `use crate::libsql_wasm;`'d, so every bare
// `libsql_wasm::` call below resolved to this local copy, not the shared
// one pipeline.rs used. Deleted in favor of `use crate::libsql_wasm;`
// (below) so there is exactly one implementation of the now-stateless,
// absolute-path-based libsql wrapper, matching pipeline.rs.
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

// Directory-name ignore list -- checked by exact path-segment match, applied
// BEFORE descending (walk_posix) or BEFORE the flat-list filter
// (collect_files), so an ignored directory's contents are never even listed
// via host_fs_readdir, not merely filtered out after the fact. Modeled on
// mcp-thorns' .thornsignore breadth (c:\dev\mcp-thorns) -- every major
// language/framework/tool/IDE/cache ecosystem gets its build-artifact and
// dependency directories skipped, since a codesearch index has zero value
// from indexing vendored/generated/cached content and every directory
// skipped here is a host_fs_readdir call (and everything under it) never
// made.
const SKIP_DIRS: &[&str] = &[
    // VCS
    ".git", ".svn", ".hg", ".bzr", "CVS", ".gm",
    // Node / JS / TS
    "node_modules", ".npm", ".yarn", ".pnp", ".next", ".nuxt", "dist", "out",
    "build", ".cache", ".parcel-cache", ".vite", ".turbo", ".nx", ".rush",
    ".lerna", ".pnpm-store", ".docusaurus", ".vuepress",
    // Python
    "__pycache__", ".pytest_cache", ".mypy_cache", ".hypothesis", ".pyre",
    ".pytype", "env", "venv", "ENV", ".venv", ".tox", "htmlcov", "site-packages",
    // Rust
    "target",
    // Go
    "vendor",
    // Java / JVM
    ".gradle", ".mvn", "bin", "obj",
    // Ruby
    ".bundle",
    // Swift / Xcode
    "Pods", "DerivedData",
    // Cloud / infra
    ".terraform", ".serverless",
    // Docker
    ".docker",
    // Caches / AI-agent tool state
    ".llamaindex", ".chroma", ".vectorstore", ".embeddings", ".langchain",
    "embeddings", "vector-db", "faiss-index", "chromadb",
    ".claude", ".wfgy", ".kilo", ".agents", ".code-search",
    ".plugkit-browser-profile-default", ".plugkit-agent-worktree",
    ".test-chrome-profile",
    // Editors / IDEs
    ".vscode", ".idea", ".vs", ".sublime-text", ".cursor", ".windsurf",
    ".zed", ".helix",
    // Test artifacts
    "coverage", ".nyc_output", "test-results", "playwright-report",
    ".plugkit-browser-profile",
    // Build/doc output that mirrors source, not source itself
    "_site", "public", "static", "site", "output", "builds", "artifacts",
    "compiled", "generated", "gen",
    // Mobile
    "Carthage", "fastlane",
    // ML experiment tracking / model weight+vocab dumps
    "mlruns", "wandb", "weights",
    // User-home tool caches (relevant when a repo root sits under $HOME)
    ".cargo", ".rustup", ".rbenv", ".rvm", ".nvm", ".pyenv", ".conda",
    ".m2", ".sbt", ".ivy2", ".gem",
];

// Filename SUFFIX ignore list -- files whose name ends with one of these are
// skipped regardless of directory, checked before host_read (the expensive
// step) ever fires. Lock files and minified/generated bundles carry zero
// codesearch value and are often large enough to meaningfully slow a walk.
const SKIP_FILE_SUFFIXES: &[&str] = &[
    ".min.js", ".min.css", ".bundle.js", ".chunk.js", ".map",
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb",
    "bun.lock", "Cargo.lock", "composer.lock", "Gemfile.lock", "poetry.lock",
    "Pipfile.lock", "go.sum", "uv.lock",
    // AI-agent-tool generated state files (not source)
    ".codeinsight", ".codeinsight.digest", ".perf-baseline.json",
    ".rs-exec.lock",
    // 3D models / game-engine assets (binary, not source)
    ".glb", ".gltf", ".vrm", ".fbx", ".blend", ".blend1", ".usdz", ".hf",
    ".uasset", ".umap",
    // Compiled binaries
    ".wasm", ".exe", ".dll", ".dylib", ".so", ".o", ".obj", ".a", ".lib",
    ".pdb", ".class", ".jar", ".war", ".ear", ".apk", ".aab", ".ipa",
    ".hex", ".elf", ".uf2", ".dfu",
    // Images / media / fonts / archives / office docs (binary, not source)
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".bmp", ".webp", ".tiff",
    ".pdf", ".mov", ".mp4", ".avi", ".flv", ".mkv", ".webm", ".mp3",
    ".m4a", ".wav", ".flac", ".ogg", ".woff", ".woff2", ".ttf", ".otf",
    ".eot", ".zip", ".tar", ".tar.gz", ".tgz", ".rar", ".7z", ".iso",
    ".bz2", ".xz", ".lz4", ".zst", ".cab", ".deb", ".rpm", ".dmg", ".msi",
    ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx",
    // Design tool binaries
    ".psd", ".ai", ".sketch", ".aep",
    // Data science / ML
    ".pkl", ".pickle", ".h5", ".hdf5", ".parquet", ".npy", ".npz",
    ".safetensors", ".ckpt", ".pt", ".pth", ".onnx", ".gguf",
    // ML tokenizer/vocab dumps -- large data files, not source, even though
    // they're JSON/text and would otherwise pass a binary-content check
    "tokenizer.json", "vocab.json", "vocab.txt", "merges.txt",
    "-tokenizer.json", "-vocab.json",
    // Crash/debug dumps
    ".stackdump", ".dmp", ".core",
    // Secrets / certificates (leak-prevention, independent of relevance)
    ".key", ".pem", ".p12", ".pfx", ".p8", ".crt", ".cer", ".der",
    "credentials.json", "secrets.yaml", "secrets.yml",
    // Database
    ".db", ".sqlite", ".sqlite3",
];

fn is_skipped_filename(name: &str) -> bool {
    SKIP_FILE_SUFFIXES.iter().any(|suf| name.ends_with(suf))
}

pub fn ensure_schema_at(path: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    libsql_wasm::open(path)?;
    let _ = drop_if_dim_mismatch(path, "code_chunks");
    let _ = drop_if_dim_mismatch(path, "memories");
    libsql_wasm::exec(path, "CREATE TABLE IF NOT EXISTS code_chunks (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, body TEXT, embedding F32_BLOB(384))")?;
    libsql_wasm::exec(path, "CREATE TABLE IF NOT EXISTS memories (id INTEGER PRIMARY KEY, namespace TEXT, text TEXT, ts INTEGER, embedding F32_BLOB(384))")?;
    crate::vecns::VecTableSpec { db_name: path, table: "code_chunks", index: "code_chunks_vec" }.ensure_index();
    crate::vecns::VecTableSpec { db_name: path, table: "memories", index: "memories_vec" }.ensure_index();
    Ok(())
}

/// Bare filename only, NOT what gets passed to the plugin -- the plugin is
/// now a stateless process-wide instance shared across every concurrently
/// active project, so a bare relative filename ("gm.db") is no longer a
/// safe identifier: two different projects both resolving to "gm.db" would
/// silently collide/share the SAME file if the plugin ever resolved
/// relative paths against its own process cwd rather than the calling
/// project's. `project_db_path` (below) is what callers actually use --
/// it resolves this filename against the CURRENT dispatch's project root
/// via host_cwd_string(), fresh every call.
fn project_db_filename(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("ext-{:x}.db", crc32(p)),
        _ => "gm.db".to_string(),
    }
}

/// Absolute `<host_cwd>/.gm/<filename>` path -- the only thing the
/// now-stateless shared libsql plugin actually uses to identify a db.
/// project_path=None resolves against THIS call's own project root
/// (host_cwd_string(), fresh every dispatch); Some(p) namespaces an
/// "external" project's db by a crc32-hash filename but still resolves the
/// directory against the CURRENT host_cwd, matching pre-existing
/// project_db_name's "gm_ext_<hash>" naming intent for a project-scoped
/// but locally-rooted extra db.
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

// Any path segment starting with "." is tool/editor/VCS/CI/agent-tooling
// metadata by convention, never indexable source -- unconditional, same rule
// as gitoutput's blanket ".*" default. Applied alongside the named SKIP_DIRS
// list (which stays, since it also matches non-dot-prefixed junk dirs like
// "node_modules"/"build"/"vendor").
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
        let is_dir_entry = !entry.contains('.');
        if gitignore_excludes(gi, &next, is_dir_entry) { continue; }
        if !is_dir_entry {
            files.push(next);
        } else {
            walk_posix(&next, max_files, files, gi);
        }
    }
}

/// Parses `source` via the treesitter plugin's `parse` verb, which walks its
/// own in-process tree_sitter::Tree (a live Tree/Node can't cross the
/// wasm-to-wasm boundary) and returns a flat JSON node list -- kind,
/// start_byte/end_byte, start_row/end_row, and a "name" field already
/// resolved server-side via child_by_field_name("name") (also something only
/// reachable while the tree is still live on the plugin side). code_index.rs
/// keeps the CHUNK_NODE_TYPES filter as its own application-level policy,
/// same as the old in-process walk did, just applied to the returned list
/// instead of to a live Node stack.
fn extract_chunks(_path: &str, source: &str, lang_name: &str) -> Vec<(String, String, usize, usize, String)> {
    let resp = call_plugin("treesitter", "parse", &json!({ "lang": lang_name, "source": source }));
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

// A single tree-sitter AST node (function/class/etc) whose body exceeds
// truncate_body's 8192-char storage cap previously had its tail silently
// dropped -- the embedding, the stored body, and the search-result snippet
// all only ever saw the first 8192 chars, so a long function's tail was
// never searchable and never shown, unlike codebasesearch's documented
// "Smart chunking: Files >1000 lines auto-split into overlapping chunks
// (200-line overlap)" behavior for large content. Split an oversized node's
// body into overlapping sub-chunks here (BEFORE truncate_body ever runs, so
// each sub-chunk individually stays under the cap) rather than at the
// per-file overlap point codebasesearch uses -- a single pathological
// function is exactly the case a whole-file split wouldn't catch, since
// most of the file's other AST nodes are already comfortably small.
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
    let resp = call_plugin("bert", "embed", &json!({ "text": text }));
    if !plugin_ok(&resp) { return None; }
    resp.get("embedding").and_then(json_to_f32_vec)
}

// BGE's query-prefix convention -- asymmetric embedding models score
// higher when the query side of a query/passage pair carries this prefix
// and the passage side doesn't. This is plugkit-side pre-processing around
// the plain "bert: embed" verb, not something the bert plugin itself needs
// to know about (mirrors crate::embed::embed_text_json_query's old
// in-process behavior, kept local to this file's own query-embedding call
// site rather than moved into the plugin).
const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

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

fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

const MANIFEST_NS: &str = "codeinsight-manifest";
// v4 adds mtime_ms, enabling a stat-only skip before any content read/hash
// for the common unchanged-file case -- old v3 manifests fail the version
// check below and are purged (one-time re-index of the whole repo, same
// cost as any other manifest-schema migration this codebase already does).
const MANIFEST_VERSION: u64 = 4;

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
    mtime_ms: f64,
    chunks: Vec<ChunkRecord>,
}

fn manifest_to_json(fp: &str, hash: u32, mtime_ms: f64, chunks: &[ChunkRecord]) -> String {
    let arr: Vec<Value> = chunks.iter().map(|c| json!({
        "key": c.key,
        "kind": c.kind,
        "name": c.name,
        "ls": c.ls,
        "le": c.le,
        "emb": c.emb,
    })).collect();
    json!({ "v": MANIFEST_VERSION, "path": fp, "hash": hash, "mtime_ms": mtime_ms, "chunks": arr }).to_string()
}

fn parse_manifest(val: &str) -> Option<(String, FileManifest)> {
    let parsed: Value = serde_json::from_str(val).ok()?;
    if parsed.get("v").and_then(|v| v.as_u64()) != Some(MANIFEST_VERSION) { return None; }
    let fp = parsed.get("path").and_then(|p| p.as_str())?.to_string();
    let hash = parsed.get("hash").and_then(|h| h.as_u64())? as u32;
    let mtime_ms = parsed.get("mtime_ms").and_then(|m| m.as_f64()).unwrap_or(0.0);
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
    Some((fp, FileManifest { hash, mtime_ms, chunks }))
}

fn purge_stale_manifest_row(row_key: &str, val: &str) {
    if let Ok(parsed) = serde_json::from_str::<Value>(val) {
        if let Some(arr) = parsed.get("chunks").and_then(|c| c.as_array()) {
            for c in arr {
                if let Some(k) = c.get("key").and_then(|x| x.as_str()) {
                    fv_delete("codeinsight", k);
                    fv_delete("codeinsight-vec", k);
                }
            }
        }
    }
    fv_delete(MANIFEST_NS, row_key);
}

fn load_manifests() -> std::collections::HashMap<String, FileManifest> {
    let mut out = std::collections::HashMap::new();
    let rows = fv_query(MANIFEST_NS, "");
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

fn chunk_rows_for_path(db_path: &str, fp: &str) -> usize {
    libsql_wasm::query_params(db_path, "SELECT COUNT(*) AS c FROM code_chunks WHERE path=?1", &[fp])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()))
        .and_then(|row| row.get("c").and_then(|v| v.as_u64()).or_else(|| row.get("c").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())))
        .unwrap_or(0) as usize
}

const INSERT_CHUNK_SQL: &str = "INSERT INTO code_chunks(path, kind, name, line_start, line_end, body, embedding) VALUES(?1,?2,?3,?4,?5,?6,vector(?7))";

fn truncate_body(body: &str) -> &str {
    let mut e = body.len().min(8192);
    while e > 0 && !body.is_char_boundary(e) { e -= 1; }
    &body[..e]
}

// Embed input gets its OWN, much smaller cap than the DB-stored body
// snippet (truncate_body's 8192 chars is sized for human-readable search
// result context, not for embedding cost). Live-witnessed this session via
// embed_text_step_timing instrumentation: an 8192-char/512-token chunk
// costs ~7.2s in model.forward ALONE (tokenize/tensor_build near-zero),
// confirmed NOT a cold-start artifact -- BERT's attention cost is
// genuinely dominant at the full MAX_TOKENS ceiling regardless of SIMD
// or warm-model state. ~1200 chars keeps most real code/prose chunks
// around 200-300 wordpiece tokens (roughly 4-6 chars/token for dense
// code, English prose runs closer to 4-5), comfortably under the
// quadratic-cost region of self-attention while still capturing a
// function's essential signature+body opening for semantic matching --
// full body content remains searchable via BM25 (which reads the
// UNtruncated DB body column) and remains fully visible in search
// result snippets (write_chunk still uses the original 8192-char
// truncate_body for storage), so this only affects what the embedding
// vector itself is computed over, not what a human/BM25 sees.
fn truncate_for_embed(body: &str) -> &str {
    let mut e = body.len().min(1200);
    while e > 0 && !body.is_char_boundary(e) { e -= 1; }
    &body[..e]
}

/// Writes one chunk row via exec_params -- the now-stateless libsql plugin
/// does its own open-prepare-bind-step-finalize-close cycle per call
/// regardless (see agentplug-libsql's src/db.rs handle() doc comment: the
/// old prepare-once/execute_bound-many handle sequence no longer exists
/// plugin-side), so there is no batching benefit left to a separate
/// prepared-statement path -- every row pays the same per-call cost either
/// way. `db_path` is the absolute path resolved once by the caller
/// (index()'s own db_path local), forwarded on every row.
fn write_chunk(libsql_ok: bool, db_path: &str, fp: &str, c: &ChunkRecord, body: &str) {
    if libsql_ok {
        let embedding_lit = vec_to_json_literal(&c.emb);
        let ls = c.ls.to_string();
        let le = c.le.to_string();
        let body_trunc = truncate_body(body);
        let params: [&str; 7] = [fp, &c.kind, &c.name, &ls, &le, body_trunc, &embedding_lit];
        let _ = libsql_wasm::exec_params(db_path, INSERT_CHUNK_SQL, &params);
    }
    // Namespace "codeinsight" (NOT "codeinsight-vec") -- host_vec_search's
    // fusion candidate lookup in codesearch() queries exactly this namespace
    // (wasm_dispatch.rs q_json.namespace="codeinsight"); writing to
    // "codeinsight-vec" left that fusion input structurally always empty,
    // silently dropping the KV-vector-search signal from every fused
    // codesearch result (see fix-codeinsight-vec-namespace-mismatch-bug).
    // clear_codeinsight()/delete_chunk_keys already scrub both namespace
    // names defensively, so this write-side rename is safe against stale
    // "codeinsight-vec" rows left by a pre-fix index.
    let emb_json = serde_json::json!({ "embedding": c.emb }).to_string();
    fv_put("codeinsight", &c.key, &emb_json);
}

fn delete_chunk_keys(chunks: &[ChunkRecord]) {
    for c in chunks {
        fv_delete("codeinsight", &c.key);
        fv_delete("codeinsight-vec", &c.key);
    }
}

pub fn index(root: &str, max_files: usize) -> Value {
    let db_path = project_db_path(None);
    // `.is_ok()` discarded the reason, leaving an intermittent libsql failure
    // visible only as a bare `libsql_ok=false` in the log -- with the digest
    // silently unwritten and every chunk read back empty as the only symptoms.
    let libsql_err = ensure_schema_at(&db_path).err().map(|e| e.to_string());
    let libsql_ok = libsql_err.is_none();
    if let Some(e) = &libsql_err {
        let msg = format!("code_index: libsql unavailable at {} -- {} (digest will not persist and chunk reads return empty)", db_path, e);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let kvvec_cleared = clear_codeinsight_if_dim_mismatch();
    if kvvec_cleared {
        let rows = fv_query(MANIFEST_NS, "");
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|k| k.as_str()) { fv_delete(MANIFEST_NS, k); }
            }
        }
    }
    let prior = load_manifests();
    let r = if root.is_empty() { "/" } else { root };
    let limit = max_files.max(50).min(2000);
    let files = collect_files(r, limit);
    {
        let msg = format!("code_index: indexing root={} files={} libsql_ok={} manifests={}", r, files.len(), libsql_ok, prior.len());
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    // Per-file batched embedding (see embed::embed_texts_batch) still cuts
    // per-file forward-pass cost the way it always did; the write side no
    // longer batches across a single begin/commit transaction or a reused
    // prepared statement -- the now-stateless shared libsql plugin opens,
    // operates, and closes on every single exec_params call regardless (see
    // agentplug-libsql's src/db.rs handle() doc comment), so there is no
    // txn/prepare amortization left to perform wasm-side. The wall budget
    // below still exists to bound a pathological repo (huge files, cold
    // model load), unrelated to the write-batching change.
    const INDEX_WALL_BUDGET_MS: u64 = 45000;
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
    // Digest entries accumulated from the same file reads this loop already
    // does, so a full completion never needs a second collect_files+host_read
    // pass just to compute current_digest() (see current_digest's own
    // doc comment for the still-separate pre-check use at the call site).
    let mut digest_entries: Vec<(String, u32)> = Vec::with_capacity(files.len());

    for raw_fp in &files {
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > INDEX_WALL_BUDGET_MS {
            deferred_files += 1;
            continue;
        }
        let canon = raw_fp.trim_start_matches("./").trim_start_matches('/').to_string();
        let fp = &canon;
        let dot = fp.rfind('.');
        let ext = match dot { Some(i) => &fp[i..], None => "" };
        let lang_name = match lang_for_ext(ext) { Some(x) => x, None => continue };

        // Stat-only fast path: if this file's mtime exactly matches the
        // prior manifest's recorded mtime AND the DB still has the expected
        // chunk-row count, skip host_read + crc32 entirely -- the single
        // biggest remaining per-file cost for the common "nothing changed"
        // case, since a full content read was previously unconditional on
        // every pass regardless of whether the file had touched since the
        // last index. Falls through to the normal read+hash path (which
        // itself still short-circuits on hash match) for any file with no
        // stat, no prior manifest, or a changed mtime -- never silently
        // skips a genuinely-changed file, since mtime is a strict
        // prerequisite check, not a substitute for the hash comparison.
        if let Some(m) = prior.get(fp) {
            if let Some(stat) = crate::wasm_dispatch::host_stat(fp)
                .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            {
                let stat_mtime = stat.get("mtime_ms").and_then(|v| v.as_f64());
                if let Some(mtime) = stat_mtime {
                    if mtime == m.mtime_ms && libsql_ok && chunk_rows_for_path(&db_path, fp) == m.chunks.len() {
                        seen.insert(fp.clone());
                        indexed += 1;
                        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
                        chunked += m.chunks.len() as i32;
                        reused += m.chunks.len() as i32;
                        reused_files += 1;
                        digest_entries.push((fp.clone(), m.hash));
                        continue;
                    }
                }
            }
        }

        let content = match host_read(fp)
            .or_else(|| host_read(raw_fp))
            .or_else(|| host_read(&format!("/{}", fp)))
        { Some(c) => c, None => continue };
        if content.len() > 256 * 1024 { continue; }
        let file_mtime = crate::wasm_dispatch::host_stat(fp)
            .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            .and_then(|s| s.get("mtime_ms").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        seen.insert(fp.clone());
        indexed += 1;
        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
        let file_hash = crc32(&content);
        let path_hash = crc32(fp);
        digest_entries.push((fp.clone(), file_hash));

        if let Some(m) = prior.get(fp) {
            if m.hash == file_hash {
                if libsql_ok && chunk_rows_for_path(&db_path, fp) == m.chunks.len() {
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

        // A single file's chunk set can itself blow past the wall budget --
        // the outer per-file elapsed() check only fires BETWEEN files, so a
        // pathological file (many/long chunks, slow unaccelerated wasm32
        // BERT inference) previously ran to completion regardless of how
        // long it took, live-witnessed this session as a single dispatch
        // taking 328s against a 45s intended budget. Cap embedding to
        // MAX_CHUNKS_PER_FILE_PER_PASS chunks per file per pass; any file
        // whose chunk count exceeds that gets its embedding work (and thus
        // its manifest write) deferred entirely to the next pass, same
        // treatment as a deferred file -- never marked `seen`, so it's
        // retried fresh rather than left in a partially-indexed state.
        // Deferring an oversized file ENTIRELY (never marking it `seen`, so
        // it retries fresh next pass) livelocks: it hits the identical cap
        // every pass, so deferred_files can never reach 0, the digest is
        // never stored, and every codesearch re-indexes the whole tree from
        // scratch -- live-witnessed as a missing .codeinsight-digest with
        // private memory climbing 397MB -> 2438MB across repeated passes on
        // a tree holding a 2MB prd.yml and several 300-700KB files. Cap the
        // work instead of discarding it: index the first
        // MAX_CHUNKS_PER_FILE_PER_PASS chunks, mark the file seen, and write
        // its manifest, so the pass still converges and the per-pass bound
        // that the cap exists to enforce still holds.
        const MAX_CHUNKS_PER_FILE_PER_PASS: usize = 64;
        let oversized = chunks.len() > MAX_CHUNKS_PER_FILE_PER_PASS;
        if oversized {
            let full = chunks.len();
            chunks.truncate(MAX_CHUNKS_PER_FILE_PER_PASS);
            let msg = format!("code_index: capping {} chunks={} -> {} (per-pass cap; file still indexed and marked seen)", fp, full, MAX_CHUNKS_PER_FILE_PER_PASS);
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
        }

        // Batch every chunk's embed input for this file into one
        // embed_texts_batch call (one candle model.forward over the whole
        // batch dimension) instead of one forward pass per chunk.
        // truncate_for_embed (not truncate_body) caps this specific input --
        // see that function's doc comment for the live-measured rationale.
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
        fv_put(MANIFEST_NS, fp, &manifest_to_json(fp, file_hash, file_mtime, &records));
    }

    let mut removed_files = 0;
    if deferred_files == 0 {
        for (fp, m) in &prior {
            if !seen.contains(fp) {
                delete_chunk_keys(&m.chunks);
                fv_delete(MANIFEST_NS, fp);
                removed_files += 1;
            }
        }
        let digest = digest_from_entries(digest_entries);
        store_digest(&digest);
        let msg = format!("code_index: done files_indexed={} chunks={} embedded={} reused={} reused_files={} removed_files={} skipped_no_embed={} digest={}", indexed, chunked, embedded, reused, reused_files, removed_files, skipped_no_embed, digest);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    } else {
        let msg = format!("code_index: partial pass (wall budget) files_indexed={} deferred_files={} embedded={} reused={} -- digest withheld, next call resumes", indexed, deferred_files, embedded, reused);
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
    let resp = call_plugin("bert", "embed_batch", &json!({ "texts": inputs }));
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
    format!("v2:{:016x}:files={}", crate::pipeline::fnv1a64(acc.as_bytes()), entries.len())
}

/// Standalone digest computation, used by callers that need to know whether
/// the index is stale BEFORE deciding to run index() at all (wasm_dispatch's
/// codesearch stale-digest check) -- necessarily a separate file-read pass
/// from index()'s own loop, since at this call site it isn't yet known
/// whether index() will run. index() itself never calls this: it accumulates
/// the same (path, hash) pairs from its own read loop and feeds them to
/// digest_from_entries directly, avoiding a second full-repo file-content
/// read on every stale-digest dispatch.
pub fn current_digest() -> String {
    let files = collect_files(".", DIGEST_MAX_FILES);
    let mut entries: Vec<(String, u32)> = Vec::new();
    for raw_fp in &files {
        let canon = raw_fp.trim_start_matches("./").trim_start_matches('/').to_string();
        let ext = match canon.rfind('.') { Some(i) => &canon[i..], None => "" };
        if lang_for_ext(ext).is_none() { continue; }
        let content = match host_read(&canon)
            .or_else(|| host_read(raw_fp))
            .or_else(|| host_read(&format!("/{}", canon)))
        { Some(c) => c, None => continue };
        if content.len() > 256 * 1024 { continue; }
        entries.push((canon, crc32(&content)));
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
    fv_delete("codeinsight", "__digest__");
}

/// A cheap, read-only summary of the ALREADY-indexed code_chunks table --
/// file count, per-kind symbol counts, and the largest files by chunk
/// count. Never triggers a scan/parse/embed pass (that only happens via
/// the codesearch verb's own index() call); this is purely a query over
/// whatever is already in the shared db, so it's safe to attach to every
/// turn-entry instruction dispatch without adding real per-dispatch cost.
/// Returns null if no index exists yet (stored_digest() is None) rather
/// than an empty-but-misleading summary.
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
    })
}

pub struct ChunkMeta {
    pub key: String,
    pub path: String,
    pub name: String,
    pub ls: usize,
    pub le: usize,
}

pub struct FusionCorpus {
    metas: Vec<ChunkMeta>,
    file_cache: std::collections::HashMap<String, Option<String>>,
}

impl FusionCorpus {
    pub fn load() -> Self {
        let mut metas = Vec::new();
        for (fp, m) in load_manifests() {
            for c in &m.chunks {
                metas.push(ChunkMeta {
                    key: c.key.clone(),
                    path: fp.clone(),
                    name: c.name.clone(),
                    ls: c.ls,
                    le: c.le,
                });
            }
        }
        FusionCorpus { metas, file_cache: std::collections::HashMap::new() }
    }

    fn file_content(&mut self, path: &str) -> Option<String> {
        if let Some(cached) = self.file_cache.get(path) { return cached.clone(); }
        let content = host_read(path).or_else(|| host_read(&format!("/{}", path)));
        self.file_cache.insert(path.to_string(), content.clone());
        content
    }

    /// Map a (path, line_start) pair -- the shape `search()`'s vector rows come
    /// back in -- to the `ci-<path_hash>-<file_hash>-<idx>` key the fusion
    /// ranker works in. Without this the vector half cannot contribute to
    /// fusion at all, since the two stores identify the same chunk differently.
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

/// Filename-token-overlap fallback, used only when embedding is unavailable
/// (embed model failed to load, or the git_commit_vectors table has zero
/// usable rows for this query) -- never the primary ranking path.
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

/// Top-k commit hashes ranked by cosine similarity of a diff+message
/// embedding against the query embedding -- the git_commit_vectors table is
/// synced incrementally (wall-budgeted) on each call so a large backlog never
/// blocks a single dispatch. Falls back to plain filename-token overlap if
/// the embed model is unavailable or the table has nothing usable yet.
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
            let like = format!("%{}%", sql_quote(query));
            let sql2 = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM code_chunks WHERE body LIKE '{}' OR name LIKE '{}' LIMIT {}", like, like, k);
            match libsql_wasm::query(&db_path, &sql2) {
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
