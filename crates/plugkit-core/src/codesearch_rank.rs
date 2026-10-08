use serde_json::{json, Value};

pub const DEFAULT_REFERENCE_CAP: usize = 15;
pub const DEFAULT_DEFINITION_CAP: usize = 8;
const REFERENCES_PER_FILE: usize = 3;
const SNIPPET_CHARS: usize = 110;
const SNIPPET_LINES: usize = 2;
const TOP_FILES: usize = 8;
pub const IDENTIFIER_SCAN_MAX_MATCHES: usize = 1500;
const RRF_OFFSET: f64 = 60.0;
const EXACT_NAME_BONUS: f64 = 0.05;
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "txt", "adoc"];
const DOC_SEGMENTS: &[&str] = &["docs", "doc"];
const LOW_PRIORITY_SEGMENTS: &[&str] = &["test", "tests", "__tests__", "examples", "example", "scripts", "tools", "bench", "benchmarks", "fixtures"];
const GENERATED_SEGMENTS: &[&str] = &["dist", "build", "out", "vendor", "generated", "coverage"];
const CONTROL_WORDS: &[&str] = &["if", "for", "while", "switch", "catch", "return", "function", "await", "typeof"];

pub struct RankOptions {
    pub include_docs: bool,
    pub verbose: bool,
    pub limit: usize,
}

impl RankOptions {
    pub fn from_body(body: &Value, limit: usize) -> RankOptions {
        let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("code");
        let include_docs = body.get("docs").and_then(|v| v.as_bool()).unwrap_or(false) || matches!(kind, "docs" | "all");
        RankOptions {
            include_docs,
            verbose: body.get("verbose").and_then(|v| v.as_bool()).unwrap_or(false),
            limit,
        }
    }
}

pub fn is_identifier(query: &str) -> bool {
    let q = query.trim();
    q.len() >= 3
        && q.len() <= 96
        && q.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !q.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn normalized(path: &str) -> &str {
    path.trim_start_matches("./").trim_start_matches('/')
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    normalized(path).split(['/', '\\'])
}

pub fn is_doc_path(path: &str) -> bool {
    let name = normalized(path).rsplit(['/', '\\']).next().unwrap_or(path);
    let ext_is_doc = name.rsplit_once('.').is_some_and(|(_, e)| DOC_EXTENSIONS.iter().any(|d| e.eq_ignore_ascii_case(d)));
    ext_is_doc || segments(path).any(|s| DOC_SEGMENTS.contains(&s.to_ascii_lowercase().as_str()))
}

fn path_tier(path: &str) -> u8 {
    let lower = normalized(path).to_ascii_lowercase();
    if segments(&lower).any(|s| GENERATED_SEGMENTS.contains(&s)) || lower.ends_with(".min.js") || lower.ends_with(".d.ts") {
        return 2;
    }
    if segments(&lower).any(|s| LOW_PRIORITY_SEGMENTS.contains(&s)) || lower.contains(".test.") || lower.contains(".spec.") {
        return 1;
    }
    0
}

fn squeeze(text: &str, limit: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(limit).collect();
    format!("{cut}...")
}

fn definition_matcher(identifier: &str) -> Option<regex::Regex> {
    let id = regex::escape(identifier);
    let pattern = format!(
        r"(?:(?:^|[^\w$])(?:class|function\*?|fn|struct|enum|trait|interface|type|def|mod|const|let|var|static|macro_rules!)\s+{id}(?:[^\w$]|$))|(?:^\s*(?:(?:export|default|static|async|get|set|pub|public|private|protected)\s+)*{id}\s*\([^)]*\)\s*(?::[^{{]+)?\{{)|(?:(?:^|[^\w$.]){id}\s*[:=]\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*=>|[\w$]+\s*=>|class\b))"
    );
    regex::Regex::new(&pattern).ok()
}

struct LineHit {
    path: String,
    line: u64,
    text: String,
}

fn parse_compact_line(raw: &str) -> Option<LineHit> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let digits = bytes[i + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && bytes.get(i + 1 + digits) == Some(&b':') {
                let line: u64 = raw[i + 1..i + 1 + digits].parse().ok()?;
                let text = raw[i + 2 + digits..].trim_start().to_string();
                return Some(LineHit { path: normalized(&raw[..i]).to_string(), line, text });
            }
        }
        i += 1;
    }
    None
}

