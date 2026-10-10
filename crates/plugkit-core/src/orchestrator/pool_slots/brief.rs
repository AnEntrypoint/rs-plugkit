use serde_json::{json, Value};

use super::traversal::{assign_row_traversal_surface, assign_traversal_surface};
use super::{now_ms, TRAVERSAL_LAUNCH_ID};
use crate::pkfs;

const WORKER_BRIEF_PATH: &str = ".gm/config-source-cache-default/prose/worker.md";

pub fn handle_brief(content: &str) -> (String, String, i32) {
    let Ok(body) = serde_json::from_str::<Value>(content) else {
        return (String::new(), "pool-brief: body must be JSON {row, session, role}".to_string(), 1);
    };
    let field = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (Some(row), Some(session), Some(role)) = (field("row"), field("session"), field("role")) else {
        let missing: Vec<&str> = ["row", "session", "role"]
            .into_iter()
            .filter(|&key| field(key).is_none())
            .collect();
        return (
            String::new(),
            format!(
                "pool-brief: body must be JSON with non-empty string fields row (a PRD row id), session (the subagent's SESSION_ID; the key is spelled session, not session_id) and role (one of: resolver, traversal). Missing or empty: {}",
                missing.join(", ")
            ),
            1,
        );
    };
    if role != "resolver" && role != "traversal" {
        return (String::new(), format!("pool-brief: role must be resolver or traversal, got {}", role), 1);
    }
    let Some(template) = pkfs::read_to_string(WORKER_BRIEF_PATH) else {
        return (String::new(), format!("pool-brief: worker brief missing at {}", WORKER_BRIEF_PATH), 1);
    };
    let mut brief = template
        .replace("\r\n", "\n")
        .replace("{row}", &row)
        .replace("{session}", &session)
        .replace("{role}", &role);
    let mut surface = Value::Null;
    if role == "traversal" {
        let assigned = if row == TRAVERSAL_LAUNCH_ID {
            assign_traversal_surface(".", now_ms())
        } else {
            assign_row_traversal_surface(".", &row, now_ms()).map(Some)
        };
        match assigned {
            Ok(Some(name)) => {
                brief.push_str(&format!(
                    "\nAssigned traversal surface: {name}. This brief leases it to your session. Scan only this surface, whatever surface your spawn prompt names, and list it under surfaces scanned in your receipt.\n"
                ));
                surface = json!(name);
            }
            Ok(None) => brief.push_str(
                "\nAssigned traversal surface: none. Every surface is leased or scanned. Log no rows and return a receipt that says so.\n",
            ),
            Err(message) => return (String::new(), message, 1),
        }
    }
    (json!({"ok": true, "verb": "pool-brief", "brief": brief, "surface": surface}).to_string(), String::new(), 0)
}
