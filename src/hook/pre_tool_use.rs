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
    rs_exec::obs::event("hook", "pre-tool-use.tool", serde_json::json!({ "tool_name": tool_name }));
    let tool_input = if data["tool_input"].is_object() || data["tool_input"].is_array() {
        &data["tool_input"]
    } else {
        &data["tool_use"]["input"]
    };
    let session_id = data["session_id"].as_str().unwrap_or("");

    if let Some(early) = needs_gm_and_skill_tracking(tool_name, tool_input) {
        println!("{}", serde_json::to_string(&early).unwrap_or_default());
        return;
    }

    let result = dispatch(tool_name, tool_input, session_id);
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
}

fn needs_gm_and_skill_tracking(tool_name: &str, tool_input: &Value) -> Option<Value> {
    let project = match project_dir() {
        Some(p) if !p.is_empty() => p,
        _ => return None,
    };
    let gm_dir = std::path::Path::new(&project).join(".gm");
    let needs_gm = gm_dir.join("needs-gm");
    let lastskill = gm_dir.join("lastskill");
    let prd = gm_dir.join("prd.yml");
    let autonomous = prd.exists();

    let skill_name = tool_input["skill"].as_str()
        .or_else(|| tool_input["name"].as_str())
        .unwrap_or("");
    let is_skill = matches!(tool_name, "Skill" | "skill");

    if is_skill && !skill_name.is_empty() {
        let _ = std::fs::create_dir_all(&gm_dir);
        let _ = std::fs::write(&lastskill, skill_name);
        if skill_name == "gm" || skill_name == "gm:gm" {
            let _ = std::fs::remove_file(&needs_gm);
        }
        return Some(allow(None));
    }

    if autonomous {
        let _ = std::fs::remove_file(&needs_gm);
    }

    if needs_gm.exists() {
        return Some(deny("HARD CONSTRAINT: invoke the Skill tool with skill: \"gm:gm\" before any other tool. The gm:gm skill must be the first action after every user message."));
    }

    let no_memo = gm_dir.join("no-memorize-this-turn");
    if !no_memo.exists() {
        let ts_path = gm_dir.join("turn-state.json");
        if let Some(counter) = read_counter(&ts_path) {
            if counter >= 3 {
                let is_mem_agent = tool_name == "Agent"
                    && tool_input.to_string().to_lowercase().contains("memorize");
                if !is_mem_agent {
                    return Some(deny("3+ exec results have resolved unknowns without a memorize call. HARD BLOCK until you spawn at least one Agent(subagent_type='gm:memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<fact>') OR write file .gm/no-memorize-this-turn (containing reason) to declare nothing memorable. Saying \"I will memorize\" is NOT a memorize call \u{2014} only the Agent tool counts."));
                }
            }
        }
    }

    let last_skill = std::fs::read_to_string(&lastskill).map(|s| s.trim().to_string()).unwrap_or_default();
    let is_file_edit = matches!(tool_name, "Write" | "Edit" | "NotebookEdit");
    let write_blocked = matches!(last_skill.as_str(), "gm-complete" | "update-docs" | "gm:gm-complete" | "gm:update-docs");
    if is_file_edit && write_blocked {
        return Some(deny(&format!(
            "File edits are not permitted in {} phase. Regress to gm-execute if changes are needed, or invoke gm-emit to re-emit.",
            last_skill
        )));
    }

    None
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
        const DOC_ALLOWLIST: &[&str] = &["claude.md", "readme.md", "agents.md", "contributing.md", "changelog.md", "license", "license.md"];
        let is_allowed_doc = DOC_ALLOWLIST.iter().any(|a| base == *a);
        if (ext == "md" || ext == "txt" || base.starts_with("features_list")) && !is_allowed_doc && !in_skills {
            return deny("Cannot create documentation files. Allowed: CLAUDE.md, readme.md, AGENTS.md, CONTRIBUTING.md, CHANGELOG.md, LICENSE. For task-specific notes use .prd. For permanent reference add to CLAUDE.md or AGENTS.md.");
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
            return deny(&format!("exec:{} requires args on the next line, not same-line. Use:\n\n  exec:{}\n  {}\n\nAll utility verbs (runner, type, kill-port, codesearch, recall, memorize, forget, wait, pause, sleep, status, close) take their argument on line 2.", verb, verb, args));
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
                        "`{}` is blocked. Use exec:codesearch for ALL codebase lookups — it handles exact strings, symbols, regex patterns, file-name fragments, and PDF pages.\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Minimum 4 attempts before concluding absent. See code-search skill for full protocol.\n\nGrep, Glob, Find, and rg/grep/find-in-bash are ALL blocked. There is no exception path — codesearch is the replacement for every exact-match / regex / file-name-pattern need. For a known absolute path, use the Read tool.",
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

            let normalized = if raw_lang == "bash" || raw_lang == "sh" { normalize_windows_paths(code) } else { code.to_string() };
            return handle_exec(&raw_lang, &normalized, cwd, session_id);
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
            return deny(&format!("Do not call {} directly. Use exec:<lang> syntax instead.\n\nexec:nodejs\nconsole.log(\"hello\")\n\nexec:codesearch\nfind all database queries", target));
        }
    }

    if !command.starts_with("exec")
        && !command.starts_with("browser:")
        && !command.starts_with("git ")
        && !command.starts_with("gh ")
        && !command.starts_with("rtk ")
        && !command.contains("claude")
    {
        let bash_deny = load_prompt("bash-deny").unwrap_or_else(|| BASH_DENY_MSG.to_string());
        return deny(&bash_deny);
    }

    if cfg!(windows) && (command.contains(" && ") || command.contains(" || ") || command.contains(" ; ")) {
        let escaped = command.replace('\'', "'\\''");
        return serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "updatedInput": { "command": format!("bash -lc '{}'", escaped) }
            }
        });
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
                return Some(cmd);
            }
        }
    }
    None
}

