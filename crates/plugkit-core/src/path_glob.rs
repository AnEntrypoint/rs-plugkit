use globset::{GlobBuilder, GlobMatcher};

pub const PATH_GLOB_SYNTAX: &str = "* ? ** [abc] [!abc] {a,b}";

pub struct PathGlob {
    matcher: GlobMatcher,
    bare_name_pattern: bool,
}

impl PathGlob {
    pub fn parse(pattern: &str) -> Result<PathGlob, String> {
        Self::parse_with_case(pattern, true)
    }

    pub fn parse_with_case(pattern: &str, case_insensitive: bool) -> Result<PathGlob, String> {
        let normalized = pattern.trim().replace('\\', "/");
        let normalized = normalized.trim_start_matches("./");
        if normalized.is_empty() {
            return Err(format!(
                "glob \"{pattern}\" is empty -- supported syntax: {PATH_GLOB_SYNTAX}"
            ));
        }
        let glob = GlobBuilder::new(normalized)
            .case_insensitive(case_insensitive)
            .literal_separator(false)
            .empty_alternates(true)
            .build()
            .map_err(|e| format!("glob \"{pattern}\" is not a valid glob ({e}) -- supported syntax: {PATH_GLOB_SYNTAX}"))?;
        Ok(PathGlob {
            matcher: glob.compile_matcher(),
            bare_name_pattern: !normalized.contains('/'),
        })
    }

    pub fn admits(&self, root: &str, scope: Option<&str>, path: &str) -> bool {
        let relative = root_relative(root, path);
        if self.matcher.is_match(relative) {
            return true;
        }
        if let Some(below_scope) = scope.and_then(|s| scope_relative(s, relative)) {
            if self.matcher.is_match(below_scope) {
                return true;
            }
        }
        self.bare_name_pattern
            && self
                .matcher
                .is_match(relative.rsplit('/').next().unwrap_or(relative))
    }
}

fn scope_relative<'a>(scope: &str, relative: &'a str) -> Option<&'a str> {
    let normalized = scope.replace('\\', "/");
    let prefix = normalized.trim_start_matches("./").trim_matches('/');
    if prefix.is_empty() || prefix == "." {
        return None;
    }
    relative
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
}

pub fn looks_like_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

/// Glob alternatives that can match no file under `scopes`: each alternative's literal leading
/// directory must lie inside a scope or contain one. A brace group in the first path segment is
/// expanded into its alternatives; a leading wildcard keeps an alternative reachable.
pub fn alternatives_outside_scopes(patterns: &[String], scopes: &[&str]) -> Vec<String> {
    let scopes: Vec<String> = scopes
        .iter()
        .map(|scope| scope_prefix(scope))
        .filter(|scope| !scope.is_empty())
        .collect();
    if scopes.is_empty() {
        return Vec::new();
    }
    patterns
        .iter()
        .flat_map(|pattern| expand_first_segment(&normalize_pattern(pattern)))
        .filter(|alternative| {
            let directory = literal_directory(alternative);
            !scopes.iter().any(|scope| {
                directory.is_empty()
                    || directory == *scope
                    || directory.starts_with(&format!("{scope}/"))
                    || scope.starts_with(&format!("{directory}/"))
            })
        })
        .collect()
}

fn normalize_pattern(pattern: &str) -> String {
    pattern.trim().replace('\\', "/").trim_start_matches("./").to_string()
}

fn scope_prefix(scope: &str) -> String {
    let normalized = scope.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./").trim_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.to_string()
    }
}

fn expand_first_segment(pattern: &str) -> Vec<String> {
    let first_end = pattern.find('/').unwrap_or(pattern.len());
    let (first, rest) = pattern.split_at(first_end);
    match (first.find('{'), first.find('}')) {
        (Some(open), Some(close)) if open < close => first[open + 1..close]
            .split(',')
            .map(|alternative| {
                format!("{}{}{}{}", &first[..open], alternative, &first[close + 1..], rest)
            })
            .collect(),
        _ => vec![pattern.to_string()],
    }
}

fn literal_directory(alternative: &str) -> String {
    let segments: Vec<&str> = alternative.split('/').collect();
    let mut literal: Vec<&str> = segments
        .iter()
        .copied()
        .take_while(|segment| !segment.contains(['*', '?', '[', '{']))
        .collect();
    if literal.len() == segments.len() {
        literal.pop();
    }
    literal.join("/")
}

pub(crate) fn root_relative<'a>(root: &str, path: &'a str) -> &'a str {
    let root = root.trim_end_matches('/');
    let under_root = match root {
        "" | "." => path,
        _ => path
            .strip_prefix(root)
            .map(|rest| rest.trim_start_matches('/'))
            .unwrap_or(path),
    };
    under_root.trim_start_matches("./")
}
