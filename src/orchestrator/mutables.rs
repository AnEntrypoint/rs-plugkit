use serde_yaml::Value;
use super::gm_dir;
use super::memorize;
use super::yaml_util::yaml_to_json;
use crate::pkfs;

pub fn mutables_path() -> std::path::PathBuf {
    gm_dir().join("mutables.yml")
}

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing body".to_string(), 1);
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
    let map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => return (String::new(), "item must be a mapping".to_string(), 1),
    };
    let id = map.get(&Value::String("id".to_string()))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("mut-{}", crate::orchestrator::state::now_ms()));
    let path = mutables_path();
    let path_s = path.to_string_lossy().to_string();
    let mut doc: Value = if pkfs::exists(&path_s) {
        let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
        serde_yaml::from_str(&raw).unwrap_or(Value::Sequence(vec![]))
    } else {
        Value::Sequence(vec![])
    };
    if let Some(seq) = doc.as_sequence_mut() {
        let mut new_with_id = map.clone();
        new_with_id.insert(Value::String("id".to_string()), Value::String(id.clone()));
        if !new_with_id.contains_key(&Value::String("status".to_string())) {
            new_with_id.insert(Value::String("status".to_string()), Value::String("unknown".to_string()));
        }
        seq.push(Value::Mapping(new_with_id));
    } else {
        return (String::new(), "mutables.yml is not a sequence".to_string(), 1);
    }
    let new_raw = serde_yaml::to_string(&doc).unwrap_or_default();
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }
    (serde_json::json!({ "added": id }).to_string(), String::new(), 0)
}

pub fn handle_list(_content: &str) -> (String, String, i32) {
    let path = mutables_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (serde_json::json!({ "items": [] }).to_string(), String::new(), 0);
    }
    let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
    let doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };
    let items: Vec<serde_json::Value> = doc.as_sequence().map(|seq| {
        seq.iter().filter_map(|v| {
            let m = v.as_mapping()?;
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    out.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            Some(serde_json::Value::Object(out))
        }).collect()
    }).unwrap_or_default();
    (serde_json::json!({ "items": items }).to_string(), String::new(), 0)
}

pub fn pending_detailed() -> Vec<serde_json::Value> {
    let path = mutables_path();
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
            let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
            if status != "witnessed" && status != "resolved" {
                if let Some(m) = item.as_mapping() {
                    let mut obj = serde_json::Map::new();
                    for (k, v) in m {
                        if let Some(ks) = k.as_str() {
                            obj.insert(ks.to_string(), yaml_to_json(v));
                        }
                    }
                    out.push(serde_json::Value::Object(obj));
                }
            }
        }
    }
    out
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing mutable id in body".to_string(), 1);
    }

    let path = mutables_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (String::new(), format!("{} does not exist", path.display()), 1);
    }

    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return (String::new(), "read failed".to_string(), 1),
    };

    let mut doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };

    let mut resolved_id: Option<String> = None;
    let mut resolved_evidence: Option<String> = None;
    let mut found_id = false;

    if let Some(seq) = doc.as_sequence_mut() {
        for item in seq.iter_mut() {
            if let Some(map) = item.as_mapping_mut() {
                let id_match = map
                    .get(&Value::String("id".to_string()))
                    .and_then(|v| v.as_str())
                    .map(|s| s == trimmed)
                    .unwrap_or(false);
                if id_match {
                    found_id = true;
                    let evidence = map
                        .get(&Value::String("witness_evidence".to_string()))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            map.get(&Value::String("evidence".to_string()))
                                .and_then(|v| v.as_str())
                        })
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if evidence.trim().is_empty() {
                        let msg = format!(
                            "Refused: mutable {} cannot be witnessed without evidence. Add evidence: \"<concrete proof: file:line, codesearch hit, exec output>\" to .gm/mutables.yml row first.",
                            trimmed
                        );
                        return (String::new(), msg, 1);
                    }
                    map.insert(
                        Value::String("status".to_string()),
                        Value::String("witnessed".to_string()),
                    );
                    resolved_id = Some(trimmed.to_string());
                    resolved_evidence = Some(evidence);
                }
            }
        }
    }

    if !found_id {
        return (String::new(), format!("mutable id not found: {}", trimmed), 1);
    }

    let new_raw = match serde_yaml::to_string(&doc) {
        Ok(s) => s,
        Err(e) => return (String::new(), format!("serialize failed: {}", e), 1),
    };
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }

    let evidence_body = resolved_evidence.unwrap_or_else(|| format!("mutable {} resolved", trimmed));
    let memo = format!(
        "## Resolved mutable: {}\n\n{}\n",
        resolved_id.as_deref().unwrap_or(""),
        evidence_body
    );
    let memo_path = memorize::fire(&memo).unwrap_or_default();

    let payload = serde_json::json!({
        "resolved": resolved_id,
        "memorize_spool": memo_path,
    });
    (payload.to_string(), String::new(), 0)
}
