// rs-learn integration helpers — append-only context augmentation, counter-driven deep cycles.
// All operations are best-effort: failures return empty strings rather than blocking hooks.
//
// Transport priority:
//   1. HTTP — if `rs-learn serve` is running (env RS_LEARN_HTTP_URL or default :8000), use it.
//      Single shared embedder, ~5ms latency. Best for hot hooks.
//   2. bun x rs-learn — always-available subprocess fallback. ~500ms latency.

use std::{env, fs, io::{Read, Write}, net::TcpStream, path::PathBuf, process::Command, time::Duration};
use super::no_window_cmd;

const DEFAULT_RECALL_TIMEOUT_SECS: u64 = 6;
const HTTP_DEFAULT_URL: &str = "http://127.0.0.1:8000";

fn project_slug(project_dir: &str) -> String {
    let mut h: u64 = 5381;
    for b in project_dir.bytes() { h = h.wrapping_mul(33).wrapping_add(b as u64); }
    format!("{:016x}", h)
}

/// Shared per-project state dir: `<project>/.gm/`. Created on demand.
/// Centralizes counters, learning snapshots, trajectory drafts so the project
/// owns its tooling state and can git-track it (or .gitignore it) deliberately.
fn gm_state_dir(project_dir: &str) -> PathBuf {
    let dir = std::path::Path::new(project_dir).join(".gm");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn counter_path(project_dir: &str) -> PathBuf {
    // Falls back to temp_dir if project has no writable .gm (e.g. read-only checkout).
    let candidate = gm_state_dir(project_dir).join("rslearn-counter.json");
    if candidate.parent().map(|p| p.exists()).unwrap_or(false) { return candidate; }
    env::temp_dir().join(format!("gm-rslearn-counter-{}.json", project_slug(project_dir)))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct CycleCounter {
    prompts: u64,
    last_communities: u64,
    last_trajectory: u64,
    last_debug: u64,
}

fn read_counter(p: &std::path::Path) -> CycleCounter {
    fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_counter(p: &std::path::Path, c: &CycleCounter) {
    let _ = fs::write(p, serde_json::to_string_pretty(c).unwrap_or_default());
}

fn run_bun_rs_learn(args: &[&str], cwd: &str, timeout: Duration) -> String {
    let bun = match which::which("bun.exe").or_else(|_| which::which("bun")) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let mut full_args: Vec<&str> = vec!["x", "rs-learn"];
    full_args.extend_from_slice(args);
    let child = match no_window_cmd(&bun)
        .args(&full_args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    wait_with_timeout(child, timeout)
}

fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut so = Vec::new();
                if let Some(mut o) = child.stdout.take() { let _ = std::io::Read::read_to_end(&mut o, &mut so); }
                return String::from_utf8_lossy(&so).trim().to_string();
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return String::new();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return String::new(),
        }
    }
}

fn spawn_detached_bun_rs_learn(args: &[&str], cwd: &str) {
    let Ok(bun) = which::which("bun.exe").or_else(|_| which::which("bun")) else { return };
    let mut full_args: Vec<&str> = vec!["x", "rs-learn"];
    full_args.extend_from_slice(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let _ = Command::new(&bun)
            .args(&full_args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new(&bun)
            .args(&full_args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn http_base() -> String {
    env::var("RS_LEARN_HTTP_URL").unwrap_or_else(|_| HTTP_DEFAULT_URL.to_string())
}

fn http_post(base: &str, path: &str, body: &str, timeout: Duration) -> Option<String> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let (host, port, path_only) = parse_url(&url)?;
    let connect_to = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect_timeout(&connect_to.parse().ok()?, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path_only, host, body.len(), body
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).ok()?;
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    Some(resp[body_start..].to_string())
}

fn parse_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => (&hostport[..i], hostport[i+1..].parse().unwrap_or(80)),
        None => (hostport, 80),
    };
    Some((host.to_string(), port, path.to_string()))
}

fn http_search(query: &str, limit: u32) -> Option<String> {
    let base = http_base();
    let body = serde_json::json!({
        "query": query,
        "scope": "episodes",
        "limit": limit,
    }).to_string();
    http_post(&base, "/search", &body, Duration::from_secs(3))
}

/// Recall episodes for a query. Returns formatted markdown or empty string on failure.
/// Tries HTTP first (fast), falls back to bun subprocess.
pub fn recall(query: &str, project_dir: &str, limit: u32) -> String {
    if query.trim().is_empty() { return String::new(); }

    if let Some(raw) = http_search(query, limit) {
        let formatted = format_recall(&raw, limit);
        if !formatted.is_empty() { return formatted; }
    }

    let limit_str = limit.to_string();
    let raw = run_bun_rs_learn(
        &["search", query, "--scope", "episodes", "--limit", &limit_str],
        project_dir,
        Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS),
    );
    if raw.is_empty() { return String::new(); }
    format_recall(&raw, limit)
}

fn format_recall(raw: &str, max: u32) -> String {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(raw);
    let Ok(v) = parsed else { return String::new() };
    // HTTP shape: {"hits":[...]}, bun CLI shape: [...]
    let arr = v.as_array().cloned()
        .or_else(|| v["hits"].as_array().cloned())
        .unwrap_or_default();
    if arr.is_empty() { return String::new(); }
    let mut out = String::new();
    for (i, item) in arr.iter().take(max as usize).enumerate() {
        // Try both shapes: {row: {...}} (CLI) or flattened (HTTP)
        let row = if item["row"].is_object() { &item["row"] } else { item };
        let content = row["content"].as_str().unwrap_or("").trim();
        let source = row["source"].as_str().unwrap_or("").trim();
        if content.is_empty() { continue; }
        let snippet: String = content.chars().take(400).collect();
        let suffix = if content.chars().count() > 400 { "…" } else { "" };
        let src = if source.is_empty() { String::new() } else { format!(" [{}]", source) };
        out.push_str(&format!("{}.{}\n   {}{}\n", i + 1, src, snippet, suffix));
    }
    out
}

/// Default project-grounding query for session-start.
pub fn project_query(project_dir: &str) -> String {
    let name = std::path::Path::new(project_dir)
        .file_name().and_then(|n| n.to_str()).unwrap_or("project");
    format!("{} feedback project decisions", name)
}

/// Counter-driven deep cycles. Increments prompt counter; spawns background work at thresholds.
/// Returns nothing (fire-and-forget). Safe: detached, never blocks the hook.
pub fn tick_and_maybe_run_deep_cycles(project_dir: &str) {
    let cpath = counter_path(project_dir);
    let mut counter = read_counter(&cpath);
    counter.prompts = counter.prompts.saturating_add(1);

    let communities_every: u64 = env::var("GM_RSLEARN_COMMUNITIES_EVERY").ok()
        .and_then(|s| s.parse().ok()).unwrap_or(5);
    let trajectory_every: u64 = env::var("GM_RSLEARN_TRAJECTORY_EVERY").ok()
        .and_then(|s| s.parse().ok()).unwrap_or(20);
    let debug_every: u64 = env::var("GM_RSLEARN_DEBUG_EVERY").ok()
        .and_then(|s| s.parse().ok()).unwrap_or(50);

    if counter.prompts.saturating_sub(counter.last_communities) >= communities_every {
        spawn_detached_bun_rs_learn(&["build-communities"], project_dir);
        counter.last_communities = counter.prompts;
    }

    if counter.prompts.saturating_sub(counter.last_trajectory) >= trajectory_every {
        spawn_trajectory_ingest(project_dir);
        counter.last_trajectory = counter.prompts;
    }

    if counter.prompts.saturating_sub(counter.last_debug) >= debug_every {
        spawn_debug_snapshot(project_dir);
        counter.last_debug = counter.prompts;
    }

    write_counter(&cpath, &counter);
}

fn spawn_trajectory_ingest(project_dir: &str) {
    // Take last 5 commit messages + diffs as a trajectory episode. Detached; OK if it fails.
    let log = no_window_cmd("git").args(["log", "-5", "--pretty=format:%h %s%n%b%n---"])
        .current_dir(project_dir).output();
    let summary = match log {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return,
    };
    if summary.trim().is_empty() { return; }
    // Drafts live under .gm/ so cleanup and audit are trivial.
    let drafts_dir = gm_state_dir(project_dir).join("trajectory-drafts");
    let _ = fs::create_dir_all(&drafts_dir);
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = drafts_dir.join(format!("{}.txt", ts));
    if fs::write(&path, &summary).is_err() { return; }
    let path_str = path.to_string_lossy().to_string();
    spawn_detached_bun_rs_learn(
        &["add", "--file", &path_str, "--source", "trajectory/recent-commits", "--no-extract"],
        project_dir,
    );
}

fn spawn_debug_snapshot(project_dir: &str) {
    let gm_dir = std::path::Path::new(project_dir).join(".gm");
    let _ = fs::create_dir_all(&gm_dir);
    let out = run_bun_rs_learn(&["debug"], project_dir, Duration::from_secs(8));
    if out.is_empty() { return; }
    let _ = fs::write(gm_dir.join("learning-state.md"), out);
}

/// Record CI outcome quality as feedback for the rs-learn router.
/// quality: 0.0 (worst) to 1.0 (best). Detached spawn — no HTTP path because /feedback isn't
/// in the http router today; subprocess is fine for end-of-session events.
pub fn record_quality(session_id: &str, project_dir: &str, quality: f32, note: &str) {
    if session_id.is_empty() { return; }
    let q = format!("{:.2}", quality.clamp(0.0, 1.0));
    if note.is_empty() {
        spawn_detached_bun_rs_learn(&["feedback", session_id, &q], project_dir);
    } else {
        spawn_detached_bun_rs_learn(&["feedback", session_id, &q, note], project_dir);
    }
}

/// Forget memories. Returns number of episodes invalidated. Directives:
///   ("by-source", "<tag>")    — invalidate all episodes whose source matches the tag exactly
///   ("by-query",  "<query>")  — search and invalidate the top hits matching the query
///   ("by-id",     "<uuid>")   — invalidate one episode by ID
///
/// Invalidation = mark `invalid_at = now()` rather than hard-delete, so the audit trail is preserved
/// and the operation is reversible. Search filters out invalidated episodes by default.
pub fn forget(directive: &str, target: &str, project_dir: &str) -> Result<usize, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("forget target is empty".into());
    }
    match directive {
        "by-id" => forget_by_ids(&[target.to_string()], project_dir),
        "by-source" => {
            let ids = list_episode_ids(Some(target), None, 200, project_dir)?;
            if ids.is_empty() { return Ok(0); }
            forget_by_ids(&ids, project_dir)
        }
        "by-query" => {
            let ids = list_episode_ids(None, Some(target), 20, project_dir)?;
            if ids.is_empty() { return Ok(0); }
            forget_by_ids(&ids, project_dir)
        }
        other => Err(format!("unknown directive '{}'. Use by-source | by-query | by-id", other)),
    }
}

