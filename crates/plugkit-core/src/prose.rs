use crate::config_path::{validate_prose_key, validate_source_path};
use crate::pkfs;

const LOCAL_BASE: &str = ".gm/instructions";

const SOURCE_SPEC_PATH: &str = ".gm/instructions/source.json";

const SOURCE_CACHE_BASE: &str = ".gm/instructions-source-cache";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    LocalOverride,
    SourceRepo,
    CompiledDefault,
    Degraded { reason: String },
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

    pub fn is_degraded(&self) -> bool {
        matches!(self, Outcome::Degraded { .. } | Outcome::ConfigRepoUnreachable { .. })
    }
}

pub fn fill_placeholders(key: &str, template: &str, values: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    let mut missing: Vec<&str> = Vec::new();
    for (name, value) in values {
        let token = format!("{{{name}}}");
        if out.contains(&token) {
            out = out.replace(&token, value);
        } else {
            missing.push(name);
        }
    }
    let unfilled = remaining_placeholders(&out);
    if !missing.is_empty() || !unfilled.is_empty() {
        report_placeholder_mismatch(key, &missing, &unfilled);
    }
    out
}

fn remaining_placeholders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = text[i + 1..].find('}') {
                let inner = &text[i + 1..i + 1 + end];
                if !inner.is_empty()
                    && inner.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                {
                    found.push(inner.to_string());
                }
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
    found
}

