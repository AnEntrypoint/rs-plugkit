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
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "adoc"];
const DOC_SEGMENTS: &[&str] = &["docs", "doc"];
pub const DOC_EXCLUDE_GLOBS: &[&str] = &[
    "*.md",
    "*.mdx",
    "*.rst",
    "*.adoc",
    "docs/**",
    "**/docs/**",
    "doc/**",
    "**/doc/**",
];
const LOW_PRIORITY_SEGMENTS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "examples",
    "example",
    "scripts",
    "tools",
    "bench",
    "benchmarks",
    "fixtures",
];
const GENERATED_SEGMENTS: &[&str] = &["dist", "build", "out", "vendor", "generated", "coverage"];
const CONTROL_WORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "function", "await", "typeof",
];
const LIMIT_FIELDS: &[&str] = &["k", "max_results", "maxResults", "limit", "head_limit"];
const DECLARATION_KEYWORDS: &str =
    r"class|function\*?|fn|struct|enum|trait|interface|type|def|mod|func|fun|macro_rules!";
const BINDING_KEYWORDS: &str = r"const|let|var|static";
const METHOD_MODIFIERS: &str =
    r"export|default|static|async|get|set|pub|public|private|protected|override";

pub struct RankOptions {
    pub include_docs: bool,
    pub verbose: bool,
    pub limit: usize,
    pub limit_was_explicit: bool,
    pub root: Option<String>,
}

impl RankOptions {
    pub fn from_body(body: &Value, limit: usize, root: Option<&str>) -> RankOptions {
        let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("code");
        RankOptions {
            include_docs: body.get("docs").and_then(|v| v.as_bool()).unwrap_or(false)
                || matches!(kind, "docs" | "all"),
            verbose: body
                .get("verbose")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            limit,
            limit_was_explicit: LIMIT_FIELDS.iter().any(|f| body.get(*f).is_some()),
            root: root
                .filter(|r| !matches!(*r, "." | "./"))
                .map(|r| r.replace('\\', "/").trim_end_matches('/').to_string()),
        }
    }

    fn relative<'a>(&self, path: &'a str) -> &'a str {
        let path = normalized(path);
        match &self.root {
            Some(root) => path
                .strip_prefix(root.as_str())
                .map(|rest| rest.trim_start_matches('/'))
                .unwrap_or(path),
            None => path,
        }
    }
}

fn contains_verbatim(haystack: &str, needle: &str) -> bool {
    !haystack.is_empty()
        && haystack
            .to_lowercase()
            .contains(&needle.to_lowercase())
}

pub fn is_identifier(query: &str) -> bool {
    query.len() >= 3
        && query.len() <= 96
        && query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !query.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn normalized(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\'])
}

pub fn is_doc_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext_is_doc = name
        .rsplit_once('.')
        .is_some_and(|(_, e)| DOC_EXTENSIONS.iter().any(|d| e.eq_ignore_ascii_case(d)));
    ext_is_doc || segments(path).any(|s| DOC_SEGMENTS.contains(&s.to_ascii_lowercase().as_str()))
}

