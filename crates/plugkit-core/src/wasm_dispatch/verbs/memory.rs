use super::*;

pub const EMBED_UNAVAILABLE: &str =
    "query embedding unavailable -- the bert embedder failed, so no vector search was attempted";

pub fn query_embedding_unusable(query_embedding: &Value) -> bool {
    match query_embedding {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        _ => true,
    }
}

pub(super) fn rssearch_vector_hits(
    query_embedding: &Value,
    namespace: &str,
    limit: u32,
    do_sync: bool,
) -> (Value, Option<Vec<String>>) {
    if query_embedding_unusable(query_embedding) {
        emit_event(
            "rssearch_vector_hits_skipped",
            json!({
                "namespace": namespace,
                "reason": EMBED_UNAVAILABLE,
            }),
        );
        return (json!({ "error": EMBED_UNAVAILABLE }), None);
    }
    let namespaces = discipline_fanout_namespaces_unioned_from_enabled_txt_and_config(namespace);
    let now_ms = unsafe { host_now_ms() } as i64;
    let cfg = crate::ragconfig::RagConfig::resolved();
    let mut memory_namespaces: Vec<String> = Vec::new();
    for ns in &namespaces {
        if cfg.namespaces.is_code(ns) {
            if let Err(e) =
                crate::rssearch_vectors::migrate_namespace_from_flat_json_cfg(ns, now_ms, &cfg)
            {
                emit_event(
                    "rssearch_vectors_migration_failed",
                    json!({ "namespace": ns, "error": e }),
                );
            }
        } else {
            if do_sync {
                let _ = crate::memory_md::export_flat_json(ns, now_ms);
            }
            memory_namespaces.push(ns.clone());
        }
    }
    let sync_started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let converged = if memory_namespaces.is_empty() {
        false
    } else if do_sync {
        let sync = crate::memory_md::sync_index(&memory_namespaces, now_ms);
        sync.get("converged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    } else if crate::memory_md::has_converged_digest(&memory_namespaces) {
        true
    } else {
        let bounded_resume_sync = crate::memory_md::sync_index(&memory_namespaces, now_ms);
        emit_event(
            "recall_read_path_sync",
            json!({
                "namespaces": memory_namespaces,
                "result": bounded_resume_sync,
            }),
        );
        bounded_resume_sync
            .get("converged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let search_started_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    emit_event(
        "recall_vector_phase_timing",
        json!({
            "sync_ms": search_started_ms - sync_started_ms,
            "did_sync": do_sync,
            "namespaces": namespaces.len(),
        }),
    );
    let hits = match crate::rssearch_vectors::search_with_recency_cfg(
        query_embedding,
        &namespaces,
        limit as usize,
        now_ms,
        &cfg,
    ) {
        Ok(hits) => {
            emit_event(
                "recall_vector_phase_timing",
                json!({
                    "search_ms": unsafe { crate::wasm_dispatch::host_now_ms() } - search_started_ms,
                    "outcome": "ok",
                }),
            );
            hits
        }
        Err(e) => {
            emit_event(
                "rssearch_vector_hits_failed",
                json!({
                    "namespace": namespace, "error": e,
                    "search_ms": unsafe { crate::wasm_dispatch::host_now_ms() } - search_started_ms,
                    "reason": "search_with_recency failed even after malformed-db recovery attempt",
                }),
            );
            json!({ "error": e })
        }
    };
    (
        hits,
        if converged {
            Some(memory_namespaces)
        } else {
            None
        },
    )
}

pub(super) const READ_PATH_MUST_NOT_TRIGGER_CORPUS_SYNC: bool = false;

pub fn memory_recall_backend(
    query_embedding: &Value,
    namespace: &str,
    limit: u32,
) -> Option<Value> {
    if query_embedding_unusable(query_embedding) {
        return None;
    }
    let (_, mem_ns) = rssearch_vector_hits(
        query_embedding,
        namespace,
        limit,
        READ_PATH_MUST_NOT_TRIGGER_CORPUS_SYNC,
    );
    let mem_ns = mem_ns?;
    let now_ms = unsafe { host_now_ms() } as i64;
    let cfg = crate::ragconfig::RagConfig::resolved();
    crate::rssearch_vectors::search_memory_hits_cfg(
        query_embedding,
        &mem_ns,
        limit as usize,
        now_ms,
        &cfg,
    )
    .ok()
    .filter(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false))
}

pub(super) const RECALL_QUERY_SHAPE: &str = "query required -- pass {\"query\":\"<the concept to recall>\"}: a plain STRING of prose, not an object, array or path; recall embeds that text and ranks stored memories by cosine times recency. Optional {\"limit\":8} row cap, {\"namespace\":\"default\"} for another namespace. There is no query-less listing mode, so a body without query is always a caller mistake";

pub(super) fn recall_reply(
    body: &Value,
    mode: &str,
    namespace: &str,
    derived_query: &str,
    hits: &Value,
    vector_hits: &Value,
) -> Value {
    let full = crate::recall_compact::wants_full(body);
    let mut reply = json!({
        "mode": mode,
        "namespace": namespace,
        "derived_query": derived_query,
        "hits": crate::recall_compact::compact_hits(hits, full),
    });
    if body
        .get("verbose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        reply["vector_hits"] = vector_hits.clone();
    }
    if !full {
        reply["expand"] = json!("recall {key:\"<hit key>\"} for one full memory; recall {query, full:true} for full text of all hits");
    }
    reply
}

pub(super) fn with_index_progress(mut reply: Value, converged: bool) -> Value {
    reply["index_converged"] = json!(converged);
    if !converged {
        reply["index_note"] = json!("the memory index has not converged: this answer ranks only the vectors indexed so far. Each recall advances indexing by one bounded slice, so repeat the recall until index_converged is true");
    }
    reply
}

pub(super) fn recall_by_key(namespace: &str, key: &str) -> u64 {
    let valid = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return err("recall", "key must be 1-128 chars of [A-Za-z0-9_-], the mem-<hash>-<n> key a compact recall hit reports");
    }
    let Some(path) = crate::memory_md::md_path(namespace, key) else {
        return err(
            "recall",
            &format!("namespace '{namespace}' has no memories directory"),
        );
    };
    match host_read(&path) {
        Some(content) => ok(
            "recall",
            match crate::memory_md::parse(&content) {
                Some(doc) => {
                    json!({ "key": key, "namespace": namespace, "created": doc.created, "updated": doc.updated, "text": doc.text })
                }
                None => json!({ "key": key, "namespace": namespace, "text": content.trim() }),
            },
        ),
        None => err(
            "recall",
            &format!("no memory file for key '{key}' in namespace '{namespace}'"),
        ),
    }
}

