use serde_json::{json, Value};
use super::host_abi::{host_fetch, unpack_to_value};
use super::verbs::{err_coded, err_json, ok, ERR_CODE_FAILED, ERR_CODE_INVALID_ARGS};

const SERP_ENGINE: &str = "duckduckgo-lite";
const SERP_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
const SERP_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const SERP_TIMEOUT_MS: u64 = 20_000;
const SERP_RESULT_CAP: usize = 10;
const SERP_SNIPPET_CHARS: usize = 200;
const SERP_TITLE_CHARS: usize = 160;
const SERP_QUERY_CHARS: usize = 400;
const SERP_LINK_MARKER: &str = "result-link";
const SERP_SNIPPET_MARKER: &str = "result-snippet";

pub fn handle(body: &Value, body_s: &str) -> u64 {
    let Some(query) = query_from_body(body, body_s) else {
        return err_json("serp", json!({
            "error": "serp needs a search query: pass the query itself as a plain-text body (raw_body), or a JSON body {\"query\": \"...\"}",
            "error_code": ERR_CODE_INVALID_ARGS,
        }));
    };
    let url = format!("{SERP_ENDPOINT}?q={}", form_encode(&query));
    if let Err(reason) = crate::config_path::validate_fetch_url(&url) {
        return err_coded("serp", ERR_CODE_INVALID_ARGS, &format!("search url rejected: {reason}"));
    }
    let opts = json!({
        "method": "GET",
        "headers": { "User-Agent": SERP_USER_AGENT, "Accept": "text/html" },
        "timeoutMs": SERP_TIMEOUT_MS,
    }).to_string();
    let packed = unsafe { host_fetch(url.as_ptr(), url.len() as u32, opts.as_ptr(), opts.len() as u32) };
    let response = unpack_to_value(packed);
    if response.is_null() {
        return err_coded("serp", ERR_CODE_FAILED, "host_fetch returned no bytes -- the search request never completed");
    }
    let status = response.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
    let html = response.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let results = parse_results(html);
    let mut payload = json!({
        "query": query,
        "engine": SERP_ENGINE,
        "status": status,
        "count": results.len(),
        "results": results,
    });
    if results.is_empty() {
        let transport_error = response.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let detail = if transport_error.is_empty() {
            format!("the search endpoint answered HTTP {status} with no parseable results -- a 202 or 403 is DuckDuckGo's bot challenge, retry once and fall back to the fetch verb with a direct URL")
        } else {
            format!("{transport_error} (HTTP {status})")
        };
        payload["error"] = json!(detail);
        payload["error_code"] = json!(ERR_CODE_FAILED);
        return err_json("serp", payload);
    }
    ok("serp", payload)
}

fn query_from_body(body: &Value, body_s: &str) -> Option<String> {
    let raw = body
        .get("query")
        .or_else(|| body.get("q"))
        .and_then(|v| v.as_str())
        .unwrap_or(body_s);
    let query = collapse_whitespace(raw.trim());
    if query.is_empty() { None } else { Some(clip(&query, SERP_QUERY_CHARS)) }
}

fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn parse_results(html: &str) -> Vec<Value> {
    let mut results = Vec::new();
    let mut cursor = 0usize;
    while results.len() < SERP_RESULT_CAP {
        let Some(link_at) = html[cursor..].find(SERP_LINK_MARKER).map(|i| cursor + i) else { break };
        let Some(anchor_at) = html[..link_at].rfind("<a") else {
            cursor = link_at + SERP_LINK_MARKER.len();
            continue;
        };
        let Some(open_end) = html[link_at..].find('>').map(|i| link_at + i) else { break };
        let Some(close_at) = html[open_end..].find("</a>").map(|i| open_end + i) else { break };
        let after_close = close_at + "</a>".len();
        let title = collapse_whitespace(&text_of(&html[open_end + 1..close_at]));
        let url = result_url(&attribute_value(&html[anchor_at..open_end], "href"));
        let snippet = snippet_after(html, after_close);
        cursor = after_close;
        if title.is_empty() || url.is_empty() { continue; }
        results.push(json!({
            "title": clip(&title, SERP_TITLE_CHARS),
            "url": url,
            "snippet": clip(&snippet, SERP_SNIPPET_CHARS),
        }));
    }
    results
}

fn snippet_after(html: &str, from: usize) -> String {
    let Some(marker_at) = html[from..].find(SERP_SNIPPET_MARKER).map(|i| from + i) else { return String::new() };
    if let Some(next_link) = html[from..].find(SERP_LINK_MARKER).map(|i| from + i) {
        if next_link < marker_at { return String::new(); }
    }
    let Some(open_end) = html[marker_at..].find('>').map(|i| marker_at + i) else { return String::new() };
    let start = open_end + 1;
    let window = &html[start..];
    let end = window
        .find("</td>")
        .or_else(|| window.find("</"))
        .map(|i| start + i)
        .unwrap_or(html.len());
    collapse_whitespace(&text_of(&html[start..end]))
}

fn attribute_value(tag: &str, name: &str) -> String {
    let needle = format!("{name}=");
    let Some(at) = tag.find(&needle) else { return String::new() };
    let rest = tag[at + needle.len()..].trim_start();
    match rest.as_bytes().first() {
        Some(b'"') | Some(b'\'') => {
            let quote = rest.as_bytes()[0] as char;
            let end = rest[1..].find(quote).map(|i| i + 1).unwrap_or(rest.len());
            rest[1..end].to_string()
        }
        _ => {
            let end = rest.find([' ', '>', '\t', '\n', '\r', '"', '\'']).unwrap_or(rest.len());
            rest[..end].to_string()
        }
    }
}

fn result_url(href: &str) -> String {
    let href = href.trim();
    if href.is_empty() { return String::new(); }
    if let Some((_, encoded_target)) = href.split_once("uddg=") {
        let value = encoded_target.split('&').next().unwrap_or("");
        let decoded = percent_decode(value);
        if decoded.starts_with("http://") || decoded.starts_with("https://") { return decoded; }
    }
    if href.starts_with("//") { return format!("https:{href}"); }
    if href.starts_with('/') { return format!("https://duckduckgo.com{href}"); }
    if href.starts_with("http://") || href.starts_with("https://") { return href.to_string(); }
    String::new()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((high << 4) | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn text_of(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                inside_tag = true;
                out.push(' ');
            }
            '>' => inside_tag = false,
            _ if inside_tag => {}
            _ => out.push(ch),
        }
    }
    decode_entities(&out)
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        let after = &rest[start + 1..];
        let Some(end) = after.find(';') else { break };
        out.push_str(&rest[..start]);
        out.push_str(&entity_char(&after[..end]));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn entity_char(body: &str) -> String {
    if let Some(digits) = body.strip_prefix('#') {
        let (radix, digits) = match digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
            Some(hex_digits) => (16u32, hex_digits),
            None => (10u32, digits),
        };
        if let Ok(code) = u32::from_str_radix(digits, radix) {
            return char::from_u32(code).map(String::from).unwrap_or_default();
        }
        return String::new();
    }
    match body {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => "",
    }
    .to_string()
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}
