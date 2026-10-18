#![cfg(all(target_arch = "wasm32", feature = "wit-env"))]

mod bindings {
    wit_bindgen::generate!({
        world: "plugkit-env",
        path: "wit",
    });
}

use super::host_abi::{pack, read_str};
use bindings::gm::plugkit::env as wit;

fn packed(v: Option<String>) -> u64 {
    v.map_or(0, pack)
}

unsafe fn copy_into<T: Copy>(src: &[T], dst: *mut T, cap: u32) {
    let n = src.len().min(cap as usize);
    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, n);
}

pub unsafe fn host_cwd() -> u64 {
    packed(wit::host_cwd())
}

pub unsafe fn host_fs_allow_root(path_ptr: *const u8, path_len: u32) -> u32 {
    wit::host_fs_allow_root(&read_str(path_ptr, path_len)) as u32
}

pub unsafe fn host_fs_read(path_ptr: *const u8, path_len: u32) -> u64 {
    packed(wit::host_fs_read(&read_str(path_ptr, path_len)))
}

pub unsafe fn host_fs_write(
    path_ptr: *const u8,
    path_len: u32,
    data_ptr: *const u8,
    data_len: u32,
) -> u32 {
    wit::host_fs_write(&read_str(path_ptr, path_len), &read_str(data_ptr, data_len)) as u32
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
        &read_str(path_ptr, path_len),
        &read_str(expected_ptr, expected_len),
        &read_str(data_ptr, data_len),
    )
}

pub unsafe fn host_fs_remove(path_ptr: *const u8, path_len: u32) -> u32 {
    wit::host_fs_remove(&read_str(path_ptr, path_len)) as u32
}

pub unsafe fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64 {
    packed(wit::host_fs_readdir(&read_str(path_ptr, path_len)))
}

pub unsafe fn host_fs_stat(path_ptr: *const u8, path_len: u32) -> u64 {
    packed(wit::host_fs_stat(&read_str(path_ptr, path_len)))
}

pub unsafe fn host_fetch(
    url_ptr: *const u8,
    url_len: u32,
    opts_ptr: *const u8,
    opts_len: u32,
) -> u64 {
    packed(wit::host_fetch(
        &read_str(url_ptr, url_len),
        &read_str(opts_ptr, opts_len),
    ))
}

pub unsafe fn host_kv_get(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u64 {
    packed(wit::host_kv_get(
        &read_str(ns_ptr, ns_len),
        &read_str(key_ptr, key_len),
    ))
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
        &read_str(ns_ptr, ns_len),
        &read_str(key_ptr, key_len),
        &read_str(val_ptr, val_len),
    )
}

pub unsafe fn host_kv_delete(
    ns_ptr: *const u8,
    ns_len: u32,
    key_ptr: *const u8,
    key_len: u32,
) -> u32 {
    wit::host_kv_delete(&read_str(ns_ptr, ns_len), &read_str(key_ptr, key_len))
}

pub unsafe fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64 {
    packed(wit::host_kv_query(
        &read_str(ns_ptr, ns_len),
        &read_str(q_ptr, q_len),
    ))
}

pub unsafe fn host_vec_search(q_ptr: *const u8, q_len: u32, k: u32) -> u64 {
    packed(wit::host_vec_search(&read_str(q_ptr, q_len), k))
}

pub unsafe fn host_vec_embed(
    text_ptr: *const u8,
    text_len: u32,
    out_ptr: *mut f32,
    out_len: u32,
) -> i32 {
    let (rc, vector) = wit::host_vec_embed(&read_str(text_ptr, text_len), out_len);
    copy_into(&vector, out_ptr, out_len);
    rc
}

pub unsafe fn host_exec_js(
    code_ptr: *const u8,
    code_len: u32,
    opts_ptr: *const u8,
    opts_len: u32,
) -> u64 {
    packed(wit::host_exec_js(
        &read_str(code_ptr, code_len),
        &read_str(opts_ptr, opts_len),
    ))
}

pub unsafe fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32 {
    wit::host_log(level, &read_str(msg_ptr, msg_len))
}

pub unsafe fn host_now_ms() -> u64 {
    wit::host_now_ms()
}

pub unsafe fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64 {
    packed(wit::host_env_get(&read_str(key_ptr, key_len)))
}

pub unsafe fn host_random_fill(ptr: *mut u8, len: u32) -> u32 {
    let (rc, bytes) = wit::host_random_fill(len);
    copy_into(&bytes, ptr, len);
    rc
}

pub unsafe fn host_task_proc(
    action_ptr: *const u8,
    action_len: u32,
    params_ptr: *const u8,
    params_len: u32,
) -> u64 {
    packed(wit::host_task_proc(
        &read_str(action_ptr, action_len),
        &read_str(params_ptr, params_len),
    ))
}

pub unsafe fn host_git(
    args_ptr: *const u8,
    args_len: u32,
    cwd_ptr: *const u8,
    cwd_len: u32,
) -> u64 {
    packed(wit::host_git(
        &read_str(args_ptr, args_len),
        &read_str(cwd_ptr, cwd_len),
    ))
}

pub unsafe fn host_plugin_call(
    plugin_ptr: *const u8,
    plugin_len: u32,
    verb_ptr: *const u8,
    verb_len: u32,
    body_ptr: *const u8,
    body_len: u32,
) -> u64 {
    packed(wit::host_plugin_call(
        &read_str(plugin_ptr, plugin_len),
        &read_str(verb_ptr, verb_len),
        &read_str(body_ptr, body_len),
    ))
}

pub unsafe fn crawl_cdp(body_ptr: *const u8, body_len: u32) -> u64 {
    packed(wit::crawl_cdp(&read_str(body_ptr, body_len)))
}
