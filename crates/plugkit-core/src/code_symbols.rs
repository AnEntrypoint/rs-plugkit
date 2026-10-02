#![cfg(target_arch = "wasm32")]

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Value};

use crate::code_index::{self, CallEdge};
use crate::libsql_wasm;
use crate::wasm_dispatch::{host_read, host_stat};

const FILES_TABLE: &str = "code_symbol_files";
const SYMBOLS_TABLE: &str = "code_symbols";
const DOC_KINDS_SQL: &str = "('section','document')";
const ROWS_PER_INSERT: usize = 120;
const SOURCE_SIZE_CAP_MULTIPLIER: usize = 4;
const SLOW_EXTRACT_LOG_MS: u64 = 500;

/// A minified bundle is one enormous line. Treesitter spends 28s on a 694KB dashboard asset and
/// yields machine-mangled names nobody searches for, which ate a whole pass's symbol budget before
/// any real source file was reached. Real source keeps its lines short.
const MINIFIED_MAX_LINE_CHARS: usize = 4_000;

/// Measured on litebox-main: one pass stored 320 JSON files and wrote 0 symbols for all of them,
/// spending the whole budget before the walk reached a source file. Data files carry no symbol.
const NO_SYMBOL_EXTS: [&str; 1] = [".json"];
const SIGNATURE_MAX_CHARS: usize = 140;
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 200;
const DEFAULT_IMPACT_DEPTH: usize = 3;
const CONTAINER_KINDS_SQL: &str = "('impl_item','trait_item','class_declaration','class_definition','struct_item','enum_item')";
const ACTIONS: &[&str] = &["overview", "status", "sync", "outline", "find", "callers", "callees", "impact", "hotspots", "orphans"];

fn host_now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

fn db_path(project_path: Option<&str>) -> String {
    code_index::project_db_path(project_path)
}

fn edges_namespace(project_path: Option<&str>) -> String {
    format!("{}-edges-by-file", code_index::code_ns_for(project_path))
}

fn legacy_edges_namespace() -> String {
    format!("{}-edges", code_index::code_ns_for(None))
}

fn number(v: Option<&Value>) -> u64 {
    v.and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)).or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0)
}

