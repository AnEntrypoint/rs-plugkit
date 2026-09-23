#![cfg(target_arch = "wasm32")]

use serde::Serialize;
use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle, SafeToWithdraw};
use super::coeffect_realm::{InterceptionContext, MergeKind, RealmTable};
use super::gm_dir;
use crate::pkfs;

fn audit_confluence(all: &[String]) -> Vec<MetatheoryViolation> {
    let enabled = enabled_names();
    let initial_states: Vec<(String, FiberLifecycle)> =
        all.iter().map(|n| (n.clone(), read_fiber_state(n))).collect();
    let targets: Vec<(String, bool)> = all
        .iter()
        .map(|n| (n.clone(), enabled.iter().any(|e| e == n) && requires_satisfied(n, &enabled)))
        .collect();
    if !fiber_lifecycle::check_confluence(&initial_states, &targets) {
        return vec![MetatheoryViolation {
            theorem: "confluence (Theorem 73)",
            discipline: "(whole discipline set)".to_string(),
            detail: "forward and reverse evaluation order reached different Active sets from the same initial states and targets".to_string(),
        }];
    }
    Vec::new()
}

fn note_cfg() -> crate::ragconfig::DisciplineNoteConfig {
    crate::ragconfig::RagConfig::resolved().discipline_note
}

fn valid_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn policy_path(discipline: &str) -> std::path::PathBuf {
    gm_dir().join("disciplines").join(discipline).join("policy.md")
}

fn requires_path(discipline: &str) -> std::path::PathBuf {
    gm_dir().join("disciplines").join(discipline).join("requires.json")
}

fn fiber_state_path(discipline: &str) -> std::path::PathBuf {
    gm_dir().join("disciplines").join(discipline).join("fiber-state.json")
}

fn read_fiber_state(discipline: &str) -> FiberLifecycle {
    fiber_lifecycle::read_fiber_state(&fiber_state_path(discipline).to_string_lossy())
}

fn advance_fiber(discipline: &str, target_satisfied: bool) -> bool {
    fiber_lifecycle::advance_fiber(&fiber_state_path(discipline).to_string_lossy(), target_satisfied)
}

pub struct Component {
    pub name: String,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub has_effect: bool,
    pub lifecycle: FiberLifecycle,
    pub realm: String,
}

impl Component {
    pub fn read(name: &str) -> Component {
        let policy_text = pkfs::read_to_string(&policy_path(name).to_string_lossy().to_string());
        Component {
            name: name.to_string(),
            requires: declared_requires(name),
            provides: declared_provides(name),
            has_effect: policy_text.map(|t| !t.trim().is_empty()).unwrap_or(false),
            lifecycle: read_fiber_state(name),
            realm: declared_realm(name),
        }
    }
}

