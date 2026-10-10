use super::*;

pub(super) const GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS: &str = "git_async";
pub(super) const GIT_PENDING_RESULT_OUTBOX_NS: &str = "outbox";
pub(super) const GIT_COMMIT_DEDUP_NS: &str = "git_commit_dedup";
pub(super) const GIT_COMMIT_DEDUP_TTL_MS: u64 = 180_000;

// Spawning git with a command line past ~32 KiB fails on Windows with os error 206, which the
// completeness gate reports as git_status_incomplete and refuses the mutation. One exclude
// pathspec per dirty path reaches that on a repo with a few hundred dirty paths, so the
// exclusions get a budget and the rest are simply not withheld: a broader scope is still complete
// worktree evidence.
pub(super) const GIT_PATHSPEC_SCOPE_EXCLUDE_BUDGET_CHARS: usize = 8000;

pub(super) fn git_commit_dedup_key(
    cwd: Option<&str>,
    head_before: &str,
    message: &str,
    paths: &[String],
    add_all: bool,
    amend: bool,
) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        cwd.unwrap_or(""),
        head_before,
        message,
        paths.join(","),
        add_all,
        amend
    )
}

pub(super) fn git_commit_dedup_lookup(key: &str, cwd: Option<&str>) -> Option<Value> {
    let raw = super::host_abi::host_kv_read(GIT_COMMIT_DEDUP_NS, key)?;
    let record: Value = serde_json::from_str(&raw).ok()?;
    let ts = record.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
    let now = unsafe { host_now_ms() };
    if now.saturating_sub(ts) > GIT_COMMIT_DEDUP_TTL_MS {
        return None;
    }
    let sha_full = record.get("sha_full").and_then(|v| v.as_str())?.to_string();
    let still_reachable = git_call_argv(&["cat-file", "-e", &sha_full], cwd)
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(1)
        == 0;
    if !still_reachable {
        return None;
    }
    Some(record)
}

pub(super) fn git_commit_dedup_record(key: &str, sha_full: &str, sha: &str, summary: &str) {
    let now = unsafe { host_now_ms() };
    let record =
        json!({ "ts": now, "sha_full": sha_full, "sha": sha, "summary": summary }).to_string();
    git_async_kv_put(GIT_COMMIT_DEDUP_NS, key, &record);
}

pub(super) struct GitPendingTokenReplayPlan {
    id: String,
    verb: String,
    body: Value,
    calls: usize,
    results: serde_json::Map<String, Value>,
    persisted: bool,
}

pub(super) fn git_async_plan_new(verb: &str, body: &Value) -> GitPendingTokenReplayPlan {
    thread_local! {
        static PLAN_SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let seq = PLAN_SEQ.with(|c| {
        let n = c.get();
        c.set(n + 1);
        n
    });
    let now = unsafe { host_now_ms() };
    GitPendingTokenReplayPlan {
        id: format!("gitplan-{}-{}", now, seq),
        verb: verb.to_string(),
        body: body.clone(),
        calls: 0,
        results: serde_json::Map::new(),
        persisted: false,
    }
}

pub(super) fn git_async_plan_load(id: &str) -> Option<GitPendingTokenReplayPlan> {
    let raw = super::host_abi::host_kv_read(GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS, id)?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(GitPendingTokenReplayPlan {
        id: id.to_string(),
        verb: v.get("verb")?.as_str()?.to_string(),
        body: v.get("body").cloned().unwrap_or(json!({})),
        calls: 0,
        results: v
            .get("results")
            .and_then(|x| x.as_object())
            .cloned()
            .unwrap_or_default(),
        persisted: true,
    })
}

pub(super) fn git_async_kv_put(ns: &str, key: &str, val: &str) {
    unsafe {
        host_kv_put(
            ns.as_ptr(),
            ns.len() as u32,
            key.as_ptr(),
            key.len() as u32,
            val.as_ptr(),
            val.len() as u32,
        );
    }
}

pub(super) fn git_async_kv_delete(ns: &str, key: &str) {
    unsafe {
        host_kv_delete(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32);
    }
}

pub(super) fn git_async_plan_persist(plan: &GitPendingTokenReplayPlan) {
    let s = json!({
        "verb": plan.verb,
        "body": plan.body,
        "results": plan.results,
    })
    .to_string();
    git_async_kv_put(GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS, &plan.id, &s);
}

pub(super) fn git_async_plan_forget(plan: &GitPendingTokenReplayPlan) {
    if plan.persisted {
        git_async_kv_delete(GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS, &plan.id);
    }
}

pub(super) fn git_async_pending_envelope(verb: &str, token: &str, plan: &str) -> u64 {
    err_json(
        verb,
        json!({
            "error": "async git host parked this op; dispatch git_poll {token} until a non-pending envelope comes back -- that envelope IS this verb's terminal result",
            "pending": true,
            "token": token,
            "plan": plan,
            "next_dispatch_hint": "git_poll",
        }),
    )
}

pub(super) fn git_step_replayed_by_call_order(
    plan: &mut GitPendingTokenReplayPlan,
    argv: &[&str],
    cwd: Option<&str>,
) -> Result<Value, u64> {
    let idx = plan.calls;
    plan.calls += 1;
    if let Some(v) = plan.results.get(&idx.to_string()) {
        return Ok(v.clone());
    }
    let r = super::host_abi::git_call_argv_async(argv, cwd);
    if let Some(token) = super::host_abi::git_pending_token(&r) {
        git_async_plan_persist(plan);
        git_async_kv_put(
            GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS,
            &format!("tok:{}", token),
            &plan.id,
        );
        return Err(git_async_pending_envelope(&plan.verb, &token, &plan.id));
    }
    plan.results.insert(idx.to_string(), r.clone());
    Ok(r)
}

pub(super) fn git_async_entry(
    verb: &str,
    body: &Value,
    run: impl FnOnce(&Value, &mut GitPendingTokenReplayPlan) -> Result<u64, u64>,
) -> u64 {
    let mut plan = body
        .get("_plan")
        .and_then(|x| x.as_str())
        .and_then(git_async_plan_load)
        .unwrap_or_else(|| git_async_plan_new(verb, body));
    match run(body, &mut plan) {
        Ok(packed) => {
            git_async_plan_forget(&plan);
            packed
        }
        Err(parked) => parked,
    }
}

pub(super) fn git_async_reenter(verb: &str, body: &Value) -> u64 {
    match verb {
        "git_status" => git_status(body),
        "git_add" => git_add(body),
        "git_commit" => git_commit(body),
        "git_amend" => git_amend(body),
        "git_log" => git_log(body),
        "git_diff" => git_diff(body),
        "git_remote" => git_remote(body),
        _ => err(
            "git_poll",
            "parked plan names a verb with no async-resume support",
        ),
    }
}

pub(super) fn git_poll(body: &Value) -> u64 {
    let token = body.get("token").and_then(|v| v.as_str()).unwrap_or("");
    if token.is_empty() {
        return err("git_poll", "token required");
    }
    let raw = super::host_abi::host_kv_read(GIT_PENDING_RESULT_OUTBOX_NS, token);
    let Some(raw) = raw else {
        return err_json(
            "git_poll",
            json!({
                "error": "no result parked under this token yet -- the async host is still driving the op; poll again",
                "pending": true,
                "token": token,
                "next_dispatch_hint": "git_poll",
            }),
        );
    };
    git_async_kv_delete(GIT_PENDING_RESULT_OUTBOX_NS, token);
    let result: Value = serde_json::from_str(&raw)
        .unwrap_or(json!({ "stdout": raw, "stderr": "", "exit_code": 0 }));
    let mapping_key = format!("tok:{}", token);
    let Some(plan_id) =
        super::host_abi::host_kv_read(GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS, &mapping_key)
    else {
        return ok("git_poll", json!({ "token": token, "result": result }));
    };
    git_async_kv_delete(GIT_ASYNC_PENDING_TOKEN_REPLAY_PLAN_NS, &mapping_key);
    let Some(mut plan) = git_async_plan_load(&plan_id) else {
        return err(
            "git_poll",
            "plan state missing for parked token -- cannot resume the parked verb",
        );
    };
    let parked_idx = plan.results.len();
    plan.results.insert(parked_idx.to_string(), result);
    git_async_plan_persist(&plan);
    let mut resumed = plan.body.clone();
    if let Some(m) = resumed.as_object_mut() {
        m.insert("_plan".to_string(), json!(plan.id));
    }
    git_async_reenter(&plan.verb, &resumed)
}

pub(super) fn run_git_checked(
    argv: &[&str],
    cwd: Option<&str>,
    verb: &str,
    fallback: &str,
) -> Result<Value, u64> {
    let r = git_call_argv(argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        return Err(err(
            verb,
            r.get("stderr").and_then(|x| x.as_str()).unwrap_or(fallback),
        ));
    }
    Ok(r)
}

pub(super) const GIT_STATUS_SUMMARY_DEFAULT_PATHS: usize = 20;

pub(super) fn git_head_ref_line(
    plan: &mut GitPendingTokenReplayPlan,
    cwd: Option<&str>,
) -> Result<String, u64> {
    let r = git_step_replayed_by_call_order(plan, &["log", "-1", "--pretty=format:%H %D"], cwd)?;
    Ok(r.get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

pub(super) fn split_head_ref_line(line: &str) -> (String, String) {
    let sha = line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let branch = line
        .split("HEAD -> ")
        .nth(1)
        .and_then(|rest| rest.split([',', ')'].as_ref()).next())
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    (sha, branch)
}
pub(super) const GIT_STATUS_SKIPPED_PATHS_MAX: usize = 25;
pub(super) const GIT_STATUS_SAMPLE_PATHS: usize = 10;
pub(super) const GIT_STATUS_DIRECTORY_BUCKETS: usize = 10;

fn git_eol_entries(listing: &str) -> Vec<Value> {
    listing
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut tokens = meta.split_whitespace();
            let index = tokens.next()?.strip_prefix("i/")?.to_string();
            let worktree = tokens.next()?.strip_prefix("w/")?.to_string();
            let attr = tokens.collect::<Vec<&str>>().join(" ");
            let attr = attr.strip_prefix("attr/").unwrap_or(&attr).to_string();
            Some(json!({ "path": path, "index": index, "worktree": worktree, "attr": attr }))
        })
        .collect()
}

fn git_status_sample(paths: &[String], take: usize) -> Value {
    Value::Array(paths.iter().take(take).map(|path| json!(path)).collect())
}

fn git_status_directory_counts(paths: &[String]) -> Value {
    let mut by_directory: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for path in paths {
        let directory = path.rsplit_once('/').map_or(".", |(directory, _)| directory);
        *by_directory.entry(directory).or_insert(0) += 1;
    }
    let directories = by_directory.len();
    let mut ranked: Vec<(&str, usize)> = by_directory.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let elsewhere: usize = ranked
        .iter()
        .skip(GIT_STATUS_DIRECTORY_BUCKETS)
        .map(|&(_, count)| count)
        .sum();
    let top: serde_json::Map<String, Value> = ranked
        .into_iter()
        .take(GIT_STATUS_DIRECTORY_BUCKETS)
        .map(|(directory, count)| (directory.to_string(), json!(count)))
        .collect();
    json!({ "directories": directories, "top": top, "elsewhere": elsewhere })
}

const GIT_STATUS_SPILL_DIR: &str = ".gm/exec-spool/out";
const GIT_STATUS_SPILL_PREFIX: &str = "git_status-";
pub(super) const GIT_STATUS_SPILL_KEEP: usize = 20;

fn git_status_spill_retention_note() -> String {
    format!(
        "newest {GIT_STATUS_SPILL_KEEP} git_status spill files are kept in {GIT_STATUS_SPILL_DIR}; the git_status call that writes a spill deletes older ones"
    )
}

fn git_status_spill_listing(spill_name: &str, lines: &[String]) -> Option<String> {
    let relative = format!("{GIT_STATUS_SPILL_DIR}/{spill_name}");
    let mut body = lines.join("\n");
    body.push('\n');
    if crate::pkfs::write(&relative, &body) {
        git_status_prune_spills();
        Some(crate::pkfs::anchor(&relative))
    } else {
        None
    }
}

fn git_status_prune_spills() {
    let Some(Value::Array(entries)) = crate::pkfs::readdir(GIT_STATUS_SPILL_DIR) else {
        return;
    };
    let mut spills: Vec<(f64, String)> = entries
        .iter()
        .filter_map(|entry| {
            let name = entry
                .as_str()
                .or_else(|| entry.get("name").and_then(Value::as_str))?;
            let is_file = entry
                .get("is_file")
                .or_else(|| entry.get("isFile"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !is_file || !name.starts_with(GIT_STATUS_SPILL_PREFIX) || !name.ends_with(".txt") {
                return None;
            }
            let mtime_ms = crate::pkfs::stat(&format!("{GIT_STATUS_SPILL_DIR}/{name}"))
                .and_then(|s| s.get("mtime_ms").or_else(|| s.get("mtimeMs")).and_then(Value::as_f64))
                .unwrap_or(0.0);
            Some((mtime_ms, name.to_string()))
        })
        .collect();
    if spills.len() <= GIT_STATUS_SPILL_KEEP {
        return;
    }
    spills.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_, name) in spills.into_iter().skip(GIT_STATUS_SPILL_KEEP) {
        git_status_remove_spill(&crate::pkfs::anchor(&format!("{GIT_STATUS_SPILL_DIR}/{name}")));
    }
}

#[cfg(target_arch = "wasm32")]
fn git_status_remove_spill(path: &str) -> bool {
    super::host_abi::host_remove_file_never_directory(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn git_status_remove_spill(_path: &str) -> bool {
    false
}

pub(super) fn git_status(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_status",
        body,
        &["path", "paths", "files", "summary", "limit", "eol"],
    ) {
        return refusal;
    }
    git_async_entry("git_status", body, |body, plan| {
        let cwd = body_cwd(body);
        let paths = body_pathspecs(body);
        let summary = body
            .get("summary")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let limit = body
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n.max(1) as usize);
        let mut argv: Vec<&str> = vec!["status", "--porcelain"];
        if !paths.is_empty() {
            argv.push("--");
            for p in &paths {
                argv.push(p.as_str());
            }
        }
        let r = git_step_replayed_by_call_order(plan, &argv, cwd)?;
        let st = super::host_abi::porcelain_from(&r);
        let porcelain = st.porcelain.clone();
        let mut modified: Vec<String> = vec![];
        let mut untracked: Vec<String> = vec![];
        let mut deleted: Vec<String> = vec![];
        let mut staged: Vec<String> = vec![];
        for line in porcelain.lines() {
            if line.len() < 3 {
                continue;
            }
            let xy = &line[..2];
            let path = line[3..].trim().to_string();
            let x = xy.chars().nth(0).unwrap_or(' ');
            let y = xy.chars().nth(1).unwrap_or(' ');
            if xy == "??" {
                untracked.push(path);
                continue;
            }
            if x != ' ' && x != '?' {
                staged.push(path.clone());
            }
            if y == 'M' || x == 'M' {
                modified.push(path.clone());
            }
            if y == 'D' || x == 'D' {
                deleted.push(path.clone());
            }
        }
        let dirty = !porcelain.trim().is_empty();
        let head_line = git_head_ref_line(plan, cwd)?;
        let (head_sha, head_branch) = split_head_ref_line(&head_line);
        let eol_run = if body.get("eol").and_then(|v| v.as_bool()).unwrap_or(false) {
            let mut eol_argv: Vec<&str> = vec!["ls-files", "--eol"];
            if !paths.is_empty() {
                eol_argv.push("--");
                for p in &paths {
                    eol_argv.push(p.as_str());
                }
            }
            Some(git_step_replayed_by_call_order(plan, &eol_argv, cwd)?)
        } else {
            None
        };
        let mut lists = if summary {
            let first_n = limit.unwrap_or(GIT_STATUS_SUMMARY_DEFAULT_PATHS);
            let entries: Vec<&str> = porcelain.lines().filter(|l| l.len() >= 3).collect();
            let first_paths: Vec<String> = entries
                .iter()
                .take(first_n)
                .map(|l| format!("{} {}", &l[..2], l[3..].trim()))
                .collect();
            json!({
                "dirty": dirty,
                "head": head_sha.clone(), "head_sha": head_sha.clone(), "branch": head_branch.clone(),
                "summary": true,
                "counts": {
                    "changed_paths": entries.len(),
                    "modified": modified.len(),
                    "staged": staged.len(),
                    "deleted": deleted.len(),
                    "untracked": untracked.len(),
                },
                "first_paths": first_paths,
                "first_paths_note": "each entry is the two-column porcelain status, a space, then the path",
                "truncated": entries.len() > first_n,
                "scoped_to": paths,
            })
        } else {
            let sample_n = limit.unwrap_or(GIT_STATUS_SAMPLE_PATHS);
            let overflow = [&modified, &untracked, &deleted, &staged]
                .iter()
                .any(|list| list.len() > sample_n);
            let mut l = json!({
                "dirty": dirty,
                "head": head_sha.clone(), "head_sha": head_sha.clone(), "branch": head_branch.clone(),
                "counts": {
                    "changed_paths": porcelain.lines().filter(|entry| entry.len() >= 3).count(),
                    "modified": modified.len(),
                    "untracked": untracked.len(),
                    "deleted": deleted.len(),
                    "staged": staged.len(),
                },
                "by_directory": {
                    "modified": git_status_directory_counts(&modified),
                    "deleted": git_status_directory_counts(&deleted),
                },
                "modified": git_status_sample(&modified, sample_n),
                "untracked": git_status_sample(&untracked, sample_n),
                "deleted": git_status_sample(&deleted, sample_n),
                "staged": git_status_sample(&staged, sample_n),
                "truncated": overflow,
            });
            if overflow {
                let lines: Vec<String> = porcelain
                    .lines()
                    .filter(|entry| entry.len() >= 3)
                    .map(str::to_string)
                    .collect();
                let spill_name = format!(
                    "git_status-{}.txt",
                    super::search::dispatch_task_id()
                        .unwrap_or_else(|| unsafe { host_now_ms() }.to_string())
                );
                match git_status_spill_listing(&spill_name, &lines) {
                    Some(file) => {
                        l["spill_file"] = json!(file);
                        l["spill_retention"] = json!(git_status_spill_retention_note());
                    }
                    None => l["spill_write_failed"] = json!(true),
                }
            }
            if !paths.is_empty() {
                l["scoped_to"] = json!(paths);
            }
            l
        };
        if let Some(run) = &eol_run {
            let code = run.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0);
            if code != 0 {
                return Err(err(
                    "git_status",
                    run.get("stderr").and_then(|v| v.as_str()).unwrap_or("git ls-files --eol failed"),
                ));
            }
            let listing = run.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let mut entries = git_eol_entries(listing);
            let mismatched = entries.iter().filter(|e| e["index"] != e["worktree"]).count();
            let eol_count = entries.len();
            let eol_cap = limit.unwrap_or(GIT_STATUS_SAMPLE_PATHS);
            let eol_truncated = eol_count > eol_cap;
            let eol_spill_file = eol_truncated.then(|| {
                let lines: Vec<String> = listing.lines().map(str::to_string).collect();
                let spill_name = format!(
                    "git_status-eol-{}.txt",
                    super::search::dispatch_task_id()
                        .unwrap_or_else(|| unsafe { host_now_ms() }.to_string())
                );
                git_status_spill_listing(&spill_name, &lines)
            });
            entries.truncate(eol_cap);
            if let Some(map) = lists.as_object_mut() {
                map.insert("eol_count".to_string(), json!(eol_count));
                map.insert("eol_mismatch_count".to_string(), json!(mismatched));
                map.insert("eol".to_string(), Value::Array(entries));
                map.insert("eol_truncated".to_string(), json!(eol_truncated));
                if eol_truncated {
                    match eol_spill_file.flatten() {
                        Some(file) => {
                            map.insert("eol_spill_file".to_string(), json!(file));
                            map.insert(
                                "spill_retention".to_string(),
                                json!(git_status_spill_retention_note()),
                            );
                        }
                        None => {
                            map.insert("eol_spill_write_failed".to_string(), json!(true));
                        }
                    }
                }
            }
        }
        // A path git cannot open (Windows MAX_PATH, or permissions) must degrade
        // this listing, never abort it: report what was read and name the rest.
        if st.partial {
            let skipped: Vec<String> = st
                .skipped_paths
                .iter()
                .take(GIT_STATUS_SKIPPED_PATHS_MAX)
                .cloned()
                .collect();
            let note = if st.failed {
                format!("git status exited {} -- the paths listed are only what it could read. This is NOT a dirty-tree signal and git_finalize is not blocked by it.", st.exit_code)
            } else {
                "git skipped path(s) it could not open (Windows MAX_PATH or permission); entries under them are absent from this listing, not clean".to_string()
            };
            if let Some(map) = lists.as_object_mut() {
                map.insert("partial".to_string(), json!(true));
                map.insert("skipped_count".to_string(), json!(st.skipped_paths.len()));
                map.insert("skipped_paths".to_string(), json!(skipped));
                map.insert("status_note".to_string(), Value::String(note));
                if st.failed {
                    map.insert("git_error".to_string(), json!(st.stderr.trim()));
                    map.insert("exit_code".to_string(), json!(st.exit_code));
                }
            }
        }
        Ok(ok("git_status", lists))
    })
}

pub(super) fn branch_status(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let branch = exec_git_in(cwd, "rev-parse --abbrev-ref HEAD")
        .trim()
        .to_string();
    if branch.is_empty() {
        return err("branch_status", "unable to determine branch");
    }
    let remote = exec_git_in(cwd, &format!("config --get branch.{}.remote", branch))
        .trim()
        .to_string();
    let remote = if remote.is_empty() {
        "origin".to_string()
    } else {
        remote
    };
    if !body
        .get("no_fetch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let _ = exec_git_in(cwd, &format!("fetch {} {}", remote, branch));
    }
    let counts = exec_git_in(
        cwd,
        &format!("rev-list --left-right --count {}/{}...HEAD", remote, branch),
    );
    let counts = counts.trim();
    let mut behind: u64 = 0;
    let mut ahead: u64 = 0;
    let parts: Vec<&str> = counts.split_whitespace().collect();
    if parts.len() == 2 {
        behind = parts[0].parse().unwrap_or(0);
        ahead = parts[1].parse().unwrap_or(0);
    }
    ok(
        "branch_status",
        json!({
            "branch": branch,
            "ahead": ahead,
            "behind": behind,
            "remote": remote,
        }),
    )
}

pub(super) fn resolve_ref(cwd: Option<&str>, refspec: &str) -> Option<String> {
    let response = git_call_argv(&["rev-parse", refspec], cwd);
    let sha = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

pub(super) struct SshHttpsFallback {
    config_args: Vec<String>,
    ssh_url: String,
    https_url: String,
}

impl SshHttpsFallback {
    fn call(&self, argv: &[&str], cwd: Option<&str>) -> Value {
        let mut full: Vec<&str> = self.config_args.iter().map(String::as_str).collect();
        full.extend_from_slice(argv);
        git_call_argv(&full, cwd)
    }
}

pub(super) fn ssh_origin_https_equivalent(url: &str) -> Option<(String, String, String)> {
    if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        if host.is_empty() {
            return None;
        }
        let https_prefix = format!("https://{}/", host);
        return Some((
            format!("ssh://{}/", authority),
            https_prefix.clone(),
            format!("{}{}", https_prefix, path),
        ));
    }
    if url.contains("://") {
        return None;
    }
    let (user_host, path) = url.split_once(':')?;
    let host = user_host.split_once('@')?.1;
    if host.is_empty() || user_host.contains('/') {
        return None;
    }
    let https_prefix = format!("https://{}/", host);
    Some((
        format!("{}:", user_host),
        https_prefix.clone(),
        format!("{}{}", https_prefix, path.trim_start_matches('/')),
    ))
}

pub(super) fn with_index_lock_report<'a>(payload: Value, steps: &[&'a Value]) -> Value {
    let mut reports: Vec<&'a Value> = Vec::new();
    for step in steps {
        if let Some(report) = (*step).get("index_lock_contention") {
            reports.push(report);
        }
    }
    if reports.is_empty() {
        return payload;
    }
    let mut waited_ms = 0u64;
    let mut attempts = 0u32;
    let mut lock = String::new();
    let mut resolved = true;
    for report in &reports {
        waited_ms += report.get("waited_ms").and_then(|x| x.as_u64()).unwrap_or(0);
        attempts += report.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        if lock.is_empty() {
            lock = report
                .get("lock")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
        }
        if !report.get("resolved").and_then(|x| x.as_bool()).unwrap_or(false) {
            resolved = false;
        }
    }
    let mut payload = payload;
    let Some(fields) = payload.as_object_mut() else {
        return payload;
    };
    fields.insert("index_lock".to_string(), json!(lock));
    fields.insert("index_lock_attempts".to_string(), json!(attempts));
    fields.insert("index_lock_wait_ms".to_string(), json!(waited_ms));
    fields.insert("index_lock_resolved_by_retry".to_string(), json!(resolved));
    fields.insert(
        "index_lock_note".to_string(),
        json!(format!(
            "{} and then {}",
            resolved_note(&lock, attempts, waited_ms),
            if resolved {
                "acquired it, so the verb waited instead of failing"
            } else {
                "gave up because the retry budget was spent"
            }
        )),
    );
    payload
}

pub(super) fn push_output_is_ssh_auth_failure(output: &str) -> bool {
    output.contains("Permission denied")
        || output.contains("publickey")
        || output.contains("Host key verification failed")
        || output.contains("Could not read from remote repository")
}

pub(super) fn push_output_is_github_auth_failure(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("could not read username")
        || output.contains("authentication failed")
        || output.contains("http basic: access denied")
        || output.contains("invalid username or token")
        || output.contains("terminal prompts disabled")
        || output.contains("password authentication is not supported")
}

pub(super) const GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS: u32 = 4;
pub(super) const GIT_PUSH_TRANSIENT_REMOTE_BACKOFF_MS: u64 = 500;

pub(super) fn push_output_is_diverged_remote(low: &str) -> bool {
    low.contains("fetch first")
        || low.contains("non-fast-forward")
        || low.contains("non fast forward")
        || low.contains("remote contains work that you do")
}

pub(super) fn push_output_is_transient_remote_rejection(output: &str) -> bool {
    let low = output.to_ascii_lowercase();
    if push_output_is_diverged_remote(&low) || push_output_is_github_auth_failure(output) {
        return false;
    }
    low.contains("internal server error")
        || low.contains("service unavailable")
        || low.contains("bad gateway")
        || low.contains("gateway timeout")
        || low.contains("request id")
        || low.contains("remote end hung up")
        || low.contains("temporarily unavailable")
}

pub(super) fn push_output_remote_request_ids(output: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in output.lines() {
        let low = line.to_ascii_lowercase();
        let Some(at) = low.find("request id") else {
            continue;
        };
        let tail = line[at + "request id".len()..]
            .trim_start()
            .trim_start_matches(':')
            .trim_start();
        let id = tail
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(|c: char| c == ',' || c == ';' || c == ')');
        if !id.is_empty() && !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

pub(super) fn pack_transient_remote_push_failure(
    repo: &Option<String>,
    branch: &str,
    source_ref: &str,
    source_sha: &str,
    preserved_dirty_worktree: bool,
    attempts: u32,
    request_ids: &[String],
    output: &str,
) -> u64 {
    log_deviation_push("push-transient-remote-rejection", branch);
    let request_ids_display = if request_ids.is_empty() {
        "(none reported)".to_string()
    } else {
        request_ids.join(", ")
    };
    let completed = attempts.saturating_sub(1) as u64;
    let waited_ms = GIT_PUSH_TRANSIENT_REMOTE_BACKOFF_MS * completed * (completed + 1) / 2;
    pack(
        json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": branch,
            "source_ref": source_ref,
            "source_sha": source_sha,
            "preserved_dirty_worktree": preserved_dirty_worktree,
            "transient_remote_rejection": true,
            "push_attempts": attempts,
            "remote_request_ids": request_ids,
            "reason": format!(
                "push of explicit source ref '{}' to {} was rejected by the remote itself with a transient server-side error ({} attempts, {} ms of backoff) -- this is NOT a non-fast-forward: the local ref did not diverge, so git_pull reports 'Already up to date' and re-pulling cannot help. GitHub Request ID(s): {}. Last git output:\n{}",
                source_ref, branch, attempts, waited_ms, request_ids_display, output
            ),
            "next_dispatch": "instruction",
            "next_action_hint": "Re-dispatch the same git_push once the remote recovers; git_pull is unnecessary and will report Already up to date. A sustained failure is a GitHub-side outage, not a local divergence.",
        })
        .to_string(),
    )
}

