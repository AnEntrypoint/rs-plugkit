use super::{allow, deny, load_prompt, plugkit_bin, project_dir, to_unix_path};
use serde_json::Value;
use std::io::Read;

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
    let session_id = data["session_id"].as_str().unwrap_or("");

    if let Some(early) = needs_gm_and_skill_tracking(tool_name, tool_input) {
        let is_deny = early["hookSpecificOutput"]["permissionDecision"].as_str() == Some("deny");
        let reason = if is_deny {
            early["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or("").chars().take(120).collect::<String>()
        } else {
            String::new()
        };
        let autonomous = project_dir()
            .map(|d| std::path::Path::new(&d).join(".gm").join("prd.yml").exists())
            .unwrap_or(false);
        let cmd_preview: String = if tool_name == "Bash" {
            tool_input["command"].as_str().unwrap_or("")
                .chars().take(120).collect::<String>()
                .replace('\n', " ⏎ ")
        } else {
            String::new()
        };
        rs_exec::obs::event("hook", "pre-tool-use.tool", serde_json::json!({
            "tool_name": tool_name,
            "outcome": if is_deny { "deny" } else { "allow" },
            "reason_preview": reason,
            "command_preview": cmd_preview,
            "autonomous": autonomous,
            "stage": "early",
        }));
        println!("{}", serde_json::to_string(&sanitize_for_host(early)).unwrap_or_default());
        return;
    }

    let result = dispatch(tool_name, tool_input, session_id);
    let is_deny = result["hookSpecificOutput"]["permissionDecision"].as_str() == Some("deny");
    let reason = if is_deny {
        result["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or("").chars().take(120).collect::<String>()
    } else {
        String::new()
    };
    let autonomous = project_dir()
        .map(|d| std::path::Path::new(&d).join(".gm").join("prd.yml").exists())
        .unwrap_or(false);
    let cmd_preview: String = if tool_name == "Bash" {
        tool_input["command"].as_str().unwrap_or("")
            .chars().take(120).collect::<String>()
            .replace('\n', " ⏎ ")
    } else {
        String::new()
    };
    rs_exec::obs::event("hook", "pre-tool-use.tool", serde_json::json!({
        "tool_name": tool_name,
        "outcome": if is_deny { "deny" } else { "allow" },
        "reason_preview": reason,
        "command_preview": cmd_preview,
        "autonomous": autonomous,
        "stage": "dispatch",
    }));
    println!("{}", serde_json::to_string(&sanitize_for_host(result)).unwrap_or_default());
}

fn sanitize_for_host(mut v: Value) -> Value {
    let Some(hso) = v.get_mut("hookSpecificOutput") else { return v };
    let decision = hso.get("permissionDecision").and_then(|d| d.as_str()).unwrap_or("");
    if decision != "allow" {
        return v;
    }
    if let Some(obj) = hso.as_object_mut() {
        obj.remove("permissionDecision");
        obj.remove("permissionDecisionReason");
    }
    v
}

fn needs_gm_and_skill_tracking(tool_name: &str, tool_input: &Value) -> Option<Value> {
    let project = match project_dir() {
        Some(p) if !p.is_empty() => p,
        _ => return None,
    };
    let gm_dir = std::path::Path::new(&project).join(".gm");
    let needs_gm = gm_dir.join("needs-gm");
    let global_needs_gm = super::tools_dir().join("needs-gm");
    let lastskill = gm_dir.join("lastskill");
    let prd = gm_dir.join("prd.yml");
    let autonomous = prd.exists();

    let skill_name = tool_input["skill"].as_str()
        .or_else(|| tool_input["name"].as_str())
        .unwrap_or("");
    let is_skill = matches!(tool_name, "Skill" | "skill");

    let gm_fired_marker = gm_dir.join("gm-fired-this-turn");

    if is_skill && !skill_name.is_empty() {
        let _ = std::fs::create_dir_all(&gm_dir);
        let _ = std::fs::write(&lastskill, skill_name);
        if skill_name == "gm" || skill_name == "gm:gm" {
            let _ = std::fs::remove_file(&needs_gm);
            let _ = std::fs::remove_file(&global_needs_gm);
            let _ = std::fs::write(&gm_fired_marker, "1");
        }
        return Some(allow(None));
    }

    if matches!(tool_name, "Task" | "Agent") {
        let st = tool_input["subagent_type"].as_str().unwrap_or("");
        if st == "gm" || st == "gm:gm" {
            let _ = std::fs::create_dir_all(&gm_dir);
            let _ = std::fs::write(&lastskill, st);
            let _ = std::fs::remove_file(&needs_gm);
            let _ = std::fs::remove_file(&global_needs_gm);
            let _ = std::fs::write(&gm_fired_marker, "1");
            return Some(allow(None));
        }
    }

    if autonomous {
        let _ = std::fs::remove_file(&needs_gm);
        let _ = std::fs::remove_file(&global_needs_gm);
    }

    let is_memorize_bash = tool_name == "Bash" && {
        let cmd = tool_input["command"].as_str().unwrap_or("").trim_start();
        cmd.starts_with("exec:memorize")
    };
    if !is_memorize_bash && (needs_gm.exists() || global_needs_gm.exists()) && !gm_fired_marker.exists() {
        return Some(deny("HARD CONSTRAINT: invoke gm before any other tool. Either Skill(skill=\"gm:gm\") OR Agent(subagent_type=\"gm:gm\") satisfies the gate. Subagent form is preferred — it isolates the orchestration loop in its own context. Must be the first action after every user message."));
    }

    let is_no_memo_write = matches!(tool_name, "Write" | "write_file") && {
        let fp = tool_input["file_path"].as_str().unwrap_or("").replace('\\', "/");
        fp.ends_with(".gm/no-memorize-this-turn")
    };
    let is_mutables_or_prd_write = matches!(tool_name, "Write" | "Edit" | "NotebookEdit" | "write_file") && {
        let fp = tool_input["file_path"].as_str().unwrap_or("").replace('\\', "/");
        fp.ends_with(".gm/mutables.yml") || fp.ends_with(".gm/prd.yml")
    };
    let no_memo = gm_dir.join("no-memorize-this-turn");
    if !no_memo.exists() && !is_no_memo_write && !is_mutables_or_prd_write {
        let ts_path = gm_dir.join("turn-state.json");
        if let Some(counter) = read_counter(&ts_path) {
            if counter >= 10 {
                let is_mem_agent = tool_name == "Agent"
                    && tool_input.to_string().to_lowercase().contains("memorize");
                if !is_mem_agent {
                    return Some(deny("10+ exec results have resolved unknowns without a memorize call. HARD BLOCK until you spawn at least one Agent(subagent_type='gm:memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<fact>') OR write file .gm/no-memorize-this-turn (containing reason) to declare nothing memorable. Saying \"I will memorize\" is NOT a memorize call \u{2014} only the Agent tool counts."));
                }
            }
        }
    }

    let last_skill = std::fs::read_to_string(&lastskill).map(|s| s.trim().to_string()).unwrap_or_default();
    let is_file_edit = matches!(tool_name, "Write" | "Edit" | "NotebookEdit");

    let is_git_mutating_bash = tool_name == "Bash" && {
        let cmd = tool_input["command"].as_str().unwrap_or("");
        cmd.contains("git commit") || cmd.contains("git push") || cmd.contains("git.exe commit") || cmd.contains("git.exe push")
    };
    if (is_file_edit && !is_mutables_or_prd_write) || is_git_mutating_bash {
        let mutables_path = gm_dir.join("mutables.yml");
        if mutables_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&mutables_path) {
                let unresolved_ids = scan_unresolved_mutable_ids(&raw);
                if !unresolved_ids.is_empty() {
                    let id_list = unresolved_ids.join(", ");
                    let target = if is_git_mutating_bash { "git commit/push" } else { tool_name };
                    return Some(deny(&format!(
                        "HARD CONSTRAINT: .gm/mutables.yml has unresolved mutables: [{}]. Cannot {} until every mutable reaches status: witnessed with filled witness_evidence. Regress to gm-execute and resolve each unknown by witness — exec:codesearch, exec:nodejs import, exec:recall, file Read. Each resolution sets status: witnessed and fills witness_evidence with file:line, codesearch hit, or dispatched test output. Empty mutables.yml or status: witnessed for all entries unblocks this gate. Saying \"I will resolve\" is NOT resolution — only an updated mutables.yml counts.",
                        id_list, target
                    )));
                }
            }
        }
    }

    let write_blocked = matches!(last_skill.as_str(), "gm-complete" | "update-docs" | "gm:gm-complete" | "gm:update-docs");
    if is_file_edit && write_blocked {
        return Some(deny(&format!(
            "File edits are not permitted in {} phase. Regress to gm-execute if changes are needed, or invoke gm-emit to re-emit.",
            last_skill
        )));
    }

    None
}