pub(super) fn recall(body: &Value) -> u64 {
    let cfg = crate::ragconfig::RagConfig::resolved();
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(cfg.budget.default_limit as u64) as u32;
    let namespace = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or(&cfg.namespaces.default);
    let routed = crate::tencentdb_memory::namespace_is_routed(namespace);
    match body.get("key") {
        None | Some(Value::Null) => {}
        Some(Value::String(_)) if routed => return err("recall", &format!("namespace '{namespace}' is routed to the TencentDB backend, which has no per-key memory file -- use recall {{query, full:true}} instead")),
        Some(Value::String(key)) => return recall_by_key(namespace, key),
        Some(other) => return err("recall", &format!("key must be a memory key string such as \"mem-<hash>-<n>\", got {other}")),
    }
    let Some(raw_query) = body.get("query") else {
        return err_retry_same_verb("recall", RECALL_QUERY_SHAPE);
    };
    let Some(query) = raw_query.as_str() else {
        let shown: String = raw_query.to_string().chars().take(80).collect();
        return err_retry_same_verb(
            "recall",
            &format!(
                "{}; got non-string JSON under \"query\": {}",
                RECALL_QUERY_SHAPE, shown
            ),
        );
    };
    if query.is_empty() {
        return err_retry_same_verb("recall", RECALL_QUERY_SHAPE);
    }
    if routed {
        let embedding = embed_query(query);
        return match crate::tencentdb_memory::recall(&embedding, namespace, limit as usize) {
            Ok(mut v) => {
                if let Some(hits) = v.get("hits").filter(|h| h.is_array()).cloned() {
                    v["hits"] = crate::recall_compact::compact_hits(
                        &hits,
                        crate::recall_compact::wants_full(body),
                    );
                }
                ok("recall", v)
            }
            Err(e) => err("recall", &e),
        };
    }
    let (_dataflow_doc, dataflow_tier, dataflow_path) = crate::dataflow::document_detailed();
    if dataflow_tier != crate::dataflow::DataflowTier::CompiledDefault {
        if let Some(pipeline) = crate::dataflow::pipeline_for("recall") {
            emit_event(
                "dataflow_pipeline_override_used",
                json!({
                    "entry_point": "recall", "tier": dataflow_tier.as_str(), "path": dataflow_path,
                }),
            );
            let mut request = body.clone();
            if let Some(obj) = request.as_object_mut() {
                obj.insert("namespaces".to_string(), json!([namespace]));
                obj.insert("limit".to_string(), json!(limit));
            }
            let out = crate::dataflow_exec::run(&pipeline, request);
            return ok("recall", out);
        }
    }
    check_sigil_ignored(query, namespace);
    let derived_query = query.to_string();
    let embedding = embed_query(query);
    let (vector_hits, mem_ns) = rssearch_vector_hits(
        &embedding,
        namespace,
        limit,
        READ_PATH_MUST_NOT_TRIGGER_CORPUS_SYNC,
    );
    if let Some(mem_ns) = &mem_ns {
        let now_ms = unsafe { host_now_ms() } as i64;
        if let Ok(md_hits) = crate::rssearch_vectors::search_memory_hits_cfg(
            &embedding,
            mem_ns,
            limit as usize,
            now_ms,
            &cfg,
        ) {
            if md_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                return ok(
                    "recall",
                    with_index_progress(
                        recall_reply(
                            body,
                            "vector_top_k",
                            namespace,
                            &derived_query,
                            &md_hits,
                            &vector_hits,
                        ),
                        true,
                    ),
                );
            }
        }
    }
    let vec_hits = vec_search_local(&embedding, namespace, limit);
    if !vec_hits.is_null() && vec_hits.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        let annotated = annotate_hits_with_score(vec_hits);
        return ok(
            "recall",
            with_index_progress(
                recall_reply(
                    body,
                    "vector_top_k",
                    namespace,
                    &derived_query,
                    &annotated,
                    &vector_hits,
                ),
                mem_ns.is_some(),
            ),
        );
    }
    let packed = unsafe {
        host_kv_query(
            namespace.as_ptr(),
            namespace.len() as u32,
            query.as_ptr(),
            query.len() as u32,
        )
    };
    let kv_hits = unpack_to_value(packed);
    let annotated = annotate_hits_with_score(kv_hits);

    let embed_failed = query_embedding_unusable(&embedding);
    let vec_err = vector_hits
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    let degraded = embed_failed || vec_err.is_some();
    let empty = annotated.as_array().map(|a| a.is_empty()).unwrap_or(true);

    if degraded {
        emit_event(
            "recall_degraded",
            json!({
                "namespace": namespace,
                "embed_failed": embed_failed,
                "vec_err": vec_err,
                "kv_hits": annotated.as_array().map(|a| a.len()).unwrap_or(0),
            }),
        );
    }

    if degraded && empty {
        return err(
            "recall",
            &format!(
                "semantic retrieval unavailable (embedder failed{}) and the keyword fallback matched nothing -- this is NOT an empty-knowledgebase result",
                vec_err.map(|e| format!(": {}", e)).unwrap_or_default()
            ),
        );
    }

    let mut reply = recall_reply(
        body,
        "fallback_like",
        namespace,
        &derived_query,
        &annotated,
        &vector_hits,
    );
    reply["degraded"] = json!(degraded);
    ok("recall", with_index_progress(reply, mem_ns.is_some()))
}

