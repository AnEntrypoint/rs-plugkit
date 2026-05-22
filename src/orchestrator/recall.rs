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
    extern "C" { fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32; }
    let _ = unsafe { host_log(2, msg.as_ptr(), msg.len() as u32) };
}

pub fn recall_hits(query_text: &str, limit: u32) -> serde_json::Value {
    if query_text.trim().is_empty() {
        return serde_json::Value::Array(Vec::new());
    }
    let query = derive_query(query_text);
    let embed_input = if query_text.len() <= 512 { query_text } else { &query };
    let namespace = "default";
    #[cfg(target_arch = "wasm32")]
    {
        use crate::wasm_dispatch::{host_vec_search, host_kv_query};
        rlog(&format!("recall::recall_hits start query_len={} embed_len={} limit={}", query.len(), embed_input.len(), limit));
        let embedding = crate::embed::embed_text_json_query(embed_input).unwrap_or(serde_json::Value::Null);
        rlog(&format!("recall::recall_hits embed_done embedded={}", !embedding.is_null()));
        let q_json = serde_json::json!({
            "query": query, "embedding": embedding, "namespace": namespace
        }).to_string();
        let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, limit) };
        rlog("recall::recall_hits vec_search returned");
        let vec_hits = crate::wasm_dispatch::unpack_to_value_pub(packed);
        if !vec_hits.is_null()
            && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false)
        {
            rlog("recall::recall_hits done via vec_search");
            return vec_hits;
        }
        let packed = unsafe {
            host_kv_query(namespace.as_ptr(), namespace.len() as u32,
                          query.as_ptr(), query.len() as u32)
        };
        rlog("recall::recall_hits kv_query returned");
        let kv_hits = crate::wasm_dispatch::unpack_to_value_pub(packed);
        if kv_hits.is_null() { serde_json::Value::Array(Vec::new()) } else { kv_hits }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (query, namespace, limit);
        serde_json::Value::Array(Vec::new())
    }
}

pub fn handle_auto_recall(content: &str) -> (String, String, i32) {
    let prompt = content.trim();
    if prompt.is_empty() {
        return (String::new(), "auto-recall requires user prompt as body".to_string(), 1);
    }
    let query = derive_query(prompt);
    let embed_input = if prompt.len() <= 512 { prompt } else { query.as_str() };
    let limit: u32 = 3;
    let namespace = "default";
    #[cfg(target_arch = "wasm32")]
    let results = {
        use crate::wasm_dispatch::{host_vec_search, host_kv_query};
        let embedding = crate::embed::embed_text_json_query(embed_input).unwrap_or(serde_json::Value::Null);
        let q_json = serde_json::json!({
            "query": query, "embedding": embedding, "namespace": namespace
        }).to_string();
        let packed = unsafe { host_vec_search(q_json.as_ptr(), q_json.len() as u32, limit) };
        let vec_hits = crate::wasm_dispatch::unpack_to_value_pub(packed);
        if !vec_hits.is_null()
            && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false)
        {
            vec_hits
        } else {
            let packed = unsafe {
                host_kv_query(namespace.as_ptr(), namespace.len() as u32,
                              query.as_ptr(), query.len() as u32)
            };
            crate::wasm_dispatch::unpack_to_value_pub(packed)
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    let results = serde_json::Value::Array(Vec::new());
    let payload = serde_json::json!({
        "query": query,
        "limit": limit,
        "results": results,
    });
    (payload.to_string(), String::new(), 0)
}
