// Background self-update: every startup, check pin file, spawn detached
// updater child if newer. Hot path NEVER blocks on this — current binary
// handles the hook; new binary takes over on next invocation.
//
// Critical: only ONE updater runs at a time across all plugkit processes.
// A lockfile in gm-tools/ guards the download; concurrent invocations skip
// silently when a live updater is already in flight.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tools_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    PathBuf::from(home).join(".claude").join("gm-tools")
}

fn platform_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") { "plugkit-win32-arm64.exe" } else { "plugkit-win32-x64.exe" }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { "plugkit-darwin-arm64" } else { "plugkit-darwin-x64" }
    } else {
        if cfg!(target_arch = "aarch64") { "plugkit-linux-arm64" } else { "plugkit-linux-x64" }
    }
}

fn parse_sha_manifest(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        if name.trim_start_matches('*') == asset {
            return Some(hash.to_lowercase());
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000 | 0x00000008 | 0x00000200);
}

#[cfg(not(target_os = "windows"))]
fn no_window(_cmd: &mut Command) {}

fn pid_alive(pid: u32) -> bool {
    use sysinfo::{System, Pid, ProcessesToUpdate};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.process(Pid::from(pid as usize)).is_some()
}

/// At startup: if a freshly-downloaded `.new` binary exists with valid sha,
/// rename it over the current binary. The CURRENT process is about to handle
/// a hook with the old code, but the NEXT invocation gets the new binary.
/// This is the moment when the move can succeed because the old binary is
/// in active use by another process, not necessarily THIS one.
fn try_promote_pending() {
    let tools = tools_dir();
    let target = tools.join(if cfg!(windows) { "plugkit.exe" } else { "plugkit" });
    let manifest_path = tools.join("plugkit.sha256");
    let pinned = match std::fs::read_to_string(tools.join("plugkit.version")) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if pinned.is_empty() { return; }
    let new_path = tools.join(format!("plugkit.{}.new", pinned));
    if !new_path.exists() { return; }
    // Verify the .new matches the pinned sha before promoting.
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let expected = match parse_sha_manifest(&manifest, platform_asset()) {
        Some(h) => h,
        None => return,
    };
    let got = match sha256_of(&new_path) {
        Some(h) => h,
        None => return,
    };
    if got != expected {
        let _ = std::fs::remove_file(&new_path);
        return;
    }
    // Attempt atomic rename; succeeds only when target isn't locked.
    if std::fs::rename(&new_path, &target).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
        }
    }
    // If rename failed (target locked), leave .new in place; another startup
    // will retry. Self-healing loop.
}

pub fn check_and_dispatch() {
    try_promote_pending();
    let pin_path = tools_dir().join("plugkit.version");
    let pinned = match std::fs::read_to_string(&pin_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if pinned.is_empty() { return; }

    // Compare BY SHA, not by --version string. Release tags advance faster than
    // Cargo.toml's CARGO_PKG_VERSION (the publisher auto-bumps the tag without
    // a matching source bump), so a tag-vs-self mismatch is the norm — but if
    // the binary's own sha already matches the manifest entry for the pinned
    // platform, we're already running the pinned binary. Skip update.
    if running_binary_matches_manifest() { return; }

    // Skip if local already cached identical .new matching pinned sha.
    if pending_new_valid(&pinned) { return; }

    // Single-instance: only spawn updater if no live updater is in flight.
    if updater_in_flight() { return; }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    // Detach via PowerShell Start-Process -WindowStyle Hidden on Windows
    // (cmd /c start /B opened a visible terminal when parent has no console).
    #[cfg(windows)]
    {
        let exe_str = exe.to_string_lossy().to_string();
        let pinned_clone = pinned.clone();
        let ps_script = format!(
            "Start-Process -FilePath '{}' -ArgumentList '--self-update','{}' \
                -WindowStyle Hidden",
            exe_str.replace('\'', "''"),
            pinned_clone.replace('\'', "''"),
        );
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps_script]);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new(&exe);
        cmd.arg("--self-update").arg(&pinned);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        let _ = cmd.spawn();
    }
}

fn running_binary_matches_manifest() -> bool {
    let exe = match std::env::current_exe() { Ok(p) => p, Err(_) => return false };
    let manifest = match std::fs::read_to_string(tools_dir().join("plugkit.sha256")) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let asset = platform_asset();
    let expected = match parse_sha_manifest(&manifest, asset) {
        Some(h) => h,
        None => return false,
    };
    match sha256_of(&exe) {
        Some(h) => h == expected,
        None => false,
    }
}

fn pending_new_valid(pinned: &str) -> bool {
    let new_path = tools_dir().join(format!("plugkit.{}.new", pinned));
    if !new_path.exists() { return false; }
    let manifest = match std::fs::read_to_string(tools_dir().join("plugkit.sha256")) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let asset = platform_asset();
    let expected = match parse_sha_manifest(&manifest, asset) {
        Some(h) => h,
        None => return false,
    };
    match sha256_of(&new_path) {
        Some(h) => h == expected,
        None => false,
    }
}

