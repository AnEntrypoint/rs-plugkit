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
        ("(fixed)", "past-tense fix markers belong in commit messages"),
        ("changelog:", "changelog entries live in CHANGELOG.md"),
        ("commit hash", "commit hashes are derivable from git log"),
        ("recent commit", "recent commits are derivable from git log"),
        ("git blame says", "git blame is derivable from the repo"),
    ];
    for (pat, msg) in bad {
        if lower.contains(pat) { return Some(msg); }
    }
    None
}

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
    if let Some(reason) = is_derivable_state(&text) {
        return (String::new(), format!("rejected: {} — memo not stored", reason), 1);
    }
    let now = unsafe { crate::wasm_dispatch::host_now_ms() };
    static HANDLE_FIRE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = HANDLE_FIRE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let key = format!("mem-{}-{}-{}", now, counter, text.len());
    let rc = unsafe {
        crate::wasm_dispatch::host_kv_put(
            namespace.as_ptr(), namespace.len() as u32,
            key.as_ptr(), key.len() as u32,
            text.as_ptr(), text.len() as u32,
        )
    };
    if rc == 0 {
        return (String::new(), "kv_put failed".to_string(), 1);
    }
    let emb_packed = unsafe {
        crate::wasm_dispatch::host_vec_embed(text.as_ptr(), text.len() as u32)
    };
    let embedded = if emb_packed != 0 {
        let vec_ns = format!("{}-vec", namespace);
        if let Some(emb_str) = crate::wasm_dispatch::unpack_to_value_pub(emb_packed).as_str().map(String::from)
            .or_else(|| Some(crate::wasm_dispatch::unpack_to_value_pub(emb_packed).to_string()))
        {
            if !emb_str.is_empty() && emb_str != "null" {
                let _ = unsafe {
                    crate::wasm_dispatch::host_kv_put(
                        vec_ns.as_ptr(), vec_ns.len() as u32,
                        key.as_ptr(), key.len() as u32,
                        emb_str.as_ptr(), emb_str.len() as u32,
                    )
                };
                true
            } else { false }
        } else { false }
    } else { false };
    let payload = serde_json::json!({
        "ok": true,
        "key": key,
        "namespace": namespace,
        "embedded": embedded,
        "bytes": text.len(),
    });
    (payload.to_string(), String::new(), 0)
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
