use std::process::{Command, Stdio};
use super::{allow_with_noop, tools_dir};

const GLOBAL_FLAGS: &[&str] = &[
    "--headed","--headless","--session","--session-name","--cdp",
    "--auto-connect","--profile","--allow-file-access","--color-scheme",
    "-p","--platform","--device",
];
const FLAGS_WITH_VALUE: &[&str] = &[
    "--session","--session-name","--cdp","--profile","--color-scheme",
    "-p","--platform","--device",
];
const AB_CMDS: &[&str] = &[
    "open","goto","navigate","close","quit","exit","back","forward","reload",
    "click","dblclick","type","fill","press","check","uncheck","select","drag",
    "upload","hover","focus","scroll","scrollintoview","wait","screenshot","pdf",
    "snapshot","get","is","find","eval","connect","tab","frame","dialog","state",
    "session","network","cookies","storage","set","trace","profiler","record",
    "console","errors","highlight","inspect","diff","keyboard","mouse","install",
    "upgrade","confirm","deny","auth","device","window",
];

fn parse_global_flags(tokens: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut globals: Vec<String> = vec![];
    let mut rest: Vec<String> = vec![];
    let mut i = 0;
    while i < tokens.len() {
        if GLOBAL_FLAGS.contains(&tokens[i]) {
            globals.push(tokens[i].to_string());
            if FLAGS_WITH_VALUE.contains(&tokens[i]) && i + 1 < tokens.len() && !tokens[i+1].starts_with("--") {
                i += 1;
                globals.push(tokens[i].to_string());
            }
            i += 1;
        } else {
            rest.extend(tokens[i..].iter().map(|s| s.to_string()));
            break;
        }
    }
    (globals, rest)
}

fn session_name(globals: &[String]) -> String {
    let mut i = 0;
    while i < globals.len() {
        if globals[i] == "--session" || globals[i] == "--session-name" {
            if i + 1 < globals.len() { return globals[i+1].clone(); }
        }
        i += 1;
    }
    "default".to_string()
}

fn sessions_path() -> std::path::PathBuf {
    std::env::temp_dir().join("gm-ab-sessions.json")
}

pub fn read_sessions() -> std::collections::HashMap<String, String> {
    let path = sessions_path();
    if !path.exists() { return std::collections::HashMap::new(); }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_sessions(sessions: &std::collections::HashMap<String, String>) {
    let _ = std::fs::write(sessions_path(), serde_json::to_string(sessions).unwrap_or_default());
}

pub fn close_all_sessions() {
    let sessions = read_sessions();
    if sessions.is_empty() { return; }
    let bin = find_ab_bin();
    let names: Vec<String> = sessions.keys().cloned().collect();
    for name in &names {
        if name == "default" {
            spawn_ab(&bin, &["close"], None, false);
        } else {
            spawn_ab(&bin, &["--session", name.as_str(), "close"], None, false);
        }
    }
    write_sessions(&std::collections::HashMap::new());
}

pub fn dispatch(code: &str) -> serde_json::Value {
    let bin = find_ab_bin();
    let lines: Vec<&str> = code.trim().split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return allow_with_noop("agent-browser output:\n\n(no commands)");
    }

    let first_tokens: Vec<&str> = lines[0].split_whitespace().collect();
    let (batch_globals, _) = parse_global_flags(&first_tokens);
    let headed = batch_globals.contains(&"--headed".to_string());

    let first_cmd = first_tokens.iter().find(|t| !t.starts_with("--")).map(|s| s.to_lowercase()).unwrap_or_default();
    let is_cmd = AB_CMDS.contains(&first_cmd.as_str());

    let mut sessions = read_sessions();

    let result = if lines.len() == 1 && is_cmd {
        let args_str: Vec<String> = lines[0].split_whitespace().map(|s| s.to_string()).collect();
        let sname = session_name(&batch_globals);
        update_session(&mut sessions, &args_str, &sname);
        write_sessions(&sessions);
        let args_ref: Vec<&str> = args_str.iter().map(|s| s.as_str()).collect();
        spawn_ab(&bin, &args_ref, None, headed)
    } else if is_cmd {
        let mut results = vec![];
        for line in &lines {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let (line_globals, line_rest) = parse_global_flags(&tokens);
            let merged = merge_globals(&batch_globals, &line_globals);
            let w = line_rest.first().map(|s| s.to_lowercase()).unwrap_or_default();
            let is_headed = merged.contains(&"--headed".to_string());
            let sname = session_name(&merged);
            if AB_CMDS.contains(&w.as_str()) {
                let mut args_str: Vec<String> = merged.clone();
                args_str.extend(line_rest);
                update_session(&mut sessions, &args_str, &sname);
                let args_ref: Vec<&str> = args_str.iter().map(|s| s.as_str()).collect();
                results.push(spawn_ab(&bin, &args_ref, None, is_headed));
            } else {
                let mut args_str: Vec<String> = merged.clone();
                args_str.push("eval".to_string());
                args_str.push("--stdin".to_string());
                let args_ref: Vec<&str> = args_str.iter().map(|s| s.as_str()).collect();
                results.push(spawn_ab(&bin, &args_ref, Some(*line), is_headed));
            }
        }
        write_sessions(&sessions);
        results.join("\n")
    } else {
        spawn_ab(&bin, &["eval", "--stdin"], Some(code), false)
    };

    allow_with_noop(&format!("agent-browser output:\n\n{}", if result.is_empty() { "(no output)" } else { &result }))
}

