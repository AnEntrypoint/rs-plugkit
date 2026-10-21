use serde_json::{json, Value};

pub const BLANKET_REFUSAL_LISTED_PATHS: usize = 50;

fn normalize_pathspec(pathspec: &str) -> &str {
    let trimmed = pathspec.trim();
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    trimmed.strip_suffix('/').unwrap_or(trimmed)
}

fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first().copied(), text.first().copied()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob_match(&pattern[1..], text)
                || (matches!(text.first().copied(), Some(c) if c != b'/') && glob_match(pattern, &text[1..]))
        }
        (Some(b'?'), Some(c)) if c != b'/' => glob_match(&pattern[1..], &text[1..]),
        (Some(p), Some(c)) if p == c => glob_match(&pattern[1..], &text[1..]),
        _ => false,
    }
}

pub fn pathspec_covers_path(pathspec: &str, path: &str) -> bool {
    let spec = normalize_pathspec(pathspec);
    let path = path.trim_start_matches("./");
    if spec.is_empty() || spec == "." || path == spec {
        return true;
    }
    if spec.contains('*') || spec.contains('?') {
        return glob_match(spec.as_bytes(), path.as_bytes());
    }
    path.starts_with(&format!("{spec}/"))
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