fn looks_like_call_not_definition(text: &str, identifier: &str) -> bool {
    let trimmed = text.trim_start();
    CONTROL_WORDS.iter().any(|w| trimmed.starts_with(w) && trimmed[w.len()..].trim_start().starts_with('('))
        && !trimmed.starts_with(identifier)
}

pub fn identifier_report(identifier: &str, compact_lines: &[String], opts: &RankOptions, scan_was_complete: bool) -> Option<Value> {
    let matcher = definition_matcher(identifier);
    let mut definitions: Vec<LineHit> = Vec::new();
    let mut references: Vec<LineHit> = Vec::new();
    let mut docs_hidden = 0usize;
    for raw in compact_lines {
        let Some(hit) = parse_compact_line(raw) else { continue };
        if !opts.include_docs && is_doc_path(&hit.path) {
            docs_hidden += 1;
            continue;
        }
        let is_definition = matcher.as_ref().is_some_and(|m| m.is_match(&hit.text))
            && !looks_like_call_not_definition(&hit.text, identifier);
        if is_definition { definitions.push(hit) } else { references.push(hit) }
    }
    if definitions.is_empty() && references.is_empty() {
        return None;
    }
    let order = |a: &LineHit, b: &LineHit| path_tier(&a.path).cmp(&path_tier(&b.path))
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.line.cmp(&b.line));
    definitions.sort_by(order);
    references.sort_by(order);

    let mut per_file: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for hit in definitions.iter().chain(references.iter()) {
        *per_file.entry(hit.path.clone()).or_insert(0) += 1;
    }
    let total_definitions = definitions.len();
    let total_references = references.len();

    let shown_definitions: Vec<String> = definitions.iter().take(DEFAULT_DEFINITION_CAP)
        .map(|h| format!("{}:{}: {}", h.path, h.line, squeeze(&h.text, SNIPPET_CHARS)))
        .collect();

    let reference_cap = opts.limit.max(DEFAULT_REFERENCE_CAP);
    let mut taken_per_file: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut shown_references: Vec<String> = Vec::new();
    for hit in &references {
        if shown_references.len() >= reference_cap { break; }
        let taken = taken_per_file.entry(hit.path.as_str()).or_insert(0);
        if *taken >= REFERENCES_PER_FILE { continue; }
        *taken += 1;
        shown_references.push(format!("{}:{}: {}", hit.path, hit.line, squeeze(&hit.text, SNIPPET_CHARS)));
    }

    let mut busiest: Vec<(String, usize)> = per_file.into_iter().collect();
    let file_count = busiest.len();
    busiest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top_files: Vec<String> = busiest.into_iter().take(TOP_FILES).map(|(p, n)| format!("{p} ({n})")).collect();

    let mut out = serde_json::Map::new();
    out.insert("mode".into(), json!("symbol"));
    out.insert("query".into(), json!(identifier));
    out.insert("definitions".into(), json!(shown_definitions));
    out.insert("references".into(), json!(shown_references));
    out.insert("counts".into(), json!({
        "definitions": total_definitions,
        "references": total_references,
        "files": file_count,
        "shown_definitions": shown_definitions.len(),
        "shown_references": shown_references.len(),
    }));
    if docs_hidden > 0 {
        out.insert("docs_hidden".into(), json!(docs_hidden));
    }
    if !scan_was_complete {
        out.insert("partial".into(), json!(true));
    }
    if shown_references.len() < total_references {
        out.insert("top_files".into(), json!(top_files));
        out.insert("more".into(), json!("all matches: mode=literal whole_word=true output=compact; docs/commits: docs=true; raw channels: verbose=true"));
    }
    Some(Value::Object(out))
}

