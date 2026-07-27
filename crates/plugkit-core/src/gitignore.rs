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
    // Retired: the rs-learn crate is removed and legacy_reaper.rs now deletes
    // these outright. Kept as ignore entries only so a project that has not yet
    // booted a reaping build does not surface them as residuals in the
    // meantime; they can be dropped once no live checkout predates the reaper.
    ".gm/rs-learn.db",
    ".gm/rs-learn.db-shm",
    ".gm/rs-learn.db-wal",
    ".gm/learning-state.md",
    ".gm/git-block-counter.json",
    ".gm/disciplines/codeinsight/",
    ".gm/disciplines/codeinsight-vec/",
    ".gm/instructions-source-cache/",
    // Materialized config repo plus config_sync.rs's per-source debounce/
    // backoff state and lock dirs (siblings of the cache dir, hence the glob).
    // All three are derived from the remote and are re-created on demand;
    // committing them would put one machine's probe timestamps and a
    // transient lock into every other checkout.
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
    // The config SPEC files must travel with the repo -- they are the project's
    // deliberate choice of which workflow it runs, and a teammate cloning
    // without them silently falls back to a different tier. Only the
    // MATERIALIZED cache is ignored (see the ignore list above): the spec is
    // authored, the cache is derived.
    //
    // Load-bearing because projects commonly carry a blanket `*` in
    // .gm/.gitignore -- live-witnessed here, where it made
    // .gm/config.source.json uncommittable, so a project could never share the
    // config repo it had chosen.
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

/// Re-include the must-stay-tracked files from INSIDE `.gm/.gitignore`.
///
/// Git applies the DEEPEST .gitignore last, so a negation written into the
/// repo-root .gitignore cannot override a blanket `*` living in a nested
/// `.gm/.gitignore` -- the nested file has the final say for paths beneath it.
/// Live-witnessed with real `git check-ignore`: root-level negations left
/// `.gm/config.source.json` ignored, which meant a project could never commit
/// (and so never share) its own choice of config repo.
///
/// So the negations have to be written into the nested file itself. Only the
/// authored spec/state files are re-included; derived caches stay ignored.
/// A missing or `*`-free `.gm/.gitignore` is left completely alone -- this is a
/// targeted antidote to a hostile blanket, not a file this code wants to own.
fn ensure_gm_dir_negations() {
    let path = ".gm/.gitignore";
    let original = match host_read(path) {
        Some(s) => s,
        None => return,
    };
    // Only intervene when a blanket pattern is actually present; anything
    // narrower cannot be silently swallowing the spec files.
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
        // These paths are written repo-root-relative; inside .gm/ they need the
        // leading ".gm/" dropped, and anything outside .gm/ does not belong here.
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
    ensure_gm_dir_negations();
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
