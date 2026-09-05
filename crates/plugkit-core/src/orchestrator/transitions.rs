use super::fsm::{self, GateDef, HookMode};
use super::state::{Phase, read_state_with_graph, set_phase_with_session_with_graph};
use super::prd;
use super::recall;
use super::mutables;

pub fn next_skill(current: &Phase, g: &fsm::Graph) -> String {
    g.state(current.as_str())
        .and_then(|s| s.skill.clone())
        .unwrap_or_else(|| format!("gm-{}", current.as_str().to_ascii_lowercase()))
}

pub fn next_phase(current: &Phase, g: &fsm::Graph) -> Phase {
    match g.default_edge_from(current.as_str()) {
        Some(e) => Phase::parse(&e.to).unwrap_or_else(|| current.clone()),
        None => current.clone(),
    }
}

pub fn known_predicates() -> Vec<(&'static str, &'static str)> {
    predicate_table().iter().map(|(name, desc, _)| (*name, *desc)).collect()
}

type PredicateFn = fn() -> bool;

fn predicate_table() -> &'static [(&'static str, &'static str, PredicateFn)] {
    &[
        ("residual-scan-fired", "true once `residual-scan` has been dispatched in this stop window (the .gm/residual-check-fired marker is present AND non-empty -- it is invalidated by truncation)", residual_scan_fired as PredicateFn),
        ("prd-all-closed", "true when .gm/prd.yml has zero rows with an open status (pending/in-progress, not completed)", pred_prd_all_closed),
        ("mutables-all-resolved", "true when .gm/mutables.yml has zero rows still in unknown/pending status", pred_mutables_all_resolved),
        ("mutables-all-typed", "true when every PENDING row in .gm/mutables.yml tagged with a PROVE-kind obligation_kind (precondition/invariant/postcondition/resource-bound/type-shape) has a valid kind AND every id in its depends_on is already resolved. Resolved rows and rows tagged with a different phase's kind never block this gate. The gate message names the specific blocking row and reason.", pred_mutables_all_typed as PredicateFn),
        ("state-obligations-ready", "true when every PENDING row tagged with a STATE-kind obligation_kind (totality/ownership/replay/effect-boundary) is validly typed and dependency-resolved. Same DAG mechanics as mutables-all-typed, scoped to STATE's own obligation kinds.", pred_state_obligations_ready as PredicateFn),
        ("conc-obligations-ready", "true when every PENDING row tagged with a CONC-kind obligation_kind (happens-before/disjointness/contention) is validly typed and dependency-resolved.", pred_conc_obligations_ready as PredicateFn),
        ("sec-obligations-ready", "true when every PENDING row tagged with a SEC-kind obligation_kind (secrets/injection/identity-authority/message-timing) is validly typed and dependency-resolved.", pred_sec_obligations_ready as PredicateFn),
        ("res-obligations-ready", "true when every PENDING row tagged with a RES-kind obligation_kind (exception-model/partial-failure/degradation/crucible) is validly typed and dependency-resolved.", pred_res_obligations_ready as PredicateFn),
        ("worktree-clean", "true when `git status --porcelain` is empty -- no uncommitted/unpushed delta", pred_worktree_clean),
        ("ci-validated-fresh", "true when .gm/exec-spool/.ci-validated exists and its head_sha matches the current `git rev-parse HEAD` -- a witnessed-green CI run for the exact pushed commit", ci_validation_fresh as PredicateFn),
        ("browser-witness-coverage", "true when every client-side file edited this session (per .gm/exec-spool/.turn-browser-edits.json) has a matching entry in .gm/exec-spool/.turn-browser-witnessed with the same content hash", pred_browser_witness_coverage),
        ("app-loads-witnessed", "true when this project has no .gm/browser-config.json (no declared browser entrypoint, nothing to check), OR the most recent `browser` dispatch this stop window recorded a healthy app-loads witness (ok:true, zero pageErrors, zero console error-level lines) in .gm/exec-spool/.app-loads-witnessed.json. Unlike browser-witness-coverage, this check is unconditional -- it does not read .turn-browser-edits.json and is never vacuously satisfied by an empty edit list, so a zero-edit confirmation/audit turn claiming the app is healthy still must dispatch `browser` this turn to prove it.", pred_app_loads_witnessed as PredicateFn),
        ("claim-audit-clean", "true when the claim audit finds no unwitnessed completion claims -- see orchestrator::claim_audit", pred_claim_audit_clean),
        ("submodules-clean", "true when no submodule has drifted from its recorded commit -- see orchestrator::submodule_drift", pred_submodules_clean),
        ("no-synthetic-test-files", "true when the working diff introduces no standing test file (*.test.*, *.spec.*, or a test/tests/__tests__ directory). VERIFY doctrine forbids them: verification is a live exec_js/browser witness against real code, never a suite asserting against mocks. Emits deviation.synthetic-test-file naming the offending paths when it fails.", pred_no_synthetic_test_files as PredicateFn),
        ("remote-hook-refused", "always false. Substituted by fsm::graph() for a gate whose ONLY condition was a hook supplied by the compiled-default tier, which never legitimately carries one: the author's condition is genuinely not being evaluated and the edge it guards must not be waved through. Local and source-repo tier hooks both execute normally and never hit this substitution. Vendor the graph (and its hook) into .gm/instructions/fsm/graph.json, or configure source.json, to restore the gate.", pred_remote_hook_refused),
        ("no-admit-deferral-markers", "true when new lines in source files (*.rs/.js/.ts/.py/.go/...) in the working diff introduce no colon-form admit marker (TODO:/FIXME:/XXX:/HACK:), no todo!()/unimplemented!() placeholder macro, and no 'not (yet) implemented' phrase. Source-scoped so the rule's own registry and prose describing it do not self-trip; prose-level deferral is covered by no-hedge-language-in-diff. A marker stands in for a complete proof. Emits deviation.admit-deferral-marker naming the offending lines when it fails.", pred_no_admit_deferral_markers as PredicateFn),
        ("no-secrets-in-diff", "true when the working diff introduces no line matching a high-confidence secret shape (AWS-style access key id, a private-key PEM header, a bearer/API token assigned to a literal string of plausible entropy, a database URL with an inline password). Heuristic and diff-scoped, not a substitute for a dedicated secret scanner -- catches the common accidental-commit shape. Emits deviation.secret-in-diff naming the offending lines (redacted) when it fails.", pred_no_secrets_in_diff as PredicateFn),
        ("no-unchecked-panics-in-diff", "true when new Rust/JS/TS lines in the working diff introduce no bare unwrap()/expect()/panic!() outside a test-only-configured Rust module or *.test.* path, and no JS/TS line that throws without a paired catch reachable in the same function body scope (best-effort, brace-balance heuristic). Exception model requires every raised error handled or explicitly propagated, never left to crash the process uncaught. Emits deviation.unchecked-panic naming the offending lines when it fails.", pred_no_unchecked_panics_in_diff as PredicateFn),
        ("no-hedge-language-in-diff", "true when prose files (*.md) touched in the working diff introduce no hedge/deferral phrase ('todo later', 'in a future session', 'for now we', 'as a stopgap', 'good enough for now', 'left as an exercise', 'out of scope for this'). Decisive commitment forbids shipping a hedge in place of a decision. Emits deviation.hedge-language naming the offending lines when it fails.", pred_no_hedge_language_in_diff as PredicateFn),
        ("split-context-swept", "true when the working diff touches at most one file, OR .gm/exec-spool/.split-context-swept exists with a head_sha matching the current `git rev-parse HEAD` -- a witnessed independent-Agent adversarial review for the exact pushed multi-file commit. A self-reviewed multi-file diff has not been adversarially swept.", split_context_swept as PredicateFn),
        ("no-graphical-symbols-in-diff", "true when new lines in the working diff introduce no decorative non-ASCII glyph (arrows, box-drawing, stars, bullets, checks/crosses, emoji) outside a binary/frozen-changelog/icon-font exemption path. Matches AGENTS.md's own no-graphical-symbols discipline as a real gate instead of an on-sight-only rule. Emits deviation.graphical-symbol naming the offending lines when it fails.", pred_no_graphical_symbols_in_diff as PredicateFn),
        ("idempotent-dispatch-replay-safe", "true when the most recent N dispatch audit-tuples (id, hash, ts) for the current stop window contain no exact-duplicate (id, hash) pair recorded as two DIFFERENT outcomes -- a same-input dispatch replayed must reach the same result (f-compose-f-equals-f), never a second, different mutation applied on top of the first. Emits deviation.non-idempotent-replay naming the conflicting tuples when it fails.", pred_idempotent_dispatch_replay_safe as PredicateFn),
    ]
}

