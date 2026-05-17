use std::fs;
use serde_yaml::Value;
use super::gm_dir;
use super::memorize;

pub fn mutables_path() -> std::path::PathBuf {
    gm_dir().join("mutables.yml")
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing mutable id in body".to_string(), 1);
    }

    let path = mutables_path();
    if !path.exists() {
        return (String::new(), format!("{} does not exist", path.display()), 1);
    }

    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return (String::new(), format!("read failed: {}", e), 1),
    };

    let mut doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };

    let mut resolved_id: Option<String> = None;
    let mut resolved_evidence: Option<String> = None;

    if let Some(seq) = doc.as_sequence_mut() {
        for item in seq.iter_mut() {
            if let Some(map) = item.as_mapping_mut() {
                let id_match = map
                    .get(&Value::String("id".to_string()))
                    .and_then(|v| v.as_str())
                    .map(|s| s == trimmed)
                    .unwrap_or(false);
                if id_match {
                    map.insert(
                        Value::String("status".to_string()),
                        Value::String("witnessed".to_string()),
                    );
                    resolved_id = Some(trimmed.to_string());
                    resolved_evidence = map
                        .get(&Value::String("evidence".to_string()))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }
    }

    if resolved_id.is_none() {
        return (String::new(), format!("mutable id not found: {}", trimmed), 1);
    }

    let new_raw = match serde_yaml::to_string(&doc) {
        Ok(s) => s,
        Err(e) => return (String::new(), format!("serialize failed: {}", e), 1),
    };
    if let Err(e) = fs::write(&path, new_raw) {
        return (String::new(), format!("write failed: {}", e), 1);
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
