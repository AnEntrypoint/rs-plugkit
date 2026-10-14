use std::collections::HashSet;

use serde_json::Value;

use crate::code_index::{
    gitignore_excludes, is_dependency_noise_dir_segment, is_hidden_segment, is_skipped_dir_segment,
    list_dir, load_repo_gitignore,
};
use crate::ragconfig::IndexConfig;
use crate::wasm_dispatch::{git_call_argv, host_exists, host_now_ms, host_stat_is_directory};

const GIT_LISTING_SPLIT_DEPTH_LIMIT: usize = 16;

const NESTED_REPO_DEPTH_LIMIT: usize = 8;
const LISTING_WALK_ANSWERABLE_BUDGET_MS: u64 = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileSource {
    Git,
    Walk,
    SingleFile,
}

impl FileSource {
    pub fn label(self) -> &'static str {
        match self {
            FileSource::Git => "git",
            FileSource::Walk => "walk",
            FileSource::SingleFile => "file",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            FileSource::Git => "git ls-files: tracked files plus untracked files git does not ignore (\"no_ignore\": true adds the ignored ones); pass \"refresh\": true to walk the disk instead",
            FileSource::Walk => "filesystem walk under the target: every file on disk that no exclusion rule dropped",
            FileSource::SingleFile => "one file named by \"path\", read straight from disk",
        }
    }
}

pub struct RuleExclusion {
    pub path: String,
    pub rule: &'static str,
    pub files: Option<usize>,
}

pub const OWN_STATE_RULE: &str = "gm_state_dir";

pub const GM_STATE_EXCLUSION_RULES: &[&str] = &[OWN_STATE_RULE, "agentplug_kv_cache"];

pub fn is_own_state_name(name: &str) -> bool {
    name == ".gm" || name.starts_with(".agentplug")
}

fn reported_own_state_entry_under_root(root: &str, path: &str) -> Option<String> {
    let prefix = join_under(root, "");
    let rel = path.strip_prefix(prefix.as_str())?;
    let segments: Vec<&str> = rel.split('/').collect();
    let parent_segment_count = segments.len() - 1;
    let own_state_at = segments[..parent_segment_count]
        .iter()
        .position(|segment| is_own_state_name(segment))?;
    let reported_child_at = (own_state_at + 1).min(parent_segment_count);
    Some(format!("{prefix}{}", segments[..=reported_child_at].join("/")))
}

fn is_versioned_gm_state(root: &str, path: &str) -> bool {
    let prefix = join_under(root, "");
    let Some(rel) = path.strip_prefix(prefix.as_str()) else {
        return false;
    };
    let segments: Vec<&str> = rel.split('/').collect();
    segments[..segments.len() - 1].iter().any(|segment| *segment == ".gm")
}

fn prune_own_state(root: &str, files: Vec<String>, keep_versioned_gm: bool) -> (Vec<String>, Vec<RuleExclusion>) {
    let mut kept = Vec::with_capacity(files.len());
    let mut pruned: Vec<RuleExclusion> = Vec::new();
    for file in files {
        if keep_versioned_gm && is_versioned_gm_state(root, &file) {
            kept.push(file);
            continue;
        }
        match reported_own_state_entry_under_root(root, &file) {
            None => kept.push(file),
            Some(entry) => match pruned.iter_mut().find(|p| p.path == entry) {
                Some(p) => p.files = Some(p.files.unwrap_or(0) + 1),
                None => pruned.push(RuleExclusion {
                    path: entry,
                    rule: OWN_STATE_RULE,
                    files: Some(1),
                }),
            },
        }
    }
    (kept, pruned)
}

