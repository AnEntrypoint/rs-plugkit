const KNOWN_SUBMODULES: &[&str] = &[
    "agentplug",
    "rs-plugkit",
    "rs-codeinsight",
    "rs-search",
    "agentplug-bert",
    "agentplug-libsql",
    "agentplug-treesitter",
];

const SUBMODULE_GITLINK_MODE: &str = "160000";

const PIN_HISTORY_SEARCH_DEPTH: usize = 50;

#[derive(serde::Serialize)]
pub struct DriftedSubmodule {
    path: String,
    gm_tracked_sha: String,
    submodule_head_sha: String,
}

#[derive(serde::Serialize)]
pub struct PinRegression {
    path: String,
    old_sha: String,
    new_sha: String,
    where_found: String,
}

#[cfg(target_arch = "wasm32")]
fn call_stdout(argv: &[&str], cwd: Option<&str>) -> String {
    crate::wasm_dispatch::git_call_argv(argv, cwd)
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(not(target_arch = "wasm32"))]
fn call_stdout(_argv: &[&str], _cwd: Option<&str>) -> String {
    String::new()
}

fn parse_tree_gitlink(stdout: &str) -> Option<String> {
    let mut fields = stdout.lines().next()?.split_whitespace();
    if fields.next()? != SUBMODULE_GITLINK_MODE {
        return None;
    }
    if fields.next()? != "commit" {
        return None;
    }
    fields.next().map(|sha| sha.trim().to_string())
}

fn parse_index_gitlink(stdout: &str) -> Option<String> {
    let mut fields = stdout.lines().next()?.split_whitespace();
    if fields.next()? != SUBMODULE_GITLINK_MODE {
        return None;
    }
    fields.next().map(|sha| sha.trim().to_string())
}

fn gm_tracked_gitlink_sha(path: &str) -> Option<String> {
    parse_tree_gitlink(&call_stdout(&["ls-tree", "HEAD", "--", path], None))
}

fn staged_gitlink_sha(path: &str) -> Option<String> {
    parse_index_gitlink(&call_stdout(&["ls-files", "--stage", "--", path], None))
}

fn pin_history_commits(path: &str) -> Vec<String> {
    let limit = format!("-n{}", PIN_HISTORY_SEARCH_DEPTH);
    call_stdout(&["log", limit.as_str(), "--format=%H", "--", path], None)
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn submodule_head_sha(path: &str) -> Option<String> {
    if !crate::pkfs::exists(&format!("{}/.git", path)) {
        return None;
    }
    let result = crate::wasm_dispatch::git_call_argv(&["rev-parse", "HEAD"], Some(path));
    let exit_code = result
        .get("exit_code")
        .and_then(|value| value.as_i64())
        .unwrap_or(1);
    if exit_code != 0 {
        return None;
    }
    let stdout = result
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout.to_string())
    }
}

#[cfg(target_arch = "wasm32")]
fn is_ancestor(ancestor: &str, descendant: &str, path: &str) -> bool {
    if ancestor == descendant {
        return true;
    }
    let result = crate::wasm_dispatch::git_call_argv(
        &["merge-base", "--is-ancestor", ancestor, descendant],
        Some(path),
    );
    result.get("exit_code").and_then(|value| value.as_i64()) == Some(0)
}

