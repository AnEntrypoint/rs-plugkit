use super::{allow, allow_with_noop, deny, project_dir, run_self};
use serde_json::Value;
use std::io::Read;

pub fn run() {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: Value = serde_json::from_str(&stdin).unwrap_or_default();
    let tool_name = data["tool_name"].as_str().unwrap_or("");
    let tool_input = &data["tool_input"];
    let result = dispatch(tool_name, tool_input);
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
}

fn dispatch(tool_name: &str, tool_input: &Value) -> Value {
    if tool_name.is_empty() { return allow(None); }

    const FORBIDDEN: &[&str] = &["find", "Find", "Glob", "Grep"];
    if FORBIDDEN.contains(&tool_name) {
        return deny("Use the code-search skill for codebase exploration instead of Grep/Glob/find. Describe what you need in plain language — it understands intent, not just patterns.");
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
        if is_test_file(&base, fp) {
            return deny("Test files forbidden on disk. Use Bash tool with real services for all testing.");
        }
    }

    const SEARCH_TOOLS: &[&str] = &["glob", "search_file_content", "Search", "search"];
    if SEARCH_TOOLS.contains(&tool_name) { return allow(None); }

    if tool_name == "Task" && tool_input["subagent_type"].as_str().unwrap_or("") == "Explore" {
        return deny("Use the code-search skill for codebase exploration. Describe what you need in plain language.");
    }

    if tool_name == "EnterPlanMode" {
        return deny("Plan mode is disabled. Use the gm skill (PLAN→EXECUTE→EMIT→VERIFY→COMPLETE state machine) instead.");
    }

    if tool_name == "Skill" {
        let skill = tool_input["skill"].as_str().unwrap_or("").to_lowercase();
        let skill = skill.trim_start_matches("gm:");
        if skill == "explore" || skill == "search" {
            return deny("Use the code-search skill for codebase exploration. Describe what you need in plain language — it understands intent, not just patterns.");
        }
    }

    if tool_name == "Bash" {
        return handle_bash(tool_input);
    }

    const ALLOWED: &[&str] = &["browser", "Skill", "code-search", "electron", "TaskOutput", "ReadMcpResourceTool", "ListMcpResourcesTool"];
    if ALLOWED.contains(&tool_name) { return allow(None); }

    allow(None)
}

