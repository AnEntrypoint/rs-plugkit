use super::project_dir;
use serde_json::json;
use std::fs;
use std::net::TcpStream;
use std::time::Duration;

pub fn run() {
    rs_exec::obs::event("hook", "session-start.fired", serde_json::json!({
        "hook_name": "session_start",
        "pid": std::process::id(),
        "action": "fired",
    }));
    let project = project_dir();
    ensure_gitignore(project.as_deref());
    ensure_claude_md_pointer(project.as_deref());
    spawn_ensure_tools_detached();
    start_exec_spool();
    spawn_acptoapi_if_needed();
    write_needs_gm_if_gm_project(project.as_deref());

    println!("{}", serde_json::to_string_pretty(&json!({ "systemMessage": "" })).unwrap_or_default());
}

fn spawn_ensure_tools_detached() {
    let plugkit = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => super::plugkit_bin(),
    };
    let mut cmd = super::no_window_cmd(plugkit);
    cmd.arg("ensure-tools");
    cmd.stdin(std::process::Stdio::null())
       .stdout(std::process::Stdio::null())
       .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000 | 0x00000008 | 0x00000200);
    }
    let _ = cmd.spawn();
}

pub fn start_exec_spool() {
    let project = match project_dir() {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };
    let gm = std::path::Path::new(&project).join(".gm");
    let _ = fs::create_dir_all(&gm);
    let spool_dir = gm.join("exec-spool");
    let _ = fs::create_dir_all(&spool_dir);

    let pid_file = spool_dir.join(".watcher.pid");
    let hb_file = spool_dir.join(".watcher.heartbeat");
    let session_file = spool_dir.join(".last-session-start.json");

    if watcher_alive(&pid_file, &hb_file) && !watcher_version_matches(&session_file) {
        kill_watcher(&pid_file);
    }

    if !watcher_alive(&pid_file, &hb_file) {
        let _ = fs::remove_dir_all(spool_dir.join("in"));
        let _ = fs::remove_dir_all(spool_dir.join("out"));
        let _ = fs::remove_dir_all(spool_dir.join("log"));
    }
    let _ = fs::create_dir_all(spool_dir.join("in"));
    let _ = fs::create_dir_all(spool_dir.join("out"));

    if watcher_alive(&pid_file, &hb_file) { return; }

    let _ = fs::remove_file(gm.join("exec-spool.started"));
    let _ = fs::remove_file(&pid_file);

    let plugkit = super::plugkit_bin();
    // On Windows, Rust's Command::spawn (even with DETACHED_PROCESS) keeps the
    // child's stdio handles inheritable from CC's hook-runner console — CC waits
    // for the watcher daemon's stdio to close before considering the hook done.
    // `cmd /c start /B` allocates a visible terminal in console-less parents.
    // Canonical Windows detach: PowerShell `Start-Process -WindowStyle Hidden`
    // which calls CreateProcess with NO_INHERIT_HANDLES and `SW_HIDE`.
    #[cfg(windows)]
    let spawn_result = {
        let plugkit_str = plugkit.to_string_lossy().to_string();
        let spool_dir_str = spool_dir.to_string_lossy().to_string();
        let version_str = env!("CARGO_PKG_VERSION");
        let ps_script = format!(
            "$env:RS_EXEC_SPOOL_DIR='{}'; $env:PLUGKIT_VERSION='{}'; \
             Start-Process -FilePath '{}' -ArgumentList 'spool' \
                -WindowStyle Hidden",
            spool_dir_str.replace('\'', "''"),
            version_str,
            plugkit_str.replace('\'', "''"),
        );
        let mut cmd = super::no_window_cmd("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps_script]);
        cmd.stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
        cmd.spawn()
    };
    #[cfg(not(windows))]
    let spawn_result = {
        let mut cmd = super::no_window_cmd(&plugkit);
        cmd.arg("spool");
        cmd.env("RS_EXEC_SPOOL_DIR", &spool_dir);
        cmd.env("PLUGKIT_VERSION", env!("CARGO_PKG_VERSION"));
        cmd.stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        cmd.spawn()
    };
    let (spawn_ok, spawn_error, watcher_pid) = match &spawn_result {
        Ok(child) => (true, String::new(), Some(child.id())),
        Err(e) => (false, e.to_string(), None),
    };
    if let Ok(child) = spawn_result {
        let _ = fs::write(&pid_file, child.id().to_string());
    }
    let now_iso = iso_now();
    let record = serde_json::json!({
        "ts": now_iso,
        "plugkit_version": env!("CARGO_PKG_VERSION"),
        "watcher_pid_attempted": watcher_pid,
        "spawn_ok": spawn_ok,
        "spawn_error": spawn_error,
    });
    let _ = fs::write(spool_dir.join(".last-session-start.json"), serde_json::to_string_pretty(&record).unwrap_or_default());
}

fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs / 86_400;
    let rem = secs % 86_400;
    let (y, mo, d) = {
        let z = day as i64 + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d_ = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let y = if m <= 2 { y + 1 } else { y };
        (y as i32, m, d_)
    };
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let se = (rem % 60) as u32;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, se)
}

