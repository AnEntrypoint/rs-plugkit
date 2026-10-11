use crate::comment_spans::ByteSpan;
use serde_json::Value;
#[cfg(target_arch = "wasm32")]
use serde_json::json;

pub const DEFAULT_MAX_FILES: usize = 20_000;
pub const DEFAULT_FINDING_LIMIT: usize = 2_000;
pub const MAX_TEXT_CHARS: usize = 240;
pub const COMMENT_KIND: &str = "comment";
pub const DOCSTRING_KIND: &str = "docstring";

#[derive(serde::Serialize)]
pub struct CommentFinding {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub enclosing: String,
    pub kind: String,
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle_audit(_content: &str) -> (String, String, i32) {
    (
        "{\"ok\":false,\"error\":\"comment-audit requires wasm32\"}".to_string(),
        String::new(),
        1,
    )
}

#[cfg(target_arch = "wasm32")]
pub fn handle_audit(content: &str) -> (String, String, i32) {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let root = requested_root(&body);
    let max_files = positive_usize(&body, &["max_files", "maxFiles"], DEFAULT_MAX_FILES);
    let limit = positive_usize(&body, &["limit", "max_results", "maxResults"], DEFAULT_FINDING_LIMIT);
    let kind_filter = body
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let mut cfg = crate::ragconfig::IndexConfig::default();
    cfg.digest_max_files = max_files;
    let files = crate::scan_universe::project_source_files(&root, max_files, &cfg);
    let mut findings: Vec<CommentFinding> = Vec::new();
    let mut scanned = 0usize;
    let mut unreadable = 0usize;
    for path in &files {
        if !crate::comment_spans::has_comment_syntax(path) {
            continue;
        }
        let text = match crate::pkfs::read_to_string(path) {
            Some(text) => text,
            None => {
                unreadable += 1;
                continue;
            }
        };
        scanned += 1;
        let shown = display_path(path, &root);
        for finding in scan_source(&shown, &text) {
            if let Some(want) = kind_filter.as_deref() {
                if finding.kind != want {
                    continue;
                }
            }
            findings.push(finding);
        }
    }
    let total = findings.len();
    let truncated = total > limit;
    findings.truncate(limit);
    let payload = json!({
        "ok": true,
        "verb": "comment-audit",
        "root": root,
        "files_scanned": scanned,
        "files_unreadable": unreadable,
        "findings_total": total,
        "truncated": truncated,
        "findings": findings,
    });
    (payload.to_string(), String::new(), 0)
}

pub fn requested_root(body: &Value) -> String {
    for key in ["root", "projectPath", "cwd"] {
        if let Some(value) = body.get(key).and_then(|v| v.as_str()) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    ".".to_string()
}

pub fn positive_usize(body: &Value, keys: &[&str], fallback: usize) -> usize {
    for key in keys {
        if let Some(value) = body.get(key).and_then(|v| v.as_u64()) {
            if value > 0 {
                return value as usize;
            }
        }
        if let Some(value) = body
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            if value > 0 {
                return value;
            }
        }
    }
    fallback
}

pub fn display_path(path: &str, root: &str) -> String {
    let path = path.replace('\\', "/");
    let root = root.replace('\\', "/").trim_end_matches('/').to_string();
    if !root.is_empty() && root != "." {
        let prefix = format!("{root}/");
        if let Some(rest) = path.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    path.trim_start_matches("./").to_string()
}

pub fn is_python(path: &str) -> bool {
    extension_of(path) == "py"
}

pub fn is_indent_language(path: &str) -> bool {
    is_python(path)
}

pub fn extension_of(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => "",
    }
}

pub fn line_ranges(source: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut start = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            out.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        out.push((start, bytes.len()));
    }
    out
}

pub fn line_of(ranges: &[(usize, usize)], offset: usize) -> usize {
    ranges.partition_point(|(_, end)| *end <= offset) + 1
}

pub fn clamp_text(raw: &str) -> String {
    let mut out = String::new();
    let mut len = 0usize;
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
            len += 1;
            pending_space = false;
        }
        out.push(ch);
        len += 1;
        if len >= MAX_TEXT_CHARS {
            break;
        }
    }
    let trimmed = out.trim().to_string();
    if len >= MAX_TEXT_CHARS {
        format!("{trimmed}...")
    } else {
        trimmed
    }
}

