use serde_json::Value;

macro_rules! host_abi_extern_block_and_host_imports_list_from_one_declaration {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)+) => {
        #[link(wasm_import_module = "env")]
        extern "C" {
            $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)+
        }

        pub const HOST_IMPORTS: &[&str] = &[$(stringify!($name)),+];
    };
}

host_abi_extern_block_and_host_imports_list_from_one_declaration! {
    fn host_cwd() -> u64;
    fn host_fs_read(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_fs_write(path_ptr: *const u8, path_len: u32, data_ptr: *const u8, data_len: u32) -> u32;
    fn host_fs_remove(path_ptr: *const u8, path_len: u32) -> u32;
    fn host_fs_readdir(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_fs_stat(path_ptr: *const u8, path_len: u32) -> u64;
    fn host_fetch(url_ptr: *const u8, url_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    fn host_kv_get(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u64;
    fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    fn host_kv_delete(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u32;
    fn host_kv_query(ns_ptr: *const u8, ns_len: u32, q_ptr: *const u8, q_len: u32) -> u64;
    fn host_vec_search(q_ptr: *const u8, q_len: u32, k: u32) -> u64;
    fn host_vec_embed(text_ptr: *const u8, text_len: u32, out_ptr: *mut f32, out_len: u32) -> i32;
    fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    fn host_now_ms() -> u64;
    fn host_env_get(key_ptr: *const u8, key_len: u32) -> u64;
    fn host_random_fill(ptr: *mut u8, len: u32) -> u32;
    fn host_browser_exec(body_ptr: *const u8, body_len: u32, cwd_ptr: *const u8, cwd_len: u32, session_id_ptr: *const u8, session_id_len: u32) -> u64;
    fn host_task_proc(action_ptr: *const u8, action_len: u32, params_ptr: *const u8, params_len: u32) -> u64;
    fn host_git(args_ptr: *const u8, args_len: u32, cwd_ptr: *const u8, cwd_len: u32) -> u64;
    fn host_plugin_call(plugin_ptr: *const u8, plugin_len: u32, verb_ptr: *const u8, verb_len: u32, body_ptr: *const u8, body_len: u32) -> u64;
}

pub fn plugin_call(plugin: &str, verb: &str, body: &Value) -> Value {
    let body_s = body.to_string();
    let packed = unsafe {
        host_plugin_call(
            plugin.as_ptr(), plugin.len() as u32,
            verb.as_ptr(), verb.len() as u32,
            body_s.as_ptr(), body_s.len() as u32,
        )
    };
    unpack_to_value(packed)
}

pub fn host_task(action: &str, params: &Value) -> Value {
    let params_s = params.to_string();
    let packed = unsafe { host_task_proc(action.as_ptr(), action.len() as u32, params_s.as_ptr(), params_s.len() as u32) };
    unpack_to_value(packed)
}

pub fn git_call(args: &str, cwd: Option<&str>) -> Value {
    let cwd_s = cwd.unwrap_or("");
    let packed = unsafe { host_git(args.as_ptr(), args.len() as u32, cwd_s.as_ptr(), cwd_s.len() as u32) };
    unpack_to_value(packed)
}

pub fn git_porcelain() -> String {
    porcelain_or_dirty(git_call("status --porcelain", None))
}

pub(crate) fn porcelain_or_dirty(v: Value) -> String {
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(true);
    let exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(0);
    if !ok || exit_code != 0 {
        return "?? git-status-failed".to_string();
    }
    v.get("stdout").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

pub fn git_call_argv(argv: &[&str], cwd: Option<&str>) -> Value {
    let json = serde_json::to_string(argv).unwrap_or_default();
    git_call(&json, cwd)
}

const _: () = assert!(
    std::mem::size_of::<usize>() == 4,
    "packed (ptr,len) u64 ABI requires a 32-bit address space: a usize wider than 4 bytes makes `ptr & 0xffff_ffff` a silent truncation"
);

pub(crate) fn pack(s: String) -> u64 {
    let mut v = s.into_bytes();
    shrink_capacity_to_length_so_plugkit_free_reconstructs_the_same_vec_layout(&mut v);
    let len = v.len();
    let ptr = v.as_mut_ptr() as usize;
    std::mem::forget(v);
    pack_ptr_len(ptr, len)
}

fn shrink_capacity_to_length_so_plugkit_free_reconstructs_the_same_vec_layout(v: &mut Vec<u8>) {
    v.shrink_to_fit();
}

pub(crate) fn pack_ptr_len(ptr: usize, len: usize) -> u64 {
    assert!(
        (ptr as u64) <= 0xffff_ffff,
        "pack: pointer {ptr:#x} exceeds the 32-bit field of the packed ABI"
    );
    assert!(
        (len as u64) <= 0xffff_ffff,
        "pack: length {len} exceeds the 32-bit field of the packed ABI"
    );
    (ptr as u64 & 0xffff_ffff) | ((len as u64) << 32)
}

pub(crate) fn read_str(ptr: *const u8, len: u32) -> String {
    if ptr.is_null() || len == 0 { return String::new(); }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn unpack_to_string(packed: u64) -> Option<String> {
    let p = (packed & 0xffff_ffff) as u32;
    let l = (packed >> 32) as u32;
    if p == 0 || l == 0 { return None; }
    let bytes = unsafe { Vec::from_raw_parts(p as *mut u8, l as usize, l as usize) };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn unpack_to_value(packed: u64) -> Value {
    match unpack_to_string(packed) {
        Some(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        None => Value::Null,
    }
}

pub fn unpack_to_value_pub(packed: u64) -> Value { unpack_to_value(packed) }

pub fn unpack_to_string_pub(packed: u64) -> Option<String> { unpack_to_string(packed) }

pub fn pack_ptr_len_pub(ptr: usize, len: usize) -> u64 { pack_ptr_len(ptr, len) }

pub fn host_cwd_string() -> Option<String> {
    let packed = unsafe { host_cwd() };
    unpack_to_string(packed)
}

pub fn host_read(path: &str) -> Option<String> {
    let packed = unsafe { host_fs_read(path.as_ptr(), path.len() as u32) };
    unpack_to_string(packed)
}

pub fn host_write(path: &str, data: &str) -> bool {
    let rc = unsafe { host_fs_write(path.as_ptr(), path.len() as u32, data.as_ptr(), data.len() as u32) };
    rc != 0
}

pub fn host_stat(path: &str) -> Option<Value> {
    let packed = unsafe { host_fs_stat(path.as_ptr(), path.len() as u32) };
    unpack_to_string(packed).map(|s| serde_json::from_str(&s).unwrap_or(Value::Null))
}

pub fn host_exists(path: &str) -> bool {
    host_stat(path).map(|v| !v.is_null()).unwrap_or(false)
}

pub fn host_remove_file_never_directory(path: &str) -> bool {
    let rc = unsafe { host_fs_remove(path.as_ptr(), path.len() as u32) };
    rc != 0
}

pub fn host_kv_read(namespace: &str, key: &str) -> Option<String> {
    if key.is_empty() { return None; }
    let packed = unsafe { host_kv_get(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
    unpack_to_string(packed)
}
