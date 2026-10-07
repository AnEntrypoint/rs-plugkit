use std::time::Duration;

pub const GIT_INDEX_LOCK_MAX_ATTEMPTS: u32 = 8;
pub const GIT_INDEX_LOCK_BACKOFF_MS: u64 = 250;
pub const GIT_INDEX_LOCK_WAIT_BUDGET_MS: u64 = GIT_INDEX_LOCK_BACKOFF_MS
    * (GIT_INDEX_LOCK_MAX_ATTEMPTS as u64 - 1)
    * GIT_INDEX_LOCK_MAX_ATTEMPTS as u64
    / 2;
pub const GIT_INDEX_LOCK_DEFAULT_NAME: &str = ".git/index.lock";

const _: () = assert!(
    GIT_INDEX_LOCK_WAIT_BUDGET_MS == 7000,
    "GIT_INDEX_LOCK_WAIT_BUDGET_MS must equal the sum of the whole backoff schedule"
);

pub struct GitAttempt {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

pub struct IndexLockReport {
    pub lock: String,
    pub attempts: u32,
    pub waited_ms: u64,
    pub resolved: bool,
}

pub fn backoff_ms_for(attempt: u32) -> u64 {
    GIT_INDEX_LOCK_BACKOFF_MS.saturating_mul(attempt as u64)
}

pub fn is_index_lock_contention(attempt: &GitAttempt) -> bool {
    is_index_lock_text(&attempt.stdout) || is_index_lock_text(&attempt.stderr)
}

fn is_index_lock_text(output: &str) -> bool {
    let low = output.to_ascii_lowercase();
    if !low.contains("index.lock") {
        return false;
    }
    low.contains("unable to create")
        || low.contains("file exists")
        || low.contains("another git process")
        || low.contains("could not lock")
        || low.contains("unable to lock")
}

pub fn index_lock_path_of(attempt: &GitAttempt) -> Option<String> {
    index_lock_path_in(&attempt.stderr).or_else(|| index_lock_path_in(&attempt.stdout))
}

fn index_lock_path_in(output: &str) -> Option<String> {
    for line in output.lines() {
        let low = line.to_ascii_lowercase();
        let Some(at) = low.find("unable to create") else {
            continue;
        };
        let rest = line[at + "unable to create".len()..].trim_start();
        let Some(after_quote) = rest.strip_prefix('\'') else {
            continue;
        };
        let Some(end) = after_quote.find('\'') else {
            continue;
        };
        let path = after_quote[..end].trim();
        if path.to_ascii_lowercase().contains("index.lock") {
            return Some(path.to_string());
        }
    }
    None
}

pub fn retry_while_index_lock_contention<T>(
    run: &mut impl FnMut() -> T,
    attempt_of: impl Fn(&T) -> GitAttempt,
) -> (T, Option<IndexLockReport>) {
    let mut last = run();
    let mut attempt = attempt_of(&last);
    if attempt.exit_code == 0 || !is_index_lock_contention(&attempt) {
        return (last, None);
    }
    let lock = index_lock_path_of(&attempt).unwrap_or_else(|| GIT_INDEX_LOCK_DEFAULT_NAME.to_string());
    let mut attempts: u32 = 1;
    let mut waited_ms: u64 = 0;
    while attempts < GIT_INDEX_LOCK_MAX_ATTEMPTS {
        let backoff = backoff_ms_for(attempts);
        std::thread::sleep(Duration::from_millis(backoff));
        waited_ms += backoff;
        attempts += 1;
        last = run();
        attempt = attempt_of(&last);
        if attempt.exit_code == 0 || !is_index_lock_contention(&attempt) {
            return (
                last,
                Some(IndexLockReport {
                    lock,
                    attempts,
                    waited_ms,
                    resolved: attempt.exit_code == 0,
                }),
            );
        }
    }
    (
        last,
        Some(IndexLockReport {
            lock,
            attempts,
            waited_ms,
            resolved: attempt.exit_code == 0,
        }),
    )
}

pub fn unresolved_message(report: &IndexLockReport) -> String {
    format!(
        "git could not acquire '{}': it was still held after {} attempt(s) spanning {} ms ({:.1} s) of bounded backoff, and the {} ms budget is spent. Nothing was changed and the lock was NOT removed -- deleting another process's index.lock corrupts its commit. Re-dispatch once the other writer finishes.",
        report.lock,
        report.attempts,
        report.waited_ms,
        report.waited_ms as f64 / 1000.0,
        GIT_INDEX_LOCK_WAIT_BUDGET_MS
    )
}

pub fn resolved_note(lock: &str, attempts: u32, waited_ms: u64) -> String {
    format!(
        "git index lock '{}' was held by another git process; waited {} ms across {} attempt(s) of bounded backoff (budget {} ms)",
        lock, waited_ms, attempts, GIT_INDEX_LOCK_WAIT_BUDGET_MS
    )
}
