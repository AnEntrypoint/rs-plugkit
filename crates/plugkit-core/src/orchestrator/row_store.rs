use crate::pkfs;
use yaml_serde::Value;
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
            Kind::Mutables => "unknowns",
        }
    }
    fn legacy_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd.yml",
            Kind::Mutables => "mutables.yml",
        }
    }
    fn legacy_dir_name(&self) -> Option<&'static str> {
        match self {
            Kind::Prd => None,
            Kind::Mutables => Some("mutables"),
        }
    }
    fn legacy_log_name(&self) -> Option<&'static str> {
        match self {
            Kind::Prd => None,
            Kind::Mutables => Some("mutables-log.jsonl"),
        }
    }
    fn log_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd-log.jsonl",
            Kind::Mutables => "unknowns-log.jsonl",
        }
    }
    fn log_dir_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd-log.d",
            Kind::Mutables => "unknowns-log.d",
        }
    }
    fn marker_name(&self) -> &'static str {
        match self {
            Kind::Prd => "prd.absorbed.json",
            Kind::Mutables => "unknowns.absorbed.json",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Prd => "prd",
            Kind::Mutables => "unknowns",
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
    pub legacy_dir: Option<String>,
    pub legacy_log: Option<String>,
    pub log: String,
    pub log_dir: String,
    log_dir_marker: String,
    marker: String,
    dir_migration_marker: Option<String>,
}

fn legacy_dir_path(gm: &str, kind: &Kind) -> Option<String> {
    kind.legacy_dir_name()
        .map(|name| format!("{}/{}", gm, name))
}

fn legacy_log_path(gm: &str, kind: &Kind) -> Option<String> {
    kind.legacy_log_name()
        .map(|name| format!("{}/{}", gm, name))
}

