use crate::pkfs;
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::cas;
use super::yaml_util::{serialize_checked, yaml_to_json};

const BOM: char = '\u{feff}';
const LOG_VERSION: u32 = 1;
const ITEM_SUFFIX: &str = ".yml";
const ITEM_STEM_MAX_CHARS: usize = 80;
#[cfg(windows)]
const RESERVED_STEMS: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];
#[cfg(not(windows))]
const RESERVED_STEMS: &[&str] = &[];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Prd,
    Mutables,
}

impl Kind {
    pub fn dir_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd",
            Kind::Mutables => "mutables",
        }
    }
    fn legacy_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd.yml",
            Kind::Mutables => "mutables.yml",
        }
    }
    fn log_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd-log.jsonl",
            Kind::Mutables => "mutables-log.jsonl",
        }
    }
    fn marker_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd.absorbed.json",
            Kind::Mutables => "mutables.absorbed.json",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Prd => "prd",
            Kind::Mutables => "mutables",
        }
    }
    pub fn legacy_rel(&self) -> String {
        format!(".gm/{}", self.legacy_name())
    }
    pub fn dir_rel(&self) -> String {
        format!(".gm/{}", self.dir_name())
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    pub kind: Kind,
    pub dir: String,
    pub legacy: String,
    pub log: String,
    marker: String,
}

impl Store {
    pub fn new(kind: Kind) -> Self {
        let gm = super::gm_dir().to_string_lossy().replace('\\', "/");
        Self {
            dir: format!("{}/{}", gm, kind.dir_name()),
            legacy: format!("{}/{}", gm, kind.legacy_name()),
            log: format!("{}/{}", gm, kind.log_name()),
            marker: format!("{}/{}", gm, kind.marker_name()),
            kind,
        }
    }
    pub fn prd() -> Self {
        Self::new(Kind::Prd)
    }
    pub fn mutables() -> Self {
        Self::new(Kind::Mutables)
    }
    pub fn for_cwd(kind: Kind, cwd: Option<&str>) -> Self {
        match cwd {
            Some(c) if !c.trim().is_empty() => {
                let root = c.trim().trim_end_matches(['/', '\\']).replace('\\', "/");
                Self {
                    dir: format!("{}/.gm/{}", root, kind.dir_name()),
                    legacy: format!("{}/.gm/{}", root, kind.legacy_name()),
                    log: format!("{}/.gm/{}", root, kind.log_name()),
                    marker: format!("{}/.gm/{}", root, kind.marker_name()),
                    kind,
                }
            }
            _ => Self::new(kind),
        }
    }
}

fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

pub fn item_stem(id: &str) -> String {
    let trimmed = id.trim();
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') || out.ends_with('.') {
        out.pop();
    }
    let out = out.trim_start_matches('.').to_string();
    if out.is_empty() {
        return "_no-id".to_string();
    }
    let suffixed = if out == trimmed {
        out.clone()
    } else {
        format!("{}-{:08x}", out, fnv1a64(trimmed))
    };
    let capped = if suffixed.chars().count() > ITEM_STEM_MAX_CHARS {
        format!(
            "{}-{:08x}",
            suffixed.chars().take(ITEM_STEM_MAX_CHARS).collect::<String>(),
            fnv1a64(trimmed)
        )
    } else {
        suffixed
    };
    let capped = capped.trim_end_matches('.').to_string();
    if RESERVED_STEMS.contains(&capped.as_str()) {
        return format!("_{}", capped);
    }
    capped
}

pub fn item_path(store: &Store, id: &str) -> String {
    format!("{}/{}{}", store.dir, item_stem(id), ITEM_SUFFIX)
}

fn item_path_by_stem(store: &Store, stem: &str) -> String {
    format!("{}/{}{}", store.dir, stem, ITEM_SUFFIX)
}

