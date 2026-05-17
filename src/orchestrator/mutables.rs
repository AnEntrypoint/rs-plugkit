use serde_yaml::Value;
use super::gm_dir;
use super::memorize;
use crate::pkfs;

pub fn mutables_path() -> std::path::PathBuf {
    gm_dir().join("mutables.yml")
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
                        .get(&Value::String("evidence".to_string()))
                        .and_then(|v| v.as_str())
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
