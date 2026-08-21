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

/// A discipline read as a Cordis component: the coeffect specification (d),
/// the provision (p), and the effect witness (e). Unifies what
/// `declared_requires`/`declared_provides`/`policy.md` otherwise track as
/// three independently-read files, giving one canonical place that asserts
/// a discipline IS a component in the paper's sense (Definition 43: a
/// component over a context is the triple (d, p, e)) rather than three
/// files that happen to correspond.
pub struct Component {
    pub name: String,
    /// d: the coeffect specification -- capability keys this component
    /// requires from the environment.
    pub requires: Vec<String>,
    /// p: the provision -- capability keys this component supplies.
    pub provides: Vec<String>,
    /// e: whether this component's effect (its policy.md, the text the
    /// runtime surfaces/removes as the discipline activates/deactivates)
    /// is currently non-empty, i.e. whether it has an effect to contribute
    /// at all.
    pub has_effect: bool,
}

impl Component {
    pub fn read(name: &str) -> Component {
        let policy_text = pkfs::read_to_string(&policy_path(name).to_string_lossy().to_string());
        Component {
            name: name.to_string(),
            requires: declared_requires(name),
            provides: declared_provides(name),
            has_effect: policy_text.map(|t| !t.trim().is_empty()).unwrap_or(false),
        }
    }
}

/// Reads a string array field out of a discipline's `requires.json`. `field`
/// is `"requires"` or `"provides"`; both share one manifest file since a
/// discipline's coeffect specification (what it needs) and provision (what
/// it supplies) are two views of the same interface (paper Definition 43:
/// a component is (d, p, e), specification and provision paired).
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

/// The capability keys a discipline supplies. A discipline with no
/// `provides` field (or no `requires.json` at all) implicitly provides
/// exactly its own name, so a bare-name `requires` entry written before
/// this field existed keeps resolving the same way.
fn declared_provides(discipline: &str) -> Vec<String> {
    let explicit = declared_field(discipline, "provides");
    if explicit.is_empty() {
        vec![discipline.to_string()]
    } else {
        explicit
    }
}

/// Reactive-coeffect satisfaction: a discipline activates only when every
/// name in its declared `requires` is a capability some enabled discipline's
/// `provides` supplies. `enabled_names` is the full activation-eligible set
/// (already includes "default"); this never recurses beyond one hop, so a
/// requires-cycle simply leaves every disc in it unsatisfied rather than
/// looping.
fn requires_satisfied(discipline: &str, enabled_names: &[String]) -> bool {
    declared_requires(discipline).iter().all(|dep| {
        enabled_names
            .iter()
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

/// The names currently in `enabled.txt`, "default" always first. Shared by
/// `active_policies` and the withdrawal guard so both read the same
/// activation-eligible set.
fn enabled_names() -> Vec<String> {
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
    names
}

/// Withdrawal-ordering guard (paper Section 4.3.1, Theorem 63): a
/// discipline may be safely disabled/removed from `enabled.txt` only once
/// no other currently-enabled, requires-satisfied discipline still resolves
/// a dependency to one of its declared `provides`. Names every dependent
/// still relying on it, mirroring `relied_n(gamma)` -- the caller (a human
/// or `handle_check_removal`) decides whether to disable anyway, but is
/// never left guessing which dependent would break.
pub fn removal_dependents(discipline: &str) -> Vec<String> {
    let names = enabled_names();
    if !names.iter().any(|n| n == discipline) {
        return Vec::new();
    }
    let provided = declared_provides(discipline);
    names
        .iter()
        .filter(|n| n.as_str() != discipline)
        .filter(|n| requires_satisfied(n, &names))
        .filter(|n| {
            declared_requires(n)
                .iter()
                .any(|dep| provided.iter().any(|cap| cap == dep))
        })
        .cloned()
        .collect()
}

/// Verb entry point for `discipline-check-removal`: reports whether a
/// discipline is safe to disable right now, and names every dependent that
/// would lose a satisfied requirement if it were.
pub fn handle_check_removal(content: &str) -> (String, String, i32) {
    let discipline = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("discipline").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    if discipline.is_empty() {
        return (String::new(), "discipline-check-removal refused: discipline name required".to_string(), 1);
    }
    let dependents = removal_dependents(&discipline);
    let payload = serde_json::json!({
        "ok": true,
        "discipline": discipline,
        "safe_to_remove": dependents.is_empty(),
        "dependents": dependents,
    });
    (payload.to_string(), String::new(), 0)
}

pub fn active_policies() -> serde_json::Value {
    let names = enabled_names();

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
