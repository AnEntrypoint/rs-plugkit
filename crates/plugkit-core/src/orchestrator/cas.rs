use serde_yaml::Value;
use crate::pkfs;

pub enum CasOutcome<T> {
    Write(Value, T),
    Abort(String, String, i32),
}

pub fn cas_retry_write<T>(
    path_s: &str,
    max_attempts: u32,
    verb_label: &str,
    mut modify: impl FnMut(Value) -> CasOutcome<T>,
) -> Result<T, (String, String, i32)> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let before_raw = if pkfs::exists(path_s) { pkfs::read_to_string(path_s).unwrap_or_default() } else { String::new() };
        let doc: Value = if before_raw.is_empty() {
            Value::Sequence(vec![])
        } else {
            serde_yaml::from_str(&before_raw).unwrap_or(Value::Sequence(vec![]))
        };

        let (new_doc, result) = match modify(doc) {
            CasOutcome::Write(new_doc, result) => (new_doc, result),
            CasOutcome::Abort(out, err, rc) => return Err((out, err, rc)),
        };

        let new_raw = serde_yaml::to_string(&new_doc).unwrap_or_default();

        // A plain read-then-write pair (even with a recheck read right
        // before the write) leaves a window between the recheck and the
        // write itself where a concurrent writer can land and get
        // silently clobbered -- the recheck only re-validates what THIS
        // caller last saw, not what is true at the instant of the write.
        // host_cas_write closes that window by doing the "is `before_raw`
        // still current" check and the write in one host-side locked
        // critical section, so a lost update is detected unconditionally
        // rather than only when the timing happens to expose it.
        match pkfs::cas_write(path_s, &before_raw, &new_raw) {
            pkfs::CasWriteOutcome::Swapped => return Ok(result),
            pkfs::CasWriteOutcome::Mismatch => {
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!("{} CAS failed after {} attempts: concurrent writer keeps changing {}", verb_label, max_attempts, path_s),
                        1,
                    ));
                }
                continue;
            }
            pkfs::CasWriteOutcome::IoError => {
                if attempt >= max_attempts {
                    return Err((String::new(), "write failed".to_string(), 1));
                }
                continue;
            }
        }
    }
}
