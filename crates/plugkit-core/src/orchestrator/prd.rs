use super::cas;
use super::gm_dir;
use super::yaml_util::{
    defer_marker_in_text, invalidate_residual_marker, levenshtein, yaml_to_json,
};
use crate::pkfs;
use serde_yaml::Value;

pub fn prd_path() -> std::path::PathBuf {
    gm_dir().join("prd.yml")
}

pub fn prd_path_for(cwd: Option<&str>) -> std::path::PathBuf {
    match cwd {
        Some(c) => std::path::Path::new(c).join(".gm").join("prd.yml"),
        None => prd_path(),
    }
}

pub fn pending_commit_comments_for_paths(cwd: Option<&str>, touched: &[String]) -> Vec<(String, String)> {
    let path = prd_path_for(cwd);
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return Vec::new();
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(seq) = doc.as_sequence() {
        for item in seq {
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
                out.push(path);
            }
        }
    }
    out
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
    let path = prd_path_for(cwd);
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return;
    }
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let _ = cas::cas_retry_write(
        &path_s,
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

const PRD_ROW_STATUS_FIELDS: &[&str] = &["status"];

const PRD_FULL_BODY_FIELDS: &[&str] = &["full", "verbose", "bodies"];

const PRD_BRIEF_BODY_FIELDS: &[&str] = &["brief", "compact"];

const PRD_TITLE_FIELDS: &[&str] = &["title", "subject"];

fn load_rows() -> Result<Vec<serde_json::Value>, (String, i32)> {
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return Ok(Vec::new());
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return Err(("read failed".to_string(), 1)),
    };
    let doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return Err((format!("parse failed: {}", e), 1)),
    };
    let seq = doc
        .as_sequence()
        .cloned()
        .or_else(|| doc.get("items").and_then(|v| v.as_sequence()).cloned())
        .unwrap_or_default();
    Ok(seq.iter().filter_map(row_to_json).collect())
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

