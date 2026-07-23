// Mechanizes the AGENTS.md prose rule ("A subagent dispatched to build/
// commit/push in a gm submodule ... closes the loop back to gm's own
// submodule pin as its explicit last step, every time, no exceptions"),
// added after being live-hit twice in one session (two distinct commit
// pairs each needing a manual `git checkout <sha> --detach` + `git_finalize`
// to close). Prose rules of that length and specificity are load-bearing
// failures waiting to recur -- this is the structural version: CONSOLIDATE
// refuses if any submodule's real HEAD differs from gm's own tracked
// gitlink pointer, naming the concrete recovery (git add <path>, commit,
// push) instead of relying on an agent remembering the rule.

const KNOWN_SUBMODULES: &[&str] = &[
    "agentplug", "rs-plugkit", "rs-codeinsight", "rs-search",
    "agentplug-bert", "agentplug-libsql", "agentplug-treesitter",
];

/// One drifted submodule: gm's own tree (via `git ls-tree HEAD -- <path>`)
/// pins a gitlink sha that the submodule's own `git rev-parse HEAD` no
/// longer matches -- either the submodule moved without gm's pointer being
/// updated+committed, or gm's own HEAD is stale relative to a submodule
/// commit that already landed and pushed to its standalone remote.
#[derive(serde::Serialize)]
pub struct DriftedSubmodule {
    path: String,
    gm_tracked_sha: String,
    submodule_head_sha: String,
}

#[cfg(target_arch = "wasm32")]
fn gm_tracked_gitlink(path: &str) -> Option<String> {
    let v = crate::wasm_dispatch::git_call_argv(&["ls-tree", "HEAD", "--", path], None);
    let out = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    // Format: "160000 commit <sha>\t<path>" -- gitlink mode 160000 marks a
    // submodule entry specifically (vs 100644 plain file / 040000 tree), so
    // this line format is only ever emitted for a real submodule path.
    let line = out.lines().next()?;
    let mut parts = line.split_whitespace();
    let mode = parts.next()?;
    if mode != "160000" { return None; }
    let kind = parts.next()?;
    if kind != "commit" { return None; }
    parts.next().map(|s| s.trim().to_string())
}

#[cfg(target_arch = "wasm32")]
fn submodule_head(path: &str) -> Option<String> {
    let v = crate::wasm_dispatch::git_call_argv(&["rev-parse", "HEAD"], Some(path));
    let exit = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(1);
    if exit != 0 { return None; }
    let out = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim();
    if out.is_empty() { None } else { Some(out.to_string()) }
}

#[cfg(not(target_arch = "wasm32"))]
fn gm_tracked_gitlink(_path: &str) -> Option<String> { None }
#[cfg(not(target_arch = "wasm32"))]
fn submodule_head(_path: &str) -> Option<String> { None }

/// Real check: for every known submodule directory that actually exists in
/// this checkout (a plain `git clone` without `--recurse-submodules` leaves
/// them empty -- absence is expected and not drift, see README.md's own
/// documented "Empty submodule directories after a normal git clone are
/// expected" note), compare gm's tracked gitlink against the submodule's own
/// live HEAD. Returns the list of genuinely drifted submodules (empty =
/// clean). A submodule directory that exists but is not a git checkout at
/// all (submodule_head returns None) is treated as clean/skip, not drift --
/// that is a distinct "submodule not initialized" condition, already
/// surfaced by ordinary git tooling, not this gate's job to re-diagnose.
pub fn drifted_submodules() -> Vec<DriftedSubmodule> {
    let mut out = Vec::new();
    for path in KNOWN_SUBMODULES {
        let Some(tracked) = gm_tracked_gitlink(path) else { continue };
        let Some(head) = submodule_head(path) else { continue };
        if tracked != head {
            out.push(DriftedSubmodule {
                path: path.to_string(),
                gm_tracked_sha: tracked,
                submodule_head_sha: head,
            });
        }
    }
    out
}

pub fn submodules_clean() -> bool {
    drifted_submodules().is_empty()
}

pub fn handle_check(_content: &str) -> (String, String, i32) {
    let drifted = drifted_submodules();
    let payload = serde_json::json!({
        "ok": true,
        "clean": drifted.is_empty(),
        "drifted": drifted.iter().map(|d| serde_json::json!({
            "path": d.path, "gm_tracked_sha": d.gm_tracked_sha, "submodule_head_sha": d.submodule_head_sha,
        })).collect::<Vec<_>>(),
        "recovery": if drifted.is_empty() { None } else {
            Some(format!(
                "cd back to gm root, `git add {}`, then git_commit/git_finalize to update gm's own tracked pointer(s) to match each submodule's current real HEAD.",
                drifted.iter().map(|d| d.path.as_str()).collect::<Vec<_>>().join(" "),
            ))
        },
    });
    (payload.to_string(), String::new(), 0)
}
