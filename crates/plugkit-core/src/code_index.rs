#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::wasm_dispatch::{host_read, host_stat, unpack_to_value_pub, plugin_call, plugin_ok, plugin_failure_code};
use crate::vecstore::{drop_if_dim_mismatch_at_cfg as drop_if_dim_mismatch_cfg, vec_to_json_literal};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
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
    let edge_ns = edges_ns();
    let edge_rows = fv_query(&edge_ns, "");
    if let Some(arr) = edge_rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete(&edge_ns, key);
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

fn clear_codeinsight_if_dim_mismatch(project_path: Option<&str>) -> bool {
    clear_codeinsight_if_dim_mismatch_cfg(&crate::ragconfig::RagConfig::default(), project_path)
}

fn clear_codeinsight_if_dim_mismatch_cfg(cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> bool {
    let vec_ns = format!("{}{}", cfg.namespaces.vec_namespace(&cfg.namespaces.code), root_ns_suffix(project_path));
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
    if !cfg.embed.should_drop_table_for_dim_mismatch(&vec_ns, old_dim) {
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
        ".yaml" | ".yml" => Some("yaml"),
        ".toml" => Some("toml"),
        ".sql" => Some("sql"),
        ".lua" => Some("lua"),
        ".kt" | ".kts" => Some("kotlin"),
        ".swift" => Some("swift"),
        ".zig" => Some("zig"),
        ".ex" | ".exs" => Some("elixir"),
        ".scala" | ".sc" => Some("scala"),
        ".pl" | ".pm" => Some("perl"),
        ".r" => Some("r"),
        ".m" | ".mm" => Some("objc"),
        ".xml" => Some("xml"),
        ".ini" | ".cfg" | ".conf" => Some("toml"),
        ".dockerfile" => Some("dockerfile"),
        ".graphql" | ".gql" => Some("graphql"),
        ".proto" => Some("proto"),
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
    // Rust build artifacts. ".pdb" and ".lib" were already here but ".rlib"
    // and ".rmeta" were not, and they carry readable symbol names: an
    // exhaustive literal scan of C:/dev/litebox-main matched "set_times_at"
    // 10 extra times inside target-myfork/*.rlib/.rmeta/.pdb, which are not
    // call sites. A sibling build dir whose name is not the literal "target"
    // (target-myfork here) is not caught by SKIP_DIRS, so the suffix is the
    // only thing that excludes it.
    ".rlib", ".rmeta",
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

fn is_skipped_filename(name: &str, cfg: &crate::ragconfig::IndexConfig) -> bool {
    cfg.skips_filename(name, SKIP_FILE_SUFFIXES)
}

pub(crate) fn is_skipped_dir_segment(seg: &str, cfg: &crate::ragconfig::IndexConfig) -> bool {
    cfg.skips_dir_segment(seg, SKIP_DIRS)
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
    let _ = drop_if_dim_mismatch_cfg(path, &cfg.legacy_memories_alongside_code_chunks.table, &cfg.embed);
    libsql_wasm::exec(path, &format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, body TEXT, embedding F32_BLOB({}))",
        cfg.code_chunks.table, cfg.dim()
    ))?;
    libsql_wasm::exec(path, &format!(
        "CREATE TABLE IF NOT EXISTS {} (id INTEGER PRIMARY KEY, namespace TEXT, text TEXT, ts INTEGER, embedding F32_BLOB({}))",
        cfg.legacy_memories_alongside_code_chunks.table, cfg.dim()
    ))?;
    crate::vecns::VecTableSpec::from_names(path, &cfg.code_chunks).ensure_index();
    crate::vecns::VecTableSpec::from_names(path, &cfg.legacy_memories_alongside_code_chunks).ensure_index();
    // Table-scoped, not the shared/global marker: this function only ever
    // checked and (if needed) dropped these two SPECIFIC tables above, so it
    // only records completion for those two, not for every table any other
    // store (rssearch_vectors, git_commit_vectors) separately owns. See
    // embed_marker.rs's marker_rel_for_table doc comment for the false-
    // negative this closes.
    crate::embed_marker::record_embed_generation_for_table(&cfg.code_chunks.table);
    crate::embed_marker::record_embed_generation_for_table(&cfg.legacy_memories_alongside_code_chunks.table);
    Ok(())
}

fn project_db_filename(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("ext-{:x}.db", crc32(p)),
        _ => crate::ragconfig::RagConfig::resolved().db_path.db_filename,
    }
}

/// Live at `<project_path>/.gm/gm.db`, not a crc32-hashed file inside the
/// CURRENT project's own `.gm/` -- a cache under the target folder's own
/// `.gm/` is reusable by any other project/session that later points at that
/// same folder, matching how the current project's own index is reusable.
/// `project_db_filename`'s crc32-hash naming stays live for callers that pass
/// a project_path but want the digest/memorize-style co-located cache
/// (memorize_at_finalize et al.) -- only code_index's own db path resolution
/// moved to the per-root `.gm/` layout.
pub(crate) fn project_db_path(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => {
            let root = p.trim_end_matches(['/', '\\']);
            let cfg = crate::ragconfig::RagConfig::resolved();
            format!("{}/{}/{}", root, cfg.db_path.state_root_dir, cfg.db_path.db_filename)
        }
        _ => libsql_wasm::absolute_db_path(&project_db_filename(None)),
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
    ensure_schema_at(&project_db_path(None))
}

fn ensure_schema_for(project_path: Option<&str>) -> Result<String, String> {
    let path = project_db_path(project_path);
    ensure_schema_at(&path)?;
    Ok(path)
}

pub(crate) fn list_dir(path: &str) -> Vec<String> {
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value_pub(packed);
    match v {
        Value::Array(arr) => arr.into_iter().filter_map(|x| {
            let entry = if let Some(s) = x.as_str() { s } else {
                x.get("name").or_else(|| x.get("path")).or_else(|| x.get("file"))
                    .and_then(|n| n.as_str())?
            };
            if !is_safe_readdir_child_name(entry) {
                return None;
            }
            Some(entry.to_string())
        }).collect(),
        _ => Vec::new(),
    }
}

fn is_safe_readdir_child_name(entry: &str) -> bool {
    !entry.is_empty()
        && !entry.starts_with('/')
        && entry
            .split('/')
            .all(|segment| segment != "." && segment != "..")
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

type GitignoreMemoKey = (String, Option<String>, Option<String>);
static GITIGNORE_MEMO: std::sync::Mutex<Option<(GitignoreMemoKey, Option<ignore::gitignore::Gitignore>)>> =
    std::sync::Mutex::new(None);

fn build_repo_gitignore(
    root: &str,
    gitignore_content: Option<&str>,
    custom_content: Option<&str>,
) -> Option<ignore::gitignore::Gitignore> {
    if gitignore_content.is_none() && custom_content.is_none() { return None; }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for content in [gitignore_content, custom_content].into_iter().flatten() {
        for line in content.lines() {
            let _ = builder.add_line(None, line);
        }
    }
    builder.build().ok()
}

pub(crate) fn load_repo_gitignore(root: &str) -> Option<ignore::gitignore::Gitignore> {
    let gitignore_content = host_read(&ignore_file_path(root, ".gitignore"));
    let custom_content = host_read(&ignore_file_path(root, ".codesearchignore"));
    let key: GitignoreMemoKey = (root.to_string(), gitignore_content.clone(), custom_content.clone());
    let mut memo = GITIGNORE_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_key, cached)) = memo.as_ref() {
        if *cached_key == key { return cached.clone(); }
    }
    let built = build_repo_gitignore(root, gitignore_content.as_deref(), custom_content.as_deref());
    *memo = Some((key, built.clone()));
    built
}

pub(crate) fn gitignore_excludes(gi: &Option<ignore::gitignore::Gitignore>, rel_path: &str, is_dir: bool) -> bool {
    match gi {
        Some(g) => g.matched(rel_path, is_dir).is_ignore(),
        None => false,
    }
}

pub(crate) fn is_hidden_segment(seg: &str) -> bool {
    seg.starts_with('.') && seg != "." && seg != ".."
}

pub(crate) fn collect_files(root: &str, max_files: usize, cfg: &crate::ragconfig::IndexConfig) -> Vec<String> {
    let gi = load_repo_gitignore(root);
    let entries = list_dir(root);
    if entries.is_empty() { return Vec::new(); }
    let has_slashes = entries.iter().any(|e| e.contains('/'));
    if has_slashes {
        return entries.into_iter()
            .filter(|p| {
                if cfg.is_force_included(p) { return true; }
                if p.split('/').any(is_hidden_segment) { return false; }
                if p.split('/').any(|seg| is_skipped_dir_segment(seg, cfg)) { return false; }
                let name = p.rsplit('/').next().unwrap_or(p.as_str());
                if is_skipped_filename(name, cfg) { return false; }
                !gitignore_excludes(&gi, p, false)
            })
            .take(max_files)
            .collect();
    }
    let mut files = Vec::new();
    walk_posix(root, max_files, &mut files, &gi, cfg);
    files
}

fn walk_posix(root: &str, max_files: usize, files: &mut Vec<String>, gi: &Option<ignore::gitignore::Gitignore>, cfg: &crate::ragconfig::IndexConfig) {
    if files.len() >= max_files { return; }
    if !cfg.is_force_included(root)
        && root.split('/').any(|seg| is_skipped_dir_segment(seg, cfg))
    { return; }
    walk_children(root, root.len(), max_files, files, gi, cfg);
}

fn walk_children(dir: &str, walk_base_len: usize, max_files: usize, files: &mut Vec<String>, gi: &Option<ignore::gitignore::Gitignore>, cfg: &crate::ragconfig::IndexConfig) {
    if files.len() >= max_files { return; }
    let dir_force_included = cfg.is_force_included(dir);
    for entry in list_dir(dir) {
        if files.len() >= max_files { return; }
        let next = if dir.ends_with('/') { format!("{}{}", dir, entry) } else { format!("{}/{}", dir, entry) };
        let force_included = dir_force_included || cfg.is_force_included(&next);
        if !force_included {
            if is_hidden_segment(&entry) { continue; }
            if is_skipped_filename(&entry, cfg) { continue; }
        }
        let is_dir_entry = host_stat(&next)
            .and_then(|v| v.get("isDirectory").and_then(|b| b.as_bool()))
            .unwrap_or_else(|| !entry.contains('.'));
        if !force_included && gitignore_excludes(gi, &next, is_dir_entry) { continue; }
        if !is_dir_entry {
            files.push(next);
            continue;
        }
        let below_walk_base = next.get(walk_base_len..).unwrap_or("");
        if !cfg.is_force_included(&next) && below_walk_base.split('/').any(|seg| is_skipped_dir_segment(seg, cfg)) {
            continue;
        }
        walk_children(&next, walk_base_len, max_files, files, gi, cfg);
    }
}

