use serde_json::Value;
use std::collections::BTreeSet;

const TARGET_FIELDS: [&str; 7] = ["subject", "title", "why", "witness", "acceptance", "acceptance_criteria", "text"];
const TARGET_EXTENSIONS: [&str; 18] = [
    ".js", ".mjs", ".cjs", ".ts", ".jsx", ".tsx", ".json", ".md", ".html", ".css", ".glsl", ".wgsl", ".rs", ".toml", ".yml", ".yaml", ".sh", ".hf",
];

#[derive(Default)]
pub(super) struct WorktreeDirt {
    pub(super) unknown: bool,
    pub(super) entries: BTreeSet<String>,
    basenames: BTreeSet<String>,
}

impl WorktreeDirt {
    fn covers(&self, target: &str) -> bool {
        if !target.contains('/') {
            return self.basenames.contains(target);
        }
        let mut candidate = target;
        loop {
            if self.entries.contains(candidate) {
                return true;
            }
            match candidate.rfind('/') {
                Some(at) => candidate = &candidate[..at],
                None => return false,
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn porcelain_entry_paths(line: &str) -> Vec<String> {
    let Some(entry) = line.get(3..) else {
        return Vec::new();
    };
    entry
        .split(" -> ")
        .map(|side| side.trim().trim_matches('"').trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty())
        .collect()
}

#[cfg(target_arch = "wasm32")]
const DIRT_SNAPSHOT_PATH: &str = ".gm/pool/dirt-snapshot.json";

#[cfg(target_arch = "wasm32")]
const DIRT_SNAPSHOT_MAX_AGE_MS: u64 = 30_000;

#[cfg(target_arch = "wasm32")]
fn dirt_with_entries(unknown: bool, entries: BTreeSet<String>) -> WorktreeDirt {
    let basenames: BTreeSet<String> = entries
        .iter()
        .filter(|entry| !entry.starts_with(".gm/"))
        .filter_map(|entry| entry.rsplit('/').next())
        .map(str::to_string)
        .collect();
    WorktreeDirt { unknown, entries, basenames }
}

#[cfg(target_arch = "wasm32")]
fn stat_signature(path: &str) -> Option<String> {
    let stat = crate::pkfs::stat(path).filter(|value| !value.is_null())?;
    Some(format!("{}@{}", stat["size"], stat["mtimeMs"]))
}

#[cfg(target_arch = "wasm32")]
fn git_state_signature() -> Option<String> {
    let index = stat_signature(".git/index")?;
    let head_log = stat_signature(".git/logs/HEAD").unwrap_or_else(|| "none".to_string());
    Some(format!("{index}|{head_log}"))
}

#[cfg(target_arch = "wasm32")]
fn read_dirt_snapshot(signature: &str, now: u64) -> Option<WorktreeDirt> {
    let snapshot: Value = serde_json::from_str(&crate::pkfs::read_to_string(DIRT_SNAPSHOT_PATH)?).ok()?;
    if snapshot["signature"].as_str()? != signature {
        return None;
    }
    let taken = snapshot["taken_ms"].as_u64()?;
    if taken > now || now - taken > DIRT_SNAPSHOT_MAX_AGE_MS {
        return None;
    }
    let entries: BTreeSet<String> = snapshot["entries"]
        .as_array()?
        .iter()
        .filter_map(|entry| entry.as_str().map(str::to_string))
        .collect();
    Some(dirt_with_entries(false, entries))
}

#[cfg(target_arch = "wasm32")]
fn write_dirt_snapshot(signature: &str, taken: u64, dirt: &WorktreeDirt) {
    let snapshot = serde_json::json!({
        "signature": signature,
        "taken_ms": taken,
        "entries": dirt.entries.iter().collect::<Vec<&String>>(),
    });
    let _ = crate::pkfs::write(DIRT_SNAPSHOT_PATH, &snapshot.to_string());
}

#[cfg(target_arch = "wasm32")]
fn status_worktree_dirt() -> WorktreeDirt {
    let response = crate::wasm_dispatch::git_call_argv(&["status", "--porcelain", "-uall", "--"], None);
    if crate::wasm_dispatch::host_abi::git_response_is_not_repository(&response) {
        return WorktreeDirt::default();
    }
    let status = crate::wasm_dispatch::host_abi::porcelain_from(&response);
    let unknown = status.failed || status.parked || (status.partial && status.skipped_paths.is_empty());
    let mut entries: BTreeSet<String> = status
        .skipped_paths
        .iter()
        .map(|skipped| skipped.trim_end_matches('/').to_string())
        .collect();
    for line in status.porcelain.lines() {
        entries.extend(porcelain_entry_paths(line));
    }
    dirt_with_entries(unknown, entries)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn worktree_dirt() -> WorktreeDirt {
    let now = super::now_ms();
    let signature = git_state_signature();
    if let Some(cached) = signature.as_deref().and_then(|signature| read_dirt_snapshot(signature, now)) {
        return cached;
    }
    let dirt = status_worktree_dirt();
    if let Some(signature) = signature {
        if !dirt.unknown && git_state_signature().as_deref() == Some(signature.as_str()) {
            write_dirt_snapshot(&signature, now, &dirt);
        }
    }
    dirt
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn worktree_dirt() -> WorktreeDirt {
    WorktreeDirt::default()
}

#[cfg(target_arch = "wasm32")]
const FIXED_ON_HEAD_WINDOW: usize = 500;

#[cfg(target_arch = "wasm32")]
const SUBJECT_SEPARATOR: char = '\u{1f}';

#[cfg(target_arch = "wasm32")]
fn is_row_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

#[cfg(target_arch = "wasm32")]
fn is_witness_record(subject: &str) -> bool {
    subject.trim_start().to_ascii_lowercase().starts_with("witness")
}

#[cfg(target_arch = "wasm32")]
pub(super) fn fixed_on_head_shas(open_ids: &BTreeSet<&str>) -> std::collections::BTreeMap<String, String> {
    let mut shas = std::collections::BTreeMap::new();
    if open_ids.is_empty() {
        return shas;
    }
    let window = FIXED_ON_HEAD_WINDOW.to_string();
    let log = crate::wasm_dispatch::host_abi::porcelain_from(&crate::wasm_dispatch::git_call_argv(
        &["log", "--format=%H%x1f%s", "-n", window.as_str(), "HEAD", "--"],
        None,
    ));
    if log.failed || log.parked {
        return shas;
    }
    for line in log.porcelain.lines() {
        let Some((sha, subject)) = line.split_once(SUBJECT_SEPARATOR) else {
            continue;
        };
        if is_witness_record(subject) {
            continue;
        }
        for token in subject.split(|c: char| !is_row_id_char(c)) {
            if open_ids.contains(token) && !shas.contains_key(token) {
                shas.insert(token.to_string(), sha.to_string());
            }
        }
    }
    shas
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn fixed_on_head_shas(_open_ids: &BTreeSet<&str>) -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::new()
}

fn target_path_of(token: &str) -> Option<String> {
    let unquoted = token.trim_matches(|c: char| {
        matches!(c, '\'' | '"' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>')
    });
    let located = unquoted.split(|c: char| c == ':' || c == '#').next().unwrap_or_default();
    let path = located.trim_end_matches('.');
    let admissible = !path.starts_with('/')
        && !path.contains("..")
        && !path.starts_with(".gm/")
        && !path.chars().any(|c| c == '*' || c == '?')
        && TARGET_EXTENSIONS
            .iter()
            .any(|extension| path.len() > extension.len() && path.ends_with(*extension));
    admissible.then(|| path.to_string())
}

const SHARED_DOCUMENTS: [&str; 3] = ["AGENTS.md", "README.md", "CHANGELOG.md"];

fn is_shared_document(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    SHARED_DOCUMENTS.contains(&name)
}

fn named_target_paths(row: &Value) -> Vec<String> {
    let own_surface: Vec<String> = row
        .get("surface")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(str::split_whitespace)
        .filter_map(target_path_of)
        .collect();
    let mut paths: Vec<String> = TARGET_FIELDS
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_str))
        .flat_map(str::split_whitespace)
        .filter_map(target_path_of)
        .filter(|path| !is_shared_document(path))
        .chain(own_surface.into_iter().filter(|path| is_shared_document(path)))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

pub(super) fn writer_targets_of(row: &Value) -> Vec<String> {
    named_target_paths(row).into_iter().filter(|path| !path.contains(':')).collect()
}

pub(super) fn dirty_target_verdict(row: &Value, dirt: &WorktreeDirt) -> Option<(&'static str, &'static str, String)> {
    let targets = named_target_paths(row);
    if dirt.unknown {
        return targets
            .into_iter()
            .next()
            .map(|target| ("git_status_unknown", "target", target));
    }
    targets
        .into_iter()
        .find(|target| dirt.covers(target))
        .map(|target| ("dirty_target", "dirty_target", target))
}

pub(super) fn row_by_id<'a>(work: &'a [(Value, usize)], id: &str) -> Option<&'a Value> {
    work.iter()
        .map(|(row, _)| row)
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
}

pub fn slots_prose(slots: &Value) -> String {
    match slots["action"].as_str() {
        Some("none") => "No open PRD rows: launch no workers.".to_string(),
        Some("hold") => "All slots are full: wait for a completion and relaunch its replacement in the same turn.".to_string(),
        _ => match slots["free"].as_u64() {
            Some(free) => {
                let launch = free.min(slots["launchable"].as_u64().unwrap_or(0));
                format!("Free slots known: launch min(free, launchable) = {} gm-worker subagents now; pool-observe adds one traversal hop when traversal.needed is true.", launch)
            }
            None => "Ceiling unknown: launch until a spawn refusal, then call pool-observe with that refusal text.".to_string(),
        },
    }
}