impl Store {
    fn build(kind: Kind, gm: String) -> Self {
        let dir_migration_marker = kind
            .legacy_dir_name()
            .map(|_| format!("{}/{}.dir-migrated.json", gm, kind.dir_name()));
        Self {
            dir: format!("{}/{}", gm, kind.dir_name()),
            legacy: format!("{}/{}", gm, kind.legacy_name()),
            legacy_dir: legacy_dir_path(&gm, &kind),
            legacy_log: legacy_log_path(&gm, &kind),
            log: format!("{}/{}", gm, kind.log_name()),
            log_dir: format!("{}/{}", gm, kind.log_dir_name()),
            log_dir_marker: format!("{}/{}.log-dir-migrated.json", gm, kind.dir_name()),
            marker: format!("{}/{}", gm, kind.marker_name()),
            dir_migration_marker,
            kind,
        }
    }
    pub fn new(kind: Kind) -> Self {
        let gm = super::gm_dir().to_string_lossy().replace('\\', "/");
        Self::build(kind, gm)
    }
    pub fn prd() -> Self {
        Self::new(Kind::Prd)
    }
    pub fn unknowns() -> Self {
        Self::new(Kind::Mutables)
    }
    pub fn for_cwd(kind: Kind, cwd: Option<&str>) -> Self {
        match cwd {
            Some(c) if !c.trim().is_empty() => {
                let root = c.trim().trim_end_matches(['/', '\\']).replace('\\', "/");
                Self::build(kind, format!("{}/.gm", root))
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
        let hash_suffix = format!("-{:08x}", fnv1a64(trimmed));
        let head_len = ITEM_STEM_MAX_CHARS - hash_suffix.len();
        format!(
            "{}{}",
            suffixed.chars().take(head_len).collect::<String>(),
            hash_suffix
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
    let doc: Value = yaml_serde::from_str(source).map_err(|e| {
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
    match yaml_serde::from_str::<Value>(source) {
        Ok(Value::Sequence(seq)) => seq,
        Ok(other) => vec![other],
        Err(_) => Vec::new(),
    }
}

fn merge_blocks(existing: Vec<Value>, incoming: Vec<Value>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(existing.len() + incoming.len());
    for block in existing {
        seen.insert(yaml_serde::to_string(&block).unwrap_or_default());
        out.push(block);
    }
    for block in incoming {
        let key = yaml_serde::to_string(&block).unwrap_or_default();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(block);
    }
    out
}

fn migrate_legacy_dir(store: &Store) {
    let Some(marker) = store.dir_migration_marker.as_deref() else {
        return;
    };
    if pkfs::exists(marker) {
        return;
    }
    let mut landed = true;
    if let Some(legacy_dir) = store.legacy_dir.as_deref() {
        if let Some(serde_json::Value::Array(entries)) = pkfs::readdir(legacy_dir) {
            for entry in entries {
                let Some(name) = entry.as_str() else { continue };
                if !name.ends_with(ITEM_SUFFIX) {
                    continue;
                }
                let to = format!("{}/{}", store.dir, name);
                if pkfs::exists(&to) {
                    continue;
                }
                let Some(raw) = pkfs::read_to_string(&format!("{}/{}", legacy_dir, name)) else {
                    continue;
                };
                if !pkfs::write(&to, &raw) {
                    landed = false;
                }
            }
        }
    }
    if let Some(legacy_log) = store.legacy_log.as_deref() {
        let current = pkfs::read_to_string(&store.log).unwrap_or_default();
        if current.trim().is_empty() {
            if let Some(raw) = pkfs::read_to_string(legacy_log) {
                if !raw.trim().is_empty() && !pkfs::write(&store.log, &raw) {
                    landed = false;
                }
            }
        }
    }
    if landed {
        let _ = pkfs::write(marker, "{\"legacy_dir_migrated\":true}");
    }
}

pub fn maybe_absorb(store: &Store) -> Result<(), String> {
    migrate_legacy_dir(store);
    if marker_matches(store) {
        return Ok(());
    }
    let Some(legacy_raw) = pkfs::read_to_string(&store.legacy) else {
        write_marker(store, None);
        return Ok(());
    };
    let legacy_text = legacy_raw.strip_prefix(BOM).unwrap_or(legacy_raw.as_str());
    if !legacy_text.trim().is_empty() {
        let legacy_blocks = parse_lenient(legacy_text);
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for block in legacy_blocks {
            let stem = item_stem(block_id(&block).unwrap_or_default().as_str());
            groups.entry(stem).or_default().push(block);
        }
        let resolved = resolved_stems(store)?;
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
            return Ok(());
        }
    }
    write_marker(store, legacy_stat(store));
    Ok(())
}

pub type SkippedRow = (String, String);

pub fn read_blocks(store: &Store) -> Result<ReadSet, (String, i32)> {
    read_blocks_mode(store, false).map(|(set, _)| set)
}

pub fn read_blocks_listing(store: &Store) -> Result<(ReadSet, Vec<SkippedRow>), (String, i32)> {
    read_blocks_mode(store, true)
}

fn read_blocks_mode(
    store: &Store,
    skip_unreadable: bool,
) -> Result<(ReadSet, Vec<SkippedRow>), (String, i32)> {
    migrate_legacy_dir(store);
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    let mut blocks: Vec<Value> = Vec::new();
    let mut skipped: Vec<SkippedRow> = Vec::new();
    let mut present: BTreeSet<String> = BTreeSet::new();
    for stem in listing_stems(store) {
        let path = item_path_by_stem(store, &stem);
        present.insert(stem.clone());
        let Some(text) = pkfs::read_to_string(&path) else {
            let message = format!("{} exists but could not be read", path);
            if skip_unreadable {
                skipped.push((path, message));
                continue;
            }
            return Err((message, 1));
        };
        let parsed = match parse_blocks(&path, &text) {
            Ok(parsed) => parsed,
            Err((message, _)) if skip_unreadable => {
                skipped.push((path, message));
                continue;
            }
            Err(error) => return Err(error),
        };
        raw.insert(stem, text);
        blocks.extend(parsed);
    }
    if !marker_matches(store) {
        if let Some(legacy_raw) = pkfs::read_to_string(&store.legacy) {
            let legacy_text = legacy_raw.strip_prefix(BOM).unwrap_or(legacy_raw.as_str());
            if !legacy_text.trim().is_empty() {
                let parsed = parse_blocks(&store.legacy, legacy_text)?;
                let resolved = resolved_stems(store).map_err(|message| (message, 1))?;
                for block in parsed {
                    let stem = item_stem(block_id(&block).unwrap_or_default().as_str());
                    if present.contains(&stem) || resolved.contains(&stem) {
                        continue;
                    }
                    blocks.push(block);
                }
            }
        }
    }
    Ok((ReadSet { blocks, raw }, skipped))
}

pub enum WriteErr {
    Conflict,
    Fatal(String),
    NoSpace(String),
}

const NO_SPACE_MARKER: &str = "no-space:";

pub fn is_no_space(err: &str) -> bool {
    err.contains(NO_SPACE_MARKER)
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
            pkfs::CasWriteOutcome::NoSpace => return Err(WriteErr::NoSpace(path)),
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
    if let Err(message) = maybe_absorb(store) {
        return Err((String::new(), format!("{} refused: {}", verb, message), 1));
    }
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
            Err(WriteErr::NoSpace(path)) => {
                return Err((
                    serde_json::json!({
                        "ok": false,
                        "error_code": "storage_full",
                        "error": format!("{}: no space left on the volume holding {}", verb, path),
                        "path": path,
                        "retried": false,
                    })
                    .to_string(),
                    String::new(),
                    1,
                ));
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

fn quarantine_torn_tail(store: &Store, raw: &str, keep: usize) {
    let sidecar = format!("{}.torn", store.log);
    if !pkfs::append(&sidecar, &format!("{}\n", &raw[keep..])) {
        return;
    }
    if matches!(
        pkfs::cas_write(&store.log, raw, &raw[..keep]),
        pkfs::CasWriteOutcome::Swapped
    ) {
        bump_torn_count(store);
    }
}

pub struct LogRead {
    pub entries: Vec<serde_json::Value>,
    pub dropped_torn_lines: usize,
}

// The log is one append-only chained file and grows with every resolution, so a lookup that needs one
// row would otherwise parse every row ever resolved. <log>.d holds one file per id carrying that id's
// newest entry; a readdir of the directory is the index, so a lookup reads one small file and a listing
// reads the directory plus the page it names. The log stays the chained record of record.
pub const LOG_COMPACT_BYTES: usize = 4 * 1024 * 1024;

// What a compaction keeps. It is well under LOG_COMPACT_BYTES so the next compaction is far away:
// compacting down to the trigger point would make every following resolve pay for a full rewrite.
const LOG_RETAIN_BYTES: usize = 2 * 1024 * 1024;

fn torn_count_path(store: &Store) -> String {
    format!("{}.torn-count.json", store.log)
}

pub fn log_torn_count(store: &Store) -> usize {
    let raw = pkfs::read_to_string(&torn_count_path(store)).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.get("dropped").and_then(|v| v.as_u64()))
        .unwrap_or(0) as usize
}

fn bump_torn_count(store: &Store) {
    let next = log_torn_count(store).saturating_add(1);
    let _ = pkfs::write(
        &torn_count_path(store),
        &serde_json::json!({ "dropped": next }).to_string(),
    );
}

fn log_sidecar_path(store: &Store, id: &str) -> String {
    format!("{}/{}.json", store.log_dir, item_stem(id))
}

fn write_log_sidecar(store: &Store, id: &str, entry: &serde_json::Value) -> bool {
    let Ok(text) = serde_json::to_string(entry) else {
        return false;
    };
    pkfs::write(&log_sidecar_path(store, id), &text)
}

fn read_log_sidecar(store: &Store, id: &str) -> Option<serde_json::Value> {
    let raw = pkfs::read_to_string(&log_sidecar_path(store, id))?;
    serde_json::from_str::<serde_json::Value>(raw.trim()).ok()
}

// The directory listing is the index. Names are sorted so a page is stable across calls.
fn log_sidecar_names(store: &Store) -> Vec<String> {
    let Some(serde_json::Value::Array(names)) = pkfs::readdir(&store.log_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = names
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|name| name.strip_suffix(".json"))
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

fn log_sidecar_entry_by_stem(store: &Store, stem: &str) -> Option<serde_json::Value> {
    let raw = pkfs::read_to_string(&format!("{}/{}.json", store.log_dir, stem))?;
    serde_json::from_str::<serde_json::Value>(raw.trim()).ok()
}

// One-time split of the historical single-file log into the per-id directory.
fn migrate_log_to_dir(store: &Store) {
    if pkfs::exists(&store.log_dir_marker) {
        return;
    }
    let Ok(entries) = log_entries(store) else {
        return;
    };
    let mut landed = true;
    for entry in &entries {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if !write_log_sidecar(store, id, entry) {
            landed = false;
        }
    }
    if !pkfs::exists(&store.log_dir) {
        landed = false;
    }
    if landed {
        let _ = pkfs::write(&store.log_dir_marker, "{\"log_dir_migrated\":true}");
    }
}

// Keeps the newest entry per id and rewrites the log as one fresh chain, so the file stays under
// LOG_COMPACT_BYTES while every resolved id stays citable. Entries keep their id, status, ts and
// material; the chain digests are recomputed because they cover the surviving chain.
// Which prev rule the log on disk uses. 1 chained each entry to the previous entry's body digest;
// 2 chains to the sha256 of the previous line, which is what append_chained writes. A log written
// under 1 is rechained once instead of carrying two conventions.
const LOG_CHAIN_VERSION: u32 = 2;

fn rechain_marker(store: &Store) -> String {
    format!("{}.chain-version.json", store.log)
}

fn chain_is_current(store: &Store) -> bool {
    let raw = pkfs::read_to_string(&rechain_marker(store)).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.get("chain_version").and_then(|v| v.as_u64()))
        .is_some_and(|version| version >= LOG_CHAIN_VERSION as u64)
}

pub fn compact_log(store: &Store) -> Result<bool, String> {
    compact_log_inner(store, false)
}

pub fn compact_log_inner(store: &Store, force: bool) -> Result<bool, String> {
    let Some(raw) = pkfs::read_to_string(&store.log) else {
        return Ok(false);
    };
    if !force && raw.len() <= LOG_COMPACT_BYTES {
        return Ok(false);
    }
    let entries = log_entries(store)?;
    // Newest entry per id, kept at its chronological position.
    let mut seen: HashSet<String> = HashSet::new();
    let mut keep: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate().rev() {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if seen.insert(id.to_string()) {
            keep.push(index);
        }
    }
    keep.reverse();
    let mut lines: Vec<String> = Vec::new();
    let mut prev = "0".repeat(64);
    for index in &keep {
        let mut entry = entries[*index].clone();
        entry["prev"] = serde_json::json!(prev);
        if let Some(map) = entry.as_object_mut() {
            map.remove("sha256");
        }
        let body = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        let hash = sha256_hex(&body);
        entry["sha256"] = serde_json::json!(hash);
        let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        // prev is the sha256 of the previous line, which is what append_chained writes, so a
        // compacted log and an appended log chain by the same rule.
        prev = sha256_hex(&line);
        lines.push(line);
    }
    // The most recent suffix that still fits stays in the active log; the older part moves to a
    // segment file. Every resolved id keeps its sidecar, so nothing becomes uncitable.
    let mut retained_bytes = 0usize;
    let mut split = lines.len();
    for (at, line) in lines.iter().enumerate().rev() {
        if retained_bytes + line.len() + 1 > LOG_RETAIN_BYTES {
            split = at + 1;
            break;
        }
        retained_bytes += line.len() + 1;
        split = at;
    }
    if split > 0 {
        let mut rotated = String::new();
        for line in &lines[..split] {
            rotated.push_str(line);
            rotated.push('\n');
        }
        let segment = format!("{}.seg-{}.jsonl", store.log, super::state::now_ms());
        if !pkfs::write(&segment, &rotated) {
            return Ok(false);
        }
    }
    // The retained suffix is rechained from the origin, so the active log is a standalone chain and
    // does not point into what moved out.
    let mut out = String::new();
    let mut retained_prev = "0".repeat(64);
    for (_slot, index) in keep.iter().enumerate().skip(split) {
        let mut entry = entries[*index].clone();
        entry["prev"] = serde_json::json!(retained_prev);
        if let Some(map) = entry.as_object_mut() {
            map.remove("sha256");
        }
        let body = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        let hash = sha256_hex(&body);
        entry["sha256"] = serde_json::json!(hash);
        let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        retained_prev = sha256_hex(&line);
        out.push_str(&line);
        out.push('\n');
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            let _ = write_log_sidecar(store, id, &entry);
        }
    }
    for (slot, index) in keep.iter().enumerate().take(split) {
        let Some(id) = entries[*index].get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&lines[slot]) {
            let _ = write_log_sidecar(store, id, &entry);
        }
    }
    if !matches!(
        pkfs::cas_write(&store.log, &raw, &out),
        pkfs::CasWriteOutcome::Swapped
    ) {
        return Ok(false);
    }
    Ok(true)
}

pub fn log_compacted_bytes(store: &Store) -> usize {
    pkfs::read_to_string(&store.log).map(|raw| raw.len()).unwrap_or(0)
}

pub fn read_log(store: &Store) -> Result<LogRead, String> {
    let Some(raw) = pkfs::read_to_string(&store.log) else {
        return Ok(LogRead {
            entries: Vec::new(),
            dropped_torn_lines: 0,
        });
    };
    let mut body: &str = &raw;
    let mut dropped_torn_lines = 0;
    if !raw.is_empty() && !raw.ends_with('\n') {
        let keep = raw.rfind('\n').map_or(0, |i| i + 1);
        let torn = raw[keep..].trim();
        if !torn.is_empty() && serde_json::from_str::<serde_json::Value>(torn).is_err() {
            quarantine_torn_tail(store, &raw, keep);
            body = &raw[..keep];
            dropped_torn_lines = 1;
        }
    }
    let mut entries = Vec::new();
    for (index, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(entry) => {
                let version = entry.get("v").and_then(|v| v.as_u64()).unwrap_or(0);
                if version > LOG_VERSION as u64 {
                    return Err(format!(
                        "prd_log_unknown_version: {} line {} has entry version {} but this build reads version {}; upgrade the runner before reading the log",
                        store.log,
                        index + 1,
                        version,
                        LOG_VERSION
                    ));
                }
                entries.push(entry)
            }
            Err(_) => {
                return Err(format!(
                    "prd_log_corrupt_line: {} line {} is not valid JSON; repair or remove that line before reading the log",
                    store.log,
                    index + 1
                ))
            }
        }
    }
    Ok(LogRead {
        entries,
        dropped_torn_lines,
    })
}

pub fn log_entries(store: &Store) -> Result<Vec<serde_json::Value>, String> {
    read_log(store).map(|log| log.entries)
}

fn resolved_stems(store: &Store) -> Result<BTreeSet<String>, String> {
    if let Some(names) = sidecar_resolved_stems(store) {
        return Ok(names);
    }
    Ok(log_entries(store)?
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|v| v.as_str()))
        .map(item_stem)
        .collect())
}

// Readdir of the sidecar directory answers "which ids are already resolved" without opening the log.
// A directory that does not exist yet is unknown, not empty, so the caller falls back to the log.
fn sidecar_resolved_stems(store: &Store) -> Option<BTreeSet<String>> {
    if pkfs::readdir(&store.log_dir).is_none() {
        return None;
    }
    let mut out = BTreeSet::new();
    for stem in log_sidecar_names(store) {
        if let Some(entry) = log_sidecar_entry_by_stem(store, &stem) {
            if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                out.insert(item_stem(id));
            }
        }
    }
    Some(out)
}

