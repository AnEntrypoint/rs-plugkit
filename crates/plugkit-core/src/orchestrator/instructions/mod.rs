pub mod entry;
pub mod plan;
pub mod execute;
pub mod emit;
pub mod verify;
pub mod consolidate;
pub mod update_docs;
pub mod browser;

use serde_json::json;
use super::state::{read_state, Phase};
use super::mutables;
use super::prd;
use super::recall;
use crate::pkfs;

fn read_spool_json(name: &str) -> serde_json::Value {
    let path = super::gm_dir().join("exec-spool").join(name);
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

pub fn get_instruction(phase: &str) -> String {
    let (key, default) = match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => ("entry", entry::TEXT),
        "PLAN" => ("plan", plan::TEXT),
        "EXECUTE" => ("execute", execute::TEXT),
        "EMIT" => ("emit", emit::TEXT),
        "VERIFY" => ("verify", verify::TEXT),
        "CONSOLIDATE" => ("consolidate", consolidate::TEXT),
        "COMPLETE" => ("update_docs", update_docs::TEXT),
        "BROWSER" => ("browser", browser::TEXT),
        _ => ("entry", entry::TEXT),
    };
    crate::prose::resolve(key, default)
}

fn next_phase_hint(phase: &str) -> Option<&'static str> {
    match phase.trim().to_ascii_uppercase().as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => Some("PLAN"),
        "PLAN" => Some("EXECUTE"),
        "EXECUTE" => Some("EMIT"),
        "EMIT" => Some("VERIFY"),
        "VERIFY" => Some("CONSOLIDATE"),
        "CONSOLIDATE" => Some("COMPLETE"),
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
    items.iter().filter(|it| item_is_open(it)).count()
}

