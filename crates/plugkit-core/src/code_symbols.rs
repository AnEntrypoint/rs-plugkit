#![cfg(target_arch = "wasm32")]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};

use crate::code_index::{self, CallEdge, FunctionMetrics, ImportRef};
use crate::libsql_wasm;
use crate::wasm_dispatch::{host_read, host_stat};

const FILES_TABLE: &str = "code_symbol_files";
const SYMBOLS_TABLE: &str = "code_symbols";
const IMPORTS_TABLE: &str = "code_imports";
const SCHEMA_VERSION: u64 = 3;
const DOC_KINDS_SQL: &str = "('section','document')";
const SOURCE_SIZE_CAP_MULTIPLIER: usize = 4;
const ROWS_PER_INSERT: usize = 120;
const SLOW_EXTRACT_LOG_MS: u64 = 500;
const NO_SYMBOL_EXTS: [&str; 1] = [".json"];
const SIGNATURE_MAX_CHARS: usize = 140;
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 200;
const DEFAULT_IMPACT_DEPTH: usize = 3;
const TEST_CALLER_DEPTH: usize = 6;
const CONTAINER_KINDS_SQL: &str =
    "('impl_item','trait_item','class_declaration','class_definition','struct_item','enum_item')";
const ACTIONS: &[&str] = &[
    "overview",
    "status",
    "sync",
    "outline",
    "find",
    "callers",
    "callees",
    "impact",
    "hotspots",
    "orphans",
    "imports",
    "importers",
    "cycles",
    "coupling",
    "complexity",
    "duplicates",
    "tests",
];
const RISK_CANDIDATE_POOL: usize = 100;
const COMMON_STD_NAMES: &[&str] = &[
    "path",
    "parent",
    "as_path",
    "to_path_buf",
    "file_stem",
    "components",
    "metadata",
    "store",
    "load",
    "set",
    "update",
    "run",
    "call",
    "apply",
    "size",
    "name",
    "id",
    "read_to_string",
    "exists",
    "trim_start_matches",
    "trim_end_matches",
    "trim_start",
    "trim_end",
    "strip_prefix",
    "strip_suffix",
    "split_once",
    "rsplit",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "is_file",
    "is_dir",
    "create_dir_all",
    "canonicalize",
    "file_name",
    "extension",
    "display",
    "borrow",
    "borrow_mut",
    "unwrap_err",
    "ok_or",
    "ok_or_else",
    "position",
    "retain",
    "truncate",
    "clear",
    "resize",
    "reserve",
    "with_capacity",
    "from_utf8_lossy",
    "to_vec",
    "as_slice",
    "get_or_insert_with",
    "sleep",
    "spawn",
    "send",
    "recv",
    "wait",
    "kill",
    "flush",
    "char_indices",
    "is_char_boundary",
    "is_alphanumeric",
    "new",
    "get",
    "get_mut",
    "len",
    "is_empty",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "as_str",
    "as_ref",
    "as_mut",
    "as_bytes",
    "as_array",
    "as_u64",
    "as_i64",
    "as_f64",
    "as_bool",
    "ok",
    "err",
    "map",
    "map_err",
    "and_then",
    "or_else",
    "unwrap",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "expect",
    "filter",
    "filter_map",
    "flat_map",
    "join",
    "insert",
    "remove",
    "push",
    "pop",
    "contains",
    "contains_key",
    "iter",
    "iter_mut",
    "into_iter",
    "collect",
    "cloned",
    "copied",
    "clone",
    "to_string",
    "to_owned",
    "into",
    "from",
    "try_from",
    "from_str",
    "default",
    "fmt",
    "eq",
    "cmp",
    "partial_cmp",
    "next",
    "take",
    "skip",
    "rev",
    "min",
    "max",
    "sort",
    "sort_by",
    "sort_by_key",
    "dedup",
    "extend",
    "entry",
    "or_insert",
    "or_insert_with",
    "or_default",
    "first",
    "last",
    "trim",
    "split",
    "lines",
    "chars",
    "bytes",
    "starts_with",
    "ends_with",
    "find",
    "replace",
    "lock",
    "drop",
    "read",
    "write",
    "open",
    "close",
    "push_str",
    "format",
    "then",
    "any",
    "all",
    "count",
    "sum",
    "zip",
    "enumerate",
    "chain",
    "flatten",
    "abs",
    "saturating_sub",
    "saturating_add",
    "min_by_key",
    "max_by_key",
    "parse",
    "keys",
    "values",
];

fn uniquely_defined_names(db: &str) -> HashSet<String> {
    rows(
        db,
        &format!(
            "SELECT name FROM {SYMBOLS_TABLE} WHERE name != '' GROUP BY name HAVING COUNT(*) = 1"
        ),
        &[],
    )
    .iter()
    .map(|r| string(r, "name"))
    .collect()
}

fn is_common_std_name(name: &str) -> bool {
    COMMON_STD_NAMES.contains(&name)
}

fn is_identifier_like(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

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
    v.and_then(|x| {
        x.as_u64()
            .or_else(|| x.as_f64().map(|f| f as u64))
            .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
    })
    .unwrap_or(0)
}

