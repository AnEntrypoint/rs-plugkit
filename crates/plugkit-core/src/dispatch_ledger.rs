#![cfg(target_arch = "wasm32")]

use crate::wasm_dispatch::{
    host_cas_write, host_fs_readdir, host_now_ms, host_read, host_remove_file_never_directory,
    host_stat, unpack_to_value_pub,
};
use crate::wasm_dispatch::host_abi::host_read_via_exec;
use serde_json::{json, Value};

const MAX_ENTRIES: usize = 5000;
pub const RETAIN_MS: u64 = 24 * 60 * 60 * 1000;
const SPOOL_TMP_MARKER: &str = ".tmp-";
const SPOOL_TMP_STALE_MS: u64 = 30 * 1000;

pub const RETAINED_ENTRIES: usize = MAX_ENTRIES;

pub fn ledger_path(cwd: &str) -> String {
    if cwd.is_empty() {
        ".gm/exec-spool/.dispatch-ledger.json".to_string()
    } else {
        format!(
            "{}/.gm/exec-spool/.dispatch-ledger.json",
            cwd.trim_end_matches('/').trim_end_matches('\\')
        )
    }
}

fn now_ms() -> u64 {
    unsafe { host_now_ms() }
}

fn spool_dir(cwd: &str) -> String {
    if cwd.is_empty() {
        ".gm/exec-spool".to_string()
    } else {
        format!(
            "{}/.gm/exec-spool",
            cwd.trim_end_matches('/').trim_end_matches('\\')
        )
    }
}

fn sweep_stale_spool_tmp(cwd: &str, now: u64) {
    sweep_stale_tmp_entries(&spool_dir(cwd), now, |name| name.contains(SPOOL_TMP_MARKER));
}

fn split_dir_and_name(path: &str) -> (&str, &str) {
    match path.rfind(['/', '\\']) {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => (".", path),
    }
}

pub fn sweep_stale_tmp_beside(path: &str, now: u64) {
    let (_, base) = split_dir_and_name(path);
    sweep_stale_tmp_under(path, base, now);
}

pub fn sweep_stale_tmp_under(path: &str, family: &str, now: u64) {
    let (dir, _) = split_dir_and_name(path);
    sweep_stale_tmp_entries(dir, now, |name| {
        name.starts_with(family) && name.contains(SPOOL_TMP_MARKER)
    });
}

pub fn prune_published_under(path: &str, family: &str, keep: usize) {
    let (dir, _) = split_dir_and_name(path);
    let Value::Array(entries) =
        unpack_to_value_pub(unsafe { host_fs_readdir(dir.as_ptr(), dir.len() as u32) })
    else {
        return;
    };
    let mut published: Vec<(u64, u64, &str)> = entries
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|name| {
            let stem = name.strip_prefix(family)?.strip_suffix(".json")?;
            let (ms, seq) = stem.split_once('-')?;
            Some((ms.parse().ok()?, seq.parse().ok()?, name))
        })
        .collect();
    published.sort_unstable_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    for (_, _, name) in published.into_iter().skip(keep) {
        host_remove_file_never_directory(&format!("{dir}/{name}"));
    }
}

fn sweep_stale_tmp_entries(dir: &str, now: u64, owns: impl Fn(&str) -> bool) {
    let Value::Array(entries) =
        unpack_to_value_pub(unsafe { host_fs_readdir(dir.as_ptr(), dir.len() as u32) })
    else {
        return;
    };
    for name in entries.iter().filter_map(Value::as_str) {
        if !owns(name) {
            continue;
        }
        let path = format!("{dir}/{name}");
        let stale = host_stat(&path)
            .and_then(|stat| stat.get("mtimeMs").and_then(Value::as_u64))
            .is_some_and(|mtime| mtime.saturating_add(SPOOL_TMP_STALE_MS) < now);
        if stale {
            host_remove_file_never_directory(&path);
        }
    }
}

const LEDGER_BODY_MAX_CHARS: usize = 8192;

const SECRET_KEYS: [&str; 8] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "authorization",
    "bearer",
];

