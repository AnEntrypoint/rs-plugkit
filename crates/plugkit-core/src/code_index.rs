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

pub(crate) fn fv_put(ns: &str, key: &str, val: &str) -> bool {
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

pub(crate) fn fv_query(ns: &str, q: &str) -> Value {
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, q.as_ptr(), q.len() as u32) };
    unpack_to_value_pub(packed)
}

pub(crate) fn fv_delete(ns: &str, key: &str) {
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
    bm25_doc_cache_clear();
    fusion_corpus_cache_clear();
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
    let _ = libsql_wasm::exec(&db_path, &format!("DELETE FROM {}", cfg.code_chunks.table));
    crate::code_symbols::clear(None);
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

pub(crate) fn lang_for_ext(ext: &str) -> Option<&'static str> {
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

pub(crate) fn is_dependency_noise_dir_segment(seg: &str, cfg: &crate::ragconfig::IndexConfig) -> bool {
    const DEPENDENCY_NOISE_DIRS: &[&str] = &[
        ".git", ".svn", ".hg", ".bzr", "CVS", ".gm",
        "node_modules", ".npm", ".yarn", ".pnp", ".pnpm-store",
        "vendor", "site-packages", "target", ".cargo", ".rustup",
        ".m2", ".sbt", ".ivy2", ".gem", "Pods", "DerivedData",
    ];
    cfg.skips_dir_segment(seg, DEPENDENCY_NOISE_DIRS)
}

pub fn ensure_schema_at(path: &str) -> Result<(), String> {
    ensure_schema_at_cfg(path, &crate::ragconfig::RagConfig::default())
}

pub fn ensure_schema_at_cfg(path: &str, cfg: &crate::ragconfig::RagConfig) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    libsql_wasm::open(path)?;
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

pub(crate) fn crc32(s: &str) -> u32 {
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
    let root_force_included = cfg.is_force_included(root);
    if !root_force_included
        && root.split('/').any(|seg| is_skipped_dir_segment(seg, cfg))
    { return; }
    for entry in list_dir(root) {
        if files.len() >= max_files { return; }
        let next = if root.ends_with('/') { format!("{}{}", root, entry) } else { format!("{}/{}", root, entry) };
        let force_included = root_force_included || cfg.is_force_included(&next);
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
        } else {
            walk_posix(&next, max_files, files, gi, cfg);
        }
    }
}

pub fn extract_chunks(_path: &str, source: &str, lang_name: &str) -> Vec<(String, String, usize, usize, String)> {
    extract_chunks_reporting_plugin_failure(_path, source, lang_name).0
}

type ChunkTuple = (String, String, usize, usize, String);

fn parse_nodes(source: &str, lang_name: &str) -> Option<Vec<Value>> {
    let resp = plugin_call("treesitter", "parse", &json!({ "lang": lang_name, "source": source }));
    if !plugin_ok(&resp) {
        crate::wasm_dispatch::emit_event("code_index_treesitter_failed", json!({
            "lang": lang_name,
            "plugin_failure": plugin_failure_code(&resp),
            "source_len": source.len(),
        }));
        return None;
    }
    match resp.get("nodes").and_then(|v| v.as_array()) {
        Some(n) => Some(n.clone()),
        None => {
            crate::wasm_dispatch::emit_event("code_index_treesitter_failed", json!({
                "lang": lang_name,
                "plugin_failure": crate::wasm_dispatch::PLUGIN_FAIL_MALFORMED,
                "source_len": source.len(),
            }));
            None
        }
    }
}

fn chunk_spans(source: &str, nodes: &[Value]) -> Vec<(ChunkTuple, (usize, usize))> {
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
        out.push(((kind.to_string(), name, line_start, line_end, body), (start, end)));
    }
    out
}

fn chunks_from_nodes(source: &str, nodes: &[Value]) -> Vec<ChunkTuple> {
    chunk_spans(source, nodes).into_iter().map(|(chunk, _)| chunk).collect()
}

pub fn extract_chunks_reporting_plugin_failure(_path: &str, source: &str, lang_name: &str) -> (Vec<ChunkTuple>, bool) {
    match parse_nodes(source, lang_name) {
        Some(nodes) => (chunks_from_nodes(source, &nodes), false),
        None => (Vec::new(), true),
    }
}

pub(crate) struct FunctionMetrics {
    pub cx: u32,
    pub nesting: u32,
    pub params: u32,
    pub sloc: u32,
    pub node_count: u32,
    pub shape_hash: u64,
}

pub(crate) struct ImportRef {
    pub spec: String,
    pub line: usize,
}

pub(crate) struct SourceAnalysis {
    pub chunks: Vec<ChunkTuple>,
    pub metrics: Vec<Option<FunctionMetrics>>,
    pub edges: Vec<CallEdge>,
    pub imports: Vec<ImportRef>,
    pub parse_failed: bool,
}

const DECISION_NODE_TYPES: &[&str] = &[
    "if_statement", "if_expression", "elif_clause", "else_if_clause",
    "for_statement", "for_in_statement", "for_of_statement", "for_expression",
    "while_statement", "while_expression", "loop_expression", "do_statement",
    "catch_clause", "except_clause", "case_statement", "switch_case", "switch_section",
    "match_arm", "expression_case", "type_case", "communication_case",
    "conditional_expression", "ternary_expression", "boolean_operator", "comprehension",
];

const NESTING_NODE_TYPES: &[&str] = &[
    "if_statement", "if_expression", "for_statement", "for_in_statement", "for_of_statement",
    "for_expression", "while_statement", "while_expression", "loop_expression", "do_statement",
    "match_expression", "switch_statement", "try_statement", "closure_expression",
    "arrow_function", "lambda", "lambda_expression",
];

const MIN_SHAPE_NODES: u32 = 40;

fn is_function_like_kind(kind: &str) -> bool {
    kind.contains("function") || kind.contains("method")
}

fn leading_parameter_count(body: &str) -> u32 {
    let Some(open) = body.find('(') else { return 0 };
    let mut depth = 1i32;
    let mut previous = '(';
    let mut inner = String::new();
    for c in body[open + 1..].chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '<' => depth += 1,
            '>' if previous != '-' && previous != '=' => depth -= 1,
            _ => {}
        }
        if depth <= 0 { break; }
        inner.push(c);
        previous = c;
    }
    let trimmed = inner.trim().trim_end_matches(',').trim();
    if trimmed.is_empty() { return 0; }
    let first = trimmed.split(',').next().unwrap_or("").trim();
    let receiver = matches!(first, "self" | "&self" | "&mut self" | "mut self" | "this" | "cls");
    let total = trimmed.matches(',').count() as u32 + 1;
    total - receiver as u32
}

