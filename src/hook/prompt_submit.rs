use super::{is_gemini, is_kilo, is_opencode, project_dir, run_self};
use serde_json::json;
use std::io::Read;

fn sanitize_bash_patterns(s: &str) -> String {
    s.replace("${", "$\\{")
}

pub fn run() {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let prompt = serde_json::from_str::<serde_json::Value>(&stdin)
        .ok()
        .and_then(|v| {
            v["prompt"].as_str()
                .or_else(|| v["message"].as_str())
                .or_else(|| v["userMessage"].as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let project = project_dir();
    let mut parts: Vec<String> = vec![
        "MANDATORY: Use the Skill tool with skill: \"gm\" for ALL work — every task, every response, every code change. The gm skill is the only valid entry point for any coding, planning, or file operation. Do NOT use the Agent tool to load skills. Skills are invoked via the Skill tool only, never as agents. DO NOT use EnterPlanMode. Do NOT attempt work without invoking gm first.".into()
    ];

    if let Some(ref dir) = project {
        if !prompt.is_empty() {
            let search_out = run_self(&["search", "--path", dir, &prompt]);
            if !search_out.is_empty() {
                parts.push(format!("=== search ===\n{}", search_out));
            }
        }

        let insight = run_self(&["codeinsight", dir]);
        if !insight.is_empty() {
            parts.push(format!("=== codeinsight ===\n{}", insight));
        }
    }

    let additional_context = sanitize_bash_patterns(&parts.join("\n\n"));

    let output = if is_gemini() {
        json!({ "systemMessage": additional_context })
    } else if is_opencode() || is_kilo() {
        json!({ "hookSpecificOutput": { "hookEventName": "message.updated", "additionalContext": additional_context } })
    } else {
        json!({ "additionalContext": additional_context })
    };

    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}