pub fn log_row(store: &Store, id: &str) -> Result<Option<serde_json::Value>, String> {
    if let Some(entry) = read_log_sidecar(store, id) {
        return Ok(Some(log_row_from_entry(store, id, &entry)));
    }
    let mut found: Option<serde_json::Value> = None;
    for entry in log_entries(store)? {
        if entry.get("id").and_then(|v| v.as_str()) == Some(id) {
            found = Some(entry);
        }
    }
    Ok(found.map(|entry| log_row_from_entry(store, id, &entry)))
}

// Reads at most `limit` sidecar files starting at `offset` of the sorted id listing, so a listing
// costs the directory index plus its own page and never the whole log. `limit` None means every
// matching row; every resolved id stays reachable through log_row.
pub fn log_rows_with_status_paged(
    store: &Store,
    statuses: &[String],
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let names = log_sidecar_names(store);
    if !names.is_empty() {
        let window: Vec<&String> = match limit {
            Some(n) => names.iter().skip(offset).take(n).collect(),
            None => names.iter().skip(offset).collect(),
        };
        let mut out = Vec::new();
        for stem in window {
            let Some(entry) = log_sidecar_entry_by_stem(store, stem) else {
                continue;
            };
            let status = entry
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !statuses.iter().any(|want| want == &status) {
                continue;
            }
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(stem.as_str())
                .to_string();
            out.push(log_row_from_entry(store, &id, &entry));
        }
        return Ok(out);
    }
    Ok(log_rows_from_entries(store, &log_entries(store)?, statuses))
}

