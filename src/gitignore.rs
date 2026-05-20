#![cfg(target_arch = "wasm32")]

use crate::wasm_dispatch::{host_read, host_write, host_log};

pub const MANAGED_ENTRIES: &[&str] = &[
    ".gm/exec-spool/",
    ".gm/gm-fired-*",
    ".gm/needs-gm",
    ".gm/lastskill",
    ".gm/turn-state.json",
    ".gm/turn-state.json.corrupted-*",
    ".gm/residual-check-fired",
    ".gm/bootstrap-status.json",
    ".gm/bootstrap-error.json",
    ".gm/rslearn-counter.json",
    ".gm/trajectory-drafts/",
    ".gm/ingest-drafts/",
    ".gm/prd-state.json",
    ".gm/subagent-*.json",
    ".gm/browser-profile/",
    ".gm/browser-profile-*/",
    ".gm/build-tool-ignores.md",
    ".gm/last-prompt.txt",
    ".gm/hooks/",
    ".gm/no-memorize-this-turn",
    ".gm/prd.paused.yml",
    ".gm/rs-learn.db-shm",
    ".gm/rs-learn.db-wal",
    ".gm/learning-state.md",
    ".gm/git-block-counter.json",
    ".plugkit-browser-profile/",
    ".plugkit-browser-profile-*/",
];

pub const MUST_STAY_TRACKED: &[&str] = &[
    ".gm/rs-learn.db",
    ".gm/code-search/",
    ".gm/disciplines/",
    ".gm/prd.yml",
    ".gm/mutables.yml",
    "gm-data/rs-learn.db",
    "gm-data/code-search/",
    "gm-data/disciplines/",
];

const START_MARKER: &str = "# >>> plugkit managed";
const END_MARKER: &str = "# <<< plugkit managed";
const LEGACY_START_GM: &str = "# >>> gm managed";
const LEGACY_END_GM: &str = "# <<< gm managed";

fn log_warn(msg: &str) {
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
}

fn log_info(msg: &str) {
    unsafe { host_log(1, msg.as_ptr(), msg.len() as u32); }
}

fn strip_block(content: &str, start: &str, end: &str) -> String {
    let mut out = String::new();
    let mut rest = content;
    loop {
        match rest.find(start) {
            None => { out.push_str(rest); return out; }
            Some(si) => {
                out.push_str(&rest[..si]);
                let after = &rest[si..];
                match after.find(end) {
                    None => {
                        return out;
                    }
                    Some(ei) => {
                        let cut = ei + end.len();
                        let mut tail = &after[cut..];
                        if tail.starts_with("\r\n") { tail = &tail[2..]; }
                        else if tail.starts_with('\n') { tail = &tail[1..]; }
                        rest = tail;
                        while out.ends_with("\n\n") { out.pop(); }
                    }
                }
            }
        }
    }
}

pub fn ensure_managed_gitignore(cwd: &str) -> Result<bool, String> {
    let path = if cwd.is_empty() {
        ".gitignore".to_string()
    } else if cwd.ends_with('/') || cwd.ends_with('\\') {
        format!("{}.gitignore", cwd)
    } else {
        format!("{}/.gitignore", cwd)
    };

    let original = host_read(&path).unwrap_or_default();

    let stripped = strip_block(&original, LEGACY_START_GM, LEGACY_END_GM);
    let stripped = strip_block(&stripped, START_MARKER, END_MARKER);

    let mut block = String::new();
    block.push_str(START_MARKER);
    block.push('\n');
    for entry in MANAGED_ENTRIES {
        block.push_str(entry);
        block.push('\n');
    }
    block.push_str(END_MARKER);

    let mut cleaned = stripped.trim_end_matches('\n').trim_end_matches('\r').to_string();
    if cleaned.is_empty() {
        cleaned = block;
    } else {
        cleaned.push_str("\n\n");
        cleaned.push_str(&block);
    }
    if !cleaned.ends_with('\n') {
        cleaned.push('\n');
    }

    let changed = cleaned != original;

    for line in cleaned.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') { continue; }
        if MANAGED_ENTRIES.iter().any(|e| *e == t) { continue; }
        if MUST_STAY_TRACKED.iter().any(|e| *e == t) {
            log_warn(&format!("plugkit gitignore: hostile entry must stay tracked: {}", t));
        }
    }

    if changed {
        if !host_write(&path, &cleaned) {
            return Err(format!("host_fs_write failed for {}", path));
        }
        log_info(&format!("plugkit gitignore: updated {} ({} entries)", path, MANAGED_ENTRIES.len()));
    }
    Ok(changed)
}
