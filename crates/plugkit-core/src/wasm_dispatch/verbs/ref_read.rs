use serde_json::{json, Value};

use super::git::refuse_unknown_fields;
use super::search::{ok_codesearch, CODESEARCH_EXHAUSTIVE_FIELDS};
use super::{err_coded, ok, ERR_CODE_FAILED};

const DEFAULT_CAP: usize = 200;
const MAX_CAP: usize = 2000;
const DEFINITION_KEYWORDS_ERE: &str = "fn|def|func|function|class|struct|enum|trait|interface|impl|mod|type";
const DEFINITION_KEYWORDS: &[&str] = &[
    "fn", "def", "func", "function", "class", "struct", "enum", "trait", "interface", "impl", "mod", "type",
];
const DEFINITION_MODIFIERS: &[&str] = &["pub", "export", "async", "unsafe", "default", "static", "extern"];
const REF_UNSUPPORTED_CODESEARCH_KEYS: &[&str] = &[
    "glob",
    "path_glob",
    "exclude",
    "exclude_glob",
    "exclude_globs",
    "files",
    "no_ignore",
    "include_ignored",
    "comments_only",
    "refresh",
    "no_cache",
    "force_disk",
    "timeout_ms",
    "output",
    "combine",
    "term_combination",
    "verbatim",
    "regex",
    "literal",
    "pattern",
    "root",
    "projectPath",
];

pub(super) fn codesearch_at_ref(body: &Value) -> Option<u64> {
    let requested = requested_ref(body)?;
    if let Some(refusal) = refuse_unknown_fields("codesearch", body, CODESEARCH_EXHAUSTIVE_FIELDS) {
        return Some(refusal);
    }
    Some(match codesearch_at_ref_data(body, requested) {
        Ok(data) => ok_codesearch(data),
        Err(reason) => err_coded("codesearch", ERR_CODE_FAILED, &reason),
    })
}

pub(super) fn codeinsight_at_ref(verb: &str, action: &str, body: &Value) -> Option<u64> {
    let requested = requested_ref(body)?;
    Some(match codeinsight_at_ref_data(action, body, requested) {
        Ok(data) => ok(verb, data),
        Err(reason) => err_coded(verb, ERR_CODE_FAILED, &reason),
    })
}

fn requested_ref(body: &Value) -> Option<Result<String, String>> {
    let raw = body.get("ref")?;
    if raw.is_null() {
        return None;
    }
    Some(match raw.as_str().map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err("ref must be a non-empty string naming a git revision: a sha, a branch such as origin/main, or a ref name".to_string()),
    })
}

fn safe_rev(rev: &str) -> bool {
    !rev.starts_with('-')
        && !rev.contains("..")
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-' | '~' | '^' | '@'))
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && !path.starts_with('/')
        && !path.contains("..")
        && !path.contains('\n')
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn text_arg<'a>(body: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        body.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    })
}

fn pathspec_of(body: &Value, keys: &[&str]) -> Result<Vec<String>, String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(single) = text_arg(body, keys) {
        paths.push(single.to_string());
    }
    if let Some(list) = body.get("paths").and_then(Value::as_array) {
        for item in list {
            let text = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "paths entries must be non-empty strings".to_string())?;
            paths.push(text.to_string());
        }
    }
    if let Some(bad) = paths.iter().find(|path| !safe_path(path.as_str())) {
        return Err(format!("path {bad} is not a safe repository-relative path"));
    }
    Ok(paths)
}

fn repo_of(path: &str) -> (Option<String>, String) {
    let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    if let Some((head, rest)) = path.split_once('/') {
        if !head.is_empty() && !rest.is_empty() && !root.is_empty() && crate::pkfs::exists(&format!("{root}/{head}/.git")) {
            return (Some(head.to_string()), rest.to_string());
        }
    }
    (None, path.to_string())
}

fn scoped_paths(paths: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut repo: Option<String> = None;
    let mut inner = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let (dir, rest) = repo_of(path);
        if index > 0 && dir != repo {
            return Err(format!(
                "path {path} lies in another repository than the earlier paths; run one ref search per repository"
            ));
        }
        repo = dir;
        inner.push(rest);
    }
    Ok((repo, inner))
}

fn scope_cwd(repo: &Option<String>) -> Option<String> {
    repo.as_ref().map(|dir| {
        format!(
            "{}/{}",
            crate::wasm_dispatch::host_cwd_string().unwrap_or_default(),
            dir
        )
    })
}

fn cap_of(body: &Value) -> usize {
    ["max_matches", "max_results", "maxResults", "limit", "head_limit", "k"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_u64))
        .map(|n| (n as usize).clamp(1, MAX_CAP))
        .unwrap_or(DEFAULT_CAP)
}

