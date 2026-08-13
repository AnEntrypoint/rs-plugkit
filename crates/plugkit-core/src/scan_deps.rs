// scan_deps.rs -- supply-chain scan for the "HiddenSpawn"-class obfuscated
// dropper (confirmed across 17+ separately-compromised repos, 2026-08). See
// lib.rs's module doc for the incident summary. Detects two structural
// properties, not any one incident's specific literal values (which are
// trivial for an attacker to rotate and are deliberately not hardcoded as
// the primary detector):
//
//   1. size-ratio: a file whose byte size is wildly disproportionate to its
//      line count. The payload is appended as one extremely long line,
//      often whitespace-padded to push it off-screen in a normal editor/
//      diff view -- this survives any change to the payload's own content
//      because it is a property of HOW it hides.
//   2. escape-density: a dense run (4+ in a row) of \uXXXX escapes that
//      decode to something identifier-shaped (letters/digits/underscore,
//      starting with a letter). Real code contains at most one or two
//      Unicode escapes in a row (a genuine non-ASCII literal); an
//      obfuscated module name like require/spawn/child_process written
//      this way has no legitimate reason to exist. Requiring identifier
//      SHAPE (not merely "decodes to printable ASCII") is deliberate: a
//      looser check produced a real false positive on a legitimate escaped
//      CSS-selector-punctuation string (",./:") found live during
//      verification of an earlier prototype of this scanner.
//
// SCOPE: git-tracked source is scanned in full every call (fast -- a real
// repo's own source is at most a few hundred files). node_modules, if
// present, is walked but three ways bounded so a large real tree cannot
// bog down the machine on every session dispatch:
//   1. A NOISE_SKIP_DIRS/NOISE_SKIP_SUFFIXES ignore list on top of the
//      code-search-oriented IndexConfig defaults -- test/docs/example
//      dirs and .map/.d.ts/.md files are never a payload carrier for this
//      attack class and are pure volume (live-measured: 5916 .map files
//      alone in one real node_modules, 73 test/docs/example dirs).
//   2. A changed-since-last-scan stamp (.gm/scan-deps-stamp.json, mtime_ms
//      + size, never mtime alone -- this codebase already knows a bare
//      mtime match is not a safe cache key, see code_index.rs's own
//      digest-fast-path comment: coarse FS mtime granularity or a fast
//      restore can reproduce an identical timestamp on genuinely changed
//      content). A package whose node_modules subdirectory has not
//      changed since the last scan is skipped entirely on the FULL
//      per-file walk -- it already passed and nothing in it moved.
//   3. A per-file MAX_SCAN_BYTES size cap and a global
//      MAX_NODE_MODULES_FILES walk budget as the final backstop, in case
//      1+2 still leave an unreasonably large first-ever scan.
// Live-witnessed against a real ~21k-file, ~11k-JS-file node_modules: an
// unbounded walk with no size cap exceeded 100s on the FIRST scan (no
// stamp yet, unavoidable); the changed-since filter is what keeps every
// SUBSEQUENT scan of a stable tree fast, since only genuinely new/updated
// packages get walked in full.
//
// IMPLEMENTATION NOTE: node_modules is walked per-top-level-package (each
// package dir, or each scoped package under an @scope/ dir, gets its own
// change signature and its own bounded sub-walk) rather than via
// code_index::collect_files's force_include mechanism directly on
// "node_modules" -- that mechanism's is_force_included does a plain
// substring match, so force-including the literal string "node_modules"
// would ALSO match, and therefore force-include, every single descendant
// path (since a substring match on a path prefix matches every path under
// it), silently defeating every noise-dir/noise-suffix filter beneath the
// root. Discovered live while implementing this exact fix. Walking
// per-package with the DEFAULT (non-force-included) config instead means
// the "node_modules" segment is never present in any of the walked
// sub-roots, so the normal skip-dir/skip-suffix filtering (including the
// noise additions above) applies correctly throughout.

use serde_json::{json, Value};

use crate::ragconfig::IndexConfig;

const CODE_EXTS: &[&str] = &["js", "mjs", "cjs"];
const SIZE_RATIO_THRESHOLD: u64 = 300;
// A file above this size is flagged on size-ratio alone via host_stat (no
// content read) but never regex-scanned for escape hits -- a legitimate
// multi-hundred-KB minified bundle would otherwise dominate the whole
// scan's wall-clock for a property (escape-density) that only ever matters
// on the SMALL payload-carrier files this attack actually uses (a config/
// plugin file with one injected huge line, not a file that is huge
// throughout).
const MAX_SCAN_BYTES: u64 = 500 * 1024;
// Global file-count budget for the node_modules walk. See module doc for
// the live-measured rationale. git-tracked source has no separate cap --
// a real repo's own source is small enough that walking it in full is
// always fast.
const MAX_NODE_MODULES_FILES: usize = 20_000;