fn pred_remote_hook_refused() -> bool { false }

fn pred_prd_all_closed() -> bool { !prd_has_open_items() }
fn pred_mutables_all_resolved() -> bool { mutables::pending_detailed().is_empty() }
fn pred_mutables_all_typed() -> bool { mutables::all_typed() }
fn pred_state_obligations_ready() -> bool { mutables::state_obligations_ready() }
fn pred_conc_obligations_ready() -> bool { mutables::conc_obligations_ready() }
fn pred_sec_obligations_ready() -> bool { mutables::sec_obligations_ready() }
fn pred_res_obligations_ready() -> bool { mutables::res_obligations_ready() }
fn pred_worktree_clean() -> bool { !worktree_dirty() }
fn pred_browser_witness_coverage() -> bool { check_browser_witness_coverage_for_cwd("").is_empty() }
#[cfg(target_arch = "wasm32")]
fn pred_app_loads_witnessed() -> bool { crate::browser_witness::app_loads_witnessed_this_stop_window("") }
#[cfg(not(target_arch = "wasm32"))]
fn pred_app_loads_witnessed() -> bool { true }
#[cfg(target_arch = "wasm32")]
fn pred_claim_audit_clean() -> bool { super::claim_audit::claim_audit_clean() }
#[cfg(not(target_arch = "wasm32"))]
fn pred_claim_audit_clean() -> bool { true }
fn pred_submodules_clean() -> bool { super::submodule_drift::submodules_clean() }

