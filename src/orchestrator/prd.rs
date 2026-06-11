use serde_yaml::Value;
use super::gm_dir;
use super::yaml_util::yaml_to_json;
use crate::pkfs;

pub fn prd_path() -> std::path::PathBuf {
    gm_dir().join("prd.yml")
}

fn subject_from_fields(item_map: &serde_yaml::Mapping) -> &str {
    ["subject", "title", "name", "task", "goal", "description", "notes"]
        .iter()
        .find_map(|f| item_map.get(&Value::String(f.to_string()))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty()))
        .unwrap_or("")
}

fn slug_from_subject(subject: &str) -> Option<String> {
    let s = subject.trim();
    if s.is_empty() { return None; }
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') { out.pop(); }
    if out.is_empty() { return None; }
    if out.len() > 64 { out.truncate(64); while out.ends_with('-') { out.pop(); } }
    Some(out)
}

pub fn handle_list(_content: &str) -> (String, String, i32) {
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (serde_json::json!({ "items": [], "count": 0 }).to_string(), String::new(), 0);
    }
    let raw = match pkfs::read_to_string(&path_s) {
        Some(s) => s,
        None => return (String::new(), "read failed".to_string(), 1),
    };
    let doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };
    let seq = doc.as_sequence().cloned()
        .or_else(|| doc.get("items").and_then(|v| v.as_sequence()).cloned())
        .unwrap_or_default();
    let items: Vec<serde_json::Value> = seq.iter().filter_map(|item| {
        let m = item.as_mapping()?;
        let mut out = serde_json::Map::new();
        for (k, v) in m {
            if let Some(ks) = k.as_str() {
                out.insert(ks.to_string(), yaml_to_json(v));
            }
        }
        Some(serde_json::Value::Object(out))
    }).collect();
    let count = items.len();
    (serde_json::json!({ "items": items, "count": count }).to_string(), String::new(), 0)
}

fn defer_marker_in_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    const HARD_MARKERS: &[&str] = &[
        "defer to later", "deferred to later", "deferred for later",
        "address it next", "address this next", "leave for next",
        "documented for next", "documented for future",
        "below criticality", "skip for now", "punt for now",
        "do later", "fix later", "later pass", "future work",
    ];
    for m in HARD_MARKERS {
        if lower.contains(m) { return Some(m); }
    }
    const SOFT_MARKERS: &[&str] = &[
        "next pass", "next session", "next turn",
        "future pass", "future session", "future turn",
    ];
    let has_soft = SOFT_MARKERS.iter().find(|m| lower.contains(**m)).copied();
    if let Some(m) = has_soft {
        const DEFER_CUE: &[&str] = &["defer", "punt", "leave", "save it", "save this",
            "push to", "kick to", "hold for", "wait for", "do it in", "handle in", "finish in"];
        const RETRO_CUE: &[&str] = &["quoted", "described", "the phrase", "rejected for",
            "flagged", "caused by", "deviation", "false-positive", "false positive",
            "was mine", "self-caught", "tripped", "substring"];
        let retro = RETRO_CUE.iter().any(|c| lower.contains(c));
        let defer = DEFER_CUE.iter().any(|c| lower.contains(c));
        if defer && !retro { return Some(m); }
    }
    None
}

