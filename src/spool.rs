use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::env;
use notify::{RecursiveMode, Watcher, RecommendedWatcher};
use std::sync::mpsc;

pub fn run_spool_daemon() -> Result<(), anyhow::Error> {
    let exec_spool = if let Ok(custom) = env::var("SPOOL_DIR") {
        PathBuf::from(custom)
    } else if let Ok(custom) = env::var("RS_EXEC_SPOOL_DIR") {
        PathBuf::from(custom)
    } else if let Ok(project) = env::var("CLAUDE_PROJECT_DIR") {
        PathBuf::from(project).join(".gm").join("exec-spool")
    } else {
        PathBuf::from(home_dir()?)
            .join(".gm")
            .join("exec-spool")
    };

    let pending_root = exec_spool.join("in");
    if !pending_root.exists() {
        fs::create_dir_all(&pending_root)?;
    }

    let output_root = exec_spool.join("out");
    if !output_root.exists() {
        fs::create_dir_all(&output_root)?;
    }

    eprintln!("[spool] watching {}", pending_root.display());

    let (tx, rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(
        tx,
        notify::Config::default().with_poll_interval(Duration::from_millis(100)),
    )?;

    watcher.watch(&pending_root, RecursiveMode::Recursive)?;

    let mut pending_paths: HashSet<PathBuf> = HashSet::new();
    let mut last_debounce: HashMap<PathBuf, SystemTime> = HashMap::new();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(_notify_event)) => {
                for entry in walkdir::WalkDir::new(&pending_root)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    let path = entry.path();
                    if !path.exists() {
                        continue;
                    }

                    let mtime = fs::metadata(path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or_else(SystemTime::now);

                    let last = last_debounce.get(path).copied().unwrap_or(SystemTime::UNIX_EPOCH);
                    let elapsed = mtime.elapsed().unwrap_or(Duration::from_secs(10));

                    if elapsed < Duration::from_millis(250) {
                        pending_paths.insert(path.to_path_buf());
                    } else if pending_paths.contains(path) {
                        pending_paths.remove(path);
                        last_debounce.insert(path.to_path_buf(), mtime);
                        dispatch_file(path, &output_root);
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = SystemTime::now();
                let mut to_dispatch = Vec::new();

                for entry in walkdir::WalkDir::new(&pending_root)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    let path = entry.path();
                    if !path.exists() {
                        continue;
                    }

                    let mtime = fs::metadata(path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or_else(SystemTime::now);

                    let elapsed = now.duration_since(mtime).unwrap_or(Duration::from_secs(10));

                    if elapsed >= Duration::from_millis(250) && pending_paths.contains(path) {
                        to_dispatch.push(path.to_path_buf());
                    }
                }

                for path in to_dispatch {
                    pending_paths.remove(&path);
                    last_debounce.insert(path.clone(), SystemTime::now());
                    dispatch_file(&path, &output_root);
                }
            }
        }
    }

    Ok(())
}

fn dispatch_file(input_path: &Path, output_root: &Path) {
    let relative = input_path
        .strip_prefix(
            input_path
                .ancestors()
                .nth(1)
                .and_then(|p| p.parent())
                .unwrap_or_else(|| Path::new("/")),
        )
        .unwrap_or(input_path);

    let parts: Vec<_> = relative.components().collect();
    if parts.len() < 2 {
        return;
    }

    let lang_or_verb = parts[0]
        .as_os_str()
        .to_string_lossy()
        .to_string();
    let file_name = parts[1]
        .as_os_str()
        .to_string_lossy()
        .to_string();

    let file_id = file_name.split('.').next().unwrap_or(&file_name).to_string();

    match fs::read_to_string(input_path) {
        Ok(content) => {
            eprintln!("[spool] dispatch {} {}", lang_or_verb, file_id);

            let stdout_path = output_root.join(format!("{}.out", file_id));
            let stderr_path = output_root.join(format!("{}.err", file_id));
            let json_path = output_root.join(format!("{}.json", file_id));

            let (stdout, stderr, exit_code) = execute_dispatch(&lang_or_verb, &file_id, &content);

            let _ = fs::write(stdout_path, stdout);
            let _ = fs::write(stderr_path, stderr);

            let metadata = serde_json::json!({
                "taskId": file_id,
                "lang": lang_or_verb,
                "ok": exit_code == 0,
                "exitCode": exit_code,
                "durationMs": 0,
                "timedOut": false,
                "startedAt": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis(),
                "endedAt": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis(),
            });

            let _ = fs::write(json_path, serde_json::to_string_pretty(&metadata).unwrap_or_default());

            let _ = fs::remove_file(input_path);
        }
        Err(e) => {
            eprintln!("[spool] error reading {}: {}", input_path.display(), e);
        }
    }
}

