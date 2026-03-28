use super::{is_gemini, is_kilo, is_opencode, project_dir, run_self};
use serde_json::json;
use std::fs;

pub fn run() {
    let project = project_dir();
    ensure_gitignore(project.as_deref());
    let mut parts: Vec<String> = vec![
        "Use the Skill tool with skill: \"gm\" to begin — do NOT use the Agent tool to load skills. Skills are invoked via the Skill tool only, never as agents. All code execution uses exec:<lang> via the Bash tool — never direct Bash(node ...) or Bash(npm ...) or Bash(npx ...) or Bash(plugkit ...).".into()
    ];

    if let Some(ref dir) = project {
        let insight = run_self(&["codeinsight", dir]);
        if !insight.is_empty() {
            parts.push(format!(
                "=== This is your initial insight of the repository, look at every possible aspect of this for initial opinionation and to offset the need for code exploration ===\n{}",
                insight
            ));
        }
    }

    let additional_context = parts.join("\n\n");

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
