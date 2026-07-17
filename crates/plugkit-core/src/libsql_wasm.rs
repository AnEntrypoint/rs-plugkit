#![cfg(target_arch = "wasm32")]

use libsql_ffi as ffi;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::Mutex;

static DBS: Mutex<Option<HashMap<String, DbHandle>>> = Mutex::new(None);

struct DbHandle(*mut ffi::sqlite3);
unsafe impl Send for DbHandle {}

pub fn open(name: &str, path: &str) -> Result<(), String> {
    let mut guard = DBS.lock().map_err(|e| e.to_string())?;
    let map = guard.get_or_insert_with(HashMap::new);
    if map.contains_key(name) { return Ok(()); }
    let cpath = CString::new(path).map_err(|e| e.to_string())?;
    let mut db: *mut ffi::sqlite3 = ptr::null_mut();
    let rc = unsafe {
        ffi::sqlite3_open_v2(
            cpath.as_ptr(),
            &mut db,
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE,
            ptr::null(),
        )
    };
    if rc != ffi::SQLITE_OK {
        let msg = if db.is_null() { format!("rc={}", rc) } else {
            let m = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            unsafe { ffi::sqlite3_close(db); }
            format!("rc={} msg={}", rc, m)
        };
        return Err(format!("sqlite3_open_v2 {}", msg));
    }
    map.insert(name.to_string(), DbHandle(db));
    Ok(())
}

pub fn close(name: &str) -> Result<(), String> {
    let mut guard = DBS.lock().map_err(|e| e.to_string())?;
    let map = match guard.as_mut() { Some(m) => m, None => return Ok(()) };
    if let Some(h) = map.remove(name) {
        unsafe { ffi::sqlite3_close(h.0); }
    }
    Ok(())
}

pub fn list_dbs() -> Vec<String> {
    let guard = DBS.lock().ok();
    guard.as_ref().and_then(|g| g.as_ref()).map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

fn with_db<F, R>(name: &str, f: F) -> Result<R, String>
where
    F: FnOnce(*mut ffi::sqlite3) -> Result<R, String>,
{
    let guard = DBS.lock().map_err(|e| e.to_string())?;
    let map = guard.as_ref().ok_or_else(|| "no dbs open".to_string())?;
    let h = map.get(name).ok_or_else(|| format!("db '{}' not open", name))?;
    f(h.0)
}

pub fn exec(name: &str, sql: &str) -> Result<(), String> {
    with_db(name, |db| {
        let csql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut err_ptr: *mut i8 = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_exec(db, csql.as_ptr(), None, ptr::null_mut(), &mut err_ptr) };
        if rc != ffi::SQLITE_OK {
            let msg = if err_ptr.is_null() {
                "unknown".to_string()
            } else {
                let s = unsafe { CStr::from_ptr(err_ptr).to_string_lossy().into_owned() };
                unsafe { ffi::sqlite3_free(err_ptr as *mut _); }
                s
            };
            return Err(format!("exec rc={} msg={}", rc, msg));
        }
        Ok(())
    })
}

