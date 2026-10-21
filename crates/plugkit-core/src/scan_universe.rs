use std::collections::HashSet;

use serde_json::Value;

use crate::code_index::{
    gitignore_excludes, is_dependency_noise_dir_segment, is_hidden_segment, is_skipped_dir_segment,
    list_dir, load_codesearchignore, load_nested_codesearchignore, load_repo_gitignore,
    nested_codesearchignore_excludes,
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

pub const GM_STATE_EXCLUSION_RULES: &[&str] = &[OWN_STATE_RULE, "agentplug_kv_cache", "gm_exec_spool"];

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

fn tracked_gm_state(root: &str) -> std::collections::HashSet<String> {
    let mut listed: Vec<String> = Vec::new();
    let _ = git_list_into(root, &["--cached"], Some(".gm"), 0, &mut listed);
    listed.into_iter().map(|rel| join_under(root, &rel)).collect()
}

fn prune_own_state(
    root: &str,
    files: Vec<String>,
    versioned_gm: &std::collections::HashSet<String>,
) -> (Vec<String>, Vec<RuleExclusion>) {
    let mut kept = Vec::with_capacity(files.len());
    let mut pruned: Vec<RuleExclusion> = Vec::new();
    for file in files {
        if versioned_gm.contains(&file) {
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
    pub git_tracked_files: usize,
    pub git_untracked_files: usize,
    pub git_nested_worktrees: usize,
    pub git_nested_untracked_failed: bool,
    pub git_nested_failed_paths: Vec<String>,
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
    let r = git_call_argv(&["check-ignore", "-v", "--", "."], git_cwd(dir));
    match git_exit_code(&r) {
        0 => Ok(r
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .lines()
            .any(|line| !line.trim().is_empty())),
        1 => Ok(false),
        code => Err(format!(
            "git check-ignore exited {code}: {}",
            git_stderr(&r)
        )),
    }
}

const NESTED_WORKTREE_SCAN_DEPTH: usize = 2;
const MAX_NESTED_WORKTREES: usize = 64;

pub(crate) fn nested_worktree_dirs(dir: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut frontier: Vec<(String, usize)> = vec![(dir.to_string(), 0)];
    while let Some((current, depth)) = frontier.pop() {
        if depth >= NESTED_WORKTREE_SCAN_DEPTH || found.len() >= MAX_NESTED_WORKTREES {
            continue;
        }
        for child in child_names(&current) {
            if is_hidden_segment(&child) {
                continue;
            }
            let path = join_under(&current, &child);
            if host_exists(&join_under(&path, ".git")) {
                if !nested_checkout_ignored(dir, &path) {
                    found.push(path);
                }
                continue;
            }
            if host_stat_is_directory(&path) == Some(true) {
                frontier.push((path, depth + 1));
            }
        }
    }
    found
}

fn nested_checkout_ignored(dir: &str, checkout: &str) -> bool {
    let rel = checkout.strip_prefix(dir).unwrap_or(checkout).trim_matches('/');
    git_exit_code(&git_call_argv(&["check-ignore", "-q", "--", rel], git_cwd(dir))) == 0
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
            (dirs, !c)
        }
        Err(_) => {
            *complete = false;
            (Vec::new(), true)
        }
    }
}

fn nested_untracked(dir: &str, nesting: usize, no_ignore: bool, failed: &mut Vec<String>) -> (Vec<String>, bool) {
    if nesting >= NESTED_REPO_DEPTH_LIMIT {
        failed.push(dir.to_string());
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
    if !complete {
        failed.push(dir.to_string());
    }
    let mut files = Vec::with_capacity(listed.len());
    for entry in listed {
        match entry.strip_suffix('/') {
            Some(nested_repo) => {
                let (nested_files, nested_complete) =
                    nested_untracked(&join_under(dir, nested_repo), nesting + 1, no_ignore, failed);
                files.extend(nested_files);
                complete &= nested_complete;
            }
            None => files.push(join_under(dir, &entry)),
        }
    }
    let (gitlink_nested, gitlink_failed) = gitlink_dirs(dir, &mut complete);
    if gitlink_failed {
        complete = false;
        failed.push(dir.to_string());
    }
    for nested_dir in gitlink_nested {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore, failed);
        files.extend(nested_files);
        complete &= nested_complete;
    }
    (files, complete)
}

fn git_worktree_files(dir: &str, nesting: usize, no_ignore: bool, failed: &mut Vec<String>) -> Result<(Vec<String>, bool, usize, usize, usize, bool), String> {
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    let others_mode: &[&str] = if no_ignore { &["--others"] } else { &["--others", "--exclude-standard"] };
    let mut complete = git_list_into(dir, &["--cached", "--recurse-submodules"], None, 0, &mut tracked)?;
    complete &= git_list_into(dir, others_mode, None, 0, &mut untracked)?;
    let mut tracked_count = tracked.len();
    let mut nested_worktrees = 0usize;
    let mut nested_untracked_failed = false;
    let mut files: Vec<String> = tracked.into_iter().map(|p| join_under(dir, &p)).collect();
    for entry in untracked {
        let Some(nested_repo) = entry.strip_suffix('/') else {
            files.push(join_under(dir, &entry));
            continue;
        };
        let nested_dir = join_under(dir, nested_repo);
        match (nesting < NESTED_REPO_DEPTH_LIMIT).then(|| git_worktree_files(&nested_dir, nesting + 1, no_ignore, failed)) {
            Some(Ok((nested_files, nested_complete, nested_tracked, _, nested_dirs, nested_failed))) => {
                files.extend(nested_files);
                complete &= nested_complete;
                nested_untracked_failed |= nested_failed;
                tracked_count += nested_tracked;
                nested_worktrees += nested_dirs;
            }
            _ => {
                complete = false;
                nested_untracked_failed = true;
                failed.push(nested_dir.clone());
            }
        }
    }
    let (mut nested_dirs, gitlink_failed) = gitlink_dirs(dir, &mut complete);
    for found in nested_worktree_dirs(dir) {
        if !nested_dirs.iter().any(|d| *d == found) {
            nested_dirs.push(found);
        }
    }
    let nested_set_capped = nested_dirs.len() >= MAX_NESTED_WORKTREES;
    if gitlink_failed || nested_set_capped {
        failed.push(dir.to_string());
    }
    nested_untracked_failed |= gitlink_failed || nested_set_capped;
    nested_worktrees += nested_dirs.len();
    for nested_dir in nested_dirs {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore, failed);
        if !nested_complete {
            nested_untracked_failed = true;
        }
        files.extend(nested_files);
    }
    files.sort_unstable();
    files.dedup();
    let untracked_count = files.len().saturating_sub(tracked_count.min(files.len()));
    Ok((files, complete && !nested_untracked_failed, tracked_count, untracked_count, nested_worktrees, nested_untracked_failed))
}