fn item_is_open(it: &serde_json::Value) -> bool {
    let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
    prd::status_is_open(status)
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

#[cfg(target_arch = "wasm32")]
fn ilog(msg: &str) {
    #[link(wasm_import_module = "env")]
    extern "C" { fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32; }
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}
#[cfg(not(target_arch = "wasm32"))]
fn ilog(_msg: &str) {}

#[cfg(target_arch = "wasm32")]
fn idev(event: &str, detail: &str) {
    #[link(wasm_import_module = "env")]
    extern "C" { fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32; }
    let evt = json!({
        "event": format!("deviation.{}", event),
        "sub": "hook",
        "detail": detail,
        "ts": super::state::now_ms(),
        "source": "rs-plugkit/instruction",
    });
    let line = format!("evt: {}", evt);
    let _ = unsafe { host_log(1, line.as_ptr(), line.len() as u32) };
}
#[cfg(not(target_arch = "wasm32"))]
fn idev(_event: &str, _detail: &str) {}

pub fn handle_instruction(content: &str) -> (String, String, i32) {
    ilog(&format!("instruction::handle start body_len={}", content.len()));
    let trimmed = content.trim();
    let mut session_id_opt: Option<String> = None;
    let mut prompt_opt: Option<String> = None;
    let raw_phase_opt = if trimmed.is_empty() {
        None
    } else if let Some(stripped) = trimmed.strip_prefix("phase=") {
        Some(stripped.trim().to_string())
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
            session_id_opt = Some(sid.to_string());
        }
        if let Some(p) = v.get("prompt").and_then(|s| s.as_str()) {
            prompt_opt = Some(p.to_string());
        }
        if let Some(s) = v.as_str() {
            Some(s.to_string())
        } else if let Some(s) = v.get("phase").and_then(|p| p.as_str()) {
            Some(s.to_string())
        } else {
            None
        }
    } else {
        Some(trimmed.to_string())
    };

    let valid_phases = ["ENTRY", "ORCHESTRATOR", "PLAN", "EXECUTE", "EMIT", "VERIFY", "CONSOLIDATE", "COMPLETE", "BROWSER"];
    let phase = match raw_phase_opt.as_deref() {
        None => read_state().phase.as_str().to_string(),
        Some(p) => {
            let upper = p.trim().to_ascii_uppercase();
            if upper.is_empty() || valid_phases.contains(&upper.as_str()) {
                if upper.is_empty() { read_state().phase.as_str().to_string() } else { upper }
            } else {
                ilog(&format!(
                    "instruction::handle invalid phase '{}' (len={}); falling back to disk state. Valid: PLAN|EXECUTE|EMIT|VERIFY|CONSOLIDATE|COMPLETE|BROWSER",
                    &p.chars().take(80).collect::<String>(),
                    p.len()
                ));
                read_state().phase.as_str().to_string()
            }
        }
    };

    let mut phase = phase;
    let fresh_prompt = prompt_opt
        .as_deref()
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false);

    if let Some(p) = &prompt_opt {
        if !p.trim().is_empty() {
            let path = super::gm_dir().join("last-prompt.txt");
            let ps = path.to_string_lossy().to_string();
            let _ = pkfs::write(&ps, p);
        }
    }

    let raw_phase_override = raw_phase_opt.as_deref().map(|p| {
        !p.trim().is_empty() && valid_phases.contains(&p.trim().to_ascii_uppercase().as_str())
    }).unwrap_or(false);

    if fresh_prompt && !raw_phase_override && phase != "PLAN" && phase != "COMPLETE"
        && prd_pending_count(&prd_items_json()) == 0
    {
        ilog(&format!("instruction::handle fresh prompt on stuck {} chain (no pending PRD) -> reset phase to PLAN", phase));
        phase = "PLAN".to_string();
        let mut st = read_state();
        st.phase = Phase::Plan;
        let _ = super::state::write_state(&st);
    }

    if phase == "COMPLETE" && !raw_phase_override {
        if fresh_prompt {
            phase = "PLAN".to_string();
            let mut st = read_state();
            st.phase = Phase::Plan;
            let _ = super::state::write_state(&st);
            ilog("instruction::handle fresh prompt on COMPLETE chain -> reset phase to PLAN");
        } else if prd_pending_count(&prd_items_json()) == 0 && session_id_opt.is_some() {
            idev(
                "complete-chain-poll",
                "instruction re-dispatched on terminal chain (phase=COMPLETE, prd_pending=0, no fresh prompt). The chain is closed; stop dispatching. A new request resets to PLAN.",
            );
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

    let early_next_step_path = super::gm_dir().join("next-step.md");
    let early_next_step_path_s = early_next_step_path.to_string_lossy().to_string();
    let existing_next_step = pkfs::read_to_string(&early_next_step_path_s).unwrap_or_default();
    let existing_body = existing_next_step.splitn(2, "\n\n---\n\n").nth(1).unwrap_or("");
    if existing_body != instruction || !existing_next_step.contains(&format!("Phase: {}\n", phase)) {
        let early_next_step = format!(
            "# Next step\n\nPhase: {}\nUpdated: {}\n\n---\n\n{}",
            phase,
            super::state::now_ms(),
            instruction
        );
        let _ = pkfs::write(&early_next_step_path_s, &early_next_step);
    }

    let mutables_pending = mutables::pending_detailed();
    let prd_items = prd_items_json();
    let prd_pending = prd_pending_count(&prd_items);
    let prd_items_open: Vec<serde_json::Value> = prd_items.iter().filter(|it| item_is_open(it)).cloned().collect();
    let next = next_phase_hint(&phase);

    let prompt_query = {
        let p = read_last_prompt();
        if p.is_empty() { String::new() } else { p.chars().take(400).collect() }
    };
    let prd_subject_query = prd_items.iter()
        .find(|it| item_is_open(it))
        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let query = if !prompt_query.is_empty() { prompt_query } else { prd_subject_query };
    ilog(&format!("instruction::handle pre-recall query_len={} prd_pending={}", query.len(), prd_pending));
    let recall_hits = if query.is_empty() {
        serde_json::Value::Array(Vec::new())
    } else {
        recall::recall_hits(&query, 5)
    };
    ilog("instruction::handle post-recall");

    let update_available = read_spool_json(".update-available.json");
    let running_tasks = super::task::live_running_tasks();
    let open_browser_sessions = super::task::open_browser_sessions();
    let stuck_spool = super::task::stuck_spool();
    let unsupervised_watcher = read_spool_json(".pre-supervised-watcher.json");
    let gm_plugkit_stale = read_spool_json(".gm-plugkit-stale.json");
    let wrapper_stale_in_memory = read_spool_json(".wrapper-stale-in-memory.json");
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

    #[cfg(target_arch = "wasm32")]
    let route_hint = crate::wasm_dispatch::route_hint(&prompt, 8000);
    #[cfg(not(target_arch = "wasm32"))]
    let route_hint = serde_json::Value::Null;

    // Proactive delivery: attach the already-indexed codeinsight overview
    // (file/symbol counts, top kinds, largest files) to every instruction
    // response instead of requiring a separate codesearch dispatch just to
    // learn "what's in this codebase" -- a one-shot overview the agent
    // would otherwise have to explicitly ask for. Purely a query over the
    // EXISTING index (never triggers a scan/parse/embed pass), so this is
    // cheap on every dispatch; null when no index exists yet (first-ever
    // session in a fresh repo, before any codesearch has run).
    #[cfg(target_arch = "wasm32")]
    let codeinsight_overview = crate::code_index::overview();
    #[cfg(not(target_arch = "wasm32"))]
    let codeinsight_overview = serde_json::Value::Null;

    let payload = json!({
        "phase": phase,
        "sub_phase": if await_result.is_some() { "AWAIT-RESULT" } else { "" },
        "await_result": await_result,
        "instruction": instruction,
        "mutables_pending": mutables_pending,
        "mutables_pending_count": mutables_pending_count,
        "epistemic_gap": mutables_pending_count,
        "prd_items": prd_items_open,
        "prd_total_count": prd_items.len(),
        "prd_pending_count": prd_pending,
        "next_phase_hint": next,
        "recall_hits": recall_hits,
        "orient_nouns": nouns,
        "codeinsight_overview": codeinsight_overview,
        "ready_wave": wave,
        "update_available": update_available,
        "gm_plugkit_stale": gm_plugkit_stale,
        "wrapper_stale_in_memory": wrapper_stale_in_memory,
        "running_tasks": running_tasks,
        "open_browser_sessions": open_browser_sessions,
        "stuck_spool": stuck_spool,
        "unsupervised_watcher": unsupervised_watcher,
        "should_residual_scan": should_scan,
        "route_hint": route_hint,
        "discipline_policies": super::discipline_note::active_policies(),
    });
    let s = payload.to_string();
    ilog(&format!("instruction::handle done out_len={}", s.len()));
    (s, String::new(), 0)
}
