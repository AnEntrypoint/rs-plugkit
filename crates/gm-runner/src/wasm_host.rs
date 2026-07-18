use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use wasmtime::{Caller, Instance, Linker, Memory};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

pub struct HostState {
    pub cwd: PathBuf,
    pub instance: Arc<Mutex<Option<Instance>>>,
    pub wasi: WasiP1Ctx,
}

impl HostState {
    pub fn new(cwd: PathBuf) -> Self {
        // libsql-ffi (linked into plugkit-core for wasm32) opens its sqlite
        // db file ("gm.db") via real WASI filesystem syscalls, resolved
        // relative to the wasm's WASI cwd -- a default WasiCtxBuilder grants
        // ZERO directory preopens, so sqlite3_open_v2 fails with rc=14
        // ("unable to open database file") on every attempt. Preopen the
        // project dir itself as "." so relative paths the wasm module opens
        // (gm.db, ext-<hash>.db, .gm/exec-spool/...) resolve the same way
        // they did in the JS wrapper (unrestricted Node fs access).
        let mut builder = WasiCtxBuilder::new();
        builder.inherit_stderr();
        if let Err(e) = builder.preopened_dir(&cwd, ".", DirPerms::all(), FilePerms::all()) {
            eprintln!(
                "[gm-runner] WARNING: failed to preopen {} for WASI: {e} -- sqlite/libsql db opens will fail",
                cwd.display()
            );
        }
        let wasi = builder.build_p1();
        Self {
            cwd,
            instance: Arc::new(Mutex::new(None)),
            wasi,
        }
    }
}

fn guest_memory(caller: &mut Caller<'_, HostState>) -> Memory {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .expect("wasm module did not export linear memory")
}

