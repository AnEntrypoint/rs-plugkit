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

/// Staleness markers describe a condition that was true when some watcher
/// wrote them, and nothing ever rewrites or clears them once that condition
/// resolves -- so a marker left by a since-retired watcher reports its warning
/// on every dispatch forever. Live-hit case: a .gm-plugkit-stale.json written
/// by the old JS watcher kept every instruction response returning
/// gm_plugkit_stale:true naming versions 2.0.1972/2.0.1976, long after the
/// native runtime took over and npm had moved to 2.0.2001. Since gm's own rules
/// require treating staleness as a same-turn deviation, a marker that cannot
/// expire sends every future session chasing an already-resolved condition.
/// Drop any marker older than this window; a genuinely current condition is
/// re-reported by whatever watcher is actually running.
fn expire_stale_marker(v: serde_json::Value) -> serde_json::Value {
    const MAX_MARKER_AGE_MS: i64 = 6 * 60 * 60 * 1000;
    let Some(ts) = v.get("ts") else { return v };
    let written_ms = match ts {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => iso8601_to_ms(s),
        _ => None,
    };
    let Some(written_ms) = written_ms else { return v };
    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
    if now_ms.saturating_sub(written_ms) > MAX_MARKER_AGE_MS {
        return serde_json::Value::Null;
    }
    v
}

/// Minimal `YYYY-MM-DDTHH:MM:SS(.sss)Z` -> epoch-ms via the standard
/// days-from-civil algorithm. Returns None on anything that does not parse, so
/// an unrecognized timestamp leaves its marker untouched rather than silently
/// expiring it.
fn iso8601_to_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' { return None; }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let ms = if b.len() >= 23 && b[19] == b'.' { num(20, 23).unwrap_or(0) } else { 0 };
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

/// `.turn-summary.json` is what SKILL.md's documented boot probe reads at the
/// start of every session, branching on its phase / prd_pending /
/// last_instruction_age_ms. It was written ONLY by the JS wrapper's
/// runSpoolWatcher, so once the native runtime took over nothing rewrote it and
/// it froze -- live-hit as a file still reporting phase=VERIFY prd_pending=14
/// from a retired watcher (version 0.1.905) while a real dispatch returned
/// phase=PLAN prd_pending=0. Every session was starting from fabricated state
/// and only recovering because the first instruction dispatch overrode it.
/// Write it here, from the same authoritative values this response carries, so
/// it cannot drift from what the orchestrator actually believes.
fn write_turn_summary(phase: &str, prd_pending: usize, mutables_pending: usize) {
    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
    let last_instruction_ts = pkfs::read_to_string(
        &super::gm_dir().join("last-instruction-ts").to_string_lossy().to_string(),
    )
    .and_then(|s| s.trim().parse::<i64>().ok())
    .filter(|n| *n > 0);
    let summary = json!({
        "ts": now_ms,
        "runtime": "native",
        "phase": phase,
        "prd_pending": prd_pending,
        "prd_pending_count": prd_pending,
        "mutables_pending_count": mutables_pending,
        "last_instruction_ts": last_instruction_ts,
        "last_instruction_age_ms": last_instruction_ts.map(|t| now_ms.saturating_sub(t)),
        "long_gap_threshold_ms": 300000,
        "update_available": serde_json::Value::Null,
    });
    let path = super::gm_dir().join("exec-spool").join(".turn-summary.json");
    let _ = pkfs::write(&path.to_string_lossy().to_string(), &summary.to_string());
}

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

/// The compiled-in default TEXT for each of the six built-in prose keys --
/// used ONLY as prose::resolve's fallback when neither a .gm/ override file
/// NOR (for a custom phase) any prose_key entry applies. A phase name the
/// active graph doesn't recognize at all still falls back to entry::TEXT,
/// matching the old catch-all `_ => ("entry", entry::TEXT)` arm.
pub fn compiled_default_for_prose_key(key: &str) -> &'static str {
    match key {
        "plan" => plan::TEXT,
        "execute" => execute::TEXT,
        "emit" => emit::TEXT,
        "verify" => verify::TEXT,
        "consolidate" => consolidate::TEXT,
        "update_docs" => update_docs::TEXT,
        "browser" => browser::TEXT,
        _ => entry::TEXT,
    }
}

