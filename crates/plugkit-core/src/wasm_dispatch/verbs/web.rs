use crate::config_path::validate_fetch_url;
use crate::orchestrator::yaml_util::base64_decode;
use crate::wasm_dispatch::host_abi::{
    host_env_get, host_fetch, unpack_to_string, unpack_to_value,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{err, err_coded, err_json, ok, ERR_CODE_INVALID_ARGS};

pub const FETCH_DEFAULT_MAX_CHARS: usize = 6_000;
pub const FETCH_RAW_DEFAULT_MAX_CHARS: usize = 1_000_000;
pub const FETCH_MAX_MAX_CHARS: usize = 4_000_000;
pub const FETCH_DEFAULT_ANSWER_CHARS: usize = 4_000;
pub const FETCH_MAX_ANSWER_CHARS: usize = 200_000;

pub const WEBSEARCH_DEFAULT_LIMIT: usize = 8;
pub const WEBSEARCH_MAX_LIMIT: usize = 25;
pub const FETCH_SEARCH_DEFAULT_LIMIT: usize = 10;
pub const FETCH_SEARCH_MAX_LIMIT: usize = 50;
pub const SEARCH_MAX_BACKENDS: usize = 4;
pub const SEARCH_ENV_BACKEND: &str = "GM_SEARCH_BACKEND";
pub const SEARCH_BACKENDS: &[(&str, &str)] = &[
    ("duckduckgo", "https://html.duckduckgo.com/html/?q={q}"),
    ("mojeek", "https://www.mojeek.com/search?q={q}"),
    ("bing", "https://www.bing.com/search?q={q}&setlang=en"),
    ("searxng", "https://searx.be/search?q={q}"),
    ("brave", "https://search.brave.com/search?q={q}"),
    ("marginalia", "https://search.marginalia.nu/search?query={q}"),
];
pub const WEBSEARCH_DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const WEBSEARCH_ENGINE_DUCKDUCKGO: &str = "duckduckgo";
pub const WEBSEARCH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

pub const FETCH_HELP: &str = "\
fetch {url, | query, prompt?, max_chars?, raw?, method?, headers?, body?, timeoutMs?, answer_max_chars?}.
  url: the page to fetch; https:// and http:// only, no whitespace, at most 2048 bytes.
  The reply is the READABLE page, not the transport payload: HTML is converted to markdown before
  it is returned (head, script, style, svg, nav, header, footer, aside, form and iframe blocks are
  dropped; article or main wins over the rest of the document; headings, lists, links, images,
  code fences and tables are kept as markdown). A body that is not HTML (JSON, plain text, CSV)
  is returned as it arrived.
  query: (aliases q, search) with no url turns the call into a web search, the discovery half of
          this port. Backends are keyless and tried in order -- duckduckgo, mojeek, bing, searxng,
          brave, marginalia -- until one parses into hits; at most 4 are tried per call. Pick one
          with backend (alias engine, a name or a results-page template holding {q}) or with the
          GM_SEARCH_BACKEND environment variable, both comma-separated lists. count (alias limit,
          max_results, k, n) caps the hits, default 10, ceiling 50. Rows are rank, title, url, host
          and snippet, a few KB in total; no raw HTML is ever returned unless raw:true.
  prompt: when present, the reply answers it instead of carrying the page. The answer is the set of
          passages of the converted page that best match the prompt, so it is compact and verbatim:
          gm has no outbound model dependency, and answer_mode is always \"extractive\". Pass no
          prompt, or raw:true, to get the page itself.
  max_chars: cap on characters returned (default 6000, ceiling 4000000). One fetch can therefore
             never dump a whole 280k-character page into the reply; the default is small enough to
             read inline, and a clipped tail names how many characters were dropped.
  answer_max_chars: cap on the prompt answer (default 4000).
  raw: true (aliases full_response, fullResponse) returns the original body with no conversion and
       no prompt answering; its default max_chars is 1000000.
  method, headers, body, opts, timeoutMs: passed to the transport; timeoutMs defaults to 60000,
       ceiling 600000.
Reply fields: url, status, content_type, body_chars, converted, max_chars, truncated, and either
content (the markdown) or prompt/answer/answer_mode/answer_chars/passages. help:true returns this
text and sends no request.";

pub const WEBSEARCH_HELP: &str = "\
websearch {query, limit?, allowed_domains?, blocked_domains?, url?, engine?, max_chars?, prompt?, timeoutMs?}.
  query: what to search for; q is an alias. Required unless url is given.
  url: fetch this search engine results page instead of building one from query. Use it when the
       default endpoint answers with a challenge or consent page, or to point at another engine.
  engine: \"duckduckgo\" (the default). It reads https://html.duckduckgo.com/html/, which needs no
       credential and no API key, and answers a challenge page to some clients, so one call walks
       the keyless backends in order -- duckduckgo, mojeek, bing, searxng -- and stops at the first
       that parses into hits, 4 at most. engine (aliases backend, search_backend) and the
       GM_SEARCH_BACKEND variable pick one, or a comma-separated list of them, and an unknown name
       is an error naming the known ones, never a no-op.
  limit: results returned, default 8, ceiling 25 (max_results, maxResults and k are aliases).
  allowed_domains / blocked_domains: arrays (or comma-separated strings) of host suffixes. A result
       is kept only when its host matches an allowed suffix, and dropped when it matches a blocked
       one. \"example.com\" matches www.example.com and docs.example.com.
  prompt: answered against the result titles, urls and snippets, the same extractive way fetch
       answers one, so a question returns the matching results and not the whole listing.
  Reply fields: query, engine, endpoint, status, count, found, limit, truncated, filters, results
  (one row per hit: title, url, host, snippet), and prompt/answer when prompt is given. help:true
  returns this text and sends no request.";

const NOISE_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "head", "nav", "footer", "header", "aside", "form",
    "iframe", "template", "select", "button", "input", "meta", "link", "canvas", "path", "source",
    "picture", "object", "embed",
];

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "what", "which",
    "who", "whom", "whose", "how", "why", "when", "where", "does", "do", "did", "can", "could",
    "should", "would", "about", "into", "than", "then", "them", "they", "their", "there", "here",
    "have", "has", "had", "not", "but", "you", "your", "its", "also", "more", "most", "some",
    "any", "all", "one", "two", "use", "used", "using", "list", "name", "give", "show", "tell",
    "page", "site", "web", "url", "http", "https", "www", "com", "org", "net",
];