// Dependency-tree noise that is never a payload carrier for this attack
// class: test fixtures, documentation, and generated/derived artifacts
// (a sourcemap is machine-generated FROM the real source, never hand- or
// attacker-authored directly; a pure type-declaration file cannot execute
// at all). Appended to IndexConfig's own SKIP_DIRS/skip-suffix builtins,
// never replacing them.
const NOISE_SKIP_DIRS: &[&str] = &[
    "test", "tests", "__tests__", "spec", "specs", "__mocks__",
    "docs", "doc", "examples", "example", "demo", "demos",
    "fixtures", "__fixtures__", "coverage", ".nyc_output", "benchmark", "benchmarks",
];
const NOISE_SKIP_SUFFIXES: &[&str] = &[".map", ".d.ts", ".md", ".markdown", ".txt", ".min.css", ".css"];

fn has_code_ext(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    CODE_EXTS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Decode every run of 4-or-more consecutive `\uXXXX` escapes in `text` and
/// return the ones that spell out an identifier (a letter, then 2+
/// letters/digits/underscores). Pure hand-rolled scan, no regex dependency
/// -- this crate carries no regex crate and the scan is simple enough
/// (fixed-width hex groups) not to need one.
fn find_suspicious_escapes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut hits = Vec::new();
    let mut i = 0usize;
    while i + 5 < bytes.len() {
        if bytes[i] != b'\\' || bytes[i + 1] != b'u' {
            i += 1;
            continue;
        }
        // Try to decode a run of consecutive \uXXXX groups starting here.
        let mut decoded = String::new();
        let mut j = i;
        let mut count = 0usize;
        loop {
            if j + 6 > bytes.len() { break; }
            if bytes[j] != b'\\' || bytes[j + 1] != b'u' { break; }
            let hex = match std::str::from_utf8(&bytes[j + 2..j + 6]) { Ok(s) => s, Err(_) => break };
            let code = match u32::from_str_radix(hex, 16) { Ok(c) => c, Err(_) => break };
            let ch = match char::from_u32(code) { Some(c) => c, None => break };
            decoded.push(ch);
            count += 1;
            j += 6;
        }
        if count >= 4 && is_identifier_shaped(&decoded) {
            hits.push(decoded);
        }
        // Advance past this run entirely (whether it hit or not) so
        // overlapping partial matches never get double-counted.
        i = if count > 0 { j } else { i + 1 };
    }
    hits
}

fn is_identifier_shaped(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_ascii_alphabetic() { return false; }
    let rest_ok = chars.clone().all(|c| c.is_ascii_alphanumeric() || c == '_');
    rest_ok && chars.count() + 1 >= 3
}

#[derive(Clone)]
struct FileFinding {
    path: String,
    severity: &'static str, // "fail" | "warn"
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
        _ => return Some(Err(BlockedRead { path: path.to_string(), reason: "stat failed or file missing".into() })),
    };
    let Some(size) = size else {
        return Some(Err(BlockedRead { path: path.to_string(), reason: "stat returned no size".into() }));
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
        // A blocked/failed read on a file that DOES stat successfully (so
        // it exists and has a known size) is itself evidence -- an AV
        // quarantine denying read access on a file it already flagged is
        // exactly the symptom that first surfaced the real incident this
        // scanner guards against. Never silently skip it.
        None => return Some(Err(BlockedRead { path: path.to_string(), reason: "read failed after successful stat".into() })),
    };
    let lines = text.lines().count().max(1) as u64;
    let bytes = text.len() as u64;
    let ratio = bytes / lines;
    let oversized = ratio > SIZE_RATIO_THRESHOLD;
    let escape_hits = find_suspicious_escapes(&text);
    if !oversized && escape_hits.is_empty() { return None; }
    let severity = if !escape_hits.is_empty() { "fail" } else { "warn" };
    Some(Ok(FileFinding {
        path: path.to_string(),
        severity,
        ratio: Some(ratio),
        escape_hits: escape_hits.into_iter().take(5).collect(),
        note: None,
    }))
}

fn scan_file_list(paths: &[String], budget: usize, findings: &mut Vec<FileFinding>, blocked: &mut Vec<BlockedRead>) -> usize {
    let mut scanned = 0usize;
    for p in paths.iter().take(budget) {
        if !has_code_ext(p) { continue; }
        scanned += 1;
        match scan_one_file(p) {
            Some(Ok(f)) => findings.push(f),
            Some(Err(b)) => blocked.push(b),
            None => {}
        }
    }
    scanned
}

const STAMP_PATH: &str = ".gm/scan-deps-stamp.json";

