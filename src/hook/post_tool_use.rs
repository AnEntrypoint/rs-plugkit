use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const EXEC_TOOL: &str = "Bash";
const MIN_OUTPUT_LEN: usize = 20;
const HARD_BLOCK_AT: u64 = 10;
const SPOOL_POLL_INTERVAL_MS: u64 = 50;
const SPOOL_STREAM_WINDOW_MS: u64 = 30_000;

const UTILITY_VERBS: &[&str] = &[
    "recall", "memorize", "codesearch", "wait", "sleep", "status",
    "runner", "type", "kill-port", "close", "pause", "forget", "feedback",
    "learn-status", "learn:status", "learn-debug", "learn:debug", "learn-build", "learn:build",
];

fn handle_write(tool_input: &Value) -> Option<(String, bool)> {
    let file_path = tool_input["file_path"].as_str()?;
    let fp_norm = file_path.replace('\\', "/");
    if fp_norm.ends_with(".gm/mutables.yml") || fp_norm.ends_with(".gm/prd.yml") || fp_norm.ends_with(".gm/no-memorize-this-turn") {
        return None;
    }
    let path = Path::new(file_path);

    // Locate "exec-spool/in" window anywhere in path components.
    // Returns index of the "in" component.
    let components: Vec<_> = path.components().collect();
    let in_comp_idx = components.windows(2).enumerate().find_map(|(i, w)| {
        let a = w[0].as_os_str().to_string_lossy();
        let b = w[1].as_os_str().to_string_lossy();
        if (a == "exec-spool" || a.contains("exec-spool")) && b == "in" {
            Some(i + 1)
        } else {
            None
        }
    })?;

    let stem = path.file_stem()?.to_string_lossy().to_string();
    stem.parse::<u64>().ok()?;

    let depth_after_in = components.len() - 1 - in_comp_idx;
    let spool_dir = if depth_after_in == 2 {
        path.parent()?.parent()?.parent()?.to_path_buf()
    } else {
        return None;
    };

    let task_id: u64 = stem.parse().ok()?;
    let meta_path = spool_dir.join("out").join(format!("{}.json", task_id));
    let out_stream = spool_dir.join("out").join(format!("{}.out", task_id));
    let err_stream = spool_dir.join("out").join(format!("{}.err", task_id));

    rs_exec::obs::event("hook", "post-tool-use.spool-write", serde_json::json!({
        "task_id": task_id,
        "meta_path": meta_path.display().to_string(),
        "out_stream": out_stream.display().to_string(),
        "err_stream": err_stream.display().to_string(),
    }));

    let started = std::time::Instant::now();
    let deadline = started + Duration::from_millis(SPOOL_STREAM_WINDOW_MS);
    let max_iters: u64 = (SPOOL_STREAM_WINDOW_MS / SPOOL_POLL_INTERVAL_MS) + 50;
    let mut iter: u64 = 0;

    loop {
        iter += 1;
        let now = std::time::Instant::now();
        if now >= deadline || iter > max_iters {
            let out_text = std::fs::read_to_string(&out_stream).unwrap_or_default();
            let err_text = std::fs::read_to_string(&err_stream).unwrap_or_default();
            let out_disp = out_stream.display();
            let err_disp = err_stream.display();
            let meta_disp = meta_path.display();
            let total_len = out_text.len() + err_text.len();
            rs_exec::obs::event("hook", "post-tool-use.spool-deadline", serde_json::json!({
                "task_id": task_id,
                "iter": iter,
                "elapsed_ms": now.duration_since(started).as_millis() as u64,
                "partial_bytes": total_len,
            }));
            let msg = if total_len == 0 {
                format!(
                    "[exec task={tid}] still running after 30s window (no output yet). Continue with: `exec:wait\\n<seconds>` (pure timer), `exec:sleep\\n{tid}` (block until next output), `exec:status\\n{tid}` (poll), `exec:close\\n{tid}` (terminate). Streams append at: {out}, {err}. Metadata lands at: {meta} once complete. If the watcher is not running, start it via `exec:runner\\nstart`.",
                    tid = task_id, out = out_disp, err = err_disp, meta = meta_disp
                )
            } else {
                let mut body = String::new();
                if !out_text.is_empty() {
                    body.push_str("\n--- stdout ---\n");
                    body.push_str(&out_text);
                }
                if !err_text.is_empty() {
                    if !body.ends_with('\n') { body.push('\n'); }
                    body.push_str("--- stderr ---\n");
                    body.push_str(&err_text);
                }
                format!(
                    "[exec task={tid} partial output after 30s — still running]{body}\n--- end partial ---\nTask is NOT finished. Continue with: `exec:sleep\\n{tid}` (block until next output chunk), `exec:wait\\n<seconds>` (pure timer), `exec:status\\n{tid}` (poll), or `exec:close\\n{tid}` (terminate). Streams keep growing at: {out}, {err}. Metadata will land at: {meta} once complete.",
                    tid = task_id, body = body, out = out_disp, err = err_disp, meta = meta_disp
                )
            };
            return Some((msg, total_len > MIN_OUTPUT_LEN));
        }

        if meta_path.exists() {
            let raw = std::fs::read_to_string(&meta_path).ok()?;
            let result: Value = serde_json::from_str(&raw).ok()?;
            let ok = result["ok"].as_bool().unwrap_or(false);
            let lang = result["lang"].as_str().unwrap_or("unknown");
            let exit_code = result["exitCode"].as_i64().unwrap_or(-1);
            let duration_ms = result["durationMs"].as_u64().unwrap_or(0);
            let timed_out = result["timedOut"].as_bool().unwrap_or(false);
            let err_field = result["error"].as_str().unwrap_or("");
            let out_text = std::fs::read_to_string(&out_stream).unwrap_or_default();
            let err_text = std::fs::read_to_string(&err_stream).unwrap_or_default();
            let header = if timed_out {
                format!("[exec task={} lang={} TIMED OUT after {}ms]", task_id, lang, duration_ms)
            } else if ok {
                format!("[exec task={} lang={} exitCode={} durationMs={}]", task_id, lang, exit_code, duration_ms)
            } else {
                format!("[exec task={} lang={} exitCode={} durationMs={}] ERROR", task_id, lang, exit_code, duration_ms)
            };
            let mut body = String::new();
            if !out_text.is_empty() {
                body.push_str("\n--- stdout ---\n");
                body.push_str(&out_text);
            }
            if !err_text.is_empty() {
                if !body.ends_with('\n') { body.push('\n'); }
                body.push_str("--- stderr ---\n");
                body.push_str(&err_text);
            }
            if !err_field.is_empty() {
                if !body.ends_with('\n') { body.push('\n'); }
                body.push_str("--- error ---\n");
                body.push_str(err_field);
            }
            let total_len = out_text.len() + err_text.len();
            let msg = format!("{}{}", header, body);
            return Some((msg, ok && total_len > MIN_OUTPUT_LEN));
        }

        if iter % 30 == 0 {
            rs_exec::obs::event("hook", "post-tool-use.spool-poll", serde_json::json!({
                "task_id": task_id,
                "iter": iter,
                "elapsed_ms": now.duration_since(started).as_millis() as u64,
            }));
        }

        std::thread::sleep(Duration::from_millis(SPOOL_POLL_INTERVAL_MS));
    }
}