fn watcher_alive(pid_file: &std::path::Path, hb_file: &std::path::Path) -> bool {
    let pid_str = match fs::read_to_string(pid_file) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    if !pid_running(pid) { return false; }
    if let Ok(meta) = fs::metadata(hb_file) {
        if let Ok(mt) = meta.modified() {
            if let Ok(age) = std::time::SystemTime::now().duration_since(mt) {
                return age.as_secs() <= 10;
            }
        }
    }
    false
}

fn watcher_version_matches(session_file: &std::path::Path) -> bool {
    let current = env!("CARGO_PKG_VERSION");
    let Ok(text) = fs::read_to_string(session_file) else { return true; };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return true; };
    match v.get("plugkit_version").and_then(|x| x.as_str()) {
        Some(recorded) => recorded == current,
        None => true,
    }
}

fn kill_watcher(pid_file: &std::path::Path) {
    let Ok(pid_str) = fs::read_to_string(pid_file) else { return };
    let Ok(pid) = pid_str.trim().parse::<u32>() else { return };
    rs_exec::kill::kill_tree(pid);
    let _ = fs::remove_file(pid_file);
}

fn pid_running(pid: u32) -> bool {
    use sysinfo::{System, Pid, ProcessesToUpdate};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.process(Pid::from(pid as usize)).is_some()
}

fn spawn_acptoapi_if_needed() {
    if !is_port_reachable("127.0.0.1", 4800) {
        std::thread::spawn(|| {
            let _ = spawn_acptoapi_daemon();
            std::thread::sleep(Duration::from_millis(500));
            let _ = spawn_acp_agents();
        });
    }
}

fn is_port_reachable(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    TcpStream::connect_timeout(&addr.parse().unwrap_or_else(|_| "127.0.0.1:4800".parse().unwrap()), Duration::from_millis(500)).is_ok()
}

fn spawn_acptoapi_daemon() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut cmd = super::no_window_cmd("bun");
        cmd.args(["x", "acptoapi@latest"]);
        cmd.stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
        let _ = cmd.spawn()?;
    }
    #[cfg(not(windows))]
    {
        let mut cmd = super::no_window_cmd("bun");
        cmd.args(["x", "acptoapi@latest"]);
        cmd.stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        let _ = cmd.spawn()?;
    }
    Ok(())
}

fn spawn_acp_agents() {
    let agents = vec!["opencode", "kilo-code", "codex", "gemini-cli", "qwen-code"];
    for agent in agents {
        if let Ok(bin_path) = find_agent_binary(agent) {
            spawn_acp_agent(&bin_path);
        }
    }
}

fn find_agent_binary(agent: &str) -> std::io::Result<String> {
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("where").arg(agent).output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
    }
    #[cfg(not(windows))]
    {
        use std::process::Command;
        let output = Command::new("which").arg(agent).output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "agent not found"))
}

fn spawn_acp_agent(bin_path: &str) {
    let mut cmd = super::no_window_cmd(bin_path);
    cmd.stdin(std::process::Stdio::piped())
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let _ = cmd.spawn();
}

fn write_needs_gm_if_gm_project(project_dir: Option<&str>) {
    let Some(dir) = project_dir else { return };
    let gm_dir = std::path::Path::new(dir).join(".gm");
    let agents_md = std::path::Path::new(dir).join("AGENTS.md");
    let global_needs_gm = super::tools_dir().join("needs-gm");
    if !gm_dir.exists() && !agents_md.exists() {
        let _ = fs::write(&global_needs_gm, "1");
        return;
    }
    let _ = fs::create_dir_all(&gm_dir);
    let _ = fs::write(gm_dir.join("needs-gm"), "1");
    let _ = fs::write(&global_needs_gm, "1");
}

