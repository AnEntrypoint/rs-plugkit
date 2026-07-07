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

fn tmp_path_for(path: &str) -> String {
    let now = unsafe { crate::wasm_dispatch::host_now_ms() };
    format!("{}.tmp-{}", path, now)
}

fn rename_batch(pairs: &[(String, String)]) -> usize {
    if pairs.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    for chunk in pairs.chunks(RENAME_BATCH_CHUNK) {
        let list: Vec<Value> = chunk.iter().map(|(t, p)| json!({ "t": t, "p": p })).collect();
        let payload = match serde_json::to_string(&Value::Array(list)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let code = format!(
            "const fs=require('fs');const pairs={};let n=0;for(const x of pairs){{try{{fs.renameSync(x.t,x.p);n++;}}catch(e){{try{{fs.unlinkSync(x.t);}}catch(e2){{}}}}}}process.stdout.write('renamed:'+n);",
            payload
        );
        let opts = "{\"timeoutMs\":30000}";
        let packed = unsafe {
            crate::wasm_dispatch::host_exec_js(
                code.as_ptr(), code.len() as u32,
                opts.as_ptr(), opts.len() as u32,
            )
        };
        let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
        let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        total += parsed
            .get("stdout")
            .and_then(|v| v.as_str())
            .and_then(|s| s.strip_prefix("renamed:"))
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
    }
    total
}

fn atomic_write(path: &str, content: &str) -> bool {
    let tmp = tmp_path_for(path);
    if !crate::wasm_dispatch::host_write(&tmp, content) {
        crate::wasm_dispatch::emit_event("memory_md_write_failed", json!({
            "path": path,
            "step": "tmp-write",
        }));
        return false;
    }
    let renamed = rename_batch(&[(tmp, path.to_string())]) == 1;
    if !renamed {
        crate::wasm_dispatch::emit_event("memory_md_write_failed", json!({
            "path": path,
            "step": "rename",
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

pub fn has_stored_digest(namespaces: &[String]) -> bool {
    if ensure_meta_table().is_err() {
        return false;
    }
    for ns in namespaces {
        if ns == "codeinsight" {
            continue;
        }
        if meta_digest(ns).is_none() {
            return false;
        }
    }
    true
}

const SYNC_EMBED_BUDGET_MS: u64 = 1500;
const SYNC_TOTAL_BUDGET_MS: u64 = 2000;
const SYNC_REKEY_ROWS_DEADLINE_MS: u64 = 3500;
const SYNC_SHADOW_ABORT_THRESHOLD: u32 = 5;
const REKEY_BATCH_MAX: usize = 25;

fn scan_corpus(ns: &str) -> (String, Vec<MemoryDoc>) {
    let dir = match md_dir(ns) {
        Some(d) => d,
        None => return ("invalid".to_string(), Vec::new()),
    };
    let entries = match crate::pkfs::readdir(&dir) {
        Some(Value::Array(a)) => a,
        _ => return ("empty".to_string(), Vec::new()),
    };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).or_else(|| e.as_str()))
        .filter(|n| n.ends_with(".md"))
        .map(|n| n.to_string())
        .collect();
    names.sort();
    if names.is_empty() {
        return ("empty".to_string(), Vec::new());
    }
    let mut acc = String::new();
    let mut docs = Vec::new();
    for name in &names {
        let path = format!("{}/{}", dir, name);
        let content = host_read(&path).unwrap_or_default();
        acc.push_str(name);
        acc.push('\0');
        acc.push_str(&format!("{:016x}", crate::pipeline::fnv1a64(content.as_bytes())));
        acc.push('\0');
        match parse(&content) {
            Some(doc) => docs.push(doc),
            None => {
                crate::wasm_dispatch::emit_event("memory_md_parse_failed", json!({
                    "path": path,
                    "namespace": ns,
                }));
            }
        }
    }
    docs.sort_by(|a, b| a.key.cmp(&b.key));
    (format!("{:016x}", crate::pipeline::fnv1a64(acc.as_bytes())), docs)
}

fn is_malformed(err: &str) -> bool {
    err.contains("malformed")
}

fn is_shadow_row(err: &str) -> bool {
    err.contains("shadow row")
}

pub fn content_key(ns: &str, text: &str) -> String {
    let normalized = normalize_text(text);
    let hash = crate::pipeline::fnv1a64(format!("{}|{}", ns, normalized).as_bytes());
    format!("mem-{:016x}-{}", hash, normalized.len())
}

fn extract_embedding(v: &Value) -> Option<Value> {
    if v.is_array() { return Some(v.clone()); }
    if let Some(arr) = v.get("embedding") {
        if arr.is_array() { return Some(arr.clone()); }
    }
    if let Some(emb) = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first()).and_then(|e| e.get("embedding")) {
        if emb.is_array() { return Some(emb.clone()); }
    }
    None
}

fn flat_vec_embedding(ns: &str, key: &str) -> Option<Value> {
    let vec_ns = format!("{}-vec", ns);
    let raw = crate::wasm_dispatch::host_kv_read(&vec_ns, key)?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let emb = extract_embedding(&parsed)?;
    if emb.as_array().map(|a| a.len()).unwrap_or(0) == 384 { Some(emb) } else { None }
}

fn remove_chunk(paths: &[String]) -> usize {
    if paths.is_empty() {
        return 0;
    }
    let payload = match serde_json::to_string(paths) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let code = format!(
        "const fs=require('fs');const paths={};let n=0;for(const p of paths){{try{{fs.unlinkSync(p);n++;}}catch(e){{}}}}process.stdout.write('removed:'+n);",
        payload
    );
    let opts = "{\"timeoutMs\":30000}";
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(), code.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    parsed
        .get("stdout")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix("removed:"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0)
}

fn remove_batch(paths: &[String]) -> usize {
    let mut total = 0usize;
    for chunk in paths.chunks(RENAME_BATCH_CHUNK) {
        total += remove_chunk(chunk);
    }
    total
}

fn recover_malformed_db() -> bool {
    let path = crate::code_index::project_db_path(None);
    crate::wasm_dispatch::emit_event("memory_md_db_recreated", json!({
        "path": path,
        "reason": "database disk image is malformed; derived state dropped for full rebuild",
    }));
    if let Err(e) = crate::shared_db::recreate_shared_db(&path) {
        crate::wasm_dispatch::emit_event("memory_md_db_recreate_failed", json!({ "path": path, "error": e }));
        return false;
    }
    crate::rssearch_vectors::ensure_schema().is_ok() && ensure_meta_table().is_ok()
}

fn scan_manifest(ns: &str) -> Option<(String, Vec<(String, String)>)> {
    let dir = md_dir(ns)?;
    let out_path = format!(".gm/exec-spool/.mem-scan-{}.json", ns);
    let dir_js = serde_json::to_string(&dir).ok()?;
    let out_js = serde_json::to_string(&out_path).ok()?;
    let code = format!(
        "const fs=require('fs');const path=require('path');const crypto=require('crypto');const dir={};let entries=[];try{{entries=fs.readdirSync(dir).filter(n=>n.endsWith('.md')).sort();}}catch(e){{}}const files=[];const h=crypto.createHash('sha256');for(const n of entries){{let c=Buffer.alloc(0);try{{c=fs.readFileSync(path.join(dir,n));}}catch(e){{}}const fh=crypto.createHash('sha256').update(c).digest('hex').slice(0,16);h.update(n+':'+fh+'\\n');files.push([n,fh]);}}const digest=files.length?h.digest('hex').slice(0,16):'empty';fs.writeFileSync({},JSON.stringify({{digest:digest,files:files}}));process.stdout.write('scan:'+digest);",
        dir_js, out_js
    );
    let opts = "{\"timeoutMs\":30000}";
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(), code.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    if !stdout.starts_with("scan:") {
        return None;
    }
    let manifest_raw = host_read(&out_path)?;
    let manifest: Value = serde_json::from_str(&manifest_raw).ok()?;
    let digest = manifest.get("digest").and_then(|d| d.as_str())?.to_string();
    let mut files = Vec::new();
    for f in manifest.get("files").and_then(|f| f.as_array())? {
        let pair = f.as_array()?;
        files.push((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()));
    }
    Some((digest, files))
}

fn ensure_files_table() -> Result<(), String> {
    crate::shared_db::shared_exec(
        "CREATE TABLE IF NOT EXISTS memories_md_files (namespace TEXT NOT NULL, name TEXT NOT NULL, hash TEXT NOT NULL, PRIMARY KEY(namespace, name))",
    )
}

fn stored_file_hashes(ns: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Ok(rows) = crate::shared_db::shared_query_params(
        "SELECT name, hash FROM memories_md_files WHERE namespace=?1",
        &[ns],
    ) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                if let (Some(n), Some(h)) = (
                    row.get("name").and_then(|v| v.as_str()),
                    row.get("hash").and_then(|v| v.as_str()),
                ) {
                    out.insert(n.to_string(), h.to_string());
                }
            }
        }
    }
    out
}

