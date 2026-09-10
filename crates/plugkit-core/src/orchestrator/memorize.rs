use std::path::PathBuf;
use super::gm_dir;
use crate::pkfs;

pub fn memorize_inbox() -> PathBuf {
    gm_dir().join("exec-spool").join("in").join("memorize")
}

pub fn fire(body: &str) -> Result<String, std::io::Error> {
    let dir = memorize_inbox();
    #[cfg(target_arch = "wasm32")]
    let n: u128 = (unsafe { crate::wasm_dispatch::host_now_ms() } as u128) * 1_000_000
        + (body.len() as u128);
    #[cfg(not(target_arch = "wasm32"))]
    let n: u128 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = dir.join(format!("{}.md", n));
    let ps = p.to_string_lossy().to_string();
    if pkfs::write(&ps, body) {
        Ok(p.display().to_string())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "pkfs write failed"))
    }
}

#[cfg(target_arch = "wasm32")]
fn parsed_kind_or_default(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_else(|| "l0".to_string())
}

#[cfg(target_arch = "wasm32")]
fn strip_sha_shaped_tokens(text: &str) -> String {
    text.split_whitespace()
        .filter(|tok| {
            let cleaned = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            !(cleaned.len() >= 7 && cleaned.len() <= 40
                && cleaned.chars().all(|c| c.is_ascii_hexdigit())
                && cleaned.chars().any(|c| c.is_ascii_digit())
                && cleaned.chars().any(|c| c.is_ascii_alphabetic()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_arch = "wasm32")]
fn strip_version_shaped_tokens(text: &str) -> String {
    text.split_whitespace()
        .filter(|tok| {
            let cleaned = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
            let digits_dotted = cleaned.strip_prefix('v').unwrap_or(cleaned);
            let looks_semver = digits_dotted.split('.').count() >= 2
                && digits_dotted.len() >= 3
                && digits_dotted.chars().all(|c| c.is_ascii_digit() || c == '.')
                && digits_dotted.chars().any(|c| c.is_ascii_digit());
            !looks_semver
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_arch = "wasm32")]
fn strip_dated_audit_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let l = line.to_lowercase();
            let has_date = l.contains("(202") || l.contains("(19")
                || (l.len() >= 10 && l.as_bytes().windows(10).any(|w| {
                    w.iter().take(4).all(|c| c.is_ascii_digit())
                        && w[4] == b'-'
                        && w[5..7].iter().all(|c| c.is_ascii_digit())
                        && w[7] == b'-'
                        && w[8..10].iter().all(|c| c.is_ascii_digit())
                }));
            let audit_shaped = l.contains("audit") || l.contains("(fixed)");
            !(has_date && audit_shaped)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(target_arch = "wasm32")]
fn has_generalizable_residue(text: &str) -> bool {
    let stripped = strip_dated_audit_lines(&strip_version_shaped_tokens(&strip_sha_shaped_tokens(text)));
    stripped.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphabetic())).count() >= 8
}

#[cfg(target_arch = "wasm32")]
fn has_banned_glyph(text: &str) -> Option<char> {
    text.chars().find(|c| {
        if c.is_ascii() { return false; }
        matches!(*c,
            '\u{2190}'..='\u{21FF}'
            | '\u{2500}'..='\u{259F}'
            | '\u{25A0}'..='\u{25FF}'
            | '\u{2600}'..='\u{27BF}'
            | '\u{1F300}'..='\u{1FAFF}'
            | '\u{2022}' | '\u{2023}' | '\u{2043}'
            | '\u{2B50}'
        )
    })
}

#[cfg(target_arch = "wasm32")]
fn is_derivable_state(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() > 40 && t.chars().filter(|c| c.is_ascii_hexdigit()).count() == t.len() {
        return Some("memo is a hex hash; git log is the source of truth".to_string());
    }
    if let Some(glyph) = has_banned_glyph(t) {
        return Some(format!("memo contains a banned decorative glyph ({:?}); convert to plain ASCII per the glyph-discipline rule", glyph));
    }
    let lower = t.to_lowercase();
    let bad: &[(&str, &str)] = &[
        ("we used to ", "historical framing belongs in git log + CHANGELOG, not the recall index"),
        ("used to do", "historical framing belongs in git log + CHANGELOG, not the recall index"),
        ("previously did", "historical framing belongs in git log + CHANGELOG, not the recall index"),
        ("(fixed)", "past-tense fix markers belong in commit messages"),
        ("fixed in commit", "commit-fix references belong in git log, not the recall index"),
        ("fix in commit", "commit-fix references belong in git log, not the recall index"),
        ("changelog:", "changelog entries live in CHANGELOG.md"),
        ("changelog entry", "changelog entries live in CHANGELOG.md"),
        ("dated audit", "dated audit entries belong in git log, not the recall index"),
        ("(added 20", "dated (added YYYY-..) annotations belong in git log, not the recall index"),
        ("commit hash", "commit hashes are derivable from git log"),
        ("recent commit", "recent commits are derivable from git log"),
        ("git blame says", "git blame is derivable from the repo"),
    ];
    for (pat, msg) in bad {
        if lower.contains(pat) { return Some(msg.to_string()); }
    }
    if !has_generalizable_residue(t) {
        return Some("memo is sha-only/version-only/dated-audit-only with no generalizable lesson surviving after those tokens/lines are stripped; per the exclusion principle, config-derivable, code-derivable, and state-of-tooling-snapshot content never enters the store".to_string());
    }
    None
}

/// Rows whose text is stored but whose vector could not be computed yet.
///
/// Refusing a silent NULL-embedding insert is right -- a row with a zero or
/// absent vector poisons cosine ranking and reads as a genuine match at
/// distance 0. But refusing the WHOLE write was the wrong consequence: the
/// lesson an agent was instructed to persist was discarded, with no record that
/// it had ever existed, while gm's own graph mandates `memorize-fire` at every
/// M_RECORD pass. So the text goes to the durable md corpus AND to the flat kv
/// store (which is exactly what `recall`'s degraded keyword path scans when the
/// embedder is down), the key is queued here, and the response says plainly that
/// the row carries no vector. "Stored, vector pending, and said so" replaces
/// both "stored silently as if fine" and "nothing stored at all".
#[cfg(target_arch = "wasm32")]
const EMBED_PENDING_LEDGER_FILE: &str = ".gm/exec-spool/.memorize-embed-pending.json";

/// Bounded so a long embedder outage cannot grow an unbounded file. The md
/// corpus is the durable store either way; this ledger only tracks which keys
/// still owe a vector, and the oldest entries are the ones most likely to have
/// been superseded.
#[cfg(target_arch = "wasm32")]
const EMBED_PENDING_LEDGER_MAX_ROWS: usize = 500;

/// How many queued rows one successful `memorize-fire` opportunistically
/// backfills. A successful fire is direct proof the embedder is up again, so it
/// is the cheapest possible recovery trigger -- no separate poll, no daemon
/// timer. Bounded per call so a 500-row backlog cannot turn one fire into a
/// minutes-long dispatch; `memorize-backfill` drains the rest on demand.
#[cfg(target_arch = "wasm32")]
const EMBED_PENDING_DRAIN_PER_SUCCESSFUL_FIRE: usize = 8;

#[cfg(target_arch = "wasm32")]
fn read_pending_ledger() -> Vec<serde_json::Value> {
    crate::wasm_dispatch::host_read(EMBED_PENDING_LEDGER_FILE)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn write_pending_ledger(rows: &[serde_json::Value]) -> bool {
    let payload = serde_json::Value::Array(rows.to_vec());
    crate::wasm_dispatch::host_write(EMBED_PENDING_LEDGER_FILE, &payload.to_string())
}

#[cfg(target_arch = "wasm32")]
fn queue_pending_embedding(
    namespace: &str,
    key: &str,
    kind: &str,
    text: &str,
    reason: &str,
    tencentdb: bool,
    now_ms: i64,
) -> usize {
    let mut rows = read_pending_ledger();
    rows.retain(|r| {
        !(r.get("key").and_then(|v| v.as_str()) == Some(key)
            && r.get("namespace").and_then(|v| v.as_str()) == Some(namespace))
    });
    rows.push(serde_json::json!({
        "namespace": namespace,
        "key": key,
        "kind": kind,
        "text": text,
        "reason": reason,
        "tencentdb": tencentdb,
        "queued_at_ms": now_ms,
    }));
    while rows.len() > EMBED_PENDING_LEDGER_MAX_ROWS {
        rows.remove(0);
    }
    let total = rows.len();
    write_pending_ledger(&rows);
    total
}

/// Re-embeds queued rows and promotes each into the vector store, dropping only
/// the ones that actually succeeded. A row whose embedding still fails stays
/// queued, so a partial recovery loses nothing.
#[cfg(target_arch = "wasm32")]
fn drain_pending_embeddings(max_rows: usize) -> serde_json::Value {
    let rows = read_pending_ledger();
    if rows.is_empty() {
        return serde_json::json!({ "embedded": 0, "still_pending": 0, "attempted": 0 });
    }
    let now = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
    let mut kept: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
    let mut embedded = 0usize;
    let mut attempted = 0usize;
    let mut last_error: Option<String> = None;
    for row in rows {
        let namespace = row.get("namespace").and_then(|v| v.as_str()).unwrap_or("default").to_string();
        let key = row.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let text = row.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if key.is_empty() || text.is_empty() || attempted >= max_rows {
            if !key.is_empty() && !text.is_empty() {
                kept.push(row);
            }
            continue;
        }
        attempted += 1;
        let emb = match crate::embed::embed_text_json(&text) {
            Some(v) => v,
            None => {
                last_error = crate::embed::last_embed_failure();
                kept.push(row);
                continue;
            }
        };
        let tencentdb = row.get("tencentdb").and_then(|v| v.as_bool()).unwrap_or(false);
        let write_result: Result<(), String> = if tencentdb {
            let kind = row.get("kind").and_then(|v| v.as_str()).unwrap_or("l0");
            crate::tencentdb_memory::write(&namespace, kind, &text, &emb, now).map(|_| ())
        } else {
            crate::rssearch_vectors::write(&namespace, &key, &text, &emb, now)
        };
        match write_result {
            Ok(()) => embedded += 1,
            Err(e) => {
                last_error = Some(e);
                kept.push(row);
            }
        }
    }
    let still_pending = kept.len();
    write_pending_ledger(&kept);
    if embedded > 0 || still_pending > 0 {
        crate::wasm_dispatch::emit_event("memorize_embed_backfill", serde_json::json!({
            "embedded": embedded,
            "still_pending": still_pending,
            "attempted": attempted,
            "last_error": last_error,
        }));
    }
    serde_json::json!({
        "embedded": embedded,
        "still_pending": still_pending,
        "attempted": attempted,
        "last_error": last_error,
    })
}

/// The degraded write: text durable, vector owed, disclosure mandatory.
#[cfg(target_arch = "wasm32")]
fn store_without_vector(
    namespace: &str,
    key: &str,
    kind: &str,
    text: &str,
    why: &str,
    tencentdb: bool,
    now_ms: i64,
) -> (String, String, i32) {
    let md_path = match crate::memory_md::write_memory(namespace, key, text, now_ms) {
        crate::memory_md::WriteOutcome::Created(p)
        | crate::memory_md::WriteOutcome::Updated(p)
        | crate::memory_md::WriteOutcome::Deduped(p) => Some(p),
        crate::memory_md::WriteOutcome::Invalid(reason) => {
            crate::wasm_dispatch::emit_event("memory_md_write_invalid", serde_json::json!({
                "key": key, "namespace": namespace, "reason": reason,
            }));
            return (String::new(), format!("memorize: md write invalid: {}", reason), 1);
        }
        crate::memory_md::WriteOutcome::Failed(p) => {
            return (String::new(), format!("memorize: md write failed at {}; the md corpus is the durable store, refusing an unbacked memory", p), 1);
        }
    };
    // Deliberately NOT mirrored into the flat kv store. host_kv is libsql-backed,
    // so the same outage that takes the embedder down leaves the libsql pool with
    // no instantiated slot -- and the pool's FIFO wait does not deny, it waits, so
    // the kv write blocks for minutes and still lands nothing. Measured on this
    // path: a degraded fire that wrote the md file in milliseconds then sat in
    // host_kv_put long enough that the ledger entry below never got written within
    // the dispatch. The md corpus is the durable store and `recall`'s md keyword
    // scan reads it directly, so the kv mirror bought nothing and cost the whole
    // degraded write.
    let queued_total = queue_pending_embedding(namespace, key, kind, text, why, tencentdb, now_ms);
    crate::wasm_dispatch::emit_event("memorize_stored_without_vector", serde_json::json!({
        "key": key,
        "namespace": namespace,
        "reason": why,
        "pending_total": queued_total,
        "tencentdb_push_pending": tencentdb,
    }));
    let mut payload = serde_json::json!({
        "ok": true,
        "key": key,
        "namespace": namespace,
        "embedded": false,
        "vector_pending": true,
        "vector_missing_reason": why,
        "stored_without_vector": true,
        "bytes": text.len(),
        "md_file": md_path,
        "recall_mode": "keyword_only",
        "pending_embedding_rows": queued_total,
        "pending_ledger": EMBED_PENDING_LEDGER_FILE,
        "backfill_verb": "memorize-backfill",
        "disclosure": format!(
            "STORED WITHOUT A VECTOR. The memo text is durable in the md corpus at {}, and recall's degraded keyword path scans that corpus directly, so this memo is still retrievable by term overlap. It will NOT appear in vector/similarity recall until it is backfilled. Embedder failure: {why}. Dispatch memorize-backfill once the embedder is healthy, or simply fire another memorize-fire -- a successful one drains up to {} queued rows automatically.",
            md_path.clone().unwrap_or_else(|| "the md corpus".to_string()),
            EMBED_PENDING_DRAIN_PER_SUCCESSFUL_FIRE
        ),
        "agents_drain": agents_drain_obligation(),
    });
    if tencentdb {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("tencentdb_push_pending".to_string(), serde_json::json!(true));
            obj.insert("tencentdb_note".to_string(), serde_json::json!(
                "this namespace is routed to the tencentdb backend, which requires a vector on insert; the row is held in the local md corpus and pending ledger and is pushed to tencentdb by memorize-backfill once the embedding succeeds"
            ));
        }
    }
    (payload.to_string(), String::new(), 0)
}

#[cfg(target_arch = "wasm32")]
pub fn handle_backfill(content: &str) -> (String, String, i32) {
    let body: serde_json::Value = serde_json::from_str(content).unwrap_or(serde_json::Value::Null);
    let max_rows = body
        .get("max_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(EMBED_PENDING_LEDGER_MAX_ROWS as u64) as usize;
    let result = drain_pending_embeddings(max_rows);
    let embedded = result.get("embedded").and_then(|v| v.as_u64()).unwrap_or(0);
    let still_pending = result.get("still_pending").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut payload = result;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("ok".to_string(), serde_json::json!(true));
        obj.insert("pending_ledger".to_string(), serde_json::json!(EMBED_PENDING_LEDGER_FILE));
        obj.insert("summary".to_string(), serde_json::json!(format!(
            "backfilled {embedded} row(s) into the vector store; {still_pending} still have no vector"
        )));
    }
    (payload.to_string(), String::new(), 0)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle_backfill(_content: &str) -> (String, String, i32) {
    ("{\"ok\":false,\"error\":\"memorize-backfill requires wasm32\"}".to_string(), String::new(), 1)
}

#[cfg(target_arch = "wasm32")]
pub fn handle_fire(content: &str) -> (String, String, i32) {
    if content.trim().is_empty() {
        return (String::new(), "empty memorize body".to_string(), 1);
    }
    let parsed: Option<serde_json::Value> = serde_json::from_str(content).ok();
    let (text, namespace) = match parsed {
        Some(v) => {
            let t = v.get("text").and_then(|x| x.as_str()).map(String::from)
                .unwrap_or_else(|| content.trim().to_string());
            let ns = v.get("namespace").and_then(|x| x.as_str()).unwrap_or("default").to_string();
            (t, ns)
        }
        None => (content.trim().to_string(), "default".to_string()),
    };
    if text.is_empty() {
        return (String::new(), "empty memorize text".to_string(), 1);
    }
    if namespace == "default" {
        for tok in text.split_whitespace() {
            if let Some(rest) = tok.strip_prefix('@') {
                let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                if !name.is_empty() {
                    const SIGIL_IGNORED_EVENT_REARM_COOLDOWN_MS: i64 = 5 * 60 * 1000;
                    static SIGIL_IGNORED_EVENT_LAST_FIRED_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
                    let now = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
                    let prev_fired_ms = SIGIL_IGNORED_EVENT_LAST_FIRED_MS.load(std::sync::atomic::Ordering::Relaxed);
                    let cooldown_elapsed = now.saturating_sub(prev_fired_ms) >= SIGIL_IGNORED_EVENT_REARM_COOLDOWN_MS;
                    if cooldown_elapsed
                        && SIGIL_IGNORED_EVENT_LAST_FIRED_MS.compare_exchange(prev_fired_ms, now, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed).is_ok()
                    {
                        crate::wasm_dispatch::emit_event("discipline_sigil_ignored", serde_json::json!({
                            "sigil": format!("@{}", name),
                            "fallback_namespace": "default",
                        }));
                    }
                    break;
                }
            }
        }
    }
    if let Some(reason) = is_derivable_state(&text) {
        let prefix: String = text.chars().take(60).collect();
        crate::wasm_dispatch::emit_event("memorize_reject", serde_json::json!({
            "reason": reason,
            "text_prefix": prefix,
            "namespace": namespace,
        }));
        return (String::new(), format!("rejected: {} -- memo not stored", reason), 1);
    }
    if let Some(dup_key) = crate::memory_md::find_body_hash_duplicate(&namespace, &text) {
        crate::wasm_dispatch::emit_event("memorize_reject", serde_json::json!({
            "reason": "byte-identical body already stored under a different key",
            "duplicate_of": dup_key,
            "namespace": namespace,
        }));
        return (String::new(), format!("rejected: byte-identical to existing memo {} -- memo not stored", dup_key), 1);
    }
    if crate::tencentdb_memory::namespace_is_routed(&namespace) {
        let kind = parsed_kind_or_default(content);
        let emb = match crate::embed::embed_text_json(&text) {
            Some(v) => v,
            None => {
                let why = crate::embed::last_embed_failure().unwrap_or_else(|| "no reason was recorded by the embedder".to_string());
                let now = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
                let content_hash = crate::hash::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
                let key = format!("mem-{:016x}-{}", content_hash, text.len());
                return store_without_vector(&namespace, &key, &kind, &text, &why, true, now);
            }
        };
        let now = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
        return match crate::tencentdb_memory::write(&namespace, &kind, &text, &emb, now) {
            Ok(v) => {
                let mut payload = v;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("ok".to_string(), serde_json::json!(true));
                    obj.insert("agents_drain".to_string(), agents_drain_obligation());
                }
                (payload.to_string(), String::new(), 0)
            }
            Err(e) => (String::new(), format!("memorize-fire: tencentdb_backend write failed: {}", e), 1),
        };
    }
    let now = unsafe { crate::wasm_dispatch::host_now_ms() };
    let content_hash = crate::hash::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
    let key = format!("mem-{:016x}-{}", content_hash, text.len());
    let flat_dedup = crate::wasm_dispatch::host_kv_read(&namespace, &key)
        .map(|existing| existing == text)
        .unwrap_or(false);
    if flat_dedup || crate::memory_md::memory_text_matches(&namespace, &key, &text) {
        let md_path = match crate::memory_md::write_memory(&namespace, &key, &text, now as i64) {
            crate::memory_md::WriteOutcome::Created(p)
            | crate::memory_md::WriteOutcome::Updated(p)
            | crate::memory_md::WriteOutcome::Deduped(p) => Some(p),
            _ => None,
        };
        crate::wasm_dispatch::emit_event("memorize_deduped", serde_json::json!({
            "key": key,
            "namespace": namespace,
        }));
        let payload = serde_json::json!({
            "ok": true,
            "key": key,
            "namespace": namespace,
            "embedded": true,
            "deduped": true,
            "bytes": text.len(),
            "md_file": md_path,
            "agents_drain": agents_drain_obligation(),
        });
        return (payload.to_string(), String::new(), 0);
    }
    let emb_str = match crate::embed::embed_text_json(&text) {
        Some(v) => v.to_string(),
        None => {
            let why = crate::embed::last_embed_failure().unwrap_or_else(|| "no reason was recorded by the embedder".to_string());
            let msg = format!("memorize: embed_text failed for key={}; storing the row WITHOUT a vector rather than dropping the memo -- {}", key, why);
            let _ = unsafe { crate::wasm_dispatch::host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("memorize_embed_failed", serde_json::json!({
                "key": key,
                "namespace": namespace,
                "error": why,
                "degraded_to": "stored_without_vector",
            }));
            let kind = parsed_kind_or_default(content);
            return store_without_vector(&namespace, &key, &kind, &text, &why, false, now as i64);
        }
    };
    let md_path = match crate::memory_md::write_memory(&namespace, &key, &text, now as i64) {
        crate::memory_md::WriteOutcome::Created(p)
        | crate::memory_md::WriteOutcome::Updated(p)
        | crate::memory_md::WriteOutcome::Deduped(p) => Some(p),
        crate::memory_md::WriteOutcome::Invalid(reason) => {
            crate::wasm_dispatch::emit_event("memory_md_write_invalid", serde_json::json!({
                "key": key, "namespace": namespace, "reason": reason,
            }));
            return (String::new(), format!("memorize: md write invalid: {}", reason), 1);
        }
        crate::memory_md::WriteOutcome::Failed(p) => {
            return (String::new(), format!("memorize: md write failed at {}; the md corpus is the durable store, refusing an unbacked memory", p), 1);
        }
    };
    let emb_val: serde_json::Value = serde_json::from_str(&emb_str).unwrap_or(serde_json::Value::Null);
    if let Err(e) = crate::rssearch_vectors::write(&namespace, &key, &text, &emb_val, now as i64) {
        crate::wasm_dispatch::emit_event("rssearch_vectors_write_failed", serde_json::json!({
            "key": key,
            "namespace": namespace,
            "error": e,
        }));
    }
    // This fire just proved the embedder is healthy, which is the cheapest
    // possible recovery trigger for rows an earlier outage stored without a
    // vector -- no poll, no timer, no separate dispatch needed.
    let backfilled = drain_pending_embeddings(EMBED_PENDING_DRAIN_PER_SUCCESSFUL_FIRE);
    let mut payload = serde_json::json!({
        "ok": true,
        "key": key,
        "namespace": namespace,
        "embedded": true,
        "bytes": text.len(),
        "md_file": md_path,
        "agents_drain": agents_drain_obligation(),
    });
    if backfilled.get("attempted").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("embed_backfill".to_string(), backfilled);
        }
    }
    (payload.to_string(), String::new(), 0)
}