#[cfg(target_arch = "wasm32")]
fn emit_unknown_predicate(other: &str) {
    crate::wasm_dispatch::emit_event("fsm_unknown_predicate", serde_json::json!({
        "predicate": other,
        "reason": "not in transitions::known_predicates(); this gate can never be satisfied. Fix the name in .gm/instructions/fsm/graph.json (see fsm/predicates.md for the valid set) or use a jit hook for a condition that has no compiled predicate.",
    }));
}
#[cfg(not(target_arch = "wasm32"))]
fn emit_unknown_predicate(_other: &str) {}

fn predicate_result(name: &str) -> bool {
    if let Some((_, _, f)) = predicate_table().iter().find(|(n, _, _)| *n == name) {
        return f();
    }
    emit_unknown_predicate(name);
    false
}

fn residual_scan_marker_matches_current_session_or_is_within_longgap_threshold(
    fired_sid: &str,
    fired_at_ms: u64,
) -> bool {
    let current_sid = super::state::read_state().session_id.unwrap_or_default();
    if !fired_sid.is_empty() && !current_sid.is_empty() {
        return fired_sid == current_sid;
    }
    let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
    let threshold_ms = super::fsm::graph().policy.longgap_threshold_ms;
    now_ms.saturating_sub(fired_at_ms) <= threshold_ms
}

#[cfg(target_arch = "wasm32")]
fn residual_scan_fired() -> bool {
    let residual_marker = super::gm_dir().join("residual-check-fired");
    let raw = match crate::pkfs::read_to_string(&residual_marker.to_string_lossy().to_string()) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return false,
    };
    let mut parts = raw.trim().splitn(2, ':');
    let fired_sid = parts.next().unwrap_or("");
    let fired_at_ms: u64 = match parts.next().and_then(|s| s.parse().ok()) {
        Some(ms) => ms,
        None => return false,
    };
    residual_scan_marker_matches_current_session_or_is_within_longgap_threshold(fired_sid, fired_at_ms)
}
#[cfg(not(target_arch = "wasm32"))]
fn residual_scan_fired() -> bool { false }

fn prd_has_open_items() -> bool {
    let (body, _err, code) = prd::handle_list("");
    if code != 0 { return false; }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else { return false };
    let Some(items) = v.get("items").and_then(|v| v.as_array()) else { return false };
    items.iter().any(|it| {
        let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
        let blocked_external = it.get("blockedBy")
            .and_then(|v| v.as_array())
            .map(|seq| seq.iter().any(|x| x.as_str() == Some("external")))
            .unwrap_or(false);
        prd::status_is_open(status) && !blocked_external
    })
}

#[cfg(target_arch = "wasm32")]
fn worktree_dirty() -> bool {
    !crate::wasm_dispatch::git_porcelain().trim().is_empty()
}

#[cfg(target_arch = "wasm32")]
fn synthetic_test_files_added_in_working_diff() -> Vec<String> {
    let porcelain = crate::wasm_dispatch::git_porcelain();
    let mut found = Vec::new();
    for line in porcelain.lines() {
        let path = line.get(3..).unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let lower = path.to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower).to_string();
        let is_test_file = name.contains(".test.") || name.contains(".spec.");
        let is_test_dir = lower.contains("/test/")
            || lower.contains("/tests/")
            || lower.contains("/__tests__/")
            || lower.starts_with("test/")
            || lower.starts_with("tests/")
            || lower.starts_with("__tests__/");
        if is_test_file || is_test_dir {
            found.push(path.to_string());
        }
    }
    found
}

#[cfg(not(target_arch = "wasm32"))]
fn synthetic_test_files_added_in_working_diff() -> Vec<String> { vec![] }

