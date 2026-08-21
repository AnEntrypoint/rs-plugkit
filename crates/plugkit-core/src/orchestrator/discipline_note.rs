#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
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

fn fiber_state_path(discipline: &str) -> std::path::PathBuf {
    gm_dir().join("disciplines").join(discipline).join("fiber-state.json")
}

/// A discipline's persisted lifecycle state (paper Section 4.3, Definition
/// 49). gm has no async load step for a discipline -- reading policy.md is
/// synchronous, so there is no analogue of `Reloading`'s in-flight window --
/// leaving three reachable states: `Inactive` (never activated, or fully
/// withdrawn), `Active` (currently providing), and `Unloading` (target
/// unsatisfied or removed from `enabled.txt`, but the previous dispatch's
/// dependents have not yet had a chance to observe the loss). This mirrors
/// L-Leave/L-Unload's split (Section 4.3.1): a fiber stops providing (drops
/// out of the coeffect context) the moment it enters `Unloading`, but the
/// fiber itself, and hence `removal_dependents`'s ability to name it as a
/// still-present-but-leaving provider, persists one more dispatch before the
/// state collapses to `Inactive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiberLifecycle {
    Inactive,
    Active,
    Unloading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FiberState {
    state: FiberLifecycle,
    #[serde(default)]
    updated_at_ms: u128,
}

fn read_fiber_state(discipline: &str) -> FiberState {
    let path = fiber_state_path(discipline).to_string_lossy().to_string();
    pkfs::read_to_string(&path)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(FiberState { state: FiberLifecycle::Inactive, updated_at_ms: 0 })
}

fn write_fiber_state(discipline: &str, state: FiberLifecycle) {
    let path = fiber_state_path(discipline).to_string_lossy().to_string();
    let body = FiberState { state, updated_at_ms: super::state::now_ms() };
    if let Ok(text) = serde_json::to_string(&body) {
        let _ = pkfs::write(&path, &text);
    }
}

/// Advances one discipline's persisted lifecycle by exactly one Cordis-style
/// transition, given whether its target (requires-satisfied and still
/// enabled) currently holds. Mirrors L-Begin/L-Leave/L-Unload (Section
/// 4.3): `Inactive` -> `Active` when the target becomes satisfied;
/// `Active` -> `Unloading` the instant it stops being satisfied (this is
/// L-Leave -- the fiber records the decision to deactivate without yet
/// discarding it, so `removal_dependents` still sees it as present-but-
/// leaving for one more dispatch); `Unloading` -> `Inactive` on the
/// following dispatch (L-Unload -- the withdrawal completes). Returns
/// whether the discipline counts as providing THIS dispatch, which is
/// `Active` alone -- an `Unloading` fiber's own withdrawal is in flight and
/// must not itself be read as still satisfying anyone's `requires`.
fn advance_fiber(discipline: &str, target_satisfied: bool) -> bool {
    let current = read_fiber_state(discipline).state;
    let next = match (current, target_satisfied) {
        (FiberLifecycle::Inactive, true) => FiberLifecycle::Active,
        (FiberLifecycle::Inactive, false) => FiberLifecycle::Inactive,
        (FiberLifecycle::Active, true) => FiberLifecycle::Active,
        (FiberLifecycle::Active, false) => FiberLifecycle::Unloading,
        (FiberLifecycle::Unloading, _) => FiberLifecycle::Inactive,
    };
    if next != current {
        write_fiber_state(discipline, next);
    }
    next == FiberLifecycle::Active
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
    /// theta: the persisted lifecycle state (Definition 44/49), read
    /// as-is without advancing it -- a caller wanting the transition side
    /// effect calls `advance_fiber` (via `active_policies`) instead.
    pub lifecycle: FiberLifecycle,
}

impl Component {
    pub fn read(name: &str) -> Component {
        let policy_text = pkfs::read_to_string(&policy_path(name).to_string_lossy().to_string());
        Component {
            name: name.to_string(),
            requires: declared_requires(name),
            provides: declared_provides(name),
            has_effect: policy_text.map(|t| !t.trim().is_empty()).unwrap_or(false),
            lifecycle: read_fiber_state(name).state,
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
        // only a fiber that has actually reached Active counts as a real
        // dependent -- one still Inactive (e.g. its own requires is not yet
        // satisfied for an unrelated reason) is not currently relying on
        // anything, so its withdrawal is not blocked by this discipline.
        .filter(|n| read_fiber_state(n).state == FiberLifecycle::Active)
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
    let lifecycle = read_fiber_state(&discipline).state;
    let payload = serde_json::json!({
        "ok": true,
        "discipline": discipline,
        "lifecycle": lifecycle,
        "safe_to_remove": dependents.is_empty(),
        "dependents": dependents,
    });
    (payload.to_string(), String::new(), 0)
}

/// Every discipline this project has ever recorded a fiber state or a
/// policy directory for -- the union of `enabled_names()` (activation
/// candidates) with any name whose `.gm/disciplines/<name>/` directory
/// already exists but is no longer enabled. A name freshly removed from
/// `enabled.txt` still needs its `advance_fiber` called with
/// `target_satisfied=false` so an `Active` fiber transitions to
/// `Unloading` rather than being silently forgotten (which would leave a
/// stale `Active` fiber-state.json behind forever, undetected by
/// `removal_dependents` since that walks `enabled_names()` alone).
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
            // enabled.txt itself is a file in this directory, not a
            // discipline; a real discipline dir has a policy.md or
            // requires.json under it, which enabled.txt cannot.
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

pub fn active_policies() -> serde_json::Value {
    let enabled = enabled_names();
    let all = all_known_discipline_dirs();

    let mut out: Vec<serde_json::Value> = Vec::new();
    for name in &all {
        let target_satisfied = enabled.iter().any(|n| n == name) && requires_satisfied(name, &enabled);
        let is_active = advance_fiber(name, target_satisfied);
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
            out.push(serde_json::json!({
                "discipline": name,
                "text": capped,
                "bytes": text.len(),
            }));
        }
    }
    serde_json::Value::Array(out)
}