fn string(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn rows(db: &str, sql: &str, params: &[&str]) -> Vec<Value> {
    libsql_wasm::query_params(db, sql, params)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn single_count(db: &str, sql: &str) -> u64 {
    rows(db, sql, &[])
        .first()
        .map(|r| number(r.get("c")))
        .unwrap_or(0)
}

const ADDED_SYMBOL_COLUMNS: &[(&str, &str)] = &[
    ("cx", "INTEGER"),
    ("nesting", "INTEGER"),
    ("params", "INTEGER"),
    ("sloc", "INTEGER"),
    ("node_count", "INTEGER"),
    ("shape_hash", "TEXT"),
    ("is_test", "INTEGER"),
    ("exported", "INTEGER"),
];

fn ensure_schema(db: &str) -> Result<(), String> {
    libsql_wasm::open(db)?;
    libsql_wasm::exec(db, &format!(
        "CREATE TABLE IF NOT EXISTS {FILES_TABLE} (path TEXT PRIMARY KEY, lang TEXT, size INTEGER, mtime_ms INTEGER, loc INTEGER, symbols INTEGER, edges INTEGER, parse_failed INTEGER, schema INTEGER, is_test INTEGER)"
    ))?;
    libsql_wasm::exec(db, &format!(
        "CREATE TABLE IF NOT EXISTS {SYMBOLS_TABLE} (id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, line_start INTEGER, line_end INTEGER, signature TEXT, cx INTEGER, nesting INTEGER, params INTEGER, sloc INTEGER, node_count INTEGER, shape_hash TEXT, is_test INTEGER, exported INTEGER)"
    ))?;
    libsql_wasm::exec(db, &format!("CREATE TABLE IF NOT EXISTS {IMPORTS_TABLE} (path TEXT NOT NULL, line INTEGER, spec TEXT)"))?;
    let _ = libsql_wasm::exec(
        db,
        &format!("ALTER TABLE {FILES_TABLE} ADD COLUMN schema INTEGER"),
    );
    let _ = libsql_wasm::exec(
        db,
        &format!("ALTER TABLE {FILES_TABLE} ADD COLUMN is_test INTEGER"),
    );
    let _ = libsql_wasm::exec(
        db,
        &format!("ALTER TABLE {FILES_TABLE} ADD COLUMN source_hash TEXT"),
    );
    for (column, kind) in ADDED_SYMBOL_COLUMNS {
        let _ = libsql_wasm::exec(
            db,
            &format!("ALTER TABLE {SYMBOLS_TABLE} ADD COLUMN {column} {kind}"),
        );
    }
    libsql_wasm::exec(
        db,
        &format!("CREATE INDEX IF NOT EXISTS {SYMBOLS_TABLE}_name ON {SYMBOLS_TABLE}(name)"),
    )?;
    libsql_wasm::exec(
        db,
        &format!("CREATE INDEX IF NOT EXISTS {SYMBOLS_TABLE}_path ON {SYMBOLS_TABLE}(path)"),
    )?;
    libsql_wasm::exec(
        db,
        &format!("CREATE INDEX IF NOT EXISTS {IMPORTS_TABLE}_path ON {IMPORTS_TABLE}(path)"),
    )?;
    Ok(())
}

fn signature_of(name: &str, body: &str) -> String {
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let signature = if first_line.is_empty() {
        name
    } else {
        first_line
    };
    signature.chars().take(SIGNATURE_MAX_CHARS).collect()
}

fn display_name(name: &str, signature: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    signature.trim_end_matches('{').trim().to_string()
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or("");
    lower
        .split('/')
        .any(|seg| matches!(seg, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || file.contains(".test.")
        || file.contains(".spec.")
        || file.ends_with("_test.go")
        || file.ends_with("_test.py")
        || file.ends_with("_test.rs")
        || file.starts_with("test_")
        || file.ends_with("test.java")
        || file.ends_with("tests.cs")
}

fn is_test_symbol(name: &str, file_is_test: bool) -> bool {
    file_is_test
        || name.starts_with("test_")
        || name.starts_with("Test")
        || name.starts_with("Benchmark")
}

fn is_exported_signature(signature: &str) -> bool {
    signature.starts_with("pub ")
        || signature.starts_with("pub(")
        || signature.starts_with("export ")
        || signature.starts_with("public ")
}

struct SymbolRow {
    kind: String,
    name: String,
    line_start: usize,
    line_end: usize,
    signature: String,
    metrics: Option<FunctionMetrics>,
    is_test: bool,
    exported: bool,
}

struct FileSymbols {
    lang: String,
    source_hash: String,
    size: u64,
    mtime_ms: u64,
    loc: usize,
    is_test: bool,
    symbols: Vec<SymbolRow>,
    edges: Vec<CallEdge>,
    imports: Vec<ImportRef>,
    parse_failed: bool,
}

fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''").replace('\0', ""))
}

fn sql_optional_number(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "NULL".to_string())
}

fn store_file(
    db: &str,
    fp: &str,
    file: &FileSymbols,
    project_path: Option<&str>,
) -> Result<(), String> {
    let path = sql_text(fp);
    let mut script = format!("BEGIN; DELETE FROM {SYMBOLS_TABLE} WHERE path={path}; DELETE FROM {IMPORTS_TABLE} WHERE path={path};");
    for batch in file.symbols.chunks(ROWS_PER_INSERT) {
        let values: Vec<String> = batch
            .iter()
            .map(|s| {
                let m = s.metrics.as_ref();
                let shape = m
                    .filter(|x| x.shape_hash != 0)
                    .map(|x| sql_text(&format!("{:016x}", x.shape_hash)))
                    .unwrap_or_else(|| "NULL".to_string());
                format!(
                    "({path},{},{},{},{},{},{},{},{},{},{},{shape},{},{})",
                    sql_text(&s.kind),
                    sql_text(&s.name),
                    s.line_start,
                    s.line_end,
                    sql_text(&s.signature),
                    sql_optional_number(m.map(|x| x.cx as u64)),
                    sql_optional_number(m.map(|x| x.nesting as u64)),
                    sql_optional_number(m.map(|x| x.params as u64)),
                    sql_optional_number(m.map(|x| x.sloc as u64)),
                    sql_optional_number(m.map(|x| x.node_count as u64)),
                    s.is_test as u8,
                    s.exported as u8,
                )
            })
            .collect();
        script.push_str(&format!(
            " INSERT INTO {SYMBOLS_TABLE}(path,kind,name,line_start,line_end,signature,cx,nesting,params,sloc,node_count,shape_hash,is_test,exported) VALUES {};",
            values.join(",")
        ));
    }
    for batch in file.imports.chunks(ROWS_PER_INSERT) {
        let values: Vec<String> = batch
            .iter()
            .map(|i| format!("({path},{},{})", i.line, sql_text(&i.spec)))
            .collect();
        script.push_str(&format!(
            " INSERT INTO {IMPORTS_TABLE}(path,line,spec) VALUES {};",
            values.join(",")
        ));
    }
    script.push_str(&format!(
        " INSERT OR REPLACE INTO {FILES_TABLE}(path,lang,size,mtime_ms,loc,symbols,edges,parse_failed,schema,is_test,source_hash) VALUES ({path},{},{},{},{},{},{},{},{SCHEMA_VERSION},{},{}); COMMIT;",
        sql_text(&file.lang),
        file.size,
        file.mtime_ms,
        file.loc,
        file.symbols.len(),
        file.edges.len(),
        file.parse_failed as u8,
        file.is_test as u8,
        sql_text(&file.source_hash),
    ));
    if !write_edges(fp, &file.edges, project_path) {
        return Err(format!("failed to store call edges for {fp}"));
    }
    libsql_wasm::exec(db, &script)?;
    Ok(())
}

fn edges_key(fp: &str) -> String {
    format!("cef-{:x}", code_index::crc32(fp))
}

fn write_edges(fp: &str, edges: &[CallEdge], project_path: Option<&str>) -> bool {
    let ns = edges_namespace(project_path);
    let packed: Vec<Value> = edges
        .iter()
        .map(|e| json!([e.caller_symbol, e.callee_symbol, e.line]))
        .collect();
    code_index::fv_put(
        &ns,
        &edges_key(fp),
        &json!({ "path": fp, "edges": packed }).to_string(),
    )
}

fn purge_legacy_edges(started_ms: u64, budget_ms: u64) {
    let ns = legacy_edges_namespace();
    let Some(legacy) = code_index::fv_query(&ns, "").as_array().cloned() else {
        return;
    };
    for row in legacy {
        if host_now_ms().saturating_sub(started_ms) > budget_ms {
            return;
        }
        if let Some(key) = row.get("key").and_then(|k| k.as_str()) {
            code_index::fv_delete(&ns, key);
        }
    }
}

fn extract_file(
    fp: &str,
    lang: &str,
    content: &str,
    size: u64,
    mtime_ms: u64,
) -> (FileSymbols, u32) {
    let analysis = code_index::analyze_source(fp, content, lang);
    let file_is_test = is_test_path(fp);
    let mut metrics = analysis.metrics.into_iter();
    let symbols = analysis
        .chunks
        .iter()
        .map(|(kind, name, ls, le, body)| {
            let signature = signature_of(name, body);
            let display = display_name(name, &signature);
            SymbolRow {
                kind: kind.clone(),
                is_test: is_test_symbol(&display, file_is_test),
                exported: is_exported_signature(&signature),
                name: display,
                line_start: *ls,
                line_end: *le,
                signature,
                metrics: metrics.next().flatten(),
            }
        })
        .collect();
    let failed = analysis.parse_failed as u32;
    let file = FileSymbols {
        lang: lang.to_string(),
        source_hash: format!("{:016x}", crate::hash::fnv1a64(content.as_bytes())),
        size,
        mtime_ms,
        loc: content.lines().count(),
        is_test: file_is_test,
        symbols,
        edges: analysis.edges,
        imports: analysis.imports,
        parse_failed: analysis.parse_failed,
    };
    (file, failed)
}

const DEFERRED_SET_NAMES_LIMIT: usize = 256;
const DEFERRED_PARTIAL_REASON: &str = "the symbol refresh deferred this file in this pass, so the outline is its cached row and may be stale; codeinsight_index.deferred_set names the deferred files, and deferred_set_truncated says when that list is cut short";
const DEFERRED_NEVER_INDEXED_REASON: &str = "the symbol refresh deferred this file before it was ever indexed, so the empty outline is not evidence that the file has no symbols; codeinsight_index.deferred_set names the deferred files";
const UNLISTED_PARTIAL_REASON: &str = "the file is not in the current listing, so the outline is its cached row from an earlier pass";
const FAILED_PARTIAL_REASON: &str = "the symbol refresh could not fully read, parse or store this file in this pass, so the outline is its cached row and may be stale or incomplete";
const REFRESH_NOT_RUN_PARTIAL_REASON: &str = "the symbol refresh did not reach the file listing in this pass, so the outline is its cached row, if one exists, and may be stale; codeinsight_index.error names the cause";
const UNINDEXED_IN_PASS_REASON: &str = "the symbol refresh did not index this file in this pass, so the empty outline is not evidence that the file has no symbols";

fn record_deferred(
    deferred_paths: &mut Vec<String>,
    focus_deferred: &mut bool,
    focus: Option<&str>,
    fp: &str,
) {
    if focus == Some(fp) {
        *focus_deferred = true;
    }
    deferred_paths.push(fp.to_string());
}

fn record_focus_failure(focus_failed: &mut bool, focus: Option<&str>, fp: &str) {
    if focus == Some(fp) {
        *focus_failed = true;
    }
}

const STORE_BUSY_REFRESH_NOTE: &str = "the shared libsql store is held by another writer (its lock directory is present), so this pass did not refresh the symbol index; the raw store error is withheld";

fn is_store_busy_error(err: &str) -> bool {
    libsql_wasm::classify_error(err) == libsql_wasm::LibsqlErrorKind::Busy
        || err.to_ascii_lowercase().contains("database is locked")
}

/// A `<db>.lock` directory is libsql's mutual-exclusion marker under this VFS: the writer removes it
/// when it finishes, so a process killed mid-write leaves it forever and every later write fails with
/// no holder to wait on. Every dispatch that writes gm.db records its pid and a heartbeat timestamp
/// beside it in `<db>.lock.owner` (the daemon's store-owner claim), so that record is the liveness
/// evidence the directory itself cannot carry: no record means no writer ever claimed it, a dead pid
/// means the writer is provably gone, and a heartbeat older than STORE_LOCK_STALE_MS means a writer
/// that stopped heartbeating. A lock whose recorded pid is alive is never touched -- only its owner
/// may remove it.
const STORE_LOCK_STALE_MS: u64 = 900_000;

/// Runs on the host because the WASI VFS offers no directory removal: `fs.rmSync` is the only way to
/// clear a lock directory from here. Prints one JSON object: `state` is clear, reaped or held.
const STORE_LOCK_SETTLE_JS: &str = r#"(function () {
  var fs = require('node:fs');
  var now = Date.now();
  var out = function (o) { process.stdout.write(JSON.stringify(o)); };
  if (!fs.existsSync(P.dir)) { out({ state: 'clear' }); return; }
  var pid = null, ts = 0;
  try {
    var lines = fs.readFileSync(P.own, 'utf8').split('\n');
    pid = parseInt(lines[0], 10);
    ts = parseInt(lines[1], 10) || 0;
  } catch (e) { pid = null; ts = 0; }
  var hasPid = typeof pid === 'number' && isFinite(pid) && pid > 0;
  var alive = false;
  if (hasPid) {
    if (pid === process.pid) { alive = true; }
    else { try { process.kill(pid, 0); alive = true; } catch (e) { alive = !!(e && e.code === 'EPERM'); } }
  }
  var ownerAge = ts > 0 ? now - ts : 0;
  var stale = !hasPid || !alive || (ts > 0 && ownerAge >= P.ttl);
  var shape = function (state, extra) {
    var o = { state: state, owner_pid: hasPid ? pid : null, owner_alive: alive, owner_age_ms: ts > 0 ? ownerAge : null };
    for (var k in extra) { o[k] = extra[k]; }
    return o;
  };
  if (!stale) { out(shape('held', {})); return; }
  try {
    fs.rmSync(P.dir, { recursive: true, force: true });
    fs.rmSync(P.own, { force: true });
    out(shape('reaped', { stale_reason: !hasPid ? 'no owner record' : (!alive ? 'owner pid is not alive' : 'owner heartbeat is older than ' + P.ttl + ' ms') }));
  } catch (e) {
    out(shape('held', { reap_error: String((e && e.message) || e) }));
  }
})();"#;

