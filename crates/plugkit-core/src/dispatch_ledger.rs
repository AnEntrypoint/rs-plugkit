#![cfg(target_arch = "wasm32")]

use crate::wasm_dispatch::{
    host_fs_readdir, host_now_ms, host_read, host_remove_file_never_directory,
    host_stat, unpack_to_value_pub,
};
use crate::wasm_dispatch::host_abi::host_read_via_exec;
use serde_json::{json, Value};

const MAX_ENTRIES: usize = 20000;
pub const RETAIN_MS: u64 = 24 * 60 * 60 * 1000;
const SPOOL_TMP_MARKER: &str = ".tmp-";
const SPOOL_TMP_STALE_MS: u64 = 30 * 1000;

pub const RETAINED_ENTRIES: usize = MAX_ENTRIES;

// How many entries the ledger may hold before one append pays for a whole-file compaction. Appending
// is O(new bytes) and compaction is O(file), so this is the amortized cost of the rewrite. It sits at
// the host mint's own ceiling (agentplug trims the whole file at DISPATCH_LEDGER_MAX_ENTRIES + 500),
// so the ledger stays below the window where the host would pay for a rewrite too.
const LEDGER_COMPACT_ENTRIES: usize = 5000;

// A ledger line that carries reply metadata for an entry already on disk. Entries are immutable once
// appended, so attaching a reply hash appends one of these instead of rewriting the file.
fn is_supplement(entry: &Value) -> bool {
    entry.get("verb").is_none()
        && entry.get("dispatch_id").is_some()
        && (entry.get("reply_sha256").is_some() || entry.get("spill_file").is_some())
}

// The head holds the counts an append needs, so appending does not read the ledger. It is a cache of
// the ledger, never a source of truth: read_entries rebuilds whatever the file actually holds.
fn ledger_head_path(cwd: &str) -> String {
    format!("{}.head", ledger_path(cwd))
}

struct Head {
    raw: String,
    entries: u64,
    seq: u64,
}