fn path_tier(path: &str) -> u8 {
    let lower = path.to_ascii_lowercase();
    if segments(&lower).any(|s| GENERATED_SEGMENTS.contains(&s))
        || lower.ends_with(".min.js")
        || lower.ends_with(".d.ts")
    {
        return 2;
    }
    if segments(&lower).any(|s| LOW_PRIORITY_SEGMENTS.contains(&s))
        || lower.contains(".test.")
        || lower.contains(".spec.")
    {
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

fn identifier_pattern(identifier: &str, substring: bool) -> String {
    if substring {
        format!(r"[\w$]*(?i:{})[\w$]*", regex::escape(identifier))
    } else {
        regex::escape(identifier)
    }
}

fn declaration_pattern(id: &str) -> String {
    format!(
        r"(?:(?:^|[^\w$])(?:{DECLARATION_KEYWORDS})\s+{id}(?:[^\w$]|$))|(?:(?:^|[^\w$])impl(?:\s*<[^>]*>)?\s+{id}(?:[^\w$]|$))|(?:(?:^|[^\w$])func\s+\([^)]*\)\s*{id}\s*\()|(?:^\s*(?:(?:{METHOD_MODIFIERS})\s+)*{id}\s*\([^)]*\)\s*(?::[^{{]+)?\{{)|(?:(?:^|[^\w$.]){id}\s*[:=]\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*=>|[\w$]+\s*=>|class\b))"
    )
}

fn binding_pattern(id: &str) -> String {
    format!(r"(?:^|[^\w$])(?:{BINDING_KEYWORDS})\s+{id}(?:[^\w$]|$)")
}

pub fn any_definition_pattern(identifier: &str, substring: bool) -> String {
    let id = identifier_pattern(identifier, substring);
    format!("{}|{}", declaration_pattern(&id), binding_pattern(&id))
}

struct DefinitionMatcher {
    declaration: Option<regex::Regex>,
    binding: Option<regex::Regex>,
}

impl DefinitionMatcher {
    fn new(identifier: &str, substring: bool) -> DefinitionMatcher {
        let id = identifier_pattern(identifier, substring);
        DefinitionMatcher {
            declaration: regex::Regex::new(&declaration_pattern(&id)).ok(),
            binding: regex::Regex::new(&binding_pattern(&id)).ok(),
        }
    }

    fn strength(&self, text: &str, identifier: &str) -> Option<u8> {
        if looks_like_call_not_definition(text, identifier) {
            return None;
        }
        if self.declaration.as_ref().is_some_and(|m| m.is_match(text)) {
            return Some(0);
        }
        if self.binding.as_ref().is_some_and(|m| m.is_match(text)) {
            return Some(1);
        }
        None
    }
}

struct LineHit {
    path: String,
    line: u64,
    text: String,
    strength: u8,
    is_doc: bool,
    tier: u8,
}

fn parse_compact_line(raw: &str) -> Option<(String, u64, String)> {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let digits = bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits > 0 && bytes.get(i + 1 + digits) == Some(&b':') {
                let line: u64 = raw[i + 1..i + 1 + digits].parse().ok()?;
                let text = raw[i + 2 + digits..].trim_start().to_string();
                return Some((normalized(&raw[..i]).to_string(), line, text));
            }
        }
        i += 1;
    }
    None
}

fn looks_like_call_not_definition(text: &str, identifier: &str) -> bool {
    let trimmed = text.trim_start();
    CONTROL_WORDS
        .iter()
        .any(|w| trimmed.starts_with(w) && trimmed[w.len()..].trim_start().starts_with('('))
        && !trimmed.starts_with(identifier)
}

fn render(hit: &LineHit) -> String {
    format!(
        "{}:{}: {}",
        hit.path,
        hit.line,
        squeeze(&hit.text, SNIPPET_CHARS)
    )
}

pub struct ScanLines<'a> {
    pub lines: &'a [String],
    pub definition_lines: &'a [String],
    pub complete: bool,
    pub definitions_complete: bool,
}

