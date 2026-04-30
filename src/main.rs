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
    },
    SessionCleanup {
        #[arg(long)] session: String,
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
    },
    /// Ingest a fact into rs-learn fast-path (HTTP-preferred, bun fallback). Detached.
    Memorize {
        #[arg(long)] source: Option<String>,
        #[arg(long)] file: Option<String>,
        content: Vec<String>,
        #[arg(long)] cwd: Option<String>,
    },
    /// Invalidate / unlearn previously-memorized facts. Directives: `by-source <tag>` | `by-query <query>` | `by-id <episode_id>`.
    Forget {
        directive: Vec<String>,
        #[arg(long)] cwd: Option<String>,
    },
    /// Pass-through to rs-learn for status/debug/feedback/build-communities. HTTP-preferred, bun fallback.
    Learn {
        action: String,
        rest: Vec<String>,
        #[arg(long)] cwd: Option<String>,
    },
}

const RS_EXEC_SHA: &str = env!("DEP_RS_EXEC_SHA");
const RS_SEARCH_SHA: &str = env!("DEP_RS_SEARCH_SHA");
const RS_CODEINSIGHT_SHA: &str = env!("DEP_RS_CODEINSIGHT_SHA");

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
        _ => {
            eprintln!("[plugkit] warn: --timeout-ms not set; applying transitional default {} ms (set --timeout-ms explicitly to silence)", DEFAULT_EXEC_TIMEOUT_MS);
            DEFAULT_EXEC_TIMEOUT_MS
        }
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
            Cmd::Codeinsight { path, json, cache, read_cache } => {
                let root = path.unwrap_or_else(|| ".".into());
                let root_path = std::path::Path::new(&root);
                if !root_path.exists() { eprintln!("Path does not exist: {}", root); exit_code = 1; return Ok(()); }
                if read_cache {
                    match fs::read_to_string(root_path.join(".codeinsight")) {
                        Ok(c) => { print!("{}", c); return Ok(()); }
                        Err(_) => { eprintln!("No cache found"); exit_code = 1; return Ok(()); }
                    }
                }
                let output = analyze(root_path, AnalyzeOptions { json_mode: json });
                println!("{}", output.text);
                if cache {
                    let _ = fs::write(root_path.join(".codeinsight"), &output.text);
                }
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
                match event.as_str() {
                    "session-start" => { hook::session_start(); return Ok(()); }
                    "pre-tool-use" => { hook::pre_tool_use(); return Ok(()); }
                    "prompt-submit" => { hook::prompt_submit(); return Ok(()); }
                    "pre-compact" => { hook::pre_compact(); return Ok(()); }
                    "session-end" => { hook::session_end(); return Ok(()); }
                    "stop" => { hook::run_stop(); return Ok(()); }
                    "stop-git" => { hook::run_stop_git(); return Ok(()); }
                    other => { eprintln!("Unknown hook event: {}", other); exit_code = 1; return Ok(()); }
                }
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
            Cmd::Recall { query, limit, cwd } => {
                let q = query.join(" ");
                if q.trim().is_empty() { eprintln!("No query provided"); exit_code = 1; return Ok(()); }
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                let out = hook::rs_learn::recall(&q, &dir, limit);
                if out.is_empty() { eprintln!("No recall results"); exit_code = 1; return Ok(()); }
                println!("{}", out);
            }
            Cmd::Memorize { source, file, content, cwd } => {
                let body = if let Some(f) = file {
                    fs::read_to_string(&f)?
                } else {
                    content.join(" ")
                };
                if body.trim().is_empty() { eprintln!("No content provided"); exit_code = 1; return Ok(()); }
                let src = source.unwrap_or_else(|| "memorize".into());
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                hook::rs_learn::ingest_fast(&body, &src, &dir);
                println!("ingested ({} bytes) source={}", body.len(), src);
            }
            Cmd::Forget { directive, cwd } => {
                let joined = directive.join(" ");
                let mut parts = joined.splitn(2, ' ');
                let kind = parts.next().unwrap_or("").trim();
                let target = parts.next().unwrap_or("").trim();
                if kind.is_empty() || target.is_empty() {
                    eprintln!("usage: plugkit forget by-source <tag> | by-query <terms> | by-id <uuid>");
                    exit_code = 1; return Ok(());
                }
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                match hook::rs_learn::forget(kind, target, &dir) {
                    Ok(n) => println!("forgot {} episode(s)", n),
                    Err(e) => { eprintln!("forget failed: {}", e); exit_code = 1; }
                }
            }
            Cmd::Learn { action, rest, cwd } => {
                let dir = cwd.unwrap_or_else(|| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
                let out = hook::rs_learn::learn_passthrough(&action, &rest, &dir);
                if out.is_empty() {
                    eprintln!("learn {} returned no output (rs-learn may not be available)", action);
                    exit_code = 1;
                } else {
                    println!("{}", out);
                }
            }
            Cmd::Search { path, query } => {
                if query.is_empty() {
                    search_mcp::run_mcp_server();
                    return Ok(());
                }
                let q = query.join(" ");
                let root = std::path::PathBuf::from(path.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string()));
                if !root.exists() { eprintln!("Path does not exist: {}", root.display()); exit_code = 1; return Ok(()); }
                let chunks = scanner::scan_repository(&root);
                let results = bm25::search(&q, &chunks);
                if results.is_empty() { println!("No results found."); return Ok(()); }
                for r in results.iter() {
                    let total = context::get_file_total_lines(&root, &r.chunk.file_path).map(|n| format!(" [{}L]", n)).unwrap_or_default();
                    let ctx = context::find_enclosing_context(&r.chunk.content, r.chunk.line_start).map(|c| format!(" (in: {})", c)).unwrap_or_default();
                    println!("{}:{}-{}{}{} ({:.1}%)", r.chunk.file_path, r.chunk.line_start, r.chunk.line_end, total, ctx, r.score * 100.0);
                    for line in r.chunk.content.split('\n').take(3) { println!("   > {}", &line[..line.len().min(80)]); }
                    println!();
                }
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
