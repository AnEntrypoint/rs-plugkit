use std::path::Path;

/// Recall episodes from rs-learn. Returns formatted text or empty string on failure.
pub fn recall_disc(query: &str, project_dir: &str, limit: u32, discipline: Option<&str>) -> String {
    let project_path = Path::new(project_dir);

    // Attempt HTTP call to rs-learn daemon first (127.0.0.1:4800)
    if let Some(result) = try_recall_http(query, limit, discipline, project_path) {
        return result;
    }

    // Fallback to reading AGENTS.md
    fallback_recall_from_agents_md(query)
}

/// Ingest a fact into rs-learn (detached, non-blocking).
pub fn ingest_fast_disc(content: &str, source: &str, project_dir: &str, discipline: Option<&str>) {
    let project_path = Path::new(project_dir);

    // Try HTTP call to rs-learn daemon
    if try_ingest_http(content, source, discipline, project_path).is_ok() {
        return;
    }

    // Fallback to AGENTS.md append (future: implement)
    eprintln!("[learn] ingest failed: rs-learn unavailable, AGENTS.md fallback not yet implemented");
}

/// Forget/unlearn episodes from rs-learn.
pub fn forget_disc(kind: &str, target: &str, project_dir: &str, discipline: Option<&str>) -> anyhow::Result<u32> {
    let project_path = Path::new(project_dir);

    // Try HTTP call
    if let Some(count) = try_forget_http(kind, target, discipline, project_path) {
        return Ok(count);
    }

    Err(anyhow::anyhow!("forget failed: rs-learn unavailable"))
}

/// Pass-through to rs-learn for status/debug/feedback/build-communities.
pub fn learn_passthrough_disc(action: &str, rest: &[String], project_dir: &str, discipline: Option<&str>) -> String {
    let project_path = Path::new(project_dir);

    // Try HTTP call
    if let Some(result) = try_learn_passthrough_http(action, rest, discipline, project_path) {
        return result;
    }

    String::new()
}

/// Attempt recall via HTTP RPC to rs-learn daemon (127.0.0.1:4800).
fn try_recall_http(query: &str, limit: u32, _discipline: Option<&str>, _project_path: &Path) -> Option<String> {
    // TODO: Implement HTTP client call to rs-learn daemon
    // For now: None to trigger fallback
    None
}

/// Attempt ingest via HTTP RPC to rs-learn daemon.
fn try_ingest_http(content: &str, source: &str, _discipline: Option<&str>, _project_path: &Path) -> Result<(), String> {
    // TODO: Implement HTTP client call to rs-learn daemon
    Err("rs-learn unavailable".into())
}

/// Attempt forget via HTTP RPC to rs-learn daemon.
fn try_forget_http(kind: &str, target: &str, _discipline: Option<&str>, _project_path: &Path) -> Option<u32> {
    // TODO: Implement HTTP client call to rs-learn daemon
    None
}

/// Attempt passthrough via HTTP RPC to rs-learn daemon.
fn try_learn_passthrough_http(action: &str, rest: &[String], _discipline: Option<&str>, _project_path: &Path) -> Option<String> {
    // TODO: Implement HTTP client call to rs-learn daemon
    None
}

/// Fallback: read AGENTS.md and search for matching entries.
fn fallback_recall_from_agents_md(query: &str) -> String {
    // TODO: Implement AGENTS.md fallback recall
    // For now return empty to signal failure
    String::new()
}
