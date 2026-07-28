//! Tiered resolution of instruction/gate/residual prose.
//!
//! Resolution order, first non-empty wins: project-vendored
//! `.gm/instructions/<key>.md`, then a repo-backed source materialized under
//! `.gm/instructions-source-cache`, then the compiled default the caller
//! supplies. The compiled default cannot fail, so [`resolve`] is total.
//!
//! # Why every non-hit is reported
//!
//! This chain previously returned `None` identically for "no source configured",
//! "source.json is malformed", "cache is cold", and "this key is absent from the
//! repo". Only the first is a normal state; the middle two mean an operator
//! wrote configuration that is doing nothing, and they were indistinguishable
//! from success at the call site. [`Outcome`] separates them, and
//! [`resolve_reporting`] emits the ones that indicate a broken configuration --
//! a tier that is CONFIGURED but not resolving is the failure mode the whole
//! tiered design exists to make visible.
//!
//! Reporting is emit-only and never changes what is served: a broken tier still
//! falls through to the next one, because refusing to serve prose would take a
//! session down over an advisory override.
//!
//! # Untrusted inputs
//!
//! Both the KEY and the source spec's `path` reach a `format!` that builds a
//! filesystem path, and neither is authored by this crate -- keys come from
//! `fsm::graph()` state values (and `graph.json` is a vendorable, operator-
//! edited artifact), `path` comes from a JSON file that may itself have been
//! vendored. Both are validated by `config_path` before any interpolation, and
//! a rejected value is reported and skipped rather than rewritten.

use crate::config_path::{validate_prose_key, validate_source_path};
use crate::pkfs;

/// Directory holding project-vendored overrides.
const LOCAL_BASE: &str = ".gm/instructions";

/// Spec file naming a repo-backed prose source.
const SOURCE_SPEC_PATH: &str = ".gm/instructions/source.json";

/// Where a repo-backed prose source is materialized.
const SOURCE_CACHE_BASE: &str = ".gm/instructions-source-cache";

/// Which tier answered, or why none did.
///
/// Carries the failure REASON rather than a bare bool because the whole point
/// of separating these is that an operator can act on "your source.json is not
/// valid JSON" and cannot act on "prose resolved".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Project-vendored `.gm/instructions/<key>.md` supplied the text.
    LocalOverride,
    /// The repo-backed cache supplied the text.
    SourceRepo,
    /// No tier was configured; the compiled default is the intended answer.
    CompiledDefault,
    /// A tier WAS configured but could not be used. `reason` names the file and
    /// the specific defect; the compiled default was served anyway.
    Degraded { reason: String },
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::LocalOverride => "local_override",
            Outcome::SourceRepo => "source_repo",
            Outcome::CompiledDefault => "compiled_default",
            Outcome::Degraded { .. } => "degraded",
        }
    }

    /// True when an operator wrote configuration that is not taking effect.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Outcome::Degraded { .. })
    }
}

/// Resolve `key`, reporting any configured-but-broken tier to the watcher log.
///
/// The entry point every call site uses. Reporting lives here rather than in
/// [`resolve_detailed`] so the pure resolution stays side-effect-free and
/// callable from a harness.
pub fn resolve(key: &str, default: &str) -> String {
    let (text, outcome) = resolve_detailed(key, default);
    report(key, &outcome);
    text
}

/// Resolve without emitting. Returns the text and which tier produced it.
pub fn resolve_detailed(key: &str, default: &str) -> (String, Outcome) {
    if let Err(reason) = validate_prose_key(key) {
        return (default.to_string(), Outcome::Degraded { reason });
    }

    let local_path = format!("{LOCAL_BASE}/{key}.md");
    if let Some(text) = read_clean(&local_path) {
        return (text, Outcome::LocalOverride);
    }

    match read_from_source_repo(key) {
        SourceRead::Hit(text) => (text, Outcome::SourceRepo),
        SourceRead::NotConfigured => (default.to_string(), Outcome::CompiledDefault),
        SourceRead::Miss => (default.to_string(), Outcome::CompiledDefault),
        SourceRead::Broken(reason) => (default.to_string(), Outcome::Degraded { reason }),
    }
}

