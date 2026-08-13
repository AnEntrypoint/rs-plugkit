use serde_json::{json, Map, Value};

use crate::pkfs;

pub const SCHEMA_VERSION: u64 = 1;

pub const MIN_READABLE_SCHEMA_VERSION: u64 = 1;

pub const PROJECT_CONFIG_REL: &str = ".gm/gm.config.json";

pub const SOURCE_SPEC_REL: &str = ".gm/config.source.json";

pub const SOURCE_CACHE_REL: &str = ".gm/config-source-cache";

pub const DEFAULT_REPO_URL: &str = "https://github.com/AnEntrypoint/gm-config";

pub const DEFAULT_REPO_CACHE_REL: &str = ".gm/config-source-cache-default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    ProjectVendored,
    ProjectRepoSpec,
    UserRepoSpec,
    ImplicitDefaultRepo,
    BuiltinDefault,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::ProjectVendored => "project_vendored",
            Tier::ProjectRepoSpec => "project_repo_spec",
            Tier::UserRepoSpec => "user_repo_spec",
            Tier::ImplicitDefaultRepo => "implicit_default_repo",
            Tier::BuiltinDefault => "builtin_default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub version: u64,
    pub value: Value,
}

impl Config {
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

    pub fn unknown_top_level_keys(&self) -> Vec<String> {
        const KNOWN: &[&str] = &[
            "version", "instructions", "index", "memory", "memory_sync", "cache", "sync",
            "fsm", "messages", "rag", "scoring", "embed", "rssearch", "git_commits",
            "code_chunks", "code_index", "pipeline", "instruction_payload", "browser_witness",
            "discipline_note", "claim_audit", "db_path", "memory_md_tables", "retention",
        ];
        let Some(obj) = self.value.as_object() else { return Vec::new() };
        obj.keys()
            .filter(|k| !k.starts_with('_'))
            .filter(|k| !KNOWN.contains(&k.as_str()))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum Load {
    Accepted(Config),
    Rejected { reason: String },
    Absent,
}

#[derive(Debug, Clone)]
pub struct RepoSource {
    pub repo: String,
    pub reference: Option<String>,
    pub path: String,
    pub cache_dir: String,
    pub tier_label: String,
}

impl RepoSource {
    pub fn config_path(&self) -> String {
        if self.path.is_empty() {
            format!("{}/gm.config.json", self.cache_dir)
        } else {
            format!("{}/{}", self.cache_dir, self.path)
        }
    }
}

pub trait RepoFetcher {
    fn refresh(&self, src: &RepoSource) -> Result<(), String>;
}

pub fn resolve_prose_repo_source(
    spec_text: &str,
    spec_path: &str,
    cache_root: &str,
    tier_label: &str,
    fetcher: &dyn RepoFetcher,
) -> Result<RepoSource, String> {
    let cleaned = spec_text.trim_start_matches('\u{feff}');
    let v: Value = serde_json::from_str(cleaned)
        .map_err(|e| format!("{spec_path}: not valid JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{spec_path}: top level must be a JSON object"))?;
    let src = parse_source_entry(obj, spec_path, cache_root, tier_label)?;
    fetcher
        .refresh(&src)
        .map_err(|e| format!("{spec_path}: could not refresh config repo {} ({e})", src.repo))?;
    Ok(src)
}

pub struct NoopFetcher;

impl RepoFetcher for NoopFetcher {
    fn refresh(&self, _src: &RepoSource) -> Result<(), String> {
        Err("no RepoFetcher wired: repo-backed config sources require the git fetch implementation".to_string())
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub config: Config,
    pub tier: Tier,
    pub why: String,
    pub rejected: Vec<String>,
    /// Absolute directory the winning tier's repo checkout is materialized
    /// into, when the winning tier is repo-backed. `None` for
    /// `ProjectVendored` (no checkout, a single file) and `BuiltinDefault`
    /// (nothing fetched). A caller resolving a path RELATIVE to this
    /// resolution's config (e.g. `fsm.graph`, an instructions-source `path`)
    /// must join against this field rather than guess a tier's cache dir
    /// from a hardcoded constant -- three distinct constants
    /// (`SOURCE_CACHE_REL`, `DEFAULT_REPO_CACHE_REL`, `user_cache_root()`)
    /// name three different directories, and only the tier that actually won
    /// knows which one holds the checkout this `Resolution` was read from.
    pub cache_dir: Option<String>,
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

/// Parse ONE repo-source object (a single entry of a tier-2/3 spec, whether
/// the spec file held a bare object or one element of an array). `cache_root`
/// is the tier's cache root; the entry's own sub-directory beneath it is
/// derived from a content hash of (repo, reference, path) so N entries in one
/// spec never collide on disk, and reordering the array (same entries, new
/// priority) resolves to the SAME sub-directories rather than triggering a
/// needless re-clone.
fn parse_source_entry(obj: &Map<String, Value>, origin: &str, cache_root: &str, tier_label: &str) -> Result<RepoSource, String> {
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
    let entry_hash = crate::hash::fnv1a64(format!("{repo}|{}|{path}", reference.as_deref().unwrap_or("")).as_bytes());
    let cache_dir = format!("{cache_root}/{entry_hash:016x}");
    Ok(RepoSource {
        repo,
        reference,
        path,
        cache_dir,
        tier_label: tier_label.to_string(),
    })
}

/// Parse a repo-source SPEC (tiers 2 and 3): either a bare `{repo,...}`
/// object (the original single-source shape, treated as a 1-element list for
/// backward compatibility) or a JSON array of such objects, resolved and
/// deep-merged in array order (first entry's keys win, falling through to the
/// next entry, then to the tier below). Separate from [`parse_config`]
/// because a spec is a pointer with an entirely different required shape --
/// conflating them would let a config-shaped file satisfy a spec read.
fn parse_source_spec(text: &str, origin: &str, cache_root: &str, tier_label: &str) -> Result<Vec<RepoSource>, String> {
    let cleaned = text.trim_start_matches('\u{feff}');
    if cleaned.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(cleaned)
        .map_err(|e| format!("{origin}: not valid JSON: {e}"))?;
    match &v {
        Value::Object(obj) => Ok(vec![parse_source_entry(obj, origin, cache_root, tier_label)?]),
        Value::Array(items) => {
            if items.is_empty() {
                return Err(format!("{origin}: source spec array must not be empty"));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let obj = item.as_object().ok_or_else(|| {
                        format!("{origin}: array entry {i} must be a JSON object, found {}", type_name_of(item))
                    })?;
                    parse_source_entry(obj, &format!("{origin}[{i}]"), cache_root, tier_label)
                })
                .collect()
        }
        other => Err(format!(
            "{origin}: top level must be a JSON object or an array of objects, found {}",
            type_name_of(other)
        )),
    }
}

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

fn join(base: &str, rel: &str) -> String {
    let b = base.trim_end_matches(['/', '\\']);
    if b.is_empty() {
        rel.to_string()
    } else {
        format!("{b}/{rel}")
    }
}

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

fn load_one_repo_source(src: &RepoSource, spec_path: &str, fetcher: &dyn RepoFetcher) -> Load {
    let refresh_err = fetcher.refresh(src).err();
    let cfg_path = src.config_path();
    let Some(text) = read_across_publish(&cfg_path) else {
        return match refresh_err {
            Some(e) => Load::Rejected {
                reason: format!(
                    "{spec_path}: could not refresh config repo {} ({e}), and no cached config file at {cfg_path} to fall back on",
                    src.repo
                ),
            },
            None => Load::Rejected {
                reason: format!(
                    "{spec_path}: config repo {} refreshed, but no config file at {cfg_path}",
                    src.repo
                ),
            },
        };
    };
    match parse_config(&text, &cfg_path) {
        Load::Absent => Load::Rejected {
            reason: format!("{cfg_path}: config file is empty"),
        },
        other => other,
    }
}

/// Load a repo-backed tier that may declare MULTIPLE sources (an array in the
/// spec file): each is loaded independently, and a bad/unreachable entry never
/// blocks the OTHERS -- only every entry failing (or the spec being genuinely
/// empty/absent) degrades the whole tier to `Absent`/`Rejected`. Configs from
/// entries that DID load are deep-merged in array order: entry 0's keys win,
/// falling through entry-by-entry, matching the same first-non-empty-wins
/// semantics `resolve_with` already applies across tiers.
fn load_repo_tier(
    spec_path: &str,
    cache_root: String,
    fetcher: &dyn RepoFetcher,
    tier_label: &str,
) -> Load {
    let Some(raw) = pkfs::read_to_string(spec_path) else {
        return Load::Absent;
    };
    let sources = match parse_source_spec(&raw, spec_path, &cache_root, tier_label) {
        Ok(list) if list.is_empty() => return Load::Absent,
        Ok(list) => list,
        Err(reason) => return Load::Rejected { reason },
    };
    let mut merged: Option<Config> = None;
    let mut entry_failures: Vec<String> = Vec::new();
    for src in &sources {
        match load_one_repo_source(src, spec_path, fetcher) {
            Load::Accepted(cfg) => {
                merged = Some(match merged {
                    None => cfg,
                    Some(prior) => Config {
                        version: prior.version,
                        value: deep_merge(&cfg.value, &prior.value),
                    },
                });
            }
            Load::Rejected { reason } => entry_failures.push(reason),
            Load::Absent => entry_failures.push(format!("{}: no config file present", src.config_path())),
        }
    }
    match merged {
        Some(cfg) => Load::Accepted(cfg),
        None => Load::Rejected {
            reason: format!(
                "{spec_path}: every source in this tier failed to load ({} entries): {}",
                sources.len(),
                entry_failures.join("; ")
            ),
        },
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
///
/// Also resolves the config, then honors the resolved `sync.debounce_ms` for
/// the repo-tier fetch that produced it. The first pass necessarily runs on
/// the compiled default debounce (`GitRepoFetcher::default()`), since the
/// debounce setting itself lives inside the config being fetched -- a
/// chicken-and-egg only a second pass can resolve. The second pass is cheap:
/// `ensure_current`'s debounce state is a per-`RepoSource` file read plus an
/// mkdir-lock, not a network round trip, unless the debounce window has
/// actually elapsed.
///
/// Cached for a few seconds per project root (mirroring `ragconfig.rs`'s
/// `RagConfig::resolved()` cache). Several independent call sites
/// (`prose::resolve`, `RagConfig::resolved`, `fsm::graph`, the `config_resolve`
/// verb) each call this function fresh within one dispatch; without a cache,
/// a cold repo-tier source gets its own mkdir-lock acquisition attempted once
/// per call site in the same few milliseconds -- the LATER attempts see the
/// FIRST one's still-held lock and misreport "another process is cloning"
/// even though it is this same process's own earlier, still-in-flight call.
#[cfg(target_arch = "wasm32")]
const RESOLVE_CACHE_TTL_MS: u64 = 2_000;

#[cfg(target_arch = "wasm32")]
struct ResolveCacheEntry {
    ts_ms: u64,
    resolution: Resolution,
}

#[cfg(target_arch = "wasm32")]
static RESOLVE_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, ResolveCacheEntry>>> = std::sync::Mutex::new(None);

#[cfg(target_arch = "wasm32")]
pub fn resolve() -> Resolution {
    let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as u64;
    if let Ok(cache) = RESOLVE_CACHE.lock() {
        if let Some(entry) = cache.as_ref().and_then(|m| m.get(&root)) {
            if now_ms.saturating_sub(entry.ts_ms) < RESOLVE_CACHE_TTL_MS {
                return entry.resolution.clone();
            }
        }
    }
    let bootstrap_fetcher = crate::config_sync::GitRepoFetcher::default();
    let first_pass = resolve_and_report(&root, &bootstrap_fetcher);
    let configured_debounce_ms = first_pass
        .config
        .get("sync")
        .and_then(|s| s.get("debounce_ms"))
        .and_then(|v| v.as_u64());
    let resolution = match configured_debounce_ms {
        Some(ms) if ms != bootstrap_fetcher.debounce_ms => {
            let tuned_fetcher = crate::config_sync::GitRepoFetcher::with_debounce_ms(ms);
            resolve_and_report(&root, &tuned_fetcher)
        }
        _ => first_pass,
    };
    if let Ok(mut cache) = RESOLVE_CACHE.lock() {
        cache.get_or_insert_with(std::collections::HashMap::new)
            .insert(root, ResolveCacheEntry { ts_ms: now_ms, resolution: resolution.clone() });
    }
    resolution
}

/// Force an immediate remote-ref probe against every repo-backed tier this
/// project could resolve, bypassing the normal debounce entirely
/// (`GitRepoFetcher::with_debounce_ms(0)` -- `ensure_current`'s own debounce
/// check is `elapsed < required`, which a `required` of 0 can never satisfy).
/// For an agent that just pushed a change to its own config repo: this is the
/// on-demand refresh that lets that change apply THIS session instead of
/// waiting out `sync.debounce_ms` (default 15 minutes). Invalidates the
/// short-lived resolve cache so the very next plain `resolve()` call sees the
/// fresh result rather than a cached pre-refresh one.
#[cfg(target_arch = "wasm32")]
pub fn resolve_forced(project_root: &str) -> Resolution {
    let forced_fetcher = crate::config_sync::GitRepoFetcher::with_debounce_ms(0);
    let resolution = resolve_with(project_root, &forced_fetcher);
    if let Ok(mut cache) = RESOLVE_CACHE.lock() {
        if let Some(map) = cache.as_mut() {
            map.remove(project_root);
        }
    }
    resolution
}

pub fn resolve_with(project_root: &str, fetcher: &dyn RepoFetcher) -> Resolution {
    let mut rejected: Vec<String> = Vec::new();

    let p1 = join(project_root, PROJECT_CONFIG_REL);
    if let Some(text) = pkfs::read_to_string(&p1) {
        match parse_config(&text, &p1) {
            Load::Accepted(config) => {
                return Resolution {
                    config,
                    tier: Tier::ProjectVendored,
                    why: format!("project-vendored config at {p1}"),
                    rejected,
                    cache_dir: None,
                }
            }
            Load::Rejected { reason } => rejected.push(reason),
            Load::Absent => {}
        }
    }

    let p2_cache_dir = join(project_root, SOURCE_CACHE_REL);
    let p2 = join(project_root, SOURCE_SPEC_REL);
    match load_repo_tier(&p2, p2_cache_dir.clone(), fetcher, Tier::ProjectRepoSpec.as_str()) {
        Load::Accepted(config) => {
            return Resolution {
                config,
                tier: Tier::ProjectRepoSpec,
                why: format!("in-project config-repo spec at {p2}"),
                rejected,
                cache_dir: Some(p2_cache_dir),
            }
        }
        Load::Rejected { reason } => rejected.push(reason),
        Load::Absent => {}
    }

    if let Some(home) = home_dir() {
        let p3_cache_dir = join(&home, SOURCE_CACHE_REL);
        let p3 = join(&home, SOURCE_SPEC_REL);
        match load_repo_tier(&p3, p3_cache_dir.clone(), fetcher, Tier::UserRepoSpec.as_str()) {
            Load::Accepted(config) => {
                return Resolution {
                    config,
                    tier: Tier::UserRepoSpec,
                    why: format!("user-wide config-repo spec at {p3}"),
                    rejected,
                    cache_dir: Some(p3_cache_dir),
                }
            }
            Load::Rejected { reason } => rejected.push(reason),
            Load::Absent => {}
        }
    }

    let implicit_cache_dir = join(project_root, DEFAULT_REPO_CACHE_REL);
    match load_implicit_default_repo_tier(project_root, fetcher) {
        Load::Accepted(config) => {
            return Resolution {
                config,
                tier: Tier::ImplicitDefaultRepo,
                why: format!("gm's own shared default config repo at {DEFAULT_REPO_URL} (no project or user config.source.json configured)"),
                rejected,
                cache_dir: Some(implicit_cache_dir),
            }
        }
        Load::Rejected { reason } => rejected.push(reason),
        Load::Absent => {}
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
        cache_dir: None,
    }
}

fn load_implicit_default_repo_tier(project_root: &str, fetcher: &dyn RepoFetcher) -> Load {
    let src = RepoSource {
        repo: DEFAULT_REPO_URL.to_string(),
        reference: None,
        path: String::new(),
        cache_dir: join(project_root, DEFAULT_REPO_CACHE_REL),
        tier_label: Tier::ImplicitDefaultRepo.as_str().to_string(),
    };
    let _ = fetcher.refresh(&src);
    let cfg_path = src.config_path();
    let Some(text) = read_across_publish(&cfg_path) else {
        return Load::Absent;
    };
    match parse_config(&text, &cfg_path) {
        Load::Accepted(config) => Load::Accepted(config),
        Load::Rejected { reason } => Load::Rejected { reason },
        Load::Absent => Load::Absent,
    }
}

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