fn shape_hash_and_count(sorted_nodes: &[RawNode], start: usize, end: usize) -> (u64, u32) {
    let first = sorted_nodes.partition_point(|n| n.start_byte < start);
    let mut hash = 0xcbf29ce484222325u64;
    let mut count = 0u32;
    for n in sorted_nodes[first..].iter().take_while(|n| n.start_byte < end).filter(|n| n.end_byte <= end) {
        count += 1;
        if n.kind.contains("identifier") || n.kind.contains("comment") { continue; }
        for byte in n.kind.bytes().chain(std::iter::once(b'|')) {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    (hash, count)
}

fn function_metrics(sorted_nodes: &[RawNode], decisions: &[RawNode], nesting_nodes: &[RawNode], start: usize, end: usize, body: &str) -> FunctionMetrics {
    let decision_start = decisions.partition_point(|n| n.start_byte < start);
    let cx = 1 + decisions[decision_start..].iter().take_while(|n| n.start_byte < end).filter(|n| n.end_byte <= end).count() as u32;
    let nesting_start = nesting_nodes.partition_point(|n| n.start_byte < start);
    let mut open_ends: Vec<usize> = Vec::new();
    let mut nesting = 0u32;
    for n in nesting_nodes[nesting_start..].iter().take_while(|n| n.start_byte < end).filter(|n| n.end_byte <= end && n.end_byte - n.start_byte < end - start) {
        while open_ends.last().is_some_and(|e| *e <= n.start_byte) { open_ends.pop(); }
        open_ends.push(n.end_byte);
        nesting = nesting.max(open_ends.len() as u32);
    }
    let (shape_hash, node_count) = shape_hash_and_count(sorted_nodes, start, end);
    FunctionMetrics {
        cx,
        nesting,
        params: leading_parameter_count(body),
        sloc: body.lines().filter(|l| !l.trim().is_empty()).count() as u32,
        node_count,
        shape_hash: if node_count >= MIN_SHAPE_NODES { shape_hash } else { 0 },
    }
}

fn first_quoted(text: &str) -> Option<String> {
    let open = text.find(['\'', '"', '`'])?;
    let quote = text[open..].chars().next()?;
    let rest = &text[open + 1..];
    let close = rest.find(quote)?;
    Some(rest[..close].to_string())
}

fn import_specs(kind: &str, lang_name: &str, text: &str) -> Vec<String> {
    let text = text.trim();
    match (lang_name, kind) {
        ("javascript" | "typescript" | "tsx", "import_statement") => first_quoted(text).into_iter().collect(),
        ("javascript" | "typescript" | "tsx", "export_statement") if text.contains(" from ") => first_quoted(text).into_iter().collect(),
        ("javascript" | "typescript" | "tsx", "call_expression") if text.starts_with("require(") || text.starts_with("import(") => first_quoted(text).into_iter().collect(),
        ("python", "import_statement") => text
            .strip_prefix("import")
            .unwrap_or("")
            .split(',')
            .filter_map(|part| part.trim().split_whitespace().next().map(str::to_string))
            .collect(),
        ("python", "import_from_statement") => text
            .strip_prefix("from")
            .and_then(|rest| rest.trim().split_whitespace().next())
            .map(str::to_string)
            .into_iter()
            .collect(),
        ("rust", "use_declaration") => {
            let after = text.find("use ").map(|i| &text[i + 4..]).unwrap_or("");
            let path = after.split(['{', ';', ' ']).next().unwrap_or("").trim_end_matches("::");
            if path.is_empty() { Vec::new() } else { vec![path.to_string()] }
        }
        ("rust", "mod_item") if text.ends_with(';') => text
            .trim_end_matches(';')
            .rsplit(' ')
            .next()
            .map(|name| format!("self::{name}"))
            .into_iter()
            .collect(),
        ("go", "import_spec") => first_quoted(text).into_iter().collect(),
        ("java" | "kotlin", "import_declaration") => {
            let path = text.strip_prefix("import").unwrap_or("").trim().trim_start_matches("static ").trim().trim_end_matches(';').trim();
            if path.is_empty() { Vec::new() } else { vec![path.to_string()] }
        }
        ("c" | "cpp", "preproc_include") if text.contains('"') => first_quoted(text).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn inline_crate_path_imports(source: &str, sorted_nodes: &[RawNode], already: &[ImportRef]) -> Vec<ImportRef> {
    let mut seen: std::collections::HashSet<String> = already.iter().map(|i| i.spec.clone()).collect();
    let mut out = Vec::new();
    for n in sorted_nodes.iter().filter(|n| matches!(n.kind, "scoped_identifier" | "scoped_type_identifier")) {
        let window_end = n.end_byte.min(n.start_byte + 200);
        let Some(text) = source.get(n.start_byte..window_end) else { continue };
        if !(text.starts_with("crate::") || text.starts_with("super::") || text.starts_with("self::")) { continue; }
        let spec: String = text.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
        if seen.insert(spec.clone()) {
            out.push(ImportRef { spec, line: n.start_row + 1 });
        }
    }
    out
}

const IMPORT_NODE_TYPES: &[&str] = &[
    "import_statement", "import_from_statement", "export_statement", "use_declaration", "mod_item",
    "import_spec", "import_declaration", "preproc_include", "call_expression",
];

fn imports_from_nodes(source: &str, lang_name: &str, sorted_nodes: &[RawNode]) -> Vec<ImportRef> {
    let mut out = Vec::new();
    for n in sorted_nodes.iter().filter(|n| IMPORT_NODE_TYPES.contains(&n.kind)) {
        let window_end = (n.end_byte).min(n.start_byte + 400);
        let Some(text) = source.get(n.start_byte..window_end).or_else(|| source.get(n.start_byte..n.end_byte.min(source.len()))) else { continue };
        for spec in import_specs(n.kind, lang_name, text) {
            out.push(ImportRef { spec, line: n.start_row + 1 });
        }
    }
    out
}

pub(crate) fn analyze_source(source: &str, lang_name: &str) -> SourceAnalysis {
    let Some(nodes) = parse_nodes(source, lang_name) else {
        return SourceAnalysis { chunks: Vec::new(), metrics: Vec::new(), edges: Vec::new(), imports: Vec::new(), parse_failed: true };
    };
    let spans = chunk_spans(source, &nodes);
    let chunks: Vec<ChunkTuple> = spans.iter().map(|(chunk, _)| chunk.clone()).collect();
    let edges = call_edges_from_nodes(source, lang_name, &nodes, &chunks);
    let mut sorted = parsed_nodes(&nodes);
    sorted.sort_by_key(|n| (n.start_byte, std::cmp::Reverse(n.end_byte)));
    let decisions: Vec<RawNode> = sorted.iter().filter(|n| DECISION_NODE_TYPES.contains(&n.kind)).copied().collect();
    let continues_else_chain = |n: &RawNode| source.get(..n.start_byte).is_some_and(|before| before.trim_end().ends_with("else"));
    let nesting_nodes: Vec<RawNode> = sorted.iter().filter(|n| NESTING_NODE_TYPES.contains(&n.kind) && !continues_else_chain(n)).copied().collect();
    let metrics = spans
        .iter()
        .map(|((kind, _, _, _, body), (start, end))| {
            is_function_like_kind(kind).then(|| function_metrics(&sorted, &decisions, &nesting_nodes, *start, *end, body))
        })
        .collect();
    let mut imports = imports_from_nodes(source, lang_name, &sorted);
    if lang_name == "rust" {
        imports.extend(inline_crate_path_imports(source, &sorted, &imports));
    }
    SourceAnalysis { chunks, metrics, edges, imports, parse_failed: false }
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

fn callee_name_for_call(call: &RawNode, sorted_nodes: &[RawNode], src_bytes: &[u8]) -> Option<String> {
    let first = sorted_nodes.partition_point(|n| n.start_byte < call.start_byte);
    let callee_expression_end = sorted_nodes[first..]
        .iter()
        .take_while(|n| n.start_byte == call.start_byte)
        .filter(|n| n.end_byte < call.end_byte)
        .map(|n| n.end_byte)
        .max()?;
    let last_leaf = sorted_nodes[first..]
        .iter()
        .take_while(|n| n.start_byte < callee_expression_end)
        .filter(|n| n.end_byte <= callee_expression_end && CALLEE_LEAF_NODE_TYPES.contains(&n.kind))
        .max_by_key(|n| (n.end_byte, n.start_byte))?;
    let end = last_leaf.end_byte.min(src_bytes.len());
    if end <= last_leaf.start_byte { return None; }
    let text = String::from_utf8_lossy(&src_bytes[last_leaf.start_byte..end]).into_owned();
    let simple_name = text.rsplit(['.', ':']).next().unwrap_or(&text).to_string();
    if simple_name.is_empty() { None } else { Some(simple_name) }
}

pub(crate) const MODULE_LEVEL_CALLER: &str = "<module>";

pub struct CallEdge {
    pub caller_symbol: String,
    pub callee_symbol: String,
    pub line: usize,
}

fn call_edges_from_nodes(source: &str, lang_name: &str, nodes_json: &[Value], chunks: &[ChunkTuple]) -> Vec<CallEdge> {
    let mut all = parsed_nodes(nodes_json);
    all.sort_by_key(|n| (n.start_byte, std::cmp::Reverse(n.end_byte)));
    let src_bytes = source.as_bytes();
    let mut enclosing_by_line: Vec<(usize, usize, &str)> = chunks.iter()
        .filter(|(kind, name, _, _, _)| !name.is_empty() && (kind.contains("function") || kind.contains("method")))
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
        let caller_symbol = find_enclosing(line).unwrap_or(MODULE_LEVEL_CALLER);
        let Some(callee_symbol) = callee_name_for_call(call, &all, src_bytes) else { continue };
        let is_rust_constructor = lang_name == "rust" && callee_symbol.chars().next().is_some_and(char::is_uppercase);
        if is_rust_constructor { continue; }
        out.push(CallEdge { caller_symbol: caller_symbol.to_string(), callee_symbol, line });
    }
    if lang_name == "rust" {
        out.extend(macro_argument_call_edges(source, &all, &find_enclosing));
    }
    out
}

fn macro_argument_call_edges<'a>(source: &str, sorted_nodes: &[RawNode], find_enclosing: &dyn Fn(usize) -> Option<&'a str>) -> Vec<CallEdge> {
    let mut outer_trees: Vec<(usize, usize)> = Vec::new();
    for tree in sorted_nodes.iter().filter(|n| n.kind == "token_tree") {
        if outer_trees.last().is_some_and(|(_, end)| tree.end_byte <= *end) { continue; }
        outer_trees.push((tree.start_byte, tree.end_byte));
    }
    let inside_macro_arguments = |pos: usize| {
        let i = outer_trees.partition_point(|(start, _)| *start <= pos);
        i > 0 && pos < outer_trees[i - 1].1
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    for id in sorted_nodes.iter().filter(|n| n.kind == "identifier" && bytes.get(n.end_byte) == Some(&b'(')) {
        if !inside_macro_arguments(id.start_byte) { continue; }
        let Some(name) = source.get(id.start_byte..id.end_byte) else { continue };
        let is_constructor = name.chars().next().is_some_and(char::is_uppercase);
        let is_declaration = source.get(..id.start_byte).is_some_and(|before| before.ends_with("fn "));
        if name.is_empty() || is_constructor || is_declaration { continue; }
        let line = id.start_row + 1;
        let caller = find_enclosing(line).unwrap_or(MODULE_LEVEL_CALLER);
        out.push(CallEdge { caller_symbol: caller.to_string(), callee_symbol: name.to_string(), line });
    }
    out
}

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

/// Trims one file's chunks to the index budget and reports which bound bit, or `None` when the
/// whole file fits. Chunks are kept in tree-sitter order, so a file that overflows keeps its head
/// and loses its tail -- the same shape as before, at a bound real files no longer reach.
fn trim_chunks_to_index_budget(chunks: &mut Vec<ChunkTuple>) -> Option<(&'static str, usize)> {
    let mut bytes = 0usize;
    let mut keep = chunks.len();
    for (idx, (_, _, _, _, body)) in chunks.iter().enumerate() {
        bytes += body.len();
        if idx >= MAX_CHUNKS_INDEXED_PER_FILE || bytes > MAX_CHUNK_BYTES_INDEXED_PER_FILE {
            keep = idx;
            break;
        }
    }
    if keep >= chunks.len() { return None; }
    let full = chunks.len();
    chunks.truncate(keep);
    let bound = if full > MAX_CHUNKS_INDEXED_PER_FILE { "chunk_count" } else { "chunk_bytes" };
    Some((bound, full))
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

fn root_ns_suffix(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("__root{:x}", crc32(p)),
        _ => String::new(),
    }
}

fn manifest_ns_for(project_path: Option<&str>) -> String {
    format!("{}{}", indexing_pipeline_namespace_config_unthreaded_default().manifest_namespace(), root_ns_suffix(project_path))
}

pub(crate) fn code_ns_for(project_path: Option<&str>) -> String {
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
const FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS: u64 = 7;

/// A chunk with no vector is still a BM25 document, so what is indexed is bounded by the tree, not
/// by the embed budget. Reusing the 64-chunk embed allowance as the index bound dropped every
/// symbol past the 64th of a large file: on litebox-main that hid `syscalls/process.rs` and
/// `syscalls/file.rs` from every ranked query that named one of their later functions.
///
/// 256 was still under the symbol count of the tree's biggest sources, so it only moved the cliff:
/// `litebox_platform_windows_userland/src/lib.rs` (15k lines) lost everything past its 256th
/// symbol, including `spawn_cross_process_fork_child` at line 12066. One symbol per definition is
/// the natural granularity of a source file, so the count bound is set where real files stop, not
/// where a round number sits.
const MAX_CHUNKS_INDEXED_PER_FILE: usize = 4096;

/// Counting chunks alone lets a node-dense generated file -- one `arrow_function` per few bytes of
/// a minified bundle -- spend a whole pass on a single file. Bodies are already split to
/// `split_chunk_above_bytes`, so bounding indexed bytes as well as chunk count is what caps the
/// work: text reaching BM25, manifest row size, and the chunk write loop are each bounded by this
/// rather than by however many nodes tree-sitter happened to find.
const MAX_CHUNK_BYTES_INDEXED_PER_FILE: usize = 4 * 1024 * 1024;

/// BM25 adds one IDF-weighted contribution per query term, so a chunk carrying three ordinary
/// query words outscores the single chunk carrying the rare identifier the query is really about:
/// `pub(crate) fn sys_accept4 accept4 flags descriptor` ranked the only chunk in the tree holding
/// `accept4` seventh of eight, behind chunks that merely held `flags` and `descriptor`. An
/// identifier is worth more than a pile of ordinary words, so the rarest term a chunk matches adds
/// its IDF again on top of the sum and cannot be out-argued by the rest.
const RARE_TERM_DOMINANCE_WEIGHT: f64 = 1.5;
/// A chunk record is written from treesitter output alone, so the reserve before extracting one
/// more file covers a read plus an extraction: single-digit milliseconds measured on a <=256KB
/// file. The old reserve was `pessimistic_ms_per_chunk` (16s, an embed-plus-write estimate),
/// which deferred every file after the first and left the digest permanently partial.
/// Re-reading every candidate to look for the literal query is linear in corpus size, so the
/// phrase check covers the ranked head only: 8x the requested k, never fewer than 40 chunks.
const PHRASE_BOOST_WINDOW_PER_RESULT: usize = 8;
const PHRASE_BOOST_WINDOW_MIN: usize = 40;

/// Measured against BM25 scores in this corpus: 4x lifts the verbatim match over a longer chunk
/// that carries the query's rarest term twice, without burying a genuinely better-ranked chunk.
const PHRASE_MATCH_SCORE_MULTIPLIER: f64 = 4.0;

const EXTRACTION_RESERVE_MS: u64 = 500;

/// Share of one pass's wall budget that may go to the bert embedder. Measured on a cold model the
/// first embed of a pass costs ~8s, so embedding must be rationed: the remaining two thirds carry
/// the text index over every file in the tree.
const EMBED_BUDGET_DIVISOR: u64 = 3;

/// Share of one pass's budget the symbol table may have once the chunk walk is done. Syncing
/// symbols re-extracts every file, so it is spare-time work, never the pass's first claim.
const SYMBOL_SYNC_BUDGET_DIVISOR: u64 = 4;

/// Embeddings are bought with time the chunk walk demonstrably does not need: while the walk is
/// still inside the first half of its own wall budget. A pass that is behind -- a fresh tree, where
/// the walk eats the whole budget -- indexes text first and vectors on a later pass.
const EMBED_WALK_HEADROOM_DIVISOR: u64 = 2;


#[derive(Clone)]
struct ChunkRecord {
    key: String,
    kind: String,
    name: String,
    ls: usize,
    le: usize,
    /// None when the chunk is text-indexed but has no vector yet: an embedder that cannot keep
    /// up with the tree must not stop the chunk from being findable by keyword and phrase search.
    emb: Option<Vec<f32>>,
    content_hash: u32,
}

struct FileManifest {
    hash: u32,
    digest_hash: Option<u32>,
    mtime_ms: f64,
    size: Option<u64>,
    commit_overview: Option<String>,
    chunks: Vec<ChunkRecord>,
    skipped_no_embed: u32,
    version: u64,
}

impl FileManifest {
    fn holds_every_chunk(&self) -> bool {
        self.skipped_no_embed == 0 && self.version >= FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS
    }
}

fn manifest_to_json(fp: &str, hash: u32, digest_hash: u32, mtime_ms: f64, size: u64, commit_overview: &Option<String>, chunks: &[ChunkRecord], skipped_no_embed: u32) -> String {
    let arr: Vec<Value> = chunks.iter().map(|c| json!({
        "key": c.key,
        "kind": c.kind,
        "name": c.name,
        "ls": c.ls,
        "le": c.le,
        "emb": c.emb,
        "ch": c.content_hash,
    })).collect();
    json!({ "v": MANIFEST_VERSION, "path": fp, "hash": hash, "digest_hash": digest_hash, "mtime_ms": mtime_ms, "size": size, "commit_overview": commit_overview, "chunks": arr, "skipped_no_embed": skipped_no_embed }).to_string()
}

fn parse_manifest(val: &str) -> Option<(String, FileManifest)> {
    let parsed: Value = serde_json::from_str(val).ok()?;
    const MIN_READABLE_MANIFEST_VERSION: u64 = 4;
    let version = match parsed.get("v").and_then(|v| v.as_u64()) {
        Some(v) if v >= MIN_READABLE_MANIFEST_VERSION && v <= MANIFEST_VERSION => v,
        _ => return None,
    };
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
        let emb = c.get("emb").and_then(json_to_f32_vec);
        let content_hash = c.get("ch").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        chunks.push(ChunkRecord { key, kind, name, ls, le, emb, content_hash });
    }
    let skipped_no_embed = parsed.get("skipped_no_embed").and_then(|s| s.as_u64()).unwrap_or(0) as u32;
    Some((fp, FileManifest { hash, digest_hash, mtime_ms, size, commit_overview, chunks, skipped_no_embed, version }))
}

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

/// Line-start byte offsets for one file's content, so slicing a chunk out of it is two lookups
/// instead of a rescan. `slice_lines` walked the file's whole `.lines()` iterator for every chunk:
/// on a 15k-line file carrying ~500 indexed chunks that is ~7.5M line scans per pass, which is what
/// turned indexing a large source file into a 70-second BM25 rank and an 87-second index walk.
struct LineIndex {
    content: String,
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(content: String) -> Self {
        let mut starts = Vec::with_capacity(content.len() / 48 + 2);
        starts.push(0);
        for (at, byte) in content.bytes().enumerate() {
            if byte == b'\n' { starts.push(at + 1); }
        }
        LineIndex { content, starts }
    }

    /// Lines `ls..=le` exactly as `content.lines().skip(ls - 1).take(le - ls + 1).join("\n")`
    /// produced them -- `\r\n` folded to `\n` and no trailing newline -- so a chunk's body stays
    /// byte-identical to what is already stored and reuse is not invalidated.
    fn slice(&self, ls: usize, le: usize) -> String {
        if ls == 0 || le < ls { return String::new(); }
        let begin = match self.starts.get(ls - 1) { Some(at) => *at, None => return String::new() };
        let end = self.starts.get(le).copied().unwrap_or(self.content.len());
        if end <= begin { return String::new(); }
        let body = &self.content[begin..end];
        let body = body.strip_suffix('\n').unwrap_or(body);
        if body.contains('\r') { body.replace("\r\n", "\n") } else { body.to_string() }
    }
}

fn slice_lines(content: &str, ls: usize, le: usize) -> String {
    if ls == 0 || le < ls { return String::new(); }
    content.lines().skip(ls - 1).take(le - ls + 1).collect::<Vec<_>>().join("\n")
}

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

fn write_chunk(libsql_ok: bool, db_path: &str, fp: &str, c: &ChunkRecord, body: &str, project_path: Option<&str>) -> bool {
    // A chunk with no vector yet is already findable: its manifest row feeds the BM25 corpus, whose
    // text is sliced from the file on disk. Only a vector is worth persisting, so there is nothing
    // to write here and nothing to report as a persistence failure.
    let emb = match &c.emb {
        Some(e) => e.as_slice(),
        None => return true,
    };
    let mut persisted = true;
    if libsql_ok {
        let embedding_lit = vec_to_json_literal(emb);
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
    let emb_json = serde_json::json!({ "embedding": emb }).to_string();
    fv_put(&code_ns_for(project_path), &c.key, &emb_json);
    bm25_doc_cache_invalidate(&c.key);
    persisted
}

fn delete_chunk_keys(chunks: &[ChunkRecord], project_path: Option<&str>) {
    for c in chunks {
        fv_delete(&code_ns_for(project_path), &c.key);
        fv_delete(&code_vec_ns_for(project_path), &c.key);
        bm25_doc_cache_invalidate(&c.key);
    }
}

pub fn index(root: &str, max_files: usize) -> Value {
    index_cfg(root, max_files, &crate::ragconfig::RagConfig::resolved())
}

pub fn index_at(root: &str, max_files: usize, project_path: &str) -> Value {
    index_cfg_impl(root, max_files, &crate::ragconfig::RagConfig::resolved(), false, 20, Some(project_path), true)
}

pub fn index_at_topup(root: &str, max_files: usize, project_path: &str, cap_ms: u64) -> Value {
    let mut cfg = crate::ragconfig::RagConfig::resolved();
    cfg.index.wall_budget_ms = cap_ms.min(cfg.index.wall_budget_ms);
    index_cfg_impl(root, max_files, &cfg, false, 20, Some(project_path), false)
}

pub fn index_with_dead_code(root: &str, max_files: usize, limit: usize) -> Value {
    let mut out = index_cfg_impl(root, max_files, &crate::ragconfig::RagConfig::resolved(), true, limit, None, true);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("dead_code_scan_forced".to_string(), json!(true));
    }
    out
}

pub fn index_cfg(root: &str, max_files: usize, cfg: &crate::ragconfig::RagConfig) -> Value {
    index_cfg_impl(root, max_files, cfg, cfg.index.likely_orphaned_symbol_scan_enabled, 20, None, true)
}

pub fn index_topup(root: &str, max_files: usize, cap_ms: u64) -> Value {
    let mut cfg = crate::ragconfig::RagConfig::resolved();
    cfg.index.wall_budget_ms = cap_ms.min(cfg.index.wall_budget_ms);
    index_cfg_impl(root, max_files, &cfg, cfg.index.likely_orphaned_symbol_scan_enabled, 20, None, false)
}

fn index_cfg_impl(
    root: &str,
    max_files: usize,
    cfg: &crate::ragconfig::RagConfig,
    include_dead_code: bool,
    orphan_scan_limit: usize,
    project_path: Option<&str>,
    full_pass: bool,
) -> Value {
    let db_path = project_db_path(project_path);
    let pass_started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut at = pass_started;
    let libsql_err = ensure_schema_at(&db_path).err().map(|e| e.to_string());
    let libsql_ok = libsql_err.is_none();
    if let Some(e) = &libsql_err {
        let msg = format!("code_index: libsql unavailable at {} -- {} (digest will not persist and chunk reads return empty)", db_path, e);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    }
    let kvvec_cleared = clear_codeinsight_if_dim_mismatch(project_path);
    let schema_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    if kvvec_cleared {
        let rows = fv_query(&manifest_ns_for(project_path), "");
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let Some(k) = row.get("key").and_then(|k| k.as_str()) { fv_delete(&manifest_ns_for(project_path), k); }
            }
        }
    }
    let prior = load_manifests(project_path);
    let manifests_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let chunk_counts = if libsql_ok {
        chunk_rows_by_path(&db_path)
    } else {
        std::collections::HashMap::new()
    };
    let chunk_rows_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let chunk_rows = |fp: &str| -> usize { chunk_counts.get(fp).copied().unwrap_or(0) };
    let r = if root.is_empty() { "." } else { root };
    let limit = max_files
        .max(cfg.index.prune_pass_file_limit_floor)
        .min(cfg.index.prune_pass_file_limit_ceiling);
    let prune_enumeration_cap = cfg.index.prune_enumeration_file_cap;
    let mut full_files = collect_files(r, limit.max(prune_enumeration_cap), &cfg.index);
    full_files.sort_by(|a, b| canonical_index_path(a).cmp(canonical_index_path(b)));
    let resume_cursor = stored_index_cursor_at(project_path);
    let files = rotated_from_cursor(&full_files, resume_cursor.as_deref());
    let mut first_deferred: Option<String> = None;
    let mut fresh_files_this_pass = 0usize;
    let collect_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    {
        let msg = format!("code_index: indexing root={} files={} libsql_ok={} manifests={}", r, files.len(), libsql_ok, prior.len());
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
    let enumeration_was_complete = full_files.len() < limit.max(prune_enumeration_cap);
    let mut indexed = 0;
    let mut chunked = 0;
    let mut embedded = 0;
    let mut reused = 0;
    let mut reused_files = 0;
    let mut skipped_no_embed = 0u32;
    let mut embed_requests = 0u32;
    let mut embed_failed = 0u32;
    let mut deferred_chunks = 0u32;
    let mut deferred_files = 0u32;
    let mut floor_grace_used_this_pass = false;
    let pessimistic_ms_per_chunk = cfg.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound.max(1);
    let mut embed_ms_spent = 0u64;
    let embed_budget_ms = if full_pass {
        index_wall_budget_ms / EMBED_BUDGET_DIVISOR
    } else {
        0
    };
    let mut measured_embed_ms_per_chunk = pessimistic_ms_per_chunk;
    let mut treesitter_failures = 0u32;
    let mut langs = std::collections::BTreeMap::<String, u32>::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut digest_entries: Vec<(String, u32)> = Vec::with_capacity(files.len());

    for raw_fp in &files {
        let canon = canonical_index_path(raw_fp).to_string();
        let fp = &canon;
        let dot = fp.rfind('.');
        let ext = match dot { Some(i) => &fp[i..], None => "" };
        let lang_name = match lang_for_ext(ext) { Some(x) => x, None => continue };
        let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        if elapsed > index_wall_budget_ms {
            deferred_files += 1;
            first_deferred.get_or_insert_with(|| fp.clone());
            continue;
        }

        if let Some(m) = prior.get(fp) {
            if let Some(stat) = crate::wasm_dispatch::host_stat(fp)
                .or_else(|| crate::wasm_dispatch::host_stat(raw_fp))
            {
                let stat_mtime = stat.get("mtime_ms").and_then(|v| v.as_f64());
                let stat_size = stat.get("size").and_then(|v| v.as_u64());
                let size_matches = m.size.is_none() || stat_size == m.size;
                let embed_budget_left = embed_budget_ms.saturating_sub(embed_ms_spent);
                if let (Some(mtime), Some(dh)) = (stat_mtime, m.digest_hash) {
                    let chunks_missing_vectors = m.skipped_no_embed > 0 && embed_budget_left > 0;
                    let chunks_present = if m.holds_every_chunk() {
                        chunk_rows(fp) == m.chunks.len()
                    } else {
                        chunk_rows(fp) + m.skipped_no_embed as usize >= m.chunks.len()
                    };
                    if mtime == m.mtime_ms && size_matches && libsql_ok && !chunks_missing_vectors && chunks_present
                    {
                        seen.insert(fp.clone());
                        indexed += 1;
                        *langs.entry(lang_name.to_string()).or_insert(0) += 1;
                        chunked += m.chunks.len() as i32;
                        reused += m.chunks.len() as i32;
                        reused_files += 1;
                        digest_entries.push((fp.clone(), dh));
                        continue;
                    }
                }
            }
        }

        let elapsed_before_extraction = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        let remaining_before_extraction = index_wall_budget_ms.saturating_sub(elapsed_before_extraction);
        if remaining_before_extraction < EXTRACTION_RESERVE_MS {
            if floor_grace_used_this_pass {
                deferred_files += 1;
                first_deferred.get_or_insert_with(|| fp.clone());
                continue;
            }
            floor_grace_used_this_pass = true;
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
            let chunks_missing_vectors = m.skipped_no_embed > 0 && embed_budget_ms.saturating_sub(embed_ms_spent) > 0;
            if m.hash == file_hash && !chunks_missing_vectors && m.version >= FIRST_MANIFEST_VERSION_RECORDING_DEFERRED_CHUNKS {
                let chunks_present = if m.holds_every_chunk() {
                    chunk_rows(fp) == m.chunks.len()
                } else {
                    chunk_rows(fp) + m.skipped_no_embed as usize >= m.chunks.len()
                };
                let mut all_persisted = true;
                if !(libsql_ok && chunks_present) {
                    if libsql_ok {
                        let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[fp]);
                    }
                    for c in &m.chunks {
                        let body = slice_lines(&content, c.ls, c.le);
                        all_persisted &= write_chunk(libsql_ok, &db_path, fp, c, &body, project_path);
                    }
                }
                chunked += m.chunks.len() as i32;
                reused += m.chunks.len() as i32;
                reused_files += 1;
                if all_persisted {
                    fv_put(&manifest_ns_for(project_path), fp, &manifest_to_json(fp, file_hash, file_digest_hash, file_mtime, file_size, &m.commit_overview, &m.chunks, m.skipped_no_embed));
                } else {
                    fv_delete(&manifest_ns_for(project_path), fp);
                }
                continue;
            }
        }

        let prior_chunk_by_identity: std::collections::HashMap<(String, String, u32), &ChunkRecord> = prior
            .get(fp)
            .map(|m| {
                m.chunks
                    .iter()
                    .map(|c| ((c.kind.clone(), c.name.clone(), c.content_hash), c))
                    .collect()
            })
            .unwrap_or_default();

        let (mut chunks, treesitter_failed) = extract_chunks_reporting_plugin_failure(fp, &content, lang_name);
        if chunks.is_empty() && lang_name == "markdown" && !content.trim().is_empty() {
            let whole = content.chars().take(4000).collect::<String>();
            let line_end = content.lines().count().max(1);
            chunks.push(("document".to_string(), String::new(), 1, line_end, whole));
        }
        if chunks.iter().any(|(_, _, _, _, body)| body.len() > crate::ragconfig::RagConfig::resolved().index.split_chunk_above_bytes) {
            chunks = chunks
                .into_iter()
                .flat_map(|(kind, name, ls, le, body)| split_oversized_chunk(&kind, &name, ls, le, &body))
                .collect();
        }

        if let Some((bound, full)) = trim_chunks_to_index_budget(&mut chunks) {
            let msg = format!(
                "code_index: capping {} chunks={} -> {} (bound={}; file still indexed and marked seen)",
                fp, full, chunks.len(), bound
            );
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_chunk_cap", json!({
                "path": fp,
                "chunks_total": full,
                "chunks_indexed": chunks.len(),
                "bound": bound,
                "count_cap": MAX_CHUNKS_INDEXED_PER_FILE,
                "byte_cap": MAX_CHUNK_BYTES_INDEXED_PER_FILE,
            }));
        }
        let max_chunks_per_file_per_pass = cfg.index.max_chunks_embedded_per_file_per_pass_count_bound_only;
        let elapsed_now = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
        let remaining_ms = index_wall_budget_ms.saturating_sub(elapsed_now);
        let budget_chunks = (remaining_ms / pessimistic_ms_per_chunk).max(1) as usize;
        let embed_remaining = embed_budget_ms.saturating_sub(embed_ms_spent);
        let walk_ahead_of_schedule = elapsed_before_extraction
            .saturating_mul(EMBED_WALK_HEADROOM_DIVISOR)
            < index_wall_budget_ms;
        let embed_allowance = if embed_remaining == 0 || !walk_ahead_of_schedule {
            0
        } else {
            ((embed_remaining / measured_embed_ms_per_chunk) as usize).clamp(1, max_chunks_per_file_per_pass)
        };
        let cap = max_chunks_per_file_per_pass.min(embed_allowance);

        let chunk_content_hashes: Vec<u32> = chunks.iter()
            .map(|(_, _, _, _, body)| crate::hash::fnv1a64(body.as_bytes()) as u32)
            .collect();
        let reused_embs: Vec<Option<Vec<f32>>> = chunks.iter().zip(chunk_content_hashes.iter())
            .map(|((kind, name, _, _, _), ch)| {
                prior_chunk_by_identity.get(&(kind.clone(), name.clone(), *ch)).and_then(|c| c.emb.clone())
            })
            .collect();
        let chunk_plan = plan_chunk_embeds(&reused_embs, cap);
        let fresh_needed = chunk_plan.iter().filter(|p| **p != ChunkEmbedPlan::Reuse).count();

        if fresh_needed > 0 {
            let fresh_file_allowance_spent = fresh_files_this_pass >= limit;
            let under_floor = remaining_ms < pessimistic_ms_per_chunk;
            if fresh_file_allowance_spent || (under_floor && floor_grace_used_this_pass) {
                deferred_files += 1;
                first_deferred.get_or_insert_with(|| fp.clone());
                continue;
            }
            if under_floor {
                floor_grace_used_this_pass = true;
            }
            fresh_files_this_pass += 1;
        }

        if let Some(m) = prior.get(fp) {
            delete_chunk_keys(&m.chunks, project_path);
        }
        if libsql_ok {
            let _ = libsql_wasm::exec_params(&db_path, &format!("DELETE FROM {} WHERE path=?1", chunks_table()), &[fp]);
        }
        if treesitter_failed {
            treesitter_failures += 1;
        }
        let deferred_in_file = chunk_plan.iter().filter(|p| **p == ChunkEmbedPlan::DeferToNextPass).count();
        if deferred_in_file > 0 {
            let msg = format!(
                "code_index: capping {} fresh_chunks={} -> {} (count_cap={} budget_chunks={} remaining_ms={}; the rest resume next pass)",
                fp, fresh_needed, cap, max_chunks_per_file_per_pass, budget_chunks, remaining_ms
            );
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_chunk_cap", json!({
                "path": fp,
                "chunks_total": chunks.len(),
                "chunks_fresh": fresh_needed,
                "chunks_embedded_now": fresh_needed - deferred_in_file,
                "chunks_deferred": deferred_in_file,
                "count_cap": max_chunks_per_file_per_pass,
                "budget_chunks": budget_chunks,
                "remaining_ms": remaining_ms,
                "pessimistic_ms_per_chunk": pessimistic_ms_per_chunk,
            }));
        }

        let embed_inputs: Vec<String> = chunks.iter().zip(chunk_plan.iter())
            .filter(|(_, plan)| **plan == ChunkEmbedPlan::EmbedNow)
            .map(|((_, name, _, _, body), _)| format!("{} {}", name, truncate_for_embed(body)))
            .collect();
        embed_requests += embed_inputs.len() as u32;
        let reused_chunk_count = reused_embs.iter().filter(|r| r.is_some()).count();
        let embed_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        let mut fresh_embeds = embed_texts_batch(&embed_inputs).into_iter();
        let embed_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(embed_started);
        embed_ms_spent += embed_ms;
        if !embed_inputs.is_empty() {
            measured_embed_ms_per_chunk = (embed_ms / embed_inputs.len() as u64).max(1);
        }
        if embed_ms > 3000 {
            let msg = format!("code_index: SLOW embed_texts_batch fp={} chunks={} reused_chunks={} embed_ms={}", fp, embed_inputs.len(), reused_chunk_count, embed_ms);
            let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("code_index_slow_file_embed", json!({
                "path": fp,
                "chunks": embed_inputs.len(),
                "reused_chunks": reused_chunk_count,
                "embed_ms": embed_ms,
            }));
        }
        if reused_chunk_count > 0 {
            crate::wasm_dispatch::emit_event("code_index_chunk_reuse", json!({
                "path": fp,
                "chunks_total": chunks.len(),
                "chunks_reused": reused_chunk_count,
                "chunks_embedded": embed_inputs.len(),
            }));
        }

        let embed_results: Vec<(Option<Vec<f32>>, ChunkEmbedPlan)> = reused_embs.into_iter().zip(chunk_plan.into_iter())
            .map(|(reused, plan)| match plan {
                ChunkEmbedPlan::Reuse => (reused, plan),
                ChunkEmbedPlan::EmbedNow => {
                    let got = fresh_embeds.next().unwrap_or(None);
                    if got.is_none() { embed_failed += 1; }
                    (got, plan)
                }
                ChunkEmbedPlan::DeferToNextPass => (None, plan),
            })
            .collect();

        let mut records: Vec<ChunkRecord> = Vec::new();
        let mut file_fully_persisted = true;
        let mut file_skipped_no_embed: u32 = 0;
        let chunk_write_loop_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        let chunks_in_this_file = chunk_content_hashes.len();
        for (idx, (((kind, name, ls, le, body), (emb_opt, plan)), content_hash)) in chunks.into_iter().zip(embed_results.into_iter()).zip(chunk_content_hashes.into_iter()).enumerate() {
            let was_reused = plan == ChunkEmbedPlan::Reuse;
            chunked += 1;
            if emb_opt.is_none() {
                file_skipped_no_embed += 1;
                if plan == ChunkEmbedPlan::DeferToNextPass {
                    deferred_chunks += 1;
                } else {
                    skipped_no_embed += 1;
                    let msg = format!("code_index: no vector for {}:{} ({}); the chunk stays in the manifest and is findable as text, with no embedding row", fp, ls, name);
                    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
                }
            } else if was_reused {
                reused += 1;
            } else {
                embedded += 1;
            }
            let key = format!("ci-{:x}-{:x}-{}", path_hash, file_hash, idx);
            let rec = ChunkRecord { key, kind, name, ls, le, emb: emb_opt, content_hash };
            file_fully_persisted &= write_chunk(libsql_ok, &db_path, fp, &rec, &body, project_path);
            records.push(rec);
        }
        let chunk_write_loop_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(chunk_write_loop_started);
        if chunk_write_loop_ms > 2000 {
            crate::wasm_dispatch::emit_event("code_index_unbounded_chunk_write_loop_slow", json!({
                "path": fp,
                "chunks_in_file": chunks_in_this_file,
                "loop_ms": chunk_write_loop_ms,
                "wall_budget_ms": index_wall_budget_ms,
                "note": "no elapsed-check guard exists inside this loop by design -- see index-resumable-partial-file-so-chunk-writes-can-be-budget-bounded for why a naive guard would be unsafe",
            }));
        }
        if file_fully_persisted {
            let over_budget =
                unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started) > index_wall_budget_ms;
            let commit_overview = if over_budget {
                crate::wasm_dispatch::emit_event("code_index_commit_overview_skipped", json!({
                    "path": fp,
                    "reason": "wall budget already exhausted; the git subprocess is enrichment and is deferred to the next pass",
                }));
                None
            } else {
                compute_commit_overview(fp)
            };
            fv_put(&manifest_ns_for(project_path), fp, &manifest_to_json(fp, file_hash, file_digest_hash, file_mtime, file_size, &commit_overview, &records, file_skipped_no_embed));
        } else {
            fv_delete(&manifest_ns_for(project_path), fp);
        }
        fusion_corpus_cache_invalidate(project_path);
    }

    // Symbols enrich hits; chunks ARE the searchable index. Syncing symbols first spent half of
    // every pass on a table nobody queried and left the pass with too little budget to index a single
    // file, so it now runs only on time the chunk walk did not need. Its budget is measured from its
    // own start: sharing the pass clock made a walk that used the budget look like a symbol sync that
    // had already overshot, which deferred every file and wrote no symbols at all.
    let walk_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };
    let symbol_sync_started = at;
    let symbol_budget_ms = if !full_pass {
        0
    } else {
        index_wall_budget_ms
            .saturating_sub(symbol_sync_started.saturating_sub(started))
            .min(index_wall_budget_ms / SYMBOL_SYNC_BUDGET_DIVISOR)
    };
    let symbol_sync = if symbol_budget_ms == 0 {
        json!({
            "ok": true,
            "files_synced": 0,
            "files_unchanged": 0,
            "files_deferred": full_files.len(),
            "complete": false,
            "skipped": if full_pass {
                "chunk walk used the whole wall budget; no time left for symbols"
            } else {
                "symbols are a full pass's spare-time work; a search-triggered topup answers the query instead"
            },
        })
    } else {
        crate::code_symbols::sync_files(
            &full_files,
            project_path,
            symbol_sync_started,
            symbol_budget_ms,
            cfg.index.max_file_bytes,
            enumeration_was_complete,
        )
    };
    let symbol_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(symbol_sync_started);
    at = unsafe { crate::wasm_dispatch::host_now_ms() };

    let files_set: std::collections::HashSet<&str> = full_files.iter().map(|s| s.trim_start_matches("./").trim_start_matches('/')).collect();
    let mut removed_files = 0;
    for (fp, m) in &prior {
        if !seen.contains(fp) && !files_set.contains(fp.as_str()) {
            delete_chunk_keys(&m.chunks, project_path);
            fv_delete(&manifest_ns_for(project_path), fp);
            fusion_corpus_cache_invalidate(project_path);
            removed_files += 1;
        }
    }
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
    let cleanup_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(at);
    let pass_ms = json!({
        "schema": schema_ms,
        "manifests": manifests_ms,
        "chunk_rows": chunk_rows_ms,
        "collect_files": collect_ms,
        "walk": walk_ms,
        "symbols": symbol_ms,
        "cleanup": cleanup_ms,
        "total": unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(pass_started),
    });
    let pass_complete = deferred_files == 0 && deferred_chunks == 0 && skipped_no_embed == 0;
    store_index_cursor_at(first_deferred.as_deref(), project_path);
    if pass_complete {
        let digest = digest_from_entries(digest_entries);
        store_digest_at(&digest, project_path);
        let msg = format!("code_index: done files_indexed={} chunks={} embedded={} reused={} reused_files={} removed_files={} skipped_no_embed={} embed_budget_ms={} embed_requests={} embed_failed={} digest={} pass_ms={}", indexed, chunked, embedded, reused, reused_files, removed_files, skipped_no_embed, embed_budget_ms, embed_requests, embed_failed, digest, pass_ms);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
    } else {
        let partial_digest = format!("{}:partial={}", digest_from_entries(digest_entries), deferred_files + deferred_chunks + skipped_no_embed);
        store_digest_at(&partial_digest, project_path);
        let msg = format!("code_index: partial pass files_indexed={} deferred_files={} deferred_chunks={} embedded={} reused={} removed_files={} skipped_no_embed={} embed_requests={} embed_failed={} resume_at={:?} pass_ms={} -- partial digest stored, next call resumes there", indexed, deferred_files, deferred_chunks, embedded, reused, removed_files, skipped_no_embed, embed_requests, embed_failed, first_deferred, pass_ms);
        let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
        crate::wasm_dispatch::emit_event("codeinsight_index_partial", json!({
            "files_indexed": indexed,
            "deferred_files": deferred_files,
            "deferred_chunks": deferred_chunks,
            "resume_at": first_deferred,
            "embedded": embedded,
        }));
    }
    let silently_empty_due_to_plugin_failure = indexed > 0 && chunked == 0 && treesitter_failures >= indexed as u32;
    json!({
        "ok": !silently_empty_due_to_plugin_failure,
        "files_scanned": files.len(),
        "files_indexed": indexed,
        "chunks": chunked,
        "embedded": embedded,
        "reused": reused,
        "reused_files": reused_files,
        "removed_files": removed_files,
        "skipped_no_embed": skipped_no_embed,
        "embed_requests": embed_requests,
        "embed_failed": embed_failed,
        "deferred_files": deferred_files,
        "deferred_chunks": deferred_chunks,
        "resume_at": first_deferred,
        "treesitter_failures": treesitter_failures,
        "kvvec_cleared_dim_mismatch": kvvec_cleared,
        "by_language": langs,
        "symbols": symbol_sync,
        "likely_orphaned": if include_dead_code {
            likely_orphaned_symbols(&db_path, orphan_scan_limit)
        } else {
            Value::Array(Vec::new())
        },
        "digest": stored_digest_at(project_path),
        "pass_ms": pass_ms,
        "complete": pass_complete,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChunkEmbedPlan {
    Reuse,
    EmbedNow,
    DeferToNextPass,
}

fn plan_chunk_embeds(reused: &[Option<Vec<f32>>], fresh_allowance: usize) -> Vec<ChunkEmbedPlan> {
    let mut granted = 0usize;
    reused.iter().map(|r| {
        if r.is_some() { return ChunkEmbedPlan::Reuse; }
        if granted < fresh_allowance {
            granted += 1;
            ChunkEmbedPlan::EmbedNow
        } else {
            ChunkEmbedPlan::DeferToNextPass
        }
    }).collect()
}

fn canonical_index_path(raw: &str) -> &str {
    raw.trim_start_matches("./").trim_start_matches('/')
}

fn rotated_from_cursor(sorted: &[String], cursor: Option<&str>) -> Vec<String> {
    let start = match cursor {
        Some(c) => sorted.partition_point(|p| canonical_index_path(p) < c),
        None => 0,
    };
    sorted[start..].iter().chain(sorted[..start].iter()).cloned().collect()
}

const INDEX_CURSOR_PATH: &str = ".gm/exec-spool/.codeinsight-cursor";

fn index_cursor_path_for(project_path: Option<&str>) -> String {
    match project_path {
        Some(p) if !p.is_empty() => format!("{}/{}", p.trim_end_matches(['/', '\\']), INDEX_CURSOR_PATH),
        _ => INDEX_CURSOR_PATH.to_string(),
    }
}

fn stored_index_cursor_at(project_path: Option<&str>) -> Option<String> {
    crate::wasm_dispatch::host_read(&index_cursor_path_for(project_path))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn store_index_cursor_at(cursor: Option<&str>, project_path: Option<&str>) {
    let _ = crate::wasm_dispatch::host_write(&index_cursor_path_for(project_path), cursor.unwrap_or(""));
}

pub fn ensure_current_insight() -> Value {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let stored = stored_digest();
    let current = current_digest_cfg(&cfg);
    let stale = stored.as_deref() != Some(current.as_str());
    let prior_partial = stored.as_deref().is_some_and(|digest| digest.contains(":partial="));
    let cold_start = stored.is_none();
    let index = if stale && !prior_partial {
        if cold_start {
            index_cfg(".", cfg.index.prune_pass_file_limit_ceiling, &cfg)
        } else {
            index_topup(".", cfg.index.prune_pass_file_limit_ceiling, cfg.index.incremental_topup_wall_budget_ms)
        }
    } else {
        let symbols = if stale { crate::code_symbols::sync_tree(&cfg, None) } else { Value::Null };
        json!({
            "ok": true,
            "reused": true,
            "digest": stored,
            "complete": !prior_partial,
            "partial": prior_partial,
            "symbols": symbols,
        })
    };
    let refreshed = index.get("digest").and_then(|v| v.as_str()).map(str::to_owned);
    let complete = index.get("complete").and_then(|v| v.as_bool()).unwrap_or(false);
    let chunks = index.get("chunks").and_then(|v| v.as_u64()).unwrap_or(0);
    let ready = index.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
        && (chunks > 0 || prior_partial || !stale);
    let fresh = !stale || (complete && refreshed.as_deref() == Some(current.as_str()));
    json!({
        "required": true,
        "ready": ready,
        "stale_before": stale,
        "fresh": fresh,
        "expected_digest": current,
        "index": index,
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

pub fn current_digest_cfg(cfg: &crate::ragconfig::RagConfig) -> String {
    current_digest_cfg_at(cfg, None)
}

const DIGEST_CACHE_TTL_MS: u64 = 5_000;

struct DigestCacheEntry {
    ts_ms: u64,
    digest: String,
}

static DIGEST_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, DigestCacheEntry>>> =
    std::sync::Mutex::new(None);

fn project_scoped_cache_key(project_path: Option<&str>) -> String {
    match project_path.filter(|p| !p.is_empty()) {
        Some(p) => format!("root:{}", p.trim_end_matches(['/', '\\'])),
        None => format!("cwd:{}", crate::wasm_dispatch::host_cwd_string().unwrap_or_default().trim_end_matches(['/', '\\'])),
    }
}

pub fn current_digest_cfg_at(cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> String {
    let cache_key = project_scoped_cache_key(project_path);
    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    if let Ok(cache) = DIGEST_CACHE.lock() {
        if let Some(entry) = cache.as_ref().and_then(|m| m.get(&cache_key)) {
            if now_ms.saturating_sub(entry.ts_ms) < DIGEST_CACHE_TTL_MS {
                return entry.digest.clone();
            }
        }
    }
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
    let digest = digest_from_entries(entries);
    if let Ok(mut cache) = DIGEST_CACHE.lock() {
        cache.get_or_insert_with(std::collections::HashMap::new)
            .insert(cache_key, DigestCacheEntry { ts_ms: now_ms, digest: digest.clone() });
    }
    digest
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
    crate::code_symbols::lean_overview(stored_digest())
}

pub(crate) fn embedded_coverage(db_path: &str) -> (u64, u64) {
    let count_via = |sql: String| -> u64 {
        libsql_wasm::query_params(db_path, &sql, &[])
            .ok()
            .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()))
            .and_then(|row| row.get("c").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))))
            .unwrap_or(0)
    };
    let files = count_via(format!("SELECT COUNT(*) AS c FROM (SELECT path FROM {} GROUP BY path)", chunks_table()));
    let chunks = count_via(format!("SELECT SUM(c) AS c FROM (SELECT COUNT(*) AS c FROM {} GROUP BY path)", chunks_table()));
    (files, chunks)
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

#[derive(Clone)]
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
    metas: std::sync::Arc<Vec<ChunkMeta>>,
    file_cache: std::collections::HashMap<String, Option<std::sync::Arc<LineIndex>>>,
    overview_by_path: std::sync::Arc<std::collections::HashMap<String, String>>,
    index_by_key: std::sync::Arc<std::collections::HashMap<String, usize>>,
    index_by_path_line: std::sync::Arc<std::collections::HashMap<(String, usize), usize>>,
}

struct Bm25DocEntry {
    tf: std::collections::HashMap<String, u32>,
    dl: f64,
}

static BM25_DOC_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, std::sync::Arc<Bm25DocEntry>>>> =
    std::sync::Mutex::new(None);

fn bm25_doc_cache_invalidate(key: &str) {
    if let Ok(mut cache) = BM25_DOC_CACHE.lock() {
        if let Some(m) = cache.as_mut() { m.remove(key); }
    }
}

fn bm25_doc_cache_clear() {
    if let Ok(mut cache) = BM25_DOC_CACHE.lock() {
        *cache = None;
    }
}

type CorpusIndexByLine = std::collections::HashMap<(String, usize), usize>;

struct CachedCorpus {
    metas: std::sync::Arc<Vec<ChunkMeta>>,
    overview_by_path: std::sync::Arc<std::collections::HashMap<String, String>>,
    index_by_key: std::sync::Arc<std::collections::HashMap<String, usize>>,
    index_by_path_line: std::sync::Arc<CorpusIndexByLine>,
}

static FUSION_CORPUS_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, CachedCorpus>>> =
    std::sync::Mutex::new(None);

fn fusion_corpus_cache_key(project_path: Option<&str>) -> String {
    project_scoped_cache_key(project_path)
}

fn fusion_corpus_cache_invalidate(project_path: Option<&str>) {
    if let Ok(mut cache) = FUSION_CORPUS_CACHE.lock() {
        if let Some(m) = cache.as_mut() {
            m.remove(&fusion_corpus_cache_key(project_path));
        }
    }
}

fn fusion_corpus_cache_clear() {
    if let Ok(mut cache) = FUSION_CORPUS_CACHE.lock() {
        *cache = None;
    }
}

impl FusionCorpus {
    pub fn load() -> Self {
        Self::load_at(None)
    }

    pub fn load_at(project_path: Option<&str>) -> Self {
        let cache_key = fusion_corpus_cache_key(project_path);
        if let Ok(cache) = FUSION_CORPUS_CACHE.lock() {
            if let Some(c) = cache.as_ref().and_then(|m| m.get(&cache_key)) {
                return FusionCorpus {
                    metas: std::sync::Arc::clone(&c.metas),
                    file_cache: std::collections::HashMap::new(),
                    overview_by_path: std::sync::Arc::clone(&c.overview_by_path),
                    index_by_key: std::sync::Arc::clone(&c.index_by_key),
                    index_by_path_line: std::sync::Arc::clone(&c.index_by_path_line),
                };
            }
        }
        let mut metas = Vec::new();
        let mut overview_by_path = std::collections::HashMap::new();
        let mut index_by_key = std::collections::HashMap::new();
        let mut index_by_path_line = std::collections::HashMap::new();
        for (fp, m) in load_manifests(project_path) {
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
        let metas = std::sync::Arc::new(metas);
        let overview_by_path = std::sync::Arc::new(overview_by_path);
        let index_by_key = std::sync::Arc::new(index_by_key);
        let index_by_path_line = std::sync::Arc::new(index_by_path_line);
        if let Ok(mut cache) = FUSION_CORPUS_CACHE.lock() {
            cache.get_or_insert_with(std::collections::HashMap::new).insert(cache_key, CachedCorpus {
                metas: std::sync::Arc::clone(&metas),
                overview_by_path: std::sync::Arc::clone(&overview_by_path),
                index_by_key: std::sync::Arc::clone(&index_by_key),
                index_by_path_line: std::sync::Arc::clone(&index_by_path_line),
            });
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

    fn file_index(&mut self, path: &str) -> Option<std::sync::Arc<LineIndex>> {
        if let Some(cached) = self.file_cache.get(path) { return cached.clone(); }
        let content = host_read(path).or_else(|| host_read(&format!("/{}", path)));
        let index = content.map(LineIndex::new).map(std::sync::Arc::new);
        self.file_cache.insert(path.to_string(), index.clone());
        index
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
        let index = self.file_index(&path)?;
        let body = index.slice(ls, le);
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
        let mut cache = BM25_DOC_CACHE.lock().ok();
        if let Some(guard) = cache.as_mut() {
            guard.get_or_insert_with(std::collections::HashMap::new);
        }
        let mut doc_tfs: Vec<(usize, std::sync::Arc<Bm25DocEntry>)> = Vec::new();
        for i in 0..self.metas.len() {
            let key = self.metas[i].key.clone();
            let cached = cache.as_ref()
                .and_then(|c| c.as_ref())
                .and_then(|m| m.get(&key))
                .cloned();
            // Sharing the cached entry by Arc matters: cloning 7486 term maps into a fresh Vec
            // per query cost 4.6s of a dispatch, all of it copying bytes the cache already owned.
            let entry = match cached {
                Some(e) => e,
                None => {
                    let (tf, dl) = match compute_doc_tf(self, i) {
                        Some(v) => v,
                        None => continue,
                    };
                    std::sync::Arc::new(Bm25DocEntry { tf, dl })
                }
            };
            if let Some(m) = cache.as_mut().and_then(|c| c.as_mut()) {
                m.insert(key, std::sync::Arc::clone(&entry));
            }
            doc_tfs.push((i, entry));
        }
        drop(cache);
        if doc_tfs.is_empty() { return Vec::new(); }
        let n = doc_tfs.len() as f64;
        let avgdl = doc_tfs.iter().map(|(_, e)| e.dl).sum::<f64>() / n;
        let avgdl = if avgdl > 0.0 { avgdl } else { 1.0 };
        let mut df: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for t in &q_tokens {
            let c = doc_tfs.iter().filter(|(_, e)| e.tf.contains_key(t)).count() as u32;
            df.insert(t.as_str(), c);
        }
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (i, e) in &doc_tfs {
            let mut score = 0.0;
            let mut rarest_matched_idf = 0.0f64;
            let dl = e.dl;
            for t in &q_tokens {
                let f = *e.tf.get(t).unwrap_or(&0) as f64;
                if f == 0.0 { continue; }
                let d = *df.get(t.as_str()).unwrap_or(&0) as f64;
                let idf = (1.0 + (n - d + 0.5) / (d + 0.5)).ln();
                rarest_matched_idf = rarest_matched_idf.max(idf);
                score += idf * (f * (k1 + 1.0)) / (f + k1 * (1.0 - b + b * dl / avgdl));
            }
            score += RARE_TERM_DOMINANCE_WEIGHT * rarest_matched_idf;
            if score > 0.0 { scored.push((*i, score)); }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // BM25 scores each query term on its own, so a chunk carrying only the rarest term
        // outranks one carrying the whole query verbatim. Re-read the top slice and lift the
        // chunks whose text contains the query as one substring, which is what the caller asked
        // for and what OR-summed term scores cannot express.
        let needle = query.trim().to_lowercase();
        if q_tokens.len() > 1 && !needle.is_empty() {
            let window = scored.len().min(k.saturating_mul(PHRASE_BOOST_WINDOW_PER_RESULT).max(PHRASE_BOOST_WINDOW_MIN));
            for (i, score) in scored.iter_mut().take(window) {
                let key = self.metas[*i].key.clone();
                if self.text_for_key(&key).unwrap_or_default().to_lowercase().contains(&needle) {
                    *score *= PHRASE_MATCH_SCORE_MULTIPLIER;
                }
            }
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }
        scored.into_iter().take(k).map(|(i, s)| (self.metas[i].key.clone(), s)).collect()
    }
}

fn compute_doc_tf(corpus: &mut FusionCorpus, i: usize) -> Option<(std::collections::HashMap<String, u32>, f64)> {
    let (path, name, ls, le) = {
        let m = &corpus.metas[i];
        (m.path.clone(), m.name.clone(), m.ls, m.le)
    };
    let index = corpus.file_index(&path)?;
    let body = index.slice(ls, le);
    let tf = term_freqs(&format!("{} {} {}", path, name, body));
    let dl = tf.values().sum::<u32>() as f64;
    Some((tf, dl))
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

pub fn git_commit_rank_at(root: &str, query: &str, k: usize) -> Vec<Value> {
    let q_tokens = rs_search::tokenize::tokenize(query);
    if q_tokens.is_empty() { return Vec::new(); }
    let log = crate::wasm_dispatch::git_call_argv(
        &["log", "--format=%H%x00%s%x1e", "-n", "100", "--no-decorate"],
        Some(root),
    );
    let stdout = log.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let mut candidates: Vec<(String, String, f64)> = stdout.split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.trim().splitn(2, '\u{0}');
            let hash = fields.next()?.trim();
            let subject = fields.next()?.trim();
            if hash.len() != 40 { return None; }
            let tokens = rs_search::tokenize::tokenize(subject);
            let score = q_tokens.iter().filter(|t| tokens.contains(t)).count() as f64 * 2.0;
            (score > 0.0).then(|| (hash.to_string(), subject.to_string(), score))
        })
        .collect();
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let candidate_cap = k.saturating_mul(4).max(k);
    let mut ranked = Vec::new();
    for (hash, subject, log_score) in candidates.into_iter().take(candidate_cap) {
        let shown = crate::wasm_dispatch::git_call_argv(
            &["show", "--no-color", "--format=", "--unified=0", "--no-ext-diff", &hash],
            Some(root),
        );
        let diff = shown.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
        let capped: String = diff.chars().take(16_000).collect();
        let diff_tokens = rs_search::tokenize::tokenize(&capped);
        let diff_score = q_tokens.iter().filter(|t| diff_tokens.contains(t)).count() as f64;
        let score = log_score + diff_score;
        if score == 0.0 { continue; }
        ranked.push(json!({
            "hash": hash,
            "message": subject,
            "score": score,
            "evidence": {
                "log_score": log_score,
                "diff_score": diff_score,
                "indexed_fields": ["commit_message", "code_diff"],
                "diff_chars_examined": capped.len(),
            },
        }));
    }
    ranked.sort_by(|a, b| {
        b.get("score").and_then(|v| v.as_f64()).partial_cmp(&a.get("score").and_then(|v| v.as_f64())).unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(k);
    ranked
}

fn glob_match_simple(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti, mut star, mut match_i) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1; ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi); match_i = ti; pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1; match_i += 1; ti = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    pi == p.len()
}

pub fn search_filenames(pattern: &str, k: usize, cfg: &crate::ragconfig::RagConfig) -> Value {
    search_filenames_at(pattern, k, cfg, None)
}

pub fn search_filenames_at(pattern: &str, k: usize, cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> Value {
    let needle = pattern.to_lowercase();
    let is_glob = needle.contains('*') || needle.contains('?');
    let root = project_path.filter(|p| !p.is_empty()).unwrap_or(".");
    let origin = if project_path.filter(|p| !p.is_empty()).is_some() {
        crate::scan_universe::TargetOrigin::CallerNamed
    } else {
        crate::scan_universe::TargetOrigin::ProjectDefault
    };
    let file_cap = cfg.index.digest_max_files.max(20000).min(LITERAL_SCAN_MAX_FILES).max(1);
    let universe = match crate::scan_universe::list_scan_universe(root, &[], file_cap.saturating_add(1), &cfg.index, origin, false) {
        Ok(e) => e,
        Err(e) => return json!({ "ok": false, "error": e, "mode": "filename" }),
    };
    let listed = universe.files;
    let files_truncated = listed.len() > file_cap;
    let full_files: &[String] = if files_truncated { &listed[..file_cap] } else { &listed[..] };
    let hits: Vec<Value> = full_files.iter()
        .filter(|p| {
            let lp = p.to_lowercase();
            if is_glob { glob_match_simple(&needle, &lp) || glob_match_simple(&needle, lp.rsplit('/').next().unwrap_or(&lp)) }
            else { lp.contains(&needle) }
        })
        .take(k)
        .map(|p| json!({ "path": p }))
        .collect();
    let exhaustive = !files_truncated && universe.listing_complete;
    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("mode".to_string(), json!("filename"));
    out.insert("hits".to_string(), json!(hits));
    out.insert("scanned".to_string(), json!(full_files.len()));
    out.insert("file_source".to_string(), json!(universe.source.label()));
    out.insert("exhaustive".to_string(), json!(exhaustive));
    if !universe.listing_complete {
        out.insert("listing_incomplete".to_string(), json!(true));
        if let Some(reason) = &universe.walk_reason { out.insert("walk_reason".to_string(), json!(reason)); }
    }
    if files_truncated {
        out.insert("files_truncated".to_string(), json!(true));
        out.insert("files_truncated_at".to_string(), json!(file_cap));
    }
    Value::Object(out)
}

pub const LITERAL_SCAN_MAX_FILES: usize = 50_000;

const LITERAL_SCAN_MAX_LINE_BYTES: usize = 512;

const LITERAL_SCAN_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

const SKIPPED_SAMPLE_LEN: usize = 10;

const LITERAL_SCAN_PREWARM_CHUNK: usize = 256;

const PREWARM_TASK_ACTION: &str = "fs_prewarm";

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "icns", "tif", "tiff", "psd", "avif", "heic",
    "mp3", "mp4", "m4a", "mov", "avi", "mkv", "wav", "ogg", "flac", "webm",
    "woff", "woff2", "ttf", "otf", "eot",
    "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar", "zst", "jar", "war", "whl", "nupkg", "cab", "msi", "dmg", "iso",
    "exe", "dll", "so", "dylib", "o", "rlib", "rmeta", "pdb", "class", "pyc", "node", "wasm", "bin",
    "sqlite", "sqlite3", "pack", "idx", "lockb", "pdf",
];

fn has_binary_extension(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, ext)) = name.rsplit_once('.') else { return false };
    BINARY_EXTENSIONS.iter().any(|b| ext.eq_ignore_ascii_case(b))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScanOutput {
    Matches,
    Compact,
    Files,
    Count,
}

impl ScanOutput {
    pub fn parse(name: &str) -> Option<ScanOutput> {
        match name {
            "matches" | "full" => Some(ScanOutput::Matches),
            "compact" => Some(ScanOutput::Compact),
            "files" => Some(ScanOutput::Files),
            "count" => Some(ScanOutput::Count),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ScanOutput::Matches => "matches",
            ScanOutput::Compact => "compact",
            ScanOutput::Files => "files",
            ScanOutput::Count => "count",
        }
    }
}

pub const SCAN_OUTPUT_NAMES: &str = "\"matches\" (default, one object per match), \"compact\" (path:line: trimmed text), \"files\" (matching paths only), \"count\" (totals and the busiest files)";
pub const DEFAULT_REPLY_MAX_CHARS: usize = 24_000;
pub const MAX_REPLY_MAX_CHARS: usize = 400_000;
const COMPACT_TEXT_CHARS: usize = 160;
const FILES_OUTPUT_DEFAULT_LIMIT: usize = 200;
const COUNT_OUTPUT_DEFAULT_LIMIT: usize = 20;
const REPLY_METADATA_RESERVE_CHARS: usize = 3_000;
const UNREADABLE_SAMPLE_LEN: usize = 5;
const DEPENDENCY_STORE_SEGMENTS: &[&str] = &["node_modules", ".pnpm", ".yarn", "site-packages", ".venv", "target"];

pub struct LiteralScan<'a> {
    pub pattern: &'a str,
    pub root: Option<&'a str>,
    pub paths: &'a [&'a str],
    pub regex: bool,
    pub case_insensitive: bool,
    pub whole_word: bool,
    pub comments_only: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub max_matches: usize,
    pub max_files: usize,
    pub context: usize,
    /// How to combine a multi-term query: Some("phrase") (match the query verbatim as one
    /// string -- the default whenever the query splits into two or more terms), Some("and")
    /// (every term on one line) or Some("or") (a ranked union of any term).
    pub term_combination: Option<&'a str>,
    /// Wall-clock ceiling for the scan. None means the configured `index.wall_budget_ms`; a
    /// caller that runs the scan as one channel of a ranked search passes a few seconds instead.
    pub budget_ms: Option<u64>,
    /// Ceiling on matches collected from any one file. `None` keeps the historical shape: the
    /// first `max_matches` hits in walk order, so one match-dense file can spend the whole budget
    /// before the walk reaches a file that sorts later. A caller that has to answer "which files
    /// contain this" -- the dual channel's phrase scan -- sets a quota so the budget is shared
    /// across files instead of being spent by the first of them. A file that goes over quota is
    /// left part-read, so `files_quota_truncated` counts those files and the reply's
    /// `exhaustive` is false: the counts are a lower bound, never a claim of completeness.
    pub max_matches_per_file: Option<usize>,
    /// Re-read rather than reuse: skip `git ls-files` for a directory walk of the target, and
    /// serve every file from `host_read` instead of the mtime-keyed content cache.
    pub refresh: bool,
    pub output: ScanOutput,
    pub list_limit: Option<usize>,
    pub max_chars: usize,
    pub spill_name: String,
    pub verbose: bool,
}

fn path_is_inside_dependency_store(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| DEPENDENCY_STORE_SEGMENTS.contains(&segment))
}

fn parse_globs(patterns: &[String]) -> Result<Vec<crate::path_glob::PathGlob>, String> {
    patterns.iter().map(|g| crate::path_glob::PathGlob::parse(g)).collect()
}

fn glob_scopes_for<'a>(paths: &'a [&'a str]) -> Vec<Option<&'a str>> {
    if paths.is_empty() { vec![None] } else { paths.iter().map(|p| Some(*p)).collect() }
}

fn spill_lines_to_out_file(spill_name: &str, lines: &[String]) -> Option<String> {
    if lines.is_empty() { return None; }
    let relative = format!(".gm/exec-spool/out/{spill_name}");
    let mut body = lines.join("\n");
    body.push('\n');
    if crate::pkfs::write(&relative, &body) { Some(crate::pkfs::anchor(&relative)) } else { None }
}

fn split_lines_at_budget(lines: Vec<String>, budget: usize) -> (Vec<String>, Vec<String>) {
    let mut used = 0usize;
    let mut cut = lines.len();
    for (i, line) in lines.iter().enumerate() {
        used += line.chars().count() + 3;
        if used > budget && i > 0 {
            cut = i;
            break;
        }
    }
    let mut shown = lines;
    let spilled = shown.split_off(cut);
    (shown, spilled)
}

fn compact_match_line(path: &str, line_no: usize, text: &str) -> String {
    let trimmed = text.trim();
    let shown: String = trimmed.chars().take(COMPACT_TEXT_CHARS).collect();
    let ellipsis = if trimmed.chars().count() > COMPACT_TEXT_CHARS { "..." } else { "" };
    format!("{path}:{line_no}: {shown}{ellipsis}")
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
    fn find_all(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            LiteralMatcher::Substring { needle, case_insensitive, whole_word } => {
                let (haystack_owned, haystack) = if *case_insensitive {
                    let lowered = line.to_lowercase();
                    (Some(lowered), "")
                } else {
                    (None, line)
                };
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

/// An unranked multi-term union can match far more lines than a caller wants in one reply, so a
/// query that was split into terms is capped here unless the caller passed an explicit
/// max_matches/limit.
const MULTI_TERM_MAX_MATCHES: usize = 200;

const MAX_QUERY_TERMS: usize = 12;

const TERM_SPLIT_METACHARACTERS: &[char] = &['|', '(', ')', '[', ']', '{', '}', '*', '+', '?', '^', '$', '\\', '.'];

const TERM_TRIM_CHARS: &[char] = &['"', '\'', '`', ',', ';', ':', '<', '>', '(', ')', '[', ']', '{', '}'];

/// Split a whitespace-separated query into its terms.
///
/// Returns an empty vec when the query should stay one matcher: a single term, more than
/// MAX_QUERY_TERMS terms, or a regex carrying a metacharacter (so `yama|ptrace_scope` keeps
/// working as one alternation instead of being chopped into `yama|ptrace_scope` fragments).
fn carries_regex_metacharacter(pattern: &str) -> bool {
    pattern.chars().any(|c| TERM_SPLIT_METACHARACTERS.contains(&c))
}

fn query_terms(pattern: &str, regex: bool) -> Vec<String> {
    if regex && carries_regex_metacharacter(pattern) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for raw in pattern.split_whitespace() {
        let term = raw.trim_matches(|c| TERM_TRIM_CHARS.contains(&c));
        if term.chars().count() < 2 { continue; }
        if !out.iter().any(|t| t == term) { out.push(term.to_string()); }
        if out.len() > MAX_QUERY_TERMS { return Vec::new(); }
    }
    if out.len() < 2 { Vec::new() } else { out }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TermCombination {
    /// One matcher: the query is a single term, or an unsplittable regex.
    Single,
    /// Terms are matched independently and ranked by how many of them a line carries.
    Or,
    /// A line must carry every term.
    And,
    /// The whole query is matched verbatim as one string, spaces included.
    Phrase,
}

impl TermCombination {
    pub fn label(self) -> &'static str {
        match self {
            TermCombination::Single => "single_term",
            TermCombination::Or => "or_any_term_ranked_union",
            TermCombination::And => "and_all_terms_on_one_line",
            TermCombination::Phrase => "phrase_all_terms_verbatim",
        }
    }
}

fn build_matcher(pattern: &str, regex: bool, case_insensitive: bool, whole_word: bool) -> Result<LiteralMatcher, String> {
    if regex {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map(LiteralMatcher::Regex)
            .map_err(|e| format!("invalid regular expression: {e}"))
    } else {
        Ok(LiteralMatcher::Substring {
            needle: if case_insensitive { pattern.to_lowercase() } else { pattern.to_string() },
            case_insensitive,
            whole_word,
        })
    }
}

const SCAN_CACHE_MAX_BYTES: usize = 48 * 1024 * 1024;
const SCAN_CACHE_MAX_ENTRIES: usize = 20_000;
const SCAN_CACHE_MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const HINT_MAX_CHARS: usize = 360;
const HINT_TERM_DETAIL_CHARS: usize = 200;

struct ScanCacheEntry {
    mtime_ms: u64,
    size: u64,
    content: String,
}

/// File contents keyed by (mtime_ms, size), so any change to a file invalidates its entry.
/// Kept in a guest static, which survives across dispatches while the host pools the Store.
struct ScanCache {
    root: String,
    bytes: usize,
    entries: std::collections::BTreeMap<String, ScanCacheEntry>,
    order: std::collections::VecDeque<String>,
    hits: usize,
    misses: usize,
}

impl ScanCache {
    fn fresh(root: &str) -> Self {
        ScanCache { root: root.to_string(), bytes: 0, entries: std::collections::BTreeMap::new(), order: std::collections::VecDeque::new(), hits: 0, misses: 0 }
    }

    fn get(&mut self, path: &str, mtime_ms: u64, size: u64) -> Option<String> {
        let entry = self.entries.get(path)?;
        if entry.mtime_ms != mtime_ms || entry.size != size { return None; }
        self.hits += 1;
        Some(entry.content.clone())
    }

    fn insert(&mut self, path: &str, mtime_ms: u64, size: u64, content: &str) {
        if content.len() > SCAN_CACHE_MAX_FILE_BYTES { return; }
        self.misses += 1;
        match self.entries.remove(path) {
            Some(old) => self.bytes = self.bytes.saturating_sub(old.content.len()),
            None => self.order.push_back(path.to_string()),
        }
        self.bytes += content.len();
        self.entries.insert(path.to_string(), ScanCacheEntry { mtime_ms, size, content: content.to_string() });
        while (self.bytes > SCAN_CACHE_MAX_BYTES || self.entries.len() > SCAN_CACHE_MAX_ENTRIES) && !self.order.is_empty() {
            let Some(victim) = self.order.pop_front() else { break };
            if let Some(old) = self.entries.remove(&victim) { self.bytes = self.bytes.saturating_sub(old.content.len()); }
        }
    }
}

static SCAN_CACHE: std::sync::Mutex<Option<ScanCache>> = std::sync::Mutex::new(None);

fn scan_cache_take(root: &str) -> ScanCache {
    let mut guard = match SCAN_CACHE.lock() { Ok(g) => g, Err(poisoned) => poisoned.into_inner() };
    match guard.take() {
        Some(cache) if cache.root == root => cache,
        _ => ScanCache::fresh(root),
    }
}

fn scan_cache_put(cache: ScanCache) {
    if let Ok(mut guard) = SCAN_CACHE.lock() { *guard = Some(cache); }
}

/// Indices into `items` that keep at most one item per path per round, until `cap` is full: take
/// item 0 of every path, then item 1 of every path that still has one, and so on. A cap spent in
/// walk order lets one match-dense file near the front of the walk consume the whole budget, so a
/// file that sorts later -- and holds the symbol the caller asked about -- answers with nothing at
/// all and reads as "this does not exist". Round-robin costs a path representation to every file
/// that matched, and only then spends the remainder on seconds and thirds.
pub fn fair_share_indices<T, F>(items: &[T], cap: usize, path_of: F) -> Vec<usize>
where
    F: Fn(&T) -> String,
{
    if items.len() <= cap { return (0..items.len()).collect(); }
    let mut queues: Vec<Vec<usize>> = Vec::new();
    let mut slot_of_path: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let path = path_of(item);
        let slot = match slot_of_path.get(&path) {
            Some(&s) => s,
            None => {
                let s = queues.len();
                slot_of_path.insert(path, s);
                queues.push(Vec::new());
                s
            }
        };
        queues[slot].push(i);
    }
    let mut out: Vec<usize> = Vec::with_capacity(cap);
    let mut round = 0usize;
    while out.len() < cap {
        let mut took_this_round = false;
        for queue in queues.iter() {
            if out.len() >= cap { break; }
            if let Some(&i) = queue.get(round) {
                out.push(i);
                took_this_round = true;
            }
        }
        if !took_this_round { break; }
        round += 1;
    }
    out.sort_unstable();
    out
}

pub fn scan_literal(req: &LiteralScan, cfg: &crate::ragconfig::RagConfig) -> Value {
    if req.pattern.is_empty() {
        return json!({ "ok": false, "error": "pattern required -- an exhaustive scan needs something to match" });
    }
    let matcher = match build_matcher(req.pattern, req.regex, req.case_insensitive, req.whole_word) {
        Ok(m) => m,
        Err(e) => return json!({
            "ok": false,
            "error": format!("mode \"{}\" got an {e}", if req.regex { "regex" } else { "literal" }),
            "pattern": req.pattern,
        }),
    };

    let root = req.root.filter(|p| !p.is_empty()).unwrap_or(".");
    let scope = req.paths.first().copied().filter(|p| !p.is_empty());
    let glob_scopes: Vec<Option<&str>> = glob_scopes_for(req.paths);
    let origin = if req.root.filter(|p| !p.is_empty()).is_some() {
        crate::scan_universe::TargetOrigin::CallerNamed
    } else {
        crate::scan_universe::TargetOrigin::ProjectDefault
    };
    let include_globs = match parse_globs(&req.include_globs) {
        Ok(parsed) => parsed,
        Err(e) => return json!({ "ok": false, "error": e, "pattern": req.pattern }),
    };
    let exclude_globs = match parse_globs(&req.exclude_globs) {
        Ok(parsed) => parsed,
        Err(e) => return json!({ "ok": false, "error": e, "pattern": req.pattern }),
    };
    let has_glob_filter = !include_globs.is_empty() || !exclude_globs.is_empty();
    let admitted = |path: &str| -> bool {
        (include_globs.is_empty() || include_globs.iter().any(|g| glob_scopes.iter().any(|s| g.admits(root, *s, path))))
            && !exclude_globs.iter().any(|g| glob_scopes.iter().any(|s| g.admits(root, *s, path)))
    };
    let file_cap = req.max_files.min(LITERAL_SCAN_MAX_FILES).max(1);
    let listing_started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let universe = match crate::scan_universe::list_scan_universe(root, req.paths, file_cap.saturating_add(1), &cfg.index, origin, req.refresh) {
        Ok(u) => u,
        Err(e) => return json!({ "ok": false, "error": e, "pattern": req.pattern }),
    };
    let listed = universe.files;
    let files_truncated = listed.len() > file_cap;
    let files: &[String] = if files_truncated { &listed[..file_cap] } else { &listed[..] };
    let files_matching_glob = files.iter().filter(|p| admitted(p)).count();
    let glob_matched_no_files = has_glob_filter && !files.is_empty() && files_matching_glob == 0;

    let started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let listing_ms = started_ms.saturating_sub(listing_started_ms);
    let budget_ms = req.budget_ms.unwrap_or(cfg.index.wall_budget_ms);
    let mut stat_ms = 0u64;
    let mut read_ms = 0u64;
    let mut prewarm_ms = 0u64;

    let terms = query_terms(req.pattern, req.regex);
    let want_and = req.term_combination == Some("and");
    // A multi-word query is ONE phrase unless the caller asks for the loose union: ranking
    // "fn sys_wait4" by how many of its terms a line carries answers with every `fn` in the tree
    // and buries the definition the caller asked for.
    let want_phrase = match req.term_combination {
        Some("phrase") => true,
        Some(_) => false,
        None => !terms.is_empty(),
    };
    let mut term_matchers: Vec<LiteralMatcher> = Vec::new();
    if !terms.is_empty() && !want_phrase {
        for term in &terms {
            match build_matcher(term, req.regex, req.case_insensitive, req.whole_word) {
                Ok(m) => term_matchers.push(m),
                Err(_) => { term_matchers.clear(); break; }
            }
        }
    }
    let multi = !term_matchers.is_empty();
    let combination = if want_phrase {
        TermCombination::Phrase
    } else if !multi {
        TermCombination::Single
    } else if want_and {
        TermCombination::And
    } else {
        TermCombination::Or
    };
    let max_matches = if multi { req.max_matches.min(MULTI_TERM_MAX_MATCHES) } else { req.max_matches };
    let per_file_quota = req.max_matches_per_file.filter(|q| *q > 0);
    // The 6x headroom exists so the relevance sort has a pool to choose from. A per-file quota
    // already spends `max_matches` across paths, so the pool buys nothing and only makes the walk
    // read more files: collect to `max_matches` and stop.
    let hit_cap = if per_file_quota.is_some() {
        max_matches
    } else if multi {
        max_matches.saturating_mul(6).clamp(200, 2_000)
    } else {
        max_matches
    };

    let mut candidates: Vec<(usize, usize, usize, Value)> = Vec::new();
    let mut compact_lines: Vec<String> = Vec::new();
    let mut file_line_counts: Vec<(String, usize)> = Vec::new();
    let mut unreadable_dependency_files = 0usize;
    let mut unreadable_sample: Vec<String> = Vec::new();
    let mut emitted_matches = 0usize;
    let mut files_scanned = 0usize;
    let mut files_with_matches = 0usize;
    let mut files_quota_truncated = 0usize;
    let mut files_skipped_too_large: Vec<String> = Vec::new();
    let mut files_skipped_too_large_count = 0usize;
    let mut files_skipped_binary_extension = 0usize;
    let mut files_skipped_binary = 0usize;
    let mut files_with_nul_scanned = 0usize;
    let mut files_without_comment_syntax = 0usize;
    let mut files_unreadable = 0usize;
    let mut lines_with_matches = 0usize;
    let mut occurrence_count = 0usize;
    let mut lines_all_terms = 0usize;
    let mut lines_phrase_matched = 0usize;
    let mut matches_truncated = false;
    let mut budget_exhausted = false;
    let mut term_lines: Vec<usize> = vec![0usize; term_matchers.len()];
    let mut term_files: Vec<usize> = vec![0usize; term_matchers.len()];
    let mut cache = if req.refresh { ScanCache::fresh(root) } else { scan_cache_take(root) };

    let wanted = |path: &String| admitted(path) && !has_binary_extension(path);
    let mut prewarmed_until = 0usize;
    for (index, path) in files.iter().enumerate() {
        if unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms) >= budget_ms {
            budget_exhausted = true;
            break;
        }
        if index >= prewarmed_until {
            prewarmed_until = (index + LITERAL_SCAN_PREWARM_CHUNK).min(files.len());
            let chunk: Vec<&String> = files[index..prewarmed_until].iter().filter(|p| wanted(p)).collect();
            if !chunk.is_empty() {
                let prewarm_started = unsafe { crate::wasm_dispatch::host_now_ms() };
                crate::wasm_dispatch::host_task(PREWARM_TASK_ACTION, &json!({ "paths": chunk }));
                prewarm_ms += unsafe { crate::wasm_dispatch::host_now_ms() } - prewarm_started;
            }
        }
        if !admitted(path) { continue; }
        if has_binary_extension(path) { files_skipped_binary_extension += 1; continue; }
        let stat_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        let stat = host_stat(path);
        let read_started = unsafe { crate::wasm_dispatch::host_now_ms() };
        stat_ms += read_started - stat_started;
        let (mtime_ms, size) = match &stat {
            Some(stat) => (
                stat.get("mtime_ms").and_then(|v| v.as_u64())
                    .or_else(|| stat.get("mtimeMs").and_then(|v| v.as_u64()))
                    .unwrap_or(0),
                stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            ),
            None => (0, 0),
        };
        if stat.is_some() {
            if size > LITERAL_SCAN_MAX_FILE_BYTES {
                files_skipped_too_large_count += 1;
                if files_skipped_too_large.len() < SKIPPED_SAMPLE_LEN { files_skipped_too_large.push(path.clone()); }
                continue;
            }
            if size == 0 { files_scanned += 1; continue; }
        }
        let content = if mtime_ms > 0 {
            match cache.get(path, mtime_ms, size) {
                Some(cached) => Some(cached),
                None => {
                    let fresh = host_read(path);
                    if let Some(ref text) = fresh { cache.insert(path, mtime_ms, size, text); }
                    fresh
                }
            }
        } else {
            host_read(path)
        };
        read_ms += unsafe { crate::wasm_dispatch::host_now_ms() } - read_started;
        let Some(content) = content else {
            if stat.is_some() {
                files_skipped_binary += 1;
            } else if path_is_inside_dependency_store(path) {
                unreadable_dependency_files += 1;
            } else {
                files_unreadable += 1;
                if unreadable_sample.len() < UNREADABLE_SAMPLE_LEN { unreadable_sample.push(path.clone()); }
            }
            continue;
        };
        if content.as_bytes().contains(&0u8) {
            let has_source_extension = path.rfind('.').and_then(|dot| lang_for_ext(&path[dot..])).is_some();
            if !has_source_extension { files_skipped_binary += 1; continue; }
            files_with_nul_scanned += 1;
        }
        files_scanned += 1;
        let comment_spans = if req.comments_only {
            match crate::comment_spans::comment_spans(path, &content) {
                Some(spans) => Some(spans),
                None => { files_without_comment_syntax += 1; continue; }
            }
        } else {
            None
        };
        let mut this_file_matched = false;
        let mut this_file_quota_hit = false;
        let mut this_file_collected = 0usize;
        let mut this_file_lines = 0usize;
        let mut term_seen_in_file: Vec<bool> = vec![false; term_matchers.len()];
        let all_lines: Vec<&str> = if req.context > 0 { content.lines().collect() } else { Vec::new() };
        let mut line_start_offset = 0usize;
        for (idx, raw_line) in content.split_inclusive('\n').enumerate() {
            let line = raw_line.trim_end_matches(['\n', '\r']);
            let line_offset = line_start_offset;
            line_start_offset += raw_line.len();
            let (found, distinct) = if !multi {
                let mut found = matcher.find_all(line);
                if let Some(spans) = &comment_spans {
                    found.retain(|&(start, _)| crate::comment_spans::span_contains(spans, line_offset + start));
                }
                if found.is_empty() { continue; }
                (found, 1usize)
            } else {
                let mut found: Vec<(usize, usize)> = Vec::new();
                let mut distinct = 0usize;
                for (ti, term_matcher) in term_matchers.iter().enumerate() {
                    let mut hits = term_matcher.find_all(line);
                    if let Some(spans) = &comment_spans {
                        hits.retain(|&(start, _)| crate::comment_spans::span_contains(spans, line_offset + start));
                    }
                    if hits.is_empty() { continue; }
                    distinct += 1;
                    term_lines[ti] += 1;
                    term_seen_in_file[ti] = true;
                    found.extend(hits);
                }
                if distinct == 0 { continue; }
                if distinct == term_matchers.len() { lines_all_terms += 1; }
                if combination == TermCombination::And && distinct != term_matchers.len() { continue; }
                if !matcher.find_all(line).is_empty() { lines_phrase_matched += 1; }
                found.sort_by_key(|(start, _)| *start);
                (found, distinct)
            };
            this_file_matched = true;
            this_file_lines += 1;
            lines_with_matches += 1;
            occurrence_count += found.len();
            if matches!(req.output, ScanOutput::Files | ScanOutput::Count) { continue; }
            // Over quota for this file: stop reading it instead of letting it take a slot another
            // file's only match needs. Leaving the rest of this file unread also leaves the scan's
            // own wall budget for files the walk has not reached yet, which is the whole point --
            // a match-dense file early in the walk otherwise spends the budget that would have
            // reached every file after it.
            if let Some(quota) = per_file_quota {
                if this_file_collected >= quota {
                    this_file_quota_hit = true;
                    break;
                }
                this_file_collected += 1;
            }
            if req.output == ScanOutput::Compact {
                if emitted_matches >= req.max_matches { matches_truncated = true; break; }
                emitted_matches += 1;
                compact_lines.push(compact_match_line(path, idx + 1, line));
                continue;
            }
            if candidates.len() >= hit_cap { matches_truncated = true; break; }
            let (start, end) = found[0];
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
            hit.insert("match".to_string(), json!(line.get(start..end).unwrap_or(req.pattern)));
            hit.insert("occurrence_count".to_string(), json!(found.len()));
            hit.insert("text".to_string(), json!(shown.trim_end()));
            if text_truncated { hit.insert("text_truncated".to_string(), json!(true)); }
            if multi { hit.insert("terms_matched".to_string(), json!(distinct)); }
            if req.context > 0 {
                let first_context_line = idx.saturating_sub(req.context);
                let last_context_line = idx.saturating_add(1 + req.context).min(all_lines.len());
                hit.insert("before".to_string(), json!(all_lines[first_context_line..idx].iter().map(|l| l.trim_end()).collect::<Vec<_>>()));
                hit.insert("after".to_string(), json!(all_lines[idx + 1..last_context_line].iter().map(|l| l.trim_end()).collect::<Vec<_>>()));
            }
            candidates.push((distinct, found.len(), candidates.len(), Value::Object(hit)));
            if matches_truncated { break; }
        }
        for (ti, seen) in term_seen_in_file.iter().enumerate() {
            if *seen { term_files[ti] += 1; }
        }
        if this_file_matched {
            files_with_matches += 1;
            file_line_counts.push((path.clone(), this_file_lines));
        }
        if this_file_quota_hit {
            files_quota_truncated += 1;
            matches_truncated = true;
        }
        // A quota hit is not a full pool: it only stopped this file, and every later file still
        // needs its slot. Files/Count and Compact never fill `candidates`, so their cap is the
        // emitted-match counter instead.
        if matches_truncated && (req.output != ScanOutput::Matches || candidates.len() >= hit_cap) { break; }
    }

    let mut matches: Vec<Value> = if multi {
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)));
        if candidates.len() > max_matches {
            matches_truncated = true;
            if per_file_quota.is_some() {
                // Same rule as collection: spend the returned rows across paths, not down the walk
                // order. A relevance sort that ties (same distinct-term count, same occurrence
                // count) falls back to insertion order, which is the walk order -- so a plain
                // truncate would silently reintroduce the bias the per-file quota removed.
                let keep = fair_share_indices(&candidates, max_matches, |c| {
                    c.3.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string()
                });
                candidates = keep.into_iter().map(|i| candidates[i].clone()).collect();
            } else {
                candidates.truncate(max_matches);
            }
        }
        candidates.into_iter().map(|(_, _, _, hit)| hit).collect()
    } else {
        candidates.into_iter().map(|(_, _, _, hit)| hit).collect()
    };
    let cache_hits = cache.hits;
    let cache_misses = cache.misses;
    let cache_entries = cache.entries.len();
    let cache_bytes = cache.bytes;
    scan_cache_put(cache);

    let exhaustive = !files_truncated
        && !matches_truncated
        && !budget_exhausted
        && files_skipped_too_large_count == 0
        && files_unreadable == 0
        && universe.listing_complete
        && !glob_matched_no_files;

    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("mode".to_string(), json!(if req.regex { "regex" } else { "literal" }));
    if req.verbose {
        if req.output != ScanOutput::Matches { out.insert("output".to_string(), json!(req.output.label())); }
        out.insert("pattern".to_string(), json!(req.pattern));
        out.insert("root".to_string(), json!(crate::scan_universe::absolute_root_for_message(root)));
        if let Some(p) = scope { out.insert("path".to_string(), json!(p)); }
        if req.paths.len() > 1 { out.insert("paths".to_string(), json!(req.paths)); }
        out.insert("case_insensitive".to_string(), json!(req.case_insensitive));
        if !req.regex { out.insert("whole_word".to_string(), json!(req.whole_word)); }
        if !req.include_globs.is_empty() { out.insert("path_glob".to_string(), json!(req.include_globs.join(", "))); }
        if !req.exclude_globs.is_empty() { out.insert("exclude_glob".to_string(), json!(req.exclude_globs.join(", "))); }
    }
    if req.comments_only {
        out.insert("comments_only".to_string(), json!(true));
        if req.verbose || files_without_comment_syntax > 0 {
            out.insert("files_without_comment_syntax".to_string(), json!(files_without_comment_syntax));
        }
    }
    if has_glob_filter && (req.verbose || glob_matched_no_files) { out.insert("files_matching_glob".to_string(), json!(files_matching_glob)); }
    if glob_matched_no_files { out.insert("glob_matched_no_files".to_string(), json!(true)); }
    let file_source = universe.source.label();
    if req.verbose || file_source != "git" {
        out.insert("file_source".to_string(), json!(file_source));
        out.insert("file_source_detail".to_string(), json!(universe.source.detail()));
    }
    if req.refresh {
        out.insert("refreshed".to_string(), json!(true));
        out.insert("refreshed_note".to_string(), json!(
            "this scan re-read from disk: git ls-files was skipped for a directory walk and the mtime-keyed content cache was bypassed, so edits and untracked files are visible"
        ));
    }
    if !universe.listing_complete {
        out.insert("listing_incomplete".to_string(), json!(true));
        if let Some(reason) = &universe.walk_reason { out.insert("walk_reason".to_string(), json!(reason)); }
    }
    let pruned_by_foreign_rule = universe.excluded.iter()
        .filter(|e| e.rule != crate::scan_universe::OWN_STATE_RULE)
        .count();
    if !req.verbose && pruned_by_foreign_rule > 0 {
        out.insert("excluded_by_rule_count".to_string(), json!(pruned_by_foreign_rule));
    }
    if req.verbose && !universe.excluded.is_empty() {
        let cap = 200usize;
        let shown: Vec<Value> = universe.excluded.iter().take(cap)
            .map(|e| match e.files {
                Some(files) => json!({ "path": e.path, "rule": e.rule, "files": files }),
                None => json!({ "path": e.path, "rule": e.rule }),
            })
            .collect();
        let mut paths_by_rule = serde_json::Map::new();
        for e in &universe.excluded {
            let seen = paths_by_rule.get(e.rule).and_then(|v| v.as_u64()).unwrap_or(0);
            paths_by_rule.insert(e.rule.to_string(), json!(seen + 1));
        }
        out.insert("excluded_by_rule".to_string(), json!(shown));
        out.insert("excluded_by_rule_summary".to_string(), Value::Object(paths_by_rule));
        out.insert("excluded_by_rule_count".to_string(), json!(universe.excluded.len()));
    }
    out.insert("term_combination".to_string(), json!(combination.label()));
    if let Some(quota) = per_file_quota {
        out.insert("max_matches_per_file".to_string(), json!(quota));
        out.insert("files_quota_truncated".to_string(), json!(files_quota_truncated));
    }
    if multi {
        let per_term: Vec<Value> = term_matchers.iter().enumerate()
            .zip(terms.iter())
            .map(|((ti, _), term)| json!({ "term": term, "lines": term_lines[ti], "files": term_files[ti] }))
            .collect();
        out.insert("terms".to_string(), Value::Array(per_term));
        out.insert("lines_matching_all_terms".to_string(), json!(lines_all_terms));
        out.insert("phrase_match_count".to_string(), json!(lines_phrase_matched));
        out.insert("query_note".to_string(), json!(match combination {
            TermCombination::And => format!(
                "combine:\"and\": every line returned carries all {} terms; combine:\"or\" would add the lines carrying fewer, ranked strictly below them, and combine:\"phrase\" (the default for a multi-word query) would match the query verbatim",
                term_matchers.len()
            ),
            _ if lines_all_terms == 0 => format!(
                "no line carries all {} terms -- these hits are the ranked union (lines matching the most terms first); combine:\"phrase\" (the default for a multi-word query) matches the query verbatim instead, combine:\"and\" would return nothing here",
                term_matchers.len()
            ),
            _ => format!(
                "ranked union (combine:\"or\"): lines carrying all {} terms come first, every line carrying fewer ranks strictly below them; combine:\"and\" keeps only the all-terms lines, combine:\"phrase\" (the default) matches the query verbatim",
                term_matchers.len()
            ),
        }));
    } else if req.regex && carries_regex_metacharacter(req.pattern) {
        out.insert("query_note".to_string(), json!(
            "the query was matched as ONE regular expression: mode \"regex\" honours every metacharacter as written, so a \"|\" alternates and a space inside the pattern belongs to the pattern -- the query was never split into terms and combine:\"or\"/\"and\" have no effect on it"
        ));
    } else if req.pattern.split_whitespace().count() > 1 {
        out.insert("query_note".to_string(), json!(
            "the query was matched as ONE phrase: a line must contain it verbatim, spaces included -- pass combine:\"or\" to split it into terms and rank by how many a line carries, or combine:\"and\" to require all of them on one line"
        ));
    }
    let match_count = match req.output {
        ScanOutput::Matches => matches.len(),
        ScanOutput::Compact => compact_lines.len(),
        ScanOutput::Files | ScanOutput::Count => lines_with_matches,
    };
    if match_count == 0 {
        let mut hint = if multi {
            let mut detail: Vec<String> = Vec::new();
            let mut used = 0usize;
            for (ti, term) in terms.iter().enumerate().take(term_matchers.len()) {
                let piece = format!("{term} {}L/{}F", term_lines[ti], term_files[ti]);
                if used + piece.len() > HINT_TERM_DETAIL_CHARS { break; }
                used += piece.len() + 2;
                detail.push(piece);
            }
            let detail = detail.join(", ");
            match combination {
                TermCombination::And => format!(
                    "no line matched all {} terms (combine:\"and\"); per term: {}; pass combine:\"or\" for a ranked union of any term, or combine:\"phrase\" (the default for a multi-word query) for the query verbatim",
                    term_matchers.len(), detail
                ),
                _ => format!(
                    "no line matched any of the {} terms (combine:\"or\"); per term: {}; the query was split on whitespace -- the default combine:\"phrase\" matches it verbatim as one string",
                    term_matchers.len(), detail
                ),
            }
        } else if req.regex && carries_regex_metacharacter(req.pattern) {
            format!(
                "no line matched /{}/ as ONE regular expression: mode \"regex\" honoured every metacharacter as written, so a \"|\" alternated and the query was never split into terms",
                req.pattern
            )
        } else if req.pattern.split_whitespace().count() > 1 {
            format!(
                "no line matched the query as ONE phrase: \"{}\" was matched verbatim, spaces included; split it into terms and pass combine:\"or\", or combine:\"and\" to require all of them on one line",
                req.pattern
            )
        } else {
            format!("no line matched \"{}\" in {} files scanned", req.pattern, files_scanned)
        };
        hint.truncate(HINT_MAX_CHARS);
        out.insert("hint".to_string(), json!(hint));
    }
    if cache_hits + cache_misses > 0 {
        out.insert("scan_cache".to_string(), json!({
            "hits": cache_hits,
            "misses": cache_misses,
            "entries": cache_entries,
            "cached_bytes": cache_bytes,
        }));
    }
    let elapsed_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms);
    if req.verbose {
        out.insert("match_count".to_string(), json!(match_count));
        out.insert("occurrence_count".to_string(), json!(occurrence_count));
        out.insert("lines_with_matches".to_string(), json!(lines_with_matches));
        out.insert("files_with_matches".to_string(), json!(files_with_matches));
        out.insert("files_scanned".to_string(), json!(files_scanned));
        out.insert("files_listed".to_string(), json!(files.len()));
        out.insert("elapsed_ms".to_string(), json!(elapsed_ms));
        out.insert("phase_ms".to_string(), json!({
            "listing": listing_ms,
            "stat": stat_ms,
            "read": read_ms,
            "prewarm": prewarm_ms,
            "match_and_other": elapsed_ms.saturating_sub(stat_ms + read_ms + prewarm_ms),
        }));
    } else {
        out.insert("count".to_string(), json!(format!(
            "{lines_with_matches} lines, {occurrence_count} occurrences, {files_with_matches} files of {files_scanned} scanned"
        )));
    }
    out.insert("exhaustive".to_string(), json!(exhaustive));
    if files_truncated {
        out.insert("files_truncated".to_string(), json!(true));
        out.insert("files_truncated_at".to_string(), json!(file_cap));
    }
    if matches_truncated {
        out.insert("matches_truncated".to_string(), json!(true));
        out.insert("matches_truncated_at".to_string(), json!(max_matches));
    }
    if budget_exhausted {
        out.insert("budget_exhausted".to_string(), json!(true));
        out.insert("timed_out".to_string(), json!(true));
        out.insert("budget_ms".to_string(), json!(budget_ms));
    }
    if files_skipped_too_large_count > 0 {
        out.insert("files_skipped_too_large_count".to_string(), json!(files_skipped_too_large_count));
        out.insert("files_skipped_too_large".to_string(), json!(files_skipped_too_large));
        out.insert("max_file_bytes".to_string(), json!(LITERAL_SCAN_MAX_FILE_BYTES));
    }
    if req.verbose && files_skipped_binary_extension > 0 { out.insert("files_skipped_binary_extension".to_string(), json!(files_skipped_binary_extension)); }
    if files_skipped_binary > 0 { out.insert("files_skipped_binary".to_string(), json!(files_skipped_binary)); }
    if files_with_nul_scanned > 0 { out.insert("files_with_nul_scanned".to_string(), json!(files_with_nul_scanned)); }
    if files_unreadable > 0 {
        out.insert("files_unreadable".to_string(), json!(files_unreadable));
        out.insert("files_unreadable_sample".to_string(), json!(unreadable_sample));
    }
    if unreadable_dependency_files > 0 {
        out.insert("files_unreadable_in_dependency_dirs".to_string(), json!(unreadable_dependency_files));
    }
    if !exhaustive {
        out.insert("exhaustive_note".to_string(), json!(
            "at least one bound fired -- this is NOT every match in the tree; the files_truncated/matches_truncated/budget_exhausted/files_skipped_* fields above name which"
        ));
    }
    let budget = req.max_chars.saturating_sub(REPLY_METADATA_RESERVE_CHARS).max(1_000);
    let mut spilled: Vec<String> = Vec::new();
    match req.output {
        ScanOutput::Matches => {
            let mut used = 0usize;
            let mut cut = matches.len();
            for (i, m) in matches.iter().enumerate() {
                used += m.to_string().chars().count() + 2;
                if used > budget && i > 0 {
                    cut = i;
                    break;
                }
            }
            let rest = matches.split_off(cut);
            spilled = rest.iter().map(|m| {
                format!(
                    "{}:{}:{}: {}",
                    m.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                    m.get("line").and_then(|v| v.as_u64()).unwrap_or(0),
                    m.get("column").and_then(|v| v.as_u64()).unwrap_or(0),
                    m.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                )
            }).collect();
            out.insert("matches".to_string(), Value::Array(matches));
        }
        ScanOutput::Compact => {
            let (shown, rest) = split_lines_at_budget(compact_lines, budget);
            spilled = rest;
            out.insert("matches".to_string(), json!(shown));
        }
        ScanOutput::Files => {
            let cap = req.list_limit.unwrap_or(FILES_OUTPUT_DEFAULT_LIMIT).max(1);
            let all: Vec<String> = file_line_counts.iter().map(|(p, _)| p.clone()).collect();
            if all.len() > cap { out.insert("files_truncated_at_limit".to_string(), json!(cap)); }
            let listed: Vec<String> = all.into_iter().take(cap).collect();
            let (shown, rest) = split_lines_at_budget(listed, budget);
            spilled = rest;
            out.insert("files".to_string(), json!(shown));
        }
        ScanOutput::Count => {
            let cap = req.list_limit.unwrap_or(COUNT_OUTPUT_DEFAULT_LIMIT).max(1);
            let mut busiest = file_line_counts.clone();
            busiest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let rows: Vec<String> = busiest.into_iter().take(cap).map(|(p, n)| format!("{n} {p}")).collect();
            out.insert("busiest_files".to_string(), json!(rows));
        }
    }
    if !spilled.is_empty() {
        let spilled_count = spilled.len();
        out.insert("reply_truncated".to_string(), json!(true));
        out.insert("max_chars".to_string(), json!(req.max_chars));
        out.insert("spilled_count".to_string(), json!(spilled_count));
        match spill_lines_to_out_file(&req.spill_name, &spilled) {
            Some(file) => {
                out.insert("spill_file".to_string(), json!(file));
                out.insert("reply_note".to_string(), json!(format!(
                    "the reply hit max_chars={}; {spilled_count} more entries (still part of this exhaustive result) are one per line in spill_file, readable directly; raise max_chars, narrow path/glob, or use output \"files\"/\"count\" to see less",
                    req.max_chars
                )));
            }
            None => {
                out.insert("reply_note".to_string(), json!(format!(
                    "the reply hit max_chars={} and {spilled_count} entries were dropped because the spill file could not be written; narrow path/glob or raise max_chars",
                    req.max_chars
                )));
            }
        }
    }
    Value::Object(out)
}

