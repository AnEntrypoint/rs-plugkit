use serde_json::{json, Value};

use crate::ragconfig::IndexConfig;

const CODE_EXTS: &[&str] = &["js", "mjs", "cjs"];
const SIZE_RATIO_THRESHOLD: u64 = 300;
const MAX_SCAN_BYTES: u64 = 500 * 1024;
const MAX_NODE_MODULES_FILES: usize = 20_000;

const NOISE_SKIP_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "specs",
    "__mocks__",
    "docs",
    "doc",
    "examples",
    "example",
    "demo",
    "demos",
    "fixtures",
    "__fixtures__",
    "coverage",
    ".nyc_output",
    "benchmark",
    "benchmarks",
];
const NOISE_SKIP_SUFFIXES: &[&str] = &[
    ".map",
    ".d.ts",
    ".md",
    ".markdown",
    ".txt",
    ".min.css",
    ".css",
];

fn has_code_ext(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    CODE_EXTS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

fn find_suspicious_escapes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();
    let mut i = 0usize;
    while i + 5 < bytes.len() {
        if bytes[i] != b'\\' || bytes[i + 1] != b'u' {
            i += 1;
            continue;
        }
        let mut decoded = String::new();
        let mut j = i;
        let mut count = 0usize;
        loop {
            if j + 6 > bytes.len() {
                break;
            }
            if bytes[j] != b'\\' || bytes[j + 1] != b'u' {
                break;
            }
            let hex = match std::str::from_utf8(&bytes[j + 2..j + 6]) {
                Ok(s) => s,
                Err(_) => break,
            };
            let code = match u32::from_str_radix(hex, 16) {
                Ok(c) => c,
                Err(_) => break,
            };
            let ch = match char::from_u32(code) {
                Some(c) => c,
                None => break,
            };
            decoded.push(ch);
            count += 1;
            j += 6;
        }
        if count >= 4 && is_identifier_shaped(&decoded) {
            hits.push(decoded);
        }
        i = if count > 0 { j } else { i + 1 };
    }
    hits
}

fn is_identifier_shaped(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let rest_ok = chars.clone().all(|c| c.is_ascii_alphanumeric() || c == '_');
    rest_ok && chars.count() + 1 >= 3
}

const HEX_IDENT_DENSITY_THRESHOLD: usize = 10;

fn count_hex_obfuscator_idents(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 3] != b"_0x" {
            i += 1;
            continue;
        }
        let mut j = i + 3;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        let hex_len = j - (i + 3);
        if (4..=6).contains(&hex_len) {
            count += 1;
        }
        i = j;
    }
    count
}

#[derive(Clone)]
struct FileFinding {
    path: String,
    severity: &'static str,
    ratio: Option<u64>,
    escape_hits: Vec<String>,
    note: Option<String>,
}

struct BlockedRead {
    path: String,
    reason: String,
}

fn scan_one_file(path: &str) -> Option<Result<FileFinding, BlockedRead>> {
    let stat = crate::wasm_dispatch::host_stat(path);
    let size = match &stat {
        Some(v) if !v.is_null() => v.get("size").and_then(|s| s.as_u64()),
        _ => {
            return Some(Err(BlockedRead {
                path: path.to_string(),
                reason: "stat failed or file missing".into(),
            }))
        }
    };
    let Some(size) = size else {
        return Some(Err(BlockedRead {
            path: path.to_string(),
            reason: "stat returned no size".into(),
        }));
    };
    if size > MAX_SCAN_BYTES {
        return Some(Ok(FileFinding {
            path: path.to_string(),
            severity: "warn",
            ratio: None,
            escape_hits: Vec::new(),
            note: Some(format!("skipped full scan, file too large ({size} bytes)")),
        }));
    }
    let text = match crate::wasm_dispatch::host_read(path) {
        Some(t) => t,
        None if size == 0 => return None,
        None => {
            return Some(Err(BlockedRead {
                path: path.to_string(),
                reason: "read failed after successful stat".into(),
            }))
        }
    };
    let lines = text.lines().count().max(1) as u64;
    let bytes = text.len() as u64;
    let ratio = bytes / lines;
    let oversized = ratio > SIZE_RATIO_THRESHOLD;
    let escape_hits = find_suspicious_escapes(&text);
    let hex_ident_count = count_hex_obfuscator_idents(&text);
    let hex_obfuscated = hex_ident_count >= HEX_IDENT_DENSITY_THRESHOLD;
    if !oversized && escape_hits.is_empty() && !hex_obfuscated {
        return None;
    }
    let severity = if !escape_hits.is_empty() || hex_obfuscated {
        "fail"
    } else {
        "warn"
    };
    let note = if hex_obfuscated && escape_hits.is_empty() {
        Some(format!(
            "{hex_ident_count} _0x-hex obfuscator-style identifiers"
        ))
    } else {
        None
    };
    Some(Ok(FileFinding {
        path: path.to_string(),
        severity,
        ratio: Some(ratio),
        escape_hits: escape_hits.into_iter().take(5).collect(),
        note,
    }))
}

