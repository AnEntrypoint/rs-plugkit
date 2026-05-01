// rs-learn integration helpers — append-only context augmentation, counter-driven deep cycles.
// All operations are best-effort: failures return empty strings rather than blocking hooks.
//
// Transport priority:
//   1. HTTP — if `rs-learn serve` is running (env RS_LEARN_HTTP_URL or default :8000), use it.
//      Single shared embedder, ~5ms latency. Best for hot hooks.
//   2. Direct rs-learn lib call via shared tokio runtime. No subprocess. No window flash.

use std::{env, fs, io::{Read, Write}, net::TcpStream, path::PathBuf, sync::{Arc, OnceLock}, time::Duration};
use super::no_window_cmd;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Semaphore;

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

/// Shared tokio runtime for rs-learn direct calls. Created once per plugkit invocation.
fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("rs-learn tokio runtime")
    })
}

/// In-process semaphore that serializes rs-learn lib work to avoid local CPU/IO thrash from
/// concurrent embedder/community-build runs. Default permits=1 (strict sequential). Override
/// via env GM_RSLEARN_MAX_PARALLEL (clamped [1,3]). Cross-process gating still requires routing
/// through `rs-learn serve` (HTTP), which has its own LLM_GATE; this semaphore handles the
/// in-process case so a single plugkit hook invocation that fans out doesn't trigger N parallel
/// embedder loads.
fn gate() -> Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| {
        let raw = std::env::var("GM_RSLEARN_MAX_PARALLEL")
            .ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
        let permits = raw.clamp(1, 3) as usize;
        Arc::new(Semaphore::new(permits))
    }).clone()
}

/// Run a future against rs-learn lib with a wall-clock timeout. Returns None on timeout/error.
/// Acquires the in-process gate to serialize concurrent rs-learn work.
///
/// Runtime resolution: prefers the ambient tokio runtime (when called from `#[tokio::main]` or
/// any tokio task) via `Handle::try_current()` + `block_in_place`. Falls back to the dedicated
/// shared runtime when called from a non-tokio context (rare; legacy hook entry points).
fn run_with_timeout<F, T>(timeout: Duration, fut: F) -> Option<T>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let gate = gate();
    let work = async move {
        let _permit = gate.acquire().await.ok()?;
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(v)) => Some(v),
            _ => None,
        }
    };
    match Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(work)),
        Err(_) => rt().block_on(work),
    }
}

/// Detached fire-and-forget lib call. Spawns onto the ambient runtime when available, falling
/// back to the dedicated shared runtime. Acquires the in-process gate so concurrent fire-and-forget
/// calls run sequentially.
fn spawn_detached<F>(fut: F)
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let gate = gate();
    let work = async move {
        let Ok(_permit) = gate.acquire_owned().await else { return };
        let _ = fut.await;
    };
    match Handle::try_current() {
        Ok(handle) => { handle.spawn(work); }
        Err(_) => { rt().spawn(work); }
    }
}

/// Open store + embedder + llm against the project's rs-learn DB. Honors RS_LEARN_DB_PATH /
/// project .gm/rs-learn.db resolution. Done once-per-call (no caching across hooks because
/// each plugkit invocation is a fresh process).
async fn open_graph(project_dir: &str) -> anyhow::Result<(
    std::sync::Arc<rs_learn::Store>,
    std::sync::Arc<rs_learn::Embedder>,
    std::sync::Arc<rs_learn::graph::llm::LlmJson>,
)> {
    // Resolve relative to project_dir by setting CWD via env override; rs_learn::resolve_db_path()
    // uses RS_LEARN_DB_PATH if set, otherwise <cwd>/.gm/rs-learn.db.
    let db_path = if let Ok(p) = std::env::var("RS_LEARN_DB_PATH") {
        std::path::PathBuf::from(p)
    } else {
        std::path::PathBuf::from(project_dir).join(".gm").join("rs-learn.db")
    };
    let db_str = db_path.to_string_lossy().to_string();
    let store = std::sync::Arc::new(rs_learn::Store::open(&db_str).await?);
    rs_search::embed_cache::set_shared_connection(store.conn.clone());
    let embedder = std::sync::Arc::new(rs_learn::Embedder::new());
    let backend = rs_learn::backend::from_env()
        .map_err(|e| anyhow::anyhow!("backend: {e}"))?;
    let llm = std::sync::Arc::new(rs_learn::graph::llm::LlmJson::new(backend));
    Ok((store, embedder, llm))
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
    let started = std::time::Instant::now();
    let result = recall_inner(query, project_dir, limit);
    let query_preview: String = query.chars().take(80).collect();
    rs_exec::obs::event("rs_learn", "recall", serde_json::json!({
        "query": query_preview,
        "query_len": query.len(),
        "limit": limit,
        "result_len": result.len(),
        "hit": !result.is_empty(),
        "dur_ms": started.elapsed().as_millis() as u64
    }));
    result
}