fn log_row_from_entry(store: &Store, id: &str, entry: &serde_json::Value) -> serde_json::Value {
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
    row
}

pub fn log_rows_with_status(
    store: &Store,
    statuses: &[String],
) -> Result<Vec<serde_json::Value>, String> {
    log_rows_with_status_paged(store, statuses, 0, None)
}

fn log_rows_from_entries(
    store: &Store,
    entries: &[serde_json::Value],
    statuses: &[String],
) -> Vec<serde_json::Value> {
    let mut newest: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for entry in entries {
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            newest.insert(id, entry);
        }
    }
    let mut out = Vec::new();
    for entry in entries {
        let status = entry
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !statuses.iter().any(|want| want == &status) {
            continue;
        }
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(last) = newest.get(id) {
            out.push(log_row_from_entry(store, id, last));
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

pub fn append_log(store: &Store, entry: serde_json::Value) -> Result<serde_json::Value, String> {
    migrate_log_to_dir(store);
    let id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let entry = append_chained(&store.log, store.kind.label(), entry)?;
    if let Some(id) = id {
        let _ = write_log_sidecar(store, &id, &entry);
    }
    if !chain_is_current(store) {
        if compact_log_inner(store, true).unwrap_or(false) {
            let _ = pkfs::write(
                &rechain_marker(store),
                &serde_json::json!({ "chain_version": LOG_CHAIN_VERSION }).to_string(),
            );
        }
    }
    if log_compacted_bytes(store) > LOG_COMPACT_BYTES {
        let _ = compact_log(store);
    }
    Ok(entry)
}

pub fn append_chained(
    log_path: &str,
    kind_label: &str,
    mut entry: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let ts_ms = super::state::now_ms();
    {
        let Some(map) = entry.as_object_mut() else {
            return Err("commit-log entry must be a JSON object".to_string());
        };
        map.insert("v".to_string(), serde_json::json!(LOG_VERSION));
        map.insert("kind".to_string(), serde_json::json!(kind_label));
        map.insert("ts_ms".to_string(), serde_json::json!(ts_ms as u64));
        map.insert("ts".to_string(), serde_json::json!(iso_ms(ts_ms)));
        if let Some(session) = super::state::dispatch_session_id() {
            if !map.contains_key("session_id") {
                map.insert("session_id".to_string(), serde_json::json!(session));
            }
        }
    }
    for attempt in 0..super::fsm::graph().policy.cas_max_attempts.max(1) {
        let before = pkfs::read_to_string(log_path).unwrap_or_default();
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
        match pkfs::cas_write(log_path, &before, &after) {
            pkfs::CasWriteOutcome::Swapped => return Ok(entry),
            pkfs::CasWriteOutcome::Mismatch => continue,
            pkfs::CasWriteOutcome::NoSpace => {
                return Err(format!(
                    "{}append to {} failed: no space left on the volume holding it",
                    NO_SPACE_MARKER, log_path
                ))
            }
            pkfs::CasWriteOutcome::IoError => {
                if attempt + 1 >= super::fsm::graph().policy.cas_max_attempts.max(1) {
                    return Err(format!("append to {} failed", log_path));
                }
            }
        }
    }
    Err(format!("append to {} failed: concurrent writer", log_path))
}

pub fn latest_in(entries: &[serde_json::Value], id: &str) -> Option<serde_json::Value> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(id))
        .cloned()
}