fn merge_globals(batch: &[String], line: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = batch.iter().filter(|f| {
        let is_valued = FLAGS_WITH_VALUE.contains(&f.as_str());
        if is_valued {
            !line.iter().any(|g| FLAGS_WITH_VALUE.contains(&g.as_str()))
        } else {
            !line.contains(f)
        }
    }).cloned().collect();
    merged.extend(line.iter().cloned());
    merged
}

fn update_session(sessions: &mut std::collections::HashMap<String, String>, args: &[String], sname: &str) {
    let cmd = args.iter().find(|t| !t.starts_with("--")).map(|s| s.to_lowercase()).unwrap_or_default();
    if ["open","goto","navigate"].contains(&cmd.as_str()) {
        let url = args.iter().skip_while(|t| t.starts_with("--") || ["open","goto","navigate"].contains(&t.to_lowercase().as_str())).next().cloned().unwrap_or_else(|| "?".to_string());
        sessions.insert(sname.to_string(), url);
    } else if ["close","quit","exit"].contains(&cmd.as_str()) {
        sessions.remove(sname);
    }
}

fn find_ab_bin() -> String {
    let dir = tools_dir();
    let ab_dir = dir.join("node_modules").join("agent-browser").join("bin");
    let (os_name, arch_name) = platform_names();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let candidate = ab_dir.join(format!("agent-browser-{}-{}{}", os_name, arch_name, ext));
    if candidate.exists() { return candidate.to_string_lossy().to_string(); }
    let local_bin = dir.join("node_modules").join(".bin").join(format!("agent-browser{}", ext));
    if local_bin.exists() { return local_bin.to_string_lossy().to_string(); }
    "agent-browser".to_string()
}

fn platform_names() -> (&'static str, &'static str) {
    let os = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    (os, arch)
}

fn spawn_ab(bin: &str, args: &[&str], stdin_data: Option<&str>, headed: bool) -> String {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if stdin_data.is_some() { cmd.stdin(Stdio::piped()); }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    #[cfg(windows)]
    if !headed {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("[spawn error: {}]", e),
    };
    if let (Some(data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(data.as_bytes());
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return format!("[wait error: {}]", e),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    let stderr = strip_footer(&stderr);
    if !stdout.is_empty() && !stderr.is_empty() {
        format!("{}\n[stderr]\n{}", stdout, stderr)
    } else {
        strip_footer(if stdout.is_empty() { &stderr } else { &stdout }).to_string()
    }
}

fn strip_footer(s: &str) -> &str {
    if let Some(idx) = s.find("\n[Running tools]") { s[..idx].trim_end() } else { s.trim_end() }
}