fn recall_inner(query: &str, project_dir: &str, limit: u32) -> String {
    if let Some(raw) = http_search(query, limit) {
        let formatted = format_recall(&raw, limit);
        if !formatted.is_empty() { return formatted; }
    }

    let q = query.to_string();
    let pd = project_dir.to_string();
    let lim = limit as usize;
    let hits = run_with_timeout(Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS), async move {
        let (store, embedder, llm) = open_graph(&pd).await?;
        let searcher = rs_learn::graph::search::Searcher::with_llm(store, embedder, llm);
        let cfg = rs_learn::graph::search::SearchConfig { limit: lim, ..Default::default() };
        let hits = searcher.search_episodes(&q, &cfg).await?;
        Ok(hits)
    });
    let Some(hits) = hits else { return String::new() };
    let raw = match serde_json::to_string(&hits) { Ok(s) => s, Err(_) => return String::new() };
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
        let pd = project_dir.to_string();
        spawn_detached(async move {
            let (store, embedder, llm) = open_graph(&pd).await?;
            let ops = rs_learn::graph::communities::CommunityOps::new(store, embedder, llm);
            let _ = ops.build_communities().await?;
            Ok(())
        });
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
    let pd = project_dir.to_string();
    let summary_text = summary.clone();
    spawn_detached(async move {
        let (store, embedder, llm) = open_graph(&pd).await?;
        let ingestor = rs_learn::graph::ingest::Ingestor::new(store, embedder, llm);
        let _ = ingestor.add_episode_fast(&summary_text, "trajectory/recent-commits", None).await?;
        Ok(())
    });
}

fn spawn_debug_snapshot(project_dir: &str) {
    let gm_dir = std::path::Path::new(project_dir).join(".gm");
    let _ = fs::create_dir_all(&gm_dir);
    let pd = project_dir.to_string();
    let snap = run_with_timeout(Duration::from_secs(8), async move {
        let _ = open_graph(&pd).await?; // ensure subsystems init
        let v = rs_learn::observability::dump();
        Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
    });
    let Some(out) = snap else { return };
    if out.is_empty() { return; }
    let _ = fs::write(gm_dir.join("learning-state.md"), out);
}

/// Record CI outcome quality as feedback for the rs-learn router.
/// quality: 0.0 (worst) to 1.0 (best). Detached spawn — no HTTP path because /feedback isn't
/// in the http router today; subprocess is fine for end-of-session events.
pub fn record_quality(session_id: &str, project_dir: &str, quality: f32, note: &str) {
    if session_id.is_empty() { return; }
    let sid = session_id.to_string();
    let pd = project_dir.to_string();
    let q = quality.clamp(0.0, 1.0);
    let signal = if note.is_empty() { None } else { Some(note.to_string()) };
    spawn_detached(async move {
        let db = std::path::PathBuf::from(&pd).join(".gm").join("rs-learn.db");
        std::env::set_var("RS_LEARN_DB_PATH", &db);
        let orch = rs_learn::Orchestrator::new_default().await?;
        orch.feedback(&sid, rs_learn::learn::instant::FeedbackPayload { quality: q, signal }).await?;
        Ok(())
    });
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
    let pd = project_dir.to_string();
    let lim = limit as usize;
    if let Some(q) = query {
        let qs = q.to_string();
        let pdc = pd.clone();
        let hits = run_with_timeout(Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS), async move {
            let (store, embedder, llm) = open_graph(&pdc).await?;
            let searcher = rs_learn::graph::search::Searcher::with_llm(store, embedder, llm);
            let cfg = rs_learn::graph::search::SearchConfig { limit: lim, ..Default::default() };
            Ok(searcher.search_episodes(&qs, &cfg).await?)
        }).ok_or_else(|| "search timed out or failed".to_string())?;
        let ids: Vec<String> = hits.iter().filter_map(|h| {
            h.row.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())
        }).collect();
        return Ok(ids);
    }
    if let Some(s) = source {
        let src = s.to_string();
        let ids = run_with_timeout(Duration::from_secs(DEFAULT_RECALL_TIMEOUT_SECS), async move {
            let (store, _embedder, _llm) = open_graph(&pd).await?;
            let mut rows = store.conn.query(
                "SELECT id FROM episodes WHERE source = ?1 AND (invalid_at IS NULL OR invalid_at = 0) \
                 ORDER BY created_at DESC LIMIT ?2",
                libsql::params![src, lim as i64],
            ).await?;
            let mut out: Vec<String> = Vec::new();
            while let Some(row) = rows.next().await? {
                if let Ok(id) = row.get::<String>(0) { out.push(id); }
            }
            Ok(out)
        }).ok_or_else(|| "episode list timed out or failed".to_string())?;
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
        let id_owned = id.clone();
        let pd = project_dir.to_string();
        let ok = run_with_timeout(Duration::from_secs(4), async move {
            let (store, _embedder, _llm) = open_graph(&pd).await?;
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64).unwrap_or(0);
            let r = store.conn.execute(
                "UPDATE episodes SET invalid_at = ?1 WHERE id = ?2 AND (invalid_at IS NULL OR invalid_at = 0)",
                libsql::params![now, id_owned],
            ).await?;
            Ok(r > 0)
        }).unwrap_or(false);
        if ok { count += 1; }
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

