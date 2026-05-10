use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const EXEC_TOOL: &str = "Bash";
const MIN_OUTPUT_LEN: usize = 20;
const HARD_BLOCK_AT: u64 = 3;
const SPOOL_POLL_INTERVAL_MS: u64 = 100;
const SPOOL_POLL_MAX: u64 = 300;

const UTILITY_VERBS: &[&str] = &[
    "recall", "memorize", "codesearch", "wait", "sleep", "status",
    "runner", "type", "kill-port", "close", "pause", "forget", "feedback",
    "learn-status", "learn:status", "learn-debug", "learn:debug", "learn-build", "learn:build",
];

fn handle_write(tool_input: &Value) -> Option<(String, bool)> {
    let file_path = tool_input["file_path"].as_str()?;
    let path = Path::new(file_path);

    let components: Vec<_> = path.components().collect();
    let has_exec_spool = components.windows(2).any(|w| {
        let a = w[0].as_os_str().to_string_lossy();
        let b = w[1].as_os_str().to_string_lossy();
        (a == "exec-spool" && b == "in") || (a.contains("exec-spool") && b == "in")
    });
    if !has_exec_spool {
        return None;
    }

    let stem = path.file_stem()?.to_string_lossy();
    let task_id: u64 = stem.parse().ok()?;

    let in_dir = path.parent()?;
    let spool_dir = in_dir.parent()?;
    let out_path = spool_dir.join("out").join(format!("{}.json", task_id));

    rs_exec::obs::event("hook", "post-tool-use.spool-write", serde_json::json!({
        "task_id": task_id,
        "out_path": out_path.display().to_string()
    }));

    for _ in 0..SPOOL_POLL_MAX {
        if out_path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(SPOOL_POLL_INTERVAL_MS));
    }

    if !out_path.exists() {
        let msg = format!(
            "[exec task={}] ERROR: timed out after 30s waiting for spool output. Is the spool watcher running?",
            task_id
        );
        return Some((msg, false));
    }

    let raw = std::fs::read_to_string(&out_path).ok()?;
    let result: Value = serde_json::from_str(&raw).ok()?;

    let ok = result["ok"].as_bool().unwrap_or(false);
    let lang = result["lang"].as_str().unwrap_or("unknown");
    let exit_code = result["exitCode"].as_i64().unwrap_or(-1);
    let output = result["output"].as_str()
        .or_else(|| result["error"].as_str())
        .unwrap_or("");

    let msg = if ok {
        format!("[exec task={} lang={} exitCode={}]\n{}", task_id, lang, exit_code, output)
    } else {
        format!("[exec task={} lang={} exitCode={}] ERROR\n{}", task_id, lang, exit_code, output)
    };

    Some((msg, ok && output.len() > MIN_OUTPUT_LEN))
}

pub fn run() {
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
        if let Some((msg, should_count)) = handle_write(tool_input) {
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
            "exec: run completed. MEMORIZE CHECK: did this output resolve any prior unknown? If YES → spawn Agent(subagent_type='gm:memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<fact>') NOW. Skipping = memory leak. (Counter: {}/3 before hard block.)",
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
