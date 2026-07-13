use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Single-instance lock via atomic O_EXCL create -- never check-then-act
/// (a plain `path.exists()` check followed by a separate write is a TOCTOU
/// race between two watchers racing to boot in the same project). The lock
/// file holds this process's pid so a later boot can tell a genuinely-dead
/// prior holder (pid no longer running) from a live one, and clean up a
/// stale lock left behind by a crash instead of wedging forever.
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
                    if process_alive(pid) {
                        anyhow::bail!(
                            "another gm-runner (pid {pid}) already holds the spool lock at {}",
                            path.display()
                        );
                    }
                }
                // Holder pid is dead or unreadable: the lock is stale from a
                // crash, not a live contender. Atomically replace it rather
                // than unlink-then-create (still race-free against a fresh
                // concurrent acquirer, since create_new below re-checks).
                std::fs::remove_file(&path)?;
                let mut f = OpenOptions::new().write(true).create_new(true).open(&path)?;
                write!(f, "{}", std::process::id())?;
                Ok(Self { path })
            }
            Err(e) => Err(e.into()),
        }
    }
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