fn string(row: &Value, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn rows(db: &str, sql: &str, params: &[&str]) -> Vec<Value> {
    libsql_wasm::query_params(db, sql, params)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn single_count(db: &str, sql: &str) -> u64 {
    rows(db, sql, &[]).first().map(|r| number(r.get("c"))).unwrap_or(0)
}

fn ensure_schema(db: &str) -> Result<(), String> {
    libsql_wasm::open(db)?;
    libsql_wasm::exec(db, &format!(
        "CREATE TABLE IF NOT EXISTS {FILES_TABLE} (path TEXT PRIMARY KEY, lang TEXT, size INTEGER, mtime_ms INTEGER, loc INTEGER, symbols INTEGER, edges INTEGER, parse_failed INTEGER)"
    ))?;
    libsql_wasm::exec(db, &format!(
        "CREATE TABLE IF NOT EXISTS {SYMBOLS_TABLE} (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, signature TEXT)"
    ))?;
    libsql_wasm::exec(db, &format!("CREATE INDEX IF NOT EXISTS {SYMBOLS_TABLE}_name ON {SYMBOLS_TABLE}(name)"))?;
    libsql_wasm::exec(db, &format!("CREATE INDEX IF NOT EXISTS {SYMBOLS_TABLE}_path ON {SYMBOLS_TABLE}(path)"))?;
    Ok(())
}

fn signature_of(name: &str, body: &str) -> String {
    let first_line = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let signature = if first_line.is_empty() { name } else { first_line };
    signature.chars().take(SIGNATURE_MAX_CHARS).collect()
}

fn display_name(name: &str, signature: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    signature.trim_end_matches('{').trim().to_string()
}

struct FileSymbols {
    lang: String,
    size: u64,
    mtime_ms: u64,
    loc: usize,
    symbols: Vec<(String, String, usize, usize, String)>,
    edges: Vec<CallEdge>,
    parse_failed: bool,
}

fn store_file(db: &str, fp: &str, file: &FileSymbols, project_path: Option<&str>) -> Result<(), String> {
    libsql_wasm::exec_params(db, &format!("DELETE FROM {SYMBOLS_TABLE} WHERE path=?1"), &[fp])?;
    for batch in file.symbols.chunks(ROWS_PER_INSERT) {
        let mut sql = format!("INSERT INTO {SYMBOLS_TABLE}(path,kind,name,line_start,line_end,signature) VALUES ");
        let mut params: Vec<String> = Vec::with_capacity(batch.len() * 6);
        for (i, (kind, name, ls, le, signature)) in batch.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            let base = i * 6;
            sql.push_str(&format!("(?{},?{},?{},?{},?{},?{})", base + 1, base + 2, base + 3, base + 4, base + 5, base + 6));
            params.extend([fp.to_string(), kind.clone(), name.clone(), ls.to_string(), le.to_string(), signature.clone()]);
        }
        let refs: Vec<&str> = params.iter().map(String::as_str).collect();
        libsql_wasm::exec_params(db, &sql, &refs)?;
    }
    let size = file.size.to_string();
    let mtime = file.mtime_ms.to_string();
    let loc = file.loc.to_string();
    let symbols = file.symbols.len().to_string();
    let edges = file.edges.len().to_string();
    let parse_failed = if file.parse_failed { "1" } else { "0" };
    libsql_wasm::exec_params(
        db,
        &format!("INSERT OR REPLACE INTO {FILES_TABLE}(path,lang,size,mtime_ms,loc,symbols,edges,parse_failed) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"),
        &[fp, &file.lang, &size, &mtime, &loc, &symbols, &edges, parse_failed],
    )?;
    write_edges(fp, &file.edges, project_path);
    Ok(())
}

fn edges_key(fp: &str) -> String {
    format!("cef-{:x}", code_index::crc32(fp))
}

fn write_edges(fp: &str, edges: &[CallEdge], project_path: Option<&str>) {
    let ns = edges_namespace(project_path);
    if edges.is_empty() {
        code_index::fv_delete(&ns, &edges_key(fp));
        return;
    }
    let packed: Vec<Value> = edges.iter().map(|e| json!([e.caller_symbol, e.callee_symbol, e.line])).collect();
    code_index::fv_put(&ns, &edges_key(fp), &json!({ "path": fp, "edges": packed }).to_string());
}

fn forget_file(db: &str, fp: &str, project_path: Option<&str>) {
    let _ = libsql_wasm::exec_params(db, &format!("DELETE FROM {SYMBOLS_TABLE} WHERE path=?1"), &[fp]);
    let _ = libsql_wasm::exec_params(db, &format!("DELETE FROM {FILES_TABLE} WHERE path=?1"), &[fp]);
    code_index::fv_delete(&edges_namespace(project_path), &edges_key(fp));
}

const LEGACY_EDGES_PURGE_FLAG_KEY: &str = "legacy_edges_purged";
const LEGACY_EDGES_DELETES_PER_PASS: usize = 50_000;
const LEGACY_EDGES_PURGE_BUDGET_MS: u64 = 20_000;

fn legacy_edges_flag_ns() -> String {
    format!("{}-meta", code_index::code_ns_for(None))
}

fn legacy_edges_purged(flag_ns: &str) -> bool {
    code_index::fv_query(flag_ns, "")
        .as_array()
        .map(|rows| rows.iter().any(|r| r.get("key").and_then(|k| k.as_str()) == Some(LEGACY_EDGES_PURGE_FLAG_KEY)))
        .unwrap_or(false)
}

/// Listing the legacy per-edge namespace measured 9.4s on a tree that still carried those rows, and
/// the old shape paid it on every index pass before deleting anything -- enough on its own to leave
/// every pass partial and the digest permanently stale. It now runs to completion once: bounded
/// deletes in one pass, then a flag so no later pass lists the namespace again.
fn purge_legacy_edges() {
    let flag_ns = legacy_edges_flag_ns();
    if legacy_edges_purged(&flag_ns) { return; }
    let ns = legacy_edges_namespace();
    let Some(legacy) = code_index::fv_query(&ns, "").as_array().cloned() else { return };
    if legacy.is_empty() {
        code_index::fv_put(&flag_ns, LEGACY_EDGES_PURGE_FLAG_KEY, "1");
        return;
    }
    let started = host_now_ms();
    let mut deleted = 0usize;
    for row in &legacy {
        if deleted >= LEGACY_EDGES_DELETES_PER_PASS { break; }
        if host_now_ms().saturating_sub(started) > LEGACY_EDGES_PURGE_BUDGET_MS { break; }
        if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
            code_index::fv_delete(&ns, key);
            deleted += 1;
        }
    }
    let drained = deleted >= legacy.len();
    if drained { code_index::fv_put(&flag_ns, LEGACY_EDGES_PURGE_FLAG_KEY, "1"); }
    let msg = format!("code_symbols: purge_legacy_edges rows={} deleted={} drained={} ms={}", legacy.len(), deleted, drained, host_now_ms().saturating_sub(started));
    unsafe { crate::wasm_dispatch::host_log(2, msg.as_ptr(), msg.len() as u32) };
}