pub(super) fn ssh_https_fallback_for_origin(repo: Option<&str>) -> Option<SshHttpsFallback> {
    let url = exec_git_in(repo, "remote get-url origin")
        .trim()
        .to_string();
    let (ssh_prefix, https_prefix, https_url) = ssh_origin_https_equivalent(&url)?;
    let fallback = SshHttpsFallback {
        config_args: vec![
            "-c".to_string(),
            format!("url.{}.insteadOf={}", https_prefix, ssh_prefix),
            "-c".to_string(),
            "credential.helper=!gh auth git-credential".to_string(),
        ],
        ssh_url: url,
        https_url,
    };
    let reachable = fallback
        .call(&["ls-remote", "origin"], repo)
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(-1)
        == 0;
    if reachable {
        Some(fallback)
    } else {
        None
    }
}

pub(super) fn git_fetch_and_resolve_remote(
    repo: Option<&str>,
    branch: &str,
    fallback: Option<&SshHttpsFallback>,
) -> Option<String> {
    match fallback {
        Some(f) => {
            let _ = f.call(&["fetch", "origin", branch], repo);
        }
        None => {
            let _ = git_call_argv(&["fetch", "origin", branch], repo);
        }
    }
    resolve_ref(repo, &format!("origin/{}", branch))
}

pub(super) fn verify_push_landed(
    cwd: Option<&str>,
    branch: &str,
    local_head: &str,
    remote_before: Option<&str>,
    fallback: Option<&SshHttpsFallback>,
) -> Result<(String, bool), String> {
    let _ = match fallback {
        Some(f) => f.call(&["fetch", "origin", branch], cwd),
        None => git_call_argv(&["fetch", "origin", branch], cwd),
    };
    let remote_after = resolve_ref(cwd, &format!("origin/{}", branch));
    let remote_after = match remote_after {
        Some(s) => s,
        None => {
            return Err(format!(
                "could not resolve origin/{} after push -- remote-tracking ref missing",
                branch
            ))
        }
    };
    if remote_after != local_head {
        return Err(format!(
            "push claimed success but origin/{} ({}) does not match local HEAD ({}) after fetch -- remote did not actually advance to this commit",
            branch, remote_after, local_head
        ));
    }
    let already_current = remote_before.map(|b| b == local_head).unwrap_or(false);
    Ok((remote_after, already_current))
}

pub(super) const PUSHED_COMMITS_LIST_LIMIT: usize = 50;

pub(super) fn pushed_commits_published(repo: Option<&str>, from: Option<&str>, to: &str) -> Value {
    let range = match from {
        Some(from) => format!("{}..{}", from, to),
        None => to.to_string(),
    };
    let count = git_call_argv(&["rev-list", "--count", range.as_str()], repo)
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    let limit = format!("--max-count={}", PUSHED_COMMITS_LIST_LIMIT);
    let log_out = git_call_argv(
        &[
            "log",
            limit.as_str(),
            "--pretty=format:%H%x1f%an%x1f%ae%x1f%s",
            range.as_str(),
        ],
        repo,
    )
    .get("stdout")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
    let mut commits: Vec<Value> = vec![];
    let mut authors: Vec<String> = vec![];
    for line in log_out.lines() {
        let fields: Vec<&str> = line.split('\u{1f}').collect();
        if fields.len() < 4 {
            continue;
        }
        let author = format!("{} <{}>", fields[1], fields[2]);
        if !authors.contains(&author) {
            authors.push(author);
        }
        commits.push(json!({
            "sha": fields[0],
            "author": fields[1],
            "author_email": fields[2],
            "subject": fields[3],
        }));
    }
    json!({
        "range": range,
        "from": from,
        "to": to,
        "count": count,
        "listed": commits.len(),
        "truncated": count > commits.len() as u64,
        "authors": authors,
        "commits": commits,
    })
}

fn bin_drift_check_js(base: &str, repo: &str) -> String {
    format!(
        r#"(() => {{
  const cp = require('child_process');
  const path = require('path');
  const repoDir = path.resolve({base}, {repo});
  const script = path.join(repoDir, 'scripts', 'check-bin-drift.mjs');
  const r = cp.spawnSync(process.execPath, [script], {{ cwd: repoDir, encoding: 'utf8', windowsHide: true, timeout: 120000 }});
  process.stdout.write(JSON.stringify({{ exit_code: r.status, output: String(r.stdout || '') + String(r.stderr || ''), error: r.error ? String(r.error.message || r.error) : null }}));
}})();"#,
        base = serde_json::to_string(base).unwrap_or_else(|_| "\"\"".to_string()),
        repo = serde_json::to_string(repo).unwrap_or_else(|_| "\"\"".to_string()),
    )
}

fn bin_drift_gate_input(path: &str) -> bool {
    path.starts_with("bin/")
        || path.starts_with("src/")
        || path == "package.json"
        || path == "scripts/"
        || path == "scripts/build.mjs"
        || path == "scripts/check-bin-drift.mjs"
}

fn bin_drift_tracks_gate(repo: Option<&str>, rev: &str) -> bool {
    let gate_blob = format!("{}:scripts/check-bin-drift.mjs", rev);
    git_call_argv(&["cat-file", "-e", gate_blob.as_str()], repo)
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(1)
        == 0
}

fn bin_drift_run(repo: Option<&str>) -> Result<(), String> {
    let checkout = repo.unwrap_or("the checkout");
    let base = super::host_abi::host_cwd_string().unwrap_or_default();
    let code = bin_drift_check_js(&base, repo.unwrap_or(""));
    let opts = json!({ "timeoutMs": 150000 }).to_string();
    let packed = unsafe {
        super::host_abi::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let envelope: Value = super::host_abi::unpack_to_string(packed)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let inner: Value = envelope
        .get("stdout")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null);
    if inner.is_null() {
        return Err(format!(
            "the host returned no check result, so check-bin-drift did not run. Run: node scripts/check-bin-drift.mjs in {checkout}. If it reports STALE, run: npm run build, commit bin/gm-mcp-server.js with the src/ change, then retry."
        ));
    }
    if inner.get("exit_code").and_then(|v| v.as_i64()) == Some(0) {
        return Ok(());
    }
    let exit = inner
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map_or_else(|| "none".to_string(), |c| c.to_string());
    let output = inner
        .get("output")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let error = inner.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let spawn_error = if error.is_empty() {
        String::new()
    } else {
        format!(" spawn error: {error}")
    };
    let mut message = format!("check-bin-drift exit {exit}. {output}{spawn_error}");
    if !output.contains("npm run build") {
        message.push_str(&format!(
            "\nRun: node scripts/check-bin-drift.mjs in {checkout} to see the failure. If it reports STALE, run: npm run build, commit bin/gm-mcp-server.js with the src/ change, then retry."
        ));
    }
    Err(message)
}

fn bin_drift_refusal(
    repo: Option<&str>,
    source_sha: &str,
    gate_inputs_dirty: bool,
) -> Option<String> {
    if !bin_drift_tracks_gate(repo, source_sha) {
        return None;
    }
    let head_sha = resolve_ref(repo, "HEAD").unwrap_or_default();
    if gate_inputs_dirty || source_sha != head_sha.as_str() {
        return Some(format!(
            "bin drift gate cannot certify {source_sha}: check-bin-drift reads bin/, src/ and the build inputs from the checkout, so the push must be HEAD ({head_sha}) with no uncommitted changes under those paths. Commit or revert them, then push again. After the commit, run: node scripts/check-bin-drift.mjs; if it reports STALE, run: npm run build, then commit bin/gm-mcp-server.js with the src/ change."
        ));
    }
    bin_drift_run(repo)
        .err()
        .map(|detail| format!("bin drift gate refused the push: {detail}"))
}

fn bin_drift_pathspec_covers(pathspecs: &[String], path: &str) -> bool {
    pathspecs.iter().any(|spec| {
        let dir = spec.trim().trim_end_matches('/');
        dir == "." || path == dir || path.starts_with(format!("{dir}/").as_str())
    })
}

fn bin_drift_commit_refusal(repo: Option<&str>, paths: &[String], add_all: bool) -> Option<String> {
    let head_sha = resolve_ref(repo, "HEAD")?;
    if !bin_drift_tracks_gate(repo, &head_sha) {
        return None;
    }
    let index_only = !add_all && paths.is_empty();
    let porcelain = git_push_porcelain_in(repo);
    let mut touches_gate = false;
    let mut outside: Vec<String> = Vec::new();
    for line in porcelain.lines() {
        let path = line.get(3..).unwrap_or("").trim().trim_matches('"');
        if !bin_drift_gate_input(path) {
            continue;
        }
        let bytes = line.as_bytes();
        let staged = bytes.first().map_or(false, |&x| x != b' ' && x != b'?');
        let unstaged = bytes.get(1).map_or(false, |&y| y != b' ');
        let in_commit = if add_all {
            true
        } else if index_only {
            staged
        } else {
            bin_drift_pathspec_covers(paths, path)
        };
        if in_commit {
            touches_gate = true;
        }
        if !in_commit || (index_only && unstaged) {
            outside.push(path.to_string());
        }
    }
    if !touches_gate {
        return None;
    }
    if !outside.is_empty() {
        let listed: Vec<String> = outside.iter().take(12).cloned().collect();
        return Some(format!(
            "bin drift gate refused the commit: the commit changes build inputs, but these gate inputs are outside the commit or have unstaged changes: {}. Stage them in this commit or revert them, then commit again. bin/gm-mcp-server.js must match a fresh build of the committed src/: run npm run build and include bin/gm-mcp-server.js in the same commit.",
            listed.join(", ")
        ));
    }
    bin_drift_run(repo)
        .err()
        .map(|detail| format!("bin drift gate refused the commit: {detail}"))
}

pub(super) fn push_url_is_no_push_sentinel(url: &str) -> bool {
    let normalized = url.trim().to_ascii_lowercase();
    let normalized = normalized.trim_end_matches('/');
    normalized.is_empty()
        || matches!(
            normalized,
            "no-push" | "no_push" | "nopush" | "disable" | "disabled" | "none" | "null"
        )
}

pub(super) fn push_url_is_real_target(url: &str, repo: Option<&str>) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    if url.contains("://") {
        return true;
    }
    if let Some((_, host)) = url.split_once('@') {
        if !host.is_empty() && (host.contains(':') || host.contains('/')) {
            return true;
        }
    }
    if url.contains('/') || url.contains('\\') || crate::pkfs::is_absolute(url) {
        return true;
    }
    match repo {
        Some(repo) => {
            let joined = format!("{}/{}", repo.trim_end_matches(['/', '\\']), url);
            crate::pkfs::exists(&joined)
        }
        None => crate::pkfs::exists(url),
    }
}

pub(super) fn configured_push_url(repo: Option<&str>, remote: &str) -> Option<String> {
    let result = git_call_argv(&["remote", "get-url", "--push", remote], repo);
    if result.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(1) != 0 {
        return None;
    }
    let url = result
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

pub(super) fn push_disabled_refusal(
    repo: Option<&str>,
    branch: &str,
    source_ref: &str,
) -> Option<Value> {
    const REMOTE: &str = "origin";
    let push_url = configured_push_url(repo, REMOTE)?;
    if push_url_is_real_target(&push_url, repo) {
        return None;
    }
    let kind = if push_url_is_no_push_sentinel(&push_url) {
        "a configured no-push sentinel"
    } else {
        "not a real push target"
    };
    Some(json!({
        "ok": false,
        "verb": "git_push",
        "gate_denied": true,
        "remote_moved": false,
        "repo": repo,
        "branch": branch,
        "remote": REMOTE,
        "push_url": push_url,
        "source_ref": source_ref,
        "push_disabled_by_config": true,
        "reason_code": "push_disabled_by_config",
        "error_code": "push_disabled_by_config",
        "reason": format!(
            "remote '{}' of {} has pushurl '{}', which is {} -- no scheme, no user@host and no local path that exists. This repository is deliberately configured not to publish: it is not a broken remote, not a credentials failure and not a remote that moved, so pulling or retrying cannot fix it. Configure a real pushurl to publish -- git config --local remote.{}.pushurl <real push url> -- or drop the override with git config --local --unset remote.{}.pushurl so the fetch url is used.",
            REMOTE, repo.unwrap_or("cwd"), push_url, kind, REMOTE, REMOTE
        ),
        "next_dispatch": "instruction",
        "next_action_hint": format!(
            "git config --local remote.{}.pushurl <real push url> then git_push again; recover_remote_moved and pull_first are refused on purpose here.",
            REMOTE
        ),
    }))
}

// A push that names no commit publishes whatever the checkout happens to carry, so one lane's
// git_push ships every other lane's unpushed commit on the shared main. Naming the sha is what
// makes a publish deliberate: it states which commit the caller means, and the response then
// reports pushed_commits. allow_foreign_commits:true is the caller's way to say "publish the tip
// anyway", after reading the delta.
fn push_rev_names_commit(repo: Option<&str>, source_ref: &str, branch: &str) -> bool {
    let r = source_ref.trim();
    if r.is_empty() || r == "HEAD" || r == "@" || r == branch {
        return false;
    }
    if r.starts_with("origin/") || r.starts_with("refs/") {
        return false;
    }
    resolve_ref(repo, r).is_some()
}

fn push_unnamed_commits(
    repo: Option<&str>,
    remote_before: Option<&str>,
    local_source: &str,
    source_ref: &str,
    branch: &str,
) -> Vec<Value> {
    if push_rev_names_commit(repo, source_ref, branch) {
        return Vec::new();
    }
    match remote_before.map(str::trim).filter(|s| !s.is_empty()) {
        Some(remote) => commits_between(repo, remote, local_source)
            .as_array()
            .cloned()
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

pub(super) fn git_push(body: &Value) -> u64 {
    let repo = body_cwd(body).map(String::from);
    let explicit_branch = body
        .get("branch")
        .and_then(|v| v.as_str())
        .map(String::from);
    let explicit_source_ref = body
        .get("rev")
        .or_else(|| body.get("source_ref"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from);
    let current_branch = exec_git_in(repo.as_deref(), "rev-parse --abbrev-ref HEAD")
        .trim()
        .to_string();
    let branch = explicit_branch.clone().unwrap_or_else(|| {
        if current_branch == "HEAD" {
            "main".to_string()
        } else {
            current_branch.clone()
        }
    });
    if branch.is_empty() {
        return err("git_push", "unable to determine branch");
    }
    if explicit_branch.is_none() && current_branch != "main" && current_branch != "HEAD" {
        log_deviation_push("push-non-main-branch", &current_branch);
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": current_branch,
            "reason": format!(
                "current checkout is on branch '{}', not 'main' -- project rule is direct-push to main always, never a branch. This is likely a worktree checked out on a non-main ref. Pass explicit {{\"branch\":\"main\"}} to push to main from this worktree (git_push pushes HEAD to that ref), or {{\"branch\":\"{}\"}} if a non-main push is genuinely intended.",
                current_branch, current_branch
            ),
            "next_dispatch": "instruction",
            "next_dispatch_hint": "instruction",
            "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
        }).to_string());
    }
    let porcelain = git_push_porcelain_in(repo.as_deref());
    let source_ref = explicit_source_ref.as_deref().unwrap_or("HEAD");
    let local_source_before = match resolve_ref(repo.as_deref(), source_ref) {
        Some(sha) => sha,
        None => {
            return err(
                "git_push",
                &format!("could not resolve source ref '{}' before push", source_ref),
            )
        }
    };
    if !porcelain.trim().is_empty() && explicit_source_ref.is_none() {
        let scope = push_dirty_scope(repo.as_deref(), &branch, &local_source_before, &porcelain);
        if scope.blocks() {
            log_deviation_push("push-dirty", &branch);
            let refusal =
                push_dirty_refusal("git_push", repo.as_deref(), &branch, &porcelain, &scope);
            return pack(refusal.to_string());
        }
    }
    let gate_inputs_dirty = porcelain.lines().any(|line| {
        let path = line.get(3..).unwrap_or("").trim().trim_matches('"');
        bin_drift_gate_input(path)
    });
    if let Some(reason) =
        bin_drift_refusal(repo.as_deref(), &local_source_before, gate_inputs_dirty)
    {
        log_deviation_push("push-bin-drift", &branch);
        return pack(
            json!({
                "ok": false,
                "verb": "git_push",
                "gate_denied": true,
                "repo": repo,
                "branch": branch,
                "source_ref": source_ref,
                "source_sha": local_source_before,
                "reason": reason,
                "next_dispatch": "instruction",
                "next_dispatch_hint": "instruction",
                "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
            })
            .to_string(),
        );
    }
    let preserved_dirty_worktree = explicit_source_ref.is_some() && !porcelain.trim().is_empty();
    if preserved_dirty_worktree {
        emit_event(
            "git_push_explicit_ref",
            json!({
                "repo": repo,
                "source_ref": source_ref,
                "source_sha": local_source_before,
                "branch": branch,
            }),
        );
    }
    if let Some(refusal) = push_disabled_refusal(repo.as_deref(), &branch, source_ref) {
        log_deviation_push("push-disabled-by-config", &branch);
        return pack(refusal.to_string());
    }
    let mut ssh_fallback: Option<SshHttpsFallback> = None;
    let mut remote_before = git_fetch_and_resolve_remote(repo.as_deref(), &branch, None);
    let allow_foreign_commits = body
        .get("allow_foreign_commits")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let unnamed_commits = if allow_foreign_commits {
        Vec::new()
    } else {
        push_unnamed_commits(
            repo.as_deref(),
            remote_before.as_deref(),
            &local_source_before,
            source_ref,
            &branch,
        )
    };
    if !unnamed_commits.is_empty() {
        let shas = unnamed_commits
            .iter()
            .filter_map(|c| c.get("sha").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        let unnamed_count = unnamed_commits.len();
        let reason = format!(
            "this push names no commit: source_ref '{}' is symbolic, so publishing it would ship every unpushed commit on {} -- {} of them, which are not necessarily this lane's work ({}). Name the commit you mean to publish with {{\"rev\":\"<sha>\"}}, or pass allow_foreign_commits:true to publish the tip deliberately.",
            source_ref, branch, unnamed_count, shas
        );
        log_deviation_push("push-unnamed-commits", &branch);
        return pack(
            json!({
                "ok": false,
                "verb": "git_push",
                "gate_denied": true,
                "repo": repo,
                "branch": branch,
                "source_ref": source_ref,
                "source_sha": local_source_before,
                "remote_before": remote_before,
                "unnamed_commits": unnamed_commits,
                "unnamed_commit_count": unnamed_count,
                "reason": reason,
                "next_dispatch": "instruction",
                "next_dispatch_hint": "instruction",
                "next_action_hint": "Dispatch git_log to read the delta, then git_push {\"rev\":\"<sha>\"} naming the commit this lane means to publish; git_finalize names it for you.",
                "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
            })
            .to_string(),
        );
    }
    let (mut push_out, mut push_succeeded) =
        exec_git_push_in(repo.as_deref(), source_ref, &branch, None);
    if !push_succeeded && push_output_is_ssh_auth_failure(&push_out) {
        if let Some(fallback) = ssh_https_fallback_for_origin(repo.as_deref()) {
            remote_before = git_fetch_and_resolve_remote(repo.as_deref(), &branch, Some(&fallback));
            let (retry_out, retry_ok) =
                exec_git_push_in(repo.as_deref(), source_ref, &branch, Some(&fallback));
            if retry_ok {
                push_out = retry_out;
                push_succeeded = true;
                if body
                    .get("persist_https_remote")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let _ = git_call_argv(
                        &["remote", "set-url", "origin", fallback.https_url.as_str()],
                        repo.as_deref(),
                    );
                }
                ssh_fallback = Some(fallback);
            } else {
                push_out = format!(
                    "{}\n[https fallback via {} also failed]\n{}",
                    push_out, fallback.https_url, retry_out
                );
            }
        }
    }
    let mut transient_attempts = 1u32;
    let mut remote_request_ids: Vec<String> = Vec::new();
    if !push_succeeded && push_output_is_transient_remote_rejection(&push_out) {
        remote_request_ids = push_output_remote_request_ids(&push_out);
        while !push_succeeded && transient_attempts < GIT_PUSH_TRANSIENT_REMOTE_MAX_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(
                GIT_PUSH_TRANSIENT_REMOTE_BACKOFF_MS * transient_attempts as u64,
            ));
            transient_attempts += 1;
            let (out, ok_now) =
                exec_git_push_in(repo.as_deref(), source_ref, &branch, ssh_fallback.as_ref());
            for id in push_output_remote_request_ids(&out) {
                if !remote_request_ids.contains(&id) {
                    remote_request_ids.push(id);
                }
            }
            push_out = out;
            push_succeeded = ok_now;
        }
    }
    let mut attempts = 0u32;
    let mut rebased = false;
    if !push_succeeded && explicit_source_ref.is_some() {
        if push_output_is_github_auth_failure(&push_out) {
            return pack(json!({
                "ok": false,
                "verb": "git_push",
                "gate_denied": true,
                "repo": repo,
                "branch": branch,
                "source_ref": source_ref,
                "source_sha": local_source_before,
                "preserved_dirty_worktree": preserved_dirty_worktree,
                "reason": format!(
                    "push of explicit source ref '{}' to {} failed because GitHub authentication did not complete; this is not evidence that the remote moved. AgentPlug supplies GitHub credentials through its automatic bridge. Complete the normal GitHub CLI sign-in for this origin (for example, `gh auth login`), then re-dispatch the same git_push request. Do not copy a token or change git credential configuration. Output:\\n{}",
                    source_ref, branch, push_out
                ),
                "next_dispatch": "instruction",
                "next_action_hint": "Complete normal GitHub CLI sign-in, then re-dispatch the same git_push request; do not copy tokens or change git credential configuration.",
            }).to_string());
        }
        if push_output_is_transient_remote_rejection(&push_out) {
            return pack_transient_remote_push_failure(
                &repo,
                &branch,
                source_ref,
                &local_source_before,
                preserved_dirty_worktree,
                transient_attempts,
                &remote_request_ids,
                &push_out,
            );
        }
        log_deviation_push("push-explicit-ref-remote-moved", &branch);
        let mut refusal = json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "remote_moved": true,
            "repo": repo,
            "branch": branch,
            "source_ref": source_ref,
            "source_sha": local_source_before,
            "preserved_dirty_worktree": preserved_dirty_worktree,
            "reason": format!(
                "push of explicit source ref '{}' to {} failed because the remote moved (e.g. a CI autobump commit landed after this ref was created); git_push will not rebase or otherwise mutate a dirty checkout for an isolated-ref publication. Recover with exactly: git_pull {{branch:\"{}\"}} to fast-forward past the remote's new commit, then git_push {{rev:\"HEAD\"}} (or git_finalize {{rev:\"HEAD\"}}) to publish this ref on top of it. Output:\n{}",
                source_ref, branch, branch, push_out
            ),
            "next_dispatch": "instruction",
            "next_action_hint": "git_pull {branch} then git_push {rev:\"HEAD\"}",
        });
        match remote_moved_recovery_eligible(body, repo.as_deref(), &branch, &local_source_before)
        {
            None => refusal = pull_and_repush_remote_moved(body, &repo, &branch, refusal),
            Some(reason) => {
                refusal["auto_recovery"] = json!({ "attempted": false, "skipped_reason": reason });
            }
        }
        hoist_identity_required(&mut refusal);
        return pack(refusal.to_string());
    }
    while !push_succeeded
        && attempts < 3
        && !push_output_is_transient_remote_rejection(&push_out)
    {
        attempts += 1;
        let rebase_argv = ["pull", "--rebase", "origin", branch.as_str()];
        let rebase_out = match ssh_fallback.as_ref() {
            Some(f) => f.call(&rebase_argv, repo.as_deref()),
            None => git_call_argv(&rebase_argv, repo.as_deref()),
        }
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
        if rebase_failed(&rebase_out) || !git_push_porcelain_in(repo.as_deref()).trim().is_empty() {
            let _ = exec_git_in(repo.as_deref(), "rebase --abort");
            log_deviation_push("push-rebase-conflict", &branch);
            return pack(json!({
                "ok": false,
                "verb": "git_push",
                "gate_denied": true,
                "repo": repo,
                "branch": branch,
                "reason": format!(
                    "push rejected (remote moved); pull --rebase origin {} conflicted and was aborted -- worktree could not be cleanly replayed onto origin. Resolve manually. Rebase output:\n{}",
                    branch, rebase_out
                ),
                "next_dispatch": "instruction",
            }).to_string());
        }
        rebased = true;
        let (out, ok_now) =
            exec_git_push_in(repo.as_deref(), source_ref, &branch, ssh_fallback.as_ref());
        push_out = out;
        push_succeeded = ok_now;
    }
    if !push_succeeded {
        if push_output_is_transient_remote_rejection(&push_out) {
            return pack_transient_remote_push_failure(
                &repo,
                &branch,
                source_ref,
                &local_source_before,
                preserved_dirty_worktree,
                transient_attempts,
                &remote_request_ids,
                &push_out,
            );
        }
        log_deviation_push("push-remote-outpaces", &branch);
        return pack(json!({
            "ok": false,
            "verb": "git_push",
            "gate_denied": true,
            "repo": repo,
            "branch": branch,
            "reason": format!(
                "push to {} failed (non-zero exit_code) after {} rebase-retries -- remote is moving faster than the push can land, or a real error occurred. Re-dispatch git_push after the remote settles. Last output:\n{}",
                branch, attempts, push_out
            ),
            "next_dispatch": "instruction",
        }).to_string());
    }
    let local_source_after = match resolve_ref(repo.as_deref(), source_ref) {
        Some(sha) => sha,
        None => {
            return err(
                "git_push",
                &format!("source ref '{}' disappeared after push", source_ref),
            )
        }
    };
    match verify_push_landed(
        repo.as_deref(),
        &branch,
        &local_source_after,
        remote_before.as_deref(),
        ssh_fallback.as_ref(),
    ) {
        Err(reason) => {
            log_deviation_push("push-claimed-success-unverified", &branch);
            pack(
                json!({
                    "ok": false,
                    "verb": "git_push",
                    "verification_failed": true,
                    "repo": repo,
                    "branch": branch,
                    "source_ref": source_ref,
                    "source_sha": local_source_after,
                    "remote_before": remote_before,
                    "subprocess_output": push_out,
                    "reason": reason,
                    "next_dispatch": "instruction",
                })
                .to_string(),
            )
        }
        Ok((remote_sha, already_current)) => ok(
            "git_push",
            json!({
                "branch": branch,
                "repo": repo,
                "output": push_out,
                "rebased": rebased,
                "rebase_retries": attempts,
                "remote_advanced": !already_current,
                "already_current": already_current,
                "remote_sha": remote_sha,
                "source_ref": source_ref,
                "source_sha": local_source_after,
                "preserved_dirty_worktree": preserved_dirty_worktree,
                "pushed_commits": pushed_commits_published(
                    repo.as_deref(),
                    remote_before.as_deref(),
                    &local_source_after,
                ),
                "ssh_fallback": ssh_fallback.as_ref().map(|f| json!({
                    "used": true,
                    "reason": "origin is an SSH URL that refused authentication while an https route was reachable",
                    "from": f.ssh_url,
                    "via": f.https_url,
                    "remote_rewritten": body.get("persist_https_remote").and_then(|v| v.as_bool()).unwrap_or(false),
                })),
            }),
        ),
    }
}