fn execute_dispatch(lang_or_verb: &str, file_id: &str, content: &str) -> (String, String, i32) {
    let is_lang = matches!(lang_or_verb,
        "nodejs" | "python" | "bash" | "rust" | "typescript" | "go" | "c" | "cpp" | "java" | "deno"
    );
    let is_verb = matches!(lang_or_verb,
        "codesearch" | "recall" | "memorize"
    );

    if is_lang {
        dispatch_to_exec_rpc(lang_or_verb, file_id, content)
    } else if is_verb {
        dispatch_to_spool_verb(lang_or_verb, file_id, content)
    } else {
        (
            format!("Unknown lang/verb: {}", lang_or_verb),
            String::new(),
            1,
        )
    }
}

fn find_plugkit() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "plugkit.exe" } else { "plugkit" };
    
    which::which(exe_name).ok()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .map(|p| p.parent().map(|d| d.join(exe_name)).unwrap_or_else(|| PathBuf::from(exe_name)))
        })
        .or_else(|| std::env::var("HOME").ok().map(PathBuf::from).map(|h| h.join(".claude").join("gm-tools").join(exe_name)))
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from).map(|h| h.join(".claude").join("gm-tools").join(exe_name)))
}

fn dispatch_to_exec_rpc(lang: &str, file_id: &str, content: &str) -> (String, String, i32) {
    use std::process::{Command, Stdio};
    
    let runtime = match lang {
        "typescript" => "nodejs",
        "rust" => "nodejs",
        "go" => "nodejs",
        "c" => "nodejs",
        "cpp" => "nodejs",
        "java" => "nodejs",
        "deno" => "nodejs",
        other => other,
    };

    let plugkit_path = find_plugkit()
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "plugkit.exe" } else { "plugkit" }));
    
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    
    let session_id = std::env::var("SESSION_ID").unwrap_or_else(|_| "spool".to_string());
    
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!("spool-{}.{}", file_id, if runtime == "nodejs" { "js" } else { "txt" }));
    let _ = std::fs::write(&temp_file, content);
    
    let output = Command::new(&plugkit_path)
        .args(["exec", "--lang", runtime, "--session", &session_id, "--timeout-ms", "300000", "--file", temp_file.to_str().unwrap_or("")])
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let _ = std::fs::remove_file(&temp_file);

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let exit_code = out.status.code().unwrap_or(1);
            
            eprintln!("[spool] executed {} task {}: exit={}", runtime, file_id, exit_code);
            (stdout, stderr, exit_code)
        }
        Err(e) => {
            let stderr = format!("Failed to execute: {}", e);
            eprintln!("[spool] {}", stderr);
            (String::new(), stderr, 1)
        }
    }
}

fn dispatch_to_spool_verb(verb: &str, file_id: &str, content: &str) -> (String, String, i32) {
    use std::process::{Command, Stdio};
    
    let plugkit_path = find_plugkit()
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "plugkit.exe" } else { "plugkit" }));
    
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    
    let output = match verb {
        "codesearch" => {
            Command::new(&plugkit_path)
                .args(["search", &content])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        }
        "recall" => {
            Command::new(&plugkit_path)
                .args(["recall", &content])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        }
        "memorize" => {
            Command::new(&plugkit_path)
                .args(["memorize", &content])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
        }
        _ => {
            return (format!("Unknown verb: {}", verb), String::new(), 1);
        }
    };

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let exit_code = out.status.code().unwrap_or(0);
            
            eprintln!("[spool] executed verb {} task {}: exit={}", verb, file_id, exit_code);
            (stdout, stderr, exit_code)
        }
        Err(e) => {
            let stderr = format!("Failed to execute {}: {}", verb, e);
            eprintln!("[spool] {}", stderr);
            (String::new(), stderr, 1)
        }
    }
}

fn home_dir() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
}

pub fn run_spool_once() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let exec_spool = PathBuf::from(home_dir()?)
        .join(".gm")
        .join("exec-spool");

    let pending_root = exec_spool.join("in");
    let output_root = exec_spool.join("out");

    if !pending_root.exists() {
        return Ok(());
    }

for entry in walkdir::WalkDir::new(&pending_root)
                     .into_iter()
                     .filter_map(|e| e.ok())
                     .filter(|e| e.file_type().is_file())
                 {
        dispatch_file(entry.path(), &output_root);
    }

    Ok(())
}
