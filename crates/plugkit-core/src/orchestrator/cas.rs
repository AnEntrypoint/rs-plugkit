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

        let recheck_raw = if pkfs::exists(path_s) { pkfs::read_to_string(path_s).unwrap_or_default() } else { String::new() };
        if recheck_raw != before_raw {
            if attempt >= max_attempts {
                return Err((
                    String::new(),
                    format!("{} CAS failed after {} attempts: concurrent writer keeps changing {}", verb_label, max_attempts, path_s),
                    1,
                ));
            }
            continue;
        }
        if !pkfs::write(path_s, &new_raw) {
            if attempt >= max_attempts {
                return Err((String::new(), "write failed".to_string(), 1));
            }
            continue;
        }

        // Post-write read-back confirmation: the pre-write recheck above only
        // catches a concurrent writer that completed strictly between our two
        // reads. It does nothing against a second writer racing through the
        // exact same window (both readers see identical `before_raw`, both
        // compute a new doc, both write -- last write wins, silently
        // discarding the other's change even though pkfs::write returned
        // true for it). Host fs writes here have no O_EXCL/lock primitive
        // reachable from wasm, so the only way to detect a lost update after
        // the fact is to read the file straight back and confirm OUR bytes
        // actually landed. A mismatch means we lost the race -- retry the
        // whole read-modify-write cycle against the now-current on-disk
        // state rather than returning a false success.
        let confirm_raw = if pkfs::exists(path_s) { pkfs::read_to_string(path_s).unwrap_or_default() } else { String::new() };
        if confirm_raw != new_raw {
            if attempt >= max_attempts {
                return Err((
                    String::new(),
                    format!("{} CAS write lost the race after {} attempts: {} was overwritten by a concurrent writer immediately after our write landed", verb_label, max_attempts, path_s),
                    1,
                ));
            }
            continue;
        }
        return Ok(result);
    }
}