fn report_unlisted_own_state(root: &str, kept: &[String], pruned: &mut Vec<RuleExclusion>) {
    for name in child_names(root).into_iter().filter(|n| is_own_state_name(n)) {
        let state_path = join_under(root, &name);
        let entries: Vec<String> = if host_stat_is_directory(&state_path) == Some(true) {
            child_names(&state_path)
                .into_iter()
                .map(|child| join_under(&state_path, &child))
                .collect()
        } else {
            vec![state_path]
        };
        for path in entries {
            let prefix = join_under(&path, "");
            let listed = kept.iter().any(|k| *k == path || k.starts_with(prefix.as_str()));
            if listed || pruned.iter().any(|p| p.path == path) {
                continue;
            }
            pruned.push(RuleExclusion {
                path,
                rule: OWN_STATE_RULE,
                files: None,
            });
        }
    }
}

pub struct ScanUniverse {
    pub files: Vec<String>,
    pub source: FileSource,
    pub listing_complete: bool,
    pub excluded: Vec<RuleExclusion>,
    pub walk_reason: Option<String>,
    // What git itself contributed to the corpus: a git corpus whose tracked count is 0 carries no
    // committed content at all, so the reply has to be able to say so -- the number is what turns
    // "searched 1803 files" from a reassurance into the blind spot it is when nothing is tracked.
    pub git_tracked_files: usize,
    pub git_untracked_files: usize,
    // Nested worktrees (submodule checkouts) this corpus had to be widened for, and whether the
    // untracked listing inside one of them failed: their untracked files are outside the parent
    // worktree's own listing, so a miss here is a blind spot a zero must not be read over.
    pub git_nested_worktrees: usize,
    pub git_nested_untracked_failed: bool,
    target: String,
}

impl ScanUniverse {
    pub fn tracked_paths_deleted_from_worktree(&self) -> HashSet<String> {
        if self.source != FileSource::Git {
            return HashSet::new();
        }
        let mut rel = Vec::new();
        match git_list_into(&self.target, &["--deleted"], None, 0, &mut rel) {
            Ok(_) => rel
                .into_iter()
                .map(|p| join_under(&self.target, &p))
                .collect(),
            Err(_) => HashSet::new(),
        }
    }
}

pub fn join_under(base: &str, rel: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{rel}")
    } else {
        format!("{base}/{rel}")
    }
}

fn git_cwd(root: &str) -> Option<&str> {
    if root.is_empty() || root == "." {
        None
    } else {
        Some(root)
    }
}

fn git_exit_code(v: &Value) -> i64 {
    v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1)
}

fn git_stderr(v: &Value) -> String {
    v.get("stderr")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn scope_inside_root(root: &str, scope: &str) -> Option<String> {
    let abs_root = absolute_root_for_message(root).replace('\\', "/");
    let abs_root = abs_root.trim_end_matches('/');
    if abs_root.is_empty() {
        return None;
    }
    let folded_root = abs_root.to_ascii_lowercase();
    let folded_scope = scope.to_ascii_lowercase();
    let rest = match folded_scope.strip_prefix(&folded_root) {
        Some(rest) => rest,
        None => return None,
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let separated_root_chars = abs_root.chars().count() + rest.chars().take_while(|c| *c == '/').count();
    Some(scope.chars().skip(separated_root_chars).collect())
}

fn relative_scope(root: &str, scope: &str) -> Result<Option<String>, String> {
    let normalized = scope.replace('\\', "/");
    let bytes = normalized.as_bytes();
    let rooted = if normalized.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':') {
        match scope_inside_root(root, &normalized) {
            Some(rel) => rel,
            None => return Err(format!(
                "path '{scope}' is outside the search root '{}' -- the root actually searched is the dispatch project (the cwd this dispatch ran in) unless body \"root\" names another directory; to search another project pass its directory as \"root\" with a path relative to it, or dispatch with that project as cwd",
                absolute_root_for_message(root),
            )),
        }
    } else {
        normalized
    };
    let segments: Vec<&str> = rooted
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segments.iter().any(|s| *s == "..") {
        return Err(format!(
            "path '{scope}' may not climb out of the search root with '..'"
        ));
    }
    Ok(if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    })
}

