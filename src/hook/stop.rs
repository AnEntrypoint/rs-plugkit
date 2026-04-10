use serde_json::json;
use std::{env, fs, path::Path, process::Command};

pub fn run_stop() {
    let session_id = env::var("CLAUDE_SESSION_ID").unwrap_or_default();
    let open = if !session_id.is_empty() {
        super::agent_browser::get_open_session_ids(&session_id)
    } else {
        vec![]
    };
    if !open.is_empty() {
        let ids = open.join(", ");
        let out = json!({
            "decision": "block",
            "reason": format!("Open browser session(s): [{}]. Close them before stopping:\n  exec:browser\n  await page.close()\n\nOr use `exec:close` to clean up background tasks.\n\nHousekeeping policy: always close browser sessions and background tasks before ending a conversation.", ids)
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        std::process::exit(2);
    }

    let project_dir = env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
    let prd = std::path::Path::new(&project_dir).join(".prd");

    if prd.exists() {
        let content = fs::read_to_string(&prd).unwrap_or_default();
        let trimmed = content.trim();
        let is_empty_array = serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .and_then(|v| v.as_array().map(|a| a.is_empty()))
            .unwrap_or(false);
        if !trimmed.is_empty() && !is_empty_array {
            let out = json!({
                "decision": "block",
                "reason": format!("Work items remain in {}. Remove completed items as they finish. Delete the file when all items are done.\n\n{}", prd.display(), trimmed)
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            std::process::exit(2);
        }
    }

    run_stop_git();
}

fn hash_path(s: &str) -> String {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{:016x}", h)
}

fn counter_path(project_dir: &str) -> std::path::PathBuf {
    let hash = hash_path(project_dir);
    env::temp_dir().join(format!("gm-git-block-counter-{}.json", hash))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Counter {
    count: u64,
    #[serde(rename = "lastGitHash")]
    last_git_hash: Option<String>,
}

fn read_counter(path: &Path) -> Counter {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_counter(path: &Path, c: &Counter) {
    let _ = fs::write(path, serde_json::to_string_pretty(c).unwrap_or_default());
}

fn git(args: &[&str], dir: &str) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn run_stop_git() {
    let project_dir = env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| env::current_dir().unwrap_or_default().to_string_lossy().to_string());

    if git(&["rev-parse", "--git-dir"], &project_dir).is_none() {
        println!("{}", json!({ "decision": "approve" }));
        return;
    }

    let current_hash = git(&["rev-parse", "HEAD"], &project_dir);
    let cpath = counter_path(&project_dir);
    let mut counter = read_counter(&cpath);

    if let (Some(last), Some(current)) = (&counter.last_git_hash, &current_hash) {
        if last != current {
            counter.count = 0;
            counter.last_git_hash = current_hash.clone();
            write_counter(&cpath, &counter);
        }
    }

    let mut issues: Vec<String> = vec![];

    if let Some(status) = git(&["status", "--porcelain"], &project_dir) {
        let tracked_changes = status.lines()
            .filter(|l| !l.starts_with("??"))
            .count();
        if tracked_changes > 0 {
            issues.push("Uncommitted changes exist".into());
        }
    }

    match git(&["rev-list", "--count", "@{u}..HEAD"], &project_dir) {
        Some(s) => {
            let n: u64 = s.parse().unwrap_or(0);
            if n > 0 { issues.push(format!("{} commit(s) not pushed", n)); }
        }
        None => issues.push("Unable to verify push status - may have unpushed commits".into()),
    }

    if let Some(s) = git(&["rev-list", "--count", "HEAD..@{u}"], &project_dir) {
        let n: u64 = s.parse().unwrap_or(0);
        if n > 0 { issues.push(format!("{} upstream change(s) not pulled", n)); }
    }

    if issues.is_empty() {
        if counter.count > 0 {
            counter.count = 0;
            write_counter(&cpath, &counter);
        }
        println!("{}", json!({ "decision": "approve" }));
        return;
    }

    counter.count += 1;
    counter.last_git_hash = current_hash;
    write_counter(&cpath, &counter);

    let reason = format!("{}, must push to remote", issues.join(", "));
    if counter.count == 1 {
        println!("{}", serde_json::to_string_pretty(&json!({ "decision": "block", "reason": format!("Git: {}", reason) })).unwrap_or_default());
        std::process::exit(2);
    } else {
        println!("{}", json!({ "decision": "approve", "reason": format!("⚠️ Git warning (attempt #{}): {} - Please commit and push your changes.", counter.count, reason) }));
    }
}
