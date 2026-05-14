use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use std::env;
use notify::{RecursiveMode, Watcher, RecommendedWatcher, Result as NotifyResult};
use std::sync::mpsc;

pub fn run_spool_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let exec_spool = PathBuf::from(home_dir()?)
        .join(".gm")
        .join("exec-spool");

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
                    .filter(|e| e.is_file())
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
                    .filter(|e| e.is_file())
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
        dispatch_to_spool_verb(lang_or_verb, file_id, content);
        (String::new(), String::new(), 0)
    } else {
        (
            format!("Unknown lang/verb: {}", lang_or_verb),
            String::new(),
            1,
        )
    }
}

fn dispatch_to_exec_rpc(lang: &str, file_id: &str, content: &str) -> (String, String, i32) {
    let home = match home_dir() {
        Ok(h) => h,
        Err(_) => {
            return (String::new(), "Cannot resolve HOME".to_string(), 1);
        }
    };
    let home_path = PathBuf::from(home);
    let spool_in = home_path.join(".gm").join("exec-spool").join("in");

    let runtime = match lang {
        "typescript" => "nodejs",
        other => other,
    };

    let lang_dir = spool_in.join(runtime);
    let _ = fs::create_dir_all(&lang_dir);

    let ext = match lang {
        "nodejs" | "typescript" => "js",
        "python" => "py",
        "bash" => "sh",
        "rust" => "rs",
        "go" => "go",
        "c" => "c",
        "cpp" => "cpp",
        "java" => "java",
        "deno" => "ts",
        _ => "txt",
    };

    let task_file = lang_dir.join(format!("{}.{}", file_id, ext));
    if let Err(e) = fs::write(&task_file, content) {
        let stderr = format!("Failed to write dispatch file: {}", e);
        return (String::new(), stderr, 1);
    }

    eprintln!("[spool] dispatched {} task {} to {}", runtime, file_id, task_file.display());
    (format!("Dispatched {} task {} to runner", runtime, file_id), String::new(), 0)
}

fn dispatch_to_spool_verb(verb: &str, file_id: &str, content: &str) {
    let home = home_dir().ok();
    if home.is_none() {
        eprintln!("[spool] cannot resolve HOME for verb dispatch");
        return;
    }
    let home_path = PathBuf::from(home.unwrap());
    let spool_in = home_path.join(".gm").join("exec-spool").join("in");

    let verb_dir = spool_in.join(verb);
    let _ = fs::create_dir_all(&verb_dir);

    let task_file = verb_dir.join(format!("{}.txt", file_id));
    let _ = fs::write(task_file, content);
    eprintln!("[spool] dispatched {} task {}", verb, file_id);
}

fn home_dir() -> Result<String, Box<dyn std::error::Error>> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

pub fn run_spool_once() -> Result<(), Box<dyn std::error::Error>> {
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
        .filter(|e| e.is_file())
    {
        dispatch_file(entry.path(), &output_root);
    }

    Ok(())
}
