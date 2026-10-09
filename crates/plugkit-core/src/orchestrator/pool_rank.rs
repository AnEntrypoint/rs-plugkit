use serde_json::{json, Value};

const CANDIDATE_CAP: usize = 50;
const BROWSER_WORDS: [&str; 6] = ["cdp", "browser", "chrome", "headful", "live page", "boot"];
const GPU_WORDS: [&str; 9] = ["gpu", "webgpu", "amd", "nvidia", "accelerated", "gpulock", "frame-time", "p50", "dpr"];
const DESIGN_WORDS: [&str; 4] = ["design decision", "cluster-enabled", "circumnavigat", "planet wrap"];

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

pub fn rank(work: &[Value], blockers: &[Value], live_rows: &[String]) -> Value {
    let pending_blocked: Vec<&str> = blockers
        .iter()
        .filter_map(row_id)
        .filter_map(blocked_row_of)
        .collect();
    let is_live = |id: &str| live_rows.iter().any(|r| r == id);
    let live_arms: Vec<&str> = work
        .iter()
        .filter(|r| row_id(r).is_some_and(is_live))
        .filter_map(arm)
        .collect();
    let mut gpu_taken = live_arms.contains(&"gpu");
    let mut browser_taken = live_arms.contains(&"browser");
    let (mut gpu, mut browser, mut design, mut node, mut blocked) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut eligible: Vec<(u8, bool, String)> = Vec::new();
    for row in work {
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
        if is_live(id) {
            continue;
        }
        match row_arm {
            Some("gpu") if gpu_taken => continue,
            Some("gpu") => gpu_taken = true,
            Some("browser") if browser_taken => continue,
            Some("browser") => browser_taken = true,
            _ => {}
        }
        let arm_rank = u8::from(row_arm.is_some());
        eligible.push((arm_rank, is_pending_blocked, id.to_string()));
    }
    eligible.sort();
    eligible.dedup_by(|a, b| a.2 == b.2);
    let candidates: Vec<String> = eligible
        .into_iter()
        .take(CANDIDATE_CAP)
        .map(|(_, _, id)| id)
        .collect();
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
        },
    })
}