pub fn fetch(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        let query = trimmed_str(body, &["query", "q", "search"]);
        if query.is_empty() {
            return err("fetch", "url required (or query, q or search for a web search)");
        }
        return search_reply(body, &query, "fetch", &["count", "limit", "max_results", "maxResults", "k", "n"], FETCH_SEARCH_DEFAULT_LIMIT);
    }
    if let Err(reason) = validate_fetch_url(url) {
        return err_coded("fetch", ERR_CODE_INVALID_ARGS, &reason);
    }
    let timeout_ms = match timeout_ms_of(body, super::FETCH_DEFAULT_TIMEOUT_MS, "fetch") {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let raw_requested = flag(body, &["raw", "full_response", "fullResponse"]);
    let max_default = if raw_requested {
        FETCH_RAW_DEFAULT_MAX_CHARS
    } else {
        FETCH_DEFAULT_MAX_CHARS
    };
    let max_chars = match bounded_usize(
        body,
        &["max_chars", "maxChars"],
        max_default,
        FETCH_MAX_MAX_CHARS,
        "fetch",
        "max_chars",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let prompt = trimmed_str(body, &["prompt"]);

    let opts = fetch_opts(body, timeout_ms);
    let v = host_fetch_value(url, &opts);
    if v.is_null() {
        return err("fetch", "host_fetch empty");
    }
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return err_json("fetch", v);
    }
    let original = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
    let content_type = v.get("content_type").and_then(|c| c.as_str()).unwrap_or("");
    let encoding = v.get("encoding").and_then(|c| c.as_str()).unwrap_or("");

    let mut data = json!({
        "ok": true,
        "url": url,
        "status": v.get("status").cloned().unwrap_or(Value::Null),
        "raw": raw_requested,
        "max_chars": max_chars,
        "body_chars": original.chars().count(),
    });
    if !content_type.is_empty() {
        data["content_type"] = json!(content_type);
    }
    if !encoding.is_empty() {
        data["encoding"] = json!(encoding);
    }

    if raw_requested || !encoding.is_empty() {
        let (text, truncated) = clamp_chars(original, max_chars);
        data["body"] = json!(text);
        data["truncated"] = json!(truncated);
        return ok("fetch", data);
    }

    let is_html = looks_like_html(original);
    let converted = if is_html {
        html_to_markdown(original)
    } else {
        original.to_string()
    };
    data["converted"] = json!(is_html);
    data["content_chars"] = json!(converted.chars().count());

    if prompt.is_empty() {
        let (text, truncated) = clamp_chars(&converted, max_chars);
        data["content"] = json!(text);
        data["truncated"] = json!(truncated);
        return ok("fetch", data);
    }

    let budget = match bounded_usize(
        body,
        &["answer_max_chars", "answerMaxChars"],
        FETCH_DEFAULT_ANSWER_CHARS,
        FETCH_MAX_ANSWER_CHARS,
        "fetch",
        "answer_max_chars",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let (answer, passages) = extractive_answer(&converted, &prompt, budget);
    data["prompt"] = json!(prompt);
    data["answer"] = json!(answer);
    data["answer_mode"] = json!("extractive");
    data["answer_chars"] = json!(answer.chars().count());
    data["passages"] = json!(passages);
    data["answer_note"] = json!("gm holds no outbound model credential, so a prompt is answered by selecting the passages of the converted page that match it best, verbatim. Pass no prompt for the page, or raw:true for the untouched response body.");
    ok("fetch", data)
}

pub fn websearch(body: &Value) -> u64 {
    let query = trimmed_str(body, &["query", "q"]);
    let explicit_url = trimmed_str(body, &["url"]);
    if query.is_empty() && explicit_url.is_empty() {
        return err("websearch", "query required (or url naming a search engine results page)");
    }
    let using_explicit_url = !explicit_url.is_empty();
    for raw in backend_names(body) {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() || name.starts_with("http://") || name.starts_with("https://") {
            continue;
        }
        if !SEARCH_BACKENDS.iter().any(|(known, _)| *known == name.as_str()) {
            let known: Vec<&str> = SEARCH_BACKENDS.iter().map(|(n, _)| *n).collect();
            return err_coded(
                "websearch",
                ERR_CODE_INVALID_ARGS,
                &format!(
                    "unknown engine {name:?}; the keyless backends are {}, and any other engine is reached by passing its results page as url or backend",
                    known.join(", ")
                ),
            );
        }
    }
    let timeout_ms = match timeout_ms_of(body, WEBSEARCH_DEFAULT_TIMEOUT_MS, "websearch") {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let limit = match bounded_usize(
        body,
        &["limit", "max_results", "maxResults", "k"],
        WEBSEARCH_DEFAULT_LIMIT,
        WEBSEARCH_MAX_LIMIT,
        "websearch",
        "limit",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let raw_requested = flag(body, &["raw", "full_response", "fullResponse"]);
    let max_chars = match bounded_usize(
        body,
        &["max_chars", "maxChars"],
        if raw_requested {
            FETCH_RAW_DEFAULT_MAX_CHARS
        } else {
            FETCH_DEFAULT_MAX_CHARS
        },
        FETCH_MAX_MAX_CHARS,
        "websearch",
        "max_chars",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };

    let mut opts = json!({
        "timeoutMs": timeout_ms,
        "headers": { "User-Agent": WEBSEARCH_USER_AGENT, "Accept-Language": "en-US,en;q=0.9" },
    });
    if let Some(headers) = body.get("headers") {
        opts["headers"] = headers.clone();
    }
    let candidates: Vec<(String, String)> = if using_explicit_url {
        vec![("url".to_string(), explicit_url.clone())]
    } else {
        search_backends(body)
            .into_iter()
            .take(SEARCH_MAX_BACKENDS)
            .collect()
    };
    let mut attempts: Vec<Value> = Vec::new();
    let mut chosen = if using_explicit_url {
        "url".to_string()
    } else {
        WEBSEARCH_ENGINE_DUCKDUCKGO.to_string()
    };
    let mut endpoint = String::new();
    let mut html = String::new();
    let mut status = Value::Null;
    for (name, template) in &candidates {
        let url = if using_explicit_url {
            template.clone()
        } else {
            backend_url(template, &query)
        };
        if let Err(reason) = validate_fetch_url(&url) {
            attempts.push(json!({ "backend": name, "error": reason }));
            continue;
        }
        let v = host_fetch_value(&url, &opts);
        let transport_ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
        let text = v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
        let seen_status = v.get("status").cloned().unwrap_or(Value::Null);
        let hits = if transport_ok { parse_results(&text).len() } else { 0 };
        attempts.push(json!({
            "backend": name,
            "url": url,
            "status": seen_status.clone(),
            "chars": text.chars().count(),
            "hits": hits,
            "transport_ok": transport_ok,
        }));
        if endpoint.is_empty() && transport_ok {
            chosen = name.clone();
            endpoint = url.clone();
            html = text.clone();
            status = seen_status;
        }
        if hits > 0 {
            chosen = name.clone();
            endpoint = url;
            html = text;
            status = seen_status;
            break;
        }
        if raw_requested && transport_ok {
            break;
        }
    }
    if endpoint.is_empty() {
        return err_json(
            "websearch",
            json!({ "error": "no search backend answered with a body", "query": query, "attempts": attempts }),
        );
    }

    let mut data = json!({
        "ok": true,
        "query": query,
        "endpoint": endpoint,
        "engine": chosen,
        "status": status,
        "body_chars": html.chars().count(),
        "attempts": attempts,
    });
    if raw_requested {
        let (text, truncated) = clamp_chars(html, max_chars);
        data["body"] = json!(text);
        data["truncated"] = json!(truncated);
        return ok("websearch", data);
    }

    let mut results = parse_results(html);
    let allowed = string_list(body, &["allowed_domains", "allowedDomains"]);
    let blocked = string_list(body, &["blocked_domains", "blockedDomains"]);
    if !allowed.is_empty() {
        results.retain(|r| host_matches_any(&r.host, &allowed));
    }
    if !blocked.is_empty() {
        results.retain(|r| !host_matches_any(&r.host, &blocked));
    }
    let found = results.len();
    results.truncate(limit);
    data["found"] = json!(found);
    data["count"] = json!(results.len());
    data["limit"] = json!(limit);
    data["truncated"] = json!(found > results.len());
    data["filters"] = json!({ "allowed_domains": allowed, "blocked_domains": blocked });
    data["results"] = json!(results
        .iter()
        .enumerate()
        .map(|(rank, r)| json!({ "rank": rank + 1, "title": r.title, "url": r.url, "host": r.host, "snippet": clamp_chars(&r.snippet, 400).0 }))
        .collect::<Vec<Value>>());
    if results.is_empty() {
        data["note"] = json!("no result parsed out of the response: the endpoint most often answers a challenge or consent page. Pass url with a results page of your own, or fetch a known url directly.");
    }

    let prompt = trimmed_str(body, &["prompt"]);
    if !prompt.is_empty() {
        let corpus: String = results
            .iter()
            .map(|r| format!("{}\n{}\n{}\n\n", r.title, r.url, r.snippet))
            .collect();
        let budget = match bounded_usize(
            body,
            &["answer_max_chars", "answerMaxChars"],
            FETCH_DEFAULT_ANSWER_CHARS,
            FETCH_MAX_ANSWER_CHARS,
            "websearch",
            "answer_max_chars",
        ) {
            Ok(n) => n,
            Err(reply) => return reply,
        };
        let (answer, passages) = extractive_answer(&corpus, &prompt, budget);
        data["prompt"] = json!(prompt);
        data["answer"] = json!(answer);
        data["answer_mode"] = json!("extractive");
        data["answer_chars"] = json!(answer.chars().count());
        data["passages"] = json!(passages);
    }
    ok("websearch", data)
}

pub fn timeout_ms_of(body: &Value, default_ms: u64, verb: &str) -> Result<u64, u64> {
    let has_explicit = body.get("timeoutMs").is_some()
        || body.get("opts").and_then(|o| o.get("timeoutMs")).is_some();
    if !has_explicit {
        return Ok(default_ms);
    }
    match crate::validation::validate_timeout_ms(body, true) {
        Ok(n) if n > super::FETCH_MAX_TIMEOUT_MS => Err(err_json(
            verb,
            json!({ "error": "timeoutMs above ceiling", "max": super::FETCH_MAX_TIMEOUT_MS, "received": n }),
        )),
        Ok(n) => Ok(n),
        Err(detail) => Err(err_json(verb, detail)),
    }
}

fn fetch_opts(body: &Value, timeout_ms: u64) -> Value {
    let mut opts = match body.get("opts") {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => json!({}),
    };
    if let Some(map) = opts.as_object_mut() {
        for key in ["method", "headers", "body"] {
            if let Some(value) = body.get(key) {
                map.insert((*key).to_string(), value.clone());
            }
        }
        map.insert("timeoutMs".to_string(), json!(timeout_ms));
    }
    opts
}

fn host_fetch_value(url: &str, opts: &Value) -> Value {
    let opts_s = opts.to_string();
    let packed = unsafe {
        host_fetch(
            url.as_ptr(),
            url.len() as u32,
            opts_s.as_ptr(),
            opts_s.len() as u32,
        )
    };
    unpack_to_value(packed)
}

fn flag(body: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| match body.get(*key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let t = s.trim().to_ascii_lowercase();
            t == "true" || t == "1" || t == "yes" || t == "on"
        }
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) > 0,
        _ => false,
    })
}