fn store_busy_lock_bail(lock_dir: &str, reason: &str, info: Value) -> Value {
    json!({
        "ok": false,
        "complete": false,
        "store_busy": true,
        "lock_dir": lock_dir,
        "lock_settle": info,
        "error": format!(
            "{} The refresh returned without waiting: the lock directory {} is present and {}.",
            STORE_BUSY_REFRESH_NOTE, lock_dir, reason
        )
    })
}

/// Clears a lock directory no live writer owns and returns None, or returns the store-busy bail that
/// sync_files must answer with. A lock whose recorded owner pid is alive is left alone.
fn settle_stale_store_lock(db: &str) -> Option<Value> {
    let lock_dir = format!("{db}.lock");
    if crate::wasm_dispatch::host_stat_is_directory(&lock_dir) != Some(true) {
        return None;
    }
    let params = serde_json::to_string(&json!({
        "dir": lock_dir,
        "own": format!("{lock_dir}.owner"),
        "ttl": STORE_LOCK_STALE_MS,
    }))
    .unwrap_or_else(|_| "{}".to_string());
    let timeout = json!({ "timeoutMs": 5000 }).to_string();
    let code = format!("const P = {params};\n{STORE_LOCK_SETTLE_JS}");
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            timeout.as_ptr(),
            timeout.len() as u32,
        )
    };
    let result = crate::wasm_dispatch::unpack_to_value_pub(packed);
    if result.get("timed_out").and_then(Value::as_bool) == Some(true)
        || result.get("exit_code").and_then(Value::as_i64) != Some(0)
    {
        return Some(store_busy_lock_bail(
            &lock_dir,
            "its owner could not be checked, so it was left in place",
            result,
        ));
    }
    let settle: Value = result
        .get("stdout")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    match settle.get("state").and_then(Value::as_str) {
        Some("clear") | Some("reaped") => None,
        Some("held") => {
            let reason = match settle.get("reap_error").and_then(Value::as_str) {
                Some(_) => "it is stale but could not be removed",
                None => "a live writer still owns it",
            };
            Some(store_busy_lock_bail(&lock_dir, reason, settle))
        }
        _ => Some(store_busy_lock_bail(
            &lock_dir,
            "its owner could not be determined, so it was left in place",
            settle,
        )),
    }
}

pub(crate) fn sync_files(
    files: &[String],
    project_path: Option<&str>,
    started_ms: u64,
    budget_ms: u64,
    max_file_bytes: usize,
    prune_absent: bool,
    focus: Option<&str>,
) -> Value {
    let db = db_path(project_path);
    if let Some(busy) = settle_stale_store_lock(&db) {
        return busy;
    }
    if let Err(e) = ensure_schema(&db) {
        if is_store_busy_error(&e) {
            return json!({ "ok": false, "complete": false, "store_busy": true, "error": STORE_BUSY_REFRESH_NOTE });
        }
        return json!({ "ok": false, "error": e });
    }
    purge_legacy_edges(started_ms, budget_ms);
    let known: HashMap<String, (u64, u64, u64, u64, String)> = rows(
        &db,
        &format!(
            "SELECT path, size, mtime_ms, schema, parse_failed, source_hash FROM {FILES_TABLE}"
        ),
        &[],
    )
    .iter()
    .map(|r| {
        (
            string(r, "path"),
            (
                number(r.get("size")),
                number(r.get("mtime_ms")),
                number(r.get("schema")),
                number(r.get("parse_failed")),
                string(r, "source_hash"),
            ),
        )
    })
    .collect();
    let size_cap = max_file_bytes.saturating_mul(SOURCE_SIZE_CAP_MULTIPLIER);
    let (
        mut synced,
        mut unchanged,
        mut deferred,
        mut symbols_written,
        mut edges_written,
        mut parse_failures,
    ) = (0u32, 0u32, 0u32, 0usize, 0usize, 0u32);
    let mut seen: HashSet<String> = HashSet::new();
    let mut deferred_paths: Vec<String> = Vec::new();
    let mut focus_deferred = false;
    let mut focus_failed = false;
    let mut unreadable = 0u32;
    let mut oversized = 0u32;
    let mut store_failures = 0u32;
    let mut store_busy = false;
    let mut unsupported = 0u32;
    for raw in files {
        let fp = raw
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string();
        if fp.rfind('.').is_some_and(|dot| {
            NO_SYMBOL_EXTS
                .iter()
                .any(|ext| fp[dot..].eq_ignore_ascii_case(ext))
        }) {
            seen.insert(fp.clone());
            unsupported += 1;
            if known.contains_key(&fp) {
                if host_now_ms().saturating_sub(started_ms) > budget_ms {
                    deferred += 1;
                    record_deferred(&mut deferred_paths, &mut focus_deferred, focus, &fp);
                    continue;
                }
                if !write_edges(&fp, &[], project_path) {
                    store_failures += 1;
                    record_focus_failure(&mut focus_failed, focus, &fp);
                    continue;
                }
                let path = sql_text(&fp);
                let script = format!("BEGIN; DELETE FROM {SYMBOLS_TABLE} WHERE path={path}; DELETE FROM {IMPORTS_TABLE} WHERE path={path}; DELETE FROM {FILES_TABLE} WHERE path={path}; COMMIT;");
                if let Err(e) = libsql_wasm::exec(&db, &script) {
                    if is_store_busy_error(&e) {
                        store_busy = true;
                    }
                    store_failures += 1;
                    record_focus_failure(&mut focus_failed, focus, &fp);
                }
            }
            continue;
        }
        let Some(lang) = fp
            .rfind('.')
            .and_then(|dot| code_index::lang_for_ext(&fp[dot..]))
        else {
            continue;
        };
        seen.insert(fp.clone());
        if host_now_ms().saturating_sub(started_ms) > budget_ms {
            deferred += 1;
            record_deferred(&mut deferred_paths, &mut focus_deferred, focus, &fp);
            continue;
        }
        let stat = host_stat(&fp).or_else(|| host_stat(raw));
        let size = stat.as_ref().map(|s| number(s.get("size"))).unwrap_or(0);
        let mtime_ms = stat
            .as_ref()
            .map(|s| number(s.get("mtime_ms")))
            .unwrap_or(0);
        let Some(content) = host_read(&fp)
            .or_else(|| host_read(raw))
            .or_else(|| host_read(&format!("/{fp}")))
        else {
            unreadable += 1;
            record_focus_failure(&mut focus_failed, focus, &fp);
            continue;
        };
        if content.len() > size_cap {
            oversized += 1;
            record_focus_failure(&mut focus_failed, focus, &fp);
            continue;
        }
        let source_hash = format!("{:016x}", crate::hash::fnv1a64(content.as_bytes()));
        if known.get(&fp) == Some(&(size, mtime_ms, SCHEMA_VERSION, 0, source_hash)) {
            unchanged += 1;
            continue;
        }
        let file_started = host_now_ms();
        let (file, failed) = extract_file(
            &fp,
            lang,
            &content,
            size.max(content.len() as u64),
            mtime_ms,
        );
        let file_ms = host_now_ms().saturating_sub(file_started);
        if file_ms > SLOW_EXTRACT_LOG_MS {
            let msg = format!(
                "code_symbols: slow extract ms={} bytes={} fp={}",
                file_ms,
                content.len(),
                fp
            );
            unsafe { crate::wasm_dispatch::host_log(2, msg.as_ptr(), msg.len() as u32) };
        }
        parse_failures += failed;
        if failed > 0 {
            record_focus_failure(&mut focus_failed, focus, &fp);
        }
        match store_file(&db, &fp, &file, project_path) {
            Ok(()) => {
                synced += 1;
                symbols_written += file.symbols.len();
                edges_written += file.edges.len();
            }
            Err(e) => {
                if is_store_busy_error(&e) {
                    store_busy = true;
                }
                store_failures += 1;
                record_focus_failure(&mut focus_failed, focus, &fp);
            }
        }
    }
    let mut removed = 0u32;
    if prune_absent && deferred == 0 && !seen.is_empty() {
        let gone: Vec<&String> = known.keys().filter(|p| !seen.contains(*p)).collect();
        removed = gone.len() as u32;
        for batch in gone.chunks(200) {
            let list = batch
                .iter()
                .map(|p| sql_text(p))
                .collect::<Vec<_>>()
                .join(",");
            let script = format!("BEGIN; DELETE FROM {SYMBOLS_TABLE} WHERE path IN ({list}); DELETE FROM {IMPORTS_TABLE} WHERE path IN ({list}); DELETE FROM {FILES_TABLE} WHERE path IN ({list}); COMMIT;");
            if let Err(e) = libsql_wasm::exec(&db, &script) {
                if is_store_busy_error(&e) {
                    store_busy = true;
                }
                store_failures += 1;
                continue;
            }
            for path in batch {
                code_index::fv_delete(&edges_namespace(project_path), &edges_key(path));
            }
        }
    }
    let mut report = json!({
        "ok": true,
        "files_synced": synced,
        "files_unchanged": unchanged,
        "files_deferred": deferred,
        "files_removed": removed,
        "symbols_written": symbols_written,
        "edges_written": edges_written,
        "treesitter_failures": parse_failures,
        "files_unreadable": unreadable,
        "files_oversized": oversized,
        "store_failures": store_failures,
        "store_busy": store_busy,
        "files_without_symbol_support": unsupported,
        "complete": deferred == 0 && parse_failures == 0 && unreadable == 0 && oversized == 0 && store_failures == 0,
        "elapsed_ms": host_now_ms().saturating_sub(started_ms),
    });
    if !deferred_paths.is_empty() {
        let listed: Vec<String> = deferred_paths
            .iter()
            .take(DEFERRED_SET_NAMES_LIMIT)
            .cloned()
            .collect();
        report["deferred_set"] = json!(listed);
        report["deferred_set_truncated"] = json!(deferred_paths.len() > DEFERRED_SET_NAMES_LIMIT);
    }
    if let Some(path) = focus {
        let state = if focus_deferred {
            "deferred"
        } else if focus_failed {
            "failed"
        } else if seen.contains(path) {
            "covered"
        } else {
            "unlisted"
        };
        report["focus"] = json!({ "path": path, "state": state });
    }
    if synced > 0 || removed > 0 || deferred > 0 {
        crate::wasm_dispatch::emit_event("codeinsight_symbols_synced", report.clone());
    }
    report
}

