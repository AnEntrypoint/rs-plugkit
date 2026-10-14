use serde_json::{json, Value};

pub const BLANKET_REFUSAL_LISTED_PATHS: usize = 50;

pub fn pathspec_covers_path(pathspec: &str, path: &str) -> bool {
    if path == pathspec {
        return true;
    }
    let dir = pathspec.strip_suffix('/').unwrap_or(pathspec);
    path.starts_with(&format!("{dir}/"))
}

pub fn blanket_stage_refusal(verb: &str, paths: &[String]) -> Value {
    let mut listed: Vec<String> = paths
        .iter()
        .take(BLANKET_REFUSAL_LISTED_PATHS)
        .cloned()
        .collect();
    if paths.len() > listed.len() {
        listed.push(format!(
            "... and {} more",
            paths.len() - BLANKET_REFUSAL_LISTED_PATHS
        ));
    }
    json!({
        "error": format!(
            "{} refuses to stage the whole worktree: {} path(s) would be swept into this commit and none of them were named. Another writer's in-flight edits can be among them, and one blanket commit destroys their provenance. Re-dispatch with paths:[...] naming exactly the files you changed and commit the rest separately, or, if you own every one of them, re-dispatch with allow_whole_index: true.",
            verb,
            paths.len()
        ),
        "error_code": "blanket_stage_refused",
        "would_stage_count": paths.len(),
        "would_stage_paths": listed,
        "requested_paths": [],
        "next_dispatch": verb,
    })
}

pub fn foreign_dirt_refusal(verb: &str, requested: &[String], unstaged: &[String]) -> Value {
    let mut listed: Vec<String> = unstaged
        .iter()
        .take(BLANKET_REFUSAL_LISTED_PATHS)
        .cloned()
        .collect();
    if unstaged.len() > listed.len() {
        listed.push(format!(
            "... and {} more",
            unstaged.len() - BLANKET_REFUSAL_LISTED_PATHS
        ));
    }
    json!({
        "error": format!(
            "{} refused under refuse_if_foreign_dirt: {} path(s) inside the named pathspec(s) carry changes the index does not hold: {}. A path-scoped commit stages every dirty hunk of a named file, so an unstaged change may belong to another writer. Nothing was staged or committed. Stage your own changes with git_add and re-dispatch, or leave a path that is not yours out of paths.",
            verb,
            unstaged.len(),
            listed.join(", ")
        ),
        "error_code": "foreign_dirt",
        "requested_paths": requested,
        "unstaged_in_requested": listed,
        "unstaged_count": unstaged.len(),
        "next_dispatch": "git_add",
    })
}

pub fn unrequested_stage_refusal(verb: &str, requested: &[String], extra: &[String]) -> Value {
    json!({
        "error": format!(
            "{} staged {} path(s) outside the requested pathspec(s) {}: {}. Nothing was committed. Add them to paths if they are yours, or commit them separately.",
            verb,
            extra.len(),
            requested.join(", "),
            extra.join(", ")
        ),
        "error_code": "staged_outside_requested_paths",
        "requested_paths": requested,
        "staged_outside_requested": extra,
        "next_dispatch": verb,
    })
}