pub fn query(name: &str, sql: &str) -> Result<Value, String> {
    with_db(name, |db| {
        let csql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_prepare_v2(db, csql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != ffi::SQLITE_OK {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            return Err(format!("prepare rc={} msg={}", rc, msg));
        }
        let ncols = unsafe { ffi::sqlite3_column_count(stmt) };
        let mut col_names = Vec::with_capacity(ncols as usize);
        for i in 0..ncols {
            let nm = unsafe { CStr::from_ptr(ffi::sqlite3_column_name(stmt, i)).to_string_lossy().into_owned() };
            col_names.push(nm);
        }
        let mut rows: Vec<Value> = Vec::new();
        loop {
            let step = unsafe { ffi::sqlite3_step(stmt) };
            if step == ffi::SQLITE_DONE { break; }
            if step != ffi::SQLITE_ROW {
                let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
                unsafe { ffi::sqlite3_finalize(stmt); }
                return Err(format!("step rc={} msg={}", step, msg));
            }
            let mut row = serde_json::Map::new();
            for i in 0..ncols {
                let ctype = unsafe { ffi::sqlite3_column_type(stmt, i) };
                let v = match ctype {
                    ffi::SQLITE_INTEGER => Value::from(unsafe { ffi::sqlite3_column_int64(stmt, i) }),
                    ffi::SQLITE_FLOAT => Value::from(unsafe { ffi::sqlite3_column_double(stmt, i) }),
                    ffi::SQLITE_NULL => Value::Null,
                    ffi::SQLITE_TEXT => {
                        let p = unsafe { ffi::sqlite3_column_text(stmt, i) };
                        if p.is_null() { Value::Null }
                        else {
                            let s = unsafe { CStr::from_ptr(p as *const _).to_string_lossy().into_owned() };
                            Value::String(s)
                        }
                    }
                    ffi::SQLITE_BLOB => {
                        let n = unsafe { ffi::sqlite3_column_bytes(stmt, i) } as usize;
                        let p = unsafe { ffi::sqlite3_column_blob(stmt, i) } as *const u8;
                        if p.is_null() || n == 0 { Value::Null }
                        else {
                            let bytes = unsafe { std::slice::from_raw_parts(p, n) };
                            Value::String(format!("blob:{}b", bytes.len()))
                        }
                    }
                    _ => Value::Null,
                };
                row.insert(col_names[i as usize].clone(), v);
            }
            rows.push(Value::Object(row));
        }
        unsafe { ffi::sqlite3_finalize(stmt); }
        Ok(Value::Array(rows))
    })
}

pub fn exec_params(name: &str, sql: &str, params: &[&str]) -> Result<(), String> {
    with_db(name, |db| {
        let csql = CString::new(sql).map_err(|e| e.to_string())?;
        let cparams: Vec<CString> = params.iter()
            .map(|p| CString::new(*p).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_prepare_v2(db, csql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != ffi::SQLITE_OK {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            return Err(format!("prepare rc={} msg={}", rc, msg));
        }
        for (i, cp) in cparams.iter().enumerate() {
            let rc = unsafe {
                ffi::sqlite3_bind_text(stmt, (i + 1) as i32, cp.as_ptr(), -1, None)
            };
            if rc != ffi::SQLITE_OK {
                unsafe { ffi::sqlite3_finalize(stmt); }
                return Err(format!("bind param {} rc={}", i, rc));
            }
        }
        let step = unsafe { ffi::sqlite3_step(stmt) };
        unsafe { ffi::sqlite3_finalize(stmt); }
        if step != ffi::SQLITE_DONE && step != ffi::SQLITE_ROW {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            return Err(format!("step rc={} msg={}", step, msg));
        }
        Ok(())
    })
}

/// begin/commit/rollback wrap the default autocommit-per-statement mode into
/// a single fsync-backed transaction for callers issuing many INSERT/UPDATE
/// statements in a loop (e.g. code_index::index()) -- one commit for the
/// whole batch instead of one per row. code_index::index() calls these
/// directly (not via a closure-wrapping helper) since its loop body has many
/// early `continue`s that don't map cleanly onto a single wrapped closure.
pub fn begin(name: &str) -> Result<(), String> { exec(name, "BEGIN IMMEDIATE") }
pub fn commit(name: &str) -> Result<(), String> { exec(name, "COMMIT") }
pub fn rollback(name: &str) -> Result<(), String> { exec(name, "ROLLBACK") }

pub struct PreparedStmt {
    db: *mut ffi::sqlite3,
    stmt: *mut ffi::sqlite3_stmt,
}
unsafe impl Send for PreparedStmt {}

impl Drop for PreparedStmt {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            unsafe { ffi::sqlite3_finalize(self.stmt); }
        }
    }
}

/// Prepares `sql` once against `name`'s open connection; reuse via
/// `execute_bound` for every row in a batch to avoid re-parsing/re-planning
/// the same INSERT statement on every call (exec_params does prepare+finalize
/// per invocation, which is fine for one-off writes but wasteful in a loop).
pub fn prepare(name: &str, sql: &str) -> Result<PreparedStmt, String> {
    with_db(name, |db| {
        let csql = CString::new(sql).map_err(|e| e.to_string())?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_prepare_v2(db, csql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != ffi::SQLITE_OK {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            return Err(format!("prepare rc={} msg={}", rc, msg));
        }
        Ok(PreparedStmt { db, stmt })
    })
}

impl PreparedStmt {
    pub fn execute_bound(&self, params: &[&str]) -> Result<(), String> {
        let cparams: Vec<CString> = params.iter()
            .map(|p| CString::new(*p).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        unsafe { ffi::sqlite3_reset(self.stmt); }
        unsafe { ffi::sqlite3_clear_bindings(self.stmt); }
        for (i, cp) in cparams.iter().enumerate() {
            let rc = unsafe { ffi::sqlite3_bind_text(self.stmt, (i + 1) as i32, cp.as_ptr(), -1, None) };
            if rc != ffi::SQLITE_OK {
                return Err(format!("bind param {} rc={}", i, rc));
            }
        }
        let step = unsafe { ffi::sqlite3_step(self.stmt) };
        if step != ffi::SQLITE_DONE && step != ffi::SQLITE_ROW {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(self.db)).to_string_lossy().into_owned() };
            return Err(format!("step rc={} msg={}", step, msg));
        }
        Ok(())
    }
}

pub fn query_params(name: &str, sql: &str, params: &[&str]) -> Result<Value, String> {
    with_db(name, |db| {
        let csql = CString::new(sql).map_err(|e| e.to_string())?;
        let cparams: Vec<CString> = params.iter()
            .map(|p| CString::new(*p).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut stmt: *mut ffi::sqlite3_stmt = ptr::null_mut();
        let rc = unsafe { ffi::sqlite3_prepare_v2(db, csql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != ffi::SQLITE_OK {
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
            return Err(format!("prepare rc={} msg={}", rc, msg));
        }
        for (i, cp) in cparams.iter().enumerate() {
            let rc = unsafe {
                ffi::sqlite3_bind_text(stmt, (i + 1) as i32, cp.as_ptr(), -1, None)
            };
            if rc != ffi::SQLITE_OK {
                unsafe { ffi::sqlite3_finalize(stmt); }
                return Err(format!("bind param {} rc={}", i, rc));
            }
        }
        let ncols = unsafe { ffi::sqlite3_column_count(stmt) };
        let mut col_names = Vec::with_capacity(ncols as usize);
        for i in 0..ncols {
            let nm = unsafe { CStr::from_ptr(ffi::sqlite3_column_name(stmt, i)).to_string_lossy().into_owned() };
            col_names.push(nm);
        }
        let mut rows: Vec<Value> = Vec::new();
        loop {
            let step = unsafe { ffi::sqlite3_step(stmt) };
            if step == ffi::SQLITE_DONE { break; }
            if step != ffi::SQLITE_ROW {
                let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)).to_string_lossy().into_owned() };
                unsafe { ffi::sqlite3_finalize(stmt); }
                return Err(format!("step rc={} msg={}", step, msg));
            }
            let mut row = serde_json::Map::new();
            for i in 0..ncols {
                let ctype = unsafe { ffi::sqlite3_column_type(stmt, i) };
                let v = match ctype {
                    ffi::SQLITE_INTEGER => Value::from(unsafe { ffi::sqlite3_column_int64(stmt, i) }),
                    ffi::SQLITE_FLOAT => Value::from(unsafe { ffi::sqlite3_column_double(stmt, i) }),
                    ffi::SQLITE_NULL => Value::Null,
                    ffi::SQLITE_TEXT => {
                        let p = unsafe { ffi::sqlite3_column_text(stmt, i) };
                        if p.is_null() { Value::Null }
                        else {
                            let s = unsafe { CStr::from_ptr(p as *const _).to_string_lossy().into_owned() };
                            Value::String(s)
                        }
                    }
                    _ => Value::Null,
                };
                row.insert(col_names[i as usize].clone(), v);
            }
            rows.push(Value::Object(row));
        }
        unsafe { ffi::sqlite3_finalize(stmt); }
        Ok(Value::Array(rows))
    })
}

pub fn serialize(name: &str) -> Result<Vec<u8>, String> {
    with_db(name, |db| {
        let schema = CString::new("main").unwrap();
        let mut size: i64 = 0;
        let p = unsafe { ffi::sqlite3_serialize(db, schema.as_ptr(), &mut size, 0) };
        if p.is_null() || size <= 0 { return Err(format!("serialize null (size={})", size)); }
        let bytes = unsafe { std::slice::from_raw_parts(p, size as usize).to_vec() };
        unsafe { ffi::sqlite3_free(p as *mut _); }
        Ok(bytes)
    })
}

pub fn deserialize(name: &str, bytes: &[u8]) -> Result<(), String> {
    with_db(name, |db| {
        let schema = CString::new("main").unwrap();
        let size = bytes.len() as i64;
        let buf = unsafe { ffi::sqlite3_malloc64(size as u64) } as *mut u8;
        if buf.is_null() { return Err("malloc failed".to_string()); }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len()); }
        let flags = (ffi::SQLITE_DESERIALIZE_FREEONCLOSE | ffi::SQLITE_DESERIALIZE_RESIZEABLE) as u32;
        let rc = unsafe {
            ffi::sqlite3_deserialize(db, schema.as_ptr(), buf, size, size, flags)
        };
        if rc != ffi::SQLITE_OK {
            return Err(format!("deserialize rc={}", rc));
        }
        Ok(())
    })
}

