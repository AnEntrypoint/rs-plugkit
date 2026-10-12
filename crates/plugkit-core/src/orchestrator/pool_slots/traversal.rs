use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::collections::HashSet;

use super::super::transitions::prd_open_rows_with_recency;
use super::{pool_dir, MODULE_EXTENSIONS};
use crate::config::PROJECT_CONFIG_REL;
use crate::pkfs;

const TRAVERSAL_ROOTS: [&str; 3] = ["src", "apps", "client"];
const TRAVERSAL_SKIP_DIRS: [&str; 12] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "coverage",
    ".next",
    "__pycache__",
    "deps",
    "_build",
    ".venv",
];
const TRAVERSAL_NODE_TTL_MS: u64 = 6 * 60 * 60 * 1000;
pub(super) const TRAVERSAL_NODE_SHOWN: usize = 12;
const TRAVERSAL_NODES_FILE: &str = "traversal-surfaces.json";
const TRAVERSAL_STATE_UNIT: &str = "module";
const TRAVERSAL_MODULE_DEPTH: usize = 3;

fn dir_entries(dir: &str) -> Vec<(String, bool)> {
    let Some(Value::Array(entries)) = pkfs::readdir(dir) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let name = match entry.as_str() {
                Some(bare) => bare.to_string(),
                None => entry.get("name").and_then(Value::as_str)?.to_string(),
            };
            let is_dir = match entry.get("is_file").or_else(|| entry.get("isFile")).and_then(Value::as_bool) {
                Some(is_file) => !is_file,
                None => !name.contains('.'),
            };
            Some((name, is_dir))
        })
        .collect()
}

fn is_skipped(name: &str) -> bool {
    name.starts_with('.') || TRAVERSAL_SKIP_DIRS.contains(&name)
}

fn traversal_config(project_root: &str) -> Value {
    let Some(text) = pkfs::read_to_string(&format!("{}/{}", project_root, PROJECT_CONFIG_REL)) else {
        return Value::Null;
    };
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    parsed.get("traversal").cloned().unwrap_or(Value::Null)
}

