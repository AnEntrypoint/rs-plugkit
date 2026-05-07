mod hook;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::{env, fs, path::PathBuf, time::Duration};

use rs_exec::{daemon, rpc_client};
use rs_codeinsight::{analyze, AnalyzeOptions};
use rs_search::{bm25, context, mcp as search_mcp, scanner};

const RUNNER_NAME: &str = "plugkit-runner";

fn port_file() -> PathBuf {
    env::temp_dir().join("glootie-runner.port")
}

fn self_exe() -> String {
    env::current_exe().unwrap_or_default().to_string_lossy().to_string()
}

#[derive(Parser)]
#[command(name = "plugkit", about = "plugkit — exec + codeinsight CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Exec {
        #[arg(long)] lang: Option<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] file: Option<String>,
        #[arg(long)] session: Option<String>,
        #[arg(long = "timeout-ms")] timeout_ms: Option<u64>,
        code: Vec<String>,
    },
    Bash {
        #[arg(long)] cwd: Option<String>,
        #[arg(long = "timeout-ms")] timeout_ms: Option<u64>,
        commands: Vec<String>,
    },
    #[command(name = "type")] Type { task_id: String, input: Vec<String>, #[arg(long)] session: Option<String> },
    Runner { sub: String },
    Pm2list,
    Codeinsight {
        path: Option<String>,
        #[arg(long)] json: bool,
        #[arg(long)] cache: bool,
        #[arg(long)] read_cache: bool,
    },
    Search {
        #[arg(long)] path: Option<String>,
        query: Vec<String>,
        #[arg(long)] discipline: Option<String>,
    },
    SessionCleanup {
        #[arg(long)] session: String,
    },
    /// Wait for a background task to produce new output (or finish). Polls listTasks.
    Sleep {
        task_id: String,
        #[arg(long, default_value_t = 30)] max_secs: u64,
    },
    /// Show status of a background task (or all tasks if none specified).
    Status {
        task_id: Option<String>,
    },
    /// Terminate a background task by id (best-effort via listTasks + kill).
    Close {
        task_id: String,
    },
    Hook {
        event: String,
    },
    #[command(name = "kill-port")] KillPort { port: u16 },
    Deps,
    Doctor,
    /// Recall episodes from rs-learn (HTTP-preferred, bun fallback). Prints formatted text.
    Recall {
        query: Vec<String>,
        #[arg(long, default_value_t = 5)] limit: u32,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] discipline: Option<String>,
    },
    /// Ingest a fact into rs-learn fast-path (HTTP-preferred, bun fallback). Detached.
    Memorize {
        #[arg(long)] source: Option<String>,
        #[arg(long)] file: Option<String>,
        content: Vec<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] discipline: Option<String>,
    },
    /// Invalidate / unlearn previously-memorized facts. Directives: `by-source <tag>` | `by-query <query>` | `by-id <episode_id>`.
    Forget {
        directive: Vec<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] discipline: Option<String>,
    },
    /// Pass-through to rs-learn for status/debug/feedback/build-communities. HTTP-preferred, bun fallback.
    Learn {
        action: String,
        rest: Vec<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] discipline: Option<String>,
    },
    /// Manage knowledge disciplines under <project>/.gm/disciplines/.
    Discipline {
        /// list | new <name> | info <name> | enable <name> | disable <name>
        sub: String,
        rest: Vec<String>,
        #[arg(long)] cwd: Option<String>,
    },
    /// Inspect the gm-log JSONL stream under ~/.claude/gm-log/.
    Log {
        /// tail | grep | stats | path | prune | subsystems
        action: String,
        /// Filter terms (grep) or subsystem (tail/stats); supports plain substring match.
        rest: Vec<String>,
        /// Limit lines (tail/grep). Default 50.
        #[arg(long, default_value_t = 50)] limit: usize,
        /// Date YYYY-MM-DD. Default = today (tail/grep/stats/subsystems).
        #[arg(long)] date: Option<String>,
        /// Retention window in days (prune: delete older; stats: aggregate range).
        #[arg(long, default_value_t = 14)] days: u32,
    },
}

fn extract_discipline_sigil(args: &mut Vec<String>, flag: Option<String>) -> Option<String> {
    if let Some(d) = flag {
        let trimmed = d.trim_start_matches('@').trim().to_string();
        if !trimmed.is_empty() { return Some(trimmed); }
    }
    if let Some(first) = args.first() {
        if let Some(rest) = first.strip_prefix('@') {
            let name = rest.trim().to_string();
            if !name.is_empty() {
                args.remove(0);
                return Some(name);
            }
        }
    }
    None
}

fn discipline_root(project_dir: &str) -> PathBuf {
    PathBuf::from(project_dir).join(".gm").join("disciplines")
}

fn discipline_db_path(project_dir: &str, name: Option<&str>) -> PathBuf {
    match name {
        Some(n) => discipline_root(project_dir).join(n).join("rs-learn.db"),
        None => PathBuf::from(project_dir).join(".gm").join("rs-learn.db"),
    }
}

fn list_enabled_disciplines(project_dir: &str) -> Vec<String> {
    let p = discipline_root(project_dir).join("enabled.txt");
    fs::read_to_string(&p)
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect())
        .unwrap_or_default()
}

