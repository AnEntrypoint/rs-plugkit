use serde_json::{json, Value};
use std::collections::HashSet;

pub const PREVIEW_CHARS: usize = 200;
const TITLE_CHARS: usize = 90;
const NEAR_DUPLICATE_JACCARD: f64 = 0.85;
const MIN_TOKENS_FOR_DEDUPE: usize = 6;

fn squeeze(text: &str, limit: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(limit).collect();
    format!("{cut}...")
}

fn token_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_lowercase())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count() as f64;
    shared / a.union(b).count() as f64
}

fn split_title(text: &str) -> (String, String) {
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    match lines.next() {
        Some(first) if first.trim_start().starts_with('#') => {
            let title = squeeze(first.trim_start_matches('#').trim(), TITLE_CHARS);
            let rest: Vec<&str> = lines.collect();
            (title, rest.join("\n"))
        }
        Some(first) => (
            String::new(),
            format!("{first}\n{}", lines.collect::<Vec<_>>().join("\n")),
        ),
        None => (String::new(), String::new()),
    }
}

pub fn compact_hits(hits: &Value, full: bool) -> Value {
    let Some(list) = hits.as_array() else {
        return hits.clone();
    };
    let mut kept: Vec<(HashSet<String>, Value)> = Vec::new();
    let mut deduped_keys: Vec<Value> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();
    for hit in list {
        let text = ["text", "value"]
            .iter()
            .find_map(|f| hit.get(*f).and_then(|v| v.as_str()))
            .unwrap_or("");
        let tokens = token_set(text);
        let repeated_key = hit
            .get("key")
            .and_then(|v| v.as_str())
            .is_some_and(|k| !seen_keys.insert(k.to_string()));
        let duplicate = repeated_key
            || (tokens.len() >= MIN_TOKENS_FOR_DEDUPE
                && kept
                    .iter()
                    .any(|(seen, _)| jaccard(seen, &tokens) >= NEAR_DUPLICATE_JACCARD));
        if duplicate {
            deduped_keys.push(hit.get("key").cloned().unwrap_or(Value::Null));
            continue;
        }
        let (title, body) = split_title(text);
        let mut row = serde_json::Map::new();
        if let Some(key) = hit.get("key") {
            row.insert("key".into(), key.clone());
        }
        if let Some(score) = hit.get("score").and_then(|v| v.as_f64()) {
            row.insert("score".into(), json!((score * 1000.0).round() / 1000.0));
        }
        if !title.is_empty() {
            row.insert("title".into(), json!(title));
        }
        if full {
            row.insert("text".into(), json!(text));
        } else {
            row.insert("text".into(), json!(squeeze(&body, PREVIEW_CHARS)));
            row.insert("chars".into(), json!(text.chars().count()));
        }
        kept.push((tokens, Value::Object(row)));
    }
    let mut rows: Vec<Value> = kept.into_iter().map(|(_, row)| row).collect();
    if !deduped_keys.is_empty() {
        rows.push(json!({ "deduped_near_identical": deduped_keys }));
    }
    Value::Array(rows)
}

pub fn wants_full(body: &Value) -> bool {
    body.get("full").and_then(|v| v.as_bool()).unwrap_or(false)
}