pub struct CommentScan<'a> {
    pub root: Option<&'a str>,
    pub paths: &'a [&'a str],
    pub path_glob: Option<&'a str>,
    pub exclude_globs: Vec<String>,
    pub max_matches: usize,
    pub max_files: usize,
    pub refresh: bool,
}

const COMMENT_TEXT_MAX_BYTES: usize = 2000;

const COMMENT_SLASH_EXTENSIONS: &[&str] = &[
    ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".mts", ".cts",
    ".rs", ".go", ".c", ".h", ".cpp", ".cc", ".hpp", ".hh", ".cxx", ".hxx", ".ino",
    ".glsl", ".vert", ".frag", ".comp", ".geom", ".tesc", ".tese", ".vsh", ".fsh", ".glslv", ".glslf",
    ".java", ".cs", ".php", ".phtml", ".swift", ".kt", ".kts", ".scala", ".sc", ".zig",
    ".d", ".groovy", ".gradle",
    ".dsp", ".lib",
    ".css", ".scss", ".sass", ".less",
];

const COMMENT_HASH_EXTENSIONS: &[&str] = &[
    ".sh", ".bash", ".zsh", ".ksh", ".fish",
    ".yaml", ".yml", ".toml", ".ini", ".cfg", ".conf", ".properties", ".env",
    ".py", ".pyi", ".rb", ".pl", ".pm", ".r", ".jl", ".ex", ".exs", ".tcl", ".awk",
    ".ps1", ".psm1", ".psd1", ".nuspec",
    ".makefile", ".mk", ".cmake", ".gitignore", ".gitattributes", ".dockerignore", ".editorconfig",
];