fn list_all_disciplines(project_dir: &str) -> Vec<String> {
    let root = discipline_root(project_dir);
    if !root.exists() { return Vec::new(); }
    let mut out: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn cmd_discipline(sub: &str, rest: &[String], project_dir: &str) -> i32 {
    let root = discipline_root(project_dir);
    match sub {
        "list" => {
            let all = list_all_disciplines(project_dir);
            let enabled: std::collections::HashSet<String> = list_enabled_disciplines(project_dir).into_iter().collect();
            if all.is_empty() {
                println!("(no disciplines under {})", root.display());
                return 0;
            }
            for name in &all {
                let mark = if enabled.contains(name) { "*" } else { " " };
                println!("{} {}", mark, name);
            }
            0
        }
        "new" => {
            let Some(name) = rest.first() else { eprintln!("usage: plugkit discipline new <name>"); return 1; };
            let d = root.join(name);
            if let Err(e) = fs::create_dir_all(d.join("code-search")) { eprintln!("mkdir failed: {}", e); return 1; }
            println!("created {}", d.display());
            0
        }
        "info" => {
            let Some(name) = rest.first() else { eprintln!("usage: plugkit discipline info <name>"); return 1; };
            let d = root.join(name);
            let db = d.join("rs-learn.db");
            let cs = d.join("code-search");
            println!("name: {}", name);
            println!("dir: {}", d.display());
            println!("rs-learn.db: {}", if db.exists() { format!("present ({} bytes)", fs::metadata(&db).map(|m| m.len()).unwrap_or(0)) } else { "absent".to_string() });
            println!("code-search: {}", if cs.exists() { "present" } else { "absent" });
            let enabled: std::collections::HashSet<String> = list_enabled_disciplines(project_dir).into_iter().collect();
            println!("enabled: {}", enabled.contains(name.as_str()));
            0
        }
        "enable" => {
            let Some(name) = rest.first() else { eprintln!("usage: plugkit discipline enable <name>"); return 1; };
            let _ = fs::create_dir_all(&root);
            let mut cur = list_enabled_disciplines(project_dir);
            if !cur.iter().any(|n| n == name) { cur.push(name.clone()); }
            let _ = fs::write(root.join("enabled.txt"), cur.join("\n") + "\n");
            println!("enabled {}", name);
            0
        }
        "disable" => {
            let Some(name) = rest.first() else { eprintln!("usage: plugkit discipline disable <name>"); return 1; };
            let cur: Vec<String> = list_enabled_disciplines(project_dir).into_iter().filter(|n| n != name).collect();
            let _ = fs::write(root.join("enabled.txt"), cur.join("\n") + "\n");
            println!("disabled {}", name);
            0
        }
        other => { eprintln!("unknown discipline action: {} (use list|new|info|enable|disable)", other); 1 }
    }
}

const RS_EXEC_SHA: &str = env!("DEP_RS_EXEC_SHA");
const RS_SEARCH_SHA: &str = env!("DEP_RS_SEARCH_SHA");
const RS_CODEINSIGHT_SHA: &str = env!("DEP_RS_CODEINSIGHT_SHA");

fn cmd_log(action: &str, rest: &[String], limit: usize, date: Option<&str>, days: u32) -> i32 {
    let root = rs_exec::obs::root_dir();
    if action == "path" { println!("{}", root.display()); return 0; }
    if action == "prune" { return cmd_log_prune(&root, days); }

    let day = date.map(String::from).unwrap_or_else(today_ymd);
    let day_dir = root.join(&day);

    if action == "subsystems" {
        if !day_dir.exists() { eprintln!("no logs for {} at {}", day, day_dir.display()); return 0; }
        let mut subs: Vec<String> = std::fs::read_dir(&day_dir).ok()
            .map(|it| it.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
                .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                .collect())
            .unwrap_or_default();
        subs.sort();
        for s in &subs { println!("{}", s); }
        return 0;
    }

    if action == "stats" && date.is_none() && days > 1 {
        return cmd_log_stats_range(&root, days);
    }

    if !day_dir.exists() {
        eprintln!("no logs for {} at {}", day, day_dir.display());
        return 0;
    }
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&day_dir).ok()
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl")).collect())
        .unwrap_or_default();
    files.sort();
    if let Some(sub) = rest.first() {
        if !sub.is_empty() && action != "grep" {
            files.retain(|p| p.file_stem().and_then(|s| s.to_str()).map(|n| n == sub).unwrap_or(false));
        }
    }
    match action {
        "tail" => {
            let mut entries: Vec<(String, String, String)> = vec![];
            for f in &files {
                if let Ok(content) = std::fs::read_to_string(f) {
                    let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                    for line in content.lines() {
                        let ts = extract_ts(line).unwrap_or_default();
                        entries.push((ts, stem.clone(), line.to_string()));
                    }
                }
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let start = entries.len().saturating_sub(limit);
            for (_, stem, line) in &entries[start..] { println!("[{}] {}", stem, line); }
            0
        }
        "grep" => {
            let needle = rest.join(" ");
            if needle.is_empty() { eprintln!("usage: plugkit log grep <terms...>"); return 1; }
            let mut count = 0;
            'outer: for f in &files {
                if let Ok(content) = std::fs::read_to_string(f) {
                    let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                    for line in content.lines() {
                        if line.contains(&needle) {
                            println!("[{}] {}", stem, line);
                            count += 1;
                            if count >= limit { break 'outer; }
                        }
                    }
                }
            }
            0
        }
        "stats" => {
            for f in &files {
                if let Ok(content) = std::fs::read_to_string(f) {
                    let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                    let n = content.lines().count();
                    let size = content.len();
                    println!("{:24} {:>8} lines  {:>10} bytes", stem, n, size);
                }
            }
            0
        }
        other => { eprintln!("unknown log action: {} (use tail|grep|stats|path|prune|subsystems)", other); 1 }
    }
}

fn extract_ts(line: &str) -> Option<String> {
    let key = "\"ts\":\"";
    let s = line.find(key)? + key.len();
    let rest = &line[s..];
    let e = rest.find('"')?;
    Some(rest[..e].to_string())
}

fn cmd_log_prune(root: &std::path::Path, days: u32) -> i32 {
    if !root.exists() { return 0; }
    let cutoff = today_minus_days(days);
    let mut removed = 0u32;
    let mut kept = 0u32;
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() { continue; }
            let name = match p.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !is_ymd(&name) { continue; }
            if name.as_str() < cutoff.as_str() {
                if std::fs::remove_dir_all(&p).is_ok() { removed += 1; }
            } else {
                kept += 1;
            }
        }
    }
    println!("pruned {} day-dir(s) older than {} ({} kept)", removed, cutoff, kept);
    0
}