pub(crate) fn sync_tree(cfg: &crate::ragconfig::RagConfig, project_path: Option<&str>) -> Value {
    sync_tree_with_budget(cfg, project_path, cfg.index.wall_budget_ms, None)
}

fn sync_tree_with_budget(
    cfg: &crate::ragconfig::RagConfig,
    project_path: Option<&str>,
    budget_ms: u64,
    focus: Option<&str>,
) -> Value {
    let started = host_now_ms();
    let root = project_path.filter(|p| !p.is_empty()).unwrap_or(".");
    let accessible_directory = crate::wasm_dispatch::host_stat_is_directory(root) == Some(true);
    if !accessible_directory && !crate::wasm_dispatch::host_allow_root(root) {
        return json!({
            "ok": false,
            "complete": false,
            "listing_complete": false,
            "root_granted": false,
            "error": format!("root '{root}' is not an existing project directory the host will grant access to"),
        });
    }
    let enumeration = match code_index::collect_files_checked_within(
        root,
        cfg.index.digest_max_files.saturating_add(1),
        &cfg.index,
    ) {
        Ok(enumeration) => enumeration,
        Err(error) => {
            return json!({ "ok": false, "complete": false, "listing_complete": false, "error": error })
        }
    };
    let listing_complete = enumeration.complete && enumeration.files.len() <= cfg.index.digest_max_files;
    let mut files = enumeration.files;
    files.truncate(cfg.index.digest_max_files);
    let mut report = sync_files(
        &files,
        project_path,
        started,
        budget_ms,
        cfg.index.max_file_bytes,
        listing_complete,
        focus,
    );
    let empty_listing_with_cached_files = files.is_empty()
        && !rows(
            &db_path(project_path),
            &format!("SELECT path FROM {FILES_TABLE} LIMIT 1"),
            &[],
        )
        .is_empty();
    report["files_listed"] = json!(files.len());
    report["listing_complete"] = json!(listing_complete);
    report["empty_listing_with_cached_files"] = json!(empty_listing_with_cached_files);
    report["complete"] = json!(
        report.get("complete").and_then(Value::as_bool) == Some(true)
            && listing_complete
            && !empty_listing_with_cached_files
    );
    report
}

pub(crate) fn clear(project_path: Option<&str>) {
    let db = db_path(project_path);
    for table in [SYMBOLS_TABLE, IMPORTS_TABLE, FILES_TABLE] {
        let _ = libsql_wasm::exec(&db, &format!("DELETE FROM {table}"));
    }
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
        let Some(parsed) = row
            .get("value")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        else {
            continue;
        };
        let path = string(&parsed, "path");
        for e in parsed
            .get("edges")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let Some(triple) = e.as_array() else { continue };
            let text = |i: usize| {
                triple
                    .get(i)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            out.push(Edge {
                path: path.clone(),
                caller: text(0),
                callee: text(1),
                line: number(triple.get(2)),
            });
        }
    }
    out
}

fn path_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn path_join(base: &str, rest: &str) -> String {
    let mut segments: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn first_existing(
    files: &HashSet<String>,
    candidates: impl IntoIterator<Item = String>,
) -> Option<String> {
    candidates.into_iter().find(|c| files.contains(c))
}

fn rust_module_dir(from: &str) -> String {
    let (dir, name) = from.rsplit_once('/').unwrap_or(("", from));
    if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_string()
    } else {
        path_join(dir, name.trim_end_matches(".rs"))
    }
}