fn resolve(rev: &str, cwd: Option<&str>) -> Result<String, String> {
    let spec = format!("{rev}^{{commit}}");
    let argv = ["rev-parse", "--verify", "--quiet", spec.as_str()];
    let result = crate::wasm_dispatch::git_call_argv(&argv, cwd);
    let exit = result.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
    let sha = result
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if exit == 0 && sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(format!(
            "ref {rev} does not resolve to a commit in the repository holding the path (git rev-parse exit {exit}); a revision that exists only on origin needs git_fetch in that repository first"
        ))
    }
}

struct Hit {
    path: String,
    line: u64,
    text: String,
}

fn grep_at(
    sha: &str,
    mode_flag: &str,
    flags: &[&str],
    pattern: &str,
    pathspec: &[String],
    cwd: Option<&str>,
) -> Result<Vec<Hit>, String> {
    let mut argv: Vec<&str> = vec!["grep", "-n", "--no-color", "-I", mode_flag];
    argv.extend_from_slice(flags);
    argv.extend_from_slice(&["-e", pattern, sha, "--"]);
    argv.extend(pathspec.iter().map(String::as_str));
    let result = crate::wasm_dispatch::git_call_argv(&argv, cwd);
    let exit = result.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
    if exit == 1 {
        return Ok(Vec::new());
    }
    if exit != 0 {
        let stderr = result.get("stderr").and_then(Value::as_str).unwrap_or("").trim();
        return Err(format!("git grep at {sha} exited {exit}: {stderr}"));
    }
    let stdout = result.get("stdout").and_then(Value::as_str).unwrap_or("");
    let prefix = format!("{sha}:");
    let mut hits = Vec::new();
    for raw in stdout.lines() {
        let Some(rest) = raw.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let mut parts = rest.splitn(3, ':');
        let (Some(path), Some(number), Some(text)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(line) = number.parse::<u64>() else {
            continue;
        };
        hits.push(Hit {
            path: path.to_string(),
            line,
            text: text.to_string(),
        });
    }
    Ok(hits)
}

fn hit_values(hits: &[Hit], cap: usize) -> (Vec<Value>, bool) {
    let truncated = hits.len() > cap;
    let values = hits
        .iter()
        .take(cap)
        .map(|hit| json!({"path": hit.path, "line": hit.line, "text": hit.text}))
        .collect();
    (values, truncated)
}

fn codesearch_at_ref_data(body: &Value, requested: Result<String, String>) -> Result<Value, String> {
    let rev = requested?;
    if !safe_rev(&rev) {
        return Err(format!("ref {rev} is not a plain revision name"));
    }
    let unsupported: Vec<&str> = REF_UNSUPPORTED_CODESEARCH_KEYS
        .iter()
        .copied()
        .filter(|key| body.get(*key).is_some_and(|value| !value.is_null()))
        .collect();
    if !unsupported.is_empty() {
        return Err(format!(
            "codesearch with ref does not support {}: a ref search is one git grep at one revision, so these options are refused rather than ignored",
            unsupported.join(", ")
        ));
    }
    let query = text_arg(body, &["query"]).ok_or_else(|| "codesearch with ref needs a non-empty query".to_string())?;
    let mode = text_arg(body, &["mode"]).unwrap_or("literal");
    let mode_flag = match mode {
        "literal" => "-F",
        "regex" => "-E",
        other => {
            return Err(format!(
                "codesearch with ref supports mode literal or regex, not {other}: dual, filename and comments read the index, which describes the worktree"
            ))
        }
    };
    let mut flags: Vec<&str> = Vec::new();
    if body.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false) {
        flags.push("-i");
    }
    if body.get("whole_word").and_then(Value::as_bool).unwrap_or(false) {
        flags.push("-w");
    }
    let (repo, pathspec) = scoped_paths(&pathspec_of(body, &["path"])?)?;
    let cwd = scope_cwd(&repo);
    let sha = resolve(&rev, cwd.as_deref())?;
    let cap = cap_of(body);
    let hits = grep_at(&sha, mode_flag, &flags, query, &pathspec, cwd.as_deref())?;
    let total = hits.len();
    let (matches, truncated) = hit_values(&hits, cap);
    Ok(json!({
        "ref": rev,
        "ref_sha": sha,
        "repo": repo.clone().unwrap_or_else(|| ".".to_string()),
        "mode": mode,
        "query": query,
        "pathspec": pathspec,
        "file_source": "git_ref",
        "file_source_detail": "git grep at ref_sha in repo; the worktree and index are not read",
        "match_count": total,
        "returned": matches.len(),
        "matches": matches,
        "truncated": truncated,
        "exhaustive": !truncated,
        "complete": !truncated,
    }))
}

