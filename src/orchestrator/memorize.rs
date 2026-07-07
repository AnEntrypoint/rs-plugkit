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
fn is_derivable_state(text: &str) -> Option<&'static str> {
    let t = text.trim();
    if t.len() > 40 && t.chars().filter(|c| c.is_ascii_hexdigit()).count() == t.len() {
        return Some("memo is a hex hash; git log is the source of truth");
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
        if lower.contains(pat) { return Some(msg); }
    }
    None
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
                    static FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !FIRED.swap(true, std::sync::atomic::Ordering::Relaxed) {
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
    let now = unsafe { crate::wasm_dispatch::host_now_ms() };
    let content_hash = crate::pipeline::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
    let key = format!("mem-{:016x}-{}", content_hash, text.len());
    if let Some(existing) = crate::wasm_dispatch::host_kv_read(&namespace, &key) {
        if existing == text {
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
    }
    let emb_str = match crate::embed::embed_text_json(&text) {
        Some(v) => v.to_string(),
        None => {
            let msg = format!("memorize: embed_text failed for key={}; refusing silent-NULL-embedding insert", key);
            let _ = unsafe { crate::wasm_dispatch::host_log(2, msg.as_ptr(), msg.len() as u32) };
            crate::wasm_dispatch::emit_event("memorize_embed_failed", serde_json::json!({
                "key": key,
                "namespace": namespace,
                "error": "embed_text returned None",
            }));
            return (String::new(), msg, 1);
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
    let edge_inserted = insert_memory_edge(&namespace, &key, &text, &emb_str, now as i64);
    let emb_val: serde_json::Value = serde_json::from_str(&emb_str).unwrap_or(serde_json::Value::Null);
    if let Err(e) = crate::rssearch_vectors::write(&namespace, &key, &text, &emb_val, now as i64) {
        crate::wasm_dispatch::emit_event("rssearch_vectors_write_failed", serde_json::json!({
            "key": key,
            "namespace": namespace,
            "error": e,
        }));
    }
    let payload = serde_json::json!({
        "ok": true,
        "key": key,
        "namespace": namespace,
        "embedded": true,
        "bytes": text.len(),
        "graph_edge": edge_inserted,
        "md_file": md_path,
        "agents_drain": agents_drain_obligation(),
    });
    (payload.to_string(), String::new(), 0)
}

#[cfg(target_arch = "wasm32")]
fn agents_drain_obligation() -> serde_json::Value {
    let text = match crate::wasm_dispatch::host_read("AGENTS.md") {
        Some(t) => t,
        None => return serde_json::Value::Null,
    };
    let bytes = text.len();
    let lines = text.lines().count();
    serde_json::json!({
        "agents_bytes": bytes,
        "agents_lines": lines,
        "instruction": "AGENTS.md is a staging ground; every memorize run drains it. THIS turn, pick a few existing AGENTS.md entries that are detail-heavy, single-crate, or single-platform (the material that belongs in rs-learn), memorize-fire their substance to the default namespace, then compress each drained paragraph to a one-line pointer in the SAME commit. Witness: this store gained the fact (recallable next turn) AND the AGENTS.md byte count dropped. A few entries per run, never a wholesale rewrite; top-level cross-cutting rules stay. Skipping the drain is the slow-bloat drift this back-pressure exists to prevent.",
    })
}

#[cfg(target_arch = "wasm32")]
fn insert_memory_edge(namespace: &str, key: &str, _text: &str, emb_str: &str, now: i64) -> bool {
    let embedding: serde_json::Value = serde_json::from_str(emb_str).unwrap_or(serde_json::Value::Null);
    if embedding.is_null() {
        return false;
    }
    match crate::rslearn_vectors::write(key, namespace, key, "memorize", "", &embedding, now) {
        Ok(()) => true,
        Err(e) => {
            crate::wasm_dispatch::emit_event("rslearn_vectors_write_failed", serde_json::json!({
                "edge_id": key,
                "error": e,
            }));
            false
        }
    }
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
