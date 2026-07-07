#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};

use crate::wasm_dispatch::{host_read, host_remove, host_stat};

const EXPORT_MARKER: &str = ".flat-export-done";

pub struct MemoryDoc {
    pub key: String,
    pub ns: String,
    pub created: i64,
    pub updated: i64,
    pub text: String,
}

pub enum WriteOutcome {
    Created(String),
    Updated(String),
    Deduped(String),
    Invalid(String),
    Failed(String),
}

pub fn valid_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.contains("..")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub fn md_dir(ns: &str) -> Option<String> {
    if !valid_component(ns) {
        return None;
    }
    if ns == "default" {
        Some(".gm/memories".to_string())
    } else {
        Some(format!(".gm/disciplines/{}/memories", ns))
    }
}

pub fn md_path(ns: &str, key: &str) -> Option<String> {
    if !valid_component(key) {
        return None;
    }
    md_dir(ns).map(|d| format!("{}/{}.md", d, key))
}

fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").trim_end_matches('\n').to_string()
}

pub fn compose(key: &str, ns: &str, created: i64, updated: i64, text: &str) -> String {
    format!(
        "---\nkey: {}\nns: {}\ncreated: {}\nupdated: {}\n---\n\n{}\n",
        key,
        ns,
        created,
        updated,
        normalize_text(text)
    )
}

pub fn parse(content: &str) -> Option<MemoryDoc> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let rest = normalized.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let (front, body) = rest.split_at(end);
    let body = &body["\n---\n".len()..];
    let mut key = String::new();
    let mut ns = String::new();
    let mut created: Option<i64> = None;
    let mut updated: Option<i64> = None;
    for line in front.lines() {
        let mut parts = line.splitn(2, ':');
        let field = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        match field {
            "key" => key = value.to_string(),
            "ns" => ns = value.to_string(),
            "created" => created = value.parse::<i64>().ok(),
            "updated" => updated = value.parse::<i64>().ok(),
            _ => {}
        }
    }
    if key.is_empty() || ns.is_empty() {
        return None;
    }
    let created = created?;
    let updated = updated?;
    let text = normalize_text(body.strip_prefix('\n').unwrap_or(body));
    Some(MemoryDoc { key, ns, created, updated, text })
}

fn atomic_write(path: &str, content: &str) -> bool {
    let path_js = serde_json::to_string(path).unwrap_or_default();
    let content_js = serde_json::to_string(content).unwrap_or_default();
    let code = format!(
        "const fs=require('fs');const path=require('path');const p={};const d={};fs.mkdirSync(path.dirname(p),{{recursive:true}});const t=p+'.tmp-'+process.pid+'-'+Date.now();fs.writeFileSync(t,d);try{{fs.renameSync(t,p);}}catch(e){{try{{fs.unlinkSync(t);}}catch(e2){{}}throw e;}}process.stdout.write('renamed');",
        path_js, content_js
    );
    let opts = "{\"timeoutMs\":15000}";
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(), code.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let exit = parsed.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let renamed = exit == 0 && stdout.contains("renamed");
    if !renamed {
        crate::wasm_dispatch::emit_event("memory_md_write_failed", json!({
            "path": path,
            "exit_code": exit,
            "stderr": parsed.get("stderr").and_then(|v| v.as_str()).unwrap_or(""),
        }));
    }
    renamed
}

pub fn read_doc(ns: &str, key: &str) -> Option<MemoryDoc> {
    let path = md_path(ns, key)?;
    let content = host_read(&path)?;
    parse(&content)
}

pub fn write_memory(ns: &str, key: &str, text: &str, now_ms: i64) -> WriteOutcome {
    let path = match md_path(ns, key) {
        Some(p) => p,
        None => return WriteOutcome::Invalid(format!("invalid ns/key: {}/{}", ns, key)),
    };
    let normalized = normalize_text(text);
    if normalized.is_empty() {
        return WriteOutcome::Invalid("empty memory text".to_string());
    }
    let existing = host_read(&path);
    let mut created = now_ms;
    let mut existed = false;
    if let Some(content) = &existing {
        existed = true;
        if let Some(doc) = parse(content) {
            if doc.text == normalized {
                return WriteOutcome::Deduped(path);
            }
            created = doc.created;
        }
    }
    let content = compose(key, ns, created, now_ms, &normalized);
    if !atomic_write(&path, &content) {
        return WriteOutcome::Failed(path);
    }
    if existed { WriteOutcome::Updated(path) } else { WriteOutcome::Created(path) }
}

