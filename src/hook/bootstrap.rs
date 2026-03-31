use std::{env, fs, path::PathBuf};

fn plugin_root() -> Option<String> {
    env::var("CLAUDE_PLUGIN_ROOT").ok()
}

fn gm_json_version(root: &str) -> Option<String> {
    let path = PathBuf::from(root).join("gm.json");
    let text = fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["plugkitVersion"].as_str().map(|s| s.to_string())
}

fn version_file(root: &str) -> PathBuf {
    PathBuf::from(root).join("bin").join(".plugkit-version")
}

fn current_version(root: &str) -> Option<String> {
    fs::read_to_string(version_file(root)).ok().map(|s| s.trim().to_string())
}

fn bin_path(root: &str) -> PathBuf {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    PathBuf::from(root).join("bin").join(format!("plugkit{}", ext))
}

fn pending_path(root: &str) -> PathBuf {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    PathBuf::from(root).join("bin").join(format!("plugkit{}.pending", ext))
}

fn asset_name() -> String {
    let os = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("plugkit-{}-{}{}", os, arch, ext)
}

fn apply_pending(root: &str) {
    let pending = pending_path(root);
    if !pending.exists() { return; }
    let bin = bin_path(root);
    let _ = fs::remove_file(&bin);
    let _ = fs::rename(&pending, &bin);
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)); }
    let pv = PathBuf::from(pending.to_string_lossy().to_string() + ".version");
    let vf = version_file(root);
    if pv.exists() {
        let _ = fs::rename(&pv, &vf);
    }
}

fn download_to(version: &str, dest: &PathBuf) -> Result<(), String> {
    let asset = asset_name();
    let url = format!("https://github.com/AnEntrypoint/rs-plugkit/releases/download/v{}/{}", version, asset);
    let output = std::process::Command::new("curl")
        .args(["-fsSL", "--location", &url, "-o", &dest.to_string_lossy()])
        .output()
        .map_err(|e| format!("curl failed: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl error: {}", stderr));
    }
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = fs::set_permissions(dest, fs::Permissions::from_mode(0o755)); }
    Ok(())
}

pub fn run() {
    let root = match plugin_root() {
        Some(r) => r,
        None => return,
    };

    apply_pending(&root);

    let required = match gm_json_version(&root) {
        Some(v) => v,
        None => return,
    };
    let current = current_version(&root);
    if current.as_deref() == Some(&required) { return; }

    let bin = bin_path(&root);
    let pending = pending_path(&root);

    let dest = if bin.exists() { &pending } else { &bin };
    match download_to(&required, dest) {
        Ok(()) => {
            if dest == &pending {
                let pv = PathBuf::from(pending.to_string_lossy().to_string() + ".version");
                let _ = fs::write(&pv, &required);
            } else {
                let _ = fs::write(version_file(&root), &required);
            }
        }
        Err(e) => {
            eprintln!("bootstrap: update failed: {}", e);
        }
    }
}
