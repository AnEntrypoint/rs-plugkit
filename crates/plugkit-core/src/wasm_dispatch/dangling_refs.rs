use serde_json::{json, Value};
use std::collections::HashSet;
use super::host_abi::{git_call_argv, host_exists, host_read};
use super::verbs::ERR_CODE_DANGLING_REFERENCE;

const MAX_SCAN_BYTES: usize = 512 * 1024;
const MAX_SCAN_FILES: usize = 400;
const MAX_CANDIDATES: usize = 400;
const MAX_REFERENCES_PER_FILE: usize = 200;
const CONTEXT_TAIL_CHARS: usize = 64;

const SKIP_DIRECTORIES: &[&str] = &["node_modules", "dist", "vendor", "build", "coverage", ".git", ".gm", ".agentplug-kv"];
const SCANNABLE_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "cts", "json"];
const PROBE_EXTENSIONS: &[&str] = &["", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".json"];
const PROBE_INDEX_SUFFIXES: &[&str] = &["/index.js", "/index.mjs", "/index.cjs", "/index.ts", "/index.json"];
const EXTERNAL_SCHEMES: &[&str] = &["node:", "npm:", "data:", "http://", "https://", "file:", "bun:", "jsr:", "deno:", "#"];
const PROTECTED_EXCLUDE_PATHSPEC: &str = ":(top,exclude).agentplug*";
const WORKSPACE_PACKAGE_PARENT: &str = "packages";
const CONDITION_KEYS: &[&str] = &["import", "default", "require", "node", "browser"];

pub struct DanglingScan {
    pub offenders: Vec<Value>,
    pub waived: Vec<String>,
    pub scanned_files: usize,
}

pub fn scan_commit(cwd: Option<&str>, paths: &[String], add_all: bool, body: &Value) -> DanglingScan {
    let waivers = waivers_from_body(body);
    let root = repo_root(cwd);
    let ordered = commit_path_set(cwd, paths, add_all);
    let committed: HashSet<String> = ordered.iter().cloned().collect();
    let mut candidates: Vec<(String, String, usize, String)> = Vec::new();
    let mut scanned_files = 0usize;

    for path in ordered.iter() {
        if scanned_files >= MAX_SCAN_FILES || candidates.len() >= MAX_CANDIDATES { break; }
        if !is_scannable(path) { continue; }
        let Some(source) = read_text(&root, path) else { continue; };
        if source.len() > MAX_SCAN_BYTES { continue; }
        scanned_files += 1;
        let is_json = extension_of(path) == "json";
        for reference in extract_references(&source, is_json) {
            if candidates.len() >= MAX_CANDIDATES { break; }
            for target in resolve(&reference.specifier, &directory_of(path), &root) {
                if target == *path { continue; }
                if committed.contains(&target) { break; }
                if !file_present(&root, &target) { continue; }
                candidates.push((path.clone(), reference.specifier.clone(), reference.line, target));
                break;
            }
        }
    }

    let targets: Vec<String> = candidates.iter().map(|c| c.3.clone()).collect();
    let tracked = tracked_paths(cwd, &targets);
    let ignored = ignored_paths(cwd, &targets);
    let mut offenders = Vec::new();
    let mut waived = Vec::new();
    for (from, specifier, line, target) in candidates {
        if tracked.contains(&target) || ignored.contains(&target) { continue; }
        if waivers.waive_all || waivers.named.contains(&target) {
            waived.push(target.clone());
            continue;
        }
        offenders.push(json!({
            "from": from,
            "line": line,
            "specifier": specifier,
            "target": target,
        }));
    }
    offenders.sort_by(|a, b| a["from"].as_str().unwrap_or("").cmp(b["from"].as_str().unwrap_or(""))
        .then(a["line"].as_u64().unwrap_or(0).cmp(&b["line"].as_u64().unwrap_or(0))));
    DanglingScan { offenders, waived, scanned_files }
}

pub fn refusal_detail(verb: &str, scan: &DanglingScan) -> Value {
    let mut fixes: Vec<String> = Vec::new();
    for offender in &scan.offenders {
        let target = offender["target"].as_str().unwrap_or("");
        fixes.push(format!("add {} to this commit's paths, or commit it first", target));
    }
    fixes.dedup();
    json!({
        "error": format!("commit would reference {} file(s) that exist on disk but are untracked and are not part of this commit", scan.offenders.len()),
        "error_code": ERR_CODE_DANGLING_REFERENCE,
        "dangling_references": scan.offenders,
        "fixes": fixes,
        "scanned_files": scan.scanned_files,
        "waived": scan.waived,
        "allow_dangling_hint": "pass allow_dangling: [\"<target>\"] to waive a named target, or allow_dangling: true to waive all",
        "next_dispatch": verb,
    })
}

