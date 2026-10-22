use super::cas;
use super::gm_dir;
use super::row_store;
use super::yaml_util::{invalidate_residual_marker, levenshtein, yaml_to_json};
use crate::pkfs;
use yaml_serde::Value;
use std::collections::BTreeMap;

const RENAME_VERBS: [&str; 8] = [
    "unknown-add",
    "unknown-resolve",
    "unknown-list",
    "unknown-defer",
    "mutable-add",
    "mutable-resolve",
    "mutable-list",
    "mutable-defer",
];

const fn verb_listed(list: &[&str], verb: &str) -> bool {
    let mut i = 0;
    while i < list.len() {
        let (name, wanted) = (list[i].as_bytes(), verb.as_bytes());
        if name.len() == wanted.len() {
            let mut j = 0;
            while j < name.len() && name[j] == wanted[j] {
                j += 1;
            }
            if j == name.len() {
                return true;
            }
        }
        i += 1;
    }
    false
}

const _: () = {
    let mut i = 0;
    while i < RENAME_VERBS.len() {
        assert!(verb_listed(super::NON_DECISION_VERBS, RENAME_VERBS[i]));
        i += 1;
    }
};

pub fn mutables_path() -> std::path::PathBuf {
    gm_dir().join("mutables.yml")
}

pub fn mutables_dir() -> std::path::PathBuf {
    gm_dir().join("mutables")
}

pub fn mutables_log_path() -> std::path::PathBuf {
    gm_dir().join("mutables-log.jsonl")
}

fn row_id(item: &Value) -> Option<&str> {
    item.as_mapping()
        .and_then(|m| m.get(&Value::String("id".to_string())))
        .and_then(|v| v.as_str())
}

fn row_status<'a>(item: &'a Value, default_status: &'a str) -> &'a str {
    item.get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(default_status)
}

fn collapse_duplicate_ids_keeping_resolved_else_last(
    seq: &mut Vec<Value>,
    resolved_statuses: &[String],
    default_status: &str,
) {
    let mut slot_of_id: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut kept: Vec<Value> = Vec::with_capacity(seq.len());
    for item in seq.drain(..) {
        let Some(id) = row_id(&item).map(|s| s.to_string()) else {
            kept.push(item);
            continue;
        };
        match slot_of_id.get(&id) {
            Some(&slot) => {
                let kept_row_already_resolved = resolved_statuses
                    .iter()
                    .any(|s| s == row_status(&kept[slot], default_status));
                if !kept_row_already_resolved {
                    kept[slot] = item;
                }
            }
            None => {
                slot_of_id.insert(id, kept.len());
                kept.push(item);
            }
        }
    }
    *seq = kept;
}

