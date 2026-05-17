use super::state::{Phase, set_phase};

pub fn next_skill(current: Phase) -> &'static str {
    match current {
        Phase::Plan => "gm-execute",
        Phase::Execute => "gm-emit",
        Phase::Emit => "gm-complete",
        Phase::Verify => "gm-complete",
        Phase::Complete => "update-docs",
    }
}

pub fn next_phase(current: Phase) -> Phase {
    match current {
        Phase::Plan => Phase::Execute,
        Phase::Execute => Phase::Emit,
        Phase::Emit => Phase::Verify,
        Phase::Verify => Phase::Complete,
        Phase::Complete => Phase::Complete,
    }
}

pub fn handle(content: &str) -> (String, String, i32) {
    let trimmed = content.trim();
    let target = if trimmed.is_empty() {
        let cur = super::state::read_state();
        let cur_phase = Phase::parse(&cur.phase).unwrap_or(Phase::Plan);
        next_phase(cur_phase)
    } else {
        match Phase::parse(trimmed) {
            Some(p) => p,
            None => return (String::new(), format!("invalid phase: {}", trimmed), 1),
        }
    };

    let skill = next_skill(target);
    match set_phase(target, Some(skill.to_string())) {
        Ok(s) => {
            let payload = serde_json::json!({
                "phase": s.phase,
                "nextSkill": skill,
            });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("write state failed: {}", e), 1),
    }
}