pub fn latest_entry(store: &Store, id: &str) -> Result<Option<serde_json::Value>, String> {
    Ok(latest_in(&log_entries(store)?, id))
}

pub fn is_stale_entry(entry: &serde_json::Value) -> bool {
    let stale_text = |text: Option<&str>| {
        text.is_some_and(|t| t.trim().to_ascii_uppercase().starts_with("STALE"))
    };
    entry
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("stale"))
        || stale_text(entry.get("note").and_then(|v| v.as_str()))
        || stale_text(
            entry
                .get("row")
                .and_then(|row| row.get("resolution"))
                .and_then(|v| v.as_str()),
        )
}

pub struct Authorized {
    by: &'static str,
}

impl Authorized {
    pub fn by(&self) -> &'static str {
        self.by
    }
}

pub fn authorize_correction(
    prior: &serde_json::Value,
    dispatch: Option<&serde_json::Value>,
    resolver_session: Option<&str>,
    binding_verified: bool,
    has_evidence: bool,
) -> Result<Authorized, String> {
    if !is_stale_entry(prior) {
        return Err("the latest resolution is not stale, so the row takes no correction".to_string());
    }
    if !has_evidence {
        return Err("no witness_evidence text was supplied".to_string());
    }
    let row_session = prior
        .get("row")
        .and_then(|row| row.get("session_id"))
        .and_then(|v| v.as_str());
    let dispatch_session = dispatch
        .and_then(|entry| entry.get("session_id"))
        .and_then(|v| v.as_str());
    if dispatch_session.is_some()
        && dispatch_session != resolver_session
        && dispatch_session != row_session
    {
        return Ok(Authorized { by: "ledger" });
    }
    if binding_verified {
        return Ok(Authorized { by: "binding" });
    }
    Err(match dispatch {
        Some(_) => "the witness_dispatch_id is in the ledger but has no session_id, or ran under the resolver's or the executor's session, and no verified witness_output_path matched its witness_output_sha256".to_string(),
        None => "no witness_dispatch_id in the ledger under a session other than the resolver's, and no verified witness_output_path matched its witness_output_sha256".to_string(),
    })
}

