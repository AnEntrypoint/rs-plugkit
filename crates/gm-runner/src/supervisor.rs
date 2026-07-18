use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_RESTART_BURST: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const BURST_BACKOFF: Duration = Duration::from_secs(60);

/// Internal panic-recovery loop, gm-runner's equivalent of
/// gm-plugkit/supervisor.js's restart-burst-backoff. Unlike the JS model
/// (a separate supervisor process spawning/monitoring a wrapper child),
/// gm-runner is one self-contained process -- a wasm-side panic reloads the
/// wasm instance in-place rather than exec'ing a new OS process. A crash
/// burst (more than MAX_RESTART_BURST reloads within RESTART_WINDOW) still
/// backs off, so a hard-crashing wasm module can't spin the CPU forever.
pub fn run_supervised(wasm_path: PathBuf, cwd: PathBuf, spool_dir: PathBuf) -> anyhow::Result<()> {
    let mut restart_timestamps: Vec<Instant> = Vec::new();

    loop {
        let wasm_path = wasm_path.clone();
        let cwd = cwd.clone();
        let spool_dir = spool_dir.clone();

        let result = std::panic::catch_unwind(move || -> anyhow::Result<crate::spool::StopReason> {
            let mut runtime = crate::spool::PlugkitRuntime::load(&wasm_path, cwd)?;
            crate::spool::run_spool_watcher(&mut runtime, &spool_dir)
        });

        match result {
            // Module::from_file re-reads wasm_path's bytes fresh next loop
            // iteration -- the install path is stable (always
            // ~/.gm-tools/plugkit.wasm), only its CONTENT changed, so simply
            // looping (not returning) picks up the new version.
            Ok(Ok(crate::spool::StopReason::Reload)) => {
                eprintln!("[gm-runner] reloading wasm module for version-skew self-heal");
                continue;
            }
            Ok(Ok(crate::spool::StopReason::ExeUpdated)) => {
                eprintln!("[gm-runner] executable self-updated on disk -- exiting for external relaunch");
                return Ok(());
            }
            Ok(Err(e)) => {
                eprintln!("[gm-runner] watcher error: {e:#}");
            }
            Err(panic_payload) => {
                let msg = panic_message(&panic_payload);
                eprintln!("[gm-runner] watcher panicked: {msg}");
            }
        }

        let now = Instant::now();
        restart_timestamps.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
        restart_timestamps.push(now);

        if restart_timestamps.len() > MAX_RESTART_BURST {
            eprintln!(
                "[gm-runner] restart-burst-exceeded: {} restarts within {:?}, backing off {:?}",
                restart_timestamps.len(),
                RESTART_WINDOW,
                BURST_BACKOFF
            );
            std::thread::sleep(BURST_BACKOFF);
            restart_timestamps.clear();
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