pub fn scan_unresolved_mutable_ids(raw: &str) -> Vec<String> {
    let mut unresolved: Vec<String> = Vec::new();
    let mut current_id: Option<String> = None;
    for line in raw.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- id:") {
            current_id = Some(rest.trim().trim_matches(|c: char| c == '"' || c == '\'').to_string());
            continue;
        }
        if let Some(rest) = t.strip_prefix("id:") {
            if current_id.is_none() {
                current_id = Some(rest.trim().trim_matches(|c: char| c == '"' || c == '\'').to_string());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("status:") {
            let v = rest.trim().trim_matches(|c: char| c == '"' || c == '\'').to_lowercase();
            if v == "unknown" {
                if let Some(ref id) = current_id {
                    if !id.is_empty() {
                        unresolved.push(id.clone());
                    } else {
                        unresolved.push("<unnamed>".to_string());
                    }
                } else {
                    unresolved.push("<unnamed>".to_string());
                }
            }
        }
    }
    unresolved
}

fn read_counter(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v["execCallsSinceMemorize"].as_u64()
}

fn dispatch(tool_name: &str, tool_input: &Value, session_id: &str) -> Value {
    if tool_name.is_empty() { return allow(None); }

    const FORBIDDEN: &[&str] = &["find", "Find", "Glob", "glob", "Grep", "grep", "Search", "search", "search_file_content", "Explore", "explore"];
    if FORBIDDEN.contains(&tool_name) {
        return deny("Glob/Grep/Find/Search/Explore are blocked. exec:codesearch is the ONLY codebase-exploration tool — it handles exact strings, symbols, regex patterns, file-name fragments, and PDF pages. Do not reach for Grep/Glob/Find/Search/Explore for any lookup.\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Minimum 4 attempts before concluding absent. See code-search skill.\n\nFor a KNOWN absolute file path, use the Read tool. For directory listing at a known path, use exec:nodejs + fs.readdirSync.");
    }

    const WRITE_TOOLS: &[&str] = &["Write", "write_file"];
    if WRITE_TOOLS.contains(&tool_name) {
        let fp = tool_input["file_path"].as_str().unwrap_or("");
        let base = std::path::Path::new(fp).file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
        let ext = std::path::Path::new(fp).extension().and_then(|e| e.to_str()).unwrap_or("");
        let in_skills = fp.contains("/skills/") || fp.contains("\\skills\\");
        let already_exists = !fp.is_empty() && std::path::Path::new(fp).exists();
        if (ext == "txt" || base.starts_with("features_list")) && !in_skills && !already_exists {
            return deny("Cannot create new plain-text documentation files. For task-specific notes use .prd. For permanent reference add to CLAUDE.md or AGENTS.md.");
        }
        if !in_skills && is_test_file(&base, fp) {
            return deny("Test files forbidden on disk. Use Bash tool with real services for all testing.");
        }
        if !in_skills && is_smoke_page(&base, fp) {
            return deny("Smoke/test/demo pages forbidden. Per paper II §5.4 the in-page observability surface is `window.__debug` — modules register on mount, deregister on unmount. Creating dedicated smoke.js, smoke-*.js, test.html, demo.html, *-playground.html, sandbox.html is a parallel test runner that fights the discipline. Register the surface in `window.__debug.<moduleName>` and assert via the project-root `test.js` integration test.");
        }
    }

    if tool_name == "Task" || tool_name == "Agent" {
        let st = tool_input["subagent_type"].as_str().unwrap_or("").to_lowercase();
        if st == "explore" || st == "search" || st == "general-purpose" || st.contains("explore") || st.contains("search") {
            return deny("The Explore/Search agent is blocked. Use exec:codesearch with the mandatory two-word start protocol:\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Iterate until found (min 4 attempts). See code-search skill for full protocol.");
        }
    }

    if tool_name == "EnterPlanMode" {
        return deny("Plan mode is disabled. Use the gm skill (PLAN→EXECUTE→EMIT→VERIFY→COMPLETE state machine) instead.");
    }

    if tool_name == "Skill" {
        let skill = tool_input["skill"].as_str().unwrap_or("").to_lowercase();
        let skill = skill.trim_start_matches("gm:");
        if skill == "explore" || skill == "search" {
            return deny("The search/explore skill is blocked. Use exec:codesearch with the mandatory two-word start protocol:\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Iterate until found (min 4 attempts). See code-search skill for full protocol.");
        }
    }

    if tool_name == "Bash" {
        return handle_bash(tool_input, session_id);
    }

    const ALLOWED: &[&str] = &["browser", "Skill", "code-search", "electron", "TaskOutput", "ReadMcpResourceTool", "ListMcpResourcesTool"];
    if ALLOWED.contains(&tool_name) { return allow(None); }

    allow(None)
}