pub fn run() {
    std::thread::spawn(|| {
        let _ = std::panic::catch_unwind(|| super::session_start::start_exec_spool());
    });
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: Value = serde_json::from_str(&stdin).unwrap_or_default();
    let tool_name = data["tool_name"].as_str()
        .or_else(|| data["tool_use"]["name"].as_str())
        .unwrap_or("");
    let tool_input = if data["tool_input"].is_object() || data["tool_input"].is_array() {
        &data["tool_input"]
    } else {
        &data["tool_use"]["input"]
    };
    let tool_output = data["tool_result"].as_str()
        .or_else(|| data["output"].as_str())
        .unwrap_or("");
    rs_exec::obs::event("hook", "post-tool-use.tool", serde_json::json!({ "tool_name": tool_name, "out_len": tool_output.len() }));

    if tool_name == "Write" {
        let ti = tool_input.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || handle_write(&ti)));
        let handled = match result {
            Ok(opt) => opt,
            Err(_) => {
                rs_exec::obs::event("hook", "post-tool-use.handle-write-panic", serde_json::json!({}));
                Some((format!("[exec — internal error during post-tool-use poll; task may still be running]"), false))
            }
        };
        if let Some((msg, should_count)) = handled {
            println!("{}", serde_json::json!({ "systemMessage": msg }));
            if should_count {
                let project = super::project_dir().unwrap_or_default();
                if !project.is_empty() {
                    let gm = Path::new(&project).join(".gm");
                    let ts_path = gm.join("turn-state.json");
                    let mut state = read_state(&ts_path);
                    state.exec_calls_since_memorize += 1;
                    write_state(&gm, &ts_path, &state);
                }
            }
        }
        return;
    }

    let project = match super::project_dir() {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };
    let gm = Path::new(&project).join(".gm");
    let ts_path = gm.join("turn-state.json");
    let mut state = read_state(&ts_path);

    if !state.first_tool_fired {
        state.first_tool_fired = true;
        state.first_tool_name = Some(tool_name.to_string());
    }

    let is_memorize_agent = tool_name == "Agent" && tool_input.to_string().to_lowercase().contains("memorize");
    if is_memorize_agent {
        state.exec_calls_since_memorize = 0;
        let _ = std::fs::remove_file(gm.join("no-memorize-this-turn"));
    }

    let mut should_emit_hint = false;

    if tool_name == EXEC_TOOL {
        let cmd = tool_input["command"].as_str().unwrap_or("");
        let leading_verb = cmd.trim_start().strip_prefix("exec:").map(|rest| {
            rest.split(|c: char| c.is_whitespace()).next().unwrap_or("").trim().to_lowercase()
        });
        if leading_verb.as_deref() == Some("recall") {
            state.recall_fired_this_turn = true;
        }
        let is_utility = leading_verb.as_deref().map(|v| UTILITY_VERBS.contains(&v)).unwrap_or(false);
        if !is_utility && tool_output.len() > MIN_OUTPUT_LEN {
            state.exec_calls_since_memorize += 1;
            should_emit_hint = true;
        }
    }

    write_state(&gm, &ts_path, &state);

    if should_emit_hint {
        rs_exec::obs::event("hook", "post-tool-use.hint", serde_json::json!({
            "counter": state.exec_calls_since_memorize,
            "hard_block_at": HARD_BLOCK_AT
        }));
        let msg = format!(
            "exec: run completed. MEMORIZE CHECK: did this output resolve any prior unknown? If YES → spawn Agent(subagent_type='gm:memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<fact>') NOW. Skipping = memory leak. (Counter: {}/10 before hard block.)",
            state.exec_calls_since_memorize
        );
        println!("{}", serde_json::json!({ "systemMessage": msg }));
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct TurnState {
    #[serde(rename = "turnId", default)]
    turn_id: u64,
    #[serde(rename = "firstToolFired", default)]
    first_tool_fired: bool,
    #[serde(rename = "firstToolName", default)]
    first_tool_name: Option<String>,
    #[serde(rename = "execCallsSinceMemorize", default)]
    exec_calls_since_memorize: u64,
    #[serde(rename = "recallFiredThisTurn", default)]
    recall_fired_this_turn: bool,
}

fn read_state(path: &Path) -> TurnState {
    std::fs::read_to_string(path).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_state(gm: &Path, path: &Path, state: &TurnState) {
    let _ = std::fs::create_dir_all(gm);
    let _ = std::fs::write(path, serde_json::to_string(state).unwrap_or_default());
}
