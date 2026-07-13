use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A holder whose OS process is alive but hasn't updated .status.json's `ts`
/// within this window is treated as hung, not merely busy -- matches
/// gm-plugkit/supervisor.js's STATUS_STALE_MS (30s) heartbeat-kill
/// threshold, so a native-runner watcher wedged on a genuine deadlock
/// (not a long verb, which refreshes ts every loop iteration regardless of
/// per-verb work duration) gets the same takeover treatment the JS
/// supervisor already gives a hung wrapper process.
const HEARTBEAT_STALE_MS: u64 = 30_000;

/// Single-instance lock via atomic O_EXCL create -- never check-then-act
/// (a plain `path.exists()` check followed by a separate write is a TOCTOU
/// race between two watchers racing to boot in the same project). The lock
/// file holds this process's pid so a later boot can tell a genuinely-dead
/// prior holder (pid no longer running) from a live one, and clean up a
/// stale lock left behind by a crash instead of wedging forever. A holder
/// that IS alive but whose heartbeat has gone stale (hung, not merely busy)
/// gets the same takeover as a dead one -- a live-but-wedged process is no
/// more useful to the project than a dead one, and neither should block a
/// fresh boot forever.
pub struct SpoolLock {
    path: PathBuf,
}

impl SpoolLock {
    pub fn acquire(spool_dir: &Path) -> anyhow::Result<Self> {
        let path = spool_dir.join(".gm-runner.lock");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                write!(f, "{}", std::process::id())?;
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder_pid = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                if let Some(pid) = holder_pid {
                    let alive = process_alive(pid);
                    if alive && !heartbeat_stale(spool_dir) {
                        anyhow::bail!(
                            "another gm-runner (pid {pid}) already holds the spool lock at {}",
                            path.display()
                        );
                    }
                    if alive {
                        eprintln!(
                            "[gm-runner] holder pid {pid} is alive but heartbeat is stale (>={HEARTBEAT_STALE_MS}ms) -- taking over as hung, not dead"
                        );
                    }
                }
                // Holder pid is dead, unreadable, or alive-but-heartbeat-stale
                // (hung): the lock does not represent useful, responsive work.
                // Atomically replace it rather than unlink-then-create (still
                // race-free against a fresh concurrent acquirer, since
                // create_new below re-checks).
                std::fs::remove_file(&path)?;
                let mut f = OpenOptions::new().write(true).create_new(true).open(&path)?;
                write!(f, "{}", std::process::id())?;
                Ok(Self { path })
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn heartbeat_stale(spool_dir: &Path) -> bool {
    let status_path = spool_dir.join(".status.json");
    let Ok(content) = std::fs::read_to_string(&status_path) else {
        // No heartbeat file at all: treat as stale (nothing to trust as
        // "responsive"), same as an unreadable holder pid above.
        return true;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return true;
    };
    let Some(ts) = v.get("ts").and_then(|t| t.as_u64()) else {
        return true;
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    now_ms.saturating_sub(ts) >= HEARTBEAT_STALE_MS
}

impl Drop for SpoolLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
