use crate::pkfs;

/// Three-tier instruction prose resolution, each tier overriding the one
/// before, evaluated per-key so a project can mix all three simultaneously
/// (e.g. vendor just `plan.md` locally while pulling everything else from a
/// shared org source repo): (1) `.gm/instructions/<key>.md` local vendored
/// file -- always wins, a project's explicit per-key freeze; (2) a
/// configured source-repo's synced copy, if `.gm/instructions/source.json`
/// exists and the daemon's git-mirror sync (agentplug-runner::daemon::
/// sync_instruction_source_if_configured) has produced a cache; (3) the
/// compiled-in default (this repo's own prose), the final fallback that
/// makes an unconfigured project behave exactly as before this tier existed.
pub fn resolve(key: &str, default: &str) -> String {
    let local_path = format!(".gm/instructions/{}.md", key);
    if let Some(text) = read_clean(&local_path) {
        return text;
    }
    if let Some(text) = read_from_source_repo(key) {
        return text;
    }
    default.to_string()
}

fn read_clean(path: &str) -> Option<String> {
    let raw = pkfs::read_to_string(path)?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if text.trim().is_empty() { None } else { Some(text) }
}

/// `path` in source.json is a subdirectory WITHIN the cloned repo where the
/// prose files live (e.g. a repo that keeps instructions/ alongside other
/// content, matching this repo's own gm-plugkit/instructions/ layout) --
/// empty string means the prose files sit at the cloned repo's root. The
/// sync step (daemon-side, native host) only clones/fetches; it does not
/// know or care about individual keys, so this read-side function is the
/// only place that resolves `path` + `key` into a concrete file.
fn read_from_source_repo(key: &str) -> Option<String> {
    let cfg_raw = pkfs::read_to_string(".gm/instructions/source.json")?;
    let cfg: serde_json::Value = serde_json::from_str(&cfg_raw).ok()?;
    let sub_path = cfg.get("path").and_then(|v| v.as_str()).unwrap_or("").trim_matches('/');
    let base = ".gm/instructions-source-cache";
    let full = if sub_path.is_empty() {
        format!("{base}/{key}.md")
    } else {
        format!("{base}/{sub_path}/{key}.md")
    };
    read_clean(&full)
}
