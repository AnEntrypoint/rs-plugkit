pub mod entry;
pub mod plan;
pub mod execute;
pub mod emit;
pub mod verify;
pub mod update_docs;

use serde_json::json;
use super::gm_dir;
use super::state::read_state;
use super::mutables::mutables_path;
use crate::pkfs;

pub fn get_instruction(phase: &str) -> &'static str {
    match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => entry::TEXT,
        "PLAN" => plan::TEXT,
        "EXECUTE" => execute::TEXT,
        "EMIT" => emit::TEXT,
        "VERIFY" => verify::TEXT,
        "UPDATE-DOCS" | "UPDATE_DOCS" | "UPDATEDOCS" | "COMPLETE" => update_docs::TEXT,
        _ => entry::TEXT,
    }
}

fn next_phase_hint(phase: &str) -> Option<&'static str> {
    match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => Some("PLAN"),
        "PLAN" => Some("EXECUTE"),
        "EXECUTE" => Some("EMIT"),
        "EMIT" => Some("VERIFY"),
        "VERIFY" => Some("UPDATE-DOCS"),
        "UPDATE-DOCS" | "UPDATE_DOCS" => Some("COMPLETE"),
        _ => None,
    }
}

fn pending_mutables() -> Vec<String> {
    let path = mutables_path();
    let path_s = path.to_string_lossy().to_string();
    let mut ids = Vec::new();
    if !pkfs::exists(&path_s) {
        return ids;
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return ids,
    };
    let doc: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return ids,
    };
    if let Some(seq) = doc.as_sequence() {
        for item in seq {
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if status != "witnessed" && status != "resolved" {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

fn pending_prd_count() -> usize {
    let path = gm_dir().join("prd.yml");
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return 0;
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return 0,
    };
    let doc: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let seq_opt = doc.as_sequence().cloned().or_else(|| {
        doc.get("items").and_then(|v| v.as_sequence()).cloned()
    });
    let mut n = 0usize;
    if let Some(items) = seq_opt {
        for item in items {
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            if status != "done" && status != "complete" && status != "completed" {
                n += 1;
            }
        }
    }
    n
}

pub fn handle_instruction(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let phase = if trimmed.is_empty() {
        read_state().phase
    } else if let Some(stripped) = trimmed.strip_prefix("phase=") {
        stripped.trim().to_string()
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        v.get("phase")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| read_state().phase)
    } else {
        trimmed.to_string()
    };

    let instruction = get_instruction(&phase);
    let mutables_pending = pending_mutables();
    let prd_pending_count = pending_prd_count();
    let next = next_phase_hint(&phase);

    let payload = json!({
        "phase": phase,
        "instruction": instruction,
        "mutables_pending": mutables_pending,
        "prd_pending_count": prd_pending_count,
        "next_phase_hint": next,
    });
    (payload.to_string(), String::new(), 0)
}