struct MergedHit {
    path: String,
    line: u64,
    kind: String,
    name: String,
    snippet: String,
    score: f64,
}

fn hit_location(hit: &Value) -> Option<(String, u64, String, String)> {
    let symbol = hit.get("symbol");
    let pick = |field: &str| symbol.and_then(|s| s.get(field)).or_else(|| hit.get(field));
    let path = pick("path").and_then(|v| v.as_str())?;
    let line = pick("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
    let kind = pick("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = pick("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some((normalized(path).to_string(), line, kind, name))
}

fn hit_snippet(hit: &Value) -> String {
    let text = hit.get("text").or_else(|| hit.get("snippet")).and_then(|v| v.as_str()).unwrap_or("");
    let body_lines = text.lines().skip(if hit.get("text").is_some() { 1 } else { 0 });
    let picked: Vec<String> = body_lines.filter(|l| !l.trim().is_empty()).take(SNIPPET_LINES)
        .map(|l| squeeze(l, SNIPPET_CHARS)).collect();
    picked.join(" | ")
}

pub fn compact_dual(query: &str, raw: &Value, opts: &RankOptions) -> Value {
    let channels = ["bm25_hits", "vector_hits", "hits"];
    let mut merged: Vec<MergedHit> = Vec::new();
    let mut index_by_location: std::collections::HashMap<(String, u64), usize> = std::collections::HashMap::new();
    let mut docs_hidden = 0usize;
    for channel in channels {
        let Some(list) = raw.get(channel).and_then(|v| v.as_array()) else { continue };
        for (rank, hit) in list.iter().enumerate() {
            let Some((path, line, kind, name)) = hit_location(hit) else { continue };
            let contribution = 1.0 / (RRF_OFFSET + rank as f64);
            let is_doc = kind == "section" || is_doc_path(&path);
            if is_doc && !opts.include_docs {
                docs_hidden += 1;
                continue;
            }
            match index_by_location.get(&(path.clone(), line)) {
                Some(&at) => merged[at].score += contribution,
                None => {
                    let exact = !name.is_empty() && name.eq_ignore_ascii_case(query);
                    index_by_location.insert((path.clone(), line), merged.len());
                    merged.push(MergedHit {
                        snippet: hit_snippet(hit),
                        score: contribution + if exact { EXACT_NAME_BONUS } else { 0.0 },
                        path, line, kind, name,
                    });
                }
            }
        }
    }
    merged.sort_by(|a, b| path_tier(&a.path).cmp(&path_tier(&b.path))
        .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)));
    let total = merged.len();
    let shown: Vec<Value> = merged.into_iter().take(opts.limit.max(1))
        .map(|h| json!({
            "at": format!("{}:{}", h.path, h.line),
            "sym": if h.name.is_empty() { h.kind } else { format!("{} {}", h.kind, h.name) },
            "snip": h.snippet,
        }))
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("mode".into(), json!("dual"));
    out.insert("hits".into(), json!(shown));
    out.insert("total_candidates".into(), json!(total));
    if docs_hidden > 0 {
        out.insert("docs_hidden".into(), json!(docs_hidden));
    }
    if opts.include_docs {
        let commits: Vec<Value> = raw.get("commits").and_then(|v| v.as_array()).map(|list| list.iter().take(5).map(|c| {
            let hash = c.get("hash").and_then(|v| v.as_str()).unwrap_or("");
            let message = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
            json!(format!("{} {}", &hash[..hash.len().min(8)], squeeze(message, SNIPPET_CHARS)))
        }).collect()).unwrap_or_default();
        out.insert("commits".into(), json!(commits));
    }
    if let Some(degraded) = raw.get("degraded").filter(|v| v.as_bool() == Some(true)) {
        out.insert("degraded".into(), degraded.clone());
    }
    Value::Object(out)
}