fn extract_depends_on(map: &yaml_serde::Mapping) -> Vec<String> {
    map.get(&Value::String("depends_on".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn find_cycle(
    start_id: &str,
    deps: &std::collections::HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    let mut path: Vec<String> = vec![start_id.to_string()];
    let mut on_path: std::collections::HashSet<String> = std::collections::HashSet::new();
    on_path.insert(start_id.to_string());

    fn walk(
        node: &str,
        deps: &std::collections::HashMap<String, Vec<String>>,
        path: &mut Vec<String>,
        on_path: &mut std::collections::HashSet<String>,
    ) -> Option<Vec<String>> {
        let Some(children) = deps.get(node) else {
            return None;
        };
        for child in children {
            if on_path.contains(child) {
                let mut cycle = path.clone();
                cycle.push(child.clone());
                return Some(cycle);
            }
            path.push(child.clone());
            on_path.insert(child.clone());
            if let Some(c) = walk(child, deps, path, on_path) {
                return Some(c);
            }
            path.pop();
            on_path.remove(child);
        }
        None
    }

    walk(start_id, deps, &mut path, &mut on_path)
}

const DISPATCH_ENVELOPE_FIELDS: &[&str] = &[
    "SESSION_ID",
    "session_id",
    "sessionId",
    "cwd",
    "repo",
    "root",
    "projectPath",
    "git_root_override",
    "_plan",
];

const MUTABLE_OWNER_SESSION_FIELDS: &[&str] = &["session_id", "SESSION_ID", "sessionId"];

const MUTABLE_OWNER_ROW_FIELDS: &[&str] = &["row", "row_id", "prd_id", "prd_row"];

fn owner_field(map: &yaml_serde::Mapping, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        map.get(&Value::String((*field).to_string()))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn owner_collision(
    existing: &Value,
    caller_session: Option<&str>,
    caller_row: Option<&str>,
    resolved_statuses: &[String],
    default_status: &str,
) -> Option<String> {
    let existing_map = existing.as_mapping()?;
    if resolved_statuses
        .iter()
        .any(|s| s == row_status(existing, default_status))
    {
        return None;
    }
    let existing_row = owner_field(existing_map, MUTABLE_OWNER_ROW_FIELDS);
    if let (Some(theirs), Some(ours)) = (existing_row.as_deref(), caller_row) {
        if theirs != ours {
            return Some(format!("row {}", theirs));
        }
        return None;
    }
    let existing_session = owner_field(existing_map, MUTABLE_OWNER_SESSION_FIELDS);
    match (existing_session.as_deref(), caller_session) {
        (Some(theirs), Some(ours)) if theirs != ours => Some(match existing_row {
            Some(row) => format!("session {} (row {})", theirs, row),
            None => format!("session {}", theirs),
        }),
        _ => None,
    }
}

const MUTABLE_ADD_SHAPE: &str = "mutable-add needs {\"id\":\"<kebab-case-slug>\",\"value\":\"<what is unknown>\",\"status\":\"unknown\"} -- `value`/`subject`/`description`/`notes` carry the unknown, `obligation_kind`/`depends_on`/`witness_evidence` type it as an obligation.";

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (
            String::new(),
            format!("missing body: {}", MUTABLE_ADD_SHAPE),
            1,
        );
    }
    let new_item: Value = match yaml_serde::from_str::<Value>(trimmed) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .and_then(|j| yaml_serde::to_value(j).ok())
        {
            Some(v) => v,
            None => return (String::new(), "parse failed".to_string(), 1),
        },
    };
    let map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => return (String::new(), "item must be a mapping".to_string(), 1),
    };
    let id = match map
        .get(&Value::String("id".to_string()))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(id) => id.to_string(),
        None => {
            let only_envelope = map
                .keys()
                .filter_map(|k| k.as_str())
                .all(|k| DISPATCH_ENVELOPE_FIELDS.contains(&k));
            let cause = if only_envelope {
                "the body carried only dispatch envelope fields, which is what a mutable-add dispatched with no body at all arrives as"
            } else {
                "the body carried no `id`"
            };
            let err = format!(
                "mutable-add refused: {} -- nothing was written. `id` is mandatory and is never generated: a `mut-<ms>` placeholder cannot be named by mutable-resolve, mutable-defer or another row's depends_on, so it reads as real pending state and blocks every transition while staying unreachable. {}",
                cause, MUTABLE_ADD_SHAPE
            );
            return (String::new(), err, 1);
        }
    };
    let payload_keys: Vec<&str> = map
        .keys()
        .filter_map(|k| k.as_str())
        .filter(|k| *k != "id" && !DISPATCH_ENVELOPE_FIELDS.contains(k))
        .collect();
    if payload_keys.is_empty() {
        let err = format!(
            "mutable-add refused: `id` \"{}\" arrived with no payload field -- nothing was written. Once `id` and the dispatch envelope ({}) are set aside the body is empty, so the row would name no unknown. Pass at least one payload field beside `id`: `value`, `subject`, `description`, `notes`, `obligation_kind`, `depends_on`, `witness_evidence`.",
            id,
            DISPATCH_ENVELOPE_FIELDS.join(", ")
        );
        return (String::new(), err, 1);
    }
    let rescope_requested = map
        .get(&Value::String("rescope".to_string()))
        .and_then(|v| v.as_bool())
        == Some(true);
    let mut map = map;
    map.remove(&Value::String("rescope".to_string()));
    let caller_session = owner_field(&map, MUTABLE_OWNER_SESSION_FIELDS)
        .or_else(super::state::dispatch_session_id);
    if let Some(session) = caller_session.as_deref() {
        if owner_field(&map, MUTABLE_OWNER_SESSION_FIELDS).is_none() {
            map.insert(
                Value::String("session_id".to_string()),
                Value::String(session.to_string()),
            );
        }
    }
    let caller_row = owner_field(&map, MUTABLE_OWNER_ROW_FIELDS);
    let new_depends_on = extract_depends_on(&map);
    let store = row_store::Store::mutables();
    let policy = super::fsm::graph().policy.clone();

    let outcome = row_store::cas_retry_write(
        &store,
        policy.cas_max_attempts,
        "mutable-add",
        |mut doc: Value| {
            if !new_depends_on.is_empty() {
                let mut deps: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                if let Some(seq) = doc.as_sequence() {
                    for item in seq {
                        if let Some(m) = item.as_mapping() {
                            if let Some(existing_id) = m
                                .get(&Value::String("id".to_string()))
                                .and_then(|v| v.as_str())
                            {
                                deps.insert(existing_id.to_string(), extract_depends_on(m));
                            }
                        }
                    }
                }
                deps.insert(id.clone(), new_depends_on.clone());
                if let Some(cycle) = find_cycle(&id, &deps) {
                    return cas::CasOutcome::Abort(
                        String::new(),
                        format!(
                            "mutable-add rejected: depends_on introduces a cycle: {}",
                            cycle.join(" -> ")
                        ),
                        1,
                    );
                }
            }
            let mut replaced_existing_id = false;
            if let Some(seq) = doc.as_sequence_mut() {
                let mut new_with_id = map.clone();
                new_with_id.insert(Value::String("id".to_string()), Value::String(id.clone()));
                if !new_with_id.contains_key(&Value::String("status".to_string())) {
                    new_with_id.insert(
                        Value::String("status".to_string()),
                        Value::String(policy.mutables_default_status.clone()),
                    );
                }
                let new_row = Value::Mapping(new_with_id);
                collapse_duplicate_ids_keeping_resolved_else_last(
                    seq,
                    &policy.mutables_resolved_statuses,
                    &policy.mutables_default_status,
                );
                match seq.iter_mut().find(|it| row_id(it) == Some(id.as_str())) {
                    Some(slot) => {
                        if !rescope_requested {
                            if let Some(collision) = owner_collision(
                                slot,
                                caller_session.as_deref(),
                                caller_row.as_deref(),
                                &policy.mutables_resolved_statuses,
                                &policy.mutables_default_status,
                            ) {
                                return cas::CasOutcome::Abort(
                                    serde_json::json!({
                                        "error": format!("mutable-add refused: id {} collides with an open unknown owned by {} -- nothing was written", id, collision),
                                        "deviation_kind": "mutable-add-id-collision",
                                        "id": id,
                                        "owner": collision,
                                        "suggested_id": caller_row.as_deref().map(|row| format!("{}/{}", row, id)),
                                        "hint": "Unknown ids are global across lanes, so a generic id such as U1 from two rows overwrites the other lane's entry. Namespace the id by its row (<row-id>/<name>), or pass rescope:true to replace the open entry deliberately.",
                                    })
                                    .to_string(),
                                    format!(
                                        "mutable-add refused: id {} collides with an open unknown owned by {}; namespace it as <row-id>/{} or pass rescope:true",
                                        id, collision, id
                                    ),
                                    1,
                                );
                            }
                        }
                        replaced_existing_id = true;
                        let mut row = new_row;
                        if !map.contains_key(&Value::String("status".to_string())) {
                            if let (Value::Mapping(m), Some(prior)) = (&mut row, slot.get("status")) {
                                m.insert(Value::String("status".to_string()), prior.clone());
                            }
                        }
                        *slot = row;
                    }
                    None => seq.push(new_row),
                }
            } else {
                return cas::CasOutcome::Abort(
                    String::new(),
                    "the mutable store is not a sequence".to_string(),
                    1,
                );
            }
            cas::CasOutcome::Write(doc, replaced_existing_id)
        },
    );
    let replaced_existing_id = match outcome {
        Ok(v) => v,
        Err((out, err, rc)) => return (out, err, rc),
    };
    invalidate_residual_marker("mutable-add");
    #[cfg(target_arch = "wasm32")]
    crate::wasm_dispatch::emit_event(
        "mutable.added",
        serde_json::json!({ "id": id, "rescoped": replaced_existing_id }),
    );
    let mut response = serde_json::Map::new();
    response.insert(
        if replaced_existing_id {
            "rescoped"
        } else {
            "added"
        }
        .to_string(),
        serde_json::Value::String(id.clone()),
    );
    (
        serde_json::Value::Object(response).to_string(),
        String::new(),
        0,
    )
}