pub fn delete_memory(ns: &str, key: &str) -> bool {
    match md_path(ns, key) {
        Some(path) => {
            if host_read(&path).is_none() {
                return false;
            }
            host_remove(&path)
        }
        None => false,
    }
}

pub fn list_docs(ns: &str) -> Vec<MemoryDoc> {
    let dir = match md_dir(ns) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let entries = match crate::pkfs::readdir(&dir) {
        Some(Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for e in entries {
        let name = match e.get("name").and_then(|n| n.as_str()).or_else(|| e.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let name = name.as_str();
        if !name.ends_with(".md") {
            continue;
        }
        let path = format!("{}/{}", dir, name);
        let content = match host_read(&path) {
            Some(c) => c,
            None => continue,
        };
        match parse(&content) {
            Some(doc) => out.push(doc),
            None => {
                crate::wasm_dispatch::emit_event("memory_md_parse_failed", json!({
                    "path": path,
                    "namespace": ns,
                }));
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

pub fn md_file_count(ns: &str) -> usize {
    let dir = match md_dir(ns) {
        Some(d) => d,
        None => return 0,
    };
    match crate::pkfs::readdir(&dir) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|e| e.get("name").and_then(|n| n.as_str()).or_else(|| e.as_str()))
            .filter(|n| n.ends_with(".md"))
            .count(),
        _ => 0,
    }
}

pub fn corpus_digest(ns: &str) -> String {
    let dir = match md_dir(ns) {
        Some(d) => d,
        None => return "invalid".to_string(),
    };
    let entries = match crate::pkfs::readdir(&dir) {
        Some(Value::Array(a)) => a,
        _ => return "empty".to_string(),
    };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).or_else(|| e.as_str()))
        .filter(|n| n.ends_with(".md"))
        .map(|n| n.to_string())
        .collect();
    names.sort();
    let mut acc = String::new();
    for name in &names {
        let path = format!("{}/{}", dir, name);
        let content = host_read(&path).unwrap_or_default();
        acc.push_str(name);
        acc.push('\0');
        acc.push_str(&format!("{:016x}", crate::pipeline::fnv1a64(content.as_bytes())));
        acc.push('\0');
    }
    if names.is_empty() {
        return "empty".to_string();
    }
    format!("{:016x}", crate::pipeline::fnv1a64(acc.as_bytes()))
}

fn ensure_meta_table() -> Result<(), String> {
    crate::rssearch_vectors::ensure_schema()?;
    crate::shared_db::shared_exec(
        "CREATE TABLE IF NOT EXISTS memories_md_meta (namespace TEXT PRIMARY KEY, digest TEXT)",
    )
}

fn meta_digest(ns: &str) -> Option<String> {
    let rows = crate::shared_db::shared_query_params(
        "SELECT digest FROM memories_md_meta WHERE namespace=?1",
        &[ns],
    ).ok()?;
    rows.as_array()?.first()?.get("digest")?.as_str().map(String::from)
}

fn store_meta_digest(ns: &str, digest: &str) {
    let _ = crate::shared_db::shared_exec_params(
        "INSERT INTO memories_md_meta(namespace, digest) VALUES(?1,?2) ON CONFLICT(namespace) DO UPDATE SET digest=excluded.digest",
        &[ns, digest],
    );
}

pub fn sync_index(namespaces: &[String], now_ms: i64) -> Value {
    if ensure_meta_table().is_err() {
        return json!({ "error": "memories_md_meta ensure failed" });
    }
    let mut report = Vec::new();
    for ns in namespaces {
        if ns == "codeinsight" {
            continue;
        }
        let digest = corpus_digest(ns);
        if meta_digest(ns).as_deref() == Some(digest.as_str()) {
            continue;
        }
        let docs = list_docs(ns);
        let rows = crate::shared_db::shared_query_params(
            "SELECT key, text, updated_at, deleted FROM rssearch_vectors WHERE namespace=?1",
            &[ns],
        ).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
        let mut existing: std::collections::HashMap<String, (String, i64, i64)> = std::collections::HashMap::new();
        for row in &rows {
            let k = row.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if k.is_empty() {
                continue;
            }
            existing.insert(
                k.to_string(),
                (
                    row.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    row.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0),
                    row.get("deleted").and_then(|v| v.as_i64()).unwrap_or(0),
                ),
            );
        }
        let mut upserted = 0u32;
        let mut retimed = 0u32;
        let mut marked_deleted = 0u32;
        let mut failed = 0u32;
        let mut doc_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for doc in &docs {
            doc_keys.insert(doc.key.clone());
            match existing.get(&doc.key) {
                Some((text, updated_at, deleted)) if text == &doc.text && *deleted == 0 => {
                    if *updated_at != doc.updated {
                        let upd = doc.updated.to_string();
                        let _ = crate::shared_db::shared_exec_params(
                            "UPDATE rssearch_vectors SET updated_at=?1 WHERE namespace=?2 AND key=?3",
                            &[&upd, ns, &doc.key],
                        );
                        retimed += 1;
                    }
                }
                _ => {
                    match crate::embed::embed_text_json(&doc.text) {
                        Some(emb) => {
                            match crate::rssearch_vectors::write(ns, &doc.key, &doc.text, &emb, doc.updated) {
                                Ok(()) => upserted += 1,
                                Err(e) => {
                                    failed += 1;
                                    crate::wasm_dispatch::emit_event("memory_md_sync_row_failed", json!({
                                        "namespace": ns, "key": doc.key, "error": e,
                                    }));
                                }
                            }
                        }
                        None => {
                            failed += 1;
                            crate::wasm_dispatch::emit_event("memory_md_sync_embed_failed", json!({
                                "namespace": ns, "key": doc.key,
                            }));
                        }
                    }
                }
            }
        }
        for (key, (_, _, deleted)) in &existing {
            if *deleted == 0 && !doc_keys.contains(key) {
                match crate::rssearch_vectors::mark_deleted(ns, key) {
                    Ok(()) => marked_deleted += 1,
                    Err(_) => failed += 1,
                }
            }
        }
        if failed == 0 {
            store_meta_digest(ns, &digest);
        }
        report.push(json!({
            "namespace": ns,
            "upserted": upserted,
            "retimed": retimed,
            "marked_deleted": marked_deleted,
            "failed": failed,
        }));
    }
    let _ = now_ms;
    Value::Array(report)
}

fn flat_kv_entries(ns: &str) -> Vec<(String, String)> {
    let packed = unsafe {
        crate::wasm_dispatch::host_kv_query(ns.as_ptr(), ns.len() as u32, "".as_ptr(), 0)
    };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for e in arr {
            if let (Some(k), Some(val)) = (
                e.get("key").and_then(|x| x.as_str()),
                e.get("value").and_then(|x| x.as_str()),
            ) {
                out.push((k.to_string(), val.to_string()));
            }
        }
    }
    out
}

fn flat_mtime_ms(ns: &str, key: &str) -> Option<i64> {
    for dir in [format!(".gm/disciplines/{}-vec", ns), format!(".gm/disciplines/{}", ns)] {
        let path = format!("{}/{}.json", dir, key);
        if let Some(st) = host_stat(&path) {
            if let Some(m) = st.get("mtime_ms").and_then(|v| v.as_f64()) {
                return Some(m as i64);
            }
        }
    }
    None
}

pub fn export_flat_json(ns: &str, now_ms: i64) -> Value {
    let dir = match md_dir(ns) {
        Some(d) => d,
        None => return json!({ "exported": 0, "reason": "invalid-namespace" }),
    };
    let marker = format!("{}/{}", dir, EXPORT_MARKER);
    if host_read(&marker).is_some() {
        return json!({ "exported": 0, "reason": "already-exported" });
    }
    let entries = flat_kv_entries(ns);
    let mut exported = 0u32;
    let mut skipped = 0u32;
    for (key, text) in &entries {
        if !key.starts_with("mem-") || !valid_component(key) {
            skipped += 1;
            continue;
        }
        let path = match md_path(ns, key) {
            Some(p) => p,
            None => { skipped += 1; continue; }
        };
        if host_read(&path).is_some() {
            skipped += 1;
            continue;
        }
        let ts = flat_mtime_ms(ns, key).unwrap_or(now_ms);
        let content = compose(key, ns, ts, ts, text);
        if atomic_write(&path, &content) {
            exported += 1;
        } else {
            skipped += 1;
        }
    }
    if atomic_write(&marker, "flat-json memory export complete\n") {
        crate::wasm_dispatch::emit_event("memory_md_exported", json!({
            "namespace": ns,
            "exported": exported,
            "skipped": skipped,
        }));
    }
    json!({ "exported": exported, "skipped": skipped, "namespace": ns })
}