fn looks_like_benchmark(code: &str) -> bool {
    let lc = code.to_lowercase();
    let signals = [
        "date +%s", "/usr/bin/time", "performance.now", "process.hrtime",
        "benchmark", "perf check", "how slow", "why is ", "is it slow",
    ];
    let mut hits = 0;
    for s in &signals {
        if lc.contains(s) { hits += 1; }
        if hits >= 1 { return true; }
    }
    false
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
        "nodejs","python","typescript","go","rust","c","cpp","java","cmd","powershell","deno","bash","sh",
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

fn normalize_windows_paths(code: &str) -> String {
    if !cfg!(windows) { return code.to_string(); }
    let mut out = String::with_capacity(code.len());
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if (b.is_ascii_uppercase() || b.is_ascii_lowercase())
            && i + 2 < bytes.len()
            && bytes[i+1] == b':'
            && (bytes[i+2] == b'\\' || bytes[i+2] == b'/')
        {
            let prev = if i > 0 { bytes[i-1] } else { b' ' };
            let path_boundary = matches!(prev, b' ' | b'"' | b'\'' | b'(' | b'`' | b'\n' | b'=' | b':');
            if path_boundary || i == 0 {
                out.push('/');
                out.push((b as char).to_ascii_lowercase());
                out.push('/');
                i += 3;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c == b'\\' { out.push('/'); i += 1; }
                    else if c == b' ' || c == b'"' || c == b'\'' || c == b'\n' || c == b'\r' || c == b')' || c == b'`' { break; }
                    else { out.push(c as char); i += 1; }
                }
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
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
    const BUILTINS: &[&str] = &["js","javascript","ts","typescript","node","nodejs","py","python","sh","bash","shell","zsh","powershell","ps1","go","rust","c","cpp","java","deno","cmd","browser","codesearch","search","runner","type","kill-port","recall","memorize","forget","feedback","learn-status","learn:status","learn-debug","learn:debug","learn-build","learn:build","wait","pause","sleep","status","close"];

    let effective_cwd = cwd.map(|c| c.to_string()).or_else(|| project_dir()).unwrap_or_default();
    let resolved_session = if !session_id.is_empty() {
        session_id.to_string()
    } else {
        std::env::var("CLAUDE_SESSION_ID").unwrap_or_else(|_| format!("pid-{}", std::process::id()))
    };
    let compound_key = if !resolved_session.is_empty() && !effective_cwd.is_empty() {
        format!("{}|{}", resolved_session, effective_cwd)
    } else {
        resolved_session.clone()
    };

    if !raw_lang.is_empty() && !BUILTINS.contains(&raw_lang) {
        if let Some(result) = try_lang_plugin(raw_lang, code, cwd) {
            return result;
        }
    }

    let lang = normalize_lang(raw_lang, code);
    let safe_code = decode_b64(code);

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
        _ => {}
    }

    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let ext = lang_ext(&lang);
    let tmp = std::env::temp_dir().join(format!("plugkit-exec-{}.{}", ts, ext));
    let _ = std::fs::write(&tmp, &safe_code);
    let tmp_unix = to_unix_path(&tmp.to_string_lossy());
    let mut cmd = format!("{} exec --lang={} --file={}", bin_unix, lang, tmp_unix);
    if let Some(c) = cwd { cmd.push_str(&format!(" --cwd={}", shell_quote(&to_unix_path(c)))); }
    if !compound_key.is_empty() { cmd.push_str(&format!(" --session={}", shell_quote(&compound_key))); }
    delegate_with_drain(&cmd, &compound_key)
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
    let log = std::env::temp_dir().join("plugkit-hook-debug.log");
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let entry = format!("[{}] len={} nl={} cmd={:?}\n", ts, cmd.len(), cmd.matches('\n').count(), cmd);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&log) {
        let _ = f.write_all(entry.as_bytes());
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": { "command": cmd }
        }
    })
}