pub fn correction_entry(
    prior: &serde_json::Value,
    id: &str,
    status: &str,
    mut row: serde_json::Value,
    authorized: &Authorized,
) -> Result<serde_json::Value, String> {
    if !is_stale_entry(prior) {
        return Err(format!("{} is already resolved and its latest resolution is not stale", id));
    }
    let corrects = serde_json::json!({
        "sha256": prior.get("sha256").cloned(),
        "ts": prior.get("ts").cloned(),
        "status": prior.get("status").cloned(),
        "note": prior.get("note").cloned(),
    });
    let note = row.get("resolution").cloned();
    if let Some(map) = row.as_object_mut() {
        map.insert("corrects".to_string(), corrects.clone());
        map.insert("witnessed_by".to_string(), serde_json::json!(authorized.by));
    }
    Ok(serde_json::json!({
        "id": id,
        "status": status,
        "note": note,
        "blocks": 0,
        "row": row,
        "material": [],
        "corrects": corrects,
    }))
}

pub fn append_correction(
    store: &Store,
    id: &str,
    status: &str,
    row: serde_json::Value,
    authorized: &Authorized,
) -> Result<serde_json::Value, String> {
    let prior = latest_entry(store, id)?
        .ok_or_else(|| format!("{} has no logged resolution to correct", id))?;
    let entry = correction_entry(&prior, id, status, row, authorized)?;
    append_log(store, entry)
}