pub(super) fn git_add(body: &Value) -> u64 {
    git_async_entry("git_add", body, |body, plan| {
        let cwd = body_cwd(body);
        let paths: Vec<String> = body
            .get("paths")
            .or_else(|| body.get("files"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let blocked_paths = hard_excluded_pathspecs(&paths);
        if !blocked_paths.is_empty() {
            return Ok(err_json(
                "git_add",
                protected_pathspec_refusal("git_add", &blocked_paths),
            ));
        }
        let mut write_steps: Vec<Value> = Vec::new();
        for argv_owned in git_add_stage_argvs(&paths, cwd) {
            let argv = as_argv(&argv_owned);
            let r = git_step_replayed_by_call_order(plan, &argv, cwd)?;
            let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
            write_steps.push(r.clone());
            if code != 0 {
                return Ok(err(
                    "git_add",
                    r.get("stderr")
                        .and_then(|x| x.as_str())
                        .unwrap_or("git add failed"),
                ));
            }
        }
        let mut staged: Vec<String> = if paths.is_empty() {
            let out = git_step_replayed_by_call_order(
                plan,
                &["diff", "--cached", "--name-only", "-z"],
                cwd,
            )?;
            out.get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .split('\0')
                .filter(|e| !e.is_empty())
                .map(String::from)
                .collect()
        } else {
            let mut check: Vec<String> = vec![
                "diff".to_string(),
                "--cached".to_string(),
                "--name-only".to_string(),
                "-z".to_string(),
                "--".to_string(),
            ];
            check.extend(paths.iter().cloned());
            let out = git_step_replayed_by_call_order(plan, &as_argv(&check), cwd)?;
            out.get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .split('\0')
                .filter(|e| !e.is_empty())
                .map(String::from)
                .collect()
        };
        staged.sort();
        staged.dedup();
        let unmatched = pathspecs_matching_nothing(cwd, &paths);
        if !paths.is_empty()
            && staged.is_empty()
            && !unmatched.is_empty()
            && unmatched.len() == paths.len()
        {
            return Ok(err_json(
                "git_add",
                pathspec_matches_nothing_refusal("git_add", &paths, &unmatched),
            ));
        }
        let mut payload = json!({ "staged": staged });
        if !paths.is_empty() && staged.is_empty() {
            payload["staged_nothing_for"] = json!(paths);
            payload["error_code"] = json!("pathspec_staged_nothing");
            payload["error"] = json!(format!(
                "git add ran clean but nothing is staged for {} -- the pathspec is empty, already committed, or excluded as not tracked-by-design",
                paths.join(", ")
            ));
        }
        let step_refs: Vec<&Value> = write_steps.iter().collect();
        Ok(ok(
            "git_add",
            with_index_lock_report(with_exclusion_report(payload, cwd, &paths), &step_refs),
        ))
    })
}

pub(super) fn git_commit_found_nothing_staged(sout: &str, serr: &str, cwd: Option<&str>) -> bool {
    for s in [sout, serr] {
        if s.contains("nothing to commit")
            || s.contains("no changes added to commit")
            || s.contains("nothing added to commit")
        {
            return true;
        }
    }
    git_call_argv(&["diff", "--cached", "--quiet"], cwd)
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(1)
        == 0
}

pub(super) fn git_commit_failed_for_missing_identity(sout: &str, serr: &str) -> bool {
    for s in [sout, serr] {
        if s.contains("Author identity unknown")
            || s.contains("Please tell me who you are")
            || s.contains("unable to auto-detect email address")
            || s.contains("unable to auto-detect name")
        {
            return true;
        }
    }
    false
}

pub(super) const MISSING_COMMIT_IDENTITY: &str = "git commit needs an author identity: pass author_name and author_email in the body, or set user.name and user.email in this repository. The identity of an earlier commit is never reused.";

pub(super) fn body_commit_identity(body: &Value) -> Option<(String, String)> {
    let field = |key: &str| body.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").to_string();
    let (name, email) = (field("author_name"), field("author_email"));
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name, email))
}

pub(super) fn commit_session_id(body: &Value) -> String {
    ["SESSION_ID", "session_id", "sessionId"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

pub(super) fn prd_foreign_rows_for_commit(paths: &[String], body: &Value, cwd: Option<&str>) -> Vec<Value> {
    if !paths.iter().any(|path| crate::orchestrator::prd::is_prd_yml_pathspec(path)) {
        return Vec::new();
    }
    let head = git_call_argv(&["show", "HEAD:.gm/prd.yml"], cwd);
    let head_text = if head.get("exit_code").and_then(Value::as_i64) == Some(0) {
        head.get("stdout").and_then(Value::as_str).unwrap_or("")
    } else {
        ""
    };
    let worktree_text = crate::orchestrator::prd::read_prd_text(&crate::orchestrator::prd::prd_path().to_string_lossy())
        .unwrap_or_default();
    crate::orchestrator::prd::foreign_prd_rows(head_text, &worktree_text, &commit_session_id(body))
}

pub(super) fn git_commit_argv(
    message: &str,
    allow_empty: bool,
    amend: bool,
    scoped_paths: &[String],
    identity: Option<&(String, String)>,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if let Some((name, email)) = identity {
        argv.push("-c".to_string());
        argv.push(format!("user.name={name}"));
        argv.push("-c".to_string());
        argv.push(format!("user.email={email}"));
    }
    argv.push("commit".to_string());
    if amend {
        argv.push("--amend".to_string());
    }
    argv.push("-m".to_string());
    argv.push(message.to_string());
    if allow_empty || amend {
        argv.push("--allow-empty".to_string());
    }
    if !scoped_paths.is_empty() {
        argv.push("--".to_string());
        argv.extend(scoped_paths.iter().cloned());
    }
    argv
}

pub(super) fn bundle_prd_commit_comments(message: &str, notes: &[(String, String)]) -> String {
    if notes.is_empty() {
        return message.to_string();
    }
    let mut out = message.to_string();
    out.push_str("\n\nResolved PRD rows:\n");
    for (id, comment) in notes {
        out.push_str(&format!("- {}: {}\n", id, comment));
    }
    out
}

pub(super) fn nul_separated_git_paths(cwd: Option<&str>, args: &str) -> Vec<String> {
    let out = exec_git_in(cwd, args);
    let mut paths: Vec<String> = Vec::new();
    for entry in out.split('\0') {
        if entry.is_empty() || paths.iter().any(|p| p == entry) {
            continue;
        }
        paths.push(entry.to_string());
    }
    paths
}

pub(super) const PRD_STATE_PATHSPEC: &str = ".gm/prd.yml";

pub(super) fn staged_paths_now(cwd: Option<&str>) -> Vec<String> {
    nul_separated_git_paths(cwd, "diff --cached --name-only -z")
}

pub(super) fn commit_note_paths(cwd: Option<&str>, scoped_paths: &[String]) -> Vec<String> {
    if scoped_paths.is_empty() {
        staged_paths_now(cwd)
    } else {
        scoped_paths.to_vec()
    }
}

pub(super) fn blanket_stage_would_take(cwd: Option<&str>) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for args in [
        "diff --cached --name-only -z",
        "diff --name-only -z",
        "ls-files --others --exclude-standard -z",
    ] {
        for entry in nul_separated_git_paths(cwd, args) {
            if !paths.contains(&entry) {
                paths.push(entry);
            }
        }
    }
    paths.sort();
    paths
}

pub(super) fn staged_outside_requested(
    cwd: Option<&str>,
    before: &[String],
    requested: &[String],
) -> Vec<String> {
    staged_paths_now(cwd)
        .into_iter()
        .filter(|path| {
            !before.iter().any(|b| b == path)
                && !requested.iter().any(|r| pathspec_covers_path(r, path))
        })
        .collect()
}

pub(super) fn git_amend(body: &Value) -> u64 {
    let mut amended = body.clone();
    if let Some(fields) = amended.as_object_mut() {
        fields.insert("amend".to_string(), json!(true));
    }
    git_commit(&amended)
}

pub(super) fn git_commit(body: &Value) -> u64 {
    git_async_entry("git_commit", body, |body, plan| {
        let cwd = body_cwd(body);
        let message = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if message.is_empty() {
            return Ok(err("git_commit", "message required"));
        }
        let allow_empty = body
            .get("allow_empty")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let amend = body
            .get("amend")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let paths: Vec<String> = match body.get("paths").or_else(|| body.get("files")) {
            None => Vec::new(),
            Some(v) if v.is_null() => Vec::new(),
            Some(v) => {
                let Some(entries) = v.as_array() else {
                    return Ok(err("git_commit", "paths must be an array of strings"));
                };
                let strings: Vec<String> = entries
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if strings.len() != entries.len() {
                    return Ok(err(
                        "git_commit",
                        &format!(
                            "paths must be an array of strings: {} of {} entries are not strings and were refused rather than dropped",
                            entries.len() - strings.len(),
                            entries.len()
                        ),
                    ));
                }
                strings
            }
        };
        let add_all = body
            .get("add_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if amend {
            let head_probe = git_call_argv(&["rev-parse", "--verify", "--quiet", "HEAD"], cwd);
            if head_probe.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(1) != 0 {
                return Ok(err("git_commit", "amend requires an existing HEAD commit; this repository has no commits yet"));
            }
            let published = git_call_argv(
                &["for-each-ref", "--count=1", "--contains", "HEAD", "--format=%(refname)", "refs/remotes"],
                cwd,
            );
            let remote_refs = published.get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if !remote_refs.is_empty() {
                return Ok(err_json(
                    "git_commit",
                    json!({
                        "error": format!("HEAD is already reachable from a remote-tracking ref ({remote_refs}), so amending would rewrite published history; refused"),
                        "error_code": "pushed_commit_refused",
                    }),
                ));
            }
        }
        let merge_probe = git_step_replayed_by_call_order(
            plan,
            &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"],
            cwd,
        )?;
        let merge_pending = match merge_probe.get("exit_code").and_then(Value::as_i64) {
            Some(0) => true,
            Some(1) => false,
            _ => {
                return Ok(err(
                    "git_commit",
                    "unable to inspect repository merge state",
                ))
            }
        };
        if merge_pending && (body.get("paths").is_some() || body.get("files").is_some() || add_all)
        {
            return Ok(err_json(
                "git_commit",
                json!({
                    "error": "a merge commit consumes the complete prepared index; path-scoped or add_all requests are refused before staging. Resolve and review the staged merge, then call git_commit without paths or add_all",
                    "error_code": "merge_requires_prepared_index",
                }),
            ));
        }
        let blocked_paths = hard_excluded_pathspecs(&paths);
        if !blocked_paths.is_empty() {
            return Ok(err_json(
                "git_commit",
                protected_pathspec_refusal("git_commit", &blocked_paths),
            ));
        }
        let allow_whole_index = body
            .get("allow_whole_index")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let foreign_prd_rows = prd_foreign_rows_for_commit(&paths, body, cwd);
        let allow_foreign_prd_rows = body
            .get("allow_foreign_prd_rows")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !foreign_prd_rows.is_empty() && !allow_foreign_prd_rows {
            return Ok(err_json(
                "git_commit",
                json!({
                    "error": format!("paths name .gm/prd.yml, which carries {} uncommitted row change(s) owned by other sessions; a path-scoped commit would sweep them in. Commit after their owners commit, or pass allow_foreign_prd_rows: true to include them deliberately", foreign_prd_rows.len()),
                    "error_code": "prd_foreign_rows",
                    "foreign_rows": foreign_prd_rows,
                }),
            ));
        }
        let staged_before = if add_all {
            Vec::new()
        } else {
            let stdout = git_step_replayed_by_call_order(
                plan,
                &["diff", "--cached", "--name-only", "-z"],
                cwd,
            )?
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
            stdout
                .split('\0')
                .filter(|entry| !entry.is_empty())
                .map(String::from)
                .collect()
        };
        if !add_all && paths.is_empty() && !allow_whole_index {
            let sweep = blanket_stage_would_take(cwd);
            if !sweep.is_empty() {
                return Ok(err_json(
                    "git_commit",
                    blanket_stage_refusal("git_commit", &sweep),
                ));
            }
        }
        let head_before_probe = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
        let dedup_key = git_commit_dedup_key(cwd, &head_before_probe, message, &paths, add_all, amend);
        if let Some(prior) = git_commit_dedup_lookup(&dedup_key, cwd) {
            let sha = prior
                .get("sha")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let summary = prior
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(ok(
                "git_commit",
                json!({
                    "committed": true, "sha": sha, "summary": summary,
                    "replayed_from_recent_identical_dispatch": true,
                }),
            ));
        }
        let status_r =
            git_step_replayed_by_call_order(plan, &as_argv(&git_porcelain_argv(&[], cwd)), cwd)?;
        let porcelain = require_complete_git_porcelain("git_commit", &status_r)?;
        if porcelain.trim().is_empty() && !allow_empty && !amend {
            return Ok(ok(
                "git_commit",
                with_exclusion_report(json!({ "nothing_to_commit": true, "requested_paths": paths }), cwd, &paths),
            ));
        }
        if let Some(reason) = bin_drift_commit_refusal(cwd, &paths, add_all) {
            return Ok(err_json(
                "git_commit",
                json!({
                    "error": reason,
                    "gate_denied": true,
                    "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
                    "next_dispatch": "instruction",
                    "next_dispatch_hint": "instruction",
                }),
            ));
        }
        if !paths.is_empty() {
            let scoped_r = git_step_replayed_by_call_order(
                plan,
                &as_argv(&git_porcelain_argv(&paths, cwd)),
                cwd,
            )?;
            let scoped_porcelain = require_complete_git_porcelain("git_commit", &scoped_r)?;
            if scoped_porcelain.trim().is_empty() && !allow_empty {
                let unmatched = pathspecs_matching_nothing(cwd, &paths);
                if !unmatched.is_empty() && unmatched.len() == paths.len() {
                    return Ok(err_json(
                        "git_commit",
                        pathspec_matches_nothing_refusal("git_commit", &paths, &unmatched),
                    ));
                }
                return Ok(err_json(
                    "git_commit",
                    json!({
                        "error": format!("nothing to commit in the requested pathspec(s): {} -- the whole repo has other dirty paths, so this refusal is scoped to what you named", paths.join(", ")),
                        "error_code": "nothing_to_commit_for_paths",
                        "requested_paths": paths,
                        "next_dispatch": "git_commit",
                    }),
                ));
            }
        }
        let head_r = git_step_replayed_by_call_order(plan, &["rev-parse", "HEAD"], cwd)?;
        let head_before = head_r
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let scan = super::dangling_refs::scan_commit(cwd, &paths, add_all, body);
        if super::dangling_refs::scan_unreadable(&scan) {
            return Ok(err_json(
                "git_commit",
                super::dangling_refs::unreadable_detail("git_commit", &scan),
            ));
        }
        if !scan.offenders.is_empty() {
            return Ok(err_json(
                "git_commit",
                super::dangling_refs::refusal_detail("git_commit", &scan),
            ));
        }
        let mut stage_stderr = String::new();
        let mut force_added_ignored_paths: Vec<String> = Vec::new();
        let mut write_steps: Vec<Value> = Vec::new();
        if add_all || !paths.is_empty() {
            let staged_paths: &[String] = if add_all { &[] } else { &paths };
            let ignored = ignored_requested_paths(plan, cwd, staged_paths)?;
            let stage_argv = if ignored.is_empty() {
                git_stage_argv(staged_paths, cwd)
            } else {
                git_stage_argv_forced(staged_paths, cwd)
            };
            let stage = git_step_replayed_by_call_order(plan, &as_argv(&stage_argv), cwd)?;
            write_steps.push(stage.clone());
            if !add_all && !paths.is_empty() {
                let extra = staged_outside_requested(cwd, &staged_before, &paths);
                if !extra.is_empty() {
                    return Ok(err_json(
                        "git_commit",
                        unrequested_stage_refusal("git_commit", &paths, &extra),
                    ));
                }
            }
            stage_stderr = stage.get("stderr").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            force_added_ignored_paths = ignored;
        }
        let scoped_paths: &[String] = if add_all { &[] } else { &paths };
        if !scoped_paths.is_empty() {
            let mut check: Vec<String> = vec![
                "diff".to_string(),
                "--cached".to_string(),
                "--name-only".to_string(),
                "--".to_string(),
            ];
            check.extend(scoped_paths.iter().cloned());
            let r = git_step_replayed_by_call_order(plan, &as_argv(&check), cwd)?;
            if r.get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().is_empty() {
                let unmatched = pathspecs_matching_nothing(cwd, scoped_paths);
                if !unmatched.is_empty() {
                    return Ok(err_json("git_commit", pathspec_matches_nothing_refusal("git_commit", scoped_paths, &unmatched)));
                }
                return Ok(err_json("git_commit", json!({
                    "error": format!("no staged content for the requested pathspec(s): {} -- git add reported: {}", scoped_paths.join(", "), if stage_stderr.is_empty() { "no matching file".to_string() } else { stage_stderr }),
                    "error_code": ERR_CODE_INVALID_ARGS,
                    "requested_paths": scoped_paths,
                })));
            }
        }
        let commit_notes = crate::orchestrator::prd::pending_commit_comments_for_paths(cwd, &commit_note_paths(cwd, scoped_paths));
        let bundled_message = bundle_prd_commit_comments(message, &commit_notes);
        let identity = body_commit_identity(body);
        let r = git_step_replayed_by_call_order(
            plan,
            &as_argv(&git_commit_argv(&bundled_message, allow_empty, amend, scoped_paths, identity.as_ref())),
            cwd,
        )?;
        write_steps.push(r.clone());
        if r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0) != 0 {
            let serr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            let sout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
            if git_commit_found_nothing_staged(sout, serr, cwd) {
                return Ok(ok(
                    "git_commit",
                    with_exclusion_report(json!({ "nothing_to_commit": true, "requested_paths": paths }), cwd, &paths),
                ));
            }
            if git_commit_failed_for_missing_identity(sout, serr) {
                return Ok(err("git_commit", MISSING_COMMIT_IDENTITY));
            }
            return Ok(err("git_commit", if serr.is_empty() { sout } else { serr }));
        }
        let after_r = git_step_replayed_by_call_order(plan, &["rev-parse", "HEAD"], cwd)?;
        let head_after = after_r
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if head_after.is_empty() || head_after == head_before {
            return Ok(err("git_commit", "commit reported success (exit 0) but HEAD did not move -- refusing to claim committed:true without a real new sha"));
        }
        crate::orchestrator::prd::drain_commit_comments(cwd, &commit_notes);
        let sha = head_after[..head_after.len().min(10)].to_string();
        let summary = message.lines().next().unwrap_or("").to_string();
        git_commit_dedup_record(&dedup_key, &head_after, &sha, &summary);
        emit_event(
            "git_commit",
            json!({ "sub": "git", "sha_full": head_after, "sha": sha, "summary": summary }),
        );
        record_commit_in_liqology(&summary, &head_after);
        let mut payload = json!({ "committed": true, "sha": sha, "summary": summary });
        if amend {
            payload["amended"] = json!(true);
            payload["replaced_sha_full"] = json!(head_before);
        }
        if !force_added_ignored_paths.is_empty() {
            payload["force_added_ignored_paths"] = json!(force_added_ignored_paths);
        }
        if !scan.waived.is_empty() {
            payload["dangling_waived"] = json!(scan.waived);
        }
        if !allow_whole_index && !add_all && paths.is_empty() && !staged_before.is_empty() {
            payload["whole_index_commit"] = json!(true);
            payload["staged_count"] = json!(staged_before.len());
            payload["staged_paths"] = json!(staged_before);
            payload["warning"] = json!(format!(
                "this commit took the whole index ({} path(s)) because no paths were given; pass paths to scope it or allow_whole_index: true to accept it explicitly",
                staged_before.len()
            ));
        }
        if allow_whole_index && paths.is_empty() {
            payload["blanket_opt_in"] = json!("allow_whole_index");
            payload["swept_paths"] = json!(blanket_stage_would_take(cwd));
        }
        let reopened_rows = crate::orchestrator::prd::reopen_rows_for_changed_paths(&files_in_commit(cwd));
        if !reopened_rows.is_empty() {
            payload["prd_reopened"] = json!(reopened_rows);
        }
        let step_refs: Vec<&Value> = write_steps.iter().collect();
        Ok(ok(
            "git_commit",
            with_index_lock_report(with_exclusion_report(payload, cwd, &paths), &step_refs),
        ))
    })
}

pub(super) fn record_commit_in_liqology(summary: &str, sha_full: &str) {
    let Some(embedding) = crate::embed::embed_text(summary) else {
        emit_event(
            "liqology_record_skipped",
            json!({ "reason": "embed_failed", "sha_full": sha_full }),
        );
        return;
    };
    let resp = call_plugin(
        "liqology",
        "record",
        &json!({ "input_embedding": embedding, "output_embedding": embedding }),
    );
    if !plugin_ok(&resp) {
        emit_event(
            "liqology_record_skipped",
            json!({
                "reason": plugin_failure_code(&resp),
                "sha_full": sha_full,
            }),
        );
    }
}

pub(super) fn check_ci_status_and_write_validated_marker_if_green(
    repo_cwd: Option<&str>,
    head_sha: &str,
) -> (Value, bool) {
    let ci_check = ci_status_value(&json!({ "cwd": repo_cwd, "sha": head_sha }));
    match &ci_check {
        Ok(v) => {
            let status = v
                .get("data")
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let written = if (status == "success" || status == "no_applicable_workflow")
                && !head_sha.is_empty()
            {
                let marker = json!({ "head_sha": head_sha, "reason": status }).to_string();
                crate::pkfs::write(".gm/exec-spool/.ci-validated", &marker)
            } else {
                false
            };
            (v.get("data").cloned().unwrap_or(Value::Null), written)
        }
        Err(e) => (e.clone(), false),
    }
}

pub(super) fn untracked_worktree_paths(cwd: Option<&str>) -> Vec<String> {
    exec_git_in(cwd, "ls-files --others --exclude-standard")
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

pub(super) fn incoming_changed_paths(cwd: Option<&str>, branch: &str) -> Vec<String> {
    exec_git_in(cwd, &format!("diff --name-only HEAD...origin/{}", branch))
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

pub(super) fn local_dirty_paths(cwd: Option<&str>) -> Vec<String> {
    let mut paths: Vec<String> = exec_git_in(cwd, "diff --name-only HEAD")
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    paths.extend(untracked_worktree_paths(cwd));
    paths
}

pub(super) fn commits_between(cwd: Option<&str>, from: &str, to: &str) -> Value {
    let listed = exec_git_in(
        cwd,
        &format!("log --reverse --pretty=format:%H%x09%s {}..{}", from, to),
    );
    let rows: Vec<Value> = listed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (sha, summary) = line.split_once('\t').unwrap_or((line, ""));
            json!({ "sha": sha.trim(), "summary": summary.trim() })
        })
        .collect();
    json!(rows)
}

pub(super) enum RemoteMovedPull {
    NotApplicable,
    Blocked(String),
    Failed(Value),
    Landed {
        ff_only: bool,
        result: Value,
        incoming_commits: Value,
        remote_sha_before: String,
    },
}

pub(super) fn pull_body_with_identity(
    cwd: Option<&str>,
    branch: &str,
    ff_only: bool,
    identity: Option<&(String, String)>,
) -> Value {
    let mut body = json!({ "cwd": cwd, "branch": branch, "ff_only": ff_only });
    if let Some((name, email)) = identity {
        if let Some(map) = body.as_object_mut() {
            map.insert("user_name".to_string(), json!(name.clone()));
            map.insert("user_email".to_string(), json!(email.clone()));
        }
    }
    body
}

pub(super) fn pull_past_remote_moved(
    cwd: Option<&str>,
    push_resp: &Value,
    identity: Option<&(String, String)>,
) -> RemoteMovedPull {
    if !push_resp
        .get("remote_moved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return RemoteMovedPull::NotApplicable;
    }
    let branch = push_resp
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if branch.is_empty() {
        return RemoteMovedPull::Blocked("push refusal named no branch to pull".to_string());
    }
    let _ = git_call_argv(&["fetch", "origin", branch.as_str()], cwd);
    let incoming = incoming_changed_paths(cwd, &branch);
    let local = local_dirty_paths(cwd);
    let clash: Vec<String> = local
        .into_iter()
        .filter(|path| incoming.iter().any(|incoming| incoming == path))
        .collect();
    if !clash.is_empty() {
        return RemoteMovedPull::Blocked(format!(
            "the incoming commit changes {} which this worktree also has uncommitted -- pulling would overwrite it",
            clash.join(", ")
        ));
    }
    let remote_sha_before = resolve_ref(cwd, &format!("origin/{}", branch)).unwrap_or_default();
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let ff_body = pull_body_with_identity(cwd, &branch, true, identity);
    let ff_resp = unpack_to_value(git_pull(&ff_body));
    if ff_resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return RemoteMovedPull::Landed {
            ff_only: true,
            result: ff_resp,
            incoming_commits: commits_between(cwd, &head_before, &remote_sha_before),
            remote_sha_before,
        };
    }
    let merge_resp = unpack_to_value(git_pull(&pull_body_with_identity(
        cwd, &branch, false, identity,
    )));
    let merged = merge_resp
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if merged {
        return RemoteMovedPull::Landed {
            ff_only: false,
            result: merge_resp,
            incoming_commits: commits_between(cwd, &head_before, &remote_sha_before),
            remote_sha_before,
        };
    }
    let conflicted = merge_resp
        .get("conflicted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if conflicted {
        let _ = git_call_argv(&["merge", "--abort"], cwd);
    }
    RemoteMovedPull::Failed(json!({
        "ff_only": ff_resp,
        "merge": merge_resp,
        "merge_aborted": conflicted,
    }))
}

/// `git_push` refuses a `remote_moved` explicit-ref publication rather than mutating the
/// checkout, which leaves the caller to dispatch `git_pull` and `git_push` by hand. This is the
/// recovery that does exactly those two dispatches: one pull through `git_pull` (strict
/// fast-forward first, ordinary merge only if the local branch diverged) and then one push.
/// The worktree and index must be clean, or it refuses unchanged with the original reason.
pub(super) enum RemoteMovedRecoveryOpt {
    Explicit(bool),
    Defaulted,
}

pub(super) fn remote_moved_recovery_opt(body: &Value) -> RemoteMovedRecoveryOpt {
    match body
        .get("recover_remote_moved")
        .or_else(|| body.get("pull_first"))
        .and_then(|v| v.as_bool())
    {
        Some(want) => RemoteMovedRecoveryOpt::Explicit(want),
        None => RemoteMovedRecoveryOpt::Defaulted,
    }
}

