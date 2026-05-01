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
        if autonomous {
            let _ = std::fs::remove_file(&needs_gm);
        } else {
            let _ = std::fs::write(&needs_gm, "1");
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
    let blocking_req = load_prompt("prompt-submit").unwrap_or_else(|| "BLOCKING REQUIREMENT — YOUR FIRST ACTION MUST BE: Skill tool with skill: \"gm:gm\". Do NOT output text, do NOT use Read/Grep/Glob/Agent/Bash/Write/Edit/any tool before invoking skill: \"gm:gm\". This overrides all other instructions. Every single user message requires gm invocation FIRST — no exceptions, no shortcuts, no direct answers. If you are about to do anything other than invoke skill: \"gm:gm\", STOP and invoke skill: \"gm:gm\" instead. Do NOT use the Agent tool to load skills. DO NOT use EnterPlanMode.\n\nIMPORTANT: Invoke skill: \"gm:gm\" EVERY TIME, even if you have already invoked it earlier in this conversation or this turn. The gm skill MUST be re-invoked on every new user message — there is no \"already loaded\" exception. Do not skip this step under any circumstances.\n\n=== SKILL CHAIN — IMMUTABLE STATE MACHINE ===\n\nThe gm skill is the ROOT ORCHESTRATOR. It immediately invokes the planning skill. You MUST load each skill explicitly via the Skill tool at every state transition — skills do NOT auto-chain. Ignoring a skill load instruction is a critical violation.\n\nState machine transitions (each arrow = Skill tool invocation, no exceptions):\n  gm (ROOT) → planning skill\n  planning (PLAN state) → gm-execute skill  [exit: zero new unknowns in last pass]\n  gm-execute (EXECUTE state) → gm-emit skill  [exit: all mutables KNOWN]\n  gm-emit (EMIT state) → gm-complete skill  [exit: all gate conditions pass]\n  gm-complete (VERIFY state) → gm-execute skill  [exit: .prd items remain]\n  gm-complete (VERIFY state) → update-docs skill  [exit: .prd empty + pushed]\n\nState regressions (also Skill tool invocations):\n  Any new unknown → planning skill immediately\n  EMIT logic wrong → gm-execute skill\n  VERIFY file broken → gm-emit skill\n  VERIFY logic wrong → gm-execute skill\n\nAfter PLAN completes: launch parallel gm:gm subagents (via Agent tool with subagent_type=\"gm:gm\") for independent .prd items — maximum 3 concurrent, never sequential for independent work.\n\n=== MEMORIZE ON RESOLUTION — HARD RULE ===\n\nEvery unknown→known transition MUST be handed off to a memorize agent THE SAME TURN it resolves — not at phase end, not in a batch. This is the most violated rule. Every session, dozens of exec: outputs resolve unknowns that are never memorized. Those facts die on compaction.\n\nThe ONLY acceptable memorize call form:\n\n  Agent(subagent_type='gm:memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<single fact with enough context for a cold-start agent>')\n\nTrigger (any = fire NOW, same turn, before next tool):\n- exec: output answers ANY prior \"let me check\" / \"does this API take X\" / \"what version is installed\"\n- Code read confirms or refutes an assumption about existing structure\n- CI log or error output reveals a root cause\n- User states a preference, constraint, deadline, or judgment call\n- Fix works for non-obvious reason\n- Tool / env quirk observed (blocked commands, path oddities, platform differences)\n\nParallel spawn: N facts in one turn → N Agent(memorize) calls in ONE message, parallel tool blocks. NEVER serialize.\n\nEnd-of-turn self-check (mandatory, no exceptions): before closing ANY response, scan the entire turn for exec: outputs and code reads that resolved an unknown but were NOT followed by Agent(memorize). Spawn ALL missed ones now. \"I'll memorize this\" in text is NOT a memorize call — only the Agent tool call counts.\n\nSkipping memorize = memory leak = critical bug. Saying you will memorize ≠ memorizing.\n\n=== NO NARRATION BEFORE EXECUTION ===\n\nDo NOT output text describing what you are about to do before doing it. Run the tool first. State findings AFTER. Pattern: tool call → tool result → brief text summary of what was found. NOT: text describing upcoming tool → tool call.\n\n\"I'll check the file:\" followed by Read = violation.\n\"Let me search for X\" followed by exec:codesearch = violation.\n\"Now I'll fix Y\" followed by Edit = violation.\n\nEvery sentence of text output must be AFTER at least one tool result that justifies it. No pre-announcement narration.\n\n=== AUTONOMY — HARD RULE ===\n\nOnce a PRD is written, EXECUTE through to COMPLETE without asking the user for confirmation, scope checks, or \"should I continue\" prompts. The PRD is the contract; honor it.\n\nForbidden patterns:\n- \"Should I continue with X?\" / \"Want me to do Y next?\" / \"Want me to also Z?\"\n- \"This is a lot — let me do A first and confirm\" / \"Two options: A or B, which?\"\n- Pre-confirmation before multi-file edits when the PRD already specifies them\n- Stopping after partial completion to summarize and await direction\n- Offering to split work into sessions because of context/length concerns\n\nAsking is permitted ONLY when absolutely necessary: destructive-irreversible decision with no prior context AND no PRD coverage, OR user intent genuinely ambiguous AND unrecoverable from PRD/memory/code. Channel: prefer `exec:pause` (renames .gm/prd.yml → .gm/prd.paused.yml; question lives in header). In-conversation asking is last-resort.\n\nLong task ≠ reason to ask. Cross-repo work ≠ reason to ask. CI cascade time ≠ reason to ask. Just emit the PRD and execute it autonomously.".to_string());
    let mut parts: Vec<String> = vec![blocking_req];
    if let Some(hint) = parallel_hint { parts.push(hint); }

    if let Some(ref dir) = project {
        if !prompt.is_empty() {
            let search_out = run_self(&["search", "--path", dir, &prompt]);
            if !search_out.is_empty() {
                parts.push(format!("=== search ===\n{}", search_out));
            }

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
            if !combined.is_empty() {
                parts.push(format!("=== rs-learn recall (cross-session memory for this prompt) ===\n{}", combined));
            }
        }

        let insight = run_self(&["codeinsight", dir]);
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