fn codeinsight_at_ref_data(action: &str, body: &Value, requested: Result<String, String>) -> Result<Value, String> {
    let rev = requested?;
    if !safe_rev(&rev) {
        return Err(format!("ref {rev} is not a plain revision name"));
    }
    match action {
        "find" => find_at(&rev, body),
        "outline" => outline_at(&rev, body),
        other => Err(format!(
            "codeinsight with ref supports action find (symbol) and outline (path), not {other}: callers, callees, impact, overview and the other graph actions read the index, which describes the worktree"
        )),
    }
}

fn find_at(rev: &str, body: &Value) -> Result<Value, String> {
    let symbol = text_arg(body, &["symbol", "name"])
        .ok_or_else(|| "codeinsight find with ref needs symbol (or name)".to_string())?;
    if !is_identifier(symbol) {
        return Err(format!("symbol {symbol} is not an identifier"));
    }
    if text_arg(body, &["kind"]).is_some() {
        return Err("codeinsight find with ref does not take kind: the definition match is keyword based, so kind is refused rather than ignored".to_string());
    }
    let (repo, pathspec) = scoped_paths(&pathspec_of(body, &["path", "path_prefix"])?)?;
    let cwd = scope_cwd(&repo);
    let sha = resolve(rev, cwd.as_deref())?;
    let pattern = format!(
        "(^|[^[:alnum:]_])({}) +{}([^[:alnum:]_]|$)",
        DEFINITION_KEYWORDS_ERE, symbol
    );
    let cap = cap_of(body);
    let hits = grep_at(&sha, "-E", &[], &pattern, &pathspec, cwd.as_deref())?;
    let total = hits.len();
    let (matches, truncated) = hit_values(&hits, cap);
    Ok(json!({
        "ref": rev,
        "ref_sha": sha,
        "repo": repo.clone().unwrap_or_else(|| ".".to_string()),
        "action": "find",
        "symbol": symbol,
        "method": "heuristic: a definition keyword followed by the symbol, matched by git grep -E at ref_sha; not a parser",
        "approximate": true,
        "pathspec": pathspec,
        "file_source": "git_ref",
        "file_source_detail": "git grep at ref_sha in repo; the worktree and index are not read",
        "match_count": total,
        "returned": matches.len(),
        "matches": matches,
        "truncated": truncated,
        "exhaustive": !truncated,
    }))
}

fn definition_name(line: &str) -> Option<&str> {
    let mut tokens = line.split_whitespace();
    let mut token = tokens.next()?;
    while DEFINITION_MODIFIERS.contains(&token) || token.starts_with("pub(") {
        token = tokens.next()?;
    }
    if !DEFINITION_KEYWORDS.contains(&token) {
        return None;
    }
    let name = tokens
        .next()?
        .split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .next()?;
    if is_identifier(name) {
        Some(name)
    } else {
        None
    }
}

fn outline_at(rev: &str, body: &Value) -> Result<Value, String> {
    let path = text_arg(body, &["path", "file"])
        .ok_or_else(|| "codeinsight outline with ref needs path (or file)".to_string())?;
    if !safe_path(path) {
        return Err(format!("path {path} is not a safe repository-relative path"));
    }
    let (repo, inner) = scoped_paths(&[path.to_string()])?;
    let inner_path = inner.into_iter().next().unwrap_or_default();
    let cwd = scope_cwd(&repo);
    let sha = resolve(rev, cwd.as_deref())?;
    let spec = format!("{sha}:{inner_path}");
    let argv = ["show", spec.as_str()];
    let result = crate::wasm_dispatch::git_call_argv(&argv, cwd.as_deref());
    let exit = result.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
    if exit != 0 {
        let stderr = result.get("stderr").and_then(Value::as_str).unwrap_or("").trim();
        return Err(format!("git show {spec} exited {exit}: {stderr}"));
    }
    let content = result.get("stdout").and_then(Value::as_str).unwrap_or("");
    let cap = cap_of(body);
    let mut definitions: Vec<Value> = Vec::new();
    let mut definition_count = 0usize;
    let mut line_count = 0usize;
    for (index, text) in content.lines().enumerate() {
        line_count = index + 1;
        if let Some(name) = definition_name(text) {
            definition_count += 1;
            if definitions.len() < cap {
                definitions.push(json!({"line": index + 1, "name": name, "text": text.trim()}));
            }
        }
    }
    Ok(json!({
        "ref": rev,
        "ref_sha": sha,
        "repo": repo.unwrap_or_else(|| ".".to_string()),
        "action": "outline",
        "path": path,
        "line_count": line_count,
        "definition_count": definition_count,
        "definitions": definitions,
        "truncated": definition_count > cap,
        "exhaustive": definition_count <= cap,
        "approximate": true,
        "method": "heuristic: a line whose first token after modifiers is a definition keyword; not a parser",
        "file_source": "git_ref",
        "file_source_detail": "git show at ref_sha in repo; the worktree is not read",
    }))
}
