use super::{is_kilo, is_opencode, load_prompt, project_dir, run_self};
use serde_json::json;
use std::{env, io::Read};

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
    let mut autonomous = false;
    if let Some(ref dir) = project {
        let gm = std::path::Path::new(dir).join(".gm");
        let live = gm.join("prd.yml");
        let paused = gm.join("prd.paused.yml");
        if paused.exists() && !live.exists() {
            let _ = std::fs::rename(&paused, &live);
        }
        autonomous = live.exists();
        let needs_gm = gm.join("needs-gm");
        let _ = std::fs::create_dir_all(&gm);
        let global_needs_gm = super::tools_dir().join("needs-gm");
        if autonomous {
            let _ = std::fs::remove_file(&needs_gm);
            let _ = std::fs::remove_file(&global_needs_gm);
        } else {
            let _ = std::fs::write(&needs_gm, "1");
            let _ = std::fs::write(&global_needs_gm, "1");
        }
        let turn_state = serde_json::json!({
            "turnId": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            "firstToolFired": false,
            "execCallsSinceMemorize": 0,
            "recallFiredThisTurn": false
        });
        let _ = std::fs::write(gm.join("turn-state.json"), serde_json::to_string(&turn_state).unwrap_or_default());
    }

    if autonomous {
        let msg = "AUTONOMOUS MODE \u{2014} .gm/prd.yml exists. Continue executing without re-invoking gm:gm. Resolve doubts via witnessed probe, recall, or PRD; never ask the user. Spawn Agent(gm:memorize) for any unknown\u{2192}known transition same turn. When .prd is empty + git clean + pushed, invoke update-docs to close out.";
        let out = if is_opencode() || is_kilo() {
            json!({ "hookSpecificOutput": { "hookEventName": "message.updated", "additionalContext": msg } })
        } else {
            json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": msg } })
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }

    if let Some(ref dir) = project {
        let dir_for_search = dir.clone();
        let dir_for_insight = dir.clone();
        let prompt_for_search = prompt.clone();
        let dir_for_prd = dir.clone();
        let prompt_for_subagent = prompt.clone();

        let search_handle = if !prompt.is_empty() {
            Some(std::thread::spawn(move || {
                run_self(&["search", "--path", &dir_for_search, &prompt_for_search])
            }))
        } else { None };
        let insight_handle = std::thread::spawn(move || {
            run_self(&["codeinsight", &dir_for_insight])
        });

        let mut context_parts: Vec<String> = Vec::new();

        if !prompt.is_empty() {
            let recall_q = super::rs_learn::short_recall_query(&prompt, dir);
            let proj_q = super::rs_learn::project_query(dir);
            let recall = super::rs_learn::recall(&recall_q, dir, 5);
            let proj_recall = if proj_q != recall_q {
                super::rs_learn::recall(&proj_q, dir, 3)
            } else {
                String::new()
            };
            let combined = match (recall.is_empty(), proj_recall.is_empty()) {
                (false, false) => format!("{}\n\n---\n{}", recall, proj_recall),
                (false, true) => recall,
                (true, false) => proj_recall,
                (true, true) => String::new(),
            };

            if let Some(h) = search_handle {
                let search_out = h.join().unwrap_or_default();
                if !search_out.is_empty() {
                    context_parts.push(format!("=== search ===\n{}", search_out));
                }
            }
            if !combined.is_empty() {
                context_parts.push(format!("=== rs-learn recall (cross-session memory for this prompt) ===\n{}", combined));
            }
        }

        let insight = insight_handle.join().unwrap_or_default();
        if !insight.is_empty() {
            context_parts.push(format!("=== codeinsight ===\n{}", insight));
        }

        super::rs_learn::tick_and_maybe_run_deep_cycles(dir);

        let prd_path = std::path::Path::new(&dir_for_prd).join(".gm").join("prd.yml");
        let workspace_context = context_parts.join("\n\n");

        let subagent_prompt = format!(
            "User prompt: {}\n\n{}\n\nWorkspace: {}\nPRD path: {}",
            prompt_for_subagent,
            if workspace_context.is_empty() { String::new() } else { format!("Initial context:\n{}", workspace_context) },
            dir_for_prd,
            prd_path.display()
        );

        let system_message = format!(
            "User prompt: {}\n\n{}\n\nWorkspace: {}\nPRD path: {}\n\nInvoke Skill(gm:gm) first. Resolve unknowns with witnessed probes, recall, or the PRD. Never ask the user when the PRD is present.",
            prompt_for_subagent,
            if workspace_context.is_empty() { String::new() } else { format!("Initial context:\n{}", workspace_context) },
            dir_for_prd,
            prd_path.display()
        );

        let sess = env::var("CLAUDE_SESSION_ID").unwrap_or_default();
        let project_str = project.as_deref().unwrap_or("");
        rs_exec::obs::event("hook", "prompt-submit-detail", serde_json::json!({
            "project_dir": project_str,
            "sess": sess,
            "autonomous": autonomous,
            "prompt_len": prompt.len()
        }));
        println!("{}", serde_json::to_string_pretty(&json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": system_message } })).unwrap_or_default());
    } else {
        println!("{}", serde_json::to_string_pretty(&json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": "" } })).unwrap_or_default());
    }
}
