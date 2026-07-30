use super::gm_dir;
use crate::pkfs;

fn looks_like_commit_hash(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    (7..=40).contains(&trimmed.len()) && trimmed.chars().all(|c| c.is_ascii_hexdigit())
}

fn extract_commit_hash_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .filter(|token| looks_like_commit_hash(token))
        .collect()
}

fn claim_audit_config() -> crate::ragconfig::ClaimAuditConfig {
    crate::ragconfig::RagConfig::resolved().claim_audit
}

fn line_asserts_shipped_claim_cfg(line: &str, cfg: &crate::ragconfig::ClaimAuditConfig) -> bool {
    let lower = line.to_ascii_lowercase();
    cfg.shipped_claim_markers_matched_case_insensitive_substring
        .iter()
        .any(|marker| lower.contains(&marker.to_ascii_lowercase()))
}

/// Which submodule, if any, a claim line is talking about.
///
/// Derived from the same `.gitmodules`-backed source `submodule_drift` uses,
/// rather than a second hardcoded copy of the list. The two copies were
/// byte-identical and had to stay that way: a name added to one and not the
/// other sends a hash to the WRONG repo to be verified, which reports a
/// perfectly valid claim as stale.
///
/// Matched on the final path component, since a claim line names a repo
/// ("landed in rs-plugkit abc1234"), not a checkout path.
fn named_submodule_in_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    super::submodule_drift::submodule_paths()
        .into_iter()
        .find(|path| {
            let name = path.rsplit('/').next().unwrap_or(path.as_str());
            !name.is_empty() && lower.contains(&name.to_ascii_lowercase())
        })
        .map(|path| path.rsplit('/').next().unwrap_or(path.as_str()).to_string())
}

#[derive(serde::Serialize)]
pub struct HashClaimFinding {
    line_excerpt: String,
    hash: String,
    hash_resolved_in_repo_history: bool,
    checked_in_repo: String,
}

#[cfg(target_arch = "wasm32")]
fn commit_hash_exists_in_repo_history(hash: &str, submodule: Option<&str>) -> bool {
    let result = crate::wasm_dispatch::git_call_argv(&["cat-file", "-e", hash], submodule);
    result.get("exit_code").and_then(|code| code.as_i64()).unwrap_or(1) == 0
}

#[cfg(not(target_arch = "wasm32"))]
fn commit_hash_exists_in_repo_history(_hash: &str, _submodule: Option<&str>) -> bool { true }

fn scan_text_for_hash_claims(text: &str, source_label: &str, findings: &mut Vec<HashClaimFinding>, scanned_line_count: &mut usize, cfg: &crate::ragconfig::ClaimAuditConfig) {
    for line in text.lines() {
        *scanned_line_count += 1;
        if !line_asserts_shipped_claim_cfg(line, cfg) { continue; }
        let hashes = extract_commit_hash_tokens(line);
        if hashes.is_empty() { continue; }
        let submodule = named_submodule_in_line(line);
        for hash in hashes {
            let hash_resolved_in_repo_history = commit_hash_exists_in_repo_history(&hash, submodule.as_deref());
            let line_excerpt: String = format!("[{}] {}", source_label, line.trim()).chars().take(180).collect();
            let checked_in_repo = submodule.clone().unwrap_or_else(|| "gm (this repo)".to_string());
            findings.push(HashClaimFinding { line_excerpt, hash, hash_resolved_in_repo_history, checked_in_repo });
        }
    }
}

pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let mut findings: Vec<HashClaimFinding> = Vec::new();
    let mut scanned_line_count = 0usize;

    let audit_cfg = claim_audit_config();

    for scan_path in &audit_cfg.scan_paths_relative_to_project_root_missing_is_skip_not_error {
        let full = std::path::Path::new(".").join(scan_path).to_string_lossy().to_string();
        if let Some(text) = pkfs::read_to_string(&full) {
            scan_text_for_hash_claims(&text, scan_path, &mut findings, &mut scanned_line_count, &audit_cfg);
        }
    }

    #[cfg(target_arch = "wasm32")]
    for (memory_key, memory_text) in crate::memory_md::flat_kv_entries("default") {
        scan_text_for_hash_claims(&memory_text, &memory_key, &mut findings, &mut scanned_line_count, &audit_cfg);
    }

    let stale_claim_count = findings.iter().filter(|finding| !finding.hash_resolved_in_repo_history).count();
    let marker_path = gm_dir().join("claim-audit-fired").to_string_lossy().to_string();
    let marker_body = if stale_claim_count > 0 { "stale" } else { "clean" };
    let _ = pkfs::write(&marker_path, marker_body);

    let claims_found = findings.len();
    let payload = serde_json::json!({
        "ok": true,
        "scanned_lines": scanned_line_count,
        "claims_found": claims_found,
        "stale": stale_claim_count,
        "findings": findings.iter().map(|finding| serde_json::json!({
            "line_excerpt": finding.line_excerpt,
            "hash": finding.hash,
            "resolved": finding.hash_resolved_in_repo_history,
            "checked_in": finding.checked_in_repo,
        })).collect::<Vec<_>>(),
    });
    (payload.to_string(), String::new(), 0)
}

pub fn claim_audit_fired() -> bool {
    let marker_path = gm_dir().join("claim-audit-fired").to_string_lossy().to_string();
    pkfs::exists(&marker_path)
}

/// Whether the last claim audit found nothing stale.
///
/// Requires the literal `clean`. Testing `!= "stale"` made every unexpected
/// body -- empty, truncated by a partial write, or any other content -- read as
/// a passing audit, so a gate on both guarded edges could be satisfied by a
/// marker no audit ever wrote. A gate must fail CLOSED on anything it does not
/// positively recognise, which is the same rule `predicate_result` applies to an
/// unknown predicate name.
pub fn claim_audit_clean() -> bool {
    let marker_path = gm_dir().join("claim-audit-fired").to_string_lossy().to_string();
    match pkfs::read_to_string(&marker_path) {
        Some(marker_body) => marker_body.trim() == "clean",
        None => false,
    }
}