fn codesearchignore_rule(gi: &Option<ignore::gitignore::Gitignore>, path: &str) -> Option<&'static str> {
    let matcher = gi.as_ref()?;
    matcher.matched_path_or_any_parents(path, false).is_ignore().then_some("codesearchignore")
}

fn load_nested_codesearchignores(root: &str, files: &[String]) -> Vec<(String, ignore::gitignore::Gitignore)> {
    let root = root.trim_end_matches(['/', '\\']);
    files
        .iter()
        .filter_map(|file| {
            let dir = file.strip_suffix("/.codesearchignore")?;
            if dir == root {
                return None;
            }
            Some((dir.to_string(), load_nested_codesearchignore(dir)?))
        })
        .collect()
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

pub const NESTED_WORKTREE_BLIND_SPOT_NOTE: &str = "a nested worktree's untracked listing is incomplete, so a zero is not a finding; pass \"refresh\": true or scope \"path\"";

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
    root: String,
    gitignore: Option<ignore::gitignore::Gitignore>,
    honour_codesearchignore: bool,
    nested_codesearchignore: Vec<(String, ignore::gitignore::Gitignore)>,
    noise: NoiseDirs,
    named_scope: bool,
    named_target: String,
    granted_root: Option<String>,
    max_files: usize,
    deadline_ms: u64,
    reached_deadline: bool,
    reached_cap: bool,
    files: Vec<String>,
    excluded: Vec<RuleExclusion>,
}