/// Looks up the current phase's prose_key in the ACTIVE graph (built-in
/// default, or a project's .gm/instructions/fsm/graph.json override) rather
/// than a fixed match -- a custom phase name (e.g. a project-defined REVIEW
/// between EMIT and VERIFY) resolves through its own graph-declared
/// prose_key, served from .gm/instructions/<prose_key>.md the same way the
/// six built-ins always have been. ENTRY/ORCHESTRATOR/BROWSER stay as
/// direct pseudo-phase pass-throughs (not real FSM states a project's graph
/// declares) exactly as before.
pub fn get_instruction(phase: &str) -> String {
    let upper = phase.trim().to_ascii_uppercase();
    let key = match upper.as_str() {
        "ENTRY" | "ORCHESTRATOR" | "" => "entry".to_string(),
        "BROWSER" => "browser".to_string(),
        _ => super::fsm::graph()
            .state(&upper)
            .map(|s| s.prose_key.clone())
            .unwrap_or_else(|| "entry".to_string()),
    };
    let default = compiled_default_for_prose_key(&key);
    crate::prose::resolve(&key, default)
}

fn next_phase_hint(phase: &str) -> Option<String> {
    let upper = phase.trim().to_ascii_uppercase();
    if upper.is_empty() || upper == "ENTRY" || upper == "ORCHESTRATOR" {
        return Some("PLAN".to_string());
    }
    let g = super::fsm::graph();
    g.default_edge_from(&upper)
        .map(|e| e.to.clone())
        // A terminal state's default edge (if the graph declares a
        // self-loop, matching the built-in default's COMPLETE->COMPLETE)
        // is not a genuine "next" hint -- suppress it the same way the old
        // hardcoded COMPLETE => None arm did.
        .filter(|to| !to.eq_ignore_ascii_case(&upper))
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

    // "Valid" now means "a state in the ACTIVE graph" (plus the two
    // always-legal pseudo-phases ENTRY/ORCHESTRATOR and the BROWSER prose
    // pseudo-phase, none of which are real FSM states a project's graph
    // declares) rather than a fixed compiled list -- a custom-graph
    // project's own phase names are accepted here without a Rust rebuild.
    let is_valid_phase = |upper: &str| -> bool {
        matches!(upper, "ENTRY" | "ORCHESTRATOR" | "BROWSER") || super::fsm::graph().has_state(upper)
    };
    let phase = match raw_phase_opt.as_deref() {
        None => read_state().phase.as_str().to_string(),
        Some(p) => {
            let upper = p.trim().to_ascii_uppercase();
            if upper.is_empty() || is_valid_phase(&upper) {
                if upper.is_empty() { read_state().phase.as_str().to_string() } else { upper }
            } else {
                let known: Vec<String> = super::fsm::graph().states.iter().map(|s| s.key.clone()).collect();
                ilog(&format!(
                    "instruction::handle invalid phase '{}' (len={}); falling back to disk state. Valid (active graph): {}",
                    &p.chars().take(80).collect::<String>(),
                    p.len(),
                    known.join("|")
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
        !p.trim().is_empty() && is_valid_phase(&p.trim().to_ascii_uppercase())
    }).unwrap_or(false);

    if fresh_prompt && !raw_phase_override && phase != "PLAN" && phase != "COMPLETE"
        && prd_pending_count(&prd_items_json()) == 0
    {
        ilog(&format!("instruction::handle fresh prompt on stuck {} chain (no pending PRD) -> reset phase to PLAN", phase));
        phase = "PLAN".to_string();
        let mut st = read_state();
        st.phase = Phase::plan();
        let _ = super::state::write_state(&st);
    }

    if phase == "COMPLETE" && !raw_phase_override {
        if fresh_prompt {
            phase = "PLAN".to_string();
            let mut st = read_state();
            st.phase = Phase::plan();
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
    let unsupervised_watcher = expire_stale_marker(read_spool_json(".pre-supervised-watcher.json"));
    let gm_plugkit_stale = expire_stale_marker(read_spool_json(".gm-plugkit-stale.json"));
    let wrapper_stale_in_memory = expire_stale_marker(read_spool_json(".wrapper-stale-in-memory.json"));
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

    write_turn_summary(&phase, prd_pending, mutables_pending_count);

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
        "prd_pending": prd_pending,
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
