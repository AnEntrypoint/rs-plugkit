use crate::pkfs;
use serde_yaml::Value;
use super::prd::prd_path;
use super::yaml_util::{repair_text, serialize_checked, YamlFault};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn prune_older_backups(path_s: &str, keep_path: &str) -> usize {
    let (dir, base) = match path_s.rfind('/') {
        Some(i) => (&path_s[..i], &path_s[i + 1..]),
        None => (".", path_s),
    };
    let keep_name = keep_path.rsplit('/').next().unwrap_or(keep_path);
    let prefix = format!("{base}.bak-");
    let Some(serde_json::Value::Array(entries)) = pkfs::readdir(dir) else {
        return 0;
    };
    let mut pruned = 0;
    for entry in &entries {
        let Some(name) = entry
            .as_str()
            .or_else(|| entry.get("name").and_then(serde_json::Value::as_str))
        else {
            continue;
        };
        let suffix = match name.strip_prefix(prefix.as_str()) {
            Some(s) => s,
            None => continue,
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) || name == keep_name {
            continue;
        }
        if pkfs::remove_file(&format!("{dir}/{name}")) {
            pruned += 1;
        }
    }
    pruned
}

fn fault_json(faults: &[YamlFault]) -> serde_json::Value {
    serde_json::Value::Array(
        faults
            .iter()
            .map(|f| {
                serde_json::json!({
                    "line": f.line,
                    "column": f.column,
                    "substring": f.substring,
                    "token": f.token,
                    "describe": f.describe(),
                })
            })
            .collect(),
    )
}

pub fn handle_repair(content: &str) -> (String, String, i32) {
    let body: Value = match serde_json::from_str::<Value>(content.trim()) {
        Ok(v) => v,
        Err(_) => return (String::new(), "prd-repair body must be JSON".to_string(), 1),
    };
    let path_s = match body.get("path").and_then(Value::as_str) {
        Some(p) => p.to_string(),
        None => prd_path().to_string_lossy().to_string(),
    };
    let dry_run = body
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_passes = body
        .get("max_passes")
        .and_then(Value::as_u64)
        .unwrap_or(16) as usize;

    let Some(before) = pkfs::read_to_string(&path_s) else {
        return (
            String::new(),
            format!("prd-repair: {path_s} does not exist or could not be read"),
            1,
        );
    };
    if before.trim().is_empty() {
        return (serde_json::json!({"ok": true, "path": path_s, "already_parses": true, "rows": 0, "faults": []}).to_string(), String::new(), 0);
    }
    if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&before) {
        let rows = doc.as_sequence().map(|s| s.len()).unwrap_or(0);
        return (
            serde_json::json!({"ok": true, "path": path_s, "already_parses": true, "rows": rows, "faults": []}).to_string(),
            String::new(),
            0,
        );
    }

    if dry_run {
        let plan = match repair_text(&before, max_passes) {
            Ok(plan) => plan,
            Err((why, _faults)) => {
                return (
                    String::new(),
                    format!("prd-repair refused: {why} (file left untouched: {path_s})"),
                    1,
                );
            }
        };
        let guarded_len = serde_yaml::from_str::<serde_yaml::Value>(&plan.repaired)
            .ok()
            .and_then(|d| serialize_checked(&d).ok())
            .map(|s| s.len())
            .unwrap_or(plan.repaired.len());
        return (
            serde_json::json!({
                "ok": true,
                "path": path_s,
                "dry_run": true,
                "would_change": plan.changed,
                "rows_after": plan.rows_after,
                "bytes_before": before.len(),
                "bytes_after": guarded_len,
                "faults": fault_json(&plan.faults),
            })
            .to_string(),
            String::new(),
            0,
        );
    }
    match repair_prd_store(&path_s, max_passes) {
        Ok(json) => (json.to_string(), String::new(), 0),
        Err((out, err, rc)) => (out, err, rc),
    }
}

pub fn repair_prd_store(
    path_s: &str,
    max_passes: usize,
) -> Result<serde_json::Value, (String, String, i32)> {
    let Some(before) = pkfs::read_to_string(path_s) else {
        return Err((
            String::new(),
            format!("prd-repair: {path_s} does not exist or could not be read"),
            1,
        ));
    };
    if before.trim().is_empty() {
        return Ok(serde_json::json!({"ok": true, "path": path_s, "already_parses": true, "rows": 0, "faults": []}));
    }
    if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&before) {
        let rows = doc.as_sequence().map(|s| s.len()).unwrap_or(0);
        return Ok(serde_json::json!({"ok": true, "path": path_s, "already_parses": true, "rows": rows, "faults": []}));
    }
    let plan = match repair_text(&before, max_passes) {
        Ok(plan) => plan,
        Err((why, _faults)) => {
            return Err((
                String::new(),
                format!("prd-repair refused: {why} (file left untouched: {path_s})"),
                1,
            ));
        }
    };
    let repaired_doc: serde_yaml::Value = serde_yaml::from_str(&plan.repaired).map_err(|e| {
        (
            String::new(),
            format!("prd-repair refused: the repaired text still does not parse ({e}); file left untouched: {path_s}"),
            1,
        )
    })?;
    let guarded = serialize_checked(&repaired_doc).map_err(|e| {
        (
            String::new(),
            format!("prd-repair refused: the repaired document is not read-back safe ({e}); file left untouched: {path_s}"),
            1,
        )
    })?;
    let faults = fault_json(&plan.faults);
    if !plan.changed {
        return Ok(serde_json::json!({
            "ok": true, "path": path_s, "already_parses": true,
            "rows": plan.rows_after, "faults": faults,
        }));
    }
    let backup_path = format!("{path_s}.bak-{}", now_ms());
    if !pkfs::write(&backup_path, &before) {
        return Err((
            String::new(),
            format!("prd-repair refused: could not back up {path_s} to {backup_path}; nothing was written"),
            1,
        ));
    }
    let mut last_err: Option<String> = None;
    for _ in 0..super::fsm::graph().policy.cas_max_attempts.max(1) {
        match pkfs::cas_write(path_s, &before, &guarded) {
            pkfs::CasWriteOutcome::Swapped => {
                last_err = None;
                break;
            }
            pkfs::CasWriteOutcome::Mismatch => {
                let current = pkfs::read_to_string(path_s).unwrap_or_default();
                if current == guarded {
                    last_err = None;
                    break;
                }
                last_err = Some(format!(
                    "prd-repair: concurrent writer changed {path_s} between the backup and the write; its bytes are at {backup_path}"
                ));
            }
            pkfs::CasWriteOutcome::IoError => {
                last_err = Some(format!("prd-repair: write to {path_s} failed"));
            }
        }
    }
    if let Some(err) = last_err {
        return Err((String::new(), err, 1));
    }
    let pruned_backups = prune_older_backups(path_s, &backup_path);
    Ok(serde_json::json!({
        "ok": true,
        "path": path_s,
        "backup": backup_path,
        "pruned_backups": pruned_backups,
        "repaired": true,
        "rows_after": plan.rows_after,
        "bytes_before": before.len(),
        "bytes_after": guarded.len(),
        "faults": faults,
    }))
}
