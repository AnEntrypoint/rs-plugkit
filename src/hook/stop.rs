use serde_json::json;
use std::{env, fs, path::Path};
use super::no_window_cmd;

fn write_needs_gm(project_dir: &str) {
    let gm_dir = Path::new(project_dir).join(".gm");
    let _ = fs::create_dir_all(&gm_dir);
    let _ = fs::write(gm_dir.join("needs-gm"), "1");
}

pub fn run_stop() {
    let session_id = env::var("CLAUDE_SESSION_ID").unwrap_or_default();
    let open = if !session_id.is_empty() {
        super::agent_browser::get_open_session_ids(&session_id)
    } else {
        vec![]
    };
    let project_dir = env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| env::current_dir().unwrap_or_default().to_string_lossy().to_string());

    if !open.is_empty() {
        let ids = open.join(", ");
        write_needs_gm(&project_dir);
        let out = json!({
            "decision": "block",
            "reason": format!("Open browser session(s): [{}]. Close them before stopping:\n  exec:browser\n  await page.close()\n\nOr use `exec:close` to clean up background tasks.\n\nHousekeeping policy: always close browser sessions and background tasks before ending a conversation.\n\nNEXT ACTION: invoke Skill(gm) first.", ids)
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        std::process::exit(2);
    }

    let prd = std::path::Path::new(&project_dir).join(".gm").join("prd.yml");

    if prd.exists() {
        let content = fs::read_to_string(&prd).unwrap_or_default();
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            write_needs_gm(&project_dir);
            let out = json!({
                "decision": "block",
                "reason": format!("Work items remain in {}. Remove completed items as they finish. Delete the file when all items are done.\n\n{}\n\nNEXT ACTION: invoke Skill(gm) first.", prd.display(), trimmed)
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            std::process::exit(2);
        }
    }

    println!("{}", json!({ "decision": "approve" }));
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
    no_window_cmd("git")
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
        let ci = watch_gh_runs_for_head(&project_dir);
        match ci {
            CiOutcome::None => println!("{}", json!({ "decision": "approve" })),
            CiOutcome::AllGreen(report) => println!("{}", json!({ "decision": "approve", "reason": format!("CI: {}", report) })),
            CiOutcome::Failures(report) => {
                write_needs_gm(&project_dir);
                println!("{}", serde_json::to_string_pretty(&json!({ "decision": "block", "reason": format!("CI failure(s) on this push:\n{}\n\nInvestigate, fix, push again. Use `gh run view <id> --log-failed` for details.\n\nNEXT ACTION: invoke Skill(gm) first.", report) })).unwrap_or_default());
                std::process::exit(2);
            }
        }
        return;
    }

    counter.count += 1;
    counter.last_git_hash = current_hash;
    write_counter(&cpath, &counter);

    let reason = format!("{}, must push to remote", issues.join(", "));
    let auth_hint = if counter.count >= 2 {
        let gh_ok = git(&["config", "--get", "credential.helper"], &project_dir)
            .map(|s| s.trim().contains("gh"))
            .unwrap_or(false);
        if gh_ok {
            "\n\nIf git push keeps prompting for password, your gh token may be expired. Check with: gh auth status. Re-auth: gh auth login -h github.com"
        } else { "" }
    } else { "" };
    if counter.count == 1 {
        write_needs_gm(&project_dir);
        println!("{}", serde_json::to_string_pretty(&json!({ "decision": "block", "reason": format!("Git: {}{}\n\nNEXT ACTION: invoke Skill(gm) first.", reason, auth_hint) })).unwrap_or_default());
        std::process::exit(2);
    } else {
        println!("{}", json!({ "decision": "approve", "reason": format!("⚠️ Git warning (attempt #{}): {}{} - Please commit and push your changes.", counter.count, reason, auth_hint) }));
    }
}

enum CiOutcome { None, AllGreen(String), Failures(String) }

#[derive(serde::Deserialize)]
struct GhRun {
    #[serde(rename = "databaseId")]
    database_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(rename = "headSha")]
    head_sha: String,
}

fn watch_gh_runs_for_head(project_dir: &str) -> CiOutcome {
    if which::which("gh").is_err() { return CiOutcome::None; }
    let head = match git(&["rev-parse", "HEAD"], project_dir) { Some(h) => h, None => return CiOutcome::None };
    let deadline_secs: u64 = env::var("GM_CI_WATCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(180);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
    let initial = match list_runs_for_head(project_dir, &head) { Some(v) => v, None => return CiOutcome::None };
    let pending: Vec<GhRun> = initial.into_iter().filter(|r| r.conclusion.as_deref().unwrap_or("").is_empty()).collect();
    if pending.is_empty() { return CiOutcome::None; }
    let pending_ids: Vec<u64> = pending.iter().map(|r| r.database_id).collect();
    eprintln!("[ci-watch] {} run(s) in flight on {}; watching up to {}s.", pending_ids.len(), &head[..7.min(head.len())], deadline_secs);
    let mut last_runs: Vec<GhRun> = pending;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(8));
        let now = match list_runs_for_head(project_dir, &head) { Some(v) => v, None => continue };
        last_runs = now.into_iter().filter(|r| pending_ids.contains(&r.database_id)).collect();
        if last_runs.iter().all(|r| !r.conclusion.as_deref().unwrap_or("").is_empty()) { break; }
    }
    let lines: Vec<String> = last_runs.iter().map(|r| {
        let c = r.conclusion.as_deref().unwrap_or("");
        let state = if c.is_empty() { format!("still {}", r.status) } else { c.to_string() };
        format!("  {} [{}] (id {})", r.name, state, r.database_id)
    }).collect();
    let report = lines.join("\n");
    let any_failed = last_runs.iter().any(|r| matches!(r.conclusion.as_deref(), Some("failure" | "cancelled" | "timed_out" | "action_required")));
    let any_unfinished = last_runs.iter().any(|r| r.conclusion.as_deref().unwrap_or("").is_empty());
    if any_failed { CiOutcome::Failures(report) }
    else if any_unfinished { CiOutcome::AllGreen(format!("watch deadline reached, still in progress:\n{}", report)) }
    else { CiOutcome::AllGreen(format!("all green on this push:\n{}", report)) }
}

fn list_runs_for_head(project_dir: &str, head: &str) -> Option<Vec<GhRun>> {
    let out = no_window_cmd("gh").args(["run", "list", "--limit", "20", "--json", "databaseId,name,status,conclusion,headSha"])
        .current_dir(project_dir).output().ok()?;
    if !out.status.success() { return None; }
    let runs: Vec<GhRun> = serde_json::from_slice(&out.stdout).ok()?;
    Some(runs.into_iter().filter(|r| r.head_sha == head).collect())
}