const COMMENT_HASH_FILENAMES: &[&str] = &[
    "dockerfile", "makefile", "gnumakefile", "cmakelists.txt", "rakefile", "gemfile", "procfile",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentSyntax {
    Slash,
    Hash,
}

impl CommentSyntax {
    fn label(self) -> &'static str {
        match self {
            CommentSyntax::Slash => "slash",
            CommentSyntax::Hash => "hash",
        }
    }
}

fn comment_syntax_for_path(path: &str) -> Option<CommentSyntax> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lowered_name = name.to_lowercase();
    if COMMENT_HASH_FILENAMES.iter().any(|n| lowered_name == *n)
        || lowered_name.starts_with("dockerfile")
        || lowered_name.starts_with("makefile")
        || lowered_name.starts_with("gnumakefile")
    {
        return Some(CommentSyntax::Hash);
    }
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!(".{ext}").to_lowercase(),
        _ => return None,
    };
    if COMMENT_SLASH_EXTENSIONS.iter().any(|e| ext == *e) { return Some(CommentSyntax::Slash); }
    if COMMENT_HASH_EXTENSIONS.iter().any(|e| ext == *e) { return Some(CommentSyntax::Hash); }
    None
}

const DIRECTIVE_BODY_PREFIXES: &[&str] = &[
    "syntax=", "shellcheck", "noqa", "type:", "pylint:", "eslint", "prettier-ignore", "tslint:",
    "rustfmt:", "clippy::", "clippy:", "golangci-lint", "hadolint", "yamllint", "ansible-lint",
    "luacheck:", "rubocop:", "checkov:", "tfsec:", "dockerfile:", "editorconfig-checker",
    "@ts-ignore", "@ts-expect-error", "@ts-nocheck", "@ts-check", "coverage:", "c8 ", "v8 ignore",
    "istanbul ignore", "sourceMappingURL=", "region", "endregion", "pragma", "include",
    "formatter:", "forbid", "allow", "deny", "warn", "expect", "cfg:", "tool:", "autopep8:",
    "flake8:", "mypy:", "pyright:", "ruff:", "biome-ignore", "deno-lint-ignore",
];

