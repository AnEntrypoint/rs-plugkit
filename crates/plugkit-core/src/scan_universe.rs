use std::collections::HashSet;

use serde_json::Value;

use crate::code_index::{
    gitignore_excludes, is_dependency_noise_dir_segment, is_hidden_segment, is_skipped_dir_segment,
    list_dir, load_repo_gitignore,
};
use crate::ragconfig::IndexConfig;
use crate::wasm_dispatch::{git_call_argv, host_now_ms, host_stat_is_directory};

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

fn gitlink_dirs(dir: &str, complete: &mut bool) -> Vec<String> {
    let mut staged = Vec::new();
    match git_list_into(dir, &["--stage"], None, 0, &mut staged) {
        Ok(c) => {
            *complete &= c;
            staged
                .iter()
                .filter_map(|entry| {
                    let after_mode = entry.strip_prefix("160000 ")?;
                    let path = after_mode.split('\t').nth(1)?;
                    (!path.is_empty()).then(|| join_under(dir, path))
                })
                .collect()
        }
        Err(_) => {
            *complete = false;
            Vec::new()
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
    for nested_dir in gitlink_dirs(dir, &mut complete) {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore);
        files.extend(nested_files);
        complete &= nested_complete;
    }
    (files, complete)
}

fn git_worktree_files(dir: &str, nesting: usize, no_ignore: bool) -> Result<(Vec<String>, bool), String> {
    let mut tracked = Vec::new();
    let mut untracked = Vec::new();
    let others_mode: &[&str] = if no_ignore { &["--others"] } else { &["--others", "--exclude-standard"] };
    let mut complete = git_list_into(dir, &["--cached", "--recurse-submodules"], None, 0, &mut tracked)?;
    complete &= git_list_into(dir, others_mode, None, 0, &mut untracked)?;
    let mut files: Vec<String> = tracked.into_iter().map(|p| join_under(dir, &p)).collect();
    for entry in untracked {
        let Some(nested_repo) = entry.strip_suffix('/') else {
            files.push(join_under(dir, &entry));
            continue;
        };
        let nested_dir = join_under(dir, nested_repo);
        match (nesting < NESTED_REPO_DEPTH_LIMIT).then(|| git_worktree_files(&nested_dir, nesting + 1, no_ignore)) {
            Some(Ok((nested_files, nested_complete))) => {
                files.extend(nested_files);
                complete &= nested_complete;
            }
            _ => complete = false,
        }
    }
    for nested_dir in gitlink_dirs(dir, &mut complete) {
        let (nested_files, nested_complete) = nested_untracked(&nested_dir, nesting + 1, no_ignore);
        files.extend(nested_files);
        complete &= nested_complete;
    }
    files.sort_unstable();
    files.dedup();
    Ok((files, complete))
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
    CallerForcedDisk,
}

struct WalkPolicy {
    honour_gitignore: bool,
    noise: NoiseDirs,
    reason: String,
}

const DEPENDENCY_WALK_SKIPS: &str = "skipping only VCS, dependency-store, cache and tool directories (build-output directories such as dist/ are read)";

const NO_IGNORE_WALK_NOTE: &str = "\"no_ignore\": true, so .gitignore/.codesearchignore rules were not applied";

fn walk_policy(cause: WalkCause, origin: TargetOrigin, no_ignore: bool) -> WalkPolicy {
    let project = |reason: String| WalkPolicy { honour_gitignore: true, noise: NoiseDirs::ProjectNoiseList, reason };
    let mut policy = match (cause, origin) {
        (WalkCause::GitListingFailed(e), _) => project(format!("git could not list the worktree, so it was walked directly ({e})")),
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
        if self.named_scope { return None; }
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
    let universe = |files, source, listing_complete, excluded, walk_reason| ScanUniverse {
        files,
        source,
        listing_complete,
        excluded,
        walk_reason,
        target: target.clone(),
    };
    if named_scope {
        match host_stat_is_directory(&target) {
            None => return Err(format!(
                "path '{}' does not exist under search root '{}' -- paths resolve relative to that root, which is the dispatch project unless `root` names another directory; if the path lives in a different project, pass that project's directory as `root` (or dispatch with its cwd)",
                scope.unwrap_or(""),
                absolute_root_for_message(root),
            )),
            Some(false) => return Ok(universe(vec![target.clone()], FileSource::SingleFile, true, Vec::new(), None)),
            Some(true) => {}
        }
    }
    let cause = if force_disk {
        WalkCause::CallerForcedDisk
    } else {
        match directory_is_gitignored(&target) {
            Ok(false) => match git_worktree_files(&target, 0, no_ignore) {
                Ok((files, complete)) => {
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
                    return Ok(universe(kept, FileSource::Git, complete, pruned, None));
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
        max_files,
        deadline_ms: unsafe { host_now_ms() }
            .saturating_add(cfg.wall_budget_ms.min(LISTING_WALK_ANSWERABLE_BUDGET_MS)),
        reached_deadline: false,
        files: Vec::new(),
        excluded: Vec::new(),
    };
    walk.descend(&target);
    Ok(universe(
        walk.files,
        FileSource::Walk,
        !walk.reached_deadline,
        walk.excluded,
        Some(policy.reason),
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
