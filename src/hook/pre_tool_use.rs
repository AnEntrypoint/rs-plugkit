use super::{allow, deny, plugkit_bin, project_dir, to_unix_path};
use serde_json::Value;
use std::io::Read;

pub fn run() {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: Value = serde_json::from_str(&stdin).unwrap_or_default();
    let tool_name = data["tool_name"].as_str().unwrap_or("");
    let tool_input = &data["tool_input"];
    let session_id = data["session_id"].as_str().unwrap_or("");
    let result = dispatch(tool_name, tool_input, session_id);
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
}

fn dispatch(tool_name: &str, tool_input: &Value, session_id: &str) -> Value {
    if tool_name.is_empty() { return allow(None); }

    const FORBIDDEN: &[&str] = &["find", "Find", "Glob", "Grep"];
    if FORBIDDEN.contains(&tool_name) {
        return deny("Glob/Grep/Find are blocked. Use exec:codesearch with the mandatory two-word start protocol:\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Iterate until found (min 4 attempts). See code-search skill for full protocol.");
    }

    const WRITE_TOOLS: &[&str] = &["Write", "write_file"];
    if WRITE_TOOLS.contains(&tool_name) {
        let fp = tool_input["file_path"].as_str().unwrap_or("");
        let base = std::path::Path::new(fp).file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
        let ext = std::path::Path::new(fp).extension().and_then(|e| e.to_str()).unwrap_or("");
        let in_skills = fp.contains("/skills/") || fp.contains("\\skills\\");
        if (ext == "md" || ext == "txt" || base.starts_with("features_list")) && !base.starts_with("claude") && !base.starts_with("readme") && !in_skills {
            return deny("Cannot create documentation files. Only CLAUDE.md and readme.md are maintained. For task-specific notes, use .prd. For permanent reference material, add to CLAUDE.md.");
        }
        if !in_skills && is_test_file(&base, fp) {
            return deny("Test files forbidden on disk. Use Bash tool with real services for all testing.");
        }
    }

    const SEARCH_TOOLS: &[&str] = &["glob", "search_file_content", "Search", "search"];
    if SEARCH_TOOLS.contains(&tool_name) { return allow(None); }

    if (tool_name == "Task" || tool_name == "Agent") && tool_input["subagent_type"].as_str().unwrap_or("") == "Explore" {
        return deny("The Explore agent is blocked. Use exec:codesearch with the mandatory two-word start protocol:\n\n  exec:codesearch\n  <two words>\n\nNo results → change one word. Still no results → add a third word. Iterate until found (min 4 attempts). See code-search skill for full protocol.");
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
        if let Some(nl) = rest.find('\n') {
            let lang_part = &rest[..nl];
            let code = &rest[nl + 1..];
            let raw_lang = lang_part.trim_start_matches(':').trim().to_lowercase();

            if (raw_lang == "bash" || raw_lang == "sh") && code.trim_start().starts_with("playwriter ") {
                return deny("Do not call playwriter via exec:bash. Use exec:browser:\n\nexec:browser\nawait page.goto('https://example.com')");
            }

            if raw_lang == "bash" || raw_lang == "sh" {
                if let Some(banned) = bash_banned_tool(code) {
                    return deny(&format!(
                        "`{}` is blocked in exec:bash. Use exec:codesearch instead:\n\n  exec:codesearch\n  <natural language description of what to find>\n\nExample:\n  exec:codesearch\n  find all database query functions",
                        banned
                    ));
                }
            }

            return handle_exec(&raw_lang, code, cwd, session_id);
        }
    }

    if command.contains("bun") && (command.contains("gm-exec") || command.contains("plugkit") || command.contains("codebasesearch")) {
        let pkg = command.split_whitespace().nth(2).unwrap_or("plugkit");
        return deny(&format!("Do not call {} directly. Use exec:<lang> syntax instead.\n\nexec:nodejs\nconsole.log(\"hello\")\n\nexec:codesearch\nfind all database queries", pkg));
    }

    if !command.starts_with("exec") && !command.starts_with("browser:") && !command.starts_with("git ") && !command.contains("claude") {
        return deny(BASH_DENY_MSG);
    }

    allow(None)
}