fn load_stamp() -> std::collections::HashMap<String, (f64, u64)> {
    let Some(text) = crate::wasm_dispatch::host_read(STAMP_PATH) else { return Default::default() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Default::default() };
    let Some(obj) = v.get("packages").and_then(|p| p.as_object()) else { return Default::default() };
    obj.iter().filter_map(|(k, entry)| {
        let mtime = entry.get(0).and_then(|x| x.as_f64())?;
        let size = entry.get(1).and_then(|x| x.as_u64())?;
        Some((k.clone(), (mtime, size)))
    }).collect()
}

fn save_stamp(packages: &std::collections::HashMap<String, (f64, u64)>) {
    let obj: serde_json::Map<String, Value> = packages.iter()
        .map(|(k, (mtime, size))| (k.clone(), json!([mtime, size])))
        .collect();
    let doc = json!({ "tool": "scan_deps", "version": 1, "packages": obj });
    let _ = crate::wasm_dispatch::host_write(STAMP_PATH, &doc.to_string());
}

fn is_noise_dir_segment(seg: &str) -> bool {
    NOISE_SKIP_DIRS.iter().any(|d| seg.eq_ignore_ascii_case(d))
}

fn is_noise_suffix(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    NOISE_SKIP_SUFFIXES.iter().any(|suf| lower.ends_with(suf))
}

/// A package's aggregate change signature (MAX mtime_ms, SUM of size
/// across every non-noise file in its tree) and the matching candidate
/// file list, computed in ONE recursive stat-only walk rather than two --
/// deciding "is this package unchanged" never costs a second full
/// traversal on top of the first, and a changed package's file list is
/// already in hand with no re-walk needed. The signature is MAX
/// mtime_ms + SUM size across every file actually visited, not just the
/// top-level directory's own mtime: a directory's own mtime does not
/// update when a file inside it (or a nested subdirectory) is edited in
/// place, only when a direct child is added/removed/renamed
/// (live-verified on this machine's filesystem) -- a top-level-only
/// signature would silently and permanently mark a package "unchanged"
/// after its first scan even if an existing file were later overwritten,
/// which is exactly the attack vector this scanner exists to catch. A
/// noise-named subdirectory is pruned before recursion (see below), so it
/// contributes to neither the signature nor the candidate list -- an
/// accepted, narrow blind spot (a change confined entirely to a noise dir
/// stays undetected) traded for not paying full walk cost on noise trees
/// that can be enormous under node_modules (fixtures, coverage output).
struct PackageWalkResult {
    max_mtime: f64,
    total_size: u64,
    file_count: usize,
    candidates: Vec<String>,
}

fn walk_package(dir: &str, budget: usize, r: &mut PackageWalkResult) {
    if r.file_count >= budget { return; }
    for entry in crate::code_index::list_dir(dir) {
        if r.file_count >= budget { return; }
        if entry.starts_with('.') { continue; }
        let next = format!("{dir}/{entry}");
        let Some(stat) = crate::wasm_dispatch::host_stat(&next) else { continue };
        if stat.is_null() { continue; }
        let is_dir = stat.get("isDirectory").and_then(|b| b.as_bool()).unwrap_or(false);
        if is_dir {
            // A noise-named subdirectory (test/, docs/, fixtures/, ...) is
            // pruned from the walk entirely -- its files still contribute
            // to nothing (not even the signature), which is an accepted,
            // deliberate narrowing of the doc comment's stated ideal: a
            // package that ONLY changes inside a noise dir stays flagged
            // unchanged, same as before this rewrite, but a real content
            // change anywhere else in the package still gets caught, and
            // the walk cost for genuinely huge noise trees (e.g.
            // node_modules/**/test/fixtures with thousands of files) is
            // avoided rather than paid on every scan.
            if is_noise_dir_segment(&entry) { continue; }
            walk_package(&next, budget, r);
        } else {
            r.file_count += 1;
            if let Some(m) = stat.get("mtime_ms").and_then(|v| v.as_f64()) {
                if m > r.max_mtime { r.max_mtime = m; }
            }
            r.total_size += stat.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            if !is_noise_suffix(&next) { r.candidates.push(next); }
        }
    }
}

