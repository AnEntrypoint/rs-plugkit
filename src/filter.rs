use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn dispatch(body: &Value, raw: &str) -> (Value, Option<String>) {
    let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let input = body.get("input").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| raw.to_string());
    match kind {
        "grep" => Ok(grep(&input, body)),
        "ls" => Ok(ls(&input, body)),
        "tree" => Ok(tree(&input, body)),
        "json" => Ok(json_compact(&input, body)),
        "diff" => Ok(diff(&input, body)),
        "git-status" => Ok(git_status(&input)),
        "log" => Ok(log_dedup(&input, body)),
        "" => Err("kind required (grep|ls|tree|json|diff|git-status|log)".to_string()),
        other => Err(format!("unknown filter kind: {}", other)),
    }.map(|v| (v, None)).unwrap_or_else(|e| (Value::Null, Some(e)))
}

fn grep(input: &str, body: &Value) -> Value {
    let max_line = body.get("maxLineChars").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
    let max_per_file = body.get("maxPerFile").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let mut by_file: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut plain: Vec<String> = Vec::new();
    let mut total_in = 0usize;
    let mut truncated = 0usize;
    for line in input.lines() {
        total_in += 1;
        let mut parts = line.splitn(3, ':');
        let (file, lineno, rest) = match (parts.next(), parts.next(), parts.next()) {
            (Some(f), Some(n), Some(r)) if n.chars().all(|c| c.is_ascii_digit()) => (f, n, r),
            _ => { plain.push(truncate(line.trim_end(), max_line)); continue; }
        };
        let entry = by_file.entry(file.to_string()).or_default();
        if entry.len() >= max_per_file { truncated += 1; continue; }
        entry.push((lineno.to_string(), truncate(rest.trim(), max_line)));
    }
    let mut out = String::new();
    for (file, hits) in &by_file {
        out.push_str(file);
        out.push('\n');
        for (n, body) in hits {
            out.push_str(&format!("  {}: {}\n", n, body));
        }
    }
    for line in &plain {
        out.push_str(line);
        out.push('\n');
    }
    let in_bytes = input.len();
    let out_bytes = out.len();
    json!({
        "output": out,
        "stats": {
            "files": by_file.len(),
            "lines_in": total_in,
            "lines_out": by_file.values().map(|v| v.len()).sum::<usize>() + plain.len(),
            "truncated_per_file": truncated,
            "bytes_in": in_bytes,
            "bytes_out": out_bytes,
            "saved_pct": pct_saved(in_bytes, out_bytes)
        }
    })
}

fn ls(input: &str, body: &Value) -> Value {
    let max_entries = body.get("maxEntries").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
    let mut entries: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = entries.len();
    let mut truncated = 0usize;
    if entries.len() > max_entries {
        truncated = entries.len() - max_entries;
        entries.truncate(max_entries);
    }
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for e in entries {
        let s = e.trim_end();
        if s.ends_with('/') || s.ends_with('\\') { dirs.push(s.to_string()); }
        else { files.push(s.to_string()); }
    }
    let mut out = String::new();
    if !dirs.is_empty() {
        out.push_str(&format!("dirs ({}): {}\n", dirs.len(), dirs.join(" ")));
    }
    if !files.is_empty() {
        out.push_str(&format!("files ({}): {}\n", files.len(), files.join(" ")));
    }
    if truncated > 0 {
        out.push_str(&format!("... +{} more entries truncated\n", truncated));
    }
    json!({
        "output": out,
        "stats": {
            "total": total,
            "dirs": dirs.len(),
            "files": files.len(),
            "truncated": truncated,
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len())
        }
    })
}

fn tree(input: &str, body: &Value) -> Value {
    let max_depth = body.get("maxDepth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
    let max_lines = body.get("maxLines").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
    let mut out_lines: Vec<String> = Vec::new();
    let mut skipped_depth = 0usize;
    let mut skipped_cap = 0usize;
    for line in input.lines() {
        let depth = line.chars().take_while(|c| matches!(c, ' ' | '\u{2502}' | '\u{251C}' | '\u{2514}' | '\u{2500}' | '|' | '`' | '-')).count() / 4;
        if depth > max_depth { skipped_depth += 1; continue; }
        if out_lines.len() >= max_lines { skipped_cap += 1; continue; }
        out_lines.push(line.trim_end().to_string());
    }
    let mut out = out_lines.join("\n");
    if skipped_depth > 0 || skipped_cap > 0 {
        out.push_str(&format!("\n... pruned {} deep + {} over-cap lines", skipped_depth, skipped_cap));
    }
    out.push('\n');
    json!({
        "output": out,
        "stats": {
            "lines_in": input.lines().count(),
            "lines_out": out_lines.len(),
            "pruned_depth": skipped_depth,
            "pruned_cap": skipped_cap,
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len())
        }
    })
}

