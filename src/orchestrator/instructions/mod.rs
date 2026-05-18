pub mod entry;
pub mod plan;
pub mod execute;
pub mod emit;
pub mod verify;
pub mod update_docs;

use serde_json::json;
use super::state::read_state;
use super::mutables;
use super::prd;
use super::recall;
use crate::pkfs;

fn read_update_available() -> serde_json::Value {
    let path = super::gm_dir().join("exec-spool").join(".update-available.json");
    let ps = path.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return serde_json::Value::Null;
    }
    match pkfs::read_to_string(&ps) {
        Some(content) => serde_json::from_str::<serde_json::Value>(&content).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }
}

pub fn get_instruction(phase: &str) -> &'static str {
    match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => entry::TEXT,
        "PLAN" => plan::TEXT,
        "EXECUTE" => execute::TEXT,
        "EMIT" => emit::TEXT,
        "VERIFY" => verify::TEXT,
        "COMPLETE" => update_docs::TEXT,
        _ => entry::TEXT,
    }
}

fn next_phase_hint(phase: &str) -> Option<&'static str> {
    match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => Some("PLAN"),
        "PLAN" => Some("EXECUTE"),
        "EXECUTE" => Some("EMIT"),
        "EMIT" => Some("VERIFY"),
        "VERIFY" => Some("COMPLETE"),
        "COMPLETE" => None,
        _ => None,
    }
}

fn prd_items_json() -> Vec<serde_json::Value> {
    let (body, _err, code) = prd::handle_list("");
    if code != 0 { return Vec::new(); }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("items").cloned())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn prd_pending_count(items: &[serde_json::Value]) -> usize {
    items.iter().filter(|it| {
        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        status != "done" && status != "complete" && status != "completed"
    }).count()
}

pub fn handle_instruction(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id_opt: Option<String> = None;
    let phase = if trimmed.is_empty() {
        read_state().phase
    } else if let Some(stripped) = trimmed.strip_prefix("phase=") {
        stripped.trim().to_string()
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
            session_id_opt = Some(sid.to_string());
        }
        if let Some(s) = v.as_str() {
            s.to_string()
        } else if let Some(s) = v.get("phase").and_then(|p| p.as_str()) {
            s.to_string()
        } else {
            read_state().phase
        }
    } else {
        trimmed.to_string()
    };

    if let Some(sid) = session_id_opt {
        let mut st = read_state();
        st.session_id = Some(sid);
        let _ = super::state::write_state(&st);
    }

    let instruction = get_instruction(&phase);
    let mutables_pending = mutables::pending_detailed();
    let prd_items = prd_items_json();
    let prd_pending = prd_pending_count(&prd_items);
    let next = next_phase_hint(&phase);

    let query = prd_items.iter()
        .find(|it| {
            let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            status != "done" && status != "complete" && status != "completed"
        })
        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let recall_hits = if query.is_empty() {
        serde_json::Value::Array(Vec::new())
    } else {
        recall::recall_hits(&query, 3)
    };

    let update_available = read_update_available();

    let payload = json!({
        "phase": phase,
        "instruction": instruction,
        "mutables_pending": mutables_pending,
        "prd_items": prd_items,
        "prd_pending_count": prd_pending,
        "next_phase_hint": next,
        "recall_hits": recall_hits,
        "update_available": update_available,
    });
    (payload.to_string(), String::new(), 0)
}