pub fn extract_chunks(_path: &str, source: &str, lang_name: &str) -> Vec<(String, String, usize, usize, String)> {
    extract_chunks_reporting_plugin_failure(_path, source, lang_name).0
}

pub fn extract_chunks_reporting_plugin_failure(_path: &str, source: &str, lang_name: &str) -> (Vec<(String, String, usize, usize, String)>, bool) {
    let resp = plugin_call("treesitter", "parse", &json!({ "lang": lang_name, "source": source }));
    if !plugin_ok(&resp) {
        crate::wasm_dispatch::emit_event("code_index_treesitter_failed", json!({
            "lang": lang_name,
            "plugin_failure": plugin_failure_code(&resp),
            "source_len": source.len(),
        }));
        return (Vec::new(), true);
    }
    let nodes = match resp.get("nodes").and_then(|v| v.as_array()) {
        Some(n) => n,
        None => {
            crate::wasm_dispatch::emit_event("code_index_treesitter_failed", json!({
                "lang": lang_name,
                "plugin_failure": crate::wasm_dispatch::PLUGIN_FAIL_MALFORMED,
                "source_len": source.len(),
            }));
            return (Vec::new(), true);
        }
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
    (out, false)
}

const CALL_NODE_TYPES: &[&str] = &[
    "call_expression",
    "method_call_expression",
    "call",
];

const CALLEE_LEAF_NODE_TYPES: &[&str] = &[
    "identifier",
    "field_identifier",
    "property_identifier",
    "scoped_identifier",
    "shorthand_field_identifier",
];

#[derive(Clone, Copy)]
struct RawNode<'a> {
    kind: &'a str,
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
}

fn parsed_nodes<'a>(nodes: &'a [Value]) -> Vec<RawNode<'a>> {
    nodes.iter().filter_map(|node| {
        let kind = node.get("kind").and_then(|v| v.as_str())?;
        let start_byte = node.get("start_byte").and_then(|v| v.as_u64())? as usize;
        let end_byte = node.get("end_byte").and_then(|v| v.as_u64())? as usize;
        let start_row = node.get("start_row").and_then(|v| v.as_u64())? as usize;
        Some(RawNode { kind, start_byte, end_byte, start_row })
    }).collect()
}

fn callee_name_for_call(call: &RawNode, all: &[RawNode], src_bytes: &[u8]) -> Option<String> {
    let mut best: Option<RawNode> = None;
    for candidate in all {
        if candidate.start_byte < call.start_byte || candidate.end_byte > call.end_byte { continue; }
        if candidate.start_byte == call.start_byte && candidate.end_byte == call.end_byte { continue; }
        if !CALLEE_LEAF_NODE_TYPES.contains(&candidate.kind) { continue; }
        let is_earlier = match &best {
            Some(b) => candidate.end_byte < b.end_byte,
            None => true,
        };
        if is_earlier { best = Some(*candidate); }
    }
    let leaf = best?;
    let end = leaf.end_byte.min(src_bytes.len());
    if end <= leaf.start_byte { return None; }
    let text = String::from_utf8_lossy(&src_bytes[leaf.start_byte..end]).into_owned();
    let simple_name = text.rsplit(['.', ':']).next().unwrap_or(&text).to_string();
    if simple_name.is_empty() { None } else { Some(simple_name) }
}

pub struct CallEdge {
    pub caller_symbol: String,
    pub callee_symbol: String,
    pub line: usize,
}

pub fn extract_call_edges(source: &str, lang_name: &str, chunks: &[(String, String, usize, usize, String)]) -> Vec<CallEdge> {
    let resp = plugin_call("treesitter", "parse", &json!({ "lang": lang_name, "source": source }));
    if !plugin_ok(&resp) { return Vec::new(); }
    let nodes_json = match resp.get("nodes").and_then(|v| v.as_array()) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let all = parsed_nodes(nodes_json);
    let src_bytes = source.as_bytes();
    let mut enclosing_by_line: Vec<(usize, usize, &str)> = chunks.iter()
        .filter(|(kind, _, _, _, _)| kind.contains("function") || kind.contains("method"))
        .map(|(_, name, ls, le, _)| (*ls, *le, name.as_str()))
        .collect();
    enclosing_by_line.sort_by_key(|(ls, le, _)| (*ls, *le));
    let find_enclosing = |row_1based: usize| -> Option<&str> {
        enclosing_by_line.iter()
            .filter(|(ls, le, _)| row_1based >= *ls && row_1based <= *le)
            .min_by_key(|(ls, le, _)| le.saturating_sub(*ls))
            .map(|(_, _, name)| *name)
    };
    let mut out = Vec::new();
    for call in all.iter().filter(|n| CALL_NODE_TYPES.contains(&n.kind)) {
        let line = call.start_row + 1;
        let Some(caller_symbol) = find_enclosing(line) else { continue };
        if caller_symbol.is_empty() { continue; }
        let Some(callee_symbol) = callee_name_for_call(call, &all, src_bytes) else { continue };
        out.push(CallEdge { caller_symbol: caller_symbol.to_string(), callee_symbol, line });
    }
    out
}

/// One kv row per FILE holding every call edge it contains. The previous
/// layout (one row per edge, deleted by scanning the whole namespace for
/// rows naming the path) cost a full-namespace read per indexed file, which
/// made extraction O(files x edges): measured at ~0.5s per file on a 2400
/// file monorepo, where it consumed the entire wall budget on its own.
fn edges_ns() -> String {
    format!("{}-call-edges", code_ns())
}

fn legacy_per_edge_ns() -> String {
    format!("{}-edges", code_ns())
}

fn purge_legacy_per_edge_rows() {
    let ns = legacy_per_edge_ns();
    let rows = fv_query(&ns, "");
    if let Some(arr) = rows.as_array() {
        for row in arr {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                fv_delete(&ns, key);
            }
        }
    }
}

fn edges_key(path: &str) -> String {
    format!("ce-{:x}", crc32(path))
}

pub fn write_call_edges(path: &str, edges: &[CallEdge]) {
    if edges.is_empty() { return; }
    let rows: Vec<Value> = edges.iter().map(|e| json!({
        "caller_path": path,
        "caller_symbol": e.caller_symbol,
        "callee_symbol": e.callee_symbol,
        "line": e.line,
    })).collect();
    fv_put(&edges_ns(), &edges_key(path), &Value::Array(rows).to_string());
}

pub fn delete_call_edges_for_path(path: &str) {
    fv_delete(&edges_ns(), &edges_key(path));
}

fn all_call_edges() -> Vec<Value> {
    let ns = edges_ns();
    let rows = fv_query(&ns, "");
    rows.as_array()
        .map(|arr| arr.iter().filter_map(|row| {
            row.get("value").and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok())
        }).flat_map(|v| match v {
            Value::Array(edges) => edges,
            other => vec![other],
        }).collect())
        .unwrap_or_default()
}

pub fn callers_of(symbol: &str) -> Value {
    let edges = all_call_edges();
    let hits: Vec<Value> = edges.into_iter()
        .filter(|e| e.get("callee_symbol").and_then(|v| v.as_str()) == Some(symbol))
        .collect();
    json!({ "symbol": symbol, "edges": hits })
}

pub fn callees_of(symbol: &str) -> Value {
    let edges = all_call_edges();
    let hits: Vec<Value> = edges.into_iter()
        .filter(|e| e.get("caller_symbol").and_then(|v| v.as_str()) == Some(symbol))
        .collect();
    json!({ "symbol": symbol, "edges": hits })
}

pub fn impact_of(symbol: &str, max_depth: usize) -> Value {
    let depth_cap = max_depth.clamp(1, 10);
    let edges = all_call_edges();
    let mut visited: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    visited.insert(symbol.to_string(), 0);
    let mut frontier = vec![symbol.to_string()];
    for depth in 1..=depth_cap {
        let mut next_frontier = Vec::new();
        for caller in &frontier {
            for e in &edges {
                let Some(caller_symbol) = e.get("caller_symbol").and_then(|v| v.as_str()) else { continue };
                if caller_symbol != caller { continue };
                let Some(callee_symbol) = e.get("callee_symbol").and_then(|v| v.as_str()) else { continue };
                if visited.contains_key(callee_symbol) { continue; }
                visited.insert(callee_symbol.to_string(), depth);
                next_frontier.push(callee_symbol.to_string());
            }
        }
        if next_frontier.is_empty() { break; }
        frontier = next_frontier;
    }
    let mut reachable: Vec<Value> = visited.into_iter()
        .filter(|(sym, _)| sym != symbol)
        .map(|(sym, depth)| json!({ "symbol": sym, "depth": depth }))
        .collect();
    reachable.sort_by_key(|v| (
        v.get("depth").and_then(|d| d.as_u64()).unwrap_or(0),
        v.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string(),
    ));
    json!({ "symbol": symbol, "max_depth": depth_cap, "reachable": reachable })
}

// Overlap is derived, not independently configured: it must stay strictly
// below the split threshold or  underflows and the splitter loops.
// Ten percent keeps that invariant true for any threshold a caller sets.
fn oversized_chunk_overlap(threshold: usize) -> usize {
    (threshold / 10).max(1).min(threshold.saturating_sub(1))
}

fn split_oversized_chunk(
    kind: &str,
    name: &str,
    line_start: usize,
    line_end: usize,
    body: &str,
) -> Vec<(String, String, usize, usize, String)> {
    let split_threshold = crate::ragconfig::RagConfig::resolved().index.split_chunk_above_bytes.max(2);
    let overlap = oversized_chunk_overlap(split_threshold);
    if body.len() <= split_threshold {
        return vec![(kind.to_string(), name.to_string(), line_start, line_end, body.to_string())];
    }
    let total_lines = line_end.saturating_sub(line_start).max(1);
    let bytes_per_line = (body.len() as f64 / total_lines as f64).max(1.0);
    let stride = split_threshold - overlap;
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut part = 0usize;
    while start < body.len() {
        let mut end = (start + split_threshold).min(body.len());
        while end > start && !body.is_char_boundary(end) { end -= 1; }
        let sub_body = &body[start..end];
        let sub_line_start = line_start + ((start as f64 / bytes_per_line) as usize);
        let sub_line_end = line_start + ((end as f64 / bytes_per_line) as usize);
        let sub_name = if part == 0 { name.to_string() } else { format!("{}#part{}", name, part + 1) };
        out.push((kind.to_string(), sub_name, sub_line_start, sub_line_end.max(sub_line_start), sub_body.to_string()));
        if end >= body.len() { break; }
        let mut next_start = end.saturating_sub(overlap);
        while next_start > 0 && !body.is_char_boundary(next_start) { next_start -= 1; }
        start = next_start.max(start + stride.min(1));
        part += 1;
    }
    out
}