fn extract_file(fp: &str, lang: &str, content: &str, size: u64, mtime_ms: u64) -> (FileSymbols, u32) {
    let (chunks, parse_failed) = code_index::extract_chunks_reporting_plugin_failure(fp, content, lang);
    let has_function = chunks.iter().any(|(kind, ..)| kind.contains("function") || kind.contains("method"));
    let edges = if parse_failed || !has_function { Vec::new() } else { code_index::extract_call_edges(content, lang, &chunks) };
    let symbols = chunks
        .iter()
        .map(|(kind, name, ls, le, body)| {
            let signature = signature_of(name, body);
            (kind.clone(), display_name(name, &signature), *ls, *le, signature)
        })
        .collect();
    let file = FileSymbols { lang: lang.to_string(), size, mtime_ms, loc: content.lines().count(), symbols, edges, parse_failed };
    (file, parse_failed as u32)
}

pub(crate) fn sync_files(
    files: &[String],
    project_path: Option<&str>,
    started_ms: u64,
    budget_ms: u64,
    max_file_bytes: usize,
    prune_absent: bool,
) -> Value {
    let db = db_path(project_path);
    if let Err(e) = ensure_schema(&db) {
        return json!({ "ok": false, "error": e });
    }
    purge_legacy_edges();
    let known: HashMap<String, (u64, u64)> = rows(&db, &format!("SELECT path, size, mtime_ms FROM {FILES_TABLE}"), &[])
        .iter()
        .map(|r| (string(r, "path"), (number(r.get("size")), number(r.get("mtime_ms")))))
        .collect();
    let size_cap = max_file_bytes.saturating_mul(SOURCE_SIZE_CAP_MULTIPLIER);
    let (mut synced, mut unchanged, mut deferred, mut symbols_written, mut edges_written, mut parse_failures) = (0u32, 0u32, 0u32, 0usize, 0usize, 0u32);
    let mut seen: HashSet<String> = HashSet::new();
    for raw in files {
        let fp = raw.trim_start_matches("./").trim_start_matches('/').to_string();
        seen.insert(fp.clone());
        let ext_dot = fp.rfind('.');
        let ext = ext_dot.map(|dot| fp[dot..].to_lowercase()).unwrap_or_default();
        if NO_SYMBOL_EXTS.iter().any(|e| ext == *e) { continue; }
        let Some(lang) = ext_dot.and_then(|dot| code_index::lang_for_ext(&fp[dot..])) else { continue };
        if host_now_ms().saturating_sub(started_ms) > budget_ms {
            deferred += 1;
            continue;
        }
        let stat = host_stat(&fp).or_else(|| host_stat(raw));
        let size = stat.as_ref().map(|s| number(s.get("size"))).unwrap_or(0);
        let mtime_ms = stat.as_ref().map(|s| number(s.get("mtime_ms"))).unwrap_or(0);
        if known.get(&fp) == Some(&(size, mtime_ms)) && mtime_ms > 0 {
            unchanged += 1;
            continue;
        }
        let Some(content) = host_read(&fp).or_else(|| host_read(raw)).or_else(|| host_read(&format!("/{fp}"))) else { continue };
        if content.len() > size_cap {
            continue;
        }
        if content.lines().any(|l| l.len() > MINIFIED_MAX_LINE_CHARS) {
            continue;
        }
        let file_started = host_now_ms();
        let (file, failed) = extract_file(&fp, lang, &content, size.max(content.len() as u64), mtime_ms);
        let file_ms = host_now_ms().saturating_sub(file_started);
        if file_ms > SLOW_EXTRACT_LOG_MS {
            let msg = format!("code_symbols: slow extract ms={} bytes={} fp={}", file_ms, content.len(), fp);
            unsafe { crate::wasm_dispatch::host_log(2, msg.as_ptr(), msg.len() as u32) };
        }
        parse_failures += failed;
        if store_file(&db, &fp, &file, project_path).is_ok() {
            synced += 1;
            symbols_written += file.symbols.len();
            edges_written += file.edges.len();
        }
    }
    let mut removed = 0u32;
    if prune_absent && deferred == 0 && !seen.is_empty() {
        for path in known.keys().filter(|p| !seen.contains(*p)) {
            forget_file(&db, path, project_path);
            removed += 1;
        }
    }
    let report = json!({
        "ok": true,
        "files_synced": synced,
        "files_unchanged": unchanged,
        "files_deferred": deferred,
        "files_removed": removed,
        "symbols_written": symbols_written,
        "edges_written": edges_written,
        "treesitter_failures": parse_failures,
        "complete": deferred == 0,
        "elapsed_ms": host_now_ms().saturating_sub(started_ms),
    });
    if synced > 0 || removed > 0 || deferred > 0 {
        crate::wasm_dispatch::emit_event("codeinsight_symbols_synced", report.clone());
    }
    report
}