fn updater_lock_path() -> PathBuf {
    tools_dir().join(".update.lock")
}

fn updater_in_flight() -> bool {
    let lock = updater_lock_path();
    let Ok(content) = std::fs::read_to_string(&lock) else { return false };
    let Ok(pid) = content.trim().parse::<u32>() else {
        let _ = std::fs::remove_file(&lock);
        return false;
    };
    if pid_alive(pid) { return true; }
    // Stale lock — old PID dead. Clean up so we don't deadlock.
    let _ = std::fs::remove_file(&lock);
    false
}

fn acquire_lock() -> bool {
    let lock = updater_lock_path();
    let _ = std::fs::create_dir_all(tools_dir());
    // Re-check under lockfile-write race: if another updater raced past
    // check_and_dispatch's updater_in_flight gate, the first writer wins.
    if updater_in_flight() { return false; }
    let pid_str = std::process::id().to_string();
    std::fs::write(&lock, pid_str).is_ok()
}

fn release_lock() {
    let _ = std::fs::remove_file(updater_lock_path());
}

pub fn run_self_update(pinned: &str) {
    if !acquire_lock() { return; }
    let result = std::panic::catch_unwind(|| do_self_update(pinned));
    release_lock();
    let _ = result;
}

fn do_self_update(pinned: &str) {
    let asset = platform_asset();
    let url = format!(
        "https://github.com/AnEntrypoint/plugkit-bin/releases/download/v{}/{}",
        pinned, asset
    );
    let sha_url = format!(
        "https://github.com/AnEntrypoint/plugkit-bin/releases/download/v{}/plugkit.sha256",
        pinned
    );
    let tools = tools_dir();
    let _ = std::fs::create_dir_all(&tools);
    let target_exe = tools.join(if cfg!(windows) { "plugkit.exe" } else { "plugkit" });
    let new_path = tools.join(format!("plugkit.{}.new", pinned));

    // If .new already exists from a prior aborted updater, see if it's valid
    // and just try to promote it without re-downloading.
    if new_path.exists() {
        if let Some(got) = sha256_of(&new_path) {
            if let Ok(manifest) = std::fs::read_to_string(tools.join("plugkit.sha256")) {
                if let Some(expected) = parse_sha_manifest(&manifest, asset) {
                    if got == expected {
                        let _ = std::fs::rename(&new_path, &target_exe);
                        return;
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&new_path);
    }

    let dl_ok = download(&url, &new_path);
    if !dl_ok { let _ = std::fs::remove_file(&new_path); return; }

    let manifest = match download_text(&sha_url) {
        Some(t) => t,
        None => { let _ = std::fs::remove_file(&new_path); return; }
    };
    let expected = match parse_sha_manifest(&manifest, asset) {
        Some(h) => h,
        None => { let _ = std::fs::remove_file(&new_path); return; }
    };
    let got = match sha256_of(&new_path) {
        Some(h) => h,
        None => { let _ = std::fs::remove_file(&new_path); return; }
    };
    if got != expected { let _ = std::fs::remove_file(&new_path); return; }

    let _ = std::fs::write(tools.join("plugkit.sha256"), &manifest);
    let _ = std::fs::write(tools.join("plugkit.version"), pinned);

    // Attempt direct rename first (succeeds if target not locked).
    if std::fs::rename(&new_path, &target_exe).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target_exe, std::fs::Permissions::from_mode(0o755));
        }
        return;
    }
    // Target was locked — leave .new in place. try_promote_pending() runs
    // at every plugkit startup and will perform the rename when the lock
    // is released. Self-healing.
}

fn download(url: &str, dest: &Path) -> bool {
    if cfg!(windows) {
        let mut cmd = Command::new("curl.exe");
        cmd.args(["-fsSL", "--connect-timeout", "10", "--max-time", "120", "-o"]).arg(dest).arg(url);
        no_window(&mut cmd);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.status().map(|s| s.success()).unwrap_or(false)
    } else {
        Command::new("curl")
            .args(["-fsSL", "--connect-timeout", "10", "--max-time", "120", "-o"]).arg(dest).arg(url)
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn download_text(url: &str) -> Option<String> {
    let bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut cmd = Command::new(bin);
    cmd.args(["-fsSL", "--connect-timeout", "10", "--max-time", "30", url]);
    no_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() { return None; }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn sha256_of(path: &Path) -> Option<String> {
    if cfg!(windows) {
        let mut cmd = Command::new("certutil.exe");
        cmd.args(["-hashfile"]).arg(path).arg("SHA256");
        no_window(&mut cmd);
        let out = cmd.output().ok()?;
        if !out.status.success() { return None; }
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let l = line.trim();
            if l.len() == 64 && l.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(l.to_lowercase());
            }
        }
        None
    } else {
        let out = Command::new("sha256sum").arg(path).output().ok()?;
        if !out.status.success() { return None; }
        let text = String::from_utf8_lossy(&out.stdout);
        text.split_whitespace().next().map(|s| s.to_lowercase())
    }
}
