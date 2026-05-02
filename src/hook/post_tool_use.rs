use serde_json::Value;
use std::io::Read;
use std::path::Path;

const EXEC_TOOL: &str = "Bash";
const MIN_OUTPUT_LEN: usize = 20;
const HARD_BLOCK_AT: u64 = 3;

const UTILITY_VERBS: &[&str] = &[
    "recall", "memorize", "codesearch", "wait", "sleep", "status",
    "runner", "type", "kill-port", "close", "pause", "forget", "feedback",
    "learn-status", "learn:status", "learn-debug", "learn:debug", "learn-build", "learn:build",
];

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
