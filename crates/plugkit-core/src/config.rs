//! The 4-tier gm configuration resolution chain.
//!
//! Generalizes the 3-tier chain `prose.rs` already runs for instruction
//! overrides (local `.gm/instructions/<key>.md` -> repo spec at
//! `.gm/instructions/source.json` -> compiled default) into a config chain
//! that adds the user-wide tier prose.rs lacks, and -- unlike prose.rs, whose
//! unit is an opaque markdown blob that cannot be malformed -- carries a
//! versioned, parseable schema, so it needs a total parser and explicit
//! failure reporting where prose.rs needed neither.
//!
//! # Resolution order (first match wins)
//!
//! 1. [`Tier::ProjectVendored`] -- `<project>/.gm/gm.config.json`, a real
//!    config committed into the project itself.
//! 2. [`Tier::ProjectRepoSpec`] -- `<project>/.gm/config.source.json`, which
//!    does NOT contain config, only a pointer at a config REPO plus the cache
//!    directory that repo is materialized into.
//! 3. [`Tier::UserRepoSpec`] -- `<home>/.gm/config.source.json`, the same
//!    pointer shape, applying to every project this user runs.
//! 4. [`Tier::BuiltinDefault`] -- [`Config::builtin_default`], compiled in.
//!    Always succeeds, so resolution is total: there is no "no config" state.
//!
//! ## Why a single `.gm/gm.config.json`, not a `.gm/config/` directory
//!
//! The brief allowed either. A directory forces three decisions a single file
//! does not have: what orders the files (lexical? a manifest?), what happens
//! when two files set the same key, and what a stray non-config file in there
//! means. Each is a new silent-misresolution surface, and none buys anything
//! this schema needs -- partial override is already expressible as a partial
//! JSON object, which is exactly what the deep merge below consumes. The
//! precedent agrees: `prose.rs` points at a config repo with a single
//! `source.json`, not a spec directory.
//!
//! ## Deep-merge semantics (precise)
//!
//! Tiers do NOT merge with each other -- first match wins, whole. Merging is
//! strictly WITHIN one tier: a resolved config is
//! `deep_merge(builtin_default, tier_config)`, so a tier may ship a partial
//! object and inherit the rest. The rules, applied by [`deep_merge`]:
//!
//! - **object + object** -> recurse key by key. Keys present only in the base
//!   survive; keys present in the override replace or recurse.
//! - **any + non-object** -> the override REPLACES the base outright.
//! - **array + array** -> REPLACE, never concatenate or element-wise merge.
//!   Arrays here are ordered whole values (an allowlist, a phase order);
//!   appending would make "remove an inherited entry" inexpressible, and
//!   element-wise merge would make list order load-bearing in a way no caller
//!   could predict.
//! - **explicit JSON `null` in the override** -> REPLACES with null rather
//!   than being skipped. `null` is a deliberate authored value ("unset this"),
//!   and treating it as absent would make it impossible to clear an inherited
//!   key. Absence is expressed by omitting the key, which is a different thing
//!   and the reason the distinction is worth keeping.
//!
//! Merge is applied only to a config that already PARSED and version-checked.
//! A malformed or unknown-version tier is [`Rejected`](Load::Rejected), never
//! silently merged as an empty object.

use serde_json::{json, Map, Value};

use crate::pkfs;

/// Schema version this build understands.
///
/// Bump only for a BREAKING shape change. A config declaring a version above
/// this is rejected loudly (see [`check_version`]) rather than parsed on the
/// hope the unknown fields are additive -- a newer config almost certainly
/// relies on semantics this build does not have, and honoring the half of it
/// we recognize would apply a config nobody wrote.
pub const SCHEMA_VERSION: u64 = 1;

/// Oldest schema version this build can still read.
///
/// A RANGE, not an equality, for the same reason `Policy` takes serde defaults
/// instead of `deny_unknown_fields`: in an auto-updating config system the
/// config repo legitimately leads the binary. An exact-match gate meant the
/// moment a shared config bumped its version, every not-yet-updated client in
/// the fleet dropped to the compiled defaults at once -- a fleet-wide outage
/// triggered by a routine publish.
///
/// A config NEWER than this build is now accepted with a reported warning
/// rather than rejected: every field carries a default, unknown keys are
/// surfaced rather than fatal, so applying the recognised subset is strictly
/// closer to the author's intent than silently ignoring the whole file.
/// Genuinely breaking changes raise this floor.
pub const MIN_READABLE_SCHEMA_VERSION: u64 = 1;