fn comment_body_is_directive(body: &str) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() { return false; }
    if trimmed.starts_with('!') { return true; }
    let lowered = trimmed.to_lowercase();
    DIRECTIVE_BODY_PREFIXES.iter().any(|p| lowered.starts_with(p))
}

struct CommentSpan {
    line: usize,
    column: usize,
    kind: &'static str,
    text: String,
    text_truncated: bool,
    inline: bool,
}

fn push_span(comments: &mut Vec<CommentSpan>, directives: &mut Vec<CommentSpan>, span: CommentSpan) {
    let body = if span.kind == "block" {
        span.text.get(2..).and_then(|s| s.strip_suffix("*/")).unwrap_or("").to_string()
    } else if let Some(rest) = span.text.strip_prefix("//") {
        rest.to_string()
    } else {
        span.text.strip_prefix('#').unwrap_or("").to_string()
    };
    if comment_body_is_directive(&body) { directives.push(span) } else { comments.push(span) }
}

fn line_has_code_before(content: &str, line_start: usize, upto: usize) -> bool {
    if upto <= line_start { return false }
    content.get(line_start..upto).unwrap_or("").trim().chars().any(|c| !c.is_whitespace())
}

fn clipped_text(full: &str) -> (String, bool) {
    if full.len() > COMMENT_TEXT_MAX_BYTES {
        (full.chars().take(COMMENT_TEXT_MAX_BYTES).collect(), true)
    } else {
        (full.to_string(), false)
    }
}

