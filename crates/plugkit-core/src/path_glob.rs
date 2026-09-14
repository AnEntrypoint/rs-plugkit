use globset::{GlobBuilder, GlobMatcher};

pub const PATH_GLOB_SYNTAX: &str = "* ? ** [abc] [!abc] {a,b}";

pub struct PathGlob {
    matcher: GlobMatcher,
    bare_name_pattern: bool,
}

impl PathGlob {
    pub fn parse(pattern: &str) -> Result<PathGlob, String> {
        let normalized = pattern.trim().replace('\\', "/");
        let normalized = normalized.trim_start_matches("./");
        if normalized.is_empty() {
            return Err(format!("glob \"{pattern}\" is empty -- supported syntax: {PATH_GLOB_SYNTAX}"));
        }
        let glob = GlobBuilder::new(normalized)
            .case_insensitive(true)
            .literal_separator(false)
            .empty_alternates(true)
            .build()
            .map_err(|e| format!("glob \"{pattern}\" is not a valid glob ({e}) -- supported syntax: {PATH_GLOB_SYNTAX}"))?;
        Ok(PathGlob { matcher: glob.compile_matcher(), bare_name_pattern: !normalized.contains('/') })
    }

    pub fn admits(&self, root: &str, path: &str) -> bool {
        let relative = root_relative(root, path);
        if self.matcher.is_match(relative) { return true; }
        self.bare_name_pattern && self.matcher.is_match(relative.rsplit('/').next().unwrap_or(relative))
    }
}

pub fn looks_like_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', '{'])
}

fn root_relative<'a>(root: &str, path: &'a str) -> &'a str {
    let root = root.trim_end_matches('/');
    let under_root = match root {
        "" | "." => path,
        _ => path.strip_prefix(root).map(|rest| rest.trim_start_matches('/')).unwrap_or(path),
    };
    under_root.trim_start_matches("./")
}