pub(crate) fn sync_tree(cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> Value {
    let started = host_now_ms();
    let root = project_path.filter(|p| !p.is_empty()).unwrap_or(".");
    let files = code_index::collect_files(root, cfg.index.digest_max_files, &cfg.index);
    sync_files(&files, project_path, started, cfg.index.wall_budget_ms, cfg.index.max_file_bytes, true)
}

pub(crate) fn clear(project_path: Option<&str>) {
    let db = db_path(project_path);
    let _ = libsql_wasm::exec(&db, &format!("DELETE FROM {SYMBOLS_TABLE}"));
    let _ = libsql_wasm::exec(&db, &format!("DELETE FROM {FILES_TABLE}"));
    let ns = edges_namespace(project_path);
    if let Some(stored) = code_index::fv_query(&ns, "").as_array() {
        for row in stored {
            if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
                code_index::fv_delete(&ns, key);
            }
        }
    }
}

struct Edge {
    path: String,
    caller: String,
    callee: String,
    line: u64,
}

fn load_edges(project_path: Option<&str>) -> Vec<Edge> {
    let stored = code_index::fv_query(&edges_namespace(project_path), "");
    let mut out = Vec::new();
    for row in stored.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(parsed) = row.get("value").and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok()) else { continue };
        let path = string(&parsed, "path");
        for e in parsed.get("edges").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or(&[]) {
            let Some(triple) = e.as_array() else { continue };
            let text = |i: usize| triple.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
            out.push(Edge { path: path.clone(), caller: text(0), callee: text(1), line: number(triple.get(2)) });
        }
    }
    out
}

fn definitions(db: &str, name: &str, limit: usize) -> Vec<Value> {
    let limit_s = limit.to_string();
    rows(
        db,
        &format!("SELECT path, kind, line_start, line_end, signature FROM {SYMBOLS_TABLE} WHERE name=?1 ORDER BY path, line_start LIMIT ?2"),
        &[name, &limit_s],
    )
}

fn location(row: &Value) -> String {
    format!("{}:{}-{}", string(row, "path"), number(row.get("line_start")), number(row.get("line_end")))
}

fn definition_locations(db: &str, name: &str) -> Vec<String> {
    definitions(db, name, 6).iter().map(|d| format!("{} {}", location(d), string(d, "kind"))).collect()
}

fn unique_definition(db: &str, name: &str) -> Option<String> {
    let defs = definitions(db, name, 2);
    if defs.len() == 1 { defs.first().map(location) } else { None }
}

fn container_of(db: &str, path: &str, line_start: u64, line_end: u64) -> Option<String> {
    let (ls, le) = (line_start.to_string(), line_end.to_string());
    rows(
        db,
        &format!(
            "SELECT name FROM {SYMBOLS_TABLE} WHERE path=?1 AND line_start<?2 AND line_end>=?3 AND kind IN {CONTAINER_KINDS_SQL} ORDER BY (line_end-line_start) ASC LIMIT 1"
        ),
        &[path, &ls, &le],
    )
    .first()
    .map(|r| string(r, "name"))
    .filter(|n| !n.is_empty())
}

fn coverage(db: &str) -> Value {
    let (embedded_files, embedded_chunks) = code_index::embedded_coverage(db);
    let symbol_files = single_count(db, &format!("SELECT COUNT(*) AS c FROM {FILES_TABLE}"));
    json!({
        "symbol_files": symbol_files,
        "embedded_files": embedded_files,
        "embedded_chunks": embedded_chunks,
        "semantic_search_covers_files_fraction": if symbol_files == 0 { 0.0 } else { (embedded_files as f64 / symbol_files as f64 * 100.0).round() / 100.0 },
    })
}