fn embed_text(text: &str) -> Option<Vec<f32>> {
    let resp = plugin_call("bert", "embed", &json!({ "text": text }));
    if !plugin_ok(&resp) {
        crate::wasm_dispatch::emit_event("code_index_embed_failed", json!({
            "plugin_failure": plugin_failure_code(&resp),
            "text_len": text.len(),
        }));
        return None;
    }
    resp.get("embedding").and_then(json_to_f32_vec)
}

fn embed_text_json_query(query_text: &str) -> Option<Value> {
    let trimmed = query_text.trim();
    if trimmed.is_empty() { return None; }
    let v = embed_text(&crate::embed::condition_query(trimmed))?;
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

fn indexing_pipeline_namespace_config_unthreaded_default() -> crate::ragconfig::NamespaceConfig {
    crate::ragconfig::NamespaceConfig::default()
}

/// KV namespaces (manifest/code/vec) live in a host-side store keyed by
/// namespace string alone, independent of the libsql db path -- so a
/// per-root db path fix without a matching namespace salt would still mix
/// every root's manifests/embeddings into the same KV rows. `project_path`
/// salts the namespace with the same crc32 tag `project_db_filename` uses,
/// keeping the default (no project_path) namespace byte-identical to the
/// pre-existing behaviour.
fn root_ns_suffix(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("__root{:x}", crc32(p)),
        _ => String::new(),
    }
}

fn manifest_ns_for(project_path: Option<&str>) -> String {
    format!("{}{}", indexing_pipeline_namespace_config_unthreaded_default().manifest_namespace(), root_ns_suffix(project_path))
}

fn code_ns_for(project_path: Option<&str>) -> String {
    format!("{}{}", indexing_pipeline_namespace_config_unthreaded_default().code, root_ns_suffix(project_path))
}

fn code_vec_ns_for(project_path: Option<&str>) -> String {
    let ns = indexing_pipeline_namespace_config_unthreaded_default();
    format!("{}{}", ns.vec_namespace(&ns.code), root_ns_suffix(project_path))
}

fn manifest_ns() -> String {
    manifest_ns_for(None)
}

fn code_ns() -> String {
    code_ns_for(None)
}

fn code_vec_ns() -> String {
    let ns = indexing_pipeline_namespace_config_unthreaded_default();
    ns.vec_namespace(&ns.code)
}

const MANIFEST_VERSION: u64 = 7;

fn summary_ns_for(project_path: Option<&str>) -> String {
    format!("codeinsight-summary{}", root_ns_suffix(project_path))
}

#[derive(Clone)]
struct ChunkRecord {
    key: String,
    kind: String,
    name: String,
    ls: usize,
    le: usize,
    emb: Vec<f32>,
    content_hash: u32,
}

#[derive(Clone)]
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
    /// Byte size at last index, alongside mtime_ms for the stat-only fast
    /// path's change-detection guard. mtime ALONE is not a safe cache key: a
    /// coarse-granularity filesystem (FAT32's 2s resolution) or a fast
    /// automated restore can reproduce an identical mtime on genuinely
    /// changed content. Size does not catch every edit either (a same-length
    /// in-place byte change), but combined with the existing chunk-row-count
    /// check it costs nothing extra -- host_stat already returns size on the
    /// same call mtime comes from -- so there is no reason not to use it.
    /// Optional for the same backward-compat reason as digest_hash: older
    /// manifest rows without it simply skip this check once.
    size: Option<u64>,
    commit_overview: Option<String>,
    /// A chunk with an empty `emb` is extracted but not yet embedded: it is
    /// absent from the chunk table, served by BM25/symbol lookup from this
    /// manifest, and picked up by the next pass's embed loop.
    chunks: Vec<ChunkRecord>,
    /// Manifests older than v7 recorded chunks that failed to embed as this
    /// count instead of as pending chunks; >0 forces a full re-extraction
    /// once, which rewrites the row in the current shape.
    skipped_no_embed: u32,
}

impl FileManifest {
    fn pending_chunks(&self) -> usize {
        self.chunks.iter().filter(|c| c.emb.is_empty()).count()
    }
}

#[derive(Default)]
struct IndexSummary {
    files: i64,
    symbols: i64,
    pending_embed_chunks: i64,
    pending_embed_files: i64,
    kinds: std::collections::HashMap<String, i64>,
}

impl IndexSummary {
    fn apply(&mut self, m: &FileManifest, sign: i64) {
        if m.chunks.is_empty() { return; }
        let pending = m.pending_chunks() as i64;
        self.files += sign;
        self.symbols += sign * m.chunks.len() as i64;
        self.pending_embed_chunks += sign * pending;
        if pending > 0 { self.pending_embed_files += sign; }
        for c in &m.chunks {
            *self.kinds.entry(c.kind.clone()).or_insert(0) += sign;
        }
    }

    fn to_json(&self) -> String {
        let mut kinds: Vec<(&String, &i64)> = self.kinds.iter().filter(|(_, c)| **c > 0).collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let by_kind: Vec<Value> = kinds.into_iter().take(10).map(|(k, c)| json!({ "kind": k, "c": c })).collect();
        json!({
            "files": self.files.max(0),
            "symbols": self.symbols.max(0),
            "pending_embed_chunks": self.pending_embed_chunks.max(0),
            "pending_embed_files": self.pending_embed_files.max(0),
            "by_kind": by_kind,
        }).to_string()
    }
}

fn load_index_summary(project_path: Option<&str>) -> Option<Value> {
    let rows = fv_query(&summary_ns_for(project_path), "");
    rows.as_array()?.iter().find_map(|row| {
        row.get("value").and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok())
    })
}

fn manifest_to_json(fp: &str, m: &FileManifest) -> String {
    let arr: Vec<Value> = m.chunks.iter().map(|c| json!({
        "key": c.key,
        "kind": c.kind,
        "name": c.name,
        "ls": c.ls,
        "le": c.le,
        "emb": c.emb,
        "ch": c.content_hash,
    })).collect();
    json!({ "v": MANIFEST_VERSION, "path": fp, "hash": m.hash, "digest_hash": m.digest_hash, "mtime_ms": m.mtime_ms, "size": m.size, "commit_overview": m.commit_overview, "chunks": arr, "skipped_no_embed": m.skipped_no_embed }).to_string()
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
    let size = parsed.get("size").and_then(|s| s.as_u64());
    let commit_overview = parsed.get("commit_overview").and_then(|v| v.as_str()).map(String::from);
    let arr = parsed.get("chunks").and_then(|c| c.as_array())?;
    let mut chunks = Vec::with_capacity(arr.len());
    for c in arr {
        let key = c.get("key").and_then(|x| x.as_str())?.to_string();
        let kind = c.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ls = c.get("ls").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let le = c.get("le").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
        let emb = c.get("emb").and_then(json_to_f32_vec).unwrap_or_default();
        // Absent on manifests written before v6 (content_hash didn't exist yet);
        // 0 is not a valid fnv1a64-derived u32 output for any real chunk body in
        // practice-safe terms here, and reuse-by-hash simply never matches it, so
        // that one chunk falls back to the pre-existing "re-embed whole file"
        // path once until it is naturally rewritten with a real hash.
        let content_hash = c.get("ch").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        chunks.push(ChunkRecord { key, kind, name, ls, le, emb, content_hash });
    }
    let skipped_no_embed = parsed.get("skipped_no_embed").and_then(|s| s.as_u64()).unwrap_or(0) as u32;
    Some((fp, FileManifest { hash, digest_hash, mtime_ms, size, commit_overview, chunks, skipped_no_embed }))
}