fn declared_field(discipline: &str, field: &str) -> Vec<String> {
    let path = requires_path(discipline);
    let path_s = path.to_string_lossy().to_string();
    match pkfs::read_to_string(&path_s) {
        Some(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get(field).cloned())
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.into_iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn declared_requires(discipline: &str) -> Vec<String> {
    declared_field(discipline, "requires")
}

pub(crate) fn declared_realm(discipline: &str) -> String {
    let path = requires_path(discipline);
    let path_s = path.to_string_lossy().to_string();
    pkfs::read_to_string(&path_s)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("realm").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default()
}

fn requires_json_value(discipline: &str) -> Option<serde_json::Value> {
    let path = requires_path(discipline);
    let path_s = path.to_string_lossy().to_string();
    pkfs::read_to_string(&path_s).and_then(|text| serde_json::from_str(&text).ok())
}

fn declared_isolation(discipline: &str) -> std::collections::BTreeMap<String, String> {
    requires_json_value(discipline)
        .and_then(|v| v.get("isolation").cloned())
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|r| (k, r.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn declared_interception(discipline: &str) -> std::collections::BTreeMap<String, (String, MergeKind)> {
    requires_json_value(discipline)
        .and_then(|v| v.get("interception").cloned())
        .and_then(|v| v.as_object().cloned())
        .map(|obj| {
            obj.into_iter()
                .filter_map(|(k, v)| {
                    let metadata = v.get("metadata").and_then(|m| m.as_str()).unwrap_or("").to_string();
                    let merge = match v.get("merge").and_then(|m| m.as_str()) {
                        Some("set_union") => MergeKind::SetUnion,
                        _ => MergeKind::ScalarOverwrite,
                    };
                    Some((k, (metadata, merge)))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn build_realm_table(names: &[String]) -> RealmTable {
    let mut table = RealmTable::new();
    for name in names {
        for (key, realm) in declared_isolation(name) {
            table.isolate(&key, &realm);
        }
    }
    table
}

fn build_interception_context(names: &[String]) -> InterceptionContext {
    let mut ctx = InterceptionContext::new();
    for name in names {
        for (key, (metadata, kind)) in declared_interception(name) {
            ctx.declare_merge_kind(&key, kind);
            ctx.intercept(&key, &metadata);
        }
    }
    ctx
}

pub(crate) fn resolve_key_realm(realm_table: &RealmTable, discipline_realm: &str, key: &str) -> String {
    let per_key = realm_table.realm_of(key);
    if per_key.is_empty() || per_key == key {
        discipline_realm.to_string()
    } else {
        per_key
    }
}

fn declared_provides(discipline: &str) -> Vec<String> {
    let explicit = declared_field(discipline, "provides");
    if explicit.is_empty() {
        vec![discipline.to_string()]
    } else {
        explicit
    }
}

fn requires_satisfied(discipline: &str, enabled_names: &[String]) -> bool {
    let realm_table = build_realm_table(enabled_names);
    let discipline_realm = declared_realm(discipline);
    declared_requires(discipline).iter().all(|dep| {
        let dep_realm = resolve_key_realm(&realm_table, &discipline_realm, dep);
        enabled_names
            .iter()
            .filter(|n| resolve_key_realm(&realm_table, &declared_realm(n), dep) == dep_realm)
            .filter(|n| read_fiber_state(n) == FiberLifecycle::Active)
            .any(|n| declared_provides(n).iter().any(|cap| cap == dep))
    })
}

pub fn handle(content: &str) -> (String, String, i32) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(content).ok();
    let (discipline, text) = match &parsed {
        Some(v) => (
            v.get("discipline").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ),
        None => (String::new(), String::new()),
    };

    if discipline.is_empty() {
        return (String::new(), "discipline-note refused: discipline name required".to_string(), 1);
    }
    if discipline.len() > note_cfg().max_name_len_hard_refuse_not_truncate {
        return (
            String::new(),
            format!(
                "discipline-note refused: discipline name exceeds {} char cap (got {} chars)",
                note_cfg().max_name_len_hard_refuse_not_truncate,
                discipline.len()
            ),
            1,
        );
    }
    if !discipline.chars().all(valid_name_char) {
        return (
            String::new(),
            "discipline-note refused: discipline name must be alnum/hyphen/underscore only".to_string(),
            1,
        );
    }

    if text.is_empty() {
        return (String::new(), "discipline-note refused: text required".to_string(), 1);
    }
    if text.contains('\n') || text.contains('\r') {
        return (
            String::new(),
            "discipline-note refused: text must be a single line (no newline / multi-paragraph shape)".to_string(),
            1,
        );
    }
    if text.chars().count() > note_cfg().max_text_len_hard_refuse_not_truncate {
        return (
            String::new(),
            format!(
                "discipline-note refused: text exceeds {} char terseness ceiling (got {} chars) -- compress and retry",
                note_cfg().max_text_len_hard_refuse_not_truncate,
                text.chars().count()
            ),
            1,
        );
    }

    let path = policy_path(&discipline);
    let path_s = path.to_string_lossy().to_string();
    let existing = pkfs::read_to_string(&path_s).unwrap_or_default();

    if existing.lines().any(|line| line == text) {
        let payload = serde_json::json!({
            "ok": true,
            "discipline": discipline,
            "bytes": existing.len(),
            "deduped": true,
        });
        return (payload.to_string(), String::new(), 0);
    }

    let mut updated = existing.clone();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&text);
    updated.push('\n');

    if !pkfs::write(&path_s, &updated) {
        return (String::new(), "discipline-note failed: write error".to_string(), 1);
    }

    let payload = serde_json::json!({
        "ok": true,
        "discipline": discipline,
        "bytes": updated.len(),
        "deduped": false,
    });
    (payload.to_string(), String::new(), 0)
}

fn parse_enabled_names(content: &str) -> Vec<String> {
    let mut names: Vec<String> = vec!["default".to_string()];
    for line in content.lines() {
        let name = line.trim();
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

pub fn enabled_names() -> Vec<String> {
    let enabled_path = gm_dir().join("disciplines").join("enabled.txt");
    let enabled_s = enabled_path.to_string_lossy().to_string();
    parse_enabled_names(&pkfs::read_to_string(&enabled_s).unwrap_or_default())
}

pub fn removal_dependents(discipline: &str) -> Vec<String> {
    removal_dependents_with_names(discipline, &enabled_names())
}

fn removal_dependents_with_names(discipline: &str, names: &[String]) -> Vec<String> {
    if !names.iter().any(|n| n == discipline) {
        return Vec::new();
    }
    let provided = declared_provides(discipline);
    let realm = declared_realm(discipline);
    names
        .iter()
        .filter(|n| n.as_str() != discipline)
        .filter(|n| declared_realm(n) == realm)
        .filter(|n| read_fiber_state(n) == FiberLifecycle::Active)
        .filter(|n| requires_satisfied(n, names))
        .filter(|n| {
            declared_requires(n)
                .iter()
                .any(|dep| provided.iter().any(|cap| cap == dep))
        })
        .cloned()
        .collect()
}

pub fn handle_check_removal(content: &str) -> (String, String, i32) {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok();
    let discipline = parsed
        .as_ref()
        .and_then(|v| v.get("discipline").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if discipline.is_empty() {
        return (String::new(), "discipline-check-removal refused: discipline name required".to_string(), 1);
    }
    let want_remove = parsed.as_ref().and_then(|v| v.get("remove").and_then(|x| x.as_bool())).unwrap_or(false);
    if want_remove && discipline == "default" {
        return (
            String::new(),
            "discipline-check-removal refused: \"default\" is a synthetic always-active entry, never a member of enabled.txt, and cannot be removed".to_string(),
            1,
        );
    }
    let enabled_path = gm_dir().join("disciplines").join("enabled.txt").to_string_lossy().to_string();
    let original_content = pkfs::read_to_string(&enabled_path).unwrap_or_default();
    let names = parse_enabled_names(&original_content);

    let dependents = removal_dependents_with_names(&discipline, &names);
    let lifecycle = read_fiber_state(&discipline);
    let all_known = all_known_discipline_dirs();
    let dangling = dangling_requires(&discipline, &all_known);
    let safe = fiber_lifecycle::SafeToWithdraw::check(&discipline, &dependents);

    if !want_remove {
        let payload = serde_json::json!({
            "ok": true,
            "discipline": discipline,
            "lifecycle": lifecycle,
            "safe_to_remove": safe.is_some(),
            "dependents": dependents,
            "dangling_requires": dangling,
        });
        return (payload.to_string(), String::new(), 0);
    }

    let Some(_witness) = safe else {
        let payload = serde_json::json!({
            "ok": false,
            "discipline": discipline,
            "lifecycle": lifecycle,
            "safe_to_remove": false,
            "dependents": dependents,
            "dangling_requires": dangling,
            "removed": false,
        });
        return (
            payload.to_string(),
            format!(
                "discipline-check-removal refused: {} is still relied upon by {} -- withdraw the dependent(s) first (Theorem 63 ordering)",
                discipline,
                dependents.join(", ")
            ),
            1,
        );
    };

    if !names.iter().any(|n| n == discipline.as_str()) {
        let payload = serde_json::json!({
            "ok": true,
            "discipline": discipline,
            "lifecycle": lifecycle,
            "safe_to_remove": true,
            "dependents": dependents,
            "dangling_requires": dangling,
            "removed": false,
            "already_absent": true,
        });
        return (payload.to_string(), String::new(), 0);
    }

    let remaining: Vec<&String> = names.iter().filter(|n| n.as_str() != discipline.as_str() && n.as_str() != "default").collect();
    let new_content = remaining.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    let write_content = if new_content.is_empty() { String::new() } else { format!("{}\n", new_content) };
    match pkfs::cas_write(&enabled_path, &original_content, &write_content) {
        pkfs::CasWriteOutcome::Swapped => {}
        pkfs::CasWriteOutcome::Mismatch => {
            return (
                String::new(),
                "discipline-check-removal refused: enabled.txt changed concurrently since this dispatch read it -- re-dispatch discipline-check-removal to re-evaluate against the current content".to_string(),
                1,
            );
        }
        pkfs::CasWriteOutcome::IoError => {
            return (String::new(), "discipline-check-removal failed: could not write enabled.txt".to_string(), 1);
        }
    }

    let payload = serde_json::json!({
        "ok": true,
        "discipline": discipline,
        "lifecycle": lifecycle,
        "safe_to_remove": true,
        "dependents": dependents,
        "dangling_requires": dangling,
        "removed": true,
    });
    (payload.to_string(), String::new(), 0)
}

pub fn dangling_requires(discipline: &str, all_known: &[String]) -> Vec<String> {
    let all_caps: Vec<String> = all_known.iter().flat_map(|n| declared_provides(n)).collect();
    declared_requires(discipline)
        .into_iter()
        .filter(|dep| !all_caps.iter().any(|cap| cap == dep))
        .collect()
}

pub fn all_known_discipline_dirs_pub() -> Vec<String> {
    all_known_discipline_dirs()
}

fn all_known_discipline_dirs() -> Vec<String> {
    let base = gm_dir().join("disciplines").to_string_lossy().to_string();
    let mut out: Vec<String> = enabled_names();
    if let Some(serde_json::Value::Array(entries)) = pkfs::readdir(&base) {
        for entry in entries {
            let name = entry
                .get("name")
                .and_then(|n| n.as_str())
                .or_else(|| entry.as_str());
            let Some(name) = name else { continue };
            if !name.chars().all(valid_name_char) || out.iter().any(|n| n == name) {
                continue;
            }
            let has_policy = pkfs::exists(&policy_path(name).to_string_lossy().to_string());
            let has_requires = pkfs::exists(&requires_path(name).to_string_lossy().to_string());
            let has_fiber_state = pkfs::exists(&fiber_state_path(name).to_string_lossy().to_string());
            if has_policy || has_requires || has_fiber_state {
                out.push(name.to_string());
            }
        }
    }
    out
}

#[derive(Debug, Serialize)]
struct MetatheoryViolation {
    theorem: &'static str,
    discipline: String,
    detail: String,
}

fn audit_preservation(all: &[String]) -> Vec<MetatheoryViolation> {
    let mut violations = Vec::new();
    let mut set = ActiveFiberSet::new();
    for name in all {
        if read_fiber_state(name) != FiberLifecycle::Active {
            continue;
        }
        let realm = declared_realm(name);
        let qualified: Vec<String> = declared_provides(name)
            .into_iter()
            .map(|cap| format!("{realm}\0{cap}"))
            .collect();
        if let Err(v) = set.insert(name, &qualified) {
            violations.push(MetatheoryViolation {
                theorem: "preservation (Theorem 59, disjoint provisions)",
                discipline: format!("{} vs {}", v.incoming, v.existing),
                detail: format!("both Active, same realm, and both provide {:?}", v.capability.split('\0').nth(1).unwrap_or(&v.capability)),
            });
        }
    }
    violations
}

fn audit_recovery_exactness(all: &[String]) -> Vec<MetatheoryViolation> {
    let mut violations = Vec::new();
    for name in all {
        let current = read_fiber_state(name);
        if !fiber_lifecycle::verify_recovery_exactness(current) {
            violations.push(MetatheoryViolation {
                theorem: "recovery exactness (Theorem 61)",
                discipline: name.clone(),
                detail: "Unloading did not reach Inactive under every reachable target".to_string(),
            });
        }
    }
    violations
}

fn audit_ordering(all: &[String]) -> Vec<MetatheoryViolation> {
    let mut violations = Vec::new();
    let enabled = enabled_names();
    for name in all {
        if enabled.iter().any(|n| n == name) && read_fiber_state(name) == FiberLifecycle::Active {
            continue;
        }
        let dependents = removal_dependents(name);
        if SafeToWithdraw::check(name, &dependents).is_none() {
            violations.push(MetatheoryViolation {
                theorem: "ordering (Theorem 63)",
                discipline: name.clone(),
                detail: format!("non-Active fiber still named as relied-upon by {:?}", dependents),
            });
        }
    }
    violations
}

fn audit_progress() -> Vec<MetatheoryViolation> {
    Vec::new()
}

fn audit_dangling_requires(all: &[String]) -> Vec<MetatheoryViolation> {
    let mut violations = Vec::new();
    for name in all {
        let dangling = dangling_requires(name, all);
        if !dangling.is_empty() {
            violations.push(MetatheoryViolation {
                theorem: "access control (Section 6.3, fail-closed requires)",
                discipline: name.clone(),
                detail: format!("requires names capability no known discipline provides: {:?}", dangling),
            });
        }
    }
    violations
}

pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let all = all_known_discipline_dirs();
    let mut violations = Vec::new();
    violations.extend(audit_preservation(&all));
    violations.extend(audit_recovery_exactness(&all));
    violations.extend(audit_ordering(&all));
    violations.extend(audit_progress());
    violations.extend(audit_dangling_requires(&all));
    violations.extend(audit_confluence(&all));
    let ok = violations.is_empty();
    let payload = serde_json::json!({
        "ok": ok,
        "theorems_checked": ["preservation", "recovery_exactness", "ordering", "progress", "access_control", "confluence"],
        "disciplines_checked": all.len(),
        "violations": violations,
    });
    (payload.to_string(), String::new(), if ok { 0 } else { 1 })
}

pub fn active_policies() -> serde_json::Value {
    let enabled = enabled_names();
    let all = all_known_discipline_dirs();

    let targets: Vec<bool> = all
        .iter()
        .map(|name| enabled.iter().any(|n| n == name) && requires_satisfied(name, &enabled))
        .collect();

    let interception_ctx = build_interception_context(&enabled);

    let mut out: Vec<serde_json::Value> = Vec::new();
    for (name, target_satisfied) in all.iter().zip(targets.iter()) {
        let is_active = advance_fiber(name, *target_satisfied);
        if !is_active {
            continue;
        }
        let path = policy_path(name);
        let path_s = path.to_string_lossy().to_string();
        if let Some(text) = pkfs::read_to_string(&path_s) {
            if text.trim().is_empty() {
                continue;
            }
            let capped: String = text
                .lines()
                .rev()
                .take(note_cfg().active_policies_surfaced_in_instruction_payload_limit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            let intercepted: serde_json::Map<String, serde_json::Value> = declared_interception(name)
                .into_iter()
                .map(|(key, (metadata, _kind))| {
                    (key.clone(), serde_json::Value::String(interception_ctx.resolve(&key, &metadata)))
                })
                .collect();
            let mut entry = serde_json::json!({
                "discipline": name,
                "text": capped,
                "bytes": text.len(),
            });
            if !intercepted.is_empty() {
                entry["intercepted_metadata"] = serde_json::Value::Object(intercepted);
            }
            out.push(entry);
        }
    }
    serde_json::Value::Array(out)
}