fn cmd_log_stats_range(root: &std::path::Path, days: u32) -> i32 {
    use std::collections::BTreeMap;
    if !root.exists() { eprintln!("no logs at {}", root.display()); return 0; }
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut day_count = 0u32;
    for d in last_n_days(days) {
        let day_dir = root.join(&d);
        if !day_dir.exists() { continue; }
        day_count += 1;
        if let Ok(entries) = std::fs::read_dir(&day_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                if let Ok(content) = std::fs::read_to_string(&p) {
                    let n = content.lines().count() as u64;
                    let sz = content.len() as u64;
                    let entry = totals.entry(stem).or_insert((0, 0));
                    entry.0 += n; entry.1 += sz;
                }
            }
        }
    }
    println!("=== {} day(s), {} active ===", days, day_count);
    for (sub, (n, sz)) in &totals {
        println!("{:24} {:>10} lines  {:>12} bytes", sub, n, sz);
    }
    0
}

fn is_ymd(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..].iter().all(|c| c.is_ascii_digit())
}

fn today_minus_days(days: u32) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs = secs.saturating_sub((days as u64) * 86_400);
    ymd_from_secs(secs)
}

fn last_n_days(days: u32) -> Vec<String> {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (0..days).map(|i| ymd_from_secs(secs.saturating_sub((i as u64) * 86_400))).collect()
}

fn ymd_from_secs(secs: u64) -> String {
    let day = (secs / 86_400) as i64 + 719_468;
    let era = if day >= 0 { day } else { day - 146_096 } / 146_097;
    let doe = (day - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn today_ymd() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let day = (secs / 86_400) as i64 + 719_468;
    let era = if day >= 0 { day } else { day - 146_096 } / 146_097;
    let doe = (day - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn cmd_deps() -> anyhow::Result<()> {
    println!("plugkit {}", env!("CARGO_PKG_VERSION"));
    println!("rs-exec         {}", RS_EXEC_SHA);
    println!("rs-search       {}", RS_SEARCH_SHA);
    println!("rs-codeinsight  {}", RS_CODEINSIGHT_SHA);
    Ok(())
}

fn cmd_doctor() -> anyhow::Result<()> {
    use hook::no_window_cmd;
    let mut fail = 0u32;
    println!("=== plugkit doctor ===");
    println!("plugkit {}", env!("CARGO_PKG_VERSION"));
    println!("deps: rs-exec={} rs-search={} rs-codeinsight={}", RS_EXEC_SHA, RS_SEARCH_SHA, RS_CODEINSIGHT_SHA);

    let port_path = port_file();
    if port_path.exists() {
        match fs::read_to_string(&port_path) {
            Ok(s) => println!("runner port_file: {} (port {})", port_path.display(), s.trim()),
            Err(e) => { println!("runner port_file: read error: {}", e); fail += 1; }
        }
    } else {
        println!("runner port_file: absent (runner not started this boot)");
    }

    let crash_path = env::temp_dir().join("rs-exec-daemon-crash.log");
    if crash_path.exists() {
        println!("daemon crash log: {} (EXISTS — inspect)", crash_path.display());
        fail += 1;
    } else {
        println!("daemon crash log: absent");
    }

    let chrome_name = if cfg!(windows) { "chrome.exe" } else { "chrome" };
    let chrome_count = if cfg!(windows) {
        no_window_cmd("tasklist").args(["/FI", &format!("IMAGENAME eq {}", chrome_name)]).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| l.contains(chrome_name)).count())
            .unwrap_or(0)
    } else {
        no_window_cmd("pgrep").args(["-fc", chrome_name]).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<usize>().unwrap_or(0))
            .unwrap_or(0)
    };
    println!("chrome processes visible: {}", chrome_count);

    if let Ok(cwd) = env::current_dir() {
        let d = cwd.join(".gm").join("code-search");
        let legacy = cwd.join(".code-search");
        let path = if d.exists() { Some(d) } else if legacy.exists() { Some(legacy) } else { None };
        match path {
            Some(p) => {
                let size_bytes: u64 = walk_size(&p).unwrap_or(0);
                println!("code-search: {} ({} KB)", p.display(), size_bytes / 1024);
            }
            None => println!("code-search: not present in cwd"),
        }
    }

    if fail > 0 {
        eprintln!("doctor: {} check(s) failed", fail);
        std::process::exit(1);
    }
    Ok(())
}