/// Whether a path lives inside a submodule, so per-file git history is skipped.
///
/// Derived from `.gitmodules` via the same helper the submodules gate uses,
/// rather than a hardcoded list of THIS repo's own sibling names. That list was
/// the identical vacuous-vocabulary shape already fixed in the gate: for any
/// other project it matched nothing, so every file in a real submodule paid a
/// `git log -1` subprocess whose output describes the wrong repository.
///
/// Matches on any path SEGMENT rather than only the first, since a submodule is
/// frequently nested (`client/vendor/wireweave`, which is exactly what this
/// project declares).
fn is_submodule_path(fp: &str) -> bool {
    let paths = crate::orchestrator::submodule_drift::submodule_paths();
    if paths.is_empty() {
        return false;
    }
    let norm = fp.replace('\\', "/");
    paths.iter().any(|p| {
        let p = p.trim_matches('/');
        !p.is_empty() && (norm == p || norm.starts_with(&format!("{p}/")))
    })
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

fn load_manifests(project_path: Option<&str>) -> std::collections::HashMap<String, FileManifest> {
    let mut out = std::collections::HashMap::new();
    let rows = fv_query(&manifest_ns_for(project_path), "");
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
    let rows = match libsql_wasm::query(db_path, &format!("SELECT path, COUNT(*) AS c FROM {} GROUP BY path", chunks_table())) {
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

fn chunks_table() -> String {
    crate::ragconfig::RagConfig::resolved().code_chunks.table
}

fn insert_chunk_sql() -> String {
    format!("INSERT INTO {}(path, kind, name, line_start, line_end, body, embedding) VALUES(?1,?2,?3,?4,?5,?6,vector(?7))", chunks_table())
}

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

/// Returns false when the chunk was meant to reach libsql and did not.
///
/// The caller writes a manifest asserting which chunks are indexed. Dropping
/// this result let that manifest claim a chunk the insert had rejected, so the
/// manifest and code_chunks disagreed permanently and the file re-processed on
/// every pass with nothing surfaced.
fn write_chunk(libsql_ok: bool, db_path: &str, fp: &str, c: &ChunkRecord, body: &str, project_path: Option<&str>) -> bool {
    let mut persisted = true;
    if libsql_ok {
        let embedding_lit = vec_to_json_literal(&c.emb);
        let ls = c.ls.to_string();
        let le = c.le.to_string();
        let body_trunc = truncate_body(body);
        let params: [&str; 7] = [fp, &c.kind, &c.name, &ls, &le, body_trunc, &embedding_lit];
        let cfg = crate::ragconfig::RagConfig::resolved();
        let spec = crate::vecns::VecTableSpec::from_names(db_path, &cfg.code_chunks);
        if let Err(e) = crate::vecns::exec_with_shadow_row_recovery(&spec, &insert_chunk_sql(), &params, |recovery_err| {
            crate::wasm_dispatch::emit_event("code_index_chunk_shadow_row_recovery", serde_json::json!({
                "path": fp,
                "chunk_key": c.key,
                "error": recovery_err,
            }));
        }) {
            persisted = false;
            crate::wasm_dispatch::emit_event("code_index_chunk_insert_failed", serde_json::json!({
                "path": fp,
                "chunk_key": c.key,
                "line_start": c.ls,
                "line_end": c.le,
                "error": e,
                "reason": "the chunk did not reach code_chunks; its file's manifest is being withheld so the next pass retries instead of recording an index that is not there",
            }));
        }
    }
    let emb_json = serde_json::json!({ "embedding": c.emb }).to_string();
    fv_put(&code_ns_for(project_path), &c.key, &emb_json);
    persisted
}

fn delete_chunk_keys(chunks: &[ChunkRecord], project_path: Option<&str>) {
    for c in chunks {
        fv_delete(&code_ns_for(project_path), &c.key);
        fv_delete(&code_vec_ns_for(project_path), &c.key);
    }
}

pub fn default_index_file_limit() -> usize {
    crate::ragconfig::RagConfig::resolved().index.digest_max_files
}

/// Source code is indexed before prose and before data/config files, so a
/// tree too large for one pass yields code symbols first: a monorepo whose
/// walk order put 1300 markdown files ahead of its 1400 JS files spent every
/// pass on docs and answered code queries with section hits only.
fn index_priority(path: &str) -> u8 {
    let ext = match path.rfind('.') { Some(i) => &path[i..], None => "" };
    match lang_for_ext(ext) {
        Some("markdown") => 1,
        Some("json") | Some("yaml") | Some("toml") | Some("xml") | Some("html") | Some("css") => 2,
        Some(_) => 0,
        None => 3,
    }
}

pub fn index(root: &str, max_files: usize) -> Value {
    index_cfg(root, max_files, &crate::ragconfig::RagConfig::resolved())
}

/// Same as [`index`], targeting an explicit project root -- its manifest,
/// chunk, and digest state live under `<project_path>/.gm/gm.db` and a
/// crc32-salted KV namespace, isolated from the current project's own index
/// so indexing a submodule/sibling never mixes into or overwrites it.
pub fn index_at(root: &str, max_files: usize, project_path: &str) -> Value {
    index_cfg_impl(root, max_files, &crate::ragconfig::RagConfig::resolved(), false, 20, Some(project_path))
}

/// Same as [`index`], but always runs the likely-orphaned-symbol scan
/// regardless of `index.likely_orphaned_symbol_scan_enabled` -- the config
/// flag defaults off because the scan is O(candidates) extra queries and
/// this codepath also backs the always-on `codeinsight_overview` hot path
/// fired on every `instruction` dispatch; an explicit opt-in dispatch
/// (`codeinsight_index {"dead_code": true}`) is the correct place to pay
/// that cost on demand, matching `scan_deps`'s `{"full": true}` pattern --
/// cheap by default, exhaustive when actually asked for.
pub fn index_with_dead_code(root: &str, max_files: usize, limit: usize) -> Value {
    let mut out = index_cfg_impl(root, max_files, &crate::ragconfig::RagConfig::resolved(), true, limit, None);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("dead_code_scan_forced".to_string(), json!(true));
    }
    out
}

/// Same as [`index`], with the knowledgebase config supplied explicitly --
/// matching the `_cfg` convention every other config-aware entry point in this
/// module already follows.
pub fn index_cfg(root: &str, max_files: usize, cfg: &crate::ragconfig::RagConfig) -> Value {
    index_cfg_impl(root, max_files, cfg, cfg.index.likely_orphaned_symbol_scan_enabled, 20, None)
}

fn index_cfg_impl(root: &str, max_files: usize, cfg: &crate::ragconfig::RagConfig, include_dead_code: bool, orphan_scan_limit: usize, project_path: Option<&str>) -> Value {
    let pass_started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let db_path = project_db_path(project_path);
    let libsql_err = ensure_schema_at(&db_path).err().map(|e| e.to_string());
    let libsql_ok = libsql_err.is_none();
    if let Some(e) = &libsql_err {
        let msg = format!("code_index: libsql unavailable at {} -- {} (digest will not persist and chunk reads return empty)", db_path, e);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let kvvec_cleared = clear_codeinsight_if_dim_mismatch(project_path);
    if kvvec_cleared {
        let rows = fv_query(&manifest_ns_for(project_path), "");
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|k| k.as_str()) { fv_delete(&manifest_ns_for(project_path), k); }
            }
        }
    }
    let prior = load_manifests(project_path);
    purge_legacy_per_edge_rows();
    // Hoisted out of the per-file loop: one GROUP BY instead of one full
    // open/query/close per file (see chunk_rows_by_path).
    let chunk_counts = if libsql_ok {
        chunk_rows_by_path(&db_path)
    } else {
        std::collections::HashMap::new()
    };
    let chunk_rows = |fp: &str| -> usize { chunk_counts.get(fp).copied().unwrap_or(0) };
    let r = if root.is_empty() { "." } else { root };
    let limit = max_files
        .max(cfg.index.prune_pass_file_limit_floor)
        .min(cfg.index.prune_pass_file_limit_ceiling);
    let prune_enumeration_cap = cfg.index.prune_enumeration_file_cap;
    let mut full_files = collect_files(r, limit.max(prune_enumeration_cap), &cfg.index);
    full_files.sort_by_key(|p| index_priority(p));
    let files: Vec<String> = full_files.iter().take(limit).cloned().collect();
    {
        let setup_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(pass_started);
        let msg = format!("code_index: indexing root={} files={} libsql_ok={} manifests={} setup_ms={}", r, files.len(), libsql_ok, prior.len(), setup_ms);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    if full_files.is_empty() && !prior.is_empty() {
        let msg = format!(
            "code_index: ABORTED root={} scanned zero files while {} prior manifests exist -- refusing to treat scan-failure as delete-everything; host_fs_readdir likely returned nothing for this root (sandbox containment or wrong root?)",
            r, prior.len()
        );
        let _ = unsafe { host_log(1, msg.as_ptr(), msg.len() as u32) };
        crate::wasm_dispatch::emit_event("codeinsight_index_zero_scan_aborted", json!({
            "root": r,
            "prior_manifests": prior.len(),
        }));
        return json!({
            "ok": false,
            "error": "zero_scan_aborted",
            "reason": format!("scanned zero files under root={} while {} previously-indexed files exist on disk; refusing to delete the existing index. Check that root resolves inside the sandboxed project directory.", r, prior.len()),
            "files_scanned": 0,
            "files_indexed": 0,
            "chunks": 0,
            "embedded": 0,
            "reused": 0,
            "reused_files": 0,
            "removed_files": 0,
            "skipped_no_embed": 0,
            "deferred_files": 0,
            "kvvec_cleared_dim_mismatch": kvvec_cleared,
            "by_language": {},
        });
    }
    let index_wall_budget_ms: u64 = cfg.index.wall_budget_ms;
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let elapsed_ms = || unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
    let manifest_ns = manifest_ns_for(project_path);
    let mut indexed = 0;
    let mut chunked = 0;
    let mut embedded = 0;
    let mut reused = 0;
    let mut reused_files = 0;
    let mut extracted_files = 0u32;
    let mut deferred_files = 0u32;
    let mut treesitter_failures = 0u32;
    let mut langs = std::collections::BTreeMap::<String, u32>::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut digest_entries: Vec<(String, u32)> = Vec::with_capacity(files.len());
    let mut pending: Vec<(String, FileManifest)> = Vec::new();
    let mut summary = IndexSummary::default();
    for m in prior.values() { summary.apply(m, 1); }

    // Pass structure: the file loop below extracts symbols for every file it
    // can reach inside the wall budget but embeds nothing new -- chunks whose
    // vector is not yet known are persisted in the manifest with an empty
    // `emb` (BM25 and symbol lookup serve them immediately). The embed loop
    // after it spends whatever budget remains on those pending chunks, in
    // file-priority order, and a file only leaves the pending set once every
    // chunk has a stored vector. A tree that cannot be embedded in one pass
    // therefore converges across passes instead of being truncated to the
    // first few chunks of each file.
    for raw_fp in &files {
        if elapsed_ms() > index_wall_budget_ms {
            deferred_files += 1;
            continue;
        }
        let canon = raw_fp.trim_start_matches("./").trim_start_matches('/').to_string();
        let fp = &canon;
        let ext = match fp.rfind('.') { Some(i) => &fp[i..], None => "" };
        let lang_name = match lang_for_ext(ext) { Some(x) => x, None => continue };

        if let Some(m) = prior.get(fp).filter(|m| m.pending_chunks() == 0) {
            if let Some(stat) = crate::wasm_dispatch::host_stat(fp)
                .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            {
                let stat_mtime = stat.get("mtime_ms").and_then(|v| v.as_f64());
                let stat_size = stat.get("size").and_then(|v| v.as_u64());
                // mtime alone is not a safe cache key (coarse filesystem
                // granularity, fast restores); size comes free from the same
                // stat call. A manifest written before `size` existed has
                // None, which makes the check vacuously true once.
                let size_matches = m.size.is_none() || stat_size == m.size;
                if let (Some(mtime), Some(dh)) = (stat_mtime, m.digest_hash) {
                    if mtime == m.mtime_ms && size_matches && m.skipped_no_embed == 0 && libsql_ok && chunk_rows(fp) == m.chunks.len() {
                        seen.insert(fp.clone());
                        indexed += 1;
                        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
                        chunked += m.chunks.len() as i32;
                        reused += m.chunks.len() as i32;
                        reused_files += 1;
                        // Must be the same content-derived value every branch
                        // pushes, or the stored digest can never equal
                        // current_digest() and every dispatch re-indexes.
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
        let file_size = content.len() as u64;
        let file_mtime = crate::wasm_dispatch::host_stat(fp)
            .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            .and_then(|s| s.get("mtime_ms").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        seen.insert(fp.clone());
        indexed += 1;
        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
        let file_hash = crc32(&content);
        let path_hash = crc32(fp);
        let file_digest_hash = crate::hash::fnv1a64(content.as_bytes()) as u32;
        digest_entries.push((fp.clone(), file_digest_hash));

        if let Some(m) = prior.get(fp) {
            if m.hash == file_hash && m.skipped_no_embed == 0 {
                let pending_here = m.pending_chunks();
                let embedded_here = m.chunks.len() - pending_here;
                if pending_here > 0 {
                    chunked += embedded_here as i32;
                    reused += embedded_here as i32;
                    reused_files += 1;
                    pending.push((fp.clone(), m.clone()));
                    continue;
                }
                if libsql_ok && chunk_rows(fp) == m.chunks.len() {
                    chunked += m.chunks.len() as i32;
                    reused += m.chunks.len() as i32;
                    reused_files += 1;
                    continue;
                }
                if libsql_ok {
                    let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[fp]);
                }
                let mut all_persisted = true;
                for c in &m.chunks {
                    let body = slice_lines(&content, c.ls, c.le);
                    all_persisted &= write_chunk(libsql_ok, &db_path, fp, c, &body, project_path);
                    chunked += 1;
                    reused += 1;
                }
                if !all_persisted {
                    fv_delete(&manifest_ns, fp);
                    summary.apply(m, -1);
                }
                reused_files += 1;
                continue;
            }
            if libsql_ok {
                let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[fp]);
            }
            delete_chunk_keys(&m.chunks, project_path);
            delete_call_edges_for_path(fp);
            summary.apply(m, -1);
        } else if libsql_ok {
            let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[fp]);
            delete_call_edges_for_path(fp);
        }

        // Tree-sitter boundaries can shift on any edit, but most edits touch
        // one function: a chunk whose (kind, name, body-hash) triple is
        // unchanged keeps its stored vector instead of re-embedding.
        let prior_chunk_by_identity: std::collections::HashMap<(String, String, u32), &ChunkRecord> = prior
            .get(fp)
            .map(|m| {
                m.chunks
                    .iter()
                    .filter(|c| !c.emb.is_empty())
                    .map(|c| ((c.kind.clone(), c.name.clone(), c.content_hash), c))
                    .collect()
            })
            .unwrap_or_default();

        let extract_started = elapsed_ms();
        let (mut chunks, treesitter_failed) = extract_chunks_reporting_plugin_failure(fp, &content, lang_name);
        let extract_ms = elapsed_ms().saturating_sub(extract_started);
        if extract_ms > 2000 {
            crate::wasm_dispatch::emit_event("code_index_slow_extract", json!({
                "path": fp,
                "extract_ms": extract_ms,
                "source_len": content.len(),
            }));
        }
        if treesitter_failed {
            treesitter_failures += 1;
        } else {
            let edges = extract_call_edges(&content, lang_name, &chunks);
            write_call_edges(fp, &edges);
        }
        if chunks.is_empty() && lang_name == "markdown" && !content.trim().is_empty() {
            let whole = content.chars().take(4000).collect::<String>();
            let line_end = content.lines().count().max(1);
            chunks.push(("document".to_string(), String::new(), 1, line_end, whole));
        }
        if chunks.iter().any(|(_, _, _, _, body)| body.len() > cfg.index.split_chunk_above_bytes) {
            chunks = chunks
                .into_iter()
                .flat_map(|(kind, name, ls, le, body)| split_oversized_chunk(&kind, &name, ls, le, &body))
                .collect();
        }
        extracted_files += 1;

        let mut records: Vec<ChunkRecord> = Vec::with_capacity(chunks.len());
        let mut file_fully_persisted = true;
        for (idx, (kind, name, ls, le, body)) in chunks.into_iter().enumerate() {
            let content_hash = crate::hash::fnv1a64(body.as_bytes()) as u32;
            let emb = prior_chunk_by_identity
                .get(&(kind.clone(), name.clone(), content_hash))
                .map(|c| c.emb.clone())
                .unwrap_or_default();
            let rec = ChunkRecord {
                key: format!("ci-{:x}-{:x}-{}", path_hash, file_hash, idx),
                kind, name, ls, le, emb, content_hash,
            };
            if !rec.emb.is_empty() {
                chunked += 1;
                reused += 1;
                file_fully_persisted &= write_chunk(libsql_ok, &db_path, fp, &rec, &body, project_path);
            }
            records.push(rec);
        }
        if !file_fully_persisted {
            fv_delete(&manifest_ns, fp);
            continue;
        }
        let manifest = FileManifest {
            hash: file_hash,
            digest_hash: Some(file_digest_hash),
            mtime_ms: file_mtime,
            size: Some(file_size),
            commit_overview: None,
            chunks: records,
            skipped_no_embed: 0,
        };
        fv_put(&manifest_ns, fp, &manifest_to_json(fp, &manifest));
        summary.apply(&manifest, 1);
        if manifest.pending_chunks() > 0 {
            pending.push((fp.clone(), manifest));
        }
    }

    let pessimistic_ms_per_chunk = cfg.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound.max(1);
    let per_file_embed_cap = cfg.index.max_chunks_embedded_per_file_per_pass_count_bound_only;
    for (fp, m) in pending.iter_mut() {
        let fp = fp.as_str();
        let remaining_ms = index_wall_budget_ms.saturating_sub(elapsed_ms());
        let cap = ((remaining_ms / pessimistic_ms_per_chunk) as usize).min(per_file_embed_cap);
        if cap == 0 { break; }
        let content = match host_read(fp).or_else(|| host_read(&format!("/{}", fp))) { Some(c) => c, None => continue };
        if crc32(&content) != m.hash { continue; }
        let targets: Vec<usize> = m.chunks.iter().enumerate()
            .filter(|(_, c)| c.emb.is_empty())
            .map(|(i, _)| i)
            .take(cap)
            .collect();
        let bodies: Vec<String> = targets.iter().map(|&i| slice_lines(&content, m.chunks[i].ls, m.chunks[i].le)).collect();
        let inputs: Vec<String> = targets.iter().zip(bodies.iter())
            .map(|(&i, body)| format!("{} {}", m.chunks[i].name, truncate_for_embed(body)))
            .collect();
        let embed_started = elapsed_ms();
        let vectors = embed_texts_batch(&inputs);
        let embed_ms = elapsed_ms().saturating_sub(embed_started);
        if embed_ms > 3000 {
            let msg = format!("code_index: SLOW embed_texts_batch fp={} chunks={} embed_ms={}", fp, inputs.len(), embed_ms);
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_slow_file_embed", json!({
                "path": fp,
                "chunks": inputs.len(),
                "embed_ms": embed_ms,
            }));
        }
        let mut newly_embedded = 0usize;
        for ((i, body), v) in targets.into_iter().zip(bodies).zip(vectors) {
            let Some(v) = v else { continue };
            m.chunks[i].emb = v;
            if write_chunk(libsql_ok, &db_path, fp, &m.chunks[i], &body, project_path) {
                newly_embedded += 1;
            } else {
                m.chunks[i].emb = Vec::new();
            }
        }
        if newly_embedded == 0 { continue; }
        embedded += newly_embedded as i32;
        chunked += newly_embedded as i32;
        summary.pending_embed_chunks -= newly_embedded as i64;
        if m.pending_chunks() == 0 { summary.pending_embed_files -= 1; }
        if m.commit_overview.is_none() && elapsed_ms() < index_wall_budget_ms {
            m.commit_overview = compute_commit_overview(fp);
        }
        fv_put(&manifest_ns, fp, &manifest_to_json(fp, m));
    }
    let pending_embed_files = pending.iter().filter(|(_, m)| m.pending_chunks() > 0).count() as u32;
    deferred_files += pending_embed_files;

    let files_set: std::collections::HashSet<&str> = full_files.iter().map(|s| s.trim_start_matches("./").trim_start_matches('/')).collect();
    let mut removed_files = 0;
    for (fp, m) in &prior {
        if !seen.contains(fp) && !files_set.contains(fp.as_str()) {
            delete_chunk_keys(&m.chunks, project_path);
            delete_call_edges_for_path(fp);
            fv_delete(&manifest_ns, fp);
            summary.apply(m, -1);
            removed_files += 1;
        }
    }
    // Orphan sweep: a process kill between a file's chunk-table DELETE+INSERT
    // and its manifest write (fv_put, further below in the main loop) leaves
    // chunk rows with no corresponding manifest entry. The loop above cannot
    // see these -- it walks `prior` (the manifest map), so a path with chunks
    // but NO manifest is invisible to it by construction. This sweep instead
    // walks the chunks TABLE directly for any path absent from both the
    // current file set and the manifest map, which is the only way to catch
    // an orphan that a normal re-index pass over that same path would
    // otherwise silently clean up on next encounter (code_index.rs's
    // per-file DELETE-before-INSERT already handles the case where the file
    // gets touched again) -- this closes the gap for a file that never gets
    // touched again (deleted from disk before its next index pass).
    if libsql_ok {
        let chunk_paths = chunk_rows_by_path(&db_path);
        let mut orphan_chunk_files = 0u32;
        for path in chunk_paths.keys() {
            if !prior.contains_key(path) && !files_set.contains(path.as_str()) {
                let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[path.as_str()]);
                orphan_chunk_files += 1;
            }
        }
        if orphan_chunk_files > 0 {
            crate::wasm_dispatch::emit_event("code_index_orphan_chunks_swept", json!({
                "orphan_chunk_files": orphan_chunk_files,
                "reason": "chunk rows present with no manifest entry and not in current file set -- a process kill between chunk write and manifest write for a file since removed from disk",
            }));
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
    fv_put(&summary_ns_for(project_path), "summary", &summary.to_json());
    if deferred_files == 0 {
        let digest = digest_from_entries(digest_entries);
        store_digest_at(&digest, project_path);
        let msg = format!("code_index: done files_indexed={} chunks={} embedded={} reused={} reused_files={} extracted_files={} removed_files={} digest={}", indexed, chunked, embedded, reused, reused_files, extracted_files, removed_files, digest);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    } else {
        // Persist the converged subset (see the rationale above). Tagged
        // ":partial=N" so it can never be mistaken for a complete-tree digest:
        // current_digest() always produces the untagged full-tree form, so a
        // partial digest still compares as "changed" next pass and the resume
        // continues -- but the file now EXISTS, which stops the
        // never-stored/always-reindex loop that starved this cache entirely.
        let partial_digest = format!("{}:partial={}", digest_from_entries(digest_entries), deferred_files);
        store_digest_at(&partial_digest, project_path);
        let msg = format!("code_index: partial pass (wall budget) files_indexed={} extracted_files={} deferred_files={} pending_embed_files={} pending_embed_chunks={} embedded={} reused={} removed_files={} -- partial digest stored, next call resumes", indexed, extracted_files, deferred_files, pending_embed_files, summary.pending_embed_chunks.max(0), embedded, reused, removed_files);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
        crate::wasm_dispatch::emit_event("codeinsight_index_partial", json!({
            "files_indexed": indexed,
            "extracted_files": extracted_files,
            "deferred_files": deferred_files,
            "pending_embed_files": pending_embed_files,
            "pending_embed_chunks": summary.pending_embed_chunks.max(0),
            "embedded": embedded,
        }));
    }
    let silently_empty_due_to_plugin_failure = indexed > 0 && chunked == 0 && treesitter_failures >= indexed as u32;
    let pass_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(pass_started);
    {
        let msg = format!("code_index: pass_ms={} (wall_budget_ms={})", pass_ms, index_wall_budget_ms);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    json!({
        "ok": !silently_empty_due_to_plugin_failure,
        "pass_ms": pass_ms,
        "files_scanned": files.len(),
        "files_indexed": indexed,
        "chunks": chunked,
        "embedded": embedded,
        "reused": reused,
        "reused_files": reused_files,
        "removed_files": removed_files,
        "extracted_files": extracted_files,
        "deferred_files": deferred_files,
        "pending_embed_files": pending_embed_files,
        "pending_embed_chunks": summary.pending_embed_chunks.max(0),
        "treesitter_failures": treesitter_failures,
        "kvvec_cleared_dim_mismatch": kvvec_cleared,
        "by_language": langs,
        "likely_orphaned": if include_dead_code {
            likely_orphaned_symbols(&db_path, orphan_scan_limit)
        } else {
            Value::Array(Vec::new())
        },
    })
}

fn embed_text_batch_fallback(inputs: &[String]) -> Vec<Option<Vec<f32>>> {
    inputs.iter().map(|t| embed_text(t)).collect()
}

fn embed_texts_batch(inputs: &[String]) -> Vec<Option<Vec<f32>>> {
    if inputs.is_empty() { return Vec::new(); }
    let resp = plugin_call("bert", "embed_batch", &json!({ "texts": inputs }));
    if !plugin_ok(&resp) {
        crate::wasm_dispatch::emit_event("code_index_embed_batch_failed", json!({
            "plugin_failure": plugin_failure_code(&resp),
            "batch_len": inputs.len(),
        }));
        return embed_text_batch_fallback(inputs);
    }
    match resp.get("embeddings").and_then(|v| v.as_array()) {
        Some(arr) if arr.len() == inputs.len() => {
            arr.iter().map(|e| if e.is_null() { None } else { json_to_f32_vec(e) }).collect()
        }
        _ => embed_text_batch_fallback(inputs),
    }
}

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
    format!("v3:{:016x}:files={}", crate::hash::fnv1a64(acc.as_bytes()), entries.len())
}

pub fn current_digest() -> String {
    current_digest_cfg(&crate::ragconfig::RagConfig::resolved())
}

pub fn current_digest_at(project_path: &str) -> String {
    current_digest_cfg_at(&crate::ragconfig::RagConfig::resolved(), Some(project_path))
}

/// Same as [`current_digest`] with explicit config. The file-size cap MUST match
/// the indexer own, or the digest counts files the index skips and the two can
/// never agree -- the same class of mismatch that made the stored digest
/// permanently unequal to the computed one.
pub fn current_digest_cfg(cfg: &crate::ragconfig::RagConfig) -> String {
    current_digest_cfg_at(cfg, None)
}

pub fn current_digest_cfg_at(cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> String {
    let root = project_path.filter(|p| !p.is_empty()).unwrap_or(".");
    let files = collect_files(root, cfg.index.digest_max_files, &cfg.index);
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
        let content_hash = crate::hash::fnv1a64(content.as_bytes()) as u32;
        entries.push((canon, content_hash));
    }
    digest_from_entries(entries)
}

fn digest_path_for(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("{}/{}", p.trim_end_matches(['/', '\\']), DIGEST_PATH),
        _ => DIGEST_PATH.to_string(),
    }
}

