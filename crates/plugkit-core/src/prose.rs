use crate::pkfs;

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

pub fn resolve_and_mark(key: &str, default: &str) -> String {
    let text = resolve(key, default);
    let marker = serde_json::json!({ "key": key, "ts": crate::orchestrator::state::now_ms() });
    let _ = pkfs::write(
        ".gm/exec-spool/.last-gate-fired.json",
        &serde_json::to_string(&marker).unwrap_or_default(),
    );
    text
}

fn read_clean(path: &str) -> Option<String> {
    let raw = pkfs::read_to_string(path)?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if text.trim().is_empty() { None } else { Some(text) }
}

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