pub fn identifier_report(
    identifier: &str,
    scan: &ScanLines,
    opts: &RankOptions,
    substring: bool,
) -> Option<Value> {
    let matcher = DefinitionMatcher::new(identifier, substring);
    let mut definitions: Vec<LineHit> = Vec::new();
    let mut references: Vec<LineHit> = Vec::new();
    let mut seen: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    let mut docs_hidden = 0usize;
    for raw in scan.lines.iter().chain(scan.definition_lines.iter()) {
        let Some((path, line, text)) = parse_compact_line(raw) else {
            continue;
        };
        if !seen.insert((path.clone(), line)) {
            continue;
        }
        let relative = opts.relative(&path);
        let is_doc = is_doc_path(relative);
        if is_doc && !opts.include_docs {
            docs_hidden += 1;
            continue;
        }
        let tier = path_tier(relative);
        match matcher.strength(&text, identifier) {
            Some(strength) => definitions.push(LineHit {
                path,
                line,
                text,
                strength,
                is_doc,
                tier,
            }),
            None => references.push(LineHit {
                path,
                line,
                text,
                strength: 2,
                is_doc,
                tier,
            }),
        }
    }
    if definitions.is_empty() && references.is_empty() {
        return None;
    }
    let order = |a: &LineHit, b: &LineHit| {
        a.is_doc
            .cmp(&b.is_doc)
            .then_with(|| a.strength.cmp(&b.strength))
            .then_with(|| a.tier.cmp(&b.tier))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    };
    definitions.sort_by(order);
    references.sort_by(order);

    let mut per_file: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for hit in definitions.iter().chain(references.iter()) {
        *per_file.entry(hit.path.as_str()).or_insert(0) += 1;
    }
    let definition_cap = if opts.limit_was_explicit {
        opts.limit.clamp(1, DEFAULT_DEFINITION_CAP)
    } else {
        DEFAULT_DEFINITION_CAP
    };
    let shown_definitions: Vec<String> = definitions
        .iter()
        .take(definition_cap)
        .map(render)
        .collect();

    let reference_cap = if opts.limit_was_explicit {
        opts.limit.max(1)
    } else {
        opts.limit.max(DEFAULT_REFERENCE_CAP)
    };
    let mut taken_per_file: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    let mut shown_references: Vec<String> = Vec::new();
    for hit in &references {
        if shown_references.len() >= reference_cap {
            break;
        }
        let taken = taken_per_file.entry(hit.path.as_str()).or_insert(0);
        if *taken >= REFERENCES_PER_FILE {
            continue;
        }
        *taken += 1;
        shown_references.push(render(hit));
    }

    let file_count = per_file.len();
    let mut busiest: Vec<(&str, usize)> = per_file.into_iter().collect();
    busiest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let top_files: Vec<String> = busiest
        .into_iter()
        .take(TOP_FILES)
        .map(|(p, n)| format!("{p} ({n})"))
        .collect();

    let mut out = serde_json::Map::new();
    out.insert(
        "mode".into(),
        json!(if substring {
            "symbol_substring"
        } else {
            "symbol"
        }),
    );
    out.insert("query".into(), json!(identifier));
    out.insert("definitions".into(), json!(shown_definitions));
    out.insert("references".into(), json!(shown_references));
    out.insert(
        "counts".into(),
        json!({
            "definitions": definitions.len(),
            "references": references.len(),
            "files": file_count,
            "shown_definitions": shown_definitions.len(),
            "shown_references": shown_references.len(),
        }),
    );
    if docs_hidden > 0 {
        out.insert("docs_hidden".into(), json!(docs_hidden));
    }
    if !scan.complete {
        out.insert(
            "partial".into(),
            json!(if scan.definitions_complete {
                "references"
            } else {
                "definitions_and_references"
            }),
        );
    }
    if shown_references.len() < references.len() || shown_definitions.len() < definitions.len() {
        out.insert("top_files".into(), json!(top_files));
        let all_matches = if substring {
            "mode=regex query=(?i)<query> output=compact"
        } else {
            "mode=literal whole_word=true output=compact"
        };
        out.insert("more".into(), json!(format!("all matches (docs included): {all_matches}; docs in this ranked view: docs=true; raw BM25/vector/commit channels: verbose=true")));
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
    literal: bool,
}

fn hit_location(hit: &Value) -> Option<(String, u64, String, String)> {
    let symbol = hit.get("symbol");
    let pick = |field: &str| symbol.and_then(|s| s.get(field)).or_else(|| hit.get(field));
    let path = pick("path").and_then(|v| v.as_str())?;
    let line = pick("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
    let kind = pick("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = pick("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((normalized(path).to_string(), line, kind, name))
}

fn hit_text(hit: &Value) -> &str {
    ["text", "snippet", "value"]
        .iter()
        .find_map(|f| hit.get(*f).and_then(|v| v.as_str()))
        .unwrap_or("")
}

fn hit_snippet(hit: &Value, skip_header: bool) -> String {
    let picked: Vec<String> = hit_text(hit)
        .lines()
        .skip(usize::from(skip_header))
        .filter(|l| !l.trim().is_empty())
        .take(SNIPPET_LINES)
        .map(|l| squeeze(l, SNIPPET_CHARS))
        .collect();
    picked.join(" | ")
}

pub fn compact_dual(query: &str, raw: &Value, opts: &RankOptions) -> Value {
    let channels = ["bm25_hits", "vector_hits", "hits"];
    let mut merged: Vec<MergedHit> = Vec::new();
    let mut unlocated: Vec<Value> = Vec::new();
    let mut index_by_location: std::collections::HashMap<(String, u64), usize> =
        std::collections::HashMap::new();
    let mut docs_hidden = 0usize;
    for channel in channels {
        let Some(list) = raw.get(channel).and_then(|v| v.as_array()) else {
            continue;
        };
        for (rank, hit) in list.iter().enumerate() {
            let contribution = 1.0 / (RRF_OFFSET + rank as f64);
            let Some((path, line, kind, name)) = hit_location(hit) else {
                if unlocated.len() < opts.limit.max(1) {
                    unlocated.push(json!({ "key": hit.get("key").cloned().unwrap_or(Value::Null), "snip": hit_snippet(hit, false) }));
                }
                continue;
            };
            let literal = contains_verbatim(hit_text(hit), query);
            let relative = opts.relative(&path);
            if (kind == "section" || is_doc_path(relative)) && !opts.include_docs {
                docs_hidden += 1;
                continue;
            }
            match index_by_location.get(&(path.clone(), line)) {
                Some(&at) => {
                    merged[at].score += contribution;
                    merged[at].literal |= literal;
                }
                None => {
                    let exact = !name.is_empty() && name.eq_ignore_ascii_case(query);
                    index_by_location.insert((path.clone(), line), merged.len());
                    merged.push(MergedHit {
                        snippet: hit_snippet(hit, hit.get("text").is_some()),
                        score: contribution + if exact { EXACT_NAME_BONUS } else { 0.0 },
                        path,
                        line,
                        kind,
                        name,
                        literal,
                    });
                }
            }
        }
    }
    merged.sort_by(|a, b| {
        path_tier(opts.relative(&a.path))
            .cmp(&path_tier(opts.relative(&b.path)))
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let total = merged.len();
    let literal_total = merged.iter().filter(|h| h.literal).count();
    let shown: Vec<Value> = merged
        .into_iter()
        .take(opts.limit.max(1))
        .map(|h| {
            json!({
                "at": format!("{}:{}", h.path, h.line),
                "sym": if h.name.is_empty() { h.kind } else { format!("{} {}", h.kind, h.name) },
                "snip": h.snippet,
                "lit": h.literal,
            })
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert(
        "mode".into(),
        raw.get("mode").cloned().unwrap_or_else(|| json!("dual")),
    );
    out.insert("hits".into(), json!(shown));
    out.insert("total_candidates".into(), json!(total));
    out.insert("literal_matches".into(), json!(literal_total));
    if literal_total == 0 && total > 0 {
        out.insert("hits_are_nearest_neighbours".into(), json!(true));
        out.insert(
            "note".into(),
            json!(format!(
                "no retrieved row contains \"{query}\" verbatim -- the rows above are ranked nearest neighbours from BM25/vector retrieval, not occurrences of the term"
            )),
        );
    }
    if !unlocated.is_empty() {
        out.insert("unlocated_hits".into(), json!(unlocated));
    }
    if docs_hidden > 0 {
        out.insert("docs_hidden".into(), json!(docs_hidden));
    }
    if opts.include_docs {
        let commits: Vec<Value> = raw
            .get("commits")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .take(5)
                    .map(|c| {
                        let hash = c.get("hash").and_then(|v| v.as_str()).unwrap_or("");
                        let message = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        json!(format!(
                            "{} {}",
                            &hash[..hash.len().min(8)],
                            squeeze(message, SNIPPET_CHARS)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert("commits".into(), json!(commits));
    }
    if let Some(degraded) = raw.get("degraded").filter(|v| v.as_bool() == Some(true)) {
        out.insert("degraded".into(), degraded.clone());
    }
    Value::Object(out)
}