pub(crate) fn lean_overview(stored_digest: Option<String>) -> Value {
    let db = db_path(None);
    let files = match libsql_wasm::query_params(&db, &format!("SELECT COUNT(*) AS c FROM {FILES_TABLE}"), &[]) {
        Ok(r) => r.as_array().and_then(|a| a.first()).map(|r| number(r.get("c"))).unwrap_or(0),
        Err(e) => {
            if stored_digest.is_none() {
                return Value::Null;
            }
            let lower = e.to_ascii_lowercase();
            let reason = if lower.contains("unknown plugin") || lower.contains("unknown_plugin") { "libsql_plugin_unavailable" } else if lower.contains("no such table") { "symbols_not_indexed_yet" } else { "libsql_query_failed" };
            return json!({ "codeinsight_available": false, "codeinsight_unavailable_reason": reason, "digest": stored_digest });
        }
    };
    if files == 0 && stored_digest.is_none() {
        return Value::Null;
    }
    let symbol_count = single_count(&db, &format!("SELECT COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind NOT IN {DOC_KINDS_SQL}"));
    let doc_sections = single_count(&db, &format!("SELECT COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind IN {DOC_KINDS_SQL}"));
    json!({
        "codeinsight_available": true,
        "file_count": files,
        "symbol_count": symbol_count,
        "doc_sections": doc_sections,
        "by_kind": rows(&db, &format!("SELECT kind, COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind NOT IN {DOC_KINDS_SQL} GROUP BY kind ORDER BY c DESC LIMIT 8"), &[]),
        "by_language": by_language(&db, 6),
        "largest_files": rows(&db, &format!("SELECT path, symbols AS c FROM {FILES_TABLE} ORDER BY symbols DESC LIMIT 5"), &[]),
        "coverage": coverage(&db),
        "digest": stored_digest,
    })
}

fn by_language(db: &str, limit: usize) -> Vec<Value> {
    let limit_s = limit.to_string();
    rows(
        db,
        &format!("SELECT lang, COUNT(*) AS files, SUM(loc) AS loc, SUM(symbols) AS symbols FROM {FILES_TABLE} GROUP BY lang ORDER BY loc DESC LIMIT ?1"),
        &[&limit_s],
    )
}

fn by_area(db: &str, limit: usize) -> Vec<Value> {
    let mut areas: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for r in rows(db, &format!("SELECT path, loc, symbols FROM {FILES_TABLE}"), &[]) {
        let path = string(&r, "path");
        let segments: Vec<&str> = path.split('/').collect();
        let area = if segments.len() > 2 { segments[..2].join("/") } else if segments.len() == 2 { segments[0].to_string() } else { ".".to_string() };
        let entry = areas.entry(area).or_default();
        entry.0 += 1;
        entry.1 += number(r.get("loc"));
        entry.2 += number(r.get("symbols"));
    }
    let mut sorted: Vec<(String, (u64, u64, u64))> = areas.into_iter().collect();
    sorted.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    sorted
        .into_iter()
        .take(limit)
        .map(|(area, (files, loc, symbols))| json!({ "area": area, "files": files, "loc": loc, "symbols": symbols }))
        .collect()
}

fn callee_fan_in(edges: &[Edge]) -> Vec<(String, usize, usize)> {
    let mut callers_by_callee: HashMap<&str, (usize, HashSet<(&str, &str)>)> = HashMap::new();
    for e in edges {
        let entry = callers_by_callee.entry(e.callee.as_str()).or_default();
        entry.0 += 1;
        entry.1.insert((e.path.as_str(), e.caller.as_str()));
    }
    let mut out: Vec<(String, usize, usize)> = callers_by_callee.into_iter().map(|(name, (calls, distinct))| (name.to_string(), calls, distinct.len())).collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    out
}

fn fan_in_rows(db: &str, edges: &[Edge], limit: usize) -> Vec<Value> {
    callee_fan_in(edges)
        .into_iter()
        .filter(|(name, _, _)| !definitions(db, name, 1).is_empty())
        .take(limit)
        .map(|(name, calls, distinct_callers)| {
            json!({ "symbol": name, "calls": calls, "distinct_callers": distinct_callers, "defined_at": unique_definition(db, &name) })
        })
        .collect()
}

