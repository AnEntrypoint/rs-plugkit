mod browser;
mod download;
mod embed;
mod exec_js;
mod lock;
mod spool;
mod supervisor;
mod wasm_host;

use std::path::PathBuf;

fn wasm_path() -> PathBuf {
    download::install_dir().join("plugkit.wasm")
}

/// Version to install when the local wasm is missing or stale. Resolution
/// order: PLUGKIT_VERSION env (explicit pin, e.g. CI/testing) then the
/// gm.json plugkitVersion this runner was built alongside (baked in at
/// compile time via build.rs-free env! since gm.json lives one repo over --
/// falls back to erroring out with an actionable message rather than
/// guessing "latest", which could silently drift the pinned/verified
/// version this runner was shipped to match).
fn resolve_target_version() -> anyhow::Result<String> {
    if let Ok(v) = std::env::var("PLUGKIT_VERSION") {
        return Ok(v);
    }
    anyhow::bail!(
        "no PLUGKIT_VERSION set and no compiled-in default yet -- set PLUGKIT_VERSION=<x.y.z> \
         (see gm.json::plugkitVersion in the gm repo for the pinned version) or run \
         `gm-runner bootstrap <version>` explicitly"
    )
}

fn ensure_wasm_installed(explicit_version: Option<&str>) -> anyhow::Result<PathBuf> {
    let existing = wasm_path();
    if existing.exists() && explicit_version.is_none() {
        return Ok(existing);
    }
    let version = match explicit_version {
        Some(v) => v.to_string(),
        None => resolve_target_version()?,
    };
    download::bootstrap_plugkit_wasm(&version)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "bootstrap" => {
            let version = args.get(2).cloned();
            let dest = ensure_wasm_installed(version.as_deref())?;
            println!("plugkit.wasm installed at {}", dest.display());
            Ok(())
        }
        "spool" => {
            let cwd = std::env::var("CLAUDE_PROJECT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::current_dir().expect("cwd unavailable"));
            let spool_dir = cwd.join(".gm").join("exec-spool");
            std::fs::create_dir_all(&spool_dir)?;
            // Atomic O_EXCL lock: two concurrent gm sessions in the same
            // project must never both spawn a watcher racing on the same
            // spool dir. A dead prior holder's stale lock is detected and
            // replaced, never blindly trusted or blindly overwritten.
            let _lock = lock::SpoolLock::acquire(&spool_dir)?;
            let wasm = ensure_wasm_installed(None)?;
            eprintln!("[gm-runner] compiling plugkit.wasm module (first load takes longer; cranelift caches after)...");
            eprintln!("[gm-runner] watching {}", spool_dir.display());
            supervisor::run_supervised(wasm, cwd, spool_dir)
        }
        "dispatch" => {
            let verb = args.get(2).cloned().unwrap_or_default();
            let body = args.get(3).cloned().unwrap_or_else(|| "{}".to_string());
            let cwd = std::env::current_dir()?;
            let wasm = ensure_wasm_installed(None)?;
            let mut runtime = spool::PlugkitRuntime::load(&wasm, cwd)?;
            let out = runtime.dispatch(&verb, &body)?;
            println!("{out}");
            Ok(())
        }
        "--version" | "version" => {
            println!("gm-runner {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "progress" => {
            let in_flight = download::current_progress();
            let ready = serde_json::json!({
                "embed_weights_ready": embed::is_ready(),
                "wasm_ready": wasm_path().exists(),
            });
            match in_flight {
                Some(mut p) => {
                    if let Some(obj) = p.as_object_mut() {
                        obj.insert("in_flight".to_string(), serde_json::Value::Bool(true));
                        obj.insert("subsystems".to_string(), ready);
                    }
                    println!("{p}");
                }
                None => {
                    println!(
                        "{}",
                        serde_json::json!({"in_flight": false, "subsystems": ready})
                    );
                }
            }
            Ok(())
        }
        other => {
            eprintln!(
                "gm-runner: unknown command '{other}'. Usage: gm-runner <bootstrap [version]|spool|dispatch|progress|version>"
            );
            std::process::exit(1);
        }
    }
}