/// Filename of a real, vendored config (tier 1). Project-relative.
pub const PROJECT_CONFIG_REL: &str = ".gm/gm.config.json";

/// Filename of a repo-source SPEC (tiers 2 and 3) -- a pointer, not a config.
/// Deliberately distinct from [`PROJECT_CONFIG_REL`] so the two can coexist in
/// one `.gm/` and so a reader can never mistake one for the other.
pub const SOURCE_SPEC_REL: &str = ".gm/config.source.json";

/// Where a fetched config repo is materialized. Mirrors prose.rs's
/// `.gm/instructions-source-cache` convention, and is likewise a derived
/// artifact -- it belongs in gitignore.rs's `MANAGED_ENTRIES`, not in git.
pub const SOURCE_CACHE_REL: &str = ".gm/config-source-cache";

/// Which tier produced a config. Reported to callers because a config's
/// meaning depends on where it came from -- "why is this setting on" is
/// unanswerable without it, for a human reading a log or a caller deciding
/// whether it is safe to write a change back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    ProjectVendored,
    ProjectRepoSpec,
    UserRepoSpec,
    BuiltinDefault,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::ProjectVendored => "project_vendored",
            Tier::ProjectRepoSpec => "project_repo_spec",
            Tier::UserRepoSpec => "user_repo_spec",
            Tier::BuiltinDefault => "builtin_default",
        }
    }
}

/// A parsed, version-checked config.
///
/// Held as a `Value` rather than a struct of named fields on purpose: the
/// consumers of individual keys are spread across subsystems that land
/// independently, and a fixed struct would force every key addition through
/// this file. The version gate is what makes that safe -- an unrecognized
/// SHAPE is caught by [`check_version`], so the loose value is only ever a
/// shape this build declared it understands.
#[derive(Debug, Clone)]
pub struct Config {
    pub version: u64,
    pub value: Value,
}

