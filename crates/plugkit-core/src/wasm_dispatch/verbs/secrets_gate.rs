use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

const RULES: &[(&str, &str)] = &[
    ("aws-access-key-id", r"\b(?:\x41KIA|ASIA)[0-9A-Z]{16}\b"),
    ("private-key-pem", r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE\x20KEY-----"),
    (
        "github-token",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})\b",
    ),
    ("slack-token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
    ("sk-style-key", r"\bsk-[A-Za-z0-9_-]{32,}"),
    (
        "url-inline-password",
        r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s/:@'"]+:([^\s/@'"]{3,})@[^\s'"]+"#,
    ),
    (
        "assigned-secret-literal",
        r#"(?i)\b(?:api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token|password)\s*[:=]\s*(?:["'][A-Za-z0-9/+_=.-]{20,}["']|[A-Za-z0-9/+_=.-]{20,})"#,
    ),
];

struct Rule {
    name: &'static str,
    re: Regex,
}

fn compiled_rules() -> Result<&'static [Rule], String> {
    static COMPILED: OnceLock<Result<Vec<Rule>, String>> = OnceLock::new();
    COMPILED
        .get_or_init(|| {
            RULES
                .iter()
                .map(|&(name, pattern)| {
                    Regex::new(pattern)
                        .map(|re| Rule { name, re })
                        .map_err(|err| format!("{name}: {err}"))
                })
                .collect()
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

fn hunk_new_start(line: &str) -> Option<usize> {
    let after_minus = line.strip_prefix("@@ -")?;
    let after_plus = after_minus.split_once(" +")?.1;
    let digits: String = after_plus.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn first_rule(text: &str, rules: &[Rule]) -> Option<&'static str> {
    rules.iter().find_map(|rule| {
        let caps = rule.re.captures(text)?;
        if rule.name == "url-inline-password" {
            let secret = caps.get(1).map_or("", |m| m.as_str());
            if matches!(secret.chars().next(), Some('$' | '<' | '{' | '%')) {
                return None;
            }
        }
        Some(rule.name)
    })
}

struct Scan {
    files: usize,
    added_lines: usize,
    findings: Vec<String>,
}

fn scan_added_lines(diff: &str, rules: &[Rule]) -> Scan {
    let mut scan = Scan {
        files: 0,
        added_lines: 0,
        findings: Vec::new(),
    };
    let mut file = String::from("(unknown)");
    let mut line_no = 0usize;
    let mut in_hunk = false;
    for raw in diff.split('\n') {
        if raw.starts_with("diff --git ") {
            in_hunk = false;
            continue;
        }
        let header = if in_hunk { None } else { raw.strip_prefix("+++ ") };
        if let Some(header) = header {
            file = header.strip_prefix("b/").unwrap_or(header).to_string();
            scan.files += 1;
            continue;
        }
        if let Some(start) = hunk_new_start(raw) {
            in_hunk = true;
            line_no = start;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(content) = raw.strip_prefix('+') {
            scan.added_lines += 1;
            if let Some(rule) = first_rule(content, rules) {
                scan.findings.push(format!("{rule} {file}:{line_no}"));
            }
            line_no += 1;
        }
    }
    scan
}

pub(super) struct StagedGate {
    pub(super) tree: String,
    files: usize,
    added_lines: usize,
}

impl StagedGate {
    pub(super) fn report(&self) -> Value {
        json!({
            "result": "PASS",
            "tree": self.tree,
            "files": self.files,
            "added_lines": self.added_lines,
            "rules": RULES.len(),
        })
    }
}

fn complete_stdout(reply: &Value) -> Option<String> {
    let exited_zero = reply.get("exit_code").and_then(Value::as_i64) == Some(0);
    let truncated = ["stdout_truncated", "stderr_truncated"]
        .iter()
        .any(|field| reply.get(*field).and_then(Value::as_bool) == Some(true));
    if !exited_zero || truncated {
        return None;
    }
    reply.get("stdout").and_then(Value::as_str).map(str::to_owned)
}

fn unreadable(detail: &str) -> Value {
    json!({
        "error": format!("the secrets gate could not read the staged set: {detail}; nothing was committed"),
        "error_code": "secrets_gate_unreadable",
        "gate": "secrets",
        "gate_denied": true,
    })
}

fn tree_at(
    step: &mut dyn FnMut(&[&str]) -> Result<Value, u64>,
    argv: &[&str],
    what: &str,
) -> Result<Result<String, Value>, u64> {
    let reply = step(argv)?;
    Ok(match complete_stdout(&reply).map(|out| out.trim().to_owned()) {
        Some(tree) if !tree.is_empty() => Ok(tree),
        _ => Err(unreadable(&format!("{what} did not return a tree"))),
    })
}

pub(super) fn staged_tree(
    step: &mut dyn FnMut(&[&str]) -> Result<Value, u64>,
) -> Result<Result<String, Value>, u64> {
    tree_at(step, &["write-tree"], "git write-tree")
}

pub(super) fn committed_tree(
    step: &mut dyn FnMut(&[&str]) -> Result<Value, u64>,
) -> Result<Result<String, Value>, u64> {
    tree_at(step, &["rev-parse", "HEAD^{tree}"], "git rev-parse HEAD^{tree}")
}

pub(super) fn gate_staged_index(
    step: &mut dyn FnMut(&[&str]) -> Result<Value, u64>,
) -> Result<Result<StagedGate, Value>, u64> {
    let rules = match compiled_rules() {
        Ok(rules) => rules,
        Err(detail) => {
            return Ok(Err(unreadable(&format!(
                "the rules did not compile ({detail})"
            ))))
        }
    };
    let tree = match staged_tree(step)? {
        Ok(tree) => tree,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let reply = step(&[
        "diff",
        "--cached",
        "-U0",
        "--no-color",
        "--no-ext-diff",
        "--text",
    ])?;
    let Some(diff) = complete_stdout(&reply) else {
        return Ok(Err(unreadable(
            "git diff --cached did not return the complete staged diff",
        )));
    };
    let scan = scan_added_lines(&diff, rules);
    if !scan.findings.is_empty() {
        return Ok(Err(json!({
            "error": format!(
                "the secrets gate refused the staged set: {} staged line(s) match a credential shape; remove the value, load it from an env var or secret store, re-stage, and commit again",
                scan.findings.len()
            ),
            "error_code": crate::wasm_dispatch::ERR_CODE_GATE_DENIED,
            "gate": "secrets",
            "gate_denied": true,
            "findings": scan.findings,
            "staged_tree": tree,
        })));
    }
    Ok(Ok(StagedGate {
        tree,
        files: scan.files,
        added_lines: scan.added_lines,
    }))
}

pub(super) fn tree_moved(gated: &str, now: &str, committed_sha: Option<&str>) -> Value {
    let error = match committed_sha {
        Some(sha) => format!(
            "commit {sha} holds tree {now}, but the secrets gate passed tree {gated}; inspect the commit before pushing"
        ),
        None => format!(
            "the staged tree is now {now}, but the secrets gate passed {gated}: the staged bytes changed after the gate, so nothing was committed; stage the set again and commit"
        ),
    };
    json!({
        "error": error,
        "error_code": "secrets_gate_tree_moved",
        "gate": "secrets",
        "gate_denied": true,
        "gated_tree": gated,
        "tree": now,
        "committed_sha_full": committed_sha,
    })
}
