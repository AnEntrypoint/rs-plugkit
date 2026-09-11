const MAX_COMPONENT_LEN: usize = 128;

const MAX_PATH_LEN: usize = 512;

const ALLOWED_URL_SCHEMES: &[&str] = &["https://", "http://", "ssh://", "git://"];

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

fn is_rooted(p: &str) -> bool {
    let s = p.replace('\\', "/");
    s.starts_with('/') || s.chars().nth(1) == Some(':')
}

pub fn path_contained_within(root: &str, candidate: &str) -> bool {
    let c = candidate.replace('\\', "/");
    if is_rooted(&c) && !is_rooted(root) {
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

const ALLOWED_FETCH_SCHEMES: &[&str] = &["https://", "http://"];

const MAX_FETCH_URL_LEN: usize = 2048;

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
