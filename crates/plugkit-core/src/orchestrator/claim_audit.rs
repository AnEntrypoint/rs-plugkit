use super::gm_dir;
use crate::pkfs;

const CLAIM_MARKERS: &[&str] = &["shipped", "validated", "confirmed live", "landed in", "fixed in", "live-witnessed"];

const KNOWN_SUBMODULES: &[&str] = &[
    "agentplug", "rs-plugkit", "rs-codeinsight", "rs-search",
    "agentplug-bert", "agentplug-libsql", "agentplug-treesitter",
];

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

fn line_asserts_shipped_claim(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    CLAIM_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn named_submodule_in_line(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    KNOWN_SUBMODULES.iter().find(|name| lower.contains(&name.to_ascii_lowercase())).copied()
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

fn scan_text_for_hash_claims(text: &str, source_label: &str, findings: &mut Vec<HashClaimFinding>, scanned_line_count: &mut usize) {
    for line in text.lines() {
        *scanned_line_count += 1;
        if !line_asserts_shipped_claim(line) { continue; }
        let hashes = extract_commit_hash_tokens(line);
        if hashes.is_empty() { continue; }
        let submodule = named_submodule_in_line(line);
        for hash in hashes {
            let hash_resolved_in_repo_history = commit_hash_exists_in_repo_history(&hash, submodule);
            let line_excerpt: String = format!("[{}] {}", source_label, line.trim()).chars().take(180).collect();
            let checked_in_repo = submodule.unwrap_or("gm (this repo)").to_string();
            findings.push(HashClaimFinding { line_excerpt, hash, hash_resolved_in_repo_history, checked_in_repo });
        }
    }
}

pub fn handle_audit(_content: &str) -> (String, String, i32) {
    let mut findings: Vec<HashClaimFinding> = Vec::new();
    let mut scanned_line_count = 0usize;

    let agents_md_path = std::path::Path::new(".").join("AGENTS.md").to_string_lossy().to_string();
    if let Some(agents_md_text) = pkfs::read_to_string(&agents_md_path) {
        scan_text_for_hash_claims(&agents_md_text, "AGENTS.md", &mut findings, &mut scanned_line_count);
    }

    #[cfg(target_arch = "wasm32")]
    for (memory_key, memory_text) in crate::memory_md::flat_kv_entries("default") {
        scan_text_for_hash_claims(&memory_text, &memory_key, &mut findings, &mut scanned_line_count);
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

pub fn claim_audit_clean() -> bool {
    let marker_path = gm_dir().join("claim-audit-fired").to_string_lossy().to_string();
    match pkfs::read_to_string(&marker_path) {
        Some(marker_body) => marker_body.trim() != "stale",
        None => false,
    }
}
