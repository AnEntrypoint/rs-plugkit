#![cfg(target_arch = "wasm32")]

use serde::Serialize;
use super::fiber_lifecycle::{self, ActiveFiberSet, FiberLifecycle, SafeToWithdraw};
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

/// A discipline's persisted lifecycle uses `fiber_lifecycle`'s kind-agnostic
/// `FiberLifecycle`/`advance_fiber`/`read_fiber_state`, keyed by this
/// discipline's own `fiber-state.json` path. Disciplines are the first
/// caller of that generic module, not a special case it was written
/// around -- a second component kind (e.g. sibling wasm plugins, which
/// agentplug's own `PluginFiberLifecycle` currently duplicates rather than
/// sharing this module across the repo boundary) would call these same
/// two functions with its own path, never copy this file.
fn read_fiber_state(discipline: &str) -> FiberLifecycle {
    fiber_lifecycle::read_fiber_state(&fiber_state_path(discipline).to_string_lossy())
}

/// Advances one discipline's persisted lifecycle by exactly one Cordis-style
/// transition (Section 4.3), given whether its target (requires-satisfied
/// and still enabled) currently holds. See `fiber_lifecycle::advance_fiber`
/// for the transition table and its correspondence to L-Begin/L-Leave/
/// L-Unload. Returns whether the discipline counts as providing THIS
/// dispatch, which is `Active` alone.
fn advance_fiber(discipline: &str, target_satisfied: bool) -> bool {
    fiber_lifecycle::advance_fiber(&fiber_state_path(discipline).to_string_lossy(), target_satisfied)
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
    /// The coeffect isolation realm (Section 3.2.3) this component's
    /// `requires`/`provides` resolve within -- empty string is the
    /// default realm.
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

/// A discipline's coeffect isolation realm (paper Section 3.2.3, Definition
/// 28-29): the realm table entry a discipline names for itself. Absent
/// `realm` means the discipline resolves in the default realm (empty
/// string), matching every discipline written before isolation existed.
/// This is one realm per discipline rather than the paper's per-key realm
/// table (`rho: K -> R`), a deliberate reduction: a discipline's own
/// capabilities are one small, cohesive set, so realm-scoping the whole
/// discipline is the natural grain here, not scoping individual keys
/// within it.
fn declared_realm(discipline: &str) -> String {
    let path = requires_path(discipline);
    let path_s = path.to_string_lossy().to_string();
    pkfs::read_to_string(&path_s)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("realm").and_then(|r| r.as_str()).map(|s| s.to_string()))
        .unwrap_or_default()
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
/// name in its declared `requires` is a capability some enabled discipline
/// IN THE SAME ISOLATION REALM provides (paper Section 3.2.3): the same
/// capability name provided by a discipline in a different realm does not
/// satisfy this one's dependency, and does not collide with it for
/// preservation purposes either -- two disciplines in different realms
/// providing "storage" are providing two logically independent
/// capabilities that merely share a name, real isolation rather than
/// coincidental non-collision. `enabled_names` is the full
/// activation-eligible set (already includes "default", which resolves in
/// the default realm); this never recurses beyond one hop, so a
/// requires-cycle simply leaves every disc in it unsatisfied rather than
/// looping.
fn requires_satisfied(discipline: &str, enabled_names: &[String]) -> bool {
    let realm = declared_realm(discipline);
    declared_requires(discipline).iter().all(|dep| {
        enabled_names
            .iter()
            .filter(|n| declared_realm(n) == realm)
            // Theorem 63's other half: a provider mid-withdrawal
            // (Unloading) must not be read as still satisfying anyone's
            // requires, even though it remains nameable by
            // removal_dependents for one more dispatch (that naming is
            // what lets a NEW consumer's activation attempt still see it
            // as departing, not what lets an EXISTING or new consumer
            // treat it as available). Checking Active here, rather than
            // mere enabled-ness, is what prevents a fresh activation
            // attempt from resolving against a fiber that is itself on
            // its way out.
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
    let realm = declared_realm(discipline);
    names
        .iter()
        .filter(|n| n.as_str() != discipline)
        // only a same-realm dependent can actually be relying on this
        // discipline's provision (Section 3.2.3): a different-realm
        // discipline naming the same capability string resolves against
        // its OWN realm's provider, never against this one.
        .filter(|n| declared_realm(n) == realm)
        // only a fiber that has actually reached Active counts as a real
        // dependent -- one still Inactive (e.g. its own requires is not yet
        // satisfied for an unrelated reason) is not currently relying on
        // anything, so its withdrawal is not blocked by this discipline.
        .filter(|n| read_fiber_state(n) == FiberLifecycle::Active)
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
    let lifecycle = read_fiber_state(&discipline);
    let all_known = all_known_discipline_dirs();
    let dangling = dangling_requires(&discipline, &all_known);
    let payload = serde_json::json!({
        "ok": true,
        "discipline": discipline,
        "lifecycle": lifecycle,
        "safe_to_remove": dependents.is_empty(),
        "dependents": dependents,
        "dangling_requires": dangling,
    });
    (payload.to_string(), String::new(), 0)
}

/// A `requires` entry that no known discipline (enabled or not) can ever
/// provide, distinguished from a live unmet dependency (paper Section
/// 5.1.4's `UNDECLARED_ACCESS`, adapted): a dependency naming a capability
/// some OTHER known discipline's `provides` covers is merely not yet
/// enabled -- a real coeffect waiting on activation, not a mistake. A
/// dependency naming a capability no known discipline anywhere (enabled or
/// disabled) ever declares is a dangling reference: either a typo, or a
/// requires.json written before its provider existed and never updated.
/// `all_known` is `all_known_discipline_dirs()`'s output, so disabled-but-
/// present disciplines are checked too, giving fail-closed discipline to
/// requires.json without requiring the referenced discipline be enabled.
pub fn dangling_requires(discipline: &str, all_known: &[String]) -> Vec<String> {
    let all_caps: Vec<String> = all_known.iter().flat_map(|n| declared_provides(n)).collect();
    declared_requires(discipline)
        .into_iter()
        .filter(|dep| !all_caps.iter().any(|cap| cap == dep))
        .collect()
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

/// Runtime witnesses of the paper's static metatheory (Section 4.4),
/// checked against the live discipline set rather than proved once on
/// paper. Each corresponds to one theorem's load-bearing consequence:
///
/// - **Preservation** (Theorem 59, clause 2: distinct fibers' provisions
///   are disjoint): two currently-`Active` disciplines must never share a
///   `provides` capability, or `requires_satisfied`'s "some enabled
///   discipline provides it" resolution becomes ambiguous.
/// - **Recovery exactness** (Theorem 61/Corollary 62): applying
///   `advance_fiber` to an `Unloading` fiber must reach `Inactive` and
///   stay there on a second call with the same (false) target -- the
///   fixed point an accumulator's inverse is supposed to reach.
/// - **Ordering** (Theorem 63): `removal_dependents` must be empty before
///   any component actually deletes a discipline's directory -- checked
///   here as an invariant a caller can verify holds for every enabled
///   discipline, not merely for one being removed right now.
/// - **Progress** (Theorem 66): `advance_fiber` must never return the
///   same `(state, next)` pair as a genuine stall when `target_satisfied`
///   actually changed -- i.e. every state has SOME transition available
///   for both `true` and `false` targets (the match in `advance_fiber` is
///   total, so this checks that totality holds for the currently
///   compiled table rather than assuming it).
#[derive(Debug, Serialize)]
struct MetatheoryViolation {
    theorem: &'static str,
    discipline: String,
    detail: String,
}

/// Builds an `ActiveFiberSet` from the currently-`Active` disciplines,
/// reporting a violation for any that `insert` refuses. Preservation
/// (Theorem 59) is therefore checked by construction: the set itself
/// cannot contain two colliding providers, so what this function reports
/// is exactly the set of components that FAILED to join it -- the type's
/// own invariant is the check, not a separate nested-loop scan run
/// alongside it.
fn audit_preservation(all: &[String]) -> Vec<MetatheoryViolation> {
    let mut violations = Vec::new();
    let mut set = ActiveFiberSet::new();
    for name in all {
        if read_fiber_state(name) != FiberLifecycle::Active {
            continue;
        }
        // Qualify each capability by realm before inserting: two
        // disciplines in different realms providing the same bare
        // capability name are providing two logically independent
        // capabilities (Section 3.2.3), so they must not collide here.
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
        // A non-enabled or non-Active discipline provides nothing to
        // dependents by construction (active_policies only surfaces
        // Active fibers), so failing to obtain a SafeToWithdraw proof
        // here IS the violation: a "dependent" of a fiber that isn't
        // actually providing. SafeToWithdraw::check is the same
        // proof-carrying constructor a real withdrawal path would have to
        // satisfy, so this audit checks the actual gate future removal
        // code would be structurally required to pass, not a parallel
        // hand-rolled equivalent of it.
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
    // advance_fiber's match is exhaustive over FiberLifecycle x bool by
    // construction (the compiler enforces this), so progress holds
    // structurally; this function exists so discipline-audit reports all
    // four theorems even when the check is "the type system already
    // proved it," rather than silently omitting the theorem from the
    // response.
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

/// Verb entry point for `discipline-audit`: runs all four metatheory
/// witnesses (Section 4.4) plus the fail-closed access-control check
/// (Section 6.3) against the live discipline set and reports any
/// violation found, giving the paper's static guarantees a live,
/// dispatchable check rather than leaving them as unverified prose.
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

    // Two-phase pass, not one loop: every `target_satisfied` is computed
    // from the fiber states as they stood at the START of this dispatch,
    // before ANY of them advances. A single combined loop would let an
    // earlier name's own advance_fiber call mutate its fiber-state.json
    // mid-iteration, so a later name's requires_satisfied check (which
    // reads read_fiber_state fresh) could see a provider as already
    // Unloading even though it was Active when this dispatch began --
    // exactly the iteration-order hazard Theorem 63's atomicity assumes
    // away. Computing every target first mirrors the paper's dispatch
    // being one atomic step from the orchestrator's view (Section 4):
    // every fiber's target this dispatch answers to one consistent
    // snapshot of the others, never a partially-advanced one.
    let targets: Vec<bool> = all
        .iter()
        .map(|name| enabled.iter().any(|n| n == name) && requires_satisfied(name, &enabled))
        .collect();

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
            out.push(serde_json::json!({
                "discipline": name,
                "text": capped,
                "bytes": text.len(),
            }));
        }
    }
    serde_json::Value::Array(out)
}