fn scan_file_list(
    paths: &[String],
    budget: usize,
    findings: &mut Vec<FileFinding>,
    blocked: &mut Vec<BlockedRead>,
) -> usize {
    let mut scanned = 0usize;
    for p in paths.iter().take(budget) {
        if !has_code_ext(p) {
            continue;
        }
        scanned += 1;
        match scan_one_file(p) {
            Some(Ok(f)) => findings.push(f),
            Some(Err(b)) => blocked.push(b),
            None => {}
        }
    }
    scanned
}

const STAMP_PATH: &str = ".gm/exec-spool/.scan-deps-stamp.json";
const LEGACY_STAMP_PATH: &str = ".gm/scan-deps-stamp.json";

fn parse_stamp(text: &str) -> std::collections::HashMap<String, (f64, u64)> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Default::default();
    };
    let Some(obj) = v.get("packages").and_then(|p| p.as_object()) else {
        return Default::default();
    };
    obj.iter()
        .filter_map(|(k, entry)| {
            let mtime = entry.get(0).and_then(|x| x.as_f64())?;
            let size = entry.get(1).and_then(|x| x.as_u64())?;
            Some((k.clone(), (mtime, size)))
        })
        .collect()
}

fn load_stamp() -> std::collections::HashMap<String, (f64, u64)> {
    crate::wasm_dispatch::host_read(STAMP_PATH)
        .or_else(|| crate::wasm_dispatch::host_read(LEGACY_STAMP_PATH))
        .as_deref()
        .map(parse_stamp)
        .unwrap_or_default()
}

fn save_stamp(packages: &std::collections::HashMap<String, (f64, u64)>) {
    let obj: serde_json::Map<String, Value> = packages
        .iter()
        .map(|(k, (mtime, size))| (k.clone(), json!([mtime, size])))
        .collect();
    let doc = json!({ "tool": "scan_deps", "version": 1, "packages": obj }).to_string();
    if crate::wasm_dispatch::host_read(STAMP_PATH).as_deref() == Some(doc.as_str()) {
        return;
    }
    let _ = crate::wasm_dispatch::host_write(STAMP_PATH, &doc);
}

fn is_noise_dir_segment(seg: &str) -> bool {
    NOISE_SKIP_DIRS.iter().any(|d| seg.eq_ignore_ascii_case(d))
}

fn is_noise_suffix(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    NOISE_SKIP_SUFFIXES.iter().any(|suf| lower.ends_with(suf))
}

struct PackageWalkResult {
    max_mtime: f64,
    total_size: u64,
    file_count: usize,
    candidates: Vec<String>,
    escapes: Vec<String>,
}

struct Containment {
    allowed_canonical_roots: Vec<String>,
}