fn store_file_hash(ns: &str, name: &str, hash: &str) {
    let _ = crate::shared_db::shared_exec_params(
        "INSERT INTO memories_md_files(namespace, name, hash) VALUES(?1,?2,?3) ON CONFLICT(namespace, name) DO UPDATE SET hash=excluded.hash",
        &[ns, name, hash],
    );
}

fn drop_file_hash(ns: &str, name: &str) {
    let _ = crate::shared_db::shared_exec_params(
        "DELETE FROM memories_md_files WHERE namespace=?1 AND name=?2",
        &[ns, name],
    );
}

pub fn sync_index(namespaces: &[String], now_ms: i64) -> Value {
    if ensure_meta_table().is_err() || ensure_files_table().is_err() {
        return json!({ "converged": false, "error": "memories md meta/files table ensure failed" });
    }
    let started = unsafe { crate::wasm_dispatch::host_now_ms() };
    let mut recreated = false;
    let mut converged = true;
    let mut report = Vec::new();
    'ns: for ns in namespaces {
        if ns == "codeinsight" {
            continue;
        }
        let (digest, files) = match scan_manifest(ns) {
            Some(v) => v,
            None => {
                let (d, docs) = scan_corpus(ns);
                (d.clone(), docs.iter().map(|doc| (format!("{}.md", doc.key), d.clone())).collect())
            }
        };
        if meta_digest(ns).as_deref() == Some(digest.as_str()) {
            continue;
        }
        converged = false;
        let known = stored_file_hashes(ns);
        let manifest_names: std::collections::HashSet<String> = files.iter().map(|(n, _)| n.clone()).collect();
        let mut removed_keys = 0u32;
        for (name, _) in &known {
            if !manifest_names.contains(name) {
                let key = name.trim_end_matches(".md");
                let _ = crate::rssearch_vectors::mark_deleted(ns, key);
                drop_file_hash(ns, name);
                removed_keys += 1;
            }
        }
        let changed: Vec<&(String, String)> = files
            .iter()
            .filter(|(n, h)| known.get(n).map(|kh| kh != h).unwrap_or(true))
            .collect();
        let mut upserted = 0u32;
        let mut retimed = 0u32;
        let mut resurrected = 0u32;
        let mut failed = 0u32;
        let mut deferred = 0u32;
        let mut rekeyed = 0u32;
        let mut shadow_failed = 0u32;
        let mut embeds = 0u32;
        let mut rekey_new_files: Vec<(String, String)> = Vec::new();
        let mut rekey_pairs: Vec<(String, String)> = Vec::new();
        let mut rekey_rows: Vec<(String, String, Value, i64)> = Vec::new();
        let mut rekey_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut processed_hashes: Vec<(String, String)> = Vec::new();
        let mut flat_by_content: Option<std::collections::HashMap<String, String>> = None;
        let mut flat_embedding_for = |doc_key: &str, cache: &mut Option<std::collections::HashMap<String, String>>| -> Option<Value> {
            if let Some(e) = flat_vec_embedding(ns, doc_key) {
                return Some(e);
            }
            if cache.is_none() {
                let mut map = std::collections::HashMap::new();
                for (old_key, value) in flat_kv_entries(ns) {
                    if old_key.starts_with("mem-") {
                        map.insert(content_key(ns, &value), old_key);
                    }
                }
                *cache = Some(map);
            }
            cache.as_ref()
                .and_then(|m| m.get(doc_key))
                .and_then(|old_key| flat_vec_embedding(ns, old_key))
        };
        let write_row = |key: &str, text: &str, emb: &Value, updated: i64,
                         upserted: &mut u32, failed: &mut u32, shadow_failed: &mut u32| -> i8 {
            match crate::rssearch_vectors::write(ns, key, text, emb, updated) {
                Ok(()) => { *upserted += 1; 0 }
                Err(e) => {
                    if is_malformed(&e) { return 1; }
                    *failed += 1;
                    if is_shadow_row(&e) { *shadow_failed += 1; }
                    crate::wasm_dispatch::emit_event("memory_md_sync_row_failed", json!({
                        "namespace": ns, "key": key, "error": e,
                    }));
                    if *shadow_failed >= SYNC_SHADOW_ABORT_THRESHOLD { 2 } else { 0 }
                }
            }
        };
        for (name, hash) in &changed {
            let total_elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
            if total_elapsed > SYNC_TOTAL_BUDGET_MS {
                deferred += 1;
                continue;
            }
            let dir = match md_dir(ns) { Some(d) => d, None => break };
            let path = format!("{}/{}", dir, name);
            let content = match host_read(&path) {
                Some(c) => c,
                None => { deferred += 1; continue; }
            };
            let doc = match parse(&content) {
                Some(d) => d,
                None => {
                    crate::wasm_dispatch::emit_event("memory_md_parse_failed", json!({
                        "path": path, "namespace": ns,
                    }));
                    processed_hashes.push((name.to_string(), hash.to_string()));
                    continue;
                }
            };
            let expected = content_key(ns, &doc.text);
            if doc.key != expected || *name != format!("{}.md", expected) {
                if rekey_pairs.len() >= REKEY_BATCH_MAX {
                    deferred += 1;
                    continue;
                }
                rekeyed += 1;
                let new_path = match md_path(ns, &expected) {
                    Some(p) => p,
                    None => { failed += 1; continue; }
                };
                rekey_pairs.push((path.clone(), new_path.clone()));
                if rekey_targets.insert(expected.clone()) {
                    if host_read(&new_path).is_none() {
                        rekey_new_files.push((new_path, compose(&expected, ns, doc.created, doc.updated, &doc.text)));
                    }
                    if let Some(emb) = flat_embedding_for(&doc.key, &mut flat_by_content) {
                        rekey_rows.push((expected.clone(), doc.text.clone(), emb, doc.updated));
                    }
                }
                let _ = crate::rssearch_vectors::mark_deleted(ns, &doc.key);
                drop_file_hash(ns, name);
                continue;
            }
            let row = crate::shared_db::shared_query_params(
                "SELECT text, updated_at, deleted FROM rssearch_vectors WHERE namespace=?1 AND key=?2",
                &[ns, &doc.key],
            ).ok().and_then(|r| r.as_array().and_then(|a| a.first().cloned()));
            match row {
                Some(r) => {
                    let text = r.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    let updated_at = r.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
                    let deleted = r.get("deleted").and_then(|v| v.as_i64()).unwrap_or(0);
                    if text == doc.text && deleted == 0 {
                        if updated_at != doc.updated {
                            let upd = doc.updated.to_string();
                            let _ = crate::shared_db::shared_exec_params(
                                "UPDATE rssearch_vectors SET updated_at=?1 WHERE namespace=?2 AND key=?3",
                                &[&upd, ns, &doc.key],
                            );
                            retimed += 1;
                        }
                        processed_hashes.push((name.to_string(), hash.to_string()));
                    } else if text == doc.text {
                        match crate::rssearch_vectors::undelete(ns, &doc.key, doc.updated) {
                            Ok(()) => { resurrected += 1; processed_hashes.push((name.to_string(), hash.to_string())); }
                            Err(_) => failed += 1,
                        }
                    } else {
                        failed += 1;
                        crate::wasm_dispatch::emit_event("memory_md_key_text_mismatch", json!({
                            "namespace": ns, "key": doc.key,
                        }));
                    }
                }
                None => {
                    let emb = match flat_embedding_for(&doc.key, &mut flat_by_content) {
                        Some(e) => Some(e),
                        None => {
                            let elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
                            if elapsed > SYNC_EMBED_BUDGET_MS && embeds > 0 {
                                deferred += 1;
                                continue;
                            }
                            embeds += 1;
                            crate::embed::embed_text_json(&doc.text)
                        }
                    };
                    match emb {
                        Some(emb) => {
                            match write_row(&doc.key, &doc.text, &emb, doc.updated, &mut upserted, &mut failed, &mut shadow_failed) {
                                0 => processed_hashes.push((name.to_string(), hash.to_string())),
                                1 => {
                                    converged = false;
                                    if !recreated {
                                        recreated = true;
                                        if recover_malformed_db() {
                                            report.push(json!({ "namespace": ns, "recreated": true }));
                                            continue 'ns;
                                        }
                                    }
                                    break 'ns;
                                }
                                _ => {
                                    crate::wasm_dispatch::emit_event("memory_md_sync_aborted", json!({
                                        "namespace": ns,
                                        "reason": "repeated vector-index shadow-row failures; pass abandoned without digest store, will retry next sync",
                                        "shadow_failed": shadow_failed,
                                    }));
                                    report.push(json!({ "namespace": ns, "aborted": true, "shadow_failed": shadow_failed }));
                                    continue 'ns;
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
        if !rekey_new_files.is_empty() || !rekey_pairs.is_empty() {
            let wrote = atomic_write_batch(&rekey_new_files);
            let verified_removals: Vec<String> = rekey_pairs
                .iter()
                .filter(|(_, new_path)| host_read(new_path).is_some())
                .map(|(old_path, _)| old_path.clone())
                .collect();
            let removed = remove_batch(&verified_removals);
            crate::wasm_dispatch::emit_event("memory_md_rekeyed_batch", json!({
                "namespace": ns,
                "rekeyed": rekeyed,
                "files_written": wrote,
                "files_removed": removed,
                "removals_skipped_unverified": rekey_pairs.len().saturating_sub(verified_removals.len()),
            }));
            for (key, text, emb, updated) in &rekey_rows {
                let total_elapsed = unsafe { crate::wasm_dispatch::host_now_ms() }.saturating_sub(started);
                if total_elapsed > SYNC_REKEY_ROWS_DEADLINE_MS {
                    break;
                }
                let rc = write_row(key, text, emb, *updated, &mut upserted, &mut failed, &mut shadow_failed);
                if rc != 0 {
                    break;
                }
            }
        }
        for (name, hash) in &processed_hashes {
            store_file_hash(ns, name, hash);
        }
        if failed == 0 && deferred == 0 && rekeyed == 0 {
            let manifest_keys: std::collections::HashSet<String> = manifest_names
                .iter()
                .map(|n| n.trim_end_matches(".md").to_string())
                .collect();
            if let Ok(rows) = crate::shared_db::shared_query_params(
                "SELECT key FROM rssearch_vectors WHERE namespace=?1 AND deleted=0",
                &[ns],
            ) {
                if let Some(arr) = rows.as_array() {
                    for row in arr {
                        if let Some(k) = row.get("key").and_then(|v| v.as_str()) {
                            if !manifest_keys.contains(k) {
                                let _ = crate::rssearch_vectors::mark_deleted(ns, k);
                            }
                        }
                    }
                }
            }
            store_meta_digest(ns, &digest);
        } else if deferred > 0 || rekeyed > 0 {
            crate::wasm_dispatch::emit_event("memory_md_sync_partial", json!({
                "namespace": ns,
                "deferred": deferred,
                "rekeyed": rekeyed,
                "upserted": upserted,
            }));
        }
        report.push(json!({
            "namespace": ns,
            "changed": changed.len(),
            "upserted": upserted,
            "retimed": retimed,
            "resurrected": resurrected,
            "removed_keys": removed_keys,
            "failed": failed,
            "deferred": deferred,
            "rekeyed": rekeyed,
        }));
    }
    let _ = now_ms;
    json!({ "converged": converged, "report": report })
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

const EXPORT_BATCH_MAX: usize = 200;

const RENAME_BATCH_CHUNK: usize = 60;

fn atomic_write_batch(files: &[(String, String)]) -> usize {
    if files.is_empty() {
        return 0;
    }
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(files.len());
    for (path, content) in files {
        let tmp = format!("{}.tmp-{}", path, pairs.len());
        if crate::wasm_dispatch::host_write(&tmp, content) {
            pairs.push((tmp, path.clone()));
        }
    }
    rename_batch(&pairs)
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
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut skipped = 0u32;
    let mut deferred = 0u32;
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
        if pending.len() >= EXPORT_BATCH_MAX {
            deferred += 1;
            continue;
        }
        let ts = flat_mtime_ms(ns, key).unwrap_or(now_ms);
        pending.push((path, compose(key, ns, ts, ts, text)));
    }
    let exported = atomic_write_batch(&pending);
    let complete = deferred == 0 && exported == pending.len();
    if complete && atomic_write(&marker, "flat-json memory export complete\n") {
        crate::wasm_dispatch::emit_event("memory_md_exported", json!({
            "namespace": ns,
            "exported": exported,
            "skipped": skipped,
        }));
    } else if deferred > 0 {
        crate::wasm_dispatch::emit_event("memory_md_export_partial", json!({
            "namespace": ns,
            "exported": exported,
            "deferred": deferred,
        }));
    }
    json!({ "exported": exported, "skipped": skipped, "deferred": deferred, "namespace": ns })
}