fn find_lang_plugin(lang: &str) -> Option<std::path::PathBuf> {
    let filename = format!("{}.js", lang);
    if let Some(project) = project_dir() {
        let candidate = std::path::Path::new(&project).join("lang").join(&filename);
        if candidate.exists() { return Some(candidate); }
    }
    if let Ok(plugin_root) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        let candidate = std::path::Path::new(&plugin_root).join("lang").join(&filename);
        if candidate.exists() { return Some(candidate); }
    }
    None
}

fn try_lang_plugin(lang: &str, code: &str, cwd: Option<&str>) -> Option<Value> {
    let plugin_file = find_lang_plugin(lang)?;
    let project = project_dir().unwrap_or_default();
    let project = if project.is_empty() { ".".to_string() } else { project };
    let plugin_path = serde_json::to_string(&plugin_file.to_string_lossy().to_string()).unwrap_or_default();
    let code_json = serde_json::to_string(code).unwrap_or_default();
    let cwd_json = serde_json::to_string(cwd.unwrap_or(&project)).unwrap_or_default();
    let runner = format!(
        "const plugin = require({});\nPromise.resolve(plugin.exec.run({}, {})).then(out => process.stdout.write(String(out||''))).catch(e=>{{process.stderr.write(e.message||String(e));process.exit(1)}});",
        plugin_path, code_json, cwd_json
    );
    let escaped = runner.replace('\'', "'\\''");
    Some(delegate_to_bash(&format!("bun -e '{}'", escaped)))
}

fn normalize_lang(raw: &str, code: &str) -> String {
    match raw {
        "js" | "javascript" | "node" | "nodejs" | "" => {
            if raw.is_empty() { detect_lang(code).to_string() } else { "nodejs".to_string() }
        }
        "ts" | "typescript" => "typescript".to_string(),
        "py" | "python" => "python".to_string(),
        "sh" | "shell" | "bash" | "zsh" => "bash".to_string(),
        "powershell" | "ps1" => "powershell".to_string(),
        "browser" => "browser".to_string(),
        "codesearch" | "search" => "codesearch".to_string(),
        other => other.to_string(),
    }
}

fn detect_lang(src: &str) -> &'static str {
    if src.contains("import ") || src.contains("console.") || src.contains("process.") { return "nodejs"; }
    if src.contains("def ") || src.contains("print(") || src.contains("import ") { return "python"; }
    "nodejs"
}

fn decode_b64(s: &str) -> String {
    let t = s.trim();
    if t.len() < 16 || t.len() % 4 != 0 { return s.to_string(); }
    if t.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '\r' || c == '\n') {
        if let Ok(decoded) = data_encoding_decode(t) {
            if !decoded.chars().any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t') {
                return decoded;
            }
        }
    }
    s.to_string()
}

fn data_encoding_decode(s: &str) -> Result<String, ()> {
    let cleaned: String = s.chars().filter(|&c| c != '\r' && c != '\n').collect();
    let bytes = base64_decode(&cleaned).ok_or(())?;
    String::from_utf8(bytes).map_err(|_| ())
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let table: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let lookup: Vec<Option<u8>> = (0u8..=255).map(|b| table.iter().position(|&t| t == b).map(|i| i as u8)).collect();
    let chars: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = vec![];
    for chunk in chars.chunks(4) {
        let v: Option<Vec<u8>> = chunk.iter().map(|&b| lookup[b as usize]).collect();
        let v = v?;
        match v.len() {
            4 => { out.push(v[0]<<2|v[1]>>4); out.push(v[1]<<4|v[2]>>2); out.push(v[2]<<6|v[3]); }
            3 => { out.push(v[0]<<2|v[1]>>4); out.push(v[1]<<4|v[2]>>2); }
            2 => { out.push(v[0]<<2|v[1]>>4); }
            _ => {}
        }
    }
    Some(out)
}

fn lang_ext(lang: &str) -> &str {
    match lang {
        "nodejs" | "typescript" => "mjs",
        "python" => "py",
        "bash" => "sh",
        "powershell" => "ps1",
        "cmd" => "bat",
        "go" => "go",
        "rust" => "rs",
        "c" => "c",
        "cpp" => "cpp",
        "java" => "java",
        _ => lang,
    }
}