fn handle_bash(tool_input: &Value, session_id: &str) -> Value {
    let command = tool_input["command"].as_str().unwrap_or("").trim().to_string();
    let cwd = tool_input["cwd"].as_str();

    if let Some(ab_code) = command.strip_prefix("browser:\n") {
        return handle_exec("browser", ab_code, cwd, session_id);
    }

    if let Some(rest) = command.strip_prefix("exec") {
        if rest.find('\n').is_none() && rest.starts_with(':') {
            // Single-line exec:<verb> <args> — user forgot to put args on next line.
            // Rewrite to two-line form so the hook path below handles it correctly.
            let inline = &rest[1..];
            let (verb, args) = match inline.find(|c: char| c.is_whitespace()) {
                Some(i) => (&inline[..i], inline[i..].trim()),
                None => (inline, ""),
            };
            let verb = verb.trim().to_lowercase();
            const UTILITIES: &[&str] = &["runner","type","kill-port","codesearch","search","recall","feedback","learn-status","learn:status","learn-debug","learn:debug","learn-build","learn:build","wait","pause","sleep","status","close","memorize","forget"];
            if UTILITIES.contains(&verb.as_str()) {
                return handle_exec(&verb, args, cwd, session_id);
            }
            return deny(&format!("Code execution does not run via the Bash tool. Write the code to the file-spool:\n\n  .gm/exec-spool/in/<lang>/<N>.<ext>     (e.g. in/nodejs/42.js, in/python/43.py, in/bash/44.sh)\n\nThe spool watcher executes it and writes out/<N>.json; result returns as systemMessage.\nLanguages: nodejs, python, bash, typescript, go, rust, c, cpp, java, deno.\n\nUtility verbs DO run via Bash — first line is the verb, query/arg on line 2:\n\n  exec:codesearch     exec:recall         exec:memorize       exec:wait\n  exec:browser        exec:runner         exec:type           exec:kill-port\n  exec:forget         exec:feedback       exec:learn-status   exec:learn-debug\n  exec:learn-build    exec:discipline     exec:pause          exec:sleep\n  exec:status         exec:close\n\nRejected: exec:{} is not a recognized utility verb.", verb));
        }
        if let Some(nl) = rest.find('\n') {
            let lang_part = &rest[..nl];
            let code = &rest[nl + 1..];
            let raw_lang = lang_part.trim_start_matches(':').trim().to_lowercase();

            if (raw_lang == "bash" || raw_lang == "sh") && code.trim_start().starts_with("playwriter ") {
                return deny("Do not call playwriter via exec:bash. Use exec:browser:\n\nexec:browser\nawait page.goto('https://example.com')");
            }

            if raw_lang == "bash" || raw_lang == "sh" {
                if let Some(nested_verb) = bash_body_starts_with_exec_verb(code) {
                    return deny(&format!(
                        "exec:{} cannot be nested inside exec:bash. Call it at the top level instead:\n\n  exec:{}\n  <args on next line>\n\nThe wrapping `exec:bash` block forwards the body to /usr/bin/bash, which sees `exec:{}` as a literal command and fails with `command not found`. All exec:<verb> utility calls (memorize, recall, codesearch, browser, runner, type, kill-port, wait, sleep, status, close, pause, forget, feedback, learn-status, learn-debug, learn-build) must be the first line of the Bash tool input — never inside another exec block.",
                        nested_verb, nested_verb, nested_verb
                    ));
                }
                if let Some(banned) = bash_banned_tool(code) {
                    return deny(&format!(
                        "`{}` is blocked for codebase lookups. Use exec:codesearch — it handles exact strings, symbols, regex patterns, file-name fragments, and PDF pages.\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Minimum 4 attempts before concluding absent.\n\nFor a known absolute file path, use the Read tool.\n\nException: grep/rg/find IS allowed when every targeted path is runtime data (substring on the command line: `gm-log`, `.claude/`, `/tmp/`, `.jsonl`, `.log`, `.ndjson`, `~/.claude`, `.gm/log`). Codebase lookups still go through codesearch.",
                        banned
                    ));
                }
                if bash_has_raw_sleep(code) {
                    return deny("Raw `sleep N` in exec:bash is blocked. Use exec:wait for raw timer waits (max 3600s):\n\n  exec:wait\n  30\n\nFor waiting on a specific background task to produce output, use exec:sleep <task_id>.");
                }
                if looks_like_benchmark(code) {
                    if let Some(ref dir) = project_dir() {
                        if !recall_fired_this_turn(dir) {
                            return deny("This looks like a benchmark/diagnostic. Run `exec:recall <2-6 word query>` first — past sessions may have already diagnosed it. After recall, re-run your benchmark if still needed. Recall is ~200 tokens / 5ms; cheaper than re-investigating.");
                        }
                    }
                }
            }

            return handle_exec(&raw_lang, code, cwd, session_id);
        }
    }

    // Block direct `bun ... <our-tool>` invocations only when bun is the command verb,
    // not when the words appear inside a quoted commit message or path argument.
    {
        let first_word = command.split_whitespace().next().unwrap_or("");
        let second_word = command.split_whitespace().nth(1).unwrap_or("");
        let third_word = command.split_whitespace().nth(2).unwrap_or("");
        let invokes_bun = matches!(first_word, "bun" | "bun.exe" | "bunx" | "npx");
        let target = if first_word == "npx" { second_word } else { third_word };
        const BLOCKED_TARGETS: &[&str] = &["gm-exec", "plugkit", "codebasesearch"];
        if invokes_bun && BLOCKED_TARGETS.iter().any(|t| target == *t) {
            return deny(&format!("Do not call {} directly. For code execution write a file to .gm/exec-spool/in/<lang>/<N>.<ext> (e.g. in/nodejs/42.js, in/python/43.py, in/bash/44.sh); the spool watcher executes it and writes out/<N>.json. For codebase search use exec:codesearch.", target));
        }
    }

    // Allow piping raw code to plugkit/rs-exec for JIT execution.
    // This enables: cat <<EOF | plugkit (raw lines without encapsulation)
    // Only plugkit and git are allowed via bash without exec: syntax.
    let pipes_to_plugkit = command.contains("| plugkit") || command.contains("| plugkit.exe")
        || command.contains("| rs-exec") || command.contains("| rs-exec.exe");
    if pipes_to_plugkit {
        return allow(None);
    }

    // Allow piping to git (e.g., `cat <<EOF | git commit -m "msg"`)
    let pipes_to_git = command.contains("| git ") || command.contains("| \"git ") || command.contains("| git.exe ");
    if pipes_to_git {
        return allow(None);
    }

    if !command.starts_with("exec")
        && !command.starts_with("browser:")
        && !is_git_or_gh(&command)
        && !command.contains("claude")
    {
        let bash_deny = load_prompt("bash-deny").unwrap_or_else(|| BASH_DENY_MSG.to_string());
        return deny(&bash_deny);
    }

    allow(None)
}

