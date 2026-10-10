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

pub fn peek_pending_commit_comments(cwd: Option<&str>) -> Vec<(String, String)> {
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
            if let Some(map) = item.as_mapping() {
                let status = map
                    .get(&Value::String("status".to_string()))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let comment = map
                    .get(&Value::String("commit_comment".to_string()))
                    .and_then(|v| v.as_str());
                if !status_is_open(status) {
                    if let Some(c) = comment {
                        if !c.trim().is_empty() {
                            let id = map
                                .get(&Value::String("id".to_string()))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            out.push((id, c.trim().to_string()));
                        }
                    }
                }
            }
        }
    }
    out
}

pub fn drain_pending_commit_comments(cwd: Option<&str>) -> Vec<(String, String)> {
    let path = prd_path_for(cwd);
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return Vec::new();
    }
    let mut drained: Vec<(String, String)> = Vec::new();
    let cas_max_attempts = super::fsm::graph().policy.cas_max_attempts;
    let _ = cas::cas_retry_write(
        &path_s,
        cas_max_attempts,
        "prd-drain-commit-comments",
        |mut doc: Value| {
            drained.clear();
            let mut closed_rows_removed = 0usize;
            if let Some(seq) = doc.as_sequence_mut() {
                let before = seq.len();
                seq.retain(|item| {
                    let Some(map) = item.as_mapping() else {
                        return true;
                    };
                    let status = map
                        .get(&Value::String("status".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if status_is_open(status) {
                        return true;
                    }
                    let id = map
                        .get(&Value::String("id".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment = map
                        .get(&Value::String("commit_comment".to_string()))
                        .and_then(|v| v.as_str())
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty());
                    if let Some(c) = comment {
                        drained.push((id, c));
                    }
                    false
                });
                closed_rows_removed = before - seq.len();
            }
            if closed_rows_removed == 0 {
                return cas::CasOutcome::Abort(
                    String::new(),
                    "no closed PRD rows to drain".to_string(),
                    0,
                );
            }
            cas::CasOutcome::Write(doc, ())
        },
    );
    drained
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

pub fn handle_list(content: &str) -> (String, String, i32) {
    let request = request_object(content);
    let rows = match load_rows() {
        Ok(rows) => rows,
        Err((message, code)) => return (String::new(), message, code),
    };
    let total = rows.len();
    let wanted_ids = request_strings(&request, PRD_ROW_ID_FIELDS);
    let wanted_statuses = request_strings(&request, PRD_ROW_STATUS_FIELDS);
    let selected: Vec<serde_json::Value> = rows
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
                total
            ),
            1,
        );
    }
    let brief_flag = request_flag(&request, PRD_BRIEF_BODY_FIELDS);
    let full_flag = request_flag(&request, PRD_FULL_BODY_FIELDS);
    let want_full =
        full_flag.unwrap_or_else(|| brief_flag.map(|b| !b).unwrap_or(!wanted_ids.is_empty()));
    let items: Vec<serde_json::Value> = if want_full {
        selected
    } else {
        selected.iter().map(brief_row).collect()
    };
    let reply = serde_json::json!({
        "items": items,
        "count": items.len(),
        "total": total,
        "brief": !want_full,
        "full": want_full,
        "hint": "pass {\"id\":\"<row id>\"} for one full row, {\"status\":\"pending\"} to filter, or {\"full\":true} for every full body",
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
        == Some(true);

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
                    add_outcome = if *slot == new_row {
                        AddOutcome::AlreadyIdentical
                    } else if overwrite {
                        AddOutcome::Rescoped
                    } else {
                        return cas::CasOutcome::Abort(
                            String::new(),
                            format!(
                                "prd-add refused: id '{}' already exists. Pass overwrite:true to rescope it",
                                id
                            ),
                            1,
                        );
                    };
                    *slot = new_row;
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
    let policy = super::fsm::graph().policy.clone();
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
                recovered.unwrap_or(id)
            } else {
                id
            }
        } else {
            id
        };
        (id, wit, comment, witness_dispatch_id, cwd)
    } else if let Some((id, wit)) = recover_truncated_envelope(trimmed) {
        (id, wit, None, None, None)
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
        (id, wit, None, None, None)
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
    #[cfg(target_arch = "wasm32")]
    let resolver_session = serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|v| v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string()));
    let policy = super::fsm::graph().policy.clone();
    let status_kept = keeps_status(trimmed);
    let has_witness = witness
        .as_ref()
        .map(|w| !w.trim().is_empty())
        .unwrap_or(false);
    if !status_kept
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
    #[cfg(target_arch = "wasm32")]
    let mut witness_session: Option<String> = None;
    #[cfg(target_arch = "wasm32")]
    if let Some(dispatch_id) = witness_dispatch_id.as_ref() {
        let cwd = resolve_cwd.as_deref().unwrap_or("");
        let witnessed = crate::dispatch_ledger::lookup(cwd, dispatch_id).is_some_and(|entry| {
            entry.get("exit_code").and_then(|v| v.as_i64()) == Some(0)
                && entry
                    .get("verb")
                    .and_then(|v| v.as_str())
                    .is_some_and(|verb| {
                        matches!(
                            verb,
                            "exec_js" | "codesearch" | "code_search" | "search" | "grep" | "rg"
                        )
                    })
        });
        if !witnessed {
            let body = serde_json::json!({
                "error": format!("prd-resolve refused: witness_dispatch_id {} not found, exited nonzero, or not a witness verb (exec_js, codesearch, grep) in this guest's dispatch ledger", dispatch_id),
                "deviation_kind": "prd-resolve-fabricated-dispatch",
                "deviation_severity": "deny",
                "prd_id": id_target,
                "witness_dispatch_id": dispatch_id,
                "hint": "witness_dispatch_id must be the `dispatch_id` field returned in a PRIOR spool response (every verb's response now carries one). This id was not found in .gm/exec-spool/.dispatch-ledger.json with exit_code 0, so it does not correspond to a real dispatch that actually succeeded -- either it was invented, it failed, or it belongs to a different cwd/project. Resolve again either omitting witness_dispatch_id, or with the exact dispatch_id copied from the response of the dispatch that produced this row's evidence.",
            }).to_string();
            return (
                body,
                format!(
                    "prd-resolve refused: unknown witness_dispatch_id for {}",
                    id_target
                ),
                1,
            );
        }
        witness_session = crate::dispatch_ledger::lookup(cwd, dispatch_id).and_then(|entry| {
            entry
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
        if resolver_session.is_some() && witness_session == resolver_session {
            let body = serde_json::json!({
                "error": format!("prd-resolve refused: witness_dispatch_id {} was dispatched by the resolving session for {}", dispatch_id, id_target),
                "deviation_kind": "prd-resolve-self-witness",
                "deviation_severity": "deny",
                "prd_id": id_target,
                "witness_dispatch_id": dispatch_id,
                "hint": "The witness and the resolve share one session_id, so the executor verified its own work. Resolve from a session other than the one that dispatched the witness; the executor reports BLOCKED until an independent session closes the row.",
            }).to_string();
            return (
                body,
                format!(
                    "prd-resolve refused: witness dispatched by the resolving session for {}",
                    id_target
                ),
                1,
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    let witness_verified = resolver_session.is_some()
        && witness_session.is_some()
        && witness_session != resolver_session;
    #[cfg(not(target_arch = "wasm32"))]
    let witness_verified = false;
    #[cfg(not(target_arch = "wasm32"))]
    let witness_session: Option<String> = None;
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (
            String::new(),
            format!("{} does not exist", path.display()),
            1,
        );
    }

    if !status_kept
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
            if let Some(seq) = doc.as_sequence_mut() {
                for item in seq.iter_mut() {
                    if let Some(map) = item.as_mapping_mut() {
                        if map
                            .get(&Value::String("id".to_string()))
                            .and_then(|v| v.as_str())
                            == Some(&id_target)
                        {
                            if !status_kept {
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
                            if let Some(c) = commit_comment.as_ref() {
                                map.insert(
                                    Value::String("commit_comment".to_string()),
                                    Value::String(c.clone()),
                                );
                            }
                            found = true;
                        }
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
            cas::CasOutcome::Write(doc, ())
        },
    );
    match outcome {
        Ok(()) => {
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event(
                if status_kept {
                    "prd.annotated"
                } else {
                    "prd.resolved"
                },
                serde_json::json!({ "id": id_target }),
            );
            let outcome_key = if status_kept { "annotated" } else { "resolved" };
            (serde_json::json!({ outcome_key: id_target, "status_kept": status_kept, "commit_comment_attached": commit_comment.is_some(), "witness_session": witness_session, "witness_dispatch_id_verified": witness_verified }).to_string(), String::new(), 0)
        }
        Err((out, err, rc)) => (out, err, rc),
    }
}