struct Waivers {
    waive_all: bool,
    named: Vec<String>,
}

fn waivers_from_body(body: &Value) -> Waivers {
    match body.get("allow_dangling") {
        Some(Value::Bool(true)) => Waivers { waive_all: true, named: Vec::new() },
        Some(Value::Array(items)) => Waivers {
            waive_all: false,
            named: items.iter().filter_map(|v| v.as_str()).map(normalize).collect(),
        },
        _ => Waivers { waive_all: false, named: Vec::new() },
    }
}

fn repo_root(cwd: Option<&str>) -> String {
    exec_git(&["rev-parse", "--show-toplevel"], cwd).trim().replace('\\', "/")
}

fn exec_git(argv: &[&str], cwd: Option<&str>) -> String {
    git_call_argv(argv, cwd).get("stdout").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn read_text(root: &str, relative: &str) -> Option<String> {
    if root.is_empty() { return host_read(relative); }
    host_read(&format!("{root}/{relative}")).or_else(|| host_read(relative))
}

fn file_present(root: &str, relative: &str) -> bool {
    if root.is_empty() { return host_exists(relative); }
    host_exists(&format!("{root}/{relative}")) || host_exists(relative)
}

fn commit_path_set(cwd: Option<&str>, paths: &[String], add_all: bool) -> Vec<String> {
    let mut set: Vec<String> = split_nul(&exec_git(&["diff", "--cached", "--name-only", "-z"], cwd));
    if !add_all && paths.is_empty() { return set; }
    let mut argv: Vec<String> = vec![
        "status".to_string(), "--porcelain".to_string(), "-z".to_string(), "-uall".to_string(), "--".to_string(),
    ];
    if add_all {
        argv.push(":/".to_string());
    } else {
        argv.extend(paths.iter().cloned());
    }
    argv.push(PROTECTED_EXCLUDE_PATHSPEC.to_string());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    for path in porcelain_paths(&exec_git(&argv, cwd)) {
        push_unique(&mut set, path);
    }
    set
}

fn tracked_paths(cwd: Option<&str>, candidates: &[String]) -> HashSet<String> {
    if candidates.is_empty() { return HashSet::new(); }
    let mut argv: Vec<String> = vec!["ls-files".to_string(), "-z".to_string(), "--".to_string()];
    argv.extend(candidates.iter().cloned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    split_nul(&exec_git(&argv, cwd)).into_iter().collect()
}

fn ignored_paths(cwd: Option<&str>, candidates: &[String]) -> HashSet<String> {
    if candidates.is_empty() { return HashSet::new(); }
    let mut argv: Vec<String> = vec!["check-ignore".to_string(), "--".to_string()];
    argv.extend(candidates.iter().cloned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    exec_git(&argv, cwd).lines().map(normalize).collect()
}

fn porcelain_paths(output: &str) -> Vec<String> {
    output.split('\0')
        .filter(|record| !record.is_empty())
        .map(|record| {
            let path = if record.len() >= 4 && record.as_bytes()[2] == b' ' { &record[3..] } else { record };
            normalize(path)
        })
        .collect()
}

fn split_nul(output: &str) -> Vec<String> {
    output.split('\0').filter(|entry| !entry.is_empty()).map(normalize).collect()
}

fn push_unique(set: &mut Vec<String>, path: String) {
    if !path.is_empty() && !set.contains(&path) { set.push(path); }
}

fn normalize(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let mut segments: Vec<&str> = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => {}
            ".." => { segments.pop(); }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn directory_of(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..index].to_string(),
        None => String::new(),
    }
}

fn extension_of(path: &str) -> &str {
    let basename = path.rsplit('/').next().unwrap_or(path);
    match basename.rsplit_once('.') {
        Some((_, extension)) if !basename.starts_with('.') => extension,
        _ => "",
    }
}

fn is_scannable(path: &str) -> bool {
    let unified = path.replace('\\', "/");
    if unified.split('/').any(|segment| SKIP_DIRECTORIES.contains(&segment)) { return false; }
    SCANNABLE_EXTENSIONS.contains(&extension_of(&unified))
}

fn has_code_extension(specifier: &str) -> bool {
    SCANNABLE_EXTENSIONS.iter().any(|extension| specifier.ends_with(&format!(".{extension}")))
}

struct Reference {
    line: usize,
    specifier: String,
}

fn extract_references(source: &str, is_json: bool) -> Vec<Reference> {
    let mut found: Vec<Reference> = Vec::new();
    let mut line = 1usize;
    let mut context: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    let mut literal = String::new();
    let mut literal_line = 1usize;
    let mut escaped = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut previous = '\0';
    let mut preceding_literal = String::new();

    for character in source.chars() {
        if in_line_comment {
            if character == '\n' { line += 1; in_line_comment = false; }
            previous = character;
            continue;
        }
        if in_block_comment {
            if character == '\n' { line += 1; }
            else if previous == '*' && character == '/' { in_block_comment = false; }
            previous = character;
            continue;
        }
        if let Some(closing) = quote {
            if escaped {
                escaped = false;
                literal.push(character);
                previous = character;
                continue;
            }
            if character == '\\' {
                escaped = true;
                previous = character;
                continue;
            }
            if character == closing {
                quote = None;
                if let Some(specifier) = specifier_in_literal(&literal, &context, &preceding_literal, is_json) {
                    found.push(Reference { line: literal_line, specifier });
                    if found.len() >= MAX_REFERENCES_PER_FILE { return found; }
                }
                preceding_literal.clear();
                preceding_literal.push_str(&literal);
                literal.clear();
                previous = character;
                continue;
            }
            if character == '\n' { line += 1; }
            literal.push(character);
            previous = character;
            continue;
        }
        if previous == '/' && character == '/' { in_line_comment = true; context.pop(); previous = character; continue; }
        if previous == '/' && character == '*' { in_block_comment = true; context.pop(); previous = character; continue; }
        if character == '\'' || character == '"' || character == '`' {
            quote = Some(character);
            literal_line = line;
            literal.clear();
            previous = character;
            continue;
        }
        if character == '\n' {
            line += 1;
            push_context(&mut context, ' ');
            previous = character;
            continue;
        }
        push_context(&mut context, character);
        previous = character;
    }
    found
}

fn push_context(context: &mut Vec<char>, character: char) {
    context.push(character);
    if context.len() > CONTEXT_TAIL_CHARS {
        let drain = context.len() - CONTEXT_TAIL_CHARS;
        context.drain(..drain);
    }
}

fn specifier_in_literal(literal: &str, context: &[char], preceding_literal: &str, is_json: bool) -> Option<String> {
    let specifier = literal.trim();
    if specifier.is_empty() { return None; }
    if EXTERNAL_SCHEMES.iter().any(|scheme| specifier.starts_with(scheme)) { return None; }
    let tail: String = context.iter().collect();
    let trimmed = tail.trim_end();
    let keyword = trimmed.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '$' && c != '_');
    let keyword = if keyword.is_empty() { preceding_literal.trim() } else { keyword };
    if is_json {
        let path_valued = (specifier.starts_with("./") || specifier.starts_with("../")) && has_code_extension(specifier);
        if keyword != "$ref" && !path_valued { return None; }
        return Some(specifier.to_string());
    }
    match keyword {
        "from" | "import" | "require" => {
            if declares_type_only(trimmed) { None } else { Some(specifier.to_string()) }
        }
        _ => None,
    }
}

fn declares_type_only(context: &str) -> bool {
    let mut tokens = context
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
        .filter(|token| !token.is_empty());
    match (tokens.next(), tokens.next()) {
        (Some("import") | Some("export"), Some("type")) => true,
        _ => false,
    }
}

fn resolve(specifier: &str, from_directory: &str, root: &str) -> Vec<String> {
    let base = if specifier.starts_with("./") || specifier.starts_with("../") {
        normalize(&format!("{from_directory}/{}", specifier))
    } else if let Some(rest) = specifier.strip_prefix('/') {
        normalize(rest)
    } else {
        return bare_package_candidates(specifier, from_directory, root);
    };
    probe(&base)
}

fn probe(base: &str) -> Vec<String> {
    if base.is_empty() || base.split('/').any(|segment| SKIP_DIRECTORIES.contains(&segment)) {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for extension in PROBE_EXTENSIONS {
        candidates.push(format!("{base}{extension}"));
    }
    for suffix in PROBE_INDEX_SUFFIXES {
        candidates.push(format!("{base}{suffix}"));
    }
    candidates
}

fn bare_package_candidates(specifier: &str, from_directory: &str, root: &str) -> Vec<String> {
    let (package, subpath) = match specifier.split_once('/') {
        Some((package, subpath)) => (package, Some(subpath)),
        None => (specifier, None),
    };
    let mut candidates = Vec::new();
    for directory in ancestor_directories(from_directory) {
        let manifest_path = join_path(&directory, "package.json");
        let Some(text) = read_text(root, &manifest_path) else { continue };
        let Ok(manifest) = serde_json::from_str::<Value>(&text) else { continue };
        if package_name_matches(&manifest, &directory, package) {
            for target in export_targets(&manifest, subpath) {
                candidates.push(normalize(&join_path(&directory, &target)));
            }
            candidates.extend(direct_subpath_candidates(&directory, subpath));
        }
        if declares_workspace_packages(&manifest) {
            let package_directory = join_path(&join_path(&directory, WORKSPACE_PACKAGE_PARENT), package);
            let manifest_path = join_path(&package_directory, "package.json");
            if let Some(text) = read_text(root, &manifest_path) {
                if let Ok(manifest) = serde_json::from_str::<Value>(&text) {
                    for target in export_targets(&manifest, subpath) {
                        candidates.push(normalize(&join_path(&package_directory, &target)));
                    }
                    candidates.extend(direct_subpath_candidates(&package_directory, subpath));
                }
            }
        }
    }
    candidates
}

fn join_path(directory: &str, suffix: &str) -> String {
    if directory.is_empty() { suffix.to_string() } else { format!("{directory}/{suffix}") }
}

fn ancestor_directories(directory: &str) -> Vec<String> {
    let mut directories = vec![directory.to_string()];
    let mut current = directory.to_string();
    while let Some(index) = current.rfind('/') {
        current.truncate(index);
        directories.push(current.clone());
    }
    if directories.last().map(String::as_str) != Some("") {
        directories.push(String::new());
    }
    directories
}

fn package_name_matches(manifest: &Value, directory: &str, package: &str) -> bool {
    if manifest.get("name").and_then(|v| v.as_str()) == Some(package) { return true; }
    directory.rsplit('/').next().unwrap_or("") == package
}

fn declares_workspace_packages(manifest: &Value) -> bool {
    let patterns: Vec<&str> = match manifest.get("workspaces") {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str()).collect(),
        Some(Value::Object(map)) => map.get("packages")
            .and_then(|v| v.as_array())
            .map(|items| items.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    patterns.iter().any(|pattern| {
        let trimmed = pattern.trim_end_matches('/');
        trimmed == "packages/*" || trimmed == "packages/**" || trimmed == "packages"
    })
}

fn direct_subpath_candidates(directory: &str, subpath: Option<&str>) -> Vec<String> {
    match subpath {
        Some(subpath) => vec![
            normalize(&join_path(directory, subpath)),
            normalize(&join_path(&join_path(directory, "src"), subpath)),
        ],
        None => PROBE_INDEX_SUFFIXES.iter().map(|suffix| normalize(&format!("{directory}{suffix}"))).collect(),
    }
}

fn export_targets(manifest: &Value, subpath: Option<&str>) -> Vec<String> {
    let Some(exports) = manifest.get("exports") else { return Vec::new() };
    let key = match subpath {
        Some(subpath) => format!("./{subpath}"),
        None => ".".to_string(),
    };
    let mut targets = Vec::new();
    match exports {
        Value::String(target) if subpath.is_none() => targets.push(target.clone()),
        Value::Object(map) => {
            if let Some(value) = map.get(&key) {
                collect_export_targets(value, &mut targets, None);
            }
            for (pattern, value) in map {
                let Some(prefix) = pattern.strip_suffix("/*") else { continue };
                let Some(rest) = key.strip_prefix(&format!("{prefix}/")) else { continue };
                collect_export_targets(value, &mut targets, Some(rest));
            }
        }
        _ => {}
    }
    targets
}

fn collect_export_targets(value: &Value, targets: &mut Vec<String>, wildcard: Option<&str>) {
    match value {
        Value::String(target) => targets.push(match wildcard {
            Some(rest) => target.replace('*', rest),
            None => target.clone(),
        }),
        Value::Object(map) => {
            for key in CONDITION_KEYS {
                if let Some(nested) = map.get(*key) {
                    collect_export_targets(nested, targets, wildcard);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_export_targets(item, targets, wildcard);
            }
        }
        _ => {}
    }
}