pub fn smoke() -> Value {
    let mut log: Vec<Value> = Vec::new();
    let n = "smoke";
    log.push(json!({ "step": "open", "result": open(n, ":memory:").err() }));
    log.push(json!({ "step": "create_table", "result": exec(n, "CREATE TABLE memos (id INTEGER PRIMARY KEY, text TEXT, emb F32_BLOB(4))").err() }));
    log.push(json!({ "step": "insert", "result": exec(n, "INSERT INTO memos(text, emb) VALUES ('hello', vector('[0.1,0.2,0.3,0.4]'))").err() }));
    log.push(json!({ "step": "create_index", "result": exec(n, "CREATE INDEX memos_idx ON memos(libsql_vector_idx(emb, 'metric=cosine'))").err() }));
    log.push(json!({ "step": "vector_top_k", "rows": query(n, "SELECT id, text, vector_distance_cos(emb, vector('[0.1,0.2,0.3,0.4]')) AS d FROM vector_top_k('memos_idx', vector('[0.1,0.2,0.3,0.4]'), 5) JOIN memos ON memos.rowid = id").ok() }));
    let _ = close(n);
    json!({ "ok": true, "smoke": log, "libsql_version": libsql_version() })
}

fn libsql_version() -> String {
    unsafe {
        let p = ffi::sqlite3_libversion();
        if p.is_null() { return "unknown".to_string(); }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