fn bash_banned_tool(code: &str) -> Option<&'static str> {
    for line in code.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }
        if t == "grep" { return Some("grep"); }
        if t == "rg" { return Some("rg"); }
        if t == "find" { return Some("find"); }
        if t == "glob" { return Some("glob"); }
        for cmd in &["grep", "rg", "find", "glob"] {
            if t.starts_with(&format!("{} ", cmd)) || t.starts_with(&format!("{}\t", cmd)) {
                if line_targets_runtime_data(t) { continue; }
                return Some(cmd);
            }
        }
    }
    None
}

fn line_targets_runtime_data(line: &str) -> bool {
    const MARKERS: &[&str] = &[
        "gm-log", ".claude/", ".claude\\",
        "/tmp/", "\\tmp\\", "/var/log",
        ".jsonl", ".log", ".ndjson",
        "$HOME/.claude", "~/.claude",
        ".gm/log", ".gm\\log",
    ];
    MARKERS.iter().any(|m| line.contains(m))
}

fn looks_like_benchmark(code: &str) -> bool {
    let lc = code.to_lowercase();
    let strong = ["date +%s", "/usr/bin/time", "performance.now", "process.hrtime"];
    let weak = ["perf check", "how slow", "why is ", "is it slow"];
    let has_strong = strong.iter().any(|s| lc.contains(s));
    if has_strong { return true; }
    let weak_hits = weak.iter().filter(|s| lc.contains(*s)).count();
    weak_hits >= 2
}

fn recall_fired_this_turn(project: &str) -> bool {
    let path = std::path::Path::new(project).join(".gm").join("turn-state.json");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    content.contains("\"recallFiredThisTurn\":true")
}