const MUTABLE_ROW_ID_FIELDS: &[&str] = &["id", "ids", "mutable_id", "slug", "key"];

const MUTABLE_ROW_STATUS_FIELDS: &[&str] = &["status", "statuses"];

const MUTABLE_ROW_ID_DISPLAY_FIELDS: &[&str] = &["id", "key", "slug"];

const MUTABLE_ROW_KEY_FIELDS: &[&str] = &["key", "slug"];

const MUTABLE_ROW_SUBJECT_FIELDS: &[&str] = &["subject", "title", "statement", "value", "description", "notes"];

const MUTABLE_FULL_BODY_FIELDS: &[&str] = &["full", "verbose", "bodies"];

const MUTABLE_BRIEF_BODY_FIELDS: &[&str] = &["brief", "compact"];

const MUTABLE_BRIEF_TEXT_CHARS: usize = 80;

fn json_row_ids(row: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for field in MUTABLE_ROW_ID_DISPLAY_FIELDS {
        if let Some(s) = row.get(*field).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                out.push(s.trim().to_string());
            }
        }
    }
    out
}

fn json_row_status(row: &serde_json::Value, default_status: &str) -> String {
    match row.get("status").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => default_status.to_string(),
    }
}

fn json_brief_text(row: &serde_json::Value, fields: &[&str], max: usize) -> Option<String> {
    let raw = super::prd::row_text(row, fields);
    if raw.trim().is_empty() {
        return None;
    }
    Some(super::prd::clamp_chars(
        &super::prd::one_line(&raw),
        max,
    ))
}