#[cfg(target_arch = "wasm32")]
fn report(key: &str, outcome: &Outcome) {
    if let Outcome::Degraded { reason } = outcome {
        crate::wasm_dispatch::emit_event(
            "prose_tier_degraded",
            serde_json::json!({
                "key": key,
                "reason": reason,
                "served": "compiled_default",
                "detail": "a prose tier is configured but could not be used, so the compiled default was served instead. The override is silently inert until this is fixed.",
            }),
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn report(_key: &str, _outcome: &Outcome) {}

/// Resolve and record which key was most recently served, for the gate-fired
/// marker other subsystems read.
pub fn resolve_and_mark(key: &str, default: &str) -> String {
    let text = resolve(key, default);
    let marker = serde_json::json!({ "key": key, "ts": crate::orchestrator::state::now_ms() });
    let _ = pkfs::write(
        ".gm/exec-spool/.last-gate-fired.json",
        &serde_json::to_string(&marker).unwrap_or_default(),
    );
    text
}

/// Read a prose file, normalising the two encodings a hand-edited markdown file
/// arrives in and treating blank content as absent.
///
/// Whitespace-only means "not set", not "serve nothing": an override file
/// truncated to empty by a failed write would otherwise blank out an
/// instruction the agent needs, which is a worse failure than ignoring it.
fn read_clean(path: &str) -> Option<String> {
    let raw = pkfs::read_to_string(path)?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if text.trim().is_empty() { None } else { Some(text) }
}

/// The four genuinely distinct results of consulting the repo-backed tier,
/// which the previous `Option<String>` collapsed into two.
enum SourceRead {
    Hit(String),
    /// No `source.json`. The overwhelmingly common case and not a problem.
    NotConfigured,
    /// Source is configured and readable, but has nothing for this key. Normal
    /// for a repo that overrides only some keys.
    Miss,
    /// Source is configured but unusable. An operator must act.
    Broken(String),
}

/// Read one prose key from the config repo the CONFIG chain already fetches.
///
/// This tier was dead on a directory-name disagreement, not a missing feature:
/// `config_sync` clones into `.gm/config-source-cache` while this module read
/// `.gm/instructions-source-cache`, a path nothing has ever written. So a
/// project could point at a config repo, have it cloned and serving for the
/// config chain, and still receive compiled prose forever.
///
/// The layout comes from the repo's own `gm.config.json`, whose `instructions`
/// block declares `dir` (default `prose`) alongside the key inventory, so the
/// repo describes its own shape rather than this module assuming one.
fn read_from_config_repo(key: &str) -> SourceRead {
    let cache = crate::config::SOURCE_CACHE_REL;
    let dir = pkfs::read_to_string(&format!("{cache}/gm.config.json"))
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| v.get("instructions").and_then(|i| i.get("dir")).and_then(|d| d.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "prose".to_string());
    if validate_source_path(&dir).is_err() {
        return SourceRead::Broken(format!("{cache}/gm.config.json: instructions.dir is not a safe relative path"));
    }
    let trimmed = dir.trim().trim_matches('/');
    let full = if trimmed.is_empty() {
        format!("{cache}/{key}.md")
    } else {
        format!("{cache}/{trimmed}/{key}.md")
    };
    match read_clean(&full) {
        Some(text) => SourceRead::Hit(text),
        None => SourceRead::Miss,
    }
}

fn read_from_source_repo(key: &str) -> SourceRead {
    let Some(cfg_raw) = pkfs::read_to_string(SOURCE_SPEC_PATH) else {
        return read_from_config_repo(key);
    };
    if cfg_raw.trim_start_matches('\u{feff}').trim().is_empty() {
        return SourceRead::NotConfigured;
    }
    let cfg: serde_json::Value = match serde_json::from_str(cfg_raw.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => {
            return SourceRead::Broken(format!("{SOURCE_SPEC_PATH}: not valid JSON: {e}"));
        }
    };
    if !cfg.is_object() {
        return SourceRead::Broken(format!(
            "{SOURCE_SPEC_PATH}: top level must be a JSON object"
        ));
    }
    let raw_path = cfg.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if let Err(reason) = validate_source_path(raw_path) {
        return SourceRead::Broken(format!("{SOURCE_SPEC_PATH}: {reason}"));
    }
    let sub_path = raw_path.trim().trim_matches('/');
    let full = if sub_path.is_empty() {
        format!("{SOURCE_CACHE_BASE}/{key}.md")
    } else {
        format!("{SOURCE_CACHE_BASE}/{sub_path}/{key}.md")
    };
    match read_clean(&full) {
        Some(text) => SourceRead::Hit(text),
        None => SourceRead::Miss,
    }
}