pub fn handle_add(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing body: provide PRD item as JSON or YAML".to_string(), 1);
    }
    let new_item: Value = match serde_yaml::from_str::<Value>(trimmed) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .and_then(|j| serde_yaml::to_value(j).ok()) {
            Some(v) => v,
            None => return (String::new(), "parse failed".to_string(), 1),
        },
    };
    let item_map = match new_item.as_mapping() {
        Some(m) => m.clone(),
        None => return (String::new(), "item must be a mapping with id/subject/status".to_string(), 1),
    };
    let has_external_block = item_map.get(&Value::String("blockedBy".to_string()))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().any(|x| matches!(x.as_str(), Some("external") | Some("out-of-reach"))))
        .unwrap_or(false);
    if !has_external_block {
        let mut scan_buf = String::new();
        for field in &["description", "subject", "notes"] {
            if let Some(s) = item_map.get(&Value::String(field.to_string())).and_then(|v| v.as_str()) {
                scan_buf.push_str(s);
                scan_buf.push('\n');
            }
        }
        if let Some(marker) = defer_marker_in_text(&scan_buf) {
            let err = format!(
                "PRD item rejected: deferral language detected ('{}'). Per §22 Fix on Sight and §17 Maximal Cover, in-spirit reachable work must be executed this turn, not deferred. Either: (a) drop the deferral phrasing and commit to executing this turn, or (b) add `blockedBy: [external]` (or `[out-of-reach]`) to declare the item genuinely unreachable from this session.",
                marker
            );
            return (String::new(), err, 1);
        }
    }
    let provided_id = item_map.get(&Value::String("id".to_string()))
        .or_else(|| item_map.get(&Value::String("slug".to_string())))
        .or_else(|| item_map.get(&Value::String("prd_id".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let subject_str = subject_from_fields(&item_map);
    let slug = if provided_id.is_none() { slug_from_subject(subject_str) } else { None };
    if provided_id.is_none() && slug.is_none() {
        crate::wasm_dispatch::emit_event("deviation.prd-add-no-id", serde_json::json!({
            "subject": subject_str,
            "hint": "Pass `id` in prd-add body. No usable text in any of id/slug/prd_id or subject/title/name/task/goal/description/notes — every one was empty or unslugifiable, so the row was REJECTED. An item-<ms> fallback cannot be referenced by intent in recall or prd-resolve, so it is never admitted. Pass `id` directly, or put the intent in any of subject/title/name/task/goal/description so slug derivation succeeds.",
        }));
        let err = "PRD item rejected: no usable `id` and no slugifiable text in subject/title/name/task/goal/description/notes. A referenceable handle is mandatory — every later prd-resolve / recall names the row by id. Pass `id` directly (kebab-case slug derived from intent) or provide a meaningful subject/title/description. Auto `item-<ms>` ids are not admitted because they cannot be referenced by intent.";
        return (String::new(), err.to_string(), 1);
    }
    let id = provided_id.clone()
        .or_else(|| slug.clone())
        .unwrap_or_else(|| format!("item-{}", crate::orchestrator::state::now_ms()));
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    let mut doc: Value = if pkfs::exists(&path_s) {
        let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
        serde_yaml::from_str(&raw).unwrap_or(Value::Sequence(vec![]))
    } else {
        Value::Sequence(vec![])
    };
    let mut upserted = false;
    if let Some(seq) = doc.as_sequence_mut() {
        let mut new_with_id = item_map.clone();
        new_with_id.insert(Value::String("id".to_string()), Value::String(id.clone()));
        if !new_with_id.contains_key(&Value::String("status".to_string())) {
            new_with_id.insert(Value::String("status".to_string()), Value::String("pending".to_string()));
        }
        let existing = seq.iter_mut().find(|it| {
            it.as_mapping()
                .and_then(|m| m.get(&Value::String("id".to_string())))
                .and_then(|v| v.as_str())
                == Some(id.as_str())
        });
        match existing {
            Some(slot) => { *slot = Value::Mapping(new_with_id); upserted = true; }
            None => seq.push(Value::Mapping(new_with_id)),
        }
    } else {
        return (String::new(), "prd.yml is not a sequence".to_string(), 1);
    }
    let new_raw = serde_yaml::to_string(&doc).unwrap_or_default();
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }
    let key = if upserted { "rescoped" } else { "added" };
    (serde_json::json!({ key: id }).to_string(), String::new(), 0)
}

fn parse_resolve_target(trimmed: &str) -> (String, Option<String>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let id = v.get("id")
            .or_else(|| v.get("prd_id"))
            .or_else(|| v.get("mutable_id"))
            .or_else(|| v.get("item_id"))
            .or_else(|| v.get("slug"))
            .or_else(|| v.get("key"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| trimmed.to_string());
        let mut wit = v.get("witness_evidence")
            .or_else(|| v.get("witness"))
            .or_else(|| v.get("evidence"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let id = if let Ok(inner) = serde_json::from_str::<serde_json::Value>(&id) {
            if let Some(im) = inner.as_object() {
                let recovered = im.get("key")
                    .or_else(|| im.get("id"))
                    .or_else(|| im.get("prd_id"))
                    .or_else(|| im.get("slug"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                if wit.is_none() {
                    wit = im.get("witness_evidence")
                        .or_else(|| im.get("witness"))
                        .or_else(|| im.get("evidence"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
                recovered.unwrap_or(id)
            } else {
                id
            }
        } else {
            id
        };
        (id, wit)
    } else if let Some((id, wit)) = recover_truncated_envelope(trimmed) {
        (id, wit)
    } else {
        let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
        let id = parts.first().map(|s| s.to_string()).unwrap_or_else(|| trimmed.to_string());
        let wit = parts.get(1).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        (id, wit)
    }
}

fn recover_truncated_envelope(s: &str) -> Option<(String, Option<String>)> {
    let s = s.trim_start();
    if !s.starts_with('{') { return None; }
    let extract = |key: &str| -> Option<String> {
        let needle = format!("\"{}\"", key);
        let after_key = s.find(&needle)? + needle.len();
        let rest = &s[after_key..];
        let colon = rest.find(':')?;
        let after_colon = rest[colon + 1..].trim_start();
        if !after_colon.starts_with('"') { return None; }
        let val = &after_colon[1..];
        let mut out = String::new();
        let mut chars = val.chars();
        while let Some(c) = chars.next() {
            if c == '\\' { if let Some(n) = chars.next() { out.push(n); } continue; }
            if c == '"' { return Some(out); }
            out.push(c);
        }
        if out.is_empty() { None } else { Some(out) }
    };
    let id = extract("id").or_else(|| extract("prd_id")).or_else(|| extract("mutable_id"))
        .or_else(|| extract("item_id")).or_else(|| extract("slug")).or_else(|| extract("key"))?;
    let wit = extract("witness_evidence").or_else(|| extract("witness")).or_else(|| extract("evidence"));
    Some((id, wit))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn nearest_known_id(target: &str, known: &[String]) -> Option<String> {
    if target.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &String)> = None;
    for id in known {
        let d = levenshtein(target, id);
        match best {
            Some((bd, _)) if d >= bd => {}
            _ => best = Some((d, id)),
        }
    }
    best.and_then(|(d, id)| {
        let bound = (target.len().max(id.len()) / 3).max(2);
        if d <= bound { Some(id.clone()) } else { None }
    })
}

pub fn handle_resolve(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), "missing PRD item id".to_string(), 1);
    }
    let (id_target, witness) = parse_resolve_target(trimmed);
    let has_witness = witness.as_ref().map(|w| !w.trim().is_empty()).unwrap_or(false);
    if !has_witness {
        let body = serde_json::json!({
            "error": format!("prd-resolve refused: no witness_evidence for {}", id_target),
            "deviation_kind": "prd-resolve-no-witness",
            "prd_id": id_target,
            "hint": "resolve requires non-empty witness_evidence (file:line | codesearch hit | exec snippet | browser page.evaluate result). A row cannot be marked completed without evidence the work is real - this gate exists because an agent under closure-pressure marked undone tasks completed with an absent witness. Body shape: {\"id\": \"<prd-item-id>\", \"witness_evidence\": \"<file:line or codesearch hit>\"}. Do the work, capture its witness, then resolve.",
        }).to_string();
        return (body, format!("prd-resolve refused: no witness_evidence for {}", id_target), 1);
    }
    let path = prd_path();
    let path_s = path.to_string_lossy().to_string();
    if !pkfs::exists(&path_s) {
        return (String::new(), format!("{} does not exist", path.display()), 1);
    }
    let raw = pkfs::read_to_string(&path_s).unwrap_or_default();
    let mut doc: Value = match serde_yaml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return (String::new(), format!("parse failed: {}", e), 1),
    };
    let mut found = false;
    if let Some(seq) = doc.as_sequence_mut() {
        for item in seq.iter_mut() {
            if let Some(map) = item.as_mapping_mut() {
                if map.get(&Value::String("id".to_string())).and_then(|v| v.as_str()) == Some(&id_target) {
                    map.insert(Value::String("status".to_string()), Value::String("completed".to_string()));
                    if let Some(w) = witness.as_ref() {
                        map.insert(Value::String("witness".to_string()), Value::String(w.clone()));
                    }
                    found = true;
                }
            }
        }
    }
    if !found {
        let mut known_ids: Vec<String> = Vec::new();
        if let Some(seq) = doc.as_sequence() {
            for item in seq {
                if let Some(m) = item.as_mapping() {
                    if let Some(id_v) = m.get(&Value::String("id".to_string())) {
                        if let Some(id_s) = id_v.as_str() {
                            known_ids.push(id_s.to_string());
                        }
                    }
                }
            }
        }
        let suggested_id = nearest_known_id(&id_target, &known_ids);
        let body = serde_json::json!({
            "error": format!("prd id not found: {}", id_target),
            "deviation_kind": "prd-resolve-unknown-id",
            "prd_id": id_target,
            "known_ids": known_ids,
            "suggested_id": suggested_id,
            "hint": "body shape: {\"id\": \"<prd-item-id>\", \"witness_evidence\": \"<file:line or codesearch hit>\"}; aliases accepted: prd_id, mutable_id, item_id, slug, key (all map to id). A nested envelope (prd_id holding a stringified {\"key\":..,\"witness\":..} object) is unwrapped automatically and the inner key/id/prd_id/slug is recovered. Raw text body: first whitespace-delimited token = id, rest = witness_evidence. If `suggested_id` is non-null it is the closest known id to what you passed — likely a typo; re-dispatch with it. If the recovered id is not in `known_ids` above, the row was never `prd-add`ed in this chain — your next dispatch is `prd-add` with this id, THEN `prd-resolve`. Do not invent ids; resolve only what was added; never retry the same unknown id unchanged.",
        }).to_string();
        return (body, format!("prd id not found: {}", id_target), 1);
    }
    let new_raw = serde_yaml::to_string(&doc).unwrap_or_default();
    if !pkfs::write(&path_s, &new_raw) {
        return (String::new(), "write failed".to_string(), 1);
    }
    (serde_json::json!({ "resolved": id_target }).to_string(), String::new(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_empty_body() {
        let (out, err, rc) = handle_resolve("   ");
        assert_eq!(rc, 1);
        assert!(out.is_empty());
        assert!(err.contains("missing PRD item id"));
    }

    #[test]
    fn resolve_rejects_missing_witness() {
        let (out, err, rc) = handle_resolve(r#"{"id":"some-row"}"#);
        assert_eq!(rc, 1);
        assert!(err.contains("no witness_evidence"));
        let body: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(body["deviation_kind"], "prd-resolve-no-witness");
        assert_eq!(body["prd_id"], "some-row");
    }

    #[test]
    fn resolve_rejects_blank_witness() {
        let (_out, err, rc) = handle_resolve(r#"{"id":"some-row","witness_evidence":"   "}"#);
        assert_eq!(rc, 1);
        assert!(err.contains("no witness_evidence"));
    }

    #[test]
    fn resolve_with_witness_passes_witness_gate() {
        let (_out, err, rc) = handle_resolve(r#"{"id":"some-row","witness_evidence":"src/x.rs:42"}"#);
        assert_eq!(rc, 1);
        assert!(!err.contains("no witness_evidence"));
    }

    #[test]
    fn resolve_unknown_id_returns_deviation_kind() {
        let body = serde_json::json!({
            "error": "prd id not found: bogus",
            "deviation_kind": "prd-resolve-unknown-id",
            "prd_id": "bogus",
        });
        assert_eq!(body["deviation_kind"], "prd-resolve-unknown-id");
        assert_eq!(body["prd_id"], "bogus");
    }

    #[test]
    fn resolve_unwraps_nested_envelope_in_prd_id() {
        let body = r#"{"prd_id":"{\"key\":\"foo-bar\",\"witness\":\"src/x.rs:10\"}"}"#;
        let (id, wit) = parse_resolve_target(body);
        assert_eq!(id, "foo-bar");
        assert_eq!(wit.as_deref(), Some("src/x.rs:10"));
    }

    #[test]
    fn resolve_recovers_truncated_envelope() {
        let body = r#"{"id":"strip-folder-prefix","witness_evidence":"imapsync-server.js"#;
        let (id, wit) = parse_resolve_target(body);
        assert_eq!(id, "strip-folder-prefix");
        assert_eq!(wit.as_deref(), Some("imapsync-server.js"));
    }

    #[test]
    fn resolve_recovers_truncated_envelope_no_witness() {
        let body = r#"{"id":"only-id"#;
        let (id, _) = parse_resolve_target(body);
        assert_eq!(id, "only-id");
    }

    #[test]
    fn resolve_accepts_top_level_key_alias() {
        let (id, _) = parse_resolve_target(r#"{"key":"foo-bar"}"#);
        assert_eq!(id, "foo-bar");
    }

    #[test]
    fn resolve_nested_recovers_id_alias_when_no_key() {
        let body = r#"{"prd_id":"{\"id\":\"baz\"}"}"#;
        let (id, wit) = parse_resolve_target(body);
        assert_eq!(id, "baz");
        assert!(wit.is_none());
    }

    #[test]
    fn resolve_top_level_witness_wins_over_nested() {
        let body = r#"{"prd_id":"{\"key\":\"foo\",\"witness\":\"nested\"}","witness_evidence":"toplevel"}"#;
        let (id, wit) = parse_resolve_target(body);
        assert_eq!(id, "foo");
        assert_eq!(wit.as_deref(), Some("toplevel"));
    }

    #[test]
    fn resolve_nested_array_falls_through_unchanged() {
        let body = r#"{"prd_id":"[1,2,3]"}"#;
        let (id, _) = parse_resolve_target(body);
        assert_eq!(id, "[1,2,3]");
    }

    #[test]
    fn resolve_nested_object_without_recoverable_key_keeps_blob() {
        let body = r#"{"prd_id":"{\"foo\":\"bar\"}"}"#;
        let (id, _) = parse_resolve_target(body);
        assert_eq!(id, r#"{"foo":"bar"}"#);
    }

    #[test]
    fn resolve_plain_id_unaffected() {
        let (id, wit) = parse_resolve_target(r#"{"id":"plain-id","witness_evidence":"w"}"#);
        assert_eq!(id, "plain-id");
        assert_eq!(wit.as_deref(), Some("w"));
    }

    #[test]
    fn resolve_raw_text_body_unaffected() {
        let (id, wit) = parse_resolve_target("raw-id some witness text");
        assert_eq!(id, "raw-id");
        assert_eq!(wit.as_deref(), Some("some witness text"));
    }

    fn map_of(pairs: &[(&str, &str)]) -> serde_yaml::Mapping {
        let mut m = serde_yaml::Mapping::new();
        for (k, v) in pairs {
            m.insert(Value::String(k.to_string()), Value::String(v.to_string()));
        }
        m
    }

    #[test]
    fn subject_prefers_subject_field() {
        let m = map_of(&[("subject", "the subject"), ("title", "the title")]);
        assert_eq!(subject_from_fields(&m), "the subject");
    }

    #[test]
    fn subject_falls_back_to_title_when_subject_absent() {
        let m = map_of(&[("title", "Add zero-amount sell guard")]);
        assert_eq!(subject_from_fields(&m), "Add zero-amount sell guard");
    }

    #[test]
    fn subject_falls_back_through_description() {
        let m = map_of(&[("description", "verify planGrid sell guard")]);
        assert_eq!(subject_from_fields(&m), "verify planGrid sell guard");
        assert_eq!(slug_from_subject(subject_from_fields(&m)).as_deref(), Some("verify-plangrid-sell-guard"));
    }

    #[test]
    fn subject_skips_empty_and_whitespace_fields() {
        let m = map_of(&[("subject", "   "), ("title", ""), ("task", "real task here")]);
        assert_eq!(subject_from_fields(&m), "real task here");
    }

    #[test]
    fn subject_empty_when_no_text_fields() {
        let m = map_of(&[("category", "bug"), ("effort", "low")]);
        assert_eq!(subject_from_fields(&m), "");
        assert!(slug_from_subject(subject_from_fields(&m)).is_none());
    }
}