impl RuleRecordingWalk<'_> {
    fn descend(&mut self, dir: &str) {
        let entries = list_dir(dir);
        let nested = if self.honour_codesearchignore
            && dir.trim_end_matches(['/', '\\']) != self.root.trim_end_matches(['/', '\\'])
            && entries.iter().any(|entry| entry == ".codesearchignore")
        {
            load_nested_codesearchignore(dir)
        } else {
            None
        };
        let pushed = nested.is_some();
        if let Some(gi) = nested {
            self.nested_codesearchignore.push((dir.to_string(), gi));
        }
        self.descend_entries(dir, entries);
        if pushed {
            self.nested_codesearchignore.pop();
        }
    }

    fn descend_entries(&mut self, dir: &str, entries: Vec<String>) {
        let children_of_granted_root = self.granted_root.as_deref() == Some(dir);
        for entry in entries {
            if self.reached_cap || self.reached_deadline {
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
                self.exclusion_rule(&name, &next, is_dir, children_of_granted_root)
            };
            match (rule, is_dir) {
                (Some(rule), _) => self.excluded.push(RuleExclusion {
                    path: next,
                    rule,
                    files: None,
                }),
                (None, true) => self.descend(&next),
                (None, false) => {
                    if self.files.len() >= self.max_files {
                        self.reached_cap = true;
                        return;
                    }
                    self.files.push(next);
                }
            }
        }
    }

    fn exclusion_rule(&self, name: &str, path: &str, is_dir: bool, granted_child: bool) -> Option<&'static str> {
        if self.named_target_is_at_or_above(path) { return None; }
        if gitignore_excludes(&self.gitignore, path, is_dir) { return Some("gitignore"); }
        if nested_codesearchignore_excludes(&self.nested_codesearchignore, path, is_dir) { return Some("codesearchignore"); }
        if !is_dir { return None; }
        if is_own_state_name(name) { return Some(OWN_STATE_RULE); }
        if is_hidden_segment(name) { return Some("hidden_dir"); }
        let noise = match self.noise {
            NoiseDirs::ProjectNoiseList => is_skipped_dir_segment(name, self.cfg),
            NoiseDirs::DependencyStoresVcsCachesOnly => {
                !granted_child && is_dependency_noise_dir_segment(name, self.cfg)
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
        acc.git_nested_failed_paths.extend(next.git_nested_failed_paths);
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
    let universe = |files: Vec<String>, source: FileSource, listing_complete: bool, excluded: Vec<RuleExclusion>, walk_reason: Option<String>, tracked: usize, untracked: usize, nested: usize, nested_failed: bool, nested_failed_paths: Vec<String>| ScanUniverse {
        files,
        source,
        listing_complete: listing_complete && !nested_failed,
        excluded,
        walk_reason,
        git_tracked_files: tracked,
        git_untracked_files: untracked,
        git_nested_worktrees: nested,
        git_nested_untracked_failed: nested_failed,
        git_nested_failed_paths: nested_failed_paths,
        target: target.clone(),
    };
    if named_scope {
        match host_stat_is_directory(&target) {
            None => return Err(format!(
                "path '{}' does not exist under search root '{}' -- paths resolve relative to that root, which is the dispatch project unless `root` names another directory; if the path lives in a different project, pass that project's directory as `root` (or dispatch with its cwd)",
                scope.unwrap_or(""),
                absolute_root_for_message(root),
            )),
            Some(false) => return Ok(universe(vec![target.clone()], FileSource::SingleFile, true, Vec::new(), None, 0, 0, 0, false, Vec::new())),
            Some(true) => {}
        }
    }
    let mut git_join: Option<(Vec<String>, bool, usize, bool)> = None;
    let mut nested_failed_paths: Vec<String> = Vec::new();
    let cause = if force_disk {
        WalkCause::CallerForcedDisk
    } else {
        match directory_is_gitignored(&target) {
            Ok(false) => match git_worktree_files(&target, 0, no_ignore, &mut nested_failed_paths) {
                Ok((files, complete, tracked, untracked, nested_dirs, nested_failed)) => {
                    let (files, mut pruned) = if named_scope {
                        (files, Vec::new())
                    } else {
                        let versioned_gm = if keep_versioned_gm { tracked_gm_state(root) } else { Default::default() };
                        let (kept, mut pruned) = prune_own_state(root, files, &versioned_gm);
                        report_unlisted_own_state(root, &kept, &mut pruned);
                        (kept, pruned)
                    };
                    let codesearchignore = if no_ignore { None } else { load_codesearchignore(root) };
                    let nested_codesearchignore = if no_ignore { Vec::new() } else { load_nested_codesearchignores(root, &files) };
                    let mut kept = Vec::with_capacity(files.len());
                    for path in files {
                        let rule = runtime_artifact_rule(&path)
                            .or_else(|| codesearchignore_rule(&codesearchignore, &path))
                            .or_else(|| nested_codesearchignore_excludes(&nested_codesearchignore, &path, false).then_some("codesearchignore"));
                        match rule {
                            Some(rule) => pruned.push(RuleExclusion {
                                path,
                                rule,
                                files: None,
                            }),
                            None => kept.push(path),
                        }
                    }
                    if tracked > 0 {
                        return Ok(universe(kept, FileSource::Git, complete, pruned, None, tracked, untracked, nested_dirs, nested_failed, std::mem::take(&mut nested_failed_paths)));
                    }
                    git_join = Some((kept, complete, nested_dirs, nested_failed));
                    WalkCause::GitTracksNothing
                }
                Err(e) => WalkCause::GitListingFailed(e),
            },
            Ok(true) => WalkCause::TargetGitignored,
            Err(e) => WalkCause::OutsideWorktree(e),
        }
    };
    let policy = walk_policy(cause, origin, no_ignore);
    let granted_root = (origin == TargetOrigin::CallerNamed && policy.noise == NoiseDirs::DependencyStoresVcsCachesOnly).then(|| target.clone());
    let mut walk = RuleRecordingWalk {
        cfg,
        root: root.to_string(),
        gitignore: if policy.honour_gitignore {
            load_repo_gitignore(root)
        } else {
            None
        },
        honour_codesearchignore: policy.honour_gitignore,
        nested_codesearchignore: Vec::new(),
        noise: policy.noise,
        named_scope,
        named_target: target.clone(),
        granted_root,
        max_files,
        deadline_ms: unsafe { host_now_ms() }
            .saturating_add(cfg.wall_budget_ms.min(LISTING_WALK_ANSWERABLE_BUDGET_MS)),
        reached_deadline: false,
        reached_cap: false,
        files: Vec::new(),
        excluded: Vec::new(),
    };
    walk.descend(&target);
    let mut files = walk.files;
    let mut listing_complete = !walk.reached_deadline && !walk.reached_cap;
    let tracked = 0usize;
    let mut untracked = 0usize;
    let mut nested = 0usize;
    let mut nested_failed = false;
    if let Some((git_files, git_complete, git_nested, git_nested_failed)) = git_join {
        listing_complete &= git_complete;
        untracked = git_files.len();
        nested = git_nested;
        nested_failed = git_nested_failed;
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
        nested,
        nested_failed,
        nested_failed_paths,
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