fn redact_secrets(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < lb.len() {
        let Some(key) = SECRET_KEYS
            .iter()
            .find(|k| lb[i..].starts_with(k.as_bytes()))
        else {
            i += 1;
            continue;
        };
        let mut j = i + key.len();
        while j < lb.len() && (lb[j].is_ascii_alphanumeric() || lb[j] == b'_') {
            j += 1;
        }
        let skip_quotes = |mut k: usize| {
            while k < lb.len() && matches!(lb[k], b'"' | b'\'' | b' ' | b'\t') {
                k += 1;
            }
            k
        };
        let mut k = skip_quotes(j);
        let value_start = if k < lb.len() && matches!(lb[k], b':' | b'=') {
            skip_quotes(k + 1)
        } else if k > j && matches!(*key, "bearer" | "authorization") {
            k
        } else {
            i = j;
            continue;
        };
        k = value_start;
        let mut end = k;
        while end < lb.len()
            && !matches!(
                lb[end],
                b'"' | b'\'' | b',' | b'&' | b'}' | b']' | b')' | b';' | b'\n' | b'\r'
            )
        {
            end += 1;
        }
        if end == k {
            i = j;
            continue;
        }
        out.push_str(&body[cursor..k]);
        out.push_str("[REDACTED]");
        cursor = end;
        i = end;
    }
    out.push_str(&body[cursor..]);
    out
}

fn bounded_request_body(body: &str) -> (String, bool) {
    let redacted = redact_secrets(body);
    let text: String = redacted.chars().take(LEDGER_BODY_MAX_CHARS).collect();
    let truncated = text.len() < redacted.len();
    (text, truncated)
}

pub fn record(
    cwd: &str,
    verb: &str,
    request_body: &str,
    fingerprint: &str,
    exit_code: i64,
    session_id: Option<&str>,
    output_bytes: Option<u64>,
) -> Result<String, String> {
    let path = ledger_path(cwd);
    sweep_stale_spool_tmp(cwd, now_ms());
    let (body_text, body_truncated) = bounded_request_body(request_body);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let existing = match host_read(&path) {
            Some(text) => text,
            None if host_stat(&path).is_none() => String::new(),
            None => return Err(ledger_unavailable(&path)),
        };
        let parsed: Option<Vec<Value>> = if existing.trim().is_empty() {
            Some(vec![])
        } else {
            match serde_json::from_str::<Value>(&existing) {
                Ok(Value::Array(a)) => Some(a),
                _ => None,
            }
        };
        let ts = now_ms();
        let Some(mut list) = parsed else {
            return Err(ledger_unavailable(&path));
        };
        let seq = list.len() as u64;
        let dispatch_id = new_dispatch_id(verb, fingerprint, ts, seq);
        list.push(json!({
            "dispatch_id": dispatch_id,
            "verb": verb,
            "fingerprint": fingerprint,
            "ts": ts,
            "exit_code": exit_code,
            "output_bytes": output_bytes,
            "session_id": session_id,
            "cwd": cwd,
            "request_body": body_text,
            "request_body_truncated": body_truncated,
        }));
        list.retain(|e| {
            e.get("ts")
                .and_then(|v| v.as_u64())
                .is_some_and(|t| t.saturating_add(RETAIN_MS) >= ts)
        });
        if list.len() > MAX_ENTRIES {
            let drop = list.len() - MAX_ENTRIES;
            list.drain(0..drop);
        }
        let serialized = Value::Array(list).to_string();
        match host_cas_write(&path, &existing, &serialized) {
            CAS_SWAPPED => return Ok(dispatch_id),
            CAS_MISMATCH if attempt < CAS_MAX_ATTEMPTS => continue,
            _ => return Err(ledger_unavailable(&path)),
        }
    }
}

fn ledger_unavailable(path: &str) -> String {
    format!(
        "dispatch ledger {path} could not be read or written as a JSON array; dispatch not recorded and the file left unchanged"
    )
}

pub fn read_entries(cwd: &str) -> Result<Vec<Value>, String> {
    let path = ledger_path(cwd);
    let raw = match host_read(&path) {
        Some(text) => text,
        None if host_stat(&path).is_none() => return Ok(vec![]),
        None => return Err(ledger_unavailable(&path)),
    };
    if raw.trim().is_empty() {
        return Ok(vec![]);
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(a)) => Ok(a),
        _ => Err(ledger_unavailable(&path)),
    }
}

fn new_dispatch_id(verb: &str, fingerprint: &str, ts: u64, seq: u64) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    verb.hash(&mut h);
    fingerprint.hash(&mut h);
    ts.hash(&mut h);
    format!("{}-{}-{:x}", ts, seq, h.finish())
}

