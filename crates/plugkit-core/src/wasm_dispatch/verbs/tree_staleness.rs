use super::{body_cwd, git_call, pack, unpack_to_value};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const STALE_FETCH_MS: u64 = 24 * 60 * 60 * 1000;

fn scope_dir(body: &Value) -> String {
    let root = body_cwd(body).unwrap_or(".").trim_end_matches(['/', '\\']).to_string();
    let named = body
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| body.get("paths").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str))
        .map(|p| p.trim().trim_start_matches("./"))
        .filter(|p| !p.is_empty() && *p != "." && !p.contains("..") && !p.contains(['*', '?', '[']));
    let Some(named) = named else {
        return root;
    };
    let full = if named.starts_with('/') || named.as_bytes().get(1) == Some(&b':') {
        named.to_string()
    } else {
        format!("{root}/{named}")
    };
    let is_dir = crate::pkfs::stat(&full)
        .and_then(|s| s.get("isDirectory").and_then(Value::as_bool))
        .unwrap_or(false);
    if is_dir {
        return full;
    }
    match full.rsplit_once(['/', '\\']) {
        Some((parent, _)) if !parent.is_empty() && crate::pkfs::stat(parent).is_some() => parent.to_string(),
        _ => root,
    }
}

fn git_ok_stdout(args: &str, dir: &str) -> Option<String> {
    let v = git_call(args, Some(dir));
    let failed = v.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0
        || v.get("ok").and_then(Value::as_bool) == Some(false);
    if failed {
        return None;
    }
    v.get("stdout").and_then(Value::as_str).map(str::to_string)
}

fn fetched_ago_ms(git_dir: &str) -> Option<u64> {
    let stat = crate::pkfs::stat(&format!("{git_dir}/FETCH_HEAD"))?;
    let mtime = stat
        .get("mtime_ms")
        .or_else(|| stat.get("mtimeMs"))
        .and_then(Value::as_f64)? as u64;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(now.saturating_sub(mtime))
}

pub(super) fn tree_staleness(body: &Value) -> Option<Value> {
    let dir = scope_dir(body);
    let heads = git_ok_stdout("rev-parse --absolute-git-dir HEAD @{u}", &dir)?;
    let mut lines = heads.lines().map(str::trim);
    let (git_dir, head, upstream) = (lines.next()?, lines.next()?, lines.next()?);
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    let fetched_ago = fetched_ago_ms(git_dir);
    let fetch_old = fetched_ago.is_some_and(|ms| ms >= STALE_FETCH_MS);
    let (behind, ahead) = if head == upstream {
        (0, 0)
    } else {
        let counts = git_ok_stdout("rev-list --left-right --count @{u}...HEAD", &dir)?;
        let mut n = counts.split_whitespace().map(|x| x.parse::<u64>().unwrap_or(0));
        (n.next().unwrap_or(0), n.next().unwrap_or(0))
    };
    if behind == 0 && !fetch_old {
        return None;
    }
    let note = if behind > 0 {
        format!("checkout {dir} is {behind} commit(s) behind its upstream as of the last fetch, so these results miss symbols on the remote; git_pull, then re-run")
    } else {
        format!("upstream of {dir} not fetched for over 24 h, so a behind count of 0 is unproven; git_pull to refresh")
    };
    Some(json!({
        "behind": behind,
        "ahead": ahead,
        "head": short(head),
        "upstream_tip": short(upstream),
        "fetched_ago_s": fetched_ago.map(|ms| ms / 1000),
        "note": note,
    }))
}

pub(super) fn stamp_tree_staleness_packed(body: &Value, packed: u64) -> u64 {
    let Some(stale) = tree_staleness(body) else {
        return packed;
    };
    let mut v = unpack_to_value(packed);
    let Some(map) = v.as_object_mut() else {
        return packed;
    };
    mark_incomplete(map);
    if let Some(data) = map.get_mut("data").and_then(Value::as_object_mut) {
        mark_incomplete(data);
    }
    map.insert("stale_tree".to_string(), stale);
    pack(v.to_string())
}

fn mark_incomplete(map: &mut serde_json::Map<String, Value>) {
    for key in ["complete", "exhaustive"] {
        if map.contains_key(key) {
            map.insert(key.to_string(), json!(false));
        }
    }
    map.insert("complete".to_string(), json!(false));
    map.insert(
        "complete_reason".to_string(),
        json!("stale_tree: checkout behind upstream or fetch older than 24 h, results may miss remote symbols"),
    );
}
