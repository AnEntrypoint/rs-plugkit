use std::collections::HashSet;

use serde_json::Value;

use crate::code_index::{gitignore_excludes, is_dependency_noise_dir_segment, is_hidden_segment, is_skipped_dir_segment, list_dir, load_repo_gitignore};
use crate::ragconfig::IndexConfig;
use crate::wasm_dispatch::{git_call_argv, host_now_ms, host_stat};

const GIT_LISTING_SPLIT_DEPTH_LIMIT: usize = 16;

const NESTED_REPO_DEPTH_LIMIT: usize = 8;

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
}

pub struct RuleExclusion {
    pub path: String,
    pub rule: &'static str,
}

pub struct ScanUniverse {
    pub files: Vec<String>,
    pub source: FileSource,
    pub listing_complete: bool,
    pub excluded: Vec<RuleExclusion>,
    pub walk_reason: Option<String>,
    target: String,
}

impl ScanUniverse {
    pub fn tracked_paths_deleted_from_worktree(&self) -> HashSet<String> {
        if self.source != FileSource::Git { return HashSet::new(); }
        let mut rel = Vec::new();
        match git_list_into(&self.target, &["--deleted"], None, 0, &mut rel) {
            Ok(_) => rel.into_iter().map(|p| join_under(&self.target, &p)).collect(),
            Err(_) => HashSet::new(),
        }
    }
}

pub fn join_under(base: &str, rel: &str) -> String {
    if base.ends_with('/') { format!("{base}{rel}") } else { format!("{base}/{rel}") }
}

fn git_cwd(root: &str) -> Option<&str> {
    if root.is_empty() || root == "." { None } else { Some(root) }
}

fn git_exit_code(v: &Value) -> i64 {
    v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1)
}

fn git_stderr(v: &Value) -> String {
    v.get("stderr").and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
}

fn stat_is_directory(path: &str) -> Option<bool> {
    host_stat(path)
        .filter(|v| !v.is_null())
        .and_then(|v| v.get("isDirectory").and_then(|b| b.as_bool()))
}

fn relative_scope(root: &str, scope: &str) -> Result<Option<String>, String> {
    let normalized = scope.replace('\\', "/");
    let bytes = normalized.as_bytes();
    if normalized.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':') {
        return Err(format!("path '{scope}' must be relative to the search root '{root}' -- pass another project as \"root\", and a location inside it as \"path\""));
    }
    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    if segments.iter().any(|s| *s == "..") {
        return Err(format!("path '{scope}' may not climb out of the search root with '..'"));
    }
    Ok(if segments.is_empty() { None } else { Some(segments.join("/")) })
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

fn git_list_into(cwd_dir: &str, mode: &[&str], pathspec: Option<&str>, depth: usize, out: &mut Vec<String>) -> Result<bool, String> {
    let mut argv = vec!["--literal-pathspecs", "ls-files", "-z"];
    argv.extend_from_slice(mode);
    if let Some(p) = pathspec {
        argv.push("--");
        argv.push(p);
    }
    let r = git_call_argv(&argv, git_cwd(cwd_dir));
    if git_exit_code(&r) != 0 {
        return Err(format!("git {} exited {}: {}", argv.join(" "), git_exit_code(&r), git_stderr(&r)));
    }
    let stdout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let capped = r.get("stdout_truncated").and_then(|x| x.as_bool()).unwrap_or(false);
    if !capped {
        out.extend(stdout.split('\0').filter(|p| !p.is_empty()).map(String::from));
        return Ok(true);
    }
    let listed_dir = pathspec.map(|p| join_under(cwd_dir, p)).unwrap_or_else(|| cwd_dir.to_string());
    if depth >= GIT_LISTING_SPLIT_DEPTH_LIMIT || stat_is_directory(&listed_dir) != Some(true) {
        let mut entries: Vec<&str> = stdout.split('\0').collect();
        entries.pop();
        out.extend(entries.into_iter().filter(|p| !p.is_empty()).map(String::from));
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
        code => Err(format!("git check-ignore exited {code}: {}", git_stderr(&r))),
    }
}