fn push_body_requests_force(body: &Value) -> bool {
    body.get("force")
        .or_else(|| body.get("force_with_lease"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `remote_moved` is a label this verb attaches to every explicit-ref push rejection that is
/// neither a GitHub auth failure nor a transient 5xx, so on its own it compares nothing at all.
/// Acting on it by default therefore needs real ancestry evidence: the remote tip resolves,
/// differs from the pushed sha, is not already contained in it, and the two share a merge base.
/// Those four are what "the remote moved" means -- the same branch carrying new commits the
/// pushed ref does not have, whether or not the pushed ref also carries commits of its own. A
/// missing remote tip, unrelated history and a remote already contained in the pushed ref all
/// fail it; a pushed ref that is merely BEHIND passes, because pulling onto it fast-forwards and
/// cannot lose work, and that is the ordinary shape this default exists for.
pub(super) fn remote_moved_is_same_branch_advance(
    repo: Option<&str>,
    branch: &str,
    local_sha: &str,
) -> bool {
    let remote_sha = match resolve_ref(repo, &format!("origin/{}", branch)) {
        Some(sha) => sha,
        None => return false,
    };
    if remote_sha == local_sha {
        return false;
    }
    let is_ancestor = |ancestor: &str, descendant: &str| {
        git_call_argv(&["merge-base", "--is-ancestor", ancestor, descendant], repo)
            .get("exit_code")
            .and_then(|code| code.as_i64())
            .unwrap_or(1)
            == 0
    };
    if is_ancestor(remote_sha.as_str(), local_sha) {
        return false;
    }
    let merge_base = exec_git_in(repo, &format!("merge-base {} {}", remote_sha, local_sha))
        .trim()
        .to_string();
    !merge_base.is_empty()
}

/// `None` means recover now. `Some(reason)` means do not touch the checkout, and names why.
pub(super) fn remote_moved_recovery_eligible(
    body: &Value,
    repo: Option<&str>,
    branch: &str,
    local_sha: &str,
) -> Option<&'static str> {
    match remote_moved_recovery_opt(body) {
        RemoteMovedRecoveryOpt::Explicit(true) => None,
        RemoteMovedRecoveryOpt::Explicit(false) => {
            Some("caller declined recovery with recover_remote_moved:false (or pull_first:false); the checkout is left exactly as it is.")
        }
        RemoteMovedRecoveryOpt::Defaulted => {
            if push_body_requests_force(body) {
                return Some("a forced push is never recovered automatically; pass recover_remote_moved:true if pulling onto it is genuinely intended.");
            }
            if resolve_ref(repo, "HEAD").as_deref() != Some(local_sha) {
                return Some("the pushed ref is not this checkout's HEAD, so it was named with intent; automatic recovery is limited to publishing the checkout itself. Pass recover_remote_moved:true to recover this ref.");
            }
            if !remote_moved_is_same_branch_advance(repo, branch, local_sha) {
                return Some("this rejection is not the same branch carrying new commits the pushed ref lacks (remote tip missing, unrelated history, or remote already contained in the pushed ref), so recovery stays opt-in.");
            }
            None
        }
    }
}

pub(super) fn pull_and_repush_remote_moved(
    body: &Value,
    repo: &Option<String>,
    branch: &str,
    mut refusal: Value,
) -> Value {
    let cwd = repo.as_deref();
    let dirty = git_push_porcelain_in(cwd);
    if !dirty.trim().is_empty() {
        refusal["recovered"] = json!(false);
        refusal["auto_recovery"] = json!({
            "attempted": false,
            "skipped_reason": format!(
                "worktree or index is not clean, so a pull could clobber uncommitted changes -- refusing exactly as before. Porcelain:\n{}",
                dirty.lines().take(8).collect::<Vec<_>>().join("\n")
            ),
        });
        return refusal;
    }
    let _ = git_call_argv(&["fetch", "origin", branch], cwd);
    let remote_now = resolve_ref(cwd, &format!("origin/{}", branch));
    let strict_fast_forward = remote_now
        .as_deref()
        .map(|remote| {
            git_call_argv(&["merge-base", "--is-ancestor", "HEAD", remote], cwd)
                .get("exit_code")
                .and_then(|code| code.as_i64())
                .unwrap_or(1)
                == 0
        })
        .unwrap_or(false);
    match pull_past_remote_moved(cwd, &refusal, body_pull_identity(body).as_ref()) {
        RemoteMovedPull::NotApplicable => refusal,
        RemoteMovedPull::Blocked(reason) => {
            refusal["recovered"] = json!(false);
            refusal["auto_recovery"] = json!({ "attempted": false, "skipped_reason": reason });
            refusal
        }
        RemoteMovedPull::Failed(result) => {
            refusal["recovered"] = json!(false);
            refusal["auto_recovery"] = json!({ "attempted": true, "pull_result": result });
            refusal
        }
        RemoteMovedPull::Landed {
            ff_only,
            result,
            incoming_commits,
            remote_sha_before,
        } => {
            let mut retry_body = body.clone();
            if let Some(map) = retry_body.as_object_mut() {
                map.insert(
                    "rev".to_string(),
                    json!(exec_git_in(cwd, "rev-parse HEAD").trim().to_string()),
                );
                map.remove("recover_remote_moved");
                map.remove("pull_first");
            }
            let mut retry_resp = unpack_to_value(git_push(&retry_body));
            let repushed = retry_resp
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let pulled_to = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
            let pull_via = if ff_only { "ff_only" } else { "merge" };
            let recovery = json!({
                "attempted": true,
                "pull_via": pull_via,
                "strict_fast_forward": strict_fast_forward,
                "pull_result": result.clone(),
                "incoming_commits": incoming_commits,
                "remote_sha_before": remote_sha_before,
                "pulled_to": pulled_to.clone(),
                "repushed": repushed,
            });
            if !repushed {
                refusal["recovered"] = json!(false);
                refusal["pulled_to"] = json!(pulled_to);
                refusal["auto_recovery"] = json!({
                    "attempted": true,
                    "pull_via": pull_via,
                    "strict_fast_forward": strict_fast_forward,
                    "pull_result": result,
                    "retry_result": retry_resp,
                    "repushed": false,
                });
                return refusal;
            }
            log_deviation_push("push-remote-moved-auto-recovered", branch);
            if let Some(data) = retry_resp.get_mut("data").and_then(|d| d.as_object_mut()) {
                data.insert("recovered".to_string(), json!(true));
                data.insert("pulled_to".to_string(), json!(pulled_to.clone()));
                data.insert("auto_recovery".to_string(), recovery.clone());
            }
            if let Some(top) = retry_resp.as_object_mut() {
                top.insert("recovered".to_string(), json!(true));
                top.insert("pulled_to".to_string(), json!(pulled_to));
                top.insert("auto_recovery".to_string(), recovery);
            }
            retry_resp
        }
    }
}

pub(super) fn push_refusal_code(push_resp: &Value) -> Option<String> {
    for candidate in [Some(push_resp), push_resp.get("data")] {
        if let Some(candidate) = candidate {
            for key in ["reason_code", "error_code"] {
                if let Some(code) = candidate.get(key).and_then(|v| v.as_str()) {
                    return Some(code.to_string());
                }
            }
        }
    }
    None
}

pub(super) fn apply_push_disabled_reason(refusal: &mut Value, push_resp: &Value) {
    if push_refusal_code(push_resp).as_deref() != Some("push_disabled_by_config") {
        return;
    }
    let mut text: Option<String> = None;
    for candidate in [Some(push_resp), push_resp.get("data")] {
        if let Some(candidate) = candidate {
            if let Some(reason) = candidate.get("reason").and_then(|v| v.as_str()) {
                if !reason.trim().is_empty() {
                    text = Some(reason.to_string());
                    break;
                }
            }
        }
    }
    if let Some(text) = text {
        refusal["reason"] = json!(text);
    }
    refusal["reason_code"] = json!("push_disabled_by_config");
    refusal["error_code"] = json!("push_disabled_by_config");
    refusal["push_disabled_by_config"] = json!(true);
}

pub(super) fn identity_required_in_pull_result(pull_result: &Value) -> Option<Value> {
    for key in ["merge", "ff_only"] {
        if let Some(candidate) = pull_result.get(key) {
            if candidate.get("error_code").and_then(|v| v.as_str()) == Some("git_identity_required")
            {
                return Some(candidate.clone());
            }
        }
    }
    if pull_result.get("error_code").and_then(|v| v.as_str()) == Some("git_identity_required") {
        return Some(pull_result.clone());
    }
    None
}

pub(super) fn hoist_identity_required(refusal: &mut Value) {
    let pull_result = match refusal.get("auto_recovery").and_then(|r| r.get("pull_result")) {
        Some(pull_result) => pull_result.clone(),
        None => return,
    };
    let pull_resp = match identity_required_in_pull_result(&pull_result) {
        Some(pull_resp) => pull_resp,
        None => return,
    };
    refusal["reason"] = json!(pull_resp
        .get("hint")
        .and_then(|v| v.as_str())
        .unwrap_or(
            "Git needs a commit identity for this merge. Set this repository's user.name and user.email, then retry."
        )
        .to_string());
    refusal["reason_code"] = json!("git_identity_required");
    refusal["error_code"] = json!("git_identity_required");
    refusal["identity_required"] = json!(true);
    if let Some(argv) = pull_resp.get("repo_local_config_argv") {
        refusal["repo_local_config_argv"] = argv.clone();
    }
    if let Some(error) = pull_resp.get("error") {
        refusal["identity_error"] = error.clone();
    }
}

pub(super) fn git_finalize(body: &Value) -> u64 {
    let repo = body_cwd(body).map(String::from);
    let cwd = repo.clone();
    let cwd_ref = cwd.as_deref();
    let explicit_source_ref = body
        .get("rev")
        .or_else(|| body.get("source_ref"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from);
    if let Some(source_ref) = explicit_source_ref {
        let push_resp = unpack_to_value(git_push(body));
        let pushed = push_resp
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !pushed {
            let mut refusal = json!({
                "ok": false,
                "verb": "git_finalize",
                "committed": false,
                "pushed": false,
                "source_ref": source_ref,
                "push_result": push_resp,
                "reason": "isolated-ref publication was refused -- read push_result.reason",
                "next_dispatch": "instruction",
            });
            apply_push_disabled_reason(&mut refusal, &push_resp);
            return pack(refusal.to_string());
        }
        let push_data = push_resp.get("data").cloned().unwrap_or(Value::Null);
        let source_sha = push_data
            .get("source_sha")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let (ci_status_summary, ci_validated_written) =
            check_ci_status_and_write_validated_marker_if_green(cwd_ref, &source_sha);
        return ok(
            "git_finalize",
            json!({
                "committed": false,
                "pushed": true,
                "source_ref": source_ref,
                "sha": source_sha,
                "branch": push_data.get("branch").cloned().unwrap_or(Value::Null),
                "remote_advanced": push_data.get("remote_advanced").cloned().unwrap_or(Value::Null),
                "already_current": push_data.get("already_current").cloned().unwrap_or(Value::Null),
                "remote_sha": push_data.get("remote_sha").cloned().unwrap_or(Value::Null),
                "preserved_dirty_worktree": push_data.get("preserved_dirty_worktree").cloned().unwrap_or(Value::Null),
                "pushed_commits": push_data.get("pushed_commits").cloned().unwrap_or(Value::Null),
                "steps": [
                    {"step": "commit", "skipped": "explicit source_ref publishes an existing commit"},
                    {"step": "push", "branch": push_data.get("branch").cloned().unwrap_or(Value::Null)},
                    {"step": "ci-status-check", "result": ci_status_summary, "ci_validated_marker_written": ci_validated_written}
                ],
                "ci_validated_marker_written": ci_validated_written,
                "next_dispatch": if ci_validated_written { "instruction" } else { "ci-status" },
            }),
        );
    }
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let paths: Vec<String> = body
        .get("paths")
        .or_else(|| body.get("files"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let scoped = !paths.is_empty();
    let blocked_paths = hard_excluded_pathspecs(&paths);
    if !blocked_paths.is_empty() {
        return err_json(
            "git_finalize",
            protected_pathspec_refusal("git_finalize", &blocked_paths),
        );
    }
    let mut steps: Vec<Value> = vec![];
    let mut write_steps: Vec<Value> = Vec::new();
    let mut committed = false;
    let mut sha = String::new();
    let mut summary = String::new();
    let head_before_any_commit = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();

    let allow_whole_index = body
        .get("allow_whole_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let staged_before: Vec<String> = exec_git_in(cwd_ref, "diff --cached --name-only -z")
        .split('\0')
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect();
    if !scoped && !allow_whole_index {
        let sweep = blanket_stage_would_take(cwd_ref);
        if !sweep.is_empty() {
            return err_json(
                "git_finalize",
                blanket_stage_refusal("git_finalize", &sweep),
            );
        }
    }
    let dirty = match checked_git_porcelain_scoped("git_finalize", cwd_ref, &paths) {
        Ok(porcelain) => !porcelain.trim().is_empty(),
        Err(refusal) => return refusal,
    };
    let mut dangling_waived: Vec<String> = Vec::new();
    if dirty {
        if message.is_empty() {
            return err(
                "git_finalize",
                "worktree dirty but no commit message provided -- pass {message}",
            );
        }
        let scan = super::dangling_refs::scan_commit(cwd_ref, &paths, !scoped, body);
        if super::dangling_refs::scan_unreadable(&scan) {
            return err_json(
                "git_finalize",
                super::dangling_refs::unreadable_detail("git_finalize", &scan),
            );
        }
        if !scan.offenders.is_empty() {
            return err_json(
                "git_finalize",
                super::dangling_refs::refusal_detail("git_finalize", &scan),
            );
        }
        dangling_waived = scan.waived;
        let foreign_prd_rows = prd_foreign_rows_for_commit(&paths, body, cwd_ref);
        let allow_foreign_prd_rows = body
            .get("allow_foreign_prd_rows")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !foreign_prd_rows.is_empty() && !allow_foreign_prd_rows {
            return err_json(
                "git_finalize",
                json!({
                    "error": format!("paths name .gm/prd.yml, which carries {} uncommitted row change(s) owned by other sessions; a path-scoped finalize would sweep them in. Commit after their owners commit, or pass allow_foreign_prd_rows: true to include them deliberately", foreign_prd_rows.len()),
                    "error_code": "prd_foreign_rows",
                    "foreign_rows": foreign_prd_rows,
                }),
            );
        }
        let ignored = ignored_requested_paths_now(cwd_ref, &paths);
        let stage_argv = if ignored.is_empty() {
            git_stage_argv(&paths, cwd_ref)
        } else {
            git_stage_argv_forced(&paths, cwd_ref)
        };
        let stage = git_call_argv(&as_argv(&stage_argv), cwd_ref);
        let stage_code = stage.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        let stage_stderr = stage
            .get("stderr")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let stage_stdout = stage
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        write_steps.push(stage.clone());
        if scoped {
            let extra = staged_outside_requested(cwd_ref, &staged_before, &paths);
            if !extra.is_empty() {
                return err_json(
                    "git_finalize",
                    unrequested_stage_refusal("git_finalize", &paths, &extra),
                );
            }
        }
        if stage_code != 0 {
            return err_json(
                "git_finalize",
                json!({
                    "error": format!("git_finalize failed at step 'git add' (exit code {}): {}", stage_code, if stage_stderr.is_empty() { stage_stdout.clone() } else { stage_stderr.clone() }),
                    "error_code": ERR_CODE_FAILED,
                    "failed_step": "git_add",
                    "step_stderr": stage_stderr,
                    "step_stdout": stage_stdout,
                    "requested_paths": paths,
                    "next_dispatch": "git_finalize",
                }),
            );
        }
        if scoped && paths_staged_nothing(cwd_ref, &paths) {
            let unmatched = pathspecs_matching_nothing(cwd_ref, &paths);
            if !unmatched.is_empty() {
                return err_json("git_finalize", pathspec_matches_nothing_refusal("git_finalize", &paths, &unmatched));
            }
            return err_json("git_finalize", json!({
                "error": format!("git_finalize failed at step 'git add': it exited 0 but staged nothing for the requested pathspec(s): {} -- git add stderr: {}", paths.join(", "), if stage_stderr.is_empty() { "(empty)".to_string() } else { stage_stderr.clone() }),
                "error_code": ERR_CODE_INVALID_ARGS,
                "failed_step": "git_add",
                "step_stderr": stage_stderr,
                "step_stdout": stage_stdout,
                "requested_paths": paths,
            }));
        }
        let scoped_paths: &[String] = if scoped { &paths } else { &[] };
        let commit_notes = crate::orchestrator::prd::pending_commit_comments_for_paths(cwd_ref, &commit_note_paths(cwd_ref, scoped_paths));
        let bundled_message = bundle_prd_commit_comments(message.as_str(), &commit_notes);
        let identity = body_commit_identity(body);
        let cr = git_call_argv(&as_argv(&git_commit_argv(&bundled_message, false, false, scoped_paths, identity.as_ref())), cwd_ref);
        if cr.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0) != 0 {
            let serr = cr.get("stderr").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let sout = cr.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if git_commit_failed_for_missing_identity(&sout, &serr) {
                return err("git_finalize", MISSING_COMMIT_IDENTITY);
            }
        }
        let ccode = cr.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        if ccode != 0 {
            let serr = cr.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            let sout = cr.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
            if !git_commit_found_nothing_staged(sout, serr, cwd_ref) {
                return err(
                    "git_finalize",
                    &format!(
                        "commit failed: {}",
                        if serr.is_empty() { sout } else { serr }
                    ),
                );
            }
        } else {
            let head_after = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
            if head_after.is_empty() || head_after == head_before_any_commit {
                return err("git_finalize", "commit reported success (exit 0) but HEAD did not move -- refusing to claim committed:true without a real new sha");
            }
            crate::orchestrator::prd::drain_commit_comments(cwd_ref, &commit_notes);
            committed = true;
            sha = head_after[..head_after.len().min(10)].to_string();
            summary = message.lines().next().unwrap_or("").to_string();
            emit_event(
                "git.commit",
                json!({ "sub": "git", "sha_full": head_after, "sha": sha, "summary": summary, "repo": repo }),
            );
            record_commit_in_liqology(&summary, &head_after);
            let mut commit_step = json!({ "step": "commit", "sha": sha, "summary": summary });
            if !ignored.is_empty() {
                commit_step["force_added_ignored_paths"] = json!(ignored);
            }
            steps.push(commit_step);
        }
    } else if !scoped {
        let flush_notes = crate::orchestrator::prd::pending_commit_comments_for_paths(cwd_ref, &[PRD_STATE_PATHSPEC.to_string()]);
        if !flush_notes.is_empty() {
            let flush_message = if message.is_empty() {
                "chore: flush resolved PRD notes".to_string()
            } else {
                message.clone()
            };
            let bundled_message = bundle_prd_commit_comments(flush_message.as_str(), &flush_notes);
            let bundled_summary = bundled_message.lines().next().unwrap_or("").to_string();
            let prd_abs = format!(
                "{}/{}",
                exec_git_in(cwd_ref, "rev-parse --show-toplevel").trim(),
                PRD_STATE_PATHSPEC
            );
            let flush_paths: Vec<String> = if crate::pkfs::exists(&prd_abs) {
                vec![PRD_STATE_PATHSPEC.to_string()]
            } else {
                Vec::new()
            };
            let flush_stage = if flush_paths.is_empty() {
                json!({ "exit_code": 0, "stdout": "", "stderr": "" })
            } else {
                git_call_argv(
                    &as_argv(&git_stage_argv_forced(&flush_paths, cwd_ref)),
                    cwd_ref,
                )
            };
            write_steps.push(flush_stage.clone());
            let flush_code = flush_stage.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
            if flush_code != 0 {
                let flush_stderr = flush_stage.get("stderr").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                let flush_stdout = flush_stage.get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                return err_json(
                    "git_finalize",
                    json!({
                        "error": format!("git_finalize failed at step 'git add' while flushing resolved PRD notes (exit code {}): {}", flush_code, if flush_stderr.is_empty() { flush_stdout.clone() } else { flush_stderr.clone() }),
                        "error_code": ERR_CODE_FAILED,
                        "failed_step": "git_add",
                        "step_stderr": flush_stderr,
                        "step_stdout": flush_stdout,
                    }),
                );
            }
            let mut commit_argv = vec![
                "commit".to_string(),
                "--allow-empty".to_string(),
                "-m".to_string(),
                bundled_message,
                "--".to_string(),
            ];
            commit_argv.extend(git_pathspec_scope(&[], cwd_ref));
            let commit_argv: Vec<&str> = commit_argv.iter().map(String::as_str).collect();
            let cr = git_call_argv(&commit_argv, cwd_ref);
            let ccode = cr.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
            let head_after = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
            if ccode == 0 && !head_after.is_empty() && head_after != head_before_any_commit {
                crate::orchestrator::prd::drain_commit_comments(cwd_ref, &flush_notes);
                committed = true;
                sha = head_after[..head_after.len().min(10)].to_string();
                summary = bundled_summary;
                emit_event(
                    "git.commit",
                    json!({ "sub": "git", "sha_full": head_after, "sha": sha, "summary": summary, "repo": repo, "flushed_pending_prd_notes": true }),
                );
                record_commit_in_liqology(&summary, &head_after);
                steps.push(json!({ "step": "commit", "sha": sha, "summary": summary, "flushed_pending_prd_notes": true }));
            }
        }
    }

    if !committed {
        if scoped {
            let unmatched = pathspecs_matching_nothing(cwd_ref, &paths);
            if !unmatched.is_empty() && unmatched.len() == paths.len() {
                return err_json(
                    "git_finalize",
                    pathspec_matches_nothing_refusal("git_finalize", &paths, &unmatched),
                );
            }
            let ahead_probe = git_call("rev-list --count @{u}..HEAD", cwd_ref);
            let ahead_n: u64 = ahead_probe
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .trim()
                .parse()
                .unwrap_or(0);
            return err_json(
                "git_finalize",
                json!({
                    "error": format!("no commit was produced for the requested pathspec(s): {} -- refusing to push, because pushing here would publish an unrelated commit under this message", paths.join(", ")),
                    "error_code": "nothing_to_commit_for_paths",
                    "requested_paths": paths,
                    "committed": false,
                    "pushed": false,
                    "unpushed_commits_ahead_of_upstream": ahead_n,
                    "next_dispatch": "git_commit",
                }),
            );
        }
        let ahead_result = git_call("rev-list --count @{u}..HEAD", cwd_ref);
        let ahead_code = ahead_result
            .get("exit_code")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        let ahead_stderr = ahead_result
            .get("stderr")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let no_upstream = ahead_code != 0
            && (ahead_stderr.contains("no upstream")
                || ahead_stderr.contains("unknown revision")
                || ahead_stderr.contains("@{u}"));
        let ahead_n: u64 = ahead_result
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("0")
            .trim()
            .parse()
            .unwrap_or(0);
        if !dirty && !no_upstream && ahead_n == 0 {
            let (ci_status_summary, ci_validated_written) =
                check_ci_status_and_write_validated_marker_if_green(
                    cwd_ref,
                    &head_before_any_commit,
                );
            return ok(
                "git_finalize",
                with_exclusion_report(
                    json!({
                        "nothing_to_commit": true,
                        "committed": false,
                        "pushed": false,
                        "steps": [{"step": "commit", "nothing_to_commit": true}, {"step": "ci-status-check", "result": ci_status_summary, "ci_validated_marker_written": ci_validated_written}],
                        "ci_validated_marker_written": ci_validated_written,
                        "next_dispatch": if ci_validated_written { "instruction" } else { "ci-status" },
                    }),
                    cwd_ref,
                    &paths,
                ),
            );
        }
        sha = head_before_any_commit[..head_before_any_commit.len().min(10)].to_string();
        summary = exec_git_in(cwd_ref, "log -1 --pretty=%s")
            .trim()
            .to_string();
        steps.push(json!({
            "step": "commit",
            "nothing_new_to_commit": true,
            "already_ahead_of_upstream": true,
            "sha": sha,
            "summary": summary,
            "no_upstream": no_upstream,
        }));
    }

    let mut leftover = match checked_git_porcelain_scoped("git_finalize", cwd_ref, &paths) {
        Ok(porcelain) => porcelain,
        Err(refusal) => return refusal,
    };
    let dirty_only_from_concurrent_writer_on_just_committed_files = committed
        && !leftover.trim().is_empty()
        && porcelain_dirty_paths_all_within_committed_set(&leftover, &files_in_commit(cwd_ref));
    if dirty_only_from_concurrent_writer_on_just_committed_files {
        leftover = match checked_git_porcelain_scoped("git_finalize", cwd_ref, &paths) {
            Ok(porcelain) => porcelain,
            Err(refusal) => return refusal,
        };
        if !leftover.trim().is_empty()
            && porcelain_dirty_paths_all_within_committed_set(&leftover, &files_in_commit(cwd_ref))
        {
            let amend_stage = git_call_argv(&as_argv(&git_stage_argv(&paths, cwd_ref)), cwd_ref);
            write_steps.push(amend_stage.clone());
            let mut amend: Vec<String> = vec![
                "commit".to_string(),
                "--amend".to_string(),
                "--no-edit".to_string(),
            ];
            if scoped {
                amend.push("--".to_string());
                amend.extend(git_pathspec_scope(&paths, cwd_ref));
            }
            let amend_argv: Vec<&str> = amend.iter().map(String::as_str).collect();
            let amend = git_call_argv(&amend_argv, cwd_ref);
            if amend.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(1) == 0 {
                let head_after = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
                sha = head_after[..head_after.len().min(10)].to_string();
                steps.push(json!({
                    "step": "absorb_concurrent_write",
                    "sha": sha,
                    "note": "a file this dispatch committed was rewritten by a concurrent writer before the porcelain probe; amended rather than refusing the push",
                }));
            }
            leftover = match checked_git_porcelain_scoped("git_finalize", cwd_ref, &paths) {
                Ok(porcelain) => porcelain,
                Err(refusal) => return refusal,
            };
        }
    }
    if !leftover.trim().is_empty() {
        let head_sha = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
        let branch_now = exec_git_in(cwd_ref, "rev-parse --abbrev-ref HEAD").trim().to_string();
        let branch_now = if branch_now.is_empty() || branch_now == "HEAD" {
            "main".to_string()
        } else {
            branch_now
        };
        let scope = push_dirty_scope(cwd_ref, &branch_now, &head_sha, &leftover);
        if scope.blocks() {
            return err(
                "git_finalize",
                &format!(
                    "worktree still dirty after commit (untriaged residual) -- refusing push because the {} dirty path(s) are not separable from what this push carries (delta scoped against origin/{}: {}, fast-forward: {}, overlapping: [{}]). Porcelain:\n{}",
                    scope.dirty_count,
                    branch_now,
                    scope.delta_known,
                    scope.fast_forward,
                    scope.overlapping.join(", "),
                    leftover.lines().take(8).collect::<Vec<_>>().join("\n")
                ),
            );
        }
    }

    // Name the commit finalize is publishing: finalize either just created it or is republishing
    // the tip it found, and in both cases the push has to say which commit it means rather than
    // defaulting to "whatever HEAD is" -- which is how one lane came to publish another lane's
    // unpushed commits.
    let push_body = {
        let mut b = body.clone();
        if let Some(m) = b.as_object_mut() {
            m.insert(
                "rev".to_string(),
                json!(exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string()),
            );
        }
        b
    };
    let push_resp_packed = git_push(&push_body);
    let mut push_resp = unpack_to_value(push_resp_packed);
    let mut pushed = push_resp
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut recovery: Option<Value> = None;
    if !pushed {
        match pull_past_remote_moved(cwd_ref, &push_resp, body_pull_identity(body).as_ref()) {
            RemoteMovedPull::NotApplicable => {}
            RemoteMovedPull::Blocked(reason) => {
                recovery = Some(json!({
                    "attempted": false,
                    "skipped_reason": reason,
                }));
            }
            RemoteMovedPull::Failed(result) => {
                recovery = Some(json!({
                    "attempted": true,
                    "pull_result": result,
                }));
            }
            RemoteMovedPull::Landed {
                ff_only,
                result,
                incoming_commits,
                remote_sha_before,
            } => {
                let mut retry_body = push_body.clone();
                if let Some(map) = retry_body.as_object_mut() {
                    map.insert(
                        "rev".to_string(),
                        json!(exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string()),
                    );
                }
                let retry_resp = unpack_to_value(git_push(&retry_body));
                let retry_pushed = retry_resp
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                steps.push(json!({
                    "step": "auto-recover-remote-moved",
                    "pull_via": if ff_only { "ff_only" } else { "merge" },
                    "incoming_commits": incoming_commits.clone(),
                    "remote_sha_before": remote_sha_before.clone(),
                    "repushed": retry_pushed,
                }));
                recovery = Some(json!({
                    "attempted": true,
                    "pull_via": if ff_only { "ff_only" } else { "merge" },
                    "pull_result": result,
                    "worktree_dirty": push_resp.get("preserved_dirty_worktree").cloned().unwrap_or(Value::Null),
                    "incoming_commits": incoming_commits,
                    "remote_sha_before": remote_sha_before,
                    "repushed": retry_pushed,
                }));
                if retry_pushed {
                    push_resp = retry_resp;
                    pushed = true;
                }
            }
        }
    }
    if !pushed {
        let mut refusal = json!({
            "ok": false,
            "verb": "git_finalize",
            "committed": committed,
            "pushed": false,
            "sha": sha,
            "steps": steps,
            "push_result": push_resp,
            "reason": "commit landed (or nothing to commit) but push was refused -- read push_result.reason",
            "next_dispatch": "instruction",
        });
        if let Some(recovery) = recovery {
            refusal["auto_recovery"] = recovery;
            refusal["auto_recovered"] = json!(false);
        }
        apply_push_disabled_reason(&mut refusal, &push_resp);
        hoist_identity_required(&mut refusal);
        return pack(refusal.to_string());
    }
    emit_event("git.push", json!({ "repo": repo, "sha": sha }));
    let push_data = push_resp.get("data");
    let branch = push_data
        .and_then(|d| d.get("branch"))
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let remote_advanced = push_data
        .and_then(|d| d.get("remote_advanced"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let already_current = push_data
        .and_then(|d| d.get("already_current"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let remote_sha = push_data
        .and_then(|d| d.get("remote_sha"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    steps.push(json!({ "step": "push", "branch": branch, "remote_advanced": remote_advanced, "already_current": already_current }));

    let head_sha = exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string();
    let (ci_status_summary, ci_validated_written) =
        check_ci_status_and_write_validated_marker_if_green(repo.as_deref(), &head_sha);
    steps.push(json!({ "step": "ci-status-check", "result": ci_status_summary, "ci_validated_marker_written": ci_validated_written }));

    let mut finalize_payload = json!({
        "committed": committed,
        "pushed": true,
        "sha": sha,
        "summary": summary,
        "branch": branch,
        "remote_advanced": remote_advanced,
        "already_current": already_current,
        "remote_sha": remote_sha,
        "pushed_commits": push_data.and_then(|d| d.get("pushed_commits")).cloned().unwrap_or(Value::Null),
        "steps": steps,
        "ssh_fallback": push_data.and_then(|d| d.get("ssh_fallback")).cloned().unwrap_or(Value::Null),
        "ci_validated_marker_written": ci_validated_written,
        "next_dispatch": if ci_validated_written { "instruction" } else { "ci-status" },
    });
    if let Some(recovery) = recovery {
        finalize_payload["incoming_commits"] = recovery
            .get("incoming_commits")
            .cloned()
            .unwrap_or(Value::Null);
        finalize_payload["auto_recovery"] = recovery;
        finalize_payload["auto_recovered"] = json!(true);
        finalize_payload["final_sha"] =
            json!(exec_git_in(cwd_ref, "rev-parse HEAD").trim().to_string());
    }
    if !dangling_waived.is_empty() {
        finalize_payload["dangling_waived"] = json!(dangling_waived);
    }
    if !allow_whole_index && !scoped && !staged_before.is_empty() {
        finalize_payload["whole_index_commit"] = json!(true);
        finalize_payload["staged_count"] = json!(staged_before.len());
        finalize_payload["staged_paths"] = json!(staged_before);
        finalize_payload["warning"] = json!(format!(
            "this commit took the whole index ({} path(s)) because no paths were given; pass paths to scope it or allow_whole_index: true to accept it explicitly",
            staged_before.len()
        ));
    }
    if allow_whole_index && !scoped {
        finalize_payload["blanket_opt_in"] = json!("allow_whole_index");
        finalize_payload["swept_paths"] = json!(files_in_commit(cwd_ref));
    }
    if committed {
        let reopened_rows = crate::orchestrator::prd::reopen_rows_for_changed_paths(&files_in_commit(cwd_ref));
        if !reopened_rows.is_empty() {
            finalize_payload["prd_reopened"] = json!(reopened_rows);
        }
    }
    let step_refs: Vec<&Value> = write_steps.iter().collect();
    ok(
        "git_finalize",
        with_index_lock_report(
            with_exclusion_report(finalize_payload, cwd_ref, &paths),
            &step_refs,
        ),
    )
}

pub(super) fn git_log(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_log",
        body,
        &[
            "limit", "count", "range", "ref", "rev", "path", "paths", "files",
        ],
    ) {
        return refusal;
    }
    git_async_entry("git_log", body, |body, plan| {
        let cwd = body_cwd(body);
        let count = body
            .get("limit")
            .and_then(|v| v.as_u64())
            .or_else(|| body.get("count").and_then(|v| v.as_u64()))
            .unwrap_or(10);
        let nflag = format!("-{}", count);
        let range = body_revision(body);
        let paths = body_pathspecs(body);
        let pretty = "--pretty=format:%h\u{1f}%H\u{1f}%an\u{1f}%ae\u{1f}%aI\u{1f}%s";
        let mut argv: Vec<&str> = vec!["log", &nflag, pretty, "--no-color"];
        if !range.is_empty() {
            argv.push(range);
        }
        if !paths.is_empty() {
            argv.push("--");
            for p in &paths {
                argv.push(p.as_str());
            }
        }
        let r = git_step_replayed_by_call_order(plan, &argv, cwd)?;
        let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        if code != 0 {
            let stderr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            return Ok(err_json(
                "git_log",
                json!({
                    "error": stderr,
                    "range": range,
                    "hint": "Inspect the error for a Git failure or host execution limit; missing output is not an empty result."
                }),
            ));
        }
        let out = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
        let commits: Vec<Value> = out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let mut it = l.splitn(6, '\u{1f}');
                let sha = it.next().unwrap_or("").to_string();
                let sha_full = it.next().unwrap_or("").to_string();
                let author_name = it.next().unwrap_or("").to_string();
                let author_email = it.next().unwrap_or("").to_string();
                let author_date = it.next().unwrap_or("").to_string();
                let subject = it.next().unwrap_or("").to_string();
                json!({
                    "sha": sha,
                    "sha_full": sha_full,
                    "subject": subject,
                    "author": { "name": author_name, "email": author_email, "date": author_date },
                })
            })
            .collect();
        Ok(ok("git_log", json!({ "commits": commits })))
    })
}

pub(super) fn git_diff(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_diff",
        body,
        &[
            "range", "ref", "rev", "staged", "stat", "path", "paths", "files",
        ],
    ) {
        return refusal;
    }
    git_async_entry("git_diff", body, |body, plan| {
        let cwd = body_cwd(body);
        let staged = body
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let paths = body_pathspecs(body);
        let range = body_revision(body);
        let stat = body.get("stat").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut argv: Vec<&str> = vec!["diff", "--no-color"];
        if staged {
            argv.push("--staged");
        }
        if stat {
            argv.push("--stat");
        }
        if !range.is_empty() {
            argv.push(range);
        }
        if !paths.is_empty() {
            argv.push("--");
            argv.extend(paths.iter().map(String::as_str));
        }
        let r = git_step_replayed_by_call_order(plan, &argv, cwd)?;
        let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        if code != 0 {
            let stderr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
            return Ok(err_json(
                "git_diff",
                json!({
                    "error": stderr,
                    "range": range,
                    "hint": "Inspect the error for a Git failure or host execution limit; missing output is not an empty result. Narrow paths/range or use stat:true."
                }),
            ));
        }
        let mut diff = r
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let truncated = diff.len() > 60000;
        if truncated {
            let cut = (0..=60000).rev().find(|&i| diff.is_char_boundary(i)).unwrap_or(0);
            diff.truncate(cut);
        }
        Ok(ok(
            "git_diff",
            json!({ "diff": diff, "truncated": truncated, "range": range, "paths": paths }),
        ))
    })
}

pub(super) fn git_show(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_show",
        body,
        &[
            "rev", "ref", "sha", "commit", "stat", "path", "paths", "files",
        ],
    ) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let refspec = ["rev", "ref", "sha", "commit"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("HEAD");
    let stat = body.get("stat").and_then(|v| v.as_bool()).unwrap_or(false);
    let path = body.get("path").and_then(|v| v.as_str());
    let paths: Vec<String> = body
        .get("paths")
        .or_else(|| body.get("files"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let combined_ref = match path {
        Some(p) => format!("{}:{}", refspec, p),
        None => refspec.to_string(),
    };
    let mut argv: Vec<&str> = vec!["show", "--no-color"];
    if stat {
        argv.push("--stat");
    }
    argv.push(&combined_ref);
    if !paths.is_empty() {
        argv.push("--");
        for p in &paths {
            argv.push(p.as_str());
        }
    }
    let r = git_call_argv(&argv, cwd);
    let mut out = r
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if out.len() > 60000 {
        let cut = (0..=60000).rev().find(|&i| out.is_char_boundary(i)).unwrap_or(0);
        out.truncate(cut);
    }
    ok("git_show", json!({ "output": out, "rev": refspec }))
}

pub(super) fn git_fetch(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let remote = body
        .get("remote")
        .and_then(|v| v.as_str())
        .unwrap_or("origin");
    let r = git_call_argv(&["fetch", remote], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        return err("git_fetch", &out);
    }
    ok("git_fetch", json!({ "remote": remote, "output": out }))
}

pub(super) fn classify_pull_hang_phase(output: &str) -> &'static str {
    let low = output.to_lowercase();
    if low.contains("hook") {
        "a post-merge/post-checkout hook"
    } else if low.contains("auto packing")
        || low.contains("garbage collect")
        || low.contains(" gc ")
    {
        "auto-gc"
    } else if low.contains("username for")
        || low.contains("password for")
        || low.contains("terminal prompts disabled")
    {
        "a credential prompt"
    } else {
        "unknown -- git exited past the host's own timeout after the fetch/merge apparently completed"
    }
}

pub(super) fn body_pull_identity(body: &Value) -> Option<(String, String)> {
    let field = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| {
                body.get(*key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or("")
            .to_string()
    };
    let name = field(&["user_name", "author_name"]);
    let email = field(&["user_email", "author_email"]);
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name, email))
}

pub(super) fn git_pull(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let remote = body
        .get("remote")
        .and_then(|v| v.as_str())
        .unwrap_or("origin")
        .trim();
    let branch = body
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let ff_only = body
        .get("ff_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let mut argv: Vec<String> = Vec::new();
    if let Some((name, email)) = body_pull_identity(body) {
        argv.push("-c".to_string());
        argv.push(format!("user.name={name}"));
        argv.push("-c".to_string());
        argv.push(format!("user.email={email}"));
    }
    argv.push("pull".to_string());
    argv.push("--no-edit".to_string());
    argv.push("--no-rebase".to_string());
    if ff_only {
        argv.push("--ff-only".to_string());
    }
    if !remote.is_empty() {
        argv.push(remote.to_string());
    }
    if !branch.is_empty() {
        argv.push(branch.to_string());
    }
    let argv_refs: Vec<&str> = argv.iter().map(|arg| arg.as_str()).collect();
    let r = git_call_argv(&argv_refs, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    let conflicts: Vec<String> = exec_git_in(cwd, "diff --name-only --diff-filter=U")
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if code != 0 {
        let lower_output = output.to_ascii_lowercase();
        if lower_output.contains("committer identity unknown")
            || lower_output.contains("author identity unknown")
            || lower_output.contains("unable to auto-detect email address")
            || lower_output.contains("empty ident name")
        {
            return err_json(
                "git_pull",
                json!({
                    "error": output,
                    "error_code": "git_identity_required",
                    "remote": remote,
                    "branch": if branch.is_empty() { Value::Null } else { json!(branch) },
                    "ff_only": ff_only,
                    "conflicted": !conflicts.is_empty(),
                    "conflicts": conflicts,
                    "head_before": head_before,
                    "hint": "Git needs a commit identity for this merge. GitHub CLI authentication does not configure Git commit identity. Set this repository's user.name and user.email to your verified GitHub identity, then retry git_pull. No identity or global configuration was changed.",
                    "repo_local_config_argv": [
                        ["git", "config", "--local", "user.name", "<your GitHub name>"],
                        ["git", "config", "--local", "user.email", "<your verified GitHub email>"],
                    ],
                }),
            );
        }
        if conflicts.is_empty() {
            let target_branch = if branch.is_empty() {
                exec_git_in(cwd, "rev-parse --abbrev-ref HEAD")
                    .trim()
                    .to_string()
            } else {
                branch.to_string()
            };
            let remote_name = if remote.is_empty() { "origin" } else { remote };
            let _ = git_call_argv(&["fetch", remote_name, &target_branch], cwd);
            let remote_head = resolve_ref(cwd, &format!("{}/{}", remote_name, target_branch));
            let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
            let worktree_clean = git_porcelain_in(cwd).trim().is_empty();
            if worktree_clean
                && !head_after.is_empty()
                && remote_head.as_deref() == Some(head_after.as_str())
            {
                return ok(
                    "git_pull",
                    json!({
                        "remote": remote,
                        "branch": if branch.is_empty() { Value::Null } else { json!(branch) },
                        "ff_only": ff_only,
                        "head_before": head_before,
                        "head_after": head_after,
                        "already_up_to_date": head_before == head_after,
                        "output": output,
                        "subprocess_reported_failure_but_merge_verified_landed": true,
                        "hung_phase": classify_pull_hang_phase(&output),
                    }),
                );
            }
        }
        return err_json(
            "git_pull",
            json!({
                "error": output,
                "remote": remote,
                "branch": if branch.is_empty() { Value::Null } else { json!(branch) },
                "ff_only": ff_only,
                "conflicted": !conflicts.is_empty(),
                "conflicts": conflicts,
                "head_before": head_before,
                "hint": "resolve conflicted paths, git_add them, then git_commit; or git_merge_abort to restore the pre-pull HEAD",
            }),
        );
    }
    let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    ok(
        "git_pull",
        json!({
            "remote": remote,
            "branch": if branch.is_empty() { Value::Null } else { json!(branch) },
            "ff_only": ff_only,
            "head_before": head_before,
            "head_after": head_after,
            "already_up_to_date": head_before == head_after,
            "output": output,
        }),
    )
}

pub(super) fn ci_status_resolve_repo_preferring_unambiguous_github_repo_field(
    body: &Value,
    cwd: Option<&str>,
) -> Result<String, u64> {
    if let Some(explicit) = body
        .get("github_repo")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("repo").and_then(|v| v.as_str()))
    {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Ok(explicit.to_string());
        }
    }
    let url = exec_git_in(cwd, "remote get-url origin")
        .trim()
        .to_string();
    if url.is_empty() {
        return Err(err("ci-status", "github_repo required (pass {github_repo:\"owner/name\"} or run inside a checkout with an origin remote) -- github_repo is preferred over the also-accepted repo field, which means a filesystem cwd path on git_finalize/git_push and every other git_* verb"));
    }
    let trimmed = url.trim_end_matches(".git");
    let owner_name = trimmed.rsplit_once('/').and_then(|(rest, name)| {
        rest.rsplit_once(['/', ':'])
            .map(|(_, owner)| format!("{}/{}", owner, name))
    });
    match owner_name {
        Some(s) if s.contains('/') => Ok(s),
        _ => Err(err(
            "ci-status",
            &format!("could not parse owner/name from origin remote url: {}", url),
        )),
    }
}

pub(super) fn ci_status_resolve_sha(body: &Value, cwd: Option<&str>) -> Result<String, u64> {
    let requested = body
        .get("sha")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("ref").and_then(|v| v.as_str()))
        .unwrap_or("latest")
        .trim();
    if requested.is_empty() || requested.eq_ignore_ascii_case("latest") {
        let sha = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
        if sha.is_empty() {
            return Err(err(
                "ci-status",
                "sha not provided and unable to resolve local HEAD",
            ));
        }
        return Ok(sha);
    }
    if is_full_sha(requested) {
        return Ok(requested.to_string());
    }
    let resolved = exec_git_in(cwd, &format!("rev-parse --verify {}^{{commit}}", requested))
        .trim()
        .to_string();
    if is_full_sha(&resolved) {
        return Ok(resolved);
    }
    Err(err("ci-status", &format!(
        "{} is neither a 40-char commit sha nor a ref git can resolve here -- pass a full sha, or a ref git knows such as HEAD or a branch name",
        requested)))
}

pub(super) fn is_full_sha(candidate: &str) -> bool {
    candidate.len() == 40 && candidate.chars().all(|c| c.is_ascii_hexdigit())
}

pub(super) fn ci_status_token() -> Option<String> {
    if let Some(s) = unpack_to_string(unsafe { host_env_get(b"GITHUB_TOKEN".as_ptr(), 12) }) {
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Some(s) = unpack_to_string(unsafe { host_env_get(b"GH_TOKEN".as_ptr(), 8) }) {
        if !s.is_empty() {
            return Some(s);
        }
    }
    ci_status_gh_cli_token()
}

pub(super) fn parse_github_fixed_width_utc_timestamp_to_epoch_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[19] != b'Z' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    let is_leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in_month = |y: i64, m: i64| -> i64 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap(y) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    };
    let mut days: i64 = 0;
    if year >= 1970 {
        for y in 1970..year {
            days += if is_leap(y) { 366 } else { 365 };
        }
    } else {
        for y in year..1970 {
            days -= if is_leap(y) { 366 } else { 365 };
        }
    }
    for m in 1..month {
        days += days_in_month(year, m);
    }
    days += day - 1;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

pub(super) fn ci_status_conclusion_to_status(conclusion: &str, gh_status: &str) -> &'static str {
    if gh_status != "completed" {
        return "pending";
    }
    match conclusion {
        "success" => "success",
        "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure" => "failure",
        _ => "unknown",
    }
}

pub(super) const CI_STATUS_GH_TIMEOUT_MS: u64 = 60_000;

pub(super) const CI_STATUS_RATE_LIMIT_MAX_ATTEMPTS: u32 = 3;
pub(super) const CI_STATUS_RATE_LIMIT_BACKOFF_MS: u64 = 1_500;
pub(super) const CI_STATUS_RATE_LIMIT_MAX_WAIT_MS: u64 = 5_000;

#[derive(Clone, Default)]
pub(super) struct CiStatusRateLimit {
    reset_epoch: Option<i64>,
    remaining: Option<i64>,
    retry_after_secs: Option<i64>,
}

#[derive(Clone, Copy)]
pub(super) enum CiStatusTransport {
    GhCli,
    GithubRest,
}

impl CiStatusTransport {
    pub(super) fn label(self) -> &'static str {
        match self {
            CiStatusTransport::GhCli => "gh_cli",
            CiStatusTransport::GithubRest => "github_rest",
        }
    }
}

pub(super) struct CiStatusAttemptFailure {
    transport: CiStatusTransport,
    reason: String,
    body: String,
    attempt: u32,
    rate_limit: Option<CiStatusRateLimit>,
}

pub(super) fn ci_status_truncate_for_error(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let taken: String = trimmed.chars().take(max_chars).collect();
    if taken.chars().count() < trimmed.chars().count() {
        format!("{}...", taken)
    } else {
        taken
    }
}

pub(super) fn ci_status_shell_result(code: &str) -> Value {
    let opts = json!({ "lang": "bash", "timeoutMs": CI_STATUS_GH_TIMEOUT_MS }).to_string();
    let packed = unsafe {
        host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let raw = unpack_to_string(packed).unwrap_or_default();
    serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({ "stdout": raw }))
}

pub(super) fn ci_status_shell_stream(result: &Value, key: &str) -> String {
    result
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn ci_status_shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub(super) fn ci_status_gh_unusable_reason() -> Option<String> {
    let probe = "command -v gh >/dev/null 2>&1 || { printf 'gh_not_installed\\n'; exit 0; }\n\
                 gh auth status >/dev/null 2>&1 && { printf 'gh_ready\\n'; exit 0; }\n\
                 printf 'gh_unauthenticated\\n'\n\
                 gh auth status 2>&1 | head -n 6";
    let result = ci_status_shell_result(probe);
    let stdout = ci_status_shell_stdout(&result);
    match stdout.lines().next().unwrap_or("").trim() {
        "gh_ready" => None,
        "gh_not_installed" => Some(
            "gh CLI is not on PATH -- install it and run `gh auth login`, or supply GITHUB_TOKEN/GH_TOKEN"
                .to_string(),
        ),
        "gh_unauthenticated" => Some(format!(
            "gh CLI is installed but not authenticated -- run `gh auth login`, or supply GITHUB_TOKEN/GH_TOKEN ({})",
            ci_status_truncate_for_error(&stdout, 300)
        )),
        _ => Some(format!(
            "gh CLI probe returned no usable answer ({})",
            ci_status_truncate_for_error(&stdout, 300)
        )),
    }
}

pub(super) fn ci_status_shell_stdout(result: &Value) -> String {
    ci_status_shell_stream(result, "stdout")
}

pub(super) fn ci_status_gh_cli_token() -> Option<String> {
    let token = ci_status_shell_stdout(&ci_status_shell_result("gh auth token 2>/dev/null"))
        .trim()
        .to_string();
    let token_shaped = !token.is_empty()
        && token.len() <= 256
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if token_shaped {
        Some(token)
    } else {
        None
    }
}

pub(super) fn ci_status_header_value(resp: &Value, name: &str) -> Option<String> {
    let wanted = name.to_ascii_lowercase();
    if let Some(map) = resp.get("headers").and_then(Value::as_object) {
        for (key, value) in map.iter() {
            if key.to_ascii_lowercase() != wanted {
                continue;
            }
            if let Some(s) = value.as_str() {
                return Some(s.trim().to_string());
            }
            if let Some(n) = value.as_i64() {
                return Some(n.to_string());
            }
        }
    }
    for entry in resp
        .get("headers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        if pair.len() < 2 {
            continue;
        }
        if pair[0].as_str().map(|k| k.to_ascii_lowercase()) != Some(wanted.clone()) {
            continue;
        }
        if let Some(s) = pair[1].as_str() {
            return Some(s.trim().to_string());
        }
        if let Some(n) = pair[1].as_i64() {
            return Some(n.to_string());
        }
    }
    None
}

pub(super) fn ci_status_reply_is_rate_limited(
    status: i64,
    rate: &CiStatusRateLimit,
    body: &str,
) -> bool {
    if status != 403 && status != 429 {
        return false;
    }
    if rate.remaining == Some(0) {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("rate limit")
        || lower.contains("abuse detection")
        || lower.contains("secondary rate")
}

pub(super) fn ci_status_rate_limit_from_reply(
    resp: &Value,
    status: i64,
    body: &str,
) -> Option<CiStatusRateLimit> {
    let rate = CiStatusRateLimit {
        reset_epoch: ci_status_header_value(resp, "x-ratelimit-reset")
            .and_then(|s| s.parse::<i64>().ok()),
        remaining: ci_status_header_value(resp, "x-ratelimit-remaining")
            .and_then(|s| s.parse::<i64>().ok()),
        retry_after_secs: ci_status_header_value(resp, "retry-after")
            .and_then(|s| s.parse::<i64>().ok()),
    };
    if ci_status_reply_is_rate_limited(status, &rate, body) {
        Some(rate)
    } else {
        None
    }
}

pub(super) fn ci_status_gh_rate_limit(reason: &str) -> Option<CiStatusRateLimit> {
    let lower = reason.to_ascii_lowercase();
    if !(lower.contains("rate limit")
        || lower.contains("abuse detection")
        || lower.contains("secondary rate"))
    {
        return None;
    }
    Some(CiStatusRateLimit {
        reset_epoch: ci_status_gh_rate_limit_reset_epoch(),
        remaining: None,
        retry_after_secs: None,
    })
}

pub(super) fn ci_status_gh_rate_limit_reset_epoch() -> Option<i64> {
    ci_status_gh_api_json("rate_limit")
        .ok()?
        .get("resources")
        .and_then(|r| r.get("core"))
        .and_then(|c| c.get("reset"))
        .and_then(Value::as_i64)
}

pub(super) fn ci_status_rate_limit_wait_ms(rate: &CiStatusRateLimit) -> Option<u64> {
    let now = unsafe { host_now_ms() } as i64 / 1000;
    if let Some(secs) = rate.retry_after_secs.filter(|s| *s > 0) {
        let wait_ms = secs.saturating_mul(1000);
        if wait_ms <= CI_STATUS_RATE_LIMIT_MAX_WAIT_MS as i64 {
            return Some(wait_ms as u64);
        }
        return None;
    }
    match rate.reset_epoch {
        None => Some(CI_STATUS_RATE_LIMIT_BACKOFF_MS),
        Some(reset) => {
            let delta_ms = (reset - now).saturating_mul(1000);
            if delta_ms <= 0 {
                Some(CI_STATUS_RATE_LIMIT_BACKOFF_MS)
            } else if delta_ms <= CI_STATUS_RATE_LIMIT_MAX_WAIT_MS as i64 {
                Some(delta_ms as u64)
            } else {
                None
            }
        }
    }
}

pub(super) fn ci_status_epoch_to_utc_iso(epoch: i64) -> String {
    let is_leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in_month = |y: i64, m: i64| -> i64 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap(y) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    };
    let mut rest = epoch.max(0);
    let mut year = 1970i64;
    loop {
        let year_secs = if is_leap(year) { 366 } else { 365 } * 86400;
        if rest < year_secs {
            break;
        }
        rest -= year_secs;
        year += 1;
    }
    let mut month = 1i64;
    loop {
        let month_secs = days_in_month(year, month) * 86400;
        if rest < month_secs {
            break;
        }
        rest -= month_secs;
        month += 1;
    }
    let day = rest / 86400 + 1;
    rest -= (day - 1) * 86400;
    let hour = rest / 3600;
    rest -= hour * 3600;
    let min = rest / 60;
    let sec = rest - min * 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

pub(super) fn ci_status_gh_api_json(path: &str) -> Result<Value, String> {
    let code = format!(
        "gh api -H 'Accept: application/vnd.github+json' {}",
        ci_status_shell_quote(path)
    );
    let result = ci_status_shell_result(&code);
    if result.get("timed_out").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "gh api {} timed out after {} ms",
            path, CI_STATUS_GH_TIMEOUT_MS
        ));
    }
    let stdout = ci_status_shell_stdout(&result);
    let stderr = ci_status_shell_stream(&result, "stderr");
    let detail_source = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let detail = ci_status_truncate_for_error(detail_source, 300);
    match (
        result.get("exit_code").and_then(Value::as_i64),
        serde_json::from_str::<Value>(stdout.trim()).ok(),
    ) {
        (Some(0), Some(parsed)) => Ok(parsed),
        (None, Some(parsed)) => Ok(parsed),
        (Some(code), _) => Err(format!("gh api {} exited {}: {}", path, code, detail)),
        (None, None) => Err(format!("gh api {} produced no JSON: {}", path, detail)),
    }
}

pub(super) fn ci_status_rest_api_json(
    path: &str,
    opts: &str,
) -> Result<Value, (i64, String, Option<CiStatusRateLimit>)> {
    let url = format!("https://api.github.com/{}", path);
    if let Err(reason) = crate::config_path::validate_fetch_url(&url) {
        return Err((0, reason, None));
    }
    let packed = unsafe {
        host_fetch(
            url.as_ptr(),
            url.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let resp = unpack_to_value(packed);
    if resp.is_null() {
        return Err((0, "host_fetch returned no response".to_string(), None));
    }
    let body_text = resp
        .get("body")
        .and_then(Value::as_str)
        .or_else(|| resp.get("text").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let status_code = resp
        .get("status")
        .and_then(Value::as_i64)
        .or_else(|| resp.get("statusCode").and_then(Value::as_i64))
        .unwrap_or(0);
    if status_code != 200 {
        let rate_limit = ci_status_rate_limit_from_reply(&resp, status_code, &body_text);
        return Err((status_code, body_text, rate_limit));
    }
    serde_json::from_str::<Value>(&body_text).map_err(|parse_error| {
        (
            status_code,
            format!("{} -- {}", parse_error, body_text),
            None,
        )
    })
}

pub(super) fn ci_status_attempt(
    transport: CiStatusTransport,
    path: &str,
    rest_opts: &str,
    attempt: u32,
) -> Result<Value, CiStatusAttemptFailure> {
    match transport {
        CiStatusTransport::GhCli => ci_status_gh_api_json(path).map_err(|reason| {
            let rate_limit = ci_status_gh_rate_limit(&reason);
            CiStatusAttemptFailure {
                transport,
                reason,
                body: String::new(),
                attempt,
                rate_limit,
            }
        }),
        CiStatusTransport::GithubRest => {
            ci_status_rest_api_json(path, rest_opts).map_err(|(status, body, rate_limit)| {
                let reason = if status > 0 {
                    format!("HTTP {} {}", status, ci_status_truncate_for_error(&body, 300))
                } else {
                    ci_status_truncate_for_error(&body, 300)
                };
                CiStatusAttemptFailure {
                    transport,
                    reason,
                    body,
                    attempt,
                    rate_limit,
                }
            })
        }
    }
}

pub(super) fn ci_status_runs_from(parsed: &Value) -> Vec<Value> {
    parsed
        .get("workflow_runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn ci_status_value(body: &Value) -> Result<Value, Value> {
    let cwd = body_cwd(body);
    let repo = ci_status_resolve_repo_preferring_unambiguous_github_repo_field(body, cwd)
        .map_err(|packed| unpack_to_value(packed))?;
    let sha = ci_status_resolve_sha(body, cwd).map_err(|packed| unpack_to_value(packed))?;
    let runs_path = format!("repos/{}/actions/runs?head_sha={}&per_page=20", repo, sha);
    let tree_path = format!("repos/{}/git/trees/{}?recursive=1", repo, sha);
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(ci_status_token);
    let mut headers = json!({
        "Accept": "application/vnd.github+json",
        "User-Agent": "plugkit-ci-status",
    });
    if let Some(t) = &token {
        headers["Authorization"] = json!(format!("Bearer {}", t));
    }
    let rest_opts = json!({ "timeoutMs": FETCH_DEFAULT_TIMEOUT_MS, "headers": headers }).to_string();
    let mut ordered_transports: Vec<CiStatusTransport> = Vec::new();
    let mut failures: Vec<CiStatusAttemptFailure> = Vec::new();
    match ci_status_gh_unusable_reason() {
        None => ordered_transports.push(CiStatusTransport::GhCli),
        Some(reason) => {
            let rate_limit = ci_status_gh_rate_limit(&reason);
            failures.push(CiStatusAttemptFailure {
                transport: CiStatusTransport::GhCli,
                reason,
                body: String::new(),
                attempt: 0,
                rate_limit,
            })
        }
    }
    ordered_transports.push(CiStatusTransport::GithubRest);
    for transport in ordered_transports {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match ci_status_attempt(transport, &runs_path, &rest_opts, attempt) {
                Ok(parsed) => {
                    let runs = ci_status_runs_from(&parsed);
                    return Ok(ci_status_summarize(
                        body,
                        &repo,
                        &sha,
                        &runs,
                        transport,
                        &tree_path,
                        &rest_opts,
                    ));
                }
                Err(failure) => {
                    let wait_ms = failure
                        .rate_limit
                        .as_ref()
                        .and_then(ci_status_rate_limit_wait_ms);
                    let retryable =
                        attempt < CI_STATUS_RATE_LIMIT_MAX_ATTEMPTS && wait_ms.is_some();
                    failures.push(failure);
                    if !retryable {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(
                        wait_ms.unwrap_or(CI_STATUS_RATE_LIMIT_BACKOFF_MS),
                    ));
                }
            }
        }
    }
    if let Some(rate) = failures
        .iter()
        .rev()
        .find_map(|f| f.rate_limit.as_ref().cloned())
    {
        let reset_epoch = rate
            .reset_epoch
            .or_else(ci_status_gh_rate_limit_reset_epoch);
        let now_secs = unsafe { host_now_ms() } as i64 / 1000;
        let seconds_until_reset = reset_epoch.map(|r| (r - now_secs).max(0));
        let reason = match (reset_epoch, seconds_until_reset) {
            (Some(_), Some(secs)) => format!(
                "GitHub API rate limit reached for this host; quota resets at {} ({}s from now)",
                ci_status_epoch_to_utc_iso(reset_epoch.unwrap_or(0)),
                secs
            ),
            _ => "GitHub API rate limit reached for this host; no reset time was published"
                .to_string(),
        };
        let last_transport = failures
            .last()
            .map(|f| f.transport.label())
            .unwrap_or("github_rest");
        return Ok(json!({
            "ok": true, "verb": "ci-status",
            "data": {
                "status": "unknown",
                "repo": repo, "sha": sha,
                "failed_jobs": [],
                "run_url": Value::Null,
                "reason": reason,
                "rate_limited": true,
                "rate_limit_reset_epoch": reset_epoch,
                "rate_limit_reset_at": reset_epoch.map(ci_status_epoch_to_utc_iso),
                "retry_after_secs": rate.retry_after_secs.or(seconds_until_reset),
                "transport": last_transport,
                "attempts": failures
                    .iter()
                    .map(|f| json!({
                        "transport": f.transport.label(),
                        "attempt": f.attempt,
                        "reason": f.reason,
                        "rate_limited": f.rate_limit.is_some(),
                    }))
                    .collect::<Vec<_>>(),
            },
        }));
    }
    Err(json!({
        "ok": false, "verb": "ci-status",
        "error": format!(
            "GitHub Actions state unreadable through every transport: {}",
            failures
                .iter()
                .map(|f| format!("{}: {}", f.transport.label(), f.reason))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        "repo": repo, "sha": sha,
        "response": failures.last().map(|f| f.body.clone()).unwrap_or_default(),
        "attempts": failures
            .iter()
            .map(|f| json!({ "transport": f.transport.label(), "reason": f.reason }))
            .collect::<Vec<_>>(),
    }))
}

pub(super) fn ci_status_summarize(
    body: &Value,
    repo: &str,
    sha: &str,
    runs: &[Value],
    transport: CiStatusTransport,
    tree_path: &str,
    rest_opts: &str,
) -> Value {
    let source = transport.label();
    if runs.is_empty() {
        let tree_result = ci_status_attempt(transport, tree_path, rest_opts, 1);
        let tree = tree_result.as_ref().ok().cloned().unwrap_or(Value::Null);
        let complete_tree = tree.get("truncated").and_then(Value::as_bool) == Some(false);
        if let Some(entries) = tree
            .get("tree")
            .and_then(Value::as_array)
            .filter(|_| complete_tree)
        {
            let paths: Option<Vec<&str>> = entries
                .iter()
                .map(|entry| {
                    entry
                        .get("path")
                        .and_then(Value::as_str)
                        .filter(|path| !path.is_empty())
                })
                .collect();
            if let Some(paths) = paths {
                let workflow_paths: Vec<&str> = paths
                    .into_iter()
                    .filter(|path| {
                        path.strip_prefix(".github/workflows/").is_some_and(|file| {
                            !file.contains('/')
                                && (file.ends_with(".yml") || file.ends_with(".yaml"))
                        })
                    })
                    .collect();
                if workflow_paths.is_empty() {
                    return json!({
                        "ok": true, "verb": "ci-status", "data": {
                            "status": "no_applicable_workflow", "repo": repo, "sha": sha,
                            "failed_jobs": [], "run_url": Value::Null,
                            "query": format!("head_sha={}", sha),
                            "reason": "a complete GitHub git tree for this exact sha contains no .github/workflows YAML files",
                            "workflow_configuration": "absent",
                            "evidence": {"source": "github_git_tree", "requested_sha": sha, "tree_sha": tree.get("sha"), "truncated": false, "entries": entries.len()},
                            "transport": source,
                        }
                    });
                }
                return json!({
                    "ok": true, "verb": "ci-status", "data": {
                        "status": "unknown", "repo": repo, "sha": sha,
                        "failed_jobs": [], "run_url": Value::Null,
                        "reason": "no workflow runs found for this sha; workflow definitions are present",
                        "workflow_paths": workflow_paths,
                        "transport": source,
                    }
                });
            }
        }
        return json!({
            "ok": true, "verb": "ci-status", "data": {
                "status": "unknown", "repo": repo, "sha": sha,
                "failed_jobs": [], "run_url": Value::Null,
                "reason": "no workflow runs found for this sha; absence of workflow definitions could not be proven from a complete GitHub git tree",
                "workflow_tree_error": tree_result.err().map(|f| f.reason).unwrap_or_default(),
                "transport": source,
            }
        });
    }
    const STALE_IN_PROGRESS_RUN_GRACE_SECS: i64 = 900;
    let default_ignored_workflows_orthogonal_to_code_under_test = ["Deploy GH Pages"];
    let ignored_names: Vec<String> = body
        .get("ignore_workflows")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            default_ignored_workflows_orthogonal_to_code_under_test
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let now_secs = unsafe { host_now_ms() } as i64 / 1000;
    let mut overall = "success";
    let mut failed_jobs: Vec<Value> = vec![];
    let mut run_url: Option<String> = None;
    let mut any_pending = false;
    let mut any_failure = false;
    let mut counted_runs = 0usize;
    let mut jobs: Vec<Value> = vec![];
    let mut jobs_unavailable: Vec<Value> = vec![];
    for run in runs {
        let run_name = run.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if ignored_names.iter().any(|n| n == run_name) {
            continue;
        }
        counted_runs += 1;
        ci_status_collect_run_jobs(transport, repo, run, rest_opts, &mut jobs, &mut jobs_unavailable);
        let gh_status = run.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let conclusion = run.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        let mut per_run_status = ci_status_conclusion_to_status(conclusion, gh_status);
        let html_url = run
            .get("html_url")
            .and_then(|v| v.as_str())
            .map(String::from);
        if run_url.is_none() {
            run_url = html_url.clone();
        }
        let mut stale = false;
        if per_run_status == "pending" {
            let updated_epoch = run
                .get("updated_at")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_github_fixed_width_utc_timestamp_to_epoch_secs(s));
            if let Some(updated) = updated_epoch {
                if now_secs.saturating_sub(updated) >= STALE_IN_PROGRESS_RUN_GRACE_SECS {
                    per_run_status = "failure";
                    stale = true;
                }
            }
        }
        if per_run_status == "pending" {
            any_pending = true;
        }
        if per_run_status == "failure" {
            any_failure = true;
            failed_jobs.push(json!({
                "name": run.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "conclusion": if stale { "stale_in_progress" } else { conclusion },
                "run_url": html_url,
            }));
        }
    }
    if any_failure {
        overall = "failure";
    } else if any_pending {
        overall = "pending";
    }
    json!({
        "ok": true, "verb": "ci-status", "data": {
            "status": overall, "repo": repo, "sha": sha,
            "failed_jobs": failed_jobs, "run_url": run_url, "run_count": counted_runs,
            "jobs": jobs, "jobs_unavailable": jobs_unavailable,
            "transport": source,
        },
    })
}

pub(super) fn ci_status_collect_run_jobs(
    transport: CiStatusTransport,
    repo: &str,
    run: &Value,
    rest_opts: &str,
    jobs: &mut Vec<Value>,
    jobs_unavailable: &mut Vec<Value>,
) {
    let workflow = run.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(run_id) = run.get("id").and_then(Value::as_u64) else {
        jobs_unavailable.push(json!({ "workflow": workflow, "reason": "workflow run carries no id" }));
        return;
    };
    let path = format!("repos/{}/actions/runs/{}/jobs?per_page=100", repo, run_id);
    match ci_status_attempt(transport, &path, rest_opts, 1) {
        Ok(parsed) => {
            let total = parsed.get("total_count").and_then(Value::as_u64).unwrap_or(0);
            match parsed.get("jobs").and_then(Value::as_array) {
                Some(listed) if total <= listed.len() as u64 => jobs.extend(
                    listed
                        .iter()
                        .map(|job| ci_status_job_entry(workflow, run_id, job)),
                ),
                Some(listed) => jobs_unavailable.push(json!({
                    "workflow": workflow,
                    "run_id": run_id,
                    "reason": format!("job list truncated: {} of {} jobs listed", listed.len(), total),
                })),
                None => jobs_unavailable.push(json!({
                    "workflow": workflow,
                    "run_id": run_id,
                    "reason": "jobs response carries no jobs array",
                })),
            }
        }
        Err(failure) => jobs_unavailable.push(json!({
            "workflow": workflow,
            "run_id": run_id,
            "reason": failure.reason,
        })),
    }
}

pub(super) fn ci_status_job_entry(workflow: &str, run_id: u64, job: &Value) -> Value {
    let status = job.get("status").and_then(Value::as_str).unwrap_or("");
    let github_conclusion = job.get("conclusion").and_then(Value::as_str);
    let conclusion = match (status, github_conclusion) {
        ("completed", Some("success" | "neutral")) => "success",
        ("completed", Some("skipped")) => "skipped",
        ("completed", _) => "failure",
        _ => "pending",
    };
    json!({
        "workflow": workflow,
        "name": job.get("name").and_then(Value::as_str).unwrap_or(""),
        "conclusion": conclusion,
        "github_status": status,
        "github_conclusion": github_conclusion,
        "run_id": run_id,
        "job_url": job.get("html_url").and_then(Value::as_str),
    })
}

pub(super) fn ci_status(body: &Value) -> u64 {
    match ci_status_value(body) {
        Ok(v) => pack(v.to_string()),
        Err(v) => pack(v.to_string()),
    }
}

pub(super) fn git_branch(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_branch", body, &["remote", "all"]) {
        return refusal;
    }
    for flag in ["remote", "all"] {
        match body.get(flag) {
            None | Some(Value::Null) | Some(Value::Bool(_)) => {}
            Some(_) => {
                return err_json(
                    "git_branch",
                    json!({
                        "error": format!("git_branch: {} must be a boolean", flag),
                        "invalid_fields": [flag],
                    }),
                );
            }
        }
    }
    let requested = |key: &str| body.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let listing_args = if requested("all") {
        "branch --no-color -a"
    } else if requested("remote") {
        "branch --no-color -r"
    } else {
        "branch --no-color"
    };
    let cwd = body_cwd(body);
    let current = exec_git_in(cwd, "rev-parse --abbrev-ref HEAD")
        .trim()
        .to_string();
    let listing = exec_git_in(cwd, listing_args);
    let branches: Vec<String> = listing
        .lines()
        .map(|l| l.trim_start_matches('*').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    ok(
        "git_branch",
        json!({ "current": current, "branches": branches }),
    )
}

pub(super) fn git_remote(body: &Value) -> u64 {
    git_async_entry("git_remote", body, |body, plan| {
        let cwd = body_cwd(body);
        let requested = body
            .get("remote")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        let listed = git_step_replayed_by_call_order(plan, &["remote"], cwd)?;
        let names: Vec<String> = listed
            .get("stdout")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let selected: Vec<String> = if requested.is_empty() {
            names
        } else if names.iter().any(|name| name == requested) {
            vec![requested.to_owned()]
        } else {
            return Ok(err("git_remote", "requested remote does not exist"));
        };
        let branch_result =
            git_step_replayed_by_call_order(plan, &["branch", "--show-current"], cwd)?;
        let branch = branch_result
            .get("stdout")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let upstream_result = if branch.is_empty() {
            Value::Null
        } else {
            git_step_replayed_by_call_order(
                plan,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
                cwd,
            )?
        };
        let upstream = upstream_result
            .get("stdout")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let mut remotes = Vec::with_capacity(selected.len());
        for name in selected {
            let fetch_result =
                git_step_replayed_by_call_order(plan, &["remote", "get-url", name.as_str()], cwd)?;
            let push_result = git_step_replayed_by_call_order(
                plan,
                &["remote", "get-url", "--push", name.as_str()],
                cwd,
            )?;
            let fetch_url = fetch_result
                .get("stdout")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            let push_url = push_result
                .get("stdout")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            remotes.push(json!({ "name": name, "fetch_url": fetch_url, "push_url": push_url }));
        }
        Ok(ok(
            "git_remote",
            json!({ "branch": branch, "upstream": upstream, "remotes": remotes }),
        ))
    })
}

pub(super) fn git_checkout_pathspec(raw: &str, top: &str, prefix: &str) -> Result<(String, bool), &'static str> {
    let spec = raw.trim();
    if spec.is_empty() {
        return Err("empty pathspec");
    }
    if spec.starts_with('-') {
        return Err("a leading '-' would be read as an option");
    }
    if spec.starts_with(':') {
        return Err("pathspec magic is refused");
    }
    let unified = spec.replace('\\', "/");
    if unified.split('/').any(|segment| segment == "..") {
        return Err("'..' traversal is refused");
    }
    let (from_top, for_git) = if crate::pkfs::is_absolute(spec) {
        let top_dir = top.trim_end_matches('/');
        let inside = unified.len() >= top_dir.len()
            && unified.is_char_boundary(top_dir.len())
            && unified[..top_dir.len()].eq_ignore_ascii_case(top_dir)
            && matches!(unified[top_dir.len()..].chars().next(), None | Some('/'));
        if !inside {
            return Err("absolute path is outside the repository");
        }
        let rest = unified[top_dir.len()..].trim_start_matches('/').to_string();
        let for_git = format!(
            ":(top){}",
            if rest.is_empty() { "." } else { rest.as_str() }
        );
        (rest, for_git)
    } else {
        (format!("{}{}", prefix, unified), spec.to_string())
    };
    let top_segment = from_top
        .split('/')
        .find(|segment| !segment.is_empty() && *segment != ".")
        .unwrap_or("")
        .to_ascii_lowercase();
    if GIT_PROTECTED_PATHSPECS.iter().any(|(name, _)| {
        top_segment == name.trim_end_matches('*')
            || (name.ends_with('*') && top_segment.starts_with(name.trim_end_matches('*')))
    }) && !is_restorable_transient_generated_path(&from_top)
    {
        return Err("the project's own .gm/ and .agentplug* are never restored");
    }
    let restorable = is_restorable_transient_generated_path(&from_top);
    Ok((for_git, restorable))
}

fn checkout_diff(source: Option<&str>, scope: &[&str], cwd: Option<&str>) -> Result<Vec<String>, String> {
    let mut argv: Vec<&str> = vec!["diff", "--name-only"];
    if let Some(s) = source {
        argv.push(s);
    }
    argv.push("--");
    argv.extend(scope.iter().copied());
    let r = git_call_argv(&argv, cwd);
    let stdout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    if r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0) != 0 {
        let stderr = r.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
        return Err(format!("{}{}", stdout, stderr).trim().to_string());
    }
    Ok(stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn checkout_restore(source_arg: Option<&str>, scope: &[&str], cwd: Option<&str>) -> Result<String, String> {
    let mut argv: Vec<&str> = vec!["restore"];
    if let Some(a) = source_arg {
        argv.push(a);
    }
    argv.push("--");
    argv.extend(scope.iter().copied());
    let r = git_call_argv(&argv, cwd);
    let output = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0) != 0 {
        return Err(output.trim().to_string());
    }
    Ok(output.trim().to_string())
}

pub(super) fn git_checkout_paths(body: &Value, cwd: Option<&str>, requested: &Value) -> u64 {
    if body
        .get("create")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return err("git_checkout", "create cannot be combined with paths");
    }
    let items: Vec<&Value> = match requested {
        Value::Array(a) => a.iter().collect(),
        single => vec![single],
    };
    if items.is_empty() {
        return err(
            "git_checkout",
            "paths must be a non-empty list of pathspecs",
        );
    }
    let source = body
        .get("ref")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if source.is_some_and(|s| s.starts_with('-')) {
        return err("git_checkout", "ref must not start with '-'");
    }
    let top = exec_git_in(cwd, "rev-parse --show-toplevel")
        .trim()
        .replace('\\', "/");
    if top.is_empty() {
        return err("git_checkout", "not inside a git worktree");
    }
    let prefix = exec_git_in(cwd, "rev-parse --show-prefix")
        .trim()
        .replace('\\', "/");
    let mut regular: Vec<String> = vec![];
    let mut exempt: Vec<String> = vec![];
    for item in items {
        let Some(raw) = item.as_str() else {
            return err("git_checkout", "every entry of paths must be a string");
        };
        match git_checkout_pathspec(raw, &top, &prefix) {
            Ok((spec, true)) => exempt.push(spec),
            Ok((spec, false)) => regular.push(spec),
            Err(reason) => {
                return err_json(
                    "git_checkout",
                    json!({ "error": format!("refused pathspec {:?}: {}", raw, reason), "path": raw }),
                )
            }
        }
    }
    let protected: Vec<&str> = GIT_PROTECTED_PATHSPECS.iter().map(|(_, spec)| *spec).collect();
    let regular_scope: Vec<&str> = regular.iter().map(|s| s.as_str()).collect();
    let exempt_scope: Vec<&str> = exempt.iter().map(|s| s.as_str()).collect();
    let mut guarded_scope: Vec<&str> = regular_scope.clone();
    guarded_scope.extend(protected.iter().copied());
    let (guarded, blocked): (Vec<String>, Vec<String>) = if regular.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let unguarded = match checkout_diff(source, &regular_scope, cwd) {
            Ok(v) => v,
            Err(e) => return err("git_checkout", &e),
        };
        let guarded = match checkout_diff(source, &guarded_scope, cwd) {
            Ok(v) => v,
            Err(e) => return err("git_checkout", &e),
        };
        let blocked: Vec<String> = {
            let kept: std::collections::HashSet<&str> = guarded.iter().map(String::as_str).collect();
            unguarded
                .iter()
                .filter(|p| !kept.contains(p.as_str()))
                .cloned()
                .collect()
        };
        (guarded, blocked)
    };
    if !blocked.is_empty() {
        let shown: Vec<&str> = blocked.iter().take(20).map(String::as_str).collect();
        let more = blocked.len() - shown.len();
        return err(
            "git_checkout",
            &format!(
                "refused by policy: the project's own .gm/ and .agentplug* are never restored; {} named path(s) fall under it: {}{}",
                blocked.len(),
                shown.join(", "),
                if more > 0 { format!(" (and {} more)", more) } else { String::new() }
            ),
        );
    }
    let exempt_differing = if exempt_scope.is_empty() {
        Vec::new()
    } else {
        match checkout_diff(source, &exempt_scope, cwd) {
            Ok(v) => v,
            Err(e) => return err("git_checkout", &e),
        }
    };
    let source_arg = source.map(|s| format!("--source={}", s));
    let mut outputs: Vec<String> = vec![];
    if !regular.is_empty() {
        match checkout_restore(source_arg.as_deref(), &guarded_scope, cwd) {
            Ok(o) => outputs.push(o),
            Err(e) => return err("git_checkout", &e),
        }
    }
    if !exempt_scope.is_empty() {
        match checkout_restore(source_arg.as_deref(), &exempt_scope, cwd) {
            Ok(o) => outputs.push(o),
            Err(e) => return err("git_checkout", &e),
        }
    }
    let mut restored = guarded;
    restored.extend(exempt_differing);
    let output = outputs
        .into_iter()
        .filter(|o| !o.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    ok(
        "git_checkout",
        json!({ "restored": restored, "source": source.unwrap_or("index"), "output": output }),
    )
}

pub(super) fn git_worktree(body: &Value) -> u64 {
    const VERB: &str = "git_worktree";
    let action = match body.get("action").and_then(Value::as_str) {
        Some("add") => "add",
        Some("list") => "list",
        Some("remove") => "remove",
        _ => return err(VERB, "action must be add, list or remove"),
    };
    let accepted: &[&str] = match action {
        "add" => &["action", "path", "ref", "detach"],
        "remove" => &["action", "path"],
        _ => &["action"],
    };
    if let Some(refusal) = refuse_unknown_fields(VERB, body, accepted) {
        return refusal;
    }
    let cwd = body_cwd(body);
    if action == "list" {
        let result = match run_git_checked(
            &["worktree", "list", "--porcelain", "-z"],
            cwd,
            VERB,
            "worktree list failed",
        ) {
            Ok(result) => result,
            Err(error) => return error,
        };
        let output = result.get("stdout").and_then(Value::as_str).unwrap_or("");
        let mut worktrees = Vec::new();
        let mut entry = serde_json::Map::new();
        for field in output.split('\0') {
            if field.is_empty() {
                if !entry.is_empty() {
                    worktrees.push(Value::Object(std::mem::take(&mut entry)));
                }
                continue;
            }
            let (key, value) = field.split_once(' ').unwrap_or((field, ""));
            entry.insert(
                key.to_string(),
                if value.is_empty() {
                    json!(true)
                } else {
                    json!(value)
                },
            );
        }
        if !entry.is_empty() {
            worktrees.push(Value::Object(entry));
        }
        return ok(VERB, json!({"action": action, "worktrees": worktrees}));
    }
    let path = match body.get("path").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() && !value.contains(['\0', '\r', '\n']) => value,
        _ => {
            return err(
                VERB,
                "path must be a nonempty string without NUL or line breaks",
            )
        }
    };
    if action == "remove" {
        if let Err(error) = run_git_checked(
            &["worktree", "remove", "--", path],
            cwd,
            VERB,
            "worktree remove failed",
        ) {
            return error;
        }
        return ok(VERB, json!({"action": action, "removed": path}));
    }
    let reference = match body.get("ref") {
        None => "HEAD",
        Some(Value::String(value))
            if !value.trim().is_empty()
                && !value.starts_with('-')
                && !value.contains(['\0', '\r', '\n']) =>
        {
            value.as_str()
        }
        _ => {
            return err(
                VERB,
                "ref must be a nonempty string without a leading '-' or line breaks",
            )
        }
    };
    let detach = match body.get("detach") {
        None => true,
        Some(Value::Bool(value)) => *value,
        _ => return err(VERB, "detach must be a boolean"),
    };
    if !detach && body.get("ref").is_none() {
        return err(
            VERB,
            "detach false requires an explicit existing branch ref",
        );
    }
    if !detach {
        let branch = format!("refs/heads/{}", reference);
        let probe = git_call_argv(&["show-ref", "--verify", "--quiet", &branch], cwd);
        if probe.get("exit_code").and_then(Value::as_i64) != Some(0) {
            return err(VERB, "detach false requires an existing local branch name");
        }
    }
    let mut argv = vec!["worktree", "add"];
    if detach {
        argv.push("--detach");
    }
    argv.extend(["--", path, reference]);
    if let Err(error) = run_git_checked(&argv, cwd, VERB, "worktree add failed") {
        return error;
    }
    ok(
        VERB,
        json!({"action": action, "added": path, "ref": reference, "detached": detach}),
    )
}

const BRANCH_CREATE_REFUSED: &str = "BRANCH_CREATE_REFUSED: this project keeps all work on main; creating a branch is refused";

pub(super) fn git_checkout(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_checkout",
        body,
        &["ref", "create", "path", "paths", "files"],
    ) {
        return refusal;
    }
    let cwd = body_cwd(body);
    if let Some(requested) = body
        .get("paths")
        .or_else(|| body.get("files"))
        .or_else(|| body.get("path"))
    {
        return git_checkout_paths(body, cwd, requested);
    }
    let refspec = body
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if refspec.is_empty() {
        return err("git_checkout", "ref required");
    }
    let create = body
        .get("create")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if create {
        return err("git_checkout", BRANCH_CREATE_REFUSED);
    }
    let argv: Vec<&str> = vec!["checkout", refspec];
    if let Err(e) = run_git_checked(&argv, cwd, "git_checkout", "checkout failed") {
        return e;
    }
    ok(
        "git_checkout",
        json!({ "checked_out": refspec, "created": false }),
    )
}

pub(super) fn git_merge(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_merge", body, &["ref", "ff_only", "message"]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let refspec = body
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if refspec.is_empty() {
        return err("git_merge", "ref required");
    }
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let ff_only = body
        .get("ff_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let mut argv: Vec<&str> = vec!["merge", "--no-edit"];
    if ff_only {
        argv.push("--ff-only");
    }
    if !message.is_empty() {
        argv.push("-m");
        argv.push(message);
    }
    argv.push(refspec);
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        let conflicts: Vec<String> = exec_git_in(cwd, "diff --name-only --diff-filter=U")
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        return err_json(
            "git_merge",
            json!({
                "error": out,
                "conflicted": !conflicts.is_empty(),
                "conflicts": conflicts,
                "head": head_before,
                "hint": "resolve each conflicted path, git_add them, then git_commit; or git_merge_abort to restore the pre-merge HEAD"
            }),
        );
    }
    let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    ok(
        "git_merge",
        json!({
            "merged": refspec,
            "head_before": head_before,
            "head_after": head_after,
            "already_up_to_date": head_before == head_after,
            "fast_forward": out.lines().any(|line| line == "Fast-forward" || line.starts_with("Fast-forward (")),
            "output": out
        }),
    )
}

pub(super) const MERGE_ABORT_SHELF_MESSAGE: &str = "gm merge-abort shelf";

pub(super) const GIT_MERGE_ABORT_HELP: &str = "\
git_merge_abort {} restores the pre-merge HEAD: {aborted, merge_in_progress, head}, plus
{preserved_paths, restored} when a merge was in progress.
  preserve: false (default true) never shelves anything -- a reset that cannot run is reported
            instead of worked around.
  No merge in progress is a clean reply: {aborted:false, merge_in_progress:false, head}, never a
  raw git error. MERGE_HEAD is repo-wide state, so dispatch it only on a repo you own.
  `git merge --abort` runs `git reset --merge`, which refuses while the index and the worktree
  disagree for a path -- typically a file git auto-merged that another session then edited in the
  worktree. The verb shelves exactly those paths with
  `git stash push --keep-index -- <paths>`, which leaves the index content in the worktree so the
  reset can run, aborts, then pops the shelf so the edits land back in the worktree as unstaged
  edits. Nothing is discarded silently: if the shelf or the pop cannot run, the reply says so and
  names the stash entry that still holds the edits.
  Unmerged (conflicted) paths are never shelved -- they are reported in `conflicted` and the verb
  refuses rather than rewriting a conflict another session is resolving.
  A refusal names every path blocking the reset in `blocking_paths` and gives `next_dispatch`:
  git_add {paths:[...]} to accept the worktree version, or git_stash {paths:[...]} then
  git_merge_abort then git_stash_pop.";

pub(super) fn git_exit_code(result: &Value) -> i64 {
    result
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
}

pub(super) fn git_output_text(result: &Value) -> String {
    format!(
        "{}{}",
        result.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        result.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    )
}

pub(super) fn git_name_only(cwd: Option<&str>, argv: &[&str]) -> Vec<String> {
    git_call_argv(argv, cwd)
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

pub(super) fn git_merge_in_progress(cwd: Option<&str>) -> bool {
    git_call_argv(&["rev-parse", "--verify", "--quiet", "MERGE_HEAD"], cwd)
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(1)
        == 0
}

pub(super) fn git_merge_abort(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_merge_abort", body, &["preserve"]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let preserve = body
        .get("preserve")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let head_now = || exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    if !git_merge_in_progress(cwd) {
        return ok(
            "git_merge_abort",
            json!({ "aborted": false, "merge_in_progress": false, "head": head_now() }),
        );
    }
    let first = git_call_argv(&["merge", "--abort"], cwd);
    if git_exit_code(&first) == 0 {
        return ok(
            "git_merge_abort",
            json!({
                "aborted": true,
                "merge_in_progress": false,
                "preserved_paths": [],
                "restored": true,
                "head": head_now(),
            }),
        );
    }
    let first_out = git_output_text(&first);
    let blocking = git_name_only(cwd, &["diff", "--name-only"]);
    let conflicted = git_name_only(cwd, &["diff", "--name-only", "--diff-filter=U"]);
    let shelvable: Vec<String> = blocking
        .iter()
        .filter(|p| !conflicted.contains(p))
        .cloned()
        .collect();
    let refuse = |error: String, extra: Value| -> u64 {
        let mut payload = json!({
            "error": error,
            "aborted": false,
            "merge_in_progress": true,
            "blocking_paths": blocking.clone(),
            "conflicted": conflicted.clone(),
            "head": head_now(),
            "next_dispatch": {
                "accept_worktree": { "verb": "git_add", "paths": blocking.clone() },
                "shelf_first": ["git_stash", "git_merge_abort", "git_stash_pop"],
            },
        });
        if let (Some(dst), Some(src)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        err_json("git_merge_abort", payload)
    };
    let uptodate_class =
        first_out.contains("not uptodate") || first_out.contains("Could not reset index file");
    if !preserve || !uptodate_class || shelvable.is_empty() {
        let reason = if !preserve {
            "preserve:false -- refusing to shelve worktree edits to make the reset run"
        } else if shelvable.is_empty() {
            "no shelvable path explains the reset failure -- nothing was modified"
        } else {
            "the reset failure is not the index/worktree mismatch this verb can shelve around"
        };
        return refuse(
            first_out,
            json!({
                "reason": reason,
                "hint": "git_add the blocking paths to accept the worktree version, then git_commit; or git_stash them, git_merge_abort, git_stash_pop",
            }),
        );
    }
    let mut argv: Vec<&str> = vec![
        "stash",
        "push",
        "--keep-index",
        "--message",
        MERGE_ABORT_SHELF_MESSAGE,
        "--",
    ];
    for p in &shelvable {
        argv.push(p.as_str());
    }
    let shelved = git_call_argv(&argv, cwd);
    if git_exit_code(&shelved) != 0 {
        return refuse(
            git_output_text(&shelved),
            json!({ "reason": "shelving the blocking paths failed -- the merge state and every worktree edit are untouched" }),
        );
    }
    let shelf_created = !git_output_text(&shelved).contains("No local changes to save");
    let shelf_ref = if shelf_created {
        exec_git_in(cwd, "stash list -1 --format=%gd").trim().to_string()
    } else {
        String::new()
    };
    let still_blocking: Vec<String> = git_name_only(cwd, &["diff", "--name-only"])
        .into_iter()
        .filter(|p| shelvable.contains(p))
        .collect();
    if !still_blocking.is_empty() {
        if shelf_created {
            let _ = git_call_argv(&["stash", "pop", shelf_ref.as_str()], cwd);
        }
        return refuse(
            format!(
                "the shelf left these paths still disagreeing with the index: {}",
                still_blocking.join(", ")
            ),
            json!({
                "reason": "the shelf did not make the worktree agree with the index, so the reset was not retried; the shelf was popped back",
                "still_blocking": still_blocking,
            }),
        );
    }
    let second = git_call_argv(&["merge", "--abort"], cwd);
    if git_exit_code(&second) != 0 {
        let restored = if shelf_created {
            git_exit_code(&git_call_argv(&["stash", "pop", shelf_ref.as_str()], cwd)) == 0
        } else {
            true
        };
        return refuse(
            git_output_text(&second),
            json!({
                "reason": "the abort still failed after shelving -- the shelf was popped back, so no edit is lost",
                "shelf_ref": shelf_ref,
                "restored": restored,
            }),
        );
    }
    let mut restored = true;
    let mut restore_out = String::new();
    if shelf_created {
        let pop = git_call_argv(&["stash", "pop", shelf_ref.as_str()], cwd);
        restore_out = git_output_text(&pop);
        restored = git_exit_code(&pop) == 0;
    }
    if !restored {
        return err_json(
            "git_merge_abort",
            json!({
                "error": restore_out,
                "aborted": true,
                "merge_in_progress": false,
                "preserved_paths": shelvable,
                "restored": false,
                "stash": shelf_ref,
                "head": head_now(),
                "hint": "the merge is aborted and the edits still sit in that stash entry -- git_stash_pop {ref} once the worktree is quiet, or git_stash_list to see it",
            }),
        );
    }
    ok(
        "git_merge_abort",
        json!({
            "aborted": true,
            "merge_in_progress": false,
            "preserved_paths": shelvable,
            "restored": true,
            "stash_popped": shelf_ref,
            "head": head_now(),
        }),
    )
}

pub(super) fn git_cherry_pick(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body
        .get("rev")
        .or_else(|| body.get("ref"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if refspec.is_empty() {
        return err("git_cherry_pick", "rev required");
    }
    let porcelain = git_porcelain_in(cwd);
    if !porcelain.trim().is_empty() {
        return err_json(
            "git_cherry_pick",
            json!({
                "clean_worktree_required": true,
                "porcelain": porcelain,
                "hint": "commit, stash, or revert the current changes before cherry-picking"
            }),
        );
    }
    let head_before = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    let result = git_call_argv(&["cherry-pick", refspec], cwd);
    let code = result
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let output = format!(
        "{}{}",
        result.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        result.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        let conflicts: Vec<String> = exec_git_in(cwd, "diff --name-only --diff-filter=U")
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        let cherry_pick_head = exec_git_in(cwd, "rev-parse --verify -q CHERRY_PICK_HEAD")
            .trim()
            .to_string();
        let aborted = if cherry_pick_head.is_empty() {
            false
        } else {
            git_call_argv(&["cherry-pick", "--abort"], cwd)
                .get("exit_code")
                .and_then(|x| x.as_i64())
                == Some(0)
        };
        return err_json(
            "git_cherry_pick",
            json!({
                "error": output,
                "conflicted": !conflicts.is_empty(),
                "conflicts": conflicts,
                "aborted": aborted,
                "head_before": head_before,
                "hint": if aborted { "cherry-pick was aborted; resolve the listed conflicts before retrying" } else { "cherry-pick did not complete; inspect git status before retrying" }
            }),
        );
    }
    let head_after = exec_git_in(cwd, "rev-parse HEAD").trim().to_string();
    if head_after.is_empty() || head_after == head_before {
        return err(
            "git_cherry_pick",
            "cherry-pick reported success but HEAD did not move",
        );
    }
    ok(
        "git_cherry_pick",
        json!({
            "cherry_picked": refspec,
            "head_before": head_before,
            "head_after": head_after,
            "output": output
        }),
    )
}

pub(super) const GIT_BODY_ENVELOPE_FIELDS: &[&str] = &[
    "SESSION_ID",
    "session_id",
    "sessionId",
    "cwd",
    "repo",
    "root",
    "projectPath",
    "git_root_override",
    "_plan",
    "full_response",
];
pub(crate) const GIT_PROTECTED_PATHSPECS: &[(&str, &str)] = &[
    (".gm", ":(top,exclude).gm"),
    (".agentplug*", ":(top,exclude).agentplug*"),
];
pub(super) const GIT_STASH_UNTRACKED_REFUSAL_THRESHOLD: usize = 2000;

pub(super) fn refuse_unknown_fields(verb: &str, body: &Value, accepted: &[&str]) -> Option<u64> {
    let unknown: Vec<&String> = body
        .as_object()?
        .keys()
        .filter(|k| {
            !GIT_BODY_ENVELOPE_FIELDS.contains(&k.as_str()) && !accepted.contains(&k.as_str())
        })
        .collect();
    if unknown.is_empty() {
        return None;
    }
    Some(err_json(
        verb,
        json!({
            "error": format!("unknown body fields for {}: {}", verb, unknown.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")),
            "unknown_fields": unknown,
            "accepted_fields": accepted,
        }),
    ))
}

pub(super) fn body_revision(body: &Value) -> &str {
    ["range", "ref", "rev"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
}

pub(super) fn body_pathspecs(body: &Value) -> Vec<String> {
    let mut specs: Vec<String> = Vec::new();
    for key in ["path", "paths", "files"] {
        match body.get(key) {
            Some(Value::String(s)) => specs.push(s.clone()),
            Some(Value::Array(arr)) => {
                specs.extend(arr.iter().filter_map(|x| x.as_str().map(String::from)))
            }
            _ => {}
        }
    }
    specs.retain(|s| !s.is_empty());
    specs.dedup();
    specs
}

pub(super) fn git_stash(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_stash",
        body,
        &["include_untracked", "message", "path", "paths", "files"],
    ) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let include_untracked = body
        .get("include_untracked")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("gm shelf")
        .trim();
    let paths = body_pathspecs(body);
    let mut scope: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    scope.extend(GIT_PROTECTED_PATHSPECS.iter().map(|(_, spec)| *spec));
    if include_untracked {
        let mut probe: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--"];
        probe.extend(scope.iter().copied());
        let untracked = git_call_argv(&probe, cwd)
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .lines()
            .count();
        if untracked > GIT_STASH_UNTRACKED_REFUSAL_THRESHOLD {
            return err_json(
                "git_stash",
                json!({
                    "error": format!("refusing to stash {} untracked files (limit {}): a stash that size runs for minutes and its clean phase deletes them from the worktree", untracked, GIT_STASH_UNTRACKED_REFUSAL_THRESHOLD),
                    "untracked_count": untracked,
                    "threshold": GIT_STASH_UNTRACKED_REFUSAL_THRESHOLD,
                    "hint": "pass paths:[...] to shelve only the files you mean, or include_untracked:false to shelve tracked changes only"
                }),
            );
        }
    }
    let mut argv = vec!["stash", "push", "--message", message];
    if include_untracked {
        argv.push("--include-untracked");
    }
    argv.push("--");
    argv.extend(scope.iter().copied());
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        return err("git_stash", &output);
    }
    let created = !output.contains("No local changes to save");
    let stash = if created {
        exec_git_in(cwd, "stash list -1 --format=%gd")
            .trim()
            .to_string()
    } else {
        String::new()
    };
    ok(
        "git_stash",
        json!({
            "created": created,
            "stash": if stash.is_empty() { Value::Null } else { json!(stash) },
            "include_untracked": include_untracked,
            "paths": paths,
            "excluded": GIT_PROTECTED_PATHSPECS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            "output": output
        }),
    )
}

pub(super) fn git_stash_list(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_stash_list", body, &[]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let r = git_call_argv(
        &["stash", "list", "--format=%gd\u{1f}%h\u{1f}%aI\u{1f}%gs"],
        cwd,
    );
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        return err(
            "git_stash_list",
            r.get("stderr")
                .and_then(|x| x.as_str())
                .unwrap_or("stash list failed"),
        );
    }
    let stashes: Vec<Value> = r
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut it = l.splitn(4, '\u{1f}');
            let stash = it.next().unwrap_or("").to_string();
            let sha = it.next().unwrap_or("").to_string();
            let date = it.next().unwrap_or("").to_string();
            let subject = it.next().unwrap_or("").to_string();
            json!({ "stash": stash, "sha": sha, "date": date, "subject": subject })
        })
        .collect();
    ok(
        "git_stash_list",
        json!({ "count": stashes.len(), "stashes": stashes }),
    )
}

pub(super) fn git_init_target(body: &Value) -> Result<Option<String>, String> {
    let base = body_cwd(body).map(|c| c.trim()).filter(|c| !c.is_empty());
    let requested = body
        .get("path")
        .map(|v| v.as_str().map(str::trim).ok_or("path must be a string"))
        .transpose()?;
    let joined = match (requested, base) {
        (Some(""), _) => return Err("path must not be empty".to_string()),
        (Some(p), Some(b)) if !crate::pkfs::is_absolute(p) => {
            format!("{}/{}", b.trim_end_matches(['/', '\\']), p)
        }
        (Some(p), _) => p.to_string(),
        (None, Some(b)) => b.to_string(),
        (None, None) => return Ok(None),
    };
    let unified = joined.replace('\\', "/");
    if unified.split('/').any(|segment| segment == "..") {
        return Err("'..' traversal is refused".to_string());
    }
    if unified.char_indices().any(|(i, c)| c == ':' && i != 1) {
        return Err("':' is refused (alternate data streams)".to_string());
    }
    let protected = unified.split('/').any(|segment| {
        let lower = segment.to_ascii_lowercase();
        GIT_PROTECTED_PATHSPECS.iter().any(|(name, _)| {
            if name.ends_with('*') {
                lower.starts_with(name.trim_end_matches('*'))
            } else {
                lower == *name
            }
        })
    });
    if protected {
        return Err("a path inside the project's own .gm/ or .agentplug* is never initialised as a repository".to_string());
    }
    Ok(Some(joined))
}

pub(super) fn git_init_config_value(body: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(raw) = body.get(key) else {
        return Ok(None);
    };
    let value = raw
        .as_str()
        .map(str::trim)
        .ok_or_else(|| format!("{} must be a string", key))?;
    if value.is_empty() {
        return Err(format!("{} must not be empty", key));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!("{} must not contain control characters", key));
    }
    Ok(Some(value.to_string()))
}

