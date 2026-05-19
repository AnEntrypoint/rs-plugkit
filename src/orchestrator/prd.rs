use serde_yaml::Value;
use super::gm_dir;
use super::yaml_util::yaml_to_json;
use crate::pkfs;

pub fn prd_path() -> std::path::PathBuf {
    gm_dir().join("prd.yml")
}

pub fn handle_list(_content: &str) -> (String, String, i32) {
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (serde_json::json!({ "items": [], "count": 0 }).to_string(), String::new(), 0);
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return (String::new(), "read failed".to_string(), 1),
    };
    let doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };
    let seq = doc.as_sequence().cloned()
        .or_else(|| doc.get("items").and_then(|v| v.as_sequence()).cloned())
        .unwrap_or_default();
    let items: Vec<serde_json::Value> = seq.iter().filter_map(|item| {
        let m = item.as_mapping()?;
        let mut out = serde_json::Map::new();
        for (k, v) in m {
            if let Some(ks) = k.as_str() {
                out.insert(ks.to_string(), yaml_to_json(v));
            }
        }
        Some(serde_json::Value::Object(out))
    }).collect();
    let count = items.len();
    (serde_json::json!({ "items": items, "count": count }).to_string(), String::new(), 0)
}

fn defer_marker_in_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    const MARKERS: &[&str] = &[
        "next pass", "next session", "next turn",
        "defer to later", "deferred to later", "deferred for later",
        "future pass", "future session", "future turn", "future work",
        "address it next", "address this next", "leave for next",
        "documented for next", "documented for future",
        "below criticality", "skip for now", "punt for now",
        "do later", "fix later", "later pass",
    ];
    for m in MARKERS {
        if lower.contains(m) { return Some(m); }
    }
    None
}

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing body: provide PRD item as JSON or YAML".to_string(), 1);
    }
    let new_item: Value = match serde_yaml::from_str::<Value>(trimmed) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .and_then(|j| serde_yaml::to_value(j).ok()) {
            Some(v) => v,
            None => return (String::new(), "parse failed".to_string(), 1),
        },
    };
    let item_map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => return (String::new(), "item must be a mapping with id/subject/status".to_string(), 1),
    };
    let has_external_block = item_map.get(&Value::String("blockedBy".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().any(|x| matches!(x.as_str(), Some("external") | Some("out-of-reach"))))
        .unwrap_or(false);
    if !has_external_block {
        let mut scan_buf = String::new();
        for field in &["description", "subject", "notes"] {
            if let Some(s) = item_map.get(&Value::String(field.to_string())).and_then(|v| v.as_str()) {
                scan_buf.push_str(s);
                scan_buf.push('\n');
            }
        }
        if let Some(marker) = defer_marker_in_text(&scan_buf) {
            let err = format!(
                "PRD item rejected: deferral language detected ('{}'). Per §22 Fix on Sight and §17 Maximal Cover, in-spirit reachable work must be executed this turn, not deferred. Either: (a) drop the deferral phrasing and commit to executing this turn, or (b) add `blockedBy: [external]` (or `[out-of-reach]`) to declare the item genuinely unreachable from this session.",
                marker
            );
            return (String::new(), err, 1);
        }
    }
    let id = item_map.get(&Value::String("id".to_string()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("item-{}", crate::orchestrator::state::now_ms()));
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    let mut doc: Value = if pkfs::exists(&path_s) {
        let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
        serde_yaml::from_str(&raw).unwrap_or(Value::Sequence(vec![]))
    } else {
        Value::Sequence(vec![])
    };
    if let Some(seq) = doc.as_sequence_mut() {
        let mut new_with_id = item_map.clone();
        new_with_id.insert(Value::String("id".to_string()), Value::String(id.clone()));
        if !new_with_id.contains_key(&Value::String("status".to_string())) {
            new_with_id.insert(Value::String("status".to_string()), Value::String("pending".to_string()));
        }
        seq.push(Value::Mapping(new_with_id));
    } else {
        return (String::new(), "prd.yml is not a sequence".to_string(), 1);
    }
    let new_raw = serde_yaml::to_string(&doc).unwrap_or_default();
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }
    (serde_json::json!({ "added": id }).to_string(), String::new(), 0)
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing PRD item id".to_string(), 1);
    }
    let id_target = if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        v.get("id").and_then(|s| s.as_str()).map(|s| s.to_string()).unwrap_or_else(|| trimmed.to_string())
    } else {
        trimmed.to_string()
    };
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (String::new(), format!("{} does not exist", path.display()), 1);
    }
    let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
    let mut doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };
    let mut found = false;
    if let Some(seq) = doc.as_sequence_mut() {
        for item in seq.iter_mut() {
            if let Some(map) = item.as_mapping_mut() {
                if map.get(&Value::String("id".to_string())).and_then(|v| v.as_str()) == Some(&id_target) {
                    map.insert(Value::String("status".to_string()), Value::String("completed".to_string()));
                    found = true;
                }
            }
        }
    }
    if !found {
        return (String::new(), format!("prd id not found: {}", id_target), 1);
    }
    let new_raw = serde_yaml::to_string(&doc).unwrap_or_default();
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }
    (serde_json::json!({ "resolved": id_target }).to_string(), String::new(), 0)
}
