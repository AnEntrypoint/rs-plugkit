//! Path and URL sanitization for every untrusted string the config chain
//! interpolates into a filesystem path or hands to `git`.
//!
//! Three of the chain's inputs are attacker-controlled in the exact sense that
//! matters -- they arrive from a file the project did not necessarily author:
//!
//! - a **prose key**, which `prose::resolve` interpolates into
//!   `.gm/instructions/{key}.md`. Keys look compile-time-fixed but are not:
//!   `instructions::get_instruction` reads them from `fsm::graph()` state
//!   `prose_key` values, and `graph.json` is itself a vendorable artifact the
//!   `fsm-vendor` verb writes and invites operators to edit.
//! - a **source-spec `path`**, joined into the instruction/config cache dir.
//!   Its only prior treatment was `trim_matches('/')`, which leaves `..`
//!   entirely intact.
//! - a **repo URL**, passed straight through to `git clone`/`git ls-remote`.
//!   `git` treats some transports as instructions rather than locations:
//!   `ext::` runs an arbitrary shell command by design.
//!
//! # Why allowlists, not `..`-stripping
//!
//! Removing `..` from a component is the classic wrong fix: `....//` collapses
//! back to `../` under a single-pass strip, and a percent- or
//! backslash-encoded separator sidesteps a check written against `/`. Every
//! function here instead states the small set of shapes it ACCEPTS and refuses
//! everything else, so a novel encoding fails closed rather than sneaking
//! through a blocklist nobody thought to extend.
//!
//! # Why refuse rather than sanitize-and-continue
//!
//! A rejected key/path/URL returns `Err` carrying the reason, and the caller
//! falls through to the next tier with the reason recorded. Silently rewriting
//! a hostile path into a benign one would serve prose from a location the
//! author did not name while reporting success -- the same silent-misresolution
//! failure the whole tiered chain exists to make impossible.

/// Longest accepted prose key or path component.
/// Not a security boundary on its own -- the character allowlist is -- but a
/// path a filesystem cannot represent produces an IO error indistinguishable
/// from "absent", so bounding the length keeps a nonsense key reporting as a
/// nonsense key.
const MAX_COMPONENT_LEN: usize = 128;

/// Longest accepted multi-component relative path.
const MAX_PATH_LEN: usize = 512;

/// Git transports a config repo may name.
/// `https`/`http` fetch over a URL and cannot name a local executable.
/// `ssh://` and `git@host:path` are the shapes real private config repos use,
/// and neither carries a command payload -- the command a git-over-ssh session
/// runs is chosen by the server, not the URL.
/// Deliberately ABSENT, each for a concrete reason:
/// - `ext::` -- documented by git as running an arbitrary command. A config
///   file naming a repo would become a config file naming a program to run.
/// - `file://` and bare local paths -- a repo-backed tier exists to fetch from
///   elsewhere; pointing it at the local disk lets a vendored spec read a
///   directory the sandbox was never meant to expose, and offers nothing a
///   tier-1 vendored config does not already do better.
/// - `ftp`/`ftps` -- deprecated by git, unauthenticated, and no config repo
///   needs them.
const ALLOWED_URL_SCHEMES: &[&str] = &["https://", "http://", "ssh://", "git://"];

/// Reject any component that is empty, a traversal step, a Windows drive/UNC
/// artifact, or contains a separator or control character.
/// `.` and `..` are refused rather than normalized away: a caller asking for
/// `..` wants a different directory, and quietly serving it the current one
/// hides the request instead of answering it.
fn check_component(component: &str, what: &str) -> Result<(), String> {
    if component.is_empty() {
        return Err(format!("{what}: empty path component"));
    }
    if component.len() > MAX_COMPONENT_LEN {
        return Err(format!(
            "{what}: path component {:?} exceeds {MAX_COMPONENT_LEN} bytes",
            &component[..component.len().min(32)]
        ));
    }
    if component == "." || component == ".." {
        return Err(format!(
            "{what}: path component {:?} would traverse outside the cache directory"
        , component));
    }
    for c in component.chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if !ok {
            return Err(format!(
                "{what}: path component {:?} contains {:?}, which is not one of [A-Za-z0-9._-]",
                component, c
            ));
        }
    }
    Ok(())
}