fn bash_banned_tool(code: &str) -> Option<&'static str> {
    const BANNED: &[&str] = &["grep ", "grep\t", " grep\n", "\ngrep\n", "rg ", "rg\t", " find ", "\nfind ", "find\t", "glob "];
    for pat in BANNED {
        if code.contains(pat) { return Some(pat.trim()); }
    }
    for line in code.lines() {
        let t = line.trim();
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

fn handle_exec(raw_lang: &str, code: &str, cwd: Option<&str>, session_id: &str) -> Value {
    const BUILTINS: &[&str] = &["js","javascript","ts","typescript","node","nodejs","py","python","sh","bash","shell","zsh","powershell","ps1","go","rust","c","cpp","java","deno","cmd","browser","codesearch","search","status","sleep","close","runner","type","kill-port"];

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
            if let Some(c) = cwd { cmd.push_str(&format!(" --path {}", to_unix_path(c))); }
            let query = safe_code.trim().replace('\n', " ");
            cmd.push_str(&format!(" {}", query));
            return delegate_to_bash(&cmd);
        }
        "status" => return delegate_to_bash(&format!("{} status {}", bin_unix, safe_code.trim())),
        "sleep" => return delegate_to_bash(&format!("{} sleep {}", bin_unix, safe_code.trim())),
        "close" => return delegate_to_bash(&format!("{} close {}", bin_unix, safe_code.trim())),
        "runner" => return delegate_to_bash(&format!("{} runner {}", bin_unix, safe_code.trim())),
        "kill-port" => return delegate_to_bash(&format!("{} kill-port {}", bin_unix, safe_code.trim())),
        "type" => {
            let mut lines = safe_code.splitn(2, '\n');
            let task_id = lines.next().unwrap_or("").trim();
            let input = lines.next().unwrap_or("").trim();
            return delegate_to_bash(&format!("{} type {} {}", bin_unix, task_id, input));
        }
        _ => {}
    }

    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let ext = lang_ext(&lang);
    let tmp = std::env::temp_dir().join(format!("plugkit-exec-{}.{}", ts, ext));
    let _ = std::fs::write(&tmp, &safe_code);
    let tmp_unix = to_unix_path(&tmp.to_string_lossy());
    let mut cmd = format!("{} exec --lang={} --file={}", bin_unix, lang, tmp_unix);
    if let Some(c) = cwd { cmd.push_str(&format!(" --cwd={}", to_unix_path(c))); }
    if !session_id.is_empty() { cmd.push_str(&format!(" --session={}", session_id)); }
    delegate_to_bash_with_reminder(&cmd)
}


fn open_sessions_reminder() -> String {
    let port_file = std::env::temp_dir().join("glootie-runner.port");
    let port: u16 = match std::fs::read_to_string(&port_file).ok().and_then(|s| s.trim().parse().ok()) {
        Some(p) => p,
        None => return String::new(),
    };
    let tasks = match rs_exec::rpc_client::rpc_call_sync(port, "listTasks", serde_json::json!({}), 2000) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let open: Vec<String> = tasks["tasks"].as_array().unwrap_or(&vec![]).iter()
        .filter(|t| matches!(t["status"].as_str(), Some("running") | Some("pending")))
        .map(|t| format!("  task_{} ({})", t["id"].as_u64().unwrap_or(0), t["status"].as_str().unwrap_or("?")))
        .collect();
    if open.is_empty() { return String::new(); }
    format!("\n[OPEN BACKGROUND TASKS — monitor these, do not lose track]\n{}\n", open.join("\n"))
}

fn delegate_to_bash_with_reminder(cmd: &str) -> Value {
    let reminder = open_sessions_reminder();
    if reminder.is_empty() {
        return delegate_to_bash(cmd);
    }
    let escaped = reminder.replace('\'', "'\\''");
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


const BASH_DENY_MSG: &str = "Bash tool only accepts these exact formats:\n\n1. Code execution — first line is exec:<lang>, rest is the code:\n   exec:nodejs\n   console.log('hello')\n\n   exec:python\n   print('hello')\n\n   exec:bash\n   echo hello\n\n   Languages: nodejs, python, bash, typescript, go, rust, c, cpp, java\n\n2. Browser automation — first line is exec:browser, rest is JS against `page`:\n   exec:browser\n   await page.goto('https://example.com')\n   console.log(await page.title())\n\n3. Utility commands — exec:<cmd> with args on next line:\n   exec:codesearch        (natural language codebase search)\n   exec:status            (check background task status)\n   exec:sleep             (sleep N seconds)\n   exec:close             (close background task)\n   exec:runner            (start/stop/status the runner daemon)\n   exec:type              (send stdin: task_id on line 1, input on line 2)\n   exec:kill-port         (kill process listening on port: port number on next line)\n\n4. Git commands — git <args> directly (no exec: prefix needed):\n   git status\n   git commit -m \"msg\"\n\nAnything else is blocked.";