fn bash_has_raw_sleep(code: &str) -> bool {
    for line in code.lines() {
        let t = line.trim();
        if t == "sleep" { return true; }
        if t.starts_with("sleep ") || t.starts_with("sleep\t") {
            let rest = t[5..].trim();
            if rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn bash_body_starts_with_exec_verb(code: &str) -> Option<String> {
    const VERBS: &[&str] = &[
        "memorize","recall","forget","feedback",
        "codesearch","search",
        "browser","runner","type","kill-port",
        "wait","pause","sleep","status","close",
        "learn-status","learn:status","learn-debug","learn:debug","learn-build","learn:build",
    ];
    for line in code.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') { continue; }
        if let Some(rest) = t.strip_prefix("exec:") {
            let verb = rest.split(|c: char| c.is_whitespace() || c == ';' || c == '|' || c == '&').next().unwrap_or("").trim().to_lowercase();
            if VERBS.iter().any(|v| *v == verb.as_str()) {
                return Some(verb);
            }
        }
        return None;
    }
    None
}

fn is_git_or_gh(cmd: &str) -> bool {
    let first_word = cmd.split_whitespace().next().unwrap_or("");

    // Direct commands: git, gh, git.exe, gh.exe
    if first_word == "git" || first_word == "gh" || first_word == "git.exe" || first_word == "gh.exe" {
        return true;
    }

    // Windows Git for Windows full paths (Git Bash shims, usr/bin, cmd directories)
    // Note: cmd may be quoted, so check full command for git path patterns
    let lower_cmd = cmd.to_lowercase();
    if lower_cmd.contains(r"\git\cmd\git.exe") || lower_cmd.contains(r"\git\usr\bin\git.exe")
        || lower_cmd.contains(r"\git\cmd\gh.exe") || lower_cmd.contains(r"\git\usr\bin\gh.exe")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod bash_banned_tests {
    use super::bash_banned_tool;

    #[test]
    fn blocks_start_of_line_grep() {
        assert_eq!(bash_banned_tool("grep foo file.txt"), Some("grep"));
        assert_eq!(bash_banned_tool("  rg pattern"), Some("rg"));
        assert_eq!(bash_banned_tool("find . -name '*.rs'"), Some("find"));
    }

    #[test]
    fn allows_grep_in_pipe() {
        assert_eq!(bash_banned_tool("echo x | grep y"), None);
        assert_eq!(bash_banned_tool("cat f | rg pattern"), None);
        assert_eq!(bash_banned_tool("ls | grep foo | sort"), None);
    }

    #[test]
    fn allows_grep_as_substring() {
        assert_eq!(bash_banned_tool("mygrep tool"), None);
        assert_eq!(bash_banned_tool("echo grepping"), None);
    }

    #[test]
    fn allows_empty_and_whitespace() {
        assert_eq!(bash_banned_tool(""), None);
        assert_eq!(bash_banned_tool("\n\n"), None);
    }

    #[test]
    fn allows_runtime_log_targets() {
        assert_eq!(bash_banned_tool("grep error ~/.claude/gm-log/2026-05-04/hook.jsonl"), None);
        assert_eq!(bash_banned_tool("grep -iE 'fail|warn' /tmp/output.log"), None);
        assert_eq!(bash_banned_tool("rg panic .gm/log/*.jsonl"), None);
        assert_eq!(bash_banned_tool("find ~/.claude/gm-log -name '*.jsonl'"), None);
    }

    #[test]
    fn still_blocks_codebase_grep_when_no_runtime_marker() {
        assert_eq!(bash_banned_tool("grep foo src/lib.rs"), Some("grep"));
        assert_eq!(bash_banned_tool("rg TODO ./crates"), Some("rg"));
        assert_eq!(bash_banned_tool("find . -name '*.rs'"), Some("find"));
    }
}


#[cfg(test)]
mod nested_exec_verb_tests {
    use super::bash_body_starts_with_exec_verb;

    #[test]
    fn detects_nested_memorize() {
        assert_eq!(bash_body_starts_with_exec_verb("exec:memorize\nfoo/bar\nfact body"), Some("memorize".into()));
    }

    #[test]
    fn detects_nested_recall_browser_codesearch() {
        assert_eq!(bash_body_starts_with_exec_verb("exec:recall\nthebird"), Some("recall".into()));
        assert_eq!(bash_body_starts_with_exec_verb("exec:browser\nawait page.goto('x')"), Some("browser".into()));
        assert_eq!(bash_body_starts_with_exec_verb("exec:codesearch\nfoo bar"), Some("codesearch".into()));
    }

    #[test]
    fn ignores_blank_lines_and_comments_at_top() {
        assert_eq!(bash_body_starts_with_exec_verb("\n\n# comment\nexec:memorize\nfoo"), Some("memorize".into()));
    }

    #[test]
    fn allows_normal_bash() {
        assert_eq!(bash_body_starts_with_exec_verb("ls -la"), None);
        assert_eq!(bash_body_starts_with_exec_verb("echo exec:memorize"), None);
        assert_eq!(bash_body_starts_with_exec_verb("for i in 1 2 3; do echo $i; done"), None);
    }

    #[test]
    fn allows_unknown_verb() {
        assert_eq!(bash_body_starts_with_exec_verb("exec:notaverb\nfoo"), None);
    }

    #[test]
    fn allows_empty() {
        assert_eq!(bash_body_starts_with_exec_verb(""), None);
        assert_eq!(bash_body_starts_with_exec_verb("\n\n"), None);
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn handle_exec(raw_lang: &str, code: &str, cwd: Option<&str>, session_id: &str) -> Value {
    let resolved_session = if !session_id.is_empty() {
        session_id.to_string()
    } else {
        std::env::var("CLAUDE_SESSION_ID").unwrap_or_else(|_| format!("pid-{}", std::process::id()))
    };
    let effective_cwd = cwd.map(|c| c.to_string()).or_else(|| project_dir()).unwrap_or_default();
    let compound_key = if !resolved_session.is_empty() && !effective_cwd.is_empty() {
        format!("{}|{}", resolved_session, effective_cwd)
    } else {
        resolved_session.clone()
    };

    let lang = raw_lang.to_string();
    let safe_code = code.to_string();

    let bin = plugkit_bin();
    let bin_unix = to_unix_path(&bin.to_string_lossy());

    match lang.as_str() {
        "codesearch" | "search" => {
            let mut cmd = format!("{} search", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --path {}", shell_quote(&to_unix_path(c)))); }
            let query = safe_code.trim().replace('\n', " ");
            cmd.push_str(&format!(" {}", shell_quote(&query)));
            return delegate_to_bash(&cmd);
        }
        "runner" => return delegate_with_drain(&format!("{} runner {}", bin_unix, safe_code.trim()), &compound_key),
        "kill-port" => return delegate_with_drain(&format!("{} kill-port {}", bin_unix, safe_code.trim()), &compound_key),
        "recall" => {
            let query = safe_code.trim().replace('\n', " ");
            if query.is_empty() { return deny("exec:recall requires a query.\n\n  exec:recall\n  <2-6 word query>"); }
            let mut cmd = format!("{} recall --limit 5", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            cmd.push_str(&format!(" {}", shell_quote(&query)));
            return delegate_to_bash(&cmd);
        }
        "memorize" => {
            // First line = source tag, rest = fact body. Or whole body if first line doesn't look like a tag.
            let trimmed = safe_code.trim_start();
            let (source, body) = if let Some(nl) = trimmed.find('\n') {
                let first = trimmed[..nl].trim();
                let looks_like_tag = !first.is_empty() && first.len() < 64 && !first.contains(' ');
                if looks_like_tag {
                    (first.to_string(), trimmed[nl+1..].to_string())
                } else {
                    ("memorize".to_string(), trimmed.to_string())
                }
            } else {
                ("memorize".to_string(), trimmed.to_string())
            };
            if body.trim().is_empty() { return deny("exec:memorize requires content.\n\n  exec:memorize\n  <source-tag>\n  <fact body>\n\nOr just:\n\n  exec:memorize\n  <fact body>"); }
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
            let tmp = std::env::temp_dir().join(format!("plugkit-memorize-{}.txt", ts));
            let _ = std::fs::write(&tmp, &body);
            let tmp_unix = to_unix_path(&tmp.to_string_lossy());
            let mut cmd = format!("{} memorize --source {} --file {}", bin_unix, shell_quote(&source), shell_quote(&tmp_unix));
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            return delegate_to_bash(&cmd);
        }
        "forget" => {
            // Body: "by-source <tag>" | "by-query <query>" | "by-id <episode_id>"
            let body = safe_code.trim();
            if body.is_empty() { return deny("exec:forget requires a directive.\n\n  exec:forget\n  by-source <source-tag>\n\n  exec:forget\n  by-query <terms>\n\n  exec:forget\n  by-id <episode_id>"); }
            let mut cmd = format!("{} forget", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            cmd.push_str(&format!(" {}", shell_quote(body)));
            return delegate_to_bash(&cmd);
        }
        "feedback" => {
            // Line 1 = request_id <quality 0..1>, optional line 2 = signal
            let body = safe_code.trim();
            if body.is_empty() {
                return deny("exec:feedback requires <request_id> <quality 0..1> [signal]");
            }
            let mut cmd = format!("{} learn feedback", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            cmd.push_str(&format!(" {}", shell_quote(body)));
            return delegate_to_bash(&cmd);
        }
        "learn-debug" | "learn:debug" => {
            let subsystem = safe_code.trim();
            let mut cmd = format!("{} learn debug", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            if !subsystem.is_empty() { cmd.push_str(&format!(" {}", shell_quote(subsystem))); }
            return delegate_to_bash(&cmd);
        }
        "learn-status" | "learn:status" => {
            let mut cmd = format!("{} learn status", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            return delegate_to_bash(&cmd);
        }
        "learn-build" | "learn:build" => {
            let mut cmd = format!("{} learn build-communities", bin_unix);
            if let Some(c) = cwd { cmd.push_str(&format!(" --cwd {}", shell_quote(&to_unix_path(c)))); }
            return delegate_to_bash(&cmd);
        }
        "type" => {
            let mut lines_iter = safe_code.splitn(2, '\n');
            let task_id = lines_iter.next().unwrap_or("").trim();
            let input = lines_iter.next().unwrap_or("").trim();
            let mut cmd = format!("{} type {} {}", bin_unix, shell_quote(task_id), shell_quote(input));
            if !compound_key.is_empty() { cmd.push_str(&format!(" --session={}", shell_quote(&compound_key))); }
            return delegate_with_drain(&cmd, &compound_key);
        }
        "wait" => {
            let body = safe_code.trim();
            let secs: u64 = body.parse().unwrap_or(0);
            if secs == 0 { return deny("exec:wait requires <seconds> on next line.\n\n  exec:wait\n  30\n\nMax 3600s. For waiting on a background task to produce output, use exec:sleep <task_id>."); }
            let secs = secs.min(3600);
            let cmd = format!("sleep {}", secs);
            return delegate_to_bash(&cmd);
        }
        "pause" => {
            let body = safe_code.trim();
            if body.is_empty() { return deny("exec:pause requires a question/reason on next line. Renames .gm/prd.yml → .gm/prd.paused.yml; question lives in the file's header comment until the user responds.\n\n  exec:pause\n  <question text>"); }
            let project = project_dir().unwrap_or_default();
            if project.is_empty() { return deny("exec:pause requires a project directory."); }
            let gm_dir = std::path::Path::new(&project).join(".gm");
            let _ = std::fs::create_dir_all(&gm_dir);
            let live = gm_dir.join("prd.yml");
            let paused = gm_dir.join("prd.paused.yml");
            let existing = std::fs::read_to_string(&live).unwrap_or_default();
            let header = format!("# PAUSED — awaiting user response\n# Question: {}\n# Saved-at: {}\n\n", body.replace('\n', " "), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
            let combined = format!("{}{}", header, existing);
            let _ = std::fs::write(&paused, combined);
            let _ = std::fs::remove_file(&live);
            let reason = format!("Paused. .gm/prd.yml → .gm/prd.paused.yml. Question recorded:\n\n{}\n\nThe Stop hook will now permit stopping. On the next user message, prompt-submit will rename prd.paused.yml back to prd.yml automatically.", body);
            return serde_json::json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}});
        }
        "sleep" => {
            let body = safe_code.trim();
            if body.is_empty() { return deny("exec:sleep requires a task_id on next line (waits for the task to produce output). For raw timer waits, use exec:wait <seconds> instead.\n\n  exec:sleep\n  <task_id>"); }
            let cmd = format!("{} sleep {}", bin_unix, shell_quote(body));
            return delegate_to_bash(&cmd);
        }
        "status" => {
            let cmd = format!("{} status", bin_unix);
            return delegate_to_bash(&cmd);
        }
        "close" => {
            let cmd = format!("{} close", bin_unix);
            return delegate_to_bash(&cmd);
        }
        _ => {
            return deny(&format!(
                "Code execution does not run via the Bash tool. Write the code to the file-spool:\n\n  .gm/exec-spool/in/<lang>/<N>.<ext>     (e.g. in/nodejs/42.js, in/python/43.py, in/bash/44.sh)\n\nThe spool watcher executes it and writes out/<N>.json; result returns as systemMessage.\nLanguages: nodejs, python, bash, typescript, go, rust, c, cpp, java, deno.\nLang plugins: lang/<name>.js in project dir with exec.run(code,cwd) interface.\n\nUtility verbs DO run via Bash — first line is the verb, query/arg on line 2:\n\n  exec:codesearch     exec:recall         exec:memorize       exec:wait\n  exec:browser        exec:runner         exec:type           exec:kill-port\n  exec:forget         exec:feedback       exec:learn-status   exec:learn-debug\n  exec:learn-build    exec:discipline     exec:pause          exec:sleep\n  exec:status         exec:close\n\nRejected: exec:{} is not a recognized utility verb.",
                lang
            ));
        }
    }

    unreachable!()
}

fn session_log_drain(session_id: &str) -> String {
    if session_id.is_empty() { return String::new(); }
    let port_file = std::env::temp_dir().join("glootie-runner.port");
    let port: u16 = match std::fs::read_to_string(&port_file).ok().and_then(|s| s.trim().parse().ok()) {
        Some(p) => p,
        None => return String::new(),
    };

    let drain = match rs_exec::rpc_client::rpc_call_sync(port, "drainSessionOutput", serde_json::json!({ "sessionId": session_id }), 2000) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let mut out = String::new();

    if let Some(tasks) = drain["tasks"].as_array() {
        for task in tasks {
            let id = task["id"].as_u64().unwrap_or(0);
            let status = task["status"].as_str().unwrap_or("");
            let output = task["output"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
            let text: String = output.iter().map(|e| e["d"].as_str().unwrap_or("")).collect::<Vec<_>>().join("");
            let text = text.trim_end();
            if !matches!(status, "running" | "pending") && !text.is_empty() {
                out.push_str(&format!("\n[task_{} {} — output]\n{}\n", id, status, text));
            }
        }
    }

    out
}

fn delegate_with_drain(cmd: &str, session_id: &str) -> Value {
    if super::is_codex() {
        return serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "additionalContext": "Codex does not accept rewritten tool input here. Write raw code to .gm/exec-spool/in/<lang>/<N>.<ext> (e.g. in/nodejs/42.js); the spool watcher executes it and writes out/<N>.json."
            }
        });
    }
    if cfg!(windows) {
        // Windows Bash tool commands are executed via PowerShell in this stack.
        // Avoid shell chaining forms like `printf ... && ...` that are not portable.
        return delegate_to_bash(cmd);
    }
    let drain = session_log_drain(session_id);
    if drain.is_empty() {
        return delegate_to_bash(cmd);
    }
    let escaped = drain.replace('\'', "'\\''");
    let full = format!("printf '%s' '{}' && {}", escaped, cmd);
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": full }
        }
    })
}

fn delegate_to_bash(cmd: &str) -> Value {
    if super::is_codex() {
        return serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "additionalContext": "Codex does not accept rewritten tool input here. Write raw code to .gm/exec-spool/in/<lang>/<N>.<ext> (e.g. in/nodejs/42.js); the spool watcher executes it and writes out/<N>.json."
            }
        });
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": cmd }
        }
    })
}