fn walk_size(p: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(p)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() { total += meta.len(); }
        else if meta.is_dir() { total += walk_size(&entry.path()).unwrap_or(0); }
    }
    Ok(total)
}

fn runner_exe_stamp() -> PathBuf {
    env::temp_dir().join("plugkit-runner.exe-stamp")
}

fn current_exe_stamp() -> String {
    // Identity by content (size + mtime), NOT by path. Multiple plugin-cache versions
    // of the same build (e.g. concurrent worktrees, different session cwds) produce
    // byte-identical plugkit.exe files at different paths; including the path here
    // made every cross-path invocation flag "binary changed" and restart the shared
    // runner, wiping every other session's live background tasks.
    let exe = self_exe();
    let meta = fs::metadata(&exe).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok()).map(|t| format!("{:?}", t)).unwrap_or_default();
    let size = meta.map(|m| m.len()).unwrap_or(0);
    format!("{}|{}", size, mtime)
}

fn runner_needs_restart() -> bool {
    let stamp_file = runner_exe_stamp();
    let current = current_exe_stamp();
    match fs::read_to_string(&stamp_file) {
        Ok(stored) => stored.trim() != current,
        Err(_) => true,
    }
}

fn runner_start_lock() -> PathBuf {
    env::temp_dir().join("plugkit-runner-start.lock")
}

async fn ensure_runner() -> anyhow::Result<()> {
    // Fast path: runner is already healthy and binary hasn't changed.
    if rpc_client::health_check().await && !runner_needs_restart() {
        return Ok(());
    }

    // Slow path: acquire a file-based lock so only one process starts the runner.
    // This prevents multiple concurrent plugkit invocations from each spawning a runner.
    tokio::time::timeout(Duration::from_millis(8000), async {
        // Wait up to 4s for an existing concurrent start to finish.
        let lock_path = runner_start_lock();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(4000);
        loop {
            // Remove stale lock files left by crashed processes (older than 10s).
            if let Ok(meta) = fs::metadata(&lock_path) {
                let stale = meta.modified().ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|age| age.as_secs() > 10)
                    .unwrap_or(false);
                if stale { let _ = fs::remove_file(&lock_path); }
            }
            match fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
                Ok(_) => break, // We hold the lock
                Err(_) => {
                    // Another process is starting the runner; wait and re-check.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if rpc_client::health_check().await && !runner_needs_restart() {
                        return Ok(()); // Other process finished starting it
                    }
                    if tokio::time::Instant::now() >= deadline {
                        // Give up waiting for the lock; try anyway
                        let _ = fs::remove_file(&lock_path);
                        break;
                    }
                }
            }
        }

        // Re-check under lock: another process may have started it while we waited.
        if rpc_client::health_check().await && !runner_needs_restart() {
            let _ = fs::remove_file(&lock_path);
            return Ok(());
        }

        if rpc_client::health_check().await {
            // Healthy but stale binary — only restart if no tasks are currently running,
            // to avoid orphaning active background tasks.
            let has_active = rpc_client::rpc_call("listTasks", json!({}), 2000).await
                .ok()
                .and_then(|v| v["tasks"].as_array().map(|a| {
                    a.iter().any(|t| matches!(t["status"].as_str(), Some("running") | Some("pending")))
                }))
                .unwrap_or(false);
            if has_active {
                // Defer restart: update the stamp so we don't keep trying this tick,
                // and let the caller proceed with the running instance.
                let _ = fs::write(runner_exe_stamp(), current_exe_stamp());
                let _ = fs::remove_file(&lock_path);
                return Ok(());
            }
            daemon::kill(RUNNER_NAME);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let result = daemon::start(RUNNER_NAME, &self_exe(), &["--runner-mode"]);
        let _ = fs::remove_file(&lock_path); // Release lock before polling
        result?;
        let _ = fs::write(runner_exe_stamp(), current_exe_stamp());
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            if rpc_client::health_check().await { return Ok(()); }
        }
        Err(anyhow::anyhow!("Runner did not become healthy in time"))
    }).await.unwrap_or_else(|_| Err(anyhow::anyhow!("Runner startup timed out")))
}

fn parse_task_id(s: &str) -> u64 {
    s.trim_start_matches("task_").parse().unwrap_or(0)
}

fn normalize_code_input(raw: String) -> String {
    raw.trim_start_matches('\u{feff}').to_string()
}


const DEFAULT_EXEC_TIMEOUT_MS: u64 = 300_000;