/// Pass through arbitrary rs-learn subcommands when the right HTTP endpoint
/// doesn't exist (status/debug/feedback/build-communities). Falls through to
/// bun subprocess. Returns stdout.
pub fn learn_passthrough(action: &str, rest: &[String], project_dir: &str) -> String {
    // Try HTTP for known endpoints first.
    if action == "build-communities" {
        if let Some(out) = http_post(&http_base(), "/build-communities", "{}", Duration::from_secs(30)) {
            return out;
        }
    }

    let pd = project_dir.to_string();
    let action_owned = action.to_string();
    let rest_owned: Vec<String> = rest.to_vec();
    run_with_timeout(Duration::from_secs(30), async move {
        match action_owned.as_str() {
            "build-communities" => {
                let (store, embedder, llm) = open_graph(&pd).await?;
                let ops = rs_learn::graph::communities::CommunityOps::new(store, embedder, llm);
                let r = ops.build_communities().await?;
                Ok(format!("communities={} members={}", r.community_count, r.member_count))
            }
            "debug" => {
                let _ = open_graph(&pd).await?;
                let v = match rest_owned.first() {
                    Some(name) => rs_learn::observability::dump()
                        .get(name.as_str()).cloned()
                        .ok_or_else(|| anyhow::anyhow!("unknown subsystem '{}'", name))?,
                    None => rs_learn::observability::dump(),
                };
                Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
            }
            "feedback" => {
                let request_id = rest_owned.first().ok_or_else(|| anyhow::anyhow!("feedback requires request_id"))?;
                let q_str = rest_owned.get(1).ok_or_else(|| anyhow::anyhow!("feedback requires quality"))?;
                let quality: f32 = q_str.parse().map_err(|_| anyhow::anyhow!("quality must be f32"))?;
                let signal = rest_owned.get(2).cloned();
                let db = std::path::PathBuf::from(&pd).join(".gm").join("rs-learn.db");
                std::env::set_var("RS_LEARN_DB_PATH", &db);
                let orch = rs_learn::Orchestrator::new_default().await?;
                orch.feedback(request_id, rs_learn::learn::instant::FeedbackPayload { quality, signal }).await?;
                Ok("ok".to_string())
            }
            "clear" => {
                let (store, embedder, llm) = open_graph(&pd).await?;
                rs_learn::graph::ingest::Ingestor::new(store, embedder, llm).clear_graph(None).await?;
                Ok("cleared".to_string())
            }
            other => Err(anyhow::anyhow!("unsupported passthrough action '{}'", other)),
        }
    }).unwrap_or_default()
}

/// Fast-path ingest. Tries HTTP first, falls back to bun. Best-effort, never blocks.
pub fn ingest_fast(content: &str, source: &str, project_dir: &str) {
    if content.trim().is_empty() { return; }

    let ingest_start = std::time::Instant::now();
    let content_len = content.len();
    let body = serde_json::json!({
        "content": content,
        "source": source,
    }).to_string();
    if let Some(_) = http_post(&http_base(), "/messages", &body, Duration::from_secs(2)) {
        rs_exec::obs::event("rs_learn", "ingest", serde_json::json!({
            "source": source,
            "content_len": content_len,
            "path": "http",
            "dur_ms": ingest_start.elapsed().as_millis() as u64
        }));
        return;
    }

    rs_exec::obs::event("rs_learn", "ingest", serde_json::json!({
        "source": source,
        "content_len": content_len,
        "path": "lib",
        "dur_ms": ingest_start.elapsed().as_millis() as u64
    }));
    // Fallback: direct lib call, fire-and-forget on shared runtime.
    let pd = project_dir.to_string();
    let content_owned = content.to_string();
    let source_owned = source.to_string();
    spawn_detached(async move {
        let (store, embedder, llm) = open_graph(&pd).await?;
        let ingestor = rs_learn::graph::ingest::Ingestor::new(store, embedder, llm);
        let _ = ingestor.add_episode_fast(&content_owned, &source_owned, None).await?;
        Ok(())
    });
}
