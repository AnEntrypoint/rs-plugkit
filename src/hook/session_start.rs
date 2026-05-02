use super::{is_gemini, is_kilo, is_opencode, load_prompt, project_dir, run_self};
use serde_json::json;
use std::fs;

pub fn run() {
    let project = project_dir();
    ensure_gitignore(project.as_deref());
    ensure_claude_md_pointer(project.as_deref());
    ensure_tools_current();
    write_needs_gm_if_gm_project(project.as_deref());

    if let Some(ref dir) = project {
        let insight = {
            let cached = run_self(&["codeinsight", dir, "--read-cache"]);
            if cached.is_empty() || cached.starts_with("No cache") || cached.starts_with("Error") {
                run_self(&["codeinsight", dir, "--cache"])
            } else {
                cached
            }
        };

        let mut context_parts: Vec<String> = Vec::new();

        if !insight.is_empty() && !insight.starts_with("Error") && !insight.starts_with("No cache") {
            context_parts.push(format!(
                "=== This is your initial insight of the repository, look at every possible aspect of this for initial opinionation and to offset the need for code exploration ===\n{}",
                insight
            ));
        }

        let recall_q = super::rs_learn::project_query(dir);
        let recall = super::rs_learn::recall(&recall_q, dir, 3);
        if !recall.is_empty() {
            context_parts.push(format!(
                "=== rs-learn recall (project memory — past decisions, feedback, and lessons) ===\n{}",
                recall
            ));
        }

        let prd_path = std::path::Path::new(dir).join(".gm").join("prd.yml");
        let workspace_context = context_parts.join("\n\n");
        let system_message = format!(
            "Session start for workspace: {}\n\n{}\n\nPRD path: {}\n\nInvoke Skill(gm:gm) first. Resolve unknowns with witnessed probes, recall, or the PRD. Never ask the user when the PRD is present.",
            dir,
            if workspace_context.is_empty() { "No prior context loaded.".to_string() } else { workspace_context },
            prd_path.display()
        );

        println!("{}", serde_json::to_string_pretty(&json!({ "systemMessage": system_message })).unwrap_or_default());
    } else {
        println!("{}", serde_json::to_string_pretty(&json!({ "systemMessage": "" })).unwrap_or_default());
    }
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
    let prd = gm_dir.join("prd.yml");
    if prd.exists() {
        let content = fs::read_to_string(&prd).unwrap_or_default();
        if !content.trim().is_empty() { return; }
    }
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

fn ensure_tools_current() {
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

    let _ = fs::write(&gitignore, new_content);
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
