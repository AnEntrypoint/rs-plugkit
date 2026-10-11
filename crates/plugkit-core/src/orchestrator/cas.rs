use crate::pkfs;
use serde_yaml::Value;
use super::yaml_util;

pub enum CasOutcome<T> {
    Write(Value, T),
    Abort(String, String, i32),
}

/// Re-read `path_s` and parse it; `None` when it is gone or unparseable.
fn read_back(path_s: &str) -> Option<Value> {
    let raw = pkfs::read_to_string(path_s)?;
    let source = raw.strip_prefix('\u{feff}').unwrap_or(raw.as_str());
    serde_yaml::from_str::<Value>(source).ok()
}

/// True when a CAS error is a lost update: the write landed but the row is not
/// in the file, so the caller must not report success.
pub fn is_lost_update(err: &str) -> bool {
    err.contains("lost-update:")
}

pub fn cas_retry_write<T>(
    path_s: &str,
    max_attempts: u32,
    verb_label: &str,
    modify: impl FnMut(Value) -> CasOutcome<T>,
) -> Result<T, (String, String, i32)> {
    cas_retry_write_verified(path_s, max_attempts, verb_label, modify, |_doc| true)
}

/// `cas_retry_write` plus a post-write read-back: the write only counts as done
/// when the bytes on disk, re-read and re-parsed, still satisfy `verify`. A row
/// that lands and then vanishes under concurrent writers is reported as a lost
/// update, never as success.
pub fn cas_retry_write_verified<T>(
    path_s: &str,
    max_attempts: u32,
    verb_label: &str,
    mut modify: impl FnMut(Value) -> CasOutcome<T>,
    verify: impl Fn(&Value) -> bool,
) -> Result<T, (String, String, i32)> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let before_raw = if pkfs::exists(path_s) {
            match pkfs::read_to_string(path_s) {
                Some(raw) => raw,
                None => {
                    return Err((
                        String::new(),
                        format!(
                            "{} refused: {} exists but could not be read; building the update from an empty read would overwrite its rows",
                            verb_label, path_s
                        ),
                        1,
                    ));
                }
            }
        } else {
            String::new()
        };
        let parse_source = before_raw
            .strip_prefix('\u{feff}')
            .unwrap_or(before_raw.as_str());
        let doc: Value = if parse_source.trim().is_empty() {
            Value::Sequence(vec![])
        } else {
            match serde_yaml::from_str(parse_source) {
                Ok(v) => v,
                Err(e) => {
                    let fault = yaml_util::parse_fault(parse_source, &e);
                    let where_at = fault
                        .as_ref()
                        .map(|f| format!(" offending text at {};", f.describe()))
                        .unwrap_or_default();
                    return Err((
                        String::new(),
                        format!("{} refused: {} contains non-empty content that failed to parse as YAML ({}).{} Writing would silently discard it, so the write is aborted instead. Repair it in place with `prd-repair {{\"path\":\"{}\"}}` (it backs the file up first) or `prd-add {{\"repair\":true,...}}`; inspect the fault with `prd-repair {{\"path\":\"{}\",\"dry_run\":true}}`.", verb_label, path_s, e, where_at, path_s, path_s),
                        1,
                    ));
                }
            }
        };

        let (new_doc, result) = match modify(doc) {
            CasOutcome::Write(new_doc, result) => (new_doc, result),
            CasOutcome::Abort(out, err, rc) => return Err((out, err, rc)),
        };

        let new_raw = match yaml_util::serialize_checked(&new_doc) {
            Ok(raw) => raw,
            Err(e) => {
                return Err((
                    String::new(),
                    format!(
                        "{} refused: the updated document for {} was not written because it is not read-back safe -- {}; writing it would leave the store unparseable for every later lane",
                        verb_label, path_s, e
                    ),
                    1,
                ));
            }
        };

        match pkfs::cas_write(path_s, &before_raw, &new_raw) {
            pkfs::CasWriteOutcome::Swapped => {
                match read_back(path_s) {
                    Some(after) if verify(&after) => return Ok(result),
                    other => {
                        if attempt >= max_attempts {
                            let why = match other {
                                Some(_) => "the file parses but does not contain what this dispatch wrote",
                                None => "the file no longer parses",
                            };
                            return Err((
                                String::new(),
                                format!(
                                    "{} lost-update: the write to {} was accepted but reading it back shows {} -- the row is NOT in the store, so this dispatch reports no success instead of a false ok",
                                    verb_label, path_s, why
                                ),
                                1,
                            ));
                        }
                        continue;
                    }
                }
            }
            pkfs::CasWriteOutcome::Mismatch => {
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!(
                            "{} CAS failed after {} attempts: concurrent writer keeps changing {}",
                            verb_label, max_attempts, path_s
                        ),
                        1,
                    ));
                }
                continue;
            }
            pkfs::CasWriteOutcome::IoError => {
                if attempt >= max_attempts {
                    return Err((
                        String::new(),
                        format!(
                            "{} write failed after {} attempts: {}",
                            verb_label, max_attempts, path_s
                        ),
                        1,
                    ));
                }
                continue;
            }
        }
    }
}