pub(super) const DIAGNOSTIC_EVENT_COOLDOWN_MS: i64 = 5 * 60 * 1000;
pub(super) static RECALL_SCORE_UNAVAILABLE_LAST_FIRED_MS: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
pub(super) static SIGIL_IGNORED_LAST_FIRED_MS: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);

pub(super) fn diagnostic_event_should_fire(last_fired: &std::sync::atomic::AtomicI64) -> bool {
    let now = unsafe { host_now_ms() } as i64;
    let prev = last_fired.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(prev) < DIAGNOSTIC_EVENT_COOLDOWN_MS {
        return false;
    }
    last_fired
        .compare_exchange(
            prev,
            now,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_ok()
}

pub(super) fn annotate_hits_with_score(v: Value) -> Value {
    let arr = match v {
        Value::Array(a) => a,
        other => return other,
    };
    let mut out = Vec::with_capacity(arr.len());
    let mut any_missing = false;
    for hit in arr {
        match hit {
            Value::Object(mut map) => {
                if !map.contains_key("score") {
                    map.insert("score".to_string(), Value::Null);
                    any_missing = true;
                }
                out.push(Value::Object(map));
            }
            other => {
                any_missing = true;
                out.push(json!({ "value": other, "score": Value::Null }));
            }
        }
    }
    if any_missing && diagnostic_event_should_fire(&RECALL_SCORE_UNAVAILABLE_LAST_FIRED_MS) {
        emit_event(
            "recall_score_unavailable",
            json!({
                "reason": "host_vec_search return shape elides per-hit score",
            }),
        );
    }
    Value::Array(out)
}

pub(super) fn check_sigil_ignored(text: &str, namespace: &str) {
    if namespace != "default" {
        return;
    }
    let sigil = extract_sigil(text);
    if let Some(s) = sigil {
        if diagnostic_event_should_fire(&SIGIL_IGNORED_LAST_FIRED_MS) {
            emit_event(
                "discipline_sigil_ignored",
                json!({
                    "sigil": s,
                    "fallback_namespace": "default",
                }),
            );
        }
    }
}

pub(super) fn extract_sigil(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let first_tok = trimmed.split_whitespace().next()?;
    let rest = first_tok.strip_prefix('@')?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(format!("@{}", name))
}

pub(super) fn memorize_with_raw(body: &Value, raw: &str) -> u64 {
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| body.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.trim().to_string());
    let namespace = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if text.is_empty() {
        return err("memorize", "text required");
    }
    if let Some(violation) = confinement_violation(body, namespace) {
        return err("memorize", &violation);
    }
    if crate::tencentdb_memory::namespace_is_routed(namespace) {
        let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("l0");
        let emb = match embed_passage(text.as_str()) {
            Some(e) => e,
            None => return err("memorize", "embed failed; refusing to write a text-only memory with no vector (un-vector-recallable orphan)"),
        };
        let now_ms = unsafe { host_now_ms() } as i64;
        return match crate::tencentdb_memory::write(namespace, kind, text.as_str(), &emb, now_ms) {
            Ok(v) => ok("memorize", v),
            Err(e) => err("memorize", &e),
        };
    }
    let text = text.as_str();
    check_sigil_ignored(text, namespace);
    let content_hash = crate::hash::fnv1a64(format!("{}|{}", namespace, text).as_bytes());
    let key = format!("mem-{:016x}-{}", content_hash, text.len());
    let flat_dedup = super::host_abi::host_kv_read(namespace, &key)
        .map(|existing| existing == text)
        .unwrap_or(false);
    if flat_dedup || crate::memory_md::memory_text_matches(namespace, &key, text) {
        let md_path = memory_md_write_path(namespace, &key, text);
        return ok(
            "memorize",
            json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": true, "deduped": true, "md_file": md_path}),
        );
    }
    let emb = match embed_passage(text) {
        Some(e) => e,
        None => return err("memorize", "embed failed; refusing to write a text-only memory with no vector (un-vector-recallable orphan)"),
    };
    let md_path = memory_md_write_path(namespace, &key, text);
    if md_path.is_none() {
        return err("memorize", "memory md write failed; the md corpus is the durable store, refusing an unbacked memory");
    }
    let now_ms = unsafe { host_now_ms() } as i64;
    if let Err(e) = crate::rssearch_vectors::write(namespace, &key, text, &emb, now_ms) {
        emit_event(
            "rssearch_vectors_write_failed",
            json!({
                "key": key,
                "namespace": namespace,
                "error": e,
            }),
        );
    }
    ok(
        "memorize",
        json!({"namespace": namespace, "key": key, "bytes": text.len(), "embedded": true, "md_file": md_path}),
    )
}