fn comparable_canonical_path(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let without_verbatim_prefix = unified.strip_prefix("//?/").unwrap_or(&unified);
    let trimmed = without_verbatim_prefix.trim_end_matches('/');
    let has_drive_letter = trimmed.as_bytes().get(1) == Some(&b':');
    if has_drive_letter {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

fn canonical_path_of(stat: &Value) -> Option<String> {
    stat.get("canonicalPath")
        .and_then(|v| v.as_str())
        .map(comparable_canonical_path)
}

impl Containment {
    fn around(directories: &[&str]) -> Self {
        let allowed_canonical_roots = directories
            .iter()
            .filter_map(|dir| crate::wasm_dispatch::host_stat(dir))
            .filter_map(|stat| canonical_path_of(&stat))
            .collect();
        Containment {
            allowed_canonical_roots,
        }
    }

    fn admits(&self, stat: &Value) -> bool {
        if self.allowed_canonical_roots.is_empty() {
            return true;
        }
        let Some(canonical) = canonical_path_of(stat) else {
            return true;
        };
        self.allowed_canonical_roots.iter().any(|root| {
            canonical == *root
                || canonical
                    .strip_prefix(root.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

fn dependency_scan_base(root: &str) -> &str {
    if is_absolute_root(root) {
        root
    } else {
        "."
    }
}

fn node_modules_directory(base: &str) -> String {
    if base == "." {
        "node_modules".to_string()
    } else {
        crate::scan_universe::join_under(base, "node_modules")
    }
}

fn normalized_root(raw: &str) -> String {
    let unified = raw.replace('\\', "/");
    let trimmed = unified.trim_end_matches('/');
    if trimmed.is_empty() {
        ".".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_absolute_root(root: &str) -> bool {
    root.starts_with('/') || root.as_bytes().get(1) == Some(&b':')
}

struct NodeModulesScan {
    findings: Vec<FileFinding>,
    blocked: Vec<BlockedRead>,
    scanned: usize,
    present: bool,
    truncated: bool,
    escapes: Vec<String>,
}

fn package_directory_identity(dir: &str) -> Option<String> {
    let (_, tail) = dir.rsplit_once("node_modules/")?;
    let segments = tail.split('/').collect::<Vec<_>>();
    let is_package_root = match segments.as_slice() {
        [_] => true,
        [scope, _] => scope.starts_with('@'),
        _ => false,
    };
    if !is_package_root {
        return None;
    }
    let text = crate::wasm_dispatch::host_read(&format!("{dir}/package.json"))?;
    let manifest = serde_json::from_str::<Value>(&text).ok()?;
    let name = manifest.get("name")?.as_str()?;
    let version = manifest.get("version")?.as_str()?;
    Some(format!("{name}@{version}"))
}

fn walk_package(
    dir: &str,
    budget: usize,
    containment: &Containment,
    visited_directories: &mut std::collections::HashSet<String>,
    r: &mut PackageWalkResult,
) {
    if r.file_count >= budget {
        return;
    }
    let Some(stat) = crate::wasm_dispatch::host_stat(dir) else {
        return;
    };
    if !stat
        .get("isDirectory")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return;
    }
    if !containment.admits(&stat) {
        r.escapes.push(dir.to_string());
        return;
    }
    let directory_identity = stat
        .get("canonicalPath")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| package_directory_identity(dir).map(|identity| format!("package:{identity}")))
        .unwrap_or_else(|| format!("path:{dir}"));
    if !visited_directories.insert(directory_identity) {
        return;
    }
    for entry in crate::code_index::list_dir(dir) {
        if r.file_count >= budget {
            return;
        }
        if entry.starts_with('.') {
            continue;
        }
        let next = format!("{dir}/{entry}");
        let Some(stat) = crate::wasm_dispatch::host_stat(&next) else {
            continue;
        };
        if stat.is_null() {
            continue;
        }
        if !containment.admits(&stat) {
            r.escapes.push(next);
            continue;
        }
        let is_dir = stat
            .get("isDirectory")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if is_dir {
            if entry == "node_modules" || is_noise_dir_segment(&entry) {
                continue;
            }
            walk_package(&next, budget, containment, visited_directories, r);
        } else {
            r.file_count += 1;
            if let Some(m) = stat.get("mtime_ms").and_then(|v| v.as_f64()) {
                if m > r.max_mtime {
                    r.max_mtime = m;
                }
            }
            r.total_size += stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            if !is_noise_suffix(&next) {
                r.candidates.push(next);
            }
        }
    }
}

fn scan_node_modules(root: &str, max_files: usize) -> NodeModulesScan {
    let mut scan = NodeModulesScan {
        findings: Vec::new(),
        blocked: Vec::new(),
        scanned: 0,
        present: false,
        truncated: false,
        escapes: Vec::new(),
    };
    let base = dependency_scan_base(root);
    let store = node_modules_directory(base);
    scan.present = crate::wasm_dispatch::host_exists(&store);
    if !scan.present {
        return scan;
    }
    let containment = Containment::around(&[base, &store]);

    let mut prior_stamp = load_stamp();
    let mut new_stamp = std::collections::HashMap::new();
    let mut visited_directories = std::collections::HashSet::new();

    let mut package_dirs: Vec<String> = Vec::new();
    for entry in crate::code_index::list_dir(&store) {
        if entry.starts_with('.') {
            continue;
        }
        let path = format!("{store}/{entry}");
        if entry.starts_with('@') {
            for scoped in crate::code_index::list_dir(&path) {
                package_dirs.push(format!("{path}/{scoped}"));
            }
        } else {
            package_dirs.push(path);
        }
    }

    for pkg_dir in package_dirs {
        if scan.scanned >= max_files {
            scan.truncated = true;
            break;
        }
        let mut r = PackageWalkResult {
            max_mtime: 0.0,
            total_size: 0,
            file_count: 0,
            candidates: Vec::new(),
            escapes: Vec::new(),
        };
        walk_package(
            &pkg_dir,
            max_files.saturating_sub(scan.scanned),
            &containment,
            &mut visited_directories,
            &mut r,
        );
        scan.escapes.append(&mut r.escapes);
        if r.file_count == 0 {
            continue;
        }
        let sig = (r.max_mtime, r.total_size);
        if prior_stamp.get(&pkg_dir) == Some(&sig) {
            new_stamp.insert(pkg_dir.clone(), sig);
            continue;
        }
        let failures_before = scan
            .findings
            .iter()
            .filter(|f| f.severity == "fail")
            .count();
        let blocked_before = scan.blocked.len();
        scan.scanned += scan_file_list(
            &r.candidates,
            r.candidates.len(),
            &mut scan.findings,
            &mut scan.blocked,
        );
        let failures_after = scan
            .findings
            .iter()
            .filter(|f| f.severity == "fail")
            .count();
        if failures_after == failures_before && scan.blocked.len() == blocked_before {
            new_stamp.insert(pkg_dir.clone(), sig);
        }
    }

    for (k, v) in prior_stamp.drain() {
        new_stamp.entry(k).or_insert(v);
    }
    save_stamp(&new_stamp);

    scan
}

const SYMLINK_ESCAPES_LISTED: usize = 20;

pub fn scan_deps(body: &Value) -> Value {
    let requested_root = body
        .get("root")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
        .filter(|p| !p.is_empty())
        .unwrap_or(".");
    let root = normalized_root(requested_root);
    let root = root.as_str();
    let force_full = body.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut cfg = IndexConfig::default();
    cfg.digest_max_files = 50_000;

    let mut findings: Vec<FileFinding> = Vec::new();
    let mut blocked: Vec<BlockedRead> = Vec::new();

    let (tracked, tracked_complete) = if is_absolute_root(root) {
        (crate::scan_universe::project_source_files(root, cfg.digest_max_files, &cfg), true)
    } else {
        let enumeration = crate::code_index::collect_files_within_wall_budget(root, cfg.digest_max_files, &cfg);
        (enumeration.files, enumeration.complete)
    };
    if !tracked_complete {
        blocked.push(BlockedRead {
            path: root.to_string(),
            reason: "wall budget expired while listing files; the scan is incomplete".into(),
        });
    }
    let tracked_scanned = scan_file_list(&tracked, tracked.len(), &mut findings, &mut blocked);

    if force_full {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(STAMP_PATH);
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(LEGACY_STAMP_PATH);
    }
    let node_modules = scan_node_modules(root, MAX_NODE_MODULES_FILES);
    findings.extend(node_modules.findings);
    blocked.extend(node_modules.blocked);

    let failing: Vec<&FileFinding> = findings.iter().filter(|f| f.severity == "fail").collect();
    let warnings: Vec<&FileFinding> = findings.iter().filter(|f| f.severity == "warn").collect();

    let files_scanned = tracked_scanned + node_modules.scanned;
    let ok = failing.is_empty() && blocked.is_empty();

    json!({
        "tool": "scan_deps",
        "version": 1,
        "root": root,
        "filesScanned": files_scanned,
        "nodeModulesPresent": node_modules.present,
        "nodeModulesTruncated": node_modules.truncated,
        "symlinkEscapeCount": node_modules.escapes.len(),
        "symlinkEscapes": node_modules.escapes.iter().take(SYMLINK_ESCAPES_LISTED).collect::<Vec<_>>(),
        "ok": ok,
        "failCount": failing.len(),
        "warnCount": warnings.len(),
        "blockedCount": blocked.len(),
        "failing": failing.iter().map(|f| json!({
            "path": f.path, "ratio": f.ratio, "escapeHits": f.escape_hits, "note": f.note,
        })).collect::<Vec<_>>(),
        "warnings": warnings.iter().map(|f| json!({
            "path": f.path, "ratio": f.ratio, "note": f.note,
        })).collect::<Vec<_>>(),
        "blocked": blocked.iter().map(|b| json!({
            "path": b.path, "reason": b.reason,
        })).collect::<Vec<_>>(),
    })
}