fn child_names(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = list_dir(dir)
        .into_iter()
        .filter_map(|e| e.split('/').next().map(String::from))
        .filter(|n| !n.is_empty() && n != ".git")
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

fn git_list_into(
    cwd_dir: &str,
    mode: &[&str],
    pathspec: Option<&str>,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<bool, String> {
    let mut argv = vec!["--literal-pathspecs", "ls-files", "-z"];
    argv.extend_from_slice(mode);
    if let Some(p) = pathspec {
        argv.push("--");
        argv.push(p);
    }
    let r = git_call_argv(&argv, git_cwd(cwd_dir));
    if git_exit_code(&r) != 0 {
        return Err(format!(
            "git {} exited {}: {}",
            argv.join(" "),
            git_exit_code(&r),
            git_stderr(&r)
        ));
    }
    let stdout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let capped = r
        .get("stdout_truncated")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    if !capped {
        out.extend(
            stdout
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(String::from),
        );
        return Ok(true);
    }
    let listed_dir = pathspec
        .map(|p| join_under(cwd_dir, p))
        .unwrap_or_else(|| cwd_dir.to_string());
    if depth >= GIT_LISTING_SPLIT_DEPTH_LIMIT || host_stat_is_directory(&listed_dir) != Some(true) {
        let mut entries: Vec<&str> = stdout.split('\0').collect();
        entries.pop();
        out.extend(
            entries
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(String::from),
        );
        return Ok(false);
    }
    let mut complete = true;
    for child in child_names(&listed_dir) {
        let rel = match pathspec {
            Some(p) => join_under(p, &child),
            None => child,
        };
        complete &= git_list_into(cwd_dir, mode, Some(&rel), depth + 1, out)?;
    }
    Ok(complete)
}

fn directory_is_gitignored(dir: &str) -> Result<bool, String> {
    let r = git_call_argv(&["check-ignore", "-q", "--", "."], git_cwd(dir));
    match git_exit_code(&r) {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!(
            "git check-ignore exited {code}: {}",
            git_stderr(&r)
        )),
    }
}

/// Directories holding their own `.git` under `dir`, found by listing rather than by `git ls-files
/// --stage`: git's own listing of the parent names a submodule's TRACKED files only, so a file
/// written into a submodule worktree is untracked there and invisible to the parent's
/// `--others --exclude-standard`. Those files are what this finds, so a fresh write is searchable
/// without `refresh: true`. The walk stops at the first `.git` it meets and at this depth, so it
/// costs a handful of readdirs and never descends a whole tree.
const NESTED_WORKTREE_SCAN_DEPTH: usize = 2;
const MAX_NESTED_WORKTREES: usize = 64;

fn nested_worktree_dirs(dir: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut frontier: Vec<(String, usize)> = vec![(dir.to_string(), 0)];
    while let Some((current, depth)) = frontier.pop() {
        if depth >= NESTED_WORKTREE_SCAN_DEPTH || found.len() >= MAX_NESTED_WORKTREES {
            continue;
        }
        for child in child_names(&current) {
            let path = join_under(&current, &child);
            if host_exists(&join_under(&path, ".git")) {
                found.push(path);
                continue;
            }
            if host_stat_is_directory(&path) == Some(true) {
                frontier.push((path, depth + 1));
            }
        }
    }
    found
}

fn gitlink_dirs(dir: &str, complete: &mut bool) -> (Vec<String>, bool) {
    let mut staged = Vec::new();
    match git_list_into(dir, &["--stage"], None, 0, &mut staged) {
        Ok(c) => {
            *complete &= c;
            let dirs = staged
                .iter()
                .filter_map(|entry| {
                    let after_mode = entry.strip_prefix("160000 ")?;
                    let path = after_mode.split('\t').nth(1)?;
                    (!path.is_empty()).then(|| join_under(dir, path))
                })
                .collect();
            (dirs, false)
        }
        Err(_) => {
            *complete = false;
            (Vec::new(), true)
        }
    }
}

fn nested_untracked(dir: &str, nesting: usize, no_ignore: bool) -> (Vec<String>, bool) {
    if nesting >= NESTED_REPO_DEPTH_LIMIT {
        return (Vec::new(), false);
    }
    let others_mode: &[&str] = if no_ignore {
        &["--others"]
    } else {
        &["--others", "--exclude-standard"]
    };
    let mut listed = Vec::new();
    let mut complete = match git_list_into(dir, others_mode, None, 0, &mut listed) {
        Ok(c) => c,
        Err(_) => false,
    };
    let mut files = Vec::with_capacity(listed.len());
    for entry in listed {
        match entry.strip_suffix('/') {
            Some(nested_repo) => {
                let (nested_files, nested_complete) =
                    nested_untracked(&join_under(dir, nested_repo), nesting + 1, no_ignore);
                files.extend(nested_files);
                complete &= nested_complete;
            }
            None => files.push(join_under(dir, &entry)),
        }
    }
    let (gitlink_nested, gitlink_failed) = gitlink_dirs(dir, &mut complete);
    if gitlink_failed {
        complete = false;
    }
    for nested_dir in gitlink_nested {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore);
        files.extend(nested_files);
        complete &= nested_complete;
    }
    (files, complete)
}