pub fn block_id(block: &Value) -> Option<String> {
    block
        .as_mapping()
        .and_then(|m| m.get(&Value::String("id".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_blocks(path: &str, text: &str) -> Result<Vec<Value>, (String, i32)> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let source = text.strip_prefix(BOM).unwrap_or(text);
    let doc: Value = serde_yaml::from_str(source).map_err(|e| {
        (
            format!(
                "{} contains non-empty content that failed to parse as YAML ({}). Writing would discard it, so the write is aborted; repair the file by hand or remove it",
                path, e
            ),
            1,
        )
    })?;
    Ok(match doc {
        Value::Sequence(seq) => seq,
        other => vec![other],
    })
}

pub fn serialize_blocks(blocks: &[Value]) -> Result<String, String> {
    let doc = if blocks.len() == 1 {
        blocks[0].clone()
    } else {
        Value::Sequence(blocks.to_vec())
    };
    serialize_checked(&doc)
}

pub struct ReadSet {
    pub blocks: Vec<Value>,
    pub raw: BTreeMap<String, String>,
}

fn is_item_file_name(name: &str) -> bool {
    !(name.starts_with('.') || name.contains(".tmp-") || !name.ends_with(ITEM_SUFFIX))
}

fn listing_stems(store: &Store) -> Vec<String> {
    let Some(listing) = pkfs::readdir(&store.dir) else {
        return Vec::new();
    };
    let Some(entries) = listing.as_array() else {
        return Vec::new();
    };
    let mut stems: Vec<String> = Vec::new();
    for entry in entries {
        let name = match entry.as_str() {
            Some(n) => n,
            None => continue,
        };
        if !is_item_file_name(name) {
            continue;
        }
        stems.push(name[..name.len() - ITEM_SUFFIX.len()].to_string());
    }
    stems.sort();
    stems
}

fn legacy_stat(store: &Store) -> Option<(u64, u64)> {
    let v = pkfs::stat(&store.legacy).filter(|v| !v.is_null())?;
    let size = v.get("size").and_then(|x| x.as_u64())?;
    let mtime = v
        .get("mtime_ms")
        .or_else(|| v.get("mtimeMs"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    Some((size, mtime))
}

fn marker_matches(store: &Store) -> bool {
    let Some(raw) = pkfs::read_to_string(&store.marker) else {
        return false;
    };
    let Ok(marker) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(state) = marker.get("legacy") else {
        return false;
    };
    match legacy_stat(store) {
        Some((size, mtime)) => {
            state.get("absent").and_then(|v| v.as_bool()) != Some(true)
                && state.get("size").and_then(|v| v.as_u64()) == Some(size)
                && state.get("mtime_ms").and_then(|v| v.as_u64()) == Some(mtime)
        }
        None => state.get("absent").and_then(|v| v.as_bool()) == Some(true),
    }
}

fn write_marker(store: &Store, stat: Option<(u64, u64)>) {
    let state = match stat {
        Some((size, mtime)) => serde_json::json!({ "size": size, "mtime_ms": mtime }),
        None => serde_json::json!({ "absent": true }),
    };
    let body = serde_json::json!({ "legacy": state, "dir": store.dir });
    let _ = pkfs::write(&store.marker, &body.to_string());
}

fn parse_lenient(text: &str) -> Vec<Value> {
    let source = text.strip_prefix(BOM).unwrap_or(text);
    if source.trim().is_empty() {
        return Vec::new();
    }
    match serde_yaml::from_str::<Value>(source) {
        Ok(Value::Sequence(seq)) => seq,
        Ok(other) => vec![other],
        Err(_) => Vec::new(),
    }
}

fn merge_blocks(existing: Vec<Value>, incoming: Vec<Value>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(existing.len() + incoming.len());
    for block in existing {
        seen.insert(serde_yaml::to_string(&block).unwrap_or_default());
        out.push(block);
    }
    for block in incoming {
        let key = serde_yaml::to_string(&block).unwrap_or_default();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(block);
    }
    out
}

pub fn maybe_absorb(store: &Store) {
    if marker_matches(store) {
        return;
    }
    let Some(legacy_raw) = pkfs::read_to_string(&store.legacy) else {
        write_marker(store, None);
        return;
    };
    let legacy_text = legacy_raw.strip_prefix(BOM).unwrap_or(legacy_raw.as_str());
    if !legacy_text.trim().is_empty() {
        let legacy_blocks = parse_lenient(legacy_text);
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for block in legacy_blocks {
            let stem = item_stem(block_id(&block).unwrap_or_default().as_str());
            groups.entry(stem).or_default().push(block);
        }
        let resolved = resolved_stems(store);
        let mut landed = true;
        for (stem, blocks) in groups {
            if resolved.contains(&stem) {
                continue;
            }
            let path = item_path_by_stem(store, &stem);
            let existing = pkfs::read_to_string(&path).unwrap_or_default();
            let merged = merge_blocks(parse_lenient(&existing), blocks);
            let Ok(raw) = serialize_blocks(&merged) else {
                landed = false;
                continue;
            };
            if raw == existing {
                continue;
            }
            if !matches!(
                pkfs::cas_write(&path, &existing, &raw),
                pkfs::CasWriteOutcome::Swapped
            ) {
                landed = false;
            }
        }
        if !landed {
            return;
        }
    }
    write_marker(store, legacy_stat(store));
}

pub fn read_blocks(store: &Store) -> Result<ReadSet, (String, i32)> {
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    let mut blocks: Vec<Value> = Vec::new();
    for stem in listing_stems(store) {
        let path = item_path_by_stem(store, &stem);
        let Some(text) = pkfs::read_to_string(&path) else {
            return Err((
                format!("{} exists but could not be read", path),
                1,
            ));
        };
        let parsed = parse_blocks(&path, &text)?;
        raw.insert(stem, text);
        blocks.extend(parsed);
    }
    if !marker_matches(store) {
        if let Some(legacy_raw) = pkfs::read_to_string(&store.legacy) {
            let legacy_text = legacy_raw.strip_prefix(BOM).unwrap_or(legacy_raw.as_str());
            if !legacy_text.trim().is_empty() {
                let parsed = parse_blocks(&store.legacy, legacy_text)?;
                let resolved = resolved_stems(store);
                for block in parsed {
                    let stem = item_stem(block_id(&block).unwrap_or_default().as_str());
                    if raw.contains_key(&stem) || resolved.contains(&stem) {
                        continue;
                    }
                    blocks.push(block);
                }
            }
        }
    }
    Ok(ReadSet { blocks, raw })
}

pub enum WriteErr {
    Conflict,
    Fatal(String),
}

pub fn write_blocks(store: &Store, read: &ReadSet, new_blocks: &[Value]) -> Result<(), WriteErr> {
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for block in new_blocks {
        let stem = item_stem(block_id(block).unwrap_or_default().as_str());
        groups.entry(stem).or_default().push(block.clone());
    }
    for (stem, blocks) in groups.iter() {
        let new_raw = match serialize_blocks(blocks) {
            Ok(raw) => raw,
            Err(e) => {
                return Err(WriteErr::Fatal(format!(
                    "the updated row for {}/{}{} was not written because it is not read-back safe -- {}",
                    store.dir, stem, ITEM_SUFFIX, e
                )))
            }
        };
        let old_raw = read.raw.get(stem).cloned().unwrap_or_default();
        if old_raw == new_raw {
            continue;
        }
        let path = item_path_by_stem(store, stem);
        match pkfs::cas_write(&path, &old_raw, &new_raw) {
            pkfs::CasWriteOutcome::Swapped => {}
            pkfs::CasWriteOutcome::Mismatch => return Err(WriteErr::Conflict),
            pkfs::CasWriteOutcome::IoError => {
                return Err(WriteErr::Fatal(format!("write failed for {}", path)))
            }
        }
    }
    for stem in read.raw.keys() {
        if groups.contains_key(stem) {
            continue;
        }
        let expected = read.raw.get(stem).cloned().unwrap_or_default();
        let path = item_path_by_stem(store, stem);
        let current = pkfs::read_to_string(&path);
        match current {
            None => {}
            Some(now) if now == expected => {
                let _ = pkfs::remove_file(&path);
            }
            Some(_) => return Err(WriteErr::Conflict),
        }
    }
    Ok(())
}

pub fn cas_retry_write<T>(
    store: &Store,
    max_attempts: u32,
    verb: &str,
    modify: impl FnMut(Value) -> cas::CasOutcome<T>,
) -> Result<T, (String, String, i32)> {
    cas_retry_write_verified(store, max_attempts, verb, modify, |_doc| true)
}

pub fn cas_retry_write_verified<T>(
    store: &Store,
    max_attempts: u32,
    verb: &str,
    mut modify: impl FnMut(Value) -> cas::CasOutcome<T>,
    verify: impl Fn(&Value) -> bool,
) -> Result<T, (String, String, i32)> {
    maybe_absorb(store);
    let max_attempts = max_attempts.max(1);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let read = match read_blocks(store) {
            Ok(read) => read,
            Err((message, code)) => {
                return Err((String::new(), format!("{} refused: {}", verb, message), code))
            }
        };
        let doc = Value::Sequence(read.blocks.clone());
        let (new_doc, result) = match modify(doc) {
            cas::CasOutcome::Write(new_doc, result) => (new_doc, result),
            cas::CasOutcome::Abort(out, err, rc) => return Err((out, err, rc)),
        };
        let new_blocks = new_doc.as_sequence().cloned().unwrap_or_default();
        match write_blocks(store, &read, &new_blocks) {
            Ok(()) => {
                let after = read_blocks(store);
                let verified = matches!(&after, Ok(after) if verify(&Value::Sequence(after.blocks.clone())));
                if verified {
                    return Ok(result);
                }
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!(
                            "{} lost-update: the write to {} was accepted but reading it back does not show what this dispatch wrote -- the row is NOT in the store, so this dispatch reports no success instead of a false ok",
                            verb, store.dir
                        ),
                        1,
                    ));
                }
                continue;
            }
            Err(WriteErr::Conflict) => {
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!(
                            "{} CAS failed after {} attempts: a concurrent writer keeps changing {}",
                            verb, max_attempts, store.dir
                        ),
                        1,
                    ));
                }
                continue;
            }
            Err(WriteErr::Fatal(message)) => {
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!("{} write failed after {} attempts: {}", verb, max_attempts, message),
                        1,
                    ));
                }
                continue;
            }
        }
    }
}