fn is_test_file(base: &str, fp: &str) -> bool {
    let fp_norm = fp.replace('\\', "/").to_lowercase();
    let base_lc = base.to_lowercase();
    let test_ext = base_lc.ends_with(".test.js") || base_lc.ends_with(".spec.js")
        || base_lc.ends_with(".test.ts") || base_lc.ends_with(".spec.ts")
        || base_lc.ends_with(".test.mjs") || base_lc.ends_with(".spec.mjs")
        || base_lc.ends_with(".test.cjs") || base_lc.ends_with(".spec.cjs");
    let test_name = (base_lc.contains("test") || base_lc.contains("spec") || base_lc.contains("browser-test"))
        && (base_lc.ends_with(".mjs") || base_lc.ends_with(".cjs"));
    let test_dir = fp_norm.contains("/__tests__/") || fp_norm.contains("/test/") || fp_norm.contains("/tests/")
        || fp_norm.contains("/fixtures/") || fp_norm.contains("/__mocks__/");
    test_ext || test_name || test_dir
}

fn is_smoke_page(base: &str, fp: &str) -> bool {
    let fp_norm = fp.replace('\\', "/").to_lowercase();
    if base == "test.js" || base == "test.mjs" || base == "test.ts" { return false; }
    if base.starts_with("smoke.") || base.starts_with("smoke-") || base.contains("-smoke.") { return true; }
    if base == "demo.html" || base == "test.html" || base == "sandbox.html" { return true; }
    if base.ends_with("-playground.html") || base.starts_with("playground.") { return true; }
    if fp_norm.contains("/docs/smoke") || fp_norm.contains("/docs/demo.") || fp_norm.contains("/docs/test.html") || fp_norm.contains("/docs/sandbox.") || fp_norm.contains("/docs/playground.") { return true; }
    false
}

