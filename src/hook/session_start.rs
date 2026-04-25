use super::{is_gemini, is_kilo, is_opencode, load_prompt, project_dir, run_self};
use serde_json::json;
use std::fs;

pub fn run() {
    let project = project_dir();
    ensure_gitignore(project.as_deref());
    ensure_tools_current();
    let session_start_msg = load_prompt("session-start").unwrap_or_else(|| "BLOCKING REQUIREMENT — READ THIS FIRST: Your VERY FIRST action on EVERY user message MUST be to invoke the Skill tool with skill: \"gm:gm\". Do NOT read files, do NOT search, do NOT answer, do NOT plan, do NOT use any other tool before invoking skill: \"gm:gm\". This is non-negotiable. If you respond without invoking gm:gm first, you are violating a hard constraint.\n\n=== TOOL RULES ===\n\nSkill tool: invoke skills by name (e.g. skill: \"gm:gm\"). Never use the Agent tool to load skills.\n\nBash tool: only these formats are allowed —\n  exec:nodejs / exec:python / exec:bash / exec:typescript / exec:go / exec:rust / exec:c / exec:cpp / exec:java\n  exec:browser  (JS automation against `page`)\n  exec:codesearch  (natural language search)\n  exec:status / exec:sleep / exec:close / exec:runner / exec:type\n  git <args>  (git commands directly, no exec: prefix)\n  Everything else is blocked. Never Bash(node ...) or Bash(npm ...) or Bash(npx ...).\n\nGlob/Grep/Find/Explore: blocked — use exec:codesearch instead.".to_string());
    let mut parts: Vec<String> = vec![session_start_msg];

    if let Some(ref dir) = project {
        let insight = {
            let cached = run_self(&["codeinsight", dir, "--read-cache"]);
            if cached.is_empty() || cached.starts_with("No cache") || cached.starts_with("Error") {
                run_self(&["codeinsight", dir, "--cache"])
            } else {
                cached
            }
        };
        if !insight.is_empty() && !insight.starts_with("Error") && !insight.starts_with("No cache") {
            parts.push(format!(
                "=== This is your initial insight of the repository, look at every possible aspect of this for initial opinionation and to offset the need for code exploration ===\n{}",
                insight
            ));
        }

        let recall_q = super::rs_learn::project_query(dir);
        let recall = super::rs_learn::recall(&recall_q, dir, 3);
        if !recall.is_empty() {
            parts.push(format!(
                "=== rs-learn recall (project memory — past decisions, feedback, and lessons) ===\n{}",
                recall
            ));
        }
    }

    let additional_context = parts.join("\n\n").replace("${", "$\\{");

    let output = if is_gemini() {
        json!({ "systemMessage": additional_context })
    } else if is_opencode() || is_kilo() {
        json!({ "hookSpecificOutput": { "hookEventName": "session.created", "additionalContext": additional_context } })
    } else {
        json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": additional_context } })
    };

    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
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
fn ensure_tools_current() {
    let Some(plugin_root) = std::env::var("CLAUDE_PLUGIN_ROOT").ok() else { return };
    let src_dir = std::path::Path::new(&plugin_root).join("bin");
    let dst_dir = super::tools_dir();
    if let Err(_) = fs::create_dir_all(&dst_dir) { return; }

    let names = if cfg!(windows) {
        &["plugkit.exe", "rs-exec.exe", "rs-exec-process.exe"][..]
    } else {
        &["plugkit", "rs-exec", "rs-exec-process"][..]
    };

    for name in names {
        let src = src_dir.join(name);
        if !src.exists() { continue; }
        let dst = dst_dir.join(name);
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
         .gm/rslearn-counter.json\n\
         .gm/git-block-counter.json\n\
         .gm/learning-state.md\n\
         .gm/trajectory-drafts/\n\
         .gm/ingest-drafts/\n\
         .gm/needs-gm\n\
         .gm/lastskill\n\
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
