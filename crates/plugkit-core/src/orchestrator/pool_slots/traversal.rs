use serde_json::{json, Value};
use std::collections::HashSet;

use super::super::transitions::prd_open_rows_with_recency;
use super::{pool_dir, MODULE_EXTENSIONS};
use crate::pkfs;

const TRAVERSAL_SURFACE_ROOTS: [&str; 3] = ["src", "apps", "client"];
const TRAVERSAL_SURFACE_TTL_MS: u64 = 6 * 60 * 60 * 1000;
pub(super) const TRAVERSAL_SURFACE_SHOWN: usize = 8;
const TRAVERSAL_SURFACES_FILE: &str = "traversal-surfaces.json";

fn list_surfaces(project_root: &str) -> Vec<String> {
    let mut surfaces = Vec::new();
    for root in TRAVERSAL_SURFACE_ROOTS {
        let Some(Value::Array(entries)) = pkfs::readdir(&format!("{}/{}", project_root, root)) else {
            continue;
        };
        for entry in entries {
            let name = match entry.as_str() {
                Some(bare) => bare.to_string(),
                None => match entry.get("name").and_then(Value::as_str) {
                    Some(obj_name) => obj_name.to_string(),
                    None => continue,
                },
            };
            let is_dir = match entry.get("is_file").or_else(|| entry.get("isFile")).and_then(Value::as_bool) {
                Some(is_file) => !is_file,
                None => !name.contains('.'),
            };
            if is_dir {
                surfaces.push(format!("{}/{}", root, name));
            }
        }
    }
    surfaces.sort();
    surfaces
}

fn surface_state_path(dir: &str) -> String {
    format!("{}/{}", dir, TRAVERSAL_SURFACES_FILE)
}

fn refused(path: &str, cause: &str) -> String {
    format!("{path} {cause}; no surface leased or recorded and the file is left unchanged; delete or repair it to resume")
}