pub(super) fn git_init_gitignore(root: &str) -> &'static str {
    if !crate::wasm_dispatch::host_allow_root(root) {
        return "unwritable";
    }
    match crate::gitignore::ensure_managed_gitignore(root) {
        Ok(true) => "added",
        Ok(false) => "present",
        Err(_) => "unwritable",
    }
}

pub(super) fn git_init(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields(
        "git_init",
        body,
        &["path", "user_name", "user_email", "initial_branch"],
    ) {
        return refusal;
    }
    let target = match git_init_target(body) {
        Ok(t) => t,
        Err(reason) => return err("git_init", &reason),
    };
    let cwd = target.as_deref();
    let shown = cwd.unwrap_or("the dispatch working directory");
    let user_name = match git_init_config_value(body, "user_name") {
        Ok(v) => v,
        Err(reason) => return err("git_init", &reason),
    };
    let user_email = match git_init_config_value(body, "user_email") {
        Ok(v) => v,
        Err(reason) => return err("git_init", &reason),
    };
    let probe = git_call_argv(&["rev-parse", "--show-toplevel"], cwd);
    let probe_code = probe
        .get("exit_code")
        .and_then(|x| x.as_i64())
        .unwrap_or(-1);
    let probe_err = probe.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
    if probe_code == 0 {
        let existing_root = probe
            .get("stdout")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .replace('\\', "/");
        return err_json(
            "git_init",
            json!({
                "error": format!("{} is already inside a git repository", shown),
                "root": existing_root,
                "hint": "git_init only turns a non-repository directory into a repository"
            }),
        );
    }
    if probe_err.contains("git cwd does not exist") {
        return err(
            "git_init",
            &format!("path does not exist or is not a directory: {}", shown),
        );
    }
    if !probe_err.to_lowercase().contains("not a git repository") {
        return err("git_init", probe_err);
    }
    let branch_arg = match git_init_config_value(body, "initial_branch") {
        Ok(Some(b)) if b.starts_with('-') => {
            return err("git_init", "initial_branch must not start with '-'")
        }
        Ok(Some(b)) => {
            let valid = git_call_argv(&["check-ref-format", "--branch", &b], cwd)
                .get("exit_code")
                .and_then(|x| x.as_i64())
                .unwrap_or(1)
                == 0;
            if !valid {
                return err(
                    "git_init",
                    &format!("initial_branch {:?} is not a valid branch name", b),
                );
            }
            Some(format!("--initial-branch={}", b))
        }
        Ok(None) => None,
        Err(reason) => return err("git_init", &reason),
    };
    let mut argv: Vec<&str> = vec!["init"];
    if let Some(a) = &branch_arg {
        argv.push(a.as_str());
    }
    if let Err(e) = run_git_checked(&argv, cwd, "git_init", "git init failed") {
        return e;
    }
    for (key, value) in [("user.name", &user_name), ("user.email", &user_email)] {
        if let Some(v) = value {
            if let Err(e) = run_git_checked(
                &["config", "--local", key, v.as_str()],
                cwd,
                "git_init",
                "git config failed",
            ) {
                return e;
            }
        }
    }
    let root = exec_git_in(cwd, "rev-parse --show-toplevel")
        .trim()
        .replace('\\', "/");
    let branch = exec_git_in(cwd, "symbolic-ref --short HEAD")
        .trim()
        .to_string();
    let gitignore = git_init_gitignore(&root);
    ok(
        "git_init",
        json!({
            "root": root,
            "branch": branch,
            "created": true,
            "gitignore": gitignore,
            "user_name": user_name,
            "user_email": user_email
        }),
    )
}

