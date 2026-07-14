use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime::{Cache, CacheConfig, Config, Engine, Instance, Linker, Module, Store};

use crate::wasm_host::{register_env_imports, register_wasi, HostState};

fn build_engine() -> anyhow::Result<Engine> {
    let mut config = Config::new();
    // Cranelift compiles plugkit.wasm (a ~150MB module) fresh on every cold
    // process start without this -- wasmtime's on-disk compilation cache
    // (keyed by module content hash) turns every load after the first into
    // a cache hit, so restart latency after the initial install matches the
    // old JS wrapper's near-instant re-exec instead of paying full Cranelift
    // compile time every spool boot.
    let cache_dir = directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".gm-tools").join("wasmtime-cache"));
    if let Some(dir) = cache_dir {
        let mut cache_config = CacheConfig::new();
        cache_config.with_directory(dir);
        config.cache(Some(Cache::new(cache_config)?));
    }
    Engine::new(&config).map_err(|e| anyhow::anyhow!(e))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct PlugkitRuntime {
    store: Store<HostState>,
    instance: Instance,
}

impl PlugkitRuntime {
    pub fn load(wasm_path: &Path, cwd: PathBuf) -> anyhow::Result<Self> {
        let engine = build_engine()?;
        let module = Module::from_file(&engine, wasm_path)?;
        let mut linker: Linker<HostState> = Linker::new(&engine);
        register_wasi(&mut linker)?;
        register_env_imports(&mut linker)?;

        let host_state = HostState::new(cwd);
        let instance_cell = host_state.instance.clone();
        let mut store = Store::new(&engine, host_state);
        let instance = linker.instantiate(&mut store, &module)?;
        *instance_cell.lock().unwrap() = Some(instance);

        Ok(Self { store, instance })
    }