async fn run_code(code: &str, runtime: &str, cwd: &str, session_id: Option<&str>, timeout_ms: Option<u64>) -> anyhow::Result<i32> {
    ensure_runner().await?;
    let effective_timeout = match timeout_ms {
        Some(n) if n > 0 => n,
        _ => DEFAULT_EXEC_TIMEOUT_MS,
    };
    let mut exec_params = json!({ "code": code, "runtime": runtime, "workingDirectory": cwd, "timeoutMs": effective_timeout });
    if let Some(sid) = session_id { exec_params["sessionId"] = json!(sid); }
    let exec_result = rpc_client::rpc_call("execute", exec_params, 0).await?;
    let result = &exec_result["result"];

    let printed_from_output = if let Some(arr) = result["output"].as_array() {
        let mut printed = false;
        for e in arr {
            let d = e["d"].as_str().unwrap_or("");
            if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); }
            if !d.is_empty() { printed = true; }
        }
        printed
    } else { false };
    if !printed_from_output {
        if let Some(s) = result["stdout"].as_str() { if !s.is_empty() { print!("{}", s); } }
        if let Some(s) = result["stderr"].as_str() { if !s.is_empty() { eprint!("{}", s); } }
    }
    let timed_out = result["timedOut"].as_bool().unwrap_or(false);
    if timed_out {
        eprintln!("[exec timed out after {} ms; partial output above]", effective_timeout);
    }
    if let Some(e) = result["error"].as_str() { if !e.is_empty() && !timed_out { eprintln!("Error: {}", e); return Ok(1); } }

    let exit_code = result["exitCode"].as_i64().unwrap_or(0) as i32;
    if timed_out { return Ok(if exit_code != 0 { exit_code } else { 124 }); }
    if result["success"].as_bool() == Some(false) { return Ok(if exit_code != 0 { exit_code } else { 1 }); }
    Ok(exit_code)
}


