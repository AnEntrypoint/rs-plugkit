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
    ".gm/claim-audit-fired",
    ".gm/fsm-graph-rejected.json",
    ".gm/bootstrap-status.json",
    ".gm/bootstrap-error.json",
    ".gm/trajectory-drafts/",
    ".gm/ingest-drafts/",
    ".gm/prd-state.json",
    ".gm/subagent-*.json",
    ".gm/browser-profile/",
    ".gm/browser-profile-*/",
    ".gm/browser-profiles/",
    ".gm/browser-chrome-profile-*/",
    ".gm/build-tool-ignores.md",
    ".gm/last-prompt.txt",
    ".gm/hooks/",
    ".gm/no-memorize-this-turn",
    ".gm/prd.paused.yml",
    ".gm/rs-learn.db",
    ".gm/rs-learn.db-shm",
    ".gm/rs-learn.db-wal",
    ".gm/learning-state.md",
    ".gm/git-block-counter.json",
    ".gm/disciplines/codeinsight/",
    ".gm/disciplines/codeinsight-vec/",
    ".gm/instructions-source-cache/",
    ".gm/config-source-cache/",
    ".gm/config-source-cache.*",
    ".plugkit-browser-profile/",
    ".plugkit-browser-profile-*/",
];

pub const MUST_STAY_TRACKED: &[&str] = &[
    ".gm/code-search/",
    ".gm/disciplines/",
    ".gm/prd.yml",
    ".gm/mutables.yml",
    ".gm/gm.config.json",
    ".gm/config.source.json",
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

fn ensure_gm_dir_negations_into_nested_gitignore() {
    let path = ".gm/.gitignore";
    let original = match host_read(path) {
        Some(s) => s,
        None => return,
    };
    let has_blanket = original
        .lines()
        .map(|l| l.trim())
        .any(|l| l == "*" || l == "**" || l == "*.*");
    if !has_blanket {
        return;
    }

    let stripped = strip_block(&original, START_MARKER, END_MARKER);
    let mut block = String::new();
    block.push_str(START_MARKER);
    block.push('\n');
    block.push_str("# A blanket ignore above would otherwise make these uncommittable.\n");
    block.push_str("# They are AUTHORED project state (the workflow this project runs, its\n");
    block.push_str("# open work) -- derived caches are deliberately not re-included here.\n");
    for entry in MUST_STAY_TRACKED {
        let rel = match entry.strip_prefix(".gm/") {
            Some(r) => r,
            None => continue,
        };
        let bare = rel.trim_end_matches('/');
        block.push_str(&format!("!{}\n", bare));
        if rel.ends_with('/') {
            block.push_str(&format!("!{}/**\n", bare));
        }
    }
    block.push_str(END_MARKER);

    let mut next = stripped.trim_end_matches(['\n', '\r']).to_string();
    if next.is_empty() {
        next = block;
    } else {
        next.push_str("\n\n");
        next.push_str(&block);
    }
    next.push('\n');
    if next != original {
        let _ = crate::wasm_dispatch::host_write(path, &next);
    }
}

pub fn ensure_managed_gitignore(cwd: &str) -> Result<bool, String> {
    ensure_gm_dir_negations_into_nested_gitignore();
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

    let mut hostile_stripped: Vec<String> = Vec::new();
    let stripped_of_hostile: String = stripped
        .lines()
        .filter(|line| {
            let t = line.trim();
            if MUST_STAY_TRACKED.iter().any(|e| *e == t) {
                hostile_stripped.push(t.to_string());
                log_warn(&format!("plugkit gitignore: stripping hostile entry outside managed block, must stay tracked: {}", t));
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut cleaned = stripped_of_hostile.trim_end_matches('\n').trim_end_matches('\r').to_string();
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

    if changed {
        if !host_write(&path, &cleaned) {
            return Err(format!("host_fs_write failed for {}", path));
        }
        log_info(&format!("plugkit gitignore: updated {} ({} entries)", path, MANAGED_ENTRIES.len()));
    }
    Ok(changed)
}