pub fn stored_digest() -> Option<String> {
    stored_digest_at(None)
}

pub fn stored_digest_at(project_path: Option<&str>) -> Option<String> {
    crate::wasm_dispatch::host_read(&digest_path_for(project_path))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn store_digest(digest: &str) {
    store_digest_at(digest, None)
}

pub fn store_digest_at(digest: &str, project_path: Option<&str>) {
    let _ = crate::wasm_dispatch::host_write(&digest_path_for(project_path), digest);
    fv_delete(&code_ns_for(project_path), "__digest__");
}

pub fn overview() -> Value {
    if stored_digest().is_none() {
        return Value::Null;
    }
    let db_path = project_db_path(None);
    // A failed count and a genuinely empty index both used to arrive as 0.
    // They are not the same thing: a `database is locked` here (the shared
    // daemon holding the file) reported "0 symbols" for a fully populated
    // store, which reads as data loss rather than as a transient failure.
    let mut count_error: Option<String> = None;
    let mut count_via = |sql: String| -> Option<u64> {
        match libsql_wasm::query_params(&db_path, &sql, &[]) {
            Ok(rows) => rows
                .as_array()
                .and_then(|a| a.first().cloned())
                .and_then(|row| row.get("c").and_then(|v| v.as_u64())),
            Err(e) => {
                if count_error.is_none() {
                    count_error = Some(e);
                }
                None
            }
        }
    };
    // COUNT via a GROUP BY subquery, not a bare aggregate: an unfiltered
    // aggregate over an F32_BLOB vector table answers 0 even when the table is
    // full. Measured on this repo's store -- COUNT(*) on code_chunks returns 0
    // (and still 0 with a predicate) while this form returns 138, matching the
    // _vec_shadow companion exactly. file_count hit the identical bug via
    // COUNT(DISTINCT path) and is fixed the same way.
    let file_count_opt = count_via(format!(
        "SELECT COUNT(*) AS c FROM (SELECT path FROM {} GROUP BY path)",
        chunks_table()
    ));
    let file_count = file_count_opt.unwrap_or(0);
    let symbol_count_opt = count_via(format!(
        "SELECT SUM(c) AS c FROM (SELECT COUNT(*) AS c FROM {} GROUP BY path)",
        chunks_table()
    ));
    let symbol_count = symbol_count_opt.unwrap_or(0);
    let by_kind = libsql_wasm::query_params(
        &db_path,
        &format!("SELECT kind, COUNT(*) AS c FROM {} GROUP BY kind ORDER BY c DESC LIMIT 10", chunks_table()),
        &[],
    )
    .unwrap_or(Value::Array(Vec::new()));
    let largest_files = libsql_wasm::query_params(
        &db_path,
        &format!("SELECT path, COUNT(*) AS c FROM {} GROUP BY path ORDER BY c DESC LIMIT 10", chunks_table()),
        &[],
    )
    .unwrap_or(Value::Array(Vec::new()));
    let mut out = json!({
        "file_count": file_count,
        "symbol_count": symbol_count,
        "by_kind": by_kind,
        "largest_files": largest_files,
        "digest": stored_digest(),
        "likely_orphaned": likely_orphaned_symbols(&db_path, 20),
    });
    let summary = load_index_summary(None);
    if let Some(s) = &summary {
        out["extracted_files"] = s["files"].clone();
        out["extracted_symbols"] = s["symbols"].clone();
        out["pending_embed_files"] = s["pending_embed_files"].clone();
        out["pending_embed_chunks"] = s["pending_embed_chunks"].clone();
    }
    if let Some(e) = count_error {
        // The chunk table lives in the libsql sibling plugin; when that plugin
        // is absent or evicted the counts fall back to the index summary the
        // last pass wrote to the host kv store, which no plugin outage can
        // hide, and the response names the source instead of reporting zero.
        out["chunk_store_unavailable"] = json!(e);
        match &summary {
            Some(s) => {
                out["counts_source"] = json!("index_summary");
                out["file_count"] = s["files"].clone();
                out["symbol_count"] = s["symbols"].clone();
                out["by_kind"] = s["by_kind"].clone();
            }
            None => {
                out["counts_unavailable"] = json!(true);
            }
        }
        crate::wasm_dispatch::emit_event("codeinsight_overview_counts_failed", json!({ "error": e, "fallback": summary.is_some() }));
    }
    out
}

fn likely_orphaned_symbols(db_path: &str, limit: usize) -> Value {
    if !crate::ragconfig::RagConfig::resolved().index.likely_orphaned_symbol_scan_enabled {
        return Value::Array(Vec::new());
    }
    let candidates = libsql_wasm::query_params(
        db_path,
        &format!(
            "SELECT id, path, kind, name, line_start FROM {} \
             WHERE kind IN ('function_item','function_declaration','method_definition') \
             AND name != '' AND LENGTH(name) > 3 LIMIT 2000",
            chunks_table()
        ),
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
        let referenced = libsql_wasm::query_params(
            db_path,
            &format!("SELECT 1 AS hit FROM {} WHERE id != ?1 AND body LIKE ?2 LIMIT 1", chunks_table()),
            &[id_s.as_str(), pat_s.as_str()],
        )
        .ok()
        .and_then(|rows| rows.as_array().map(|a| !a.is_empty()))
        .unwrap_or(true);
        if !referenced {
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

fn normalized_path(path: &str) -> &str {
    path.trim_start_matches("./").trim_start_matches('/')
}

pub struct FusionCorpus {
    metas: Vec<ChunkMeta>,
    file_cache: std::collections::HashMap<String, Option<String>>,
    overview_by_path: std::collections::HashMap<String, String>,
    index_by_key: std::collections::HashMap<String, usize>,
    index_by_path_line: std::collections::HashMap<(String, usize), usize>,
}

impl FusionCorpus {
    pub fn load() -> Self {
        let mut metas = Vec::new();
        let mut overview_by_path = std::collections::HashMap::new();
        let mut index_by_key = std::collections::HashMap::new();
        let mut index_by_path_line = std::collections::HashMap::new();
        for (fp, m) in load_manifests(None) {
            if let Some(ov) = &m.commit_overview {
                overview_by_path.insert(fp.clone(), ov.clone());
            }
            for c in &m.chunks {
                let at = metas.len();
                index_by_key.entry(c.key.clone()).or_insert(at);
                index_by_path_line
                    .entry((normalized_path(&fp).to_string(), c.ls))
                    .or_insert(at);
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
        FusionCorpus {
            metas,
            file_cache: std::collections::HashMap::new(),
            overview_by_path,
            index_by_key,
            index_by_path_line,
        }
    }

    pub fn overview_for_key(&self, key: &str) -> Option<String> {
        let m = self.metas.get(*self.index_by_key.get(key)?)?;
        self.overview_by_path.get(&m.path).cloned()
    }

    pub fn symbol_for_key(&self, key: &str) -> Option<Value> {
        let m = self.metas.get(*self.index_by_key.get(key)?)?;
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
        let at = *self.index_by_path_line.get(&(normalized_path(path).to_string(), ls))?;
        self.metas.get(at).map(|m| m.key.clone())
    }

    pub fn text_for_key(&mut self, key: &str) -> Option<String> {
        let i = *self.index_by_key.get(key)?;
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

    pub fn bm25_rank(&mut self, query: &str, k: usize) -> Vec<(String, f64)> {
        self.bm25_rank_cfg(query, k, &crate::ragconfig::RagConfig::resolved().scoring)
    }

    pub fn bm25_rank_cfg(&mut self, query: &str, k: usize, scoring: &crate::ragconfig::ScoringConfig) -> Vec<(String, f64)> {
        let k1 = scoring.bm25_k1_term_frequency_saturation;
        let b = scoring.bm25_b_document_length_normalization;
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
                score += idf * (f * (k1 + 1.0)) / (f + k1 * (1.0 - b + b * dl / avgdl));
            }
            if score > 0.0 { scored.push((*i, score)); }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(i, s)| (self.metas[i].key.clone(), s)).collect()
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

fn git_commit_rank_fallback(query: &str, k: usize) -> Vec<(String, String, f64)> {
    let q_tokens = rs_search::tokenize::tokenize(query);
    if q_tokens.is_empty() { return Vec::new(); }
    let log = crate::wasm_dispatch::git_call("log --format=%x00%H%x00%s -n 100 --name-only --no-decorate", None);
    let stdout = log.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let mut commits: Vec<(String, String, f64)> = Vec::new();
    let mut cur_hash: Option<String> = None;
    let mut cur_subject = String::new();
    let mut cur_score = 0.0f64;
    let flush = |commits: &mut Vec<(String, String, f64)>, hash: Option<String>, subject: String, score: f64| {
        if let Some(h) = hash {
            if score > 0.0 { commits.push((h, subject, score)); }
        }
    };
    for line in stdout.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }
        if let Some(rest) = t.strip_prefix('\u{0}') {
            flush(&mut commits, cur_hash.take(), std::mem::take(&mut cur_subject), cur_score);
            cur_score = 0.0;
            let mut parts = rest.splitn(2, '\u{0}');
            cur_hash = parts.next().map(|s| s.to_string());
            cur_subject = parts.next().unwrap_or("").to_string();
            let stoks = rs_search::tokenize::tokenize(&cur_subject);
            cur_score += q_tokens.iter().filter(|q| stoks.contains(q)).count() as f64 * 2.0;
        } else if cur_hash.is_some() {
            let ftoks = rs_search::tokenize::tokenize(t);
            cur_score += q_tokens.iter().filter(|q| ftoks.contains(q)).count() as f64;
        }
    }
    flush(&mut commits, cur_hash.take(), cur_subject, cur_score);
    commits.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    commits.into_iter().take(k).collect()
}

pub fn git_commit_rank(query: &str, k: usize) -> Vec<(String, String, f64)> {
    let _ = crate::git_commit_vectors::sync_incremental();
    let embedding = embed_text_json_query(query);
    if let Some(emb) = embedding {
        if let Ok(hits) = crate::git_commit_vectors::search(&emb, k) {
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    git_commit_rank_fallback(query, k)
}

pub fn search_filenames(pattern: &str, k: usize, cfg: &crate::ragconfig::RagConfig) -> Value {
    search_filenames_at(pattern, k, cfg, None)
}

pub fn search_filenames_at(pattern: &str, k: usize, cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> Value {
    let glob = match crate::path_glob::looks_like_glob(pattern) {
        true => match crate::path_glob::PathGlob::parse(pattern) {
            Ok(g) => Some(g),
            Err(e) => return json!({ "ok": false, "mode": "filename", "error": e }),
        },
        false => None,
    };
    let needle = pattern.to_lowercase();
    let root = project_path.filter(|p| !p.is_empty()).unwrap_or(".");
    let full_files = collect_files(root, cfg.index.digest_max_files.max(20000), &cfg.index);
    let hits: Vec<Value> = full_files.iter()
        .filter(|p| match &glob {
            Some(g) => g.admits(root, None, p),
            None => p.to_lowercase().contains(&needle),
        })
        .take(k)
        .map(|p| json!({ "path": p }))
        .collect();
    json!({ "ok": true, "mode": "filename", "hits": hits, "scanned": full_files.len() })
}

/// Ceiling on files enumerated for an exhaustive literal/regex scan.
///
/// Deliberately NOT `IndexConfig::digest_max_files` (2000 when this was
/// measured on the real C:/dev/litebox-main workspace, which enumerates 2427
/// files, so reusing the digest cap would have dropped ~400 files and returned
/// a confidently wrong "every match" answer. That cap exists to bound a digest
/// whose own doc comment already concedes it can believe an index converged
/// when it has not; an exhaustive scan has the opposite contract and cannot
/// inherit it. A tree larger than this ceiling is reported via
/// `files_truncated: true`, never silently shortened.
pub const LITERAL_SCAN_MAX_FILES: usize = 50_000;

/// Per-line text returned with a match, capped so one minified or generated
/// line cannot dominate the response. A capped line sets `text_truncated`.
const LITERAL_SCAN_MAX_LINE_BYTES: usize = 512;

/// Largest file an exhaustive scan will read.
///
/// Deliberately NOT `IndexConfig::max_file_bytes` (256KB). That bound exists to
/// cap EMBEDDING cost per file, and a literal scan embeds nothing -- it is one
/// linear pass over bytes. Witnessed on the real C:/dev/litebox-main workspace:
/// inheriting the 256KB cap skipped 15 files including
/// `litebox_shim_linux/src/syscalls/file.rs` and
/// `litebox_platform_windows_userland/src/lib.rs`, both genuine Rust source a
/// call-graph trace must cover. Large enough that no plausible hand-written
/// source file is excluded; a file past it is still reported via
/// `files_skipped_too_large`, never silently dropped.
const LITERAL_SCAN_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

const LITERAL_SCAN_REPORTED_EXCLUSIONS: usize = 200;

/// What `scan_literal` was asked for. A struct rather than a long parameter
/// list so a caller cannot transpose two same-typed flags silently.
pub struct LiteralScan<'a> {
    pub pattern: &'a str,
    pub root: Option<&'a str>,
    pub regex: bool,
    pub case_insensitive: bool,
    pub whole_word: bool,
    pub scope: Option<&'a str>,
    pub path_glob: Option<&'a str>,
    pub max_matches: usize,
    pub max_files: usize,
}

enum LiteralMatcher {
    Substring { needle: String, case_insensitive: bool, whole_word: bool },
    Regex(regex::Regex),
}

fn char_is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn occurrence_is_whole_word(haystack: &str, start: usize, end: usize) -> bool {
    let bytes = haystack.as_bytes();
    let before_is_word = start > 0 && char_is_word_byte(bytes[start - 1]);
    let after_is_word = end < bytes.len() && char_is_word_byte(bytes[end]);
    !before_is_word && !after_is_word
}

impl LiteralMatcher {
    /// Every byte offset pair this matcher hits in one line, not just the
    /// first -- two call sites on one line are two real call sites, and a
    /// call-graph trace that reported one would be wrong.
    fn find_all(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            LiteralMatcher::Substring { needle, case_insensitive, whole_word } => {
                let (haystack_owned, haystack) = if *case_insensitive {
                    let lowered = line.to_lowercase();
                    (Some(lowered), "")
                } else {
                    (None, line)
                };
                // A lowercased copy can differ in byte length from the
                // original (e.g. 'İ'), which would make offsets taken in the
                // copy wrong in the original. Fall back to a char-aligned
                // scan of the original whenever the lengths disagree.
                let search_in: &str = match &haystack_owned {
                    Some(lowered) if lowered.len() == line.len() => lowered.as_str(),
                    Some(_) => return self.find_all_case_insensitive_unaligned(line, needle, *whole_word),
                    None => haystack,
                };
                let mut out = Vec::new();
                let mut from = 0usize;
                while let Some(rel) = search_in[from..].find(needle.as_str()) {
                    let start = from + rel;
                    let end = start + needle.len();
                    if !*whole_word || occurrence_is_whole_word(line, start, end) {
                        out.push((start, end));
                    }
                    from = start + needle.len().max(1);
                    if from > search_in.len() { break; }
                }
                out
            }
            LiteralMatcher::Regex(re) => re
                .find_iter(line)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }

    fn find_all_case_insensitive_unaligned(&self, line: &str, needle: &str, whole_word: bool) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let nlen = needle.chars().count();
        if nlen == 0 { return out; }
        let offsets: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
        for (ci, &start) in offsets.iter().enumerate() {
            let end = offsets.get(ci + nlen).copied().unwrap_or(line.len());
            if ci + nlen > offsets.len() { break; }
            let candidate = &line[start..end];
            if candidate.to_lowercase() == needle
                && (!whole_word || occurrence_is_whole_word(line, start, end))
            {
                out.push((start, end));
            }
        }
        out
    }
}

/// Exhaustive literal/regex scan with ripgrep semantics: EVERY match, each
/// with `path` and `line`, in enumeration order, with no relevance ranking
/// and no top-k truncation.
///
/// Touches none of the retrieval machinery the `dual` mode uses -- no corpus
/// digest comparison, no `index()` rebuild, no query embedding, no vector
/// search, no `FusionCorpus`. That is the point, not an optimisation: those
/// steps are what made a literal question over a large workspace cost minutes
/// (measured on C:/dev/litebox-main: 120s and 240s timeouts, one ~420s
/// answer), and an exact-match answer needs none of them. Cost here is one
/// file listing plus one read per file.
///
/// Every bound it hits is disclosed in the response rather than quietly
/// shortening the answer, because a caller tracing a call graph acts on this
/// being complete: `files_truncated`, `matches_truncated`, `budget_exhausted`,
/// `files_skipped_too_large`, `files_unreadable`, `git_listing_incomplete`
/// and `excluded_by_rule` each mean "this result is NOT the whole tree", and
/// `exhaustive` is true only when none of them fired.
pub fn scan_literal(req: &LiteralScan, cfg: &crate::ragconfig::RagConfig) -> Value {
    if req.pattern.is_empty() {
        return json!({ "ok": false, "error": "pattern required -- an exhaustive scan needs something to match" });
    }
    let matcher = if req.regex {
        match regex::RegexBuilder::new(req.pattern)
            .case_insensitive(req.case_insensitive)
            .build()
        {
            Ok(re) => LiteralMatcher::Regex(re),
            // A bad pattern is an error naming the parse failure, never a
            // silent fall back to substring matching: a caller who asked for
            // regex and got substring results would read them as regex
            // results.
            Err(e) => return json!({
                "ok": false,
                "error": format!("mode \"regex\" got an invalid regular expression: {e}"),
                "pattern": req.pattern,
            }),
        }
    } else {
        LiteralMatcher::Substring {
            needle: if req.case_insensitive { req.pattern.to_lowercase() } else { req.pattern.to_string() },
            case_insensitive: req.case_insensitive,
            whole_word: req.whole_word,
        }
    };

    let path_glob = match req.path_glob.map(crate::path_glob::PathGlob::parse).transpose() {
        Ok(g) => g,
        Err(e) => return json!({ "ok": false, "error": e, "path_glob": req.path_glob }),
    };
    let root = req.root.filter(|p| !p.is_empty()).unwrap_or(".");
    let file_cap = req.max_files.min(LITERAL_SCAN_MAX_FILES).max(1);
    let started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let budget_ms = cfg.index.wall_budget_ms;
    let universe = match crate::scan_universe::list_scan_universe(root, req.scope, file_cap.saturating_add(1), &cfg.index) {
        Ok(u) => u,
        Err(e) => return json!({ "ok": false, "error": e, "path": req.scope, "root": root }),
    };
    let listing_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms);
    let listed = &universe.files;
    let files_truncated = listed.len() > file_cap;
    let files: &[String] = if files_truncated { &listed[..file_cap] } else { &listed[..] };

    let mut matches: Vec<Value> = Vec::new();
    let mut files_scanned = 0usize;
    let mut files_matching_glob = 0usize;
    let mut files_with_matches = 0usize;
    let mut files_skipped_too_large: Vec<String> = Vec::new();
    let mut files_skipped_binary = 0usize;
    let mut unreadable_paths: Vec<String> = Vec::new();
    let mut directories_listed_as_files: Vec<String> = Vec::new();
    let mut lines_with_matches = 0usize;
    let mut matches_truncated = false;
    let mut budget_exhausted = false;

    for path in files {
        if unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms) >= budget_ms {
            budget_exhausted = true;
            break;
        }
        if let Some(glob) = &path_glob {
            if !glob.admits(root, req.scope, path) { continue; }
            files_matching_glob += 1;
        }
        let stat = host_stat(path).filter(|v| !v.is_null());
        if let Some(stat) = &stat {
            if stat.get("isDirectory").and_then(|v| v.as_bool()) == Some(true) {
                directories_listed_as_files.push(path.clone());
                continue;
            }
            let size = stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            if size > LITERAL_SCAN_MAX_FILE_BYTES {
                files_skipped_too_large.push(path.clone());
                continue;
            }
            if size == 0 {
                files_scanned += 1;
                continue;
            }
        }
        let Some(content) = host_read(path) else {
            // `host_read` returns None for both "could not read" and "not
            // valid UTF-8", which are different facts about completeness. A
            // successful stat means the file is there and sized, so the read
            // failing is a decode failure: binary content, which cannot
            // contain a text match and is therefore NOT a gap in the answer.
            // A failed stat is a genuine IO/permission failure, which is.
            if stat.is_some() { files_skipped_binary += 1 } else { unreadable_paths.push(path.clone()) }
            continue;
        };
        if content.as_bytes().contains(&0u8) { files_skipped_binary += 1; continue; }
        files_scanned += 1;
        let mut this_file_matched = false;
        for (idx, line) in content.lines().enumerate() {
            let found = matcher.find_all(line);
            if found.is_empty() { continue; }
            this_file_matched = true;
            lines_with_matches += 1;
            for (start, end) in found {
                if matches.len() >= req.max_matches { matches_truncated = true; break; }
                let text_truncated = line.len() > LITERAL_SCAN_MAX_LINE_BYTES;
                let shown: String = if text_truncated {
                    line.chars().take(LITERAL_SCAN_MAX_LINE_BYTES).collect()
                } else {
                    line.to_string()
                };
                let mut hit = serde_json::Map::new();
                hit.insert("path".to_string(), json!(path));
                hit.insert("line".to_string(), json!(idx + 1));
                hit.insert("column".to_string(), json!(start + 1));
                // `get` rather than a slice index: a case-insensitive search
                // takes offsets from a lowercased copy of the line, and a
                // pathological char whose lowercase is the same byte length
                // but a different boundary would panic on a raw slice.
                hit.insert("match".to_string(), json!(line.get(start..end).unwrap_or(req.pattern)));
                hit.insert("text".to_string(), json!(shown.trim_end()));
                if text_truncated { hit.insert("text_truncated".to_string(), json!(true)); }
                matches.push(Value::Object(hit));
            }
            if matches_truncated { break; }
        }
        if this_file_matched { files_with_matches += 1; }
        if matches_truncated { break; }
    }

    // `files_skipped_binary` deliberately does NOT void exhaustiveness: a file
    // that is not text cannot hold a text match, so skipping it leaves the
    // answer complete. Counting it as a gap would make `exhaustive` false on
    // essentially every real repo and train the caller to ignore the flag,
    // which is worse than not having it.
    let deleted_from_worktree = if unreadable_paths.is_empty() {
        std::collections::HashSet::new()
    } else {
        universe.tracked_paths_deleted_from_worktree()
    };
    let (tracked_deleted, unreadable_paths): (Vec<String>, Vec<String>) =
        unreadable_paths.into_iter().partition(|p| deleted_from_worktree.contains(p));
    let glob_matched_no_files = path_glob.is_some() && files_matching_glob == 0 && !files.is_empty();
    let exhaustive = !files_truncated
        && !glob_matched_no_files
        && !matches_truncated
        && !budget_exhausted
        && files_skipped_too_large.is_empty()
        && unreadable_paths.is_empty()
        && universe.listing_complete
        && universe.excluded.is_empty();

    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("mode".to_string(), json!(if req.regex { "regex" } else { "literal" }));
    out.insert("pattern".to_string(), json!(req.pattern));
    out.insert("root".to_string(), json!(root));
    out.insert("case_insensitive".to_string(), json!(req.case_insensitive));
    if !req.regex { out.insert("whole_word".to_string(), json!(req.whole_word)); }
    if let Some(s) = req.scope { out.insert("path".to_string(), json!(s)); }
    if let Some(g) = req.path_glob {
        out.insert("path_glob".to_string(), json!(g));
        out.insert("files_matching_glob".to_string(), json!(files_matching_glob));
    }
    if glob_matched_no_files {
        out.insert("glob_matched_no_files".to_string(), json!(true));
        out.insert("glob_note".to_string(), json!(format!(
            "path_glob admitted none of the {} listed files, so zero matches says nothing about the tree -- the glob is matched case-insensitively against each path relative to the root, relative to \"path\" when one is given, and against the bare file name when it has no '/'; supported syntax: {}",
            files.len(),
            crate::path_glob::PATH_GLOB_SYNTAX,
        )));
    }
    out.insert("match_count".to_string(), json!(matches.len()));
    out.insert("lines_with_matches".to_string(), json!(lines_with_matches));
    out.insert("files_with_matches".to_string(), json!(files_with_matches));
    out.insert("files_scanned".to_string(), json!(files_scanned));
    out.insert("files_listed".to_string(), json!(files.len()));
    out.insert("elapsed_ms".to_string(), json!(unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms)));
    out.insert("exhaustive".to_string(), json!(exhaustive));
    out.insert("listing_ms".to_string(), json!(listing_ms));
    out.insert("file_source".to_string(), json!(universe.source.label()));
    if let Some(reason) = &universe.walk_reason {
        out.insert("walk_reason".to_string(), json!(reason));
    }
    if !universe.listing_complete {
        out.insert("git_listing_incomplete".to_string(), json!(true));
    }
    if !universe.excluded.is_empty() {
        out.insert("excluded_by_rule_count".to_string(), json!(universe.excluded.len()));
        out.insert("excluded_by_rule".to_string(), Value::Array(
            universe.excluded.iter().take(LITERAL_SCAN_REPORTED_EXCLUSIONS)
                .map(|x| json!({ "path": x.path, "rule": x.rule }))
                .collect(),
        ));
    }
    if !tracked_deleted.is_empty() {
        out.insert("tracked_files_deleted_in_worktree".to_string(), json!(tracked_deleted));
    }
    if !directories_listed_as_files.is_empty() {
        out.insert("submodules_not_checked_out".to_string(), json!(directories_listed_as_files));
    }
    if files_truncated {
        out.insert("files_truncated".to_string(), json!(true));
        out.insert("files_truncated_at".to_string(), json!(file_cap));
    }
    if matches_truncated {
        out.insert("matches_truncated".to_string(), json!(true));
        out.insert("matches_truncated_at".to_string(), json!(req.max_matches));
    }
    if budget_exhausted {
        out.insert("budget_exhausted".to_string(), json!(true));
        out.insert("budget_ms".to_string(), json!(budget_ms));
    }
    if !files_skipped_too_large.is_empty() {
        out.insert("files_skipped_too_large".to_string(), json!(files_skipped_too_large));
        out.insert("max_file_bytes".to_string(), json!(LITERAL_SCAN_MAX_FILE_BYTES));
    }
    if files_skipped_binary > 0 { out.insert("files_skipped_binary".to_string(), json!(files_skipped_binary)); }
    if !unreadable_paths.is_empty() {
        out.insert("files_unreadable".to_string(), json!(unreadable_paths.len()));
        out.insert("files_unreadable_paths".to_string(), json!(unreadable_paths));
    }
    if !exhaustive {
        out.insert("exhaustive_note".to_string(), json!(
            "at least one bound or skip rule fired -- this is NOT every match in the tree; files_truncated/matches_truncated/budget_exhausted/files_skipped_too_large/files_unreadable/git_listing_incomplete/excluded_by_rule/glob_matched_no_files name which"
        ));
    }
    out.insert("matches".to_string(), Value::Array(matches));
    Value::Object(out)
}

pub fn search(query: &str, k: usize, inline_embedding: Option<&Value>) -> Value {
    search_at(query, k, inline_embedding, None)
}

pub fn search_at(query: &str, k: usize, inline_embedding: Option<&Value>, project_path: Option<&str>) -> Value {
    if let Err(e) = ensure_schema_for(project_path) { return json!({ "ok": false, "error": e }); }
    let db_path = project_db_path(project_path);
    let qvec = match inline_embedding.and_then(json_to_f32_vec).or_else(|| embed_text(query)) {
        Some(v) => v,
        None => {
            crate::wasm_dispatch::emit_event("codesearch_degraded_to_substring", json!({
                "reason": "no query embedding available; results are substring matches, not semantic ranking",
                "mode": "fallback_like",
            }));
            let like = format!("%{}%", query);
            let sql = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM {} WHERE body LIKE ?1 OR name LIKE ?1 LIMIT {}", chunks_table(), k);
            return match libsql_wasm::query_params(&db_path, &sql, &[&like]) {
                Ok(rows) => json!({ "ok": true, "degraded": true, "degraded_reason": "embedding unavailable", "mode": "fallback_like", "rows": rows }),
                Err(e) => json!({ "ok": false, "degraded": true, "mode": "fallback_like", "error": e }),
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
        Err(e) if crate::shared_db::is_malformed_by_sqlite_error_code(&e) && crate::shared_db::recover_malformed_shared_db() => {
            let _ = ensure_schema_for(project_path);
            match libsql_wasm::query_params(&db_path, &sql, &[&qlit, &qlit]) {
                Ok(rows) => json!({ "ok": true, "mode": "vector_top_k_after_recover", "recovered_from": e, "rows": rows }),
                Err(e2) => json!({ "ok": false, "mode": "recovered_but_still_failing", "vec_err": e, "retry_err": e2 }),
            }
        }
        Err(e) => {
            crate::wasm_dispatch::emit_event("codesearch_degraded_to_substring", json!({
                "reason": "vector query failed; results are substring matches, not semantic ranking",
                "mode": "fallback_like_after_vec_err",
                "vec_err": e,
            }));
            let like = format!("%{}%", query);
            let sql2 = format!("SELECT path, kind, name, line_start, line_end, substr(body,1,400) AS snippet FROM {} WHERE body LIKE ?1 OR name LIKE ?1 LIMIT {}", chunks_table(), k);
            match libsql_wasm::query_params(&db_path, &sql2, &[&like]) {
                Ok(rows) => json!({ "ok": true, "degraded": true, "degraded_reason": "vector query failed", "mode": "fallback_like_after_vec_err", "vec_err": e, "rows": rows }),
                Err(e2) => json!({ "ok": false, "degraded": true, "vec_err": e, "fallback_err": e2 }),
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