fn config_list(config: &Value, key: &str) -> Vec<String> {
    config
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn is_safe_root(root: &str) -> bool {
    !root.is_empty()
        && !root.starts_with('/')
        && !root.contains('\\')
        && !root.contains(':')
        && root.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn configured_roots(config: &Value) -> Vec<String> {
    config_list(config, "roots")
        .into_iter()
        .map(|root| root.trim_start_matches("./").trim_end_matches('/').to_string())
        .filter(|root| is_safe_root(root))
        .collect()
}

fn module_extensions(project_root: &str) -> Vec<String> {
    let configured = config_list(&traversal_config(project_root), "module_extensions");
    if configured.is_empty() {
        return MODULE_EXTENSIONS.iter().map(|extension| extension.to_string()).collect();
    }
    configured
        .into_iter()
        .map(|extension| {
            if extension.starts_with('.') {
                extension
            } else {
                format!(".{}", extension)
            }
        })
        .collect()
}

fn discovered_roots(project_root: &str) -> Vec<String> {
    let mut roots: Vec<String> = dir_entries(project_root)
        .into_iter()
        .filter(|(name, is_dir)| *is_dir && !is_skipped(name.as_str()))
        .map(|(name, _)| name)
        .collect();
    roots.sort();
    roots
}

fn traversal_roots(project_root: &str) -> Vec<String> {
    let configured = configured_roots(&traversal_config(project_root));
    if !configured.is_empty() {
        return configured;
    }
    let preferred: Vec<String> = TRAVERSAL_ROOTS
        .iter()
        .map(|root| root.to_string())
        .filter(|root| pkfs::readdir(&format!("{}/{}", project_root, root)).is_some())
        .collect();
    if !preferred.is_empty() {
        return preferred;
    }
    discovered_roots(project_root)
}

fn walk_modules(dir: &str, prefix: &str, depth: usize, extensions: &[String], nodes: &mut Vec<String>) {
    for (name, is_dir) in dir_entries(dir) {
        if is_skipped(&name) {
            continue;
        }
        let relative = if prefix.is_empty() { name.clone() } else { format!("{}/{}", prefix, name) };
        if !is_dir {
            if extensions.iter().any(|extension| name.ends_with(extension.as_str())) {
                nodes.push(relative);
            }
        } else if depth > 0 {
            walk_modules(&format!("{}/{}", dir, name), &relative, depth - 1, extensions, nodes);
        }
    }
}

fn list_nodes(project_root: &str) -> Vec<String> {
    let extensions = module_extensions(project_root);
    let mut nodes = Vec::new();
    for root in traversal_roots(project_root) {
        walk_modules(&format!("{}/{}", project_root, root), &root, TRAVERSAL_MODULE_DEPTH, &extensions, &mut nodes);
    }
    nodes.sort();
    nodes.dedup();
    nodes
}

fn node_state_path(dir: &str) -> String {
    format!("{}/{}", dir, TRAVERSAL_NODES_FILE)
}

fn refused(path: &str, cause: &str) -> String {
    format!("{path} {cause}; no node leased or recorded and the file is left unchanged; delete or repair it to resume")
}

fn is_node_key(name: &str) -> bool {
    match name.rsplit('/').next().and_then(|segment| segment.find('.')) {
        Some(index) => index > 0,
        None => false,
    }
}

pub(super) fn read_node_state(dir: &str) -> Result<BTreeMap<String, u64>, String> {
    let path = node_state_path(dir);
    if !pkfs::exists(&path) {
        return Ok(BTreeMap::new());
    }
    let Some(text) = pkfs::read_to_string(&path) else {
        return Err(refused(&path, "exists but cannot be read"));
    };
    let record: Value = serde_json::from_str(&text)
        .map_err(|error| refused(&path, &format!("is not valid JSON ({error})")))?;
    let scanned = record
        .get("scanned")
        .and_then(Value::as_object)
        .ok_or_else(|| refused(&path, "has no scanned object"))?;
    scanned
        .iter()
        .filter(|(name, _)| is_node_key(name.as_str()))
        .map(|(name, ts)| {
            ts.as_u64()
                .map(|ts| (name.clone(), ts))
                .ok_or_else(|| refused(&path, &format!("holds a non-integer lease time for {name}")))
        })
        .collect()
}

fn write_node_state(dir: &str, state: &BTreeMap<String, u64>) -> bool {
    pkfs::write(
        &node_state_path(dir),
        &json!({"scanned": state, "unit": TRAVERSAL_STATE_UNIT}).to_string(),
    )
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn recorded_node_names(project_root: &str, names: &[String]) -> Vec<String> {
    if names.iter().all(|name| is_node_key(name)) {
        return names.to_vec();
    }
    let nodes = list_nodes(project_root);
    let mut recorded = Vec::new();
    for name in names {
        if is_node_key(name) {
            recorded.push(name.clone());
            continue;
        }
        recorded.extend(nodes.iter().filter(|node| names_surface(name, node.as_str())).cloned());
    }
    recorded
}

pub(super) fn record_scanned_nodes(
    project_root: &str,
    dir: &str,
    names: &[String],
    now: u64,
) -> Result<(), String> {
    let mut state = read_node_state(dir)?;
    for name in recorded_node_names(project_root, names) {
        state.insert(name, now);
    }
    if write_node_state(dir, &state) {
        Ok(())
    } else {
        Err(format!("could not write {}", node_state_path(dir)))
    }
}

pub(super) fn assign_traversal_node(project_root: &str, now: u64) -> Result<Option<String>, String> {
    let dir = pool_dir(project_root);
    let mut state = read_node_state(&dir).map_err(|reason| format!("pool-brief: {reason}"))?;
    let Some(node) = traversal_candidates(project_root, &state, now).into_iter().next() else {
        return Ok(None);
    };
    state.insert(node.clone(), now);
    if !write_node_state(&dir, &state) {
        return Err(format!(
            "pool-brief: could not lease {} in {}/{}",
            node,
            dir,
            TRAVERSAL_NODES_FILE
        ));
    }
    Ok(Some(node))
}

fn open_row_named(row_id: &str) -> Option<Value> {
    prd_open_rows_with_recency()
        .into_iter()
        .map(|(row, _)| row)
        .find(|row| row.get("id").and_then(Value::as_str) == Some(row_id))
}

fn surface_token_of(token: &str) -> String {
    let unquoted = token.trim_matches(|c: char| {
        matches!(c, '\'' | '"' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>')
    });
    unquoted
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches(|c: char| c == '.' || c == ':')
        .to_string()
}

fn names_surface(token: &str, surface: &str) -> bool {
    token
        .strip_prefix(surface)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn named_traversal_node(row: &Value, nodes: &[String]) -> Option<String> {
    let tokens: Vec<String> = row
        .get("surface")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(surface_token_of)
        .collect();
    for token in &tokens {
        if let Some(node) = nodes.iter().find(|node| node.as_str() == token.as_str()).cloned() {
            return Some(node);
        }
    }
    for token in &tokens {
        if let Some(node) = nodes.iter().find(|node| names_surface(token, node.as_str())).cloned() {
            return Some(node);
        }
    }
    for token in &tokens {
        let file = file_name_of(token);
        if let Some(node) = nodes.iter().find(|node| file_name_of(node.as_str()) == file).cloned() {
            return Some(node);
        }
    }
    None
}

pub(super) fn assign_row_traversal_node(
    project_root: &str,
    row_id: &str,
    now: u64,
) -> Result<String, String> {
    let Some(row) = open_row_named(row_id) else {
        return Err(format!(
            "pool-brief: row {row_id} is not an open row in the gm store (absent, resolved, blocked, or the store did not read); no node leased"
        ));
    };
    let nodes = list_nodes(project_root);
    let Some(node) = named_traversal_node(&row, &nodes) else {
        let field = row.get("surface").and_then(Value::as_str).unwrap_or_default();
        let extensions = module_extensions(project_root).join(" ");
        let roots = traversal_roots(project_root).join(", ");
        return Err(format!(
            "pool-brief: row {} names no traversal node (surface: \"{}\"); a traversal node is one source file ({}) up to {} directory levels under a traversal root ({}); no node leased",
            row_id,
            field,
            extensions,
            TRAVERSAL_MODULE_DEPTH,
            roots
        ));
    };
    let dir = pool_dir(project_root);
    let mut state = read_node_state(&dir).map_err(|reason| format!("pool-brief: {reason}"))?;
    if let Some(&leased_at) = state.get(&node) {
        let expires_at = leased_at.saturating_add(TRAVERSAL_NODE_TTL_MS);
        if now <= expires_at {
            return Err(format!(
                "pool-brief: row {row_id} names {node}, which is already scanned or leased until epoch ms {expires_at}; no node leased"
            ));
        }
    }
    state.insert(node.clone(), now);
    if !write_node_state(&dir, &state) {
        return Err(format!(
            "pool-brief: could not lease {} in {}/{}",
            node,
            dir,
            TRAVERSAL_NODES_FILE
        ));
    }
    Ok(node)
}

fn unscanned_nodes(nodes: &[String], state: &BTreeMap<String, u64>, now: u64) -> Vec<String> {
    nodes
        .iter()
        .filter(|node| match state.get(*node) {
            Some(ts) => now.saturating_sub(*ts) > TRAVERSAL_NODE_TTL_MS,
            None => true,
        })
        .cloned()
        .collect()
}

fn module_stem(module: &str) -> &str {
    let name = file_name_of(module);
    name.split('.').next().unwrap_or(name)
}

fn scripts_witnessed_words(project_root: &str) -> HashSet<String> {
    let extensions = module_extensions(project_root);
    let mut words: HashSet<String> = HashSet::new();
    for (name, is_dir) in dir_entries(&format!("{}/scripts", project_root)) {
        if is_dir || !extensions.iter().any(|extension| name.ends_with(extension.as_str())) {
            continue;
        }
        let Some(text) = pkfs::read_to_string(&format!("{}/scripts/{}", project_root, name)) else {
            continue;
        };
        words.extend(
            text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .filter(|word| !word.is_empty())
                .map(str::to_string),
        );
        words.extend(
            text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '-'))
                .filter(|word| word.contains('-'))
                .map(str::to_string),
        );
    }
    words
}

fn node_is_unwitnessed(node: &str, words: &HashSet<String>) -> bool {
    !words.contains(module_stem(node))
}

pub(super) fn traversal_candidates(project_root: &str, state: &BTreeMap<String, u64>, now: u64) -> Vec<String> {
    let unscanned = unscanned_nodes(&list_nodes(project_root), state, now);
    if unscanned.is_empty() {
        return Vec::new();
    }
    let words = scripts_witnessed_words(project_root);
    unscanned
        .into_iter()
        .filter(|node| node_is_unwitnessed(node, &words))
        .collect()
}
