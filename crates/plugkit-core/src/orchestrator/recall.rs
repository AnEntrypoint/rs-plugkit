fn derive_query(prompt: &str) -> String {
    let stop: &[&str] = &[
        "the", "a", "an", "to", "of", "in", "on", "for", "and", "or",
        "is", "are", "was", "were", "be", "been", "being", "do", "does",
        "did", "have", "has", "had", "i", "you", "we", "they", "it",
        "this", "that", "these", "those", "with", "from", "as", "at",
        "by", "but", "if", "then", "so", "can", "could", "would",
        "should", "will", "shall", "may", "might", "please", "me",
        "my", "our", "your", "their", "his", "her",
    ];
    let mut words: Vec<&str> = prompt
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .filter(|w| {
            let lower = w.to_lowercase();
            !stop.contains(&lower.as_str())
        })
        .collect();
    words.truncate(6);
    if words.len() < 2 {
        return prompt.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
    }
    words.join(" ")
}

#[cfg(target_arch = "wasm32")]
fn rlog(msg: &str) {
    #[link(wasm_import_module = "env")]
    extern "C" { fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32; }
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}

#[cfg(target_arch = "wasm32")]
fn emit_recall(query: &str, hits: &serde_json::Value, mode: &str, namespace: &str) {
    let arr = hits.as_array();
    let n_hits = arr.map(|a| a.len()).unwrap_or(0);
    let top_score = arr.and_then(|a| a.first()).and_then(|r| r.get("score")).and_then(|d| d.as_f64());
    let hit_keys: Vec<serde_json::Value> = arr
        .map(|a| {
            a.iter()
                .filter_map(|h| h.get("key").and_then(|k| k.as_str()))
                .map(|k| serde_json::Value::String(k.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut fields = serde_json::Map::new();
    fields.insert("sub".to_string(), serde_json::Value::String("memory".to_string()));
    fields.insert("query".to_string(), serde_json::Value::String(query.chars().take(200).collect::<String>()));
    fields.insert("hit".to_string(), serde_json::Value::Bool(n_hits > 0));
    fields.insert("mode".to_string(), serde_json::Value::String(mode.to_string()));
    fields.insert("n_hits".to_string(), serde_json::Value::Number(serde_json::Number::from(n_hits as u64)));
    fields.insert("namespace".to_string(), serde_json::Value::String(namespace.to_string()));
    fields.insert("hit_keys".to_string(), serde_json::Value::Array(hit_keys));
    if let Some(s) = top_score { if let Some(num) = serde_json::Number::from_f64(s) { fields.insert("top_score".to_string(), serde_json::Value::Number(num)); } }
    crate::wasm_dispatch::emit_event("recall", serde_json::Value::Object(fields));
}

/// The flat-kv keyword fallback answers `{key, value}` rows while every vector
/// path answers `{key, namespace, text}`. A caller reading `text` therefore saw
/// nothing at all from the degraded path -- the hits were present and looked
/// empty. Mapping `value` onto `text` here makes the degraded result consumable
/// by the same readers, and `vector_pending` states why it arrived by the
/// keyword route rather than by similarity.
#[cfg(target_arch = "wasm32")]
fn normalize_kv_hits(hits: serde_json::Value, namespace: &str) -> serde_json::Value {
    let arr = match hits.as_array() {
        Some(a) => a,
        None => return hits,
    };
    let mapped: Vec<serde_json::Value> = arr
        .iter()
        .map(|h| {
            let mut obj = h.as_object().cloned().unwrap_or_default();
            if !obj.contains_key("text") {
                if let Some(v) = obj.get("value").cloned() {
                    obj.insert("text".to_string(), v);
                }
            }
            obj.entry("namespace".to_string())
                .or_insert_with(|| serde_json::Value::String(namespace.to_string()));
            obj.insert("vector_pending".to_string(), serde_json::Value::Bool(true));
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(mapped)
}

pub fn recall_hits(query_text: &str, limit: u32) -> serde_json::Value {
    recall_hits_reporting_embed_failure(query_text, limit).0
}

pub fn recall_hits_reporting_embed_failure(query_text: &str, limit: u32) -> (serde_json::Value, bool) {
    if query_text.trim().is_empty() {
        return (serde_json::Value::Array(Vec::new()), false);
    }
    let query = derive_query(query_text);
    let embed_input = if query_text.len() <= 512 { query_text } else { &query };
    let namespace = "default";
    #[cfg(target_arch = "wasm32")]
    {
        use crate::wasm_dispatch::host_kv_query;
        rlog(&format!("recall::recall_hits start query_len={} embed_len={} limit={}", query.len(), embed_input.len(), limit));
        let embedding_opt = crate::embed::embed_text_json_query(embed_input);
        let embed_failed = embedding_opt.is_none();
        let embedding = embedding_opt.unwrap_or(serde_json::Value::Null);
        rlog(&format!("recall::recall_hits embed_done embedded={}", !embedding.is_null()));
        if embed_failed {
            crate::wasm_dispatch::emit_event("embed_fail", serde_json::json!({
                "step": "recall_hits_embed_query",
                "query_len": query.len(),
            }));
        }
        if let Some(md_hits) = crate::wasm_dispatch::memory_recall_backend(&embedding, namespace, limit) {
            rlog("recall::recall_hits done via md-index");
            emit_recall(&query, &md_hits, "vector_top_k", namespace);
            return (md_hits, embed_failed);
        }
        let vec_hits = crate::wasm_dispatch::vec_search_local(&embedding, namespace, limit);
        rlog("recall::recall_hits vec_search returned");
        if !vec_hits.is_null()
            && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false)
        {
            rlog("recall::recall_hits done via vec_search");
            emit_recall(&query, &vec_hits, "vector_top_k", namespace);
            return (vec_hits, embed_failed);
        }
        // Order is load-bearing when the embedder is down. host_kv is
        // libsql-backed and the shared plugin pool's wait does not deny, it
        // waits, so querying kv against an uninstantiated libsql slot blocks for
        // minutes and still answers nothing. The md corpus needs no plugin at
        // all, so on the degraded path it is tried FIRST; kv stays ahead of it
        // whenever the embedder was healthy and only the vector search missed.
        if embed_failed {
            let md_hits = crate::memory_md::keyword_scan(namespace, &query, limit as usize);
            if md_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                rlog("recall::recall_hits done via md-corpus keyword scan (embedder down)");
                emit_recall(&query, &md_hits, "md_corpus_keyword_scan", namespace);
                return (md_hits, embed_failed);
            }
        }
        let packed = unsafe {
            host_kv_query(namespace.as_ptr(), namespace.len() as u32,
                          query.as_ptr(), query.len() as u32)
        };
        rlog("recall::recall_hits kv_query returned");
        let kv_hits = crate::wasm_dispatch::unpack_to_value_pub(packed);
        let result = if kv_hits.is_null() { serde_json::Value::Array(Vec::new()) } else { normalize_kv_hits(kv_hits, namespace) };
        if result.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            // host_kv is libsql-backed, so a project whose libsql pool slot is
            // empty answers the kv query with nothing at all -- the same
            // outage that takes the embedder down can take this rung with it.
            // The md corpus is plain files and is what write_memory treats as
            // durable, so it is the rung that still answers.
            let md_hits = crate::memory_md::keyword_scan(namespace, &query, limit as usize);
            if md_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                rlog("recall::recall_hits done via md-corpus keyword scan");
                emit_recall(&query, &md_hits, "md_corpus_keyword_scan", namespace);
                return (md_hits, embed_failed);
            }
        }
        let mode = if embed_failed { "kv_query_degraded" } else { "kv_query" };
        emit_recall(&query, &result, mode, namespace);
        (result, embed_failed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (&query, embed_input, namespace, limit);
        (serde_json::Value::Array(Vec::new()), false)
    }
}

pub fn handle_auto_recall(content: &str) -> (String, String, i32) {
    let prompt = content.trim();
    if prompt.is_empty() {
        return (String::new(), "auto-recall requires user prompt as body".to_string(), 1);
    }
    let (results, embed_failed) = recall_hits_reporting_embed_failure(prompt, 3);
    let query = derive_query(prompt);
    #[cfg(target_arch = "wasm32")]
    emit_recall(&query, &results, "auto_recall", "default");
    let payload = serde_json::json!({
        "query": query,
        "limit": 3,
        "results": results,
        "embed_failed": embed_failed,
    });
    let empty = results.as_array().map(|a| a.is_empty()).unwrap_or(true);
    // A degraded read that still returned rows is a partial answer, not a
    // failure: reporting exit 1 over real hits told the caller to discard
    // matches it was actually holding. Only an empty degraded result carries
    // the "not exhaustive" warning it was written for.
    if embed_failed && empty {
        (payload.to_string(), "embedder unavailable; recall results are empty, not exhaustive -- do not treat as a genuine no-hits result".to_string(), 1)
    } else if embed_failed {
        (payload.to_string(), "embedder unavailable; these hits came from the degraded keyword path and are ranked by term overlap, not similarity -- treat them as a partial answer".to_string(), 0)
    } else {
        (payload.to_string(), String::new(), 0)
    }
}
