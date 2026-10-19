use super::cas;
use super::gm_dir;
use super::row_store;
use super::yaml_util::{
    defer_marker_in_text, invalidate_residual_marker, levenshtein, yaml_to_json,
};
use crate::pkfs;
use serde_yaml::Value;

pub fn prd_path() -> std::path::PathBuf {
    gm_dir().join("prd.yml")
}

pub fn prd_dir() -> std::path::PathBuf {
    gm_dir().join("prd")
}

pub fn prd_log_path() -> std::path::PathBuf {
    gm_dir().join("prd-log.jsonl")
}

pub fn prd_path_for(cwd: Option<&str>) -> std::path::PathBuf {
    match cwd {
        Some(c) => std::path::Path::new(c).join(".gm").join("prd.yml"),
        None => prd_path(),
    }
}

pub fn row_has_id(doc: &Value, id: &str) -> bool {
    doc.as_sequence()
        .map(|seq| {
            seq.iter().any(|it| {
                it.as_mapping()
                    .and_then(|m| m.get(&Value::String("id".to_string())))
                    .and_then(|v| v.as_str())
                    == Some(id)
            })
        })
        .unwrap_or(false)
}

fn row_status_for(doc: &Value, id: &str) -> Option<String> {
    doc.as_sequence()
        .and_then(|seq| {
            seq.iter().rev().find(|it| {
                it.as_mapping()
                    .and_then(|m| m.get(&Value::String("id".to_string())))
                    .and_then(|v| v.as_str())
                    == Some(id)
            })
        })
        .and_then(|it| it.as_mapping())
        .and_then(|m| m.get(&Value::String("status".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn pending_commit_comments_for_paths(cwd: Option<&str>, touched: &[String]) -> Vec<(String, String)> {
    let store = row_store::Store::for_cwd(row_store::Kind::Prd, cwd);
    let seq = match row_store::read_blocks(&store) {
        Ok(read) => read.blocks,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in &seq {
        let Some(map) = item.as_mapping() else {
            continue;
        };
        let status = map
            .get(&Value::String("status".to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if status_is_open(status) {
            continue;
        }
        let Some(comment) = map
            .get(&Value::String("commit_comment".to_string()))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let referenced = row_referenced_paths(map);
        let touches_referenced = referenced
            .iter()
            .any(|r| touched.iter().any(|t| path_reference_matches(r, t)));
        if !touches_referenced {
            continue;
        }
        let id = map
            .get(&Value::String("id".to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push((id, comment.to_string()));
    }
    out
}

fn row_referenced_paths(map: &serde_yaml::Mapping) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["witness_evidence", "witness", "resolution", "commit_comment"] {
        let text = match map.get(&Value::String(key.to_string())) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Sequence(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        for token in text.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;|=*".contains(c)) {
            if let Some(path) = referenced_path_token(token) {
                if !is_markdown_path(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

fn is_markdown_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".md", ".mdx", ".markdown"].iter().any(|ext| lowered.ends_with(ext))
}

fn markdown_heading_slug(heading: &str) -> String {
    heading
        .trim()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() || c == '-' {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

fn markdown_section(text: &str, anchor: &str) -> Option<String> {
    let mut level: Option<usize> = None;
    let mut out = String::new();
    for line in text.lines() {
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        let heading = (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ');
        if heading {
            match level {
                Some(open) if hashes <= open => break,
                Some(_) => {}
                None if markdown_heading_slug(&line[hashes..]) == anchor => level = Some(hashes),
                None => continue,
            }
        }
        if level.is_some() {
            out.push_str(line);
            out.push('\n');
        }
    }
    level.map(|_| out)
}

fn rule_section_fingerprint(path: &str, anchor: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::pkfs::read_to_string(path)
        .and_then(|text| markdown_section(&text, anchor))
        .map(|section| format!("{:x}", Sha256::digest(section.as_bytes())))
        .unwrap_or_else(|| "absent".to_string())
}

fn rule_references(texts: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for text in texts {
        for token in text.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;|=*".contains(c)) {
            let token = token.trim_end_matches(|c: char| matches!(c, '.' | ':' | '!' | '?'));
            let Some((path, anchor)) = token.split_once('#') else {
                continue;
            };
            let path = path.replace('\\', "/").trim_start_matches("./").to_string();
            let anchor = anchor.to_ascii_lowercase();
            let safe = !path.is_empty()
                && !pkfs::is_absolute(&path)
                && !path.split('/').any(|segment| segment == "..");
            if safe && !anchor.is_empty() && is_markdown_path(&path) && !out.contains(&(path.clone(), anchor.clone())) {
                out.push((path, anchor));
            }
        }
    }
    out
}

fn rule_bindings_value(texts: &[String]) -> Value {
    Value::Sequence(
        rule_references(texts)
            .into_iter()
            .map(|(path, anchor)| {
                let fingerprint = rule_section_fingerprint(&path, &anchor);
                let mut map = serde_yaml::Mapping::new();
                map.insert(Value::String("path".to_string()), Value::String(path));
                map.insert(Value::String("anchor".to_string()), Value::String(anchor));
                map.insert(Value::String("sha256".to_string()), Value::String(fingerprint));
                Value::Mapping(map)
            })
            .collect(),
    )
}

fn referenced_path_token(token: &str) -> Option<String> {
    let mut path = token.replace('\\', "/");
    while let Some(colon) = path.rfind(':') {
        let tail = &path[colon + 1..];
        let is_line_suffix = !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit() || c == '-');
        if !is_line_suffix {
            break;
        }
        path.truncate(colon);
    }
    let path = path
        .trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?'))
        .to_string();
    if path.is_empty() {
        return None;
    }
    let has_directory = path.contains('/');
    let has_extension = path.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=8).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    (has_directory || has_extension).then_some(path)
}

fn path_reference_matches(reference: &str, touched: &str) -> bool {
    let reference = reference.replace('\\', "/").to_ascii_lowercase();
    let touched = touched.replace('\\', "/").to_ascii_lowercase();
    !touched.is_empty() && (reference == touched || reference.ends_with(&format!("/{}", touched)))
}

pub fn drain_commit_comments(cwd: Option<&str>, notes: &[(String, String)]) {
    if notes.is_empty() {
        return;
    }
    let store = row_store::Store::for_cwd(row_store::Kind::Prd, cwd);
    if !pkfs::exists(&store.dir) && !pkfs::exists(&store.legacy) {
        return;
    }
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let _ = row_store::cas_retry_write(
        &store,
        cas_max_attempts,
        "prd-drain-commit-comments",
        |mut doc: Value| {
            let mut drained_any = false;
            let comment_key = Value::String("commit_comment".to_string());
            if let Some(seq) = doc.as_sequence_mut() {
                for item in seq.iter_mut() {
                    let Some(map) = item.as_mapping_mut() else {
                        continue;
                    };
                    let status = map
                        .get(&Value::String("status".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if status_is_open(&status) {
                        continue;
                    }
                    let id = map
                        .get(&Value::String("id".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment = map
                        .get(&comment_key)
                        .and_then(|v| v.as_str())
                        .map(|c| c.trim().to_string())
                        .unwrap_or_default();
                    if notes.iter().any(|(nid, ncomment)| nid == &id && ncomment == &comment) {
                        map.remove(&comment_key);
                        drained_any = true;
                    }
                }
            }
            if !drained_any {
                return cas::CasOutcome::Abort(
                    String::new(),
                    "no queued commit_comment matches the notes bundled into this commit".to_string(),
                    0,
                );
            }
            cas::CasOutcome::Write(doc, ())
        },
    );
}

pub fn status_is_open(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    !super::fsm::graph()
        .policy
        .prd_closed_statuses
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&normalized))
}

pub fn status_is_blocked(status: &str) -> bool {
    status.trim().eq_ignore_ascii_case("blocked")
}

fn status_matches_filter(want: &str, status: &str) -> bool {
    if want.eq_ignore_ascii_case("pending") {
        return status_is_open(status) && !status_is_blocked(status);
    }
    want == status
}

fn subject_from_fields(item_map: &serde_yaml::Mapping) -> &str {
    [
        "subject",
        "title",
        "name",
        "task",
        "goal",
        "description",
        "notes",
    ]
    .iter()
    .find_map(|f| {
        item_map
            .get(&Value::String(f.to_string()))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    })
    .unwrap_or("")
}

fn slug_from_subject(subject: &str) -> Option<String> {
    let s = subject.trim();
    if s.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        return None;
    }
    if out.len() > 64 {
        out.truncate(64);
        while out.ends_with('-') {
            out.pop();
        }
    }
    Some(out)
}

const PRD_BRIEF_TITLE_CHARS: usize = 140;

const PRD_ROW_ID_FIELDS: &[&str] = &["id", "ids", "row_id"];

const PRD_ID_PREFIX_FIELDS: &[&str] = &["id_prefix", "idPrefix"];

const PRD_ROW_STATUS_FIELDS: &[&str] = &["status"];

const PRD_FULL_BODY_FIELDS: &[&str] = &["full", "verbose", "bodies"];

const PRD_BRIEF_BODY_FIELDS: &[&str] = &["brief", "compact"];
const PRD_LIST_ITEMS_BUDGET_CHARS: usize = 20_000;

const PRD_TITLE_FIELDS: &[&str] = &["title", "subject"];

fn load_blocks() -> Result<Vec<Value>, (String, i32)> {
    Ok(row_store::read_blocks(&row_store::Store::prd())?.blocks)
}

fn load_rows() -> Result<Vec<serde_json::Value>, (String, i32)> {
    Ok(load_blocks()?.iter().filter_map(row_to_json).collect())
}

fn row_to_json(item: &Value) -> Option<serde_json::Value> {
    let m = item.as_mapping()?;
    let mut out = serde_json::Map::new();
    for (k, v) in m {
        if let Some(ks) = k.as_str() {
            out.insert(ks.to_string(), yaml_to_json(v));
        }
    }
    Some(serde_json::Value::Object(out))
}

pub(super) fn row_text(row: &serde_json::Value, fields: &[&str]) -> String {
    fields
        .iter()
        .filter_map(|key| row.get(*key).and_then(|v| v.as_str()))
        .find(|s| !s.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

fn row_status(row: &serde_json::Value) -> String {
    match row.get("status").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "pending".to_string(),
    }
}

pub(super) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn clamp_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push_str("...");
    }
    out
}

fn brief_row(row: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": row_text(row, &["id"]),
        "status": row_status(row),
        "title": clamp_chars(&one_line(&row_text(row, PRD_TITLE_FIELDS)), PRD_BRIEF_TITLE_CHARS),
    })
}

pub(super) fn request_object(content: &str) -> serde_json::Value {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    let parsed = serde_json::from_str::<serde_json::Value>(trimmed).ok().or_else(|| {
        serde_yaml::from_str::<Value>(trimmed)
            .ok()
            .and_then(|v| serde_json::to_value(v).ok())
    });
    match parsed {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    }
}

pub(super) fn request_strings(request: &serde_json::Value, fields: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for field in fields {
        match request.get(*field) {
            Some(serde_json::Value::String(s)) => {
                if !s.trim().is_empty() {
                    out.push(s.trim().to_string());
                }
            }
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    if let Some(s) = item.as_str() {
                        if !s.trim().is_empty() {
                            out.push(s.trim().to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(super) fn request_flag(request: &serde_json::Value, fields: &[&str]) -> Option<bool> {
    fields
        .iter()
        .filter_map(|field| request.get(*field))
        .find_map(|v| v.as_bool())
}

pub fn handle_list_full() -> (String, String, i32) {
    let rows = match load_rows() {
        Ok(rows) => rows,
        Err((message, code)) => return (String::new(), message, code),
    };
    let count = rows.len();
    (
        serde_json::json!({ "items": rows, "count": count }).to_string(),
        String::new(),
        0,
    )
}

fn request_limit(request: &serde_json::Value) -> Result<Option<usize>, String> {
    match request.get("limit") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
            .map(|n| Some(usize::try_from(n).unwrap_or(usize::MAX)))
            .ok_or_else(|| "prd-list limit must be a non-negative integer".to_string()),
    }
}

fn request_offset(request: &serde_json::Value) -> Result<usize, String> {
    match request.get("offset") {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
            .ok_or_else(|| "prd-list offset must be a non-negative integer".to_string()),
    }
}

fn fold_last_blocks(rows: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut last_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let id = row_text(row, &["id"]);
        if !id.is_empty() {
            last_index.insert(id, index);
        }
    }
    rows.into_iter()
        .enumerate()
        .filter(|(index, row)| {
            let id = row_text(row, &["id"]);
            id.is_empty() || last_index.get(&id) == Some(index)
        })
        .map(|(_, row)| row)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn nearest_ids(ids: &[String], want: &str, n: usize) -> Vec<String> {
    let mut scored: Vec<(usize, &String)> = ids
        .iter()
        .map(|id| (edit_distance(id, want), id))
        .collect();
    scored.sort();
    scored.into_iter().take(n).map(|(_, id)| id.clone()).collect()
}

pub const LOOKUP_MISS_EXIT: i32 = 4;

const PRD_LIST_DEFAULT_PAGE: usize = 10;

pub fn handle_list(content: &str) -> (String, String, i32) {
    let request = request_object(content);
    let limit = match request_limit(&request) {
        Ok(limit) => limit,
        Err(message) => return (String::new(), message, 1),
    };
    let rows = match load_rows() {
        Ok(rows) => rows,
        Err((message, code)) => return (String::new(), message, code),
    };
    let store_total = rows.len();
    let wanted_ids = request_strings(&request, PRD_ROW_ID_FIELDS);
    let id_prefixes = request_strings(&request, PRD_ID_PREFIX_FIELDS);
    let requested_statuses = request_strings(&request, PRD_ROW_STATUS_FIELDS);
    let limit = limit.or(if wanted_ids.is_empty() && requested_statuses.is_empty() {
        Some(PRD_LIST_DEFAULT_PAGE)
    } else {
        None
    });
    let every_status = requested_statuses
        .iter()
        .any(|want| want.eq_ignore_ascii_case("all"));
    let wanted_statuses: Vec<String> = if every_status {
        Vec::new()
    } else {
        requested_statuses
    };
    let folded_rows = fold_last_blocks(rows);
    let closed_statuses = super::fsm::graph().policy.prd_closed_statuses.clone();
    if let Some(unknown) = wanted_statuses.iter().find(|want| {
        !want.eq_ignore_ascii_case("pending")
            && !status_is_blocked(want.as_str())
            && !closed_statuses.iter().any(|c| c.eq_ignore_ascii_case(want.as_str()))
            && !folded_rows
                .iter()
                .any(|row| row_status(row).eq_ignore_ascii_case(want.as_str()))
    }) {
        let accepted: Vec<String> = ["pending", "blocked", "all"]
            .iter()
            .map(|s| s.to_string())
            .chain(closed_statuses.iter().map(|c| c.to_string()))
            .collect();
        return (
            String::new(),
            format!(
                "prd-list refused: unknown status '{}'; accepted values: {}",
                unknown,
                accepted.join(", ")
            ),
            1,
        );
    }
    let open_ids: Vec<String> = folded_rows
        .iter()
        .map(|row| row_text(row, &["id"]))
        .collect();
    let selected: Vec<serde_json::Value> = folded_rows
        .into_iter()
        .filter(|row| {
            let id = row_text(row, &["id"]);
            let status = row_status(row);
            let id_ok = wanted_ids.is_empty() || wanted_ids.iter().any(|want| want == &id);
            let prefix_ok = id_prefixes.is_empty()
                || id_prefixes.iter().any(|prefix| id.starts_with(prefix.as_str()));
            let status_ok = wanted_statuses.is_empty()
                || wanted_statuses
                    .iter()
                    .any(|want| status_matches_filter(want, &status));
            id_ok && prefix_ok && status_ok
        })
        .collect();
    let store = row_store::Store::prd();
    let log = match row_store::read_log(&store) {
        Ok(log) => log,
        Err(message) => return (String::new(), message, 1),
    };
    let mut selected = selected;
    if !wanted_ids.is_empty() {
        for want in &wanted_ids {
            if selected.iter().any(|row| row_text(row, &["id"]) == *want) {
                continue;
            }
            match row_store::log_row(&store, want) {
                Ok(Some(row)) => selected.push(row),
                Ok(None) => {}
                Err(message) => return (String::new(), message, 1),
            }
        }
    } else if every_status || !wanted_statuses.is_empty() {
        let closed = super::fsm::graph().policy.prd_closed_statuses.clone();
        if every_status
            || wanted_statuses
                .iter()
                .any(|want| closed.iter().any(|c| c.eq_ignore_ascii_case(want)))
        {
            let logged = match row_store::log_rows_with_status(&store, &closed) {
                Ok(logged) => logged,
                Err(message) => return (String::new(), message, 1),
            };
            for row in logged {
                if every_status
                    || wanted_statuses
                        .iter()
                        .any(|want| want.eq_ignore_ascii_case(&row_status(&row)))
                {
                    selected.push(row);
                }
            }
        }
    }
    if !id_prefixes.is_empty() {
        selected.retain(|row| {
            let id = row_text(row, &["id"]);
            id_prefixes.iter().any(|prefix| id.starts_with(prefix.as_str()))
        });
    }
    if !wanted_ids.is_empty() && selected.is_empty() {
        return (
            String::new(),
            format!(
                "no PRD row with exact id {} among the {} open rows in .gm/prd/ (or the resolved rows in .gm/prd-log.jsonl; ids match exactly, not by prefix); nearest open ids: [{}] -- pass no id for the listing",
                wanted_ids.join(", "),
                open_ids.len(),
                nearest_ids(&open_ids, &wanted_ids[0], 3).join(", ")
            ),
            LOOKUP_MISS_EXIT,
        );
    }
    let matched = selected.len();
    let offset = match request_offset(&request) {
        Ok(offset) => offset.min(matched),
        Err(message) => return (String::new(), message, 1),
    };
    let requested = limit.map_or(matched - offset, |n| n.min(matched - offset));
    let brief_flag = request_flag(&request, PRD_BRIEF_BODY_FIELDS);
    let full_flag = request_flag(&request, PRD_FULL_BODY_FIELDS);
    let want_full =
        full_flag.unwrap_or_else(|| brief_flag.map(|b| !b).unwrap_or(!wanted_ids.is_empty()));
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut items_chars = 0usize;
    for row in selected.into_iter().skip(offset).take(requested) {
        let item = if want_full { row } else { brief_row(&row) };
        let item_chars = item.to_string().len();
        if !items.is_empty() && items_chars + item_chars > PRD_LIST_ITEMS_BUDGET_CHARS {
            break;
        }
        items_chars += item_chars;
        items.push(item);
    }
    let page = items.len();
    let truncated = offset + page < matched;
    let mut reply = serde_json::json!({
        "items": items,
        "count": items.len(),
        "total": matched,
        "store_total": store_total,
        "limit": limit,
        "offset": offset,
        "next_offset": truncated.then_some(offset + page),
        "truncated": truncated,
        "brief": !want_full,
        "full": want_full,
        "hint": "pass {\"id\":\"<row id>\"} for one row in full (repeated ids fold to the last); {\"status\":\"pending\"} to filter; {\"status\":\"pending\",\"limit\":<n>} to cap; {\"status\":\"all\",\"limit\":<n>,\"offset\":<n>} to page (next_offset null when done); {\"full\":true} for every full body. .gm/prd/ holds open work only; resolved rows are in .gm/prd-log.jsonl.",
    });
    if truncated {
        reply["hint"] = serde_json::json!("more rows: repeat with offset set to next_offset");
    }
    if log.dropped_torn_lines > 0 {
        reply["dropped_torn_lines"] = serde_json::json!(log.dropped_torn_lines);
        reply["dropped_torn_lines_sidecar"] = serde_json::json!(".gm/prd-log.jsonl.torn");
    }
    (reply.to_string(), String::new(), 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddOutcome {
    Added,
    Rescoped,
    AlreadyIdentical,
}

impl AddOutcome {
    fn response_key(self) -> &'static str {
        match self {
            AddOutcome::Rescoped => "rescoped",
            AddOutcome::Added | AddOutcome::AlreadyIdentical => "added",
        }
    }
}

const DISPOSITIONS: [&str; 4] = ["open", "accepted", "rejected", "deferred"];

fn mapping_text(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(&Value::String(key.to_string()))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn disposition_head(disposition: &str) -> &str {
    disposition.split(':').next().unwrap_or("").trim()
}

fn body_session_id(body: &serde_json::Value) -> Option<String> {
    ["session_id", "session", "SESSION_ID", "sessionId"]
        .iter()
        .find_map(|key| body.get(*key).and_then(serde_json::Value::as_str))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (
            String::new(),
            "missing body: provide PRD item as JSON or YAML".to_string(),
            1,
        );
    }
    if serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|b| b.get("repair").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
    {
        if let Err((_, err, rc)) = super::prd_repair::repair_prd_store(
            &prd_path().to_string_lossy(),
            16,
        ) {
            return (String::new(), err, rc);
        }
    }
    let new_item: Value = match serde_yaml::from_str::<Value>(trimmed) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .and_then(|j| serde_yaml::to_value(j).ok())
        {
            Some(v) => v,
            None => return (String::new(), "parse failed".to_string(), 1),
        },
    };
    let mut item_map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => {
            return (
                String::new(),
                "item must be a mapping with id/subject/status".to_string(),
                1,
            )
        }
    };
    if let Some(disposition) = mapping_text(&item_map, "disposition") {
        if !DISPOSITIONS.contains(&disposition_head(&disposition)) {
            return (
                String::new(),
                format!(
                    "prd-add refused: disposition '{}' must start with one of open, accepted, rejected, deferred (a reason may follow ':')",
                    disposition
                ),
                1,
            );
        }
    }
    if mapping_text(&item_map, "owner").is_none() {
        if let Some(filer) = body_session_id(&request_object(trimmed)) {
            item_map.insert(Value::String("owner".to_string()), Value::String(filer));
        }
    }
    if let Some(recheck) = mapping_text(&item_map, "recheck_by") {
        if mapping_text(&item_map, "owner").as_deref() == Some(recheck.as_str()) {
            return (
                String::new(),
                format!(
                    "prd-add refused: recheck_by '{}' equals owner; the session that re-checks a finding must differ from the one that acts on it",
                    recheck
                ),
                1,
            );
        }
    }
    let has_external_block = item_map
        .get(&Value::String("blockedBy".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .any(|x| matches!(x.as_str(), Some("external") | Some("out-of-reach")))
        })
        .unwrap_or(false);
    if !has_external_block {
        let mut scan_buf = String::new();
        for field in &["description", "subject", "notes"] {
            if let Some(s) = item_map
                .get(&Value::String(field.to_string()))
                .and_then(|v| v.as_str())
            {
                scan_buf.push_str(s);
                scan_buf.push('\n');
            }
        }
        if let Some(marker) = defer_marker_in_text(&scan_buf) {
            let err = format!(
                "PRD item rejected: deferral language detected ('{}'). Per §22 Fix on Sight and §17 Maximal Cover, in-spirit reachable work must be executed this turn, not deferred. Everything is fixable: drop the deferral phrasing and either execute this turn, or -- if the fix depends on something outside the tree -- rescope the row to the concrete reach action that builds past it (drive the crashing tool's protocol directly, spawn your own instance, open the cross-repo change, script the credential path) and execute THAT. There is no 'declare it externally blocked' exit; `blockedBy` may only transiently carry the reach path while it is being built.",
                marker
            );
            return (String::new(), err, 1);
        }
    }
    let provided_id = item_map
        .get(&Value::String("id".to_string()))
        .or_else(|| item_map.get(&Value::String("slug".to_string())))
        .or_else(|| item_map.get(&Value::String("prd_id".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let subject_str = subject_from_fields(&item_map);
    let slug = if provided_id.is_none() {
        slug_from_subject(subject_str)
    } else {
        None
    };
    if provided_id.is_none() && slug.is_none() {
        #[cfg(target_arch = "wasm32")]
        crate::wasm_dispatch::emit_event(
            "deviation.prd-add-no-id",
            serde_json::json!({
                "subject": subject_str,
                "hint": "Pass `id` in prd-add body. No usable text in any of id/slug/prd_id or subject/title/name/task/goal/description/notes -- every one was empty or unslugifiable, so the row was REJECTED. An item-<ms> fallback cannot be referenced by intent in prd-resolve, so it is never admitted. Pass `id` directly, or put the intent in any of subject/title/name/task/goal/description so slug derivation succeeds.",
            }),
        );
        let err = "PRD item rejected: no usable `id` and no slugifiable text in subject/title/name/task/goal/description/notes. A referenceable handle is mandatory -- every later prd-resolve / recall names the row by id. Pass `id` directly (kebab-case slug derived from intent) or provide a meaningful subject/title/description. Auto `item-<ms>` ids are not admitted because they cannot be referenced by intent. Flat example: prd-add needs {\"id\":\"<kebab-case-slug>\",\"subject\":\"<one-line intent>\",\"description\":\"<what to change and why>\"}, with every field at the top level of the body, not under a nested key.";
        return (String::new(), err.to_string(), 1);
    }
    let id = provided_id
        .clone()
        .or_else(|| slug.clone())
        .unwrap_or_else(|| format!("item-{}", crate::orchestrator::state::now_ms()));
    let store = row_store::Store::prd();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let overwrite = item_map
        .get(&Value::String("overwrite".to_string()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let verify_id = id.clone();
    let outcome = row_store::cas_retry_write_verified(
        &store,
        cas_max_attempts,
        "prd-add",
        |mut doc: Value| {
        let mut add_outcome = AddOutcome::Added;
        if let Some(seq) = doc.as_sequence_mut() {
            let mut new_with_id = item_map.clone();
            new_with_id.remove(&Value::String("overwrite".to_string()));
            let mut id_first = serde_yaml::Mapping::new();
            id_first.insert(Value::String("id".to_string()), Value::String(id.clone()));
            for (key, value) in new_with_id.into_iter() {
                if key.as_str() == Some("id") {
                    continue;
                }
                id_first.insert(key, value);
            }
            if !id_first.contains_key(&Value::String("status".to_string())) {
                id_first.insert(
                    Value::String("status".to_string()),
                    Value::String("pending".to_string()),
                );
            }
            let new_row = Value::Mapping(id_first);
            let existing = seq.iter_mut().find(|it| {
                it.as_mapping()
                    .and_then(|m| m.get(&Value::String("id".to_string())))
                    .and_then(|v| v.as_str())
                    == Some(id.as_str())
            });
            match existing {
                Some(slot) => {
                    if *slot == new_row {
                        add_outcome = AddOutcome::AlreadyIdentical;
                    } else if overwrite {
                        let existing_status = slot
                            .as_mapping()
                            .and_then(|m| m.get(&Value::String("status".to_string())))
                            .and_then(|v| v.as_str())
                            .unwrap_or("pending")
                            .to_string();
                        if !status_is_open(&existing_status) {
                            return cas::CasOutcome::Abort(
                                String::new(),
                                format!(
                                    "prd-add refused: id '{}' is closed (status '{}'). overwrite cannot rescope or reopen a resolved row; log the new work under a new id.",
                                    id, existing_status
                                ),
                                1,
                            );
                        }
                        let mut rescoped = new_row;
                        if !item_map.contains_key(&Value::String("status".to_string())) {
                            if let Some(map) = rescoped.as_mapping_mut() {
                                map.insert(
                                    Value::String("status".to_string()),
                                    Value::String(existing_status),
                                );
                            }
                        }
                        add_outcome = AddOutcome::Rescoped;
                        *slot = rescoped;
                    } else {
                        return cas::CasOutcome::Abort(
                            String::new(),
                            format!(
                                "prd-add refused: id '{}' already exists. Pass overwrite:true to rescope it, or prd-block {{id, note}} to annotate it.",
                                id
                            ),
                            1,
                        );
                    }
                }
                None => seq.push(new_row),
            }
        } else {
            return cas::CasOutcome::Abort(
                String::new(),
                "prd.yml is not a sequence".to_string(),
                1,
            );
        }
        cas::CasOutcome::Write(doc, add_outcome)
    },
        |doc: &Value| row_has_id(doc, &verify_id),
    );
    let add_outcome = match outcome {
        Ok(u) => u,
        Err((out, err, rc)) => {
            if cas::is_lost_update(&err) {
                return (
                    serde_json::json!({
                        "ok": false,
                        "added": false,
                        "id": id,
                        "reason": err,
                    })
                    .to_string(),
                    String::new(),
                    rc,
                );
            }
            return (out, err, rc);
        }
    };
    if add_outcome != AddOutcome::AlreadyIdentical {
        invalidate_residual_marker("prd-add");
    }
    #[cfg(target_arch = "wasm32")]
    crate::wasm_dispatch::emit_event(
        "prd.added",
        serde_json::json!({
            "id": id,
            "rescoped": add_outcome == AddOutcome::Rescoped,
            "already_identical": add_outcome == AddOutcome::AlreadyIdentical,
        }),
    );
    let mut response = serde_json::Map::new();
    response.insert(
        add_outcome.response_key().to_string(),
        serde_json::Value::String(id.clone()),
    );
    if add_outcome == AddOutcome::AlreadyIdentical {
        response.insert(
            "already_identical".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    (
        serde_json::Value::Object(response).to_string(),
        String::new(),
        0,
    )
}

pub fn handle_defer(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing body: {\"id\": \"<prd-item-id>\", \"reason\": \"<why this is genuinely out of reach this session, and what session/path would resolve it>\"}".to_string(), 1);
    }
    let v: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return (
                String::new(),
                "parse failed: body must be JSON {\"id\":..,\"reason\":..}".to_string(),
                1,
            )
        }
    };
    let id_target = v
        .get("id")
        .or_else(|| v.get("prd_id"))
        .or_else(|| v.get("slug"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    let reason = v
        .get("reason")
        .or_else(|| v.get("witness_evidence"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    if id_target.is_none() || reason.is_none() {
        let mut missing: Vec<&str> = Vec::new();
        if id_target.is_none() {
            missing.push("id (string: the prd row id; aliases prd_id, slug)");
        }
        if reason.is_none() {
            missing.push("reason (string, non-empty: why this row is genuinely out of reach this session and what session/path would resolve it; alias witness_evidence) -- bare deferral language is rejected, same as prd-add");
        }
        return (
            String::new(),
            format!("prd-defer rejected: {} required field(s) missing -- every required field and its accepted shape is listed here, so fix all in one pass. Body shape: {{\"id\": \"<prd-item-id>\", \"reason\": \"<why out of reach this session, and what would resolve it>\"}}. Missing: {}", missing.len(), missing.join(" | ")),
            1,
        );
    }
    let id_target = id_target.expect("id validated non-None above");
    let reason = reason.expect("reason validated non-None above");
    if let Some(marker) = defer_marker_in_text(&reason) {
        let err = format!(
            "prd-defer refused: deferral language detected ('{}'). Same rule as prd-add's own gate -- name the concrete reason this is genuinely cross-session/out-of-reach (e.g. 'flaky multiplayer repro needs its own dedicated debugging session, unrelated to this session's rendering-pipeline fix'), not bare 'later'/'next session' phrasing with no substance.",
            marker
        );
        return (String::new(), err, 1);
    }
    let store = row_store::Store::prd();
    if !pkfs::exists(&store.dir) && !pkfs::exists(&store.legacy) {
        return (
            String::new(),
            format!("{} does not exist", store.dir),
            1,
        );
    }
    let policy = super::fsm::graph().policy.clone();
    let outcome = row_store::cas_retry_write(
        &store,
        policy.cas_max_attempts,
        "prd-defer",
        |mut doc: Value| {
            let mut found = false;
            if let Some(seq) = doc.as_sequence_mut() {
                for item in seq.iter_mut() {
                    if let Some(map) = item.as_mapping_mut() {
                        if map
                            .get(&Value::String("id".to_string()))
                            .and_then(|v| v.as_str())
                            == Some(&id_target)
                        {
                            map.insert(
                                Value::String("blockedBy".to_string()),
                                Value::Sequence(vec![Value::String("external".to_string())]),
                            );
                            map.insert(
                                Value::String("deferReason".to_string()),
                                Value::String(reason.clone()),
                            );
                            found = true;
                        }
                    }
                }
            }
            if !found {
                let body = serde_json::json!({
                    "error": format!("prd id not found: {}", id_target),
                    "deviation_kind": "prd-defer-unknown-id",
                    "deviation_severity": "deny",
                    "prd_id": id_target,
                })
                .to_string();
                return cas::CasOutcome::Abort(body, format!("prd id not found: {}", id_target), 1);
            }
            cas::CasOutcome::Write(doc, ())
        },
    );
    match outcome {
        Ok(()) => {
            invalidate_residual_marker("prd-defer");
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event(
                "prd.deferred",
                serde_json::json!({ "id": id_target, "reason": reason }),
            );
            (
                serde_json::json!({ "deferred": id_target, "blockedBy": ["external"] }).to_string(),
                String::new(),
                0,
            )
        }
        Err((out, err, rc)) => (out, err, rc),
    }
}

fn parse_resolve_target(
    trimmed: &str,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let id = v
            .get("id")
            .or_else(|| v.get("prd_id"))
            .or_else(|| v.get("mutable_id"))
            .or_else(|| v.get("item_id"))
            .or_else(|| v.get("slug"))
            .or_else(|| v.get("key"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| trimmed.to_string());
        let mut wit = v
            .get("witness_evidence")
            .or_else(|| v.get("witness"))
            .or_else(|| v.get("evidence"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let mut comment = v
            .get("commit_comment")
            .or_else(|| v.get("commit_message"))
            .or_else(|| v.get("resolution_note"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let mut witness_dispatch_id = v
            .get("witness_dispatch_id")
            .or_else(|| v.get("dispatch_id"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let cwd = v.get("cwd").and_then(|s| s.as_str()).map(|s| s.to_string());
        let mut resolution = v
            .get("resolution")
            .or_else(|| v.get("resolution_text"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let mut commit_sha = v
            .get("commit_sha")
            .or_else(|| v.get("commit"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let mut requested_status = v
            .get("status")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let id = if let Ok(inner) = serde_json::from_str::<serde_json::Value>(&id) {
            if let Some(im) = inner.as_object() {
                let recovered = im
                    .get("key")
                    .or_else(|| im.get("id"))
                    .or_else(|| im.get("prd_id"))
                    .or_else(|| im.get("slug"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                if wit.is_none() {
                    wit = im
                        .get("witness_evidence")
                        .or_else(|| im.get("witness"))
                        .or_else(|| im.get("evidence"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                if comment.is_none() {
                    comment = im
                        .get("commit_comment")
                        .or_else(|| im.get("commit_message"))
                        .or_else(|| im.get("resolution_note"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                if witness_dispatch_id.is_none() {
                    witness_dispatch_id = im
                        .get("witness_dispatch_id")
                        .or_else(|| im.get("dispatch_id"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                if resolution.is_none() {
                    resolution = im
                        .get("resolution")
                        .or_else(|| im.get("resolution_text"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                if commit_sha.is_none() {
                    commit_sha = im
                        .get("commit_sha")
                        .or_else(|| im.get("commit"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                if requested_status.is_none() {
                    requested_status = im
                        .get("status")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                recovered.unwrap_or(id)
            } else {
                id
            }
        } else {
            id
        };
        (id, wit, comment, witness_dispatch_id, cwd, resolution, commit_sha, requested_status)
    } else if let Some((id, wit)) = recover_truncated_envelope(trimmed) {
        (id, wit, None, None, None, None, None, None)
    } else {
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        let id = parts
            .first()
            .map(|s| s.to_string())
            .unwrap_or_else(|| trimmed.to_string());
        let wit = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        (id, wit, None, None, None, None, None, None)
    }
}

fn keeps_status(trimmed: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .map(|v| {
            ["keep_status", "preserve_status", "leave_pending"]
                .iter()
                .any(|k| v.get(*k).and_then(|b| b.as_bool()) == Some(true))
        })
        .unwrap_or(false)
}

fn recover_truncated_envelope(s: &str) -> Option<(String, Option<String>)> {
    let s = s.trim_start();
    if !s.starts_with('{') {
        return None;
    }
    let extract = |key: &str| -> Option<String> {
        let needle = format!("\"{}\"", key);
        let after_key = s.find(&needle)? + needle.len();
        let rest = &s[after_key..];
        let colon = rest.find(':')?;
        let after_colon = rest[colon + 1..].trim_start();
        if !after_colon.starts_with('"') {
            return None;
        }
        let val = &after_colon[1..];
        let mut out = String::new();
        let mut chars = val.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
                continue;
            }
            if c == '"' {
                return Some(out);
            }
            out.push(c);
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    };
    let id = extract("id")
        .or_else(|| extract("prd_id"))
        .or_else(|| extract("mutable_id"))
        .or_else(|| extract("item_id"))
        .or_else(|| extract("slug"))
        .or_else(|| extract("key"))?;
    let wit = extract("witness_evidence")
        .or_else(|| extract("witness"))
        .or_else(|| extract("evidence"));
    Some((id, wit))
}

fn nearest_known_id(target: &str, known: &[String]) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &String)> = None;
    for id in known {
        let d = levenshtein(target, id);
        match best {
            Some((bd, _)) if d >= bd => {}
            _ => best = Some((d, id)),
        }
    }
    best.and_then(|(d, id)| {
        let bound = (target.len().max(id.len()) / 3).max(2);
        if d <= bound {
            Some(id.clone())
        } else {
            None
        }
    })
}

fn deviation_refuses(kind: &str) -> bool {
    super::deviations::effective_severity(kind) == super::deviations::Severity::Deny
}

const WITNESS_BINDING_FIELDS: &[&str] = &[
    "witness_exit_code",
    "witness_output_sha256",
    "witness_output_path",
    "witness_ts",
];

const WITNESS_OUTPUT_MAX_BYTES: usize = 16 * 1024 * 1024;

const WITNESS_BINDING_HINT: &str = "bind the witness with witness_dispatch_id (string: the dispatch_id of your own live run in this project), or with all four of witness_exit_code (integer, must be 0), witness_output_sha256 (string: sha256 of the witness output file, 64 lowercase hex), witness_output_path (string: that output file, relative to the project root, no .. segments) and witness_ts (string: RFC 3339 timestamp). prd-resolve re-reads the file, re-hashes it and refuses on any mismatch.";

fn witness_field_shape(field: &str) -> &'static str {
    match field {
        "witness_exit_code" => "integer 0",
        "witness_output_sha256" => "string, 64 lowercase hex characters: sha256 of the witness output file",
        "witness_output_path" => "string, relative to the project root with no leading /, drive letter or .. segment, e.g. .gm/witness-out/<row>.txt",
        "witness_ts" => "string, RFC 3339 timestamp, e.g. 2026-10-09T13:44:57Z",
        "witness_dispatch_id" => "string, the dispatch_id of your own live run in this project; alone it is a complete binding",
        _ => "see accepted_fields",
    }
}

fn prd_resolve_required_fields() -> serde_json::Value {
    serde_json::json!({
        "id": "string: the row id, at top level (aliases prd_id, mutable_id, item_id, slug, key)",
        "witness_evidence": "string, non-empty: a file:line, codesearch hit or exec output specific to this row",
        "binding": "witness_dispatch_id (string), OR all four of witness_exit_code (integer 0), witness_output_sha256 (string, 64 lowercase hex), witness_output_path (string) and witness_ts (string, RFC 3339)",
    })
}

fn witness_evidence_kind(value: Option<&serde_json::Value>) -> &'static str {
    match value {
        None => "absent",
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => "empty string",
        Some(serde_json::Value::String(_)) => "string",
        Some(serde_json::Value::Null) => "null",
        Some(serde_json::Value::Bool(_)) => "boolean",
        Some(serde_json::Value::Number(_)) => "number",
        Some(serde_json::Value::Array(_)) => "array",
        Some(serde_json::Value::Object(_)) => "object",
    }
}

#[cfg(target_arch = "wasm32")]
const SPOOL_OUT_COMPLETION_LOOKBACK_MS: u64 = 300_000;
#[cfg(target_arch = "wasm32")]
const SPOOL_OUT_COMPLETION_LOOKAHEAD_MS: u64 = 60_000;
#[cfg(target_arch = "wasm32")]
const SPOOL_OUT_SCAN_MAX_FILES: usize = 400;
const REVERIFY_LISTED_PATHS: usize = 20;

#[cfg(target_arch = "wasm32")]
fn spool_out_completed_ms(out_dir: &str, name: &str) -> Option<u64> {
    let stat = crate::pkfs::stat(&format!("{out_dir}/{name}"))?;
    let mtime = stat.get("mtime_ms").or_else(|| stat.get("mtimeMs"))?;
    mtime.as_u64().or_else(|| mtime.as_f64().map(|ms| ms as u64))
}

#[cfg(target_arch = "wasm32")]
#[cfg(target_arch = "wasm32")]
fn ledger_unreadable_refusal(cwd: &str, detail: &str) -> (String, String, i32) {
    let searched = crate::dispatch_ledger::ledger_path(cwd);
    let message = format!(
        "prd-resolve refused: the dispatch ledger {} cannot be read ({}); a witness cannot be verified against it until the ledger is repaired",
        searched, detail
    );
    let body = serde_json::json!({
        "error": message,
        "ledger_searched": searched,
        "deviation_kind": "prd-resolve-ledger-unreadable",
        "deviation_severity": "deny",
    })
    .to_string();
    (body, message, 1)
}

#[cfg(target_arch = "wasm32")]
pub fn dispatch_in_spool_out(cwd: &str, dispatch_id: &str) -> bool {
    let Some(id_ts) = dispatch_id
        .split('-')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return false;
    };
    let out_dir = if cwd.is_empty() {
        ".gm/exec-spool/out".to_string()
    } else {
        format!("{}/.gm/exec-spool/out", cwd.trim_end_matches(['/', '\\']))
    };
    let Some(serde_json::Value::Array(entries)) = crate::pkfs::readdir(&out_dir) else {
        return false;
    };
    let mut candidates: Vec<(u64, String)> = entries
        .iter()
        .filter_map(|entry| {
            let name = entry
                .as_str()
                .or_else(|| entry.get("name").and_then(serde_json::Value::as_str))?;
            if !name.ends_with(".json") {
                return None;
            }
            let completed_ms = spool_out_completed_ms(&out_dir, name)?;
            let lookback_start = id_ts.saturating_sub(SPOOL_OUT_COMPLETION_LOOKBACK_MS);
            let lookahead_end = id_ts.saturating_add(SPOOL_OUT_COMPLETION_LOOKAHEAD_MS);
            let inside_window = (lookback_start..=lookahead_end).contains(&completed_ms);
            inside_window.then(|| (completed_ms, name.to_string()))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let needle = format!("\"dispatch_id\":\"{dispatch_id}\"");
    candidates
        .into_iter()
        .take(SPOOL_OUT_SCAN_MAX_FILES)
        .any(|(_, name)| {
            crate::pkfs::read_to_string(&format!("{out_dir}/{name}"))
                .is_some_and(|text| text.contains(&needle))
        })
}

fn path_is_within(path: &str, other: &str) -> bool {
    path == other || path.ends_with(&format!("/{other}")) || other.ends_with(&format!("/{path}"))
}

fn is_runtime_state_path(path: &str) -> bool {
    let anchored = format!("/{path}");
    anchored.ends_with("/.gm/witness-log.md")
        || is_prd_state_pathspec(path)
        || anchored.contains("/.gm/pool/")
        || anchored.contains("/.gm/witness-out/")
}

pub fn is_prd_yml_pathspec(path: &str) -> bool {
    path.trim().replace('\\', "/").trim_start_matches("./") == ".gm/prd.yml"
}

pub fn is_prd_state_pathspec(path: &str) -> bool {
    let trimmed = path.trim().replace('\\', "/");
    let p = trimmed.trim_start_matches("./");
    if p == ".gm/prd.yml"
        || p == ".gm/mutables.yml"
        || p == ".gm/prd-log.jsonl"
        || p == ".gm/mutables-log.jsonl"
    {
        return true;
    }
    let anchored = format!("/{p}");
    anchored.contains("/.gm/prd/") || anchored.contains("/.gm/mutables/")
}

fn prd_row_blocks(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = vec![0];
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if offset > 0 && line.starts_with("- ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).copied().unwrap_or(text.len());
            &text[start..end]
        })
        .filter(|block| !block.trim().is_empty())
        .collect()
}

fn prd_block_field(block: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    block.lines().find_map(|line| {
        let unindented = line.strip_prefix("- ").or_else(|| line.strip_prefix("  "))?;
        let value = unindented.strip_prefix(prefix.as_str())?;
        Some(value.trim().trim_matches(|c: char| c == '\'' || c == '"').to_string())
    })
}

pub fn foreign_prd_rows(head_text: &str, worktree_text: &str, session_id: &str) -> Vec<serde_json::Value> {
    let head_blocks = prd_row_blocks(head_text);
    let worktree_blocks = prd_row_blocks(worktree_text);
    let changed = worktree_blocks
        .iter()
        .filter(|block| !head_blocks.contains(*block))
        .chain(head_blocks.iter().filter(|block| !worktree_blocks.contains(*block)));
    let mut seen: Vec<(String, Option<String>)> = Vec::new();
    let mut foreign: Vec<serde_json::Value> = Vec::new();
    for block in changed {
        let owner = prd_block_field(block, "session_id");
        if owner.as_deref() == Some(session_id) {
            continue;
        }
        let id = prd_block_field(block, "id").unwrap_or_default();
        let key = (id.clone(), owner.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        foreign.push(serde_json::json!({ "id": id, "session_id": owner }));
    }
    foreign
}

pub fn own_prd_rows(head_text: &str, worktree_text: &str, session_id: &str) -> Vec<serde_json::Value> {
    let head_blocks = prd_row_blocks(head_text);
    let worktree_blocks = prd_row_blocks(worktree_text);
    let changed = worktree_blocks
        .iter()
        .filter(|block| !head_blocks.contains(*block))
        .chain(head_blocks.iter().filter(|block| !worktree_blocks.contains(*block)));
    let mut seen: Vec<(String, Option<String>)> = Vec::new();
    let mut own: Vec<serde_json::Value> = Vec::new();
    for block in changed {
        let owner = prd_block_field(block, "session_id");
        if owner.as_deref() != Some(session_id) {
            continue;
        }
        let id = prd_block_field(block, "id").unwrap_or_default();
        let key = (id.clone(), owner.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        own.push(serde_json::json!({ "id": id, "session_id": owner }));
    }
    own
}

pub fn prd_text_without_foreign_changes(
    head_text: &str,
    worktree_text: &str,
    session_id: &str,
) -> String {
    let head_blocks = prd_row_blocks(head_text);
    let worktree_blocks = prd_row_blocks(worktree_text);
    let id_of = |block: &str| prd_block_field(block, "id").unwrap_or_default();
    let mut out = String::new();
    let mut emitted: Vec<String> = Vec::new();
    for index in 0..worktree_blocks.len() {
        let block = worktree_blocks[index];
        let unchanged = head_blocks.iter().any(|hb| *hb == block);
        let owner = prd_block_field(block, "session_id");
        if !unchanged && owner.as_deref() != Some(session_id) {
            let id = id_of(block);
            if !emitted.contains(&id) {
                if let Some(position) = head_blocks.iter().position(|hb| id_of(hb) == id) {
                    out.push_str(head_blocks[position]);
                }
                emitted.push(id);
            }
            continue;
        }
        emitted.push(id_of(block));
        out.push_str(block);
    }
    for index in 0..head_blocks.len() {
        let head_block = head_blocks[index];
        let id = id_of(head_block);
        if emitted.contains(&id) {
            continue;
        }
        if prd_block_field(head_block, "session_id").as_deref() == Some(session_id) {
            continue;
        }
        emitted.push(id);
        out.push_str(head_block);
    }
    out
}

pub fn prd_text_with_missing_blocks_appended(base: &str, extra: &str) -> String {
    let base_blocks = prd_row_blocks(base);
    let mut out = base.to_string();
    let extra_blocks = prd_row_blocks(extra);
    for index in 0..extra_blocks.len() {
        let block = extra_blocks[index];
        if !base_blocks.iter().any(|bb| *bb == block) {
            out.push_str(block);
        }
    }
    out
}

fn stale_completed_rows(doc: &Value, changed: &[String]) -> Vec<(String, Vec<String>)> {
    use sha2::{Digest, Sha256};
    let Some(seq) = doc.as_sequence() else {
        return vec![];
    };
    let key = |name: &str| Value::String(name.to_string());
    seq.iter()
        .filter_map(|item| {
            let map = item.as_mapping()?;
            if map.get(&key("status"))?.as_str()? != "completed" {
                return None;
            }
            let id = map.get(&key("id"))?.as_str()?.to_string();
            let mut reasons: Vec<String> = Vec::new();
            if let Some(binding) = map.get(&key("witness_binding")).and_then(Value::as_mapping) {
                let output_path = binding.get(&key("output_path")).and_then(Value::as_str);
                let bound_sha = binding.get(&key("output_sha256")).and_then(Value::as_str);
                if let (Some(output_path), Some(bound_sha)) = (output_path, bound_sha) {
                    if changed.iter().any(|c| path_is_within(output_path, c)) {
                        match crate::pkfs::read_to_string(output_path) {
                            Some(content) => {
                                let now = format!("{:x}", Sha256::digest(content.as_bytes()));
                                if now != bound_sha {
                                    reasons.push(format!(
                                        "witness output {output_path} changed: sha256 is now {now}, bound {bound_sha}"
                                    ));
                                }
                            }
                            None => reasons.push(format!(
                                "witness output {output_path} is missing or unreadable"
                            )),
                        }
                    }
                }
            }
            if let Some(rules) = map.get(&key("rule_bindings")).and_then(Value::as_sequence) {
                for rule in rules.iter().filter_map(Value::as_mapping) {
                    let field = |name: &str| rule.get(&key(name)).and_then(Value::as_str);
                    let (Some(path), Some(anchor), Some(bound)) = (field("path"), field("anchor"), field("sha256")) else {
                        continue;
                    };
                    if changed.iter().any(|c| path_is_within(path, c)) {
                        let now = rule_section_fingerprint(path, anchor);
                        if now != bound {
                            reasons.push(format!("rule {path}#{anchor} changed: fingerprint is now {now}, bound {bound}"));
                        }
                    }
                }
            }
            for referenced in row_referenced_paths(map) {
                if let Some(module) = changed.iter().find(|c| path_is_within(&referenced, c)) {
                    reasons.push(format!("module {module} changed after this row was witnessed"));
                }
            }
            reasons.sort();
            reasons.dedup();
            (!reasons.is_empty()).then_some((id, reasons))
        })
        .collect()
}

fn reopen_stale_rows(doc: &mut Value, stale: &[(String, Vec<String>)], changed: &[String]) {
    let Some(seq) = doc.as_sequence_mut() else {
        return;
    };
    let key = |name: &str| Value::String(name.to_string());
    let listed: Vec<Value> = changed
        .iter()
        .take(REVERIFY_LISTED_PATHS)
        .cloned()
        .map(Value::String)
        .collect();
    for item in seq.iter_mut() {
        let Some(map) = item.as_mapping_mut() else {
            continue;
        };
        let Some(id) = map
            .get(&key("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some((_, reasons)) = stale.iter().find(|(stale_id, _)| *stale_id == id) else {
            continue;
        };
        map.insert(key("status"), Value::String("pending".to_string()));
        if let Some(binding) = map.remove(&key("witness_binding")) {
            map.insert(key("previous_witness_binding"), binding);
        }
        map.insert(
            key("reverify_reasons"),
            Value::Sequence(reasons.iter().cloned().map(Value::String).collect()),
        );
        map.insert(key("reverify_changed_paths"), Value::Sequence(listed.clone()));
    }
}

const HEARTBEAT_FRESH_MS: u64 = 600_000;

fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe { crate::wasm_dispatch::host_now_ms() }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

fn iso_utc_ms(text: &str) -> Option<u64> {
    let (date, time) = text.trim().trim_end_matches('Z').split_once('T')?;
    let mut date_parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let year = date_parts.next()??;
    let month = date_parts.next()??;
    let day = date_parts.next()??;
    let mut time_parts = time
        .split('.')
        .next()?
        .split(':')
        .map(|part| part.parse::<i64>().ok());
    let hour = time_parts.next()??;
    let minute = time_parts.next()??;
    let second = time_parts.next()??;
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    if seconds < 0 {
        None
    } else {
        Some(seconds as u64 * 1_000)
    }
}

fn held_row_ids(at_ms: u64) -> std::collections::HashSet<String> {
    let mut held = std::collections::HashSet::new();
    let Some(serde_json::Value::Array(entries)) = crate::pkfs::readdir(".gm/pool") else {
        return held;
    };
    for entry in &entries {
        let Some(name) = entry
            .as_str()
            .or_else(|| entry.get("name").and_then(serde_json::Value::as_str))
        else {
            continue;
        };
        if !name.ends_with(".live") {
            continue;
        }
        let Some(text) = crate::pkfs::read_to_string(&format!(".gm/pool/{name}")) else {
            continue;
        };
        let field = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key))
                .map(str::trim)
        };
        let fresh = field("start:").and_then(iso_utc_ms).is_some_and(|start| {
            at_ms
                .checked_sub(start)
                .is_some_and(|age| age < HEARTBEAT_FRESH_MS)
        });
        if !fresh {
            continue;
        }
        if let Some(rows) = field("row:") {
            for token in rows.split(|c: char| c == ',' || c == '(' || c == ')' || c.is_whitespace()) {
                if !token.is_empty() {
                    held.insert(token.to_string());
                }
            }
        }
    }
    held
}

const BROWSER_OR_GPU_WITNESS_MARKERS: &[&str] = &[
    "gpulock",
    "chromium",
    "chrome-launcher",
    "puppeteer",
    "playwright",
    "remote-debugging",
    "requestadapter",
    "webgl",
    "webgpu",
    "headless",
    "crawl_cdp",
    "cdp-browser",
];

const REVERIFY_MAX_RERUNS_PER_COMMIT: usize = 8;

fn is_node_script_path(path: &str) -> bool {
    path.starts_with("scripts/")
        && [".mjs", ".js", ".cjs"].iter().any(|ext| path.ends_with(ext))
        && !pkfs::is_absolute(path)
        && !path.split('/').any(|segment| segment == "..")
}

const WITNESS_OUT_DIR: &str = ".gm/witness-out";

pub fn persisted_witness_path(dispatch_id: &str) -> String {
    format!("{WITNESS_OUT_DIR}/witness-{dispatch_id}.mjs")
}

fn canonical_persisted_witness_path(path: &str) -> Option<String> {
    let trimmed = path.trim_start_matches("./");
    let canonical = if trimmed.starts_with("gm/witness-out/") {
        format!(".{trimmed}")
    } else if trimmed.starts_with(".gm/witness-out/") {
        trimmed.to_string()
    } else {
        return None;
    };
    let named = canonical.contains("/witness-")
        && [".mjs", ".js"].iter().any(|ext| canonical.ends_with(ext));
    let safe = !pkfs::is_absolute(path) && !path.split('/').any(|segment| segment == "..");
    (named && safe).then_some(canonical)
}

fn bound_witness_script(map: &serde_yaml::Mapping) -> Option<String> {
    let text = match map.get(&Value::String("witness".to_string())) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    text.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;|=*".contains(c))
        .filter_map(referenced_path_token)
        .find_map(|path| {
            if is_node_script_path(&path) {
                return pkfs::exists(&path).then_some(path);
            }
            let canonical = canonical_persisted_witness_path(&path)?;
            pkfs::exists(&canonical).then_some(canonical)
        })
}

fn witness_source_is_node_only(source: &str) -> bool {
    let lowered = source.to_ascii_lowercase();
    !BROWSER_OR_GPU_WITNESS_MARKERS
        .iter()
        .any(|marker| lowered.contains(*marker))
}

fn last_row_with_id<'a>(doc: &'a Value, id: &str) -> Option<&'a serde_yaml::Mapping> {
    doc.as_sequence()?
        .iter()
        .filter_map(|item| item.as_mapping())
        .filter(|map| map.get(&Value::String("id".to_string())).and_then(Value::as_str) == Some(id))
        .last()
}

fn row_status_is_completed(doc: &Value, id: &str) -> bool {
    last_row_with_id(doc, id)
        .and_then(|map| map.get(&Value::String("status".to_string())))
        .and_then(Value::as_str)
        == Some("completed")
}

fn rerun_bound_witness(script: &str) -> Result<serde_json::Value, String> {
    #[cfg(target_arch = "wasm32")]
    {
        use sha2::{Digest, Sha256};
        const TIMEOUT_MS: u64 = 60_000;
        let project_dir = crate::wasm_dispatch::host_cwd_string()
            .ok_or_else(|| "project directory unavailable".to_string())?;
        let code = format!(
            "const cp=require('child_process');const r=cp.spawnSync(process.execPath,[{script}],{{cwd:{cwd},encoding:'utf8',timeout:{timeout},windowsHide:true,maxBuffer:8388608}});process.stdout.write(JSON.stringify({{status:r.status,timed_out:!!(r.error&&r.error.code==='ETIMEDOUT'),stdout:r.stdout||''}}));",
            script = serde_json::to_string(script).unwrap_or_default(),
            cwd = serde_json::to_string(&project_dir).unwrap_or_default(),
            timeout = TIMEOUT_MS,
        );
        let options = serde_json::json!({ "timeoutMs": TIMEOUT_MS + 5_000 }).to_string();
        let packed = unsafe {
            crate::wasm_dispatch::host_exec_js(
                code.as_ptr(),
                code.len() as u32,
                options.as_ptr(),
                options.len() as u32,
            )
        };
        let envelope = crate::wasm_dispatch::unpack_to_string_pub(packed)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .ok_or_else(|| "host exec returned no envelope".to_string())?;
        if envelope.get("exit_code").and_then(serde_json::Value::as_i64) != Some(0) {
            return Err("host exec did not exit cleanly".to_string());
        }
        let inner = envelope
            .get("stdout")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .ok_or_else(|| "re-run output unreadable".to_string())?;
        let timed_out = inner
            .get("timed_out")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let status = inner.get("status").and_then(serde_json::Value::as_i64);
        let witness_stdout = inner
            .get("stdout")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let result_lines: Vec<&str> = witness_stdout
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("RESULT:"))
            .collect();
        let passed = !timed_out
            && status == Some(0)
            && result_lines
                .last()
                .is_some_and(|line| line.starts_with("RESULT: PASS"));
        let detail = if timed_out {
            format!("timed out after {}s", TIMEOUT_MS / 1000)
        } else {
            let exit = status.map_or_else(
                || "killed by signal".to_string(),
                |code| format!("exit {code}"),
            );
            match result_lines.last() {
                Some(line) => format!("{exit}, {}", line.chars().take(160).collect::<String>()),
                None => format!("{exit}, no RESULT line"),
            }
        };
        let fingerprint = format!("{:x}", Sha256::digest(script.as_bytes()));
        let dispatch_id = crate::dispatch_ledger::record(
            "",
            "witness-rerun",
            &script,
            &fingerprint,
            status.unwrap_or(-1),
            None,
            Some(witness_stdout.len() as u64),
        )?;
        Ok(serde_json::json!({
            "dispatch_id": dispatch_id,
            "passed": passed,
            "detail": detail,
        }))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = script;
        Err("witness re-run needs the wasm host".to_string())
    }
}

const PRD_BOM: char = '\u{feff}';

pub fn read_prd_text(path_s: &str) -> Option<String> {
    let raw = crate::pkfs::read_to_string(path_s)?;
    Some(raw.strip_prefix(PRD_BOM).unwrap_or(raw.as_str()).to_string())
}

fn prd_unreadable_report(path_s: &str, reason: String) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "state": "prd_unreadable",
        "path": path_s,
        "reason": reason,
        "reopened": 0,
    })]
}

pub fn reopen_rows_for_changed_paths(changed: &[String]) -> Vec<serde_json::Value> {
    let changed: Vec<String> = changed
        .iter()
        .map(|path| path.trim().replace('\\', "/").trim_start_matches("./").to_string())
        .filter(|path| !path.is_empty() && !is_runtime_state_path(path))
        .collect();
    if changed.is_empty() {
        return vec![];
    }
    let store = row_store::Store::prd();
    let blocks = match row_store::read_blocks(&store) {
        Ok(read) => read.blocks,
        Err((reason, _)) => {
            return prd_unreadable_report(
                &store.dir,
                format!("{} is unreadable: {}", store.dir, reason),
            )
        }
    };
    let doc: Value = Value::Sequence(blocks);
    let stale = stale_completed_rows(&doc, &changed);
    if stale.is_empty() {
        return vec![];
    }
    let held = held_row_ids(now_ms());
    let mut report: Vec<serde_json::Value> = Vec::new();
    let mut failed: Vec<(String, Vec<String>)> = Vec::new();
    let mut reruns = 0usize;
    for (id, reasons) in stale {
        if held.contains(&id) {
            report.push(serde_json::json!({ "id": id, "state": "deferred_held", "reasons": reasons }));
            continue;
        }
        let Some(map) = last_row_with_id(&doc, &id) else {
            continue;
        };
        let Some(script) = bound_witness_script(map) else {
            report.push(serde_json::json!({
                "id": id,
                "state": "not_rerunnable",
                "reason": "witness cites no existing scripts/*.mjs, .js or .cjs and no persisted .gm/witness-out/witness-<dispatch id>.mjs",
                "reasons": reasons,
            }));
            continue;
        };
        let node_only = crate::pkfs::read_to_string(&script)
            .as_deref()
            .map(witness_source_is_node_only);
        if node_only != Some(true) {
            report.push(serde_json::json!({
                "id": id,
                "state": "rerun_deferred",
                "script": script,
                "reason": "bound witness drives a browser or GPU arm, or its source is unreadable",
                "reasons": reasons,
            }));
            continue;
        }
        if reruns >= REVERIFY_MAX_RERUNS_PER_COMMIT {
            report.push(serde_json::json!({
                "id": id,
                "state": "rerun_deferred",
                "script": script,
                "reason": "re-run budget per commit reached",
                "reasons": reasons,
            }));
            continue;
        }
        reruns += 1;
        match rerun_bound_witness(&script) {
            Ok(run) => {
                let dispatch_id = run["dispatch_id"].as_str().unwrap_or_default().to_string();
                if run["passed"].as_bool() == Some(true) {
                    report.push(serde_json::json!({
                        "id": id,
                        "state": "rerun_pass",
                        "script": script,
                        "dispatch_id": dispatch_id,
                        "reasons": reasons,
                    }));
                } else {
                    let detail = run["detail"].as_str().unwrap_or_default();
                    let mut reopen_reasons = reasons.clone();
                    reopen_reasons.push(format!(
                        "bound witness {script} re-run dispatch {dispatch_id} FAIL: {detail}"
                    ));
                    report.push(serde_json::json!({
                        "id": id,
                        "state": "reopened",
                        "script": script,
                        "dispatch_id": dispatch_id,
                        "reasons": reopen_reasons,
                    }));
                    failed.push((id, reopen_reasons));
                }
            }
            Err(reason) => report.push(serde_json::json!({
                "id": id,
                "state": "rerun_unavailable",
                "script": script,
                "reason": reason,
                "reasons": reasons,
            })),
        }
    }
    if failed.is_empty() {
        return report;
    }
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let outcome = row_store::cas_retry_write(&store, cas_max_attempts, "prd-reverify", |mut doc: Value| {
        let still_completed: Vec<(String, Vec<String>)> = failed
            .iter()
            .filter(|(id, _)| row_status_is_completed(&doc, id))
            .cloned()
            .collect();
        if still_completed.is_empty() {
            return cas::CasOutcome::Abort(String::new(), String::new(), 0);
        }
        reopen_stale_rows(&mut doc, &still_completed, &changed);
        cas::CasOutcome::Write(doc, ())
    });
    if outcome.is_err() {
        for row in report.iter_mut() {
            if row["state"] == "reopened" {
                row["state"] = serde_json::json!("reopen_failed");
            }
        }
    }
    report
}

struct VerifiedWitness {
    exit_code: i64,
    output_sha256: String,
    output_path: String,
    ts: String,
}

impl VerifiedWitness {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "exit_code": self.exit_code,
            "output_sha256": self.output_sha256,
            "output_path": self.output_path,
            "ts": self.ts,
        })
    }

    fn to_yaml(&self) -> Value {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            Value::String("exit_code".to_string()),
            serde_yaml::to_value(self.exit_code).unwrap_or(Value::Null),
        );
        map.insert(
            Value::String("output_sha256".to_string()),
            Value::String(self.output_sha256.clone()),
        );
        map.insert(
            Value::String("output_path".to_string()),
            Value::String(self.output_path.clone()),
        );
        map.insert(
            Value::String("ts".to_string()),
            Value::String(self.ts.clone()),
        );
        Value::Mapping(map)
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_relative_project_path(path: &str) -> bool {
    !path.is_empty()
        && !pkfs::is_absolute(path)
        && !path.contains(':')
        && path.split(['/', '\\']).all(|segment| segment != "..")
}

fn is_rfc3339_timestamp(ts: &str) -> bool {
    let bytes = ts.as_bytes();
    let digits = |from: usize, len: usize| -> Option<u32> {
        let end = from.checked_add(len)?;
        let part = bytes.get(from..end)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let at = |index: usize, want: u8| bytes.get(index).copied() == Some(want);
    let (Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(5, 2),
        digits(8, 2),
        digits(11, 2),
        digits(14, 2),
        digits(17, 2),
    ) else {
        return false;
    };
    if digits(0, 4).is_none()
        || !at(4, b'-')
        || !at(7, b'-')
        || !(at(10, b'T') || at(10, b't'))
        || !at(13, b':')
        || !at(16, b':')
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return false;
    }
    let mut index = 19;
    if at(index, b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return false;
        }
    }
    match bytes.get(index).copied() {
        Some(b'Z') | Some(b'z') => index + 1 == bytes.len(),
        Some(b'+') | Some(b'-') => {
            digits(index + 1, 2).is_some_and(|h| h <= 23)
                && at(index + 3, b':')
                && digits(index + 4, 2).is_some_and(|m| m <= 59)
                && index + 6 == bytes.len()
        }
        _ => false,
    }
}

fn verify_witness_binding(
    request: &serde_json::Value,
) -> Result<VerifiedWitness, (&'static str, String)> {
    use sha2::{Digest, Sha256};
    let exit_code = match request.get("witness_exit_code").and_then(|v| v.as_i64()) {
        Some(0) => 0,
        Some(code) => {
            return Err((
                "witness_exit_code",
                format!(
                    "exit code {} is not 0 -- a witness closes a row only with exit code 0",
                    code
                ),
            ))
        }
        None => {
            return Err((
                "witness_exit_code",
                "must be an integer exit code".to_string(),
            ))
        }
    };
    let output_sha256 = request
        .get("witness_output_sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !is_sha256_hex(&output_sha256) {
        return Err((
            "witness_output_sha256",
            "must be 64 lowercase hex characters, the sha256 of the witness output file"
                .to_string(),
        ));
    }
    let output_path = request
        .get("witness_output_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !is_relative_project_path(&output_path) {
        return Err((
            "witness_output_path",
            format!(
                "must be a relative path inside the project root with no .. segments (received {:?}); copy the output file into the project, e.g. .gm/witness-out/<row>.txt, and pass that relative path",
                output_path
            ),
        ));
    }
    let ts = request
        .get("witness_ts")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !is_rfc3339_timestamp(&ts) {
        return Err((
            "witness_ts",
            "must be an RFC 3339 timestamp such as 2026-10-09T13:44:57Z".to_string(),
        ));
    }
    let content = pkfs::read_to_string(&output_path).ok_or_else(|| {
        (
            "witness_output_path",
            format!(
                "output file {} is not readable under the project root",
                output_path
            ),
        )
    })?;
    if content.len() > WITNESS_OUTPUT_MAX_BYTES {
        return Err((
            "witness_output_path",
            format!(
                "output file {} is larger than {} bytes",
                output_path, WITNESS_OUTPUT_MAX_BYTES
            ),
        ));
    }
    let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
    if digest != output_sha256 {
        return Err((
            "witness_output_sha256",
            format!(
                "sha256 of {} is {}, not the claimed {}",
                output_path, digest, output_sha256
            ),
        ));
    }
    Ok(VerifiedWitness {
        exit_code,
        output_sha256,
        output_path,
        ts,
    })
}

fn dispatch_id_in_text(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|token| {
            let mut parts = token.split('-');
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(ts), Some(seq), Some(hash), None) => {
                    ts.len() >= 10
                        && ts.bytes().all(|b| b.is_ascii_digit())
                        && !seq.is_empty()
                        && seq.bytes().all(|b| b.is_ascii_digit())
                        && (1..=16).contains(&hash.len())
                        && hash.bytes().all(|b| b.is_ascii_hexdigit())
                }
                _ => false,
            }
        })
        .map(str::to_string)
}

fn prune_receipts(id: &str) {
    let plain_segment = !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\']);
    if plain_segment {
        crate::pkfs::remove_tree(&format!(".gm/receipts/{}", id));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn scratch_temp_prefix() -> Option<String> {
    let temp = std::env::temp_dir();
    Some(format!("{}/", temp.to_string_lossy().trim_end_matches('/')))
}

#[cfg(target_arch = "wasm32")]
fn scratch_temp_prefix() -> Option<String> {
    None
}

fn scratch_witness_root(root: &str) -> Option<String> {
    let root = root.trim().trim_end_matches('/');
    let inside_tmp = scratch_temp_prefix().is_some_and(|prefix| root.strip_prefix(prefix.as_str()).is_some_and(|rest| !rest.is_empty()));
    let escapes = root.split('/').any(|segment| segment == "..");
    (inside_tmp && !escapes).then(|| root.to_string())
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing PRD item id".to_string(), 1);
    }
    let parsed = parse_resolve_target(trimmed);
    let (id_target, witness, commit_comment, witness_dispatch_id) =
        (parsed.0, parsed.1, parsed.2, parsed.3);
#[cfg(target_arch = "wasm32")]
    let resolve_cwd = parsed.4;
    let resolution = parsed.5.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    let commit_sha = parsed.6.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let requested_status = parsed.7.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let scratch_root = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|v| v.get("witness_root").and_then(|s| s.as_str()).and_then(scratch_witness_root));
    if let Some(sha) = commit_sha.as_deref() {
        if !(7..=40).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            let body = serde_json::json!({
                "error": format!(
                    "prd-resolve refused: commit_sha for {} must be 7 to 40 hex characters",
                    id_target
                ),
                "prd_id": id_target,
                "field": "commit_sha",
                "accepted_fields": ["commit_sha", "commit"],
            })
            .to_string();
            return (
                body,
                format!("prd-resolve refused: invalid commit_sha for {}", id_target),
                1,
            );
        }
    }
    let policy = super::fsm::graph().policy.clone();
    let annotate_only = keeps_status(trimmed);
    let request_live = request_object(trimmed).get("live").and_then(|v| v.as_u64());
    let count_of_record = match super::pool_slots::floor_gate(
        "prd-resolve",
        request_live,
        Some(id_target.as_str()),
    ) {
        Ok(record) => record,
        Err(denial) if !annotate_only => {
            let message = denial["error"].as_str().unwrap_or_default().to_string();
            return (denial.to_string(), message, 1);
        }
        Err(denial) => denial["count_of_record"].clone(),
    };
    let has_witness = witness
        .as_ref()
        .map(|w| !w.trim().is_empty())
        .unwrap_or(false);
    if !annotate_only
        && policy.require_witness_evidence
        && !has_witness
        && deviation_refuses("prd-resolve-no-witness")
    {
        let received = witness_evidence_kind(request_object(trimmed).get("witness_evidence"));
        let body = serde_json::json!({
            "error": format!(
                "prd-resolve refused: no witness_evidence for {} -- witness_evidence must be a non-empty string (received {})",
                id_target, received
            ),
            "deviation_kind": "prd-resolve-no-witness",
            "deviation_severity": "deny",
            "prd_id": id_target,
            "required_fields": prd_resolve_required_fields(),
            "witness_evidence_received": received,
            "hint": "resolve requires a non-empty string witness_evidence (file:line | codesearch hit | exec snippet) and a binding: witness_dispatch_id, or all four of witness_exit_code, witness_output_sha256, witness_output_path and witness_ts. A row cannot be marked completed without evidence the work is real - this gate exists because an agent under closure-pressure marked undone tasks completed with an absent witness. Body shape: {\"id\": \"<prd-item-id>\", \"witness_evidence\": \"<file:line or codesearch hit>\", \"witness_dispatch_id\": \"<dispatch_id of your own live run>\", \"commit_comment\": \"<optional one-line resolution note, bundled into the next commit message>\"}. Do the work, capture its witness, then resolve.",
        }).to_string();
        return (
            body,
            format!("prd-resolve refused: no witness_evidence for {}", id_target),
            1,
        );
    }
    let binding_request = request_object(trimmed);
    if let Some(nested) = binding_request.get("witness_binding").filter(|value| !value.is_null()) {
        let received = witness_evidence_kind(Some(nested));
        let body = serde_json::json!({
            "error": format!(
                "prd-resolve refused: witness_binding for {} is nested (received {}) -- pass the binding flat: witness_exit_code, witness_output_sha256, witness_output_path and witness_ts, or witness_dispatch_id",
                id_target, received
            ),
            "deviation_kind": "prd-resolve-witness-binding-nested",
            "deviation_severity": "deny",
            "prd_id": id_target,
            "field": "witness_binding",
            "received": received,
            "accepted_fields": ["witness_dispatch_id", "witness_exit_code", "witness_output_sha256", "witness_output_path", "witness_ts"],
            "flat_binding_fields": WITNESS_BINDING_FIELDS,
            "hint": WITNESS_BINDING_HINT,
        })
        .to_string();
        return (
            body,
            format!("prd-resolve refused: nested witness_binding for {}", id_target),
            1,
        );
    }
    let binding_present: Vec<&str> = WITNESS_BINDING_FIELDS
        .iter()
        .copied()
        .filter(|field| binding_request.get(*field).is_some_and(|v| !v.is_null()))
        .collect();
    let binding_missing: Vec<&str> = WITNESS_BINDING_FIELDS
        .iter()
        .copied()
        .filter(|field| !binding_present.contains(field))
        .collect();
    let mut witness_binding: Option<VerifiedWitness> = None;
    if !binding_present.is_empty() {
        if !binding_missing.is_empty() {
            let body = serde_json::json!({
                "error": format!(
                    "prd-resolve refused: witness binding for {} is incomplete, missing {} -- bind with witness_dispatch_id alone, or with all four of witness_exit_code, witness_output_sha256, witness_output_path and witness_ts",
                    id_target,
                    binding_missing.join(", ")
                ),
                "deviation_kind": "prd-resolve-unbound-witness",
                "deviation_severity": "deny",
                "prd_id": id_target,
                "missing_fields": binding_missing,
                "accepted_fields": ["witness_dispatch_id", "witness_exit_code", "witness_output_sha256", "witness_output_path", "witness_ts"],
                "hint": WITNESS_BINDING_HINT,
            })
            .to_string();
            return (
                body,
                format!(
                    "prd-resolve refused: incomplete witness binding for {}, missing {}; bind with witness_dispatch_id alone or with all four flat witness_* fields",
                    id_target,
                    binding_missing.join(", ")
                ),
                1,
            );
        }
        match verify_witness_binding(&binding_request) {
            Ok(verified) => witness_binding = Some(verified),
            Err((field, reason)) => {
                let body = serde_json::json!({
                    "error": format!("prd-resolve refused: {} is invalid for {}: {}", field, id_target, reason),
                    "deviation_kind": "prd-resolve-witness-binding-invalid",
                    "deviation_severity": "deny",
                    "prd_id": id_target,
                    "field": field,
                    "reason": reason,
                    "accepted_shape": witness_field_shape(field),
                    "hint": WITNESS_BINDING_HINT,
                })
                .to_string();
                return (
                    body,
                    format!(
                        "prd-resolve refused: invalid {} for {}: {}; accepted shape: {}",
                        field,
                        id_target,
                        reason,
                        witness_field_shape(field)
                    ),
                    1,
                );
            }
        }
    }
    let evidence_dispatch_id = dispatch_id_in_text(witness.as_deref().unwrap_or(""));
    #[cfg(target_arch = "wasm32")]
    let (dispatch_verified, evidence_in_ledger, bound_in_project, verified_via) = {
        let cwd = resolve_cwd.as_deref().unwrap_or("");
        let host_root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
        let known_via = |dispatch_id: &str| -> Result<Option<&'static str>, String> {
            let mut roots: Vec<&str> = vec![cwd];
            if !host_root.is_empty() && host_root.as_str() != cwd {
                roots.push(host_root.as_str());
            }
            for root in roots {
                if crate::dispatch_ledger::lookup(root, dispatch_id)?.is_some() {
                    return Ok(Some("ledger"));
                }
                if dispatch_in_spool_out(root, dispatch_id) {
                    return Ok(Some("spool_out"));
                }
                if crate::dispatch_ledger::submodule_holding(root, dispatch_id)?.is_some() {
                    return Ok(Some("nested_ledger"));
                }
            }
            if let Some(scratch) = scratch_root.as_deref() {
                if crate::dispatch_ledger::lookup_outside_root(scratch, dispatch_id)?.is_some() {
                    return Ok(Some("scratch_ledger"));
                }
            }
            Ok(None)
        };
        let evidence_in_ledger = match evidence_dispatch_id.as_deref() {
            None => None,
            Some(id) => match known_via(id) {
                Ok(via) => Some(via.is_some()),
                Err(e) => return ledger_unreadable_refusal(cwd, &e),
            },
        };
        let dispatch_verified = match witness_dispatch_id.as_deref() {
            None => false,
            Some(dispatch_id) => match known_via(dispatch_id) {
                Err(e) => return ledger_unreadable_refusal(cwd, &e),
                Ok(Some(_)) => true,
                Ok(None) if witness_binding.is_some() => false,
                Ok(None) => {
                let id_ts = dispatch_id
                    .split('-')
                    .next()
                    .and_then(|s| s.parse::<u64>().ok());
                let window = match crate::dispatch_ledger::window(cwd) {
                    Ok(w) => w,
                    Err(e) => return ledger_unreadable_refusal(cwd, &e),
                };
                let aged_out = matches!(
                    window,
                    Some((len, oldest))
                        if len >= crate::dispatch_ledger::RETAINED_ENTRIES
                            && id_ts.is_some_and(|t| t < oldest)
                );
                let kind = if aged_out {
                    "prd-resolve-unbound-witness"
                } else {
                    "prd-resolve-fabricated-dispatch"
                };
                if aged_out && !deviation_refuses(kind) {
                    false
                } else {
                    let searched = crate::dispatch_ledger::ledger_path(cwd);
                    let expected_root = if cwd.is_empty() {
                        crate::wasm_dispatch::host_cwd_string().unwrap_or_default()
                    } else {
                        cwd.to_string()
                    };
                    let (reason, hint): (&str, String) = if aged_out {
                        ("aged_out", "the ledger keeps only the most recent ledger_retained_entries dispatches of a project; this id is older than its oldest retained entry, so the dispatch ran but aged out of the window. Bind the witness instead: witness_exit_code 0, witness_output_sha256, witness_output_path and witness_ts do not depend on the ledger.".to_string())
                    } else {
                        ("not_recorded", format!("no dispatch with this id is in the ledger {searched}, and the id is not older than the ledger's oldest entry. The expected repo root is {expected_root}: the directory that holds .gm/exec-spool/.dispatch-ledger.json. A parent of the repo holds no such ledger, so pass the repo root as witness_root, or pass the dispatch_id copied from the response of the dispatch that produced the evidence, or bind the witness with witness_exit_code, witness_output_sha256, witness_output_path and witness_ts."))
                    };
                    let body = serde_json::json!({
                        "error": format!(
                            "prd-resolve refused: witness_dispatch_id {} is not in the dispatch ledger {} ({})",
                            dispatch_id, searched, reason
                        ),
                        "ledger_searched": searched,
                        "expected_root": expected_root,
                        "deviation_kind": kind,
                        "deviation_severity": "deny",
                        "prd_id": id_target,
                        "field": "witness_dispatch_id",
                        "witness_dispatch_id": dispatch_id,
                        "witness_dispatch_id_field": "witness_dispatch_id",
                        "reason": reason,
                        "ledger_retained_entries": crate::dispatch_ledger::RETAINED_ENTRIES,
                        "ledger_entries": window.map(|(len, _)| len),
                        "ledger_oldest_ts": window.map(|(_, oldest)| oldest),
                        "hint": hint,
                    })
                    .to_string();
                    return (
                        body,
                        if aged_out {
                            format!(
                                "prd-resolve refused: field witness_dispatch_id {} for {} aged out of the dispatch ledger (it keeps the most recent {} dispatches); bind the four flat witness_* fields or re-run the witness",
                                dispatch_id, id_target, crate::dispatch_ledger::RETAINED_ENTRIES
                            )
                        } else {
                            format!(
                                "prd-resolve refused: field witness_dispatch_id {} for {} is not recorded in {}; pass the dispatch_id of a live run in this project",
                                dispatch_id, id_target, searched
                            )
                        },
                        1,
                    );
                }
                }
            },
        };
        let bound_in_project = match witness_dispatch_id.as_deref() {
            None => None,
            Some(id) => match known_via(id) {
                Ok(via) => Some(via.is_some()),
                Err(e) => return ledger_unreadable_refusal(cwd, &e),
            },
        };
        let verified_via = match (dispatch_verified, bound_in_project) {
            (true, Some(true)) => witness_dispatch_id.as_deref().and_then(|id| known_via(id).ok().flatten()),
            (true, _) => Some("spool_out"),
            _ => None,
        };
        (dispatch_verified, evidence_in_ledger, bound_in_project, verified_via)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let (dispatch_verified, evidence_in_ledger, bound_in_project, verified_via) = (
        witness_dispatch_id.is_some(),
        None::<bool>,
        None::<bool>,
        None::<&'static str>,
    );
    if !annotate_only
        && witness_binding.is_none()
        && witness_dispatch_id.is_none()
        && deviation_refuses("prd-resolve-unbound-witness")
    {
        let row_store_for_lookup = row_store::Store::prd();
        let folder_blocks = match row_store::read_blocks(&row_store_for_lookup) {
            Ok(read) => read.blocks,
            Err((message, code)) => return (String::new(), message, code),
        };
        let row_present = folder_blocks
            .iter()
            .filter_map(Value::as_mapping)
            .any(|map| mapping_text(map, "id").as_deref() == Some(id_target.as_str()));
        if !row_present {
            return (
                String::new(),
                format!("prd-resolve refused: no PRD row {} in {}", id_target, row_store_for_lookup.dir),
                1,
            );
        }
        let body = serde_json::json!({
            "error": format!(
                "prd-resolve refused: witness for {} is unbound -- pass witness_dispatch_id, or all of witness_exit_code, witness_output_sha256, witness_output_path and witness_ts",
                id_target
            ),
            "deviation_kind": "prd-resolve-unbound-witness",
            "deviation_severity": "deny",
            "prd_id": id_target,
            "missing_fields": ["witness_dispatch_id", "witness_exit_code", "witness_output_sha256", "witness_output_path", "witness_ts"],
            "required_fields": prd_resolve_required_fields(),
            "witness_dispatch_id_field": "witness_dispatch_id",
            "witness_dispatch_id_in_evidence": evidence_dispatch_id,
            "witness_dispatch_id_in_ledger": evidence_in_ledger,
            "hint": WITNESS_BINDING_HINT,
        })
        .to_string();
        return (
            body,
            format!("prd-resolve refused: unbound witness for {}", id_target),
            1,
        );
    }
    let store = row_store::Store::prd();
    let existing_blocks = match row_store::read_blocks(&store) {
        Ok(read) => read.blocks,
        Err((message, code)) => return (String::new(), message, code),
    };
    if existing_blocks.is_empty() {
        return (
            String::new(),
            format!("{} holds no PRD rows", store.dir),
            1,
        );
    }
    if let Some(row) = existing_blocks.iter().filter_map(Value::as_mapping).find(|map| {
        mapping_text(map, "id").as_deref() == Some(id_target.as_str())
    }) {
        let resolver = body_session_id(&request_object(trimmed));
        let recheck_by = mapping_text(row, "recheck_by");
        let disposition_is_open = mapping_text(row, "disposition")
            .map(|d| disposition_head(&d) == "open")
            .unwrap_or(false);
        let refusal = if recheck_by.is_some() && recheck_by == resolver {
            Some(format!(
                "prd-resolve refused: {} has recheck_by equal to the resolving session; a row cannot be closed by the session that must recheck it",
                id_target
            ))
        } else if disposition_is_open {
            Some(format!(
                "prd-resolve refused: {} has disposition open; set accepted, rejected or deferred before resolving",
                id_target
            ))
        } else {
            None
        };
        if let Some(message) = refusal {
            let body = serde_json::json!({
                "error": message.clone(),
                "deviation_kind": "prd-resolve-recheck-refused",
                "deviation_severity": "deny",
                "prd_id": id_target.clone(),
            })
            .to_string();
            return (body, message, 1);
        }
    }

    if !annotate_only
        && policy.reject_duplicate_witness
        && deviation_refuses("prd-resolve-duplicate-witness")
    {
        if let Some(binding) = witness_binding.as_ref() {
            for item in &existing_blocks {
                let Some(map) = item.as_mapping() else {
                    continue;
                };
                let other_id = map
                    .get(&Value::String("id".to_string()))
                    .and_then(|v| v.as_str());
                if other_id == Some(id_target.as_str()) {
                    continue;
                }
                let other_sha = map
                    .get(&Value::String("witness_binding".to_string()))
                    .and_then(|b| b.as_mapping())
                    .and_then(|b| b.get(&Value::String("output_sha256".to_string())))
                    .and_then(|v| v.as_str());
                if other_sha == Some(binding.output_sha256.as_str()) {
                    let body = serde_json::json!({
                        "error": format!(
                            "prd-resolve refused: witness binding for {} reuses output_sha256 {}, already bound to {}. Each witness output closes one row: make the witness print a run identity (its dispatch id or a run timestamp) into its output, re-run it, and bind the new output file",
                            id_target,
                            binding.output_sha256,
                            other_id.unwrap_or("?")
                        ),
                        "deviation_kind": "prd-resolve-duplicate-witness",
                        "deviation_severity": "deny",
                        "prd_id": id_target,
                        "duplicate_of": other_id,
                        "hint": "One witness output closes one row. A witness that prints no run identity writes the same bytes on every run, so a second row cannot bind a distinct output: make the witness print its dispatch id or a run timestamp into the output file, re-run it, and bind the new file by its witness_output_sha256. A row that really shares a witness with another row should be re-scoped with prd-add, not closed with the same output file.",
                    })
                    .to_string();
                    return (
                        body,
                        format!(
                            "prd-resolve refused: duplicate witness binding for {}",
                            id_target
                        ),
                        1,
                    );
                }
            }
        }
    }
    if !annotate_only
        && policy.reject_duplicate_witness
        && deviation_refuses("prd-resolve-duplicate-witness")
    {
        if let Some(w) = witness.as_ref() {
            let trimmed_w = w.trim();
            if trimmed_w.len() >= 24 {
                for item in &existing_blocks {
                    let Some(map) = item.as_mapping() else {
                        continue;
                    };
                    let other_id = map
                        .get(&Value::String("id".to_string()))
                        .and_then(|v| v.as_str());
                    if other_id == Some(id_target.as_str()) {
                        continue;
                    }
                    let other_witness = map
                        .get(&Value::String("witness".to_string()))
                        .and_then(|v| v.as_str());
                    if other_witness
                        .map(|ow| ow.trim() == trimmed_w)
                        .unwrap_or(false)
                    {
                        let body = serde_json::json!({
                            "error": format!("prd-resolve refused: witness_evidence for {} is byte-identical to the witness already recorded for {}", id_target, other_id.unwrap_or("?")),
                            "deviation_kind": "prd-resolve-duplicate-witness",
                            "deviation_severity": "deny",
                            "prd_id": id_target,
                            "duplicate_of": other_id,
                            "hint": "Identical witness text across structurally distinct PRD rows is the rubber-stamp tell -- generic phrasing like 'code written and tested' copy-pasted across multiple ids means the rows were marked completed without each one's own real, distinct evidence. Every row's witness_evidence must be specific to what THAT row actually did: a distinct file:line, a distinct exec_js output, a distinct codesearch hit. If the rows genuinely share one piece of evidence (rare), that itself is a sign they should have been one row, not three -- re-scope via prd-add instead of resolving separately with copy-pasted text.",
                        }).to_string();
                        return (body, format!("prd-resolve refused: duplicate witness_evidence for {}", id_target), 1);
                    }
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    if !annotate_only && deviation_refuses("prd-resolve-self-witness") {
        if let Some(dispatch_id) = witness_dispatch_id.as_deref() {
            let cwd = resolve_cwd.as_deref().unwrap_or("");
            let dispatch_session = crate::dispatch_ledger::lookup(cwd, dispatch_id)
                .ok()
                .flatten()
                .and_then(|entry| entry.get("session_id")?.as_str().map(str::to_string));
            let resolving_session = body_session_id(&request_object(trimmed))
                .or_else(crate::orchestrator::state::dispatch_session_id);
            let row_session = existing_blocks
                .iter()
                .filter_map(|item| item.as_mapping())
                .find(|map| {
                    map.get(&Value::String("id".to_string())).and_then(|v| v.as_str())
                        == Some(id_target.as_str())
                })
                .and_then(|map| map.get(&Value::String("session_id".to_string())))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let self_witness = dispatch_session.as_deref().is_some_and(|ds| {
                resolving_session.as_deref() == Some(ds) || row_session.as_deref() == Some(ds)
            });
            let unattributed = dispatch_session.is_none()
                && (dispatch_in_spool_out(cwd, dispatch_id)
                    || crate::dispatch_ledger::submodule_holding(cwd, dispatch_id)
                        .ok()
                        .flatten()
                        .is_some());
            let refusal = if self_witness {
                Some((
                    "self_witness",
                    format!(
                        "prd-resolve refused: witness_dispatch_id {} was produced by session {}, the session resolving {} or its executor session -- a row cannot be witnessed by the session that resolves or executed it",
                        dispatch_id,
                        dispatch_session.as_deref().unwrap_or_default(),
                        id_target
                    ),
                ))
            } else if unattributed {
                Some((
                    "unattributed",
                    format!(
                        "prd-resolve refused: witness_dispatch_id {} has no session recorded in the dispatch ledger, so it cannot be shown to come from a session other than the one resolving {}",
                        dispatch_id, id_target
                    ),
                ))
            } else {
                None
            };
            if let Some((reason, error)) = refusal {
                let body = serde_json::json!({
                    "error": error,
                    "deviation_kind": "prd-resolve-self-witness",
                    "deviation_severity": "deny",
                    "prd_id": id_target,
                    "field": "witness_dispatch_id",
                    "reason": reason,
                    "witness_dispatch_id": dispatch_id,
                    "dispatch_session": dispatch_session,
                    "executor_session": row_session,
                    "resolving_session": resolving_session,
                    "next_verb": "prd-witness-request",
                    "hint": "The witness must be a live run dispatched under a session other than the one resolving the row and other than its executor. Dispatch prd-witness-request {id} to record the request and receive a verifier brief, run the witness from that verifier session, and resolve with its dispatch id.",
                })
                .to_string();
                return (
                    body,
                    format!("prd-resolve refused: self-witness for {}", id_target),
                    1,
                );
            }
        }
    }

    let resolved_status = if policy
        .prd_closed_statuses
        .iter()
        .any(|s| s.eq_ignore_ascii_case("completed"))
    {
        "completed".to_string()
    } else {
        policy
            .prd_closed_statuses
            .first()
            .cloned()
            .unwrap_or_else(|| "completed".to_string())
    };

    let rule_texts: Vec<String> = [witness.clone(), resolution.clone(), commit_comment.clone()]
        .into_iter()
        .flatten()
        .collect();
    let outcome = row_store::cas_retry_write_verified(
        &store,
        policy.cas_max_attempts,
        "prd-resolve",
        |mut doc: Value| {
            let mut found = false;
            let mut status_matches = true;
            if let Some(seq) = doc.as_sequence_mut() {
                let last_match = seq
                    .iter_mut()
                    .enumerate()
                    .filter(|(_, item)| {
                        item.as_mapping()
                            .and_then(|map| map.get(&Value::String("id".to_string())))
                            .and_then(|v| v.as_str())
                            == Some(&id_target)
                    })
                    .map(|(index, _)| index)
                    .last();
                if let Some(index) = last_match {
                    if let Some(map) = seq
                        .iter_mut()
                        .nth(index)
                        .and_then(|item| item.as_mapping_mut())
                    {
                        if !annotate_only {
                            map.insert(
                                Value::String("status".to_string()),
                                Value::String(resolved_status.clone()),
                            );
                        }
                        if let Some(w) = witness.as_ref() {
                            map.insert(
                                Value::String("witness".to_string()),
                                Value::String(w.clone()),
                            );
                        }
                        if let Some(binding) = witness_binding.as_ref() {
                            map.insert(
                                Value::String("witness_binding".to_string()),
                                binding.to_yaml(),
                            );
                        }
                        if let Some(c) = commit_comment.as_ref() {
                            map.insert(
                                Value::String("commit_comment".to_string()),
                                Value::String(c.clone()),
                            );
                        }
                        if !annotate_only {
                            map.insert(
                                Value::String("rule_bindings".to_string()),
                                rule_bindings_value(&rule_texts),
                            );
                        }
                        if let Some(r) = resolution.as_ref() {
                            map.insert(
                                Value::String("resolution".to_string()),
                                Value::String(r.clone()),
                            );
                        }
                        if let Some(sha) = commit_sha.as_ref() {
                            map.insert(
                                Value::String("commit_sha".to_string()),
                                Value::String(sha.clone()),
                            );
                        }
                        status_matches = requested_status.as_ref().map_or(true, |want| {
                            map.get(&Value::String("status".to_string()))
                                .and_then(|v| v.as_str())
                                .is_some_and(|have| have.eq_ignore_ascii_case(want.as_str()))
                        });
                        found = true;
                    }
                }
            }
            if !found {
                let logged = match row_store::log_row(&store, &id_target) {
                    Ok(logged) => logged,
                    Err(message) => return cas::CasOutcome::Abort(String::new(), message, 1),
                };
                if let Some(logged) = logged {
                    let status = logged.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let body = serde_json::json!({
                        "error": format!(
                            "prd-resolve refused: {} is already resolved (status {}); its record is in .gm/prd-log.jsonl, read it with prd-list id={}",
                            id_target, status, id_target
                        ),
                        "deviation_kind": "prd-resolve-already-resolved",
                        "deviation_severity": "deny",
                        "prd_id": id_target,
                        "status": status,
                        "resolved_at": logged.get("resolved_at").cloned().unwrap_or(serde_json::Value::Null),
                    })
                    .to_string();
                    return cas::CasOutcome::Abort(body, format!("prd-resolve refused: {} already resolved", id_target), 1);
                }
                let mut known_ids: Vec<String> = Vec::new();
                if let Some(seq) = doc.as_sequence() {
                    for item in seq {
                        if let Some(m) = item.as_mapping() {
                            if let Some(id_v) = m.get(&Value::String("id".to_string())) {
                                if let Some(id_s) = id_v.as_str() {
                                    known_ids.push(id_s.to_string());
                                }
                            }
                        }
                    }
                }
                let suggested_id = nearest_known_id(&id_target, &known_ids);
                let body = serde_json::json!({
                "error": format!("prd id not found: {}", id_target),
                "deviation_kind": "prd-resolve-unknown-id",
                "deviation_severity": "deny",
                "prd_id": id_target,
                "known_ids": known_ids,
                "suggested_id": suggested_id,
                "hint": "body shape: {\"id\": \"<prd-item-id>\", \"witness_evidence\": \"<file:line or codesearch hit>\", \"commit_comment\": \"<optional one-line resolution note>\", \"keep_status\": true (optional: record witness/commit_comment on the row WITHOUT completing it; status is left as-is and no witness is required; aliases preserve_status, leave_pending)}; aliases accepted: prd_id, mutable_id, item_id, slug, key (all map to id); commit_message, resolution_note (map to commit_comment). commit_comment is optional -- when present it rides on the row until the next git_commit/git_finalize bundles it into that commit's message and clears the row. A nested envelope (prd_id holding a stringified {\"key\":..,\"witness\":..} object) is unwrapped automatically and the inner key/id/prd_id/slug is recovered. Raw text body: first whitespace-delimited token = id, rest = witness_evidence. If `suggested_id` is non-null it is the closest known id to what you passed -- likely a typo; re-dispatch with it. If the recovered id is not in `known_ids` above, the row was never `prd-add`ed in this chain -- your next dispatch is `prd-add` with this id, THEN `prd-resolve`. Do not invent ids; resolve only what was added; never retry the same unknown id unchanged.",
            }).to_string();
                return cas::CasOutcome::Abort(body, format!("prd id not found: {}", id_target), 1);
            }
            cas::CasOutcome::Write(doc, status_matches)
        },
        |doc: &Value| {
            row_has_id(doc, &id_target)
                && (annotate_only
                    || row_status_for(doc, &id_target).as_deref() == Some(resolved_status.as_str()))
        },
    );
    match outcome {
        Ok(status_matches) => {
            let filed = if annotate_only {
                None
            } else {
                match row_store::retire_to_log(
                    &store,
                    &id_target,
                    &resolved_status,
                    resolution.as_deref(),
                ) {
                    Ok(entry) => {
                        prune_receipts(&id_target);
                        Some(entry)
                    }
                    Err(reason) => {
                        return (
                            String::new(),
                            format!(
                                "prd-resolve: {} was set to {} but could not be moved into {}: {}",
                                id_target, resolved_status, store.log, reason
                            ),
                            1,
                        )
                    }
                }
            };
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event(
                if annotate_only {
                    "prd.annotated"
                } else {
                    "prd.resolved"
                },
                serde_json::json!({ "id": id_target }),
            );
            let outcome_key = if annotate_only { "annotated" } else { "resolved" };
            let mut reply = serde_json::json!({
                outcome_key: id_target,
                "status_kept": status_matches,
                "commit_comment_attached": commit_comment.is_some() || commit_sha.is_some(),
                "resolution_attached": resolution.is_some(),
                "commit_sha_attached": commit_sha.is_some(),
                "witness_bound": witness_binding.is_some() || dispatch_verified,
                "witness_binding": witness_binding.as_ref().map(VerifiedWitness::to_json),
                "witness_dispatch_id_verified": dispatch_verified,
                "witness_dispatch_id_verified_via": verified_via,
                "witness_root": (if verified_via == Some("scratch_ledger") { scratch_root.clone() } else { None }),
                "witness_dispatch_id_field": "witness_dispatch_id",
                "witness_dispatch_id_in_evidence": witness_dispatch_id,
                "witness_dispatch_id_in_ledger": bound_in_project,
                "count_of_record": count_of_record,
            });
            if let Some(entry) = filed {
                if let Some(map) = reply.as_object_mut() {
                    map.insert(
                        "filed_to".to_string(),
                        serde_json::Value::String(format!(".gm/{}", "prd-log.jsonl")),
                    );
                    map.insert("filed_at".to_string(), entry.get("ts").cloned().unwrap_or(serde_json::Value::Null));
                    map.insert(
                        "row_file_cleared".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
            }
            (reply.to_string(), String::new(), 0)
        }
        Err((out, err, rc)) => (out, err, rc),
    }
}

pub fn handle_block(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str::<Value>(content.trim()) {
        Ok(v) => v,
        Err(_) => return (String::new(), "prd-block body must be JSON {id, note}".to_string(), 1),
    };
    let (Some(id), Some(note)) = (
        body.get("id").and_then(Value::as_str).map(str::to_string),
        body.get("note").and_then(Value::as_str).map(str::to_string),
    ) else {
        return (String::new(), "prd-block requires id and note".to_string(), 1);
    };
    let store = row_store::Store::prd();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let outcome = row_store::cas_retry_write(&store, cas_max_attempts, "prd-block", |mut doc: Value| {
        let Some(seq) = doc.as_sequence_mut() else {
            return cas::CasOutcome::Abort(String::new(), "the PRD store is not a sequence".to_string(), 1);
        };
        let key = Value::String("id".to_string());
        let Some(row) = seq
            .iter_mut()
            .find(|it| it.as_mapping().and_then(|m| m.get(&key)).and_then(|v| v.as_str()) == Some(id.as_str()))
        else {
            return cas::CasOutcome::Abort(String::new(), format!("no PRD row with id {}", id), 1);
        };
        let Some(map) = row.as_mapping_mut() else {
            return cas::CasOutcome::Abort(String::new(), "PRD row is not a mapping".to_string(), 1);
        };
        let notes_key = Value::String("blocker_notes".to_string());
        let mut notes = map
            .get(&notes_key)
            .and_then(|v| v.as_sequence().cloned())
            .unwrap_or_default();
        notes.push(Value::String(note.clone()));
        map.insert(notes_key, Value::Sequence(notes));
        cas::CasOutcome::Write(doc, ())
    });
    match outcome {
        Ok(()) => (serde_json::json!({"ok": true, "blocked_annotated": id}).to_string(), String::new(), 0),
        Err((out, err, rc)) => (out, err, rc),
    }
}

fn json_body(content: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(content.trim()).unwrap_or(serde_json::Value::Null)
}

fn body_str(body: &serde_json::Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        body.get(*field)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

pub fn handle_witness_request(content: &str) -> (String, String, i32) {
    let body = json_body(content);
    let Some(id) = body_str(&body, &["id", "prd_id", "row"]) else {
        return (
            String::new(),
            "prd-witness-request requires {\"id\":\"<prd row id>\"}; optional \"note\" names what the verifier must observe".to_string(),
            1,
        );
    };
    let note = body_str(&body, &["note", "expect", "observe"]);
    let requester = super::state::dispatch_session_id();
    let ts = super::state::now_ms() as u64;
    let store = row_store::Store::prd();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let outcome = row_store::cas_retry_write(&store, cas_max_attempts, "prd-witness-request", |mut doc: Value| {
        let Some(seq) = doc.as_sequence_mut() else {
            return cas::CasOutcome::Abort(String::new(), "the PRD store is not a sequence".to_string(), 1);
        };
        let key = Value::String("id".to_string());
        let Some(row) = seq
            .iter_mut()
            .find(|it| it.as_mapping().and_then(|m| m.get(&key)).and_then(|v| v.as_str()) == Some(id.as_str()))
        else {
            return cas::CasOutcome::Abort(String::new(), format!("prd-witness-request refused: no open PRD row with id {}", id), 1);
        };
        let Some(map) = row.as_mapping_mut() else {
            return cas::CasOutcome::Abort(String::new(), "PRD row is not a mapping".to_string(), 1);
        };
        let executor = map
            .get(&Value::String("session_id".to_string()))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut request = serde_yaml::Mapping::new();
        request.insert(Value::String("ts_ms".to_string()), Value::Number(ts.into()));
        if let Some(r) = requester.as_deref() {
            request.insert(Value::String("requested_by".to_string()), Value::String(r.to_string()));
        }
        if let Some(n) = note.as_deref() {
            request.insert(Value::String("note".to_string()), Value::String(n.to_string()));
        }
        map.insert(Value::String("witness_request".to_string()), Value::Mapping(request));
        cas::CasOutcome::Write(doc, executor)
    });
    let executor = match outcome {
        Ok(v) => v,
        Err((out, err, rc)) => return (out, err, rc),
    };
    invalidate_residual_marker("prd-witness-request");
    let excluded: Vec<&str> = [executor.as_deref(), requester.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let reply = serde_json::json!({
        "witness_requested": id,
        "requested_by": requester,
        "executor_session": executor,
        "excluded_sessions": excluded,
        "note": note,
        "verifier_brief": format!(
            "Load gm-prd with row={}; session=<a new session id, not {}>; mode=witness. Re-run the row's witness live{}, then prd-resolve {{\"id\":\"{}\",\"witness_evidence\":\"<observed output>\",\"witness_dispatch_id\":\"<your dispatch id>\"}}.",
            id,
            if excluded.is_empty() { "the executor".to_string() } else { excluded.join(" or ") },
            note.as_deref().map(|n| format!(" observing: {}", n)).unwrap_or_default(),
            id
        ),
    });
    (reply.to_string(), String::new(), 0)
}

pub fn expansion_audit_log_path() -> String {
    gm_dir().join("expansion-audit.jsonl").to_string_lossy().replace('\\', "/")
}

fn body_count(body: &serde_json::Value, field: &str) -> Result<Option<u64>, String> {
    match body.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("expansion-audit refused: field {} must be a non-negative integer", field)),
    }
}

pub fn handle_expansion_audit(content: &str) -> (String, String, i32) {
    let body = json_body(content);
    let log_path = expansion_audit_log_path();
    let list_only = body.get("list").and_then(|v| v.as_bool()) == Some(true);
    if list_only {
        let text = pkfs::read_to_string(&log_path).unwrap_or_default();
        let entries: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let tail = entries[entries.len().saturating_sub(limit)..].to_vec();
        return (
            serde_json::json!({
                "log": ".gm/expansion-audit.jsonl",
                "total": entries.len(),
                "latest": entries.last(),
                "entries": tail,
            })
            .to_string(),
            String::new(),
            0,
        );
    }
    let Some(scope) = body_str(&body, &["scope", "goal", "subject"]) else {
        return (
            String::new(),
            "expansion-audit requires {\"scope\":\"<what was swept>\",\"rows_added\":<n>,\"unknowns_added\":<n>}; optional \"result\" (fixed_point|expanded), \"note\"; {\"list\":true} reads the log".to_string(),
            1,
        );
    };
    let (rows_added, unknowns_added) = match (body_count(&body, "rows_added"), body_count(&body, "unknowns_added")) {
        (Ok(Some(r)), Ok(Some(u))) => (r, u),
        (Err(e), _) | (_, Err(e)) => return (String::new(), e, 1),
        (Ok(None), _) => return (String::new(), "expansion-audit refused: field rows_added is required (rows this sweep filed with prd-add)".to_string(), 1),
        (_, Ok(None)) => return (String::new(), "expansion-audit refused: field unknowns_added is required (unknowns this sweep filed with mutable-add)".to_string(), 1),
    };
    let derived = if rows_added == 0 && unknowns_added == 0 { "fixed_point" } else { "expanded" };
    if let Some(claimed) = body_str(&body, &["result"]) {
        if claimed != derived {
            return (
                String::new(),
                format!(
                    "expansion-audit refused: field result is {} but rows_added={} and unknowns_added={} make it {}",
                    claimed, rows_added, unknowns_added, derived
                ),
                1,
            );
        }
    }
    let policy = super::fsm::graph().policy.clone();
    let prd_open = match row_store::read_blocks(&row_store::Store::prd()) {
        Ok(read) => read
            .blocks
            .iter()
            .filter(|row| {
                let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                !policy.prd_closed_statuses.iter().any(|s| s == status)
            })
            .count(),
        Err((message, code)) => return (String::new(), message, code),
    };
    let unknowns_open = super::mutables::pending_detailed().len();
    let end_state = derived == "fixed_point" && prd_open == 0 && unknowns_open == 0;
    let entry = serde_json::json!({
        "id": format!("expansion-audit:{}", super::state::now_ms()),
        "scope": scope,
        "rows_added": rows_added,
        "unknowns_added": unknowns_added,
        "result": derived,
        "prd_open": prd_open,
        "unknowns_open": unknowns_open,
        "verifiable_end_state": end_state,
        "note": body_str(&body, &["note"]),
    });
    match row_store::append_chained(&log_path, "expansion-audit", entry) {
        Ok(recorded) => (
            serde_json::json!({
                "recorded": recorded,
                "log": ".gm/expansion-audit.jsonl",
                "verifiable_end_state": end_state,
                "hint": if end_state {
                    "fixed point with zero open rows and unknowns: a goal stop may cite this entry's sha256"
                } else if derived == "expanded" {
                    "the sweep expanded scope: drain the new rows, then sweep again and record another audit"
                } else {
                    "fixed point reached but rows or unknowns are still open: drain them before a goal stop"
                },
            })
            .to_string(),
            String::new(),
            0,
        ),
        Err(e) => (String::new(), format!("expansion-audit: {}", e), 1),
    }
}

