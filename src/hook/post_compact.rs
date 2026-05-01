use serde_json::json;
use std::path::Path;

pub fn run() {
    let project = match std::env::var("CLAUDE_PROJECT_DIR") {
        Ok(p) if !p.is_empty() => p,
        _ => match std::env::current_dir() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => return,
        },
    };
    let lastskill_path = Path::new(&project).join(".gm").join("lastskill");
    let last = match std::fs::read_to_string(&lastskill_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if last.is_empty() { return; }
    let out = json!({
        "type": "text",
        "text": format!("Last active skill before compaction: `{0}`. Invoke the Skill tool with skill: \"{0}\" to resume it.", last)
    });
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
}