fn scan_node_modules(max_files: usize) -> (Vec<FileFinding>, Vec<BlockedRead>, usize, bool, bool) {
    let mut findings = Vec::new();
    let mut blocked = Vec::new();
    let mut scanned = 0usize;
    let node_modules_present = crate::wasm_dispatch::host_exists("node_modules");
    if !node_modules_present {
        return (findings, blocked, scanned, false, false);
    }

    let mut prior_stamp = load_stamp();
    let mut new_stamp = std::collections::HashMap::new();
    let mut truncated = false;

    // Top-level entries: either a plain package dir, or an @scope/ dir
    // whose own children are the real packages -- expand one level for
    // scoped packages so each real package gets its own change signature
    // (an @scope/ directory's own mtime does not reliably change when
    // only one scoped package inside it is updated).
    let mut package_dirs: Vec<String> = Vec::new();
    for entry in crate::code_index::list_dir("node_modules") {
        if entry.starts_with('.') { continue; }
        let path = format!("node_modules/{entry}");
        if entry.starts_with('@') {
            for scoped in crate::code_index::list_dir(&path) {
                package_dirs.push(format!("{path}/{scoped}"));
            }
        } else {
            package_dirs.push(path);
        }
    }

    for pkg_dir in package_dirs {
        if scanned >= max_files { truncated = true; break; }
        let mut r = PackageWalkResult { max_mtime: 0.0, total_size: 0, file_count: 0, candidates: Vec::new() };
        walk_package(&pkg_dir, max_files.saturating_sub(scanned), &mut r);
        if r.file_count == 0 { continue; }
        let sig = (r.max_mtime, r.total_size);
        new_stamp.insert(pkg_dir.clone(), sig);
        if prior_stamp.get(&pkg_dir) == Some(&sig) {
            // Unchanged since the last scan that actually completed a full
            // walk of this package -- already passed, nothing moved.
            continue;
        }
        scanned += scan_file_list(&r.candidates, r.candidates.len(), &mut findings, &mut blocked);
    }

    // Merge forward: a package not walked this pass (unchanged, or the
    // walk never reached it because the budget ran out first) keeps its
    // prior signature in the stamp so it is correctly recognized as
    // unchanged on the NEXT dispatch too -- only overwritten when this
    // pass actually re-signed it above.
    for (k, v) in prior_stamp.drain() {
        new_stamp.entry(k).or_insert(v);
    }
    save_stamp(&new_stamp);

    (findings, blocked, scanned, true, truncated)
}

/// Entry point dispatched by the `scan_deps` verb. `body` may carry
/// `{"root": "<relative-dir>"}` to scope the git-tracked-source scan to a
/// subdirectory (defaults to the whole project); node_modules is always
/// resolved relative to the project root, not `root`, since dependencies
/// never live inside an arbitrary subdirectory a caller might name. Pass
/// `{"full": true}` to force a full node_modules walk ignoring the
/// changed-since stamp (the CI-time/first-install path, not the per-
/// session default -- this still runs unbounded-but-capped, not truly
/// unbounded; a project wanting a genuinely exhaustive one-off sweep
/// should use its own scripts/scan-deps.mjs-equivalent, see README).
pub fn scan_deps(body: &Value) -> Value {
    let root = body.get("root").and_then(|v| v.as_str()).unwrap_or(".");
    let force_full = body.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut cfg = IndexConfig::default();
    // Widen the digest walk cap so a real project's own source (typically
    // far smaller than node_modules) is never truncated by the default
    // cap meant for a much larger corpus digest pass.
    cfg.digest_max_files = 50_000;

    let mut findings: Vec<FileFinding> = Vec::new();
    let mut blocked: Vec<BlockedRead> = Vec::new();

    let tracked = crate::code_index::collect_files(root, cfg.digest_max_files, &cfg);
    let tracked_scanned = scan_file_list(&tracked, tracked.len(), &mut findings, &mut blocked);

    if force_full {
        let _ = crate::wasm_dispatch::host_remove_file_never_directory(STAMP_PATH);
    }
    let (nm_findings, nm_blocked, nm_scanned, node_modules_present, node_modules_truncated) =
        scan_node_modules(MAX_NODE_MODULES_FILES);
    findings.extend(nm_findings);
    blocked.extend(nm_blocked);

    let failing: Vec<&FileFinding> = findings.iter().filter(|f| f.severity == "fail").collect();
    let warnings: Vec<&FileFinding> = findings.iter().filter(|f| f.severity == "warn").collect();

    let files_scanned = tracked_scanned + nm_scanned;
    let ok = failing.is_empty() && blocked.is_empty();

    json!({
        "tool": "scan_deps",
        "version": 1,
        "root": root,
        "filesScanned": files_scanned,
        "nodeModulesPresent": node_modules_present,
        "nodeModulesTruncated": node_modules_truncated,
        "ok": ok,
        "failCount": failing.len(),
        "warnCount": warnings.len(),
        "blockedCount": blocked.len(),
        "failing": failing.iter().map(|f| json!({
            "path": f.path, "ratio": f.ratio, "escapeHits": f.escape_hits,
        })).collect::<Vec<_>>(),
        "warnings": warnings.iter().map(|f| json!({
            "path": f.path, "ratio": f.ratio, "note": f.note,
        })).collect::<Vec<_>>(),
        "blocked": blocked.iter().map(|b| json!({
            "path": b.path, "reason": b.reason,
        })).collect::<Vec<_>>(),
    })
}