const AGENTS_DRAIN_STATE_FILE: &str = ".gm/exec-spool/.agents-drain-state.json";
const FLAT_STREAK_WARN_THRESHOLD: u32 = 3;

#[cfg(target_arch = "wasm32")]
fn agents_drain_obligation() -> serde_json::Value {
    let text = match crate::wasm_dispatch::host_read("AGENTS.md") {
        Some(t) => t,
        None => return serde_json::Value::Null,
    };
    let bytes = text.len();
    let lines = text.lines().count();

    let prior = crate::wasm_dispatch::host_read(AGENTS_DRAIN_STATE_FILE)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let prior_bytes = prior.as_ref().and_then(|v| v.get("agents_bytes")).and_then(|v| v.as_u64());
    let prior_streak = prior.as_ref().and_then(|v| v.get("flat_streak")).and_then(|v| v.as_u64()).unwrap_or(0);

    let dropped = prior_bytes.map(|p| (bytes as u64) < p).unwrap_or(false);
    let flat_streak: u64 = if dropped { 0 } else { prior_streak + 1 };

    let new_state = serde_json::json!({ "agents_bytes": bytes as u64, "flat_streak": flat_streak });
    let _ = crate::wasm_dispatch::host_write(AGENTS_DRAIN_STATE_FILE, &new_state.to_string());

    let back_pressure_warning = if flat_streak >= FLAT_STREAK_WARN_THRESHOLD as u64 {
        Some(format!(
            "AGENTS.md byte count has not dropped across the last {} memorize-fire calls this session (currently {} bytes) -- the drain obligation has been skipped repeatedly, not merely absent this one turn. If genuinely nothing is eligible to drain, that is fine; if something detail-heavy/single-crate/single-platform is sitting in AGENTS.md, drain it THIS turn.",
            flat_streak, bytes
        ))
    } else {
        None
    };

    serde_json::json!({
        "agents_bytes": bytes,
        "agents_lines": lines,
        "dropped_since_last_fire": dropped,
        "flat_streak": flat_streak,
        "back_pressure_warning": back_pressure_warning,
        "instruction": "AGENTS.md is a staging ground; every memorize run drains it. THIS turn, pick a few existing AGENTS.md entries that are detail-heavy, single-crate, or single-platform (the material that belongs in rs-learn), memorize-fire their substance to the default namespace, then compress each drained paragraph to a one-line pointer in the SAME commit. Witness: this store gained the fact (recallable next turn) AND the AGENTS.md byte count dropped. A few entries per run, never a wholesale rewrite; top-level cross-cutting rules stay. Skipping the drain is the slow-bloat drift this back-pressure exists to prevent.",
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle_fire(content: &str) -> (String, String, i32) {
    if content.trim().is_empty() {
        return (String::new(), "empty memorize body".to_string(), 1);
    }
    match fire(content) {
        Ok(path) => {
            let payload = serde_json::json!({ "ok": true, "spooled": path });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("memorize fire failed: {}", e), 1),
    }
}
