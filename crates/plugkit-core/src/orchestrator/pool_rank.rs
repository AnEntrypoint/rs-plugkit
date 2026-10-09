use serde_json::{json, Value};

const CANDIDATE_CAP: usize = 50;
const CAPPED_ARMS: [&str; 2] = ["gpu", "browser"];

fn text_field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn row_id(row: &Value) -> Option<&str> {
    text_field(row, "id")
}

fn needs_design(row: &Value) -> bool {
    row.get("decision").and_then(Value::as_bool).unwrap_or(false)
        || row.get("needs_design").and_then(Value::as_bool).unwrap_or(false)
}

fn arm(row: &Value) -> Option<&str> {
    text_field(row, "arm").filter(|a| CAPPED_ARMS.contains(a))
}

pub fn is_blocker_id(id: &str) -> bool {
    id.contains("-blocker-")
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
    let (mut gpu, mut browser, mut design, mut blocked) = (0usize, 0usize, 0usize, 0usize);
    let mut eligible: Vec<(bool, String)> = Vec::new();
    for row in work {
        let Some(id) = row_id(row) else { continue };
        let is_pending_blocked = pending_blocked.contains(&id);
        if is_pending_blocked {
            blocked += 1;
        }
        match arm(row) {
            Some("gpu") => gpu += 1,
            Some("browser") => browser += 1,
            _ => {}
        }
        if needs_design(row) {
            design += 1;
            continue;
        }
        if is_live(id) {
            continue;
        }
        match arm(row) {
            Some("gpu") if gpu_taken => continue,
            Some("gpu") => gpu_taken = true,
            Some("browser") if browser_taken => continue,
            Some("browser") => browser_taken = true,
            _ => {}
        }
        eligible.push((is_pending_blocked, id.to_string()));
    }
    eligible.sort();
    eligible.dedup_by(|a, b| a.1 == b.1);
    let candidates: Vec<String> = eligible
        .into_iter()
        .take(CANDIDATE_CAP)
        .map(|(_, id)| id)
        .collect();
    json!({
        "candidates": candidates,
        "supply": {
            "open_work": work.len(),
            "pending_blocker_rows": blockers.len(),
            "rows_with_pending_blocker": blocked,
            "gpu_arm_rows": gpu,
            "browser_arm_rows": browser,
            "design_decision_rows": design,
        },
    })
}
