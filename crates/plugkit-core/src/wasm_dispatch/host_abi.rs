use serde_json::Value;

/// Declares the host ABI once and derives both the `extern "C"` block and
/// [`HOST_IMPORTS`] from that single list.
///
/// A name is now structurally incapable of appearing in one and not the other.
/// The previous arrangement kept the two by hand and drifted twice: first to 16
/// of 20 (omitting host_cwd, host_git, host_kv_delete and host_plugin_call, so
/// `health` did not advertise that inter-plugin calling exists), and then again
/// after that repair, when host_fs_remove was added to the extern block alone
/// and host_random_fill was declared in a function-scoped extern in embed.rs --
/// leaving `health`, which serves this list as the truth about what the guest
/// imports, wrong about two of them.
macro_rules! host_abi {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)+) => {
        #[link(wasm_import_module = "env")]
        extern "C" {
            $(pub fn $name($($arg: $ty),*) $(-> $ret)?;)+
        }

        /// Every name declared by the `host_abi!` invocation above, in
        /// declaration order. `health` advertises this.
        pub const HOST_IMPORTS: &[&str] = &[$(stringify!($name)),+];
    };
}

host_abi! {
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

/// Hand a string to the host as a packed `(ptr, len)` pair.
///
/// `shrink_to_fit` is load-bearing, not tidiness. `String::into_bytes` keeps
/// the string's CAPACITY, which routinely exceeds its length, while the
/// matching `plugkit_free` reconstructs the Vec with `len` as both length and
/// capacity. Handing the allocator a different layout than it issued is
/// undefined behaviour, and it was reachable on every packed response whose
/// backing string had spare capacity -- which is most of them, since they are
/// built by `format!` and `to_string`.
pub(crate) fn pack(s: String) -> u64 {
    let mut v = s.into_bytes();
    v.shrink_to_fit();
    let len = v.len() as u64;
    let ptr = v.as_mut_ptr() as u64;
    std::mem::forget(v);
    (ptr & 0xffff_ffff) | (len << 32)
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

/// Delete a file, via the host's own filesystem import.
///
/// Previously this spawned a node subprocess through `host_exec_js` to run
/// `fs.unlinkSync`, then decided the outcome by string-matching `"removed"` in
/// its stdout -- a process launch, a 15s timeout, a JSON round-trip, and a
/// stringly-typed result, all to delete one file. The host has implemented
/// `host_fs_remove` natively the whole time; it simply was not declared in the
/// extern block above, so the guest could not see it.
///
/// The host refuses directories and reports 0 for both "was a directory" and
/// "failed", which is the same collapsed outcome the old shim produced, so
/// this is a straight substitution with no behavioural change to callers.
pub fn host_remove(path: &str) -> bool {
    let rc = unsafe { host_fs_remove(path.as_ptr(), path.len() as u32) };
    rc != 0
}

pub fn host_kv_read(namespace: &str, key: &str) -> Option<String> {
    if key.is_empty() { return None; }
    let packed = unsafe { host_kv_get(namespace.as_ptr(), namespace.len() as u32, key.as_ptr(), key.len() as u32) };
    unpack_to_string(packed)
}