    pub fn dispatch(&mut self, verb: &str, body: &str) -> anyhow::Result<String> {
        let alloc = self
            .instance
            .get_typed_func::<u32, u32>(&mut self.store, "plugkit_alloc")?;
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or_else(|| anyhow::anyhow!("wasm module has no exported memory"))?;

        let verb_ptr = alloc.call(&mut self.store, verb.len() as u32)?;
        memory.write(&mut self.store, verb_ptr as usize, verb.as_bytes())?;
        let body_ptr = alloc.call(&mut self.store, body.len() as u32)?;
        memory.write(&mut self.store, body_ptr as usize, body.as_bytes())?;

        let dispatch = self
            .instance
            .get_typed_func::<(u32, u32, u32, u32), u64>(&mut self.store, "dispatch_verb")?;
        let packed = dispatch.call(
            &mut self.store,
            (verb_ptr, verb.len() as u32, body_ptr, body.len() as u32),
        )?;

        let ptr = (packed & 0xffff_ffff) as u32;
        let len = (packed >> 32) as u32;
        if ptr == 0 || len == 0 {
            return Ok(String::new());
        }
        let mut buf = vec![0u8; len as usize];
        memory.read(&mut self.store, ptr as usize, &mut buf)?;

        if let Ok(free) = self
            .instance
            .get_typed_func::<(u32, u32), ()>(&mut self.store, "plugkit_free")
        {
            let _ = free.call(&mut self.store, (ptr, len));
        }

        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// Distinguishes why the watcher loop stopped -- a version-skew reload must
/// re-run ensure_wasm_installed + PlugkitRuntime::load with the fresh
/// binary, while any other clean stop is terminal. Conflating the two
/// (an earlier version returned plain `Ok(())` for both) made the
/// supervisor treat a detected version skew as "done, exit the whole
/// process" instead of "reload and keep serving."
pub enum StopReason {
    Reload,
}

/// Polls `<spool_dir>/in/<verb>/*.txt`, dispatches each to the wasm instance,
/// writes `<spool_dir>/out/<verb>-<name>.json`. Mirrors runSpoolWatcher in
/// gm-plugkit/plugkit-wasm-wrapper.js: same directory layout, same
/// verb-N naming, so existing gm-skill dispatch code needs no ABI change to
/// talk to this runner instead of the JS wrapper.
pub fn run_spool_watcher(runtime: &mut PlugkitRuntime, spool_dir: &Path) -> anyhow::Result<StopReason> {
    let in_dir = spool_dir.join("in");
    let out_dir = spool_dir.join("out");
    fs::create_dir_all(&in_dir)?;
    fs::create_dir_all(&out_dir)?;

    let status_path = spool_dir.join(".status.json");
    let mut processed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let booted_version_path = crate::download::install_dir().join("plugkit.version");
    let booted_version = fs::read_to_string(&booted_version_path).unwrap_or_default();
    let mut last_version_check = std::time::Instant::now();
    const VERSION_CHECK_INTERVAL: Duration = Duration::from_secs(30);
    // Remote-latest polling is deliberately much coarser than the local-skew
    // check above (30s): it costs a real network round-trip, and unlike the
    // local check (another process on this same machine already did the
    // download) a remote miss just means "nothing new yet" -- no reason to
    // hammer the GitHub API every 30s. This is the actual auto-update
    // mechanism: without it, ensure_wasm_installed only ever short-circuits
    // on existing.exists() and a wasm installed once is served forever, even
    // after a newer plugkit-bin release ships.
    let mut last_remote_check = std::time::Instant::now();
    const REMOTE_CHECK_INTERVAL: Duration = Duration::from_secs(600);

    loop {
        write_status(&status_path)?;

        // Version-skew self-heal: another process (a bootstrap re-run, or a
        // concurrent gm-runner instance) may have written a newer
        // plugkit.version to disk after this process loaded its wasm module.
        // Serving stale prose/gates against fresh plugkit.wasm content is the
        // exact staleness class AGENTS.md calls out as a same-turn deviation
        // to resolve -- exit cleanly so the supervisor (or the next boot
        // probe) picks up the fresh binary, rather than silently serving old
        // behavior indefinitely.
        if last_version_check.elapsed() >= VERSION_CHECK_INTERVAL {
            last_version_check = std::time::Instant::now();
            if let Ok(on_disk) = fs::read_to_string(&booted_version_path) {
                if !booted_version.is_empty() && on_disk.trim() != booted_version.trim() {
                    eprintln!(
                        "[gm-runner] version skew detected (booted {}, on-disk {}) -- exiting for supervisor reload",
                        booted_version.trim(),
                        on_disk.trim()
                    );
                    return Ok(StopReason::Reload);
                }
            }
        }

        // Real auto-update: periodically ask plugkit-bin's GitHub Releases
        // API what the latest published version is, and if it differs from
        // what's booted, download + verify it and write it to disk so the
        // local-skew check above (which runs every 30s) picks it up and
        // triggers the same clean reload path. Best-effort -- a network
        // failure here is silently ignored (Ok(None) or an Err both just
        // skip this cycle) so a flaky connection never blocks dispatch.
        if last_remote_check.elapsed() >= REMOTE_CHECK_INTERVAL {
            last_remote_check = std::time::Instant::now();
            if let Ok(Some(latest)) = crate::download::fetch_latest_plugkit_version() {
                let current = fs::read_to_string(&booted_version_path).unwrap_or_default();
                if !latest.is_empty() && latest.trim() != current.trim() {
                    eprintln!(
                        "[gm-runner] remote plugkit-bin latest is {} (booted {}) -- downloading",
                        latest,
                        current.trim()
                    );
                    if let Err(e) = crate::download::bootstrap_plugkit_wasm(&latest) {
                        eprintln!("[gm-runner] auto-update download failed: {e:#} -- will retry next interval");
                    }
                    // bootstrap_plugkit_wasm already wrote plugkit.version on
                    // success; the next VERSION_CHECK_INTERVAL tick (<=30s
                    // away) will see the on-disk/booted mismatch and reload.
                }
            }
        }

        let mut work_done = false;
        if let Ok(verb_dirs) = fs::read_dir(&in_dir) {
            for verb_entry in verb_dirs.flatten() {
                if !verb_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let verb = verb_entry.file_name().to_string_lossy().into_owned();
                let verb_dir = verb_entry.path();
                let Ok(files) = fs::read_dir(&verb_dir) else { continue };
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                        continue;
                    }
                    if processed.contains(&path) {
                        continue;
                    }
                    let Ok(body) = fs::read_to_string(&path) else { continue };
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();

                    let result = runtime.dispatch(&verb, &body).unwrap_or_else(|e| {
                        serde_json::json!({"ok": false, "verb": verb, "error": e.to_string()})
                            .to_string()
                    });

                    let out_path = out_dir.join(format!("{verb}-{stem}.json"));
                    fs::write(&out_path, result)?;
                    processed.insert(path.clone());
                    let _ = fs::remove_file(&path);
                    work_done = true;
                }
            }
        }

        if !work_done {
            sleep(Duration::from_millis(150));
        }
    }
}

fn write_status(status_path: &Path) -> anyhow::Result<()> {
    let status = serde_json::json!({
        "pid": std::process::id(),
        "ts": now_ms(),
        "runtime": "gm-runner-native",
    });
    fs::write(status_path, status.to_string())?;
    Ok(())
}