const CAS_SWAPPED: u32 = 1;
const CAS_MISMATCH: u32 = 2;
const CAS_MAX_ATTEMPTS: u32 = 16;

pub fn attach_reply_sha256(cwd: &str, dispatch_id: &str, reply_sha256: &str) {
    let path = ledger_path(cwd);
    for _ in 0..CAS_MAX_ATTEMPTS {
        let existing = host_read(&path).unwrap_or_default();
        let Ok(Value::Array(mut list)) = serde_json::from_str::<Value>(&existing) else {
            return;
        };
        let Some(entry) = list
            .iter_mut()
            .find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id))
        else {
            return;
        };
        entry["reply_sha256"] = json!(reply_sha256);
        if host_cas_write(&path, &existing, &Value::Array(list).to_string()) == CAS_SWAPPED {
            return;
        }
    }
}

pub fn subagent_running_count(
    cwd: &str,
    parent_session: &str,
    now_ms: u64,
    window_ms: u64,
) -> Result<usize, String> {
    let prefix = format!("{}-", parent_session);
    let since_ms = now_ms.saturating_sub(window_ms);
    Ok(read_entries(cwd)?
        .iter()
        .filter(|e| {
            e.get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with(&prefix) && s.contains("-sub"))
                && e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0) >= since_ms
        })
        .count())
}

pub fn subagent_dispatches_since(
    cwd: &str,
    parent_session: &str,
    since_ms: u64,
) -> Result<usize, String> {
    let prefix = format!("{}-", parent_session);
    Ok(read_entries(cwd)?
        .iter()
        .filter(|e| {
            e.get("session_id")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with(&prefix))
                && e.get("ts").and_then(|v| v.as_u64()).unwrap_or(0) >= since_ms
        })
        .count())
}

pub fn lookup(cwd: &str, dispatch_id: &str) -> Result<Option<Value>, String> {
    Ok(read_entries(cwd)?
        .into_iter()
        .find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id)))
}

pub fn lookup_outside_root(cwd: &str, dispatch_id: &str) -> Result<Option<Value>, String> {
    let path = ledger_path(cwd);
    let raw = host_read_via_exec(&path).ok_or_else(|| ledger_unavailable(&path))?;
    let entries = if raw.trim().is_empty() {
        Vec::new()
    } else {
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Array(a)) => a,
            _ => return Err(ledger_unavailable(&path)),
        }
    };
    Ok(entries
        .into_iter()
        .find(|e| e.get("dispatch_id").and_then(|v| v.as_str()) == Some(dispatch_id)))
}

fn nested_scope_roots(cwd: &str) -> Vec<String> {
    let project = if cwd.is_empty() {
        crate::wasm_dispatch::host_cwd_string().unwrap_or_default()
    } else {
        cwd.to_string()
    };
    let root = project.trim_end_matches('/').trim_end_matches('\\');
    let under = |child: &str| format!("{}/{}", root, child);
    let gitmodules = host_read(&under(".gitmodules")).unwrap_or_default();
    let submodules: Vec<String> = gitmodules
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("path")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|path| under(path.trim()))
        })
        .collect();
    submodules
        .iter()
        .flat_map(|sub| {
            std::iter::once(sub.clone()).chain(crate::scan_universe::nested_worktree_dirs(sub))
        })
        .chain(crate::scan_universe::nested_worktree_dirs(root))
        .collect()
}

pub fn lookup_in_scope(cwd: &str, dispatch_id: &str) -> Result<Option<Value>, String> {
    if let Some(entry) = lookup(cwd, dispatch_id)? {
        return Ok(Some(entry));
    }
    for sub_cwd in nested_scope_roots(cwd) {
        if let Some(entry) = lookup(&sub_cwd, dispatch_id)? {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

pub fn submodule_holding(cwd: &str, dispatch_id: &str) -> Result<Option<String>, String> {
    for sub_cwd in nested_scope_roots(cwd) {
        if lookup(&sub_cwd, dispatch_id)?.is_some() {
            return Ok(Some(ledger_path(&sub_cwd)));
        }
    }
    Ok(None)
}

pub fn window(cwd: &str) -> Result<Option<(usize, u64)>, String> {
    let list = read_entries(cwd)?;
    let oldest = list
        .iter()
        .filter_map(|e| e.get("ts").and_then(|v| v.as_u64()))
        .min();
    Ok(oldest.map(|oldest| (list.len(), oldest)))
}