fn fan_out_rows(edges: &[Edge], limit: usize) -> Vec<Value> {
    let mut callees_by_caller: HashMap<(&str, &str), HashSet<&str>> = HashMap::new();
    for e in edges {
        callees_by_caller.entry((e.path.as_str(), e.caller.as_str())).or_default().insert(e.callee.as_str());
    }
    let mut out: Vec<((&str, &str), usize)> = callees_by_caller.into_iter().map(|(k, v)| (k, v.len())).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.into_iter().take(limit).map(|((path, caller), n)| json!({ "symbol": caller, "path": path, "distinct_callees": n })).collect()
}

fn function_symbol_rows(db: &str) -> Vec<Value> {
    rows(
        db,
        &format!(
            "SELECT path, kind, name, line_start, line_end FROM {SYMBOLS_TABLE} WHERE (kind LIKE '%function%' OR kind LIKE '%method%') AND name != '' ORDER BY path, line_start"
        ),
        &[],
    )
}

fn no_direct_callers(db: &str, edges: &[Edge], limit: usize) -> (usize, Vec<String>) {
    let called: HashSet<&str> = edges.iter().map(|e| e.callee.as_str()).collect();
    let orphans: Vec<Value> = function_symbol_rows(db)
        .into_iter()
        .filter(|r| {
            let name = string(r, "name");
            name != "main" && !called.contains(name.as_str())
        })
        .collect();
    let sample = orphans.iter().take(limit).map(|r| format!("{} {}", location(r), string(r, "name"))).collect();
    (orphans.len(), sample)
}

fn biggest_symbols(db: &str, limit: usize) -> Vec<String> {
    let limit_s = limit.to_string();
    rows(
        db,
        &format!(
            "SELECT path, kind, name, line_start, line_end, (line_end-line_start+1) AS span FROM {SYMBOLS_TABLE} WHERE kind NOT IN {DOC_KINDS_SQL} AND kind NOT IN ('impl_item','trait_item','class_declaration','class_definition') ORDER BY span DESC LIMIT ?1"
        ),
        &[&limit_s],
    )
    .iter()
    .map(|r| format!("{} lines {} {} {}", number(r.get("span")), location(r), string(r, "kind"), string(r, "name")))
    .collect()
}

fn overview(project_path: Option<&str>, limit: usize) -> Value {
    let db = db_path(project_path);
    let edges = load_edges(project_path);
    let (orphan_total, orphan_sample) = no_direct_callers(&db, &edges, limit);
    let mut out = lean_overview(code_index::stored_digest_at(project_path));
    if let Some(map) = out.as_object_mut() {
        map.insert("by_language".into(), json!(by_language(&db, 12)));
        map.insert("by_area".into(), json!(by_area(&db, limit.min(15))));
        map.insert("biggest_symbols".into(), json!(biggest_symbols(&db, limit.min(10))));
        map.insert("most_called".into(), json!(fan_in_rows(&db, &edges, limit.min(10))));
        map.insert("widest_callers".into(), json!(fan_out_rows(&edges, limit.min(10))));
        map.insert("call_edges".into(), json!(edges.len()));
        map.insert("functions_without_direct_callers".into(), json!({ "total": orphan_total, "sample": orphan_sample }));
    }
    out
}

fn status(project_path: Option<&str>) -> Value {
    let db = db_path(project_path);
    let files = rows(&db, &format!("SELECT lang, COUNT(*) AS files, SUM(parse_failed) AS parse_failed, SUM(CASE WHEN symbols=0 THEN 1 ELSE 0 END) AS without_symbols FROM {FILES_TABLE} GROUP BY lang ORDER BY files DESC"), &[]);
    json!({
        "coverage": coverage(&db),
        "by_language_health": files,
        "stored_digest": code_index::stored_digest_at(project_path),
        "hint": "action=sync completes the symbol index without embeddings; codeinsight_index continues the semantic (embedding) index",
    })
}

fn outline(db: &str, path: &str) -> Result<Value, String> {
    let normalized = path.trim_start_matches("./").trim_start_matches('/');
    let file = rows(db, &format!("SELECT lang, loc, symbols FROM {FILES_TABLE} WHERE path=?1"), &[normalized]);
    let Some(file) = file.first() else {
        let like = format!("%{normalized}%");
        let near: Vec<String> = rows(db, &format!("SELECT path FROM {FILES_TABLE} WHERE path LIKE ?1 ORDER BY path LIMIT 8"), &[&like]).iter().map(|r| string(r, "path")).collect();
        return Err(format!("path not indexed: {normalized}; near matches: {}", near.join(", ")));
    };
    let symbols = rows(db, &format!("SELECT kind, name, line_start, line_end, signature FROM {SYMBOLS_TABLE} WHERE path=?1 ORDER BY line_start, line_end DESC"), &[normalized]);
    let mut stack: Vec<u64> = Vec::new();
    let lines: Vec<String> = symbols
        .iter()
        .map(|s| {
            let (ls, le) = (number(s.get("line_start")), number(s.get("line_end")));
            while stack.last().is_some_and(|end| ls > *end) {
                stack.pop();
            }
            let indent = "  ".repeat(stack.len());
            stack.push(le);
            format!("{indent}{ls}-{le} {} {}", string(s, "kind"), string(s, "signature"))
        })
        .collect();
    Ok(json!({ "path": normalized, "lang": string(file, "lang"), "loc": number(file.get("loc")), "symbol_count": symbols.len(), "outline": lines }))
}

