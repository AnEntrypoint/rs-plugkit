mod browser;
mod daemon;
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

pub fn wasm_path_pub() -> PathBuf {
    wasm_path()
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
    // Retry a self-update whose rename was blocked last time by the running
    // exe's own file lock. Runs before any command dispatch so the target is as
    // unlocked as it will ever be during this process's life; without it a
    // staged .new binary is abandoned forever and gm-runner never updates
    // itself (witnessed: a 29h-old .new sitting beside a byte-different exe).
    if let Ok(exe) = std::env::current_exe() {
        let _ = download::adopt_staged_self_update(&exe);
    }

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

            if std::env::var("GM_RUNNER_NO_DAEMON").is_err() {
                ensure_wasm_installed(None)?;
                daemon::register_project(&cwd)?;
                if daemon::ensure_daemon_running()? {
                    eprintln!(
                        "[gm-runner] registered {} with the shared system-wide daemon -- no dedicated per-project process spawned",
                        cwd.display()
                    );
                    return Ok(());
                }
                eprintln!("[gm-runner] shared daemon unavailable, falling back to a dedicated per-project process");
            }

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
        "daemon" => daemon::run_daemon(),
        "dispatch" => {
            let verb = args.get(2).cloned().unwrap_or_default();
            let body = args.get(3).cloned().unwrap_or_else(|| "{}".to_string());
            let cwd = std::env::current_dir()?;
            let wasm = if let Ok(p) = std::env::var("GM_RUNNER_WASM_PATH_OVERRIDE") {
                PathBuf::from(p)
            } else {
                ensure_wasm_installed(None)?
            };
            let mut runtime = spool::PlugkitRuntime::load(&wasm, cwd)?;
            let out = runtime.dispatch(&verb, &body)?;
            println!("{out}");
            Ok(())
        }
        // One-shot native embed, no wasm module involved at all -- exists so
        // a slim-build JS-wrapper host (plugkit-wasm-wrapper.js's
        // globalThis.__hostEmbedSync) can delegate a single host_vec_embed
        // call to gm-runner's own candle path (crates/gm-runner/src/embed.rs)
        // via a synchronous spawnSync subprocess, without paying wasm
        // instantiation/compile cost per call. Input is raw text on stdin (a
        // pipe avoids argv length/quoting limits the JSON-arg `dispatch`
        // command already accepts for its own body); output is
        // `{"embedding":[f32...]}` on success or `{"error":"..."}` (exit
        // code 1) on failure -- caller distinguishes by exit code, never by
        // parsing prose.
        "embed-text" => {
            use std::io::Read as _;
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            match embed::embed(&text) {
                Ok(values) => {
                    println!("{}", serde_json::json!({ "embedding": values }));
                    Ok(())
                }
                Err(e) => {
                    println!("{}", serde_json::json!({ "error": e }));
                    std::process::exit(1);
                }
            }
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
