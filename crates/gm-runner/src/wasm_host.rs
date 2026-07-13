use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use wasmtime::{Caller, Instance, Linker, Memory};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

pub struct HostState {
    pub cwd: PathBuf,
    pub instance: Arc<Mutex<Option<Instance>>>,
    pub wasi: WasiP1Ctx,
}

impl HostState {
    pub fn new(cwd: PathBuf) -> Self {
        let wasi = WasiCtxBuilder::new().inherit_stderr().build_p1();
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

    linker.func_wrap(
        "env",
        "host_fetch",
        move |caller: Caller<'_, HostState>, _u: u32, _v: u32, _o: u32, _l: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_get",
        move |caller: Caller<'_, HostState>, _a: u32, _b: u32, _c: u32, _d: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_put",
        move |caller: Caller<'_, HostState>, _a: u32, _b: u32, _c: u32, _d: u32, _e: u32, _f: u32| -> u32 {
            let _ = not_implemented(caller);
            0
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_delete",
        move |caller: Caller<'_, HostState>, _a: u32, _b: u32, _c: u32, _d: u32| -> u32 {
            let _ = not_implemented(caller);
            0
        },
    )?;
    linker.func_wrap(
        "env",
        "host_kv_query",
        move |caller: Caller<'_, HostState>, _a: u32, _b: u32, _c: u32, _d: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_vec_search",
        move |caller: Caller<'_, HostState>, _q: u32, _l: u32, _k: u32| -> u64 { not_implemented(caller) },
    )?;
    linker.func_wrap(
        "env",
        "host_vec_embed",
        move |_caller: Caller<'_, HostState>, _t: u32, _l: u32, _o: u32, _ol: u32| -> i32 { -1 },
    )?;
    linker.func_wrap(
        "env",
        "host_exec_js",
        move |caller: Caller<'_, HostState>, _c: u32, _l: u32, _o: u32, _ol: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_browser_exec",
        move |caller: Caller<'_, HostState>, _b: u32, _bl: u32, _c: u32, _cl: u32, _s: u32, _sl: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_task_proc",
        move |caller: Caller<'_, HostState>, _a: u32, _al: u32, _p: u32, _pl: u32| -> u64 {
            not_implemented(caller)
        },
    )?;
    linker.func_wrap(
        "env",
        "host_git",
        move |caller: Caller<'_, HostState>, _a: u32, _al: u32, _c: u32, _cl: u32| -> u64 {
            not_implemented(caller)
        },
    )?;

    Ok(())
}
