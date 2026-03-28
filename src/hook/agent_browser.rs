use std::{fs, path::PathBuf, process::{Command, Stdio}};
use super::{allow_with_noop, tools_dir};

pub fn dispatch(code: &str) -> serde_json::Value {
    let bin = find_ab_bin();
    let lines: Vec<&str> = code.trim().split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return allow_with_noop("agent-browser output:\n\n(no commands)");
    }

    let ab_cmds = ["open","goto","navigate","close","quit","exit","back","forward","reload",
        "click","dblclick","type","fill","press","check","uncheck","select","drag","upload",
        "hover","focus","scroll","scrollintoview","wait","screenshot","pdf","snapshot","get",
        "is","find","eval","connect","tab","frame","dialog","state","session","network",
        "cookies","storage","set","trace","profiler","record","console","errors","highlight",
        "inspect","diff","keyboard","mouse","install","upgrade","confirm","deny","auth",
        "device","window"];

    let first_word = lines[0].split_whitespace()
        .find(|t| !t.starts_with("--"))
        .unwrap_or("")
        .to_lowercase();

    let is_cmd = ab_cmds.contains(&first_word.as_str());

    let result = if lines.len() == 1 && is_cmd {
        let args: Vec<&str> = lines[0].split_whitespace().collect();
        spawn_ab(&bin, &args, None)
    } else if is_cmd {
        let mut results = vec![];
        for line in &lines {
            let args: Vec<&str> = line.split_whitespace().collect();
            let w = args.iter().find(|t| !t.starts_with("--")).map(|s| s.to_lowercase()).unwrap_or_default();
            if ab_cmds.contains(&w.as_str()) {
                results.push(spawn_ab(&bin, &args, None));
            } else {
                results.push(spawn_ab(&bin, &["eval", "--stdin"], Some(line)));
            }
        }
        results.join("\n")
    } else {
        spawn_ab(&bin, &["eval", "--stdin"], Some(code))
    };

    allow_with_noop(&format!("agent-browser output:\n\n{}", if result.is_empty() { "(no output)" } else { &result }))
}

fn find_ab_bin() -> String {
    let dir = tools_dir();
    let ab_dir = dir.join("node_modules").join("agent-browser").join("bin");
    let (os_name, arch_name) = platform_names();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let candidate = ab_dir.join(format!("agent-browser-{}-{}{}", os_name, arch_name, ext));
    if candidate.exists() {
        return candidate.to_string_lossy().to_string();
    }
    let local_bin = dir.join("node_modules").join(".bin").join(format!("agent-browser{}", ext));
    if local_bin.exists() {
        return local_bin.to_string_lossy().to_string();
    }
    "agent-browser".to_string()
}

fn platform_names() -> (&'static str, &'static str) {
    let os = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    (os, arch)
}

fn spawn_ab(bin: &str, args: &[&str], stdin_data: Option<&str>) -> String {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("[spawn error: {}]", e),
    };

    if let (Some(stdin_data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(stdin_data.as_bytes());
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
    if let Some(idx) = s.find("\n[Running tools]") {
        s[..idx].trim_end()
    } else {
        s.trim_end()
    }
}
