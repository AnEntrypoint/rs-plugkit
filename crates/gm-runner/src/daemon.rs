use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wasmtime::{Cache, CacheConfig, Config, Engine, Instance, Linker, Module, Store};

use crate::wasm_host::{register_env_imports, register_wasi, HostState};

/// System-wide registry file listing every project directory gm-runner's
/// single shared daemon should watch. Each line is an absolute project root
/// path. A project registers itself here (append-if-absent) the first time
/// its own `gm-runner spool` boot line runs against a daemon-aware build --
/// the daemon polls this file for new entries so a freshly-registered
/// project starts being served without a daemon restart.
fn registry_path() -> PathBuf {
    crate::download::install_dir().join("daemon-registry.txt")
}

fn daemon_status_path() -> PathBuf {
    crate::download::install_dir().join("daemon-status.json")
}

fn daemon_lock_path() -> PathBuf {
    crate::download::install_dir().join("daemon.lock")
}

const DAEMON_STALE_MS: u64 = 60_000;

/// Checks whether a shared daemon is genuinely alive (fresh heartbeat in
/// daemon-status.json) and, if not, spawns exactly one fresh detached daemon
/// process system-wide. Returns Ok(true) once a live daemon is confirmed
/// (either pre-existing or freshly spawned and its first heartbeat observed
/// within a bounded wait), Ok(false) if a daemon could not be confirmed
/// alive within that bound (caller falls back to the old per-project
/// dedicated-process path rather than hanging indefinitely).
pub fn ensure_daemon_running() -> anyhow::Result<bool> {
    if is_daemon_fresh() {
        return Ok(true);
    }

    // Atomic O_EXCL-style guard so a burst of `gm-runner spool` invocations
    // across many projects at once (exactly the scenario that caused the
    // original N-processes problem) never spawns more than one daemon --
    // the same acquire-or-fail-fast pattern this crate's own SpoolLock uses
    // for per-project locking, applied at the system-wide daemon level.
    let lock_path = daemon_lock_path();
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let acquired = fs::OpenOptions::new().write(true).create_new(true).open(&lock_path).is_ok();
    if !acquired {
        // Someone else is already spawning it (or a stale lock file from a
        // crashed spawn attempt) -- wait briefly for their heartbeat rather
        // than racing a second daemon into existence.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(200));
            if is_daemon_fresh() {
                return Ok(true);
            }
        }
        let _ = fs::remove_file(&lock_path);
        return Ok(false);
    }

    let spawn_result = spawn_detached_daemon();
    let _ = fs::remove_file(&lock_path);
    spawn_result?;

    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(200));
        if is_daemon_fresh() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_daemon_fresh() -> bool {
    let Ok(raw) = fs::read_to_string(daemon_status_path()) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { return false };
    let Some(ts) = v.get("ts").and_then(|t| t.as_u64()) else { return false };
    now_ms().saturating_sub(ts) < DAEMON_STALE_MS
}

fn spawn_detached_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (0x08000000) + DETACHED_PROCESS (0x00000008):
        // no console window flash, and the daemon survives the spawning
        // gm-runner spool invocation's own process exiting -- this is the
        // exact windowsHide-at-spawn-time requirement this project's own
        // memory already documents (setting it after spawn is too late).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach from the spawning process's session so it isn't killed
        // when the parent shell/session exits.
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(unix)]
fn libc_setsid() {
    // Minimal setsid() call without pulling in the full libc crate as a
    // dependency -- direct extern declaration, POSIX-guaranteed signature.
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid(); }
}

pub fn register_project(cwd: &Path) -> anyhow::Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let cwd_str = cwd.to_string_lossy().to_string();
    if existing.lines().any(|l| l.trim() == cwd_str) {
        return Ok(());
    }
    use std::io::Write as _;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{cwd_str}")?;
    Ok(())
}