pub fn item_blocks(store: &Store, id: &str) -> Vec<Value> {
    let path = item_path(store, id);
    match pkfs::read_to_string(&path) {
        Some(text) => parse_lenient(&text),
        None => Vec::new(),
    }
}

pub fn remove_item(store: &Store, id: &str) -> bool {
    pkfs::remove_file(&item_path(store, id))
}

pub fn log_entries(store: &Store) -> Vec<serde_json::Value> {
    let Some(raw) = pkfs::read_to_string(&store.log) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            out.push(entry);
        }
    }
    out
}

fn resolved_stems(store: &Store) -> BTreeSet<String> {
    log_entries(store)
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|v| v.as_str()))
        .map(item_stem)
        .collect()
}

pub fn log_row(store: &Store, id: &str) -> Option<serde_json::Value> {
    let mut found: Option<serde_json::Value> = None;
    for entry in log_entries(store) {
        if entry.get("id").and_then(|v| v.as_str()) == Some(id) {
            found = Some(entry);
        }
    }
    let entry = found?;
    let mut row = match entry.get("row") {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map.clone()),
        _ => serde_json::json!({}),
    };
    if let Some(map) = row.as_object_mut() {
        map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
        map.insert(
            "status".to_string(),
            entry
                .get("status")
                .cloned()
                .unwrap_or(serde_json::Value::String("completed".to_string())),
        );
        map.insert("resolved".to_string(), serde_json::Value::Bool(true));
        map.insert(
            "source".to_string(),
            serde_json::Value::String(format!(".gm/{}", store.kind.log_name())),
        );
        if let Some(ts) = entry.get("ts") {
            map.insert("resolved_at".to_string(), ts.clone());
        }
    }
    Some(row)
}