fn rust_source_root(from: &str, files: &HashSet<String>) -> Option<String> {
    let mut dir = path_dir(from).to_string();
    loop {
        let has_crate_root = ["lib.rs", "main.rs"]
            .iter()
            .any(|root| files.contains(&path_join(&dir, root)));
        if has_crate_root || dir == "src" || dir.ends_with("/src") {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = path_dir(&dir).to_string();
    }
}

fn resolve_rust(from: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
    let mut segments: Vec<&str> = spec.split("::").collect();
    let base = match segments.first().copied()? {
        "crate" => {
            segments.remove(0);
            rust_source_root(from, files)?
        }
        "self" => {
            segments.remove(0);
            rust_module_dir(from)
        }
        "super" => {
            let mut base = rust_module_dir(from);
            while segments.first() == Some(&"super") {
                segments.remove(0);
                base = path_dir(&base).to_string();
            }
            base
        }
        _ => return None,
    };
    (1..=segments.len()).rev().find_map(|k| {
        let module = path_join(&base, &segments[..k].join("/"));
        first_existing(files, [format!("{module}.rs"), format!("{module}/mod.rs")])
    })
}

fn resolve_javascript(from: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
    if !spec.starts_with('.') {
        return None;
    }
    let base = path_join(path_dir(from), spec);
    let mut candidates = vec![base.clone()];
    for ext in ["js", "ts", "jsx", "tsx", "mjs", "cjs", "json"] {
        candidates.push(format!("{base}.{ext}"));
    }
    for ext in ["js", "ts", "tsx", "jsx", "mjs"] {
        candidates.push(format!("{base}/index.{ext}"));
    }
    if let Some(stem) = ["js", "mjs", "cjs", "jsx"]
        .iter()
        .find_map(|e| base.strip_suffix(&format!(".{e}")))
    {
        candidates.push(format!("{stem}.ts"));
        candidates.push(format!("{stem}.tsx"));
    }
    first_existing(files, candidates)
}

fn resolve_python(from: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
    let dots = spec.chars().take_while(|c| *c == '.').count();
    let module = spec[dots..].replace('.', "/");
    if dots > 0 {
        let mut base = path_dir(from).to_string();
        for _ in 1..dots {
            base = path_dir(&base).to_string();
        }
        let joined = path_join(&base, &module);
        let init = if module.is_empty() {
            format!("{base}/__init__.py")
        } else {
            format!("{joined}/__init__.py")
        };
        return first_existing(files, [format!("{joined}.py"), init]);
    }
    let wanted = [format!("{module}.py"), format!("{module}/__init__.py")];
    files
        .iter()
        .filter(|f| {
            wanted
                .iter()
                .any(|w| f.as_str() == w || f.ends_with(&format!("/{w}")))
        })
        .min()
        .cloned()
}

fn resolve_by_suffix(spec_path: &str, files: &HashSet<String>) -> Option<String> {
    let suffix = format!("/{spec_path}");
    let matches: Vec<&String> = files
        .iter()
        .filter(|f| f.as_str() == spec_path || f.ends_with(&suffix))
        .collect();
    if matches.len() == 1 {
        matches.first().map(|m| (*m).clone())
    } else {
        None
    }
}

fn resolve_import(from: &str, lang: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
    match lang {
        "rust" => resolve_rust(from, spec, files),
        "javascript" | "typescript" | "tsx" => resolve_javascript(from, spec, files),
        "python" => resolve_python(from, spec, files),
        "java" | "kotlin" => resolve_by_suffix(
            &format!(
                "{}.{}",
                spec.replace('.', "/"),
                if lang == "java" { "java" } else { "kt" }
            ),
            files,
        ),
        "c" | "cpp" => first_existing(files, [path_join(path_dir(from), spec)])
            .or_else(|| resolve_by_suffix(spec, files)),
        _ => None,
    }
}

struct ImportGraph {
    forward: BTreeMap<String, BTreeSet<String>>,
    reverse: BTreeMap<String, BTreeSet<String>>,
    external: BTreeMap<String, BTreeSet<String>>,
    total_imports: usize,
    resolved_imports: usize,
}

fn load_import_graph(db: &str) -> ImportGraph {
    let langs: HashMap<String, String> =
        rows(db, &format!("SELECT path, lang FROM {FILES_TABLE}"), &[])
            .iter()
            .map(|r| (string(r, "path"), string(r, "lang")))
            .collect();
    let files: HashSet<String> = langs.keys().cloned().collect();
    let mut graph = ImportGraph {
        forward: BTreeMap::new(),
        reverse: BTreeMap::new(),
        external: BTreeMap::new(),
        total_imports: 0,
        resolved_imports: 0,
    };
    for r in rows(db, &format!("SELECT path, spec FROM {IMPORTS_TABLE}"), &[]) {
        let (path, spec) = (string(&r, "path"), string(&r, "spec"));
        graph.total_imports += 1;
        let lang = langs.get(&path).map(String::as_str).unwrap_or("");
        match resolve_import(&path, lang, &spec, &files) {
            Some(target) if target != path => {
                graph.resolved_imports += 1;
                graph
                    .reverse
                    .entry(target.clone())
                    .or_default()
                    .insert(path.clone());
                graph.forward.entry(path).or_default().insert(target);
            }
            Some(_) => graph.resolved_imports += 1,
            None => {
                graph.external.entry(path).or_default().insert(spec);
            }
        }
    }
    graph
}

fn strongly_connected_components(forward: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for (from, targets) in forward {
        names.insert(from);
        names.extend(targets.iter().map(String::as_str));
    }
    let names: Vec<&str> = names.into_iter().collect();
    let index_of: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let adjacency: Vec<Vec<usize>> = names
        .iter()
        .map(|n| {
            forward
                .get(*n)
                .map(|t| {
                    t.iter()
                        .filter_map(|x| index_of.get(x.as_str()).copied())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    let count = names.len();
    let (mut index, mut lowlink) = (vec![usize::MAX; count], vec![0usize; count]);
    let (mut on_stack, mut stack, mut next_index) =
        (vec![false; count], Vec::<usize>::new(), 0usize);
    let mut components = Vec::new();
    for root in 0..count {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(node, child)) = work.last() {
            if child == 0 && index[node] == usize::MAX {
                index[node] = next_index;
                lowlink[node] = next_index;
                next_index += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            if child < adjacency[node].len() {
                if let Some(top) = work.last_mut() {
                    top.1 += 1;
                }
                let next = adjacency[node][child];
                if index[next] == usize::MAX {
                    work.push((next, 0));
                } else if on_stack[next] {
                    lowlink[node] = lowlink[node].min(index[next]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[node]);
                }
                if lowlink[node] == index[node] {
                    let mut component = Vec::new();
                    while let Some(member) = stack.pop() {
                        on_stack[member] = false;
                        component.push(names[member].to_string());
                        if member == node {
                            break;
                        }
                    }
                    if component.len() > 1 {
                        component.sort();
                        components.push(component);
                    }
                }
            }
        }
    }
    components.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    components
}

fn definitions(db: &str, name: &str, limit: usize) -> Vec<Value> {
    let limit_s = limit.to_string();
    rows(
        db,
        &format!("SELECT path, kind, line_start, line_end, signature, is_test FROM {SYMBOLS_TABLE} WHERE name=?1 ORDER BY path, line_start LIMIT ?2"),
        &[name, &limit_s],
    )
}

fn location(row: &Value) -> String {
    format!(
        "{}:{}-{}",
        string(row, "path"),
        number(row.get("line_start")),
        number(row.get("line_end"))
    )
}

fn definition_locations(db: &str, name: &str) -> Vec<String> {
    definitions(db, name, 6)
        .iter()
        .map(|d| format!("{} {}", location(d), string(d, "kind")))
        .collect()
}

fn unique_definition(db: &str, name: &str) -> Option<String> {
    let defs = definitions(db, name, 2);
    if defs.len() == 1 {
        defs.first().map(location)
    } else {
        None
    }
}

fn is_test_name(db: &str, name: &str) -> bool {
    definitions(db, name, 4)
        .iter()
        .any(|d| number(d.get("is_test")) == 1)
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
    let files = match libsql_wasm::query_params(
        &db,
        &format!("SELECT COUNT(*) AS c FROM {FILES_TABLE}"),
        &[],
    ) {
        Ok(r) => r
            .as_array()
            .and_then(|a| a.first())
            .map(|r| number(r.get("c")))
            .unwrap_or(0),
        Err(e) => {
            if stored_digest.is_none() {
                return Value::Null;
            }
            let lower = e.to_ascii_lowercase();
            let reason = if lower.contains("unknown plugin") || lower.contains("unknown_plugin") {
                "libsql_plugin_unavailable"
            } else if lower.contains("no such table") {
                "symbols_not_indexed_yet"
            } else {
                "libsql_query_failed"
            };
            return json!({ "codeinsight_available": false, "codeinsight_unavailable_reason": reason, "digest": stored_digest });
        }
    };
    if files == 0 && stored_digest.is_none() {
        return Value::Null;
    }
    let symbol_count = single_count(
        &db,
        &format!("SELECT COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind NOT IN {DOC_KINDS_SQL}"),
    );
    let doc_sections = single_count(
        &db,
        &format!("SELECT COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind IN {DOC_KINDS_SQL}"),
    );
    json!({
        "codeinsight_available": true,
        "file_count": files,
        "symbol_count": symbol_count,
        "doc_sections": doc_sections,
        "by_kind": rows(&db, &format!("SELECT kind, COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE kind NOT IN {DOC_KINDS_SQL} GROUP BY kind ORDER BY c DESC LIMIT 8"), &[]),
        "by_language": by_language(&db, 6),
        "largest_files": rows(&db, &format!("SELECT path, symbols AS c FROM {FILES_TABLE} ORDER BY symbols DESC LIMIT 5"), &[]),
        "coverage": coverage(&db),
        "semantic_index_digest": stored_digest,
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
    for r in rows(
        db,
        &format!("SELECT path, loc, symbols FROM {FILES_TABLE}"),
        &[],
    ) {
        let path = string(&r, "path");
        let segments: Vec<&str> = path.split('/').collect();
        let area = if segments.len() > 2 {
            segments[..2].join("/")
        } else if segments.len() == 2 {
            segments[0].to_string()
        } else {
            ".".to_string()
        };
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
    let mut out: Vec<(String, usize, usize)> = callers_by_callee
        .into_iter()
        .map(|(name, (calls, distinct))| (name.to_string(), calls, distinct.len()))
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    out
}

fn fan_in_rows(db: &str, edges: &[Edge], limit: usize) -> Vec<Value> {
    let unique = uniquely_defined_names(db);
    callee_fan_in(edges)
        .into_iter()
        .filter(|(name, _, _)| name != code_index::MODULE_LEVEL_CALLER && !is_common_std_name(name) && unique.contains(name))
        .take(limit)
        .map(|(name, calls, distinct_callers)| {
            json!({ "symbol": name, "calls": calls, "distinct_callers": distinct_callers, "defined_at": unique_definition(db, &name) })
        })
        .collect()
}

fn fan_out_rows(edges: &[Edge], limit: usize) -> Vec<Value> {
    let mut callees_by_caller: HashMap<(&str, &str), HashSet<&str>> = HashMap::new();
    for e in edges {
        callees_by_caller
            .entry((e.path.as_str(), e.caller.as_str()))
            .or_default()
            .insert(e.callee.as_str());
    }
    let mut out: Vec<((&str, &str), usize)> = callees_by_caller
        .into_iter()
        .map(|(k, v)| (k, v.len()))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.into_iter()
        .take(limit)
        .map(|((path, caller), n)| json!({ "symbol": caller, "path": path, "distinct_callees": n }))
        .collect()
}

fn function_symbol_rows(db: &str, lang: &str, path_prefix: &str) -> Vec<Value> {
    rows(
        db,
        &format!(
            "SELECT path, kind, name, line_start, line_end, exported FROM {SYMBOLS_TABLE} WHERE (kind LIKE '%function%' OR kind LIKE '%method%') AND name != '' AND COALESCE(is_test,0)=0 AND (?1='' OR path IN (SELECT path FROM {FILES_TABLE} WHERE lang=?1)) AND path LIKE ?2 ESCAPE '\\' ORDER BY path, line_start"
        ),
        &[lang, &format!("{}%", like_escaped(path_prefix))],
    )
    .into_iter()
    .filter(|r| is_identifier_like(&string(r, "name")))
    .collect()
}

fn no_direct_callers(
    db: &str,
    edges: &[Edge],
    limit: usize,
    lang: &str,
    path_prefix: &str,
) -> (usize, Vec<String>) {
    let called: HashSet<&str> = edges.iter().map(|e| e.callee.as_str()).collect();
    let mut orphans: Vec<Value> = function_symbol_rows(db, lang, path_prefix)
        .into_iter()
        .filter(|r| {
            let name = string(r, "name");
            name != "main" && !called.contains(name.as_str())
        })
        .collect();
    orphans.sort_by_key(|r| number(r.get("exported")));
    let sample = orphans
        .iter()
        .take(limit)
        .map(|r| {
            let api = if number(r.get("exported")) == 1 {
                " (exported)"
            } else {
                ""
            };
            format!("{} {}{api}", location(r), string(r, "name"))
        })
        .collect();
    (orphans.len(), sample)
}

fn symbol_span_line(r: &Value) -> String {
    format!(
        "{} lines {} {} {}",
        number(r.get("span")),
        location(r),
        string(r, "kind"),
        string(r, "name")
    )
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
    .map(symbol_span_line)
    .collect()
}

fn complexity_line(r: &Value) -> String {
    format!(
        "cx {} nest {} params {} sloc {} {} {}",
        number(r.get("cx")),
        number(r.get("nesting")),
        number(r.get("params")),
        number(r.get("sloc")),
        location(r),
        string(r, "name")
    )
}

fn most_complex(
    db: &str,
    sort: &str,
    limit: usize,
    include_tests: bool,
    path_prefix: &str,
) -> Vec<String> {
    let order = match sort {
        "nesting" => "nesting",
        "params" => "params",
        "sloc" => "sloc",
        _ => "cx",
    };
    let limit_s = limit.to_string();
    rows(
        db,
        &format!(
            "SELECT path, name, line_start, line_end, cx, nesting, params, sloc FROM {SYMBOLS_TABLE} WHERE cx IS NOT NULL AND cx != '' AND (?2='1' OR COALESCE(is_test,0)=0) AND path LIKE ?3 ESCAPE '\\' ORDER BY {order} DESC, cx DESC LIMIT ?1"
        ),
        &[&limit_s, if include_tests { "1" } else { "0" }, &format!("{}%", like_escaped(path_prefix))],
    )
    .iter()
    .map(complexity_line)
    .collect()
}

fn complexity(db: &str, project_path: Option<&str>, body: &Value, limit: usize) -> Value {
    let sort = text_field(body, &["sort"]).unwrap_or("cx");
    let include_tests = body
        .get("include_tests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path_prefix = normalized_path(text_field(body, &["path", "path_prefix"]).unwrap_or(""));
    let thresholds = json!({ "cx_over": 15, "nesting_over": 5, "params_over": 5, "sloc_over": 80 });
    let flagged = single_count(
        db,
        &format!("SELECT COUNT(*) AS c FROM {SYMBOLS_TABLE} WHERE COALESCE(is_test,0)=0 AND (CAST(NULLIF(cx,'') AS INTEGER)>15 OR CAST(NULLIF(nesting,'') AS INTEGER)>5 OR CAST(NULLIF(params,'') AS INTEGER)>5 OR CAST(NULLIF(sloc,'') AS INTEGER)>80)"),
    );
    if sort != "risk" {
        return json!({
            "sort": sort,
            "meaning": "cx = 1 + decision points (if/loop/match arm/catch/ternary); nest = deepest control nesting; params excludes self/this",
            "flagged_over_thresholds": flagged,
            "thresholds": thresholds,
            "functions": most_complex(db, sort, limit, include_tests, &path_prefix),
        });
    }
    let edges = load_edges(project_path);
    let unique = uniquely_defined_names(db);
    let fan_in: HashMap<String, usize> = callee_fan_in(&edges)
        .into_iter()
        .filter(|(name, _, _)| !is_common_std_name(name) && unique.contains(name))
        .map(|(name, _, distinct)| (name, distinct))
        .collect();
    let pool = rows(
        db,
        &format!("SELECT path, name, line_start, line_end, cx, nesting, params, sloc FROM {SYMBOLS_TABLE} WHERE cx IS NOT NULL AND cx != '' AND COALESCE(is_test,0)=0 AND path LIKE ?1 ESCAPE '\\' ORDER BY CAST(cx AS INTEGER) DESC LIMIT {RISK_CANDIDATE_POOL}"),
        &[&format!("{}%", like_escaped(&path_prefix))],
    );
    let mut scored: Vec<(u64, String)> = pool
        .iter()
        .map(|r| {
            let callers = fan_in.get(&string(r, "name")).copied().unwrap_or(0) as u64;
            let score = number(r.get("cx")) * (1 + callers);
            (
                score,
                format!(
                    "risk {score} (cx x {} callers) {}",
                    1 + callers,
                    complexity_line(r)
                ),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    json!({
        "sort": "risk",
        "meaning": "cx multiplied by (1 + distinct callers): complex code many others depend on",
        "flagged_over_thresholds": flagged,
        "thresholds": thresholds,
        "functions": scored.into_iter().take(limit).map(|(_, line)| line).collect::<Vec<_>>(),
    })
}

fn duplicate_groups(db: &str, limit: usize) -> (usize, Vec<String>) {
    let groups = rows(
        db,
        &format!(
            "SELECT shape_hash, COUNT(*) AS c, MAX(CAST(node_count AS INTEGER)) AS nodes FROM {SYMBOLS_TABLE} WHERE shape_hash IS NOT NULL AND shape_hash != '' AND COALESCE(is_test,0)=0 AND CAST(sloc AS INTEGER) >= 6 AND name GLOB '[A-Za-z_$]*' GROUP BY shape_hash HAVING COUNT(*) > 1 ORDER BY nodes * COUNT(*) DESC"
        ),
        &[],
    );
    let total = groups.len();
    let lines = groups
        .iter()
        .take(limit)
        .map(|g| {
            let hash = string(g, "shape_hash");
            let members: Vec<String> = rows(
                db,
                &format!("SELECT path, name, line_start, line_end FROM {SYMBOLS_TABLE} WHERE shape_hash=?1 ORDER BY path, line_start LIMIT 8"),
                &[&hash],
            )
            .iter()
            .map(|r| format!("{} {}", location(r), string(r, "name")))
            .collect();
            format!("{} copies, ~{} nodes each: {}", number(g.get("c")), number(g.get("nodes")), members.join(" | "))
        })
        .collect();
    (total, lines)
}

fn duplicates(db: &str, limit: usize) -> Value {
    let (total, groups) = duplicate_groups(db, limit);
    json!({
        "meaning": "functions whose syntax-tree shape is identical once identifiers, comments and literal text are ignored, at least 40 nodes each; candidates to extract into one function",
        "total_groups": total,
        "truncated": total > limit,
        "groups": groups,
    })
}

fn import_lines(db: &str, path: &str) -> Vec<String> {
    let langs = rows(
        db,
        &format!("SELECT lang FROM {FILES_TABLE} WHERE path=?1"),
        &[path],
    );
    let lang = langs.first().map(|r| string(r, "lang")).unwrap_or_default();
    let files: HashSet<String> = rows(db, &format!("SELECT path FROM {FILES_TABLE}"), &[])
        .iter()
        .map(|r| string(r, "path"))
        .collect();
    rows(
        db,
        &format!("SELECT line, spec FROM {IMPORTS_TABLE} WHERE path=?1 ORDER BY line"),
        &[path],
    )
    .iter()
    .map(|r| {
        let spec = string(r, "spec");
        match resolve_import(path, &lang, &spec, &files) {
            Some(target) => format!("{} {spec} -> {target}", number(r.get("line"))),
            None => format!("{} {spec} (external or unresolved)", number(r.get("line"))),
        }
    })
    .collect()
}

fn normalized_path(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn imports(db: &str, path: &str) -> Result<Value, String> {
    let path = normalized_path(path);
    let known = single_count(
        db,
        &format!(
            "SELECT COUNT(*) AS c FROM {FILES_TABLE} WHERE path='{}'",
            path.replace('\'', "''")
        ),
    );
    if known == 0 {
        return Err(format!("path not indexed: {path}"));
    }
    Ok(json!({ "path": path, "imports": import_lines(db, &path) }))
}

fn importers(db: &str, path: &str, limit: usize) -> Value {
    let path = normalized_path(path);
    let graph = load_import_graph(db);
    let found: Vec<&String> = graph
        .reverse
        .get(&path)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    json!({
        "path": path,
        "total": found.len(),
        "truncated": found.len() > limit,
        "importers": found.into_iter().take(limit).collect::<Vec<_>>(),
    })
}

const CYCLE_MEMBERS_SHOWN: usize = 12;

fn cycle_summary(component: &[String]) -> String {
    let shown = component
        .iter()
        .take(CYCLE_MEMBERS_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(" <-> ");
    let more = component.len().saturating_sub(CYCLE_MEMBERS_SHOWN);
    if more == 0 {
        format!("{} files: {shown}", component.len())
    } else {
        format!("{} files: {shown} (+{more} more)", component.len())
    }
}

fn cycles(db: &str, limit: usize) -> Value {
    let graph = load_import_graph(db);
    let components = strongly_connected_components(&graph.forward);
    json!({
        "meaning": "each entry is a strongly connected set of files that import each other, directly or through the others",
        "import_edges_resolved": graph.forward.values().map(BTreeSet::len).sum::<usize>(),
        "cycle_count": components.len(),
        "truncated": components.len() > limit,
        "cycles": components.iter().take(limit).map(|c| cycle_summary(c)).collect::<Vec<_>>(),
    })
}

fn coupling_rows(graph: &ImportGraph, db: &str) -> Vec<(String, usize, usize, bool)> {
    rows(db, &format!("SELECT path, is_test FROM {FILES_TABLE}"), &[])
        .iter()
        .map(|r| {
            let path = string(r, "path");
            let fan_in = graph.reverse.get(&path).map(BTreeSet::len).unwrap_or(0);
            let fan_out = graph.forward.get(&path).map(BTreeSet::len).unwrap_or(0);
            (path, fan_in, fan_out, number(r.get("is_test")) == 1)
        })
        .collect()
}

fn coupling(db: &str, limit: usize, path_prefix: &str) -> Value {
    let graph = load_import_graph(db);
    let all: Vec<(String, usize, usize, bool)> = coupling_rows(&graph, db)
        .into_iter()
        .filter(|r| r.0.starts_with(path_prefix))
        .collect();
    let line = |(path, fan_in, fan_out, _): &(String, usize, usize, bool)| {
        let instability = if fan_in + fan_out == 0 {
            0.0
        } else {
            (*fan_out as f64 / (*fan_in + *fan_out) as f64 * 100.0).round() / 100.0
        };
        format!("in {fan_in} out {fan_out} instability {instability} {path}")
    };
    let mut by_total: Vec<&(String, usize, usize, bool)> = all.iter().filter(|r| !r.3).collect();
    by_total.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)).then(a.0.cmp(&b.0)));
    let mut hubs: Vec<&(String, usize, usize, bool)> =
        all.iter().filter(|r| !r.3 && r.1 > 0).collect();
    hubs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut entry_files: Vec<&(String, usize, usize, bool)> =
        all.iter().filter(|r| !r.3 && r.1 == 0 && r.2 > 0).collect();
    entry_files.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    json!({
        "meaning": "in = files importing this file, out = indexed files it imports, instability = out/(in+out); only imports resolved to indexed files count",
        "import_specs": graph.total_imports,
        "resolved_to_indexed_files": graph.resolved_imports,
        "most_coupled": by_total.iter().take(limit).map(|r| line(*r)).collect::<Vec<_>>(),
        "hubs": hubs.iter().take(limit).map(|r| line(*r)).collect::<Vec<_>>(),
        "entry_files": entry_files.iter().take(limit).map(|r| line(*r)).collect::<Vec<_>>(),
    })
}

fn overview(project_path: Option<&str>, limit: usize) -> Value {
    let db = db_path(project_path);
    let edges = load_edges(project_path);
    let (orphan_total, orphan_sample) = no_direct_callers(&db, &edges, limit, "", "");
    let graph = load_import_graph(&db);
    let cycle_sets = strongly_connected_components(&graph.forward);
    let (duplicate_total, duplicate_sample) = duplicate_groups(&db, 3);
    let mut out = lean_overview(code_index::stored_digest_at(project_path));
    if let Some(map) = out.as_object_mut() {
        map.insert("by_language".into(), json!(by_language(&db, 12)));
        map.insert("by_area".into(), json!(by_area(&db, limit.min(15))));
        map.insert(
            "biggest_symbols".into(),
            json!(biggest_symbols(&db, limit.min(10))),
        );
        map.insert(
            "most_complex".into(),
            json!(most_complex(&db, "cx", limit.min(8), false, "")),
        );
        map.insert(
            "most_called".into(),
            json!(fan_in_rows(&db, &edges, limit.min(10))),
        );
        map.insert(
            "widest_callers".into(),
            json!(fan_out_rows(&edges, limit.min(10))),
        );
        map.insert("call_edges".into(), json!(edges.len()));
        map.insert(
            "functions_without_direct_callers".into(),
            json!({ "total": orphan_total, "sample": orphan_sample }),
        );
        map.insert(
            "test_files".into(),
            json!(single_count(
                &db,
                &format!("SELECT COUNT(*) AS c FROM {FILES_TABLE} WHERE is_test=1")
            )),
        );
        map.insert("imports".into(), json!({
            "specs": graph.total_imports,
            "resolved_to_indexed_files": graph.resolved_imports,
            "cycles": cycle_sets.len(),
            "largest_cycle": cycle_sets.first().map(|c| format!("{} files: {}", c.len(), c.iter().take(6).cloned().collect::<Vec<_>>().join(", "))),
        }));
        map.insert(
            "duplicate_function_groups".into(),
            json!({ "total": duplicate_total, "top": duplicate_sample }),
        );
    }
    out
}

fn status(project_path: Option<&str>) -> Value {
    let db = db_path(project_path);
    let files = rows(&db, &format!("SELECT lang, COUNT(*) AS files, SUM(parse_failed) AS parse_failed, SUM(CASE WHEN symbols=0 THEN 1 ELSE 0 END) AS without_symbols FROM {FILES_TABLE} GROUP BY lang ORDER BY files DESC"), &[]);
    let graph = load_import_graph(&db);
    json!({
        "schema_version": SCHEMA_VERSION,
        "coverage": coverage(&db),
        "by_language_health": files,
        "imports": { "specs": graph.total_imports, "resolved_to_indexed_files": graph.resolved_imports },
        "stored_digest": code_index::stored_digest_at(project_path),
        "hint": "action=sync completes the symbol index without embeddings; codeinsight_index continues the semantic (embedding) index",
    })
}

fn outline_partial_reason(focus_state: Option<&str>) -> Option<&'static str> {
    match focus_state {
        Some("covered") => None,
        Some("deferred") => Some(DEFERRED_PARTIAL_REASON),
        Some("failed") => Some(FAILED_PARTIAL_REASON),
        Some("unlisted") => Some(UNLISTED_PARTIAL_REASON),
        _ => Some(REFRESH_NOT_RUN_PARTIAL_REASON),
    }
}

fn outline(db: &str, path: &str, refresh: &Value) -> Result<Value, String> {
    let normalized = normalized_path(path);
    let refresh_complete = refresh.get("complete").and_then(Value::as_bool) == Some(true);
    let focus_state = refresh.pointer("/focus/state").and_then(Value::as_str);
    let file = rows(
        db,
        &format!("SELECT lang, loc, symbols FROM {FILES_TABLE} WHERE path=?1"),
        &[&normalized],
    );
    let Some(file) = file.first() else {
        if !refresh_complete && matches!(focus_state, Some("deferred") | Some("failed") | None) {
            let reason = if focus_state == Some("deferred") {
                DEFERRED_NEVER_INDEXED_REASON
            } else {
                UNINDEXED_IN_PASS_REASON
            };
            return Ok(json!({
                "path": normalized,
                "lang": Value::Null,
                "loc": Value::Null,
                "symbol_count": 0,
                "outline": Vec::<String>::new(),
                "partial": true,
                "current": false,
                "partial_reason": reason,
            }));
        }
        let like = format!("%{normalized}%");
        let near: Vec<String> = rows(
            db,
            &format!("SELECT path FROM {FILES_TABLE} WHERE path LIKE ?1 ORDER BY path LIMIT 8"),
            &[&like],
        )
        .iter()
        .map(|r| string(r, "path"))
        .collect();
        return Err(format!(
            "path not indexed: {normalized}; near matches: {}",
            near.join(", ")
        ));
    };
    let symbols = rows(db, &format!("SELECT kind, name, line_start, line_end, signature, cx FROM {SYMBOLS_TABLE} WHERE path=?1 ORDER BY line_start, line_end DESC"), &[&normalized]);
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
            let cx = string(s, "cx");
            let cx_tag = if cx.is_empty() {
                String::new()
            } else {
                format!(" [cx {cx}]")
            };
            format!(
                "{indent}{ls}-{le} {} {}{cx_tag}",
                string(s, "kind"),
                string(s, "signature")
            )
        })
        .collect();
    let mut result = json!({
        "path": normalized,
        "lang": string(file, "lang"),
        "loc": number(file.get("loc")),
        "symbol_count": symbols.len(),
        "outline": lines,
    });
    if !refresh_complete {
        let reason = outline_partial_reason(focus_state);
        result["partial"] = json!(reason.is_some());
        result["current"] = json!(reason.is_none());
        if let Some(reason) = reason {
            result["partial_reason"] = json!(reason);
        }
    }
    Ok(result)
}

fn like_escaped(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn find(
    db: &str,
    name: &str,
    kind: Option<&str>,
    path_prefix: Option<&str>,
    limit: usize,
) -> Value {
    let escaped_name = like_escaped(name);
    let like = format!("%{escaped_name}%");
    let prefix_like = format!(
        "{}%",
        like_escaped(path_prefix.unwrap_or("").trim_start_matches("./"))
    );
    let limit_s = (limit + 1).to_string();
    let kind_filter = kind.unwrap_or("");
    let found = rows(
        db,
        &format!(
            "SELECT path, kind, name, line_start, line_end, signature, CASE WHEN name=?1 THEN 0 WHEN LOWER(name)=LOWER(?1) THEN 1 WHEN LOWER(name) LIKE LOWER(?6)||'%' ESCAPE '\\' THEN 2 ELSE 3 END AS rank FROM {SYMBOLS_TABLE} WHERE (name LIKE ?2 ESCAPE '\\' OR signature LIKE ?2 ESCAPE '\\') AND (?3='' OR kind=?3) AND path LIKE ?4 ESCAPE '\\' ORDER BY rank, LENGTH(name), path, line_start LIMIT ?5"
        ),
        &[name, &like, kind_filter, &prefix_like, &limit_s, &escaped_name],
    );
    let truncated = found.len() > limit;
    let matches: Vec<String> = found
        .iter()
        .take(limit)
        .map(|r| {
            let container = container_of(
                db,
                &string(r, "path"),
                number(r.get("line_start")),
                number(r.get("line_end")),
            );
            let inside = container.map(|c| format!(" in {c}")).unwrap_or_default();
            format!(
                "{} {}{} :: {}",
                location(r),
                string(r, "kind"),
                inside,
                string(r, "signature")
            )
        })
        .collect();
    json!({ "query": name, "matches": matches, "truncated": truncated })
}

fn edge_line(e: &Edge) -> String {
    format!("{}:{} {} -> {}", e.path, e.line, e.caller, e.callee)
}

fn callers_or_callees(
    db: &str,
    project_path: Option<&str>,
    symbol: &str,
    want_callers: bool,
    limit: usize,
) -> Value {
    let edges = load_edges(project_path);
    let matched: Vec<&Edge> = edges
        .iter()
        .filter(|e| {
            if want_callers {
                e.callee == symbol
            } else {
                e.caller == symbol
            }
        })
        .collect();
    let total = matched.len();
    let mut out = json!({
        "symbol": symbol,
        "edge_resolution": "unqualified-name",
        "ambiguous_definitions": definitions(db, symbol, 2).len() > 1,
        "defined_at": definition_locations(db, symbol),
        "total": total,
        "truncated": total > limit,
    });
    if want_callers {
        let distinct: HashSet<(&str, &str)> = matched
            .iter()
            .map(|e| (e.path.as_str(), e.caller.as_str()))
            .collect();
        out["distinct_callers"] = json!(distinct.len());
        out["edges"] = json!(matched
            .iter()
            .take(limit)
            .map(|e| edge_line(e))
            .collect::<Vec<_>>());
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
            .map(|(name, n)| {
                if is_common_std_name(name) {
                    return format!("{name} x{n} (std-style name, definition not resolved)");
                }
                match unique_definition(db, name) {
                    Some(at) => format!("{name} x{n} ({at})"),
                    None => format!("{name} x{n}"),
                }
            })
            .collect::<Vec<_>>());
    }
    if total == 0 {
        out["note"] = json!("no call edges recorded for this symbol name; edges are keyed by simple callee name, so check spelling with action=find and confirm action=status shows the defining file synced");
    }
    out
}

fn reach(
    db: &str,
    edges: &[Edge],
    symbol: &str,
    max_depth: usize,
    upstream: bool,
    through_ambiguous: bool,
) -> Vec<(String, usize)> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        let (from, to) = if upstream {
            (e.callee.as_str(), e.caller.as_str())
        } else {
            (e.caller.as_str(), e.callee.as_str())
        };
        adjacency.entry(from).or_default().push(to);
    }
    let mut visited: HashMap<&str, usize> = HashMap::from([(symbol, 0)]);
    let mut ambiguity: HashMap<String, bool> = HashMap::new();
    let mut is_ambiguous = |name: &str| -> bool {
        *ambiguity
            .entry(name.to_string())
            .or_insert_with(|| is_common_std_name(name) || definitions(db, name, 2).len() > 1)
    };
    let mut frontier = vec![symbol];
    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for node in &frontier {
            if *node != symbol && !through_ambiguous && is_ambiguous(node) {
                continue;
            }
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
    let mut reached: Vec<(String, usize)> = visited
        .into_iter()
        .filter(|(name, _)| *name != symbol)
        .map(|(n, d)| (n.to_string(), d))
        .collect();
    reached.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    reached
}

fn impact(
    db: &str,
    project_path: Option<&str>,
    symbol: &str,
    max_depth: usize,
    upstream: bool,
    limit: usize,
    through_ambiguous: bool,
) -> Value {
    let depth_cap = max_depth.clamp(1, 10);
    let edges = load_edges(project_path);
    let reached = reach(db, &edges, symbol, depth_cap, upstream, through_ambiguous);
    let total = reached.len();
    let tests_reached = reached
        .iter()
        .filter(|(name, _)| is_test_name(db, name))
        .count();
    let lines: Vec<String> = reached
        .iter()
        .take(limit)
        .map(|(name, depth)| {
            if is_common_std_name(name) {
                return format!("d{depth} {name} (std-style name, not expanded)");
            }
            match unique_definition(db, name) {
                Some(at) => format!("d{depth} {name} ({at})"),
                None if definitions(db, name, 2).len() > 1 => {
                    format!("d{depth} {name} (ambiguous: several definitions, not expanded)")
                }
                None => format!("d{depth} {name}"),
            }
        })
        .collect();
    json!({
        "symbol": symbol,
        "direction": if upstream { "callers: what breaks if this changes" } else { "callees: what this depends on" },
        "edge_resolution": "unqualified-name",
        "ambiguous_definitions": definitions(db, symbol, 2).len() > 1,
        "defined_at": definition_locations(db, symbol),
        "max_depth": depth_cap,
        "total": total,
        "tests_reached": tests_reached,
        "truncated": total > limit,
        "reached": lines,
    })
}

fn tests_for(db: &str, project_path: Option<&str>, symbol: &str, limit: usize) -> Value {
    let edges = load_edges(project_path);
    let reached = reach(db, &edges, symbol, TEST_CALLER_DEPTH, true, false);
    let mut found: Vec<String> = Vec::new();
    for (name, depth) in &reached {
        for d in definitions(db, name, 6)
            .iter()
            .filter(|d| number(d.get("is_test")) == 1)
        {
            found.push(format!("d{depth} {} {name}", location(d)));
        }
    }
    let mut names: HashSet<&str> = reached.iter().map(|(n, _)| n.as_str()).collect();
    names.insert(symbol);
    let test_file_calls: Vec<String> = edges
        .iter()
        .filter(|e| names.contains(e.callee.as_str()) && is_test_path(&e.path))
        .map(|e| format!("{}:{} {} calls {}", e.path, e.line, e.caller, e.callee))
        .collect();
    let direct_files: Vec<String> = rows(
        db,
        &format!("SELECT DISTINCT path FROM {SYMBOLS_TABLE} WHERE name=?1 AND COALESCE(is_test,0)=1 ORDER BY path LIMIT 10"),
        &[symbol],
    )
    .iter()
    .map(|r| string(r, "path"))
    .collect();
    json!({
        "symbol": symbol,
        "test_callers_within_depth": TEST_CALLER_DEPTH,
        "total": found.len() + test_file_calls.len(),
        "truncated": found.len() > limit || test_file_calls.len() > limit,
        "tests": found.into_iter().take(limit).collect::<Vec<_>>(),
        "calls_from_test_files": test_file_calls.iter().take(limit).collect::<Vec<_>>(),
        "defined_in_test_files": direct_files,
        "note": "a symbol with no tests listed is not exercised by any indexed test through direct or ambiguous-free call chains",
    })
}

fn hotspots(db: &str, project_path: Option<&str>, limit: usize) -> Value {
    let edges = load_edges(project_path);
    let limit_s = limit.to_string();
    json!({
        "most_called": fan_in_rows(db, &edges, limit),
        "widest_callers": fan_out_rows(&edges, limit),
        "most_complex": most_complex(db, "cx", limit, false, ""),
        "biggest_symbols": biggest_symbols(db, limit),
        "densest_files": rows(db, &format!("SELECT path, symbols, loc FROM {FILES_TABLE} ORDER BY symbols DESC LIMIT ?1"), &[&limit_s]),
        "longest_files": rows(db, &format!("SELECT path, loc, symbols FROM {FILES_TABLE} ORDER BY loc DESC LIMIT ?1"), &[&limit_s]),
    })
}

fn orphans(
    db: &str,
    project_path: Option<&str>,
    limit: usize,
    lang: &str,
    path_prefix: &str,
) -> Value {
    let edges = load_edges(project_path);
    let (total, sample) = no_direct_callers(db, &edges, limit, lang, path_prefix);
    json!({
        "meaning": "non-test function or method symbols whose name is never the target of a recorded call; private ones first; functions used only as values (callbacks, match arms, exports, trait impls) also appear here",
        "total": total,
        "truncated": total > limit,
        "functions": sample,
    })
}

fn text_field<'a>(body: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
}

pub(crate) fn handle(body: &Value) -> Result<Value, String> {
    let action = text_field(body, &["action", "mode"]).unwrap_or("overview");
    let project_root = text_field(body, &["root", "projectPath"]).map(crate::pkfs::anchor);
    let project_path = project_root.as_deref();
    if matches!(action, "callers" | "callees" | "impact" | "tests") {
        let unsupported: Vec<&str> = [
            "path",
            "file",
            "path_prefix",
            "paths",
            "glob",
            "path_glob",
            "line",
            "line_start",
            "line_end",
        ]
        .into_iter()
        .filter(|field| body.get(*field).is_some())
        .collect();
        if !unsupported.is_empty() {
            return Err(format!("{action} does not support scope fields {}; call edges are keyed by unqualified names, not resolved definitions. Use outline/find for path-scoped definitions and codesearch for exact call sites", unsupported.join(", ")));
        }
    }
    let cfg = crate::ragconfig::RagConfig::resolved();
    if let Some(path) = text_field(body, &["path", "file", "path_prefix"]) {
        if path
            .rsplit_once('.')
            .map(|(_, extension)| extension.eq_ignore_ascii_case("json"))
            == Some(true)
        {
            return Err(format!(
                "JSON has no structural symbol support: {path}; use codesearch for its contents"
            ));
        }
    }
    if action == "sync" {
        return Ok(sync_tree(&cfg, project_path));
    }
    let focus = if action == "outline" {
        text_field(body, &["path", "file"]).map(normalized_path)
    } else {
        None
    };
    let refresh = sync_tree_with_budget(
        &cfg,
        project_path,
        cfg.index.incremental_topup_wall_budget_ms,
        focus.as_deref(),
    );
    let refresh_complete = refresh.get("complete").and_then(Value::as_bool) == Some(true);
    let answer_from_index = refresh.get("store_busy").and_then(Value::as_bool) == Some(true) && action == "callers";
    if !refresh_complete && action != "outline" && !answer_from_index {
        if refresh.get("root_granted").and_then(Value::as_bool) == Some(false) {
            return Err(format!(
                "root '{}' is not a directory the host grants access to, so {action} read no index for it; a retry does not grant access",
                project_path.unwrap_or_default()
            ));
        }
        if let Some(root) = project_path {
            return Err(format!(
                "root '{root}': symbol index refresh is incomplete, so {action} read no current answer from it; cached graph is not current evidence. Run action=sync and retry. Refresh: {refresh}"
            ));
        }
        return Err(format!("symbol index refresh is incomplete; cached graph is not current evidence. Run action=sync and retry. Refresh: {refresh}"));
    }
    let limit = body
        .get("limit")
        .or_else(|| body.get("k"))
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);
    let db = db_path(project_path);
    let symbol = text_field(body, &["symbol", "name"]);
    let need_symbol = || symbol.ok_or_else(|| format!("{action} requires `symbol`"));
    let need_path =
        || text_field(body, &["path", "file"]).ok_or_else(|| format!("{action} requires `path`"));
    let mut output = match action {
        "overview" => Ok(overview(project_path, limit)),
        "status" => Ok(status(project_path)),
        "outline" => outline(&db, need_path()?, &refresh),
        "find" => Ok(find(
            &db,
            need_symbol()?,
            text_field(body, &["kind"]),
            text_field(body, &["path", "path_prefix"]),
            limit,
        )),
        "callers" => Ok(callers_or_callees(
            &db,
            project_path,
            need_symbol()?,
            true,
            limit,
        )),
        "callees" => Ok(callers_or_callees(
            &db,
            project_path,
            need_symbol()?,
            false,
            limit,
        )),
        "impact" => {
            let depth = body
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_IMPACT_DEPTH);
            let upstream = text_field(body, &["direction"])
                .map(|d| d != "callees" && d != "downstream")
                .unwrap_or(true);
            let through_ambiguous = body
                .get("through_ambiguous")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(impact(
                &db,
                project_path,
                need_symbol()?,
                depth,
                upstream,
                limit,
                through_ambiguous,
            ))
        }
        "hotspots" => Ok(hotspots(&db, project_path, limit)),
        "orphans" => Ok(orphans(
            &db,
            project_path,
            limit,
            text_field(body, &["language", "lang"]).unwrap_or(""),
            &normalized_path(text_field(body, &["path", "path_prefix"]).unwrap_or("")),
        )),
        "imports" => imports(&db, need_path()?),
        "importers" => Ok(importers(&db, need_path()?, limit)),
        "cycles" => Ok(cycles(&db, limit)),
        "coupling" => Ok(coupling(
            &db,
            limit,
            &normalized_path(text_field(body, &["path", "path_prefix"]).unwrap_or("")),
        )),
        "complexity" => Ok(complexity(&db, project_path, body, limit)),
        "duplicates" => Ok(duplicates(&db, limit)),
        "tests" => Ok(tests_for(&db, project_path, need_symbol()?, limit)),
        other => Err(format!(
            "unknown action `{other}`; accepted: {}",
            ACTIONS.join(", ")
        )),
    }?;
    if answer_from_index {
        output["store"] = json!("busy");
        output["index_current"] = json!(false);
        output["answered_from"] = json!("persisted call-edge index (host KV); the symbol index was not refreshed because the libsql store was busy, and definition lookups read that store and may be empty this pass");
    }
    output["codeinsight_index"] = refresh;
    Ok(output)
}