fn hash_opens_comment(bytes: &[u8], at: usize, line_start: usize) -> bool {
    if at <= line_start { return true; }
    bytes.get(at - 1).map(|b| b.is_ascii_whitespace()).unwrap_or(false)
}

fn scan_content_for_comments(content: &str, syntax: CommentSyntax) -> (Vec<CommentSpan>, Vec<CommentSpan>) {
    let bytes = content.as_bytes();
    let mut comments: Vec<CommentSpan> = Vec::new();
    let mut directives: Vec<CommentSpan> = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut line_start = 0usize;
    let mut in_block = false;
    let mut block_start = 0usize;
    let mut block_start_line = 0usize;
    let mut block_start_column = 0usize;
    let mut block_inline = false;
    let mut string_delim: Option<u8> = None;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\n' {
            string_delim = None;
            i += 1;
            line += 1;
            line_start = i;
            continue;
        }
        if in_block {
            if byte == b'*' && bytes.get(i + 1) == Some(&b'/') {
                let end = (i + 2).min(bytes.len());
                let (text, text_truncated) = clipped_text(content.get(block_start..end).unwrap_or(""));
                push_span(&mut comments, &mut directives, CommentSpan {
                    line: block_start_line,
                    column: block_start_column,
                    kind: "block",
                    text,
                    text_truncated,
                    inline: block_inline,
                });
                in_block = false;
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(delim) = string_delim {
            if byte == b'\\' {
                i += 1;
                if let Some(next) = content.get(i..).and_then(|s| s.chars().next()) { i += next.len_utf8(); }
                continue;
            }
            if byte == delim { string_delim = None; }
            i += 1;
            continue;
        }
        if byte == b'"' || byte == b'\'' || byte == b'`' {
            string_delim = Some(byte);
            i += 1;
            continue;
        }
        let column = i - line_start + 1;
        let starts_line_comment = match syntax {
            CommentSyntax::Slash => byte == b'/' && bytes.get(i + 1) == Some(&b'/'),
            CommentSyntax::Hash => byte == b'#' && hash_opens_comment(bytes, i, line_start),
        };
        if starts_line_comment {
            let eol = content.get(i..).and_then(|s| s.find('\n')).map(|d| i + d).unwrap_or(bytes.len());
            let (text, text_truncated) = clipped_text(content.get(i..eol).unwrap_or(""));
            push_span(&mut comments, &mut directives, CommentSpan {
                line,
                column,
                kind: "line",
                text,
                text_truncated,
                inline: line_has_code_before(content, line_start, i),
            });
            i = eol;
            continue;
        }
        if syntax == CommentSyntax::Slash && byte == b'/' && bytes.get(i + 1) == Some(&b'*') {
            in_block = true;
            block_start = i;
            block_start_line = line;
            block_start_column = column;
            block_inline = line_has_code_before(content, line_start, i);
            i += 2;
            continue;
        }
        i += 1;
    }
    if in_block {
        let (text, text_truncated) = clipped_text(content.get(block_start..).unwrap_or(""));
        push_span(&mut comments, &mut directives, CommentSpan {
            line: block_start_line,
            column: block_start_column,
            kind: "block",
            text,
            text_truncated,
            inline: block_inline,
        });
    }
    (comments, directives)
}