pub(super) fn read_surface_state(dir: &str) -> Result<std::collections::BTreeMap<String, u64>, String> {
    let path = surface_state_path(dir);
    if !pkfs::exists(&path) {
        return Ok(std::collections::BTreeMap::new());
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
        .map(|(name, ts)| {
            ts.as_u64()
                .map(|ts| (name.clone(), ts))
                .ok_or_else(|| refused(&path, &format!("holds a non-integer lease time for {name}")))
        })
        .collect()
}

fn write_surface_state(dir: &str, state: &std::collections::BTreeMap<String, u64>) -> bool {
    pkfs::write(&format!("{}/{}", dir, TRAVERSAL_SURFACES_FILE), &json!({"scanned": state}).to_string())
}

pub(super) fn record_scanned_surfaces(dir: &str, names: &[String], now: u64) -> Result<(), String> {
    let mut state = read_surface_state(dir)?;
    for name in names {
        state.insert(name.clone(), now);
    }
    if write_surface_state(dir, &state) {
        Ok(())
    } else {
        Err(format!("could not write {}", surface_state_path(dir)))
    }
}

pub(super) fn assign_traversal_surface(project_root: &str, now: u64) -> Result<Option<String>, String> {
    let dir = pool_dir(project_root);
    let mut state = read_surface_state(&dir).map_err(|reason| format!("pool-brief: {reason}"))?;
    let Some(surface) = traversal_candidates(project_root, &state, now).into_iter().next() else {
        return Ok(None);
    };
    state.insert(surface.clone(), now);
    if !write_surface_state(&dir, &state) {
        return Err(format!("pool-brief: could not lease {} in {}/{}", surface, dir, TRAVERSAL_SURFACES_FILE));
    }
    Ok(Some(surface))
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

fn named_traversal_surface(row: &Value, surfaces: &[String]) -> Option<String> {
    row.get("surface")
        .and_then(Value::as_str)
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(surface_token_of)
        .find_map(|token| {
            surfaces
                .iter()
                .find(|surface| names_surface(&token, surface.as_str()))
                .cloned()
        })
}

pub(super) fn assign_row_traversal_surface(
    project_root: &str,
    row_id: &str,
    now: u64,
) -> Result<String, String> {
    let Some(row) = open_row_named(row_id) else {
        return Err(format!(
            "pool-brief: row {row_id} is not an open row in the gm store (absent, resolved, blocked, or the store did not read); no surface leased"
        ));
    };
    let surfaces = list_surfaces(project_root);
    let Some(surface) = named_traversal_surface(&row, &surfaces) else {
        let field = row.get("surface").and_then(Value::as_str).unwrap_or_default();
        return Err(format!(
            "pool-brief: row {row_id} names no traversal surface (surface: \"{field}\"); a traversal surface is a directory under src, apps or client; no surface leased"
        ));
    };
    let dir = pool_dir(project_root);
    let mut state = read_surface_state(&dir).map_err(|reason| format!("pool-brief: {reason}"))?;
    if let Some(&leased_at) = state.get(&surface) {
        let expires_at = leased_at.saturating_add(TRAVERSAL_SURFACE_TTL_MS);
        if now <= expires_at {
            return Err(format!(
                "pool-brief: row {row_id} names {surface}, which is already scanned or leased until epoch ms {expires_at}; no surface leased"
            ));
        }
    }
    state.insert(surface.clone(), now);
    if !write_surface_state(&dir, &state) {
        return Err(format!("pool-brief: could not lease {} in {}/{}", surface, dir, TRAVERSAL_SURFACES_FILE));
    }
    Ok(surface)
}

fn unscanned_surfaces(surfaces: &[String], state: &std::collections::BTreeMap<String, u64>, now: u64) -> Vec<String> {
    surfaces
        .iter()
        .filter(|surface| match state.get(*surface) {
            Some(ts) => now.saturating_sub(*ts) > TRAVERSAL_SURFACE_TTL_MS,
            None => true,
        })
        .cloned()
        .collect()
}

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

fn surface_modules(dir: &str, prefix: &str, depth: usize, modules: &mut Vec<String>) {
    for (name, is_dir) in dir_entries(dir) {
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let relative = if prefix.is_empty() { name.clone() } else { format!("{}/{}", prefix, name) };
        if !is_dir {
            if MODULE_EXTENSIONS.iter().any(|extension| name.ends_with(*extension)) {
                modules.push(relative);
            }
        } else if depth > 0 {
            surface_modules(&format!("{}/{}", dir, name), &relative, depth - 1, modules);
        }
    }
}

fn module_stem(module: &str) -> &str {
    let name = module.rsplit('/').next().unwrap_or(module);
    name.split('.').next().unwrap_or(name)
}

fn scripts_witnessed_words(project_root: &str) -> HashSet<String> {
    let mut words: HashSet<String> = HashSet::new();
    for (name, is_dir) in dir_entries(&format!("{}/scripts", project_root)) {
        if is_dir || !MODULE_EXTENSIONS.iter().any(|extension| name.ends_with(*extension)) {
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

fn surface_has_unwitnessed_module(project_root: &str, surface: &str, words: &HashSet<String>) -> bool {
    let mut modules = Vec::new();
    surface_modules(&format!("{}/{}", project_root, surface), "", TRAVERSAL_MODULE_DEPTH, &mut modules);
    modules.iter().any(|module| !words.contains(module_stem(module)))
}

pub(super) fn traversal_candidates(project_root: &str, state: &std::collections::BTreeMap<String, u64>, now: u64) -> Vec<String> {
    let unscanned = unscanned_surfaces(&list_surfaces(project_root), state, now);
    if unscanned.is_empty() {
        return Vec::new();
    }
    let words = scripts_witnessed_words(project_root);
    unscanned
        .into_iter()
        .filter(|surface| surface_has_unwitnessed_module(project_root, surface, &words))
        .collect()
}
