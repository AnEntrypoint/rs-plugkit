#![cfg(all(target_arch = "wasm32", feature = "wit-env"))]

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "env-imports",
    });
}

use bindings::plugkit::env::host as wit;

const HOST_FS_READ_EMPTY_SUCCESS: u64 = 1;

unsafe fn bytes<'a>(ptr: *const u8, len: u32) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len as usize)
    }
}

unsafe fn text<'a>(ptr: *const u8, len: u32) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(bytes(ptr, len))
}

fn pack(value: Option<Vec<u8>>) -> u64 {
    match value {
        Some(mut v) if !v.is_empty() => {
            v.shrink_to_fit();
            let len = v.len();
            let ptr = v.as_mut_ptr() as usize;
            std::mem::forget(v);
            crate::wasm_dispatch::host_abi::pack_ptr_len(ptr, len)
        }
        _ => 0,
    }
}

pub unsafe fn host_cwd() -> u64 {
    pack(wit::host_cwd())
}

pub unsafe fn host_fs_allow_root(path_ptr: *const u8, path_len: u32) -> u32 {
    wit::host_fs_allow_root(&text(path_ptr, path_len))
}

pub unsafe fn host_fs_read(path_ptr: *const u8, path_len: u32) -> u64 {
    match wit::host_fs_read(&text(path_ptr, path_len)) {
        Some(v) if v.is_empty() => HOST_FS_READ_EMPTY_SUCCESS,
        other => pack(other),
    }
}

pub unsafe fn host_fs_write(path_ptr: *const u8, path_len: u32, data_ptr: *const u8, data_len: u32) -> u32 {
    wit::host_fs_write(&text(path_ptr, path_len), bytes(data_ptr, data_len))
}

pub unsafe fn host_fs_cas_write(
    path_ptr: *const u8,
    path_len: u32,
    expected_ptr: *const u8,
    expected_len: u32,
    data_ptr: *const u8,
    data_len: u32,
) -> u32 {
    wit::host_fs_cas_write(
        &text(path_ptr, path_len),
        bytes(expected_ptr, expected_len),
        bytes(data_ptr, data_len),
    )
}

pub unsafe fn host_fs_remove(path_ptr: *const u8, path_len: u32) -> u32 {
    wit::host_fs_remove(&text(path_ptr, path_len))
}

pub unsafe fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64 {
    pack(wit::host_fs_readdir(&text(path_ptr, path_len)))
}

pub unsafe fn host_fs_stat(path_ptr: *const u8, path_len: u32) -> u64 {
    pack(wit::host_fs_stat(&text(path_ptr, path_len)))
}

pub unsafe fn host_fetch(url_ptr: *const u8, url_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64 {
    pack(wit::host_fetch(&text(url_ptr, url_len), &text(opts_ptr, opts_len)))
}

pub unsafe fn host_kv_get(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u64 {
    pack(wit::host_kv_get(&text(ns_ptr, ns_len), &text(key_ptr, key_len)))
}

pub unsafe fn host_kv_put(
    ns_ptr: *const u8,
    ns_len: u32,
    key_ptr: *const u8,
    key_len: u32,
    val_ptr: *const u8,
    val_len: u32,
) -> u32 {
    wit::host_kv_put(
        &text(ns_ptr, ns_len),
        &text(key_ptr, key_len),
        bytes(val_ptr, val_len),
    )
}

pub unsafe fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32 {
    wit::host_kv_delete(&text(ns_ptr, ns_len), &text(key_ptr, key_len))
}

pub unsafe fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64 {
    pack(wit::host_kv_query(&text(ns_ptr, ns_len), &text(q_ptr, q_len)))
}

pub unsafe fn host_vec_search(q_ptr: *const u8, q_len: u32, k: u32) -> u64 {
    pack(wit::host_vec_search(&text(q_ptr, q_len), k))
}

pub unsafe fn host_vec_embed(text_ptr: *const u8, text_len: u32, out_ptr: *mut f32, out_len: u32) -> i32 {
    match wit::host_vec_embed(&text(text_ptr, text_len), out_len) {
        Ok(v) => {
            let n = v.len().min(out_len as usize);
            std::ptr::copy_nonoverlapping(v.as_ptr(), out_ptr, n);
            v.len() as i32
        }
        Err(code) => code,
    }
}

pub unsafe fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64 {
    pack(wit::host_exec_js(&text(code_ptr, code_len), &text(opts_ptr, opts_len)))
}

pub unsafe fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32 {
    wit::host_log(level, &text(msg_ptr, msg_len))
}

pub unsafe fn host_now_ms() -> u64 {
    wit::host_now_ms()
}

pub unsafe fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64 {
    pack(wit::host_env_get(&text(key_ptr, key_len)))
}

pub unsafe fn host_random_fill(ptr: *mut u8, len: u32) -> u32 {
    match wit::host_random_fill(len) {
        Some(v) => {
            let n = v.len().min(len as usize);
            std::ptr::copy_nonoverlapping(v.as_ptr(), ptr, n);
            n as u32
        }
        None => 0,
    }
}

pub unsafe fn host_task_proc(action_ptr: *const u8, action_len: u32, params_ptr: *const u8, params_len: u32) -> u64 {
    pack(wit::host_task_proc(&text(action_ptr, action_len), &text(params_ptr, params_len)))
}

pub unsafe fn host_git(args_ptr: *const u8, args_len: u32, cwd_ptr: *const u8, cwd_len: u32) -> u64 {
    pack(wit::host_git(&text(args_ptr, args_len), &text(cwd_ptr, cwd_len)))
}

pub unsafe fn host_plugin_call(
    plugin_ptr: *const u8,
    plugin_len: u32,
    verb_ptr: *const u8,
    verb_len: u32,
    body_ptr: *const u8,
    body_len: u32,
) -> u64 {
    pack(wit::host_plugin_call(
        &text(plugin_ptr, plugin_len),
        &text(verb_ptr, verb_len),
        &text(body_ptr, body_len),
    ))
}

pub unsafe fn crawl_cdp(body_ptr: *const u8, body_len: u32) -> u64 {
    pack(wit::crawl_cdp(&text(body_ptr, body_len)))
}