fn comment_span_json(path: &str, syntax: CommentSyntax, span: &CommentSpan) -> Value {
    let mut hit = serde_json::Map::new();
    hit.insert("path".to_string(), json!(path));
    hit.insert("line".to_string(), json!(span.line));
    hit.insert("column".to_string(), json!(span.column));
    hit.insert("kind".to_string(), json!(span.kind));
    hit.insert("syntax".to_string(), json!(syntax.label()));
    hit.insert("inline".to_string(), json!(span.inline));
    hit.insert("text".to_string(), json!(span.text.trim_end()));
    if span.text_truncated { hit.insert("text_truncated".to_string(), json!(true)); }
    Value::Object(hit)
}

pub fn scan_comments(req: &CommentScan, cfg: &crate::ragconfig::RagConfig) -> Value {
    let root = req.root.filter(|p| !p.is_empty()).unwrap_or(".");
    let scope = req.paths.first().copied().filter(|p| !p.is_empty());
    let glob_scopes: Vec<Option<&str>> = glob_scopes_for(req.paths);
    let origin = if req.root.filter(|p| !p.is_empty()).is_some() {
        crate::scan_universe::TargetOrigin::CallerNamed
    } else {
        crate::scan_universe::TargetOrigin::ProjectDefault
    };
    let glob = match req.path_glob.filter(|g| !g.is_empty()) {
        Some(g) => match crate::path_glob::PathGlob::parse(g) {
            Ok(parsed) => Some(parsed),
            Err(e) => return json!({ "ok": false, "error": e, "mode": "comments" }),
        },
        None => None,
    };
    let mut exclude_globs: Vec<crate::path_glob::PathGlob> = Vec::new();
    for pattern in &req.exclude_globs {
        match crate::path_glob::PathGlob::parse(pattern) {
            Ok(parsed) => exclude_globs.push(parsed),
            Err(e) => return json!({ "ok": false, "error": e, "mode": "comments" }),
        }
    }
    let file_cap = req.max_files.min(LITERAL_SCAN_MAX_FILES).max(1);
    let started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let universe = match crate::scan_universe::list_scan_universe(root, req.paths, file_cap.saturating_add(1), &cfg.index, origin, req.refresh) {
        Ok(u) => u,
        Err(e) => return json!({ "ok": false, "error": e, "mode": "comments" }),
    };
    let listed = universe.files;
    let files_truncated = listed.len() > file_cap;
    let files: &[String] = if files_truncated { &listed[..file_cap] } else { &listed[..] };
    let files_matching_glob = match &glob {
        Some(g) => files.iter().filter(|p| glob_scopes.iter().any(|s| g.admits(root, *s, p))).count(),
        None => files.len(),
    };
    let glob_matched_no_files = glob.is_some() && !files.is_empty() && files_matching_glob == 0;
    let mut cache = if req.refresh { ScanCache::fresh(root) } else { scan_cache_take(root) };
    let max_matches = req.max_matches.max(1);
    let mut comments: Vec<Value> = Vec::new();
    let mut directives: Vec<Value> = Vec::new();
    let mut files_scanned = 0usize;
    let mut files_with_comments = 0usize;
    let mut files_unreadable = 0usize;
    let mut files_skipped_binary = 0usize;
    let mut files_skipped_binary_extension = 0usize;
    let mut files_skipped_too_large = 0usize;
    let mut files_skipped_no_syntax: Vec<String> = Vec::new();
    let mut matches_truncated = false;
    for path in files {
        if comments.len() + directives.len() >= max_matches { matches_truncated = true; break; }
        if let Some(g) = &glob {
            if !glob_scopes.iter().any(|s| g.admits(root, *s, path)) { continue; }
        }
        if exclude_globs.iter().any(|g| glob_scopes.iter().any(|s| g.admits(root, *s, path))) { continue; }
        if has_binary_extension(path) { files_skipped_binary_extension += 1; continue; }
        let Some(syntax) = comment_syntax_for_path(path) else {
            if files_skipped_no_syntax.len() < SKIPPED_SAMPLE_LEN { files_skipped_no_syntax.push(path.clone()); }
            continue;
        };
        let stat = host_stat(path);
        let (mtime_ms, size) = match &stat {
            Some(stat) => (
                stat.get("mtime_ms").and_then(|v| v.as_u64())
                    .or_else(|| stat.get("mtimeMs").and_then(|v| v.as_u64()))
                    .unwrap_or(0),
                stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            ),
            None => (0, 0),
        };
        if stat.is_some() {
            if size > LITERAL_SCAN_MAX_FILE_BYTES { files_skipped_too_large += 1; continue; }
            if size == 0 { files_scanned += 1; continue; }
        }
        let content = if mtime_ms > 0 {
            match cache.get(path, mtime_ms, size) {
                Some(cached) => Some(cached),
                None => {
                    let fresh = host_read(path);
                    if let Some(ref text) = fresh { cache.insert(path, mtime_ms, size, text); }
                    fresh
                }
            }
        } else {
            host_read(path)
        };
        let Some(content) = content else {
            if stat.is_some() { files_skipped_binary += 1 } else { files_unreadable += 1 }
            continue;
        };
        if content.as_bytes().contains(&0u8) { files_skipped_binary += 1; continue; }
        files_scanned += 1;
        let (file_comments, file_directives) = scan_content_for_comments(&content, syntax);
        if file_comments.is_empty() && file_directives.is_empty() { continue; }
        files_with_comments += 1;
        for span in &file_comments {
            if comments.len() + directives.len() >= max_matches { matches_truncated = true; break; }
            comments.push(comment_span_json(path, syntax, span));
        }
        for span in &file_directives {
            if comments.len() + directives.len() >= max_matches { matches_truncated = true; break; }
            directives.push(comment_span_json(path, syntax, span));
        }
    }
    let cache_hits = cache.hits;
    let cache_misses = cache.misses;
    let cache_entries = cache.entries.len();
    let cache_bytes = cache.bytes;
    scan_cache_put(cache);
    let exhaustive = !files_truncated && !matches_truncated && files_unreadable == 0
        && files_skipped_too_large == 0 && universe.listing_complete && !glob_matched_no_files;
    let mut out = serde_json::Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("mode".to_string(), json!("comments"));
    out.insert("root".to_string(), json!(root));
    if req.paths.len() > 1 { out.insert("paths".to_string(), json!(req.paths)); }
    else if let Some(p) = scope { out.insert("path".to_string(), json!(p)); }
    if let Some(g) = req.path_glob { out.insert("path_glob".to_string(), json!(g)); }
    if !req.exclude_globs.is_empty() { out.insert("exclude_glob".to_string(), json!(req.exclude_globs.join(", "))); }
    if glob.is_some() { out.insert("files_matching_glob".to_string(), json!(files_matching_glob)); }
    if glob_matched_no_files { out.insert("glob_matched_no_files".to_string(), json!(true)); }
    out.insert("file_source".to_string(), json!(universe.source.label()));
    out.insert("file_source_detail".to_string(), json!(universe.source.detail()));
    if req.refresh {
        out.insert("refreshed".to_string(), json!(true));
        out.insert("refreshed_note".to_string(), json!(
            "this scan re-read from disk: git ls-files was skipped for a directory walk and the mtime-keyed content cache was bypassed, so edits and untracked files are visible"
        ));
    }
    if !universe.listing_complete {
        out.insert("listing_incomplete".to_string(), json!(true));
        if let Some(reason) = &universe.walk_reason { out.insert("walk_reason".to_string(), json!(reason)); }
    }
    out.insert("comment_count".to_string(), json!(comments.len()));
    out.insert("directive_count".to_string(), json!(directives.len()));
    out.insert("files_with_comments".to_string(), json!(files_with_comments));
    out.insert("files_scanned".to_string(), json!(files_scanned));
    out.insert("files_listed".to_string(), json!(files.len()));
    let comment_paths: Vec<Value> = {
        let mut seen: Vec<String> = Vec::new();
        for hit in comments.iter().chain(directives.iter()) {
            let p = hit.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !seen.contains(&p) { seen.push(p); }
        }
        seen.into_iter().map(|p| json!(p)).collect()
    };
    out.insert("files".to_string(), Value::Array(comment_paths));
    out.insert("output".to_string(), Value::Array(
        comments.iter().chain(directives.iter())
            .map(|hit| json!(format!(
                "{}:{}:{}: {}",
                hit.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                hit.get("line").and_then(|v| v.as_u64()).unwrap_or(0),
                hit.get("column").and_then(|v| v.as_u64()).unwrap_or(0),
                hit.get("text").and_then(|v| v.as_str()).unwrap_or(""),
            )))
            .collect(),
    ));
    if cache_hits + cache_misses > 0 {
        out.insert("scan_cache".to_string(), json!({
            "hits": cache_hits,
            "misses": cache_misses,
            "entries": cache_entries,
            "cached_bytes": cache_bytes,
        }));
    }
    if files_skipped_binary_extension > 0 { out.insert("files_skipped_binary_extension".to_string(), json!(files_skipped_binary_extension)); }
    if files_skipped_binary > 0 { out.insert("files_skipped_binary".to_string(), json!(files_skipped_binary)); }
    if files_skipped_too_large > 0 {
        out.insert("files_skipped_too_large_count".to_string(), json!(files_skipped_too_large));
        out.insert("max_file_bytes".to_string(), json!(LITERAL_SCAN_MAX_FILE_BYTES));
    }
    if files_unreadable > 0 { out.insert("files_unreadable".to_string(), json!(files_unreadable)); }
    if !files_skipped_no_syntax.is_empty() {
        out.insert("files_skipped_no_syntax".to_string(), json!(files_skipped_no_syntax));
        out.insert("files_skipped_no_syntax_note".to_string(), json!(
            "no comment syntax is mapped for these extensions; narrow the scan with \"glob\" or \"path\" if a language here is missing"
        ));
    }
    let elapsed_ms = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started_ms);
    out.insert("elapsed_ms".to_string(), json!(elapsed_ms));
    out.insert("max_matches".to_string(), json!(max_matches));
    out.insert("exhaustive".to_string(), json!(exhaustive));
    if files_truncated {
        out.insert("files_truncated".to_string(), json!(true));
        out.insert("files_truncated_at".to_string(), json!(file_cap));
    }
    if matches_truncated {
        out.insert("matches_truncated".to_string(), json!(true));
        out.insert("matches_truncated_at".to_string(), json!(max_matches));
    }
    out.insert("comments".to_string(), Value::Array(comments));
    out.insert("directives".to_string(), Value::Array(directives));
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