fn list_episode_ids(source: Option<&str>, query: Option<&str>, limit: u32, project_dir: &str) -> Result<Vec<String>, String> {
    if let Some(q) = query {
        let raw = if let Some(r) = http_search(q, limit) { r } else {
            let limit_str = limit.to_string();
            run_bun_rs_learn(&["search", q, "--scope", "episodes", "--limit", &limit_str], project_dir, Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS))
        };
        if raw.is_empty() { return Ok(vec![]); }
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let arr = v.as_array().cloned()
            .or_else(|| v["hits"].as_array().cloned())
            .unwrap_or_default();
        let ids: Vec<String> = arr.iter().filter_map(|item| {
            let row = if item["row"].is_object() { &item["row"] } else { item };
            row["id"].as_str().map(|s| s.to_string())
        }).collect();
        return Ok(ids);
    }
    if let Some(s) = source {
        let raw = run_bun_rs_learn(
            &["episodes", "--source", s, "--limit", &limit.to_string()],
            project_dir,
            Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS),
        );
        if raw.is_empty() { return Ok(vec![]); }
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let arr = v.as_array().cloned().unwrap_or_default();
        let ids: Vec<String> = arr.iter().filter_map(|item| {
            item["id"].as_str().map(|s| s.to_string())
        }).collect();
        return Ok(ids);
    }
    Ok(vec![])
}

