// Background self-update: every startup, check pin file, spawn detached
// updater child if newer. Hot path NEVER blocks on this — current binary
// handles the hook; new binary takes over on next invocation.

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

pub fn check_and_dispatch() {
    let pin_path = tools_dir().join("plugkit.version");
    let pinned = match std::fs::read_to_string(&pin_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if pinned.is_empty() { return; }
    let current = env!("CARGO_PKG_VERSION").to_string();
    if pinned == current { return; }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut cmd = Command::new(&exe);
    cmd.arg("--self-update").arg(&pinned);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    let _ = cmd.spawn();
}

pub fn run_self_update(pinned: &str) {
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

    schedule_swap(&new_path, &target_exe);
}

fn download(url: &str, dest: &Path) -> bool {
    if cfg!(windows) {
        let mut cmd = Command::new("curl.exe");
        cmd.args(["-fsSL", "-o"]).arg(dest).arg(url);
        no_window(&mut cmd);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.status().map(|s| s.success()).unwrap_or(false)
    } else {
        Command::new("curl")
            .args(["-fsSL", "-o"]).arg(dest).arg(url)
            .stdout(Stdio::null()).stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn download_text(url: &str) -> Option<String> {
    let bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut cmd = Command::new(bin);
    cmd.args(["-fsSL", url]);
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

fn schedule_swap(new_path: &Path, target_exe: &Path) {
    if cfg!(windows) {
        let new_s = new_path.to_string_lossy();
        let target_s = target_exe.to_string_lossy();
        let script = format!(
            "ping -n 4 127.0.0.1 >NUL & move /Y \"{}\" \"{}\" >NUL 2>&1",
            new_s, target_s
        );
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/c", &script]);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        let _ = cmd.spawn();
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(new_path, std::fs::Permissions::from_mode(0o755));
        }
        let new_s = new_path.to_string_lossy().to_string();
        let target_s = target_exe.to_string_lossy().to_string();
        let script = format!("sleep 3; mv -f '{}' '{}'", new_s, target_s);
        let _ = Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
}