#[cfg(target_arch = "wasm32")]
fn pred_no_synthetic_test_files() -> bool {
    let found = synthetic_test_files_added_in_working_diff();
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event("deviation.synthetic-test-file", serde_json::json!({
        "files": found,
        "reason": "VERIFY doctrine forbids standing test files: delete them and replace their assertions with a live exec_js/browser witness, then re-verify",
    }));
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn pred_no_synthetic_test_files() -> bool { true }
#[cfg(not(target_arch = "wasm32"))]
fn worktree_dirty() -> bool { false }

#[cfg(target_arch = "wasm32")]
fn added_lines_in_diff() -> Vec<(String, usize, String)> {
    let raw = crate::wasm_dispatch::git_call("diff --unified=0 HEAD", None);
    let stdout = raw.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
    let mut out = Vec::new();
    let mut current_path = String::new();
    let mut current_line = 0usize;
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_path = path.to_string();
            continue;
        }
        if let Some(hunk) = line.strip_prefix("@@ ") {
            if let Some(plus) = hunk.split("+").nth(1) {
                let num_part = plus.split(|c: char| c == ',' || c == ' ').next().unwrap_or("0");
                current_line = num_part.parse().unwrap_or(0);
            }
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if !added.starts_with("++") {
                out.push((current_path.clone(), current_line, added.to_string()));
                current_line += 1;
            }
            continue;
        }
    }
    out
}
#[cfg(not(target_arch = "wasm32"))]
fn added_lines_in_diff() -> Vec<(String, usize, String)> { vec![] }

fn is_test_scoped_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".test.") || lower.contains(".spec.") || lower.contains("/test/") || lower.contains("/tests/") || lower.contains("/__tests__/")
}

fn needle_first_occurrence_sits_inside_quoted_string_literal(text: &str, needle: &str) -> bool {
    let Some(idx) = text.find(needle) else { return false };
    let before = &text[..idx];
    let d = before.matches('"').count();
    let s = before.matches('\'').count();
    let b = before.matches('`').count();
    d % 2 == 1 || s % 2 == 1 || b % 2 == 1
}

