use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

use super::super::pool_rank;
use super::{LAUNCH_ID_PREFIX, MODULE_EXTENSIONS};
use crate::pkfs;

const LAUNCH_ID_EXCLUDED_SEGMENTS: [&str; 3] = ["-blocker-", "-finding-", "-defect-"];

fn module_path_of(row: &Value) -> Option<String> {
    ["subject", "witness", "why", "title", "acceptance", "acceptance_criteria"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_str))
        .flat_map(str::split_whitespace)
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | ',' | ';' | '(' | ')' | '[' | ']'))
                .trim_end_matches('.')
        })
        .map(|token| token.split(':').next().unwrap_or_default())
        .find(|token| {
            token.contains('/')
                && !token.starts_with('/')
                && !token.contains("..")
                && MODULE_EXTENSIONS.iter().any(|extension| token.ends_with(*extension))
        })
        .map(str::to_string)
}

fn node_only_witness(row: &Value) -> bool {
    ["subject", "witness", "why", "title", "acceptance", "acceptance_criteria", "text"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_str))
        .all(|text| !text.to_ascii_lowercase().contains("cargo"))
}

pub(super) fn witness_gap_refusal(row: &Value, project_root: &str) -> Option<String> {
    let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
    if !id.starts_with(LAUNCH_ID_PREFIX) {
        return Some("not_witness_gap_id".to_string());
    }
    if let Some(segment) = LAUNCH_ID_EXCLUDED_SEGMENTS.iter().find(|segment| id.contains(**segment)) {
        return Some(format!("excluded_segment:{}", segment.trim_matches('-')));
    }
    let status = row.get("status").and_then(Value::as_str).unwrap_or("pending");
    if status != "pending" {
        return Some(format!("status:{status}"));
    }
    if pool_rank::has_blocker_notes(row) {
        return Some("blocker_notes".to_string());
    }
    if !node_only_witness(row) {
        return Some("cargo_in_witness_text".to_string());
    }
    let Some(module) = module_path_of(row) else {
        return Some("no_module_path".to_string());
    };
    let Some(source) = pkfs::read_to_string(&format!("{}/{}", project_root, module)) else {
        return Some(format!("module_unreadable:{module}"));
    };
    pool_rank::browser_or_gpu_global_in(&source).map(|token| format!("browser_gpu_global:{token}"))
}

pub(super) fn witness_gap_admitted(row: &Value, project_root: &str) -> bool {
    witness_gap_refusal(row, project_root).is_none()
}

pub(super) fn node_only_module(row: &Value, project_root: &str) -> bool {
    module_path_of(row).is_some_and(|module| {
        pkfs::read_to_string(&format!("{}/{}", project_root, module))
            .is_some_and(|source| !pool_rank::references_browser_or_gpu_global(&source))
    })
}

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
pub(super) fn worktree_dirt() -> WorktreeDirt {
    let response = crate::wasm_dispatch::git_call_argv(&["status", "--porcelain", "-uall", "--"], None);
    if crate::wasm_dispatch::host_abi::git_response_is_not_repository(&response) {
        return WorktreeDirt::default();
    }
    let status = crate::wasm_dispatch::host_abi::porcelain_from(&response);
    let mut dirt = WorktreeDirt {
        unknown: status.failed || status.parked || (status.partial && status.skipped_paths.is_empty()),
        entries: BTreeSet::new(),
        basenames: BTreeSet::new(),
    };
    dirt.entries.extend(
        status
            .skipped_paths
            .iter()
            .map(|skipped| skipped.trim_end_matches('/').to_string()),
    );
    for line in status.porcelain.lines() {
        dirt.entries.extend(porcelain_entry_paths(line));
    }
    dirt.basenames = dirt
        .entries
        .iter()
        .filter(|entry| !entry.starts_with(".gm/"))
        .filter_map(|entry| entry.rsplit('/').next())
        .map(str::to_string)
        .collect();
    dirt
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn worktree_dirt() -> WorktreeDirt {
    WorktreeDirt::default()
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

pub(super) fn launch_filter_of(work: &[(Value, usize)], id: &str, admitted: &HashSet<String>, project_root: &str) -> &'static str {
    match row_by_id(work, id) {
        None => "not_in_work",
        Some(_) if !id.starts_with(LAUNCH_ID_PREFIX) => "not_witness_gap",
        Some(_) if !admitted.contains(id) => "not_admitted",
        Some(row) if pool_rank::node_arm(row, true).is_some() => "arm_lane",
        Some(row) if !node_only_module(row, project_root) => "not_node_only",
        Some(_) => "launchable",
    }
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
