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

fn read_unsupervised_watcher() -> serde_json::Value {
    let path = super::gm_dir().join("exec-spool").join(".pre-supervised-watcher.json");
    let ps = path.to_string_lossy().to_string();
    if !pkfs::exists(&ps) {
        return serde_json::Value::Null;
    }
    match pkfs::read_to_string(&ps) {
        Some(content) => serde_json::from_str::<serde_json::Value>(&content).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }
}

fn residual_check_fired_recently() -> bool {
    let marker = super::gm_dir().join("residual-check-fired");
    let ms = marker.to_string_lossy().to_string();
    pkfs::exists(&ms)
}

fn should_residual_scan(prd_pending: usize, running_tasks_count: usize) -> bool {
    if residual_check_fired_recently() { return false; }
    prd_pending == 0 && running_tasks_count == 0
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

fn item_is_open(it: &serde_json::Value) -> bool {
    let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
    status != "done" && status != "complete" && status != "completed"
}

fn ready_wave(items: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let completed_ids: std::collections::HashSet<String> = items.iter()
        .filter(|it| !item_is_open(it))
        .filter_map(|it| it.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    items.iter()
        .filter(|it| item_is_open(it))
        .filter(|it| {
            it.get("blockedBy")
                .or_else(|| it.get("dependencies"))
                .and_then(|v| v.as_array())
                .map(|deps| deps.iter().all(|d| {
                    d.as_str()
                        .map(|s| s == "external" || completed_ids.contains(s))
                        .unwrap_or(false)
                }))
                .unwrap_or(true)
        })
        .take(3)
        .cloned()
        .collect()
}

fn orient_nouns(prompt: &str) -> Vec<String> {
    let stop: &[&str] = &[
        "the","a","an","to","of","in","on","for","and","or","is","are","was","were",
        "be","been","being","do","does","did","have","has","had","i","you","we","they",
        "it","this","that","these","those","with","from","as","at","by","but","if",
        "then","so","can","could","would","should","will","shall","may","might",
        "please","me","my","our","your","their","his","her",
    ];
    let mut words: Vec<String> = prompt
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() > 2)
        .filter(|w| {
            let lower = w.to_lowercase();
            !stop.contains(&lower.as_str())
        })
        .map(|s| s.to_string())
        .collect();
    words.sort();
    words.dedup();
    words.truncate(5);
    words
}

fn read_last_prompt() -> String {
    let p = super::gm_dir().join("last-prompt.txt");
    let ps = p.to_string_lossy().to_string();
    pkfs::read_to_string(&ps).unwrap_or_default()
}

fn pending_step_block(st: &super::state::TurnState) -> Option<serde_json::Value> {
    let step_id = st.pending_step_id.as_ref()?;
    let deadline = st.pending_step_deadline_ms?;
    let now = super::state::now_ms();
    if now > deadline {
        return None;
    }
    Some(json!({
        "pending_step_id": step_id,
        "pending_step_deadline_ms": deadline,
        "required_next_verb": "memorize-continue",
        "required_next_body_shape": {
            "token": "<the token from the prior pending_step response>",
            "step_id": step_id,
            "result": "<an object obeying the prior pending_step.result_schema>"
        },
        "imperative": "Pipeline suspended at step_id. Compute the suspended step inline using its prompt_template against payload.input. Do NOT call any external tool or other verb. Dispatch memorize-continue with the result. No other verb is valid until this completes."
    }))
}

pub fn handle_instruction(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id_opt: Option<String> = None;
    let mut prompt_opt: Option<String> = None;
    let phase = if trimmed.is_empty() {
        read_state().phase
    } else if let Some(stripped) = trimmed.strip_prefix("phase=") {
        stripped.trim().to_string()
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
            session_id_opt = Some(sid.to_string());
        }
        if let Some(p) = v.get("prompt").and_then(|s| s.as_str()) {
            prompt_opt = Some(p.to_string());
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

    if let Some(p) = &prompt_opt {
        if !p.trim().is_empty() {
            let path = super::gm_dir().join("last-prompt.txt");
            let ps = path.to_string_lossy().to_string();
            let _ = pkfs::write(&ps, p);
        }
    }

    if let Some(sid) = session_id_opt {
        let mut st = read_state();
        st.session_id = Some(sid);
        let _ = super::state::write_state(&st);
    }

    #[cfg(target_arch = "wasm32")]
    {
        crate::poll_detect::scan_turn_entry("");
    }

    let instruction = get_instruction(&phase);
    let mutables_pending = mutables::pending_detailed();
    let prd_items = prd_items_json();
    let prd_pending = prd_pending_count(&prd_items);
    let next = next_phase_hint(&phase);

    let prompt_query = {
        let p = read_last_prompt();
        if p.is_empty() { String::new() } else { p.chars().take(400).collect() }
    };
    let prd_subject_query = prd_items.iter()
        .find(|it| {
            let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            status != "done" && status != "complete" && status != "completed"
        })
        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let query = if !prompt_query.is_empty() { prompt_query } else { prd_subject_query };
    let recall_hits = if query.is_empty() {
        serde_json::Value::Array(Vec::new())
    } else {
        recall::recall_hits(&query, 5)
    };

    let update_available = read_update_available();
    let running_tasks = super::task::live_running_tasks();
    let open_browser_sessions = super::task::open_browser_sessions();
    let stuck_spool = super::task::stuck_spool();
    let unsupervised_watcher = read_unsupervised_watcher();
    let running_tasks_count = match &running_tasks {
        serde_json::Value::Array(a) => a.len(),
        _ => 0,
    };
    let should_scan = should_residual_scan(prd_pending, running_tasks_count);

    let prompt = read_last_prompt();
    let nouns = orient_nouns(&prompt);
    let wave = ready_wave(&prd_items);
    let mutables_pending_count = mutables_pending.len();

    let turn_state = super::state::read_state();
    let await_result = pending_step_block(&turn_state);

    let payload = json!({
        "phase": phase,
        "sub_phase": if await_result.is_some() { "AWAIT-RESULT" } else { "" },
        "await_result": await_result,
        "instruction": instruction,
        "mutables_pending": mutables_pending,
        "mutables_pending_count": mutables_pending_count,
        "epistemic_gap": mutables_pending_count,
        "prd_items": prd_items,
        "prd_pending_count": prd_pending,
        "next_phase_hint": next,
        "recall_hits": recall_hits,
        "orient_nouns": nouns,
        "ready_wave": wave,
        "update_available": update_available,
        "running_tasks": running_tasks,
        "open_browser_sessions": open_browser_sessions,
        "stuck_spool": stuck_spool,
        "unsupervised_watcher": unsupervised_watcher,
        "should_residual_scan": should_scan,
    });
    (payload.to_string(), String::new(), 0)
}