#[cfg(test)]
mod test_file_tests {
    use super::is_test_file;
    #[test]
    fn blocks_classic_test_extensions() {
        assert!(is_test_file("foo.test.js", "C:/p/foo.test.js"));
        assert!(is_test_file("foo.spec.ts", "C:/p/foo.spec.ts"));
        assert!(is_test_file("bar.test.mjs", "C:/p/bar.test.mjs"));
        assert!(is_test_file("bar.spec.mjs", "C:/p/bar.spec.mjs"));
        assert!(is_test_file("baz.test.cjs", "C:/p/baz.test.cjs"));
    }
    #[test]
    fn blocks_browser_test_mjs() {
        assert!(is_test_file("browser-test.mjs", "C:/p/scripts/browser-test.mjs"));
        assert!(is_test_file("my-test.mjs", "C:/p/scripts/my-test.mjs"));
        assert!(is_test_file("spec-helper.mjs", "C:/p/spec-helper.mjs"));
    }
    #[test]
    fn blocks_test_directories() {
        assert!(is_test_file("foo.js", "C:/p/__tests__/foo.js"));
        assert!(is_test_file("bar.js", "C:/p/test/bar.js"));
        assert!(is_test_file("baz.js", "C:/p/tests/baz.js"));
        assert!(is_test_file("mock.js", "C:/p/__mocks__/mock.js"));
    }
    #[test]
    fn allows_non_test_mjs() {
        assert!(!is_test_file("server.mjs", "C:/p/server.mjs"));
        assert!(!is_test_file("index.mjs", "C:/p/src/index.mjs"));
        assert!(!is_test_file("util.cjs", "C:/p/util.cjs"));
    }
}