fn json_brief_row(row: &serde_json::Value, default_status: &str) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "id".to_string(),
        serde_json::Value::String(
            super::prd::row_text(row, MUTABLE_ROW_ID_DISPLAY_FIELDS),
        ),
    );
    out.insert(
        "status".to_string(),
        serde_json::Value::String(json_row_status(row, default_status)),
    );
    if let Some(text) = json_brief_text(row, MUTABLE_ROW_KEY_FIELDS, MUTABLE_BRIEF_TEXT_CHARS) {
        out.insert("key".to_string(), serde_json::Value::String(text));
    } else if let Some(text) =
        json_brief_text(row, MUTABLE_ROW_SUBJECT_FIELDS, MUTABLE_BRIEF_TEXT_CHARS)
    {
        out.insert("subject".to_string(), serde_json::Value::String(text));
    }
    serde_json::Value::Object(out)
}

fn load_mutable_rows() -> Result<Vec<serde_json::Value>, (String, i32)> {
    let store = row_store::Store::mutables();
    let blocks = row_store::read_blocks(&store)?.blocks;
    Ok(blocks
        .iter()
        .filter_map(|v| {
            let m = v.as_mapping()?;
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    out.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            Some(serde_json::Value::Object(out))
        })
        .collect())
}

fn fold_duplicate_ids_keeping_resolved_else_last(
    rows: Vec<serde_json::Value>,
    resolved_statuses: &[String],
    default_status: &str,
) -> Vec<serde_json::Value> {
    let mut slot_of_id: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut kept: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => {
                kept.push(row);
                continue;
            }
        };
        match slot_of_id.get(&id).copied() {
            Some(slot) => {
                let kept_is_resolved = resolved_statuses
                    .iter()
                    .any(|s| s.as_str() == json_row_status(&kept[slot], default_status));
                if !kept_is_resolved {
                    kept[slot] = row;
                }
            }
            None => {
                slot_of_id.insert(id, kept.len());
                kept.push(row);
            }
        }
    }
    kept
}

fn mutable_list_limit(request: &serde_json::Value) -> Result<Option<usize>, String> {
    match request.get("limit") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
            .map(|n| Some(usize::try_from(n).unwrap_or(usize::MAX)))
            .ok_or_else(|| "mutable-list limit must be a non-negative integer".to_string()),
    }
}