pub(super) fn git_stash_drop(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_stash_drop", body, &["ref"]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let refspec = body
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("stash@{0}")
        .trim();
    if refspec.is_empty() || refspec.starts_with('-') {
        return err("git_stash_drop", "ref must name a stash such as stash@{0}");
    }
    let r = git_call_argv(&["stash", "drop", refspec], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        return err("git_stash_drop", &output);
    }
    let remaining = exec_git_in(cwd, "stash list --format=%gd")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    ok(
        "git_stash_drop",
        json!({ "dropped": refspec, "remaining": remaining, "output": output }),
    )
}

pub(super) fn git_stash_pop(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_stash_pop", body, &["ref"]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let refspec = body
        .get("ref")
        .and_then(|v| v.as_str())
        .unwrap_or("stash@{0}")
        .trim();
    let r = git_call_argv(&["stash", "pop", "--index", refspec], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        let conflicts: Vec<String> = exec_git_in(cwd, "diff --name-only --diff-filter=U")
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        return err_json(
            "git_stash_pop",
            json!({
                "error": output,
                "conflicted": !conflicts.is_empty(),
                "conflicts": conflicts,
                "hint": "resolve conflicted paths, git_add them, then git_commit; the stash remains when pop fails"
            }),
        );
    }
    ok(
        "git_stash_pop",
        json!({ "restored": refspec, "output": output }),
    )
}

