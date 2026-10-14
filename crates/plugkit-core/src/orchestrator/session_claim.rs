use super::prd::{request_flag, request_object};
use super::session_liveness::ActivityProbe;
use super::state::{dispatch_session_id, now_ms, read_state, write_state};
use serde_json::{json, Value};

const DEFAULT_IDLE_SECS: u64 = 600;

pub const HELP: &str = "\
session-claim {force, dry_run, idle_secs}.
  Makes the session of this dispatch the owner of the state machine in turn-state.json. That owner
  is what phase-status and instruction compare with the caller (session_mismatch).
  Without force the verb takes ownership only when no owner is recorded, or when the recorded owner
  shows no sign of life: no live session lease among the leases the host passes in
  (_lease_sessions) and no spool activity of that session in the last idle_secs seconds (default
  600). The caller that already owns the state machine changes nothing. A live owner is refused,
  with its evidence.
  force:true takes ownership from any owner. dry_run:true reports the decision and writes nothing.
  reason: no_owner, already_owner, owner_gone, forced or owner_live (the refusal).
  reply: ok, claimed, would_claim, reason, session_id, previous_owner, owner_evidence {live, basis,
         lease_pid, last_activity_ms, idle_ms}. basis is session_lease, recent_activity,
         no_lease_and_idle or idle_without_lease_facts.
  help:true returns this text and writes no state.";

fn request_session_id(request: &Value) -> Option<String> {
    ["session_id", "SESSION_ID", "sessionId"]
        .iter()
        .find_map(|key| request.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn idle_secs(request: &Value) -> u64 {
    request
        .get("idle_secs")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .unwrap_or(DEFAULT_IDLE_SECS)
}

pub fn handle(content: &str) -> (String, String, i32) {
    let request = request_object(content);
    let Some(caller) = dispatch_session_id().or_else(|| request_session_id(&request)) else {
        return (
            String::new(),
            "session-claim needs the caller's session_id in the body".to_string(),
            1,
        );
    };
    let force = request_flag(&request, &["force"]).unwrap_or(false);
    let dry_run = request_flag(&request, &["dry_run", "dryRun"]).unwrap_or(false);
    let idle_secs = idle_secs(&request);
    let leases = request
        .get("_lease_sessions")
        .and_then(Value::as_array)
        .cloned();
    let now = now_ms() as u64;
    let mut state = read_state();
    let previous_owner = state.session_id.clone();
    let (reason, evidence) = match previous_owner.as_deref() {
        None => ("no_owner", None),
        Some(owner) if owner == caller => ("already_owner", None),
        Some(owner) => {
            let evidence = ActivityProbe::load().assess(
                owner,
                leases.as_deref(),
                idle_secs.saturating_mul(1000),
                now,
            );
            let reason = if force {
                "forced"
            } else if evidence.live {
                "owner_live"
            } else {
                "owner_gone"
            };
            (reason, Some(evidence))
        }
    };
    let would_claim = matches!(reason, "no_owner" | "owner_gone" | "forced");
    let claimed = would_claim && !dry_run;
    let owner_evidence = evidence.as_ref().map(|found| found.to_json(now));
    let reply = json!({
        "ok": reason != "owner_live",
        "claimed": claimed,
        "would_claim": would_claim,
        "reason": reason,
        "dry_run": dry_run,
        "session_id": caller,
        "previous_owner": previous_owner,
        "owner_evidence": owner_evidence,
        "idle_secs": idle_secs,
    });
    if reason == "owner_live" {
        let basis = evidence.as_ref().map(|found| found.basis).unwrap_or("");
        return (
            reply.to_string(),
            format!(
                "session-claim refused: {} still owns the state machine ({basis}); pass force:true to take it over, or work under a session_id of your own",
                previous_owner.unwrap_or_default()
            ),
            1,
        );
    }
    if claimed {
        state.session_id = Some(caller.clone());
        state.updated_at_ms = now_ms();
        if write_state(&state).is_err() {
            return (
                String::new(),
                "session-claim could not write turn-state.json".to_string(),
                1,
            );
        }
        #[cfg(target_arch = "wasm32")]
        crate::wasm_dispatch::emit_event(
            "session-claim",
            json!({ "session_id": caller, "previous_owner": previous_owner, "reason": reason }),
        );
    }
    (reply.to_string(), String::new(), 0)
}