#[cfg(target_arch = "wasm32")]
fn pred_no_admit_deferral_markers() -> bool {
    const COLON_MARKERS: &[&str] = &["TODO:", "FIXME:", "XXX:", "HACK:", "todo!(", "unimplemented!("];
    const PHRASES: &[&str] = &["not implemented", "not yet implemented"];
    const SOURCE_EXTS: &[&str] = &[".rs", ".js", ".ts", ".jsx", ".tsx", ".mjs", ".cjs", ".py", ".go", ".java", ".c", ".cc", ".cpp", ".h", ".hpp", ".sh", ".ps1", ".vue", ".svelte"];
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if !SOURCE_EXTS.iter().any(|e| path.ends_with(e)) { continue; }
        let upper = text.to_ascii_uppercase();
        let colon_hit = COLON_MARKERS.iter().any(|m| {
            let mu = m.to_ascii_uppercase();
            upper.contains(&mu) && !needle_first_occurrence_sits_inside_quoted_string_literal(&upper, &mu)
        });
        let lower = text.to_ascii_lowercase();
        let phrase_hit = PHRASES.iter().any(|p| lower.contains(p) && !needle_first_occurrence_sits_inside_quoted_string_literal(&lower, p));
        if colon_hit || phrase_hit {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    if found.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.admit-deferral-marker", serde_json::json!({
        "lines": found,
        "reason": "an admit/deferral marker in a source file stands in for a complete proof -- finish the work or remove the marker, then re-attempt",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_no_admit_deferral_markers() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn pred_no_secrets_in_diff() -> bool {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        let looks_like_aws_key = text.contains("AKIA") && text.matches(|c: char| c.is_ascii_alphanumeric()).count() >= 20;
        let looks_like_private_key = text.contains("-----BEGIN") && text.contains("PRIVATE KEY");
        let looks_like_db_url_with_password = (text.contains("://") && text.contains('@'))
            && (text.contains("postgres") || text.contains("mysql") || text.contains("mongodb") || text.contains("redis"))
            && text.contains(':') && !text.contains("<") && !text.contains("${") && !text.contains("%s");
        let lower = text.to_ascii_lowercase();
        let looks_like_bearer_literal = (lower.contains("api_key") || lower.contains("apikey") || lower.contains("bearer ") || lower.contains("secret_key"))
            && text.contains('"') && text.matches(|c: char| c.is_ascii_alphanumeric()).count() >= 24
            && !lower.contains("process.env") && !lower.contains("env::var") && !lower.contains("getenv");
        if looks_like_aws_key || looks_like_private_key || looks_like_db_url_with_password || looks_like_bearer_literal {
            let redacted: String = text.chars().take(20).collect();
            found.push(format!("{path}:{line_no}: {redacted}... (redacted)"));
        }
    }
    if found.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.secret-in-diff", serde_json::json!({
        "lines": found,
        "reason": "a line in the working diff matches a high-confidence secret shape -- remove the literal, route it through an env var or secret store, then re-attempt",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_no_secrets_in_diff() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn pred_no_unchecked_panics_in_diff() -> bool {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if is_test_scoped_path(&path) { continue; }
        let trimmed = text.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let is_rust = path.ends_with(".rs");
        let is_js_like = path.ends_with(".js") || path.ends_with(".ts") || path.ends_with(".jsx") || path.ends_with(".tsx") || path.ends_with(".mjs") || path.ends_with(".cjs");
        if is_rust {
            let has_unwrap = text.contains(".unwrap()") && !text.contains("unwrap_or");
            let has_expect = text.contains(".expect(");
            let has_panic = text.contains("panic!(");
            if has_unwrap || has_expect || has_panic {
                found.push(format!("{path}:{line_no}: {trimmed}"));
            }
        } else if is_js_like {
            if trimmed.starts_with("throw ") && !trimmed.contains("//") {
                found.push(format!("{path}:{line_no}: {trimmed}"));
            }
        }
    }
    if found.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.unchecked-panic", serde_json::json!({
        "lines": found,
        "reason": "a new line panics/throws/unwraps outside a test path with no visible handling -- propagate the error explicitly (Result/catch) or justify the panic as a real precondition violation, then re-attempt",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_no_unchecked_panics_in_diff() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn pred_no_hedge_language_in_diff() -> bool {
    const HEDGES: &[&str] = &["todo later", "in a future session", "for now we", "as a stopgap", "good enough for now", "left as an exercise", "out of scope for this", "not yet implemented", "we'll come back to"];
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if !path.ends_with(".md") { continue; }
        let lower = text.to_ascii_lowercase();
        if HEDGES.iter().any(|h| lower.contains(h)) {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    if found.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.hedge-language", serde_json::json!({
        "lines": found,
        "reason": "a hedge/deferral phrase in touched prose stands in for a decision -- commit to the real answer or remove the hedge, then re-attempt",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_no_hedge_language_in_diff() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn pred_no_graphical_symbols_in_diff() -> bool {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if path.ends_with("CHANGELOG.md") { continue; }
        let has_glyph = text.chars().any(|c| {
            let cp = c as u32;
            matches!(cp, 0x2190..=0x21FF | 0x2500..=0x257F | 0x2600..=0x27BF | 0x1F300..=0x1FAFF | 0x2B00..=0x2BFF)
        });
        if has_glyph {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    if found.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.graphical-symbol", serde_json::json!({
        "lines": found,
        "reason": "a decorative non-ASCII glyph landed in tracked source/prose -- convert to its plain-ASCII equivalent (->, -/*, [x]/[ ], done/todo/pass/fail), then re-attempt",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_no_graphical_symbols_in_diff() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn pred_idempotent_dispatch_replay_safe() -> bool {
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.audit-tuples.json").unwrap_or_default();
    if raw.trim().is_empty() { return true; }
    let Ok(serde_json::Value::Array(tuples)) = serde_json::from_str::<serde_json::Value>(&raw) else { return true };
    let mut seen: std::collections::HashMap<(String, String), String> = std::collections::HashMap::new();
    let mut conflicts = Vec::new();
    for t in &tuples {
        let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let hash = t.get("hash").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let outcome = t.get("outcome").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() || hash.is_empty() { continue; }
        let key = (id.clone(), hash.clone());
        match seen.get(&key) {
            Some(prior) if *prior != outcome => {
                conflicts.push(format!("{id}@{hash}: {prior} then {outcome}"));
            }
            _ => { seen.insert(key, outcome); }
        }
    }
    if conflicts.is_empty() { return true; }
    crate::wasm_dispatch::emit_event("deviation.non-idempotent-replay", serde_json::json!({
        "conflicts": conflicts,
        "reason": "the same (id, hash) audit tuple was recorded with two different outcomes this stop window -- a replayed dispatch must reach the same result, never a second different mutation",
    }));
    false
}
#[cfg(not(target_arch = "wasm32"))]
fn pred_idempotent_dispatch_replay_safe() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn ci_validation_fresh() -> bool {
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.ci-validated").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() { return false; }
    let current_head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if current_head.is_empty() { return false; }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            let marker_sha = v.get("head_sha").and_then(|s| s.as_str()).unwrap_or("");
            !marker_sha.is_empty() && marker_sha == current_head
        }
        Err(_) => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn ci_validation_fresh() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn working_diff_touched_file_count() -> usize {
    let porcelain_count = {
        let raw = crate::wasm_dispatch::git_porcelain();
        raw.lines().filter(|l| !l.trim().is_empty()).count()
    };
    if porcelain_count > 0 { return porcelain_count; }
    let raw = crate::wasm_dispatch::git_call("show --name-only --format=", None);
    raw.get("stdout").and_then(|s| s.as_str()).unwrap_or("")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}
#[cfg(not(target_arch = "wasm32"))]
fn working_diff_touched_file_count() -> usize { 0 }

#[cfg(target_arch = "wasm32")]
fn split_context_swept() -> bool {
    if working_diff_touched_file_count() <= 1 { return true; }
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.split-context-swept").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() { return false; }
    let current_head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if current_head.is_empty() { return false; }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            let marker_sha = v.get("head_sha").and_then(|s| s.as_str()).unwrap_or("");
            !marker_sha.is_empty() && marker_sha == current_head
        }
        Err(_) => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn split_context_swept() -> bool { true }

#[cfg(target_arch = "wasm32")]
fn check_browser_witness_coverage_for_cwd(cwd: &str) -> Vec<String> {
    let edits_path = if cwd.is_empty() {
        ".gm/exec-spool/.turn-browser-edits.json".to_string()
    } else {
        format!("{}/.gm/exec-spool/.turn-browser-edits.json", cwd.trim_end_matches('/').trim_end_matches('\\'))
    };
    let edits_raw = crate::pkfs::read_to_string(&edits_path).unwrap_or_default();
    if edits_raw.trim().is_empty() { return vec![]; }
    let edits: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(&edits_raw) {
        Ok(serde_json::Value::Array(arr)) => arr,
        _ => return vec![],
    };
    if edits.is_empty() { return vec![]; }
    let witness_path = if cwd.is_empty() {
        ".gm/exec-spool/.turn-browser-witnessed".to_string()
    } else {
        format!("{}/.gm/exec-spool/.turn-browser-witnessed", cwd.trim_end_matches('/').trim_end_matches('\\'))
    };
    let witness_raw = crate::pkfs::read_to_string(&witness_path).unwrap_or_default();
    // browser_witness.rs::record_witness (the ONLY writer of this file) always writes a
    // FLAT {file: hash} object -- it has never written a nested "witnessed_hashes" wrapper.
    // Looking for that wrapper key here meant `.get("witnessed_hashes")` always returned
    // None on a real .turn-browser-witnessed file, so `witnessed_hashes` was ALWAYS empty
    // regardless of how many successful browser/cdp/serp dispatches had rewritten the file,
    // making this predicate permanently unsatisfiable for every previously-edited client
    // file. Accept the flat object directly, and keep the nested-wrapper shape as a
    // fallback in case some other writer or a future schema version does use it.
    let witnessed_hashes: serde_json::Map<String, serde_json::Value> = if witness_raw.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str::<serde_json::Value>(&witness_raw).ok().and_then(|v| {
            match v {
                serde_json::Value::Object(ref m) if m.contains_key("witnessed_hashes") => {
                    m.get("witnessed_hashes").and_then(|w| w.as_object().cloned())
                }
                serde_json::Value::Object(m) => Some(m),
                _ => None,
            }
        }).unwrap_or_default()
    };
    let mut unwitnessed: Vec<String> = vec![];
    for entry in edits.iter() {
        let file = match entry.get("file").and_then(|v| v.as_str()) {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        if !crate::browser_witness::is_browser_running_file(file) { continue; }
        let edit_hash = entry.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        if edit_hash.is_empty() {
            unwitnessed.push(format!("{file} (edit recorded with no readable content hash)"));
            continue;
        }
        let witness_hash = witnessed_hashes.get(file).and_then(|v| v.as_str()).unwrap_or("");
        if witness_hash != edit_hash {
            let kind = if witness_hash.is_empty() {
                "browser-witness-missing"
            } else {
                "browser-witness-hash-mismatch"
            };
            crate::wasm_dispatch::emit_event(&format!("deviation.{kind}"), serde_json::json!({
                "file": file,
                "kind": kind,
                "severity": super::deviations::effective_severity(kind).as_str(),
                "edit_hash": edit_hash,
                "witness_hash": witness_hash,
                "reason": if witness_hash.is_empty() {
                    "this file was edited but never witnessed in a browser dispatch"
                } else {
                    "this file was witnessed, then edited again -- the witness is stale"
                },
            }));
            unwitnessed.push(file.to_string());
        }
    }
    unwitnessed
}
#[cfg(not(target_arch = "wasm32"))]
fn check_browser_witness_coverage_for_cwd(_cwd: &str) -> Vec<String> { vec![] }

pub enum HookOutcome {
    Passed,
    Missing,
    ExecFailed,
    ReturnedFalse,
    Unsupported,
}

impl HookOutcome {
    pub fn passed(&self) -> bool {
        matches!(self, HookOutcome::Passed)
    }

    pub fn reason(&self, hook_path: &str) -> Option<String> {
        match self {
            HookOutcome::Passed => None,
            HookOutcome::Missing => Some(format!(
                "hook `{hook_path}` is MISSING at .gm/instructions/hooks/{hook_path} -- gates fail CLOSED, so a hook that is not there denies forever. Create it, or clear the gate's `hook` field."
            )),
            HookOutcome::ExecFailed => Some(format!(
                "hook `{hook_path}` FAILED TO RUN (threw, timed out, or returned an unreadable result) -- this is a broken hook, not a legitimate denial."
            )),
            HookOutcome::ReturnedFalse => Some(format!(
                "hook `{hook_path}` ran and returned something other than `true` -- this is the hook denying on purpose. Note a hook body needs an explicit `return`; a bare trailing expression is discarded and reads as a denial."
            )),
            HookOutcome::Unsupported => Some(format!(
                "hook `{hook_path}` cannot run on this build (no exec_js host) -- gates fail CLOSED."
            )),
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn hook_outcome(hook_path: &str) -> HookOutcome {
    let Some(full) = fsm::resolve_hook_path(hook_path) else { return HookOutcome::Missing };
    let Some(script) = crate::pkfs::read_to_string(&full) else { return HookOutcome::Missing };
    crate::wasm_dispatch::emit_event("fsm_hook_executing", serde_json::json!({
        "hook_path": hook_path,
        "resolved_path": full,
    }));
    let opts = serde_json::json!({ "timeoutMs": fsm::graph().policy.hook_timeout_ms }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            script.as_ptr(), script.len() as u32,
            opts.as_ptr(), opts.len() as u32,
        )
    };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return HookOutcome::ExecFailed;
    }
    if v.get("result").and_then(|r| r.as_bool()).unwrap_or(false) {
        HookOutcome::Passed
    } else {
        HookOutcome::ReturnedFalse
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn hook_outcome(_hook_path: &str) -> HookOutcome { HookOutcome::Unsupported }

fn hook_result(hook_path: &str) -> bool {
    hook_outcome(hook_path).passed()
}

fn evaluate_gate(g: &GateDef) -> bool {
    match g.hook_mode {
        HookMode::PredicateOnly => g.predicate.as_deref().map(predicate_result).unwrap_or(true),
        HookMode::HookOnly => g.hook.as_deref().map(hook_result).unwrap_or(false),
        HookMode::Both => {
            let pred_ok = g.predicate.as_deref().map(predicate_result).unwrap_or(true);
            let hook_ok = g.hook.as_deref().map(hook_result).unwrap_or(false);
            pred_ok && hook_ok
        }
    }
}

fn gate_rejection(graph: &fsm::Graph, from: &str, to: &str) -> Option<(String, String, i32)> {
    let Some(edge) = graph.edge_between(from, to) else {
        return Some((
            String::new(),
            format!(
                "transition rejected: no edge from `{}` to `{}` in the active FSM graph -- there is no legal direct path between these phases.",
                from, to
            ),
            1,
        ));
    };
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else { continue };
        if !evaluate_gate(g) {
            let detail = hook_denial_detail_or_none_if_predicate_caused_it(g);
            let message = match detail {
                Some(d) => format!("{} -- {}", g.message, d),
                None => g.message.clone(),
            };
            return Some((String::new(), message, 1));
        }
    }
    None
}

fn obligation_dag_gate_kinds(predicate_name: &str) -> Option<&'static [&'static str]> {
    match predicate_name {
        "mutables-all-typed" => Some(mutables::PROVE_OBLIGATION_KINDS),
        "state-obligations-ready" => Some(mutables::STATE_OBLIGATION_KINDS),
        "conc-obligations-ready" => Some(mutables::CONC_OBLIGATION_KINDS),
        "sec-obligations-ready" => Some(mutables::SEC_OBLIGATION_KINDS),
        "res-obligations-ready" => Some(mutables::RES_OBLIGATION_KINDS),
        _ => None,
    }
}

fn hook_denial_detail_or_none_if_predicate_caused_it(g: &GateDef) -> Option<String> {
    if matches!(g.hook_mode, HookMode::PredicateOnly) {
        if let Some(predicate_name) = g.predicate.as_deref() {
            if let Some(kinds) = obligation_dag_gate_kinds(predicate_name) {
                let msg = mutables::obligations_blocker_message(kinds);
                return if msg.is_empty() { None } else { Some(msg) };
            }
        }
        return None;
    }
    let hook_path = g.hook.as_deref()?;
    hook_outcome(hook_path).reason(hook_path)
}

pub fn gate_residuals(from: &str, to: &str) -> (Vec<String>, Option<String>) {
    let graph = fsm::graph();
    let Some(edge) = graph.edge_between(from, to) else {
        return (
            vec![format!("no edge from `{from}` to `{to}` in the active FSM graph -- no legal direct path between these phases")],
            Some("instruction".to_string()),
        );
    };
    let mut residuals = Vec::new();
    let mut next_dispatch: Option<String> = None;
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else { continue };
        if !evaluate_gate(g) {
            residuals.push(match hook_denial_detail_or_none_if_predicate_caused_it(g) {
                Some(d) => format!("{} -- {}", g.message, d),
                None => g.message.clone(),
            });
            if next_dispatch.is_none() {
                next_dispatch = Some(match g.next_dispatch.as_deref() {
                    Some(v) if !v.is_empty() => v,
                    _ => match gate_name.as_str() {
                    "residual-scan-fired" => "residual-scan",
                    "prd-all-closed" => "prd-resolve",
                    "mutables-all-resolved" => "mutable-resolve",
                    "mutables-all-typed" => "mutable-add",
                    "state-obligations-ready" => "mutable-add",
                    "conc-obligations-ready" => "mutable-add",
                    "sec-obligations-ready" => "mutable-add",
                    "res-obligations-ready" => "mutable-add",
                    "worktree-clean" => "git_finalize",
                    "ci-validated-fresh" => "ci-status",
                    "browser-witness-coverage" => "browser",
                    "app-loads-witnessed" => "browser",
                    "claim-audit-clean" => "claim-audit",
                    "submodules-clean" => "git_add",
                        _ => "instruction",
                    },
                }.to_string());
            }
        }
    }
    (residuals, next_dispatch)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle(_content: &str) -> (String, String, i32) {
    ("{\"ok\":false,\"error\":\"transition requires wasm32\"}".to_string(), String::new(), 1)
}

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id: Option<String> = None;
    let graph = fsm::graph();
    let cur = read_state_with_graph(&graph);
    let cur_phase = cur.phase.clone();
    let target = if trimmed.is_empty() {
        next_phase(&cur_phase, &graph)
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
            session_id = Some(sid.to_string());
        }
        let to_str = v.get("to").and_then(|s| s.as_str())
            .or_else(|| v.get("phase").and_then(|s| s.as_str()))
            .or_else(|| v.as_str());
        match to_str {
            Some(s) => match Phase::parse(s) {
                Some(p) => p,
                None => return (String::new(), format!("invalid phase: {}", s), 1),
            },
            None => next_phase(&cur_phase, &graph),
        }
    } else {
        match Phase::parse(trimmed) {
            Some(p) => p,
            None => return (String::new(), format!("invalid phase: {}", trimmed), 1),
        }
    };

    if !graph.has_state(target.as_str()) {
        return (
            String::new(),
            format!(
                "transition rejected: `{}` is not a state in the active FSM graph (states: {}). A custom graph must declare every phase it uses -- see .gm/instructions/fsm/graph.json.",
                target.as_str(),
                graph.states.iter().map(|s| s.key.as_str()).collect::<Vec<_>>().join(", ")
            ),
            1,
        );
    }

    if let Some(r) = gate_rejection(&graph, cur_phase.as_str(), target.as_str()) {
        return r;
    }

    let skill = next_skill(&target, &graph);
    match set_phase_with_session_with_graph(target.clone(), Some(skill.clone()), session_id, &graph) {
        Ok(s) => {
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event("phase.transitioned", serde_json::json!({ "from": cur_phase.as_str(), "phase": s.phase.as_str() }));
            #[cfg(target_arch = "wasm32")]
            if s.phase.as_str().eq_ignore_ascii_case(fsm::graph().policy.terminal_phase.as_str()) {
                let receipt = crate::evidence_receipt::write();
                crate::wasm_dispatch::emit_event("evidence.receipt", receipt);
            }
            let query = {
                let (body, _err, code) = prd::handle_list("");
                if code == 0 {
                    serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("items").cloned())
                        .and_then(|v| v.as_array().cloned())
                        .and_then(|arr| {
                            arr.iter().find(|it| {
                                let status = it.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                                prd::status_is_open(status)
                            }).cloned()
                        })
                        .and_then(|it| it.get("subject").and_then(|v| v.as_str()).map(|s| s.to_string()))
                        .unwrap_or_default()
                } else { String::new() }
            };
            let combined = if query.is_empty() { s.phase.as_str().to_string() } else { format!("{} {}", s.phase.as_str(), query) };
            let hits = recall::recall_hits(&combined, crate::ragconfig::InstructionPayloadConfig::default().transition_recall_hits);
            let payload = serde_json::json!({
                "phase": s.phase.as_str(),
                "phase_label": skill,
                "recall_hits": hits,
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("write state failed: {}", e), 1),
    }
}

/// Reverts the most recent phase transition. Body is ignored (no fields
/// take arguments); each dispatch pops exactly one history entry, so
/// reverting a multi-step reshaping means dispatching this once per step.
pub fn handle_revert(_content: &str) -> (String, String, i32) {
    match super::state::revert_last_transition() {
        Ok(s) => {
            let payload = serde_json::json!({ "phase": s.phase.as_str() });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("transition-revert failed: {}", e), 1),
    }
}
