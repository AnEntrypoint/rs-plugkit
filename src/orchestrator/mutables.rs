use serde_yaml::Value;
use super::gm_dir;
use super::memorize;
use super::yaml_util::yaml_to_json;
use crate::pkfs;

pub fn mutables_path() -> std::path::PathBuf {
    gm_dir().join("mutables.yml")
}

fn levenshtein(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let m = av.len();
    let n = bv.len();
    if m == 0 { return n; }
    if n == 0 { return m; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i;
        for j in 1..=n {
            let cost = if av[i - 1] == bv[j - 1] { 0 } else { 1 };
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
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
    let raw_trimmed = content.trim();
    if raw_trimmed.is_empty() {
        return (String::new(), "missing mutable id in body".to_string(), 1);
    }

    let (id_str, inline_evidence): (String, Option<String>) = match serde_json::from_str::<serde_json::Value>(raw_trimmed) {
        Ok(serde_json::Value::Object(map)) => {
            let id = map.get("mutable_id")
                .or_else(|| map.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| raw_trimmed.to_string());
            let evidence = map.get("witness_evidence")
                .or_else(|| map.get("evidence"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string());
            (id, evidence)
        }
        Ok(serde_json::Value::String(s)) => (s, None),
        _ => (raw_trimmed.to_string(), None),
    };
    let trimmed = id_str.as_str();

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
                    let evidence = row_evidence.or_else(|| inline_evidence.clone()).unwrap_or_default();
                    if evidence.trim().is_empty() {
                        let msg = format!(
                            "Refused: mutable {} cannot be witnessed without evidence. Pass {{\"mutable_id\":\"{}\",\"witness_evidence\":\"<concrete proof>\"}} in the body, or add evidence to the .gm/mutables.yml row first.",
                            trimmed, trimmed
                        );
                        return (String::new(), msg, 1);
                    }
                    if !row_had_evidence {
                        map.insert(
                            Value::String("witness_evidence".to_string()),
                            Value::String(evidence.clone()),
                        );
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
            let near: Vec<String> = candidates.iter().take(3).map(|c| c.0.clone()).collect();
            format!(" -- did you mean one of: {}", near.join(", "))
        };
        return (String::new(), format!("mutable id not found: {}{}", trimmed, hint), 1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_empty_body() {
        let (out, err, rc) = handle_resolve("");
        assert_eq!(rc, 1);
        assert!(out.is_empty());
        assert!(err.contains("missing mutable id"));
    }

    #[test]
    fn resolve_parses_witness_evidence_from_json() {
        let body = serde_json::json!({
            "mutable_id": "m1",
            "witness_evidence": "src/foo.rs:42"
        }).to_string();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["mutable_id"], "m1");
        assert_eq!(v["witness_evidence"], "src/foo.rs:42");
        let body2 = serde_json::json!({"mutable_id": "m1", "witness_evidence": "  "}).to_string();
        let v2: serde_json::Value = serde_json::from_str(&body2).unwrap();
        let trimmed = v2["witness_evidence"].as_str().unwrap().trim();
        assert!(trimmed.is_empty(), "blank evidence must be treated as missing");
    }

    #[test]
    fn levenshtein_distance_works() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
    }
}
