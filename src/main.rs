mod hook;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::{env, fs, path::PathBuf, time::Duration};

use rs_exec::{daemon, rpc_client};
use rs_codeinsight::{analyze, AnalyzeOptions};
use rs_search::{bm25, context, mcp as search_mcp, scanner};

const HARD_CEILING_MS: u64 = 15000;
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
        code: Vec<String>,
    },
    Bash {
        #[arg(long)] cwd: Option<String>,
        commands: Vec<String>,
    },
    Status { task_id: String },
    Sleep { task_id: String, seconds: Option<u64>, #[arg(long)] next_output: bool },
    Close { task_id: String },
    #[command(name = "type")] Type { task_id: String, input: Vec<String> },
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
}

fn runner_exe_stamp() -> PathBuf {
    env::temp_dir().join("plugkit-runner.exe-stamp")
}

fn current_exe_stamp() -> String {
    let exe = self_exe();
    let meta = fs::metadata(&exe).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok()).map(|t| format!("{:?}", t)).unwrap_or_default();
    let size = meta.map(|m| m.len()).unwrap_or(0);
    format!("{}|{}|{}", exe, size, mtime)
}

fn runner_needs_restart() -> bool {
    let stamp_file = runner_exe_stamp();
    let current = current_exe_stamp();
    match fs::read_to_string(&stamp_file) {
        Ok(stored) => stored.trim() != current,
        Err(_) => true,
    }
}

