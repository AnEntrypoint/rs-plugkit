const KNOWN_SUBMODULES: &[&str] = &[
    "agentplug", "rs-plugkit", "rs-codeinsight", "rs-search",
    "agentplug-bert", "agentplug-libsql", "agentplug-treesitter",
];

const SUBMODULE_GITLINK_MODE: &str = "160000";

#[derive(serde::Serialize)]
pub struct DriftedSubmodule {
    path: String,
    gm_tracked_sha: String,
    submodule_head_sha: String,
}

#[cfg(target_arch = "wasm32")]
fn gm_tracked_gitlink_sha(path: &str) -> Option<String> {
    let result = crate::wasm_dispatch::git_call_argv(&["ls-tree", "HEAD", "--", path], None);
    let stdout = result.get("stdout").and_then(|value| value.as_str()).unwrap_or("");
    let first_line = stdout.lines().next()?;
    let mut fields = first_line.split_whitespace();
    let mode = fields.next()?;
    if mode != SUBMODULE_GITLINK_MODE { return None; }
    let object_type = fields.next()?;
    if object_type != "commit" { return None; }
    fields.next().map(|sha| sha.trim().to_string())
}

#[cfg(target_arch = "wasm32")]
fn submodule_head_sha(path: &str) -> Option<String> {
    let result = crate::wasm_dispatch::git_call_argv(&["rev-parse", "HEAD"], Some(path));
    let exit_code = result.get("exit_code").and_then(|value| value.as_i64()).unwrap_or(1);
    if exit_code != 0 { return None; }
    let stdout = result.get("stdout").and_then(|value| value.as_str()).unwrap_or("").trim();
    if stdout.is_empty() { None } else { Some(stdout.to_string()) }
}

#[cfg(target_arch = "wasm32")]
fn is_ancestor(ancestor: &str, descendant: &str, path: &str) -> bool {
    if ancestor == descendant { return true; }
    let result = crate::wasm_dispatch::git_call_argv(&["merge-base", "--is-ancestor", ancestor, descendant], Some(path));
    result.get("exit_code").and_then(|value| value.as_i64()) == Some(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn gm_tracked_gitlink_sha(_path: &str) -> Option<String> { None }
#[cfg(not(target_arch = "wasm32"))]
fn submodule_head_sha(_path: &str) -> Option<String> { None }
#[cfg(not(target_arch = "wasm32"))]
fn is_ancestor(_ancestor: &str, _descendant: &str, _path: &str) -> bool { false }

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
        let Some((key, value)) = trimmed.split_once('=') else { continue };
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

pub fn drifted_submodules() -> Vec<DriftedSubmodule> {
    let mut drifted = Vec::new();
    for path in submodule_paths() {
        let Some(gm_tracked_sha) = gm_tracked_gitlink_sha(&path) else { continue };
        let Some(submodule_head_sha) = submodule_head_sha(&path) else { continue };
        if gm_tracked_sha != submodule_head_sha && !is_ancestor(&gm_tracked_sha, &submodule_head_sha, &path) {
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
    let recovery_command = if drifted.is_empty() {
        None
    } else {
        let drifted_paths = drifted.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>().join(" ");
        Some(format!(
            "cd back to gm root, `git add {}`, then git_commit/git_finalize to update gm's own tracked pointer(s) to match each submodule's current real HEAD.",
            drifted_paths,
        ))
    };
    let payload = serde_json::json!({
        "ok": true,
        "clean": drifted.is_empty(),
        "drifted": drifted.iter().map(|entry| serde_json::json!({
            "path": entry.path,
            "gm_tracked_sha": entry.gm_tracked_sha,
            "submodule_head_sha": entry.submodule_head_sha,
        })).collect::<Vec<_>>(),
        "recovery": recovery_command,
    });
    (payload.to_string(), String::new(), 0)
}