fn git_worktree_files(dir: &str, nesting: usize, no_ignore: bool) -> Result<(Vec<String>, bool, usize, usize, usize, bool), String> {
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    let others_mode: &[&str] = if no_ignore { &["--others"] } else { &["--others", "--exclude-standard"] };
    let mut complete = git_list_into(dir, &["--cached", "--recurse-submodules"], None, 0, &mut tracked)?;
    complete &= git_list_into(dir, others_mode, None, 0, &mut untracked)?;
    let mut tracked_count = tracked.len();
    let mut nested_worktrees = 0usize;
    let mut files: Vec<String> = tracked.into_iter().map(|p| join_under(dir, &p)).collect();
    for entry in untracked {
        let Some(nested_repo) = entry.strip_suffix('/') else {
            files.push(join_under(dir, &entry));
            continue;
        };
        let nested_dir = join_under(dir, nested_repo);
        match (nesting < NESTED_REPO_DEPTH_LIMIT).then(|| git_worktree_files(&nested_dir, nesting + 1, no_ignore)) {
            Some(Ok((nested_files, nested_complete, nested_tracked, _, nested_dirs, nested_failed))) => {
                files.extend(nested_files);
                complete &= nested_complete && !nested_failed;
                tracked_count += nested_tracked;
                nested_worktrees += nested_dirs;
            }
            _ => complete = false,
        }
    }
    // A submodule worktree is a repository of its own, so its untracked files are not in the parent
    // worktree's `--others` listing. They are listed inside each one and joined here, so a file
    // written minutes ago is in the corpus without `refresh: true`. Both discovery routes are
    // tried because either can be the one that answers: git's own gitlink listing, and a listing
    // for the worktrees that route missed.
    let (mut nested_dirs, gitlink_failed) = gitlink_dirs(dir, &mut complete);
    for found in nested_worktree_dirs(dir) {
        if !nested_dirs.iter().any(|d| *d == found) {
            nested_dirs.push(found);
        }
    }
    let mut nested_untracked_failed = gitlink_failed;
    nested_worktrees += nested_dirs.len();
    for nested_dir in nested_dirs {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore);
        if !nested_complete {
            nested_untracked_failed = true;
        }
        files.extend(nested_files);
    }
    files.sort_unstable();
    files.dedup();
    let untracked_count = files.len().saturating_sub(tracked_count.min(files.len()));
    Ok((files, complete, tracked_count, untracked_count, nested_worktrees, nested_untracked_failed))
}