pub fn handle_list(content: &str) -> (String, String, i32) {
    let request = super::prd::request_object(content);
    let limit = match mutable_list_limit(&request) {
        Ok(l) => l,
        Err(message) => return (String::new(), message, 1),
    };
    let rows = match load_mutable_rows() {
        Ok(rows) => rows,
        Err((message, code)) => return (String::new(), message, code),
    };
    let policy = super::fsm::graph().policy.clone();
    let default_status = policy.mutables_default_status.clone();
    let resolved_statuses = policy.mutables_resolved_statuses.clone();
    let folded = fold_duplicate_ids_keeping_resolved_else_last(
        rows,
        &resolved_statuses,
        &default_status,
    );
    let store_total = folded.len();
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for row in &folded {
        *by_status
            .entry(json_row_status(row, &default_status))
            .or_insert(0) += 1;
    }
    let wanted_ids = super::prd::request_strings(&request, MUTABLE_ROW_ID_FIELDS);
    let wanted_statuses = super::prd::request_strings(&request, MUTABLE_ROW_STATUS_FIELDS);
    let mut selected: Vec<serde_json::Value> = folded
        .into_iter()
        .filter(|row| {
            let id_ok = wanted_ids.is_empty()
                || wanted_ids
                    .iter()
                    .any(|want| json_row_ids(row).iter().any(|have| have == want));
            let status_ok = wanted_statuses.is_empty()
                || wanted_statuses
                    .iter()
                    .any(|want| want == &json_row_status(row, &default_status));
            id_ok && status_ok
        })
        .collect();
    let store = row_store::Store::mutables();
    if !wanted_ids.is_empty() {
        for want in &wanted_ids {
            if selected
                .iter()
                .any(|row| json_row_ids(row).iter().any(|have| have == want))
            {
                continue;
            }
            match row_store::log_row(&store, want) {
                Ok(Some(row)) => selected.push(row),
                Ok(None) => {}
                Err(message) => return (String::new(), message, 1),
            }
        }
    } else if !wanted_statuses.is_empty() {
        if wanted_statuses
            .iter()
            .any(|want| resolved_statuses.iter().any(|c| c.eq_ignore_ascii_case(want)))
        {
            let logged = match row_store::log_rows_with_status(&store, &resolved_statuses) {
                Ok(logged) => logged,
                Err(message) => return (String::new(), message, 1),
            };
            for row in logged {
                if wanted_statuses
                    .iter()
                    .any(|want| want.eq_ignore_ascii_case(&json_row_status(&row, &default_status)))
                {
                    selected.push(row);
                }
            }
        }
    }
    if !wanted_ids.is_empty() && selected.is_empty() {
        return (
            String::new(),
            format!(
                "no mutable row with id {} of the {} rows in .gm/mutables/ or the resolved rows in .gm/mutables-log.jsonl -- pass no id for the listing",
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
    let brief_flag = super::prd::request_flag(&request, MUTABLE_BRIEF_BODY_FIELDS);
    let full_flag = super::prd::request_flag(&request, MUTABLE_FULL_BODY_FIELDS);
    let want_full = full_flag
        .unwrap_or_else(|| brief_flag.map(|b| !b).unwrap_or(!wanted_ids.is_empty()));
    let items: Vec<serde_json::Value> = if want_full {
        window
    } else {
        window
            .iter()
            .map(|row| json_brief_row(row, &default_status))
            .collect()
    };
    let reply = serde_json::json!({
        "items": items,
        "count": items.len(),
        "total": matched,
        "store_total": store_total,
        "by_status": by_status,
        "limit": limit,
        "truncated": truncated,
        "brief": !want_full,
        "full": want_full,
        "hint": "pass {\"id\":\"<mutable id>\"} for one full row (repeated rows of an id fold to the last one unless an earlier one is resolved), {\"status\":\"pending\"} to filter, {\"status\":\"pending\",\"limit\":<n>} to cap the listing (total counts every match), or {\"full\":true} for every full body. .gm/mutables/ holds OPEN work only: a resolved mutable is moved to .gm/mutables-log.jsonl, and an id or a resolved status asked for here is answered from that log.",
    });
    (reply.to_string(), String::new(), 0)
}

pub const PROVE_OBLIGATION_KINDS: &[&str] = &[
    "precondition",
    "invariant",
    "postcondition",
    "resource-bound",
    "type-shape",
];
pub const STATE_OBLIGATION_KINDS: &[&str] = &["totality", "ownership", "replay", "effect-boundary"];
pub const CONC_OBLIGATION_KINDS: &[&str] = &["happens-before", "disjointness", "contention"];
pub const SEC_OBLIGATION_KINDS: &[&str] = &[
    "secrets",
    "injection",
    "identity-authority",
    "message-timing",
];
pub const RES_OBLIGATION_KINDS: &[&str] = &[
    "exception-model",
    "partial-failure",
    "degradation",
    "crucible",
];

pub fn all_obligation_kinds() -> Vec<&'static str> {
    PROVE_OBLIGATION_KINDS
        .iter()
        .chain(STATE_OBLIGATION_KINDS.iter())
        .chain(CONC_OBLIGATION_KINDS.iter())
        .chain(SEC_OBLIGATION_KINDS.iter())
        .chain(RES_OBLIGATION_KINDS.iter())
        .copied()
        .collect()
}

pub fn all_typed() -> bool {
    obligations_ready(PROVE_OBLIGATION_KINDS).is_ok()
}

pub fn obligations_ready(kinds: &[&str]) -> Result<(), Vec<String>> {
    let pending = pending_detailed();
    let pending_ids: std::collections::HashSet<String> = pending
        .iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let unclassified_rows_belong_to_prove = kinds == PROVE_OBLIGATION_KINDS;

    let mut blockers = Vec::new();
    for item in &pending {
        let obligation_kind = item.get("obligation_kind").and_then(|v| v.as_str());
        let in_scope = match obligation_kind {
            Some(k) => kinds.contains(&k),
            None => unclassified_rows_belong_to_prove,
        };
        if !in_scope {
            continue;
        }
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("<no-id>");
        match obligation_kind {
            None => blockers.push(format!("{} has no obligation_kind", id)),
            Some(k) if !all_obligation_kinds().contains(&k) => {
                blockers.push(format!("{} has unrecognized obligation_kind '{}'", id, k));
            }
            _ => {}
        }
        let depends_on = item
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        let unresolved_deps: Vec<&str> = depends_on
            .iter()
            .filter(|d| pending_ids.contains(**d))
            .copied()
            .collect();
        if !unresolved_deps.is_empty() {
            blockers.push(format!(
                "{} blocked on unresolved depends_on: {}",
                id,
                unresolved_deps.join(", ")
            ));
        }
    }
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(blockers)
    }
}

pub fn state_obligations_ready() -> bool {
    obligations_ready(STATE_OBLIGATION_KINDS).is_ok()
}
pub fn conc_obligations_ready() -> bool {
    obligations_ready(CONC_OBLIGATION_KINDS).is_ok()
}
pub fn sec_obligations_ready() -> bool {
    obligations_ready(SEC_OBLIGATION_KINDS).is_ok()
}
pub fn res_obligations_ready() -> bool {
    obligations_ready(RES_OBLIGATION_KINDS).is_ok()
}

pub fn obligations_blocker_message(kinds: &[&str]) -> String {
    match obligations_ready(kinds) {
        Ok(()) => String::new(),
        Err(blockers) => blockers.join("; "),
    }
}

fn mutable_blocked_external(item: &yaml_serde::Value) -> bool {
    item.get("blockedBy")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .any(|x| matches!(x.as_str(), Some("external") | Some("out-of-reach")))
        })
        .unwrap_or(false)
}