pub fn scan_source(path: &str, source: &str) -> Vec<CommentFinding> {
    if !crate::comment_spans::has_comment_syntax(path) {
        return Vec::new();
    }
    let spans = crate::comment_spans::comment_spans(path, source).unwrap_or_default();
    let mut marked: Vec<(usize, usize, &str)> = Vec::new();
    for (start, end) in &spans {
        marked.push((*start, *end, COMMENT_KIND));
    }
    if is_python(path) {
        for (start, end) in python_docstring_spans(source) {
            marked.push((start, end, DOCSTRING_KIND));
        }
    }
    marked.sort_by_key(|item| item.0);
    let ranges = line_ranges(source);
    let names = enclosing_names_in(path, source, &spans);
    let mut out = Vec::with_capacity(marked.len());
    for (start, end, kind) in marked {
        let end = end.min(source.len());
        let start = start.min(end);
        let line = line_of(&ranges, start);
        let enclosing = names.get(line.saturating_sub(1)).cloned().unwrap_or_default();
        out.push(CommentFinding {
            file: path.to_string(),
            line,
            text: clamp_text(&source[start..end]),
            enclosing: if enclosing.is_empty() {
                "-".to_string()
            } else {
                enclosing
            },
            kind: kind.to_string(),
        });
    }
    out
}

pub fn enclosing_names(path: &str, source: &str) -> Vec<String> {
    let spans = crate::comment_spans::comment_spans(path, source).unwrap_or_default();
    enclosing_names_in(path, source, &spans)
}

pub fn enclosing_names_in(path: &str, source: &str, spans: &[ByteSpan]) -> Vec<String> {
    let ranges = line_ranges(source);
    let depths = line_depths(source, spans);
    let indent_based = is_indent_language(path);
    let mut stack: Vec<(i64, String)> = Vec::new();
    let mut out = Vec::with_capacity(ranges.len());
    for (index, (start, end)) in ranges.iter().enumerate() {
        let line = &source[*start..*end];
        let body_start = *start + leading_whitespace(line);
        let body = line.trim_start_matches(|ch| ch == ' ' || ch == '\t');
        if indent_based {
            if body.trim().is_empty() {
                out.push(top_name(&stack));
                continue;
            }
            let indent = indent_width(line) as i64;
            while stack.last().map(|entry| entry.0 >= indent).unwrap_or(false) {
                stack.pop();
            }
            if !crate::comment_spans::span_contains(spans, body_start) {
                if let Some(name) = python_declared_name(body) {
                    stack.push((indent, name));
                }
            }
            out.push(top_name(&stack));
            continue;
        }
        let before = *depths.get(index).unwrap_or(&0);
        let after = *depths.get(index + 1).unwrap_or(&before);
        if !crate::comment_spans::span_contains(spans, body_start) {
            if let Some(name) = declared_name(body) {
                stack.push((before as i64 + 1, name));
            }
        }
        while stack.last().map(|entry| entry.0 > after as i64).unwrap_or(false) {
            stack.pop();
        }
        out.push(top_name(&stack));
    }
    out
}

pub fn line_depths(source: &str, spans: &[ByteSpan]) -> Vec<i32> {
    let bytes = source.as_bytes();
    let mut depths = vec![0i32];
    let mut depth = 0i32;
    let mut index = 0usize;
    let mut span_index = 0usize;
    while index < bytes.len() {
        while span_index < spans.len() && spans[span_index].1 <= index {
            span_index += 1;
        }
        if span_index < spans.len() && spans[span_index].0 <= index {
            index = spans[span_index].1;
            span_index += 1;
            continue;
        }
        let byte = bytes[index];
        match byte {
            b'\n' => {
                depths.push(depth);
                index += 1;
            }
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth -= 1;
                index += 1;
            }
            b'"' | b'`' => index = skip_quoted(source, index),
            b'\'' => index = skip_single_quote(source, index),
            _ => index += 1,
        }
    }
    depths
}

pub fn leading_whitespace(line: &str) -> usize {
    line.chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .count()
}

pub fn indent_width(line: &str) -> usize {
    let mut width = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += 8,
            _ => break,
        }
    }
    width
}

pub fn top_name(stack: &[(i64, String)]) -> String {
    stack.last().map(|entry| entry.1.clone()).unwrap_or_default()
}

pub fn skip_quoted(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\n' => return index,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    index
}

pub fn skip_single_quote(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let next = bytes.get(start + 1).copied();
    match next {
        None => start + 1,
        Some(byte) if byte.is_ascii_alphanumeric() || byte == b'_' => {
            let prev = if start == 0 { b' ' } else { bytes[start - 1] };
            let is_lifetime = matches!(prev, b'&' | b'<' | b',' | b'(' | b':' | b' ' | b'\t');
            if is_lifetime {
                let mut index = start + 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                index
            } else {
                skip_quoted(source, start)
            }
        }
        Some(byte) => {
            if bytes.get(start + 2).copied() == Some(b'\'') {
                start + 3
            } else if byte == b'\\' {
                let mut index = start + 2;
                while index < bytes.len() && bytes[index] != b'\'' && bytes[index] != b'\n' {
                    index += 1;
                }
                (index + 1).min(bytes.len())
            } else {
                start + 1
            }
        }
    }
}

