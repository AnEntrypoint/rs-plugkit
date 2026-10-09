use super::fsm::{self, GateDef, HookMode};
use super::mutables;
use super::prd;
#[cfg(target_arch = "wasm32")]
use super::recall;
use super::predicate_registry::{PredicateFn, PREDICATE_REGISTRY};
use super::state::Phase;
#[cfg(target_arch = "wasm32")]
use super::state::{read_state_with_graph, set_phase_with_session_with_graph};

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
    predicate_table()
        .iter()
        .map(|(name, desc, _)| (*name, *desc))
        .collect()
}

pub fn handle_predicates_md(_content: &str) -> (String, String, i32) {
    let md = super::predicate_registry::generated_predicates_md();
    let payload = serde_json::json!({
        "predicates_md": md,
        "predicate_count": predicate_table().len(),
    });
    (payload.to_string(), String::new(), 0)
}

fn predicate_table() -> &'static [(&'static str, &'static str, PredicateFn)] {
    PREDICATE_REGISTRY
}

pub(super) fn pred_remote_hook_refused() -> bool {
    false
}

pub(super) fn pred_prd_all_closed() -> bool {
    !prd_has_open_items()
}
pub(super) fn pred_mutables_all_resolved() -> bool {
    mutables::pending_detailed().is_empty()
}
pub(super) fn pred_mutables_all_typed() -> bool {
    mutables::all_typed()
}
pub(super) fn pred_state_obligations_ready() -> bool {
    mutables::state_obligations_ready()
}
pub(super) fn pred_conc_obligations_ready() -> bool {
    mutables::conc_obligations_ready()
}
pub(super) fn pred_sec_obligations_ready() -> bool {
    mutables::sec_obligations_ready()
}
pub(super) fn pred_res_obligations_ready() -> bool {
    mutables::res_obligations_ready()
}
pub(super) fn pred_worktree_clean() -> bool {
    !worktree_dirty()
}
#[cfg(target_arch = "wasm32")]
pub(super) fn pred_claim_audit_clean() -> bool {
    crate::wasm_dispatch::host_abi::git_repository_absent()
        || super::claim_audit::claim_audit_clean()
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_claim_audit_clean() -> bool {
    true
}
pub(super) fn pred_submodules_clean() -> bool {
    super::submodule_drift::submodules_clean()
}

pub(super) fn pred_pool_floor_met() -> bool {
    super::pool_slots::slot_state(".")["action"].as_str() != Some("launch")
}

fn pool_floor_denial_detail() -> String {
    let slots = super::pool_slots::slot_state(".");
    format!(
        "pool-floor-met denied: open_rows={} live={} free={} action={}; launch gm-worker subagents for the candidates in .gm/pool slots, and on a spawn refusal dispatch pool-observe with its text.",
        slots["open_rows"], slots["live"], slots["free"], slots["action"]
    )
}

fn lean_prd_items() -> Option<Vec<serde_json::Value>> {
    let (body, _err, code) = prd::handle_list_full();
    if code != 0 {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()?
        .get("items")?
        .as_array()
        .cloned()
}

fn lean_row_status(item: &serde_json::Value) -> String {
    item.get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
}

pub(super) fn pred_lean_one_task_in_flight() -> bool {
    let Some(items) = lean_prd_items() else {
        return false;
    };
    items
        .iter()
        .filter(|it| lean_row_status(it) == "in-progress")
        .count()
        <= 1
}

pub(super) fn pred_lean_always_true() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
fn lean_worktree_clean() -> bool {
    let mut argv: Vec<&str> = vec!["status", "--porcelain", "--"];
    argv.extend(crate::wasm_dispatch::GIT_PROTECTED_PATHSPECS.iter().map(|(_, spec)| *spec));
    argv.push(":/");
    let st = crate::wasm_dispatch::host_abi::porcelain_from(&crate::wasm_dispatch::git_call_argv(
        &argv,
        None,
    ));
    !st.partial && st.porcelain.trim().is_empty()
}
#[cfg(not(target_arch = "wasm32"))]
fn lean_worktree_clean() -> bool {
    false
}

pub(super) fn pred_lean_contract_recorded() -> bool {
    let Some(items) = lean_prd_items() else {
        return false;
    };
    if items
        .iter()
        .any(|it| prd::status_is_open(&lean_row_status(it)))
    {
        return false;
    }
    lean_worktree_clean()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_lean_net_negative() -> bool {
    let st = crate::wasm_dispatch::host_abi::porcelain_from(&crate::wasm_dispatch::git_call(
        "diff --numstat HEAD",
        None,
    ));
    if st.partial {
        return false;
    }
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for line in st.porcelain.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = line.splitn(3, '\t');
        let (Some(a), Some(d), Some(_path)) = (cols.next(), cols.next(), cols.next()) else {
            return false;
        };
        if a == "-" && d == "-" {
            continue;
        }
        let (Ok(a), Ok(d)) = (a.parse::<u64>(), d.parse::<u64>()) else {
            return false;
        };
        added = added.saturating_add(a);
        removed = removed.saturating_add(d);
    }
    added <= removed
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_lean_net_negative() -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
fn advisory_messages(graph: &fsm::Graph, from: &str, to: &str) -> Vec<String> {
    let Some(edge) = graph.transition_edge(from, to) else {
        return Vec::new();
    };
    edge.gates
        .iter()
        .filter_map(|name| graph.gate(name))
        .filter(|g| g.advisory)
        .map(|g| g.message.clone())
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn emit_unknown_predicate(other: &str) {
    crate::wasm_dispatch::emit_event(
        "fsm_unknown_predicate",
        serde_json::json!({
            "predicate": other,
            "reason": "not in transitions::known_predicates(); this gate can never be satisfied. Fix the name in .gm/instructions/fsm/graph.json (see fsm/predicates.md for the valid set) or use a jit hook for a condition that has no compiled predicate.",
        }),
    );
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

#[cfg(target_arch = "wasm32")]
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
pub(super) fn residual_scan_fired() -> bool {
    match super::yaml_util::read_residual_marker() {
        super::yaml_util::ResidualMarker::Live { session_id, fired_at_ms } =>
            residual_scan_marker_matches_current_session_or_is_within_longgap_threshold(&session_id, fired_at_ms),
        _ => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn residual_scan_fired() -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
fn residual_scan_denial_detail() -> String {
    use super::yaml_util::ResidualMarker;
    let marker_path = super::yaml_util::residual_marker_path();
    match super::yaml_util::read_residual_marker() {
        ResidualMarker::Absent => format!(
            "`{}` does not exist or is empty: no `residual-scan` has ever reached the fired state in this project. A response of `scan: \"skipped\"` (PRD open, task running, dirty tree) returns exit code 0 but does NOT write the marker -- clear that skip reason, then re-dispatch `residual-scan`.",
            marker_path
        ),
        ResidualMarker::Live { session_id, fired_at_ms } => {
            let current_sid = super::state::read_state().session_id.unwrap_or_default();
            if !session_id.is_empty() && !current_sid.is_empty() && session_id != current_sid {
                return format!(
                    "`{}` records a fired scan from session `{}`, but this session is `{}` -- the marker is not reused across sessions. Re-dispatch `residual-scan` in this session.",
                    marker_path, session_id, current_sid
                );
            }
            let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() };
            let threshold_ms = super::fsm::graph().policy.longgap_threshold_ms;
            format!(
                "`{}` records a fired scan with no usable session id, stamped {}ms ago, past the {}ms longgap threshold. Re-dispatch `residual-scan`.",
                marker_path,
                now_ms.saturating_sub(fired_at_ms),
                threshold_ms
            )
        }
        ResidualMarker::Invalidated { reason } => format!(
            "a `residual-scan` DID fire in this session, and then `{}` invalidated its marker afterwards -- `.gm/residual-check-fired` is deliberately invalidated by every PRD/mutable write, so the scan is stale rather than missing. Re-dispatch `residual-scan` as the LAST verb before `transition`, with no prd-add / mutable-add / prd-defer / mutable-defer in between.",
            reason
        ),
        ResidualMarker::Malformed { raw } => format!(
            "`{}` is present but does not read as `<session_id>:<fired_at_ms>` (contents: `{}`). Re-dispatch `residual-scan`.",
            marker_path, raw
        ),
    }
}

pub(super) fn prd_open_rows() -> Vec<serde_json::Value> {
    let (body, _err, code) = prd::handle_list_full();
    if code != 0 { return Vec::new(); }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else { return Vec::new() };
    let Some(items) = v.get("items").and_then(|v| v.as_array()) else { return Vec::new() };
    items
        .iter()
        .filter(|it| {
            let status = it
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let blocked_external = it
                .get("blockedBy")
                .and_then(|v| v.as_array())
                .map(|seq| seq.iter().any(|x| x.as_str() == Some("external")))
                .unwrap_or(false);
            prd::status_is_open(status) && !blocked_external
        })
        .cloned()
        .collect()
}

fn prd_has_open_items() -> bool {
    !prd_open_rows().is_empty()
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


#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_synthetic_test_files() -> bool {
    let found = synthetic_test_files_added_in_working_diff();
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.synthetic-test-file",
        serde_json::json!({
            "files": found,
            "reason": "VERIFY doctrine forbids standing test files: delete them and replace their assertions with a live exec_js witness, then re-verify",
        }),
    );
    false
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_synthetic_test_files() -> bool {
    true
}
#[cfg(not(target_arch = "wasm32"))]
fn worktree_dirty() -> bool {
    false
}

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
                let num_part = plus
                    .split(|c: char| c == ',' || c == ' ')
                    .next()
                    .unwrap_or("0");
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

#[cfg(target_arch = "wasm32")]
fn is_test_scoped_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
}

/// Best-effort brace-balance scope walk: starting just before a `throw` statement,
/// scan the preceding file text backward one enclosing block at a time. A block
/// whose opening-brace text names a `catch` (`catch (...) {` or `.catch(...) {`)
/// means the throw is lexically handled. A block that itself opens a function
/// (`function ...`/`=>`) with no `catch` in its own opener ends the walk at that
/// function's own scope boundary -- an outer catch cannot lexically reach across it.
/// Character-level (not line-level) so a same-line `} catch (e) {` pairs its own
/// `{` to the throw's block without misreading the leading `}` of the closed `try`.
#[cfg(target_arch = "wasm32")]
fn js_throw_has_enclosing_catch(full_text: &str, throw_line_no: usize) -> bool {
    let lines: Vec<&str> = full_text.lines().collect();
    if throw_line_no == 0 || throw_line_no > lines.len() {
        return false;
    }
    let throw_line = lines[throw_line_no - 1];
    let throw_pos_in_line = throw_line.find("throw").unwrap_or(0);
    let mut preceding = String::new();
    for l in &lines[..throw_line_no - 1] {
        preceding.push_str(l);
        preceding.push('\n');
    }
    preceding.push_str(&throw_line[..throw_pos_in_line]);
    let chars: Vec<char> = preceding.chars().collect();

    let mut cursor = chars.len();
    let mut hops = 0;
    loop {
        hops += 1;
        if hops > 25 {
            return false;
        }
        let mut depth: i32 = 0;
        let mut open_idx: Option<usize> = None;
        let mut j = cursor;
        while j > 0 {
            j -= 1;
            match chars[j] {
                '}' => depth += 1,
                '{' => {
                    if depth == 0 {
                        open_idx = Some(j);
                        break;
                    } else {
                        depth -= 1;
                    }
                }
                _ => {}
            }
        }
        let Some(idx) = open_idx else { return false };
        let window_start = idx.saturating_sub(80);
        let window: String = chars[window_start..idx].iter().collect();
        if window.contains("catch") {
            return true;
        }
        if window.contains("function") || window.contains("=>") {
            return false;
        }
        cursor = idx;
    }
}

#[cfg(target_arch = "wasm32")]
fn needle_first_occurrence_sits_inside_quoted_string_literal(text: &str, needle: &str) -> bool {
    let Some(idx) = text.find(needle) else {
        return false;
    };
    let before = &text[..idx];
    let d = before.matches('"').count();
    let s = before.matches('\'').count();
    let b = before.matches('`').count();
    d % 2 == 1 || s % 2 == 1 || b % 2 == 1
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_admit_deferral_markers() -> bool {
    const COLON_MARKERS: &[&str] = &[
        "TODO:",
        "FIXME:",
        "XXX:",
        "HACK:",
        "todo!(",
        "unimplemented!(",
    ];
    const PHRASES: &[&str] = &["not implemented", "not yet implemented"];
    const SOURCE_EXTS: &[&str] = &[
        ".rs", ".js", ".ts", ".jsx", ".tsx", ".mjs", ".cjs", ".py", ".go", ".java", ".c", ".cc",
        ".cpp", ".h", ".hpp", ".sh", ".ps1", ".vue", ".svelte",
    ];
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if !SOURCE_EXTS.iter().any(|e| path.ends_with(e)) {
            continue;
        }
        let upper = text.to_ascii_uppercase();
        let colon_hit = COLON_MARKERS.iter().any(|m| {
            let mu = m.to_ascii_uppercase();
            upper.contains(&mu)
                && !needle_first_occurrence_sits_inside_quoted_string_literal(&upper, &mu)
        });
        let lower = text.to_ascii_lowercase();
        let phrase_hit = PHRASES.iter().any(|p| {
            lower.contains(p)
                && !needle_first_occurrence_sits_inside_quoted_string_literal(&lower, p)
        });
        if colon_hit || phrase_hit {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.admit-deferral-marker",
        serde_json::json!({
            "lines": found,
            "reason": "an admit/deferral marker in a source file stands in for a complete proof -- finish the work or remove the marker, then re-attempt",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_admit_deferral_markers() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_secrets_in_diff() -> bool {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        let looks_like_aws_key = text.contains("AKIA")
            && text.matches(|c: char| c.is_ascii_alphanumeric()).count() >= 20;
        let looks_like_private_key = text.contains("-----BEGIN") && text.contains("PRIVATE KEY");
        let looks_like_db_url_with_password = (text.contains("://") && text.contains('@'))
            && (text.contains("postgres")
                || text.contains("mysql")
                || text.contains("mongodb")
                || text.contains("redis"))
            && text.contains(':')
            && !text.contains("<")
            && !text.contains("${")
            && !text.contains("%s");
        let lower = text.to_ascii_lowercase();
        let looks_like_bearer_literal = (lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("bearer ")
            || lower.contains("secret_key"))
            && text.contains('"')
            && text.matches(|c: char| c.is_ascii_alphanumeric()).count() >= 24
            && !lower.contains("process.env")
            && !lower.contains("env::var")
            && !lower.contains("getenv");
        if looks_like_aws_key
            || looks_like_private_key
            || looks_like_db_url_with_password
            || looks_like_bearer_literal
        {
            let redacted: String = text.chars().take(20).collect();
            found.push(format!("{path}:{line_no}: {redacted}... (redacted)"));
        }
    }
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.secret-in-diff",
        serde_json::json!({
            "lines": found,
            "reason": "a line in the working diff matches a high-confidence secret shape -- remove the literal, route it through an env var or secret store, then re-attempt",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_secrets_in_diff() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_unchecked_panics_in_diff() -> bool {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if is_test_scoped_path(&path) {
            continue;
        }
        let trimmed = text.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let is_rust = path.ends_with(".rs");
        let is_js_like = path.ends_with(".js")
            || path.ends_with(".ts")
            || path.ends_with(".jsx")
            || path.ends_with(".tsx")
            || path.ends_with(".mjs")
            || path.ends_with(".cjs");
        if is_rust {
            let has_unwrap = text.contains(".unwrap()") && !text.contains("unwrap_or");
            let has_expect = text.contains(".expect(");
            let has_panic = text.contains("panic!(");
            if has_unwrap || has_expect || has_panic {
                found.push(format!("{path}:{line_no}: {trimmed}"));
            }
        } else if is_js_like {
            if trimmed.starts_with("throw ") && !trimmed.contains("//") {
                let handled = crate::pkfs::read_to_string(&path)
                    .map(|content| js_throw_has_enclosing_catch(&content, line_no))
                    .unwrap_or(false);
                if !handled {
                    found.push(format!("{path}:{line_no}: {trimmed}"));
                }
            }
        }
    }
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.unchecked-panic",
        serde_json::json!({
            "lines": found,
            "reason": "a new line panics/throws/unwraps outside a test path with no visible handling -- propagate the error explicitly (Result/catch) or justify the panic as a real precondition violation, then re-attempt",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_unchecked_panics_in_diff() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_hedge_language_in_diff() -> bool {
    const HEDGES: &[&str] = &[
        "todo later",
        "in a future session",
        "for now we",
        "as a stopgap",
        "good enough for now",
        "left as an exercise",
        "out of scope for this",
        "not yet implemented",
        "we'll come back to",
    ];
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if !path.ends_with(".md") {
            continue;
        }
        let lower = text.to_ascii_lowercase();
        if HEDGES.iter().any(|h| lower.contains(h)) {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.hedge-language",
        serde_json::json!({
            "lines": found,
            "reason": "a hedge/deferral phrase in touched prose stands in for a decision -- commit to the real answer or remove the hedge, then re-attempt",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_hedge_language_in_diff() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
fn graphical_symbol_lines_in_diff() -> Vec<String> {
    let mut found = Vec::new();
    for (path, line_no, text) in added_lines_in_diff() {
        if path.ends_with("CHANGELOG.md") {
            continue;
        }
        let has_glyph = text.chars().any(|c| {
            let cp = c as u32;
            matches!(cp, 0x2190..=0x21FF | 0x2500..=0x257F | 0x2600..=0x27BF | 0x1F300..=0x1FAFF | 0x2B00..=0x2BFF)
        });
        if has_glyph {
            found.push(format!("{path}:{line_no}: {}", text.trim()));
        }
    }
    found
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_no_graphical_symbols_in_diff() -> bool {
    let found = graphical_symbol_lines_in_diff();
    if found.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.graphical-symbol",
        serde_json::json!({
            "lines": found,
            "reason": "a decorative non-ASCII glyph landed in tracked source/prose -- convert to its plain-ASCII equivalent (->, -/*, [x]/[ ], done/todo/pass/fail), then re-attempt",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_no_graphical_symbols_in_diff() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(super) fn pred_idempotent_dispatch_replay_safe() -> bool {
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.audit-tuples.json").unwrap_or_default();
    if raw.trim().is_empty() {
        return true;
    }
    let Ok(serde_json::Value::Array(tuples)) = serde_json::from_str::<serde_json::Value>(&raw)
    else {
        return true;
    };
    let mut seen: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    let mut conflicts = Vec::new();
    for t in &tuples {
        let id = t
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let hash = t
            .get("hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let outcome = t
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() || hash.is_empty() {
            continue;
        }
        let key = (id.clone(), hash.clone());
        match seen.get(&key) {
            Some(prior) if *prior != outcome => {
                conflicts.push(format!("{id}@{hash}: {prior} then {outcome}"));
            }
            _ => {
                seen.insert(key, outcome);
            }
        }
    }
    if conflicts.is_empty() {
        return true;
    }
    crate::wasm_dispatch::emit_event(
        "deviation.non-idempotent-replay",
        serde_json::json!({
            "conflicts": conflicts,
            "reason": "the same (id, hash) audit tuple was recorded with two different outcomes this stop window -- a replayed dispatch must reach the same result, never a second different mutation",
        }),
    );
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn pred_idempotent_dispatch_replay_safe() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
pub(super) fn ci_validation_fresh() -> bool {
    if crate::wasm_dispatch::host_abi::git_repository_absent() {
        return true;
    }
    let raw = crate::pkfs::read_to_string(".gm/exec-spool/.ci-validated").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let current_head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if current_head.is_empty() {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            let marker_sha = v.get("head_sha").and_then(|s| s.as_str()).unwrap_or("");
            !marker_sha.is_empty() && marker_sha == current_head
        }
        Err(_) => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn ci_validation_fresh() -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
fn working_diff_touched_file_count() -> usize {
    let porcelain_count = {
        let raw = crate::wasm_dispatch::git_porcelain();
        raw.lines().filter(|l| !l.trim().is_empty()).count()
    };
    if porcelain_count > 0 {
        return porcelain_count;
    }
    let raw = crate::wasm_dispatch::git_call("show --name-only --format=", None);
    raw.get("stdout")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn split_context_swept() -> bool {
    if working_diff_touched_file_count() <= 1 {
        return true;
    }
    let raw =
        crate::pkfs::read_to_string(".gm/exec-spool/.split-context-swept").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let current_head = crate::wasm_dispatch::git_call("rev-parse HEAD", None)
        .get("stdout")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if current_head.is_empty() {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => {
            let marker_sha = v.get("head_sha").and_then(|s| s.as_str()).unwrap_or("");
            !marker_sha.is_empty() && marker_sha == current_head
        }
        Err(_) => false,
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn split_context_swept() -> bool {
    true
}

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
    let Some(full) = fsm::resolve_hook_path(hook_path) else {
        return HookOutcome::Missing;
    };
    let Some(script) = crate::pkfs::read_to_string(&full) else {
        return HookOutcome::Missing;
    };
    crate::wasm_dispatch::emit_event(
        "fsm_hook_executing",
        serde_json::json!({
            "hook_path": hook_path,
            "resolved_path": full,
        }),
    );
    let opts = serde_json::json!({ "timeoutMs": fsm::graph().policy.hook_timeout_ms }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            script.as_ptr(),
            script.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
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
fn hook_outcome(_hook_path: &str) -> HookOutcome {
    HookOutcome::Unsupported
}

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

#[cfg(target_arch = "wasm32")]
fn predicate_detail(predicate_name: Option<&str>) -> Option<String> {
    match predicate_name {
        Some("no-graphical-symbols-in-diff") => {
            let lines = graphical_symbol_lines_in_diff();
            (!lines.is_empty()).then(|| lines.join("; "))
        }
        Some("residual-scan-fired") => {
            let detail = residual_scan_denial_detail();
            (!detail.is_empty()).then_some(detail)
        }
        Some("pool-floor-met") => Some(pool_floor_denial_detail()),
        _ => None,
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn predicate_detail(_predicate_name: Option<&str>) -> Option<String> {
    None
}

fn entry_refusal(graph: &fsm::Graph, from: &str, to: &str) -> Option<String> {
    graph.is_entry_state(to).then(|| {
        format!(
            "transition rejected: entry state `{}` can only be entered from the terminal state `{}`, not from `{}`.",
            to, graph.policy.terminal_phase, from
        )
    })
}

#[cfg(target_arch = "wasm32")]
fn gate_rejection(graph: &fsm::Graph, from: &str, to: &str) -> Option<(String, String, i32)> {
    let Some(edge) = graph.transition_edge(from, to) else {
        let message = entry_refusal(graph, from, to).unwrap_or_else(|| {
            format!(
                "transition rejected: no edge from `{}` to `{}` in the active FSM graph -- there is no legal direct path between these phases.",
                from, to
            )
        });
        return Some((String::new(), message, 1));
    };
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else {
            continue;
        };
        if g.advisory {
            continue;
        }
        if !evaluate_gate(g) {
            let detail = hook_denial_detail_or_none_if_predicate_caused_it(g)
                .or_else(|| predicate_detail(g.predicate.as_deref()));
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
    let Some(edge) = graph.transition_edge(from, to) else {
        let residual = entry_refusal(&graph, from, to).unwrap_or_else(|| {
            format!("no edge from `{from}` to `{to}` in the active FSM graph -- no legal direct path between these phases")
        });
        return (vec![residual], Some("instruction".to_string()));
    };
    let mut residuals = Vec::new();
    let mut next_dispatch: Option<String> = None;
    for gate_name in &edge.gates {
        let Some(g) = graph.gate(gate_name) else {
            continue;
        };
        if g.advisory {
            continue;
        }
        if !evaluate_gate(g) {
            residuals.push(match hook_denial_detail_or_none_if_predicate_caused_it(g)
                .or_else(|| predicate_detail(g.predicate.as_deref()))
            {
                Some(d) => format!("{} -- {}", g.message, d),
                None => g.message.clone(),
            });
            if next_dispatch.is_none() {
                next_dispatch = Some(
                    match g.next_dispatch.as_deref() {
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
                            "claim-audit-clean" => "claim-audit",
                            "submodules-clean" => "git_add",
                            _ => "instruction",
                        },
                    }
                    .to_string(),
                );
            }
        }
    }
    (residuals, next_dispatch)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn handle(_content: &str) -> (String, String, i32) {
    (
        "{\"ok\":false,\"error\":\"transition requires wasm32\"}".to_string(),
        String::new(),
        1,
    )
}

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let mut session_id: Option<String> = None;
    let (graph, graph_tier, graph_path) = fsm::graph_detailed();
    if let Some(refusal) = fsm::configured_graph_refusal(graph_tier) {
        return (String::new(), refusal, 1);
    }
    let cur = read_state_with_graph(&graph);
    let cur_phase = cur.phase.clone();
    let target = if trimmed.is_empty() {
        next_phase(&cur_phase, &graph)
    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(sid) = crate::validation::session_id_from_body(&v) {
            session_id = Some(sid);
        }
        let to_str = v
            .get("to")
            .and_then(|s| s.as_str())
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
    let advisory = advisory_messages(&graph, cur_phase.as_str(), target.as_str());
    let edge = graph
        .transition_edge(cur_phase.as_str(), target.as_str())
        .map(|e| {
            serde_json::json!({
                "from": e.from,
                "to": e.to,
                "kind": e.kind,
                "label": e.label,
                "phase": e.phase,
            })
        });

    let skill = next_skill(&target, &graph);
    match set_phase_with_session_with_graph(target.clone(), Some(skill.clone()), session_id, &graph)
    {
        Ok(s) => {
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event(
                "phase.transitioned",
                serde_json::json!({ "from": cur_phase.as_str(), "phase": s.phase.as_str() }),
            );
            #[cfg(target_arch = "wasm32")]
            if s.phase
                .as_str()
                .eq_ignore_ascii_case(fsm::graph().policy.terminal_phase.as_str())
            {
                let receipt = crate::evidence_receipt::write();
                crate::wasm_dispatch::emit_event("evidence.receipt", receipt);
            }
            let query = {
                let (body, _err, code) = prd::handle_list_full();
                if code == 0 {
                    serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("items").cloned())
                        .and_then(|v| v.as_array().cloned())
                        .and_then(|arr| {
                            arr.iter()
                                .find(|it| {
                                    let status = it
                                        .get("status")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("pending");
                                    prd::status_is_open(status)
                                })
                                .cloned()
                        })
                        .and_then(|it| {
                            it.get("subject")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            };
            let combined = if query.is_empty() {
                s.phase.as_str().to_string()
            } else {
                format!("{} {}", s.phase.as_str(), query)
            };
            let hits = recall::recall_hits(
                &combined,
                crate::ragconfig::InstructionPayloadConfig::default().transition_recall_hits,
            );
            let payload = serde_json::json!({
                "phase": s.phase.as_str(),
                "phase_label": skill,
                "graph_tier": graph_tier.as_str(),
                "graph_path": graph_path,
                "edge": edge,
                "advisory": advisory,
                "recall_hits": crate::recall_compact::compact_hits(&hits, false),
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("write state failed: {}", e), 1),
    }
}

pub fn handle_revert(_content: &str) -> (String, String, i32) {
    match super::state::revert_last_transition() {
        Ok(s) => {
            let payload = serde_json::json!({ "phase": s.phase.as_str() });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("transition-revert failed: {}", e), 1),
    }
}