fn parse_head(raw: &str) -> Head {
    let parsed = serde_json::from_str::<Value>(raw).ok();
    let entries = parsed
        .as_ref()
        .and_then(|v| v.get("entries"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let seq = parsed
        .as_ref()
        .and_then(|v| v.get("seq"))
        .and_then(Value::as_u64)
        .unwrap_or(entries);
    Head {
        raw: raw.to_string(),
        entries,
        seq,
    }
}

fn write_head(cwd: &str, entries: u64, seq: u64) -> bool {
    let path = ledger_head_path(cwd);
    let next = json!({ "entries": entries, "seq": seq }).to_string();
    let current = crate::pkfs::read_to_string(&path).unwrap_or_default();
    if current.trim().is_empty() {
        return crate::pkfs::write(&path, &next);
    }
    if parse_head(&current).raw == next {
        return true;
    }
    matches!(
        crate::pkfs::cas_write(&path, &current, &next),
        crate::pkfs::CasWriteOutcome::Swapped
    )
}

fn count_lines(raw: &str) -> u64 {
    raw.lines().filter(|line| !line.trim().is_empty()).count() as u64
}

// One-time conversion of the historical single-array ledger into JSONL. Returns the entry count.
fn ensure_jsonl(cwd: &str) -> Result<u64, String> {
    let path = ledger_path(cwd);
    if let Some(raw) = crate::pkfs::read_to_string(&ledger_head_path(cwd)) {
        if !raw.trim().is_empty() {
            return Ok(parse_head(&raw).entries);
        }
    }
    let existing = match host_read(&path) {
        Some(text) => text,
        None if host_stat(&path).is_none() => String::new(),
        None => return Err(ledger_unavailable(&path)),
    };
    if existing.trim().is_empty() {
        if !crate::pkfs::write(&path, "") {
            return Err(ledger_unavailable(&path));
        }
        let _ = write_head(cwd, 0, 0);
        return Ok(0);
    }
    if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&existing) {
        let mut out = String::new();
        for entry in &list {
            out.push_str(&entry.to_string());
            out.push('\n');
        }
        if !matches!(
            crate::pkfs::cas_write(&path, &existing, &out),
            crate::pkfs::CasWriteOutcome::Swapped
        ) {
            return Err(ledger_unavailable(&path));
        }
        let count = list.len() as u64;
        let _ = write_head(cwd, count, count);
        return Ok(count);
    }
    let count = count_lines(&existing);
    let _ = write_head(cwd, count, count);
    Ok(count)
}

fn compact_ledger(cwd: &str, ts: u64) -> Result<(), String> {
    let path = ledger_path(cwd);
    let Some(existing) = host_read(&path) else {
        return Err(ledger_unavailable(&path));
    };
    let mut entries: Vec<Value> = Vec::new();
    let mut supplements: Vec<Value> = Vec::new();
    for line in existing.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if is_supplement(&entry) {
            supplements.push(entry);
        } else {
            entries.push(entry);
        }
    }
    let cited = crate::orchestrator::prd::open_row_witness_dispatch_ids();
    entries.retain(|e| {
        e.get("ts")
            .and_then(|v| v.as_u64())
            .is_some_and(|t| t.saturating_add(RETAIN_MS) >= ts)
    });
    if entries.len() > MAX_ENTRIES {
        let mut excess = entries.len() - MAX_ENTRIES;
        entries.retain(|e| {
            let is_cited = e
                .get("dispatch_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| cited.contains(id));
            if excess > 0 && !is_cited {
                excess -= 1;
                return false;
            }
            true
        });
    }
    let mut out = String::new();
    for entry in &entries {
        out.push_str(&entry.to_string());
        out.push('\n');
    }
    for supplement in &supplements {
        let keep = supplement
            .get("dispatch_id")
            .and_then(Value::as_str)
            .is_some_and(|id| entries.iter().any(|e| e.get("dispatch_id").and_then(Value::as_str) == Some(id)));
        if keep {
            out.push_str(&supplement.to_string());
            out.push('\n');
        }
    }
    if !matches!(
        crate::pkfs::cas_write(&path, &existing, &out),
        crate::pkfs::CasWriteOutcome::Swapped
    ) {
        return Ok(());
    }
    let count = entries.len() as u64;
    let _ = write_head(cwd, count, count);
    Ok(())
}

pub fn ledger_path(cwd: &str) -> String {
    if cwd.is_empty() {
        crate::pkfs::anchor(".gm/exec-spool/.dispatch-ledger.json")
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

fn ledger_request_body(verb: &str, body: &str) -> String {
    if verb != "kv_put" {
        return body.to_string();
    }
    let Ok(mut parsed) = serde_json::from_str::<Value>(body) else {
        return "[REDACTED kv_put body]".to_string();
    };
    if let Some(map) = parsed.as_object_mut() {
        if let Some(value) = map.get("value").cloned() {
            use sha2::{Digest, Sha256};
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            map.insert(
                "value".to_string(),
                json!(format!(
                    "[REDACTED sha256:{:x}]",
                    Sha256::digest(text.as_bytes())
                )),
            );
        }
    }
    parsed.to_string()
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
    let (body_text, body_truncated) = bounded_request_body(&ledger_request_body(verb, request_body));
    let entries_before = ensure_jsonl(cwd)?;
    let ts = now_ms();
    let head = crate::pkfs::read_to_string(&ledger_head_path(cwd))
        .map(|raw| parse_head(&raw))
        .unwrap_or(Head {
            raw: String::new(),
            entries: entries_before,
            seq: entries_before,
        });
    let dispatch_id = new_dispatch_id(verb, fingerprint, ts, head.seq);
    let line = json!({
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
    })
    .to_string();
    if !crate::pkfs::append(&path, &format!("{}\n", line)) {
        return Err(ledger_unavailable(&path));
    }
    let _ = write_head(cwd, head.entries + 1, head.seq + 1);
    if head.entries as usize >= LEDGER_COMPACT_ENTRIES {
        let _ = compact_ledger(cwd, ts);
    }
    Ok(dispatch_id)
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
    if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&raw) {
        return Ok(list);
    }
    parse_ledger(&raw, &path)
}

// Parses a JSONL ledger: entry lines in order, supplement lines merged into the entry they name. A
// torn last line is an interrupted append and is dropped; any other unparseable line is corruption
// and is reported, so a caller never counts a broken ledger as zero entries.
fn parse_ledger(raw: &str, path: &str) -> Result<Vec<Value>, String> {
    let mut entries: Vec<Value> = Vec::new();
    let mut supplements: Vec<Value> = Vec::new();
    let mut lines: Vec<&str> = raw.lines().collect();
    if let Some(last) = lines.last() {
        if !last.trim().is_empty() && serde_json::from_str::<Value>(last.trim()).is_err() {
            lines.pop();
        }
    }
    for (index, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            return Err(format!(
                "dispatch ledger {path} line {} is not valid JSON; repair or remove that line before reading the ledger",
                index + 1
            ));
        };
        if is_supplement(&entry) {
            supplements.push(entry);
        } else {
            entries.push(entry);
        }
    }
    for supplement in supplements {
        let Some(id) = supplement.get("dispatch_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(target) = entries
            .iter_mut()
            .find(|e| e.get("dispatch_id").and_then(Value::as_str) == Some(id))
        else {
            continue;
        };
        let Some(map) = target.as_object_mut() else {
            continue;
        };
        for key in ["reply_sha256", "spill_file"] {
            if let Some(value) = supplement.get(key) {
                map.insert(key.to_string(), value.clone());
            }
        }
    }
    Ok(entries)
}

fn new_dispatch_id(verb: &str, fingerprint: &str, ts: u64, seq: u64) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    verb.hash(&mut h);
    fingerprint.hash(&mut h);
    ts.hash(&mut h);
    format!("{}-{}-{:x}", ts, seq, h.finish())
}

pub fn attach_reply_sha256(
    cwd: &str,
    dispatch_id: &str,
    reply_sha256: &str,
    spill_file: Option<&str>,
) -> Result<(), String> {
    let path = ledger_path(cwd);
    let mut supplement = json!({
        "dispatch_id": dispatch_id,
        "reply_sha256": reply_sha256,
    });
    if let Some(spill) = spill_file {
        supplement["spill_file"] = json!(spill);
    }
    if crate::pkfs::append(&path, &format!("{}\n", supplement)) {
        Ok(())
    } else {
        Err(ledger_unavailable(&path))
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
    } else if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&raw) {
        list
    } else {
        parse_ledger(&raw, &path)?
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