pub fn python_declared_name(line: &str) -> Option<String> {
    let body = line.trim_start();
    let rest = body
        .strip_prefix("async def ")
        .or_else(|| body.strip_prefix("def "))
        .or_else(|| body.strip_prefix("class "))?;
    let name = ident_prefix(rest);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn declared_name(line: &str) -> Option<String> {
    let body = line.trim();
    if body.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = body
        .split(|ch: char| ch.is_whitespace())
        .filter(|token| !token.is_empty())
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let Some(keyword) = keyword_head(token) else {
            continue;
        };
        if keyword == "func" {
            if let Some(next) = tokens.get(index + 1) {
                if next.starts_with('(') {
                    if let Some(name) = go_func_name(body) {
                        return Some(name);
                    }
                    continue;
                }
            }
        }
        for offset in 1..=2 {
            if let Some(candidate) = tokens.get(index + offset) {
                let name = ident_prefix(candidate);
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    fallback_name(body)
}

pub fn keyword_head(token: &str) -> Option<&'static str> {
    let head: String = token
        .chars()
        .take_while(|ch| ch.is_ascii_lowercase())
        .collect();
    match head.as_str() {
        "fn" => Some("fn"),
        "struct" => Some("struct"),
        "enum" => Some("enum"),
        "trait" => Some("trait"),
        "impl" => Some("impl"),
        "mod" => Some("mod"),
        "union" => Some("union"),
        "type" => Some("type"),
        "interface" => Some("interface"),
        "class" => Some("class"),
        "function" => Some("function"),
        "namespace" => Some("namespace"),
        "record" => Some("record"),
        "func" => Some("func"),
        _ => None,
    }
}

pub fn go_func_name(line: &str) -> Option<String> {
    let close = line.find(')')?;
    let name = ident_prefix(line[close + 1..].trim_start());
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn fallback_name(line: &str) -> Option<String> {
    if !line.contains('(') || !line.contains('{') {
        return None;
    }
    let open = line.find('(')?;
    let before = &line[..open];
    let reversed: String = before
        .chars()
        .rev()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
        .collect();
    let name: String = reversed.chars().rev().collect();
    if name.is_empty() || is_control_word(&name) {
        None
    } else {
        Some(name)
    }
}

pub fn is_control_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "for"
            | "while"
            | "switch"
            | "else"
            | "try"
            | "catch"
            | "finally"
            | "do"
            | "return"
            | "match"
            | "loop"
            | "unsafe"
            | "defer"
            | "go"
            | "select"
            | "case"
            | "default"
            | "lock"
            | "using"
            | "fixed"
            | "checked"
            | "unchecked"
            | "new"
            | "when"
            | "where"
    )
}

pub fn ident_prefix(token: &str) -> String {
    token
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
        .collect()
}

pub fn python_docstring_spans(source: &str) -> Vec<ByteSpan> {
    let mut out = Vec::new();
    let mut expect_statement = true;
    let mut skip_until = 0usize;
    for (start, end) in line_ranges(source) {
        if start < skip_until {
            continue;
        }
        let line = &source[start..end];
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let offset = start + leading_whitespace(line);
        if expect_statement {
            if let Some(span) = string_literal_span(source, offset) {
                out.push(span);
                skip_until = span.1;
                expect_statement = false;
                continue;
            }
        }
        expect_statement = is_python_block_opener(body);
    }
    out
}

pub fn is_python_block_opener(body: &str) -> bool {
    let body = body.trim_end();
    if !body.ends_with(':') {
        return false;
    }
    body.starts_with("def ") || body.starts_with("class ") || body.starts_with("async def ")
}

pub fn string_literal_span(source: &str, start: usize) -> Option<ByteSpan> {
    let bytes = source.as_bytes();
    let quote = *bytes.get(start)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    if bytes.get(start + 1) == Some(&quote) && bytes.get(start + 2) == Some(&quote) {
        let mut index = start + 3;
        while index + 3 <= bytes.len() {
            if bytes[index] == quote && bytes[index + 1] == quote && bytes[index + 2] == quote {
                return Some((start, index + 3));
            }
            if bytes[index] == b'\\' {
                index += 2;
                continue;
            }
            index += 1;
        }
        return Some((start, bytes.len()));
    }
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\n' => return None,
            byte if byte == quote => return Some((start, index + 1)),
            _ => index += 1,
        }
    }
    None
}