#[tokio::main]
async fn main() {
    if env::args().any(|a| a == "--exec-process-mode") {
        rs_exec::run_exec_process();
        return;
    }
    rs_exec::install_broken_pipe_handler();

    if env::args().any(|a| a == "--runner-mode") {
        rs_exec::runner::run_server().await.expect("Runner failed");
        return;
    }

    let cli = Cli::parse();
    let mut exit_code = 0i32;

    let result: anyhow::Result<()> = async {
        match cli.command {
            Cmd::Exec { lang, cwd, file, session, timeout_ms, code } => {
                let code_str = if let Some(ref f) = file { normalize_code_input(fs::read_to_string(f)?) } else { normalize_code_input(code.join(" ")) };
                if let Some(ref f) = file { let _ = fs::remove_file(f); }
                if code_str.trim().is_empty() { eprintln!("No code provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let mut runtime = lang.unwrap_or_else(|| "nodejs".into());
                if runtime == "typescript" || runtime == "auto" { runtime = "nodejs".into(); }
                exit_code = run_code(&code_str, &runtime, &cwd, session.as_deref(), timeout_ms).await?;
            }
            Cmd::Bash { cwd, timeout_ms, commands } => {
                let cmd = commands.join(" ");
                if cmd.trim().is_empty() { eprintln!("No commands provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let runtime = if cfg!(windows) { "powershell" } else { "bash" };
                exit_code = run_code(&cmd, runtime, &cwd, None, timeout_ms).await?;
            }
            Cmd::Type { task_id, input, session } => {
                ensure_runner().await?;
                let mut stdin_params = json!({ "taskId": parse_task_id(&task_id), "data": format!("{}\n", input.join(" ")) });
                if let Some(ref sid) = session { stdin_params["sessionId"] = json!(sid); }
                let res = rpc_client::rpc_call("sendStdin", stdin_params, 10000).await?;
                if res["ok"].as_bool().unwrap_or(false) { println!("Sent to task {}", task_id); }
                else { eprintln!("Task {} not found or not running", task_id); }
            }
            Cmd::Runner { sub } => match sub.as_str() {
                "start" => {
                    if rpc_client::health_check().await && !runner_needs_restart() {
                        println!("Runner already healthy on port {}", fs::read_to_string(port_file()).unwrap_or_default().trim().to_string());
                        return Ok(());
                    }
                    if rpc_client::health_check().await { daemon::kill(RUNNER_NAME); tokio::time::sleep(Duration::from_millis(200)).await; }
                    daemon::start(RUNNER_NAME, &self_exe(), &["--runner-mode"])?;
                    let _ = fs::write(runner_exe_stamp(), current_exe_stamp());
                    for _ in 0..20 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if rpc_client::health_check().await {
                            println!("Runner started on port {}", fs::read_to_string(port_file()).unwrap_or_default().trim().to_string());
                            return Ok(());
                        }
                    }
                    return Err(anyhow::anyhow!("Runner did not become healthy"));
                }
                "stop" => { daemon::kill(RUNNER_NAME); println!("Runner stopped"); }
                "status" => {
                    match daemon::describe(RUNNER_NAME) {
                        None => println!("{}: not found", RUNNER_NAME),
                        Some(d) => {
                            println!("name:   {}", d.name);
                            println!("status: {}", d.status);
                            println!("pid:    {}", d.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into()));
                            if let Ok(p) = fs::read_to_string(port_file()) { println!("port:   {}", p.trim()); }
                        }
                    }
                }
                _ => { eprintln!("Unknown runner subcommand: {}", sub); exit_code = 1; }
            }
            Cmd::Pm2list => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("pm2list", json!({}), 10000).await?;
                let procs = daemon::list();
                let online: Vec<_> = procs.iter().filter(|p| p.status == "online").collect();
                if online.is_empty() && res["processes"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("No processes found.");
                } else {
                    for p in online { println!("{}  status={}  pid={}", p.name, p.status, p.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into())); }
                    if let Some(arr) = res["processes"].as_array() {
                        for p in arr { println!("{}  status={}  pid={}", p["name"].as_str().unwrap_or("?"), p["status"].as_str().unwrap_or("?"), p["pid"]); }
                    }
                }
            }
            Cmd::Sleep { task_id, max_secs } => {
                ensure_runner().await?;
                let id = parse_task_id(&task_id);
                let deadline = std::time::Instant::now() + Duration::from_secs(max_secs.min(3600));
                let mut last_output_len = 0usize;
                loop {
                    let res = rpc_client::rpc_call("listTasks", json!({}), 5000).await?;
                    let task = res["tasks"].as_array().and_then(|arr| arr.iter().find(|t| t["id"].as_u64() == Some(id))).cloned();
                    let Some(task) = task else {
                        eprintln!("Task task_{} not found", id);
                        exit_code = 1; return Ok(());
                    };
                    let output = task["output"].as_array().map(|a| a.iter().map(|e| e["d"].as_str().unwrap_or("")).collect::<String>()).unwrap_or_default();
                    if output.len() > last_output_len {
                        print!("{}", &output[last_output_len..]);
                        last_output_len = output.len();
                    }
                    let status = task["status"].as_str().unwrap_or("");
                    if status == "completed" || status == "failed" || status == "killed" {
                        println!("\n[task task_{} {}]", id, status);
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        println!("\n[task task_{} still {} after {}s]", id, status, max_secs);
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            Cmd::Status { task_id } => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("listTasks", json!({}), 5000).await?;
                let arr = res["tasks"].as_array().cloned().unwrap_or_default();
                if let Some(tid) = task_id {
                    let id = parse_task_id(&tid);
                    match arr.iter().find(|t| t["id"].as_u64() == Some(id)) {
                        Some(t) => println!("task_{}  status={}  exitCode={}", id, t["status"].as_str().unwrap_or("?"), t["exitCode"]),
                        None => { eprintln!("Task task_{} not found", id); exit_code = 1; }
                    }
                } else {
                    if arr.is_empty() { println!("No background tasks."); }
                    else { for t in &arr { println!("task_{}  status={}  exitCode={}", t["id"].as_u64().unwrap_or(0), t["status"].as_str().unwrap_or("?"), t["exitCode"]); } }
                }
            }
            Cmd::Close { task_id } => {
                ensure_runner().await?;
                let id = parse_task_id(&task_id);
                let res = rpc_client::rpc_call("deleteTask", json!({ "taskId": id }), 5000).await?;
                if res["ok"].as_bool().unwrap_or(false) || res["deleted"].as_bool().unwrap_or(false) {
                    println!("task_{} closed", id);
                } else {
                    eprintln!("Could not close task_{}: {}", id, res);
                    exit_code = 1;
                }
            }
            Cmd::Codeinsight { path, json, cache, read_cache } => {
                let root = path.unwrap_or_else(|| ".".into());
                let root_path = std::path::Path::new(&root);
                if !root_path.exists() { eprintln!("Path does not exist: {}", root); exit_code = 1; return Ok(()); }
                let started = std::time::Instant::now();
                rs_exec::obs::event("rs_codeinsight", "analyze.start", serde_json::json!({
                    "root": root, "json": json, "cache": cache, "read_cache": read_cache
                }));
                if read_cache {
                    match fs::read_to_string(root_path.join(".codeinsight")) {
                        Ok(c) => {
                            print!("{}", c);
                            rs_exec::obs::event("rs_codeinsight", "analyze.end", serde_json::json!({
                                "root": root, "dur_ms": started.elapsed().as_millis() as u64,
                                "out_len": c.len(), "source": "cache"
                            }));
                            return Ok(());
                        }
                        Err(_) => {
                            eprintln!("No cache found");
                            rs_exec::obs::event("rs_codeinsight", "analyze.end", serde_json::json!({
                                "root": root, "dur_ms": started.elapsed().as_millis() as u64,
                                "out_len": 0, "source": "cache_miss", "ok": false
                            }));
                            exit_code = 1; return Ok(());
                        }
                    }
                }
                let output = analyze(root_path, AnalyzeOptions { json_mode: json });
                println!("{}", output.text);
                if cache {
                    let _ = fs::write(root_path.join(".codeinsight"), &output.text);
                }
                rs_exec::obs::event("rs_codeinsight", "analyze.end", serde_json::json!({
                    "root": root, "dur_ms": started.elapsed().as_millis() as u64,
                    "out_len": output.text.len(), "source": "fresh"
                }));
            }
            Cmd::SessionCleanup { session } => {
                if rpc_client::health_check().await {
                    let res = rpc_client::rpc_call("deleteSessionTasks", json!({ "sessionId": session }), 10000).await;
                    if let Ok(v) = res {
                        let count = v["deleted"].as_u64().unwrap_or(0);
                        if count > 0 { eprintln!("Cleaned up {} task(s) for session {}", count, session); }
                    }
                }
                if session.is_empty() {
                    hook::agent_browser::close_all_sessions();
                } else {
                    hook::agent_browser::close_sessions_for(&session);
                }
            }
            Cmd::Hook { event } => {
                let ev = event.as_str();
                let known = matches!(ev,
                    "session-start" | "pre-tool-use" | "post-tool-use" | "prompt-submit" |
                    "pre-compact" | "post-compact" | "session-end" | "stop" | "stop-git"
                );
                if !known {
                    eprintln!("Unknown hook event: {}", ev);
                    exit_code = 1;
                    return Ok(());
                }
                rs_exec::obs::span("hook", ev, serde_json::json!({}), || {
                    match ev {
                        "session-start" => hook::session_start(),
                        "pre-tool-use" => hook::pre_tool_use(),
                        "post-tool-use" => hook::post_tool_use(),
                        "prompt-submit" => hook::prompt_submit(),
                        "pre-compact" => hook::pre_compact(),
                        "post-compact" => hook::post_compact(),
                        "session-end" => hook::session_end(),
                        "stop" => hook::run_stop(),
                        "stop-git" => hook::run_stop_git(),
                        _ => {}
                    }
                });
                return Ok(());
            }
            Cmd::KillPort { port } => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("killPort", json!({ "port": port }), 10000).await?;
                if res["ok"].as_bool().unwrap_or(false) {
                    println!("Killed process on port {} (pid {})", port, res["killedPid"]);
                } else {
                    eprintln!("No process found listening on port {}", port);
                    exit_code = 1;
                }
            }
            Cmd::Deps => { cmd_deps()?; }
            Cmd::Doctor => { cmd_doctor()?; }
            Cmd::Recall { query, limit, cwd, discipline } => {
                let mut q_parts = query;
                let disc = extract_discipline_sigil(&mut q_parts, discipline);
                let q = q_parts.join(" ");
                if q.trim().is_empty() { eprintln!("No query provided"); exit_code = 1; return Ok(()); }
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                let out = hook::rs_learn::recall_disc(&q, &dir, limit, disc.as_deref());
                if out.is_empty() { eprintln!("No recall results"); exit_code = 1; return Ok(()); }
                println!("{}", out);
            }
            Cmd::Memorize { source, file, content, cwd, discipline } => {
                let mut body_parts = content;
                let disc = extract_discipline_sigil(&mut body_parts, discipline);
                let body = if let Some(f) = file {
                    fs::read_to_string(&f)?
                } else {
                    body_parts.join(" ")
                };
                if body.trim().is_empty() { eprintln!("No content provided"); exit_code = 1; return Ok(()); }
                let src = source.unwrap_or_else(|| "memorize".into());
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                hook::rs_learn::ingest_fast_disc(&body, &src, &dir, disc.as_deref());
                let project_name = std::path::Path::new(&dir).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                rs_exec::obs::event("rs_learn", "memorize", serde_json::json!({
                    "bytes": body.len(),
                    "source": src,
                    "project": project_name,
                    "discipline": disc.clone().unwrap_or_else(|| "default".into())
                }));
                println!("ingested ({} bytes) source={} discipline={}", body.len(), src, disc.unwrap_or_else(|| "default".into()));
            }
            Cmd::Forget { directive, cwd, discipline } => {
                let mut directive_parts = directive;
                let disc = extract_discipline_sigil(&mut directive_parts, discipline);
                let joined = directive_parts.join(" ");
                let mut parts = joined.splitn(2, ' ');
                let kind = parts.next().unwrap_or("").trim();
                let target = parts.next().unwrap_or("").trim();
                if kind.is_empty() || target.is_empty() {
                    eprintln!("usage: plugkit forget by-source <tag> | by-query <terms> | by-id <uuid>");
                    exit_code = 1; return Ok(());
                }
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                match hook::rs_learn::forget_disc(kind, target, &dir, disc.as_deref()) {
                    Ok(n) => println!("forgot {} episode(s)", n),
                    Err(e) => { eprintln!("forget failed: {}", e); exit_code = 1; }
                }
            }
            Cmd::Learn { action, rest, cwd, discipline } => {
                let mut rest_parts = rest;
                let disc = extract_discipline_sigil(&mut rest_parts, discipline);
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                let out = hook::rs_learn::learn_passthrough_disc(&action, &rest_parts, &dir, disc.as_deref());
                if out.is_empty() {
                    eprintln!("learn {} returned no output (rs-learn may not be available)", action);
                    exit_code = 1;
                } else {
                    println!("{}", out);
                }
            }
            Cmd::Discipline { sub, rest, cwd } => {
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                exit_code = cmd_discipline(&sub, &rest, &dir);
            }
            Cmd::Log { action, rest, limit, date, days } => {
                exit_code = cmd_log(&action, &rest, limit, date.as_deref(), days);
            }
            Cmd::Search { path, query, discipline } => {
                let mut q_parts = query;
                let disc = extract_discipline_sigil(&mut q_parts, discipline);
                if q_parts.is_empty() {
                    search_mcp::run_mcp_server();
                    return Ok(());
                }
                let q = q_parts.join(" ");
                let root = std::path::PathBuf::from(path.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string()));
                if !root.exists() { eprintln!("Path does not exist: {}", root.display()); exit_code = 1; return Ok(()); }
                let project_dir = root.to_string_lossy().to_string();
                let enabled = list_enabled_disciplines(&project_dir);
                let started = std::time::Instant::now();
                let labels: Vec<String> = if let Some(name) = disc.as_deref() {
                    vec![name.to_string()]
                } else if enabled.is_empty() {
                    vec!["default".to_string()]
                } else {
                    let mut v = vec!["default".to_string()];
                    for d in &enabled { v.push(d.clone()); }
                    v
                };
                let mut total_chunks: usize = 0;
                let mut total_results: usize = 0;
                let mut printed_any = false;
                for label in &labels {
                    let cs_dir = if label == "default" {
                        root.join(".gm").join("code-search")
                    } else {
                        root.join(".gm").join("disciplines").join(label).join("code-search")
                    };
                    std::env::set_var("RS_CODEINSIGHT_CACHE_DIR", &cs_dir);
                    std::env::set_var("RS_SEARCH_DISCIPLINE", label);
                    let chunks = scanner::scan_repository(&root);
                    let results = bm25::search(&q, &chunks);
                    total_chunks += chunks.len();
                    total_results += results.len();
                    if results.is_empty() { continue; }
                    printed_any = true;
                    for r in results.iter() {
                        let total = context::get_file_total_lines(&root, &r.chunk.file_path).map(|n| format!(" [{}L]", n)).unwrap_or_default();
                        let ctx = context::find_enclosing_context(&r.chunk.content, r.chunk.line_start).map(|c| format!(" (in: {})", c)).unwrap_or_default();
                        println!("[discipline:{}] {}:{}-{}{}{} ({:.1}%)", label, r.chunk.file_path, r.chunk.line_start, r.chunk.line_end, total, ctx, r.score * 100.0);
                        for line in r.chunk.content.split('\n').take(3) { println!("   > {}", &line[..line.len().min(80)]); }
                        println!();
                    }
                }
                std::env::remove_var("RS_CODEINSIGHT_CACHE_DIR");
                std::env::remove_var("RS_SEARCH_DISCIPLINE");
                rs_exec::obs::event("rs_search", "query", serde_json::json!({
                    "root": root.display().to_string(),
                    "q_len": q.len(),
                    "n_chunks": total_chunks,
                    "n_results": total_results,
                    "n_disciplines": labels.len(),
                    "discipline": disc.clone().unwrap_or_else(|| "default".into()),
                    "dur_ms": started.elapsed().as_millis() as u64
                }));
                if !printed_any { println!("No results found."); return Ok(()); }
            }
        }
        Ok(())
    }.await;

    if let Err(e) = result { eprintln!("Error: {}", e); exit_code = 1; }
    std::process::exit(exit_code);
}