#[cfg(test)]
mod mutables_scanner_tests {
    use super::scan_unresolved_mutable_ids;

    #[test]
    fn empty_input_returns_empty() {
        assert!(scan_unresolved_mutable_ids("").is_empty());
        assert!(scan_unresolved_mutable_ids("\n\n").is_empty());
    }

    #[test]
    fn all_witnessed_returns_empty() {
        let y = "- id: foo\n  status: witnessed\n- id: bar\n  status: witnessed\n";
        assert!(scan_unresolved_mutable_ids(y).is_empty());
    }

    #[test]
    fn collects_unknown_ids() {
        let y = "- id: alpha\n  status: unknown\n- id: beta\n  status: witnessed\n- id: gamma\n  status: unknown\n";
        let r = scan_unresolved_mutable_ids(y);
        assert_eq!(r, vec!["alpha".to_string(), "gamma".to_string()]);
    }

    #[test]
    fn handles_quoted_ids() {
        let y = "- id: \"quoted-id\"\n  status: unknown\n";
        assert_eq!(scan_unresolved_mutable_ids(y), vec!["quoted-id".to_string()]);
    }

    #[test]
    fn case_insensitive_status() {
        let y = "- id: alpha\n  status: UNKNOWN\n";
        assert_eq!(scan_unresolved_mutable_ids(y), vec!["alpha".to_string()]);
    }

    #[test]
    fn ignores_other_status_values() {
        let y = "- id: alpha\n  status: pending\n- id: beta\n  status: in_progress\n";
        assert!(scan_unresolved_mutable_ids(y).is_empty());
    }
}

#[cfg(test)]
mod smoke_page_tests {
    use super::is_smoke_page;
    #[test]
    fn blocks_smoke_files() {
        assert!(is_smoke_page("smoke.js", "C:/p/docs/smoke.js"));
        assert!(is_smoke_page("smoke-network.js", "C:/p/docs/smoke-network.js"));
        assert!(is_smoke_page("test.html", "C:/p/docs/test.html"));
        assert!(is_smoke_page("demo.html", "C:/p/docs/demo.html"));
        assert!(is_smoke_page("sandbox.html", "C:/p/docs/sandbox.html"));
        assert!(is_smoke_page("foo-playground.html", "C:/p/docs/foo-playground.html"));
        assert!(is_smoke_page("voice-smoke.js", "C:/p/voice-smoke.js"));
    }
    #[test]
    fn allows_canonical_test_js() {
        assert!(!is_smoke_page("test.js", "C:/p/test.js"));
        assert!(!is_smoke_page("test.ts", "C:/p/test.ts"));
    }
    #[test]
    fn allows_unrelated() {
        assert!(!is_smoke_page("index.html", "C:/p/docs/index.html"));
        assert!(!is_smoke_page("client.js", "C:/p/agentgui/client.js"));
    }
}


const BASH_DENY_MSG: &str = "The Bash tool accepts ONLY git commands directly (no exec: prefix): `git status`, `git commit -m \"msg\"`, `git push`, etc.\n\nEverything else — code execution AND utility verbs — goes through the file-spool. Write a file at:\n\n  .gm/exec-spool/in/<lang-or-verb>/<N>.<ext>\n\nExamples:\n  in/nodejs/42.js              in/python/43.py            in/bash/44.sh\n  in/codesearch/45.txt         in/recall/46.txt           in/memorize/47.md\n  in/wait/48.txt               in/browser/49.js           in/runner/50.txt\n\nLanguages: nodejs, python, bash, typescript, go, rust, c, cpp, java, deno\nVerbs: codesearch, recall, memorize, wait, sleep, status, close, browser, runner, type, kill-port, forget, feedback, learn-status, learn-debug, learn-build, discipline, pause\n\nThe spool watcher executes the request and writes out/<N>.json; result returns as systemMessage on next tool use.\n\nAnything else via Bash is blocked.";