pub fn log_rows_with_status(store: &Store, statuses: &[String]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for entry in log_entries(store) {
        let status = entry
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !statuses.iter().any(|want| want == &status) {
            continue;
        }
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(row) = log_row(store, id) {
            out.push(row);
        }
    }
    out
}

fn iso_ms(ms: u128) -> String {
    let secs = (ms / 1000) as i64;
    if secs <= 0 {
        return "1970-01-01T00:00:00.000Z".to_string();
    }
    let millis = (ms % 1000) as u32;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        millis
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn append_log(store: &Store, mut entry: serde_json::Value) -> Result<serde_json::Value, String> {
    let ts_ms = super::state::now_ms();
    {
        let Some(map) = entry.as_object_mut() else {
            return Err("commit-log entry must be a JSON object".to_string());
        };
        map.insert("v".to_string(), serde_json::json!(LOG_VERSION));
        map.insert("kind".to_string(), serde_json::json!(store.kind.label()));
        map.insert("ts_ms".to_string(), serde_json::json!(ts_ms as u64));
        map.insert("ts".to_string(), serde_json::json!(iso_ms(ts_ms)));
        if let Some(session) = super::state::dispatch_session_id() {
            if !map.contains_key("session_id") {
                map.insert("session_id".to_string(), serde_json::json!(session));
            }
        }
    }
    for attempt in 0..super::fsm::graph().policy.cas_max_attempts.max(1) {
        let before = pkfs::read_to_string(&store.log).unwrap_or_default();
        let prev = before
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(|line| sha256_hex(line.trim()))
            .unwrap_or_else(|| "0".repeat(64));
        entry["prev"] = serde_json::json!(prev);
        let line = {
            let mut probe = entry.clone();
            if let Some(map) = probe.as_object_mut() {
                map.remove("sha256");
            }
            let body = serde_json::to_string(&probe).map_err(|e| e.to_string())?;
            let hash = sha256_hex(&body);
            entry["sha256"] = serde_json::json!(hash);
            serde_json::to_string(&entry).map_err(|e| e.to_string())?
        };
        let after = if before.is_empty() || before.ends_with('\n') {
            format!("{}{}\n", before, line)
        } else {
            format!("{}\n{}\n", before, line)
        };
        match pkfs::cas_write(&store.log, &before, &after) {
            pkfs::CasWriteOutcome::Swapped => return Ok(entry),
            pkfs::CasWriteOutcome::Mismatch => continue,
            pkfs::CasWriteOutcome::IoError => {
                if attempt + 1 >= super::fsm::graph().policy.cas_max_attempts.max(1) {
                    return Err(format!("append to {} failed", store.log));
                }
            }
        }
    }
    Err(format!("append to {} failed: concurrent writer", store.log))
}

pub fn retire_to_log(
    store: &Store,
    id: &str,
    status: &str,
    note: Option<&str>,
) -> Result<serde_json::Value, String> {
    let blocks = item_blocks(store, id);
    if blocks.is_empty() {
        return Err(format!("no item file for {} to retire", id));
    }
    let rows: Vec<serde_json::Value> = blocks.iter().map(yaml_to_json).collect();
    let entry = serde_json::json!({
        "id": id,
        "status": status,
        "file": format!("{}/{}{}", store.kind.dir_rel(), item_stem(id), ITEM_SUFFIX),
        "note": note,
        "blocks": rows.len(),
        "row": rows.last().cloned().unwrap_or(serde_json::Value::Null),
        "material": rows,
    });
    let entry = append_log(store, entry)?;
    remove_item(store, id);
    Ok(entry)
}