/// Auto-update gm-tools binaries from the active plugin cache when newer.
///
/// Why: ~/.claude/gm-tools/plugkit.exe is the canonical path used by exec
/// dispatch (so plugkit can keep running across plugin upgrades without
/// being held open by Claude Code). When the plugin updates, the cached
/// binary at <CLAUDE_PLUGIN_ROOT>/bin/plugkit.exe is fresh; the gm-tools
/// copy may be stale or missing. Copy newer-or-missing binaries here.
///
/// Windows quirk: a running plugkit.exe has a write-lock on its on-disk
/// image. We side-step it by writing to <name>.new, then renaming the
/// current one to <name>.old (best-effort, ignored on lock) and renaming
/// .new into place. Old copies accumulate as .old; that's fine — they
/// get cleaned on the next update cycle when not held.
fn bootstrap_cache_dir() -> Option<std::path::PathBuf> {
    let version_file = {
        let plugin_root = std::env::var("CLAUDE_PLUGIN_ROOT").ok()?;
        std::path::Path::new(&plugin_root).join("bin").join("plugkit.version")
    };
    let version = fs::read_to_string(&version_file).ok()?.trim().to_string();
    if version.is_empty() { return None; }
    let cache_root = if cfg!(windows) {
        let base = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| format!("{}\\AppData\\Local", std::env::var("USERPROFILE").unwrap_or_default()));
        std::path::PathBuf::from(base).join("plugkit").join("bin")
    } else if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join("Library").join("Caches").join("plugkit").join("bin")
    } else {
        let xdg = std::env::var("XDG_CACHE_HOME")
            .unwrap_or_else(|_| format!("{}/.cache", std::env::var("HOME").unwrap_or_default()));
        std::path::PathBuf::from(xdg).join("plugkit").join("bin")
    };
    let ver_dir = cache_root.join(format!("v{}", version));
    if ver_dir.join(".ok").exists() { Some(ver_dir) } else { None }
}