pub fn now_iso() -> String {
    iso_ms(super::state::now_ms())
}

pub fn retire_to_log(
    store: &Store,
    id: &str,
    status: &str,
    note: Option<&str>,
) -> Result<serde_json::Value, String> {
    close_to_log(store, id, status, note, serde_json::Value::Null)
}

pub fn close_to_log(
    store: &Store,
    id: &str,
    status: &str,
    note: Option<&str>,
    closure: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let blocks = item_blocks(store, id);
    if blocks.is_empty() {
        return Err(format!("no item file for {} to retire", id));
    }
    let rows: Vec<serde_json::Value> = blocks.iter().map(yaml_to_json).collect();
    let extra = closure.as_object().cloned().unwrap_or_default();
    let mut row = rows.last().cloned().unwrap_or(serde_json::Value::Null);
    if let Some(map) = row.as_object_mut() {
        for (key, value) in &extra {
            map.insert(key.clone(), value.clone());
        }
    }
    let mut entry = serde_json::json!({
        "id": id,
        "status": status,
        "file": format!("{}/{}{}", store.kind.dir_rel(), item_stem(id), ITEM_SUFFIX),
        "note": note,
        "blocks": rows.len(),
        "row": row,
        "material": rows,
    });
    if let Some(map) = entry.as_object_mut() {
        for (key, value) in extra {
            map.insert(key, value);
        }
    }
    let entry = append_log(store, entry)?;
    if !remove_item(store, id) && pkfs::exists(&item_path(store, id)) {
        return Err(format!(
            "{} is logged as {} but its item file {} could not be removed, so prd-list would keep it open",
            id,
            status,
            item_path(store, id)
        ));
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::item_stem;

    #[test]
    fn item_stem_keeps_plain_ids_verbatim() {
        assert_eq!(item_stem("row-1"), "row-1");
    }

    #[test]
    fn item_stem_separates_ids_that_fold_to_one_name() {
        assert_ne!(item_stem("a/b"), item_stem("a-b"));
        assert!(item_stem("a/b").starts_with("a-b-"));
    }

    const STALE_SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn stale_entry(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "status": "completed",
            "sha256": STALE_SHA,
            "ts": "2026-10-20T21:08:33.370Z",
            "note": "STALE: premise refuted by the source envelope",
            "blocks": 1,
            "row": {
                "id": id,
                "session_id": "drain-row",
                "witness": "STALE (refuted premise)",
                "resolution": "STALE: premise refuted by the source envelope"
            }
        })
    }

    fn ledger_entry(session: &str) -> serde_json::Value {
        serde_json::json!({"dispatch_id": "1792609004526-950-d13b1980a322d0c", "session_id": session})
    }

    #[test]
    fn stale_row_accepts_witnessed_correction_and_keeps_stale_entry() {
        let mut entries = vec![stale_entry("row-x")];
        let prior = super::latest_in(&entries, "row-x").unwrap();
        let dispatch = ledger_entry("verify-row-x");
        let authorized = super::authorize_correction(&prior, Some(&dispatch), Some("resolve-row-x"), false, true).unwrap();
        assert_eq!(authorized.by(), "ledger");
        let row = serde_json::json!({"id": "row-x", "witness": "replay run output", "resolution": "CORRECTION: live witness"});
        let entry = super::correction_entry(&prior, "row-x", "completed", row, &authorized).unwrap();
        entries.push(entry);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], stale_entry("row-x"));
        let latest = super::latest_in(&entries, "row-x").unwrap();
        assert_eq!(latest["row"]["witness"].as_str(), Some("replay run output"));
        assert_eq!(latest["corrects"]["sha256"].as_str(), Some(STALE_SHA));
        assert!(!super::is_stale_entry(&latest));
    }

    #[test]
    fn stale_row_refuses_correction_without_witness() {
        let entries = vec![stale_entry("row-y")];
        let prior = super::latest_in(&entries, "row-y").unwrap();
        assert!(super::authorize_correction(&prior, None, Some("resolve-row-y"), false, true).is_err());
        assert!(super::authorize_correction(&prior, None, Some("resolve-row-y"), false, false).is_err());
        let witnessed = ledger_entry("verify-row-y");
        assert!(super::authorize_correction(&prior, Some(&witnessed), Some("resolve-row-y"), false, false).is_err());
        let same_session = ledger_entry("resolve-row-y");
        assert!(super::authorize_correction(&prior, Some(&same_session), Some("resolve-row-y"), false, true).is_err());
        let executor = ledger_entry("drain-row");
        assert!(super::authorize_correction(&prior, Some(&executor), Some("resolve-row-y"), false, true).is_err());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn resolved_row_refuses_re_resolve_and_correction() {
        let entries = vec![serde_json::json!({
            "id": "row-z",
            "status": "completed",
            "sha256": STALE_SHA,
            "ts": "2026-10-20T21:08:33.370Z",
            "note": "fixed by commit abc1234",
            "blocks": 1,
            "row": {"id": "row-z", "session_id": "drain-row", "witness": "exec output", "resolution": "fixed by commit abc1234"}
        })];
        let prior = super::latest_in(&entries, "row-z").unwrap();
        assert!(!super::is_stale_entry(&prior));
        let witnessed = ledger_entry("verify-row-z");
        assert!(super::authorize_correction(&prior, Some(&witnessed), Some("resolve-row-z"), true, true).is_err());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn item_stem_maps_blank_ids_to_placeholder() {
        assert_eq!(item_stem("   "), "_no-id");
    }
}