fn row_text(row: &serde_json::Value, fields: &[&str]) -> String {
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

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clamp_chars(s: &str, max: usize) -> String {
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

fn request_object(content: &str) -> serde_json::Value {
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

fn request_strings(request: &serde_json::Value, fields: &[&str]) -> Vec<String> {
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

fn request_flag(request: &serde_json::Value, fields: &[&str]) -> Option<bool> {
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
    let wanted_statuses = request_strings(&request, PRD_ROW_STATUS_FIELDS);
    let selected: Vec<serde_json::Value> = fold_last_blocks(rows)
        .into_iter()
        .filter(|row| {
            let id = row_text(row, &["id"]);
            let status = row_status(row);
            let id_ok = wanted_ids.is_empty() || wanted_ids.iter().any(|want| want == &id);
            let status_ok =
                wanted_statuses.is_empty() || wanted_statuses.iter().any(|want| want == &status);
            id_ok && status_ok
        })
        .collect();
    if !wanted_ids.is_empty() && selected.is_empty() {
        return (
            String::new(),
            format!(
                "no PRD row with id {} of the {} rows in .gm/prd.yml -- pass no id for the listing",
                wanted_ids.join(", "),
                store_total
            ),
            1,
        );
    }
    let matched = selected.len();
    let shown = limit.map_or(matched, |n| n.min(matched));
    let truncated = shown < matched;
    let window: Vec<serde_json::Value> = selected.into_iter().take(shown).collect();
    let brief_flag = request_flag(&request, PRD_BRIEF_BODY_FIELDS);
    let full_flag = request_flag(&request, PRD_FULL_BODY_FIELDS);
    let want_full =
        full_flag.unwrap_or_else(|| brief_flag.map(|b| !b).unwrap_or(!wanted_ids.is_empty()));
    let items: Vec<serde_json::Value> = if want_full {
        window
    } else {
        window.iter().map(brief_row).collect()
    };
    let reply = serde_json::json!({
        "items": items,
        "count": items.len(),
        "total": matched,
        "store_total": store_total,
        "limit": limit,
        "truncated": truncated,
        "brief": !want_full,
        "full": want_full,
        "hint": "pass {\"id\":\"<row id>\"} for one full row (repeated blocks of an id fold to the last one), {\"status\":\"pending\"} to filter, {\"status\":\"pending\",\"limit\":<n>} to cap the listing (total counts every match), or {\"full\":true} for every full body",
    });
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

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (
            String::new(),
            "missing body: provide PRD item as JSON or YAML".to_string(),
            1,
        );
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
    let item_map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => {
            return (
                String::new(),
                "item must be a mapping with id/subject/status".to_string(),
                1,
            )
        }
    };
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
                "hint": "Pass `id` in prd-add body. No usable text in any of id/slug/prd_id or subject/title/name/task/goal/description/notes -- every one was empty or unslugifiable, so the row was REJECTED. An item-<ms> fallback cannot be referenced by intent in recall or prd-resolve, so it is never admitted. Pass `id` directly, or put the intent in any of subject/title/name/task/goal/description so slug derivation succeeds.",
            }),
        );
        let err = "PRD item rejected: no usable `id` and no slugifiable text in subject/title/name/task/goal/description/notes. A referenceable handle is mandatory -- every later prd-resolve / recall names the row by id. Pass `id` directly (kebab-case slug derived from intent) or provide a meaningful subject/title/description. Auto `item-<ms>` ids are not admitted because they cannot be referenced by intent.";
        return (String::new(), err.to_string(), 1);
    }
    let id = provided_id
        .clone()
        .or_else(|| slug.clone())
        .unwrap_or_else(|| format!("item-{}", crate::orchestrator::state::now_ms()));
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let overwrite = item_map
        .get(&Value::String("overwrite".to_string()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let outcome = cas::cas_retry_write(&path_s, cas_max_attempts, "prd-add", |mut doc: Value| {
        let mut add_outcome = AddOutcome::Added;
        if let Some(seq) = doc.as_sequence_mut() {
            let mut new_with_id = item_map.clone();
            new_with_id.remove(&Value::String("overwrite".to_string()));
            new_with_id.insert(Value::String("id".to_string()), Value::String(id.clone()));
            if !new_with_id.contains_key(&Value::String("status".to_string())) {
                new_with_id.insert(
                    Value::String("status".to_string()),
                    Value::String("pending".to_string()),
                );
            }
            let new_row = Value::Mapping(new_with_id);
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
    });
    let add_outcome = match outcome {
        Ok(u) => u,
        Err((out, err, rc)) => return (out, err, rc),
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
    let id_target = match v
        .get("id")
        .or_else(|| v.get("prd_id"))
        .or_else(|| v.get("slug"))
        .and_then(|s| s.as_str())
    {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (String::new(), "missing `id`".to_string(), 1),
    };
    let reason = match v.get("reason").or_else(|| v.get("witness_evidence")).and_then(|s| s.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (String::new(), "missing `reason`: name why this row is genuinely out of reach this session and what would resolve it -- bare deferral language is rejected, same as prd-add".to_string(), 1),
    };
    if let Some(marker) = defer_marker_in_text(&reason) {
        let err = format!(
            "prd-defer refused: deferral language detected ('{}'). Same rule as prd-add's own gate -- name the concrete reason this is genuinely cross-session/out-of-reach (e.g. 'flaky multiplayer repro needs its own dedicated debugging session, unrelated to this session's rendering-pipeline fix'), not bare 'later'/'next session' phrasing with no substance.",
            marker
        );
        return (String::new(), err, 1);
    }
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (
            String::new(),
            format!("{} does not exist", path.display()),
            1,
        );
    }
    let policy = super::fsm::graph().policy;
    let outcome = cas::cas_retry_write(
        &path_s,
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

const WITNESS_BINDING_HINT: &str = "bind the witness with witness_exit_code (integer, must be 0), witness_output_sha256 (sha256 of the witness output file, 64 lowercase hex), witness_output_path (that output file, relative to the project root, no .. segments) and witness_ts (RFC 3339 timestamp). prd-resolve re-reads the file, re-hashes it and refuses on any mismatch.";

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
            "must be a relative path inside the project root with no .. segments".to_string(),
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
    let policy = super::fsm::graph().policy;
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
        let body = serde_json::json!({
            "error": format!("prd-resolve refused: no witness_evidence for {}", id_target),
            "deviation_kind": "prd-resolve-no-witness",
            "deviation_severity": "deny",
            "prd_id": id_target,
            "hint": "resolve requires non-empty witness_evidence (file:line | codesearch hit | exec snippet). A row cannot be marked completed without evidence the work is real - this gate exists because an agent under closure-pressure marked undone tasks completed with an absent witness. Body shape: {\"id\": \"<prd-item-id>\", \"witness_evidence\": \"<file:line or codesearch hit>\", \"commit_comment\": \"<optional one-line resolution note, bundled into the next commit message>\"}. Do the work, capture its witness, then resolve.",
        }).to_string();
        return (
            body,
            format!("prd-resolve refused: no witness_evidence for {}", id_target),
            1,
        );
    }
    let binding_request = request_object(trimmed);
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
                    "prd-resolve refused: witness binding for {} is incomplete, missing {}",
                    id_target,
                    binding_missing.join(", ")
                ),
                "deviation_kind": "prd-resolve-unbound-witness",
                "deviation_severity": "deny",
                "prd_id": id_target,
                "missing_fields": binding_missing,
                "hint": WITNESS_BINDING_HINT,
            })
            .to_string();
            return (
                body,
                format!(
                    "prd-resolve refused: incomplete witness binding for {}",
                    id_target
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
                    "hint": WITNESS_BINDING_HINT,
                })
                .to_string();
                return (
                    body,
                    format!("prd-resolve refused: invalid {} for {}", field, id_target),
                    1,
                );
            }
        }
    }
    let evidence_dispatch_id = dispatch_id_in_text(witness.as_deref().unwrap_or(""));
    #[cfg(target_arch = "wasm32")]
    let (dispatch_verified, evidence_in_ledger) = {
        let cwd = resolve_cwd.as_deref().unwrap_or("");
        let evidence_in_ledger = evidence_dispatch_id
            .as_deref()
            .map(|id| crate::dispatch_ledger::lookup(cwd, id).is_some());
        let dispatch_verified = match witness_dispatch_id.as_deref() {
            None => false,
            Some(dispatch_id) if crate::dispatch_ledger::lookup(cwd, dispatch_id).is_some() => {
                true
            }
            Some(_) if witness_binding.is_some() => false,
            Some(dispatch_id) => {
                let id_ts = dispatch_id
                    .split('-')
                    .next()
                    .and_then(|s| s.parse::<u64>().ok());
                let window = crate::dispatch_ledger::window(cwd);
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
                    let (reason, hint) = if aged_out {
                        ("aged_out", "the ledger keeps only the most recent ledger_retained_entries dispatches of a project; this id is older than its oldest retained entry, so the dispatch ran but aged out of the window. Bind the witness instead: witness_exit_code 0, witness_output_sha256, witness_output_path and witness_ts do not depend on the ledger.")
                    } else {
                        ("not_recorded", "no dispatch with this id is in this project's .gm/exec-spool/.dispatch-ledger.json and the id is not older than the ledger's oldest entry, so the cwd may be another project or the id may be invented. Pass the dispatch_id copied from the response of the dispatch that produced the evidence, or bind the witness with witness_exit_code, witness_output_sha256, witness_output_path and witness_ts.")
                    };
                    let body = serde_json::json!({
                        "error": format!(
                            "prd-resolve refused: witness_dispatch_id {} is not in this project's dispatch ledger ({})",
                            dispatch_id, reason
                        ),
                        "deviation_kind": kind,
                        "deviation_severity": "deny",
                        "prd_id": id_target,
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
                        format!(
                            "prd-resolve refused: witness_dispatch_id {} not in the dispatch ledger for {}",
                            dispatch_id, id_target
                        ),
                        1,
                    );
                }
            }
        };
        (dispatch_verified, evidence_in_ledger)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let (dispatch_verified, evidence_in_ledger) = (witness_dispatch_id.is_some(), None::<bool>);
    if !annotate_only
        && witness_binding.is_none()
        && witness_dispatch_id.is_none()
        && deviation_refuses("prd-resolve-unbound-witness")
    {
        let body = serde_json::json!({
            "error": format!(
                "prd-resolve refused: witness for {} is unbound -- pass witness_dispatch_id, or all of witness_exit_code, witness_output_sha256, witness_output_path and witness_ts",
                id_target
            ),
            "deviation_kind": "prd-resolve-unbound-witness",
            "deviation_severity": "deny",
            "prd_id": id_target,
            "missing_fields": ["witness_dispatch_id", "witness_exit_code", "witness_output_sha256", "witness_output_path", "witness_ts"],
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
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (
            String::new(),
            format!("{} does not exist", path.display()),
            1,
        );
    }

    if !annotate_only
        && policy.reject_duplicate_witness
        && deviation_refuses("prd-resolve-duplicate-witness")
    {
        if let Some(binding) = witness_binding.as_ref() {
            if let Some(existing) = pkfs::read_to_string(&path_s) {
                if let Ok(doc) = serde_yaml::from_str::<Value>(&existing) {
                    if let Some(seq) = doc.as_sequence() {
                        for item in seq {
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
                                        "prd-resolve refused: witness binding for {} reuses output_sha256 {}, already bound to {}",
                                        id_target,
                                        binding.output_sha256,
                                        other_id.unwrap_or("?")
                                    ),
                                    "deviation_kind": "prd-resolve-duplicate-witness",
                                    "deviation_severity": "deny",
                                    "prd_id": id_target,
                                    "duplicate_of": other_id,
                                    "hint": "One witness output closes one row. A row that really shares a witness with another row should be re-scoped with prd-add, not closed with the same output file.",
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
                if let Some(existing) = pkfs::read_to_string(&path_s) {
                    if let Ok(doc) = serde_yaml::from_str::<Value>(&existing) {
                        if let Some(seq) = doc.as_sequence() {
                            for item in seq {
                                if let Some(map) = item.as_mapping() {
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
                }
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

    let outcome = cas::cas_retry_write(
        &path_s,
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
    );
    match outcome {
        Ok(status_matches) => {
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
            let reply = serde_json::json!({
                outcome_key: id_target,
                "status_kept": status_matches,
                "commit_comment_attached": commit_comment.is_some() || commit_sha.is_some(),
                "resolution_attached": resolution.is_some(),
                "commit_sha_attached": commit_sha.is_some(),
                "witness_bound": witness_binding.is_some(),
                "witness_binding": witness_binding.as_ref().map(VerifiedWitness::to_json),
                "witness_dispatch_id_verified": dispatch_verified,
                "witness_dispatch_id_field": "witness_dispatch_id",
                "witness_dispatch_id_in_evidence": witness_dispatch_id,
                "witness_dispatch_id_in_ledger": witness_dispatch_id.as_ref().map(|_| dispatch_verified),
                "count_of_record": count_of_record,
            });
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
    let path_s = prd_path().to_string_lossy().to_string();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let outcome = cas::cas_retry_write(&path_s, cas_max_attempts, "prd-block", |mut doc: Value| {
        let Some(seq) = doc.as_sequence_mut() else {
            return cas::CasOutcome::Abort(String::new(), "prd.yml is not a sequence".to_string(), 1);
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