fn forget_by_ids(ids: &[String], project_dir: &str) -> Result<usize, String> {
    let mut count = 0usize;
    for id in ids {
        let base = http_base();
        let path = format!("/episode/{}", id);
        if http_delete(&base, &path, Duration::from_secs(2)).is_some() {
            count += 1;
            continue;
        }
        // Fallback: bun subprocess
        let out = run_bun_rs_learn(&["forget", id], project_dir, Duration::from_secs(4));
        if !out.is_empty() && !out.contains("error") { count += 1; }
    }
    Ok(count)
}

fn http_delete(base: &str, path: &str, timeout: Duration) -> Option<String> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let (host, port, path_only) = parse_url(&url)?;
    let connect_to = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect_timeout(&connect_to.parse().ok()?, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    let req = format!(
        "DELETE {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path_only, host
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).ok()?;
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    Some(resp[body_start..].to_string())
}

/// Fast-path ingest. Tries HTTP first, falls back to bun. Best-effort, never blocks.
pub fn ingest_fast(content: &str, source: &str, project_dir: &str) {
    if content.trim().is_empty() { return; }

    let body = serde_json::json!({
        "content": content,
        "source": source,
    }).to_string();
    if let Some(_) = http_post(&http_base(), "/messages", &body, Duration::from_secs(2)) {
        return;
    }

    // Fallback: subprocess. Write under .gm/ingest-drafts/ so it's auditable, then auto-clean.
    let drafts_dir = gm_state_dir(project_dir).join("ingest-drafts");
    let _ = fs::create_dir_all(&drafts_dir);
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = drafts_dir.join(format!("{}.txt", ts));
    if fs::write(&path, content).is_err() { return; }
    let path_str = path.to_string_lossy().to_string();
    spawn_detached_bun_rs_learn(
        &["add", "--file", &path_str, "--source", source, "--no-extract"],
        project_dir,
    );
}
