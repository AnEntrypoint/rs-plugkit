use super::{is_gemini, is_kilo, is_opencode, project_dir, run_self};
use serde_json::json;
use std::fs;

pub fn run() {
    let project = project_dir();
    ensure_gitignore(project.as_deref());
    let mut parts: Vec<String> = vec![
        "=== TOOL RULES ===\n\nSkill tool: invoke skills by name (e.g. skill: \"gm\"). Never use the Agent tool to load skills.\n\nBash tool: only these formats are allowed —\n  exec:nodejs / exec:python / exec:bash / exec:typescript / exec:go / exec:rust / exec:c / exec:cpp / exec:java\n  exec:browser  (JS automation against `page`)\n  exec:codesearch  (natural language search)\n  exec:status / exec:sleep / exec:close / exec:runner / exec:type\n  git <args>  (git commands directly, no exec: prefix)\n  Everything else is blocked. Never Bash(node ...) or Bash(npm ...) or Bash(npx ...).\n\nGlob/Grep/Find/Explore: blocked — use exec:codesearch instead.\n\nStart work: invoke the gm skill first.".into()
    ];

    if let Some(ref dir) = project {
        let insight = {
            let cached = run_self(&["codeinsight", dir, "--read-cache"]);
            if cached.is_empty() || cached.starts_with("No cache") || cached.starts_with("Error") {
                run_self(&["codeinsight", dir, "--cache"])
            } else {
                cached
            }
        };
        if !insight.is_empty() && !insight.starts_with("Error") && !insight.starts_with("No cache") {
            parts.push(format!(
                "=== This is your initial insight of the repository, look at every possible aspect of this for initial opinionation and to offset the need for code exploration ===\n{}",
                insight
            ));
        }
    }

    let additional_context = parts.join("\n\n").replace("${", "$\\{");

    let output = if is_gemini() {
        json!({ "systemMessage": additional_context })
    } else if is_opencode() || is_kilo() {
        json!({ "hookSpecificOutput": { "hookEventName": "session.created", "additionalContext": additional_context } })
    } else {
        json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": additional_context } })
    };

    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}

fn ensure_gitignore(project_dir: Option<&str>) {
    let Some(dir) = project_dir else { return };
    let gitignore = std::path::Path::new(dir).join(".gitignore");
    let entry = ".gm-stop-verified";
    let content = fs::read_to_string(&gitignore).unwrap_or_default();
    if content.lines().any(|l| l.trim() == entry) { return; }
    let new_content = if content.is_empty() || content.ends_with('\n') {
        format!("{}{}\n", content, entry)
    } else {
        format!("{}\n{}\n", content, entry)
    };
    let _ = fs::write(&gitignore, new_content);
}