#[cfg(target_arch = "wasm32")]
fn commit_present(path: &str, sha: &str) -> bool {
    let result = crate::wasm_dispatch::git_call_argv(
        &["cat-file", "-e", &format!("{}^{{commit}}", sha)],
        Some(path),
    );
    result.get("exit_code").and_then(|value| value.as_i64()) == Some(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn submodule_head_sha(_path: &str) -> Option<String> {
    None
}
#[cfg(not(target_arch = "wasm32"))]
fn is_ancestor(_ancestor: &str, _descendant: &str, _path: &str) -> bool {
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn commit_present(_path: &str, _sha: &str) -> bool {
    false
}

fn descendant_in_submodule(path: &str, old_sha: &str, new_sha: &str) -> bool {
    pin_direction_known(path, old_sha, new_sha) && is_ancestor(old_sha, new_sha, path)
}

fn pin_direction_known(path: &str, old_sha: &str, new_sha: &str) -> bool {
    commit_present(path, old_sha) && commit_present(path, new_sha)
}



pub fn submodule_paths() -> Vec<String> {
    let parsed = crate::pkfs::read_to_string(".gitmodules")
        .map(|raw| parse_gitmodules_paths(&raw))
        .unwrap_or_default();
    if parsed.is_empty() {
        return KNOWN_SUBMODULES.iter().map(|s| s.to_string()).collect();
    }
    parsed
}

fn parse_gitmodules_paths(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let path = value.trim().trim_matches('"').trim_end_matches('/');
        if !path.is_empty() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

pub fn pin_regressions() -> Vec<PinRegression> {
    let mut out = Vec::new();
    for path in submodule_paths() {
        let Some(pinned) = gm_tracked_gitlink_sha(&path) else {
            continue;
        };
        if let Some(staged) = staged_gitlink_sha(&path) {
            if staged != pinned
                && pin_direction_known(&path, &pinned, &staged)
                && !descendant_in_submodule(&path, &pinned, &staged)
            {
                out.push(PinRegression {
                    path: path.clone(),
                    old_sha: pinned.clone(),
                    new_sha: staged,
                    where_found: "the index -- the pin the next commit writes".to_string(),
                });
            }
        }
        for commit in pin_history_commits(&path) {
            let Some(previous) = parse_tree_gitlink(&call_stdout(&["ls-tree", commit.as_str(), "--", path.as_str()], None)) else {
                continue;
            };
            if previous == pinned {
                continue;
            }
            if descendant_in_submodule(&path, &pinned, &previous) {
                out.push(PinRegression {
                    path: path.clone(),
                    old_sha: previous,
                    new_sha: pinned.clone(),
                    where_found: format!("parent commit {} -- already published", commit),
                });
            }
            break;
        }
    }
    out
}

pub fn pin_regression_message(entry: &PinRegression) -> String {
    format!(
        "[submodule-pin-regression] SUBMODULE PIN MOVED BACKWARDS: {} -- old {}, new {}, and {} is NOT a descendant of {} ({}). This repo is pointing at a commit that does not contain one it already pointed at, which is what a stale read of the submodule tip looks like when another lane published after this one read it: that lane's commit is dropped from the pin. Not refused -- the pin is written as asked. Re-read the tip (git -C {} fetch origin, then git -C {} rev-parse origin/main) and bump the pin forward, or leave it if the older commit is deliberate.",
        entry.path,
        entry.old_sha,
        entry.new_sha,
        entry.new_sha,
        entry.old_sha,
        entry.where_found,
        entry.path,
        entry.path,
    )
}

pub fn drifted_submodules() -> Vec<DriftedSubmodule> {
    let mut drifted = Vec::new();
    for path in submodule_paths() {
        let Some(gm_tracked_sha) = gm_tracked_gitlink_sha(&path) else {
            continue;
        };
        let Some(submodule_head_sha) = submodule_head_sha(&path) else {
            continue;
        };
        if gm_tracked_sha != submodule_head_sha
            && pin_direction_known(&path, &gm_tracked_sha, &submodule_head_sha)
            && !is_ancestor(&gm_tracked_sha, &submodule_head_sha, &path)
        {
            drifted.push(DriftedSubmodule {
                path,
                gm_tracked_sha,
                submodule_head_sha,
            });
        }
    }
    drifted
}

pub fn submodules_clean() -> bool {
    drifted_submodules().is_empty()
}

pub fn handle_check(_content: &str) -> (String, String, i32) {
    let drifted = drifted_submodules();
    let regressions = pin_regressions();
    let recovery_command = if drifted.is_empty() {
        None
    } else {
        let drifted_paths = drifted
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!(
            "cd back to gm root, `git add {}`, then git_commit/git_finalize to update gm's own tracked pointer(s) to match each submodule's current real HEAD.",
            drifted_paths,
        ))
    };
    let warnings: Vec<String> = regressions.iter().map(pin_regression_message).collect();
    let payload = serde_json::json!({
        "ok": true,
        "clean": drifted.is_empty(),
        "drifted": drifted.iter().map(|entry| serde_json::json!({
            "path": entry.path,
            "gm_tracked_sha": entry.gm_tracked_sha,
            "submodule_head_sha": entry.submodule_head_sha,
        })).collect::<Vec<_>>(),
        "recovery": recovery_command,
        "pin_regressions": regressions.iter().map(|entry| serde_json::json!({
            "path": entry.path,
            "old_sha": entry.old_sha,
            "new_sha": entry.new_sha,
            "where_found": entry.where_found,
        })).collect::<Vec<_>>(),
        "pin_regression_count": regressions.len(),
        "pin_regression_warning": if warnings.is_empty() { None } else { Some(warnings.join("\n")) },
    });
    (payload.to_string(), String::new(), 0)
}