fn runtime_artifact_rule(path: &str) -> Option<&'static str> {
    let normalized_path = path.replace('\\', "/");
    let mut segments = normalized_path.split('/');
    while let Some(segment) = segments.next() {
        if segment == ".agentplug-kv" {
            return Some("agentplug_kv_cache");
        }
        if segment == ".gm" && segments.next() == Some("exec-spool") {
            return Some("gm_exec_spool");
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TargetOrigin {
    ProjectDefault,
    CallerNamed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoiseDirs {
    ProjectNoiseList,
    DependencyStoresVcsCachesOnly,
}

enum WalkCause {
    TargetGitignored,
    OutsideWorktree(String),
    GitListingFailed(String),
    GitTracksNothing,
    CallerForcedDisk,
}

struct WalkPolicy {
    honour_gitignore: bool,
    noise: NoiseDirs,
    reason: String,
}

const DEPENDENCY_WALK_SKIPS: &str = "skipping only VCS, dependency-store, cache and tool directories (build-output directories such as dist/ are read)";

const NO_IGNORE_WALK_NOTE: &str = "\"no_ignore\": true, so .gitignore/.codesearchignore rules were not applied";

const GIT_TRACKS_NOTHING_REASON: &str = "git tracks no file in this worktree (no commit yet, or an empty index), so its listing holds only the files the ignore rules spare; the target was walked on disk as well and its files joined with that listing";

pub const CORPUS_BLIND_SPOT_REASON: &str = "git tracks no file in this worktree, so its corpus is untracked files only; the disk walk that covers the rest did not finish, so the corpus does not hold every source file here and a zero over it is not a finding";

pub const NESTED_WORKTREE_BLIND_SPOT_NOTE: &str = "this corpus was listed by git, which does not hold the untracked files written inside a nested worktree (a submodule checkout): those files belong to the nested repository, not to this one. The listing of at least one nested worktree did not finish, so files written into it are missing here and a zero over this corpus is not a finding for them -- pass \"refresh\": true to walk the disk instead, or scope \"path\" to the worktree itself, before concluding a name is absent";

fn walk_policy(cause: WalkCause, origin: TargetOrigin, no_ignore: bool) -> WalkPolicy {
    let project = |reason: String| WalkPolicy { honour_gitignore: true, noise: NoiseDirs::ProjectNoiseList, reason };
    let mut policy = match (cause, origin) {
        (WalkCause::GitListingFailed(e), _) => project(format!("git could not list the worktree, so it was walked directly ({e})")),
        (WalkCause::GitTracksNothing, _) => project(GIT_TRACKS_NOTHING_REASON.to_string()),
        (WalkCause::CallerForcedDisk, _) => project("the caller passed \"refresh\": true, so git ls-files was not consulted and the target was walked on disk".to_string()),
        (WalkCause::TargetGitignored, TargetOrigin::ProjectDefault) => project("the target is gitignored, so git lists nothing there and it was walked directly".to_string()),
        (WalkCause::OutsideWorktree(e), TargetOrigin::ProjectDefault) => project(format!("not inside a git worktree, so it was walked directly ({e})")),
        (WalkCause::TargetGitignored, TargetOrigin::CallerNamed) => WalkPolicy {
            honour_gitignore: false,
            noise: NoiseDirs::DependencyStoresVcsCachesOnly,
            reason: format!("the named target is gitignored, so git lists nothing there; it was walked directly without .gitignore rules, {DEPENDENCY_WALK_SKIPS}"),
        },
        (WalkCause::OutsideWorktree(e), TargetOrigin::CallerNamed) => WalkPolicy {
            honour_gitignore: true,
            noise: NoiseDirs::DependencyStoresVcsCachesOnly,
            reason: format!("the named target is not inside a git worktree ({e}); it was walked directly honouring its root's .gitignore, {DEPENDENCY_WALK_SKIPS}"),
        },
    };
    if no_ignore && policy.honour_gitignore {
        policy.honour_gitignore = false;
        policy.reason = format!("{}; {NO_IGNORE_WALK_NOTE}", policy.reason);
    }
    policy
}

struct RuleRecordingWalk<'a> {
    cfg: &'a IndexConfig,
    gitignore: Option<ignore::gitignore::Gitignore>,
    noise: NoiseDirs,
    named_scope: bool,
    named_target: String,
    max_files: usize,
    deadline_ms: u64,
    reached_deadline: bool,
    files: Vec<String>,
    excluded: Vec<RuleExclusion>,
}

impl RuleRecordingWalk<'_> {
    fn descend(&mut self, dir: &str) {
        for entry in list_dir(dir) {
            if self.files.len() >= self.max_files || self.reached_deadline {
                return;
            }
            if unsafe { host_now_ms() } >= self.deadline_ms {
                self.reached_deadline = true;
                return;
            }
            let name = entry
                .rsplit('/')
                .next()
                .unwrap_or(entry.as_str())
                .to_string();
            if name == ".git" {
                continue;
            }
            let next = join_under(dir, &entry);
            let is_dir = host_stat_is_directory(&next).unwrap_or(false);
            let rule = if self.cfg.is_force_included(&next) {
                None
            } else {
                self.exclusion_rule(&name, &next, is_dir)
            };
            match (rule, is_dir) {
                (Some(rule), _) => self.excluded.push(RuleExclusion {
                    path: next,
                    rule,
                    files: None,
                }),
                (None, true) => self.descend(&next),
                (None, false) => self.files.push(next),
            }
        }
    }

    fn exclusion_rule(&self, name: &str, path: &str, is_dir: bool) -> Option<&'static str> {
        if self.named_target_is_at_or_above(path) { return None; }
        if gitignore_excludes(&self.gitignore, path, is_dir) { return Some("gitignore"); }
        if !is_dir { return None; }
        if is_hidden_segment(name) { return Some("hidden_dir"); }
        let noise = match self.noise {
            NoiseDirs::ProjectNoiseList => is_skipped_dir_segment(name, self.cfg),
            NoiseDirs::DependencyStoresVcsCachesOnly => {
                is_dependency_noise_dir_segment(name, self.cfg)
            }
        };
        noise.then_some("noise_dir_name")
    }

    fn named_target_is_at_or_above(&self, path: &str) -> bool {
        if !self.named_scope {
            return false;
        }
        if self.named_target == path {
            return true;
        }
        self.named_target
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

pub fn scope_has_no_entries(root: &str, scope: Option<&str>) -> bool {
    let rel = match scope {
        Some(s) => match relative_scope(root, s) {
            Ok(r) => r,
            Err(_) => return false,
        },
        None => None,
    };
    let target = rel
        .as_deref()
        .map(|r| join_under(root, r))
        .unwrap_or_else(|| root.to_string());
    host_stat_is_directory(&target) == Some(true) && child_names(&target).is_empty()
}

// Does a caller-named scope resolve to something that exists under this root? A named scope that
// resolves to nothing must be answered as an error: an empty scan over a scope that was never
// opened is indistinguishable from a real zero-match, and a caller reading the second as the
// first drops a hit that is really there.
pub fn scope_path_exists(root: &str, rel: &str) -> bool {
    host_stat_is_directory(&join_under(root, rel)).is_some()
}

pub fn empty_universe_refusal(
    root: &str,
    scope: Option<&str>,
    source: FileSource,
    listing_complete: bool,
    walk_reason: Option<&str>,
    file_cap: usize,
) -> String {
    let searched = match scope {
        Some(s) => format!("scope '{s}' under root '{}'", absolute_root_for_message(root)),
        None => format!("root '{}'", absolute_root_for_message(root)),
    };
    let cause = match (source, walk_reason) {
        (FileSource::Git, _) => "git listed no file in it".to_string(),
        (FileSource::SingleFile, _) => "the named file was not listed".to_string(),
        (_, Some(reason)) => format!("a disk walk listed no file in it ({reason})"),
        (FileSource::Walk, None) => "a disk walk listed no file in it".to_string(),
    };
    let completeness = if listing_complete {
        "the listing reported itself complete"
    } else {
        "the listing was cut short by its file cap or its time budget"
    };
    format!(
        "nothing was searched: {searched} yielded no searchable file, {cause}, and {completeness} (file cap {file_cap}) -- an empty result here would read as \"no matches\" when it really means \"nothing scanned\", so this is refused instead; narrow \"path\" to a subtree that exists, or pass \"refresh\": true to walk the disk instead of asking git, or \"no_ignore\": true to include ignored paths, then re-dispatch"
    )
}

pub fn budget_exhausted_before_any_read_refusal(
    root: &str,
    scope: Option<&str>,
    listed: usize,
    budget_ms: u64,
) -> String {
    let searched = match scope {
        Some(s) => format!("scope '{s}' under root '{}'", absolute_root_for_message(root)),
        None => format!("root '{}'", absolute_root_for_message(root)),
    };
    format!(
        "nothing was searched: {searched} listed {listed} file(s) but the {budget_ms} ms scan budget ran out before the first one was read, so an empty result here would read as \"no matches\" when it really means \"nothing scanned\" -- this is refused instead; narrow \"path\" to a subtree (a scoped scan finishes where an unscoped one does not), raise \"timeout_ms\", or re-dispatch when the box is less loaded"
    )
}

pub fn absolute_root_for_message(root: &str) -> String {
    if crate::pkfs::is_absolute(root) {
        return root.to_string();
    }
    crate::pkfs::anchor(root).trim_end_matches("/.").to_string()
}

pub fn list_scan_universe(root: &str, scopes: &[&str], max_files: usize, cfg: &IndexConfig, origin: TargetOrigin, force_disk: bool, no_ignore: bool, keep_versioned_gm: bool) -> Result<ScanUniverse, String> {
    let mut acc = list_scan_scope(root, scopes.first().copied(), max_files, cfg, origin, force_disk, no_ignore, keep_versioned_gm)?;
    for scope in scopes.iter().skip(1) {
        let next = list_scan_scope(root, Some(scope), max_files, cfg, origin, force_disk, no_ignore, keep_versioned_gm)?;
        acc.files.extend(next.files);
        acc.excluded.extend(next.excluded);
        acc.listing_complete &= next.listing_complete;
        acc.git_tracked_files = acc.git_tracked_files.saturating_add(next.git_tracked_files);
        acc.git_untracked_files = acc.git_untracked_files.saturating_add(next.git_untracked_files);
        acc.git_nested_worktrees = acc.git_nested_worktrees.saturating_add(next.git_nested_worktrees);
        acc.git_nested_untracked_failed |= next.git_nested_untracked_failed;
        if acc.walk_reason.is_none() { acc.walk_reason = next.walk_reason; }
        if acc.source != next.source { acc.source = FileSource::Walk; }
    }
    if scopes.len() > 1 {
        let mut seen: HashSet<String> = HashSet::with_capacity(acc.files.len());
        acc.files.retain(|f| seen.insert(f.clone()));
    }
    Ok(acc)
}

fn list_scan_scope(root: &str, scope: Option<&str>, max_files: usize, cfg: &IndexConfig, origin: TargetOrigin, force_disk: bool, no_ignore: bool, keep_versioned_gm: bool) -> Result<ScanUniverse, String> {
    let rel = match scope {
        Some(s) => relative_scope(root, s)?,
        None => None,
    };
    let named_scope = rel.is_some();
    let target = rel.as_deref().map(|r| join_under(root, r)).unwrap_or_else(|| root.to_string());
    let universe = |files, source, listing_complete, excluded, walk_reason, tracked, untracked, nested, nested_failed| ScanUniverse {
        files,
        source,
        listing_complete,
        excluded,
        walk_reason,
        git_tracked_files: tracked,
        git_untracked_files: untracked,
        git_nested_worktrees: nested,
        git_nested_untracked_failed: nested_failed,
        target: target.clone(),
    };
    if named_scope {
        match host_stat_is_directory(&target) {
            None => return Err(format!(
                "path '{}' does not exist under search root '{}' -- paths resolve relative to that root, which is the dispatch project unless `root` names another directory; if the path lives in a different project, pass that project's directory as `root` (or dispatch with its cwd)",
                scope.unwrap_or(""),
                absolute_root_for_message(root),
            )),
            Some(false) => return Ok(universe(vec![target.clone()], FileSource::SingleFile, true, Vec::new(), None, 0, 0, 0, false)),
            Some(true) => {}
        }
    }
    // What git listed, kept aside when its tracked set is empty: the walk below then answers on the
    // union, so an untracked tree is scanned instead of being reported as covered-but-empty.
    let mut git_join: Option<(Vec<String>, bool)> = None;
    let cause = if force_disk {
        WalkCause::CallerForcedDisk
    } else {
        match directory_is_gitignored(&target) {
            Ok(false) => match git_worktree_files(&target, 0, no_ignore) {
                Ok((files, complete, tracked, untracked, nested_dirs, nested_failed)) => {
                    let (files, mut pruned) = if named_scope {
                        (files, Vec::new())
                    } else {
                        let (kept, mut pruned) = prune_own_state(root, files, keep_versioned_gm);
                        report_unlisted_own_state(root, &kept, &mut pruned);
                        (kept, pruned)
                    };
                    let mut kept = Vec::with_capacity(files.len());
                    for path in files {
                        match runtime_artifact_rule(&path) {
                            Some(rule) => pruned.push(RuleExclusion {
                                path,
                                rule,
                                files: None,
                            }),
                            None => kept.push(path),
                        }
                    }
                    if tracked > 0 {
                        return Ok(universe(kept, FileSource::Git, complete, pruned, None, tracked, untracked, nested_dirs, nested_failed));
                    }
                    git_join = Some((kept, complete));
                    WalkCause::GitTracksNothing
                }
                Err(e) => WalkCause::GitListingFailed(e),
            },
            Ok(true) => WalkCause::TargetGitignored,
            Err(e) => WalkCause::OutsideWorktree(e),
        }
    };
    let policy = walk_policy(cause, origin, no_ignore);
    let mut walk = RuleRecordingWalk {
        cfg,
        gitignore: if policy.honour_gitignore {
            load_repo_gitignore(root)
        } else {
            None
        },
        noise: policy.noise,
        named_scope,
        named_target: target.clone(),
        max_files,
        deadline_ms: unsafe { host_now_ms() }
            .saturating_add(cfg.wall_budget_ms.min(LISTING_WALK_ANSWERABLE_BUDGET_MS)),
        reached_deadline: false,
        files: Vec::new(),
        excluded: Vec::new(),
    };
    walk.descend(&target);
    let mut files = walk.files;
    let mut listing_complete = !walk.reached_deadline;
    // git contributed no tracked file here (that is why this is a walk at all), so 0 is the count
    // the reply reports: it is the number that names the blind spot.
    let tracked = 0usize;
    let mut untracked = 0usize;
    if let Some((git_files, git_complete)) = git_join {
        listing_complete &= git_complete;
        untracked = git_files.len();
        let mut seen: HashSet<String> = files.iter().cloned().collect();
        for file in git_files {
            if seen.insert(file.clone()) {
                files.push(file);
            }
        }
    }
    Ok(universe(
        files,
        FileSource::Walk,
        listing_complete,
        walk.excluded,
        Some(policy.reason),
        tracked,
        untracked,
        0,
        false,
    ))
}

pub fn project_source_files(root: &str, max_files: usize, cfg: &IndexConfig) -> Vec<String> {
    let bytes = root.as_bytes();
    let absolute = root.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':');
    let (base, scope) = if root.is_empty() || root == "." || absolute {
        (if root.is_empty() { "." } else { root }, None)
    } else {
        (".", Some(root))
    };
    let project_node_modules = join_under(base, "node_modules/");
    let origin = if absolute { TargetOrigin::CallerNamed } else { TargetOrigin::ProjectDefault };
    match list_scan_universe(base, &scope.into_iter().collect::<Vec<&str>>(), max_files, cfg, origin, false, false, false) {
        Ok(u) => u.files.into_iter().filter(|p| !p.starts_with(&project_node_modules)).take(max_files).collect(),
        Err(_) => Vec::new(),
    }
}
