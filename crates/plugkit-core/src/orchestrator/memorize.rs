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
                let msg = format!("memorize-fire: embed_text failed for tencentdb-routed namespace={}; refusing silent-NULL-embedding insert", namespace);
                return (String::new(), msg, 1);
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
        "md_file": md_path,
        "agents_drain": agents_drain_obligation(),
    });
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