fn read_registry() -> Vec<PathBuf> {
    fs::read_to_string(registry_path())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

fn build_engine() -> anyhow::Result<Engine> {
    let mut config = Config::new();
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A per-project wasm instance, lazily created against the daemon's ONE
/// shared compiled Module (Module::from_file is the genuinely expensive
/// part; a Store+Instance instantiated from an already-compiled Module is
/// cheap). This is the only per-project MEMORY cost -- no separate OS
/// process, no separate bun.exe/node.exe/gm-runner.exe overhead, no
/// separate 133MB embed-model load (embedding is delegated to the SAME
/// process's native candle path via host_vec_embed, never re-loaded
/// per-project).
struct ProjectRuntime {
    store: Store<HostState>,
    instance: Instance,
    last_active: Instant,
}

impl ProjectRuntime {
    fn new(engine: &Engine, module: &Module, cwd: PathBuf) -> anyhow::Result<Self> {
        let mut linker: Linker<HostState> = Linker::new(engine);
        register_wasi(&mut linker)?;
        register_env_imports(&mut linker)?;
        let host_state = HostState::new(cwd);
        let instance_cell = host_state.instance.clone();
        let mut store = Store::new(engine, host_state);
        let instance = linker.instantiate(&mut store, module)?;
        *instance_cell.lock().unwrap() = Some(instance);
        Ok(Self { store, instance, last_active: Instant::now() })
    }

    fn dispatch(&mut self, verb: &str, body: &str) -> anyhow::Result<String> {
        self.last_active = Instant::now();
        let alloc = self.instance.get_typed_func::<u32, u32>(&mut self.store, "plugkit_alloc")?;
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
        let packed = dispatch.call(&mut self.store, (verb_ptr, verb.len() as u32, body_ptr, body.len() as u32))?;
        let ptr = (packed & 0xffff_ffff) as u32;
        let len = (packed >> 32) as u32;
        if ptr == 0 || len == 0 {
            return Ok(String::new());
        }
        let mut buf = vec![0u8; len as usize];
        memory.read(&mut self.store, ptr as usize, &mut buf)?;
        if let Ok(free) = self.instance.get_typed_func::<(u32, u32), ()>(&mut self.store, "plugkit_free") {
            let _ = free.call(&mut self.store, (ptr, len));
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// Idle projects are evicted after this long with zero dispatches -- frees
/// that project's Store/Instance memory (embed-call working buffers, any
/// wasm linear-memory growth from a heavy reindex) back to the OS. The
/// shared Engine/Module are NEVER evicted; only a full daemon exit releases
/// those (and the wasmtime on-disk compile cache means a fresh daemon start
/// re-loads near-instantly regardless).
const PROJECT_IDLE_EVICT_MS: u64 = 30 * 60 * 1000;
const REGISTRY_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Single system-wide daemon entrypoint. Compiles plugkit.wasm ONCE, then
/// polls the project registry for every known project's spool `in/`
/// directory, dispatching through that project's own lazily-created
/// ProjectRuntime while sharing the one compiled Module/Engine across all of
/// them. This is the actual fix for the N-processes-times-2.5GB memory
/// problem live-observed this session (13 concurrent per-project gm-runner/
/// bun.exe processes each independently loading a full wasm+embed-model
/// stack) -- one process, one wasm compile, one embed-model residency,
/// however many projects are active.
fn write_daemon_heartbeat(project_count: usize) {
    let _ = fs::write(
        daemon_status_path(),
        serde_json::json!({
            "pid": std::process::id(),
            "ts": now_ms(),
            "active_projects": project_count,
        })
        .to_string(),
    );
}

pub fn run_daemon() -> anyhow::Result<()> {
    let wasm = crate::wasm_path_pub();
    eprintln!("[gm-runner daemon] compiling plugkit.wasm module (shared across every project)...");
    let engine = build_engine()?;
    let module = Module::from_file(&engine, &wasm)?;
    eprintln!("[gm-runner daemon] module ready, watching registry {}", registry_path().display());
    write_daemon_heartbeat(0);

    let mut projects: HashMap<PathBuf, ProjectRuntime> = HashMap::new();
    let mut last_registry_poll = Instant::now() - REGISTRY_POLL_INTERVAL;
    let mut last_heartbeat = Instant::now();
    let mut known_roots: Vec<PathBuf> = Vec::new();
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

    loop {
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat = Instant::now();
            write_daemon_heartbeat(projects.len());
        }

        if last_registry_poll.elapsed() >= REGISTRY_POLL_INTERVAL {
            last_registry_poll = Instant::now();
            known_roots = read_registry();
        }

        let mut any_work = false;
        for root in &known_roots {
            let spool_dir = root.join(".gm").join("exec-spool");
            let in_dir = spool_dir.join("in");
            let out_dir = spool_dir.join("out");
            if fs::create_dir_all(&in_dir).is_err() || fs::create_dir_all(&out_dir).is_err() {
                continue;
            }

            // Cheap heartbeat per known project so gm-skill's own dead-watcher
            // detection (reads .status.json ts freshness) still works
            // unmodified against this shared daemon -- every registered
            // project's spool dir gets its own current heartbeat file, even
            // though there is only ONE real OS process behind all of them.
            let status_path = spool_dir.join(".status.json");
            let _ = fs::write(
                &status_path,
                serde_json::json!({
                    "pid": std::process::id(),
                    "ts": now_ms(),
                    "daemon": true,
                    "shared_process": true,
                })
                .to_string(),
            );

            let Ok(entries) = fs::read_dir(&in_dir) else { continue };
            for verb_entry in entries.flatten() {
                if !verb_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let verb = verb_entry.file_name().to_string_lossy().into_owned();
                let verb_dir = verb_entry.path();
                let Ok(files) = fs::read_dir(&verb_dir) else { continue };
                for file_entry in files.flatten() {
                    let file_path = file_entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) != Some("txt") {
                        continue;
                    }
                    any_work = true;
                    let task = file_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let body = fs::read_to_string(&file_path).unwrap_or_default();
                    let _ = fs::remove_file(&file_path);

                    let runtime = match projects.get_mut(root) {
                        Some(r) => r,
                        None => {
                            eprintln!("[gm-runner daemon] instantiating project runtime for {}", root.display());
                            let rt = match ProjectRuntime::new(&engine, &module, root.clone()) {
                                Ok(rt) => rt,
                                Err(e) => {
                                    eprintln!("[gm-runner daemon] failed to instantiate runtime for {}: {e:#}", root.display());
                                    continue;
                                }
                            };
                            projects.insert(root.clone(), rt);
                            projects.get_mut(root).unwrap()
                        }
                    };

                    let result = runtime.dispatch(&verb, &body);
                    let out_name = format!("{verb}-{task}.json");
                    let out_body = match result {
                        Ok(s) if !s.is_empty() => s,
                        Ok(_) => serde_json::json!({"ok": false, "error": "empty dispatch result", "verb": verb}).to_string(),
                        Err(e) => serde_json::json!({"ok": false, "error": format!("{e:#}"), "verb": verb}).to_string(),
                    };
                    let tmp = out_dir.join(format!("{out_name}.tmp.{}", std::process::id()));
                    if fs::write(&tmp, &out_body).is_ok() {
                        let _ = fs::rename(&tmp, out_dir.join(&out_name));
                    }
                }
            }
        }

        // Evict idle project runtimes -- this is the actual memory-reclaim
        // mechanism: a project untouched for PROJECT_IDLE_EVICT_MS has its
        // Store+Instance dropped, releasing its wasm linear memory back to
        // the OS, while the shared Engine/Module (and any other active
        // project's own runtime) are entirely unaffected.
        let evict_before = Instant::now() - Duration::from_millis(PROJECT_IDLE_EVICT_MS);
        let to_evict: Vec<PathBuf> = projects
            .iter()
            .filter(|(_, rt)| rt.last_active < evict_before)
            .map(|(root, _)| root.clone())
            .collect();
        for root in to_evict {
            eprintln!("[gm-runner daemon] evicting idle project runtime for {}", root.display());
            projects.remove(&root);
        }

        if !any_work {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