pub(super) fn memory_md_write_path(namespace: &str, key: &str, text: &str) -> Option<String> {
    let now_ms = unsafe { host_now_ms() } as i64;
    match crate::memory_md::write_memory(namespace, key, text, now_ms) {
        crate::memory_md::WriteOutcome::Created(p)
        | crate::memory_md::WriteOutcome::Updated(p)
        | crate::memory_md::WriteOutcome::Deduped(p) => Some(p),
        crate::memory_md::WriteOutcome::Invalid(reason) => {
            emit_event(
                "memory_md_write_invalid",
                json!({
                    "key": key, "namespace": namespace, "reason": reason,
                }),
            );
            None
        }
        crate::memory_md::WriteOutcome::Failed(_) => None,
    }
}

pub(super) fn memorize_vacuum(body: &Value) -> u64 {
    let namespace = body.get("namespace").and_then(|v| v.as_str());
    match crate::rssearch_vectors::vacuum_tombstones(namespace) {
        Ok(n) => ok(
            "memorize-vacuum",
            json!({
                "namespace": namespace,
                "rows_reclaimed": n,
                "note": if n == 0 { "no soft-deleted rows to reclaim" } else { "tombstoned rows hard-deleted; the vector index no longer carries them" },
            }),
        ),
        Err(e) => err("memorize-vacuum", &e),
    }
}