pub fn ensure_tools_current() {
    let src_dir = match bootstrap_cache_dir() {
        Some(d) => d,
        None => return,
    };
    let dst_dir = super::tools_dir();
    if let Err(_) = fs::create_dir_all(&dst_dir) { return; }

    let (platform_key, ext) = if cfg!(windows) {
        if cfg!(target_arch = "aarch64") { ("win32-arm64", ".exe") } else { ("win32-x64", ".exe") }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { ("darwin-arm64", "") } else { ("darwin-x64", "") }
    } else {
        if cfg!(target_arch = "aarch64") { ("linux-arm64", "") } else { ("linux-x64", "") }
    };

    let binary_names = [
        (format!("plugkit-{}{}", platform_key, ext), format!("plugkit{}", ext)),
        (format!("rs-exec-{}{}", platform_key, ext), format!("rs-exec{}", ext)),
        (format!("rs-exec-process-{}{}", platform_key, ext), format!("rs-exec-process{}", ext)),
    ];

    for (src_name, dst_name) in &binary_names {
        let src = src_dir.join(src_name);
        if !src.exists() { continue; }
        let dst = dst_dir.join(dst_name);
        if let Some(reason) = should_copy(&src, &dst) {
            let _ = reason;
            copy_with_fallback(&src, &dst);
        }
    }

    // Best-effort cleanup of stale .old.exe leftovers (older than 24h, not held).
    if let Ok(entries) = fs::read_dir(&dst_dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let p = entry.path();
            let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if n.ends_with(".old.exe") || n.ends_with(".old") {
                if let Ok(meta) = fs::metadata(&p) {
                    if let Ok(mt) = meta.modified() {
                        if now.duration_since(mt).map(|d| d.as_secs() > 86400).unwrap_or(false) {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
            }
        }
    }
}

fn should_copy(src: &std::path::Path, dst: &std::path::Path) -> Option<&'static str> {
    if !dst.exists() { return Some("missing"); }
    let src_meta = fs::metadata(src).ok()?;
    let dst_meta = fs::metadata(dst).ok()?;
    if src_meta.len() != dst_meta.len() { return Some("size"); }
    let src_mt = src_meta.modified().ok()?;
    let dst_mt = dst_meta.modified().ok()?;
    if src_mt > dst_mt { return Some("newer"); }
    None
}


fn copy_with_fallback(src: &std::path::Path, dst: &std::path::Path) {
    // Direct overwrite first (works if dst not held).
    if fs::copy(src, dst).is_ok() { return; }
    // Held: write side-by-side, rotate.
    let new_path = dst.with_extension(format!(
        "{}.new",
        dst.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    if fs::copy(src, &new_path).is_err() { return; }
    let old_path = dst.with_extension(format!(
        "old.{}",
        dst.extension().and_then(|s| s.to_str()).unwrap_or("bin")
    ));
    let _ = fs::rename(dst, &old_path);
    let _ = fs::rename(&new_path, dst);
}

/// Manage a marked block of .gitignore rules for gm tooling state.
///
/// Goal: keep persistent assets (rs-learn.db, search index) tracked so a
/// fresh clone of the repo gets the project's accumulated memory and
/// search index for free; ignore only the volatile per-run scratch
/// (counters, drafts, .new/.old binary swaps, lock files).
///
/// Block is idempotent: managed by START/END markers, rewritten in place
/// without touching unrelated user rules. Existing rules outside the
/// block are preserved verbatim.
fn ensure_gitignore(project_dir: Option<&str>) {
    let Some(dir) = project_dir else { return };
    let gitignore = std::path::Path::new(dir).join(".gitignore");
    let content = fs::read_to_string(&gitignore).unwrap_or_default();

    const START: &str = "# >>> gm managed (do not edit between markers)";
    const END:   &str = "# <<< gm managed";
let block = format!(
        "{START}\n\
         .gm-stop-verified\n\
         .gm/prd-state.json\n\
         .gm/rslearn-counter.json\n\
         .gm/git-block-counter.json\n\
         .gm/learning-state.md\n\
         .gm/trajectory-drafts/\n\
         .gm/ingest-drafts/\n\
         .gm/needs-gm\n\
         .gm/lastskill\n\
         .gm/turn-state.json\n\
         .gm/no-memorize-this-turn\n\
         .gm/prd.paused.yml\n\
         .gm/rs-learn.db-shm\n\
         .gm/rs-learn.db-wal\n\
         .gm/exec-spool.in\n\
         .gm/exec-spool.out\n\
         .gm/exec-spool.started\n\
         # tracked: .gm/rs-learn.db, .gm/code-search/, AGENTS.md, .gm/prd.yml\n\
         {END}\n"
    );

    let new_content = if let (Some(s), Some(e)) = (content.find(START), content.find(END)) {
        if e > s {
            let end_idx = e + END.len();
            let after = &content[end_idx..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            format!("{}{}{}", &content[..s], block, after)
        } else {
            // Markers in wrong order — re-append fresh block.
            ensure_trailing_newline(&content) + &block
        }
    } else {
        // No block yet; append (also strip legacy bare ".gm-stop-verified" line if present).
        let stripped: String = content.lines()
            .filter(|l| l.trim() != ".gm-stop-verified")
            .collect::<Vec<_>>()
            .join("\n");
        let base = ensure_trailing_newline(&stripped);
        base + &block
    };

    if new_content != content {
        let _ = fs::write(&gitignore, new_content);
    }
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.is_empty() { return String::new(); }
    if s.ends_with('\n') { s.to_string() } else { format!("{}\n", s) }
}

/// Ensure CLAUDE.md is exactly "@AGENTS.md\n" so the model loads AGENTS.md as
/// the single source of truth. If CLAUDE.md exists with other content, that
/// content is preserved verbatim in .gm/imported-claude-md-<unix-ts>.md so a
/// human can review and merge into AGENTS.md without losing anything; CLAUDE.md
/// itself is then rewritten to the pointer form.
///
/// Skips silently when AGENTS.md does not exist (don't unilaterally redirect
/// to a file that isn't there).
fn ensure_claude_md_pointer(project_dir: Option<&str>) {
    let Some(dir) = project_dir else { return };
    let claude_md = std::path::Path::new(dir).join("CLAUDE.md");
    let agents_md = std::path::Path::new(dir).join("AGENTS.md");
    if !agents_md.exists() { return; }
    const POINTER: &str = "@AGENTS.md\n";
    let existing = fs::read_to_string(&claude_md).unwrap_or_default();
    if existing.trim() == "@AGENTS.md" { return; }
    if !existing.trim().is_empty() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let gm_dir = std::path::Path::new(dir).join(".gm");
        let _ = fs::create_dir_all(&gm_dir);
        let imported = gm_dir.join(format!("imported-claude-md-{}.md", ts));
        let _ = fs::write(&imported, &existing);
        eprintln!(
            "[session-start] CLAUDE.md non-pointer content folded to {} — review and merge into AGENTS.md if needed",
            imported.display()
        );
    }
    let _ = fs::write(&claude_md, POINTER);
}