/// Validate a prose key for interpolation into `<base>/<key>.md`.
/// Keys are legitimately hierarchical -- `residual/prd-open`, `gates/dirty-tree`
/// -- so `/` is a permitted SEPARATOR while being forbidden inside any
/// component. That distinction is the whole point: it keeps the nesting real
/// call sites depend on while making `../` unrepresentable.
/// Backslash is refused outright rather than treated as a separator. On Windows
/// the OS accepts it as one, so allowing it would create a second syntax for
/// the same traversal that a `/`-oriented check would miss; every in-tree key
/// uses `/`, so nothing legitimate is lost.
pub fn validate_prose_key(key: &str) -> Result<(), String> {
    let what = "prose key";
    if key.is_empty() {
        return Err(format!("{what}: empty"));
    }
    if key.len() > MAX_PATH_LEN {
        return Err(format!("{what}: exceeds {MAX_PATH_LEN} bytes"));
    }
    if key.contains('\\') {
        return Err(format!(
            "{what}: {:?} contains a backslash; use '/' for hierarchical keys",
            key
        ));
    }
    if key.contains('\0') {
        return Err(format!("{what}: contains a NUL byte"));
    }
    if key.starts_with('/') {
        return Err(format!(
            "{what}: {:?} is absolute; keys are relative to the instructions directory",
            key
        ));
    }
    for component in key.split('/') {
        check_component(component, what)?;
    }
    Ok(())
}

/// Validate the `path` field of a repo-source spec: the subdirectory WITHIN a
/// materialized config/instructions repo that holds the artifacts.
/// Accepts the empty string (meaning "the repo root"), which is how a spec that
/// omits the field is already read. Everything else must be a relative path of
/// safe components -- the same rule as a prose key, because both are joined
/// into a cache directory the spec must never be able to escape.
pub fn validate_source_path(path: &str) -> Result<(), String> {
    let what = "source spec `path`";
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(());
    }
    if trimmed.len() > MAX_PATH_LEN {
        return Err(format!("{what}: exceeds {MAX_PATH_LEN} bytes"));
    }
    if trimmed.contains('\\') {
        return Err(format!(
            "{what}: {:?} contains a backslash; use '/' as the separator",
            trimmed
        ));
    }
    if trimmed.contains('\0') {
        return Err(format!("{what}: contains a NUL byte"));
    }
    for component in trimmed.split('/') {
        check_component(component, what)?;
    }
    Ok(())
}

fn normalize_lexically(path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in path.replace('\\', "/").split('/') {
        match raw {
            "" | "." => {}
            ".." => match out.last().map(String::as_str) {
                Some("..") | None => out.push("..".to_string()),
                _ => {
                    out.pop();
                }
            },
            other => out.push(other.to_string()),
        }
    }
    out
}

pub fn path_contained_within(root: &str, candidate: &str) -> bool {
    let c = candidate.replace('\\', "/");
    if c.starts_with('/') || c.starts_with("//") {
        return false;
    }
    if c.chars().nth(1) == Some(':') {
        return false;
    }
    let root_parts = normalize_lexically(root);
    let cand_parts = normalize_lexically(candidate);
    if cand_parts.iter().any(|p| p == "..") {
        return false;
    }
    cand_parts.len() >= root_parts.len()
        && root_parts
            .iter()
            .zip(cand_parts.iter())
            .all(|(r, c)| r == c)
}

/// Validate a git remote URL before any argv carrying it reaches the host.
/// Two accepted shapes: a scheme from [`ALLOWED_URL_SCHEMES`], or git's
/// `user@host:path` scp-like syntax. A leading `-` is refused separately from
/// both, because git would read it as an OPTION rather than a URL -- the
/// argv-injection variant of this bug, and one an argv array does not prevent
/// on its own since the string still lands in an option position.
pub fn validate_repo_url(url: &str) -> Result<(), String> {
    let what = "config repo url";
    let u = url.trim();
    if u.is_empty() {
        return Err(format!("{what}: empty"));
    }
    if u.len() > 2048 {
        return Err(format!("{what}: exceeds 2048 bytes"));
    }
    if u.starts_with('-') {
        return Err(format!(
            "{what}: {:?} starts with '-' and would be parsed by git as an option, not a location",
            u
        ));
    }
    if u.chars().any(|c| c.is_control()) {
        return Err(format!("{what}: contains a control character"));
    }
    if u.contains(char::is_whitespace) {
        return Err(format!(
            "{what}: {:?} contains whitespace; a git remote URL never does",
            u
        ));
    }
    let lower = u.to_ascii_lowercase();
    if ALLOWED_URL_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return Ok(());
    }
    if is_scp_like(u) {
        return Ok(());
    }
    Err(format!(
        "{what}: {:?} does not use an allowed transport. Permitted: {} or git's user@host:path form. \
         Local paths and file:// are refused because a repo-backed tier exists to fetch from \
         elsewhere, and ext:// is refused because git executes it as a command rather than \
         fetching from it.",
        u,
        ALLOWED_URL_SCHEMES.join(", ")
    ))
}