pub(super) fn memorize_retention(body: &Value) -> u64 {
    let namespace = body.get("namespace").and_then(|v| v.as_str());
    let cfg = crate::ragconfig::RagConfig::resolved();
    let census = match crate::rssearch_vectors::tombstone_census(namespace) {
        Ok(c) => c,
        Err(e) => return err("memorize-retention", &e),
    };
    let due = crate::rssearch_vectors::retention_reclaim_due(&census, &cfg);
    let apply = body.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);

    let reclaimed = if apply && due {
        match crate::rssearch_vectors::vacuum_tombstones(namespace) {
            Ok(n) => Some(n),
            Err(e) => return err("memorize-retention", &e),
        }
    } else {
        None
    };

    ok(
        "memorize-retention",
        json!({
            "namespace": namespace,
            "live": census.live,
            "tombstoned": census.tombstoned,
            "tombstone_ratio": census.tombstone_ratio(),
            "would_reclaim": census.tombstoned,
            "reclaim_due": due,
            "auto_vacuum_enabled": cfg.retention.auto_vacuum_enabled,
            "thresholds": {
                "tombstone_ratio_threshold": cfg.retention.tombstone_ratio_threshold,
                "tombstone_count_threshold": cfg.retention.tombstone_count_threshold,
            },
            "applied": reclaimed.is_some(),
            "rows_reclaimed": reclaimed,
            "note": "Report-only unless `apply:true` AND a threshold is crossed. Reclaims space from rows ALREADY tombstoned by an agent's own prune -- it never tombstones a live row, so no memory is deleted that was not already judged unwanted.",
        }),
    )
}

pub(super) fn tencentdb_compat_probe(body: &Value) -> u64 {
    let path = match body.get("vectors_db_path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p,
        _ => return err("tencentdb-compat-probe", "vectors_db_path required"),
    };
    match crate::tencentdb_compat::probe(path) {
        Ok(counts) => {
            let embedding_fingerprint =
                crate::tencentdb_compat::read_embedding_provider_fingerprint(path).ok();
            let mut resp =
                json!({"path": path, "counts": counts, "embedding_meta": embedding_fingerprint});
            if let Some(data_dir) = body
                .get("data_dir")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                let scene_block_count =
                    crate::tencentdb_compat::read_l2_scene_block_files(data_dir, u64::MAX)
                        .ok()
                        .and_then(|v| v.as_array().map(|a| a.len()))
                        .unwrap_or(0);
                let persona_file = crate::tencentdb_compat::read_l3_persona_file(data_dir);
                resp["file_counts"] = json!({
                    "l2_scenes": scene_block_count,
                    "l3_persona_exists": persona_file.get("exists").cloned().unwrap_or(Value::Bool(false)),
                });
            }
            let skills_limit = body
                .get("skills_limit")
                .and_then(Value::as_u64)
                .unwrap_or(1000);
            match crate::tencentdb_compat::read_skills_summary(path, skills_limit) {
                Ok(summary) => resp["skills"] = summary,
                Err(e) => resp["skills_error"] = json!(e),
            }
            if let Some(skill_id) = body
                .get("skill_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                match crate::tencentdb_compat::read_skill_version_history(path, skill_id) {
                    Ok(versions) => resp["skill_versions"] = versions,
                    Err(e) => resp["skill_versions_error"] = json!(e),
                }
            }
            ok("tencentdb-compat-probe", resp)
        }
        Err(e) => err("tencentdb-compat-probe", &e),
    }
}