fn is_test_file(base: &str, fp: &str) -> bool {
    (base.ends_with(".test.js") || base.ends_with(".spec.js") || base.ends_with(".test.ts") || base.ends_with(".spec.ts"))
        || fp.contains("/__tests__/") || fp.contains("/test/") || fp.contains("/tests/")
        || fp.contains("/fixtures/") || fp.contains("/__mocks__/")
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


#[cfg(windows)]
const BASH_DENY_MSG: &str = "Bash tool only accepts these exact formats:\n\n1. Code execution — first line is exec:<lang>, rest is the code:\n   exec:nodejs\n   console.log('hello')\n\n   exec:python\n   print('hello')\n\n   exec:cmd        (PREFERRED on Windows — runs via cmd.exe)\n   echo hello\n\n   exec:bash       (only if you have a real bash; flaky on Windows)\n   echo hello\n\n   Languages: nodejs, python, cmd, powershell, bash, typescript, go, rust, c, cpp, java\n\n2. Browser automation — first line is exec:browser, rest is JS against `page`:\n   exec:browser\n   await page.goto('https://example.com')\n   console.log(await page.title())\n\n3. Utility commands — exec:<cmd> with args on next line:\n   exec:codesearch        (natural language codebase search)\n   exec:runner            (start/stop/status the runner daemon)\n   exec:type              (send stdin: task_id on line 1, input on line 2)\n   exec:kill-port         (kill process listening on port: port number on next line)\n   exec:wait              (raw timer: <seconds> on next line, max 3600)\n   exec:sleep             (wait for task output: <task_id> on next line)\n   exec:pause             (rename .gm/prd.yml↔prd.paused.yml; <question> on next line)\n   exec:status / exec:close\n   exec:recall / exec:memorize / exec:forget\n\n4. Git commands — git <args> directly (no exec: prefix needed):\n   git status\n   git commit -m \"msg\"\n\nAnything else is blocked.\n\nNotes on Windows shell environment:\n- Default to `exec:cmd` on Windows. cmd.exe is always present and avoids the msys/git-bash quirks that `exec:bash` inherits (path translation, missing builtins, heredoc parsing). Only use `exec:bash` when you genuinely need POSIX shell features.\n- Raw `sleep N` is blocked — use exec:wait <seconds> instead.\n- Inside `exec:cmd`: use `cd /d C:\\path` for drive-aware cd, `set VAR=value` for env, `&&` for chaining. No backtick escapes — `^` is the cmd escape.\n- If you do use `exec:bash`: builtins `time`, `pushd`, `popd`, `source` are NOT available. For timing use `START=$(date +%s%3N); ...; END=$(date +%s%3N); echo \"$((END-START))ms\"`. Prefer /c/Users/foo over C:\\\\Users\\\\foo inside heredocs.";

#[cfg(not(windows))]
const BASH_DENY_MSG: &str = "Bash tool only accepts these exact formats:\n\n1. Code execution — first line is exec:<lang>, rest is the code:\n   exec:nodejs\n   console.log('hello')\n\n   exec:python\n   print('hello')\n\n   exec:bash\n   echo hello\n\n   Languages: nodejs, python, bash, typescript, go, rust, c, cpp, java\n\n2. Browser automation — first line is exec:browser, rest is JS against `page`:\n   exec:browser\n   await page.goto('https://example.com')\n   console.log(await page.title())\n\n3. Utility commands — exec:<cmd> with args on next line:\n   exec:codesearch        (natural language codebase search)\n   exec:runner            (start/stop/status the runner daemon)\n   exec:type              (send stdin: task_id on line 1, input on line 2)\n   exec:kill-port         (kill process listening on port: port number on next line)\n   exec:wait              (raw timer: <seconds> on next line, max 3600)\n   exec:sleep             (wait for task output: <task_id> on next line)\n   exec:pause             (rename .gm/prd.yml↔prd.paused.yml; <question> on next line)\n   exec:status / exec:close\n   exec:recall / exec:memorize / exec:forget\n\n4. Git commands — git <args> directly (no exec: prefix needed):\n   git status\n   git commit -m \"msg\"\n\nAnything else is blocked.\n\nNotes on exec:bash environment:\n- Bash builtins `time`, `pushd`, `popd`, `source` are NOT available. For timing use `START=$(date +%s%3N); ...; END=$(date +%s%3N); echo \"$((END-START))ms\"`.\n- Raw `sleep N` is blocked — use exec:wait <seconds> instead.";