#[cfg(test)]
mod exec_lang_flag_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn exec_parses_lang_flag_then_positional_code() {
        let cli = Cli::try_parse_from(["plugkit", "exec", "--lang", "nodejs", "console.log(1)"]).unwrap();
        match cli.command {
            Cmd::Exec { lang, code, .. } => {
                assert_eq!(lang.as_deref(), Some("nodejs"));
                assert_eq!(code, vec!["console.log(1)".to_string()]);
            }
            _ => panic!("expected Cmd::Exec"),
        }
    }

    #[test]
    fn exec_parses_multi_word_code() {
        let cli = Cli::try_parse_from(["plugkit", "exec", "--lang", "python", "print(1)", "print(2)"]).unwrap();
        match cli.command {
            Cmd::Exec { lang, code, .. } => {
                assert_eq!(lang.as_deref(), Some("python"));
                assert_eq!(code, vec!["print(1)".to_string(), "print(2)".to_string()]);
            }
            _ => panic!("expected Cmd::Exec"),
        }
    }

    #[test]
    fn exec_requires_lang_as_flag_not_positional() {
        let cli = Cli::try_parse_from(["plugkit", "exec", "nodejs", "console.log(1)"]).unwrap();
        match cli.command {
            Cmd::Exec { lang, code, .. } => {
                assert_eq!(lang, None, "first positional should NOT be treated as lang");
                assert_eq!(code, vec!["nodejs".to_string(), "console.log(1)".to_string()]);
            }
            _ => panic!("expected Cmd::Exec"),
        }
    }

    #[test]
    fn exec_with_file_flag() {
        let cli = Cli::try_parse_from(["plugkit", "exec", "--lang", "bash", "--file", "/tmp/x.sh"]).unwrap();
        match cli.command {
            Cmd::Exec { lang, file, code, .. } => {
                assert_eq!(lang.as_deref(), Some("bash"));
                assert_eq!(file.as_deref(), Some("/tmp/x.sh"));
                assert!(code.is_empty());
            }
            _ => panic!("expected Cmd::Exec"),
        }
    }

    #[test]
    fn exec_with_session_flag() {
        let cli = Cli::try_parse_from(["plugkit", "exec", "--lang", "nodejs", "--session=sid-1", "code"]).unwrap();
        match cli.command {
            Cmd::Exec { lang, session, .. } => {
                assert_eq!(lang.as_deref(), Some("nodejs"));
                assert_eq!(session.as_deref(), Some("sid-1"));
            }
            _ => panic!("expected Cmd::Exec"),
        }
    }
}