pub(super) fn archive_batch_best_effort_count_moved(pairs: &[(String, String)]) -> usize {
    if pairs.is_empty() {
        return 0;
    }
    let list: Vec<Value> = pairs
        .iter()
        .map(|(s, a)| json!({ "s": s, "a": a }))
        .collect();
    let payload = match serde_json::to_string(&Value::Array(list)) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let code = format!(
        "const fs=require('fs');const path=require('path');const pairs={};let n=0;for(const x of pairs){{try{{fs.mkdirSync(path.dirname(x.a),{{recursive:true}});fs.renameSync(x.s,x.a);n++;}}catch(e){{}}}}process.stdout.write('archived:'+n);",
        payload
    );
    let opts = "{\"timeoutMs\":30000}";
    let packed = unsafe {
        host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let out = crate::wasm_dispatch::unpack_to_string_pub(packed).unwrap_or_default();
    let parsed: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    parsed
        .get("stdout")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix("archived:"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0)
}

pub(super) fn namespace_prefixed_archive_path(source_namespace: &str, filename: &str) -> String {
    format!(
        ".gm/memories-archive-tencentdb/{}/{}",
        source_namespace, filename
    )
}

pub(super) const GM_NATIVE_EMBEDDER_FIXED_DIM: usize = 384;

pub(super) fn reject_if_dest_namespace_dim_not_384(dest_namespace: &str, resolved_dim: usize) -> Option<u64> {
    if resolved_dim == GM_NATIVE_EMBEDDER_FIXED_DIM {
        return None;
    }
    Some(err(
        "tencentdb-memory-import",
        &format!(
            "dest_namespace '{}' resolves to memory.tencentdb_backend.vectors_db_dims={}, but gm's own embedder produces 384-dim vectors -- importing would write vectors the configured table's fixed-width column cannot hold, and they would never be reachable through recall's per-project (not per-namespace) dim config. Configure vectors_db_dims=384 for a namespace dedicated to gm-native memory imports (separate from any namespace holding externally-embedded 768-dim content), then retry.",
            dest_namespace, resolved_dim
        ),
    ))
}

pub(super) fn native_memory_md_corpus_entries(
    source_namespace: &str,
) -> Option<Vec<(String, String, String)>> {
    let dir = crate::memory_md::md_dir(source_namespace)?;
    let listing = crate::pkfs::readdir(&dir);
    let filenames: Vec<String> = listing
        .as_ref()
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for name in &filenames {
        if !name.ends_with(".md") {
            continue;
        }
        let path = format!("{}/{}", dir, name);
        let content = match crate::pkfs::read_to_string(&path) {
            Some(c) => c,
            None => continue,
        };
        if let Some(doc) = crate::memory_md::parse(&content) {
            entries.push((doc.key, doc.text, path));
        }
    }
    Some(entries)
}

pub(super) fn tencentdb_memory_import_gm_native_memories_reembedded_384dim(body: &Value) -> u64 {
    let source_namespace = match body.get("source_namespace").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return err("tencentdb-memory-import", "source_namespace required"),
    };
    let dest_namespace = body
        .get("dest_namespace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(source_namespace);
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("l1");

    if !crate::tencentdb_memory::namespace_is_routed(dest_namespace) {
        return err(
            "tencentdb-memory-import",
            &format!(
                "dest_namespace '{}' is not listed in memory.tencentdb_backend.namespaces (or the backend is disabled) -- refusing to import into an unrouted namespace",
                dest_namespace
            ),
        );
    }

    let entries = match native_memory_md_corpus_entries(source_namespace) {
        Some(e) => e,
        None => {
            return err(
                "tencentdb-memory-import",
                &format!("invalid source_namespace '{}'", source_namespace),
            )
        }
    };
    if entries.is_empty() {
        return ok(
            "tencentdb-memory-import",
            json!({"source_namespace": source_namespace, "dest_namespace": dest_namespace, "imported": 0, "failed": 0, "reason": "no-source-docs"}),
        );
    }

    let cfg = crate::tencentdb_memory::resolved_config();
    if let Some(mismatch) = reject_if_dest_namespace_dim_not_384(dest_namespace, cfg.dim) {
        return mismatch;
    }

    let archive_source = body
        .get("archive_source")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
    let mut imported = 0u32;
    let mut failed: Vec<String> = Vec::new();
    let mut to_archive: Vec<(String, String)> = Vec::new();
    for (key, text, source_path) in &entries {
        let embedding = match crate::embed::embed_text_json_passage(text) {
            Some(v) => v,
            None => {
                failed.push(key.clone());
                continue;
            }
        };
        match crate::tencentdb_memory::write_cfg(
            dest_namespace,
            kind,
            text,
            &embedding,
            now_ms,
            &cfg,
        ) {
            Ok(_) => {
                imported += 1;
                if archive_source {
                    let filename = source_path.rsplit('/').next().unwrap_or(key).to_string();
                    to_archive.push((
                        source_path.clone(),
                        namespace_prefixed_archive_path(source_namespace, &filename),
                    ));
                }
            }
            Err(e) => {
                emit_event(
                    "tencentdb_memory_import_row_error",
                    json!({"key": key, "namespace": dest_namespace, "error": e}),
                );
                failed.push(key.clone());
            }
        }
    }
    let archived = if archive_source {
        archive_batch_best_effort_count_moved(&to_archive)
    } else {
        0
    };
    ok(
        "tencentdb-memory-import",
        json!({
            "source_namespace": source_namespace,
            "dest_namespace": dest_namespace,
            "kind": kind,
            "imported": imported,
            "archived": archived,
            "failed": failed.len(),
            "failed_keys": failed,
        }),
    )
}

pub(super) fn mark_deleted_index_first_before_file_removal_to_close_resurrection_window(
    namespace: &str,
    key: &str,
) -> bool {
    match crate::rssearch_vectors::mark_deleted_reporting_match(namespace, key) {
        Ok(v) => v,
        Err(e) => {
            emit_event(
                "memory.prune_index_error",
                json!({"key": key, "namespace": namespace, "error": e}),
            );
            false
        }
    }
}

pub(super) fn memorize_prune(body: &Value) -> u64 {
    let namespace = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let mut keys: Vec<String> = Vec::new();
    if let Some(k) = body.get("key").and_then(|v| v.as_str()) {
        if !k.is_empty() {
            keys.push(k.to_string());
        }
    }
    if let Some(arr) = body.get("keys").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                keys.push(s.to_string());
            }
        }
    }
    if !keys.is_empty() && crate::tencentdb_memory::namespace_is_routed(namespace) {
        let mut deleted = Vec::new();
        let mut not_found = Vec::new();
        for key in &keys {
            match crate::tencentdb_memory::delete_index_first_then_file(namespace, key) {
                Ok(true) => deleted.push(key.clone()),
                Ok(false) => not_found.push(key.clone()),
                Err(e) => {
                    emit_event(
                        "tencentdb_memory_prune_error",
                        json!({"key": key, "namespace": namespace, "error": e}),
                    );
                    not_found.push(key.clone());
                }
            }
        }
        let mut resp = json!({"namespace": namespace, "deleted": deleted, "mode": "explicit-key"});
        if !not_found.is_empty() {
            resp["not_found"] = json!(not_found);
        }
        return ok("memorize-prune", resp);
    }
    if !keys.is_empty() {
        let vec_ns = format!("{}-vec", namespace);
        let mut deleted = Vec::new();
        let mut not_found = Vec::new();
        for key in &keys {
            let idx_marked =
                mark_deleted_index_first_before_file_removal_to_close_resurrection_window(
                    namespace, key,
                );
            let flat_rc = unsafe {
                host_kv_delete(
                    namespace.as_ptr(),
                    namespace.len() as u32,
                    key.as_ptr(),
                    key.len() as u32,
                )
            };
            let _ = unsafe {
                host_kv_delete(
                    vec_ns.as_ptr(),
                    vec_ns.len() as u32,
                    key.as_ptr(),
                    key.len() as u32,
                )
            };
            let md_deleted = crate::memory_md::delete_memory(namespace, key);
            let legacy_deleted = crate::memory_md::delete_legacy_flat(namespace, key);
            if flat_rc != 0 || md_deleted || idx_marked || legacy_deleted {
                deleted.push(key.clone());
                emit_event(
                    "memory.pruned",
                    json!({"key": key, "namespace": namespace, "mode": "explicit-key", "md_deleted": md_deleted, "index_marked": idx_marked, "legacy_deleted": legacy_deleted}),
                );
            } else {
                not_found.push(key.clone());
                emit_event(
                    "memory.prune-miss",
                    json!({"key": key, "namespace": namespace, "mode": "explicit-key"}),
                );
            }
        }
        let mut resp = json!({"namespace": namespace, "deleted": deleted, "mode": "explicit-key"});
        if !not_found.is_empty() {
            resp["not_found"] = json!(not_found);
            resp["note"] = json!("Keys in not_found did not exist in this namespace -- nothing was pruned for them. The key is likely under a different namespace (pass {namespace:<the recall hit's namespace>}) or the key string did not match exactly. Re-run memorize-prune {query} to get live candidates with their exact keys + namespaces.");
        }
        return ok("memorize-prune", resp);
    }
    let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return err(
            "memorize-prune",
            "provide `key`/`keys` to delete, or `query` to list prune candidates",
        );
    }
    let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as u32;
    let embedding = embed_query(query);
    if query_embedding_unusable(&embedding) {
        emit_event(
            "memorize_prune_degraded",
            json!({
                "namespace": namespace,
                "reason": EMBED_UNAVAILABLE,
            }),
        );
        return err(
            "memorize-prune",
            "semantic retrieval unavailable (the embedder failed) so no prune candidates could be ranked -- this is NOT an empty-namespace result, and deleting on the strength of it would prune memories that were never searched",
        );
    }
    if crate::tencentdb_memory::namespace_is_routed(namespace) {
        return match crate::tencentdb_memory::recall(&embedding, namespace, k as usize) {
            Ok(v) => {
                let mut resp = v;
                if let Some(obj) = resp.as_object_mut() {
                    obj.insert("mode".to_string(), json!("review"));
                }
                ok("memorize-prune", resp)
            }
            Err(e) => err("memorize-prune", &e),
        };
    }
    let (vector_candidates, _) = rssearch_vector_hits(&embedding, namespace, k, true);
    let mut candidates = vec_search_local(&embedding, namespace, k);
    let (unindexed_listed, unindexed_total) = unindexed_on_disk_candidates(namespace);
    let listed_keys: Vec<String> = candidates
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("key").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if let Some(arr) = candidates.as_array_mut() {
        for entry in &unindexed_listed {
            let Some(key) = entry.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            if listed_keys.iter().any(|listed| listed == key) {
                continue;
            }
            arr.push(entry.clone());
        }
    }
    let mut resp = json!({
        "namespace": namespace,
        "mode": "review",
        "candidates": candidates,
        "vector_candidates": vector_candidates,
        "unindexed_on_disk": unindexed_listed,
        "unindexed_on_disk_total": unindexed_total,
        "note": "Review-only: re-dispatch memorize-prune with {keys:[...]} naming the stale ones to delete. Pruning is agent-judged, never auto-similarity-deleted. candidates falls back to the libsql rssearch_vectors result when host_vec_search is unimplemented (both native runtimes stub it). Entries carrying source=on_disk_unindexed exist on disk with no vector row, so they never surface in recall and never appear in vector candidates; they are still real memos and are prunable by explicit key.",
    });
    if unindexed_total > unindexed_listed.len() {
        resp["unindexed_on_disk_truncated"] = json!(unindexed_total - unindexed_listed.len());
    }
    ok("memorize-prune", resp)
}