fn read_guest_string(caller: &mut Caller<'_, HostState>, ptr: u32, len: u32) -> String {
    if len == 0 {
        return String::new();
    }
    let memory = guest_memory(caller);
    let mut buf = vec![0u8; len as usize];
    let _ = memory.read(&mut *caller, ptr as usize, &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn write_guest_bytes(caller: &mut Caller<'_, HostState>, bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let instance = caller
        .data()
        .instance
        .lock()
        .unwrap()
        .expect("instance not yet bound to host state");
    let alloc = instance
        .get_typed_func::<u32, u32>(&mut *caller, "plugkit_alloc")
        .expect("plugkit_alloc export missing on wasm module");
    let ptr = alloc
        .call(&mut *caller, bytes.len() as u32)
        .expect("plugkit_alloc call trapped");
    let memory = guest_memory(caller);
    memory
        .write(&mut *caller, ptr as usize, bytes)
        .expect("failed writing into guest linear memory");
    let len = bytes.len() as u64;
    (ptr as u64 & 0xffff_ffff) | (len << 32)
}

fn write_guest_json(caller: &mut Caller<'_, HostState>, v: serde_json::Value) -> u64 {
    write_guest_bytes(caller, v.to_string().as_bytes())
}

pub fn register_wasi(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    wasmtime_wasi::p1::add_to_linker_sync(linker, |s: &mut HostState| &mut s.wasi)?;
    Ok(())
}

/// Registers the `env`-module host imports plugkit-core's wasm expects
/// (crates/plugkit-core/src/wasm_dispatch.rs). fs/log/env/time are real;
/// kv/vec/fetch/exec_js/browser/task/git return an explicit
/// `{"ok":false,"error":"not_implemented_native_runner"}` envelope rather
/// than silently succeeding -- callers see a real, typed failure instead of
/// an opaque zero, until each subsystem lands per its own PRD row.
pub fn register_env_imports(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    linker.func_wrap(
        "env",
        "host_cwd",
        |mut caller: Caller<'_, HostState>| -> u64 {
            let cwd = caller.data().cwd.to_string_lossy().into_owned();
            write_guest_bytes(&mut caller, cwd.as_bytes())
        },
    )?;
    linker.func_wrap(
        "env",
        "host_fs_read",
        |mut caller: Caller<'_, HostState>, path_ptr: u32, path_len: u32| -> u64 {
            let path = read_guest_string(&mut caller, path_ptr, path_len);
            let full = caller.data().cwd.join(&path);
            match fs::read_to_string(&full) {
                Ok(content) => write_guest_bytes(&mut caller, content.as_bytes()),
                Err(_) => 0,
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "host_fs_write",
        |mut caller: Caller<'_, HostState>, path_ptr: u32, path_len: u32, data_ptr: u32, data_len: u32| -> u32 {
            let path = read_guest_string(&mut caller, path_ptr, path_len);
            let data = read_guest_string(&mut caller, data_ptr, data_len);
            let full = caller.data().cwd.join(&path);
            if let Some(parent) = full.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&full, data) {
                Ok(()) => 1,
                Err(_) => 0,
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "host_fs_remove",
        |mut caller: Caller<'_, HostState>, path_ptr: u32, path_len: u32| -> u32 {
            let path = read_guest_string(&mut caller, path_ptr, path_len);
            let full = caller.data().cwd.join(&path);
            match fs::metadata(&full) {
                Ok(md) if md.is_dir() => 0,
                Ok(_) => match fs::remove_file(&full) {
                    Ok(()) => 1,
                    Err(_) => 0,
                },
                Err(_) => 0,
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "host_fs_readdir",
        |mut caller: Caller<'_, HostState>, path_ptr: u32, path_len: u32| -> u64 {
            let path = read_guest_string(&mut caller, path_ptr, path_len);
            let full = caller.data().cwd.join(&path);
            let entries: Vec<String> = fs::read_dir(&full)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            write_guest_json(&mut caller, serde_json::json!(entries))
        },
    )?;

    linker.func_wrap(
        "env",
        "host_fs_stat",
        |mut caller: Caller<'_, HostState>, path_ptr: u32, path_len: u32| -> u64 {
            let path = read_guest_string(&mut caller, path_ptr, path_len);
            let full = caller.data().cwd.join(&path);
            match fs::metadata(&full) {
                Ok(md) => {
                    let v = serde_json::json!({
                        "isDirectory": md.is_dir(),
                        "isFile": md.is_file(),
                        "size": md.len(),
                    });
                    write_guest_json(&mut caller, v)
                }
                Err(_) => 0,
            }
        },
    )?;

    linker.func_wrap("env", "host_now_ms", |_caller: Caller<'_, HostState>| -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })?;

    // TEMP local-only verification stub for the fat-vs-slim artifact-selection
    // witness (publish-real-slim-wasm-artifact PRD row) -- host_plugin_call is
    // a genuine, separate, pre-existing gap in gm-runner (never registered
    // here at all, unlike the JS wrapper's hostPluginCallViaAgentplugRunner)
    // that blocks ANY gm-runner dispatch against either wasm variant, fat or
    // slim, identically. NOT part of the slim/fat change; reverted before
    // this task's edits are finalized. Real fix is a follow-up PRD row.
    linker.func_wrap(
        "env",
        "host_plugin_call",
        |_caller: Caller<'_, HostState>, _p: u32, _pl: u32, _v: u32, _vl: u32, _b: u32, _bl: u32| -> u64 {
            0
        },
    )?;

    linker.func_wrap(
        "env",
        "host_log",
        |mut caller: Caller<'_, HostState>, level: u32, msg_ptr: u32, msg_len: u32| -> u32 {
            let msg = read_guest_string(&mut caller, msg_ptr, msg_len);
            eprintln!("[plugkit L{level}] {msg}");
            1
        },
    )?;

    linker.func_wrap(
        "env",
        "host_env_get",
        |mut caller: Caller<'_, HostState>, key_ptr: u32, key_len: u32| -> u64 {
            let key = read_guest_string(&mut caller, key_ptr, key_len);
            match std::env::var(&key) {
                Ok(val) => write_guest_bytes(&mut caller, val.as_bytes()),
                Err(_) => 0,
            }
        },
    )?;

    // embed.rs registers getrandom::register_custom_getrandom! against this
    // import (candle/tokenizers need real entropy for numeric stability,
    // not just RNG-seeding); fill directly into guest memory since it's a
    // caller-owned out-buffer, not a host-allocated return value.
    linker.func_wrap(
        "env",
        "host_random_fill",
        |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> u32 {
            use std::time::{SystemTime, UNIX_EPOCH};
            let mut buf = vec![0u8; len as usize];
            // xorshift* seeded from wall-clock + pid: sufficient for
            // candle/tokenizers' internal RNG needs (dropout masks, etc.
            // none of which are cryptographic) without pulling in a
            // getrandom-compatible OS RNG crate as an extra dependency.
            let mut seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
                ^ (std::process::id() as u64).wrapping_mul(0xBF58476D1CE4E5B9);
            for byte in buf.iter_mut() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *byte = (seed & 0xff) as u8;
            }
            let memory = guest_memory(&mut caller);
            if memory.write(&mut caller, ptr as usize, &buf).is_err() {
                return 0;
            }
            1
        },
    )?;

    let not_implemented = |mut caller: Caller<'_, HostState>| -> u64 {
        write_guest_json(
            &mut caller,
            serde_json::json!({"ok": false, "error": "not_implemented_native_runner"}),
        )
    };

    // Matches plugkit-wasm-wrapper.js host_fetch's {status, body} contract
    // (or {status:0, error} on failure) -- a plain GET/POST-with-body fetch,
    // never streaming, matching the wasm-side callers' usage (they always
    // await the full response text).
    linker.func_wrap(
        "env",
        "host_fetch",
        |mut caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32, opts_ptr: u32, opts_len: u32| -> u64 {
            let url = read_guest_string(&mut caller, url_ptr, url_len);
            let opts_str = read_guest_string(&mut caller, opts_ptr, opts_len);
            let opts: serde_json::Value = if opts_str.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&opts_str).unwrap_or(serde_json::json!({}))
            };
            let method = opts.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_uppercase();
            let body = opts.get("body").and_then(|v| v.as_str());

            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(10))
                .build();
            let req = agent.request(&method, &url);
            let resp = match body {
                Some(b) => req.send_string(b),
                None => req.call(),
            };
            let result = match resp {
                Ok(r) => {
                    let status = r.status();
                    let text = r.into_string().unwrap_or_default();
                    serde_json::json!({"status": status, "body": text})
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    serde_json::json!({"status": code, "body": text})
                }
                Err(e) => serde_json::json!({"status": 0, "error": e.to_string()}),
            };

            write_guest_json(&mut caller, result)
        },
    )?;
    // kv: one JSON file per (namespace, key), under
    // <project>/.gm/disciplines/<safe-ns>/<safe-key>.json -- matches
    // plugkit-wasm-wrapper.js's kvFilePath/safeName layout exactly, so an
    // existing project's kv store is readable identically by either host.
    linker.func_wrap(
        "env",
        "host_kv_get",
        |mut caller: Caller<'_, HostState>, ns_ptr: u32, ns_len: u32, key_ptr: u32, key_len: u32| -> u64 {
            let ns = read_guest_string(&mut caller, ns_ptr, ns_len);
            let key = read_guest_string(&mut caller, key_ptr, key_len);
            if ns.is_empty() || key.is_empty() {
                return 0;
            }
            let path = kv_file_path(&caller.data().cwd, &ns, &key);
            match fs::read_to_string(&path) {
                Ok(content) => write_guest_bytes(&mut caller, content.as_bytes()),
                Err(_) => 0,
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_put",
        |mut caller: Caller<'_, HostState>,
         ns_ptr: u32,
         ns_len: u32,
         key_ptr: u32,
         key_len: u32,
         val_ptr: u32,
         val_len: u32|
         -> u32 {
            let ns = read_guest_string(&mut caller, ns_ptr, ns_len);
            let key = read_guest_string(&mut caller, key_ptr, key_len);
            let val = read_guest_string(&mut caller, val_ptr, val_len);
            if ns.is_empty() || key.is_empty() {
                return 0;
            }
            let path = kv_file_path(&caller.data().cwd, &ns, &key);
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, val) {
                Ok(()) => 1,
                Err(_) => 0,
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_delete",
        |mut caller: Caller<'_, HostState>, ns_ptr: u32, ns_len: u32, key_ptr: u32, key_len: u32| -> u32 {
            let ns = read_guest_string(&mut caller, ns_ptr, ns_len);
            let key = read_guest_string(&mut caller, key_ptr, key_len);
            if ns.is_empty() || key.is_empty() {
                return 0;
            }
            let path = kv_file_path(&caller.data().cwd, &ns, &key);
            match fs::remove_file(&path) {
                Ok(()) => 1,
                Err(_) => 0,
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_query",
        |mut caller: Caller<'_, HostState>, ns_ptr: u32, ns_len: u32, q_ptr: u32, q_len: u32| -> u64 {
            let ns = read_guest_string(&mut caller, ns_ptr, ns_len);
            let q = read_guest_string(&mut caller, q_ptr, q_len).to_lowercase();
            if ns.is_empty() {
                return 0;
            }
            let dir = kv_namespace_dir(&caller.data().cwd, &ns);
            let mut results = Vec::new();
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(content) = fs::read_to_string(&path) {
                        if q.is_empty() || content.to_lowercase().contains(&q) {
                            results.push(content);
                        }
                    }
                }
            }
            write_guest_json(&mut caller, serde_json::json!(results))
        },
    )?;
    linker.func_wrap(
        "env",
        "host_vec_search",
        move |caller: Caller<'_, HostState>, _q: u32, _l: u32, _k: u32| -> u64 { not_implemented(caller) },
    )?;
    // Writes directly into the caller-owned out_ptr buffer (same convention
    // as host_random_fill), returning the embedding dimension on success or
    // -1 on failure -- matches embed.rs's probe_host_embed contract exactly,
    // so plugkit-core's existing host-delegation probe (skip loading the
    // 133MB wasm-embedded safetensors fallback if the host already serves
    // embeddings) picks this up with zero changes on the wasm side.
    linker.func_wrap(
        "env",
        "host_vec_embed",
        |mut caller: Caller<'_, HostState>, text_ptr: u32, text_len: u32, out_ptr: u32, out_len: u32| -> i32 {
            let text = read_guest_string(&mut caller, text_ptr, text_len);
            match crate::embed::embed(&text) {
                Ok(values) => {
                    let dim = values.len().min(out_len as usize);
                    let mut bytes = Vec::with_capacity(dim * 4);
                    for v in &values[..dim] {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    let memory = guest_memory(&mut caller);
                    if memory.write(&mut caller, out_ptr as usize, &bytes).is_err() {
                        return -1;
                    }
                    dim as i32
                }
                Err(e) => {
                    eprintln!("[gm-runner] host_vec_embed failed: {e}");
                    -1
                }
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "host_exec_js",
        |mut caller: Caller<'_, HostState>, code_ptr: u32, code_len: u32, opts_ptr: u32, opts_len: u32| -> u64 {
            let code = read_guest_string(&mut caller, code_ptr, code_len);
            let opts_str = read_guest_string(&mut caller, opts_ptr, opts_len);
            let opts: serde_json::Value = if opts_str.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&opts_str).unwrap_or(serde_json::json!({}))
            };
            let cwd = caller.data().cwd.clone();
            let result = crate::exec_js::run(&code, &opts, &cwd);
            write_guest_json(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_browser_exec",
        |mut caller: Caller<'_, HostState>,
         body_ptr: u32,
         body_len: u32,
         cwd_ptr: u32,
         cwd_len: u32,
         sid_ptr: u32,
         sid_len: u32|
         -> u64 {
            let body = read_guest_string(&mut caller, body_ptr, body_len);
            let cwd_arg = read_guest_string(&mut caller, cwd_ptr, cwd_len);
            let session_id = read_guest_string(&mut caller, sid_ptr, sid_len);
            let session_id = if session_id.is_empty() { "default".to_string() } else { session_id };
            let cwd = if cwd_arg.is_empty() { caller.data().cwd.clone() } else { PathBuf::from(&cwd_arg) };
            let result = crate::browser::run(&body, &cwd, &session_id);
            write_guest_json(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_task_proc",
        move |caller: Caller<'_, HostState>, _a: u32, _al: u32, _p: u32, _pl: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    // args is either a JSON array ('["status","--porcelain"]', from
    // git_call_argv) or a raw whitespace-split string ("status --porcelain",
    // from git_call) -- same dual-format contract plugkit-wasm-wrapper.js's
    // host_git implements, so callers on either host parse identically.
    linker.func_wrap(
        "env",
        "host_git",
        |mut caller: Caller<'_, HostState>, args_ptr: u32, args_len: u32, cwd_ptr: u32, cwd_len: u32| -> u64 {
            let args = read_guest_string(&mut caller, args_ptr, args_len);
            let cwd_arg = read_guest_string(&mut caller, cwd_ptr, cwd_len);
            let trimmed = args.trim();
            let argv: Vec<String> = if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<String>>(trimmed)
                    .unwrap_or_else(|_| trimmed.split_whitespace().map(String::from).collect())
            } else {
                trimmed.split_whitespace().map(String::from).collect()
            };
            let cwd = if cwd_arg.is_empty() {
                caller.data().cwd.clone()
            } else {
                PathBuf::from(&cwd_arg)
            };
            let output = std::process::Command::new("git").args(&argv).current_dir(&cwd).output();
            let v = match output {
                Ok(out) => serde_json::json!({
                    "stdout": String::from_utf8_lossy(&out.stdout),
                    "stderr": String::from_utf8_lossy(&out.stderr),
                    "exit_code": out.status.code().unwrap_or(-1),
                }),
                Err(e) => serde_json::json!({"stdout": "", "stderr": e.to_string(), "exit_code": 1}),
            };
            write_guest_json(&mut caller, v)
        },
    )?;

    Ok(())
}

fn kv_namespace_dir(cwd: &std::path::Path, ns: &str) -> PathBuf {
    cwd.join(".gm").join("disciplines").join(safe_name(ns))
}

fn kv_file_path(cwd: &std::path::Path, ns: &str, key: &str) -> PathBuf {
    kv_namespace_dir(cwd, ns).join(format!("{}.json", safe_name(key)))
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