fn handle_bash(tool_input: &Value) -> Value {
    let command = tool_input["command"].as_str().unwrap_or("").trim().to_string();
    let cwd = tool_input["cwd"].as_str();

    if let Some(ab_code) = command.strip_prefix("browser:\n") {
        return handle_exec("browser", ab_code, cwd);
    }

    if let Some(rest) = command.strip_prefix("exec") {
        if let Some(nl) = rest.find('\n') {
            let lang_part = &rest[..nl];
            let code = &rest[nl + 1..];
            let raw_lang = lang_part.trim_start_matches(':').trim().to_lowercase();

            if (raw_lang == "bash" || raw_lang == "sh") && code.trim_start().starts_with("playwriter ") {
                return deny("Do not call playwriter via exec:bash. Use exec:browser:\n\nexec:browser\nawait page.goto('https://example.com')");
            }

            return handle_exec(&raw_lang, code, cwd);
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

fn handle_exec(raw_lang: &str, code: &str, cwd: Option<&str>) -> Value {
    const BUILTINS: &[&str] = &["js","javascript","ts","typescript","node","nodejs","py","python","sh","bash","shell","zsh","powershell","ps1","go","rust","c","cpp","java","deno","cmd","browser","codesearch","search","status","sleep","close","runner","type"];

    if !raw_lang.is_empty() && !BUILTINS.contains(&raw_lang) {
        if let Some(result) = try_lang_plugin(raw_lang, code, cwd) {
            return result;
        }
    }

    let lang = normalize_lang(raw_lang, code);
    let safe_code = decode_b64(code);

    match lang.as_str() {
        "codesearch" | "search" => {
            let mut args = vec!["search".to_string()];
            if let Some(c) = cwd { args.push("--path".to_string()); args.push(c.to_string()); }
            args.push(safe_code.trim().to_string());
            let r = run_self(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
            return allow_with_noop(&format!("exec:{} output:\n\n{}", lang, if r.is_empty() { "(no results)" } else { &r }));
        }
        "status" => {
            let r = run_self(&["status", safe_code.trim()]);
            return allow_with_noop(&format!("exec:status output:\n\n{}", r));
        }
        "sleep" => {
            let parts: Vec<&str> = safe_code.trim().split_whitespace().collect();
            let mut args = vec!["sleep"];
            args.extend_from_slice(&parts);
            let r = run_self(&args);
            return allow_with_noop(&format!("exec:sleep output:\n\n{}", r));
        }
        "close" => {
            let r = run_self(&["close", safe_code.trim()]);
            return allow_with_noop(&format!("exec:close output:\n\n{}", r));
        }
        "runner" => {
            let r = run_self(&["runner", safe_code.trim()]);
            return allow_with_noop(&format!("exec:runner output:\n\n{}", r));
        }
        "type" => {
            let mut lines = safe_code.splitn(2, '\n');
            let task_id = lines.next().unwrap_or("").trim();
            let input = lines.next().unwrap_or("").trim();
            let r = run_self(&["type", task_id, input]);
            return allow_with_noop(&format!("exec:type output:\n\n{}", r));
        }
        _ => {}
    }

    let r = run_with_file(&lang, &safe_code, cwd);
    allow_with_noop(&format!("exec:{} output:\n\n{}", lang, r))
}

fn run_with_file(lang: &str, code: &str, cwd: Option<&str>) -> String {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let ext = lang_ext(lang);
    let tmp = std::env::temp_dir().join(format!("plugkit-exec-{}.{}", ts, ext));
    let _ = std::fs::write(&tmp, code);
    let tmp_str = tmp.to_string_lossy();
    let mut args = vec!["exec".to_string(), format!("--lang={}", lang), format!("--file={}", tmp_str)];
    if let Some(c) = cwd { args.push(format!("--cwd={}", c)); }
    let r = run_self(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let _ = std::fs::remove_file(&tmp);
    strip_footer(&r).to_string()
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
    let runner = format!(
        "const plugin = require({});\nPromise.resolve(plugin.exec.run({}, {})).then(out => process.stdout.write(String(out||''))).catch(e=>{{process.stderr.write(e.message||String(e));process.exit(1)}});",
        serde_json::to_string(&plugin_file.to_string_lossy().to_string()).unwrap_or_default(),
        serde_json::to_string(code).unwrap_or_default(),
        serde_json::to_string(cwd.unwrap_or(&project)).unwrap_or_default()
    );
    let mut child = std::process::Command::new("bun").args(["-e", &runner])
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().ok()?;
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(allow_with_noop(&format!("exec:{} error:\n\nlang plugin timed out after 15s", lang)));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return Some(allow_with_noop(&format!("exec:{} error:\n\nlang plugin failed", lang))),
        }
    }
    let r = child.wait_with_output().ok()?;
    let out = String::from_utf8_lossy(&r.stdout).trim_end().to_string();
    let err = String::from_utf8_lossy(&r.stderr).trim_end().to_string();
    if r.status.success() {
        Some(allow_with_noop(&format!("exec:{} output:\n\n{}", lang, if out.is_empty() { "(no output)" } else { &out })))
    } else {
        Some(allow_with_noop(&format!("exec:{} error:\n\n{}", lang, if err.is_empty() { "exec failed" } else { &err })))
    }
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

fn strip_footer(s: &str) -> &str {
    if let Some(idx) = s.find("\n[Running tools]") { s[..idx].trim_end() } else { s.trim_end() }
}

const BASH_DENY_MSG: &str = "Bash is restricted to exec:<lang>, browser:, and git.\n\nexec:<lang> syntax:\n  exec:nodejs / exec:python / exec:bash / exec:typescript\n  exec:go / exec:rust / exec:java / exec:c / exec:cpp\n  exec:codesearch\n  exec:status / exec:sleep / exec:close / exec:runner / exec:type\n\nAll other Bash commands are blocked.";
