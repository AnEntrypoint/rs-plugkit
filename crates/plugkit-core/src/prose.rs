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
    /// gm-config is the mandatory source for this key and it could not be
    /// reached (no local override, no cached checkout, remote unreachable).
    /// The compiled default is served as the emergency payload so a dispatch
    /// still gets usable text, but this Outcome is distinguishable from
    /// `CompiledDefault` (the normal, healthy "nobody overrode this key"
    /// case) so a caller can surface the reachability failure loudly instead
    /// of treating a config outage as ordinary operation.
    ConfigRepoUnreachable { reason: String },
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::LocalOverride => "local_override",
            Outcome::SourceRepo => "source_repo",
            Outcome::CompiledDefault => "compiled_default",
            Outcome::Degraded { .. } => "degraded",
            Outcome::ConfigRepoUnreachable { .. } => "config_repo_unreachable",
        }
    }

    /// True when an operator wrote configuration that is not taking effect.
    pub fn is_degraded(&self) -> bool {
        matches!(self, Outcome::Degraded { .. } | Outcome::ConfigRepoUnreachable { .. })
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
    if !crate::config_path::path_contained_within(LOCAL_BASE, &local_path) {
        return (
            default.to_string(),
            Outcome::Degraded {
                reason: format!("prose key resolves to {local_path}, which escapes {LOCAL_BASE}"),
            },
        );
    }
    if let Some(text) = read_clean(&local_path) {
        return (text, Outcome::LocalOverride);
    }

    match read_from_source_repo(key) {
        SourceRead::Hit(text) => (text, Outcome::SourceRepo),
        SourceRead::NotConfigured => (default.to_string(), Outcome::CompiledDefault),
        SourceRead::Miss => (default.to_string(), Outcome::CompiledDefault),
        SourceRead::Broken(reason) => (default.to_string(), Outcome::Degraded { reason }),
        SourceRead::ConfigRepoUnreachable(reason) => {
            (default.to_string(), Outcome::ConfigRepoUnreachable { reason })
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn report(key: &str, outcome: &Outcome) {
    match outcome {
        Outcome::Degraded { reason } => {
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
        Outcome::ConfigRepoUnreachable { reason } => {
            crate::wasm_dispatch::emit_event(
                "prose_config_repo_unreachable",
                serde_json::json!({
                    "key": key,
                    "reason": reason,
                    "served": "compiled_default",
                    "detail": "gm-config, the mandatory default prose source, did not resolve for this key. The compiled default was served as an emergency payload -- this project is running on baked-in prose that may be stale relative to gm-config's actual current content, not a healthy no-override state.",
                }),
            );
        }
        _ => {}
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
    /// The mandatory gm-config default source itself never resolved (no
    /// cached checkout anywhere, remote unreachable) -- distinct from
    /// `Broken`, which describes an EXPLICIT project/user `source.json` an
    /// operator wrote and misconfigured. This is not a misconfiguration; it
    /// is the one mandatory dependency being unreachable.
    ConfigRepoUnreachable(String),
}

/// Read one prose key from the config repo the CONFIG chain already fetches.
///
/// This tier was dead on a directory-name disagreement, not a missing feature:
/// `config_sync` clones into `.gm/config-source-cache` while this module read
/// `.gm/instructions-source-cache`, a path nothing has ever written. So a
/// project could point at a config repo, have it cloned and serving for the
/// config chain, and still receive compiled prose forever.
///
/// The layout comes from the repo's own `gm.config.json`: the `instructions`
/// block declares `dir` (default `prose`) alongside the key inventory, and the
/// `messages` block declares `gates_dir` and `residual_dir` for the `gates/`
/// and `residual/` key namespaces, so the repo describes its own shape rather
/// than this module assuming one. A namespace whose directory is not declared
/// falls back to `instructions.dir`, which is what every pre-`messages` config
/// repo relies on.
pub fn config_repo_text(key: &str) -> Option<String> {
    match read_from_config_repo(key) {
        SourceRead::Hit(text) => Some(text),
        _ => None,
    }
}

/// Consults the SAME resolution `config::resolve()` uses for `gm.config.json`
/// itself, rather than guessing a tier's cache dir from a hardcoded constant.
/// `config::resolve()` pulls (calls the fetcher, which materializes/refreshes
/// the winning tier's checkout) before returning, so this is a real pull on
/// every prose resolution, not a passive read of whatever cache happened to
/// already exist from an unrelated earlier call this session.
fn read_from_config_repo(key: &str) -> SourceRead {
    let resolved = crate::config::resolve();
    match resolved.cache_dir {
        Some(cache_dir) => read_from_cache_root(&cache_dir, key),
        None => SourceRead::ConfigRepoUnreachable(format!(
            "gm-config (the mandatory default prose source) did not resolve: {}",
            resolved.why
        )),
    }
}

const MESSAGE_NAMESPACES: &[(&str, &str)] = &[("gates/", "gates_dir"), ("residual/", "residual_dir")];

struct CacheLocation {
    dir: String,
    stem: String,
    declaring_field: String,
}

fn instructions_location(config: Option<&serde_json::Value>, key: &str) -> CacheLocation {
    CacheLocation {
        dir: config
            .and_then(|v| v.get("instructions"))
            .and_then(|i| i.get("dir"))
            .and_then(|d| d.as_str())
            .unwrap_or("prose")
            .to_string(),
        stem: key.to_string(),
        declaring_field: "instructions.dir".to_string(),
    }
}

fn message_location(config: Option<&serde_json::Value>, key: &str) -> Option<CacheLocation> {
    let messages = config?.get("messages")?;
    for &(namespace, field) in MESSAGE_NAMESPACES {
        let Some(stem) = key.strip_prefix(namespace) else {
            continue;
        };
        if stem.is_empty() {
            return None;
        }
        let dir = messages.get(field).and_then(|d| d.as_str())?;
        return Some(CacheLocation {
            dir: dir.to_string(),
            stem: stem.to_string(),
            declaring_field: format!("messages.{field}"),
        });
    }
    None
}

fn read_from_cache_root(cache: &str, key: &str) -> SourceRead {
    let config = pkfs::read_to_string(&format!("{cache}/gm.config.json"))
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.trim_start_matches('\u{feff}')).ok());
    let CacheLocation { dir, stem, declaring_field } = message_location(config.as_ref(), key)
        .unwrap_or_else(|| instructions_location(config.as_ref(), key));
    if validate_source_path(&dir).is_err() {
        return SourceRead::Broken(format!("{cache}/gm.config.json: {declaring_field} is not a safe relative path"));
    }
    let trimmed = dir.trim().trim_matches('/');
    let full = if trimmed.is_empty() {
        format!("{cache}/{stem}.md")
    } else {
        format!("{cache}/{trimmed}/{stem}.md")
    };
    if !crate::config_path::path_contained_within(cache, &full) {
        return SourceRead::Broken(format!(
            "{cache}/gm.config.json: {declaring_field} resolves to {full}, which escapes {cache}"
        ));
    }
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
    if !crate::config_path::path_contained_within(SOURCE_CACHE_BASE, &full) {
        return SourceRead::Broken(format!(
            "{SOURCE_SPEC_PATH}: `path` resolves to {full}, which escapes {SOURCE_CACHE_BASE}"
        ));
    }
    match read_clean(&full) {
        Some(text) => SourceRead::Hit(text),
        None => SourceRead::Miss,
    }
}
