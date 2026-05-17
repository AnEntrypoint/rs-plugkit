#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use rs_plugkit::orchestrator::mutables;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct TempProject {
    dir: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("rs-plugkit-test-{}-{}", tag, n));
        fs::create_dir_all(dir.join(".gm")).unwrap();
        std::env::set_var("CLAUDE_PROJECT_DIR", &dir);
        TempProject { dir, _guard: guard }
    }

    fn write_mutables(&self, body: &str) {
        fs::write(self.dir.join(".gm").join("mutables.yml"), body).unwrap();
    }

    fn read_mutables(&self) -> String {
        fs::read_to_string(self.dir.join(".gm").join("mutables.yml")).unwrap()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn test_resolve_witnessed_when_evidence_present() {
    let p = TempProject::new("evpresent");
    p.write_mutables("- id: x\n  claim: thing\n  status: unknown\n  evidence: \"file.rs:42\"\n");
    let (out, err, code) = mutables::handle_resolve("x");
    assert_eq!(code, 0, "expected success, got err={}", err);
    assert!(out.contains("\"resolved\""), "out missing resolved key: {}", out);
    let after = p.read_mutables();
    assert!(after.contains("status: witnessed"), "status not flipped:\n{}", after);
}

#[test]
fn test_resolve_refused_when_evidence_empty() {
    let p = TempProject::new("evempty");
    p.write_mutables("- id: y\n  claim: thing\n  status: unknown\n  evidence: \"\"\n");
    let (_out, err, code) = mutables::handle_resolve("y");
    assert_ne!(code, 0, "expected refusal");
    assert!(err.to_lowercase().contains("evidence"), "err lacks 'evidence': {}", err);
    let after = p.read_mutables();
    assert!(!after.contains("witnessed"), "status flipped despite empty evidence:\n{}", after);
    assert!(after.contains("status: unknown"), "status no longer unknown:\n{}", after);
}

#[test]
fn test_resolve_refused_when_evidence_missing() {
    let p = TempProject::new("evmissing");
    p.write_mutables("- id: z\n  claim: thing\n  status: unknown\n");
    let (_out, err, code) = mutables::handle_resolve("z");
    assert_ne!(code, 0, "expected refusal");
    assert!(err.to_lowercase().contains("evidence"), "err lacks 'evidence': {}", err);
    let after = p.read_mutables();
    assert!(!after.contains("witnessed"), "status flipped despite missing evidence:\n{}", after);
}

#[test]
fn test_resolve_fires_memorize() {
    let p = TempProject::new("memofire");
    p.write_mutables("- id: w\n  claim: c\n  status: unknown\n  evidence: \"src/foo.rs:7 codesearch hit\"\n");
    let inbox = p.dir.join(".gm").join("exec-spool").join("in").join("memorize");
    let (_out, err, code) = mutables::handle_resolve("w");
    assert_eq!(code, 0, "expected success, got err={}", err);
    assert!(inbox.exists(), "memorize inbox not created");
    let entries: Vec<_> = fs::read_dir(&inbox).unwrap().collect();
    assert!(!entries.is_empty(), "no memorize spool file written");
}