fn trimmed_str(body: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| body.get(*key).and_then(|v| v.as_str()))
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn bounded_usize(
    body: &Value,
    keys: &[&str],
    default: usize,
    ceiling: usize,
    verb: &str,
    label: &str,
) -> Result<usize, u64> {
    let mut value = default;
    for key in keys {
        let raw = match body.get(*key) {
            None => continue,
            Some(v) => v,
        };
        let parsed = match raw {
            Value::Number(n) => n.as_u64().map(|n| n as usize),
            Value::String(s) => s.trim().parse::<u64>().ok().map(|n| n as usize),
            _ => None,
        };
        match parsed {
            None => {
                return Err(err_json(
                    verb,
                    json!({ "error": format!("{label} must be a positive integer"), "field": key }),
                ))
            }
            Some(0) => {
                return Err(err_json(
                    verb,
                    json!({ "error": format!("{label} must be greater than zero"), "field": key }),
                ))
            }
            Some(n) if n > ceiling => {
                return Err(err_json(
                    verb,
                    json!({ "error": format!("{label} above ceiling"), "field": key, "max": ceiling, "received": n }),
                ))
            }
            Some(n) => {
                value = n;
                break;
            }
        }
    }
    Ok(value)
}

fn string_list(body: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        match body.get(*key) {
            Some(Value::Array(items)) => {
                return items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| normalize_domain(s))
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            Some(Value::String(s)) => {
                return s
                    .split(',')
                    .map(normalize_domain)
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    Vec::new()
}

fn normalize_domain(raw: &str) -> String {
    let mut d = raw.trim().to_ascii_lowercase();
    for prefix in ["https://", "http://"] {
        if let Some(rest) = d.strip_prefix(prefix) {
            d = rest.to_string();
        }
    }
    if let Some(slash) = d.find('/') {
        d = d[..slash].to_string();
    }
    d.trim_start_matches("www.").trim().to_string()
}

fn host_of(url: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    host.trim_start_matches("www.").to_ascii_lowercase()
}

fn host_matches_any(host: &str, domains: &[String]) -> bool {
    domains
        .iter()
        .any(|d| host == d.as_str() || host.ends_with(&format!(".{d}")))
}

pub fn clamp_chars(s: &str, max: usize) -> (String, bool) {
    let total = s.chars().count();
    if total <= max {
        return (s.to_string(), false);
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(&format!("\n...[+{} chars clipped; raise max_chars or pass prompt to answer against the page]", total - max));
    (out, true)
}

fn take_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn looks_like_html(s: &str) -> bool {
    let head: String = s.chars().take(4096).collect();
    let low = head.to_ascii_lowercase();
    low.contains("<!doctype")
        || low.contains("<html")
        || low.contains("<body")
        || low.contains("<div")
        || low.contains("<p>")
        || low.contains("<p ")
        || low.contains("<article")
        || low.contains("<main")
        || low.contains("<h1")
        || low.contains("<table")
        || low.contains("<ul")
        || low.contains("<a ")
}

fn starts_ci(chars: &[char], at: usize, pat: &str) -> bool {
    let mut k = at;
    for p in pat.chars() {
        match chars.get(k) {
            Some(c) if c.to_ascii_lowercase() == p.to_ascii_lowercase() => k += 1,
            _ => return false,
        }
    }
    true
}

fn find_ci(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let mut k = from;
    while k < chars.len() {
        if starts_ci(chars, k, pat) {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    let mut k = from;
    while k < chars.len() {
        if chars[k] == needle {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn after_gt(chars: &[char], from: usize) -> usize {
    match find_char(chars, from, '>') {
        Some(g) => g + 1,
        None => chars.len(),
    }
}

fn matches_tag(chars: &[char], at: usize, pat: &str) -> bool {
    if !starts_ci(chars, at, pat) {
        return false;
    }
    match chars.get(at + pat.chars().count()) {
        None => true,
        Some(c) => !c.is_ascii_alphanumeric() && *c != '-' && *c != '_',
    }
}

fn tag_name_at(chars: &[char], at: usize) -> Option<String> {
    if chars.get(at) != Some(&'<') {
        return None;
    }
    let mut k = at + 1;
    if chars.get(k) == Some(&'/') {
        k += 1;
    }
    let mut name = String::new();
    while let Some(c) = chars.get(k) {
        if c.is_ascii_alphanumeric() {
            name.push(*c);
            k += 1;
        } else {
            break;
        }
    }
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

fn matching_close(chars: &[char], start: usize, name: &str) -> Option<(usize, usize)> {
    let open = format!("<{}", name);
    let close = format!("</{}", name);
    let mut depth = 1usize;
    let mut k = start + 1;
    while k < chars.len() {
        if chars[k] == '<' {
            if matches_tag(chars, k, &close) {
                depth -= 1;
                if depth == 0 {
                    return Some((k, after_gt(chars, k)));
                }
                k += close.chars().count();
                continue;
            }
            if matches_tag(chars, k, &open) {
                depth += 1;
                k += open.chars().count();
                continue;
            }
        }
        k += 1;
    }
    None
}

fn find_open_tag(chars: &[char], from: usize, name: &str) -> Option<usize> {
    let open = format!("<{}", name);
    let mut k = from;
    while k < chars.len() {
        if chars[k] == '<' && matches_tag(chars, k, &open) {
            return Some(k);
        }
        k += 1;
    }
    None
}

pub fn strip_noise(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        if chars[i] == '<' {
            if starts_ci(&chars, i, "<!--") {
                match find_ci(&chars, i + 4, "-->") {
                    Some(e) => {
                        i = e + 3;
                        continue;
                    }
                    None => {
                        i = n;
                        continue;
                    }
                }
            }
            if let Some(name) = tag_name_at(&chars, i) {
                if NOISE_TAGS.contains(&name.as_str()) {
                    match matching_close(&chars, i, &name) {
                        Some((_, next)) => {
                            i = next;
                            continue;
                        }
                        None => {
                            let next = after_gt(&chars, i);
                            i = next;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub fn extract_main(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut best: Option<(usize, usize)> = None;
    for name in ["article", "main"] {
        let mut from = 0usize;
        while let Some(start) = find_open_tag(&chars, from, name) {
            match matching_close(&chars, start, name) {
                Some((close_start, next)) => {
                    let len = close_start.saturating_sub(start);
                    let better = match best {
                        None => true,
                        Some((s, e)) => len > e.saturating_sub(s),
                    };
                    if better {
                        best = Some((start, close_start));
                    }
                    from = next;
                }
                None => from = start + name.chars().count() + 1,
            }
        }
    }
    if let Some((start, close_start)) = best {
        let content_start = after_gt(&chars, start);
        if close_start > content_start {
            let inner: String = chars[content_start..close_start].iter().collect();
            if inner.chars().count() > 200 {
                return inner;
            }
        }
    }
    html.to_string()
}

const MAIN_REGION_MIN_CHARS: usize = 500;

pub fn html_to_markdown(html: &str) -> String {
    let cleaned = strip_noise(html);
    let region = tags_to_markdown(&extract_main(&cleaned));
    if region.chars().count() >= MAIN_REGION_MIN_CHARS {
        return region;
    }
    let whole = tags_to_markdown(&cleaned);
    if whole.chars().count() > region.chars().count() {
        whole
    } else {
        region
    }
}

pub fn tags_to_markdown(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n / 2 + 64);
    let mut hrefs: Vec<String> = Vec::new();
    let mut in_pre = false;
    let mut i = 0usize;
    while i < n {
        if chars[i] != '<' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if starts_ci(&chars, i, "<!--") {
            match find_ci(&chars, i + 4, "-->") {
                Some(e) => i = e + 3,
                None => i = n,
            }
            continue;
        }
        let gt = match find_char(&chars, i, '>') {
            Some(g) => g,
            None => {
                i = n;
                continue;
            }
        };
        let raw: String = chars[i + 1..gt].iter().collect();
        i = gt + 1;
        let low = raw.to_ascii_lowercase();
        let (is_close, rest) = match low.strip_prefix('/') {
            Some(r) => (true, r),
            None => (false, low.as_str()),
        };
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        match name.as_str() {
            "br" => out.push('\n'),
            "hr" => {
                blank(&mut out);
                out.push_str("---");
                blank(&mut out);
            }
            "script" | "style" | "noscript" | "svg" | "template" | "iframe" | "select"
            | "textarea" => {
                let close = format!("</{}", name);
                if let Some(e) = find_ci(&chars, i, &close) {
                    i = after_gt(&chars, e);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if is_close {
                    blank(&mut out);
                } else {
                    blank(&mut out);
                    let level = name
                        .chars()
                        .nth(1)
                        .and_then(|c| c.to_digit(10))
                        .unwrap_or(1) as usize;
                    for _ in 0..level {
                        out.push('#');
                    }
                    out.push(' ');
                }
            }
            "p" | "div" | "section" | "article" | "main" | "aside" | "header" | "footer" | "nav"
            | "figure" | "figcaption" | "form" | "table" | "thead" | "tbody" | "blockquote"
            | "ul" | "ol" | "dl" | "fieldset" => blank(&mut out),
            "tr" => out.push('\n'),
            "li" | "dt" | "dd" => {
                if is_close {
                    out.push('\n');
                } else {
                    out.push('\n');
                    out.push_str("- ");
                }
            }
            "td" | "th" => {
                if !is_close {
                    out.push_str(" | ");
                }
            }
            "pre" => {
                if is_close {
                    out.push_str("\n```");
                    blank(&mut out);
                    in_pre = false;
                } else {
                    blank(&mut out);
                    out.push_str("```\n");
                    in_pre = true;
                }
            }
            "code" => {
                if !in_pre {
                    out.push('`');
                }
            }
            "strong" | "b" => out.push_str("**"),
            "em" | "i" => out.push('*'),
            "a" => {
                if is_close {
                    out.push(']');
                    out.push('(');
                    out.push_str(hrefs.pop().unwrap_or_default().as_str());
                    out.push(')');
                } else {
                    hrefs.push(attr_value(&raw, "href").unwrap_or_default());
                    out.push('[');
                }
            }
            "img" => {
                let src = attr_value(&raw, "src").unwrap_or_default();
                let alt = attr_value(&raw, "alt").unwrap_or_default();
                out.push_str("![");
                out.push_str(&alt);
                out.push_str("](");
                out.push_str(&src);
                out.push(')');
            }
            _ => {}
        }
    }
    tidy(&out)
}

fn blank(out: &mut String) {
    if out.is_empty() {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
}

fn attr_value(raw: &str, name: &str) -> Option<String> {
    let low = raw.to_ascii_lowercase();
    let needle = format!("{}=", name);
    let mut from = 0usize;
    while let Some(rel) = low[from..].find(&needle) {
        let at = from + rel;
        let preceded_ok = at == 0
            || raw[..at]
                .chars()
                .last()
                .map(|c| c.is_whitespace())
                .unwrap_or(false);
        let value_at = at + needle.len();
        if preceded_ok {
            let rest = &raw[value_at..];
            match rest.chars().next() {
                Some(q) if q == '"' || q == '\'' => {
                    let inner: String = rest[1..].chars().take_while(|c| *c != q).collect();
                    return Some(decode_entities(&inner));
                }
                Some(_) => {
                    let inner: String = rest
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != '>')
                        .collect();
                    return Some(decode_entities(&inner));
                }
                None => return Some(String::new()),
            }
        }
        from = value_at;
    }
    None
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '&' {
            if let Some(rel) = chars[i + 1..].iter().position(|c| *c == ';') {
                let semi = i + 1 + rel;
                if semi - i <= 12 {
                    let ent: String = chars[i + 1..semi].iter().collect();
                    if let Some(repl) = entity_char(&ent) {
                        out.push(repl);
                        i = semi + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn entity_char(ent: &str) -> Option<char> {
    match ent {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "mdash" => Some('-'),
        "ndash" => Some('-'),
        "hellip" => Some('.'),
        "#39" => Some('\''),
        "#34" => Some('"'),
        _ => {
            if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = ent.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn tidy(s: &str) -> String {
    let decoded = decode_entities(s);
    let mut lines: Vec<String> = Vec::new();
    let mut blank_run = 0usize;
    for raw_line in decoded.split('\n') {
        let collapsed = collapse_spaces(raw_line.trim());
        if collapsed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !lines.is_empty() {
                lines.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        if collapsed
            .chars()
            .all(|c| c == '|' || c == ' ' || c == '-' || c == '*')
        {
            continue;
        }
        lines.push(collapsed);
    }
    let mut text = lines.join("\n");
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text.trim().to_string()
}

fn prompt_terms(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in prompt.split(|c: char| !c.is_ascii_alphanumeric()) {
        let w = word.to_ascii_lowercase();
        if w.chars().count() < 3 || STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

fn split_blocks(markdown: &str) -> Vec<String> {
    markdown
        .split("\n\n")
        .map(|b| b.trim())
        .filter(|b| b.chars().count() >= 40)
        .map(|b| b.to_string())
        .collect()
}

fn document_frequencies(blocks: &[String], terms: &[String]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for t in terms {
        let needle = t.as_str();
        let count = blocks
            .iter()
            .filter(|b| b.to_ascii_lowercase().contains(needle))
            .count();
        m.insert(t.clone(), count);
    }
    m
}

fn matched_terms(block: &str, terms: &[String]) -> Vec<String> {
    let low = block.to_ascii_lowercase();
    terms
        .iter()
        .filter(|t| low.contains(t.as_str()))
        .cloned()
        .collect()
}

fn score_block(block: &str, terms: &[String], df: &HashMap<String, usize>, total: f64) -> f64 {
    let low = block.to_ascii_lowercase();
    let mut score = 0.0f64;
    let mut hits = 0usize;
    for t in terms {
        if low.contains(t.as_str()) {
            hits += 1;
            let d = *df.get(t).unwrap_or(&1) as f64;
            score += 1.0 + (total / (1.0 + d)).ln().max(0.0);
        }
    }
    if hits == 0 {
        return 0.0;
    }
    score += hits as f64 * 0.5;
    if block.chars().count() < 1200 {
        score += 0.5;
    }
    if low.starts_with('#') {
        score += 0.75;
    }
    score
}

pub fn extractive_answer(markdown: &str, prompt: &str, budget: usize) -> (String, Vec<Value>) {
    let terms = prompt_terms(prompt);
    let blocks = split_blocks(markdown);
    if terms.is_empty() || blocks.is_empty() {
        return (take_chars(markdown, budget), Vec::new());
    }
    let total = blocks.len() as f64;
    let df = document_frequencies(&blocks, &terms);
    let mut scored: Vec<(usize, f64)> = blocks
        .iter()
        .enumerate()
        .map(|(idx, b)| (idx, score_block(b, &terms, &df, total)))
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut answer = String::new();
    let mut passages: Vec<Value> = Vec::new();
    let mut used = 0usize;
    for (idx, score) in scored {
        if used >= budget || passages.len() >= 8 {
            break;
        }
        let room = budget.saturating_sub(used);
        if room < 80 {
            break;
        }
        let piece = take_chars(&blocks[idx], room);
        if piece.is_empty() {
            continue;
        }
        if !answer.is_empty() {
            answer.push_str("\n\n");
        }
        answer.push_str(&piece);
        used += piece.chars().count() + 2;
        passages.push(json!({
            "chars": piece.chars().count(),
            "score": (score * 100.0).round() / 100.0,
            "matched_terms": matched_terms(&blocks[idx], &terms),
        }));
    }
    if passages.is_empty() {
        (take_chars(markdown, budget), Vec::new())
    } else {
        (answer, passages)
    }
}

struct SearchResult {
    title: String,
    url: String,
    host: String,
    snippet: String,
}

fn parse_results(html: &str) -> Vec<SearchResult> {
    let duck = parse_duckduckgo_results(html);
    if !duck.is_empty() {
        return duck;
    }
    parse_generic_results(html)
}

fn parse_duckduckgo_results(html: &str) -> Vec<SearchResult> {
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<SearchResult> = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("result__a") {
        let at = from + rel;
        let href_at = match lower[at..].find("href=\"") {
            Some(r) => at + r + 6,
            None => break,
        };
        let href_end = match lower[href_at..].find('"') {
            Some(r) => href_at + r,
            None => break,
        };
        let gt = match html[href_end..].find('>') {
            Some(r) => href_end + r,
            None => break,
        };
        let close = match lower[gt..].find("</a>") {
            Some(r) => gt + r,
            None => break,
        };
        let url = resolve_result_url(&html[href_at..href_end]);
        let title = clean_text(&html[gt + 1..close]);
        let snippet = match lower[close..].find("result__snippet") {
            Some(r) => {
                let s_at = close + r;
                match html[s_at..].find('>') {
                    Some(g) => {
                        let text_at = s_at + g + 1;
                        let tail = &html[text_at..];
                        let end = match tail.to_ascii_lowercase().find("</a>") {
                            Some(e) => e,
                            None => match tail.to_ascii_lowercase().find("</div>") {
                                Some(e) => e,
                            None => tail.len().min(1200),
                            },
                        };
                        clean_text(&tail[..end.min(tail.len())])
                    }
                    None => String::new(),
                }
            }
            None => String::new(),
        };
        let host = host_of(&url);
        if title.is_empty() || host.contains("duckduckgo.com") || url.is_empty() {
            from = close + 4;
            continue;
        }
        from = close + 4;
        out.push(SearchResult {
            title,
            url,
            host,
            snippet,
        });
        if out.len() >= 64 {
            break;
        }
    }
    out
}

fn parse_generic_results(html: &str) -> Vec<SearchResult> {
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<SearchResult> = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("<a ") {
        let at = from + rel;
        let href_at = match lower[at..].find("href=\"") {
            Some(r) => at + r + 6,
            None => break,
        };
        let href_end = match lower[href_at..].find('"') {
            Some(r) => href_at + r,
            None => break,
        };
        let gt = match html[href_end..].find('>') {
            Some(r) => href_end + r,
            None => break,
        };
        let close = match lower[gt..].find("</a>") {
            Some(r) => gt + r,
            None => break,
        };
        let url = resolve_result_url(&html[href_at..href_end]);
        let title = clean_text(&html[gt + 1..close]);
        from = close + 4;
        let host = host_of(&url);
        if !url.starts_with("http")
            || title.chars().count() < 3
            || host.contains("duckduckgo.com")
            || host.is_empty()
        {
            continue;
        }
        out.push(SearchResult {
            title,
            url,
            host,
            snippet: String::new(),
        });
        if out.len() >= 64 {
            break;
        }
    }
    out
}

fn clean_text(raw: &str) -> String {
    let stripped = strip_noise(raw);
    let mut out = String::with_capacity(stripped.len());
    let chars: Vec<char> = stripped.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '<' {
            match find_char(&chars, i, '>') {
                Some(g) => i = g + 1,
                None => i = chars.len(),
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    tidy(&decode_entities(&out))
}

fn resolve_result_url(href: &str) -> String {
    let mut h = href.trim().to_string();
    if h.starts_with("//") {
        h = format!("https:{}", h);
    }
    for prefix in ["https://duckduckgo.com/l/?", "http://duckduckgo.com/l/?"] {
        if let Some(rest) = h.strip_prefix(prefix) {
            for pair in rest.split('&') {
                if let Some(v) = pair.strip_prefix("uddg=") {
                    return percent_decode(v);
                }
            }
        }
    }
    h
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

const SEARCH_ENGINE_HOSTS: &[&str] = &[
    "duckduckgo.com", "bing.com", "microsoft.com", "msn.com", "live.com", "mojeek.com",
    "searx.be", "searxng.site", "brave.com", "search.brave.com", "marginalia.nu", "google.com",
    "yandex.com", "startpage.com", "ecosia.org", "qwant.com",
];

const SEARCH_SNIPPET_SCAN_CHARS: usize = 1_200;

fn search_reply(
    body: &Value,
    query: &str,
    verb: &str,
    limit_keys: &[&str],
    default_limit: usize,
) -> u64 {
    let limit = match bounded_usize(
        body,
        limit_keys,
        default_limit,
        FETCH_SEARCH_MAX_LIMIT,
        verb,
        "count",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let timeout_ms = match timeout_ms_of(body, WEBSEARCH_DEFAULT_TIMEOUT_MS, verb) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let raw_requested = flag(body, &["raw", "full_response", "fullResponse"]);
    let max_chars = match bounded_usize(
        body,
        &["max_chars", "maxChars"],
        if raw_requested {
            FETCH_RAW_DEFAULT_MAX_CHARS
        } else {
            FETCH_DEFAULT_MAX_CHARS
        },
        FETCH_MAX_MAX_CHARS,
        verb,
        "max_chars",
    ) {
        Ok(n) => n,
        Err(reply) => return reply,
    };
    let allowed = string_list(body, &["allowed_domains", "allowedDomains"]);
    let blocked = string_list(body, &["blocked_domains", "blockedDomains"]);
    let backends = search_backends(body);
    let tried: Vec<String> = backends
        .iter()
        .take(SEARCH_MAX_BACKENDS)
        .map(|(name, _)| name.clone())
        .collect();
    let mut attempts: Vec<Value> = Vec::new();
    for (name, template) in backends.iter().take(SEARCH_MAX_BACKENDS) {
        let endpoint = backend_url(template, query);
        if let Err(reason) = validate_fetch_url(&endpoint) {
            attempts.push(json!({ "backend": name, "error": reason }));
            continue;
        }
        let opts = json!({
            "timeoutMs": timeout_ms,
            "headers": { "User-Agent": WEBSEARCH_USER_AGENT, "Accept-Language": "en-US,en;q=0.9" },
        });
        let v = host_fetch_value(&endpoint, &opts);
        let completed = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
        let status = v.get("status").cloned().unwrap_or(Value::Null);
        let html = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
        if !completed || html.is_empty() {
            attempts.push(json!({
                "backend": name,
                "status": status,
                "error": v.get("error").cloned().unwrap_or_else(|| json!(format!("http status {status}"))),
            }));
            continue;
        }
        let mut data = json!({
            "ok": true,
            "mode": "search",
            "query": query,
            "backend": name,
            "endpoint": endpoint,
            "status": status,
            "body_chars": html.chars().count(),
        });
        if raw_requested {
            let (text, truncated) = clamp_chars(html, max_chars);
            data["body"] = json!(text);
            data["truncated"] = json!(truncated);
            if !attempts.is_empty() {
                data["attempts"] = json!(attempts);
            }
            return ok(verb, data);
        }
        let mut results = parse_backend_results(html);
        if !allowed.is_empty() {
            results.retain(|r| host_matches_any(&r.host, &allowed));
        }
        if !blocked.is_empty() {
            results.retain(|r| !host_matches_any(&r.host, &blocked));
        }
        if results.is_empty() {
            attempts.push(json!({
                "backend": name,
                "status": status,
                "hits": 0,
                "error": "no result parsed out of the response",
            }));
            continue;
        }
        let found = results.len();
        results.truncate(limit);
        data["count"] = json!(results.len());
        data["found"] = json!(found);
        data["limit"] = json!(limit);
        data["truncated"] = json!(found > results.len());
        data["filters"] = json!({ "allowed_domains": allowed, "blocked_domains": blocked });
        data["results"] = json!(results
            .iter()
            .enumerate()
            .map(|(rank, r)| json!({
                "rank": rank + 1,
                "title": r.title,
                "url": r.url,
                "host": r.host,
                "snippet": clamp_chars(&r.snippet, 400).0,
            }))
            .collect::<Vec<Value>>());
        if !attempts.is_empty() {
            data["attempts"] = json!(attempts);
        }
        return ok(verb, data);
    }
    err_json(
        verb,
        json!({
            "error": "no search backend returned hits",
            "query": query,
            "mode": "search",
            "backends": tried,
            "attempts": attempts,
            "note": "every keyless backend answered with a challenge page or nothing parseable. Pass url with a results page of your own, name a backend with backend, or fetch a known url directly.",
        }),
    )
}

fn parse_backend_results(html: &str) -> Vec<SearchResult> {
    if looks_like_challenge(html) {
        return Vec::new();
    }
    if html.to_ascii_lowercase().contains("result__a") {
        return parse_results(html);
    }
    parse_anchor_results(html)
}

fn looks_like_challenge(html: &str) -> bool {
    if html.chars().count() > 20_000 {
        return false;
    }
    let lower = html.to_ascii_lowercase();
    [
        "enable javascript",
        "javascript is required",
        "cf-browser-verification",
        "checking your browser",
        "just a moment",
        "captcha",
        "are you a robot",
        "unusual traffic",
        "verify you are human",
        "attention required",
        "access denied",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn parse_anchor_results(html: &str) -> Vec<SearchResult> {
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<SearchResult> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("<a ") {
        let at = from + rel;
        let href_at = match lower[at..].find("href=\"") {
            Some(r) => at + r + 6,
            None => break,
        };
        let href_end = match lower[href_at..].find('"') {
            Some(r) => href_at + r,
            None => break,
        };
        let gt = match html[href_end..].find('>') {
            Some(r) => href_end + r,
            None => break,
        };
        let close = match lower[gt..].find("</a>") {
            Some(r) => gt + r,
            None => break,
        };
        let url = unwrap_backend_url(&html[href_at..href_end]);
        let title = clean_text(&html[gt + 1..close]);
        let snippet = snippet_after(html, &lower, close + 4);
        from = close + 4;
        let host = host_of(&url);
        let key = url.split('#').next().unwrap_or(&url).to_string();
        if !url.starts_with("http")
            || title.chars().count() < 3
            || host.is_empty()
            || is_engine_host(&host)
            || seen.contains(&key)
        {
            continue;
        }
        seen.push(key);
        out.push(SearchResult {
            title,
            url,
            host,
            snippet,
        });
        if out.len() >= 64 {
            break;
        }
    }
    out
}

fn snippet_after(html: &str, lower: &str, from: usize) -> String {
    let limit = (from + SEARCH_SNIPPET_SCAN_CHARS).min(html.len());
    let end = lower[from..]
        .find("<a ")
        .map(|r| from + r)
        .unwrap_or(limit)
        .min(limit);
    if end <= from {
        return String::new();
    }
    let text = clean_text(&html[from..end]);
    if text.chars().count() < 25 {
        return String::new();
    }
    clamp_chars(&text, 400).0
}

fn is_engine_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    SEARCH_ENGINE_HOSTS
        .iter()
        .any(|bad| host == *bad || host.ends_with(&format!(".{bad}")))
}

fn unwrap_backend_url(href: &str) -> String {
    let mut h = href.trim().to_string();
    if h.starts_with("//") {
        h = format!("https:{h}");
    }
    let host = host_of(&h);
    let (path, query) = match h.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (h.clone(), String::new()),
    };
    if host.ends_with("duckduckgo.com") {
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("uddg=") {
                let decoded = percent_decode(v);
                if decoded.starts_with("http") {
                    return decoded;
                }
            }
        }
        return String::new();
    }
    if (host.ends_with("bing.com") || host.ends_with("microsoft.com")) && path.ends_with("/ck/a") {
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("u=") {
                let payload = v.strip_prefix("a1").unwrap_or(v);
                if let Some(decoded) = base64_url_target(payload) {
                    return decoded;
                }
            }
        }
        return String::new();
    }
    h
}

fn base64_url_target(payload: &str) -> Option<String> {
    let bytes = base64_decode(payload)
        .ok()
        .or_else(|| base64_decode(&payload.replace('-', "+").replace('_', "/")).ok())?;
    let text = String::from_utf8(bytes).ok()?;
    if text.starts_with("http://") || text.starts_with("https://") {
        Some(text)
    } else {
        None
    }
}

fn search_backends(body: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in backend_names(body) {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if name.starts_with("http://") || name.starts_with("https://") {
            out.push(("custom".to_string(), name.clone()));
            continue;
        }
        if let Some((_, template)) = SEARCH_BACKENDS.iter().find(|(known, _)| *known == name) {
            out.push((name.clone(), (*template).to_string()));
        }
    }
    if out.is_empty() {
        return SEARCH_BACKENDS
            .iter()
            .map(|(name, template)| ((*name).to_string(), (*template).to_string()))
            .collect();
    }
    for (name, template) in SEARCH_BACKENDS {
        if !out.iter().any(|(chosen, _)| chosen == name) {
            out.push(((*name).to_string(), (*template).to_string()));
        }
    }
    out
}

fn backend_names(body: &Value) -> Vec<String> {
    for key in ["backend", "engine", "search_backend"] {
        match body.get(key) {
            Some(Value::String(s)) => return split_backend_list(s),
            Some(Value::Array(items)) => {
                return items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.to_string())
                    .collect()
            }
            _ => {}
        }
    }
    env_backend().unwrap_or_default()
}

fn env_backend() -> Option<Vec<String>> {
    let packed = unsafe {
        host_env_get(
            SEARCH_ENV_BACKEND.as_ptr(),
            SEARCH_ENV_BACKEND.len() as u32,
        )
    };
    unpack_to_string(packed)
        .map(|raw| split_backend_list(&raw))
        .filter(|names: &Vec<String>| !names.is_empty())
}

fn split_backend_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn backend_url(template: &str, query: &str) -> String {
    let encoded = percent_encode(query);
    if template.contains("{q}") {
        template.replace("{q}", &encoded)
    } else {
        format!("{template}{encoded}")
    }
}