async fn ensure_runner() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_millis(5000), async {
        if rpc_client::health_check().await {
            if !runner_needs_restart() { return Ok(()); }
            daemon::kill(RUNNER_NAME);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        daemon::start(RUNNER_NAME, &self_exe(), &["--runner-mode"])?;
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

async fn run_code(code: &str, runtime: &str, cwd: &str, session_id: Option<&str>) -> anyhow::Result<i32> {
    ensure_runner().await?;
    let mut create_params = json!({ "code": code, "runtime": runtime, "workingDirectory": cwd });
    if let Some(sid) = session_id { create_params["sessionId"] = json!(sid); }
    let task_id_val = rpc_client::rpc_call("createTask", create_params, 10000).await?;
    let task_id = task_id_val["taskId"].as_u64().unwrap_or(0);

    let safety = tokio::time::sleep(Duration::from_millis(HARD_CEILING_MS));
    tokio::pin!(safety);

    let exec_fut = async {
        tokio::time::timeout(
            Duration::from_millis(HARD_CEILING_MS),
            rpc_client::rpc_call(
                "execute",
                json!({ "code": code, "runtime": runtime, "workingDirectory": cwd, "timeout": HARD_CEILING_MS, "backgroundTaskId": task_id, "sessionId": session_id }),
                HARD_CEILING_MS + 5000,
            )
        ).await
    };

    let result = tokio::select! {
        r = exec_fut => match r {
            Ok(Ok(v)) => v["result"].clone(),
            Ok(Err(e)) => json!({ "error": e.to_string() }),
            Err(_) => json!({ "backgroundTaskId": task_id, "persisted": true }),
        },
        _ = safety => json!({ "backgroundTaskId": task_id, "persisted": true }),
    };

    if result["persisted"].as_bool().unwrap_or(false) || (result["backgroundTaskId"].is_u64() && !result["completed"].as_bool().unwrap_or(false)) {
        let id = format!("task_{}", result["backgroundTaskId"].as_u64().unwrap_or(task_id));
        let partial = tokio::time::timeout(Duration::from_millis(2000), rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": task_id }), 2000)).await.ok().and_then(|r| r.ok());
        if let Some(out) = partial {
            if let Some(arr) = out["output"].as_array() {
                for entry in arr {
                    let d = entry["d"].as_str().unwrap_or("");
                    if entry["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); }
                }
            }
        }
        println!("\nStill running after 15s — backgrounded.\nTask ID: {}\n", id);
        println!("  plugkit sleep {}       # wait for completion", id);
        println!("  plugkit status {}      # drain output buffer", id);
        println!("  plugkit close {}       # delete task when done", id);
        std::process::exit(0);
    }

    if result["backgroundTaskId"].is_u64() && result["completed"].as_bool().unwrap_or(false) {
        let _ = rpc_client::rpc_call("deleteTask", json!({ "taskId": result["backgroundTaskId"] }), 5000).await;
    } else {
        let _ = rpc_client::rpc_call("deleteTask", json!({ "taskId": task_id }), 5000).await;
    }

    if let Some(s) = result["stdout"].as_str() { if !s.is_empty() { print!("{}", s); } }
    if let Some(s) = result["stderr"].as_str() { if !s.is_empty() { eprint!("{}", s); } }
    if let Some(e) = result["error"].as_str() { if !e.is_empty() { eprintln!("Error: {}", e); return Ok(1); } }

    let exit_code = result["exitCode"].as_i64().unwrap_or(0) as i32;
    if result["success"].as_bool() == Some(false) { return Ok(if exit_code != 0 { exit_code } else { 1 }); }
    Ok(exit_code)
}

async fn cmd_status(task_id_str: &str) -> anyhow::Result<()> {
    ensure_runner().await?;
    let raw_id = parse_task_id(task_id_str);
    let task = rpc_client::rpc_call("getTask", json!({ "taskId": raw_id }), 10000).await?;
    let task = &task["task"];
    if task.is_null() { eprintln!("Task not found"); std::process::exit(1); }
    let status = task["status"].as_str().unwrap_or("unknown");
    println!("Status: {}", status);
    if let Some(r) = task["result"].as_object() {
        if let Some(s) = r.get("stdout").and_then(|v| v.as_str()) { if !s.is_empty() { print!("{}", s); } }
        if let Some(s) = r.get("stderr").and_then(|v| v.as_str()) { if !s.is_empty() { eprint!("{}", s); } }
        if let Some(e) = r.get("error").and_then(|v| v.as_str()) { if !e.is_empty() { eprintln!("Error: {}", e); } }
    }
    let output = rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": raw_id }), 5000).await?;
    if let Some(arr) = output["output"].as_array() {
        for e in arr { let d = e["d"].as_str().unwrap_or(""); if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); } }
    }
    if status == "running" {
        println!("\nTask still running. Options:");
        println!("  plugkit sleep {}      # wait for completion (up to 30s) — recommended", task_id_str);
        println!("  plugkit type {} <input>  # send stdin to running task", task_id_str);
        println!("  plugkit status {}     # check status again (snapshot)", task_id_str);
    } else if status == "completed" || status == "failed" {
        println!("\nTask finished. Clean up:");
        println!("  plugkit close {}      # delete task", task_id_str);
        println!("  plugkit runner stop          # stop runner if no more tasks");
    }
    Ok(())
}

async fn cmd_sleep(task_id_str: &str, secs: u64, next_output: bool) -> anyhow::Result<()> {
    ensure_runner().await?;
    let raw_id = parse_task_id(task_id_str);
    let timeout = Duration::from_secs(secs);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= timeout { break; }
        let task = rpc_client::rpc_call("getTask", json!({ "taskId": raw_id }), 5000).await?;
        let task = &task["task"];
        if task.is_null() { println!("Task not found or already completed."); return Ok(()); }
        let output = rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": raw_id }), 5000).await?;
        if let Some(arr) = output["output"].as_array() {
            for e in arr { let d = e["d"].as_str().unwrap_or(""); if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); } }
        }
        let status = task["status"].as_str().unwrap_or("");
        if status != "running" && status != "pending" {
            if let Some(e) = task["result"]["error"].as_str() { if !e.is_empty() { eprintln!("Error: {}", e); } }
            println!("\nTask finished ({}). Clean up:", status);
            println!("  plugkit close {}      # delete task", task_id_str);
            println!("  plugkit runner stop          # stop runner if no more tasks");
            return Ok(());
        }
        if next_output {
            let remaining = timeout.saturating_sub(start.elapsed()).min(Duration::from_secs(900));
            let _ = rpc_client::rpc_call("waitForOutput", json!({ "taskId": raw_id, "timeoutMs": remaining.as_millis() as u64 }), remaining.as_millis() as u64 + 5000).await;
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    println!("\nTimeout after {}s. Task still running.", secs);
    println!("  plugkit sleep {}       # wait again (up to 15m) — recommended", task_id_str);
    println!("  plugkit status {}      # check current status (snapshot)", task_id_str);
    Ok(())
}

#[tokio::main]
async fn main() {
    if env::args().any(|a| a == "--exec-process-mode") {
        rs_exec::run_exec_process();
        return;
    }

    if env::args().any(|a| a == "--runner-mode") {
        rs_exec::runner::run_server().await.expect("Runner failed");
        return;
    }

    let cli = Cli::parse();
    let mut exit_code = 0i32;

    let result: anyhow::Result<()> = async {
        match cli.command {
            Cmd::Exec { lang, cwd, file, session, code } => {
                let code_str = if let Some(ref f) = file { fs::read_to_string(f)? } else { code.join(" ") };
                if let Some(ref f) = file { let _ = fs::remove_file(f); }
                if code_str.trim().is_empty() { eprintln!("No code provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let mut runtime = lang.unwrap_or_else(|| "nodejs".into());
                if runtime == "typescript" || runtime == "auto" { runtime = "nodejs".into(); }
                exit_code = run_code(&code_str, &runtime, &cwd, session.as_deref()).await?;
            }
            Cmd::Bash { cwd, commands } => {
                let cmd = commands.join(" ");
                if cmd.trim().is_empty() { eprintln!("No commands provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let runtime = if cfg!(windows) { "powershell" } else { "bash" };
                exit_code = run_code(&cmd, runtime, &cwd, None).await?;
            }
            Cmd::Status { task_id } => cmd_status(&task_id).await?,
            Cmd::Sleep { task_id, seconds, next_output } => cmd_sleep(&task_id, seconds.unwrap_or(30), next_output).await?,
            Cmd::Close { task_id } => {
                ensure_runner().await?;
                rpc_client::rpc_call("deleteTask", json!({ "taskId": parse_task_id(&task_id) }), 10000).await?;
                println!("Task {} closed", task_id);
                let res = rpc_client::rpc_call("listTasks", json!({}), 5000).await.unwrap_or_default();
                let remaining: Vec<_> = res["tasks"].as_array().map(|a| {
                    a.iter().filter(|t| matches!(t["status"].as_str(), Some("running") | Some("pending"))).collect()
                }).unwrap_or_default();
                if !remaining.is_empty() {
                    println!("\n{} task(s) still running:", remaining.len());
                    for t in &remaining {
                        println!("  plugkit sleep task_{}       # wait for completion (up to 30s)", t["id"]);
                    }
                } else {
                    println!("  plugkit runner stop          # no more tasks — stop runner");
                }
            }
            Cmd::Type { task_id, input } => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("sendStdin", json!({ "taskId": parse_task_id(&task_id), "data": format!("{}\n", input.join(" ")) }), 10000).await?;
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
                    "session-end" => { hook::session_end(); return Ok(()); }
                    "stop" => { hook::run_stop(); return Ok(()); }
                    "stop-git" => { hook::run_stop_git(); return Ok(()); }
                    other => { eprintln!("Unknown hook event: {}", other); exit_code = 1; return Ok(()); }
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