/// Transports the `fetch` verb may name.
/// Narrower than [`ALLOWED_URL_SCHEMES`] on purpose. That list serves `git`,
/// which legitimately speaks `ssh://` and `git://`; `host_fetch` is an HTTP
/// client and can do nothing with either, so admitting them would widen the
/// accepted surface without enabling a single real call.
/// Deliberately ABSENT, each for a concrete reason:
/// - `file://` -- the host's fetch implementation would read local disk through
///   a verb whose whole contract is "reach the network", bypassing the
///   `path_within_project` containment every fs_* verb applies.
/// - `data:` and `blob:` -- carry their payload inline, so a caller that can
///   name one can hand the host arbitrary bytes to interpret as a response.
/// - schemeless input (`example.com/x`, `//example.com/x`) -- resolution is
///   left to the host, and a bare `/etc/passwd` or a UNC `\\host\share` reads
///   as a local location under some resolvers.
const ALLOWED_FETCH_SCHEMES: &[&str] = &["https://", "http://"];

/// Longest accepted fetch URL.
const MAX_FETCH_URL_LEN: usize = 2048;

/// Validate a URL before it reaches `host_fetch`.
/// The scheme allowlist is the substantive check; the control-character,
/// whitespace and length rules exist because `host_fetch` hands the string to a
/// URL parser and then to an HTTP client, and a newline inside a URL is the
/// classic request-splitting primitive.
/// A host is required to be present and non-empty so that `https://` alone, or
/// `https:///etc/passwd` (empty authority, which several parsers read as a
/// local path), is refused rather than passed to the host to interpret.
pub fn validate_fetch_url(url: &str) -> Result<(), String> {
    let what = "fetch url";
    let u = url.trim();
    if u.is_empty() {
        return Err(format!("{what}: empty"));
    }
    if u.len() > MAX_FETCH_URL_LEN {
        return Err(format!("{what}: exceeds {MAX_FETCH_URL_LEN} bytes"));
    }
    if u.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{what}: contains a control character; a newline inside a URL is a request-splitting primitive"
        ));
    }
    if u.contains(char::is_whitespace) {
        return Err(format!("{what}: {:?} contains whitespace", u));
    }
    let lower = u.to_ascii_lowercase();
    let Some(scheme) = ALLOWED_FETCH_SCHEMES.iter().find(|s| lower.starts_with(**s)) else {
        return Err(format!(
            "{what}: {:?} does not use an allowed transport. Permitted: {}. \
             file:// and data: are refused because fetch exists to reach the network, \
             and a schemeless URL is refused because its resolution is left to the host.",
            u,
            ALLOWED_FETCH_SCHEMES.join(", ")
        ));
    };
    let authority = &u[scheme.len()..];
    let host_end = authority
        .find(['/', '?', '#'])
        .unwrap_or(authority.len());
    let host = &authority[..host_end];
    let host = host.rsplit('@').next().unwrap_or(host);
    if host.is_empty() {
        return Err(format!(
            "{what}: {:?} names no host; an empty authority is read as a local path by some parsers",
            u
        ));
    }
    Ok(())
}

/// Recognise git's scp-like `[user@]host:path` remote syntax.
/// Requires a `:` that is not part of a scheme (no `//` follows it) and a
/// non-empty host and path. A Windows drive letter (`C:\repo`) is excluded by
/// the single-character-host check, so a local path cannot masquerade as an
/// scp-like remote.
fn is_scp_like(u: &str) -> bool {
    let Some(colon) = u.find(':') else {
        return false;
    };
    let (host_part, rest) = u.split_at(colon);
    let path = &rest[1..];
    if host_part.is_empty() || path.is_empty() || path.starts_with('/') {
        return false;
    }
    let host = host_part.rsplit('@').next().unwrap_or(host_part);
    if host.len() < 2 || !host.contains('.') {
        return false;
    }
    host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && !host.starts_with('.')
        && !host.starts_with('-')
}
