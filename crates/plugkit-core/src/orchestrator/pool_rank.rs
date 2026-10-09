use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::HashSet;

const BROWSER_WORDS: [&str; 6] = ["cdp", "browser", "chrome", "headful", "live page", "boot"];
const GPU_WORDS: [&str; 9] = ["gpu", "webgpu", "amd", "nvidia", "accelerated", "gpulock", "frame-time", "p50", "dpr"];
const DESIGN_WORDS: [&str; 4] = ["design decision", "cluster-enabled", "circumnavigat", "planet wrap"];
const OUTCOME_KINDS: [&str; 2] = ["outcome", "witness-outcome"];

fn text_field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn row_id(row: &Value) -> Option<&str> {
    text_field(row, "id")
}

fn row_text(row: &Value) -> String {
    ["title", "subject", "body", "text", "description", "acceptance_criteria"]
        .iter()
        .filter_map(|key| text_field(row, key))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn mentions_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| {
        text.match_indices(word)
            .any(|(at, _)| at == 0 || !text.as_bytes()[at - 1].is_ascii_alphanumeric())
    })
}

fn needs_design(row: &Value) -> bool {
    let explicit: Vec<bool> = ["decision", "needs_design"]
        .iter()
        .filter_map(|key| row.get(*key).and_then(Value::as_bool))
        .collect();
    if explicit.is_empty() {
        mentions_any(&row_text(row), &DESIGN_WORDS)
    } else {
        explicit.contains(&true)
    }
}

fn arm(row: &Value) -> Option<&'static str> {
    if let Some(explicit) = text_field(row, "arm").filter(|a| !a.is_empty()) {
        return match explicit {
            "gpu" => Some("gpu"),
            "browser" => Some("browser"),
            _ => None,
        };
    }
    let text = row_text(row);
    if mentions_any(&text, &BROWSER_WORDS) {
        Some("browser")
    } else if mentions_any(&text, &GPU_WORDS) {
        Some("gpu")
    } else {
        None
    }
}

pub fn is_blocker_id(id: &str) -> bool {
    id.to_ascii_lowercase().contains("blocker")
}

pub fn is_blocker_row(row: &Value) -> bool {
    row_id(row).is_some_and(is_blocker_id)
        || text_field(row, "subject")
            .is_some_and(|s| s.trim_start().to_ascii_uppercase().starts_with("BLOCKER"))
}

fn blocked_row_of(blocker_id: &str) -> Option<&str> {
    blocker_id.split("-blocker-").next().filter(|p| !p.is_empty())
}

fn has_id_segment(id: &str, segment: &str) -> bool {
    id.split('-').any(|part| part == segment)
}

fn is_outcome_row(row: &Value) -> bool {
    let by_id = row_id(row).is_some_and(|id| has_id_segment(id, "outcome") || id.contains("outcome-hop"));
    let by_kind = text_field(row, "kind")
        .is_some_and(|kind| OUTCOME_KINDS.iter().any(|known| kind.trim().eq_ignore_ascii_case(known)));
    by_id || by_kind
}

fn is_refuted_row(row: &Value) -> bool {
    let by_id = row_id(row).is_some_and(|id| has_id_segment(id, "refuted"));
    let by_title = ["title", "subject"]
        .iter()
        .filter_map(|key| text_field(row, key))
        .any(|text| text.trim_start().to_ascii_uppercase().starts_with("REFUTED"));
    by_id || by_title
}

fn severity_rank(row: &Value) -> u8 {
    match text_field(row, "severity").map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("critical" | "p0") => 4,
        Some("high" | "p1") => 3,
        Some("low" | "p3") => 1,
        _ => 2,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Ranked {
    arm_rank: u8,
    pending_blocked: bool,
    severity: Reverse<u8>,
    recency: Reverse<usize>,
    id: String,
    arm: Option<&'static str>,
}

pub fn rank(work: &[(Value, usize)], blockers: &[Value], live_rows: &[String]) -> Value {
    let pending_blocked: Vec<&str> = blockers
        .iter()
        .filter_map(row_id)
        .filter_map(blocked_row_of)
        .collect();
    let is_live = |id: &str| live_rows.iter().any(|r| r == id);
    let live_arms: Vec<&str> = work
        .iter()
        .filter(|(row, _)| row_id(row).is_some_and(is_live))
        .filter_map(|(row, _)| arm(row))
        .collect();
    let mut gpu_offered = live_arms.contains(&"gpu");
    let mut browser_offered = live_arms.contains(&"browser");
    let (mut gpu, mut browser, mut design, mut node, mut blocked) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut outcome, mut refuted) = (0usize, 0usize);
    let mut ranked: Vec<Ranked> = Vec::new();
    for (row, recency) in work {
        let Some(id) = row_id(row) else { continue };
        let is_pending_blocked = pending_blocked.contains(&id);
        if is_pending_blocked {
            blocked += 1;
        }
        let row_arm = arm(row);
        match row_arm {
            Some("gpu") => gpu += 1,
            Some("browser") => browser += 1,
            _ => {}
        }
        if needs_design(row) {
            design += 1;
            continue;
        }
        if row_arm.is_none() {
            node += 1;
        }
        if is_outcome_row(row) {
            outcome += 1;
            continue;
        }
        if is_refuted_row(row) {
            refuted += 1;
            continue;
        }
        if is_live(id) {
            continue;
        }
        ranked.push(Ranked {
            arm_rank: u8::from(row_arm.is_some()),
            pending_blocked: is_pending_blocked,
            severity: Reverse(severity_rank(row)),
            recency: Reverse(*recency),
            id: id.to_string(),
            arm: row_arm,
        });
    }
    ranked.sort();
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<String> = Vec::new();
    for entry in ranked {
        match entry.arm {
            Some("gpu") if gpu_offered => continue,
            Some("gpu") => gpu_offered = true,
            Some("browser") if browser_offered => continue,
            Some("browser") => browser_offered = true,
            _ => {}
        }
        if seen.insert(entry.id.clone()) {
            candidates.push(entry.id);
        }
    }
    json!({
        "candidates": candidates,
        "supply": {
            "open_work": work.len(),
            "pending_blocker_rows": blockers.len(),
            "rows_with_pending_blocker": blocked,
            "node_rows": node,
            "gpu_arm_rows": gpu,
            "browser_arm_rows": browser,
            "design_decision_rows": design,
            "excluded_outcome_rows": outcome,
            "excluded_refuted_rows": refuted,
        },
    })
}