pub(super) fn git_branch_delete(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let name = body
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return err("git_branch_delete", "branch required");
    }
    let remote = body
        .get("remote")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    if remote {
        let remote_name = body
            .get("remote_name")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");
        let r = git_call_argv(&["push", remote_name, "--delete", name], cwd);
        let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
        let out = format!(
            "{}{}",
            r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
            r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
        );
        if code != 0 {
            return err("git_branch_delete", &out);
        }
        return ok(
            "git_branch_delete",
            json!({ "deleted": name, "scope": "remote", "remote": remote_name, "output": out }),
        );
    }
    let flag = if force { "-D" } else { "-d" };
    let r = git_call_argv(&["branch", flag, name], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let out = format!(
        "{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or("")
    );
    if code != 0 {
        return err_json(
            "git_branch_delete",
            json!({
                "error": out,
                "unmerged_guard": !force && out.contains("not fully merged"),
                "hint": "git refused because the branch holds commits reachable from nowhere else; merge it first, or pass force true only if that work is genuinely disposable"
            }),
        );
    }
    ok(
        "git_branch_delete",
        json!({ "deleted": name, "scope": "local", "forced": force, "output": out }),
    )
}

pub(super) fn git_rm(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_rm", body, &["paths", "force", "cached"]) {
        return refusal;
    }
    let cwd = body_cwd(body);
    let paths: Vec<String> = body
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return err("git_rm", "paths required");
    }
    let cached = body
        .get("cached")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut argv: Vec<&str> = vec!["rm"];
    if cached {
        argv.push("--cached");
    }
    if force {
        argv.push("-f");
    }
    argv.push("-r");
    for p in &paths {
        argv.push(p.as_str());
    }
    if let Err(e) = run_git_checked(&argv, cwd, "git_rm", "git rm failed") {
        return e;
    }
    ok(
        "git_rm",
        json!({ "removed": paths, "cached": cached, "forced": force }),
    )
}

pub(super) fn git_revert(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    if let Some(arr) = body.get("paths").and_then(|v| v.as_array()) {
        let paths: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        if paths.is_empty() {
            return err("git_revert", "paths empty");
        }
        let mut argv: Vec<&str> = vec!["checkout", "--"];
        for p in &paths {
            argv.push(p.as_str());
        }
        if let Err(e) = run_git_checked(&argv, cwd, "git_revert", "discard failed") {
            return e;
        }
        return ok("git_revert", json!({ "discarded": paths }));
    }
    if let Some(refspec) = body.get("ref").and_then(|v| v.as_str()) {
        if let Err(e) = run_git_checked(
            &["revert", "--no-edit", refspec],
            cwd,
            "git_revert",
            "revert failed",
        ) {
            return e;
        }
        return ok("git_revert", json!({ "reverted": refspec }));
    }
    err(
        "git_revert",
        "pass {paths:[...]} to discard working changes or {ref} to revert a commit",
    )
}