#[cfg(target_arch = "wasm32")]
fn report_placeholder_mismatch(key: &str, missing: &[&str], unfilled: &[String]) {
    crate::wasm_dispatch::emit_event(
        "prose_placeholder_mismatch",
        serde_json::json!({
            "key": key,
            "never_substituted": missing,
            "left_literal_in_output": unfilled,
            "detail": "a resolved message dropped a placeholder this call site fills (its value is lost from the output) or names one the call site never supplies (it renders literally). Both read as working text.",
        }),
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn report_placeholder_mismatch(_key: &str, _missing: &[&str], _unfilled: &[String]) {}

pub fn resolve(key: &str, default: &str) -> String {
    let (text, outcome) = resolve_detailed(key, default);
    report(key, &outcome);
    text
}

enum TierResult {
    Answered(String, Outcome),
    FallThrough,
    FallThroughDegraded(Outcome),
    Terminal(Outcome),
}

fn tier1_project_vendored(key: &str) -> TierResult {
    let local_path = format!("{LOCAL_BASE}/{key}.md");
    if !crate::config_path::path_contained_within(LOCAL_BASE, &local_path) {
        return TierResult::Terminal(Outcome::Degraded {
            reason: format!("prose key resolves to {local_path}, which escapes {LOCAL_BASE}"),
        });
    }
    match read_clean(&local_path) {
        Some(text) => TierResult::Answered(text, Outcome::LocalOverride),
        None => TierResult::FallThrough,
    }
}

fn tier2_in_project_repo(key: &str) -> TierResult {
    match read_from_source_repo(key) {
        SourceRead::Hit(text) => TierResult::Answered(text, Outcome::SourceRepo),
        SourceRead::NotConfigured => TierResult::FallThrough,
        SourceRead::Miss => TierResult::FallThrough,
        SourceRead::Broken(reason) => TierResult::FallThroughDegraded(Outcome::Degraded { reason }),
        SourceRead::ConfigRepoUnreachable(reason) => {
            TierResult::FallThroughDegraded(Outcome::ConfigRepoUnreachable { reason })
        }
    }
}

fn tier3_user_wide_repo(_key: &str) -> TierResult {
    TierResult::FallThrough
}

pub fn resolve_detailed(key: &str, default: &str) -> (String, Outcome) {
    if let Err(reason) = validate_prose_key(key) {
        return (default.to_string(), Outcome::Degraded { reason });
    }

    let mut pending_degraded: Option<Outcome> = None;
    for tier in [tier1_project_vendored, tier2_in_project_repo, tier3_user_wide_repo] {
        match tier(key) {
            TierResult::Answered(text, outcome) => return (text, outcome),
            TierResult::Terminal(outcome) => return (default.to_string(), outcome),
            TierResult::FallThrough => {}
            TierResult::FallThroughDegraded(outcome) => {
                if pending_degraded.is_none() {
                    pending_degraded = Some(outcome);
                }
            }
        }
    }

    match pending_degraded {
        Some(outcome) => (default.to_string(), outcome),
        None => (default.to_string(), Outcome::CompiledDefault),
    }
}

#[cfg(target_arch = "wasm32")]
fn report(key: &str, outcome: &Outcome) {
    match outcome {
        Outcome::CompiledDefault
            if !crate::orchestrator::instructions::has_compiled_default_for_prose_key(key) =>
        {
            crate::wasm_dispatch::emit_event(
                "prose_key_has_no_default",
                serde_json::json!({
                    "key": key,
                    "served": "entry_prose_via_fallthrough",
                    "detail": "this prose key has no vendored .gm/instructions/<key>.md and no compiled default, so ENTRY prose was served under its name. The phase is running on the wrong text -- vendor the file or add a compiled default.",
                }),
            );
        }
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
            let marker = serde_json::json!({
                "key": key,
                "reason": reason,
                "ts": crate::orchestrator::state::now_ms(),
            });
            let _ = pkfs::write(
                ".gm/exec-spool/.config-repo-unreachable.json",
                &marker.to_string(),
            );
        }
        _ => {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn report(_key: &str, _outcome: &Outcome) {}

pub fn resolve_and_mark(key: &str, default: &str) -> String {
    let text = resolve(key, default);
    let marker = serde_json::json!({ "key": key, "ts": crate::orchestrator::state::now_ms() });
    let _ = pkfs::write(
        ".gm/exec-spool/.last-gate-fired.json",
        &serde_json::to_string(&marker).unwrap_or_default(),
    );
    text
}

fn read_clean(path: &str) -> Option<String> {
    let raw = pkfs::read_to_string(path)?;
    let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if text.trim().is_empty() { None } else { Some(text) }
}

enum SourceRead {
    Hit(String),
    NotConfigured,
    Miss,
    Broken(String),
    ConfigRepoUnreachable(String),
}

pub fn config_repo_text(key: &str) -> Option<String> {
    match read_from_config_repo(key) {
        SourceRead::Hit(text) => Some(text),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
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

#[cfg(not(target_arch = "wasm32"))]
fn read_from_config_repo(_key: &str) -> SourceRead {
    SourceRead::ConfigRepoUnreachable(
        "gm-config (the mandatory default prose source) requires wasm32 (config::resolve's git-backed fetcher is a wasm-host-bridge operation)".to_string()
    )
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
    let has_repo_field = cfg.get("repo").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty()).unwrap_or(false);
    if has_repo_field {
        return read_from_repo_spec_schema(key, &cfg_raw);
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

#[cfg(target_arch = "wasm32")]
fn read_from_repo_spec_schema(key: &str, cfg_raw: &str) -> SourceRead {
    let fetcher = crate::config_sync::GitRepoFetcher::default();
    let src = match crate::config::resolve_prose_repo_source(
        cfg_raw,
        SOURCE_SPEC_PATH,
        SOURCE_CACHE_BASE,
        "prose_source_repo",
        &fetcher,
    ) {
        Ok(src) => src,
        Err(reason) => return SourceRead::Broken(reason),
    };
    let full = format!("{}/{key}.md", src.cache_dir);
    if !crate::config_path::path_contained_within(&src.cache_dir, &full) {
        return SourceRead::Broken(format!(
            "{SOURCE_SPEC_PATH}: key {key} resolves to {full}, which escapes {}",
            src.cache_dir
        ));
    }
    match read_clean(&full) {
        Some(text) => SourceRead::Hit(text),
        None => SourceRead::Miss,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_from_repo_spec_schema(_key: &str, _cfg_raw: &str) -> SourceRead {
    SourceRead::NotConfigured
}