fn find(db: &str, name: &str, kind: Option<&str>, path_prefix: Option<&str>, limit: usize) -> Value {
    let like = format!("%{}%", name.replace('%', "").replace('_', ""));
    let prefix_like = format!("{}%", path_prefix.unwrap_or("").trim_start_matches("./"));
    let limit_s = (limit + 1).to_string();
    let kind_filter = kind.unwrap_or("");
    let found = rows(
        db,
        &format!(
            "SELECT path, kind, name, line_start, line_end, signature, CASE WHEN name=?1 THEN 0 WHEN LOWER(name)=LOWER(?1) THEN 1 WHEN LOWER(name) LIKE LOWER(?1)||'%' THEN 2 ELSE 3 END AS rank FROM {SYMBOLS_TABLE} WHERE (name LIKE ?2 OR signature LIKE ?2) AND (?3='' OR kind=?3) AND path LIKE ?4 ORDER BY rank, LENGTH(name), path, line_start LIMIT ?5"
        ),
        &[name, &like, kind_filter, &prefix_like, &limit_s],
    );
    let truncated = found.len() > limit;
    let matches: Vec<String> = found
        .iter()
        .take(limit)
        .map(|r| {
            let container = container_of(db, &string(r, "path"), number(r.get("line_start")), number(r.get("line_end")));
            let inside = container.map(|c| format!(" in {c}")).unwrap_or_default();
            format!("{} {}{} :: {}", location(r), string(r, "kind"), inside, string(r, "signature"))
        })
        .collect();
    json!({ "query": name, "matches": matches, "truncated": truncated })
}

fn edge_line(e: &Edge) -> String {
    format!("{}:{} {} -> {}", e.path, e.line, e.caller, e.callee)
}

fn callers_or_callees(db: &str, project_path: Option<&str>, symbol: &str, want_callers: bool, limit: usize) -> Value {
    let edges = load_edges(project_path);
    let matched: Vec<&Edge> = edges.iter().filter(|e| if want_callers { e.callee == symbol } else { e.caller == symbol }).collect();
    let total = matched.len();
    let mut out = json!({
        "symbol": symbol,
        "defined_at": definition_locations(db, symbol),
        "total": total,
        "truncated": total > limit,
    });
    if want_callers {
        let distinct: HashSet<(&str, &str)> = matched.iter().map(|e| (e.path.as_str(), e.caller.as_str())).collect();
        out["distinct_callers"] = json!(distinct.len());
        out["edges"] = json!(matched.iter().take(limit).map(|e| edge_line(e)).collect::<Vec<_>>());
    } else {
        let mut by_callee: BTreeMap<&str, usize> = BTreeMap::new();
        for e in &matched {
            *by_callee.entry(e.callee.as_str()).or_default() += 1;
        }
        let mut ranked: Vec<(&str, usize)> = by_callee.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out["distinct_callees"] = json!(ranked.len());
        out["callees"] = json!(ranked
            .into_iter()
            .take(limit)
            .map(|(name, n)| match unique_definition(db, name) {
                Some(at) => format!("{name} x{n} ({at})"),
                None => format!("{name} x{n}"),
            })
            .collect::<Vec<_>>());
    }
    if total == 0 {
        out["note"] = json!("no call edges recorded for this symbol name; edges are keyed by simple callee name, so check spelling with action=find and confirm action=status shows the defining file synced");
    }
    out
}