pub(super) fn git_reset(body: &Value) -> u64 {
    let cwd = body_cwd(body);
    let refspec = body.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("mixed");
    let paths: Vec<String> = body
        .get("paths")
        .or_else(|| body.get("files"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if !paths.is_empty() {
        if mode != "mixed" {
            return err("git_reset", "paths scope the index only: mode must be mixed or omitted");
        }
        let blocked = hard_excluded_pathspecs(&paths);
        if !blocked.is_empty() {
            return err_json("git_reset", protected_pathspec_refusal("git_reset", &blocked));
        }
        let mut argv: Vec<&str> = vec!["reset", refspec, "--"];
        argv.extend(paths.iter().map(String::as_str));
        if let Err(e) = run_git_checked(&argv, cwd, "git_reset", "reset failed") {
            return e;
        }
        return ok("git_reset", json!({ "reset_to": refspec, "mode": "mixed", "paths": paths }));
    }
    let mode_flag = match mode {
        "soft" => "--soft",
        "hard" => "--hard",
        _ => "--mixed",
    };
    if let Err(e) = run_git_checked(
        &["reset", mode_flag, refspec],
        cwd,
        "git_reset",
        "reset failed",
    ) {
        return e;
    }
    ok("git_reset", json!({ "reset_to": refspec, "mode": mode }))
}

pub(super) fn git_worktree_protected_segment(unified: &str) -> bool {
    unified.split('/').any(|segment| {
        let lower = segment.to_ascii_lowercase();
        GIT_PROTECTED_PATHSPECS.iter().any(|(name, _)| if name.ends_with('*') { lower.starts_with(name.trim_end_matches('*')) } else { lower == *name })
    })
}

pub(super) fn git_worktree_target(body: &Value, cwd: Option<&str>, verb: &str) -> Result<(String, String), Value> {
    let Some(raw) = body.get("path").and_then(|v| v.as_str()) else {
        return Err(json!({ "error": format!("{} requires a path", verb) }));
    };
    let spec = raw.trim();
    if spec.is_empty() { return Err(json!({ "error": format!("{} path must not be empty", verb) })); }
    if spec.starts_with('-') { return Err(json!({ "error": format!("{} path must not start with '-'", verb) })); }
    let unified = spec.replace('\\', "/");
    if unified.split('/').any(|segment| segment == "..") { return Err(json!({ "error": format!("{} path must not contain '..'", verb) })); }
    if git_worktree_protected_segment(&unified) {
        return Err(json!({ "error": format!("{} refuses a path inside the project's own .gm/ or .agentplug*", verb), "path": spec }));
    }
    let top = exec_git_in(cwd, "rev-parse --show-toplevel").trim().replace('\\', "/");
    if top.is_empty() { return Err(json!({ "error": format!("{} needs a repository: not inside a git worktree", verb) })); }
    if !crate::pkfs::is_absolute(spec) {
        return Ok((top.clone(), format!("{}/{}", top.trim_end_matches('/'), unified)));
    }
    let top_dir = top.trim_end_matches('/');
    let inside = unified.len() >= top_dir.len()
        && unified.is_char_boundary(top_dir.len())
        && unified[..top_dir.len()].eq_ignore_ascii_case(top_dir)
        && matches!(unified[top_dir.len()..].chars().next(), None | Some('/'));
    if inside || body.get("allowOutsideRoot").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok((top, unified));
    }
    Err(json!({
        "error": format!("{} refuses an absolute path outside the repository root {}", verb, top),
        "root": top,
        "path": spec,
        "hint": "pass allowOutsideRoot:true to place a worktree outside this repository"
    }))
}

pub(super) fn git_worktree_add(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_worktree_add", body, &["path", "ref", "create", "allowOutsideRoot"]) { return refusal; }
    let cwd = body_cwd(body);
    let (root, path) = match git_worktree_target(body, cwd, "git_worktree_add") { Ok(v) => v, Err(detail) => return err_json("git_worktree_add", detail) };
    let create = body.get("create").and_then(|v| v.as_bool()).unwrap_or(false);
    let reference = body.get("ref").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    if create && reference.is_none() { return err("git_worktree_add", "create:true requires a ref naming the new branch"); }
    if reference.is_some_and(|s| s.starts_with('-')) { return err("git_worktree_add", "ref must not start with '-'"); }
    let mut argv: Vec<String> = vec!["worktree".to_string(), "add".to_string()];
    if create {
        argv.push("-b".to_string());
        argv.push(reference.unwrap_or_default().to_string());
        argv.push(path.clone());
    } else {
        if let Some(s) = reference { argv.push(s.to_string()); }
        argv.push(path.clone());
    }
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let r = git_call_argv(&argv_refs, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 { return err("git_worktree_add", &output); }
    let branch = exec_git_in(Some(path.as_str()), "rev-parse --abbrev-ref HEAD").trim().to_string();
    ok("git_worktree_add", json!({ "root": root, "path": path, "branch": branch, "created": true, "output": output.trim() }))
}

pub(super) fn git_worktree_row(path: &str) -> serde_json::Map<String, Value> {
    let mut row = serde_json::Map::new();
    row.insert("path".to_string(), json!(path.replace('\\', "/")));
    row.insert("head".to_string(), Value::Null);
    row.insert("branch".to_string(), Value::Null);
    row.insert("bare".to_string(), json!(false));
    row.insert("detached".to_string(), json!(false));
    row.insert("locked".to_string(), Value::Null);
    row.insert("prunable".to_string(), Value::Null);
    row
}

pub(super) fn git_worktree_list(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_worktree_list", body, &[]) { return refusal; }
    let cwd = body_cwd(body);
    let r = git_call_argv(&["worktree", "list", "--porcelain"], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 { return err("git_worktree_list", &output); }
    let mut rows: Vec<Value> = Vec::new();
    let mut row: Option<serde_json::Map<String, Value>> = None;
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(finished) = row.take() { rows.push(Value::Object(finished)); }
            row = Some(git_worktree_row(rest.trim()));
            continue;
        }
        let Some(current) = row.as_mut() else { continue; };
        if let Some(rest) = line.strip_prefix("HEAD ") { current.insert("head".to_string(), json!(rest.trim())); }
        else if let Some(rest) = line.strip_prefix("branch ") { current.insert("branch".to_string(), json!(rest.trim())); }
        else if line == "bare" { current.insert("bare".to_string(), json!(true)); }
        else if line == "detached" { current.insert("detached".to_string(), json!(true)); }
        else if line == "locked" { current.insert("locked".to_string(), json!(true)); }
        else if let Some(rest) = line.strip_prefix("locked ") { current.insert("locked".to_string(), json!(rest.trim())); }
        else if line == "prunable" { current.insert("prunable".to_string(), json!(true)); }
        else if let Some(rest) = line.strip_prefix("prunable ") { current.insert("prunable".to_string(), json!(rest.trim())); }
    }
    if let Some(finished) = row.take() { rows.push(Value::Object(finished)); }
    ok("git_worktree_list", json!({ "count": rows.len(), "worktrees": rows }))
}

pub(super) fn git_worktree_remove(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_worktree_remove", body, &["path", "force", "allowOutsideRoot"]) { return refusal; }
    let cwd = body_cwd(body);
    let (_, path) = match git_worktree_target(body, cwd, "git_worktree_remove") { Ok(v) => v, Err(detail) => return err_json("git_worktree_remove", detail) };
    let force = body.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut argv: Vec<&str> = vec!["worktree", "remove"];
    if force { argv.push("--force"); }
    argv.push(path.as_str());
    let r = git_call_argv(&argv, cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 { return err("git_worktree_remove", &output); }
    ok("git_worktree_remove", json!({ "removed": true, "path": path, "output": output.trim() }))
}

pub(super) fn git_worktree_prune(body: &Value) -> u64 {
    if let Some(refusal) = refuse_unknown_fields("git_worktree_prune", body, &[]) { return refusal; }
    let cwd = body_cwd(body);
    let r = git_call_argv(&["worktree", "prune"], cwd);
    let code = r.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let output = format!("{}{}",
        r.get("stdout").and_then(|x| x.as_str()).unwrap_or(""),
        r.get("stderr").and_then(|x| x.as_str()).unwrap_or(""));
    if code != 0 { return err("git_worktree_prune", &output); }
    ok("git_worktree_prune", json!({ "pruned": true, "output": output.trim() }))
}

pub(super) fn rebase_failed(out: &str) -> bool {
    let l = out.to_lowercase();
    l.contains("conflict")
        || l.contains("could not apply")
        || l.contains("error:")
        || l.contains("needs merge")
        || l.contains("automatic merge failed")
}

pub(super) fn exec_git_in(repo: Option<&str>, args: &str) -> String {
    let v = git_call(args, repo);
    v.get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

pub(super) fn git_porcelain_in(repo: Option<&str>) -> String {
    git_porcelain_scoped(repo, &[])
}

pub(super) fn is_restorable_transient_generated_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    matches!(
        path,
        ".gm/.last-scan-deps-ts"
            | ".gm/.last-scan-deps-result.json"
            | ".agentplug-kv/codeinsight-edges"
    ) || path.starts_with(".agentplug-kv/codeinsight-edges/")
}

pub(super) fn git_push_porcelain_in(repo: Option<&str>) -> String {
    let porcelain = git_porcelain_in(repo);
    porcelain
        .lines()
        .filter(|line| !is_transient_submodule_status(repo, line))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn is_transient_submodule_status(repo: Option<&str>, line: &str) -> bool {
    if line.get(..2) != Some(" m") {
        return false;
    }
    let Some(path) = line.get(3..) else {
        return false;
    };
    let path = path.trim();
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return false;
    }
    let index = git_call_argv(&["ls-files", "--stage", "--", path], repo);
    let gitlink = index
        .get("stdout")
        .and_then(|value| value.as_str())
        .map(|stdout| {
            stdout.lines().any(|entry| {
                entry.starts_with("160000 ")
                    && entry
                        .split_once('\t')
                        .map(|(_, indexed_path)| indexed_path == path)
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !gitlink {
        return false;
    }
    let root = exec_git_in(repo, "rev-parse --show-toplevel");
    let root = root.trim_end_matches(['\r', '\n']);
    if root.is_empty() {
        return false;
    }
    let submodule = format!("{}/{}", root.trim_end_matches(['/', '\\']), path);
    let inside = git_call_argv(&["rev-parse", "--is-inside-work-tree"], Some(&submodule));
    if inside
        .get("stdout")
        .and_then(|value| value.as_str())
        .map(str::trim)
        != Some("true")
    {
        return false;
    }
    let nested_porcelain = git_porcelain_in(Some(&submodule));
    !nested_porcelain.trim().is_empty()
        && nested_porcelain
            .lines()
            .all(is_transient_submodule_runtime_status)
}

pub(super) fn is_transient_submodule_runtime_status(line: &str) -> bool {
    let Some(path) = line.get(3..) else {
        return false;
    };
    let path = path.trim();
    matches!(path, ".gm" | ".agentplug-kv")
        || path.starts_with(".gm/")
        || path.starts_with(".agentplug-kv/")
}

pub(super) fn require_complete_git_porcelain(verb: &str, response: &Value) -> Result<String, u64> {
    let status = super::host_abi::porcelain_from(response);
    if status.partial {
        return Err(err_json(
            verb,
            json!({
                "error": "git status is incomplete -- refusing mutation without complete worktree evidence",
                "error_code": "git_status_incomplete",
                "partial": true,
                "failed": status.failed,
                "parked": status.parked,
                "exit_code": status.exit_code,
                "stderr": status.stderr,
                "skipped_paths": status.skipped_paths,
                "observed_porcelain": status.porcelain,
                "next_dispatch": "git_status",
            }),
        ));
    }
    Ok(status.porcelain)
}

pub(super) fn checked_git_porcelain_scoped(
    verb: &str,
    repo: Option<&str>,
    paths: &[String],
) -> Result<String, u64> {
    require_complete_git_porcelain(
        verb,
        &git_call_argv(&as_argv(&git_porcelain_argv(paths, repo)), repo),
    )
}

pub(super) fn git_porcelain_scoped(repo: Option<&str>, paths: &[String]) -> String {
    super::host_abi::porcelain_or_dirty(git_call_argv(
        &as_argv(&git_porcelain_argv(paths, repo)),
        repo,
    ))
}

pub(super) fn as_argv(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

pub(super) fn tracked_by_design(path: &str) -> bool {
    crate::gitignore::MUST_STAY_TRACKED.iter().any(|entry| {
        entry.starts_with(".gm/")
            && (path == *entry || (entry.ends_with('/') && path.starts_with(entry)))
    })
}

pub(super) fn git_tracked_protected_paths(cwd: Option<&str>) -> Vec<String> {
    let r = git_call_argv(
        &["ls-files", "-z", "--", ":(top).gm", ":(top).agentplug*"],
        cwd,
    );
    r.get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(String::from)
        .collect()
}

pub(super) fn tracked_by_design_in(tracked: &[String], path: &str) -> bool {
    if tracked_by_design(path) {
        return true;
    }
    let path = path.replace('\\', "/");
    tracked.iter().any(|entry| {
        let entry = entry.replace('\\', "/");
        entry == path || entry.starts_with(&format!("{path}/"))
    })
}

pub(super) fn dirty_protected_entries(cwd: Option<&str>) -> Vec<(String, String)> {
    let r = git_call_argv(
        &[
            "status",
            "--porcelain",
            "-z",
            "-uall",
            "--",
            ":(top).gm",
            ":(top).agentplug*",
        ],
        cwd,
    );
    let stdout = r.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let mut records = stdout.split('\0').filter(|record| record.len() > 3);
    let mut entries = vec![];
    while let Some(record) = records.next() {
        let status = record[..2].trim().to_string();
        let path = record[3..].to_string();
        if status.starts_with('R') || status.starts_with('C') {
            records.next();
        }
        entries.push((
            if status.is_empty() {
                "M".to_string()
            } else {
                status
            },
            path,
        ));
    }
    entries
}

pub(super) fn withheld_dirty_entries(cwd: Option<&str>) -> Vec<(String, String)> {
    let tracked = git_tracked_protected_paths(cwd);
    dirty_protected_entries(cwd)
        .into_iter()
        .filter(|(_, path)| !tracked_by_design_in(&tracked, path))
        .collect()
}

pub(super) fn caller_pathspec_covers(paths: &[String], candidate: &str) -> bool {
    paths.iter().any(|spec| {
        let spec = spec.replace('\\', "/");
        let spec = spec.trim_end_matches('/');
        if spec.is_empty() {
            return false;
        }
        if spec == candidate || candidate.starts_with(&format!("{spec}/")) {
            return true;
        }
        if !spec.contains('*') && !spec.contains('?') && !spec.contains('[') {
            return false;
        }
        match spec.rfind('/') {
            Some(slash) => candidate.starts_with(&format!("{}/", &spec[..slash])),
            None => true,
        }
    })
}

pub(super) fn excluded_pathspecs(paths: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut out = vec![".agentplug*".to_string()];
    for (_, path) in withheld_dirty_entries(cwd) {
        if !path.starts_with(".agentplug") && !caller_pathspec_covers(paths, &path) {
            out.push(path);
        }
    }
    out
}

pub(super) fn git_pathspec_scope(paths: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut scope: Vec<String> = vec![":(top,exclude).agentplug*".to_string()];
    let mut used: usize = scope[0].len() + 1;
    for (_, path) in withheld_dirty_entries(cwd) {
        if path.starts_with(".agentplug") || caller_pathspec_covers(paths, &path) {
            continue;
        }
        let spec = format!(":(top,exclude,literal){}", path);
        if used + spec.len() + 1 > GIT_PATHSPEC_SCOPE_EXCLUDE_BUDGET_CHARS {
            continue;
        }
        if scope.iter().any(|held| held == &spec) {
            continue;
        }
        used += spec.len() + 1;
        scope.push(spec);
    }
    if paths.is_empty() {
        scope.push(":/".to_string());
    } else {
        scope.extend(paths.iter().cloned());
    }
    scope
}

pub(super) fn hard_excluded_pathspecs(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|spec| spec.replace('\\', "/").starts_with(".agentplug"))
        .cloned()
        .collect()
}

pub(super) fn protected_pathspec_refusal(verb: &str, blocked: &[String]) -> Value {
    json!({
        "error": format!("refusing to stage protected pathspec(s): {} -- .agentplug* is runtime state and is never staged", blocked.join(", ")),
        "error_code": ERR_CODE_INVALID_ARGS,
        "blocked_paths": blocked,
        "next_dispatch": verb,
    })
}

pub(super) fn pathspecs_matching_nothing(cwd: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for spec in paths {
        let listed = git_call_argv(&["ls-files", "-z", "--", spec.as_str()], cwd);
        let has_tracked = !listed
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .is_empty();
        let status = git_call_argv(
            &["status", "--porcelain", "-uall", "-z", "--", spec.as_str()],
            cwd,
        );
        let has_dirty = !status
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .is_empty();
        if !has_tracked && !has_dirty {
            out.push(spec.clone());
        }
    }
    out
}

pub(super) fn pathspec_matches_nothing_refusal(verb: &str, paths: &[String], unmatched: &[String]) -> Value {
    json!({
        "error": format!("no such path in this repo, so nothing was staged or committed: {} -- pass paths relative to the repo root, or stage the file first", unmatched.join(", ")),
        "error_code": "pathspec_matches_nothing",
        "requested_paths": paths,
        "unmatched_paths": unmatched,
        "next_dispatch": verb,
    })
}

pub(super) fn paths_staged_nothing(cwd: Option<&str>, paths: &[String]) -> bool {
    let mut argv: Vec<String> = vec![
        "diff".to_string(),
        "--cached".to_string(),
        "--name-only".to_string(),
        "--".to_string(),
    ];
    argv.extend(paths.iter().cloned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    git_call_argv(&argv, cwd)
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .is_empty()
}

pub(super) fn git_stage_argv(paths: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["add".to_string(), "--".to_string()];
    if paths.is_empty() {
        argv.extend(git_pathspec_scope(paths, cwd));
    } else {
        argv.extend(paths.iter().cloned());
    }
    argv
}

pub(super) fn git_stage_argv_forced(paths: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["add".to_string(), "--force".to_string(), "--".to_string()];
    argv.extend(git_pathspec_scope(paths, cwd));
    argv
}

pub(super) fn parse_ignore_list(stdout: &str) -> Vec<String> {
    stdout.lines().map(|line| line.trim()).filter(|line| !line.is_empty()).map(String::from).collect()
}

pub(super) fn ignored_requested_paths(
    plan: &mut GitPendingTokenReplayPlan,
    cwd: Option<&str>,
    paths: &[String],
) -> Result<Vec<String>, u64> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut argv: Vec<String> = vec!["check-ignore".to_string(), "--".to_string()];
    argv.extend(paths.iter().cloned());
    let r = git_step_replayed_by_call_order(plan, &as_argv(&argv), cwd)?;
    Ok(parse_ignore_list(r.get("stdout").and_then(|v| v.as_str()).unwrap_or("")))
}

pub(super) fn ignored_requested_paths_now(cwd: Option<&str>, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut argv: Vec<String> = vec!["check-ignore".to_string(), "--".to_string()];
    argv.extend(paths.iter().cloned());
    parse_ignore_list(git_call_argv(&as_argv(&argv), cwd).get("stdout").and_then(|v| v.as_str()).unwrap_or(""))
}

pub(super) fn gitlink_paths(cwd: Option<&str>) -> Vec<String> {
    git_call_argv(&["ls-files", "--stage", "-z"], cwd)
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .split('\0')
        .filter_map(|entry| {
            let (metadata, path) = entry.split_once('\t')?;
            metadata.starts_with("160000 ").then(|| path.to_string())
        })
        .collect()
}

pub(super) fn changed_gitlink_paths(cwd: Option<&str>) -> Vec<String> {
    let gitlinks = gitlink_paths(cwd);
    let changed = git_call_argv(&["diff", "--name-only", "-z"], cwd);
    changed
        .get("stdout")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .split('\0')
        .filter(|path| gitlinks.iter().any(|gitlink| gitlink == path))
        .map(String::from)
        .collect()
}

pub(super) fn git_add_stage_argvs(paths: &[String], cwd: Option<&str>) -> Vec<Vec<String>> {
    if !paths.is_empty() {
        return vec![git_stage_argv(paths, cwd)];
    }
    let gitlinks = gitlink_paths(cwd);
    if gitlinks.is_empty() {
        return vec![git_stage_argv(paths, cwd)];
    }
    let mut source_scope = git_pathspec_scope(paths, cwd);
    for gitlink in &gitlinks {
        source_scope.push(format!(":(top,exclude,literal){}", gitlink));
    }
    let mut source_argv = vec!["add".to_string(), "--".to_string()];
    source_argv.extend(source_scope);
    let changed_gitlinks = changed_gitlink_paths(cwd);
    if changed_gitlinks.is_empty() {
        return vec![source_argv];
    }
    let mut gitlink_argv = vec!["add".to_string(), "--".to_string()];
    gitlink_argv.extend(changed_gitlinks);
    vec![gitlink_argv, source_argv]
}

pub(super) fn git_porcelain_argv(paths: &[String], cwd: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "status".to_string(),
        "--porcelain".to_string(),
        "--".to_string(),
    ];
    argv.extend(git_pathspec_scope(paths, cwd));
    argv
}

pub(super) fn with_exclusion_report(mut data: Value, cwd: Option<&str>, paths: &[String]) -> Value {
    let withheld = withheld_dirty_entries(cwd);
    let listed: Vec<String> = withheld
        .iter()
        .take(50)
        .map(|(status, path)| format!("{} {}", status, path))
        .collect();
    let active = excluded_pathspecs(paths, cwd);
    let active_count = active.len();
    let active: Vec<String> = active.into_iter().take(5).collect();
    if let Some(map) = data.as_object_mut() {
        map.insert("excluded".to_string(), json!(active));
        map.insert("excluded_count".to_string(), json!(active_count));
        if !paths.is_empty() {
            map.insert("requested_paths".to_string(), json!(paths));
        }
        map.insert("excluded_but_dirty".to_string(), json!(listed));
        map.insert(
            "excluded_but_dirty_count".to_string(),
            json!(withheld.len()),
        );
        if !withheld.is_empty() {
            map.insert("excluded_but_dirty_warning".to_string(), json!(format!(
                "{} dirty path(s) under .gm/.agentplug* are NOT committed: they are runtime/transient state, not tracked-by-design (memories, disciplines, prd.yml, mutables.yml, config, code-search). If any are real work, stage them explicitly or add them to the tracked-by-design set.",
                withheld.len()
            )));
        }
    }
    data
}

pub(super) fn files_in_commit(repo: Option<&str>) -> Vec<String> {
    exec_git_in(repo, "show --name-only --pretty=format: HEAD")
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub(super) fn porcelain_dirty_paths_all_within_committed_set(porcelain: &str, committed: &[String]) -> bool {
    if committed.is_empty() {
        return false;
    }
    let mut any = false;
    for line in porcelain.lines() {
        let path = line.get(3..).unwrap_or("").trim().trim_matches('"');
        if path.is_empty() {
            continue;
        }
        any = true;
        let normalized = path.replace('\\', "/");
        if !committed.iter().any(|c| c.replace('\\', "/") == normalized) {
            return false;
        }
    }
    any
}

pub(super) fn porcelain_dirty_paths(porcelain: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in porcelain.lines() {
        let raw = line.get(3..).unwrap_or("").trim().trim_matches('"');
        if raw.is_empty() {
            continue;
        }
        match raw.split_once(" -> ") {
            Some((old, new)) => {
                for part in [old, new].iter().copied() {
                    let normalized = part.trim().replace('\\', "/");
                    if !normalized.is_empty() {
                        out.push(normalized);
                    }
                }
            }
            None => out.push(raw.replace('\\', "/")),
        }
    }
    out
}

pub(super) fn resolve_commit(cwd: Option<&str>, refspec: &str) -> Option<String> {
    let peeled = format!("{}^{{commit}}", refspec);
    let response = git_call_argv(&["rev-parse", "--verify", "--quiet", peeled.as_str()], cwd);
    if response.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(1) != 0 {
        return None;
    }
    let sha = response
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

pub(super) fn ref_is_ancestor(cwd: Option<&str>, ancestor: &str, descendant: &str) -> bool {
    git_call_argv(&["merge-base", "--is-ancestor", ancestor, descendant], cwd)
        .get("exit_code")
        .and_then(|v| v.as_i64())
        == Some(0)
}

pub(super) fn delta_paths_between(cwd: Option<&str>, from: &str, to: &str) -> Option<Vec<String>> {
    let range = format!("{}..{}", from, to);
    let response = git_call_argv(&["diff", "--name-only", "-z", range.as_str()], cwd);
    if response.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(1) != 0 {
        return None;
    }
    Some(
        response
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(|entry| entry.replace('\\', "/"))
            .collect(),
    )
}

pub(super) struct PushDirtyScope {
    pub dirty_count: usize,
    pub delta_known: bool,
    pub fast_forward: bool,
    pub overlapping: Vec<String>,
}

impl PushDirtyScope {
    pub fn blocks(&self) -> bool {
        self.dirty_count > 0
            && !(self.delta_known && self.fast_forward && self.overlapping.is_empty())
    }
}

pub(super) fn push_dirty_scope(
    cwd: Option<&str>,
    branch: &str,
    source_sha: &str,
    porcelain: &str,
) -> PushDirtyScope {
    let dirty_paths = porcelain_dirty_paths(porcelain);
    if dirty_paths.is_empty() {
        return PushDirtyScope {
            dirty_count: 0,
            delta_known: true,
            fast_forward: true,
            overlapping: Vec::new(),
        };
    }
    let remote_ref = format!("refs/remotes/origin/{}", branch);
    let remote_sha = match resolve_commit(cwd, remote_ref.as_str()) {
        Some(sha) => sha,
        None => {
            return PushDirtyScope {
                dirty_count: dirty_paths.len(),
                delta_known: false,
                fast_forward: false,
                overlapping: Vec::new(),
            }
        }
    };
    let delta = match delta_paths_between(cwd, remote_sha.as_str(), source_sha) {
        Some(paths) => paths,
        None => {
            return PushDirtyScope {
                dirty_count: dirty_paths.len(),
                delta_known: false,
                fast_forward: false,
                overlapping: Vec::new(),
            }
        }
    };
    let dirty_count = dirty_paths.len();
    let overlapping: Vec<String> = dirty_paths
        .into_iter()
        .filter(|path| delta.iter().any(|changed| changed == path))
        .collect();
    PushDirtyScope {
        dirty_count,
        delta_known: true,
        fast_forward: ref_is_ancestor(cwd, &remote_sha, source_sha),
        overlapping,
    }
}

pub(super) fn push_dirty_refusal(
    verb: &str,
    cwd: Option<&str>,
    branch: &str,
    porcelain: &str,
    scope: &PushDirtyScope,
) -> Value {
    let preview: String = porcelain.lines().take(8).collect::<Vec<_>>().join("\n");
    let more = if porcelain.lines().count() > 8 {
        format!("\n... +{} more", porcelain.lines().count() - 8)
    } else {
        String::new()
    };
    let reason = if !scope.delta_known {
        format!(
            "worktree dirty in {} and the delta of branch {} cannot be bounded: refs/remotes/origin/{} does not resolve, so no path can be shown to sit outside what the push would carry. Commit or revert before pushing; an unpushed delta that cannot be separated from a dirty tree is an unwitnessed slice. Porcelain:\n{}{}",
            cwd.unwrap_or("cwd"), branch, branch, preview, more
        )
    } else if !scope.fast_forward {
        format!(
            "worktree dirty in {} and branch {} is not a fast-forward of refs/remotes/origin/{}: publishing needs a pull first, and a pull over {} dirty path(s) can clobber them. Commit or revert, then push. Porcelain:\n{}{}",
            cwd.unwrap_or("cwd"), branch, branch, scope.dirty_count, preview, more
        )
    } else {
        format!(
            "worktree dirty in {} and {} of its {} dirty path(s) are files the delta of branch {} over origin/{} also touches, so those uncommitted edits belong to what the push would carry: {}. Commit or revert them, then push. Porcelain:\n{}{}",
            cwd.unwrap_or("cwd"),
            scope.overlapping.len(),
            scope.dirty_count,
            branch,
            branch,
            scope.overlapping.join(", "),
            preview,
            more
        )
    };
    json!({
        "ok": false,
        "verb": verb,
        "gate_denied": true,
        "repo": cwd,
        "branch": branch,
        "porcelain": format!("{}{}", preview, more),
        "dirty_count": scope.dirty_count,
        "dirty_overlapping_delta": scope.overlapping,
        "push_delta_scoped": scope.delta_known,
        "fast_forward": scope.fast_forward,
        "reason": reason,
        "next_dispatch": "instruction",
        "next_dispatch_hint": "instruction",
        "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
        "next_action_hint": "Dirty paths the pushed commits do not touch are not part of the delta and no longer block this push; only dirty_overlapping_delta does. Commit or revert those, dispatch git_status to confirm, then re-dispatch git_push.",
    })
}

pub(super) fn exec_git_push_in(
    repo: Option<&str>,
    source_ref: &str,
    branch: &str,
    fallback: Option<&SshHttpsFallback>,
) -> (String, bool) {
    let refspec = format!("{}:{}", source_ref, branch);
    let push_argv = ["push", "origin", refspec.as_str()];
    let v = match fallback {
        Some(f) => f.call(&push_argv, repo),
        None => git_call_argv(&push_argv, repo),
    };
    let stdout = v.get("stdout").and_then(|x| x.as_str()).unwrap_or("");
    let stderr = v.get("stderr").and_then(|x| x.as_str()).unwrap_or("");
    let exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1);
    (format!("{}{}", stdout, stderr), exit_code == 0)
}

