#![cfg(target_arch = "wasm32")]

use super::gm_dir;
use crate::pkfs;

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

/// A discipline's coeffect specification: names of other enabled disciplines
/// it depends on. Absent `requires.json` means no declared dependency (the
/// discipline always activates, matching prior behavior). `default` and any
/// name it lists are treated as always-satisfied roots.
fn declared_requires(discipline: &str) -> Vec<String> {
    let path = requires_path(discipline);
    let path_s = path.to_string_lossy().to_string();
    match pkfs::read_to_string(&path_s) {
        Some(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("requires").cloned())
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

/// Reactive-coeffect satisfaction: a discipline activates only when every
/// name in its declared `requires` is itself an enabled discipline with a
/// non-empty policy. `enabled_names` is the full activation-eligible set
/// (already includes "default"); this never recurses beyond one hop, so a
/// requires-cycle simply leaves every disc in it unsatisfied rather than
/// looping.
fn requires_satisfied(discipline: &str, enabled_names: &[String]) -> bool {
    declared_requires(discipline)
        .iter()
        .all(|dep| enabled_names.iter().any(|n| n == dep))
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

pub fn active_policies() -> serde_json::Value {
    let mut names: Vec<String> = vec!["default".to_string()];
    let enabled_path = gm_dir().join("disciplines").join("enabled.txt");
    let enabled_s = enabled_path.to_string_lossy().to_string();
    if let Some(content) = pkfs::read_to_string(&enabled_s) {
        for line in content.lines() {
            let name = line.trim();
            if !name.is_empty() && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }

    let mut out: Vec<serde_json::Value> = Vec::new();
    for name in &names {
        if !requires_satisfied(name, &names) {
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
            out.push(serde_json::json!({
                "discipline": name,
                "text": capped,
                "bytes": text.len(),
            }));
        }
    }
    serde_json::Value::Array(out)
}
