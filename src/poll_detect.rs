#![cfg(target_arch = "wasm32")]

use serde_json::Value;
use crate::wasm_dispatch::{host_read, host_write, host_log, host_now_ms};

const SCAN_WINDOW_MS: u64 = 600_000;
const OFFSET_PATH: &str = ".gm/exec-spool/.poll-scan-offset.json";
const TURN_IDLE_MS: u64 = 30_000;

fn log_warn(msg: &str) {
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
}

fn home_dir() -> String {
    if let Ok(s) = std::env::var("HOME") { return s; }
    if let Ok(s) = std::env::var("USERPROFILE") { return s; }
    ".".to_string()
}

fn ms_to_ymd(ms: u64) -> (i64, u32, u32) {
    let days = (ms / 86_400_000) as i64;
    let mut z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

fn today_str(ms: u64) -> String {
    let (y, m, d) = ms_to_ymd(ms);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn ci_contains(hay: &str, needle: &str) -> bool {
    if needle.is_empty() { return true; }
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() { return false; }
    'outer: for i in 0..=(h.len() - n.len()) {
        for j in 0..n.len() {
            let a = h[i + j];
            let b = n[j];
            let al = if a.is_ascii_uppercase() { a + 32 } else { a };
            let bl = if b.is_ascii_uppercase() { b + 32 } else { b };
            if al != bl { continue 'outer; }
        }
        return true;
    }
    false
}

fn strip_heredocs_and_string_literals(command: &str) -> String {
    let mut s = command.to_string();
    s = strip_heredoc(&s, true);
    s = strip_heredoc(&s, false);
    s = strip_quoted_after(&s, "-m ");
    s = strip_quoted_after(&s, "--message ");
    s = strip_quoted_after(&s, "--message=");
    s
}

fn strip_heredoc(s: &str, single_quote: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'<' && bytes[i+1] == b'<' {
            let mut j = i + 2;
            if j < bytes.len() && bytes[j] == b'-' { j += 1; }
            while j < bytes.len() && bytes[j] == b' ' { j += 1; }
            let need_open = if single_quote { b'\'' } else { b'"' };
            let saw_quote = j < bytes.len() && bytes[j] == need_open;
            if saw_quote { j += 1; }
            let tag_start = j;
            while j < bytes.len() && (bytes[j].is_ascii_uppercase() || bytes[j] == b'_') {
                j += 1;
            }
            let tag_end = j;
            if tag_end == tag_start { out.push(bytes[i] as char); i += 1; continue; }
            if saw_quote {
                if j >= bytes.len() || bytes[j] != need_open { out.push(bytes[i] as char); i += 1; continue; }
                j += 1;
            } else if single_quote {
                out.push(bytes[i] as char); i += 1; continue;
            }
            let tag = &s[tag_start..tag_end];
            if let Some(end_idx) = find_terminator(&s[j..], tag) {
                i = j + end_idx + tag.len();
                continue;
            } else {
                return out;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_terminator(s: &str, tag: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let tag_b = tag.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let start = i + 1;
            if start + tag_b.len() <= bytes.len() && &bytes[start..start+tag_b.len()] == tag_b {
                let after = start + tag_b.len();
                if after == bytes.len() || bytes[after] == b'\n' || bytes[after] == b'\r' {
                    return Some(start);
                }
            }
        }
        i += 1;
    }
    None
}

fn strip_quoted_after(s: &str, marker: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find(marker) {
            None => { out.push_str(rest); return out; }
            Some(idx) => {
                out.push_str(&rest[..idx]);
                out.push_str(marker);
                let after = &rest[idx + marker.len()..];
                let bytes = after.as_bytes();
                if bytes.is_empty() { return out; }
                let q = bytes[0];
                if q != b'\'' && q != b'"' {
                    rest = after;
                    continue;
                }
                let mut j = 1usize;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() { j += 2; continue; }
                    if bytes[j] == q { j += 1; break; }
                    j += 1;
                }
                out.push_str("STR");
                rest = &after[j..];
            }
        }
    }
}

fn references_spool(cmd: &str) -> bool {
    ci_contains(cmd, ".gm/exec-spool")
        || ci_contains(cmd, ".gm\\exec-spool")
        || ci_contains(cmd, ".gm/spool")
        || ci_contains(cmd, ".gm\\spool")
}

fn has_word(hay: &str, word: &str) -> bool {
    let h = hay.as_bytes();
    let w = word.as_bytes();
    if w.is_empty() || w.len() > h.len() { return false; }
    'outer: for i in 0..=(h.len() - w.len()) {
        let before_ok = i == 0 || !is_word_byte(h[i-1]);
        if !before_ok { continue; }
        for j in 0..w.len() {
            let a = h[i + j];
            let b = w[j];
            let al = if a.is_ascii_uppercase() { a + 32 } else { a };
            let bl = if b.is_ascii_uppercase() { b + 32 } else { b };
            if al != bl { continue 'outer; }
        }
        let end = i + w.len();
        let after_ok = end == h.len() || !is_word_byte(h[end]);
        if after_ok { return true; }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

const SLEEPS: &[&str] = &["sleep", "Start-Sleep"];
const READS: &[&str] = &[
    "cat","ls","tail","head","find","test","grep","Get-Content","Test-Path",
    "Get-ChildItem","gci","gc","tp","type","less","more","dir","xargs","parallel","fzf"
];
const STATUS_FILES: &[&str] = &[".status.json", ".watcher.log"];
const LOOP_KW: &[&str] = &["while", "until", "for"];

pub fn is_spool_poll_command(cmd: &str) -> Option<&'static str> {
    if cmd.is_empty() { return None; }
    let stripped = strip_heredocs_and_string_literals(cmd);
    if !references_spool(&stripped) { return None; }

    for sf in STATUS_FILES {
        if ci_contains(&stripped, sf) {
            return Some("spool-status-file-read");
        }
    }

    let has_sleep = SLEEPS.iter().any(|w| has_word(&stripped, w));
    let has_read = READS.iter().any(|w| has_word(&stripped, w));
    let has_loop = LOOP_KW.iter().any(|w| has_word(&stripped, w));

    if has_sleep && has_read {
        return Some("sleep-then-read-spool");
    }
    if has_loop && has_sleep {
        return Some("loop-sleep-spool");
    }
    if has_loop && ci_contains(&stripped, "Test-Path") {
        return Some("loop-test-path-spool");
    }
    if has_loop && (ci_contains(&stripped, "-f ") || ci_contains(&stripped, "-e ")) {
        return Some("loop-test-file-spool");
    }
    if has_read {
        return Some("read-spool-direct");
    }
    None
}

fn read_offset() -> (u64, u64, String) {
    let raw = host_read(OFFSET_PATH).unwrap_or_default();
    if raw.is_empty() {
        return (0, 0, String::new());
    }
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let offset = v.get("offset").and_then(|x| x.as_u64()).unwrap_or(0);
    let last_scan = v.get("last_scan_ms").and_then(|x| x.as_u64()).unwrap_or(0);
    let date = v.get("date").and_then(|x| x.as_str()).unwrap_or("").to_string();
    (offset, last_scan, date)
}

fn write_offset(offset: u64, last_scan_ms: u64, date: &str) {
    let body = serde_json::json!({
        "offset": offset,
        "last_scan_ms": last_scan_ms,
        "date": date,
    }).to_string();
    let _ = host_write(OFFSET_PATH, &body);
}

fn extract_command(line: &str) -> Option<(String, &str)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let tool = v.get("tool").and_then(|t| t.as_str()).unwrap_or("");
    if tool != "Bash" { return None; }
    let cmd = v.get("command")
        .or_else(|| v.get("tool_input").and_then(|ti| ti.get("command")))
        .and_then(|c| c.as_str())?;
    Some((cmd.to_string(), tool))
}

fn line_ts(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get("ts").and_then(|t| t.as_u64())
        .or_else(|| v.get("timestamp").and_then(|t| t.as_u64()))
}

pub fn scan_turn_entry(_cwd: &str) {
    let now = unsafe { host_now_ms() };
    let (prev_offset, last_scan, prev_date) = read_offset();

    if last_scan != 0 && now.saturating_sub(last_scan) < TURN_IDLE_MS {
        return;
    }

    let date = today_str(now);
    let log_path = format!("{}/.claude/gm-log/{}/hook.jsonl", home_dir(), date);
    let content = match host_read(&log_path) {
        Some(s) => s,
        None => {
            write_offset(prev_offset, now, &date);
            return;
        }
    };

    let start_offset = if prev_date == date {
        prev_offset.min(content.len() as u64) as usize
    } else {
        0
    };

    let new_content = &content[start_offset..];
    let mut deviation_count = 0u32;

    for line in new_content.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        let (cmd, _tool) = match extract_command(trimmed) {
            Some(t) => t,
            None => continue,
        };
        if let Some(ts) = line_ts(trimmed) {
            if now.saturating_sub(ts) > SCAN_WINDOW_MS { continue; }
        }
        if let Some(pattern) = is_spool_poll_command(&cmd) {
            let cmd_preview: String = cmd.chars().take(160).collect();
            let msg = format!("deviation.spool-poll pattern={} command={}", pattern, cmd_preview);
            log_warn(&msg);
            deviation_count += 1;
            if deviation_count >= 32 { break; }
        }
    }

    write_offset(content.len() as u64, now, &date);
}