fn json_compact(input: &str, body: &Value) -> Value {
    let keys_only = body.get("keysOnly").and_then(|v| v.as_bool()).unwrap_or(false);
    let parsed: serde_json::Result<Value> = serde_json::from_str(input.trim());
    let out = match parsed {
        Ok(v) => {
            if keys_only { value_keys_only(&v).to_string() } else { v.to_string() }
        }
        Err(e) => format!("// json parse error: {}\n{}", e, input.trim()),
    };
    json!({
        "output": out,
        "stats": {
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len()),
            "keys_only": keys_only
        }
    })
}

fn value_keys_only(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, vv) in m {
                out.insert(k.clone(), value_keys_only(vv));
            }
            Value::Object(out)
        }
        Value::Array(a) => {
            if let Some(first) = a.first() {
                Value::Array(vec![value_keys_only(first), Value::String(format!("...+{}", a.len().saturating_sub(1)))])
            } else { Value::Array(vec![]) }
        }
        Value::String(_) => Value::String("<str>".to_string()),
        Value::Number(_) => Value::String("<num>".to_string()),
        Value::Bool(_) => Value::String("<bool>".to_string()),
        Value::Null => Value::Null,
    }
}

fn diff(input: &str, _body: &Value) -> Value {
    let mut out = String::new();
    let mut current_file: Option<String> = None;
    let mut adds = 0usize;
    let mut dels = 0usize;
    let mut hunks = 0usize;
    for line in input.lines() {
        if let Some(f) = line.strip_prefix("+++ b/") {
            current_file = Some(f.to_string());
            out.push_str(&format!("\n=== {} ===\n", f));
        } else if line.starts_with("@@") {
            hunks += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            if !rest.starts_with("++") {
                adds += 1;
                out.push_str(&format!("+ {}\n", rest.trim_end()));
            }
        } else if let Some(rest) = line.strip_prefix('-') {
            if !rest.starts_with("--") {
                dels += 1;
                out.push_str(&format!("- {}\n", rest.trim_end()));
            }
        }
    }
    let _ = current_file;
    json!({
        "output": out,
        "stats": {
            "hunks": hunks,
            "additions": adds,
            "deletions": dels,
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len())
        }
    })
}

fn git_status(input: &str) -> Value {
    let mut staged: Vec<String> = Vec::new();
    let mut unstaged: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    for line in input.lines() {
        if line.len() < 3 { continue; }
        let (x, y, path) = (line.chars().nth(0).unwrap_or(' '),
                             line.chars().nth(1).unwrap_or(' '),
                             line.get(3..).unwrap_or("").to_string());
        if x == '?' && y == '?' { untracked.push(path); continue; }
        if x != ' ' && x != '?' { staged.push(format!("{} {}", x, path.clone())); }
        if y != ' ' && y != '?' { unstaged.push(format!("{} {}", y, path)); }
    }
    let mut out = String::new();
    if !staged.is_empty() { out.push_str(&format!("staged ({}):\n  {}\n", staged.len(), staged.join("\n  "))); }
    if !unstaged.is_empty() { out.push_str(&format!("unstaged ({}):\n  {}\n", unstaged.len(), unstaged.join("\n  "))); }
    if !untracked.is_empty() { out.push_str(&format!("untracked ({}):\n  {}\n", untracked.len(), untracked.join("\n  "))); }
    if out.is_empty() { out.push_str("clean\n"); }
    json!({
        "output": out,
        "stats": {
            "staged": staged.len(),
            "unstaged": unstaged.len(),
            "untracked": untracked.len(),
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len())
        }
    })
}

fn log_dedup(input: &str, body: &Value) -> Value {
    let max_lines = body.get("maxLines").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in input.lines() {
        let key = normalize_log_line(line);
        if !counts.contains_key(&key) { order.push(key.clone()); }
        *counts.entry(key).or_insert(0) += 1;
    }
    let mut out = String::new();
    let mut emitted = 0usize;
    for key in &order {
        if emitted >= max_lines { break; }
        let c = counts[key];
        if c > 1 { out.push_str(&format!("[{}x] {}\n", c, key)); }
        else { out.push_str(&format!("{}\n", key)); }
        emitted += 1;
    }
    json!({
        "output": out,
        "stats": {
            "unique": order.len(),
            "lines_in": input.lines().count(),
            "lines_out": emitted,
            "bytes_in": input.len(),
            "bytes_out": out.len(),
            "saved_pct": pct_saved(input.len(), out.len())
        }
    })
}

fn normalize_log_line(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let mut in_num = false;
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !in_num { s.push('N'); in_num = true; }
        } else {
            in_num = false;
            s.push(c);
        }
    }
    s.trim().to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("{}...", &s[..max.saturating_sub(3)]) }
}

fn pct_saved(input: usize, output: usize) -> u64 {
    if input == 0 { return 0; }
    let saved = input.saturating_sub(output);
    ((saved as f64 / input as f64) * 100.0) as u64
}
