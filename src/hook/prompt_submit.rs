use super::{is_gemini, is_kilo, is_opencode, load_prompt, project_dir, run_self};
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
            "turnId": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
            "firstToolFired": false,
            "execCallsSinceMemorize": 0,
            "recallFiredThisTurn": false
        });
        let _ = std::fs::write(gm.join("turn-state.json"), turn_state.to_string());
        let _ = std::fs::remove_file(gm.join("no-memorize-this-turn"));
    }

    if autonomous {
        let msg = "AUTONOMOUS MODE \u{2014} .gm/prd.yml exists. Continue executing without re-invoking gm:gm. Resolve doubts via witnessed probe, recall, or PRD; never ask the user. Spawn Agent(gm:memorize) for any unknown\u{2192}known transition same turn. When .prd is empty + git clean + pushed, invoke update-docs to close out.";
        let out = if is_gemini() {
            json!({ "systemMessage": msg })
        } else if is_opencode() || is_kilo() {
            json!({ "hookSpecificOutput": { "hookEventName": "message.updated", "additionalContext": msg } })
        } else {
            json!({ "systemMessage": msg })
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }

    let parallel_hint = {
        let q_count = prompt.matches('?').count();
        let numbered = prompt.lines().filter(|l| {
            let t = l.trim_start();
            t.starts_with("1.") || t.starts_with("2.") || t.starts_with("- ") || t.starts_with("1)") || t.starts_with("2)")
        }).count();
        if q_count >= 2 || numbered >= 2 {
            Some(format!("\n\n=== PARALLELISM HINT ===\n\nThis prompt contains {} questions / {} list items — they appear independent. After invoking gm and writing the PRD, launch parallel Agent(subagent_type=\"gm:gm\") subagents (≤3 concurrent) for independent items in ONE message. Sequential execution of independent work is a critical violation.", q_count, numbered))
        } else { None }
    };
    let blocking_req = load_prompt("prompt-submit").unwrap_or_else(|| "BLOCKING REQUIREMENT — YOUR FIRST ACTION MUST BE: Skill tool with skill: \"gm:gm\". Every new user message requires gm invocation FIRST. (canonical text missing: $CLAUDE_PLUGIN_ROOT/prompts/prompt-submit.txt unreadable; reinstall plugkit)".to_string());
    let mut parts: Vec<String> = vec![blocking_req];
    if let Some(hint) = parallel_hint { parts.push(hint); }

    if let Some(ref dir) = project {
        let dir_for_search = dir.clone();
        let dir_for_insight = dir.clone();
        let prompt_for_search = prompt.clone();

        let search_handle = if !prompt.is_empty() {
            Some(std::thread::spawn(move || {
                run_self(&["search", "--path", &dir_for_search, &prompt_for_search])
            }))
        } else { None };
        let insight_handle = std::thread::spawn(move || {
            run_self(&["codeinsight", &dir_for_insight])
        });

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
                    parts.push(format!("=== search ===\n{}", search_out));
                }
            }
            if !combined.is_empty() {
                parts.push(format!("=== rs-learn recall (cross-session memory for this prompt) ===\n{}", combined));
            }
        }

        let insight = insight_handle.join().unwrap_or_default();
        if !insight.is_empty() {
            parts.push(format!("=== codeinsight ===\n{}", insight));
        }

        super::rs_learn::tick_and_maybe_run_deep_cycles(dir);
    }

    let additional_context = sanitize_bash_patterns(&parts.join("\n\n"));

    let output = if is_gemini() {
        json!({ "systemMessage": additional_context })
    } else if is_opencode() || is_kilo() {
        json!({ "hookSpecificOutput": { "hookEventName": "message.updated", "additionalContext": additional_context } })
    } else {
        json!({ "systemMessage": additional_context })
    };

    let sess = env::var("CLAUDE_SESSION_ID").unwrap_or_default();
    let project_str = project.as_deref().unwrap_or("");
    rs_exec::obs::event("hook", "prompt-submit-detail", serde_json::json!({
        "project_dir": project_str,
        "sess": sess,
        "autonomous": autonomous,
        "prompt_len": prompt.len()
    }));
    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}