pub fn pending_detailed() -> Vec<serde_json::Value> {
    let store = row_store::Store::mutables();
    let seq = match row_store::read_blocks(&store) {
        Ok(read) => read.blocks,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut slot_of_id: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let policy = super::fsm::graph().policy.clone();
    let resolved_statuses = policy.mutables_resolved_statuses;
    for item in &seq {
        let status = item
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or(&policy.mutables_default_status);
        if !resolved_statuses.iter().any(|s| s == status) && !mutable_blocked_external(item) {
            if let Some(m) = item.as_mapping() {
                let mut obj = serde_json::Map::new();
                for (k, v) in m {
                    if let Some(ks) = k.as_str() {
                        obj.insert(ks.to_string(), yaml_to_json(v));
                    }
                }
                let row = serde_json::Value::Object(obj);
                match row_id(item).map(|s| s.to_string()) {
                    Some(id) => match slot_of_id.get(&id) {
                        Some(&slot) => out[slot] = row,
                        None => {
                            slot_of_id.insert(id, out.len());
                            out.push(row);
                        }
                    },
                    None => out.push(row),
                }
            }
        }
    }
    out
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let raw_trimmed = content.trim();
    if raw_trimmed.is_empty() {
        return (String::new(), "missing mutable id in body".to_string(), 1);
    }

    let (id_str, inline_evidence, measured_value): (String, Option<String>, Option<f64>) =
        match serde_json::from_str::<serde_json::Value>(raw_trimmed) {
            Ok(serde_json::Value::Object(map)) => {
                let id = map
                    .get("mutable_id")
                    .or_else(|| map.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| raw_trimmed.to_string());
                let evidence = map
                    .get("witness_evidence")
                    .or_else(|| map.get("evidence"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string());
                let measured = map.get("measured_value").and_then(|v| v.as_f64());
                (id, evidence, measured)
            }
            Ok(serde_json::Value::String(s)) => (s, None, None),
            _ => (raw_trimmed.to_string(), None, None),
        };
    let trimmed = id_str.as_str();

    let store = row_store::Store::mutables();
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
        "mutable-resolve",
        |mut doc: Value| {
            let mut found_id = false;
            let mut resolved_id: Option<String> = None;
            let mut resolved_evidence: Option<String> = None;

            let resolved_statuses = policy.mutables_resolved_statuses.clone();
            let mut unresolved_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            if let Some(seq) = doc.as_sequence() {
                for item in seq {
                    if let Some(m) = item.as_mapping() {
                        let status = m
                            .get(&Value::String("status".to_string()))
                            .and_then(|v| v.as_str())
                            .unwrap_or(&policy.mutables_default_status);
                        if !resolved_statuses.iter().any(|s| s == status) {
                            if let Some(iid) = m
                                .get(&Value::String("id".to_string()))
                                .and_then(|v| v.as_str())
                            {
                                unresolved_ids.insert(iid.to_string());
                            }
                        }
                    }
                }
            }

            if let Some(seq) = doc.as_sequence_mut() {
                for item in seq.iter_mut() {
                    if let Some(map) = item.as_mapping_mut() {
                        let id_match = map
                            .get(&Value::String("id".to_string()))
                            .and_then(|v| v.as_str())
                            .map(|s| s == trimmed)
                            .unwrap_or(false);
                        if id_match {
                            let depends_on = extract_depends_on(map);
                            let blockers: Vec<&String> = depends_on
                                .iter()
                                .filter(|d| unresolved_ids.contains(*d) && d.as_str() != trimmed)
                                .collect();
                            if !blockers.is_empty() {
                                let names: Vec<String> = blockers.into_iter().cloned().collect();
                                return cas::CasOutcome::Abort(
                                String::new(),
                                format!(
                                    "mutable-resolve refused: {} depends_on unresolved id(s): {} -- resolve those first.",
                                    trimmed, names.join(", ")
                                ),
                                1,
                            );
                            }
                            let row_kind = map
                                .get(&Value::String("obligation_kind".to_string()))
                                .and_then(|v| v.as_str());
                            let row_bound = map
                                .get(&Value::String("bound".to_string()))
                                .and_then(|v| v.as_f64());
                            if row_kind == Some("resource-bound") {
                                if let (Some(bound), Some(measured)) = (row_bound, measured_value) {
                                    if measured > bound {
                                        return cas::CasOutcome::Abort(
                                        String::new(),
                                        format!(
                                            "mutable-resolve refused: {} is a resource-bound obligation with bound={}, but measured_value={} exceeds it -- the claim does not hold, resolve with a corrected implementation or a corrected bound.",
                                            trimmed, bound, measured
                                        ),
                                        1,
                                    );
                                    }
                                }
                            }
                            found_id = true;
                            let row_evidence: Option<String> = map
                                .get(&Value::String("witness_evidence".to_string()))
                                .and_then(|v| v.as_str())
                                .or_else(|| {
                                    map.get(&Value::String("evidence".to_string()))
                                        .and_then(|v| v.as_str())
                                })
                                .map(|s| s.to_string())
                                .filter(|s| !s.trim().is_empty());
                            let row_had_evidence = row_evidence.is_some();
                            let evidence = row_evidence
                                .or_else(|| inline_evidence.clone())
                                .unwrap_or_default();
                            if policy.mutables_require_witness_evidence
                                && evidence.trim().is_empty()
                            {
                                let msg = format!(
                                "Refused: mutable {} cannot be witnessed without evidence. Pass {{\"mutable_id\":\"{}\",\"witness_evidence\":\"<concrete proof>\"}} in the body, or add evidence to the .gm/mutables.yml row first.",
                                trimmed, trimmed
                            );
                                return cas::CasOutcome::Abort(String::new(), msg, 1);
                            }
                            if !row_had_evidence && !evidence.trim().is_empty() {
                                map.insert(
                                    Value::String("witness_evidence".to_string()),
                                    Value::String(evidence.clone()),
                                );
                            }
                            map.insert(
                                Value::String("status".to_string()),
                                Value::String(policy.mutables_witness_status.clone()),
                            );
                            resolved_id = Some(trimmed.to_string());
                            resolved_evidence = Some(evidence);
                        }
                    }
                }
            }

            if !found_id {
                let mut candidates: Vec<(String, usize)> = Vec::new();
                if let Some(seq) = doc.as_sequence() {
                    for item in seq.iter() {
                        if let Some(id) = item
                            .as_mapping()
                            .and_then(|m| m.get(&Value::String("id".to_string())))
                            .and_then(|v| v.as_str())
                        {
                            let d = levenshtein(trimmed, id);
                            candidates.push((id.to_string(), d));
                        }
                    }
                }
                candidates.sort_by_key(|c| c.1);
                let hint = if candidates.is_empty() {
                    String::from(" (no mutables in file)")
                } else {
                    let near: Vec<String> =
                        candidates.iter().take(3).map(|c| c.0.clone()).collect();
                    format!(" -- did you mean one of: {}", near.join(", "))
                };
                return cas::CasOutcome::Abort(
                    String::new(),
                    format!("mutable id not found: {}{}", trimmed, hint),
                    1,
                );
            }

            cas::CasOutcome::Write(doc, (resolved_id, resolved_evidence))
        },
    );
    let (resolved_id, resolved_evidence) = match outcome {
        Ok(v) => v,
        Err((out, err, rc)) => return (out, err, rc),
    };
    let filed_id = resolved_id.clone().unwrap_or_else(|| trimmed.to_string());
    let filed = match row_store::retire_to_log(
        &store,
        &filed_id,
        &policy.mutables_witness_status,
        resolved_evidence.as_deref(),
    ) {
        Ok(entry) => entry,
        Err(reason) => {
            return (
                String::new(),
                format!(
                    "mutable-resolve: {} was set to {} but could not be moved into {}: {}",
                    filed_id, policy.mutables_witness_status, store.log, reason
                ),
                1,
            )
        }
    };

    #[cfg(target_arch = "wasm32")]
    crate::wasm_dispatch::emit_event("mutable.resolved", serde_json::json!({ "id": resolved_id }));
    let payload = serde_json::json!({
        "resolved": resolved_id,
        "filed_to": ".gm/mutables-log.jsonl",
        "filed_at": filed.get("ts").cloned().unwrap_or(serde_json::Value::Null),
        "row_file_cleared": true,
    });
    (payload.to_string(), String::new(), 0)
}

pub fn handle_defer(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing body: {\"id\": \"<mutable-id>\", \"reason\": \"<why this is genuinely out of reach this session, and what session/path would resolve it>\"}".to_string(), 1);
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
        .or_else(|| v.get("mutable_id"))
        .or_else(|| v.get("slug"))
        .and_then(|s| s.as_str())
    {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (String::new(), "missing `id`".to_string(), 1),
    };
    let reason = match v.get("reason").or_else(|| v.get("witness_evidence")).and_then(|s| s.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (String::new(), "missing `reason`: name why this row is genuinely out of reach this session and what would resolve it -- bare deferral language is rejected, same as mutable-add/prd-defer".to_string(), 1),
    };
    if let Some(marker) = super::yaml_util::defer_marker_in_text(&reason) {
        let err = format!(
            "mutable-defer refused: deferral language detected ('{}'). Same rule as prd-defer's own gate -- name the concrete reason this is genuinely cross-session/out-of-reach (e.g. 'flaky multiplayer repro needs its own dedicated debugging session, unrelated to this session's rendering-pipeline fix'), not bare 'later'/'next session' phrasing with no substance.",
            marker
        );
        return (String::new(), err, 1);
    }
    let store = row_store::Store::mutables();
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
        "mutable-defer",
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
                    "error": format!("mutable id not found: {}", id_target),
                    "deviation_kind": "mutable-defer-unknown-id",
                    "deviation_severity": "deny",
                    "mutable_id": id_target,
                })
                .to_string();
                return cas::CasOutcome::Abort(
                    body,
                    format!("mutable id not found: {}", id_target),
                    1,
                );
            }
            cas::CasOutcome::Write(doc, ())
        },
    );
    match outcome {
        Ok(()) => {
            invalidate_residual_marker("mutable-defer");
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event(
                "mutable.deferred",
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