fn git_worktree_files(dir: &str, nesting: usize) -> Result<(Vec<String>, bool), String> {
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    let mut complete = git_list_into(dir, &["--cached", "--recurse-submodules"], None, 0, &mut tracked)?;
    complete &= git_list_into(dir, &["--others", "--exclude-standard"], None, 0, &mut untracked)?;
    let mut files: Vec<String> = tracked.into_iter().map(|p| join_under(dir, &p)).collect();
    for entry in untracked {
        let Some(nested_repo) = entry.strip_suffix('/') else {
            files.push(join_under(dir, &entry));
            continue;
        };
        let nested_dir = join_under(dir, nested_repo);
        match (nesting < NESTED_REPO_DEPTH_LIMIT).then(|| git_worktree_files(&nested_dir, nesting + 1)) {
            Some(Ok((nested_files, nested_complete))) => {
                files.extend(nested_files);
                complete &= nested_complete;
            }
            _ => complete = false,
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok((files, complete))
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
}

struct WalkPolicy {
    honour_gitignore: bool,
    noise: NoiseDirs,
    reason: String,
}

const DEPENDENCY_WALK_SKIPS: &str = "skipping only VCS, dependency-store, cache and tool directories (build-output directories such as dist/ are read)";

fn walk_policy(cause: WalkCause, origin: TargetOrigin) -> WalkPolicy {
    let project = |reason: String| WalkPolicy { honour_gitignore: true, noise: NoiseDirs::ProjectNoiseList, reason };
    match (cause, origin) {
        (WalkCause::GitListingFailed(e), _) => project(format!("git could not list the worktree, so it was walked directly ({e})")),
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
    }
}

struct RuleRecordingWalk<'a> {
    cfg: &'a IndexConfig,
    gitignore: Option<ignore::gitignore::Gitignore>,
    noise: NoiseDirs,
    max_files: usize,
    deadline_ms: u64,
    reached_deadline: bool,
    files: Vec<String>,
    excluded: Vec<RuleExclusion>,
}

impl RuleRecordingWalk<'_> {
    fn descend(&mut self, dir: &str) {
        for entry in list_dir(dir) {
            if self.files.len() >= self.max_files || self.reached_deadline { return; }
            if unsafe { host_now_ms() } >= self.deadline_ms {
                self.reached_deadline = true;
                return;
            }
            let name = entry.rsplit('/').next().unwrap_or(entry.as_str()).to_string();
            if name == ".git" { continue; }
            let next = join_under(dir, &entry);
            let is_dir = stat_is_directory(&next).unwrap_or(false);
            let rule = if self.cfg.is_force_included(&next) { None } else { self.exclusion_rule(&name, &next, is_dir) };
            match (rule, is_dir) {
                (Some(rule), _) => self.excluded.push(RuleExclusion { path: next, rule }),
                (None, true) => self.descend(&next),
                (None, false) => self.files.push(next),
            }
        }
    }

    fn exclusion_rule(&self, name: &str, path: &str, is_dir: bool) -> Option<&'static str> {
        if gitignore_excludes(&self.gitignore, path, is_dir) { return Some("gitignore"); }
        if !is_dir { return None; }
        if is_hidden_segment(name) { return Some("hidden_dir"); }
        let noise = match self.noise {
            NoiseDirs::ProjectNoiseList => is_skipped_dir_segment(name, self.cfg),
            NoiseDirs::DependencyStoresVcsCachesOnly => is_dependency_noise_dir_segment(name, self.cfg),
        };
        noise.then_some("noise_dir_name")
    }
}

pub fn list_scan_universe(root: &str, scope: Option<&str>, max_files: usize, cfg: &IndexConfig, origin: TargetOrigin) -> Result<ScanUniverse, String> {
    let rel = match scope {
        Some(s) => relative_scope(root, s)?,
        None => None,
    };
    let target = rel.as_deref().map(|r| join_under(root, r)).unwrap_or_else(|| root.to_string());
    let universe = |files, source, listing_complete, excluded, walk_reason| ScanUniverse {
        files, source, listing_complete, excluded, walk_reason,
        target: target.clone(),
    };
    if rel.is_some() {
        match stat_is_directory(&target) {
            None => return Err(format!("path '{}' does not exist under search root '{root}'", scope.unwrap_or(""))),
            Some(false) => return Ok(universe(vec![target.clone()], FileSource::SingleFile, true, Vec::new(), None)),
            Some(true) => {}
        }
    }
    let cause = match directory_is_gitignored(&target) {
        Ok(false) => match git_worktree_files(&target, 0) {
            Ok((files, complete)) => return Ok(universe(files, FileSource::Git, complete, Vec::new(), None)),
            Err(e) => WalkCause::GitListingFailed(e),
        },
        Ok(true) => WalkCause::TargetGitignored,
        Err(e) => WalkCause::OutsideWorktree(e),
    };
    let policy = walk_policy(cause, origin);
    let mut walk = RuleRecordingWalk {
        cfg,
        gitignore: if policy.honour_gitignore { load_repo_gitignore(root) } else { None },
        noise: policy.noise,
        max_files,
        deadline_ms: unsafe { host_now_ms() }.saturating_add(cfg.wall_budget_ms),
        reached_deadline: false,
        files: Vec::new(),
        excluded: Vec::new(),
    };
    walk.descend(&target);
    Ok(universe(walk.files, FileSource::Walk, !walk.reached_deadline, walk.excluded, Some(policy.reason)))
}

pub fn project_source_files(root: &str, max_files: usize, cfg: &IndexConfig) -> Vec<String> {
    let bytes = root.as_bytes();
    let absolute = root.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':');
    let (base, scope) = if root.is_empty() || root == "." || absolute { (if root.is_empty() { "." } else { root }, None) } else { (".", Some(root)) };
    let project_node_modules = join_under(base, "node_modules/");
    match list_scan_universe(base, scope, max_files, cfg, TargetOrigin::ProjectDefault) {
        Ok(u) => u.files.into_iter().filter(|p| !p.starts_with(&project_node_modules)).take(max_files).collect(),
        Err(_) => Vec::new(),
    }
}