fn impact(db: &str, project_path: Option<&str>, symbol: &str, max_depth: usize, upstream: bool, limit: usize) -> Value {
    let depth_cap = max_depth.clamp(1, 10);
    let edges = load_edges(project_path);
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &edges {
        let (from, to) = if upstream { (e.callee.as_str(), e.caller.as_str()) } else { (e.caller.as_str(), e.callee.as_str()) };
        adjacency.entry(from).or_default().push(to);
    }
    let mut visited: HashMap<&str, usize> = HashMap::from([(symbol, 0)]);
    let mut frontier = vec![symbol];
    for depth in 1..=depth_cap {
        let mut next = Vec::new();
        for node in &frontier {
            for neighbour in adjacency.get(node).map(Vec::as_slice).unwrap_or(&[]) {
                if !visited.contains_key(neighbour) {
                    visited.insert(neighbour, depth);
                    next.push(*neighbour);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let mut reached: Vec<(&str, usize)> = visited.into_iter().filter(|(name, _)| *name != symbol).collect();
    reached.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
    let total = reached.len();
    let lines: Vec<String> = reached
        .into_iter()
        .take(limit)
        .map(|(name, depth)| match unique_definition(db, name) {
            Some(at) => format!("d{depth} {name} ({at})"),
            None => format!("d{depth} {name}"),
        })
        .collect();
    json!({
        "symbol": symbol,
        "direction": if upstream { "callers: what breaks if this changes" } else { "callees: what this depends on" },
        "defined_at": definition_locations(db, symbol),
        "max_depth": depth_cap,
        "total": total,
        "truncated": total > limit,
        "reached": lines,
    })
}

fn hotspots(db: &str, project_path: Option<&str>, limit: usize) -> Value {
    let edges = load_edges(project_path);
    let limit_s = limit.to_string();
    json!({
        "most_called": fan_in_rows(db, &edges, limit),
        "widest_callers": fan_out_rows(&edges, limit),
        "biggest_symbols": biggest_symbols(db, limit),
        "densest_files": rows(db, &format!("SELECT path, symbols, loc FROM {FILES_TABLE} ORDER BY symbols DESC LIMIT ?1"), &[&limit_s]),
        "longest_files": rows(db, &format!("SELECT path, loc, symbols FROM {FILES_TABLE} ORDER BY loc DESC LIMIT ?1"), &[&limit_s]),
    })
}

fn orphans(db: &str, project_path: Option<&str>, limit: usize) -> Value {
    let edges = load_edges(project_path);
    let (total, sample) = no_direct_callers(db, &edges, limit);
    json!({
        "meaning": "function or method symbols whose name is never the target of a recorded call; functions used only as values (callbacks, match arms, exports, trait impls) also appear here",
        "total": total,
        "truncated": total > limit,
        "functions": sample,
    })
}

fn text_field<'a>(body: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| body.get(*k).and_then(|v| v.as_str())).filter(|s| !s.is_empty())
}

pub(crate) fn handle(body: &Value) -> Result<Value, String> {
    let action = text_field(body, &["action", "mode"]).unwrap_or("overview");
    let project_path = text_field(body, &["root", "projectPath"]);
    let limit = body.get("limit").or_else(|| body.get("k")).and_then(|v| v.as_u64()).map(|n| (n as usize).clamp(1, MAX_LIMIT)).unwrap_or(DEFAULT_LIMIT);
    let db = db_path(project_path);
    let symbol = text_field(body, &["symbol", "name"]);
    let need_symbol = || symbol.ok_or_else(|| format!("{action} requires `symbol`"));
    match action {
        "overview" => Ok(overview(project_path, limit)),
        "status" => Ok(status(project_path)),
        "sync" => Ok(sync_tree(&crate::ragconfig::RagConfig::resolved(), project_path)),
        "outline" => outline(&db, text_field(body, &["path", "file"]).ok_or("outline requires `path`")?),
        "find" => Ok(find(&db, need_symbol()?, text_field(body, &["kind"]), text_field(body, &["path", "path_prefix"]), limit)),
        "callers" => Ok(callers_or_callees(&db, project_path, need_symbol()?, true, limit)),
        "callees" => Ok(callers_or_callees(&db, project_path, need_symbol()?, false, limit)),
        "impact" => {
            let depth = body.get("max_depth").and_then(|v| v.as_u64()).map(|n| n as usize).unwrap_or(DEFAULT_IMPACT_DEPTH);
            let upstream = text_field(body, &["direction"]).map(|d| d != "callees" && d != "downstream").unwrap_or(true);
            Ok(impact(&db, project_path, need_symbol()?, depth, upstream, limit))
        }
        "hotspots" => Ok(hotspots(&db, project_path, limit)),
        "orphans" => Ok(orphans(&db, project_path, limit)),
        other => Err(format!("unknown action `{other}`; accepted: {}", ACTIONS.join(", "))),
    }
}
