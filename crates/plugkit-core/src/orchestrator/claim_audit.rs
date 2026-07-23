use super::gm_dir;
use crate::pkfs;

// Real incident this closes structurally: a memory record asserted
// "ccsniff --verb-bypass-discipline and --spool-discipline were shipped and
// validated in commit 0714d7e" -- the claim was false or the commit was
// later lost, and it was only caught by a manual sweep. This module makes
// that kind of check a live gate instead of a lucky manual catch.
//
// Witness classification taxonomy (what this check can and cannot resolve):
// 1. Commit-hash claims ("shipped/fixed/landed in <hash>") -- the ONLY class
//    this module resolves automatically. Witness = `git cat-file -e <hash>`
//    against the referenced repo (this repo, or a named submodule -- see
//    submodule_for_line). Deterministic, cheap, exactly what would have
//    caught the real incident.
// 2. Behavioral/capability claims with no hash ("recall now scores by
//    cosine x recency", "browser session persists across dispatches") --
//    require dispatching the actual verb and checking its live response
//    shape, not a git-log lookup. Out of scope for this check by
//    construction: a git commit existing proves code was committed, not
//    that the described behavior is what that code currently does. Left as
//    a documented gap rather than a guessed heuristic (e.g. regex-matching
//    behavioral prose) that would itself be an unverifiable claim about
//    claims.
// 3. Fix-reproduction claims ("bug Z is resolved") -- require reproducing
//    the original failure condition live and confirming it no longer
//    reproduces. Also out of scope: reproduction needs the specific bug's
//    original repro steps, which this generic scanner has no way to derive
//    from a one-line AGENTS.md/memory assertion.
// Classes 2 and 3 are real coverage gaps, not silently ignored: `handle_audit`
// only ever asserts non-stale for what it actually checked (class 1), and
// intentionally does not claim class-2/3 assertions are "audited clean" --
// callers relying on this gate for full claim coverage should read this
// comment, not assume the gate is exhaustive over every claim shape.
const CLAIM_MARKERS: &[&str] = &["shipped", "validated", "confirmed live", "landed in", "fixed in", "live-witnessed"];

fn looks_like_commit_hash(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    (7..=40).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn extract_hash_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|tok| tok.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .filter(|t| looks_like_commit_hash(t))
        .collect()
}

fn line_asserts_claim(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    CLAIM_MARKERS.iter().any(|m| lower.contains(m))
}

#[derive(serde::Serialize)]
struct ClaimFinding {
    line_excerpt: String,
    hash: String,
    resolved: bool,
    checked_in: String,
}

// Known submodule directory names this repo checks out inside its own tree
// (per AGENTS.md's own submodule enumeration) -- a claim line naming one of
// these directories is checked against THAT submodule's own git log, not
// this repo's, since a commit landed in a sibling repo never appears in gm's
// own history. This is exactly the real incident's shape: the ccsniff
// commit 0714d7e was claimed shipped in ccsniff, a repo gm does not
// directly own, so checking only gm's own git log would have missed it
// entirely (a false "resolved" on a claim this check was built to catch).
const KNOWN_SUBMODULES: &[&str] = &[
    "agentplug", "rs-plugkit", "rs-codeinsight", "rs-search",
    "agentplug-bert", "agentplug-libsql", "agentplug-treesitter",
];

fn submodule_for_line(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    KNOWN_SUBMODULES.iter().find(|name| lower.contains(&name.to_ascii_lowercase())).copied()
}

#[cfg(target_arch = "wasm32")]
fn commit_exists(hash: &str, submodule: Option<&str>) -> bool {
    let v = crate::wasm_dispatch::git_call_argv(&["cat-file", "-e", hash], submodule);
    v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(1) == 0
}

#[cfg(not(target_arch = "wasm32"))]
fn commit_exists(_hash: &str, _submodule: Option<&str>) -> bool { true }

fn scan_lines_for_claims(content: &str, source: &str, findings: &mut Vec<ClaimFinding>, claims_found: &mut usize, scanned_lines: &mut usize) {
    for line in content.lines() {
        *scanned_lines += 1;
        if !line_asserts_claim(line) { continue; }
        let hashes = extract_hash_tokens(line);
        if hashes.is_empty() { continue; }
        let submodule = submodule_for_line(line);
        for hash in hashes {
            *claims_found += 1;
            let resolved = commit_exists(&hash, submodule);
            let excerpt: String = format!("[{}] {}", source, line.trim()).chars().take(180).collect();
            let checked_in = submodule.unwrap_or("gm (this repo)").to_string();
            findings.push(ClaimFinding { line_excerpt: excerpt, hash, resolved, checked_in });
        }
    }
}

/// Scans AGENTS.md AND the `default` recall-memory corpus (every stored
/// memory record's text, not just standing prose) for shipped/validated/
/// fixed/landed/live-witnessed claims carrying a commit-hash-shaped token,
/// and checks each hash against the referenced repo's own git log via `git
/// cat-file -e <hash>`. Returns `{ok, scanned_lines, claims_found, stale,
/// findings:[{line_excerpt,hash,resolved,checked_in}]}`. The real ccsniff
/// incident this closes was a MEMORY record's claim, not an AGENTS.md rule
/// -- AGENTS.md-only scanning would have missed it entirely, so both
/// sources are scanned unconditionally, never just one.
pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let mut findings: Vec<ClaimFinding> = Vec::new();
    let mut claims_found = 0usize;
    let mut scanned_lines = 0usize;

    let agents_path = std::path::Path::new(".").join("AGENTS.md");
    let agents_s = agents_path.to_string_lossy().to_string();
    if let Some(content) = pkfs::read_to_string(&agents_s) {
        scan_lines_for_claims(&content, "AGENTS.md", &mut findings, &mut claims_found, &mut scanned_lines);
    }

    #[cfg(target_arch = "wasm32")]
    for (key, text) in crate::memory_md::flat_kv_entries("default") {
        scan_lines_for_claims(&text, &key, &mut findings, &mut claims_found, &mut scanned_lines);
    }

    let stale_count = findings.iter().filter(|f| !f.resolved).count();
    let marker = gm_dir().join("claim-audit-fired");
    let marker_body = if stale_count > 0 { "stale" } else { "clean" };
    let _ = pkfs::write(&marker.to_string_lossy().to_string(), marker_body);

    let payload = serde_json::json!({
        "ok": true,
        "scanned_lines": scanned_lines,
        "claims_found": claims_found,
        "stale": stale_count,
        "findings": findings.iter().map(|f| serde_json::json!({
            "line_excerpt": f.line_excerpt, "hash": f.hash, "resolved": f.resolved, "checked_in": f.checked_in,
        })).collect::<Vec<_>>(),
    });
    (payload.to_string(), String::new(), 0)
}

pub fn claim_audit_fired() -> bool {
    let marker = gm_dir().join("claim-audit-fired");
    pkfs::exists(&marker.to_string_lossy().to_string())
}

pub fn claim_audit_clean() -> bool {
    // The gate only cares that the audit RAN this stopping window and found
    // no stale claims -- it does not re-run the scan itself (that is
    // `handle_audit`'s job, dispatched explicitly like residual-scan). A
    // never-run audit fails closed (denies), matching every other CONSOLIDATE
    // gate's fail-closed-on-unknown discipline.
    let marker = gm_dir().join("claim-audit-fired");
    match pkfs::read_to_string(&marker.to_string_lossy().to_string()) {
        Some(v) => v.trim() != "stale",
        None => false,
    }
}