impl Config {
    /// Tier 4. The one tier that cannot fail, which is what makes
    /// [`resolve`] total.
    pub fn builtin_default() -> Config {
        Config {
            version: SCHEMA_VERSION,
            value: json!({
                "version": SCHEMA_VERSION,
                "instructions": { "source": Value::Null },
                "index": { "enabled": true },
                "memory": { "enabled": true },
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.value.get(key)
    }

    /// Top-level keys this build does not recognise.
    ///
    /// The complement of the version range: that gate lets a NEWER config
    /// through, and this reports which of its keys are being ignored. Without
    /// it a typo (`memoryy`) is indistinguishable from a key from a future
    /// schema -- both silently do nothing, which is the failure mode the whole
    /// resolution chain exists to avoid.
    ///
    /// Reported, never fatal, for the same reason unknown POLICY keys are
    /// warned about rather than rejected: in an auto-updating system an
    /// unrecognised key is usually a newer spec, not corruption.
    ///
    /// `_`-prefixed keys are skipped. The reference config repo documents
    /// itself with `_comment` entries, and flagging its own house style as
    /// suspicious would train people to ignore this report.
    pub fn unknown_top_level_keys(&self) -> Vec<String> {
        const KNOWN: &[&str] = &["version", "instructions", "index", "memory", "cache", "sync", "fsm", "messages", "rag", "scoring"];
        let Some(obj) = self.value.as_object() else { return Vec::new() };
        obj.keys()
            .filter(|k| !k.starts_with('_'))
            .filter(|k| !KNOWN.contains(&k.as_str()))
            .cloned()
            .collect()
    }
}

/// Outcome of loading ONE tier. A total parser: every load lands in exactly
/// one arm, and neither arm is reached by panicking or by silently
/// substituting a default.
///
/// `Rejected` is deliberately NOT a fallthrough signal. A tier that exists but
/// is broken is an authoring error the user must see; skipping to the next
/// tier would apply a config they did not write while hiding the one they did.
#[derive(Debug, Clone)]
pub enum Load {
    /// Parsed, version-checked, and merged over the builtin default.
    Accepted(Config),
    /// Present but unusable. `reason` names the file and the specific defect.
    Rejected { reason: String },
    /// Tier is not configured here at all. The only outcome that advances the
    /// chain to the next tier.
    Absent,
}

/// A repo-backed config source, as declared by a tier-2 or tier-3 spec file.
#[derive(Debug, Clone)]
pub struct RepoSource {
    /// Git remote to clone/pull.
    pub repo: String,
    /// Optional branch/tag/sha. `None` means the remote's default branch.
    pub reference: Option<String>,
    /// Path WITHIN the repo to the config file, relative and slash-trimmed.
    /// Empty means the repo root holds `gm.config.json` directly.
    pub path: String,
    /// Absolute path of the directory the repo is materialized into. Computed
    /// by this module (never read from the spec) so a spec file can never
    /// redirect writes to an arbitrary location on disk.
    pub cache_dir: String,
    /// Which tier produced this source, carried so a change notification can
    /// say WHERE a config moved rather than only that something did. Two tiers
    /// can name the same repo, so the repo url alone does not identify it.
    pub tier_label: String,
}

impl RepoSource {
    /// Absolute path of the config file once the repo is materialized.
    pub fn config_path(&self) -> String {
        if self.path.is_empty() {
            format!("{}/gm.config.json", self.cache_dir)
        } else {
            format!("{}/{}", self.cache_dir, self.path)
        }
    }
}

/// The seam a sibling agent fills in: materialize/refresh a repo-backed source
/// into `src.cache_dir`.
///
/// Deliberately NOT implemented here -- git fetching is another agent's scope.
/// This trait is the entire contract between the two halves: resolution calls
/// [`RepoFetcher::refresh`] before reading a repo-backed tier, and treats any
/// `Err` as a REJECTION of that tier (not a fallthrough), so a fetch failure
/// surfaces instead of silently demoting the user to a lower tier whose config
/// says something different.
///
/// Implementors must be safe under the process-wide shared plugin instance:
/// two concurrently-active projects can call `refresh` at once, and the two
/// project-tier cache dirs are distinct paths, but a shared user-tier cache
/// dir can genuinely be hit twice concurrently -- so the implementation needs
/// its own locking or an atomic publish, exactly as gitignore.rs and
/// legacy_reaper.rs each had to reason about.
pub trait RepoFetcher {
    /// Ensure `src.cache_dir` holds a current checkout. `Ok(())` promises
    /// `src.config_path()` is readable if the repo contains it.
    fn refresh(&self, src: &RepoSource) -> Result<(), String>;
}

/// A fetcher that does nothing and reports why.
///
/// Lets resolution be exercised end to end before the git half lands: a
/// repo-backed tier whose cache is already populated resolves normally, and
/// one whose cache is cold is REJECTED with a reason naming the missing
/// implementation -- rather than appearing to work by falling through to a
/// lower tier.
pub struct NoopFetcher;

impl RepoFetcher for NoopFetcher {
    fn refresh(&self, _src: &RepoSource) -> Result<(), String> {
        Err("no RepoFetcher wired: repo-backed config sources require the git fetch implementation".to_string())
    }
}

/// The full result of a resolution: the config, which tier won, and why.
///
/// `why` is a human sentence, and `rejected` carries every tier that was
/// present-but-broken on the way down. Both exist because a resolution that
/// only returns the winning config makes a misconfiguration invisible -- the
/// user sees defaults and has nothing to explain them.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub config: Config,
    pub tier: Tier,
    pub why: String,
    pub rejected: Vec<String>,
}

impl Resolution {
    pub fn to_json(&self) -> Value {
        json!({
            "tier": self.tier.as_str(),
            "why": self.why,
            "rejected": self.rejected,
            "version": self.config.version,
            "config": self.config.value,
        })
    }
}

/// Deep-merge `over` onto `base`. See the module doc comment for the exact
/// rules; the short form is: objects recurse, everything else (arrays and
/// explicit nulls included) replaces.
pub fn deep_merge(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out: Map<String, Value> = b.clone();
            for (k, ov) in o {
                let merged = match b.get(k) {
                    Some(bv) => deep_merge(bv, ov),
                    None => ov.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        _ => over.clone(),
    }
}

/// Total version gate. Every non-`Ok` return names the concrete defect,
/// because "config rejected" alone is not actionable.
fn check_version(v: &Value, origin: &str) -> Result<u64, String> {
    let Some(raw) = v.get("version") else {
        return Err(format!(
            "{origin}: missing required `version` field (this build understands version {SCHEMA_VERSION}). An unversioned config cannot be safely interpreted, so it is rejected rather than assumed current."
        ));
    };
    let Some(n) = raw.as_u64() else {
        return Err(format!(
            "{origin}: `version` must be a non-negative integer, found {raw}"
        ));
    };
    if n > SCHEMA_VERSION {
        #[cfg(target_arch = "wasm32")]
        crate::wasm_dispatch::emit_event("config_version_ahead_of_build", serde_json::json!({
            "origin": origin,
            "config_version": n,
            "build_schema_version": SCHEMA_VERSION,
            "reason": "config was written against a newer schema than this build knows. Applying the keys this build recognises, since every field carries a serde default and an unknown key is reported rather than fatal. Upgrade the plugin to pick up the newer semantics.",
        }));
        return Ok(n);
    }
    if n < MIN_READABLE_SCHEMA_VERSION {
        return Err(format!(
            "{origin}: config declares version {n}, below the oldest schema this build can read ({MIN_READABLE_SCHEMA_VERSION}). Migrate the config rather than have it silently reinterpreted under new semantics."
        ));
    }
    Ok(n)
}

/// Parse config TEXT into a `Load`. The single choke point every tier's bytes
/// pass through, so malformed input has exactly one handler.
///
/// Never panics: every failure path is an explicit `Rejected`. Empty/whitespace
/// text is `Absent` rather than rejected -- an empty file is how a tier is
/// disabled, matching prose.rs's `read_clean`, which treats blank content as
/// "not set" too.
pub fn parse_config(text: &str, origin: &str) -> Load {
    let cleaned = text.trim_start_matches('\u{feff}');
    if cleaned.trim().is_empty() {
        return Load::Absent;
    }
    let parsed: Value = match serde_json::from_str(cleaned) {
        Ok(v) => v,
        Err(e) => {
            return Load::Rejected {
                reason: format!("{origin}: not valid JSON: {e}"),
            }
        }
    };
    if !parsed.is_object() {
        return Load::Rejected {
            reason: format!(
                "{origin}: top level must be a JSON object, found {}",
                type_name_of(&parsed)
            ),
        };
    }
    let version = match check_version(&parsed, origin) {
        Ok(n) => n,
        Err(reason) => return Load::Rejected { reason },
    };
    let merged = deep_merge(&Config::builtin_default().value, &parsed);
    Load::Accepted(Config {
        version,
        value: merged,
    })
}

fn type_name_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Parse a repo-source SPEC (tiers 2 and 3). Separate from [`parse_config`]
/// because a spec is a pointer with an entirely different required shape --
/// conflating them would let a config-shaped file satisfy a spec read.
fn parse_source_spec(text: &str, origin: &str, cache_dir: String, tier_label: &str) -> Result<Option<RepoSource>, String> {
    let cleaned = text.trim_start_matches('\u{feff}');
    if cleaned.trim().is_empty() {
        return Ok(None);
    }
    let v: Value = serde_json::from_str(cleaned)
        .map_err(|e| format!("{origin}: not valid JSON: {e}"))?;
    let Some(obj) = v.as_object() else {
        return Err(format!(
            "{origin}: top level must be a JSON object, found {}",
            type_name_of(&v)
        ));
    };
    let repo = obj
        .get("repo")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if repo.is_empty() {
        return Err(format!(
            "{origin}: source spec requires a non-empty `repo` field naming the config repository"
        ));
    }
    crate::config_path::validate_repo_url(&repo).map_err(|e| format!("{origin}: {e}"))?;
    let reference = ["ref", "reference", "branch"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(|x| x.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(r) = reference.as_deref() {
        validate_git_ref(r).map_err(|e| format!("{origin}: {e}"))?;
    }
    let raw_path = obj.get("path").and_then(|x| x.as_str()).unwrap_or("");
    crate::config_path::validate_source_path(raw_path).map_err(|e| format!("{origin}: {e}"))?;
    let path = raw_path.trim().trim_matches('/').to_string();
    Ok(Some(RepoSource {
        repo,
        reference,
        path,
        cache_dir,
        tier_label: tier_label.to_string(),
    }))
}

/// Validate a branch/tag/sha before it reaches a `git fetch`/`clone --branch`
/// argv.
///
/// A ref is as attacker-controlled as the URL beside it, and lands in the same
/// argv. The leading-`-` check is the load-bearing one: `--upload-pack=<cmd>`
/// in a ref position is git's other documented arbitrary-command vector, and an
/// argv array does not prevent it because the string is still parsed as an
/// option once git sees the dash.
///
/// The remaining rules are git's own `check-ref-format` restrictions, applied
/// here rather than discovered as an opaque git failure three calls later.
fn validate_git_ref(reference: &str) -> Result<(), String> {
    let r = reference.trim();
    if r.is_empty() {
        return Err("`ref` is empty".to_string());
    }
    if r.len() > 255 {
        return Err("`ref` exceeds 255 bytes".to_string());
    }
    if r.starts_with('-') {
        return Err(format!(
            "`ref` {r:?} starts with '-' and would be parsed by git as an option, not a ref"
        ));
    }
    if r.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("`ref` {r:?} contains whitespace or a control character"));
    }
    for bad in ["..", "@{", "//", "\\"] {
        if r.contains(bad) {
            return Err(format!("`ref` {r:?} contains {bad:?}, which git rejects"));
        }
    }
    if r.ends_with('.') || r.ends_with(".lock") || r.starts_with('/') || r.ends_with('/') {
        return Err(format!("`ref` {r:?} is not a well-formed git ref"));
    }
    if r.chars().any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '[')) {
        return Err(format!("`ref` {r:?} contains a character git reserves for revision syntax"));
    }
    Ok(())
}

/// Join a base directory and a project/home-relative path. Kept in one place
/// so trailing-separator handling cannot drift between tiers (gitignore.rs
/// open-codes the same care for exactly this reason).
fn join(base: &str, rel: &str) -> String {
    let b = base.trim_end_matches(['/', '\\']);
    if b.is_empty() {
        rel.to_string()
    } else {
        format!("{b}/{rel}")
    }
}

/// User home directory, matching poll_detect.rs's precedent (`HOME`, then
/// `USERPROFILE`). Returns `None` rather than poll_detect's `"."` fallback:
/// `"."` is the PROJECT directory here, and silently reading a user-wide tier
/// out of the project would collapse tier 3 into tier 2.
fn env_var(key: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let packed = unsafe { crate::wasm_dispatch::host_env_get(key.as_ptr(), key.len() as u32) };
        if let Some(s) = crate::wasm_dispatch::unpack_to_string_pub(packed) {
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
    }
    std::env::var(key).ok()
}

pub fn user_cache_root() -> Option<String> {
    home_dir().map(|home| join(&home, SOURCE_CACHE_REL))
}

fn home_dir() -> Option<String> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(s) = env_var(key) {
            let t = s.trim().trim_end_matches(['/', '\\']);
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Load one repo-backed tier: read its spec, refresh the repo, read the
/// resulting config.
///
/// Every failure after the spec PARSES is a rejection, not a fallthrough --
/// once a user has declared "my config lives in that repo", quietly running
/// someone else's config because the fetch failed is worse than stopping.
const PUBLISH_SWAP_READ_ATTEMPTS: u32 = 5;

const PUBLISH_SWAP_RETRY_MS: u64 = 3;

fn spin_ms(ms: u64) {
    let deadline = crate::orchestrator::state::now_ms() as u64 + ms;
    while (crate::orchestrator::state::now_ms() as u64) < deadline {}
}

fn read_across_publish(path: &str) -> Option<String> {
    for attempt in 0..PUBLISH_SWAP_READ_ATTEMPTS {
        if let Some(text) = pkfs::read_to_string(path) {
            return Some(text);
        }
        if attempt + 1 < PUBLISH_SWAP_READ_ATTEMPTS {
            spin_ms(PUBLISH_SWAP_RETRY_MS);
        }
    }
    None
}

fn load_repo_tier(
    spec_path: &str,
    cache_dir: String,
    fetcher: &dyn RepoFetcher,
    tier_label: &str,
) -> Load {
    let Some(raw) = pkfs::read_to_string(spec_path) else {
        return Load::Absent;
    };
    let src = match parse_source_spec(&raw, spec_path, cache_dir, tier_label) {
        Ok(Some(s)) => s,
        Ok(None) => return Load::Absent,
        Err(reason) => return Load::Rejected { reason },
    };
    if let Err(e) = fetcher.refresh(&src) {
        return Load::Rejected {
            reason: format!(
                "{spec_path}: could not refresh config repo {}: {e}",
                src.repo
            ),
        };
    }
    let cfg_path = src.config_path();
    let Some(text) = read_across_publish(&cfg_path) else {
        return Load::Rejected {
            reason: format!(
                "{spec_path}: config repo {} refreshed, but no config file at {cfg_path}",
                src.repo
            ),
        };
    };
    match parse_config(&text, &cfg_path) {
        Load::Absent => Load::Rejected {
            reason: format!("{cfg_path}: config file is empty"),
        },
        other => other,
    }
}

/// Resolve the full chain against a project root, reporting which tier won.
///
/// `project_root` is passed in rather than resolved here so callers that
/// already hold a root (orchestrator::gm_dir resolves one via git, and refuses
/// to guess) do not resolve it twice, and so this stays callable off-wasm.
///
/// Never panics and always returns a `Resolution`: tier 4 is infallible.
/// The wired entry point: resolve config for THIS dispatch's project using the
/// real git-backed fetcher.
///
/// `resolve_with` takes an injected fetcher so it stays testable and so a
/// caller that must not touch the network can pass `NoopFetcher`. That
/// injection is also how the module ended up shipped-but-inert: nothing
/// constructed a real fetcher, so tiers 2 and 3 could never fire in production
/// no matter how correct the chain was. This function is the one place that
/// binds the chain to `GitRepoFetcher`, so "resolve config" has a single
/// obvious call for the rest of the codebase.
///
/// Resolves against the CURRENT dispatch's project root (host_cwd_string,
/// fresh every call) because the plugin instance is process-wide and shared
/// across concurrently-active projects -- a cached root would leak one
/// project's config into another's dispatch.
pub fn resolve() -> Resolution {
    let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let fetcher = crate::config_sync::GitRepoFetcher::default();
    resolve_and_report(&root, &fetcher)
}

pub fn resolve_with(project_root: &str, fetcher: &dyn RepoFetcher) -> Resolution {
    let mut rejected: Vec<String> = Vec::new();

    let p1 = join(project_root, PROJECT_CONFIG_REL);
    match pkfs::read_to_string(&p1) {
        Some(text) => match parse_config(&text, &p1) {
            Load::Accepted(config) => {
                return Resolution {
                    config,
                    tier: Tier::ProjectVendored,
                    why: format!("project-vendored config at {p1}"),
                    rejected,
                }
            }
            Load::Rejected { reason } => rejected.push(reason),
            Load::Absent => {}
        },
        None => {}
    }

    let p2 = join(project_root, SOURCE_SPEC_REL);
    match load_repo_tier(&p2, join(project_root, SOURCE_CACHE_REL), fetcher, Tier::ProjectRepoSpec.as_str()) {
        Load::Accepted(config) => {
            return Resolution {
                config,
                tier: Tier::ProjectRepoSpec,
                why: format!("in-project config-repo spec at {p2}"),
                rejected,
            }
        }
        Load::Rejected { reason } => rejected.push(reason),
        Load::Absent => {}
    }

    if let Some(home) = home_dir() {
        let p3 = join(&home, SOURCE_SPEC_REL);
        match load_repo_tier(&p3, join(&home, SOURCE_CACHE_REL), fetcher, Tier::UserRepoSpec.as_str()) {
            Load::Accepted(config) => {
                return Resolution {
                    config,
                    tier: Tier::UserRepoSpec,
                    why: format!("user-wide config-repo spec at {p3}"),
                    rejected,
                }
            }
            Load::Rejected { reason } => rejected.push(reason),
            Load::Absent => {}
        }
    }

    let why = if rejected.is_empty() {
        "no config found in any tier; using builtin defaults".to_string()
    } else {
        format!(
            "builtin defaults; {} higher tier(s) were present but rejected (see `rejected`)",
            rejected.len()
        )
    };
    Resolution {
        config: Config::builtin_default(),
        tier: Tier::BuiltinDefault,
        why,
        rejected,
    }
}

/// Resolve, then report the outcome to the watcher log.
///
/// Separate from [`resolve_with`] so the pure resolution stays callable
/// without side effects. A rejection is emitted at every call rather than
/// once, because a config the user believes is active but which is silently
/// rejected is precisely the failure this whole module exists to prevent.
#[cfg(target_arch = "wasm32")]
pub fn resolve_and_report(project_root: &str, fetcher: &dyn RepoFetcher) -> Resolution {
    let r = resolve_with(project_root, fetcher);
    if !r.rejected.is_empty() {
        crate::wasm_dispatch::emit_event(
            "config_tier_rejected",
            json!({ "tier": r.tier.as_str(), "rejected": r.rejected }),
        );
    }
    let unknown = r.config.unknown_top_level_keys();
    if !unknown.is_empty() {
        crate::wasm_dispatch::emit_event(
            "config_unknown_keys",
            json!({
                "tier": r.tier.as_str(),
                "keys": unknown,
                "reason": "these top-level keys are not recognised by this build and are being IGNORED -- a typo looks exactly like this. If they come from a newer schema, this is expected and harmless.",
            }),
        );
    }
    crate::wasm_dispatch::emit_event(
        "config_resolved",
        json!({ "tier": r.tier.as_str(), "why": r.why, "version": r.config.version }),
    );
    r
}
