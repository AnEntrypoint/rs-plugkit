use super::gm_dir;
use crate::pkfs;
use serde_json::{json, Value};

const MIN_EPOCH_MS: u64 = 1_000_000_000_000;

pub struct SessionEvidence {
    pub live: bool,
    pub basis: &'static str,
    pub last_activity_ms: Option<u64>,
    pub lease_pid: Option<u64>,
}

impl SessionEvidence {
    pub fn to_json(&self, now_ms: u64) -> Value {
        json!({
            "live": self.live,
            "basis": self.basis,
            "lease_pid": self.lease_pid,
            "last_activity_ms": self.last_activity_ms,
            "idle_ms": self.last_activity_ms.map(|ts| now_ms.saturating_sub(ts)),
        })
    }
}

pub struct ActivityProbe {
    spool_dir: String,
    out_names: Vec<String>,
}

fn dashed_name(entry: &Value) -> Option<String> {
    entry
        .as_str()
        .or_else(|| entry.get("name").and_then(Value::as_str))
        .map(|name| format!("-{name}"))
}

fn digits_only(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn task_timestamp(dashed_name: &str, needle: &str) -> Option<u64> {
    let mut from = 0;
    while let Some(at) = dashed_name[from..].find(needle) {
        let after = from + at + needle.len();
        let task_tail = dashed_name[after..].split('.').next().unwrap_or("");
        let parts: Vec<&str> = task_tail.split('-').collect();
        if parts.len() == 3 && parts.iter().all(|part| digits_only(part)) {
            let stamped = parts[1].parse::<u64>().ok().filter(|ts| *ts >= MIN_EPOCH_MS);
            if stamped.is_some() {
                return stamped;
            }
        }
        from += at + 1;
    }
    None
}

impl ActivityProbe {
    pub fn load() -> Self {
        let spool_dir = gm_dir().join("exec-spool").to_string_lossy().to_string();
        let out_names = pkfs::readdir(&format!("{spool_dir}/out"))
            .as_ref()
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(dashed_name).collect())
            .unwrap_or_default();
        ActivityProbe {
            spool_dir,
            out_names,
        }
    }

    pub fn last_activity_ms(&self, session_id: &str) -> Option<u64> {
        let needle = format!("-{session_id}-");
        let from_out_files = self
            .out_names
            .iter()
            .filter_map(|name| task_timestamp(name, &needle))
            .max();
        let marker = format!("{}/.last-instruction-hash-{session_id}.json", self.spool_dir);
        let from_marker = pkfs::stat(&marker)
            .and_then(|stat| {
                stat.get("mtime_ms")
                    .or_else(|| stat.get("mtimeMs"))
                    .and_then(Value::as_f64)
            })
            .map(|ms| ms as u64);
        from_out_files.into_iter().chain(from_marker).max()
    }

    pub fn assess(
        &self,
        session_id: &str,
        leases: Option<&[Value]>,
        idle_ms: u64,
        now_ms: u64,
    ) -> SessionEvidence {
        let lease_pid = leases
            .and_then(|facts| {
                facts
                    .iter()
                    .find(|fact| fact.get("session_id").and_then(Value::as_str) == Some(session_id))
            })
            .map(|fact| fact.get("pid").and_then(Value::as_u64).unwrap_or(0));
        let last_activity_ms = self.last_activity_ms(session_id);
        let recent = last_activity_ms.is_some_and(|ts| now_ms.saturating_sub(ts) < idle_ms);
        let (live, basis) = if lease_pid.is_some() {
            (true, "session_lease")
        } else if recent {
            (true, "recent_activity")
        } else if leases.is_some() {
            (false, "no_lease_and_idle")
        } else {
            (false, "idle_without_lease_facts")
        };
        SessionEvidence {
            live,
            basis,
            last_activity_ms,
            lease_pid,
        }
    }
}
